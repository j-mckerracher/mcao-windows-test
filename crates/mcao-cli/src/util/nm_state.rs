//! Python-parity readers of no-mistakes' local state database.
//!
//! Two programs in the shell scripts read this database with Python's `sqlite3`:
//! `bin/fm-nm-run-lib.sh` (the capped run inventory) and `bin/fm-pipeline-spend.sh` (the spend
//! record). Their Python sources are the specification: this module reproduces their stdout and
//! their ledger line byte for byte, or the case is listed as a known gap in the tests.
//!
//! Errors are typed. Where Python catches an exception and reports it as data (the `unknown|`
//! inventory line, or the `unavailable` spend record), the typed error carries what is needed to
//! render that text. Where Python lets an exception escape, the typed error means exit 1.

mod py;
mod sqlite;
#[cfg(test)]
mod tests;

use std::cmp::Ordering;
use std::collections::{HashMap, HashSet};
use std::fs::OpenOptions;
use std::io::{Read, Write};
use std::os::unix::fs::OpenOptionsExt;
use std::path::Path;

use py::{GmtimeError, Json, JsonError};
use sqlite::{Cell, DbError, Key};

// ---------------------------------------------------------------------------------------------
// Capped run inventory (bin/fm-nm-run-lib.sh, fm_nm_select_run)
// ---------------------------------------------------------------------------------------------

/// The inputs of the inventory program, as the shell passes them in `sys.argv`.
pub struct InventoryRequest<'a> {
    pub database: &'a Path,
    pub branch: &'a str,
    /// The `no-mistakes axi` overview text.
    pub overview: &'a str,
    /// The ids the overview shows for the branch, joined with ", ".
    pub available_ids: &'a str,
}

/// One same-branch run row, every field already validated.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RunRow {
    pub id: String,
    pub branch: String,
    pub status: String,
    pub head: String,
}

/// Why the inventory could not be read, and the run ids the program had collected by then.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct InventoryFailure {
    pub run_ids: Vec<String>,
    pub error: InventoryError,
}

/// Each way the inventory program's `try` block fails.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum InventoryError {
    /// The overview has no single `repo:` line naming an absolute path.
    RepoPath,
    /// The database path is not absolute, so Python cannot form its URI.
    DatabasePath,
    /// SQLite refused the open or a query, or a selected text cell is not UTF-8.
    Database(DbError),
    /// The repository path matches this many `repos` rows instead of one.
    RepoCount(usize),
    /// The repository path is not representable as UTF-8 text for SQLite.
    RepoPathEncoding,
    /// A row holds a value outside the accepted shapes.
    RowRejected,
    /// An overview id is not among the branch's rows.
    OverviewIdMissing,
}

impl std::fmt::Display for InventoryError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            InventoryError::RepoPath => {
                write!(f, "the overview has no single absolute repo line")
            }
            InventoryError::DatabasePath => write!(f, "the database path is not absolute"),
            InventoryError::Database(error) => write!(f, "{}", error.message),
            InventoryError::RepoCount(count) => {
                write!(f, "{count} repos rows match the repository path")
            }
            InventoryError::RepoPathEncoding => {
                write!(f, "the repository path is not encodable for SQLite")
            }
            InventoryError::RowRejected => write!(f, "a same-branch row failed validation"),
            InventoryError::OverviewIdMissing => {
                write!(f, "an overview run id has no same-branch row")
            }
        }
    }
}

/// Reads the same-branch runs of the repository the overview names, as the inventory program
/// does, and returns the rows in the program's order.
pub fn read_run_inventory(request: &InventoryRequest<'_>) -> Result<Vec<RunRow>, InventoryFailure> {
    let mut run_ids = split_ids(request.available_ids);
    match read_rows(request, &mut run_ids) {
        Ok(rows) => Ok(rows),
        Err(error) => Err(InventoryFailure { run_ids, error }),
    }
}

fn split_ids(available_ids: &str) -> Vec<String> {
    if available_ids.is_empty() {
        Vec::new()
    } else {
        available_ids.split(", ").map(str::to_owned).collect()
    }
}

fn read_rows(
    request: &InventoryRequest<'_>,
    run_ids: &mut Vec<String>,
) -> Result<Vec<RunRow>, InventoryError> {
    let repo = repo_path(request.overview)?;
    if !Path::new(request.database).is_absolute() {
        return Err(InventoryError::DatabasePath);
    }
    let displayed: HashSet<String> = run_ids.iter().cloned().collect();
    let connection = sqlite::open_read_only(request.database).map_err(InventoryError::Database)?;
    sqlite::begin(&connection).map_err(InventoryError::Database)?;
    let repos_sql = "SELECT id FROM repos WHERE working_path = ?";
    let repo_text = match repo {
        RepoPath::Text(text) => text,
        RepoPath::Unencodable => {
            sqlite::prepare(&connection, repos_sql).map_err(InventoryError::Database)?;
            return Err(InventoryError::RepoPathEncoding);
        }
    };
    let repos = sqlite::query(&connection, repos_sql, &[Cell::Text(repo_text)])
        .map_err(InventoryError::Database)?;
    if repos.len() != 1 {
        return Err(InventoryError::RepoCount(repos.len()));
    }
    let rows = sqlite::query(
        &connection,
        "SELECT id, branch, status, head_sha FROM runs WHERE repo_id = ? AND branch = ? \
         ORDER BY created_at DESC, id DESC",
        &[repos[0][0].clone(), Cell::Text(request.branch.to_owned())],
    )
    .map_err(InventoryError::Database)?;

    for row in &rows {
        if let Cell::Text(id) = &row[0]
            && is_run_id(id)
            && !run_ids.contains(id)
        {
            run_ids.push(id.clone());
        }
    }
    let row_ids: HashSet<&str> = rows
        .iter()
        .filter_map(|row| match &row[0] {
            Cell::Text(id) => Some(id.as_str()),
            _ => None,
        })
        .collect();
    if !displayed.iter().all(|id| row_ids.contains(id.as_str())) {
        return Err(InventoryError::OverviewIdMissing);
    }

    let mut runs = Vec::with_capacity(rows.len());
    for row in rows {
        let (Cell::Text(id), Cell::Text(branch), Cell::Text(status), Cell::Text(head)) =
            (&row[0], &row[1], &row[2], &row[3])
        else {
            return Err(InventoryError::RowRejected);
        };
        if !is_run_id(id) || branch != request.branch || !is_status(status) || !is_head(head) {
            return Err(InventoryError::RowRejected);
        }
        runs.push(RunRow {
            id: id.clone(),
            branch: branch.clone(),
            status: status.clone(),
            head: head.clone(),
        });
    }
    Ok(runs)
}

/// The text the inventory program would print for `outcome`, byte for byte.
pub fn render_run_inventory(outcome: &Result<Vec<RunRow>, InventoryFailure>) -> String {
    match outcome {
        Ok(rows) => {
            let mut out = format!(
                "count: {0} of {0} total\nruns[{0}]{{id,branch,status,head,pr}}:\n",
                rows.len()
            );
            for row in rows {
                out.push_str(&format!(
                    "  {},{},{},{},\"\"\n",
                    py::json_string(&row.id, false),
                    py::json_string(&row.branch, false),
                    py::json_string(&row.status, false),
                    py::json_string(&row.head, false),
                ));
            }
            out
        }
        Err(failure) => format!(
            "unknown|complete same-branch run inventory unreadable; run ids: {}\n",
            failure.run_ids.join(", ")
        ),
    }
}

/// The repository path the program reads from the overview's single `repo:` line.
enum RepoPath {
    Text(String),
    /// A JSON string with a lone surrogate: Python can name it but SQLite cannot encode it.
    Unencodable,
}

fn repo_path(overview: &str) -> Result<RepoPath, InventoryError> {
    let lines: Vec<&str> = py::splitlines(overview)
        .into_iter()
        .filter(|line| line.starts_with("repo: "))
        .map(|line| py::strip(&line[6..]))
        .collect();
    let [raw] = lines.as_slice() else {
        return Err(InventoryError::RepoPath);
    };
    if raw.starts_with('"') {
        let Ok(Json::Str(units)) = py::json_loads(raw) else {
            return Err(InventoryError::RepoPath);
        };
        if units.first() != Some(&u16::from(b'/')) {
            return Err(InventoryError::RepoPath);
        }
        return Ok(match String::from_utf16(&units) {
            Ok(text) => RepoPath::Text(text),
            Err(_) => RepoPath::Unencodable,
        });
    }
    if !raw.starts_with('/') {
        return Err(InventoryError::RepoPath);
    }
    Ok(RepoPath::Text((*raw).to_owned()))
}

/// `re.fullmatch(r"[A-Za-z0-9_-]+", value)`.
fn is_run_id(value: &str) -> bool {
    !value.is_empty()
        && value
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'_' || b == b'-')
}

/// `re.fullmatch(r"[a-z_-]+", value)`.
fn is_status(value: &str) -> bool {
    !value.is_empty()
        && value
            .bytes()
            .all(|b| b.is_ascii_lowercase() || b == b'_' || b == b'-')
}

/// `re.fullmatch(r"[a-fA-F0-9]{7,40}", value)`.
fn is_head(value: &str) -> bool {
    (7..=40).contains(&value.len()) && value.bytes().all(|b| b.is_ascii_hexdigit())
}

// ---------------------------------------------------------------------------------------------
// Spend record (bin/fm-pipeline-spend.sh)
// ---------------------------------------------------------------------------------------------

/// The arguments of the spend program's `sys.argv`, in its order.
pub struct SpendRequest<'a> {
    pub task: &'a str,
    /// Empty when the task has no spawn generation, which the record writes as null.
    pub spawn_gen: &'a str,
    pub repo: &'a str,
    pub branch: &'a str,
    /// Empty for no reflog; otherwise decimal epoch seconds.
    pub since: &'a str,
    pub database: &'a str,
    /// Non-empty when the shell already knows the runs cannot be read; the database is then not
    /// opened.
    pub reason: &'a str,
}

/// Each way building the record makes the spend program exit 1 instead of writing it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SpendBuildError {
    /// `int(since)` or the `since` timestamp failed, outside the program's `try`.
    Since,
    /// A timestamp `time.gmtime` cannot convert.
    Time(GmtimeError),
    /// A Python `TypeError` or `ValueError` the program lets escape, with the operation at fault.
    Value(&'static str),
}

/// Why the reader could not produce the runs. Python catches these and records `unavailable`.
#[derive(Debug)]
enum SpendReadError {
    Unavailable(String),
    Build(SpendBuildError),
}

impl From<DbError> for SpendReadError {
    fn from(error: DbError) -> Self {
        SpendReadError::Unavailable(error.message)
    }
}

impl std::fmt::Display for SpendBuildError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            SpendBuildError::Since => write!(f, "since is not a decimal epoch in range"),
            SpendBuildError::Time(error) => write!(f, "timestamp: {error:?}"),
            SpendBuildError::Value(message) => write!(f, "{message}"),
        }
    }
}

/// Builds the one-line JSON record the program appends, without its trailing newline.
pub fn spend_record_line(
    request: &SpendRequest<'_>,
    recorded_at: i64,
) -> Result<String, SpendBuildError> {
    let since = if request.since.is_empty() {
        None
    } else {
        Some(py::parse_int(request.since).ok_or(SpendBuildError::Since)?)
    };
    let since_text = match since {
        Some(epoch) => Some(py::utc_text(epoch).map_err(|_| SpendBuildError::Since)?),
        None => None,
    };
    let recorded_text = py::utc_text(recorded_at).map_err(SpendBuildError::Time)?;
    let reason = if request.reason.is_empty() {
        match read_spend(request, since.unwrap_or(0)) {
            Ok(spend) => {
                return spend_json(request, &recorded_text, since_text.as_deref(), Ok(spend));
            }
            Err(SpendReadError::Unavailable(message)) => {
                format!("cannot read no-mistakes state: {message}")
            }
            Err(SpendReadError::Build(error)) => return Err(error),
        }
    } else {
        request.reason.to_owned()
    };
    spend_json(request, &recorded_text, since_text.as_deref(), Err(reason))
}

/// The record's JSON. `spend` is `Err(reason)` for the `unavailable` record.
fn spend_json(
    request: &SpendRequest<'_>,
    recorded_text: &str,
    since_text: Option<&str>,
    spend: Result<Spend, String>,
) -> Result<String, SpendBuildError> {
    let head = format!(
        "{{\"task\":{},\"spawn_gen\":{},\"recorded_at\":{}",
        py::json_string(request.task, true),
        optional_text(request.spawn_gen),
        py::json_string(recorded_text, true),
    );
    let tail = format!(
        "\"repo\":{},\"branch\":{},\"since\":{}",
        optional_text(request.repo),
        optional_text(request.branch),
        since_text.map_or_else(|| "null".to_owned(), |text| py::json_string(text, true)),
    );
    match spend {
        Err(reason) => Ok(format!(
            "{head},\"source\":\"unavailable\",\"reason\":{},{tail},\"total\":null,\"runs\":[],\"purposes\":[]}}",
            py::json_string(&reason, true),
        )),
        Ok(spend) => Ok(format!(
            "{head},\"source\":\"no-mistakes-state\",\"reason\":null,{tail},\"total\":{},\"runs\":[{}],\"purposes\":[{}]}}",
            spend.total.to_json()?,
            spend
                .runs
                .iter()
                .map(Run::to_json)
                .collect::<Result<Vec<_>, _>>()?
                .join(","),
            spend
                .purposes
                .iter()
                .map(|(purpose, tally)| -> Result<String, SpendBuildError> {
                    Ok(format!(
                        "{{\"purpose\":{},{}}}",
                        json_value(purpose)?,
                        tally.fields_json()?
                    ))
                })
                .collect::<Result<Vec<_>, SpendBuildError>>()?
                .join(","),
        )),
    }
}

/// `value or None` as JSON: null for empty text.
fn optional_text(text: &str) -> String {
    if text.is_empty() {
        "null".to_owned()
    } else {
        py::json_string(text, true)
    }
}

/// What the reader produced, before it is written: the total, the runs oldest first, and the
/// purposes sorted by name.
struct Spend {
    total: Tally,
    runs: Vec<Run>,
    purposes: Vec<(Cell, Tally)>,
}

struct Run {
    id: Cell,
    status: Cell,
    created_at: String,
    tally: Tally,
}

impl Run {
    fn to_json(&self) -> Result<String, SpendBuildError> {
        Ok(format!(
            "{{\"id\":{},\"status\":{},\"created_at\":{},{}}}",
            json_value(&self.id)?,
            json_value(&self.status)?,
            py::json_string(&self.created_at, true),
            self.tally.fields_json()?,
        ))
    }
}

/// The columns the program reads from `agent_invocations`, in its select order. A column the
/// database lacks is selected as NULL.
const INVOCATION_COLUMNS: [&str; 12] = [
    "run_id",
    "purpose",
    "session_mode",
    "exit_status",
    "duration_ms",
    "input_tokens",
    "output_tokens",
    "cache_read_tokens",
    "cache_creation_tokens",
    "delta_input_tokens",
    "delta_output_tokens",
    "delta_cache_read_tokens",
];

/// One `agent_invocations` row, by the position of its column in [`INVOCATION_COLUMNS`].
struct Invocation {
    run_id: Cell,
    purpose: Cell,
    session_mode: Cell,
    exit_status: Cell,
    duration_ms: Cell,
    /// `input_tokens`, `output_tokens`, `cache_read_tokens`, `cache_creation_tokens`.
    raw: [Cell; 4],
    delta_input: Cell,
    delta_output: Cell,
    delta_cache_read: Cell,
}

impl Invocation {
    fn from_cells(cells: &[Cell]) -> Invocation {
        Invocation {
            run_id: cells[0].clone(),
            purpose: cells[1].clone(),
            session_mode: cells[2].clone(),
            exit_status: cells[3].clone(),
            duration_ms: cells[4].clone(),
            raw: [
                cells[5].clone(),
                cells[6].clone(),
                cells[7].clone(),
                cells[8].clone(),
            ],
            delta_input: cells[9].clone(),
            delta_output: cells[10].clone(),
            delta_cache_read: cells[11].clone(),
        }
    }

    /// The per-invocation token values the program sums. `Cell::Null` is an unknown count.
    fn tokens(&self) -> [Cell; 4] {
        let resumed = matches!(&self.session_mode, Cell::Text(mode) if mode == "resumed");
        let mut proven = true;
        let counters = [
            (&self.raw[0], &self.delta_input),
            (&self.raw[1], &self.delta_output),
            (&self.raw[2], &self.delta_cache_read),
        ];
        let mut tokens = [Cell::Null, Cell::Null, Cell::Null, Cell::Null];
        for (slot, (raw, delta)) in counters.into_iter().enumerate() {
            if *delta == Cell::Null {
                proven = false;
                tokens[slot] = if resumed { Cell::Null } else { raw.clone() };
            } else {
                proven = proven && Key::of(raw) == Key::of(delta);
                tokens[slot] = delta.clone();
            }
        }
        let per_invocation = !resumed || proven;
        tokens[3] = if per_invocation {
            self.raw[3].clone()
        } else {
            Cell::Null
        };
        tokens
    }
}

/// A Python number: `int` is exact and unbounded in practice, so `i128` holds every sum.
#[derive(Debug, Clone, Copy, PartialEq)]
enum Num {
    Int(i128),
    Float(f64),
}

impl Num {
    fn plus(self, other: Num) -> Result<Num, SpendBuildError> {
        Ok(match (self, other) {
            (Num::Int(a), Num::Int(b)) => Num::Int(
                a.checked_add(b)
                    .ok_or(SpendBuildError::Value("integer total exceeds i128"))?,
            ),
            (Num::Int(a), Num::Float(b)) => Num::Float(a as f64 + b),
            (Num::Float(a), Num::Int(b)) => Num::Float(a + b as f64),
            (Num::Float(a), Num::Float(b)) => Num::Float(a + b),
        })
    }

    fn to_json(self) -> Result<String, SpendBuildError> {
        Ok(match self {
            Num::Int(value) => value.to_string(),
            Num::Float(value) => py::json_float(value),
        })
    }
}

/// `t[field]["total"]` and `"unknown"` for one token field.
#[derive(Debug, Clone, Copy)]
struct TokenTotal {
    total: Num,
    unknown: i64,
}

/// One tally: the program's `tally()` dictionary.
#[derive(Clone)]
struct Tally {
    invocations: i64,
    /// `exit` in first-seen key order, keyed by Python equality.
    exit: Vec<(Cell, i64)>,
    exit_index: HashMap<Key, usize>,
    duration_ms: Num,
    /// `input_tokens`, `output_tokens`, `cache_read_tokens`, `cache_creation_tokens`.
    tokens: [TokenTotal; 4],
}

const TOKEN_FIELDS: [&str; 4] = [
    "input_tokens",
    "output_tokens",
    "cache_read_tokens",
    "cache_creation_tokens",
];

impl Tally {
    fn new() -> Tally {
        Tally {
            invocations: 0,
            exit: Vec::new(),
            exit_index: HashMap::new(),
            duration_ms: Num::Int(0),
            tokens: [TokenTotal {
                total: Num::Int(0),
                unknown: 0,
            }; 4],
        }
    }

    /// The program's `add(t, row, tokens)`.
    fn add(&mut self, invocation: &Invocation, tokens: &[Cell; 4]) -> Result<(), SpendBuildError> {
        self.invocations += 1;
        let key = Key::of(&invocation.exit_status);
        match self.exit_index.get(&key) {
            Some(&index) => self.exit[index].1 += 1,
            None => {
                self.exit_index.insert(key, self.exit.len());
                self.exit.push((invocation.exit_status.clone(), 1));
            }
        }
        let duration = if invocation.duration_ms.is_truthy() {
            number(&invocation.duration_ms)?
        } else {
            Num::Int(0)
        };
        self.duration_ms = self.duration_ms.plus(duration)?;
        for (slot, cell) in self.tokens.iter_mut().zip(tokens) {
            if *cell == Cell::Null {
                slot.unknown += 1;
            } else {
                slot.total = slot.total.plus(number(cell)?)?;
            }
        }
        Ok(())
    }

    /// The tally's members after the purpose or run fields, as JSON object members.
    fn fields_json(&self) -> Result<String, SpendBuildError> {
        let mut exit = Vec::with_capacity(self.exit.len());
        for (key, count) in &self.exit {
            exit.push(format!("{}:{count}", json_key(key)?));
        }
        let mut members = vec![
            format!("\"invocations\":{}", self.invocations),
            format!("\"exit\":{{{}}}", exit.join(",")),
            format!("\"duration_ms\":{}", self.duration_ms.to_json()?),
        ];
        for (name, slot) in TOKEN_FIELDS.iter().zip(&self.tokens) {
            members.push(format!(
                "\"{name}\":{{\"total\":{},\"unknown\":{}}}",
                slot.total.to_json()?,
                slot.unknown
            ));
        }
        Ok(members.join(","))
    }

    /// The whole tally as one JSON object, for the record's `total`.
    fn to_json(&self) -> Result<String, SpendBuildError> {
        Ok(format!("{{{}}}", self.fields_json()?))
    }
}

/// `value` as `json.dumps` writes a value: bytes have no JSON form.
fn json_value(cell: &Cell) -> Result<String, SpendBuildError> {
    match cell {
        Cell::Null => Ok("null".to_owned()),
        Cell::Int(value) => Ok(value.to_string()),
        Cell::Real(value) => Ok(py::json_float(*value)),
        Cell::Text(text) => Ok(py::json_string(text, true)),
        Cell::Blob(_) => Err(SpendBuildError::Value("bytes are not JSON serializable")),
    }
}

/// `value` as a `json.dumps` object key: `None`, numbers and floats become their JSON text.
fn json_key(cell: &Cell) -> Result<String, SpendBuildError> {
    match cell {
        Cell::Null => Ok("\"null\"".to_owned()),
        Cell::Int(value) => Ok(format!("\"{value}\"")),
        Cell::Real(value) => Ok(format!("\"{}\"", py::json_float(*value))),
        Cell::Text(text) => Ok(py::json_string(text, true)),
        Cell::Blob(_) => Err(SpendBuildError::Value(
            "bytes keys are not JSON serializable",
        )),
    }
}

/// The program's `invocation_tokens` and `add` operands: a numeric value, or a `TypeError`.
fn number(cell: &Cell) -> Result<Num, SpendBuildError> {
    match cell {
        Cell::Int(value) => Ok(Num::Int(i128::from(*value))),
        Cell::Real(value) => Ok(Num::Float(*value)),
        Cell::Null | Cell::Text(_) | Cell::Blob(_) => Err(SpendBuildError::Value(
            "a token or duration value is not a number",
        )),
    }
}

/// `time.gmtime(value)` for a created_at cell: integers and floats only.
fn created_at_text(cell: &Cell) -> Result<String, SpendBuildError> {
    match cell {
        Cell::Int(value) => py::utc_text(*value).map_err(SpendBuildError::Time),
        Cell::Real(value) => py::utc_text_from_float(*value).map_err(SpendBuildError::Time),
        Cell::Null | Cell::Text(_) | Cell::Blob(_) => {
            Err(SpendBuildError::Value("created_at is not a number"))
        }
    }
}

/// Reads the spend the way the program's `read_spend` does, with `since` already an integer.
fn read_spend(request: &SpendRequest<'_>, since: i64) -> Result<Spend, SpendReadError> {
    if !Path::new(request.database).is_absolute() {
        return Err(SpendReadError::Unavailable(format!(
            "state database path {} is not absolute",
            request.database
        )));
    }
    let connection = sqlite::open_read_only(Path::new(request.database))?;
    sqlite::begin(&connection)?;
    let columns: HashSet<String> =
        sqlite::query(&connection, "PRAGMA table_info(agent_invocations)", &[])?
            .into_iter()
            .filter_map(|row| match row.get(1) {
                Some(Cell::Text(name)) => Some(name.clone()),
                _ => None,
            })
            .collect();
    if columns.is_empty() {
        return Err(SpendReadError::Unavailable(
            "no agent_invocations table".to_owned(),
        ));
    }
    let repos = sqlite::query(
        &connection,
        "SELECT id FROM repos WHERE working_path = ?",
        &[Cell::Text(request.repo.to_owned())],
    )?;
    if repos.len() != 1 {
        return Err(SpendReadError::Unavailable(format!(
            "no repository {}",
            request.repo
        )));
    }
    let runs = sqlite::query(
        &connection,
        "SELECT id, status, created_at FROM runs WHERE repo_id = ? AND branch = ? AND created_at >= ? \
         ORDER BY created_at, id",
        &[
            repos[0][0].clone(),
            Cell::Text(request.branch.to_owned()),
            Cell::Int(since),
        ],
    )?;
    let select = INVOCATION_COLUMNS
        .iter()
        .map(|column| {
            if columns.contains(*column) {
                (*column).to_owned()
            } else {
                format!("NULL AS {column}")
            }
        })
        .collect::<Vec<_>>()
        .join(", ");
    let mut invocations = Vec::new();
    for run in &runs {
        for cells in sqlite::query(
            &connection,
            &format!(
                "SELECT {select} FROM agent_invocations WHERE run_id = ? ORDER BY started_at, id"
            ),
            &[run[0].clone()],
        )? {
            invocations.push(Invocation::from_cells(&cells));
        }
    }

    let mut total = Tally::new();
    let mut per_run: HashMap<Key, Tally> = runs
        .iter()
        .map(|run| (Key::of(&run[0]), Tally::new()))
        .collect();
    let mut per_purpose: Vec<(Cell, Tally)> = Vec::new();
    let mut purpose_index: HashMap<Key, usize> = HashMap::new();
    for invocation in &invocations {
        let tokens = invocation.tokens();
        total
            .add(invocation, &tokens)
            .map_err(SpendReadError::Build)?;
        per_run
            .get_mut(&Key::of(&invocation.run_id))
            .ok_or(SpendReadError::Build(SpendBuildError::Value(
                "an invocation's run is not in the run list",
            )))?
            .add(invocation, &tokens)
            .map_err(SpendReadError::Build)?;
        let key = Key::of(&invocation.purpose);
        let index = *purpose_index.entry(key).or_insert_with(|| {
            per_purpose.push((invocation.purpose.clone(), Tally::new()));
            per_purpose.len() - 1
        });
        per_purpose[index]
            .1
            .add(invocation, &tokens)
            .map_err(SpendReadError::Build)?;
    }

    let mut run_records = Vec::with_capacity(runs.len());
    for run in &runs {
        let created_at = created_at_text(&run[2]).map_err(SpendReadError::Build)?;
        let tally = per_run
            .get(&Key::of(&run[0]))
            .cloned()
            .unwrap_or_else(Tally::new);
        run_records.push(Run {
            id: run[0].clone(),
            status: run[1].clone(),
            created_at,
            tally,
        });
    }
    sort_purposes(&mut per_purpose).map_err(SpendReadError::Build)?;
    Ok(Spend {
        total,
        runs: run_records,
        purposes: per_purpose,
    })
}

/// `sorted(per_purpose)`: numbers order among themselves, text among itself, and any other mix
/// raises `TypeError`. Bytes purposes then fail the JSON encoding.
fn sort_purposes(purposes: &mut [(Cell, Tally)]) -> Result<(), SpendBuildError> {
    if purposes
        .iter()
        .any(|(purpose, _)| matches!(purpose, Cell::Blob(_)))
    {
        return Err(SpendBuildError::Value(
            "bytes purpose is not JSON serializable",
        ));
    }
    if purposes.len() < 2 {
        return Ok(());
    }
    let all_numbers = purposes
        .iter()
        .all(|(purpose, _)| matches!(purpose, Cell::Int(_) | Cell::Real(_)));
    let all_text = purposes
        .iter()
        .all(|(purpose, _)| matches!(purpose, Cell::Text(_)));
    if !all_numbers && !all_text {
        return Err(SpendBuildError::Value(
            "purposes are not orderable together",
        ));
    }
    purposes.sort_by(|(a, _), (b, _)| match (a, b) {
        (Cell::Text(x), Cell::Text(y)) => x.cmp(y),
        (x, y) => compare_numeric(x, y),
    });
    Ok(())
}

fn compare_numeric(a: &Cell, b: &Cell) -> Ordering {
    let as_ref = |cell: &Cell| match cell {
        Cell::Int(value) => py::NumberRef::Int(i128::from(*value)),
        Cell::Real(value) => py::NumberRef::Float(*value),
        _ => py::NumberRef::Int(0),
    };
    py::compare_numbers(as_ref(a), as_ref(b))
}

// ---------------------------------------------------------------------------------------------
// Ledger append (bin/fm-pipeline-spend.sh, the record writer)
// ---------------------------------------------------------------------------------------------

/// What the ledger write did, as the program reports it on stdout.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LedgerOutcome {
    Recorded,
    AlreadyRecorded,
}

/// Why the record was not written. Every variant is exit 1 in the program.
#[derive(Debug)]
pub enum SpendRecordError {
    Build(SpendBuildError),
    Ledger(LedgerError),
}

impl std::fmt::Display for SpendRecordError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            SpendRecordError::Build(error) => write!(f, "{error}"),
            SpendRecordError::Ledger(error) => write!(f, "{error}"),
        }
    }
}

/// A ledger failure.
#[derive(Debug)]
pub enum LedgerError {
    Io(std::io::Error),
    /// The ledger is not valid UTF-8, so Python's text read raises `UnicodeDecodeError`.
    Decode,
    /// A ledger line nests deeper than the JSON decoder allows, which Python raises as
    /// `RecursionError`, not `ValueError`, so the program aborts.
    NestingTooDeep,
}

impl std::fmt::Display for LedgerError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            LedgerError::Io(error) => write!(f, "cannot write the ledger: {error}"),
            LedgerError::Decode => write!(f, "the ledger is not valid UTF-8"),
            LedgerError::NestingTooDeep => write!(f, "a ledger line nests too deeply to decode"),
        }
    }
}

/// Builds the record for `request` and appends it to `ledger` unless the ledger already holds
/// the same task and spawn generation.
pub fn record_spend(
    request: &SpendRequest<'_>,
    ledger: &Path,
    recorded_at: i64,
) -> Result<LedgerOutcome, SpendRecordError> {
    let line = spend_record_line(request, recorded_at).map_err(SpendRecordError::Build)?;
    let spawn_gen = (!request.spawn_gen.is_empty()).then_some(request.spawn_gen);
    append_record(ledger, request.task, spawn_gen, &line).map_err(SpendRecordError::Ledger)
}

/// The text the program prints after a record: `recorded` or `already recorded`, then the task
/// and its spawn generation or `-`.
pub fn render_spend_outcome(outcome: LedgerOutcome, task: &str, spawn_gen: &str) -> String {
    let generation = if spawn_gen.is_empty() { "-" } else { spawn_gen };
    match outcome {
        LedgerOutcome::Recorded => format!("recorded {task} {generation}\n"),
        LedgerOutcome::AlreadyRecorded => format!("already recorded {task} {generation}\n"),
    }
}

fn append_record(
    ledger: &Path,
    task: &str,
    spawn_gen: Option<&str>,
    line: &str,
) -> Result<LedgerOutcome, LedgerError> {
    let mut file = OpenOptions::new()
        .read(true)
        .append(true)
        .create(true)
        .mode(0o600)
        .custom_flags(libc::O_NOFOLLOW)
        .open(ledger)
        .map_err(LedgerError::Io)?;
    file.lock().map_err(LedgerError::Io)?;
    let mut bytes = Vec::new();
    file.read_to_end(&mut bytes).map_err(LedgerError::Io)?;
    let text = std::str::from_utf8(&bytes).map_err(|_| LedgerError::Decode)?;
    // Python's text mode reads with universal newlines, so a CR or CRLF ends a line too.
    let content = text.replace("\r\n", "\n").replace('\r', "\n");
    if ledger_holds(&content, task, spawn_gen)? {
        return Ok(LedgerOutcome::AlreadyRecorded);
    }
    let mut out = String::with_capacity(line.len() + 2);
    if !content.is_empty() && !content.ends_with('\n') {
        out.push('\n');
    }
    out.push_str(line);
    out.push('\n');
    file.write_all(out.as_bytes()).map_err(LedgerError::Io)?;
    file.sync_all().map_err(LedgerError::Io)?;
    Ok(LedgerOutcome::Recorded)
}

/// The program's duplicate test: a line that decodes to an object whose `task` is the task and
/// whose `spawn_gen` equals the record's (a missing key reads as null).
fn ledger_holds(content: &str, task: &str, spawn_gen: Option<&str>) -> Result<bool, LedgerError> {
    for line in py::splitlines(content) {
        let fields = match py::json_loads(line) {
            Ok(Json::Object(fields)) => fields,
            Ok(_) => continue,
            Err(JsonError::Malformed) => continue,
            Err(JsonError::TooDeep) => return Err(LedgerError::NestingTooDeep),
        };
        let field = |name: &str| {
            fields
                .iter()
                .rev()
                .find(|(key, _)| py::units_eq(key, name))
                .map(|(_, value)| value)
        };
        let task_matches =
            matches!(field("task"), Some(Json::Str(units)) if py::units_eq(units, task));
        let generation_matches = match spawn_gen {
            Some(generation) => {
                matches!(field("spawn_gen"), Some(Json::Str(units)) if py::units_eq(units, generation))
            }
            None => matches!(field("spawn_gen"), None | Some(Json::Null)),
        };
        if task_matches && generation_matches {
            return Ok(true);
        }
    }
    Ok(false)
}
