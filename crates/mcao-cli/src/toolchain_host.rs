//! The machine behind the qualification [`Host`]: the process environment, the file system, and
//! process starts with argv only. Nothing here starts a shell, `cmd`, or `npm`.

use std::ffi::{OsStr, OsString};
use std::io::{self, ErrorKind, Read};
use std::path::Path;
use std::process::{Command, Stdio};
use std::sync::mpsc::{self, Receiver, RecvTimeoutError};
use std::thread;
use std::time::{Duration, Instant};

use mcao_core::toolchain::{Host, Platform, RunOutput};

/// How long one probe process may run, output included, before it is killed. A wedged tool must not
/// hang `qualify`.
const PROBE_DEADLINE: Duration = Duration::from_secs(30);

/// How long all probe processes of one `qualify` run may take together. The probes run one after
/// another, so a per-process limit alone let a run of wedged tools take several minutes.
const QUALIFY_BUDGET: Duration = Duration::from_secs(60);

/// How often a running probe process is checked for exit.
const POLL_INTERVAL: Duration = Duration::from_millis(10);

/// The host `mcao toolchain` probes. Every process start is bounded by `deadline`, and all starts
/// share one budget that ends at `budget_ends`.
pub struct RealHost {
    deadline: Duration,
    budget_ends: Instant,
}

impl RealHost {
    /// A host whose processes each run at most `deadline`, and whose starts all end within `budget`
    /// of this call.
    fn new(deadline: Duration, budget: Duration) -> Self {
        Self {
            deadline,
            budget_ends: Instant::now() + budget,
        }
    }
}

impl Default for RealHost {
    fn default() -> Self {
        Self::new(PROBE_DEADLINE, QUALIFY_BUDGET)
    }
}

impl Host for RealHost {
    fn platform(&self) -> Platform {
        if cfg!(windows) {
            Platform::Windows
        } else {
            Platform::Unix
        }
    }

    fn env_var(&self, name: &str) -> Option<OsString> {
        std::env::var_os(name).filter(|value| !value.is_empty())
    }

    fn is_file(&self, path: &Path) -> bool {
        path.is_file()
    }

    fn is_dir(&self, path: &Path) -> bool {
        path.is_dir()
    }

    fn is_runnable(&self, path: &Path) -> bool {
        path.is_file() && is_executable(path)
    }

    fn read_to_string(&self, path: &Path) -> Option<String> {
        std::fs::read_to_string(path).ok()
    }

    /// Starts `exe` with argv, waits for it to exit, and returns its output. The process is killed when
    /// it runs past its limit, which is the per-process deadline or the budget left, whichever ends
    /// first. Output still held open past that limit is an error, and no process starts once the
    /// budget is spent.
    fn run(&self, exe: &Path, args: &[&OsStr]) -> io::Result<RunOutput> {
        let started = Instant::now();
        if started >= self.budget_ends {
            return Err(io::Error::new(
                ErrorKind::TimedOut,
                "the qualification time budget was spent, so this probe did not start",
            ));
        }
        let deadline = (started + self.deadline).min(self.budget_ends);
        let limit = deadline.saturating_duration_since(started);
        let budget_ends_first = self.budget_ends < started + self.deadline;
        let mut child = Command::new(exe)
            .args(args)
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()?;
        let stdout = read_in_background(child.stdout.take());
        let stderr = read_in_background(child.stderr.take());
        let status = loop {
            if let Some(status) = child.try_wait()? {
                break status;
            }
            if Instant::now() >= deadline {
                // The kill is reaped so a killed probe does not linger as a zombie.
                let _ = child.kill();
                let _ = child.wait();
                let cause = if budget_ends_first {
                    " when the qualification budget ran out"
                } else {
                    ""
                };
                return Err(io::Error::new(
                    ErrorKind::TimedOut,
                    format!("timed out after {limit:?}{cause} and was killed"),
                ));
            }
            thread::sleep(POLL_INTERVAL);
        };
        let stdout = collect(&stdout, deadline)?;
        let stderr = collect(&stderr, deadline)?;
        Ok(RunOutput {
            success: status.success(),
            code: status.code(),
            stdout: String::from_utf8_lossy(&stdout).into_owned(),
            stderr: String::from_utf8_lossy(&stderr).into_owned(),
        })
    }
}

/// Reads a child's output pipe on its own thread, so a full pipe cannot stall the wait for exit.
fn read_in_background<R: Read + Send + 'static>(pipe: Option<R>) -> Receiver<Vec<u8>> {
    let (sender, receiver) = mpsc::channel();
    thread::spawn(move || {
        let mut bytes = Vec::new();
        if let Some(mut pipe) = pipe {
            let _ = pipe.read_to_end(&mut bytes);
        }
        // The receiver is gone only after the probe gave up, and then nothing reads these bytes.
        let _ = sender.send(bytes);
    });
    receiver
}

/// The bytes one pipe produced, waiting no later than `deadline`. A pipe that a grandchild still holds
/// open is reported as an error rather than waited on, so the probe stays bounded.
fn collect(output: &Receiver<Vec<u8>>, deadline: Instant) -> io::Result<Vec<u8>> {
    output
        .recv_timeout(deadline.saturating_duration_since(Instant::now()))
        .map_err(|error| match error {
            RecvTimeoutError::Timeout => io::Error::new(
                ErrorKind::TimedOut,
                "output was still open after the deadline",
            ),
            // The reader thread always sends before it exits, so a disconnect is not a normal result.
            RecvTimeoutError::Disconnected => {
                io::Error::other("output reader stopped unexpectedly")
            }
        })
}

#[cfg(all(test, unix))]
mod tests {
    use std::ffi::OsStr;
    use std::io::ErrorKind;
    use std::path::Path;
    use std::time::{Duration, Instant};

    use super::RealHost;
    use mcao_core::toolchain::Host;

    #[test]
    fn captures_stdout_and_success() {
        let host = RealHost::new(Duration::from_secs(10), Duration::from_secs(60));
        let output = host
            .run(Path::new("/bin/echo"), &[OsStr::new("hello")])
            .expect("echo starts");
        assert!(output.success);
        assert_eq!(output.code, Some(0));
        assert_eq!(output.stdout, "hello\n");
    }

    #[test]
    fn a_process_past_the_deadline_is_killed_and_reported() {
        let host = RealHost::new(Duration::from_millis(300), Duration::from_secs(60));
        let started = Instant::now();
        let error = host
            .run(Path::new("/bin/sleep"), &[OsStr::new("30")])
            .expect_err("a process past its deadline is an error");
        assert_eq!(error.kind(), ErrorKind::TimedOut);
        assert!(error.to_string().contains("killed"), "{error}");
        // The per-process deadline fired, so the message must not blame the budget.
        assert!(!error.to_string().contains("budget"), "{error}");
        assert!(started.elapsed() < Duration::from_secs(10));
    }

    #[test]
    fn output_held_open_by_a_grandchild_is_bounded_by_the_deadline() {
        // The shell exits at once, but the background sleep keeps the output pipes open. The probe
        // must not wait for that grandchild. This test drives the shell itself; the product never does.
        let host = RealHost::new(Duration::from_millis(300), Duration::from_secs(60));
        let started = Instant::now();
        let error = host
            .run(
                Path::new("/bin/sh"),
                &[OsStr::new("-c"), OsStr::new("sleep 3 &")],
            )
            .expect_err("output still open at the deadline is an error");
        assert_eq!(error.kind(), ErrorKind::TimedOut);
        assert!(started.elapsed() < Duration::from_secs(3));
    }

    #[test]
    fn probes_share_one_budget_and_none_starts_after_it_is_spent() {
        let host = RealHost::new(Duration::from_secs(30), Duration::from_millis(300));
        let started = Instant::now();
        let first = host
            .run(Path::new("/bin/sleep"), &[OsStr::new("30")])
            .expect_err("the budget ends a probe that outlives it");
        assert_eq!(first.kind(), ErrorKind::TimedOut);
        assert!(first.to_string().contains("killed"), "{first}");
        // The budget ended the probe before its own deadline, and the message says so.
        assert!(
            first.to_string().contains("qualification budget"),
            "{first}"
        );
        let second = host
            .run(Path::new("/bin/echo"), &[OsStr::new("hello")])
            .expect_err("a probe that starts after the budget is refused");
        assert_eq!(second.kind(), ErrorKind::TimedOut);
        assert!(second.to_string().contains("budget"), "{second}");
        assert!(started.elapsed() < Duration::from_secs(10));
    }
}

#[cfg(all(test, windows))]
mod windows_tests {
    use std::ffi::OsStr;
    use std::io::ErrorKind;
    use std::path::Path;
    use std::time::{Duration, Instant};

    use super::RealHost;
    use mcao_core::toolchain::Host;

    #[test]
    fn a_process_past_the_deadline_is_killed_and_reported_on_windows() {
        // ping.exe is a native executable started with argv only. With -n 30 it runs for about 30
        // seconds, far past the 300 ms deadline.
        let host = RealHost::new(Duration::from_millis(300), Duration::from_secs(60));
        let started = Instant::now();
        let error = host
            .run(
                Path::new("ping.exe"),
                &[OsStr::new("-n"), OsStr::new("30"), OsStr::new("127.0.0.1")],
            )
            .expect_err("a process past its deadline is an error");
        assert_eq!(error.kind(), ErrorKind::TimedOut);
        assert!(error.to_string().contains("killed"), "{error}");
        assert!(started.elapsed() < Duration::from_secs(10));
    }
}

#[cfg(unix)]
fn is_executable(path: &Path) -> bool {
    use std::os::unix::fs::PermissionsExt;

    path.metadata()
        .is_ok_and(|meta| meta.permissions().mode() & 0o111 != 0)
}

/// Windows decides runnability by extension, which discovery has already applied.
#[cfg(not(unix))]
fn is_executable(_path: &Path) -> bool {
    true
}
