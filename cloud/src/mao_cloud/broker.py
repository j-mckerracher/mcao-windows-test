"""Controller broker: the owner-facing HTTP API and the internal reconcile over the protocol core.

Every state change goes through protocol.py's transition functions and is persisted with a
compare-and-set on the version that was read. Storage, signing, launching, and time are injected
ports, so the broker runs against in-memory adapters in tests. serve.py is the controller's
entrypoint: it composes the production adapters from configuration and refuses to start when
configuration is missing or a production client cannot open (mao_cloud.production). Caller identity
is resolved from each request's Google ID token by
BearerIdentity, which reads the access mode and identity map on every request.
Removing a mapped grant may still allow derived ownership while mayo-wide remains enabled.
"""

import json
import logging
import re
import uuid
from collections.abc import Callable, Mapping, Sequence
from dataclasses import dataclass, field
from datetime import UTC, datetime, timedelta
from typing import Any, Protocol
from uuid import UUID

from fastapi import FastAPI, Request
from fastapi.concurrency import run_in_threadpool
from fastapi.responses import JSONResponse
from google.auth.exceptions import GoogleAuthError
from google.oauth2 import id_token
from pydantic import BaseModel

from mao_cloud.admission import check_queue_room, holds_capacity, select_next_dispatch
from mao_cloud.bearer import (
    CERT_FETCH_TIMEOUT_SECONDS,
    bearer_credential,
    bounded_fetch,
)
from mao_cloud.config import (
    AccessMode,
    Identity,
    Policy,
    Role,
    is_mayo_email,
)
from mao_cloud.protocol import (
    MAX_LOG_LIMIT,
    PROTOCOL_VERSION,
    RUN_ID_PATTERN,
    AcceptedReceipt,
    Admission,
    CleanupFacts,
    ClaimDocument,
    Cursor,
    CreateRunResponse,
    CursorStream,
    ErrorCode,
    FailureCode,
    IdempotencyRecord,
    LogEntry,
    LogPage,
    ObjectKind,
    ObservedInput,
    ProtocolError,
    RunRecord,
    RunResultDocument,
    RunState,
    RunView,
    SeqGap,
    StatusReport,
    StatusView,
    TERMINAL_STATES,
    TeardownStatus,
    ToolCompatibility,
    UploadCapability,
    WorkerBootstrap,
    WorkerIntake,
    accept_claim,
    accept_result,
    accept_status,
    action_digest,
    admission_deadline,
    admit_start,
    artifact_object_name,
    begin_dispatch,
    build_bootstrap,
    build_cancel,
    build_control,
    capability_expiry,
    decide_admission,
    decode_cursor,
    deadline_passed,
    effective_admission_floor,
    encode_cursor,
    failure,
    is_owner_cancelled,
    liveness_decision,
    unknown_launch_alert_due,
    manifest_from_request,
    new_idempotency_record,
    object_name,
    parse_bulk_cancel_request,
parse_cancel_request,
    parse_create_request,
    parse_document,
    parse_retry_request,
    parse_start_request,
    plan_page,
    replay_start,
    request_cancel,
    retry,
    settle_capacity,
    run_view,
    snapshot,
    status_view,
    teardown_status,
    transition,
    upload_deadline,
    verify_artifacts,
    verify_bootstrap,
)
from mao_cloud.signer import CapabilityRequest, SignerClient, SignerRefused
from mao_cloud.sweeper import SweepAction, plan_sweep_page, reconcile_backoff_seconds, retention_due

DEFAULT_LOG_LIMIT = 100
# The Scheduler tick sends no body, so its default must cover the runs admission can hold at once
# (team queue depth plus team worker slots). A larger due set is walked with the returned cursor.
RECONCILE_BATCH_DEFAULT = 50
RECONCILE_BATCH_MAX = 50

_PRE_DISPATCH = frozenset({RunState.AWAITING_UPLOAD, RunState.QUEUED})
# Compare-and-set writes tried for one execution name before the name is reported unrecorded.
_EXECUTION_NAME_WRITES = 3
_STATUS_INGEST = frozenset(
    {
        RunState.STARTING,
        RunState.RUNNING,
        RunState.COLLECTING,
        RunState.RECONCILING,
        RunState.CANCEL_REQUESTED,
    }
)
_RESULT_INGEST = frozenset(
    {RunState.STARTING, RunState.RUNNING, RunState.COLLECTING, RunState.RECONCILING}
)


class AdmissionSwitch(Protocol):
    def is_open(self) -> bool: ...

    def set_open(self, open_: bool) -> None: ...


class Clock(Protocol):
    def now(self) -> datetime: ...


@dataclass(frozen=True)
class RunSnapshot:
    """One consistent view of all capacity records and their shared store revision."""

    records: tuple[RunRecord, ...]
    revision: int


class RunStore(Protocol):
    """Durable run and dedup records. compare_and_set must be atomic."""

    def get_run(self, run_id: str) -> RunRecord | None: ...

    def find_run_by_request(self, owner_id: str, request_uuid: UUID) -> RunRecord | None: ...

    def get_idempotency(self, owner_id: str, request_uuid: UUID) -> IdempotencyRecord | None: ...

    def restore_floor(self) -> datetime | None: ...

    def create_run(self, run: RunRecord, idempotency: IdempotencyRecord) -> None:
        """Insert a run and its dedup record together.

        Raises ProtocolError(VERSION_CONFLICT) if either row already exists. A concurrent
        duplicate create therefore gets a retryable conflict, and its retry replays.
        """

    def compare_and_set(
        self,
        run: RunRecord,
        *,
        expected_version: int,
        idempotency: IdempotencyRecord | None = None,
        expected_revision: int | None = None,
    ) -> None:
        """Replace the run atomically with its dedup record after checking both versions.

        When supplied, expected_revision must match the shared revision returned by
        admission_snapshot, in the same transaction as the write. Every run mutation,
        including create and restore, advances that revision; a per-run CAS alone is insufficient.

        A supplied idempotency record is stored under its own (owner_id, request_uuid). A retry's
        record therefore never replaces the create record of the run it acts on.
        """

    def admission_snapshot(self) -> RunSnapshot:
        """Read all capacity records and their shared revision in one consistent transaction."""

    def list_runs(self) -> Sequence[RunRecord]:
        """List records for inspection; admission decisions use admission_snapshot instead."""


class ObjectStore(Protocol):
    def head(self, name: str) -> ObservedInput | None: ...

    def get(self, name: str) -> bytes | None: ...

    def create_only(self, name: str, data: bytes) -> None:
        """Write name only if it does not exist. Raises ProtocolError(VERSION_CONFLICT) if it does."""


class LaunchAmbiguous(Exception):
    """The launch response was lost, so the worker job may or may not exist. Never relaunch on it."""


class LaunchFailed(RuntimeError):
    """The launcher raised after the dispatch fence was stored. The attempt must not relaunch."""


class Launcher(Protocol):
    def execution_stopped(self, record: RunRecord) -> bool:
        """Authoritatively confirm all executions for this attempt are stopped or absent.

        True also requires that no pending launch can later start an execution. Unknown,
        active, or failed observations return False. Match run, attempt, fence and claim.
        An attempt with no recorded execution_name can never be confirmed, so it returns False.
        """

    def request_stop(self, record: RunRecord) -> None:
        """Ask the platform to stop every execution for this attempt, without waiting.

        Idempotent: the broker repeats it on every reconcile until execution_stopped is True.
        Raising means the request was not accepted, and the next reconcile retries it.
        Match run, attempt, fence and claim, as execution_stopped does.
        """

    def launch(self, bootstrap: WorkerBootstrap, intake_url: str) -> str | None:
        """Start the worker for a dispatched attempt, and return the platform execution name.

        intake_url is the attempt's read-only signed URL for its intake object. It is the only
        per-attempt value the worker receives beyond the run and attempt identifiers, and the
        launcher must never log it. The name is stored on the attempt by compare-and-set, so stop
        and confirmation can address it later. None means the launch succeeded but its answer named
        no execution.

        Raising LaunchAmbiguous moves the run to reconciling and keeps its reservation. Any other
        raise leaves the run dispatching; liveness recovers it, and it is never relaunched.
        """


class SystemClock:
    def now(self) -> datetime:
        return datetime.now(UTC)


def _new_run_id() -> str:
    return f"run_{uuid.uuid4().hex}"


def _new_claim_id() -> str:
    return f"claim_{uuid.uuid4().hex}"


@dataclass(frozen=True)
class Ports:
    """Everything the broker touches outside its own logic.

    owner_of returns the owner_id the request's caller acts as, or None when the caller is not an
    owner. is_controller reports whether the caller may reconcile. is_operator reports whether it
    may drain admissions and bulk-cancel. All answer from the request's credential on every call
    and keep no copy of it.

    admissions is the durable admissions switch. The broker reads it once per admitting request,
    before that request writes anything; it is never cached across requests.
    """

    store: RunStore
    objects: ObjectStore
    signer: SignerClient
    launcher: Launcher
    clock: Clock
    owner_of: Callable[[Request], str | None]
    is_controller: Callable[[Request], bool]
    is_operator: Callable[[Request], bool]
    admissions: AdmissionSwitch
    new_run_id: Callable[[], str] = field(default=_new_run_id)
    new_claim_id: Callable[[], str] = field(default=_new_claim_id)


def _claim_pending(record: RunRecord) -> bool:
    """A claim is accepted once. A late claim may revive reconciling only if none was ever seen."""
    return record.state is RunState.DISPATCHING or (
        record.state is RunState.RECONCILING and record.last_seen_at is None
    )


def _same_launch(record: RunRecord, launched: RunRecord) -> bool:
    """True when record is the same launched attempt: run, attempt, fence and claim all match."""
    return (
        record.run_id == launched.run_id
        and record.attempt == launched.attempt
        and record.fence == launched.fence
        and record.claim_id == launched.claim_id
    )


_RESULT_FAILURES = frozenset({FailureCode.WORKER_FAILED, FailureCode.CHECKSUM_MISMATCH})


def _is_current(
    record: RunRecord, document: ClaimDocument | StatusReport | RunResultDocument
) -> bool:
    """A worker document counts only if it names the current attempt, fence, and claim.

    Anything else is fenced telemetry from a superseded or foreign writer and is ignored. It is
    never raised: a raise during ingest would stop reconcile from applying liveness, so a
    poisoned object could keep a run from ever reaching a deadline.
    """
    return (
        document.attempt == record.attempt
        and document.fence == record.fence
        and record.claim_id is not None
        and document.claim_id == record.claim_id
    )


def _result_accepted(record: RunRecord) -> bool:
    """Only accept_result (WORKER_FAILED) and the checksum gate (CHECKSUM_MISMATCH) record a result."""
    if record.state is RunState.SUCCEEDED:
        return True
    return (
        record.state is RunState.FAILED
        and record.failure is not None
        and record.failure.code in _RESULT_FAILURES
    )


def _parse_limit(
    raw: str | None, *, default: int = DEFAULT_LOG_LIMIT, maximum: int = MAX_LOG_LIMIT
) -> int:
    if raw is None:
        return default
    try:
        value = int(raw)
    except ValueError as exc:
        raise ProtocolError(ErrorCode.INVALID_REQUEST, "limit must be an integer") from exc
    if not 1 <= value <= maximum:
        raise ProtocolError(ErrorCode.INVALID_REQUEST, f"limit must be between 1 and {maximum}")
    return value


def _parse_run_cursor(raw: str | None) -> str | None:
    if raw is None:
        return None
    if re.fullmatch(RUN_ID_PATTERN, raw) is None:
        raise ProtocolError(ErrorCode.INVALID_REQUEST, "after must be a run id")
    return raw


def _parse_log_object(data: bytes, *, seq: int) -> LogEntry:
    try:
        entry = parse_document(LogEntry, json.loads(data))
    except (ValueError, ProtocolError) as exc:
        # Below the durable watermark a malformed log is an integrity fault, not a client error.
        raise ProtocolError(
            ErrorCode.CHECKSUM_MISMATCH, "stored log object is not a valid log entry", seq=seq
        ) from exc
    if entry.seq != seq:
        raise ProtocolError(
            ErrorCode.CHECKSUM_MISMATCH, "stored log object seq does not match its name", seq=seq
        )
    return entry


class Broker:
    """Protocol operations with no HTTP concerns. Each mutation is one CAS commit."""

    def __init__(self, *, policy: Policy, ports: Ports) -> None:
        self._policy = policy
        self._ports = ports

    def _require_admissions_open(self) -> None:
        """Refuse new queue entry while the admissions switch is closed.

        Called after each admitting path's replay check and before it writes, so a replayed
        request still gets its answer. Cancel never calls this: closing the switch cancels nothing.
        """
        if not self._ports.admissions.is_open():
            raise ProtocolError(
                ErrorCode.ADMISSIONS_PAUSED,
                "new admissions are paused; runs already admitted are not affected",
            )

    # -- owner operations ----------------------------------------------------

    def create_run(self, owner: str, payload: Any) -> tuple[CreateRunResponse, Admission]:
        request = parse_create_request(payload)
        store = self._ports.store
        now = self._ports.clock.now()
        existing = store.get_idempotency(owner, request.request_uuid)
        orphan = (
            store.find_run_by_request(owner, request.request_uuid) if existing is None else None
        )
        admission = decide_admission(
            existing=existing,
            orphan_run_id=orphan.run_id if orphan is not None else None,
            request_uuid=request.request_uuid,
            digest=request.canonical_digest,
            now=now,
            admission_floor=effective_admission_floor(
                now=now, restore_floor=store.restore_floor()
            ),
            skew_seconds=self._policy.skew_seconds,
        )
        if admission is Admission.REPLAY:
            assert existing is not None
            run = self._must_get(existing.run_id)
            return self._create_response(run, now), admission

        self._require_admissions_open()
        manifest = manifest_from_request(request)
        compatibility = self._policy.admit_manifest(manifest)
        record = RunRecord(
            run_id=self._ports.new_run_id(),
            owner_id=owner,
            server_id=self._policy.server_id,
            request_uuid=request.request_uuid,
            manifest=manifest,
            manifest_digest=request.canonical_digest,
            image_digest=compatibility.image_digest,
            state=RunState.AWAITING_UPLOAD,
            version=0,
            attempt=1,
            fence=1,
            max_attempts=self._policy.max_attempts,
            admitted_at=now,
            state_entered_at=now,
            deadline_at=admission_deadline(
                admitted_at=now, max_runtime_seconds=manifest.budget.max_runtime_seconds
            ),
        )
        dedup = new_idempotency_record(
            owner_id=owner,
            request_uuid=request.request_uuid,
            digest=request.canonical_digest,
            run_id=record.run_id,
            now=now,
        )
        store.create_run(record, dedup)
        return self._create_response(record, now), admission

    def start_run(self, owner: str, run_id: str, payload: Any) -> AcceptedReceipt:
        request = parse_start_request(payload)
        record = self._owned(owner, run_id)
        if record.receipt is not None:
            return replay_start(record, request)
        self._require_admissions_open()
        now = self._ports.clock.now()
        compatibility = self._compatibility_for(record)
        observed = self._ports.objects.head(object_name(ObjectKind.INPUT, run_id=run_id))
        queued = admit_start(
            record, request, observed=observed, compatibility=compatibility, now=now
        )
        capacity = self._ports.store.admission_snapshot()
        check_queue_room(capacity.records, owner_id=owner, limits=self._policy.admission)
        self._commit(record, queued, expected_revision=capacity.revision)
        assert queued.receipt is not None
        return queued.receipt

    def cancel_run(self, owner: str, run_id: str, payload: Any) -> RunView:
        parse_cancel_request(payload)
        record = self._owned(owner, run_id)
        now = self._ports.clock.now()
        return self._view(self._cancel(record, now), now)

    def _cancel(self, record: RunRecord, now: datetime) -> RunRecord:
        updated, _changed = request_cancel(record, now=now)
        return self._settle_execution(self._commit(record, updated), now)

    # -- operator operations -------------------------------------------------

    def admissions_status(self) -> dict[str, Any]:
        return self._admissions_view()

    def set_admissions_open(self, *, open_: bool) -> dict[str, Any]:
        self._ports.admissions.set_open(open_)
        return self._admissions_view()

    def bulk_cancel(self, payload: Any) -> dict[str, Any]:
        request = parse_bulk_cancel_request(payload)
        now = self._ports.clock.now()
        return {
            "protocol_version": "v1",
            "outcomes": [self._bulk_cancel_one(run_id, now) for run_id in request.run_ids],
        }

    def _bulk_cancel_one(self, run_id: str, now: datetime) -> dict[str, Any]:
        """One selected run under the owner cancel rule. A refusal is reported, never raised."""
        try:
            record = self._must_get(run_id)
        except ProtocolError:
            return {"run_id": run_id, "outcome": "not_found", "state": None, "holding_capacity": False}
        was_terminal = record.state in TERMINAL_STATES
        try:
            current = self._cancel(record, now)
        except ProtocolError as exc:
            return {
                "run_id": run_id,
                "outcome": "refused",
                "code": exc.code.value,
                "state": None,
                "holding_capacity": None,
            }
        if was_terminal:
            outcome = "already_terminal"
        elif current.state is RunState.CANCELLED:
            outcome = "cancelled"
        else:
            outcome = "cancel_requested"
        return {
            "run_id": run_id,
            "outcome": outcome,
            "state": current.state.value,
            "holding_capacity": holds_capacity(current),
        }

    def _admissions_view(self) -> dict[str, Any]:
        holding = sum(
            1 for record in self._ports.store.admission_snapshot().records if holds_capacity(record)
        )
        return {
            "state": "open" if self._ports.admissions.is_open() else "drained",
            "policy_version": self._policy.admission.version,
            "holding_runs": holding,
        }

    def retry_run(self, owner: str, run_id: str, payload: Any) -> RunView:
        request = parse_retry_request(payload)
        record = self._owned(owner, run_id)
        store = self._ports.store
        now = self._ports.clock.now()
        digest = action_digest(
            run_id=run_id,
            action="retry",
            body={
                "expected_attempt": request.expected_attempt,
                "expected_version": request.expected_version,
            },
        )
        existing = store.get_idempotency(owner, request.request_uuid)
        already_applied = request.request_uuid in record.retry_request_uuids
        admission = decide_admission(
            existing=existing,
            orphan_run_id=run_id if existing is None and already_applied else None,
            request_uuid=request.request_uuid,
            digest=digest,
            now=now,
            admission_floor=effective_admission_floor(
                now=now, restore_floor=store.restore_floor()
            ),
            skew_seconds=self._policy.skew_seconds,
        )
        if admission is Admission.REPLAY:
            return self._view(record, now)
        self._require_admissions_open()
        updated = retry(
            record,
            request_uuid=request.request_uuid,
            expected_version=request.expected_version,
            expected_attempt=request.expected_attempt,
            now=now,
        )
        capacity = store.admission_snapshot()
        check_queue_room(capacity.records, owner_id=owner, limits=self._policy.admission)
        dedup = new_idempotency_record(
            owner_id=owner,
            request_uuid=request.request_uuid,
            digest=digest,
            run_id=run_id,
            now=now,
        )
        store.compare_and_set(
            updated, expected_version=record.version, idempotency=dedup,
            expected_revision=capacity.revision,
        )
        return self._view(updated, now)

    def get_run(self, owner: str, run_id: str) -> RunView:
        record = self._owned(owner, run_id)
        return self._view(record, self._ports.clock.now())

    def get_status(self, owner: str, run_id: str) -> StatusView:
        record = self._owned(owner, run_id)
        now = self._ports.clock.now()
        return status_view(
            record,
            now=now,
            stale_after_seconds=self._policy.stale_after_seconds,
            skew_seconds=self._policy.skew_seconds,
            result_available=_result_accepted(record),
        )

    def get_teardown(self, owner: str, run_id: str) -> TeardownStatus:
        """Read-only cleanup outcome for one of the owner's runs. Nothing is deleted here."""
        record = self._owned(owner, run_id)
        return teardown_status(record, self._cleanup_facts(record, self._ports.clock.now()))

    def _cleanup_facts(self, record: RunRecord, now: datetime) -> CleanupFacts:
        """Teardown facts, each from the source that owns it, or a disclosed default where none exists.

        store_reachable: the record was just read from the run store, so the store answered.
        retention_exception: no retention-exception model exists, so none can be in force.
        execution_terminal: terminal state with settled capacity, which requires confirmed termination.
        capability_expiry_passed: every capability is clamped to the run deadline (capability_expiry).
        records_past_retention: the approved per-class durations in the policy (G01). Unset durations
            mean nothing is due.
        deletion_started: no deletion path exists in this service; sweeper planning deletes nothing.
        data_present: reported true. ObjectStore has no listing and checkpoint ids are not recorded
            durably, so absence cannot be proven. Overstating presence keeps data in the pending set
            rather than claiming it is gone.
        """
        return CleanupFacts(
            store_reachable=True,
            retention_exception=False,
            execution_terminal=record.state in TERMINAL_STATES and record.reserved_seconds == 0,
            capability_expiry_passed=deadline_passed(now, record.deadline_at),
            data_present=True,
            deletion_started=False,
            records_past_retention=bool(
                retention_due(record, now=now, retention=self._policy.retention)
            ),
        )

    def get_logs(
        self,
        owner: str,
        run_id: str,
        *,
        cursor: str | None,
        limit: str | None,
    ) -> LogPage:
        record = self._owned(owner, run_id)
        now = self._ports.clock.now()
        page_limit = _parse_limit(limit)
        if cursor is None:
            requested = 1
        else:
            decoded = decode_cursor(cursor, run_id=run_id, stream=CursorStream.LOGS)
            if decoded.attempt != record.attempt:
                raise ProtocolError(
                    ErrorCode.CURSOR_INVALID,
                    "cursor belongs to a previous attempt; logs restart with each attempt",
                    attempt=decoded.attempt,
                    current_attempt=record.attempt,
                )
            requested = decoded.seq
        plan = plan_page(
            requested_seq=requested,
            retained_from=record.log_retained_from,
            watermark=record.last_log_watermark,
            limit=page_limit,
        )
        entries = tuple(
            _parse_log_object(
                self._require_object(
                    object_name(ObjectKind.LOG, run_id=run_id, attempt=record.attempt, seq=seq),
                    seq=seq,
                ),
                seq=seq,
            )
            for seq in range(plan.first_seq, plan.last_seq + 1)
        )
        return LogPage(
            run_id=run_id,
            attempt=record.attempt,
            entries=entries,
            next_cursor=encode_cursor(
                Cursor(
                    run_id=run_id,
                    attempt=record.attempt,
                    stream=CursorStream.LOGS,
                    seq=plan.next_seq,
                )
            ),
            watermark=record.last_log_watermark,
            retained_from=record.log_retained_from,
            trimmed=plan.trimmed,
            gap=SeqGap(from_seq=plan.gap[0], to_seq=plan.gap[1]) if plan.gap else None,
            snapshot=snapshot(record, now=now),
        )

    def get_result(self, owner: str, run_id: str) -> RunResultDocument:
        record = self._owned(owner, run_id)
        if record.state not in TERMINAL_STATES:
            raise ProtocolError(
                ErrorCode.RESULT_NOT_READY,
                "the attempt has not produced a result",
                state=record.state.value,
            )
        # A worker result written after liveness already ended the attempt is fenced, so it
        # is never served. Only an accepted result (see accept_result) counts.
        if not _result_accepted(record):
            raise ProtocolError(
                ErrorCode.NOT_FOUND,
                "no worker result was accepted for this attempt",
                state=record.state.value,
            )
        document = self._read_document(
            object_name(ObjectKind.RESULT, run_id=run_id, attempt=record.attempt),
            RunResultDocument,
        )
        if document is None:
            raise ProtocolError(ErrorCode.NOT_FOUND, "accepted result object is missing or unreadable")
        return document

    # -- internal operations -------------------------------------------------

    def reconcile(self, run_id: str) -> RunView:
        """Advance one run from the durable documents and the clock. Safe to repeat."""
        record = self._must_get(run_id)
        now = self._ports.clock.now()
        if record.state in _PRE_DISPATCH:
            current = self._apply_liveness(record, now)
            if current.state is RunState.QUEUED:
                current = self._dispatch_if_current(current, now)
            return self._view(current, now)
        current = self._ingest(record, now)
        current = self._fail_missing_result_if_stopped(current, now)
        current = self._apply_liveness(current, now)
        current = self._raise_unknown_launch_alert(current, now)
        current = self._settle_execution(current, now)
        return self._view(current, now)

    def reconcile_batch(self, *, limit: int, after: str | None) -> dict[str, Any]:
        """Plan the due runs after the cursor, then reconcile each planned run. Synchronous.

        Each run goes through reconcile, which re-reads it and commits by compare-and-set, so a run
        that changed since the listing is judged on its current record. A conflict or refusal is
        reported against that run and never stops the rest of the batch. Nothing outlives the call.
        """
        page = plan_sweep_page(
            self._ports.store.list_runs(),
            now=self._ports.clock.now(),
            policy=self._policy,
            limit=limit,
            after=after,
        )
        return {
            "protocol_version": PROTOCOL_VERSION,
            "reconciled": [self._reconcile_planned(action) for action in page.actions],
            "next_after": page.next_after,
        }

    # -- internals -----------------------------------------------------------

    def _reconcile_planned(self, action: SweepAction) -> dict[str, Any]:
        entry: dict[str, Any] = {"run_id": action.run_id, "reason": action.reason}
        try:
            view = self.reconcile(action.run_id)
        except LaunchFailed:
            return entry | {"outcome": "launch_failed", "state": RunState.DISPATCHING.value}
        except ProtocolError as exc:
            if exc.code is ErrorCode.VERSION_CONFLICT:
                outcome = "conflict"
            elif exc.code is ErrorCode.NOT_FOUND:
                outcome = "not_found"
            else:
                outcome = "refused"
            return entry | {"outcome": outcome, "code": exc.code.value}
        return entry | {"outcome": "reconciled", "state": view.state.value}

    def _must_get(self, run_id: str) -> RunRecord:
        record = (
            self._ports.store.get_run(run_id) if re.fullmatch(RUN_ID_PATTERN, run_id) else None
        )
        if record is None:
            raise ProtocolError(ErrorCode.NOT_FOUND, "run not found")
        return record

    def _owned(self, owner: str, run_id: str) -> RunRecord:
        record = self._must_get(run_id)
        if record.owner_id != owner:
            # Same refusal as a missing run, so one owner cannot probe another's run ids.
            raise ProtocolError(ErrorCode.NOT_FOUND, "run not found")
        return record

    def _view(self, record: RunRecord, now: datetime) -> RunView:
        return run_view(
            record,
            now=now,
            stale_after_seconds=self._policy.stale_after_seconds,
            skew_seconds=self._policy.skew_seconds,
        )

    def _create_response(self, record: RunRecord, now: datetime) -> CreateRunResponse:
        upload = None
        if record.state is RunState.AWAITING_UPLOAD:
            upload = self._upload_capability(record, now)
        return CreateRunResponse(run=self._view(record, now), upload=upload)

    def _upload_capability(self, record: RunRecord, now: datetime) -> UploadCapability:
        expires_at = capability_expiry(
            ObjectKind.INPUT,
            "PUT",
            now=now,
            attempt_deadline=upload_deadline(
                admitted_at=record.admitted_at, deadline_at=record.deadline_at
            ),
        )
        request = CapabilityRequest(
            owner_id=record.owner_id,
            run_id=record.run_id,
            kind=ObjectKind.INPUT,
            method="PUT",
            object_name=object_name(ObjectKind.INPUT, run_id=record.run_id),
            expires_at=expires_at,
            sha256_hex=record.manifest.source_upload.sha256,
        )
        try:
            issued = self._ports.signer.issue(request)
        except SignerRefused:
            raise ProtocolError(
                ErrorCode.NOT_ALLOWED, "the upload capability was not issued"
            ) from None
        return UploadCapability(
            object_name=issued.object_name,
            method="PUT",
            url=issued.url,
            headers=dict(issued.headers),
            expires_at=issued.expires_at,
        )

    def _compatibility_for(self, record: RunRecord) -> ToolCompatibility:
        compatibility = self._policy.admit_manifest(record.manifest)
        if compatibility.image_digest != record.image_digest:
            raise ProtocolError(
                ErrorCode.NOT_ALLOWED,
                "toolchain profile no longer matches the image admitted for this run",
                profile=record.manifest.toolchain_profile,
            )
        return compatibility

    def _commit(
        self, previous: RunRecord, updated: RunRecord, *, expected_revision: int | None = None
    ) -> RunRecord:
        if updated == previous:
            return previous
        self._ports.store.compare_and_set(
            updated, expected_version=previous.version, expected_revision=expected_revision
        )
        return updated

    def _settle_execution(self, record: RunRecord, now: datetime) -> RunRecord:
        """Finish an owner cancel and release capacity only once termination is confirmed.

        An open cancel and a terminal attempt that still holds capacity both wait on the launcher's
        confirmation, never on the clock. Until it arrives, every call repeats the idempotent stop
        request, so an uncertain execution keeps being asked to stop while it stays fenced.
        """
        open_cancel = record.state is RunState.CANCEL_REQUESTED
        unsettled_terminal = record.state in TERMINAL_STATES and record.reserved_seconds > 0
        if not (open_cancel or unsettled_terminal):
            return record
        marker_written = not open_cancel or self._ensure_cancel_marker(record)
        if not self._execution_stopped(record):
            self._request_stop(record)
            return self._schedule_reconcile(record, now)
        if not marker_written:
            # Keep the durable cancellation open so sweeps retry marker publication.
            return record
        current = record
        if open_cancel and is_owner_cancelled(record):
            current = self._commit(
                record,
                transition(
                    record,
                    to=RunState.CANCELLED,
                    expected_version=record.version,
                    expected_attempt=record.attempt,
                    now=now,
                ),
            )
        if current.state in TERMINAL_STATES and current.reserved_seconds > 0:
            return self._commit(current, settle_capacity(current, now=now))
        return current

    def _schedule_reconcile(self, record: RunRecord, now: datetime) -> RunRecord:
        """Record when the planner may next hint this unsettled run, after a stop that was not confirmed.

        Explicit reconcile calls still request the stop every time. Only the planner waits, so a sweep
        does not re-stop every unsettled run on every tick.
        """
        attempts = record.unsettled_reconciles + 1
        delay = reconcile_backoff_seconds(attempts, record.run_id)
        return self._commit(
            record,
            record.model_copy(
                update={
                    "version": record.version + 1,
                    "unsettled_reconciles": attempts,
                    "next_reconcile_at": now + timedelta(seconds=delay),
                }
            ),
        )

    def _ensure_cancel_marker(self, record: RunRecord) -> bool:
        """Write the attempt's cancel directive once. Create-only, so a retry never rewrites it."""
        name = object_name(ObjectKind.CANCEL, run_id=record.run_id, attempt=record.attempt)
        try:
            self._ports.objects.create_only(name, build_cancel(record).model_dump_json().encode())
        except Exception as exc:
            if isinstance(exc, ProtocolError) and exc.code is ErrorCode.VERSION_CONFLICT:
                # Already written. The marker is monotonic, so there is nothing to repeat.
                return True
            # Not written, so the cancel stays open and the next reconcile writes it again.
            _AUDIT.warning(
                "cancel marker not written for run %s attempt %s (%s); retrying on reconcile",
                record.run_id,
                record.attempt,
                type(exc).__name__,
            )
            return False
        return True

    def _execution_stopped(self, record: RunRecord) -> bool:
        try:
            return self._ports.launcher.execution_stopped(record)
        except Exception:
            # A failed observation is uncertainty, never permission to admit another worker.
            return False

    def _request_stop(self, record: RunRecord) -> None:
        try:
            self._ports.launcher.request_stop(record)
        except Exception as exc:
            # Not accepted, so the cancel stays open and the next reconcile repeats the request.
            _AUDIT.warning(
                "platform stop not accepted for run %s attempt %s (%s); retrying on reconcile",
                record.run_id,
                record.attempt,
                type(exc).__name__,
            )

    def _dispatch_if_current(self, record: RunRecord, now: datetime) -> RunRecord:
        try:
            return self._dispatch(record, now)
        except ProtocolError as error:
            if error.code != ErrorCode.VERSION_CONFLICT:
                raise
            # Another reconcile dispatched this run after it was read. The stored record is the
            # truth, and this call never launches the attempt a second time.
            return self._must_get(record.run_id)

    def _dispatch(self, record: RunRecord, now: datetime) -> RunRecord:
        capacity = self._ports.store.admission_snapshot()
        chosen = select_next_dispatch(capacity.records, now=now, limits=self._policy.admission)
        if chosen is None or chosen.run_id != record.run_id:
            return self._must_get(record.run_id)
        dispatching = begin_dispatch(
            record,
            claim_id=self._ports.new_claim_id(),
            dispatch_order=capacity.revision + 1,
            expected_version=record.version,
            expected_attempt=record.attempt,
            now=now,
        )
        bootstrap = build_bootstrap(dispatching)
        # Refuse before any state change: the bootstrap must match the accepted receipt.
        assert dispatching.receipt is not None
        verify_bootstrap(bootstrap, dispatching.receipt)
        # Persisted before launch, so a failed launch leaves a dispatching record that
        # liveness recovers. It never relaunches, which keeps the attempt single-launch.
        self._commit(record, dispatching, expected_revision=capacity.revision)
        # The worker's identity exists before the job can start. Create-only: a second write for
        # the same attempt is refused, never an overwrite.
        self._ports.objects.create_only(
            object_name(ObjectKind.CONTROL, run_id=record.run_id, attempt=record.attempt),
            build_control(dispatching).model_dump_json().encode(),
        )
        # The intake holds the two documents the worker verifies. Written and capability-minted
        # before jobs.run, so the job can only start with its intake readable.
        intake_url = self._write_intake(dispatching, bootstrap, now=now)
        try:
            execution_name = self._ports.launcher.launch(bootstrap, intake_url)
        except LaunchAmbiguous:
            # The job may already exist. Keep the reservation, move to reconciling, and let
            # authoritative execution observation release capacity. The launch is never replayed.
            reconciling = transition(
                dispatching,
                to=RunState.RECONCILING,
                expected_version=dispatching.version,
                expected_attempt=dispatching.attempt,
                now=now,
            )
            return self._commit(dispatching, reconciling)
        except Exception as exc:
            raise LaunchFailed(str(exc)) from exc
        return self._record_execution(dispatching, execution_name)

    def _write_intake(
        self, dispatching: RunRecord, bootstrap: WorkerBootstrap, *, now: datetime
    ) -> str:
        """Write the attempt's intake object, create-only, and return its read-only signed URL.

        The URL is a credential. It is returned to the launcher and never logged or put in an
        error message. A refused capability raises LaunchFailed before any launch, so the attempt
        keeps its reservation and is never relaunched, as for a failed control write.
        """
        receipt = dispatching.receipt
        assert receipt is not None
        name = object_name(
            ObjectKind.INTAKE, run_id=dispatching.run_id, attempt=dispatching.attempt
        )
        self._ports.objects.create_only(
            name,
            WorkerIntake(bootstrap=bootstrap, receipt=receipt).model_dump_json().encode(),
        )
        request = CapabilityRequest(
            owner_id=dispatching.owner_id,
            run_id=dispatching.run_id,
            kind=ObjectKind.INTAKE,
            method="GET",
            object_name=name,
            expires_at=capability_expiry(
                ObjectKind.INTAKE,
                "GET",
                now=now,
                attempt_deadline=dispatching.deadline_at,
            ),
            attempt=dispatching.attempt,
            fence=dispatching.fence,
            claim_id=dispatching.claim_id,
        )
        try:
            issued = self._ports.signer.issue(request)
        except SignerRefused:
            raise LaunchFailed(
                "the worker intake capability was not issued; jobs.run was not called"
            ) from None
        return issued.url

    def _record_execution(self, dispatched: RunRecord, execution_name: str | None) -> RunRecord:
        """Store the platform execution name of a launched attempt, by compare-and-set.

        A conflicting write re-reads the stored record. The name is written only while that record
        is still this attempt, claim and fence, so a concurrent cancel or status write is kept as
        stored. If the name cannot be stored, the attempt keeps its reservation with no handle,
        which is the unresolved state: it is logged, never retried by relaunching.
        """
        if execution_name is None:
            _AUDIT.error(
                "launch answer named no execution for run %s attempt %s; "
                "its capacity cannot be confirmed released",
                dispatched.run_id,
                dispatched.attempt,
            )
            return dispatched
        current = dispatched
        for _ in range(_EXECUTION_NAME_WRITES):
            if current.execution_name == execution_name:
                return current
            if current.execution_name is not None or not _same_launch(current, dispatched):
                _AUDIT.error(
                    "stored record for run %s attempt %s does not match the launched attempt; "
                    "execution name not recorded",
                    dispatched.run_id,
                    dispatched.attempt,
                )
                return current
            try:
                return self._commit(
                    current,
                    current.model_copy(
                        update={"version": current.version + 1, "execution_name": execution_name}
                    ),
                )
            except ProtocolError as error:
                if error.code is not ErrorCode.VERSION_CONFLICT:
                    raise
                current = self._must_get(dispatched.run_id)
        _AUDIT.error(
            "execution name not recorded for run %s attempt %s after concurrent writes; "
            "its capacity cannot be confirmed released",
            dispatched.run_id,
            dispatched.attempt,
        )
        return current

    def _ingest(self, record: RunRecord, now: datetime) -> RunRecord:
        current = record
        attempt = current.attempt
        run_id = current.run_id

        if _claim_pending(current):
            claim = self._read_document(
                object_name(ObjectKind.CLAIM, run_id=run_id, attempt=attempt), ClaimDocument
            )
            if claim is not None and _is_current(current, claim):
                current = self._commit(current, accept_claim(current, claim, now=now))

        if current.state in _STATUS_INGEST:
            report = self._read_document(
                object_name(ObjectKind.STATUS, run_id=run_id, attempt=attempt), StatusReport
            )
            if report is not None and _is_current(current, report):
                current = self._accept_status_report(current, report, now)

        if current.state in _RESULT_INGEST and not deadline_passed(now, current.deadline_at):
            document = self._read_document(
                object_name(ObjectKind.RESULT, run_id=run_id, attempt=attempt),
                RunResultDocument,
            )
            # Fence before checksum: a result from another claim must not fail the run.
            if document is not None and _is_current(current, document):
                try:
                    verify_artifacts(document, read=self._artifact_reader(current))
                except ProtocolError as exc:
                    if exc.code is not ErrorCode.CHECKSUM_MISMATCH:
                        raise
                    return self._commit(current, self._fail_checksum(current, exc.message, now))
                current = self._commit(current, accept_result(current, document, now=now))
        return current

    def _accept_status_report(self, record: RunRecord, report: StatusReport, now: datetime) -> RunRecord:
        try:
            updated, _verdict = accept_status(
                record, report, now=now, skew_seconds=self._policy.skew_seconds
            )
        except ProtocolError as exc:
            if exc.code is ErrorCode.SEQUENCE_REGRESSED:
                # Stale telemetry: liveness keeps the last accepted sequence.
                return record
            raise
        return self._commit(record, updated)

    def _artifact_reader(self, record: RunRecord) -> Callable[[str], bytes | None]:
        def read(name: str) -> bytes | None:
            return self._ports.objects.get(
                artifact_object_name(record.run_id, record.attempt, name)
            )

        return read

    def _fail_missing_result_if_stopped(self, record: RunRecord, now: datetime) -> RunRecord:
        """A collecting attempt whose execution ended with no valid result object fails.

        A result may arrive between initial ingest and execution-stop confirmation, so ingest
        again after that confirmation before declaring it missing. A deadline that has passed is left to liveness, which owns deadline_exceeded. A failed
        observation of the execution is uncertainty and never a failure verdict by itself.
        """
        if record.state is not RunState.COLLECTING or deadline_passed(now, record.deadline_at):
            return record
        try:
            stopped = self._ports.launcher.execution_stopped(record)
        except Exception:
            return record
        if not stopped:
            return record
        current = self._ingest(record, now)
        if current.state is not RunState.COLLECTING:
            return current
        return self._commit(
            current,
            transition(
                current,
                to=RunState.FAILED,
                expected_version=current.version,
                expected_attempt=current.attempt,
                now=now,
                failure=failure(
                    FailureCode.MISSING_RESULT,
                    "the execution ended without a valid result for this attempt",
                ),
            ),
        )

    def _fail_checksum(self, record: RunRecord, message: str, now: datetime) -> RunRecord:
        """Checksum failures are terminal and non-retryable, and must pass through collecting."""
        collecting = (
            record
            if record.state is RunState.COLLECTING
            else transition(
                record,
                to=RunState.COLLECTING,
                expected_version=record.version,
                expected_attempt=record.attempt,
                now=now,
            )
        )
        return transition(
            collecting,
            to=RunState.FAILED,
            expected_version=collecting.version,
            expected_attempt=collecting.attempt,
            now=now,
            failure=failure(FailureCode.CHECKSUM_MISMATCH, message),
        )

    def _apply_liveness(self, record: RunRecord, now: datetime) -> RunRecord:
        decision = liveness_decision(
            record,
            now=now,
            startup_timeout_seconds=self._policy.startup_timeout_seconds,
            stale_after_seconds=self._policy.stale_after_seconds,
            skew_seconds=self._policy.skew_seconds,
            queued_deadline_seconds=self._policy.admission.queued_deadline_seconds,
        )
        if decision is None:
            return record
        updated = transition(
            record,
            to=decision.to,
            expected_version=record.version,
            expected_attempt=record.attempt,
            now=now,
            failure=decision.failure,
        )
        return self._commit(record, updated)

    def _raise_unknown_launch_alert(self, record: RunRecord, now: datetime) -> RunRecord:
        """Record the ten-minute unknown-launch alert once per attempt. Capacity is never touched."""
        if not unknown_launch_alert_due(record, now=now):
            return record
        updated = self._commit(
            record,
            record.model_copy(
                update={"version": record.version + 1, "unknown_launch_alerted_at": now}
            ),
        )
        _AUDIT.error(
            "unknown launch alert run=%s attempt=%s: no report ten minutes after dispatch; "
            "capacity stays reserved until the launcher confirms absence or termination",
            record.run_id,
            record.attempt,
        )
        return updated

    def _read_document[M: BaseModel](self, name: str, model: type[M]) -> M | None:
        """Read a worker-written document. Malformed content reads as absent, never raises.

        Ingest runs before liveness on every reconcile, so raising here would wedge the run.
        """
        data = self._ports.objects.get(name)
        if data is None:
            return None
        try:
            return parse_document(model, json.loads(data))
        except (ValueError, ProtocolError):
            return None

    def _require_object(self, name: str, *, seq: int) -> bytes:
        data = self._ports.objects.get(name)
        if data is None:
            raise ProtocolError(
                ErrorCode.NOT_FOUND,
                "log object is missing below the durable watermark",
                seq=seq,
            )
        return data


# -- caller identity ----------------------------------------------------------

_AUDIT = logging.getLogger("mao_cloud.audit")


@dataclass(frozen=True)
class IdentitySnapshot:
    """One policy's identity map and access mode, read together so they can never disagree."""

    by_subject: Mapping[str, Identity]
    access_mode: AccessMode

    def identity_for(self, subject: str) -> Identity | None:
        return self.by_subject.get(subject)


class IdentityDirectory(Protocol):
    """The identity map and access mode, read once per request as one snapshot."""

    def snapshot(self) -> IdentitySnapshot: ...


class PolicyIdentities:
    """An IdentityDirectory over the identities and access mode the policy file declares.

    replace() is the refresh path. It builds the whole snapshot and swaps it in with one assignment,
    so a request reads either the old policy's identities and mode or the new one's, never a mix.
    """

    def __init__(self, policy: Policy) -> None:
        self._snapshot = _snapshot_of(policy)

    def replace(self, policy: Policy) -> None:
        self._snapshot = _snapshot_of(policy)

    def snapshot(self) -> IdentitySnapshot:
        return self._snapshot

    def identity_for(self, subject: str) -> Identity | None:
        return self._snapshot.identity_for(subject)


def _snapshot_of(policy: Policy) -> IdentitySnapshot:
    by_subject = {identity.subject: identity for identity in policy.identities}
    return IdentitySnapshot(by_subject=by_subject, access_mode=policy.access_mode)


@dataclass(frozen=True)
class _Caller:
    """What an admitted token grants. owner_id is set only for an owner."""

    role: Role
    owner_id: str | None


class BearerIdentity:
    """Resolves a request's caller from its Google ID token, through the identity snapshot.

    The credential is the bearer token in the Authorization header. Cloud Run's invoker check reads
    X-Serverless-Authorization, which carries the same token, but that header alone never identifies
    a caller here. google-auth verifies the signature against Google's published certificates, the
    issuer, the audience, and the expiry. Only a verified token can reach the rules below.

    A verified subject that the identity map holds must match its provisioned identity: the token's
    email must equal that identity's email and be verified. A mapped subject never falls through to
    any other rule. A verified subject the map does not hold is admitted only in mayo-wide access
    mode, and only when its email is verified and exactly on the Mayo domain. Such a caller is an
    owner keyed by its own numeric subject, so it can never be an operator or a scheduler. Every other
    outcome resolves to no caller.

    Each request reads the directory once, so the access mode and the identity map always come from
    the same policy. Resolution blocks on the certificate fetch, so the HTTP layer runs it off the
    event loop.
    """

    def __init__(
        self, *, audience: str, directory: IdentityDirectory, transport: Callable[..., Any]
    ) -> None:
        self._audience = audience
        self._directory = directory
        self._transport = bounded_fetch(transport, CERT_FETCH_TIMEOUT_SECONDS)

    def owner_of(self, request: Request) -> str | None:
        caller = self._caller(request)
        if caller is None or caller.role is not Role.OWNER:
            return None
        return caller.owner_id

    def is_controller(self, request: Request) -> bool:
        caller = self._caller(request)
        return caller is not None and caller.role in (Role.OPERATOR, Role.SCHEDULER)

    def is_operator(self, request: Request) -> bool:
        caller = self._caller(request)
        return caller is not None and caller.role is Role.OPERATOR

    def _caller(self, request: Request) -> _Caller | None:
        token = bearer_credential(request)
        if token is None:
            _audit("refused", reason="no_credential")
            return None
        try:
            claims = id_token.verify_oauth2_token(token, self._transport, audience=self._audience)
        except (ValueError, GoogleAuthError) as exc:
            # Only the exception type is logged: its message can echo token content.
            _audit("refused", reason="token_invalid", error=type(exc).__name__)
            return None
        except Exception as exc:  # the certificate transport failed; refuse rather than error
            _audit("refused", reason="verifier_unavailable", error=type(exc).__name__)
            return None
        subject = claims.get("sub")
        snapshot = self._directory.snapshot()
        identity = snapshot.identity_for(subject) if isinstance(subject, str) else None
        if identity is not None:
            return _mapped_caller(identity, claims, subject)
        if isinstance(subject, str) and snapshot.access_mode is AccessMode.MAYO_WIDE:
            return _mayo_owner(subject, claims)
        _audit("refused", reason="unknown_subject", subject=subject)
        return None


def _mapped_caller(identity: Identity, claims: Mapping[str, Any], subject: str) -> _Caller | None:
    """A provisioned subject: its token email must equal the identity's email and be verified."""
    if claims.get("email") != identity.email:
        _audit("refused", reason="email_mismatch", subject=subject)
        return None
    if re.fullmatch(r"[0-9]{1,30}", subject) is None:
        _audit("refused", reason="invalid_subject")
        return None
    if claims.get("email_verified") is not True:
        _audit("refused", reason="email_unverified", subject=subject)
        return None
    _audit("accepted", subject=subject, role=identity.role.value, owner=identity.owner_id)
    return _Caller(role=identity.role, owner_id=identity.owner_id)


def _mayo_owner(subject: str, claims: Mapping[str, Any]) -> _Caller | None:
    """An unmapped verified subject, admitted as its own owner when its email is on the Mayo domain.

    The owner is the numeric subject itself. That never collides with a fixed owner id, because
    owner ids start with a letter and subjects are digits only.
    """
    if re.fullmatch(r"[0-9]{1,30}", subject) is None:
        _audit("refused", reason="invalid_subject")
        return None
    if claims.get("email_verified") is not True:
        _audit("refused", reason="email_unverified", subject=subject)
        return None
    email = claims.get("email")
    if not isinstance(email, str) or not is_mayo_email(email):
        _audit("refused", reason="not_mayo_email", subject=subject)
        return None
    _audit("accepted", subject=subject, role=Role.OWNER.value, owner=subject)
    return _Caller(role=Role.OWNER, owner_id=subject)


def _audit(outcome: str, **fields: object) -> None:
    """Record a caller decision by category and numeric subject. Never a token, email, or claim."""
    detail = " ".join(f"{key}={value}" for key, value in fields.items() if value is not None)
    _AUDIT.info("caller %s %s", outcome, detail)


# -- HTTP layer -------------------------------------------------------------


async def _owner(ports: Ports, request: Request) -> str:
    owner = await run_in_threadpool(ports.owner_of, request)
    if not owner:
        raise ProtocolError(ErrorCode.NOT_ALLOWED, "caller identity is not authorized for this API")
    return owner


async def _operator(ports: Ports, request: Request) -> None:
    if not await run_in_threadpool(ports.is_operator, request):
        raise ProtocolError(ErrorCode.NOT_ALLOWED, "internal route requires the operator identity")


async def _json_body(request: Request) -> Any:
    raw = await request.body()
    try:
        return json.loads(raw)
    except ValueError as exc:
        raise ProtocolError(ErrorCode.INVALID_REQUEST, "request body is not valid JSON") from exc


def _ok(model: BaseModel, status_code: int = 200) -> JSONResponse:
    return JSONResponse(status_code=status_code, content=model.model_dump(mode="json"))


def create_app(*, policy: Policy, ports: Ports) -> FastAPI:
    broker = Broker(policy=policy, ports=ports)
    app = FastAPI(
        title="mao-cloud broker",
        version=PROTOCOL_VERSION,
        docs_url=None,
        redoc_url=None,
        openapi_url=None,
    )

    @app.exception_handler(ProtocolError)
    async def _refuse(_request: Request, exc: ProtocolError) -> JSONResponse:
        return JSONResponse(
            status_code=exc.http_status,
            content=exc.body().model_dump(mode="json"),
        )

    @app.post("/v1/runs")
    async def create_run(request: Request) -> JSONResponse:
        response, admission = broker.create_run(await _owner(ports, request), await _json_body(request))
        return _ok(response, 201 if admission is Admission.CREATE else 200)

    @app.post("/v1/runs/{run_id}:start")
    async def start_run(run_id: str, request: Request) -> JSONResponse:
        receipt = broker.start_run(await _owner(ports, request), run_id, await _json_body(request))
        return _ok(receipt)

    @app.post("/v1/runs/{run_id}:cancel")
    async def cancel_run(run_id: str, request: Request) -> JSONResponse:
        return _ok(broker.cancel_run(await _owner(ports, request), run_id, await _json_body(request)))

    @app.post("/v1/runs/{run_id}:retry")
    async def retry_run(run_id: str, request: Request) -> JSONResponse:
        return _ok(broker.retry_run(await _owner(ports, request), run_id, await _json_body(request)))

    @app.get("/v1/runs/{run_id}")
    async def get_run(run_id: str, request: Request) -> JSONResponse:
        return _ok(broker.get_run(await _owner(ports, request), run_id))

    @app.get("/v1/runs/{run_id}/status")
    async def get_status(run_id: str, request: Request) -> JSONResponse:
        return _ok(broker.get_status(await _owner(ports, request), run_id))

    @app.get("/v1/runs/{run_id}/teardown")
    async def get_teardown(run_id: str, request: Request) -> JSONResponse:
        return _ok(broker.get_teardown(await _owner(ports, request), run_id))

    @app.get("/v1/runs/{run_id}/logs")
    async def get_logs(run_id: str, request: Request) -> JSONResponse:
        page = broker.get_logs(
            await _owner(ports, request),
            run_id,
            cursor=request.query_params.get("cursor"),
            limit=request.query_params.get("limit"),
        )
        return _ok(page)

    @app.get("/v1/runs/{run_id}/result")
    async def get_result(run_id: str, request: Request) -> JSONResponse:
        return _ok(broker.get_result(await _owner(ports, request), run_id))

    @app.post("/internal/v1/runs/{run_id}:reconcile")
    async def reconcile(run_id: str, request: Request) -> JSONResponse:
        if not await run_in_threadpool(ports.is_controller, request):
            raise ProtocolError(
                ErrorCode.NOT_ALLOWED, "internal route requires the controller identity"
            )
        return _ok(broker.reconcile(run_id))

    @app.post("/internal/v1/runs:reconcile")
    async def reconcile_batch(request: Request) -> JSONResponse:
        if not await run_in_threadpool(ports.is_controller, request):
            raise ProtocolError(
                ErrorCode.NOT_ALLOWED, "internal route requires the controller identity"
            )
        limit = _parse_limit(
            request.query_params.get("limit"),
            default=RECONCILE_BATCH_DEFAULT,
            maximum=RECONCILE_BATCH_MAX,
        )
        after = _parse_run_cursor(request.query_params.get("after"))
        return JSONResponse(
            await run_in_threadpool(broker.reconcile_batch, limit=limit, after=after)
        )

    @app.get("/internal/v1/admissions")
    async def admissions_status(request: Request) -> JSONResponse:
        await _operator(ports, request)
        return JSONResponse(broker.admissions_status())

    @app.post("/internal/v1/admissions:drain")
    async def drain_admissions(request: Request) -> JSONResponse:
        await _operator(ports, request)
        return JSONResponse(broker.set_admissions_open(open_=False))

    @app.post("/internal/v1/admissions:resume")
    async def resume_admissions(request: Request) -> JSONResponse:
        await _operator(ports, request)
        return JSONResponse(broker.set_admissions_open(open_=True))

    @app.post("/internal/v1/runs:bulk-cancel")
    async def bulk_cancel(request: Request) -> JSONResponse:
        await _operator(ports, request)
        body = await _json_body(request)
        outcomes = await run_in_threadpool(broker.bulk_cancel, body)
        return JSONResponse(outcomes)

    return app
