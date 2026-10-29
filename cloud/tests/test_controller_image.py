"""Static checks over the controller image definition (DPES-819 AC1, AC2).

The controller is a separate non-root image from the worker. These read the Dockerfile and the
exported, hash-locked requirements as text. They cannot prove the image builds or that the
server starts; that needs a container daemon and is a stated gap in cloud/docs/release-and-rollback.md.
"""

import json
import re
from pathlib import Path

import pytest

from fakes import policy_document
from mao_cloud import serve

CLOUD = Path(__file__).resolve().parents[1]
CONTROLLER_DOCKERFILE = CLOUD / "Dockerfile.controller"
WORKER_DOCKERFILE = CLOUD / "Dockerfile"
REQUIREMENTS = CLOUD / "requirements" / "controller.txt"
MANIFEST = CLOUD / "toolchain.json"

BASE_DIGEST = "sha256:294b683cb724975bec92580e1e685676bd4b50bda910ddb8c51d4cabeaec77e6"
CONTROLLER_RUNTIME = {
    "fastapi",
    "uvicorn",
    "google-auth",
    "google-cloud-firestore",
    "google-cloud-storage",
    "pydantic",
}
# cryptography is deliberately absent: google-auth 2.62 requires it at runtime (uv tree --invert),
# so the controller legitimately carries it. Only test and lint tooling is dev-only.
DEV_ONLY = {"pytest", "httpx", "iniconfig", "pluggy", "pygments"}


def _instructions(path: Path) -> list[str]:
    joined: list[str] = []
    pending = ""
    for raw in path.read_text(encoding="utf-8").splitlines():
        line = raw.strip()
        if not line or line.startswith("#"):
            continue
        if line.endswith("\\"):
            pending += line[:-1] + " "
            continue
        joined.append(pending + line)
        pending = ""
    return joined


def _controller() -> list[str]:
    return _instructions(CONTROLLER_DOCKERFILE)


def _copy_sources(instruction: str) -> list[str]:
    """Source paths of one COPY instruction: flags and the destination removed."""
    return [token for token in instruction.split()[1:-1] if not token.startswith("--")]


def _requirement_names() -> set[str]:
    names: set[str] = set()
    for line in REQUIREMENTS.read_text(encoding="utf-8").splitlines():
        if not line or line.startswith((" ", "#", "--")):
            continue
        match = re.match(r"^([A-Za-z0-9._-]+)==", line)
        assert match, f"unpinned requirement line: {line!r}"
        names.add(match.group(1).lower())
    return names


def test_controller_base_is_the_same_digest_pinned_alpine_as_the_worker() -> None:
    from_lines = [i for i in _controller() if i.startswith("FROM ")]
    assert from_lines == [f"FROM alpine:3.24.2@{BASE_DIGEST}"]
    manifest = json.loads(MANIFEST.read_text(encoding="utf-8"))
    assert manifest["base_image"]["digest"] == BASE_DIGEST


def test_controller_runs_as_a_numeric_non_root_user() -> None:
    user_lines = [i for i in _controller() if i.startswith("USER ")]
    assert len(user_lines) == 1
    uid = user_lines[0].split()[1].split(":")[0]
    assert uid.isdigit()
    assert int(uid) >= 10000


def test_controller_installs_only_hash_locked_runtime_dependencies() -> None:
    pip_runs = [i for i in _controller() if "pip install" in i]
    assert len(pip_runs) == 1
    assert "--require-hashes" in pip_runs[0]
    assert "--no-deps" in pip_runs[0]
    assert "-r /opt/mao-cloud/requirements/controller.txt" in pip_runs[0]


def test_every_controller_requirement_is_pinned_with_a_sha256_hash() -> None:
    lines = REQUIREMENTS.read_text(encoding="utf-8").splitlines()
    pins = [line for line in lines if re.match(r"^[A-Za-z0-9._-]+==", line)]
    assert pins
    text = REQUIREMENTS.read_text(encoding="utf-8")
    assert text.count("--hash=sha256:") >= len(pins)


def test_controller_requirements_are_the_runtime_extra_and_no_dev_tooling() -> None:
    names = _requirement_names()
    assert CONTROLLER_RUNTIME <= names
    assert not (names & DEV_ONLY)


def test_controller_image_has_no_worker_runtime_pi_node_or_npm() -> None:
    text = CONTROLLER_DOCKERFILE.read_text(encoding="utf-8")
    for forbidden in ("pi-coding-agent", "nodejs", "npm install", "bubblewrap"):
        assert forbidden not in text, forbidden


def test_controller_copies_only_the_service_source_and_is_root_owned_read_only() -> (
    None
):
    copies = [i for i in _controller() if i.startswith("COPY ")]
    assert copies
    for copy in copies:
        assert "--chown=root:root" in copy
        assert "--chmod=0555" in copy or "--chmod=0444" in copy
        assert "." not in _copy_sources(copy)


def test_controller_sources_are_read_from_the_cloud_directory_of_the_repo_root_context() -> (
    None
):
    # The reusable build-image workflow builds with the repository root as context, so every
    # source path is written relative to that root.
    copies = [i for i in _controller() if i.startswith("COPY ")]
    sources = [s for c in copies for s in _copy_sources(c)]
    assert "cloud/src/mao_cloud" in sources
    assert "cloud/requirements/controller.txt" in sources
    assert all(s.startswith("cloud/") for s in sources)


def test_controller_entrypoint_is_the_serve_module() -> None:
    entrypoint = [i for i in _controller() if i.startswith("ENTRYPOINT ")]
    assert entrypoint == ['ENTRYPOINT ["python3", "-m", "mao_cloud.serve"]']


def test_controller_fetches_nothing_at_start_and_bakes_no_credentials() -> None:
    text = CONTROLLER_DOCKERFILE.read_text(encoding="utf-8")
    forbidden = [
        r"\bADD\s+https?://",
        r"\bcurl\b.*\|\s*(sh|bash)",
        r"GOOGLE_APPLICATION_CREDENTIALS",
        r"\bARG\b.*(TOKEN|SECRET|PASSWORD|KEY)",
        r"--privileged",
        r"^VOLUME\b",
        r"\.ssh|\.gitconfig|\.npmrc|\.config/gcloud|\.pi/auth",
    ]
    for pattern in forbidden:
        assert not re.search(pattern, text, flags=re.MULTILINE), pattern
    assert not [i for i in _controller() if i.startswith(("ADD ", "CMD "))]


def test_worker_dockerfile_copies_are_repo_root_relative_after_the_context_change() -> (
    None
):
    copies = [i for i in _instructions(WORKER_DOCKERFILE) if i.startswith("COPY ")]
    sources = [s for c in copies for s in _copy_sources(c)]
    assert "cloud/src/mao_cloud" in sources
    assert "cloud/toolchain.json" in sources
    assert all(s.startswith("cloud/") for s in sources)


def test_the_controller_entrypoint_refuses_to_start_without_production_configuration(
    tmp_path: Path, capsys: pytest.CaptureFixture[str]
) -> None:
    # With a valid policy but no MAO_CLOUD_* settings, the image's serve entrypoint refuses with
    # exit 2, names the missing settings, and binds nothing. The environment is passed explicitly so
    # that the result does not depend on the shell that runs the test.
    policy = tmp_path / "policy.json"
    document = {**policy_document(), "api_audience": "https://mao-cloud.example.test"}
    policy.write_text(json.dumps(document), encoding="utf-8")
    assert serve.main(["--policy", str(policy)], environ={}) == 2
    assert (
        "missing required configuration: MAO_CLOUD_PROJECT" in capsys.readouterr().err
    )
