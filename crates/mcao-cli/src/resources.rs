//! `mcao resources`: the host resource snapshot and the admission decision for one agent start
//! (DPES-1019, DPES-866).
//!
//! Sampling, thresholds, evaluation, and both JSON shapes live in `mcao_core::resources`. This
//! module parses the flags, reads the running-agent records of the local homes, and maps the
//! decision to an exit status. A thresholds file that cannot be applied falls back to the defaults
//! with a warning, and an unmeasurable reading stays unknown, so both fail open.

use std::io::{self, Write};
use std::path::PathBuf;
use std::process::ExitCode;

use clap::{Args, Subcommand, ValueEnum};
use mcao_core::McaoError;
use mcao_core::resources::{
    Admission, AdmitInputs, Disposition, Evaluation, Level, SampleMode, SnapshotView, StartKind,
    StartRequest, State, SysSampler, admit, count_running_agents, default_registry, evaluate,
    load_thresholds, render_admission_json, sample_mode, utc_stamp,
};
use mcao_core::state::TaskId;

use crate::home_guard::refuse_onedrive_home_for_state;
use mcao_core::util::clock;

/// Arguments for `mcao resources`, which holds one subcommand.
#[derive(Debug, Args)]
pub struct ResourcesArgs {
    #[command(subcommand)]
    command: ResourcesCommand,
}

#[derive(Debug, Subcommand)]
enum ResourcesCommand {
    /// Sample every resource and print the snapshot. Exits 0.
    Show(ShowArgs),
    /// Decide whether one agent start may proceed. Exits 0 to admit and 75 to defer, with the
    /// deferral reason on standard error.
    Admit(AdmitArgs),
}

/// Flags both subcommands take.
#[derive(Debug, Args)]
struct HostArgs {
    /// Directory that holds `resource-thresholds`.
    #[arg(long, value_name = "DIR")]
    config_dir: PathBuf,
    /// State directory of a local home, whose task records count as running agents. Repeat it for
    /// each home; the first is the starting home's.
    #[arg(long, value_name = "DIR")]
    state_dir: Vec<PathBuf>,
    /// Path whose filesystem free space is measured. Defaults to the parent of `--config-dir`.
    #[arg(long, value_name = "DIR")]
    disk_path: Option<PathBuf>,
}

/// Flags for `mcao resources show`.
#[derive(Debug, Args)]
struct ShowArgs {
    #[command(flatten)]
    host: HostArgs,
    /// Print the snapshot as JSON instead of text.
    #[arg(long)]
    json: bool,
}

/// Flags for `mcao resources admit`.
#[derive(Debug, Args)]
struct AdmitArgs {
    #[command(flatten)]
    host: HostArgs,
    /// The kind of start being admitted.
    #[arg(long, value_enum)]
    kind: KindArg,
    /// The task id the start would create or relaunch.
    #[arg(long, value_name = "ID")]
    task: String,
}

/// The `--kind` values, which name the same starts as [`StartKind`].
#[derive(Clone, Copy, Debug, ValueEnum)]
enum KindArg {
    Ship,
    Scout,
    Secondmate,
    Relaunch,
}

impl From<KindArg> for StartKind {
    fn from(kind: KindArg) -> Self {
        match kind {
            KindArg::Ship => Self::Ship,
            KindArg::Scout => Self::Scout,
            KindArg::Secondmate => Self::Secondmate,
            KindArg::Relaunch => Self::Relaunch,
        }
    }
}

/// Runs `mcao resources` and returns its exit status. A failure prints its message and exits with
/// the code its [`McaoError`] variant names.
pub fn run(args: &ResourcesArgs) -> ExitCode {
    let result = refuse_onedrive_home_for_state(|name| std::env::var_os(name)).and_then(|()| {
        match &args.command {
            ResourcesCommand::Show(show_args) => run_show(show_args),
            ResourcesCommand::Admit(admit_args) => run_admit(admit_args),
        }
    });
    match result {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) => {
            eprintln!("{error}");
            let code = u8::try_from(error.exit_code()).expect("mcao exit codes fit in a byte");
            ExitCode::from(code)
        }
    }
}

fn run_show(args: &ShowArgs) -> Result<(), McaoError> {
    let disk_path = disk_path(&args.host);
    let loaded = load_thresholds(&args.host.config_dir);
    // `show` reports the running-agent count even when its limit is off, so the count is always taken.
    let agents = Some(count_running_agents(&args.host.state_dir, None));
    let snapshot = sample_mode(
        &SysSampler::new(),
        &loaded.thresholds,
        SampleMode::All,
        &disk_path,
        agents,
        sampled_at(),
        platform(),
    );
    let evaluation = evaluate(snapshot, &loaded);
    let text = if args.json {
        serde_json::to_string(&SnapshotView::from(&evaluation))
            .expect("the snapshot view holds only plain JSON values")
    } else {
        render_text(&evaluation)
    };
    write_stdout(&text)
}

fn run_admit(args: &AdmitArgs) -> Result<(), McaoError> {
    let request = StartRequest {
        kind: args.kind.into(),
        task: TaskId::parse(&args.task)?,
    };
    let inputs = AdmitInputs {
        config_dir: args.host.config_dir.clone(),
        state_dirs: args.host.state_dir.clone(),
        disk_path: disk_path(&args.host),
    };
    let admission = admit(&request, &inputs, &SysSampler::new(), &default_registry());
    if let Some(warning) = &admission.evaluation.config_warning {
        eprintln!("warning: {warning}");
    }
    write_stdout(&render_admission_json(&request, &admission))?;
    decide(admission)
}

/// The exit decision for an admission: `Ok` admits, and a deferral becomes [`McaoError::Deferred`].
fn decide(admission: Admission) -> Result<(), McaoError> {
    match admission.disposition {
        Disposition::Admit => Ok(()),
        Disposition::Defer(deferral) => Err(McaoError::Deferred(deferral)),
    }
}

/// `--disk-path`, or the parent of the config directory when it is absent. The parent is the
/// filesystem that holds the root Firstmate home. A bare relative name such as `cfg` has an empty
/// parent, which names the working directory, so it maps to `.`.
fn disk_path(host: &HostArgs) -> PathBuf {
    host.disk_path
        .clone()
        .unwrap_or_else(|| match host.config_dir.parent() {
            Some(parent) if parent.as_os_str().is_empty() => PathBuf::from("."),
            Some(parent) => parent.to_path_buf(),
            None => host.config_dir.clone(),
        })
}

/// The current time as RFC 3339 UTC, for the snapshot's `sampled_at`.
fn sampled_at() -> String {
    utc_stamp(u64::try_from(clock::now_ms() / 1000).unwrap_or_default())
}

/// `<os>-<arch>`, the platform form the snapshot contract uses.
fn platform() -> String {
    format!("{}-{}", std::env::consts::OS, std::env::consts::ARCH)
}

/// The snapshot as one line per resource, for `mcao resources show` without `--json`.
fn render_text(evaluation: &Evaluation) -> String {
    let snapshot = &evaluation.snapshot;
    let mut lines = vec![format!(
        "resources {} on {}: {}",
        snapshot.sampled_at,
        snapshot.platform,
        state_label(evaluation.state)
    )];
    lines.push(match &evaluation.config_warning {
        Some(warning) => format!("config: {} (warning: {warning})", evaluation.config_source),
        None => format!("config: {}", evaluation.config_source),
    });
    for (kind, level, limit) in &evaluation.levels {
        let reading = snapshot.readings.iter().find(|r| r.kind == *kind);
        let value = match reading {
            Some(r) => match (r.value, &r.reason) {
                (Some(value), _) => value.to_string(),
                (None, Some(reason)) => format!("unknown ({reason})"),
                (None, None) => "unknown".to_owned(),
            },
            None => "unknown".to_owned(),
        };
        let limit = limit.map_or_else(|| "off".to_owned(), |limit| limit.to_string());
        lines.push(format!(
            "{}: {value}, limit {limit}, {}",
            kind.key(),
            level_label(*level)
        ));
    }
    lines.join("\n")
}

fn state_label(state: State) -> &'static str {
    match state {
        State::Ok => "ok",
        State::Breached => "breached",
        State::Unknown => "unknown",
    }
}

fn level_label(level: Level) -> &'static str {
    match level {
        Level::Ok => "ok",
        Level::Breached => "breached",
        Level::Unknown => "unknown",
        Level::Disabled => "disabled",
    }
}

/// Writes `text` and a newline to standard output.
fn write_stdout(text: &str) -> Result<(), McaoError> {
    writeln!(io::stdout().lock(), "{text}").map_err(McaoError::Io)
}
