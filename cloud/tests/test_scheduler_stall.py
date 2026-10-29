"""A silently stalled Scheduler is not masked by healthy per-run reads (DPES-815 edge addendum).

Scheduler health is judged only from the time of its last successful pass, with the same freshness
rules that judge worker heartbeats. Reads that reconcile individual runs carry no progress evidence,
so the judgement takes no such input.
"""

from datetime import datetime, timedelta, timezone

from mao_cloud.protocol import Freshness
from mao_cloud.scheduler_health import scheduler_freshness

NOW = datetime(2026, 10, 22, 12, 0, tzinfo=timezone.utc)
MAX_AGE_SECONDS = 900
SKEW_SECONDS = 30


def _judge(
    last_success_at: datetime | None,
    previous_success_at: datetime | None = None,
) -> Freshness:
    return scheduler_freshness(
        last_success_at=last_success_at,
        previous_success_at=previous_success_at,
        now=NOW,
        max_age_seconds=MAX_AGE_SECONDS,
        skew_seconds=SKEW_SECONDS,
    )


def test_a_recent_successful_pass_is_fresh() -> None:
    assert _judge(NOW - timedelta(seconds=60)) is Freshness.FRESH


def test_a_pass_exactly_at_the_window_edge_is_still_fresh() -> None:
    assert _judge(NOW - timedelta(seconds=MAX_AGE_SECONDS)) is Freshness.FRESH


def test_a_silently_nonadvancing_scheduler_is_stale_even_when_run_reads_succeed() -> None:
    # The per-run reads that reconcile individual runs are healthy by assumption here. They are
    # not an input, so they cannot make an old last pass look current.
    stalled_since = NOW - timedelta(seconds=MAX_AGE_SECONDS + 1)

    assert _judge(stalled_since) is Freshness.STALE


def test_a_disabled_scheduler_with_no_recorded_pass_is_stale() -> None:
    assert _judge(None) is Freshness.STALE


def test_a_pass_recorded_in_the_future_beyond_skew_is_a_clock_anomaly_not_health() -> None:
    assert _judge(NOW + timedelta(seconds=SKEW_SECONDS + 1)) is Freshness.CLOCK_ANOMALY


def test_a_pass_recorded_within_skew_of_the_future_is_accepted() -> None:
    assert _judge(NOW + timedelta(seconds=SKEW_SECONDS)) is Freshness.FRESH


def test_a_last_pass_that_moves_backward_is_a_clock_anomaly() -> None:
    previous = NOW - timedelta(seconds=120)
    rewritten = NOW - timedelta(seconds=600)

    assert _judge(rewritten, previous_success_at=previous) is Freshness.CLOCK_ANOMALY
