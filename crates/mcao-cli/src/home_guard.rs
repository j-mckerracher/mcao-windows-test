//! The OneDrive guard for stateful `mcao` commands (DPES-892 AC4 and AC5).
//!
//! A stateful command calls [`guarded`] before it reads or writes any state, so a positively
//! detected OneDrive-synchronized home refuses with exit 4 and nothing is created. The detection
//! and the relocation message are those of `mcao_core::preflight`. Help, version, `preflight`, and
//! the stateless commands never call this guard.

use std::ffi::OsString;
use std::path::PathBuf;
use std::process::ExitCode;

use mcao_core::McaoError;
use mcao_core::preflight::{default_home, refuse_onedrive_home};

/// Runs `command` when the operational home passes the OneDrive rule. A refusal prints its
/// relocation guidance on standard error and returns its exit status, 4, without running `command`.
pub fn guarded(command: impl FnOnce() -> ExitCode) -> ExitCode {
    match refuse_onedrive_home_for_state(|name| std::env::var_os(name)) {
        Ok(()) => command(),
        Err(error) => {
            eprintln!("mcao: {error}");
            let code = u8::try_from(error.exit_code()).expect("mcao exit codes fit in a byte");
            ExitCode::from(code)
        }
    }
}

/// Refuses when the operational home resolved from `env` is OneDrive-synchronized. A home with no
/// resolution, which is any unix host without `FM_HOME`, has nothing to check.
pub fn refuse_onedrive_home_for_state(
    env: impl Fn(&str) -> Option<OsString>,
) -> Result<(), McaoError> {
    match resolved_home(&env)? {
        Some(home) => refuse_onedrive_home(&home, &env),
        None => Ok(()),
    }
}

/// Reports a refused operational home on standard error and returns `true`, so a hook writes
/// nothing and stands down with its own exit contract. A home that may be used returns `false`.
/// A home that cannot be placed or compared is refused too, since the rule cannot be checked.
pub fn hook_home_refused() -> bool {
    match refuse_onedrive_home_for_state(|name| std::env::var_os(name)) {
        Ok(()) => false,
        Err(error) => {
            eprintln!("mcao: {error}");
            true
        }
    }
}

/// The operational home a stateful command resolves: `FM_HOME` when it is set and non-empty.
/// Without it, Windows uses the default home under `%LOCALAPPDATA%`, and other hosts have none.
///
/// # Errors
///
/// Returns the error from [`default_home`] when the Windows default cannot be placed.
pub fn resolved_home(env: impl Fn(&str) -> Option<OsString>) -> Result<Option<PathBuf>, McaoError> {
    if let Some(home) = env("FM_HOME").filter(|home| !home.is_empty()) {
        return Ok(Some(PathBuf::from(home)));
    }
    if cfg!(windows) {
        return default_home(env).map(Some);
    }
    Ok(None)
}

#[cfg(test)]
mod tests {
    use mcao_core::preflight::ONEDRIVE_RELOCATION;

    use super::*;

    /// Spells a Unix-style absolute test path for the host, as the core tests do.
    fn host(unix: &str) -> String {
        if cfg!(windows) {
            format!("C:{}", unix.replace('/', "\\"))
        } else {
            unix.to_owned()
        }
    }

    /// An environment lookup over the given name and value pairs.
    fn env_of<'a>(pairs: &'a [(&'a str, String)]) -> impl Fn(&str) -> Option<OsString> + 'a {
        move |name| {
            pairs
                .iter()
                .find(|(key, _)| *key == name)
                .map(|(_, value)| OsString::from(value))
        }
    }

    #[test]
    fn a_selected_fm_home_is_the_resolved_home() {
        let pairs = [("FM_HOME", host("/home/u/fm"))];
        let home = resolved_home(env_of(&pairs)).expect("resolves");

        assert_eq!(home, Some(PathBuf::from(host("/home/u/fm"))));
    }

    #[test]
    fn an_empty_fm_home_counts_as_unset() {
        let pairs = [("FM_HOME", String::new())];

        if cfg!(unix) {
            assert_eq!(resolved_home(env_of(&pairs)).expect("resolves"), None);
        }
    }

    #[test]
    fn an_unset_home_resolves_to_the_windows_default_only_on_windows() {
        let pairs = [("LOCALAPPDATA", host("/home/u/AppData/Local"))];
        let resolved = resolved_home(env_of(&pairs));

        if cfg!(windows) {
            let home = resolved.expect("LOCALAPPDATA is absolute");
            assert_eq!(home, Some(default_home(env_of(&pairs)).expect("default")));
        } else {
            assert_eq!(resolved.expect("resolves"), None);
        }
    }

    #[test]
    fn a_home_under_an_environment_root_refuses_with_exit_four() {
        let pairs = [
            ("FM_HOME", host("/home/u/OneDrive/fm")),
            ("OneDrive", host("/home/u/OneDrive")),
        ];
        let error = refuse_onedrive_home_for_state(env_of(&pairs)).expect_err("under OneDrive");

        assert!(matches!(error, McaoError::Refused(_)), "{error:?}");
        assert_eq!(error.exit_code(), 4);
        assert!(error.to_string().contains(ONEDRIVE_RELOCATION), "{error}");
    }

    #[test]
    fn a_name_only_sync_folder_does_not_refuse() {
        let pairs = [
            ("FM_HOME", host("/home/u/OneDrive - Contoso/fm")),
            ("USERPROFILE", host("/home/u")),
        ];

        refuse_onedrive_home_for_state(env_of(&pairs)).expect("a name alone only warns");
    }

    #[test]
    fn a_home_outside_every_signal_passes() {
        let pairs = [
            ("FM_HOME", host("/home/u/fm")),
            ("OneDrive", host("/home/u/OneDrive")),
        ];

        refuse_onedrive_home_for_state(env_of(&pairs)).expect("outside OneDrive");
    }
}
