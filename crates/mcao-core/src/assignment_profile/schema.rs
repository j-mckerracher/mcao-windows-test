//! Schema evaluator for the keywords the assignment-profile schema uses. It follows `evaluate`
//! in bin/fm-assignment-profile.py: the same keyword order, the same early return on a type
//! mismatch, and the same error paths. Any keyword outside the validating set fails closed.

use super::Refusal;
use super::json::{COMPACT, Json, dump};
use std::collections::BTreeSet;

const VALIDATING: &[&str] = &[
    "$ref",
    "type",
    "const",
    "enum",
    "pattern",
    "minLength",
    "minItems",
    "uniqueItems",
    "items",
    "required",
    "properties",
    "additionalProperties",
    "allOf",
    "if",
    "then",
];
const ANNOTATIONS: &[&str] = &["$schema", "$id", "title", "description"];

/// The only two `pattern` strings this evaluator implements. Any other pattern is unsupported.
const PATTERN_DECISION_ID: &str = "^[^/]+/[1-9][0-9]*$";
const PATTERN_COMPARISON_BASE: &str = "^[0-9a-f]{40}$";

pub(super) type Errors = Vec<(String, String)>;

pub(super) fn evaluate(
    schema: &Json,
    value: &Json,
    root: &Json,
    path: &str,
    errors: &mut Errors,
) -> Result<(), Refusal> {
    let Some(node) = schema.as_object() else {
        return Err(Refusal::usage("schema node is not an object"));
    };
    let unknown: BTreeSet<&str> = node
        .iter()
        .map(|(key, _)| key.as_str())
        .filter(|key| !VALIDATING.contains(key) && !ANNOTATIONS.contains(key))
        .collect();
    if let Some(first) = unknown.first() {
        return Err(Refusal::usage(format!(
            "unsupported schema keyword: {first}"
        )));
    }
    if schema.get("then").is_some() && schema.get("if").is_none() {
        return Err(Refusal::usage("schema has then without if"));
    }
    if let Some(pattern) = schema.get("pattern") {
        match pattern.as_str() {
            Some(PATTERN_DECISION_ID | PATTERN_COMPARISON_BASE) => {}
            _ => return Err(unsupported_pattern(pattern)),
        }
    }

    if let Some(reference) = schema.get("$ref") {
        let target = resolve(root, reference)?;
        evaluate(target, value, root, path, errors)?;
    }
    if let Some(declared) = schema.get("type") {
        let names: Vec<&Json> = match declared {
            Json::Array(items) => items.iter().collect(),
            single => vec![single],
        };
        let mut matched = false;
        for name in &names {
            if is_type(name, value)? {
                matched = true;
                break;
            }
        }
        if !matched {
            let labels = names
                .iter()
                .map(|name| {
                    name.as_str()
                        .map(str::to_string)
                        .ok_or_else(|| unsupported_type(name))
                })
                .collect::<Result<Vec<_>, _>>()?;
            errors.push((path.to_string(), format!("is not a {}", labels.join("/"))));
            return Ok(());
        }
    }
    if let Some(constant) = schema.get("const")
        && !same(value, constant)
    {
        errors.push((
            path.to_string(),
            format!("must be {}", dump(constant, COMPACT)),
        ));
    }
    if let Some(options) = schema.get("enum") {
        let options = options
            .as_array()
            .ok_or_else(|| Refusal::usage("enum must be a list"))?;
        if !options.iter().any(|option| same(value, option)) {
            errors.push((
                path.to_string(),
                "is not one of the allowed values".to_string(),
            ));
        }
    }
    if let Json::Str(text) = value {
        if let Some(pattern) = schema.get("pattern").and_then(Json::as_str)
            && !matches_pattern(pattern, text)
        {
            errors.push((
                path.to_string(),
                "does not match the required form".to_string(),
            ));
        }
        if let Some(minimum) = schema.get("minLength") {
            let minimum = integer(minimum)?;
            if (text.chars().count() as i64) < minimum {
                errors.push((path.to_string(), format!("is shorter than {minimum}")));
            }
        }
    }
    if let Json::Array(items) = value {
        if let Some(minimum) = schema.get("minItems") {
            let minimum = integer(minimum)?;
            if (items.len() as i64) < minimum {
                errors.push((
                    path.to_string(),
                    format!("needs at least {minimum} item(s)"),
                ));
            }
        }
        if schema.get("uniqueItems").is_some_and(Json::truthy) {
            let keys: Vec<String> = items.iter().map(|item| dump(item, COMPACT)).collect();
            let distinct: BTreeSet<&String> = keys.iter().collect();
            if distinct.len() != keys.len() {
                errors.push((path.to_string(), "repeats an entry".to_string()));
            }
        }
        if let Some(item_schema) = schema.get("items") {
            for (index, item) in items.iter().enumerate() {
                evaluate(item_schema, item, root, &format!("{path}/{index}"), errors)?;
            }
        }
    }
    if let Json::Object(members) = value {
        if let Some(required) = schema.get("required") {
            let required = required
                .as_array()
                .ok_or_else(|| Refusal::usage("required must be a list"))?;
            for key in required {
                let key = key
                    .as_str()
                    .ok_or_else(|| Refusal::usage("required entries must be strings"))?;
                if value.get(key).is_none() {
                    errors.push((format!("{path}/{key}"), "is required".to_string()));
                }
            }
        }
        let properties = match schema.get("properties") {
            None => &[][..],
            Some(props) => props
                .as_object()
                .ok_or_else(|| Refusal::usage("properties must be an object"))?,
        };
        for (key, sub) in properties {
            if let Some(child) = value.get(key) {
                evaluate(sub, child, root, &format!("{path}/{key}"), errors)?;
            }
        }
        if let Some(extra) = schema.get("additionalProperties") {
            for (key, child) in members {
                if properties.iter().any(|(known, _)| known == key) {
                    continue;
                }
                match extra {
                    Json::Bool(false) => {
                        errors.push((
                            format!("{path}/{key}"),
                            "is not a field of this record".to_string(),
                        ));
                    }
                    Json::Object(_) => {
                        evaluate(extra, child, root, &format!("{path}/{key}"), errors)?
                    }
                    _ => {}
                }
            }
        }
    }
    if let Some(all_of) = schema.get("allOf") {
        let all_of = all_of
            .as_array()
            .ok_or_else(|| Refusal::usage("allOf must be a list"))?;
        for sub in all_of {
            evaluate(sub, value, root, path, errors)?;
        }
    }
    if let Some(condition) = schema.get("if") {
        let mut probe = Errors::new();
        evaluate(condition, value, root, path, &mut probe)?;
        if probe.is_empty()
            && let Some(then) = schema.get("then")
        {
            evaluate(then, value, root, path, errors)?;
        }
    }
    Ok(())
}

fn is_type(name: &Json, value: &Json) -> Result<bool, Refusal> {
    Ok(match name.as_str() {
        Some("object") => matches!(value, Json::Object(_)),
        Some("array") => matches!(value, Json::Array(_)),
        Some("string") => matches!(value, Json::Str(_)),
        Some("boolean") => matches!(value, Json::Bool(_)),
        Some("integer") => matches!(value, Json::Int(_)),
        _ => return Err(unsupported_type(name)),
    })
}

fn unsupported_type(name: &Json) -> Refusal {
    Refusal::usage(format!("unsupported type name: {}", dump(name, COMPACT)))
}

fn unsupported_pattern(pattern: &Json) -> Refusal {
    Refusal::usage(format!("unsupported pattern: {}", dump(pattern, COMPACT)))
}

fn integer(keyword: &Json) -> Result<i64, Refusal> {
    match keyword {
        Json::Int(digits) => digits
            .parse::<i64>()
            .map_err(|_| Refusal::usage("schema integer keyword is out of range")),
        _ => Err(Refusal::usage("schema length keyword must be an integer")),
    }
}

fn resolve<'a>(root: &'a Json, reference: &Json) -> Result<&'a Json, Refusal> {
    let reference = reference
        .as_str()
        .ok_or_else(|| Refusal::usage("schema $ref must be a string"))?;
    let Some(pointer) = reference.strip_prefix("#/") else {
        return Err(Refusal::usage(format!(
            "unsupported reference: {reference}"
        )));
    };
    let mut node = root;
    for part in pointer.split('/') {
        let key = part.replace("~1", "/").replace("~0", "~");
        node = node
            .get(&key)
            .ok_or_else(|| Refusal::usage(format!("unresolved reference: {reference}")))?;
    }
    Ok(node)
}

/// Equality as the Python evaluator defines it: booleans never equal numbers, containers compare
/// element by element, and an integer equals a float of the same numeric value.
fn same(a: &Json, b: &Json) -> bool {
    match (a, b) {
        (Json::Bool(x), Json::Bool(y)) => x == y,
        (Json::Bool(_), _) | (_, Json::Bool(_)) => false,
        (Json::Object(x), Json::Object(y)) => {
            x.len() == y.len()
                && x.iter()
                    .all(|(key, value)| b.get(key).is_some_and(|other| same(value, other)))
        }
        (Json::Array(x), Json::Array(y)) => {
            x.len() == y.len() && x.iter().zip(y).all(|(p, q)| same(p, q))
        }
        (Json::Object(_) | Json::Array(_), _) | (_, Json::Object(_) | Json::Array(_)) => false,
        (Json::Null, Json::Null) => true,
        (Json::Str(x), Json::Str(y)) => x == y,
        (Json::Int(x), Json::Int(y)) => x == y,
        (Json::Float(x), Json::Float(y)) => x == y,
        (Json::Int(digits), Json::Float(f)) | (Json::Float(f), Json::Int(digits)) => {
            f.is_finite() && f.fract() == 0.0 && {
                let exact = format!("{f:.0}");
                let exact = if exact == "-0" { "0" } else { exact.as_str() };
                digits == exact
            }
        }
        _ => false,
    }
}

/// Exact matcher for the schema's `pattern` strings under Python's `re.search`. The trailing
/// `$` also matches before one final newline, so a single trailing `\n` is accepted.
fn matches_pattern(pattern: &str, value: &str) -> bool {
    match pattern {
        PATTERN_DECISION_ID => matches_decision_id(value),
        PATTERN_COMPARISON_BASE => matches_comparison_base(value),
        _ => false,
    }
}

fn matches_decision_id(value: &str) -> bool {
    let Some((head, tail)) = value.split_once('/') else {
        return false;
    };
    if head.is_empty() {
        return false;
    }
    let core = tail.strip_suffix('\n').unwrap_or(tail);
    let mut digits = core.bytes();
    matches!(digits.next(), Some(b'1'..=b'9')) && digits.all(|b| b.is_ascii_digit())
}

fn matches_comparison_base(value: &str) -> bool {
    let core = value.strip_suffix('\n').unwrap_or(value);
    core.len() == 40 && core.bytes().all(|b| matches!(b, b'0'..=b'9' | b'a'..=b'f'))
}

#[cfg(test)]
mod tests {
    use super::super::json;
    use super::*;

    fn run_schema(schema: &str, value: &str) -> Result<Errors, Refusal> {
        let schema = json::parse(schema).expect("schema");
        let value = json::parse(value).expect("value");
        let mut errors = Errors::new();
        evaluate(&schema, &value, &schema, "", &mut errors).map(|()| errors)
    }

    #[test]
    fn unknown_keyword_fails_closed_as_usage() {
        let refusal = run_schema(r#"{"format": "uuid"}"#, "{}").expect_err("must not pass");
        assert_eq!(refusal.exit_code(), 2);
        assert_eq!(refusal.reason(), "unsupported schema keyword: format");
    }

    #[test]
    fn unsupported_pattern_is_usage_even_for_non_strings() {
        let refusal = run_schema(r#"{"pattern": "^x$"}"#, "3").expect_err("must not pass");
        assert_eq!(refusal.exit_code(), 2);
    }

    #[test]
    fn pattern_dollar_accepts_one_trailing_newline_like_python() {
        assert!(matches_comparison_base(&format!("{}\n", "a".repeat(40))));
        assert!(!matches_comparison_base(&format!("{}\n\n", "a".repeat(40))));
        assert!(!matches_comparison_base(&"A".repeat(40)));
        assert!(matches_decision_id("T-1/12"));
        assert!(!matches_decision_id("/1"));
        assert!(!matches_decision_id("T-1/01"));
    }

    #[test]
    fn integer_equals_float_of_same_value_but_booleans_do_not_equal_numbers() {
        let one = json::parse("1").expect("int");
        assert!(same(&one, &json::parse("1.0").expect("float")));
        assert!(!same(&one, &json::parse("true").expect("bool")));
        assert!(!same(&one, &json::parse("1.5").expect("float")));
    }

    #[test]
    fn python_float_repr_matches_fixed_and_scientific_forms() {
        assert_eq!(json::py_float(1.0), "1.0");
        assert_eq!(json::py_float(0.0001), "0.0001");
        assert_eq!(json::py_float(1.5e-5), "1.5e-05");
        assert_eq!(json::py_float(1e16), "1e+16");
        assert_eq!(json::py_float(-0.0), "-0.0");
    }
}
