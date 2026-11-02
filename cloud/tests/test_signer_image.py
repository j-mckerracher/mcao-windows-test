"""Static checks over the signer image definition (DPES-899 AC1, AC2).

The signer is a separate non-root image from the controller, and it is the only image that signs.
These read the Dockerfile, its build-context allowlist and the exported requirements as text. They
cannot prove the image builds or that the server starts; that needs a container daemon and is a
stated gap in cloud/docs/release-and-rollback.md.
"""

import re
from pathlib import Path

CLOUD = Path(__file__).resolve().parents[1]
SIGNER_DOCKERFILE = CLOUD / "Dockerfile.signer"
SIGNER_DOCKERIGNORE = CLOUD / "Dockerfile.signer.dockerignore"
CONTROLLER_DOCKERFILE = CLOUD / "Dockerfile.controller"
REQUIREMENTS = CLOUD / "requirements" / "controller.txt"
SIGNER_ENTRYPOINT_MODULE = CLOUD / "src" / "mao_cloud" / "signer_serve.py"

BASE_DIGEST = "sha256:294b683cb724975bec92580e1e685676bd4b50bda910ddb8c51d4cabeaec77e6"
# The signer runs the controller's hash-locked runtime: the same export, so the same packages.
CONTROLLER_RUNTIME = {
    "fastapi",
    "uvicorn",
    "google-auth",
    "google-cloud-firestore",
    "google-cloud-storage",
    "pydantic",
}
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


def _signer() -> list[str]:
    return _instructions(SIGNER_DOCKERFILE)


def _signer_user_id() -> int:
    user_lines = [i for i in _signer() if i.startswith("USER ")]
    assert len(user_lines) == 1
    return int(user_lines[0].split()[1].split(":")[0])


def test_signer_base_is_the_same_digest_pinned_alpine_as_the_controller() -> None:
    from_lines = [i for i in _signer() if i.startswith("FROM ")]
    controller_from = [
        i for i in _instructions(CONTROLLER_DOCKERFILE) if i.startswith("FROM ")
    ]
    assert from_lines == [f"FROM alpine:3.24.2@{BASE_DIGEST}"]
    assert from_lines == controller_from


def test_signer_runs_as_a_numeric_non_root_user_distinct_from_the_controller() -> None:
    uid = _signer_user_id()
    controller_uid = int(
        [i for i in _instructions(CONTROLLER_DOCKERFILE) if i.startswith("USER ")][0]
        .split()[1]
        .split(":")[0]
    )
    assert uid >= 10000
    assert uid != controller_uid


def test_the_signer_user_is_the_one_the_image_creates() -> None:
    # USER names a numeric uid, so the account it runs as must be created with that uid.
    creation = " ".join(i for i in _signer() if "adduser" in i or "addgroup" in i)
    assert re.search(rf"-g {_signer_user_id()}\b", creation)
    assert re.search(rf"-u {_signer_user_id()}\b", creation)


def test_signer_installs_only_the_hash_locked_runtime_the_controller_carries() -> None:
    pip_runs = [i for i in _signer() if "pip install" in i]
    assert len(pip_runs) == 1
    assert "--require-hashes" in pip_runs[0]
    assert "--no-deps" in pip_runs[0]
    assert "-r /opt/mao-cloud/requirements/controller.txt" in pip_runs[0]


def test_signer_requirements_are_the_controller_runtime_set_and_no_dev_tooling() -> (
    None
):
    # The signer reuses the controller's hash-locked export: it needs the same runtime (Firestore,
    # Storage, token verification) and nothing from the dev group.
    names = _requirement_names()
    assert CONTROLLER_RUNTIME <= names
    assert not (names & DEV_ONLY)
    copies = [i for i in _signer() if i.startswith("COPY ")]
    assert "cloud/requirements/controller.txt" in [
        s for c in copies for s in _copy_sources(c)
    ]


def test_signer_entrypoint_is_the_signer_module_and_not_the_controller() -> None:
    entrypoint = [i for i in _signer() if i.startswith("ENTRYPOINT ")]
    assert entrypoint == ['ENTRYPOINT ["python3", "-m", "mao_cloud.signer_serve"]']
    assert SIGNER_ENTRYPOINT_MODULE.is_file()
    assert not [i for i in _signer() if "mao_cloud.serve" in i]


def test_signer_image_has_no_worker_runtime_pi_node_or_npm() -> None:
    text = SIGNER_DOCKERFILE.read_text(encoding="utf-8")
    for forbidden in ("pi-coding-agent", "nodejs", "npm install", "bubblewrap"):
        assert forbidden not in text, forbidden


def test_signer_copies_only_the_service_source_and_is_root_owned_read_only() -> None:
    copies = [i for i in _signer() if i.startswith("COPY ")]
    assert copies
    for copy in copies:
        assert "--chown=root:root" in copy
        assert "--chmod=0555" in copy or "--chmod=0444" in copy
        assert "." not in _copy_sources(copy)


def test_signer_sources_are_read_from_the_cloud_directory_of_the_repo_root_context() -> (
    None
):
    copies = [i for i in _signer() if i.startswith("COPY ")]
    sources = [s for c in copies for s in _copy_sources(c)]
    assert "cloud/src/mao_cloud" in sources
    assert "cloud/requirements/controller.txt" in sources
    assert all(s.startswith("cloud/") for s in sources)


def test_signer_fetches_nothing_at_start_and_bakes_no_credentials() -> None:
    text = SIGNER_DOCKERFILE.read_text(encoding="utf-8")
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
    assert not [i for i in _signer() if i.startswith(("ADD ", "CMD "))]


def test_signer_build_context_allows_exactly_what_the_image_copies() -> None:
    # Everything is excluded first, then re-included by name. The allowlist and the COPY sources
    # must name the same paths, so the context cannot carry a file the image never reads.
    allowed = {
        line[1:].removesuffix("/**").strip()
        for line in SIGNER_DOCKERIGNORE.read_text(encoding="utf-8").splitlines()
        if line.startswith("!")
    }
    copies = [i for i in _signer() if i.startswith("COPY ")]
    sources = {s.removesuffix("/**") for c in copies for s in _copy_sources(c)}
    assert allowed == sources
    lines = SIGNER_DOCKERIGNORE.read_text(encoding="utf-8").splitlines()
    assert lines.index("**") < min(
        i for i, line in enumerate(lines) if line.startswith("!")
    )


def test_signer_build_context_excludes_env_files_and_bytecode() -> None:
    text = SIGNER_DOCKERIGNORE.read_text(encoding="utf-8")
    assert "**/.env*" in text
    assert "**/__pycache__" in text
    assert "**/*.pyc" in text
