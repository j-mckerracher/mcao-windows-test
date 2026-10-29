//! Command-line parsing for the probe. Mock and native backends are accepted; anything else is refused.

use std::fmt;
use std::path::PathBuf;

use crate::{mock, stub};

pub const USAGE: &str = "usage: mcao-probe --backend mock --harness mock --scenario cooperative|uncooperative\n       mcao-probe --backend native --harness stub --scenario cooperative|uncooperative [--evidence-out PATH]";

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Scenario {
    /// The agent stops its running turn after one Escape.
    Cooperative,
    /// The agent ignores the Escape and keeps running.
    Uncooperative,
}

/// Which backend runs the lifecycle. Each one is paired with exactly one harness, so an
/// unsupported pairing cannot be expressed after parsing.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Backend {
    Mock,
    /// The real process backend of the platform the probe was built for, driving the stub harness.
    Native,
}

impl Backend {
    pub fn harness(self) -> &'static str {
        match self {
            Backend::Mock => mock::HARNESS,
            Backend::Native => stub::HARNESS,
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Config {
    pub backend: Backend,
    pub scenario: Scenario,
    /// When set, the redacted evidence and its redaction manifest are also written here.
    pub evidence_out: Option<PathBuf>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ArgError {
    Missing(&'static str),
    MissingValue(String),
    Unknown(String),
    Unsupported(String),
    BadScenario(String),
}

impl fmt::Display for ArgError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            ArgError::Missing(flag) => write!(f, "missing required flag {flag}"),
            ArgError::MissingValue(flag) => write!(f, "flag {flag} needs a value"),
            ArgError::Unknown(flag) => write!(f, "unknown flag `{flag}`"),
            ArgError::Unsupported(reason) => write!(f, "{reason}"),
            ArgError::BadScenario(value) => {
                write!(f, "scenario `{value}` is not cooperative or uncooperative")
            }
        }
    }
}

pub fn parse<I>(args: I) -> Result<Config, ArgError>
where
    I: IntoIterator<Item = String>,
{
    let mut backend = None;
    let mut harness = None;
    let mut scenario = None;
    let mut evidence_out = None;

    let mut args = args.into_iter();
    while let Some(flag) = args.next() {
        let value = args
            .next()
            .ok_or_else(|| ArgError::MissingValue(flag.clone()))?;
        match flag.as_str() {
            "--backend" => backend = Some(value),
            "--harness" => harness = Some(value),
            "--scenario" => scenario = Some(value),
            "--evidence-out" => evidence_out = Some(PathBuf::from(value)),
            _ => return Err(ArgError::Unknown(flag)),
        }
    }

    let backend = match backend.ok_or(ArgError::Missing("--backend"))?.as_str() {
        "mock" => Backend::Mock,
        "native" => Backend::Native,
        other => {
            return Err(ArgError::Unsupported(format!(
                "backend `{other}` is not implemented; only `mock` and `native` are"
            )));
        }
    };

    let harness = harness.ok_or(ArgError::Missing("--harness"))?;
    if harness != backend.harness() {
        return Err(ArgError::Unsupported(format!(
            "harness `{harness}` is not implemented for this backend; it runs only `{}`",
            backend.harness()
        )));
    }

    let scenario = match scenario.ok_or(ArgError::Missing("--scenario"))?.as_str() {
        "cooperative" => Scenario::Cooperative,
        "uncooperative" => Scenario::Uncooperative,
        other => return Err(ArgError::BadScenario(other.to_owned())),
    };

    Ok(Config {
        backend,
        scenario,
        evidence_out,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn parse_strs(args: &[&str]) -> Result<Config, ArgError> {
        parse(args.iter().map(|arg| (*arg).to_owned()))
    }

    #[test]
    fn native_backend_pairs_with_the_stub_harness() {
        let config = parse_strs(&[
            "--backend",
            "native",
            "--harness",
            "stub",
            "--scenario",
            "uncooperative",
            "--evidence-out",
            "out.json",
        ])
        .unwrap();
        assert_eq!(
            config,
            Config {
                backend: Backend::Native,
                scenario: Scenario::Uncooperative,
                evidence_out: Some(PathBuf::from("out.json")),
            }
        );
    }

    #[test]
    fn native_backend_refuses_the_mock_harness() {
        let error = parse_strs(&[
            "--backend",
            "native",
            "--harness",
            "mock",
            "--scenario",
            "cooperative",
        ])
        .unwrap_err();
        assert_eq!(
            error.to_string(),
            "harness `mock` is not implemented for this backend; it runs only `stub`"
        );
    }

    #[test]
    fn mock_backend_refuses_the_stub_harness() {
        let error = parse_strs(&[
            "--backend",
            "mock",
            "--harness",
            "stub",
            "--scenario",
            "cooperative",
        ])
        .unwrap_err();
        assert!(matches!(error, ArgError::Unsupported(_)));
    }

    #[test]
    fn herdr_backend_is_refused() {
        let error = parse_strs(&[
            "--backend",
            "herdr",
            "--harness",
            "stub",
            "--scenario",
            "cooperative",
        ])
        .unwrap_err();
        assert_eq!(
            error.to_string(),
            "backend `herdr` is not implemented; only `mock` and `native` are"
        );
    }

    #[test]
    fn evidence_out_without_a_value_is_refused() {
        let error = parse_strs(&["--evidence-out"]).unwrap_err();
        assert_eq!(error, ArgError::MissingValue("--evidence-out".into()));
    }
}
