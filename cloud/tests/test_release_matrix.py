"""Rollout and rollback compatibility matrix (DPES-819 AC5, AC6, edge addendum).

These are pure decisions over recorded identities. No cloud calls, no containers. Proof that a
real rollback leaves Cloud Run, Jobs, and the Scheduler intact needs real GCP and is a stated gap
(DPES-804, 807, 819, 871); the fake-level proof here is the plan contents plus the no-delete check.
"""

import json
from pathlib import Path

import pytest

from mao_cloud.release_matrix import (
    ACTIVE_CHECKPOINT_RULE,
    KILL_SWITCH_CLASSES,
    MatrixError,
    QueuedRun,
    ActiveCheckpoint,
    Drain,
    Release,
    load_matrix,
    plan_rollout,
    plan_rollback,
    rollout_order,
)

CLOUD = Path(__file__).resolve().parents[1]
SHIPPED_MATRIX = CLOUD / "release" / "compatibility-matrix.json"

OLD_CONTROLLER = "sha256:" + "a1" * 32
NEW_CONTROLLER = "sha256:" + "b2" * 32
OLD_WORKER = "sha256:" + "c3" * 32
NEW_WORKER = "sha256:" + "d4" * 32
OLD_REGISTRY = "sha256:" + "e5" * 32
NEW_REGISTRY = "sha256:" + "f6" * 32
TRUSTED = "mao_cloud.worker_entry"


def release(
    *,
    controller: str,
    worker: str,
    registry: str,
    schema: int = 1,
    profile: str = "4/16",
    entrypoint: str = TRUSTED,
    harness: str = "pi-1.3.0",
) -> Release:
    return Release(
        controller_image_digest=controller,
        worker_image_digest=worker,
        worker_profile_id=profile,
        worker_entrypoint=entrypoint,
        protocol_version="v1",
        checkpoint_schema_version=schema,
        tool_registry_digest=registry,
        harness_version=harness,
    )


OLD = release(controller=OLD_CONTROLLER, worker=OLD_WORKER, registry=OLD_REGISTRY)
NEW = release(controller=NEW_CONTROLLER, worker=NEW_WORKER, registry=NEW_REGISTRY)
RETAINED = frozenset({OLD_CONTROLLER, OLD_WORKER, OLD_REGISTRY, NEW_CONTROLLER, NEW_WORKER, NEW_REGISTRY})


def matrix_of(*releases: Release):
    return load_matrix({"schema_version": 1, "supported": [r.to_payload() for r in releases]})


MATRIX = matrix_of(OLD, NEW)


def test_shipped_matrix_loads_and_every_entry_is_pilot_and_trusted() -> None:
    matrix = load_matrix(json.loads(SHIPPED_MATRIX.read_text(encoding="utf-8")))
    for entry in matrix.supported:
        assert entry.worker_profile_id == "4/16"
        assert entry.worker_entrypoint == TRUSTED


def test_matrix_rejects_a_digest_that_is_not_sha256_hex() -> None:
    bad = OLD.to_payload() | {"worker_image_digest": "sha256:not-hex"}
    with pytest.raises(MatrixError):
        load_matrix({"schema_version": 1, "supported": [bad]})


def test_matrix_rejects_a_duplicate_identity() -> None:
    with pytest.raises(MatrixError):
        load_matrix({"schema_version": 1, "supported": [OLD.to_payload(), OLD.to_payload()]})


def test_matrix_rejects_an_unknown_schema_version() -> None:
    with pytest.raises(MatrixError):
        load_matrix({"schema_version": 2, "supported": []})


def test_rollout_order_is_identities_then_images_then_smoke_then_scheduler_then_pilot_flag() -> None:
    assert rollout_order() == (
        "deploy identities and state",
        "deploy controller and worker images by digest",
        "run service, Jobs, and auth smoke checks",
        "enable Scheduler and monitoring",
        "enable the pilot flag",
    )


def test_rollback_disables_admissions_before_engaging_every_kill_switch_class() -> None:
    plan = plan_rollback(
        source=NEW,
        target=OLD,
        matrix=MATRIX,
        retained=RETAINED,
        queued=(),
        checkpoints=(),
        drain=Drain.DRAIN,
        admissions_disabled=True,
    )
    assert plan.refused == ()
    assert plan.steps[0] == "disable new admissions, drain admitting requests, and capture the affected run snapshot"
    assert plan.steps[1] == "engage kill switch: " + ", ".join(sorted(KILL_SWITCH_CLASSES))


def test_rollback_plan_never_contains_a_delete_step() -> None:
    plan = plan_rollback(
        source=NEW,
        target=OLD,
        matrix=MATRIX,
        retained=RETAINED,
        queued=(QueuedRun("run-1", OLD_WORKER),),
        checkpoints=(ActiveCheckpoint("run-2", 1, OLD_WORKER, "pi-1.3.0"),),
        drain=Drain.CANCEL,
        admissions_disabled=True,
    )
    assert not any("delete" in step.lower() for step in plan.steps)


def test_rollback_restores_the_exact_target_digests() -> None:
    plan = plan_rollback(
        source=NEW,
        target=OLD,
        matrix=MATRIX,
        retained=RETAINED,
        queued=(),
        checkpoints=(),
        drain=Drain.DRAIN,
        admissions_disabled=True,
    )
    joined = "\n".join(plan.steps)
    assert OLD_CONTROLLER in joined
    assert OLD_WORKER in joined
    assert OLD_REGISTRY in joined


def test_rollback_requires_the_drain_choice_to_be_explicit() -> None:
    with pytest.raises(TypeError):
        plan_rollback(  # type: ignore[call-arg]
            source=NEW,
            target=OLD,
            matrix=MATRIX,
            retained=RETAINED,
            queued=(),
            checkpoints=(),
        )


def test_queued_run_admitted_under_the_target_worker_dispatches_after_rollback() -> None:
    plan = plan_rollback(
        source=NEW,
        target=OLD,
        matrix=MATRIX,
        retained=RETAINED,
        queued=(QueuedRun("run-old", OLD_WORKER),),
        checkpoints=(),
        drain=Drain.DRAIN,
        admissions_disabled=True,
    )
    assert plan.dispatch == ("run-old",)
    assert plan.preserved == ()


def test_queued_run_admitted_under_the_rolled_back_worker_is_preserved_not_reinterpreted() -> None:
    # NEW is the release being rolled back from (source); its queued run must not be dispatched
    # under OLD (target) and must not be reinterpreted.
    plan = plan_rollback(
        source=NEW,
        target=OLD,
        matrix=MATRIX,
        retained=RETAINED,
        queued=(QueuedRun("run-new", NEW_WORKER),),
        checkpoints=(),
        drain=Drain.DRAIN,
        admissions_disabled=True,
    )
    assert plan.dispatch == ()
    assert [p.run_id for p in plan.preserved] == ["run-new"]
    assert "not reinterpreted" in plan.preserved[0].reason


def test_active_checkpoint_from_the_target_resumes_after_rollback() -> None:
    plan = plan_rollback(
        source=NEW,
        target=OLD,
        matrix=MATRIX,
        retained=RETAINED,
        queued=(),
        checkpoints=(ActiveCheckpoint("run-ckpt", 1, OLD_WORKER, "pi-1.3.0"),),
        drain=Drain.DRAIN,
        admissions_disabled=True,
    )
    assert plan.resume == ("run-ckpt",)
    assert plan.preserved == ()


def test_active_checkpoint_written_by_the_rolled_back_image_is_preserved_and_never_resumed() -> None:
    plan = plan_rollback(
        source=OLD,
        target=NEW,
        matrix=MATRIX,
        retained=RETAINED,
        queued=(),
        checkpoints=(ActiveCheckpoint("run-ckpt", 1, OLD_WORKER, "pi-1.3.0"),),
        drain=Drain.DRAIN,
        admissions_disabled=True,
    )
    assert plan.resume == ()
    assert [p.run_id for p in plan.preserved] == ["run-ckpt"]


def test_checkpoint_with_a_different_schema_is_refused_not_coerced() -> None:
    plan = plan_rollout(
        source=OLD,
        target=release(
            controller=NEW_CONTROLLER,
            worker=NEW_WORKER,
            registry=NEW_REGISTRY,
            schema=2,
        ),
        matrix=matrix_of(
            OLD,
            release(controller=NEW_CONTROLLER, worker=NEW_WORKER, registry=NEW_REGISTRY, schema=2),
        ),
        retained=RETAINED,
        queued=(),
        checkpoints=(ActiveCheckpoint("run-ckpt", 1, OLD_WORKER, "pi-1.3.0"),),
    )
    assert plan.resume == ()
    assert [p.run_id for p in plan.preserved] == ["run-ckpt"]
    assert "schema" in plan.preserved[0].reason


def test_plain_pi_fallback_entrypoint_is_refused() -> None:
    plain = release(controller=OLD_CONTROLLER, worker=OLD_WORKER, registry=OLD_REGISTRY, entrypoint="pi")
    plan = plan_rollback(
        source=NEW,
        target=plain,
        matrix=matrix_of(NEW, plain),
        retained=RETAINED,
        queued=(QueuedRun("run-old", OLD_WORKER),),
        checkpoints=(),
        drain=Drain.DRAIN,
        admissions_disabled=True,
    )
    assert plan.refused
    assert any("plain-pi" in reason or "entrypoint" in reason for reason in plan.refused)
    assert plan.dispatch == ()
    assert plan.steps == ()


def test_small_profile_is_refused_for_the_pilot() -> None:
    small = release(
        controller=OLD_CONTROLLER, worker=OLD_WORKER, registry=OLD_REGISTRY, profile="2/8"
    )
    plan = plan_rollback(
        source=NEW,
        target=small,
        matrix=matrix_of(NEW, small),
        retained=RETAINED,
        queued=(),
        checkpoints=(),
        drain=Drain.DRAIN,
        admissions_disabled=True,
    )
    assert any("profile" in reason for reason in plan.refused)
    assert plan.steps == ()


def test_small_profile_is_refused_for_the_rollout() -> None:
    """DPES-811 AC6. The 2/8 profile stays excluded on the forward path, not only on rollback."""
    small = release(
        controller=NEW_CONTROLLER, worker=NEW_WORKER, registry=NEW_REGISTRY, profile="2/8"
    )
    plan = plan_rollout(
        source=OLD,
        target=small,
        matrix=matrix_of(OLD, small),
        retained=RETAINED,
        queued=(),
        checkpoints=(),
    )
    assert any("profile" in reason for reason in plan.refused)
    assert plan.steps == ()


def test_unsupported_target_is_refused_and_every_run_is_preserved() -> None:
    unknown = release(controller=NEW_CONTROLLER, worker="sha256:" + "99" * 32, registry=NEW_REGISTRY)
    plan = plan_rollback(
        source=NEW,
        target=unknown,
        matrix=MATRIX,
        retained=RETAINED,
        queued=(QueuedRun("run-a", NEW_WORKER),),
        checkpoints=(ActiveCheckpoint("run-b", 1, NEW_WORKER, "pi-1.3.0"),),
        drain=Drain.DRAIN,
        admissions_disabled=True,
    )
    assert any("not a supported" in reason for reason in plan.refused)
    assert plan.dispatch == () and plan.resume == ()
    assert {p.run_id for p in plan.preserved} == {"run-a", "run-b"}
    assert plan.steps == ()


def test_missing_retained_artifact_refuses_the_whole_plan() -> None:
    plan = plan_rollback(
        source=NEW,
        target=OLD,
        matrix=MATRIX,
        retained=RETAINED - {OLD_WORKER},
        queued=(QueuedRun("run-old", OLD_WORKER),),
        checkpoints=(),
        drain=Drain.DRAIN,
        admissions_disabled=True,
    )
    assert any("not retained" in reason and OLD_WORKER in reason for reason in plan.refused)
    assert plan.dispatch == ()
    assert plan.steps == ()


def test_missing_retained_tool_registry_refuses_the_whole_plan() -> None:
    plan = plan_rollback(
        source=NEW,
        target=OLD,
        matrix=MATRIX,
        retained=RETAINED - {OLD_REGISTRY},
        queued=(),
        checkpoints=(ActiveCheckpoint("run-ckpt", 1, OLD_WORKER, "pi-1.3.0"),),
        drain=Drain.DRAIN,
        admissions_disabled=True,
    )
    assert any(OLD_REGISTRY in reason for reason in plan.refused)
    assert plan.resume == ()


def test_unrecorded_source_is_refused_because_rollback_needs_a_known_start() -> None:
    unknown_source = release(controller=NEW_CONTROLLER, worker="sha256:" + "98" * 32, registry=NEW_REGISTRY)
    plan = plan_rollback(
        source=unknown_source,
        target=OLD,
        matrix=MATRIX,
        retained=RETAINED,
        queued=(),
        checkpoints=(),
        drain=Drain.DRAIN,
        admissions_disabled=True,
    )
    assert any("source" in reason for reason in plan.refused)
    assert plan.steps == ()


def test_checkpoint_rule_text_states_exact_identity_match() -> None:
    assert "exact" in ACTIVE_CHECKPOINT_RULE
    assert "not reinterpreted" in ACTIVE_CHECKPOINT_RULE


@pytest.mark.parametrize("field", ["controller_image_digest", "worker_image_digest", "tool_registry_digest"])
def test_digest_with_a_trailing_newline_is_rejected(field: str) -> None:
    bad = OLD.to_payload() | {field: getattr(OLD, field) + "\n"}
    with pytest.raises(MatrixError):
        Release.from_payload(bad)


def test_rollback_without_confirmed_admission_pause_refuses_all_actions() -> None:
    plan = plan_rollback(
        source=NEW, target=OLD, matrix=MATRIX, retained=RETAINED,
        queued=(QueuedRun("queued", OLD_WORKER),),
        checkpoints=(ActiveCheckpoint("active", 1, OLD_WORKER, "pi-1.3.0"),),
        drain=Drain.DRAIN,
    )
    assert plan.refused
    assert any("disable new admissions" in reason for reason in plan.refused)
    assert plan.steps == ()
    assert plan.dispatch == ()
    assert plan.resume == ()
    assert {p.run_id for p in plan.preserved} == {"queued", "active"}


@pytest.mark.parametrize("choice", [Drain.DRAIN, Drain.CANCEL])
def test_cancel_choice_never_authorizes_a_matching_checkpoint_to_resume(choice: Drain) -> None:
    plan = plan_rollback(
        source=NEW, target=OLD, matrix=MATRIX, retained=RETAINED, queued=(),
        checkpoints=(ActiveCheckpoint("active", 1, OLD_WORKER, "pi-1.3.0"),),
        drain=choice, admissions_disabled=True,
    )
    assert plan.refused == ()
    if choice is Drain.CANCEL:
        assert plan.resume == ()
        assert [p.run_id for p in plan.preserved] == ["active"]
        assert "cancellation" in plan.preserved[0].reason
    else:
        assert plan.resume == ("active",)
        assert plan.preserved == ()
