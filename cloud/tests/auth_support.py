"""Offline fixtures for the caller-identity tests.

Identity tokens are RS256 JWTs signed by throwaway keys. The certificate for the trusted key is
served by an in-memory transport, so google-auth's real verifier runs end to end without reaching
Google. The identity map is mutable here, standing in for provisioning changes that land between
two requests. Test-only; not shipped.
"""

import json
import time
from collections.abc import Callable, Iterable
from dataclasses import dataclass, replace
from datetime import UTC, datetime, timedelta
from typing import Any

from cryptography import x509
from cryptography.hazmat.primitives import hashes, serialization
from cryptography.hazmat.primitives.asymmetric import rsa
from cryptography.x509.oid import NameOID
from fastapi.testclient import TestClient
from google.auth import crypt, jwt

from fakes import (
    EPOCH,
    OWNER_A,
    OWNER_B,
    SOURCE_TAR,
    InMemoryObjectStore,
    InMemoryRunStore,
    ManualClock,
    RecordingLauncher,
    create_payload,
    make_policy,
    make_ports,
    start_payload,
    uuid7,
)
from flows import World
from mao_cloud.broker import BearerIdentity, IdentitySnapshot, create_app
from mao_cloud.config import AccessMode, Identity, Role
from mao_cloud.protocol import ObjectKind, object_name

AUDIENCE = "https://mao-cloud.example.test"
ISSUER = "https://accounts.google.com"

SUBJECT_OWNER_A = "104455667788990011"
SUBJECT_OWNER_B = "104455667788990022"
SUBJECT_OPERATOR = "104455667788990033"
SUBJECT_SCHEDULER = "104455667788990044"
EMAIL_OWNER_A = "invoker-a@mao-test.iam.gserviceaccount.com"
EMAIL_OWNER_B = "invoker-b@mao-test.iam.gserviceaccount.com"
EMAIL_OPERATOR = "operator@mao-test.iam.gserviceaccount.com"
EMAIL_SCHEDULER = "scheduler@mao-test.iam.gserviceaccount.com"

IDENTITIES: tuple[Identity, ...] = (
    Identity(subject=SUBJECT_OWNER_A, email=EMAIL_OWNER_A, role=Role.OWNER, owner_id=OWNER_A),
    Identity(subject=SUBJECT_OWNER_B, email=EMAIL_OWNER_B, role=Role.OWNER, owner_id=OWNER_B),
    Identity(subject=SUBJECT_OPERATOR, email=EMAIL_OPERATOR, role=Role.OPERATOR),
    Identity(subject=SUBJECT_SCHEDULER, email=EMAIL_SCHEDULER, role=Role.SCHEDULER),
)

UNKNOWN_RUN_ID = "run_" + "f" * 32


@dataclass(frozen=True)
class SigningKey:
    kid: str
    private_pem: bytes
    certificate_pem: bytes


def make_signing_key(kid: str) -> SigningKey:
    key = rsa.generate_private_key(public_exponent=65537, key_size=2048)
    name = x509.Name([x509.NameAttribute(NameOID.COMMON_NAME, f"mao-cloud test signer {kid}")])
    now = datetime.now(UTC)
    certificate = (
        x509.CertificateBuilder()
        .subject_name(name)
        .issuer_name(name)
        .public_key(key.public_key())
        .serial_number(x509.random_serial_number())
        .not_valid_before(now - timedelta(days=1))
        .not_valid_after(now + timedelta(days=1))
        .sign(key, hashes.SHA256())
    )
    return SigningKey(
        kid=kid,
        private_pem=key.private_bytes(
            serialization.Encoding.PEM,
            serialization.PrivateFormat.PKCS8,
            serialization.NoEncryption(),
        ),
        certificate_pem=certificate.public_bytes(serialization.Encoding.PEM),
    )


GOOGLE_SIGNER = make_signing_key("google-kid-1")
# Same key id as the trusted signer but a different key: a forged signature under a known kid.
ROGUE_SIGNER = make_signing_key("google-kid-1")
# A key whose id is not in the published certificate set.
UNPUBLISHED_SIGNER = make_signing_key("rotated-kid-9")


def mint(
    signer: SigningKey = GOOGLE_SIGNER,
    *,
    subject: str = SUBJECT_OWNER_A,
    email: str | None = EMAIL_OWNER_A,
    email_verified: bool | None = True,
    audience: str = AUDIENCE,
    issuer: str = ISSUER,
    lifetime_seconds: int = 3600,
) -> str:
    """An identity token shaped like an IAM generateIdToken result with includeEmail=true."""
    issued = int(time.time())
    claims: dict[str, Any] = {
        "iss": issuer,
        "aud": audience,
        "sub": subject,
        "iat": issued,
        "exp": issued + lifetime_seconds,
    }
    if email is not None:
        claims["email"] = email
    if email_verified is not None:
        claims["email_verified"] = email_verified
    signed = jwt.encode(crypt.RSASigner.from_string(signer.private_pem, signer.kid), claims)
    return signed.decode("ascii")


@dataclass(frozen=True)
class _Response:
    status: int
    data: bytes


class CertTransport:
    """The transport google-auth calls to fetch signing certificates. Serves a fixed key set."""

    def __init__(self, *signers: SigningKey) -> None:
        body = {signer.kid: signer.certificate_pem.decode("ascii") for signer in signers}
        self._body = json.dumps(body).encode("utf-8")
        self.status = 200
        self.fetches = 0

    def __call__(self, url: str, method: str = "GET", **_: Any) -> _Response:
        self.fetches += 1
        return _Response(status=self.status, data=self._body)


class MutableIdentities:
    """The identity map the broker reads on every request. Tests change it between requests."""

    def __init__(
        self,
        identities: Iterable[Identity] = IDENTITIES,
        *,
        access_mode: AccessMode = AccessMode.ALLOWLIST,
    ) -> None:
        self._by_subject = {identity.subject: identity for identity in identities}
        self.access_mode = access_mode

    def identity_for(self, subject: str) -> Identity | None:
        return self._by_subject.get(subject)

    def snapshot(self) -> IdentitySnapshot:
        return IdentitySnapshot(by_subject=dict(self._by_subject), access_mode=self.access_mode)

    def put(self, identity: Identity) -> None:
        self._by_subject[identity.subject] = identity

    def remove(self, subject: str) -> None:
        del self._by_subject[subject]


@dataclass
class AuthWorld(World):
    identities: MutableIdentities
    transport: CertTransport


def make_auth_world(
    identities: MutableIdentities | None = None, *, transport: CertTransport | None = None
) -> AuthWorld:
    """A broker wired to the production BearerIdentity adapter, with in-memory storage."""
    identity_map = identities or MutableIdentities()
    cert_transport = transport or CertTransport(GOOGLE_SIGNER)
    bearer = BearerIdentity(
        audience=AUDIENCE, directory=identity_map, transport=cert_transport
    )
    clock = ManualClock()
    store = InMemoryRunStore()
    objects = InMemoryObjectStore()
    launcher = RecordingLauncher()
    ports = replace(
        make_ports(store=store, objects=objects, launcher=launcher, clock=clock),
        owner_of=bearer.owner_of,
        is_controller=bearer.is_controller,
        is_operator=bearer.is_operator,
    )
    app = create_app(policy=make_policy(api_audience=AUDIENCE, identities=IDENTITIES), ports=ports)
    return AuthWorld(
        client=TestClient(app),
        clock=clock,
        store=store,
        objects=objects,
        launcher=launcher,
        admissions=ports.admissions,
        identities=identity_map,
        transport=cert_transport,
    )


def bearer(token: str) -> dict[str, str]:
    return {"Authorization": f"Bearer {token}"}


def create_as(
    world: World, headers: dict[str, str], **overrides: Any
) -> tuple[str, dict[str, Any]]:
    """Create a run with the given headers. Returns (run_id, create payload)."""
    create = create_payload(**overrides)
    created = world.client.post("/v1/runs", json=create, headers=headers)
    assert created.status_code == 201, created.text
    return created.json()["run"]["run_id"], create


def admit_and_start_as(
    world: World, headers: dict[str, str], **overrides: Any
) -> tuple[str, dict[str, Any]]:
    """Create, upload the source, and start with the given headers. Returns (run_id, create)."""
    run_id, create = create_as(world, headers, **overrides)
    name = object_name(ObjectKind.INPUT, run_id=run_id)
    world.objects.write(name, SOURCE_TAR)
    started = world.client.post(
        f"/v1/runs/{run_id}:start",
        json=start_payload(create, generation=world.objects.generation_of(name)),
        headers=headers,
    )
    assert started.status_code == 200, started.text
    return run_id, create


def reconcile_as(world: World, run_id: str, headers: dict[str, str]) -> Any:
    return world.client.post(f"/internal/v1/runs/{run_id}:reconcile", headers=headers)


# -- shared owner-route inventory and caller headers ---------------------------------

NO_AUTH: dict[str, str] = {}
START_BODY = start_payload(create_payload(), generation=1)
CANCEL_BODY = {"protocol_version": "v1"}
RETRY_BODY = {
    "protocol_version": "v1",
    "request_uuid": str(uuid7(EPOCH)),
    "expected_attempt": 1,
    "expected_version": 0,
}

Call = Callable[[Any, str, dict[str, str]], Any]

# Every owner route, called as (client, run_id, headers). Each one is owner-gated.
OWNER_ROUTES: dict[str, Call] = {
    "get run": lambda client, run_id, headers: client.get(f"/v1/runs/{run_id}", headers=headers),
    "get status": lambda client, run_id, headers: client.get(
        f"/v1/runs/{run_id}/status", headers=headers
    ),
    "get logs": lambda client, run_id, headers: client.get(
        f"/v1/runs/{run_id}/logs", headers=headers
    ),
    "get result": lambda client, run_id, headers: client.get(
        f"/v1/runs/{run_id}/result", headers=headers
    ),
    "start": lambda client, run_id, headers: client.post(
        f"/v1/runs/{run_id}:start", json=START_BODY, headers=headers
    ),
    "cancel": lambda client, run_id, headers: client.post(
        f"/v1/runs/{run_id}:cancel", json=CANCEL_BODY, headers=headers
    ),
    "retry": lambda client, run_id, headers: client.post(
        f"/v1/runs/{run_id}:retry", json=RETRY_BODY, headers=headers
    ),
}


def as_owner_a() -> dict[str, str]:
    return bearer(mint())


def as_owner_b() -> dict[str, str]:
    return bearer(mint(subject=SUBJECT_OWNER_B, email=EMAIL_OWNER_B))


def as_operator() -> dict[str, str]:
    return bearer(mint(subject=SUBJECT_OPERATOR, email=EMAIL_OPERATOR))


def as_scheduler() -> dict[str, str]:
    return bearer(mint(subject=SUBJECT_SCHEDULER, email=EMAIL_SCHEDULER))
