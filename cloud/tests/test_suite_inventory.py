"""Test inventory guard for the cloud Python suite (DPES-819 AC1).

Every module under cloud/tests is either a collected test module or a named support module. A new
file that pytest would silently skip, or a support module that nobody declared, fails here.
"""

import re
from pathlib import Path

import tomllib

CLOUD = Path(__file__).resolve().parents[1]
TESTS = CLOUD / "tests"
COLLECTED = re.compile(r"^(test_[A-Za-z0-9_]+|[A-Za-z0-9_]+_test)\.py$")
SUPPORT = {
    "auth_support.py",
    "conftest.py",
    "continuation_fakes.py",
    "fakes.py",
    "firestore_fake.py",
    "flows.py",
    "gitfixtures.py",
    "result_fixtures.py",
    "storage_fakes.py",
    "submit_fakes.py",
}


def test_every_test_module_matches_the_pytest_collection_pattern_or_is_declared_support() -> None:
    unexplained = [
        path.name
        for path in sorted(TESTS.glob("*.py"))
        if not COLLECTED.match(path.name) and path.name not in SUPPORT
    ]
    assert unexplained == []


def test_every_declared_support_module_exists() -> None:
    missing = [name for name in sorted(SUPPORT) if not (TESTS / name).is_file()]
    assert missing == []


def test_pytest_is_pinned_to_the_tests_directory_and_fails_on_unknown_markers() -> None:
    config = tomllib.loads((CLOUD / "pyproject.toml").read_text(encoding="utf-8"))
    options = config["tool"]["pytest"]["ini_options"]
    assert options["testpaths"] == ["tests"]
    assert "--strict-markers" in options["addopts"]


def test_the_collected_suite_is_not_empty() -> None:
    collected = [p for p in TESTS.glob("*.py") if COLLECTED.match(p.name)]
    assert len(collected) >= 30
