//! Identity of a task's worker process: its PID, its creation time, and the job object
//! that holds its tree.
//!
//! A PID alone is not identity, because Windows reuses PIDs. The creation time of the
//! process, read from the child's own handle when it starts, tells the original process
//! from a later one with the same PID. [`classify_process`] turns what a platform
//! observes into an [`ProcessEvidence`] verdict. It is pure, so every platform tests it.

use std::fmt;
use std::fs;
use std::path::Path;
use std::str::FromStr;

use crate::McaoError;
use crate::state::lock::lock_meta;
use crate::state::store::publish_meta_encoded_locked;
use crate::state::{PathStyle, TaskId};

/// The `TaskMeta` key that holds the worker's PID.
pub const PID_KEY: &str = "pid";
/// The `TaskMeta` key that holds the worker's creation time.
pub const PID_START_KEY: &str = "pid_start";

/// The prefix of every job object name, in the session-local namespace.
pub const JOB_NAME_PREFIX: &str = r"Local\mcao-";

/// The PID of a worker and the creation time of that process.
///
/// `start` is a Windows FILETIME: 100-nanosecond intervals since 1601-01-01 UTC.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ProcessIdentity {
    /// The process id.
    pub pid: u32,
    /// The creation time of that process.
    pub start: u64,
}

/// What a lookup of the recorded PID found now.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PidProbe {
    /// A process with this PID exists. `start` is its creation time, and `running` is
    /// false once it has exited but is still held open.
    Found {
        /// The creation time of the process now holding the PID.
        start: u64,
        /// Whether the process has not exited.
        running: bool,
    },
    /// No process has this PID.
    Absent,
    /// The lookup was refused or failed, so nothing is known.
    Unreadable,
}

/// What a lookup of the worker's job object found now.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum JobProbe {
    /// The job exists and holds this many live processes.
    Members(u32),
    /// No job with the worker's name exists.
    Absent,
    /// The job could not be opened or queried.
    Unreadable,
}

/// The verdict on whether the recorded worker is still running.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ProcessEvidence {
    /// The recorded process runs, and its job still holds it.
    Alive,
    /// The recorded process has ended, or its PID now belongs to another process.
    Dead,
    /// Neither the recorded process nor its job can be found.
    Missing,
    /// The evidence could not be read, so no verdict is safe.
    Unreadable,
    /// The evidence contradicts itself, for example a live PID that its job does not
    /// hold. Nothing is decided on it.
    Ambiguous,
}

/// Decides whether the recorded worker is still running from what was observed.
///
/// - A PID that now carries a different creation time means the recorded process is gone.
///   It is `Dead` when its job is empty or absent. A job that still holds processes makes
///   the evidence `Ambiguous`, because those descendants may still be writing to the
///   worktree. An unreadable job is `Unreadable`.
/// - An unreadable PID is `Unreadable`.
/// - A live PID is `Alive` when its job holds processes. If its job is empty or absent,
///   the process is outside its job and the evidence is `Ambiguous`. If the job cannot
///   be read, it is `Unreadable`.
/// - An absent or exited PID is `Dead` when its job is empty, `Ambiguous` when its job
///   still holds processes, `Missing` when no job exists, and `Unreadable` when the job
///   cannot be read.
pub fn classify_process(stored: ProcessIdentity, pid: PidProbe, job: JobProbe) -> ProcessEvidence {
    let pid = match pid {
        PidProbe::Found { start, .. } if start != stored.start => {
            return match job {
                JobProbe::Members(0) | JobProbe::Absent => ProcessEvidence::Dead,
                JobProbe::Members(1..) => ProcessEvidence::Ambiguous,
                JobProbe::Unreadable => ProcessEvidence::Unreadable,
            };
        }
        PidProbe::Found { running: false, .. } => PidProbe::Absent,
        other => other,
    };
    match (pid, job) {
        (PidProbe::Unreadable, _) => ProcessEvidence::Unreadable,
        (PidProbe::Found { .. }, JobProbe::Members(1..)) => ProcessEvidence::Alive,
        (PidProbe::Found { .. }, JobProbe::Unreadable) => ProcessEvidence::Unreadable,
        (PidProbe::Found { .. }, JobProbe::Members(0) | JobProbe::Absent) => {
            ProcessEvidence::Ambiguous
        }
        (PidProbe::Absent, JobProbe::Members(0)) => ProcessEvidence::Dead,
        (PidProbe::Absent, JobProbe::Members(1..)) => ProcessEvidence::Ambiguous,
        (PidProbe::Absent, JobProbe::Absent) => ProcessEvidence::Missing,
        (PidProbe::Absent, JobProbe::Unreadable) => ProcessEvidence::Unreadable,
    }
}

/// Returns the job object name for a task: `Local\mcao-<hash8>-<task_id>`.
///
/// `home_canonical` is the canonical path of the operational home. `hash8` is the first
/// eight hex digits of a 64-bit FNV-1a hash of that path, so one home's jobs never share
/// a name with another home's. The name is stable across restarts, which is what lets a
/// later process reopen the job.
pub fn job_object_name(home_canonical: &str, task_id: &str) -> String {
    format!("{JOB_NAME_PREFIX}{}-{task_id}", home_hash8(home_canonical))
}

/// Returns the first eight hex digits of the 64-bit FNV-1a hash of `home_canonical`.
pub fn home_hash8(home_canonical: &str) -> String {
    format!("{:08x}", fnv1a_64(home_canonical.as_bytes()) >> 32)
}

fn fnv1a_64(bytes: &[u8]) -> u64 {
    const OFFSET_BASIS: u64 = 0xcbf2_9ce4_8422_2325;
    const PRIME: u64 = 0x0000_0100_0000_01b3;
    bytes.iter().fold(OFFSET_BASIS, |hash, byte| {
        (hash ^ u64::from(*byte)).wrapping_mul(PRIME)
    })
}

/// A `TaskMeta` record whose identity keys cannot be read as one identity.
#[derive(Clone, Debug, PartialEq, Eq)]
#[non_exhaustive]
pub enum IdentityRecordError {
    /// The key appears more than once, so its value is unknown.
    Ambiguous {
        /// The repeated key.
        key: &'static str,
    },
    /// The key's value is not a number.
    Malformed {
        /// The key whose value is malformed.
        key: &'static str,
        /// The value as recorded.
        value: String,
    },
    /// Only one of `pid` and `pid_start` is present.
    Partial,
}

impl fmt::Display for IdentityRecordError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            IdentityRecordError::Ambiguous { key } => {
                write!(f, "the record repeats the {key} key")
            }
            IdentityRecordError::Malformed { key, value } => {
                write!(f, "the {key} value {value:?} is not a number")
            }
            IdentityRecordError::Partial => {
                write!(f, "the record holds one of pid and pid_start, not both")
            }
        }
    }
}

impl std::error::Error for IdentityRecordError {}

/// Reads one identity key as a number. An absent key is `None`.
pub(crate) fn parse_identity_value<T: FromStr>(
    key: &'static str,
    value: Option<&str>,
) -> Result<Option<T>, IdentityRecordError> {
    match value {
        None => Ok(None),
        Some(text) => text
            .parse()
            .map(Some)
            .map_err(|_| IdentityRecordError::Malformed {
                key,
                value: text.to_owned(),
            }),
    }
}

/// Returns `raw` with its `pid=` and `pid_start=` lines replaced by `identity`.
///
/// Every other line is kept, so writing identity never drops a key this code does not read.
/// A CRLF line end is rewritten as LF, so the result never mixes line ends. The result is
/// ready for [`crate::state::publish_meta_encoded`].
pub fn merge_identity_into_record(raw: &str, identity: ProcessIdentity) -> String {
    let mut merged = String::with_capacity(raw.len() + 48);
    for line in raw.split_inclusive('\n') {
        let key = line
            .strip_prefix('\u{feff}')
            .unwrap_or(line)
            .split_once('=')
            .map(|(key, _)| key);
        if matches!(key, Some(PID_KEY | PID_START_KEY)) {
            continue;
        }
        match line.strip_suffix("\r\n") {
            Some(body) => {
                merged.push_str(body);
                merged.push('\n');
            }
            None => merged.push_str(line),
        }
    }
    if !merged.is_empty() && !merged.ends_with('\n') {
        merged.push('\n');
    }
    merged.push_str(&format!(
        "{PID_KEY}={}\n{PID_START_KEY}={}\n",
        identity.pid, identity.start
    ));
    merged
}

/// Writes `identity` into the task's `TaskMeta` record without touching its other
/// keys, publishing the result through [`publish_meta_encoded`] so the record passes the
/// same checks and version stamp as every other record write.
///
/// `style` is the path syntax of the system that wrote the identity, not of this host.
///
/// The read, the merge, and the publish run under the record's sidecar lock, `<id>.meta.lock`,
/// taken before the read and released when the function returns. Every Rust writer of the record
/// takes that same lock: [`publish_meta`], [`publish_meta_encoded`], [`migrate_meta`], and
/// [`rollback_meta`]. So no other Rust writer can land between this read and this publish.
///
/// The shell's `.meta-<id>.lock` directory lock is a different lock. This function does not
/// exclude a shell writer, and the shell writers do not take the sidecar.
///
/// A missing record is refused before the sidecar is created, so a refused write leaves no lock
/// file behind.
///
/// # Errors
///
/// Returns [`McaoError::Io`] when the record does not exist, cannot be read, or the sidecar
/// cannot be taken, and [`McaoError::Uncertain`] when the sidecar is a symlink or another
/// non-regular file. Returns the errors of [`publish_meta_encoded`] when the merged record is
/// refused or cannot be published.
///
/// [`publish_meta`]: crate::state::publish_meta
/// [`publish_meta_encoded`]: crate::state::publish_meta_encoded
/// [`migrate_meta`]: crate::state::migrate_meta
/// [`rollback_meta`]: crate::state::rollback_meta
pub fn record_process_identity(
    state_dir: &Path,
    id: &TaskId,
    identity: ProcessIdentity,
    style: PathStyle,
) -> Result<(), McaoError> {
    let path = state_dir.join(format!("{}.meta", id.as_str()));
    fs::symlink_metadata(&path)?;
    let _lock = lock_meta(state_dir, id)?;
    let raw = fs::read_to_string(&path)?;
    let merged = merge_identity_into_record(&raw, identity);
    publish_meta_encoded_locked(state_dir, id, &merged, style)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::state::{migrate_meta, publish_meta, publish_meta_encoded, rollback_meta};
    use std::fs::File;
    use std::fs::OpenOptions;
    use std::io;
    use std::sync::mpsc;
    use std::thread;
    use std::time::Duration;

    const STORED: ProcessIdentity = ProcessIdentity {
        pid: 4242,
        start: 133_000_000_000_000_000,
    };

    fn found(start: u64) -> PidProbe {
        PidProbe::Found {
            start,
            running: true,
        }
    }

    #[test]
    fn a_live_pid_with_its_job_members_is_alive() {
        assert_eq!(
            classify_process(STORED, found(STORED.start), JobProbe::Members(3)),
            ProcessEvidence::Alive
        );
    }

    #[test]
    fn a_reused_pid_is_dead_only_when_its_job_is_empty_or_absent() {
        let reused = found(STORED.start + 1);
        for job in [JobProbe::Members(0), JobProbe::Absent] {
            assert_eq!(
                classify_process(STORED, reused, job),
                ProcessEvidence::Dead,
                "{job:?}"
            );
        }
        assert_eq!(
            classify_process(STORED, reused, JobProbe::Members(5)),
            ProcessEvidence::Ambiguous,
            "live job members may still write to the worktree"
        );
        assert_eq!(
            classify_process(STORED, reused, JobProbe::Unreadable),
            ProcessEvidence::Unreadable
        );
    }

    #[test]
    fn an_exited_pid_with_an_empty_job_is_dead() {
        let exited = PidProbe::Found {
            start: STORED.start,
            running: false,
        };
        assert_eq!(
            classify_process(STORED, exited, JobProbe::Members(0)),
            ProcessEvidence::Dead
        );
    }

    #[test]
    fn no_pid_and_no_job_is_missing() {
        assert_eq!(
            classify_process(STORED, PidProbe::Absent, JobProbe::Absent),
            ProcessEvidence::Missing
        );
    }

    #[test]
    fn no_pid_with_an_empty_job_is_dead() {
        assert_eq!(
            classify_process(STORED, PidProbe::Absent, JobProbe::Members(0)),
            ProcessEvidence::Dead
        );
    }

    #[test]
    fn a_job_still_holding_processes_without_the_pid_is_ambiguous() {
        assert_eq!(
            classify_process(STORED, PidProbe::Absent, JobProbe::Members(2)),
            ProcessEvidence::Ambiguous
        );
    }

    #[test]
    fn a_live_pid_outside_its_job_is_ambiguous() {
        for job in [JobProbe::Members(0), JobProbe::Absent] {
            assert_eq!(
                classify_process(STORED, found(STORED.start), job),
                ProcessEvidence::Ambiguous,
                "{job:?}"
            );
        }
    }

    #[test]
    fn unreadable_evidence_is_never_decided() {
        assert_eq!(
            classify_process(STORED, PidProbe::Unreadable, JobProbe::Members(1)),
            ProcessEvidence::Unreadable
        );
        assert_eq!(
            classify_process(STORED, found(STORED.start), JobProbe::Unreadable),
            ProcessEvidence::Unreadable
        );
        assert_eq!(
            classify_process(STORED, PidProbe::Absent, JobProbe::Unreadable),
            ProcessEvidence::Unreadable
        );
    }

    #[test]
    fn the_job_name_has_the_local_prefix_hash_and_task_id() {
        let name = job_object_name(r"\\?\C:\fm\home", "task-7");
        let rest = name
            .strip_prefix(JOB_NAME_PREFIX)
            .expect("the name starts with the local prefix");
        let (hash, task) = rest.split_once('-').expect("the hash ends at a dash");
        assert_eq!(hash.len(), 8, "{name}");
        assert!(hash.bytes().all(|b| b.is_ascii_hexdigit()), "{name}");
        assert_eq!(task, "task-7");
    }

    #[test]
    fn the_home_hash_is_stable_and_separates_homes() {
        assert_eq!(home_hash8("C:/a"), home_hash8("C:/a"));
        assert_ne!(home_hash8("C:/a"), home_hash8("C:/b"));
    }

    #[test]
    fn fnv1a_matches_its_published_vectors() {
        assert_eq!(fnv1a_64(b""), 0xcbf2_9ce4_8422_2325);
        assert_eq!(fnv1a_64(b"a"), 0xaf63_dc4c_8601_ec8c);
    }

    #[test]
    fn merging_replaces_identity_and_keeps_other_lines() {
        let raw = "backend=native\npid=1\npid_start=2\nharness=claude\n";
        let merged = merge_identity_into_record(raw, STORED);
        assert_eq!(
            merged,
            format!(
                "backend=native\nharness=claude\npid={}\npid_start={}\n",
                STORED.pid, STORED.start
            )
        );
    }

    #[test]
    fn merging_appends_identity_to_a_record_without_a_trailing_newline() {
        let merged = merge_identity_into_record("backend=native", STORED);
        assert!(merged.starts_with("backend=native\npid=4242\n"), "{merged}");
    }

    #[test]
    fn a_byte_order_mark_does_not_hide_the_first_identity_key() {
        let raw = "\u{feff}pid=1\nkind=ship\n";
        let merged = merge_identity_into_record(raw, STORED);
        assert!(!merged.contains("pid=1"), "{merged}");
        assert!(merged.contains("kind=ship\n"), "{merged}");
    }

    #[test]
    fn the_identity_keys_parse_back_as_one_identity() {
        let id = TaskId::parse("task-7").expect("a valid task id");
        let merged = merge_identity_into_record("backend=native\n", STORED);
        let meta = crate::state::TaskMeta::parse(id, &merged);
        assert_eq!(meta.process_identity().expect("readable"), Some(STORED));
    }

    #[test]
    fn a_partial_or_malformed_identity_is_refused() {
        let id = TaskId::parse("task-7").expect("a valid task id");
        let partial = crate::state::TaskMeta::parse(id.clone(), "pid=1\n");
        assert_eq!(
            partial.process_identity(),
            Err(IdentityRecordError::Partial)
        );
        let malformed = crate::state::TaskMeta::parse(id, "pid=x\npid_start=2\n");
        assert!(matches!(
            malformed.process_identity(),
            Err(IdentityRecordError::Malformed { key: "pid", .. })
        ));
    }

    /// Removes its directory on drop, so a failed assertion leaves nothing behind.
    struct ScratchDir(std::path::PathBuf);

    impl ScratchDir {
        fn new(name: &str) -> Self {
            let dir =
                std::env::temp_dir().join(format!("mcao-identity-{}-{name}", std::process::id()));
            let _ = fs::remove_dir_all(&dir);
            fs::create_dir_all(&dir).expect("scratch dir is created");
            Self(dir)
        }
    }

    impl Drop for ScratchDir {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.0);
        }
    }

    /// A local tmux record that the record writer's checks accept.
    const LOCAL_RECORD: &str =
        "harness=claude\nkind=ship\nwindow=mcao:fm-task-7\nworktree=wt-7\nproject=motif\n";

    #[test]
    fn recording_identity_publishes_a_checked_record_with_the_identity() {
        let scratch = ScratchDir::new("publish");
        let id = TaskId::parse("task-7").expect("a valid task id");
        fs::write(
            scratch.0.join("task-7.meta"),
            format!("{LOCAL_RECORD}pid=1\npid_start=2\n"),
        )
        .expect("seed is written");

        record_process_identity(&scratch.0, &id, STORED, PathStyle::Posix)
            .expect("the identity is recorded");

        let text = fs::read_to_string(scratch.0.join("task-7.meta")).expect("record is readable");
        let meta = crate::state::TaskMeta::parse(id, &text);
        assert_eq!(meta.process_identity().expect("readable"), Some(STORED));
        assert_eq!(meta.get("harness"), Some("claude"), "{text}");
        assert_eq!(meta.get("kind"), Some("ship"), "{text}");
        assert_eq!(meta.get("window"), Some("mcao:fm-task-7"), "{text}");
        assert_eq!(meta.get("format"), Some("2"), "{text}");
        assert_eq!(meta.get("path_style"), Some("posix"), "{text}");
        let pid_lines = text.lines().filter(|line| line.starts_with("pid=")).count();
        assert_eq!(pid_lines, 1, "the old pid line is replaced: {text}");
        let format_lines = text
            .lines()
            .filter(|line| line.starts_with("format="))
            .count();
        assert_eq!(format_lines, 1, "{text}");
    }

    #[test]
    fn recording_identity_leaves_no_carriage_return_behind() {
        let scratch = ScratchDir::new("crlf");
        let id = TaskId::parse("task-7").expect("a valid task id");
        let seeded = LOCAL_RECORD.replace('\n', "\r\n") + "pid=1\r\npid_start=2\r\n";
        fs::write(scratch.0.join("task-7.meta"), seeded).expect("seed is written");

        record_process_identity(&scratch.0, &id, STORED, PathStyle::Windows)
            .expect("the identity is recorded");

        let text = fs::read_to_string(scratch.0.join("task-7.meta")).expect("record is readable");
        assert!(!text.contains('\r'), "{text:?}");
        let meta = crate::state::TaskMeta::parse(id, &text);
        assert_eq!(meta.process_identity().expect("readable"), Some(STORED));
        assert_eq!(meta.get("harness"), Some("claude"), "{text}");
        assert_eq!(meta.get("path_style"), Some("windows"), "{text}");
    }

    #[test]
    fn a_cloud_record_is_refused_and_left_unchanged() {
        let scratch = ScratchDir::new("cloud");
        let id = TaskId::parse("task-7").expect("a valid task id");
        let seeded = format!("{LOCAL_RECORD}execution_target=gcp\n");
        fs::write(scratch.0.join("task-7.meta"), &seeded).expect("seed is written");

        let result = record_process_identity(&scratch.0, &id, STORED, PathStyle::Posix);

        assert!(matches!(result, Err(McaoError::Refused(_))), "{result:?}");
        assert_eq!(
            fs::read_to_string(scratch.0.join("task-7.meta")).expect("record is readable"),
            seeded,
            "a refused record is not rewritten"
        );
    }

    #[test]
    fn recording_identity_waits_for_the_record_lock_then_keeps_the_other_keys() {
        let scratch = ScratchDir::new("locked");
        let id = TaskId::parse("task-7").expect("a valid task id");
        let record = scratch.0.join("task-7.meta");
        fs::write(&record, format!("{LOCAL_RECORD}pid=1\npid_start=2\n")).expect("seed is written");
        let holder = OpenOptions::new()
            .read(true)
            .write(true)
            .create(true)
            .truncate(false)
            .open(scratch.0.join("task-7.meta.lock"))
            .expect("the sidecar lock opens");
        holder.lock().expect("the test takes the sidecar lock");

        let (done_tx, done_rx) = mpsc::channel();
        let state_dir = scratch.0.clone();
        let writer = thread::spawn(move || {
            let result = record_process_identity(&state_dir, &id, STORED, PathStyle::Posix);
            done_tx.send(result).expect("the test is still listening");
        });

        assert!(
            done_rx.recv_timeout(Duration::from_millis(300)).is_err(),
            "the identity write must wait while the sidecar lock is held"
        );
        drop(holder);
        done_rx
            .recv_timeout(Duration::from_secs(10))
            .expect("the write finishes once the lock is released")
            .expect("the identity is recorded");
        writer.join().expect("the writer thread finishes");

        let text = fs::read_to_string(&record).expect("record is readable");
        let meta = crate::state::TaskMeta::parse(TaskId::parse("task-7").expect("valid"), &text);
        assert_eq!(
            meta.process_identity().expect("readable"),
            Some(STORED),
            "{text}"
        );
        assert_eq!(meta.get("harness"), Some("claude"), "{text}");
        assert_eq!(meta.get("window"), Some("mcao:fm-task-7"), "{text}");
    }

    /// Takes the sidecar lock of the task-7 record, as another writer would, and keeps it until the
    /// returned handle is dropped.
    fn hold_sidecar(scratch: &ScratchDir) -> File {
        let holder = OpenOptions::new()
            .read(true)
            .write(true)
            .create(true)
            .truncate(false)
            .open(scratch.0.join("task-7.meta.lock"))
            .expect("the sidecar lock opens");
        holder.lock().expect("the test takes the sidecar lock");
        holder
    }

    #[test]
    fn recording_identity_for_a_missing_record_leaves_no_sidecar_behind() {
        let scratch = ScratchDir::new("missing");
        let id = TaskId::parse("task-7").expect("a valid task id");

        let result = record_process_identity(&scratch.0, &id, STORED, PathStyle::Posix);

        assert!(
            matches!(&result, Err(McaoError::Io(error)) if error.kind() == io::ErrorKind::NotFound),
            "{result:?}"
        );
        assert!(
            !scratch.0.join("task-7.meta.lock").exists(),
            "a refused write must not create the sidecar lock"
        );
    }

    #[cfg(unix)]
    #[test]
    fn a_symlinked_sidecar_lock_is_refused_and_the_record_is_unchanged() {
        let scratch = ScratchDir::new("symlinked-sidecar");
        let id = TaskId::parse("task-7").expect("a valid task id");
        let record = scratch.0.join("task-7.meta");
        fs::write(&record, LOCAL_RECORD).expect("seed is written");
        let target = scratch.0.join("elsewhere");
        fs::write(&target, "").expect("the symlink target is written");
        std::os::unix::fs::symlink(&target, scratch.0.join("task-7.meta.lock"))
            .expect("the symlink is made");

        let result = record_process_identity(&scratch.0, &id, STORED, PathStyle::Posix);

        assert!(matches!(result, Err(McaoError::Uncertain(_))), "{result:?}");
        assert_eq!(
            fs::read_to_string(&record).expect("record is readable"),
            LOCAL_RECORD,
            "a refused write does not rewrite the record"
        );
    }

    #[test]
    fn publishing_a_record_waits_for_the_sidecar_lock_the_identity_write_uses() {
        let scratch = ScratchDir::new("publish-locked");
        let id = TaskId::parse("task-7").expect("a valid task id");
        let holder = hold_sidecar(&scratch);

        let (done_tx, done_rx) = mpsc::channel();
        let state_dir = scratch.0.clone();
        let writer = thread::spawn(move || {
            let result = publish_meta_encoded(&state_dir, &id, LOCAL_RECORD, PathStyle::Posix);
            done_tx.send(result).expect("the test is still listening");
        });

        assert!(
            done_rx.recv_timeout(Duration::from_millis(300)).is_err(),
            "the publish must wait while the sidecar lock is held"
        );
        drop(holder);
        done_rx
            .recv_timeout(Duration::from_secs(10))
            .expect("the publish finishes once the lock is released")
            .expect("the record is published");
        writer.join().expect("the writer thread finishes");
    }

    #[test]
    fn publish_meta_waits_for_the_sidecar_lock_the_identity_write_uses() {
        let scratch = ScratchDir::new("publish-plain-locked");
        let id = TaskId::parse("task-7").expect("a valid task id");
        let holder = hold_sidecar(&scratch);

        let (done_tx, done_rx) = mpsc::channel();
        let state_dir = scratch.0.clone();
        let writer = thread::spawn(move || {
            let result = publish_meta(&state_dir, &id, LOCAL_RECORD);
            done_tx.send(result).expect("the test is still listening");
        });

        assert!(
            done_rx.recv_timeout(Duration::from_millis(300)).is_err(),
            "publish_meta must wait while the sidecar lock is held"
        );
        drop(holder);
        done_rx
            .recv_timeout(Duration::from_secs(10))
            .expect("the publish finishes once the lock is released")
            .expect("the record is published");
        writer.join().expect("the writer thread finishes");
    }

    #[test]
    fn rollback_meta_waits_for_the_sidecar_lock_the_identity_write_uses() {
        let scratch = ScratchDir::new("rollback-locked");
        let id = TaskId::parse("task-7").expect("a valid task id");
        fs::write(scratch.0.join("task-7.meta"), LOCAL_RECORD).expect("record is seeded");
        let holder = hold_sidecar(&scratch);

        let (done_tx, done_rx) = mpsc::channel();
        let state_dir = scratch.0.clone();
        let writer = thread::spawn(move || {
            let result = rollback_meta(&state_dir, &id);
            done_tx.send(result).expect("the test is still listening");
        });

        assert!(
            done_rx.recv_timeout(Duration::from_millis(300)).is_err(),
            "rollback_meta must wait while the sidecar lock is held"
        );
        drop(holder);
        done_rx
            .recv_timeout(Duration::from_secs(10))
            .expect("the rollback finishes once the lock is released")
            .expect("the record rolls back");
        writer.join().expect("the writer thread finishes");
    }

    #[test]
    fn a_migration_and_an_identity_write_both_wait_and_neither_loses_the_other() {
        let scratch = ScratchDir::new("serialized");
        let id = TaskId::parse("task-7").expect("a valid task id");
        fs::write(scratch.0.join("task-7.meta"), LOCAL_RECORD).expect("legacy record is seeded");
        let holder = hold_sidecar(&scratch);

        let (migrated_tx, migrated_rx) = mpsc::channel();
        let migrating_dir = scratch.0.clone();
        let migrating_id = id.clone();
        let migrator = thread::spawn(move || {
            let result = migrate_meta(&migrating_dir, &migrating_id, PathStyle::Posix);
            migrated_tx
                .send(result)
                .expect("the test is still listening");
        });
        let (recorded_tx, recorded_rx) = mpsc::channel();
        let recording_dir = scratch.0.clone();
        let recording_id = id.clone();
        let recorder = thread::spawn(move || {
            let result =
                record_process_identity(&recording_dir, &recording_id, STORED, PathStyle::Posix);
            recorded_tx
                .send(result)
                .expect("the test is still listening");
        });

        assert!(
            migrated_rx
                .recv_timeout(Duration::from_millis(300))
                .is_err(),
            "the migration must wait while the sidecar lock is held"
        );
        assert!(
            recorded_rx
                .recv_timeout(Duration::from_millis(300))
                .is_err(),
            "the identity write must wait while the sidecar lock is held"
        );
        drop(holder);
        migrated_rx
            .recv_timeout(Duration::from_secs(10))
            .expect("the migration finishes once the lock is released")
            .expect("the migration succeeds");
        recorded_rx
            .recv_timeout(Duration::from_secs(10))
            .expect("the identity write finishes once the lock is released")
            .expect("the identity is recorded");
        migrator.join().expect("the migrator thread finishes");
        recorder.join().expect("the recorder thread finishes");

        let text = fs::read_to_string(scratch.0.join("task-7.meta")).expect("record is readable");
        let meta = crate::state::TaskMeta::parse(id, &text);
        assert_eq!(
            meta.process_identity().expect("readable"),
            Some(STORED),
            "the identity survives the migration: {text}"
        );
        assert_eq!(meta.get("format"), Some("2"), "{text}");
        assert_eq!(meta.get("harness"), Some("claude"), "{text}");
    }
}
