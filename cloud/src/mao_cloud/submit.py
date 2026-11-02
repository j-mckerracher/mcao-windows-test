"""Submit one exported snapshot as a run: create, upload, start, each step safe to repeat.

The request UUID names the request. Its record (run_record.py) holds the state reached so far, so a
retry resumes at the last durable step rather than creating a second run:

  planned  -> create   (201 new run, or 200 replay with the upload capability while awaiting upload)
  created  -> upload   (signed PUT; 200 yields the object generation)
  uploaded -> start    (200 yields the accepted receipt; a repeated start replays it)
  started  -> done, with no network call

Exit codes: 0 started; 1 refused, and the record says why; 75 retry the same command later.
Refusals that cannot change on retry are recorded, so a later run does not repeat them.

Digests bound into the manifest (defined here, not in protocol-v1; the worker does not verify them):
  task_digest   sha256 of canonical JSON {"task_id", "prompt_id"}
  config_digest sha256 of canonical JSON {"harness", "model", "target", "thinking", "toolchain_profile"}
canonical JSON is sort_keys with no whitespace. canonical_digest is protocol manifest_digest.
"""

import hashlib
import json
import secrets
import time
import uuid
from collections.abc import Mapping
from dataclasses import dataclass, field
from typing import Any, NoReturn, Protocol, TypeVar

from pydantic import BaseModel, ValidationError

from mao_cloud.protocol import (
    AcceptedReceipt,
    CreateRunManifest,
    CreateRunRequest,
    CreateRunResponse,
    RunState,
    SourceReceipt,
    StartRunRequest,
    UploadCapability,
    manifest_digest,
)
from mao_cloud.run_record import RecordConflict, RecordStore, RunRecord
from mao_cloud.source import Snapshot

RETRY = 75
_T = TypeVar("_T", bound=BaseModel)


@dataclass(frozen=True)
class Reply:
    status: int
    body: Any = None
    headers: Mapping[str, str] = field(default_factory=dict)


class TransportError(Exception):
    """The request or its reply was lost. The service may or may not have acted."""


class Transport(Protocol):
    def create(self, body: Mapping[str, Any]) -> Reply: ...

    def put(self, url: str, headers: Mapping[str, str], data: bytes) -> Reply: ...

    def start(self, run_id: str, body: Mapping[str, Any]) -> Reply: ...


@dataclass(frozen=True)
class SubmitRequest:
    task_id: str
    prompt_id: str
    toolchain_profile: str
    target: str
    harness: str
    model: str
    thinking: str
    max_runtime_seconds: int
    workflow_correlation: str
    request_uuid: str


@dataclass(frozen=True)
class Plan:
    request_uuid: str
    canonical_digest: str
    create_body: dict[str, Any]


@dataclass(frozen=True)
class SubmitResult:
    exit_code: int
    run_id: str | None = None
    code: str | None = None
    message: str = ""


class _Stop(Exception):
    """Unwinds one submit attempt to its exit code, leaving the record at its last durable state."""

    def __init__(self, exit_code: int, code: str, message: str) -> None:
        super().__init__(message)
        self.exit_code = exit_code
        self.code = code
        self.message = message


def new_request_uuid() -> str:
    """A random RFC 9562 version 7 UUID: 48-bit Unix milliseconds, then 74 random bits."""
    millis = time.time_ns() // 1_000_000
    random_bits = int.from_bytes(secrets.token_bytes(10), "big")
    value = (
        ((millis & ((1 << 48) - 1)) << 80)
        | (0x7 << 76)
        | (((random_bits >> 62) & 0xFFF) << 64)
        | (0b10 << 62)
        | (random_bits & ((1 << 62) - 1))
    )
    return str(uuid.UUID(int=value))


def plan(snapshot: Snapshot, request: SubmitRequest) -> Plan:
    """Build and validate the create body. Invalid input raises pydantic's ValidationError, before
    any record or network call."""
    manifest = CreateRunManifest.model_validate(
        {
            "protocol_version": "v1",
            "request_uuid": request.request_uuid,
            "harness": request.harness,
            "model": request.model,
            "thinking": request.thinking,
            "toolchain_profile": request.toolchain_profile,
            "target": {"id": request.target},
            "source": {
                "base_sha": snapshot.base_sha,
                "source_digest": snapshot.source_digest,
                "task_digest": _digest(
                    {"task_id": request.task_id, "prompt_id": request.prompt_id}
                ),
                "config_digest": _digest(
                    {
                        "harness": request.harness,
                        "model": request.model,
                        "target": request.target,
                        "thinking": request.thinking,
                        "toolchain_profile": request.toolchain_profile,
                    }
                ),
            },
            "source_upload": {
                "size_bytes": snapshot.archive_size,
                "sha256": snapshot.archive_sha256,
                "media_type": "application/x-tar",
            },
            "budget": {"max_runtime_seconds": request.max_runtime_seconds},
            "capacity": "standard",
            "assignment": {
                "task_id": request.task_id,
                "workflow_correlation": request.workflow_correlation,
            },
        }
    )
    digest = manifest_digest(manifest)
    create = CreateRunRequest.model_validate(
        {**manifest.model_dump(mode="json", exclude_none=True), "canonical_digest": digest}
    )
    return Plan(
        request_uuid=str(manifest.request_uuid),
        canonical_digest=digest,
        create_body=create.model_dump(mode="json", exclude_none=True),
    )


def submit_snapshot(
    snapshot: Snapshot,
    request: SubmitRequest,
    *,
    records: RecordStore,
    transport: Transport,
) -> SubmitResult:
    prepared = plan(snapshot, request)
    with records.lock(prepared.request_uuid):
        try:
            record = records.open_or_create(
                prepared.request_uuid,
                canonical_digest=prepared.canonical_digest,
                create_body=prepared.create_body,
                archive=snapshot.archive,
            )
        except RecordConflict as exc:
            return SubmitResult(1, None, "request_uuid_conflict", str(exc))
        try:
            _advance(record, records, transport)
        except _Stop as stop:
            return SubmitResult(stop.exit_code, record.run_id, stop.code, stop.message)
        except TransportError as exc:
            return SubmitResult(RETRY, record.run_id, "transport_error", str(exc))
        return SubmitResult(0, record.run_id, None, "run started")


def _advance(record: RunRecord, records: RecordStore, transport: Transport) -> None:
    if record.state == "refused":
        refusal = record.refusal or {"code": "refused", "message": "this request was refused"}
        raise _Stop(1, refusal["code"], refusal["message"])
    if record.state == "started":
        return
    _verify_archive(record, records)
    if record.state == "planned":
        _create(record, records, transport)
    if record.state == "created":
        _upload(record, records, transport)
    if record.state == "uploaded":
        _start(record, records, transport)


def _verify_archive(record: RunRecord, records: RecordStore) -> None:
    try:
        archive = records.archive(record)
    except OSError as exc:
        raise _Stop(
            1, "archive_mismatch", "the stored source archive for this request is missing"
        ) from exc
    if (
        hashlib.sha256(archive).hexdigest() != record.archive_sha256
        or len(archive) != record.archive_size
    ):
        raise _Stop(
            1,
            "archive_mismatch",
            "the stored source archive does not match its record; use a new request UUID",
        )


def _create(record: RunRecord, records: RecordStore, transport: Transport) -> None:
    _apply_create(record, records, transport.create(record.create_body))


def _apply_create(record: RunRecord, records: RecordStore, reply: Reply) -> None:
    if reply.status in (200, 201):
        response = _parse(CreateRunResponse, reply.body)
        record.run_id = response.run.run_id
        if response.upload is not None:
            record.upload = response.upload.model_dump(mode="json")
            record.state = "created"
        elif record.generation is not None:
            # Replay start with the retained upload descriptor to recover the accepted receipt.
            record.state = "uploaded"
        elif response.run.state in {
            RunState.QUEUED,
            RunState.DISPATCHING,
            RunState.STARTING,
            RunState.RUNNING,
            RunState.COLLECTING,
            RunState.RECONCILING,
            RunState.SUCCEEDED,
        }:
            record.state = "started"
        else:
            # Cancelled or expired uploads can also omit a capability without any accepted start.
            failed = response.run.failure
            code = failed.code.value if failed is not None else "start_not_accepted"
            message = (
                failed.message if failed is not None and failed.message else
                f"the run is {response.run.state.value}; prior start acceptance was not established"
            )
            _refuse(record, records, code, message)
        records.save(record)
        return
    _fail(record, records, reply)


def _upload(record: RunRecord, records: RecordStore, transport: Transport) -> None:
    reply = _put(record, records, transport)
    if reply.status in (401, 403):
        # An expired or refused signed URL: ask the service for a fresh one by replaying the create,
        # then try once more. A second refusal stops the attempt without marking the record refused,
        # because a later attempt can still succeed.
        _apply_create(record, records, transport.create(record.create_body))
        if record.state != "created":
            return
        reply = _put(record, records, transport)
        if reply.status in (401, 403):
            raise _Stop(
                1,
                "upload_expired",
                "the upload URL was refused twice; submit again to request a fresh upload",
            )
    if reply.status == 412:
        # The object already exists under this request, so an earlier attempt landed and its reply
        # was lost. v1 offers no read of the generation, so the request cannot resume: refuse it.
        _refuse(
            record,
            records,
            "upload_unknown",
            "the source already exists under this request, so an earlier upload landed; "
            "submit again with a new request UUID",
        )
    if reply.status != 200:
        _fail(record, records, reply)
    generation_text = _header(reply.headers, "x-goog-generation")
    if generation_text is None or not generation_text.isdigit() or int(generation_text) < 1:
        _refuse(
            record,
            records,
            "upload_unknown",
            "the storage reply named no object generation; submit again with a new request UUID",
        )
    record.generation = int(generation_text)
    record.state = "uploaded"
    records.save(record)


def _put(record: RunRecord, records: RecordStore, transport: Transport) -> Reply:
    upload = UploadCapability.model_validate(record.upload)
    return transport.put(upload.url, upload.headers, records.archive(record))


def _start(record: RunRecord, records: RecordStore, transport: Transport) -> None:
    if record.run_id is None or record.generation is None:
        raise _Stop(1, "invalid_record", "the record has an upload but no run or generation")
    body = StartRunRequest(
        protocol_version="v1",
        canonical_digest=record.canonical_digest,
        source_upload=SourceReceipt(
            object_generation=record.generation,
            sha256=record.archive_sha256,
            size_bytes=record.archive_size,
        ),
    ).model_dump(mode="json")
    reply = transport.start(record.run_id, body)
    if reply.status != 200:
        _fail(record, records, reply)
    receipt = _parse(AcceptedReceipt, reply.body)
    record.receipt = receipt.model_dump(mode="json")
    record.state = "started"
    records.save(record)


def _fail(record: RunRecord, records: RecordStore, reply: Reply) -> NoReturn:
    """A server answer that is not success. Rate limits, pauses, and server faults retry; any other
    refusal is final for this request UUID."""
    code, message = _error_of(reply)
    if reply.status in (429, 503) or reply.status >= 500:
        raise _Stop(RETRY, code, message)
    _refuse(record, records, code, message)


def _refuse(record: RunRecord, records: RecordStore, code: str, message: str) -> NoReturn:
    record.state = "refused"
    record.refusal = {"code": code, "message": message}
    records.save(record)
    raise _Stop(1, code, message)


def _error_of(reply: Reply) -> tuple[str, str]:
    error = reply.body.get("error") if isinstance(reply.body, dict) else None
    detail: dict[str, Any] = error if isinstance(error, dict) else {}
    code = detail.get("code")
    message = detail.get("message")
    return (
        str(code) if code else f"http_{reply.status}",
        str(message) if message else f"the service answered HTTP {reply.status}",
    )


def _parse(model: type[_T], body: Any) -> _T:
    try:
        return model.model_validate(body)
    except ValidationError as exc:
        raise _Stop(
            1,
            "invalid_response",
            f"the service reply does not match the protocol ({exc.error_count()} problem(s))",
        ) from exc


def _header(headers: Mapping[str, str], name: str) -> str | None:
    for key, value in headers.items():
        if key.lower() == name:
            return value
    return None


def _digest(payload: Mapping[str, Any]) -> str:
    canonical = json.dumps(payload, sort_keys=True, separators=(",", ":")).encode("utf-8")
    return hashlib.sha256(canonical).hexdigest()
