//! The SQLite side of the readers: a read-only connection with Python's busy timeout, cell values
//! as `sqlite3` returns them to Python, and the text decoding that raises for invalid UTF-8.

use std::path::Path;
use std::time::Duration;

use rusqlite::types::{Value as SqlValue, ValueRef};
use rusqlite::{Connection, OpenFlags};

/// `sqlite3.connect(..., timeout=30)`: the busy handler waits this long for a lock.
const BUSY_TIMEOUT: Duration = Duration::from_secs(30);

/// One column value as Python's `sqlite3` yields it: `None`, `int`, `float`, `str` or `bytes`.
#[derive(Debug, Clone, PartialEq)]
pub enum Cell {
    Null,
    Int(i64),
    Real(f64),
    Text(String),
    Blob(Vec<u8>),
}

impl Cell {
    /// Python truthiness of the value, as `value or 0` evaluates it.
    pub fn is_truthy(&self) -> bool {
        match self {
            Cell::Null => false,
            Cell::Int(value) => *value != 0,
            Cell::Real(value) => *value != 0.0,
            Cell::Text(text) => !text.is_empty(),
            Cell::Blob(bytes) => !bytes.is_empty(),
        }
    }
}

/// Python's equality and hash for dictionary keys: `1 == 1.0`, while text and bytes never match.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum Key {
    Null,
    Int(i128),
    /// A non-integral, infinite or out-of-range float, by its bit pattern.
    Float(u64),
    Text(String),
    Blob(Vec<u8>),
}

impl Key {
    pub fn of(cell: &Cell) -> Key {
        match cell {
            Cell::Null => Key::Null,
            Cell::Int(value) => Key::Int(i128::from(*value)),
            Cell::Real(value) => {
                let integral = value.is_finite() && value.fract() == 0.0;
                if integral && value.abs() < 1.701_411_834_604_692_3e38 {
                    Key::Int(*value as i128)
                } else {
                    Key::Float(value.to_bits())
                }
            }
            Cell::Text(text) => Key::Text(text.clone()),
            Cell::Blob(bytes) => Key::Blob(bytes.clone()),
        }
    }
}

/// A Python exception the reader would raise, with its `str()` text.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DbError {
    pub message: String,
}

/// Opens `path` read-only with the 30 second busy timeout. Read-only means no journal, no
/// checkpoint, and no creation of the database file; WAL readers still create `-shm` and
/// `-wal` next to it, as Python's `mode=ro` connection does.
pub fn open_read_only(path: &Path) -> Result<Connection, DbError> {
    let connection = Connection::open_with_flags(path, OpenFlags::SQLITE_OPEN_READ_ONLY)
        .map_err(|err| open_error(&err))?;
    connection
        .busy_timeout(BUSY_TIMEOUT)
        .map_err(|err| sqlite_error(&err))?;
    Ok(connection)
}

/// `BEGIN` as Python's reader issues it: a deferred transaction, so the reads that follow see one
/// snapshot and the lock is taken by the first read rather than here.
pub fn begin(connection: &Connection) -> Result<(), DbError> {
    connection
        .execute_batch("BEGIN")
        .map_err(|err| sqlite_error(&err))
}

/// Compiles `sql` without binding or stepping it. Compiling reads the schema, so it takes the
/// read lock and creates the WAL sidecars the way CPython's `execute` does before it binds.
pub fn prepare(connection: &Connection, sql: &str) -> Result<(), DbError> {
    connection
        .prepare(sql)
        .map(drop)
        .map_err(|err| sqlite_error(&err))
}

/// Runs `sql` with `params` and returns every row. Like Python's `fetchall`, it fails on the first
/// TEXT cell in any row that is not valid UTF-8, naming that column.
pub fn query(
    connection: &Connection,
    sql: &str,
    params: &[Cell],
) -> Result<Vec<Vec<Cell>>, DbError> {
    let mut statement = connection.prepare(sql).map_err(|err| sqlite_error(&err))?;
    let names: Vec<String> = statement.column_names().map(str::to_owned).collect();
    let bound: Vec<SqlValue> = params.iter().map(to_sql_value).collect();
    let mut rows = statement
        .query(rusqlite::params_from_iter(bound))
        .map_err(|err| sqlite_error(&err))?;
    let mut out = Vec::new();
    while let Some(row) = rows.next().map_err(|err| sqlite_error(&err))? {
        let mut cells = Vec::with_capacity(names.len());
        for (index, name) in names.iter().enumerate() {
            let cell = match row.get_ref(index).map_err(|err| sqlite_error(&err))? {
                ValueRef::Null => Cell::Null,
                ValueRef::Integer(value) => Cell::Int(value),
                ValueRef::Real(value) => Cell::Real(value),
                ValueRef::Text(bytes) => match std::str::from_utf8(bytes) {
                    Ok(text) => Cell::Text(text.to_owned()),
                    Err(_) => {
                        return Err(DbError {
                            message: format!(
                                "Could not decode to UTF-8 column '{name}' with text '{}'",
                                String::from_utf8_lossy(bytes)
                            ),
                        });
                    }
                },
                ValueRef::Blob(bytes) => Cell::Blob(bytes.to_vec()),
            };
            cells.push(cell);
        }
        out.push(cells);
    }
    Ok(out)
}

fn to_sql_value(cell: &Cell) -> SqlValue {
    match cell {
        Cell::Null => SqlValue::Null,
        Cell::Int(value) => SqlValue::Integer(*value),
        Cell::Real(value) => SqlValue::Real(*value),
        Cell::Text(text) => SqlValue::Text(text.clone()),
        Cell::Blob(bytes) => SqlValue::Blob(bytes.clone()),
    }
}

/// A failed open as CPython's `sqlite3.connect` words it. rusqlite appends the path to the message,
/// which CPython does not, so the open failure is named by its code.
fn open_error(err: &rusqlite::Error) -> DbError {
    match err {
        rusqlite::Error::SqliteFailure(failure, _)
            if failure.code == rusqlite::ErrorCode::CannotOpen =>
        {
            DbError {
                message: "unable to open database file".to_owned(),
            }
        }
        other => sqlite_error(other),
    }
}

fn sqlite_error(err: &rusqlite::Error) -> DbError {
    let message = match err {
        rusqlite::Error::SqliteFailure(_, Some(message)) => message.clone(),
        other => other.to_string(),
    };
    DbError { message }
}
