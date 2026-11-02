//! PR 214 review nits N1 to N9 (DPES-866 slice 866.B), pinned in a file of their own.
//!
//! Every sampler here is a fake. Its readings prove the evaluation logic only and are never
//! native host evidence.

use std::cell::Cell;
use std::path::{Path, PathBuf};
use std::time::Duration;

use mcao_core::resources::{
    DiskStats, Evaluation, Level, LoadedThresholds, MemoryStats, ProcessRss, Reading, ResourceKind,
    ResourceSampler, ResourceSnapshot, SampleError, State, Thresholds, evaluate, parse_thresholds,
    sample,
};

const SAMPLED_AT: &str = "2026-10-20T20:15:03Z";
const PLATFORM: &str = "fake-test";
const CPU_REASON: &str = "CPU reading is not a finite, non-negative percentage";

/// Tokens no source file under `src/resources` may contain, at any depth. `process::` also
/// matches `std::process` and every `use std::{process::Command}` import form.
const FORBIDDEN: [&str; 4] = ["process::", "Command::new", "thread::spawn", "sh -c"];

struct Fake {
    memory: Result<MemoryStats, SampleError>,
    cpu: Result<f64, SampleError>,
    disk: Result<DiskStats, SampleError>,
    memory_calls: Cell<u32>,
}

impl Fake {
    /// Memory 20 percent used, swap 10 percent used, CPU 12.4, disk 41.0 percent free.
    fn healthy() -> Self {
        Self {
            memory: Ok(MemoryStats {
                total: 1000,
                available: 800,
                swap_total: 1000,
                swap_used: 100,
            }),
            cpu: Ok(12.4),
            disk: Ok(DiskStats {
                total: 1000,
                available: 410,
            }),
            memory_calls: Cell::new(0),
        }
    }
}

impl ResourceSampler for Fake {
    fn memory(&self) -> Result<MemoryStats, SampleError> {
        self.memory_calls.set(self.memory_calls.get() + 1);
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

fn defaults() -> Thresholds {
    Thresholds::defaults()
}

fn loaded(thresholds: Thresholds) -> LoadedThresholds {
    LoadedThresholds {
        thresholds,
        source: "defaults".to_string(),
        warning: None,
    }
}

fn sampled(fake: &Fake, thresholds: &Thresholds) -> ResourceSnapshot {
    sample(
        fake,
        thresholds,
        Path::new("/unused"),
        SAMPLED_AT.to_string(),
        PLATFORM.to_string(),
    )
}

fn snapshot_of(readings: Vec<Reading>) -> ResourceSnapshot {
    ResourceSnapshot {
        sampled_at: SAMPLED_AT.to_string(),
        platform: PLATFORM.to_string(),
        readings,
    }
}

fn reading_of(snapshot: &ResourceSnapshot, kind: ResourceKind) -> &Reading {
    snapshot
        .readings
        .iter()
        .find(|r| r.kind == kind)
        .expect("reading is present")
}

fn level_of(eval: &Evaluation, kind: ResourceKind) -> Level {
    eval.levels
        .iter()
        .find(|(k, _, _)| *k == kind)
        .expect("level is present")
        .1
}

// N2: CPU exactly 0.0 is a valid idle reading, not unknown.
#[test]
fn cpu_zero_is_a_valid_idle_reading() {
    let fake = Fake {
        cpu: Ok(0.0),
        ..Fake::healthy()
    };
    let snap = sampled(&fake, &defaults());
    let cpu = reading_of(&snap, ResourceKind::CpuUsedPct);
    assert_eq!(cpu.value, Some(0.0));
    assert_eq!(cpu.reason, None);
    let eval = evaluate(snap, &loaded(defaults()));
    assert_eq!(level_of(&eval, ResourceKind::CpuUsedPct), Level::Ok);
}

// N1: negative zero is reported as plain zero.
#[test]
fn cpu_negative_zero_is_reported_as_plain_zero() {
    let fake = Fake {
        cpu: Ok(-0.0),
        ..Fake::healthy()
    };
    let snap = sampled(&fake, &defaults());
    let value = reading_of(&snap, ResourceKind::CpuUsedPct)
        .value
        .expect("value is present");
    assert_eq!(value, 0.0);
    assert!(value.is_sign_positive(), "negative zero leaked: {value:?}");
}

// N1: non-finite values, and finite values that overflow when rounded, are unknown with a
// reason at the sample() level. They never reach evaluate() as an infinite value.
#[test]
fn cpu_values_that_cannot_be_rounded_are_unknown_with_a_reason() {
    for raw in [f64::MAX, f64::INFINITY, f64::NAN] {
        let fake = Fake {
            cpu: Ok(raw),
            ..Fake::healthy()
        };
        let snap = sampled(&fake, &defaults());
        let cpu = reading_of(&snap, ResourceKind::CpuUsedPct);
        assert_eq!(cpu.value, None, "raw {raw:?}");
        assert_eq!(cpu.reason.as_deref(), Some(CPU_REASON), "raw {raw:?}");
        let eval = evaluate(snap, &loaded(defaults()));
        assert_eq!(
            level_of(&eval, ResourceKind::CpuUsedPct),
            Level::Unknown,
            "raw {raw:?}"
        );
        assert!(eval.breaches.is_empty(), "raw {raw:?}");
        assert_eq!(eval.state, State::Unknown, "raw {raw:?}");
    }
}

// N1: rounding happens before the sign test, so a tiny negative rounds to zero and reads
// ok, while a clearly negative value stays unknown.
#[test]
fn cpu_tiny_negative_rounds_to_zero_and_clear_negative_is_unknown() {
    let tiny = Fake {
        cpu: Ok(-0.04),
        ..Fake::healthy()
    };
    let tiny_snap = sampled(&tiny, &defaults());
    assert_eq!(
        reading_of(&tiny_snap, ResourceKind::CpuUsedPct).value,
        Some(0.0)
    );

    let clear = Fake {
        cpu: Ok(-0.5),
        ..Fake::healthy()
    };
    let clear_snap = sampled(&clear, &defaults());
    let cpu = reading_of(&clear_snap, ResourceKind::CpuUsedPct);
    assert_eq!(cpu.value, None);
    assert_eq!(cpu.reason.as_deref(), Some(CPU_REASON));
}

// N3: each derived unknown reason is pinned by its exact text.
#[test]
fn derived_unknown_reasons_are_pinned_exactly() {
    let memory_total_zero = Fake {
        memory: Ok(MemoryStats {
            total: 0,
            available: 0,
            swap_total: 0,
            swap_used: 0,
        }),
        ..Fake::healthy()
    };
    let memory_available_over_total = Fake {
        memory: Ok(MemoryStats {
            total: 1000,
            available: 1100,
            swap_total: 1000,
            swap_used: 100,
        }),
        ..Fake::healthy()
    };
    let swap_over_total = Fake {
        memory: Ok(MemoryStats {
            total: 1000,
            available: 800,
            swap_total: 100,
            swap_used: 150,
        }),
        ..Fake::healthy()
    };
    let disk_size_zero = Fake {
        disk: Ok(DiskStats {
            total: 0,
            available: 0,
        }),
        ..Fake::healthy()
    };
    let disk_free_over_size = Fake {
        disk: Ok(DiskStats {
            total: 1000,
            available: 1100,
        }),
        ..Fake::healthy()
    };
    let cpu_nan = Fake {
        cpu: Ok(f64::NAN),
        ..Fake::healthy()
    };

    let memory_reason = "memory total is 0 or available exceeds total";
    let swap_reason = "swap used exceeds swap total";
    let disk_reason = "filesystem size is 0 or free space exceeds size";
    let cases: [(&Fake, ResourceKind, &str); 6] = [
        (
            &memory_total_zero,
            ResourceKind::MemoryUsedPct,
            memory_reason,
        ),
        (
            &memory_available_over_total,
            ResourceKind::MemoryUsedPct,
            memory_reason,
        ),
        (&swap_over_total, ResourceKind::SwapUsedPct, swap_reason),
        (&disk_size_zero, ResourceKind::DiskFreePct, disk_reason),
        (&disk_free_over_size, ResourceKind::DiskFreePct, disk_reason),
        (&cpu_nan, ResourceKind::CpuUsedPct, CPU_REASON),
    ];
    for (fake, kind, expected) in cases {
        let snap = sampled(fake, &defaults());
        assert_eq!(
            reading_of(&snap, kind).reason.as_deref(),
            Some(expected),
            "{kind:?}"
        );
    }
}

// N4: whitespace-only lines and indented comments are ignored, so trim() is load-bearing.
#[test]
fn whitespace_only_and_indented_comment_lines_are_ignored() {
    let blank_between = parse_thresholds("memory_used_pct 50\n   \nswap_used_pct 60\n", "t");
    assert_eq!(blank_between.warning, None);
    assert_eq!(blank_between.thresholds.memory_used_pct, Some(50.0));
    assert_eq!(blank_between.thresholds.swap_used_pct, Some(60.0));

    let indented_comment = parse_thresholds("  # note\nmemory_used_pct 50\n\t# another\n", "t");
    assert_eq!(indented_comment.warning, None);
    assert_eq!(indented_comment.thresholds.memory_used_pct, Some(50.0));
}

// N5: a swap-only configuration samples swap even with memory off, and can breach.
#[test]
fn swap_only_configuration_samples_swap_and_breaches() {
    let thresholds = Thresholds {
        memory_used_pct: None,
        swap_used_pct: Some(85.0),
        cpu_used_pct: None,
        disk_free_pct: None,
        running_agents: None,
    };
    let fake = Fake {
        memory: Ok(MemoryStats {
            total: 1000,
            available: 800,
            swap_total: 1000,
            swap_used: 900,
        }),
        ..Fake::healthy()
    };
    let snap = sampled(&fake, &thresholds);
    assert_eq!(snap.readings.len(), 1, "only swap is enabled");
    assert_eq!(
        reading_of(&snap, ResourceKind::SwapUsedPct).value,
        Some(90.0)
    );
    assert_eq!(fake.memory_calls.get(), 1, "memory is read for swap");

    let eval = evaluate(snap, &loaded(thresholds.clone()));
    assert_eq!(eval.state, State::Breached);
    assert_eq!(eval.breaches.len(), 1);
    assert_eq!(eval.breaches[0].kind, ResourceKind::SwapUsedPct);
}

// N6: duplicate readings for one kind: the first one decides, in both orders.
#[test]
fn duplicate_readings_for_one_kind_the_first_one_decides() {
    let memory = |value: f64| Reading {
        kind: ResourceKind::MemoryUsedPct,
        value: Some(value),
        reason: None,
    };

    let low_first = evaluate(
        snapshot_of(vec![memory(50.0), memory(95.0)]),
        &loaded(defaults()),
    );
    assert_eq!(level_of(&low_first, ResourceKind::MemoryUsedPct), Level::Ok);
    assert!(low_first.breaches.is_empty());

    let high_first = evaluate(
        snapshot_of(vec![memory(95.0), memory(50.0)]),
        &loaded(defaults()),
    );
    assert_eq!(
        level_of(&high_first, ResourceKind::MemoryUsedPct),
        Level::Breached
    );
    assert_eq!(high_first.breaches.len(), 1);
    assert_eq!(high_first.breaches[0].value, 95.0);
}

/// Every `.rs` file under `dir`, at any depth, sorted.
fn rust_files_under(dir: &Path) -> Vec<PathBuf> {
    let mut found = Vec::new();
    let mut pending = vec![dir.to_path_buf()];
    while let Some(next) = pending.pop() {
        for entry in std::fs::read_dir(&next).expect("read source dir") {
            let path = entry.expect("read source dir entry").path();
            if path.is_dir() {
                pending.push(path);
            } else if path.extension().and_then(|ext| ext.to_str()) == Some("rs") {
                found.push(path);
            }
        }
    }
    found.sort();
    found
}

/// One line per forbidden token found in any `.rs` file under `dir`, at any depth.
fn process_violations(dir: &Path) -> Vec<String> {
    let mut violations = Vec::new();
    for path in rust_files_under(dir) {
        let text = std::fs::read_to_string(&path).expect("read source file");
        for token in FORBIDDEN {
            if text.contains(token) {
                violations.push(format!("{} contains {token:?}", path.display()));
            }
        }
    }
    violations
}

// N9: the no-process guard covers nested directories, not only the top level.
#[test]
fn resources_sources_spawn_no_processes_at_any_depth() {
    let dir = Path::new(env!("CARGO_MANIFEST_DIR")).join("src/resources");
    let files = rust_files_under(&dir);
    for required in [
        "mod.rs",
        "sample.rs",
        "evaluate.rs",
        "snapshot.rs",
        "thresholds.rs",
        "sys.rs",
        "guard.rs",
    ] {
        assert!(
            files
                .iter()
                .any(|path| path.file_name().and_then(|n| n.to_str()) == Some(required)),
            "{required} must be scanned"
        );
    }
    assert_eq!(process_violations(&dir), Vec::<String>::new());
}

// N9: the recursive scan finds a violation in a nested directory and in an import form the
// old guard missed (`use std::{process::Command}`).
#[test]
fn recursive_scan_reports_nested_and_import_forms() {
    let root = std::env::temp_dir().join(format!("mcao-resources-guard-{}", std::process::id()));
    let nested = root.join("native");
    std::fs::create_dir_all(&nested).expect("create nested fixture dir");
    std::fs::write(nested.join("probe.rs"), "use std::{process::Command};\n")
        .expect("write nested fixture");
    std::fs::write(root.join("top.rs"), "fn clean() {}\n").expect("write top fixture");

    let violations = process_violations(&root);
    let _ = std::fs::remove_dir_all(&root);

    assert_eq!(violations.len(), 1, "{violations:?}");
    assert!(violations[0].contains("probe.rs"), "{violations:?}");
}
