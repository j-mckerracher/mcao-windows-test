"""Static checks over the worker image definition (DPES-811 AC1).

These read the Dockerfile and toolchain manifest as text. They cannot prove the image builds or
that pi starts; that proof needs a container daemon and is a stated gap in cloud/docs.
"""

import json
import re
import subprocess
from pathlib import Path

CLOUD = Path(__file__).resolve().parents[1]
DOCKERFILE = CLOUD / "Dockerfile"
MANIFEST = CLOUD / "toolchain.json"
PI_MANIFEST = CLOUD / "config" / "pi" / "package.json"
PI_LOCK = CLOUD / "config" / "pi" / "package-lock.json"
CHECK_UNDICI = CLOUD / "config" / "pi" / "check-undici.js"
FIXED_UNDICI = "8.12.2"

BASE_DIGEST = "sha256:294b683cb724975bec92580e1e685676bd4b50bda910ddb8c51d4cabeaec77e6"
APK_PACKAGES = {
    "bash",
    "bubblewrap",
    "ca-certificates",
    "fd",
    "git",
    "jq",
    "nodejs",
    "npm",
    "py3-pydantic",
    "python3",
    "ripgrep",
}


def _dockerfile() -> str:
    return DOCKERFILE.read_text(encoding="utf-8")


def _instructions() -> list[str]:
    """Logical instructions with line continuations joined, comments dropped."""
    joined: list[str] = []
    pending = ""
    for raw in _dockerfile().splitlines():
        line = raw.strip()
        if not line or line.startswith("#"):
            continue
        if line.endswith("\\"):
            pending += line[:-1] + " "
            continue
        joined.append(pending + line)
        pending = ""
    return joined


def _apk_add_packages() -> set[str]:
    for instruction in _instructions():
        match = re.search(r"apk add --no-cache (.+?)(?:\s&&|$)", instruction)
        if match:
            return set(match.group(1).split())
    raise AssertionError("no apk add --no-cache instruction found")


def test_base_image_is_alpine_3_24_2_pinned_by_digest() -> None:
    from_lines = [i for i in _instructions() if i.startswith("FROM ")]
    assert from_lines == [f"FROM alpine:3.24.2@{BASE_DIGEST}"]


def test_base_digest_is_the_one_recorded_in_the_toolchain_manifest() -> None:
    manifest = json.loads(MANIFEST.read_text(encoding="utf-8"))
    assert manifest["base_image"] == {"ref": "alpine:3.24.2", "digest": BASE_DIGEST}


def test_apk_upgrade_runs_before_the_pinned_package_install() -> None:
    instructions = _instructions()
    upgrade = next(i for i, text in enumerate(instructions) if "apk upgrade" in text)
    install = next(i for i, text in enumerate(instructions) if "apk add --no-cache" in text)
    assert upgrade < install


def test_the_apk_install_is_exactly_the_required_toolchain() -> None:
    assert _apk_add_packages() == APK_PACKAGES


def test_node_is_guarded_at_build_time_to_at_least_22_19() -> None:
    text = _dockerfile()
    assert "22.19" in text
    assert "process.exit(1)" in text


def test_pi_is_installed_from_the_lockfile_at_the_exact_pinned_version() -> None:
    manifest = json.loads(PI_MANIFEST.read_text(encoding="utf-8"))
    assert manifest["dependencies"] == {"@earendil-works/pi-coding-agent": "1.3.0"}
    lock = json.loads(PI_LOCK.read_text(encoding="utf-8"))
    assert lock["packages"]["node_modules/@earendil-works/pi-coding-agent"]["version"] == "1.3.0"


def test_npm_is_removed_after_the_pi_install_and_absent_at_the_end() -> None:
    # Install and removal share one RUN so npm never survives in a lower layer. Order is therefore
    # checked by position inside that single logical instruction, not by instruction index.
    instructions = _instructions()
    install_run = next(i for i in instructions if "npm ci" in i)
    assert "apk del npm" in install_run
    assert install_run.index("npm ci") < install_run.index("apk del npm")
    assert any("command -v npm" in text and "exit 1" in text for text in instructions)


def test_offline_and_telemetry_environment_is_fixed() -> None:
    env_lines = " ".join(i for i in _instructions() if i.startswith("ENV "))
    assert "PI_OFFLINE=1" in env_lines
    assert "PI_SKIP_VERSION_CHECK=1" in env_lines
    assert "PI_TELEMETRY=0" in env_lines


def test_the_image_runs_as_a_numeric_non_root_user() -> None:
    user_lines = [i for i in _instructions() if i.startswith("USER ")]
    assert len(user_lines) == 1
    uid = user_lines[0].split()[1].split(":")[0]
    assert uid.isdigit()
    assert int(uid) >= 10000


def test_the_bootstrap_code_is_root_owned_and_not_writable_by_the_worker() -> None:
    copies = [i for i in _instructions() if i.startswith("COPY ")]
    bootstrap_copies = [c for c in copies if "mao_cloud" in c or "toolchain.json" in c]
    assert bootstrap_copies
    for copy in bootstrap_copies:
        assert "--chown=root:root" in copy
        assert "--chmod=0555" in copy or "--chmod=0444" in copy


def test_the_entrypoint_is_the_worker_entry_and_its_only_fetch_is_the_intake() -> None:
    entrypoint = [i for i in _instructions() if i.startswith("ENTRYPOINT ")]
    assert entrypoint == ['ENTRYPOINT ["python3", "-m", "mao_cloud.worker_entry"]']
    assert not [i for i in _instructions() if i.startswith(("ADD ", "CMD "))]


def test_no_network_fetch_or_credential_material_is_baked_in() -> None:
    text = _dockerfile()
    forbidden = [
        r"\bADD\s+https?://",
        r"\bcurl\b.*\|\s*(sh|bash)",
        r"\bpip\s+install\b",
        r"GOOGLE_APPLICATION_CREDENTIALS",
        r"\bARG\b.*(TOKEN|SECRET|PASSWORD|KEY)",
        r"--privileged",
        r"^VOLUME\b",
        r"\.ssh|\.gitconfig|\.npmrc|\.config/gcloud|\.pi/auth",
    ]
    for pattern in forbidden:
        assert not re.search(pattern, text, flags=re.MULTILINE), pattern


def test_no_broad_copy_of_the_build_context_is_made() -> None:
    copies = [i for i in _instructions() if i.startswith("COPY ")]
    assert copies
    for copy in copies:
        sources = copy.split()[1:-1]
        assert "." not in sources
        assert not any(source.startswith("/") for source in sources)


def test_the_build_context_is_allowlisted_to_the_worker_sources() -> None:
    # The reusable build workflow uses the repository root as context, so this per-Dockerfile
    # ignore file must exclude everything first, then re-include only what the image copies.
    rules = [
        line.strip()
        for line in (CLOUD / "Dockerfile.dockerignore").read_text(encoding="utf-8").splitlines()
        if line.strip() and not line.strip().startswith("#")
    ]
    assert rules[0] == "**"
    for allowed in (
        "!cloud/src/mao_cloud/**",
        "!cloud/toolchain.json",
        "!cloud/config/pi/package.json",
        "!cloud/config/pi/package-lock.json",
        "!cloud/config/pi/check-undici.js",
    ):
        assert allowed in rules, allowed
    for excluded in ("**/__pycache__", "**/*.pyc", "**/.env*"):
        assert excluded in rules[rules.index("!cloud/toolchain.json") + 1 :], excluded


def test_the_toolchain_manifest_matches_the_dockerfile() -> None:
    manifest = json.loads(MANIFEST.read_text(encoding="utf-8"))
    assert manifest["pi"] == {"package": "@earendil-works/pi-coding-agent", "version": "1.3.0"}
    assert manifest["node"]["minimum"] == "22.19.0"
    assert set(manifest["apk_packages"]) == APK_PACKAGES
    assert manifest["environment"] == {
        "PI_OFFLINE": "1",
        "PI_SKIP_VERSION_CHECK": "1",
        "PI_TELEMETRY": "0",
    }
    assert manifest["startup_downloads"] == []
    assert manifest["sandbox"]["tool"] == "bubblewrap"


def test_the_pi_lockfile_resolves_undici_only_to_the_fixed_version() -> None:
    # pi pins undici exactly to 8.10.2, so the overrides entry must reach every undici copy.
    manifest = json.loads(PI_MANIFEST.read_text(encoding="utf-8"))
    assert manifest["overrides"] == {"undici": "8.12.2"}
    toolchain = json.loads(MANIFEST.read_text(encoding="utf-8"))
    assert toolchain["npm_overrides"] == manifest["overrides"]
    lock = json.loads(PI_LOCK.read_text(encoding="utf-8"))
    assert not [key for key, entry in lock["packages"].items() if entry.get("link")]
    undici_copies = {
        key: entry["version"]
        for key, entry in lock["packages"].items()
        if key and (entry.get("name") or key.rsplit("node_modules/", 1)[-1]) == "undici"
    }
    assert undici_copies == {"node_modules/undici": "8.12.2"}


def test_every_locked_package_has_a_registry_url_and_an_integrity_hash() -> None:
    lock = json.loads(PI_LOCK.read_text(encoding="utf-8"))
    installed = {key: entry for key, entry in lock["packages"].items() if key}
    assert installed
    for key, entry in installed.items():
        assert entry["resolved"].startswith("https://registry.npmjs.org/"), key
        assert entry["integrity"].startswith("sha512-"), key


def _write_package(directory: Path, name: str, version: str) -> None:
    directory.mkdir(parents=True, exist_ok=True)
    (directory / "package.json").write_text(json.dumps({"name": name, "version": version}), encoding="utf-8")


def _run_undici_check(tree: Path) -> subprocess.CompletedProcess[str]:
    return subprocess.run(
        ["node", str(CHECK_UNDICI), str(tree), FIXED_UNDICI],
        capture_output=True,
        text=True,
        check=False,
        timeout=60,
    )


def test_undici_check_accepts_a_tree_whose_every_copy_is_the_fixed_version(tmp_path: Path) -> None:
    _write_package(tmp_path / "node_modules" / "undici", "undici", FIXED_UNDICI)
    _write_package(tmp_path / "node_modules" / "pkg" / "node_modules" / "undici", "undici", FIXED_UNDICI)
    result = _run_undici_check(tmp_path)
    assert result.returncode == 0, result.stderr


def test_undici_check_rejects_a_nested_old_copy_even_when_the_top_level_is_fixed(tmp_path: Path) -> None:
    _write_package(tmp_path / "node_modules" / "undici", "undici", FIXED_UNDICI)
    _write_package(tmp_path / "node_modules" / "pkg" / "node_modules" / "undici", "undici", "8.10.2")
    result = _run_undici_check(tmp_path)
    assert result.returncode != 0
    assert "undici 8.10.2" in result.stderr


def test_undici_check_rejects_an_old_copy_nested_under_undici_itself(tmp_path: Path) -> None:
    _write_package(tmp_path / "node_modules" / "undici", "undici", FIXED_UNDICI)
    _write_package(tmp_path / "node_modules" / "undici" / "node_modules" / "undici", "undici", "8.10.2")
    result = _run_undici_check(tmp_path)
    assert result.returncode != 0
    assert "undici 8.10.2" in result.stderr


def test_undici_check_rejects_an_npm_alias_whose_package_is_the_old_copy(tmp_path: Path) -> None:
    _write_package(tmp_path / "node_modules" / "undici", "undici", FIXED_UNDICI)
    _write_package(tmp_path / "node_modules" / "undici-legacy", "undici", "8.10.2")
    result = _run_undici_check(tmp_path)
    assert result.returncode != 0
    assert "undici 8.10.2" in result.stderr


def test_undici_check_rejects_a_symlinked_old_copy(tmp_path: Path) -> None:
    _write_package(tmp_path / "node_modules" / "undici", "undici", FIXED_UNDICI)
    old_copy = tmp_path / "elsewhere" / "undici"
    _write_package(old_copy, "undici", "8.10.2")
    (tmp_path / "node_modules" / "pkg" / "node_modules").mkdir(parents=True)
    (tmp_path / "node_modules" / "pkg" / "node_modules" / "undici").symlink_to(old_copy, target_is_directory=True)
    result = _run_undici_check(tmp_path)
    assert result.returncode != 0
    assert "undici 8.10.2" in result.stderr


def test_undici_check_accepts_a_symlinked_fixed_copy_and_npm_bin_links(tmp_path: Path) -> None:
    _write_package(tmp_path / "node_modules" / "undici", "undici", FIXED_UNDICI)
    (tmp_path / "node_modules" / "undici" / "cli.js").write_text("", encoding="utf-8")
    fixed_copy = tmp_path / "elsewhere" / "undici"
    _write_package(fixed_copy, "undici", FIXED_UNDICI)
    (tmp_path / "node_modules" / "pkg" / "node_modules").mkdir(parents=True)
    (tmp_path / "node_modules" / "pkg" / "node_modules" / "undici").symlink_to(fixed_copy, target_is_directory=True)
    (tmp_path / "node_modules" / ".bin").mkdir()
    (tmp_path / "node_modules" / ".bin" / "undici").symlink_to(tmp_path / "node_modules" / "undici" / "cli.js")
    result = _run_undici_check(tmp_path)
    assert result.returncode == 0, result.stderr


def test_undici_check_ignores_other_packages_and_undici_named_folders_in_packages(tmp_path: Path) -> None:
    _write_package(tmp_path / "node_modules" / "undici", "undici", FIXED_UNDICI)
    _write_package(tmp_path / "node_modules" / "undici-types", "undici-types", "6.21.0")
    _write_package(tmp_path / "node_modules" / "pkg" / "test" / "fixtures" / "undici", "undici", "8.10.2")
    result = _run_undici_check(tmp_path)
    assert result.returncode == 0, result.stderr


def test_undici_check_fails_closed_on_a_dangling_symlink(tmp_path: Path) -> None:
    _write_package(tmp_path / "node_modules" / "undici", "undici", FIXED_UNDICI)
    (tmp_path / "node_modules" / "pkg" / "node_modules").mkdir(parents=True)
    (tmp_path / "node_modules" / "pkg" / "node_modules" / "undici").symlink_to(tmp_path / "missing")
    result = _run_undici_check(tmp_path)
    assert result.returncode != 0
    assert "cannot resolve" in result.stderr


def test_undici_check_terminates_on_a_symlink_cycle(tmp_path: Path) -> None:
    _write_package(tmp_path / "node_modules" / "undici", "undici", FIXED_UNDICI)
    (tmp_path / "node_modules" / "pkg" / "node_modules").mkdir(parents=True)
    (tmp_path / "node_modules" / "pkg" / "node_modules" / "loop").symlink_to(
        tmp_path / "node_modules" / "pkg", target_is_directory=True
    )
    result = _run_undici_check(tmp_path)
    assert result.returncode == 0, result.stderr


def test_undici_check_rejects_a_tree_with_no_undici(tmp_path: Path) -> None:
    _write_package(tmp_path / "node_modules" / "pkg", "pkg", "1.0.0")
    result = _run_undici_check(tmp_path)
    assert result.returncode != 0
