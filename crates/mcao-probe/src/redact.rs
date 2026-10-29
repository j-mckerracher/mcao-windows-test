//! Redaction of the evidence artifact, and the manifest that says what was redacted.
//!
//! The probe's records can carry operating-system error text and process output, and either can
//! name a user, a home directory or a runner path. Before an artifact leaves the machine, every
//! string value is scanned for the literals below and each hit is replaced by a fixed token. The
//! manifest lists each rule with its token and hit count. It never holds the redacted values.

use std::env;

use serde::Serialize;
use serde_json::Value;

/// Values shorter than this are not redacted: a short literal would rewrite unrelated text.
const MIN_LEN: usize = 4;

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Rule {
    name: &'static str,
    value: Option<String>,
}

#[derive(Debug, PartialEq, Eq, Serialize)]
pub struct Applied {
    pub rule: &'static str,
    pub token: String,
    /// False when the literal was unknown or too short to redact on this machine.
    pub active: bool,
    pub replacements: usize,
}

#[derive(Debug, PartialEq, Eq, Serialize)]
pub struct Manifest {
    pub version: u32,
    pub note: &'static str,
    pub rules: Vec<Applied>,
}

fn token(name: &str) -> String {
    format!("<redacted:{name}>")
}

/// The rules for this machine, taken from the environment and the running binary.
pub fn machine_rules() -> Vec<Rule> {
    let var = |name: &str| env::var(name).ok();
    vec![
        Rule {
            name: "home",
            value: var("HOME").or_else(|| var("USERPROFILE")),
        },
        Rule {
            name: "user",
            value: var("USER").or_else(|| var("USERNAME")),
        },
        Rule {
            name: "cwd",
            value: env::current_dir()
                .ok()
                .map(|path| path.display().to_string()),
        },
        Rule {
            name: "temp",
            value: Some(env::temp_dir().display().to_string()),
        },
        Rule {
            name: "exe_dir",
            value: env::current_exe()
                .ok()
                .and_then(|path| path.parent().map(|dir| dir.display().to_string())),
        },
    ]
}

/// Redacts every string value in `evidence` and reports what was replaced. Longer literals go
/// first, so a directory inside the home directory is named by its own rule.
pub fn redact(evidence: &mut Value, rules: &[Rule]) -> Manifest {
    let mut order: Vec<usize> = (0..rules.len()).collect();
    order.sort_by_key(|&index| {
        std::cmp::Reverse(rules[index].value.as_ref().map_or(0, String::len))
    });

    let mut applied: Vec<Applied> = rules
        .iter()
        .map(|rule| Applied {
            rule: rule.name,
            token: token(rule.name),
            active: rule
                .value
                .as_ref()
                .is_some_and(|value| value.len() >= MIN_LEN),
            replacements: 0,
        })
        .collect();
    for index in order {
        if let Some(value) = rules[index].value.as_deref().filter(|v| v.len() >= MIN_LEN) {
            applied[index].replacements =
                replace_in_strings(evidence, value, &applied[index].token);
        }
    }
    Manifest {
        version: 1,
        note: "Rules name what was redacted and how often. The redacted values are never recorded.",
        rules: applied,
    }
}

fn replace_in_strings(value: &mut Value, needle: &str, token: &str) -> usize {
    match value {
        Value::String(text) => {
            let hits = text.matches(needle).count();
            if hits > 0 {
                *text = text.replace(needle, token);
            }
            hits
        }
        Value::Array(items) => items
            .iter_mut()
            .map(|item| replace_in_strings(item, needle, token))
            .sum(),
        Value::Object(fields) => fields
            .values_mut()
            .map(|field| replace_in_strings(field, needle, token))
            .sum(),
        _ => 0,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn rule(name: &'static str, value: Option<&str>) -> Rule {
        Rule {
            name,
            value: value.map(str::to_owned),
        }
    }

    #[test]
    fn redacts_string_values_and_counts_hits() {
        let mut evidence = json!([
            { "reason": "cannot open /home/alice/work/x: denied", "duration_ms": 3 },
            { "reason": "ok", "output_sample": "alice-box wrote /home/alice/log" }
        ]);
        let manifest = redact(
            &mut evidence,
            &[
                rule("home", Some("/home/alice")),
                rule("user", Some("alice")),
            ],
        );
        assert_eq!(
            evidence,
            json!([
                { "reason": "cannot open <redacted:home>/work/x: denied", "duration_ms": 3 },
                { "reason": "ok", "output_sample": "<redacted:user>-box wrote <redacted:home>/log" }
            ])
        );
        let counts: Vec<(&str, usize)> = manifest
            .rules
            .iter()
            .map(|applied| (applied.rule, applied.replacements))
            .collect();
        assert_eq!(counts, [("home", 2), ("user", 1)]);
    }

    #[test]
    fn longer_literal_wins_and_manifest_keeps_rule_order() {
        let mut evidence = json!({ "reason": "/home/alice/probe/bin" });
        let manifest = redact(
            &mut evidence,
            &[
                rule("home", Some("/home/alice")),
                rule("exe_dir", Some("/home/alice/probe/bin")),
            ],
        );
        assert_eq!(evidence, json!({ "reason": "<redacted:exe_dir>" }));
        assert_eq!(manifest.rules[0].rule, "home");
        assert_eq!(manifest.rules[0].replacements, 0);
        assert_eq!(manifest.rules[1].replacements, 1);
    }

    #[test]
    fn unknown_and_short_values_are_inactive_and_change_nothing() {
        let mut evidence = json!({ "reason": "a b c" });
        let manifest = redact(
            &mut evidence,
            &[rule("user", Some("a")), rule("home", None)],
        );
        assert_eq!(evidence, json!({ "reason": "a b c" }));
        assert!(manifest.rules.iter().all(|applied| !applied.active));
    }

    #[test]
    fn keys_and_non_string_values_are_left_alone() {
        let mut evidence = json!({ "user_name": true, "n": 1234 });
        redact(&mut evidence, &[rule("user", Some("user_name"))]);
        assert_eq!(evidence, json!({ "user_name": true, "n": 1234 }));
    }

    #[test]
    fn manifest_never_contains_the_redacted_value() {
        let mut evidence = json!({ "reason": "secret-token-value" });
        let manifest = redact(&mut evidence, &[rule("user", Some("secret-token-value"))]);
        let text = serde_json::to_string(&manifest).unwrap();
        assert!(!text.contains("secret-token-value"));
    }
}
