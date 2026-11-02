"""Shared fixtures for the DPES-817 result archive, destination, and import tests.

The returned work is built by real git through the runner's own bundle function, so the bundles
under test are the ones git produces. Storage is the in-memory fake standing in for GCS.
"""

import os
import subprocess
from collections.abc import Callable, Mapping
from dataclasses import dataclass
from datetime import timedelta
from pathlib import Path

from fakes import CLAIM_ID, EPOCH, IMAGE_DIGEST, RUN_ID, run_record
from gitfixtures import AUTHOR_ENV, clone, commit_all, git, make_repo
from mao_cloud.protocol import (
    ObjectKind,
    RunRecord,
    RunResultDocument,
    RunState,
    object_name,
)
from mao_cloud.results import (
    ArchiveLimits,
    ArchiveSubject,
    ReportedTest,
    ResultArchive,
    accept_archived_result,
    build_archive,
    create_return_bundle,
    store_archive,
)
from storage_fakes import InMemoryStorage

REPOSITORY = "mc-innersource/motif-hve-multi-agent-orchestrator"
NOW = EPOCH + timedelta(seconds=1)
BASE_FILES: dict[str, bytes] = {
    "README.md": b"base\n",
    "src/app.py": b"print('base')\n",
}
WORK_BRANCH = "work"
RESULT_NAME = object_name(ObjectKind.RESULT, run_id=RUN_ID, attempt=1)


def git_sha(repo: Path, rev: str = "HEAD") -> str:
    return git(repo, "rev-parse", rev).strip()


@dataclass(frozen=True)
class Returned:
    """A finished attempt: the commits it made above base, and the clone that holds only base."""

    source: Path
    local: Path
    base_sha: str
    final_sha: str
    bundle: bytes


def make_returned(
    root: Path,
    work: Mapping[str, bytes | None],
    *,
    links: Mapping[str, str] | None = None,
    executable: tuple[str, ...] = (),
) -> Returned:
    """Commit work on WORK_BRANCH above the base, bundle it, and clone the base for the review side.

    A None value in work deletes that base path. links maps a new symlink path to its target.
    """
    source = make_repo(root, BASE_FILES)
    base_sha = git_sha(source)
    local = clone(source, root / "local")
    git(source, "checkout", "-q", "-b", WORK_BRANCH)
    for relative, data in work.items():
        target = source / relative
        if data is None:
            target.unlink()
            continue
        target.parent.mkdir(parents=True, exist_ok=True)
        target.write_bytes(data)
    for relative, link_target in (links or {}).items():
        (source / relative).parent.mkdir(parents=True, exist_ok=True)
        os.symlink(link_target, source / relative)
    for relative in executable:
        os.chmod(source / relative, 0o755)
    commit_all(source, "work")
    final_sha = git_sha(source)
    bundle = create_return_bundle(source, branch=WORK_BRANCH, base_sha=base_sha)
    return Returned(
        source=source,
        local=local,
        base_sha=base_sha,
        final_sha=final_sha,
        bundle=bundle,
    )


def _plumb(
    repo: Path,
    args: list[str],
    *,
    env: Mapping[str, str] | None = None,
    stdin: bytes | None = None,
) -> str:
    """Run one git plumbing command with names kept byte-exact.

    core.ignorecase is forced off so case-variant names index apart. core.precomposeunicode is forced
    off so a macOS host does not rewrite a decomposed argv path to NFC before it is indexed.
    """
    completed = subprocess.run(
        [
            "git",
            "-C",
            str(repo),
            "-c",
            "core.ignorecase=false",
            "-c",
            "core.precomposeunicode=false",
            *args,
        ],
        input=stdin,
        capture_output=True,
        env={**os.environ, **(env or {})},
        check=True,
    )
    return completed.stdout.decode().strip()


def make_returned_tree(
    root: Path, entries: Mapping[str, tuple[str, bytes]]
) -> Returned:
    """Commit the base tree plus entries on WORK_BRANCH, written by plumbing only.

    Each entry maps a path to (git mode, blob bytes). Names that collide on a case-insensitive disk
    stay in the object store and never touch the working tree, so the fixture works on macOS.
    """
    source = make_repo(root, BASE_FILES)
    base_sha = git_sha(source)
    local = clone(source, root / "local")
    index = source / ".git" / "fixture-index"
    env = {**AUTHOR_ENV, "GIT_INDEX_FILE": str(index)}
    _plumb(source, ["read-tree", base_sha], env=env)
    for path, (mode, data) in entries.items():
        blob = _plumb(source, ["hash-object", "-w", "--stdin"], stdin=data)
        _plumb(
            source,
            ["update-index", "--add", "--cacheinfo", f"{mode},{blob},{path}"],
            env=env,
        )
    tree = _plumb(source, ["write-tree"], env=env)
    final_sha = _plumb(
        source, ["commit-tree", tree, "-p", base_sha, "-m", "work"], env=env
    )
    _plumb(source, ["update-ref", f"refs/heads/{WORK_BRANCH}", final_sha])
    index.unlink(missing_ok=True)
    bundle = create_return_bundle(source, branch=WORK_BRANCH, base_sha=base_sha)
    return Returned(
        source=source,
        local=local,
        base_sha=base_sha,
        final_sha=final_sha,
        bundle=bundle,
    )


def archive_for(
    returned: Returned,
    *,
    tests: tuple[ReportedTest, ...] = (ReportedTest("unit", "passed"),),
    complete: bool = True,
    reason: str | None = None,
    limits: ArchiveLimits = ArchiveLimits(),
    bundle: bytes | None = None,
    base_sha: str | None = None,
    final_sha: str | None = None,
    logs: tuple[str, ...] = ("ran tests\n",),
) -> ResultArchive:
    subject = ArchiveSubject(
        run_id=RUN_ID,
        attempt=1,
        fence=1,
        claim_id=CLAIM_ID,
        repository=REPOSITORY,
        base_sha=base_sha or returned.base_sha,
        final_sha=final_sha or returned.final_sha,
        image_digest=IMAGE_DIGEST,
        cli_version="cli-1.0",
        model="model-a",
        profile="standard-4vcpu-16gib",
    )
    return build_archive(
        subject,
        return_bundle=returned.bundle if bundle is None else bundle,
        agent_final=b'{"status":"done"}',
        tests=tests,
        log_lines=logs,
        complete=complete,
        incomplete_reason=reason,
        limits=limits,
    )


def running_storage(*, state: RunState = RunState.RUNNING) -> InMemoryStorage:
    storage = InMemoryStorage()
    storage.seed_run(run_record(state=state, claim_id=CLAIM_ID))
    return storage


def result_document(
    *,
    outcome: str = "succeeded",
    artifacts: tuple[object, ...] = (),
    commit_sha: str | None = None,
    attempt: int = 1,
    fence: int = 1,
) -> RunResultDocument:
    return RunResultDocument(
        protocol_version="v1",
        run_id=RUN_ID,
        attempt=attempt,
        fence=fence,
        claim_id=CLAIM_ID,
        outcome=outcome,  # type: ignore[arg-type]
        exit_code=0 if outcome == "succeeded" else 1,
        commit_sha=commit_sha,
        artifacts=tuple(artifacts),  # type: ignore[arg-type]
        redacted=True,
    )


def publish_archive(
    storage: InMemoryStorage,
    archive: ResultArchive,
    *,
    document: RunResultDocument | None = None,
) -> RunRecord:
    """The runner's path: store the archive objects, then post the result through the gate."""
    store_archive(storage, archive)
    if document is None:
        document = result_document(
            commit_sha=archive.subject.final_sha, artifacts=archive.refs
        )
    return accept_archived_result(storage, document, now=NOW)


class InjectedFault(Exception):
    """Raised by a fault hook at one named boundary. Never a production error."""


def fault_at(name: str) -> Callable[[str], None]:
    """A boundary hook that raises once, at the named boundary."""
    fired: list[str] = []

    def hook(boundary: str) -> None:
        if boundary == name and not fired:
            fired.append(boundary)
            raise InjectedFault(boundary)

    return hook


def linked_worktrees(repo: Path) -> list[str]:
    lines = git(repo, "worktree", "list", "--porcelain").splitlines()
    paths = [
        line.removeprefix("worktree ") for line in lines if line.startswith("worktree ")
    ]
    return paths[1:]


def refs_under(repo: Path, prefix: str) -> list[str]:
    return git(repo, "for-each-ref", "--format=%(refname)", prefix).split()
