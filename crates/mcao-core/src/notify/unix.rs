//! Unix channels. The opener runs `open` on macOS and `xdg-open` elsewhere. The wedge alarm runs
//! `osascript` on macOS and `notify-send` elsewhere.
//!
//! Each handler runs with argv and no shell, so the summary and URI text are never parsed as
//! shell syntax. Its stdio is discarded, as the legacy script discarded it, so no pipe can be held
//! open by a child the handler leaves behind. A handler that runs past its deadline is killed. A
//! missing handler is a preflight failure. A handler that exits non-zero, or misses its deadline,
//! is a backend failure.

use std::io::ErrorKind;
use std::path::Path;
use std::process::{Command, Stdio};
use std::thread;
use std::time::{Duration, Instant};

use crate::McaoError;
use crate::notify::daemon_visibility::DaemonVisibility;
use crate::notify::opener::Opener;

#[cfg(target_os = "macos")]
const OPEN_PROGRAM: &str = "open";
#[cfg(not(target_os = "macos"))]
const OPEN_PROGRAM: &str = "xdg-open";

/// Title of the wedge alarm. The banner keeps the title it had before the port.
const WEDGE_TITLE: &str = "firstmate: away-mode escalations WEDGED";

/// Seconds a handler may run when `FM_WEDGE_ALARM_TIMEOUT_SECS` is unset or outside
/// `1..=MAX_TIMEOUT_SECS`.
const DEFAULT_TIMEOUT_SECS: u64 = 10;

/// Largest handler deadline, in seconds, that `FM_WEDGE_ALARM_TIMEOUT_SECS` may set.
const MAX_TIMEOUT_SECS: u64 = 3600;

/// Environment variable that overrides the handler deadline, as the legacy wedge alarm did.
const TIMEOUT_VARIABLE: &str = "FM_WEDGE_ALARM_TIMEOUT_SECS";

/// How often a running handler is checked against its deadline.
const POLL_INTERVAL: Duration = Duration::from_millis(20);

/// A handler program and its argv, run without a shell and bounded by a deadline.
#[derive(Debug, Clone, PartialEq, Eq)]
struct Invocation {
    program: &'static str,
    args: Vec<String>,
    timeout: Duration,
}

impl Invocation {
    fn new(program: &'static str, args: &[&str]) -> Self {
        Self {
            program,
            args: args.iter().map(|arg| (*arg).to_owned()).collect(),
            timeout: handler_timeout(),
        }
    }

    /// Runs the handler and waits for it. A handler still running at the deadline is killed.
    fn run(&self) -> Result<(), McaoError> {
        // Computed before the spawn so a deadline that cannot be represented starts no handler.
        let Some(deadline) = Instant::now().checked_add(self.timeout) else {
            return Err(McaoError::Backend(format!(
                "{} deadline of {:?} overflows the clock; no handler was started",
                self.program, self.timeout
            )));
        };
        let mut child = match Command::new(self.program)
            .args(&self.args)
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
        {
            Ok(child) => child,
            Err(error) if error.kind() == ErrorKind::NotFound => {
                return Err(McaoError::Preflight(format!(
                    "{} was not found on PATH",
                    self.program
                )));
            }
            Err(error) => return Err(McaoError::Io(error)),
        };
        loop {
            match child.try_wait().map_err(McaoError::Io)? {
                Some(status) if status.success() => return Ok(()),
                Some(status) => {
                    return Err(McaoError::Backend(format!(
                        "{} exited with {status}",
                        self.program
                    )));
                }
                None if Instant::now() < deadline => thread::sleep(POLL_INTERVAL),
                None => {
                    // The kill is reaped so the killed handler does not linger as a zombie.
                    let _ = child.kill();
                    let _ = child.wait();
                    return Err(McaoError::Backend(format!(
                        "{} did not exit within {:?} and was killed",
                        self.program, self.timeout
                    )));
                }
            }
        }
    }
}

/// The handler deadline: `FM_WEDGE_ALARM_TIMEOUT_SECS` when it holds an integer in
/// `1..=MAX_TIMEOUT_SECS`, otherwise the default. Zero, non-numeric, and out-of-range values all
/// use the default.
fn handler_timeout() -> Duration {
    timeout_from_setting(std::env::var(TIMEOUT_VARIABLE).ok().as_deref())
}

/// Parses a raw `FM_WEDGE_ALARM_TIMEOUT_SECS` value. Parsing fails for negative, non-numeric, and
/// too-large values, and the range check rejects zero and anything above the maximum, so each of
/// those falls back to the default.
fn timeout_from_setting(setting: Option<&str>) -> Duration {
    let seconds = setting
        .and_then(|value| value.parse::<u64>().ok())
        .filter(|seconds| (1..=MAX_TIMEOUT_SECS).contains(seconds))
        .unwrap_or(DEFAULT_TIMEOUT_SECS);
    Duration::from_secs(seconds)
}

/// Opens URIs and paths through the platform's default handler.
#[derive(Debug, Clone, Copy)]
pub struct UnixOpener;

impl Opener for UnixOpener {
    fn open_validated(&self, uri: &str) -> Result<(), McaoError> {
        open_invocation(uri).run()
    }

    fn path_uri(&self, path: &Path) -> Result<String, McaoError> {
        file_uri(path)
    }
}

/// Posts the wedge alarm through macOS Notification Center or a Linux notification daemon.
#[derive(Debug, Clone, Copy)]
pub struct UnixDaemonVisibility;

impl DaemonVisibility for UnixDaemonVisibility {
    fn wedge_alarm(&self, summary: &str) -> Result<(), McaoError> {
        wedge_alarm_invocation(summary).run()
    }
}

fn open_invocation(uri: &str) -> Invocation {
    Invocation::new(OPEN_PROGRAM, &[uri])
}

/// Builds the `file://` URI for a local path. Relative paths are made absolute first. Every byte
/// outside the URI unreserved set and `/` is percent-encoded, so the path cannot put a refused
/// character into the handler's argument. Only UTF-8 paths are accepted. The result uses POSIX
/// path syntax, which is why this lives in the Unix module.
fn file_uri(path: &Path) -> Result<String, McaoError> {
    let absolute = std::path::absolute(path).map_err(McaoError::Io)?;
    let text = absolute
        .to_str()
        .ok_or_else(|| McaoError::Usage(format!("path {path:?} is not valid UTF-8")))?;
    let mut uri = String::from("file://");
    for byte in text.bytes() {
        if byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'.' | b'_' | b'~' | b'/') {
            uri.push(char::from(byte));
        } else {
            uri.push_str(&format!("%{byte:02X}"));
        }
    }
    Ok(uri)
}

/// The macOS form posts the summary through argv, so AppleScript source never contains it. The
/// `--` ends osascript's own options, so a summary that starts with `-` reaches the script as data.
#[cfg(target_os = "macos")]
fn wedge_alarm_invocation(summary: &str) -> Invocation {
    let display = format!(
        "display notification (item 1 of argv) with title \"{WEDGE_TITLE}\" sound name \"Basso\""
    );
    Invocation::new(
        "osascript",
        &[
            "-e",
            "on run argv",
            "-e",
            display.as_str(),
            "-e",
            "end run",
            "--",
            summary,
        ],
    )
}

/// The Linux form passes the summary as the body after a `--` terminator.
#[cfg(not(target_os = "macos"))]
fn wedge_alarm_invocation(summary: &str) -> Invocation {
    Invocation::new(
        "notify-send",
        &["--urgency=critical", "--", WEDGE_TITLE, summary],
    )
}

#[cfg(test)]
mod tests {
    use std::ffi::OsStr;
    use std::os::unix::ffi::OsStrExt;
    use std::path::Path;
    use std::time::{Duration, Instant};

    use super::{
        DEFAULT_TIMEOUT_SECS, Invocation, UnixOpener, file_uri, open_invocation,
        timeout_from_setting, wedge_alarm_invocation,
    };
    use crate::McaoError;
    use crate::notify::opener::{Opener, validate_uri};

    #[test]
    fn open_invocation_passes_the_uri_as_one_argument() {
        let uri = "https://example.test/a?b=c";
        assert_eq!(open_invocation(uri).args, [uri]);
    }

    #[test]
    fn refused_uri_runs_no_handler() {
        let refused = UnixOpener.open_url("javascript:alert(1)");
        assert!(matches!(refused, Err(McaoError::Refused(_))));
    }

    #[test]
    fn open_path_refuses_a_non_utf8_path_before_running_anything() {
        // `file_uri` rejects the path before any handler starts.
        let path = Path::new(OsStr::from_bytes(b"/tmp/\xff"));
        assert!(matches!(
            UnixOpener.open_path(path),
            Err(McaoError::Usage(_))
        ));
    }

    #[test]
    fn file_uri_encodes_spaces_and_refused_characters() {
        let uri = file_uri(Path::new("/tmp/a b(c)$.md")).expect("absolute path converts");
        assert_eq!(uri, "file:///tmp/a%20b%28c%29%24.md");
        assert!(
            validate_uri(&uri).is_ok(),
            "encoded file URI must pass validation"
        );
    }

    #[test]
    fn file_uri_keeps_unreserved_characters() {
        let uri = file_uri(Path::new("/var/folders/x_y-z.0~/state")).expect("path converts");
        assert_eq!(uri, "file:///var/folders/x_y-z.0~/state");
    }

    #[test]
    fn missing_handler_is_a_preflight_failure() {
        let invocation = Invocation::new("mcao-no-such-handler-for-tests", &[]);
        assert!(matches!(invocation.run(), Err(McaoError::Preflight(_))));
    }

    #[test]
    fn failing_handler_is_a_backend_failure() {
        let invocation = Invocation::new("false", &[]);
        assert!(matches!(invocation.run(), Err(McaoError::Backend(_))));
    }

    #[test]
    fn handler_past_its_deadline_is_killed_and_reported() {
        let invocation = Invocation {
            program: "sleep",
            args: vec!["30".to_owned()],
            timeout: Duration::from_millis(100),
        };
        let started = Instant::now();
        assert!(matches!(invocation.run(), Err(McaoError::Backend(_))));
        assert!(
            started.elapsed() < Duration::from_secs(10),
            "the handler was not killed at its deadline"
        );
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn wedge_alarm_matches_the_osascript_pattern_from_the_daemon() {
        let invocation = wedge_alarm_invocation("2 sessions stalled");
        assert_eq!(invocation.program, "osascript");
        assert_eq!(
            invocation.args,
            [
                "-e",
                "on run argv",
                "-e",
                "display notification (item 1 of argv) with title \"firstmate: away-mode escalations WEDGED\" sound name \"Basso\"",
                "-e",
                "end run",
                "--",
                "2 sessions stalled",
            ]
        );
    }

    #[cfg(not(target_os = "macos"))]
    #[test]
    fn wedge_alarm_sends_a_critical_notify_send() {
        let invocation = wedge_alarm_invocation("2 sessions stalled");
        assert_eq!(invocation.program, "notify-send");
        assert_eq!(
            invocation.args,
            [
                "--urgency=critical",
                "--",
                "firstmate: away-mode escalations WEDGED",
                "2 sessions stalled",
            ]
        );
    }

    #[test]
    fn wedge_alarm_passes_shell_syntax_in_the_summary_as_one_argument() {
        // The handler runs without a shell, so the summary stays data in its final argv slot.
        let summary = "$(touch /tmp/x); `id`";
        let invocation = wedge_alarm_invocation(summary);
        assert_eq!(invocation.args.last().map(String::as_str), Some(summary));
    }

    #[test]
    fn wedge_alarm_ends_options_before_a_dash_prefixed_summary() {
        let summary = "-ereturn \"injected\"";
        let args = wedge_alarm_invocation(summary).args;
        let end_of_options = args
            .iter()
            .position(|arg| arg == "--")
            .expect("the argv carries a `--` terminator");
        let summary_index = args
            .iter()
            .position(|arg| arg == summary)
            .expect("the summary is one argv item");
        assert!(end_of_options < summary_index);
    }

    #[test]
    fn timeout_setting_uses_only_one_through_the_maximum() {
        let default = Duration::from_secs(DEFAULT_TIMEOUT_SECS);
        assert_eq!(timeout_from_setting(None), default, "unset falls back");
        assert_eq!(
            timeout_from_setting(Some("1")),
            Duration::from_secs(1),
            "the minimum is used"
        );
        assert_eq!(
            timeout_from_setting(Some("5")),
            Duration::from_secs(5),
            "a valid value is used"
        );
        assert_eq!(
            timeout_from_setting(Some("3600")),
            Duration::from_secs(3600),
            "the maximum is used"
        );
    }

    #[test]
    fn out_of_range_timeout_settings_fall_back_without_panicking() {
        let default = Duration::from_secs(DEFAULT_TIMEOUT_SECS);
        for setting in [
            "0",
            "3601",
            "9223372036854775808",
            "18446744073709551615",
            "18446744073709551616",
            "99999999999999999999999999",
        ] {
            assert_eq!(
                timeout_from_setting(Some(setting)),
                default,
                "{setting} falls back"
            );
        }
    }

    #[test]
    fn negative_and_garbage_timeout_settings_fall_back() {
        let default = Duration::from_secs(DEFAULT_TIMEOUT_SECS);
        for setting in [
            "-1",
            "-18446744073709551615",
            "",
            " 5",
            "5s",
            "1.5",
            "0x10",
            "abc",
        ] {
            assert_eq!(
                timeout_from_setting(Some(setting)),
                default,
                "{setting:?} falls back"
            );
        }
    }

    #[test]
    fn unrepresentable_deadline_is_a_backend_failure_and_starts_no_handler() {
        // A missing program would fail as a preflight error if the spawn were attempted first.
        let invocation = Invocation {
            program: "mcao-no-such-handler-for-tests",
            args: Vec::new(),
            timeout: Duration::MAX,
        };
        assert!(matches!(invocation.run(), Err(McaoError::Backend(_))));
    }
}
