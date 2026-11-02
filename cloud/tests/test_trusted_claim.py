"""Trusted claim: one create-only attempt claim, with an ownership proof for ambiguous outcomes.

DPES-811 AC2. The claim store here is an in-memory script of outcomes, so these tests prove the
decision logic only. The signed GCS adapter that produces real 412 and ambiguous responses is
DPES-807 and is not simulated here.
"""

import re
from collections.abc import Mapping

import pytest

from mao_cloud.protocol import (
    ClaimDocument,
    ObjectKind,
    canonical_bytes,
    object_name,
)
from mao_cloud.runner import PlannedWrite
from mao_cloud.trusted_claim import (
    ClaimAmbiguous,
    ClaimConflict,
    ClaimRefused,
    ClaimUnreadable,
    claim_attempt,
    new_runner_nonce,
)

RUN_ID = "run_" + "a" * 32
CLAIM_ID = "claim_" + "b" * 32
OUR_NONCE = "1f" * 16
OTHER_NONCE = "2e" * 16


def _planned(nonce: str = OUR_NONCE) -> PlannedWrite:
    claim = ClaimDocument(
        protocol_version="v1",
        run_id=RUN_ID,
        attempt=1,
        fence=1,
        claim_id=CLAIM_ID,
        runner_nonce=nonce,
    )
    return PlannedWrite(
        name=object_name(ObjectKind.CLAIM, run_id=RUN_ID, attempt=1),
        method="PUT",
        headers={"x-goog-if-generation-match": "0"},
        body=canonical_bytes(claim.model_dump(mode="json")),
    )


class ScriptedStore:
    """Replays scripted create outcomes and a scripted readback. Records every call."""

    def __init__(
        self,
        *,
        create: Exception | None = None,
        readback: bytes | None | Exception = None,
    ) -> None:
        self._create = create
        self._readback = readback
        self.creates: list[tuple[str, Mapping[str, str]]] = []
        self.reads: list[str] = []

    def create(self, name: str, body: bytes, headers: Mapping[str, str]) -> int:
        self.creates.append((name, headers))
        if self._create is not None:
            raise self._create
        return 1

    def read(self, name: str) -> bytes | None:
        self.reads.append(name)
        if isinstance(self._readback, Exception):
            raise self._readback
        return self._readback


# --- the create succeeds -------------------------------------------------------------------


def test_a_created_claim_needs_no_readback() -> None:
    store = ScriptedStore()
    claim_attempt(store, _planned(), runner_nonce=OUR_NONCE)
    assert len(store.creates) == 1
    assert store.reads == []


# --- 412: another runner holds the attempt --------------------------------------------------


def test_a_412_refuses_and_is_never_retried_or_deleted() -> None:
    store = ScriptedStore(create=ClaimConflict("exists"))
    with pytest.raises(ClaimRefused, match="another runner"):
        claim_attempt(store, _planned(), runner_nonce=OUR_NONCE)
    assert len(store.creates) == 1
    assert store.reads == []


def test_the_create_carries_the_generation_zero_precondition() -> None:
    store = ScriptedStore()
    claim_attempt(store, _planned(), runner_nonce=OUR_NONCE)
    _, headers = store.creates[0]
    assert headers == {"x-goog-if-generation-match": "0"}


# --- ambiguous create: ownership must be proven by exact-key readback -----------------------


def test_an_ambiguous_create_proceeds_when_the_readback_carries_our_nonce() -> None:
    planned = _planned()
    store = ScriptedStore(create=ClaimAmbiguous("timeout"), readback=planned.body)
    claim_attempt(store, planned, runner_nonce=OUR_NONCE)
    assert store.reads == [planned.name]
    assert len(store.creates) == 1


def test_an_ambiguous_create_refuses_when_the_readback_names_another_runner() -> None:
    store = ScriptedStore(create=ClaimAmbiguous("reset"), readback=_planned(OTHER_NONCE).body)
    with pytest.raises(ClaimRefused, match="another runner"):
        claim_attempt(store, _planned(), runner_nonce=OUR_NONCE)


def test_an_ambiguous_create_refuses_when_no_claim_is_visible() -> None:
    store = ScriptedStore(create=ClaimAmbiguous("503"), readback=None)
    with pytest.raises(ClaimRefused, match="no claim"):
        claim_attempt(store, _planned(), runner_nonce=OUR_NONCE)


def test_an_ambiguous_create_refuses_when_the_readback_cannot_complete() -> None:
    store = ScriptedStore(create=ClaimAmbiguous("reset"), readback=ClaimUnreadable("503"))
    with pytest.raises(ClaimRefused, match="could not be read"):
        claim_attempt(store, _planned(), runner_nonce=OUR_NONCE)


def test_an_ambiguous_create_refuses_a_malformed_readback() -> None:
    store = ScriptedStore(create=ClaimAmbiguous("reset"), readback=b"{not json")
    with pytest.raises(ClaimRefused, match="malformed"):
        claim_attempt(store, _planned(), runner_nonce=OUR_NONCE)


def test_an_ambiguous_create_refuses_a_readback_for_another_attempt() -> None:
    other_attempt = ClaimDocument(
        protocol_version="v1",
        run_id=RUN_ID,
        attempt=2,
        fence=2,
        claim_id=CLAIM_ID,
        runner_nonce=OUR_NONCE,
    )
    store = ScriptedStore(
        create=ClaimAmbiguous("reset"),
        readback=canonical_bytes(other_attempt.model_dump(mode="json")),
    )
    with pytest.raises(ClaimRefused, match="another attempt"):
        claim_attempt(store, _planned(), runner_nonce=OUR_NONCE)


def test_an_ambiguous_create_is_not_retried() -> None:
    planned = _planned()
    store = ScriptedStore(create=ClaimAmbiguous("reset"), readback=planned.body)
    claim_attempt(store, planned, runner_nonce=OUR_NONCE)
    assert len(store.creates) == 1


# --- nonce generation -----------------------------------------------------------------------


def test_a_new_runner_nonce_is_32_lowercase_hex_and_unpredictable() -> None:
    first, second = new_runner_nonce(), new_runner_nonce()
    assert re.fullmatch(r"[0-9a-f]{32}", first)
    assert first != second


def test_a_new_runner_nonce_is_accepted_by_the_claim_document() -> None:
    nonce = new_runner_nonce()
    assert _planned(nonce).body  # builds without a validation error
