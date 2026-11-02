"""Check that a returned tree can be written to a named destination profile without renaming anything.

A profile is an explicit, named assumption the caller chooses about how a destination treats names
and modes. Nothing here probes a volume. A tree that a profile cannot represent is refused whole;
no path is ever renamed, normalized into a different name, or silently dropped.
"""

import unicodedata
from collections.abc import Iterable
from dataclasses import dataclass

from mao_cloud.results import ResultRefused
from mao_cloud.trusted_bundle import BundleRefused, check_path

MAX_PATH_BYTES = 1024
_REGULAR_MODES = frozenset({"100644", "100755"})
_LINK_MODE = "120000"


@dataclass(frozen=True)
class DestinationProfile:
    """How a named destination compares names and whether it can hold symlinks."""

    name: str
    case_insensitive: bool
    normalize_nfc: bool
    symlinks: bool


CASE_SENSITIVE = DestinationProfile(
    name="case-sensitive", case_insensitive=False, normalize_nfc=False, symlinks=True
)
CASE_INSENSITIVE_NORMALIZING = DestinationProfile(
    name="case-insensitive-nfc",
    case_insensitive=True,
    normalize_nfc=True,
    symlinks=True,
)
CASE_INSENSITIVE_NO_SYMLINKS = DestinationProfile(
    name="case-insensitive-no-symlinks",
    case_insensitive=True,
    normalize_nfc=False,
    symlinks=False,
)


@dataclass(frozen=True)
class TreeEntry:
    path: str
    mode: str


def _key(text: str, profile: DestinationProfile) -> str:
    if profile.normalize_nfc:
        text = unicodedata.normalize("NFC", text)
    if profile.case_insensitive:
        text = text.casefold()
    return text


def _check_entry(entry: TreeEntry, profile: DestinationProfile) -> None:
    try:
        check_path(entry.path.encode("utf-8"), max_path_bytes=MAX_PATH_BYTES)
    except BundleRefused:
        raise ResultRefused(
            "unsafe-path", "a returned path is unsafe to write"
        ) from None
    if entry.mode == _LINK_MODE:
        if not profile.symlinks:
            raise ResultRefused(
                "unsupported-mode", f"profile {profile.name} cannot hold symlinks"
            )
        return
    if entry.mode not in _REGULAR_MODES:
        raise ResultRefused(
            "unsupported-mode", "a returned entry is a gitlink or unexpected mode"
        )


def _check_collisions(
    entries: Iterable[TreeEntry], profile: DestinationProfile
) -> None:
    names: dict[str, str] = {}
    files: set[str] = set()
    for entry in entries:
        key = _key(entry.path, profile)
        if key in names:
            raise ResultRefused(
                "destination-collision",
                f"two returned paths share one name on {profile.name}",
            )
        names[key] = entry.path
        files.add(key)
    directories: dict[str, str] = {}
    for path in names.values():
        parts = path.split("/")
        for depth in range(1, len(parts)):
            directory = "/".join(parts[:depth])
            key = _key(directory, profile)
            if key in directories and directories[key] != directory:
                raise ResultRefused(
                    "destination-collision",
                    f"two returned directories share one name on {profile.name}",
                )
            directories[key] = directory
    if files & directories.keys():
        raise ResultRefused(
            "file-directory-collision",
            f"a returned file and directory share one name on {profile.name}",
        )


def validate_destination(entries: list[TreeEntry], profile: DestinationProfile) -> None:
    """Raise ResultRefused when the tree cannot be written to the profile exactly as returned."""
    for entry in entries:
        _check_entry(entry, profile)
    _check_collisions(entries, profile)
