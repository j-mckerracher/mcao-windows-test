//! `mcao-probe` binary. See `args::USAGE`.
//!
//! Exit 0 when the run completed and its evidence was written, even if an operation recorded
//! `unresolved` or `unsupported`: that is evidence, not a crash. Exit 2 for refused arguments.
//! Exit 1 when evidence could not be encoded or written. `mcao-probe stub-harness` is not a probe
//! run: it is the stub child the native backends start.

use std::fs;
use std::process::ExitCode;

use mcao_probe::args::{self, Backend, Config};
use mcao_probe::redact::{self, Manifest};
use mcao_probe::{lifecycle, mock, native, stub};

fn main() -> ExitCode {
    let mut args = std::env::args().skip(1).peekable();
    if args.peek().map(String::as_str) == Some(stub::SUBCOMMAND) {
        args.next();
        return run_stub(args);
    }

    let config = match args::parse(args) {
        Ok(config) => config,
        Err(error) => {
            eprintln!("mcao-probe: {error}\n{}", args::USAGE);
            return ExitCode::from(2);
        }
    };

    match probe(&config) {
        Ok(()) => ExitCode::SUCCESS,
        Err(message) => {
            eprintln!("mcao-probe: {message}");
            ExitCode::FAILURE
        }
    }
}

fn run_stub(args: impl Iterator<Item = String>) -> ExitCode {
    let options = match stub::Options::parse(args) {
        Ok(options) => options,
        Err(message) => {
            eprintln!("mcao-probe stub-harness: {message}");
            return ExitCode::from(2);
        }
    };
    match stub::run(options) {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) => {
            eprintln!("mcao-probe stub-harness: {error}");
            ExitCode::FAILURE
        }
    }
}

fn probe(config: &Config) -> Result<(), String> {
    let mut backend: Box<dyn mcao_core::SessionBackend> = match config.backend {
        Backend::Mock => Box::new(mock::MockBackend::new(config.scenario)),
        Backend::Native => native::backend(config.scenario),
    };
    let records = lifecycle::run_lifecycle(backend.as_mut(), config.backend.harness());
    // Dropping the backend releases anything it still owns before evidence is written.
    drop(backend);

    let mut evidence = serde_json::to_value(&records)
        .map_err(|error| format!("could not encode evidence: {error}"))?;
    let manifest = redact::redact(&mut evidence, &redact::machine_rules());
    let json = serde_json::to_string_pretty(&evidence)
        .map_err(|error| format!("could not encode evidence: {error}"))?;
    println!("{json}");

    if let Some(path) = &config.evidence_out {
        write_artifact(path, &json, &manifest)?;
    }
    Ok(())
}

/// Writes the evidence to `path` and its manifest beside it as `<path>.redaction.json`.
fn write_artifact(path: &std::path::Path, json: &str, manifest: &Manifest) -> Result<(), String> {
    let mut manifest_path = path.as_os_str().to_owned();
    manifest_path.push(".redaction.json");
    let manifest_json = serde_json::to_string_pretty(manifest)
        .map_err(|error| format!("could not encode the redaction manifest: {error}"))?;
    fs::write(path, json)
        .map_err(|error| format!("could not write {}: {error}", path.display()))?;
    fs::write(&manifest_path, manifest_json).map_err(|error| {
        format!(
            "could not write {}: {error}",
            std::path::Path::new(&manifest_path).display()
        )
    })
}
