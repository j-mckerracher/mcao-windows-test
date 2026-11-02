//! The `mcao` command grammar, parsed by clap and dispatched by `main`.

use std::ffi::OsString;

use clap::{CommandFactory, FromArgMatches, Parser, Subcommand};

use crate::audit::AuditArgs;
use crate::herdr::HerdrArgs;
use crate::hook::HookArgs;
use crate::mem_guard::MemGuardArgs;
use crate::notify::NotifyArgs;
use crate::proc::ProcArgs;
use crate::resources::ResourcesArgs;
use crate::toolchain::ToolchainArgs;

/// Multi-Agent Orchestrator command line.
#[derive(Debug, Parser)]
#[command(
    name = "mcao",
    bin_name = "mcao",
    version,
    about = "Multi-Agent Orchestrator"
)]
pub struct Cli {
    #[command(subcommand)]
    pub command: Option<Command>,
}

#[derive(Debug, Subcommand)]
pub enum Command {
    /// Audit envelope tooling (DPES-884). `validate` checks one envelope or fleet ledger record and
    /// exits 1 for a rejection it records. `activity-map` projects one envelope to a sanitized
    /// `audit.activity` event and exits 1 for a refusal. `append`, `verify`, and `project-ledger`
    /// write and read the hash-chained audit store.
    Audit(AuditArgs),
    /// PreToolUse hook ports (DPES-894). `subagent-pretool` denies delegation outside the fleet.
    Hook(HookArgs),
    /// Host memory and swap verdict, read through the shared host sampler.
    MemGuard(MemGuardArgs),
    /// herdr control-socket commands. `eventwait` reads status events and `workspace-move` sends one
    /// workspace move request; neither issues a lifecycle command.
    Herdr(HerdrArgs),
    /// Governance toolchain qualification. `qualify` writes the evidence record as JSON, and `check`
    /// exits 3 unless the tools a delivery path requires are supported.
    Toolchain(ToolchainArgs),
    /// Opens a URI or path with the platform's default handler, or posts the daemon wedge alarm.
    Notify(NotifyArgs),
    /// Process identity queries. `parent` prints a process's parent PID and creation time, and
    /// guards against PID reuse with `--expect-start`.
    Proc(ProcArgs),
    /// Host resources. `show` prints the snapshot, and `admit` decides whether one agent start may
    /// proceed, exiting 75 with the deferral reason when a threshold is breached.
    Resources(ResourcesArgs),
    /// Prerequisite checks before any state changes: Git on `PATH`, then the `FM_HOME` path-length
    /// and OneDrive rules when `FM_HOME` is set. Exits 3 naming the first failure.
    Preflight,
}

impl Cli {
    /// Parses `argv`, listing `util` in help even though `main` runs it before clap.
    pub fn parse_args(argv: Vec<OsString>) -> Result<Self, clap::Error> {
        let mut matches = Self::command()
            .subcommand(
                clap::Command::new("util")
                    .about("Developer helpers, with their own usage text")
                    .disable_help_flag(true),
            )
            .try_get_matches_from(argv)?;
        Self::from_arg_matches_mut(&mut matches)
    }
}
