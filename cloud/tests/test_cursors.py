"""Log cursors: opaque token encoding, scope checks, and page planning across trimmed history."""

import base64
from typing import Any

import pytest

from fakes import RUN_ID, refusal_code
from mao_cloud.protocol import (
    MAX_LOG_LIMIT,
    Cursor,
    CursorStream,
    ErrorCode,
    canonical_bytes,
    decode_cursor,
    encode_cursor,
    plan_page,
)

ATTEMPT = 1


def _cursor(**overrides: Any) -> Cursor:
    values: dict[str, Any] = {
        "run_id": RUN_ID,
        "attempt": ATTEMPT,
        "stream": CursorStream.LOGS,
        "seq": 7,
    }
    values.update(overrides)
    return Cursor(**values)


def _raw_token(payload: Any) -> str:
    """Encode an arbitrary payload exactly as encode_cursor does, to build malformed cursors."""
    return base64.urlsafe_b64encode(canonical_bytes(payload)).rstrip(b"=").decode("ascii")


def _good_payload(**overrides: Any) -> dict[str, Any]:
    payload: dict[str, Any] = {"v": 1, "r": RUN_ID, "a": ATTEMPT, "s": "logs", "q": 7}
    payload.update(overrides)
    return payload


# --- Encoding -----------------------------------------------------------------------------------


def test_cursor_round_trips_through_its_token() -> None:
    original = _cursor()
    token = encode_cursor(original)
    assert decode_cursor(token, run_id=RUN_ID, stream=CursorStream.LOGS) == original


def test_encoding_is_deterministic() -> None:
    assert encode_cursor(_cursor()) == encode_cursor(_cursor())


def test_token_is_urlsafe_base64_without_padding() -> None:
    token = encode_cursor(_cursor())
    assert "=" not in token
    assert "+" not in token
    assert "/" not in token
    assert set(token) <= set("ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789-_")


def test_token_payload_is_canonical_json_with_short_keys() -> None:
    token = encode_cursor(_cursor())
    decoded = base64.urlsafe_b64decode(token + "=" * (-len(token) % 4))
    assert decoded == canonical_bytes({"v": 1, "r": RUN_ID, "a": ATTEMPT, "s": "logs", "q": 7})


# --- Refusals -----------------------------------------------------------------------------------


def test_cursor_for_another_run_is_invalid() -> None:
    token = encode_cursor(_cursor())
    code = refusal_code(
        lambda: decode_cursor(token, run_id="run_" + "c" * 32, stream=CursorStream.LOGS)
    )
    assert code is ErrorCode.CURSOR_INVALID


def test_unsupported_cursor_version_names_the_version_not_a_generic_failure() -> None:
    token = _raw_token(_good_payload(v=2))
    code = refusal_code(lambda: decode_cursor(token, run_id=RUN_ID, stream=CursorStream.LOGS))
    assert code is ErrorCode.CURSOR_VERSION_UNSUPPORTED


def test_encode_with_a_future_version_produces_an_unsupported_token() -> None:
    token = encode_cursor(_cursor(), version=2)
    code = refusal_code(lambda: decode_cursor(token, run_id=RUN_ID, stream=CursorStream.LOGS))
    assert code is ErrorCode.CURSOR_VERSION_UNSUPPORTED


def test_non_object_payload_is_invalid() -> None:
    token = _raw_token([1, 2, 3])
    code = refusal_code(lambda: decode_cursor(token, run_id=RUN_ID, stream=CursorStream.LOGS))
    assert code is ErrorCode.CURSOR_INVALID


@pytest.mark.parametrize("missing", ["r", "a", "s", "q"])
def test_payload_missing_a_key_is_invalid(missing: str) -> None:
    payload = _good_payload()
    del payload[missing]
    token = _raw_token(payload)
    code = refusal_code(lambda: decode_cursor(token, run_id=RUN_ID, stream=CursorStream.LOGS))
    assert code is ErrorCode.CURSOR_INVALID


def test_unknown_stream_name_is_invalid() -> None:
    token = _raw_token(_good_payload(s="status"))
    code = refusal_code(lambda: decode_cursor(token, run_id=RUN_ID, stream=CursorStream.LOGS))
    assert code is ErrorCode.CURSOR_INVALID


def test_sequence_zero_is_invalid() -> None:
    token = _raw_token(_good_payload(q=0))
    code = refusal_code(lambda: decode_cursor(token, run_id=RUN_ID, stream=CursorStream.LOGS))
    assert code is ErrorCode.CURSOR_INVALID


def test_attempt_zero_is_invalid() -> None:
    token = _raw_token(_good_payload(a=0))
    code = refusal_code(lambda: decode_cursor(token, run_id=RUN_ID, stream=CursorStream.LOGS))
    assert code is ErrorCode.CURSOR_INVALID


@pytest.mark.parametrize("token", ["!!!", "%%%%", "not a cursor at all"])
def test_malformed_token_is_invalid(token: str) -> None:
    code = refusal_code(lambda: decode_cursor(token, run_id=RUN_ID, stream=CursorStream.LOGS))
    assert code is ErrorCode.CURSOR_INVALID


# --- Page planning ------------------------------------------------------------------------------


def test_normal_page_covers_the_requested_range() -> None:
    plan = plan_page(requested_seq=1, retained_from=1, watermark=10, limit=4)
    assert (plan.first_seq, plan.last_seq, plan.next_seq) == (1, 4, 5)
    assert plan.trimmed is False
    assert plan.gap is None


def test_page_is_bounded_by_the_watermark() -> None:
    plan = plan_page(requested_seq=8, retained_from=1, watermark=10, limit=5)
    assert (plan.first_seq, plan.last_seq, plan.next_seq) == (8, 10, 11)


def test_cursor_at_the_watermark_plus_one_yields_an_empty_page_that_stays_put() -> None:
    plan = plan_page(requested_seq=11, retained_from=1, watermark=10, limit=5)
    assert plan.last_seq < plan.first_seq
    assert plan.next_seq == 11


def test_cursor_beyond_the_watermark_plus_one_is_invalid() -> None:
    code = refusal_code(
        lambda: plan_page(requested_seq=12, retained_from=1, watermark=10, limit=5)
    )
    assert code is ErrorCode.CURSOR_INVALID


def test_cursor_into_trimmed_history_discloses_the_gap() -> None:
    plan = plan_page(requested_seq=2, retained_from=5, watermark=10, limit=3)
    assert plan.trimmed is True
    assert plan.gap == (2, 4)
    assert (plan.first_seq, plan.last_seq, plan.next_seq) == (5, 7, 8)


def test_cursor_at_the_first_retained_entry_is_not_trimmed() -> None:
    plan = plan_page(requested_seq=5, retained_from=5, watermark=10, limit=3)
    assert plan.trimmed is False
    assert plan.gap is None


@pytest.mark.parametrize("limit", [0, MAX_LOG_LIMIT + 1])
def test_limit_outside_the_allowed_range_is_refused(limit: int) -> None:
    code = refusal_code(
        lambda: plan_page(requested_seq=1, retained_from=1, watermark=10, limit=limit)
    )
    assert code is ErrorCode.CURSOR_INVALID


def test_limit_at_the_maximum_is_accepted() -> None:
    plan = plan_page(requested_seq=1, retained_from=1, watermark=1000, limit=MAX_LOG_LIMIT)
    assert plan.last_seq == MAX_LOG_LIMIT


def test_requested_sequence_zero_is_refused() -> None:
    code = refusal_code(
        lambda: plan_page(requested_seq=0, retained_from=1, watermark=10, limit=5)
    )
    assert code is ErrorCode.CURSOR_INVALID
