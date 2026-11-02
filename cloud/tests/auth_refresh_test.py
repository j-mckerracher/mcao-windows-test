"""Policy refresh: a replaced policy changes the identity map on the very next request.

These tests use the production directory, PolicyIdentities, rather than the mutable test double, so
they exercise the refresh path the broker actually runs. A subject removed or remapped by a
replacement policy is refused, or resolved to its new owner, on its next request.
"""

from collections.abc import Iterable
from dataclasses import replace

from fastapi.testclient import TestClient

from auth_support import (
    AUDIENCE,
    EMAIL_OWNER_A,
    GOOGLE_SIGNER,
    IDENTITIES,
    NO_AUTH,
    SUBJECT_OWNER_A,
    UNKNOWN_RUN_ID,
    CertTransport,
    as_owner_a,
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
from mao_cloud.config import Identity, Policy, Role


def policy_with(identities: Iterable[Identity]) -> Policy:
    return make_policy(api_audience=AUDIENCE, identities=tuple(identities))


def policy_app() -> tuple[TestClient, PolicyIdentities, InMemoryRunStore]:
    """The control API wired to BearerIdentity over PolicyIdentities, built from IDENTITIES."""
    directory = PolicyIdentities(policy_with(IDENTITIES))
    bearer = BearerIdentity(
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
        owner_of=bearer.owner_of,
        is_controller=bearer.is_controller,
        is_operator=bearer.is_operator,
    )
    app = create_app(policy=policy_with(IDENTITIES), ports=ports)
    return TestClient(app), directory, store


def create_run_as_owner_a(client: TestClient) -> str:
    created = client.post("/v1/runs", json=create_payload(), headers=as_owner_a())
    assert created.status_code == 201, created.text
    return created.json()["run"]["run_id"]


def test_subject_removed_by_a_replacement_policy_is_refused_on_its_next_request() -> None:
    client, directory, store = policy_app()
    run_id = create_run_as_owner_a(client)
    before = store.get_run(run_id)
    directory.replace(policy_with(i for i in IDENTITIES if i.subject != SUBJECT_OWNER_A))

    refused = client.get(f"/v1/runs/{run_id}", headers=as_owner_a())

    assert refused.status_code == 403
    assert refused.json() == client.get(f"/v1/runs/{run_id}", headers=NO_AUTH).json()
    assert store.get_run(run_id) == before


def test_subject_remapped_by_a_replacement_policy_gets_the_missing_run_answer() -> None:
    client, directory, store = policy_app()
    run_id = create_run_as_owner_a(client)
    before = store.get_run(run_id)
    directory.replace(
        policy_with(
            Identity(subject=SUBJECT_OWNER_A, email=EMAIL_OWNER_A, role=Role.OWNER, owner_id=OWNER_B)
            if i.subject == SUBJECT_OWNER_A
            else i
            for i in IDENTITIES
        )
    )

    read = client.get(f"/v1/runs/{run_id}", headers=as_owner_a())
    missing = client.get(f"/v1/runs/{UNKNOWN_RUN_ID}", headers=as_owner_a())

    assert read.status_code == 404
    assert read.json() == missing.json()
    assert store.get_run(run_id) == before


def test_restoring_the_policy_restores_access_to_the_same_run() -> None:
    client, directory, _store = policy_app()
    run_id = create_run_as_owner_a(client)
    directory.replace(policy_with(i for i in IDENTITIES if i.subject != SUBJECT_OWNER_A))
    directory.replace(policy_with(IDENTITIES))

    response = client.get(f"/v1/runs/{run_id}", headers=as_owner_a())

    assert response.status_code == 200
    assert response.json()["run_id"] == run_id
