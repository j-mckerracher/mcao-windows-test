//! CLI-level differential tests of `mcao util nm-run-inventory` and `mcao util nm-spend-record`.
//!
//! The oracle is the Python program each command replaces, copied verbatim into
//! `tests/fixtures/nm-state/oracle` (`inventory.py` and `spend.py` are the python blocks of
//! `bin/fm-nm-run-lib.sh` and `bin/fm-pipeline-spend.sh` at origin/DPES-751). The case table is the
//! reader library's table, and the committed goldens under `golden/` are the slice 2 goldens, so
//! every golden case runs through the built `mcao` binary. `golden-cli/` holds the argument and
//! stdin shapes the library table does not cover; `MCAO_NM_CLI_REGENERATE=1` rewrites those goldens
//! from the live oracle. The live oracle runs too when `python3`'s SQLite is the golden manifest's
//! `python_sqlite`.
//!
//! The CLI reads the wall clock for `recorded_at`, while the oracle is pinned, so `recorded_at` is
//! normalized on both sides. `wall_clock_recorded_at_is_the_current_time` checks the real clock.
//!
//! A case in `KNOWN_GAPS` must NOT match the oracle, as in the library tests.

#![cfg(unix)]

use std::collections::{BTreeMap, BTreeSet};
use std::env;
use std::ffi::{OsStr, OsString};
use std::fs;
use std::io::Write;
use std::os::unix::ffi::{OsStrExt, OsStringExt};
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::{SystemTime, UNIX_EPOCH};

use serde_json::{Value, json};

const FIXTURES: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/tests/fixtures/nm-state");
const REGENERATE: &str = "MCAO_NM_CLI_REGENERATE";
const REPO_PATH: &str = "/work/repo";
const LEDGER_NAME: &str = "pipeline-spend.jsonl";
const INVENTORY_USAGE: &str =
    "usage: mcao util nm-run-inventory <database> <branch> <available-ids-csv>\n";
const SPEND_USAGE: &str = "usage: mcao util nm-spend-record <ledger> <task> <spawn-gen> <repo> <branch> <since> <database> <reason>\n";
const UTIL_USAGE: &str = "usage: mcao util <now-ms|awaiting-hold-ids|awaiting-hold-object|nm-run-inventory|nm-spend-record>\n";

/// The oracle wrapper: pins `time.time` to 1788000000 and pops the program path, so the program sees the same
/// `sys.argv[1:]` the shell gives it.
const WRAPPER: &str = "import sys, time\n\
    program = sys.argv.pop(1)\n\
    time.time = lambda: 1788000000.0\n\
    with open(program, encoding='utf-8') as handle:\n    source = handle.read()\n\
    exec(compile(source, program, 'exec'), {'__name__': '__main__', '__file__': program})\n";

/// One differential case. Field names match the library test table; `subdir`, `padding` and
/// `extra` are the CLI's additions for argument and stdin shapes.
struct Case {
    name: &'static str,
    /// Directory under `db/` copied into the work directory; `None` leaves no database file.
    fixture: Option<&'static str>,
    /// A subdirectory of the work directory that holds the database and the ledger.
    subdir: Option<&'static str>,
    /// Replaces the database argument.
    database: Option<&'static str>,
    /// The `no-mistakes axi` overview: the stdin of the inventory command.
    overview: &'static str,
    /// Bytes of non-repository lines appended to the overview.
    padding: usize,
    /// Raw bytes appended after the overview and the padding.
    extra: &'static [u8],
    branch: &'static str,
    available_ids: &'static str,
    task: &'static str,
    spawn_gen: &'static str,
    repo: &'static str,
    since: &'static str,
    reason: &'static str,
    /// Bytes the ledger holds before the spend run; empty means no ledger file.
    ledger: &'static [u8],
    /// Builds the ledger line instead of `ledger`.
    nesting: Option<Nesting>,
    /// Makes the work directory read-only after the copy, so no sidecar or ledger can be created.
    read_only_dir: bool,
}

/// The overview bytes both programs receive: the overview, the padding, then the extra bytes.
fn overview_bytes(case: &Case) -> Vec<u8> {
    let mut bytes = case.overview.as_bytes().to_vec();
    bytes.extend("pad line\n".repeat(case.padding / 9).as_bytes());
    bytes.extend_from_slice(case.extra);
    bytes
}

/// Known gaps: each case must NOT match the oracle. The reasons are the library's, see
/// `crates/mcao-cli/src/util/nm_state/tests.rs`.
const GAP_CREATED_AT_OVERFLOW: &str = "created_at beyond the years gmtime represents: CPython reports the errno text of an OSError, which is platform specific; this port exits 1 instead of recording unavailable";
const GAP_SINCE_UNICODE_DIGIT: &str = "since is decimal text only in the shell; CPython's int() also accepts non-ASCII decimal digits, which this port rejects";
const GAP_MISSING_STARTED_AT: &str = "the bundled SQLite 3.54.1 appends ' in <statement>' to a prepare error text; CPython's SQLite 3.53.4 and 3.51.0 report 'no such column: started_at' alone";

/// Non-UTF-8 argv: `main` refuses every util argument that is not UTF-8 with exit 2 before the
/// command runs, while Python reads argv with surrogateescape and runs the program. The test
/// `non_utf8_argument_is_refused_with_exit_two` pins the CLI side.
const GAP_NON_UTF8_ARGV: &str = "non-UTF-8 argv: the util entry point refuses it with exit 2 (the convention of every util command); Python decodes the bytes with surrogateescape and runs";

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
    subdir: None,
    database: None,
    overview: OVERVIEW,
    padding: 0,
    extra: b"",
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

/// Cases whose CLI output must differ from the oracle, with the reason.
const KNOWN_GAPS_CLI: &[(&str, &str)] = &[
    ("created-at-overflow", GAP_CREATED_AT_OVERFLOW),
    ("since-unicode-digit", GAP_SINCE_UNICODE_DIGIT),
    ("missing-started-at", GAP_MISSING_STARTED_AT),
    ("ledger-deep-nesting-array-1001", LEDGER_DEEP_NESTING),
    ("ledger-deep-nesting-array-2000", LEDGER_DEEP_NESTING),
    ("ledger-deep-nesting-object-1001", LEDGER_DEEP_NESTING),
    ("ledger-deep-nesting-object-2000", LEDGER_DEEP_NESTING),
];

fn known_gap(name: &str) -> Option<&'static str> {
    KNOWN_GAPS_CLI
        .iter()
        .find(|(case, _)| *case == name)
        .map(|(_, reason)| *reason)
}

/// The bytes and the decisions one program run produced.
#[derive(Debug, Clone, PartialEq)]
struct Output {
    stdout: Vec<u8>,
    exit: i32,
    /// Files created in the work directory by the run, relative to it.
    created: BTreeSet<String>,
    /// The ledger after a spend run, with `recorded_at` normalized, when the ledger exists.
    ledger: Option<Vec<u8>>,
}

/// A work directory holding a copy of one fixture, removed on drop.
struct Work {
    dir: PathBuf,
    db_dir: PathBuf,
    read_only: bool,
}

static COUNTER: AtomicUsize = AtomicUsize::new(0);

impl Work {
    fn new(case: &Case) -> Work {
        let index = COUNTER.fetch_add(1, Ordering::SeqCst);
        let dir = env::temp_dir().join(format!(
            "mcao-nm-cli-{}-{index}-{}",
            std::process::id(),
            case.name
        ));
        fs::create_dir_all(&dir).expect("create work directory");
        let db_dir = match case.subdir {
            Some(subdir) => dir.join(subdir),
            None => dir.clone(),
        };
        fs::create_dir_all(&db_dir).expect("create database directory");
        if let Some(fixture) = case.fixture {
            let source = Path::new(FIXTURES).join("db").join(fixture);
            for entry in fs::read_dir(&source).expect("fixture directory") {
                let entry = entry.expect("fixture entry");
                fs::copy(entry.path(), db_dir.join(entry.file_name())).expect("copy fixture");
            }
        }
        let ledger = match case.nesting {
            Some(nesting) => nesting.line(),
            None => case.ledger.to_vec(),
        };
        if !ledger.is_empty() {
            fs::write(db_dir.join(LEDGER_NAME), ledger).expect("write ledger");
        }
        if case.read_only_dir {
            fs::set_permissions(&dir, fs::Permissions::from_mode(0o555)).expect("chmod work");
        }
        Work {
            dir,
            db_dir,
            read_only: case.read_only_dir,
        }
    }

    fn database(&self) -> PathBuf {
        self.db_dir.join("state.sqlite")
    }

    fn ledger(&self) -> PathBuf {
        self.db_dir.join(LEDGER_NAME)
    }

    /// Every file under the work directory and the database directory, keyed by relative path.
    fn snapshot(&self) -> BTreeMap<String, (Vec<u8>, SystemTime)> {
        let mut files = BTreeMap::new();
        collect(&self.dir, &self.dir, &mut files);
        if self.db_dir != self.dir {
            collect(&self.db_dir, &self.dir, &mut files);
        }
        files
    }
}

fn collect(dir: &Path, root: &Path, files: &mut BTreeMap<String, (Vec<u8>, SystemTime)>) {
    for entry in fs::read_dir(dir).expect("read work directory") {
        let entry = entry.expect("work entry");
        let meta = entry.metadata().expect("work metadata");
        if meta.is_file() {
            let key = entry
                .path()
                .strip_prefix(root)
                .expect("under the work directory")
                .to_string_lossy()
                .into_owned();
            let bytes = fs::read(entry.path()).expect("read work file");
            files.insert(key, (bytes, meta.modified().expect("mtime")));
        }
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

/// Asserts that the run left every database file it found as it was: bytes and mtime.
fn assert_database_untouched(
    before: &BTreeMap<String, (Vec<u8>, SystemTime)>,
    after: &BTreeMap<String, (Vec<u8>, SystemTime)>,
    label: &str,
) {
    for (name, (bytes, mtime)) in before {
        if name
            .rsplit('/')
            .next()
            .is_some_and(|leaf| leaf.starts_with("state.sqlite"))
        {
            let (after_bytes, after_mtime) = after
                .get(name)
                .unwrap_or_else(|| panic!("{label}: {name} disappeared"));
            assert_eq!(bytes, after_bytes, "{label}: {name} bytes changed");
            assert_eq!(mtime, after_mtime, "{label}: {name} mtime changed");
        }
    }
}

/// The database argument: the case's override, or the work directory's database.
fn database_arg(case: &Case, work: &Work) -> OsString {
    match case.database {
        Some(path) => OsString::from(path),
        None => work.database().into_os_string(),
    }
}

/// Runs a command with `stdin` written from a helper thread, and returns its stdout and exit code.
fn run_with_stdin(mut command: Command, stdin: Vec<u8>) -> (Vec<u8>, i32) {
    command
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null());
    let mut child = command.spawn().expect("spawn command");
    let mut pipe = child.stdin.take().expect("command stdin");
    let writer = std::thread::spawn(move || {
        // A usage error exits before it reads stdin, so a broken pipe here is not a failure.
        let _ = pipe.write_all(&stdin);
    });
    let output = child.wait_with_output().expect("command output");
    writer.join().expect("stdin writer");
    (output.stdout, output.status.code().unwrap_or(-1))
}

fn mcao_command(args: &[&OsStr]) -> Command {
    let mut command = Command::new(env!("CARGO_BIN_EXE_mcao"));
    command.arg("util").args(args);
    command
}

fn oracle_command(program: &str, args: &[&OsStr]) -> Command {
    let mut command = Command::new("python3");
    command
        .arg("-c")
        .arg(WRAPPER)
        .arg(Path::new(FIXTURES).join("oracle").join(program))
        .args(args);
    command
}

/// Replaces the wall-clock `recorded_at` of every ledger record with a fixed placeholder, so the
/// pinned oracle and the real clock compare equal. A value of another shape is left as it is.
fn normalize_recorded_at(bytes: &[u8]) -> Vec<u8> {
    const KEY: &[u8] = b"\"recorded_at\":\"";
    const STAMP_LEN: usize = "2026-08-29T12:34:56Z".len();
    let mut out = Vec::with_capacity(bytes.len());
    let mut rest = bytes;
    while let Some(at) = rest.windows(KEY.len()).position(|window| window == KEY) {
        let (head, tail) = rest.split_at(at + KEY.len());
        out.extend_from_slice(head);
        let stamp = tail.get(..STAMP_LEN).unwrap_or_default();
        if is_stamp(stamp) && tail.get(STAMP_LEN) == Some(&b'"') {
            out.extend_from_slice(b"<recorded_at>");
            rest = &tail[STAMP_LEN..];
        } else {
            rest = tail;
        }
    }
    out.extend_from_slice(rest);
    out
}

/// `YYYY-MM-DDTHH:MM:SSZ`.
fn is_stamp(bytes: &[u8]) -> bool {
    bytes.len() == 20
        && bytes.iter().enumerate().all(|(index, byte)| match index {
            4 | 7 => *byte == b'-',
            10 => *byte == b'T',
            13 | 16 => *byte == b':',
            19 => *byte == b'Z',
            _ => byte.is_ascii_digit(),
        })
}

fn rust_inventory(case: &Case, work: &Work) -> Output {
    let before = work.snapshot();
    let database = database_arg(case, work);
    let mut command = mcao_command(&[
        OsStr::new("nm-run-inventory"),
        &database,
        OsStr::new(case.branch),
        OsStr::new(case.available_ids),
    ]);
    command.env_remove("CLICOLOR_FORCE");
    let (stdout, exit) = run_with_stdin(command, overview_bytes(case));
    let created = names_after(&before, work);
    assert_database_untouched(&before, &work.snapshot(), "rust inventory");
    Output {
        stdout,
        exit,
        created,
        ledger: None,
    }
}

fn rust_spend(case: &Case, work: &Work) -> Output {
    let before = work.snapshot();
    let ledger = work.ledger().into_os_string();
    let database = database_arg(case, work);
    let command = mcao_command(&[
        OsStr::new("nm-spend-record"),
        &ledger,
        OsStr::new(case.task),
        OsStr::new(case.spawn_gen),
        OsStr::new(case.repo),
        OsStr::new(case.branch),
        OsStr::new(case.since),
        &database,
        OsStr::new(case.reason),
    ]);
    let (stdout, exit) = run_with_stdin(command, Vec::new());
    let created = names_after(&before, work);
    assert_database_untouched(&before, &work.snapshot(), "rust spend");
    Output {
        stdout,
        exit,
        created,
        ledger: fs::read(work.ledger())
            .ok()
            .map(|bytes| normalize_recorded_at(&bytes)),
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

/// Runs an oracle program with the given `sys.argv[1:]`, and returns its stdout and exit code.
fn python_program(program: &str, args: &[&OsStr]) -> (Vec<u8>, i32) {
    let output = oracle_command(program, args)
        .stdin(Stdio::null())
        .output()
        .expect("run python3");
    (output.stdout, output.status.code().unwrap_or(-1))
}

fn python_inventory(case: &Case, work: &Work) -> Output {
    let before = work.snapshot();
    let database = database_arg(case, work);
    let overview = overview_bytes(case);
    let (stdout, exit) = python_program(
        "inventory.py",
        &[
            OsStr::new(case.branch),
            OsStr::from_bytes(&overview),
            OsStr::new(case.available_ids),
            &database,
        ],
    );
    let created = names_after(&before, work);
    assert_database_untouched(&before, &work.snapshot(), "python inventory");
    Output {
        stdout,
        exit,
        created,
        ledger: None,
    }
}

fn python_spend(case: &Case, work: &Work) -> Output {
    let before = work.snapshot();
    let ledger = work.ledger().into_os_string();
    let database = database_arg(case, work);
    let (stdout, exit) = python_program(
        "spend.py",
        &[
            &ledger,
            OsStr::new(case.task),
            OsStr::new(case.spawn_gen),
            OsStr::new(case.repo),
            OsStr::new(case.branch),
            OsStr::new(case.since),
            &database,
            OsStr::new(case.reason),
        ],
    );
    let created = names_after(&before, work);
    assert_database_untouched(&before, &work.snapshot(), "python spend");
    Output {
        stdout,
        exit,
        created,
        ledger: fs::read(work.ledger())
            .ok()
            .map(|bytes| normalize_recorded_at(&bytes)),
    }
}

/// Runs the inventory and the spend command of `case` in fresh work directories.
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

fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|byte| format!("{byte:02x}")).collect()
}

fn unhex(text: &str) -> Vec<u8> {
    (0..text.len())
        .step_by(2)
        .map(|at| u8::from_str_radix(&text[at..at + 2], 16).expect("golden hex"))
        .collect()
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
        ledger: value["ledger_hex"]
            .as_str()
            .map(|text| normalize_recorded_at(&unhex(text))),
    }
}

fn golden_path(directory: &str, name: &str) -> PathBuf {
    Path::new(FIXTURES)
        .join(directory)
        .join(format!("{name}.json"))
}

fn read_golden(directory: &str, name: &str) -> Value {
    let path = golden_path(directory, name);
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
        "exit {}, created {:?}, ledger {:?}\nstdout {:?}",
        output.exit,
        output.created,
        output
            .ledger
            .as_ref()
            .map(|bytes| String::from_utf8_lossy(bytes).into_owned()),
        shown
    )
}

/// The failure text for one case, when its outcome disagrees with its gap status. A gap is a case
/// whose inventory or spend output differs from the oracle; it must keep differing.
fn compare_pair(
    name: &str,
    source: &str,
    rust: &(Output, Output),
    expected: &(Output, Output),
) -> Option<String> {
    let matches = rust.0 == expected.0 && rust.1 == expected.1;
    match (known_gap(name), matches) {
        (None, true) | (Some(_), false) => None,
        (None, false) => Some(format!(
            "[{source}] {name}: differs from the oracle\n  inventory rust:   {}\n  inventory oracle: {}\n  spend rust:   {}\n  spend oracle: {}",
            describe(&rust.0),
            describe(&expected.0),
            describe(&rust.1),
            describe(&expected.1),
        )),
        (Some(reason), true) => Some(format!(
            "[{source}] {name}: known gap ({reason}) now matches the oracle; remove it from the table and the PR body"
        )),
    }
}

/// Checks one case against its committed golden, and against the live oracle when it runs.
/// Regeneration rewrites the golden from the live oracle instead.
fn check_case(directory: &str, case: &Case, live: bool, regenerate: bool) -> Vec<String> {
    let rust = run_pair(case, Program::Rust);
    if regenerate {
        let oracle = run_pair(case, Program::Python);
        let value = json!({
            "inventory": output_to_json(&oracle.0),
            "spend": output_to_json(&oracle.1),
        });
        fs::write(
            golden_path(directory, case.name),
            serde_json::to_string_pretty(&value).expect("golden JSON") + "\n",
        )
        .expect("write golden");
        return compare_pair(case.name, "live oracle", &rust, &oracle)
            .into_iter()
            .collect();
    }
    let golden = read_golden(directory, case.name);
    let golden_pair = (
        output_from_json(&golden["inventory"]),
        output_from_json(&golden["spend"]),
    );
    let mut failures: Vec<String> = compare_pair(case.name, "golden", &rust, &golden_pair)
        .into_iter()
        .collect();
    if live {
        let oracle = run_pair(case, Program::Python);
        failures.extend(compare_pair(case.name, "live oracle", &rust, &oracle));
    }
    failures
}

/// Whether python3's SQLite is the version the goldens were recorded with.
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

/// The library table's case names, as the committed manifest lists them.
fn manifest_cases() -> Vec<String> {
    let manifest = fs::read_to_string(Path::new(FIXTURES).join("golden").join("MANIFEST.json"))
        .expect("golden manifest");
    let manifest: Value = serde_json::from_str(&manifest).expect("manifest JSON");
    manifest["cases"]
        .as_array()
        .expect("manifest cases")
        .iter()
        .map(|name| name.as_str().expect("case name").to_owned())
        .collect()
}

#[test]
fn cli_matches_every_library_golden_case() {
    // The library goldens are the slice 2 files; only the argument goldens are regenerated.
    let live = live_oracle_matches_goldens() && python_available();
    let names: Vec<String> = CASES.iter().map(|case| case.name.to_owned()).collect();
    assert_eq!(
        names,
        manifest_cases(),
        "the case table must list every golden case of the reader library, in order"
    );
    let mut failures = Vec::new();
    for case in CASES {
        failures.extend(check_case("golden", case, live, false));
    }
    assert!(
        failures.is_empty(),
        "{} failure(s):\n{}",
        failures.len(),
        failures.join("\n")
    );
}

#[test]
fn cli_matches_argument_and_stdin_goldens() {
    let regenerate = env::var_os(REGENERATE).is_some();
    let live = live_oracle_matches_goldens() && python_available();
    assert!(
        live || !regenerate,
        "{REGENERATE} needs python3 with the manifest's SQLite"
    );
    if regenerate {
        fs::create_dir_all(Path::new(FIXTURES).join("golden-cli")).expect("golden-cli dir");
    }
    let mut failures = Vec::new();
    for case in EDGE_CASES {
        failures.extend(check_case("golden-cli", case, live, regenerate));
    }
    assert!(
        failures.is_empty(),
        "{} failure(s):\n{}",
        failures.len(),
        failures.join("\n")
    );
}

/// The `ledger-deep-nesting` gap of the library table, unchanged.
const LEDGER_DEEP_NESTING: &str = "a ledger line nested beyond 1000 levels: this port exits 1 without a record; CPython's json.loads decodes it (its depth limit is interpreter dependent, 116188 on 3.14.6, 995 on 3.9.6)";

/// Argument and stdin shapes the library table does not cover. Goldens in `golden-cli/`.
const EDGE_CASES: &[Case] = &[
    Case {
        name: "argv-empty-strings",
        branch: "",
        task: "",
        spawn_gen: "",
        repo: "",
        since: "",
        reason: "",
        available_ids: "",
        ..DEFAULT
    },
    Case {
        name: "argv-empty-database",
        database: Some(""),
        ..DEFAULT
    },
    Case {
        name: "argv-leading-dashes",
        branch: "--feat/x",
        task: "--task-1",
        spawn_gen: "-gen-1",
        available_ids: "--run-new, -run-mid",
        reason: "--no-mistakes is not installed",
        ..DEFAULT
    },
    Case {
        name: "argv-spaces",
        subdir: Some("dir with spaces"),
        task: "task one",
        reason: "reason with spaces",
        ..DEFAULT
    },
    Case {
        name: "argv-uri-characters",
        subdir: Some("a?b#c%20d"),
        ..DEFAULT
    },
    Case {
        name: "argv-subdir-ledger-with-record",
        subdir: Some("ledger dir"),
        ledger: b"{\"task\":\"other\"}\n",
        ..DEFAULT
    },
    Case {
        name: "argv-unicode-ids-echo",
        fixture: None,
        branch: "feat/ü",
        task: "tâsk-🚀",
        available_ids: "run-é, run-🚀",
        ..DEFAULT
    },
    Case {
        name: "argv-unicode-repo",
        overview: "repo: /work/rëpo\n",
        repo: "/work/rëpo",
        available_ids: "run-new",
        ..DEFAULT
    },
    Case {
        name: "argv-since-leading-zeros",
        fixture: Some("since-boundary"),
        since: "00001700000000",
        available_ids: "run-after, run-at, run-before",
        ..DEFAULT
    },
    Case {
        name: "argv-since-plus-sign",
        fixture: Some("since-boundary"),
        since: "+1700000000",
        available_ids: "run-after, run-at, run-before",
        ..DEFAULT
    },
    Case {
        name: "argv-since-negative",
        fixture: Some("since-boundary"),
        since: "-1",
        available_ids: "run-after, run-at, run-before",
        ..DEFAULT
    },
    Case {
        name: "stdin-empty",
        overview: "",
        available_ids: "run-new",
        ..DEFAULT
    },
    Case {
        name: "stdin-no-trailing-newline",
        overview: "count: 3 of 3 total\nrepo: /work/repo",
        ..DEFAULT
    },
    Case {
        name: "stdin-crlf-overview",
        overview: "count: 3 of 3 total\r\nrepo: /work/repo\r\n",
        ..DEFAULT
    },
    Case {
        name: "stdin-huge",
        padding: 100_000,
        ..DEFAULT
    },
    Case {
        name: "stdin-invalid-utf8-tail",
        extra: b"junk \xff\xfe\n",
        ..DEFAULT
    },
    Case {
        name: "stdin-invalid-utf8-repo-line",
        overview: "",
        extra: b"repo: /work/re\xffpo\n",
        available_ids: "run-new",
        ..DEFAULT
    },
];

/// The `util` usage errors: exit 2, nothing on stdout, and the command's usage line.
#[test]
fn inventory_usage_errors_exit_two() {
    let operands = ["db", "feat/x", "run-new", "extra"];
    for count in [0, 1, 2, 4] {
        let output = Command::new(env!("CARGO_BIN_EXE_mcao"))
            .args(["util", "nm-run-inventory"])
            .args(&operands[..count])
            .stdin(Stdio::null())
            .output()
            .expect("mcao runs");
        assert_eq!(output.status.code(), Some(2), "{count} operands");
        assert!(output.stdout.is_empty(), "{count} operands");
        assert_eq!(
            String::from_utf8_lossy(&output.stderr),
            INVENTORY_USAGE,
            "{count} operands"
        );
    }
}

#[test]
fn spend_usage_errors_exit_two() {
    let operands = ["l", "t", "g", "r", "b", "s", "d", "x", "extra"];
    for count in [0, 1, 2, 3, 4, 5, 6, 7, 9] {
        let output = Command::new(env!("CARGO_BIN_EXE_mcao"))
            .args(["util", "nm-spend-record"])
            .args(&operands[..count])
            .stdin(Stdio::null())
            .output()
            .expect("mcao runs");
        assert_eq!(output.status.code(), Some(2), "{count} operands");
        assert!(output.stdout.is_empty(), "{count} operands");
        assert_eq!(
            String::from_utf8_lossy(&output.stderr),
            SPEND_USAGE,
            "{count} operands"
        );
    }
}

#[test]
fn unix_util_usage_names_the_readers() {
    let output = Command::new(env!("CARGO_BIN_EXE_mcao"))
        .args(["util", "nope"])
        .stdin(Stdio::null())
        .output()
        .expect("mcao runs");
    assert_eq!(output.status.code(), Some(2));
    assert_eq!(String::from_utf8_lossy(&output.stderr), UTIL_USAGE);
}

/// `GAP_NON_UTF8_ARGV`: the CLI side only. Python is not run, because its argv decoding is the gap.
#[test]
fn non_utf8_argument_is_refused_with_exit_two() {
    let bad = OsString::from_vec(vec![0xff]);
    let output = Command::new(env!("CARGO_BIN_EXE_mcao"))
        .args(["util", "nm-run-inventory"])
        .arg(&bad)
        .args(["feat/x", "run-new"])
        .stdin(Stdio::null())
        .output()
        .expect("mcao runs");
    assert_eq!(output.status.code(), Some(2), "{GAP_NON_UTF8_ARGV}");
    assert!(output.stdout.is_empty(), "{GAP_NON_UTF8_ARGV}");
    assert_eq!(
        String::from_utf8_lossy(&output.stderr),
        "Usage: invalid UTF-8 was detected in one or more arguments (see mcao --help)\n",
        "{GAP_NON_UTF8_ARGV}"
    );
}

/// Civil date and time text of a Unix epoch second, `YYYY-MM-DDTHH:MM:SSZ`, by Howard Hinnant's
/// `civil_from_days`: an independent formatter for the clock check.
fn utc_text(epoch: i64) -> String {
    let days = epoch.div_euclid(86_400);
    let seconds = epoch.rem_euclid(86_400);
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z.rem_euclid(146_097);
    let yoe = (doe - doe / 1_460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let day = doy - (153 * mp + 2) / 5 + 1;
    let month = if mp < 10 { mp + 3 } else { mp - 9 };
    let year = yoe + era * 400 + i64::from(month <= 2);
    format!(
        "{year:04}-{month:02}-{day:02}T{:02}:{:02}:{:02}Z",
        seconds / 3600,
        seconds % 3600 / 60,
        seconds % 60
    )
}

fn now_epoch() -> i64 {
    i64::try_from(
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("clock after epoch")
            .as_secs(),
    )
    .expect("epoch fits i64")
}

/// The CLI reads the wall clock: its `recorded_at` is the current second, the one thing the
/// normalized comparisons above do not check.
#[test]
fn wall_clock_recorded_at_is_the_current_time() {
    let work = Work::new(&DEFAULT);
    let before = utc_text(now_epoch());
    let output = rust_spend(&DEFAULT, &work);
    let after = utc_text(now_epoch());
    assert_eq!(output.exit, 0);
    let ledger = fs::read_to_string(work.ledger()).expect("ledger after the run");
    let stamp = ledger
        .split("\"recorded_at\":\"")
        .nth(1)
        .and_then(|rest| rest.get(..20))
        .expect("recorded_at in the record");
    assert!(
        before.as_str() <= stamp && stamp <= after.as_str(),
        "recorded_at {stamp} outside [{before}, {after}]"
    );
}
