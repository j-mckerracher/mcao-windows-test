"""End-to-end isolation: worker credential text never reaches a caller or a stored object.

The worker writes its log lines and status message through the runner plans, the same path a
real worker uses. Redaction happens at the model boundary, so each planted credential must be
replaced before anything is stored. The raw text must then be absent from every HTTP surface a
caller can read and from every object the run left in storage. Runs against the in-memory fakes;
no cloud is contacted. The launch bootstrap is deliberately not scanned: it is delivered to the
worker, not to a caller or to storage.
"""

from typing import Literal

import pytest

from fakes import OWNER_A, owner_headers
from flows import World, admit_and_start, make_world, reconcile, to_starting
from mao_cloud.protocol import ObjectKind, object_name
from mao_cloud.runner import plan_log_entry, plan_result, plan_status

# Each value is the secret part of one redaction rule's match. Distinct strings so a leak is
# unambiguous in any surface.
ACCESS_TOKEN = "ya29.a0AfH6SMBSentinelAccessToken01"
BEARER_VALUE = "SentinelBearerToken01"
JWT_SIGNATURE = "SentinelJwtSignature01"
SIGNED_URL_SIGNATURE = "SentinelSignedUrl01"
PASSWORD_VALUE = "SentinelPassword01"
API_KEY_VALUE = "SentinelApiKey01"

SENTINELS = (
    ACCESS_TOKEN,
    BEARER_VALUE,
    JWT_SIGNATURE,
    SIGNED_URL_SIGNATURE,
    PASSWORD_VALUE,
    API_KEY_VALUE,
)

LOG_LINES = [
    f"curl -H 'Authorization: Bearer {BEARER_VALUE}' https://example.invalid",
    f"oauth {ACCESS_TOKEN}",
    f"id eyJSentinelHeader01.eyJSentinelPayload01.{JWT_SIGNATURE}",
    f"fetched https://storage.googleapis.com/bucket/o?X-Goog-Signature={SIGNED_URL_SIGNATURE}",
    f"password={PASSWORD_VALUE}",
    "tests passed",
]
STATUS_MESSAGE = f"retrying with api_key={API_KEY_VALUE}"


def _run_worker_with_credentials(
    world: World, outcome: Literal["succeeded", "failed"]
) -> tuple[str, str]:
    """Drive one run to a terminal outcome, with credential text in the worker's writes.

    Returns (run_id, status object name).
    """
    run_id, _, receipt = admit_and_start(world)
    bootstrap = to_starting(world, run_id, receipt)
    status_name = object_name(ObjectKind.STATUS, run_id=run_id, attempt=bootstrap.attempt)

    world.objects.put_planned(plan_log_entry(bootstrap, seq=1, lines=LOG_LINES))
    world.objects.put_planned(
        plan_status(
            bootstrap,
            seq=1,
            phase="running",
            observed_at=world.clock.now(),
            log_watermark=1,
            message=STATUS_MESSAGE,
            expected_generation=world.objects.generation_of(status_name),
        )
    )
    assert reconcile(world, run_id)["state"] == "running"

    succeeded = outcome == "succeeded"
    world.objects.put_planned(
        plan_result(
            bootstrap,
            outcome=outcome,
            exit_code=0 if succeeded else 1,
            commit_sha="b" * 40 if succeeded else None,
            artifacts=[],
        )
    )
    assert reconcile(world, run_id)["state"] == outcome
    return run_id, status_name


def _caller_surfaces(world: World, run_id: str) -> dict[str, str]:
    """Raw bodies of every read route an owner can reach for the run."""
    surfaces: dict[str, str] = {}
    for label, path in {
        "run": f"/v1/runs/{run_id}",
        "status": f"/v1/runs/{run_id}/status",
        "logs": f"/v1/runs/{run_id}/logs",
        "result": f"/v1/runs/{run_id}/result",
    }.items():
        response = world.client.get(path, headers=owner_headers(OWNER_A))
        assert response.status_code == 200, f"{label}: {response.text}"
        surfaces[f"{label} response"] = response.text
    return surfaces


def _stored_objects(world: World, run_id: str) -> dict[str, str]:
    """The run record and every object the run left in storage, as text."""
    record = world.store.get_run(run_id)
    assert record is not None
    stored = {"run record": record.model_dump_json()}
    for name in world.objects.names():
        data = world.objects.get(name)
        assert data is not None
        stored[f"object {name}"] = data.decode()
    return stored


@pytest.mark.parametrize("outcome", ["succeeded", "failed"])
def test_no_worker_credential_reaches_a_caller_surface_or_stored_object(
    outcome: Literal["succeeded", "failed"],
) -> None:
    world = make_world()
    run_id, status_name = _run_worker_with_credentials(world, outcome)

    surfaces = _caller_surfaces(world, run_id)
    stored = _stored_objects(world, run_id)

    for label, text in {**surfaces, **stored}.items():
        for secret in SENTINELS:
            assert secret not in text, f"{secret!r} reached {label}"

    # The replacement markers prove redaction ran. Absence alone could also mean the lines
    # were dropped, so the non-credential text must still be there.
    assert "[REDACTED:oauth-token]" in surfaces["logs response"]
    assert "[REDACTED:signed-url]" in surfaces["logs response"]
    assert "tests passed" in surfaces["logs response"]
    assert "api_key=[REDACTED]" in stored[f"object {status_name}"]
