//! Stale-lock proofs. A lock may be removed only when every check proves it stale.

use std::fs;
use std::io;
use std::path::Path;
use std::time::{Duration, SystemTime};

use crate::lock::probe::{HolderCheck, HolderProbe};

/// Why a lock is not provably stale. Each reason means the lock stays in place.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[non_exhaustive]
pub enum Uncertainty {
    /// The lock file does not exist.
    Missing,
    /// A live process holds the lock or its companion directory.
    LiveHolder,
    /// The holder probe cannot tell whether a process holds the lock.
    HolderUnknown,
    /// The lock's modification time cannot be read, or lies in the future.
    AgeUnreadable,
    /// The lock is younger than the required minimum age.
    TooYoung {
        /// The lock's age when it was checked.
        age: Duration,
    },
}

/// The outcome of a stale-lock check.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum LockVerdict {
    /// The lock has no live holder and is at least the minimum age, so it is stale.
    ProvablyStale,
    /// The lock might be live, so it must not be removed.
    NotProvablyStale(Uncertainty),
}

/// Proves that a lock is stale, consulting a holder probe for its holder.
#[derive(Clone, Copy, Debug)]
pub struct LockProof<P> {
    probe: P,
}

impl<P: HolderProbe> LockProof<P> {
    /// Creates a proof that asks `probe` who holds each lock.
    pub fn new(probe: P) -> Self {
        Self { probe }
    }

    /// Checks whether `lock`, with companion directory `dir`, is provably stale.
    ///
    /// The verdict is [`LockVerdict::ProvablyStale`] only when the lock exists, the probe reports
    /// [`HolderCheck::Dead`], and the lock's age, read after the probe, is at least `min_age`.
    /// Every other case returns [`LockVerdict::NotProvablyStale`] with the first reason found. The
    /// verdict is a snapshot: a live process may take the lock after it, so the caller must confirm
    /// before removing it.
    pub fn is_provably_stale(&self, lock: &Path, dir: &Path, min_age: Duration) -> LockVerdict {
        // Existence, then holder, then age. A lock refreshed while the probe runs must read as young.
        if let Err(uncertainty) = lock_metadata(lock) {
            return LockVerdict::NotProvablyStale(uncertainty);
        }
        match self.probe.holder(lock, dir) {
            HolderCheck::Alive => LockVerdict::NotProvablyStale(Uncertainty::LiveHolder),
            HolderCheck::Unknown => LockVerdict::NotProvablyStale(Uncertainty::HolderUnknown),
            HolderCheck::Dead => match modified_time(lock) {
                Ok(modified) => match age_since(modified) {
                    Some(age) if age >= min_age => LockVerdict::ProvablyStale,
                    Some(age) => LockVerdict::NotProvablyStale(Uncertainty::TooYoung { age }),
                    None => LockVerdict::NotProvablyStale(Uncertainty::AgeUnreadable),
                },
                Err(uncertainty) => LockVerdict::NotProvablyStale(uncertainty),
            },
        }
    }
}

/// Reads the lock's metadata, or explains why the lock is missing or unreadable.
fn lock_metadata(lock: &Path) -> Result<fs::Metadata, Uncertainty> {
    fs::metadata(lock).map_err(|error| match error.kind() {
        io::ErrorKind::NotFound => Uncertainty::Missing,
        _ => Uncertainty::AgeUnreadable,
    })
}

/// Reads the lock's modification time, or explains why it is missing or unreadable.
fn modified_time(lock: &Path) -> Result<SystemTime, Uncertainty> {
    lock_metadata(lock)?
        .modified()
        .map_err(|_| Uncertainty::AgeUnreadable)
}

/// The time elapsed since `modified`, or `None` when `modified` lies in the future.
fn age_since(modified: SystemTime) -> Option<Duration> {
    SystemTime::now().duration_since(modified).ok()
}
