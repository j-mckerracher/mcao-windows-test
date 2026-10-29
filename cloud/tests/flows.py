"""Drive one run through the HTTP API, with the worker side replayed through runner plans.

Helpers take a World so tests can reach the clock, stores, and launcher they need. Nothing here
talks to a cloud; the object store and launcher are the in-memory fakes from fakes.py.
"""

import io
import tarfile
from dataclasses import dataclass
from datetime import datetime
from typing import Any, Literal

from fastapi.testclient import TestClient

from mao_cloud.trusted_bundle import SOURCE_MEMBER, check_bundle_header

from fakes import (
    CONTROLLER_HEADERS,
    OPERATOR_HEADERS,
    OWNER_A,
    SOURCE_TAR,
    InMemoryAdmissionSwitch,
    InMemoryObjectStore,
    InMemoryRunStore,
    ManualClock,
    RUNNER_NONCE,
    RecordingLauncher,
    create_payload,
    make_policy,
    make_ports,
    owner_headers,
    start_payload,
    uuid7,
)
from mao_cloud.broker import create_app
from mao_cloud.config import Policy
from mao_cloud.protocol import (
    AcceptedReceipt,
    ObjectKind,
    WorkerBootstrap,
    object_name,
)
from mao_cloud.runner import plan_claim, plan_status


@dataclass
class World:
    client: TestClient
    clock: ManualClock
    store: InMemoryRunStore
    objects: InMemoryObjectStore
    launcher: RecordingLauncher
    admissions: InMemoryAdmissionSwitch


def make_world(
    *,
    policy: Policy | None = None,
    clock: ManualClock | None = None,
    store: InMemoryRunStore | None = None,
) -> World:
    clock = clock or ManualClock()
    store = store or InMemoryRunStore()
    objects = InMemoryObjectStore()
    launcher = RecordingLauncher()
    admissions = InMemoryAdmissionSwitch()
    ports = make_ports(
        store=store,
        objects=objects,
        launcher=launcher,
        clock=clock,
        admissions=admissions,
    )
    app = create_app(policy=policy or make_policy(), ports=ports)
    return World(TestClient(app), clock, store, objects, launcher, admissions)


def admissions_status(world: World) -> dict[str, Any]:
    response = world.client.get("/internal/v1/admissions", headers=OPERATOR_HEADERS)
    assert response.status_code == 200, response.text
    return response.json()


def source_tip(archive: bytes) -> str:
    """The commit a source archive's bundle names: the base SHA a controller claims for that source."""
    with tarfile.open(fileobj=io.BytesIO(archive)) as tar:
        member = tar.extractfile(SOURCE_MEMBER)
        assert member is not None
        return check_bundle_header(member.read())[0][0].decode("ascii")


def admit_and_start(
    world: World,
    owner: str = OWNER_A,
    *,
    source: bytes = SOURCE_TAR,
    **overrides: Any,
) -> tuple[str, dict[str, Any], AcceptedReceipt]:
    """Create, upload the source, and start. Returns (run_id, create payload, receipt)."""
    create = create_payload(source=source, **overrides)
    created = world.client.post("/v1/runs", json=create, headers=owner_headers(owner))
    assert created.status_code == 201, created.text
    run_id = created.json()["run"]["run_id"]
    name = created.json()["upload"]["object_name"]
    assert name == object_name(ObjectKind.INPUT, run_id=run_id)
    world.objects.write(name, source)
    started = world.client.post(
        f"/v1/runs/{run_id}:start",
        json=start_payload(create, generation=world.objects.generation_of(name), source=source),
        headers=owner_headers(owner),
    )
    assert started.status_code == 200, started.text
    return run_id, create, AcceptedReceipt.model_validate(started.json())


LONG_BUDGET = {"max_runtime_seconds": 3600}


def view(world: World, run_id: str, owner: str = OWNER_A) -> dict[str, Any]:
    response = world.client.get(f"/v1/runs/{run_id}", headers=owner_headers(owner))
    assert response.status_code == 200, response.text
    return response.json()


def retry_body(
    world: World,
    *,
    expected_version: int,
    expected_attempt: int = 1,
    request_uuid: str | None = None,
) -> dict[str, Any]:
    """A retry request. Pass request_uuid to resend the same request; omit it for a new one."""
    return {
        "protocol_version": "v1",
        "request_uuid": request_uuid or str(uuid7(world.clock.now())),
        "expected_attempt": expected_attempt,
        "expected_version": expected_version,
    }


def post_retry(world: World, run_id: str, body: dict[str, Any]) -> Any:
    return world.client.post(f"/v1/runs/{run_id}:retry", json=body, headers=owner_headers())


def drive_to_launch_uncertain(world: World, run_id: str) -> None:
    """Dispatch, let the launch go silent past startup and the stale window, then fail it."""
    dispatch(world, run_id)
    world.clock.advance(seconds=601)
    assert reconcile(world, run_id)["state"] == "reconciling"
    world.clock.advance(seconds=121)
    failed = reconcile(world, run_id)
    assert failed["state"] == "failed"
    assert failed["failure"]["code"] == "launch_uncertain"
    world.launcher.confirm_stopped(world.store.get_run(run_id))
    reconcile(world, run_id)


def fail_launch_uncertain(world: World) -> tuple[str, AcceptedReceipt]:
    run_id, _, receipt = admit_and_start(world, budget=LONG_BUDGET)
    drive_to_launch_uncertain(world, run_id)
    return run_id, receipt


def reconcile(world: World, run_id: str) -> dict[str, Any]:
    response = world.client.post(
        f"/internal/v1/runs/{run_id}:reconcile", headers=CONTROLLER_HEADERS
    )
    assert response.status_code == 200, response.text
    return response.json()


def dispatch(world: World, run_id: str) -> WorkerBootstrap:
    """Reconcile a queued run into dispatching. Returns the bootstrap the launcher received."""
    assert reconcile(world, run_id)["state"] == "dispatching"
    return world.launcher.launched[-1]


def claim(world: World, bootstrap: WorkerBootstrap, receipt: AcceptedReceipt) -> None:
    """Replay the runner's claim, then reconcile so the run moves to starting."""
    world.objects.put_planned(
        plan_claim(bootstrap, receipt, now=world.clock.now(), runner_nonce=RUNNER_NONCE)
    )
    assert reconcile(world, bootstrap.run_id)["state"] == "starting"


def to_starting(world: World, run_id: str, receipt: AcceptedReceipt) -> WorkerBootstrap:
    """Dispatch, then let the runner claim the attempt. Returns the bootstrap."""
    bootstrap = dispatch(world, run_id)
    claim(world, bootstrap, receipt)
    return bootstrap


def write_status(
    world: World,
    bootstrap: WorkerBootstrap,
    *,
    seq: int,
    log_watermark: int = 0,
    phase: Literal["starting", "running", "collecting"] = "running",
    observed_at: datetime | None = None,
) -> None:
    """Write a status report with the generation precondition the runner would use.

    observed_at defaults to the server clock; a test passes another instant to model a worker
    whose clock is ahead or behind.
    """
    name = object_name(ObjectKind.STATUS, run_id=bootstrap.run_id, attempt=bootstrap.attempt)
    world.objects.put_planned(
        plan_status(
            bootstrap,
            seq=seq,
            phase=phase,
            observed_at=observed_at or world.clock.now(),
            log_watermark=log_watermark,
            message=f"{phase} {seq}",
            expected_generation=world.objects.generation_of(name),
        )
    )
