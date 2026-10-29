//! The admission decision for one agent start: sample the host, evaluate the limits, and run the
//! threshold actions when a limit breaches.
//!
//! Admission never fails. An input that cannot be read leaves its resource unknown, and an
//! unknown resource never breaches.

use std::path::PathBuf;
use std::time::{SystemTime, UNIX_EPOCH};

use super::{
    ActionRegistry, Deferral, Evaluation, ResourceSampler, SampleMode, StartRequest,
    count_running_agents, evaluate, load_thresholds, sample_mode, utc_stamp,
};

/// Where an admission reads its inputs. The caller chooses every path, and admission reads nothing
/// else.
pub struct AdmitInputs {
    /// The config directory that holds `resource-thresholds`.
    pub config_dir: PathBuf,
    /// State directories of local homes. The first is the starting home.
    pub state_dirs: Vec<PathBuf>,
    /// A path on the filesystem whose free space is measured.
    pub disk_path: PathBuf,
}

/// Whether the start may proceed.
#[derive(Clone, Debug, PartialEq)]
pub enum Disposition {
    Admit,
    Defer(Deferral),
}

/// The decision and the evaluation it was made on.
///
/// Dropping an admission starts the agent without the resource check, so the result must be used.
#[derive(Clone, Debug, PartialEq)]
#[must_use = "dropping an admission starts the agent without the resource check"]
pub struct Admission {
    pub disposition: Disposition,
    pub evaluation: Evaluation,
}

/// Decides whether `req` may start now.
///
/// This function does not print [`Evaluation::config_warning`]. A malformed thresholds file falls
/// back to the defaults, and the caller that shows the decision (slice 866.E) must print that
/// warning, or the fallback goes unreported.
pub fn admit(
    req: &StartRequest,
    inputs: &AdmitInputs,
    sampler: &dyn ResourceSampler,
    registry: &ActionRegistry,
) -> Admission {
    let loaded = load_thresholds(&inputs.config_dir);
    let agents = loaded
        .thresholds
        .running_agents
        .map(|_| count_running_agents(&inputs.state_dirs, Some(&req.task)));
    let snapshot = sample_mode(
        sampler,
        &loaded.thresholds,
        SampleMode::Enabled,
        &inputs.disk_path,
        agents,
        sampled_at(),
        platform(),
    );
    let evaluation = evaluate(snapshot, &loaded);
    if evaluation.breaches.is_empty() {
        return Admission {
            disposition: Disposition::Admit,
            evaluation,
        };
    }
    let deferral = registry.run(req, &evaluation);
    Admission {
        disposition: Disposition::Defer(deferral),
        evaluation,
    }
}

/// The current time as RFC 3339 UTC. Admission reads the clock here because the sampler reads none.
fn sampled_at() -> String {
    let epoch = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default();
    utc_stamp(epoch.as_secs())
}

/// `<os>-<arch>`, the platform form the snapshot contract uses.
fn platform() -> String {
    format!("{}-{}", std::env::consts::OS, std::env::consts::ARCH)
}
