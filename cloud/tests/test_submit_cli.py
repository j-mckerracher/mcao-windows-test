"""Submit and doctor through the command line: exit codes, dry-run silence, and refusal of bad input.

Exit codes follow cli.py: 0 success or plan, 1 input or schema refused, 2 input cannot be read or
used (including no transport for --execute), 75 retryable.
"""

from pathlib import Path

import pytest

from fakes import EPOCH, PROFILE_ID, uuid7
from flows import make_world
from gitfixtures import make_repo, write_files
from mao_cloud.cli import main
from submit_fakes import OfflineTransport, SpyTransport, WorldTransport


def _repo(tmp_path: Path) -> Path:
    return make_repo(tmp_path / "work", {"src/app.py": b"print('hi')\n"})


def _submit_argv(repo: Path, state: Path, *extra: str, task_id: str = "DPES-805") -> list[str]:
    return [
        "submit",
        "--repo",
        str(repo),
        "--ref",
        "main",
        "--state-dir",
        str(state),
        "--task-id",
        task_id,
        "--prompt-id",
        "prompt-805",
        "--profile",
        PROFILE_ID,
        "--target",
        "cloud-pilot",
        "--harness",
        "pi",
        "--model",
        "gemini-3.8-flash",
        "--thinking",
        "medium",
        "--max-runtime-seconds",
        "600",
        "--workflow-correlation",
        "wf-805",
        "--request-uuid",
        str(uuid7(EPOCH)),
        *extra,
    ]


def test_doctor_on_a_clean_repository_returns_zero(tmp_path: Path) -> None:
    repo = _repo(tmp_path)

    assert main(["doctor", "--repo", str(repo), "--ref", "main"]) == 0


def test_doctor_on_a_dirty_repository_returns_one(tmp_path: Path, capsys) -> None:
    repo = _repo(tmp_path)
    write_files(repo, {"src/app.py": b"print('changed')\n"})

    assert main(["doctor", "--repo", str(repo), "--ref", "main"]) == 1
    assert "src/app.py" in capsys.readouterr().out


def test_submit_without_execute_plans_only_and_touches_nothing(tmp_path: Path) -> None:
    repo = _repo(tmp_path)
    state = tmp_path / "state"
    spy = SpyTransport(None)

    code = main(_submit_argv(repo, state), transport=spy)

    assert code == 0
    assert spy.calls == []
    assert not state.exists()


def test_submit_execute_starts_a_run_and_prints_its_id(tmp_path: Path, capsys) -> None:
    world = make_world()
    repo = _repo(tmp_path)
    state = tmp_path / "state"

    code = main(_submit_argv(repo, state, "--execute"), transport=WorldTransport(world))

    assert code == 0
    run_ids = [run.run_id for run in world.store.list_runs()]
    assert len(run_ids) == 1
    assert run_ids[0] in capsys.readouterr().out


def test_missing_required_flag_is_a_usage_error(tmp_path: Path) -> None:
    with pytest.raises(SystemExit) as caught:
        main(["submit", "--ref", "main", "--state-dir", str(tmp_path / "state")])

    assert caught.value.code == 2


def test_execute_without_a_transport_is_a_usage_error_and_writes_no_record(tmp_path: Path) -> None:
    repo = _repo(tmp_path)
    state = tmp_path / "state"

    code = main(_submit_argv(repo, state, "--execute"))

    assert code == 2
    assert not state.exists()


def test_transport_failure_maps_to_retryable_exit_75(tmp_path: Path) -> None:
    repo = _repo(tmp_path)
    state = tmp_path / "state"

    code = main(_submit_argv(repo, state, "--execute"), transport=OfflineTransport())

    assert code == 75


def test_task_id_with_shell_metacharacters_is_refused_before_any_call(tmp_path: Path) -> None:
    repo = _repo(tmp_path)
    spy = SpyTransport(None)

    code = main(
        _submit_argv(repo, tmp_path / "state", "--execute", task_id="DPES-805; rm -rf ~"),
        transport=spy,
    )

    assert code == 1
    assert spy.calls == []
