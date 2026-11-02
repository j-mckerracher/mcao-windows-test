//! The stub harness: a small child process that stands in for an agent harness.
//!
//! It is the probe's own binary started as `mcao-probe stub-harness`, so no shell, interpreter or
//! credential is needed on any platform. It claims nothing about Claude Code or any other real
//! harness. Every run is bounded by a lifetime, so a child that outlives its controller exits on its
//! own.
//!
//! With stdin it speaks a line protocol on stdout: it reports `state: idle` when ready, and a line of
//! input starts a turn (`state: running`). One Escape byte (0x1b) stops a running turn unless the
//! stub was started with `--ignore-escape`. Without stdin (`--no-stdin`) it only stays alive, which
//! is all a process backend with no terminal can observe.

use std::env;
use std::io::{self, Read, Write};
use std::process::{Child, Command, Stdio};
use std::sync::mpsc;
use std::thread;
use std::time::{Duration, Instant};

use crate::lifecycle::STOP_MARKER;

/// The harness name recorded in evidence for this stub.
pub const HARNESS: &str = "stub";
/// First argument that turns the probe binary into the stub harness.
pub const SUBCOMMAND: &str = "stub-harness";
/// Printed once the stub is listening.
pub const READY: &str = "stub harness ready";
pub const STATE_IDLE: &str = "state: idle";
pub const STATE_RUNNING: &str = "state: running";
/// The byte a controller sends as the interrupt key.
pub const ESCAPE: u8 = 0x1b;

const DEFAULT_LIFETIME: Duration = Duration::from_secs(30);

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Options {
    /// Keep running when an Escape arrives.
    pub ignore_escape: bool,
    /// Do not read stdin or print. Used where the controller attaches no terminal.
    pub no_stdin: bool,
    /// Start one child of the same kind, so a controller can check that it owns a process tree.
    pub with_child: bool,
    pub lifetime: Duration,
}

impl Default for Options {
    fn default() -> Self {
        Self {
            ignore_escape: false,
            no_stdin: false,
            with_child: false,
            lifetime: DEFAULT_LIFETIME,
        }
    }
}

impl Options {
    /// Parses the arguments after `stub-harness`.
    pub fn parse<I>(args: I) -> Result<Self, String>
    where
        I: IntoIterator<Item = String>,
    {
        let mut options = Self::default();
        let mut args = args.into_iter();
        while let Some(flag) = args.next() {
            match flag.as_str() {
                "--ignore-escape" => options.ignore_escape = true,
                "--no-stdin" => options.no_stdin = true,
                "--with-child" => options.with_child = true,
                "--lifetime-secs" => {
                    let secs = args
                        .next()
                        .and_then(|value| value.parse::<u64>().ok())
                        .ok_or("--lifetime-secs needs a whole number of seconds")?;
                    options.lifetime = Duration::from_secs(secs);
                }
                other => return Err(format!("unknown stub-harness flag `{other}`")),
            }
        }
        Ok(options)
    }

    /// The arguments that recreate these options after `stub-harness`.
    pub fn to_args(self) -> Vec<String> {
        let mut args = Vec::new();
        if self.ignore_escape {
            args.push("--ignore-escape".to_owned());
        }
        if self.no_stdin {
            args.push("--no-stdin".to_owned());
        }
        if self.with_child {
            args.push("--with-child".to_owned());
        }
        args.push("--lifetime-secs".to_owned());
        args.push(self.lifetime.as_secs().to_string());
        args
    }
}

/// Runs the stub until its lifetime ends, or, with stdin, until stdin closes.
pub fn run(options: Options) -> io::Result<()> {
    let deadline = Instant::now() + options.lifetime;
    let mut child = options
        .with_child
        .then(|| {
            let child_options = Options {
                with_child: false,
                no_stdin: true,
                ..options
            };
            spawn_detached(child_options)
        })
        .transpose()?;

    let result = if options.no_stdin {
        thread::sleep(options.lifetime);
        Ok(())
    } else {
        converse(options, deadline)
    };

    if let Some(child) = child.as_mut() {
        // Best effort: the child also stops at its own deadline.
        let _ = child.kill();
        let _ = child.wait();
    }
    result
}

fn spawn_detached(options: Options) -> io::Result<Child> {
    Command::new(env::current_exe()?)
        .arg(SUBCOMMAND)
        .args(options.to_args())
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
}

fn converse(options: Options, deadline: Instant) -> io::Result<()> {
    let mut out = io::stdout().lock();
    writeln!(out, "{READY}")?;
    writeln!(out, "{STATE_IDLE}")?;

    let (sender, receiver) = mpsc::channel();
    thread::spawn(move || {
        for byte in io::stdin().lock().bytes() {
            match byte {
                Ok(byte) if sender.send(byte).is_ok() => {}
                _ => break,
            }
        }
    });

    let mut line = Vec::new();
    let mut running = false;
    loop {
        let Some(remaining) = deadline.checked_duration_since(Instant::now()) else {
            return Ok(());
        };
        match receiver.recv_timeout(remaining) {
            Ok(ESCAPE) => {
                if running && !options.ignore_escape {
                    running = false;
                    writeln!(out, "{STOP_MARKER}")?;
                    writeln!(out, "{STATE_IDLE}")?;
                }
            }
            Ok(b'\n') => {
                writeln!(out, "> {}", String::from_utf8_lossy(&line))?;
                writeln!(out, "{STATE_RUNNING}")?;
                running = true;
                line.clear();
            }
            Ok(byte) => line.push(byte),
            // Lifetime over, or the controller closed stdin.
            Err(_) => return Ok(()),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn parse(args: &[&str]) -> Result<Options, String> {
        Options::parse(args.iter().map(|arg| (*arg).to_owned()))
    }

    #[test]
    fn options_round_trip_through_arguments() {
        let options = Options {
            ignore_escape: true,
            no_stdin: true,
            with_child: true,
            lifetime: Duration::from_secs(7),
        };
        assert_eq!(Options::parse(options.to_args()), Ok(options));
    }

    #[test]
    fn defaults_are_cooperative_with_stdin_and_a_bounded_lifetime() {
        assert_eq!(parse(&[]), Ok(Options::default()));
        assert_eq!(Options::default().lifetime, Duration::from_secs(30));
    }

    #[test]
    fn unknown_flag_and_bad_lifetime_are_refused() {
        assert_eq!(
            parse(&["--bogus"]),
            Err("unknown stub-harness flag `--bogus`".to_owned())
        );
        assert!(parse(&["--lifetime-secs", "soon"]).is_err());
        assert!(parse(&["--lifetime-secs"]).is_err());
    }
}
