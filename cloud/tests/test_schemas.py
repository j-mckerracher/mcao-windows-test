"""Committed protocol-v1 schemas: they must equal the models, and BLOCKING markers must be present."""

import json
import shutil
from pathlib import Path
from typing import Any

from mao_cloud import schemas
from mao_cloud.protocol import SCHEMA_MODELS

COMMITTED = Path(__file__).resolve().parents[1] / "schemas" / "protocol-v1"
STALE_MESSAGE = "stale {name}; regenerate with `mao-cloud schema export`"


def _blocking_markers(node: Any, path: str = "") -> dict[str, list[str]]:
    """Every x-blocking marker in a schema tree, keyed by its JSON path."""
    found: dict[str, list[str]] = {}
    if isinstance(node, dict):
        if "x-blocking" in node:
            found[path] = node["x-blocking"]
        for key, value in node.items():
            found.update(_blocking_markers(value, f"{path}/{key}"))
    elif isinstance(node, list):
        for index, value in enumerate(node):
            found.update(_blocking_markers(value, f"{path}/{index}"))
    return found


def _copy_committed(tmp_path: Path) -> Path:
    target = tmp_path / "protocol-v1"
    shutil.copytree(COMMITTED, target)
    return target


# --- Committed files match the models -----------------------------------------------------------


def test_committed_schemas_match_the_rendered_models() -> None:
    assert schemas.check(COMMITTED) == []


def test_committed_schema_contents_equal_the_rendered_text() -> None:
    rendered = schemas.render_all()
    committed = {path.name: path.read_text(encoding="utf-8") for path in COMMITTED.glob("*.json")}
    assert committed == rendered


def test_committed_directory_holds_exactly_one_file_per_schema_model() -> None:
    committed_names = sorted(path.name for path in COMMITTED.glob("*.json"))
    assert committed_names == sorted(f"{name}.json" for name in SCHEMA_MODELS)
    assert len(committed_names) == 21


def test_rendering_is_deterministic() -> None:
    assert schemas.render_all() == schemas.render_all()


def test_rendered_text_is_sorted_indented_json_with_a_final_newline() -> None:
    for name, text in schemas.render_all().items():
        assert text == json.dumps(json.loads(text), indent=2, sort_keys=True) + "\n", name


# --- BLOCKING markers ---------------------------------------------------------------------------


def test_blocking_markers_are_exactly_the_ones_the_brief_names() -> None:
    markers: dict[tuple[str, str], list[str]] = {}
    for name, text in schemas.render_all().items():
        for path, tickets in _blocking_markers(json.loads(text)).items():
            markers[(name, path)] = tickets
    assert markers == {
        ("create-run-request.json", "/$defs/Budget/properties/restart_budget"): ["DPES-870"],
        ("worker-bootstrap.json", "/properties/checkpoint_ref"): ["DPES-869"],
        ("worker-bootstrap.json", "/properties/continuation_budget"): ["DPES-870"],
        ("worker-bootstrap.json", "/properties/durable_root"): ["DPES-869"],
        # The intake embeds the worker bootstrap, so it carries the same three blocked fields.
        ("worker-intake.json", "/$defs/WorkerBootstrap/properties/checkpoint_ref"): ["DPES-869"],
        ("worker-intake.json", "/$defs/WorkerBootstrap/properties/continuation_budget"): [
            "DPES-870"
        ],
        ("worker-intake.json", "/$defs/WorkerBootstrap/properties/durable_root"): ["DPES-869"],
    }


# --- Export and check ---------------------------------------------------------------------------


def test_export_writes_every_schema_and_the_result_checks_clean(tmp_path: Path) -> None:
    written = schemas.export(tmp_path / "out")
    assert len(written) == 21
    assert schemas.check(tmp_path / "out") == []


def test_export_creates_missing_parent_directories(tmp_path: Path) -> None:
    schemas.export(tmp_path / "deep" / "nested" / "dir")
    assert (tmp_path / "deep" / "nested" / "dir" / "worker-bootstrap.json").is_file()


def test_a_fresh_copy_of_the_committed_directory_checks_clean(tmp_path: Path) -> None:
    assert schemas.check(_copy_committed(tmp_path)) == []


def test_a_modified_schema_is_reported_as_stale(tmp_path: Path) -> None:
    target = _copy_committed(tmp_path)
    (target / "worker-bootstrap.json").write_text("{}\n", encoding="utf-8")
    assert schemas.check(target) == [STALE_MESSAGE.format(name="worker-bootstrap.json")]


def test_a_removed_schema_is_reported_as_missing(tmp_path: Path) -> None:
    target = _copy_committed(tmp_path)
    (target / "claim-document.json").unlink()
    assert schemas.check(target) == ["missing claim-document.json"]


def test_an_extra_json_file_is_reported_as_unexpected(tmp_path: Path) -> None:
    target = _copy_committed(tmp_path)
    (target / "extra.json").write_text("{}\n", encoding="utf-8")
    assert schemas.check(target) == ["unexpected extra.json"]


def test_a_missing_directory_is_reported_as_not_a_directory(tmp_path: Path) -> None:
    absent = tmp_path / "absent"
    assert schemas.check(absent) == [f"{absent} is not a directory"]


def test_a_file_path_is_reported_as_not_a_directory(tmp_path: Path) -> None:
    file_path = tmp_path / "plain.txt"
    file_path.write_text("x", encoding="utf-8")
    assert schemas.check(file_path) == [f"{file_path} is not a directory"]
