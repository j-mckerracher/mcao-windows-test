//! Per-connection read-only console session (DPES-883).
//!
//! A sans-I/O state machine for one client connection. The transport owns the I/O: it decodes
//! client frames with [`decode_from`](super::frame::decode_from) and passes each one to
//! [`ReadOnlySession::handle_frame`] (or [`ReadOnlySession::handle_decode_error`] and
//! [`ReadOnlySession::peer_closed`]), writes the frames returned, streams OUTPUT frames by
//! calling [`ReadOnlySession::poll_output`] until it returns `None`, and reports each written
//! OUTPUT payload with [`ReadOnlySession::drained`].
//!
//! Behavior:
//! - Nothing is sent before SUBSCR. SUBSCR starts the stream. DETACH closes cleanly at any time.
//! - SUBSCR(offset) replays ring bytes from `offset` as OUTPUT frames of at most
//!   [`MAX_PAYLOAD_LEN`] bytes. An offset older than the ring's oldest byte is clamped to that
//!   oldest byte, so the client receives the newest bytes the ring still holds (`peek` always
//!   subscribes at 0). No gap marker is sent, so the client cannot see that bytes were skipped.
//!   An offset at or past the current end replays nothing and streams live output from there.
//! - Output appended after SUBSCR is forwarded by `poll_output`.
//! - Every OUTPUT and ERROR frame the session returns counts against the backlog (1 MiB by
//!   default) until the transport reports it written with `drained`. Exceeding the limit, or the
//!   ring overwriting bytes not yet sent, sends ERROR "backlog exceeded" and closes. This includes
//!   refusals, so a flood of refused INPUT frames to a client that does not read also closes it.
//! - INPUT and RESIZE are refused with an ERROR and nothing is forwarded; the session stays open.
//! - Before SUBSCR, INPUT and RESIZE are refused the same way and the session waits for SUBSCR.
//! - A second SUBSCR, or OUTPUT or ERROR from the client, sends an ERROR and closes.
//! - A length above 65,535 sends ERROR "frame exceeds 65535 bytes" and closes. A length of 0 to 5
//!   sends ERROR "frame size out of range" and closes.

use std::collections::VecDeque;

use super::frame::{Frame, FrameError, MAX_FRAME_LEN, MAX_PAYLOAD_LEN, error_frame};
use super::ring::RingBuffer;
use crate::peek::BacklogTracker;

const BACKLOG_EXCEEDED: &str = "backlog exceeded";
const INPUT_REFUSED: &str = "read-only session: INPUT refused";
const RESIZE_REFUSED: &str = "read-only session: RESIZE refused";
const ALREADY_SUBSCRIBED: &str = "already subscribed";
const UNEXPECTED_FRAME: &str = "unexpected frame from client";
const FRAME_TOO_LARGE: &str = "frame exceeds 65535 bytes";
const FRAME_TOO_SMALL: &str = "frame size out of range";

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum State {
    AwaitingSubscribe,
    Streaming,
    Closed,
}

/// Returns the number of bytes a frame counts against the backlog while it waits to be written.
///
/// Every frame the session returns counts: OUTPUT payloads and ERROR messages. The session only
/// returns OUTPUT and ERROR frames, so other variants count as zero.
pub fn queued_len(frame: &Frame) -> usize {
    match frame {
        Frame::Output(bytes) => bytes.len(),
        Frame::Error(message) => message.len(),
        _ => 0,
    }
}

/// The server side of one read-only console connection.
#[derive(Debug)]
pub struct ReadOnlySession {
    state: State,
    cursor: u64,
    pending: VecDeque<u8>,
    backlog: BacklogTracker,
    backlog_exceeded: bool,
}

impl ReadOnlySession {
    /// Creates a session with the default backlog limit of [`crate::peek::MAX_BACKLOG_BYTES`].
    pub fn new() -> Self {
        Self::with_backlog(BacklogTracker::new())
    }

    /// Creates a session with a custom backlog tracker.
    pub fn with_backlog(backlog: BacklogTracker) -> Self {
        Self {
            state: State::AwaitingSubscribe,
            cursor: 0,
            pending: VecDeque::new(),
            backlog,
            backlog_exceeded: false,
        }
    }

    /// Returns `true` once the session has closed and no further frames will be sent.
    pub fn is_closed(&self) -> bool {
        self.state == State::Closed
    }

    /// Returns `true` when the session closed because the client's backlog passed its limit, or
    /// the ring overwrote bytes the client had not received. The ERROR frame is returned with
    /// the call that closed the session.
    pub fn backlog_exceeded(&self) -> bool {
        self.backlog_exceeded
    }

    /// Handles one frame from the client and returns the frames to send back.
    pub fn handle_frame(&mut self, frame: Frame, ring: &RingBuffer) -> Vec<Frame> {
        if self.is_closed() {
            return Vec::new();
        }
        match frame {
            Frame::Subscribe { offset } => {
                if self.state == State::Streaming {
                    return vec![self.fail(ALREADY_SUBSCRIBED)];
                }
                self.subscribe(offset, ring);
                Vec::new()
            }
            Frame::Detach => {
                self.state = State::Closed;
                Vec::new()
            }
            Frame::Input(_) => vec![self.refuse(INPUT_REFUSED)],
            Frame::Resize { .. } => vec![self.refuse(RESIZE_REFUSED)],
            Frame::Output(_) | Frame::Error(_) => vec![self.fail(UNEXPECTED_FRAME)],
        }
    }

    /// Handles a decode failure on the client stream and returns the frames to send back.
    ///
    /// The session always closes after a decode failure, because the stream can no longer be
    /// trusted to be in sync.
    pub fn handle_decode_error(&mut self, err: FrameError) -> Vec<Frame> {
        if self.is_closed() {
            return Vec::new();
        }
        match err {
            FrameError::Oversize { len } if len > MAX_FRAME_LEN => {
                vec![self.fail(FRAME_TOO_LARGE)]
            }
            FrameError::Oversize { .. } => vec![self.fail(FRAME_TOO_SMALL)],
            FrameError::Truncated | FrameError::Io(_) => {
                self.state = State::Closed;
                Vec::new()
            }
            FrameError::UnknownTag(_) | FrameError::BadPayload { .. } => {
                let message = err.to_string();
                vec![self.fail(&message)]
            }
        }
    }

    /// Records that the client closed its end of the connection.
    pub fn peer_closed(&mut self) {
        self.state = State::Closed;
    }

    /// Returns the next OUTPUT frame to send, or `None` when nothing is pending.
    ///
    /// An ERROR frame is returned when the backlog limit is exceeded or the ring overwrote bytes
    /// the client has not received; the session is closed after it.
    pub fn poll_output(&mut self, ring: &RingBuffer) -> Option<Frame> {
        if self.state != State::Streaming {
            return None;
        }
        if self.pending.is_empty() {
            let current = ring.current_offset();
            if self.cursor >= current {
                return None;
            }
            if self.cursor < ring.oldest_offset() {
                return Some(self.overflow());
            }
            self.pending = VecDeque::from(ring.read_from(self.cursor));
        }

        let take = self.pending.len().min(MAX_PAYLOAD_LEN);
        let chunk: Vec<u8> = self.pending.drain(..take).collect();
        if self.backlog.enqueue(chunk.len()).is_err() {
            return Some(self.overflow());
        }
        self.cursor += chunk.len() as u64;
        Some(Frame::Output(chunk))
    }

    /// Reports that `bytes` of queued payload have been written to the client.
    ///
    /// Pass the value of [`queued_len`] for each frame that was written or skipped.
    pub fn drained(&mut self, bytes: usize) {
        self.backlog.drained(bytes);
    }

    fn subscribe(&mut self, offset: u64, ring: &RingBuffer) {
        let start = offset.clamp(ring.oldest_offset(), ring.current_offset());
        self.cursor = start;
        self.state = State::Streaming;
    }

    /// Closes the session with an ERROR frame and counts that frame against the backlog.
    fn fail(&mut self, message: &str) -> Frame {
        self.state = State::Closed;
        self.account(error_frame(message))
    }

    /// Returns an ERROR frame for a refused request and counts it against the backlog. The
    /// session stays open.
    fn refuse(&mut self, message: &str) -> Frame {
        self.account(error_frame(message))
    }

    fn account(&mut self, frame: Frame) -> Frame {
        if self.backlog.enqueue(queued_len(&frame)).is_ok() {
            frame
        } else {
            self.overflow()
        }
    }

    /// Closes the session because the backlog is full. The ERROR frame is not counted: it is the
    /// last frame this session sends.
    fn overflow(&mut self) -> Frame {
        self.state = State::Closed;
        self.backlog_exceeded = true;
        error_frame(BACKLOG_EXCEEDED)
    }
}

impl Default for ReadOnlySession {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::console::frame::{decode_from, encode};
    use crate::console::ring::DEFAULT_CAPACITY;
    use std::io::{self, Cursor, Read};

    fn pattern(len: usize) -> Vec<u8> {
        (0..len).map(|i| (i % 251) as u8).collect()
    }

    /// Drains every pending OUTPUT frame, acknowledging each one as written.
    fn drain_all(session: &mut ReadOnlySession, ring: &RingBuffer) -> Vec<Frame> {
        let mut frames = Vec::new();
        while let Some(frame) = session.poll_output(ring) {
            if let Frame::Output(bytes) = &frame {
                session.drained(bytes.len());
            }
            frames.push(frame);
        }
        frames
    }

    fn output_bytes(frames: &[Frame]) -> Vec<u8> {
        frames
            .iter()
            .flat_map(|frame| match frame {
                Frame::Output(bytes) => bytes.clone(),
                other => panic!("expected OUTPUT frame, got {other:?}"),
            })
            .collect()
    }

    /// Feeds raw client bytes through the same decode loop a transport would run.
    fn feed_raw(session: &mut ReadOnlySession, ring: &RingBuffer, bytes: &[u8]) -> Vec<Frame> {
        feed_reader(session, ring, &mut Cursor::new(bytes))
    }

    fn feed_reader(
        session: &mut ReadOnlySession,
        ring: &RingBuffer,
        reader: &mut impl Read,
    ) -> Vec<Frame> {
        let mut out = Vec::new();
        while !session.is_closed() {
            match decode_from(reader) {
                Ok(Some(frame)) => out.extend(session.handle_frame(frame, ring)),
                Ok(None) => session.peer_closed(),
                Err(err) => out.extend(session.handle_decode_error(err)),
            }
        }
        out
    }

    /// Builds a wire frame with any tag and payload, bypassing [`encode`] so malformed frames can
    /// be sent.
    fn raw_frame(tag: &[u8; 6], payload: &[u8]) -> Vec<u8> {
        let mut bytes = ((tag.len() + payload.len()) as u32).to_le_bytes().to_vec();
        bytes.extend_from_slice(tag);
        bytes.extend_from_slice(payload);
        bytes
    }

    /// A reader whose every read fails, like a connection reset by the peer.
    struct FailingReader(io::ErrorKind);

    impl Read for FailingReader {
        fn read(&mut self, _buf: &mut [u8]) -> io::Result<usize> {
            Err(io::Error::from(self.0))
        }
    }

    #[test]
    fn subscribe_zero_replays_exact_bytes_in_bounded_chunks() {
        let data = pattern(DEFAULT_CAPACITY);
        let mut ring = RingBuffer::with_default_capacity();
        ring.append(&data);
        let mut session = ReadOnlySession::new();
        assert!(
            session
                .handle_frame(Frame::Subscribe { offset: 0 }, &ring)
                .is_empty()
        );

        let frames = drain_all(&mut session, &ring);
        assert_eq!(frames.len(), 33);
        for frame in &frames {
            let encoded = encode(frame).expect("every OUTPUT frame fits the codec");
            assert!(encoded.len() <= 4 + MAX_FRAME_LEN);
        }
        assert_eq!(output_bytes(&frames), data);
        assert!(!session.is_closed());
    }

    #[test]
    fn three_mib_through_two_mib_ring_replays_only_newest_two_mib() {
        let data = pattern(3 * 1024 * 1024);
        let mut ring = RingBuffer::with_default_capacity();
        ring.append(&data);
        let mut session = ReadOnlySession::new();
        session.handle_frame(Frame::Subscribe { offset: 0 }, &ring);

        let replayed = output_bytes(&drain_all(&mut session, &ring));
        assert_eq!(replayed.len(), DEFAULT_CAPACITY);
        assert_eq!(replayed, data[data.len() - DEFAULT_CAPACITY..]);
    }

    #[test]
    fn live_append_after_subscribe_is_forwarded() {
        let mut ring = RingBuffer::new(4096);
        let mut session = ReadOnlySession::new();
        session.handle_frame(Frame::Subscribe { offset: 0 }, &ring);
        assert!(session.poll_output(&ring).is_none());

        ring.append(b"hello");
        assert_eq!(
            drain_all(&mut session, &ring),
            vec![Frame::Output(b"hello".to_vec())]
        );

        ring.append(b" world");
        assert_eq!(
            drain_all(&mut session, &ring),
            vec![Frame::Output(b" world".to_vec())]
        );
    }

    #[test]
    fn undrained_replay_past_backlog_limit_sends_error_and_closes() {
        let ring = {
            let mut ring = RingBuffer::with_default_capacity();
            ring.append(&pattern(DEFAULT_CAPACITY));
            ring
        };
        let mut session = ReadOnlySession::new();
        session.handle_frame(Frame::Subscribe { offset: 0 }, &ring);

        // 16 chunks of 65,529 bytes fit in 1 MiB; the 17th exceeds the limit.
        for _ in 0..16 {
            assert!(matches!(session.poll_output(&ring), Some(Frame::Output(_))));
        }
        assert_eq!(
            session.poll_output(&ring),
            Some(error_frame("backlog exceeded"))
        );
        assert!(session.is_closed());
        assert!(session.poll_output(&ring).is_none());
    }

    #[test]
    fn detach_closes_cleanly_before_and_after_subscribe() {
        let mut ring = RingBuffer::new(64);
        ring.append(b"abc");

        let mut early = ReadOnlySession::new();
        assert!(early.handle_frame(Frame::Detach, &ring).is_empty());
        assert!(early.is_closed());

        let mut late = ReadOnlySession::new();
        late.handle_frame(Frame::Subscribe { offset: 0 }, &ring);
        assert!(late.handle_frame(Frame::Detach, &ring).is_empty());
        assert!(late.is_closed());
        assert!(late.poll_output(&ring).is_none());
    }

    #[test]
    fn oversize_length_sends_exact_error_and_closes() {
        let ring = RingBuffer::new(64);
        let mut session = ReadOnlySession::new();
        let bytes = 65_536u32.to_le_bytes();
        let frames = feed_raw(&mut session, &ring, &bytes);
        assert_eq!(
            frames,
            vec![Frame::Error("frame exceeds 65535 bytes".into())]
        );
        assert!(session.is_closed());
    }

    #[test]
    fn undersize_length_sends_exact_error_and_closes() {
        for len in 0u32..=5 {
            let ring = RingBuffer::new(64);
            let mut session = ReadOnlySession::new();
            let frames = feed_raw(&mut session, &ring, &len.to_le_bytes());
            assert_eq!(
                frames,
                vec![Frame::Error("frame size out of range".into())],
                "length {len}"
            );
            assert!(session.is_closed(), "length {len}");
        }
    }

    #[test]
    fn input_and_resize_are_refused_without_forwarding() {
        let mut ring = RingBuffer::new(64);
        ring.append(b"out");
        let mut session = ReadOnlySession::new();
        session.handle_frame(Frame::Subscribe { offset: 0 }, &ring);

        let input = session.handle_frame(Frame::Input(b"rm -rf\n".to_vec()), &ring);
        let resize = session.handle_frame(Frame::Resize { cols: 80, rows: 24 }, &ring);
        assert_eq!(input, vec![Frame::Error(INPUT_REFUSED.into())]);
        assert_eq!(resize, vec![Frame::Error(RESIZE_REFUSED.into())]);
        assert!(!session.is_closed());
        assert_eq!(
            drain_all(&mut session, &ring),
            vec![Frame::Output(b"out".to_vec())]
        );
    }

    #[test]
    fn refused_input_counts_against_the_backlog_until_drained() {
        let ring = RingBuffer::new(DEFAULT_CAPACITY);
        let mut session = ReadOnlySession::with_backlog(BacklogTracker::with_limit(100));
        session.handle_frame(Frame::Subscribe { offset: 0 }, &ring);
        let refusal_len = queued_len(&error_frame("read-only session: INPUT refused"));

        let limit_frames = 100 / refusal_len;
        for _ in 0..limit_frames {
            let frames = session.handle_frame(Frame::Input(b"k".to_vec()), &ring);
            assert!(matches!(frames.as_slice(), [Frame::Error(_)]));
        }
        assert!(!session.backlog_exceeded());

        let closed = session.handle_frame(Frame::Input(b"k".to_vec()), &ring);
        assert_eq!(closed, vec![Frame::Error("backlog exceeded".into())]);
        assert!(session.backlog_exceeded());
        assert!(session.is_closed());

        let mut drained = ReadOnlySession::with_backlog(BacklogTracker::with_limit(100));
        drained.handle_frame(Frame::Subscribe { offset: 0 }, &ring);
        for _ in 0..limit_frames {
            drained.handle_frame(Frame::Input(b"k".to_vec()), &ring);
            drained.drained(refusal_len);
        }
        assert!(
            !drained.backlog_exceeded(),
            "written refusals free the backlog"
        );
    }

    #[test]
    fn frames_before_subscribe_wait_for_subscribe_and_output_is_rejected() {
        let mut ring = RingBuffer::new(64);
        ring.append(b"stored");

        let mut session = ReadOnlySession::new();
        let refused = session.handle_frame(Frame::Input(b"x".to_vec()), &ring);
        assert_eq!(refused, vec![Frame::Error(INPUT_REFUSED.into())]);
        assert!(!session.is_closed());
        assert!(session.poll_output(&ring).is_none());
        session.handle_frame(Frame::Subscribe { offset: 0 }, &ring);
        assert_eq!(
            drain_all(&mut session, &ring),
            vec![Frame::Output(b"stored".to_vec())]
        );

        let mut rogue = ReadOnlySession::new();
        let rejected = rogue.handle_frame(Frame::Output(b"spoof".to_vec()), &ring);
        assert_eq!(rejected, vec![Frame::Error(UNEXPECTED_FRAME.into())]);
        assert!(rogue.is_closed());
    }

    #[test]
    fn second_subscribe_sends_error_and_closes() {
        let ring = RingBuffer::new(64);
        let mut session = ReadOnlySession::new();
        session.handle_frame(Frame::Subscribe { offset: 0 }, &ring);
        let frames = session.handle_frame(Frame::Subscribe { offset: 0 }, &ring);
        assert_eq!(frames, vec![Frame::Error(ALREADY_SUBSCRIBED.into())]);
        assert!(session.is_closed());
    }

    #[test]
    fn subscribe_mid_ring_offset_replays_exact_tail() {
        let mut ring = RingBuffer::new(64);
        let data = pattern(100);
        ring.append(&data);
        assert_eq!(ring.oldest_offset(), 36);

        let mut session = ReadOnlySession::new();
        session.handle_frame(Frame::Subscribe { offset: 60 }, &ring);
        assert_eq!(output_bytes(&drain_all(&mut session, &ring)), data[60..]);
    }

    #[test]
    fn subscribe_past_end_replays_nothing_then_streams_live_output() {
        let mut ring = RingBuffer::new(64);
        ring.append(b"abc");

        let mut session = ReadOnlySession::new();
        assert!(
            session
                .handle_frame(Frame::Subscribe { offset: 100 }, &ring)
                .is_empty()
        );
        assert!(drain_all(&mut session, &ring).is_empty());

        ring.append(b"d");
        assert_eq!(
            drain_all(&mut session, &ring),
            vec![Frame::Output(b"d".to_vec())]
        );
        assert!(!session.is_closed());
    }

    #[test]
    fn client_that_never_polls_before_ring_wraps_gets_backlog_exceeded() {
        let mut ring = RingBuffer::new(64);
        let mut session = ReadOnlySession::new();
        session.handle_frame(Frame::Subscribe { offset: 0 }, &ring);

        ring.append(&pattern(200));
        assert_eq!(
            drain_all(&mut session, &ring),
            vec![Frame::Error("backlog exceeded".into())]
        );
        assert!(session.is_closed());
    }

    #[test]
    fn cursor_at_oldest_byte_streams_but_losing_the_cursor_byte_errors() {
        let mut ring = RingBuffer::new(64);
        let mut session = ReadOnlySession::new();
        session.handle_frame(Frame::Subscribe { offset: 0 }, &ring);
        ring.append(&pattern(64));
        assert_eq!(output_bytes(&drain_all(&mut session, &ring)), pattern(64));

        // The client cursor is 64. Writing 64 more leaves byte 64 as the oldest retained byte.
        ring.append(&pattern(64));
        assert_eq!(ring.oldest_offset(), 64);
        assert_eq!(output_bytes(&drain_all(&mut session, &ring)), pattern(64));

        // Cursor is 128. Writing 65 without polling overwrites byte 128.
        ring.append(&pattern(65));
        assert_eq!(
            drain_all(&mut session, &ring),
            vec![Frame::Error("backlog exceeded".into())]
        );
        assert!(session.is_closed());
    }

    #[test]
    fn peer_closed_stops_output_and_later_frames() {
        let mut ring = RingBuffer::new(64);
        ring.append(b"abc");
        let mut session = ReadOnlySession::new();
        session.handle_frame(Frame::Subscribe { offset: 0 }, &ring);

        session.peer_closed();
        assert!(session.is_closed());
        assert!(session.poll_output(&ring).is_none());
        assert!(
            session
                .handle_frame(Frame::Input(b"x".to_vec()), &ring)
                .is_empty()
        );
    }

    #[test]
    fn clean_end_of_stream_after_subscribe_closes_silently() {
        let mut ring = RingBuffer::new(64);
        ring.append(b"abc");
        let mut session = ReadOnlySession::new();
        let subscribe = encode(&Frame::Subscribe { offset: 0 }).unwrap();

        assert!(feed_raw(&mut session, &ring, &subscribe).is_empty());
        assert!(session.is_closed());
        assert!(session.poll_output(&ring).is_none());
    }

    #[test]
    fn truncated_frame_closes_without_an_error_frame() {
        let subscribe = raw_frame(b"SUBSCR", &0u64.to_le_bytes());
        // Cut inside the length header, then inside the body.
        for cut in [2, subscribe.len() - 1] {
            let ring = RingBuffer::new(64);
            let mut session = ReadOnlySession::new();
            let frames = feed_raw(&mut session, &ring, &subscribe[..cut]);
            assert!(frames.is_empty(), "cut at {cut}");
            assert!(session.is_closed(), "cut at {cut}");
        }
    }

    #[test]
    fn io_error_on_read_closes_without_an_error_frame() {
        let ring = RingBuffer::new(64);
        let mut session = ReadOnlySession::new();
        let mut reader = FailingReader(io::ErrorKind::ConnectionReset);
        assert!(feed_reader(&mut session, &ring, &mut reader).is_empty());
        assert!(session.is_closed());
    }

    #[test]
    fn unknown_tag_sends_error_and_closes() {
        let ring = RingBuffer::new(64);
        let mut session = ReadOnlySession::new();
        let frames = feed_raw(&mut session, &ring, &raw_frame(b"BOGUS!", &[]));
        assert_eq!(
            frames,
            vec![Frame::Error(
                "unknown frame tag: [66, 79, 71, 85, 83, 33]".into()
            )]
        );
        assert!(session.is_closed());
    }

    #[test]
    fn bad_payload_sends_error_and_closes() {
        let ring = RingBuffer::new(64);
        let mut session = ReadOnlySession::new();
        let frames = feed_raw(&mut session, &ring, &raw_frame(b"SUBSCR", &[0; 7]));
        assert_eq!(
            frames,
            vec![Frame::Error(
                "bad frame payload: subscribe payload must be exactly 8 bytes".into()
            )]
        );
        assert!(session.is_closed());
    }
}
