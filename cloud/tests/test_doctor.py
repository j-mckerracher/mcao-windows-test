"""Doctor: a local, no-network preflight that reports whether a branch can be submitted.

Doctor runs the same export the submit path runs, so a passing doctor and a submittable snapshot
cannot disagree.
"""

from pathlib import Path

from gitfixtures import commit_files, make_repo, write_files
from mao_cloud.doctor import DoctorReport, run_doctor


def _text(report: DoctorReport) -> str:
    return "\n".join(report.lines)


def test_clean_repository_passes_without_any_transport(tmp_path: Path) -> None:
    repo = make_repo(tmp_path, {"src/app.py": b"print('hi')\n", "README.md": b"# app\n"})

    report = run_doctor(repo, "main")

    assert report.ok
    assert "refs/heads/main" in _text(report)


def test_dirty_tree_fails_and_names_the_file(tmp_path: Path) -> None:
    repo = make_repo(tmp_path, {"src/app.py": b"print('hi')\n"})
    write_files(repo, {"src/app.py": b"print('changed')\n"})

    report = run_doctor(repo, "main")

    assert not report.ok
    assert "src/app.py" in _text(report)


def test_git_lfs_policy_fails(tmp_path: Path) -> None:
    repo = make_repo(tmp_path, {"README.md": b"# app\n"})
    commit_files(
        repo,
        {".gitattributes": b"*.bin filter=lfs diff=lfs merge=lfs -text\n"},
        "use lfs",
    )

    report = run_doctor(repo, "main")

    assert not report.ok
    assert "LFS" in _text(report)


def test_missing_branch_fails_with_guidance(tmp_path: Path) -> None:
    repo = make_repo(tmp_path, {"README.md": b"# app\n"})

    report = run_doctor(repo, "release")

    assert not report.ok
    assert "no branch named release" in _text(report)
