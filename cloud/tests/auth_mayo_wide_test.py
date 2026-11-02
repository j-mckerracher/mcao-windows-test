"""Mayo-wide access: a verified caller on the Mayo domain is admitted as its own owner.

In mayo-wide mode the control API admits a caller whose ID token verifies for the API audience and
whose verified email is exactly <local>@mayo.edu, even when its subject is not in the identity map.
Such a caller is an owner keyed by its own numeric subject, so it reaches only its own runs and never
becomes an operator or a scheduler. Allowlist mode admits only the fixed map.

Each spoofing case is paired in the same test with a genuine Mayo caller that must be admitted. A
refusal alone would pass on any build that admits nothing new, so the pairing is what makes each
test fail before the change and pass after it.
"""

import logging
from collections.abc import Callable
from dataclasses import replace
from typing import Any
from uuid import UUID

import pytest
from fastapi.testclient import TestClient

from auth_support import (
    AUDIENCE,
    GOOGLE_SIGNER,
    IDENTITIES,
    NO_AUTH,
    OWNER_ROUTES,
    ROGUE_SIGNER,
    SUBJECT_OWNER_A,
    UNKNOWN_RUN_ID,
    UNPUBLISHED_SIGNER,
    AuthWorld,
    CertTransport,
    MutableIdentities,
    admit_and_start_as,
    as_owner_a,
    as_scheduler,
    bearer,
    create_as,
    make_auth_world,
    mint,
    reconcile_as,
)
from fakes import (
    OWNER_B,
    InMemoryObjectStore,
    InMemoryRunStore,
    ManualClock,
    RecordingLauncher,
    create_payload,
    make_policy,
    make_ports,
)
from mao_cloud.broker import BearerIdentity, PolicyIdentities, create_app
from mao_cloud.config import AccessMode, Identity, Policy, Role
from mao_cloud.protocol import RunState

MAYO_SUBJECT = "109911223344556677"
MAYO_EMAIL = "jane.doe@mayo.edu"
OTHER_MAYO_SUBJECT = "109911223344556688"
OTHER_MAYO_EMAIL = "john.roe@mayo.edu"
# A subject the identity map does not hold, used for every spoofed token.
UNMAPPED_SUBJECT = "109900000000000001"


def mayo_world(mode: AccessMode = AccessMode.MAYO_WIDE) -> AuthWorld:
    return make_auth_world(MutableIdentities(IDENTITIES, access_mode=mode))


def as_mayo(
    subject: str = MAYO_SUBJECT, email: str | None = MAYO_EMAIL, **claims: Any
) -> dict[str, str]:
    return bearer(mint(subject=subject, email=email, **claims))


def as_other_mayo() -> dict[str, str]:
    return as_mayo(subject=OTHER_MAYO_SUBJECT, email=OTHER_MAYO_EMAIL)


def refusal_of_create(world: AuthWorld) -> Any:
    """What an unauthenticated create answers. A refused caller must look exactly like this."""
    return world.client.post("/v1/runs", json=create_payload(), headers=NO_AUTH)


# -- admission and ownership ---------------------------------------------------


def test_any_verified_mayo_caller_creates_and_reads_its_own_run() -> None:
    world = mayo_world()

    created, _ = create_as(world, as_mayo())
    read = world.client.get(f"/v1/runs/{created}", headers=as_mayo())

    assert read.status_code == 200
    assert read.json()["run_id"] == created


def test_mayo_owner_is_keyed_by_its_own_numeric_subject() -> None:
    world = mayo_world()

    run_id, _ = create_as(world, as_mayo())

    assert world.store.get_run(run_id).owner_id == MAYO_SUBJECT


@pytest.mark.parametrize("caller", [as_owner_a, as_other_mayo], ids=["mapped-owner", "other-mayo"])
@pytest.mark.parametrize("route", list(OWNER_ROUTES))
def test_a_run_owned_by_a_mayo_caller_answers_the_missing_run_to_every_other_caller(
    route: str, caller: Callable[[], dict[str, str]]
) -> None:
    world = mayo_world()
    run_id, _ = admit_and_start_as(world, as_mayo())
    before = world.store.get_run(run_id)
    call = OWNER_ROUTES[route]

    refused = call(world.client, run_id, caller())
    missing = call(world.client, UNKNOWN_RUN_ID, caller())

    assert refused.status_code == 404
    assert refused.json() == missing.json()
    assert world.store.get_run(run_id) == before


def test_the_owner_comes_from_the_token_so_a_body_owner_is_refused_and_creates_nothing() -> None:
    world = mayo_world()
    assert world.client.post("/v1/runs", json=create_payload(), headers=as_mayo()).status_code == 201
    payload = {**create_payload(), "owner_id": OTHER_MAYO_SUBJECT}

    response = world.client.post("/v1/runs", json=payload, headers=as_mayo())

    assert response.status_code == 400
    assert response.json()["error"]["code"] == "client_controlled_field"
    request_uuid = UUID(payload["request_uuid"])
    assert world.store.find_run_by_request(MAYO_SUBJECT, request_uuid) is None
    assert world.store.find_run_by_request(OTHER_MAYO_SUBJECT, request_uuid) is None


# -- a mayo owner gains no peer authority ----------------------------------------


def test_mayo_owner_is_refused_reconcile_while_the_scheduler_still_reconciles() -> None:
    world = mayo_world()
    run_id, _ = admit_and_start_as(world, as_mayo())
    before = world.store.get_run(run_id)

    refused = reconcile_as(world, run_id, as_mayo())

    assert refused.status_code == 403
    assert refused.json()["error"]["code"] == "not_allowed"
    assert world.store.get_run(run_id) == before
    assert reconcile_as(world, run_id, as_scheduler()).status_code == 200


# -- spoofed identities are refused, each beside a genuine caller that is admitted --

SPOOFED_EMAILS = [
    pytest.param("jane.doe@mayo.edu.evil.test", id="lookalike-domain"),
    pytest.param("jane.doe@evilmayo.edu", id="suffix-lookalike"),
    pytest.param("jane.doe@cdh.mayo.edu", id="subdomain"),
    pytest.param("jane.doe@mayo.edu.", id="trailing-dot"),
    pytest.param("jane.doe@MAYO.EDU", id="upper-case-domain"),
    pytest.param("jane@evil.test@mayo.edu", id="second-at-sign"),
    pytest.param("invoker@mao-test.iam.gserviceaccount.com", id="service-account"),
]


@pytest.mark.parametrize("email", SPOOFED_EMAILS)
def test_an_email_outside_the_exact_mayo_domain_is_refused_while_a_genuine_caller_is_admitted(
    email: str,
) -> None:
    world = mayo_world()
    create_as(world, as_mayo())

    spoof = world.client.post(
        "/v1/runs", json=create_payload(), headers=bearer(mint(subject=UNMAPPED_SUBJECT, email=email))
    )

    assert spoof.status_code == 403
    assert spoof.json() == refusal_of_create(world).json()


@pytest.mark.parametrize("email_verified", [False, None], ids=["unverified", "unstated"])
def test_an_unverified_mayo_email_is_refused_while_a_genuine_caller_is_admitted(
    email_verified: bool | None,
) -> None:
    world = mayo_world()
    create_as(world, as_mayo())

    spoof = world.client.post(
        "/v1/runs",
        json=create_payload(),
        headers=bearer(mint(subject=UNMAPPED_SUBJECT, email=MAYO_EMAIL, email_verified=email_verified)),
    )

    assert spoof.status_code == 403
    assert spoof.json() == refusal_of_create(world).json()


def test_a_token_without_an_email_is_refused_while_a_genuine_caller_is_admitted() -> None:
    world = mayo_world()
    create_as(world, as_mayo())

    spoof = world.client.post(
        "/v1/runs",
        json=create_payload(),
        headers=bearer(mint(subject=UNMAPPED_SUBJECT, email=None)),
    )

    assert spoof.status_code == 403
    assert spoof.json() == refusal_of_create(world).json()


FORGED_TOKENS = [
    pytest.param({"audience": "https://other.example.test"}, id="wrong-audience"),
    pytest.param({"issuer": "https://evil.example.test"}, id="wrong-issuer"),
    pytest.param({"lifetime_seconds": -60}, id="expired"),
    pytest.param({"signer": ROGUE_SIGNER}, id="forged-signature"),
    pytest.param({"signer": UNPUBLISHED_SIGNER}, id="unpublished-key"),
]


@pytest.mark.parametrize("overrides", FORGED_TOKENS)
def test_a_mayo_email_on_a_token_that_fails_verification_is_refused(
    overrides: dict[str, Any],
) -> None:
    world = mayo_world()
    create_as(world, as_mayo())

    spoof = world.client.post(
        "/v1/runs",
        json=create_payload(),
        headers=bearer(mint(subject=UNMAPPED_SUBJECT, email=MAYO_EMAIL, **overrides)),
    )

    assert spoof.status_code == 403
    assert spoof.json() == refusal_of_create(world).json()


def test_a_mapped_subject_presenting_a_mayo_address_is_not_admitted_as_a_derived_owner() -> None:
    world = mayo_world()
    create_as(world, as_mayo())
    create_as(world, as_owner_a())

    spoof = world.client.post(
        "/v1/runs",
        json=create_payload(),
        headers=bearer(mint(subject=SUBJECT_OWNER_A, email=MAYO_EMAIL)),
    )

    assert spoof.status_code == 403
    assert spoof.json() == refusal_of_create(world).json()


# -- allowlist mode and mode changes ----------------------------------------------


def test_closing_the_mode_refuses_the_mayo_caller_and_the_mapped_owner_still_creates_runs() -> None:
    world = mayo_world()
    create_as(world, as_mayo())

    world.identities.access_mode = AccessMode.ALLOWLIST

    assert create_as_status(world, as_mayo()) == 403
    assert create_as_status(world, as_owner_a()) == 201


def test_allowlist_is_the_default_and_refuses_a_verified_mayo_caller() -> None:
    world = make_auth_world()

    assert create_as_status(world, as_mayo()) == 403
    assert create_as_status(world, as_owner_a()) == 201


def test_reopening_the_mode_admits_the_mayo_caller_on_its_next_request() -> None:
    world = mayo_world(AccessMode.ALLOWLIST)
    assert create_as_status(world, as_mayo()) == 403

    world.identities.access_mode = AccessMode.MAYO_WIDE

    assert create_as_status(world, as_mayo()) == 201


def test_a_policy_refresh_switches_the_mode_on_the_next_request() -> None:
    client, directory, store = refreshable_app(policy_with_mode(AccessMode.ALLOWLIST))
    assert client.post("/v1/runs", json=create_payload(), headers=as_mayo()).status_code == 403

    directory.replace(policy_with_mode(AccessMode.MAYO_WIDE))
    created = client.post("/v1/runs", json=create_payload(), headers=as_mayo())
    assert created.status_code == 201, created.text
    run_id = created.json()["run"]["run_id"]

    directory.replace(policy_with_mode(AccessMode.ALLOWLIST))

    refused = client.get(f"/v1/runs/{run_id}", headers=as_mayo())
    assert refused.status_code == 403
    assert refused.json() == client.get(f"/v1/runs/{run_id}", headers=NO_AUTH).json()
    assert store.get_run(run_id).owner_id == MAYO_SUBJECT


# -- revocation under mayo-wide ---------------------------------------------------


@pytest.mark.parametrize("route", list(OWNER_ROUTES))
def test_closing_the_mode_refuses_a_mayo_owner_on_every_owner_route_and_its_run_is_unchanged(
    route: str,
) -> None:
    world = mayo_world()
    run_id, _ = admit_and_start_as(world, as_mayo())
    before = world.store.get_run(run_id)
    world.identities.access_mode = AccessMode.ALLOWLIST
    call = OWNER_ROUTES[route]

    refused = call(world.client, run_id, as_mayo())

    assert refused.status_code == 403
    assert refused.json() == call(world.client, run_id, NO_AUTH).json()
    assert world.store.get_run(run_id) == before


def test_a_queued_mayo_run_keeps_progressing_under_the_scheduler_after_the_mode_closes() -> None:
    world = mayo_world()
    run_id, _ = admit_and_start_as(world, as_mayo())
    world.identities.access_mode = AccessMode.ALLOWLIST

    assert world.client.get(f"/v1/runs/{run_id}", headers=as_mayo()).status_code == 403
    cancel = world.client.post(
        f"/v1/runs/{run_id}:cancel", json={"protocol_version": "v1"}, headers=as_mayo()
    )
    assert cancel.status_code == 403
    reconciled = reconcile_as(world, run_id, as_scheduler())

    assert reconciled.status_code == 200, reconciled.text
    assert world.store.get_run(run_id).state is RunState.DISPATCHING
    assert len(world.launcher.launched) == 1


def test_reopening_the_mode_restores_the_mayo_owner_its_same_run() -> None:
    world = mayo_world()
    run_id, _ = create_as(world, as_mayo())
    world.identities.access_mode = AccessMode.ALLOWLIST
    world.identities.access_mode = AccessMode.MAYO_WIDE

    response = world.client.get(f"/v1/runs/{run_id}", headers=as_mayo())

    assert response.status_code == 200
    assert response.json()["run_id"] == run_id


# -- remapping a subject that is now in the map -----------------------------------


def test_a_mayo_subject_later_mapped_to_another_owner_cannot_reach_its_derived_run() -> None:
    world = mayo_world()
    run_id, _ = create_as(world, as_mayo())
    before = world.store.get_run(run_id)
    world.identities.put(
        Identity(subject=MAYO_SUBJECT, email=MAYO_EMAIL, role=Role.OWNER, owner_id=OWNER_B)
    )

    read = world.client.get(f"/v1/runs/{run_id}", headers=as_mayo())
    missing = world.client.get(f"/v1/runs/{UNKNOWN_RUN_ID}", headers=as_mayo())

    assert read.status_code == 404
    assert read.json() == missing.json()
    assert world.store.get_run(run_id) == before


def test_a_mayo_subject_later_mapped_as_scheduler_reconciles_but_owns_nothing() -> None:
    world = mayo_world()
    run_id, _ = admit_and_start_as(world, as_mayo())
    world.identities.put(Identity(subject=MAYO_SUBJECT, email=MAYO_EMAIL, role=Role.SCHEDULER))

    assert world.client.get(f"/v1/runs/{run_id}", headers=as_mayo()).status_code == 403
    assert reconcile_as(world, run_id, as_mayo()).status_code == 200


# -- audit ------------------------------------------------------------------------


def test_mayo_admission_and_refusal_are_audited_by_subject_without_email_or_token(
    caplog: pytest.LogCaptureFixture,
) -> None:
    caplog.set_level(logging.INFO, logger="mao_cloud.audit")
    world = mayo_world()
    admitted = mint(subject=MAYO_SUBJECT, email=MAYO_EMAIL)
    spoofed_email = "jane.doe@mayo.edu.evil.test"
    spoofed = mint(subject=UNMAPPED_SUBJECT, email=spoofed_email)

    world.client.post("/v1/runs", json=create_payload(), headers=bearer(admitted))
    world.client.post("/v1/runs", json=create_payload(), headers=bearer(spoofed))

    audit = [record.getMessage() for record in caplog.records if record.name == "mao_cloud.audit"]
    assert any("accepted" in line and MAYO_SUBJECT in line and "role=owner" in line for line in audit)
    assert any("refused" in line and "not_mayo_email" in line and UNMAPPED_SUBJECT in line for line in audit)
    for secret in (MAYO_EMAIL, spoofed_email, admitted, spoofed):
        assert secret not in caplog.text


# -- helpers for direct app construction ------------------------------------------


def create_as_status(world: AuthWorld, headers: dict[str, str]) -> int:
    return world.client.post("/v1/runs", json=create_payload(), headers=headers).status_code


def policy_with_mode(mode: AccessMode) -> Policy:
    return make_policy(api_audience=AUDIENCE, identities=IDENTITIES, access_mode=mode)


def refreshable_app(policy: Policy) -> tuple[TestClient, PolicyIdentities, InMemoryRunStore]:
    """The control API over the production directory, so a policy refresh changes the mode."""
    directory = PolicyIdentities(policy)
    bearer_identity = BearerIdentity(
        audience=AUDIENCE, directory=directory, transport=CertTransport(GOOGLE_SIGNER)
    )
    store = InMemoryRunStore()
    ports = replace(
        make_ports(
            store=store,
            objects=InMemoryObjectStore(),
            launcher=RecordingLauncher(),
            clock=ManualClock(),
        ),
        owner_of=bearer_identity.owner_of,
        is_controller=bearer_identity.is_controller,
        is_operator=bearer_identity.is_operator,
    )
    return TestClient(create_app(policy=policy, ports=ports)), directory, store


@pytest.mark.parametrize("subject", ["owner-a", "12a", "1" * 31, "", "١٢٣", "123\n"])
def test_invalid_subject_cannot_become_a_derived_mayo_owner(subject: str) -> None:
    world = mayo_world()
    create_as(world, as_mayo())
    before = world.store.run_count()
    response = world.client.post("/v1/runs", json=create_payload(), headers=as_mayo(subject=subject))
    assert response.status_code == 403
    assert response.json() == refusal_of_create(world).json()
    assert world.store.run_count() == before


def test_removing_a_mapped_grant_restores_only_derived_ownership_in_mayo_wide_mode() -> None:
    base = policy_with_mode(AccessMode.MAYO_WIDE)
    client, directory, store = refreshable_app(base)
    created = client.post("/v1/runs", json=create_payload(), headers=as_mayo())
    assert created.status_code == 201
    derived_run = created.json()["run"]["run_id"]
    assert store.get_run(derived_run).owner_id == MAYO_SUBJECT

    mapped = Identity(subject=MAYO_SUBJECT, email=MAYO_EMAIL, role=Role.OWNER, owner_id=OWNER_B)
    directory.replace(replace(base, identities=(*base.identities, mapped)))
    created = client.post("/v1/runs", json=create_payload(), headers=as_mayo())
    assert created.status_code == 201
    mapped_run = created.json()["run"]["run_id"]
    assert store.get_run(mapped_run).owner_id == OWNER_B

    directory.replace(base)
    assert client.get(f"/v1/runs/{derived_run}", headers=as_mayo()).status_code == 200
    assert client.get(f"/v1/runs/{mapped_run}", headers=as_mayo()).status_code == 404
    created = client.post("/v1/runs", json=create_payload(), headers=as_mayo())
    assert created.status_code == 201
    assert store.get_run(created.json()["run"]["run_id"]).owner_id == MAYO_SUBJECT
