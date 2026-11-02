//! Turns one sampler call into a snapshot. Reads no clock and no host itself.

use std::path::Path;
use std::time::Duration;

use super::{Reading, ResourceKind, ResourceSampler, ResourceSnapshot, SampleError, Thresholds};

/// The one deliberate wait: a single CPU window, taken only when the CPU reading is sampled.
pub const CPU_WINDOW: Duration = Duration::from_millis(250);

/// Which resources a sample covers.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SampleMode {
    /// Only the resources whose thresholds are enabled, as admission samples them.
    Enabled,
    /// Every resource, enabled or not, as `show` samples them.
    All,
}

/// Samples only the resources whose thresholds are enabled, with no running-agent count.
///
/// An enabled running-agent limit with no count therefore reads as unavailable and evaluates as
/// unknown, never as a breach.
///
/// A failed or unusable reading becomes a reading with `value: None` and a reason. It never
/// panics and never changes another reading.
pub fn sample(
    sampler: &dyn ResourceSampler,
    thresholds: &Thresholds,
    disk_path: &Path,
    sampled_at: String,
    platform: String,
) -> ResourceSnapshot {
    sample_mode(
        sampler,
        thresholds,
        SampleMode::Enabled,
        disk_path,
        None,
        sampled_at,
        platform,
    )
}

/// Samples the resources that `mode` covers. The sampler is the only host reader.
///
/// `agents` is the running-agent outcome, taken by the caller with `count_running_agents`, so this
/// function reads no state directory. It follows the same coverage rule as every other reading: it
/// appears when `mode` is [`SampleMode::All`] or its limit is enabled, and is left out otherwise.
/// `None` means no count was taken for this sample. Then the running-agent reading appears only
/// when its limit is enabled, and it is unavailable with the same reason [`sample`] gives.
///
/// A reading's value does not depend on its limit, only on whether its resource is covered, so an
/// enabled limit gets the same level in every mode.
///
/// A failed or unusable reading becomes a reading with `value: None` and a reason. It never
/// panics and never changes another reading.
///
/// Memory and swap come from [`ResourceSampler::memory`] and [`ResourceSampler::swap`]. A swap
/// failure leaves only the swap reading unknown, and a memory failure leaves only the memory reading
/// unknown.
pub fn sample_mode(
    sampler: &dyn ResourceSampler,
    thresholds: &Thresholds,
    mode: SampleMode,
    disk_path: &Path,
    agents: Option<Result<u32, SampleError>>,
    sampled_at: String,
    platform: String,
) -> ResourceSnapshot {
    let wanted = |enabled: bool| mode == SampleMode::All || enabled;
    let mut readings = Vec::new();

    let memory_on = wanted(thresholds.memory_used_pct.is_some());
    let swap_on = wanted(thresholds.swap_used_pct.is_some());
    if memory_on {
        readings.push(match sampler.memory() {
            Ok(m) => reading(
                ResourceKind::MemoryUsedPct,
                m.total
                    .checked_sub(m.available)
                    .and_then(|used| percent(used, m.total)),
                "memory total is 0 or available exceeds total",
            ),
            Err(e) => unavailable(ResourceKind::MemoryUsedPct, e.reason),
        });
    }
    if swap_on {
        readings.push(match sampler.swap() {
            Ok(s) => reading(
                ResourceKind::SwapUsedPct,
                if s.total == 0 {
                    Some(0.0)
                } else {
                    percent(s.used, s.total)
                },
                "swap used exceeds swap total",
            ),
            Err(e) => unavailable(ResourceKind::SwapUsedPct, e.reason),
        });
    }

    if wanted(thresholds.cpu_used_pct.is_some()) {
        readings.push(match sampler.cpu_used_pct(CPU_WINDOW) {
            Ok(v) => reading(
                ResourceKind::CpuUsedPct,
                cpu_pct(v),
                "CPU reading is not a finite, non-negative percentage",
            ),
            Err(e) => unavailable(ResourceKind::CpuUsedPct, e.reason),
        });
    }

    if wanted(thresholds.disk_free_pct.is_some()) {
        readings.push(match sampler.disk(disk_path) {
            Ok(d) => reading(
                ResourceKind::DiskFreePct,
                percent(d.available, d.total),
                "filesystem size is 0 or free space exceeds size",
            ),
            Err(e) => unavailable(ResourceKind::DiskFreePct, e.reason),
        });
    }

    let agents_on = wanted(thresholds.running_agents.is_some());
    match agents {
        Some(outcome) if agents_on => readings.push(match outcome {
            Ok(count) => Reading {
                kind: ResourceKind::RunningAgents,
                value: Some(f64::from(count)),
                reason: None,
            },
            Err(e) => unavailable(ResourceKind::RunningAgents, e.reason),
        }),
        None if thresholds.running_agents.is_some() => readings.push(unavailable(
            ResourceKind::RunningAgents,
            "no running-agent count was supplied to this sample".to_string(),
        )),
        _ => {}
    }

    ResourceSnapshot {
        sampled_at,
        platform,
        readings,
    }
}

fn reading(kind: ResourceKind, value: Option<f64>, missing_reason: &str) -> Reading {
    match value {
        Some(v) => Reading {
            kind,
            value: Some(v),
            reason: None,
        },
        None => unavailable(kind, missing_reason.to_string()),
    }
}

fn unavailable(kind: ResourceKind, reason: String) -> Reading {
    Reading {
        kind,
        value: None,
        reason: Some(reason),
    }
}

/// `part` as a percentage of `whole`, one decimal. `None` when `whole` is 0 or `part` exceeds it.
fn percent(part: u64, whole: u64) -> Option<f64> {
    if whole == 0 || part > whole {
        return None;
    }
    Some(one_decimal(part as f64 * 100.0 / whole as f64))
}

/// A CPU reading is usable when it is finite and not negative after rounding. Rounding comes
/// first, so a finite input too large to scale reads as unusable instead of as infinity. Values
/// above 100 are kept and compared, so an overshoot breaches instead of reading unknown.
fn cpu_pct(raw: f64) -> Option<f64> {
    let rounded = one_decimal(raw);
    if !rounded.is_finite() || rounded < 0.0 {
        return None;
    }
    // Negative zero compares equal to zero; report plain zero.
    Some(if rounded == 0.0 { 0.0 } else { rounded })
}

/// Rounds to one decimal, nearest. Rounding happens before any comparison, so 89.96 used reads
/// as 90.0 and breaches, while 9.96 free reads as 10.0 and does not. Each case moves at most
/// 0.05 points.
fn one_decimal(raw: f64) -> f64 {
    (raw * 10.0).round() / 10.0
}
