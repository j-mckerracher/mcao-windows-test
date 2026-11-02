"""Idempotency admission: create versus replay, expiry, orphans, clock skew, and the restore floor."""

import uuid
from datetime import timedelta
from typing import Any

import pytest

from fakes import EPOCH, RUN_ID, uuid7
from mao_cloud.protocol import (
    IDEMPOTENCY_TTL,
    Admission,
    ErrorCode,
    IdempotencyRecord,
    ProtocolError,
    decide_admission,
    effective_admission_floor,
    new_idempotency_record,
    uuid7_timestamp,
)

OWNER = "owner-a"
DIGEST = "a" * 64
SKEW = 30
NOW = EPOCH + timedelta(days=1)


def _existing(**overrides: Any) -> IdempotencyRecord:
    values: dict[str, Any] = {
        "owner_id": OWNER,
        "request_uuid": uuid7(EPOCH),
        "digest": DIGEST,
        "run_id": RUN_ID,
        "created_at": EPOCH,
        "expires_at": EPOCH + IDEMPOTENCY_TTL,
    }
    values.update(overrides)
    return IdempotencyRecord(**values)


def _decide(**overrides: Any) -> Admission:
    values: dict[str, Any] = {
        "existing": None,
        "orphan_run_id": None,
        "request_uuid": uuid7(NOW),
        "digest": DIGEST,
        "now": NOW,
        "admission_floor": effective_admission_floor(now=NOW, restore_floor=None),
        "skew_seconds": SKEW,
    }
    values.update(overrides)
    return decide_admission(**values)


def _refusal(**overrides: Any) -> ProtocolError:
    try:
        _decide(**overrides)
    except ProtocolError as exc:
        return exc
    raise AssertionError("expected a ProtocolError, but admission was granted")


# --- UUIDv7 timestamps --------------------------------------------------------------------------


def test_uuid7_timestamp_round_trips_the_stamped_instant() -> None:
    assert uuid7_timestamp(uuid7(EPOCH)) == EPOCH


def test_uuid7_timestamp_is_millisecond_precise() -> None:
    at = EPOCH + timedelta(milliseconds=123)
    assert uuid7_timestamp(uuid7(at)) == at


@pytest.mark.parametrize("value", [uuid.uuid4(), uuid.UUID(int=0)], ids=["v4", "nil"])
def test_non_v7_request_uuid_is_invalid_request(value: uuid.UUID) -> None:
    with pytest.raises(ProtocolError) as caught:
        uuid7_timestamp(value)
    assert caught.value.code is ErrorCode.INVALID_REQUEST


# --- Admission floor ----------------------------------------------------------------------------


def test_admission_floor_is_the_ttl_window_without_a_restore_floor() -> None:
    assert effective_admission_floor(now=NOW, restore_floor=None) == NOW - IDEMPOTENCY_TTL


def test_a_later_restore_floor_raises_the_admission_floor() -> None:
    restored_at = NOW - timedelta(days=2)
    assert effective_admission_floor(now=NOW, restore_floor=restored_at) == restored_at


def test_an_earlier_restore_floor_does_not_lower_the_window() -> None:
    old_restore = NOW - timedelta(days=30)
    assert effective_admission_floor(now=NOW, restore_floor=old_restore) == NOW - IDEMPOTENCY_TTL


def test_admission_floor_honours_a_custom_ttl() -> None:
    floor = effective_admission_floor(now=NOW, restore_floor=None, ttl=timedelta(hours=1))
    assert floor == NOW - timedelta(hours=1)


def test_idempotency_ttl_is_seven_days() -> None:
    assert IDEMPOTENCY_TTL == timedelta(days=7)


# --- New dedup records --------------------------------------------------------------------------


def test_new_record_expires_seven_days_after_creation() -> None:
    record = new_idempotency_record(
        owner_id=OWNER,
        request_uuid=uuid7(NOW),
        digest=DIGEST,
        run_id=RUN_ID,
        now=NOW,
    )
    assert record.created_at == NOW
    assert record.expires_at == NOW + timedelta(days=7)
    assert record.run_id == RUN_ID
    assert record.digest == DIGEST
    assert record.owner_id == OWNER


# --- decide_admission: each branch and the precedence between them ------------------------------


def test_fresh_request_in_the_window_creates() -> None:
    assert _decide() is Admission.CREATE


def test_same_digest_before_expiry_replays() -> None:
    existing = _existing(expires_at=NOW + timedelta(seconds=1))
    assert _decide(existing=existing) is Admission.REPLAY


def test_same_digest_at_expiry_is_idempotency_expired_by_ttl() -> None:
    existing = _existing(expires_at=NOW)
    error = _refusal(existing=existing)
    assert error.code is ErrorCode.IDEMPOTENCY_EXPIRED
    assert error.detail["reason"] == "ttl"
    assert error.detail["run_id"] == RUN_ID


def test_different_digest_conflicts_even_when_the_record_has_expired() -> None:
    existing = _existing(expires_at=NOW - timedelta(seconds=1))
    error = _refusal(existing=existing, digest="b" * 64)
    assert error.code is ErrorCode.REQUEST_DIGEST_CONFLICT
    assert error.detail["run_id"] == RUN_ID


def test_different_digest_conflicts_on_a_live_record() -> None:
    existing = _existing(expires_at=NOW + timedelta(days=1))
    assert _refusal(existing=existing, digest="b" * 64).code is ErrorCode.REQUEST_DIGEST_CONFLICT


def test_orphan_run_without_a_dedup_record_is_refused_not_recreated() -> None:
    error = _refusal(orphan_run_id=RUN_ID)
    assert error.code is ErrorCode.IDEMPOTENCY_EXPIRED
    assert error.detail["reason"] == "record_missing_run_present"
    assert error.detail["run_id"] == RUN_ID


def test_a_live_dedup_record_wins_over_an_orphan_marker() -> None:
    existing = _existing(expires_at=NOW + timedelta(days=1))
    assert _decide(existing=existing, orphan_run_id=RUN_ID) is Admission.REPLAY


def test_request_stamped_ahead_of_server_time_beyond_skew_is_refused() -> None:
    error = _refusal(request_uuid=uuid7(NOW + timedelta(seconds=SKEW + 1)))
    assert error.code is ErrorCode.INVALID_REQUEST


def test_request_stamped_exactly_at_the_skew_limit_is_admitted() -> None:
    assert _decide(request_uuid=uuid7(NOW + timedelta(seconds=SKEW))) is Admission.CREATE


def test_request_stamped_before_the_admission_floor_is_expired() -> None:
    error = _refusal(request_uuid=uuid7(NOW - IDEMPOTENCY_TTL - timedelta(seconds=1)))
    assert error.code is ErrorCode.IDEMPOTENCY_EXPIRED
    assert error.detail["reason"] == "before_admission_floor"


def test_request_stamped_exactly_at_the_admission_floor_is_admitted() -> None:
    assert _decide(request_uuid=uuid7(NOW - IDEMPOTENCY_TTL)) is Admission.CREATE


def test_request_before_a_restore_floor_is_expired_even_inside_the_ttl() -> None:
    restore_floor = NOW - timedelta(hours=1)
    floor = effective_admission_floor(now=NOW, restore_floor=restore_floor)
    error = _refusal(
        request_uuid=uuid7(NOW - timedelta(hours=2)),
        admission_floor=floor,
    )
    assert error.code is ErrorCode.IDEMPOTENCY_EXPIRED
    assert error.detail["reason"] == "before_admission_floor"


def test_restore_floor_does_not_block_a_request_stamped_after_it() -> None:
    restore_floor = NOW - timedelta(hours=1)
    floor = effective_admission_floor(now=NOW, restore_floor=restore_floor)
    assert (
        _decide(request_uuid=uuid7(NOW - timedelta(minutes=30)), admission_floor=floor)
        is Admission.CREATE
    )
