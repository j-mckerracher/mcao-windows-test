//! Pins the path order and the non-UTF-8 directory handling of `Config::skipped`, which the
//! existing config tests leave unchecked. Both fixtures need a file name that is not UTF-8.
//! APFS refuses such names with EILSEQ, so this file runs only on Unix filesystems other than
//! macOS, as the non-UTF-8 name fixture in `config.rs` does.
#![cfg(all(unix, not(target_os = "macos")))]

mod support;

use std::collections::BTreeMap;
use std::ffi::OsStr;
use std::fs;
use std::os::unix::ffi::OsStrExt;
use std::os::unix::fs::symlink;

use mcao_core::config;
use mcao_core::config::{SkipReason, Skipped};
use mcao_core::home::{Env, FmHome, resolve};
use support::{TempDir, env, text, write};

fn fixture_home(tmp: &TempDir) -> FmHome {
    resolve(
        None,
        &env(&[("FM_HOME", text(&tmp.path().join("home")))]),
        tmp.path(),
        &tmp.path().join("code"),
    )
    .expect("fixture home resolves")
}

#[test]
fn skipped_entries_are_sorted_by_path_not_by_scan_order() {
    let tmp = TempDir::new("skip-sort");
    let home = fixture_home(&tmp);
    fs::create_dir_all(&home.config).expect("create config dir");
    // The scan records a non-UTF-8 name before it reaches any link, so an unsorted listing
    // would start with `z-` and the path-ordered listing starts with `a-link`.
    let non_utf8 = OsStr::from_bytes(b"z-\xff");
    write(&home.config.join(non_utf8), "value\n");
    symlink("nowhere", home.config.join("a-link")).expect("create dangling link");

    let loaded = config::load(&home, &BTreeMap::new(), &Env::new())
        .expect("unknown entries do not abort the load");
    assert_eq!(
        loaded.skipped().to_vec(),
        vec![
            Skipped {
                path: home.config.join("a-link"),
                reason: SkipReason::Symlink,
            },
            Skipped {
                path: home.config.join(non_utf8),
                reason: SkipReason::NameNotUtf8,
            },
        ],
        "skipped entries must follow path order, whatever order the scan found them"
    );
}

#[test]
fn non_utf8_directory_under_config_is_neither_skipped_nor_an_error() {
    let tmp = TempDir::new("skip-dir");
    let home = fixture_home(&tmp);
    let directory = home.config.join(OsStr::from_bytes(b"sub-\xff"));
    fs::create_dir_all(&directory).expect("create non-UTF-8 directory");

    let loaded = config::load(&home, &BTreeMap::new(), &Env::new())
        .expect("a non-UTF-8 directory does not abort the load");
    assert!(
        loaded.skipped().is_empty(),
        "a directory is never a skipped entry, whatever its name: {:?}",
        loaded.skipped()
    );
}
