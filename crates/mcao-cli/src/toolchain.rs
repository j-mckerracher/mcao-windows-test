//! `mcao toolchain`: qualify the governance toolchain and refuse a delivery path it cannot serve
//! (DPES-890).
//!
//! `qualify` probes this machine and writes the evidence record as JSON. `check` reads a record and
//! exits 3 unless every tool the delivery path requires is supported. Neither command installs
//! anything. The logic lives in `mcao_core::toolchain`; this module parses flags and reports.

use std::io::{self, Write};
use std::path::PathBuf;
use std::process::ExitCode;

use clap::{Args, Subcommand};
use mcao_core::McaoError;
use mcao_core::resources::utc_stamp;
use mcao_core::toolchain::{
    ALL_SUPPORTED_MESSAGE, RunContext, check_required_tools, qualify, required_tools_for,
};
use mcao_core::util::clock;

use crate::toolchain_host::RealHost;

/// Arguments for `mcao toolchain`.
#[derive(Debug, Args)]
pub struct ToolchainArgs {
    #[command(subcommand)]
    command: ToolchainCommand,
}

#[derive(Debug, Subcommand)]
enum ToolchainCommand {
    /// Probe the governance tools on this machine and write the qualification record as JSON.
    Qualify(QualifyArgs),
    /// Exit 3 unless every tool the delivery path requires is supported in a qualification record.
    Check(CheckArgs),
}

#[derive(Debug, Args)]
struct QualifyArgs {
    /// Write the record as JSON. Required, since JSON is the only format.
    #[arg(long)]
    output_json: bool,
    /// Write the record to this file instead of standard output.
    #[arg(long, value_name = "PATH")]
    output_file: Option<PathBuf>,
}

#[derive(Debug, Args)]
struct CheckArgs {
    /// A qualification record written by `qualify --output-json`.
    #[arg(long, value_name = "PATH")]
    evidence: PathBuf,
    /// The delivery path to check: `no-mistakes`, `direct-PR`, or `local-only`.
    #[arg(long, value_name = "DELIVERY_PATH")]
    delivery_path: String,
}

/// Runs `mcao toolchain` and returns its exit status. A failure prints its message and exits with
/// the code its [`McaoError`] variant names.
pub fn run(args: &ToolchainArgs) -> ExitCode {
    let result = match &args.command {
        ToolchainCommand::Qualify(qualify_args) => run_qualify(qualify_args),
        ToolchainCommand::Check(check_args) => run_check(check_args),
    };
    match result {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) => {
            eprintln!("{error}");
            let code = u8::try_from(error.exit_code()).expect("mcao exit codes fit in a byte");
            ExitCode::from(code)
        }
    }
}

fn run_qualify(args: &QualifyArgs) -> Result<(), McaoError> {
    if !args.output_json {
        return Err(McaoError::Usage(
            "qualify requires --output-json".to_owned(),
        ));
    }
    let text = qualify(&RealHost::default(), &run_context()).to_json_text();
    match &args.output_file {
        Some(path) => std::fs::write(path, text).map_err(McaoError::Io),
        None => io::stdout()
            .lock()
            .write_all(text.as_bytes())
            .map_err(McaoError::Io),
    }
}

fn run_check(args: &CheckArgs) -> Result<(), McaoError> {
    let required = required_tools_for(&args.delivery_path)?;
    let text = std::fs::read_to_string(&args.evidence).map_err(|error| {
        McaoError::Preflight(format!(
            "cannot read the qualification record {}: {error}",
            args.evidence.display()
        ))
    })?;
    check_required_tools(&text, required)?;
    println!("{ALL_SUPPORTED_MESSAGE}");
    Ok(())
}

/// The run identity: the UTC time, the CI runner, and the CI job URL when CI sets one.
fn run_context() -> RunContext {
    let epoch_secs = u64::try_from(clock::now_ms() / 1000).unwrap_or_default();
    RunContext {
        timestamp: utc_stamp(epoch_secs),
        runner: env_or("RUNNER_NAME", "local"),
        ci_job_url: ci_job_url(),
    }
}

/// The CI job URL that evidence links point at: `CI_JOB_URL` when the pipeline sets it, otherwise the
/// GitHub Actions run page. GitHub does not expose the job's own URL to a step, so the run page is the
/// fallback. Empty outside CI.
fn ci_job_url() -> String {
    if let Some(url) = non_empty_env("CI_JOB_URL") {
        return url;
    }
    match (
        non_empty_env("GITHUB_SERVER_URL"),
        non_empty_env("GITHUB_REPOSITORY"),
        non_empty_env("GITHUB_RUN_ID"),
    ) {
        (Some(server), Some(repository), Some(run_id)) => {
            format!("{server}/{repository}/actions/runs/{run_id}")
        }
        _ => String::new(),
    }
}

fn non_empty_env(name: &str) -> Option<String> {
    std::env::var(name).ok().filter(|value| !value.is_empty())
}

fn env_or(name: &str, fallback: &str) -> String {
    non_empty_env(name).unwrap_or_else(|| fallback.to_owned())
}
