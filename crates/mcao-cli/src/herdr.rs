//! `mcao herdr`: commands over the herdr control socket and the lab viewer. `eventwait` subscribes
//! to pane status changes and prints one line per event; `workspace-move` sends one `workspace.move`
//! request and prints the reply. Neither issues a lifecycle command. `lab-viewer` attaches a
//! foreground viewer to a lab session over a pty (see `herdr_lab_viewer`).
//!
//! The wire and the waits live in `mcao_core::herdr`. This module parses the flags, prints the
//! results, and maps a failure to its exit status. On Windows all three commands exit 7, the
//! unsupported result.

use std::io;
use std::path::PathBuf;
use std::process::ExitCode;

use clap::{Args, Subcommand};
use mcao_core::herdr::eventwait::{self, EventwaitRequest};
use mcao_core::herdr::workspace_move::{self, WorkspaceMoveRequest};

use crate::herdr_lab_viewer;

/// Arguments for `mcao herdr`, which holds one subcommand.
#[derive(Debug, Args)]
pub struct HerdrArgs {
    #[command(subcommand)]
    command: HerdrCommand,
}

#[derive(Debug, Subcommand)]
enum HerdrCommand {
    /// Subscribe to status changes for the given panes and print one line per event until the
    /// timeout. Each line holds `pane_id`, `workspace_id`, `agent_status`, and `agent`, separated
    /// by TABs.
    Eventwait(EventwaitArgs),
    /// Move a workspace to an insert index and print herdr's reply. The reply must be the
    /// `workspace_list` answer to this request.
    WorkspaceMove(WorkspaceMoveArgs),
    /// Attach one foreground herdr viewer to a lab session over a pty, record the viewer's identity
    /// in PIDFILE, and exit with the viewer's status. Only `fm-lab-` sessions are accepted.
    LabViewer(LabViewerArgs),
}

/// Flags for `mcao herdr eventwait`.
#[derive(Debug, Args)]
struct EventwaitArgs {
    /// Path of the herdr control socket.
    socket: PathBuf,
    /// Seconds to stay subscribed, a positive number.
    timeout: String,
    /// Pane ids to subscribe to; at least one.
    #[arg(required = true)]
    panes: Vec<String>,
}

/// Flags for `mcao herdr workspace-move`.
#[derive(Debug, Args)]
struct WorkspaceMoveArgs {
    /// Path of the herdr control socket. It must be absolute.
    socket: PathBuf,
    /// The workspace id to move. It must be non-empty. A leading dash is part of the id, as the
    /// reference script reads it, so `-ws-1` is not parsed as a flag.
    #[arg(allow_hyphen_values = true)]
    workspace_id: String,
    /// The position to move the workspace to: plain non-negative digits. A leading dash reaches
    /// the same parser, so `-1` is refused with the same message as any other bad index.
    #[arg(allow_hyphen_values = true)]
    insert_index: String,
}

/// Flags for `mcao herdr lab-viewer`.
#[derive(Debug, Args)]
struct LabViewerArgs {
    /// The lab session to attach to, named `fm-lab-<label>`. `default` is refused.
    session: String,
    /// Absolute path where the launcher and viewer PIDs and start times are recorded.
    pidfile: PathBuf,
}

/// Runs `mcao herdr`, returning the exit status of its subcommand.
pub fn run(args: &HerdrArgs) -> ExitCode {
    match &args.command {
        HerdrCommand::Eventwait(eventwait_args) => run_eventwait(eventwait_args),
        HerdrCommand::WorkspaceMove(move_args) => run_workspace_move(move_args),
        HerdrCommand::LabViewer(viewer_args) => run_lab_viewer(viewer_args),
    }
}

/// Runs `mcao herdr eventwait`. Its failures keep the reference script's exit codes.
fn run_eventwait(args: &EventwaitArgs) -> ExitCode {
    let outcome = eventwait::parse_timeout(&args.timeout).and_then(|timeout| {
        let request = EventwaitRequest {
            socket_path: args.socket.clone(),
            timeout,
            panes: args.panes.clone(),
        };
        eventwait::run(&request, &mut io::stdout().lock())
    });
    match outcome {
        Ok(()) => ExitCode::SUCCESS,
        Err(err) => {
            eprintln!("mcao herdr eventwait: {err}");
            ExitCode::from(err.exit_code())
        }
    }
}

/// Runs `mcao herdr workspace-move`. Its failures keep the reference script's exit codes.
fn run_workspace_move(args: &WorkspaceMoveArgs) -> ExitCode {
    let outcome = workspace_move::parse_insert_index(&args.insert_index).and_then(|insert_index| {
        let request = WorkspaceMoveRequest {
            socket_path: args.socket.clone(),
            workspace_id: args.workspace_id.clone(),
            insert_index,
        };
        workspace_move::run(&request, &mut io::stdout().lock())
    });
    match outcome {
        Ok(()) => ExitCode::SUCCESS,
        Err(err) => {
            eprintln!("mcao herdr workspace-move: {err}");
            ExitCode::from(err.exit_code())
        }
    }
}

/// Runs `mcao herdr lab-viewer`. A refused argument or a failed setup exits with its code; a viewer
/// that ran exits with its own status.
fn run_lab_viewer(args: &LabViewerArgs) -> ExitCode {
    match herdr_lab_viewer::run(&args.session, &args.pidfile) {
        Ok(status) => ExitCode::from(status),
        Err(err) => {
            eprintln!("mcao herdr lab-viewer: {err}");
            ExitCode::from(err.exit_code())
        }
    }
}
