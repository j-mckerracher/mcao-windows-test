"""Protocol-v1 contract shared by the controller, runner, sweeper, and CLI.

Owns the wire models, field-origin classification, client-input screening,
redaction, canonical digests, the run state machine with compare-and-set,
idempotency admission, server-clock rules, signed-object capability rules,
cursor paging, and teardown outcomes.

Every function here is pure: no I/O, no cloud clients. Callers supply "now"
and every stored record, so each rule is testable without a network.
"""

import base64
import binascii
import hashlib
import hmac
import json
import math
import re
import uuid
from collections.abc import Mapping
from dataclasses import dataclass
from datetime import UTC, datetime, timedelta
from enum import StrEnum
from typing import Annotated, Any, Literal, NamedTuple, get_args
from uuid import UUID

from pydantic import (
    AwareDatetime,
    BaseModel,
    BeforeValidator,
    ConfigDict,
    Field,
    ValidationError,
    field_validator,
    model_validator,
)

# ---------------------------------------------------------------------------
# Constants. Changing any value here is a protocol decision, not a config knob.
# ---------------------------------------------------------------------------

PROTOCOL_VERSION = "v1"
SUPPORTED_PROTOCOL_VERSIONS: tuple[str, ...] = (PROTOCOL_VERSION,)
SUPPORTED_CURSOR_VERSIONS: tuple[int, ...] = (1,)

MAX_SOURCE_BYTES = 512 * 1024 * 1024
# The source size the controller admits and the worker's bundle intake accepts (DPES-811 AC6). The
# 512 MiB upload capability above is only a backstop: a larger source is refused at admission,
# before any attempt is claimed, instead of being claimed and then refused by the worker.
MAX_ADMITTED_SOURCE_BYTES = 200 * 1024 * 1024
MIN_RUNTIME_SECONDS = 60
MAX_RUNTIME_SECONDS = 2 * 60 * 60
MAX_ATTEMPTS = 3
# Spec timings. Upload URLs last exactly as long as the upload/start deadline.
UPLOAD_WINDOW_SECONDS = 10 * 60
STARTUP_TIMEOUT_SECONDS = 10 * 60
INPUT_READ_TTL_SECONDS = 20 * 60
CLAIM_TTL_SECONDS = 20 * 60
ATTEMPT_CAPABILITY_TTL_SECONDS = 3 * 60 * 60
# The worker entry reads its intake at container start, inside the startup window, so the read
# capability lasts exactly as long as that window.
INTAKE_READ_TTL_SECONDS = STARTUP_TIMEOUT_SECONDS
IDEMPOTENCY_TTL = timedelta(days=7)

MAX_CLAIM_BYTES = 4096
# The intake holds one bootstrap and one accepted receipt, each a few kilobytes. The worker entry
# reads at most this many bytes and refuses a longer object.
MAX_INTAKE_BYTES = 64 * 1024
# Worker launch contract: the only container overrides the launcher sends, and the names the worker
# entry reads them under. The worker image cannot import the controller-only launcher module.
RUN_ID_ENV = "MAO_RUN_ID"
ATTEMPT_ID_ENV = "MAO_ATTEMPT_ID"
INTAKE_URL_ENV = "MAO_INTAKE_URL"
MAX_STATUS_BYTES = 64 * 1024
MAX_STATUS_MESSAGE_CHARS = 512
MAX_LOG_OBJECT_BYTES = 1024 * 1024
MAX_LOG_LINE_CHARS = 4096
MAX_LOG_LINES_PER_ENTRY = 500
MAX_LOG_LIMIT = 500
MAX_RESULT_BYTES = 1024 * 1024
# Returned work (a patch or changed-files bundle) is a separate artifact with its own cap; the
# result document above stays at MAX_RESULT_BYTES. DPES-897 decision 4, section 15.5.
MAX_RESULT_ARTIFACT_BYTES = 200 * 1024 * 1024
MAX_CHECKPOINT_BYTES = 64 * 1024 * 1024
# DPES-817: the returned work bundle is a separate, versioned limit. The result document stays under
# MAX_RESULT_BYTES; the bundle travels as a referenced artifact, so it is capped on its own.
MAX_RETURNED_WORK_BYTES = 200 * 1024 * 1024

RUN_ID_PATTERN = r"^run_[0-9a-f]{32}$"
CLAIM_ID_PATTERN = r"^claim_[0-9a-f]{32}$"
RUNNER_NONCE_PATTERN = r"^[0-9a-f]{32}$"

Harness = Literal["pi"]
VertexModel = Literal["gemini-3.8-flash"]
Thinking = Literal["low", "medium", "high"]
ResourceClass = Literal["standard"]
HARNESS_VALUES: tuple[str, ...] = get_args(Harness)
MODEL_VALUES: tuple[str, ...] = get_args(VertexModel)
THINKING_VALUES: tuple[str, ...] = get_args(Thinking)

Sha256Hex = Annotated[str, Field(pattern=r"^[0-9a-f]{64}$")]
GitSha = Annotated[str, Field(pattern=r"^[0-9a-f]{40}$")]
ImageDigest = Annotated[str, Field(pattern=r"^sha256:[0-9a-f]{64}$")]
ProfileId = Annotated[str, Field(pattern=r"^[a-z][a-z0-9-]{1,62}$")]
TargetId = Annotated[str, Field(pattern=r"^[a-z][a-z0-9-]{2,62}$")]
TaskId = Annotated[str, Field(pattern=r"^[A-Za-z][A-Za-z0-9_.:-]{0,127}$")]
RunId = Annotated[str, Field(pattern=RUN_ID_PATTERN)]
ClaimId = Annotated[str, Field(pattern=CLAIM_ID_PATTERN)]
RunnerNonce = Annotated[str, Field(pattern=RUNNER_NONCE_PATTERN)]


# ---------------------------------------------------------------------------
# Enumerations
# ---------------------------------------------------------------------------


class ErrorCode(StrEnum):
    INVALID_REQUEST = "invalid_request"
    UNSUPPORTED_VERSION = "unsupported_version"
    UNKNOWN_FIELD = "unknown_field"
    CLIENT_CONTROLLED_FIELD = "client_controlled_field"
    BLOCKING_FIELD = "blocking_field"
    NOT_ALLOWED = "not_allowed"
    NOT_FOUND = "not_found"
    DIGEST_MISMATCH = "digest_mismatch"
    REQUEST_DIGEST_CONFLICT = "request_digest_conflict"
    IDEMPOTENCY_EXPIRED = "idempotency_expired"
    ILLEGAL_TRANSITION = "illegal_transition"
    TERMINAL_STATE = "terminal_state"
    VERSION_CONFLICT = "version_conflict"
    STALE_ATTEMPT = "stale_attempt"
    SEQUENCE_REGRESSED = "sequence_regressed"
    RETRY_NOT_ALLOWED = "retry_not_allowed"
    DEADLINE_EXCEEDED = "deadline_exceeded"
    SOURCE_MISMATCH = "source_mismatch"
    UPLOAD_MISMATCH = "upload_mismatch"
    CHECKSUM_MISMATCH = "checksum_mismatch"
    BOOTSTRAP_MISMATCH = "bootstrap_mismatch"
    RESULT_FENCED = "result_fenced"
    RESULT_NOT_READY = "result_not_ready"
    CURSOR_INVALID = "cursor_invalid"
    CURSOR_VERSION_UNSUPPORTED = "cursor_version_unsupported"
    QUEUE_FULL = "queue_full"
    ADMISSIONS_PAUSED = "admissions_paused"


HTTP_STATUS: Mapping[ErrorCode, int] = {
    ErrorCode.INVALID_REQUEST: 400,
    ErrorCode.UNSUPPORTED_VERSION: 400,
    ErrorCode.UNKNOWN_FIELD: 400,
    ErrorCode.CLIENT_CONTROLLED_FIELD: 400,
    ErrorCode.BLOCKING_FIELD: 400,
    ErrorCode.DIGEST_MISMATCH: 400,
    ErrorCode.CURSOR_INVALID: 400,
    ErrorCode.CURSOR_VERSION_UNSUPPORTED: 400,
    ErrorCode.NOT_ALLOWED: 403,
    ErrorCode.NOT_FOUND: 404,
    ErrorCode.IDEMPOTENCY_EXPIRED: 410,
    ErrorCode.REQUEST_DIGEST_CONFLICT: 409,
    ErrorCode.ILLEGAL_TRANSITION: 409,
    ErrorCode.TERMINAL_STATE: 409,
    ErrorCode.VERSION_CONFLICT: 409,
    ErrorCode.STALE_ATTEMPT: 409,
    ErrorCode.SEQUENCE_REGRESSED: 409,
    ErrorCode.RETRY_NOT_ALLOWED: 409,
    ErrorCode.DEADLINE_EXCEEDED: 409,
    ErrorCode.SOURCE_MISMATCH: 409,
    ErrorCode.UPLOAD_MISMATCH: 409,
    ErrorCode.CHECKSUM_MISMATCH: 409,
    ErrorCode.BOOTSTRAP_MISMATCH: 409,
    ErrorCode.RESULT_FENCED: 409,
    ErrorCode.RESULT_NOT_READY: 409,
    ErrorCode.QUEUE_FULL: 429,
    ErrorCode.ADMISSIONS_PAUSED: 503,
}


class RunState(StrEnum):
    AWAITING_UPLOAD = "awaiting_upload"
    QUEUED = "queued"
    DISPATCHING = "dispatching"
    STARTING = "starting"
    RUNNING = "running"
    COLLECTING = "collecting"
    RECONCILING = "reconciling"
    CANCEL_REQUESTED = "cancel_requested"
    SUCCEEDED = "succeeded"
    FAILED = "failed"
    TIMED_OUT = "timed_out"
    CANCELLED = "cancelled"


class FailureCode(StrEnum):
    UPLOAD_EXPIRED = "upload_expired"
    QUEUED_EXPIRED = "queued_expired"
    DEADLINE_EXCEEDED = "deadline_exceeded"
    STARTUP_TIMEOUT = "startup_timeout"
    HEARTBEAT_LOST = "heartbeat_lost"
    LAUNCH_UNCERTAIN = "launch_uncertain"
    WORKER_FAILED = "worker_failed"
    OWNER_CANCELLED = "owner_cancelled"
    CHECKSUM_MISMATCH = "checksum_mismatch"
    MISSING_RESULT = "missing_result"


# Retry eligibility belongs to the failure code, never to caller input.
RETRYABLE: Mapping[FailureCode, bool] = {
    FailureCode.UPLOAD_EXPIRED: False,
    FailureCode.QUEUED_EXPIRED: False,
    FailureCode.DEADLINE_EXCEEDED: False,
    FailureCode.STARTUP_TIMEOUT: True,
    FailureCode.HEARTBEAT_LOST: True,
    FailureCode.LAUNCH_UNCERTAIN: True,
    FailureCode.WORKER_FAILED: True,
    FailureCode.OWNER_CANCELLED: False,
    FailureCode.CHECKSUM_MISMATCH: False,
    # The platform execution finished, so a replay would spend budget on output nobody can verify.
    FailureCode.MISSING_RESULT: False,
}


class Freshness(StrEnum):
    FRESH = "fresh"
    STALE = "stale"
    CLOCK_ANOMALY = "clock_anomaly"


class TimestampVerdict(StrEnum):
    ACCEPTED = "accepted"
    FUTURE = "future"
    BACKWARD = "backward"


class CleanupOutcome(StrEnum):
    ELIGIBLE = "eligible"
    FENCED = "fenced"
    RUNNING = "running"
    INCOMPLETE = "incomplete"
    COMPLETE = "complete"
    RETAINED_EXCEPTION = "retained-exception"
    EXPIRED = "expired"
    UNAVAILABLE = "unavailable"


class EventKind(StrEnum):
    ADMITTED = "admitted"
    UPLOAD_VERIFIED = "upload_verified"
    DISPATCHED = "dispatched"
    STATE_CHANGED = "state_changed"
    STATUS_RECORDED = "status_recorded"
    RESULT_ACCEPTED = "result_accepted"
    RESULT_FENCED = "result_fenced"
    CANCEL_REQUESTED = "cancel_requested"
    RETRY_ADMITTED = "retry_admitted"
    RECONCILE_APPLIED = "reconcile_applied"


class ObjectKind(StrEnum):
    INPUT = "input"
    CLAIM = "claim"
    CONTROL = "control"
    INTAKE = "intake"
    CANCEL = "cancel"
    STATUS = "status"
    CHECKPOINT = "checkpoint"
    LOG = "log"
    RESULT = "result"


class CursorStream(StrEnum):
    """Only logs are paged by cursor. Status is a snapshot, never a history."""

    LOGS = "logs"


class Admission(StrEnum):
    CREATE = "create"
    REPLAY = "replay"


class Origin(StrEnum):
    """Who may set a schema field. Recorded in FIELD_ORIGINS and enforced by tests."""

    CLIENT_MANIFEST = "client-manifest"
    SERVER_DERIVED = "server-derived"
    CONTROLLER_ASSIGNED = "controller-assigned"
    RUNNER_REPORTED = "runner-reported"


TERMINAL_STATES: frozenset[RunState] = frozenset(
    {RunState.SUCCEEDED, RunState.FAILED, RunState.TIMED_OUT, RunState.CANCELLED}
)

ACTIVE_STATES: frozenset[RunState] = frozenset(
    {
        RunState.DISPATCHING,
        RunState.STARTING,
        RunState.RUNNING,
        RunState.COLLECTING,
        RunState.RECONCILING,
    }
)

# Terminal states have no outgoing edges: once an attempt is terminal it never changes.
ALLOWED_TRANSITIONS: Mapping[RunState, frozenset[RunState]] = {
    RunState.AWAITING_UPLOAD: frozenset(
        {RunState.QUEUED, RunState.CANCELLED, RunState.TIMED_OUT}
    ),
    RunState.QUEUED: frozenset({RunState.DISPATCHING, RunState.CANCELLED, RunState.TIMED_OUT}),
    RunState.DISPATCHING: frozenset(
        {RunState.STARTING, RunState.RECONCILING, RunState.FAILED, RunState.CANCEL_REQUESTED}
    ),
    RunState.STARTING: frozenset(
        {
            RunState.RUNNING,
            RunState.COLLECTING,
            RunState.RECONCILING,
            RunState.FAILED,
            RunState.CANCEL_REQUESTED,
        }
    ),
    RunState.RUNNING: frozenset(
        {RunState.COLLECTING, RunState.RECONCILING, RunState.FAILED, RunState.CANCEL_REQUESTED}
    ),
    RunState.COLLECTING: frozenset(
        {RunState.SUCCEEDED, RunState.FAILED, RunState.RECONCILING, RunState.CANCEL_REQUESTED}
    ),
    # Reconciling may only leave by a live worker reporting in, or by a failure.
    # Timeouts and cancels go through CANCEL_REQUESTED so the runner is told to stop.
    RunState.RECONCILING: frozenset(
        {
            RunState.STARTING,
            RunState.RUNNING,
            RunState.COLLECTING,
            RunState.FAILED,
            RunState.CANCEL_REQUESTED,
        }
    ),
    RunState.CANCEL_REQUESTED: frozenset({RunState.CANCELLED, RunState.FAILED, RunState.TIMED_OUT}),
    RunState.SUCCEEDED: frozenset(),
    RunState.FAILED: frozenset(),
    RunState.TIMED_OUT: frozenset(),
    RunState.CANCELLED: frozenset(),
}


# ---------------------------------------------------------------------------
# Errors
# ---------------------------------------------------------------------------


class ProtocolError(Exception):
    """A typed refusal. The code is the contract; the message is for humans."""

    def __init__(self, code: ErrorCode, message: str, **detail: Any) -> None:
        super().__init__(message)
        self.code = code
        self.message = message
        self.detail: dict[str, Any] = detail

    @property
    def http_status(self) -> int:
        return HTTP_STATUS[self.code]

    def body(self) -> "ErrorBody":
        return ErrorBody(
            error=ErrorDetail(code=self.code, message=self.message, detail=self.detail)
        )


# ---------------------------------------------------------------------------
# Wire models
# ---------------------------------------------------------------------------


class _Wire(BaseModel):
    model_config = ConfigDict(extra="forbid", frozen=True)


class ErrorDetail(_Wire):
    code: ErrorCode
    message: str
    detail: dict[str, Any] = Field(default_factory=dict)


class ErrorBody(_Wire):
    error: ErrorDetail


class TypedFailure(_Wire):
    code: FailureCode
    retryable: bool
    message: str = Field(default="", max_length=MAX_STATUS_MESSAGE_CHARS)

    @model_validator(mode="after")
    def _retryable_follows_code(self) -> "TypedFailure":
        if self.retryable != RETRYABLE[self.code]:
            raise ValueError("retryable must follow the failure code")
        return self


def failure(code: FailureCode, message: str = "") -> TypedFailure:
    text = redact(message).text[:MAX_STATUS_MESSAGE_CHARS]
    return TypedFailure(code=code, retryable=RETRYABLE[code], message=text)


def _blocking(ticket: str) -> Any:
    """BLOCKING marker: the field exists in the schema but its value is not yet specified."""

    def check(value: Any) -> Any:
        if value is not None:
            raise ValueError(f"BLOCKING: field depends on {ticket} and is not yet specified")
        return value

    return check


def _redact_text(value: str) -> str:
    return redact(value).text


class CloudTarget(_Wire):
    id: TargetId


class SourceBinding(_Wire):
    base_sha: GitSha
    source_digest: Sha256Hex
    task_digest: Sha256Hex
    config_digest: Sha256Hex


class SourceUpload(_Wire):
    """The client's declared intent to upload immutable source bytes."""

    size_bytes: int = Field(ge=1, le=MAX_SOURCE_BYTES)
    sha256: Sha256Hex
    media_type: Literal["application/x-tar"]


class Budget(_Wire):
    max_runtime_seconds: int = Field(ge=MIN_RUNTIME_SECONDS, le=MAX_RUNTIME_SECONDS)
    restart_budget: Annotated[
        None,
        BeforeValidator(_blocking("DPES-870")),
        Field(json_schema_extra={"x-blocking": ["DPES-870"]}),
    ] = None


class Assignment(_Wire):
    task_id: TaskId
    workflow_correlation: str = Field(pattern=r"^[A-Za-z0-9_.:/-]{1,128}$")


class CreateRunManifest(_Wire):
    """The canonical manifest a client submits. Its digest identifies the request content.

    Serialization for digests: JSON mode, exclude_none=True, sorted keys, no whitespace.
    Optional fields added later with a None default keep every v1 digest unchanged.
    """

    protocol_version: Literal["v1"]
    request_uuid: UUID
    harness: Harness
    model: VertexModel
    thinking: Thinking
    toolchain_profile: ProfileId
    target: CloudTarget
    source: SourceBinding
    source_upload: SourceUpload
    budget: Budget
    capacity: ResourceClass
    assignment: Assignment | None = None

    @field_validator("request_uuid")
    @classmethod
    def _uuid_v7(cls, value: UUID) -> UUID:
        if value.version != 7 or value.variant != uuid.RFC_4122:
            raise ValueError("request_uuid must be an RFC 4122 version 7 UUID")
        return value


class CreateRunRequest(CreateRunManifest):
    canonical_digest: Sha256Hex


class SourceReceipt(_Wire):
    """A descriptor of an upload the client already completed. Not bytes."""

    object_generation: int = Field(ge=1)
    sha256: Sha256Hex
    size_bytes: int = Field(ge=1, le=MAX_SOURCE_BYTES)


class StartRunRequest(_Wire):
    protocol_version: Literal["v1"]
    canonical_digest: Sha256Hex
    source_upload: SourceReceipt


class ActionRequest(_Wire):
    protocol_version: Literal["v1"]


class RetryRequest(ActionRequest):
    """expected_* are compare-and-set preconditions, never assignments."""

    request_uuid: UUID
    expected_attempt: int = Field(ge=1)
    expected_version: int = Field(ge=0)

    @field_validator("request_uuid")
    @classmethod
    def _uuid_v7(cls, value: UUID) -> UUID:
        if value.version != 7 or value.variant != uuid.RFC_4122:
            raise ValueError("request_uuid must be an RFC 4122 version 7 UUID")
        return value


class BootstrapSource(_Wire):
    """The uploaded source the worker must verify. base_sha is the claimed commit its bundle tip must be.

    It is part of the source model, so the projection the receipt and bootstrap share covers it
    (DPES-811 AC3).
    """

    object_name: str
    generation: int = Field(ge=1)
    sha256: Sha256Hex
    size_bytes: int = Field(ge=1, le=MAX_SOURCE_BYTES)
    base_sha: GitSha


class ToolCompatibility(_Wire):
    """Server-selected pinned tuple. The client names a profile id; the server resolves this."""

    image_digest: ImageDigest
    profile_id: ProfileId
    toolchain_manifest_digest: Sha256Hex
    instruction_digest: Sha256Hex


class UploadCapability(_Wire):
    object_name: str
    method: Literal["PUT"]
    url: str
    headers: dict[str, str]
    expires_at: AwareDatetime


class AcceptedReceipt(_Wire):
    """Returned by start. Its projection is the identity the worker bootstrap must reproduce."""

    protocol_version: Literal["v1"]
    run_id: RunId
    state: RunState
    version: int = Field(ge=0)
    attempt: int = Field(ge=1)
    manifest_digest: Sha256Hex
    source: BootstrapSource
    compatibility: ToolCompatibility
    deadline_at: AwareDatetime
    max_runtime_seconds: int = Field(ge=MIN_RUNTIME_SECONDS, le=MAX_RUNTIME_SECONDS)
    projection_digest: Sha256Hex


class WorkerBootstrap(_Wire):
    """Controller-produced after admission and dispatch. Carries no credentials or argv.

    durable_root, checkpoint_ref, and continuation_budget depend on DPES-869/870 and are
    BLOCKING: any value is refused until those tickets specify it.
    """

    protocol_version: Literal["v1"]
    run_id: RunId
    attempt: int = Field(ge=1)
    fence: int = Field(ge=1)
    claim_id: ClaimId
    source: BootstrapSource
    compatibility: ToolCompatibility
    deadline_at: AwareDatetime
    max_runtime_seconds: int = Field(ge=MIN_RUNTIME_SECONDS, le=MAX_RUNTIME_SECONDS)
    durable_root: Annotated[
        None,
        BeforeValidator(_blocking("DPES-869")),
        Field(json_schema_extra={"x-blocking": ["DPES-869"]}),
    ] = None
    checkpoint_ref: Annotated[
        None,
        BeforeValidator(_blocking("DPES-869")),
        Field(json_schema_extra={"x-blocking": ["DPES-869"]}),
    ] = None
    continuation_budget: Annotated[
        None,
        BeforeValidator(_blocking("DPES-870")),
        Field(json_schema_extra={"x-blocking": ["DPES-870"]}),
    ] = None


class WorkerIntake(_Wire):
    """Controller-written before jobs.run, create-only, read once by the worker entry.

    It carries the two documents the worker verifies and hands to trusted_bootstrap: the bootstrap
    and the accepted receipt it must reproduce. Both are built from accepted run state, never from
    client input. The object is named by run and attempt, and the worker reads it through one
    signed GET that the launcher passes as its only intake override.
    """

    bootstrap: WorkerBootstrap
    receipt: AcceptedReceipt


class ClaimDocument(_Wire):
    """Runner-written, create-only. Proves the runner took this attempt and fence.

    runner_nonce is unpredictable per runner process. After an ambiguous create response the runner
    reads the claim back by its exact key: a readback carrying this process's nonce proves it owns
    the attempt, and any other value proves another runner owns it.
    """

    protocol_version: Literal["v1"]
    run_id: RunId
    attempt: int = Field(ge=1)
    fence: int = Field(ge=1)
    claim_id: ClaimId
    runner_nonce: RunnerNonce


class ControlDocument(_Wire):
    """Controller-written before launch. Identity only: it grants no cancel or write authority."""

    protocol_version: Literal["v1"]
    run_id: RunId
    attempt: int = Field(ge=1)
    fence: int = Field(ge=1)
    claim_id: ClaimId


class CancelDocument(_Wire):
    """Controller-written cancel directive for one attempt. Create-only, so it is monotonic.

    Once written it is never rewritten or removed, and a retried write is a no-op. It names the
    fenced attempt it stops and grants the runner no write authority.
    """

    protocol_version: Literal["v1"]
    run_id: RunId
    attempt: int = Field(ge=1)
    fence: int = Field(ge=1)
    claim_id: ClaimId


class StatusReport(_Wire):
    """Runner-written status. Telemetry until the controller accepts it.

    seq must strictly increase for liveness; log_watermark is the highest log seq durably
    written. observed_at is the worker's clock and never moves liveness or deadlines.
    """

    protocol_version: Literal["v1"]
    run_id: RunId
    attempt: int = Field(ge=1)
    fence: int = Field(ge=1)
    claim_id: ClaimId
    seq: int = Field(ge=1)
    phase: Literal["starting", "running", "collecting"]
    observed_at: AwareDatetime
    log_watermark: int = Field(ge=0)
    message: str = Field(default="", max_length=MAX_STATUS_MESSAGE_CHARS)

    @field_validator("message")
    @classmethod
    def _redact_message(cls, value: str) -> str:
        return _redact_text(value)


class ArtifactRef(_Wire):
    name: str = Field(pattern=r"^[A-Za-z0-9][A-Za-z0-9._/-]{0,255}$")
    sha256: Sha256Hex
    size_bytes: int = Field(ge=0, le=MAX_RESULT_ARTIFACT_BYTES)

    @field_validator("name")
    @classmethod
    def _no_traversal(cls, value: str) -> str:
        if value.startswith("/") or ".." in value.split("/"):
            raise ValueError("artifact name must be relative and free of '..' segments")
        return value


class RunResultDocument(_Wire):
    protocol_version: Literal["v1"]
    run_id: RunId
    attempt: int = Field(ge=1)
    fence: int = Field(ge=1)
    claim_id: ClaimId
    outcome: Literal["succeeded", "failed"]
    exit_code: int | None = None
    commit_sha: GitSha | None = None
    artifacts: tuple[ArtifactRef, ...] = ()
    redacted: Literal[True] = True


class LogEntry(_Wire):
    """One log object holds exactly one entry; its seq must equal the object's seq.

    Lines are redacted at the model boundary, so storage never holds secrets.
    """

    protocol_version: Literal["v1"]
    seq: int = Field(ge=1)
    lines: tuple[str, ...] = Field(max_length=MAX_LOG_LINES_PER_ENTRY)

    @field_validator("lines")
    @classmethod
    def _redact_lines(cls, value: tuple[str, ...]) -> tuple[str, ...]:
        if any(len(line) > MAX_LOG_LINE_CHARS for line in value):
            raise ValueError("log line exceeds the protocol limit")
        return tuple(_redact_text(line) for line in value)


class SeqGap(_Wire):
    from_seq: int = Field(ge=1)
    to_seq: int = Field(ge=1)


class Snapshot(_Wire):
    state: RunState
    version: int = Field(ge=0)
    attempt: int = Field(ge=1)
    server_time: AwareDatetime


class LogPage(_Wire):
    run_id: RunId
    attempt: int = Field(ge=1)
    entries: tuple[LogEntry, ...]
    next_cursor: str
    watermark: int = Field(ge=0)
    retained_from: int = Field(ge=1)
    trimmed: bool
    gap: SeqGap | None = None
    snapshot: Snapshot


class RunView(_Wire):
    run_id: RunId
    owner_id: str
    state: RunState
    version: int = Field(ge=0)
    attempt: int = Field(ge=1)
    max_attempts: int = Field(ge=1)
    deadline_at: AwareDatetime
    server_time: AwareDatetime
    freshness: Freshness
    last_seen_at: AwareDatetime | None = None
    failure: TypedFailure | None = None


class EffectiveRun(_Wire):
    """What the run is pinned to. Client-chosen model and resource class; server-resolved image."""

    model: VertexModel
    resource_class: ResourceClass
    image_digest: ImageDigest


class StatusView(_Wire):
    """Latest accepted status snapshot. Not a history; history is the event log.

    result_available is true only when an accepted result exists for the current attempt. It is
    not a promise that the result's artifacts are downloadable; GET result verifies those.
    """

    run: RunView
    effective: EffectiveRun
    result_available: bool
    status_seq: int = Field(ge=0)
    log_watermark: int = Field(ge=0)
    retained_from: int = Field(ge=1)


class CreateRunResponse(_Wire):
    run: RunView
    upload: UploadCapability | None = None


class TeardownStatus(_Wire):
    """Separates normal terminal execution from durable data still pending deletion."""

    run_id: RunId
    execution_state: RunState
    data_cleanup: CleanupOutcome
    pending_deletion: bool
    preserved_records: tuple[Literal["idempotency_record", "audit_record"], ...] = (
        "idempotency_record",
        "audit_record",
    )
    fresh_run_implied: Literal[False] = False


class RunEvent(_Wire):
    run_id: RunId
    seq: int = Field(ge=1)
    kind: EventKind
    attempt: int = Field(ge=1)
    from_state: RunState | None = None
    to_state: RunState | None = None
    at: AwareDatetime
    actor: Literal["broker", "runner", "sweeper", "owner", "operator"]


class IdempotencyRecord(_Wire):
    """Dedup record keyed by (owner, request UUID). Lives IDEMPOTENCY_TTL, then expires.

    digest is the canonical manifest digest for a create, or the action digest for a retry.
    """

    owner_id: str = Field(min_length=1)
    request_uuid: UUID
    digest: Sha256Hex
    run_id: RunId
    created_at: AwareDatetime
    expires_at: AwareDatetime


class RunRecord(_Wire):
    """Internal lifecycle record. Holds the canonical manifest, never source or prompts."""

    run_id: RunId
    owner_id: str = Field(min_length=1)
    server_id: str = Field(min_length=1)
    request_uuid: UUID
    manifest: CreateRunManifest
    manifest_digest: Sha256Hex
    image_digest: ImageDigest
    state: RunState
    version: int = Field(ge=0)
    attempt: int = Field(ge=1)
    fence: int = Field(ge=1)
    max_attempts: int = Field(ge=1, le=MAX_ATTEMPTS)
    admitted_at: AwareDatetime
    state_entered_at: AwareDatetime
    deadline_at: AwareDatetime
    receipt: AcceptedReceipt | None = None
    claim_id: ClaimId | None = None
    retry_request_uuids: tuple[UUID, ...] = ()
    last_seen_at: AwareDatetime | None = None
    last_observed_at: AwareDatetime | None = None
    last_status_seq: int = Field(default=0, ge=0)
    last_log_watermark: int = Field(default=0, ge=0)
    log_retained_from: int = Field(default=1, ge=1)
    failure: TypedFailure | None = None
    # Capacity remains reserved until authoritative execution termination is confirmed.
    # Settled usage stays on the UTC admission day, even across midnight.
    dispatched_at: AwareDatetime | None = None
    # The platform execution name that jobs.run returned for this attempt, stored by compare-and-set
    # right after a successful launch. Internal only: no public view or receipt carries it. Without
    # it no stop can be requested and no termination confirmed, so capacity stays reserved.
    execution_name: str | None = None
    # Set once per attempt, by compare-and-set, when a dispatched execution has never reported ten
    # minutes after dispatch. The alert records a fact; it never releases or reopens capacity.
    unknown_launch_alerted_at: AwareDatetime | None = None
    # Written by the broker on each pass that could not confirm termination. The planner waits for
    # next_reconcile_at before hinting a reconcile, so sweeps do not re-stop every unsettled run on
    # every tick. It never delays a liveness decision, and an explicit reconcile still requests stop.
    unsettled_reconciles: int = Field(default=0, ge=0)
    next_reconcile_at: AwareDatetime | None = None
    dispatch_order: int = Field(default=0, ge=0)
    reserved_seconds: int = Field(default=0, ge=0)
    consumed_seconds: int = Field(default=0, ge=0)


# ---------------------------------------------------------------------------
# Client-controlled input screening and redaction
# ---------------------------------------------------------------------------

CLIENT_CONTROLLED_FIELDS: frozenset[str] = frozenset(
    {
        "owner",
        "owner_id",
        "actor",
        "principal",
        "server_id",
        "run_id",
        "attempt",
        "attempt_number",
        "fence",
        "fence_token",
        "claim",
        "claim_id",
        "claim_nonce",
        "control",
        "image",
        "image_ref",
        "image_digest",
        "container_image",
        "service_account",
        "service_account_email",
        "runtime_identity",
        "secret",
        "secrets",
        "secret_name",
        "secret_ref",
        "credential",
        "credentials",
        "token",
        "access_token",
        "id_token",
        "signed_url",
        "upload_url",
        "download_url",
        "url",
        "local_path",
        "path",
        "argv",
        "command",
        "env",
        "environment",
        "source_bytes",
        "source_content",
        "body",
    }
)

# Identity fields are legitimate in runner and controller documents, so they are
# screened out of client requests only.
IDENTITY_FIELDS: frozenset[str] = frozenset({"run_id", "attempt", "fence", "claim_id"})
DOCUMENT_FORBIDDEN_FIELDS: frozenset[str] = CLIENT_CONTROLLED_FIELDS - IDENTITY_FIELDS


class RedactionRule(NamedTuple):
    pattern: re.Pattern[str]
    replacement: str


REDACTION_RULES: tuple[RedactionRule, ...] = (
    RedactionRule(
        re.compile(r"https://[^\s\"'<>]+X-Goog-Signature=[^\s\"'<>]+"),
        "[REDACTED:signed-url]",
    ),
    RedactionRule(
        re.compile(r"-----BEGIN [A-Z ]*PRIVATE KEY-----.*?-----END [A-Z ]*PRIVATE KEY-----", re.S),
        "[REDACTED:private-key]",
    ),
    RedactionRule(re.compile(r"\bBearer\s+[A-Za-z0-9._~+/=-]+"), "Bearer [REDACTED]"),
    RedactionRule(re.compile(r"\bya29\.[A-Za-z0-9._-]+"), "[REDACTED:oauth-token]"),
    RedactionRule(
        re.compile(r"\beyJ[A-Za-z0-9_-]{8,}\.[A-Za-z0-9_-]{8,}\.[A-Za-z0-9_-]{8,}"),
        "[REDACTED:jwt]",
    ),
    RedactionRule(
        re.compile(r"(?i)\b(password|passwd|secret|token|api[_-]?key)(\s*[=:]\s*)\S+"),
        r"\1\2[REDACTED]",
    ),
)

# POSIX absolute, home-relative, file URI, UNC and backslash-led, drive-letter, and a leading
# traversal segment. A segment such as "..notes" is not a traversal.
_LOCAL_PATH = re.compile(r"^(/|~/|file:|\\|[A-Za-z]:|\.\.(?:[\\/]|$))")


class Redaction(NamedTuple):
    text: str
    count: int


def redact(text: str) -> Redaction:
    """Replace credential material. Idempotent: redact(redact(x).text).text == redact(x).text."""
    total = 0
    for rule in REDACTION_RULES:
        text, found = rule.pattern.subn(rule.replacement, text)
        total += found
    return Redaction(text, total)


def _screen_node(
    value: Any,
    path: str,
    *,
    forbidden: frozenset[str],
    reject_strings: bool,
) -> None:
    if isinstance(value, str):
        if reject_strings and redact(value).count:
            raise ProtocolError(
                ErrorCode.CLIENT_CONTROLLED_FIELD,
                "credential material is not accepted in a request",
                field=path,
            )
        if reject_strings and _LOCAL_PATH.match(value):
            raise ProtocolError(
                ErrorCode.CLIENT_CONTROLLED_FIELD,
                "local paths are not accepted in a request",
                field=path,
            )
    elif isinstance(value, Mapping):
        _screen(value, forbidden=forbidden, reject_strings=reject_strings, prefix=f"{path}.")
    elif isinstance(value, list | tuple):
        for index, item in enumerate(value):
            _screen_node(item, f"{path}[{index}]", forbidden=forbidden, reject_strings=reject_strings)


def _screen(
    payload: Mapping[str, Any],
    *,
    forbidden: frozenset[str],
    reject_strings: bool,
    prefix: str = "",
) -> None:
    for key, value in payload.items():
        path = f"{prefix}{key}"
        if key in forbidden:
            raise ProtocolError(
                ErrorCode.CLIENT_CONTROLLED_FIELD,
                f"client may not set {path}",
                field=path,
            )
        _screen_node(value, path, forbidden=forbidden, reject_strings=reject_strings)


# ---------------------------------------------------------------------------
# Canonical bytes and digests
# ---------------------------------------------------------------------------


def canonical_bytes(payload: Any) -> bytes:
    """Canonical JSON: sorted keys, no insignificant whitespace, UTF-8, no NaN."""
    return json.dumps(
        payload,
        sort_keys=True,
        separators=(",", ":"),
        ensure_ascii=False,
        allow_nan=False,
    ).encode("utf-8")


def sha256_hex(data: bytes) -> str:
    return hashlib.sha256(data).hexdigest()


def verify_sha256(data: bytes, expected: str) -> None:
    if not hmac.compare_digest(sha256_hex(data), expected.lower()):
        raise ProtocolError(ErrorCode.CHECKSUM_MISMATCH, "content checksum does not match")


def manifest_canonical_bytes(manifest: CreateRunManifest) -> bytes:
    return canonical_bytes(manifest.model_dump(mode="json", exclude_none=True))


def manifest_digest(manifest: CreateRunManifest) -> str:
    return sha256_hex(manifest_canonical_bytes(manifest))


def action_digest(*, run_id: str, action: str, body: Mapping[str, Any]) -> str:
    return sha256_hex(canonical_bytes({"run_id": run_id, "action": action, "body": dict(body)}))


# ---------------------------------------------------------------------------
# Parsing and version gating
# ---------------------------------------------------------------------------

def _check_version(payload: Mapping[str, Any]) -> None:
    if "protocol_version" not in payload:
        raise ProtocolError(ErrorCode.INVALID_REQUEST, "protocol_version is required")
    version = payload["protocol_version"]
    if not isinstance(version, str) or version not in SUPPORTED_PROTOCOL_VERSIONS:
        raise ProtocolError(
            ErrorCode.UNSUPPORTED_VERSION,
            "protocol version is not supported",
            version=str(version),
        )


def _from_validation_error(exc: ValidationError) -> ProtocolError:
    errors = exc.errors()
    fields = [".".join(str(part) for part in err["loc"]) for err in errors]
    for err in errors:
        if "BLOCKING:" in err["msg"]:
            return ProtocolError(
                ErrorCode.BLOCKING_FIELD,
                err["msg"].split("Value error, ", 1)[-1],
                fields=fields,
            )
    for err in errors:
        if err["type"] == "extra_forbidden":
            return ProtocolError(
                ErrorCode.UNKNOWN_FIELD,
                "unknown field is not part of protocol v1",
                fields=fields,
            )
    return ProtocolError(
        ErrorCode.INVALID_REQUEST,
        "request does not match protocol v1",
        fields=fields,
    )


def _validate[M: BaseModel](model: type[M], payload: Mapping[str, Any]) -> M:
    try:
        return model.model_validate(payload)
    except ValidationError as exc:
        raise _from_validation_error(exc) from exc


def _require_mapping(payload: Any) -> Mapping[str, Any]:
    if not isinstance(payload, Mapping):
        raise ProtocolError(ErrorCode.INVALID_REQUEST, "payload must be a JSON object")
    return payload


def parse_request[M: BaseModel](model: type[M], payload: Any) -> M:
    """Parse a client request: version gate, client-field screen, then strict validation."""
    body = _require_mapping(payload)
    _check_version(body)
    _screen(body, forbidden=CLIENT_CONTROLLED_FIELDS, reject_strings=True)
    return _validate(model, body)


def parse_document[M: BaseModel](model: type[M], payload: Any) -> M:
    """Parse a runner- or controller-written document. Text is redacted, never rejected.

    Only top-level keys are screened against the client-controlled list. Nested keys are
    governed by each model's extra="forbid": a controller document legitimately carries a
    name such as image_digest inside its pinned compatibility tuple.
    """
    body = _require_mapping(payload)
    _check_version(body)
    for key in body:
        if key in DOCUMENT_FORBIDDEN_FIELDS:
            raise ProtocolError(
                ErrorCode.CLIENT_CONTROLLED_FIELD,
                f"client may not set {key}",
                field=key,
            )
    return _validate(model, body)


def manifest_from_request(request: CreateRunRequest) -> CreateRunManifest:
    return CreateRunManifest.model_validate(request.model_dump(exclude={"canonical_digest"}))


def parse_create_request(payload: Any) -> CreateRunRequest:
    """Parse a create request and verify its canonical digest against the manifest."""
    request = parse_request(CreateRunRequest, payload)
    computed = manifest_digest(manifest_from_request(request))
    if computed != request.canonical_digest:
        raise ProtocolError(
            ErrorCode.DIGEST_MISMATCH,
            "canonical_digest does not match the manifest",
            computed=computed,
        )
    return request


def parse_start_request(payload: Any) -> StartRunRequest:
    return parse_request(StartRunRequest, payload)


BULK_CANCEL_LIMIT = 50


class BulkCancelRequest(ActionRequest):
    """An operator's explicit selection. Each run id appears at most once."""

    run_ids: list[RunId] = Field(min_length=1, max_length=BULK_CANCEL_LIMIT)

    @field_validator("run_ids")
    @classmethod
    def _unique(cls, value: list[str]) -> list[str]:
        if len(set(value)) != len(value):
            raise ValueError("run_ids must not repeat a run id")
        return value


def parse_bulk_cancel_request(payload: Any) -> BulkCancelRequest:
    return parse_request(BulkCancelRequest, payload)


def parse_retry_request(payload: Any) -> RetryRequest:
    return parse_request(RetryRequest, payload)


def parse_cancel_request(payload: Any) -> ActionRequest:
    return parse_request(ActionRequest, payload)


# ---------------------------------------------------------------------------
# Field origin classification (the schema's authority rules)
# ---------------------------------------------------------------------------


def _origins(model: type[BaseModel], default: Origin, *, controller_identity: bool) -> dict[str, Origin]:
    """Origin of every field. Identity fields are controller-assigned when the flag is set."""
    return {
        name: Origin.CONTROLLER_ASSIGNED
        if controller_identity and name in IDENTITY_FIELDS
        else default
        for name in model.model_fields
    }


FIELD_ORIGINS: Mapping[str, Mapping[str, Origin]] = {
    "CreateRunRequest": _origins(CreateRunRequest, Origin.CLIENT_MANIFEST, controller_identity=False),
    "StartRunRequest": _origins(StartRunRequest, Origin.CLIENT_MANIFEST, controller_identity=False),
    "RetryRequest": _origins(RetryRequest, Origin.CLIENT_MANIFEST, controller_identity=False),
    "AcceptedReceipt": _origins(AcceptedReceipt, Origin.SERVER_DERIVED, controller_identity=True),
    "WorkerBootstrap": _origins(WorkerBootstrap, Origin.SERVER_DERIVED, controller_identity=True),
    "ClaimDocument": _origins(ClaimDocument, Origin.RUNNER_REPORTED, controller_identity=True),
    "StatusReport": _origins(StatusReport, Origin.RUNNER_REPORTED, controller_identity=True),
    "RunResultDocument": _origins(RunResultDocument, Origin.RUNNER_REPORTED, controller_identity=True),
    "RunView": _origins(RunView, Origin.SERVER_DERIVED, controller_identity=False),
}


# ---------------------------------------------------------------------------
# Server clock, freshness, deadlines
# ---------------------------------------------------------------------------


def freshness(
    *,
    last_seen_at: datetime | None,
    now: datetime,
    stale_after_seconds: int,
    skew_seconds: int,
) -> Freshness:
    """Liveness from server-received time only. A last_seen in the future is an anomaly."""
    if last_seen_at is None:
        return Freshness.STALE
    if last_seen_at > now + timedelta(seconds=skew_seconds):
        return Freshness.CLOCK_ANOMALY
    if now - last_seen_at > timedelta(seconds=stale_after_seconds):
        return Freshness.STALE
    return Freshness.FRESH


def classify_worker_timestamp(
    observed: datetime,
    *,
    now: datetime,
    skew_seconds: int,
    previous: datetime | None = None,
) -> TimestampVerdict:
    """Informational only: verdicts never move liveness, deadlines, or freshness."""
    if observed > now + timedelta(seconds=skew_seconds):
        return TimestampVerdict.FUTURE
    if previous is not None and observed < previous:
        return TimestampVerdict.BACKWARD
    return TimestampVerdict.ACCEPTED


def deadline_passed(now: datetime, deadline_at: datetime) -> bool:
    return now >= deadline_at


def admission_deadline(*, admitted_at: datetime, max_runtime_seconds: int) -> datetime:
    """Deadline from the server clock, once, at admission. Retries never extend it."""
    return admitted_at + timedelta(seconds=max_runtime_seconds)


def upload_deadline(*, admitted_at: datetime, deadline_at: datetime) -> datetime:
    return min(admitted_at + timedelta(seconds=UPLOAD_WINDOW_SECONDS), deadline_at)


# ---------------------------------------------------------------------------
# Run state machine and compare-and-set
# ---------------------------------------------------------------------------


def can_transition(source: RunState, target: RunState) -> bool:
    return target in ALLOWED_TRANSITIONS[source]


def _check_cas(record: RunRecord, expected_version: int, expected_attempt: int) -> None:
    if expected_attempt != record.attempt:
        raise ProtocolError(
            ErrorCode.STALE_ATTEMPT,
            "request names an attempt that is no longer current",
            current_attempt=record.attempt,
            expected_attempt=expected_attempt,
        )
    if expected_version != record.version:
        raise ProtocolError(
            ErrorCode.VERSION_CONFLICT,
            "run changed since it was read",
            current_version=record.version,
            expected_version=expected_version,
        )


def _require_current(
    record: RunRecord,
    *,
    attempt: int,
    fence: int,
    claim_id: str,
) -> None:
    if attempt != record.attempt or fence != record.fence:
        raise ProtocolError(
            ErrorCode.STALE_ATTEMPT,
            "document names an attempt or fence that is no longer current",
            current_attempt=record.attempt,
        )
    if record.claim_id is None or claim_id != record.claim_id:
        raise ProtocolError(ErrorCode.STALE_ATTEMPT, "document claim does not match the dispatch")


def check_selection_publishable(
    record: RunRecord,
    *,
    attempt: int,
    fence: int,
    claim_id: str,
    expected_attempt: int,
    expected_version: int,
) -> None:
    """Refuse a checkpoint selection unless it names the current identity (DPES-869 section 15.5).

    The publisher's compare-and-set preconditions and the document's identity both must match the
    controller's run record. A stale attempt is refused here, before any selection is written.
    """
    _check_cas(record, expected_version, expected_attempt)
    _require_current(record, attempt=attempt, fence=fence, claim_id=claim_id)


def settle_capacity(record: RunRecord, *, now: datetime) -> RunRecord:
    """Release capacity only after the caller confirms all attempt executions are stopped.

    The caller must also establish that no outstanding launch can start an execution later.
    A lifecycle failure or elapsed deadline alone is not sufficient evidence.
    """
    if record.state not in TERMINAL_STATES:
        raise ProtocolError(
            ErrorCode.ILLEGAL_TRANSITION,
            "capacity can only settle for a terminal attempt",
            state=record.state.value,
        )
    if record.reserved_seconds == 0:
        return record
    return record.model_copy(
        update={
            "version": record.version + 1,
            "consumed_seconds": record.consumed_seconds + _settled_seconds(record, now),
            "reserved_seconds": 0,
            "unsettled_reconciles": 0,
            "next_reconcile_at": None,
        }
    )


def _settled_seconds(record: RunRecord, now: datetime) -> int:
    """Seconds charged at termination: elapsed since dispatch, never above the reservation."""
    if record.dispatched_at is None:
        return record.reserved_seconds
    elapsed = math.ceil((now - record.dispatched_at).total_seconds())
    return min(record.reserved_seconds, max(0, elapsed))


def transition(
    record: RunRecord,
    *,
    to: RunState,
    expected_version: int,
    expected_attempt: int,
    now: datetime,
    failure: TypedFailure | None = None,
) -> RunRecord:
    """The only function that changes state. CAS first, then the protocol table."""
    _check_cas(record, expected_version, expected_attempt)
    if record.state in TERMINAL_STATES:
        raise ProtocolError(
            ErrorCode.TERMINAL_STATE,
            "terminal states never change for an attempt",
            state=record.state.value,
        )
    if not can_transition(record.state, to):
        raise ProtocolError(
            ErrorCode.ILLEGAL_TRANSITION,
            "transition is not in the protocol table",
            from_state=record.state.value,
            to_state=to.value,
        )
    carries_failure = to in TERMINAL_STATES or to is RunState.CANCEL_REQUESTED
    if failure is not None and not carries_failure:
        raise ValueError("a failure may only accompany a terminal or cancel_requested state")
    if failure is not None:
        next_failure: TypedFailure | None = failure
    else:
        next_failure = record.failure if carries_failure else None
    update: dict[str, Any] = {
        "state": to,
        "version": record.version + 1,
        "state_entered_at": now,
        "failure": next_failure,
    }
    return record.model_copy(update=update)


def admit_start(
    record: RunRecord,
    request: StartRunRequest,
    *,
    observed: "ObservedInput | None",
    compatibility: ToolCompatibility,
    now: datetime,
) -> RunRecord:
    """First start: verify the uploaded input against the accepted manifest, then queue."""
    if request.canonical_digest != record.manifest_digest:
        raise ProtocolError(
            ErrorCode.REQUEST_DIGEST_CONFLICT,
            "start names a canonical manifest that differs from the accepted one",
        )
    if record.state is not RunState.AWAITING_UPLOAD:
        raise ProtocolError(
            ErrorCode.ILLEGAL_TRANSITION,
            "only an awaiting-upload run can be started",
            state=record.state.value,
        )
    closes = upload_deadline(admitted_at=record.admitted_at, deadline_at=record.deadline_at)
    if deadline_passed(now, closes):
        raise ProtocolError(ErrorCode.DEADLINE_EXCEEDED, "the upload window has closed")
    verify_source_upload(record, request.source_upload, observed)
    queued = transition(
        record,
        to=RunState.QUEUED,
        expected_version=record.version,
        expected_attempt=record.attempt,
        now=now,
    )
    receipt_source = BootstrapSource(
        object_name=object_name(ObjectKind.INPUT, run_id=record.run_id),
        generation=request.source_upload.object_generation,
        sha256=request.source_upload.sha256,
        size_bytes=request.source_upload.size_bytes,
        base_sha=record.manifest.source.base_sha,
    )
    receipt = build_receipt(queued, source=receipt_source, compatibility=compatibility)
    return queued.model_copy(update={"receipt": receipt})


class ObservedInput(NamedTuple):
    """What the object store reports for the uploaded input object."""

    generation: int
    size_bytes: int
    sha256: str


def verify_source_upload(
    record: RunRecord,
    receipt: SourceReceipt,
    observed: ObservedInput | None,
) -> None:
    """Start-time binding: client-claimed bytes must match the accepted manifest and the store."""
    upload = record.manifest.source_upload
    if receipt.sha256 != upload.sha256 or receipt.size_bytes != upload.size_bytes:
        raise ProtocolError(
            ErrorCode.SOURCE_MISMATCH,
            "start names source bytes that differ from the accepted manifest",
        )
    if observed is None:
        raise ProtocolError(ErrorCode.UPLOAD_MISMATCH, "the uploaded input object is missing")
    if observed.sha256 != upload.sha256 or observed.size_bytes != upload.size_bytes:
        raise ProtocolError(
            ErrorCode.UPLOAD_MISMATCH,
            "uploaded object checksum or size does not match the manifest",
        )
    if observed.generation != receipt.object_generation:
        raise ProtocolError(
            ErrorCode.UPLOAD_MISMATCH,
            "uploaded object generation does not match the start receipt",
        )


def replay_start(record: RunRecord, request: StartRunRequest) -> AcceptedReceipt:
    """A repeated start returns the recorded receipt; different content is a conflict."""
    if record.receipt is None:
        raise ProtocolError(
            ErrorCode.ILLEGAL_TRANSITION,
            "run has not been started",
            state=record.state.value,
        )
    if request.canonical_digest != record.manifest_digest:
        raise ProtocolError(
            ErrorCode.REQUEST_DIGEST_CONFLICT,
            "start names a canonical manifest that differs from the accepted one",
        )
    sent = request.source_upload
    recorded = record.receipt.source
    if (
        sent.sha256 != recorded.sha256
        or sent.size_bytes != recorded.size_bytes
        or sent.object_generation != recorded.generation
    ):
        raise ProtocolError(
            ErrorCode.SOURCE_MISMATCH,
            "start names source bytes that differ from the recorded receipt",
        )
    return record.receipt


def begin_dispatch(
    record: RunRecord,
    *,
    claim_id: str,
    expected_version: int,
    expected_attempt: int,
    now: datetime,
    dispatch_order: int = 0,
) -> RunRecord:
    if dispatch_order < 0:
        raise ProtocolError(ErrorCode.INVALID_REQUEST, "dispatch order cannot be negative")
    dispatched = transition(
        record,
        to=RunState.DISPATCHING,
        expected_version=expected_version,
        expected_attempt=expected_attempt,
        now=now,
    )
    return dispatched.model_copy(
        update={
            "claim_id": claim_id,
            "dispatched_at": now,
            "dispatch_order": dispatch_order,
            # Reserve the whole budget until execution termination is confirmed.
            "reserved_seconds": record.manifest.budget.max_runtime_seconds,
        }
    )


def accept_claim(record: RunRecord, claim: ClaimDocument, *, now: datetime) -> RunRecord:
    """The runner's create-only claim proves it took this attempt. A late claim revives reconciling."""
    _require_current(record, attempt=claim.attempt, fence=claim.fence, claim_id=claim.claim_id)
    started = transition(
        record,
        to=RunState.STARTING,
        expected_version=record.version,
        expected_attempt=record.attempt,
        now=now,
    )
    return started.model_copy(update={"last_seen_at": now})


_STATUS_TARGETS: Mapping[tuple[str, RunState], RunState] = {
    ("running", RunState.STARTING): RunState.RUNNING,
    ("running", RunState.RECONCILING): RunState.RUNNING,
    ("collecting", RunState.STARTING): RunState.COLLECTING,
    ("collecting", RunState.RUNNING): RunState.COLLECTING,
    ("collecting", RunState.RECONCILING): RunState.COLLECTING,
}

_STATUS_STATES: frozenset[RunState] = frozenset(
    {
        RunState.STARTING,
        RunState.RUNNING,
        RunState.COLLECTING,
        RunState.RECONCILING,
        RunState.CANCEL_REQUESTED,
    }
)


def accept_status(
    record: RunRecord,
    report: StatusReport,
    *,
    now: datetime,
    skew_seconds: int,
) -> tuple[RunRecord, TimestampVerdict]:
    """Accept one runner status. Liveness uses server receipt time and a strictly higher seq.

    A repeated seq is an idempotent replay: it changes nothing and creates no liveness.
    """
    if record.state in TERMINAL_STATES:
        raise ProtocolError(
            ErrorCode.TERMINAL_STATE,
            "a terminal attempt accepts no further status",
            state=record.state.value,
        )
    _require_current(record, attempt=report.attempt, fence=report.fence, claim_id=report.claim_id)
    if record.state not in _STATUS_STATES:
        raise ProtocolError(
            ErrorCode.ILLEGAL_TRANSITION,
            "status is not accepted in this state",
            state=record.state.value,
        )
    if report.seq < record.last_status_seq or report.log_watermark < record.last_log_watermark:
        raise ProtocolError(
            ErrorCode.SEQUENCE_REGRESSED,
            "status sequence or log watermark moved backwards",
            last_seq=record.last_status_seq,
            seq=report.seq,
        )
    verdict = classify_worker_timestamp(
        report.observed_at,
        now=now,
        skew_seconds=skew_seconds,
        previous=record.last_observed_at,
    )
    if report.seq == record.last_status_seq:
        return record, verdict

    observed_at = report.observed_at if verdict is TimestampVerdict.ACCEPTED else None
    updated = record.model_copy(
        update={
            "version": record.version + 1,
            "last_seen_at": now,
            "last_observed_at": observed_at or record.last_observed_at,
            "last_status_seq": report.seq,
            "last_log_watermark": report.log_watermark,
        }
    )
    target = _STATUS_TARGETS.get((report.phase, record.state))
    if target is not None:
        updated = transition(
            updated,
            to=target,
            expected_version=updated.version,
            expected_attempt=updated.attempt,
            now=now,
        )
    return updated, verdict


def accept_result(
    record: RunRecord,
    document: RunResultDocument,
    *,
    now: datetime,
) -> RunRecord:
    """Accept the runner's single create-only result. Cancellation and timeout fence it.

    Artifact checksums are verified by the caller with verify_artifacts before this call.
    """
    _require_current(record, attempt=document.attempt, fence=document.fence, claim_id=document.claim_id)
    if record.state in {RunState.SUCCEEDED, RunState.FAILED}:
        raise ProtocolError(
            ErrorCode.TERMINAL_STATE,
            "a result is already recorded for this attempt",
            state=record.state.value,
        )
    if record.state in {RunState.CANCEL_REQUESTED, RunState.CANCELLED, RunState.TIMED_OUT}:
        raise ProtocolError(ErrorCode.RESULT_FENCED, "cancellation or timeout fences the result")
    if deadline_passed(now, record.deadline_at):
        raise ProtocolError(ErrorCode.DEADLINE_EXCEEDED, "result arrived after the run deadline")
    current = record
    if current.state is not RunState.COLLECTING:
        current = transition(
            current,
            to=RunState.COLLECTING,
            expected_version=current.version,
            expected_attempt=current.attempt,
            now=now,
        )
    if document.outcome == "succeeded":
        return transition(
            current,
            to=RunState.SUCCEEDED,
            expected_version=current.version,
            expected_attempt=current.attempt,
            now=now,
        )
    return transition(
        current,
        to=RunState.FAILED,
        expected_version=current.version,
        expected_attempt=current.attempt,
        now=now,
        failure=failure(FailureCode.WORKER_FAILED),
    )


def request_cancel(record: RunRecord, *, now: datetime) -> tuple[RunRecord, bool]:
    """Owner cancel. Idempotent: returns (record, changed). Terminal runs are unchanged."""
    if record.state in TERMINAL_STATES or record.state is RunState.CANCEL_REQUESTED:
        return record, False
    target = (
        RunState.CANCELLED
        if record.state in {RunState.AWAITING_UPLOAD, RunState.QUEUED}
        else RunState.CANCEL_REQUESTED
    )
    updated = transition(
        record,
        to=target,
        expected_version=record.version,
        expected_attempt=record.attempt,
        now=now,
        failure=failure(FailureCode.OWNER_CANCELLED),
    )
    return updated, True


def retry(
    record: RunRecord,
    *,
    request_uuid: UUID,
    expected_version: int,
    expected_attempt: int,
    now: datetime,
) -> RunRecord:
    """Owner retry. Opens attempt N+1 with a new fence, under the original deadline.

    Only a failed attempt whose failure code is retryable may retry, and only within the
    attempt budget and before the original deadline. The retry UUID is recorded so a
    later replay of it is recognised rather than mistaken for a new request.
    """
    _check_cas(record, expected_version, expected_attempt)
    if record.state is not RunState.FAILED or record.failure is None:
        raise ProtocolError(
            ErrorCode.RETRY_NOT_ALLOWED,
            "only a failed attempt can be retried",
            state=record.state.value,
        )
    if record.reserved_seconds > 0:
        raise ProtocolError(
            ErrorCode.RETRY_NOT_ALLOWED,
            "the previous attempt still holds capacity pending confirmed execution termination",
        )
    if not record.failure.retryable:
        raise ProtocolError(
            ErrorCode.RETRY_NOT_ALLOWED,
            "the failure is not retryable",
            failure=record.failure.code.value,
        )
    if deadline_passed(now, record.deadline_at):
        raise ProtocolError(
            ErrorCode.DEADLINE_EXCEEDED,
            "the original deadline has passed, so retry cannot extend it",
        )
    if record.attempt >= record.max_attempts:
        raise ProtocolError(
            ErrorCode.RETRY_NOT_ALLOWED,
            "attempt budget is exhausted",
            attempt=record.attempt,
            max_attempts=record.max_attempts,
        )
    return record.model_copy(
        update={
            "state": RunState.QUEUED,
            "version": record.version + 1,
            "attempt": record.attempt + 1,
            "fence": record.fence + 1,
            "state_entered_at": now,
            "claim_id": None,
            "retry_request_uuids": (*record.retry_request_uuids, request_uuid),
            "last_seen_at": None,
            "last_observed_at": None,
            "last_status_seq": 0,
            "last_log_watermark": 0,
            "log_retained_from": 1,
            "failure": None,
            "execution_name": None,
            "unknown_launch_alerted_at": None,
        }
    )


# ---------------------------------------------------------------------------
# Timeouts: pure policy. The caller applies the decision with CAS.
# ---------------------------------------------------------------------------


class TimeoutDecision(NamedTuple):
    to: RunState
    failure: TypedFailure | None


def is_owner_cancelled(record: RunRecord) -> bool:
    return record.failure is not None and record.failure.code is FailureCode.OWNER_CANCELLED


def liveness_decision(
    record: RunRecord,
    *,
    now: datetime,
    startup_timeout_seconds: int,
    stale_after_seconds: int,
    skew_seconds: int,
    queued_deadline_seconds: int,
) -> TimeoutDecision | None:
    """Which transition, if any, time alone requires. Only server time is consulted.

    Deadlines are absolute from admission. Stale liveness moves a run to reconciling, and
    only a stale reconciling run that stays stale fails. A clock anomaly changes nothing
    beyond deadlines, so a skewed server clock cannot create or destroy liveness.
    """
    state = record.state
    if state in TERMINAL_STATES:
        return None
    if state is RunState.AWAITING_UPLOAD:
        closes = upload_deadline(admitted_at=record.admitted_at, deadline_at=record.deadline_at)
        if deadline_passed(now, closes):
            return TimeoutDecision(RunState.TIMED_OUT, failure(FailureCode.UPLOAD_EXPIRED))
        return None
    if state is RunState.QUEUED:
        queued_expired = (now - record.state_entered_at).total_seconds() >= queued_deadline_seconds
        if deadline_passed(now, record.deadline_at) or queued_expired:
            return TimeoutDecision(RunState.TIMED_OUT, failure(FailureCode.QUEUED_EXPIRED))
        return None

    expired = deadline_passed(now, record.deadline_at)
    fresh = freshness(
        last_seen_at=record.last_seen_at,
        now=now,
        stale_after_seconds=stale_after_seconds,
        skew_seconds=skew_seconds,
    )
    stale = fresh is Freshness.STALE

    if state is RunState.CANCEL_REQUESTED:
        if is_owner_cancelled(record):
            # Neither time nor silence completes an owner cancel. Only confirmed termination does,
            # and the broker decides that from the launcher, never from the clock.
            return None
        if not (expired or stale):
            return None
        if expired:
            return TimeoutDecision(RunState.TIMED_OUT, failure(FailureCode.DEADLINE_EXCEEDED))
        return TimeoutDecision(RunState.FAILED, failure(FailureCode.HEARTBEAT_LOST))

    if expired:
        return TimeoutDecision(RunState.CANCEL_REQUESTED, failure(FailureCode.DEADLINE_EXCEEDED))
    if fresh is Freshness.CLOCK_ANOMALY:
        return None

    in_state = now - record.state_entered_at
    startup_limit = timedelta(seconds=startup_timeout_seconds)
    if state is RunState.DISPATCHING:
        return TimeoutDecision(RunState.RECONCILING, None) if in_state > startup_limit else None
    if state is RunState.STARTING:
        if in_state > startup_limit:
            return TimeoutDecision(RunState.FAILED, failure(FailureCode.STARTUP_TIMEOUT))
        return TimeoutDecision(RunState.RECONCILING, None) if stale else None
    if state in {RunState.RUNNING, RunState.COLLECTING}:
        return TimeoutDecision(RunState.RECONCILING, None) if stale else None
    if state is RunState.RECONCILING:
        if stale and in_state >= timedelta(seconds=stale_after_seconds):
            code = (
                FailureCode.LAUNCH_UNCERTAIN
                if record.last_seen_at is None
                else FailureCode.HEARTBEAT_LOST
            )
            return TimeoutDecision(RunState.FAILED, failure(code))
        return None
    return None


UNKNOWN_LAUNCH_ALERT_SECONDS = STARTUP_TIMEOUT_SECONDS


def unknown_launch_alert_due(record: RunRecord, *, now: datetime) -> bool:
    """True once an attempt that never reported has held its capacity ten minutes past dispatch.

    Due once per attempt. The caller records the alert with compare-and-set. Capacity is not
    touched: the reservation stays until the launcher confirms absence or termination.
    """
    if record.unknown_launch_alerted_at is not None or record.last_seen_at is not None:
        return False
    if record.dispatched_at is None or record.reserved_seconds == 0:
        return False
    return (now - record.dispatched_at).total_seconds() >= UNKNOWN_LAUNCH_ALERT_SECONDS


# ---------------------------------------------------------------------------
# Idempotency admission
# ---------------------------------------------------------------------------

_EPOCH = datetime(1970, 1, 1, tzinfo=UTC)


def uuid7_timestamp(value: UUID) -> datetime:
    """The millisecond timestamp stamped into a version 7 UUID's top 48 bits."""
    if value.version != 7 or value.variant != uuid.RFC_4122:
        raise ProtocolError(ErrorCode.INVALID_REQUEST, "request_uuid must be a version 7 UUID")
    return _EPOCH + timedelta(milliseconds=value.int >> 80)


def effective_admission_floor(
    *,
    now: datetime,
    restore_floor: datetime | None,
    ttl: timedelta = IDEMPOTENCY_TTL,
) -> datetime:
    """Oldest request UUID timestamp still admissible as a new request.

    A restore from backup raises the floor to its completion time, so a UUID stamped before
    the restore whose dedup record is missing is refused, never recreated.
    """
    window_start = now - ttl
    if restore_floor is None:
        return window_start
    return max(window_start, restore_floor)


def decide_admission(
    *,
    existing: IdempotencyRecord | None,
    orphan_run_id: str | None,
    request_uuid: UUID,
    digest: str,
    now: datetime,
    admission_floor: datetime,
    skew_seconds: int,
) -> Admission:
    """Decide create versus replay for (owner, request UUID). A refusal never creates anything.

    orphan_run_id is a run already stored under this request UUID whose dedup record is
    gone. Creating again would mint a second run, so it is refused as expired instead.
    """
    if existing is not None:
        if existing.digest != digest:
            raise ProtocolError(
                ErrorCode.REQUEST_DIGEST_CONFLICT,
                "request UUID was already used with different content",
                run_id=existing.run_id,
            )
        if now >= existing.expires_at:
            raise ProtocolError(
                ErrorCode.IDEMPOTENCY_EXPIRED,
                "request UUID is past its idempotency lifetime; submit a new request",
                run_id=existing.run_id,
                reason="ttl",
            )
        return Admission.REPLAY
    if orphan_run_id is not None:
        raise ProtocolError(
            ErrorCode.IDEMPOTENCY_EXPIRED,
            "request UUID's dedup record is gone but its run exists; no new run is created",
            run_id=orphan_run_id,
            reason="record_missing_run_present",
        )
    stamped = uuid7_timestamp(request_uuid)
    if stamped > now + timedelta(seconds=skew_seconds):
        raise ProtocolError(
            ErrorCode.INVALID_REQUEST,
            "request UUID timestamp is ahead of server time",
        )
    if stamped < admission_floor:
        raise ProtocolError(
            ErrorCode.IDEMPOTENCY_EXPIRED,
            "request UUID predates the admission window; submit a new request",
            reason="before_admission_floor",
        )
    return Admission.CREATE


def new_idempotency_record(
    *,
    owner_id: str,
    request_uuid: UUID,
    digest: str,
    run_id: str,
    now: datetime,
) -> IdempotencyRecord:
    return IdempotencyRecord(
        owner_id=owner_id,
        request_uuid=request_uuid,
        digest=digest,
        run_id=run_id,
        created_at=now,
        expires_at=now + IDEMPOTENCY_TTL,
    )


# ---------------------------------------------------------------------------
# Receipts and bootstrap projection
# ---------------------------------------------------------------------------


def build_receipt(
    record: RunRecord,
    *,
    source: BootstrapSource,
    compatibility: ToolCompatibility,
) -> AcceptedReceipt:
    digest = sha256_hex(
        _projection_bytes(
            run_id=record.run_id,
            source=source,
            compatibility=compatibility,
            deadline_at=record.deadline_at,
            max_runtime_seconds=record.manifest.budget.max_runtime_seconds,
        )
    )
    return AcceptedReceipt(
        protocol_version=PROTOCOL_VERSION,
        run_id=record.run_id,
        state=record.state,
        version=record.version,
        attempt=record.attempt,
        manifest_digest=record.manifest_digest,
        source=source,
        compatibility=compatibility,
        deadline_at=record.deadline_at,
        max_runtime_seconds=record.manifest.budget.max_runtime_seconds,
        projection_digest=digest,
    )


def _projection_bytes(
    *,
    run_id: str,
    source: BootstrapSource,
    compatibility: ToolCompatibility,
    deadline_at: datetime,
    max_runtime_seconds: int,
) -> bytes:
    """The single serializer for the immutable identity. Receipt and bootstrap both use it."""
    return canonical_bytes(
        {
            "run_id": run_id,
            "source": source.model_dump(mode="json"),
            "compatibility": compatibility.model_dump(mode="json"),
            "deadline_at": deadline_at.astimezone(UTC).strftime("%Y-%m-%dT%H:%M:%S.%fZ"),
            "max_runtime_seconds": max_runtime_seconds,
        }
    )


def projection_bytes(model: AcceptedReceipt | WorkerBootstrap) -> bytes:
    return _projection_bytes(
        run_id=model.run_id,
        source=model.source,
        compatibility=model.compatibility,
        deadline_at=model.deadline_at,
        max_runtime_seconds=model.max_runtime_seconds,
    )


def projection_digest(model: AcceptedReceipt | WorkerBootstrap) -> str:
    return sha256_hex(projection_bytes(model))


def build_bootstrap(record: RunRecord) -> WorkerBootstrap:
    """Controller-produced bootstrap for the current attempt. Requires a dispatched run."""
    if record.state is not RunState.DISPATCHING or record.receipt is None or record.claim_id is None:
        raise ProtocolError(
            ErrorCode.ILLEGAL_TRANSITION,
            "a bootstrap needs a dispatched run with a receipt and a claim",
            state=record.state.value,
        )
    receipt = record.receipt
    return WorkerBootstrap(
        protocol_version=PROTOCOL_VERSION,
        run_id=record.run_id,
        attempt=record.attempt,
        fence=record.fence,
        claim_id=record.claim_id,
        source=receipt.source,
        compatibility=receipt.compatibility,
        deadline_at=receipt.deadline_at,
        max_runtime_seconds=receipt.max_runtime_seconds,
    )


def build_cancel(record: RunRecord) -> CancelDocument:
    """Controller-written cancel directive for an attempt. Needs a run that has a claim."""
    if record.claim_id is None:
        raise ProtocolError(
            ErrorCode.ILLEGAL_TRANSITION,
            "a cancel marker needs a run with a claim",
            state=record.state.value,
        )
    return CancelDocument(
        protocol_version=PROTOCOL_VERSION,
        run_id=record.run_id,
        attempt=record.attempt,
        fence=record.fence,
        claim_id=record.claim_id,
    )


def build_control(record: RunRecord) -> ControlDocument:
    """Controller-written identity of a dispatched attempt. Needs a dispatched run with a claim."""
    if record.state is not RunState.DISPATCHING or record.claim_id is None:
        raise ProtocolError(
            ErrorCode.ILLEGAL_TRANSITION,
            "a control document needs a dispatched run with a claim",
            state=record.state.value,
        )
    return ControlDocument(
        protocol_version=PROTOCOL_VERSION,
        run_id=record.run_id,
        attempt=record.attempt,
        fence=record.fence,
        claim_id=record.claim_id,
    )


def verify_bootstrap(bootstrap: WorkerBootstrap, receipt: AcceptedReceipt) -> None:
    """The bootstrap's immutable identity must match the accepted receipt byte for byte.

    attempt, fence, and claim are excluded by design: they change on retry while the
    receipt's identity does not.
    """
    if bootstrap.run_id != receipt.run_id:
        raise ProtocolError(ErrorCode.BOOTSTRAP_MISMATCH, "bootstrap run differs from the receipt")
    if projection_bytes(bootstrap) != projection_bytes(receipt):
        raise ProtocolError(
            ErrorCode.BOOTSTRAP_MISMATCH,
            "bootstrap projection does not match the accepted receipt",
        )
    if projection_digest(bootstrap) != receipt.projection_digest:
        raise ProtocolError(
            ErrorCode.BOOTSTRAP_MISMATCH,
            "bootstrap projection digest does not match the receipt",
        )


# ---------------------------------------------------------------------------
# Signed objects and capability rules
# ---------------------------------------------------------------------------


def object_name(
    kind: ObjectKind,
    *,
    run_id: str,
    attempt: int | None = None,
    seq: int | None = None,
) -> str:
    if not re.fullmatch(RUN_ID_PATTERN, run_id):
        raise ValueError("run_id does not match the protocol pattern")
    if kind is ObjectKind.INPUT:
        return f"runs/{run_id}/input/source.tar"
    if attempt is None or attempt < 1:
        raise ValueError("attempt is required for attempt-scoped objects")
    base = f"runs/{run_id}/attempts/{attempt:06d}"
    if kind is ObjectKind.CLAIM:
        return f"{base}/claim.json"
    if kind is ObjectKind.CONTROL:
        return f"{base}/control.json"
    if kind is ObjectKind.INTAKE:
        return f"{base}/intake.json"
    if kind is ObjectKind.CANCEL:
        return f"{base}/cancel.json"
    if kind is ObjectKind.STATUS:
        return f"{base}/status.json"
    if kind is ObjectKind.RESULT:
        return f"{base}/result.json"
    if kind is ObjectKind.CHECKPOINT:
        raise ValueError(
            "checkpoint objects are named by checkpoint_id; use checkpoint_object_names"
        )
    if seq is None or seq < 1:
        raise ValueError("seq is required for sequenced objects")
    return f"{base}/logs/{seq:010d}.ndjson"


def artifact_object_name(run_id: str, attempt: int, name: str) -> str:
    """Artifacts are scoped to their attempt, so a retry never reads an earlier attempt's bytes."""
    if not re.fullmatch(RUN_ID_PATTERN, run_id) or attempt < 1:
        raise ValueError("run_id or attempt is outside the protocol")
    return f"runs/{run_id}/attempts/{attempt:06d}/artifacts/{name}"


CHECKPOINT_ID_PATTERN = re.compile(r"^ck-[0-9]{4,12}$")


def checkpoint_object_names(run_id: str, attempt: int, checkpoint_id: str) -> dict[str, str]:
    """Checkpoint layout (DPES-897 decision 3): one directory per checkpoint id, holding exactly
    harness.bin, workspace.tar.gz and manifest.json. This is the single owner of that layout.
    """
    if not re.fullmatch(RUN_ID_PATTERN, run_id) or attempt < 1:
        raise ValueError("run_id or attempt is outside the protocol")
    if not CHECKPOINT_ID_PATTERN.fullmatch(checkpoint_id):
        raise ValueError("checkpoint_id does not match ck-<digits>")
    base = f"runs/{run_id}/attempts/{attempt:06d}/checkpoints/{checkpoint_id}"
    return {
        "harness": f"{base}/harness.bin",
        "workspace": f"{base}/workspace.tar.gz",
        "manifest": f"{base}/manifest.json",
    }


@dataclass(frozen=True)
class Capability:
    """A signed-URL capability. Controller reads use its own identity, never signed URLs."""

    kind: ObjectKind
    method: Literal["GET", "PUT"]
    holder: Literal["client", "runner"]
    ttl_seconds: int
    precondition: Literal["create_only", "generation_match", "none"]
    max_bytes: int | None
    content_type: str | None
    sha256_metadata: bool
    redacted: bool
    blocking: str | None = None


CAPABILITIES: Mapping[tuple[ObjectKind, str], Capability] = {
    (ObjectKind.INPUT, "PUT"): Capability(
        ObjectKind.INPUT, "PUT", "client", UPLOAD_WINDOW_SECONDS, "create_only",
        MAX_SOURCE_BYTES, "application/x-tar", True, False,
    ),
    (ObjectKind.INPUT, "GET"): Capability(
        ObjectKind.INPUT, "GET", "runner", INPUT_READ_TTL_SECONDS, "none",
        None, None, False, False,
    ),
    (ObjectKind.CLAIM, "PUT"): Capability(
        ObjectKind.CLAIM, "PUT", "runner", CLAIM_TTL_SECONDS, "create_only",
        MAX_CLAIM_BYTES, "application/json", False, False,
    ),
    (ObjectKind.CONTROL, "GET"): Capability(
        ObjectKind.CONTROL, "GET", "runner", ATTEMPT_CAPABILITY_TTL_SECONDS, "none",
        None, None, False, False,
    ),
    (ObjectKind.INTAKE, "GET"): Capability(
        ObjectKind.INTAKE, "GET", "runner", INTAKE_READ_TTL_SECONDS, "none",
        None, None, False, False,
    ),
    (ObjectKind.CANCEL, "GET"): Capability(
        ObjectKind.CANCEL, "GET", "runner", ATTEMPT_CAPABILITY_TTL_SECONDS, "none",
        None, None, False, False,
    ),
    (ObjectKind.STATUS, "PUT"): Capability(
        ObjectKind.STATUS, "PUT", "runner", ATTEMPT_CAPABILITY_TTL_SECONDS, "generation_match",
        MAX_STATUS_BYTES, "application/json", False, True,
    ),
    (ObjectKind.CHECKPOINT, "PUT"): Capability(
        ObjectKind.CHECKPOINT, "PUT", "runner", ATTEMPT_CAPABILITY_TTL_SECONDS, "create_only",
        MAX_CHECKPOINT_BYTES, "application/octet-stream", True, False, blocking="DPES-869",
    ),
    (ObjectKind.LOG, "PUT"): Capability(
        ObjectKind.LOG, "PUT", "runner", ATTEMPT_CAPABILITY_TTL_SECONDS, "create_only",
        MAX_LOG_OBJECT_BYTES, "application/x-ndjson", False, True,
    ),
    (ObjectKind.RESULT, "PUT"): Capability(
        ObjectKind.RESULT, "PUT", "runner", ATTEMPT_CAPABILITY_TTL_SECONDS, "create_only",
        MAX_RESULT_BYTES, "application/json", True, True,
    ),
}


def capability_expiry(
    kind: ObjectKind,
    method: Literal["GET", "PUT"],
    *,
    now: datetime,
    attempt_deadline: datetime,
) -> datetime:
    """The holder's TTL, clamped so no capability outlives the attempt deadline."""
    if deadline_passed(now, attempt_deadline):
        raise ProtocolError(ErrorCode.DEADLINE_EXCEEDED, "no capability is issued after the deadline")
    capability = CAPABILITIES[(kind, method)]
    return min(now + timedelta(seconds=capability.ttl_seconds), attempt_deadline)


def required_headers(
    kind: ObjectKind,
    method: Literal["GET", "PUT"],
    *,
    sha256_hex_value: str | None = None,
    expected_generation: int | None = None,
) -> dict[str, str]:
    capability = CAPABILITIES[(kind, method)]
    headers: dict[str, str] = {}
    if method == "PUT" and capability.content_type is not None:
        headers["content-type"] = capability.content_type
    if capability.precondition == "create_only":
        headers["x-goog-if-generation-match"] = "0"
    elif capability.precondition == "generation_match":
        if expected_generation is None:
            raise ValueError("generation_match capabilities need expected_generation")
        headers["x-goog-if-generation-match"] = str(expected_generation)
    if capability.sha256_metadata:
        if sha256_hex_value is None:
            raise ValueError("this capability binds a sha256 metadata header")
        headers["x-goog-meta-sha256"] = sha256_hex_value
    return headers


def verify_artifacts(document: RunResultDocument, read: Any) -> None:
    """Each artifact the result names must exist with the recorded size and checksum.

    read(name) returns the object bytes, or None when the object is absent.
    """
    for artifact in document.artifacts:
        data = read(artifact.name)
        if data is None:
            raise ProtocolError(
                ErrorCode.CHECKSUM_MISMATCH,
                "result names an artifact that is not present",
                artifact=artifact.name,
            )
        if len(data) != artifact.size_bytes:
            raise ProtocolError(
                ErrorCode.CHECKSUM_MISMATCH,
                "artifact size does not match the result",
                artifact=artifact.name,
            )
        verify_sha256(data, artifact.sha256)


# ---------------------------------------------------------------------------
# Cursors and paging
# ---------------------------------------------------------------------------


@dataclass(frozen=True)
class Cursor:
    run_id: str
    attempt: int
    stream: CursorStream
    seq: int


def encode_cursor(cursor: Cursor, *, version: int = 1) -> str:
    payload = {
        "v": version,
        "r": cursor.run_id,
        "a": cursor.attempt,
        "s": cursor.stream.value,
        "q": cursor.seq,
    }
    return base64.urlsafe_b64encode(canonical_bytes(payload)).rstrip(b"=").decode("ascii")


def decode_cursor(token: str, *, run_id: str, stream: CursorStream) -> Cursor:
    """Decode a cursor. An unsupported version refuses; the run stays readable through GET."""
    try:
        padded = token + "=" * (-len(token) % 4)
        payload = json.loads(base64.urlsafe_b64decode(padded.encode("ascii")))
    except (ValueError, binascii.Error, UnicodeError) as exc:
        raise ProtocolError(ErrorCode.CURSOR_INVALID, "cursor is malformed") from exc
    if not isinstance(payload, dict):
        raise ProtocolError(ErrorCode.CURSOR_INVALID, "cursor is malformed")
    version = payload.get("v")
    if version not in SUPPORTED_CURSOR_VERSIONS:
        raise ProtocolError(
            ErrorCode.CURSOR_VERSION_UNSUPPORTED,
            "cursor version is not supported; the run remains readable",
            version=version,
            run_id=run_id,
        )
    try:
        cursor = Cursor(
            run_id=str(payload["r"]),
            attempt=int(payload["a"]),
            stream=CursorStream(payload["s"]),
            seq=int(payload["q"]),
        )
    except (KeyError, TypeError, ValueError) as exc:
        raise ProtocolError(ErrorCode.CURSOR_INVALID, "cursor is malformed") from exc
    if cursor.run_id != run_id or cursor.stream is not stream or cursor.seq < 1 or cursor.attempt < 1:
        raise ProtocolError(ErrorCode.CURSOR_INVALID, "cursor does not belong to this request")
    return cursor


@dataclass(frozen=True)
class PagePlan:
    first_seq: int
    last_seq: int
    next_seq: int
    trimmed: bool
    gap: tuple[int, int] | None


def plan_page(*, requested_seq: int, retained_from: int, watermark: int, limit: int) -> PagePlan:
    """Plan one page. A cursor into trimmed history is answered with the gap disclosed."""
    if requested_seq < 1 or limit < 1 or limit > MAX_LOG_LIMIT:
        raise ProtocolError(ErrorCode.CURSOR_INVALID, "cursor position or limit is out of range")
    if requested_seq > watermark + 1:
        raise ProtocolError(
            ErrorCode.CURSOR_INVALID,
            "cursor is ahead of the durable watermark",
            watermark=watermark,
        )
    trimmed = requested_seq < retained_from
    gap = (requested_seq, retained_from - 1) if trimmed else None
    first = max(requested_seq, retained_from)
    last = min(watermark, first + limit - 1)
    next_seq = max(first, last + 1)
    return PagePlan(first, last, next_seq, trimmed, gap)


# ---------------------------------------------------------------------------
# Teardown outcomes
# ---------------------------------------------------------------------------


@dataclass(frozen=True)
class CleanupFacts:
    store_reachable: bool
    retention_exception: bool
    execution_terminal: bool
    capability_expiry_passed: bool
    data_present: bool
    deletion_started: bool
    records_past_retention: bool


def classify_cleanup(facts: CleanupFacts) -> CleanupOutcome:
    """Ordered precedence: the first matching rule wins."""
    if not facts.store_reachable:
        return CleanupOutcome.UNAVAILABLE
    if facts.retention_exception:
        return CleanupOutcome.RETAINED_EXCEPTION
    if not facts.execution_terminal:
        return CleanupOutcome.RUNNING
    if not facts.capability_expiry_passed:
        return CleanupOutcome.FENCED
    if not facts.data_present and facts.records_past_retention:
        return CleanupOutcome.EXPIRED
    if not facts.data_present:
        return CleanupOutcome.COMPLETE
    if facts.deletion_started:
        return CleanupOutcome.INCOMPLETE
    return CleanupOutcome.ELIGIBLE


# Durable data that still exists and will be deleted. RUNNING is excluded because the
# run is still live; RETAINED_EXCEPTION is excluded because deletion is deliberately off.
PENDING_DELETION_OUTCOMES: frozenset[CleanupOutcome] = frozenset(
    {CleanupOutcome.FENCED, CleanupOutcome.ELIGIBLE, CleanupOutcome.INCOMPLETE}
)


def teardown_status(record: RunRecord, facts: CleanupFacts) -> TeardownStatus:
    outcome = classify_cleanup(facts)
    return TeardownStatus(
        run_id=record.run_id,
        execution_state=record.state,
        data_cleanup=outcome,
        pending_deletion=outcome in PENDING_DELETION_OUTCOMES,
    )


# ---------------------------------------------------------------------------
# Views
# ---------------------------------------------------------------------------


def run_view(
    record: RunRecord,
    *,
    now: datetime,
    stale_after_seconds: int,
    skew_seconds: int,
) -> RunView:
    return RunView(
        run_id=record.run_id,
        owner_id=record.owner_id,
        state=record.state,
        version=record.version,
        attempt=record.attempt,
        max_attempts=record.max_attempts,
        deadline_at=record.deadline_at,
        server_time=now,
        freshness=freshness(
            last_seen_at=record.last_seen_at,
            now=now,
            stale_after_seconds=stale_after_seconds,
            skew_seconds=skew_seconds,
        ),
        last_seen_at=record.last_seen_at,
        failure=record.failure,
    )


def status_view(
    record: RunRecord,
    *,
    now: datetime,
    stale_after_seconds: int,
    skew_seconds: int,
    result_available: bool,
) -> StatusView:
    return StatusView(
        run=run_view(
            record,
            now=now,
            stale_after_seconds=stale_after_seconds,
            skew_seconds=skew_seconds,
        ),
        effective=EffectiveRun(
            model=record.manifest.model,
            resource_class=record.manifest.capacity,
            image_digest=record.image_digest,
        ),
        result_available=result_available,
        status_seq=record.last_status_seq,
        log_watermark=record.last_log_watermark,
        retained_from=record.log_retained_from,
    )


def snapshot(record: RunRecord, *, now: datetime) -> Snapshot:
    return Snapshot(
        state=record.state,
        version=record.version,
        attempt=record.attempt,
        server_time=now,
    )


SCHEMA_MODELS: Mapping[str, type[BaseModel]] = {
    "create-run-request": CreateRunRequest,
    "start-run-request": StartRunRequest,
    "retry-request": RetryRequest,
    "cancel-request": ActionRequest,
    "upload-capability": UploadCapability,
    "create-run-response": CreateRunResponse,
    "accepted-receipt": AcceptedReceipt,
    "worker-bootstrap": WorkerBootstrap,
    "claim-document": ClaimDocument,
    "control-document": ControlDocument,
    "worker-intake": WorkerIntake,
    "cancel-document": CancelDocument,
    "status-report": StatusReport,
    "run-result": RunResultDocument,
    "log-entry": LogEntry,
    "log-page": LogPage,
    "run-view": RunView,
    "status-view": StatusView,
    "run-event": RunEvent,
    "teardown-status": TeardownStatus,
    "error-body": ErrorBody,
}
