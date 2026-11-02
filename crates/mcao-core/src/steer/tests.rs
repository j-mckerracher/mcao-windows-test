//! Tests for the steering layer: slot ownership, frame decisions, framing failures, and attach
//! argv (DPES-885, DPES-953, DPES-962).

use super::MSG_CONPTY;
use super::attach::{AttachMode, unix_attach_argv};
use super::decision::{ClientState, FrameAction, decode_error_action, finish_client, handle_frame};
use super::slot::{Claim, InteractiveSlot, Resize};
use crate::console::frame::{Frame, MAX_FRAME_LEN, decode_from, encode, error_frame};
use crate::error::McaoError;
use crate::state::{BackendToken, TaskId, TaskIdError};
use std::io::{self, Cursor, Read};
use std::sync::PoisonError;

/// Builds a raw wire frame with `tag` and `payload`, bypassing the encoder's checks.
fn raw_frame(tag: &[u8; 6], payload: &[u8]) -> Vec<u8> {
    let len = u32::try_from(6 + payload.len()).expect("test payload fits");
    let mut bytes = len.to_le_bytes().to_vec();
    bytes.extend_from_slice(tag);
    bytes.extend_from_slice(payload);
    bytes
}

/// Runs a reader through the codec and the decision table, as a server read loop will.
fn dispatch_reader(
    state: &mut ClientState,
    slot: &InteractiveSlot,
    reader: &mut impl Read,
) -> FrameAction {
    match decode_from(reader) {
        Ok(Some(frame)) => handle_frame(state, slot, &frame),
        Ok(None) => panic!("expected a frame on the wire"),
        Err(error) => decode_error_action(&error),
    }
}

/// Runs wire bytes through the codec and the decision table.
fn dispatch(state: &mut ClientState, slot: &InteractiveSlot, wire: &[u8]) -> FrameAction {
    dispatch_reader(state, slot, &mut Cursor::new(wire))
}

fn input_wire(bytes: &[u8]) -> Vec<u8> {
    encode(&Frame::Input(bytes.to_vec())).expect("input encodes")
}

fn detach_wire() -> Vec<u8> {
    encode(&Frame::Detach).expect("detach encodes")
}

/// A reader whose every read fails with an I/O error.
struct FailingReader;

impl Read for FailingReader {
    fn read(&mut self, _buf: &mut [u8]) -> io::Result<usize> {
        Err(io::Error::new(io::ErrorKind::ConnectionReset, "test reset"))
    }
}

#[test]
fn test_slot_claim_and_release() {
    let slot = InteractiveSlot::new();
    assert_eq!(slot.holder(), None);

    let first = slot.claim(1).expect("a free slot is claimed");
    assert_eq!(slot.holder(), Some(1));

    // Refused while held, including for the same connection id.
    assert_eq!(slot.claim(1), None);
    assert_eq!(slot.claim(2), None);
    assert_eq!(slot.holder(), Some(1));

    // A claim with the right id but the wrong epoch releases nothing.
    let forged = Claim {
        connection_id: 1,
        epoch: first.epoch + 1,
        slot_id: first.slot_id,
    };
    assert!(!slot.release(forged));
    assert_eq!(slot.holder(), Some(1));

    assert!(slot.release(first));
    assert_eq!(slot.holder(), None);

    // A re-claim gets a new epoch, so the released claim no longer matches.
    let second = slot.claim(2).expect("a released slot is claimed");
    assert_ne!(first, second);
    assert!(!slot.is_current(first));
    assert!(slot.is_current(second));
    assert!(!slot.release(first));
    assert_eq!(slot.holder(), Some(2));
}

/// Poisons the slot's lock from a thread that panics while it holds the guard.
fn poison(slot: &InteractiveSlot) {
    let poisoner = slot.clone();
    let joined = std::thread::spawn(move || {
        let _guard = poisoner.inner.state.lock().expect("fresh lock");
        panic!("poisoning mutex");
    })
    .join();
    assert!(joined.is_err());
}

/// Reads the holder past the poison check, so a test can see what a release left behind.
fn raw_holder(slot: &InteractiveSlot) -> Option<Claim> {
    slot.inner
        .state
        .lock()
        .unwrap_or_else(PoisonError::into_inner)
        .holder
}

#[test]
#[should_panic(expected = "interactive slot lock is not poisoned")]
fn test_slot_poisoned_lock_panics() {
    let slot = InteractiveSlot::new();
    poison(&slot);

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

    // SUBSCR bad length replies and closes, with the literal wire text
    let bad = raw_frame(b"SUBSCR", &[1, 2, 3]);
    assert_eq!(
        dispatch(&mut state, &slot, &bad),
        FrameAction::Reply {
            message: "bad SUBSCR",
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
    assert!(state_a.claim.is_some());
    assert_eq!(slot.holder(), Some(10));

    // AC2: second client INPUT gets the held reply and nothing is forwarded
    assert_eq!(
        dispatch(&mut state_b, &slot, &input_wire(b"world")),
        FrameAction::Reply {
            message: "interactive slot held",
            close: false,
        }
    );
    assert!(state_b.claim.is_none());
    assert_eq!(slot.holder(), Some(10));

    // AC3: DETACH by the first releases the slot and closes
    assert_eq!(
        dispatch(&mut state_a, &slot, &detach_wire()),
        FrameAction::Close
    );
    assert!(state_a.claim.is_none());
    assert_eq!(slot.holder(), None);

    // AC4: second INPUT claims and forwards
    assert_eq!(
        dispatch(&mut state_b, &slot, &input_wire(b"world")),
        FrameAction::ForwardInput(b"world".to_vec())
    );
    assert!(state_b.claim.is_some());
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
    assert!(state.claim.is_none());
    assert_eq!(slot.holder(), None);
}

#[test]
fn test_wire_empty_input_from_other_slot_replies_and_changes_nothing() {
    let slot_a = InteractiveSlot::new();
    let slot_b = InteractiveSlot::new();
    let mut owner = ClientState::new(1);
    assert_eq!(
        dispatch(&mut owner, &slot_a, &input_wire(b"a")),
        FrameAction::ForwardInput(b"a".to_vec())
    );

    assert_eq!(
        dispatch(&mut owner, &slot_b, &input_wire(b"")),
        FrameAction::Reply {
            message: "connection holds another slot",
            close: false,
        }
    );
    assert!(owner.claim.is_some());
    assert_eq!(slot_a.holder(), Some(1));
    assert_eq!(slot_b.holder(), None);
}

#[test]
fn test_dropped_client_releases_its_claim() {
    let slot = InteractiveSlot::new();
    {
        let mut state = ClientState::new(1);
        assert_eq!(
            dispatch(&mut state, &slot, &input_wire(b"a")),
            FrameAction::ForwardInput(b"a".to_vec())
        );
        assert_eq!(slot.holder(), Some(1));
    }
    assert_eq!(slot.holder(), None);

    let mut next = ClientState::new(2);
    assert_eq!(
        dispatch(&mut next, &slot, &input_wire(b"b")),
        FrameAction::ForwardInput(b"b".to_vec())
    );
    assert_eq!(slot.holder(), Some(2));
}

#[test]
fn test_dropped_client_on_poisoned_slot_releases_without_panic() {
    let slot = InteractiveSlot::new();
    let mut state = ClientState::new(1);
    assert_eq!(
        dispatch(&mut state, &slot, &input_wire(b"a")),
        FrameAction::ForwardInput(b"a".to_vec())
    );
    poison(&slot);

    drop(state);
    assert_eq!(raw_holder(&slot), None);
}

#[test]
fn test_unwinding_past_client_on_poisoned_slot_does_not_abort() {
    let slot = InteractiveSlot::new();
    let mut state = ClientState::new(1);
    assert_eq!(
        dispatch(&mut state, &slot, &input_wire(b"a")),
        FrameAction::ForwardInput(b"a".to_vec())
    );
    poison(&slot);

    // The panic unwinds through `held`, so its Drop runs during unwinding. A panic inside that
    // Drop would abort the test process instead of returning `Err` here.
    let outcome = std::panic::catch_unwind(std::panic::AssertUnwindSafe(move || {
        let _held = state;
        panic!("unwind past a live claim");
    }));
    assert!(outcome.is_err());
    assert_eq!(raw_holder(&slot), None);
}

#[test]
fn test_input_holder_no_reclaim() {
    let slot = InteractiveSlot::new();
    let mut state = ClientState::new(1);

    assert_eq!(
        dispatch(&mut state, &slot, &input_wire(b"first")),
        FrameAction::ForwardInput(b"first".to_vec())
    );
    let claim = state.claim.expect("first input claims");

    // INPUT again by holder forwards without re-claim: the claim is unchanged.
    assert_eq!(
        dispatch(&mut state, &slot, &input_wire(b"second")),
        FrameAction::ForwardInput(b"second".to_vec())
    );
    assert_eq!(state.claim, Some(claim));
}

#[test]
fn test_wire_resize() {
    let slot = InteractiveSlot::new();
    let mut state = ClientState::new(1);
    let good = raw_frame(b"RESIZE", &[80, 0, 24, 0]);
    let bad = raw_frame(b"RESIZE", &[80, 0, 24]);

    // RESIZE from a connection without a claim is held, never applied
    assert_eq!(dispatch(&mut state, &slot, &good), FrameAction::Ignore);

    // The first INPUT claims the slot and lands the held size before its input
    assert_eq!(
        dispatch(&mut state, &slot, &input_wire(b"x")),
        FrameAction::ApplyResizeThenForward {
            resize: Resize { cols: 80, rows: 24 },
            input: b"x".to_vec(),
        }
    );

    // RESIZE good from the holder applies
    assert_eq!(
        dispatch(&mut state, &slot, &good),
        FrameAction::ApplyResize(Resize { cols: 80, rows: 24 })
    );

    // RESIZE bad length replies and closes, holder or not, with the literal wire text
    let reply = FrameAction::Reply {
        message: "bad RESIZE",
        close: true,
    };
    assert_eq!(dispatch(&mut state, &slot, &bad), reply);
    finish_client(&mut state);
    assert_eq!(dispatch(&mut state, &slot, &bad), reply);
}

#[test]
fn test_resize_zero_dimensions_never_reach_backend() {
    let slot = InteractiveSlot::new();
    let mut state = ClientState::new(1);
    assert!(matches!(
        dispatch(&mut state, &slot, &input_wire(b"x")),
        FrameAction::ForwardInput(_)
    ));

    // Zero rows, zero columns, and both zero are ignored for the holder.
    for payload in [[0u8, 0, 24, 0], [80, 0, 0, 0], [0, 0, 0, 0]] {
        let wire = raw_frame(b"RESIZE", &payload);
        assert_eq!(
            dispatch(&mut state, &slot, &wire),
            FrameAction::Ignore,
            "payload {payload:?} must not reach the backend"
        );
    }

    // A valid size after the zero frames still applies.
    let valid = raw_frame(b"RESIZE", &[80, 0, 24, 0]);
    assert_eq!(
        dispatch(&mut state, &slot, &valid),
        FrameAction::ApplyResize(Resize { cols: 80, rows: 24 })
    );
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
    finish_client(&mut state);
    assert_eq!(slot.holder(), None);
    assert!(state.claim.is_none());
}

#[test]
fn test_framing_failures_close_and_hold_slot_until_finish() {
    let slot = InteractiveSlot::new();
    let mut state = ClientState::new(1);
    assert!(matches!(
        dispatch(&mut state, &slot, &input_wire(b"k")),
        FrameAction::ForwardInput(_)
    ));

    // Truncated: the length header ends after two bytes.
    assert_eq!(dispatch(&mut state, &slot, &[5, 0]), FrameAction::Close);
    // Truncated body: the header promises 10 bytes and 3 arrive.
    assert_eq!(
        dispatch(&mut state, &slot, &[10, 0, 0, 0, b'I', b'N', b'P']),
        FrameAction::Close
    );
    // Oversize: a length below the 6-byte tag.
    assert_eq!(
        dispatch(&mut state, &slot, &5u32.to_le_bytes()),
        FrameAction::Close
    );
    // Oversize: a length above the frame maximum, refused before the body is read.
    let too_long = u32::try_from(MAX_FRAME_LEN + 1).expect("fits u32");
    assert_eq!(
        dispatch(&mut state, &slot, &too_long.to_le_bytes()),
        FrameAction::Close
    );
    // I/O failure while reading the stream.
    assert_eq!(
        dispatch_reader(&mut state, &slot, &mut FailingReader),
        FrameAction::Close
    );

    // Each framing close keeps the claim until the server calls finish_client.
    assert_eq!(slot.holder(), Some(1));
    finish_client(&mut state);
    assert_eq!(slot.holder(), None);
}

#[test]
fn test_wire_unknown_and_output_tags_reply_unknown() {
    let slot = InteractiveSlot::new();
    let mut state = ClientState::new(1);
    let unknown = FrameAction::Reply {
        message: "unknown",
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
fn test_release_and_reclaim_with_stale_duplicate_connection() {
    let slot = InteractiveSlot::new();
    let mut state_a = ClientState::new(1);
    assert!(matches!(
        dispatch(&mut state_a, &slot, &input_wire(b"a")),
        FrameAction::ForwardInput(_)
    ));

    // A duplicate connection with id 1 and no claim is refused while A holds the slot.
    let mut duplicate = ClientState::new(1);
    assert_eq!(
        dispatch(&mut duplicate, &slot, &input_wire(b"dup")),
        FrameAction::Reply {
            message: "interactive slot held",
            close: false,
        }
    );
    assert!(duplicate.claim.is_none());

    // A stale copy of A's claim, as a duplicated connection record would carry it.
    let mut stale = ClientState {
        connection_id: 1,
        claim: state_a.claim,
        slot: state_a.slot.clone(),
        pending_resize: None,
    };

    // A detaches, and C claims the released slot.
    assert_eq!(
        dispatch(&mut state_a, &slot, &detach_wire()),
        FrameAction::Close
    );
    let mut state_c = ClientState::new(2);
    assert!(matches!(
        dispatch(&mut state_c, &slot, &input_wire(b"c")),
        FrameAction::ForwardInput(_)
    ));
    assert_eq!(slot.holder(), Some(2));

    // The stale copy with the reused id cannot forward into C's session.
    assert_eq!(
        dispatch(&mut stale, &slot, &input_wire(b"stale")),
        FrameAction::Reply {
            message: "interactive slot held",
            close: false,
        }
    );
    assert!(stale.claim.is_none());
    assert_eq!(slot.holder(), Some(2));

    // A stale DETACH closes without releasing C's claim.
    assert_eq!(
        dispatch(&mut stale, &slot, &detach_wire()),
        FrameAction::Close
    );
    assert_eq!(slot.holder(), Some(2));

    // C keeps its single-writer access.
    assert_eq!(
        dispatch(&mut state_c, &slot, &input_wire(b"c2")),
        FrameAction::ForwardInput(b"c2".to_vec())
    );
}

#[test]
fn test_stale_copy_with_reused_id_after_reclaim() {
    // The stale copy carries the id of the NEW holder. Liveness must compare the full claim
    // (id and epoch), not the id alone, or this stale copy would forward into the new session.
    let slot = InteractiveSlot::new();
    let mut first = ClientState::new(1);
    assert!(matches!(
        dispatch(&mut first, &slot, &input_wire(b"a")),
        FrameAction::ForwardInput(_)
    ));
    let stale_claim = first.claim;
    let stale_slot = first.slot.clone();
    assert_eq!(
        dispatch(&mut first, &slot, &detach_wire()),
        FrameAction::Close
    );

    let mut second = ClientState::new(1);
    assert!(matches!(
        dispatch(&mut second, &slot, &input_wire(b"b")),
        FrameAction::ForwardInput(_)
    ));
    assert_eq!(slot.holder(), Some(1));

    let mut stale = ClientState {
        connection_id: 1,
        claim: stale_claim,
        slot: stale_slot,
        pending_resize: None,
    };
    assert_eq!(
        dispatch(&mut stale, &slot, &input_wire(b"stale")),
        FrameAction::Reply {
            message: "interactive slot held",
            close: false,
        }
    );
    assert!(stale.claim.is_none());
    assert_eq!(slot.holder(), Some(1));
    assert_eq!(
        dispatch(&mut second, &slot, &input_wire(b"b2")),
        FrameAction::ForwardInput(b"b2".to_vec())
    );
}

#[test]
fn test_resize_before_claim_lands_before_first_input() {
    let slot = InteractiveSlot::new();
    let mut state = ClientState::new(1);

    // Connect-time size arrives before any input, the order an attach client produces.
    assert_eq!(
        dispatch(&mut state, &slot, &raw_frame(b"RESIZE", &[80, 0, 24, 0])),
        FrameAction::Ignore
    );
    assert_eq!(slot.holder(), None);

    // The first input claims, and the held size is applied before the input.
    assert_eq!(
        dispatch(&mut state, &slot, &input_wire(b"ls\r")),
        FrameAction::ApplyResizeThenForward {
            resize: Resize { cols: 80, rows: 24 },
            input: b"ls\r".to_vec(),
        }
    );

    // Later input forwards alone, and a holder resize applies directly.
    assert_eq!(
        dispatch(&mut state, &slot, &input_wire(b"x")),
        FrameAction::ForwardInput(b"x".to_vec())
    );
    assert_eq!(
        dispatch(&mut state, &slot, &raw_frame(b"RESIZE", &[100, 0, 30, 0])),
        FrameAction::ApplyResize(Resize {
            cols: 100,
            rows: 30,
        })
    );
}

#[test]
fn test_pending_resize_keeps_latest_valid_size_only() {
    let slot = InteractiveSlot::new();
    let mut state = ClientState::new(1);

    assert_eq!(
        dispatch(&mut state, &slot, &raw_frame(b"RESIZE", &[80, 0, 24, 0])),
        FrameAction::Ignore
    );
    assert_eq!(
        dispatch(&mut state, &slot, &raw_frame(b"RESIZE", &[100, 0, 30, 0])),
        FrameAction::Ignore
    );
    // Zero dimensions neither apply nor replace the held valid size.
    for payload in [[0u8, 0, 24, 0], [80, 0, 0, 0], [0, 0, 0, 0]] {
        assert_eq!(
            dispatch(&mut state, &slot, &raw_frame(b"RESIZE", &payload)),
            FrameAction::Ignore
        );
    }

    assert_eq!(
        dispatch(&mut state, &slot, &input_wire(b"x")),
        FrameAction::ApplyResizeThenForward {
            resize: Resize {
                cols: 100,
                rows: 30,
            },
            input: b"x".to_vec(),
        }
    );
}

#[test]
fn test_competing_resize_never_applies_without_claim() {
    let slot = InteractiveSlot::new();
    let mut holder = ClientState::new(1);
    let mut rival = ClientState::new(2);

    assert_eq!(
        dispatch(&mut holder, &slot, &input_wire(b"h")),
        FrameAction::ForwardInput(b"h".to_vec())
    );

    // The rival's size is held but never applied while the holder keeps the slot.
    assert_eq!(
        dispatch(&mut rival, &slot, &raw_frame(b"RESIZE", &[120, 0, 40, 0])),
        FrameAction::Ignore
    );
    assert_eq!(slot.holder(), Some(1));

    // The holder's own resize applies, and it never uses the rival's held size.
    assert_eq!(
        dispatch(&mut holder, &slot, &raw_frame(b"RESIZE", &[90, 0, 20, 0])),
        FrameAction::ApplyResize(Resize { cols: 90, rows: 20 })
    );

    // After the holder detaches, the rival's own held size lands on its claim.
    assert_eq!(
        dispatch(&mut holder, &slot, &detach_wire()),
        FrameAction::Close
    );
    assert_eq!(
        dispatch(&mut rival, &slot, &input_wire(b"r")),
        FrameAction::ApplyResizeThenForward {
            resize: Resize {
                cols: 120,
                rows: 40,
            },
            input: b"r".to_vec(),
        }
    );
    assert_eq!(slot.holder(), Some(2));
}

#[test]
fn test_resize_only_connection_never_claims_or_applies() {
    // A connection that sends RESIZE and never INPUT must never claim or resize. The frame layer
    // has no view-only mode; refusing INPUT from view-only attachers is the wiring's job.
    let slot = InteractiveSlot::new();
    let mut resize_only = ClientState::new(5);
    for payload in [[80u8, 0, 24, 0], [120, 0, 40, 0]] {
        assert_eq!(
            dispatch(&mut resize_only, &slot, &raw_frame(b"RESIZE", &payload)),
            FrameAction::Ignore
        );
    }
    assert_eq!(slot.holder(), None);

    // While a writer holds the slot, the resize-only connection's size never reaches the worker.
    let mut writer = ClientState::new(1);
    assert_eq!(
        dispatch(&mut writer, &slot, &input_wire(b"w")),
        FrameAction::ForwardInput(b"w".to_vec())
    );
    assert_eq!(
        dispatch(
            &mut resize_only,
            &slot,
            &raw_frame(b"RESIZE", &[200, 0, 60, 0])
        ),
        FrameAction::Ignore
    );
    assert_eq!(slot.holder(), Some(1));
    assert!(resize_only.claim.is_none());
}

#[test]
fn test_disconnect_drops_held_size_before_reuse() {
    let slot = InteractiveSlot::new();
    let mut holder = ClientState::new(1);
    assert!(matches!(
        dispatch(&mut holder, &slot, &input_wire(b"h")),
        FrameAction::ForwardInput(_)
    ));

    let mut waiter = ClientState::new(2);
    assert_eq!(
        dispatch(&mut waiter, &slot, &raw_frame(b"RESIZE", &[90, 0, 30, 0])),
        FrameAction::Ignore
    );

    // The connection disconnects before it ever claimed; its held size goes with it.
    finish_client(&mut waiter);
    assert_eq!(waiter.pending_resize, None);

    assert_eq!(
        dispatch(&mut holder, &slot, &detach_wire()),
        FrameAction::Close
    );
    assert_eq!(
        dispatch(&mut waiter, &slot, &input_wire(b"n")),
        FrameAction::ForwardInput(b"n".to_vec())
    );
}

#[test]
fn test_zero_resize_from_non_holder_is_never_kept() {
    let slot = InteractiveSlot::new();
    let mut state = ClientState::new(1);

    assert_eq!(
        dispatch(&mut state, &slot, &raw_frame(b"RESIZE", &[0, 0, 0, 0])),
        FrameAction::Ignore
    );
    assert_eq!(state.pending_resize, None);
    assert_eq!(
        dispatch(&mut state, &slot, &input_wire(b"x")),
        FrameAction::ForwardInput(b"x".to_vec())
    );
}

#[test]
fn test_claim_from_one_slot_never_matches_another_slot() {
    // DPES-962 review F1: the same connection state and the same connection id, used against a
    // second slot, must not write into that slot while another connection holds it.
    let slot_a = InteractiveSlot::new();
    let slot_b = InteractiveSlot::new();

    let mut x = ClientState::new(1);
    assert_eq!(
        dispatch(&mut x, &slot_a, &input_wire(b"a")),
        FrameAction::ForwardInput(b"a".to_vec())
    );
    let x_claim_a = x.claim.expect("X claimed slot A");

    let mut y = ClientState::new(1);
    assert_eq!(
        dispatch(&mut y, &slot_b, &input_wire(b"y")),
        FrameAction::ForwardInput(b"y".to_vec())
    );

    // X's claim was issued by slot A and is bound to it. Against slot B X is refused, nothing
    // reaches slot B's holder, and X keeps its claim on A.
    assert_eq!(
        dispatch(&mut x, &slot_b, &input_wire(b"x-into-b")),
        FrameAction::Reply {
            message: "connection holds another slot",
            close: false,
        }
    );
    assert_eq!(x.claim, Some(x_claim_a));
    assert_eq!(slot_b.holder(), Some(1));
    assert_eq!(slot_a.holder(), Some(1));

    // A claim from slot A releases nothing in slot B, and slot A still holds it.
    assert!(!slot_b.is_current(x_claim_a));
    assert!(!slot_b.release(x_claim_a));
    assert_eq!(slot_b.holder(), Some(1));
    assert!(slot_a.is_current(x_claim_a));
    assert!(slot_a.release(x_claim_a));
    assert_eq!(slot_a.holder(), None);
}

/// DPES-979 F1 regression, the reproduced sequence: claim on A, attempted use on B, the connection
/// closes and finishes, then a fresh client acquires A.
#[test]
fn test_cross_slot_use_never_orphans_claim_on_issuing_slot() {
    let slot_a = InteractiveSlot::new();
    let slot_b = InteractiveSlot::new();
    let mut x = ClientState::new(1);

    assert_eq!(
        dispatch(&mut x, &slot_a, &input_wire(b"a")),
        FrameAction::ForwardInput(b"a".to_vec())
    );
    let claim_a = x.claim.expect("X claimed slot A");

    // Attempted use on B: INPUT and DETACH are refused and RESIZE is ignored. Nothing claims B,
    // and X keeps its claim on A.
    assert_eq!(
        dispatch(&mut x, &slot_b, &input_wire(b"b")),
        FrameAction::Reply {
            message: "connection holds another slot",
            close: false,
        }
    );
    assert_eq!(
        dispatch(&mut x, &slot_b, &raw_frame(b"RESIZE", &[90, 0, 30, 0])),
        FrameAction::Ignore
    );
    assert_eq!(
        dispatch(&mut x, &slot_b, &detach_wire()),
        FrameAction::Reply {
            message: "connection holds another slot",
            close: false,
        }
    );
    assert_eq!(slot_b.holder(), None);
    assert_eq!(slot_a.holder(), Some(1));
    assert_eq!(x.claim, Some(claim_a));
    assert_eq!(x.pending_resize, None);

    // The connection closes and finishes. `finish_client` takes no slot, so it releases the claim
    // on A, the slot that issued it. Nothing is left held by a connection that no longer exists.
    finish_client(&mut x);
    assert_eq!(slot_a.holder(), None);
    assert!(x.claim.is_none());
    assert!(x.slot.is_none());

    // A fresh client acquires A, so the wedge is gone.
    let mut fresh = ClientState::new(2);
    assert_eq!(
        dispatch(&mut fresh, &slot_a, &input_wire(b"fresh")),
        FrameAction::ForwardInput(b"fresh".to_vec())
    );
    assert_eq!(slot_a.holder(), Some(2));
    assert_eq!(slot_b.holder(), None);
}

/// DPES-979 F1: a binding whose claim went stale refuses other slots until finish clears it, and
/// then the same connection can claim B.
///
/// Production paths reach a stale claim only through `release`, which `finish_client` calls. This
/// test releases the claim directly to reach the stale state, so it pins the refusal and the
/// recovery rather than a path the wiring can take today.
#[test]
fn test_stale_binding_refuses_other_slot_until_finish() {
    let slot_a = InteractiveSlot::new();
    let slot_b = InteractiveSlot::new();
    let mut x = ClientState::new(1);

    assert!(matches!(
        dispatch(&mut x, &slot_a, &input_wire(b"a")),
        FrameAction::ForwardInput(_)
    ));
    // Release X's claim on A directly, so the binding is stale.
    let claim_a = x.claim.expect("X claimed slot A");
    assert!(slot_a.release(claim_a));
    assert_eq!(slot_a.holder(), None);

    // The stale binding still refuses B, and the refusal leaves the binding in place.
    assert_eq!(
        dispatch(&mut x, &slot_b, &input_wire(b"b")),
        FrameAction::Reply {
            message: "connection holds another slot",
            close: false,
        }
    );
    assert_eq!(x.claim, Some(claim_a));
    assert_eq!(slot_b.holder(), None);

    // `finish_client` clears the stale binding; B is then claimable by the same connection.
    finish_client(&mut x);
    assert!(x.claim.is_none());
    assert!(x.slot.is_none());
    assert_eq!(
        dispatch(&mut x, &slot_b, &input_wire(b"b")),
        FrameAction::ForwardInput(b"b".to_vec())
    );
    assert_eq!(slot_b.holder(), Some(1));
    assert_eq!(slot_a.holder(), None);
}

#[test]
fn test_finish_client() {
    let slot = InteractiveSlot::new();
    let mut state = ClientState::new(1);

    // finish_client when no claim is held is a no-op
    finish_client(&mut state);
    assert_eq!(slot.holder(), None);

    // interactive client
    assert!(matches!(
        dispatch(&mut state, &slot, &input_wire(b"k")),
        FrameAction::ForwardInput(_)
    ));
    assert_eq!(slot.holder(), Some(1));

    // finish_client releases once and is idempotent
    finish_client(&mut state);
    assert_eq!(slot.holder(), None);
    assert!(state.claim.is_none());

    // second call is idempotent
    finish_client(&mut state);
    assert_eq!(slot.holder(), None);
    assert!(state.claim.is_none());
}

#[test]
fn test_unix_attach_argv() {
    // tmux writable: exact-match target `=fm-<id>` (DPES-962 AC1)
    let tmux_argv =
        unix_attach_argv(BackendToken::Tmux, "task-1", AttachMode::Writable).expect("tmux argv");
    assert_eq!(
        tmux_argv,
        vec!["tmux", "attach-session", "-t", "=fm-task-1"]
    );

    // tmux read-only attaches with -r before -t
    let tmux_ro =
        unix_attach_argv(BackendToken::Tmux, "task-1", AttachMode::ReadOnly).expect("tmux argv");
    assert_eq!(
        tmux_ro,
        vec!["tmux", "attach-session", "-r", "-t", "=fm-task-1"]
    );

    // herdr: no argv until the pinned CLI is verified (DPES-962 AC4), in either mode
    for mode in [AttachMode::Writable, AttachMode::ReadOnly] {
        let err = unix_attach_argv(BackendToken::Herdr, "task-1", mode)
            .expect_err("herdr attach is refused until verified");
        assert_eq!(err.exit_code(), 7);
        assert!(matches!(
            err,
            McaoError::Unsupported { ref backend, op: "herdr attach" } if backend == "herdr"
        ));
    }

    // uppercase and underscore ids attach as the base attach did
    let underscored = unix_attach_argv(BackendToken::Tmux, "a_b", AttachMode::Writable)
        .expect("underscore id attaches");
    assert_eq!(underscored.last().map(String::as_str), Some("=fm-a_b"));
    let upper = unix_attach_argv(BackendToken::Tmux, "Task-1", AttachMode::Writable)
        .expect("uppercase id attaches");
    assert_eq!(upper.last().map(String::as_str), Some("=fm-Task-1"));

    // windows has no unix attach argv
    let err = unix_attach_argv(BackendToken::Windows, "task-1", AttachMode::Writable)
        .expect_err("windows has no unix attach");
    assert_eq!(err.exit_code(), 7);
}

#[test]
fn test_unix_attach_id_cap_matches_shell_creator() {
    // 64 bytes is the shell creator's cap (`fm_task_id_creation_valid`); attach applies it too.
    let at_cap = "a".repeat(64);
    let over_cap = "a".repeat(65);
    let argv = unix_attach_argv(BackendToken::Tmux, &at_cap, AttachMode::Writable)
        .expect("a 64-byte id attaches");
    assert_eq!(
        argv.last().map(String::as_str),
        Some(format!("={}", "fm-".to_owned() + &at_cap).as_str())
    );
    let err = unix_attach_argv(BackendToken::Tmux, &over_cap, AttachMode::Writable)
        .expect_err("65-byte id must not build an attach argv");
    assert_eq!(err.exit_code(), 2);
    // Creation's own rule carries the same cap, through its own error variant.
    assert!(matches!(
        TaskId::parse_for_creation(&over_cap),
        Err(TaskIdError::TooLong { len: 65 })
    ));
}

#[test]
fn test_unix_attach_refuses_dotted_empty_and_separator_ids() {
    // Dotted ids (tmux pane separator), empty ids, leading dots, and separators build no argv.
    for raw in ["a.b", "fm-a.b", "", ".x", "a/b", "a b"] {
        for backend in [BackendToken::Tmux, BackendToken::Herdr] {
            for mode in [AttachMode::Writable, AttachMode::ReadOnly] {
                let err = unix_attach_argv(backend, raw, mode)
                    .expect_err("creation-refused id must not build an attach argv");
                assert_eq!(err.exit_code(), 2, "{raw:?} is a usage error");
            }
        }
    }
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
