"""The signer's HTTP entrypoint over SignerService (DPES-899 AC1 and AC2).

The real app runs in-process through TestClient, over an in-memory run store, an in-memory signing
backend, and Google's test key set. Nothing reaches a network, a credential, or an emulator, so live
behaviour stays unverified. Every refusal is checked for its exact body: the code and its fixed
message, and no run data.
"""

import asyncio
import json
from datetime import timedelta
from typing import Any

import httpx
import pytest
from fastapi.testclient import TestClient

from auth_support import (
    AUDIENCE,
    GOOGLE_SIGNER,
    ROGUE_SIGNER,
    UNPUBLISHED_SIGNER,
    CertTransport,
    bearer,
    mint,
)
from fakes import (
    CLAIM_ID,
    EPOCH,
    OWNER_A,
    OWNER_B,
    RUN_ID,
    InMemorySigningBackend,
    run_record,
)
from mao_cloud import protocol, signer_serve
from mao_cloud.protocol import CAPABILITIES, ObjectKind, RunRecord, RunState, sha256_hex
from mao_cloud.serve import EXIT_CONFIG, EXIT_UNAVAILABLE
from mao_cloud.signer import (
    CapabilityRequest,
    RefusalCode,
    SignerRefused,
    SignerService,
)
from mao_cloud.signer_client import HttpSignerClient
from mao_cloud.signer_serve import (
    IdTokenCaller,
    MAX_REQUEST_BYTES,
    SignerSdk,
    build_service,
    create_app,
    load_config,
    main,
)
from mao_cloud.signer_wire import (
    CAPABILITY_PATH,
    issued_body,
    issued_from,
    refusal_body,
    refusal_code_from,
    request_body,
    status_for,
)

CONTROLLER_EMAIL = "mcao-cloud-controller@mao-test.iam.gserviceaccount.com"
CONTROLLER_SUBJECT = "104455667788990066"
WORKER_EMAIL = "mcao-cloud-worker@mao-test.iam.gserviceaccount.com"
NOW = EPOCH + timedelta(seconds=60)
ATTEMPT = f"runs/{RUN_ID}/attempts/000001"
CHECKPOINT_ID = "ck-0001"
SHA = sha256_hex(b"checksum bound payload")
NAMES: dict[ObjectKind, str] = {
    ObjectKind.INPUT: f"runs/{RUN_ID}/input/source.tar",
    ObjectKind.CLAIM: f"{ATTEMPT}/claim.json",
    ObjectKind.CONTROL: f"{ATTEMPT}/control.json",
    ObjectKind.INTAKE: f"{ATTEMPT}/intake.json",
    ObjectKind.CANCEL: f"{ATTEMPT}/cancel.json",
    ObjectKind.STATUS: f"{ATTEMPT}/status.json",
    ObjectKind.CHECKPOINT: f"{ATTEMPT}/checkpoints/{CHECKPOINT_ID}/manifest.json",
    ObjectKind.LOG: f"{ATTEMPT}/logs/0000000001.ndjson",
    ObjectKind.RESULT: f"{ATTEMPT}/result.json",
}


class FakeRuns:
    def __init__(self, *records: RunRecord) -> None:
        self._records = {record.run_id: record for record in records}

    def get_run(self, run_id: str) -> RunRecord | None:
        return self._records.get(run_id)


class FailingRuns:
    def get_run(self, run_id: str) -> RunRecord | None:
        raise RuntimeError(f"firestore detail for {run_id}")


class FailingBackend:
    def sign(self, name: str, **_: Any) -> str:
        raise RuntimeError(f"iam detail for {name}")


def make_client(
    *records: RunRecord,
    runs: Any = None,
    backend: Any = None,
    now: Any = NOW,
) -> tuple[TestClient, Any]:
    signing = InMemorySigningBackend() if backend is None else backend
    service = SignerService(
        runs=FakeRuns(*records) if runs is None else runs,
        backend=signing,
        controller_identity=CONTROLLER_EMAIL,
        now=lambda: now,
    )
    app = create_app(
        service=service,
        caller=IdTokenCaller(audience=AUDIENCE, transport=CertTransport(GOOGLE_SIGNER)),
    )
    return TestClient(app), signing


def request_for(
    record: RunRecord, kind: ObjectKind, method: str = "PUT", **fields: Any
) -> CapabilityRequest:
    values: dict[str, Any] = {
        "owner_id": record.owner_id,
        "run_id": record.run_id,
        "kind": kind,
        "method": method,
        "object_name": NAMES[kind],
        "expires_at": NOW + timedelta(seconds=1),
    }
    if kind is not ObjectKind.INPUT:
        values.update(
            attempt=record.attempt, fence=record.fence, claim_id=record.claim_id
        )
    if kind is ObjectKind.LOG:
        values["seq"] = 1
    if kind is ObjectKind.CHECKPOINT:
        values["checkpoint_id"] = CHECKPOINT_ID
    capability = CAPABILITIES.get((kind, method))
    if capability is not None:
        # A request carries the preconditions its capability demands, so a test that wants a
        # refusal overrides exactly one of them.
        if capability.precondition == "generation_match":
            values["expected_generation"] = 1
        if capability.sha256_metadata:
            values["sha256_hex"] = (
                record.manifest.source_upload.sha256
                if kind is ObjectKind.INPUT
                else SHA
            )
    values.update(fields)
    return CapabilityRequest(**values)


def controller_token(**overrides: Any) -> str:
    values: dict[str, Any] = {"email": CONTROLLER_EMAIL, "subject": CONTROLLER_SUBJECT}
    values.update(overrides)
    return mint(**values)


def post(client: TestClient, body: Any, token: str | None = None) -> Any:
    headers = bearer(controller_token() if token is None else token)
    if isinstance(body, bytes):
        return client.post(CAPABILITY_PATH, content=body, headers=headers)
    return client.post(CAPABILITY_PATH, json=body, headers=headers)


def assert_refused(response: Any, code: RefusalCode) -> None:
    assert response.status_code == status_for(code), response.text
    assert response.json() == {
        "refusal": code.value,
        "message": str(SignerRefused(code)),
    }


def running(**overrides: Any) -> RunRecord:
    return run_record(state=RunState.RUNNING, claim_id=CLAIM_ID, **overrides)


# ---------------------------------------------------------------------------
# A verified controller request is signed; the answer is the wire shape
# ---------------------------------------------------------------------------


def test_a_verified_controller_request_is_answered_with_the_signed_capability() -> None:
    record = running()
    client, signing = make_client(record)
    request = request_for(record, ObjectKind.STATUS, "PUT")

    response = post(client, request_body(request))

    assert response.status_code == 200
    assert set(response.json()) == {
        "object_name",
        "method",
        "url",
        "headers",
        "expires_at",
    }
    issued = issued_from(response.json())
    assert issued.object_name == NAMES[ObjectKind.STATUS]
    assert issued.url.startswith("https://storage.invalid/")
    assert [call[:2] for call in signing.calls] == [(NAMES[ObjectKind.STATUS], "PUT")]


def test_the_intake_capability_is_signed_for_a_live_run() -> None:
    record = running()
    client, signing = make_client(record)

    response = post(client, request_body(request_for(record, ObjectKind.INTAKE, "GET")))

    assert response.status_code == 200
    assert issued_from(response.json()).object_name == NAMES[ObjectKind.INTAKE]
    assert [call[:2] for call in signing.calls] == [(NAMES[ObjectKind.INTAKE], "GET")]


# ---------------------------------------------------------------------------
# Every granted (kind, method) is signed for an accepted run, or refused with its own code
# ---------------------------------------------------------------------------


def _state_for(key: tuple[ObjectKind, str]) -> RunState:
    if key == (ObjectKind.INPUT, "PUT"):
        return RunState.AWAITING_UPLOAD
    if key == (ObjectKind.CLAIM, "PUT"):
        return RunState.DISPATCHING
    return RunState.RUNNING


@pytest.mark.parametrize(
    "key", list(CAPABILITIES), ids=lambda k: f"{k[0].value}-{k[1]}"
)
def test_every_granted_capability_carries_the_preconditions_the_protocol_requires(
    key: tuple[ObjectKind, str],
) -> None:
    kind, method = key
    capability = CAPABILITIES[key]
    record = run_record(state=_state_for(key), claim_id=CLAIM_ID)
    request = request_for(record, kind, method)
    client, signing = make_client(record)

    response = post(client, request_body(request))

    if capability.blocking is not None:
        assert_refused(response, RefusalCode.BLOCKED)
        assert signing.calls == []
        return
    assert response.status_code == 200, response.text
    issued = issued_from(response.json())
    assert issued.object_name == NAMES[kind]
    assert issued.headers == protocol.required_headers(
        kind,
        method,
        sha256_hex_value=request.sha256_hex,
        expected_generation=request.expected_generation,
    )


# ---------------------------------------------------------------------------
# Refusals: forged, stale and cross-owner requests, each with its exact body
# ---------------------------------------------------------------------------


def test_a_forged_attempt_ahead_of_the_accepted_run_is_refused() -> None:
    record = running()
    client, signing = make_client(record)
    request = request_for(record, ObjectKind.STATUS, attempt=record.attempt + 1)

    response = post(client, request_body(request))

    assert_refused(response, RefusalCode.FORGED_IDENTITY)
    assert signing.calls == []


def test_a_forged_fence_ahead_of_the_accepted_run_is_refused() -> None:
    record = running()
    client, signing = make_client(record)

    response = post(
        client, request_body(request_for(record, ObjectKind.RESULT, fence=99))
    )

    assert_refused(response, RefusalCode.FORGED_IDENTITY)
    assert signing.calls == []


def test_a_forged_claim_id_is_refused() -> None:
    record = running()
    client, signing = make_client(record)

    response = post(
        client,
        request_body(
            request_for(record, ObjectKind.STATUS, claim_id="claim_" + "c" * 32)
        ),
    )

    assert_refused(response, RefusalCode.FORGED_IDENTITY)
    assert signing.calls == []


def test_a_stale_attempt_is_refused_once_the_run_has_moved_on() -> None:
    record = running(attempt=2)
    client, signing = make_client(record)
    stale = request_for(record, ObjectKind.STATUS, attempt=1)

    response = post(client, request_body(stale))

    assert_refused(response, RefusalCode.STALE_ATTEMPT)
    assert signing.calls == []


def test_a_stale_fence_is_refused_once_the_run_has_moved_on() -> None:
    record = running(fence=2)
    client, signing = make_client(record)
    stale = request_for(record, ObjectKind.CONTROL, "GET", fence=1)

    response = post(client, request_body(stale))

    assert_refused(response, RefusalCode.STALE_FENCE)
    assert signing.calls == []


def test_a_request_for_another_owners_run_is_refused_as_not_found() -> None:
    record = running()
    client, signing = make_client(record)
    foreign = request_for(record, ObjectKind.STATUS, owner_id=OWNER_B)

    response = post(client, request_body(foreign))

    assert_refused(response, RefusalCode.NOT_FOUND)
    assert signing.calls == []


def test_a_foreign_owner_and_an_unknown_run_get_the_same_refusal_body() -> None:
    # Identical bodies mean a caller cannot probe which run ids exist or who owns them.
    record = running()
    client, _ = make_client(record)
    foreign = post(
        client, request_body(request_for(record, ObjectKind.STATUS, owner_id=OWNER_B))
    )
    unknown = post(
        client,
        request_body(
            request_for(
                record, ObjectKind.STATUS, run_id="run_" + "f" * 32, owner_id=OWNER_A
            )
        ),
    )

    assert foreign.status_code == unknown.status_code
    assert foreign.json() == unknown.json()


def test_a_capability_for_another_attempts_object_is_refused_as_object_mismatch() -> (
    None
):
    record = running()
    client, signing = make_client(record)
    other = f"runs/{RUN_ID}/attempts/000002/status.json"

    response = post(
        client, request_body(request_for(record, ObjectKind.STATUS, object_name=other))
    )

    assert_refused(response, RefusalCode.OBJECT_MISMATCH)
    assert signing.calls == []


def test_a_claim_signed_for_a_run_not_yet_dispatching_is_refused_by_state() -> None:
    record = running()
    client, signing = make_client(record)

    response = post(client, request_body(request_for(record, ObjectKind.CLAIM, "PUT")))

    assert_refused(response, RefusalCode.RUN_STATE_REFUSED)
    assert signing.calls == []


def test_an_input_upload_whose_checksum_is_not_the_manifest_checksum_is_refused() -> (
    None
):
    record = run_record(state=RunState.AWAITING_UPLOAD, claim_id=CLAIM_ID)
    client, signing = make_client(record)
    wrong = sha256_hex(b"a different tarball")

    response = post(
        client, request_body(request_for(record, ObjectKind.INPUT, sha256_hex=wrong))
    )

    assert_refused(response, RefusalCode.PRECONDITION_REFUSED)
    assert signing.calls == []


def test_an_expiry_beyond_the_allowed_window_is_refused() -> None:
    record = running()
    client, signing = make_client(record)
    too_late = request_for(
        record, ObjectKind.STATUS, expires_at=NOW + timedelta(days=2)
    )

    response = post(client, request_body(too_late))

    assert_refused(response, RefusalCode.EXPIRY_REFUSED)
    assert signing.calls == []


def test_a_request_after_the_attempt_deadline_is_refused() -> None:
    record = running()
    past_deadline = record.deadline_at + timedelta(seconds=1)
    client, signing = make_client(record, now=past_deadline)

    response = post(
        client,
        request_body(
            request_for(
                record,
                ObjectKind.STATUS,
                expires_at=past_deadline + timedelta(seconds=1),
            )
        ),
    )

    assert_refused(response, RefusalCode.DEADLINE_PASSED)
    assert signing.calls == []


def test_the_checkpoint_write_stays_blocked_until_dpes_869_lands() -> None:
    record = running()
    client, signing = make_client(record)

    response = post(
        client, request_body(request_for(record, ObjectKind.CHECKPOINT, "PUT"))
    )

    assert_refused(response, RefusalCode.BLOCKED)
    assert signing.calls == []


# ---------------------------------------------------------------------------
# Authentication: no credential, bad credential, or a caller that is not the controller
# ---------------------------------------------------------------------------


def test_a_request_without_a_credential_is_refused_unauthenticated() -> None:
    record = running()
    client, signing = make_client(record)

    response = client.post(
        CAPABILITY_PATH, json=request_body(request_for(record, ObjectKind.STATUS))
    )

    assert_refused(response, RefusalCode.UNAUTHENTICATED)
    assert signing.calls == []


@pytest.mark.parametrize(
    "token",
    [
        pytest.param(
            mint(ROGUE_SIGNER, email=CONTROLLER_EMAIL), id="same-kid-other-key"
        ),
        pytest.param(
            mint(UNPUBLISHED_SIGNER, email=CONTROLLER_EMAIL), id="unpublished-key"
        ),
        pytest.param(
            mint(email=CONTROLLER_EMAIL, audience="https://elsewhere.test"),
            id="audience",
        ),
        pytest.param(
            mint(email=CONTROLLER_EMAIL, email_verified=False), id="unverified-email"
        ),
        pytest.param(
            mint(email=CONTROLLER_EMAIL, email_verified=None), id="no-email-claim"
        ),
    ],
)
def test_a_credential_that_does_not_verify_is_refused_unauthenticated(
    token: str,
) -> None:
    record = running()
    client, signing = make_client(record)

    response = post(
        client, request_body(request_for(record, ObjectKind.STATUS)), token=token
    )

    assert_refused(response, RefusalCode.UNAUTHENTICATED)
    assert signing.calls == []


def test_a_verified_caller_that_is_not_the_controller_is_refused_unauthenticated() -> (
    None
):
    record = running()
    client, signing = make_client(record)
    worker = mint(email=WORKER_EMAIL, subject="104455667788990077")

    response = post(
        client, request_body(request_for(record, ObjectKind.STATUS)), token=worker
    )

    assert_refused(response, RefusalCode.UNAUTHENTICATED)
    assert signing.calls == []


# ---------------------------------------------------------------------------
# Malformed requests and transport failures refuse with fixed bodies
# ---------------------------------------------------------------------------


def test_a_body_that_is_not_json_is_refused_as_malformed() -> None:
    client, signing = make_client(running())

    response = post(client, b"not json at all")

    assert_refused(response, RefusalCode.MALFORMED_REQUEST)
    assert signing.calls == []


def test_a_body_with_an_unknown_field_is_refused_as_malformed() -> None:
    record = running()
    client, signing = make_client(record)
    body = {**request_body(request_for(record, ObjectKind.STATUS)), "extra": "x"}

    response = post(client, body)

    assert_refused(response, RefusalCode.MALFORMED_REQUEST)
    assert signing.calls == []


def test_a_naive_expiry_is_refused_as_malformed() -> None:
    record = running()
    client, signing = make_client(record)
    body = request_body(request_for(record, ObjectKind.STATUS))
    body["expires_at"] = "2026-10-21T12:01:00"

    response = post(client, body)

    assert_refused(response, RefusalCode.MALFORMED_REQUEST)
    assert signing.calls == []


def test_an_oversized_body_is_refused_before_it_is_parsed() -> None:
    client, signing = make_client(running())
    body = b'{"x": "' + b"a" * MAX_REQUEST_BYTES + b'"}'

    response = post(client, body)

    assert_refused(response, RefusalCode.MALFORMED_REQUEST)
    assert signing.calls == []


def _streamed_post(
    client: TestClient, chunks: list[bytes], *, token: str | None
) -> tuple[httpx.Response, int]:
    consumed = 0
    sent: list[dict[str, Any]] = []
    headers = {} if token is None else bearer(token)
    scope = {
        "type": "http",
        "asgi": {"version": "3.0"},
        "http_version": "1.1",
        "method": "POST",
        "scheme": "https",
        "path": CAPABILITY_PATH,
        "raw_path": CAPABILITY_PATH.encode(),
        "query_string": b"",
        "root_path": "",
        "headers": [
            (name.lower().encode(), value.encode()) for name, value in headers.items()
        ],
        "server": ("signer.test", 443),
        "client": ("127.0.0.1", 12345),
    }

    async def receive() -> dict[str, Any]:
        nonlocal consumed
        chunk = chunks[consumed]
        consumed += 1
        return {
            "type": "http.request", "body": chunk, "more_body": consumed < len(chunks)
        }

    async def send(message: dict[str, Any]) -> None:
        sent.append(message)

    async def request() -> None:
        await client.app(scope, receive, send)

    asyncio.run(request())
    status = next(
        message["status"] for message in sent if message["type"] == "http.response.start"
    )
    body = b"".join(message.get("body", b"") for message in sent)
    return httpx.Response(status, content=body), consumed


@pytest.mark.parametrize("authenticated", [False, True])
@pytest.mark.parametrize("first_chunk_size", [MAX_REQUEST_BYTES, MAX_REQUEST_BYTES * 8])
def test_streamed_oversized_bodies_stop_reading_and_keep_authentication_before_parsing(
    authenticated: bool, first_chunk_size: int
) -> None:
    client, signing = make_client(running())
    chunks = [b"x" * first_chunk_size, b"x", b"remaining input must not be consumed"]

    response, consumed = _streamed_post(
        client, chunks, token=controller_token() if authenticated else None
    )

    assert_refused(
        response,
        RefusalCode.MALFORMED_REQUEST if authenticated else RefusalCode.UNAUTHENTICATED,
    )
    assert consumed == (2 if first_chunk_size == MAX_REQUEST_BYTES else 1)
    assert signing.calls == []


def test_a_streamed_valid_request_exactly_at_the_body_limit_is_signed() -> None:
    record = running()
    client, signing = make_client(record)
    raw = json.dumps(request_body(request_for(record, ObjectKind.STATUS))).encode()
    raw += b" " * (MAX_REQUEST_BYTES - len(raw))

    response, consumed = _streamed_post(
        client, [raw[:100], raw[100:]], token=controller_token()
    )

    assert response.status_code == 200
    assert issued_from(response.json()).object_name == NAMES[ObjectKind.STATUS]
    assert consumed == 2
    assert len(signing.calls) == 1


def test_a_capability_the_protocol_does_not_grant_is_refused_unknown() -> None:
    assert (ObjectKind.CLAIM, "GET") not in CAPABILITIES
    record = running()
    client, signing = make_client(record)

    response = post(client, request_body(request_for(record, ObjectKind.CLAIM, "GET")))

    assert_refused(response, RefusalCode.UNKNOWN_CAPABILITY)
    assert signing.calls == []


def test_a_run_store_failure_is_refused_unavailable_without_its_detail() -> None:
    client, signing = make_client(runs=FailingRuns())

    response = post(client, request_body(request_for(running(), ObjectKind.STATUS)))

    assert_refused(response, RefusalCode.UNAVAILABLE)
    assert RUN_ID not in response.text
    assert "firestore detail" not in response.text
    assert signing.calls == []


def test_a_signing_failure_is_refused_unavailable_without_its_detail() -> None:
    client, _ = make_client(running(), backend=FailingBackend())

    response = post(client, request_body(request_for(running(), ObjectKind.STATUS)))

    assert_refused(response, RefusalCode.UNAVAILABLE)
    assert "iam detail" not in response.text
    assert RUN_ID not in response.text


# ---------------------------------------------------------------------------
# The controller's bearer reaches the application through the platform's invoker check
# ---------------------------------------------------------------------------


def cloud_run_front(headers: dict[str, str]) -> dict[str, str]:
    """The headers the application receives behind Cloud Run's invoker check. Modeled, not observed.

    The model follows the controller CLI's dual-header path (cli.invoker_headers). When
    X-Serverless-Authorization carries the IAM check, the platform consumes it and forwards
    Authorization intact. With Authorization alone, the platform takes that header for its own
    check, so the application receives no credential. Live platform behaviour is unverified.
    """
    consumed = (
        "X-Serverless-Authorization"
        if "X-Serverless-Authorization" in headers
        else "Authorization"
    )
    return {name: value for name, value in headers.items() if name != consumed}


def through_cloud_run(client: TestClient) -> Any:
    """A signer post for HttpSignerClient that sends its headers through the platform front."""

    def post(url: str, **kwargs: Any) -> Any:
        return client.post(
            CAPABILITY_PATH,
            json=kwargs["json"],
            headers=cloud_run_front(kwargs["headers"]),
        )

    return post


def test_the_controller_client_reaches_the_application_through_the_platform() -> None:
    record = running()
    client, signing = make_client(record)
    controller = HttpSignerClient(
        origin=AUDIENCE,
        post=through_cloud_run(client),
        fetch_token=lambda audience: controller_token(),
    )
    request = request_for(record, ObjectKind.STATUS)

    issued = controller.issue(request)

    assert issued.object_name == request.object_name
    assert len(signing.calls) == 1


def test_authorization_alone_never_reaches_the_application() -> None:
    record = running()
    client, signing = make_client(record)
    authorization_only = bearer(controller_token())

    response = client.post(
        CAPABILITY_PATH,
        json=request_body(request_for(record, ObjectKind.STATUS)),
        headers=cloud_run_front(authorization_only),
    )

    assert_refused(response, RefusalCode.UNAUTHENTICATED)
    assert signing.calls == []


# ---------------------------------------------------------------------------
# Wire shapes
# ---------------------------------------------------------------------------


@pytest.mark.parametrize("code", list(RefusalCode), ids=lambda c: c.value)
def test_every_refusal_body_is_the_code_and_its_fixed_message_and_nothing_else(
    code: RefusalCode,
) -> None:
    body = refusal_body(code)

    assert set(body) == {"refusal", "message"}
    assert body["refusal"] == code.value
    assert body["message"] == str(SignerRefused(code))
    assert status_for(code) in {400, 401, 403, 503}


def test_an_issued_capability_round_trips_through_the_wire_unchanged() -> None:
    record = running()
    client, _ = make_client(record)
    response = post(client, request_body(request_for(record, ObjectKind.STATUS)))
    issued = issued_from(response.json())

    again = issued_from(json.loads(json.dumps(issued_body(issued))))

    assert again == issued


@pytest.mark.parametrize(
    "mutate",
    [
        pytest.param(lambda b: b.pop("url"), id="missing-field"),
        pytest.param(lambda b: b.update(extra="x"), id="extra-field"),
        pytest.param(lambda b: b.update(headers=["x"]), id="headers-not-a-mapping"),
        pytest.param(lambda b: b.update(headers={"a": 1}), id="header-value-not-text"),
        pytest.param(
            lambda b: b.update(expires_at="2026-10-21T12:01:00"), id="naive-expiry"
        ),
        pytest.param(lambda b: b.update(url=None), id="url-not-text"),
    ],
)
def test_a_malformed_issued_answer_is_rejected(mutate: Any) -> None:
    record = running()
    client, _ = make_client(record)
    body = dict(
        post(client, request_body(request_for(record, ObjectKind.STATUS))).json()
    )
    mutate(body)

    with pytest.raises(ValueError):
        issued_from(body)


def test_a_refusal_answer_names_only_a_known_code() -> None:
    assert refusal_code_from({"refusal": "forged_identity", "message": "x"}) is (
        RefusalCode.FORGED_IDENTITY
    )
    assert refusal_code_from({"refusal": "not-a-code"}) is None
    assert refusal_code_from({}) is None
    assert refusal_code_from(None) is None


# ---------------------------------------------------------------------------
# Configuration and startup: each step refuses in order, before anything binds
# ---------------------------------------------------------------------------

COMPLETE_ENV = {
    "MAO_CLOUD_PROJECT": "mao-test",
    "MAO_CLOUD_OBJECT_BUCKET": "mao-test-objects",
    "MAO_CLOUD_SIGNER_AUDIENCE": AUDIENCE,
    "MAO_CLOUD_CONTROLLER_IDENTITY": CONTROLLER_EMAIL,
    "MAO_CLOUD_SIGNING_IDENTITY": "mcao-cloud-signer@mao-test.iam.gserviceaccount.com",
}


def test_load_config_reads_a_complete_environment_and_defaults_the_database() -> None:
    config = load_config(COMPLETE_ENV)

    assert config.project == "mao-test"
    assert config.firestore_database == "mcao-cloud"
    assert config.audience == AUDIENCE
    assert config.controller_identity == CONTROLLER_EMAIL


def test_load_config_names_every_missing_setting_and_echoes_no_value() -> None:
    with pytest.raises(signer_serve.SignerConfigError) as error:
        load_config({"MAO_CLOUD_PROJECT": "mao-test"})

    message = str(error.value)
    for name in (
        "MAO_CLOUD_OBJECT_BUCKET",
        "MAO_CLOUD_SIGNER_AUDIENCE",
        "MAO_CLOUD_CONTROLLER_IDENTITY",
        "MAO_CLOUD_SIGNING_IDENTITY",
    ):
        assert name in message
    assert "mao-test" not in message


@pytest.mark.parametrize(
    "name,value",
    [
        ("MAO_CLOUD_CONTROLLER_IDENTITY", "attacker@evil.example"),
        ("MAO_CLOUD_CONTROLLER_IDENTITY", "mcao@mao-test.iam.gserviceaccount.com/../x"),
        ("MAO_CLOUD_SIGNING_IDENTITY", "signer@example.com"),
        ("MAO_CLOUD_SIGNER_AUDIENCE", "http://signer.example.test"),
        ("MAO_CLOUD_SIGNER_AUDIENCE", "https://signer.example.test/path"),
        ("MAO_CLOUD_PROJECT", "Not A Project"),
    ],
)
def test_load_config_refuses_a_malformed_identity_audience_or_project(
    name: str, value: str
) -> None:
    with pytest.raises(signer_serve.SignerConfigError) as error:
        load_config({**COMPLETE_ENV, name: value})

    assert name in str(error.value)
    assert value not in str(error.value)


def test_a_bad_port_refuses_with_exit_2_before_configuration_is_read(
    capsys: pytest.CaptureFixture[str],
) -> None:
    served: list[Any] = []

    code = main(
        environ={"PORT": "70000"},
        make_service=lambda config: pytest.fail("no client may open"),
        serve=lambda *a, **k: served.append((a, k)),
    )

    assert code == EXIT_CONFIG
    assert served == []
    assert "PORT" in capsys.readouterr().err


def test_missing_configuration_refuses_with_exit_2_and_binds_nothing(
    capsys: pytest.CaptureFixture[str],
) -> None:
    served: list[Any] = []

    code = main(
        environ={},
        make_service=lambda config: pytest.fail("no client may open"),
        serve=lambda *a, **k: served.append((a, k)),
    )

    assert code == EXIT_CONFIG
    assert served == []
    assert "missing required configuration" in capsys.readouterr().err


def test_a_client_that_cannot_open_refuses_with_exit_3_without_its_detail(
    capsys: pytest.CaptureFixture[str],
) -> None:
    served: list[Any] = []

    def refuse_to_open(**_: Any) -> Any:
        raise RuntimeError("secret credential material")

    sdk = SignerSdk(
        firestore_client=refuse_to_open,
        signing_backend=lambda **_: InMemorySigningBackend(),
    )
    code = main(
        environ=COMPLETE_ENV,
        make_service=lambda config: build_service(config, sdk),
        serve=lambda *a, **k: served.append((a, k)),
    )

    assert code == EXIT_UNAVAILABLE
    assert served == []
    err = capsys.readouterr().err
    assert "RuntimeError" in err
    assert "secret credential material" not in err


def test_the_entrypoint_serves_the_verified_controller_on_all_interfaces_at_the_port() -> (
    None
):
    record = running()
    service = SignerService(
        runs=FakeRuns(record),
        backend=InMemorySigningBackend(),
        controller_identity=CONTROLLER_EMAIL,
        now=lambda: NOW,
    )
    served: list[Any] = []

    code = main(
        environ=COMPLETE_ENV,
        make_service=lambda config: service,
        serve=lambda app, **kwargs: served.append((app, kwargs)),
        transport=CertTransport(GOOGLE_SIGNER),
    )

    assert code == 0
    app, kwargs = served[0]
    assert kwargs == {"host": "0.0.0.0", "port": 8080}
    response = TestClient(app).post(
        CAPABILITY_PATH,
        json=request_body(request_for(record, ObjectKind.STATUS)),
        headers=bearer(controller_token()),
    )
    assert response.status_code == 200
