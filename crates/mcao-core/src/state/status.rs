//! The status log: an append-only event stream per task, and the keyed open-decision fold over it.
//!
//! `bin/fm-classify-lib.sh` defines the fold, and this module reproduces it. The fixtures under
//! `tests/fixtures/status/` hold the shell's own output and are the parity reference. Four places
//! differ from the shell on purpose. They are documented on [`StatusLog::open_decisions`] and
//! [`StatusLog::append`]: an unreadable or non-regular status file is an error, not an empty set;
//! a close whose key token never terminates is refused; non-UTF-8 bytes are replaced; and the
//! shell's environment overrides for the verbs and the reserved namespace are not read here.

use std::fs::{self, File, OpenOptions};
use std::io::{self, Read, Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};

use crate::error::McaoError;

use super::lock::{lock_sidecar, refuse_non_regular};

/// The verb that closes a keyed decision. The shell's `FM_CLASSIFY_RESOLVE_VERB_DEFAULT`.
const RESOLVE_VERB: &str = "resolved";

/// The held-close token: the verb that closes a keyed decision by holding it in the backlog.
/// Copied from the shell classifier's held-verb default, so the two stay byte-for-byte equal.
pub const HELD_VERB: &str = "captain-held";

/// The verbs that open a keyed decision.
const OPENING_VERBS: [&str; 2] = ["needs-decision", "blocked"];

/// The key given to a line that states no key token. The shell's `default`.
const DEFAULT_KEY: &str = "default";

/// A decision-key namespace that only its owner's vocabulary may open or close. The shell's
/// `FM_CLASSIFY_RESERVED_KEY_PREFIXES_DEFAULT`.
const RESERVED_KEY_PREFIX: &str = "pending-reply-";

/// Opens a key token, `[key=<slug>]`.
const KEY_OPENER: &str = "[key=";

/// Opens a worker-written time stamp, `[at=<epoch>]`, which the fold ignores.
const STAMP_OPENER: &str = "[at=";

/// The kind of task a status log belongs to. It decides whether that task's terminal lines clear
/// the open set.
#[non_exhaustive]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum StatusKind {
    /// A ship. A `done:` or `failed:` line clears every open decision.
    Ship,
    /// A scout. A `done:` or `failed:` line clears every open decision, as for a ship.
    Scout,
    /// A secondmate. Its terminal lines may describe other work, so they clear nothing.
    Secondmate,
    /// Any kind the shell does not recognise. Its terminal lines clear nothing, as for a
    /// secondmate.
    Unknown,
}

/// A decision that a status log still holds open.
#[non_exhaustive]
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct OpenDecision {
    /// The decision key. A line with no key token uses `default`.
    pub key: String,
    /// The verb that opened the decision: `needs-decision` or `blocked`.
    pub verb: String,
    /// The text after the line's first colon, with a key token at the note head removed.
    pub note: String,
}

/// The bytes one append wrote, counted from the start of the status file. `start` is the offset of
/// the first byte written and `end` is the offset just past the last.
#[non_exhaustive]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ByteRange {
    /// The offset of the first byte the append wrote.
    pub start: u64,
    /// The offset just past the last byte the append wrote.
    pub end: u64,
}

/// One task's status file, appended to and folded as an event log.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct StatusLog {
    path: PathBuf,
}

impl StatusLog {
    /// Names the status file at `path`. Nothing is opened until a method runs.
    pub fn new(path: impl Into<PathBuf>) -> Self {
        Self { path: path.into() }
    }

    /// Returns the path of the status file.
    pub fn path(&self) -> &Path {
        &self.path
    }

    /// Appends `line` as one event, followed by a newline, and returns the byte range the append
    /// wrote.
    ///
    /// Appenders in this crate take a blocking exclusive lock on a sidecar file, the status path
    /// with `.lock` appended, before the torn-tail check, and release it after the write. The lock
    /// exists because the torn-tail check reads the last byte of the file. On Linux a write becomes
    /// visible page by page, so without the lock a check that runs during another appender's write
    /// can see a mid-record byte, take the line for torn, and add a blank separator line. That
    /// happened under 8 threads appending 500 events each.
    ///
    /// The lock is on the sidecar and never on the status file, so readers take no lock and a
    /// Windows mandatory lock cannot make a read fail. Closing the sidecar releases the lock.
    ///
    /// The shell appenders do not take this lock, so a race between them and this crate can still
    /// add one blank separator line. Both folds ignore blank lines, so the decisions are unchanged.
    ///
    /// The record goes out in a single write call, and the file is opened in append mode. A short
    /// write is retried by `write_all`, which can split a record only when the kernel accepts part
    /// of it. The file is created when it is missing. Existing bytes are never rewritten or
    /// truncated.
    ///
    /// When the existing file is non-empty and does not end in a newline, the record begins with a
    /// newline in the same write, so the new event never joins a torn final line. The separator is
    /// part of what was written, so the returned range starts at it.
    ///
    /// `end` is the file position after the write. Against other appenders in this crate, the lock
    /// and, on Unix, `O_APPEND` keep that position within this call's own write. The position is
    /// computed the same way on Windows, but nothing in this crate verifies it there, and shell
    /// appenders are not serialized against it.
    ///
    /// Trailing `\r` characters on `line` are removed, so a record always ends in `\n` alone.
    ///
    /// A `line` that contains a newline is refused with [`McaoError::Usage`] before any file is
    /// opened, because it would split into two events. A symlink or other non-regular status file
    /// or sidecar is refused with [`McaoError::Uncertain`] before it is opened, as in
    /// [`StatusLog::open_decisions`]. The check does not hold across the open, so a path swapped
    /// for a symlink between the two steps is not caught.
    pub fn append(&self, line: &str) -> Result<ByteRange, McaoError> {
        if line.contains('\n') {
            return Err(McaoError::Usage(
                "a status line must not contain a newline".to_owned(),
            ));
        }
        // The record ends in `\n` only: a trailing `\r` on `line` would make it a CRLF record.
        let line = line.trim_end_matches('\r');
        refuse_non_regular(&self.path)?;
        let lock = lock_sidecar(&self.path)?;
        let mut file = OpenOptions::new()
            .create(true)
            .read(true)
            .append(true)
            .open(&self.path)?;
        let separator = if ends_torn(&mut file)? { "\n" } else { "" };
        let record = format!("{separator}{line}\n");
        file.write_all(record.as_bytes())?;
        let end = file.stream_position()?;
        let length = u64::try_from(record.len()).expect("a record length fits in u64");
        let start = end
            .checked_sub(length)
            .ok_or_else(|| io::Error::other("append position is before the record"))?;
        // Closing the sidecar releases the lock. The close is not reported, so no error can hide a
        // record that is already written.
        drop(lock);
        Ok(ByteRange { start, end })
    }

    /// Folds the whole log into the decisions it still holds open, as the shell's
    /// `status_open_decisions` does. The result is in the order the shell prints: a decision
    /// re-opened by a later line moves to the end.
    ///
    /// A missing file holds no open decisions. A symlink or any other non-regular file is
    /// [`McaoError::Uncertain`], not an empty set: the shell reads it as empty, but a decision
    /// that cannot be read must not look closed. Other read failures are [`McaoError::Io`].
    ///
    /// A resolve or held line whose `[key=` opener never reaches a `]` is refused, so a malformed
    /// line cannot close a decision. The shell closes `default` on such a line when it has a colon,
    /// so this is a deliberate difference. An opening line with the same malformed token still
    /// opens as in the shell.
    ///
    /// Bytes that are not UTF-8 are replaced with U+FFFD before matching. Verbs and keys are ASCII,
    /// so the replacement can only change a note.
    pub fn open_decisions(&self, kind: StatusKind) -> Result<Vec<OpenDecision>, McaoError> {
        refuse_non_regular(&self.path)?;
        let bytes = match fs::read(&self.path) {
            Ok(bytes) => bytes,
            Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(Vec::new()),
            Err(error) => return Err(error.into()),
        };
        let text = String::from_utf8_lossy(&bytes);
        let mut open = Vec::new();
        for line in text.split_inclusive('\n') {
            // A `\r` directly before the `\n` is the line ending of a record a Windows editor
            // wrote. A final line with no `\n` keeps its `\r`, as `TaskMeta::parse` does.
            let line = line
                .strip_suffix('\n')
                .map_or(line, |line| line.strip_suffix('\r').unwrap_or(line));
            fold_line(&mut open, line, kind);
        }
        Ok(open)
    }
}

/// Whether the file holds bytes and its last byte is not a newline, so that the next record would
/// join a torn final line. The file must be open for reading. The caller must hold the sidecar
/// lock, or another appender's in-flight write can be read as a torn line.
fn ends_torn(file: &mut File) -> io::Result<bool> {
    if file.metadata()?.len() == 0 {
        return Ok(false);
    }
    file.seek(SeekFrom::End(-1))?;
    let mut last = [0_u8];
    file.read_exact(&mut last)?;
    Ok(last[0] != b'\n')
}

/// Folds one status line into `open`, as the shell's `_fm_decision_fold_line` does.
fn fold_line(open: &mut Vec<OpenDecision>, line: &str, kind: StatusKind) {
    let unstamped = unstamp(line);
    // A transition needs a colon or a complete key token. Any other line is prose or a bare word.
    if !(unstamped.contains(':') || has_key_token(&unstamped)) {
        return;
    }
    let verb = status_verb(line);
    if unstamped.contains(':') && is_terminal_clear(&verb, kind) {
        open.clear();
        return;
    }
    let opens = OPENING_VERBS.contains(&verb.as_str());
    let closes = verb == RESOLVE_VERB || verb == HELD_VERB;
    if !(opens || closes) {
        return;
    }
    if closes && unterminated_key_token(&unstamped) {
        return;
    }
    let Some(key) = decision_key(line) else {
        return;
    };
    let note = status_note(line);
    if !transition_allowed(&key, &note) {
        return;
    }
    open.retain(|decision| decision.key != key);
    if opens {
        open.push(OpenDecision { key, verb, note });
    }
}

/// Whether a terminal line clears the open set. Only a ship or scout's `done:` or `failed:` does.
fn is_terminal_clear(verb: &str, kind: StatusKind) -> bool {
    matches!(verb, "done" | "failed") && matches!(kind, StatusKind::Ship | StatusKind::Scout)
}

/// Whether a transition may act on `key`. A reserved key moves only for a note that starts with
/// its namespace and carries a colon.
fn transition_allowed(key: &str, note: &str) -> bool {
    !key.starts_with(RESERVED_KEY_PREFIX)
        || (note.starts_with(RESERVED_KEY_PREFIX) && note.contains(':'))
}

/// Returns the line with each `[at=...]` stamp removed. A stamp is removed only while no colon
/// precedes it, so a readable time such as `[at=10:30]` cannot move the head boundary.
fn unstamp(line: &str) -> String {
    let mut rest = line;
    let mut kept = String::new();
    while let Some(opener_at) = rest.find(STAMP_OPENER) {
        let after_opener = &rest[opener_at + STAMP_OPENER.len()..];
        let Some(close_at) = after_opener.find(']') else {
            break;
        };
        let before = &rest[..opener_at];
        if before.contains(':') {
            break;
        }
        kept.push_str(before.strip_suffix(' ').unwrap_or(before));
        rest = &after_opener[close_at + 1..];
    }
    kept.push_str(rest);
    kept
}

/// Returns the verb of a status line: the text before its first colon or tag, trimmed, with any
/// unbracketed correlation token dropped from the words after the first.
fn status_verb(line: &str) -> String {
    let head = head(line);
    let before_tag = head.split_once('[').map_or(head, |(before, _)| before);
    let verb = before_tag.trim_matches(is_shell_space);
    if !verb.contains("corr=") {
        return verb.to_owned();
    }
    let mut words = verb.split(is_shell_space).filter(|word| !word.is_empty());
    let Some(first) = words.next() else {
        return String::new();
    };
    let mut out = first.to_owned();
    for word in words {
        if !is_corr_token(word) {
            out.push(' ');
            out.push_str(word);
        }
    }
    out
}

/// Returns the key of a decision line, or `None` when the line states a key that is not a valid
/// slug. A before-colon token wins over a token at the note head. A line with neither uses
/// `default`.
fn decision_key(line: &str) -> Option<String> {
    let unstamped = unstamp(line);
    let slug = token_slug(head(&unstamped)).or_else(|| note_head_slug(&unstamped));
    match slug {
        None => Some(DEFAULT_KEY.to_owned()),
        Some(slug) if slug_ok(slug) => Some(slug.to_owned()),
        Some(_) => None,
    }
}

/// Returns the note of a status line: the text after its first colon, trimmed, with a key token
/// at the note head removed when that token states the key. A line with no colon is its own note.
fn status_note(line: &str) -> String {
    let unstamped = unstamp(line);
    let Some((_, after)) = unstamped.split_once(':') else {
        return unstamped;
    };
    let note = after.trim_start_matches(is_shell_space);
    if token_slug(head(&unstamped)).is_none()
        && let Some(slug) = note_head_slug(&unstamped)
        && slug_ok(slug)
    {
        let token = format!("{KEY_OPENER}{slug}]");
        if let Some(rest) = note.strip_prefix(token.as_str()) {
            return rest.trim_start_matches(is_shell_space).to_owned();
        }
    }
    note.to_owned()
}

/// Returns the text before the first colon, or the whole line when it has no colon.
fn head(line: &str) -> &str {
    line.split_once(':').map_or(line, |(before, _)| before)
}

/// Returns the slug of the first `[key=<slug>]` token in `text`, when that token closes.
fn token_slug(text: &str) -> Option<&str> {
    let (_, rest) = text.split_once(KEY_OPENER)?;
    rest.find(']').map(|end| &rest[..end])
}

/// Returns the slug of a `[key=<slug>]` token that starts the note, after the first colon.
fn note_head_slug(line: &str) -> Option<&str> {
    let (_, after) = line.split_once(':')?;
    let note = after.trim_start_matches(is_shell_space);
    let rest = note.strip_prefix(KEY_OPENER)?;
    rest.find(']').map(|end| &rest[..end])
}

/// Whether `text` contains a `[key=` opener that a `]` closes.
fn has_key_token(text: &str) -> bool {
    token_slug(text).is_some()
}

/// Whether a key position in `unstamped` opens a `[key=` token that never closes. The key
/// position is the head before the first colon, or else the note head.
fn unterminated_key_token(unstamped: &str) -> bool {
    if let Some((_, rest)) = head(unstamped).split_once(KEY_OPENER) {
        return !rest.contains(']');
    }
    let Some((_, after)) = unstamped.split_once(':') else {
        return false;
    };
    let note = after.trim_start_matches(is_shell_space);
    note.strip_prefix(KEY_OPENER)
        .is_some_and(|rest| !rest.contains(']'))
}

/// Whether `slug` is a valid key: nonempty, and only ASCII letters, digits, `.`, `_`, and `-`.
fn slug_ok(slug: &str) -> bool {
    !slug.is_empty()
        && slug
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'.' | b'_' | b'-'))
}

/// Whether `word` is exactly the unbracketed `corr=<16 hex digits>` token that the fleet's
/// tooling writes.
fn is_corr_token(word: &str) -> bool {
    word.strip_prefix("corr=")
        .is_some_and(|hex| hex.len() == 16 && hex.bytes().all(|byte| byte.is_ascii_hexdigit()))
}

/// The whitespace bash's `[[:space:]]` class matches.
fn is_shell_space(character: char) -> bool {
    matches!(character, ' ' | '\t' | '\n' | '\u{0B}' | '\u{0C}' | '\r')
}
