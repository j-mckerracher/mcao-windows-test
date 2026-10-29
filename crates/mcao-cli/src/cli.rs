//! The `mcao` command grammar, parsed by clap and dispatched by `main`.

use std::ffi::OsString;

use clap::{CommandFactory, FromArgMatches, Parser, Subcommand};

use crate::herdr::HerdrArgs;
use crate::mem_guard::MemGuardArgs;
use crate::notify::NotifyArgs;
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
