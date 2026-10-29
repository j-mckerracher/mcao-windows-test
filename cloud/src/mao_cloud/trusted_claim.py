"""Trusted claim: take one attempt with a create-only write, proving ownership if the answer is unclear.

The worker runs this before it runs any model or repository code. Only the store adapter talks to
the network; this module holds the decision logic and no I/O.

Outcomes:
- created: the claim is ours. Continue.
- 412 (ClaimConflict): another writer holds the attempt. Refuse. Never retry, never delete.
- ambiguous (ClaimAmbiguous): the create may or may not have landed. Read the claim back by its
  exact key. Continue only if the readback names this process's runner nonce and this attempt.

Nothing here deletes or overwrites a claim, so a live claim is never reused.
"""

import secrets
from collections.abc import Mapping
from typing import Protocol

from pydantic import ValidationError

from mao_cloud.protocol import ClaimDocument
from mao_cloud.runner import PlannedWrite

RUNNER_NONCE_BYTES = 16


class ClaimConflict(Exception):
    """The create-only name already exists (HTTP 412)."""


class ClaimAmbiguous(Exception):
    """The create's outcome is unknown: timeout, connection reset, or a 5xx response."""


class ClaimUnreadable(Exception):
    """The exact-key readback could not complete."""


class ClaimRefused(Exception):
    """The worker must not run this attempt. The message is the reason, for the durable record."""


class ClaimStore(Protocol):
    """The write and readback surface the claim needs. Deliberately has no delete."""

    def create(self, name: str, body: bytes, headers: Mapping[str, str]) -> int:
        """Create the object with the plan's headers. Returns its generation.

        Raises ClaimConflict on 412 and ClaimAmbiguous when the outcome is unknown.
        """

    def read(self, name: str) -> bytes | None:
        """Return the object's bytes by exact key, or None if absent. Raises ClaimUnreadable."""


def new_runner_nonce() -> str:
    """An unpredictable nonce unique to this runner process."""
    return secrets.token_hex(RUNNER_NONCE_BYTES)


def claim_attempt(store: ClaimStore, planned: PlannedWrite, *, runner_nonce: str) -> None:
    """Take the attempt, or raise ClaimRefused. Returns only when this runner owns the claim."""
    try:
        store.create(planned.name, planned.body, planned.headers)
    except ClaimConflict as exc:
        raise ClaimRefused("the attempt already has a claim; another runner holds it") from exc
    except ClaimAmbiguous:
        _prove_ownership(store, planned, runner_nonce=runner_nonce)


def _prove_ownership(store: ClaimStore, planned: PlannedWrite, *, runner_nonce: str) -> None:
    try:
        readback = store.read(planned.name)
    except ClaimUnreadable as exc:
        raise ClaimRefused("the claim after an ambiguous create could not be read") from exc
    if readback is None:
        raise ClaimRefused("the create was ambiguous and no claim is visible; refusing to run")
    try:
        found = ClaimDocument.model_validate_json(readback)
    except ValidationError as exc:
        raise ClaimRefused("the claim readback is malformed") from exc
    expected = ClaimDocument.model_validate_json(planned.body)
    if found.model_dump(exclude={"runner_nonce"}) != expected.model_dump(exclude={"runner_nonce"}):
        raise ClaimRefused("the claim readback is for another attempt")
    if not secrets.compare_digest(found.runner_nonce, runner_nonce):
        raise ClaimRefused("the claim belongs to another runner")
