"""DPES-817 result archive: the runner's versioned archive, the return bundle header, the controller gate, and the runner's checkpoint of dirty files.

The archive is one manifest plus four artifacts, all stored under the attempt. The manifest lists the four and is itself listed by the result document, so the create-only RESULT object checksums every part. The RESULT object stores the canonical result document, so the fetch side reads the authoritative artifact list from storage.
"""

import json
import os
import re
import subprocess
import tempfile
from collections.abc import Callable, Sequence
from dataclasses import dataclass
from datetime import datetime
from pathlib import Path
from typing import NamedTuple

from mao_cloud.checkpoint_publish import publish_result
from mao_cloud.protocol import (
    MAX_CHECKPOINT_BYTES,
    MAX_RETURNED_WORK_BYTES,
    ArtifactRef,
    ErrorCode,
    ProtocolError,
    RunRecord,
    RunResultDocument,
    accept_result,
    artifact_object_name,
    redact,
    sha256_hex,
    verify_artifacts,
)
from mao_cloud.storage import Storage, StorageConflict

AGENT_FINAL_NAME = "agent-final.json"
LOG_NAME = "run.log"
MANIFEST_NAME = "manifest.json"
RETURN_BUNDLE_NAME = "return.bundle"
TESTS_NAME = "tests.json"
ARCHIVE_SCHEMA = "mao-result-archive/v1"
REQUIRED_ARTIFACTS = frozenset(
    {AGENT_FINAL_NAME, LOG_NAME, MANIFEST_NAME, RETURN_BUNDLE_NAME, TESTS_NAME}
)

_BUNDLE_HEADER = b"# v2 git bundle\n"
_SHA1 = re.compile(r"[0-9a-f]{40}")


class ResultRefused(Exception):
    """A typed refusal naming one check. The code is the contract; the message never echoes untrusted bytes."""

    def __init__(self, code: str, message: str) -> None:
        super().__init__(message)
        self.code = code
        self.message = message


@dataclass(frozen=True)
class ArchiveLimits:
    """Fetch and build limits. The returned-work cap is versioned in the protocol, separate from the result document."""

    max_returned_work_bytes: int = MAX_RETURNED_WORK_BYTES


@dataclass(frozen=True)
class CheckpointLimits:
    max_files: int = 10_000
    max_bytes: int = MAX_CHECKPOINT_BYTES


class ReportedTest(NamedTuple):
    """A test result the runner reported about itself. It is never independent verification."""

    name: str
    status: str


@dataclass(frozen=True)
class ArchiveSubject:
    run_id: str
    attempt: int
    fence: int
    claim_id: str
    repository: str
    base_sha: str
    final_sha: str
    image_digest: str
    cli_version: str
    model: str
    profile: str


@dataclass(frozen=True)
class ResultArchive:
    subject: ArchiveSubject
    objects: dict[str, bytes]
    refs: tuple[ArtifactRef, ...]


@dataclass(frozen=True)
class ParsedArchive:
    subject: ArchiveSubject
    return_bundle: bytes
    complete: bool
    incomplete_reason: str | None
    independent_verification: bool
    tests: tuple[ReportedTest, ...]


@dataclass(frozen=True)
class ReturnHeader:
    prerequisites: tuple[str, ...]
    ref: str
    tip: str


@dataclass(frozen=True)
class DirtyEntry:
    path: str
    absolute: Path
    present: bool
    size_bytes: int


@dataclass(frozen=True)
class CheckpointPlan:
    """The exact paths a checkpoint commits. Present paths are staged; removed paths are unstaged."""

    present: tuple[str, ...]
    removed: tuple[str, ...]

    @property
    def paths(self) -> tuple[str, ...]:
        return self.present + self.removed


def _json(data: object) -> bytes:
    return json.dumps(data, sort_keys=True, separators=(",", ":")).encode()


def _redacted_utf8(data: bytes) -> bytes:
    try:
        text = data.decode("utf-8")
    except UnicodeDecodeError:
        raise ResultRefused("unscannable", "agent final message is not UTF-8") from None
    return redact(text).text.encode()


def build_archive(
    subject: ArchiveSubject,
    *,
    return_bundle: bytes,
    agent_final: bytes,
    tests: Sequence[ReportedTest],
    log_lines: Sequence[str],
    complete: bool,
    incomplete_reason: str | None,
    limits: ArchiveLimits,
) -> ResultArchive:
    """Build the runner's archive. Logs and the agent message are redacted before they are stored."""
    if len(return_bundle) > limits.max_returned_work_bytes:
        raise ResultRefused(
            "returned-work-too-large", "returned work exceeds its own cap"
        )
    if not complete and not incomplete_reason:
        raise ValueError("an incomplete archive must say why")
    log_text = "".join(
        redact(line).text if line.endswith("\n") else redact(line).text + "\n"
        for line in log_lines
    )
    tests_block = {
        "source": "self-reported",
        "outcomes": [{"name": test.name, "status": test.status} for test in tests],
    }
    objects: dict[str, bytes] = {
        RETURN_BUNDLE_NAME: return_bundle,
        AGENT_FINAL_NAME: _redacted_utf8(agent_final),
        LOG_NAME: log_text.encode(),
        TESTS_NAME: _json(tests_block),
    }
    listed = [
        {"name": name, "sha256": sha256_hex(data), "size_bytes": len(data)}
        for name, data in objects.items()
    ]
    manifest = {
        "schema": ARCHIVE_SCHEMA,
        "run_id": subject.run_id,
        "attempt": subject.attempt,
        "fence": subject.fence,
        "claim_id": subject.claim_id,
        "repository": subject.repository,
        "base_sha": subject.base_sha,
        "final_sha": subject.final_sha,
        "image_digest": subject.image_digest,
        "cli_version": subject.cli_version,
        "model": subject.model,
        "profile": subject.profile,
        "complete": complete,
        "incomplete_reason": incomplete_reason,
        "artifacts": listed,
        "tests": tests_block,
        "independent_verification": False,
    }
    objects[MANIFEST_NAME] = _json(manifest)
    refs = tuple(
        ArtifactRef(name=name, sha256=sha256_hex(data), size_bytes=len(data))
        for name, data in objects.items()
    )
    return ResultArchive(subject=subject, objects=objects, refs=refs)


def store_archive(storage: Storage, archive: ResultArchive) -> None:
    """Create each archive object once. Re-storing identical bytes is idempotent; different bytes refuse."""
    for name, data in archive.objects.items():
        target = artifact_object_name(
            archive.subject.run_id, archive.subject.attempt, name
        )
        try:
            storage.create_object(target, data)
        except StorageConflict:
            if storage.read_object(target) != data:
                raise ResultRefused(
                    "archive-conflict", "a stored artifact differs from this archive"
                ) from None


def parse_return_header(bundle: bytes, *, base_sha: str) -> ReturnHeader:
    """Read the bundle header without git. It must name exactly one branch and exactly the recorded base."""
    if not bundle.startswith(_BUNDLE_HEADER):
        raise ResultRefused("bad-header", "the returned work is not a v2 git bundle")
    prerequisites: list[str] = []
    refs: list[tuple[str, str]] = []
    position = len(_BUNDLE_HEADER)
    while True:
        end = bundle.find(b"\n", position)
        if end < 0:
            raise ResultRefused("bad-header", "the bundle header is truncated")
        raw_line = bundle[position:end]
        if raw_line.startswith(b"-"):
            try:
                sha = raw_line[1:].partition(b" ")[0].decode("ascii")
            except UnicodeDecodeError:
                raise ResultRefused(
                    "bad-header", "the bundle prerequisite line is malformed"
                ) from None
            if not _SHA1.fullmatch(sha):
                raise ResultRefused(
                    "bad-header", "the bundle prerequisite line is malformed"
                )
            prerequisites.append(sha)
            position = end + 1
            continue
        try:
            line = raw_line.decode("ascii")
        except UnicodeDecodeError:
            raise ResultRefused(
                "bad-header", "the bundle header is not ASCII"
            ) from None
        position = end + 1
        if line == "":
            break
        if line.startswith("@"):
            raise ResultRefused(
                "bad-header",
                "the bundle declares capabilities this worker does not accept",
            )
        sha, separator, ref = line.partition(" ")
        if (
            not separator
            or not _SHA1.fullmatch(sha)
            or not ref.startswith("refs/heads/")
        ):
            raise ResultRefused("bad-header", "the bundle ref line is malformed")
        refs.append((sha, ref))
    if len(refs) != 1:
        raise ResultRefused("bad-header", "the bundle must name exactly one branch ref")
    if tuple(prerequisites) != (base_sha,):
        raise ResultRefused(
            "wrong-base", "the bundle is not built on the recorded base"
        )
    tip, ref = refs[0]
    return ReturnHeader(prerequisites=tuple(prerequisites), ref=ref, tip=tip)


def _bad_manifest() -> ResultRefused:
    return ResultRefused("bad-manifest", "the result archive manifest is malformed")


def parse_archive(
    document: RunResultDocument,
    read: Callable[[str], bytes | None],
    *,
    limits: ArchiveLimits,
) -> ParsedArchive:
    """Verify the result's artifacts, then read the archive they describe.

    read takes a relative artifact name and returns its bytes, or None when absent.
    """
    try:
        verify_artifacts(document, read)
    except ProtocolError:
        raise ResultRefused(
            "checksum-mismatch",
            "a result artifact does not match its recorded checksum",
        ) from None
    if not REQUIRED_ARTIFACTS <= {ref.name for ref in document.artifacts}:
        raise ResultRefused(
            "no-result-archive", "the result names no complete result archive"
        )
    bundle = read(RETURN_BUNDLE_NAME)
    if bundle is None:
        raise ResultRefused(
            "no-result-archive", "the result archive lacks its returned work"
        )
    if len(bundle) > limits.max_returned_work_bytes:
        raise ResultRefused(
            "returned-work-too-large", "returned work exceeds its own cap"
        )
    manifest_bytes = read(MANIFEST_NAME)
    tests_bytes = read(TESTS_NAME)
    if manifest_bytes is None or tests_bytes is None:
        raise ResultRefused(
            "no-result-archive", "the result archive lacks its manifest"
        )
    try:
        manifest = json.loads(manifest_bytes)
        tests_block = manifest["tests"]
        tests_artifact = json.loads(tests_bytes)
        if (
            manifest["schema"] != ARCHIVE_SCHEMA
            or manifest["independent_verification"] is not False
        ):
            raise _bad_manifest()
        if (
            tests_block.get("source") != "self-reported"
            or tests_artifact != tests_block
        ):
            raise _bad_manifest()
        complete = manifest["complete"]
        incomplete_reason = manifest["incomplete_reason"]
        if not isinstance(complete, bool):
            raise _bad_manifest()
        subject = ArchiveSubject(
            run_id=manifest["run_id"],
            attempt=manifest["attempt"],
            fence=manifest["fence"],
            claim_id=manifest["claim_id"],
            repository=manifest["repository"],
            base_sha=manifest["base_sha"],
            final_sha=manifest["final_sha"],
            image_digest=manifest["image_digest"],
            cli_version=manifest["cli_version"],
            model=manifest["model"],
            profile=manifest["profile"],
        )
        listed = {entry["name"]: entry for entry in manifest["artifacts"]}
        outcomes = tuple(
            ReportedTest(name=entry["name"], status=entry["status"])
            for entry in tests_block["outcomes"]
        )
    except (KeyError, TypeError, ValueError, AttributeError):
        raise _bad_manifest() from None
    if set(listed) != REQUIRED_ARTIFACTS - {MANIFEST_NAME}:
        raise _bad_manifest()
    declared = {ref.name: ref for ref in document.artifacts}
    for name, entry in listed.items():
        ref = declared[name]
        if (
            entry.get("sha256") != ref.sha256
            or entry.get("size_bytes") != ref.size_bytes
        ):
            raise ResultRefused(
                "subject-mismatch",
                "the manifest and the result disagree on an artifact",
            )
    if (
        subject.run_id != document.run_id
        or subject.attempt != document.attempt
        or subject.fence != document.fence
        or subject.claim_id != document.claim_id
        or subject.final_sha != document.commit_sha
    ):
        raise ResultRefused(
            "subject-mismatch", "the archive does not describe this result"
        )
    return ParsedArchive(
        subject=subject,
        return_bundle=bundle,
        complete=complete,
        incomplete_reason=incomplete_reason,
        independent_verification=False,
        tests=outcomes,
    )


def create_return_bundle(source: Path, *, branch: str, base_sha: str) -> bytes:
    """Bundle the branch's commits above base. The bundle names base as its one prerequisite."""
    with tempfile.TemporaryDirectory(prefix="mao-return-") as scratch:
        target = Path(scratch) / RETURN_BUNDLE_NAME
        subprocess.run(
            [
                "git",
                "-C",
                str(source),
                "bundle",
                "create",
                str(target),
                f"refs/heads/{branch}",
                f"^{base_sha}",
            ],
            check=True,
            capture_output=True,
        )
        return target.read_bytes()


def accept_archived_result(
    storage: Storage,
    document: RunResultDocument,
    *,
    now: datetime,
) -> RunRecord:
    """The controller gate. Success needs its complete archive verified in storage; failure needs none.

    Identity, terminal state, fence, and deadline are checked before any artifact is read.
    """
    record = storage.read_run(document.run_id)
    if record is None:
        raise ProtocolError(ErrorCode.NOT_FOUND, "run is not stored")
    accept_result(record, document, now=now)
    if document.outcome == "succeeded":
        if not REQUIRED_ARTIFACTS <= {ref.name for ref in document.artifacts}:
            raise ProtocolError(
                ErrorCode.INVALID_REQUEST,
                "a succeeded result must name its result archive",
            )
        try:
            archive = parse_archive(
                document,
                lambda name: storage.read_object(
                    artifact_object_name(document.run_id, document.attempt, name)
                ),
                limits=ArchiveLimits(),
            )
            header = parse_return_header(
                archive.return_bundle, base_sha=archive.subject.base_sha
            )
            if header.tip != archive.subject.final_sha:
                raise ResultRefused(
                    "subject-mismatch", "the bundle does not name the archive's final commit"
                )
        except ResultRefused as error:
            code = (
                ErrorCode.CHECKSUM_MISMATCH
                if error.code == "checksum-mismatch"
                else ErrorCode.INVALID_REQUEST
            )
            raise ProtocolError(code, error.message) from None
    return publish_result(
        storage, document, body=document.model_dump_json().encode(), now=now
    )


def _git(repo: Path, *args: str) -> bytes:
    completed = subprocess.run(
        [
            "git",
            "--literal-pathspecs",
            "-C",
            str(repo),
            "-c",
            "core.hooksPath=/dev/null",
            *args,
        ],
        capture_output=True,
        check=True,
    )
    return completed.stdout


def gather_dirty_entries(repo: Path) -> list[DirtyEntry]:
    """Every changed, non-ignored path. Renames appear as a delete and an add, never as one pair."""
    output = _git(
        repo, "status", "--porcelain=v1", "-z", "--no-renames", "--untracked-files=all"
    )
    entries: list[DirtyEntry] = []
    for record in output.split(b"\0"):
        if not record:
            continue
        path = os.fsdecode(record[3:])
        absolute = repo / path
        present = os.path.lexists(absolute)
        size = absolute.lstat().st_size if present else 0
        entries.append(
            DirtyEntry(path=path, absolute=absolute, present=present, size_bytes=size)
        )
    return entries


def _content(entry: DirtyEntry) -> bytes:
    if entry.absolute.is_symlink():
        return os.fsencode(os.readlink(entry.absolute))
    return entry.absolute.read_bytes()


def plan_checkpoint(
    entries: Sequence[DirtyEntry], *, limits: CheckpointLimits
) -> CheckpointPlan:
    """Refuse the whole checkpoint if it is too large, unscannable, or holds a suspected credential."""
    if len(entries) > limits.max_files:
        raise ResultRefused(
            "too-many-files", "the checkpoint changes more files than its limit"
        )
    if sum(entry.size_bytes for entry in entries) > limits.max_bytes:
        raise ResultRefused(
            "checkpoint-too-large", "the checkpoint is larger than its limit"
        )
    for entry in entries:
        if not entry.present:
            continue
        try:
            text = _content(entry).decode("utf-8")
        except UnicodeDecodeError:
            raise ResultRefused(
                "unscannable", f"{entry.path} is not UTF-8 text and cannot be scanned"
            ) from None
        if redact(text).count:
            raise ResultRefused(
                "secret-suspected", f"{entry.path} looks like it holds a credential"
            )
    return CheckpointPlan(
        present=tuple(entry.path for entry in entries if entry.present),
        removed=tuple(entry.path for entry in entries if not entry.present),
    )


# A checkpoint is a machine commit. It names its own identity instead of inheriting host git config,
# which a bare container or CI runner does not have.
_CHECKPOINT_IDENTITY = ("-c", "user.name=mao-cloud", "-c", "user.email=mao-cloud@invalid")


def commit_checkpoint(repo: Path, plan: CheckpointPlan, *, message: str) -> str:
    """Commit exactly the planned paths. Hooks are off; an empty plan commits nothing.

    git add refuses a pathspec that matches nothing, so removed paths are unstaged with --ignore-unmatch.
    commit --only then commits only the planned paths and leaves any other staged change alone.
    """
    if not plan.paths:
        return _git(repo, "rev-parse", "HEAD").decode().strip()
    if plan.present:
        _git(repo, "add", "-A", "--", *plan.present)
    if plan.removed:
        _git(repo, "rm", "-q", "--cached", "--ignore-unmatch", "--", *plan.removed)
    _git(
        repo,
        *_CHECKPOINT_IDENTITY,
        "commit",
        "--only",
        "--no-verify",
        "-m",
        message,
        "--",
        *plan.paths,
    )
    return _git(repo, "rev-parse", "HEAD").decode().strip()
