"""DPES-817 local import: fetch a verified result archive into an isolated review worktree.

Order of work:
1. Read-only checks against storage: owner, attempt, run state, the stored result, its archive,
   repository, base, and the bundle header. Nothing local changes yet.
2. Fetch the returned objects into the local object store with no refs written, then check the
   tree against the destination profile. Refusals so far leave no refs and no worktrees.
3. Under a non-blocking project lock, write an ownership journal before any ref or worktree exists,
   then create a staging ref, a branch, and a worktree, check the materialized tree, write the
   receipt, and release the staging ref.

A repeat fetch reads the journal, so it reconciles exactly what this import owns. Anything it cannot
prove it owns is refused and left in place.
"""

import fcntl
import hashlib
import json
import os
import re
import stat
import subprocess
import tempfile
from collections.abc import Callable, Iterator
from contextlib import contextmanager
from dataclasses import dataclass
from pathlib import Path
from typing import Literal

from pydantic import BaseModel, ConfigDict, ValidationError

from mao_cloud.protocol import (
    ObjectKind,
    RunResultDocument,
    RunState,
    artifact_object_name,
    object_name,
)
from mao_cloud.result_paths import DestinationProfile, TreeEntry, validate_destination
from mao_cloud.results import (
    MANIFEST_NAME,
    ArchiveLimits,
    ParsedArchive,
    ResultRefused,
    parse_archive,
    parse_return_header,
)
from mao_cloud.storage import Storage

BOUNDARIES: tuple[str, ...] = (
    "lock-held",
    "journal-staged",
    "staging-ref-created",
    "worktree-created",
    "materialized",
    "receipt-written",
    "staging-released",
)
_ZERO_SHA = "0" * 40
_IMPORT_ID = re.compile(r"[0-9a-f]{32}")
_GIT_HOOKS_OFF = "core.hooksPath=/dev/null"
_LOCK_DIR = ("mao-import",)


def branch_name(run_id: str, attempt: int, import_id: str) -> str:
    return f"cloud-import/{run_id}-a{attempt}-{import_id}"


def worktree_name(run_id: str, attempt: int, import_id: str) -> str:
    return f"{run_id}-a{attempt}-{import_id}"


def staging_ref(import_id: str) -> str:
    return f"refs/mao-import/{import_id}"


@dataclass(frozen=True)
class ImportRequest:
    owner_id: str
    run_id: str
    attempt: int
    repository: str
    base_sha: str
    destination_root: Path
    fs_profile: DestinationProfile
    limits: ArchiveLimits = ArchiveLimits()
    import_id: str | None = None


class ImportReceipt(BaseModel):
    """What the import produced. Equal receipts mean the same owned import."""

    model_config = ConfigDict(extra="forbid", frozen=True)

    status: Literal["complete", "partial"]
    reason: str | None
    import_id: str
    run_id: str
    attempt: int
    base_sha: str
    final_sha: str
    branch: str
    worktree: str
    archive_manifest_sha256: str
    tests_source: Literal["self-reported"]
    independent_verification: Literal[False]


class ImportJournal(BaseModel):
    """Written before any ref or worktree exists. It is the proof of what this import owns."""

    model_config = ConfigDict(extra="forbid", frozen=True)

    import_id: str
    owner_id: str
    run_id: str
    attempt: int
    repository: str
    base_sha: str
    final_sha: str
    branch: str
    worktree: str
    fs_profile: str
    archive_manifest_sha256: str


@dataclass(frozen=True)
class _Remote:
    document: RunResultDocument
    parsed: ParsedArchive
    tip: str
    ref: str
    manifest_sha256: str


def lock_path(local: Path) -> Path:
    common = Path(_run(local, "rev-parse", "--git-common-dir").decode().strip())
    if not common.is_absolute():
        common = local / common
    return common.resolve().joinpath(*_LOCK_DIR, "import.lock")


def _run(repo: Path, *args: str, config: tuple[str, ...] = ()) -> bytes:
    """Run Git with hooks and fsmonitor off; importing never uses checkout or filters."""
    argv = ["git", "-C", str(repo), "-c", _GIT_HOOKS_OFF, "-c", "core.fsmonitor=false"]
    for item in config:
        argv += ["-c", item]
    argv += list(args)
    try:
        completed = subprocess.run(argv, capture_output=True, check=True)
    except subprocess.CalledProcessError:
        raise ResultRefused(
            "git-failed", f"git {args[0]} failed on the local repository"
        ) from None
    return completed.stdout


def _ref_sha(repo: Path, ref: str) -> str | None:
    completed = subprocess.run(
        [
            "git",
            "-C",
            str(repo),
            "-c",
            _GIT_HOOKS_OFF,
            "-c",
            "core.fsmonitor=false",
            "rev-parse",
            "--verify",
            "--quiet",
            ref,
        ],
        capture_output=True,
        check=False,
    )
    if completed.returncode != 0:
        return None
    return completed.stdout.decode().strip()


def _atomic_write(path: Path, data: bytes) -> None:
    temporary = path.with_name(path.name + ".tmp")
    temporary.write_bytes(data)
    os.replace(temporary, path)


def _derive_import_id(request: ImportRequest, tip: str) -> str:
    material = json.dumps(
        [
            request.owner_id,
            request.run_id,
            request.attempt,
            request.base_sha,
            tip,
            request.fs_profile.name,
        ],
        separators=(",", ":"),
    ).encode()
    return hashlib.sha256(material).hexdigest()[:32]


def _read_remote(storage: Storage, request: ImportRequest) -> _Remote:
    run = storage.read_run(request.run_id)
    if run is None:
        raise ResultRefused("not-found", "the run is not stored")
    if run.owner_id != request.owner_id:
        raise ResultRefused("not-owner", "the run belongs to another owner")
    if request.attempt != run.attempt:
        raise ResultRefused(
            "stale-attempt", "the requested attempt is not the run's current attempt"
        )
    if run.state != RunState.SUCCEEDED:
        raise ResultRefused(
            "not-succeeded", "the run did not succeed, so it has nothing to import"
        )
    raw = storage.read_object(
        object_name(ObjectKind.RESULT, run_id=run.run_id, attempt=request.attempt)
    )
    if raw is None:
        raise ResultRefused("no-result-archive", "the run has no stored result")
    try:
        document = RunResultDocument.model_validate_json(raw)
    except ValidationError:
        raise ResultRefused(
            "no-result-archive", "the stored result does not describe a result archive"
        ) from None
    if (
        document.run_id != run.run_id
        or document.attempt != request.attempt
        or document.outcome != "succeeded"
    ):
        raise ResultRefused(
            "subject-mismatch",
            "the stored result does not describe this succeeded attempt",
        )

    def read(name: str) -> bytes | None:
        return storage.read_object(
            artifact_object_name(run.run_id, request.attempt, name)
        )

    parsed = parse_archive(document, read, limits=request.limits)
    if parsed.subject.repository != request.repository:
        raise ResultRefused(
            "wrong-repository", "the archive belongs to another repository"
        )
    if parsed.subject.base_sha != request.base_sha:
        raise ResultRefused("wrong-base", "the archive was built on another base")
    header = parse_return_header(parsed.return_bundle, base_sha=request.base_sha)
    if header.tip != parsed.subject.final_sha:
        raise ResultRefused(
            "subject-mismatch", "the bundle tip is not the archive's final commit"
        )
    manifest = next(ref for ref in document.artifacts if ref.name == MANIFEST_NAME)
    return _Remote(
        document=document,
        parsed=parsed,
        tip=header.tip,
        ref=header.ref,
        manifest_sha256=manifest.sha256,
    )


def _fetch_objects(local: Path, bundle: bytes, tip: str, ref: str) -> None:
    """Put the returned objects in the local store. No refs, no FETCH_HEAD."""
    with tempfile.TemporaryDirectory(prefix="mao-import-") as scratch:
        path = Path(scratch) / "return.bundle"
        path.write_bytes(bundle)
        _run(
            local,
            "fetch",
            "--quiet",
            "--no-write-fetch-head",
            "--no-tags",
            "--no-recurse-submodules",
            str(path),
            ref,
            config=("transfer.fsckObjects=true", "fetch.fsckObjects=true"),
        )
    _run(local, "cat-file", "-e", f"{tip}^{{commit}}")


def _tree_entries(local: Path, tip: str) -> list[TreeEntry]:
    listing = _run(local, "ls-tree", "-r", "-z", "--full-tree", tip)
    entries: list[TreeEntry] = []
    for record in listing.split(b"\0"):
        if not record:
            continue
        meta, _, raw_path = record.partition(b"\t")
        mode = meta.split()[0].decode("ascii")
        try:
            path = raw_path.decode("utf-8")
        except UnicodeDecodeError:
            raise ResultRefused("unsafe-path", "a returned path is not UTF-8") from None
        entries.append(TreeEntry(path=path, mode=mode))
    return entries


def _worktree_files(root: Path) -> set[str]:
    """Every file and symlink under the worktree, except the root .git pointer. Links are not followed."""
    found: set[str] = set()
    for dirpath, dirnames, filenames in os.walk(root):
        here = Path(dirpath)
        for name in dirnames:
            if (here / name).is_symlink():
                found.add((here / name).relative_to(root).as_posix())
        for name in filenames:
            if here == root and name == ".git":
                continue
            found.add((here / name).relative_to(root).as_posix())
    return found


def _check_materialized(
    worktree: Path, tip: str, entries: list[TreeEntry], code: str
) -> None:
    head = _run(worktree, "rev-parse", "HEAD").decode().strip()
    if head != tip:
        raise ResultRefused(code, "the worktree is not at the returned tip")
    index_tree = _run(worktree, "write-tree").strip()
    if index_tree != _run(worktree, "rev-parse", f"{tip}^{{tree}}").strip():
        raise ResultRefused(code, "the worktree index differs from the returned tree")
    if _worktree_files(worktree) != {entry.path for entry in entries}:
        raise ResultRefused(code, "the worktree holds files the returned tree does not")
    for entry in entries:
        path = worktree / entry.path
        mode = path.lstat().st_mode
        expected = _run(worktree, "cat-file", "blob", f"{tip}:{entry.path}")
        if entry.mode == "120000":
            if not stat.S_ISLNK(mode) or os.readlink(os.fsencode(path)) != expected:
                raise ResultRefused(code, "a returned symlink's type or target differs")
        elif (
            not stat.S_ISREG(mode)
            or bool(mode & 0o111) != (entry.mode == "100755")
            or path.read_bytes() != expected
        ):
            raise ResultRefused(code, "a returned file's type, mode, or content differs")


def _materialize_raw(worktree: Path, tip: str, entries: list[TreeEntry]) -> None:
    """Populate the index and write raw blobs without invoking attributes or external filters."""
    _run(worktree, "read-tree", tip)
    for entry in entries:
        path = worktree / entry.path
        path.parent.mkdir(parents=True, exist_ok=True)
        data = _run(worktree, "cat-file", "blob", f"{tip}:{entry.path}")
        if entry.mode == "120000":
            os.symlink(data, os.fsencode(path))
        else:
            with path.open("xb") as handle:
                handle.write(data)
            path.chmod(0o755 if entry.mode == "100755" else 0o644)


def _registered_worktrees(local: Path) -> set[Path]:
    listing = _run(local, "worktree", "list", "--porcelain").decode()
    return {
        Path(line.removeprefix("worktree ")).resolve()
        for line in listing.splitlines()
        if line.startswith("worktree ")
    }


def _ensure_worktree(
    local: Path,
    worktree: Path,
    branch: str,
    tip: str,
    entries: list[TreeEntry],
    hook: Callable[[str], None],
) -> bool:
    """Create the worktree, or prove an existing one is ours. Returns True when this call created it."""
    branch_sha = _ref_sha(local, f"refs/heads/{branch}")
    if not os.path.lexists(worktree):
        if branch_sha is None:
            _run(
                local, "worktree", "add", "--no-checkout", "--quiet", "-b", branch,
                str(worktree), tip,
            )
        elif branch_sha == tip:
            _run(
                local, "worktree", "add", "--no-checkout", "--quiet", str(worktree), branch
            )
        else:
            raise ResultRefused(
                "unknown-partial",
                "the import branch points somewhere this import did not put it",
            )
        _materialize_raw(worktree, tip, entries)
        hook("worktree-created")
        return True
    if worktree.resolve() not in _registered_worktrees(local):
        raise ResultRefused(
            "unknown-partial",
            "the destination exists but is not a worktree this import owns",
        )
    head_branch = _run(worktree, "symbolic-ref", "-q", "HEAD").decode().strip()
    if head_branch != f"refs/heads/{branch}":
        raise ResultRefused(
            "unknown-partial", "the owned worktree is not on its import branch"
        )
    return False


def _ensure_staging_ref(
    local: Path, import_id: str, tip: str, hook: Callable[[str], None]
) -> None:
    ref = staging_ref(import_id)
    current = _ref_sha(local, ref)
    if current is None:
        _run(local, "update-ref", ref, tip, _ZERO_SHA)
        hook("staging-ref-created")
    elif current != tip:
        raise ResultRefused(
            "unknown-partial",
            "the staging ref points somewhere this import did not put it",
        )


def _release_staging_ref(local: Path, import_id: str, tip: str) -> None:
    ref = staging_ref(import_id)
    if _ref_sha(local, ref) is not None:
        _run(local, "update-ref", "-d", ref, tip)


@contextmanager
def _import_lock(local: Path, hook: Callable[[str], None]) -> Iterator[None]:
    path = lock_path(local)
    path.parent.mkdir(parents=True, exist_ok=True)
    with open(path, "a+b") as handle:
        try:
            fcntl.flock(handle.fileno(), fcntl.LOCK_EX | fcntl.LOCK_NB)
        except BlockingIOError:
            raise ResultRefused(
                "import-busy", "another import is running in this checkout"
            ) from None
        hook("lock-held")
        yield


def fetch_and_import(
    storage: Storage,
    local: Path,
    request: ImportRequest,
    *,
    boundary: Callable[[str], None] | None = None,
) -> ImportReceipt:
    """Import the run's verified result into its own worktree. Repeats reconcile instead of duplicating."""
    hook = boundary or (lambda _name: None)
    remote = _read_remote(storage, request)
    _fetch_objects(local, remote.parsed.return_bundle, remote.tip, remote.ref)
    entries = _tree_entries(local, remote.tip)
    validate_destination(entries, request.fs_profile)

    if request.import_id is None:
        import_id = _derive_import_id(request, remote.tip)
    elif _IMPORT_ID.fullmatch(request.import_id):
        import_id = request.import_id
    else:
        raise ResultRefused(
            "bad-import-id", "the import id is not a 32-character lowercase hex string"
        )

    branch = branch_name(request.run_id, request.attempt, import_id)
    worktree = request.destination_root.resolve() / worktree_name(
        request.run_id, request.attempt, import_id
    )
    expected = ImportJournal(
        import_id=import_id,
        owner_id=request.owner_id,
        run_id=request.run_id,
        attempt=request.attempt,
        repository=request.repository,
        base_sha=request.base_sha,
        final_sha=remote.tip,
        branch=branch,
        worktree=str(worktree),
        fs_profile=request.fs_profile.name,
        archive_manifest_sha256=remote.manifest_sha256,
    )
    journal_dir = lock_path(local).parent
    journal_path = journal_dir / f"{import_id}.journal.json"
    receipt_path = journal_dir / f"{import_id}.receipt.json"

    with _import_lock(local, hook):
        if receipt_path.exists():
            return _published(local, expected, journal_path, receipt_path, remote)
        if journal_path.exists():
            journal = ImportJournal.model_validate_json(journal_path.read_bytes())
            if journal != expected:
                raise ResultRefused(
                    "journal-mismatch",
                    "the journal for this import id belongs to another import",
                )
            created_now = False
        else:
            _refuse_unowned(local, import_id, branch, worktree)
            journal_dir.mkdir(parents=True, exist_ok=True)
            request.destination_root.mkdir(parents=True, exist_ok=True)
            _atomic_write(journal_path, expected.model_dump_json().encode())
            hook("journal-staged")
            created_now = True

        _ensure_staging_ref(local, import_id, remote.tip, hook)
        created = _ensure_worktree(local, worktree, branch, remote.tip, entries, hook)
        code = "materialized-mismatch" if created_now and created else "unknown-partial"
        _check_materialized(worktree, remote.tip, entries, code)
        hook("materialized")

        receipt = _receipt(expected, remote)
        _atomic_write(receipt_path, receipt.model_dump_json().encode())
        hook("receipt-written")
        _release_staging_ref(local, import_id, remote.tip)
        hook("staging-released")
        return receipt


def _refuse_unowned(local: Path, import_id: str, branch: str, worktree: Path) -> None:
    """A fresh import may not start over anything it has no journal for."""
    if _ref_sha(local, f"refs/heads/{branch}") is not None:
        raise ResultRefused("branch-exists", "the import branch name is already taken")
    if os.path.lexists(worktree):
        raise ResultRefused(
            "dirty-destination",
            "the destination already exists and this import does not own it",
        )
    if _ref_sha(local, staging_ref(import_id)) is not None:
        raise ResultRefused(
            "unknown-partial", "a staging ref exists with no journal for this import"
        )


def _published(
    local: Path,
    expected: ImportJournal,
    journal_path: Path,
    receipt_path: Path,
    remote: _Remote,
) -> ImportReceipt:
    if not journal_path.exists():
        raise ResultRefused(
            "unknown-partial", "a receipt exists with no journal for this import"
        )
    journal = ImportJournal.model_validate_json(journal_path.read_bytes())
    if journal != expected:
        raise ResultRefused(
            "journal-mismatch",
            "the journal for this import id belongs to another import",
        )
    receipt = ImportReceipt.model_validate_json(receipt_path.read_bytes())
    if not Path(receipt.worktree).is_dir():
        raise ResultRefused(
            "published-worktree-missing",
            "the published worktree was removed; it is not recreated",
        )
    _release_staging_ref(local, expected.import_id, remote.tip)
    return receipt


def _receipt(journal: ImportJournal, remote: _Remote) -> ImportReceipt:
    complete = remote.parsed.complete
    return ImportReceipt(
        status="complete" if complete else "partial",
        reason=None if complete else remote.parsed.incomplete_reason,
        import_id=journal.import_id,
        run_id=journal.run_id,
        attempt=journal.attempt,
        base_sha=journal.base_sha,
        final_sha=journal.final_sha,
        branch=journal.branch,
        worktree=journal.worktree,
        archive_manifest_sha256=journal.archive_manifest_sha256,
        tests_source="self-reported",
        independent_verification=False,
    )
