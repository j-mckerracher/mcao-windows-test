//! Command-line contract for `mcao`: offline `--version` and `--help`, usage errors that exit 2,
//! and `util` arguments that reach `util` unchanged.

use std::ffi::OsString;
use std::path::Path;
use std::process::{Command, Output, Stdio};
use std::{env, fs};

const UTIL_USAGE: &str = "usage: mcao util <now-ms|awaiting-hold-ids|awaiting-hold-object>\n";

/// Builds an `mcao` invocation whose colour output does not depend on the caller's environment.
fn command(args: &[&str]) -> Command {
    let mut command = Command::new(env!("CARGO_BIN_EXE_mcao"));
    command
        .args(args)
        .env_remove("CLICOLOR_FORCE")
        .env("NO_COLOR", "1")
        .stdin(Stdio::null());
    command
}

fn mcao(args: &[&str]) -> Output {
    command(args).output().expect("mcao runs")
}

/// An argument that is not valid Unicode: an invalid byte on Unix, an unpaired surrogate on Windows.
fn non_unicode_arg() -> OsString {
    #[cfg(unix)]
    {
        std::os::unix::ffi::OsStringExt::from_vec(vec![0xff])
    }
    #[cfg(windows)]
    {
        std::os::windows::ffi::OsStringExt::from_wide(&[0xd800])
    }
}

#[test]
fn version_flag_prints_the_package_version_and_exits_zero() {
    let output = mcao(&["--version"]);
    assert_eq!(output.status.code(), Some(0));
    assert_eq!(
        String::from_utf8_lossy(&output.stdout),
        format!("mcao {}\n", env!("CARGO_PKG_VERSION"))
    );
    assert!(output.stderr.is_empty());
}

#[test]
fn help_flag_lists_util_and_succeeds_without_a_home() {
    // mcao never reads FM_HOME for help, so pointing it at a missing directory changes nothing.
    let output = command(&["--help"])
        .env(
            "FM_HOME",
            Path::new(env!("CARGO_MANIFEST_DIR")).join("no-such-home"),
        )
        .output()
        .expect("mcao runs");
    assert_eq!(output.status.code(), Some(0));
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(stdout.contains("Usage: mcao [COMMAND]"), "{stdout}");
    assert!(stdout.contains("  util"), "{stdout}");
    assert!(output.stderr.is_empty());
}

#[test]
fn unknown_top_level_command_is_a_usage_error_with_exit_two() {
    let output = mcao(&["bogus"]);
    assert_eq!(output.status.code(), Some(2));
    assert!(output.stdout.is_empty());
    assert_eq!(
        String::from_utf8_lossy(&output.stderr),
        "Usage: unrecognized subcommand 'bogus' (see mcao --help)\n"
    );
}

#[test]
fn util_arguments_reach_util_unchanged() {
    // `--help` after `util` belongs to util, which answers with its own usage and exit 2.
    // A leading `--` is an argument to util, not an end-of-options marker for mcao.
    let cases: [&[&str]; 5] = [
        &["util"],
        &["util", "nope"],
        &["util", "--help"],
        &["util", "-x"],
        &["util", "--", "now-ms"],
    ];
    for args in cases {
        let output = mcao(args);
        assert_eq!(output.status.code(), Some(2), "{args:?}");
        assert!(output.stdout.is_empty(), "{args:?}");
        assert_eq!(
            String::from_utf8_lossy(&output.stderr),
            UTIL_USAGE,
            "{args:?}"
        );
    }
}

#[test]
fn util_now_ms_still_runs_through_dispatch() {
    let output = mcao(&["util", "now-ms"]);
    assert_eq!(output.status.code(), Some(0));
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(
        stdout.trim_end().chars().all(|c| c.is_ascii_digit()) && !stdout.trim().is_empty(),
        "{stdout}"
    );
}

#[test]
fn non_unicode_util_arguments_are_a_usage_error_with_exit_two() {
    let cases: [Vec<OsString>; 2] = [
        vec!["util".into(), non_unicode_arg()],
        vec!["util".into(), "now-ms".into(), non_unicode_arg()],
    ];
    for args in cases {
        let output = command(&[]).args(&args).output().expect("mcao runs");
        assert_eq!(output.status.code(), Some(2), "{args:?}");
        assert!(output.stdout.is_empty(), "{args:?}");
        assert_eq!(
            String::from_utf8_lossy(&output.stderr),
            "Usage: invalid UTF-8 was detected in one or more arguments (see mcao --help)\n",
            "{args:?}"
        );
    }
}

#[test]
fn help_names_the_program_mcao_whatever_the_binary_file_is_called() {
    // clap derives a usage name from argv[0] unless `bin_name` is set, so a renamed binary shows
    // the guard on every platform, not only where the binary is `mcao.exe`.
    // A hard link rather than a copy: a parallel test's fork can hold a fresh copy's write
    // descriptor, and exec of that file then fails with ETXTBSY. A link opens nothing for writing.
    let renamed = Path::new(env!("CARGO_TARGET_TMPDIR"))
        .join(format!("renamed-mcao-help{}", env::consts::EXE_SUFFIX));
    // A leftover link from an interrupted run would make the new link fail.
    let _ = fs::remove_file(&renamed);
    fs::hard_link(env!("CARGO_BIN_EXE_mcao"), &renamed)
        .expect("link the mcao binary under a new name");
    let output = Command::new(&renamed)
        .arg("--help")
        .env_remove("CLICOLOR_FORCE")
        .env("NO_COLOR", "1")
        .stdin(Stdio::null())
        .output();
    fs::remove_file(&renamed).expect("remove the renamed link");
    let output = output.expect("renamed mcao runs");
    assert_eq!(output.status.code(), Some(0));
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(stdout.contains("Usage: mcao [COMMAND]"), "{stdout}");
    assert!(!stdout.contains("renamed-mcao-help"), "{stdout}");
}

#[test]
fn help_for_util_is_user_facing_text_only() {
    let output = mcao(&["help", "util"]);
    assert_eq!(output.status.code(), Some(0));
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(stdout.contains("Developer helpers"), "{stdout}");
    assert!(!stdout.contains("clap"), "{stdout}");
    assert!(!stdout.contains("dispatches"), "{stdout}");
}
