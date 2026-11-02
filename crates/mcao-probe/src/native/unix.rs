//! Unix process backend: the stub harness over pipes.
//!
//! This is a process and pipe backend, not a terminal one. It proves that the probe can start a real
//! child, read its output, feed it input, send it the Escape byte and tear it down. It does not
//! prove Escape behaviour through a pseudo-terminal, which is a later slice.

use std::cell::RefCell;
use std::env;
use std::io::{Read, Write};
use std::process::{Child, ChildStdin, Command, Stdio};
use std::sync::{Arc, Mutex};
use std::thread::{self, JoinHandle};

use mcao_core::{AgentState, Execution, OpError, SessionBackend};

use super::{RECONNECT_UNSUPPORTED, WAIT_INTERVAL, WAIT_POLLS};
use crate::args::Scenario;
use crate::stub::{self, ESCAPE, Options};

struct Session {
    handle: String,
    child: RefCell<Child>,
    stdin: ChildStdin,
    output: Arc<Mutex<String>>,
    reader: Option<JoinHandle<()>>,
}

impl Session {
    fn output(&self) -> String {
        self.output.lock().expect("output buffer poisoned").clone()
    }

    /// Kills the child, reaps it, and joins the reader. Safe to call on an exited child.
    fn stop(mut self) -> Result<(), OpError> {
        let child = self.child.get_mut();
        // An already exited child makes kill fail; wait still reaps it.
        let _ = child.kill();
        child
            .wait()
            .map_err(|error| OpError::Failed(format!("could not reap the child: {error}")))?;
        if let Some(reader) = self.reader.take() {
            let _ = reader.join();
        }
        Ok(())
    }
}

pub struct UnixPipeBackend {
    options: Options,
    session: Option<Session>,
}

impl UnixPipeBackend {
    pub fn new(scenario: Scenario) -> Self {
        Self {
            options: Options {
                ignore_escape: scenario == Scenario::Uncooperative,
                ..Options::default()
            },
            session: None,
        }
    }

    fn session(&self, handle: &str) -> Result<&Session, OpError> {
        self.session
            .as_ref()
            .filter(|session| session.handle == handle)
            .ok_or_else(|| OpError::Failed(format!("no session `{handle}`")))
    }
}

impl Drop for UnixPipeBackend {
    fn drop(&mut self) {
        if let Some(session) = self.session.take() {
            let _ = session.stop();
        }
    }
}

impl SessionBackend for UnixPipeBackend {
    fn name(&self) -> &'static str {
        "unix-pipe"
    }

    fn execution(&self) -> Execution {
        Execution::Native
    }

    fn spawn(&mut self, harness: &str) -> Result<String, OpError> {
        if harness != stub::HARNESS {
            return Err(OpError::Unsupported(format!(
                "the unix-pipe backend runs only the `{}` harness",
                stub::HARNESS
            )));
        }
        if self.session.is_some() {
            return Err(OpError::Failed("a session is already running".to_owned()));
        }
        let exe = env::current_exe()
            .map_err(|error| OpError::Failed(format!("cannot locate the probe binary: {error}")))?;
        let mut child = Command::new(exe)
            .arg(stub::SUBCOMMAND)
            .args(self.options.to_args())
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .spawn()
            .map_err(|error| OpError::Failed(format!("cannot start the stub harness: {error}")))?;
        let (Some(stdin), Some(mut stdout)) = (child.stdin.take(), child.stdout.take()) else {
            let _ = child.kill();
            let _ = child.wait();
            return Err(OpError::Failed("child pipes were not created".to_owned()));
        };

        let output = Arc::new(Mutex::new(String::new()));
        let sink = Arc::clone(&output);
        let reader = thread::spawn(move || {
            let mut chunk = [0_u8; 1024];
            while let Ok(read) = stdout.read(&mut chunk) {
                if read == 0 {
                    break;
                }
                sink.lock()
                    .expect("output buffer poisoned")
                    .push_str(&String::from_utf8_lossy(&chunk[..read]));
            }
        });

        let session = Session {
            handle: format!("unix-pipe-{}", child.id()),
            child: RefCell::new(child),
            stdin,
            output,
            reader: Some(reader),
        };
        for _ in 0..WAIT_POLLS {
            if session.output().contains(stub::READY) {
                let handle = session.handle.clone();
                self.session = Some(session);
                return Ok(handle);
            }
            thread::sleep(WAIT_INTERVAL);
        }
        let _ = session.stop();
        Err(OpError::Failed(format!(
            "the stub harness did not print `{}` within the bounded wait",
            stub::READY
        )))
    }

    fn capture(&self, handle: &str) -> Result<String, OpError> {
        Ok(self.session(handle)?.output())
    }

    fn send_text(&mut self, handle: &str, text: &str) -> Result<(), OpError> {
        self.session(handle)?;
        let session = self.session.as_mut().expect("session checked above");
        writeln!(session.stdin, "{text}")
            .and_then(|()| session.stdin.flush())
            .map_err(|error| OpError::Failed(format!("write to the child failed: {error}")))
    }

    fn interrupt(&mut self, handle: &str) -> Result<(), OpError> {
        self.session(handle)?;
        let session = self.session.as_mut().expect("session checked above");
        session
            .stdin
            .write_all(&[ESCAPE])
            .and_then(|()| session.stdin.flush())
            .map_err(|error| OpError::Failed(format!("Escape was not delivered: {error}")))
    }

    fn agent_state(&self, handle: &str) -> Result<AgentState, OpError> {
        let session = self.session(handle)?;
        let exited =
            session.child.borrow_mut().try_wait().map_err(|error| {
                OpError::Failed(format!("cannot read the child status: {error}"))
            })?;
        if exited.is_some() {
            return Ok(AgentState::Missing);
        }
        let output = session.output();
        match output
            .lines()
            .rev()
            .find(|line| line.starts_with("state: "))
        {
            Some(line) if line == stub::STATE_RUNNING => Ok(AgentState::Running),
            Some(line) if line == stub::STATE_IDLE => Ok(AgentState::Idle),
            _ => Err(OpError::Failed(
                "the child is alive but has reported no state".to_owned(),
            )),
        }
    }

    fn reconnect(&self, handle: &str) -> Result<(), OpError> {
        self.session(handle)?;
        Err(OpError::Unsupported(RECONNECT_UNSUPPORTED.to_owned()))
    }

    fn cleanup(&mut self, handle: &str) -> Result<(), OpError> {
        self.session(handle)?;
        let session = self.session.take().expect("session checked above");
        session.stop()
    }
}
