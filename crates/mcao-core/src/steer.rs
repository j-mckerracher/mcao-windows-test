//! Steering layer for interactive console slots and frame dispatch (DPES-885).
//!
//! Split by responsibility: the single-writer slot and its claims live in `slot`, the
//! per-connection frame decision table in `decision`, and the Unix attach argv in `attach`.
//! Task ids reuse [`crate::state::TaskId`], so the shell contract's id rule lives in one place.
//!
//! # Ownership
//!
//! The slot issues a [`Claim`] with a fresh epoch on every successful claim. Ownership is the
//! claim, never the connection id: connection ids may repeat, so a connection that reuses an id
//! cannot write while another claim holds the slot, and a stale claim stops matching as soon as
//! the slot is released or re-claimed. Liveness compares the full claim (DPES-953 item b).
//!
//! # Read-only
//!
//! The steering layer has no view-only mode. Its frames carry no read-only flag, so any
//! connection that sends INPUT can claim the slot. Read-only attach is enforced only by tmux for
//! a direct `tmux attach-session -r` (see [`AttachMode`]). Wiring must refuse INPUT from a
//! view-only attacher before it reaches [`handle_frame`].
//!
//! # Wiring preconditions and limitations
//!
//! - Keep exactly one [`ClientState`] per connection and never copy one. A copy carries the
//!   original claim, so it can forward input for as long as that claim is current.
//! - A [`ClientState`] is bound to the slot that issued its claim (DPES-979 F1). INPUT or DETACH
//!   for any other slot replies [`MSG_OTHER_SLOT`] and changes nothing; RESIZE for it is ignored.
//!   [`finish_client`] takes no slot: it releases the claim on the slot that issued it, so the
//!   caller cannot release the wrong one. A stale claim is released as a no-op and its binding is
//!   cleared the same way.
//! - Apply each [`FrameAction`] before the same connection reads its next frame. A
//!   [`FrameAction::ApplyResizeThenForward`] applies its size before its input.
//! - Call [`finish_client`] on every path that closes a connection, or drop its [`ClientState`];
//!   dropping calls `finish_client`. `Close` alone releases nothing while the state is alive,
//!   including after a framing failure. A state that stays alive after its connection ends keeps
//!   its claim, so the issuing slot stays held: there is no lease or timeout. Cross-slot use is
//!   refused, so it cannot add a second held slot to that connection.
//! - Ownership is decided when a frame is handled. The slot cannot recall an action that was
//!   already returned, so input returned before a release can still be written once.
//! - A RESIZE from a connection that does not hold the slot is never applied. Its latest valid
//!   size is kept and applied when that connection claims the slot (DPES-962 AC2, option c).
//!   Zero dimensions are never kept or applied.
//! - Herdr attach is refused, so no Herdr argv is emitted until the pinned CLI is verified
//!   (DPES-962 AC4).

mod attach;
mod decision;
mod slot;

pub use attach::{AttachMode, unix_attach_argv};
pub use decision::{ClientState, FrameAction, decode_error_action, finish_client, handle_frame};
pub use slot::{Claim, InteractiveSlot, Resize};

/// Message returned when an interactive slot is already occupied by another claim.
pub const MSG_SLOT_HELD: &str = "interactive slot held";

/// Message returned when a connection's claim belongs to a different slot than the frame's.
pub const MSG_OTHER_SLOT: &str = "connection holds another slot";

/// Message returned when a subscription frame payload length is invalid.
pub const MSG_BAD_SUBSCRIBE: &str = "bad SUBSCR";

/// Message returned when a resize frame payload length is invalid.
pub const MSG_BAD_RESIZE: &str = "bad RESIZE";

/// Message returned for unrecognized inbound frame tags.
pub const MSG_UNKNOWN: &str = "unknown";

/// Message indicating a ConPTY subsystem error.
pub const MSG_CONPTY: &str = "ConPTY error";

#[cfg(test)]
mod tests;
