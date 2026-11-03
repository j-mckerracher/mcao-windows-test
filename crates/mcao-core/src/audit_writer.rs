//! Hash-chained audit store writer for audit envelope v1 records.
//!
//! Ports the `append`, `verify`, and `project-ledger` paths of `bin/fm-audit-writer.py`
//! (DPES-753, DPES-884 AC4) over one store directory. The reference owns the store layout and
//! recovery contract (`docs/audit-writer.md`). This module is the library core only: it is not
//! wired to any CLI command.
//!
//! # Store layout
//!
//! - `segment-<first seq, 20 digits>.jsonl`: one canonical JSON record per line, each followed by
//!   `\n`. A record's `prev_hash` is [`audit_hash`] of the previous line without its newline.
//! - `checkpoint.json`: canonical JSON `{"last_hash","last_seq","segment","v"}` plus `\n`. Its
//!   atomic replacement is the commit point of an append.
//!
//! Every record is checked with [`audit_envelope::validate`]. The caller supplies zone offsets
//! through `utc_offset_secs`, as the other audit modules do.
//!
//! # Not ported
//!
//! - Off unix, the reference's `flock` on `.lock` is not taken: [`StoreLock`] is cfg(unix).
//!   Concurrent writers there are not excluded, so they can lose or duplicate sequence numbers.
//! - The rejection record the reference writes to the state directory.
//! - Duplicate-key refusal at parse time. A stored line is accepted only when it equals the
//!   canonical form of its parsed value, which excludes duplicate keys.
//! - Numbers outside the exact 64-bit range. [`canonical_json`] refuses them, so the store
//!   refuses such a record instead of writing bytes that may differ from the reference.

use std::fs::{self, DirBuilder, File, OpenOptions};
use std::io::{self, Write};
use std::path::{Path, PathBuf};

use serde_json::{Map, Value, json};

use crate::audit_envelope::{
    self, Definition, Reason, Rejection, days_from_civil, parse_ts_calendar,
};
use crate::canonical_json::{CanonicalJsonError, try_to_canonical_string};
use crate::sha256::{audit_hash, is_audit_hash};

/// Records per segment before rotation, as the reference's `SEGMENT_RECORDS`.
pub const SEGMENT_RECORDS: u64 = 1000;

const CHECKPOINT: &str = "checkpoint.json";
#[cfg(unix)]
const LOCK: &str = ".lock";
const CHECKPOINT_TMP: &str = "checkpoint.json.tmp";
const LEDGER_V1_EVENTS: [&str; 5] = [
    "task.dispatched",
    "task.status",
    "task.pr_ready",
    "task.merged",
    "task.cleaned_up",
];

/// Zone offset lookup, as [`audit_envelope::validate`] takes it.
pub type UtcOffset<'a> = &'a dyn Fn(&str, i64) -> Option<i32>;

/// The store cannot be trusted as history, so nothing is repaired or appended.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StoreRefusal {
    /// The refusal reason, spelled as the reference spells it (for example `hash_mismatch`).
    pub reason: String,
    /// The sequence number at which the refusal was found, when one applies.
    pub seq: Option<i64>,
    /// The segment, or `checkpoint.json`, at which the refusal was found, when one applies.
    pub segment: Option<String>,
    /// JSON Pointer paths of the failing members, when the refusal names them.
    pub paths: Vec<String>,
    /// Names of the segments the walk scanned without damage.
    pub intact_segments: Vec<String>,
}

impl StoreRefusal {
    fn new(reason: &str, seq: Option<i64>, segment: Option<&str>, paths: Vec<String>) -> Self {
        Self {
            reason: reason.to_owned(),
            seq,
            segment: segment.map(str::to_owned),
            paths,
            intact_segments: Vec::new(),
        }
    }
}

/// Why a store operation did not succeed.
#[derive(Debug)]
pub enum WriterError {
    /// The envelope does not conform to the v1 contract, so nothing is written.
    Rejected(Rejection),
    /// The store cannot be trusted as history; see [`StoreRefusal`].
    Refused(StoreRefusal),
    /// The store directory does not exist.
    MissingStore,
    /// A record or checkpoint holds a number that canonical JSON cannot reproduce exactly.
    Canonical(CanonicalJsonError),
    /// A filesystem operation failed before the record could be committed.
    Io(io::Error),
    /// The append did not commit. `may_be_durable` is true when the record may already be on disk;
    /// it is false when the failed write was rolled back to its committed length.
    WriteFailed {
        may_be_durable: bool,
        source: io::Error,
    },
}

impl From<io::Error> for WriterError {
    fn from(error: io::Error) -> Self {
        Self::Io(error)
    }
}

impl From<CanonicalJsonError> for WriterError {
    fn from(error: CanonicalJsonError) -> Self {
        Self::Canonical(error)
    }
}

impl From<StoreRefusal> for WriterError {
    fn from(refusal: StoreRefusal) -> Self {
        Self::Refused(refusal)
    }
}

/// A checkpoint publish that did not complete.
enum PublishError {
    /// The checkpoint was not replaced.
    Io(io::Error),
    /// The checkpoint was replaced, but the directory sync that makes the replace durable failed.
    CommitUnknown(io::Error),
}

impl From<PublishError> for WriterError {
    fn from(error: PublishError) -> Self {
        match error {
            PublishError::Io(error) => Self::Io(error),
            // Repair writes no record, so the caller's record is not on disk.
            PublishError::CommitUnknown(source) => Self::WriteFailed {
                may_be_durable: false,
                source,
            },
        }
    }
}

/// An uncommitted record that a repair adopted into the checkpoint.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Adopted {
    /// The sequence number of the adopted record.
    pub seq: i64,
    /// The segment that holds the adopted record.
    pub segment: String,
}

/// What the repair before an append did, as the reference's `recovered` result member.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Recovery {
    /// The unterminated tail that was truncated, when there was one.
    pub torn_tail: Option<TornTail>,
    /// The uncommitted record that was adopted, when there was one.
    pub adopted: Option<Adopted>,
}

impl Recovery {
    /// True when the repair changed nothing, so the append result has no `recovered` member.
    pub fn is_empty(&self) -> bool {
        self.torn_tail.is_none() && self.adopted.is_none()
    }
}

/// A committed append.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Committed {
    /// The sequence number the writer assigned.
    pub seq: i64,
    /// The `event_id` of the committed envelope.
    pub event_id: String,
    /// The repair the append ran before it committed.
    pub recovered: Recovery,
}

/// One segment as a verification walk saw it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SegmentEntry {
    /// The segment file name.
    pub name: String,
    /// The number of records the walk read from it before any refusal.
    pub records: u64,
    /// `intact`, or `damaged` when the walk refused it.
    pub status: &'static str,
}

/// An unterminated tail after the last newline of the last segment.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TornTail {
    /// The segment holding the tail.
    pub segment: String,
    /// The number of tail bytes.
    pub bytes: u64,
}

/// The result of a read-only walk of the whole store.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct VerifyReport {
    /// The number of records in the walk, which is `last_seq + 1`.
    pub records: i64,
    /// The sequence number the checkpoint commits.
    pub committed_seq: i64,
    /// The last sequence number present in the segments.
    pub last_seq: i64,
    /// Every segment in order.
    pub segments: Vec<SegmentEntry>,
    /// The unterminated tail, when one exists.
    pub torn_tail: Option<TornTail>,
    /// The sequence number of a record written but not yet committed by the checkpoint.
    pub uncommitted_seq: Option<i64>,
}

/// The checkpoint state. `last_seq` is -1 for an empty store, as the reference's genesis.
#[derive(Debug, Clone, PartialEq, Eq)]
struct Checkpoint {
    last_seq: i64,
    last_hash: Option<String>,
    segment: Option<String>,
}

const GENESIS: Checkpoint = Checkpoint {
    last_seq: -1,
    last_hash: None,
    segment: None,
};

/// One hash-chained store directory.
pub struct AuditStore<'a> {
    dir: PathBuf,
    segment_records: u64,
    utc_offset_secs: UtcOffset<'a>,
}

impl<'a> AuditStore<'a> {
    /// A store at `dir` that rotates segments every [`SEGMENT_RECORDS`] records.
    pub fn new(dir: impl Into<PathBuf>, utc_offset_secs: UtcOffset<'a>) -> Self {
        Self::with_segment_records(dir, SEGMENT_RECORDS, utc_offset_secs)
    }

    /// A store at `dir` that rotates segments every `segment_records` records.
    pub fn with_segment_records(
        dir: impl Into<PathBuf>,
        segment_records: u64,
        utc_offset_secs: UtcOffset<'a>,
    ) -> Self {
        Self {
            dir: dir.into(),
            segment_records,
            utc_offset_secs,
        }
    }

    /// Validates `envelope`, assigns `seq` and `prev_hash`, and commits the record.
    ///
    /// The caller must not supply `seq` or `prev_hash`; either one is a rejection.
    pub fn append(&self, envelope: Value) -> Result<Committed, WriterError> {
        let mut record = self.checked_record(envelope)?;
        let event_id = record
            .get("event_id")
            .and_then(Value::as_str)
            .map(str::to_owned)
            .unwrap_or_default();
        create_store_dir(&self.dir)?;
        let _lock = StoreLock::acquire(&self.dir, false)?;
        let (state, recovered) = self.repair()?;
        let seq = state.last_seq + 1;
        record.insert("seq".to_owned(), json!(seq));
        record.insert(
            "prev_hash".to_owned(),
            state.last_hash.clone().map_or(Value::Null, Value::String),
        );
        self.commit(&Value::Object(record), seq, &state)?;
        Ok(Committed {
            seq,
            event_id,
            recovered,
        })
    }

    /// Walks every segment and the checkpoint without repairing anything.
    pub fn verify(&self) -> Result<VerifyReport, WriterError> {
        self.require_dir()?;
        let _lock = StoreLock::acquire(&self.dir, true)?;
        let walk = self.walk()?;
        Ok(VerifyReport {
            records: walk.last_seq + 1,
            committed_seq: walk.ckpt.last_seq,
            last_seq: walk.last_seq,
            segments: walk.listing,
            torn_tail: walk.torn.map(|torn| TornTail {
                segment: torn.segment,
                bytes: torn.bytes,
            }),
            uncommitted_seq: (walk.ahead > 0).then_some(walk.last_seq),
        })
    }

    /// The fleet-ledger v1 base record of each committed envelope that has one, as canonical lines.
    pub fn ledger_v1_lines(&self) -> Result<Vec<String>, WriterError> {
        self.require_dir()?;
        let _lock = StoreLock::acquire(&self.dir, true)?;
        let walk = self.walk()?;
        let mut lines = Vec::new();
        for doc in &walk.records {
            let seq = doc.get("seq").and_then(Value::as_i64).unwrap_or(-1);
            if seq > walk.ckpt.last_seq {
                continue;
            }
            let Some(projected) = ledger_v1_record(doc)? else {
                continue;
            };
            if audit_envelope::validate(&projected, Definition::FleetLedgerV1, self.utc_offset_secs)
                .is_err()
            {
                return Err(
                    StoreRefusal::new("ledger_projection_invalid", None, None, Vec::new()).into(),
                );
            }
            lines.push(try_to_canonical_string(&projected)?);
        }
        Ok(lines)
    }

    fn require_dir(&self) -> Result<(), WriterError> {
        if self.dir.is_dir() {
            Ok(())
        } else {
            Err(WriterError::MissingStore)
        }
    }

    /// Checks a caller's envelope before the store is touched and returns it as a record map.
    ///
    /// The writer assigns `seq` and `prev_hash`, so the check runs on a placeholder genesis
    /// position, which the assigned position matches by construction.
    fn checked_record(&self, envelope: Value) -> Result<Map<String, Value>, WriterError> {
        let Value::Object(record) = envelope else {
            return Err(WriterError::Rejected(Rejection {
                reason: Reason::SchemaInvalid,
                paths: vec![String::new()],
            }));
        };
        for member in ["seq", "prev_hash"] {
            if record.contains_key(member) {
                return Err(WriterError::Rejected(Rejection {
                    reason: Reason::SchemaInvalid,
                    paths: vec![format!("/{member}")],
                }));
            }
        }
        let mut placeholder = record.clone();
        placeholder.insert("seq".to_owned(), json!(0));
        placeholder.insert("prev_hash".to_owned(), Value::Null);
        audit_envelope::validate(
            &Value::Object(placeholder),
            Definition::Envelope,
            self.utc_offset_secs,
        )
        .map_err(WriterError::Rejected)?;
        Ok(record)
    }

    /// Verifies the store, then performs the repairs the next append needs: genesis checkpoint
    /// for an empty store, truncation of a torn tail, and adoption of one uncommitted record.
    /// Returns the committed state the new record links to, and what the repairs changed.
    fn repair(&self) -> Result<(Checkpoint, Recovery), WriterError> {
        let walk = self.walk()?;
        let state = Checkpoint {
            last_seq: walk.last_seq,
            last_hash: walk.last_hash.clone(),
            segment: walk.last_segment.clone(),
        };
        let mut recovered = Recovery::default();
        if !walk.present {
            self.publish_checkpoint(&GENESIS)?;
        }
        if let Some(torn) = &walk.torn {
            truncate_durably(&self.dir.join(&torn.segment), torn.good_size)?;
            recovered.torn_tail = Some(TornTail {
                segment: torn.segment.clone(),
                bytes: torn.bytes,
            });
        }
        if walk.ahead == 1 {
            self.publish_checkpoint(&state)?;
            recovered.adopted = state.segment.clone().map(|segment| Adopted {
                seq: state.last_seq,
                segment,
            });
        }
        Ok((state, recovered))
    }

    fn commit(&self, record: &Value, seq: i64, state: &Checkpoint) -> Result<(), WriterError> {
        let line = try_to_canonical_string(record)?;
        let rotate = match state.segment.as_deref().and_then(segment_first_seq) {
            None => true,
            Some(first) => i128::from(seq) - first >= i128::from(self.segment_records),
        };
        let name = match (rotate, &state.segment) {
            (false, Some(segment)) => segment.clone(),
            _ => segment_name(seq),
        };
        let path = self.dir.join(&name);
        let mut before = None;
        if let Err(source) = append_line(&path, &line, &self.dir, rotate, &mut before) {
            return Err(WriterError::WriteFailed {
                may_be_durable: before.is_some_and(|size| !rollback(&path, size)),
                source,
            });
        }
        let checkpoint = Checkpoint {
            last_seq: seq,
            last_hash: Some(audit_hash(line.as_bytes())),
            segment: Some(name),
        };
        match self.publish_checkpoint(&checkpoint) {
            Ok(()) => Ok(()),
            Err(PublishError::CommitUnknown(source)) => Err(WriterError::WriteFailed {
                may_be_durable: true,
                source,
            }),
            Err(PublishError::Io(source)) => Err(WriterError::WriteFailed {
                may_be_durable: before.is_some_and(|size| !rollback(&path, size)),
                source,
            }),
        }
    }

    /// Replaces the checkpoint through a temporary file, which is the commit point. A failure before
    /// the replace removes the temporary file, as the reference does. A failed directory sync after
    /// it leaves the commit outcome unknown.
    fn publish_checkpoint(&self, state: &Checkpoint) -> Result<(), PublishError> {
        let data = try_to_canonical_string(&json!({
            "v": 1,
            "last_seq": state.last_seq,
            "last_hash": state.last_hash,
            "segment": state.segment,
        }))
        .map_err(|error| PublishError::Io(io::Error::new(io::ErrorKind::InvalidData, error)))?;
        let tmp = self.dir.join(CHECKPOINT_TMP);
        let replaced = write_synced(&tmp, format!("{data}\n").as_bytes())
            .and_then(|()| fs::rename(&tmp, self.dir.join(CHECKPOINT)));
        if let Err(error) = replaced {
            // A failed removal is ignored, as the reference suppresses it.
            let _ = fs::remove_file(&tmp);
            return Err(PublishError::Io(error));
        }
        sync_dir(&self.dir).map_err(PublishError::CommitUnknown)
    }

    fn segments(&self) -> Result<Vec<(i128, String)>, WriterError> {
        let mut found = Vec::new();
        for entry in fs::read_dir(&self.dir)? {
            let entry = entry?;
            let Ok(name) = entry.file_name().into_string() else {
                continue;
            };
            let Some(first) = segment_first_seq(&name) else {
                continue;
            };
            if !entry.file_type()?.is_file() {
                return Err(StoreRefusal::new(
                    "segment_invalid",
                    None,
                    Some(name.as_str()),
                    Vec::new(),
                )
                .into());
            }
            found.push((first, name));
        }
        found.sort();
        Ok(found)
    }

    fn checkpoint(&self, segments: &[(i128, String)]) -> Result<(Checkpoint, bool), WriterError> {
        match fs::read(self.dir.join(CHECKPOINT)) {
            Ok(raw) => Ok((parse_checkpoint(&raw)?, true)),
            Err(error) if error.kind() == io::ErrorKind::NotFound => {
                if segments.is_empty() {
                    Ok((GENESIS, false))
                } else {
                    Err(
                        StoreRefusal::new("checkpoint_missing", None, Some(CHECKPOINT), Vec::new())
                            .into(),
                    )
                }
            }
            Err(error) => Err(error.into()),
        }
    }

    /// Verifies every segment and the checkpoint. The first damage is reported together with the
    /// segments that stayed intact, as the reference does.
    fn walk(&self) -> Result<Walk, WriterError> {
        let segments = self.segments()?;
        let (ckpt, present) = self.checkpoint(&segments)?;
        let mut walk = Walk {
            ckpt,
            present,
            last_seq: -1,
            last_hash: None,
            last_segment: None,
            ahead: 0,
            checkpoint_seen: false,
            torn: None,
            listing: Vec::new(),
            records: Vec::new(),
        };
        let mut damage: Option<StoreRefusal> = None;
        let mut next_seq: Option<i64> = Some(0);
        let mut link: Option<String> = None;
        for (index, (first, name)) in segments.iter().enumerate() {
            let raw = fs::read(self.dir.join(name))?;
            let (lines, tail) = split_segment(&raw);
            let segment = SegmentBytes {
                name: name.as_str(),
                first: i64::try_from(*first).ok(),
                good_size: (raw.len() - tail.len()) as u64,
                lines: &lines,
                tail,
                last: index + 1 == segments.len(),
            };
            let mut entry = SegmentEntry {
                name: name.clone(),
                records: 0,
                status: "intact",
            };
            if let Err(refusal) =
                self.scan_segment(&mut walk, &mut entry, &segment, next_seq, link.as_deref())
            {
                entry.status = "damaged";
                damage.get_or_insert(refusal);
            }
            walk.listing.push(entry);
            next_seq = segment
                .first
                .and_then(|first| first.checked_add(lines.len() as i64));
            link = lines.last().map(|line| audit_hash(line));
        }
        if let Some(mut refusal) = damage.or_else(|| checkpoint_refusal(&walk)) {
            refusal.intact_segments = walk
                .listing
                .iter()
                .filter(|entry| entry.status == "intact")
                .map(|entry| entry.name.clone())
                .collect();
            return Err(refusal.into());
        }
        Ok(walk)
    }

    fn scan_segment(
        &self,
        walk: &mut Walk,
        entry: &mut SegmentEntry,
        segment: &SegmentBytes,
        next_seq: Option<i64>,
        link: Option<&str>,
    ) -> Result<(), StoreRefusal> {
        let name = Some(segment.name);
        // A name beyond the i64 range matches no reachable sequence, so it is a gap, as the reference
        // reports it, rather than a segment the walk may skip.
        let first = match segment.first {
            Some(first) if Some(first) == next_seq => first,
            _ => {
                return Err(StoreRefusal::new("segment_gap", next_seq, name, Vec::new()));
            }
        };
        if !segment.tail.is_empty() && !segment.last {
            return Err(StoreRefusal::new(
                "unterminated_record",
                Some(first + segment.lines.len() as i64),
                name,
                Vec::new(),
            ));
        }
        if segment.lines.is_empty() && !segment.last {
            return Err(StoreRefusal::new(
                "segment_empty",
                Some(first),
                name,
                Vec::new(),
            ));
        }
        if !segment.tail.is_empty() {
            walk.torn = Some(Torn {
                segment: segment.name.to_owned(),
                good_size: segment.good_size,
                bytes: segment.tail.len() as u64,
            });
        }
        for (offset, line) in segment.lines.iter().enumerate() {
            let seq = first + offset as i64;
            let doc = parse_record(line, segment.name, seq)?;
            if doc.get("seq").and_then(Value::as_i64) != Some(seq) {
                return Err(StoreRefusal::new(
                    "seq_mismatch",
                    Some(seq),
                    name,
                    vec!["/seq".to_owned()],
                ));
            }
            let expected: Option<String> = if seq == 0 {
                None
            } else if offset == 0 {
                link.map(str::to_owned)
            } else {
                Some(audit_hash(segment.lines[offset - 1]))
            };
            let actual = doc.get("prev_hash").cloned().unwrap_or(Value::Null);
            if actual != expected.map_or(Value::Null, Value::String) {
                return Err(StoreRefusal::new(
                    "hash_mismatch",
                    Some(seq),
                    name,
                    vec!["/prev_hash".to_owned()],
                ));
            }
            if let Err(rejection) =
                audit_envelope::validate(&doc, Definition::Envelope, self.utc_offset_secs)
            {
                return Err(StoreRefusal::new(
                    rejection.reason.as_str(),
                    Some(seq),
                    name,
                    rejection.paths,
                ));
            }
            let hash = audit_hash(line);
            walk.last_seq = seq;
            walk.last_hash = Some(hash.clone());
            walk.last_segment = Some(segment.name.to_owned());
            walk.records.push(doc);
            entry.records += 1;
            if seq == walk.ckpt.last_seq {
                if walk.ckpt.last_hash.as_deref() != Some(hash.as_str())
                    || walk.ckpt.segment.as_deref() != Some(segment.name)
                {
                    return Err(StoreRefusal::new(
                        "checkpoint_mismatch",
                        Some(seq),
                        name,
                        Vec::new(),
                    ));
                }
                walk.checkpoint_seen = true;
            } else if seq > walk.ckpt.last_seq {
                walk.ahead += 1;
            }
        }
        Ok(())
    }
}

/// Mutable state of one verification walk.
struct Walk {
    ckpt: Checkpoint,
    present: bool,
    last_seq: i64,
    last_hash: Option<String>,
    last_segment: Option<String>,
    ahead: u64,
    checkpoint_seen: bool,
    torn: Option<Torn>,
    listing: Vec<SegmentEntry>,
    records: Vec<Value>,
}

/// The unterminated tail of the last segment, and where its good bytes end.
struct Torn {
    segment: String,
    good_size: u64,
    bytes: u64,
}

/// One segment's bytes, split into its newline-terminated lines and an unterminated tail.
struct SegmentBytes<'a> {
    name: &'a str,
    /// The first sequence the name carries, or `None` when it is beyond the i64 range.
    first: Option<i64>,
    good_size: u64,
    lines: &'a [&'a [u8]],
    tail: &'a [u8],
    last: bool,
}

fn checkpoint_refusal(walk: &Walk) -> Option<StoreRefusal> {
    let ckpt = &walk.ckpt;
    if ckpt.last_seq >= 0 && !walk.checkpoint_seen {
        return Some(StoreRefusal::new(
            "checkpoint_mismatch",
            Some(ckpt.last_seq),
            ckpt.segment.as_deref(),
            Vec::new(),
        ));
    }
    if walk.ahead > 1 {
        return Some(StoreRefusal::new(
            "checkpoint_mismatch",
            Some(walk.last_seq),
            walk.last_segment.as_deref(),
            Vec::new(),
        ));
    }
    None
}

/// Splits a segment into its newline-terminated lines and the bytes after the last newline.
fn split_segment(raw: &[u8]) -> (Vec<&[u8]>, &[u8]) {
    let cut = raw
        .iter()
        .rposition(|&byte| byte == b'\n')
        .map_or(0, |index| index + 1);
    let (terminated, tail) = raw.split_at(cut);
    let mut lines: Vec<&[u8]> = terminated.split(|&byte| byte == b'\n').collect();
    lines.pop();
    (lines, tail)
}

fn parse_record(line: &[u8], segment: &str, seq: i64) -> Result<Value, StoreRefusal> {
    let malformed = || StoreRefusal::new("malformed_line", Some(seq), Some(segment), Vec::new());
    let text = std::str::from_utf8(line).map_err(|_| malformed())?;
    let doc: Value = serde_json::from_str(text).map_err(|_| malformed())?;
    let canonical = try_to_canonical_string(&doc).map_err(|_| {
        StoreRefusal::new("canonical_refused", Some(seq), Some(segment), Vec::new())
    })?;
    if !doc.is_object() || canonical.as_bytes() != line {
        return Err(malformed());
    }
    Ok(doc)
}

fn parse_checkpoint(raw: &[u8]) -> Result<Checkpoint, StoreRefusal> {
    let refused = || StoreRefusal::new("checkpoint_invalid", None, Some(CHECKPOINT), Vec::new());
    let text = std::str::from_utf8(raw).map_err(|_| refused())?;
    let doc: Value = serde_json::from_str(text).map_err(|_| refused())?;
    let map = doc.as_object().ok_or_else(refused)?;
    let mut keys: Vec<&str> = map.keys().map(String::as_str).collect();
    keys.sort_unstable();
    if keys != ["last_hash", "last_seq", "segment", "v"] {
        return Err(refused());
    }
    let canonical = try_to_canonical_string(&doc).map_err(|_| refused())?;
    if format!("{canonical}\n").as_bytes() != raw {
        return Err(refused());
    }
    checkpoint_fields(map).ok_or_else(refused)
}

/// The checkpoint's member rules: `v` is the integer 1, `last_seq` an integer at least -1, and
/// the hash and segment present only for a committed sequence.
fn checkpoint_fields(map: &Map<String, Value>) -> Option<Checkpoint> {
    if map.get("v")?.as_i64()? != 1 {
        return None;
    }
    let last_seq = map.get("last_seq")?.as_i64()?;
    if last_seq < -1 {
        return None;
    }
    let (last_hash, segment) = (map.get("last_hash")?, map.get("segment")?);
    if last_seq == -1 {
        return (last_hash.is_null() && segment.is_null()).then_some(GENESIS);
    }
    let last_hash = last_hash.as_str().filter(|hash| is_audit_hash(hash))?;
    let segment = segment
        .as_str()
        .filter(|name| segment_first_seq(name).is_some())?;
    Some(Checkpoint {
        last_seq,
        last_hash: Some(last_hash.to_owned()),
        segment: Some(segment.to_owned()),
    })
}

/// The fleet-ledger v1 base record of one stored envelope, or `None` for other events.
fn ledger_v1_record(doc: &Value) -> Result<Option<Value>, WriterError> {
    let event = doc.get("event").and_then(Value::as_str);
    let Some(task) = doc.get("task_id") else {
        return Ok(None);
    };
    if !event.is_some_and(|event| LEDGER_V1_EVENTS.contains(&event)) {
        return Ok(None);
    }
    // Stored records passed the envelope check, so the timestamp has the v1 calendar form.
    let (year, month, day, hour, minute, second) = doc
        .get("ts")
        .and_then(Value::as_str)
        .and_then(parse_ts_calendar)
        .ok_or_else(|| StoreRefusal::new("ledger_projection_invalid", None, None, Vec::new()))?;
    let ts = days_from_civil(year, month, day) * 86_400
        + i64::from(hour) * 3_600
        + i64::from(minute) * 60
        + i64::from(second);
    let mut record = Map::new();
    record.insert("v".to_owned(), json!(1));
    record.insert("ts".to_owned(), json!(ts));
    record.insert("event".to_owned(), json!(event));
    record.insert("task".to_owned(), task.clone());
    if let Some(pr) = doc.get("pr").filter(|pr| !pr.is_null()) {
        record.insert("pr".to_owned(), pr.clone());
    }
    Ok(Some(Value::Object(record)))
}

fn segment_name(seq: i64) -> String {
    format!("segment-{seq:020}.jsonl")
}

/// The first sequence a segment name carries, parsed wide enough for every twenty-digit name, as
/// the reference's `SEGMENT_RE` accepts them.
fn segment_first_seq(name: &str) -> Option<i128> {
    let digits = name.strip_prefix("segment-")?.strip_suffix(".jsonl")?;
    if digits.len() != 20 || !digits.bytes().all(|byte| byte.is_ascii_digit()) {
        return None;
    }
    digits.parse().ok()
}

fn create_store_dir(path: &Path) -> io::Result<()> {
    let mut builder = DirBuilder::new();
    builder.recursive(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::DirBuilderExt;
        builder.mode(0o700);
    }
    builder.create(path)
}

/// The store's `.lock` file, held until dropped. Appends take it exclusively and reads shared, as
/// the reference's `_locked` does.
#[cfg(unix)]
struct StoreLock {
    /// Closing the descriptor on drop releases the `flock`.
    _file: File,
}

#[cfg(unix)]
impl StoreLock {
    /// Blocks until the lock is held, as the reference's blocking `flock` does.
    fn acquire(dir: &Path, shared: bool) -> io::Result<Self> {
        let mut options = OpenOptions::new();
        options.read(true).write(true).create(true);
        set_private_mode(&mut options);
        let file = options.open(dir.join(LOCK))?;
        loop {
            let locked = if shared {
                file.lock_shared()
            } else {
                file.lock()
            };
            match locked {
                Ok(()) => return Ok(Self { _file: file }),
                // Python's fcntl.flock retries an interrupted call (PEP 475).
                Err(error) if error.kind() == io::ErrorKind::Interrupted => {}
                Err(error) => return Err(error),
            }
        }
    }
}

/// Off unix the store is not locked. See the module docs.
#[cfg(not(unix))]
struct StoreLock;

#[cfg(not(unix))]
impl StoreLock {
    fn acquire(_dir: &Path, _shared: bool) -> io::Result<Self> {
        Ok(Self)
    }
}

fn open_append(path: &Path) -> io::Result<File> {
    let mut options = OpenOptions::new();
    options.create(true).append(true);
    set_private_mode(&mut options);
    options.open(path)
}

/// Appends `line` and its newline to the segment, then syncs the segment, and the directory when the
/// segment is new. `before` receives the segment's length before the append, which a failed append
/// rolls back to.
fn append_line(
    path: &Path,
    line: &str,
    dir: &Path,
    rotate: bool,
    before: &mut Option<u64>,
) -> io::Result<()> {
    let mut file = open_append(path)?;
    *before = Some(file.metadata()?.len());
    file.write_all(format!("{line}\n").as_bytes())?;
    file.sync_all()?;
    drop(file);
    if rotate {
        sync_dir(dir)?;
    }
    Ok(())
}

/// Truncates a segment back to `size` after a failed append. True only when the truncation is
/// durable, as the reference's `rollback`.
fn rollback(path: &Path, size: u64) -> bool {
    truncate_durably(path, size).is_ok()
}

fn write_synced(path: &Path, bytes: &[u8]) -> io::Result<()> {
    let mut file = open_create(path)?;
    file.write_all(bytes)?;
    file.sync_all()
}

fn open_create(path: &Path) -> io::Result<File> {
    let mut options = OpenOptions::new();
    options.create(true).write(true).truncate(true);
    set_private_mode(&mut options);
    options.open(path)
}

#[cfg(unix)]
fn set_private_mode(options: &mut OpenOptions) {
    use std::os::unix::fs::OpenOptionsExt;
    options.mode(0o600);
}

#[cfg(not(unix))]
fn set_private_mode(_options: &mut OpenOptions) {}

fn truncate_durably(path: &Path, size: u64) -> io::Result<()> {
    let file = OpenOptions::new().write(true).open(path)?;
    file.set_len(size)?;
    file.sync_all()
}

fn sync_dir(path: &Path) -> io::Result<()> {
    File::open(path)?.sync_all()
}

#[cfg(test)]
mod tests {
    use super::*;
    #[cfg(unix)]
    use std::process::{Child, Command};
    #[cfg(unix)]
    use std::sync::mpsc;
    #[cfg(unix)]
    use std::time::Duration;
    use std::time::{SystemTime, UNIX_EPOCH};

    const APPEND_INPUTS: [&str; 3] = [
        include_str!(
            "../../../tests/fixtures/pre-port-baseline/outputs/fm-audit-writer/chain/inputs/append-0.json"
        ),
        include_str!(
            "../../../tests/fixtures/pre-port-baseline/outputs/fm-audit-writer/chain/inputs/append-1.json"
        ),
        include_str!(
            "../../../tests/fixtures/pre-port-baseline/outputs/fm-audit-writer/chain/inputs/append-2.json"
        ),
    ];

    /// The one zone the committed append inputs name, at their instants (daylight time, UTC-5).
    fn chicago(zone: &str, _utc_epoch_secs: i64) -> Option<i32> {
        (zone == "America/Chicago").then_some(-5 * 3600)
    }

    /// A scratch directory, removed when dropped.
    struct Scratch(PathBuf);

    impl Scratch {
        fn new(label: &str) -> Self {
            let nanos = SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .expect("the clock is after the epoch")
                .as_nanos();
            Self(std::env::temp_dir().join(format!(
                "mcao-audit-writer-{label}-{}-{nanos}",
                std::process::id()
            )))
        }

        fn store(&self) -> PathBuf {
            self.0.join("store")
        }
    }

    impl Drop for Scratch {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.0);
        }
    }

    fn envelope(index: usize) -> Value {
        serde_json::from_str(APPEND_INPUTS[index]).expect("committed append input parses")
    }

    #[cfg(unix)]
    fn segment_lines(store: &Path, name: &str) -> Vec<String> {
        fs::read_to_string(store.join(name))
            .expect("reading segment")
            .lines()
            .map(str::to_owned)
            .collect()
    }

    /// The environment variable that names the store a re-executed test binary appends to.
    #[cfg(unix)]
    const CHILD_STORE: &str = "MCAO_AUDIT_WRITER_TEST_CHILD_STORE";
    #[cfg(unix)]
    const CHILD_APPENDS: usize = 100;
    #[cfg(unix)]
    const TEST_TIMEOUT: Duration = Duration::from_secs(30);

    #[test]
    fn audit_writer_empty_store_verifies_as_genesis() {
        let scratch = Scratch::new("genesis");
        fs::create_dir_all(scratch.store()).expect("creating store");
        let report = AuditStore::new(scratch.store(), &chicago)
            .verify()
            .expect("empty store verifies");
        assert_eq!(report.records, 0);
        assert_eq!(report.committed_seq, -1);
        assert_eq!(report.last_seq, -1);
        assert!(report.segments.is_empty());
        assert_eq!(report.uncommitted_seq, None);
    }

    // The tests that append are unix-only. Off unix `sync_dir` opens the store directory with
    // `File::open`, which Windows refuses (os error 5), and the CLI refuses the writer off unix.
    // The empty-store and rejected-envelope tests run on every platform.
    #[cfg(unix)]
    #[test]
    fn audit_writer_links_each_record_to_the_hash_of_the_previous_line() {
        let scratch = Scratch::new("link");
        let store = AuditStore::new(scratch.store(), &chicago);
        for index in 0..3 {
            store.append(envelope(index)).expect("append commits");
        }
        let lines = segment_lines(&scratch.store(), "segment-00000000000000000000.jsonl");
        assert_eq!(lines.len(), 3);
        assert_eq!(
            serde_json::from_str::<Value>(&lines[0]).unwrap()["prev_hash"],
            Value::Null
        );
        for offset in 1..3 {
            let prev: Value = serde_json::from_str(&lines[offset]).unwrap();
            assert_eq!(
                prev["prev_hash"],
                json!(audit_hash(lines[offset - 1].as_bytes()))
            );
            assert_eq!(prev["seq"], json!(offset));
        }
        assert_eq!(store.verify().expect("chain verifies").last_seq, 2);
    }

    #[cfg(unix)]
    #[test]
    fn audit_writer_refuses_a_record_edited_after_commit() {
        let scratch = Scratch::new("tamper");
        let store = AuditStore::new(scratch.store(), &chicago);
        for index in 0..2 {
            store.append(envelope(index)).expect("append commits");
        }
        let name = "segment-00000000000000000000.jsonl";
        let path = scratch.store().join(name);
        let edited = fs::read_to_string(&path)
            .expect("reading segment")
            .replacen("fix-login", "fix-logix", 1);
        fs::write(&path, edited).expect("writing edited segment");
        match store.verify() {
            Err(WriterError::Refused(refusal)) => {
                assert_eq!(refusal.reason, "hash_mismatch");
                assert_eq!(refusal.seq, Some(1));
                assert_eq!(refusal.segment.as_deref(), Some(name));
                assert!(refusal.intact_segments.is_empty());
            }
            other => panic!("edited record must be refused, got {other:?}"),
        }
    }

    #[cfg(unix)]
    #[test]
    fn audit_writer_rotates_segments_and_links_across_them() {
        let scratch = Scratch::new("rotate");
        let store = AuditStore::with_segment_records(scratch.store(), 2, &chicago);
        for index in 0..3 {
            store.append(envelope(index)).expect("append commits");
        }
        let report = store.verify().expect("rotated chain verifies");
        assert_eq!(report.records, 3);
        let names: Vec<&str> = report
            .segments
            .iter()
            .map(|segment| segment.name.as_str())
            .collect();
        assert_eq!(
            names,
            [
                "segment-00000000000000000000.jsonl",
                "segment-00000000000000000002.jsonl"
            ]
        );
    }

    #[test]
    fn audit_writer_refuses_caller_seq_and_leaves_the_store_untouched() {
        let scratch = Scratch::new("caller-seq");
        let mut document = envelope(0);
        document["seq"] = json!(0);
        let store = AuditStore::new(scratch.store(), &chicago);
        match store.append(document) {
            Err(WriterError::Rejected(rejection)) => {
                assert_eq!(rejection.reason, Reason::SchemaInvalid);
                assert_eq!(rejection.paths, vec!["/seq".to_owned()]);
            }
            other => panic!("caller seq must be rejected, got {other:?}"),
        }
        assert!(
            !scratch.store().exists(),
            "a rejected envelope must not create the store"
        );
    }

    #[cfg(unix)]
    const FIRST_SEGMENT: &str = "segment-00000000000000000000.jsonl";

    /// Expected results come from `bin/fm-audit-writer.py` at d80bcd15, run offline with
    /// `python3 -I -B -X utf8` on a copy of the three-record chain store plus one empty file of the
    /// tested name: `verify --store DIR` and `append --store DIR <inputs>/append-0.json` both exit
    /// with `reason` `segment_gap`, `seq` 3, the tested name as `segment`, and only the first
    /// segment intact. Each name has 20 digits and a value beyond i64; one carries a leading zero.
    #[cfg(unix)]
    #[test]
    fn audit_writer_refuses_a_segment_name_beyond_i64_as_a_gap() {
        for name in [
            "segment-99999999999999999999.jsonl",
            "segment-18446744073709551615.jsonl",
            "segment-09223372036854775808.jsonl",
        ] {
            let scratch = Scratch::new("segment-gap");
            let store = AuditStore::new(scratch.store(), &chicago);
            for index in 0..3 {
                store.append(envelope(index)).expect("append commits");
            }
            fs::write(scratch.store().join(name), b"").expect("writing rogue segment");
            let committed = fs::read(scratch.store().join(FIRST_SEGMENT)).expect("reading segment");
            let expected = StoreRefusal {
                reason: "segment_gap".to_owned(),
                seq: Some(3),
                segment: Some(name.to_owned()),
                paths: Vec::new(),
                intact_segments: vec![FIRST_SEGMENT.to_owned()],
            };
            match store.verify() {
                Err(WriterError::Refused(refusal)) => {
                    assert_eq!(refusal, expected, "verify beside {name}");
                }
                other => panic!("verify beside {name} must be refused, got {other:?}"),
            }
            match store.append(envelope(0)) {
                Err(WriterError::Refused(refusal)) => {
                    assert_eq!(refusal, expected, "append beside {name}");
                }
                other => panic!("append beside {name} must be refused, got {other:?}"),
            }
            assert_eq!(
                fs::read(scratch.store().join(FIRST_SEGMENT)).expect("reading segment"),
                committed,
                "a refused append must leave the committed segment unchanged"
            );
        }
    }

    /// The reference's `SEGMENT_RE` does not match a 21-digit name, so it neither scans nor refuses
    /// that file. The same offline run gives `verify` exit 0 with `last_seq` 2, and `append` commits
    /// seq 3 into the first segment.
    #[cfg(unix)]
    #[test]
    fn audit_writer_skips_a_segment_name_outside_the_reference_pattern() {
        let scratch = Scratch::new("segment-pattern");
        let store = AuditStore::new(scratch.store(), &chicago);
        for index in 0..3 {
            store.append(envelope(index)).expect("append commits");
        }
        fs::write(
            scratch.store().join("segment-100000000000000000000.jsonl"),
            b"",
        )
        .expect("writing unmatched name");
        assert_eq!(
            store.verify().expect("unmatched name is skipped").last_seq,
            2
        );
        assert_eq!(store.append(envelope(0)).expect("append commits").seq, 3);
        assert_eq!(segment_lines(&scratch.store(), FIRST_SEGMENT).len(), 4);
    }

    #[cfg(unix)]
    #[test]
    fn audit_writer_append_and_verify_wait_for_the_store_lock() {
        let scratch = Scratch::new("lock-held");
        fs::create_dir_all(scratch.store()).expect("creating store");
        let held = StoreLock::acquire(&scratch.store(), false).expect("holding the lock");
        let (append_done, append_result) = mpsc::channel();
        let (verify_done, verify_result) = mpsc::channel();
        std::thread::scope(|scope| {
            let store = scratch.store();
            scope.spawn(move || {
                let outcome = AuditStore::new(store, &chicago).append(envelope(0));
                let _ = append_done.send(outcome.map(|committed| committed.seq));
            });
            let store = scratch.store();
            scope.spawn(move || {
                let outcome = AuditStore::new(store, &chicago).verify();
                let _ = verify_done.send(outcome.map(|report| report.records));
            });
            std::thread::sleep(Duration::from_millis(300));
            assert!(
                append_result.try_recv().is_err(),
                "append must wait while the lock is held"
            );
            assert!(
                verify_result.try_recv().is_err(),
                "verify must wait while the exclusive lock is held"
            );
            drop(held);
            let seq = append_result
                .recv_timeout(TEST_TIMEOUT)
                .expect("append finishes once the lock is released")
                .expect("append commits");
            assert_eq!(seq, 0);
            verify_result
                .recv_timeout(TEST_TIMEOUT)
                .expect("verify finishes once the lock is released")
                .expect("verify succeeds");
        });
    }

    /// A record written before its checkpoint publish failed is rolled back: the segment returns to
    /// its committed length, and the store verifies and takes the next append. The failure is real:
    /// a directory at the checkpoint's temporary path makes the publish fail. Run through the reference
    /// (d80bcd15, offline, `python3 -I -B -X utf8`) on the same store, `append` exits 3 with
    /// `may_be_durable` false and an unchanged segment.
    #[cfg(unix)]
    #[test]
    fn audit_writer_rolls_back_a_record_whose_checkpoint_publish_failed() {
        let scratch = Scratch::new("rollback");
        let store = AuditStore::new(scratch.store(), &chicago);
        for index in 0..3 {
            store.append(envelope(index)).expect("append commits");
        }
        let segment = scratch.store().join(FIRST_SEGMENT);
        let committed = fs::read(&segment).expect("reading segment");
        let blocker = scratch.store().join(CHECKPOINT_TMP);
        fs::create_dir(&blocker).expect("blocking the checkpoint temporary file");
        match store.append(envelope(0)) {
            Err(WriterError::WriteFailed { may_be_durable, .. }) => {
                assert!(!may_be_durable, "a rolled-back record is not durable");
            }
            other => panic!("a failed publish must report a failed write, got {other:?}"),
        }
        assert_eq!(
            fs::read(&segment).expect("reading segment"),
            committed,
            "the failed record must be rolled back to the committed length"
        );
        fs::remove_dir(&blocker).expect("removing the blocking directory");
        assert_eq!(
            store
                .verify()
                .expect("store verifies after rollback")
                .last_seq,
            2
        );
        assert_eq!(store.append(envelope(0)).expect("append commits").seq, 3);
    }

    /// Re-executed by `audit_writer_two_processes_append_without_losing_or_duplicating_sequence_numbers`
    /// with [`CHILD_STORE`] set. Without the variable it does nothing.
    #[cfg(unix)]
    #[test]
    fn audit_writer_child_appends_to_the_store_named_in_the_environment() {
        let Some(store) = std::env::var_os(CHILD_STORE) else {
            return;
        };
        let store = AuditStore::new(PathBuf::from(store), &chicago);
        for index in 0..CHILD_APPENDS {
            store
                .append(envelope(index % APPEND_INPUTS.len()))
                .expect("child append commits");
        }
    }

    #[cfg(unix)]
    #[test]
    fn audit_writer_two_processes_append_without_losing_or_duplicating_sequence_numbers() {
        let scratch = Scratch::new("two-processes");
        fs::create_dir_all(scratch.store()).expect("creating store");
        let exe = std::env::current_exe().expect("locating the test binary");
        let children: Vec<Child> = (0..2)
            .map(|_| {
                Command::new(&exe)
                    .args([
                        "--exact",
                        "audit_writer::tests::audit_writer_child_appends_to_the_store_named_in_the_environment",
                        "--nocapture",
                    ])
                    .env(CHILD_STORE, scratch.store())
                    .spawn()
                    .expect("spawning child append process")
            })
            .collect();
        for mut child in children {
            let status = child.wait().expect("waiting for child append process");
            assert!(status.success(), "child append process failed: {status}");
        }
        let total = 2 * CHILD_APPENDS as i64;
        let report = AuditStore::new(scratch.store(), &chicago)
            .verify()
            .expect("store verifies after concurrent appends");
        assert_eq!(report.records, total);
        assert_eq!(report.committed_seq, total - 1);
        assert_eq!(report.last_seq, total - 1);
    }
}
