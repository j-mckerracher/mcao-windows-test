//! Steering layer for interactive console slots and frame dispatch (DPES-885).
//!
//! Provides the single-writer interactive steering slot, the per-client frame decision table
//! over the canonical [`crate::console::frame`] codec (INPUT, RESIZE, SUBSCR, DETACH), and Unix
//! attach argv construction. Task ids and backend names reuse [`TaskId`] and [`BackendToken`],
//! so the shell contract's rules live in one place.

use std::sync::{Mutex, MutexGuard};

use crate::console::frame::{Frame, FrameError, TAG_DETACH, TAG_RESIZE, TAG_SUBSCRIBE};
use crate::error::McaoError;
use crate::state::{BackendToken, TaskId};

/// Message returned when an interactive slot is already occupied by another client.
pub const MSG_SLOT_HELD: &str = "interactive slot held";

/// Message returned when a subscription frame payload length is invalid.
pub const MSG_BAD_SUBSCRIBE: &str = "bad SUBSCR";

/// Message returned when a resize frame payload length is invalid.
pub const MSG_BAD_RESIZE: &str = "bad RESIZE";

/// Message returned for unrecognized inbound frame tags.
pub const MSG_UNKNOWN: &str = "unknown";

/// Message indicating a ConPTY subsystem error.
pub const MSG_CONPTY: &str = "ConPTY error";

/// Single-writer interactive slot tracking which connection currently steers the console.
#[derive(Debug, Default)]
pub struct InteractiveSlot {
    holder: Mutex<Option<u32>>,
}

impl InteractiveSlot {
    /// Creates a new, unoccupied interactive slot.
    pub fn new() -> Self {
        Self {
            holder: Mutex::new(None),
        }
    }

    /// Locks the holder. A poisoned lock panics and its contents are never read.
    ///
    /// The critical sections here cannot panic, so poison means another thread broke an
    /// invariant, and recovering the guard would let a slot admit writers after that failure.
    fn lock(&self) -> MutexGuard<'_, Option<u32>> {
        self.holder
            .lock()
            .expect("interactive slot lock is not poisoned")
    }

    /// Claims the interactive steering slot for `connection_id`.
    ///
    /// Returns `true` if the slot was free or already held by this connection id.
    /// Returns `false` if another connection currently holds the slot.
    pub fn claim(&self, connection_id: u32) -> bool {
        let mut guard = self.lock();
        match *guard {
            None => {
                *guard = Some(connection_id);
                true
            }
            Some(id) if id == connection_id => true,
            Some(_) => false,
        }
    }

    /// Releases the interactive steering slot if held by `connection_id`.
    ///
    /// Returns `true` only if this connection id was the current holder.
    pub fn release(&self, connection_id: u32) -> bool {
        let mut guard = self.lock();
        if *guard == Some(connection_id) {
            *guard = None;
            true
        } else {
            false
        }
    }

    /// Returns the connection identifier of the current slot holder, if any.
    pub fn holder(&self) -> Option<u32> {
        *self.lock()
    }
}

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

/// Per-client steering state tracking identity and interactive privileges.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ClientState {
    /// Numeric identifier for this client connection.
    pub connection_id: u32,
    /// Whether this client currently holds interactive steering permissions.
    pub interactive: bool,
}

impl ClientState {
    /// Creates a new client state with `interactive` set to `false`.
    pub fn new(connection_id: u32) -> Self {
        Self {
            connection_id,
            interactive: false,
        }
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

/// Evaluates a decoded inbound client frame against the client state and interactive slot.
///
/// Frames arrive from [`crate::console::frame::decode_from`]; a frame the codec rejects goes
/// to [`decode_error_action`] instead.
pub fn handle_frame(state: &mut ClientState, slot: &InteractiveSlot, frame: &Frame) -> FrameAction {
    match frame {
        Frame::Subscribe { offset } => FrameAction::Subscribe { offset: *offset },
        Frame::Input(payload) => {
            if payload.is_empty() {
                FrameAction::Ignore
            } else {
                if !state.interactive {
                    if !slot.claim(state.connection_id) {
                        return FrameAction::Reply {
                            message: MSG_SLOT_HELD,
                            close: false,
                        };
                    }
                    state.interactive = true;
                }
                FrameAction::ForwardInput(payload.clone())
            }
        }
        Frame::Resize { cols, rows } => {
            if state.interactive {
                FrameAction::ApplyResize(Resize {
                    cols: *cols,
                    rows: *rows,
                })
            } else {
                FrameAction::Ignore
            }
        }
        Frame::Detach => {
            if state.interactive {
                slot.release(state.connection_id);
                state.interactive = false;
            }
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
/// client must still go through [`finish_client`] to release any slot it held.
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

/// Releases any interactive slot held by `state` when the client disconnects or encounters an error.
///
/// This operation is idempotent: repeated calls on the same state have no effect.
pub fn finish_client(state: &mut ClientState, slot: &InteractiveSlot) {
    if state.interactive {
        slot.release(state.connection_id);
        state.interactive = false;
    }
}

/// Builds the argv vector to attach to a session on a Unix backend.
///
/// Takes a parsed [`TaskId`], so the id rule is [`TaskId::parse`]'s alone. A rejected id is
/// [`McaoError::Usage`] through its `From` impl, which exits with code 2.
///
/// `read_only` requests a view-only attach: tmux gets `-r` before `-t`, and herdr gets
/// `--read-only`. Without it, both argv forms are writable.
///
/// # Errors
///
/// Returns [`McaoError::Unsupported`] if `backend` is [`BackendToken::Windows`].
pub fn unix_attach_argv(
    backend: BackendToken,
    task_id: &TaskId,
    read_only: bool,
) -> Result<Vec<String>, McaoError> {
    let session = format!("fm-{}", task_id.as_str());
    match backend {
        BackendToken::Tmux => {
            let mut argv = vec!["tmux".to_string(), "attach-session".to_string()];
            if read_only {
                argv.push("-r".to_string());
            }
            argv.push("-t".to_string());
            argv.push(session);
            Ok(argv)
        }
        BackendToken::Herdr => {
            let mut argv = vec!["herdr".to_string(), "viewer".to_string(), session];
            if read_only {
                argv.push("--read-only".to_string());
            }
            Ok(argv)
        }
        BackendToken::Windows => Err(McaoError::Unsupported {
            backend: backend.as_str().to_owned(),
            op: "unix attach",
        }),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::console::frame::{encode, error_frame};
    use crate::state::TaskIdError;
    use std::io::Cursor;
    use std::sync::Arc;

    /// Builds a raw wire frame with `tag` and `payload`, bypassing the encoder's checks.
    fn raw_frame(tag: &[u8; 6], payload: &[u8]) -> Vec<u8> {
        let len = u32::try_from(6 + payload.len()).expect("test payload fits");
        let mut bytes = len.to_le_bytes().to_vec();
        bytes.extend_from_slice(tag);
        bytes.extend_from_slice(payload);
        bytes
    }

    /// Runs wire bytes through the codec and the decision table, as a server read loop will.
    fn dispatch(state: &mut ClientState, slot: &InteractiveSlot, wire: &[u8]) -> FrameAction {
        match crate::console::frame::decode_from(&mut Cursor::new(wire)) {
            Ok(Some(frame)) => handle_frame(state, slot, &frame),
            Ok(None) => panic!("expected a frame on the wire"),
            Err(error) => decode_error_action(&error),
        }
    }

    fn input_wire(bytes: &[u8]) -> Vec<u8> {
        encode(&Frame::Input(bytes.to_vec())).expect("input encodes")
    }

    fn detach_wire() -> Vec<u8> {
        encode(&Frame::Detach).expect("detach encodes")
    }

    #[test]
    fn test_slot_claim_and_release() {
        let slot = InteractiveSlot::new();
        assert_eq!(slot.holder(), None);

        // claim by A succeeds
        assert!(slot.claim(1));
        assert_eq!(slot.holder(), Some(1));

        // claim by B fails
        assert!(!slot.claim(2));
        assert_eq!(slot.holder(), Some(1));

        // claim by A again succeeds
        assert!(slot.claim(1));
        assert_eq!(slot.holder(), Some(1));

        // release by B returns false and A still holds
        assert!(!slot.release(2));
        assert_eq!(slot.holder(), Some(1));

        // release by A returns true and holder is None
        assert!(slot.release(1));
        assert_eq!(slot.holder(), None);

        // B then claims
        assert!(slot.claim(2));
        assert_eq!(slot.holder(), Some(2));
    }

    #[test]
    #[should_panic(expected = "interactive slot lock is not poisoned")]
    fn test_slot_poisoned_lock_panics() {
        let slot = Arc::new(InteractiveSlot::new());
        let poisoner = Arc::clone(&slot);
        let joined = std::thread::spawn(move || {
            let _guard = poisoner.holder.lock().expect("fresh lock");
            panic!("poisoning mutex");
        })
        .join();
        assert!(joined.is_err());

        // Poisoned: the holder is never read, so this panics.
        slot.holder();
    }

    #[test]
    fn test_to_console_size() {
        assert_eq!(
            Resize { cols: 80, rows: 24 }.to_console_size(),
            Some((80, 24))
        );
        assert_eq!(Resize { cols: 0, rows: 24 }.to_console_size(), None);
        assert_eq!(Resize { cols: 80, rows: 0 }.to_console_size(), None);
        assert_eq!(
            Resize {
                cols: 40000,
                rows: 50000,
            }
            .to_console_size(),
            Some((32767, 32767))
        );
    }

    #[test]
    fn test_wire_subscr() {
        let mut state = ClientState::new(1);
        let slot = InteractiveSlot::new();

        // SUBSCR good (offset 7)
        let good = raw_frame(b"SUBSCR", &7u64.to_le_bytes());
        assert_eq!(
            dispatch(&mut state, &slot, &good),
            FrameAction::Subscribe { offset: 7 }
        );

        // SUBSCR bad length replies and closes
        let bad = raw_frame(b"SUBSCR", &[1, 2, 3]);
        assert_eq!(
            dispatch(&mut state, &slot, &bad),
            FrameAction::Reply {
                message: MSG_BAD_SUBSCRIBE,
                close: true,
            }
        );
    }

    #[test]
    fn test_wire_input_contention_and_detach() {
        let slot = InteractiveSlot::new();
        let mut state_a = ClientState::new(10);
        let mut state_b = ClientState::new(20);

        // AC1: first INPUT claims and forwards
        assert_eq!(
            dispatch(&mut state_a, &slot, &input_wire(b"hello")),
            FrameAction::ForwardInput(b"hello".to_vec())
        );
        assert!(state_a.interactive);
        assert_eq!(slot.holder(), Some(10));

        // AC2: second client INPUT gets the held reply and nothing is forwarded
        assert_eq!(
            dispatch(&mut state_b, &slot, &input_wire(b"world")),
            FrameAction::Reply {
                message: MSG_SLOT_HELD,
                close: false,
            }
        );
        assert!(!state_b.interactive);
        assert_eq!(slot.holder(), Some(10));

        // AC3: DETACH by the first releases the slot and closes
        assert_eq!(
            dispatch(&mut state_a, &slot, &detach_wire()),
            FrameAction::Close
        );
        assert!(!state_a.interactive);
        assert_eq!(slot.holder(), None);

        // AC4: second INPUT claims and forwards
        assert_eq!(
            dispatch(&mut state_b, &slot, &input_wire(b"world")),
            FrameAction::ForwardInput(b"world".to_vec())
        );
        assert!(state_b.interactive);
        assert_eq!(slot.holder(), Some(20));
    }

    #[test]
    fn test_wire_empty_input_is_ignored_without_claim() {
        let slot = InteractiveSlot::new();
        let mut state = ClientState::new(1);

        assert_eq!(
            dispatch(&mut state, &slot, &input_wire(b"")),
            FrameAction::Ignore
        );
        assert!(!state.interactive);
        assert_eq!(slot.holder(), None);
    }

    #[test]
    fn test_input_holder_no_reclaim() {
        let slot = InteractiveSlot::new();
        let mut state = ClientState::new(1);

        assert_eq!(
            dispatch(&mut state, &slot, &input_wire(b"first")),
            FrameAction::ForwardInput(b"first".to_vec())
        );
        assert!(state.interactive);

        // INPUT again by holder forwards without re-claim
        assert_eq!(
            dispatch(&mut state, &slot, &input_wire(b"second")),
            FrameAction::ForwardInput(b"second".to_vec())
        );
        assert!(state.interactive);
    }

    #[test]
    fn test_wire_resize() {
        let slot = InteractiveSlot::new();
        let mut state = ClientState::new(1);
        let good = raw_frame(b"RESIZE", &[80, 0, 24, 0]);
        let bad = raw_frame(b"RESIZE", &[80, 0, 24]);

        // RESIZE from non-interactive is ignored
        assert_eq!(dispatch(&mut state, &slot, &good), FrameAction::Ignore);

        // Claim and become interactive
        assert!(slot.claim(1));
        state.interactive = true;

        // RESIZE good from interactive applies
        assert_eq!(
            dispatch(&mut state, &slot, &good),
            FrameAction::ApplyResize(Resize { cols: 80, rows: 24 })
        );

        // RESIZE bad length replies and closes, interactive or not
        let reply = FrameAction::Reply {
            message: MSG_BAD_RESIZE,
            close: true,
        };
        assert_eq!(dispatch(&mut state, &slot, &bad), reply);
        state.interactive = false;
        assert_eq!(dispatch(&mut state, &slot, &bad), reply);
    }

    #[test]
    fn test_detach_non_holder_does_not_release() {
        let slot = InteractiveSlot::new();
        let mut state_a = ClientState::new(1);
        let mut state_b = ClientState::new(2);

        assert_eq!(
            dispatch(&mut state_a, &slot, &input_wire(b"data")),
            FrameAction::ForwardInput(b"data".to_vec())
        );
        assert_eq!(slot.holder(), Some(1));

        // DETACH by non-holder closes but leaves the holder in place
        assert_eq!(
            dispatch(&mut state_b, &slot, &detach_wire()),
            FrameAction::Close
        );
        assert_eq!(slot.holder(), Some(1));
    }

    #[test]
    fn test_wire_detach_with_payload_closes_and_finish_releases() {
        let slot = InteractiveSlot::new();
        let mut state = ClientState::new(1);
        assert_eq!(
            dispatch(&mut state, &slot, &input_wire(b"k")),
            FrameAction::ForwardInput(b"k".to_vec())
        );

        // Malformed DETACH closes without a reply; the server's disconnect path releases.
        let malformed = raw_frame(b"DETACH", b"x");
        assert_eq!(dispatch(&mut state, &slot, &malformed), FrameAction::Close);
        assert_eq!(slot.holder(), Some(1));
        finish_client(&mut state, &slot);
        assert_eq!(slot.holder(), None);
        assert!(!state.interactive);
    }

    #[test]
    fn test_wire_unknown_and_output_tags_reply_unknown() {
        let slot = InteractiveSlot::new();
        let mut state = ClientState::new(1);
        let unknown = FrameAction::Reply {
            message: MSG_UNKNOWN,
            close: false,
        };

        assert_eq!(
            dispatch(&mut state, &slot, &raw_frame(b"OUTPUT", b"out")),
            unknown
        );
        let error_wire = encode(&error_frame("err")).expect("error encodes");
        assert_eq!(dispatch(&mut state, &slot, &error_wire), unknown);
        assert_eq!(
            dispatch(&mut state, &slot, &raw_frame(b"XYZ123", b"")),
            unknown
        );
    }

    #[test]
    fn test_finish_client() {
        let slot = InteractiveSlot::new();
        let mut state = ClientState::new(1);

        // finish_client when non-interactive is a no-op
        finish_client(&mut state, &slot);
        assert_eq!(slot.holder(), None);
        assert!(!state.interactive);

        // interactive client
        assert!(slot.claim(1));
        state.interactive = true;

        // finish_client releases once and is idempotent
        finish_client(&mut state, &slot);
        assert_eq!(slot.holder(), None);
        assert!(!state.interactive);

        // second call is idempotent
        finish_client(&mut state, &slot);
        assert_eq!(slot.holder(), None);
        assert!(!state.interactive);
    }

    #[test]
    fn test_unix_attach_argv() {
        let task = TaskId::parse("task-1").expect("valid id");

        // tmux
        let tmux_argv = unix_attach_argv(BackendToken::Tmux, &task, false).expect("tmux argv");
        assert_eq!(tmux_argv, vec!["tmux", "attach-session", "-t", "fm-task-1"]);

        // tmux read-only attaches with -r before -t
        let tmux_ro = unix_attach_argv(BackendToken::Tmux, &task, true).expect("tmux argv");
        assert_eq!(
            tmux_ro,
            vec!["tmux", "attach-session", "-r", "-t", "fm-task-1"]
        );

        // herdr
        let herdr_argv = unix_attach_argv(BackendToken::Herdr, &task, false).expect("herdr argv");
        assert_eq!(herdr_argv, vec!["herdr", "viewer", "fm-task-1"]);

        // herdr read-only
        let herdr_ro = unix_attach_argv(BackendToken::Herdr, &task, true).expect("herdr argv");
        assert_eq!(
            herdr_ro,
            vec!["herdr", "viewer", "fm-task-1", "--read-only"]
        );

        // underscore ids pass the shared rule and reach the argv
        let underscored = TaskId::parse("a_b").expect("valid id");
        let argv = unix_attach_argv(BackendToken::Tmux, &underscored, false).expect("argv");
        assert_eq!(argv.last().map(String::as_str), Some("fm-a_b"));

        // windows has no unix attach argv
        let err = unix_attach_argv(BackendToken::Windows, &task, false)
            .expect_err("windows has no unix attach");
        assert_eq!(err.exit_code(), 7);
    }

    #[test]
    fn test_task_id_rule_comes_from_task_id() {
        assert_eq!(TaskId::parse(""), Err(TaskIdError::Empty));
        assert_eq!(TaskId::parse(".x"), Err(TaskIdError::LeadingDot));
        assert!(matches!(
            TaskId::parse("a b"),
            Err(TaskIdError::InvalidChar { ch: ' ' })
        ));
        assert!(matches!(
            TaskId::parse("a/b"),
            Err(TaskIdError::InvalidChar { ch: '/' })
        ));
        assert_eq!(
            McaoError::from(TaskIdError::Empty).exit_code(),
            2,
            "a bad id is a usage error"
        );
    }

    #[test]
    fn test_msg_conpty_constant() {
        assert_eq!(MSG_CONPTY, "ConPTY error");
    }
}
