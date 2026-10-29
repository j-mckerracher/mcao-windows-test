"""Candidate image read-back checks (DPES-819 AC2, AC5).

The payload shape follows a real `gcloud artifacts docker images describe --show-all-metadata
--format=json` read of two main-build digests: a finished scan with no vulnerability summary. The
shape of a summary that does list findings comes from the gcloud reference, not from a read, so
those cases prove the check refuses them and nothing about what the registry returned.
"""

import json
from pathlib import Path

import pytest

from mao_cloud.release_evidence import (
    EvidenceError,
    candidate_blockers,
    main,
    parse_image_describe,
)

CONTROLLER = "sha256:" + "3a" * 32
WORKER = "sha256:" + "4b" * 32
SIGNER = "sha256:" + "6d" * 32
REQUESTED = {"mao-controller": CONTROLLER, "mao-worker": WORKER, "mao-signer": SIGNER}


def describe(digest: str, *, status: str | None = "FINISHED_SUCCESS", findings: object = None) -> dict:
    payload: dict[str, object] = {
        "image_summary": {"digest": digest, "slsa_build_level": "unknown"},
        "package_summary": {"packages": []},
    }
    if status is not None:
        payload["discovery_summary"] = {
            "discovery": [
                {
                    "kind": "DISCOVERY",
                    "discovery": {
                        "analysisStatus": status,
                        "continuousAnalysis": "ACTIVE",
                        "lastScanTime": "2026-10-22T21:32:01.809949199Z",
                    },
                }
            ]
        }
    if findings is not None:
        payload["package_vulnerability_summary"] = {"vulnerabilities": findings}
    return payload


def trio(controller: dict, worker: dict, signer: dict):
    return (
        parse_image_describe("mao-controller", controller),
        parse_image_describe("mao-worker", worker),
        parse_image_describe("mao-signer", signer),
    )


def test_finished_scan_without_findings_leaves_no_blocker() -> None:
    images = trio(describe(CONTROLLER), describe(WORKER), describe(SIGNER))
    controller = images[0]
    assert controller.scan_status == "FINISHED_SUCCESS"
    assert controller.last_scan_time == "2026-10-22T21:32:01.809949199Z"
    assert (controller.critical, controller.high) == (0, 0)
    assert candidate_blockers(images, requested=REQUESTED) == ()


def test_a_scan_that_has_not_finished_blocks_the_candidate() -> None:
    images = trio(describe(CONTROLLER, status="SCANNING"), describe(WORKER), describe(SIGNER))
    assert candidate_blockers(images, requested=REQUESTED) == (
        "mao-controller: scan status is SCANNING, not FINISHED_SUCCESS",
    )


def test_a_read_with_no_discovery_record_is_not_a_finished_scan() -> None:
    images = trio(describe(CONTROLLER), describe(WORKER, status=None), describe(SIGNER))
    assert candidate_blockers(images, requested=REQUESTED) == (
        "mao-worker: scan status is None, not FINISHED_SUCCESS",
    )


def test_critical_and_high_findings_block_and_lower_severities_do_not() -> None:
    findings = {"CRITICAL": [{}], "HIGH": [{}, {}], "LOW": [{}, {}, {}]}
    images = trio(describe(CONTROLLER), describe(WORKER, findings=findings), describe(SIGNER))
    assert (images[1].critical, images[1].high) == (1, 2)
    assert candidate_blockers(images, requested=REQUESTED) == (
        "mao-worker: 1 Critical and 2 High findings",
    )
    only_low = describe(WORKER, findings={"LOW": [{}], "MEDIUM": [{}]})
    images = trio(describe(CONTROLLER), only_low, describe(SIGNER))
    assert candidate_blockers(images, requested=REQUESTED) == ()


def test_a_signer_with_a_high_finding_blocks_the_candidate() -> None:
    images = trio(describe(CONTROLLER), describe(WORKER), describe(SIGNER, findings={"HIGH": [{}]}))
    assert candidate_blockers(images, requested=REQUESTED) == ("mao-signer: 0 Critical and 1 High findings",)


def test_a_digest_other_than_the_requested_one_blocks() -> None:
    other = "sha256:" + "5c" * 32
    images = trio(describe(other), describe(WORKER), describe(SIGNER))
    assert candidate_blockers(images, requested=REQUESTED) == (
        f"mao-controller: read digest {other} is not the requested {CONTROLLER}",
    )


def test_one_digest_for_two_images_blocks() -> None:
    images = trio(describe(CONTROLLER), describe(WORKER), describe(CONTROLLER))
    reasons = candidate_blockers(
        images, requested={"mao-controller": CONTROLLER, "mao-worker": WORKER, "mao-signer": CONTROLLER}
    )
    assert reasons == ("mao-controller and mao-signer are the same digest; they are separate images",)


@pytest.mark.parametrize(
    "payload",
    [
        [],
        {},
        {"image_summary": {"digest": "latest"}},
        {"image_summary": {"digest": CONTROLLER}, "discovery_summary": {"discovery": {}}},
        {"image_summary": {"digest": CONTROLLER}, "package_vulnerability_summary": {"vulnerabilities": []}},
        {"image_summary": {"digest": CONTROLLER}, "package_vulnerability_summary": {"vulnerabilities": {"HIGH": 2}}},
        {"image_summary": {"digest": CONTROLLER}, "package_vulnerability_summary": {}},
        {"image_summary": {"digest": CONTROLLER}, "package_vulnerability_summary": {"counts": {"CRITICAL": 1}}},
        {"image_summary": {"digest": CONTROLLER}, "package_vulnerability_summary": {"unexpected_findings": [{}]}},
        {"image_summary": {"digest": CONTROLLER}, "package_vulnerability_summary": []},
        {"image_summary": {"digest": CONTROLLER}, "package_vulnerability_summary": {"vulnerabilities": {"SEVERE": [{}]}}},
        {"image_summary": {"digest": CONTROLLER}, "package_vulnerability_summary": {"vulnerabilities": {"SEVERITY_UNSPECIFIED": [{}]}}},
    ],
)
def test_a_malformed_read_is_refused_not_read_as_clean(payload: object) -> None:
    with pytest.raises(EvidenceError):
        parse_image_describe("mao-controller", payload)


def test_an_empty_list_under_an_unknown_severity_is_not_a_finding() -> None:
    evidence = parse_image_describe("mao-worker", describe(WORKER, findings={"SEVERE": []}))
    assert (evidence.critical, evidence.high) == (0, 0)


def test_lowercase_gated_severity_labels_are_still_counted() -> None:
    evidence = parse_image_describe("mao-worker", describe(WORKER, findings={"high": [{}], "Critical": [{}]}))
    assert (evidence.critical, evidence.high) == (1, 1)


def write(path: Path, payload: dict) -> Path:
    path.write_text(json.dumps(payload), encoding="utf-8")
    return path


def arguments(
    tmp_path: Path,
    controller: dict,
    worker: dict,
    signer: dict,
    *,
    digests: tuple[str, str, str] = (CONTROLLER, WORKER, SIGNER),
) -> list[str]:
    return [
        "--controller-describe", str(write(tmp_path / "controller.json", controller)),
        "--controller-digest", digests[0],
        "--worker-describe", str(write(tmp_path / "worker.json", worker)),
        "--worker-digest", digests[1],
        "--signer-describe", str(write(tmp_path / "signer.json", signer)),
        "--signer-digest", digests[2],
    ]


def test_cli_exits_zero_and_says_the_attestation_was_not_read(
    tmp_path: Path, capsys: pytest.CaptureFixture[str]
) -> None:
    code = main(arguments(tmp_path, describe(CONTROLLER), describe(WORKER), describe(SIGNER)))
    record = json.loads(capsys.readouterr().out)
    assert code == 0
    assert record["blockers"] == []
    assert record["attestation"] == "not read by this check"
    assert record["worker"]["digest"] == WORKER
    assert record["signer"]["digest"] == SIGNER


def test_cli_exits_nonzero_when_the_candidate_is_blocked(
    tmp_path: Path, capsys: pytest.CaptureFixture[str]
) -> None:
    code = main(arguments(tmp_path, describe(CONTROLLER, status="SCANNING"), describe(WORKER), describe(SIGNER)))
    record = json.loads(capsys.readouterr().out)
    assert code == 1
    assert record["blockers"] == ["mao-controller: scan status is SCANNING, not FINISHED_SUCCESS"]


def test_cli_exits_nonzero_when_the_signer_is_blocked(
    tmp_path: Path, capsys: pytest.CaptureFixture[str]
) -> None:
    code = main(arguments(tmp_path, describe(CONTROLLER), describe(WORKER), describe(SIGNER, status="FINISHED_UNSUPPORTED")))
    record = json.loads(capsys.readouterr().out)
    assert code == 1
    assert record["blockers"] == ["mao-signer: scan status is FINISHED_UNSUPPORTED, not FINISHED_SUCCESS"]


def test_cli_reads_gcloud_output_that_holds_raw_control_characters(
    tmp_path: Path, capsys: pytest.CaptureFixture[str]
) -> None:
    controller = write(tmp_path / "controller.json", describe(CONTROLLER))
    raw = controller.read_text(encoding="utf-8").replace('"unknown"', '"unk\tnown"')
    controller.write_text(raw, encoding="utf-8")
    code = main(
        [
            "--controller-describe", str(controller),
            "--controller-digest", CONTROLLER,
            "--worker-describe", str(write(tmp_path / "worker.json", describe(WORKER))),
            "--worker-digest", WORKER,
            "--signer-describe", str(write(tmp_path / "signer.json", describe(SIGNER))),
            "--signer-digest", SIGNER,
        ]
    )
    assert code == 0
    assert json.loads(capsys.readouterr().out)["blockers"] == []


@pytest.mark.parametrize(
    "summary",
    [{"counts": {"CRITICAL": 1}}, {"unexpected_findings": [{}]}, {}],
)
def test_cli_refuses_a_finished_scan_whose_summary_has_no_recognized_findings_mapping(
    tmp_path: Path, summary: dict
) -> None:
    controller = describe(CONTROLLER)
    controller["package_vulnerability_summary"] = summary
    with pytest.raises(EvidenceError):
        main(arguments(tmp_path, controller, describe(WORKER), describe(SIGNER)))


def test_cli_still_passes_a_finished_scan_with_no_summary_at_all(
    tmp_path: Path, capsys: pytest.CaptureFixture[str]
) -> None:
    controller = describe(CONTROLLER)
    assert "package_vulnerability_summary" not in controller
    assert main(arguments(tmp_path, controller, describe(WORKER), describe(SIGNER))) == 0
    assert json.loads(capsys.readouterr().out)["blockers"] == []
