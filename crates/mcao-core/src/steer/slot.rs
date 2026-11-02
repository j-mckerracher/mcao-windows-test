//! The single-writer interactive slot and the claims that hold it (DPES-885, DPES-953 item b).

use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};

/// The source of claim epochs for every slot in the process.
///
/// Epochs are unique across slots, not only within one slot. A claim issued by one slot can then
/// never match the holder of another slot, even when both carry the same connection id (DPES-962
/// review F1). `fetch_add` guarantees each value is handed out once.
static NEXT_EPOCH: AtomicU64 = AtomicU64::new(0);

/// The source of slot identities. Each [`InteractiveSlot`] takes one value at construction, so a
/// claim names the slot that issued it (DPES-979 F1). `fetch_add` keeps every identity unique.
static NEXT_SLOT_ID: AtomicU64 = AtomicU64::new(0);

/// Terminal dimensions in columns and rows.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Resize {
    /// Number of horizontal character columns.
    pub cols: u16,
    /// Number of vertical character rows.
    pub rows: u16,
}

impl Resize {
    /// Converts dimensions into signed 16-bit console coordinates.
    ///
    /// Returns `None` if `cols` or `rows` is 0. Clamps values above 32767 to `i16::MAX`.
    pub fn to_console_size(self) -> Option<(i16, i16)> {
        if self.cols == 0 || self.rows == 0 {
            return None;
        }
        let cols = self.cols.min(i16::MAX as u16) as i16;
        let rows = self.rows.min(i16::MAX as u16) as i16;
        Some((cols, rows))
    }
}

/// The identity of one interactive slot claim, issued by [`InteractiveSlot::claim`].
///
/// The epoch is unique across every slot in the process, so a claim that was released or
/// superseded never matches the current holder of any slot, even when another connection carries
/// the same connection id. The slot id names the slot that issued the claim, so a claim is never
/// mistaken for a holder of a different slot.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Claim {
    pub(super) connection_id: u32,
    pub(super) epoch: u64,
    pub(super) slot_id: u64,
}

#[derive(Debug, Default)]
pub(super) struct SlotState {
    pub(super) holder: Option<Claim>,
}

/// The shared body of an [`InteractiveSlot`]: its identity and its holder.
#[derive(Debug)]
pub(super) struct SlotInner {
    pub(super) id: u64,
    pub(super) state: Mutex<SlotState>,
}

/// Single-writer interactive slot that holds at most one live [`Claim`].
///
/// Clones are handles to the same slot. A [`ClientState`](super::ClientState) keeps one clone of
/// the slot that issued its claim, so it can release that claim without the caller naming the
/// slot again (DPES-979 F1).
#[derive(Debug, Clone)]
pub struct InteractiveSlot {
    pub(super) inner: Arc<SlotInner>,
}

impl PartialEq for InteractiveSlot {
    /// Two handles are equal when they name the same slot.
    fn eq(&self, other: &Self) -> bool {
        Arc::ptr_eq(&self.inner, &other.inner)
    }
}

impl Eq for InteractiveSlot {}

impl Default for InteractiveSlot {
    fn default() -> Self {
        Self::new()
    }
}

impl InteractiveSlot {
    /// Creates a new, unoccupied interactive slot with its own identity.
    pub fn new() -> Self {
        Self {
            inner: Arc::new(SlotInner {
                id: NEXT_SLOT_ID.fetch_add(1, Ordering::Relaxed),
                state: Mutex::new(SlotState::default()),
            }),
        }
    }

    /// Returns this slot's identity, the value a claim it issues carries.
    pub(super) fn id(&self) -> u64 {
        self.inner.id
    }

    /// Locks the slot state for a claim path: claim, liveness, and holder reads. A poisoned lock
    /// panics and its contents are never read.
    ///
    /// The critical sections here cannot panic, so poison means another thread broke an
    /// invariant, and recovering the guard would let a slot admit writers after that failure.
    fn lock(&self) -> MutexGuard<'_, SlotState> {
        self.inner
            .state
            .lock()
            .expect("interactive slot lock is not poisoned")
    }

    /// Locks the slot state for release only. A poisoned lock is recovered and never panics.
    ///
    /// Release runs from [`ClientState`](super::ClientState)'s `Drop`, which can run while a panic
    /// unwinds, and a panic there aborts the process (DPES-1004 F5). Recovering is safe here
    /// because release only clears a holder that equals the claim. That frees the slot and admits
    /// no writer, so the claim paths keep the panic policy in [`lock`](Self::lock).
    fn lock_for_release(&self) -> MutexGuard<'_, SlotState> {
        self.inner
            .state
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
    }

    /// Claims the slot for `connection_id` if no claim holds it, and returns the new claim.
    ///
    /// Returns `None` while any claim holds the slot, including a claim made by a connection with
    /// the same id. An id can repeat; a claim cannot.
    ///
    /// # Panics
    ///
    /// Panics if the slot lock is poisoned.
    pub fn claim(&self, connection_id: u32) -> Option<Claim> {
        let mut guard = self.lock();
        if guard.holder.is_some() {
            return None;
        }
        let claim = Claim {
            connection_id,
            epoch: NEXT_EPOCH.fetch_add(1, Ordering::Relaxed),
            slot_id: self.id(),
        };
        guard.holder = Some(claim);
        Some(claim)
    }

    /// Reports whether `claim` is the current holder, comparing both its id and its epoch.
    ///
    /// # Panics
    ///
    /// Panics if the slot lock is poisoned.
    pub fn is_current(&self, claim: Claim) -> bool {
        self.lock().holder == Some(claim)
    }

    /// Releases the slot if `claim` is still the current holder.
    ///
    /// Returns `true` only when this call released the slot. `false` means the claim was already
    /// released or superseded; a stale claim releases nothing, so callers must choose that outcome
    /// explicitly.
    ///
    /// Never panics, even when the slot lock is poisoned: release is reached from `Drop`, so a
    /// panic here could abort the process during unwinding.
    #[must_use = "false means the claim was already superseded; decide explicitly what that means"]
    pub fn release(&self, claim: Claim) -> bool {
        let mut guard = self.lock_for_release();
        if guard.holder == Some(claim) {
            guard.holder = None;
            true
        } else {
            false
        }
    }

    /// Returns the connection identifier of the current holder, if any.
    ///
    /// # Panics
    ///
    /// Panics if the slot lock is poisoned.
    pub fn holder(&self) -> Option<u32> {
        self.lock().holder.map(|claim| claim.connection_id)
    }
}
