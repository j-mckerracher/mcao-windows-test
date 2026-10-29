"""Export one committed branch as the bundle and archive the worker intake accepts.

The export reads committed history only. A dirty or untracked tree, a symlink, a gitlink, Git LFS,
a secret-shaped string in the tip tree, or an archive over the source limit is refused before any
network call. Every export is then verified with the worker's own intake code, so the local verdict
and the worker's verdict cannot diverge.

Disclosure: the bundle carries the branch's full history, because a bundle with prerequisites is
refused by the worker. The secret scan covers the tip tree only; history is not scanned.
"""

import hashlib
import io
import os
import re
import subprocess
import tarfile
import tempfile
from dataclasses import dataclass
from pathlib import Path

from mao_cloud.trusted_bundle import (
    SOURCE_MEMBER,
    BaseMismatch,
    BundleLimits,
    BundleRefused,
    extract_source,
    materialize_checkout,
)


class SourceRefused(Exception):
    """The branch cannot be exported as it stands. The message says what to change."""


class SourceUnreadable(Exception):
    """The path is not a readable git repository."""


# Secret-shaped strings searched in the tip tree. A refusal reports the path and the pattern name;
# the matched text is never echoed.
SECRET_PATTERNS: tuple[tuple[str, str], ...] = (
    ("private-key", r"-----BEGIN [A-Z ]*PRIVATE KEY"),
    ("aws-access-key-id", r"AKIA[0-9A-Z]{16}"),
    ("github-token", r"gh[pousr]_[A-Za-z0-9]{36,}"),
    ("google-api-key", r"AIza[0-9A-Za-z_-]{35}"),
    ("slack-token", r"xox[baprs]-[A-Za-z0-9-]{10,}"),
)

_BRANCH_NAME = re.compile(r"^[A-Za-z0-9][A-Za-z0-9._/-]{0,199}$")
_RAW_SHA = re.compile(r"^(?:[0-9a-f]{40}|[0-9a-f]{64})$")
_GIT_ENV_PREFIX = "GIT_"


@dataclass(frozen=True)
class Snapshot:
    """A verified export: the bundle, the archive that carries it, and the digests a run binds to."""

    base_sha: str
    bundle: bytes
    archive: bytes
    source_digest: str
    archive_sha256: str
    archive_size: int


def export_snapshot(repo: Path, ref: str, *, limits: BundleLimits = BundleLimits()) -> Snapshot:
    """Export the committed branch ref from repo, or raise SourceRefused with the reason."""
    _require_repository(repo)
    full_ref = _resolve_branch(repo, ref)
    base_sha = _git(repo, "rev-parse", "--verify", full_ref).strip()
    _refuse_tree_policy(repo, base_sha)
    _refuse_dirty_tree(repo)
    _refuse_secrets(repo, base_sha)
    with tempfile.TemporaryDirectory(prefix="mao-export-") as scratch:
        bundle_path = Path(scratch) / SOURCE_MEMBER
        _git(repo, "bundle", "create", str(bundle_path), full_ref)
        if bundle_path.stat().st_size > limits.max_bundle_bytes:
            raise SourceRefused(
                f"the source archive exceeds the source limit of {limits.max_bundle_bytes} bytes"
            )
        with bundle_path.open("rb") as handle:
            bundle = handle.read(limits.max_bundle_bytes + 1)
        if len(bundle) > limits.max_bundle_bytes:
            raise SourceRefused(
                f"the source archive exceeds the source limit of {limits.max_bundle_bytes} bytes"
            )
    archive = _archive(bundle)
    if len(archive) > limits.max_bundle_bytes:
        raise SourceRefused(
            f"the source archive exceeds the source limit of {limits.max_bundle_bytes} bytes; "
            f"this branch packs to {len(archive)} bytes"
        )
    _verify_like_worker(archive, limits, expected_tip=base_sha)
    return Snapshot(
        base_sha=base_sha,
        bundle=bundle,
        archive=archive,
        source_digest=hashlib.sha256(bundle).hexdigest(),
        archive_sha256=hashlib.sha256(archive).hexdigest(),
        archive_size=len(archive),
    )


def _verify_like_worker(archive: bytes, limits: BundleLimits, *, expected_tip: str) -> None:
    """Run the worker's intake on the archive: archive checks, bundle checks, and tree checks."""
    try:
        bundle = extract_source(
            archive,
            expected_sha256=hashlib.sha256(archive).hexdigest(),
            expected_size=len(archive),
            limits=limits,
        )
        with tempfile.TemporaryDirectory(prefix="mao-verify-") as scratch:
            parent = Path(scratch) / "verify"
            parent.mkdir()
            materialize_checkout(
                bundle, parent / "checkout", limits=limits, expected_base_sha=expected_tip
            )
    except BaseMismatch as exc:
        raise SourceRefused("the branch changed during export; retry with a stable branch") from exc
    except BundleRefused as exc:
        raise SourceRefused(str(exc)) from exc


def _archive(bundle: bytes) -> bytes:
    """A plain tar with one root member, built with fixed metadata so equal bundles give equal bytes."""
    buffer = io.BytesIO()
    with tarfile.open(fileobj=buffer, mode="w", format=tarfile.USTAR_FORMAT) as tar:
        info = tarfile.TarInfo(SOURCE_MEMBER)
        info.size = len(bundle)
        info.mtime = 0
        info.mode = 0o644
        info.uid = info.gid = 0
        info.uname = info.gname = ""
        tar.addfile(info, io.BytesIO(bundle))
    return buffer.getvalue()


def _resolve_branch(repo: Path, ref: str) -> str:
    if _RAW_SHA.fullmatch(ref):
        raise SourceRefused(
            "pass a branch name, not a raw commit SHA; a snapshot must name a branch such as main"
        )
    if not _BRANCH_NAME.fullmatch(ref) or ".." in ref or ref.endswith("/"):
        raise SourceRefused(f"{ref!r} is not a valid branch name")
    full_ref = f"refs/heads/{ref}"
    if _git_run(repo, "show-ref", "--verify", "--quiet", full_ref).returncode == 0:
        return full_ref
    if _git_run(repo, "show-ref", "--verify", "--quiet", f"refs/tags/{ref}").returncode == 0:
        raise SourceRefused(
            f"{ref} is a tag; pass a branch name, because a tag is not accepted as a snapshot source"
        )
    raise SourceRefused(f"no branch named {ref}; pass an existing branch name")


def _refuse_dirty_tree(repo: Path) -> None:
    # --no-optional-locks keeps the status read from rewriting the user's index.
    output = _git(
        repo,
        "--no-optional-locks",
        "status",
        "--porcelain=v1",
        "-z",
        "--untracked-files=all",
    )
    for entry in output.split("\0"):
        if not entry:
            continue
        code, path = entry[:2], entry[3:]
        if code == "??":
            raise SourceRefused(f"untracked file {path}; commit it or remove it before export")
        raise SourceRefused(f"uncommitted change to {path}; commit or revert it before export")


_SYMLINK_MODE = b"120000"
_GITLINK_MODE = b"160000"


def _refuse_tree_policy(repo: Path, full_ref: str) -> None:
    """Refuse committed entries the worker refuses, read from the branch's tree, not the checkout."""
    listing = _git_bytes(repo, "ls-tree", "-r", "-z", full_ref)
    for record in listing.split(b"\0"):
        if not record:
            continue
        meta, _, raw_path = record.partition(b"\t")
        mode = meta.split(b" ", 1)[0]
        path = os.fsdecode(raw_path)
        if mode == _SYMLINK_MODE:
            raise SourceRefused(
                f"{path} is a symlink; symlinks are refused, replace it with a file"
            )
        if mode == _GITLINK_MODE:
            raise SourceRefused(
                f"{path} is a gitlink (submodule); gitlinks are refused, vendor its files instead"
            )
        if path.rsplit("/", 1)[-1] == ".gitattributes":
            attributes = _git_bytes(repo, "show", f"{full_ref}:{path}")
            if b"filter=lfs" in attributes:
                raise SourceRefused(
                    f"the tree uses Git LFS in {path} (filter=lfs), which is refused; "
                    "commit the files directly or vendor them"
                )


def _refuse_secrets(repo: Path, full_ref: str) -> None:
    for name, pattern in SECRET_PATTERNS:
        done = _git_run(repo, "grep", "-I", "-l", "-E", "-e", pattern, full_ref, "--")
        if done.returncode == 1:
            continue
        if done.returncode != 0:
            raise SourceRefused(f"the secret scan could not run: {_first_line(done.stderr)}")
        for line in done.stdout.decode("utf-8", "replace").splitlines():
            path = line[len(full_ref) + 1 :] if line.startswith(full_ref + ":") else line
            raise SourceRefused(
                f"{path} contains a {name} pattern; remove it from the branch before export "
                "(the matched text is not shown)"
            )


def _require_repository(repo: Path) -> None:
    if _git_run(repo, "rev-parse", "--git-dir").returncode != 0:
        raise SourceUnreadable(f"{repo} is not a readable git repository")


def _git_env() -> dict[str, str]:
    # Drop every GIT_* variable so a caller's environment cannot redirect the export's repository,
    # config, or index.
    env = {k: v for k, v in os.environ.items() if not k.startswith(_GIT_ENV_PREFIX)}
    env.update(
        {
            "GIT_TERMINAL_PROMPT": "0",
            "GIT_ALLOW_PROTOCOL": "",
            "GIT_NO_LAZY_FETCH": "1",
            "GIT_NO_REPLACE_OBJECTS": "1",
            "GIT_CONFIG_GLOBAL": os.devnull,
            "GIT_CONFIG_NOSYSTEM": "1",
            "LC_ALL": "C",
        }
    )
    return env


def _git_run(repo: Path, *args: str) -> subprocess.CompletedProcess[bytes]:
    """Run git with argv only, never a shell, so a path or ref is data."""
    return subprocess.run(
        ["git", "-C", str(repo), *args],
        capture_output=True,
        check=False,
        env=_git_env(),
    )


def _git_bytes(repo: Path, *args: str) -> bytes:
    done = _git_run(repo, *args)
    if done.returncode != 0:
        raise SourceRefused(f"git {args[0]} failed: {_first_line(done.stderr)}")
    return done.stdout


def _git(repo: Path, *args: str) -> str:
    return _git_bytes(repo, *args).decode("utf-8", "replace")


def _first_line(data: bytes) -> str:
    text = data.decode("utf-8", "replace").strip()
    return text.splitlines()[0] if text else "no detail"
