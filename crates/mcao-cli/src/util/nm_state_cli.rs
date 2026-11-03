//! `mcao util nm-run-inventory` and `mcao util nm-spend-record`: the command-line entry points of the
//! no-mistakes readers in `nm_state`.
//!
//! Each command takes the arguments its Python program reads from `sys.argv`, in the same order,
//! with two differences. The inventory's `no-mistakes axi` overview arrives on stdin instead of as
//! an argument, and the inventory's database comes first, as the command's contract names it. The
//! stdout bytes and the exit status are the program's; see `nm_state` for the readers themselves.

use std::io::{self, Read, Write};
use std::path::Path;
use std::process::ExitCode;

use mcao_core::util::clock;

use super::nm_state::{
    InventoryRequest, SpendRequest, read_run_inventory, record_spend, render_run_inventory,
    render_spend_outcome,
};

const INVENTORY_USAGE: &str =
    "usage: mcao util nm-run-inventory <database> <branch> <available-ids-csv>";
const SPEND_USAGE: &str = "usage: mcao util nm-spend-record <ledger> <task> <spawn-gen> <repo> <branch> <since> <database> <reason>";

/// `nm-run-inventory <database> <branch> <available-ids-csv>`, with the overview on stdin.
pub fn inventory(args: &[String]) -> ExitCode {
    let [database, branch, available_ids] = args else {
        eprintln!("{INVENTORY_USAGE}");
        return ExitCode::from(2);
    };
    let mut input = Vec::new();
    if let Err(err) = io::stdin().lock().read_to_end(&mut input) {
        eprintln!("mcao util: cannot read stdin: {err}");
        return ExitCode::from(1);
    }
    // Undecodable overview bytes become U+FFFD. Python keeps them as surrogates, which differs only
    // when the database stores U+FFFD in the repository path; the declared gap is in the tests.
    let overview = String::from_utf8_lossy(&input);
    let request = InventoryRequest {
        database: Path::new(database),
        branch,
        overview: &overview,
        available_ids,
    };
    emit(&render_run_inventory(&read_run_inventory(&request)))
}

/// `nm-spend-record <ledger> <task> <spawn-gen> <repo> <branch> <since> <database> <reason>`.
pub fn spend_record(args: &[String]) -> ExitCode {
    let [
        ledger,
        task,
        spawn_gen,
        repo,
        branch,
        since,
        database,
        reason,
    ] = args
    else {
        eprintln!("{SPEND_USAGE}");
        return ExitCode::from(2);
    };
    let request = SpendRequest {
        task,
        spawn_gen,
        repo,
        branch,
        since,
        database,
        reason,
    };
    match record_spend(
        &request,
        Path::new(ledger),
        clock::now_ms().div_euclid(1000),
    ) {
        Ok(outcome) => emit(&render_spend_outcome(outcome, task, spawn_gen)),
        Err(error) => {
            eprintln!("mcao util: nm-spend-record {ledger}: {error}");
            ExitCode::from(1)
        }
    }
}

fn emit(text: &str) -> ExitCode {
    let mut stdout = io::stdout().lock();
    if stdout
        .write_all(text.as_bytes())
        .and_then(|()| stdout.flush())
        .is_err()
    {
        return ExitCode::from(1);
    }
    ExitCode::SUCCESS
}
