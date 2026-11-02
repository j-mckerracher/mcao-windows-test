"""Revocation: a token keeps verifying, but authority follows the identity map on every request.

Revoking a caller takes effect on its next request, with no denylist. Work already accepted is not
cancelled by revocation: a queued run continues under the scheduler until its own deadline. A run's
owner_id is fixed when it is created and never rebinds, so a remapped identity cannot take over a
run that another owner created.
"""

import logging

import pytest
from google.oauth2 import id_token

from auth_support import (
    AUDIENCE,
    CANCEL_BODY,
    EMAIL_OWNER_A,
    IDENTITIES,
    NO_AUTH,
    OWNER_ROUTES,
    START_BODY,
    SUBJECT_OWNER_A,
    UNKNOWN_RUN_ID,
    AuthWorld,
    admit_and_start_as,
    as_owner_a,
    as_scheduler,
    bearer,
    create_as,
    make_auth_world,
    mint,
    reconcile_as,
)
from fakes import OWNER_A, OWNER_B
from mao_cloud.config import Identity, Role
from mao_cloud.protocol import RunState

ORIGINAL_OWNER_A = next(identity for identity in IDENTITIES if identity.subject == SUBJECT_OWNER_A)


@pytest.fixture
def world() -> AuthWorld:
    return make_auth_world()


@pytest.mark.parametrize("route", OWNER_ROUTES)
def test_revoked_owner_is_refused_on_every_owner_route_and_its_run_is_unchanged(
    world: AuthWorld, route: str
) -> None:
    run_id, _ = admit_and_start_as(world, as_owner_a())
    before = world.store.get_run(run_id)
    world.identities.remove(SUBJECT_OWNER_A)
    call = OWNER_ROUTES[route]

    refused = call(world.client, run_id, as_owner_a())

    assert refused.status_code == 403
    assert refused.json() == call(world.client, run_id, NO_AUTH).json()
    assert world.store.get_run(run_id) == before


def test_revoked_owner_is_refused_on_create_and_gets_no_run_back(world: AuthWorld) -> None:
    _run_id, create = create_as(world, as_owner_a())
    world.identities.remove(SUBJECT_OWNER_A)

    replay = world.client.post("/v1/runs", json=create, headers=as_owner_a())

    assert replay.status_code == 403
    assert "run" not in replay.json()


def test_revoked_between_create_and_start_is_refused_and_the_run_stays_unstarted(
    world: AuthWorld,
) -> None:
    run_id, _ = create_as(world, as_owner_a())
    before = world.store.get_run(run_id)
    world.identities.remove(SUBJECT_OWNER_A)

    refused = world.client.post(
        f"/v1/runs/{run_id}:start", json=START_BODY, headers=as_owner_a()
    )

    assert refused.status_code == 403
    assert refused.json()["error"]["code"] == "not_allowed"
    assert world.store.get_run(run_id) == before
    assert not world.launcher.launched


def test_revoked_token_still_verifies_so_the_identity_map_alone_decides(world: AuthWorld) -> None:
    token = mint()
    world.identities.remove(SUBJECT_OWNER_A)

    claims = id_token.verify_oauth2_token(token, world.transport, audience=AUDIENCE)

    assert claims["sub"] == SUBJECT_OWNER_A
    # Denial is decided before the run is looked up, so an unknown run gets the refusal too.
    refused = world.client.get(f"/v1/runs/{UNKNOWN_RUN_ID}", headers=bearer(token))
    assert refused.status_code == 403


def test_restoring_the_identity_restores_access_to_the_same_run(world: AuthWorld) -> None:
    run_id, _ = create_as(world, as_owner_a())
    world.identities.remove(SUBJECT_OWNER_A)
    world.identities.put(ORIGINAL_OWNER_A)

    response = world.client.get(f"/v1/runs/{run_id}", headers=as_owner_a())

    assert response.status_code == 200
    assert response.json()["run_id"] == run_id


def test_remapped_identity_cannot_reach_the_run_it_created(world: AuthWorld) -> None:
    run_id, _ = create_as(world, as_owner_a())
    before = world.store.get_run(run_id)
    world.identities.put(
        Identity(subject=SUBJECT_OWNER_A, email=EMAIL_OWNER_A, role=Role.OWNER, owner_id=OWNER_B)
    )

    read = world.client.get(f"/v1/runs/{run_id}", headers=as_owner_a())
    missing = world.client.get(f"/v1/runs/{UNKNOWN_RUN_ID}", headers=as_owner_a())

    assert read.status_code == 404
    assert read.json() == missing.json()
    assert world.store.get_run(run_id) == before


def test_owner_id_never_rebinds_when_the_identity_is_remapped(world: AuthWorld) -> None:
    run_id, _ = create_as(world, as_owner_a())
    world.identities.put(
        Identity(subject=SUBJECT_OWNER_A, email=EMAIL_OWNER_A, role=Role.OWNER, owner_id=OWNER_B)
    )

    assert world.store.get_run(run_id).owner_id == OWNER_A


def test_remapped_identity_replaying_a_create_gets_a_new_run_for_its_new_owner(
    world: AuthWorld,
) -> None:
    run_id, create = create_as(world, as_owner_a())
    world.identities.put(
        Identity(subject=SUBJECT_OWNER_A, email=EMAIL_OWNER_A, role=Role.OWNER, owner_id=OWNER_B)
    )

    replay = world.client.post("/v1/runs", json=create, headers=as_owner_a())

    assert replay.status_code == 201
    new_run_id = replay.json()["run"]["run_id"]
    assert new_run_id != run_id
    assert world.store.get_run(new_run_id).owner_id == OWNER_B
    assert world.store.get_run(run_id).owner_id == OWNER_A


def test_role_change_from_owner_to_scheduler_ends_owner_access_and_grants_reconcile(
    world: AuthWorld,
) -> None:
    run_id, _ = admit_and_start_as(world, as_owner_a())
    world.identities.put(
        Identity(subject=SUBJECT_OWNER_A, email=EMAIL_OWNER_A, role=Role.SCHEDULER)
    )

    assert world.client.get(f"/v1/runs/{run_id}", headers=as_owner_a()).status_code == 403
    assert reconcile_as(world, run_id, as_owner_a()).status_code == 200


def test_queued_run_keeps_progressing_under_the_scheduler_after_its_owner_is_revoked(
    world: AuthWorld,
) -> None:
    run_id, _ = admit_and_start_as(world, as_owner_a())
    world.identities.remove(SUBJECT_OWNER_A)

    assert world.client.get(f"/v1/runs/{run_id}", headers=as_owner_a()).status_code == 403
    cancel = world.client.post(
        f"/v1/runs/{run_id}:cancel", json=CANCEL_BODY, headers=as_owner_a()
    )
    assert cancel.status_code == 403
    reconciled = reconcile_as(world, run_id, as_scheduler())

    assert reconciled.status_code == 200, reconciled.text
    record = world.store.get_run(run_id)
    assert record.state is RunState.DISPATCHING
    assert len(world.launcher.launched) == 1


def test_replay_by_a_still_authorized_owner_returns_the_run_without_readmitting_it(
    world: AuthWorld,
) -> None:
    run_id, create = create_as(world, as_owner_a())
    before = world.store.get_run(run_id)

    replay = world.client.post("/v1/runs", json=create, headers=as_owner_a())

    assert replay.status_code == 200
    assert replay.json()["run"]["run_id"] == run_id
    assert world.store.get_run(run_id) == before


def test_revoked_caller_is_audited_by_subject_without_owner_or_token(
    world: AuthWorld, caplog: pytest.LogCaptureFixture
) -> None:
    caplog.set_level(logging.INFO, logger="mao_cloud.audit")
    token = mint()
    world.identities.remove(SUBJECT_OWNER_A)

    world.client.get(f"/v1/runs/{UNKNOWN_RUN_ID}", headers=bearer(token))

    audit = [record.getMessage() for record in caplog.records if record.name == "mao_cloud.audit"]
    assert any("unknown_subject" in line and SUBJECT_OWNER_A in line for line in audit)
    assert not any(OWNER_A in line for line in audit)
    assert token not in caplog.text
