//! `mcao util`: developer ports of the awaiting-summary parsers and the millisecond clock.
//!
//! The parsers are pure functions of stdin; the clock is the only input `now-ms` reads. The
//! parsers' output is pinned byte for byte to the reference programs by the saved fixtures in
//! `tests/fixtures/util-awaiting`.

use std::env;
use std::fs;
use std::io::{self, Read, Write};
use std::process::ExitCode;

use mcao_core::util::awaiting::{self, Run};
use mcao_core::util::clock;
use mcao_core::util::path;

mod durable;
#[cfg(unix)]
mod kimi;
#[cfg(unix)]
mod nm_state;
#[cfg(unix)]
mod nm_state_cli;
mod timing;

/// The usage line on unix also names the no-mistakes readers; Windows builds do not compile them.
#[cfg(unix)]
pub const USAGE: &str = "usage: mcao util <now-ms|awaiting-hold-ids|awaiting-hold-object|nm-run-inventory|nm-spend-record>";
#[cfg(not(unix))]
pub const USAGE: &str = "usage: mcao util <now-ms|awaiting-hold-ids|awaiting-hold-object>";
const REALPATH_USAGE: &str = "usage: mcao util realpath <path>";
const DURABLE_USAGE: &str = "usage: mcao util durable-sync <path>";

pub fn run(args: &[String]) -> ExitCode {
    match args {
        [command] if command == "now-ms" => {
            println!("{}", clock::now_ms());
            ExitCode::SUCCESS
        }
        [command] if command == "awaiting-hold-ids" => filter(awaiting::hold_ids),
        [command] if command == "awaiting-hold-object" => filter(awaiting::hold_object),
        [command, arg] if command == "realpath" => realpath(arg),
        [command, ..] if command == "realpath" => usage(REALPATH_USAGE),
        [command, arg] if command == "durable-sync" => durable::run(arg),
        [command, ..] if command == "durable-sync" => usage(DURABLE_USAGE),
        [command, rest @ ..] if command == "aggregate-timing-json" => timing::aggregate(rest),
        [command, rest @ ..] if command == "write-timing-json" => timing::write(rest),
        [command, rest @ ..] if command == "kimi-turnend" => kimi_turnend(rest),
        #[cfg(unix)]
        [command, rest @ ..] if command == "nm-run-inventory" => nm_state_cli::inventory(rest),
        #[cfg(unix)]
        [command, rest @ ..] if command == "nm-spend-record" => nm_state_cli::spend_record(rest),
        _ => usage(USAGE),
    }
}

#[cfg(unix)]
fn kimi_turnend(args: &[String]) -> ExitCode {
    kimi::run(args)
}

#[cfg(not(unix))]
fn kimi_turnend(_args: &[String]) -> ExitCode {
    eprintln!(
        "fm-kimi-turnend-hook: refused: the Kimi turn-end hook is not implemented on this platform."
    );
    ExitCode::from(1)
}

fn usage(text: &str) -> ExitCode {
    eprintln!("{text}");
    ExitCode::from(2)
}

/// Prints `os.path.realpath(arg)` relative to the working directory.
///
/// Windows is refused. The walk is POSIX-only and the Windows drive and UNC semantics are not
/// verified in this slice, so printing a path there would claim parity that is not tested.
fn realpath(arg: &str) -> ExitCode {
    if cfg!(windows) {
        eprintln!("mcao util: realpath is not implemented for Windows paths yet");
        return ExitCode::from(1);
    }
    match path::realpath(os_cwd, arg, os_entry) {
        Ok(resolved) => {
            println!("{resolved}");
            ExitCode::SUCCESS
        }
        Err(message) => {
            eprintln!("mcao util: {message}");
            ExitCode::from(1)
        }
    }
}

/// The working directory as UTF-8 text. `realpath` calls this only for a relative path, so an
/// absolute path never touches the working directory.
fn os_cwd() -> Result<String, String> {
    match env::current_dir() {
        Ok(dir) => dir
            .into_os_string()
            .into_string()
            .map_err(|_| "the working directory is not valid UTF-8".to_owned()),
        Err(err) => Err(format!("cannot read the working directory: {err}")),
    }
}

/// The filesystem answer `realpath` needs for one path. An lstat or readlink failure is plain,
/// which is how CPython's non-strict walk treats it. A link target that is not UTF-8 cannot be
/// walked as text, so it is an error.
fn os_entry(location: &str) -> Result<path::Entry, String> {
    let is_link = fs::symlink_metadata(location).is_ok_and(|meta| meta.file_type().is_symlink());
    if !is_link {
        return Ok(path::Entry::Plain);
    }
    match fs::read_link(location) {
        Ok(target) => match target.into_os_string().into_string() {
            Ok(text) => Ok(path::Entry::Link(text)),
            Err(_) => Err(format!(
                "symbolic link {location} has a target that is not valid UTF-8"
            )),
        },
        Err(_) => Ok(path::Entry::Plain),
    }
}

/// Reads all of stdin, runs one parser, and replays its stdout, stderr and exit status.
fn filter(parse: fn(&[u8]) -> Run) -> ExitCode {
    let mut input = Vec::new();
    if let Err(err) = io::stdin().lock().read_to_end(&mut input) {
        eprintln!("mcao util: cannot read stdin: {err}");
        return ExitCode::from(1);
    }
    let run = parse(&input);
    let mut stdout = io::stdout().lock();
    if stdout
        .write_all(&run.stdout)
        .and_then(|()| stdout.flush())
        .is_err()
    {
        return ExitCode::from(1);
    }
    eprint!("{}", run.stderr);
    ExitCode::from(run.exit)
}
