//! DPES-866 review attempt 2 corrections (PR 351): unreadable swap is unknown rather than zero,
//! confirmed zero swap stays zero, running-agent counts print as floats (`3.0`) as before PR 351,
//! and the running-agent reason is current. Fake samplers only; the macOS memory check lives in
//! `resources_live_host.rs`.

use std::path::Path;
use std::time::Duration;

use mcao_core::resources::{
    ActionRegistry, AdmitInputs, DiskStats, Disposition, MemoryStats, ProcessRss, Reading,
    ResourceKind, ResourceSampler, ResourceSnapshot, SampleError, SnapshotView, StartKind,
    StartRequest, State, SwapStats, admit, evaluate, parse_thresholds, sample,
};
use mcao_core::state::TaskId;

/// A fake host. `swap` of `None` takes the swap reading from the memory read, as the trait default
/// does. `Some` overrides it, so a test can make swap unreadable on its own.
struct Host {
    memory: Result<MemoryStats, SampleError>,
    swap: Option<Result<SwapStats, SampleError>>,
    cpu: Result<f64, SampleError>,
    disk: Result<DiskStats, SampleError>,
}

impl Host {
    /// Memory 50 percent used, CPU 12.4, disk 41 percent free, swap 0 of 0 bytes.
    fn healthy() -> Self {
        Self {
            memory: Ok(MemoryStats {
                total: 100,
                available: 50,
                swap_total: 0,
                swap_used: 0,
            }),
            swap: None,
            cpu: Ok(12.4),
            disk: Ok(DiskStats {
                total: 100,
                available: 41,
            }),
        }
    }
}

impl ResourceSampler for Host {
    fn memory(&self) -> Result<MemoryStats, SampleError> {
        self.memory.clone()
    }

    fn swap(&self) -> Result<SwapStats, SampleError> {
        match &self.swap {
            Some(outcome) => outcome.clone(),
            None => self.memory().map(|m| SwapStats {
                total: m.swap_total,
                used: m.swap_used,
            }),
        }
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

fn reading_of(snapshot: &ResourceSnapshot, kind: ResourceKind) -> &Reading {
    snapshot
        .readings
        .iter()
        .find(|r| r.kind == kind)
        .expect("reading is sampled")
}

fn level_of(
    eval: &mcao_core::resources::Evaluation,
    kind: ResourceKind,
) -> mcao_core::resources::Level {
    eval.levels
        .iter()
        .find(|(k, _, _)| *k == kind)
        .map(|(_, level, _)| *level)
        .expect("level is evaluated")
}

/// Admits a ship start against the default limits, with no thresholds file and no state homes.
fn admit_defaults(host: &Host) -> mcao_core::resources::Admission {
    let req = StartRequest {
        kind: StartKind::Ship,
        task: TaskId::parse("ship-1").expect("fixture task id is valid"),
    };
    let inputs = AdmitInputs {
        config_dir: std::env::temp_dir().join("mcao-resources-no-thresholds-file"),
        state_dirs: Vec::new(),
        disk_path: std::env::temp_dir(),
    };
    admit(&req, &inputs, host, &ActionRegistry::default())
}

#[test]
fn resources_unreadable_swap_is_unknown_not_zero() {
    let host = Host {
        swap: Some(Err(SampleError::new("swap counters unreadable"))),
        ..Host::healthy()
    };

    let admission = admit_defaults(&host);
    let eval = &admission.evaluation;

    assert_eq!(admission.disposition, Disposition::Admit);
    assert_eq!(eval.state, State::Unknown);
    let swap = reading_of(&eval.snapshot, ResourceKind::SwapUsedPct);
    assert_eq!(swap.value, None, "unreadable swap must not read as zero");
    assert_eq!(swap.reason.as_deref(), Some("swap counters unreadable"));
    assert_eq!(
        level_of(eval, ResourceKind::SwapUsedPct),
        mcao_core::resources::Level::Unknown
    );
    assert_eq!(
        reading_of(&eval.snapshot, ResourceKind::MemoryUsedPct).value,
        Some(50.0),
        "an unreadable swap leaves the memory reading alone"
    );
}

#[test]
fn resources_unreadable_swap_does_not_hide_a_memory_breach() {
    let host = Host {
        memory: Ok(MemoryStats {
            total: 100,
            available: 5,
            swap_total: 0,
            swap_used: 0,
        }),
        swap: Some(Err(SampleError::new("swap counters unreadable"))),
        ..Host::healthy()
    };

    let admission = admit_defaults(&host);

    assert!(matches!(admission.disposition, Disposition::Defer(_)));
    assert_eq!(admission.evaluation.state, State::Breached);
}

#[test]
fn resources_confirmed_zero_swap_reads_as_zero_percent() {
    let host = Host {
        swap: Some(Ok(SwapStats { total: 0, used: 0 })),
        ..Host::healthy()
    };

    let admission = admit_defaults(&host);
    let swap = reading_of(&admission.evaluation.snapshot, ResourceKind::SwapUsedPct);

    assert_eq!(swap.value, Some(0.0));
    assert_eq!(swap.reason, None);
    assert_eq!(
        level_of(&admission.evaluation, ResourceKind::SwapUsedPct),
        mcao_core::resources::Level::Ok
    );
}

#[test]
fn resources_default_swap_reading_is_taken_from_the_memory_read() {
    let host = Host {
        memory: Ok(MemoryStats {
            total: 100,
            available: 50,
            swap_total: 200,
            swap_used: 50,
        }),
        ..Host::healthy()
    };

    let admission = admit_defaults(&host);
    let swap = reading_of(&admission.evaluation.snapshot, ResourceKind::SwapUsedPct);

    assert_eq!(swap.value, Some(25.0));
}

#[test]
fn resources_running_agent_count_prints_as_a_float() {
    let loaded = parse_thresholds("running_agents 3\n", "/cfg/resource-thresholds");
    let snapshot = ResourceSnapshot {
        sampled_at: "2026-10-20T20:15:03Z".to_string(),
        platform: "fake-test".to_string(),
        readings: vec![Reading {
            kind: ResourceKind::RunningAgents,
            value: Some(3.0),
            reason: None,
        }],
    };
    let eval = evaluate(snapshot, &loaded);

    let rendered = serde_json::to_string(&SnapshotView::from(&eval)).expect("view serializes");

    assert!(
        rendered.contains(
            r#"{"kind":"running_agents","value":3.0,"limit":3.0,"level":"breached","reason":null}"#
        ),
        "{rendered}"
    );
    assert!(
        rendered.contains(r#""breaches":[{"kind":"running_agents","value":3.0,"limit":3.0}]"#),
        "{rendered}"
    );
}

#[test]
fn resources_absent_running_agent_count_reason_names_the_missing_count() {
    let loaded = parse_thresholds("running_agents 3\n", "/cfg/resource-thresholds");
    let snapshot = sample(
        &Host::healthy(),
        &loaded.thresholds,
        Path::new("/"),
        "2026-10-20T20:15:03Z".to_string(),
        "fake-test".to_string(),
    );

    let agents = reading_of(&snapshot, ResourceKind::RunningAgents);

    assert_eq!(agents.value, None);
    assert_eq!(
        agents.reason.as_deref(),
        Some("no running-agent count was supplied to this sample")
    );
}
