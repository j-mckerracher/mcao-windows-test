//! Stale-lock proofs: a lock is removable only when a holder probe proves it dead and it is old
//! enough. Every other case must stay in place.

use std::cell::RefCell;
use std::env;
use std::fs;
use std::fs::{File, OpenOptions};
use std::path::{Path, PathBuf};
use std::process;
use std::rc::Rc;
use std::time::{Duration, SystemTime};

use mcao_core::lock::{
    HolderCheck, HolderProbe, LockProof, LockVerdict, Uncertainty, UnknownHolderProbe,
};

/// A probe that gives one fixed answer, standing in for a platform probe.
#[derive(Clone, Copy)]
struct FakeProbe(HolderCheck);

impl HolderProbe for FakeProbe {
    fn holder(&self, _lock: &Path, _dir: &Path) -> HolderCheck {
        self.0
    }
}

/// A probe that records each `(lock, dir)` it is asked about and answers `Dead`.
#[derive(Clone, Default)]
struct RecordingProbe {
    asked: Rc<RefCell<Vec<(PathBuf, PathBuf)>>>,
}

impl RecordingProbe {
    /// The `(lock, dir)` pairs the probe was asked about, in order.
    fn asked(&self) -> Vec<(PathBuf, PathBuf)> {
        self.asked.borrow().clone()
    }
}

impl HolderProbe for RecordingProbe {
    fn holder(&self, lock: &Path, dir: &Path) -> HolderCheck {
        self.asked
            .borrow_mut()
            .push((lock.to_path_buf(), dir.to_path_buf()));
        HolderCheck::Dead
    }
}

/// A probe that refreshes the lock's mtime to now while it answers, as a new owner would.
struct RefreshingProbe;

impl HolderProbe for RefreshingProbe {
    fn holder(&self, lock: &Path, _dir: &Path) -> HolderCheck {
        let file = OpenOptions::new()
            .write(true)
            .open(lock)
            .expect("open lock to refresh it");
        file.set_modified(SystemTime::now())
            .expect("refresh lock mtime");
        HolderCheck::Dead
    }
}

/// A scratch directory under the system temp dir, removed when the test ends.
struct Scratch(PathBuf);

impl Scratch {
    /// Makes an empty scratch directory for `test`, unique to this process.
    fn new(test: &str) -> Self {
        let dir = env::temp_dir().join(format!("mcao-lock-proof-{test}-{}", process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).expect("create scratch dir");
        Self(dir)
    }
}

impl Drop for Scratch {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

/// Creates a lock file in `scratch` whose modification time is `modified`.
fn lock_modified_at(scratch: &Scratch, modified: SystemTime) -> PathBuf {
    let lock = scratch.0.join("state.lock");
    let file = File::create(&lock).expect("create lock file");
    file.set_modified(modified).expect("set lock mtime");
    lock
}

/// Creates a lock file in `scratch` that was last modified `age` ago.
fn lock_aged(scratch: &Scratch, age: Duration) -> PathBuf {
    lock_modified_at(scratch, SystemTime::now() - age)
}

#[test]
fn host_without_holder_probe_is_not_provably_stale() {
    // DPES-753 AC5: a Windows host with no holder probe returns HolderUnknown.
    let scratch = Scratch::new("no-probe");
    let lock = lock_aged(&scratch, Duration::from_secs(600));
    let proof = LockProof::new(UnknownHolderProbe);

    let verdict = proof.is_provably_stale(&lock, &scratch.0, Duration::from_secs(60));

    assert_eq!(
        verdict,
        LockVerdict::NotProvablyStale(Uncertainty::HolderUnknown),
        "an old lock with no holder probe must stay in place"
    );
}

#[test]
fn live_holder_is_never_provably_stale() {
    let scratch = Scratch::new("live");
    let lock = lock_aged(&scratch, Duration::from_secs(600));
    let proof = LockProof::new(FakeProbe(HolderCheck::Alive));

    let verdict = proof.is_provably_stale(&lock, &scratch.0, Duration::from_secs(60));

    assert_eq!(
        verdict,
        LockVerdict::NotProvablyStale(Uncertainty::LiveHolder),
        "a lock with a live holder must never be provably stale, even when old"
    );
}

#[test]
fn dead_holder_past_min_age_is_provably_stale() {
    let scratch = Scratch::new("dead");
    let lock = lock_aged(&scratch, Duration::from_secs(600));
    let proof = LockProof::new(FakeProbe(HolderCheck::Dead));

    let verdict = proof.is_provably_stale(&lock, &scratch.0, Duration::from_secs(60));

    assert_eq!(
        verdict,
        LockVerdict::ProvablyStale,
        "a dead holder on an old lock is provably stale"
    );
}

#[test]
fn dead_holder_younger_than_min_age_is_not_provably_stale() {
    let scratch = Scratch::new("young");
    let lock = lock_aged(&scratch, Duration::from_secs(10));
    let proof = LockProof::new(FakeProbe(HolderCheck::Dead));

    let verdict = proof.is_provably_stale(&lock, &scratch.0, Duration::from_secs(600));

    assert!(
        matches!(
            verdict,
            LockVerdict::NotProvablyStale(Uncertainty::TooYoung { .. })
        ),
        "a lock younger than the minimum age must stay in place, got {verdict:?}"
    );
}

#[test]
fn unknown_probe_answer_blocks_the_proof_when_old() {
    let scratch = Scratch::new("unknown");
    let lock = lock_aged(&scratch, Duration::from_secs(600));
    let proof = LockProof::new(FakeProbe(HolderCheck::Unknown));

    let verdict = proof.is_provably_stale(&lock, &scratch.0, Duration::from_secs(60));

    assert_eq!(
        verdict,
        LockVerdict::NotProvablyStale(Uncertainty::HolderUnknown),
        "an unknown holder must block the proof even when the lock is old"
    );
}

#[test]
fn missing_lock_is_not_provably_stale() {
    let scratch = Scratch::new("missing");
    let lock = scratch.0.join("state.lock");
    let proof = LockProof::new(FakeProbe(HolderCheck::Dead));

    let verdict = proof.is_provably_stale(&lock, &scratch.0, Duration::from_secs(60));

    assert_eq!(
        verdict,
        LockVerdict::NotProvablyStale(Uncertainty::Missing),
        "a lock that does not exist cannot be proven stale"
    );
}

#[test]
fn future_mtime_is_not_provably_stale() {
    let scratch = Scratch::new("future");
    let lock = lock_modified_at(&scratch, SystemTime::now() + Duration::from_secs(3600));
    let proof = LockProof::new(FakeProbe(HolderCheck::Dead));

    let verdict = proof.is_provably_stale(&lock, &scratch.0, Duration::from_secs(60));

    assert_eq!(
        verdict,
        LockVerdict::NotProvablyStale(Uncertainty::AgeUnreadable),
        "a lock whose age cannot be computed must stay in place"
    );
}

#[test]
fn lock_refreshed_during_probe_is_not_provably_stale() {
    // Review finding 1: the age is read after the probe, so a refresh during the probe reads young.
    let scratch = Scratch::new("refreshed");
    let lock = lock_aged(&scratch, Duration::from_secs(600));
    let proof = LockProof::new(RefreshingProbe);

    let verdict = proof.is_provably_stale(&lock, &scratch.0, Duration::from_secs(60));

    assert!(
        matches!(
            verdict,
            LockVerdict::NotProvablyStale(Uncertainty::TooYoung { .. })
        ),
        "a lock refreshed while the probe runs must stay in place, got {verdict:?}"
    );
}

#[test]
fn probe_is_asked_about_the_lock_and_its_companion_dir() {
    // Review finding 2: the probe must receive the lock file and the companion directory.
    let scratch = Scratch::new("asked");
    let lock = lock_aged(&scratch, Duration::from_secs(600));
    let dir = scratch.0.join("companion");
    let probe = RecordingProbe::default();
    let proof = LockProof::new(probe.clone());

    proof.is_provably_stale(&lock, &dir, Duration::from_secs(60));

    assert_eq!(
        probe.asked(),
        vec![(lock, dir)],
        "the probe must be asked once, about the lock file and its companion directory"
    );
}

#[test]
fn probe_is_not_asked_when_the_lock_is_missing() {
    let scratch = Scratch::new("asked-missing");
    let lock = scratch.0.join("state.lock");
    let probe = RecordingProbe::default();
    let proof = LockProof::new(probe.clone());

    proof.is_provably_stale(&lock, &scratch.0, Duration::from_secs(60));

    assert!(
        probe.asked().is_empty(),
        "a missing lock must be rejected before the probe runs"
    );
}
