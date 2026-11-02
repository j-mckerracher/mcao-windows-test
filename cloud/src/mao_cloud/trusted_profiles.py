"""Trusted worker profiles: the two sizes, and the RAM and size limits they must hold.

DPES-811 AC6. The accounting is arithmetic over declared sizes. Real container memory enforcement
is DPES-804. Nothing here adjusts a limit to make a profile fit; a profile that cannot hold its
declared disks is reported as not fitting.
"""

from dataclasses import dataclass

from mao_cloud.protocol import (
    MAX_ADMITTED_SOURCE_BYTES,
    MAX_RESULT_ARTIFACT_BYTES,
    MAX_RESULT_BYTES,
)

MiB = 1024 * 1024

# The brief names 200 MiB for source. The controller admits sources up to this size and the worker
# intake refuses anything larger, so the two cannot disagree (DPES-811 AC6).
SOURCE_LIMIT_BYTES = MAX_ADMITTED_SOURCE_BYTES

# The result document keeps the protocol's 1 MiB cap (DPES-897 decision 4).
RESULT_LIMIT_BYTES = MAX_RESULT_BYTES

# Returned work (a patch or changed-files bundle with checksum) has its own 200 MiB cap, which
# matches the source limit in the brief. Section 15.5 of protocol-v1 records the separation.
RESULT_ARTIFACT_LIMIT_BYTES = MAX_RESULT_ARTIFACT_BYTES

WORKSPACE_GIB = 8
TMP_GIB = 1


@dataclass(frozen=True)
class Profile:
    name: str
    vcpus: int
    memory_gib: int


SMALL = Profile(name="2/8", vcpus=2, memory_gib=8)
STANDARD = Profile(name="4/16", vcpus=4, memory_gib=16)


@dataclass(frozen=True)
class Accounting:
    profile: Profile
    needed_gib: int
    fits: bool
    shortfall_gib: int


def accounting(profile: Profile) -> Accounting:
    """Whether the profile's memory holds the workspace and tmp disks it must carry."""
    needed = WORKSPACE_GIB + TMP_GIB
    shortfall = max(0, needed - profile.memory_gib)
    return Accounting(
        profile=profile,
        needed_gib=needed,
        fits=shortfall == 0,
        shortfall_gib=shortfall,
    )
