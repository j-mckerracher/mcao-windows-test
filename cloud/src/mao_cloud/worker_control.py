"""Worker-side control gate: the cancel check before each model call, polled at most every 15 seconds.

The controller writes control.json (identity only) before dispatch and cancel.json at most once per
attempt. The runner holds read-only capabilities for both. This gate decides whether the attempt may
begin another model call. Reads go through an injected port, so no storage client lives here. The
production reader and the in-flight poll during a model call belong to the runner loop, which is not
in this tree (protocol-v1.md gap 36; DPES-807 and DPES-819).

Fail closed: a missing control object, an identity mismatch, a cancel directive, or a failed read
stops the attempt from making a model call. A cancel observed once is latched for the attempt.
"""

from dataclasses import dataclass
from datetime import datetime, timedelta
from typing import Literal, Protocol

from mao_cloud.protocol import CancelDocument, ControlDocument, WorkerBootstrap

CONTROL_POLL_SECONDS = 15

Refusal = Literal["control_missing", "identity_mismatch", "cancel_requested"]


class ControlRefused(Exception):
    """The attempt must not make a model call. reason says why; nothing is written or discarded."""

    def __init__(self, reason: Refusal) -> None:
        super().__init__(reason)
        self.reason: Refusal = reason


@dataclass(frozen=True)
class AttemptIdentity:
    """The identity a runner was bootstrapped with. Control and cancel documents must match it."""

    run_id: str
    attempt: int
    fence: int
    claim_id: str

    @classmethod
    def from_bootstrap(cls, bootstrap: WorkerBootstrap) -> "AttemptIdentity":
        return cls(
            run_id=bootstrap.run_id,
            attempt=bootstrap.attempt,
            fence=bootstrap.fence,
            claim_id=bootstrap.claim_id,
        )

    def matches(self, document: ControlDocument | CancelDocument) -> bool:
        return (
            document.run_id == self.run_id
            and document.attempt == self.attempt
            and document.fence == self.fence
            and document.claim_id == self.claim_id
        )


class ControlReader(Protocol):
    """Read-only access to one attempt's control and cancel objects. None means the object is absent."""

    def read_control(self, run_id: str, attempt: int) -> ControlDocument | None: ...

    def read_cancel(self, run_id: str, attempt: int) -> CancelDocument | None: ...


class ControlGate:
    """Decides, before each model call, whether the attempt is still allowed to proceed.

    The control object is read once and must match the identity. The cancel object is polled only
    when the last poll is at least CONTROL_POLL_SECONDS old, so a model call may start up to that
    long after a cancel was written. A poll that raises records nothing, so the next call retries it.
    """

    def __init__(self, identity: AttemptIdentity, reader: ControlReader) -> None:
        self._identity = identity
        self._reader = reader
        self._control_verified = False
        self._last_poll_at: datetime | None = None
        self._stop: Refusal | None = None

    def before_model_call(self, now: datetime) -> None:
        """Return only when no stop is known. Raises ControlRefused otherwise."""
        if self._stop is not None:
            raise ControlRefused(self._stop)
        if not self._control_verified:
            control = self._reader.read_control(
                self._identity.run_id, self._identity.attempt
            )
            if control is None:
                raise ControlRefused("control_missing")
            if not self._identity.matches(control):
                raise ControlRefused("identity_mismatch")
            self._control_verified = True
        if self._poll_due(now):
            cancel = self._reader.read_cancel(
                self._identity.run_id, self._identity.attempt
            )
            self._last_poll_at = now
            if cancel is not None:
                self._stop = (
                    "cancel_requested"
                    if self._identity.matches(cancel)
                    else "identity_mismatch"
                )
                raise ControlRefused(self._stop)

    def _poll_due(self, now: datetime) -> bool:
        if self._last_poll_at is None or now < self._last_poll_at:
            return True
        return now - self._last_poll_at >= timedelta(seconds=CONTROL_POLL_SECONDS)
