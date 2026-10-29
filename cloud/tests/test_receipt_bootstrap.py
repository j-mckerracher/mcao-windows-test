"""Accepted receipts and worker bootstraps: one projection, verified byte for byte."""

from datetime import timedelta, timezone
from typing import Any

from fakes import (
    CLAIM_ID,
    DEFAULT_MAX_RUNTIME_SECONDS,
    EPOCH,
    IMAGE_DIGEST,
    PROFILE_ID,
    RUN_ID,
    SOURCE_TAR,
    bootstrap_source,
    refusal_code,
    run_record,
    tool_compatibility,
    worker_bootstrap,
)
from mao_cloud.protocol import (
    AcceptedReceipt,
    BootstrapSource,
    ErrorCode,
    ObjectKind,
    RunState,
    ToolCompatibility,
    build_bootstrap,
    build_receipt,
    object_name,
    projection_bytes,
    projection_digest,
    sha256_hex,
    verify_bootstrap,
)

OTHER_RUN_ID = "run_" + "c" * 32
DEADLINE = EPOCH + timedelta(seconds=DEFAULT_MAX_RUNTIME_SECONDS)
EAST_OF_UTC = timezone(timedelta(hours=-5))


def _receipt(**overrides: Any) -> AcceptedReceipt:
    return build_receipt(
        run_record(**overrides),
        source=bootstrap_source(),
        compatibility=tool_compatibility(),
    )


def _other_compatibility() -> ToolCompatibility:
    return ToolCompatibility(
        image_digest="sha256:" + "e" * 64,
        profile_id=PROFILE_ID,
        toolchain_manifest_digest="1" * 64,
        instruction_digest="2" * 64,
    )


def _other_source() -> BootstrapSource:
    return BootstrapSource(
        object_name=object_name(ObjectKind.INPUT, run_id=RUN_ID),
        generation=1,
        sha256="f" * 64,
        size_bytes=len(SOURCE_TAR),
        base_sha="b" * 40,
    )


# --- Receipt construction -----------------------------------------------------------------------


def test_receipt_projection_digest_is_the_sha256_of_its_projection_bytes() -> None:
    receipt = _receipt()
    assert receipt.projection_digest == sha256_hex(projection_bytes(receipt))


def test_receipt_projection_digest_is_recomputable_from_the_receipt() -> None:
    receipt = _receipt()
    assert projection_digest(receipt) == receipt.projection_digest


def test_receipt_carries_the_run_facts_it_was_built_from() -> None:
    record = run_record(state=RunState.DISPATCHING, version=4)
    receipt = build_receipt(record, source=bootstrap_source(), compatibility=tool_compatibility())
    assert receipt.run_id == RUN_ID
    assert receipt.state is RunState.DISPATCHING
    assert receipt.version == 4
    assert receipt.attempt == 1
    assert receipt.manifest_digest == record.manifest_digest
    assert receipt.deadline_at == DEADLINE
    assert receipt.max_runtime_seconds == DEFAULT_MAX_RUNTIME_SECONDS


# --- Bootstrap construction ---------------------------------------------------------------------


def test_bootstrap_for_a_dispatched_run_matches_the_receipt_identity() -> None:
    receipt = _receipt()
    record = run_record(state=RunState.DISPATCHING, claim_id=CLAIM_ID, receipt=receipt)
    assert build_bootstrap(record) == worker_bootstrap()


def test_bootstrap_for_a_retry_carries_the_new_attempt_fence_and_claim() -> None:
    receipt = _receipt()
    record = run_record(
        state=RunState.DISPATCHING,
        attempt=2,
        fence=2,
        claim_id="claim_" + "c" * 32,
        receipt=receipt,
    )
    bootstrap = build_bootstrap(record)
    assert (bootstrap.attempt, bootstrap.fence, bootstrap.claim_id) == (2, 2, "claim_" + "c" * 32)


def test_bootstrap_without_a_receipt_is_illegal() -> None:
    record = run_record(state=RunState.DISPATCHING, claim_id=CLAIM_ID)
    code = refusal_code(lambda: build_bootstrap(record))
    assert code is ErrorCode.ILLEGAL_TRANSITION


def test_bootstrap_without_a_claim_is_illegal() -> None:
    record = run_record(state=RunState.DISPATCHING, receipt=_receipt())
    code = refusal_code(lambda: build_bootstrap(record))
    assert code is ErrorCode.ILLEGAL_TRANSITION


def test_bootstrap_for_a_run_not_yet_dispatching_is_illegal() -> None:
    record = run_record(state=RunState.QUEUED, receipt=_receipt(), claim_id=CLAIM_ID)
    code = refusal_code(lambda: build_bootstrap(record))
    assert code is ErrorCode.ILLEGAL_TRANSITION


# --- Verification: success ----------------------------------------------------------------------


def test_matching_bootstrap_verifies_against_its_receipt() -> None:
    verify_bootstrap(worker_bootstrap(), _receipt())


def test_attempt_fence_and_claim_are_excluded_from_the_identity() -> None:
    bootstrap = worker_bootstrap(attempt=2, fence=2, claim_id="claim_" + "c" * 32)
    verify_bootstrap(bootstrap, _receipt())


def test_equal_instants_in_different_offsets_produce_identical_projection_bytes() -> None:
    shifted = DEADLINE.astimezone(EAST_OF_UTC)
    assert projection_bytes(worker_bootstrap(deadline_at=shifted)) == projection_bytes(
        worker_bootstrap()
    )


def test_equal_instants_in_different_offsets_verify_against_the_receipt() -> None:
    shifted = DEADLINE.astimezone(EAST_OF_UTC)
    verify_bootstrap(worker_bootstrap(deadline_at=shifted), _receipt())


# --- Verification: each immutable field refuses --------------------------------------------------


def _mismatch(bootstrap: Any, receipt: AcceptedReceipt) -> ErrorCode:
    return refusal_code(lambda: verify_bootstrap(bootstrap, receipt))


def test_different_run_id_is_a_bootstrap_mismatch() -> None:
    assert _mismatch(worker_bootstrap(run_id=OTHER_RUN_ID), _receipt()) is (
        ErrorCode.BOOTSTRAP_MISMATCH
    )


def test_different_source_sha_is_a_bootstrap_mismatch() -> None:
    assert _mismatch(worker_bootstrap(source=_other_source()), _receipt()) is (
        ErrorCode.BOOTSTRAP_MISMATCH
    )


def test_different_compatibility_is_a_bootstrap_mismatch() -> None:
    assert _mismatch(worker_bootstrap(compatibility=_other_compatibility()), _receipt()) is (
        ErrorCode.BOOTSTRAP_MISMATCH
    )


def test_different_deadline_instant_is_a_bootstrap_mismatch() -> None:
    later = DEADLINE + timedelta(seconds=1)
    assert _mismatch(worker_bootstrap(deadline_at=later), _receipt()) is (
        ErrorCode.BOOTSTRAP_MISMATCH
    )


def test_different_max_runtime_is_a_bootstrap_mismatch() -> None:
    assert _mismatch(worker_bootstrap(max_runtime_seconds=900), _receipt()) is (
        ErrorCode.BOOTSTRAP_MISMATCH
    )


def test_tampered_receipt_digest_is_a_bootstrap_mismatch() -> None:
    receipt = _receipt().model_copy(update={"projection_digest": "0" * 64})
    assert _mismatch(worker_bootstrap(), receipt) is ErrorCode.BOOTSTRAP_MISMATCH


def test_receipt_with_a_different_deadline_refuses_the_matching_bootstrap() -> None:
    assert _mismatch(worker_bootstrap(), _receipt(deadline_at=DEADLINE + timedelta(hours=1))) is (
        ErrorCode.BOOTSTRAP_MISMATCH
    )


def test_bootstrap_for_another_profile_is_a_bootstrap_mismatch() -> None:
    other = ToolCompatibility(
        image_digest=IMAGE_DIGEST,
        profile_id="pi-other",
        toolchain_manifest_digest="1" * 64,
        instruction_digest="2" * 64,
    )
    assert _mismatch(worker_bootstrap(compatibility=other), _receipt()) is (
        ErrorCode.BOOTSTRAP_MISMATCH
    )
