//! `mcao audit append`, `verify`, and `project-ledger`: the CLI over `mcao_core::audit_writer`
//! (DPES-753, DPES-884 AC4). It ports `bin/fm-audit-writer.py`, and `docs/audit-writer.md` owns the
//! store contract. Result lines, exit statuses, and the stdout bytes follow the reference. Usage and
//! input errors print a stderr line that names this command, which differs from the reference's
//! wording (the `audit_writer_cli` tests name the cases).
//!
//! The store lock is `flock`, which only unix targets provide. Off unix every writer command refuses
//! to run, so an append never runs without the lock.

use std::io;
use std::path::PathBuf;
use std::process::ExitCode;

use clap::Args;
use mcao_core::McaoError;
use mcao_core::audit_writer::{
    AuditStore, Committed, Recovery, SEGMENT_RECORDS, StoreRefusal, VerifyReport, WriterError,
};
use serde_json::{Map, Value, json};

use super::{input, input_failure, py_json, record_rejection, tzif};

/// Flags for `mcao audit append`.
#[derive(Debug, Args)]
pub struct AppendArgs {
    /// The store directory, created when it does not exist.
    #[arg(long)]
    store: PathBuf,
    /// Records per segment before rotation, at least 1.
    #[arg(long, default_value_t = SEGMENT_RECORDS)]
    segment_records: u64,
    /// The envelope to append, without `seq` or `prev_hash`, or `-` for stdin.
    input: String,
}

/// Flags for `mcao audit verify`.
#[derive(Debug, Args)]
pub struct VerifyArgs {
    /// The store directory to verify. It must exist.
    #[arg(long)]
    store: PathBuf,
}

/// Flags for `mcao audit project-ledger`.
#[derive(Debug, Args)]
pub struct ProjectLedgerArgs {
    /// The store directory to project. It must exist.
    #[arg(long)]
    store: PathBuf,
}

/// Runs `mcao audit append`: prints `committed` (exit 0), `rejected` after recording it (exit 1), or
/// a `refused` or `failed` line (exit 3). A usage or input error, and a rejection that cannot be
/// recorded, exit 2.
pub fn run_append(args: &AppendArgs) -> ExitCode {
    if let Some(error) = unlocked_platform_refusal(cfg!(unix)) {
        return report_refusal(&error);
    }
    if args.segment_records == 0 {
        return input_failure("append", "--segment-records must be at least 1");
    }
    let document = match input::load(&args.input) {
        Ok(document) => document,
        Err(err) => return input_failure("append", &err.to_string()),
    };
    let store =
        AuditStore::with_segment_records(&args.store, args.segment_records, &tzif::utc_offset_secs);
    match store.append(document) {
        Ok(committed) => {
            println!("{}", py_json(&committed_json(&committed)));
            ExitCode::SUCCESS
        }
        Err(WriterError::Rejected(rejection)) => {
            if let Err(reason) = record_rejection("envelope", &rejection) {
                return input_failure("append", &format!("rejection not recorded: {reason}"));
            }
            let result = json!({
                "definition": "envelope",
                "paths": rejection.paths,
                "reason": rejection.reason.as_str(),
                "result": "rejected",
            });
            println!("{}", py_json(&result));
            ExitCode::from(1)
        }
        Err(WriterError::Refused(refusal)) => {
            println!("{}", py_json(&refusal_json("refused", &refusal)));
            ExitCode::from(3)
        }
        Err(WriterError::WriteFailed { may_be_durable, .. }) => {
            let result = json!({"may_be_durable": may_be_durable, "result": "failed"});
            println!("{}", py_json(&result));
            ExitCode::from(3)
        }
        // The reference reports an OSError raised during an append as a failed result, and the
        // record is not on disk because the append never reached the commit point.
        Err(WriterError::Io(error)) => {
            let result = json!({
                "error": python_error_name(&error),
                "may_be_durable": false,
                "result": "failed",
            });
            println!("{}", py_json(&result));
            ExitCode::from(3)
        }
        Err(error) => store_input_failure("append", &error),
    }
}

/// Runs `mcao audit verify`: prints `verified` (exit 0), or `corrupt` with the first damage found
/// (exit 1). A missing or unreadable store exits 2.
pub fn run_verify(args: &VerifyArgs) -> ExitCode {
    if let Some(error) = unlocked_platform_refusal(cfg!(unix)) {
        return report_refusal(&error);
    }
    let store = AuditStore::new(&args.store, &tzif::utc_offset_secs);
    match store.verify() {
        Ok(report) => {
            println!("{}", py_json(&verify_json(&report)));
            ExitCode::SUCCESS
        }
        Err(WriterError::Refused(refusal)) => {
            println!("{}", py_json(&refusal_json("corrupt", &refusal)));
            ExitCode::from(1)
        }
        Err(error) => store_input_failure("verify", &error),
    }
}

/// Runs `mcao audit project-ledger`: prints one fleet-ledger v1 line per committed projected record
/// (exit 0), or a `refused` line when the store cannot be trusted (exit 1). A missing or unreadable
/// store exits 2.
pub fn run_project_ledger(args: &ProjectLedgerArgs) -> ExitCode {
    if let Some(error) = unlocked_platform_refusal(cfg!(unix)) {
        return report_refusal(&error);
    }
    let store = AuditStore::new(&args.store, &tzif::utc_offset_secs);
    match store.ledger_v1_lines() {
        Ok(lines) => {
            for line in lines {
                println!("{line}");
            }
            ExitCode::SUCCESS
        }
        Err(WriterError::Refused(refusal)) => {
            println!("{}", py_json(&refusal_json("refused", &refusal)));
            ExitCode::from(1)
        }
        Err(error) => store_input_failure("project-ledger", &error),
    }
}

/// The store lock is `flock`, which only unix targets provide. Without it two appends could assign
/// the same sequence number, so the writer commands refuse to run on any other target.
fn unlocked_platform_refusal(has_flock: bool) -> Option<McaoError> {
    (!has_flock).then(|| McaoError::Unsupported {
        backend: std::env::consts::OS.to_owned(),
        op: "the audit store writer, whose lock needs flock",
    })
}

/// Prints a refusal to run on this target and returns its exit status.
fn report_refusal(error: &McaoError) -> ExitCode {
    eprintln!("mcao audit: {error}");
    ExitCode::from(u8::try_from(error.exit_code()).expect("mcao exit codes fit in a byte"))
}

/// Reports a store error that the reference raises outside its result lines: a missing store, an
/// unreadable store, or a record that cannot be canonicalized. Exit status 2, as the reference.
fn store_input_failure(command: &str, error: &WriterError) -> ExitCode {
    let message = match error {
        WriterError::MissingStore => "store directory does not exist".to_owned(),
        WriterError::Io(error) => format!("cannot read store: {}", python_error_name(error)),
        _ => "cannot canonicalize the record".to_owned(),
    };
    input_failure(command, &message)
}

/// The `committed` result line. It carries the `recovered` member only when the append repaired the
/// store first.
fn committed_json(committed: &Committed) -> Value {
    let mut result = json!({
        "event_id": committed.event_id,
        "result": "committed",
        "seq": committed.seq,
    });
    if !committed.recovered.is_empty() {
        result["recovered"] = recovered_json(&committed.recovered);
    }
    result
}

/// The `recovered` member: each repair that ran, and only those.
fn recovered_json(recovered: &Recovery) -> Value {
    let mut members = Map::new();
    if let Some(torn) = &recovered.torn_tail {
        members.insert(
            "torn_tail".to_owned(),
            json!({"segment": torn.segment, "bytes": torn.bytes}),
        );
    }
    if let Some(adopted) = &recovered.adopted {
        members.insert(
            "adopted".to_owned(),
            json!({"segment": adopted.segment, "seq": adopted.seq}),
        );
    }
    Value::Object(members)
}

/// A refusal result line, with the reason, sequence, segment, paths, and intact segments.
fn refusal_json(result: &str, refusal: &StoreRefusal) -> Value {
    json!({
        "result": result,
        "reason": refusal.reason,
        "seq": refusal.seq,
        "segment": refusal.segment,
        "paths": refusal.paths,
        "intact_segments": refusal.intact_segments,
    })
}

/// The `verified` result line.
fn verify_json(report: &VerifyReport) -> Value {
    let segments: Vec<Value> = report
        .segments
        .iter()
        .map(|segment| {
            json!({"name": segment.name, "records": segment.records, "status": segment.status})
        })
        .collect();
    json!({
        "result": "verified",
        "records": report.records,
        "committed_seq": report.committed_seq,
        "last_seq": report.last_seq,
        "segments": segments,
        "torn_tail": report.torn_tail.as_ref().map(|torn| json!({"segment": torn.segment, "bytes": torn.bytes})),
        "uncommitted_seq": report.uncommitted_seq,
    })
}

/// The name of the Python `OSError` subclass the reference reports for `error`, chosen by errno.
/// An error without an errno, or one with an errno Python does not subclass, is `OSError`.
#[cfg(unix)]
fn python_error_name(error: &io::Error) -> &'static str {
    match error.raw_os_error() {
        Some(libc::EAGAIN | libc::EALREADY | libc::EINPROGRESS) => "BlockingIOError",
        Some(libc::ECHILD) => "ChildProcessError",
        Some(libc::EPIPE | libc::ESHUTDOWN) => "BrokenPipeError",
        Some(libc::ECONNABORTED) => "ConnectionAbortedError",
        Some(libc::ECONNREFUSED) => "ConnectionRefusedError",
        Some(libc::ECONNRESET) => "ConnectionResetError",
        Some(libc::EEXIST) => "FileExistsError",
        Some(libc::ENOENT) => "FileNotFoundError",
        Some(libc::EISDIR) => "IsADirectoryError",
        Some(libc::ENOTDIR) => "NotADirectoryError",
        Some(libc::EINTR) => "InterruptedError",
        Some(libc::EACCES | libc::EPERM) => "PermissionError",
        Some(libc::ESRCH) => "ProcessLookupError",
        Some(libc::ETIMEDOUT) => "TimeoutError",
        _ => "OSError",
    }
}

/// Off unix the writer commands refuse before any store is touched, so this name is never printed.
#[cfg(not(unix))]
fn python_error_name(_error: &io::Error) -> &'static str {
    "OSError"
}

#[cfg(test)]
mod tests {
    use super::unlocked_platform_refusal;

    #[test]
    fn a_target_without_flock_refuses_the_writer_commands() {
        assert!(unlocked_platform_refusal(true).is_none());
        let refusal = unlocked_platform_refusal(false).expect("refused without flock");
        assert_eq!(refusal.exit_code(), 7);
    }

    #[cfg(unix)]
    #[test]
    fn errno_names_follow_the_python_oserror_subclasses() {
        use super::python_error_name;
        use std::io;
        assert_eq!(
            python_error_name(&io::Error::from_raw_os_error(libc::ENOENT)),
            "FileNotFoundError"
        );
        assert_eq!(
            python_error_name(&io::Error::from_raw_os_error(libc::EISDIR)),
            "IsADirectoryError"
        );
        assert_eq!(
            python_error_name(&io::Error::from_raw_os_error(libc::ENOSPC)),
            "OSError"
        );
    }
}
