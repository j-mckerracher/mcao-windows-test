//! `mcao notify`: opens a URI or path and posts the daemon wedge alarm through the platform
//! channel. Only Unix has a channel so far. Windows reports `Unsupported` until DPES-752 lands.

use std::path::PathBuf;
use std::process::ExitCode;

use clap::{Args, Subcommand};
use mcao_core::McaoError;

/// Flags for `mcao notify`.
#[derive(Debug, Args)]
pub struct NotifyArgs {
    #[command(subcommand)]
    pub command: NotifyCommand,
}

#[derive(Debug, Subcommand)]
pub enum NotifyCommand {
    /// Opens an http, https, or file URI, or a local path, with the default handler.
    Open(OpenArgs),
    /// Posts the daemon wedge alarm carrying SUMMARY.
    WedgeAlarm(WedgeAlarmArgs),
}

/// The target of `mcao notify open`: exactly one of `--url` or `--path`.
#[derive(Debug, Args)]
#[group(required = true, multiple = false)]
pub struct OpenArgs {
    /// An http, https, or file URI to open. Refused if it has a shell metacharacter.
    #[arg(long, value_name = "URI")]
    url: Option<String>,
    /// A local file or directory to open through its file URI.
    #[arg(long, value_name = "PATH")]
    path: Option<PathBuf>,
}

/// Summary text for `mcao notify wedge-alarm`.
#[derive(Debug, Args)]
pub struct WedgeAlarmArgs {
    /// The text shown in the alarm. It may start with a dash.
    #[arg(allow_hyphen_values = true)]
    summary: String,
}

/// Runs `mcao notify`: performs the request and maps any failure to its exit code.
pub fn run(args: &NotifyArgs) -> ExitCode {
    let result = match &args.command {
        NotifyCommand::Open(open) => open_target(open),
        NotifyCommand::WedgeAlarm(alarm) => wedge_alarm(&alarm.summary),
    };
    match result {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) => {
            eprintln!("{error}");
            let code = u8::try_from(error.exit_code()).expect("mcao exit codes fit in a byte");
            ExitCode::from(code)
        }
    }
}

#[cfg(unix)]
fn open_target(args: &OpenArgs) -> Result<(), McaoError> {
    use mcao_core::notify::opener::Opener;
    use mcao_core::notify::unix::UnixOpener;

    match (&args.url, &args.path) {
        (Some(url), None) => UnixOpener.open_url(url),
        (None, Some(path)) => UnixOpener.open_path(path),
        _ => Err(McaoError::Usage(
            "pass exactly one of --url or --path".to_owned(),
        )),
    }
}

#[cfg(unix)]
fn wedge_alarm(summary: &str) -> Result<(), McaoError> {
    use mcao_core::notify::daemon_visibility::DaemonVisibility;
    use mcao_core::notify::unix::UnixDaemonVisibility;

    UnixDaemonVisibility.wedge_alarm(summary)
}

#[cfg(not(unix))]
fn open_target(_args: &OpenArgs) -> Result<(), McaoError> {
    Err(unsupported("notify open"))
}

#[cfg(not(unix))]
fn wedge_alarm(_summary: &str) -> Result<(), McaoError> {
    Err(unsupported("notify wedge-alarm"))
}

#[cfg(not(unix))]
fn unsupported(op: &'static str) -> McaoError {
    McaoError::Unsupported {
        backend: std::env::consts::OS.to_owned(),
        op,
    }
}
