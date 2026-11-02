"""Doctor: a local, no-network preflight that reports whether a branch can be submitted.

Doctor runs the same export submit runs, so a passing doctor and a submittable snapshot cannot
disagree. A refusal is a failed report; an unreadable repository is raised to the caller.
"""

from dataclasses import dataclass
from pathlib import Path

from mao_cloud.source import SourceRefused, export_snapshot
from mao_cloud.trusted_bundle import BundleLimits


@dataclass(frozen=True)
class DoctorReport:
    ok: bool
    lines: tuple[str, ...]


def run_doctor(repo: Path, ref: str, *, limits: BundleLimits = BundleLimits()) -> DoctorReport:
    try:
        snapshot = export_snapshot(repo, ref, limits=limits)
    except SourceRefused as exc:
        return DoctorReport(ok=False, lines=(f"fail: {exc}",))
    return DoctorReport(
        ok=True,
        lines=(
            f"ok: refs/heads/{ref} exports at {snapshot.base_sha}",
            f"ok: source archive is {snapshot.archive_size} bytes, within the "
            f"{limits.max_bundle_bytes} byte limit",
        ),
    )
