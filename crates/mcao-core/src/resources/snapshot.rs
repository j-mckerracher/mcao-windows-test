//! The `fm-resource-snapshot.v1` JSON shape, built from an [`Evaluation`].
//!
//! The view borrows from the evaluation and adds no data of its own. Its keys and names are
//! the live contract in DPES-866; a key change needs a new schema version.

use serde::Serialize;
use serde_json::Number;

use super::{Evaluation, Level, ResourceKind, State};

/// Schema version of the snapshot JSON.
pub const SNAPSHOT_SCHEMA: &str = "fm-resource-snapshot.v1";

#[derive(Clone, Debug, Serialize)]
pub struct SnapshotView<'a> {
    pub schema: &'static str,
    pub sampled_at: &'a str,
    pub platform: &'a str,
    pub state: State,
    pub config: ConfigView<'a>,
    pub readings: Vec<ReadingView<'a>>,
    pub breaches: Vec<BreachView>,
}

#[derive(Clone, Debug, Serialize)]
pub struct ConfigView<'a> {
    pub source: &'a str,
    pub warning: Option<&'a str>,
}

/// One resource in the snapshot. `limit` comes from the thresholds, so a disabled resource
/// carries `limit: null` and `level: disabled`.
#[derive(Clone, Debug, Serialize)]
pub struct ReadingView<'a> {
    pub kind: ResourceKind,
    pub value: Option<Number>,
    pub limit: Option<Number>,
    pub level: Level,
    pub reason: Option<&'a str>,
}

/// One breached limit. Its value and limit print as JSON floats, like the readings.
#[derive(Clone, Debug, Serialize)]
pub struct BreachView {
    pub kind: ResourceKind,
    pub value: Number,
    pub limit: Number,
}

impl<'a> From<&'a Evaluation> for SnapshotView<'a> {
    fn from(eval: &'a Evaluation) -> Self {
        let readings = eval
            .levels
            .iter()
            .map(|(kind, level, limit)| {
                let reading = eval.snapshot.readings.iter().find(|r| r.kind == *kind);
                ReadingView {
                    kind: *kind,
                    value: reading.and_then(|r| r.value).and_then(Number::from_f64),
                    limit: limit.and_then(Number::from_f64),
                    level: *level,
                    reason: reading.and_then(|r| r.reason.as_deref()),
                }
            })
            .collect();
        let breaches = eval
            .breaches
            .iter()
            .filter_map(|b| {
                let value = Number::from_f64(b.value);
                let limit = Number::from_f64(b.limit);
                // `evaluate` records a breach only for a finite value. Thresholds parse only finite
                // limits, since `parse_percent` rejects NaN and infinity. So `Number::from_f64`
                // yields a value for every breach, and a `None` here would be a bug the assert
                // catches in debug.
                debug_assert!(value.is_some() && limit.is_some(), "{b:?}");
                Some(BreachView {
                    kind: b.kind,
                    value: value?,
                    limit: limit?,
                })
            })
            .collect();
        Self {
            schema: SNAPSHOT_SCHEMA,
            sampled_at: &eval.snapshot.sampled_at,
            platform: &eval.snapshot.platform,
            state: eval.state,
            config: ConfigView {
                source: &eval.config_source,
                warning: eval.config_warning.as_deref(),
            },
            readings,
            breaches,
        }
    }
}
