//! OneDrive sync-conflict detection for the mcao home.
//!
//! This module is a thin entry point over the DPES-888 rule in
//! [`crate::preflight::check_home_outside_onedrive`], so the repository has one OneDrive rule with
//! one input contract and one error message. That rule refuses a home under a root named by one of
//! [`crate::preflight::ONEDRIVE_ENV_VARS`], and refuses a home that cannot be compared, such as a
//! relative or device-namespace path. Read its documentation for the exact contract.
//!
//! Detection is by environment root, which refuses, and by a sync folder directly beneath
//! `%USERPROFILE%` that carries OneDrive's name, which only warns. A directory that is merely named
//! `onedrive` elsewhere is not flagged, because a developer's own `onedrive` project folder would be
//! a false positive. A sync folder that no environment variable names and no profile folder name
//! identifies is not detected until the reparse-tag probe arrives in a later slice.

use std::ffi::OsString;
use std::path::Path;

use crate::error::McaoError;
use crate::preflight::{OneDriveCheck, check_home_outside_onedrive};

/// Checks that the mcao home is outside OneDrive, reading roots from the process environment.
///
/// Returns the [`OneDriveCheck`] for a home that is not refused, or `Err(McaoError::Preflight)`
/// naming `FM_HOME` as the remedy when the home is under a OneDrive root or cannot be checked.
pub fn detect_sync_conflict(fm_home: &Path) -> Result<OneDriveCheck, McaoError> {
    detect_sync_conflict_with(fm_home, |name| std::env::var_os(name))
}

/// Checks that the mcao home is outside OneDrive, reading roots through `env`.
///
/// The caller resolves `fm_home` first, as [`check_home_outside_onedrive`] requires.
pub fn detect_sync_conflict_with(
    fm_home: &Path,
    env: impl Fn(&str) -> Option<OsString>,
) -> Result<OneDriveCheck, McaoError> {
    check_home_outside_onedrive(fm_home, env)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Spells a Unix-style absolute test path for the host. On Windows it gains a drive, because a
    /// rooted path without one is not absolute there and the home check refuses it.
    fn host(unix: &str) -> String {
        if cfg!(windows) {
            format!("C:{}", unix.replace('/', "\\"))
        } else {
            unix.to_owned()
        }
    }

    /// An environment lookup that knows exactly one variable.
    fn only(key: &'static str, value: &str) -> impl Fn(&str) -> Option<OsString> {
        let value = OsString::from(host(value));
        move |name| (name == key).then(|| value.clone())
    }

    fn refusal(home: &str, env: impl Fn(&str) -> Option<OsString>) -> String {
        match detect_sync_conflict_with(Path::new(&host(home)), env) {
            Err(McaoError::Preflight(message)) => message,
            other => panic!("expected Err(McaoError::Preflight), got {other:?}"),
        }
    }

    #[test]
    fn every_onedrive_variable_names_a_root() {
        for key in ["OneDrive", "OneDriveCommercial", "OneDriveConsumer"] {
            let message = refusal("/home/u/Sync/mcao", only(key, "/home/u/Sync"));
            assert!(message.contains(&format!("named by {key}")), "{message}");
            assert!(message.contains("FM_HOME"), "{message}");
            assert!(message.contains(&host("/home/u/Sync/mcao")), "{message}");
        }
    }

    #[test]
    fn a_home_equal_to_the_root_is_refused() {
        let message = refusal("/home/u/Sync", only("OneDrive", "/home/u/Sync"));
        assert!(message.contains("under the OneDrive folder"), "{message}");
    }

    #[test]
    fn a_home_beside_the_root_passes() {
        let env = only("OneDrive", "/home/u/OneDrive");
        for home in ["/home/u/OneDrive2/mcao", "/home/u/local/mcao"] {
            detect_sync_conflict_with(Path::new(&host(home)), &env).expect(home);
        }
    }

    #[test]
    fn a_plain_directory_named_onedrive_is_not_flagged() {
        detect_sync_conflict_with(Path::new(&host("/dev/onedrive/proj")), |_| None)
            .expect("no root names it");
        detect_sync_conflict_with(
            Path::new(&host("/dev/onedrive/proj")),
            only("OneDrive", "/home/u/Sync"),
        )
        .expect("a root elsewhere does not name it");
    }

    #[test]
    fn a_verbatim_home_is_compared_without_its_prefix() {
        let verbatim = format!(r"\\?\{}", host("/home/u/Sync/mcao"));
        let env = only("OneDrive", "/home/u/Sync");
        let message = match detect_sync_conflict_with(Path::new(&verbatim), env) {
            Err(McaoError::Preflight(message)) => message,
            other => panic!("expected Err(McaoError::Preflight), got {other:?}"),
        };
        assert!(message.contains("named by OneDrive"), "{message}");
    }

    #[test]
    fn a_relative_home_is_refused_not_compared() {
        let message = refusal("Sync/mcao", only("OneDrive", "/home/u/Sync"));
        assert!(message.contains("cannot be checked"), "{message}");
    }

    #[test]
    fn a_dotdot_home_is_resolved_before_comparison() {
        let env = only("OneDrive", "/home/u/Sync");
        detect_sync_conflict_with(Path::new(&host("/home/u/Sync/../Other/mcao")), &env)
            .expect("the resolved home is outside the root");
        let message = refusal("/home/u/Other/../Sync/mcao", &env);
        assert!(message.contains("named by OneDrive"), "{message}");
    }

    #[test]
    fn an_unset_or_empty_root_names_nothing() {
        detect_sync_conflict_with(Path::new(&host("/home/u/Sync/mcao")), |_| None)
            .expect("no root set");
        detect_sync_conflict_with(Path::new("Sync/mcao"), |_| Some(OsString::new()))
            .expect("an empty root is ignored");
    }
}
