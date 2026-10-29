"""Trusted sandbox: the bubblewrap argv the worker runs under, and the probe that qualifies it.

DPES-811 AC3 and AC6. The whole worker process runs inside bubblewrap. The root filesystem is
read-only, only the workspace is writable from the host, and the environment is a fixed allowlist.
If the sandbox cannot start, or does not prove a read-only root and a writable workspace, the
attempt refuses. Nothing falls back to an unsandboxed run.

The network is shared on purpose. Sandboxed commands can reach the metadata server and so mint the
worker service account token. That is a documented limitation, not a control; it is qualified in
DPES-804 and must not be described as isolated.
"""

from collections.abc import Callable
from pathlib import Path

from mao_cloud.trusted_profiles import TMP_GIB

GiB = 1024 * 1024 * 1024
WORKSPACE_MOUNT = "/workspace"
TMP_MOUNT = "/tmp"

# Fixed, offline, and independent of the launcher's environment. Nothing inherited is passed on.
_WORKER_ENV = {
    "PATH": "/opt/pi/node_modules/.bin:/usr/local/bin:/usr/bin:/bin",
    "HOME": TMP_MOUNT,
    "LANG": "C.UTF-8",
    "PI_OFFLINE": "1",
    "PI_SKIP_VERSION_CHECK": "1",
    "PI_TELEMETRY": "0",
}

# Mount flags establish read-only confinement independently of the worker's UID permissions.
ROOT_MOUNT_AWK = """
$5 == "/" {
    roots++
    count = split($6, options, ",")
    ro = 0
    rw = 0
    for (i = 1; i <= count; i++) {
        if (options[i] == "ro") ro = 1
        if (options[i] == "rw") rw = 1
    }
    readonly += (ro && !rw)
    writable += rw
}
END {
    if (roots == 1 && readonly == 1) print "ROOT_READONLY"
    else if (roots == 1 && writable == 1) print "ROOT_WRITABLE"
    else print "ROOT_UNVERIFIED"
}
"""

PROBE_SCRIPT = (
    f"awk '{ROOT_MOUNT_AWK}' /proc/self/mountinfo\n"
    "if : > /workspace/.mao-probe-$$ 2>/dev/null; then\n"
    "  rm -f /workspace/.mao-probe-$$\n"
    "  echo WORKSPACE_WRITABLE\n"
    "else\n"
    "  echo WORKSPACE_UNWRITABLE\n"
    "fi\n"
)

Run = Callable[[list[str]], tuple[int, str]]


class SandboxUnavailable(Exception):
    """The sandbox cannot be used. The message is the missing requirement, for the durable record."""


def worker_env() -> dict[str, str]:
    """The worker's full environment. A copy, so callers cannot widen the allowlist in place."""
    return dict(_WORKER_ENV)


def bwrap_argv(workspace: str, command: list[str]) -> list[str]:
    """The argv that runs command inside the sandbox. The workspace must be an absolute host path."""
    if not workspace.startswith("/"):
        raise ValueError("the sandbox workspace must be an absolute host path")
    if not command:
        raise ValueError("the sandbox needs a command to run")
    argv = [
        "bwrap",
        "--die-with-parent",
        "--new-session",
        "--unshare-pid",
        "--unshare-ipc",
        "--unshare-uts",
        "--ro-bind",
        "/",
        "/",
        "--proc",
        "/proc",
        "--dev",
        "/dev",
        "--size",
        str(TMP_GIB * GiB),
        "--tmpfs",
        TMP_MOUNT,
        "--bind",
        workspace,
        WORKSPACE_MOUNT,
        "--chdir",
        WORKSPACE_MOUNT,
        "--clearenv",
    ]
    for name, value in _WORKER_ENV.items():
        argv += ["--setenv", name, value]
    return [*argv, "--", *command]


def qualify_sandbox(run: Run, workspace: str | Path) -> None:
    """Prove the sandbox starts, the root is read-only, and the workspace is writable.

    run executes an argv and returns (returncode, stdout). Raises SandboxUnavailable otherwise.
    """
    argv = bwrap_argv(str(workspace), ["/bin/sh", "-c", PROBE_SCRIPT])
    try:
        returncode, stdout = run(argv)
    except OSError as exc:
        raise SandboxUnavailable(f"bwrap could not be started ({type(exc).__name__})") from exc
    if returncode != 0:
        raise SandboxUnavailable(f"the sandbox did not start (bwrap exited {returncode})")
    seen = set(stdout.split())
    if "ROOT_WRITABLE" in seen:
        raise SandboxUnavailable(
            "the root filesystem is writable inside the sandbox; it must be read-only"
        )
    if "ROOT_READONLY" not in seen:
        raise SandboxUnavailable("the probe did not prove a read-only root filesystem")
    if "WORKSPACE_UNWRITABLE" in seen:
        raise SandboxUnavailable("the workspace is not writable inside the sandbox")
    if "WORKSPACE_WRITABLE" not in seen:
        raise SandboxUnavailable("the probe did not prove the workspace is writable")
