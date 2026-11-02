//! Lock ownership proofs and holder probes.

mod probe;
mod proof;

pub use probe::{HolderCheck, HolderProbe, UnknownHolderProbe};
pub use proof::{LockProof, LockVerdict, Uncertainty};
