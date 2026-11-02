//! `bin/fm-audit-writer.py` chain case replayed against the pre-port baseline (DPES-753, DPES-884).
//!
//! The baseline is `tests/fixtures/pre-port-baseline/outputs/fm-audit-writer/chain`, generated only
//! by the Python reference at main `d80bcd15` (see that directory's README). The steps and their
//! committed inputs come from `manifest.json`. Each step's `.command` gives the arguments, and its
//! `.stdout`, `.stderr`, and `.exit` give the expected result. Nothing here computes an expected
//! value.
//!
//! Comparison: the stdout of a JSON-result step is compared as a parsed JSON value, because the
//! reference's default separators (`", "`, `": "`) are not reproduced by this library. The
//! fleet-ledger stdout is compared byte for byte, and so are the segment and checkpoint files of the
//! store. The reference prints nothing on stderr for these steps, and the writer prints nothing.
//!
//! `KNOWN_GAPS` names steps where the Rust writer is known to differ. A listed step must still
//! differ, and a listed step that now matches fails the test so the entry is removed.
//!
//! Zone offsets: the chain fixtures name only `America/Chicago`, at 2026-10-16T02:00Z. The stub
//! below is one fixed UTC-5 (daylight time) offset for that zone at every instant. It is not tzdata:
//! the zoneinfo reader lives in the private `mcao-cli` audit module, which this test cannot reach.
//! The stub is right at the fixture instants and wrong in standard time (see
//! `fixture_offset_stub_is_fixed_at_daylight_time`), so this replay cannot detect a DST divergence.

use std::fs;
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

use mcao_core::audit_writer::{AuditStore, Committed, VerifyReport, WriterError};
use mcao_core::canonical_json::try_to_canonical_string;
use mcao_core::sha256::hex_digest;
use serde_json::{Value, json};

const CHAIN_PREFIX: &str = "outputs/fm-audit-writer/chain/";
const EXPECTED_STEPS: usize = 6;

/// A chain step the Rust writer is known to answer differently from the reference.
struct KnownGap {
    /// The step name, such as `append-0`.
    step: &'static str,
    /// Why the Rust writer differs.
    reason: &'static str,
}

/// No chain step differs from the reference. Add an entry only for a real, documented difference.
const KNOWN_GAPS: &[KnownGap] = &[];

/// Fixed UTC-5 offset for America/Chicago at every instant: right for daylight time, wrong for
/// standard time. See `fixture_offset_stub_is_fixed_at_daylight_time`.
fn fixture_utc_offset(zone: &str, _utc_epoch_secs: i64) -> Option<i32> {
    (zone == "America/Chicago").then_some(-5 * 3600)
}

fn baseline_dir() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../../tests/fixtures/pre-port-baseline")
}

fn repo_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../..")
}

fn read_baseline(relative: &str) -> Vec<u8> {
    let path = baseline_dir().join(relative);
    fs::read(&path).unwrap_or_else(|err| panic!("reading {}: {err}", path.display()))
}

/// A scratch directory, removed when the test ends.
struct Scratch(PathBuf);

impl Scratch {
    fn new(label: &str) -> Self {
        let nanos = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("the clock is after the epoch")
            .as_nanos();
        Self(std::env::temp_dir().join(format!(
            "mcao-audit-writer-chain-{label}-{}-{nanos}",
            std::process::id()
        )))
    }
}

impl Drop for Scratch {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

/// One recorded step, with the paths it names resolved.
struct Step {
    name: String,
    subcommand: String,
    input: Option<PathBuf>,
    expected_stdout: Vec<u8>,
    expected_stderr: Vec<u8>,
    expected_exit: i64,
}

/// Reads the chain steps from the manifest, in manifest order.
fn chain_steps() -> Vec<Step> {
    let manifest_raw =
        fs::read(baseline_dir().join("manifest.json")).expect("reading manifest.json");
    let manifest: Value = serde_json::from_slice(&manifest_raw).expect("parsing manifest.json");
    let steps = manifest["steps"]
        .as_array()
        .expect("manifest steps is an array");
    steps
        .iter()
        .filter_map(|step| {
            let name = step["path"].as_str()?.strip_prefix(CHAIN_PREFIX)?;
            Some(parse_step(step["command"].as_str()?, name))
        })
        .collect()
}

/// Parses one recorded command. Only `append` names an input file, as its last argument.
fn parse_step(command: &str, name: &str) -> Step {
    let tokens: Vec<&str> = command.split_whitespace().collect();
    let tool = tokens
        .iter()
        .position(|token| token.ends_with("fm-audit-writer.py"))
        .expect("chain command names fm-audit-writer.py");
    let subcommand = tokens[tool + 1].to_owned();
    let input = (subcommand == "append").then(|| {
        let last = tokens[tokens.len() - 1];
        if let Some(rel) = last.strip_prefix("<out>/") {
            baseline_dir().join(rel)
        } else if let Some(rel) = last.strip_prefix("<inputs>/") {
            repo_root().join(rel)
        } else {
            panic!("unexpected input token in chain command: {last}")
        }
    });
    Step {
        name: name.to_owned(),
        subcommand,
        input,
        expected_stdout: read_baseline(&format!("{CHAIN_PREFIX}{name}.stdout")),
        expected_stderr: read_baseline(&format!("{CHAIN_PREFIX}{name}.stderr")),
        expected_exit: String::from_utf8(read_baseline(&format!("{CHAIN_PREFIX}{name}.exit")))
            .expect("exit file is UTF-8")
            .trim()
            .parse()
            .expect("exit file holds an integer"),
    }
}

/// The stdout a JSON-result step prints for a committed append.
fn committed_value(committed: &Committed) -> Value {
    json!({"result": "committed", "seq": committed.seq, "event_id": committed.event_id})
}

/// The stdout a JSON-result step prints for a rejected envelope.
fn rejected_value(rejection: &mcao_core::audit_envelope::Rejection) -> Value {
    json!({
        "definition": "envelope",
        "paths": rejection.paths,
        "reason": rejection.reason.as_str(),
        "result": "rejected",
    })
}

/// The stdout a `verify` step prints, with the reference's key names.
fn verify_value(report: &VerifyReport) -> Value {
    json!({
        "committed_seq": report.committed_seq,
        "last_seq": report.last_seq,
        "records": report.records,
        "result": "verified",
        "segments": report.segments.iter().map(|segment| json!({
            "name": segment.name,
            "records": segment.records,
            "status": segment.status,
        })).collect::<Vec<_>>(),
        "torn_tail": report.torn_tail.as_ref().map_or(Value::Null, |torn| json!({
            "bytes": torn.bytes,
            "segment": torn.segment,
        })),
        "uncommitted_seq": report.uncommitted_seq,
    })
}

/// What one step printed: a JSON result value, or the raw ledger bytes.
enum Stdout {
    Json(Value),
    Bytes(Vec<u8>),
}

/// Runs one step against `store`. Returns the exit status and stdout the Rust writer produces.
fn run_step(step: &Step, store: &AuditStore) -> Result<(i64, Stdout), String> {
    match step.subcommand.as_str() {
        "append" => {
            let input = step.input.as_ref().ok_or("append step names no input")?;
            let raw = fs::read(input).map_err(|err| format!("reading input: {err}"))?;
            let envelope: Value =
                serde_json::from_slice(&raw).map_err(|err| format!("parsing input: {err}"))?;
            match store.append(envelope) {
                Ok(committed) => Ok((0, Stdout::Json(committed_value(&committed)))),
                Err(WriterError::Rejected(rejection)) => {
                    Ok((1, Stdout::Json(rejected_value(&rejection))))
                }
                Err(error) => Err(format!("unexpected writer error: {error:?}")),
            }
        }
        "verify" => match store.verify() {
            Ok(report) => Ok((0, Stdout::Json(verify_value(&report)))),
            Err(error) => Err(format!("unexpected writer error: {error:?}")),
        },
        "project-ledger" => match store.ledger_v1_lines() {
            Ok(lines) => {
                let mut stdout = Vec::new();
                for line in lines {
                    stdout.extend_from_slice(line.as_bytes());
                    stdout.push(b'\n');
                }
                Ok((0, Stdout::Bytes(stdout)))
            }
            Err(error) => Err(format!("unexpected writer error: {error:?}")),
        },
        other => Err(format!("unknown writer subcommand {other}")),
    }
}

/// The recorded stdout differs from the observed stdout, or `None` when they match. JSON-result
/// steps compare parsed values; the ledger compares bytes.
fn stdout_mismatch(step: &Step, observed: &Stdout) -> Option<String> {
    match observed {
        Stdout::Json(value) => {
            let expected: Value = match serde_json::from_slice(&step.expected_stdout) {
                Ok(parsed) => parsed,
                Err(err) => return Some(format!("recorded stdout is not JSON: {err}")),
            };
            (&expected != value)
                .then(|| format!("stdout differs: expected {expected}, observed {value}"))
        }
        Stdout::Bytes(bytes) => (bytes.as_slice() != step.expected_stdout.as_slice()).then(|| {
            format!(
                "stdout differs: expected {} bytes, observed {} bytes",
                step.expected_stdout.len(),
                bytes.len()
            )
        }),
    }
}

/// Mismatches of one step, or an empty list when it matches the recorded result.
fn step_mismatches(step: &Step, observed: Result<(i64, Stdout), String>) -> Vec<String> {
    let (exit, stdout) = match observed {
        Ok(result) => result,
        Err(error) => return vec![error],
    };
    let mut mismatches = Vec::new();
    if exit != step.expected_exit {
        mismatches.push(format!(
            "exit differs: expected {}, observed {exit}",
            step.expected_exit
        ));
    }
    if let Some(detail) = stdout_mismatch(step, &stdout) {
        mismatches.push(detail);
    }
    if !step.expected_stderr.is_empty() {
        mismatches.push(format!(
            "recorded stderr is non-empty ({} bytes); the Rust writer prints none",
            step.expected_stderr.len()
        ));
    }
    mismatches
}

fn known_gap(step: &str) -> Option<&'static KnownGap> {
    KNOWN_GAPS.iter().find(|gap| gap.step == step)
}

/// Every committed fixture file under the chain case matches its manifest SHA-256 and size.
#[test]
fn chain_baseline_fixtures_match_the_manifest() {
    let manifest_raw =
        fs::read(baseline_dir().join("manifest.json")).expect("reading manifest.json");
    let manifest: Value = serde_json::from_slice(&manifest_raw).expect("parsing manifest.json");
    let files = manifest["files"]
        .as_object()
        .expect("manifest files is an object");
    let chain_files: Vec<(&String, &Value)> = files
        .iter()
        .filter(|(path, _)| path.starts_with(CHAIN_PREFIX))
        .collect();
    assert!(!chain_files.is_empty(), "the manifest lists no chain files");
    let mut failures = Vec::new();
    for (path, entry) in &chain_files {
        let raw = fs::read(baseline_dir().join(path))
            .unwrap_or_else(|err| panic!("reading {path}: {err}"));
        if Some(hex_digest(&raw).as_str()) != entry["sha256"].as_str()
            || Some(raw.len() as u64) != entry["bytes"].as_u64()
        {
            failures.push(format!("{path}: committed bytes differ from the manifest"));
        }
    }
    assert!(
        failures.is_empty(),
        "fixture integrity:\n{}",
        failures.join("\n")
    );
}

/// Replays the chain case and compares every step and the final store with the baseline.
#[test]
fn chain_baseline_replays_every_recorded_step() {
    let steps = chain_steps();
    assert!(
        !steps.is_empty(),
        "zero chain steps found: the baseline is missing or unreadable"
    );
    assert_eq!(
        steps.len(),
        EXPECTED_STEPS,
        "the manifest chain step count changed"
    );

    let scratch = Scratch::new("replay");
    let store_dir = scratch.0.join("store");
    let store = AuditStore::new(&store_dir, &fixture_utc_offset);

    let mut failures = Vec::new();
    let mut replayed = 0;
    for step in &steps {
        let mismatches = step_mismatches(step, run_step(step, &store));
        replayed += 1;
        match (known_gap(&step.name), mismatches.is_empty()) {
            (Some(_), false) => {}
            (Some(gap), true) => failures.push(format!(
                "{}: listed in KNOWN_GAPS but now matches; remove the entry ({})",
                step.name, gap.reason
            )),
            (None, false) => failures.extend(
                mismatches
                    .into_iter()
                    .map(|detail| format!("{}: {detail}", step.name)),
            ),
            (None, true) => {}
        }
    }
    assert_eq!(replayed, steps.len(), "not every chain step was replayed");

    let mut expected_store: Vec<String> =
        fs::read_dir(baseline_dir().join(format!("{CHAIN_PREFIX}store")))
            .expect("reading baseline store")
            .map(|entry| {
                entry
                    .expect("baseline store entry")
                    .file_name()
                    .into_string()
                    .expect("UTF-8 name")
            })
            .collect();
    expected_store.sort();
    let mut observed_store: Vec<String> = fs::read_dir(&store_dir)
        .map(|entries| {
            entries
                .map(|entry| {
                    entry
                        .expect("store entry")
                        .file_name()
                        .into_string()
                        .expect("UTF-8 name")
                })
                .collect()
        })
        .unwrap_or_default();
    observed_store.sort();
    if expected_store != observed_store {
        failures.push(format!(
            "store files differ: expected {expected_store:?}, observed {observed_store:?}"
        ));
    }
    for name in &expected_store {
        if !observed_store.contains(name) {
            continue;
        }
        let expected = read_baseline(&format!("{CHAIN_PREFIX}store/{name}"));
        let observed = fs::read(store_dir.join(name)).expect("reading store file");
        if expected != observed {
            failures.push(format!("store/{name} differs from the recorded bytes"));
        }
    }

    assert!(
        failures.is_empty(),
        "chain replay mismatches:\n{}",
        failures.join("\n")
    );
}

/// The stub's limit at two America/Chicago instants. The zoneinfo offsets are the rows of
/// `crates/mcao-cli/tests/fixtures/audit-validate/tzif-offsets.tsv`, recorded from CPython 3.14.6:
/// 1782907200 (2026-07-01T12:00Z, daylight) is -18000, and 1772953200 (2026-03-08T07:00Z, before the
/// 08:00Z change to daylight time) is -21600. The stub returns -18000 at both.
#[test]
fn fixture_offset_stub_is_fixed_at_daylight_time() {
    const ZONEINFO_STANDARD_OFFSET: i32 = -21_600;
    assert_eq!(
        fixture_utc_offset("America/Chicago", 1_782_907_200),
        Some(-18_000)
    );
    assert_eq!(
        fixture_utc_offset("America/Chicago", 1_772_953_200),
        Some(-18_000),
        "the stub is fixed at daylight time; zoneinfo gives {ZONEINFO_STANDARD_OFFSET} here"
    );
}

/// Canonical JSON sorts keys by code point, as Python's `sort_keys=True` does, including unsorted
/// and non-BMP keys. Expected bytes come from the Python reference, run as:
///
/// ```text
/// python3 -I -B -X utf8 -c 'import json; t = <the input below>; \
///   print(json.dumps(json.loads(t), sort_keys=True, separators=(",", ":")))'
/// ```
///
/// Run at the repository's baseline interpreter (CPython 3.14.6). The key `"\ud83d\ude00"` (U+1F600)
/// sorts after `"\ue000"` by code point, but before it when sorted by UTF-16 code unit.
#[test]
fn canonical_json_key_sort_matches_the_python_reference() {
    let text = r#"{"z":1,"\u00e9":2,"\ue000":3,"\ud83d\ude00":4,"a":{"b":true,"B":null,"\u00e4":"x"},"9":5,"10":6,"Z":[{"y":1,"x":2}]}"#;
    let value: Value = serde_json::from_str(text).expect("golden input parses");
    assert_eq!(
        try_to_canonical_string(&value).expect("golden input is in range"),
        r#"{"10":6,"9":5,"Z":[{"x":2,"y":1}],"a":{"B":null,"b":true,"\u00e4":"x"},"z":1,"\u00e9":2,"\ue000":3,"\ud83d\ude00":4}"#
    );
}
