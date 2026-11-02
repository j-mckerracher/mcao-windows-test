"""Read-back checks for a candidate controller, worker and signer image set (DPES-819 AC2, AC5).

Pure decisions over the JSON that `gcloud artifacts docker images describe --show-all-metadata
--format=json` prints. Nothing here calls GCP. The digest-recording workflow and the operator both run
the read and pass the files in.

A candidate is blocked, never assumed good, when the scan did not finish, when the registry shows a
Critical or High finding, when it lists findings under a severity this check does not know, when two
images share a digest, or when the digest read is not the digest that was asked for. An
`mc-vuln-attestor` attestation is stored in the attestor project, so this read cannot see it; a
candidate that passes here is still unattested as far as this check knows.
"""

import argparse
import json
import re
import sys
from collections.abc import Mapping, Sequence
from dataclasses import dataclass
from itertools import combinations
from pathlib import Path

SCAN_FINISHED = "FINISHED_SUCCESS"
IMAGE_ROLES = (("controller", "mao-controller"), ("worker", "mao-worker"), ("signer", "mao-signer"))
_DIGEST = re.compile(r"^sha256:[0-9a-f]{64}$")
_GATED_SEVERITIES = ("CRITICAL", "HIGH")
_KNOWN_SEVERITIES = frozenset((*_GATED_SEVERITIES, "MEDIUM", "LOW", "MINIMAL"))


class EvidenceError(ValueError):
    """The registry read is malformed. It is refused, never partly used."""


@dataclass(frozen=True)
class ImageEvidence:
    name: str
    digest: str
    scan_status: str | None
    continuous_analysis: str | None
    last_scan_time: str | None
    critical: int
    high: int


def _mapping(value: object, what: str) -> Mapping[str, object]:
    if not isinstance(value, Mapping):
        raise EvidenceError(f"{what} must be an object")
    return value


def _optional_text(value: object, what: str) -> str | None:
    if value is None:
        return None
    if not isinstance(value, str):
        raise EvidenceError(f"{what} must be a string")
    return value


def _severity_counts(summary: object) -> dict[str, int]:
    counts = {severity: 0 for severity in _GATED_SEVERITIES}
    if summary is None:
        return counts
    document = _mapping(summary, "package_vulnerability_summary")
    if "vulnerabilities" not in document:
        raise EvidenceError("package_vulnerability_summary has no vulnerabilities mapping, so it cannot be read as clean")
    findings = _mapping(document["vulnerabilities"], "vulnerabilities")
    for severity, entries in findings.items():
        if not isinstance(entries, list):
            raise EvidenceError(f"vulnerabilities.{severity} must be a list")
        key = severity.upper()
        if key not in _KNOWN_SEVERITIES:
            if entries:
                raise EvidenceError(f"vulnerabilities.{severity} is not a known severity, so it cannot be read as clean")
            continue
        if key in counts:
            counts[key] += len(entries)
    return counts


def parse_image_describe(name: str, payload: object) -> ImageEvidence:
    """Reduce one describe read to the digest, the scan state, and the gated finding counts."""
    document = _mapping(payload, "describe output")
    digest = _mapping(document.get("image_summary"), "image_summary").get("digest")
    if not isinstance(digest, str) or not _DIGEST.fullmatch(digest):
        raise EvidenceError(f"{name}: image_summary.digest is not sha256:<64 lowercase hex>: {digest!r}")
    summary = document.get("discovery_summary")
    entries = _mapping(summary, "discovery_summary").get("discovery", []) if summary is not None else []
    if not isinstance(entries, list):
        raise EvidenceError(f"{name}: discovery_summary.discovery must be a list")
    status = continuous = last_scan = None
    for entry in entries:
        discovery = _mapping(_mapping(entry, "discovery entry").get("discovery"), "discovery")
        status = _optional_text(discovery.get("analysisStatus"), "analysisStatus")
        continuous = _optional_text(discovery.get("continuousAnalysis"), "continuousAnalysis")
        last_scan = _optional_text(discovery.get("lastScanTime"), "lastScanTime")
    counts = _severity_counts(document.get("package_vulnerability_summary"))
    return ImageEvidence(
        name=name,
        digest=digest,
        scan_status=status,
        continuous_analysis=continuous,
        last_scan_time=last_scan,
        critical=counts["CRITICAL"],
        high=counts["HIGH"],
    )


def candidate_blockers(
    images: Sequence[ImageEvidence],
    *,
    requested: Mapping[str, str],
) -> tuple[str, ...]:
    """Reasons the set cannot be pinned. An empty result means this read found none."""
    reasons: list[str] = []
    for image in images:
        asked = requested.get(image.name)
        if asked != image.digest:
            reasons.append(f"{image.name}: read digest {image.digest} is not the requested {asked}")
        if image.scan_status != SCAN_FINISHED:
            reasons.append(f"{image.name}: scan status is {image.scan_status}, not {SCAN_FINISHED}")
        if image.critical or image.high:
            reasons.append(f"{image.name}: {image.critical} Critical and {image.high} High findings")
    for first, second in combinations(images, 2):
        if first.digest == second.digest:
            reasons.append(f"{first.name} and {second.name} are the same digest; they are separate images")
    return tuple(reasons)


def _load(path: Path) -> object:
    # gcloud prints raw control characters inside some strings, so strict parsing refuses real reads.
    return json.loads(path.read_text(encoding="utf-8"), strict=False)


def main(argv: Sequence[str] | None = None) -> int:
    parser = argparse.ArgumentParser(description="Check a candidate controller, worker and signer digest set.")
    for role, _ in IMAGE_ROLES:
        parser.add_argument(f"--{role}-describe", type=Path, required=True)
        parser.add_argument(f"--{role}-digest", required=True)
    args = parser.parse_args(argv)
    images = [
        parse_image_describe(name, _load(getattr(args, f"{role}_describe")))
        for role, name in IMAGE_ROLES
    ]
    requested = {name: getattr(args, f"{role}_digest") for role, name in IMAGE_ROLES}
    reasons = candidate_blockers(images, requested=requested)
    record: dict[str, object] = {role: image.__dict__ for (role, _), image in zip(IMAGE_ROLES, images)}
    record["blockers"] = list(reasons)
    record["attestation"] = "not read by this check"
    print(json.dumps(record, indent=2))
    return 1 if reasons else 0


if __name__ == "__main__":
    sys.exit(main())
