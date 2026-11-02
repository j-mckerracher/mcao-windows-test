"""Regression tests for the digest recorder's describe normalization (DPES-819, PR 210 review).

gcloud can print raw control characters inside JSON strings. jq rejects them, so a jq read of the raw
describe output stops the recorder before its scan gate. release_evidence accepts them (strict=False).
describe_normalize rewrites each response once so every later reader sees the same strict document.
"""

import json
import os
import shutil
import subprocess
import sys
from pathlib import Path

import pytest

from mao_cloud.describe_normalize import main, normalize_file

CLOUD = Path(__file__).resolve().parents[1]
DIGEST = "sha256:" + "3a" * 32
RAW_TAB = "unk\tnown"


def raw_describe(path: Path) -> Path:
    # One literal tab inside a JSON string, as gcloud prints it.
    path.write_text(
        '{"image_summary": {"digest": "' + DIGEST + '", "note": "' + RAW_TAB + '"}}',
        encoding="utf-8",
    )
    return path


def test_the_raw_output_is_refused_by_strict_json_and_accepted_by_the_permissive_reader(
    tmp_path: Path,
) -> None:
    text = raw_describe(tmp_path / "controller.json").read_text(encoding="utf-8")
    with pytest.raises(json.JSONDecodeError):
        json.loads(text)
    assert json.loads(text, strict=False)["image_summary"]["digest"] == DIGEST


def test_normalized_output_is_strict_json_and_keeps_the_digest_and_the_tab(tmp_path: Path) -> None:
    path = raw_describe(tmp_path / "controller.json")
    normalize_file(path)
    document = json.loads(path.read_text(encoding="utf-8"))
    assert document["image_summary"]["digest"] == DIGEST
    assert document["image_summary"]["note"] == RAW_TAB


def test_normalizing_twice_changes_nothing(tmp_path: Path) -> None:
    path = raw_describe(tmp_path / "controller.json")
    normalize_file(path)
    once = path.read_text(encoding="utf-8")
    normalize_file(path)
    assert path.read_text(encoding="utf-8") == once


@pytest.mark.skipif(shutil.which("jq") is None, reason="jq is not installed")
def test_jq_reads_the_digest_only_after_normalization(tmp_path: Path) -> None:
    path = raw_describe(tmp_path / "controller.json")
    jq = ["jq", "-r", ".image_summary.digest", str(path)]
    refused = subprocess.run(jq, capture_output=True, text=True, check=False)
    assert refused.returncode != 0, "jq accepted raw control characters; the premise of this change is gone"
    normalize_file(path)
    read = subprocess.run(jq, capture_output=True, text=True, check=True)
    assert read.stdout.strip() == DIGEST


def test_the_module_rewrites_the_file_it_is_given(tmp_path: Path) -> None:
    path = raw_describe(tmp_path / "controller.json")
    assert main([str(path)]) == 0
    assert json.loads(path.read_text(encoding="utf-8"))["image_summary"]["digest"] == DIGEST


def test_the_entry_point_runs_the_way_the_workflow_runs_it(tmp_path: Path) -> None:
    path = raw_describe(tmp_path / "controller.json")
    env = {**os.environ, "PYTHONPATH": str(CLOUD / "src")}
    result = subprocess.run(
        [sys.executable, "-m", "mao_cloud.describe_normalize", str(path)],
        capture_output=True,
        text=True,
        env=env,
        check=False,
    )
    assert result.returncode == 0, result.stderr
    assert json.loads(path.read_text(encoding="utf-8"))["image_summary"]["note"] == RAW_TAB


@pytest.mark.parametrize("argv", [[], ["a.json", "b.json"]])
def test_the_module_refuses_a_call_without_exactly_one_path(argv: list[str]) -> None:
    assert main(argv) == 2


def test_unparseable_output_still_fails_the_recorder(tmp_path: Path) -> None:
    path = tmp_path / "controller.json"
    path.write_text("ERROR: (gcloud.artifacts.docker.images.describe) reauthentication failed", encoding="utf-8")
    with pytest.raises(json.JSONDecodeError):
        normalize_file(path)
