//! DPES-866 slice D tests: the threshold-action registry, the admission decision, and the
//! deferral that maps to exit code 75.
//!
//! Admission runs against a fake host and temporary directories. No test reads real host metrics
//! or real fleet state.

mod support;

use std::cell::RefCell;
use std::path::Path;
use std::rc::Rc;
use std::time::Duration;

use mcao_core::McaoError;
use mcao_core::resources::{
    ActionError, ActionOutcome, ActionRegistry, Admission, AdmitInputs, Breach, Deferral,
    DiskStats, Disposition, Evaluation, MemoryStats, ProcessRss, Reading, ResourceKind,
    ResourceSampler, SampleError, StartKind, StartRequest, State, ThresholdAction, admit,
};
use mcao_core::state::TaskId;
use support::{TempDir, write};

const DEFAULT_MEMORY_MESSAGE: &str = "deferred: host resources are past a threshold (memory_used_pct 90.0 >= 90.0); task ship-1 was not launched and its backlog item stays queued - dispatch it again once usage is back within the thresholds (docs/configuration.md \"Host resource gate\")";

/// A fake host. Each reading is either a value or the error that stops it from being read.
struct Host {
    memory: Result<MemoryStats, SampleError>,
    cpu: Result<f64, SampleError>,
    disk: Result<DiskStats, SampleError>,
}

impl Host {
    /// Every reading comfortably inside the default limits.
    fn healthy() -> Self {
        Self {
            memory: Ok(MemoryStats {
                total: 100,
                available: 50,
                swap_total: 100,
                swap_used: 0,
            }),
            cpu: Ok(12.4),
            disk: Ok(DiskStats {
                total: 100,
                available: 41,
            }),
        }
    }

    /// Memory used at exactly its default limit of 90 percent.
    fn memory_at_limit() -> Self {
        Self {
            memory: Ok(MemoryStats {
                total: 100,
                available: 10,
                swap_total: 100,
                swap_used: 0,
            }),
            ..Self::healthy()
        }
    }
}

impl ResourceSampler for Host {
    fn memory(&self) -> Result<MemoryStats, SampleError> {
        self.memory.clone()
    }

    fn cpu_used_pct(&self, _window: Duration) -> Result<f64, SampleError> {
        self.cpu.clone()
    }

    fn disk(&self, _path: &Path) -> Result<DiskStats, SampleError> {
        self.disk.clone()
    }

    fn top_processes(&self, _n: usize) -> Result<Vec<ProcessRss>, SampleError> {
        Ok(Vec::new())
    }
}

/// One call a probe action received.
#[derive(Debug, PartialEq)]
struct Call {
    action: &'static str,
    task: String,
    breaches: Vec<Breach>,
}

type Calls = Rc<RefCell<Vec<Call>>>;
type Outcome = fn() -> Result<ActionOutcome, ActionError>;

/// A test action that records each call and then returns the outcome it was built with.
struct Probe {
    name: &'static str,
    calls: Calls,
    outcome: Outcome,
}

impl ThresholdAction for Probe {
    fn name(&self) -> &'static str {
        self.name
    }

    fn run(&self, req: &StartRequest, eval: &Evaluation) -> Result<ActionOutcome, ActionError> {
        self.calls.borrow_mut().push(Call {
            action: self.name,
            task: req.task.as_str().to_owned(),
            breaches: eval.breaches.clone(),
        });
        (self.outcome)()
    }
}

fn probe(name: &'static str, calls: &Calls, outcome: Outcome) -> Box<dyn ThresholdAction> {
    Box::new(Probe {
        name,
        calls: Rc::clone(calls),
        outcome,
    })
}

fn proceed() -> Result<ActionOutcome, ActionError> {
    Ok(ActionOutcome::Continue)
}

fn refuse() -> Result<ActionOutcome, ActionError> {
    Ok(ActionOutcome::Defer(Deferral {
        breaches: Vec::new(),
        message: "probe refused".to_owned(),
    }))
}

fn fail() -> Result<ActionOutcome, ActionError> {
    Err(ActionError {
        reason: "probe broke".to_owned(),
    })
}

fn start(task: &str) -> StartRequest {
    StartRequest {
        kind: StartKind::Ship,
        task: TaskId::parse(task).expect("fixture task id is valid"),
    }
}

fn inputs(config: &Path, state_dirs: &[&Path]) -> AdmitInputs {
    AdmitInputs {
        config_dir: config.to_path_buf(),
        state_dirs: state_dirs.iter().map(|dir| dir.to_path_buf()).collect(),
        disk_path: config.to_path_buf(),
    }
}

/// Admits `ship-1` with no thresholds file and no state records, so the built-in defaults apply.
fn admit_with_defaults(host: &Host, registry: &ActionRegistry) -> Admission {
    let config = TempDir::new("admit-config");
    let state = TempDir::new("admit-state");
    admit(
        &start("ship-1"),
        &inputs(config.path(), &[state.path()]),
        host,
        registry,
    )
}

fn memory_breach() -> Breach {
    Breach {
        kind: ResourceKind::MemoryUsedPct,
        value: 90.0,
        limit: 90.0,
    }
}

#[test]
fn default_memory_limit_defers_the_start_with_its_message() {
    let admission = admit_with_defaults(&Host::memory_at_limit(), &ActionRegistry::default());

    assert_eq!(
        admission.disposition,
        Disposition::Defer(Deferral {
            breaches: vec![memory_breach()],
            message: DEFAULT_MEMORY_MESSAGE.to_owned(),
        })
    );
}

#[test]
fn deferral_maps_to_exit_code_75_and_shows_its_message() {
    let Disposition::Defer(deferral) =
        admit_with_defaults(&Host::memory_at_limit(), &ActionRegistry::default()).disposition
    else {
        panic!("memory at its limit must defer");
    };
    let error = McaoError::Deferred(deferral);

    assert_eq!(error.exit_code(), 75);
    assert_eq!(error.to_string(), DEFAULT_MEMORY_MESSAGE);
}

#[test]
fn healthy_host_admits_and_runs_no_action() {
    let calls = Calls::default();
    let mut registry = ActionRegistry::default();
    registry.register(probe("probe", &calls, proceed));

    let admission = admit_with_defaults(&Host::healthy(), &registry);

    assert_eq!(admission.disposition, Disposition::Admit);
    assert_eq!(admission.evaluation.state, State::Ok);
    assert!(calls.borrow().is_empty());
}

#[test]
fn breach_runs_each_action_once_with_the_request_and_evaluation() {
    let calls = Calls::default();
    let mut registry = ActionRegistry::default();
    registry.register(probe("probe", &calls, proceed));

    let admission = admit_with_defaults(&Host::memory_at_limit(), &registry);

    assert_eq!(
        *calls.borrow(),
        vec![Call {
            action: "probe",
            task: "ship-1".to_owned(),
            breaches: vec![memory_breach()],
        }]
    );
    assert_eq!(
        admission.disposition,
        Disposition::Defer(Deferral {
            breaches: vec![memory_breach()],
            message: DEFAULT_MEMORY_MESSAGE.to_owned(),
        })
    );
}

#[test]
fn first_defer_decides_and_later_actions_do_not_run() {
    let calls = Calls::default();
    let mut registry = ActionRegistry::default();
    registry.register(probe("first", &calls, proceed));
    registry.register(probe("second", &calls, refuse));
    registry.register(probe("third", &calls, proceed));

    let admission = admit_with_defaults(&Host::memory_at_limit(), &registry);

    let names: Vec<&str> = calls.borrow().iter().map(|call| call.action).collect();
    assert_eq!(names, ["first", "second"]);
    assert_eq!(
        admission.disposition,
        Disposition::Defer(Deferral {
            breaches: Vec::new(),
            message: "probe refused".to_owned(),
        })
    );
}

#[test]
fn failing_action_is_skipped_and_the_start_still_defers() {
    let calls = Calls::default();
    let mut registry = ActionRegistry::default();
    registry.register(probe("broken", &calls, fail));
    registry.register(probe("after", &calls, proceed));

    let admission = admit_with_defaults(&Host::memory_at_limit(), &registry);

    let names: Vec<&str> = calls.borrow().iter().map(|call| call.action).collect();
    assert_eq!(names, ["broken", "after"]);
    assert_eq!(
        admission.disposition,
        Disposition::Defer(Deferral {
            breaches: vec![memory_breach()],
            message: DEFAULT_MEMORY_MESSAGE.to_owned(),
        })
    );
}

#[test]
fn unreadable_memory_admits_with_unknown_state() {
    let host = Host {
        memory: Err(SampleError::new("host memory unreadable")),
        ..Host::healthy()
    };

    let admission = admit_with_defaults(&host, &ActionRegistry::default());

    assert_eq!(admission.disposition, Disposition::Admit);
    assert_eq!(admission.evaluation.state, State::Unknown);
    assert!(admission.evaluation.snapshot.readings.contains(&Reading {
        kind: ResourceKind::SwapUsedPct,
        value: None,
        reason: Some("host memory unreadable".to_owned()),
    }));
}

#[test]
fn disk_breach_message_uses_the_below_comparison() {
    let host = Host {
        disk: Ok(DiskStats {
            total: 100,
            available: 9,
        }),
        ..Host::healthy()
    };

    let admission = admit_with_defaults(&host, &ActionRegistry::default());

    let Disposition::Defer(deferral) = admission.disposition else {
        panic!("disk below its minimum must defer");
    };
    assert_eq!(
        deferral.breaches,
        vec![Breach {
            kind: ResourceKind::DiskFreePct,
            value: 9.0,
            limit: 10.0,
        }]
    );
    assert!(
        deferral.message.contains("(disk_free_pct 9.0 < 10.0)"),
        "{}",
        deferral.message
    );
}

#[test]
fn running_agent_limit_counts_held_slots_across_homes_and_defers() {
    let config = TempDir::new("admit-agents-config");
    write(
        &config.path().join("resource-thresholds"),
        "running_agents 3\ncpu_used_pct off\n",
    );
    let home = TempDir::new("admit-agents-home");
    let other = TempDir::new("admit-agents-other");
    write(
        &home.path().join("ship-a.meta"),
        "kind=ship\nproject=mcao\n",
    );
    write(
        &home.path().join("ship-b.meta"),
        "kind=ship\nproject=mcao\n",
    );
    write(
        &home.path().join("secondmate.meta"),
        "kind=secondmate\nproject=mcao\n",
    );
    write(
        &home.path().join("handed-off.meta"),
        "kind=ship\nproject=mcao\npr=https://example.test/pull/1\n",
    );
    write(
        &other.path().join("ship-c.meta"),
        "kind=ship\nproject=mcao\n",
    );

    let admission = admit(
        &start("new-ship"),
        &inputs(config.path(), &[home.path(), other.path()]),
        &Host::healthy(),
        &ActionRegistry::default(),
    );

    let Disposition::Defer(deferral) = admission.disposition else {
        panic!("three held slots at a limit of 3 must defer");
    };
    assert_eq!(
        deferral.breaches,
        vec![Breach {
            kind: ResourceKind::RunningAgents,
            value: 3.0,
            limit: 3.0,
        }]
    );
    assert!(
        deferral.message.contains("(running_agents 3 >= 3)"),
        "{}",
        deferral.message
    );
}

#[test]
fn unmeasurable_running_agent_count_admits_as_unknown() {
    let config = TempDir::new("admit-count-config");
    write(
        &config.path().join("resource-thresholds"),
        "running_agents 3\ncpu_used_pct off\n",
    );
    let missing = config.path().join("no-such-state");

    let admission = admit(
        &start("ship-1"),
        &inputs(config.path(), &[missing.as_path()]),
        &Host::healthy(),
        &ActionRegistry::default(),
    );

    assert_eq!(admission.disposition, Disposition::Admit);
    assert_eq!(admission.evaluation.state, State::Unknown);
}

#[test]
fn malformed_thresholds_file_keeps_the_defaults_and_reports_the_line() {
    let config = TempDir::new("admit-bad-config");
    write(
        &config.path().join("resource-thresholds"),
        "# limits\n\nmemory_used_pct abc\n",
    );
    let state = TempDir::new("admit-bad-state");

    let admission = admit(
        &start("ship-1"),
        &inputs(config.path(), &[state.path()]),
        &Host::memory_at_limit(),
        &ActionRegistry::default(),
    );

    assert_eq!(admission.evaluation.config_source, "defaults");
    assert!(
        admission
            .evaluation
            .config_warning
            .as_deref()
            .is_some_and(|warning| warning.contains(":3:"))
    );
    let Disposition::Defer(deferral) = admission.disposition else {
        panic!("the default memory limit must still defer");
    };
    assert_eq!(deferral.message, DEFAULT_MEMORY_MESSAGE);
}

#[test]
fn resource_keys_match_the_snapshot_contract() {
    assert_eq!(
        ResourceKind::ALL.map(ResourceKind::key),
        [
            "memory_used_pct",
            "swap_used_pct",
            "cpu_used_pct",
            "disk_free_pct",
            "running_agents",
        ]
    );
    for kind in ResourceKind::ALL {
        let serialized = serde_json::to_string(&kind).expect("a resource kind serializes");
        assert_eq!(serialized, format!("\"{}\"", kind.key()));
    }
}
