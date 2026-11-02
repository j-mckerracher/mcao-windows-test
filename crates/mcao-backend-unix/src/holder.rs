//! Who holds a lock file and its companion directory open, as reported by `lsof`.
//!
//! [`UnixHolderProbe`] implements the core [`HolderProbe`] contract. It answers
//! [`HolderCheck::Dead`] only when `lsof` proves that neither path is held. `lsof` exits 0 when it
//! selects a process that holds the path, and 1 when it selects none. Exit 1 is a proof of "none"
//! only with empty stdout and empty stderr. Any other output, exit status, or a missing `lsof` is
//! [`HolderCheck::Unknown`], so the probe never reports a holder-free path it could not check.

use std::ffi::{OsStr, OsString};
use std::path::Path;
use std::process::{Command, Stdio};

use mcao_core::lock::{HolderCheck, HolderProbe};

/// The `lsof` program, looked up on `PATH`.
const LSOF_PROGRAM: &str = "lsof";

/// Asks `lsof` who holds a lock and its companion directory open.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct UnixHolderProbe {
    program: OsString,
}

impl UnixHolderProbe {
    /// A probe that runs `lsof` from `PATH`.
    pub fn new() -> Self {
        Self {
            program: OsString::from(LSOF_PROGRAM),
        }
    }

    /// A probe that runs `program` instead of `lsof` from `PATH`.
    pub fn with_program(program: &OsStr) -> Self {
        Self {
            program: program.to_os_string(),
        }
    }

    /// The answer for one path, in the core vocabulary: `Alive` when a process holds it, `Dead`
    /// when lsof proves none does, and `Unknown` otherwise.
    ///
    /// `--` ends lsof's option parsing, so a path that begins with `-` is still a path.
    fn check_path(&self, path: &Path) -> HolderCheck {
        let output = Command::new(&self.program)
            .args(["-F", "p", "--"])
            .arg(path)
            .stdin(Stdio::null())
            .output();
        let Ok(output) = output else {
            return HolderCheck::Unknown;
        };
        match output.status.code() {
            Some(0) if reports_a_process(&output.stdout) => HolderCheck::Alive,
            Some(1) if output.stdout.is_empty() && output.stderr.is_empty() => HolderCheck::Dead,
            _ => HolderCheck::Unknown,
        }
    }
}

impl Default for UnixHolderProbe {
    fn default() -> Self {
        Self::new()
    }
}

impl HolderProbe for UnixHolderProbe {
    /// `Alive` when either path has a holder, `Dead` only when both are proven unheld, and
    /// `Unknown` in every other case.
    fn holder(&self, lock: &Path, dir: &Path) -> HolderCheck {
        match (self.check_path(lock), self.check_path(dir)) {
            (HolderCheck::Alive, _) | (_, HolderCheck::Alive) => HolderCheck::Alive,
            (HolderCheck::Dead, HolderCheck::Dead) => HolderCheck::Dead,
            _ => HolderCheck::Unknown,
        }
    }
}

/// True when lsof's `-F p` output names at least one process id.
fn reports_a_process(stdout: &[u8]) -> bool {
    String::from_utf8_lossy(stdout)
        .lines()
        .any(|line| line.starts_with('p'))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn process_lines_mean_a_holder_is_named() {
        assert!(reports_a_process(b"p4242\nf3\n"));
    }

    #[test]
    fn output_without_a_process_line_names_no_holder() {
        assert!(!reports_a_process(b""));
        assert!(!reports_a_process(b"f3\n"));
    }
}
