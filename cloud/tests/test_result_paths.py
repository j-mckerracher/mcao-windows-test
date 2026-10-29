"""DPES-817 edge addendum: validate the full returned tree against an explicit destination profile.

The profiles are named assumptions the caller chooses. Nothing here probes a volume or claims one
filesystem behaves the same everywhere; each control names the profile it runs against.
"""

import pytest

from mao_cloud.result_paths import (
    CASE_INSENSITIVE_NORMALIZING,
    CASE_INSENSITIVE_NO_SYMLINKS,
    CASE_SENSITIVE,
    TreeEntry,
    validate_destination,
)
from mao_cloud.results import ResultRefused

FILE = "100644"
EXEC = "100755"
LINK = "120000"
GITLINK = "160000"


def _entries(*pairs: tuple[str, str]) -> list[TreeEntry]:
    return [TreeEntry(path=path, mode=mode) for path, mode in pairs]


# --- compatible controls: the destination can represent these names --------------------------------


def test_compatible_control_case_only_names_pass_on_a_case_sensitive_profile() -> None:
    validate_destination(_entries(("Foo", FILE), ("foo", FILE)), CASE_SENSITIVE)


def test_compatible_control_nfc_and_nfd_names_pass_when_the_profile_does_not_normalize() -> (
    None
):
    validate_destination(
        _entries(("café.txt", FILE), ("café.txt", FILE)), CASE_SENSITIVE
    )


def test_compatible_control_symlinks_pass_when_the_profile_supports_them() -> None:
    validate_destination(_entries(("link", LINK), ("target.txt", FILE)), CASE_SENSITIVE)


def test_compatible_control_executable_files_pass() -> None:
    validate_destination(_entries(("run.sh", EXEC)), CASE_SENSITIVE)


# --- incompatible controls: refused, never renamed --------------------------------------------------


def test_incompatible_control_case_equivalent_names_are_refused_not_renamed() -> None:
    with pytest.raises(ResultRefused) as info:
        validate_destination(
            _entries(("Readme.md", FILE), ("README.md", FILE)),
            CASE_INSENSITIVE_NORMALIZING,
        )
    assert info.value.code == "destination-collision"


def test_incompatible_control_normalization_equivalent_names_are_refused() -> None:
    with pytest.raises(ResultRefused) as info:
        validate_destination(
            _entries(("café.txt", FILE), ("café.txt", FILE)),
            CASE_INSENSITIVE_NORMALIZING,
        )
    assert info.value.code == "destination-collision"


def test_file_versus_directory_collision_is_refused() -> None:
    with pytest.raises(ResultRefused) as info:
        validate_destination(_entries(("a", FILE), ("a/b.txt", FILE)), CASE_SENSITIVE)
    assert info.value.code == "file-directory-collision"


@pytest.mark.parametrize(
    "first,second",
    [("Docs/a.txt", "docs/b.txt"), ("café/a.txt", "café/b.txt")],
)
def test_equivalent_directory_spellings_are_refused(first: str, second: str) -> None:
    with pytest.raises(ResultRefused) as info:
        validate_destination(
            _entries((first, FILE), (second, FILE)), CASE_INSENSITIVE_NORMALIZING
        )
    assert info.value.code == "destination-collision"


def test_repeated_directory_prefixes_with_identical_spelling_are_allowed() -> None:
    validate_destination(
        _entries(("Docs/a.txt", FILE), ("Docs/b.txt", FILE)),
        CASE_INSENSITIVE_NORMALIZING,
    )


def test_case_folded_file_versus_directory_collision_is_refused_on_insensitive_profile() -> (
    None
):
    with pytest.raises(ResultRefused) as info:
        validate_destination(
            _entries(("Docs", FILE), ("docs/guide.md", FILE)),
            CASE_INSENSITIVE_NORMALIZING,
        )
    assert info.value.code == "file-directory-collision"


def test_symlink_is_refused_where_the_profile_cannot_represent_it() -> None:
    with pytest.raises(ResultRefused) as info:
        validate_destination(_entries(("link", LINK)), CASE_INSENSITIVE_NO_SYMLINKS)
    assert info.value.code == "unsupported-mode"


@pytest.mark.parametrize("profile", [CASE_SENSITIVE, CASE_INSENSITIVE_NO_SYMLINKS])
def test_gitlink_submodule_entries_are_always_refused(profile: object) -> None:
    with pytest.raises(ResultRefused) as info:
        validate_destination(_entries(("vendor/lib", GITLINK)), profile)  # type: ignore[arg-type]
    assert info.value.code == "unsupported-mode"


@pytest.mark.parametrize(
    "path", ["../escape.txt", "/abs.txt", ".git/config", "a/./b", "a\\b.txt"]
)
def test_unsafe_tree_paths_are_refused_before_any_destination_check(path: str) -> None:
    with pytest.raises(ResultRefused) as info:
        validate_destination(_entries((path, FILE)), CASE_SENSITIVE)
    assert info.value.code == "unsafe-path"


def test_a_three_way_case_collision_is_refused_as_one_group() -> None:
    with pytest.raises(ResultRefused) as info:
        validate_destination(
            _entries(("ok.txt", FILE), ("OK.txt", FILE), ("OK.TXT", FILE)),
            CASE_INSENSITIVE_NORMALIZING,
        )
    assert info.value.code == "destination-collision"
