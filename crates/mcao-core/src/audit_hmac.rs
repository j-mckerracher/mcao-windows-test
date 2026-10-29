//! HMAC-SHA256 and canonical JSON digests for the audit sanitizer.
//!
//! SHA-256 comes from [`crate::sha256`] and canonical JSON from [`crate::canonical_json`]. This
//! module adds HMAC-SHA256 (RFC 2104), home-key parsing, and the `hmac-sha256:` digest string.
//! The digest matches the Python reference `digest()` in `bin/fm-audit-sanitize.py`, which hashes
//! `json.dumps(value, sort_keys=True, separators=(",", ":"), ensure_ascii=True)`, for every value
//! this module accepts. Values it refuses are listed under Known limits.
//!
//! # Known limits
//!
//! Without `serde_json`'s `arbitrary_precision` feature, an integer literal outside the `i64` and
//! `u64` ranges parses as an `f64`. A parsed [`serde_json::Value`] cannot tell that integer from a
//! float literal of the same magnitude, and Python keeps the integer exact. This module therefore
//! refuses every `f64` at or beyond `2^64` or at or below `-2^63` with
//! [`DigestError::AmbiguousInteger`] instead of producing a digest that may differ from the
//! reference. The refusal also covers float literals in those magnitudes.
//!
//! Negative zero has the same problem in a different form. serde_json reads the integer literal `-0`
//! as the float `-0.0`, while Python reads `-0` as the integer `0` and `-0.0` as the float `-0.0`.
//! A parsed `Value` does not keep the source literal, and preserving it would need serde_json's
//! `arbitrary_precision` feature, which is workspace-wide and changes `Number` for every consumer.
//! This module therefore refuses every negative-zero `f64` with [`DigestError::NegativeZero`],
//! including the float literal `-0.0`, until DPES-957 resolves literal provenance. Real `-0.0`
//! values are refused for now, not accepted.
//!
//! NaN and infinity cannot appear in a `serde_json::Value`, so they are outside this module's input.
//!
//! # Parse boundary
//!
//! This module takes a parsed [`serde_json::Value`], so rules about the source text run before it.
//! With this crate's `serde_json` features, parsing refuses three inputs that the Python reference
//! handles differently: an integer literal beyond `f64` range (a 401-digit literal fails with
//! `number out of range`), a float literal that overflows to infinity such as `1e400`, and a lone
//! surrogate escape such as `"\ud800"`. Python keeps the exact integer, parses `1e400` to infinity
//! without refusing it, and drops a lone surrogate as a member under `docs/audit-sanitization.md`.
//! The caller must decide those outcomes when the sanitizer port is wired (DPES-884). This module
//! never sees those inputs.
//!
//! Duplicate object keys are also invisible in a `Value`: `serde_json` keeps the last value, while
//! Python's event loader refuses duplicate keys. A caller must refuse duplicates while parsing.

use std::error::Error;
use std::fmt;

use crate::canonical_json::to_canonical_string;
use crate::sha256::{self, Sha256};

/// Errors that can occur when computing a canonical JSON digest.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub enum DigestError {
    /// A number lies outside the exact `i64` and `u64` range, so its canonical text cannot be
    /// produced faithfully.
    AmbiguousInteger,
    /// A number is negative zero. A parsed `Value` cannot tell the integer literal `-0` from the
    /// float `-0.0`, and Python serializes them differently.
    NegativeZero,
}

impl fmt::Display for DigestError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::AmbiguousInteger => write!(
                f,
                "numbers outside the exact i64 and u64 range are not supported in digest values"
            ),
            Self::NegativeZero => write!(f, "negative zero is not supported in digest values"),
        }
    }
}

impl Error for DigestError {}

/// `2^64`. Integer literals at or above this value do not fit `u64` and parse as `f64`.
const TWO_POW_64: f64 = 18_446_744_073_709_551_616.0;

/// `2^63`. Integer literals at or below `-2^63 - 1` do not fit `i64` and parse as `f64` rounded to this value.
const TWO_POW_63: f64 = 9_223_372_036_854_775_808.0;

/// Computes the HMAC-SHA256 message authentication code of `message` using `key` according to RFC 2104.
#[must_use]
pub fn hmac_sha256(key: &[u8], message: &[u8]) -> [u8; 32] {
    let mut key_block = [0u8; 64];
    if key.len() > 64 {
        key_block[..32].copy_from_slice(&sha256::digest(key));
    } else {
        key_block[..key.len()].copy_from_slice(key);
    }

    let mut ipad = [0u8; 64];
    let mut opad = [0u8; 64];
    for i in 0..64 {
        ipad[i] = key_block[i] ^ 0x36;
        opad[i] = key_block[i] ^ 0x5c;
    }

    let mut inner = Sha256::new();
    inner.update(&ipad);
    inner.update(message);
    let inner_hash = inner.finalize();

    let mut outer = Sha256::new();
    outer.update(&opad);
    outer.update(&inner_hash);
    outer.finalize()
}

/// Parses an operational home key from a 64-character lowercase hexadecimal string.
///
/// Returns `None` if `text` is not exactly 64 characters or contains any character outside `0-9` and `a-f`.
pub fn parse_home_key(text: &str) -> Option<[u8; 32]> {
    let bytes = text.as_bytes();
    if bytes.len() != 64 {
        return None;
    }
    let mut out = [0u8; 32];
    for (i, pair) in bytes.chunks_exact(2).enumerate() {
        let hi = match pair[0] {
            b'0'..=b'9' => pair[0] - b'0',
            b'a'..=b'f' => pair[0] - b'a' + 10,
            _ => return None,
        };
        let lo = match pair[1] {
            b'0'..=b'9' => pair[1] - b'0',
            b'a'..=b'f' => pair[1] - b'a' + 10,
            _ => return None,
        };
        out[i] = (hi << 4) | lo;
    }
    Some(out)
}

/// Refuses numbers whose exact integer value a parsed [`serde_json::Value`] cannot recover.
fn check_representable(value: &serde_json::Value) -> Result<(), DigestError> {
    match value {
        serde_json::Value::Number(number) => check_number(number),
        serde_json::Value::Array(items) => items.iter().try_for_each(check_representable),
        serde_json::Value::Object(map) => map.values().try_for_each(check_representable),
        serde_json::Value::Null | serde_json::Value::Bool(_) | serde_json::Value::String(_) => {
            Ok(())
        }
    }
}

fn check_number(number: &serde_json::Number) -> Result<(), DigestError> {
    if number.is_u64() || number.is_i64() {
        return Ok(());
    }
    // Only `f64` numbers remain: finite float literals, and integer literals outside i64 and u64.
    match number.as_f64() {
        Some(f) if f == 0.0 && f.is_sign_negative() => Err(DigestError::NegativeZero),
        Some(f) if f >= TWO_POW_64 || f <= -TWO_POW_63 => Err(DigestError::AmbiguousInteger),
        _ => Ok(()),
    }
}

/// Serializes a JSON value into canonical form matching Python's
/// `json.dumps(v, sort_keys=True, separators=(",", ":"), ensure_ascii=True)`.
///
/// Finite floats use Python's `repr` form.
///
/// Returns `Err(DigestError::AmbiguousInteger)` if a number lies outside the exact `i64` and `u64`
/// range, and `Err(DigestError::NegativeZero)` for any negative zero; see the module's known limits.
///
/// When a value holds more than one unsupported number, the error comes from the first one the
/// traversal reaches. Array elements are visited in document order. Object members are visited in
/// `serde_json::Value` map order, which is sorted by key because this crate does not enable
/// `serde_json`'s `preserve_order` feature, so the reported error can differ from source order.
/// One number cannot be both negative zero and out of range, so the order of the two checks within
/// a single number does not matter.
pub fn canonical_json(value: &serde_json::Value) -> Result<String, DigestError> {
    check_representable(value)?;
    Ok(to_canonical_string(value))
}

/// Computes the HMAC-SHA256 digest over the canonical JSON representation of `value`.
///
/// Returns `"hmac-sha256:<hex>"` where `<hex>` is the lowercase hexadecimal digest.
///
/// Returns the same errors as [`canonical_json`].
pub fn digest_value(home_key: &[u8], value: &serde_json::Value) -> Result<String, DigestError> {
    let canonical = canonical_json(value)?;
    let hmac = hmac_sha256(home_key, canonical.as_bytes());
    let mut out = String::with_capacity(12 + 64);
    out.push_str("hmac-sha256:");
    out.push_str(&sha256::format_hex(&hmac));
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn parse(text: &str) -> serde_json::Value {
        serde_json::from_str(text).expect("test JSON must parse")
    }

    #[test]
    fn test_sha256() {
        assert_eq!(
            sha256::format_hex(&sha256::digest(b"")),
            "e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855"
        );
        assert_eq!(
            sha256::format_hex(&sha256::digest(b"abc")),
            "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad"
        );
        assert_eq!(
            sha256::format_hex(&sha256::digest(
                b"abcdbcdecdefdefgefghfghighijhijkijkljklmklmnlmnomnopnopq"
            )),
            "248d6a61d20638b8e5c026930c3e6039a33ce45964ff2167f6ecedd419db06c1"
        );
        assert_eq!(
            sha256::format_hex(&sha256::digest(&vec![b'a'; 1_000_000])),
            "cdc76e5c9914fb9281a1c7e284d73e67f1809a48a497200e046d39ccc7112cd0"
        );
        assert_eq!(
            sha256::format_hex(&sha256::digest(&[b'a'; 55])),
            "9f4390f8d30c2dd92ec9f095b65e2b9ae9b0a925a5258e241c9f1e910f734318"
        );
        assert_eq!(
            sha256::format_hex(&sha256::digest(&[b'a'; 56])),
            "b35439a4ac6f0948b6d6f9e3c6af0f5f590ce20f1bde7090ef7970686ec6738a"
        );
        assert_eq!(
            sha256::format_hex(&sha256::digest(&[b'a'; 64])),
            "ffe054fe7ae0cb6dc65c3af9b61d5209f439851db43d0ba5997337df154668eb"
        );
    }

    #[test]
    fn test_hmac_sha256() {
        assert_eq!(
            sha256::format_hex(&hmac_sha256(&[0x0b; 20], b"Hi There")),
            "b0344c61d8db38535ca8afceaf0bf12b881dc200c9833da726e9376c2e32cff7"
        );
        assert_eq!(
            sha256::format_hex(&hmac_sha256(b"Jefe", b"what do ya want for nothing?")),
            "5bdcc146bf60754e6a042426089575c75a003f089d2739839dec58b964ec3843"
        );
        assert_eq!(
            sha256::format_hex(&hmac_sha256(&[0xaa; 20], &[0xdd; 50])),
            "773ea91e36800e46854db8ebd09181a72959098b3ef8c122d9635514ced565fe"
        );
        let key_1_to_25: Vec<u8> = (1..=25).collect();
        assert_eq!(
            sha256::format_hex(&hmac_sha256(&key_1_to_25, &[0xcd; 50])),
            "82558a389a443c0ea4cc819899f2083a85f0faa3e578f8077a2e3ff46729665b"
        );
        assert_eq!(
            sha256::format_hex(&hmac_sha256(
                &[0xaa; 131],
                b"Test Using Larger Than Block-Size Key - Hash Key First"
            )),
            "60e431591ee0b67f0d8a26aacbf5b77f8e0bc6213728c5140546040f0ee37f54"
        );
        assert_eq!(
            sha256::format_hex(&hmac_sha256(
                &[0xaa; 131],
                b"This is a test using a larger than block-size key and a larger than block-size data. The key needs to be hashed before being used by the HMAC algorithm."
            )),
            "9b09ffa71b942fcb27635fbcd5b0e944bfdc63644f0713938a7f51535c3a35e2"
        );
    }

    #[test]
    fn test_parse_home_key() {
        assert_eq!(parse_home_key(&"a".repeat(64)), Some([0xaa; 32]));
        assert_eq!(parse_home_key(&"A".repeat(64)), None);
        assert_eq!(parse_home_key(&"a".repeat(63)), None);
        assert_eq!(parse_home_key(&"a".repeat(65)), None);
        assert_eq!(parse_home_key(&format!("{}\n", "a".repeat(64))), None);
        assert_eq!(parse_home_key(&format!("{}g", "a".repeat(63))), None);
        assert_eq!(parse_home_key(""), None);
    }

    #[test]
    fn test_canonical_json() {
        let v1: serde_json::Value = serde_json::json!(["a", "b"]);
        assert_eq!(canonical_json(&v1).unwrap(), "[\"a\",\"b\"]");

        let v2: serde_json::Value = serde_json::json!({"b": 1, "a": [true, null, -5]});
        assert_eq!(
            canonical_json(&v2).unwrap(),
            "{\"a\":[true,null,-5],\"b\":1}"
        );

        let v3: serde_json::Value =
            serde_json::Value::String("héllo 😀 \u{007f} \u{001f} \"\\ \n\t\x08\x0c\r".to_string());
        assert_eq!(
            canonical_json(&v3).unwrap(),
            "\"h\\u00e9llo \\ud83d\\ude00 \\u007f \\u001f \\\"\\\\ \\n\\t\\b\\f\\r\""
        );

        let v4: serde_json::Value = serde_json::json!({"é": 1, "z": 2, "a": {}});
        assert_eq!(
            canonical_json(&v4).unwrap(),
            "{\"a\":{},\"z\":2,\"\\u00e9\":1}"
        );

        let v5: serde_json::Value = serde_json::json!([]);
        assert_eq!(canonical_json(&v5).unwrap(), "[]");

        let v6: serde_json::Value = serde_json::json!({});
        assert_eq!(canonical_json(&v6).unwrap(), "{}");

        let v7: serde_json::Value = serde_json::json!(0);
        assert_eq!(canonical_json(&v7).unwrap(), "0");

        let v8: serde_json::Value = serde_json::json!("");
        assert_eq!(canonical_json(&v8).unwrap(), "\"\"");

        let v9: serde_json::Value = serde_json::json!(u64::MAX);
        assert_eq!(canonical_json(&v9).unwrap(), "18446744073709551615");

        let v10: serde_json::Value = serde_json::json!(1.5);
        assert_eq!(canonical_json(&v10).unwrap(), "1.5");

        let v11: serde_json::Value = serde_json::json!([{"a": 2.0}]);
        assert_eq!(canonical_json(&v11).unwrap(), "[{\"a\":2.0}]");
    }

    #[test]
    fn test_canonical_json_finite_floats_match_python_repr() {
        assert_eq!(
            canonical_json(&parse("[0.1, 2.0, 1e16, 1e-5, -1.5, 5e-324, 1e15, 0.0001]")).unwrap(),
            "[0.1,2.0,1e+16,1e-05,-1.5,5e-324,1000000000000000.0,0.0001]"
        );
        assert_eq!(
            canonical_json(&parse("[2.2250738585072014e-308, 5e-324]")).unwrap(),
            "[2.2250738585072014e-308,5e-324]"
        );
        assert_eq!(
            canonical_json(&parse(r#"{"x":15198712109.619461}"#)).unwrap(),
            r#"{"x":15198712109.619461}"#
        );
        assert_eq!(
            canonical_json(&parse(r#"{"x":7.120236347223045e-307}"#)).unwrap(),
            r#"{"x":7.120236347223045e-307}"#
        );
    }

    #[test]
    fn test_canonical_json_nested_values() {
        let value = parse(r#"{"outer":{"b":[{"z":0.0,"a":1}],"a":2.5},"k":[[[3.25]]]}"#);
        assert_eq!(
            canonical_json(&value).unwrap(),
            r#"{"k":[[[3.25]]],"outer":{"a":2.5,"b":[{"a":1,"z":0.0}]}}"#
        );
    }

    #[test]
    fn test_canonical_json_positive_zero() {
        assert_eq!(canonical_json(&parse("0.0")).unwrap(), "0.0");
        assert_eq!(canonical_json(&parse("0")).unwrap(), "0");
    }

    #[test]
    fn test_canonical_json_refuses_negative_zero() {
        // Real float -0.0 is refused as well as the integer literal -0 until DPES-957 resolves
        // literal provenance. Refused, not accepted. Parity gap: Python digests -0.0 as `-0.0`,
        // so this module does not match the reference here; it does not claim parity.
        assert_eq!(
            canonical_json(&parse("-0.0")),
            Err(DigestError::NegativeZero)
        );
        assert_eq!(canonical_json(&parse("-0")), Err(DigestError::NegativeZero));
        assert_eq!(
            canonical_json(&parse(r#"{"a":[{"b":-0.0}]}"#)),
            Err(DigestError::NegativeZero)
        );
    }

    #[test]
    fn test_canonical_json_integer_edges() {
        assert_eq!(
            canonical_json(&parse(
                r#"{"min":-9223372036854775808,"max":18446744073709551615,"neg1":-1,"zero":0}"#
            ))
            .unwrap(),
            r#"{"max":18446744073709551615,"min":-9223372036854775808,"neg1":-1,"zero":0}"#
        );
        assert_eq!(
            canonical_json(&parse("[18446744073709551615, 1.0, -9223372036854775808]")).unwrap(),
            "[18446744073709551615,1.0,-9223372036854775808]"
        );
    }

    #[test]
    fn test_canonical_json_refuses_integers_outside_exact_range() {
        // 2^64 is one past u64::MAX; serde_json parses it as f64.
        assert_eq!(
            canonical_json(&parse("18446744073709551616")),
            Err(DigestError::AmbiguousInteger)
        );
        // One below i64::MIN; serde_json parses it as f64 rounded to -2^63.
        assert_eq!(
            canonical_json(&parse("-9223372036854775809")),
            Err(DigestError::AmbiguousInteger)
        );
        // Refused nested, so no digest is produced for a partly exact value.
        assert_eq!(
            canonical_json(&parse(
                r#"{"a":[{"b":340282366920938463463374607431768211456}]}"#
            )),
            Err(DigestError::AmbiguousInteger)
        );
        // Float literals in the same magnitude are refused too; fail closed.
        assert_eq!(
            canonical_json(&parse("1e20")),
            Err(DigestError::AmbiguousInteger)
        );
        assert_eq!(
            canonical_json(&parse("1.7976931348623157e308")),
            Err(DigestError::AmbiguousInteger)
        );
    }

    #[test]
    fn test_digest_value() {
        let mut key = [0u8; 32];
        key[31] = 0x01;

        let v1: serde_json::Value = serde_json::json!(["a", "b"]);
        assert_eq!(
            digest_value(&key, &v1).unwrap(),
            "hmac-sha256:2f0024b434487539764c7a728f62e5d6a7414b9ba539cb9882e8c7129fc3de83"
        );

        let v2: serde_json::Value = serde_json::json!({"b": 1, "a": [true, null, -5]});
        assert_eq!(
            digest_value(&key, &v2).unwrap(),
            "hmac-sha256:f8fb1db951f3d5600c587c9589c3f15b3d4f123f028587d31913ea90ade99d1c"
        );

        let v3: serde_json::Value =
            serde_json::Value::String("héllo 😀 \u{007f} \u{001f} \"\\ \n\t\x08\x0c\r".to_string());
        assert_eq!(
            digest_value(&key, &v3).unwrap(),
            "hmac-sha256:219a6da2350681cbccdc752b972064555e40495c560dbd7a670e49d3da50eb98"
        );

        let v4: serde_json::Value = serde_json::json!({"é": 1, "z": 2, "a": {}});
        assert_eq!(
            digest_value(&key, &v4).unwrap(),
            "hmac-sha256:42efb98352ecd7e5902a942ee364d08dcca70c318432d213f1090df9c53f27d8"
        );

        let v5: serde_json::Value = serde_json::json!([]);
        assert_eq!(
            digest_value(&key, &v5).unwrap(),
            "hmac-sha256:1483454e7cbc6e1475e25bbbf7fcebb9dbf4c273cabb519137d7d316c39a5632"
        );

        let v6: serde_json::Value = serde_json::json!({});
        assert_eq!(
            digest_value(&key, &v6).unwrap(),
            "hmac-sha256:0ecd395123dc31feba83585533430c461f44a315776f7a2bb230db01c4af9a06"
        );

        let v7: serde_json::Value = serde_json::json!(0);
        assert_eq!(
            digest_value(&key, &v7).unwrap(),
            "hmac-sha256:0fddfb414d3172febefa135a5e42b3bfec6695435c87d034b200da8cce0e8f0b"
        );

        let v8: serde_json::Value = serde_json::json!("");
        assert_eq!(
            digest_value(&key, &v8).unwrap(),
            "hmac-sha256:58e8a3404cd6e7e6d012ff989235f260d2eea0bbf9fc5c6160c6bf57a96ba0f1"
        );

        let v9: serde_json::Value = serde_json::json!(1.5);
        assert_eq!(
            digest_value(&key, &v9).unwrap(),
            "hmac-sha256:12a3446cb83622db993940e0dc8f84f17b92a03494bab584969cec7ca5a18d9c"
        );

        let v10: serde_json::Value = serde_json::json!([{"a": 2.0}]);
        assert_eq!(
            digest_value(&key, &v10).unwrap(),
            "hmac-sha256:e8988d9fdfa99386da13260344d6d3e82ce339f04456853ff15a3bd091fc8147"
        );
    }

    #[test]
    fn test_digest_value_matches_python_reference() {
        // Expected values: Python 3.14 `hmac.new(key, json.dumps(v, sort_keys=True,
        // separators=(",", ":"), ensure_ascii=True), sha256)` with key = 31 zero bytes then 0x01.
        // These are reference-implementation outputs, not the DPES-803 QA baseline fixture.
        // Negative zero is omitted: it is refused until DPES-957 resolves literal provenance.
        let mut key = [0u8; 32];
        key[31] = 0x01;

        let cases: [(&str, &str); 8] = [
            (
                r#"{"x": 1.5}"#,
                "hmac-sha256:967fe49ec52a7633cc3cbfb7a30c68476feb495af3a8c2bfb0b0256673ac62de",
            ),
            (
                "[0.1, 2.0, 1e16, 1e-5, -1.5, 5e-324, 1e15, 0.0001]",
                "hmac-sha256:282af7c50c2b54ee1714ec8ecbcd0cb60d34f06c2454c8c77d7fbfc5094c90c0",
            ),
            (
                r#"{"outer":{"b":[{"z":0.0,"a":1}],"a":2.5},"k":[[[3.25]]]}"#,
                "hmac-sha256:3974934788626ad4937c6fc9651f743a06dd111c997e5d6cd8d1fcfc2e918481",
            ),
            (
                "0.0",
                "hmac-sha256:4e0bcbbac7b0b9ec616edafe7e5f34a1f35e6f18cff248367a5d3b95b6a64721",
            ),
            (
                r#"{"min":-9223372036854775808,"max":18446744073709551615,"neg1":-1,"zero":0}"#,
                "hmac-sha256:96e8b117aae63ddc5a386c859f1f54569c9cb1c6e61a9a84df164b00b98a14c3",
            ),
            (
                "[18446744073709551615, 1.0, -9223372036854775808]",
                "hmac-sha256:364d26e3fe8e0e63d13b57c0b5cd6cea27c370b0615ac832e53877c7f3418860",
            ),
            (
                r#"{"x":15198712109.619461}"#,
                "hmac-sha256:b1ccc60cf0a26d6930d87160d296bc772ed0e396fed3adb881d3ded2fb6f6665",
            ),
            (
                r#"{"x":7.120236347223045e-307}"#,
                "hmac-sha256:171f321474d45b23e563c14b601caf32a099cd27fa88d929206464f1662e6c35",
            ),
        ];
        for (text, expected) in cases {
            assert_eq!(
                digest_value(&key, &parse(text)).unwrap(),
                expected,
                "digest mismatch for {text}"
            );
        }
    }

    #[test]
    fn test_digest_value_refuses_negative_zero() {
        let key = [0u8; 32];
        assert_eq!(
            digest_value(&key, &parse("-0.0")),
            Err(DigestError::NegativeZero)
        );
        assert_eq!(
            digest_value(&key, &parse("-0")),
            Err(DigestError::NegativeZero)
        );
        assert!(digest_value(&key, &parse("0.0")).is_ok());
    }

    #[test]
    fn test_parsed_home_key_digest_known_answer() {
        // Home key from its 64-character hex form, through digest_value; the expected value is the
        // Python reference for ["a","b"] with the same 32-byte key.
        let key = parse_home_key(&format!("{}01", "00".repeat(31))).expect("valid home key");
        assert_eq!(
            digest_value(&key, &parse(r#"["a","b"]"#)).unwrap(),
            "hmac-sha256:2f0024b434487539764c7a728f62e5d6a7414b9ba539cb9882e8c7129fc3de83"
        );
    }

    #[test]
    fn test_digest_value_refuses_ambiguous_integers() {
        let key = [0u8; 32];
        assert_eq!(
            digest_value(&key, &parse("18446744073709551616")),
            Err(DigestError::AmbiguousInteger)
        );
    }

    #[test]
    fn test_canonical_json_float_band_matches_python_reference() {
        // DPES-964 AC3: accepted floats between 2^53 and 2^64, plus the negative band above -2^63.
        // Expected text and digests come from the Python reference, not from this module:
        // `json.dumps(v, sort_keys=True, separators=(",", ":"), ensure_ascii=True)` and
        // `hmac.new(key, text, sha256)` with key = 31 zero bytes then 0x01.
        let mut key = [0u8; 32];
        key[31] = 0x01;
        let cases: [(&str, &str, &str); 11] = [
            // Tie between 2^53 and 2^53 + 2 rounds to the even neighbour 2^53 (rounded form).
            (
                "9007199254740993.0",
                "9007199254740992.0",
                "hmac-sha256:6cbd36538a9b1f038be45df35bb82312bfdd88e52ee80fd81d504537b492e587",
            ),
            // Exact, decimal form below 1e16.
            (
                "9007199254740994.0",
                "9007199254740994.0",
                "hmac-sha256:2a21c7871efd13452c97aa3a156eac07f1ef62b24777ec6e149edd7af45d7a25",
            ),
            // Tie between 2^53 + 2 and 2^53 + 4 rounds to the even neighbour 2^53 + 4.
            (
                "9007199254740995.0",
                "9007199254740996.0",
                "hmac-sha256:9f0d3a1e107a67c4c8949338c2332e906f1d7f1e84a74478eca84c91fc76394b",
            ),
            (
                "-9007199254740993.0",
                "-9007199254740992.0",
                "hmac-sha256:46ee1a15a616adacbec51ced199eb28273ade12c18afeb8cb8317e68711c5b80",
            ),
            // 2^63 - 1 as a float is 2^63; exponent form.
            (
                "9223372036854775807.0",
                "9.223372036854776e+18",
                "hmac-sha256:31a3fc0370005d4dbe61aa91283f1976d37677bbe043d2feedc89d2847f157d8",
            ),
            (
                "9223372036854775808.0",
                "9.223372036854776e+18",
                "hmac-sha256:31a3fc0370005d4dbe61aa91283f1976d37677bbe043d2feedc89d2847f157d8",
            ),
            (
                "-9.2e18",
                "-9.2e+18",
                "hmac-sha256:8b560d294fb0fba4dd33e6d7156c9f28371dc1fea52112a56a943caad58853f5",
            ),
            // Negative band edge: the next double above -2^63.
            (
                "-9223372036854774784.0",
                "-9.223372036854775e+18",
                "hmac-sha256:7117ac257abd5e419e1708936de6712085557afdabff3a9d3773bd8805e65f21",
            ),
            (
                "1e19",
                "1e+19",
                "hmac-sha256:0040dd7d46e56e84f81cc3093afe1ed65886d5f19d53e41b12d4df1cd0bd5d5a",
            ),
            (
                "12345678901234567890.0",
                "1.2345678901234567e+19",
                "hmac-sha256:2772783e311d91568a1ac0f0acee2eb03614e2714f2b24825a7ee0d91e6ee27a",
            ),
            // Largest double below 2^64.
            (
                "18446744073709549568.0",
                "1.844674407370955e+19",
                "hmac-sha256:7bfea82857368b978cb2e66bc60379f24f425d57ab3f6680498544ede7a9359d",
            ),
        ];
        for (text, canonical, digest) in cases {
            let value = parse(text);
            assert_eq!(
                canonical_json(&value).unwrap(),
                canonical,
                "canonical mismatch for {text}"
            );
            assert_eq!(
                digest_value(&key, &value).unwrap(),
                digest,
                "digest mismatch for {text}"
            );
        }
    }

    #[test]
    fn test_canonical_json_float_band_edges_refused() {
        // DPES-964 AC3 edges: 2^64 and anything rounding to it are refused.
        assert_eq!(
            canonical_json(&parse("18446744073709551615.0")),
            Err(DigestError::AmbiguousInteger)
        );
        assert_eq!(
            canonical_json(&parse("18446744073709551616.0")),
            Err(DigestError::AmbiguousInteger)
        );
        // -2^63 is refused; -9223372036854775807.0 rounds to -2^63 and is refused too.
        assert_eq!(
            canonical_json(&parse("-9223372036854775808.0")),
            Err(DigestError::AmbiguousInteger)
        );
        assert_eq!(
            canonical_json(&parse("-9223372036854775807.0")),
            Err(DigestError::AmbiguousInteger)
        );
    }

    #[test]
    fn test_digest_error_precedence_with_multiple_unsupported_numbers() {
        // DPES-964 AC2. The first unsupported number the traversal reaches decides the error.
        // Arrays are visited in document order.
        assert_eq!(
            canonical_json(&parse("[1e20, -0.0]")),
            Err(DigestError::AmbiguousInteger)
        );
        assert_eq!(
            canonical_json(&parse("[-0.0, 1e20]")),
            Err(DigestError::NegativeZero)
        );
        assert_eq!(
            canonical_json(&parse("[18446744073709551616, -0, 1e20]")),
            Err(DigestError::AmbiguousInteger)
        );
        // Objects are visited in serde_json map order, sorted by key, not in source order.
        assert_eq!(
            canonical_json(&parse(r#"{"b":1e20,"a":-0.0}"#)),
            Err(DigestError::NegativeZero)
        );
        assert_eq!(
            canonical_json(&parse(r#"{"a":1e20,"b":-0.0}"#)),
            Err(DigestError::AmbiguousInteger)
        );
        // Nested: "a" is visited before "z", so its array is reached first.
        assert_eq!(
            canonical_json(&parse(r#"{"z":[-0.0],"a":[{"y":1e20}]}"#)),
            Err(DigestError::AmbiguousInteger)
        );
        // A duplicate key leaves only its last value for the traversal to see.
        assert_eq!(
            canonical_json(&parse(r#"{"a":1e20,"a":-0.0}"#)),
            Err(DigestError::NegativeZero)
        );
    }

    #[test]
    fn test_parse_boundary_refusals_happen_before_this_module() {
        // DPES-964 AC4. serde_json refuses these at parse time, so no Value reaches this module.
        // Python parses the 401-digit integer exactly and parses 1e400 to infinity; see the module's
        // Parse boundary section.
        let huge_int = format!("1{}", "0".repeat(400));
        for (text, message) in [
            (huge_int.as_str(), "number out of range"),
            ("1e400", "number out of range"),
            ("-1e400", "number out of range"),
            ("\"\\ud800\"", "end of hex escape"),
            ("\"\\udc00\"", "lone leading surrogate"),
        ] {
            let error = serde_json::from_str::<serde_json::Value>(text)
                .expect_err("serde_json must refuse this input at parse time");
            assert!(
                error.to_string().contains(message),
                "unexpected parse error for {text:.12}: {error}"
            );
        }
    }

    #[test]
    fn test_duplicate_keys_collapse_before_this_module() {
        // serde_json keeps the last duplicate, so this module cannot see the first. Python's event
        // loader refuses duplicate keys; a caller must refuse them while parsing.
        assert_eq!(
            canonical_json(&parse(r#"{"a":1,"a":2}"#)).unwrap(),
            r#"{"a":2}"#
        );
    }

    #[test]
    fn test_digest_error_display() {
        assert_eq!(
            DigestError::AmbiguousInteger.to_string(),
            "numbers outside the exact i64 and u64 range are not supported in digest values"
        );
        assert_eq!(
            DigestError::NegativeZero.to_string(),
            "negative zero is not supported in digest values"
        );
    }
}
