//! Differential tests of the no-mistakes readers against the Python programs they mirror.
//!
//! The oracle is the Python source of the two shell scripts, copied verbatim into
//! `tests/fixtures/nm-state/oracle`. The committed goldens in `tests/fixtures/nm-state/golden` are
//! always checked, so CI without Python still checks the Rust side. When `python3` is on the path
//! and its SQLite is the golden manifest's `python_sqlite`, every case also runs the oracle live.
//! `MCAO_NM_STATE_REGENERATE=1` reseeds the databases with `seed.py` and rewrites the goldens from
//! the live Python run (see MANIFEST.json).
//!
//! A case listed in `KNOWN_GAPS` must NOT match the oracle. If it starts matching, the test fails
//! and the gap must be removed from the table and from the pull request body.

use std::collections::{BTreeMap, BTreeSet};
use std::env;
use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::SystemTime;

use serde_json::{Value, json};

use super::py;
use super::{
    InventoryRequest, LedgerError, SpendRecordError, SpendRequest, read_run_inventory,
    record_spend, render_run_inventory, render_spend_outcome,
};

const FIXTURES: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/tests/fixtures/nm-state");
const REGENERATE: &str = "MCAO_NM_STATE_REGENERATE";
/// The `time.time()` the oracle sees, and the `recorded_at` the Rust readers take.
const NOW: i64 = 1_788_000_000;
const REPO_PATH: &str = "/work/repo";
const LEDGER_NAME: &str = "pipeline-spend.jsonl";

/// One differential case: a database fixture plus the arguments both programs receive.
struct Case {
    name: &'static str,
    /// Directory under `db/` copied into the work directory; `None` leaves no database file.
    fixture: Option<&'static str>,
    /// Replaces the work directory's `state.sqlite` as the database argument.
    database: Option<&'static str>,
    overview: &'static str,
    branch: &'static str,
    available_ids: &'static str,
    task: &'static str,
    spawn_gen: &'static str,
    repo: &'static str,
    since: &'static str,
    reason: &'static str,
    /// Bytes the ledger holds before the spend run; empty means no ledger file.
    ledger: &'static [u8],
    /// Builds the ledger line instead of `ledger`, for lines too long to write as a literal.
    nesting: Option<Nesting>,
    /// Makes the work directory read-only after the copy, so no sidecar can be created.
    read_only_dir: bool,
}

/// A ledger line of nested containers, `depth` levels deep.
#[derive(Clone, Copy)]
enum Nesting {
    /// `[[...]]`.
    Array(usize),
    /// Objects under the `task` and `spawn_gen` of `DEFAULT`, so the line is a duplicate of it when
    /// the decoder accepts it.
    Object(usize),
}

impl Nesting {
    fn line(self) -> Vec<u8> {
        let mut text = String::new();
        match self {
            Nesting::Array(depth) => {
                text.push_str(&"[".repeat(depth));
                text.push_str(&"]".repeat(depth));
            }
            Nesting::Object(depth) => {
                text.push_str("{\"task\":\"task-1\",\"spawn_gen\":\"gen-1\",\"n\":");
                text.push_str(&"{\"n\":".repeat(depth - 1));
                text.push('0');
                text.push_str(&"}".repeat(depth));
            }
        }
        text.push('\n');
        text.into_bytes()
    }
}

const OVERVIEW: &str = "count: 3 of 3 total\nrepo: /work/repo\n";

const DEFAULT: Case = Case {
    name: "",
    fixture: Some("wal-at-rest"),
    database: None,
    overview: OVERVIEW,
    branch: "feat/x",
    available_ids: "run-new, run-mid, run-old",
    task: "task-1",
    spawn_gen: "gen-1",
    repo: REPO_PATH,
    since: "",
    reason: "",
    ledger: b"",
    nesting: None,
    read_only_dir: false,
};

const CASES: &[Case] = &[
    Case { name: "wal-at-rest", ..DEFAULT },
    Case {
        name: "wal-live",
        fixture: Some("wal-live"),
        available_ids: "run-live, run-new",
        ..DEFAULT
    },
    Case {
        name: "rollback-journal",
        fixture: Some("rollback-journal"),
        ..DEFAULT
    },
    Case { name: "pre-delta-era", fixture: Some("pre-delta-era"), ..DEFAULT },
    Case {
        name: "pre-delta-no-deltas",
        fixture: Some("pre-delta-no-deltas"),
        available_ids: "run-d1",
        ..DEFAULT
    },
    Case {
        name: "created-at-ties",
        fixture: Some("created-at-ties"),
        available_ids: "run-t1, run-t4",
        ..DEFAULT
    },
    Case {
        name: "missing-repos-table",
        fixture: Some("missing-repos-table"),
        available_ids: "",
        ..DEFAULT
    },
    Case {
        name: "missing-runs-table",
        fixture: Some("missing-runs-table"),
        available_ids: "",
        ..DEFAULT
    },
    Case {
        name: "missing-agent-invocations-table",
        fixture: Some("missing-agent-invocations-table"),
        available_ids: "run-a",
        ..DEFAULT
    },
    Case {
        name: "empty-runs",
        fixture: Some("empty-runs"),
        available_ids: "",
        ..DEFAULT
    },
    Case {
        name: "zero-row-repo",
        fixture: Some("zero-row-repo"),
        available_ids: "run-a",
        ..DEFAULT
    },
    Case {
        name: "many-runs",
        fixture: Some("many-runs"),
        overview: "count: 10 of 60 total\nrepo: /work/repo\n",
        available_ids: "run-059, run-058, run-057, run-056, run-055, run-054, run-053, run-052, run-051, run-050",
        ..DEFAULT
    },
    Case {
        name: "unicode-names",
        fixture: Some("unicode-names"),
        branch: "feat/ünï-🚀-漢字",
        available_ids: "run-u2, run-u1",
        task: "tâsk-🚀",
        ..DEFAULT
    },
    Case {
        name: "control-chars",
        fixture: Some("control-chars"),
        branch: "feat/\t\u{1}\u{7f}\u{8}\u{c}\r\n\"\\x",
        available_ids: "run-c1",
        ..DEFAULT
    },
    Case { name: "nul-bytes", fixture: Some("nul-bytes"), available_ids: "run-n1", ..DEFAULT },
    Case { name: "token-values", fixture: Some("token-values"), available_ids: "run-t1", ..DEFAULT },
    Case { name: "huge-sums", fixture: Some("huge-sums"), available_ids: "run-h1", ..DEFAULT },
    Case {
        name: "float-duration",
        fixture: Some("float-duration"),
        available_ids: "run-f1",
        ..DEFAULT
    },
    Case { name: "text-token", fixture: Some("text-token"), available_ids: "run-x1", ..DEFAULT },
    Case { name: "blob-token", fixture: Some("blob-token"), available_ids: "run-b1", ..DEFAULT },
    Case {
        name: "invalid-utf8-status",
        fixture: Some("invalid-utf8-status"),
        available_ids: "run-s1",
        ..DEFAULT
    },
    Case {
        name: "invalid-utf8-purpose",
        fixture: Some("invalid-utf8-purpose"),
        available_ids: "run-p1",
        ..DEFAULT
    },
    Case { name: "not-sqlite", fixture: Some("not-sqlite"), ..DEFAULT },
    Case {
        name: "created-at-real",
        fixture: Some("created-at-real"),
        available_ids: "run-r1, run-r2",
        ..DEFAULT
    },
    Case {
        name: "created-at-text",
        fixture: Some("created-at-text"),
        available_ids: "run-tx",
        ..DEFAULT
    },
    Case {
        name: "created-at-overflow",
        fixture: Some("created-at-overflow"),
        available_ids: "run-o1",
        ..DEFAULT
    },
    Case {
        name: "purposes-mixed",
        fixture: Some("purposes-mixed"),
        available_ids: "run-m1",
        ..DEFAULT
    },
    Case {
        name: "purpose-null-only",
        fixture: Some("purpose-null-only"),
        available_ids: "run-n2",
        ..DEFAULT
    },
    Case {
        name: "purpose-numeric",
        fixture: Some("purpose-numeric"),
        available_ids: "run-q1",
        ..DEFAULT
    },
    Case { name: "exit-types", fixture: Some("exit-types"), available_ids: "run-e1", ..DEFAULT },
    Case {
        name: "resumed-sessions",
        fixture: Some("resumed-sessions"),
        available_ids: "run-z1",
        ..DEFAULT
    },
    Case {
        name: "missing-started-at",
        fixture: Some("missing-started-at"),
        available_ids: "run-l1",
        ..DEFAULT
    },
    Case {
        name: "since-boundary",
        fixture: Some("since-boundary"),
        available_ids: "run-after, run-at, run-before",
        since: "1700000000",
        ..DEFAULT
    },
    Case {
        name: "since-unicode-digit",
        fixture: Some("wal-at-rest"),
        since: "\u{663}",
        ..DEFAULT
    },
    Case { name: "since-invalid", fixture: Some("wal-at-rest"), since: "abc", ..DEFAULT },
    Case { name: "db-missing", fixture: None, available_ids: "", ..DEFAULT },
    Case {
        name: "db-relative",
        fixture: Some("wal-at-rest"),
        database: Some("relative/state.sqlite"),
        ..DEFAULT
    },
    Case {
        name: "read-only-dir",
        fixture: Some("wal-at-rest"),
        read_only_dir: true,
        ..DEFAULT
    },
    Case {
        name: "spend-reason-given",
        fixture: None,
        reason: "no-mistakes is not installed",
        database: Some(""),
        ..DEFAULT
    },
    Case {
        name: "repo-json-quoted",
        overview: "repo: \"/work/repo\"\n",
        ..DEFAULT
    },
    Case {
        name: "repo-json-lone-surrogate",
        overview: "repo: \"/\\ud800\"\n",
        available_ids: "run-new",
        ..DEFAULT
    },
    Case {
        name: "repo-raw-relative",
        overview: "repo: work/repo\n",
        available_ids: "run-new",
        ..DEFAULT
    },
    Case {
        name: "repo-two-lines",
        overview: "repo: /work/repo\nrepo: /work/repo\n",
        available_ids: "run-new",
        ..DEFAULT
    },
    Case {
        name: "overview-id-missing",
        available_ids: "run-new, ghost",
        ..DEFAULT
    },
    Case {
        name: "bad-head",
        fixture: Some("bad-head"),
        available_ids: "run-hd1, run-hd2",
        ..DEFAULT
    },
    Case {
        name: "ledger-duplicate",
        ledger: b"{\"task\":\"other\",\"spawn_gen\":\"gen-1\"}\n{\"task\":\"task-1\",\"spawn_gen\":\"gen-1\",\"x\":1}\n",
        ..DEFAULT
    },
    Case {
        name: "ledger-null-generation",
        spawn_gen: "",
        ledger: b"{\"task\":\"task-1\"}\n",
        ..DEFAULT
    },
    Case {
        name: "ledger-no-trailing-newline",
        ledger: b"{\"task\":\"other\"}",
        ..DEFAULT
    },
    Case {
        name: "ledger-crlf-ending",
        ledger: b"{\"task\":\"other\"}\r\n{\"task\":\"x\"}\r",
        ..DEFAULT
    },
    Case {
        name: "ledger-invalid-utf8",
        ledger: b"{\"task\":\"other\"}\n\xff\n",
        ..DEFAULT
    },
    Case {
        name: "ledger-malformed-lines",
        ledger: b"garbage\n[1,2]\nNaN\n{\"task\":\"task-1\",\"spawn_gen\":\"gen-1\"\n{\"task\": \"task-1\", \"spawn_gen\": NaN}\n{\"task\":\"other\",\"spawn_gen\":\"gen-1\"}\n",
        ..DEFAULT
    },
    Case {
        name: "ledger-last-key-wins",
        ledger: b"{\"task\":\"zzz\",\"task\":\"task-1\",\"spawn_gen\":\"gen-1\"}\n",
        ..DEFAULT
    },
    Case {
        name: "ledger-nan-record",
        ledger: b"{\"task\":\"task-1\",\"spawn_gen\":\"gen-1\",\"total\":NaN,\"x\":-Infinity}\n",
        ..DEFAULT
    },
    Case {
        name: "ledger-vertical-tab-splits-line",
        ledger: b"{\"task\":\"task-1\"\x0b,\"spawn_gen\":\"gen-1\"}\n",
        ..DEFAULT
    },
    Case {
        name: "ledger-unicode-separator",
        ledger: "{\"task\":\"task-1\",\"spawn_gen\":\"gen-1\"}\u{2028}".as_bytes(),
        ..DEFAULT
    },
    Case {
        name: "ledger-deep-nesting-array-1001",
        nesting: Some(Nesting::Array(1001)),
        ..DEFAULT
    },
    Case {
        name: "ledger-deep-nesting-array-2000",
        nesting: Some(Nesting::Array(2000)),
        ..DEFAULT
    },
    Case {
        name: "ledger-deep-nesting-object-1001",
        nesting: Some(Nesting::Object(1001)),
        ..DEFAULT
    },
    Case {
        name: "ledger-deep-nesting-object-2000",
        nesting: Some(Nesting::Object(2000)),
        ..DEFAULT
    },
];

/// The `ledger-deep-nesting` gap: a ledger line nested deeper than `MAX_DEPTH` (1000). This port
/// refuses such a line with exit 1 and no record. CPython's `json.loads` decodes it, and the
/// measured limit is interpreter dependent: CPython 3.14.6 accepts array nesting to 116187 and
/// fails at 116188; CPython 3.9.6 fails from 995 (its limit counts the caller's stack). The spend
/// record never nests deeper than 4 levels (record, runs, run, exit), so only a corrupted or hostile
/// ledger can hold such a line. The port does not match the threshold; it does not overflow the
/// stack either (see `ledger_line_nested_200000_levels_is_refused_without_overflow`).
const LEDGER_DEEP_NESTING: &str = "a ledger line nested beyond 1000 levels: this port exits 1 without a record; CPython's json.loads decodes it (its depth limit is interpreter dependent, 116188 on 3.14.6, 995 on 3.9.6)";

/// Cases whose output must differ from the oracle, with the reason. A listed case that starts
/// matching fails the test, so a fixed gap leaves this table and the pull request body.
const KNOWN_GAPS: &[(&str, &str)] = &[
    (
        "created-at-overflow",
        "created_at beyond the years gmtime represents: CPython reports the errno text of an OSError, \
         which is platform specific; this port exits 1 instead of recording unavailable",
    ),
    (
        "since-unicode-digit",
        "since is decimal text only in the shell; CPython's int() also accepts non-ASCII decimal digits, \
         which this port rejects",
    ),
    (
        "missing-started-at",
        "the bundled SQLite 3.54.1 appends ' in <statement>' to a prepare error text; CPython's SQLite \
         3.53.4 and 3.51.0 report 'no such column: started_at' alone",
    ),
    ("ledger-deep-nesting-array-1001", LEDGER_DEEP_NESTING),
    ("ledger-deep-nesting-array-2000", LEDGER_DEEP_NESTING),
    ("ledger-deep-nesting-object-1001", LEDGER_DEEP_NESTING),
    ("ledger-deep-nesting-object-2000", LEDGER_DEEP_NESTING),
];

fn known_gap(name: &str) -> Option<&'static str> {
    KNOWN_GAPS
        .iter()
        .find(|(case, _)| *case == name)
        .map(|(_, reason)| *reason)
}

/// The bytes and the decisions one program run produced.
#[derive(Debug, Clone)]
struct Output {
    stdout: Vec<u8>,
    exit: i32,
    /// Names created in the work directory by the run.
    created: BTreeSet<String>,
    /// The ledger after a spend run, when the ledger exists.
    ledger: Option<Vec<u8>>,
    /// The Rust error behind an exit 1 or a refused inventory, for failure messages only.
    detail: String,
}

/// Equality is the observable behavior: stdout, exit, created names and the ledger. The detail is
/// Rust-side diagnostics that the oracle has no counterpart for.
impl PartialEq for Output {
    fn eq(&self, other: &Output) -> bool {
        self.stdout == other.stdout
            && self.exit == other.exit
            && self.created == other.created
            && self.ledger == other.ledger
    }
}

/// A work directory holding a copy of one fixture, removed on drop.
struct Work {
    dir: PathBuf,
    read_only: bool,
}

static COUNTER: AtomicUsize = AtomicUsize::new(0);

impl Work {
    fn new(case: &Case) -> Work {
        let index = COUNTER.fetch_add(1, Ordering::SeqCst);
        let dir = env::temp_dir().join(format!(
            "mcao-nm-state-{}-{index}-{}",
            std::process::id(),
            case.name
        ));
        fs::create_dir_all(&dir).expect("create work directory");
        if let Some(fixture) = case.fixture {
            let source = Path::new(FIXTURES).join("db").join(fixture);
            for entry in fs::read_dir(&source).expect("fixture directory") {
                let entry = entry.expect("fixture entry");
                fs::copy(entry.path(), dir.join(entry.file_name())).expect("copy fixture");
            }
        }
        let ledger = match case.nesting {
            Some(nesting) => nesting.line(),
            None => case.ledger.to_vec(),
        };
        if !ledger.is_empty() {
            fs::write(dir.join(LEDGER_NAME), ledger).expect("write ledger");
        }
        if case.read_only_dir {
            fs::set_permissions(&dir, fs::Permissions::from_mode(0o555)).expect("chmod work");
        }
        Work {
            dir,
            read_only: case.read_only_dir,
        }
    }

    fn database(&self) -> PathBuf {
        self.dir.join("state.sqlite")
    }

    fn ledger(&self) -> PathBuf {
        self.dir.join(LEDGER_NAME)
    }

    fn snapshot(&self) -> BTreeMap<String, (Vec<u8>, SystemTime)> {
        let mut files = BTreeMap::new();
        for entry in fs::read_dir(&self.dir).expect("read work directory") {
            let entry = entry.expect("work entry");
            let meta = entry.metadata().expect("work metadata");
            if meta.is_file() {
                let name = entry.file_name().to_string_lossy().into_owned();
                let bytes = fs::read(entry.path()).expect("read work file");
                files.insert(name, (bytes, meta.modified().expect("mtime")));
            }
        }
        files
    }
}

impl Drop for Work {
    fn drop(&mut self) {
        if self.read_only {
            let _ = fs::set_permissions(&self.dir, fs::Permissions::from_mode(0o755));
        }
        let _ = fs::remove_dir_all(&self.dir);
    }
}

/// Which program produced an output.
#[derive(Clone, Copy, PartialEq)]
enum Program {
    Rust,
    Python,
}

fn names_after(before: &BTreeMap<String, (Vec<u8>, SystemTime)>, work: &Work) -> BTreeSet<String> {
    work.snapshot()
        .keys()
        .filter(|name| !before.contains_key(*name))
        .cloned()
        .collect()
}

/// Asserts that the run left every database file it found as it was: bytes and mtime. Sidecars the
/// run created are allowed; they are compared with the oracle's.
fn assert_database_untouched(
    before: &BTreeMap<String, (Vec<u8>, SystemTime)>,
    after: &BTreeMap<String, (Vec<u8>, SystemTime)>,
    label: &str,
) {
    for (name, (bytes, mtime)) in before {
        if name.starts_with("state.sqlite") {
            let (after_bytes, after_mtime) = after
                .get(name)
                .unwrap_or_else(|| panic!("{label}: {name} disappeared"));
            assert_eq!(bytes, after_bytes, "{label}: {name} bytes changed");
            assert_eq!(mtime, after_mtime, "{label}: {name} mtime changed");
        }
    }
}

fn rust_inventory(case: &Case, work: &Work) -> Output {
    let before = work.snapshot();
    let database = case
        .database
        .map(PathBuf::from)
        .unwrap_or_else(|| work.database());
    let request = InventoryRequest {
        database: &database,
        branch: case.branch,
        overview: case.overview,
        available_ids: case.available_ids,
    };
    let outcome = read_run_inventory(&request);
    let detail = match &outcome {
        Err(failure) => failure.error.to_string(),
        Ok(_) => String::new(),
    };
    let stdout = render_run_inventory(&outcome).into_bytes();
    let created = names_after(&before, work);
    assert_database_untouched(&before, &work.snapshot(), "rust inventory");
    Output {
        stdout,
        exit: 0,
        created,
        ledger: None,
        detail,
    }
}

fn rust_spend(case: &Case, work: &Work) -> Output {
    let before = work.snapshot();
    let database = case
        .database
        .map(str::to_owned)
        .unwrap_or_else(|| work.database().to_string_lossy().into_owned());
    let request = SpendRequest {
        task: case.task,
        spawn_gen: case.spawn_gen,
        repo: case.repo,
        branch: case.branch,
        since: case.since,
        database: &database,
        reason: case.reason,
    };
    let (stdout, exit, detail) = match record_spend(&request, &work.ledger(), NOW) {
        Ok(outcome) => (
            render_spend_outcome(outcome, case.task, case.spawn_gen).into_bytes(),
            0,
            String::new(),
        ),
        Err(error) => (Vec::new(), 1, error.to_string()),
    };
    let created = names_after(&before, work);
    assert_database_untouched(&before, &work.snapshot(), "rust spend");
    Output {
        stdout,
        exit,
        created,
        ledger: fs::read(work.ledger()).ok(),
        detail,
    }
}

fn python_available() -> bool {
    Command::new("python3")
        .arg("--version")
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .is_ok_and(|status| status.success())
}

/// Runs an oracle program with `sys.time` pinned to `NOW`. The wrapper pops the program path
/// from argv so the program sees exactly the shell's arguments.
fn python_program(program: &str, args: &[&str]) -> (Vec<u8>, i32) {
    const WRAPPER: &str = "import sys, time\n\
        program = sys.argv.pop(1)\n\
        time.time = lambda: 1788000000.0\n\
        with open(program, encoding='utf-8') as handle:\n    source = handle.read()\n\
        exec(compile(source, program, 'exec'), {'__name__': '__main__', '__file__': program})\n";
    let oracle = Path::new(FIXTURES).join("oracle").join(program);
    let output = Command::new("python3")
        .arg("-c")
        .arg(WRAPPER)
        .arg(&oracle)
        .args(args)
        .stdin(Stdio::null())
        .output()
        .expect("run python3");
    (output.stdout, output.status.code().unwrap_or(-1))
}

fn python_inventory(case: &Case, work: &Work) -> Output {
    let before = work.snapshot();
    let database = case
        .database
        .map(str::to_owned)
        .unwrap_or_else(|| work.database().to_string_lossy().into_owned());
    let (stdout, exit) = python_program(
        "inventory.py",
        &[case.branch, case.overview, case.available_ids, &database],
    );
    let created = names_after(&before, work);
    assert_database_untouched(&before, &work.snapshot(), "python inventory");
    Output {
        stdout,
        exit,
        created,
        ledger: None,
        detail: String::new(),
    }
}

fn python_spend(case: &Case, work: &Work) -> Output {
    let before = work.snapshot();
    let database = case
        .database
        .map(str::to_owned)
        .unwrap_or_else(|| work.database().to_string_lossy().into_owned());
    let ledger = work.ledger().to_string_lossy().into_owned();
    let (stdout, exit) = python_program(
        "spend.py",
        &[
            &ledger,
            case.task,
            case.spawn_gen,
            case.repo,
            case.branch,
            case.since,
            &database,
            case.reason,
        ],
    );
    let created = names_after(&before, work);
    assert_database_untouched(&before, &work.snapshot(), "python spend");
    Output {
        stdout,
        exit,
        created,
        ledger: fs::read(work.ledger()).ok(),
        detail: String::new(),
    }
}

/// Runs the inventory and the spend program of `case` in fresh work directories.
fn run_pair(case: &Case, program: Program) -> (Output, Output) {
    let inventory_work = Work::new(case);
    let spend_work = Work::new(case);
    match program {
        Program::Rust => (
            rust_inventory(case, &inventory_work),
            rust_spend(case, &spend_work),
        ),
        Program::Python => (
            python_inventory(case, &inventory_work),
            python_spend(case, &spend_work),
        ),
    }
}

fn output_to_json(output: &Output) -> Value {
    json!({
        "exit": output.exit,
        "stdout": String::from_utf8(output.stdout.clone()).expect("oracle stdout is UTF-8"),
        "created": output.created.iter().collect::<Vec<_>>(),
        "ledger_hex": output.ledger.as_ref().map(|bytes| hex(bytes)),
    })
}

fn output_from_json(value: &Value) -> Output {
    Output {
        exit: value["exit"].as_i64().expect("golden exit") as i32,
        stdout: value["stdout"]
            .as_str()
            .expect("golden stdout")
            .as_bytes()
            .to_vec(),
        created: value["created"]
            .as_array()
            .expect("golden created")
            .iter()
            .map(|name| name.as_str().expect("created name").to_owned())
            .collect(),
        ledger: value["ledger_hex"].as_str().map(unhex),
        detail: String::new(),
    }
}

fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|byte| format!("{byte:02x}")).collect()
}

fn unhex(text: &str) -> Vec<u8> {
    (0..text.len())
        .step_by(2)
        .map(|at| u8::from_str_radix(&text[at..at + 2], 16).expect("golden hex"))
        .collect()
}

fn golden_path(name: &str) -> PathBuf {
    Path::new(FIXTURES)
        .join("golden")
        .join(format!("{name}.json"))
}

fn read_golden(name: &str) -> Value {
    let path = golden_path(name);
    let text = fs::read_to_string(&path).unwrap_or_else(|err| {
        panic!(
            "golden {} is missing ({err}); regenerate with {REGENERATE}=1",
            path.display()
        )
    });
    serde_json::from_str(&text).expect("golden is JSON")
}

fn describe(output: &Output) -> String {
    let text = String::from_utf8_lossy(&output.stdout);
    let shown: String = text.chars().take(600).collect();
    format!(
        "exit {}, created {:?}, ledger {:?}, detail {:?}\nstdout {:?}",
        output.exit,
        output.created,
        output
            .ledger
            .as_ref()
            .map(|bytes| String::from_utf8_lossy(bytes).into_owned()),
        output.detail,
        shown
    )
}

/// Compares one case. Returns the failure text when the outcome disagrees with its gap status.
/// Compares the Rust output for `case` with its committed golden, and with the live oracle when it
/// runs. Regeneration rewrites the golden from the live oracle instead of reading it.
fn check_case(case: &Case, live: bool, regenerate: bool) -> Vec<String> {
    let rust = run_pair(case, Program::Rust);
    if regenerate {
        let oracle = run_pair(case, Program::Python);
        write_golden(case.name, &oracle);
        return compare_pair(case, &rust, &oracle, "live oracle")
            .into_iter()
            .collect();
    }
    let golden = read_golden(case.name);
    let golden_pair = (
        output_from_json(&golden["inventory"]),
        output_from_json(&golden["spend"]),
    );
    let mut failures: Vec<String> = compare_pair(case, &rust, &golden_pair, "golden")
        .into_iter()
        .collect();
    if live {
        let oracle = run_pair(case, Program::Python);
        failures.extend(compare_pair(case, &rust, &oracle, "live oracle"));
    }
    failures
}

fn compare_pair(
    case: &Case,
    rust: &(Output, Output),
    oracle: &(Output, Output),
    source: &str,
) -> Option<String> {
    compare(
        case.name,
        known_gap(case.name),
        (&rust.0, &oracle.0),
        (&rust.1, &oracle.1),
    )
    .map(|failure| format!("[{source}] {failure}"))
}

fn write_golden(name: &str, oracle: &(Output, Output)) {
    let value = json!({
        "inventory": output_to_json(&oracle.0),
        "spend": output_to_json(&oracle.1),
    });
    let text = serde_json::to_string_pretty(&value).expect("golden JSON") + "\n";
    fs::create_dir_all(golden_path(name).parent().expect("golden dir")).expect("golden dir");
    fs::write(golden_path(name), text).expect("write golden");
}

fn compare(
    name: &str,
    known_gap: Option<&str>,
    inventory: (&Output, &Output),
    spend: (&Output, &Output),
) -> Option<String> {
    let inventory_matches = inventory.0 == inventory.1;
    let spend_matches = spend.0 == spend.1;
    let matches = inventory_matches && spend_matches;
    match (known_gap, matches) {
        (None, true) => None,
        (None, false) => Some(format!(
            "{name}: differs from the oracle\n  inventory rust:   {}\n  inventory oracle: {}\n  spend rust:   {}\n  spend oracle: {}",
            describe(inventory.0),
            describe(inventory.1),
            describe(spend.0),
            describe(spend.1),
        )),
        (Some(gap), true) => Some(format!(
            "{name}: known gap ({gap}) now matches the oracle; remove it from the table and the PR body"
        )),
        (Some(_), false) => None,
    }
}

/// The helper oracle: Python's str, float, int and time helpers on a fixed input set.
fn helper_input() -> Value {
    let epochs: Vec<i64> = vec![
        0,
        -1,
        1,
        1_500_000_000,
        253_402_300_799,
        253_402_300_800,
        9_999_999_999,
        1_099_511_627_776,
        6_000_000_000_000_000,
        67_767_976_233_316_800,
        67_767_976_233_316_799,
        -62_135_596_800,
        -62_135_596_801,
        -62_167_219_200,
        -62_167_219_201,
        1_788_000_000,
        4_294_967_296,
        i64::MAX,
        -i64::MAX,
    ];
    json!({
        "floats": ["0.0", "-0.0", "1.0", "1.5", "0.1", "-2.5", "1e15", "1e16", "9999999999999998.0",
                   "1e-4", "1e-5", "123456789.125", "5e-324", "1.7976931348623157e308",
                   "inf", "-inf", "1e300", "-1e300", "1e18", "6.8e16", "-1.5", "-0.5", "1234.5"],
        "strings": ["", " ", "a b", "\u{1c}\u{1d}\u{1e}\u{1f}", "\u{b}\u{c}", "\u{85}", "\u{a0}",
                    "\u{2028}\u{2029}", "\r\n", "a\rb\nc\r\nd", "x\u{200b}x", "\u{180e}",
                    "\u{3000}漢字\u{3000}", "😀\n", "\t\u{1}\u{7f}", "\"\\/\u{8}\u{c}\n\r\t",
                    "é🚀\u{10ffff}", "\u{202f}\u{205f}", "\u{1680}\u{2000}\u{200a}", "ß",
                    "\u{1f}\u{7f}\u{80}\u{9f}", "a\u{b}b\u{c}c\u{1c}d\u{85}e\u{2028}f"],
        "epochs": epochs,
        "ints": ["0", "-0", "+5", " 7 ", "1_000", "1__0", "_1", "1_", "", " ", "-", "+",
                 "9223372036854775807", "9223372036854775808", "-9223372036854775808",
                 "-9223372036854775809", "00012", "1e3", "12a", "\u{a0}7\u{a0}", "1_2_3"],
    })
}

/// The helper results Rust computes for `input`, shaped like the Python oracle's output.
fn rust_helper_output(input: &Value) -> Value {
    let floats: Vec<f64> = input["floats"]
        .as_array()
        .expect("floats")
        .iter()
        .map(|text| text.as_str().expect("float text").parse().expect("f64"))
        .collect();
    let strings: Vec<&str> = input["strings"]
        .as_array()
        .expect("strings")
        .iter()
        .map(|text| text.as_str().expect("string"))
        .collect();
    let epochs: Vec<i64> = input["epochs"]
        .as_array()
        .expect("epochs")
        .iter()
        .map(|epoch| epoch.as_i64().expect("epoch"))
        .collect();
    let ints: Vec<&str> = input["ints"]
        .as_array()
        .expect("ints")
        .iter()
        .map(|text| text.as_str().expect("int text"))
        .collect();
    let utc = |result: Result<String, py::GmtimeError>| match result {
        Ok(text) => json!(text),
        Err(py::GmtimeError::TimestampOutOfRange) => json!({"error": "OverflowError"}),
        Err(py::GmtimeError::YearOutOfRange) => json!({"error": "OSError"}),
    };
    let isspace: Vec<u32> = (0..0x11_0000u32)
        .filter(|code| char::from_u32(*code).is_some_and(py::is_space))
        .collect();
    json!({
        "json_float": floats.iter().map(|value| py::json_float(*value)).collect::<Vec<_>>(),
        "json_string_ascii": strings.iter().map(|text| py::json_string(text, true)).collect::<Vec<_>>(),
        "json_string_utf8": strings.iter().map(|text| py::json_string(text, false)).collect::<Vec<_>>(),
        "splitlines": strings.iter().map(|text| py::splitlines(text)).collect::<Vec<_>>(),
        "strip": strings.iter().map(|text| py::strip(text)).collect::<Vec<_>>(),
        "utc": epochs.iter().map(|epoch| utc(py::utc_text(*epoch))).collect::<Vec<_>>(),
        "utc_float": floats.iter().map(|value| utc(py::utc_text_from_float(*value))).collect::<Vec<_>>(),
        "parse_int": ints.iter().map(|text| json!(py::parse_int(text))).collect::<Vec<_>>(),
        "isspace": isspace,
    })
}

/// The helper oracle's output for `input`: live Python, or the golden when Python is absent.
fn python_helper_output(input: &Value) -> Value {
    let mut child = Command::new("python3")
        .arg(Path::new(FIXTURES).join("oracle").join("helpers.py"))
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .spawn()
        .expect("run helpers.py");
    use std::io::Write;
    child
        .stdin
        .take()
        .expect("helpers stdin")
        .write_all(input.to_string().as_bytes())
        .expect("write helper input");
    let output = child.wait_with_output().expect("helpers output");
    assert!(output.status.success(), "helpers.py failed");
    serde_json::from_slice(&output.stdout).expect("helpers JSON")
}

/// Compares the helper results with the committed golden, and with the live helper oracle when it
/// runs. Regeneration rewrites the golden from the live oracle first.
fn check_helpers(live: bool, regenerate: bool, failures: &mut Vec<String>) {
    let input = helper_input();
    if regenerate {
        let value = json!({"input": input, "output": python_helper_output(&input)});
        fs::write(
            golden_path("helpers"),
            serde_json::to_string_pretty(&value).expect("helpers golden") + "\n",
        )
        .expect("write helpers golden");
    }
    let golden = read_golden("helpers");
    compare_helpers(
        "golden",
        &rust_helper_output(&golden["input"]),
        &golden["output"],
        failures,
    );
    if live {
        compare_helpers(
            "live oracle",
            &rust_helper_output(&input),
            &python_helper_output(&input),
            failures,
        );
    }
}

fn compare_helpers(source: &str, actual: &Value, expected: &Value, failures: &mut Vec<String>) {
    let expected_map = expected.as_object().expect("helper output object");
    let actual_map = actual.as_object().expect("helper output object");
    for (key, value) in expected_map {
        if actual_map.get(key) != Some(value) {
            failures.push(format!(
                "[{source}] helpers {key}: rust {:?} != oracle {:?}",
                actual_map.get(key),
                value
            ));
        }
    }
}

/// Whether python3's SQLite is the version the goldens were recorded with. SQLite 3.51 cannot open
/// a WAL database at rest read-only, so another interpreter is not the oracle for the goldens.
fn live_oracle_matches_goldens() -> bool {
    let Ok(output) = Command::new("python3")
        .arg("-c")
        .arg("import sqlite3; print(sqlite3.sqlite_version)")
        .output()
    else {
        return false;
    };
    let manifest = fs::read_to_string(Path::new(FIXTURES).join("golden").join("MANIFEST.json"))
        .expect("golden manifest");
    let manifest: Value = serde_json::from_str(&manifest).expect("manifest JSON");
    String::from_utf8_lossy(&output.stdout).trim()
        == manifest["python_sqlite"].as_str().unwrap_or_default()
}

fn regenerate_fixtures() {
    let status = Command::new("python3")
        .arg(Path::new(FIXTURES).join("seed.py"))
        .arg(FIXTURES)
        .status()
        .expect("run seed.py");
    assert!(status.success(), "seed.py failed");
}

fn write_manifest() {
    let version = Command::new("python3")
        .arg("-c")
        .arg("import sqlite3, sys, platform; print(sys.version.replace(chr(10), ' ')); print(sqlite3.sqlite_version); print(platform.platform())")
        .output()
        .expect("python version");
    let text = String::from_utf8_lossy(&version.stdout);
    let mut lines = text.lines();
    let manifest = json!({
        "python": lines.next().unwrap_or_default(),
        "python_sqlite": lines.next().unwrap_or_default(),
        "python_platform": lines.next().unwrap_or_default(),
        "generated_by": format!("{REGENERATE}=1 cargo test -p mcao-cli nm_state_readers_match_the_python_oracle"),
        "now": NOW,
        "cases": CASES.iter().map(|case| case.name).collect::<Vec<_>>(),
    });
    fs::write(
        Path::new(FIXTURES).join("golden").join("MANIFEST.json"),
        serde_json::to_string_pretty(&manifest).expect("manifest") + "\n",
    )
    .expect("write manifest");
}

#[test]
fn nm_state_readers_match_the_python_oracle() {
    let regenerate = env::var_os(REGENERATE).is_some();
    let python = python_available();
    assert!(
        python || !regenerate,
        "{REGENERATE} needs python3 on the path"
    );
    let live = python && (regenerate || live_oracle_matches_goldens());
    if regenerate {
        regenerate_fixtures();
        fs::create_dir_all(Path::new(FIXTURES).join("golden")).expect("golden dir");
    }
    let mut failures = Vec::new();
    check_helpers(live, regenerate, &mut failures);
    for case in CASES {
        failures.extend(check_case(case, live, regenerate));
    }
    if regenerate {
        write_manifest();
    }
    assert!(
        failures.is_empty(),
        "{} failure(s):\n{}",
        failures.len(),
        failures.join("\n")
    );
}

/// A ledger line nested 200000 levels stops the spend with its nesting error and leaves the ledger
/// as it was. The decoder gives up at its depth limit, so the recursion never reaches that depth.
/// The reason is set, so the database is not opened.
#[test]
fn ledger_line_nested_200000_levels_is_refused_without_overflow() {
    let dir = env::temp_dir().join(format!("mcao-nm-state-{}-deep-200000", std::process::id()));
    fs::create_dir_all(&dir).expect("create work directory");
    let ledger = dir.join(LEDGER_NAME);
    let line = Nesting::Array(200_000).line();
    fs::write(&ledger, &line).expect("write ledger");
    let request = SpendRequest {
        task: "task-1",
        spawn_gen: "gen-1",
        repo: REPO_PATH,
        branch: "feat/x",
        since: "",
        database: "/nonexistent/state.sqlite",
        reason: "no-mistakes is not installed",
    };
    let outcome = record_spend(&request, &ledger, NOW);
    let after = fs::read(&ledger);
    let _ = fs::remove_dir_all(&dir);
    assert!(
        matches!(
            outcome,
            Err(SpendRecordError::Ledger(LedgerError::NestingTooDeep))
        ),
        "expected the nesting error, got {outcome:?}"
    );
    assert_eq!(after.expect("ledger after the run"), line);
}
