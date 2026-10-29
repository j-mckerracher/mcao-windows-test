//! Python-compatible canonical JSON serializer.
//!
//! Produces the output of Python's
//! `json.dumps(value, sort_keys=True, separators=(",", ":"), ensure_ascii=True)`
//! for a [`serde_json::Value`]. Use [`try_to_canonical_string`] for input that has not already
//! been vetted; it is the only entry point that reports refused input as an error.
//!
//! # Input-boundary policy
//!
//! A parsed `Value` does not keep the source literal, so some numbers cannot be canonicalized to
//! the bytes Python produces. Those numbers are refused with a [`CanonicalJsonError`] instead of
//! being emitted with different bytes:
//!
//! - Integer literals outside `[i64::MIN, u64::MAX]`. Without serde_json's `arbitrary_precision`
//!   feature they parse as `f64`, while Python keeps them exact. Every `f64` at or beyond `2^64`
//!   or at or below `-2^63` is refused as [`CanonicalJsonError::AmbiguousInteger`]. This also
//!   refuses float literals of those magnitudes, because the parsed value cannot tell them apart.
//! - Negative zero. serde_json reads the integer literal `-0` as the float `-0.0`, but Python
//!   reads `-0` as the integer `0` and `-0.0` as the float `-0.0`. Every negative-zero `f64` is
//!   refused as [`CanonicalJsonError::NegativeZero`], including the float literal `-0.0`.
//!
//! Integers in `[i64::MIN, u64::MAX]` serialize exactly. Other finite floats use Python's `repr`
//! form, including the float tie, parser-precision and shortest-neighbour cases covered by tests.
//!
//! # Limits of the parsed input
//!
//! - serde_json rejects overflowing float literals such as `1e400` with `number out of range`
//!   during parsing, so non-finite numbers never reach this module. The Python reference accepts
//!   them as infinity; inputs containing them are outside this module's domain.
//! - serde_json rejects lone surrogate escapes such as `"\ud800"` during parsing, while Python
//!   accepts them. Such strings are outside this module's domain.
//!
//! The string escaping follows `ensure_ascii=True` and is exact for every valid Rust `str`.

use serde_json::{Number, Value};
use std::fmt;

/// Reason a value cannot be canonicalized to Python's bytes, or an internal invariant failed.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub enum CanonicalJsonError {
    /// NaN or infinity. A parsed `Value` cannot hold these; they reach the formatter only when
    /// called directly with such a float.
    NonFinite,
    /// A number outside the exact 64-bit range, whose parsed form cannot be told apart from an
    /// integer literal of the same magnitude. See the input-boundary policy.
    AmbiguousInteger,
    /// A negative zero, which Python reads as the integer `0` or the float `-0.0` depending on
    /// the source text. See the input-boundary policy.
    NegativeZero,
    /// An internal invariant failed. Inputs accepted by this module never reach this variant;
    /// it reports a broken assumption instead of emitting a fallback value.
    Invariant(&'static str),
}

impl fmt::Display for CanonicalJsonError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::NonFinite => write!(f, "non-finite float is not allowed"),
            Self::AmbiguousInteger => write!(
                f,
                "number outside the exact 64-bit integer range cannot be canonicalized exactly"
            ),
            Self::NegativeZero => write!(
                f,
                "negative zero cannot be canonicalized: Python reads integer -0 and float -0.0 differently"
            ),
            Self::Invariant(reason) => write!(f, "internal invariant violated: {reason}"),
        }
    }
}

impl std::error::Error for CanonicalJsonError {}

/// `2^63`, which is exactly representable as an `f64`.
const TWO_POW_63: f64 = 9_223_372_036_854_775_808.0;
/// `2^64`, which is exactly representable as an `f64`.
const TWO_POW_64: f64 = 18_446_744_073_709_551_616.0;

fn push_unicode_escape(out: &mut String, val: u16) {
    const HEX_DIGITS: &[u8; 16] = b"0123456789abcdef";
    out.push_str("\\u");
    out.push(HEX_DIGITS[((val >> 12) & 0x0f) as usize] as char);
    out.push(HEX_DIGITS[((val >> 8) & 0x0f) as usize] as char);
    out.push(HEX_DIGITS[((val >> 4) & 0x0f) as usize] as char);
    out.push(HEX_DIGITS[(val & 0x0f) as usize] as char);
}

/// Escapes a string slice according to Python `ensure_ascii=True` JSON formatting rules,
/// returning the string enclosed in double quotes.
fn escape_string(text: &str) -> String {
    let mut out = String::with_capacity(text.len() + 2);
    out.push('"');
    for c in text.chars() {
        match c {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            '\x08' => out.push_str("\\b"),
            '\x0C' => out.push_str("\\f"),
            c if c.is_ascii() => {
                let byte = c as u8;
                if byte < 0x20 || byte == 0x7f {
                    push_unicode_escape(&mut out, byte as u16);
                } else {
                    out.push(c);
                }
            }
            c => {
                let code = c as u32;
                if code <= 0xffff {
                    push_unicode_escape(&mut out, code as u16);
                } else {
                    let mut buf = [0u16; 2];
                    for &mut surrogate in c.encode_utf16(&mut buf) {
                        push_unicode_escape(&mut out, surrogate);
                    }
                }
            }
        }
    }
    out.push('"');
    out
}

/// Returns the decimals one unit either side of `scientific` at `digits` significant digits.
///
/// `scientific` is a `{:e}`-style string with exactly `digits` significant digits. A carry
/// across a power of ten keeps the digit count and moves the exponent. A string that is not in
/// that form is an invariant failure, not an empty result.
fn same_length_neighbours(
    scientific: &str,
    digits: usize,
) -> Result<Vec<String>, CanonicalJsonError> {
    let (mantissa, exponent) = scientific
        .split_once('e')
        .ok_or(CanonicalJsonError::Invariant(
            "scientific form has no exponent",
        ))?;
    let exponent: i32 = exponent
        .parse()
        .map_err(|_| CanonicalJsonError::Invariant("scientific form exponent is not an integer"))?;
    let base: u128 = mantissa
        .replace('.', "")
        .parse()
        .map_err(|_| CanonicalJsonError::Invariant("scientific form mantissa is not digits"))?;
    let low = 10u128.pow(digits as u32 - 1);
    let high = low * 10;
    let render = |units: u128, exponent: i32| {
        let digits = units.to_string();
        let (lead, rest) = digits.split_at(1);
        if rest.is_empty() {
            format!("{lead}e{exponent}")
        } else {
            format!("{lead}.{rest}e{exponent}")
        }
    };
    let mut out = Vec::with_capacity(2);
    if base + 1 == high {
        out.push(render(low, exponent + 1));
    } else {
        out.push(render(base + 1, exponent));
    }
    if let Some(below) = base.checked_sub(1) {
        if base == low {
            out.push(render(high - 1, exponent - 1));
        } else {
            out.push(render(below, exponent));
        }
    }
    Ok(out)
}

/// Returns the shortest scientific form of a finite, non-negative `f64` that parses back to it.
///
/// Python `repr` takes the shortest digit string that round-trips and, at that length, the
/// candidate nearest the exact value, with exact decimal ties going to even. At each digit
/// count this tries the correctly rounded candidate, then its two same-length neighbours:
/// the nearest decimal can miss the double while a neighbour hits it, and Rust's `{:e}`
/// breaks exact ties upward.
fn shortest_round_trip(abs_val: f64) -> Result<String, CanonicalJsonError> {
    let round_trips = |candidate: &str| candidate.parse::<f64>() == Ok(abs_val);
    for precision in 0..=16usize {
        let nearest = format!("{abs_val:.precision$e}");
        if round_trips(&nearest) {
            return Ok(nearest);
        }
        if let Some(neighbour) = same_length_neighbours(&nearest, precision + 1)?
            .into_iter()
            .find(|candidate| round_trips(candidate))
        {
            return Ok(neighbour);
        }
    }
    Err(CanonicalJsonError::Invariant(
        "no 17-digit form round-trips a finite f64",
    ))
}

/// Formats a 64-bit float according to Python's `repr(float)` semantics.
///
/// Returns [`CanonicalJsonError::NonFinite`] for NaN or infinity. This is the float formatter
/// only; it does not apply the integer-boundary policy in the module documentation.
fn format_float(value: f64) -> Result<String, CanonicalJsonError> {
    if !value.is_finite() {
        return Err(CanonicalJsonError::NonFinite);
    }

    let is_neg = value.is_sign_negative();
    let s = shortest_round_trip(value.abs())?;

    let (mantissa_str, exp_str) = s.split_once('e').ok_or(CanonicalJsonError::Invariant(
        "shortest form has no exponent",
    ))?;
    let x: i32 = exp_str
        .parse()
        .map_err(|_| CanonicalJsonError::Invariant("shortest form exponent is not an integer"))?;

    let d: String = mantissa_str.chars().filter(|&c| c != '.').collect();
    if d.is_empty() {
        return Err(CanonicalJsonError::Invariant("shortest form has no digits"));
    }

    let mut out = String::new();
    if is_neg {
        out.push('-');
    }

    if !(-4..16).contains(&x) {
        out.push_str(&d[..1]);
        if d.len() > 1 {
            out.push('.');
            out.push_str(&d[1..]);
        }
        out.push('e');
        if x >= 0 {
            out.push('+');
        } else {
            out.push('-');
        }
        let abs_x = x.unsigned_abs();
        if abs_x < 10 {
            out.push('0');
        }
        out.push_str(&abs_x.to_string());
    } else if x >= 0 {
        let x_plus_1 = (x + 1) as usize;
        if d.len() >= x_plus_1 {
            out.push_str(&d[..x_plus_1]);
            out.push('.');
            let rem = &d[x_plus_1..];
            if rem.is_empty() {
                out.push('0');
            } else {
                out.push_str(rem);
            }
        } else {
            out.push_str(&d);
            for _ in 0..(x_plus_1 - d.len()) {
                out.push('0');
            }
            out.push_str(".0");
        }
    } else {
        out.push_str("0.");
        let zeros = (-x - 1) as usize;
        for _ in 0..zeros {
            out.push('0');
        }
        out.push_str(&d);
    }

    Ok(out)
}

/// Formats one number under the input-boundary policy.
fn canonical_number(num: &Number) -> Result<String, CanonicalJsonError> {
    if let Some(u) = num.as_u64() {
        return Ok(u.to_string());
    }
    if let Some(i) = num.as_i64() {
        return Ok(i.to_string());
    }
    let f = num.as_f64().ok_or(CanonicalJsonError::Invariant(
        "number is neither an integer nor a float",
    ))?;
    if !f.is_finite() {
        return Err(CanonicalJsonError::NonFinite);
    }
    if f >= TWO_POW_64 || f <= -TWO_POW_63 {
        return Err(CanonicalJsonError::AmbiguousInteger);
    }
    if f == 0.0 && f.is_sign_negative() {
        return Err(CanonicalJsonError::NegativeZero);
    }
    format_float(f)
}

fn write_canonical(value: &Value, out: &mut String) -> Result<(), CanonicalJsonError> {
    match value {
        Value::Null => out.push_str("null"),
        Value::Bool(true) => out.push_str("true"),
        Value::Bool(false) => out.push_str("false"),
        Value::Number(num) => out.push_str(&canonical_number(num)?),
        Value::String(s) => out.push_str(&escape_string(s)),
        Value::Array(arr) => {
            out.push('[');
            for (i, elem) in arr.iter().enumerate() {
                if i > 0 {
                    out.push(',');
                }
                write_canonical(elem, out)?;
            }
            out.push(']');
        }
        Value::Object(map) => {
            out.push('{');
            let mut entries: Vec<(&String, &Value)> = map.iter().collect();
            entries.sort_by(|a, b| a.0.cmp(b.0));
            for (i, (k, v)) in entries.iter().enumerate() {
                if i > 0 {
                    out.push(',');
                }
                out.push_str(&escape_string(k));
                out.push(':');
                write_canonical(v, out)?;
            }
            out.push('}');
        }
    }
    Ok(())
}

/// Serializes a JSON [`Value`] to a canonical ASCII-only JSON string matching Python's formatting.
///
/// Returns an error instead of bytes that could differ from Python's output for any number
/// refused by the input-boundary policy.
pub fn try_to_canonical_string(value: &Value) -> Result<String, CanonicalJsonError> {
    let mut out = String::new();
    write_canonical(value, &mut out)?;
    Ok(out)
}

/// Serializes a JSON [`Value`] to a canonical ASCII-only JSON string matching Python's formatting.
///
/// # Panics
///
/// Panics when [`try_to_canonical_string`] would return an error. It never emits bytes that
/// differ from Python's output. Callers with untrusted input must use
/// [`try_to_canonical_string`]; `audit_hmac` checks its input first with the same policy.
#[must_use]
pub fn to_canonical_string(value: &Value) -> String {
    try_to_canonical_string(value).unwrap_or_else(|err| {
        panic!(
            "canonical JSON refused input ({err}); use try_to_canonical_string for untrusted input"
        )
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn parse(text: &str) -> Value {
        serde_json::from_str(text).unwrap()
    }

    #[test]
    fn test_cpython_object_sorting_and_escaping() {
        let val: Value = serde_json::from_str(r#"{"b":1,"a":[1,2.5,"é"],"é":null}"#).unwrap();
        assert_eq!(
            to_canonical_string(&val),
            r#"{"a":[1,2.5,"\u00e9"],"b":1,"\u00e9":null}"#
        );
    }

    #[test]
    fn test_escape_special_string() {
        let text = "😀\u{7f}\x08\x0c\u{1f}\"\\/";
        assert_eq!(
            escape_string(text),
            "\"\\ud83d\\ude00\\u007f\\b\\f\\u001f\\\"\\\\/\""
        );
    }

    #[test]
    fn test_escape_carriage_return_tab_and_space() {
        assert_eq!(escape_string("\r"), "\"\\r\"");
        assert_eq!(escape_string("\t"), "\"\\t\"");
        assert_eq!(escape_string(" "), "\" \"");
    }

    #[test]
    fn test_printable_ascii_passes_through() {
        let printable: String = (0x20u8..=0x7e)
            .map(char::from)
            .filter(|&c| c != '"' && c != '\\')
            .collect();
        assert_eq!(escape_string(&printable), format!("\"{printable}\""));
    }

    #[test]
    fn test_newline_in_key_and_nested_object_sort() {
        let val: Value = serde_json::from_str("{\"k\\n\":{\"z\":[],\"y\":{}}}").unwrap();
        assert_eq!(to_canonical_string(&val), "{\"k\\n\":{\"y\":{},\"z\":[]}}");
    }

    #[test]
    fn test_format_float_cases() {
        assert_eq!(format_float(1e16).unwrap(), "1e+16");
        assert_eq!(format_float(1.5e16).unwrap(), "1.5e+16");
        assert_eq!(format_float(1e-5).unwrap(), "1e-05");
        assert_eq!(format_float(0.0001).unwrap(), "0.0001");
        assert_eq!(format_float(1e15).unwrap(), "1000000000000000.0");
        assert_eq!(format_float(0.0).unwrap(), "0.0");
        assert_eq!(format_float(-0.0).unwrap(), "-0.0");
        assert_eq!(
            format_float(123456789012345680.0).unwrap(),
            "1.2345678901234568e+17"
        );
        assert_eq!(
            format_float(12345678901234567.0).unwrap(),
            "1.2345678901234568e+16"
        );
        assert_eq!(format_float(5e-324).unwrap(), "5e-324");
        assert_eq!(format_float(f64::MAX).unwrap(), "1.7976931348623157e+308");
        assert_eq!(format_float(0.1).unwrap(), "0.1");
        assert_eq!(format_float(100.0).unwrap(), "100.0");
        assert_eq!(format_float(1e22).unwrap(), "1e+22");
        assert_eq!(format_float(2.5e-7).unwrap(), "2.5e-07");
        assert_eq!(format_float(0.00001234).unwrap(), "1.234e-05");
        assert_eq!(format_float(-1.5).unwrap(), "-1.5");
        assert_eq!(format_float(f64::NAN), Err(CanonicalJsonError::NonFinite));
        assert_eq!(
            format_float(f64::INFINITY),
            Err(CanonicalJsonError::NonFinite)
        );
        assert_eq!(
            format_float(f64::NEG_INFINITY),
            Err(CanonicalJsonError::NonFinite)
        );
    }

    #[test]
    fn test_format_float_decimal_ties_round_half_even() {
        // Exact decimal ties: Python repr rounds half to even; Rust `{:e}` rounds up.
        // Built from exact f64 sums so clippy's excessive_precision lint stays quiet.
        assert_eq!(
            format_float(1011988713385131.0 + 0.25).unwrap(),
            "1011988713385131.2"
        );
        assert_eq!(
            format_float(1011988713385131.0 + 0.75).unwrap(),
            "1011988713385131.8"
        );
        assert_eq!(
            format_float(1000000000000000.0 + 0.25).unwrap(),
            "1000000000000000.2"
        );
    }

    #[test]
    fn test_format_float_neighbour_of_nearest_decimal_round_trips() {
        // The correctly rounded 16-digit decimal misses these doubles; a same-length neighbour
        // round-trips, and Python repr keeps that neighbour. Expected values are Python repr output.
        assert_eq!(
            format_float(7.120236347223045e-307).unwrap(),
            "7.120236347223045e-307"
        );
        assert_eq!(
            format_float(5.960464477539063e-08).unwrap(),
            "5.960464477539063e-08"
        );
        assert_eq!(
            format_float(6.189700196426902e26).unwrap(),
            "6.189700196426902e+26"
        );
    }

    #[test]
    fn test_format_float_values_near_power_of_ten() {
        // Doubles just either side of 1e-300 (Python repr output).
        assert_eq!(
            format_float(9.999999999999999e-301).unwrap(),
            "9.999999999999999e-301"
        );
        assert_eq!(
            format_float(1.000000000000001e-300).unwrap(),
            "1.000000000000001e-300"
        );
    }

    #[test]
    fn test_parsed_float_neighbour_of_nearest_decimal_round_trips() {
        let val: Value = serde_json::from_str(r#"{"x":7.120236347223045e-307}"#).unwrap();
        assert_eq!(to_canonical_string(&val), r#"{"x":7.120236347223045e-307}"#);
    }

    #[test]
    fn test_parsed_float_needing_17_digits_round_trips() {
        // Default serde_json parsing lands on a neighbouring double here; Python keeps the digits.
        let val: Value = serde_json::from_str(r#"{"x":15198712109.619461}"#).unwrap();
        assert_eq!(to_canonical_string(&val), r#"{"x":15198712109.619461}"#);
    }

    #[test]
    fn test_integers() {
        let u_max = Value::Number(serde_json::Number::from(u64::MAX));
        assert_eq!(to_canonical_string(&u_max), "18446744073709551615");

        let i_min = Value::Number(serde_json::Number::from(i64::MIN));
        assert_eq!(to_canonical_string(&i_min), "-9223372036854775808");

        let zero = Value::Number(serde_json::Number::from(0));
        assert_eq!(to_canonical_string(&zero), "0");

        let neg_one = Value::Number(serde_json::Number::from(-1));
        assert_eq!(to_canonical_string(&neg_one), "-1");
    }

    #[test]
    fn test_integer_boundaries_inside_64_bit_range_are_exact() {
        // Parsed as exact PosInt/NegInt, so the digits are Python's digits.
        assert_eq!(
            try_to_canonical_string(&parse("18446744073709551615")).unwrap(),
            "18446744073709551615"
        );
        assert_eq!(
            try_to_canonical_string(&parse("-9223372036854775808")).unwrap(),
            "-9223372036854775808"
        );
        assert_eq!(
            try_to_canonical_string(&parse("-9223372036854775807")).unwrap(),
            "-9223372036854775807"
        );
        assert_eq!(
            try_to_canonical_string(&parse("9223372036854775808")).unwrap(),
            "9223372036854775808"
        );
    }

    #[test]
    fn test_integer_literal_above_u64_max_is_refused() {
        // 2^64 is one past u64::MAX; serde_json parses it as f64.
        assert_eq!(
            try_to_canonical_string(&parse("18446744073709551616")),
            Err(CanonicalJsonError::AmbiguousInteger)
        );
        assert_eq!(
            try_to_canonical_string(&parse("340282366920938463463374607431768211456")),
            Err(CanonicalJsonError::AmbiguousInteger)
        );
    }

    #[test]
    fn test_integer_literal_below_i64_min_is_refused() {
        // One below i64::MIN; serde_json parses it as f64 rounded to -2^63.
        assert_eq!(
            try_to_canonical_string(&parse("-9223372036854775809")),
            Err(CanonicalJsonError::AmbiguousInteger)
        );
        // -2^63 as a float literal shares the refused magnitude.
        assert_eq!(
            try_to_canonical_string(&parse("-9223372036854775808.0")),
            Err(CanonicalJsonError::AmbiguousInteger)
        );
    }

    #[test]
    fn test_refusal_applies_nested_so_no_partial_output() {
        assert_eq!(
            try_to_canonical_string(&parse(r#"{"a":[{"b":18446744073709551616}]}"#)),
            Err(CanonicalJsonError::AmbiguousInteger)
        );
        assert_eq!(
            try_to_canonical_string(&parse("[1,-0]")),
            Err(CanonicalJsonError::NegativeZero)
        );
    }

    #[test]
    fn test_float_literals_in_out_of_range_magnitude_are_refused() {
        assert_eq!(
            try_to_canonical_string(&parse("1e20")),
            Err(CanonicalJsonError::AmbiguousInteger)
        );
        assert_eq!(
            try_to_canonical_string(&parse("1.7976931348623157e308")),
            Err(CanonicalJsonError::AmbiguousInteger)
        );
    }

    #[test]
    fn test_float_literal_inside_range_keeps_python_repr() {
        // 1e19 is below 2^64, so serde_json's f64 cannot be an out-of-range integer.
        assert_eq!(try_to_canonical_string(&parse("1e19")).unwrap(), "1e+19");
        assert_eq!(try_to_canonical_string(&parse("1e16")).unwrap(), "1e+16");
    }

    #[test]
    fn test_negative_zero_integer_literal_is_refused() {
        // Python reads `-0` as the integer 0; serde_json reads it as the float -0.0.
        assert_eq!(
            try_to_canonical_string(&parse("-0")),
            Err(CanonicalJsonError::NegativeZero)
        );
    }

    #[test]
    fn test_negative_zero_float_literal_is_refused() {
        // Python reads `-0.0` as the float -0.0; the parsed value is indistinguishable from `-0`.
        assert_eq!(
            try_to_canonical_string(&parse("-0.0")),
            Err(CanonicalJsonError::NegativeZero)
        );
    }

    #[test]
    fn test_zero_and_negative_non_zero_are_unaffected() {
        assert_eq!(try_to_canonical_string(&parse("0")).unwrap(), "0");
        assert_eq!(try_to_canonical_string(&parse("0.0")).unwrap(), "0.0");
        assert_eq!(try_to_canonical_string(&parse("-1.5")).unwrap(), "-1.5");
    }

    #[test]
    fn test_format_float_keeps_negative_zero_for_direct_formatting() {
        // The float formatter itself still matches Python repr(-0.0); the value-level policy
        // above decides whether a parsed negative zero may be serialized.
        assert_eq!(format_float(-0.0).unwrap(), "-0.0");
    }

    #[test]
    fn test_overflowing_float_literal_is_rejected_by_parser() {
        // serde_json rejects `1e400` before this module sees it; Python accepts it as infinity.
        assert!(serde_json::from_str::<Value>("1e400").is_err());
        assert!(serde_json::from_str::<Value>("-1e400").is_err());
    }

    #[test]
    fn test_lone_surrogate_escape_is_rejected_by_parser() {
        assert!(serde_json::from_str::<Value>(r#""\ud800""#).is_err());
    }

    #[test]
    #[should_panic(expected = "canonical JSON refused input")]
    fn test_to_canonical_string_panics_on_refused_input() {
        let _ = to_canonical_string(&parse("-0"));
    }

    #[test]
    fn test_invalid_neighbour_input_is_an_invariant_error() {
        assert_eq!(
            same_length_neighbours("1.25", 3),
            Err(CanonicalJsonError::Invariant(
                "scientific form has no exponent"
            ))
        );
        assert!(matches!(
            same_length_neighbours("1.5ex", 2),
            Err(CanonicalJsonError::Invariant(_))
        ));
    }

    #[test]
    fn test_error_type_is_copy_and_displays() {
        let err = CanonicalJsonError::NonFinite;
        let copy = err;
        assert_eq!(err, copy);
        assert_eq!(err.to_string(), "non-finite float is not allowed");
    }

    #[test]
    fn test_key_order_independent_of_insertion_order() {
        let mut map1 = serde_json::Map::new();
        map1.insert("b".to_string(), Value::from(1));
        map1.insert("a".to_string(), Value::from(2));

        let mut map2 = serde_json::Map::new();
        map2.insert("a".to_string(), Value::from(2));
        map2.insert("b".to_string(), Value::from(1));

        let val1 = Value::Object(map1);
        let val2 = Value::Object(map2);

        assert_eq!(to_canonical_string(&val1), to_canonical_string(&val2));
        assert_eq!(to_canonical_string(&val1), r#"{"a":2,"b":1}"#);
    }

    #[test]
    fn test_nested_arrays_and_objects() {
        let val = json!({
            "outer_arr": [1, {"nested_key": [2, 3]}],
            "empty_obj": {},
            "empty_arr": []
        });
        assert_eq!(
            to_canonical_string(&val),
            r#"{"empty_arr":[],"empty_obj":{},"outer_arr":[1,{"nested_key":[2,3]}]}"#
        );
    }

    #[test]
    fn test_empty_string() {
        assert_eq!(escape_string(""), "\"\"");
        assert_eq!(to_canonical_string(&Value::String("".to_string())), "\"\"");
    }

    #[test]
    fn test_parsed_float_vs_int() {
        let val: Value = serde_json::from_str(r#"{"x":1.0,"y":1}"#).unwrap();
        assert_eq!(to_canonical_string(&val), r#"{"x":1.0,"y":1}"#);
    }
}
