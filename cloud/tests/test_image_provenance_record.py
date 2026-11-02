"""Guard for the DPES-819 image provenance read-back record (cloud/docs/image-provenance.md).

The record holds values read from two workflow runs. This test keeps those values, the run
identities and the stated gaps from drifting. It cannot prove a registry digest, a scan result or
an attestation at runtime; those stay named gaps in the record.
"""

from pathlib import Path

CLOUD = Path(__file__).resolve().parents[1]
RECORD = CLOUD / "docs" / "image-provenance.md"

PRE_MERGE_RUN = "39828880739"
PRE_MERGE_HEAD = "0c369553a2654c5161463d1fac1e0e946dd90190"
MAIN_RUN = "39852080399"
MAIN_HEAD = "fe230a3270122b4ac899d1b0c6e2289e8b974a29"

DIGESTS = {
    "mao-controller": {
        PRE_MERGE_RUN: "sha256:ac35ee8a8e6800da80f457283c5adf32611f166b86d59dafefcf9c21b5ffc89f",
        MAIN_RUN: "sha256:c3d735e948b6ab22559ba4a2cfcc3f7ec2e5eb62c40a5767c67dbb1e094451e6",
    },
    "mao-worker": {
        PRE_MERGE_RUN: "sha256:9e4b20b14021cbaf1421a7fb79ca4f44b8cfb0b0ad993de3a8423e03404f90e6",
        MAIN_RUN: "sha256:09c20f10d933b38f1b050f0b201a5c99eb80042911142ed87c7aeb184fba363e",
    },
}

CANDIDATE_RUN = "39857722683"
CANDIDATE_HEAD = "ddce90acb68f37c7e4c42c8c0b73ed8d4d1414ab"
CANDIDATE_DIGESTS = (
    "sha256:3658015d8ce36ba62ce70d29f81b8d550e13ffff092d91740b9bf5e63433f077",
    "sha256:3cf4c489faea0fa3137c4765add639456626467b779f377d9d80f08b79ebcbfe",
)

GAP_TICKETS = ("DPES-804", "DPES-807", "DPES-819", "DPES-871")


def _record() -> str:
    return RECORD.read_text(encoding="utf-8")


def test_provenance_record_exists_under_cloud_docs() -> None:
    assert RECORD.is_file()


def test_every_pushed_digest_is_recorded_for_both_runs() -> None:
    text = _record()
    for image, by_run in DIGESTS.items():
        assert image in text
        for digest in by_run.values():
            assert digest in text, f"{image} digest {digest} is missing from the record"


def test_both_runs_and_their_head_commits_are_identified() -> None:
    text = _record()
    for value in (PRE_MERGE_RUN, MAIN_RUN, PRE_MERGE_HEAD, MAIN_HEAD):
        assert value in text


def test_missing_mc_vuln_attestor_is_stated_as_not_proven() -> None:
    text = _record()
    assert "**Not proven: `mc-vuln-attestor`.**" in text


def test_every_stated_gap_names_its_blocking_ticket() -> None:
    text = _record()
    for ticket in GAP_TICKETS:
        assert ticket in text, f"gap ticket {ticket} is not named in the record"


def test_current_candidate_names_its_run_commit_digests_and_scan_state() -> None:
    text = _record()
    section = text[text.index("## Current candidate") : text.index("## CAF edit still needed")]
    for value in (CANDIDATE_RUN, CANDIDATE_HEAD, *CANDIDATE_DIGESTS):
        assert value in section
    assert section.count("`FINISHED_SUCCESS`") == 2


def test_operator_example_passes_all_three_images_to_the_release_check() -> None:
    # release_evidence requires a describe file and a digest for each of the three images; an example
    # that names fewer exits with an argparse error before any scan is read.
    text = _record()
    command = next(line for line in text.splitlines() if "mao_cloud.release_evidence" in line and "uv run" in line)
    for role in ("controller", "worker", "signer"):
        assert f"--{role}-describe {role}.json" in command, role
        assert f"--{role}-digest <{role}-digest>" in command, role


def test_record_does_not_claim_a_caf_pipeline_change_is_needed_for_the_attestation() -> None:
    text = " ".join(_record().split())
    assert "no pipeline step is needed to obtain one" in text
    assert "a CAF pipeline change is still needed" not in text
