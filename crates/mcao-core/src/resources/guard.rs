//! The `mem-guard` verdict over the shared host sampler (DPES-864, slice C).
//!
//! [`assess`] reproduces `bin/fm-jev-mem-guard.py`: its comparisons, its rounding (percentages to
//! one decimal and gigabytes to two, both before any comparison), its UNKNOWN rule, and its
//! top-process rule. [`render_json`] and [`render_text`] print the report in the reference's
//! layout. Reads no clock and no host itself; the caller supplies the sampler and `checked_at`.

use super::{MemoryStats, ProcessRss, ResourceSampler};
use crate::util::json::encode_ascii;
use crate::util::pystr::from_str;

/// The name the reference report carries, kept so the report matches it byte for byte.
const REPORT_NAME: &str = "fm-jev-mem-guard";
/// How many processes the report lists: the reference's `get_top_rss_processes` default.
const TOP_PROCESS_COUNT: usize = 10;
/// Processes under this many kB resident are not listed, as in the reference.
const MIN_RSS_KB: u64 = 10_240;
/// The machine-readable reason an UNKNOWN verdict carries, as the reference words it.
const UNKNOWN_REASON: &str = "meminfo-unavailable";

/// The four thresholds, in percent. The defaults are the reference's flag defaults.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct GuardLimits {
    pub warn_mem_pct: f64,
    pub crit_mem_pct: f64,
    pub warn_swap_pct: f64,
    pub crit_swap_pct: f64,
}

impl Default for GuardLimits {
    fn default() -> Self {
        Self {
            warn_mem_pct: 90.0,
            crit_mem_pct: 95.0,
            warn_swap_pct: 85.0,
            crit_swap_pct: 95.0,
        }
    }
}

/// The verdict. Labels are the reference's status strings.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum GuardStatus {
    Ok,
    Warning,
    Critical,
    Unknown,
}

impl GuardStatus {
    /// The status word the reference prints.
    pub const fn label(self) -> &'static str {
        match self {
            Self::Ok => "OK",
            Self::Warning => "WARNING",
            Self::Critical => "CRITICAL",
            Self::Unknown => "UNKNOWN",
        }
    }

    /// Whether `--check` exits 1. UNKNOWN fails open and exits 0, as in the reference.
    pub const fn fails_check(self) -> bool {
        matches!(self, Self::Warning | Self::Critical)
    }

    const fn recommendation(self) -> &'static str {
        match self {
            Self::Ok => {
                "Memory and swap utilization are within thresholds; the caller should continue unchanged."
            }
            Self::Warning => {
                "Memory or swap utilization is above a warning threshold but below a critical one; degraded but explained, see the top RSS processes."
            }
            Self::Critical => {
                "Memory or swap utilization is at or above a critical threshold; this host condition can explain worker silence while it holds."
            }
            Self::Unknown => {
                "/proc/meminfo is unreadable or incomplete on this host; the verdict is withheld rather than fabricated."
            }
        }
    }
}

/// One assessment, shaped like the reference report.
#[derive(Clone, Debug, PartialEq)]
pub struct GuardReport {
    pub checked_at: String,
    pub status: GuardStatus,
    pub reason: Option<&'static str>,
    pub summary: Summary,
    pub top_processes: Vec<TopProcess>,
}

/// Memory and swap figures. Every field is `None` when the verdict is UNKNOWN.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Summary {
    pub mem_total_gb: Option<f64>,
    pub mem_available_gb: Option<f64>,
    pub mem_used_pct: Option<f64>,
    pub swap_total_gb: Option<f64>,
    pub swap_used_gb: Option<f64>,
    pub swap_used_pct: Option<f64>,
}

/// One listed process, with its resident size in MB rounded to one decimal.
#[derive(Clone, Debug, PartialEq)]
pub struct TopProcess {
    pub pid: u32,
    pub comm: String,
    pub rss_mb: f64,
}

/// Assesses host memory and swap against `limits`. `checked_at` is the caller's UTC stamp.
///
/// A memory reading that fails or has no total is UNKNOWN, never OK. The shared sampler reports
/// unreadable swap as zero, so swap alone never makes the verdict UNKNOWN.
pub fn assess(
    sampler: &dyn ResourceSampler,
    limits: GuardLimits,
    checked_at: String,
) -> GuardReport {
    let top_processes = top_processes(sampler);
    match sampler.memory() {
        Ok(memory) if memory.total > 0 => measured(memory, limits, checked_at, top_processes),
        _ => GuardReport {
            checked_at,
            status: GuardStatus::Unknown,
            reason: Some(UNKNOWN_REASON),
            summary: Summary::default(),
            top_processes,
        },
    }
}

fn measured(
    memory: MemoryStats,
    limits: GuardLimits,
    checked_at: String,
    top_processes: Vec<TopProcess>,
) -> GuardReport {
    let used = memory.total.saturating_sub(memory.available);
    let mem_used_pct = round(percent(used, memory.total), 1);
    let (swap_used_gb, swap_used_pct) = if memory.swap_total == 0 {
        (0.0, 0.0)
    } else {
        (
            gb(memory.swap_used),
            round(percent(memory.swap_used, memory.swap_total), 1),
        )
    };
    let critical = mem_used_pct >= limits.crit_mem_pct || swap_used_pct >= limits.crit_swap_pct;
    let warning = mem_used_pct >= limits.warn_mem_pct || swap_used_pct >= limits.warn_swap_pct;
    let status = if critical {
        GuardStatus::Critical
    } else if warning {
        GuardStatus::Warning
    } else {
        GuardStatus::Ok
    };
    GuardReport {
        checked_at,
        status,
        reason: None,
        summary: Summary {
            mem_total_gb: Some(gb(memory.total)),
            mem_available_gb: Some(gb(memory.available)),
            mem_used_pct: Some(mem_used_pct),
            swap_total_gb: Some(gb(memory.swap_total)),
            swap_used_gb: Some(swap_used_gb),
            swap_used_pct: Some(swap_used_pct),
        },
        top_processes,
    }
}

/// Largest resident processes first, ties by lower pid, with those under the floor dropped. A
/// failed listing shows no processes, as in the reference. The sampler returns its largest
/// processes first, so the first [`TOP_PROCESS_COUNT`] already hold every one that is listed.
fn top_processes(sampler: &dyn ResourceSampler) -> Vec<TopProcess> {
    let mut listed: Vec<ProcessRss> = sampler
        .top_processes(TOP_PROCESS_COUNT)
        .unwrap_or_default()
        .into_iter()
        .filter(|p| p.rss_bytes / 1024 >= MIN_RSS_KB)
        .collect();
    listed.sort_by(|a, b| b.rss_bytes.cmp(&a.rss_bytes).then(a.pid.cmp(&b.pid)));
    listed.truncate(TOP_PROCESS_COUNT);
    listed
        .into_iter()
        .map(|p| TopProcess {
            pid: p.pid,
            comm: p.name,
            rss_mb: round((p.rss_bytes / 1024) as f64 / 1024.0, 1),
        })
        .collect()
}

/// `part` as a percentage of `whole`, computed in the reference's order: divide, then scale.
fn percent(part: u64, whole: u64) -> f64 {
    part as f64 / whole as f64 * 100.0
}

/// Bytes as gigabytes (GiB), rounded to two decimals like the reference's `round(kb / 1024², 2)`.
fn gb(bytes: u64) -> f64 {
    round(bytes as f64 / 1_073_741_824.0, 2)
}

/// Python's `round(value, places)`: the decimal nearest the exact binary value, ties to even.
/// Formatting the exact value rounds the same way. Callers pass finite values only.
fn round(value: f64, places: usize) -> f64 {
    format!("{value:.places$}")
        .parse()
        .expect("a formatted finite float parses back")
}

/// The report as `json.dumps(report, indent=2)` prints it: the reference's key order, ASCII
/// escapes, and floats in Python's `repr` form. No trailing newline.
pub fn render_json(report: &GuardReport) -> String {
    let s = &report.summary;
    let processes = if report.top_processes.is_empty() {
        "[]".to_string()
    } else {
        let rows: Vec<String> = report
            .top_processes
            .iter()
            .map(|p| {
                format!(
                    "    {{\n      \"pid\": {},\n      \"comm\": {},\n      \"rss_mb\": {:?}\n    }}",
                    p.pid,
                    json_string(&p.comm),
                    p.rss_mb
                )
            })
            .collect();
        format!("[\n{}\n  ]", rows.join(",\n"))
    };
    let reason = report.reason.map_or("null".to_string(), json_string);
    format!(
        "{{\n  \"name\": {},\n  \"checked_at\": {},\n  \"status\": {},\n  \"recommendation\": {},\n  \"reason\": {reason},\n  \"summary\": {{\n    \"mem_total_gb\": {},\n    \"mem_available_gb\": {},\n    \"mem_used_pct\": {},\n    \"swap_total_gb\": {},\n    \"swap_used_gb\": {},\n    \"swap_used_pct\": {}\n  }},\n  \"top_processes\": {processes}\n}}",
        json_string(REPORT_NAME),
        json_string(&report.checked_at),
        json_string(report.status.label()),
        json_string(report.status.recommendation()),
        json_float(s.mem_total_gb),
        json_float(s.mem_available_gb),
        json_float(s.mem_used_pct),
        json_float(s.swap_total_gb),
        json_float(s.swap_used_gb),
        json_float(s.swap_used_pct),
    )
}

/// The report as the reference prints it without `--json`, one line per item. No trailing newline.
pub fn render_text(report: &GuardReport) -> String {
    let s = &report.summary;
    let ram = match (s.mem_used_pct, s.mem_available_gb, s.mem_total_gb) {
        (Some(used), Some(available), Some(total)) => {
            format!("  • RAM: {used:?}% used ({available:?} GB available / {total:?} GB total)")
        }
        _ => "  • RAM: unavailable".to_string(),
    };
    let swap = match (s.swap_used_pct, s.swap_used_gb, s.swap_total_gb) {
        (Some(used), Some(used_gb), Some(total_gb)) => {
            format!("  • Swap: {used:?}% used ({used_gb:?} GB used / {total_gb:?} GB total)")
        }
        _ => "  • Swap: unknown (not measurable)".to_string(),
    };
    let mut lines = vec![
        format!("{REPORT_NAME} — {}", report.checked_at),
        ram,
        swap,
        format!("  • Status: {}", report.status.label()),
        format!("  • Recommendation: {}", report.status.recommendation()),
    ];
    if !report.top_processes.is_empty() {
        lines.push(format!(
            "\n  Top {} RSS Processes:",
            report.top_processes.len()
        ));
        lines.extend(
            report
                .top_processes
                .iter()
                .map(|p| format!("    - PID {} ({}): {:?} MB", p.pid, p.comm, p.rss_mb)),
        );
    }
    lines.join("\n")
}

/// `datetime.now(timezone.utc).strftime("%Y-%m-%dT%H:%M:%SZ")` for whole epoch seconds.
pub fn utc_stamp(epoch_secs: u64) -> String {
    let (year, month, day) = civil_from_days((epoch_secs / 86_400) as i64);
    let secs = epoch_secs % 86_400;
    format!(
        "{year:04}-{month:02}-{day:02}T{:02}:{:02}:{:02}Z",
        secs / 3_600,
        secs % 3_600 / 60,
        secs % 60
    )
}

/// Days since 1970-01-01 as (year, month, day), proleptic Gregorian.
fn civil_from_days(days: i64) -> (i64, i64, i64) {
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z.rem_euclid(146_097);
    let yoe = (doe - doe / 1_460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let day = doy - (153 * mp + 2) / 5 + 1;
    let month = if mp < 10 { mp + 3 } else { mp - 9 };
    (yoe + era * 400 + i64::from(month <= 2), month, day)
}

fn json_string(text: &str) -> String {
    encode_ascii(&from_str(text))
}

fn json_float(value: Option<f64>) -> String {
    value.map_or_else(|| "null".to_string(), |v| format!("{v:?}"))
}

#[cfg(test)]
mod tests {
    use std::path::Path;
    use std::time::Duration;

    use super::{
        GuardLimits, GuardReport, GuardStatus, Summary, TopProcess, UNKNOWN_REASON, assess,
        render_text, round, utc_stamp,
    };
    use crate::resources::{DiskStats, MemoryStats, ProcessRss, ResourceSampler, SampleError};

    /// Fixed readings, so every platform runs the same mapping. Processes come largest first,
    /// as the shared sampler returns them.
    struct Fixed {
        memory: Result<MemoryStats, SampleError>,
        procs: Vec<ProcessRss>,
    }

    impl ResourceSampler for Fixed {
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

    /// Memory in kB, as `/proc/meminfo` states it.
    fn host(total_kb: u64, available_kb: u64, swap_kb: u64, swap_used_kb: u64) -> Fixed {
        Fixed {
            memory: Ok(MemoryStats {
                total: total_kb * 1024,
                available: available_kb * 1024,
                swap_total: swap_kb * 1024,
                swap_used: swap_used_kb * 1024,
            }),
            procs: Vec::new(),
        }
    }

    fn check(sampler: &Fixed) -> GuardReport {
        assess(sampler, GuardLimits::default(), String::new())
    }

    #[test]
    fn rounds_half_to_even_on_the_exact_binary_value() {
        assert_eq!(round(0.25, 1), 0.2);
        assert_eq!(round(0.35, 1), 0.3);
        assert_eq!(round(0.45, 1), 0.5);
        assert_eq!(round(2.5, 0), 2.0);
    }

    #[test]
    fn thresholds_are_inclusive_and_compare_rounded_values() {
        // 89.96% rounds to 90.0 before the comparison, so it breaches the 90 warning.
        assert_eq!(
            check(&host(10_000, 1_004, 0, 0)).status,
            GuardStatus::Warning
        );
        assert_eq!(
            check(&host(1_000_000, 100_000, 0, 0)).status,
            GuardStatus::Warning
        );
        assert_eq!(
            check(&host(1_000_000, 50_000, 0, 0)).status,
            GuardStatus::Critical
        );
        assert_eq!(
            check(&host(1_000_000, 900_000, 1_000_000, 960_000)).status,
            GuardStatus::Critical
        );
    }

    #[test]
    fn unreadable_or_empty_memory_is_unknown_never_ok() {
        let failed = Fixed {
            memory: Err(SampleError::new("no MemAvailable")),
            procs: Vec::new(),
        };
        for report in [check(&failed), check(&host(0, 0, 0, 0))] {
            assert_eq!(report.status, GuardStatus::Unknown);
            assert_eq!(report.reason, Some(UNKNOWN_REASON));
            assert_eq!(report.summary, Summary::default());
        }
    }

    #[test]
    fn available_above_total_reads_zero_used_and_zero_swap_reads_zero() {
        let report = check(&host(1_000_000, 2_000_000, 0, 0));
        assert_eq!(report.status, GuardStatus::Ok);
        assert_eq!(report.summary.mem_used_pct, Some(0.0));
        assert_eq!(report.summary.swap_used_pct, Some(0.0));
    }

    #[test]
    fn top_processes_drop_small_ones_sort_by_size_and_cap_at_ten() {
        // Pids 1..=12 at 5 MiB times pid: pid 1 is under the 10 MB floor and drops out.
        let procs = (1..=12u32)
            .rev()
            .map(|pid| ProcessRss {
                pid,
                name: format!("p{pid}"),
                rss_bytes: u64::from(pid) * 5 * 1024 * 1024,
            })
            .collect();
        let mut sampler = host(1_000_000, 900_000, 0, 0);
        sampler.procs = procs;
        let pids: Vec<u32> = check(&sampler)
            .top_processes
            .iter()
            .map(|p| p.pid)
            .collect();
        assert_eq!(pids, (3..=12).rev().collect::<Vec<u32>>());
    }

    #[test]
    fn text_layout_follows_the_reference_prints() {
        let report = GuardReport {
            checked_at: "2026-01-02T03:04:05Z".to_string(),
            status: GuardStatus::Warning,
            reason: None,
            summary: Summary {
                mem_total_gb: Some(15.5),
                mem_available_gb: Some(1.25),
                mem_used_pct: Some(91.9),
                swap_total_gb: Some(2.0),
                swap_used_gb: Some(0.5),
                swap_used_pct: Some(25.0),
            },
            top_processes: vec![TopProcess {
                pid: 7,
                comm: "python3".to_string(),
                rss_mb: 214.0,
            }],
        };
        let expected = "fm-jev-mem-guard — 2026-01-02T03:04:05Z\n  • RAM: 91.9% used (1.25 GB available / 15.5 GB total)\n  • Swap: 25.0% used (0.5 GB used / 2.0 GB total)\n  • Status: WARNING\n  • Recommendation: Memory or swap utilization is above a warning threshold but below a critical one; degraded but explained, see the top RSS processes.\n\n  Top 1 RSS Processes:\n    - PID 7 (python3): 214.0 MB";
        assert_eq!(render_text(&report), expected);
    }

    #[test]
    fn unknown_text_withholds_every_figure() {
        let report = GuardReport {
            checked_at: "2026-01-02T03:04:05Z".to_string(),
            status: GuardStatus::Unknown,
            reason: Some(UNKNOWN_REASON),
            summary: Summary::default(),
            top_processes: Vec::new(),
        };
        let expected = "fm-jev-mem-guard — 2026-01-02T03:04:05Z\n  • RAM: unavailable\n  • Swap: unknown (not measurable)\n  • Status: UNKNOWN\n  • Recommendation: /proc/meminfo is unreadable or incomplete on this host; the verdict is withheld rather than fabricated.";
        assert_eq!(render_text(&report), expected);
    }

    #[test]
    fn utc_stamp_formats_known_instants() {
        assert_eq!(utc_stamp(0), "1970-01-01T00:00:00Z");
        assert_eq!(utc_stamp(1_000_000_000), "2001-09-09T01:46:40Z");
        assert_eq!(utc_stamp(1_700_000_000), "2023-11-14T22:13:20Z");
    }
}
