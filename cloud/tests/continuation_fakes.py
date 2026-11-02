"""Labeled fakes for the continuation reconciler. Every result these produce is FAKE.

They stand in for the compare-and-set run store and the Run Admin until DPES-869's real store and
the Cloud Run admin client exist. Results proven against them show the claim logic and the policy,
never the durable store or live execution.
"""

import threading
from collections.abc import Callable

from mao_cloud.continuation_reconcile import ExecutionObservation
from mao_cloud.protocol import RunRecord


class FakeRunStore:
    """FAKE compare-and-set store, held in memory under one lock.

    The hook from `set_after_load` runs after each read returns and before the caller can
    compare-and-set. Tests use it to land another writer inside that window, either directly or
    behind a barrier that holds every racing reader at the same snapshot.
    """

    def __init__(self, record: RunRecord) -> None:
        self._record = record
        self._lock = threading.Lock()
        self._after_load: Callable[[], None] | None = None
        self.compare_and_set_calls = 0

    @property
    def current(self) -> RunRecord:
        with self._lock:
            return self._record

    def set_after_load(self, hook: Callable[[], None]) -> None:
        self._after_load = hook

    def load(self, run_id: str) -> RunRecord:
        with self._lock:
            snapshot = self._record
        if self._after_load is not None:
            self._after_load()
        return snapshot

    def compare_and_set(
        self, run_id: str, *, expected_version: int, record: RunRecord
    ) -> bool:
        with self._lock:
            self.compare_and_set_calls += 1
            if record.run_id != run_id or self._record.version != expected_version:
                return False
            self._record = record
            return True

    def land_other_writer(self, record: RunRecord) -> None:
        """FAKE: another writer (cancel, late checkpoint, terminal result) commits `record`."""
        with self._lock:
            self._record = record


class FakeRunAdmin:
    """FAKE Run Admin. Answers every observation with one scripted execution state."""

    def __init__(self, observation: ExecutionObservation) -> None:
        self._observation = observation
        self.observations: list[tuple[str, int]] = []

    def observe(self, run_id: str, attempt: int) -> ExecutionObservation:
        self.observations.append((run_id, attempt))
        return self._observation
