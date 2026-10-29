"""Tool replay policy after an interrupted attempt (DPES-869, captain decisions c and Q3).

Only operation classes proved replay-safe rerun when a resumed attempt restores an interrupted
tool call. Shell and network effects are never rerun: their outcome may already exist outside
the workspace, so the interrupted call is surfaced for an owner decision. A class this module
does not know is treated the same way, so a new tool class cannot rerun until it is classified.

IDEMPOTENT_EDIT replays rely on the caller restoring the workspace from the last committed
checkpoint before the replay; this module does not enforce that ordering.

TOOL_REPLAY_CLASSES maps the built-in tools of the pinned pi-durable CodingTools (read, write,
edit, bash) to their class. Any other tool name, including grep, glob, ls, and custom tools,
has no class and is never replayed until it is defined and classified here.
"""

from collections.abc import Iterable, Mapping
from dataclasses import dataclass
from enum import StrEnum
from types import MappingProxyType


class ToolClass(StrEnum):
    READ = "read"
    SEARCH = "search"
    IDEMPOTENT_EDIT = "idempotent_edit"
    SHELL = "shell"
    NETWORK = "network"


class ReplayOutcome(StrEnum):
    REPLAY = "replay"
    INTERRUPTED_NEEDS_OWNER = "interrupted_needs_owner"


_REPLAY_SAFE: frozenset[ToolClass] = frozenset(
    {ToolClass.READ, ToolClass.SEARCH, ToolClass.IDEMPOTENT_EDIT}
)

TOOL_REPLAY_CLASSES: Mapping[str, ToolClass] = MappingProxyType(
    {
        "read": ToolClass.READ,
        "edit": ToolClass.IDEMPOTENT_EDIT,
        "write": ToolClass.IDEMPOTENT_EDIT,
        "bash": ToolClass.SHELL,
    }
)


def replay_outcome(tool_class: ToolClass | str) -> ReplayOutcome:
    """Return what a resumed attempt does with an interrupted call of this class."""
    try:
        resolved = ToolClass(tool_class)
    except ValueError:
        return ReplayOutcome.INTERRUPTED_NEEDS_OWNER
    if resolved in _REPLAY_SAFE:
        return ReplayOutcome.REPLAY
    return ReplayOutcome.INTERRUPTED_NEEDS_OWNER


def tool_replay_outcome(tool_name: str) -> ReplayOutcome:
    """Return what a resumed attempt does with an interrupted call to this named tool."""
    tool_class = TOOL_REPLAY_CLASSES.get(tool_name)
    if tool_class is None:
        return ReplayOutcome.INTERRUPTED_NEEDS_OWNER
    return replay_outcome(tool_class)


@dataclass(frozen=True)
class InterruptedCall:
    """A tool call that was in flight when its attempt was interrupted."""

    call_id: str
    tool_name: str


@dataclass(frozen=True)
class HeldCall:
    """An interrupted call that will not rerun. Its outcome stays on the record for an owner."""

    call_id: str
    tool_name: str
    outcome: ReplayOutcome


@dataclass(frozen=True)
class ResumePlan:
    """What a resumed attempt may rerun, and what it must hold. Planning executes nothing."""

    replay: tuple[InterruptedCall, ...]
    held: tuple[HeldCall, ...]


def plan_resume(calls: Iterable[InterruptedCall]) -> ResumePlan:
    """Split interrupted calls, in their original order, into replayable and held sets.

    Every input call lands in exactly one of the two sets. A held call keeps the outcome
    INTERRUPTED_NEEDS_OWNER so the interruption stays observable instead of disappearing.
    """
    replay: list[InterruptedCall] = []
    held: list[HeldCall] = []
    for call in calls:
        outcome = tool_replay_outcome(call.tool_name)
        if outcome is ReplayOutcome.REPLAY:
            replay.append(call)
        else:
            held.append(
                HeldCall(
                    call_id=call.call_id, tool_name=call.tool_name, outcome=outcome
                )
            )
    return ResumePlan(replay=tuple(replay), held=tuple(held))
