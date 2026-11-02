//! One tool's qualification record and the verdict derived from its facts (AC5).

use serde_json::{Value, json};

/// The qualification verdict. Its `as_str` names are the values in the JSON record.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Verdict {
    /// Present, started with argv only, and reports a version.
    Supported,
    /// Present, but it needs a shell, a script shim, or a refused install path.
    Unsupported,
    /// Absent from the machine.
    Blocked,
}

impl Verdict {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Supported => "supported",
            Self::Unsupported => "unsupported",
            Self::Blocked => "blocked",
        }
    }
}

/// The facts a verdict is derived from.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ToolFacts {
    pub binary_exists: bool,
    pub argv_only: bool,
    pub version_ok: bool,
    /// The tool is refused on this platform, so no probe result can make it supported.
    pub refused: bool,
    pub blockers_present: bool,
}

/// AC5: refused is unsupported; missing is blocked; present, argv-only, versioned, and unblocked is
/// supported; any other present tool is unsupported.
pub fn decide(facts: ToolFacts) -> Verdict {
    if facts.refused {
        Verdict::Unsupported
    } else if !facts.binary_exists {
        Verdict::Blocked
    } else if facts.argv_only && facts.version_ok && !facts.blockers_present {
        Verdict::Supported
    } else {
        Verdict::Unsupported
    }
}

/// The qualification record for one tool.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ToolRecord {
    pub name: &'static str,
    pub binary_exists: bool,
    /// The route the tool is started by, such as `native_exe` or `node_script`.
    pub launcher_type: &'static str,
    pub install_method: &'static str,
    /// True only when the tool is started with argv and no shell, `cmd`, or npm entry point. This is a
    /// verdict input, not a record field, so it is not serialized.
    pub argv_only: bool,
    pub version: Option<String>,
    pub limitations: Vec<String>,
    pub blockers: Vec<String>,
    pub verdict: Verdict,
    /// The CI job that produced this record, or empty outside CI.
    pub evidence_link: String,
}

impl ToolRecord {
    /// A record with no facts yet. Call [`ToolRecord::settle`] before the record is used.
    pub fn new(
        name: &'static str,
        launcher_type: &'static str,
        install_method: &'static str,
    ) -> Self {
        Self {
            name,
            binary_exists: false,
            launcher_type,
            install_method,
            argv_only: false,
            version: None,
            limitations: Vec::new(),
            blockers: Vec::new(),
            verdict: Verdict::Blocked,
            evidence_link: String::new(),
        }
    }

    /// Derives the verdict from the record's facts. `refused` marks a tool refused on this platform.
    pub fn settle(mut self, refused: bool) -> Self {
        self.verdict = decide(ToolFacts {
            binary_exists: self.binary_exists,
            argv_only: self.argv_only,
            version_ok: self.version.is_some(),
            refused,
            blockers_present: !self.blockers.is_empty(),
        });
        self
    }

    /// The record in the normative tool shape: `{name, binary_exists, launcher_type, install_method,
    /// version, limitations, blockers, verdict, evidence_link}`.
    pub fn to_json(&self) -> Value {
        json!({
            "name": self.name,
            "binary_exists": self.binary_exists,
            "launcher_type": self.launcher_type,
            "install_method": self.install_method,
            "version": self.version,
            "limitations": self.limitations,
            "blockers": self.blockers,
            "verdict": self.verdict.as_str(),
            "evidence_link": self.evidence_link,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn facts(binary_exists: bool, argv_only: bool, version_ok: bool) -> ToolFacts {
        ToolFacts {
            binary_exists,
            argv_only,
            version_ok,
            refused: false,
            blockers_present: false,
        }
    }

    #[test]
    fn verdict_table_follows_ac5() {
        // supported = exists + argv-only + version-ok
        assert_eq!(decide(facts(true, true, true)), Verdict::Supported);
        // unsupported = exists + shell-required
        assert_eq!(decide(facts(true, false, true)), Verdict::Unsupported);
        assert_eq!(decide(facts(true, false, false)), Verdict::Unsupported);
        // a present argv-only tool without a working version is not supported
        assert_eq!(decide(facts(true, true, false)), Verdict::Unsupported);
        // blocked = missing, whatever else is true
        assert_eq!(decide(facts(false, true, true)), Verdict::Blocked);
        assert_eq!(decide(facts(false, false, false)), Verdict::Blocked);
    }

    #[test]
    fn blockers_and_refusal_override_otherwise_supported_facts() {
        let blocked = ToolFacts {
            blockers_present: true,
            ..facts(true, true, true)
        };
        assert_eq!(decide(blocked), Verdict::Unsupported);

        let refused = ToolFacts {
            refused: true,
            ..facts(true, true, true)
        };
        assert_eq!(decide(refused), Verdict::Unsupported);

        let refused_and_missing = ToolFacts {
            refused: true,
            ..facts(false, false, false)
        };
        assert_eq!(decide(refused_and_missing), Verdict::Unsupported);
    }

    #[test]
    fn settle_reads_the_record_facts() {
        let mut record = ToolRecord::new("tool", "native_exe", "PATH executable");
        record.binary_exists = true;
        record.argv_only = true;
        record.version = Some("1.0".to_owned());
        assert_eq!(record.clone().settle(false).verdict, Verdict::Supported);
        assert_eq!(record.settle(true).verdict, Verdict::Unsupported);
    }

    #[test]
    fn verdict_strings_are_the_record_vocabulary() {
        assert_eq!(Verdict::Supported.as_str(), "supported");
        assert_eq!(Verdict::Unsupported.as_str(), "unsupported");
        assert_eq!(Verdict::Blocked.as_str(), "blocked");
    }
}
