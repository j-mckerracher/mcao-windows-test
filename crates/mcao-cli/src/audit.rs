//! `mcao audit`: audit envelope tooling (DPES-884).
//!
//! `validate` ports `bin/fm-audit-envelope.py validate`. The rules live in
//! `mcao_core::audit_envelope`. This module reads the document, supplies zone offsets from the
//! system zoneinfo database, records a rejection, and prints the result line the reference prints.
//!
//! `activity-map` ports `bin/fm-audit-activity-map.py map`. The projection and the DPES-737
//! sanitization boundary live in `mcao_core::audit_activity` and `mcao_core::audit_sanitize`. This
//! module reads the closed sanitizer config and the record, then prints the `mapped` or `refused`
//! line the reference prints. It writes nothing, so a refusal is not recorded.
//!
//! `append`, `verify`, and `project-ledger` port `bin/fm-audit-writer.py` over the store in
//! `mcao_core::audit_writer`; see `writer`.

mod input;
mod tzif;
mod writer;

use std::fs::OpenOptions;
use std::io::Write;
use std::path::PathBuf;
use std::process::ExitCode;

use clap::{Args, Subcommand, ValueEnum};
use mcao_core::audit_activity::{self, Refusal};
use mcao_core::audit_envelope::{self, Definition, Rejection};
use mcao_core::audit_sanitize;
use mcao_core::canonical_json::to_canonical_string;
use mcao_core::util::clock;
use serde_json::{Value, json};
use writer::{AppendArgs, ProjectLedgerArgs, VerifyArgs};

use crate::home_guard::{guarded, resolved_home};

/// The rejection log, under the state directory, one JSON line per recorded rejection.
const REJECTIONS_FILE: &str = "audit-rejections.jsonl";

/// Flags for `mcao audit`.
#[derive(Debug, Args)]
pub struct AuditArgs {
    #[command(subcommand)]
    command: AuditCommand,
}

#[derive(Debug, Subcommand)]
enum AuditCommand {
    /// Validates one audit envelope or fleet ledger record. Exits 0 when accepted, 1 when
    /// rejected and the rejection was recorded, and 2 for a usage or input error.
    Validate(ValidateArgs),
    /// Projects one audit envelope to a sanitized `audit.activity` event. Exits 0 when mapped,
    /// 1 when refused, and 2 for a usage or input error.
    ActivityMap(ActivityMapArgs),
    /// Appends one envelope to an audit store, assigning `seq` and `prev_hash`. Exits 0 when
    /// committed, 1 when rejected and the rejection was recorded, 3 when not committed, and 2 for a
    /// usage or input error.
    Append(AppendArgs),
    /// Verifies every segment and the checkpoint of an audit store without repairing it. Exits 0
    /// when intact, 1 when corrupt, and 2 for a usage or input error.
    Verify(VerifyArgs),
    /// Prints the fleet-ledger v1 base record of each committed envelope in an audit store. Exits 0
    /// when printed, 1 when the store is refused, and 2 for a usage or input error.
    ProjectLedger(ProjectLedgerArgs),
}

/// Flags for `mcao audit validate`.
#[derive(Debug, Args)]
pub struct ValidateArgs {
    /// The document definition to validate against.
    #[arg(long, value_enum, default_value_t = DefinitionArg::Envelope)]
    definition: DefinitionArg,
    /// The JSON document to read, or `-` for stdin.
    input: String,
}

/// Flags for `mcao audit activity-map`.
#[derive(Debug, Args)]
pub struct ActivityMapArgs {
    /// The closed sanitizer config holding the operational home key. Must be a file, not stdin.
    #[arg(long)]
    config: String,
    /// The audit envelope record to map, or `-` for stdin.
    input: String,
}

/// The definitions `validate --definition` accepts, spelled as the reference spells them.
#[derive(Debug, Clone, Copy, ValueEnum)]
enum DefinitionArg {
    Envelope,
    #[value(name = "fleet_ledger_v1")]
    FleetLedgerV1,
}

impl DefinitionArg {
    /// The reference's spelling of this definition, as it appears in every result and record.
    fn name(self) -> &'static str {
        match self {
            Self::Envelope => "envelope",
            Self::FleetLedgerV1 => "fleet_ledger_v1",
        }
    }

    fn definition(self) -> Definition {
        match self {
            Self::Envelope => Definition::Envelope,
            Self::FleetLedgerV1 => Definition::FleetLedgerV1,
        }
    }
}

/// Runs `mcao audit`.
pub fn run(args: &AuditArgs) -> ExitCode {
    match &args.command {
        AuditCommand::Validate(validate) => guarded(|| run_validate(validate)),
        AuditCommand::ActivityMap(activity_map) => run_activity_map(activity_map),
        AuditCommand::Append(append) => guarded(|| writer::run_append(append)),
        AuditCommand::Verify(verify) => guarded(|| writer::run_verify(verify)),
        AuditCommand::ProjectLedger(ledger) => guarded(|| writer::run_project_ledger(ledger)),
    }
}

/// Runs `mcao audit activity-map`: loads the config, maps the record, and prints `mapped` (exit 0)
/// or `refused` (exit 1). A config refusal and a refused record both exit 1, as the reference does.
fn run_activity_map(args: &ActivityMapArgs) -> ExitCode {
    if args.config == "-" {
        return input_failure("activity-map", "the config must be a file path, not stdin");
    }
    let home_key = match load_home_key(&args.config) {
        Ok(home_key) => home_key,
        Err(refusal) => return refuse(refusal.reason, &refusal.paths),
    };
    let record = match input::load(&args.input) {
        Ok(record) => record,
        Err(err) => return input_failure("activity-map", &err.to_string()),
    };
    match audit_activity::map_record(&record, &home_key, &tzif::utc_offset_secs) {
        Ok(mapped) => {
            let result = json!({
                "result": "mapped",
                "changed": mapped.changed,
                "event": mapped.event,
                "log_entry": mapped.log_entry,
            });
            println!("{}", py_json(&result));
            ExitCode::SUCCESS
        }
        Err(Refusal { reason, paths }) => refuse(reason, &paths),
    }
}

/// Reads the sanitizer config at `path` and returns its home key. A file that cannot be read or
/// parsed, or a config that fails [`audit_sanitize::home_key_from_config`], is a `config_invalid`
/// refusal that names pointers only.
fn load_home_key(path: &str) -> Result<[u8; 32], Refusal> {
    let config_invalid = |paths: Vec<String>| Refusal {
        reason: "config_invalid",
        paths,
    };
    let text = std::fs::read_to_string(path).map_err(|_| config_invalid(vec![String::new()]))?;
    let doc = input::parse(&text).map_err(|_| config_invalid(vec![String::new()]))?;
    audit_sanitize::home_key_from_config(&doc).map_err(|refusal| config_invalid(refusal.paths))
}

/// Prints one `refused` line and returns exit status 1.
fn refuse(reason: &str, paths: &[String]) -> ExitCode {
    let result = json!({"result": "refused", "reason": reason, "paths": paths});
    println!("{}", py_json(&result));
    ExitCode::from(1)
}

/// Runs `mcao audit validate`: reads the document, prints `accepted` or `rejected`, and records a
/// rejection before printing it.
fn run_validate(args: &ValidateArgs) -> ExitCode {
    let document = match input::load(&args.input) {
        Ok(document) => document,
        Err(err) => return input_failure("validate", &err.to_string()),
    };
    let definition = args.definition;
    match audit_envelope::validate(&document, definition.definition(), &tzif::utc_offset_secs) {
        Ok(()) => {
            println!(
                "{{\"result\": \"accepted\", \"definition\": {}}}",
                py_string(definition.name())
            );
            ExitCode::SUCCESS
        }
        Err(rejection) => {
            if let Err(reason) = record_rejection(definition.name(), &rejection) {
                return input_failure("validate", &format!("rejection not recorded: {reason}"));
            }
            println!("{}", rejected_line(definition.name(), &rejection));
            ExitCode::from(1)
        }
    }
}

/// Reports an input or usage failure of `mcao audit <command>` on stderr and returns exit status 2.
fn input_failure(command: &str, message: &str) -> ExitCode {
    eprintln!("mcao audit {command}: {message}");
    ExitCode::from(2)
}

/// The `rejected` result line, with the keys in the reference's order.
fn rejected_line(definition: &str, rejection: &Rejection) -> String {
    let paths: Vec<String> = rejection.paths.iter().map(|path| py_string(path)).collect();
    format!(
        "{{\"result\": \"rejected\", \"definition\": {}, \"reason\": {}, \"paths\": [{}]}}",
        py_string(definition),
        py_string(rejection.reason.as_str()),
        paths.join(", ")
    )
}

/// Appends one rejection line to the rejection log under the state directory.
///
/// The line is `json.dumps(record, sort_keys=True)` from the reference: sorted keys, `, ` and
/// `: ` separators, and ASCII-escaped strings.
fn record_rejection(definition: &str, rejection: &Rejection) -> Result<(), String> {
    let log = rejection_log_path()?;
    let paths: Vec<String> = rejection.paths.iter().map(|path| py_string(path)).collect();
    let rejected_at = clock::now_ms() / 1000;
    let line = format!(
        "{{\"definition\": {}, \"paths\": [{}], \"reason\": {}, \"rejected_at\": {rejected_at}, \"v\": 1}}\n",
        py_string(definition),
        paths.join(", "),
        py_string(rejection.reason.as_str()),
    );
    let mut file = OpenOptions::new()
        .create(true)
        .append(true)
        .open(log)
        .map_err(|err| err.kind().to_string())?;
    file.write_all(line.as_bytes())
        .map_err(|err| err.kind().to_string())
}

/// The rejection log path: `FM_STATE_OVERRIDE`, else `state` under the resolved home. The home is
/// `FM_HOME`, or the Windows default home when `FM_HOME` is unset. An empty variable counts as
/// unset, as it does in the reference.
fn rejection_log_path() -> Result<PathBuf, String> {
    let set = |name: &str| std::env::var_os(name).filter(|value| !value.is_empty());
    if let Some(state) = set("FM_STATE_OVERRIDE") {
        return Ok(PathBuf::from(state).join(REJECTIONS_FILE));
    }
    match resolved_home(|name| std::env::var_os(name)).map_err(|error| error.to_string())? {
        Some(home) => Ok(home.join("state").join(REJECTIONS_FILE)),
        None => {
            Err("set FM_HOME or FM_STATE_OVERRIDE so the rejection can be recorded".to_string())
        }
    }
}

/// One string as `json.dumps` writes it with `ensure_ascii=True`.
fn py_string(text: &str) -> String {
    to_canonical_string(&Value::String(text.to_owned()))
}

/// A value as `json.dumps(value, sort_keys=True)` writes it: sorted keys, `", "` between items,
/// and `": "` after each key.
///
/// The record's numbers are the `i64` and `u64` values the validator admits, so the scalar
/// formatter cannot refuse one.
fn py_json(value: &Value) -> String {
    match value {
        Value::Array(items) => {
            let items: Vec<String> = items.iter().map(py_json).collect();
            format!("[{}]", items.join(", "))
        }
        Value::Object(members) => {
            let members: Vec<String> = members
                .iter()
                .map(|(key, member)| format!("{}: {}", py_string(key), py_json(member)))
                .collect();
            format!("{{{}}}", members.join(", "))
        }
        scalar => to_canonical_string(scalar),
    }
}

#[cfg(test)]
mod tests {
    use super::{DefinitionArg, py_string};

    #[test]
    fn strings_are_ascii_escaped_like_the_reference() {
        assert_eq!(py_string("a\"b"), r#""a\"b""#);
        assert_eq!(py_string("é"), r#""\u00e9""#);
    }

    #[test]
    fn definition_names_match_the_reference_spelling() {
        assert_eq!(DefinitionArg::FleetLedgerV1.name(), "fleet_ledger_v1");
        assert_eq!(DefinitionArg::Envelope.name(), "envelope");
    }
}
