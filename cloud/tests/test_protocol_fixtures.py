"""Protocol v1 JSON fixtures: the success sequence and refused requests, validated through the models.

Fixtures live under tests/fixtures/protocol-v1. Validation uses the pydantic models that generate the
committed schemas and the same parsers the server runs. No separate JSON Schema validator is used
because jsonschema is not a cloud dependency. Refused create fixtures keep the original
canonical_digest on purpose: the screen runs before the digest check, so each refusal code shows
which check refused the request.
"""

import json
from pathlib import Path
from typing import Any

import pytest

from mao_cloud.protocol import (
    AcceptedReceipt,
    CreateRunResponse,
    ErrorCode,
    ProtocolError,
    WorkerBootstrap,
    parse_create_request,
    parse_document,
    parse_start_request,
    projection_bytes,
    projection_digest,
)

FIXTURES = Path(__file__).parent / "fixtures" / "protocol-v1"
BLOCKING_FIELDS = ("durable_root", "checkpoint_ref", "continuation_budget")

REFUSALS: list[tuple[str, str, ErrorCode]] = [
    ("reject/create-changed-manifest.json", "create", ErrorCode.DIGEST_MISMATCH),
    ("reject/create-unknown-version.json", "create", ErrorCode.UNSUPPORTED_VERSION),
    ("reject/create-local-path.json", "create", ErrorCode.CLIENT_CONTROLLED_FIELD),
    ("reject/create-client-actor.json", "create", ErrorCode.CLIENT_CONTROLLED_FIELD),
    ("reject/create-signed-url.json", "create", ErrorCode.CLIENT_CONTROLLED_FIELD),
    ("reject/create-credential.json", "create", ErrorCode.CLIENT_CONTROLLED_FIELD),
    ("reject/start-client-attempt.json", "start", ErrorCode.CLIENT_CONTROLLED_FIELD),
    ("reject/start-client-fence.json", "start", ErrorCode.CLIENT_CONTROLLED_FIELD),
    ("reject/start-client-claim.json", "start", ErrorCode.CLIENT_CONTROLLED_FIELD),
]
PARSERS = {"create": parse_create_request, "start": parse_start_request}


def _load(relative: str) -> Any:
    return json.loads((FIXTURES / relative).read_text(encoding="utf-8"))


def test_success_sequence_validates_through_the_models() -> None:
    create = parse_create_request(_load("accept/create-request.json"))
    # The create response carries no protocol_version, so it is a plain response model, not a document.
    created = CreateRunResponse.model_validate(_load("accept/create-response.json"))
    start = parse_start_request(_load("accept/start-request.json"))
    receipt = parse_document(AcceptedReceipt, _load("accept/accepted-receipt.json"))
    bootstrap = parse_document(WorkerBootstrap, _load("accept/worker-bootstrap.json"))

    assert created.run.run_id == receipt.run_id
    assert created.upload is not None
    assert created.upload.object_name == receipt.source.object_name
    assert start.canonical_digest == create.canonical_digest
    assert start.source_upload.sha256 == create.source_upload.sha256
    assert start.source_upload.size_bytes == create.source_upload.size_bytes
    assert start.source_upload.object_generation == receipt.source.generation
    assert receipt.source.base_sha == create.source.base_sha
    assert receipt.run_id == bootstrap.run_id
    assert receipt.attempt == bootstrap.attempt


def test_worker_bootstrap_matches_the_accepted_receipt_byte_for_byte() -> None:
    receipt = parse_document(AcceptedReceipt, _load("accept/accepted-receipt.json"))
    bootstrap = parse_document(WorkerBootstrap, _load("accept/worker-bootstrap.json"))

    assert projection_bytes(bootstrap) == projection_bytes(receipt)
    assert projection_digest(bootstrap) == receipt.projection_digest


def test_worker_bootstrap_fixture_carries_no_blocking_fields() -> None:
    document = _load("accept/worker-bootstrap.json")

    assert not set(BLOCKING_FIELDS) & set(document)


@pytest.mark.parametrize(
    ("name", "kind", "code"), REFUSALS, ids=[case[0] for case in REFUSALS]
)
def test_refused_fixture_is_refused_with_its_typed_code(
    name: str, kind: str, code: ErrorCode
) -> None:
    with pytest.raises(ProtocolError) as raised:
        PARSERS[kind](_load(name))

    assert raised.value.code == code


def test_every_refusal_fixture_on_disk_is_exercised() -> None:
    on_disk = sorted(
        p.relative_to(FIXTURES).as_posix() for p in (FIXTURES / "reject").glob("*.json")
    )

    assert on_disk == sorted(name for name, _, _ in REFUSALS)
