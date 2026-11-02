"""Trusted source intake: verify the uploaded archive and bundle before any repository code runs.

Order matters. Every refusal here happens before a working tree exists:
1. The archive is hashed, size-checked, and read as plain tar with exactly one regular member,
   source.bundle, at the root.
2. The bundle header is parsed by this module, not by git. It must be a v2 bundle with exactly one
   ref and no prerequisites, so the bundle is self-contained. Its tip must equal the base SHA the
   controller claimed (DPES-811 AC3). A mismatch is refused here, before any staging directory or
   git process exists.
3. git clones the bundle into an object store with no checkout, with hooks and templates off and
   ambient git configuration ignored. Objects are checked with fsck.
4. The tree is listed and refused on symlinks, gitlinks, unsafe paths, and entry-count limits.
   Checkout bytes are summed per path; unpacked object size is checked separately with cat-file.
5. Only then is the tip checked out, detached, with hooks still disabled.
"""

import hashlib
import hmac
import io
import os
import re
import subprocess
import tarfile
from dataclasses import dataclass
from pathlib import Path

from mao_cloud.trusted_profiles import SOURCE_LIMIT_BYTES

MiB = 1024 * 1024
GiB = 1024 * MiB

SOURCE_MEMBER = "source.bundle"
BUNDLE_HEADER = b"# v2 git bundle\n"
GIT_TIMEOUT_SECONDS = 600
_SHA1_LINE = re.compile(rb"[0-9a-f]{40}")
_ALLOWED_MODES = frozenset({b"100644", b"100755"})


class BundleRefused(Exception):
    """The source is refused. The message names the check, never the untrusted content."""


class BaseMismatch(BundleRefused):
    """The bundle's tip is not the base SHA the controller claimed for this source."""


@dataclass(frozen=True)
class BundleLimits:
    """Tree and archive limits. Defaults are this module's proposal, flagged for review."""

    max_bundle_bytes: int = SOURCE_LIMIT_BYTES
    max_tree_entries: int = 50_000
    max_unpacked_bytes: int = 1 * GiB
    max_path_bytes: int = 1024


def extract_source(
    archive: bytes, *, expected_sha256: str, expected_size: int, limits: BundleLimits
) -> bytes:
    """Return the bundle bytes from a plain tar archive whose hash and size match the bootstrap."""
    if len(archive) != expected_size:
        raise BundleRefused("the source archive size does not match the bootstrap record")
    if len(archive) > limits.max_bundle_bytes:
        raise BundleRefused("the source archive exceeds the source limit")
    if not hmac.compare_digest(hashlib.sha256(archive).hexdigest(), expected_sha256.lower()):
        raise BundleRefused("the source archive checksum does not match the bootstrap record")
    try:
        with tarfile.open(fileobj=io.BytesIO(archive), mode="r:") as tar:
            members = tar.getmembers()
            if len(members) != 1:
                raise BundleRefused("the source archive must hold exactly one member")
            member = members[0]
            if member.name != SOURCE_MEMBER:
                raise BundleRefused("the source archive must hold source.bundle at its root")
            if not member.isreg():
                raise BundleRefused("the source member must be a regular file")
            if member.size > limits.max_bundle_bytes:
                raise BundleRefused("the source bundle exceeds the source limit")
            handle = tar.extractfile(member)
            if handle is None:
                raise BundleRefused("the source member cannot be read")
            data = handle.read(limits.max_bundle_bytes + 1)
    except tarfile.TarError as exc:
        raise BundleRefused("the source archive is not a plain tar archive") from exc
    if len(data) > limits.max_bundle_bytes:
        raise BundleRefused("the source bundle exceeds the source limit")
    return data


def check_bundle_header(bundle: bytes) -> list[tuple[bytes, bytes]]:
    """Parse the v2 header and return its (sha, ref) pairs. Refuses prerequisites and anything odd."""
    if not bundle.startswith(BUNDLE_HEADER):
        raise BundleRefused("the source is not a v2 git bundle")
    refs: list[tuple[bytes, bytes]] = []
    position = len(BUNDLE_HEADER)
    while True:
        end = bundle.find(b"\n", position)
        if end < 0:
            raise BundleRefused("the bundle header is truncated")
        line = bundle[position:end]
        position = end + 1
        if line == b"":
            break
        if line.startswith(b"-"):
            raise BundleRefused("the bundle has prerequisites; it must be self-contained")
        if line.startswith(b"@"):
            raise BundleRefused("the bundle declares capabilities this worker does not accept")
        sha, separator, ref = line.partition(b" ")
        if not separator or not ref or not _SHA1_LINE.fullmatch(sha):
            raise BundleRefused("the bundle header has a malformed ref line")
        refs.append((sha, ref))
    if not refs:
        raise BundleRefused("the bundle names no refs")
    return refs


def check_path(path: bytes, *, max_path_bytes: int) -> None:
    """Refuse empty, absolute, traversing, backslash, and .git-directory tree paths."""
    if not path:
        raise BundleRefused("a tree entry has an empty path")
    if len(path) > max_path_bytes:
        raise BundleRefused("a tree path is too long")
    if path.startswith(b"/") or b"\\" in path:
        raise BundleRefused("a tree path is absolute or uses backslashes")
    for part in path.split(b"/"):
        if part in (b"", b".", b".."):
            raise BundleRefused("a tree path has an empty, current, or parent component")
        if part.lower() == b".git":
            raise BundleRefused("a tree path enters a .git directory")


def _git_env(home: Path) -> dict[str, str]:
    """A git environment built from scratch: no ambient config, templates, or hooks."""
    return {
        "PATH": os.environ.get("PATH", "/usr/bin:/bin"),
        "HOME": str(home),
        "LC_ALL": "C",
        "GIT_CONFIG_NOSYSTEM": "1",
        "GIT_CONFIG_GLOBAL": "/dev/null",
        "GIT_TERMINAL_PROMPT": "0",
    }


def _git(
    home: Path,
    subcommand: str,
    *args: str,
    config: tuple[str, ...] = (),
    cwd: Path | None = None,
) -> bytes:
    """Run one git subcommand and return stdout. Errors name the subcommand, never its output."""
    argv = ["git", "-c", "core.hooksPath=/dev/null"]
    for item in config:
        argv += ["-c", item]
    argv += [subcommand, *args]
    result = subprocess.run(
        argv,
        env=_git_env(home),
        cwd=cwd,
        capture_output=True,
        timeout=GIT_TIMEOUT_SECONDS,
        check=False,
    )
    if result.returncode != 0:
        raise BundleRefused(f"git {subcommand} failed on the source")
    return result.stdout


def materialize_checkout(
    bundle: bytes, dest: Path, *, limits: BundleLimits, expected_base_sha: str
) -> str:
    """Verify the bundle's base, objects, and tree, then check out its tip detached.

    expected_base_sha is the commit the controller claimed for this source. It is required so no
    caller can materialize a bundle without binding it to a base. Returns the tip SHA.
    """
    refs = check_bundle_header(bundle)
    if len(refs) != 1:
        raise BundleRefused("the bundle must name exactly one ref")
    tip = refs[0][0].decode("ascii")
    if tip != expected_base_sha:
        raise BaseMismatch("the bundle tip is not the claimed base SHA")

    staging = dest.parent / "bundle-staging"
    home = dest.parent / "git-home"
    staging.mkdir(mode=0o700, parents=True, exist_ok=False)
    home.mkdir(mode=0o700, exist_ok=False)
    bundle_path = staging / SOURCE_MEMBER
    with open(bundle_path, "xb") as handle:
        handle.write(bundle)

    _git(
        home,
        "clone",
        str(bundle_path),
        str(dest),
        "--no-checkout",
        "--template=",
        config=("transfer.fsckObjects=true",),
    )
    _git(home, "fsck", "--strict", "--no-dangling", "--no-progress", cwd=dest)
    _check_tree(home, dest, tip, limits)
    _check_unpacked(home, dest, limits)
    _git(home, "checkout", "--force", "--detach", tip, cwd=dest)
    return tip


def _check_tree(home: Path, dest: Path, tip: str, limits: BundleLimits) -> None:
    listing = _git(home, "ls-tree", "-r", "-l", "-z", tip, cwd=dest)
    entries = [entry for entry in listing.split(b"\0") if entry]
    if len(entries) > limits.max_tree_entries:
        raise BundleRefused("the tree has more entries than the limit")
    checkout_bytes = 0
    for entry in entries:
        meta, _, path = entry.partition(b"\t")
        mode, object_type, _, size = meta.split()
        if mode == b"120000":
            raise BundleRefused("the tree contains a symlink, which is refused")
        if mode == b"160000" or object_type == b"commit":
            raise BundleRefused("the tree contains a gitlink (submodule), which is refused")
        if mode not in _ALLOWED_MODES:
            raise BundleRefused("the tree contains an entry with an unexpected mode")
        check_path(path, max_path_bytes=limits.max_path_bytes)
        checkout_bytes += int(size)
        if checkout_bytes > limits.max_unpacked_bytes:
            raise BundleRefused("the unpacked checkout content exceeds the limit")


def _check_unpacked(home: Path, dest: Path, limits: BundleLimits) -> None:
    listing = _git(
        home,
        "cat-file",
        "--batch-all-objects",
        "--batch-check=%(objectsize)",
        cwd=dest,
    )
    total = sum(int(line) for line in listing.split() if line.isdigit())
    if total > limits.max_unpacked_bytes:
        raise BundleRefused("the unpacked content exceeds the limit")
