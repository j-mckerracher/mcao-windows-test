//! SHA-256 (FIPS 180-4) digests and audit hash formatting.
//!
//! The compression function comes from the pinned `sha2` crate (DPES-884 section 9). This module
//! wraps it in the streaming [`Sha256`] type and formats the `sha256:<64 lowercase hex>` audit hash.

use sha2::Digest;

/// Prefix of an audit hash string.
const AUDIT_PREFIX: &str = "sha256:";

/// Length of an audit hash string: the prefix plus 64 lowercase hexadecimal digits.
const AUDIT_HASH_LEN: usize = AUDIT_PREFIX.len() + 64;

/// Table of lowercase hexadecimal ASCII characters.
const HEX_CHARS: &[u8; 16] = b"0123456789abcdef";

/// Formats a 32-byte digest as a 64-character lowercase hexadecimal string.
pub(crate) fn format_hex(bytes: &[u8; 32]) -> String {
    let mut out = String::with_capacity(64);
    for &b in bytes {
        out.push(HEX_CHARS[(b >> 4) as usize] as char);
        out.push(HEX_CHARS[(b & 0x0f) as usize] as char);
    }
    out
}

/// A streaming SHA-256 (FIPS 180-4) hasher backed by [`sha2::Sha256`].
#[derive(Clone, Debug)]
pub struct Sha256 {
    inner: sha2::Sha256,
}

impl PartialEq for Sha256 {
    /// Two hashers are equal when they have been fed the same bytes.
    ///
    /// The inner hasher exposes no state comparison, so this compares the digests of copies.
    /// Equal digests stand for equal input; SHA-256 collisions are out of scope.
    fn eq(&self, other: &Self) -> bool {
        self.clone().finalize() == other.clone().finalize()
    }
}

impl Eq for Sha256 {}

impl Sha256 {
    /// Creates a new `Sha256` hasher initialized to the standard FIPS 180-4 initial state.
    pub fn new() -> Self {
        Self {
            inner: sha2::Sha256::new(),
        }
    }

    /// Updates the hasher state with the given chunk of `data`.
    ///
    /// Accepts chunks of any size, including empty slices.
    pub fn update(&mut self, data: &[u8]) {
        Digest::update(&mut self.inner, data);
    }

    /// Finalizes the hash computation and returns the 32-byte digest.
    #[must_use]
    pub fn finalize(self) -> [u8; 32] {
        self.inner.finalize().into()
    }
}

impl Default for Sha256 {
    /// Returns the default `Sha256` hasher, identical to [`Sha256::new`].
    fn default() -> Self {
        Self::new()
    }
}

/// Computes the SHA-256 digest of `data` in one shot.
#[must_use]
pub fn digest(data: &[u8]) -> [u8; 32] {
    let mut hasher = Sha256::new();
    hasher.update(data);
    hasher.finalize()
}

/// Computes the SHA-256 digest of `data` and returns it as a 64-character lowercase hex string.
#[must_use]
pub fn hex_digest(data: &[u8]) -> String {
    format_hex(&digest(data))
}

/// Computes the audit hash string (`sha256:<64 lowercase hex>`) for a canonical JSON line.
///
/// The line bytes must not include a trailing newline.
#[must_use]
pub fn audit_hash(line: &[u8]) -> String {
    let mut out = String::with_capacity(AUDIT_HASH_LEN);
    out.push_str(AUDIT_PREFIX);
    out.push_str(&hex_digest(line));
    out
}

/// Returns `true` if `text` matches the audit hash format (`sha256:[0-9a-f]{64}`).
#[must_use]
pub fn is_audit_hash(text: &str) -> bool {
    if text.len() != AUDIT_HASH_LEN {
        return false;
    }
    match text.strip_prefix(AUDIT_PREFIX) {
        Some(hex) => hex.bytes().all(|b| matches!(b, b'0'..=b'9' | b'a'..=b'f')),
        None => false,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_known_vectors() {
        assert_eq!(
            hex_digest(b""),
            "e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855"
        );
        assert_eq!(
            hex_digest(b"abc"),
            "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad"
        );
        assert_eq!(
            hex_digest(b"abcdbcdecdefdefgefghfghighijhijkijkljklmklmnlmnomnopnopq"),
            "248d6a61d20638b8e5c026930c3e6039a33ce45964ff2167f6ecedd419db06c1"
        );

        let million_a = vec![b'a'; 1_000_000];
        assert_eq!(
            hex_digest(&million_a),
            "cdc76e5c9914fb9281a1c7e284d73e67f1809a48a497200e046d39ccc7112cd0"
        );
    }

    #[test]
    fn test_audit_hash() {
        assert_eq!(
            audit_hash(b"abc"),
            "sha256:ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad"
        );
    }

    #[test]
    fn test_padding_boundaries() {
        let lengths = [0, 1, 54, 55, 56, 57, 63, 64, 65, 119, 120, 128];
        for &len in &lengths {
            let input = vec![b'x'; len];
            let one_shot_hex = hex_digest(&input);
            assert_eq!(one_shot_hex.len(), 64);

            let mut streaming = Sha256::new();
            for &byte in &input {
                streaming.update(&[byte]);
            }
            let streaming_hex = format_hex(&streaming.finalize());
            assert_eq!(
                streaming_hex, one_shot_hex,
                "streaming mismatch at length {len}"
            );
        }
    }

    #[test]
    fn test_56_byte_vector_splits() {
        let msg = b"abcdbcdecdefdefgefghfghighijhijkijkljklmklmnlmnomnopnopq";
        assert_eq!(msg.len(), 56);
        let expected = "248d6a61d20638b8e5c026930c3e6039a33ce45964ff2167f6ecedd419db06c1";
        for split in 0..=56 {
            let mut hasher = Sha256::new();
            hasher.update(&msg[..split]);
            hasher.update(&msg[split..]);
            let hex = format_hex(&hasher.finalize());
            assert_eq!(hex, expected, "Failed at split point {split}");
        }
    }

    #[test]
    fn test_is_audit_hash() {
        let valid = "sha256:ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad";
        assert!(is_audit_hash(valid));

        // Uppercase hex
        assert!(!is_audit_hash(
            "sha256:BA7816BF8F01CFEA414140DE5DAE2223B00361A396177A9CB410FF61F20015AD"
        ));
        assert!(!is_audit_hash(
            "sha256:ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015aD"
        ));

        // Wrong prefix
        assert!(!is_audit_hash(
            "sha512:ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad"
        ));
        assert!(!is_audit_hash(
            "sha256-ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad"
        ));
        assert!(!is_audit_hash(
            "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad"
        ));

        // 63 digits
        assert!(!is_audit_hash(
            "sha256:ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015a"
        ));

        // 65 digits
        assert!(!is_audit_hash(
            "sha256:ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad0"
        ));

        // Trailing newline
        let trailing_nl = format!("{valid}\n");
        assert!(!is_audit_hash(&trailing_nl));

        // Leading space
        let leading_space = format!(" {valid}");
        assert!(!is_audit_hash(&leading_space));

        // Trailing space
        let trailing_space = format!("{valid} ");
        assert!(!is_audit_hash(&trailing_space));

        // Empty string
        assert!(!is_audit_hash(""));

        // Non-hex characters
        assert!(!is_audit_hash(
            "sha256:zz7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad"
        ));
    }

    /// Repeats the lowercase alphabet to `len` bytes. The expected digests below come from
    /// `yes abcdefghijklmnopqrstuvwxyz | tr -d '\n' | head -c <len> | shasum -a 256`.
    fn alphabet(len: usize) -> Vec<u8> {
        b"abcdefghijklmnopqrstuvwxyz"
            .iter()
            .cycle()
            .take(len)
            .copied()
            .collect()
    }

    #[test]
    fn test_known_answers_around_padding_boundary() {
        let cases: [(usize, &str); 8] = [
            (
                55,
                "595615dbe4f0f407ae397d08b4c2cb870cb9b0e11937416f950c5160acf9c005",
            ),
            (
                57,
                "808f0738aa4401bdee842e5a15a7baad5809f976d8eb6f9bd2683cebd2e8d671",
            ),
            (
                63,
                "5ca3e1ef5207490eac01a795e5cc94d59582a5118bf9534665c8668d87aa647c",
            ),
            (
                64,
                "2fcd5a0d60e4c941381fcc4e00a4bf8be422c3ddfafb93c809e8d1e2bfffae8e",
            ),
            (
                65,
                "1b3cd1877ab2f2f19f7be001722554f336cb799df0329de0bb4c118dc6abc06d",
            ),
            (
                119,
                "faef67da856d6fd9c8d12f9ed0a4fefd3cf0ce085ab43e2907418d457e3c354b",
            ),
            (
                120,
                "c9512b08619c19fbb503c7da6b46ef20301e5f7a7a5f43989182398536f5c5c8",
            ),
            (
                128,
                "6c05be2c4268843ae47e68e611277ce62c02153f2f4d2e1e2a1a4b44f766cf74",
            ),
        ];
        for (len, expected) in cases {
            assert_eq!(hex_digest(&alphabet(len)), expected, "length {len}");
        }
    }

    #[test]
    fn test_known_answer_mixed_chunk_updates() {
        let input = alphabet(300);
        let expected = "ae1026abde5a8569f2c4aa0cb9fd79d9b4694194db665dd882c909b339f14056";
        // Later updates start with a non-empty buffer and span past the space left in it.
        let splits: [&[usize]; 5] = [
            &[10, 100, 190],
            &[60, 10, 63, 70, 97],
            &[1, 200, 99],
            &[64, 64, 172],
            &[100, 3, 197],
        ];
        for sizes in splits {
            let mut hasher = Sha256::new();
            let mut offset = 0;
            for &size in sizes {
                hasher.update(&input[offset..offset + size]);
                offset += size;
            }
            assert_eq!(offset, input.len());
            assert_eq!(
                format_hex(&hasher.finalize()),
                expected,
                "chunk sizes {sizes:?}"
            );
        }
    }

    #[test]
    fn test_is_audit_hash_accepts_full_hex_alphabet() {
        assert!(is_audit_hash(&format!(
            "sha256:{}",
            "0123456789abcdef".repeat(4)
        )));
    }

    #[test]
    fn test_is_audit_hash_rejects_neighbours_of_hex_ranges() {
        let valid = "sha256:ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad";
        let first_digit = "sha256:".len();
        let last_digit = valid.len() - 1;
        for bad in ['/', ':', '`', 'g', '@', 'G'] {
            for pos in [first_digit, last_digit] {
                let mut text = valid.to_string();
                text.replace_range(pos..=pos, &bad.to_string());
                assert!(!is_audit_hash(&text), "accepted {bad:?} at byte {pos}");
            }
        }
    }

    #[test]
    fn test_default_equals_new() {
        assert_eq!(Sha256::default(), Sha256::new());
    }

    #[test]
    fn test_partial_eq_tracks_bytes_seen() {
        let mut short = Sha256::new();
        let mut long = Sha256::new();
        short.update(b"ab");
        long.update(b"a");
        assert_ne!(short, long);
        long.update(b"b");
        assert_eq!(short, long);
    }

    #[test]
    fn test_incremental_update_matches_one_shot_for_every_two_way_split() {
        // 130 bytes span two full blocks plus a tail, so the splits cover every buffer fill level.
        let input = alphabet(130);
        let expected = digest(&input);
        for split in 0..=input.len() {
            let mut hasher = Sha256::new();
            hasher.update(&input[..split]);
            hasher.update(&input[split..]);
            assert_eq!(hasher.finalize(), expected, "split at {split}");
        }
    }

    #[test]
    fn test_empty_updates_leave_digest_unchanged() {
        let input = alphabet(70);
        let mut hasher = Sha256::new();
        hasher.update(&[]);
        hasher.update(&input[..1]);
        hasher.update(&[]);
        hasher.update(&input[1..64]);
        hasher.update(&[]);
        hasher.update(&input[64..]);
        hasher.update(&[]);
        assert_eq!(hasher.finalize(), digest(&input));
    }

    #[test]
    fn test_clone_mid_stream_forks_independent_states() {
        // 100 bytes leave 36 bytes buffered, so both forks resume from a partial block.
        let prefix = alphabet(100);
        let mut base = Sha256::new();
        base.update(&prefix);

        let mut left = base.clone();
        left.update(b"-left");
        let mut right = base;
        right.update(b"-right");

        let mut left_input = prefix.clone();
        left_input.extend_from_slice(b"-left");
        let mut right_input = prefix;
        right_input.extend_from_slice(b"-right");
        assert_eq!(left.finalize(), digest(&left_input));
        assert_eq!(right.finalize(), digest(&right_input));
    }

    #[test]
    fn test_audit_hash_is_prefix_and_hex_digest_for_every_line_length() {
        for len in 0..=130 {
            let line = alphabet(len);
            let hash = audit_hash(&line);
            assert_eq!(
                hash,
                format!("sha256:{}", hex_digest(&line)),
                "length {len}"
            );
            assert!(is_audit_hash(&hash), "length {len}");
        }
    }
}
