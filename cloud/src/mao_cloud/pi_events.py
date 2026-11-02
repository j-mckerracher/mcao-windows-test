"""Map pi's JSON event stream onto bounded, sanitized worker log records.

Child stdout and stderr are untrusted. A line is forwarded only when it is one JSON object whose
type is one of the four pi events the protocol names, and the record is built from a fixed field
whitelist: the tool name when it is a safe token and the error flag when it is a boolean. Payload
text, arguments, results, unknown types, and oversized or over-nested lines are dropped and
counted, so a credential printed by a model or a tool never reaches a log object.
"""

from __future__ import annotations

import json
import re
from collections.abc import Iterable
from dataclasses import dataclass

# Parsing is bounded: a line this large is dropped before json.loads ever sees it.
MAX_PI_EVENT_CHARS = 1024 * 1024
# Nesting is bounded here, outer object included: json's own limit varies by interpreter.
MAX_PI_EVENT_DEPTH = 32
_PI_EVENTS = frozenset({"message_end", "tool_execution_start", "tool_execution_end", "agent_end"})
_TOOL_TYPES = frozenset({"tool_execution_start", "tool_execution_end"})
_SAFE_TOOL_NAME = re.compile(r"[A-Za-z0-9_.-]{1,64}")


@dataclass(frozen=True)
class PiMapping:
    records: tuple[str, ...]
    dropped: int


def map_pi_lines(lines: Iterable[str], *, max_records: int) -> PiMapping:
    """Map output lines to at most max_records records. Every line not recorded is counted."""
    if max_records < 1:
        raise ValueError("max_records must be positive")
    records: list[str] = []
    dropped = 0
    for raw in lines:
        record = _record(raw)
        if record is None or len(records) >= max_records:
            dropped += 1
            continue
        records.append(record)
    return PiMapping(records=tuple(records), dropped=dropped)


def _exceeds_depth(raw: str) -> bool:
    depth = 0
    in_string = False
    escaped = False
    for char in raw:
        if in_string:
            if escaped:
                escaped = False
            elif char == "\\":
                escaped = True
            elif char == '"':
                in_string = False
        elif char == '"':
            in_string = True
        elif char in ("[", "{"):
            depth += 1
            if depth > MAX_PI_EVENT_DEPTH:
                return True
        elif char in ("]", "}"):
            depth -= 1
    return False


def _record(raw: str) -> str | None:
    if len(raw) > MAX_PI_EVENT_CHARS or _exceeds_depth(raw):
        return None
    try:
        event = json.loads(raw)
    except (ValueError, RecursionError):
        return None
    if not isinstance(event, dict):
        return None
    kind = event.get("type")
    if not isinstance(kind, str) or kind not in _PI_EVENTS:
        return None
    parts = [f"pi {kind}"]
    if kind in _TOOL_TYPES:
        tool = event.get("toolName")
        if isinstance(tool, str) and _SAFE_TOOL_NAME.fullmatch(tool):
            parts.append(f"tool={tool}")
    if kind == "tool_execution_end":
        failed = event.get("isError")
        if isinstance(failed, bool):
            parts.append(f"error={'true' if failed else 'false'}")
    return " ".join(parts)
