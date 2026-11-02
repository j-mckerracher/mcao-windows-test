"""Server-selected policy: target, harness, model, thinking, toolchain, and caller allowlists.

Policy is data loaded at startup from an operator-controlled file. Clients may only name
entries present here. Images, service accounts, and secrets never come from a client, so a
manifest that tries to supply them is refused by the protocol screen before this module runs.

The policy also declares who may call the control API: the audience their tokens must carry,
and one fixed identity per caller (a numeric Google subject, its email, its role, and for an
owner its owner_id). Identity is never read from a client header or a request body.
"""

import json
import re
from collections.abc import Iterable, Mapping
from dataclasses import dataclass
from enum import StrEnum
from pathlib import Path

from pydantic import BaseModel, ConfigDict, Field, ValidationError

from mao_cloud.protocol import (
    HARNESS_VALUES,
    MAX_ADMITTED_SOURCE_BYTES,
    MAX_ATTEMPTS,
    MAX_RUNTIME_SECONDS,
    MODEL_VALUES,
    STARTUP_TIMEOUT_SECONDS,
    THINKING_VALUES,
    CreateRunManifest,
    ErrorCode,
    ProtocolError,
    ToolCompatibility,
)

_TARGET_PATTERN = re.compile(r"^[a-z][a-z0-9-]{2,62}$")
_PROFILE_PATTERN = re.compile(r"^[a-z][a-z0-9-]{1,62}$")
_SERVER_ID_PATTERN = re.compile(r"^[A-Za-z0-9][A-Za-z0-9._:-]{0,63}$")
_SUBJECT_PATTERN = re.compile(r"^[0-9]{1,30}$")
_EMAIL_PATTERN = re.compile(r"^[a-z0-9][a-z0-9._+-]{0,63}@[a-z0-9-]+(\.[a-z0-9-]+)+$")
_OWNER_ID_PATTERN = re.compile(r"^[a-z][a-z0-9-]{1,62}$")
API_AUDIENCE_PATTERN = re.compile(r"^https://[a-z0-9]([a-z0-9.-]{0,251}[a-z0-9])?(:[0-9]{1,5})?$")


class PolicyError(ValueError):
    """The policy file is malformed or violates a server invariant. Startup refuses."""


class AccessMode(StrEnum):
    """Which callers the control API admits beyond the fixed identity map.

    allowlist admits only the mapped identities. mayo-wide also admits a caller whose verified email
    is exactly on the Mayo domain, as an owner keyed by its own numeric subject. The mode is read
    from the policy file; an absent field means allowlist.
    """

    MAYO_WIDE = "mayo-wide"
    ALLOWLIST = "allowlist"


MAYO_EMAIL_DOMAIN = "mayo.edu"
_MAYO_EMAIL_PATTERN = re.compile(r"[a-z0-9][a-z0-9._+-]{0,63}@" + re.escape(MAYO_EMAIL_DOMAIN))


def is_mayo_email(email: str) -> bool:
    """True only when the whole address is <local>@mayo.edu.

    Exact by design: no case folding, no trailing dot, no subdomain, and no suffix match, so a
    lookalike domain such as mayo.edu.example or evilmayo.edu is refused.
    """
    return _MAYO_EMAIL_PATTERN.fullmatch(email) is not None


class Role(StrEnum):
    """What a caller may do. Only an owner reaches owner routes, and only for its own runs.

    An operator and a scheduler reach no owner route. Only an operator or a scheduler may call
    internal reconciliation.
    """

    OWNER = "owner"
    OPERATOR = "operator"
    SCHEDULER = "scheduler"


@dataclass(frozen=True)
class Identity:
    """One caller the server admits: a fixed numeric subject, the email it must carry, and a role.

    The owner_id is the protocol owner that an owner identity acts as. It is set here, by
    provisioning, and never derived from a token claim the caller can influence.
    """

    subject: str
    email: str
    role: Role
    owner_id: str | None = None

    def __post_init__(self) -> None:
        if not _SUBJECT_PATTERN.fullmatch(self.subject):
            raise PolicyError("identity subject must be the numeric Google account id")
        if not _EMAIL_PATTERN.fullmatch(self.email):
            raise PolicyError("identity email must be a lowercase email address")
        if self.role is Role.OWNER:
            if self.owner_id is None or not _OWNER_ID_PATTERN.fullmatch(self.owner_id):
                raise PolicyError("an owner identity needs an owner_id in the protocol pattern")
        elif self.owner_id is not None:
            raise PolicyError("only an owner identity may carry an owner_id")


@dataclass(frozen=True)
class AdmissionLimits:
    """Queue, worker, daily-cost, and deadline limits the broker applies to admissions.

    The version identifies this limit set in the policy document so an operator can see which
    limits are live. Every limit must be positive; a policy without them refuses to load.
    """

    version: int
    team_queue_depth: int
    owner_queue_depth: int
    team_worker_slots: int
    owner_worker_slots: int
    team_daily_worker_seconds: int
    queued_deadline_seconds: int

    def __post_init__(self) -> None:
        for name in (
            "version",
            "team_queue_depth",
            "owner_queue_depth",
            "team_worker_slots",
            "owner_worker_slots",
            "team_daily_worker_seconds",
            "queued_deadline_seconds",
        ):
            value = getattr(self, name)
            if isinstance(value, bool) or not isinstance(value, int) or value < 1:
                raise PolicyError(f"admission {name} must be a positive integer")


@dataclass(frozen=True)
class RetentionDurations:
    """Approved age, in whole days, after which each durable object class may be cleaned up.

    Values are policy data subject to G01 approval. A policy without them has no approved
    durations, so nothing is due for cleanup.
    """

    inputs_days: int
    results_checkpoints_days: int
    claims_status_state_days: int
    logs_days: int

    def __post_init__(self) -> None:
        for name in (
            "inputs_days",
            "results_checkpoints_days",
            "claims_status_state_days",
            "logs_days",
        ):
            if getattr(self, name) < 1:
                raise PolicyError(f"{name} must be positive")


@dataclass(frozen=True)
class Policy:
    server_id: str
    targets: frozenset[str]
    harnesses: frozenset[str]
    models: frozenset[str]
    thinking: frozenset[str]
    profiles: Mapping[str, ToolCompatibility]
    stale_after_seconds: int
    skew_seconds: int
    admission: AdmissionLimits
    startup_timeout_seconds: int = STARTUP_TIMEOUT_SECONDS
    max_attempts: int = MAX_ATTEMPTS
    max_source_bytes: int = MAX_ADMITTED_SOURCE_BYTES
    max_runtime_seconds: int = MAX_RUNTIME_SECONDS
    api_audience: str | None = None
    identities: tuple[Identity, ...] = ()
    retention: RetentionDurations | None = None
    access_mode: AccessMode = AccessMode.ALLOWLIST

    def __post_init__(self) -> None:
        if not _SERVER_ID_PATTERN.fullmatch(self.server_id):
            raise PolicyError("server_id does not match the protocol pattern")
        _require_subset("harnesses", self.harnesses, HARNESS_VALUES)
        _require_subset("models", self.models, MODEL_VALUES)
        _require_subset("thinking", self.thinking, THINKING_VALUES)
        for name, values in (("targets", self.targets), ("harnesses", self.harnesses),
                             ("models", self.models), ("thinking", self.thinking),
                             ("profiles", frozenset(self.profiles))):
            if not values:
                raise PolicyError(f"{name} allowlist must not be empty")
        for target in self.targets:
            if not _TARGET_PATTERN.fullmatch(target):
                raise PolicyError(f"target id {target!r} does not match the protocol pattern")
        for profile_id, compatibility in self.profiles.items():
            if not _PROFILE_PATTERN.fullmatch(profile_id):
                raise PolicyError(f"profile id {profile_id!r} does not match the protocol pattern")
            if compatibility.profile_id != profile_id:
                raise PolicyError(f"profile {profile_id!r} pins a different profile_id")
        if self.stale_after_seconds < 1:
            raise PolicyError("stale_after_seconds must be positive")
        if self.skew_seconds < 0:
            raise PolicyError("skew_seconds must not be negative")
        if self.startup_timeout_seconds < 1:
            raise PolicyError("startup_timeout_seconds must be positive")
        if not 1 <= self.max_attempts <= MAX_ATTEMPTS:
            raise PolicyError(f"max_attempts must be between 1 and {MAX_ATTEMPTS}")
        if not 1 <= self.max_source_bytes <= MAX_ADMITTED_SOURCE_BYTES:
            raise PolicyError(
                f"max_source_bytes must be between 1 and {MAX_ADMITTED_SOURCE_BYTES}"
            )
        if not 1 <= self.max_runtime_seconds <= MAX_RUNTIME_SECONDS:
            raise PolicyError(f"max_runtime_seconds must be between 1 and {MAX_RUNTIME_SECONDS}")
        if self.api_audience is not None and not API_AUDIENCE_PATTERN.fullmatch(self.api_audience):
            raise PolicyError("api_audience must be the bare https origin of the control API")
        if self.identities and self.api_audience is None:
            raise PolicyError("identities require api_audience, the audience tokens must carry")
        if self.access_mode is AccessMode.MAYO_WIDE and self.api_audience is None:
            raise PolicyError("mayo-wide access requires api_audience, the audience tokens must carry")
        _require_unique("identity subject", (identity.subject for identity in self.identities))
        _require_unique("identity email", (identity.email for identity in self.identities))

    def admit_manifest(self, manifest: CreateRunManifest) -> ToolCompatibility:
        """Gate a manifest against the server allowlists and return its pinned tuple."""
        if manifest.target.id not in self.targets:
            raise ProtocolError(ErrorCode.NOT_ALLOWED, "cloud target is not on the server allowlist")
        if manifest.harness not in self.harnesses:
            raise ProtocolError(ErrorCode.NOT_ALLOWED, "harness is not on the server allowlist")
        if manifest.model not in self.models:
            raise ProtocolError(ErrorCode.NOT_ALLOWED, "model is not on the server allowlist")
        if manifest.thinking not in self.thinking:
            raise ProtocolError(ErrorCode.NOT_ALLOWED, "thinking level is not on the server allowlist")
        compatibility = self.profiles.get(manifest.toolchain_profile)
        if compatibility is None:
            raise ProtocolError(ErrorCode.NOT_ALLOWED, "toolchain profile is not on the server allowlist")
        if manifest.source_upload.size_bytes > self.max_source_bytes:
            raise ProtocolError(ErrorCode.NOT_ALLOWED, "source size is above the server limit")
        if manifest.budget.max_runtime_seconds > self.max_runtime_seconds:
            raise ProtocolError(ErrorCode.NOT_ALLOWED, "runtime budget is above the server limit")
        return compatibility


def _require_subset(name: str, values: frozenset[str], allowed: tuple[str, ...]) -> None:
    unknown = values - set(allowed)
    if unknown:
        raise PolicyError(f"{name} names values outside the protocol: {sorted(unknown)}")


def _require_unique(name: str, values: Iterable[str]) -> None:
    listed = list(values)
    if len(set(listed)) != len(listed):
        raise PolicyError(f"{name} must be unique across identities")


class _IdentityDocument(BaseModel):
    model_config = ConfigDict(extra="forbid")

    subject: str
    email: str
    role: Role
    owner_id: str | None = None


class _RetentionDocument(BaseModel):
    model_config = ConfigDict(extra="forbid")

    inputs_days: int = Field(ge=1)
    results_checkpoints_days: int = Field(ge=1)
    claims_status_state_days: int = Field(ge=1)
    logs_days: int = Field(ge=1)


class _AdmissionDocument(BaseModel):
    model_config = ConfigDict(extra="forbid", strict=True)

    version: int = Field(ge=1)
    team_queue_depth: int = Field(ge=1)
    owner_queue_depth: int = Field(ge=1)
    team_worker_slots: int = Field(ge=1)
    owner_worker_slots: int = Field(ge=1)
    team_daily_worker_seconds: int = Field(ge=1)
    queued_deadline_seconds: int = Field(ge=1)


class _PolicyDocument(BaseModel):
    model_config = ConfigDict(extra="forbid")

    server_id: str
    targets: list[str]
    harnesses: list[str]
    models: list[str]
    thinking: list[str]
    profiles: dict[str, ToolCompatibility]
    stale_after_seconds: int = Field(ge=1)
    skew_seconds: int = Field(ge=0)
    admission: _AdmissionDocument
    startup_timeout_seconds: int = Field(default=STARTUP_TIMEOUT_SECONDS, ge=1)
    max_attempts: int = Field(default=MAX_ATTEMPTS, ge=1, le=MAX_ATTEMPTS)
    max_source_bytes: int = MAX_ADMITTED_SOURCE_BYTES
    max_runtime_seconds: int = MAX_RUNTIME_SECONDS
    api_audience: str | None = None
    identities: list[_IdentityDocument] = Field(default_factory=list)
    retention: _RetentionDocument | None = None
    access_mode: AccessMode = AccessMode.ALLOWLIST


def policy_from_text(text: str) -> Policy:
    try:
        document = _PolicyDocument.model_validate(json.loads(text))
    except (ValueError, ValidationError) as exc:
        raise PolicyError(f"policy file is not valid: {exc}") from exc
    return Policy(
        server_id=document.server_id,
        targets=frozenset(document.targets),
        harnesses=frozenset(document.harnesses),
        models=frozenset(document.models),
        thinking=frozenset(document.thinking),
        profiles=dict(document.profiles),
        stale_after_seconds=document.stale_after_seconds,
        skew_seconds=document.skew_seconds,
        admission=AdmissionLimits(**document.admission.model_dump()),
        startup_timeout_seconds=document.startup_timeout_seconds,
        max_attempts=document.max_attempts,
        max_source_bytes=document.max_source_bytes,
        max_runtime_seconds=document.max_runtime_seconds,
        api_audience=document.api_audience,
        identities=tuple(
            Identity(
                subject=entry.subject,
                email=entry.email,
                role=entry.role,
                owner_id=entry.owner_id,
            )
            for entry in document.identities
        ),
        retention=(
            RetentionDurations(**document.retention.model_dump())
            if document.retention is not None
            else None
        ),
        access_mode=document.access_mode,
    )


def load_policy(path: Path) -> Policy:
    try:
        text = path.read_text(encoding="utf-8")
    except (OSError, UnicodeDecodeError) as exc:
        raise PolicyError(f"cannot read policy file {path}: {exc}") from exc
    return policy_from_text(text)
