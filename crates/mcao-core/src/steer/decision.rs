//! The per-connection frame decision table (DPES-885, DPES-953, DPES-962 AC2, DPES-979 F1).

use super::slot::{Claim, InteractiveSlot, Resize};
use super::{MSG_BAD_RESIZE, MSG_BAD_SUBSCRIBE, MSG_OTHER_SLOT, MSG_SLOT_HELD, MSG_UNKNOWN};
use crate::console::frame::{Frame, FrameError, TAG_DETACH, TAG_RESIZE, TAG_SUBSCRIBE};

/// Per-connection steering state: the connection id, the claim this connection made and the slot
/// that issued it, and any size it sent while it did not hold the slot.
///
/// The state is bound to the slot that issued its claim (DPES-979 F1). Frames for any other slot
/// are refused and change nothing, and [`finish_client`] releases the claim on that slot without
/// the caller naming it. Dropping the state calls [`finish_client`], so a connection that ends
/// releases its claim even when no caller reached `finish_client`.
///
/// Not `Clone`: a copy would carry the same claim to a second owner (see the module docs).
#[derive(Debug, PartialEq, Eq)]
pub struct ClientState {
    /// Numeric identifier for this connection. Ids may repeat; the claim decides ownership.
    pub connection_id: u32,
    pub(super) claim: Option<Claim>,
    /// The slot that issued `claim`. Present exactly while `claim` is.
    pub(super) slot: Option<InteractiveSlot>,
    /// Latest valid size sent while this connection did not hold the slot.
    pub(super) pending_resize: Option<Resize>,
}

impl ClientState {
    /// Creates a new connection state that holds no claim and no pending size.
    pub fn new(connection_id: u32) -> Self {
        Self {
            connection_id,
            claim: None,
            slot: None,
            pending_resize: None,
        }
    }
}

impl Drop for ClientState {
    fn drop(&mut self) {
        finish_client(self);
    }
}

/// Action determined for an inbound client frame.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum FrameAction {
    /// Subscribe to console output starting from the given byte offset.
    Subscribe {
        /// Byte offset to stream from.
        offset: u64,
    },
    /// Forward terminal input bytes to the worker console.
    ForwardInput(Vec<u8>),
    /// This frame claimed the slot and a size was pending: apply `resize`, then forward `input`.
    ApplyResizeThenForward {
        /// The size the connection sent before it held the slot.
        resize: Resize,
        /// The input bytes that claimed the slot.
        input: Vec<u8>,
    },
    /// Apply a new terminal window size.
    ApplyResize(Resize),
    /// Reply to the client with an informational or error message.
    Reply {
        /// Static message text describing the condition.
        message: &'static str,
        /// Whether to close the client connection after replying.
        close: bool,
    },
    /// Close the client connection without an error message.
    Close,
    /// Ignore the frame without any side effect or reply.
    Ignore,
}

/// Returns whether this connection's claim was issued by a slot other than `slot`.
///
/// Such a claim is neither tested against `slot` nor dropped here: its own slot releases it.
fn bound_elsewhere(state: &ClientState, slot: &InteractiveSlot) -> bool {
    state.claim.is_some_and(|claim| claim.slot_id != slot.id())
}

/// Returns this connection's claim while the slot that issued it still honours it.
///
/// Liveness compares the whole claim, id and epoch, through [`InteractiveSlot::is_current`]. A
/// stale claim is dropped with its binding, so this connection stops treating itself as the
/// writer. Callers check [`bound_elsewhere`] first, so `slot` is the issuing slot.
fn live_claim(state: &mut ClientState, slot: &InteractiveSlot) -> Option<Claim> {
    let claim = state.claim?;
    if slot.is_current(claim) {
        Some(claim)
    } else {
        state.claim = None;
        state.slot = None;
        None
    }
}

/// Evaluates a decoded inbound client frame against the connection state and interactive slot.
///
/// Frames arrive from [`crate::console::frame::decode_from`]; a frame the codec rejects goes
/// to [`decode_error_action`] instead.
pub fn handle_frame(state: &mut ClientState, slot: &InteractiveSlot, frame: &Frame) -> FrameAction {
    match frame {
        Frame::Subscribe { offset } => FrameAction::Subscribe { offset: *offset },
        Frame::Input(payload) => {
            if bound_elsewhere(state, slot) {
                return FrameAction::Reply {
                    message: MSG_OTHER_SLOT,
                    close: false,
                };
            }
            if payload.is_empty() {
                return FrameAction::Ignore;
            }
            if live_claim(state, slot).is_none() {
                match slot.claim(state.connection_id) {
                    Some(claim) => {
                        state.claim = Some(claim);
                        state.slot = Some(slot.clone());
                    }
                    None => {
                        return FrameAction::Reply {
                            message: MSG_SLOT_HELD,
                            close: false,
                        };
                    }
                }
                // The size sent before the claim lands before the first input, so the worker
                // sees the attacher's dimensions from the start (DPES-962 AC2 option c).
                if let Some(resize) = state.pending_resize.take() {
                    return FrameAction::ApplyResizeThenForward {
                        resize,
                        input: payload.clone(),
                    };
                }
            }
            FrameAction::ForwardInput(payload.clone())
        }
        Frame::Resize { cols, rows } => {
            let size = Resize {
                cols: *cols,
                rows: *rows,
            };
            // A zero dimension never reaches the backend or the pending slot (DPES-953 item a).
            if size.to_console_size().is_none() {
                return FrameAction::Ignore;
            }
            // A size for another slot is not this connection's to hold or apply (DPES-979 F1).
            if bound_elsewhere(state, slot) {
                return FrameAction::Ignore;
            }
            if live_claim(state, slot).is_some() {
                FrameAction::ApplyResize(size)
            } else {
                state.pending_resize = Some(size);
                FrameAction::Ignore
            }
        }
        Frame::Detach => {
            // A DETACH for another slot is refused and the connection stays open: closing here
            // would leave the issuing slot's claim with a connection that no longer exists.
            if bound_elsewhere(state, slot) {
                return FrameAction::Reply {
                    message: MSG_OTHER_SLOT,
                    close: false,
                };
            }
            finish_client(state);
            FrameAction::Close
        }
        Frame::Output(_) | Frame::Error(_) => FrameAction::Reply {
            message: MSG_UNKNOWN,
            close: false,
        },
    }
}

/// Maps a frame the codec rejected to the action the steering layer owes the client.
///
/// Malformed SUBSCR and RESIZE payloads reply with their message and close. A malformed
/// DETACH closes without a reply. Unknown tags and other malformed payloads reply `unknown`
/// and stay open. Framing failures close, because the stream cannot resynchronize. A closed
/// connection releases any slot it held once its state is dropped or passed to [`finish_client`].
pub fn decode_error_action(error: &FrameError) -> FrameAction {
    match error {
        FrameError::BadPayload { tag, .. } if *tag == TAG_SUBSCRIBE => FrameAction::Reply {
            message: MSG_BAD_SUBSCRIBE,
            close: true,
        },
        FrameError::BadPayload { tag, .. } if *tag == TAG_RESIZE => FrameAction::Reply {
            message: MSG_BAD_RESIZE,
            close: true,
        },
        FrameError::BadPayload { tag, .. } if *tag == TAG_DETACH => FrameAction::Close,
        FrameError::UnknownTag(_) | FrameError::BadPayload { .. } => FrameAction::Reply {
            message: MSG_UNKNOWN,
            close: false,
        },
        FrameError::Io(_) | FrameError::Oversize { .. } | FrameError::Truncated => {
            FrameAction::Close
        }
    }
}

/// Releases the claim and any pending size held by `state` when its connection ends.
///
/// The claim is released on the slot that issued it, which the state remembers, so no caller can
/// release a different slot by mistake (DPES-979 F1). [`handle_frame`] and [`decode_error_action`]
/// return `Close` without releasing, so the caller releases through this call or by dropping the
/// state. The call is idempotent. A stale claim releases nothing, and the binding is cleared
/// either way.
///
/// A state that stays alive after its connection ends keeps its claim: there is no lease or
/// timeout.
///
/// Never panics, including when the slot lock is poisoned, because this runs from `Drop` (see
/// [`InteractiveSlot::release`]).
pub fn finish_client(state: &mut ClientState) {
    state.pending_resize = None;
    let claim = state.claim.take();
    let slot = state.slot.take();
    if let (Some(claim), Some(slot)) = (claim, slot) {
        // `false` means the claim was already superseded. A stale claim has nothing to release,
        // so the result is deliberately not an error.
        let _released = slot.release(claim);
    }
}
