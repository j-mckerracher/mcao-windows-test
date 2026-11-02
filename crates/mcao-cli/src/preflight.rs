//! `mcao preflight`: prerequisite checks before any state changes (DPES-754).
//!
//! The checks run in order and stop at the first failure. Each passing check prints one line on
//! standard output. A failure is printed on standard error with the name of its check, and the
//! process exits with the code its [`McaoError`] variant names. The checks read `PATH`, `FM_HOME`,
//! and the OneDrive variables, and they start no process.

use std::ffi::{OsStr, OsString};
use std::io::{self, Write};
use std::path::Path;
use std::process::ExitCode;

use mcao_core::McaoError;
use mcao_core::preflight::{
    OneDriveCheck, PathCheck, PathLimit, check_gh, check_git, check_home_budget,
    check_home_outside_onedrive,
};

/// Runs `mcao preflight` against the process environment and returns its exit status.
pub fn run() -> ExitCode {
    let stdout = io::stdout();
    match run_checks(|name| std::env::var_os(name), &mut stdout.lock()) {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) => {
            eprintln!("{error}");
            let code = u8::try_from(error.exit_code()).expect("mcao exit codes fit in a byte");
            ExitCode::from(code)
        }
    }
}

/// Runs the checks in order against `env`, writing one line per passing check to `out`. Returns
/// the first failure, named after its check.
pub fn run_checks(
    env: impl Fn(&str) -> Option<OsString>,
    out: &mut impl Write,
) -> Result<(), McaoError> {
    let path = env("PATH");
    let git = check_git(path.as_deref()).map_err(|error| named("Git", error))?;
    writeln!(out, "Git: ok ({})", git.display())?;
    let gh = check_gh(path.as_deref()).map_err(|error| named("Gh", error))?;
    writeln!(out, "Gh: ok ({})", gh.display())?;

    if let Some(home) = env("FM_HOME").filter(|home| !home.is_empty()) {
        check_fm_home(&home, &env, out)?;
    }
    writeln!(out, "Preflight: all checks passed")?;
    Ok(())
}

/// Checks the `FM_HOME` path length and OneDrive placement. The home is printed as given, so a
/// space, a non-ASCII character, or a `$` reaches the operator unchanged. A relative home is
/// refused, because both rules need the full path. A home under a OneDrive root is refused, while a
/// home under a sync folder named beneath the profile only prints a warning.
fn check_fm_home(
    home: &OsStr,
    env: &impl Fn(&str) -> Option<OsString>,
    out: &mut impl Write,
) -> Result<(), McaoError> {
    let Some(text) = home.to_str() else {
        return Err(named(
            "FM_HOME",
            McaoError::Preflight("the value is not valid UTF-8".to_owned()),
        ));
    };
    let path = Path::new(text);
    if !path.is_absolute() {
        return Err(named(
            "FM_HOME",
            McaoError::Preflight(format!(
                "{text} is not an absolute path; set FM_HOME to its full path"
            )),
        ));
    }

    // The path-length budget is a Windows limit. Other hosts report that it was not checked.
    if cfg!(windows) {
        let budget = check_home_budget(path, PathLimit::Classic)
            .map_err(|error| named("FM_HOME path length", error))?;
        let note = match budget {
            PathCheck::Within => "",
            PathCheck::ToolsMayFail => "; tools that do not opt in to long paths may fail",
        };
        writeln!(out, "FM_HOME path length: ok ({text}{note})")?;
    } else {
        writeln!(
            out,
            "FM_HOME path length: not checked on this host (Windows only)"
        )?;
    }

    match check_home_outside_onedrive(path, env)
        .map_err(|error| named("FM_HOME OneDrive", error))?
    {
        OneDriveCheck::Clear => writeln!(out, "FM_HOME OneDrive: ok ({text})")?,
        OneDriveCheck::Warning(message) => writeln!(out, "FM_HOME OneDrive: warning: {message}")?,
    }
    Ok(())
}

/// Puts the name of the failing check in front of a preflight message. Other failures pass through.
fn named(check: &str, error: McaoError) -> McaoError {
    match error {
        McaoError::Preflight(message) => McaoError::Preflight(format!("{check}: {message}")),
        other => other,
    }
}
