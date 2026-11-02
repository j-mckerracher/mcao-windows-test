"""pi headless adapter: the argv, the event-stream verdict, and the trusted-bootstrap run port.

DPES-813. The worker runs the pinned pi coding agent in JSON mode against an approved Vertex Gemini
model, under the trusted supervisor and sandbox. pi exits 0 when a model call fails, so the verdict
comes from the event stream: the last assistant message_end stopReason, the tool events, and whether
every line parsed. Effective model identity and token usage are kept from the same stream.

Only the pi-specific parts live here. Process supervision, redaction, and sandboxing are the merged
trusted slice's; the replay classes are trusted_replay's. Real pi, Vertex, and bubblewrap runs are not
proved in this module: they are stated gaps naming DPES-804, DPES-819, and DPES-871.

models.json at cloud/config/pi/models.json sets providers.google-vertex.apiKey to the sentinel
"gcp-vertex-credentials". That is not a secret. It makes pi use the SDK's default credential chain
(the worker service account through the metadata server) instead of an ADC file (upstream
earendil-works/pi#5323).
"""

import json
import re
from collections.abc import Callable, Iterable, Sequence
from dataclasses import dataclass
from enum import StrEnum
from pathlib import Path
from typing import Literal, get_args

from mao_cloud.pi_events import map_pi_lines
from mao_cloud.protocol import MAX_LOG_OBJECT_BYTES, Thinking, VertexModel
from mao_cloud.trusted_sandbox import bwrap_argv, worker_env
from mao_cloud.trusted_supervise import (
    ChildOutcome,
    ChildResult,
    OutputLine,
    SupervisorTiming,
    supervise,
)

PI_VERSION = "1.3.0"
PI_EXECUTABLE: tuple[str, ...] = ("pi",)
PROVIDER = "google-vertex"
# The built-in tools, in replay-class order. Pinned to TOOL_REPLAY_CLASSES by the test suite.
PI_TOOLS: tuple[str, ...] = ("read", "write", "edit", "bash")
MAX_TASK_BYTES = 64 * 1024
# The largest stream line the verdict will parse, in bytes. A longer line is truncated and so fails
# closed. pi_events.MAX_PI_EVENT_CHARS is the separate, character-counted cap on log records.
MAX_PI_EVENT_BYTES = MAX_LOG_OBJECT_BYTES

_THROTTLE_MARKER = re.compile(
    r"\b429\b|resource_exhausted|rate.?limit|too many requests|quota exceeded",
    re.IGNORECASE,
)
_AUTH_MARKER = re.compile(
    r"\b40[13]\b|unauthenticated|unauthorized|permission_denied|permission denied|forbidden"
    r"|credentials",
    re.IGNORECASE,
)


class PiReason(StrEnum):
    COMPLETED = "completed"
    MALFORMED_STREAM = "malformed_stream"
    TOOL_POLICY_VIOLATION = "tool_policy_violation"
    NO_TERMINAL_MESSAGE = "no_terminal_message"
    AUTH_FAILED = "auth_failed"
    THROTTLED = "throttled"
    MODEL_ERROR = "model_error"
    OUTPUT_TRUNCATED = "output_truncated"
    ABORTED = "aborted"
    INCOMPLETE_TURN = "incomplete_turn"
    LAST_SHELL_COMMAND_FAILED = "last_shell_command_failed"


@dataclass(frozen=True)
class PiVerdict:
    """The lifecycle verdict and normalized facts from one stream. Holds no raw provider text."""

    status: Literal["succeeded", "failed"]
    reason: PiReason
    effective_model: str | None
    effective_provider: str | None
    tool_calls: int
    tool_errors: int
    input_tokens: int | None
    output_tokens: int | None

    @property
    def succeeded(self) -> bool:
        return self.status == "succeeded"


def _check_task(task: str) -> None:
    if not task or task.startswith(("-", "@")):
        raise ValueError("the task must be non-empty and must not start with '-' or '@'")
    if "\x00" in task:
        raise ValueError("the task must not contain NUL bytes")
    if len(task.encode("utf-8")) > MAX_TASK_BYTES:
        raise ValueError(f"the task exceeds {MAX_TASK_BYTES} bytes")


def pi_args(*, model: VertexModel, thinking: Thinking, task: str) -> list[str]:
    """The pi arguments after the executable. The task is last, after `--`, and never a flag."""
    if model not in get_args(VertexModel):
        raise ValueError("the model is not on the approved Vertex allowlist")
    if thinking not in get_args(Thinking):
        raise ValueError("the thinking level is not on the allowlist")
    _check_task(task)
    return [
        "--mode",
        "json",
        "--provider",
        PROVIDER,
        "--model",
        model,
        "--thinking",
        thinking,
        "--no-session",
        "--no-extensions",
        "--no-mcp",
        "--no-skills",
        "--no-prompt-templates",
        "--no-themes",
        "--no-context-files",
        "--approve",
        "--tools",
        ",".join(PI_TOOLS),
        "--",
        task,
    ]


def _classify_error(text: str) -> PiReason:
    if _THROTTLE_MARKER.search(text):
        return PiReason.THROTTLED
    if _AUTH_MARKER.search(text):
        return PiReason.AUTH_FAILED
    return PiReason.MODEL_ERROR


def _add(total: int | None, value: object) -> int | None:
    if not isinstance(value, int) or isinstance(value, bool):
        return total
    return value if total is None else total + value


class PiStreamJudge:
    """Folds one JSON-mode stream into a verdict. Bounded state: nothing is retained per event."""

    def __init__(self) -> None:
        self._malformed = False
        self._violation = False
        self._seen_assistant = False
        self._stop: str | None = None
        self._error_text = ""
        self._model: str | None = None
        self._provider: str | None = None
        self._last_shell_failed = False
        self._tool_calls = 0
        self._tool_errors = 0
        self._input: int | None = None
        self._output: int | None = None

    def feed(self, line: str, *, truncated: bool = False) -> None:
        if truncated or len(line.encode("utf-8")) > MAX_PI_EVENT_BYTES:
            self._malformed = True
            return
        try:
            event = json.loads(line)
        except (ValueError, RecursionError):
            self._malformed = True
            return
        kind = event.get("type") if isinstance(event, dict) else None
        if not isinstance(kind, str):
            self._malformed = True
            return
        if kind == "message_end":
            self._message_end(event)
        elif kind in ("tool_execution_start", "tool_execution_end"):
            self._tool(kind, event)

    def _message_end(self, event: dict) -> None:
        message = event.get("message")
        if not isinstance(message, dict) or message.get("role") != "assistant":
            return
        self._seen_assistant = True
        stop = message.get("stopReason")
        self._stop = stop if isinstance(stop, str) else None
        error = message.get("errorMessage")
        self._error_text = error if isinstance(error, str) else ""
        model = message.get("model")
        self._model = model if isinstance(model, str) else None
        provider = message.get("provider")
        self._provider = provider if isinstance(provider, str) else None
        usage = message.get("usage")
        if isinstance(usage, dict):
            self._input = _add(self._input, usage.get("input"))
            self._output = _add(self._output, usage.get("output"))

    def _tool(self, kind: str, event: dict) -> None:
        name = event.get("toolName")
        if not isinstance(name, str):
            self._malformed = True
            return
        if name not in PI_TOOLS:
            self._violation = True
        if kind == "tool_execution_start":
            self._tool_calls += 1
            return
        is_error = event.get("isError")
        if not isinstance(is_error, bool):
            self._malformed = True
            return
        if is_error:
            self._tool_errors += 1
        if name == "bash":
            self._last_shell_failed = is_error

    def _reason(self) -> PiReason:
        if self._malformed:
            return PiReason.MALFORMED_STREAM
        if self._violation:
            return PiReason.TOOL_POLICY_VIOLATION
        if not self._seen_assistant:
            return PiReason.NO_TERMINAL_MESSAGE
        if self._stop == "error":
            return _classify_error(self._error_text)
        if self._stop == "stop":
            return (
                PiReason.LAST_SHELL_COMMAND_FAILED
                if self._last_shell_failed
                else PiReason.COMPLETED
            )
        if self._stop == "length":
            return PiReason.OUTPUT_TRUNCATED
        if self._stop == "aborted":
            return PiReason.ABORTED
        return PiReason.INCOMPLETE_TURN

    def verdict(self) -> PiVerdict:
        reason = self._reason()
        return PiVerdict(
            status="succeeded" if reason is PiReason.COMPLETED else "failed",
            reason=reason,
            effective_model=self._model,
            effective_provider=self._provider,
            tool_calls=self._tool_calls,
            tool_errors=self._tool_errors,
            input_tokens=self._input,
            output_tokens=self._output,
        )


def judge_stream(lines: Iterable[str]) -> PiVerdict:
    judge = PiStreamJudge()
    for line in lines:
        judge.feed(line)
    return judge.verdict()


def _no_heartbeat() -> None:
    """The heartbeat sink is wired by the production ports; the adapter only forwards it."""


class PiWorker:
    """The pi port for trusted_bootstrap.Ports.run_worker: run pi once under supervision and judge it.

    A clean zero exit is COMPLETED only when the stream verdict succeeded. Otherwise the outcome is
    FAILED, so pi's exit 0 on a failed model call is never reported as success. After a call, verdict
    holds the stream judgment, including on timeout, for diagnostics.
    """

    def __init__(
        self,
        *,
        task: str,
        model: VertexModel,
        thinking: Thinking,
        executable: Sequence[str] = PI_EXECUTABLE,
        wrap: Callable[[str, list[str]], list[str]] = bwrap_argv,
        timing: SupervisorTiming = SupervisorTiming(),
        heartbeat: Callable[[], None] = _no_heartbeat,
        log_sink: Callable[[str], None] | None = None,
        run: Callable[..., ChildResult] = supervise,
    ) -> None:
        self._args = pi_args(model=model, thinking=thinking, task=task)
        self._executable = list(executable)
        self._wrap = wrap
        self._timing = timing
        self._heartbeat = heartbeat
        self._log_sink = log_sink
        self._run = run
        self.verdict: PiVerdict | None = None

    def __call__(self, workspace: Path, max_runtime_seconds: float) -> ChildResult:
        judge = PiStreamJudge()

        def raw_sink(line: OutputLine) -> None:
            judge.feed(line.text, truncated=line.truncated)
            if not line.truncated and self._log_sink is not None:
                # Only pi_events' whitelisted records reach the log, never raw model or tool text.
                for record in map_pi_lines([line.text], max_records=1).records:
                    self._log_sink(record)

        argv = self._wrap(str(workspace), [*self._executable, *self._args])
        result = self._run(
            argv,
            env=worker_env(),
            max_runtime_seconds=max_runtime_seconds,
            sink=lambda _line: None,
            raw_sink=raw_sink,
            heartbeat=self._heartbeat,
            timing=self._timing,
            line_limit=MAX_PI_EVENT_BYTES,
        )
        self.verdict = judge.verdict()
        if result.outcome is ChildOutcome.COMPLETED and not self.verdict.succeeded:
            return ChildResult(
                outcome=ChildOutcome.FAILED, returncode=result.returncode
            )
        return result
