"""Signed-object naming, capability rules, artifact checks, redaction, and canonical bytes."""

import hashlib
from datetime import timedelta
from typing import Any

import pytest
from pydantic import ValidationError

from fakes import CLAIM_ID, DEFAULT_MAX_RUNTIME_SECONDS, EPOCH, RUN_ID, refusal_code
from mao_cloud.protocol import (
    CAPABILITIES,
    ArtifactRef,
    ErrorCode,
    LogEntry,
    ObjectKind,
    RunResultDocument,
    StatusReport,
    artifact_object_name,
    canonical_bytes,
    capability_expiry,
    object_name,
    redact,
    required_headers,
    sha256_hex,
    verify_artifacts,
    verify_sha256,
)

SHA_OF_ABC = hashlib.sha256(b"abc").hexdigest()
JWT = (
    "eyJhbGciOiJIUzI1NiJ9.eyJzdWIiOiIxMjM0NTY3ODkwIn0."  # gitleaks:allow - synthetic JWT-shaped fixture, not a credential
    "dozjgNryP4J3jVmNHl0w5N_XgL0n3I9PlFUP0THsR8U"
)


def _result(*artifacts: ArtifactRef) -> RunResultDocument:
    return RunResultDocument(
        protocol_version="v1",
        run_id=RUN_ID,
        attempt=1,
        fence=1,
        claim_id=CLAIM_ID,
        outcome="succeeded",
        exit_code=0,
        commit_sha=None,
        artifacts=artifacts,
    )


# --- Object names -------------------------------------------------------------------------------


def test_input_object_is_run_scoped_and_needs_no_attempt() -> None:
    assert object_name(ObjectKind.INPUT, run_id=RUN_ID) == f"runs/{RUN_ID}/input/source.tar"


@pytest.mark.parametrize(
    ("kind", "leaf"),
    [
        (ObjectKind.CLAIM, "claim.json"),
        (ObjectKind.CONTROL, "control.json"),
        (ObjectKind.INTAKE, "intake.json"),
        (ObjectKind.CANCEL, "cancel.json"),
        (ObjectKind.STATUS, "status.json"),
        (ObjectKind.RESULT, "result.json"),
    ],
)
def test_attempt_scoped_singletons_use_fixed_leaf_names(kind: ObjectKind, leaf: str) -> None:
    assert object_name(kind, run_id=RUN_ID, attempt=1) == f"runs/{RUN_ID}/attempts/000001/{leaf}"


def test_checkpoint_objects_are_never_named_by_sequence() -> None:
    with pytest.raises(ValueError, match="checkpoint_id"):
        object_name(ObjectKind.CHECKPOINT, run_id=RUN_ID, attempt=1, seq=3)


def test_log_object_is_sequenced_and_zero_padded() -> None:
    assert (
        object_name(ObjectKind.LOG, run_id=RUN_ID, attempt=1, seq=12)
        == f"runs/{RUN_ID}/attempts/000001/logs/0000000012.ndjson"
    )


def test_attempt_number_is_zero_padded_to_six_digits() -> None:
    assert object_name(ObjectKind.CLAIM, run_id=RUN_ID, attempt=12).endswith(
        "/attempts/000012/claim.json"
    )


def test_attempt_scoped_object_without_an_attempt_is_refused() -> None:
    with pytest.raises(ValueError, match="attempt"):
        object_name(ObjectKind.CLAIM, run_id=RUN_ID)


def test_attempt_zero_is_refused() -> None:
    with pytest.raises(ValueError, match="attempt"):
        object_name(ObjectKind.RESULT, run_id=RUN_ID, attempt=0)


@pytest.mark.parametrize("kind", [ObjectKind.LOG])
def test_sequenced_object_without_a_seq_is_refused(kind: ObjectKind) -> None:
    with pytest.raises(ValueError, match="seq"):
        object_name(kind, run_id=RUN_ID, attempt=1)


def test_malformed_run_id_is_refused() -> None:
    with pytest.raises(ValueError, match="run_id"):
        object_name(ObjectKind.INPUT, run_id="run_XYZ")


def test_artifact_object_is_scoped_to_its_attempt() -> None:
    assert (
        artifact_object_name(RUN_ID, 2, "report.txt")
        == f"runs/{RUN_ID}/attempts/000002/artifacts/report.txt"
    )


def test_artifact_object_refuses_attempt_zero() -> None:
    with pytest.raises(ValueError):
        artifact_object_name(RUN_ID, 0, "report.txt")


# --- Capability table ---------------------------------------------------------------------------


def test_only_the_checkpoint_write_is_blocked_and_it_names_its_ticket() -> None:
    blocked = {key: cap.blocking for key, cap in CAPABILITIES.items() if cap.blocking is not None}
    assert blocked == {(ObjectKind.CHECKPOINT, "PUT"): "DPES-869"}


def test_only_the_input_upload_is_held_by_the_client() -> None:
    client_held = {key for key, cap in CAPABILITIES.items() if cap.holder == "client"}
    assert client_held == {(ObjectKind.INPUT, "PUT")}


@pytest.mark.parametrize(
    ("key", "precondition"),
    [
        ((ObjectKind.INPUT, "PUT"), "create_only"),
        ((ObjectKind.INPUT, "GET"), "none"),
        ((ObjectKind.CLAIM, "PUT"), "create_only"),
        ((ObjectKind.CONTROL, "GET"), "none"),
        ((ObjectKind.INTAKE, "GET"), "none"),
        ((ObjectKind.STATUS, "PUT"), "generation_match"),
        ((ObjectKind.CHECKPOINT, "PUT"), "create_only"),
        ((ObjectKind.LOG, "PUT"), "create_only"),
        ((ObjectKind.RESULT, "PUT"), "create_only"),
    ],
)
def test_each_capability_carries_its_precondition(
    key: tuple[ObjectKind, str], precondition: str
) -> None:
    assert CAPABILITIES[key].precondition == precondition


def test_every_upload_has_a_size_bound_and_a_content_type() -> None:
    for (kind, method), cap in CAPABILITIES.items():
        if method == "PUT":
            assert cap.max_bytes is not None and cap.max_bytes > 0, kind
            assert cap.content_type is not None, kind


def test_checksum_bound_uploads_are_exactly_input_checkpoint_and_result() -> None:
    bound = {key for key, cap in CAPABILITIES.items() if cap.sha256_metadata}
    assert bound == {
        (ObjectKind.INPUT, "PUT"),
        (ObjectKind.CHECKPOINT, "PUT"),
        (ObjectKind.RESULT, "PUT"),
    }


# --- Capability expiry --------------------------------------------------------------------------


def test_capability_expires_after_its_holder_ttl() -> None:
    expiry = capability_expiry(
        ObjectKind.CLAIM,
        "PUT",
        now=EPOCH,
        attempt_deadline=EPOCH + timedelta(seconds=DEFAULT_MAX_RUNTIME_SECONDS),
    )
    assert expiry == EPOCH + timedelta(seconds=1200)


def test_capability_expiry_is_clamped_to_the_attempt_deadline() -> None:
    expiry = capability_expiry(
        ObjectKind.STATUS,
        "PUT",
        now=EPOCH,
        attempt_deadline=EPOCH + timedelta(seconds=7200),
    )
    assert expiry == EPOCH + timedelta(seconds=7200)


def test_no_capability_is_issued_after_the_attempt_deadline() -> None:
    code = refusal_code(
        lambda: capability_expiry(
            ObjectKind.LOG,
            "PUT",
            now=EPOCH + timedelta(seconds=DEFAULT_MAX_RUNTIME_SECONDS),
            attempt_deadline=EPOCH + timedelta(seconds=DEFAULT_MAX_RUNTIME_SECONDS),
        )
    )
    assert code is ErrorCode.DEADLINE_EXCEEDED


# --- Required headers ---------------------------------------------------------------------------


def test_create_only_claim_upload_requires_the_zero_generation_precondition() -> None:
    assert required_headers(ObjectKind.CLAIM, "PUT") == {
        "content-type": "application/json",
        "x-goog-if-generation-match": "0",
    }


def test_status_upload_requires_the_expected_generation() -> None:
    headers = required_headers(ObjectKind.STATUS, "PUT", expected_generation=7)
    assert headers == {
        "content-type": "application/json",
        "x-goog-if-generation-match": "7",
    }


def test_status_upload_without_an_expected_generation_is_refused() -> None:
    with pytest.raises(ValueError, match="expected_generation"):
        required_headers(ObjectKind.STATUS, "PUT")


def test_result_upload_binds_its_sha256_metadata() -> None:
    headers = required_headers(ObjectKind.RESULT, "PUT", sha256_hex_value=SHA_OF_ABC)
    assert headers["x-goog-meta-sha256"] == SHA_OF_ABC
    assert headers["x-goog-if-generation-match"] == "0"


def test_checksum_bound_upload_without_a_checksum_is_refused() -> None:
    with pytest.raises(ValueError, match="sha256"):
        required_headers(ObjectKind.RESULT, "PUT")


def test_input_upload_requires_a_checksum_and_the_tar_content_type() -> None:
    headers = required_headers(ObjectKind.INPUT, "PUT", sha256_hex_value=SHA_OF_ABC)
    assert headers == {
        "content-type": "application/x-tar",
        "x-goog-if-generation-match": "0",
        "x-goog-meta-sha256": SHA_OF_ABC,
    }


def test_reads_carry_no_precondition_or_content_type() -> None:
    assert required_headers(ObjectKind.INPUT, "GET") == {}
    assert required_headers(ObjectKind.CONTROL, "GET") == {}
    assert required_headers(ObjectKind.INTAKE, "GET") == {}


# --- Artifact verification ----------------------------------------------------------------------


def _artifact(name: str = "report.txt", data: bytes = b"abc") -> ArtifactRef:
    return ArtifactRef(name=name, sha256=sha256_hex(data), size_bytes=len(data))


def test_artifacts_that_match_their_references_verify() -> None:
    verify_artifacts(_result(_artifact()), {"report.txt": b"abc"}.get)


def test_a_result_with_no_artifacts_verifies_trivially() -> None:
    verify_artifacts(_result(), lambda _name: None)


def test_missing_artifact_is_a_checksum_mismatch() -> None:
    code = refusal_code(lambda: verify_artifacts(_result(_artifact()), lambda _name: None))
    assert code is ErrorCode.CHECKSUM_MISMATCH


def test_artifact_of_the_wrong_size_is_a_checksum_mismatch() -> None:
    code = refusal_code(
        lambda: verify_artifacts(_result(_artifact()), {"report.txt": b"abcd"}.get)
    )
    assert code is ErrorCode.CHECKSUM_MISMATCH


def test_artifact_with_the_right_size_and_wrong_bytes_is_a_checksum_mismatch() -> None:
    code = refusal_code(
        lambda: verify_artifacts(_result(_artifact()), {"report.txt": b"xyz"}.get)
    )
    assert code is ErrorCode.CHECKSUM_MISMATCH


# --- Artifact references ------------------------------------------------------------------------


@pytest.mark.parametrize("name", ["/etc/passwd", "../escape.txt", "a/../b.txt", ""])
def test_artifact_names_must_be_relative_and_traversal_free(name: str) -> None:
    with pytest.raises(ValidationError):
        ArtifactRef(name=name, sha256="a" * 64, size_bytes=1)


def test_nested_relative_artifact_name_is_accepted() -> None:
    assert ArtifactRef(name="reports/out.txt", sha256="a" * 64, size_bytes=1).name == "reports/out.txt"


def test_a_returned_artifact_of_200_mib_is_accepted() -> None:
    size = 200 * 1024 * 1024
    ref = ArtifactRef(name="work.patch", sha256="a" * 64, size_bytes=size)
    assert ref.size_bytes == size


def test_a_returned_artifact_over_200_mib_is_refused() -> None:
    size = 200 * 1024 * 1024 + 1
    with pytest.raises(ValidationError):
        ArtifactRef(name="work.patch", sha256="a" * 64, size_bytes=size)


# --- Checksums and canonical bytes --------------------------------------------------------------


def test_sha256_hex_matches_the_standard_digest() -> None:
    assert sha256_hex(b"abc") == SHA_OF_ABC


def test_expected_checksum_is_compared_case_insensitively() -> None:
    verify_sha256(b"abc", SHA_OF_ABC.upper())


def test_checksum_mismatch_is_refused_with_its_code() -> None:
    code = refusal_code(lambda: verify_sha256(b"abd", SHA_OF_ABC))
    assert code is ErrorCode.CHECKSUM_MISMATCH


def test_canonical_bytes_do_not_depend_on_key_order() -> None:
    assert canonical_bytes({"b": 1, "a": 2}) == canonical_bytes({"a": 2, "b": 1})


def test_canonical_bytes_are_compact_and_keep_non_ascii_as_utf8() -> None:
    assert canonical_bytes({"k": "é"}) == '{"k":"é"}'.encode()


def test_canonical_bytes_refuse_nan() -> None:
    with pytest.raises(ValueError):
        canonical_bytes({"x": float("nan")})


# --- Redaction ----------------------------------------------------------------------------------

REDACTION_SAMPLES: list[tuple[str, str]] = [
    (
        "https://storage.invalid/obj?X-Goog-Signature=abcd1234",
        "[REDACTED:signed-url]",
    ),
    (
        "-----BEGIN RSA PRIVATE KEY-----\nMIIBOgIBAAJBAKj\n-----END RSA PRIVATE KEY-----",
        "[REDACTED:private-key]",
    ),
    ("Authorization: Bearer abc.def-123", "Bearer [REDACTED]"),
    ("ya29.a0AfH6SMBx_test-token", "[REDACTED:oauth-token]"),
    (JWT, "[REDACTED:jwt]"),
    ("password=hunter2", "password=[REDACTED]"),
    ("api_key: sk-123", "api_key: [REDACTED]"),
]


@pytest.mark.parametrize(("sample", "expected"), REDACTION_SAMPLES, ids=[
    "signed-url", "private-key", "bearer", "oauth", "jwt", "password", "api-key",
])
def test_each_redaction_rule_replaces_its_secret(sample: str, expected: str) -> None:
    result = redact(sample)
    assert expected in result.text
    assert result.count == 1


@pytest.mark.parametrize(("sample", "_expected"), REDACTION_SAMPLES, ids=[
    "signed-url", "private-key", "bearer", "oauth", "jwt", "password", "api-key",
])
def test_redaction_is_idempotent(sample: str, _expected: str) -> None:
    once = redact(sample).text
    assert redact(once).text == once


def test_clean_text_passes_through_untouched() -> None:
    result = redact("all tests passed in 3.2s")
    assert result.text == "all tests passed in 3.2s"
    assert result.count == 0


def test_every_secret_in_one_text_is_counted() -> None:
    result = redact("token=a1 then ya29.abc and password: hunter2")
    assert result.count == 3
    assert "a1" not in result.text
    assert "abc" not in result.text
    assert "hunter2" not in result.text


def test_log_entry_lines_are_redacted_at_the_model_boundary() -> None:
    entry = LogEntry(protocol_version="v1", seq=1, lines=("token=abc123", "plain"))
    assert entry.lines == ("token=[REDACTED]", "plain")


def test_log_line_over_the_limit_is_refused() -> None:
    with pytest.raises(ValidationError, match="protocol limit"):
        LogEntry(protocol_version="v1", seq=1, lines=("x" * 4097,))


def test_log_line_exactly_at_the_limit_is_accepted() -> None:
    entry = LogEntry(protocol_version="v1", seq=1, lines=("x" * 4096,))
    assert len(entry.lines[0]) == 4096


def test_status_message_is_redacted_at_the_model_boundary() -> None:
    status: Any = StatusReport(
        protocol_version="v1",
        run_id=RUN_ID,
        attempt=1,
        fence=1,
        claim_id=CLAIM_ID,
        seq=1,
        phase="running",
        observed_at=EPOCH,
        log_watermark=0,
        message="Bearer abc.def-123",
    )
    assert "abc.def" not in status.message
    assert "[REDACTED]" in status.message
