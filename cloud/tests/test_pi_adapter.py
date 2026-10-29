"""DPES-813 pi headless adapter: argv, environment, JSON-mode verdicts, and the bootstrap port.

Everything runs against a fake pi process, a Python script that prints scripted JSON events. No
network, no model, and no bubblewrap run here. Real pi, Vertex, and bubblewrap proof are stated gaps
naming DPES-804, DPES-819, and DPES-871.
"""

import io
import json
import re
import subprocess
import sys
import tarfile
from pathlib import Path
from typing import get_args

import pytest

from flows import LONG_BUDGET, admit_and_start, dispatch, make_world, source_tip
from mao_cloud.agents import pi
from mao_cloud.protocol import MAX_LOG_LINE_CHARS, Thinking
from mao_cloud.replay import TOOL_REPLAY_CLASSES
from mao_cloud.trusted_bootstrap import Ports, run_attempt
from mao_cloud.trusted_bundle import SOURCE_MEMBER
from mao_cloud.trusted_sandbox import bwrap_argv, worker_env
from mao_cloud.trusted_supervise import ChildOutcome, ChildResult, SupervisorTiming

CLOUD = Path(__file__).resolve().parents[1]
DOCKERFILE = CLOUD / "Dockerfile"
MODELS_JSON = CLOUD / "config" / "pi" / "models.json"

TASK = "Fix the failing parser test and run the suite."
PI_FLAGS = [
    "--mode",
    "json",
    "--provider",
    "google-vertex",
    "--model",
    "gemini-3.8-flash",
    "--thinking",
    "medium",
    "--no-session",
    "--no-extensions",
    "--no-mcp",
    "--no-skills",
    "--no-prompt-templates",
    "--no-themes",
    "--no-context-files",
    "--approve",
    "--tools",
    "read,write,edit,bash",
]
FAST = SupervisorTiming(
    heartbeat_seconds=0.05,
    checkpoint_min_interval_seconds=60.0,
    flush_grace_seconds=0.5,
)
CREDENTIAL_NAME = re.compile(
    r"KEY|SECRET|TOKEN|PASSWORD|CREDENTIAL|GOOGLE_", re.IGNORECASE
)
GIT_ENV = {
    "PATH": "/usr/bin:/bin",
    "HOME": "/nonexistent",
    "GIT_CONFIG_NOSYSTEM": "1",
    "GIT_CONFIG_GLOBAL": "/dev/null",
    "GIT_AUTHOR_NAME": "t",
    "GIT_AUTHOR_EMAIL": "t@example.invalid",
    "GIT_COMMITTER_NAME": "t",
    "GIT_COMMITTER_EMAIL": "t@example.invalid",
}
PROBE_OK = (0, "ROOT_READONLY\nWORKSPACE_WRITABLE\n")


def _no_sandbox(workspace: str, command: list[str]) -> list[str]:
    return command


def _assistant_end(
    stop: str,
    *,
    error: str | None = None,
    usage: dict | None = None,
    padding: str = "",
) -> str:
    message: dict = {
        "role": "assistant",
        "content": padding,
        "stopReason": stop,
        "model": "gemini-3.8-flash",
        "provider": "google-vertex",
    }
    if error is not None:
        message["errorMessage"] = error
    if usage is not None:
        message["usage"] = usage
    return json.dumps({"type": "message_end", "message": message})


def _tool(kind: str, name: str, *, is_error: bool = False) -> str:
    event: dict = {
        "type": f"tool_execution_{kind}",
        "toolCallId": f"{name}-1",
        "toolName": name,
    }
    if kind == "end":
        event["isError"] = is_error
    return json.dumps(event)


def _fake_pi(
    tmp_path: Path,
    lines: list[str],
    *,
    name: str = "fake-pi",
    exit_code: int = 0,
    hang: bool = False,
) -> tuple[list[str], Path]:
    """A fake pi: records its argv and environment, prints lines, then exits or hangs."""
    script = tmp_path / f"{name}.py"
    record = tmp_path / f"{name}.record.json"
    body = (
        "import json, os, sys, time\n"
        f"with open({str(record)!r}, 'w') as fh:\n"
        "    json.dump({'argv': sys.argv[1:], 'env': dict(os.environ)}, fh)\n"
        f"for line in {lines!r}:\n"
        "    sys.stdout.write(line + '\\n')\n"
        "sys.stdout.flush()\n"
        + ("time.sleep(60)\n" if hang else "")
        + f"sys.exit({exit_code})\n"
    )
    script.write_text(body, encoding="utf-8")
    return [sys.executable, str(script)], record


def _worker(
    program: list[str], *, log: list[str] | None = None, **kwargs
) -> pi.PiWorker:
    options: dict = {"wrap": _no_sandbox, "timing": FAST}
    options.update(kwargs)
    return pi.PiWorker(
        task=TASK,
        model="gemini-3.8-flash",
        thinking="medium",
        executable=program,
        log_sink=log.append if log is not None else None,
        **options,
    )


def _run(
    tmp_path: Path,
    lines: list[str],
    *,
    exit_code: int = 0,
    hang: bool = False,
    max_runtime: float = 30.0,
    log: list[str] | None = None,
) -> tuple[ChildResult, pi.PiWorker, Path]:
    program, record = _fake_pi(tmp_path, lines, exit_code=exit_code, hang=hang)
    worker = _worker(program, log=log)
    result = worker(tmp_path, max_runtime)
    return result, worker, record


def _judge(*lines: str) -> pi.PiVerdict:
    return pi.judge_stream(lines)


# --- argv and task input -------------------------------------------------------------------


def test_the_argv_is_the_approved_pi_contract_with_the_task_last() -> None:
    assert pi.pi_args(model="gemini-3.8-flash", thinking="medium", task=TASK) == [
        *PI_FLAGS,
        "--",
        TASK,
    ]


@pytest.mark.parametrize("thinking", list(get_args(Thinking)))
def test_every_allowlisted_thinking_level_reaches_the_argv(thinking: str) -> None:
    args = pi.pi_args(model="gemini-3.8-flash", thinking=thinking, task=TASK)
    assert args[args.index("--thinking") + 1] == thinking


@pytest.mark.parametrize("model", ["gemini-2.5-pro", "gemini-3.8-flash-001", ""])
def test_a_model_outside_the_vertex_allowlist_is_refused(model: str) -> None:
    with pytest.raises(ValueError):
        pi.pi_args(model=model, thinking="medium", task=TASK)


@pytest.mark.parametrize("thinking", ["xhigh", "max", "none", ""])
def test_a_thinking_level_outside_the_allowlist_is_refused(thinking: str) -> None:
    with pytest.raises(ValueError):
        pi.pi_args(model="gemini-3.8-flash", thinking=thinking, task=TASK)


@pytest.mark.parametrize(
    "task", ["--tools=bash,grep", "-x", "--", "@README.md", "@/etc/passwd", "@", "", "fix\x00this"]
)
def test_a_task_with_reserved_pi_syntax_or_empty_or_binary_text_is_refused(
    task: str,
) -> None:
    with pytest.raises(ValueError):
        pi.pi_args(model="gemini-3.8-flash", thinking="medium", task=task)


def test_an_oversized_task_is_refused_before_it_reaches_argv() -> None:
    with pytest.raises(ValueError):
        pi.pi_args(
            model="gemini-3.8-flash",
            thinking="medium",
            task="x" * (pi.MAX_TASK_BYTES + 1),
        )


def test_the_tool_allowlist_is_exactly_the_replay_classified_builtins() -> None:
    assert set(pi.PI_TOOLS) == set(TOOL_REPLAY_CLASSES)
    assert ",".join(pi.PI_TOOLS) == "read,write,edit,bash"


# --- version, environment, and config ------------------------------------------------------


def test_the_pi_version_is_the_version_the_image_installs() -> None:
    assert pi.PI_VERSION == "1.3.0"
    assert f"@earendil-works/pi-coding-agent@{pi.PI_VERSION}" in DOCKERFILE.read_text(
        encoding="utf-8"
    )


def test_the_worker_environment_is_offline_and_holds_no_model_credentials() -> None:
    env = worker_env()
    assert {
        "PI_OFFLINE": "1",
        "PI_SKIP_VERSION_CHECK": "1",
        "PI_TELEMETRY": "0",
    }.items() <= (env.items())
    assert [name for name in env if CREDENTIAL_NAME.search(name)] == []


def test_models_json_routes_google_vertex_through_the_default_credential_chain() -> (
    None
):
    manifest = json.loads(MODELS_JSON.read_text(encoding="utf-8"))
    assert manifest == {
        "providers": {"google-vertex": {"apiKey": "gcp-vertex-credentials"}}
    }


# --- stream verdicts (pure) ----------------------------------------------------------------


def test_a_clean_stop_with_allowed_tools_succeeds_and_keeps_the_effective_model() -> (
    None
):
    verdict = _judge(
        json.dumps(
            {"type": "message_end", "message": {"role": "user", "content": TASK}}
        ),
        _tool("start", "read"),
        _tool("end", "read"),
        _assistant_end("stop", usage={"input": 120, "output": 45}),
    )
    assert verdict.status == "succeeded"
    assert verdict.reason == pi.PiReason.COMPLETED
    assert verdict.effective_model == "gemini-3.8-flash"
    assert verdict.effective_provider == "google-vertex"
    assert (verdict.tool_calls, verdict.tool_errors) == (1, 0)
    assert (verdict.input_tokens, verdict.output_tokens) == (120, 45)


def test_usage_is_none_when_the_stream_reports_none() -> None:
    verdict = _judge(_assistant_end("stop"))
    assert (verdict.input_tokens, verdict.output_tokens) == (None, None)


def test_the_last_assistant_message_decides_not_an_earlier_error() -> None:
    verdict = _judge(
        _assistant_end("error", error="429 Too Many Requests"), _assistant_end("stop")
    )
    assert verdict.status == "succeeded"


@pytest.mark.parametrize(
    "message",
    [
        "PERMISSION_DENIED: Permission 'aiplatform.endpoints.predict' denied",
        "401 Unauthorized",
        "UNAUTHENTICATED: request had invalid authentication credentials",
        "Could not load the default credentials. Browse to https://cloud.google.com",
    ],
)
def test_vertex_authentication_failures_are_classified_as_auth(message: str) -> None:
    verdict = _judge(_assistant_end("error", error=message))
    assert verdict.status == "failed"
    assert verdict.reason == pi.PiReason.AUTH_FAILED


@pytest.mark.parametrize(
    "message",
    [
        "429 Too Many Requests",
        "RESOURCE_EXHAUSTED: Quota exceeded for aiplatform.googleapis.com",
        "Rate limit reached for requests",
    ],
)
def test_vertex_throttling_is_classified_as_throttled(message: str) -> None:
    verdict = _judge(_assistant_end("error", error=message))
    assert verdict.status == "failed"
    assert verdict.reason == pi.PiReason.THROTTLED


def test_other_model_errors_are_model_error_and_the_raw_text_never_leaves_the_judge() -> (
    None
):
    verdict = _judge(_assistant_end("error", error="internal upstream failure xyzzy"))
    assert verdict.status == "failed"
    assert verdict.reason == pi.PiReason.MODEL_ERROR
    assert "xyzzy" not in repr(verdict)


@pytest.mark.parametrize(
    ("stop", "reason"),
    [
        ("length", pi.PiReason.OUTPUT_TRUNCATED),
        ("aborted", pi.PiReason.ABORTED),
        ("toolUse", pi.PiReason.INCOMPLETE_TURN),
    ],
)
def test_turns_that_did_not_stop_cleanly_are_failed(
    stop: str, reason: pi.PiReason
) -> None:
    verdict = _judge(_assistant_end(stop))
    assert verdict.status == "failed"
    assert verdict.reason == reason


def test_a_stream_with_no_assistant_terminal_message_is_failed() -> None:
    verdict = _judge(_tool("start", "read"), _tool("end", "read"))
    assert verdict.status == "failed"
    assert verdict.reason == pi.PiReason.NO_TERMINAL_MESSAGE


@pytest.mark.parametrize("name", list(pi.PI_TOOLS))
def test_every_allowlisted_tool_is_permitted(name: str) -> None:
    verdict = _judge(_tool("start", name), _tool("end", name), _assistant_end("stop"))
    assert verdict.status == "succeeded"


def test_a_tool_outside_the_allowlist_is_a_policy_violation_even_when_the_turn_stops_cleanly() -> (
    None
):
    verdict = _judge(
        _tool("start", "grep"), _tool("end", "grep"), _assistant_end("stop")
    )
    assert verdict.status == "failed"
    assert verdict.reason == pi.PiReason.TOOL_POLICY_VIOLATION


@pytest.mark.parametrize(
    "line",
    [
        "not json at all",
        "[1, 2]",
        '{"no": "type"}',
        '{"type": 7}',
        '{"type": "message_end", "message": ',
        json.dumps({"type": "tool_execution_start"}),
        "[" * 100_000,
    ],
)
def test_a_malformed_line_is_a_malformed_stream_even_when_the_turn_stops_cleanly(
    line: str,
) -> None:
    verdict = _judge(line, _assistant_end("stop"))
    assert verdict.status == "failed"
    assert verdict.reason == pi.PiReason.MALFORMED_STREAM


def test_a_last_shell_command_that_failed_is_not_reported_as_success() -> None:
    verdict = _judge(
        _tool("start", "bash"),
        _tool("end", "bash", is_error=True),
        _assistant_end("stop"),
    )
    assert verdict.status == "failed"
    assert verdict.reason == pi.PiReason.LAST_SHELL_COMMAND_FAILED
    assert verdict.tool_errors == 1


def test_a_shell_command_that_failed_and_was_then_followed_by_a_passing_one_succeeds() -> (
    None
):
    verdict = _judge(
        _tool("start", "bash"),
        _tool("end", "bash", is_error=True),
        _tool("start", "bash"),
        _tool("end", "bash"),
        _assistant_end("stop"),
    )
    assert verdict.status == "succeeded"
    assert (verdict.tool_calls, verdict.tool_errors) == (2, 1)


# --- the worker port under supervision (fake pi process) -----------------------------------


# The fake pi is a Python process. On macOS, CoreFoundation adds __CF_USER_TEXT_ENCODING to every
# interpreter's environment at startup. The production sandbox clears the environment entirely
# (bwrap --clearenv), so only that runtime-injected key may appear beyond worker_env().
_INTERPRETER_INJECTED = {"__CF_USER_TEXT_ENCODING"}


def test_the_child_receives_exactly_the_pi_argv_and_the_fixed_environment(
    tmp_path: Path,
) -> None:
    _, _, record = _run(tmp_path, [_assistant_end("stop")])
    seen = json.loads(record.read_text(encoding="utf-8"))
    assert seen["argv"] == [*PI_FLAGS, "--", TASK]
    fixed = worker_env()
    assert {key: seen["env"][key] for key in fixed} == fixed
    assert set(seen["env"]) - set(fixed) <= _INTERPRETER_INJECTED


def test_a_clean_run_is_completed_and_its_verdict_is_recorded(tmp_path: Path) -> None:
    result, worker, _ = _run(tmp_path, [_assistant_end("stop")])
    assert result == ChildResult(ChildOutcome.COMPLETED, 0)
    assert worker.verdict is not None
    assert worker.verdict.status == "succeeded"


def test_a_zero_exit_with_a_failed_verdict_is_failed_not_completed(
    tmp_path: Path,
) -> None:
    result, worker, _ = _run(
        tmp_path,
        [_assistant_end("error", error="PERMISSION_DENIED: denied")],
    )
    assert result == ChildResult(ChildOutcome.FAILED, 0)
    assert worker.verdict is not None
    assert worker.verdict.reason == pi.PiReason.AUTH_FAILED


def test_a_nonzero_exit_is_failed_even_when_the_stream_looks_clean(
    tmp_path: Path,
) -> None:
    result, _, _ = _run(tmp_path, [_assistant_end("stop")], exit_code=3)
    assert result == ChildResult(ChildOutcome.FAILED, 3)


def test_the_deadline_stops_a_hung_worker_and_reports_timed_out(tmp_path: Path) -> None:
    result, _, _ = _run(tmp_path, [], hang=True, max_runtime=0.5)
    assert result.outcome is ChildOutcome.TIMED_OUT
    assert result.outcome is not ChildOutcome.COMPLETED


def test_a_long_event_line_arrives_whole_so_its_stop_reason_is_judged(
    tmp_path: Path,
) -> None:
    result, worker, _ = _run(
        tmp_path,
        [_assistant_end("stop", padding="x" * 20000)],
    )
    assert result == ChildResult(ChildOutcome.COMPLETED, 0)
    assert worker.verdict is not None
    assert worker.verdict.reason == pi.PiReason.COMPLETED


def test_an_event_line_beyond_the_event_limit_fails_closed_as_malformed(
    tmp_path: Path,
) -> None:
    result, worker, _ = _run(
        tmp_path,
        [_assistant_end("stop", padding="x" * pi.MAX_PI_EVENT_BYTES)],
    )
    assert result == ChildResult(ChildOutcome.FAILED, 0)
    assert worker.verdict is not None
    assert worker.verdict.reason == pi.PiReason.MALFORMED_STREAM


def test_the_log_is_redacted_and_capped_and_the_raw_event_is_not_logged(
    tmp_path: Path,
) -> None:
    log: list[str] = []
    _run(
        tmp_path,
        [
            "-----BEGIN PRIVATE KEY-----",
            "SECRETBODY",
            "-----END PRIVATE KEY-----",
            _assistant_end("stop", padding="y" * 9000),
        ],
        log=log,
    )
    assert log
    assert all(len(line) <= MAX_LOG_LINE_CHARS for line in log)
    assert "SECRETBODY" not in "\n".join(log)


def test_the_log_carries_only_pi_event_records_never_model_or_tool_text(
    tmp_path: Path,
) -> None:
    log: list[str] = []
    _run(
        tmp_path,
        [_tool("start", "bash"), _assistant_end("stop", padding="MODEL-TEXT-MARKER")],
        log=log,
    )
    joined = "\n".join(log)
    assert "pi message_end" in joined
    assert "pi tool_execution_start tool=bash" in joined
    assert "MODEL-TEXT-MARKER" not in joined


def test_the_worker_runs_under_bubblewrap_with_the_pi_command(tmp_path: Path) -> None:
    captured: dict = {}

    def fake_supervise(argv, **kwargs) -> ChildResult:
        captured["argv"] = argv
        captured["env"] = kwargs["env"]
        return ChildResult(ChildOutcome.FAILED, 1)

    worker = pi.PiWorker(
        task=TASK,
        model="gemini-3.8-flash",
        thinking="medium",
        executable=["pi"],
        timing=FAST,
        run=fake_supervise,
    )
    worker(tmp_path, 30.0)
    assert captured["argv"][0] == "bwrap"
    assert captured["argv"] == bwrap_argv(str(tmp_path), ["pi", *PI_FLAGS, "--", TASK])
    assert captured["env"] == worker_env()


# --- the port plugged into the trusted bootstrap ---------------------------------------------


def _source_archive(tmp_path: Path) -> bytes:
    repo = tmp_path / "repo"
    repo.mkdir()
    subprocess.run(
        ["git", "init", "-q", "-b", "main", str(repo)], env=GIT_ENV, check=True
    )
    (repo / "hello.txt").write_text("hi\n", encoding="utf-8")
    subprocess.run(["git", "-C", str(repo), "add", "-A"], env=GIT_ENV, check=True)
    subprocess.run(
        [
            "git",
            "-C",
            str(repo),
            "-c",
            "commit.gpgsign=false",
            "commit",
            "-q",
            "-m",
            "init",
        ],
        env=GIT_ENV,
        check=True,
    )
    bundle_path = tmp_path / "made.bundle"
    subprocess.run(
        ["git", "-C", str(repo), "bundle", "create", str(bundle_path), "HEAD"],
        env=GIT_ENV,
        check=True,
        capture_output=True,
    )
    data = bundle_path.read_bytes()
    buffer = io.BytesIO()
    with tarfile.open(fileobj=buffer, mode="w") as archive:
        info = tarfile.TarInfo(SOURCE_MEMBER)
        info.size = len(data)
        archive.addfile(info, io.BytesIO(data))
    return buffer.getvalue()


class _Store:
    def create(self, name: str, body: bytes, headers) -> int:
        return 1

    def read(self, name: str) -> bytes | None:
        return None


def _published_outcome(tmp_path: Path, lines: list[str]) -> dict:
    source = _source_archive(tmp_path)
    world = make_world()
    run_id, _, receipt = admit_and_start(
        world, source=source, base_sha=source_tip(source), budget=LONG_BUDGET
    )
    bootstrap = dispatch(world, run_id)
    program, _ = _fake_pi(tmp_path, lines)
    published: list[bytes] = []
    ports = Ports(
        claim_store=_Store(),
        read_source=lambda _source: source,
        probe_sandbox=lambda argv: PROBE_OK,
        run_worker=_worker(program),
        publish_result=published.append,
    )
    intake = tmp_path / "intake"
    intake.mkdir()
    result = run_attempt(
        bootstrap,
        receipt,
        ports=ports,
        runner_nonce="1f" * 16,
        now=world.clock.now(),
        intake_root=intake,
        clock=world.clock.now,
    )
    assert result.status == "reported"
    assert len(published) == 1
    return json.loads(published[0])


def test_a_clean_pi_run_publishes_one_succeeded_result_through_the_bootstrap(
    tmp_path: Path,
) -> None:
    document = _published_outcome(
        tmp_path, [_tool("start", "read"), _assistant_end("stop")]
    )
    assert document["outcome"] == "succeeded"
    assert document["exit_code"] == 0


def test_a_policy_violating_pi_run_publishes_failed_through_the_bootstrap(
    tmp_path: Path,
) -> None:
    document = _published_outcome(
        tmp_path, [_tool("start", "grep"), _assistant_end("stop")]
    )
    assert document["outcome"] == "failed"


def test_a_truncated_valid_json_prefix_cannot_be_a_successful_event(tmp_path: Path) -> None:
    prefix = _assistant_end("stop")
    malformed = prefix + " " * (pi.MAX_PI_EVENT_BYTES - len(prefix.encode("utf-8"))) + "invalid"
    log: list[str] = []
    result, worker, _ = _run(tmp_path, [malformed, _assistant_end("stop")], log=log)

    assert result == ChildResult(ChildOutcome.FAILED, 0)
    assert worker.verdict is not None
    assert worker.verdict.reason is pi.PiReason.MALFORMED_STREAM
    assert log == ["pi message_end"]


def test_explicit_truncation_is_rejected_even_when_the_prefix_is_valid_json() -> None:
    judge = pi.PiStreamJudge()
    judge.feed(_assistant_end("stop"), truncated=True)
    judge.feed(_assistant_end("stop"))
    assert judge.verdict().reason is pi.PiReason.MALFORMED_STREAM


@pytest.mark.parametrize(
    "text",
    [
        "token=placeholder password=hunter2",
        "-----BEGIN PRIVATE KEY-----\nSECRETBODY\n-----END PRIVATE KEY-----",
    ],
)
def test_raw_model_and_tool_payloads_are_judged_without_redaction_but_never_logged(
    tmp_path: Path, text: str
) -> None:
    tool_end = json.loads(_tool("end", "read"))
    tool_end["result"] = {"content": text}
    log: list[str] = []
    result, worker, _ = _run(
        tmp_path,
        [_tool("start", "read"), json.dumps(tool_end), _assistant_end("stop", padding=text)],
        log=log,
    )

    assert result == ChildResult(ChildOutcome.COMPLETED, 0)
    assert worker.verdict is not None
    assert worker.verdict.reason is pi.PiReason.COMPLETED
    assert worker.verdict.tool_calls == 1
    assert log == [
        "pi tool_execution_start tool=read",
        "pi tool_execution_end tool=read error=false",
        "pi message_end",
    ]
    assert "placeholder" not in "\n".join(log)
    assert "hunter2" not in "\n".join(log)
    assert "SECRETBODY" not in "\n".join(log)
