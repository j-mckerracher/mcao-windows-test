//! Smoke tests for `SysSampler` against the real host (DPES-866, slice A).
//!
//! The tests assert only that readings are well formed. No value is compared with a machine
//! figure, because every runner differs.

use mcao_core::resources::{
    CPU_WINDOW, ResourceKind, ResourceSampler, SysSampler, Thresholds, sample,
};

fn all_enabled() -> Thresholds {
    Thresholds {
        memory_used_pct: Some(90.0),
        swap_used_pct: Some(85.0),
        cpu_used_pct: Some(90.0),
        disk_free_pct: Some(10.0),
        running_agents: None,
    }
}

#[test]
fn memory_is_positive_and_consistent() {
    let memory = SysSampler.memory().expect("host memory is readable");
    assert!(memory.total > 0, "total memory must be positive");
    assert!(memory.available <= memory.total, "available exceeds total");
    assert!(
        memory.swap_used <= memory.swap_total,
        "swap used exceeds swap total"
    );
}

#[test]
fn cpu_is_a_percentage() {
    let pct = SysSampler
        .cpu_used_pct(CPU_WINDOW)
        .expect("host CPU usage is readable");
    assert!(
        (0.0..=100.0).contains(&pct),
        "cpu_used_pct {pct} is outside 0..=100"
    );
}

#[test]
fn disk_for_temp_dir_is_positive() {
    let disk = SysSampler
        .disk(&std::env::temp_dir())
        .expect("temp dir filesystem is readable");
    assert!(disk.total > 0, "filesystem total must be positive");
    assert!(disk.available <= disk.total, "available exceeds total");
}

#[test]
fn missing_disk_path_is_an_error_not_zero() {
    let missing = std::env::temp_dir().join("mcao-sys-sampler-no-such-path-7f3a9c");
    assert!(
        SysSampler.disk(&missing).is_err(),
        "an unresolvable path must be an error, never a zero reading"
    );
}

#[test]
fn sample_reads_every_enabled_resource_as_a_percentage() {
    let snapshot = sample(
        &SysSampler,
        &all_enabled(),
        &std::env::temp_dir(),
        "2000-01-01T00:00:00Z".to_owned(),
        "test-test".to_owned(),
    );
    for kind in [
        ResourceKind::MemoryUsedPct,
        ResourceKind::SwapUsedPct,
        ResourceKind::CpuUsedPct,
        ResourceKind::DiskFreePct,
    ] {
        let reading = snapshot
            .readings
            .iter()
            .find(|reading| reading.kind == kind)
            .unwrap_or_else(|| panic!("no reading for {kind:?}"));
        let value = reading
            .value
            .unwrap_or_else(|| panic!("{kind:?} is unknown: {:?}", reading.reason));
        assert!(
            (0.0..=100.0).contains(&value),
            "{kind:?} {value} is outside 0..=100"
        );
    }
}

#[test]
fn top_processes_are_bounded_and_ordered_by_rss_then_pid() {
    let processes = SysSampler
        .top_processes(5)
        .expect("process list is readable");
    assert!(!processes.is_empty(), "the host lists at least one process");
    assert!(processes.len() <= 5, "more processes than requested");
    assert!(
        processes.windows(2).all(|pair| {
            let (first, second) = (&pair[0], &pair[1]);
            first.rss_bytes > second.rss_bytes
                || (first.rss_bytes == second.rss_bytes && first.pid < second.pid)
        }),
        "processes are not ordered by resident memory, largest first, then lower pid"
    );
}

#[cfg(target_os = "linux")]
#[test]
fn linux_dev_shm_is_measured_on_its_own_tmpfs() {
    let shm = std::path::Path::new("/dev/shm");
    if !shm.is_dir() {
        return;
    }
    let disk = SysSampler
        .disk(shm)
        .expect("/dev/shm is readable as its own tmpfs mount");
    assert!(disk.total > 0, "/dev/shm total must be positive");
}
