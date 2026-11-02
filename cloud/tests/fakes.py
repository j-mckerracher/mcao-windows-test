"""In-memory adapters and request builders for the contract tests. Test-only; not shipped.

Nothing here reaches a cloud API. The object store enforces the same generation preconditions
that the signed-object rules describe, so a runner plan can be replayed against it honestly.
"""

import secrets
import uuid
from collections.abc import Callable, Mapping
from dataclasses import asdict, dataclass
from datetime import UTC, datetime, timedelta
from typing import Any
from threading import RLock

from mao_cloud.broker import AdmissionSwitch, Ports, RunSnapshot
from mao_cloud.config import AdmissionLimits, Policy
from mao_cloud.protocol import (
    BootstrapSource,
    CreateRunManifest,
    ErrorCode,
    IdempotencyRecord,
    ObjectKind,
    ObservedInput,
    ProtocolError,
    RunRecord,
    RunState,
    ToolCompatibility,
    WorkerBootstrap,
    manifest_digest,
    object_name,
    sha256_hex,
)
from mao_cloud.runner import PlannedWrite
from mao_cloud.signer import DirectSignerClient, SignerService

EPOCH = datetime(2026, 10, 21, 12, 0, 0, tzinfo=UTC)
PROFILE_ID = "pi-standard"
IMAGE_DIGEST = "sha256:" + "d" * 64
SOURCE_TAR = b"synthetic source tarball " * 64
OWNER_A = "owner-a"
OWNER_B = "owner-b"


def uuid7(at: datetime) -> uuid.UUID:
    """A UUIDv7 stamped with the given instant, random tail from the OS entropy source."""
    millis = int(at.timestamp() * 1000)
    rand = secrets.token_bytes(10)
    raw = bytearray(16)
    raw[0:6] = millis.to_bytes(6, "big")
    raw[6] = 0x70 | (rand[0] & 0x0F)
    raw[7] = rand[1]
    raw[8] = 0x80 | (rand[2] & 0x3F)
    raw[9:16] = rand[3:10]
    return uuid.UUID(bytes=bytes(raw))


class ManualClock:
    def __init__(self, start: datetime = EPOCH) -> None:
        self.value = start

    def now(self) -> datetime:
        return self.value

    def advance(self, **delta: float) -> None:
        self.value = self.value + timedelta(**delta)


@dataclass(frozen=True)
class StoreBackup:
    """A point-in-time copy of the run and dedup tables, as a backup would hold them."""

    runs: dict[str, RunRecord]
    idem: dict[tuple[str, uuid.UUID], IdempotencyRecord]
    by_request: dict[tuple[str, uuid.UUID], str]


class InMemoryRunStore:
    """Atomic run writes and shared admission revision, matching the durable store contract."""

    def __init__(self, restore_floor: datetime | None = None) -> None:
        self._runs: dict[str, RunRecord] = {}
        self._idem: dict[tuple[str, uuid.UUID], IdempotencyRecord] = {}
        self._by_request: dict[tuple[str, uuid.UUID], str] = {}
        self._restore_floor = restore_floor
        self._revision = 0
        self._lock = RLock()

    def get_run(self, run_id: str) -> RunRecord | None:
        with self._lock:
            return self._runs.get(run_id)

    def find_run_by_request(self, owner_id: str, request_uuid: uuid.UUID) -> RunRecord | None:
        with self._lock:
            run_id = self._by_request.get((owner_id, request_uuid))
            return self._runs.get(run_id) if run_id is not None else None

    def get_idempotency(self, owner_id: str, request_uuid: uuid.UUID) -> IdempotencyRecord | None:
        with self._lock:
            return self._idem.get((owner_id, request_uuid))

    def restore_floor(self) -> datetime | None:
        with self._lock:
            return self._restore_floor

    def set_restore_floor(self, floor: datetime | None) -> None:
        with self._lock:
            self._restore_floor = floor
            self._revision += 1

    def drop_idempotency(self, owner_id: str, request_uuid: uuid.UUID) -> None:
        """Simulate a dedup row expiring while its run survives."""
        with self._lock:
            self._idem.pop((owner_id, request_uuid), None)
            self._revision += 1

    def run_count(self) -> int:
        with self._lock:
            return len(self._runs)

    def backup(self) -> StoreBackup:
        with self._lock:
            return StoreBackup(dict(self._runs), dict(self._idem), dict(self._by_request))

    def restore(self, backup: StoreBackup, *, floor: datetime) -> None:
        """Restore tables without reusing a revision held by an in-flight admission."""
        with self._lock:
            self._runs = dict(backup.runs)
            self._idem = dict(backup.idem)
            self._by_request = dict(backup.by_request)
            self._restore_floor = floor
            self._revision = max(
                self._revision, max((run.dispatch_order for run in self._runs.values()), default=0)
            ) + 1

    def create_run(self, run: RunRecord, idempotency: IdempotencyRecord) -> None:
        with self._lock:
            key = (run.owner_id, run.request_uuid)
            if run.run_id in self._runs or key in self._idem:
                raise ProtocolError(ErrorCode.VERSION_CONFLICT, "run or dedup record already exists")
            self._runs[run.run_id] = run
            self._idem[key] = idempotency
            self._by_request[key] = run.run_id
            self._revision = max(self._revision, run.dispatch_order) + 1

    def compare_and_set(
        self,
        run: RunRecord,
        *,
        expected_version: int,
        idempotency: IdempotencyRecord | None = None,
        expected_revision: int | None = None,
    ) -> None:
        with self._lock:
            if expected_revision is not None and expected_revision != self._revision:
                raise ProtocolError(
                    ErrorCode.VERSION_CONFLICT, "admission snapshot changed",
                    expected=expected_revision, actual=self._revision,
                )
            current = self._runs.get(run.run_id)
            if current is None:
                raise ProtocolError(ErrorCode.NOT_FOUND, "run not found")
            if current.version != expected_version:
                raise ProtocolError(
                    ErrorCode.VERSION_CONFLICT, "stored version differs from the version read",
                    expected=expected_version, actual=current.version,
                )
            self._runs[run.run_id] = run
            if idempotency is not None:
                self._idem[(idempotency.owner_id, idempotency.request_uuid)] = idempotency
            self._revision += 1

    def admission_snapshot(self) -> RunSnapshot:
        with self._lock:
            return RunSnapshot(records=tuple(self._runs.values()), revision=self._revision)

    def list_runs(self) -> list[RunRecord]:
        return list(self.admission_snapshot().records)


class _Stored:
    def __init__(self, data: bytes, generation: int) -> None:
        self.data = data
        self.generation = generation


class InMemorySigningBackend:
    """Stands in for the signer's own signing identity and records every signature it is asked for."""

    def __init__(self) -> None:
        self.calls: list[tuple[str, str, datetime, dict[str, str]]] = []

    def sign(
        self, name: str, *, method: str, expires_at: datetime, headers: Mapping[str, str]
    ) -> str:
        self.calls.append((name, method, expires_at, dict(headers)))
        return f"https://storage.invalid/{name}?expires={int(expires_at.timestamp())}"


class InMemoryObjectStore:
    """Honors x-goog-if-generation-match: '0' creates only, 'N' overwrites only at generation N."""

    def __init__(self) -> None:
        self._objects: dict[str, _Stored] = {}
        self._next_generation = 1

    def head(self, name: str) -> ObservedInput | None:
        stored = self._objects.get(name)
        if stored is None:
            return None
        return ObservedInput(
            generation=stored.generation,
            size_bytes=len(stored.data),
            sha256=sha256_hex(stored.data),
        )

    def get(self, name: str) -> bytes | None:
        stored = self._objects.get(name)
        return stored.data if stored is not None else None

    def generation_of(self, name: str) -> int:
        stored = self._objects.get(name)
        return stored.generation if stored is not None else 0

    def names(self) -> list[str]:
        """Every stored object name, so a test can scan all that a run left behind."""
        return sorted(self._objects)

    def delete(self, name: str) -> None:
        """Remove an object, as partial cleanup would. Deleting an absent name is a no-op."""
        self._objects.pop(name, None)

    def write(
        self,
        name: str,
        data: bytes,
        *,
        if_generation_match: int | None = None,
    ) -> int:
        """Store bytes under name. Raises ProtocolError(VERSION_CONFLICT) on a failed precondition."""
        current = self._objects.get(name)
        if if_generation_match is not None:
            actual = current.generation if current is not None else 0
            if actual != if_generation_match:
                raise ProtocolError(
                    ErrorCode.VERSION_CONFLICT,
                    "object generation precondition failed",
                    name=name,
                )
        generation = self._next_generation
        self._next_generation += 1
        self._objects[name] = _Stored(data, generation)
        return generation

    def create_only(self, name: str, data: bytes) -> None:
        """Write only when absent. The second write for a name raises VERSION_CONFLICT."""
        self.write(name, data, if_generation_match=0)

    def put_planned(self, planned: PlannedWrite) -> int:
        """Replay a runner plan exactly as its headers request it."""
        raw = planned.headers.get("x-goog-if-generation-match")
        return self.write(
            planned.name,
            planned.body,
            if_generation_match=int(raw) if raw is not None else None,
        )


def _attempt_key(record: RunRecord) -> tuple[str, int, int, str | None]:
    return (record.run_id, record.attempt, record.fence, record.claim_id)


class RecordingLauncher:
    def __init__(self) -> None:
        self.launched: list[Any] = []
        # The intake URL each accepted launch carried. It is a credential, so tests read it only to
        # hand it to the worker entry, never to assert on its text in a message.
        self.intake_urls: list[str] = []
        self.calls = 0
        self.fail_next = False
        self.ambiguous_next = False
        self.fail_stop_next = False
        self.stop_requests: list[tuple[str, int, int, str | None]] = []
        self.stopped_attempts: set[tuple[str, int, int, str | None]] = set()
        # The execution name each launch returns. None models a launch whose answer names none.
        self.execution_name: str | None = None

    def confirm_stopped(self, record: RunRecord) -> None:
        self.stopped_attempts.add(_attempt_key(record))

    def stop_requested(self, record: RunRecord) -> bool:
        return _attempt_key(record) in self.stop_requests

    def request_stop(self, record: RunRecord) -> None:
        if self.fail_stop_next:
            self.fail_stop_next = False
            raise RuntimeError("stop request unavailable")
        self.stop_requests.append(_attempt_key(record))

    def execution_stopped(self, record: RunRecord) -> bool:
        return _attempt_key(record) in self.stopped_attempts

    def launch(self, bootstrap: Any, intake_url: str) -> str | None:
        self.calls += 1
        if self.fail_next:
            self.fail_next = False
            raise RuntimeError("launcher unavailable")
        if self.ambiguous_next:
            from mao_cloud.broker import LaunchAmbiguous

            self.ambiguous_next = False
            raise LaunchAmbiguous("launch response lost; the job may or may not exist")
        self.launched.append(bootstrap)
        self.intake_urls.append(intake_url)
        return self.execution_name


def admission_limits(**overrides: Any) -> AdmissionLimits:
    values: dict[str, Any] = {
        "version": 1,
        "team_queue_depth": 20,
        "owner_queue_depth": 10,
        "team_worker_slots": 4,
        "owner_worker_slots": 4,
        "team_daily_worker_seconds": 8 * 3600,
        "queued_deadline_seconds": 30 * 60,
    }
    values.update(overrides)
    return AdmissionLimits(**values)


class InMemoryAdmissionSwitch:
    """Stands in for the admissions switch. Tests flip it between requests."""

    def __init__(self) -> None:
        self._open = True

    def is_open(self) -> bool:
        return self._open

    def set_open(self, open_: bool) -> None:
        self._open = open_


def make_policy(**overrides: Any) -> Policy:
    compatibility = ToolCompatibility(
        image_digest=IMAGE_DIGEST,
        profile_id=PROFILE_ID,
        toolchain_manifest_digest="e" * 64,
        instruction_digest="f" * 64,
    )
    values: dict[str, Any] = {
        "server_id": "srv-test",
        "targets": frozenset({"cloud-pilot"}),
        "harnesses": frozenset({"pi"}),
        "models": frozenset({"gemini-3.8-flash"}),
        "thinking": frozenset({"low", "medium", "high"}),
        "profiles": {PROFILE_ID: compatibility},
        "stale_after_seconds": 120,
        "skew_seconds": 30,
        "startup_timeout_seconds": 600,
        "max_attempts": 3,
        "admission": admission_limits(),
    }
    values.update(overrides)
    return Policy(**values)


def create_payload(
    *,
    request_uuid: uuid.UUID | None = None,
    source: bytes = SOURCE_TAR,
    at: datetime = EPOCH,
    base_sha: str = "a" * 40,
    **overrides: Any,
) -> dict[str, Any]:
    """A valid create request whose canonical_digest matches its manifest."""
    manifest: dict[str, Any] = {
        "protocol_version": "v1",
        "request_uuid": str(request_uuid or uuid7(at)),
        "harness": "pi",
        "model": "gemini-3.8-flash",
        "thinking": "medium",
        "toolchain_profile": PROFILE_ID,
        "target": {"id": "cloud-pilot"},
        "source": {
            "base_sha": base_sha,
            "source_digest": "1" * 64,
            "task_digest": "2" * 64,
            "config_digest": "3" * 64,
        },
        "source_upload": {
            "size_bytes": len(source),
            "sha256": sha256_hex(source),
            "media_type": "application/x-tar",
        },
        "budget": {"max_runtime_seconds": 600},
        "capacity": "standard",
        "assignment": {"task_id": "DPES-805", "workflow_correlation": "wf-805"},
    }
    manifest.update(overrides)
    digest = manifest_digest(CreateRunManifest.model_validate(manifest))
    return {**manifest, "canonical_digest": digest}


def start_payload(create: dict[str, Any], *, generation: int, source: bytes = SOURCE_TAR) -> dict[str, Any]:
    return {
        "protocol_version": "v1",
        "canonical_digest": create["canonical_digest"],
        "source_upload": {
            "object_generation": generation,
            "sha256": sha256_hex(source),
            "size_bytes": len(source),
        },
    }


SIGNER_CALLER = "mcao-controller@example.test"


def make_signer(store: InMemoryRunStore, clock: ManualClock) -> DirectSignerClient:
    """The controller's signer in tests: the real service over in-memory run state and a fake key."""
    return DirectSignerClient(
        SignerService(
            runs=store,
            backend=InMemorySigningBackend(),
            controller_identity=SIGNER_CALLER,
            now=clock.now,
        ),
        caller=SIGNER_CALLER,
    )


def make_ports(
    *,
    store: InMemoryRunStore,
    objects: InMemoryObjectStore,
    launcher: RecordingLauncher,
    clock: ManualClock,
    run_ids: list[str] | None = None,
    claim_ids: list[str] | None = None,
    admissions: AdmissionSwitch | None = None,
) -> Ports:
    run_pool = iter(run_ids or [])
    claim_pool = iter(claim_ids or [])

    def _next_run() -> str:
        return next(run_pool, None) or f"run_{uuid.uuid4().hex}"

    def _next_claim() -> str:
        return next(claim_pool, None) or f"claim_{uuid.uuid4().hex}"

    return Ports(
        store=store,
        objects=objects,
        signer=make_signer(store, clock),
        launcher=launcher,
        clock=clock,
        owner_of=lambda request: request.headers.get("x-owner") or None,
        is_controller=lambda request: request.headers.get("x-controller") == "1",
        is_operator=lambda request: request.headers.get("x-operator") == "1",
        admissions=admissions or InMemoryAdmissionSwitch(),
        new_run_id=_next_run,
        new_claim_id=_next_claim,
    )


def owner_headers(owner: str = OWNER_A) -> dict[str, str]:
    return {"x-owner": owner}


def refusal_code(call: Callable[[], object]) -> ErrorCode:
    """The typed code a refused call raises. Fails the test if the call returns instead."""
    try:
        call()
    except ProtocolError as exc:
        return exc.code
    raise AssertionError("expected a ProtocolError, but the call returned")


CONTROLLER_HEADERS = {"x-controller": "1"}
OPERATOR_HEADERS = {"x-operator": "1"}

RUN_ID = "run_" + "a" * 32
CLAIM_ID = "claim_" + "b" * 32
RUNNER_NONCE = "1f" * 16
DEFAULT_MAX_RUNTIME_SECONDS = 3600


def run_record(**overrides: Any) -> RunRecord:
    """A RunRecord built directly, for pure state-machine and time tests. No broker involved.

    The default deadline is DEFAULT_MAX_RUNTIME_SECONDS after EPOCH, so tests that need a live
    deadline get one.
    """
    body = {
        key: value
        for key, value in create_payload(
            budget={"max_runtime_seconds": DEFAULT_MAX_RUNTIME_SECONDS}
        ).items()
        if key != "canonical_digest"
    }
    manifest = CreateRunManifest.model_validate(body)
    values: dict[str, Any] = {
        "run_id": RUN_ID,
        "owner_id": OWNER_A,
        "server_id": "srv-test",
        "request_uuid": manifest.request_uuid,
        "manifest": manifest,
        "manifest_digest": manifest_digest(manifest),
        "image_digest": IMAGE_DIGEST,
        "state": RunState.QUEUED,
        "version": 1,
        "attempt": 1,
        "fence": 1,
        "max_attempts": 3,
        "admitted_at": EPOCH,
        "state_entered_at": EPOCH,
        "deadline_at": EPOCH + timedelta(seconds=DEFAULT_MAX_RUNTIME_SECONDS),
    }
    values.update(overrides)
    return RunRecord.model_validate(values)


def bootstrap_source() -> BootstrapSource:
    return BootstrapSource(
        object_name=object_name(ObjectKind.INPUT, run_id=RUN_ID),
        generation=1,
        sha256=sha256_hex(SOURCE_TAR),
        size_bytes=len(SOURCE_TAR),
        base_sha="a" * 40,
    )


def tool_compatibility() -> ToolCompatibility:
    return make_policy().profiles[PROFILE_ID]


def worker_bootstrap(**overrides: Any) -> WorkerBootstrap:
    """A bootstrap whose immutable identity matches the receipt for run_record()'s defaults."""
    values: dict[str, Any] = {
        "protocol_version": "v1",
        "run_id": RUN_ID,
        "attempt": 1,
        "fence": 1,
        "claim_id": CLAIM_ID,
        "source": bootstrap_source(),
        "compatibility": tool_compatibility(),
        "deadline_at": EPOCH + timedelta(seconds=DEFAULT_MAX_RUNTIME_SECONDS),
        "max_runtime_seconds": DEFAULT_MAX_RUNTIME_SECONDS,
    }
    values.update(overrides)
    return WorkerBootstrap.model_validate(values)


def policy_document(policy: Policy | None = None) -> dict[str, Any]:
    """The JSON policy file that policy_from_text reads back into the same policy."""
    policy = policy or make_policy()
    return {
        "server_id": policy.server_id,
        "targets": sorted(policy.targets),
        "harnesses": sorted(policy.harnesses),
        "models": sorted(policy.models),
        "thinking": sorted(policy.thinking),
        "profiles": {pid: c.model_dump(mode="json") for pid, c in policy.profiles.items()},
        "stale_after_seconds": policy.stale_after_seconds,
        "skew_seconds": policy.skew_seconds,
        "startup_timeout_seconds": policy.startup_timeout_seconds,
        "max_attempts": policy.max_attempts,
        "admission": asdict(policy.admission),
    }
