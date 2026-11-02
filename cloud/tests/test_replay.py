"""Replay policy for interrupted tool calls (DPES-869 AC5, T5 at the policy level)."""

import pytest

from mao_cloud.replay import (
    HeldCall,
    InterruptedCall,
    ReplayOutcome,
    ToolClass,
    plan_resume,
    replay_outcome,
    tool_replay_outcome,
)


@pytest.mark.parametrize(
    ("tool_class", "expected"),
    [
        ("read", ReplayOutcome.REPLAY),
        ("search", ReplayOutcome.REPLAY),
        ("idempotent_edit", ReplayOutcome.REPLAY),
        ("shell", ReplayOutcome.INTERRUPTED_NEEDS_OWNER),
        ("network", ReplayOutcome.INTERRUPTED_NEEDS_OWNER),
    ],
)
def test_each_declared_class_has_its_decided_outcome(
    tool_class: str, expected: ReplayOutcome
) -> None:
    assert replay_outcome(tool_class) is expected


def test_enum_members_resolve_the_same_as_their_values() -> None:
    assert replay_outcome(ToolClass.IDEMPOTENT_EDIT) is ReplayOutcome.REPLAY
    assert replay_outcome(ToolClass.SHELL) is ReplayOutcome.INTERRUPTED_NEEDS_OWNER


@pytest.mark.parametrize("tool_class", ["exec", "fetch", "READ", "", " read"])
def test_unknown_or_unclassified_class_is_never_replayed(tool_class: str) -> None:
    assert replay_outcome(tool_class) is ReplayOutcome.INTERRUPTED_NEEDS_OWNER


@pytest.mark.parametrize(
    ("tool_name", "expected"),
    [
        ("read", ReplayOutcome.REPLAY),
        ("edit", ReplayOutcome.REPLAY),
        ("write", ReplayOutcome.REPLAY),
        ("bash", ReplayOutcome.INTERRUPTED_NEEDS_OWNER),
    ],
)
def test_pinned_coding_tools_have_their_decided_outcome(
    tool_name: str, expected: ReplayOutcome
) -> None:
    assert tool_replay_outcome(tool_name) is expected


@pytest.mark.parametrize(
    "tool_name",
    ["grep", "glob", "ls", "Read", "READ", "Bash", "count", "", " read", "bash "],
)
def test_tool_names_outside_the_pinned_set_are_never_replayed(tool_name: str) -> None:
    assert tool_replay_outcome(tool_name) is ReplayOutcome.INTERRUPTED_NEEDS_OWNER


# --- Resume planning: an interrupted unsafe call is held, never replayed (DPES-869 AC5, T5) ----


def test_an_interrupted_bash_call_is_held_with_its_outcome_and_never_replayed() -> None:
    plan = plan_resume([InterruptedCall(call_id="call-1", tool_name="bash")])
    assert plan.replay == ()
    assert plan.held == (
        HeldCall(
            call_id="call-1",
            tool_name="bash",
            outcome=ReplayOutcome.INTERRUPTED_NEEDS_OWNER,
        ),
    )


@pytest.mark.parametrize("tool_name", ["bash", "grep", "fetch", "Bash", ""])
def test_no_unsafe_or_unknown_interrupted_call_reaches_the_replay_list(
    tool_name: str,
) -> None:
    plan = plan_resume([InterruptedCall(call_id="call-x", tool_name=tool_name)])
    assert plan.replay == ()
    assert [held.call_id for held in plan.held] == ["call-x"]
    assert plan.held[0].outcome is ReplayOutcome.INTERRUPTED_NEEDS_OWNER


def test_mixed_interrupted_calls_split_in_order_and_each_is_accounted_for_once() -> (
    None
):
    calls = [
        InterruptedCall(call_id="call-1", tool_name="read"),
        InterruptedCall(call_id="call-2", tool_name="bash"),
        InterruptedCall(call_id="call-3", tool_name="edit"),
        InterruptedCall(call_id="call-4", tool_name="grep"),
        InterruptedCall(call_id="call-5", tool_name="write"),
    ]
    plan = plan_resume(calls)
    assert [call.call_id for call in plan.replay] == ["call-1", "call-3", "call-5"]
    assert [held.call_id for held in plan.held] == ["call-2", "call-4"]
    assert all(
        held.outcome is ReplayOutcome.INTERRUPTED_NEEDS_OWNER for held in plan.held
    )
    accounted = [call.call_id for call in plan.replay] + [
        held.call_id for held in plan.held
    ]
    assert sorted(accounted) == sorted(call.call_id for call in calls)


def test_an_empty_interrupted_set_yields_an_empty_plan() -> None:
    plan = plan_resume([])
    assert plan.replay == ()
    assert plan.held == ()
