//! Simulated native-declaring backends for the driver's interrupt judgment (DPES-752 F3).
//!
//! These fakes declare `Execution::Native` so the driver takes the path a native backend would
//! take. They are simulated: they prove driver logic only, never a native lifecycle, a real
//! harness, a process, or a terminal. Their records are not native evidence.

use std::cell::{Cell, RefCell};

use mcao_core::{
    AgentState, EvidenceRecord, Execution, OpError, Operation, Outcome, SessionBackend, Support,
};
use mcao_probe::lifecycle::{STOP_MARKER, run_lifecycle};

#[derive(Clone, Copy)]
enum Escape {
    /// The Escape changes nothing.
    Ignored,
    /// A running turn stops, prints the stop marker, and the agent is idle.
    Stops,
    /// A running turn stops and the agent is idle, but no stop marker is printed.
    IdleSilently,
    /// A running turn stops with the stop marker after this many agent-state reads.
    StopsAfterReads(usize),
    /// The session disappears.
    Vanishes,
}

struct Simulated {
    runs_after_text: bool,
    escape: Escape,
    state: Cell<AgentState>,
    output: RefCell<String>,
    escapes_sent: usize,
    /// Agent-state reads made so far, so an injected failure can land on a chosen read.
    state_reads: Cell<usize>,
    /// Every agent-state read from this zero-based index on fails with the error.
    state_failure: Option<(usize, OpError)>,
    /// Capture calls made so far. Call 0 is the Capture step, call 1 counts stop markers before the
    /// Escape, and call 2 counts them after it.
    capture_calls: Cell<usize>,
    /// Every capture call from this zero-based index on fails with the error.
    capture_failure: Option<(usize, OpError)>,
    /// When set, every Escape fails with this error.
    interrupt_failure: Option<OpError>,
    /// Agent-state reads still owed before a late stop lands.
    reads_until_stop: Cell<Option<usize>>,
}

impl Simulated {
    /// Never starts a turn: the agent stays Idle and an Escape changes nothing.
    fn idle_noop() -> Self {
        Self {
            runs_after_text: false,
            ..Self::running(Escape::Ignored)
        }
    }

    /// Starts a turn on send_text, then reacts to one Escape as configured.
    fn running(escape: Escape) -> Self {
        Self {
            runs_after_text: true,
            escape,
            state: Cell::new(AgentState::Idle),
            output: RefCell::new("simulated ready\n".to_owned()),
            escapes_sent: 0,
            state_reads: Cell::new(0),
            state_failure: None,
            capture_calls: Cell::new(0),
            capture_failure: None,
            interrupt_failure: None,
            reads_until_stop: Cell::new(None),
        }
    }

    /// Fails every agent-state read from zero-based read `from` onward with `error`.
    fn failing_state_reads_from(mut self, from: usize, error: OpError) -> Self {
        self.state_failure = Some((from, error));
        self
    }

    /// Fails every capture from zero-based call `from` onward with `error`.
    fn failing_capture_from(mut self, from: usize, error: OpError) -> Self {
        self.capture_failure = Some((from, error));
        self
    }

    /// Starts with a stop marker already in the output, as an earlier interrupt would leave it.
    fn with_marker_already_printed(mut self) -> Self {
        self.output.get_mut().push_str(STOP_MARKER);
        self
    }

    /// Fails every Escape with `error`.
    fn failing_interrupt(mut self, error: OpError) -> Self {
        self.interrupt_failure = Some(error);
        self
    }

    /// Ends a running turn and prints the stop marker.
    fn stop_with_marker(&self) {
        self.stop_silently();
        self.output.borrow_mut().push_str(STOP_MARKER);
    }

    /// Ends a running turn without printing the stop marker.
    fn stop_silently(&self) {
        self.state.set(AgentState::Idle);
    }

    /// Counts one agent-state read against a pending late stop; the stop lands on the read after
    /// the last one it owes.
    fn advance_late_stop(&self) {
        match self.reads_until_stop.get() {
            Some(0) => {
                self.reads_until_stop.set(None);
                self.stop_with_marker();
            }
            Some(reads) => self.reads_until_stop.set(Some(reads - 1)),
            None => {}
        }
    }
}

/// The error a fault injected from zero-based call `from` returns on `call` and after it.
fn fault_at(fault: &Option<(usize, OpError)>, call: usize) -> Option<OpError> {
    match fault {
        Some((from, error)) if call >= *from => Some(error.clone()),
        _ => None,
    }
}

impl SessionBackend for Simulated {
    fn name(&self) -> &'static str {
        "simulated-native"
    }

    fn execution(&self) -> Execution {
        Execution::Native
    }

    fn spawn(&mut self, _harness: &str) -> Result<String, OpError> {
        Ok("simulated-1".to_owned())
    }

    fn capture(&self, _handle: &str) -> Result<String, OpError> {
        let call = self.capture_calls.get();
        self.capture_calls.set(call + 1);
        if let Some(error) = fault_at(&self.capture_failure, call) {
            return Err(error);
        }
        Ok(self.output.borrow().clone())
    }

    fn send_text(&mut self, _handle: &str, _text: &str) -> Result<(), OpError> {
        if self.runs_after_text {
            self.state.set(AgentState::Running);
        }
        Ok(())
    }

    fn interrupt(&mut self, _handle: &str) -> Result<(), OpError> {
        self.escapes_sent += 1;
        if let Some(error) = &self.interrupt_failure {
            return Err(error.clone());
        }
        match (self.escape, self.state.get()) {
            (Escape::Stops, AgentState::Running) => self.stop_with_marker(),
            (Escape::IdleSilently, AgentState::Running) => self.stop_silently(),
            (Escape::StopsAfterReads(reads), AgentState::Running) => {
                self.reads_until_stop.set(Some(reads));
            }
            (Escape::Vanishes, _) => self.state.set(AgentState::Missing),
            _ => {}
        }
        Ok(())
    }

    fn agent_state(&self, _handle: &str) -> Result<AgentState, OpError> {
        let read = self.state_reads.get();
        self.state_reads.set(read + 1);
        if let Some(error) = fault_at(&self.state_failure, read) {
            return Err(error);
        }
        self.advance_late_stop();
        Ok(self.state.get())
    }

    fn reconnect(&self, _handle: &str) -> Result<(), OpError> {
        Ok(())
    }

    fn cleanup(&mut self, _handle: &str) -> Result<(), OpError> {
        Ok(())
    }
}

fn record(records: &[EvidenceRecord], operation: Operation) -> &EvidenceRecord {
    records
        .iter()
        .find(|record| record.observation().operation == operation)
        .expect("operation recorded")
}

#[test]
fn escape_is_not_sent_to_an_agent_never_seen_running() {
    let mut backend = Simulated::idle_noop();
    let records = run_lifecycle(&mut backend, "simulated");

    assert_eq!(backend.escapes_sent, 0);
    let interrupt = record(&records, Operation::Interrupt);
    assert_eq!(
        interrupt.observation().outcome,
        Outcome::Failed {
            reason: "Escape not sent: agent was idle before it, not running".to_owned()
        }
    );
    assert_eq!(interrupt.support(), Support::Unresolved);
    assert_eq!(
        record(&records, Operation::AgentState)
            .observation()
            .output_sample
            .as_deref(),
        Some("idle")
    );
}

#[test]
fn running_agent_that_ignores_escape_is_unresolved() {
    let mut backend = Simulated::running(Escape::Ignored);
    let records = run_lifecycle(&mut backend, "simulated");

    assert_eq!(backend.escapes_sent, 1);
    let interrupt = record(&records, Operation::Interrupt);
    assert_eq!(
        interrupt.observation().outcome,
        Outcome::Failed {
            reason: "agent still running after one Escape".to_owned()
        }
    );
    assert_eq!(interrupt.support(), Support::Unresolved);
}

#[test]
fn session_missing_after_escape_is_not_reported_as_still_running() {
    let mut backend = Simulated::running(Escape::Vanishes);
    let records = run_lifecycle(&mut backend, "simulated");

    assert_eq!(backend.escapes_sent, 1);
    let interrupt = record(&records, Operation::Interrupt);
    assert_eq!(
        interrupt.observation().outcome,
        Outcome::Failed {
            reason: "agent session missing after one Escape; no stop observed".to_owned()
        }
    );
    assert_eq!(interrupt.support(), Support::Unresolved);
    assert_eq!(
        record(&records, Operation::AgentState)
            .observation()
            .output_sample
            .as_deref(),
        Some("missing")
    );
}

#[test]
fn running_agent_that_stops_on_escape_is_judged_success() {
    let mut backend = Simulated::running(Escape::Stops);
    let records = run_lifecycle(&mut backend, "simulated");

    assert_eq!(backend.escapes_sent, 1);
    assert_eq!(
        record(&records, Operation::Interrupt).observation().outcome,
        Outcome::Success
    );
}

#[test]
fn idle_without_stop_marker_is_not_judged_a_stop() {
    let mut backend = Simulated::running(Escape::IdleSilently);
    let records = run_lifecycle(&mut backend, "simulated");

    assert_eq!(backend.escapes_sent, 1);
    let interrupt = record(&records, Operation::Interrupt);
    assert_eq!(
        interrupt.observation().outcome,
        Outcome::Failed {
            reason: "agent went idle after one Escape without a stop marker".to_owned()
        }
    );
    assert_eq!(interrupt.support(), Support::Unresolved);
    assert_eq!(
        record(&records, Operation::AgentState)
            .observation()
            .output_sample
            .as_deref(),
        Some("idle")
    );
}

#[test]
fn stale_stop_marker_from_before_the_escape_is_not_counted() {
    let mut backend = Simulated::running(Escape::IdleSilently).with_marker_already_printed();
    let records = run_lifecycle(&mut backend, "simulated");

    assert_eq!(backend.escapes_sent, 1);
    let interrupt = record(&records, Operation::Interrupt);
    assert_eq!(
        interrupt.observation().outcome,
        Outcome::Failed {
            reason: "agent went idle after one Escape without a stop marker".to_owned()
        }
    );
    assert_eq!(interrupt.support(), Support::Unresolved);
}

#[test]
fn late_stop_within_the_bounded_wait_is_judged_success() {
    let mut backend = Simulated::running(Escape::StopsAfterReads(3));
    let records = run_lifecycle(&mut backend, "simulated");

    assert_eq!(backend.escapes_sent, 1);
    assert_eq!(
        record(&records, Operation::Interrupt).observation().outcome,
        Outcome::Success
    );
    assert_eq!(
        record(&records, Operation::AgentState)
            .observation()
            .output_sample
            .as_deref(),
        Some("idle")
    );
}

#[test]
fn state_read_failure_before_escape_sends_no_escape() {
    let mut backend = Simulated::running(Escape::Stops)
        .failing_state_reads_from(0, OpError::Failed("state probe failed".to_owned()));
    let records = run_lifecycle(&mut backend, "simulated");

    assert_eq!(backend.escapes_sent, 0);
    let interrupt = record(&records, Operation::Interrupt);
    assert_eq!(
        interrupt.observation().outcome,
        Outcome::Failed {
            reason: "state probe failed".to_owned()
        }
    );
    assert_eq!(interrupt.support(), Support::Unresolved);
}

#[test]
fn unsupported_state_read_before_escape_is_reported_as_unsupported() {
    let mut backend = Simulated::running(Escape::Stops)
        .failing_state_reads_from(0, OpError::Unsupported("state unavailable".to_owned()));
    let records = run_lifecycle(&mut backend, "simulated");

    assert_eq!(backend.escapes_sent, 0);
    let interrupt = record(&records, Operation::Interrupt);
    assert_eq!(
        interrupt.observation().outcome,
        Outcome::Unsupported {
            reason: "state unavailable".to_owned()
        }
    );
    assert_eq!(interrupt.support(), Support::Unsupported);
}

#[test]
fn rejected_escape_is_reported_as_the_escape_failure() {
    let mut backend = Simulated::running(Escape::Stops)
        .failing_interrupt(OpError::Failed("escape rejected".to_owned()));
    let records = run_lifecycle(&mut backend, "simulated");

    assert_eq!(backend.escapes_sent, 1);
    let interrupt = record(&records, Operation::Interrupt);
    assert_eq!(
        interrupt.observation().outcome,
        Outcome::Failed {
            reason: "escape rejected".to_owned()
        }
    );
    assert_eq!(interrupt.support(), Support::Unresolved);
    assert_eq!(
        record(&records, Operation::AgentState)
            .observation()
            .output_sample
            .as_deref(),
        Some("running")
    );
}

#[test]
fn state_read_failure_after_escape_is_reported_not_judged_a_stop() {
    let mut backend = Simulated::running(Escape::Stops)
        .failing_state_reads_from(1, OpError::Failed("state probe failed".to_owned()));
    let records = run_lifecycle(&mut backend, "simulated");

    assert_eq!(backend.escapes_sent, 1);
    let interrupt = record(&records, Operation::Interrupt);
    assert_eq!(
        interrupt.observation().outcome,
        Outcome::Failed {
            reason: "state probe failed".to_owned()
        }
    );
    assert_eq!(interrupt.support(), Support::Unresolved);
}

#[test]
fn state_read_failure_mid_settle_is_reported_not_judged_a_stop() {
    // Read 0 is the check before the Escape, read 1 is the first settle poll, and read 2 is a
    // later settle poll. The stop would land only after read 4, so a swallowed error would be
    // judged a stop.
    let mut backend = Simulated::running(Escape::StopsAfterReads(3))
        .failing_state_reads_from(2, OpError::Failed("state probe failed".to_owned()));
    let records = run_lifecycle(&mut backend, "simulated");

    assert_eq!(backend.escapes_sent, 1);
    let interrupt = record(&records, Operation::Interrupt);
    assert_eq!(
        interrupt.observation().outcome,
        Outcome::Failed {
            reason: "state probe failed".to_owned()
        }
    );
    assert_eq!(interrupt.support(), Support::Unresolved);
}

#[test]
fn capture_failure_before_escape_sends_no_escape() {
    // Capture call 0 is the Capture step, so call 1 is the stop-marker count before the Escape.
    let mut backend = Simulated::running(Escape::Stops)
        .failing_capture_from(1, OpError::Failed("capture probe failed".to_owned()));
    let records = run_lifecycle(&mut backend, "simulated");

    assert_eq!(backend.escapes_sent, 0);
    let interrupt = record(&records, Operation::Interrupt);
    assert_eq!(
        interrupt.observation().outcome,
        Outcome::Failed {
            reason: "capture probe failed".to_owned()
        }
    );
    assert_eq!(interrupt.support(), Support::Unresolved);
}

#[test]
fn capture_failure_after_escape_is_reported_not_judged_a_stop() {
    // Capture call 2 is the stop-marker count after the Escape.
    let mut backend = Simulated::running(Escape::Stops)
        .failing_capture_from(2, OpError::Failed("capture probe failed".to_owned()));
    let records = run_lifecycle(&mut backend, "simulated");

    assert_eq!(backend.escapes_sent, 1);
    let interrupt = record(&records, Operation::Interrupt);
    assert_eq!(
        interrupt.observation().outcome,
        Outcome::Failed {
            reason: "capture probe failed".to_owned()
        }
    );
    assert_eq!(interrupt.support(), Support::Unresolved);
}
