//! Pure threshold evaluation: snapshot plus thresholds in, levels, breaches, and state out.

use serde::Serialize;

use super::{LoadedThresholds, ResourceKind, ResourceSnapshot};

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Level {
    Ok,
    Breached,
    Unknown,
    Disabled,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum State {
    Ok,
    Breached,
    Unknown,
}

#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct Breach {
    pub kind: ResourceKind,
    pub value: f64,
    pub limit: f64,
}

#[derive(Clone, Debug, PartialEq)]
pub struct Evaluation {
    pub snapshot: ResourceSnapshot,
    pub levels: Vec<(ResourceKind, Level, Option<f64>)>,
    pub breaches: Vec<Breach>,
    pub state: State,
    pub config_source: String,
    pub config_warning: Option<String>,
}

/// Evaluates every resource, in [`ResourceKind::ALL`] order.
///
/// Unknown readings never breach. A disabled limit is `Disabled`. An enabled limit with no
/// usable value is `Unknown`. Disk free space breaches when below its limit; every other
/// resource breaches when at or above its limit.
pub fn evaluate(snapshot: ResourceSnapshot, config: &LoadedThresholds) -> Evaluation {
    let thresholds = &config.thresholds;
    let mut levels = Vec::with_capacity(ResourceKind::ALL.len());
    let mut breaches = Vec::new();

    for kind in ResourceKind::ALL {
        let limit = thresholds.limit_for(kind);
        let value = snapshot
            .readings
            .iter()
            .find(|r| r.kind == kind)
            .and_then(|r| r.value);
        let level = match (limit, value) {
            (None, _) => Level::Disabled,
            (Some(_), None) => Level::Unknown,
            (Some(_), Some(v)) if !v.is_finite() => Level::Unknown,
            (Some(limit), Some(v)) => {
                if breaches_limit(kind, v, limit) {
                    breaches.push(Breach {
                        kind,
                        value: v,
                        limit,
                    });
                    Level::Breached
                } else {
                    Level::Ok
                }
            }
        };
        levels.push((kind, level, limit));
    }

    let state = if !breaches.is_empty() {
        State::Breached
    } else if levels.iter().any(|(_, level, _)| *level == Level::Unknown) {
        State::Unknown
    } else {
        State::Ok
    };

    Evaluation {
        snapshot,
        levels,
        breaches,
        state,
        config_source: config.source.clone(),
        config_warning: config.warning.clone(),
    }
}

fn breaches_limit(kind: ResourceKind, value: f64, limit: f64) -> bool {
    match kind {
        ResourceKind::DiskFreePct => value < limit,
        _ => value >= limit,
    }
}
