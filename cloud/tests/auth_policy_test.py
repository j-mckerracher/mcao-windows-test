"""Policy file: the identity map, audience, and caps parse into the server's own policy.

Anything malformed refuses at startup. A partly-valid identity map is never loaded.
"""

import json
from typing import Any

import pytest

from auth_support import (
    AUDIENCE,
    EMAIL_OPERATOR,
    EMAIL_OWNER_A,
    IDENTITIES,
    SUBJECT_OWNER_A,
)
from fakes import OWNER_A, create_payload, make_policy, policy_document
from mao_cloud.broker import PolicyIdentities
from mao_cloud.config import (
    AccessMode,
    Identity,
    PolicyError,
    Role,
    is_mayo_email,
    policy_from_text,
)
from mao_cloud.protocol import (
    MAX_RUNTIME_SECONDS,
    MAX_SOURCE_BYTES,
    ErrorCode,
    ProtocolError,
    manifest_from_request,
    parse_create_request,
)
from mao_cloud.trusted_profiles import SOURCE_LIMIT_BYTES


def identity_document(identity: Identity) -> dict[str, Any]:
    return {
        "subject": identity.subject,
        "email": identity.email,
        "role": identity.role.value,
        "owner_id": identity.owner_id,
    }


def document(**overrides: Any) -> str:
    """The JSON policy file as text, with the identity map and audience set."""
    values: dict[str, Any] = {
        **policy_document(),
        "api_audience": AUDIENCE,
        "identities": [identity_document(identity) for identity in IDENTITIES],
    }
    values.update(overrides)
    return json.dumps(values)


def test_identities_parse_into_the_policy_identity_map() -> None:
    policy = policy_from_text(document())

    assert policy.api_audience == AUDIENCE
    assert policy.identities == IDENTITIES


def test_identities_require_an_api_audience_in_the_file() -> None:
    with pytest.raises(PolicyError):
        policy_from_text(document(api_audience=None))


@pytest.mark.parametrize(
    "audience",
    [
        "http://mao.example.test",
        "https://mao.example.test/",
        "https://mao.example.test/v1",
        "https://MAO.example.test",
        "mao.example.test",
    ],
)
def test_api_audience_must_be_a_bare_https_origin(audience: str) -> None:
    with pytest.raises(PolicyError):
        policy_from_text(document(api_audience=audience))


@pytest.mark.parametrize("subject", ["", "owner-a", "12a", "1" * 31])
def test_subject_must_be_the_numeric_google_account_id(subject: str) -> None:
    with pytest.raises(PolicyError):
        Identity(subject=subject, email=EMAIL_OWNER_A, role=Role.OWNER, owner_id=OWNER_A)


@pytest.mark.parametrize(
    "email",
    [
        "Invoker@mao-test.iam.gserviceaccount.com",
        "no-at-sign.iam.gserviceaccount.com",
        "invoker@localhost",
        "in voker@mao-test.iam.gserviceaccount.com",
    ],
)
def test_email_must_be_a_lowercase_well_formed_address(email: str) -> None:
    with pytest.raises(PolicyError):
        Identity(subject=SUBJECT_OWNER_A, email=email, role=Role.OWNER, owner_id=OWNER_A)


@pytest.mark.parametrize("owner_id", [None, "Owner-A", "a", "owner_a", "9owner"])
def test_an_owner_needs_a_protocol_owner_id(owner_id: str | None) -> None:
    with pytest.raises(PolicyError):
        Identity(subject=SUBJECT_OWNER_A, email=EMAIL_OWNER_A, role=Role.OWNER, owner_id=owner_id)


@pytest.mark.parametrize("role", [Role.OPERATOR, Role.SCHEDULER])
def test_only_an_owner_may_carry_an_owner_id(role: Role) -> None:
    with pytest.raises(PolicyError):
        Identity(subject=SUBJECT_OWNER_A, email=EMAIL_OPERATOR, role=role, owner_id=OWNER_A)


def test_duplicate_subject_is_refused() -> None:
    duplicate = IDENTITIES[0]

    with pytest.raises(PolicyError):
        make_policy(api_audience=AUDIENCE, identities=(duplicate, duplicate))


def test_duplicate_email_under_a_different_subject_is_refused() -> None:
    first = IDENTITIES[0]
    other_subject = Identity(
        subject="104455667788990099", email=first.email, role=Role.OWNER, owner_id="owner-c"
    )

    with pytest.raises(PolicyError):
        make_policy(api_audience=AUDIENCE, identities=(first, other_subject))


def test_identities_are_refused_without_an_audience_in_the_policy_object() -> None:
    with pytest.raises(PolicyError):
        make_policy(identities=IDENTITIES)


def test_unknown_identity_fields_are_refused_in_the_file() -> None:
    entry = {**identity_document(IDENTITIES[0]), "token": "not-a-field"}

    with pytest.raises(PolicyError):
        policy_from_text(document(identities=[entry]))


@pytest.mark.parametrize(
    "override",
    [
        {"max_source_bytes": 0},
        {"max_source_bytes": MAX_SOURCE_BYTES + 1},
        {"max_source_bytes": SOURCE_LIMIT_BYTES + 1},
        {"max_runtime_seconds": 0},
        {"max_runtime_seconds": MAX_RUNTIME_SECONDS + 1},
    ],
)
def test_caps_must_stay_within_the_protocol_maximums(override: dict[str, int]) -> None:
    with pytest.raises(PolicyError):
        make_policy(**override)


def test_the_default_source_cap_is_the_size_the_worker_intake_accepts() -> None:
    """DPES-811 AC6. A source admitted by the controller must be one the worker will extract."""
    assert make_policy().max_source_bytes == SOURCE_LIMIT_BYTES


@pytest.mark.parametrize(
    ("size_bytes", "admitted"),
    [(SOURCE_LIMIT_BYTES, True), (SOURCE_LIMIT_BYTES + 1, False)],
)
def test_default_admission_refuses_the_source_the_worker_would_refuse(
    size_bytes: int, admitted: bool
) -> None:
    policy = make_policy()
    manifest = manifest_from_request(parse_create_request(create_payload()))
    sized = manifest.model_copy(
        update={
            "source_upload": manifest.source_upload.model_copy(update={"size_bytes": size_bytes})
        }
    )

    if admitted:
        policy.admit_manifest(sized)
        return
    with pytest.raises(ProtocolError) as refused:
        policy.admit_manifest(sized)
    assert refused.value.code is ErrorCode.NOT_ALLOWED
    assert "source size" in refused.value.message


def test_source_above_the_server_cap_is_refused_before_a_run_is_created() -> None:
    policy = make_policy(max_source_bytes=1024)
    manifest = manifest_from_request(parse_create_request(create_payload()))

    with pytest.raises(ProtocolError) as refused:
        policy.admit_manifest(manifest)

    assert refused.value.code is ErrorCode.NOT_ALLOWED
    assert "source size" in refused.value.message


def test_runtime_above_the_server_cap_is_refused_before_a_run_is_created() -> None:
    policy = make_policy(max_runtime_seconds=300)
    manifest = manifest_from_request(parse_create_request(create_payload()))

    with pytest.raises(ProtocolError) as refused:
        policy.admit_manifest(manifest)

    assert refused.value.code is ErrorCode.NOT_ALLOWED
    assert "runtime budget" in refused.value.message


def test_manifest_within_every_cap_is_admitted() -> None:
    policy = make_policy()
    manifest = manifest_from_request(parse_create_request(create_payload()))

    compatibility = policy.admit_manifest(manifest)

    assert compatibility.profile_id == manifest.toolchain_profile


def test_policy_identities_resolve_a_subject_to_its_declared_identity() -> None:
    directory = PolicyIdentities(make_policy(api_audience=AUDIENCE, identities=IDENTITIES))

    assert directory.identity_for(SUBJECT_OWNER_A) == IDENTITIES[0]
    assert directory.identity_for("999999999999999999") is None


def test_access_mode_defaults_to_allowlist_when_the_file_omits_it() -> None:
    assert policy_from_text(document()).access_mode is AccessMode.ALLOWLIST


def test_mayo_wide_parses_from_the_file() -> None:
    policy = policy_from_text(document(access_mode="mayo-wide"))

    assert policy.access_mode is AccessMode.MAYO_WIDE


@pytest.mark.parametrize("value", ["open", "MAYO-WIDE", "", None])
def test_an_unknown_access_mode_refuses_startup(value: str | None) -> None:
    with pytest.raises(PolicyError):
        policy_from_text(document(access_mode=value))


def test_mayo_wide_refuses_startup_from_a_file_without_an_api_audience() -> None:
    # No identities here: the identities-need-audience rule must not be the one refusing.
    with pytest.raises(PolicyError):
        policy_from_text(document(access_mode="mayo-wide", api_audience=None, identities=[]))


def test_mayo_wide_refuses_a_policy_object_without_an_api_audience() -> None:
    with pytest.raises(PolicyError):
        make_policy(access_mode=AccessMode.MAYO_WIDE)


@pytest.mark.parametrize("email", ["jane.doe@mayo.edu", "a+b@mayo.edu"])
def test_is_mayo_email_admits_the_exact_mayo_domain(email: str) -> None:
    assert is_mayo_email(email) is True


@pytest.mark.parametrize(
    "email",
    [
        "jane.doe@mayo.edu.evil.test",
        "jane.doe@evilmayo.edu",
        "jane.doe@cdh.mayo.edu",
        "jane.doe@mayo.edu.",
        "jane.doe@mayo.edux",
        "jane.doe@MAYO.EDU",
        "jane@evil.test@mayo.edu",
        "@mayo.edu",
        "jane.doe",
        "",
    ],
)
def test_is_mayo_email_refuses_every_other_address(email: str) -> None:
    assert is_mayo_email(email) is False
