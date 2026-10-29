//! Holder probes: what a platform can prove about the process that holds a lock.

use std::path::Path;

/// What a holder probe can establish about a lock.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum HolderCheck {
    /// A live process holds the lock or its companion directory.
    Alive,
    /// The probe proves that no process holds the lock or its companion directory.
    Dead,
    /// The probe cannot tell, for example because no holder probe exists on this host.
    Unknown,
}

/// Reports whether a live process holds a lock.
///
/// `lock` is the lock file and `dir` is its companion directory. An implementation answers
/// [`HolderCheck::Dead`] only with proof that nothing holds either path. Any doubt is
/// [`HolderCheck::Unknown`].
pub trait HolderProbe {
    /// Checks the holder of `lock` and of its companion `dir`.
    fn holder(&self, lock: &Path, dir: &Path) -> HolderCheck;
}

/// The probe for a host with no native holder probe, such as Windows until one is proven.
///
/// It answers [`HolderCheck::Unknown`] for every lock, so no lock is proven stale through it.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct UnknownHolderProbe;

impl HolderProbe for UnknownHolderProbe {
    fn holder(&self, _lock: &Path, _dir: &Path) -> HolderCheck {
        HolderCheck::Unknown
    }
}
