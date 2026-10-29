"""Source export: a committed branch becomes a verified bundle and archive, or the export is refused.

Every repository here is real git under a pytest temp directory. The export is judged by the
worker's own intake: the archive must round-trip through extract_source, and the bundle must unpack
into the committed tree.
"""

import hashlib
import io
import os
import tarfile
from dataclasses import replace
from pathlib import Path

import pytest

from gitfixtures import (
    bundle_heads,
    commit_all,
    commit_files,
    git,
    ls_tree,
    make_repo,
    unpack,
    write_files,
)
from mao_cloud import source
from mao_cloud.source import Snapshot, SourceRefused, export_snapshot
from mao_cloud.trusted_bundle import BundleLimits, extract_source


def _round_trip(snapshot: Snapshot) -> bytes:
    return extract_source(
        snapshot.archive,
        expected_sha256=snapshot.archive_sha256,
        expected_size=snapshot.archive_size,
        limits=BundleLimits(),
    )


def test_committed_branch_exports_as_one_verified_bundle(tmp_path: Path) -> None:
    repo = make_repo(tmp_path, {"src/app.py": b"print('hi')\n", "README.md": b"# app\n"})
    tip = git(repo, "rev-parse", "HEAD").strip()

    snapshot = export_snapshot(repo, "main")

    assert snapshot.base_sha == tip
    assert snapshot.bundle.startswith(b"# v2 git bundle\n")
    assert bundle_heads(snapshot.bundle, tmp_path / "heads") == [f"{tip} refs/heads/main"]
    assert _round_trip(snapshot) == snapshot.bundle
    assert ls_tree(unpack(snapshot.bundle, tmp_path / "check"), tip) == ["README.md", "src/app.py"]


def test_archive_holds_one_root_member_named_source_bundle(tmp_path: Path) -> None:
    repo = make_repo(tmp_path, {"README.md": b"# app\n"})

    snapshot = export_snapshot(repo, "main")

    with tarfile.open(fileobj=io.BytesIO(snapshot.archive)) as archive:
        assert archive.getnames() == ["source.bundle"]
    assert snapshot.source_digest == hashlib.sha256(snapshot.bundle).hexdigest()
    assert snapshot.archive_size == len(snapshot.archive)


def test_history_is_included_in_the_bundle(tmp_path: Path) -> None:
    repo = make_repo(tmp_path, {"README.md": b"one\n"})
    commit_files(repo, {"README.md": b"two\n"}, "second")

    snapshot = export_snapshot(repo, "main")
    unpacked = unpack(snapshot.bundle, tmp_path / "check")

    assert git(unpacked, "rev-list", "--count", snapshot.base_sha).strip() == "2"


def test_dirty_tracked_file_is_refused_by_name(tmp_path: Path) -> None:
    repo = make_repo(tmp_path, {"src/app.py": b"print('hi')\n"})
    write_files(repo, {"src/app.py": b"print('changed')\n"})

    with pytest.raises(SourceRefused) as caught:
        export_snapshot(repo, "main")

    assert "uncommitted" in str(caught.value)
    assert "src/app.py" in str(caught.value)


def test_untracked_file_is_refused_by_name(tmp_path: Path) -> None:
    repo = make_repo(tmp_path, {"src/app.py": b"print('hi')\n"})
    write_files(repo, {"notes/todo.txt": b"not committed\n"})

    with pytest.raises(SourceRefused) as caught:
        export_snapshot(repo, "main")

    assert "untracked" in str(caught.value)
    assert "notes/todo.txt" in str(caught.value)


def test_ignored_and_laptop_state_is_not_exported(tmp_path: Path) -> None:
    repo = make_repo(tmp_path, {"README.md": b"# app\n"})
    commit_files(repo, {".gitignore": b".env\nlocal/\n"}, "ignore local state")
    write_files(repo, {".env": b"TOKEN=laptop-only\n", "local/state.json": b"{}\n"})
    tip = git(repo, "rev-parse", "HEAD").strip()

    snapshot = export_snapshot(repo, "main")
    paths = ls_tree(unpack(snapshot.bundle, tmp_path / "check"), tip)

    assert paths == [".gitignore", "README.md"]
    assert b"laptop-only" not in snapshot.bundle


def test_remote_url_credentials_never_reach_the_bundle(tmp_path: Path) -> None:
    repo = make_repo(tmp_path, {"README.md": b"# app\n"})
    git(
        repo,
        "remote",
        "add",
        "origin",
        "https://deploy:SECRET-TOKEN-VALUE@git.example.invalid/acme/app.git",
    )

    snapshot = export_snapshot(repo, "main")

    assert b"SECRET-TOKEN-VALUE" not in snapshot.bundle
    assert b"SECRET-TOKEN-VALUE" not in snapshot.archive


def test_branch_advance_after_policy_scan_cannot_export_an_unscanned_tip(
    tmp_path: Path, monkeypatch: pytest.MonkeyPatch
) -> None:
    repo = make_repo(tmp_path, {"README.md": b"# app\n"})
    pinned = git(repo, "rev-parse", "HEAD").strip()
    check_policy = source._refuse_tree_policy
    scan_secrets = source._refuse_secrets

    def advance_after_policy(path: Path, commit: str) -> None:
        assert commit == pinned
        check_policy(path, commit)
        commit_files(
            repo,
            {"key.txt": ("-----BEGIN " + "OPENSSH PRIVATE KEY-----\n").encode()},
            "advance during export",
        )

    def scan_pinned_commit(path: Path, commit: str) -> None:
        assert commit == pinned
        scan_secrets(path, commit)

    monkeypatch.setattr(source, "_refuse_tree_policy", advance_after_policy)
    monkeypatch.setattr(source, "_refuse_secrets", scan_pinned_commit)

    with pytest.raises(SourceRefused, match="branch changed during export"):
        export_snapshot(repo, "main")


def test_missing_promisor_blob_is_refused_without_contacting_the_remote(
    tmp_path: Path, monkeypatch: pytest.MonkeyPatch
) -> None:
    files = {"README.md": b"# app\n"}
    repo = make_repo(tmp_path / "local", files)
    remote = make_repo(tmp_path / "remote", files)
    marker = tmp_path / "remote-contacted"
    upload_pack = tmp_path / "upload-pack"
    upload_pack.write_text(
        '#!/bin/sh\ntouch "' + str(marker) + '"\nexec git-upload-pack "$@"\n',
        encoding="utf-8",
    )
    upload_pack.chmod(0o755)
    git(repo, "remote", "add", "origin", remote.as_uri())
    git(repo, "config", "remote.origin.promisor", "true")
    git(repo, "config", "extensions.partialClone", "origin")
    git(repo, "config", "remote.origin.uploadpack", str(upload_pack))
    git(repo, "config", "protocol.file.allow", "always")
    blob = git(repo, "rev-parse", "HEAD:README.md").strip()
    object_path = repo / ".git" / "objects" / blob[:2] / blob[2:]
    object_path.unlink()
    monkeypatch.setenv("GIT_ALLOW_PROTOCOL", "file")
    monkeypatch.setenv("GIT_NO_LAZY_FETCH", "0")

    with pytest.raises(SourceRefused):
        export_snapshot(repo, "main")

    assert not marker.exists()
    assert not object_path.exists()


def test_export_needs_no_network_even_when_the_remote_is_unreachable(tmp_path: Path) -> None:
    repo = make_repo(tmp_path, {"README.md": b"# app\n"})
    git(repo, "remote", "add", "origin", "https://git.example.invalid/acme/app.git")

    snapshot = export_snapshot(repo, "main")

    assert snapshot.base_sha == git(repo, "rev-parse", "HEAD").strip()


def test_raw_commit_sha_is_refused_with_guidance(tmp_path: Path) -> None:
    repo = make_repo(tmp_path, {"README.md": b"# app\n"})
    tip = git(repo, "rev-parse", "HEAD").strip()

    with pytest.raises(SourceRefused, match="branch name"):
        export_snapshot(repo, tip)


def test_tag_is_refused_with_guidance(tmp_path: Path) -> None:
    repo = make_repo(tmp_path, {"README.md": b"# app\n"})
    git(repo, "tag", "v1.0")

    with pytest.raises(SourceRefused, match="tag"):
        export_snapshot(repo, "v1.0")


def test_symlink_is_refused(tmp_path: Path) -> None:
    repo = make_repo(tmp_path, {"README.md": b"# app\n"})
    os.symlink("README.md", repo / "link")
    commit_all(repo, "add symlink")

    with pytest.raises(SourceRefused, match="symlink"):
        export_snapshot(repo, "main")


def test_gitlink_is_refused(tmp_path: Path) -> None:
    repo = make_repo(tmp_path, {"README.md": b"# app\n"})
    tip = git(repo, "rev-parse", "HEAD").strip()
    git(repo, "update-index", "--add", "--cacheinfo", f"160000,{tip},vendor/sub")
    git(repo, "commit", "-q", "--no-verify", "-m", "add gitlink")

    with pytest.raises(SourceRefused, match="gitlink"):
        export_snapshot(repo, "main")


def test_git_lfs_policy_is_refused(tmp_path: Path) -> None:
    repo = make_repo(
        tmp_path,
        {
            ".gitattributes": b"*.bin filter=lfs diff=lfs merge=lfs -text\n",
            "model.bin": b"\x00\x01",
        },
    )

    with pytest.raises(SourceRefused, match="LFS"):
        export_snapshot(repo, "main")


def test_archive_over_the_source_limit_is_refused(tmp_path: Path) -> None:
    repo = make_repo(tmp_path, {"README.md": b"# app\n"})
    limits = replace(BundleLimits(), max_bundle_bytes=10)

    with pytest.raises(SourceRefused, match="archive exceeds"):
        export_snapshot(repo, "main", limits=limits)


def test_oversized_bundle_is_refused_before_its_contents_are_read(
    tmp_path: Path, monkeypatch: pytest.MonkeyPatch
) -> None:
    repo = make_repo(tmp_path, {"README.md": b"# app\n"})
    open_path = Path.open

    def refuse_bundle_read(path: Path, mode: str = "r", *args, **kwargs):
        if path.name == "source.bundle" and mode == "rb":
            raise AssertionError("an oversized bundle must not be loaded into memory")
        return open_path(path, mode, *args, **kwargs)

    monkeypatch.setattr(Path, "open", refuse_bundle_read)

    with pytest.raises(SourceRefused, match="archive exceeds"):
        export_snapshot(repo, "main", limits=replace(BundleLimits(), max_bundle_bytes=10))


def test_unpacked_content_over_the_limit_is_refused(tmp_path: Path) -> None:
    repo = make_repo(tmp_path, {"README.md": b"# app with more than one byte\n"})
    limits = replace(BundleLimits(), max_unpacked_bytes=1)

    with pytest.raises(SourceRefused, match="unpacked"):
        export_snapshot(repo, "main", limits=limits)


def test_secret_pattern_is_refused_naming_path_and_pattern_only(tmp_path: Path) -> None:
    key_header = "-----BEGIN " + "OPENSSH PRIVATE KEY-----"
    repo = make_repo(tmp_path, {"deploy/key.txt": (key_header + "\n").encode()})

    with pytest.raises(SourceRefused) as caught:
        export_snapshot(repo, "main")

    message = str(caught.value)
    assert "deploy/key.txt" in message
    assert "private-key" in message
    assert "OPENSSH" not in message


def test_identical_repository_exports_identical_bytes(tmp_path: Path) -> None:
    repo = make_repo(tmp_path, {"src/app.py": b"print('hi')\n", "README.md": b"# app\n"})

    first = export_snapshot(repo, "main")
    second = export_snapshot(repo, "main")

    assert first.bundle == second.bundle
    assert first.archive == second.archive
    assert first.archive_sha256 == second.archive_sha256


def test_repository_path_with_shell_metacharacters_is_data(tmp_path: Path) -> None:
    root = tmp_path / "x; touch PWNED $(touch PWNED) 'q'"
    root.mkdir()
    repo = make_repo(root, {"README.md": b"# app\n"})

    snapshot = export_snapshot(repo, "main")

    assert snapshot.base_sha == git(repo, "rev-parse", "HEAD").strip()
    assert not any(path.name == "PWNED" for path in tmp_path.rglob("*"))
