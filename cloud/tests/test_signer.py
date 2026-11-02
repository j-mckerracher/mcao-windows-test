"""Signer service: the only path that signs object capabilities, and the controller's use of it.

The controller holds no signing method. It asks the signer service for a capability; the service
signs only a request that matches accepted run state and refuses every other request with a typed
refusal that names no run data.
"""

from dataclasses import replace
from datetime import datetime, timedelta
from typing import Any

import pytest
from pydantic import ValidationError

from fakes import (
    CLAIM_ID,
    DEFAULT_MAX_RUNTIME_SECONDS,
    EPOCH,
    OWNER_A,
    OWNER_B,
    RUN_ID,
    InMemoryObjectStore,
    InMemoryRunStore,
    InMemorySigningBackend,
    ManualClock,
    RecordingLauncher,
    create_payload,
    make_policy,
    make_ports,
    run_record,
)
from mao_cloud.broker import Broker, ObjectStore
from mao_cloud.protocol import CAPABILITIES, ObjectKind, RunRecord, RunState, sha256_hex
from mao_cloud.signer import (
    CapabilityRequest,
    DirectSignerClient,
    IssuedCapability,
    RefusalCode,
    SignerRefused,
    SignerService,
)

CONTROLLER = "mcao-controller@example.test"
OTHER_RUN_ID = "run_" + "e" * 32
ATTEMPT = f"runs/{RUN_ID}/attempts/000001"
SHA = sha256_hex(b"checksum bound payload")
SIGNER_NOW = EPOCH + timedelta(seconds=60)
ATTEMPT_DEADLINE = EPOCH + timedelta(seconds=DEFAULT_MAX_RUNTIME_SECONDS)
SEQUENCED = frozenset({ObjectKind.LOG})
CHECKPOINT_ID = "ck-0001"
DEFAULT_NAMES: dict[ObjectKind, str] = {
    ObjectKind.INPUT: f"runs/{RUN_ID}/input/source.tar",
    ObjectKind.CLAIM: f"{ATTEMPT}/claim.json",
    ObjectKind.CONTROL: f"{ATTEMPT}/control.json",
    ObjectKind.INTAKE: f"{ATTEMPT}/intake.json",
    ObjectKind.CANCEL: f"{ATTEMPT}/cancel.json",
    ObjectKind.STATUS: f"{ATTEMPT}/status.json",
    ObjectKind.CHECKPOINT: f"{ATTEMPT}/checkpoints/{CHECKPOINT_ID}/manifest.json",
    ObjectKind.LOG: f"{ATTEMPT}/logs/0000000001.ndjson",
    ObjectKind.RESULT: f"{ATTEMPT}/result.json",
}


class FakeRuns:
    def __init__(self, *records: RunRecord) -> None:
        self._records = {record.run_id: record for record in records}

    def get_run(self, run_id: str) -> RunRecord | None:
        return self._records.get(run_id)


def make_service(
    *records: RunRecord, now: datetime = SIGNER_NOW
) -> tuple[SignerService, InMemorySigningBackend]:
    backend = InMemorySigningBackend()
    service = SignerService(
        runs=FakeRuns(*records),
        backend=backend,
        controller_identity=CONTROLLER,
        now=lambda: now,
    )
    return service, backend


def req(
    record: RunRecord, kind: ObjectKind, method: str = "PUT", **fields: Any
) -> CapabilityRequest:
    values: dict[str, Any] = {
        "owner_id": record.owner_id,
        "run_id": record.run_id,
        "kind": kind,
        "method": method,
        "object_name": DEFAULT_NAMES[kind],
        "expires_at": SIGNER_NOW + timedelta(minutes=5),
    }
    if kind is not ObjectKind.INPUT:
        values.update(
            attempt=record.attempt,
            fence=record.fence,
            claim_id=record.claim_id,
            seq=1 if kind in SEQUENCED else None,
        )
    if kind is ObjectKind.CHECKPOINT:
        values["checkpoint_id"] = CHECKPOINT_ID
    values.update(fields)
    return CapabilityRequest(**values)


def refusal_code(
    service: SignerService, request: CapabilityRequest, caller: str | None = CONTROLLER
) -> RefusalCode:
    with pytest.raises(SignerRefused) as error:
        service.issue(caller=caller, request=request)
    return error.value.code


def running() -> RunRecord:
    return run_record(state=RunState.RUNNING, claim_id=CLAIM_ID)


# ---------------------------------------------------------------------------
# Signs what matches accepted run state
# ---------------------------------------------------------------------------


def test_input_upload_is_signed_for_the_awaiting_run_with_its_checksum_and_create_only_header() -> (
    None
):
    record = run_record(state=RunState.AWAITING_UPLOAD)
    service, backend = make_service(record)
    sha = record.manifest.source_upload.sha256

    issued = service.issue(
        caller=CONTROLLER,
        request=req(
            record,
            ObjectKind.INPUT,
            "PUT",
            sha256_hex=sha,
            expires_at=SIGNER_NOW + timedelta(minutes=4),
        ),
    )

    assert issued.object_name == f"runs/{RUN_ID}/input/source.tar"
    assert issued.headers == {
        "content-type": "application/x-tar",
        "x-goog-if-generation-match": "0",
        "x-goog-meta-sha256": sha,
    }
    assert [(name, method) for name, method, _, _ in backend.calls] == [(issued.object_name, "PUT")]


def test_claim_capability_is_create_only_for_the_dispatching_attempt() -> None:
    record = run_record(state=RunState.DISPATCHING, claim_id=CLAIM_ID)
    service, _ = make_service(record)

    issued = service.issue(caller=CONTROLLER, request=req(record, ObjectKind.CLAIM, "PUT"))

    assert issued.object_name == f"{ATTEMPT}/claim.json"
    assert issued.headers == {"content-type": "application/json", "x-goog-if-generation-match": "0"}


@pytest.mark.parametrize("generation", [0, 7], ids=["first-write", "replacement"])
def test_status_capability_carries_the_expected_generation_precondition(generation: int) -> None:
    service, _ = make_service(running())

    issued = service.issue(
        caller=CONTROLLER,
        request=req(running(), ObjectKind.STATUS, "PUT", expected_generation=generation),
    )

    assert issued.object_name == f"{ATTEMPT}/status.json"
    assert issued.headers == {
        "content-type": "application/json",
        "x-goog-if-generation-match": str(generation),
    }


@pytest.mark.parametrize(
    "state", [RunState.STARTING, RunState.RUNNING, RunState.RECONCILING, RunState.COLLECTING]
)
def test_result_capability_binds_the_checksum_and_create_only_precondition(state: RunState) -> None:
    record = run_record(state=state, claim_id=CLAIM_ID)
    service, _ = make_service(record)

    issued = service.issue(
        caller=CONTROLLER, request=req(record, ObjectKind.RESULT, "PUT", sha256_hex=SHA)
    )

    assert issued.object_name == f"{ATTEMPT}/result.json"
    assert issued.headers == {
        "content-type": "application/json",
        "x-goog-if-generation-match": "0",
        "x-goog-meta-sha256": SHA,
    }


def test_log_capability_is_named_by_its_exact_sequence_number() -> None:
    service, _ = make_service(running())

    issued = service.issue(
        caller=CONTROLLER,
        request=req(
            running(), ObjectKind.LOG, "PUT", seq=2, object_name=f"{ATTEMPT}/logs/0000000002.ndjson"
        ),
    )

    assert issued.object_name == f"{ATTEMPT}/logs/0000000002.ndjson"
    assert issued.headers == {
        "content-type": "application/x-ndjson",
        "x-goog-if-generation-match": "0",
    }


def test_input_read_is_signed_without_a_precondition_for_a_dispatched_run() -> None:
    service, _ = make_service(running())

    issued = service.issue(caller=CONTROLLER, request=req(running(), ObjectKind.INPUT, "GET"))

    assert issued.object_name == f"runs/{RUN_ID}/input/source.tar"
    assert issued.headers == {}


@pytest.mark.parametrize("now", [SIGNER_NOW, EPOCH + timedelta(minutes=11)])
def test_input_read_retains_its_full_ttl_past_the_upload_window(now: datetime) -> None:
    record = running()
    service, backend = make_service(record, now=now)
    expires_at = now + timedelta(minutes=20)

    issued = service.issue(
        caller=CONTROLLER,
        request=req(record, ObjectKind.INPUT, "GET", expires_at=expires_at),
    )

    assert issued.expires_at == expires_at
    assert backend.calls == [(issued.object_name, "GET", expires_at, {})]


def test_input_upload_is_refused_after_the_upload_window() -> None:
    record = run_record(state=RunState.AWAITING_UPLOAD)
    now = EPOCH + timedelta(minutes=11)
    service, backend = make_service(record, now=now)

    code = refusal_code(
        service,
        req(
            record,
            ObjectKind.INPUT,
            "PUT",
            sha256_hex=record.manifest.source_upload.sha256,
            expires_at=now + timedelta(minutes=1),
        ),
    )

    assert code is RefusalCode.DEADLINE_PASSED
    assert backend.calls == []


def test_expiry_at_the_attempt_deadline_is_the_latest_one_signed() -> None:
    service, backend = make_service(running())

    issued = service.issue(
        caller=CONTROLLER,
        request=req(
            running(), ObjectKind.STATUS, "PUT", expected_generation=7, expires_at=ATTEMPT_DEADLINE
        ),
    )

    assert issued.expires_at == ATTEMPT_DEADLINE
    assert backend.calls[0][2] == ATTEMPT_DEADLINE


def test_checkpoint_is_refused_while_its_blocking_ticket_is_open() -> None:
    service, backend = make_service(running())

    code = refusal_code(service, req(running(), ObjectKind.CHECKPOINT, "PUT", sha256_hex=SHA))

    assert code is RefusalCode.BLOCKED
    assert backend.calls == []


def test_checkpoint_is_signed_with_create_only_once_its_ticket_clears(
    monkeypatch: pytest.MonkeyPatch,
) -> None:
    key = (ObjectKind.CHECKPOINT, "PUT")
    monkeypatch.setitem(CAPABILITIES, key, replace(CAPABILITIES[key], blocking=None))
    service, _ = make_service(running())

    issued = service.issue(
        caller=CONTROLLER, request=req(running(), ObjectKind.CHECKPOINT, "PUT", sha256_hex=SHA)
    )

    assert issued.object_name == f"{ATTEMPT}/checkpoints/{CHECKPOINT_ID}/manifest.json"
    assert issued.headers == {
        "content-type": "application/octet-stream",
        "x-goog-if-generation-match": "0",
        "x-goog-meta-sha256": SHA,
    }


@pytest.mark.parametrize(
    "other_name",
    [
        f"{ATTEMPT}/checkpoints/ck-0002/manifest.json",
        f"{ATTEMPT}/checkpoints/0000000001.json",
    ],
    ids=["component-of-another-checkpoint-id", "retired-sequenced-name"],
)
def test_a_checkpoint_request_must_name_a_component_of_its_checkpoint_id(
    other_name: str,
) -> None:
    service, backend = make_service(running())

    code = refusal_code(
        service,
        req(running(), ObjectKind.CHECKPOINT, "PUT", sha256_hex=SHA, object_name=other_name),
    )

    assert code is RefusalCode.OBJECT_MISMATCH
    assert backend.calls == []


def test_direct_client_asks_the_service_as_the_controller_identity() -> None:
    service, _ = make_service(running())
    client = DirectSignerClient(service, caller=CONTROLLER)

    issued = client.issue(req(running(), ObjectKind.STATUS, "PUT", expected_generation=7))

    assert isinstance(issued, IssuedCapability)
    assert issued.object_name == f"{ATTEMPT}/status.json"


# ---------------------------------------------------------------------------
# Refuses everything else, with typed refusals and no run data
# ---------------------------------------------------------------------------


def test_forged_claim_is_refused_before_any_signature() -> None:
    service, backend = make_service(running())

    code = refusal_code(
        service,
        req(
            running(), ObjectKind.STATUS, "PUT", expected_generation=7, claim_id="claim_" + "c" * 32
        ),
    )

    assert code is RefusalCode.FORGED_IDENTITY
    assert backend.calls == []


def test_forged_attempt_ahead_of_the_accepted_attempt_is_refused() -> None:
    service, backend = make_service(running())

    code = refusal_code(
        service, req(running(), ObjectKind.STATUS, "PUT", expected_generation=7, attempt=2)
    )

    assert code is RefusalCode.FORGED_IDENTITY
    assert backend.calls == []


def test_stale_attempt_is_refused_after_a_retry_advanced_the_attempt() -> None:
    record = run_record(state=RunState.RUNNING, attempt=2, fence=2, claim_id=CLAIM_ID)
    service, backend = make_service(record)

    code = refusal_code(
        service,
        req(record, ObjectKind.STATUS, "PUT", expected_generation=7, attempt=1, fence=1),
    )

    assert code is RefusalCode.STALE_ATTEMPT
    assert backend.calls == []


def test_stale_fence_is_refused_for_the_current_attempt() -> None:
    record = run_record(state=RunState.RUNNING, attempt=1, fence=2, claim_id=CLAIM_ID)
    service, backend = make_service(record)

    code = refusal_code(
        service, req(record, ObjectKind.STATUS, "PUT", expected_generation=7, fence=1)
    )

    assert code is RefusalCode.STALE_FENCE
    assert backend.calls == []


def test_another_owners_run_answers_exactly_like_a_missing_run() -> None:
    service, backend = make_service(running())

    with pytest.raises(SignerRefused) as foreign:
        service.issue(
            caller=CONTROLLER,
            request=req(
                running(), ObjectKind.STATUS, "PUT", expected_generation=7, owner_id=OWNER_B
            ),
        )
    with pytest.raises(SignerRefused) as missing:
        service.issue(
            caller=CONTROLLER,
            request=req(
                running(), ObjectKind.STATUS, "PUT", expected_generation=7, run_id=OTHER_RUN_ID
            ),
        )

    assert foreign.value.code is RefusalCode.NOT_FOUND
    assert missing.value.code is RefusalCode.NOT_FOUND
    assert str(foreign.value) == str(missing.value)
    assert backend.calls == []


@pytest.mark.parametrize(
    "object_name",
    [
        f"{ATTEMPT}/../../input/source.tar",
        f"{ATTEMPT}/result.json",
        "config/signer-key.json",
        f"runs/{OTHER_RUN_ID}/attempts/000001/status.json",
    ],
    ids=["traversal", "other-kind-name", "outside-the-runs-namespace", "other-run-name"],
)
def test_arbitrary_object_names_are_refused(object_name: str) -> None:
    service, backend = make_service(running())

    code = refusal_code(
        service,
        req(running(), ObjectKind.STATUS, "PUT", expected_generation=7, object_name=object_name),
    )

    assert code is RefusalCode.OBJECT_MISMATCH
    assert backend.calls == []


def test_methods_and_kinds_outside_the_capability_table_are_refused() -> None:
    service, backend = make_service(running())

    code = refusal_code(service, req(running(), ObjectKind.CANCEL, "PUT"))

    assert code is RefusalCode.UNKNOWN_CAPABILITY
    assert backend.calls == []


def test_a_method_other_than_get_or_put_does_not_parse() -> None:
    with pytest.raises(ValidationError):
        req(running(), ObjectKind.STATUS, "DELETE", expected_generation=7)


def test_status_generation_cannot_be_negative() -> None:
    with pytest.raises(ValidationError) as error:
        req(running(), ObjectKind.STATUS, "PUT", expected_generation=-1)

    assert error.value.errors()[0]["loc"] == ("expected_generation",)
    assert error.value.errors()[0]["type"] == "greater_than_equal"


@pytest.mark.parametrize(
    "expires_at",
    [SIGNER_NOW.replace(tzinfo=None), "2026-10-22T19:30:00"],
    ids=["datetime", "timestamp-without-offset"],
)
def test_expiry_without_a_timezone_does_not_parse(expires_at: datetime | str) -> None:
    with pytest.raises(ValidationError) as error:
        req(running(), ObjectKind.STATUS, "PUT", expected_generation=0, expires_at=expires_at)

    assert error.value.errors()[0]["loc"] == ("expires_at",)
    assert error.value.errors()[0]["type"] == "timezone_aware"


@pytest.mark.parametrize(
    ("expires_at", "now"),
    [
        (SIGNER_NOW + timedelta(hours=3), SIGNER_NOW),
        (SIGNER_NOW - timedelta(seconds=1), SIGNER_NOW),
    ],
    ids=["beyond-the-holder-ttl-and-deadline", "already-expired"],
)
def test_expiry_outside_the_allowed_window_is_refused(expires_at: datetime, now: datetime) -> None:
    service, backend = make_service(running(), now=now)

    code = refusal_code(
        service,
        req(running(), ObjectKind.STATUS, "PUT", expected_generation=7, expires_at=expires_at),
    )

    assert code is RefusalCode.EXPIRY_REFUSED
    assert backend.calls == []


def test_no_capability_is_signed_after_the_attempt_deadline() -> None:
    service, backend = make_service(running(), now=ATTEMPT_DEADLINE + timedelta(seconds=1))

    code = refusal_code(service, req(running(), ObjectKind.STATUS, "PUT", expected_generation=7))

    assert code is RefusalCode.DEADLINE_PASSED
    assert backend.calls == []


@pytest.mark.parametrize(
    ("state", "kind", "method", "fields", "claim"),
    [
        (RunState.DISPATCHING, ObjectKind.INPUT, "PUT", {"sha256_hex": SHA}, None),
        (RunState.RUNNING, ObjectKind.CLAIM, "PUT", {}, CLAIM_ID),
        (RunState.SUCCEEDED, ObjectKind.STATUS, "PUT", {"expected_generation": 7}, CLAIM_ID),
        (RunState.QUEUED, ObjectKind.RESULT, "PUT", {"sha256_hex": SHA}, None),
    ],
    ids=[
        "upload-closed-after-dispatch",
        "claim-after-acceptance",
        "terminal-run",
        "result-before-dispatch",
    ],
)
def test_capability_outside_the_run_state_is_refused(
    state: RunState, kind: ObjectKind, method: str, fields: dict[str, Any], claim: str | None
) -> None:
    record = run_record(state=state, claim_id=claim)
    service, backend = make_service(record)

    code = refusal_code(service, req(record, kind, method, **fields))

    assert code is RefusalCode.RUN_STATE_REFUSED
    assert backend.calls == []


@pytest.mark.parametrize(
    "state",
    [
        RunState.DISPATCHING,
        RunState.CANCEL_REQUESTED,
        RunState.SUCCEEDED,
        RunState.FAILED,
        RunState.TIMED_OUT,
        RunState.CANCELLED,
    ],
)
def test_result_is_refused_before_startup_or_after_cancellation_or_termination(
    state: RunState,
) -> None:
    record = run_record(state=state, claim_id=CLAIM_ID)
    service, backend = make_service(record)

    code = refusal_code(service, req(record, ObjectKind.RESULT, "PUT", sha256_hex=SHA))

    assert code is RefusalCode.RUN_STATE_REFUSED
    assert backend.calls == []


@pytest.mark.parametrize(
    ("state", "kind", "claim", "fields"),
    [
        (RunState.RUNNING, ObjectKind.STATUS, CLAIM_ID, {}),
        (RunState.DISPATCHING, ObjectKind.CLAIM, CLAIM_ID, {"expected_generation": 3}),
        (RunState.COLLECTING, ObjectKind.RESULT, CLAIM_ID, {}),
        (RunState.RUNNING, ObjectKind.LOG, CLAIM_ID, {"sha256_hex": SHA}),
        (RunState.AWAITING_UPLOAD, ObjectKind.INPUT, None, {"sha256_hex": "f" * 64}),
    ],
    ids=[
        "generation-match-without-generation",
        "create-only-with-a-generation",
        "checksum-bound-without-checksum",
        "checksum-on-an-unbound-capability",
        "input-checksum-not-the-accepted-one",
    ],
)
def test_precondition_that_does_not_match_the_protocol_is_refused(
    state: RunState, kind: ObjectKind, claim: str | None, fields: dict[str, Any]
) -> None:
    record = run_record(state=state, claim_id=claim)
    service, backend = make_service(record)

    code = refusal_code(service, req(record, kind, "PUT", **fields))

    assert code is RefusalCode.PRECONDITION_REFUSED
    assert backend.calls == []


@pytest.mark.parametrize(
    "caller", [None, "someone-else@example.test"], ids=["anonymous", "other-identity"]
)
def test_only_the_controller_identity_may_ask_for_a_signature(caller: str | None) -> None:
    service, backend = make_service(running())

    code = refusal_code(
        service,
        req(running(), ObjectKind.STATUS, "PUT", expected_generation=7),
        caller=caller,
    )

    assert code is RefusalCode.UNAUTHENTICATED
    assert backend.calls == []


@pytest.mark.parametrize("code", list(RefusalCode))
def test_every_refusal_message_is_fixed_and_names_no_run_data(code: RefusalCode) -> None:
    message = str(SignerRefused(code))

    for secret in (RUN_ID, OTHER_RUN_ID, CLAIM_ID, OWNER_A, "attempts", "status.json"):
        assert secret not in message


# ---------------------------------------------------------------------------
# The controller asks the signer; it holds no signing method
# ---------------------------------------------------------------------------


class RecordingSigner:
    """A fake signer that answers the controller's request and remembers it."""

    def __init__(self) -> None:
        self.requests: list[CapabilityRequest] = []

    def issue(self, request: CapabilityRequest) -> IssuedCapability:
        self.requests.append(request)
        return IssuedCapability(
            object_name=request.object_name,
            method=request.method,
            url=f"https://signer.invalid/{request.object_name}",
            headers={"x-goog-if-generation-match": "0"},
            expires_at=request.expires_at,
        )


def test_create_run_takes_its_upload_capability_from_the_signer() -> None:
    clock = ManualClock(EPOCH)
    store = InMemoryRunStore()
    base = make_ports(
        store=store,
        objects=InMemoryObjectStore(),
        launcher=RecordingLauncher(),
        clock=clock,
        run_ids=[RUN_ID],
    )
    signer = RecordingSigner()

    response, _ = Broker(policy=make_policy(), ports=replace(base, signer=signer)).create_run(
        OWNER_A, create_payload()
    )

    assert response.upload is not None
    assert response.upload.url == f"https://signer.invalid/runs/{RUN_ID}/input/source.tar"
    [request] = signer.requests
    stored = store.get_run(RUN_ID)
    assert stored is not None
    assert request.kind is ObjectKind.INPUT
    assert request.method == "PUT"
    assert request.owner_id == OWNER_A
    assert request.sha256_hex == stored.manifest.source_upload.sha256


def test_the_object_store_port_holds_no_signing_method() -> None:
    assert "sign_put" not in dir(ObjectStore)
    assert not hasattr(InMemoryObjectStore, "sign_put")
