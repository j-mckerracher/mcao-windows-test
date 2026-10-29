"""Client-controlled input screening: what a request may carry, and what a stored document may carry.

Redaction rules and their idempotency are covered in test_objects.py. These tests cover the screen
that runs before strict validation, and the difference between requests (refused) and documents
(credential text redacted, identity fields accepted).
"""

from typing import Any

import pytest

from fakes import CLAIM_ID, EPOCH, IMAGE_DIGEST, RUN_ID, create_payload, start_payload, worker_bootstrap
from mao_cloud.protocol import (
    CLIENT_CONTROLLED_FIELDS,
    DOCUMENT_FORBIDDEN_FIELDS,
    FIELD_ORIGINS,
    IDENTITY_FIELDS,
    ActionRequest,
    CreateRunRequest,
    ErrorCode,
    Origin,
    ProtocolError,
    RetryRequest,
    StartRunRequest,
    StatusReport,
    WorkerBootstrap,
    parse_create_request,
    parse_document,
    parse_start_request,
)

CREDENTIAL_SAMPLES = [
    "Bearer abc.def-123",
    "password=hunter2",
    "ya29.a0AfH6SMBx_test-token",
    "https://storage.invalid/obj?X-Goog-Signature=abcd1234",
]
LOCAL_PATH_SAMPLES = [
    "/etc/passwd",
    "~/.ssh/id_rsa",
    "file:///var/tmp/source.tar",
    "C:\\Users\\dev\\repo",
    "c:/work/repo",
    "C:Users/dev/repo",
    "C:../outside/secret",
    "\\\\server\\share\\src",
    "\\relative-looking-unc",
    "..\\outside",
    "../outside/secret",
    "..",
]
RELATIVE_SAMPLES = ["wf/805/relative", "..notes/805", "DPES-805/C-drive-notes", "a/../b"]


def _status_dict() -> dict[str, Any]:
    report = StatusReport(
        protocol_version="v1",
        run_id=RUN_ID,
        attempt=1,
        fence=1,
        claim_id=CLAIM_ID,
        seq=1,
        phase="running",
        observed_at=EPOCH,
        log_watermark=0,
        message="ok",
    )
    return report.model_dump(mode="json")


# --- Requests: a well-formed request passes -----------------------------------------------------


def test_a_well_formed_create_request_passes_the_screen() -> None:
    assert isinstance(parse_create_request(create_payload()), CreateRunRequest)


def test_a_well_formed_start_request_passes_the_screen() -> None:
    start = start_payload(create_payload(), generation=1)
    assert isinstance(parse_start_request(start), StartRunRequest)


# --- Requests: client-controlled keys are refused at the top level and nested -------------------


@pytest.mark.parametrize("key", sorted(CLIENT_CONTROLLED_FIELDS))
def test_request_refuses_every_client_controlled_top_level_key(key: str) -> None:
    payload = {**create_payload(), key: "client-supplied"}
    with pytest.raises(ProtocolError) as caught:
        parse_create_request(payload)
    assert caught.value.code is ErrorCode.CLIENT_CONTROLLED_FIELD
    assert caught.value.detail == {"field": key}


def test_start_request_refuses_a_client_controlled_key() -> None:
    start = {**start_payload(create_payload(), generation=1), "owner_id": "owner-b"}
    with pytest.raises(ProtocolError) as caught:
        parse_start_request(start)
    assert caught.value.code is ErrorCode.CLIENT_CONTROLLED_FIELD


def test_request_refuses_a_client_controlled_key_nested_in_a_section() -> None:
    payload = create_payload()
    payload["source"]["token"] = "abc"
    with pytest.raises(ProtocolError) as caught:
        parse_create_request(payload)
    assert caught.value.code is ErrorCode.CLIENT_CONTROLLED_FIELD
    assert caught.value.detail == {"field": "source.token"}


def test_request_refuses_a_client_controlled_key_inside_a_list() -> None:
    payload = create_payload()
    payload["target"]["aliases"] = [{"ok": "x"}, {"secret": "x"}]
    with pytest.raises(ProtocolError) as caught:
        parse_create_request(payload)
    assert caught.value.code is ErrorCode.CLIENT_CONTROLLED_FIELD
    assert caught.value.detail == {"field": "target.aliases[1].secret"}


# --- Requests: credential material and local paths in string values ----------------------------


@pytest.mark.parametrize("secret", CREDENTIAL_SAMPLES)
def test_request_refuses_credential_material_in_a_string(secret: str) -> None:
    payload = create_payload()
    payload["assignment"]["workflow_correlation"] = secret
    with pytest.raises(ProtocolError) as caught:
        parse_create_request(payload)
    assert caught.value.code is ErrorCode.CLIENT_CONTROLLED_FIELD
    assert caught.value.detail == {"field": "assignment.workflow_correlation"}
    assert "credential material" in caught.value.message


@pytest.mark.parametrize("path", LOCAL_PATH_SAMPLES)
def test_request_refuses_a_local_path_in_a_string(path: str) -> None:
    payload = create_payload()
    payload["assignment"]["workflow_correlation"] = path
    with pytest.raises(ProtocolError) as caught:
        parse_create_request(payload)
    assert caught.value.code is ErrorCode.CLIENT_CONTROLLED_FIELD
    assert "local paths" in caught.value.message


@pytest.mark.parametrize("relative", RELATIVE_SAMPLES)
def test_a_relative_path_shaped_string_is_not_a_local_path(relative: str) -> None:
    payload = create_payload(assignment={"task_id": "DPES-805", "workflow_correlation": relative})
    assert isinstance(parse_create_request(payload), CreateRunRequest)


# --- Requests: ordering of the gates -------------------------------------------------------------


def test_a_missing_version_is_refused_before_the_screen_runs() -> None:
    payload = {**create_payload(), "owner": "x"}
    del payload["protocol_version"]
    with pytest.raises(ProtocolError) as caught:
        parse_create_request(payload)
    assert caught.value.code is ErrorCode.INVALID_REQUEST


def test_an_unsupported_version_is_refused_before_the_screen_runs() -> None:
    payload = {**create_payload(), "owner": "x", "protocol_version": "v9"}
    with pytest.raises(ProtocolError) as caught:
        parse_create_request(payload)
    assert caught.value.code is ErrorCode.UNSUPPORTED_VERSION


def test_the_screen_runs_before_strict_validation() -> None:
    payload = {**create_payload(), "owner": "x", "surprise": True}
    with pytest.raises(ProtocolError) as caught:
        parse_create_request(payload)
    assert caught.value.code is ErrorCode.CLIENT_CONTROLLED_FIELD


def test_an_unknown_field_alone_is_unknown_not_client_controlled() -> None:
    payload = {**create_payload(), "surprise": True}
    with pytest.raises(ProtocolError) as caught:
        parse_create_request(payload)
    assert caught.value.code is ErrorCode.UNKNOWN_FIELD


def test_a_tampered_canonical_digest_is_refused_after_the_screen() -> None:
    payload = {**create_payload(), "canonical_digest": "0" * 64}
    with pytest.raises(ProtocolError) as caught:
        parse_create_request(payload)
    assert caught.value.code is ErrorCode.DIGEST_MISMATCH


@pytest.mark.parametrize("payload", [[], "create", 7])
def test_a_non_object_request_is_invalid(payload: Any) -> None:
    with pytest.raises(ProtocolError) as caught:
        parse_create_request(payload)
    assert caught.value.code is ErrorCode.INVALID_REQUEST


# --- Documents: identity fields are accepted; the other client-controlled names are not ---------


def test_identity_fields_are_screened_from_requests_but_not_from_documents() -> None:
    assert IDENTITY_FIELDS <= CLIENT_CONTROLLED_FIELDS
    assert not IDENTITY_FIELDS & DOCUMENT_FORBIDDEN_FIELDS


# --- Field origins: the Python classification that the Origin docstring describes -------------


@pytest.mark.parametrize("model", [CreateRunRequest, StartRunRequest, RetryRequest, ActionRequest])
def test_no_request_field_is_a_name_the_screen_refuses(model: type) -> None:
    # A request field named like a client-controlled key could never be set by a client.
    assert not set(model.model_fields) & CLIENT_CONTROLLED_FIELDS


@pytest.mark.parametrize("model", [CreateRunRequest, StartRunRequest, RetryRequest])
def test_every_request_field_is_client_manifest_origin(model: type) -> None:
    assert set(FIELD_ORIGINS[model.__name__].values()) == {Origin.CLIENT_MANIFEST}


def test_identity_fields_are_never_client_origin() -> None:
    for name, origins in FIELD_ORIGINS.items():
        for field in IDENTITY_FIELDS & origins.keys():
            assert origins[field] is not Origin.CLIENT_MANIFEST, f"{name}.{field}"


def test_a_worker_bootstrap_document_passes_with_its_identity_fields() -> None:
    payload = worker_bootstrap().model_dump(mode="json")
    assert {"run_id", "attempt", "fence", "claim_id"} <= payload.keys()
    assert parse_document(WorkerBootstrap, payload) == worker_bootstrap()


@pytest.mark.parametrize("key", sorted(DOCUMENT_FORBIDDEN_FIELDS))
def test_document_refuses_every_forbidden_top_level_key(key: str) -> None:
    payload = {**worker_bootstrap().model_dump(mode="json"), key: "x"}
    with pytest.raises(ProtocolError) as caught:
        parse_document(WorkerBootstrap, payload)
    assert caught.value.code is ErrorCode.CLIENT_CONTROLLED_FIELD
    assert caught.value.detail == {"field": key}


def test_document_permits_a_forbidden_name_nested_in_a_pinned_tuple() -> None:
    payload = worker_bootstrap().model_dump(mode="json")
    assert payload["compatibility"]["image_digest"] == IMAGE_DIGEST
    assert isinstance(parse_document(WorkerBootstrap, payload), WorkerBootstrap)


# --- Documents: text is redacted, never rejected -----------------------------------------------


def test_a_document_redacts_credential_text_instead_of_refusing_it() -> None:
    payload = {**_status_dict(), "message": "Bearer abc.def-123"}
    assert parse_document(StatusReport, payload).message == "Bearer [REDACTED]"


def test_a_document_keeps_local_path_text_as_written() -> None:
    payload = {**_status_dict(), "message": "/var/log/app.log"}
    assert parse_document(StatusReport, payload).message == "/var/log/app.log"


def test_document_version_gate_applies() -> None:
    payload = _status_dict()
    del payload["protocol_version"]
    with pytest.raises(ProtocolError) as missing:
        parse_document(StatusReport, payload)
    assert missing.value.code is ErrorCode.INVALID_REQUEST

    with pytest.raises(ProtocolError) as unsupported:
        parse_document(StatusReport, {**_status_dict(), "protocol_version": "v9"})
    assert unsupported.value.code is ErrorCode.UNSUPPORTED_VERSION


def test_document_unknown_top_level_field_is_unknown() -> None:
    payload = {**_status_dict(), "surprise": True}
    with pytest.raises(ProtocolError) as caught:
        parse_document(StatusReport, payload)
    assert caught.value.code is ErrorCode.UNKNOWN_FIELD


def test_a_non_object_document_is_invalid() -> None:
    with pytest.raises(ProtocolError) as caught:
        parse_document(StatusReport, [])
    assert caught.value.code is ErrorCode.INVALID_REQUEST
