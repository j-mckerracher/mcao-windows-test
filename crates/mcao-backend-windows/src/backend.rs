//! The Windows worker backend behind the shared [`SessionBackend`] contract.
//!
//! A [`WindowsBackend`] session is a [`Session`]: a child in its own Job Object, on a
//! ConPTY terminal. Each contract operation maps onto that session and nothing else.
//! The interrupt is [`escalate`], the path that DPES-886 AC4 landed.

use std::collections::BTreeMap;
use std::path::PathBuf;
use std::thread;
use std::time::{Duration, Instant};

use mcao_core::{AgentState, Execution, InterruptSemantics, OpError, SessionBackend};

use crate::conpty::negotiated_size;
use crate::interrupt::escalate;
use crate::session::Session;

/// The total wait an interrupt gets before its job is terminated.
///
/// DPES-755 proposes 500 ms in total. The measured value is owed by DPES-758 on the
/// Windows runner, so this is a proposal and not a measurement.
pub const INTERRUPT_WAIT: Duration = Duration::from_millis(500);

/// The exit code that a cleanup or a drop gives the job.
const CLEANUP_EXIT: u32 = 1;

/// How long a cleanup waits for the child to exit and the job to empty.
const CLEANUP_WAIT: Duration = Duration::from_secs(5);

/// How often a cleanup reads the child status and the job member count.
const CLEANUP_POLL: Duration = Duration::from_millis(25);

/// The program and arguments that start one harness.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Launch {
    /// The program to start. The rules of [`crate::process::spawn_in_job`] apply.
    pub program: PathBuf,
    /// The arguments after the program name.
    pub args: Vec<String>,
}

/// Maps a harness name to the launch that starts it. `None` refuses the harness.
pub type Resolver = fn(&str) -> Option<Launch>;

/// The Windows backend. Each session is one child on its own terminal, in its own job.
#[derive(Debug)]
pub struct WindowsBackend {
    resolve: Resolver,
    sessions: BTreeMap<String, Session>,
}

impl WindowsBackend {
    /// Creates a backend that starts each harness through `resolve`.
    pub fn new(resolve: Resolver) -> Self {
        Self {
            resolve,
            sessions: BTreeMap::new(),
        }
    }

    fn session(&self, handle: &str) -> Result<&Session, OpError> {
        self.sessions
            .get(handle)
            .ok_or_else(|| OpError::Failed(format!("no session `{handle}`")))
    }

    fn session_mut(&mut self, handle: &str) -> Result<&mut Session, OpError> {
        self.sessions
            .get_mut(handle)
            .ok_or_else(|| OpError::Failed(format!("no session `{handle}`")))
    }
}

impl Drop for WindowsBackend {
    fn drop(&mut self) {
        // Closing a job handle does not end its tree here, so every job is terminated
        // explicitly. The result is ignored because a drop cannot report it.
        for session in self.sessions.values() {
            let _ = session.job().terminate(CLEANUP_EXIT);
        }
    }
}

impl SessionBackend for WindowsBackend {
    fn name(&self) -> &'static str {
        "windows-conpty"
    }

    fn execution(&self) -> Execution {
        Execution::Native
    }

    fn spawn(&mut self, harness: &str) -> Result<String, OpError> {
        let launch = (self.resolve)(harness).ok_or_else(|| {
            OpError::Unsupported(format!(
                "the windows-conpty backend has no launch for the `{harness}` harness"
            ))
        })?;
        let args: Vec<&str> = launch.args.iter().map(String::as_str).collect();
        let session =
            Session::spawn(&launch.program, &args, negotiated_size()).map_err(|error| {
                OpError::Failed(format!("cannot start the `{harness}` harness: {error}"))
            })?;
        let handle = format!("windows-conpty-{}", session.child().id());
        if self.sessions.contains_key(&handle) {
            let _ = session.job().terminate(CLEANUP_EXIT);
            return Err(OpError::Failed(format!(
                "session `{handle}` already exists"
            )));
        }
        self.sessions.insert(handle.clone(), session);
        Ok(handle)
    }

    /// Returns the retained terminal stream as normalized text: UTF-8 with LF line ends and no
    /// terminal control sequences.
    ///
    /// This is the raw stream, not a rendered screen. Output past the session's capture limit
    /// is dropped from the front, and that drop is not reported. A reader that failed is an
    /// error, so a dead stream is never mistaken for quiet output. A stream that ended
    /// normally, as it does when the child exits, is still readable.
    fn capture(&self, handle: &str) -> Result<String, OpError> {
        let capture = self.session(handle)?.capture().map_err(|error| {
            OpError::Failed(format!("cannot read the terminal output: {error}"))
        })?;
        if let Some(reason) = capture.read_error {
            return Err(OpError::Failed(format!(
                "the terminal output stream failed: {reason}"
            )));
        }
        Ok(capture.text())
    }

    fn send_text(&mut self, handle: &str, text: &str) -> Result<(), OpError> {
        self.session_mut(handle)?
            .send_text(text)
            .map_err(|error| OpError::Failed(format!("cannot write to the terminal: {error}")))
    }

    /// Interrupts with [`escalate`]: Ctrl+C on the terminal, then job termination if the child
    /// has not stopped within [`INTERRUPT_WAIT`]. Both outcomes return `Ok`, so a caller reads
    /// the result from [`SessionBackend::agent_state`]. Under this backend an interrupt is a
    /// stop-or-kill, which [`SessionBackend::interrupt_semantics`] declares. The stop is the
    /// landed DPES-886 AC4 path, not a turn stop, so the Escape rung is not sent here.
    fn interrupt(&mut self, handle: &str) -> Result<(), OpError> {
        escalate(self.session_mut(handle)?, INTERRUPT_WAIT)
            .map(|_| ())
            .map_err(|error| OpError::Failed(format!("cannot interrupt the session: {error}")))
    }

    /// Stop-or-kill: the worker is stopped within [`INTERRUPT_WAIT`], and its job is terminated
    /// if it has not stopped. A session that is `Missing` afterward is the expected result.
    fn interrupt_semantics(&self) -> InterruptSemantics {
        InterruptSemantics::StopOrKill
    }

    /// Reports `Running` while the child is alive, and `Missing` only when the child has
    /// exited and its job holds no process.
    ///
    /// The job is part of the answer, because [`SessionBackend::interrupt`] and
    /// [`SessionBackend::cleanup`] both account for the whole tree. A child that has exited
    /// while a descendant still runs is an error, not `Missing`, since that descendant may
    /// still write to the worktree. The backend never reports `Idle`, because it has no
    /// detector for that state yet. An inspection error is never reported as `Missing`.
    fn agent_state(&self, handle: &str) -> Result<AgentState, OpError> {
        let session = self.session(handle)?;
        let exited = session
            .child()
            .wait_timeout(Duration::ZERO)
            .map_err(|error| OpError::Failed(format!("cannot read the child status: {error}")))?
            .is_some();
        if !exited {
            return Ok(AgentState::Running);
        }
        match session.job().active_processes() {
            Ok(0) => Ok(AgentState::Missing),
            Ok(members) => Err(OpError::Failed(format!(
                "the child exited but its job still holds {members} live processes, so the state is unknown until cleanup"
            ))),
            Err(error) => Err(OpError::Failed(format!(
                "cannot read the job accounting: {error}"
            ))),
        }
    }

    /// Reattaches to a session this backend holds.
    ///
    /// A session is held only by the process that spawned it. Reattaching after a
    /// controller restart needs the durable identity, which is a later slice, so an
    /// unknown handle is an error.
    fn reconnect(&self, handle: &str) -> Result<(), OpError> {
        self.session(handle).map(|_| ())
    }

    /// Terminates the job, then waits until the child has exited and the job is empty.
    ///
    /// The session is released only after both are confirmed. If either is not confirmed,
    /// the session stays held so that its processes are still accounted for.
    fn cleanup(&mut self, handle: &str) -> Result<(), OpError> {
        let session = self.session(handle)?;
        session
            .job()
            .terminate(CLEANUP_EXIT)
            .map_err(|error| OpError::Failed(format!("cannot terminate the job: {error}")))?;
        let deadline = Instant::now() + CLEANUP_WAIT;
        loop {
            let exited = session
                .child()
                .wait_timeout(Duration::ZERO)
                .map_err(|error| OpError::Failed(format!("cannot read the child status: {error}")))?
                .is_some();
            let members = session.job().active_processes().map_err(|error| {
                OpError::Failed(format!("cannot read the job accounting: {error}"))
            })?;
            if exited && members == 0 {
                break;
            }
            if Instant::now() >= deadline {
                return Err(OpError::Failed(format!(
                    "after termination the child exited: {exited}; the job held {members} live processes; the session is kept"
                )));
            }
            thread::sleep(CLEANUP_POLL);
        }
        self.sessions.remove(handle);
        Ok(())
    }
}
