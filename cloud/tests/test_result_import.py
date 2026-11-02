"""DPES-817 AC3, AC4, and the edge addendum: the owned, recoverable local import transaction.

Every test runs against real git repositories and the in-memory storage standing in for GCS.
Fault injection fires at each named boundary; a repeat fetch must reconcile the owned import and
must never overwrite or delete what it does not own.
"""

import fcntl
import os
import shutil
import threading
from dataclasses import dataclass
from pathlib import Path
from typing import Any

import pytest

from fakes import OWNER_A, OWNER_B, RUN_ID
from gitfixtures import clone, commit_files, git, make_repo
from mao_cloud.checkpoint_publish import publish_result
from mao_cloud.protocol import ErrorCode, ProtocolError, RunState, artifact_object_name
from mao_cloud.result_import import (
    BOUNDARIES,
    ImportRequest,
    branch_name,
    fetch_and_import,
    lock_path,
    staging_ref,
    worktree_name,
)
from mao_cloud.result_paths import (
    CASE_INSENSITIVE_NO_SYMLINKS,
    CASE_INSENSITIVE_NORMALIZING,
    CASE_SENSITIVE,
)
from mao_cloud.results import (
    RETURN_BUNDLE_NAME,
    ArchiveLimits,
    ResultRefused,
    create_return_bundle,
)
from result_fixtures import (
    BASE_FILES,
    NOW,
    REPOSITORY,
    WORK_BRANCH,
    InjectedFault,
    Returned,
    archive_for,
    fault_at,
    git_sha,
    linked_worktrees,
    make_returned,
    make_returned_tree,
    publish_archive,
    refs_under,
    result_document,
    running_storage,
)

FIXED_ID = "0123456789abcdef0123456789abcdef"
WORK = {"README.md": b"base edited\n", "src/new.py": b"print('new')\n"}


@dataclass
class Arena:
    storage: Any
    returned: Any
    destination: Path


def _arena_from(returned: Any, root: Path, archive: Any) -> Arena:
    storage = running_storage()
    publish_archive(storage, archive)
    return Arena(storage=storage, returned=returned, destination=root / "reviews")


def arena(root: Path, *, complete: bool = True, reason: str | None = None) -> Arena:
    """A succeeded run whose archive is accepted through the gate, plus its base-only clone."""
    returned = make_returned(root, WORK)
    return _arena_from(
        returned, root, archive_for(returned, complete=complete, reason=reason)
    )


def tree_arena(root: Path, entries: dict[str, tuple[str, bytes]]) -> Arena:
    """A succeeded run whose returned tree is written from plumbing, so names never reach the disk."""
    returned = make_returned_tree(root, entries)
    return _arena_from(returned, root, archive_for(returned))


def request(a: Arena, **overrides: Any) -> ImportRequest:
    values: dict[str, Any] = {
        "owner_id": OWNER_A,
        "run_id": RUN_ID,
        "attempt": 1,
        "repository": REPOSITORY,
        "base_sha": a.returned.base_sha,
        "destination_root": a.destination,
        "fs_profile": CASE_SENSITIVE,
        "limits": ArchiveLimits(),
    }
    values.update(overrides)
    return ImportRequest(**values)


def fetch(a: Arena, *, boundary: Any = None, **overrides: Any) -> Any:
    return fetch_and_import(
        a.storage, a.returned.local, request(a, **overrides), boundary=boundary
    )


def _cloud_branches(repo: Path) -> list[str]:
    return refs_under(repo, "refs/heads/cloud-import/")


def _journal_dir(a: Arena) -> Path:
    return lock_path(a.returned.local).parent


def _assert_nothing_created(a: Arena) -> None:
    assert linked_worktrees(a.returned.local) == []
    assert _cloud_branches(a.returned.local) == []
    assert refs_under(a.returned.local, "refs/mao-import/") == []


class _Tampered:
    """Storage wrapper that returns altered bytes for one object, as a corrupted GCS read would."""

    def __init__(self, inner: Any, name: str, data: bytes) -> None:
        self._inner = inner
        self._name = name
        self._data = data

    def read_object(self, name: str) -> bytes | None:
        if name == self._name:
            return self._data
        return self._inner.read_object(name)

    def __getattr__(self, attribute: str) -> Any:
        return getattr(self._inner, attribute)


# --- the happy path and repeat fetch ---------------------------------------------------------------


def test_fetch_imports_into_a_fresh_unique_worktree_and_branch(tmp_path: Path) -> None:
    a = arena(tmp_path)
    receipt = fetch(a)
    assert receipt.status == "complete"
    assert receipt.reason is None
    assert receipt.run_id == RUN_ID
    assert receipt.attempt == 1
    assert receipt.base_sha == a.returned.base_sha
    assert receipt.final_sha == a.returned.final_sha
    assert receipt.branch == branch_name(RUN_ID, 1, receipt.import_id)
    assert receipt.branch.startswith("cloud-import/")
    assert len(receipt.archive_manifest_sha256) == 64
    assert receipt.tests_source == "self-reported"
    assert receipt.independent_verification is False
    worktree = Path(receipt.worktree)
    assert worktree.name == worktree_name(RUN_ID, 1, receipt.import_id)
    assert [Path(p).resolve() for p in linked_worktrees(a.returned.local)] == [
        worktree.resolve()
    ]
    assert (worktree / "README.md").read_bytes() == b"base edited\n"
    assert (worktree / "src" / "new.py").read_bytes() == b"print('new')\n"
    assert (worktree / "src" / "app.py").read_bytes() == BASE_FILES["src/app.py"]
    assert git(worktree, "rev-parse", "HEAD").strip() == a.returned.final_sha
    assert git(worktree, "status", "--porcelain") == ""
    assert refs_under(a.returned.local, "refs/mao-import/") == []


def test_repeat_fetch_returns_the_same_receipt_without_a_second_worktree(
    tmp_path: Path,
) -> None:
    a = arena(tmp_path)
    first = fetch(a)
    second = fetch(a)
    assert second == first
    assert len(linked_worktrees(a.returned.local)) == 1
    assert len(_cloud_branches(a.returned.local)) == 1


def test_import_never_writes_to_the_source(tmp_path: Path) -> None:
    a = arena(tmp_path)
    before = git(a.returned.source, "for-each-ref", "--format=%(refname) %(objectname)")
    fetch(a)
    assert (
        git(a.returned.source, "for-each-ref", "--format=%(refname) %(objectname)")
        == before
    )


def test_returned_executable_files_are_never_run(tmp_path: Path) -> None:
    sentinel = tmp_path / "ran.marker"
    script = f"#!/bin/sh\necho ran > {sentinel}\n".encode()
    a = tree_arena(
        tmp_path,
        {"run.sh": ("100755", script), "README.md": ("100644", b"base edited\n")},
    )
    receipt = fetch(a)
    assert (Path(receipt.worktree) / "run.sh").exists()
    assert not sentinel.exists()


def test_a_symlink_is_imported_as_a_link_on_a_profile_that_supports_it(
    tmp_path: Path,
) -> None:
    a = tree_arena(
        tmp_path,
        {"link": ("120000", b"README.md"), "README.md": ("100644", b"base edited\n")},
    )
    receipt = fetch(a, fs_profile=CASE_SENSITIVE)
    assert (Path(receipt.worktree) / "link").is_symlink()


@pytest.mark.parametrize(
    "setting",
    ["core.fsmonitor", "filter.review.smudge", "filter.review.clean", "filter.review.process"],
)
def test_import_never_executes_configured_commands_from_the_returned_tree(
    tmp_path: Path, setting: str
) -> None:
    marker = tmp_path / "returned-command-ran"
    a = tree_arena(
        tmp_path,
        {
            "tools/external": ("100755", f'#!/bin/sh\ntouch "{marker}"\nexit 1\n'.encode()),
            ".gitattributes": ("100644", b"*.txt filter=review\n"),
            "payload.txt": ("100644", b"returned bytes\n"),
        },
    )
    git(a.returned.local, "config", setting, "./tools/external")
    git(a.returned.local, "config", "filter.review.required", "true")

    receipt = fetch(a)

    assert receipt.status == "complete"
    assert (Path(receipt.worktree) / "payload.txt").read_bytes() == b"returned bytes\n"
    assert not marker.exists()


def test_symlinks_are_preserved_even_when_local_git_disables_symlink_checkout(
    tmp_path: Path,
) -> None:
    a = tree_arena(tmp_path, {"link": ("120000", b"README.md")})
    git(a.returned.local, "config", "core.symlinks", "false")

    receipt = fetch(a)
    link = Path(receipt.worktree) / "link"

    assert link.is_symlink()
    assert os.readlink(link) == "README.md"


@pytest.mark.parametrize("regular_file", [False, True])
def test_changed_symlink_type_or_target_is_refused_before_a_receipt(
    tmp_path: Path, regular_file: bool
) -> None:
    a = tree_arena(tmp_path, {"link": ("120000", b"README.md")})
    git(a.returned.local, "config", "core.symlinks", "false")
    target = a.destination / worktree_name(RUN_ID, 1, FIXED_ID) / "link"

    def change_link(boundary: str) -> None:
        if boundary == "worktree-created":
            target.unlink()
            if regular_file:
                target.write_bytes(b"README.md")
            else:
                target.symlink_to("src/app.py")

    with pytest.raises(ResultRefused) as info:
        fetch(a, boundary=change_link, import_id=FIXED_ID)

    assert info.value.code == "materialized-mismatch"
    assert not (_journal_dir(a) / f"{FIXED_ID}.receipt.json").exists()


def test_branch_text_in_a_base_subject_does_not_select_the_fetch_ref(tmp_path: Path) -> None:
    source = make_repo(tmp_path, BASE_FILES)
    git(source, "commit", "--amend", "--no-verify", "-m", "Fix refs/heads/topic")
    base_sha = git_sha(source)
    local = clone(source, tmp_path / "local")
    git(source, "checkout", "-q", "-b", WORK_BRANCH)
    commit_files(source, WORK, "returned work")
    returned = Returned(
        source=source,
        local=local,
        base_sha=base_sha,
        final_sha=git_sha(source),
        bundle=create_return_bundle(source, branch=WORK_BRANCH, base_sha=base_sha),
    )
    a = _arena_from(returned, tmp_path, archive_for(returned))

    receipt = fetch(a)

    assert receipt.final_sha == returned.final_sha
    assert (Path(receipt.worktree) / "README.md").read_bytes() == WORK["README.md"]


@pytest.mark.parametrize(
    "first,second",
    [("Docs/a.txt", "docs/b.txt"), ("café/a.txt", "café/b.txt")],
)
def test_directory_spelling_collisions_are_refused_before_creating_import_state(
    tmp_path: Path, first: str, second: str
) -> None:
    a = tree_arena(tmp_path, {first: ("100644", b"a"), second: ("100644", b"b")})

    with pytest.raises(ResultRefused) as info:
        fetch(a, fs_profile=CASE_INSENSITIVE_NORMALIZING)

    assert info.value.code == "destination-collision"
    _assert_nothing_created(a)
    assert not a.destination.exists()


# --- refusals before any local mutation ------------------------------------------------------------


def test_an_owner_other_than_the_run_owner_is_refused_and_nothing_is_created(
    tmp_path: Path,
) -> None:
    a = arena(tmp_path)
    with pytest.raises(ResultRefused) as info:
        fetch(a, owner_id=OWNER_B)
    assert info.value.code == "not-owner"
    _assert_nothing_created(a)


def test_a_run_that_did_not_succeed_has_nothing_to_import(tmp_path: Path) -> None:
    a = arena(tmp_path)
    a.storage = running_storage(state=RunState.RUNNING)
    with pytest.raises(ResultRefused) as info:
        fetch(a)
    assert info.value.code == "not-succeeded"
    _assert_nothing_created(a)


def test_success_without_a_result_archive_is_refused_on_fetch(tmp_path: Path) -> None:
    a = arena(tmp_path)
    storage = running_storage()
    # The legacy publish path accepts success without artifacts; the import must still refuse it.
    publish_result(
        storage,
        result_document(commit_sha=a.returned.final_sha),
        body=b'{"outcome":"succeeded"}',
        now=NOW,
    )
    a.storage = storage
    with pytest.raises(ResultRefused) as info:
        fetch(a)
    assert info.value.code == "no-result-archive"
    _assert_nothing_created(a)


def test_an_import_for_another_attempt_is_refused_as_stale(tmp_path: Path) -> None:
    a = arena(tmp_path)
    with pytest.raises(ResultRefused) as info:
        fetch(a, attempt=2)
    assert info.value.code == "stale-attempt"
    _assert_nothing_created(a)


def test_tampered_bundle_bytes_fail_the_checksum(tmp_path: Path) -> None:
    a = arena(tmp_path)
    altered = bytearray(a.returned.bundle)
    altered[-1] ^= 0xFF
    bundle_object = artifact_object_name(RUN_ID, 1, RETURN_BUNDLE_NAME)
    a.storage = _Tampered(a.storage, bundle_object, bytes(altered))
    with pytest.raises(ResultRefused) as info:
        fetch(a)
    assert info.value.code == "checksum-mismatch"
    _assert_nothing_created(a)


def test_a_changed_base_is_refused_and_nothing_is_created(tmp_path: Path) -> None:
    a = arena(tmp_path)
    with pytest.raises(ResultRefused) as info:
        fetch(a, base_sha="e" * 40)
    assert info.value.code == "wrong-base"
    _assert_nothing_created(a)


def test_a_bundle_built_on_another_commit_is_refused_as_wrong_base(
    tmp_path: Path,
) -> None:
    a = arena(tmp_path)
    source = a.returned.source
    first_work = a.returned.final_sha
    commit_files(source, {"second.txt": b"second\n"}, "second work commit")
    second_bundle = create_return_bundle(
        source, branch=WORK_BRANCH, base_sha=first_work
    )
    archive = archive_for(a.returned, bundle=second_bundle, final_sha=git_sha(source))
    a.storage = running_storage()
    # The controller gate refuses a wrong-base archive at publish, before any import runs.
    with pytest.raises(ProtocolError) as gate:
        publish_archive(a.storage, archive)
    assert gate.value.code is ErrorCode.INVALID_REQUEST
    # Post the same archive past the gate so the import-side check is what refuses it.
    document = result_document(
        commit_sha=archive.subject.final_sha, artifacts=archive.refs
    )
    publish_result(
        a.storage, document, body=document.model_dump_json().encode(), now=NOW
    )
    with pytest.raises(ResultRefused) as info:
        fetch(a)
    assert info.value.code == "wrong-base"
    _assert_nothing_created(a)


def test_a_different_repository_is_refused(tmp_path: Path) -> None:
    a = arena(tmp_path)
    with pytest.raises(ResultRefused) as info:
        fetch(a, repository="someone-else/elsewhere")
    assert info.value.code == "wrong-repository"
    _assert_nothing_created(a)


def test_oversize_returned_work_is_refused_on_fetch(tmp_path: Path) -> None:
    a = arena(tmp_path)
    with pytest.raises(ResultRefused) as info:
        fetch(a, limits=ArchiveLimits(max_returned_work_bytes=10))
    assert info.value.code == "returned-work-too-large"
    _assert_nothing_created(a)


def test_a_symlink_is_refused_on_a_profile_that_cannot_represent_it(
    tmp_path: Path,
) -> None:
    a = tree_arena(
        tmp_path,
        {"link": ("120000", b"README.md"), "README.md": ("100644", b"base edited\n")},
    )
    with pytest.raises(ResultRefused) as info:
        fetch(a, fs_profile=CASE_INSENSITIVE_NO_SYMLINKS)
    assert info.value.code == "unsupported-mode"
    _assert_nothing_created(a)


def test_case_equivalent_names_are_refused_on_an_insensitive_profile_and_nothing_is_renamed(
    tmp_path: Path,
) -> None:
    a = tree_arena(
        tmp_path, {"Docs.md": ("100644", b"one\n"), "docs.md": ("100644", b"two\n")}
    )
    with pytest.raises(ResultRefused) as info:
        fetch(a, fs_profile=CASE_INSENSITIVE_NORMALIZING)
    assert info.value.code == "destination-collision"
    _assert_nothing_created(a)


def test_a_destination_that_already_exists_is_refused_and_left_alone(
    tmp_path: Path,
) -> None:
    a = arena(tmp_path)
    target = a.destination / worktree_name(RUN_ID, 1, FIXED_ID)
    target.mkdir(parents=True)
    (target / "keep.txt").write_bytes(b"local work\n")
    with pytest.raises(ResultRefused) as info:
        fetch(a, import_id=FIXED_ID)
    assert info.value.code == "dirty-destination"
    assert (target / "keep.txt").read_bytes() == b"local work\n"
    assert _cloud_branches(a.returned.local) == []


def test_a_live_branch_with_the_import_name_is_never_overwritten(
    tmp_path: Path,
) -> None:
    a = arena(tmp_path)
    live = branch_name(RUN_ID, 1, FIXED_ID)
    git(a.returned.local, "branch", live, a.returned.base_sha)
    with pytest.raises(ResultRefused) as info:
        fetch(a, import_id=FIXED_ID)
    assert info.value.code == "branch-exists"
    assert git_sha(a.returned.local, f"refs/heads/{live}") == a.returned.base_sha
    assert linked_worktrees(a.returned.local) == []
    assert refs_under(a.returned.local, "refs/mao-import/") == []


# --- concurrency -----------------------------------------------------------------------------------


def test_a_held_project_lock_refuses_the_import_without_touching_anything(
    tmp_path: Path,
) -> None:
    a = arena(tmp_path)
    path = lock_path(a.returned.local)
    path.parent.mkdir(parents=True, exist_ok=True)
    with open(path, "a+b") as handle:
        fcntl.flock(handle.fileno(), fcntl.LOCK_EX | fcntl.LOCK_NB)
        with pytest.raises(ResultRefused) as info:
            fetch(a)
        assert info.value.code == "import-busy"
    _assert_nothing_created(a)


def test_a_second_import_during_the_first_is_refused_not_interleaved(
    tmp_path: Path,
) -> None:
    a = arena(tmp_path)
    entered = threading.Event()
    release = threading.Event()
    outcome: dict[str, Any] = {}

    def hold(boundary: str) -> None:
        if boundary == "lock-held":
            entered.set()
            release.wait(timeout=30)

    def first() -> None:
        try:
            outcome["first"] = fetch(a, boundary=hold)
        except Exception as exc:  # reported by the assertions below
            outcome["error"] = exc

    worker = threading.Thread(target=first)
    worker.start()
    try:
        assert entered.wait(timeout=30)
        with pytest.raises(ResultRefused) as info:
            fetch(a)
        assert info.value.code == "import-busy"
    finally:
        release.set()
        worker.join(timeout=60)
    assert "error" not in outcome, outcome.get("error")
    assert outcome["first"].status == "complete"
    assert len(linked_worktrees(a.returned.local)) == 1


# --- fault injection at every boundary -------------------------------------------------------------


def test_every_boundary_is_named_once_and_covers_each_owned_step() -> None:
    assert len(BOUNDARIES) == len(set(BOUNDARIES))
    assert {
        "lock-held",
        "journal-staged",
        "staging-ref-created",
        "worktree-created",
        "materialized",
        "receipt-written",
        "staging-released",
    } <= set(BOUNDARIES)


@pytest.mark.parametrize("boundary", BOUNDARIES)
def test_a_fault_at_each_boundary_is_recovered_by_a_repeat_fetch(
    tmp_path: Path, boundary: str
) -> None:
    a = arena(tmp_path)
    with pytest.raises(InjectedFault):
        fetch(a, boundary=fault_at(boundary), import_id=FIXED_ID)

    receipt = fetch(a, import_id=FIXED_ID)

    assert receipt.status == "complete"
    assert receipt.import_id == FIXED_ID
    assert len(linked_worktrees(a.returned.local)) == 1
    assert _cloud_branches(a.returned.local) == [f"refs/heads/{receipt.branch}"]
    assert refs_under(a.returned.local, "refs/mao-import/") == []
    worktree = Path(receipt.worktree)
    assert (worktree / "README.md").read_bytes() == b"base edited\n"
    assert git(worktree, "status", "--porcelain") == ""
    assert git(worktree, "rev-parse", "HEAD").strip() == a.returned.final_sha
    assert fetch(a, import_id=FIXED_ID) == receipt


def test_a_repeat_fetch_without_an_explicit_id_reuses_the_same_identity(
    tmp_path: Path,
) -> None:
    a = arena(tmp_path)
    with pytest.raises(InjectedFault):
        fetch(a, boundary=fault_at("worktree-created"))
    receipt = fetch(a)
    assert receipt == fetch(a)
    assert len(linked_worktrees(a.returned.local)) == 1


def test_an_owned_partial_worktree_with_foreign_content_is_neither_overwritten_nor_removed(
    tmp_path: Path,
) -> None:
    a = arena(tmp_path)
    with pytest.raises(InjectedFault):
        fetch(a, boundary=fault_at("worktree-created"), import_id=FIXED_ID)
    partial = a.destination / worktree_name(RUN_ID, 1, FIXED_ID)
    (partial / "stray.txt").write_bytes(b"someone's notes\n")

    with pytest.raises(ResultRefused) as info:
        fetch(a, import_id=FIXED_ID)

    assert info.value.code == "unknown-partial"
    assert (partial / "stray.txt").read_bytes() == b"someone's notes\n"
    assert _cloud_branches(a.returned.local) == [
        f"refs/heads/{branch_name(RUN_ID, 1, FIXED_ID)}"
    ]


def test_a_materialized_tree_that_differs_from_the_returned_tree_is_not_published(
    tmp_path: Path,
) -> None:
    a = arena(tmp_path)
    target = a.destination / worktree_name(RUN_ID, 1, FIXED_ID)

    def add_foreign_file(boundary: str) -> None:
        if boundary == "worktree-created":
            (target / "extra.txt").write_bytes(b"not returned\n")

    with pytest.raises(ResultRefused) as info:
        fetch(a, boundary=add_foreign_file, import_id=FIXED_ID)

    assert info.value.code == "materialized-mismatch"
    assert not (_journal_dir(a) / f"{FIXED_ID}.receipt.json").exists()
    assert (target / "extra.txt").read_bytes() == b"not returned\n"
    assert refs_under(a.returned.local, "refs/mao-import/") == [staging_ref(FIXED_ID)]
    with pytest.raises(ResultRefused) as again:
        fetch(a, import_id=FIXED_ID)
    assert again.value.code == "unknown-partial"


def test_a_published_import_whose_worktree_was_removed_is_not_recreated(
    tmp_path: Path,
) -> None:
    a = arena(tmp_path)
    receipt = fetch(a)
    shutil.rmtree(receipt.worktree)
    with pytest.raises(ResultRefused) as info:
        fetch(a)
    assert info.value.code == "published-worktree-missing"
    assert not Path(receipt.worktree).exists()


# --- partial and self-reported ---------------------------------------------------------------------


def test_a_partial_archive_imports_visibly_incomplete(tmp_path: Path) -> None:
    a = arena(tmp_path, complete=False, reason="checkpoint stopped after 3 of 5 files")
    receipt = fetch(a)
    assert receipt.status == "partial"
    assert receipt.reason == "checkpoint stopped after 3 of 5 files"


def test_a_self_reported_test_result_is_never_claimed_as_independent(
    tmp_path: Path,
) -> None:
    receipt = fetch(arena(tmp_path))
    assert receipt.independent_verification is False
    assert receipt.tests_source == "self-reported"
