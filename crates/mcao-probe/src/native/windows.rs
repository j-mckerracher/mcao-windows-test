//! Windows process backend: the stub harness inside a Job Object.
//!
//! The child is started with no terminal and no redirected standard handles, because the Windows
//! backend has no ConPTY yet. What this backend can prove is process ownership: the child and its
//! own child are both inside one job, the job reports a live count, and terminating the job ends
//! the tree. Capture, input and Escape are `Unsupported` until a terminal exists.

use std::env;
use std::thread;
use std::time::Duration;

use mcao_backend_windows::job::JobObject;
use mcao_backend_windows::process::{ChildProcess, spawn_in_job};
use mcao_core::{AgentState, Execution, OpError, SessionBackend};

use super::{RECONNECT_UNSUPPORTED, WAIT_INTERVAL, WAIT_POLLS};
use crate::stub::{self, Options};

const NO_TERMINAL: &str = "the Windows backend starts the child with no terminal and no redirected standard handles, so there is no output to read and no input or Escape to send; ConPTY wiring is a later slice";

/// Processes the stub tree holds: the stub and the one child it starts.
const TREE_SIZE: u32 = 2;
/// Exit code the job forces on its members at cleanup.
const CLEANUP_EXIT: u32 = 1;

struct Session {
    handle: String,
    job: JobObject,
    child: ChildProcess,
}

pub struct WindowsJobBackend {
    session: Option<Session>,
}

impl WindowsJobBackend {
    pub fn new() -> Self {
        Self { session: None }
    }

    fn session(&self, handle: &str) -> Result<&Session, OpError> {
        self.session
            .as_ref()
            .filter(|session| session.handle == handle)
            .ok_or_else(|| OpError::Failed(format!("no session `{handle}`")))
    }
}

impl Drop for WindowsJobBackend {
    fn drop(&mut self) {
        if let Some(session) = self.session.take() {
            // Best effort: the stub also stops at its own deadline.
            let _ = session.job.terminate(CLEANUP_EXIT);
        }
    }
}

/// Polls the job until it holds exactly `expected` live processes or the bounded wait runs out.
/// Returns the last count read.
fn wait_for_count(job: &JobObject, expected: u32) -> Result<u32, OpError> {
    let mut count = read_count(job)?;
    for _ in 0..WAIT_POLLS {
        if count == expected {
            break;
        }
        thread::sleep(WAIT_INTERVAL);
        count = read_count(job)?;
    }
    Ok(count)
}

fn read_count(job: &JobObject) -> Result<u32, OpError> {
    job.active_processes()
        .map_err(|error| OpError::Failed(format!("cannot read the job accounting: {error}")))
}

impl SessionBackend for WindowsJobBackend {
    fn name(&self) -> &'static str {
        "windows-job"
    }

    fn execution(&self) -> Execution {
        Execution::Native
    }

    /// Succeeds only when the job reports both the stub and its child alive.
    fn spawn(&mut self, harness: &str) -> Result<String, OpError> {
        if harness != stub::HARNESS {
            return Err(OpError::Unsupported(format!(
                "the windows-job backend runs only the `{}` harness",
                stub::HARNESS
            )));
        }
        if self.session.is_some() {
            return Err(OpError::Failed("a session is already running".to_owned()));
        }
        let exe = env::current_exe()
            .map_err(|error| OpError::Failed(format!("cannot locate the probe binary: {error}")))?;
        let job = JobObject::new()
            .map_err(|error| OpError::Failed(format!("cannot create the job: {error}")))?;
        let mut args = vec![stub::SUBCOMMAND.to_owned()];
        args.extend(
            Options {
                no_stdin: true,
                with_child: true,
                ..Options::default()
            }
            .to_args(),
        );
        let args: Vec<&str> = args.iter().map(String::as_str).collect();
        let child = spawn_in_job(&job, &exe, &args)
            .map_err(|error| OpError::Failed(format!("cannot start the stub harness: {error}")))?;

        let count = wait_for_count(&job, TREE_SIZE);
        if !matches!(count, Ok(TREE_SIZE)) {
            let _ = job.terminate(CLEANUP_EXIT);
            return Err(OpError::Failed(match count {
                Ok(count) => format!(
                    "the job held {count} live processes within the bounded wait, expected {TREE_SIZE}"
                ),
                Err(OpError::Failed(reason) | OpError::Unsupported(reason)) => reason,
            }));
        }
        let handle = format!("windows-job-{}", child.id());
        self.session = Some(Session {
            handle: handle.clone(),
            job,
            child,
        });
        Ok(handle)
    }

    fn capture(&self, handle: &str) -> Result<String, OpError> {
        self.session(handle)?;
        Err(OpError::Unsupported(NO_TERMINAL.to_owned()))
    }

    fn send_text(&mut self, handle: &str, _text: &str) -> Result<(), OpError> {
        self.session(handle)?;
        Err(OpError::Unsupported(NO_TERMINAL.to_owned()))
    }

    fn interrupt(&mut self, handle: &str) -> Result<(), OpError> {
        self.session(handle)?;
        Err(OpError::Unsupported(NO_TERMINAL.to_owned()))
    }

    /// The stub with no terminal has no turn to finish, so a live process reads as running.
    fn agent_state(&self, handle: &str) -> Result<AgentState, OpError> {
        let session = self.session(handle)?;
        match session.child.wait_timeout(Duration::ZERO) {
            Ok(None) => Ok(AgentState::Running),
            Ok(Some(_)) => Ok(AgentState::Missing),
            Err(error) => Err(OpError::Failed(format!(
                "cannot read the child status: {error}"
            ))),
        }
    }

    fn reconnect(&self, handle: &str) -> Result<(), OpError> {
        self.session(handle)?;
        Err(OpError::Unsupported(RECONNECT_UNSUPPORTED.to_owned()))
    }

    /// Terminates the job and succeeds only when it reports no live process and the stub has exited.
    fn cleanup(&mut self, handle: &str) -> Result<(), OpError> {
        self.session(handle)?;
        let session = self.session.take().expect("session checked above");
        let before = read_count(&session.job)?;
        session
            .job
            .terminate(CLEANUP_EXIT)
            .map_err(|error| OpError::Failed(format!("cannot terminate the job: {error}")))?;
        let after = wait_for_count(&session.job, 0)?;
        if after != 0 {
            return Err(OpError::Failed(format!(
                "the job still held {after} live processes after termination (held {before} before)"
            )));
        }
        match session.child.wait_timeout(WAIT_INTERVAL * 20) {
            Ok(Some(_)) => Ok(()),
            Ok(None) => Err(OpError::Failed(
                "the job reported no live process but the stub had not exited".to_owned(),
            )),
            Err(error) => Err(OpError::Failed(format!(
                "cannot confirm the stub exited: {error}"
            ))),
        }
    }
}
