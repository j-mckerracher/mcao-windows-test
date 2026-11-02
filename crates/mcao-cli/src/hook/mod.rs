//! `mcao hook`: native ports of the firstmate hook entrypoints (DPES-894, DPES-893).
//!
//! `subagent-pretool` ports `bin/fm-subagent-pretool-check.sh`, and `session-start` wires the
//! session-open decisions of `mcao_core::hook_source` to `bin/fm-sessionstart-run.sh`. The shell
//! scripts stay the behavioral authority, and this change changes no registration.
//! `bin/fm-subagent-pretool-check.sh` is wired only through `.claude/settings.json`, which Cursor
//! also loads. Codex, OpenCode, Pi, and Grok have no subagent registration
//! (`docs/subagent-guard.md`, "Known residual gap"). `host-mirror` ports the writer half of
//! `bin/fm-host-mirror.sh` and is unwired. `turnend-guard`, `stop-autoarm`, and `cd-pretool` are
//! not ported yet. Divergences are listed in [`subagent_pretool`], [`session_start`], and
//! [`host_mirror`].

mod host_mirror;
mod session_start;
mod subagent_pretool;

use std::process::ExitCode;

use clap::{Args, Subcommand};

pub use host_mirror::HostMirrorArgs;
pub use session_start::SessionStartHookArgs;
pub use subagent_pretool::SubagentPretoolArgs;

/// Flags for `mcao hook`.
#[derive(Debug, Args)]
pub struct HookArgs {
    #[command(subcommand)]
    pub command: HookCommand,
}

#[derive(Debug, Subcommand)]
pub enum HookCommand {
    /// PreToolUse guard against delegating work outside the fleet. Exits 0 to allow and 2 to deny.
    SubagentPretool(SubagentPretoolArgs),
    /// Session-open source router (DPES-893). Runs the full digest, the re-emit, or the nudge.
    /// Accepts the shell's arguments, `--source <name>` and `--pi-prerequisite`.
    SessionStart(SessionStartHookArgs),
    /// Dialog-mirror writer (DPES-894). Appends one user or main entry from a hook payload to
    /// `state/.host-mirror.jsonl` under the shell's directory lock. Always exits 0, as the shell hook does.
    HostMirror(HostMirrorArgs),
}

/// Runs `mcao hook`. The hook owns its exit code: 0 allows and 2 denies.
pub fn run(args: &HookArgs) -> ExitCode {
    match &args.command {
        HookCommand::SubagentPretool(args) => subagent_pretool::run(args),
        HookCommand::SessionStart(args) => session_start::run(args),
        HookCommand::HostMirror(args) => host_mirror::run(args),
    }
}
