//! Host resource contract: the resource types, the [`ResourceSampler`] boundary, threshold
//! parsing, and evaluation (DPES-866).
//!
//! [`SysSampler`] in `sys.rs` is the one native reader. It reads memory, CPU, disk, and processes
//! through `sysinfo` on every platform. On macOS the admission path reads swap from the
//! `vm.swapusage` sysctl, while the mem-guard reads swap through `sysinfo`, which reports 0 when
//! that sysctl fails. Everything else takes a sampler, so tests use fakes, and fake readings are
//! never native evidence. [`sample`] turns one sampler call into a snapshot.
//! [`count_running_agents`] counts held slots, and the caller passes that count into a sample.
//! [`admit`] decides whether a start may proceed, and [`render_admission_json`] prints the
//! decision. Process control is not implemented here. The mem-guard verdict in `guard.rs` reads
//! the same sampler.

mod actions;
mod admit;
mod agents;
mod evaluate;
mod guard;
mod render;
mod sample;
mod snapshot;
mod sys;
mod thresholds;

use std::fmt;
use std::path::Path;
use std::time::Duration;

use serde::Serialize;

pub use actions::{
    ActionError, ActionOutcome, ActionRegistry, DeferStartAction, Deferral, StartKind,
    StartRequest, ThresholdAction, default_registry,
};
pub use admit::{Admission, AdmitInputs, Disposition, admit};
pub use agents::count_running_agents;
pub use evaluate::{Breach, Evaluation, Level, State, evaluate};
pub use guard::{
    GuardLimits, GuardReport, GuardStatus, assess, render_json, render_text, utc_stamp,
};
pub use render::{ADMISSION_SCHEMA, render_admission_json};
pub use sample::{CPU_WINDOW, SampleMode, sample, sample_mode};
pub use snapshot::{ConfigView, ReadingView, SNAPSHOT_SCHEMA, SnapshotView};
pub use sys::SysSampler;
pub use thresholds::{LoadedThresholds, Thresholds, load_thresholds, parse_thresholds};

/// The five resources. Serialized names are the keys of the live `fm-resource-snapshot.v1` contract.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ResourceKind {
    MemoryUsedPct,
    SwapUsedPct,
    CpuUsedPct,
    DiskFreePct,
    RunningAgents,
}

impl ResourceKind {
    pub const ALL: [Self; 5] = [
        Self::MemoryUsedPct,
        Self::SwapUsedPct,
        Self::CpuUsedPct,
        Self::DiskFreePct,
        Self::RunningAgents,
    ];

    /// The snake_case key the threshold file and the snapshot use for this resource. Matches the
    /// serde name, and a test checks that they agree.
    pub const fn key(self) -> &'static str {
        match self {
            Self::MemoryUsedPct => "memory_used_pct",
            Self::SwapUsedPct => "swap_used_pct",
            Self::CpuUsedPct => "cpu_used_pct",
            Self::DiskFreePct => "disk_free_pct",
            Self::RunningAgents => "running_agents",
        }
    }
}

/// One measurement. `value` is `None` when it could not be measured, and `reason` says why.
#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct Reading {
    pub kind: ResourceKind,
    pub value: Option<f64>,
    pub reason: Option<String>,
}

/// One sample of the host. `sampled_at` is RFC 3339 UTC and `platform` is `<os>-<arch>`.
/// The caller supplies both and nothing here checks them, so a fake sampler can report any
/// platform string. Tests use `fake-test`.
#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct ResourceSnapshot {
    pub sampled_at: String,
    pub platform: String,
    pub readings: Vec<Reading>,
}

/// Memory counters in bytes. The `swap_*` fields are raw counters from the memory read. The swap
/// reading comes from [`ResourceSampler::swap`], which may refuse to call a zero total swap.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct MemoryStats {
    pub total: u64,
    pub available: u64,
    pub swap_total: u64,
    pub swap_used: u64,
}

/// Swap counters in bytes. A `total` of zero from [`ResourceSampler::swap`] means the sampler
/// confirmed there is no swap. A sampler that cannot confirm that returns an error instead.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct SwapStats {
    pub total: u64,
    pub used: u64,
}

/// Filesystem counters in bytes.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct DiskStats {
    pub total: u64,
    pub available: u64,
}

/// Resident memory of one process. The shape is a placeholder for `mem-guard` (DPES-864).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ProcessRss {
    pub pid: u32,
    pub name: String,
    pub rss_bytes: u64,
}

/// Why a sampler could not read something.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SampleError {
    pub reason: String,
}

impl SampleError {
    pub fn new(reason: impl Into<String>) -> Self {
        Self {
            reason: reason.into(),
        }
    }
}

impl fmt::Display for SampleError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.reason)
    }
}

impl std::error::Error for SampleError {}

/// The host-reading boundary. Implementations must not start processes or mutate the machine.
pub trait ResourceSampler {
    fn memory(&self) -> Result<MemoryStats, SampleError>;
    /// Swap counters. The default takes them from [`ResourceSampler::memory`]. A sampler that cannot
    /// tell zero swap from unreadable swap overrides this, so unreadable swap is an error here and
    /// the swap reading becomes unknown rather than zero.
    fn swap(&self) -> Result<SwapStats, SampleError> {
        self.memory().map(|m| SwapStats {
            total: m.swap_total,
            used: m.swap_used,
        })
    }
    fn cpu_used_pct(&self, window: Duration) -> Result<f64, SampleError>;
    fn disk(&self, path: &Path) -> Result<DiskStats, SampleError>;
    fn top_processes(&self, n: usize) -> Result<Vec<ProcessRss>, SampleError>;
}
