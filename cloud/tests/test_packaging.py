"""Packaging metadata: console scripts, extras, and the committed lock agree with pyproject.toml.

Nothing here builds a wheel or an image. The checks read the committed metadata, import each
console script's module, and prove the split the extras claim: the CLI, runner, and sweeper import
with the controller packages blocked, while the broker does not.
"""

import importlib
import os
import re
import subprocess
import sys
import tomllib
from pathlib import Path
from typing import Any

import pytest

CLOUD = Path(__file__).resolve().parents[1]
PYPROJECT = tomllib.loads((CLOUD / "pyproject.toml").read_text(encoding="utf-8"))
LOCK = tomllib.loads((CLOUD / "uv.lock").read_text(encoding="utf-8"))
PROJECT: dict[str, Any] = PYPROJECT["project"]
SCRIPTS: dict[str, str] = PROJECT["scripts"]
EXTRAS: dict[str, list[str]] = PROJECT["optional-dependencies"]
CONTROLLER_ONLY = (
    "fastapi",
    "uvicorn",
    "google.cloud.storage",
    "google.cloud.firestore",
)

PROBE = """
import importlib
import sys

blocked = tuple(sys.argv[1].split(","))


class Blocker:
    def find_spec(self, name, path=None, target=None):
        if any(name == package or name.startswith(package + ".") for package in blocked):
            raise ImportError(f"controller-only package blocked: {name}")
        return None


sys.meta_path.insert(0, Blocker())
for module in sys.argv[2].split(","):
    importlib.import_module(module)
"""


def _names(specs: list[str]) -> set[str]:
    return {
        re.split(r"[<>=!~;\[ ]", spec, maxsplit=1)[0].strip().lower() for spec in specs
    }


def _lock_names(entries: list[dict[str, str]]) -> set[str]:
    return {entry["name"] for entry in entries}


def _lock_entry() -> dict[str, Any]:
    entries = [
        package for package in LOCK["package"] if package["name"] == PROJECT["name"]
    ]
    assert len(entries) == 1, entries
    entry: dict[str, Any] = entries[0]
    return entry


def _probe(
    modules: list[str], blocked: tuple[str, ...]
) -> subprocess.CompletedProcess[str]:
    return subprocess.run(
        [sys.executable, "-c", PROBE, ",".join(blocked), ",".join(modules)],
        capture_output=True,
        text=True,
        check=False,
        env={**os.environ, "PYTHONPATH": str(CLOUD / "src")},
    )


def test_console_scripts_are_the_four_reviewed_entry_points() -> None:
    assert SCRIPTS == {
        "mao-cloud": "mao_cloud.cli:main",
        "mao-cloud-broker": "mao_cloud.serve:main",
        "mao-cloud-runner": "mao_cloud.runner:main",
        "mao-cloud-sweeper": "mao_cloud.sweeper:main",
    }


@pytest.mark.parametrize("name", sorted(SCRIPTS))
def test_console_script_names_an_importable_main(name: str) -> None:
    module_name, _, attribute = SCRIPTS[name].partition(":")
    assert attribute == "main", SCRIPTS[name]
    assert callable(getattr(importlib.import_module(module_name), attribute))


def test_core_install_depends_only_on_pydantic() -> None:
    assert _names(PROJECT["dependencies"]) == {"pydantic"}


def test_runner_extra_adds_only_google_auth() -> None:
    assert _names(EXTRAS["runner"]) == {"google-auth"}


def test_controller_extra_carries_the_broker_dependencies() -> None:
    assert _names(EXTRAS["controller"]) == {
        "fastapi",
        "uvicorn",
        "google-auth",
        "google-cloud-firestore",
        "google-cloud-storage",
        "requests",
    }


def test_controller_extra_pins_the_firestore_sdk_exactly() -> None:
    # The SDK is pinned by exact version: the port's transaction and query semantics were verified
    # against this release only. A version bump is a deliberate change with its own verification.
    assert "google-cloud-firestore==2.34.1" in EXTRAS["controller"]


@pytest.mark.parametrize(
    "module", ["mao_cloud.cli", "mao_cloud.runner", "mao_cloud.sweeper"]
)
def test_non_controller_entry_points_import_without_controller_packages(
    module: str,
) -> None:
    result = _probe([module], CONTROLLER_ONLY)
    assert result.returncode == 0, result.stderr


def test_broker_entry_point_needs_the_controller_packages() -> None:
    result = _probe(["mao_cloud.serve"], CONTROLLER_ONLY)
    assert result.returncode != 0
    assert "controller-only package blocked" in result.stderr


def test_firestore_port_module_loads_without_the_firestore_sdk() -> None:
    # The port module must not import the SDK at load: only the factory's lazy import reaches it.
    # Only the SDK is blocked here; the module's other controller imports are not this check's concern.
    result = _probe(["mao_cloud.firestore_store"], ("google.cloud.firestore",))
    assert result.returncode == 0, result.stderr


def test_firestore_sdk_adapter_needs_the_firestore_sdk() -> None:
    result = _probe(["mao_cloud.firestore_sdk"], CONTROLLER_ONLY)
    assert result.returncode != 0
    assert "controller-only package blocked: google.cloud.firestore" in result.stderr


def test_lock_records_the_project_version_and_editable_source() -> None:
    entry = _lock_entry()
    assert entry["version"] == PROJECT["version"]
    assert entry["source"] == {"editable": "."}


def test_lock_requires_the_same_core_and_extra_dependencies_as_pyproject() -> None:
    entry = _lock_entry()
    assert _lock_names(entry["dependencies"]) == _names(PROJECT["dependencies"])
    assert set(entry["optional-dependencies"]) == set(EXTRAS)
    for extra, specs in EXTRAS.items():
        assert _lock_names(entry["optional-dependencies"][extra]) == _names(specs), (
            extra
        )


def test_lock_metadata_declares_the_same_extras_and_requirements() -> None:
    metadata = _lock_entry()["metadata"]
    assert set(metadata["provides-extras"]) == set(EXTRAS)
    declared = {requirement["name"] for requirement in metadata["requires-dist"]}
    expected = _names(PROJECT["dependencies"])
    for specs in EXTRAS.values():
        expected |= _names(specs)
    assert declared == expected
