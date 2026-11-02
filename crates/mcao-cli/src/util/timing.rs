//! `mcao util aggregate-timing-json` and `mcao util write-timing-json`: the timing artifacts
//! `bin/fm-test-run.sh` writes. Each is a port of a python3 heredoc, and the JSON it emits is
//! byte-identical to `json.dump(doc, indent=2, sort_keys=True)` with `ensure_ascii=True`.

use std::fmt::Write as _;
use std::fs;
use std::path::Path;
use std::process::ExitCode;

use serde_json::{Map, Value, json};

const AGGREGATE_USAGE: &str = "usage: mcao util aggregate-timing-json <out> <timing.json>...\n";
const WRITE_USAGE: &str = "usage: mcao util write-timing-json <out> <started> <finished> \
<run-id> <total> <failed> <skipped> <duration-ms> <selection> <records-file> <families-file>\n";

const SLOWEST_COUNT: usize = 15;

pub fn aggregate(args: &[String]) -> ExitCode {
    match args {
        [out, inputs @ ..] if !inputs.is_empty() => report(aggregate_files(out, inputs)),
        _ => usage(AGGREGATE_USAGE),
    }
}

pub fn write(args: &[String]) -> ExitCode {
    match args {
        [
            out,
            started,
            finished,
            run_id,
            total,
            failed,
            skipped,
            duration,
            selection,
            records,
            families,
        ] => {
            let fields = ArtifactFields {
                started,
                finished,
                run_id,
                selection,
                total,
                failed,
                skipped,
                duration,
            };
            report(write_artifact(out, &fields, records, families))
        }
        _ => usage(WRITE_USAGE),
    }
}

fn usage(text: &str) -> ExitCode {
    eprint!("{text}");
    ExitCode::from(2)
}

fn report(result: Result<Option<String>, String>) -> ExitCode {
    match result {
        Ok(line) => {
            if let Some(line) = line {
                println!("{line}");
            }
            ExitCode::SUCCESS
        }
        Err(message) => {
            eprintln!("mcao util: {message}");
            ExitCode::from(1)
        }
    }
}

/// Merges lane timing artifacts into one aggregate document at `out` and returns the summary
/// line the shell script prints.
fn aggregate_files(out: &str, inputs: &[String]) -> Result<Option<String>, String> {
    let mut lanes = Vec::new();
    let mut scripts: Vec<(i64, String, Map<String, Value>)> = Vec::new();
    let mut total = 0;
    let mut failed = 0;
    let mut skipped = 0;
    let mut wall_ms = 0;
    for input in inputs {
        let path = pathlib_normal(input);
        let text = fs::read_to_string(&path).map_err(|err| format!("cannot read {path}: {err}"))?;
        let doc = parse_object(&text, &path)?;
        let summary = object_or_empty(doc.get("summary"), "summary")?;
        total += py_int(summary.get("total"))?;
        failed += py_int(summary.get("failed"))?;
        skipped += py_int(summary.get("skipped_gate"))?;
        wall_ms = wall_ms.max(py_int(summary.get("duration_ms"))?);
        let selection = doc.get("selection").cloned().unwrap_or(Value::Null);
        let run_id = doc.get("run_id").cloned().unwrap_or(Value::Null);
        lanes.push(json!({
            "path": path,
            "run_id": run_id,
            "selection": selection,
            "started_at": doc.get("started_at").cloned().unwrap_or(Value::Null),
            "finished_at": doc.get("finished_at").cloned().unwrap_or(Value::Null),
            "summary": Value::Object(summary),
        }));
        for script in array_or_empty(doc.get("scripts"), "scripts")? {
            let Value::Object(mut row) = script else {
                return Err(format!("{input}: a script entry is not an object"));
            };
            let duration = py_int(row.get("duration_ms"))?;
            let path = py_str(row.get("path"))?;
            row.insert("lane_selection".to_owned(), selection.clone());
            row.insert("lane_run_id".to_owned(), run_id.clone());
            scripts.push((duration, path, row));
        }
    }

    scripts.sort_by(|a, b| b.0.cmp(&a.0).then_with(|| a.1.cmp(&b.1)));
    let rows: Vec<Value> = scripts
        .into_iter()
        .map(|(_, _, row)| Value::Object(row))
        .collect();
    let slowest: Vec<Value> = rows.iter().take(SLOWEST_COUNT).cloned().collect();
    let aggregate = json!({
        "kind": "aggregate",
        "lanes": lanes,
        "summary": {
            "lanes": inputs.len(),
            "total": total,
            "failed": failed,
            "skipped_gate": skipped,
            "critical_path_duration_ms": wall_ms,
        },
        "scripts": rows,
        "slowest": slowest,
    });
    write_document(out, &aggregate)?;
    Ok(Some(format!(
        "FM_TEST_AGGREGATE lanes={} total={total} failed={failed} skipped_gate={skipped} critical_path_duration_ms={wall_ms}",
        inputs.len()
    )))
}

/// Normalizes a lane path the way `str(pathlib.Path(p))` does on POSIX: redundant separators,
/// `.` components, and a trailing slash are dropped, a leading `//` is kept, and `..` is left
/// alone. The Python reader opened the normalized path too, so inputs are read through it.
fn pathlib_normal(path: &str) -> String {
    let root = if path.starts_with("//") && !path.starts_with("///") {
        "//"
    } else if path.starts_with('/') {
        "/"
    } else {
        ""
    };
    let joined = path
        .split('/')
        .filter(|part| !part.is_empty() && *part != ".")
        .collect::<Vec<_>>()
        .join("/");
    if root.is_empty() && joined.is_empty() {
        ".".to_owned()
    } else {
        format!("{root}{joined}")
    }
}

struct ArtifactFields<'a> {
    started: &'a str,
    finished: &'a str,
    run_id: &'a str,
    selection: &'a str,
    total: &'a str,
    failed: &'a str,
    skipped: &'a str,
    duration: &'a str,
}

/// Writes the timing artifact for one run from its scalar fields and the two tab-separated
/// record files the runner accumulates.
fn write_artifact(
    out: &str,
    fields: &ArtifactFields,
    records_file: &str,
    families_file: &str,
) -> Result<Option<String>, String> {
    let mut scripts = Vec::new();
    for row in read_rows(records_file, 7)? {
        scripts.push(json!({
            "path": row[0],
            "family": row[1],
            "expected_gate_skip": row[2],
            "duration_ms": parse_int(&row[4], "duration")?,
            "exit": parse_int(&row[3], "exit")?,
            "gate_skip": row[5] == "true",
            "gate_skip_reason": row[6],
        }));
    }
    let mut families = Vec::new();
    for row in read_rows(families_file, 4)? {
        families.push(json!({
            "name": row[0],
            "count": parse_int(&row[1], "count")?,
            "duration_ms": parse_int(&row[2], "duration")?,
            "failed": parse_int(&row[3], "failed")?,
        }));
    }
    let document = json!({
        "run_id": fields.run_id,
        "started_at": fields.started,
        "finished_at": fields.finished,
        "selection": fields.selection,
        "summary": {
            "total": parse_int(fields.total, "total")?,
            "failed": parse_int(fields.failed, "failed")?,
            "skipped_gate": parse_int(fields.skipped, "skipped")?,
            "duration_ms": parse_int(fields.duration, "duration")?,
        },
        "scripts": scripts,
        "families": families,
    });
    write_document(out, &document)?;
    Ok(None)
}

/// Splits a record file into rows of `columns` tab-separated fields. Blank lines are skipped
/// and CR and CRLF line endings read as LF, as Python's text mode does.
fn read_rows(path: &str, columns: usize) -> Result<Vec<Vec<String>>, String> {
    let text = fs::read_to_string(path).map_err(|err| format!("cannot read {path}: {err}"))?;
    let mut rows = Vec::new();
    for line in text.replace("\r\n", "\n").replace('\r', "\n").split('\n') {
        if line.is_empty() {
            continue;
        }
        let fields: Vec<String> = line.split('\t').map(str::to_owned).collect();
        if fields.len() != columns {
            return Err(format!(
                "{path}: expected {columns} tab-separated fields, found {}",
                fields.len()
            ));
        }
        rows.push(fields);
    }
    Ok(rows)
}

/// Writes `document` as `json.dump(indent=2, sort_keys=True)` plus a trailing newline.
fn write_document(out: &str, document: &Value) -> Result<(), String> {
    let pretty = serde_json::to_string_pretty(document).map_err(|err| err.to_string())?;
    let mut text = escape_non_ascii(&pretty);
    text.push('\n');
    let parent = Path::new(out)
        .parent()
        .filter(|parent| !parent.as_os_str().is_empty());
    if let Some(parent) = parent {
        fs::create_dir_all(parent)
            .map_err(|err| format!("cannot create {}: {err}", parent.display()))?;
    }
    fs::write(out, text).map_err(|err| format!("cannot write {out}: {err}"))
}

/// `ensure_ascii=True`: every character from DEL upward becomes `\uXXXX`, with surrogate pairs
/// above the BMP. serde_json leaves those characters raw, so they are escaped here.
fn escape_non_ascii(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    for c in text.chars() {
        if (c as u32) < 0x7f {
            out.push(c);
            continue;
        }
        let mut units = [0u16; 2];
        for unit in c.encode_utf16(&mut units) {
            let _ = write!(out, "\\u{unit:04x}");
        }
    }
    out
}

fn parse_object(text: &str, origin: &str) -> Result<Map<String, Value>, String> {
    match serde_json::from_str::<Value>(text) {
        Ok(Value::Object(map)) => Ok(map),
        Ok(_) => Err(format!("{origin}: the timing document is not an object")),
        Err(err) => Err(format!("{origin}: {err}")),
    }
}

/// `doc.get(key) or {}`: a missing or falsy value reads as an empty object.
fn object_or_empty(value: Option<&Value>, key: &str) -> Result<Map<String, Value>, String> {
    match value {
        None => Ok(Map::new()),
        Some(Value::Object(map)) => Ok(map.clone()),
        Some(value) if is_falsy(value) => Ok(Map::new()),
        Some(_) => Err(format!("{key} is not an object")),
    }
}

/// `doc.get(key) or []`, as an owned list.
fn array_or_empty(value: Option<&Value>, key: &str) -> Result<Vec<Value>, String> {
    match value {
        None => Ok(Vec::new()),
        Some(Value::Array(items)) => Ok(items.clone()),
        Some(value) if is_falsy(value) => Ok(Vec::new()),
        Some(_) => Err(format!("{key} is not a list")),
    }
}

fn is_falsy(value: &Value) -> bool {
    match value {
        Value::Null => true,
        Value::Bool(flag) => !flag,
        Value::Number(number) => number.as_f64() == Some(0.0),
        Value::String(text) => text.is_empty(),
        Value::Array(items) => items.is_empty(),
        Value::Object(map) => map.is_empty(),
    }
}

/// `int(value or 0)` for a value read from a timing document.
fn py_int(value: Option<&Value>) -> Result<i64, String> {
    let Some(value) = value.filter(|value| !is_falsy(value)) else {
        return Ok(0);
    };
    match value {
        Value::Bool(_) => Ok(1),
        Value::Number(number) => match (number.as_i64(), number.as_f64()) {
            (Some(int), _) => Ok(int),
            (None, Some(float)) => Ok(float.trunc() as i64),
            (None, None) => Err("integer out of range".to_owned()),
        },
        Value::String(text) => parse_int(text, "value"),
        _ => Err("value is not a number".to_owned()),
    }
}

/// `value or ""` for a sort key: the path must compare as text.
fn py_str(value: Option<&Value>) -> Result<String, String> {
    match value {
        None => Ok(String::new()),
        Some(Value::String(text)) => Ok(text.clone()),
        Some(value) if is_falsy(value) => Ok(String::new()),
        Some(_) => Err("script path is not a string".to_owned()),
    }
}

/// `int(text)` for a field of the runner's record files.
fn parse_int(text: &str, what: &str) -> Result<i64, String> {
    text.trim()
        .parse::<i64>()
        .map_err(|_| format!("{what} is not an integer: {text:?}"))
}
