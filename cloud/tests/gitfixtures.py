"""Real git repositories for the source-export tests.

Every repository is built under a pytest temp directory with an isolated HOME and no global git
config, so the tests never read or write the developer's configuration or the checkout under test.
"""

import os
import subprocess
from collections.abc import Mapping
from pathlib import Path

AUTHOR_ENV = {
    "GIT_AUTHOR_NAME": "Fixture Author",
    "GIT_AUTHOR_EMAIL": "author@example.invalid",
    "GIT_COMMITTER_NAME": "Fixture Author",
    "GIT_COMMITTER_EMAIL": "author@example.invalid",
    "GIT_AUTHOR_DATE": "2026-10-21T12:00:00+0000",
    "GIT_COMMITTER_DATE": "2026-10-21T12:00:00+0000",
}


def _run(argv: list[str], *, home: Path) -> subprocess.CompletedProcess[str]:
    home.mkdir(parents=True, exist_ok=True)
    env = {
        "PATH": os.environ["PATH"],
        "HOME": str(home),
        "LC_ALL": "C",
        "GIT_CONFIG_GLOBAL": os.devnull,
        "GIT_CONFIG_NOSYSTEM": "1",
        **AUTHOR_ENV,
    }
    return subprocess.run(argv, check=True, capture_output=True, text=True, env=env)


def git(repo: Path, *args: str) -> str:
    return _run(["git", "-C", str(repo), *args], home=repo.parent / "home").stdout


def write_files(repo: Path, files: Mapping[str, bytes]) -> None:
    for relative, data in files.items():
        target = repo / relative
        target.parent.mkdir(parents=True, exist_ok=True)
        target.write_bytes(data)


def commit_all(repo: Path, message: str) -> None:
    git(repo, "add", "--all")
    git(repo, "commit", "-q", "--no-verify", "-m", message)


def commit_files(repo: Path, files: Mapping[str, bytes], message: str) -> None:
    write_files(repo, files)
    commit_all(repo, message)


def make_repo(root: Path, files: Mapping[str, bytes], *, branch: str = "main") -> Path:
    """A repository at root/repo with one commit holding files, on branch."""
    repo = root / "repo"
    repo.mkdir(parents=True)
    _run(["git", "init", "-q", "-b", branch, str(repo)], home=root / "home")
    commit_files(repo, files, "initial")
    return repo


def clone(source: Path, dest: Path, *flags: str) -> Path:
    """Clone a local repository through file://, so depth and filter flags take effect."""
    dest.parent.mkdir(parents=True, exist_ok=True)
    _run(
        ["git", "clone", "-q", *flags, f"file://{source}", str(dest)],
        home=dest.parent / "home",
    )
    return dest


def _write_bundle(bundle: bytes, root: Path) -> Path:
    root.mkdir(parents=True, exist_ok=True)
    path = root / "snapshot.bundle"
    path.write_bytes(bundle)
    return path


def bundle_heads(bundle: bytes, root: Path) -> list[str]:
    """The refs a bundle carries, as git lists them: one "<sha> <ref>" line each."""
    path = _write_bundle(bundle, root)
    output = _run(["git", "bundle", "list-heads", str(path)], home=root / "home").stdout
    return output.splitlines()


def unpack(bundle: bytes, root: Path) -> Path:
    """Materialize a bundle's objects in root/unpacked, with no checkout, through git's own clone."""
    path = _write_bundle(bundle, root)
    dest = root / "unpacked"
    _run(["git", "clone", "-q", "--no-checkout", str(path), str(dest)], home=root / "home")
    return dest


def ls_tree(repo: Path, ref: str) -> list[str]:
    """Every path in the tree at ref, sorted as git lists them."""
    return git(repo, "ls-tree", "-r", "--name-only", ref).splitlines()
