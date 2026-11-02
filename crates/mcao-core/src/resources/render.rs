//! The `fm-resource-admission.v1` JSON shape for `mcao resources admit`, built from an
//! [`Admission`]. The subcommand is not yet wired into the CLI, so nothing prints this shape yet.
//!
//! The view borrows from the admission. Around its `snapshot` it adds only the envelope keys
//! (`schema`, `disposition`, `kind`, `task`, `message`). The snapshot is the `fm-resource-snapshot.v1`
//! view, and the keys are the live contract in DPES-866. A key change needs a new schema version.

use serde::Serialize;

use super::{Admission, Disposition, SnapshotView, StartKind, StartRequest};

/// Schema version of the admission JSON.
pub const ADMISSION_SCHEMA: &str = "fm-resource-admission.v1";

#[derive(Clone, Debug, Serialize)]
struct AdmissionView<'a> {
    schema: &'static str,
    disposition: &'static str,
    kind: StartKind,
    task: &'a str,
    message: Option<&'a str>,
    snapshot: SnapshotView<'a>,
}

impl<'a> AdmissionView<'a> {
    fn new(req: &'a StartRequest, admission: &'a Admission) -> Self {
        let (disposition, message) = match &admission.disposition {
            Disposition::Admit => ("admit", None),
            Disposition::Defer(deferral) => ("defer", Some(deferral.message.as_str())),
        };
        Self {
            schema: ADMISSION_SCHEMA,
            disposition,
            kind: req.kind,
            task: req.task.as_str(),
            message,
            snapshot: SnapshotView::from(&admission.evaluation),
        }
    }
}

/// Renders `admission` for `req` as one line of JSON, the planned output of `mcao resources admit`
/// (not yet wired).
///
/// `message` is `null` on admit and the deferral line on defer. The serialized view holds only
/// strings, numbers, and nulls, so serialization cannot fail.
pub fn render_admission_json(req: &StartRequest, admission: &Admission) -> String {
    serde_json::to_string(&AdmissionView::new(req, admission))
        .expect("the admission view holds only plain JSON values")
}

#[cfg(test)]
mod tests {
    use serde_json::{Value, json};

    use super::*;
    use crate::resources::{
        Evaluation, Reading, ResourceKind, ResourceSnapshot, default_registry, evaluate,
        parse_thresholds,
    };
    use crate::state::TaskId;

    const SAMPLED_AT: &str = "2026-10-20T20:15:03Z";
    const PLATFORM: &str = "fake-test";

    fn request(kind: StartKind) -> StartRequest {
        StartRequest {
            kind,
            task: TaskId::parse("ship-1").expect("fixture task id is valid"),
        }
    }

    /// One memory reading against a 90 percent limit. Below the limit is ok, at or above it breaches.
    /// The other limits are off, so the fixture carries no reading for them.
    fn evaluation(memory_used_pct: f64) -> Evaluation {
        let config = parse_thresholds(
            "memory_used_pct 90\nswap_used_pct off\ncpu_used_pct off\ndisk_free_pct off\n",
            "/cfg/resource-thresholds",
        );
        let snapshot = ResourceSnapshot {
            sampled_at: SAMPLED_AT.to_string(),
            platform: PLATFORM.to_string(),
            readings: vec![Reading {
                kind: ResourceKind::MemoryUsedPct,
                value: Some(memory_used_pct),
                reason: None,
            }],
        };
        evaluate(snapshot, &config)
    }

    fn parse(rendered: &str) -> Value {
        serde_json::from_str(rendered).expect("rendered admission is valid JSON")
    }

    #[test]
    fn resources_admission_json_admit_has_null_message_and_snapshot() {
        let req = request(StartKind::Ship);
        let admission = Admission {
            disposition: Disposition::Admit,
            evaluation: evaluation(50.0),
        };
        let value = parse(&render_admission_json(&req, &admission));

        assert_eq!(value["schema"], json!("fm-resource-admission.v1"));
        assert_eq!(value["disposition"], json!("admit"));
        assert_eq!(value["kind"], json!("ship"));
        assert_eq!(value["task"], json!("ship-1"));
        assert_eq!(value["message"], json!(null));
        assert_eq!(
            value["snapshot"]["schema"],
            json!("fm-resource-snapshot.v1")
        );
        assert_eq!(value["snapshot"]["state"], json!("ok"));
        assert_eq!(value["snapshot"]["breaches"], json!([]));
    }

    #[test]
    fn resources_admission_json_defer_carries_the_message_and_the_breach() {
        let req = request(StartKind::Relaunch);
        let evaluation = evaluation(93.1);
        let deferral = default_registry().run(&req, &evaluation);
        let message = deferral.message.clone();
        let admission = Admission {
            disposition: Disposition::Defer(deferral),
            evaluation,
        };
        let value = parse(&render_admission_json(&req, &admission));

        assert_eq!(value["disposition"], json!("defer"));
        assert_eq!(value["kind"], json!("relaunch"));
        assert_eq!(value["message"], json!(message));
        assert!(
            message.contains("memory_used_pct 93.1 >= 90.0"),
            "{message}"
        );
        assert_eq!(value["snapshot"]["state"], json!("breached"));
        assert_eq!(
            value["snapshot"]["breaches"],
            json!([{"kind": "memory_used_pct", "value": 93.1, "limit": 90.0}])
        );
    }

    #[test]
    fn resources_admission_json_key_order_and_single_line_match_the_contract() {
        let req = request(StartKind::Ship);
        let admission = Admission {
            disposition: Disposition::Admit,
            evaluation: evaluation(50.0),
        };
        let rendered = render_admission_json(&req, &admission);

        assert!(
            rendered.starts_with(
                r#"{"schema":"fm-resource-admission.v1","disposition":"admit","kind":"ship","task":"ship-1","message":null,"snapshot":{"schema":"fm-resource-snapshot.v1","#
            ),
            "{rendered}"
        );
        assert!(!rendered.contains('\n'), "{rendered}");
    }

    #[test]
    fn resources_admission_json_kind_names_are_the_cli_kind_values() {
        for (kind, name) in [
            (StartKind::Ship, "ship"),
            (StartKind::Scout, "scout"),
            (StartKind::Secondmate, "secondmate"),
            (StartKind::Relaunch, "relaunch"),
        ] {
            assert_eq!(
                serde_json::to_value(kind).expect("kind serializes"),
                json!(name)
            );
        }
    }
}
