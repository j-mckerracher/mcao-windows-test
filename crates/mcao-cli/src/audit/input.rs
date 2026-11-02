//! Reads one audit document the way `bin/fm-audit-envelope.py` does: UTF-8 text from a file or
//! stdin, exactly one JSON value, and no object that repeats a key. Every refusal here is an
//! input error, which the command reports with exit status 2.
//!
//! Known gaps against the reference (DPES-884 review 1), kept because serde_json exposes no
//! number-preserving or nesting-unbounded parse without a different parser:
//!  - An integer literal beyond u64/i64 (`seq` = 2^64, `ledger ts` = 2**70) and `-0` arrive as
//!    f64, so the schema rejects them with exit 1 where the reference accepts them with exit 0.
//!  - An out-of-range float (`1e400`) and a lone surrogate escape (`\ud800`) are invalid JSON here
//!    (exit 2); the reference records a schema or identity rejection (exit 1).
//!  - Nesting deeper than serde_json's default limit of 128 is invalid JSON here (exit 2); the
//!    reference records a rejection (exit 1).
//!
//! The ignored tests below state the reference behavior for each gap.

use std::fmt;
use std::io::{self, Read};

use serde::Deserialize;
use serde::de::{self, Deserializer, MapAccess, SeqAccess, Visitor};
use serde_json::{Map, Number, Value};

/// Why an input document could not be read.
#[derive(Debug, PartialEq, Eq)]
pub enum InputError {
    /// The file or stdin could not be read as UTF-8 text.
    Unreadable(String),
    /// The text is not one JSON value, or an object repeats a key.
    Invalid(&'static str),
}

impl fmt::Display for InputError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Unreadable(reason) => write!(f, "cannot read input: {reason}"),
            Self::Invalid(reason) => f.write_str(reason),
        }
    }
}

/// Reads the document at `path`, or stdin when `path` is `-`.
pub fn load(path: &str) -> Result<Value, InputError> {
    let text = if path == "-" {
        let mut text = String::new();
        io::stdin()
            .read_to_string(&mut text)
            .map_err(|err| InputError::Unreadable(err.kind().to_string()))?;
        text
    } else {
        let bytes =
            std::fs::read(path).map_err(|err| InputError::Unreadable(err.kind().to_string()))?;
        String::from_utf8(bytes).map_err(|_| InputError::Unreadable("invalid UTF-8".to_string()))?
    };
    parse(&text)
}

/// Parses `text` as one JSON value, refusing a duplicate object key.
pub fn parse(text: &str) -> Result<Value, InputError> {
    serde_json::from_str::<StrictValue>(text)
        .map(|StrictValue(value)| value)
        .map_err(|err| {
            if err.to_string().contains(DUPLICATE_KEY) {
                InputError::Invalid("input contains a duplicate object key")
            } else {
                InputError::Invalid("input is not valid JSON")
            }
        })
}

/// The message a duplicate key raises, so the caller can name it.
const DUPLICATE_KEY: &str = "duplicate object key";

/// A JSON value built by a visitor that refuses a repeated object key. `serde_json::Value` keeps
/// the last of two equal keys, which the reference refuses instead.
struct StrictValue(Value);

impl<'de> Deserialize<'de> for StrictValue {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        deserializer.deserialize_any(StrictVisitor)
    }
}

struct StrictVisitor;

impl<'de> Visitor<'de> for StrictVisitor {
    type Value = StrictValue;

    fn expecting(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("a JSON value")
    }

    fn visit_bool<E>(self, value: bool) -> Result<StrictValue, E> {
        Ok(StrictValue(Value::Bool(value)))
    }

    fn visit_i64<E>(self, value: i64) -> Result<StrictValue, E> {
        Ok(StrictValue(Value::from(value)))
    }

    fn visit_u64<E>(self, value: u64) -> Result<StrictValue, E> {
        Ok(StrictValue(Value::from(value)))
    }

    fn visit_f64<E: de::Error>(self, value: f64) -> Result<StrictValue, E> {
        Number::from_f64(value)
            .map(|number| StrictValue(Value::Number(number)))
            .ok_or_else(|| E::custom("non-finite number"))
    }

    fn visit_str<E>(self, value: &str) -> Result<StrictValue, E> {
        Ok(StrictValue(Value::String(value.to_owned())))
    }

    fn visit_string<E>(self, value: String) -> Result<StrictValue, E> {
        Ok(StrictValue(Value::String(value)))
    }

    fn visit_unit<E>(self) -> Result<StrictValue, E> {
        Ok(StrictValue(Value::Null))
    }

    fn visit_none<E>(self) -> Result<StrictValue, E> {
        Ok(StrictValue(Value::Null))
    }

    fn visit_seq<A: SeqAccess<'de>>(self, mut seq: A) -> Result<StrictValue, A::Error> {
        let mut items = Vec::new();
        while let Some(StrictValue(item)) = seq.next_element()? {
            items.push(item);
        }
        Ok(StrictValue(Value::Array(items)))
    }

    fn visit_map<A: MapAccess<'de>>(self, mut map: A) -> Result<StrictValue, A::Error> {
        let mut object = Map::new();
        while let Some(key) = map.next_key::<String>()? {
            if object.contains_key(&key) {
                return Err(de::Error::custom(DUPLICATE_KEY));
            }
            let StrictValue(value) = map.next_value()?;
            object.insert(key, value);
        }
        Ok(StrictValue(Value::Object(object)))
    }
}

#[cfg(test)]
mod tests {
    use super::{InputError, parse};

    #[test]
    fn a_duplicate_key_is_refused_at_any_depth() {
        assert_eq!(
            parse(r#"{"a": 1, "a": 2}"#),
            Err(InputError::Invalid("input contains a duplicate object key"))
        );
        assert_eq!(
            parse(r#"{"outer": {"k": 1, "k": 1}}"#),
            Err(InputError::Invalid("input contains a duplicate object key"))
        );
    }

    #[test]
    fn non_finite_literals_and_trailing_text_are_not_valid_json() {
        for text in ["NaN", "Infinity", "[1] x", "", "\u{feff}{}"] {
            assert_eq!(
                parse(text),
                Err(InputError::Invalid("input is not valid JSON")),
                "{text:?}"
            );
        }
    }

    #[test]
    fn one_value_with_distinct_keys_parses() {
        let value = parse(r#" {"b": [1, 2.5, null, true], "a": "x"} "#).expect("valid JSON");
        assert_eq!(value["b"][1], 2.5);
        assert_eq!(value["a"], "x");
    }

    #[test]
    #[ignore = "known gap: an integer beyond u64 is read as f64 (DPES-884 review 1)"]
    fn an_integer_beyond_u64_stays_an_integer() {
        let value = parse(r#"{"seq": 18446744073709551616}"#).expect("valid JSON");
        assert!(!value["seq"].is_f64());
    }

    #[test]
    #[ignore = "known gap: 1e400 is refused as invalid JSON (DPES-884 review 1)"]
    fn an_out_of_range_float_parses_as_the_reference_reads_it() {
        assert!(parse(r#"{"seq": 1e400}"#).is_ok());
    }

    #[test]
    #[ignore = "known gap: a lone surrogate escape is refused as invalid JSON (DPES-884 review 1)"]
    fn a_lone_surrogate_escape_parses_as_the_reference_reads_it() {
        assert!(parse(r#"{"a": "\ud800"}"#).is_ok());
    }

    #[test]
    #[ignore = "known gap: nesting past serde_json's limit of 128 is refused (DPES-884 review 1)"]
    fn nesting_past_the_default_limit_parses() {
        let text = format!("{}{}", "[".repeat(500), "]".repeat(500));
        assert!(parse(&text).is_ok());
    }
}
