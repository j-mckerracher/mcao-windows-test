"""DPES-817 AC1 and AC2: the runner's result archive, its checkpoint, and the controller gate."""

import json
import os
import subprocess
from pathlib import Path

import pytest
from pydantic import ValidationError

from fakes import CLAIM_ID, RUN_ID
from gitfixtures import git, make_repo
from result_fixtures import (
    NOW,
    RESULT_NAME,
    WORK_BRANCH,
    archive_for,
    git_sha,
    make_returned,
    publish_archive,
    result_document,
    running_storage,
)
from mao_cloud.protocol import (
    MAX_CHECKPOINT_BYTES,
    MAX_RESULT_BYTES,
    MAX_RETURNED_WORK_BYTES,
    ArtifactRef,
    ErrorCode,
    ProtocolError,
    RunResultDocument,
    RunState,
    artifact_object_name,
    sha256_hex,
)
from mao_cloud.results import (
    AGENT_FINAL_NAME,
    LOG_NAME,
    MANIFEST_NAME,
    RETURN_BUNDLE_NAME,
    TESTS_NAME,
    ArchiveLimits,
    CheckpointLimits,
    ReportedTest,
    ResultRefused,
    accept_archived_result,
    commit_checkpoint,
    create_return_bundle,
    gather_dirty_entries,
    parse_archive,
    parse_return_header,
    plan_checkpoint,
)


@pytest.fixture(autouse=True)
def hermetic_git_env(
    tmp_path_factory: pytest.TempPathFactory, monkeypatch: pytest.MonkeyPatch
) -> None:
    """Run the controller's own git calls with no ambient config or identity, as in a bare container."""
    monkeypatch.setenv("HOME", str(tmp_path_factory.mktemp("home")))
    monkeypatch.setenv("GIT_CONFIG_GLOBAL", os.devnull)
    monkeypatch.setenv("GIT_CONFIG_NOSYSTEM", "1")


def _manifest(archive) -> dict:  # type: ignore[no-untyped-def]
    return json.loads(archive.objects[MANIFEST_NAME])


def _read(archive):  # type: ignore[no-untyped-def]
    return lambda name: archive.objects.get(name)


# --- limits ----------------------------------------------------------------------------------------


def test_returned_work_has_its_own_cap_separate_from_the_result_document() -> None:
    assert MAX_RETURNED_WORK_BYTES == 200 * 1024 * 1024
    assert MAX_RESULT_BYTES == 1024 * 1024


# --- AC1: the archive ------------------------------------------------------------------------------


def test_archive_names_every_part_and_its_checksum_and_size(tmp_path: Path) -> None:
    archive = archive_for(make_returned(tmp_path, {"src/new.py": b"print('new')\n"}))
    manifest = _manifest(archive)
    assert manifest["schema"] == "mao-result-archive/v1"
    assert manifest["run_id"] == RUN_ID
    assert manifest["attempt"] == 1
    assert manifest["fence"] == 1
    assert manifest["claim_id"] == CLAIM_ID
    assert manifest["base_sha"] == archive.subject.base_sha
    assert manifest["final_sha"] == archive.subject.final_sha
    assert manifest["image_digest"].startswith("sha256:")
    assert manifest["cli_version"] == "cli-1.0"
    assert manifest["model"] == "model-a"
    assert manifest["profile"] == "standard-4vcpu-16gib"
    listed = {entry["name"]: entry for entry in manifest["artifacts"]}
    assert set(listed) == {RETURN_BUNDLE_NAME, AGENT_FINAL_NAME, TESTS_NAME, LOG_NAME}
    for name, entry in listed.items():
        data = archive.objects[name]
        assert entry["sha256"] == sha256_hex(data)
        assert entry["size_bytes"] == len(data)


def test_self_reported_tests_are_labelled_and_never_independent(tmp_path: Path) -> None:
    archive = archive_for(make_returned(tmp_path, {"a.txt": b"a\n"}))
    manifest = _manifest(archive)
    assert manifest["tests"]["source"] == "self-reported"
    assert manifest["tests"]["outcomes"] == [{"name": "unit", "status": "passed"}]
    assert manifest["independent_verification"] is False


def test_logs_are_redacted_before_they_are_stored(tmp_path: Path) -> None:
    archive = archive_for(
        make_returned(tmp_path, {"a.txt": b"a\n"}),
        logs=("token=abc123def\n", "key ya29.SECRETVALUE\n"),
    )
    stored = archive.objects[LOG_NAME]
    assert b"abc123def" not in stored
    assert b"SECRETVALUE" not in stored
    assert b"[REDACTED" in stored


def test_archive_refuses_returned_work_over_the_cap_at_build(tmp_path: Path) -> None:
    returned = make_returned(tmp_path, {"a.txt": b"a\n"})
    with pytest.raises(ResultRefused) as info:
        archive_for(returned, limits=ArchiveLimits(max_returned_work_bytes=10))
    assert info.value.code == "returned-work-too-large"


def test_parse_refuses_returned_work_over_the_cap_on_the_fetch_side(
    tmp_path: Path,
) -> None:
    archive = archive_for(make_returned(tmp_path, {"a.txt": b"a\n"}))
    document = result_document(
        commit_sha=archive.subject.final_sha, artifacts=archive.refs
    )
    with pytest.raises(ResultRefused) as info:
        parse_archive(
            document, _read(archive), limits=ArchiveLimits(max_returned_work_bytes=10)
        )
    assert info.value.code == "returned-work-too-large"


def test_returned_work_exactly_at_its_cap_is_archived_and_parsed(tmp_path: Path) -> None:
    returned = make_returned(tmp_path, {"a.txt": b"a\n"})
    size = len(archive_for(returned).objects[RETURN_BUNDLE_NAME])
    limits = ArchiveLimits(max_returned_work_bytes=size)
    archive = archive_for(returned, limits=limits)
    document = result_document(
        commit_sha=archive.subject.final_sha, artifacts=archive.refs
    )
    parse_archive(document, _read(archive), limits=limits)
    assert len(archive.objects[RETURN_BUNDLE_NAME]) == size


def test_returned_work_one_byte_over_its_cap_is_refused_at_build_and_fetch(
    tmp_path: Path,
) -> None:
    returned = make_returned(tmp_path, {"a.txt": b"a\n"})
    size = len(archive_for(returned).objects[RETURN_BUNDLE_NAME])
    limits = ArchiveLimits(max_returned_work_bytes=size - 1)
    with pytest.raises(ResultRefused) as build:
        archive_for(returned, limits=limits)
    assert build.value.code == "returned-work-too-large"
    archive = archive_for(returned)
    document = result_document(
        commit_sha=archive.subject.final_sha, artifacts=archive.refs
    )
    with pytest.raises(ResultRefused) as fetch:
        parse_archive(document, _read(archive), limits=limits)
    assert fetch.value.code == "returned-work-too-large"


def test_parse_refuses_a_manifest_that_disagrees_with_the_result_document(
    tmp_path: Path,
) -> None:
    archive = archive_for(make_returned(tmp_path, {"a.txt": b"a\n"}))
    document = result_document(commit_sha="f" * 40, artifacts=archive.refs)
    with pytest.raises(ResultRefused) as info:
        parse_archive(document, _read(archive), limits=ArchiveLimits())
    assert info.value.code == "subject-mismatch"


def test_parse_round_trips_the_subject_bundle_and_self_reported_tests(
    tmp_path: Path,
) -> None:
    returned = make_returned(tmp_path, {"a.txt": b"a\n"})
    archive = archive_for(returned)
    document = result_document(
        commit_sha=archive.subject.final_sha, artifacts=archive.refs
    )
    parsed = parse_archive(document, _read(archive), limits=ArchiveLimits())
    assert parsed.subject.base_sha == returned.base_sha
    assert parsed.subject.final_sha == returned.final_sha
    assert parsed.return_bundle == returned.bundle
    assert parsed.complete is True
    assert parsed.incomplete_reason is None
    assert parsed.independent_verification is False
    assert parsed.tests == (ReportedTest("unit", "passed"),)


def test_a_partial_archive_is_visibly_incomplete(tmp_path: Path) -> None:
    archive = archive_for(
        make_returned(tmp_path, {"a.txt": b"a\n"}),
        complete=False,
        reason="checkpoint stopped after 3 of 5 files",
    )
    document = result_document(
        commit_sha=archive.subject.final_sha, artifacts=archive.refs
    )
    parsed = parse_archive(document, _read(archive), limits=ArchiveLimits())
    assert parsed.complete is False
    assert parsed.incomplete_reason == "checkpoint stopped after 3 of 5 files"


def test_an_artifact_name_with_traversal_is_not_a_valid_reference() -> None:
    with pytest.raises(ValidationError):
        ArtifactRef(name="archive/../escape", sha256="a" * 64, size_bytes=1)


# --- the return bundle header ----------------------------------------------------------------------


def test_header_names_one_base_prerequisite_and_one_branch_ref(tmp_path: Path) -> None:
    returned = make_returned(tmp_path, {"a.txt": b"a\n"})
    header = parse_return_header(returned.bundle, base_sha=returned.base_sha)
    assert header.prerequisites == (returned.base_sha,)
    assert header.ref == f"refs/heads/{WORK_BRANCH}"
    assert header.tip == returned.final_sha


def test_bundle_names_only_the_work_branch_above_base(tmp_path: Path) -> None:
    returned = make_returned(tmp_path, {"a.txt": b"a\n"})
    refs = [
        line.split(b" ", 1)[1]
        for line in returned.bundle.split(b"\n")
        if b" refs/" in line
    ]
    assert refs == [f"refs/heads/{WORK_BRANCH}".encode()]


def test_self_contained_bundle_is_refused_as_wrong_base(tmp_path: Path) -> None:
    source = make_repo(tmp_path, {"a.txt": b"a\n"})
    sha = git_sha(source)
    git(source, "checkout", "-q", "-b", WORK_BRANCH)
    bundle_path = tmp_path / "self.bundle"
    subprocess.run(
        ["git", "-C", str(source), "bundle", "create", str(bundle_path), WORK_BRANCH],
        check=True,
        capture_output=True,
        env={
            "PATH": os.environ["PATH"],
            "HOME": str(tmp_path),
            "GIT_CONFIG_GLOBAL": os.devnull,
        },
    )
    with pytest.raises(ResultRefused) as info:
        parse_return_header(bundle_path.read_bytes(), base_sha=sha)
    assert info.value.code == "wrong-base"


def test_bundle_against_another_base_is_refused(tmp_path: Path) -> None:
    returned = make_returned(tmp_path, {"a.txt": b"a\n"})
    with pytest.raises(ResultRefused) as info:
        parse_return_header(returned.bundle, base_sha="e" * 40)
    assert info.value.code == "wrong-base"


@pytest.mark.parametrize(
    "bundle",
    [
        b"",
        b"# v2 git bundle\n",
        b"# v3 git bundle\n\n",
        b"# v2 git bundle\n@object-format=sha256\n\n",
        b"# v2 git bundle\n"
        + b"1" * 40
        + b" refs/heads/a\n"
        + b"2" * 40
        + b" refs/heads/b\n\n",
        b"# v2 git bundle\n-" + b"1" * 40 + b" base\n" + b"2" * 40 + b" refs/heads/a\n",
    ],
)
def test_malformed_or_multi_ref_headers_are_refused(bundle: bytes) -> None:
    with pytest.raises(ResultRefused) as info:
        parse_return_header(bundle, base_sha="1" * 40)
    assert info.value.code in {"bad-header", "wrong-base"}


# --- AC2: the controller gate ----------------------------------------------------------------------


def test_success_without_a_result_archive_is_refused_and_nothing_is_written(
    tmp_path: Path,
) -> None:
    storage = running_storage()
    with pytest.raises(ProtocolError) as info:
        accept_archived_result(
            storage, result_document(commit_sha="a" * 40, artifacts=()), now=NOW
        )
    assert info.value.code is ErrorCode.INVALID_REQUEST
    assert storage.object_names() == []
    run = storage.read_run(RUN_ID)
    assert run is not None and run.state is RunState.RUNNING


def test_failure_needs_no_result_archive(tmp_path: Path) -> None:
    storage = running_storage()
    run = accept_archived_result(storage, result_document(outcome="failed"), now=NOW)
    assert run.state is RunState.FAILED


def test_gate_refuses_an_archive_object_missing_from_storage(tmp_path: Path) -> None:
    archive = archive_for(make_returned(tmp_path, {"a.txt": b"a\n"}))
    storage = running_storage()
    for name in (MANIFEST_NAME, AGENT_FINAL_NAME, TESTS_NAME, LOG_NAME):
        storage.create_object(
            artifact_object_name(RUN_ID, 1, name), archive.objects[name]
        )
    document = result_document(
        commit_sha=archive.subject.final_sha, artifacts=archive.refs
    )
    with pytest.raises(ProtocolError) as info:
        accept_archived_result(storage, document, now=NOW)
    assert info.value.code is ErrorCode.CHECKSUM_MISMATCH
    assert RESULT_NAME not in storage.object_names()


def test_gate_refuses_a_checksum_that_does_not_match_the_stored_bytes(
    tmp_path: Path,
) -> None:
    archive = archive_for(make_returned(tmp_path, {"a.txt": b"a\n"}))
    storage = running_storage()
    for name, data in archive.objects.items():
        storage.create_object(artifact_object_name(RUN_ID, 1, name), data)
    forged = [
        ArtifactRef(
            name=ref.name, sha256=sha256_hex(b"other"), size_bytes=ref.size_bytes
        )
        if ref.name == RETURN_BUNDLE_NAME
        else ref
        for ref in archive.refs
    ]
    document = result_document(commit_sha=archive.subject.final_sha, artifacts=forged)
    with pytest.raises(ProtocolError) as info:
        accept_archived_result(storage, document, now=NOW)
    assert info.value.code is ErrorCode.CHECKSUM_MISMATCH
    assert RESULT_NAME not in storage.object_names()


def test_verified_archive_is_accepted_once_and_the_result_is_immutable(
    tmp_path: Path,
) -> None:
    archive = archive_for(make_returned(tmp_path, {"a.txt": b"a\n"}))
    storage = running_storage()
    run = publish_archive(storage, archive)
    assert run.state is RunState.SUCCEEDED
    stored = RunResultDocument.model_validate_json(storage.read_object(RESULT_NAME))
    assert stored == result_document(
        commit_sha=archive.subject.final_sha, artifacts=archive.refs
    )
    with pytest.raises(ProtocolError) as info:
        publish_archive(storage, archive)
    assert info.value.code is ErrorCode.TERMINAL_STATE


def test_stale_attempt_is_refused_before_any_artifact_is_read(tmp_path: Path) -> None:
    archive = archive_for(make_returned(tmp_path, {"a.txt": b"a\n"}))
    storage = running_storage()
    document = result_document(
        commit_sha=archive.subject.final_sha, artifacts=archive.refs, attempt=2
    )
    with pytest.raises(ProtocolError) as info:
        accept_archived_result(storage, document, now=NOW)
    assert info.value.code is ErrorCode.STALE_ATTEMPT
    assert storage.object_names() == []


def test_a_cancelled_run_fences_the_archived_result(tmp_path: Path) -> None:
    archive = archive_for(make_returned(tmp_path, {"a.txt": b"a\n"}))
    storage = running_storage(state=RunState.CANCEL_REQUESTED)
    document = result_document(
        commit_sha=archive.subject.final_sha, artifacts=archive.refs
    )
    with pytest.raises(ProtocolError) as info:
        accept_archived_result(storage, document, now=NOW)
    assert info.value.code is ErrorCode.RESULT_FENCED


# --- AC1: runner checkpoint of dirty files ---------------------------------------------------------


def _worktree(tmp_path: Path) -> Path:
    repo = make_repo(
        tmp_path, {"a.txt": b"a\n", "b.txt": b"b\n", ".gitignore": b"*.log\n"}
    )
    (repo / "a.txt").write_bytes(b"a changed\n")
    (repo / "b.txt").unlink()
    (repo / "new.txt").write_bytes(b"new\n")
    (repo / "debug.log").write_bytes(b"ignored\n")
    return repo


def test_checkpoint_commits_nonignored_changes_and_skips_ignored_files(
    tmp_path: Path,
) -> None:
    repo = _worktree(tmp_path)
    before = git_sha(repo)
    plan = plan_checkpoint(gather_dirty_entries(repo), limits=CheckpointLimits())
    commit_checkpoint(repo, plan, message="checkpoint: DPES-817 test")
    assert git_sha(repo) != before
    tree = git(repo, "ls-tree", "-r", "--name-only", "HEAD").split()
    assert "new.txt" in tree
    assert "a.txt" in tree
    assert "b.txt" not in tree
    assert "debug.log" not in tree


def test_checkpoint_records_a_rename_as_a_delete_and_an_add(tmp_path: Path) -> None:
    repo = make_repo(tmp_path, {"old.txt": b"same\n"})
    git(repo, "mv", "old.txt", "moved.txt")
    plan = plan_checkpoint(gather_dirty_entries(repo), limits=CheckpointLimits())
    commit_checkpoint(repo, plan, message="checkpoint: DPES-817 rename")
    tree = git(repo, "ls-tree", "-r", "--name-only", "HEAD").split()
    assert tree == ["moved.txt"]


def test_checkpoint_refuses_a_secret_and_commits_nothing(tmp_path: Path) -> None:
    repo = make_repo(tmp_path, {"a.txt": b"a\n"})
    (repo / "config.txt").write_bytes(b"api_key = hunter2hunter2\n")
    before = git_sha(repo)
    with pytest.raises(ResultRefused) as info:
        plan_checkpoint(gather_dirty_entries(repo), limits=CheckpointLimits())
    assert info.value.code == "secret-suspected"
    assert git_sha(repo) == before


def test_checkpoint_refuses_content_it_cannot_scan(tmp_path: Path) -> None:
    repo = make_repo(tmp_path, {"a.txt": b"a\n"})
    (repo / "blob.bin").write_bytes(b"\xff\xfe\x00\x01")
    with pytest.raises(ResultRefused) as info:
        plan_checkpoint(gather_dirty_entries(repo), limits=CheckpointLimits())
    assert info.value.code == "unscannable"


def test_checkpoint_refuses_too_many_files_and_too_many_bytes(tmp_path: Path) -> None:
    repo = _worktree(tmp_path)
    with pytest.raises(ResultRefused) as files:
        plan_checkpoint(
            gather_dirty_entries(repo), limits=CheckpointLimits(max_files=1)
        )
    assert files.value.code == "too-many-files"
    with pytest.raises(ResultRefused) as size:
        plan_checkpoint(
            gather_dirty_entries(repo), limits=CheckpointLimits(max_bytes=4)
        )
    assert size.value.code == "checkpoint-too-large"
    assert CheckpointLimits().max_bytes == MAX_CHECKPOINT_BYTES


def test_return_header_ignores_utf8_base_commit_subject(tmp_path: Path) -> None:
    repo = make_repo(tmp_path, {"a.txt": b"base\n"})
    git(repo, "commit", "--amend", "-m", "Fix café handling")
    base_sha = git_sha(repo)
    git(repo, "checkout", "-q", "-b", WORK_BRANCH)
    (repo / "a.txt").write_bytes(b"changed\n")
    git(repo, "add", "a.txt")
    git(repo, "commit", "-m", "work")

    bundle = create_return_bundle(repo, branch=WORK_BRANCH, base_sha=base_sha)
    header = parse_return_header(bundle, base_sha=base_sha)

    assert header.prerequisites == (base_sha,)
    assert header.ref == f"refs/heads/{WORK_BRANCH}"
    assert header.tip == git_sha(repo)


@pytest.mark.parametrize("defect", ["manifest", "header", "base", "tip"])
def test_gate_refuses_checksummed_but_invalid_archive_before_publication(
    tmp_path: Path, defect: str
) -> None:
    returned = make_returned(tmp_path, {"a.txt": b"a\n"})
    archive = archive_for(
        returned,
        base_sha="e" * 40 if defect == "base" else None,
        final_sha="f" * 40 if defect == "tip" else None,
        bundle=b"not a bundle" if defect == "header" else None,
    )
    if defect == "manifest":
        archive.objects[MANIFEST_NAME] = b"{}"
    refs = tuple(
        ArtifactRef(name=name, sha256=sha256_hex(data), size_bytes=len(data))
        for name, data in archive.objects.items()
    )
    storage = running_storage()
    for name, data in archive.objects.items():
        storage.create_object(artifact_object_name(RUN_ID, 1, name), data)
    document = result_document(commit_sha=archive.subject.final_sha, artifacts=refs)

    with pytest.raises(ProtocolError) as info:
        accept_archived_result(storage, document, now=NOW)

    assert info.value.code is ErrorCode.INVALID_REQUEST
    assert RESULT_NAME not in storage.object_names()
    run = storage.read_run(RUN_ID)
    assert run is not None and run.state is RunState.RUNNING


@pytest.mark.parametrize("remove", [False, True], ids=["changed", "removed"])
def test_checkpoint_treats_magic_looking_filenames_as_literal_paths(
    tmp_path: Path, remove: bool
) -> None:
    name = ":(literal)notes.txt"
    repo = make_repo(tmp_path, {name: b"literal base\n", "notes.txt": b"other base\n"})
    if remove:
        (repo / name).unlink()
    else:
        (repo / name).write_bytes(b"literal changed\n")
    plan = plan_checkpoint(gather_dirty_entries(repo), limits=CheckpointLimits())

    commit_checkpoint(repo, plan, message="checkpoint: literal filename")

    assert git(repo, "show", "--format=", "--name-only", "HEAD").strip() == name
    assert git(repo, "show", "HEAD:notes.txt") == "other base\n"
    if not remove:
        assert git(repo, "show", f"HEAD:{name}") == "literal changed\n"
