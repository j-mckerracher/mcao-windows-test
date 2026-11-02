//! Checks [`UnixHolderProbe`] against real files, a real companion directory, and a real `lsof`.

#![cfg(unix)]

use std::ffi::OsStr;
use std::fs::{self, File};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};

use mcao_backend_unix::UnixHolderProbe;
use mcao_core::lock::{HolderCheck, HolderProbe};

/// A scratch file or directory under the temp directory, removed when dropped.
struct Scratch {
    path: PathBuf,
}

impl Scratch {
    fn file(name: &str) -> Self {
        let scratch = Self::named(name);
        File::create(&scratch.path).expect("create the scratch file");
        scratch
    }

    fn directory(name: &str) -> Self {
        let scratch = Self::named(name);
        fs::create_dir(&scratch.path).expect("create the scratch directory");
        scratch
    }

    fn named(name: &str) -> Self {
        let path = std::env::temp_dir().join(format!("mcao-holder-{}-{name}", std::process::id()));
        Self { path }
    }
}

impl Drop for Scratch {
    fn drop(&mut self) {
        let _ = if self.path.is_dir() {
            fs::remove_dir_all(&self.path)
        } else {
            fs::remove_file(&self.path)
        };
    }
}

/// A process that holds a path open, killed when dropped. It holds a file through its standard
/// input, or a directory as its working directory.
struct Holder {
    child: Child,
}

impl Holder {
    fn through_file(path: &Path) -> Self {
        let file = File::open(path).expect("open the scratch file for the holder");
        Self::spawn(Command::new("sleep").arg("60").stdin(Stdio::from(file)))
    }

    fn in_directory(path: &Path) -> Self {
        Self::spawn(Command::new("sleep").arg("60").current_dir(path))
    }

    fn spawn(command: &mut Command) -> Self {
        let child = command
            .spawn()
            .expect("spawn sleep; it stands in for a process that holds the path");
        Self { child }
    }
}

impl Drop for Holder {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

#[test]
fn an_unheld_lock_and_companion_directory_are_dead() {
    let lock = Scratch::file("dead-lock");
    let dir = Scratch::directory("dead-dir");

    assert_eq!(
        UnixHolderProbe::new().holder(&lock.path, &dir.path),
        HolderCheck::Dead
    );
}

#[test]
fn a_process_holding_the_lock_file_is_alive_until_it_lets_go() {
    let lock = Scratch::file("held-lock");
    let dir = Scratch::directory("held-lock-dir");
    let probe = UnixHolderProbe::new();
    assert_eq!(probe.holder(&lock.path, &dir.path), HolderCheck::Dead);

    let holder = Holder::through_file(&lock.path);
    assert_eq!(probe.holder(&lock.path, &dir.path), HolderCheck::Alive);

    drop(holder);
    assert_eq!(probe.holder(&lock.path, &dir.path), HolderCheck::Dead);
}

#[test]
fn a_process_in_the_companion_directory_is_alive() {
    let lock = Scratch::file("dir-lock");
    let dir = Scratch::directory("dir-holder");
    let _holder = Holder::in_directory(&dir.path);

    assert_eq!(
        UnixHolderProbe::new().holder(&lock.path, &dir.path),
        HolderCheck::Alive
    );
}

#[test]
fn a_missing_lock_path_is_unknown_not_dead() {
    let dir = Scratch::directory("missing-lock-dir");
    let missing = dir.path.join("no-such-lock");

    assert_eq!(
        UnixHolderProbe::new().holder(&missing, &dir.path),
        HolderCheck::Unknown
    );
}

#[test]
fn a_missing_companion_directory_is_unknown_not_dead() {
    let lock = Scratch::file("missing-dir-lock");
    let missing =
        std::env::temp_dir().join(format!("mcao-holder-{}-no-such-dir", std::process::id()));

    assert_eq!(
        UnixHolderProbe::new().holder(&lock.path, &missing),
        HolderCheck::Unknown
    );
}

#[test]
fn a_path_that_lsof_rejects_is_unknown_not_dead() {
    let dir = Scratch::directory("rejected-dir");
    // A leading `-` would read as an option if `--` did not end option parsing. Either way lsof
    // rejects the name, and the probe must not read that rejection as "no holder".
    let rejected = PathBuf::from(format!("-mcao-holder-{}-nope", std::process::id()));

    assert_eq!(
        UnixHolderProbe::new().holder(&rejected, &dir.path),
        HolderCheck::Unknown
    );
}

#[test]
fn a_missing_lsof_is_unknown_and_does_not_crash() {
    let lock = Scratch::file("unknown");
    let dir = Scratch::directory("unknown-dir");
    let probe = UnixHolderProbe::with_program(OsStr::new("/nonexistent/mcao-lsof"));

    assert_eq!(probe.holder(&lock.path, &dir.path), HolderCheck::Unknown);
}
