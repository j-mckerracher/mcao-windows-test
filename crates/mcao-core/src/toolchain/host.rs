//! The seam between the qualification logic and the machine. [`Host`] is the only way a probe reads
//! the environment, reads a file, or starts a process, so tests drive the probes with a fake host.

use std::ffi::{OsStr, OsString};
use std::io;
use std::path::Path;

/// The platform whose rules apply to a run. The real host reports the platform it was compiled for.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Platform {
    Windows,
    Unix,
}

/// What one process run produced. Output is decoded lossily, so it can be compared as text.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RunOutput {
    pub success: bool,
    pub code: Option<i32>,
    pub stdout: String,
    pub stderr: String,
}

/// Machine access for the qualification. Implementations start processes only with argv, never
/// through a shell, `cmd`, or `npm`.
pub trait Host {
    /// The platform whose rules apply to this run.
    fn platform(&self) -> Platform;
    /// Reads an environment variable. An empty value counts as unset.
    fn env_var(&self, name: &str) -> Option<OsString>;
    /// True when `path` is an existing regular file.
    fn is_file(&self, path: &Path) -> bool;
    /// True when `path` is an existing directory.
    fn is_dir(&self, path: &Path) -> bool;
    /// True when `path` is a file the operating system runs directly.
    fn is_runnable(&self, path: &Path) -> bool;
    /// Reads a text file, or `None` when it is absent or unreadable.
    fn read_to_string(&self, path: &Path) -> Option<String>;
    /// Runs `exe` with `args` and waits for it. An error means the process did not start.
    fn run(&self, exe: &Path, args: &[&OsStr]) -> io::Result<RunOutput>;
}

#[cfg(test)]
pub(crate) mod fake {
    use std::cell::RefCell;
    use std::collections::{HashMap, HashSet};
    use std::ffi::{OsStr, OsString};
    use std::io;
    use std::path::{Path, PathBuf};

    use super::{Host, Platform, RunOutput};

    /// An executable path and its argv as lossy text: the key a scripted run is matched by.
    pub(crate) type RunKey = (PathBuf, Vec<String>);

    /// A scripted host. Files, directories, and runnable paths are declared up front, and a process
    /// is scripted by its exe and argv. An unscripted process fails to start, as a missing binary does.
    pub(crate) struct FakeHost {
        platform: Platform,
        env: HashMap<String, OsString>,
        files: HashMap<PathBuf, String>,
        dirs: HashSet<PathBuf>,
        runnable: HashSet<PathBuf>,
        scripted: HashMap<RunKey, RunOutput>,
        runs: RefCell<Vec<RunKey>>,
    }

    impl FakeHost {
        pub(crate) fn new(platform: Platform) -> Self {
            Self {
                platform,
                env: HashMap::new(),
                files: HashMap::new(),
                dirs: HashSet::new(),
                runnable: HashSet::new(),
                scripted: HashMap::new(),
                runs: RefCell::new(Vec::new()),
            }
        }

        pub(crate) fn env(mut self, name: &str, value: impl Into<OsString>) -> Self {
            self.env.insert(name.to_owned(), value.into());
            self
        }

        /// Sets PATH from directory names, joined with the host's own separator.
        pub(crate) fn path_var(self, dirs: &[&str]) -> Self {
            let joined = std::env::join_paths(dirs).expect("test directories hold no separator");
            self.env("PATH", joined)
        }

        pub(crate) fn file(mut self, path: &str, text: &str) -> Self {
            self.files.insert(PathBuf::from(path), text.to_owned());
            self
        }

        pub(crate) fn dir(mut self, path: &str) -> Self {
            self.dirs.insert(PathBuf::from(path));
            self
        }

        /// A runnable file: present on disk and executable.
        pub(crate) fn exe(mut self, path: &str) -> Self {
            let path = PathBuf::from(path);
            self.files.insert(path.clone(), String::new());
            self.runnable.insert(path);
            self
        }

        pub(crate) fn script(mut self, exe: &str, args: &[&str], output: RunOutput) -> Self {
            let argv = args.iter().map(|arg| argv_key(OsStr::new(arg))).collect();
            self.scripted.insert((PathBuf::from(exe), argv), output);
            self
        }

        pub(crate) fn runs(&self) -> Vec<RunKey> {
            self.runs.borrow().clone()
        }
    }

    impl Host for FakeHost {
        fn platform(&self) -> Platform {
            self.platform
        }

        fn env_var(&self, name: &str) -> Option<OsString> {
            self.env
                .get(name)
                .filter(|value| !value.is_empty())
                .cloned()
        }

        fn is_file(&self, path: &Path) -> bool {
            self.files.contains_key(path)
        }

        fn is_dir(&self, path: &Path) -> bool {
            self.dirs.contains(path)
        }

        fn is_runnable(&self, path: &Path) -> bool {
            self.runnable.contains(path)
        }

        fn read_to_string(&self, path: &Path) -> Option<String> {
            self.files.get(path).cloned()
        }

        fn run(&self, exe: &Path, args: &[&OsStr]) -> io::Result<RunOutput> {
            let key: RunKey = (
                exe.to_path_buf(),
                args.iter().map(|arg| argv_key(arg)).collect(),
            );
            self.runs.borrow_mut().push(key.clone());
            self.scripted
                .get(&key)
                .cloned()
                .ok_or_else(|| io::Error::new(io::ErrorKind::NotFound, "process not scripted"))
        }
    }

    /// One argv entry as the key text it is matched by. Path separators fold to `/`: a path built with
    /// `Path::join` takes the host's separator, while a literal script path in a test does not, so
    /// both the scripted and the requested argv pass through this form.
    fn argv_key(arg: &OsStr) -> String {
        arg.to_string_lossy().replace('\\', "/")
    }

    pub(crate) fn ok(stdout: &str) -> RunOutput {
        RunOutput {
            success: true,
            code: Some(0),
            stdout: stdout.to_owned(),
            stderr: String::new(),
        }
    }

    pub(crate) fn failed(code: i32) -> RunOutput {
        RunOutput {
            success: false,
            code: Some(code),
            stdout: String::new(),
            stderr: String::new(),
        }
    }

    /// Asserts that no recorded run started a shell, an npm entry point, or a script shim.
    pub(crate) fn assert_no_shell_or_npm(runs: &[RunKey]) {
        const FORBIDDEN: [&str; 7] = [
            "npm",
            "npm.cmd",
            "cmd",
            "cmd.exe",
            "sh",
            "powershell",
            "powershell.exe",
        ];
        for (exe, _) in runs {
            let name = exe
                .file_name()
                .and_then(|name| name.to_str())
                .unwrap_or_default()
                .to_ascii_lowercase();
            assert!(
                !FORBIDDEN.contains(&name.as_str())
                    && !name.ends_with(".cmd")
                    && !name.ends_with(".bat")
                    && !name.ends_with(".ps1"),
                "a forbidden program was started: {}",
                exe.display()
            );
        }
    }
}

#[cfg(test)]
mod tests {
    use std::ffi::OsStr;
    use std::path::Path;

    use super::fake::{FakeHost, ok};
    use super::{Host, Platform};

    #[test]
    fn scripted_argv_matches_across_path_separator_styles() {
        // A script path built with `Path::join` on Windows carries `\`; the literal in a test carries `/`.
        let host = FakeHost::new(Platform::Windows).script(
            "wbin/node.exe",
            &["wappdata/npm/node_modules/cli.js", "--version"],
            ok("1.0.0"),
        );
        let output = host
            .run(
                Path::new("wbin/node.exe"),
                &[
                    OsStr::new("wappdata\\npm\\node_modules\\cli.js"),
                    OsStr::new("--version"),
                ],
            )
            .expect("the scripted run matches");
        assert_eq!(output.stdout, "1.0.0");
        assert_eq!(
            host.runs(),
            [(
                Path::new("wbin/node.exe").to_path_buf(),
                vec![
                    "wappdata/npm/node_modules/cli.js".to_owned(),
                    "--version".to_owned(),
                ],
            )]
        );
    }
}
