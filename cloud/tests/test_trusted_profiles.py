"""Trusted profiles: the two worker sizes, and the RAM and size limits they must hold.

DPES-811 AC6. The accounting is arithmetic over declared sizes; real container memory enforcement
is DPES-804. The 2/8 profile does not hold the 8 GiB workspace plus 1 GiB tmp. That is reported
as a finding here, not adjusted so the test passes.
"""

from mao_cloud import trusted_profiles
from mao_cloud.protocol import MAX_RESULT_BYTES, MAX_SOURCE_BYTES
from mao_cloud.trusted_bundle import BundleLimits
from mao_cloud.trusted_profiles import (
    RESULT_LIMIT_BYTES,
    SMALL,
    SOURCE_LIMIT_BYTES,
    STANDARD,
    TMP_GIB,
    WORKSPACE_GIB,
    accounting,
)

MiB = 1024 * 1024


def test_the_two_profiles_are_two_and_four_vcpus_with_eight_and_sixteen_gib() -> None:
    assert (SMALL.name, SMALL.vcpus, SMALL.memory_gib) == ("2/8", 2, 8)
    assert (STANDARD.name, STANDARD.vcpus, STANDARD.memory_gib) == ("4/16", 4, 16)


def test_the_workspace_is_eight_gib_and_tmp_is_one_gib() -> None:
    assert WORKSPACE_GIB == 8
    assert TMP_GIB == 1


def test_the_standard_profile_holds_the_workspace_and_tmp() -> None:
    result = accounting(STANDARD)
    assert result.needed_gib == 9
    assert result.fits is True
    assert result.shortfall_gib == 0


def test_the_small_profile_does_not_hold_the_workspace_and_tmp_as_declared() -> None:
    """Finding: 8 GiB workspace plus 1 GiB tmp is 9 GiB, and the 2/8 profile has 8 GiB."""
    result = accounting(SMALL)
    assert result.needed_gib == 9
    assert result.fits is False
    assert result.shortfall_gib == 1


def test_the_source_limit_is_the_stricter_of_the_brief_and_the_protocol() -> None:
    assert SOURCE_LIMIT_BYTES == min(MAX_SOURCE_BYTES, 200 * MiB) == 200 * MiB


def test_the_bundle_intake_uses_the_same_source_limit() -> None:
    assert BundleLimits().max_bundle_bytes == SOURCE_LIMIT_BYTES


def test_the_result_document_limit_is_the_protocol_one_mebibyte() -> None:
    """The result document keeps the protocol's 1 MiB cap (DPES-897 decision 4)."""
    assert RESULT_LIMIT_BYTES == MAX_RESULT_BYTES == MiB


def test_the_returned_artifact_limit_is_200_mib_not_the_document_limit() -> None:
    """Returned work (patch or bundle) gets its own 200 MiB cap; the document stays at 1 MiB."""
    assert trusted_profiles.RESULT_ARTIFACT_LIMIT_BYTES == 200 * MiB
    assert trusted_profiles.RESULT_ARTIFACT_LIMIT_BYTES != RESULT_LIMIT_BYTES
