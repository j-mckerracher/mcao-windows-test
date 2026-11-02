"""The run state machine: transition table, compare-and-set order, and each mutation's refusals."""

from datetime import timedelta
from typing import Any

import pytest
from pydantic import ValidationError

from fakes import (
    CLAIM_ID,
    RUNNER_NONCE,
    DEFAULT_MAX_RUNTIME_SECONDS,
    EPOCH,
    RUN_ID,
    SOURCE_TAR,
    refusal_code,
    run_record,
    start_payload,
    tool_compatibility,
    uuid7,
)
from mao_cloud.protocol import (
    ALLOWED_TRANSITIONS,
    RETRYABLE,
    ClaimDocument,
    ErrorCode,
    FailureCode,
    ObservedInput,
    RunResultDocument,
    RunState,
    StatusReport,
    TimestampVerdict,
    TypedFailure,
    accept_claim,
    accept_result,
    accept_status,
    admit_start,
    begin_dispatch,
    failure,
    parse_start_request,
    replay_start,
    request_cancel,
    retry,
    sha256_hex,
    transition,
)

TERMINAL = frozenset({RunState.SUCCEEDED, RunState.FAILED, RunState.TIMED_OUT, RunState.CANCELLED})
DEADLINE = EPOCH + timedelta(seconds=DEFAULT_MAX_RUNTIME_SECONDS)
SKEW = 30


def _claim(**overrides: Any) -> ClaimDocument:
    values: dict[str, Any] = {
        "protocol_version": "v1",
        "run_id": RUN_ID,
        "attempt": 1,
        "fence": 1,
        "claim_id": CLAIM_ID,
        "runner_nonce": RUNNER_NONCE,
    }
    values.update(overrides)
    return ClaimDocument.model_validate(values)


def _status(**overrides: Any) -> StatusReport:
    values: dict[str, Any] = {
        "protocol_version": "v1",
        "run_id": RUN_ID,
        "attempt": 1,
        "fence": 1,
        "claim_id": CLAIM_ID,
        "seq": 1,
        "phase": "running",
        "observed_at": EPOCH,
        "log_watermark": 0,
    }
    values.update(overrides)
    return StatusReport.model_validate(values)


def _result(outcome: str = "succeeded", **overrides: Any) -> RunResultDocument:
    values: dict[str, Any] = {
        "protocol_version": "v1",
        "run_id": RUN_ID,
        "attempt": 1,
        "fence": 1,
        "claim_id": CLAIM_ID,
        "outcome": outcome,
        "exit_code": 0 if outcome == "succeeded" else 2,
        "commit_sha": "b" * 40,
    }
    values.update(overrides)
    return RunResultDocument.model_validate(values)


def _running(**overrides: Any) -> Any:
    values: dict[str, Any] = {
        "state": RunState.RUNNING,
        "claim_id": CLAIM_ID,
        "version": 4,
        "last_seen_at": EPOCH,
    }
    values.update(overrides)
    return run_record(**values)


def _start_request(record: Any, *, source: bytes = SOURCE_TAR, generation: int = 1) -> Any:
    """A start request naming the record's accepted manifest digest and the given bytes."""
    payload = start_payload({"canonical_digest": record.manifest_digest}, generation=generation, source=source)
    return parse_start_request(payload)


def _observed(**overrides: Any) -> ObservedInput:
    values: dict[str, Any] = {
        "generation": 1,
        "size_bytes": len(SOURCE_TAR),
        "sha256": sha256_hex(SOURCE_TAR),
    }
    values.update(overrides)
    return ObservedInput(**values)


# --- Transition table shape --------------------------------------------------------------------


def test_every_state_has_a_transition_entry() -> None:
    assert set(ALLOWED_TRANSITIONS) == set(RunState)


def test_terminal_states_have_no_outgoing_edges() -> None:
    for state in TERMINAL:
        assert ALLOWED_TRANSITIONS[state] == frozenset(), state


def test_no_state_transitions_to_itself() -> None:
    for state, targets in ALLOWED_TRANSITIONS.items():
        assert state not in targets, state


def test_non_terminal_states_can_reach_a_terminal_state() -> None:
    reachable = {state for state in RunState if state in TERMINAL}
    changed = True
    while changed:
        changed = False
        for state, targets in ALLOWED_TRANSITIONS.items():
            if state not in reachable and targets & reachable:
                reachable.add(state)
                changed = True
    assert reachable == set(RunState)


# --- Compare-and-set order: attempt, then version, then terminal, then table ----------------


def test_wrong_attempt_on_a_terminal_run_is_stale_attempt_first() -> None:
    record = run_record(state=RunState.SUCCEEDED, version=3, attempt=1)
    code = refusal_code(
        lambda: transition(
            record, to=RunState.RUNNING, expected_version=99, expected_attempt=2, now=EPOCH
        )
    )
    assert code is ErrorCode.STALE_ATTEMPT


def test_wrong_version_on_a_terminal_run_is_version_conflict() -> None:
    record = run_record(state=RunState.SUCCEEDED, version=3, attempt=1)
    code = refusal_code(
        lambda: transition(
            record, to=RunState.RUNNING, expected_version=2, expected_attempt=1, now=EPOCH
        )
    )
    assert code is ErrorCode.VERSION_CONFLICT


def test_matching_terminal_run_is_terminal_state() -> None:
    record = run_record(state=RunState.SUCCEEDED, version=3, attempt=1)
    code = refusal_code(
        lambda: transition(
            record, to=RunState.RUNNING, expected_version=3, expected_attempt=1, now=EPOCH
        )
    )
    assert code is ErrorCode.TERMINAL_STATE


def test_matching_non_terminal_run_off_the_table_is_illegal_transition() -> None:
    record = run_record(state=RunState.QUEUED, version=1, attempt=1)
    code = refusal_code(
        lambda: transition(
            record, to=RunState.COLLECTING, expected_version=1, expected_attempt=1, now=EPOCH
        )
    )
    assert code is ErrorCode.ILLEGAL_TRANSITION


def test_transition_bumps_version_and_stamps_entry_time() -> None:
    record = run_record(state=RunState.QUEUED, version=1, attempt=1)
    later = EPOCH + timedelta(seconds=5)
    moved = transition(
        record, to=RunState.DISPATCHING, expected_version=1, expected_attempt=1, now=later
    )
    assert moved.state is RunState.DISPATCHING
    assert moved.version == 2
    assert moved.state_entered_at == later


def test_failure_on_a_non_terminal_target_is_a_value_error() -> None:
    record = run_record(state=RunState.QUEUED, version=1, attempt=1)
    with pytest.raises(ValueError, match="failure may only accompany"):
        transition(
            record,
            to=RunState.DISPATCHING,
            expected_version=1,
            expected_attempt=1,
            now=EPOCH,
            failure=failure(FailureCode.WORKER_FAILED),
        )


def test_failure_survives_cancel_requested_to_cancelled() -> None:
    cancelling, changed = request_cancel(_running(), now=EPOCH + timedelta(seconds=1))
    assert changed is True
    cancelled = transition(
        cancelling,
        to=RunState.CANCELLED,
        expected_version=cancelling.version,
        expected_attempt=1,
        now=EPOCH + timedelta(seconds=2),
    )
    assert cancelled.failure is not None
    assert cancelled.failure.code is FailureCode.OWNER_CANCELLED


def test_non_failure_transition_clears_a_stale_failure() -> None:
    record = run_record(
        state=RunState.DISPATCHING,
        version=2,
        claim_id=CLAIM_ID,
        failure=failure(FailureCode.LAUNCH_UNCERTAIN),
    )
    moved = transition(
        record, to=RunState.STARTING, expected_version=2, expected_attempt=1, now=EPOCH
    )
    assert moved.failure is None


# --- Failure codes and redaction ----------------------------------------------------------------


def test_retryable_follows_the_failure_code_table() -> None:
    for code, retryable in RETRYABLE.items():
        assert failure(code).retryable is retryable, code


def test_typed_failure_rejects_a_retryable_flag_that_contradicts_its_code() -> None:
    with pytest.raises(ValidationError, match="retryable must follow the failure code"):
        TypedFailure(code=FailureCode.HEARTBEAT_LOST, retryable=False, message="")


def test_failure_message_is_redacted() -> None:
    built = failure(FailureCode.WORKER_FAILED, "token=abc123 failed")
    assert "abc123" not in built.message
    assert "[REDACTED]" in built.message


def test_failure_message_is_truncated_to_the_status_limit() -> None:
    built = failure(FailureCode.WORKER_FAILED, "x" * 1000)
    assert len(built.message) == 512


# --- admit_start --------------------------------------------------------------------------------


def _awaiting() -> Any:
    return run_record(state=RunState.AWAITING_UPLOAD, version=1, last_seen_at=None)


def test_admit_start_queues_the_run_and_records_its_receipt() -> None:
    record = _awaiting()
    queued = admit_start(
        record,
        _start_request(record),
        observed=_observed(),
        compatibility=tool_compatibility(),
        now=EPOCH + timedelta(seconds=1),
    )
    assert queued.state is RunState.QUEUED
    assert queued.version == record.version + 1
    assert queued.receipt is not None
    assert queued.receipt.source.generation == 1
    assert queued.receipt.source.sha256 == sha256_hex(SOURCE_TAR)


def test_admit_start_refuses_a_different_manifest_digest() -> None:
    record = _awaiting()
    request = parse_start_request(start_payload({"canonical_digest": "0" * 64}, generation=1))
    code = refusal_code(
        lambda: admit_start(
            record,
            request,
            observed=_observed(),
            compatibility=tool_compatibility(),
            now=EPOCH,
        )
    )
    assert code is ErrorCode.REQUEST_DIGEST_CONFLICT


def test_admit_start_refuses_a_run_that_is_not_awaiting_upload() -> None:
    record = run_record(state=RunState.QUEUED, version=1)
    code = refusal_code(
        lambda: admit_start(
            record,
            _start_request(record),
            observed=_observed(),
            compatibility=tool_compatibility(),
            now=EPOCH,
        )
    )
    assert code is ErrorCode.ILLEGAL_TRANSITION


def test_admit_start_refuses_after_the_upload_window_closes() -> None:
    record = _awaiting()
    code = refusal_code(
        lambda: admit_start(
            record,
            _start_request(record),
            observed=_observed(),
            compatibility=tool_compatibility(),
            now=EPOCH + timedelta(seconds=600),
        )
    )
    assert code is ErrorCode.DEADLINE_EXCEEDED


def test_admit_start_refuses_source_bytes_that_differ_from_the_manifest() -> None:
    record = _awaiting()
    other = b"different bytes entirely"
    code = refusal_code(
        lambda: admit_start(
            record,
            _start_request(record, source=other),
            observed=_observed(sha256=sha256_hex(other), size_bytes=len(other)),
            compatibility=tool_compatibility(),
            now=EPOCH,
        )
    )
    assert code is ErrorCode.SOURCE_MISMATCH


def test_admit_start_refuses_when_the_store_has_no_object() -> None:
    record = _awaiting()
    code = refusal_code(
        lambda: admit_start(
            record,
            _start_request(record),
            observed=None,
            compatibility=tool_compatibility(),
            now=EPOCH,
        )
    )
    assert code is ErrorCode.UPLOAD_MISMATCH


@pytest.mark.parametrize(
    "observed",
    [
        _observed(sha256="0" * 64),
        _observed(size_bytes=len(SOURCE_TAR) + 1),
        _observed(generation=2),
    ],
    ids=["checksum", "size", "generation"],
)
def test_admit_start_refuses_store_objects_that_differ_from_the_upload(observed: ObservedInput) -> None:
    record = _awaiting()
    code = refusal_code(
        lambda: admit_start(
            record,
            _start_request(record),
            observed=observed,
            compatibility=tool_compatibility(),
            now=EPOCH,
        )
    )
    assert code is ErrorCode.UPLOAD_MISMATCH


# --- replay_start -------------------------------------------------------------------------------


def _queued_with_receipt() -> Any:
    record = _awaiting()
    return admit_start(
        record,
        _start_request(record),
        observed=_observed(),
        compatibility=tool_compatibility(),
        now=EPOCH + timedelta(seconds=1),
    )


def test_replay_start_returns_the_recorded_receipt() -> None:
    queued = _queued_with_receipt()
    assert replay_start(queued, _start_request(queued)) == queued.receipt


def test_replay_start_before_any_start_is_illegal() -> None:
    record = _awaiting()
    code = refusal_code(lambda: replay_start(record, _start_request(record)))
    assert code is ErrorCode.ILLEGAL_TRANSITION


def test_replay_start_with_a_different_digest_conflicts() -> None:
    queued = _queued_with_receipt()
    request = parse_start_request(start_payload({"canonical_digest": "0" * 64}, generation=1))
    code = refusal_code(lambda: replay_start(queued, request))
    assert code is ErrorCode.REQUEST_DIGEST_CONFLICT


def test_replay_start_with_a_different_generation_is_source_mismatch() -> None:
    queued = _queued_with_receipt()
    code = refusal_code(lambda: replay_start(queued, _start_request(queued, generation=2)))
    assert code is ErrorCode.SOURCE_MISMATCH


def test_replay_start_with_different_bytes_is_source_mismatch() -> None:
    queued = _queued_with_receipt()
    code = refusal_code(
        lambda: replay_start(queued, _start_request(queued, source=b"other bytes here"))
    )
    assert code is ErrorCode.SOURCE_MISMATCH


# --- begin_dispatch, accept_claim ---------------------------------------------------------------


def test_begin_dispatch_moves_to_dispatching_and_binds_the_claim() -> None:
    queued = run_record(state=RunState.QUEUED, version=1)
    dispatching = begin_dispatch(
        queued, claim_id=CLAIM_ID, expected_version=1, expected_attempt=1, now=EPOCH
    )
    assert dispatching.state is RunState.DISPATCHING
    assert dispatching.claim_id == CLAIM_ID
    assert dispatching.version == 2


def test_begin_dispatch_with_a_stale_version_conflicts() -> None:
    queued = run_record(state=RunState.QUEUED, version=1)
    code = refusal_code(
        lambda: begin_dispatch(
            queued, claim_id=CLAIM_ID, expected_version=0, expected_attempt=1, now=EPOCH
        )
    )
    assert code is ErrorCode.VERSION_CONFLICT


def test_accept_claim_moves_dispatching_to_starting_and_records_liveness() -> None:
    dispatching = run_record(state=RunState.DISPATCHING, version=2, claim_id=CLAIM_ID)
    later = EPOCH + timedelta(seconds=3)
    started = accept_claim(dispatching, _claim(), now=later)
    assert started.state is RunState.STARTING
    assert started.last_seen_at == later


def test_accept_claim_revives_a_reconciling_run() -> None:
    reconciling = run_record(state=RunState.RECONCILING, version=5, claim_id=CLAIM_ID)
    assert accept_claim(reconciling, _claim(), now=EPOCH).state is RunState.STARTING


def test_accept_claim_from_running_is_illegal() -> None:
    code = refusal_code(lambda: accept_claim(_running(), _claim(), now=EPOCH))
    assert code is ErrorCode.ILLEGAL_TRANSITION


def test_accept_claim_for_another_attempt_is_stale() -> None:
    dispatching = run_record(state=RunState.DISPATCHING, version=2, claim_id=CLAIM_ID)
    code = refusal_code(lambda: accept_claim(dispatching, _claim(attempt=2), now=EPOCH))
    assert code is ErrorCode.STALE_ATTEMPT


def test_accept_claim_with_no_dispatch_on_record_is_stale() -> None:
    queued = run_record(state=RunState.QUEUED, version=1, claim_id=None)
    code = refusal_code(lambda: accept_claim(queued, _claim(), now=EPOCH))
    assert code is ErrorCode.STALE_ATTEMPT


# --- accept_status -----------------------------------------------------------------------------


def test_status_on_starting_run_moves_it_to_running() -> None:
    starting = run_record(state=RunState.STARTING, version=3, claim_id=CLAIM_ID)
    updated, verdict = accept_status(starting, _status(seq=1), now=EPOCH + timedelta(seconds=1), skew_seconds=SKEW)
    assert updated.state is RunState.RUNNING
    assert verdict is TimestampVerdict.ACCEPTED
    assert updated.last_status_seq == 1
    assert updated.last_seen_at == EPOCH + timedelta(seconds=1)


def test_status_advances_version_on_each_accepted_report() -> None:
    running = _running()
    updated, _ = accept_status(running, _status(seq=1), now=EPOCH + timedelta(seconds=1), skew_seconds=SKEW)
    assert updated.state is RunState.RUNNING
    assert updated.version == running.version + 1


def test_repeated_seq_is_an_idempotent_replay() -> None:
    running = _running(last_status_seq=2, last_log_watermark=0)
    returned, verdict = accept_status(
        running, _status(seq=2), now=EPOCH + timedelta(seconds=5), skew_seconds=SKEW
    )
    assert returned is running
    assert verdict is TimestampVerdict.ACCEPTED


def test_status_seq_regression_is_refused() -> None:
    running = _running(last_status_seq=5)
    code = refusal_code(
        lambda: accept_status(running, _status(seq=4), now=EPOCH, skew_seconds=SKEW)
    )
    assert code is ErrorCode.SEQUENCE_REGRESSED


def test_log_watermark_regression_is_refused() -> None:
    running = _running(last_status_seq=1, last_log_watermark=3)
    code = refusal_code(
        lambda: accept_status(
            running, _status(seq=2, log_watermark=2), now=EPOCH, skew_seconds=SKEW
        )
    )
    assert code is ErrorCode.SEQUENCE_REGRESSED


def test_status_for_another_fence_is_stale() -> None:
    code = refusal_code(
        lambda: accept_status(_running(), _status(fence=2), now=EPOCH, skew_seconds=SKEW)
    )
    assert code is ErrorCode.STALE_ATTEMPT


def test_status_for_another_claim_is_stale() -> None:
    code = refusal_code(
        lambda: accept_status(
            _running(), _status(claim_id="claim_" + "c" * 32), now=EPOCH, skew_seconds=SKEW
        )
    )
    assert code is ErrorCode.STALE_ATTEMPT


def test_status_on_a_terminal_run_is_terminal_state() -> None:
    succeeded = run_record(state=RunState.SUCCEEDED, version=6, claim_id=CLAIM_ID)
    code = refusal_code(
        lambda: accept_status(succeeded, _status(), now=EPOCH, skew_seconds=SKEW)
    )
    assert code is ErrorCode.TERMINAL_STATE


def test_status_on_a_dispatching_run_is_illegal() -> None:
    dispatching = run_record(state=RunState.DISPATCHING, version=2, claim_id=CLAIM_ID)
    code = refusal_code(
        lambda: accept_status(dispatching, _status(), now=EPOCH, skew_seconds=SKEW)
    )
    assert code is ErrorCode.ILLEGAL_TRANSITION


def test_cancel_requested_accepts_status_without_changing_state() -> None:
    cancelling = _running(
        state=RunState.CANCEL_REQUESTED,
        failure=failure(FailureCode.OWNER_CANCELLED),
    )
    updated, _ = accept_status(cancelling, _status(seq=1), now=EPOCH, skew_seconds=SKEW)
    assert updated.state is RunState.CANCEL_REQUESTED
    assert updated.failure == cancelling.failure
    assert updated.version == cancelling.version + 1


def test_future_observed_at_is_recorded_as_future_and_not_trusted() -> None:
    running = _running()
    future = EPOCH + timedelta(minutes=10)
    updated, verdict = accept_status(
        running, _status(seq=1, observed_at=future), now=EPOCH, skew_seconds=SKEW
    )
    assert verdict is TimestampVerdict.FUTURE
    assert updated.last_observed_at is None
    assert updated.last_status_seq == 1


def test_backward_observed_at_keeps_the_previous_observation() -> None:
    previous = EPOCH + timedelta(seconds=10)
    running = _running(last_observed_at=previous)
    updated, verdict = accept_status(
        running,
        _status(seq=1, observed_at=EPOCH + timedelta(seconds=5)),
        now=EPOCH + timedelta(seconds=11),
        skew_seconds=SKEW,
    )
    assert verdict is TimestampVerdict.BACKWARD
    assert updated.last_observed_at == previous


def test_accepted_observed_at_is_recorded() -> None:
    observed = EPOCH + timedelta(seconds=1)
    updated, verdict = accept_status(
        _running(),
        _status(seq=1, observed_at=observed),
        now=EPOCH + timedelta(seconds=2),
        skew_seconds=SKEW,
    )
    assert verdict is TimestampVerdict.ACCEPTED
    assert updated.last_observed_at == observed


def test_collecting_status_from_running_moves_to_collecting() -> None:
    updated, _ = accept_status(
        _running(), _status(seq=1, phase="collecting"), now=EPOCH, skew_seconds=SKEW
    )
    assert updated.state is RunState.COLLECTING


# --- accept_result -----------------------------------------------------------------------------


def test_succeeded_result_from_running_passes_through_collecting() -> None:
    running = _running()
    done = accept_result(running, _result("succeeded"), now=EPOCH + timedelta(seconds=10))
    assert done.state is RunState.SUCCEEDED
    assert done.version == running.version + 2
    assert done.failure is None


def test_failed_result_records_a_retryable_worker_failure() -> None:
    done = accept_result(_running(), _result("failed"), now=EPOCH + timedelta(seconds=10))
    assert done.state is RunState.FAILED
    assert done.failure is not None
    assert done.failure.code is FailureCode.WORKER_FAILED
    assert done.failure.retryable is True


@pytest.mark.parametrize(
    "state",
    [RunState.CANCEL_REQUESTED, RunState.CANCELLED, RunState.TIMED_OUT],
)
def test_result_is_fenced_by_cancellation_or_timeout(state: RunState) -> None:
    fenced = _running(state=state)
    code = refusal_code(lambda: accept_result(fenced, _result(), now=EPOCH))
    assert code is ErrorCode.RESULT_FENCED


@pytest.mark.parametrize("state", [RunState.SUCCEEDED, RunState.FAILED])
def test_second_result_on_a_finished_attempt_is_terminal_state(state: RunState) -> None:
    done = _running(state=state, failure=failure(FailureCode.WORKER_FAILED) if state is RunState.FAILED else None)
    code = refusal_code(lambda: accept_result(done, _result(), now=EPOCH))
    assert code is ErrorCode.TERMINAL_STATE


def test_result_after_the_deadline_is_refused() -> None:
    code = refusal_code(lambda: accept_result(_running(), _result(), now=DEADLINE))
    assert code is ErrorCode.DEADLINE_EXCEEDED


def test_result_for_another_fence_is_stale() -> None:
    code = refusal_code(lambda: accept_result(_running(), _result(fence=2), now=EPOCH))
    assert code is ErrorCode.STALE_ATTEMPT


def test_result_with_no_claim_on_record_is_stale() -> None:
    queued = run_record(state=RunState.QUEUED, version=1, claim_id=None)
    code = refusal_code(lambda: accept_result(queued, _result(), now=EPOCH))
    assert code is ErrorCode.STALE_ATTEMPT


def test_result_for_another_attempt_is_stale() -> None:
    code = refusal_code(lambda: accept_result(_running(), _result(attempt=2), now=EPOCH))
    assert code is ErrorCode.STALE_ATTEMPT


def test_result_for_another_claim_is_stale() -> None:
    code = refusal_code(
        lambda: accept_result(_running(), _result(claim_id="claim_" + "c" * 32), now=EPOCH)
    )
    assert code is ErrorCode.STALE_ATTEMPT


# --- request_cancel ----------------------------------------------------------------------------


@pytest.mark.parametrize("state", [RunState.AWAITING_UPLOAD, RunState.QUEUED])
def test_owner_cancel_before_dispatch_cancels_immediately(state: RunState) -> None:
    record = run_record(state=state, version=1)
    cancelled, changed = request_cancel(record, now=EPOCH + timedelta(seconds=1))
    assert changed is True
    assert cancelled.state is RunState.CANCELLED
    assert cancelled.failure is not None
    assert cancelled.failure.code is FailureCode.OWNER_CANCELLED


def test_owner_cancel_of_a_running_attempt_requests_cancellation() -> None:
    cancelling, changed = request_cancel(_running(), now=EPOCH + timedelta(seconds=1))
    assert changed is True
    assert cancelling.state is RunState.CANCEL_REQUESTED
    assert cancelling.failure is not None
    assert cancelling.failure.code is FailureCode.OWNER_CANCELLED


def test_repeated_owner_cancel_is_idempotent() -> None:
    cancelling, _ = request_cancel(_running(), now=EPOCH)
    again, changed = request_cancel(cancelling, now=EPOCH + timedelta(seconds=1))
    assert changed is False
    assert again is cancelling


@pytest.mark.parametrize("state", [RunState.SUCCEEDED, RunState.FAILED, RunState.TIMED_OUT])
def test_owner_cancel_of_a_terminal_run_changes_nothing(state: RunState) -> None:
    done = run_record(state=state, version=6, claim_id=CLAIM_ID)
    unchanged, changed = request_cancel(done, now=EPOCH)
    assert changed is False
    assert unchanged is done


# --- retry -------------------------------------------------------------------------------------


def _failed(code: FailureCode = FailureCode.HEARTBEAT_LOST, **overrides: Any) -> Any:
    values: dict[str, Any] = {
        "state": RunState.FAILED,
        "claim_id": CLAIM_ID,
        "version": 5,
        "attempt": 1,
        "fence": 1,
        "failure": failure(code),
        "last_seen_at": EPOCH,
        "last_status_seq": 4,
        "last_log_watermark": 4,
        "log_retained_from": 3,
    }
    values.update(overrides)
    return run_record(**values)


def test_retry_of_a_retryable_failure_opens_the_next_attempt() -> None:
    failed = _failed()
    request_uuid = uuid7(EPOCH + timedelta(seconds=30))
    queued = retry(
        failed,
        request_uuid=request_uuid,
        expected_version=5,
        expected_attempt=1,
        now=EPOCH + timedelta(seconds=60),
    )
    assert queued.state is RunState.QUEUED
    assert queued.attempt == 2
    assert queued.fence == 2
    assert queued.version == 6
    assert queued.claim_id is None
    assert queued.failure is None
    assert queued.retry_request_uuids == (request_uuid,)
    assert queued.last_seen_at is None
    assert queued.last_observed_at is None
    assert queued.last_status_seq == 0
    assert queued.last_log_watermark == 0
    assert queued.log_retained_from == 1


def test_retry_keeps_the_original_deadline() -> None:
    queued = retry(
        _failed(),
        request_uuid=uuid7(EPOCH),
        expected_version=5,
        expected_attempt=1,
        now=EPOCH + timedelta(seconds=60),
    )
    assert queued.deadline_at == DEADLINE


def test_retry_of_a_non_retryable_failure_is_refused() -> None:
    code = refusal_code(
        lambda: retry(
            _failed(FailureCode.CHECKSUM_MISMATCH),
            request_uuid=uuid7(EPOCH),
            expected_version=5,
            expected_attempt=1,
            now=EPOCH,
        )
    )
    assert code is ErrorCode.RETRY_NOT_ALLOWED


def test_retry_of_a_run_that_is_not_failed_is_refused() -> None:
    code = refusal_code(
        lambda: retry(
            _running(version=5),
            request_uuid=uuid7(EPOCH),
            expected_version=5,
            expected_attempt=1,
            now=EPOCH,
        )
    )
    assert code is ErrorCode.RETRY_NOT_ALLOWED


def test_retry_of_a_failed_run_with_no_recorded_failure_is_refused() -> None:
    failed_without_reason = run_record(state=RunState.FAILED, version=5, attempt=1, claim_id=CLAIM_ID)
    code = refusal_code(
        lambda: retry(
            failed_without_reason,
            request_uuid=uuid7(EPOCH),
            expected_version=5,
            expected_attempt=1,
            now=EPOCH,
        )
    )
    assert code is ErrorCode.RETRY_NOT_ALLOWED


def test_retry_after_the_deadline_is_refused() -> None:
    code = refusal_code(
        lambda: retry(
            _failed(),
            request_uuid=uuid7(EPOCH),
            expected_version=5,
            expected_attempt=1,
            now=DEADLINE,
        )
    )
    assert code is ErrorCode.DEADLINE_EXCEEDED


def test_retry_past_the_attempt_budget_is_refused() -> None:
    code = refusal_code(
        lambda: retry(
            _failed(attempt=3, fence=3),
            request_uuid=uuid7(EPOCH),
            expected_version=5,
            expected_attempt=3,
            now=EPOCH,
        )
    )
    assert code is ErrorCode.RETRY_NOT_ALLOWED


def test_retry_with_a_stale_version_conflicts() -> None:
    code = refusal_code(
        lambda: retry(
            _failed(),
            request_uuid=uuid7(EPOCH),
            expected_version=4,
            expected_attempt=1,
            now=EPOCH,
        )
    )
    assert code is ErrorCode.VERSION_CONFLICT


def test_retry_with_a_stale_attempt_is_stale_even_when_the_version_also_differs() -> None:
    code = refusal_code(
        lambda: retry(
            _failed(),
            request_uuid=uuid7(EPOCH),
            expected_version=0,
            expected_attempt=2,
            now=EPOCH,
        )
    )
    assert code is ErrorCode.STALE_ATTEMPT


@pytest.mark.parametrize(
    "state", [state for state in RunState if state is not RunState.FAILED], ids=lambda state: state.value
)
def test_retry_is_refused_from_every_state_but_failed_even_with_matching_cas(state: RunState) -> None:
    # The expected version and attempt match the record, so only the state gate can refuse.
    record = run_record(state=state, claim_id=CLAIM_ID, version=4, last_seen_at=EPOCH)
    code = refusal_code(
        lambda: retry(
            record,
            request_uuid=uuid7(EPOCH),
            expected_version=record.version,
            expected_attempt=record.attempt,
            now=EPOCH,
        )
    )
    assert code is ErrorCode.RETRY_NOT_ALLOWED

