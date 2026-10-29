//! Drives the seven DPES-752 lifecycle operations, in AC1 order, against one `SessionBackend`.
//!
//! Each operation yields one `EvidenceRecord`. A failed step does not stop the run, so later
//! steps still report what they observe. Steps that need a session but have none are recorded
//! as not attempted rather than omitted.

use std::thread;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use mcao_core::{
    AgentState, EvidenceRecord, Execution, InterruptSemantics, Observation, OpError, Operation,
    Outcome, SessionBackend,
};

/// Text sent by the `send_text` step.
pub const PROBE_TEXT: &str = "mcao probe: send_text check";

/// Text a harness prints when a running turn stops after an Escape. A stop is judged only when
/// this marker grows in the captured output, so an agent that merely turns idle is not a stop.
pub const STOP_MARKER: &str = "[turn interrupted]";

/// Bounded wait for an agent state to change: `SETTLE_POLLS` reads, each after `SETTLE_INTERVAL`,
/// about two seconds in all. It covers a native agent starting a turn after `send_text`, and a
/// running agent leaving Running after one Escape.
const SETTLE_POLLS: u32 = 20;
const SETTLE_INTERVAL: Duration = Duration::from_millis(100);

const SAMPLE_CHARS: usize = 200;

const AFTER_SPAWN: [Operation; 6] = [
    Operation::Capture,
    Operation::SendText,
    Operation::Interrupt,
    Operation::AgentState,
    Operation::Reconnect,
    Operation::Cleanup,
];

pub fn run_lifecycle(backend: &mut dyn SessionBackend, harness: &str) -> Vec<EvidenceRecord> {
    let mut run = Run {
        backend: backend.name(),
        execution: backend.execution(),
        harness: harness.to_owned(),
        platform: format!("{}-{}", std::env::consts::OS, std::env::consts::ARCH),
        records: Vec::with_capacity(AFTER_SPAWN.len() + 1),
    };

    let started = Instant::now();
    let spawned = backend.spawn(harness);
    let handle = spawned.as_ref().ok().cloned();
    run.push(
        Operation::Spawn,
        handle.clone(),
        started.elapsed(),
        spawned.map(|_| None),
    );

    let Some(handle) = handle else {
        for operation in AFTER_SPAWN {
            run.push(
                operation,
                None,
                Duration::ZERO,
                Err(OpError::Failed(
                    "not attempted: spawn produced no session".to_owned(),
                )),
            );
        }
        return run.records;
    };

    let started = Instant::now();
    let result = backend.capture(&handle).map(|text| Some(sample(&text)));
    run.push(
        Operation::Capture,
        Some(handle.clone()),
        started.elapsed(),
        result,
    );

    let started = Instant::now();
    let result = backend.send_text(&handle, PROBE_TEXT).map(|()| None);
    run.push(
        Operation::SendText,
        Some(handle.clone()),
        started.elapsed(),
        result,
    );

    let started = Instant::now();
    let interrupt = interrupt_step(backend, &handle).map(|()| None);
    run.push(
        Operation::Interrupt,
        Some(handle.clone()),
        started.elapsed(),
        interrupt,
    );

    let started = Instant::now();
    let state = backend.agent_state(&handle);
    run.push(
        Operation::AgentState,
        Some(handle.clone()),
        started.elapsed(),
        state.map(|observed| Some(observed.as_str().to_owned())),
    );

    let started = Instant::now();
    let result = backend.reconnect(&handle).map(|()| None);
    run.push(
        Operation::Reconnect,
        Some(handle.clone()),
        started.elapsed(),
        result,
    );

    let started = Instant::now();
    let result = backend.cleanup(&handle).map(|()| None);
    run.push(Operation::Cleanup, Some(handle), started.elapsed(), result);

    run.records
}

/// Sends one interrupt and judges it. Running must be observed before the interrupt, because a
/// native agent may need a moment after `send_text` to start its turn, and no interrupt is sent
/// to an agent never seen running.
///
/// The judgment depends on what the backend declares `interrupt` means. Under
/// [`InterruptSemantics::StopTurn`], a stop counts only when the agent left Running within the
/// bounded wait, settled Idle, and the stop marker grew in the captured output after the
/// interrupt. Under [`InterruptSemantics::StopOrKill`], the backend has already ended the worker
/// within its own bounded wait, so a session that is Missing afterward is the passing outcome.
/// Idle is judged the same way under both, and Running after the wait is a failure under both.
fn interrupt_step(backend: &mut dyn SessionBackend, handle: &str) -> Result<(), OpError> {
    let semantics = backend.interrupt_semantics();
    let before = wait_while(backend, handle, AgentState::Idle)?;
    if before != AgentState::Running {
        return Err(OpError::Failed(format!(
            "Escape not sent: agent was {} before it, not running",
            before.as_str()
        )));
    }
    let markers_before = stop_marker_count(backend, handle)?;
    backend.interrupt(handle)?;
    match wait_while(backend, handle, AgentState::Running)? {
        AgentState::Running => Err(OpError::Failed(
            "agent still running after one Escape".to_owned(),
        )),
        AgentState::Missing => match semantics {
            InterruptSemantics::StopOrKill => Ok(()),
            InterruptSemantics::StopTurn => Err(OpError::Failed(
                "agent session missing after one Escape; no stop observed".to_owned(),
            )),
        },
        AgentState::Idle => {
            if stop_marker_count(backend, handle)? > markers_before {
                Ok(())
            } else {
                Err(OpError::Failed(
                    "agent went idle after one Escape without a stop marker".to_owned(),
                ))
            }
        }
    }
}

/// Reads the agent state until it leaves `pending` or the bounded wait runs out, and returns the
/// last state read.
fn wait_while(
    backend: &dyn SessionBackend,
    handle: &str,
    pending: AgentState,
) -> Result<AgentState, OpError> {
    let mut state = backend.agent_state(handle)?;
    let mut polls = 0;
    while state == pending && polls < SETTLE_POLLS {
        thread::sleep(SETTLE_INTERVAL);
        state = backend.agent_state(handle)?;
        polls += 1;
    }
    Ok(state)
}

fn stop_marker_count(backend: &dyn SessionBackend, handle: &str) -> Result<usize, OpError> {
    Ok(backend.capture(handle)?.matches(STOP_MARKER).count())
}

struct Run {
    backend: &'static str,
    execution: Execution,
    harness: String,
    platform: String,
    records: Vec<EvidenceRecord>,
}

impl Run {
    fn push(
        &mut self,
        operation: Operation,
        handle: Option<String>,
        elapsed: Duration,
        result: Result<Option<String>, OpError>,
    ) {
        let (outcome, output_sample) = match result {
            Ok(sample) => (Outcome::Success, sample),
            Err(error) => (Outcome::from(error), None),
        };
        let observation = Observation {
            backend: self.backend.to_owned(),
            harness: self.harness.clone(),
            operation,
            execution: self.execution,
            platform: self.platform.clone(),
            outcome,
            duration_ms: u64::try_from(elapsed.as_millis()).unwrap_or(u64::MAX),
            timestamp_unix_ms: now_unix_ms(),
            handle,
            output_sample,
        };
        self.records.push(EvidenceRecord::new(observation));
    }
}

fn sample(text: &str) -> String {
    text.chars().take(SAMPLE_CHARS).collect()
}

fn now_unix_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .ok()
        .and_then(|elapsed| u64::try_from(elapsed.as_millis()).ok())
        .unwrap_or(0)
}
