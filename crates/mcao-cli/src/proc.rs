//! `mcao proc parent`: prints the parent PID and creation time of a process (DPES-894).
//!
//! `--expect-start` guards against PID reuse. When the process's creation time differs from the
//! expected one by more than one second, the command exits 5 rather than report a parent that may
//! belong to a different process. A query that the operating system refuses exits 2.

use std::process::ExitCode;

use clap::{Args, Subcommand};
use mcao_core::McaoError;
use mcao_core::proc_info::{
    EXIT_QUERY_FAILED, StartGuard, check_start_guard, guard_exit_code, parse_output_format,
    query_process_info, render,
};

/// Flags for `mcao proc`.
#[derive(Debug, Args)]
pub struct ProcArgs {
    #[command(subcommand)]
    pub command: ProcCommand,
}

#[derive(Debug, Subcommand)]
pub enum ProcCommand {
    /// Prints the parent PID and creation time of `--pid`.
    Parent(ParentArgs),
}

/// Flags for `mcao proc parent`.
#[derive(Debug, Args)]
pub struct ParentArgs {
    /// The process to inspect.
    #[arg(long, value_name = "PID")]
    pid: u32,
    /// The creation time the caller recorded for `--pid`, in Unix milliseconds. The command exits
    /// 5 if the process started more than one second away from it.
    #[arg(long, value_name = "MS")]
    expect_start: Option<u64>,
    /// `json` prints `{"parent_pid": P, "creation_time_ms": C}`; `text` prints the parent PID.
    #[arg(long, value_name = "FORMAT", value_parser = ["json", "text"])]
    output: String,
}

/// Runs `mcao proc`: performs the request and maps any failure to its exit code.
pub fn run(args: &ProcArgs) -> ExitCode {
    match &args.command {
        ProcCommand::Parent(parent) => parent_run(parent),
    }
}

fn parent_run(args: &ParentArgs) -> ExitCode {
    let Some(format) = parse_output_format(&args.output) else {
        // clap's value parser admits only `json` and `text`, so this branch is unreachable.
        eprintln!("error: unknown output format {}", args.output);
        return exit(EXIT_QUERY_FAILED);
    };
    let info = match query_process_info(args.pid) {
        Ok(info) => info,
        Err(error) => {
            eprintln!("error: {error}");
            return exit(EXIT_QUERY_FAILED);
        }
    };
    let guard = check_start_guard(info.creation_time_ms, args.expect_start);
    if let StartGuard::Mismatch { diff_ms } = guard {
        let error = McaoError::Uncertain(format!(
            "process {} started {diff_ms} ms away from the expected start; its PID may have been reused",
            args.pid
        ));
        eprintln!("{error}");
        return exit(guard_exit_code(guard));
    }
    println!("{}", render(&info, format));
    ExitCode::SUCCESS
}

/// Converts an exit code from `mcao_core::proc_info`, which always fits in a byte.
fn exit(code: i32) -> ExitCode {
    ExitCode::from(u8::try_from(code).expect("mcao exit codes fit in a byte"))
}
