//! `mcao-cli`: PROPOSED crate name, not yet confirmed by DPES-752.
//!
//! Skeleton plus the `util` developer subcommands. clap parses the top-level command line, and a
//! malformed one exits with the code its [`McaoError`] variant names. `util` is dispatched before
//! clap runs, so its arguments reach it exactly as typed.

mod cli;
mod herdr;
mod mem_guard;
mod notify;
mod toolchain;
mod toolchain_host;
mod util;

use std::ffi::OsString;
use std::process::ExitCode;

use mcao_core::McaoError;

use cli::{Cli, Command};

fn main() -> ExitCode {
    let argv: Vec<OsString> = std::env::args_os().collect();
    // clap would strip a leading `--` from the arguments of `util`, so `util` takes the raw rest.
    if argv.get(1).is_some_and(|arg| arg == "util") {
        return run_util(&argv[2..]);
    }
    let cli = match Cli::parse_args(argv) {
        Ok(cli) => cli,
        Err(err) if err.use_stderr() => return usage_failure(&err),
        // `--help` and `--version` print to stdout and exit 0.
        Err(err) => err.exit(),
    };
    match cli.command {
        // Bare invocation keeps the output it had before clap.
        None => {
            println!(
                "mcao {} (PROPOSED skeleton, no subcommands yet)",
                env!("CARGO_PKG_VERSION")
            );
            ExitCode::SUCCESS
        }
        Some(Command::MemGuard(args)) => mem_guard::run(&args),
        Some(Command::Herdr(args)) => herdr::run(&args),
        Some(Command::Toolchain(args)) => toolchain::run(&args),
        Some(Command::Notify(args)) => notify::run(&args),
    }
}

/// Runs `mcao util` with its arguments as typed. A non-UTF-8 argument is a usage error.
fn run_util(raw: &[OsString]) -> ExitCode {
    let args: Result<Vec<String>, OsString> =
        raw.iter().cloned().map(OsString::into_string).collect();
    match args {
        Ok(args) => util::run(&args),
        Err(_) => report_usage("invalid UTF-8 was detected in one or more arguments"),
    }
}

/// Reports a malformed command line as [`McaoError::Usage`] and returns its exit code.
fn usage_failure(err: &clap::Error) -> ExitCode {
    let rendered = err.to_string();
    let first_line = rendered.lines().next().unwrap_or_default();
    let message = first_line.strip_prefix("error: ").unwrap_or(first_line);
    report_usage(message)
}

/// Prints `message` as a usage error and returns the exit code of [`McaoError::Usage`].
fn report_usage(message: &str) -> ExitCode {
    let error = McaoError::Usage(format!("{message} (see mcao --help)"));
    eprintln!("{error}");
    let code = u8::try_from(error.exit_code()).expect("mcao exit codes fit in a byte");
    ExitCode::from(code)
}
