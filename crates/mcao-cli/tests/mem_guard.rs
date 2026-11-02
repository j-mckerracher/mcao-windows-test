//! `mem-guard` parity and CLI tests (DPES-864, AC3 and AC6).
//!
//! The cases under `tests/fixtures/mem-guard` are saved output of the pinned Python reference,
//! recorded once on Linux (see `provenance.txt`). Each case replays its recorded meminfo and
//! process rows through the Rust verdict, and the JSON must match the saved stdout byte for byte.
//! Nothing here runs Python or generates an expected value.

use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command, Output, Stdio};
use std::time::Duration;

use mcao_core::resources::{
    DiskStats, GuardLimits, GuardReport, MemoryStats, ProcessRss, ResourceSampler, SampleError,
    assess, render_json, render_text,
};

/// Replays one recorded case: its meminfo, and the process rows the Python run listed.
struct Recorded {
    memory: Result<MemoryStats, SampleError>,
    procs: Vec<ProcessRss>,
}

impl ResourceSampler for Recorded {
    fn memory(&self) -> Result<MemoryStats, SampleError> {
        self.memory.clone()
    }

    fn cpu_used_pct(&self, _window: Duration) -> Result<f64, SampleError> {
        Err(SampleError::new("not sampled by mem-guard"))
    }

    fn disk(&self, _path: &Path) -> Result<DiskStats, SampleError> {
        Err(SampleError::new("not sampled by mem-guard"))
    }

    fn top_processes(&self, n: usize) -> Result<Vec<ProcessRss>, SampleError> {
        Ok(self.procs.iter().take(n).cloned().collect())
    }
}

fn read(path: &Path) -> String {
    fs::read_to_string(path).unwrap_or_else(|err| panic!("cannot read {}: {err}", path.display()))
}

fn fixture_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/mem-guard")
}

/// The four thresholds in flag order: memory warn, memory crit, swap warn, swap crit.
fn read_limits(path: &Path) -> GuardLimits {
    let values: Vec<f64> = read(path)
        .split_whitespace()
        .map(|value| value.parse().expect("a threshold is a number"))
        .collect();
    let &[warn_mem_pct, crit_mem_pct, warn_swap_pct, crit_swap_pct] = values.as_slice() else {
        panic!("{} must hold four thresholds", path.display());
    };
    GuardLimits {
        warn_mem_pct,
        crit_mem_pct,
        warn_swap_pct,
        crit_swap_pct,
    }
}

/// The meminfo keys the verdict reads, in kB. A missing MemTotal or MemAvailable makes the
/// sampler unreadable, which the reference reports as UNKNOWN. A missing SwapTotal reads as zero
/// swap. The shared sampler cannot express unknown swap, so a missing SwapFree reads as no swap
/// used; no recorded case omits it.
fn read_memory(path: &Path) -> Result<MemoryStats, SampleError> {
    let text = read(path);
    let kb = |key: &str| -> Option<u64> {
        text.lines().find_map(|line| {
            let (name, rest) = line.split_once(':')?;
            if name.trim() != key {
                return None;
            }
            rest.split_whitespace().next()?.parse().ok()
        })
    };
    let total = kb("MemTotal").ok_or_else(|| SampleError::new("MemTotal is missing"))?;
    let available =
        kb("MemAvailable").ok_or_else(|| SampleError::new("MemAvailable is missing"))?;
    let swap_total = kb("SwapTotal").unwrap_or(0);
    let swap_free = kb("SwapFree").unwrap_or(swap_total);
    Ok(MemoryStats {
        total: total * 1024,
        available: available * 1024,
        swap_total: swap_total * 1024,
        swap_used: swap_total.saturating_sub(swap_free) * 1024,
    })
}

/// The process rows the Python run listed: pid, comm, and rss_mb, tab-separated. The sampler
/// reports bytes, so each MB value is scaled back. Rounding to one decimal recovers the value.
fn read_procs(path: &Path) -> Vec<ProcessRss> {
    read(path)
        .lines()
        .map(|line| {
            let mut fields = line.splitn(3, '\t');
            let pid = fields
                .next()
                .and_then(|field| field.parse().ok())
                .expect("a pid column");
            let name = fields.next().expect("a comm column").to_string();
            let rss_mb: f64 = fields
                .next()
                .and_then(|field| field.parse().ok())
                .expect("an rss_mb column");
            ProcessRss {
                pid,
                name,
                rss_bytes: (rss_mb * 1_048_576.0).round() as u64,
            }
        })
        .collect()
}

/// The `checked_at` the recorded run printed. It is replayed as-is, so the comparison stays exact.
fn recorded_checked_at(stdout: &str) -> String {
    let line = stdout
        .lines()
        .find(|line| line.trim_start().starts_with("\"checked_at\""))
        .expect("recorded stdout carries checked_at");
    line.trim()
        .trim_start_matches("\"checked_at\": ")
        .trim_end_matches(',')
        .trim_matches('"')
        .to_string()
}

/// Replays one recorded case directory into the report the Rust verdict makes for it.
fn replay(dir: &Path) -> GuardReport {
    let sampler = Recorded {
        memory: read_memory(&dir.join("meminfo")),
        procs: read_procs(&dir.join("procs")),
    };
    assess(
        &sampler,
        read_limits(&dir.join("limits")),
        recorded_checked_at(&read(&dir.join("stdout"))),
    )
}

/// The JSON keys at the report's top level and in its summary, in order. Process rows sit deeper
/// and are skipped, so the list does not depend on how many processes a host lists.
#[cfg(target_os = "linux")]
fn report_keys(json: &str) -> Vec<&str> {
    json.lines()
        .filter_map(|line| {
            let indent = line.len() - line.trim_start().len();
            if indent != 2 && indent != 4 {
                return None;
            }
            line.trim_start().strip_prefix('"')?.split('"').next()
        })
        .collect()
}

#[test]
fn every_recorded_case_matches_the_python_reference() {
    let mut cases: Vec<PathBuf> = fs::read_dir(fixture_root())
        .expect("fixture root is readable")
        .map(|entry| entry.expect("fixture entry is readable").path())
        .filter(|path| path.join("stdout").is_file())
        .collect();
    cases.sort();
    assert_eq!(cases.len(), 9, "all nine recorded cases must be replayed");

    for case in &cases {
        let name = case
            .file_name()
            .map(|name| name.to_string_lossy().into_owned())
            .unwrap_or_default();
        let stdout = read(&case.join("stdout"));
        let report = replay(case);
        assert_eq!(
            format!("{}\n", render_json(&report)),
            stdout,
            "{name}: JSON differs from the recorded Python output"
        );
        let expected_exit: i32 = read(&case.join("exit"))
            .trim()
            .parse()
            .expect("exit holds an integer");
        assert_eq!(
            i32::from(report.status.fails_check()),
            expected_exit,
            "{name}: exit status differs from the recorded Python run"
        );
    }
}

#[cfg(target_os = "linux")]
#[test]
fn live_sampler_gives_a_measured_report_on_linux() {
    use mcao_core::resources::{GuardStatus, SysSampler};

    let report = assess(
        &SysSampler::new(),
        GuardLimits::default(),
        "2000-01-01T00:00:00Z".to_string(),
    );
    assert_ne!(
        report.status,
        GuardStatus::Unknown,
        "a live Linux host must be measurable"
    );
    let used = report
        .summary
        .mem_used_pct
        .expect("a measured report carries memory use");
    assert!(
        (0.0..=100.0).contains(&used),
        "memory use {used} is a percentage"
    );
    assert!(report.top_processes.len() <= 10);
    let json = render_json(&report);
    assert!(json.contains("\"name\": \"fm-jev-mem-guard\""));
    assert!(json.contains("\"summary\": {"));
}

/// The live verdict against the host's own `/proc/meminfo`, read at the same moment. The shared
/// sampler and this test read the same file, so the figures agree within the AC's 1 pp tolerance.
/// The JSON keys must match the reference's recorded output.
#[cfg(target_os = "linux")]
#[test]
fn live_report_agrees_with_proc_meminfo_on_linux() {
    use mcao_core::resources::{GuardStatus, SysSampler};

    let report = assess(
        &SysSampler::new(),
        GuardLimits::default(),
        "2000-01-01T00:00:00Z".to_string(),
    );
    let meminfo = read_memory(Path::new("/proc/meminfo")).expect("/proc/meminfo is readable");
    let summary = &report.summary;

    let total_gb = summary
        .mem_total_gb
        .expect("a measured report carries total memory");
    let expected_total_gb = meminfo.total as f64 / 1_073_741_824.0;
    assert!(
        (total_gb - expected_total_gb).abs() <= 0.01,
        "total {total_gb} GB differs from /proc/meminfo's {expected_total_gb} GB"
    );

    let expected_mem_pct =
        (meminfo.total - meminfo.available) as f64 / meminfo.total as f64 * 100.0;
    let mem_pct = summary
        .mem_used_pct
        .expect("a measured report carries memory use");
    assert!(
        (mem_pct - expected_mem_pct).abs() <= 1.0,
        "memory use {mem_pct}% differs from /proc/meminfo's {expected_mem_pct}% by over 1 pp"
    );

    let expected_swap_pct = if meminfo.swap_total == 0 {
        0.0
    } else {
        meminfo.swap_used as f64 / meminfo.swap_total as f64 * 100.0
    };
    let swap_pct = summary
        .swap_used_pct
        .expect("a measured report carries swap use");
    assert!(
        (swap_pct - expected_swap_pct).abs() <= 1.0,
        "swap use {swap_pct}% differs from /proc/meminfo's {expected_swap_pct}% by over 1 pp"
    );

    assert_ne!(
        report.status,
        GuardStatus::Unknown,
        "a live Linux host is measurable"
    );
    let recorded = read(&fixture_root().join("swap-warning/stdout"));
    assert_eq!(
        report_keys(&render_json(&report)),
        report_keys(&recorded),
        "live JSON keys differ from the recorded reference output"
    );
}

/// The reference's text branch (`bin/fm-jev-mem-guard.py`, the `else` of its `args.json` test)
/// applied to the recorded swap-warning values. The expected text is written out by hand from
/// that branch, so the layout is checked against the reference, not against this crate's output.
#[test]
fn text_report_follows_the_reference_print_block_for_a_recorded_swap_warning() {
    let report = replay(&fixture_root().join("swap-warning"));
    assert_eq!(
        render_text(&report),
        "fm-jev-mem-guard — 2026-10-24T16:31:08Z
  • RAM: 10.0% used (0.86 GB available / 0.95 GB total)
  • Swap: 90.0% used (0.86 GB used / 0.95 GB total)
  • Status: WARNING
  • Recommendation: Memory or swap utilization is above a warning threshold but below a critical one; degraded but explained, see the top RSS processes.

  Top 1 RSS Processes:
    - PID 7 (python3): 19.7 MB"
    );
}

/// The same reference branch for a recorded UNKNOWN verdict: the RAM and swap lines fall back to
/// their "unavailable" and "not measurable" wording, and the process list still prints.
#[test]
fn text_report_follows_the_reference_print_block_for_recorded_meminfo_unavailable() {
    let report = replay(&fixture_root().join("meminfo-unavailable"));
    assert_eq!(
        render_text(&report),
        "fm-jev-mem-guard — 2026-10-24T16:31:10Z
  • RAM: unavailable
  • Swap: unknown (not measurable)
  • Status: UNKNOWN
  • Recommendation: /proc/meminfo is unreadable or incomplete on this host; the verdict is withheld rather than fabricated.

  Top 1 RSS Processes:
    - PID 7 (python3): 19.7 MB"
    );
}

fn mcao(args: &[&str]) -> Output {
    Command::new(env!("CARGO_BIN_EXE_mcao"))
        .args(args)
        .env("NO_COLOR", "1")
        .stdin(Stdio::null())
        .output()
        .expect("mcao starts")
}

#[test]
fn mem_guard_json_has_the_reference_shape() {
    let out = mcao(&["mem-guard", "--json"]);
    assert_eq!(
        out.status.code(),
        Some(0),
        "stderr: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    let stdout = String::from_utf8(out.stdout).expect("the report is UTF-8");
    assert!(stdout.starts_with("{\n  \"name\": \"fm-jev-mem-guard\",\n"));
    for key in [
        "\"checked_at\": \"",
        "\"status\": \"",
        "\"summary\": {",
        "\"top_processes\": ",
    ] {
        assert!(stdout.contains(key), "report lacks {key}");
    }
}

#[test]
fn mem_guard_text_starts_with_the_report_title() {
    let out = mcao(&["mem-guard"]);
    let stdout = String::from_utf8(out.stdout).expect("the report is UTF-8");
    assert!(stdout.starts_with("fm-jev-mem-guard — "), "{stdout}");
}

#[test]
fn mem_guard_check_exits_zero_or_one_never_usage_or_unsupported() {
    let code = mcao(&["mem-guard", "--check"]).status.code();
    assert!(matches!(code, Some(0 | 1)), "--check exited {code:?}");
}

#[test]
fn mem_guard_check_exits_one_when_a_threshold_is_breached() {
    // Any measured host is at or above a zero critical threshold, so the verdict is CRITICAL.
    let out = mcao(&["mem-guard", "--check", "--crit-mem-pct", "0"]);
    assert_eq!(out.status.code(), Some(1));
}

#[test]
fn mem_guard_rejects_a_non_numeric_threshold_as_usage() {
    let out = mcao(&["mem-guard", "--warn-mem-pct", "high"]);
    assert_eq!(out.status.code(), Some(2));
}
