//! DPES-866 first-slice tests for `mcao_core::resources`.
//!
//! Every sampler here is a fake (`fake_*`). Their readings prove the evaluation logic only and
//! are never native host evidence. No test reads real host metrics or real fleet state.

use std::cell::{Cell, RefCell};
use std::path::{Path, PathBuf};
use std::time::Duration;

use mcao_core::resources::SnapshotView;
use mcao_core::resources::{
    CPU_WINDOW, DiskStats, Level, LoadedThresholds, MemoryStats, ProcessRss, Reading, ResourceKind,
    ResourceSampler, ResourceSnapshot, SampleError, State, Thresholds, evaluate, load_thresholds,
    parse_thresholds, sample,
};
use serde_json::json;

const SAMPLED_AT: &str = "2026-10-20T20:15:03Z";
const PLATFORM: &str = "fake-test";

struct FakeSampler {
    memory: Result<MemoryStats, SampleError>,
    cpu: Result<f64, SampleError>,
    disk: Result<DiskStats, SampleError>,
    memory_calls: Cell<u32>,
    cpu_calls: Cell<u32>,
    cpu_window: Cell<Option<Duration>>,
    disk_paths: RefCell<Vec<PathBuf>>,
}

impl FakeSampler {
    fn with(
        memory: Result<MemoryStats, SampleError>,
        cpu: Result<f64, SampleError>,
        disk: Result<DiskStats, SampleError>,
    ) -> Self {
        Self {
            memory,
            cpu,
            disk,
            memory_calls: Cell::new(0),
            cpu_calls: Cell::new(0),
            cpu_window: Cell::new(None),
            disk_paths: RefCell::new(Vec::new()),
        }
    }

    /// Memory 20 percent used, CPU 12.4, disk 41.0 percent free: every enabled check passes.
    fn healthy() -> Self {
        Self::with(
            Ok(MemoryStats {
                total: 1000,
                available: 800,
                swap_total: 1000,
                swap_used: 100,
            }),
            Ok(12.4),
            Ok(DiskStats {
                total: 1000,
                available: 410,
            }),
        )
    }
}

impl ResourceSampler for FakeSampler {
    fn memory(&self) -> Result<MemoryStats, SampleError> {
        self.memory_calls.set(self.memory_calls.get() + 1);
        self.memory.clone()
    }

    fn cpu_used_pct(&self, window: Duration) -> Result<f64, SampleError> {
        self.cpu_calls.set(self.cpu_calls.get() + 1);
        self.cpu_window.set(Some(window));
        self.cpu.clone()
    }

    fn disk(&self, path: &Path) -> Result<DiskStats, SampleError> {
        self.disk_paths.borrow_mut().push(path.to_path_buf());
        self.disk.clone()
    }

    fn top_processes(&self, _n: usize) -> Result<Vec<ProcessRss>, SampleError> {
        Ok(Vec::new())
    }
}

fn as_loaded(thresholds: Thresholds) -> LoadedThresholds {
    LoadedThresholds {
        thresholds,
        source: "test".to_string(),
        warning: None,
    }
}

fn run_fake(sampler: &FakeSampler, thresholds: &Thresholds) -> mcao_core::resources::Evaluation {
    let snapshot = sample(
        sampler,
        thresholds,
        Path::new("/"),
        SAMPLED_AT.to_string(),
        PLATFORM.to_string(),
    );
    evaluate(snapshot, &as_loaded(thresholds.clone()))
}

fn snapshot_of(readings: Vec<Reading>) -> ResourceSnapshot {
    ResourceSnapshot {
        sampled_at: SAMPLED_AT.to_string(),
        platform: PLATFORM.to_string(),
        readings,
    }
}

fn measured(kind: ResourceKind, value: f64) -> Reading {
    Reading {
        kind,
        value: Some(value),
        reason: None,
    }
}

fn level_of(eval: &mcao_core::resources::Evaluation, kind: ResourceKind) -> Level {
    eval.levels
        .iter()
        .find(|(k, _, _)| *k == kind)
        .map(|(_, level, _)| *level)
        .expect("every kind is evaluated")
}

fn reading_of(snapshot: &ResourceSnapshot, kind: ResourceKind) -> &Reading {
    snapshot
        .readings
        .iter()
        .find(|r| r.kind == kind)
        .expect("reading present")
}

fn temp_dir_for(name: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("mcao-resources-{}-{name}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).expect("create fixture dir");
    dir
}

// --- Evaluation boundaries (AC1 / AC2 semantics) -------------------------------------------

#[test]
fn boundaries_memory_swap_and_cpu_breach_at_or_above_limit() {
    let t = Thresholds::defaults();
    let below = snapshot_of(vec![
        measured(ResourceKind::MemoryUsedPct, 89.9),
        measured(ResourceKind::SwapUsedPct, 84.9),
        measured(ResourceKind::CpuUsedPct, 89.9),
        measured(ResourceKind::DiskFreePct, 50.0),
    ]);
    let eval = evaluate(below, &as_loaded(t.clone()));
    assert_eq!(level_of(&eval, ResourceKind::MemoryUsedPct), Level::Ok);
    assert_eq!(level_of(&eval, ResourceKind::SwapUsedPct), Level::Ok);
    assert_eq!(level_of(&eval, ResourceKind::CpuUsedPct), Level::Ok);
    assert!(eval.breaches.is_empty());
    assert_eq!(eval.state, State::Ok);

    let at = snapshot_of(vec![
        measured(ResourceKind::MemoryUsedPct, 90.0),
        measured(ResourceKind::SwapUsedPct, 85.0),
        measured(ResourceKind::CpuUsedPct, 90.0),
        measured(ResourceKind::DiskFreePct, 50.0),
    ]);
    let eval = evaluate(at, &as_loaded(t));
    assert_eq!(eval.breaches.len(), 3);
    assert_eq!(
        level_of(&eval, ResourceKind::MemoryUsedPct),
        Level::Breached
    );
    assert_eq!(eval.state, State::Breached);
}

#[test]
fn boundaries_disk_free_breaches_only_strictly_below_minimum() {
    let t = Thresholds::defaults();
    let at_minimum = evaluate(
        snapshot_of(vec![measured(ResourceKind::DiskFreePct, 10.0)]),
        &as_loaded(t.clone()),
    );
    assert_eq!(level_of(&at_minimum, ResourceKind::DiskFreePct), Level::Ok);

    let below = evaluate(
        snapshot_of(vec![measured(ResourceKind::DiskFreePct, 9.9)]),
        &as_loaded(t),
    );
    assert_eq!(level_of(&below, ResourceKind::DiskFreePct), Level::Breached);
    assert_eq!(below.breaches[0].kind, ResourceKind::DiskFreePct);
    assert_eq!(below.breaches[0].limit, 10.0);
}

#[test]
fn rounding_to_one_decimal_happens_before_comparison() {
    // 8996 of 10000 used is 89.96 percent, which rounds to 90.0 and breaches.
    let sampler = FakeSampler::with(
        Ok(MemoryStats {
            total: 10_000,
            available: 1_004,
            swap_total: 0,
            swap_used: 0,
        }),
        Ok(0.0),
        Ok(DiskStats {
            total: 1,
            available: 1,
        }),
    );
    let eval = run_fake(&sampler, &Thresholds::defaults());
    assert_eq!(
        level_of(&eval, ResourceKind::MemoryUsedPct),
        Level::Breached
    );
    assert_eq!(eval.breaches[0].value, 90.0);
}

#[test]
fn fake_sampler_healthy_host_is_state_ok_with_no_breaches() {
    let sampler = FakeSampler::healthy();
    let eval = run_fake(&sampler, &Thresholds::defaults());
    assert_eq!(eval.state, State::Ok);
    assert!(eval.breaches.is_empty());
    assert_eq!(
        level_of(&eval, ResourceKind::RunningAgents),
        Level::Disabled
    );
    assert_eq!(eval.snapshot.platform, PLATFORM);
    assert_eq!(eval.snapshot.sampled_at, SAMPLED_AT);
}

#[test]
fn fake_sampler_memory_over_limit_breaches_with_exact_values() {
    let sampler = FakeSampler::with(
        Ok(MemoryStats {
            total: 1000,
            available: 100,
            swap_total: 1000,
            swap_used: 0,
        }),
        Ok(1.0),
        Ok(DiskStats {
            total: 1000,
            available: 500,
        }),
    );
    let eval = run_fake(&sampler, &Thresholds::defaults());
    assert_eq!(eval.state, State::Breached);
    assert_eq!(eval.breaches.len(), 1);
    assert_eq!(eval.breaches[0].kind, ResourceKind::MemoryUsedPct);
    assert_eq!(eval.breaches[0].value, 90.0);
    assert_eq!(eval.breaches[0].limit, 90.0);
}

// --- Unknown and unmeasurable readings (AC3 semantics) --------------------------------------

#[test]
fn fake_sampler_unreadable_memory_leaves_memory_and_swap_unknown() {
    // `memory()` supplies both memory and swap, so one failed read makes both unknown.
    let sampler = FakeSampler::with(
        Err(SampleError::new("memory counters unreadable")),
        Ok(12.4),
        Ok(DiskStats {
            total: 1000,
            available: 410,
        }),
    );
    let thresholds = Thresholds::defaults();
    let snapshot = sample(
        &sampler,
        &thresholds,
        Path::new("/"),
        SAMPLED_AT.to_string(),
        PLATFORM.to_string(),
    );
    for kind in [ResourceKind::MemoryUsedPct, ResourceKind::SwapUsedPct] {
        let reading = reading_of(&snapshot, kind);
        assert_eq!(reading.value, None, "{kind:?}");
        assert_eq!(
            reading.reason.as_deref(),
            Some("memory counters unreadable"),
            "{kind:?}"
        );
    }

    let eval = evaluate(snapshot, &as_loaded(thresholds));
    assert_eq!(level_of(&eval, ResourceKind::MemoryUsedPct), Level::Unknown);
    assert_eq!(level_of(&eval, ResourceKind::SwapUsedPct), Level::Unknown);
    assert!(eval.breaches.is_empty());
    assert_eq!(eval.state, State::Unknown);
}

#[test]
fn fake_sampler_unknown_does_not_hide_a_breach_elsewhere() {
    let sampler = FakeSampler::with(
        Ok(MemoryStats {
            total: 1000,
            available: 50,
            swap_total: 1000,
            swap_used: 0,
        }),
        Err(SampleError::new("cpu counters unavailable")),
        Ok(DiskStats {
            total: 1000,
            available: 500,
        }),
    );
    let eval = run_fake(&sampler, &Thresholds::defaults());
    assert_eq!(level_of(&eval, ResourceKind::CpuUsedPct), Level::Unknown);
    assert_eq!(eval.breaches.len(), 1);
    assert_eq!(eval.state, State::Breached);
}

#[test]
fn fake_sampler_zero_totals_do_not_divide_or_panic() {
    let sampler = FakeSampler::with(
        Ok(MemoryStats {
            total: 0,
            available: 0,
            swap_total: 0,
            swap_used: 0,
        }),
        Ok(1.0),
        Ok(DiskStats {
            total: 0,
            available: 0,
        }),
    );
    let thresholds = Thresholds::defaults();
    let snapshot = sample(
        &sampler,
        &thresholds,
        Path::new("/"),
        SAMPLED_AT.to_string(),
        PLATFORM.to_string(),
    );
    let memory = reading_of(&snapshot, ResourceKind::MemoryUsedPct);
    assert_eq!(memory.value, None);
    assert!(memory.reason.is_some());
    let disk = reading_of(&snapshot, ResourceKind::DiskFreePct);
    assert_eq!(disk.value, None);
    assert!(disk.reason.is_some());
    // Swap with no swap configured is 0 percent used, not unknown.
    assert_eq!(
        reading_of(&snapshot, ResourceKind::SwapUsedPct).value,
        Some(0.0)
    );

    let eval = evaluate(snapshot, &as_loaded(thresholds));
    assert_eq!(eval.state, State::Unknown);
    assert!(eval.breaches.is_empty());
}

#[test]
fn fake_sampler_inconsistent_counts_are_unknown() {
    let sampler = FakeSampler::with(
        Ok(MemoryStats {
            total: 100,
            available: 200,
            swap_total: 10,
            swap_used: 20,
        }),
        Ok(1.0),
        Ok(DiskStats {
            total: 100,
            available: 50,
        }),
    );
    let eval = run_fake(&sampler, &Thresholds::defaults());
    assert_eq!(level_of(&eval, ResourceKind::MemoryUsedPct), Level::Unknown);
    assert_eq!(level_of(&eval, ResourceKind::SwapUsedPct), Level::Unknown);
    assert!(eval.breaches.is_empty());
}

#[test]
fn fake_sampler_non_finite_cpu_is_unknown_not_breach() {
    let sampler = FakeSampler::with(
        Ok(MemoryStats {
            total: 1000,
            available: 800,
            swap_total: 1000,
            swap_used: 0,
        }),
        Ok(f64::NAN),
        Ok(DiskStats {
            total: 1000,
            available: 500,
        }),
    );
    let eval = run_fake(&sampler, &Thresholds::defaults());
    assert_eq!(level_of(&eval, ResourceKind::CpuUsedPct), Level::Unknown);
    assert!(eval.breaches.is_empty());
}

#[test]
fn enabled_running_agents_without_a_count_is_unknown() {
    let mut t = Thresholds::defaults();
    t.running_agents = Some(3);
    let sampler = FakeSampler::healthy();
    let eval = run_fake(&sampler, &t);
    assert_eq!(level_of(&eval, ResourceKind::RunningAgents), Level::Unknown);
    assert!(eval.breaches.is_empty());
    assert_eq!(eval.state, State::Unknown);
}

#[test]
fn running_agents_breaches_at_limit_when_a_count_is_supplied() {
    // Evaluation semantics only: this slice never counts agents itself.
    let config = parse_thresholds("running_agents 3\n", "/cfg/resource-thresholds");
    let eval = evaluate(
        snapshot_of(vec![measured(ResourceKind::RunningAgents, 3.0)]),
        &config,
    );
    assert_eq!(
        level_of(&eval, ResourceKind::RunningAgents),
        Level::Breached
    );
    assert_eq!(eval.breaches.len(), 1);
    assert_eq!(eval.breaches[0].value, 3.0);
    assert_eq!(eval.breaches[0].limit, 3.0);
}

#[test]
fn enabled_running_agents_is_sampled_as_unavailable_with_a_reason() {
    let mut t = Thresholds::defaults();
    t.running_agents = Some(3);
    let sampler = FakeSampler::healthy();
    let snapshot = sample(
        &sampler,
        &t,
        Path::new("/"),
        SAMPLED_AT.to_string(),
        PLATFORM.to_string(),
    );
    let reading = reading_of(&snapshot, ResourceKind::RunningAgents);
    assert_eq!(reading.value, None);
    assert!(
        reading
            .reason
            .as_deref()
            .is_some_and(|r| r.contains("no running-agent count was supplied")),
        "{reading:?}"
    );
}

#[test]
fn snapshot_json_uses_the_live_v1_shape() {
    let config = parse_thresholds(
        "memory_used_pct 50\nrunning_agents 3\n",
        "/cfg/resource-thresholds",
    );
    let snapshot = snapshot_of(vec![
        measured(ResourceKind::MemoryUsedPct, 93.1),
        Reading {
            kind: ResourceKind::SwapUsedPct,
            value: None,
            reason: Some("swap size unreadable".to_string()),
        },
        measured(ResourceKind::CpuUsedPct, 12.4),
        measured(ResourceKind::DiskFreePct, 41.0),
        Reading {
            kind: ResourceKind::RunningAgents,
            value: None,
            reason: Some("no running-agent count was supplied to this sample".to_string()),
        },
    ]);
    let eval = evaluate(snapshot, &config);
    let value = serde_json::to_value(SnapshotView::from(&eval)).unwrap();

    assert_eq!(value["schema"], json!("fm-resource-snapshot.v1"));
    assert_eq!(value["sampled_at"], json!(SAMPLED_AT));
    assert_eq!(value["platform"], json!(PLATFORM));
    assert_eq!(value["state"], json!("breached"));
    assert_eq!(
        value["config"],
        json!({"source": "/cfg/resource-thresholds", "warning": null})
    );
    assert_eq!(
        value["readings"],
        json!([
            {"kind": "memory_used_pct", "value": 93.1, "limit": 50.0, "level": "breached", "reason": null},
            {"kind": "swap_used_pct", "value": null, "limit": 85.0, "level": "unknown", "reason": "swap size unreadable"},
            {"kind": "cpu_used_pct", "value": 12.4, "limit": 90.0, "level": "ok", "reason": null},
            {"kind": "disk_free_pct", "value": 41.0, "limit": 10.0, "level": "ok", "reason": null},
            {"kind": "running_agents", "value": null, "limit": 3.0, "level": "unknown", "reason": "no running-agent count was supplied to this sample"}
        ])
    );
    assert_eq!(
        value["breaches"],
        json!([{"kind": "memory_used_pct", "value": 93.1, "limit": 50.0}])
    );
}

/// Captured stdout of `fm_resource_gate_show` (bin/fm-resource-gate-lib.sh) run against the real
/// mcao with `running_agents 3` in the thresholds file and three running-agent records. Only the
/// running-agent reading and breach are compared; the other readings are host-dependent.
const SHELL_SHOW_RUNNING_AGENTS: &str =
    include_str!("fixtures/resources/shell-show-running-agents.json");

#[test]
fn running_agents_reading_and_breach_render_the_shell_show_bytes() {
    let reading =
        r#"{"kind":"running_agents","value":3.0,"limit":3.0,"level":"breached","reason":null}"#;
    let breach = r#"{"kind":"running_agents","value":3.0,"limit":3.0}"#;
    assert!(
        SHELL_SHOW_RUNNING_AGENTS.contains(reading),
        "{SHELL_SHOW_RUNNING_AGENTS}"
    );
    assert!(
        SHELL_SHOW_RUNNING_AGENTS.contains(breach),
        "{SHELL_SHOW_RUNNING_AGENTS}"
    );

    let config = parse_thresholds("running_agents 3\n", "/cfg/resource-thresholds");
    let eval = evaluate(
        snapshot_of(vec![measured(ResourceKind::RunningAgents, 3.0)]),
        &config,
    );
    let rendered = serde_json::to_string(&SnapshotView::from(&eval)).unwrap();

    assert!(rendered.contains(reading), "{rendered}");
    assert!(rendered.contains(breach), "{rendered}");
}

// --- Disk and CPU edges (AC1 / AC3 semantics) ---------------------------------------------------

fn healthy_with_disk(total: u64, available: u64) -> FakeSampler {
    let mut sampler = FakeSampler::healthy();
    sampler.disk = Ok(DiskStats { total, available });
    sampler
}

fn healthy_with_cpu(cpu: Result<f64, SampleError>) -> FakeSampler {
    let mut sampler = FakeSampler::healthy();
    sampler.cpu = cpu;
    sampler
}

fn sampled(sampler: &FakeSampler, thresholds: &Thresholds) -> ResourceSnapshot {
    sample(
        sampler,
        thresholds,
        Path::new("/"),
        SAMPLED_AT.to_string(),
        PLATFORM.to_string(),
    )
}

#[test]
fn disk_free_percentage_is_pinned_through_sample_and_evaluate() {
    // 50 of 1000 bytes free is 5.0 percent free, below the 10 percent minimum. An inverted
    // formula would read 95.0 and pass, so this test catches it.
    let thresholds = Thresholds::defaults();
    let snapshot = sampled(&healthy_with_disk(1000, 50), &thresholds);
    assert_eq!(
        reading_of(&snapshot, ResourceKind::DiskFreePct).value,
        Some(5.0)
    );
    let eval = evaluate(snapshot, &as_loaded(thresholds));
    assert_eq!(level_of(&eval, ResourceKind::DiskFreePct), Level::Breached);
    assert_eq!(eval.breaches[0].kind, ResourceKind::DiskFreePct);
    assert_eq!(eval.breaches[0].value, 5.0);
    assert_eq!(eval.breaches[0].limit, 10.0);
}

#[test]
fn disk_free_at_exactly_the_minimum_is_ok_through_sample() {
    let thresholds = Thresholds::defaults();
    let snapshot = sampled(&healthy_with_disk(1000, 100), &thresholds);
    assert_eq!(
        reading_of(&snapshot, ResourceKind::DiskFreePct).value,
        Some(10.0)
    );
    let eval = evaluate(snapshot, &as_loaded(thresholds));
    assert_eq!(level_of(&eval, ResourceKind::DiskFreePct), Level::Ok);
}

#[test]
fn disk_free_rounds_up_to_the_minimum_and_does_not_breach() {
    // 996 of 10000 bytes free is 9.96 percent, which rounds to 10.0 before the comparison.
    let thresholds = Thresholds::defaults();
    let snapshot = sampled(&healthy_with_disk(10_000, 996), &thresholds);
    assert_eq!(
        reading_of(&snapshot, ResourceKind::DiskFreePct).value,
        Some(10.0)
    );
    let eval = evaluate(snapshot, &as_loaded(thresholds));
    assert_eq!(level_of(&eval, ResourceKind::DiskFreePct), Level::Ok);
}

#[test]
fn sample_forwards_the_disk_path_to_the_sampler() {
    let path = Path::new("/var/mcao/state");
    let sampler = FakeSampler::healthy();
    let _ = sample(
        &sampler,
        &Thresholds::defaults(),
        path,
        SAMPLED_AT.to_string(),
        PLATFORM.to_string(),
    );
    assert_eq!(
        *sampler.disk_paths.borrow(),
        vec![PathBuf::from("/var/mcao/state")]
    );

    let mut off = Thresholds::defaults();
    off.disk_free_pct = None;
    let sampler = FakeSampler::healthy();
    let _ = sample(
        &sampler,
        &off,
        path,
        SAMPLED_AT.to_string(),
        PLATFORM.to_string(),
    );
    assert!(sampler.disk_paths.borrow().is_empty());
}

#[test]
fn cpu_overshoot_is_kept_and_breaches_instead_of_reading_unknown() {
    for (raw, shown) in [(100.0, 100.0), (100.04, 100.0), (150.0, 150.0)] {
        let thresholds = Thresholds::defaults();
        let snapshot = sampled(&healthy_with_cpu(Ok(raw)), &thresholds);
        assert_eq!(
            reading_of(&snapshot, ResourceKind::CpuUsedPct).value,
            Some(shown),
            "raw {raw}"
        );
        let eval = evaluate(snapshot, &as_loaded(thresholds));
        assert_eq!(
            level_of(&eval, ResourceKind::CpuUsedPct),
            Level::Breached,
            "raw {raw}"
        );
    }
}

#[test]
fn cpu_negative_reading_is_unknown_not_breach() {
    let thresholds = Thresholds::defaults();
    let snapshot = sampled(&healthy_with_cpu(Ok(-0.5)), &thresholds);
    let reading = reading_of(&snapshot, ResourceKind::CpuUsedPct);
    assert_eq!(reading.value, None);
    assert!(reading.reason.is_some());
    let eval = evaluate(snapshot, &as_loaded(thresholds));
    assert_eq!(level_of(&eval, ResourceKind::CpuUsedPct), Level::Unknown);
    assert!(eval.breaches.is_empty());
}

#[test]
fn full_memory_and_full_cpu_breach_at_the_top_of_the_range() {
    let mut sampler = FakeSampler::healthy();
    sampler.memory = Ok(MemoryStats {
        total: 1000,
        available: 0,
        swap_total: 1000,
        swap_used: 0,
    });
    sampler.cpu = Ok(100.0);
    let eval = run_fake(&sampler, &Thresholds::defaults());
    assert_eq!(
        level_of(&eval, ResourceKind::MemoryUsedPct),
        Level::Breached
    );
    assert_eq!(level_of(&eval, ResourceKind::CpuUsedPct), Level::Breached);
    assert_eq!(eval.breaches.len(), 2);
    assert_eq!(eval.breaches[0].value, 100.0);
}

#[test]
fn hand_built_non_finite_values_evaluate_unknown_not_breach() {
    for value in [f64::NAN, f64::INFINITY] {
        let snapshot = snapshot_of(vec![
            measured(ResourceKind::MemoryUsedPct, value),
            measured(ResourceKind::DiskFreePct, value),
        ]);
        let eval = evaluate(snapshot, &as_loaded(Thresholds::defaults()));
        assert_eq!(
            level_of(&eval, ResourceKind::MemoryUsedPct),
            Level::Unknown,
            "{value}"
        );
        assert_eq!(
            level_of(&eval, ResourceKind::DiskFreePct),
            Level::Unknown,
            "{value}"
        );
        assert!(eval.breaches.is_empty(), "{value}");
    }
}

// --- Sampling only what is enabled ---------------------------------------------------------

#[test]
fn fake_sampler_cpu_window_is_taken_only_when_cpu_threshold_enabled() {
    let mut off = Thresholds::defaults();
    off.cpu_used_pct = None;
    let sampler = FakeSampler::healthy();
    let eval = run_fake(&sampler, &off);
    assert_eq!(sampler.cpu_calls.get(), 0);
    assert_eq!(level_of(&eval, ResourceKind::CpuUsedPct), Level::Disabled);

    let on = Thresholds::defaults();
    let sampler = FakeSampler::healthy();
    run_fake(&sampler, &on);
    assert_eq!(sampler.cpu_calls.get(), 1);
    assert_eq!(sampler.cpu_window.get(), Some(CPU_WINDOW));
    assert_eq!(CPU_WINDOW, Duration::from_millis(250));
}

#[test]
fn fake_sampler_memory_is_not_called_when_memory_and_swap_are_off() {
    let mut t = Thresholds::defaults();
    t.memory_used_pct = None;
    t.swap_used_pct = None;
    let sampler = FakeSampler::healthy();
    let eval = run_fake(&sampler, &t);
    assert_eq!(sampler.memory_calls.get(), 0);
    assert_eq!(
        level_of(&eval, ResourceKind::MemoryUsedPct),
        Level::Disabled
    );
    assert_eq!(level_of(&eval, ResourceKind::SwapUsedPct), Level::Disabled);
}

// --- Threshold grammar (AC4 semantics) ------------------------------------------------------

#[test]
fn threshold_grammar_accepts_valid_file_with_comments_and_off() {
    let text = "# host limits\n\nmemory_used_pct off\nswap_used_pct 70.5\n  cpu_used_pct 80  \ndisk_free_pct 5\nrunning_agents 3\n";
    let loaded = parse_thresholds(text, "/cfg/resource-thresholds");
    assert_eq!(loaded.warning, None);
    assert_eq!(loaded.source, "/cfg/resource-thresholds");
    assert_eq!(loaded.thresholds.memory_used_pct, None);
    assert_eq!(loaded.thresholds.swap_used_pct, Some(70.5));
    assert_eq!(loaded.thresholds.cpu_used_pct, Some(80.0));
    assert_eq!(loaded.thresholds.disk_free_pct, Some(5.0));
    assert_eq!(loaded.thresholds.running_agents, Some(3));
}

#[test]
fn threshold_grammar_bad_line_discards_whole_file_with_file_and_line() {
    let label = "/cfg/resource-thresholds";
    // Each body follows a valid `swap_used_pct 50` line on line 1. That valid line must be
    // discarded too, so the file falls back to defaults rather than keeping swap at 50.
    // The number is the file line of the first bad line.
    let cases: [(&str, usize); 14] = [
        ("memory_used_pct", 2),
        ("memory_used_pct 90 extra", 2),
        ("memory_used_pct abc", 2),
        ("memory_used_pct 0", 2),
        ("memory_used_pct 101", 2),
        ("memory_used_pct NaN", 2),
        ("memory_used_pct inf", 2),
        ("memory_used_pct -inf", 2),
        ("memory_used_pct 90\nmemory_used_pct 80", 3),
        ("bogus_key 5", 2),
        ("running_agents 0", 2),
        ("running_agents 1000000", 2),
        ("running_agents 2.5", 2),
        ("running_agents -1", 2),
    ];
    for (body, line) in cases {
        let text = format!("swap_used_pct 50\n{body}\n");
        let loaded = parse_thresholds(&text, label);
        assert_eq!(
            loaded.thresholds,
            Thresholds::defaults(),
            "body {body:?} must fall back to defaults"
        );
        assert_eq!(loaded.source, "defaults", "body {body:?}");
        let warning = loaded
            .warning
            .unwrap_or_else(|| panic!("body {body:?} must warn"));
        let prefix = format!("{label}:{line}:");
        assert!(
            warning.contains(&prefix),
            "body {body:?}: warning {warning:?} must contain {prefix:?}"
        );
    }
}

#[test]
fn threshold_grammar_accepts_upper_bounds() {
    let loaded = parse_thresholds(
        "memory_used_pct 100\nrunning_agents 999999\n",
        "/cfg/resource-thresholds",
    );
    assert_eq!(loaded.warning, None);
    assert_eq!(loaded.thresholds.memory_used_pct, Some(100.0));
    assert_eq!(loaded.thresholds.running_agents, Some(999_999));
}

#[test]
fn running_agents_accepts_off_and_both_range_endpoints() {
    for (value, expected) in [("off", None), ("1", Some(1)), ("999999", Some(999_999))] {
        let loaded = parse_thresholds(
            &format!("running_agents {value}\n"),
            "/cfg/resource-thresholds",
        );
        assert_eq!(
            loaded.warning, None,
            "running_agents {value} must be accepted"
        );
        assert_eq!(
            loaded.thresholds.running_agents, expected,
            "running_agents {value}"
        );
    }
}

#[test]
fn threshold_warning_line_number_counts_comments_and_blank_lines() {
    let loaded = parse_thresholds(
        "# host limits\n\nmemory_used_pct 50\nbogus_key 1\n",
        "/cfg/resource-thresholds",
    );
    let warning = loaded.warning.expect("unknown key must warn");
    assert!(
        warning.starts_with("/cfg/resource-thresholds:4: "),
        "{warning}"
    );
    assert_eq!(loaded.thresholds, Thresholds::defaults());
}

#[test]
fn threshold_leading_byte_order_mark_is_not_part_of_the_first_key() {
    let loaded = parse_thresholds(
        "\u{feff}memory_used_pct 50\nswap_used_pct 70\n",
        "/cfg/resource-thresholds",
    );
    assert_eq!(loaded.warning, None);
    assert_eq!(loaded.thresholds.memory_used_pct, Some(50.0));
    assert_eq!(loaded.thresholds.swap_used_pct, Some(70.0));
}

#[test]
fn threshold_absent_file_uses_defaults_silently() {
    let dir = temp_dir_for("absent");
    let loaded = load_thresholds(&dir);
    assert_eq!(loaded.thresholds, Thresholds::defaults());
    assert_eq!(loaded.source, "defaults");
    assert_eq!(loaded.warning, None);
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn threshold_file_in_config_dir_is_loaded_and_named() {
    let dir = temp_dir_for("present");
    let path = dir.join("resource-thresholds");
    std::fs::write(&path, "memory_used_pct 75\n").expect("write fixture");
    let loaded = load_thresholds(&dir);
    assert_eq!(loaded.warning, None);
    assert_eq!(loaded.source, path.display().to_string());
    assert_eq!(loaded.thresholds.memory_used_pct, Some(75.0));
    assert_eq!(loaded.thresholds.swap_used_pct, Some(85.0));
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn threshold_unreadable_file_warns_and_uses_defaults() {
    // A directory where the file should be cannot be read as text on any platform.
    let dir = temp_dir_for("unreadable");
    std::fs::create_dir_all(dir.join("resource-thresholds")).expect("create fixture dir");
    let loaded = load_thresholds(&dir);
    assert_eq!(loaded.thresholds, Thresholds::defaults());
    assert_eq!(loaded.source, "defaults");
    let warning = loaded.warning.expect("unreadable file must warn");
    assert!(warning.contains("unreadable"), "{warning}");
    let _ = std::fs::remove_dir_all(&dir);
}

// --- Contract names and scope ---------------------------------------------------------------

#[test]
fn serialized_names_match_the_live_snapshot_contract() {
    assert_eq!(
        serde_json::to_value(ResourceKind::MemoryUsedPct).unwrap(),
        json!("memory_used_pct")
    );
    assert_eq!(
        serde_json::to_value(ResourceKind::SwapUsedPct).unwrap(),
        json!("swap_used_pct")
    );
    assert_eq!(
        serde_json::to_value(ResourceKind::CpuUsedPct).unwrap(),
        json!("cpu_used_pct")
    );
    assert_eq!(
        serde_json::to_value(ResourceKind::DiskFreePct).unwrap(),
        json!("disk_free_pct")
    );
    assert_eq!(
        serde_json::to_value(ResourceKind::RunningAgents).unwrap(),
        json!("running_agents")
    );
    assert_eq!(
        serde_json::to_value(Level::Breached).unwrap(),
        json!("breached")
    );
    assert_eq!(
        serde_json::to_value(State::Unknown).unwrap(),
        json!("unknown")
    );
}

#[test]
fn resources_module_spawns_no_processes_and_uses_no_shell() {
    // Scan every source file in the directory, so a new module cannot skip the check. The
    // known names must all be present, which catches a scan that silently reads nothing.
    let dir = Path::new(env!("CARGO_MANIFEST_DIR")).join("src/resources");
    let mut scanned = Vec::new();
    for entry in std::fs::read_dir(&dir).expect("read resources source dir") {
        let path = entry.expect("read resources dir entry").path();
        if path.extension().and_then(|ext| ext.to_str()) != Some("rs") {
            continue;
        }
        let text = std::fs::read_to_string(&path).expect("read resources source file");
        for forbidden in ["std::process", "Command::new", "thread::spawn", "sh -c"] {
            assert!(
                !text.contains(forbidden),
                "{} must not contain {forbidden:?}",
                path.display()
            );
        }
        scanned.push(
            path.file_name()
                .expect("source file has a name")
                .to_string_lossy()
                .into_owned(),
        );
    }
    for required in [
        "mod.rs",
        "sample.rs",
        "evaluate.rs",
        "snapshot.rs",
        "thresholds.rs",
    ] {
        assert!(
            scanned.iter().any(|name| name == required),
            "{required} must be scanned, got {scanned:?}"
        );
    }
}
