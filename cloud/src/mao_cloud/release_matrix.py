"""Rollout and rollback compatibility matrix for a cloud release (DPES-819 AC5, AC6, AC7).

Pure decisions over recorded release identities. Nothing here calls GCP, pulls an image, or
touches a run. The supported identities are data (cloud/release/compatibility-matrix.json); the
plans say what a rollout or rollback may do and what happens to each queued run and checkpoint.

A plan is refused as a whole when any identity, artifact, or profile is unsupported. A refused
plan has no steps and dispatches or resumes nothing. Every run it touches is preserved as written.
Proof that a real rollback leaves Cloud Run, Jobs, and the Scheduler intact needs GCP and is a
stated gap (DPES-804, 807, 819, 871).
"""

import re
from collections.abc import Collection, Mapping
from dataclasses import dataclass, fields
from enum import Enum

from mao_cloud.continuation import KillSwitchClass
from mao_cloud.protocol import PROTOCOL_VERSION
from mao_cloud.trusted_profiles import STANDARD

MATRIX_SCHEMA_VERSION = 1
PILOT_PROFILE = STANDARD.name
TRUSTED_ENTRYPOINT = "mao_cloud.worker_entry"
KILL_SWITCH_CLASSES = frozenset(KillSwitchClass)
ACTIVE_CHECKPOINT_RULE = (
    "An active checkpoint resumes only under the exact release identity that wrote it: the same "
    "checkpoint schema, worker image digest, and harness version. Any other checkpoint is "
    "preserved, not reinterpreted, and not resumed."
)

_DIGEST = re.compile(r"^sha256:[0-9a-f]{64}$")
_DIGEST_FIELDS = ("controller_image_digest", "worker_image_digest", "tool_registry_digest")


class MatrixError(ValueError):
    """The compatibility matrix or a release entry is malformed. It is refused, never partly used."""


@dataclass(frozen=True)
class Release:
    """One release identity: the exact images, registry, profile, entrypoint, and harness."""

    controller_image_digest: str
    worker_image_digest: str
    worker_profile_id: str
    worker_entrypoint: str
    protocol_version: str
    checkpoint_schema_version: int
    tool_registry_digest: str
    harness_version: str

    def __post_init__(self) -> None:
        for name in _DIGEST_FIELDS:
            value = getattr(self, name)
            if not isinstance(value, str) or not _DIGEST.fullmatch(value):
                raise MatrixError(f"{name} is not sha256:<64 lowercase hex>: {value!r}")
        for name in ("worker_profile_id", "worker_entrypoint", "harness_version"):
            value = getattr(self, name)
            if not isinstance(value, str) or not value:
                raise MatrixError(f"{name} must be a non-empty string")
        if self.protocol_version != PROTOCOL_VERSION:
            raise MatrixError(f"protocol_version {self.protocol_version!r} is not {PROTOCOL_VERSION!r}")
        schema = self.checkpoint_schema_version
        if isinstance(schema, bool) or not isinstance(schema, int) or schema < 1:
            raise MatrixError(f"checkpoint_schema_version must be an integer >= 1: {schema!r}")

    def to_payload(self) -> dict[str, object]:
        return {field.name: getattr(self, field.name) for field in fields(self)}

    @classmethod
    def from_payload(cls, payload: object) -> "Release":
        if not isinstance(payload, Mapping):
            raise MatrixError("a supported release entry must be an object")
        expected = {field.name for field in fields(cls)}
        if set(payload) != expected:
            raise MatrixError(f"release keys must be exactly {sorted(expected)}, got {sorted(payload)}")
        return cls(**payload)


@dataclass(frozen=True)
class Matrix:
    supported: tuple[Release, ...]


def load_matrix(payload: Mapping[str, object]) -> Matrix:
    if set(payload) != {"schema_version", "supported"}:
        raise MatrixError("matrix keys must be exactly schema_version and supported")
    if payload["schema_version"] != MATRIX_SCHEMA_VERSION:
        raise MatrixError(f"unknown matrix schema_version {payload['schema_version']!r}")
    entries = payload["supported"]
    if not isinstance(entries, list):
        raise MatrixError("supported must be a list")
    releases = tuple(Release.from_payload(entry) for entry in entries)
    if len(set(releases)) != len(releases):
        raise MatrixError("the supported list repeats a release identity")
    return Matrix(supported=releases)


class Drain(Enum):
    """What happens to in-flight runs during a rollback. The choice is explicit, never defaulted."""

    DRAIN = "drain"
    CANCEL = "cancel"


@dataclass(frozen=True)
class QueuedRun:
    run_id: str
    worker_image_digest: str


@dataclass(frozen=True)
class ActiveCheckpoint:
    run_id: str
    schema_version: int
    worker_image_digest: str
    harness_version: str


@dataclass(frozen=True)
class Preserved:
    run_id: str
    reason: str


@dataclass(frozen=True)
class Plan:
    steps: tuple[str, ...] = ()
    refused: tuple[str, ...] = ()
    dispatch: tuple[str, ...] = ()
    resume: tuple[str, ...] = ()
    preserved: tuple[Preserved, ...] = ()


def rollout_order() -> tuple[str, ...]:
    return (
        "deploy identities and state",
        "deploy controller and worker images by digest",
        "run service, Jobs, and auth smoke checks",
        "enable Scheduler and monitoring",
        "enable the pilot flag",
    )


def plan_rollout(
    *,
    source: Release,
    target: Release,
    matrix: Matrix,
    retained: Collection[str],
    queued: tuple[QueuedRun, ...],
    checkpoints: tuple[ActiveCheckpoint, ...],
) -> Plan:
    return _plan(
        source=source,
        target=target,
        matrix=matrix,
        retained=retained,
        queued=queued,
        checkpoints=checkpoints,
        steps=rollout_order(),
    )


def plan_rollback(
    *,
    source: Release,
    target: Release,
    matrix: Matrix,
    retained: Collection[str],
    queued: tuple[QueuedRun, ...],
    checkpoints: tuple[ActiveCheckpoint, ...],
    drain: Drain,
    admissions_disabled: bool = False,
) -> Plan:
    """Plan only from a snapshot captured after admissions were disabled and writes drained.

    admissions_disabled confirms the operator has closed the durable DPES-818 admissions
    switch on every broker and waited for in-flight admitting requests before this snapshot.
    The planner does not change that switch; without confirmation it authorizes nothing.
    """
    steps = (
        "disable new admissions, drain admitting requests, and capture the affected run snapshot",
        f"engage kill switch: {', '.join(sorted(KILL_SWITCH_CLASSES))}",
        f"in-flight runs: {drain.value}",
        f"restore controller image: {target.controller_image_digest}",
        f"restore worker image: {target.worker_image_digest}",
        f"restore tool registry: {target.tool_registry_digest}",
        "confirm Cloud Run, Jobs, and Scheduler resources are intact; no resource is removed",
        "release kill switch after dispatch and resume decisions are recorded",
        "re-enable admissions only after the target release and recorded run decisions are ready",
    )
    return _plan(
        source=source,
        target=target,
        matrix=matrix,
        retained=retained,
        queued=queued,
        checkpoints=checkpoints,
        steps=steps,
        admissions_disabled=admissions_disabled,
        cancel_checkpoints=drain is Drain.CANCEL,
    )


def _plan(
    *,
    source: Release,
    target: Release,
    matrix: Matrix,
    retained: Collection[str],
    queued: tuple[QueuedRun, ...],
    checkpoints: tuple[ActiveCheckpoint, ...],
    steps: tuple[str, ...],
    admissions_disabled: bool = True,
    cancel_checkpoints: bool = False,
) -> Plan:
    refusals = _refusals(source=source, target=target, matrix=matrix, retained=retained)
    if admissions_disabled is not True:
        refusals += (
            "disable new admissions and drain admitting requests before capturing the run snapshot",
        )
    if refusals:
        held = [Preserved(run.run_id, "plan refused; left as admitted") for run in queued]
        held += [Preserved(ckpt.run_id, "plan refused; left as written") for ckpt in checkpoints]
        return Plan(refused=refusals, preserved=tuple(held))

    dispatch: list[str] = []
    resume: list[str] = []
    preserved: list[Preserved] = []
    for run in queued:
        if run.worker_image_digest == target.worker_image_digest:
            dispatch.append(run.run_id)
        else:
            preserved.append(
                Preserved(
                    run.run_id,
                    f"admitted under worker {run.worker_image_digest}, not the target worker; "
                    "not reinterpreted",
                )
            )
    for ckpt in checkpoints:
        if cancel_checkpoints:
            preserved.append(Preserved(ckpt.run_id, "selected for cancellation; not authorized to resume"))
            continue
        mismatch = _checkpoint_mismatch(ckpt, target)
        if mismatch is None:
            resume.append(ckpt.run_id)
        else:
            preserved.append(Preserved(ckpt.run_id, mismatch))
    return Plan(
        steps=steps,
        dispatch=tuple(dispatch),
        resume=tuple(resume),
        preserved=tuple(preserved),
    )


def _refusals(
    *, source: Release, target: Release, matrix: Matrix, retained: Collection[str]
) -> tuple[str, ...]:
    reasons: list[str] = []
    if source not in matrix.supported:
        reasons.append(
            "source release is not a recorded supported identity; the plan needs a known start"
        )
    if target.worker_profile_id != PILOT_PROFILE:
        reasons.append(
            f"worker profile {target.worker_profile_id} is not the pilot profile {PILOT_PROFILE}"
        )
    if target.worker_entrypoint != TRUSTED_ENTRYPOINT:
        reasons.append(
            f"worker entrypoint {target.worker_entrypoint} is not the trusted worker entry; "
            "a plain-pi fallback is refused"
        )
    if target not in matrix.supported:
        reasons.append("target release is not a supported identity in the compatibility matrix")
    for name in _DIGEST_FIELDS:
        digest = getattr(target, name)
        if digest not in retained:
            reasons.append(f"target artifact not retained: {digest}")
    return tuple(reasons)


def _checkpoint_mismatch(ckpt: ActiveCheckpoint, target: Release) -> str | None:
    if ckpt.schema_version != target.checkpoint_schema_version:
        return (
            f"checkpoint schema {ckpt.schema_version} does not match target schema "
            f"{target.checkpoint_schema_version}; not reinterpreted"
        )
    if ckpt.worker_image_digest != target.worker_image_digest or ckpt.harness_version != target.harness_version:
        return "checkpoint was written by a different worker image or harness; not reinterpreted"
    return None
