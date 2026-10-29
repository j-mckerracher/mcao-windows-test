"""Trusted source intake: archive, bundle, and tree checks before any repository code runs.

DPES-811 AC3. These tests use real git and real tar, because the claim is about how git behaves
(hooks, templates, clone, checkout). Bubblewrap confinement of the checkout is a separate gap.
"""

import hashlib
import io
import os
import shutil
import subprocess
import tarfile
from pathlib import Path

import pytest

from mao_cloud.trusted_bundle import (
    SOURCE_MEMBER,
    BundleLimits,
    BundleRefused,
    check_bundle_header,
    check_path,
    extract_source,
    materialize_checkout,
)

pytestmark = pytest.mark.skipif(shutil.which("git") is None, reason="git is required")

GIT_ENV = {
    "PATH": os.environ.get("PATH", "/usr/bin:/bin"),
    "HOME": "/nonexistent",
    "GIT_CONFIG_NOSYSTEM": "1",
    "GIT_CONFIG_GLOBAL": "/dev/null",
    "GIT_AUTHOR_NAME": "t",
    "GIT_AUTHOR_EMAIL": "t@example.invalid",
    "GIT_COMMITTER_NAME": "t",
    "GIT_COMMITTER_EMAIL": "t@example.invalid",
}


def _git(repo: Path, *args: str) -> str:
    result = subprocess.run(
        ["git", "-c", "commit.gpgsign=false", "-C", str(repo), *args],
        env=GIT_ENV,
        check=True,
        capture_output=True,
        text=True,
    )
    return result.stdout.strip()


def _commit_all(repo: Path, message: str = "init") -> str:
    _git(repo, "add", "-A")
    _git(repo, "commit", "-q", "--allow-empty", "-m", message)
    return _head(repo)


def _head(repo: Path) -> str:
    return _git(repo, "rev-parse", "HEAD")


def _new_repo(root: Path) -> Path:
    repo = root / "repo"
    repo.mkdir()
    _git(repo, "init", "-q", "-b", "main")
    return repo


def _write(repo: Path, name: str, data: bytes) -> None:
    target = repo / name
    target.parent.mkdir(parents=True, exist_ok=True)
    target.write_bytes(data)


def _bundle_of(repo: Path, root: Path, *refspec: str) -> bytes:
    path = root / "made.bundle"
    _git(repo, "bundle", "create", str(path), *(refspec or ("HEAD",)))
    return path.read_bytes()


def _tar_of(members: dict[str, bytes], *, symlinks: dict[str, str] | None = None) -> bytes:
    buffer = io.BytesIO()
    with tarfile.open(fileobj=buffer, mode="w") as archive:
        for name, data in members.items():
            info = tarfile.TarInfo(name)
            info.size = len(data)
            archive.addfile(info, io.BytesIO(data))
        for name, target in (symlinks or {}).items():
            info = tarfile.TarInfo(name)
            info.type = tarfile.SYMTYPE
            info.linkname = target
            archive.addfile(info)
    return buffer.getvalue()


def _intake(tar: bytes, limits: BundleLimits | None = None) -> bytes:
    return extract_source(
        tar,
        expected_sha256=hashlib.sha256(tar).hexdigest(),
        expected_size=len(tar),
        limits=limits or BundleLimits(),
    )


# --- archive: hash, size, plain tar, exactly one regular member ------------------------------


def test_a_well_formed_archive_yields_its_bundle_bytes(tmp_path: Path) -> None:
    bundle = b"bundle-bytes"
    assert _intake(_tar_of({SOURCE_MEMBER: bundle})) == bundle


def test_a_hash_mismatch_is_refused_before_the_archive_is_read(tmp_path: Path) -> None:
    tar = _tar_of({SOURCE_MEMBER: b"x"})
    with pytest.raises(BundleRefused, match="checksum"):
        extract_source(tar, expected_sha256="0" * 64, expected_size=len(tar), limits=BundleLimits())


def test_a_size_mismatch_is_refused(tmp_path: Path) -> None:
    tar = _tar_of({SOURCE_MEMBER: b"x"})
    with pytest.raises(BundleRefused, match="size"):
        extract_source(
            tar,
            expected_sha256=hashlib.sha256(tar).hexdigest(),
            expected_size=len(tar) + 1,
            limits=BundleLimits(),
        )


def test_an_archive_over_the_source_limit_is_refused(tmp_path: Path) -> None:
    tar = _tar_of({SOURCE_MEMBER: b"x" * 100})
    with pytest.raises(BundleRefused, match="limit"):
        _intake(tar, BundleLimits(max_bundle_bytes=64))


def test_a_compressed_archive_is_refused_as_not_plain_tar(tmp_path: Path) -> None:
    import gzip

    tar = _tar_of({SOURCE_MEMBER: b"x"})
    with pytest.raises(BundleRefused, match="plain tar"):
        _intake(gzip.compress(tar))


@pytest.mark.parametrize(
    "members",
    [
        {SOURCE_MEMBER: b"x", "extra.txt": b"y"},
        {"nested/" + SOURCE_MEMBER: b"x"},
        {"../" + SOURCE_MEMBER: b"x"},
        {"/" + SOURCE_MEMBER: b"x"},
        {"other.bundle": b"x"},
    ],
)
def test_the_archive_must_hold_exactly_one_source_bundle_at_the_root(
    members: dict[str, bytes],
) -> None:
    with pytest.raises(BundleRefused):
        _intake(_tar_of(members))


def test_a_symlink_member_is_refused(tmp_path: Path) -> None:
    tar = _tar_of({}, symlinks={SOURCE_MEMBER: "/etc/passwd"})
    with pytest.raises(BundleRefused, match="regular"):
        _intake(tar)


# --- bundle header: v2, self-contained, ref lines only --------------------------------------


def test_a_real_bundle_header_is_accepted(tmp_path: Path) -> None:
    repo = _new_repo(tmp_path)
    _write(repo, "a.txt", b"a")
    _commit_all(repo)
    check_bundle_header(_bundle_of(repo, tmp_path))


def test_a_bundle_with_prerequisites_is_refused(tmp_path: Path) -> None:
    repo = _new_repo(tmp_path)
    _write(repo, "a.txt", b"a")
    base = _commit_all(repo, "base")
    _write(repo, "b.txt", b"b")
    _commit_all(repo, "tip")
    partial = _bundle_of(repo, tmp_path, "main", f"^{base}")
    with pytest.raises(BundleRefused, match="prerequisites"):
        check_bundle_header(partial)


def test_bytes_that_are_not_a_v2_bundle_are_refused() -> None:
    with pytest.raises(BundleRefused, match="v2 git bundle"):
        check_bundle_header(b"# v3 git bundle\n\n")


def test_a_truncated_header_is_refused() -> None:
    with pytest.raises(BundleRefused, match="truncated"):
        check_bundle_header(b"# v2 git bundle\nabc")


# --- paths and modes ------------------------------------------------------------------------


@pytest.mark.parametrize(
    "path",
    [
        b"",
        b"/abs",
        b"../up",
        b"a/../b",
        b"a//b",
        b"a/./b",
        b".git/config",
        b"x/.GIT/hooks/x",
        b"a\\b",
    ],
)
def test_unsafe_tree_paths_are_refused(path: bytes) -> None:
    with pytest.raises(BundleRefused):
        check_path(path, max_path_bytes=1024)


def test_an_over_long_path_is_refused() -> None:
    with pytest.raises(BundleRefused, match="long"):
        check_path(b"a" * 20, max_path_bytes=10)


def test_ordinary_nested_paths_are_accepted() -> None:
    check_path(b"src/pkg/module.py", max_path_bytes=1024)


# --- materialized checkout: real git, hooks off, tree limits --------------------------------


def test_a_clean_bundle_materializes_as_the_committed_tree(tmp_path: Path) -> None:
    repo = _new_repo(tmp_path)
    _write(repo, "src/app.py", b"print('hi')\n")
    tip = _commit_all(repo)
    bundle = _bundle_of(repo, tmp_path)
    dest = tmp_path / "checkout"

    sha = materialize_checkout(bundle, dest, limits=BundleLimits(), expected_base_sha=tip)

    assert sha == tip
    assert (dest / "src" / "app.py").read_bytes() == b"print('hi')\n"


def test_a_bundle_whose_tip_is_not_the_claimed_base_is_refused_before_any_clone(
    tmp_path: Path,
) -> None:
    """DPES-811 AC3: the bundle tip must be the base SHA the controller claimed, checked before git runs."""
    repo = _new_repo(tmp_path)
    _write(repo, "src/app.py", b"print('hi')\n")
    tip = _commit_all(repo)
    bundle = _bundle_of(repo, tmp_path)
    claimed = "0" * 40
    assert claimed != tip
    dest = tmp_path / "checkout"

    with pytest.raises(BundleRefused, match="base"):
        materialize_checkout(bundle, dest, limits=BundleLimits(), expected_base_sha=claimed)

    assert not (tmp_path / "bundle-staging").exists()
    assert not (tmp_path / "git-home").exists()
    assert not dest.exists()


def test_a_symlink_in_the_tree_is_refused(tmp_path: Path) -> None:
    repo = _new_repo(tmp_path)
    os.symlink("/etc/passwd", repo / "escape")
    _commit_all(repo)
    with pytest.raises(BundleRefused, match="symlink"):
        materialize_checkout(
            _bundle_of(repo, tmp_path),
            tmp_path / "c",
            limits=BundleLimits(),
            expected_base_sha=_head(repo),
        )


def test_a_submodule_gitlink_in_the_tree_is_refused(tmp_path: Path) -> None:
    repo = _new_repo(tmp_path)
    _write(repo, "keep.txt", b"k")
    _commit_all(repo)
    _git(repo, "update-index", "--add", "--cacheinfo", f"160000,{'a' * 40},sub")
    _git(repo, "commit", "-q", "-m", "gitlink")
    with pytest.raises(BundleRefused, match="gitlink|submodule"):
        materialize_checkout(
            _bundle_of(repo, tmp_path),
            tmp_path / "c",
            limits=BundleLimits(),
            expected_base_sha=_head(repo),
        )


def test_a_tree_over_the_entry_limit_is_refused(tmp_path: Path) -> None:
    repo = _new_repo(tmp_path)
    for index in range(5):
        _write(repo, f"f{index}.txt", b"x")
    _commit_all(repo)
    with pytest.raises(BundleRefused, match="entries"):
        materialize_checkout(
            _bundle_of(repo, tmp_path),
            tmp_path / "c",
            limits=BundleLimits(max_tree_entries=3),
            expected_base_sha=_head(repo),
        )


def test_unpacked_content_over_the_limit_is_refused(tmp_path: Path) -> None:
    repo = _new_repo(tmp_path)
    _write(repo, "big.bin", b"z" * 4096)
    _commit_all(repo)
    with pytest.raises(BundleRefused, match="unpacked"):
        materialize_checkout(
            _bundle_of(repo, tmp_path),
            tmp_path / "c",
            limits=BundleLimits(max_unpacked_bytes=1024),
            expected_base_sha=_head(repo),
        )


@pytest.mark.parametrize("over_limit", [False, True])
def test_checkout_size_counts_every_path_sharing_a_blob(
    tmp_path: Path, over_limit: bool
) -> None:
    repo = _new_repo(tmp_path)
    content = b"z" * 1024
    for index in range(20):
        _write(repo, f"f{index}.bin", content)
    _commit_all(repo)
    bundle = _bundle_of(repo, tmp_path)
    dest = tmp_path / "checkout"
    limits = BundleLimits(max_unpacked_bytes=20 * len(content) - int(over_limit))

    if over_limit:
        with pytest.raises(BundleRefused, match="unpacked checkout"):
            materialize_checkout(bundle, dest, limits=limits, expected_base_sha=_head(repo))
        assert list(dest.iterdir()) == [dest / ".git"]
    else:
        materialize_checkout(bundle, dest, limits=limits, expected_base_sha=_head(repo))
        assert all((dest / f"f{index}.bin").read_bytes() == content for index in range(20))


def test_repository_hooks_and_templates_never_run(tmp_path: Path, monkeypatch) -> None:
    """Ambient git config and templates are ignored, and the checkout runs no hook."""
    marker = tmp_path / "hook-ran"
    hooks = tmp_path / "ambient-hooks"
    hooks.mkdir()
    for name in ("post-checkout", "post-clone", "post-merge"):
        hook = hooks / name
        hook.write_text(f"#!/bin/sh\ntouch {marker}\n", encoding="utf-8")
        hook.chmod(0o755)
    template = tmp_path / "template"
    (template / "hooks").mkdir(parents=True)
    shutil.copy(hooks / "post-checkout", template / "hooks" / "post-checkout")
    (template / "hooks" / "post-checkout").chmod(0o755)
    ambient_home = tmp_path / "home"
    ambient_home.mkdir()
    (ambient_home / ".gitconfig").write_text(f"[core]\n\thooksPath = {hooks}\n", encoding="utf-8")
    monkeypatch.setenv("HOME", str(ambient_home))
    monkeypatch.setenv("GIT_TEMPLATE_DIR", str(template))
    monkeypatch.setenv("GIT_CONFIG_GLOBAL", str(ambient_home / ".gitconfig"))

    repo = _new_repo(tmp_path)
    _write(repo, "a.txt", b"a")
    _commit_all(repo)
    materialize_checkout(
        _bundle_of(repo, tmp_path),
        tmp_path / "c",
        limits=BundleLimits(),
        expected_base_sha=_head(repo),
    )

    assert not marker.exists()
    assert (tmp_path / "c" / "a.txt").exists()
