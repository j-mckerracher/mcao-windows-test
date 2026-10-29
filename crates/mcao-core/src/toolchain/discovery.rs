//! Argv-only executable discovery. On Windows a name resolves through PATH to a `.exe` or `.com`
//! file. A `.cmd`, `.bat`, or `.ps1` shim is reported as [`Found::ShimOnly`] and is never returned as
//! runnable. Discovery reads the file system and starts nothing.

use std::path::PathBuf;

use super::host::{Host, Platform};

/// The result of resolving a name on PATH.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Found {
    /// A runnable executable, which may be started with argv.
    Exe(PathBuf),
    /// Only a script shim exists under this name. It is rejected and never started.
    ShimOnly(PathBuf),
    /// Nothing runnable or shim-shaped exists on PATH.
    Missing,
}

/// Windows extensions that may be started directly, in the order they are tried.
const WINDOWS_EXE_EXTENSIONS: [&str; 2] = ["exe", "com"];
/// Windows script shims. They are detected for evidence and never started.
const WINDOWS_SHIM_EXTENSIONS: [&str; 3] = ["cmd", "bat", "ps1"];

/// Resolves `name` through PATH. PATH is split with [`std::env::split_paths`], so the separator is
/// the platform's own.
pub fn find_executable(host: &dyn Host, name: &str) -> Found {
    let Some(path_var) = host.env_var("PATH") else {
        return Found::Missing;
    };
    let dirs: Vec<PathBuf> = std::env::split_paths(&path_var).collect();
    match host.platform() {
        Platform::Unix => {
            for dir in &dirs {
                let candidate = dir.join(name);
                if host.is_runnable(&candidate) {
                    return Found::Exe(candidate);
                }
            }
            Found::Missing
        }
        Platform::Windows => {
            for dir in &dirs {
                for ext in WINDOWS_EXE_EXTENSIONS {
                    let candidate = dir.join(format!("{name}.{ext}"));
                    if host.is_runnable(&candidate) {
                        return Found::Exe(candidate);
                    }
                }
            }
            for dir in &dirs {
                for ext in WINDOWS_SHIM_EXTENSIONS {
                    let candidate = dir.join(format!("{name}.{ext}"));
                    if host.is_file(&candidate) {
                        return Found::ShimOnly(candidate);
                    }
                }
            }
            Found::Missing
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::toolchain::host::fake::FakeHost;

    #[test]
    fn path_split_matches_join_on_this_platform() {
        let joined = std::env::join_paths(["alpha", "beta"]).expect("plain names join");
        let split: Vec<PathBuf> = std::env::split_paths(&joined).collect();
        assert_eq!(split, [PathBuf::from("alpha"), PathBuf::from("beta")]);
    }

    /// Runs only on Windows, where PATH is separated by `;`. Not exercised on the macOS
    /// development host, so the Windows run is unverified until CI or a Windows host runs it.
    #[cfg(windows)]
    #[test]
    fn windows_path_separator_is_semicolon() {
        let split: Vec<PathBuf> =
            std::env::split_paths(std::ffi::OsStr::new(r"C:\a;C:\b")).collect();
        assert_eq!(split, [PathBuf::from(r"C:\a"), PathBuf::from(r"C:\b")]);
    }

    #[test]
    fn windows_resolves_exe_and_rejects_script_shims() {
        let shim_only = FakeHost::new(Platform::Windows)
            .path_var(&["wbin", "wtools"])
            .file("wtools/gh.cmd", "@echo off");
        assert_eq!(
            find_executable(&shim_only, "gh"),
            Found::ShimOnly(PathBuf::from("wtools").join("gh.cmd"))
        );

        let with_exe = shim_only.exe("wbin/gh.exe");
        assert_eq!(
            find_executable(&with_exe, "gh"),
            Found::Exe(PathBuf::from("wbin").join("gh.exe"))
        );
    }

    #[test]
    fn windows_reports_bat_and_ps1_shims_without_running_them() {
        let host = FakeHost::new(Platform::Windows)
            .path_var(&["wbin"])
            .file("wbin/tool.bat", "")
            .file("wbin/other.ps1", "");
        assert!(matches!(find_executable(&host, "tool"), Found::ShimOnly(_)));
        assert!(matches!(
            find_executable(&host, "other"),
            Found::ShimOnly(_)
        ));
        assert!(host.runs().is_empty());
    }

    #[test]
    fn windows_missing_name_is_missing() {
        let host = FakeHost::new(Platform::Windows).path_var(&["wbin"]);
        assert_eq!(find_executable(&host, "absent"), Found::Missing);
    }

    #[test]
    fn unix_requires_an_executable_file() {
        let plain = FakeHost::new(Platform::Unix)
            .path_var(&["ubin"])
            .file("ubin/tool", "");
        assert_eq!(find_executable(&plain, "tool"), Found::Missing);

        let runnable = plain.exe("ubin/tool");
        assert_eq!(
            find_executable(&runnable, "tool"),
            Found::Exe(PathBuf::from("ubin").join("tool"))
        );
    }

    #[test]
    fn unset_path_finds_nothing() {
        let host = FakeHost::new(Platform::Windows).exe("wbin/gh.exe");
        assert_eq!(find_executable(&host, "gh"), Found::Missing);
    }
}
