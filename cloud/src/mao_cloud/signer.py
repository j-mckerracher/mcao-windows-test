"""Signer service: the only code that signs object capabilities.

The controller holds no signing method. It asks SignerService for one capability at a time and
presents its own identity. The service signs only a request that matches accepted run state, and
refuses every other request with a RefusalCode whose message names no run data.
"""

import re
from collections.abc import Callable, Mapping
from dataclasses import dataclass
from datetime import datetime
from enum import StrEnum
from typing import Literal, Protocol

from pydantic import AwareDatetime, BaseModel, ConfigDict, Field

from mao_cloud import protocol
from mao_cloud.protocol import (
    ACTIVE_STATES,
    CAPABILITIES,
    Capability,
    ObjectKind,
    RunRecord,
    RunState,
)

_LIVE_STATES = ACTIVE_STATES | {RunState.CANCEL_REQUESTED}
_SHA256 = re.compile(r"[0-9a-f]{64}")

# The run states in which each capability may be signed. A capability with no entry is refused.
_ALLOWED_STATES: Mapping[tuple[ObjectKind, str], frozenset[RunState]] = {
    (ObjectKind.INPUT, "PUT"): frozenset({RunState.AWAITING_UPLOAD}),
    (ObjectKind.INPUT, "GET"): _LIVE_STATES,
    (ObjectKind.CLAIM, "PUT"): frozenset({RunState.DISPATCHING}),
    (ObjectKind.CONTROL, "GET"): _LIVE_STATES,
    (ObjectKind.INTAKE, "GET"): _LIVE_STATES,
    (ObjectKind.CANCEL, "GET"): _LIVE_STATES,
    (ObjectKind.STATUS, "PUT"): _LIVE_STATES,
    (ObjectKind.CHECKPOINT, "PUT"): _LIVE_STATES,
    (ObjectKind.LOG, "PUT"): _LIVE_STATES,
    (ObjectKind.RESULT, "PUT"): frozenset(
        {RunState.STARTING, RunState.RUNNING, RunState.RECONCILING, RunState.COLLECTING}
    ),
}


class RefusalCode(StrEnum):
    UNAUTHENTICATED = "unauthenticated"
    UNKNOWN_CAPABILITY = "unknown_capability"
    NOT_FOUND = "not_found"
    FORGED_IDENTITY = "forged_identity"
    STALE_ATTEMPT = "stale_attempt"
    STALE_FENCE = "stale_fence"
    OBJECT_MISMATCH = "object_mismatch"
    RUN_STATE_REFUSED = "run_state_refused"
    BLOCKED = "blocked"
    DEADLINE_PASSED = "deadline_passed"
    EXPIRY_REFUSED = "expiry_refused"
    PRECONDITION_REFUSED = "precondition_refused"
    # Transport outcomes only: the signer's HTTP entrypoint and its controller client produce them.
    # SignerService never returns either one.
    MALFORMED_REQUEST = "malformed_request"
    UNAVAILABLE = "unavailable"


_MESSAGES: Mapping[RefusalCode, str] = {
    RefusalCode.UNAUTHENTICATED: "the caller is not the controller",
    RefusalCode.UNKNOWN_CAPABILITY: "the protocol grants no such access",
    RefusalCode.NOT_FOUND: "no accepted run matches the request",
    RefusalCode.FORGED_IDENTITY: "the request does not match the accepted run identity",
    RefusalCode.STALE_ATTEMPT: "the request is for an attempt that is no longer current",
    RefusalCode.STALE_FENCE: "the request is for a fence that is no longer current",
    RefusalCode.OBJECT_MISMATCH: "the object is not the one this request may use",
    RefusalCode.RUN_STATE_REFUSED: "the run is not in a state that allows this access",
    RefusalCode.BLOCKED: "this access is not yet enabled",
    RefusalCode.DEADLINE_PASSED: "the attempt deadline has passed",
    RefusalCode.EXPIRY_REFUSED: "the requested expiry is outside the allowed window",
    RefusalCode.PRECONDITION_REFUSED: "the precondition does not match the protocol",
    RefusalCode.MALFORMED_REQUEST: "the request is not a well-formed capability request",
    RefusalCode.UNAVAILABLE: "the signer is unavailable; signing outcome is unknown",
}


class SignerRefused(Exception):
    """A typed refusal. The code is the contract; the message is fixed and names no run data."""

    def __init__(self, code: RefusalCode) -> None:
        super().__init__(_MESSAGES[code])
        self.code = code


class CapabilityRequest(BaseModel):
    """What the controller asks for. The service checks every field against accepted run state."""

    model_config = ConfigDict(extra="forbid", frozen=True)

    owner_id: str
    run_id: str
    kind: ObjectKind
    method: Literal["GET", "PUT"]
    object_name: str
    expires_at: AwareDatetime
    attempt: int | None = None
    fence: int | None = None
    claim_id: str | None = None
    seq: int | None = None
    checkpoint_id: str | None = None
    expected_generation: int | None = Field(default=None, ge=0)
    sha256_hex: str | None = None


@dataclass(frozen=True)
class IssuedCapability:
    object_name: str
    method: str
    url: str
    headers: dict[str, str]
    expires_at: datetime


class SigningBackend(Protocol):
    """The signer's own signing identity. Called only after every check has passed."""

    def sign(
        self, name: str, *, method: str, expires_at: datetime, headers: Mapping[str, str]
    ) -> str: ...


class RunLookup(Protocol):
    def get_run(self, run_id: str) -> RunRecord | None: ...


class SignerClient(Protocol):
    """What the controller sees: a way to ask for one capability. It holds no signing method."""

    def issue(self, request: CapabilityRequest) -> IssuedCapability: ...


class SignerService:
    def __init__(
        self,
        *,
        runs: RunLookup,
        backend: SigningBackend,
        controller_identity: str,
        now: Callable[[], datetime],
    ) -> None:
        self._runs = runs
        self._backend = backend
        self._controller_identity = controller_identity
        self._now = now

    def issue(self, *, caller: str | None, request: CapabilityRequest) -> IssuedCapability:
        """Sign one capability or raise SignerRefused. Checks run in the documented order."""
        if caller is None or caller != self._controller_identity:
            raise SignerRefused(RefusalCode.UNAUTHENTICATED)
        key = (request.kind, request.method)
        capability = CAPABILITIES.get(key)
        if capability is None:
            raise SignerRefused(RefusalCode.UNKNOWN_CAPABILITY)
        record = self._runs.get_run(request.run_id)
        if record is None or record.owner_id != request.owner_id:
            raise SignerRefused(RefusalCode.NOT_FOUND)
        _check_identity(record, request)
        name = _expected_name(record, request)
        if name != request.object_name:
            raise SignerRefused(RefusalCode.OBJECT_MISMATCH)
        if record.state not in _ALLOWED_STATES.get(key, frozenset()):
            raise SignerRefused(RefusalCode.RUN_STATE_REFUSED)
        if capability.blocking is not None:
            raise SignerRefused(RefusalCode.BLOCKED)
        now = self._now()
        deadline = _attempt_deadline(record, request.kind, request.method)
        if protocol.deadline_passed(now, deadline):
            raise SignerRefused(RefusalCode.DEADLINE_PASSED)
        latest = protocol.capability_expiry(
            request.kind, request.method, now=now, attempt_deadline=deadline
        )
        if not now < request.expires_at <= latest:
            raise SignerRefused(RefusalCode.EXPIRY_REFUSED)
        headers = _headers(record, request, capability)
        url = self._backend.sign(
            name, method=request.method, expires_at=request.expires_at, headers=headers
        )
        return IssuedCapability(
            object_name=name,
            method=request.method,
            url=url,
            headers=headers,
            expires_at=request.expires_at,
        )


class DirectSignerClient:
    """The controller's in-process path to the signer. It presents only the controller identity."""

    def __init__(self, service: SignerService, *, caller: str) -> None:
        self._service = service
        self._caller = caller

    def issue(self, request: CapabilityRequest) -> IssuedCapability:
        return self._service.issue(caller=self._caller, request=request)


def _check_identity(record: RunRecord, request: CapabilityRequest) -> None:
    """The input upload is bound to the run alone; attempt-scoped objects are bound to the attempt."""
    if request.kind is ObjectKind.INPUT:
        return
    if request.attempt is None or request.attempt > record.attempt:
        raise SignerRefused(RefusalCode.FORGED_IDENTITY)
    if request.attempt < record.attempt:
        raise SignerRefused(RefusalCode.STALE_ATTEMPT)
    if request.fence is None or request.fence > record.fence:
        raise SignerRefused(RefusalCode.FORGED_IDENTITY)
    if request.fence < record.fence:
        raise SignerRefused(RefusalCode.STALE_FENCE)
    if request.claim_id != record.claim_id:
        raise SignerRefused(RefusalCode.FORGED_IDENTITY)


def _expected_name(record: RunRecord, request: CapabilityRequest) -> str:
    try:
        if request.kind is ObjectKind.CHECKPOINT:
            return _checkpoint_component(record, request)
        return protocol.object_name(
            request.kind, run_id=record.run_id, attempt=record.attempt, seq=request.seq
        )
    except ValueError:
        raise SignerRefused(RefusalCode.OBJECT_MISMATCH) from None


def _checkpoint_component(record: RunRecord, request: CapabilityRequest) -> str:
    """A checkpoint PUT must name one component of the checkpoint id it carries (DPES-897)."""
    if request.checkpoint_id is None:
        raise ValueError("a checkpoint request names its checkpoint id")
    components = protocol.checkpoint_object_names(
        record.run_id, record.attempt, request.checkpoint_id
    )
    if request.object_name not in components.values():
        raise ValueError("the object is not a component of this checkpoint id")
    return request.object_name


def _attempt_deadline(record: RunRecord, kind: ObjectKind, method: str) -> datetime:
    if kind is ObjectKind.INPUT and method == "PUT":
        return protocol.upload_deadline(
            admitted_at=record.admitted_at, deadline_at=record.deadline_at
        )
    return record.deadline_at


def _headers(
    record: RunRecord, request: CapabilityRequest, capability: Capability
) -> dict[str, str]:
    if capability.precondition == "generation_match":
        if request.expected_generation is None:
            raise SignerRefused(RefusalCode.PRECONDITION_REFUSED)
    elif request.expected_generation is not None:
        raise SignerRefused(RefusalCode.PRECONDITION_REFUSED)
    if capability.sha256_metadata:
        sha = request.sha256_hex
        if sha is None or _SHA256.fullmatch(sha) is None:
            raise SignerRefused(RefusalCode.PRECONDITION_REFUSED)
        if request.kind is ObjectKind.INPUT and sha != record.manifest.source_upload.sha256:
            raise SignerRefused(RefusalCode.PRECONDITION_REFUSED)
    elif request.sha256_hex is not None:
        raise SignerRefused(RefusalCode.PRECONDITION_REFUSED)
    return protocol.required_headers(
        request.kind,
        request.method,
        sha256_hex_value=request.sha256_hex,
        expected_generation=request.expected_generation,
    )
