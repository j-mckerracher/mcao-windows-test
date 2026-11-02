"""Worker control gate: pre-model check and the 15-second cancel poll (AC4).

Fake reader only. The production reader and the in-flight poll during a model call are the runner
loop's job (protocol-v1.md gap 36); these tests pin the gate's decisions and cadence.
"""

from datetime import UTC, datetime, timedelta
from typing import Any

import pytest

from mao_cloud.protocol import CancelDocument, ControlDocument
from mao_cloud.worker_control import (
    CONTROL_POLL_SECONDS,
    AttemptIdentity,
    ControlGate,
    ControlRefused,
)

RUN_ID = "run_" + "a" * 32
CLAIM_ID = "claim_" + "b" * 32
OTHER_CLAIM_ID = "claim_" + "c" * 32
T0 = datetime(2026, 10, 23, 0, 0, tzinfo=UTC)
IDENTITY = AttemptIdentity(run_id=RUN_ID, attempt=1, fence=1, claim_id=CLAIM_ID)


def _control(**changes: Any) -> ControlDocument:
    fields: dict[str, Any] = {
        "protocol_version": "v1",
        "run_id": RUN_ID,
        "attempt": 1,
        "fence": 1,
        "claim_id": CLAIM_ID,
    }
    fields.update(changes)
    return ControlDocument.model_validate(fields)


def _cancel(**changes: Any) -> CancelDocument:
    fields: dict[str, Any] = {
        "protocol_version": "v1",
        "run_id": RUN_ID,
        "attempt": 1,
        "fence": 1,
        "claim_id": CLAIM_ID,
    }
    fields.update(changes)
    return CancelDocument.model_validate(fields)


class FakeReader:
    """Stands in for the runner's read-only capabilities. Counts reads; can fail one read once."""

    def __init__(
        self, control: ControlDocument | None, cancel: CancelDocument | None = None
    ) -> None:
        self.control = control
        self.cancel = cancel
        self.control_reads = 0
        self.cancel_reads = 0
        self.fail_next_control = False
        self.fail_next_cancel = False

    def read_control(self, run_id: str, attempt: int) -> ControlDocument | None:
        self.control_reads += 1
        if self.fail_next_control:
            self.fail_next_control = False
            raise OSError("storage unavailable")
        return self.control

    def read_cancel(self, run_id: str, attempt: int) -> CancelDocument | None:
        self.cancel_reads += 1
        if self.fail_next_cancel:
            self.fail_next_cancel = False
            raise OSError("storage unavailable")
        return self.cancel


def test_a_matching_attempt_with_no_cancel_may_call_the_model() -> None:
    reader = FakeReader(control=_control())
    gate = ControlGate(IDENTITY, reader)

    gate.before_model_call(T0)

    assert reader.control_reads == 1
    assert reader.cancel_reads == 1


def test_a_missing_control_object_refuses_and_never_reads_cancel() -> None:
    reader = FakeReader(control=None)
    gate = ControlGate(IDENTITY, reader)

    with pytest.raises(ControlRefused) as refused:
        gate.before_model_call(T0)

    assert refused.value.reason == "control_missing"
    assert reader.cancel_reads == 0


@pytest.mark.parametrize(
    "changes",
    [
        {"claim_id": OTHER_CLAIM_ID},
        {"fence": 2},
        {"attempt": 2},
        {"run_id": "run_" + "d" * 32},
    ],
    ids=["claim", "fence", "attempt", "run"],
)
def test_a_control_for_another_identity_refuses_as_identity_mismatch(
    changes: dict[str, Any],
) -> None:
    gate = ControlGate(IDENTITY, FakeReader(control=_control(**changes)))

    with pytest.raises(ControlRefused) as refused:
        gate.before_model_call(T0)

    assert refused.value.reason == "identity_mismatch"


def test_a_cancel_for_this_attempt_refuses_as_cancel_requested() -> None:
    gate = ControlGate(IDENTITY, FakeReader(control=_control(), cancel=_cancel()))

    with pytest.raises(ControlRefused) as refused:
        gate.before_model_call(T0)

    assert refused.value.reason == "cancel_requested"


def test_a_cancel_naming_another_claim_refuses_as_identity_mismatch() -> None:
    gate = ControlGate(
        IDENTITY,
        FakeReader(control=_control(), cancel=_cancel(claim_id=OTHER_CLAIM_ID)),
    )

    with pytest.raises(ControlRefused) as refused:
        gate.before_model_call(T0)

    assert refused.value.reason == "identity_mismatch"


def test_the_control_object_is_read_once_per_attempt() -> None:
    reader = FakeReader(control=_control())
    gate = ControlGate(IDENTITY, reader)

    gate.before_model_call(T0)
    gate.before_model_call(T0 + timedelta(seconds=CONTROL_POLL_SECONDS))

    assert reader.control_reads == 1


def test_cancel_is_polled_at_most_every_fifteen_seconds() -> None:
    reader = FakeReader(control=_control())
    gate = ControlGate(IDENTITY, reader)

    gate.before_model_call(T0)
    gate.before_model_call(T0 + timedelta(seconds=14))
    assert reader.cancel_reads == 1

    gate.before_model_call(T0 + timedelta(seconds=15))
    assert reader.cancel_reads == 2


def test_a_cancel_written_inside_the_window_is_seen_at_the_next_due_poll() -> None:
    reader = FakeReader(control=_control())
    gate = ControlGate(IDENTITY, reader)
    gate.before_model_call(T0)

    reader.cancel = _cancel()
    gate.before_model_call(T0 + timedelta(seconds=5))

    with pytest.raises(ControlRefused) as refused:
        gate.before_model_call(T0 + timedelta(seconds=CONTROL_POLL_SECONDS))
    assert refused.value.reason == "cancel_requested"


def test_an_observed_stop_is_latched_without_reading_again() -> None:
    reader = FakeReader(control=_control(), cancel=_cancel())
    gate = ControlGate(IDENTITY, reader)

    for offset in (0, 3600):
        with pytest.raises(ControlRefused) as refused:
            gate.before_model_call(T0 + timedelta(seconds=offset))
        assert refused.value.reason == "cancel_requested"

    assert reader.cancel_reads == 1


def test_a_failed_cancel_read_records_no_poll_so_the_next_call_retries() -> None:
    reader = FakeReader(control=_control())
    reader.fail_next_cancel = True
    gate = ControlGate(IDENTITY, reader)

    with pytest.raises(OSError):
        gate.before_model_call(T0)

    gate.before_model_call(T0)
    assert reader.cancel_reads == 2


def test_a_failed_control_read_propagates_and_is_retried() -> None:
    reader = FakeReader(control=_control())
    reader.fail_next_control = True
    gate = ControlGate(IDENTITY, reader)

    with pytest.raises(OSError):
        gate.before_model_call(T0)

    gate.before_model_call(T0)
    assert reader.control_reads == 2


def test_a_clock_that_moves_backwards_polls_again() -> None:
    reader = FakeReader(control=_control())
    gate = ControlGate(IDENTITY, reader)
    gate.before_model_call(T0)

    gate.before_model_call(T0 - timedelta(seconds=1))

    assert reader.cancel_reads == 2
