"""Version recovery over HTTP: a refused request leaves the run and its identity intact.

Only protocol version v1 exists. These tests send unsupported versions on either side of it (v0,
an older version that never existed, and v9, a newer one), then a corrected v1 request on the same
run and owner. No older supported version is invented to make a test pass. A refused request must
open nothing, and a corrected request must recover with the same run_id, attempt, and receipt.
"""

import pytest

from fakes import SOURCE_TAR, create_payload, owner_headers, start_payload
from flows import World, view
from mao_cloud.protocol import AcceptedReceipt, ObjectKind, object_name

UNSUPPORTED_VERSIONS = ["v0", "v9"]


@pytest.mark.parametrize("version", UNSUPPORTED_VERSIONS)
def test_refused_start_then_corrected_v1_start_recovers_with_identity_preserved(
    world: World, version: str
) -> None:
    create = create_payload()
    created = world.client.post("/v1/runs", json=create, headers=owner_headers())
    assert created.status_code == 201, created.text
    run_id = created.json()["run"]["run_id"]
    name = created.json()["upload"]["object_name"]
    assert name == object_name(ObjectKind.INPUT, run_id=run_id)
    world.objects.write(name, SOURCE_TAR)
    generation = world.objects.generation_of(name)
    before = view(world, run_id)

    refused = world.client.post(
        f"/v1/runs/{run_id}:start",
        json={
            **start_payload(create, generation=generation),
            "protocol_version": version,
        },
        headers=owner_headers(),
    )
    assert refused.status_code == 400, refused.text
    assert refused.json()["error"]["code"] == "unsupported_version"
    assert refused.json()["error"]["detail"]["version"] == version

    after_refusal = view(world, run_id)
    assert after_refusal["run_id"] == before["run_id"] == run_id
    assert after_refusal["attempt"] == before["attempt"] == 1
    assert after_refusal["state"] == before["state"] == "awaiting_upload"
    assert after_refusal["version"] == before["version"]

    corrected = world.client.post(
        f"/v1/runs/{run_id}:start",
        json=start_payload(create, generation=generation),
        headers=owner_headers(),
    )
    assert corrected.status_code == 200, corrected.text
    receipt = AcceptedReceipt.model_validate(corrected.json())
    assert receipt.run_id == run_id
    assert receipt.attempt == 1
    assert view(world, run_id)["attempt"] == 1

    replayed = world.client.post(
        f"/v1/runs/{run_id}:start",
        json=start_payload(create, generation=generation),
        headers=owner_headers(),
    )
    assert replayed.status_code == 200, replayed.text
    assert AcceptedReceipt.model_validate(replayed.json()) == receipt


@pytest.mark.parametrize("version", UNSUPPORTED_VERSIONS)
def test_refused_create_opens_nothing_and_the_corrected_create_opens_one_run(
    world: World, version: str
) -> None:
    create = create_payload()

    refused = world.client.post(
        "/v1/runs",
        json={**create, "protocol_version": version},
        headers=owner_headers(),
    )
    assert refused.status_code == 400, refused.text
    assert refused.json()["error"]["code"] == "unsupported_version"

    corrected = world.client.post("/v1/runs", json=create, headers=owner_headers())
    assert corrected.status_code == 201, (
        f"a refused request must not admit the create: {corrected.text}"
    )
    run_id = corrected.json()["run"]["run_id"]

    replayed = world.client.post("/v1/runs", json=create, headers=owner_headers())
    assert replayed.status_code == 200, replayed.text
    assert replayed.json()["run"]["run_id"] == run_id
    assert view(world, run_id)["attempt"] == 1
