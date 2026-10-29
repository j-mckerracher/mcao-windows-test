"""Scheduler progress judged with the worker heartbeat freshness rules.

A Scheduler job can be disabled or silently stop advancing while the API keeps answering reads of
individual runs. Those reads prove nothing about the Scheduler, so this judgement takes only the
time of the last successful Scheduler pass. A missing, stale, future, or backward value is never
healthy, and it reuses protocol.freshness instead of defining a second clock contract.
"""

from __future__ import annotations

from datetime import datetime

from mao_cloud.protocol import Freshness, freshness


def scheduler_freshness(
    *,
    last_success_at: datetime | None,
    previous_success_at: datetime | None,
    now: datetime,
    max_age_seconds: int,
    skew_seconds: int,
) -> Freshness:
    """FRESH only when the last successful pass is recent and was not rewound from an earlier one."""
    if (
        last_success_at is not None
        and previous_success_at is not None
        and last_success_at < previous_success_at
    ):
        return Freshness.CLOCK_ANOMALY
    return freshness(
        last_seen_at=last_success_at,
        now=now,
        stale_after_seconds=max_age_seconds,
        skew_seconds=skew_seconds,
    )
