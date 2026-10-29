"""Schema evolution at the HTTP boundary: unknown fields, removed required fields, and versions.

Protocol v1 has no compatibility matrix between revisions (protocol-v1.md section 17). Its rule is
that a change needs a new protocol version, so a v1 server refuses what it does not define rather
than guessing at it or dropping it. These tests pin that refusal on each route that takes a body.
"""

import pytest
from fakes import create_payload, owner_headers, start_payload
from flows import World, fail_launch_uncertain, post_retry, retry_body, view


def test_unknown_field_from_a_newer_client_is_refused_and_creates_nothing(world: World) -> None:
    body = {**create_payload(), "region_preference": "us-central1"}
    response = world.client.post("/v1/runs", json=body, headers=owner_headers())
    assert response.status_code == 400, response.text
    error = response.json()["error"]
    assert error["code"] == "unknown_field"
    assert error["detail"]["fields"] == ["region_preference"]
    assert world.store.run_count() == 0


def test_unknown_field_on_a_retry_is_refused_and_opens_no_attempt(world: World) -> None:
    run_id, _ = fail_launch_uncertain(world)
    body = {
        **retry_body(world, expected_version=view(world, run_id)["version"]),
        "priority_hint": 1,
    }
    response = post_retry(world, run_id, body)
    assert response.status_code == 400, response.text
    error = response.json()["error"]
    assert error["code"] == "unknown_field"
    assert error["detail"]["fields"] == ["priority_hint"]
    assert view(world, run_id)["attempt"] == 1
    assert len(world.launcher.launched) == 1


def test_create_missing_a_required_field_is_refused_and_creates_nothing(world: World) -> None:
    body = create_payload()
    del body["source_upload"]
    response = world.client.post("/v1/runs", json=body, headers=owner_headers())
    assert response.status_code == 400, response.text
    error = response.json()["error"]
    assert error["code"] == "invalid_request"
    assert error["detail"]["fields"] == ["source_upload"]
    assert world.store.run_count() == 0


def test_start_missing_its_upload_receipt_is_refused_and_the_run_stays_awaiting_upload(
    world: World,
) -> None:
    create = create_payload()
    created = world.client.post("/v1/runs", json=create, headers=owner_headers())
    assert created.status_code == 201, created.text
    run_id = created.json()["run"]["run_id"]

    body = start_payload(create, generation=1)
    del body["source_upload"]
    response = world.client.post(f"/v1/runs/{run_id}:start", json=body, headers=owner_headers())
    assert response.status_code == 400, response.text
    error = response.json()["error"]
    assert error["code"] == "invalid_request"
    assert error["detail"]["fields"] == ["source_upload"]
    assert view(world, run_id)["state"] == "awaiting_upload"


# -- protocol versions the server does not define -----------------------------


@pytest.mark.parametrize("version", ["v0", "v2"])
@pytest.mark.parametrize("route", ["create", "start", "retry", "cancel"])
def test_unsupported_protocol_version_is_refused_on_every_route(
    world: World, route: str, version: str
) -> None:
    create = create_payload()
    if route == "create":
        response = world.client.post(
            "/v1/runs", json={**create, "protocol_version": version}, headers=owner_headers()
        )
        assert world.store.run_count() == 0
    else:
        made = world.client.post("/v1/runs", json=create, headers=owner_headers())
        assert made.status_code == 201, made.text
        run_id = made.json()["run"]["run_id"]
        if route == "start":
            body = {**start_payload(create, generation=1), "protocol_version": version}
            response = world.client.post(
                f"/v1/runs/{run_id}:start", json=body, headers=owner_headers()
            )
        elif route == "retry":
            body = {**retry_body(world, expected_version=1), "protocol_version": version}
            response = post_retry(world, run_id, body)
        else:
            response = world.client.post(
                f"/v1/runs/{run_id}:cancel",
                json={"protocol_version": version},
                headers=owner_headers(),
            )
        assert view(world, run_id)["state"] == "awaiting_upload"
        assert view(world, run_id)["attempt"] == 1

    assert response.status_code == 400, response.text
    error = response.json()["error"]
    assert error["code"] == "unsupported_version"
    assert error["detail"]["version"] == version
