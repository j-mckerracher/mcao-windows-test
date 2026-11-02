"""Trusted sandbox: the bubblewrap argv the worker builds, and the probe that qualifies it.

DPES-811 AC3 and AC6. These tests pin the argv shape and the fail-closed probe logic with a fake
runner. Real bubblewrap confinement, the --size flag on the tmpfs, and the shared network that lets
sandboxed commands reach the metadata server are qualified by DPES-804 and not simulated here.
"""

from pathlib import Path
import shutil
import subprocess

import pytest

from mao_cloud.trusted_sandbox import (
    SandboxUnavailable,
    ROOT_MOUNT_AWK,
    bwrap_argv,
    qualify_sandbox,
    worker_env,
)

WORKSPACE = "/var/mao/intake/workspace"
ONE_GIB = 1024**3
PROBE_OK = "ROOT_READONLY\nWORKSPACE_WRITABLE\n"


class FakeRun:
    """Records each probe argv and answers with scripted output, or a scripted launch error."""

    def __init__(
        self, *, returncode: int = 0, stdout: str = "", error: Exception | None = None
    ) -> None:
        self.returncode = returncode
        self.stdout = stdout
        self.error = error
        self.calls: list[list[str]] = []

    def __call__(self, argv: list[str]) -> tuple[int, str]:
        self.calls.append(list(argv))
        if self.error is not None:
            raise self.error
        return self.returncode, self.stdout


def _bind_pairs(argv: list[str]) -> list[tuple[str, str, str]]:
    return [
        (token, argv[index + 1], argv[index + 2])
        for index, token in enumerate(argv)
        if token in ("--ro-bind", "--bind")
    ]


def _setenv_pairs(argv: list[str]) -> dict[str, str]:
    return {
        argv[index + 1]: argv[index + 2] for index, token in enumerate(argv) if token == "--setenv"
    }


# --- argv shape -----------------------------------------------------------------------------


def test_the_worker_runs_under_bubblewrap_with_the_command_after_a_separator() -> None:
    argv = bwrap_argv(WORKSPACE, ["pi", "--print", "hello"])
    assert argv[0] == "bwrap"
    separator = argv.index("--")
    assert argv[separator + 1 :] == ["pi", "--print", "hello"]


def test_the_sandbox_dies_with_its_parent_and_unshares_pid_ipc_and_uts() -> None:
    argv = bwrap_argv(WORKSPACE, ["true"])
    for flag in (
        "--die-with-parent",
        "--new-session",
        "--unshare-pid",
        "--unshare-ipc",
        "--unshare-uts",
    ):
        assert flag in argv


def test_the_root_filesystem_is_mounted_read_only() -> None:
    assert ("--ro-bind", "/", "/") in _bind_pairs(bwrap_argv(WORKSPACE, ["true"]))


def test_only_the_workspace_is_writable_from_the_host() -> None:
    writable = [
        pair for pair in _bind_pairs(bwrap_argv(WORKSPACE, ["true"])) if pair[0] == "--bind"
    ]
    assert writable == [("--bind", WORKSPACE, "/workspace")]


def test_no_host_path_is_bound_beyond_the_root_and_the_workspace() -> None:
    sources = {source for _, source, _ in _bind_pairs(bwrap_argv(WORKSPACE, ["true"]))}
    assert sources == {"/", WORKSPACE}


def test_the_working_directory_is_the_workspace() -> None:
    argv = bwrap_argv(WORKSPACE, ["true"])
    assert argv[argv.index("--chdir") + 1] == "/workspace"


def test_tmp_is_a_one_gibibyte_tmpfs_sized_by_the_flag_before_it() -> None:
    argv = bwrap_argv(WORKSPACE, ["true"])
    tmpfs = argv.index("--tmpfs")
    assert argv[tmpfs + 1] == "/tmp"
    assert argv[tmpfs - 2 : tmpfs] == ["--size", str(ONE_GIB)]


def test_proc_and_dev_are_mounted_fresh() -> None:
    argv = bwrap_argv(WORKSPACE, ["true"])
    assert argv[argv.index("--proc") + 1] == "/proc"
    assert argv[argv.index("--dev") + 1] == "/dev"


def test_the_environment_is_cleared_before_the_worker_variables_are_set() -> None:
    argv = bwrap_argv(WORKSPACE, ["true"])
    assert argv.index("--clearenv") < argv.index("--setenv")
    assert _setenv_pairs(argv) == worker_env()


def test_the_network_is_shared_so_the_metadata_server_stays_reachable() -> None:
    """Documented limitation (DPES-804): sandboxed commands can reach the metadata server."""
    assert "--unshare-net" not in bwrap_argv(WORKSPACE, ["true"])


def test_a_relative_workspace_or_an_empty_command_is_a_programming_error() -> None:
    with pytest.raises(ValueError):
        bwrap_argv("relative/workspace", ["true"])
    with pytest.raises(ValueError):
        bwrap_argv(WORKSPACE, [])


# --- environment allowlist ------------------------------------------------------------------


def test_the_worker_environment_is_a_fixed_offline_allowlist(monkeypatch) -> None:
    monkeypatch.setenv("GOOGLE_APPLICATION_CREDENTIALS", "/secret/key.json")
    monkeypatch.setenv("ACTIONS_RUNTIME_TOKEN", "token-value")
    env = worker_env()
    assert set(env) == {
        "PATH",
        "HOME",
        "LANG",
        "PI_OFFLINE",
        "PI_SKIP_VERSION_CHECK",
        "PI_TELEMETRY",
    }
    assert env["PI_OFFLINE"] == "1"
    assert env["PI_SKIP_VERSION_CHECK"] == "1"
    assert env["PI_TELEMETRY"] == "0"


# --- qualification probe --------------------------------------------------------------------


def test_a_read_only_root_and_a_writable_workspace_qualify_the_sandbox() -> None:
    run = FakeRun(stdout=PROBE_OK)
    qualify_sandbox(run, WORKSPACE)
    assert len(run.calls) == 1
    assert run.calls[0][0] == "bwrap"
    assert ("--ro-bind", "/", "/") in _bind_pairs(run.calls[0])


def test_a_writable_root_is_refused() -> None:
    run = FakeRun(stdout="ROOT_WRITABLE\nWORKSPACE_WRITABLE\n")
    with pytest.raises(SandboxUnavailable, match="read-only"):
        qualify_sandbox(run, WORKSPACE)


def test_an_unwritable_workspace_is_refused() -> None:
    run = FakeRun(stdout="ROOT_READONLY\nWORKSPACE_UNWRITABLE\n")
    with pytest.raises(SandboxUnavailable, match="workspace is not writable"):
        qualify_sandbox(run, WORKSPACE)


def test_a_sandbox_that_does_not_start_is_refused() -> None:
    run = FakeRun(returncode=1, stdout="bwrap: No permissions to create new namespace")
    with pytest.raises(SandboxUnavailable, match="did not start"):
        qualify_sandbox(run, WORKSPACE)


def test_a_missing_bwrap_binary_is_refused() -> None:
    run = FakeRun(error=FileNotFoundError("bwrap"))
    with pytest.raises(SandboxUnavailable, match="bwrap"):
        qualify_sandbox(run, WORKSPACE)


def test_probe_output_that_proves_nothing_is_refused() -> None:
    run = FakeRun(stdout="")
    with pytest.raises(SandboxUnavailable, match="did not prove"):
        qualify_sandbox(run, WORKSPACE)


def test_the_workspace_argument_is_a_path_object_or_string() -> None:
    run = FakeRun(stdout=PROBE_OK)
    qualify_sandbox(run, Path(WORKSPACE))
    assert ("--bind", WORKSPACE, "/workspace") in _bind_pairs(run.calls[0])


def test_sandbox_path_keeps_the_installed_pi_binary_after_clearing_the_environment() -> None:
    environment = _setenv_pairs(bwrap_argv(WORKSPACE, ["pi", "--version"]))
    assert environment["PATH"].split(":")[0] == "/opt/pi/node_modules/.bin"


@pytest.mark.skipif(shutil.which("awk") is None, reason="awk is required")
@pytest.mark.parametrize(
    ("mountinfo", "expected"),
    [
        ("1 0 0:1 / / ro,nosuid - overlay overlay rw\n", "ROOT_READONLY"),
        ("1 0 0:1 / / rw,nosuid - overlay overlay rw\n", "ROOT_WRITABLE"),
        ("1 0 0:1 / / errors=remount-ro - overlay overlay rw\n", "ROOT_UNVERIFIED"),
        ("", "ROOT_UNVERIFIED"),
        ("1 0 0:1 / / ro - overlay overlay rw\n2 1 0:2 / / rw - tmpfs tmpfs rw\n", "ROOT_UNVERIFIED"),
    ],
)
def test_root_qualification_uses_the_root_mount_flags(mountinfo: str, expected: str) -> None:
    observed = subprocess.run(
        ["awk", ROOT_MOUNT_AWK], input=mountinfo, text=True, capture_output=True, check=True
    )
    assert observed.stdout.strip() == expected


def test_an_unproven_root_mount_refuses_even_when_workspace_writes_succeed() -> None:
    run = FakeRun(stdout="ROOT_UNVERIFIED\nWORKSPACE_WRITABLE\n")
    with pytest.raises(SandboxUnavailable, match="did not prove"):
        qualify_sandbox(run, WORKSPACE)
