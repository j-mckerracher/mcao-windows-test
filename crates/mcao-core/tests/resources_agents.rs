//! DPES-866 slice C tests: the running-agent count and the `all` sampling mode.
//!
//! Counts run against temporary state directories. Sampling runs against a fake host. No test
//! reads real host metrics or real fleet state.

mod support;

use std::fs;
use std::path::{Path, PathBuf};
use std::time::Duration;

use mcao_core::resources::{
    DiskStats, Level, LoadedThresholds, MemoryStats, ProcessRss, Reading, ResourceKind,
    ResourceSampler, ResourceSnapshot, SampleError, SampleMode, Thresholds, count_running_agents,
    evaluate, sample, sample_mode,
};
use mcao_core::state::TaskId;
use support::{TempDir, write};

const SHIP: &str = "kind=ship\nproject=mcao\n";
const SAMPLED_AT: &str = "2026-10-25T00:00:00Z";
const PLATFORM: &str = "fake-test";

fn task(raw: &str) -> TaskId {
    TaskId::parse(raw).expect("fixture task id is valid")
}

fn count(dirs: &[&Path], own: Option<&str>) -> Result<u32, SampleError> {
    let dirs: Vec<PathBuf> = dirs.iter().map(|dir| dir.to_path_buf()).collect();
    let own = own.map(task);
    count_running_agents(&dirs, own.as_ref())
}

#[test]
fn counts_only_task_records_that_hold_a_slot() {
    let home = TempDir::new("agents-slots");
    let state = home.path();
    write(&state.join("ship-a.meta"), SHIP);
    write(&state.join("ship-b.meta"), "project=mcao\nkind=ship\n");
    write(
        &state.join("ship-c.meta"),
        "kind=ship\nproject=mcao\nbackend=tmux\n",
    );
    write(
        &state.join("local-target.meta"),
        "kind=ship\nproject=mcao\nexecution_target=local\n",
    );
    write(
        &state.join("empty-pr.meta"),
        "kind=ship\nproject=mcao\npr=\n",
    );
    write(
        &state.join("secondmate.meta"),
        "kind=secondmate\nproject=mcao\n",
    );
    write(
        &state.join("handed-off.meta"),
        "kind=ship\nproject=mcao\npr=https://example.test/pull/1\n",
    );
    write(&state.join("no-project.meta"), "kind=ship\n");
    write(&state.join("empty-project.meta"), "kind=ship\nproject=\n");
    write(
        &state.join("cloud.meta"),
        "kind=ship\nproject=mcao\nexecution_target=gcp\n",
    );
    write(
        &state.join("ship-d.status"),
        "working [at=1]: not a record\n",
    );
    write(&state.join("notes.txt"), SHIP);
    fs::create_dir(state.join("dir.meta")).expect("create directory named like a record");

    let before = fs::read(state.join("ship-a.meta")).expect("read fixture");
    assert_eq!(count(&[state], None), Ok(5));
    assert_eq!(
        fs::read(state.join("ship-a.meta")).expect("read fixture"),
        before
    );
}

#[test]
fn unknown_execution_target_counts_as_a_slot_as_the_shell_does() {
    let home = TempDir::new("agents-targets");
    let state = home.path();
    write(&state.join("ship-a.meta"), SHIP);
    write(
        &state.join("odd-target.meta"),
        "kind=ship\nproject=mcao\nexecution_target=cloud\n",
    );
    write(
        &state.join("dup-target.meta"),
        "kind=ship\nproject=mcao\nexecution_target=gcp\nexecution_target=local\n",
    );

    assert_eq!(count(&[state], None), Ok(3));
}

#[test]
fn own_record_in_the_starting_home_is_not_counted() {
    let first = TempDir::new("agents-own-first");
    let second = TempDir::new("agents-own-second");
    write(&first.path().join("ship-a.meta"), SHIP);
    write(&first.path().join("ship-b.meta"), SHIP);
    write(&second.path().join("ship-a.meta"), SHIP);

    // The own record in the starting home is the one the start replaces. The same id in another
    // home is a different holder and still counts.
    assert_eq!(count(&[first.path(), second.path()], Some("ship-a")), Ok(2));
}

#[test]
fn each_home_counts_and_a_repeated_home_is_read_once() {
    let first = TempDir::new("agents-homes-first");
    let second = TempDir::new("agents-homes-second");
    write(&first.path().join("ship-a.meta"), SHIP);
    write(&second.path().join("ship-b.meta"), SHIP);
    write(&second.path().join("ship-c.meta"), SHIP);

    assert_eq!(
        count(&[first.path(), second.path(), second.path()], None),
        Ok(3)
    );
}

#[cfg(unix)]
#[test]
fn symlinked_records_and_homes_are_not_counted_twice() {
    use std::os::unix::fs::symlink;

    let home = TempDir::new("agents-links");
    let state = home.path().join("state");
    write(&state.join("ship-a.meta"), SHIP);
    symlink(state.join("ship-a.meta"), state.join("linked.meta")).expect("link record");
    let alias = home.path().join("alias");
    symlink(&state, &alias).expect("link home");

    assert_eq!(count(&[state.as_path(), alias.as_path()], None), Ok(1));
}

#[test]
fn missing_home_is_an_error_not_zero() {
    let home = TempDir::new("agents-missing");
    let missing = home.path().join("gone");
    write(&home.path().join("ship-a.meta"), SHIP);

    let error = count(&[home.path(), missing.as_path()], None)
        .expect_err("a missing home must not count as zero");
    assert!(
        error.reason.contains(&missing.display().to_string()),
        "reason names the missing home: {}",
        error.reason
    );
}

#[test]
fn no_home_supplied_is_an_error() {
    assert_eq!(
        count_running_agents(&[], None),
        Err(SampleError::new("no state directory was supplied"))
    );
}

#[test]
fn non_utf8_record_is_read_as_the_shell_reads_it() {
    // The shell's fm_meta_get compares bytes, so invalid UTF-8 in an unrelated value does not stop
    // a record from holding a slot, and invalid UTF-8 in a `pr` value still hands it off.
    let home = TempDir::new("agents-bad-bytes");
    write(&home.path().join("ship-a.meta"), SHIP);
    write(
        &home.path().join("ship-b.meta"),
        b"kind=ship\nproject=mcao\nnote=\xff\xfe\n",
    );
    write(
        &home.path().join("ship-c.meta"),
        b"kind=ship\nproject=mcao\npr=\xff\n",
    );

    assert_eq!(count(&[home.path()], None), Ok(2));
}

#[test]
fn dot_prefixed_records_are_skipped_as_the_shell_glob_skips_them() {
    let home = TempDir::new("agents-dotfiles");
    write(&home.path().join("ship-a.meta"), SHIP);
    write(&home.path().join(".hidden.meta"), SHIP);
    write(&home.path().join(".meta"), SHIP);

    assert_eq!(count(&[home.path()], None), Ok(1));
}

#[test]
fn repeated_execution_target_reads_its_last_value_as_the_shell_does() {
    // Mirrors the shell loop in fm_project_capacity_occupants, which reads each record through
    // fm_meta_get (the last value of a key) and fm_cloud_execution_target (only `gcp` is skipped).
    let home = TempDir::new("agents-last-target");
    let state = home.path();
    write(&state.join("ship-a.meta"), SHIP);
    write(
        &state.join("local-then-gcp.meta"),
        "kind=ship\nproject=mcao\nexecution_target=local\nexecution_target=gcp\n",
    );
    write(
        &state.join("gcp-then-local.meta"),
        "kind=ship\nproject=mcao\nexecution_target=gcp\nexecution_target=local\n",
    );
    write(
        &state.join("gcp-then-empty.meta"),
        "kind=ship\nproject=mcao\nexecution_target=gcp\nexecution_target=\n",
    );

    assert_eq!(count(&[state], None), Ok(3));
}

#[cfg(unix)]
#[test]
fn a_record_removed_during_the_scan_is_skipped_not_an_error() {
    use std::sync::Arc;
    use std::sync::atomic::{AtomicBool, Ordering};
    use std::thread;

    // Creates and removes a record while the count runs. A record that disappears between the
    // directory listing and its read holds no slot, so every count succeeds with the stable record
    // plus at most the churned one. Unix only: a file pending deletion can read as access denied on
    // Windows, which is a different error this test does not model.
    let home = TempDir::new("agents-churn");
    let state = home.path().to_path_buf();
    write(&state.join("ship-a.meta"), SHIP);
    let churned = state.join("churn.meta");

    let stop = Arc::new(AtomicBool::new(false));
    let worker = {
        let stop = Arc::clone(&stop);
        let churned = churned.clone();
        thread::spawn(move || {
            while !stop.load(Ordering::Relaxed) {
                let _ = fs::write(&churned, SHIP);
                let _ = fs::remove_file(&churned);
            }
        })
    };
    let outcomes: Vec<Result<u32, SampleError>> =
        (0..5000).map(|_| count(&[state.as_path()], None)).collect();
    stop.store(true, Ordering::Relaxed);
    worker.join().expect("churn thread finishes");

    for outcome in outcomes {
        let n = outcome.expect("a record removed mid-scan is skipped, not an error");
        assert!(
            (1..=2).contains(&n),
            "count {n} is the stable record plus at most one churned record"
        );
    }
}

#[test]
fn record_with_an_invalid_task_id_is_an_error() {
    let home = TempDir::new("agents-bad-id");
    write(&home.path().join("ship-a.meta"), SHIP);
    write(&home.path().join("bad name.meta"), SHIP);

    let error = count(&[home.path()], None).expect_err("invalid task id must not be skipped");
    assert!(
        error.reason.contains("bad name.meta"),
        "reason names the record: {}",
        error.reason
    );
}

/// A healthy fake host. Memory 50.0 percent used, swap 10.0, CPU 12.5, disk 50.0 percent free.
struct Host {
    memory: Result<MemoryStats, SampleError>,
    cpu: Result<f64, SampleError>,
    disk: Result<DiskStats, SampleError>,
}

impl Host {
    fn healthy() -> Self {
        Self {
            memory: Ok(MemoryStats {
                total: 100,
                available: 50,
                swap_total: 200,
                swap_used: 20,
            }),
            cpu: Ok(12.5),
            disk: Ok(DiskStats {
                total: 1000,
                available: 500,
            }),
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

fn off() -> Thresholds {
    Thresholds {
        memory_used_pct: None,
        swap_used_pct: None,
        cpu_used_pct: None,
        disk_free_pct: None,
        running_agents: None,
    }
}

fn sample_in(
    host: &Host,
    thresholds: &Thresholds,
    mode: SampleMode,
    agents: Option<Result<u32, SampleError>>,
) -> ResourceSnapshot {
    sample_mode(
        host,
        thresholds,
        mode,
        Path::new("/"),
        agents,
        SAMPLED_AT.to_string(),
        PLATFORM.to_string(),
    )
}

fn kinds(snapshot: &ResourceSnapshot) -> Vec<ResourceKind> {
    snapshot
        .readings
        .iter()
        .map(|reading| reading.kind)
        .collect()
}

fn reading_of(snapshot: &ResourceSnapshot, kind: ResourceKind) -> &Reading {
    snapshot
        .readings
        .iter()
        .find(|reading| reading.kind == kind)
        .expect("reading is present")
}

fn level_of(eval: &mcao_core::resources::Evaluation, kind: ResourceKind) -> Level {
    eval.levels
        .iter()
        .find(|(entry, _, _)| *entry == kind)
        .map(|(_, level, _)| *level)
        .expect("level is present")
}

fn loaded(thresholds: Thresholds) -> LoadedThresholds {
    LoadedThresholds {
        thresholds,
        source: "test".to_string(),
        warning: None,
    }
}

#[test]
fn all_mode_samples_every_resource_even_when_disabled() {
    let snapshot = sample_in(&Host::healthy(), &off(), SampleMode::All, None);

    assert_eq!(
        kinds(&snapshot),
        vec![
            ResourceKind::MemoryUsedPct,
            ResourceKind::SwapUsedPct,
            ResourceKind::CpuUsedPct,
            ResourceKind::DiskFreePct,
        ]
    );
    assert_eq!(
        reading_of(&snapshot, ResourceKind::MemoryUsedPct).value,
        Some(50.0)
    );
    assert_eq!(
        reading_of(&snapshot, ResourceKind::SwapUsedPct).value,
        Some(10.0)
    );
    assert_eq!(
        reading_of(&snapshot, ResourceKind::CpuUsedPct).value,
        Some(12.5)
    );
    assert_eq!(
        reading_of(&snapshot, ResourceKind::DiskFreePct).value,
        Some(50.0)
    );
}

#[test]
fn enabled_mode_samples_only_enabled_resources() {
    let mut memory_only = off();
    memory_only.memory_used_pct = Some(90.0);
    assert_eq!(
        kinds(&sample_in(
            &Host::healthy(),
            &memory_only,
            SampleMode::Enabled,
            None
        )),
        vec![ResourceKind::MemoryUsedPct]
    );

    let mut swap_only = off();
    swap_only.swap_used_pct = Some(85.0);
    assert_eq!(
        kinds(&sample_in(
            &Host::healthy(),
            &swap_only,
            SampleMode::Enabled,
            None
        )),
        vec![ResourceKind::SwapUsedPct]
    );
}

#[test]
fn plain_sample_is_the_enabled_mode_with_no_count() {
    let mut thresholds = Thresholds::defaults();
    thresholds.running_agents = Some(3);
    let host = Host::healthy();

    let plain = sample(
        &host,
        &thresholds,
        Path::new("/"),
        SAMPLED_AT.to_string(),
        PLATFORM.to_string(),
    );
    let enabled = sample_in(&host, &thresholds, SampleMode::Enabled, None);
    assert_eq!(plain, enabled);
}

#[test]
fn all_mode_reports_a_supplied_count() {
    let snapshot = sample_in(&Host::healthy(), &off(), SampleMode::All, Some(Ok(3)));
    let reading = reading_of(&snapshot, ResourceKind::RunningAgents);
    assert_eq!(reading.value, Some(3.0));
    assert_eq!(reading.reason, None);
}

#[test]
fn a_failed_count_is_unavailable_with_its_reason() {
    let failed = SampleError::new("state directory unreadable");
    let snapshot = sample_in(&Host::healthy(), &off(), SampleMode::All, Some(Err(failed)));
    let reading = reading_of(&snapshot, ResourceKind::RunningAgents);
    assert_eq!(reading.value, None);
    assert_eq!(
        reading.reason.as_deref(),
        Some("state directory unreadable")
    );
}

#[test]
fn no_count_and_no_limit_leaves_running_agents_out() {
    let snapshot = sample_in(&Host::healthy(), &off(), SampleMode::All, None);
    assert!(
        snapshot
            .readings
            .iter()
            .all(|reading| reading.kind != ResourceKind::RunningAgents)
    );
}

#[test]
fn enabled_mode_leaves_a_supplied_count_out_when_its_limit_is_off() {
    let enabled_only = off();
    let counted = sample_in(
        &Host::healthy(),
        &enabled_only,
        SampleMode::Enabled,
        Some(Ok(3)),
    );
    let failed = sample_in(
        &Host::healthy(),
        &enabled_only,
        SampleMode::Enabled,
        Some(Err(SampleError::new("state directory unreadable"))),
    );

    assert!(!kinds(&counted).contains(&ResourceKind::RunningAgents));
    assert!(!kinds(&failed).contains(&ResourceKind::RunningAgents));
}

#[test]
fn enabled_limit_with_a_supplied_count_breaches_at_the_limit() {
    let mut thresholds = off();
    thresholds.running_agents = Some(3);
    let snapshot = sample_in(
        &Host::healthy(),
        &thresholds,
        SampleMode::Enabled,
        Some(Ok(3)),
    );
    let eval = evaluate(snapshot, &loaded(thresholds));
    assert_eq!(
        level_of(&eval, ResourceKind::RunningAgents),
        Level::Breached
    );
    assert_eq!(eval.breaches.len(), 1);
    assert_eq!(eval.breaches[0].value, 3.0);
    assert_eq!(eval.breaches[0].limit, 3.0);
}

#[test]
fn all_mode_levels_match_enabled_mode_for_enabled_limits() {
    let host = Host {
        memory: Ok(MemoryStats {
            total: 100,
            available: 5,
            swap_total: 200,
            swap_used: 190,
        }),
        cpu: Ok(95.0),
        disk: Ok(DiskStats {
            total: 1000,
            available: 50,
        }),
    };
    let thresholds = Thresholds::defaults();

    let enabled = sample_in(&host, &thresholds, SampleMode::Enabled, None);
    let all = sample_in(&host, &thresholds, SampleMode::All, None);
    for kind in [
        ResourceKind::MemoryUsedPct,
        ResourceKind::SwapUsedPct,
        ResourceKind::CpuUsedPct,
        ResourceKind::DiskFreePct,
    ] {
        assert_eq!(reading_of(&enabled, kind), reading_of(&all, kind));
    }

    let enabled_eval = evaluate(enabled, &loaded(thresholds.clone()));
    let all_eval = evaluate(all, &loaded(thresholds));
    for kind in [
        ResourceKind::MemoryUsedPct,
        ResourceKind::SwapUsedPct,
        ResourceKind::CpuUsedPct,
        ResourceKind::DiskFreePct,
    ] {
        assert_eq!(level_of(&enabled_eval, kind), level_of(&all_eval, kind));
    }
    assert_eq!(
        level_of(&all_eval, ResourceKind::MemoryUsedPct),
        Level::Breached
    );
    assert_eq!(
        level_of(&all_eval, ResourceKind::DiskFreePct),
        Level::Breached
    );
}
