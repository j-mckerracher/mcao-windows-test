//! Framed wire codec for console observation.
//!
//! Wire frame format: `[u32 little-endian length][6-byte ASCII tag][payload]`.
//! The length counts the tag plus the payload and excludes the 4 length bytes.
//! Maximum length is 65,535, so the payload holds at most 65,529 bytes. (The source
//! text says 65,530; the tag takes 6 of the 65,535, so 65,529 is used.)

use std::fmt;
use std::io::Read;

/// Maximum allowed frame length including the 6-byte tag (65,535 bytes).
pub const MAX_FRAME_LEN: usize = 65_535;

/// Length of the ASCII tag prefix in bytes (6 bytes).
pub const TAG_LEN: usize = 6;

/// Maximum payload length in bytes (65,529 bytes, which is [`MAX_FRAME_LEN`] minus [`TAG_LEN`]).
pub const MAX_PAYLOAD_LEN: usize = MAX_FRAME_LEN - TAG_LEN;

/// Frame tag for subscribing to console output stream.
pub const TAG_SUBSCRIBE: [u8; 6] = *b"SUBSCR";

/// Frame tag for console output payload.
pub const TAG_OUTPUT: [u8; 6] = *b"OUTPUT";

/// Frame tag for detaching from console observation.
pub const TAG_DETACH: [u8; 6] = *b"DETACH";

/// Frame tag for error messages.
pub const TAG_ERROR: [u8; 6] = *b"ERROR ";

/// Frame tag for terminal input bytes sent by an interactive client.
pub const TAG_INPUT: [u8; 6] = *b"INPUT ";

/// Frame tag for a terminal resize request sent by an interactive client.
pub const TAG_RESIZE: [u8; 6] = *b"RESIZE";

/// A framed message exchanged over the console wire protocol.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Frame {
    /// Subscribe to output starting at the specified absolute byte offset.
    Subscribe {
        /// Absolute byte offset to begin reading from.
        offset: u64,
    },
    /// Raw console output chunk.
    Output(Vec<u8>),
    /// Detach from observation.
    Detach,
    /// Error notification with a descriptive message.
    Error(String),
    /// Terminal input bytes from an interactive client, to forward to the worker console.
    Input(Vec<u8>),
    /// Terminal resize request from an interactive client, in columns and rows.
    Resize {
        /// Number of horizontal character columns.
        cols: u16,
        /// Number of vertical character rows.
        rows: u16,
    },
}

/// Errors that can occur during frame encoding or decoding.
#[derive(Debug, PartialEq, Eq)]
pub enum FrameError {
    /// An underlying I/O error occurred.
    Io(std::io::ErrorKind),
    /// Frame length is below [`TAG_LEN`] or above [`MAX_FRAME_LEN`].
    Oversize {
        /// The offending frame length in bytes.
        len: usize,
    },
    /// Stream ended before a complete frame could be read.
    Truncated,
    /// The frame tag was not recognized.
    UnknownTag([u8; 6]),
    /// Frame payload violates protocol constraints for the frame with `tag`.
    BadPayload {
        /// Tag of the frame whose payload was rejected.
        tag: [u8; 6],
        /// Why the payload was rejected.
        reason: &'static str,
    },
}

impl fmt::Display for FrameError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Io(kind) => write!(f, "I/O error: {kind:?}"),
            Self::Oversize { len } => write!(f, "frame length {len} is out of bounds"),
            Self::Truncated => write!(f, "frame data truncated"),
            Self::UnknownTag(tag) => write!(f, "unknown frame tag: {tag:?}"),
            Self::BadPayload { reason, .. } => write!(f, "bad frame payload: {reason}"),
        }
    }
}

impl std::error::Error for FrameError {}

impl From<std::io::Error> for FrameError {
    fn from(err: std::io::Error) -> Self {
        Self::Io(err.kind())
    }
}

/// Encodes a [`Frame`] into its wire representation:
/// `[u32 little-endian length][6-byte ASCII tag][payload]`.
///
/// Length counts the tag (6 bytes) plus payload, excluding the 4 length bytes.
/// Returns [`FrameError::Oversize`] if the payload exceeds [`MAX_PAYLOAD_LEN`].
pub fn encode(frame: &Frame) -> Result<Vec<u8>, FrameError> {
    let offset_bytes;
    let resize_bytes;
    let (tag, payload): ([u8; 6], &[u8]) = match frame {
        Frame::Subscribe { offset } => {
            offset_bytes = offset.to_le_bytes();
            (TAG_SUBSCRIBE, &offset_bytes)
        }
        Frame::Output(bytes) => (TAG_OUTPUT, bytes.as_slice()),
        Frame::Detach => (TAG_DETACH, &[]),
        Frame::Error(msg) => (TAG_ERROR, msg.as_bytes()),
        Frame::Input(bytes) => (TAG_INPUT, bytes.as_slice()),
        Frame::Resize { cols, rows } => {
            let [c0, c1] = cols.to_le_bytes();
            let [r0, r1] = rows.to_le_bytes();
            resize_bytes = [c0, c1, r0, r1];
            (TAG_RESIZE, &resize_bytes)
        }
    };

    if payload.len() > MAX_PAYLOAD_LEN {
        return Err(FrameError::Oversize {
            len: TAG_LEN + payload.len(),
        });
    }

    let frame_len = TAG_LEN + payload.len();
    let len_u32 = frame_len as u32;

    let mut buf = Vec::with_capacity(4 + frame_len);
    buf.extend_from_slice(&len_u32.to_le_bytes());
    buf.extend_from_slice(&tag);
    buf.extend_from_slice(payload);
    Ok(buf)
}

fn parse_payload(tag: [u8; 6], payload: &[u8]) -> Result<Frame, FrameError> {
    match tag {
        TAG_SUBSCRIBE => {
            if payload.len() != 8 {
                return Err(FrameError::BadPayload {
                    tag,
                    reason: "subscribe payload must be exactly 8 bytes",
                });
            }
            let mut offset_bytes = [0u8; 8];
            offset_bytes.copy_from_slice(payload);
            let offset = u64::from_le_bytes(offset_bytes);
            Ok(Frame::Subscribe { offset })
        }
        TAG_INPUT => Ok(Frame::Input(payload.to_vec())),
        TAG_RESIZE => match payload {
            [c0, c1, r0, r1] => Ok(Frame::Resize {
                cols: u16::from_le_bytes([*c0, *c1]),
                rows: u16::from_le_bytes([*r0, *r1]),
            }),
            _ => Err(FrameError::BadPayload {
                tag,
                reason: "resize payload must be exactly 4 bytes",
            }),
        },
        TAG_OUTPUT => Ok(Frame::Output(payload.to_vec())),
        TAG_DETACH => {
            if !payload.is_empty() {
                return Err(FrameError::BadPayload {
                    tag,
                    reason: "detach payload must be empty",
                });
            }
            Ok(Frame::Detach)
        }
        TAG_ERROR => match std::str::from_utf8(payload) {
            Ok(s) => Ok(Frame::Error(s.to_string())),
            Err(_) => Err(FrameError::BadPayload {
                tag,
                reason: "error payload must be valid UTF-8",
            }),
        },
        _ => Err(FrameError::UnknownTag(tag)),
    }
}

/// Reads and decodes the next [`Frame`] from a reader.
///
/// Returns `Ok(None)` on clean end of stream before any bytes of a frame are read.
/// Returns [`FrameError::Truncated`] if the stream ends mid-frame.
/// Rejects frames with length below [`TAG_LEN`] or above [`MAX_FRAME_LEN`] as
/// [`FrameError::Oversize`] before reading or allocating the frame body.
pub fn decode_from(reader: &mut impl Read) -> Result<Option<Frame>, FrameError> {
    let mut len_buf = [0u8; 4];
    let mut read_len = 0;
    while read_len < 4 {
        match reader.read(&mut len_buf[read_len..]) {
            Ok(0) => {
                if read_len == 0 {
                    return Ok(None);
                }
                return Err(FrameError::Truncated);
            }
            Ok(n) => read_len += n,
            Err(ref e) if e.kind() == std::io::ErrorKind::Interrupted => continue,
            Err(e) => return Err(FrameError::Io(e.kind())),
        }
    }

    let len = u32::from_le_bytes(len_buf) as usize;
    if !(TAG_LEN..=MAX_FRAME_LEN).contains(&len) {
        return Err(FrameError::Oversize { len });
    }

    let mut body = vec![0u8; len];
    let mut read_body = 0;
    while read_body < len {
        match reader.read(&mut body[read_body..]) {
            Ok(0) => return Err(FrameError::Truncated),
            Ok(n) => read_body += n,
            Err(ref e) if e.kind() == std::io::ErrorKind::Interrupted => continue,
            Err(e) => return Err(FrameError::Io(e.kind())),
        }
    }

    let mut tag = [0u8; 6];
    tag.copy_from_slice(&body[..TAG_LEN]);
    let payload = &body[TAG_LEN..];

    parse_payload(tag, payload).map(Some)
}

/// Decodes a [`Frame`] from an in-memory byte slice.
///
/// Returns the decoded [`Frame`] and the number of bytes consumed (4 length bytes + frame length).
/// Returns [`FrameError::Truncated`] if the slice is too short for the header or body.
/// Rejects frames with length below [`TAG_LEN`] or above [`MAX_FRAME_LEN`] as
/// [`FrameError::Oversize`].
pub fn decode_slice(bytes: &[u8]) -> Result<(Frame, usize), FrameError> {
    if bytes.len() < 4 {
        return Err(FrameError::Truncated);
    }

    let mut len_buf = [0u8; 4];
    len_buf.copy_from_slice(&bytes[..4]);
    let len = u32::from_le_bytes(len_buf) as usize;

    if !(TAG_LEN..=MAX_FRAME_LEN).contains(&len) {
        return Err(FrameError::Oversize { len });
    }

    let total_len = 4 + len;
    if bytes.len() < total_len {
        return Err(FrameError::Truncated);
    }

    let mut tag = [0u8; 6];
    tag.copy_from_slice(&bytes[4..4 + TAG_LEN]);
    let payload = &bytes[4 + TAG_LEN..total_len];

    let frame = parse_payload(tag, payload)?;
    Ok((frame, total_len))
}

/// Constructs an error [`Frame`] with a message truncated to fit within
/// [`MAX_PAYLOAD_LEN`] on a UTF-8 character boundary.
pub fn error_frame(message: &str) -> Frame {
    if message.len() <= MAX_PAYLOAD_LEN {
        Frame::Error(message.to_string())
    } else {
        let mut cut = MAX_PAYLOAD_LEN;
        while !message.is_char_boundary(cut) {
            cut -= 1;
        }
        Frame::Error(message[..cut].to_string())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn round_trip_for_each_of_the_four_frames() {
        let frames = vec![
            Frame::Subscribe {
                offset: 123_456_789,
            },
            Frame::Output(b"binary \x00\xff console output".to_vec()),
            Frame::Detach,
            Frame::Error("something failed".to_string()),
        ];

        for frame in &frames {
            let encoded = encode(frame).expect("encode should succeed");

            // Test decode_slice
            let (decoded_slice, consumed) =
                decode_slice(&encoded).expect("decode_slice should succeed");
            assert_eq!(&decoded_slice, frame);
            assert_eq!(consumed, encoded.len());

            // Test decode_from
            let mut cursor = std::io::Cursor::new(&encoded);
            let decoded_from = decode_from(&mut cursor)
                .expect("decode_from should succeed")
                .expect("frame should be present");
            assert_eq!(&decoded_from, frame);
        }
    }

    #[test]
    fn subscribe_offset_little_endian_bytes_exact() {
        let frame = Frame::Subscribe { offset: 1 };
        let encoded = encode(&frame).expect("encode should succeed");
        let expected = [
            14, 0, 0, 0, b'S', b'U', b'B', b'S', b'C', b'R', 1, 0, 0, 0, 0, 0, 0, 0,
        ];
        assert_eq!(encoded, expected);
    }

    #[test]
    fn max_size_output_and_oversize_on_encode() {
        // 65,529 bytes payload round trips
        let max_payload = vec![b'a'; MAX_PAYLOAD_LEN];
        let frame = Frame::Output(max_payload);
        let encoded = encode(&frame).expect("max-size output should encode");
        assert_eq!(encoded.len(), 4 + TAG_LEN + MAX_PAYLOAD_LEN);
        let (decoded, consumed) = decode_slice(&encoded).expect("max-size output should decode");
        assert_eq!(decoded, frame);
        assert_eq!(consumed, encoded.len());

        // 65,530 bytes payload is Oversize on encode
        let oversize_payload = vec![b'a'; MAX_PAYLOAD_LEN + 1];
        let oversize_frame = Frame::Output(oversize_payload);
        assert_eq!(
            encode(&oversize_frame),
            Err(FrameError::Oversize {
                len: TAG_LEN + MAX_PAYLOAD_LEN + 1
            })
        );
    }

    struct HeaderOnlyReader {
        data: [u8; 4],
        pos: usize,
    }

    impl HeaderOnlyReader {
        fn new(data: [u8; 4]) -> Self {
            Self { data, pos: 0 }
        }
    }

    impl Read for HeaderOnlyReader {
        fn read(&mut self, buf: &mut [u8]) -> std::io::Result<usize> {
            if self.pos >= self.data.len() {
                return Err(std::io::Error::other("read past 4 header bytes"));
            }
            let available = self.data.len() - self.pos;
            let n = available.min(buf.len());
            buf[..n].copy_from_slice(&self.data[self.pos..self.pos + n]);
            self.pos += n;
            Ok(n)
        }
    }

    #[test]
    fn decode_length_65536_is_oversize_without_reading_body() {
        let len_bytes = 65_536u32.to_le_bytes();
        let mut reader = HeaderOnlyReader::new(len_bytes);
        assert_eq!(
            decode_from(&mut reader),
            Err(FrameError::Oversize { len: 65_536 })
        );
    }

    #[test]
    fn length_five_is_oversize() {
        let mut data = 5u32.to_le_bytes().to_vec();
        data.extend_from_slice(b"12345");

        assert_eq!(decode_slice(&data), Err(FrameError::Oversize { len: 5 }));

        let mut cursor = std::io::Cursor::new(data);
        assert_eq!(
            decode_from(&mut cursor),
            Err(FrameError::Oversize { len: 5 })
        );
    }

    #[test]
    fn clean_eof_is_ok_none() {
        let mut cursor = std::io::Cursor::new(Vec::<u8>::new());
        assert_eq!(decode_from(&mut cursor), Ok(None));
    }

    #[test]
    fn eof_mid_frame_is_truncated() {
        // Truncated during length header
        let partial_header = vec![14u8, 0];
        let mut cursor = std::io::Cursor::new(partial_header.clone());
        assert_eq!(decode_from(&mut cursor), Err(FrameError::Truncated));
        assert_eq!(decode_slice(&partial_header), Err(FrameError::Truncated));

        // Truncated during body
        let mut partial_body = 14u32.to_le_bytes().to_vec();
        partial_body.extend_from_slice(b"SUB");
        let mut cursor2 = std::io::Cursor::new(partial_body.clone());
        assert_eq!(decode_from(&mut cursor2), Err(FrameError::Truncated));
        assert_eq!(decode_slice(&partial_body), Err(FrameError::Truncated));
    }

    #[test]
    fn unknown_tag() {
        let tag = *b"UNKWNN";
        let mut data = 6u32.to_le_bytes().to_vec();
        data.extend_from_slice(&tag);

        assert_eq!(decode_slice(&data), Err(FrameError::UnknownTag(tag)));

        let mut cursor = std::io::Cursor::new(data);
        assert_eq!(decode_from(&mut cursor), Err(FrameError::UnknownTag(tag)));
    }

    #[test]
    fn bad_subscribe_payload_length() {
        // Subscribe with 4 bytes instead of 8
        let mut data = (TAG_LEN as u32 + 4).to_le_bytes().to_vec();
        data.extend_from_slice(&TAG_SUBSCRIBE);
        data.extend_from_slice(&[1, 2, 3, 4]);

        let expected = || FrameError::BadPayload {
            tag: TAG_SUBSCRIBE,
            reason: "subscribe payload must be exactly 8 bytes",
        };
        assert_eq!(decode_slice(&data), Err(expected()));

        let mut cursor = std::io::Cursor::new(data);
        assert_eq!(decode_from(&mut cursor), Err(expected()));
    }

    #[test]
    fn bad_detach_payload_length() {
        let mut data = (TAG_LEN as u32 + 1).to_le_bytes().to_vec();
        data.extend_from_slice(&TAG_DETACH);
        data.push(b'x');

        let expected = || FrameError::BadPayload {
            tag: TAG_DETACH,
            reason: "detach payload must be empty",
        };
        assert_eq!(decode_slice(&data), Err(expected()));

        let mut cursor = std::io::Cursor::new(data);
        assert_eq!(decode_from(&mut cursor), Err(expected()));
    }

    #[test]
    fn invalid_utf8_error_payload() {
        let mut data = (TAG_LEN as u32 + 2).to_le_bytes().to_vec();
        data.extend_from_slice(&TAG_ERROR);
        data.extend_from_slice(&[0xFF, 0xFE]);

        let expected = || FrameError::BadPayload {
            tag: TAG_ERROR,
            reason: "error payload must be valid UTF-8",
        };
        assert_eq!(decode_slice(&data), Err(expected()));

        let mut cursor = std::io::Cursor::new(data);
        assert_eq!(decode_from(&mut cursor), Err(expected()));
    }

    #[test]
    fn two_frames_back_to_back_decode_slice() {
        let f1 = Frame::Subscribe { offset: 99 };
        let f2 = Frame::Output(b"stream bytes".to_vec());
        let enc1 = encode(&f1).expect("enc1 should succeed");
        let enc2 = encode(&f2).expect("enc2 should succeed");

        let mut combined = enc1.clone();
        combined.extend_from_slice(&enc2);

        let (dec1, c1) = decode_slice(&combined).expect("first frame should decode");
        assert_eq!(dec1, f1);
        assert_eq!(c1, enc1.len());

        let (dec2, c2) = decode_slice(&combined[c1..]).expect("second frame should decode");
        assert_eq!(dec2, f2);
        assert_eq!(c2, enc2.len());
        assert_eq!(c1 + c2, combined.len());
    }

    #[test]
    fn input_and_resize_round_trip() {
        let frames = [
            Frame::Input(b"keys\r".to_vec()),
            Frame::Resize { cols: 80, rows: 24 },
        ];
        for frame in &frames {
            let encoded = encode(frame).expect("encode should succeed");
            let (decoded_slice, consumed) =
                decode_slice(&encoded).expect("decode_slice should succeed");
            assert_eq!(&decoded_slice, frame);
            assert_eq!(consumed, encoded.len());

            let mut cursor = std::io::Cursor::new(&encoded);
            assert_eq!(decode_from(&mut cursor), Ok(Some(frame.clone())));
        }
    }

    #[test]
    fn input_wire_bytes_exact() {
        let encoded = encode(&Frame::Input(b"x".to_vec())).expect("encode should succeed");
        let expected = [7, 0, 0, 0, b'I', b'N', b'P', b'U', b'T', b' ', b'x'];
        assert_eq!(encoded, expected);
    }

    #[test]
    fn resize_wire_bytes_exact_cols_then_rows_little_endian() {
        let encoded = encode(&Frame::Resize { cols: 80, rows: 24 }).expect("encode should succeed");
        let expected = [
            10, 0, 0, 0, b'R', b'E', b'S', b'I', b'Z', b'E', 80, 0, 24, 0,
        ];
        assert_eq!(encoded, expected);
    }

    #[test]
    fn bad_resize_payload_length() {
        let mut data = (TAG_LEN as u32 + 3).to_le_bytes().to_vec();
        data.extend_from_slice(&TAG_RESIZE);
        data.extend_from_slice(&[80, 0, 24]);

        let expected = || FrameError::BadPayload {
            tag: TAG_RESIZE,
            reason: "resize payload must be exactly 4 bytes",
        };
        assert_eq!(decode_slice(&data), Err(expected()));

        let mut cursor = std::io::Cursor::new(data);
        assert_eq!(decode_from(&mut cursor), Err(expected()));
    }

    #[test]
    fn error_frame_truncation_on_multibyte_char_boundary() {
        let mut s = "x".repeat(MAX_PAYLOAD_LEN - 1);
        s.push('€'); // 3 bytes in UTF-8
        let frame = error_frame(&s);
        match frame {
            Frame::Error(ref msg) => {
                assert_eq!(msg.len(), MAX_PAYLOAD_LEN - 1);
                assert_eq!(msg, &"x".repeat(MAX_PAYLOAD_LEN - 1));
                let encoded = encode(&frame).expect("truncated error should encode");
                let (decoded, _) = decode_slice(&encoded).expect("truncated error should decode");
                assert_eq!(decoded, frame);
            }
            _ => panic!("expected Frame::Error"),
        }
    }
}
