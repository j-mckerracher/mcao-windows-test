"""Server policy: file loading, document shape, server invariants, and manifest admission."""

import json
from pathlib import Path
from typing import Any

import pytest

from fakes import IMAGE_DIGEST, PROFILE_ID, create_payload, make_policy, policy_document
from mao_cloud.config import Policy, PolicyError, load_policy, policy_from_text
from mao_cloud.protocol import (
    MAX_ATTEMPTS,
    STARTUP_TIMEOUT_SECONDS,
    CreateRunManifest,
    ErrorCode,
    ProtocolError,
    ToolCompatibility,
)


def _document(**changes: Any) -> dict[str, Any]:
    document = policy_document()
    document.update(changes)
    return document


def _load(document: dict[str, Any]) -> Policy:
    return policy_from_text(json.dumps(document))


def _manifest() -> CreateRunManifest:
    payload = {key: value for key, value in create_payload().items() if key != "canonical_digest"}
    return CreateRunManifest.model_validate(payload)


def _other_compatibility(profile_id: str = "pi-other") -> ToolCompatibility:
    return ToolCompatibility(
        image_digest=IMAGE_DIGEST,
        profile_id=profile_id,
        toolchain_manifest_digest="1" * 64,
        instruction_digest="2" * 64,
    )


def _refusal(policy: Policy, manifest: CreateRunManifest) -> ProtocolError:
    with pytest.raises(ProtocolError) as caught:
        policy.admit_manifest(manifest)
    assert caught.value.code is ErrorCode.NOT_ALLOWED
    return caught.value


# --- Loading ------------------------------------------------------------------------------------


def test_the_policy_document_reads_back_into_the_same_policy() -> None:
    assert _load(policy_document()) == make_policy()


def test_load_policy_reads_a_file(tmp_path: Path) -> None:
    path = tmp_path / "policy.json"
    path.write_text(json.dumps(policy_document()), encoding="utf-8")
    assert load_policy(path) == make_policy()


def test_a_missing_policy_file_is_a_policy_error(tmp_path: Path) -> None:
    with pytest.raises(PolicyError, match="cannot read policy file"):
        load_policy(tmp_path / "absent.json")


def test_a_policy_file_that_is_not_utf8_is_a_policy_error(tmp_path: Path) -> None:
    path = tmp_path / "policy.json"
    path.write_bytes(b"\xff\xfe{")
    with pytest.raises(PolicyError, match="cannot read policy file"):
        load_policy(path)


def test_omitted_timing_fields_take_the_protocol_defaults() -> None:
    document = _document()
    document.pop("startup_timeout_seconds", None)
    document.pop("max_attempts", None)
    policy = _load(document)
    assert policy.startup_timeout_seconds == STARTUP_TIMEOUT_SECONDS
    assert policy.max_attempts == MAX_ATTEMPTS


# --- Document shape -----------------------------------------------------------------------------


def test_invalid_json_is_a_policy_error() -> None:
    with pytest.raises(PolicyError, match="not valid"):
        policy_from_text("{not json")


def test_a_non_object_document_is_a_policy_error() -> None:
    with pytest.raises(PolicyError, match="not valid"):
        policy_from_text("[]")


def test_an_unknown_key_is_a_policy_error() -> None:
    with pytest.raises(PolicyError, match="not valid"):
        _load(_document(surprise=True))


def test_a_missing_required_key_is_a_policy_error() -> None:
    document = _document()
    del document["server_id"]
    with pytest.raises(PolicyError, match="not valid"):
        _load(document)


@pytest.mark.parametrize(
    ("field", "value"),
    [
        ("stale_after_seconds", 0),
        ("skew_seconds", -1),
        ("startup_timeout_seconds", 0),
        ("max_attempts", 0),
        ("max_attempts", MAX_ATTEMPTS + 1),
    ],
)
def test_timing_bounds_are_enforced(field: str, value: int) -> None:
    with pytest.raises(PolicyError, match="not valid"):
        _load(_document(**{field: value}))


@pytest.mark.parametrize(
    ("field", "value"),
    [
        ("skew_seconds", 0),
        ("startup_timeout_seconds", 1),
        ("max_attempts", 1),
        ("max_attempts", MAX_ATTEMPTS),
    ],
)
def test_timing_bounds_accept_their_edges(field: str, value: int) -> None:
    policy = _load(_document(**{field: value}))
    assert getattr(policy, field) == value


# --- Server invariants --------------------------------------------------------------------------


@pytest.mark.parametrize("server_id", ["bad id!", "-leading", "x" * 65])
def test_server_id_must_match_the_protocol_pattern(server_id: str) -> None:
    with pytest.raises(PolicyError, match="server_id does not match"):
        _load(_document(server_id=server_id))


@pytest.mark.parametrize("server_id", ["a", "x" * 64])
def test_server_id_accepts_the_pattern_edges(server_id: str) -> None:
    assert _load(_document(server_id=server_id)).server_id == server_id


@pytest.mark.parametrize("name", ["targets", "harnesses", "models", "thinking", "profiles"])
def test_every_allowlist_must_name_at_least_one_entry(name: str) -> None:
    empty: Any = {} if name == "profiles" else []
    with pytest.raises(PolicyError, match="allowlist must not be empty"):
        _load(_document(**{name: empty}))


@pytest.mark.parametrize(
    ("name", "value"),
    [("harnesses", ["not-a-harness"]), ("models", ["not-a-model"]), ("thinking", ["extreme"])],
)
def test_a_policy_cannot_extend_the_protocol_enumerations(name: str, value: list[str]) -> None:
    with pytest.raises(PolicyError, match="outside the protocol"):
        _load(_document(**{name: value}))


@pytest.mark.parametrize("target", ["A", "ab", "cloud_pilot"])
def test_target_ids_must_match_the_protocol_pattern(target: str) -> None:
    with pytest.raises(PolicyError, match="does not match the protocol pattern"):
        _load(_document(targets=[target]))


def test_a_profile_key_must_match_the_profile_pattern() -> None:
    profiles = {"Bad_Profile": policy_document()["profiles"][PROFILE_ID]}
    with pytest.raises(PolicyError, match="does not match the protocol pattern"):
        _load(_document(profiles=profiles))


def test_a_profile_must_pin_its_own_id() -> None:
    profiles = {"pi-renamed": policy_document()["profiles"][PROFILE_ID]}
    with pytest.raises(PolicyError, match="pins a different profile_id"):
        _load(_document(profiles=profiles))


# --- Manifest admission -------------------------------------------------------------------------


def test_an_allowlisted_manifest_returns_its_pinned_tuple() -> None:
    policy = make_policy()
    assert policy.admit_manifest(_manifest()) == policy.profiles[PROFILE_ID]


def test_a_target_outside_the_allowlist_is_refused() -> None:
    policy = make_policy(targets=frozenset({"other-pilot"}))
    assert _refusal(policy, _manifest()).message == "cloud target is not on the server allowlist"


def test_a_thinking_level_outside_the_allowlist_is_refused() -> None:
    policy = make_policy(thinking=frozenset({"low"}))
    assert _refusal(policy, _manifest()).message == "thinking level is not on the server allowlist"


def test_a_toolchain_profile_outside_the_allowlist_is_refused() -> None:
    policy = make_policy(profiles={"pi-other": _other_compatibility()})
    assert _refusal(policy, _manifest()).message == "toolchain profile is not on the server allowlist"


@pytest.mark.parametrize(
    ("changes", "message"),
    [
        ({"harness": "other-harness"}, "harness is not on the server allowlist"),
        ({"model": "other-model"}, "model is not on the server allowlist"),
    ],
)
def test_harness_and_model_are_checked_against_the_allowlist(
    changes: dict[str, str], message: str
) -> None:
    # The protocol enumerations admit one value each, so a validated manifest can never carry
    # these values. model_copy builds the copy the allowlist must still refuse.
    manifest = _manifest().model_copy(update=changes)
    assert _refusal(make_policy(), manifest).message == message


def test_the_target_is_checked_before_the_harness() -> None:
    policy = make_policy(targets=frozenset({"other-pilot"}))
    manifest = _manifest().model_copy(update={"harness": "other-harness"})
    assert _refusal(policy, manifest).message == "cloud target is not on the server allowlist"
