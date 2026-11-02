"""Trusted bootstrap: the attempt's order of operations, refusals, and exit codes.

DPES-811 AC2, AC3, AC5. The ports here are fakes that record every call, so these tests prove
ordering and refusal logic: a 412 never reaches the source, sandbox, or worker. The production
ports (signed GCS, real bubblewrap, Vertex) are not built, and main fails closed on them.
"""

import io
import shutil
import subprocess
import tarfile
from datetime import timedelta
from dataclasses import replace
from pathlib import Path

import pytest

from flows import LONG_BUDGET, admit_and_start, dispatch, make_world, source_tip
from mao_cloud import trusted_bootstrap
from mao_cloud.protocol import ClaimDocument, RunResultDocument
from mao_cloud.trusted_bootstrap import (
    EXIT_REFUSED,
    EXIT_REPORTED,
    EXIT_UNAVAILABLE,
    EXIT_UNREADABLE,
    Ports,
    main,
    run_attempt,
)
from mao_cloud.trusted_bundle import SOURCE_MEMBER
from mao_cloud.trusted_claim import ClaimConflict
from mao_cloud.trusted_supervise import ChildOutcome, ChildResult

pytestmark = pytest.mark.skipif(shutil.which("git") is None, reason="git is required")

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
PROBE_ROOT_WRITABLE = (0, "ROOT_WRITABLE\nWORKSPACE_WRITABLE\n")


def _source_archive(tmp_path: Path) -> bytes:
    """A real plain tar holding one source.bundle of a real one-commit repository."""
    repo = tmp_path / "repo"
    repo.mkdir()
    subprocess.run(["git", "init", "-q", "-b", "main", str(repo)], env=GIT_ENV, check=True)
    (repo / "hello.txt").write_text("hi\n", encoding="utf-8")
    subprocess.run(["git", "-C", str(repo), "add", "-A"], env=GIT_ENV, check=True)
    subprocess.run(
        ["git", "-C", str(repo), "-c", "commit.gpgsign=false", "commit", "-q", "-m", "init"],
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


def _started(source: bytes, *, base_sha: str | None = None):
    """A real world with the run dispatched: returns (world, bootstrap, receipt).

    The claimed base defaults to the archive's real tip, as a controller exporting it would claim.
    """
    world = make_world()
    claimed = base_sha or source_tip(source)
    run_id, _, receipt = admit_and_start(
        world, source=source, base_sha=claimed, budget=LONG_BUDGET
    )
    bootstrap = dispatch(world, run_id)
    return world, bootstrap, receipt


class Recorder:
    """Ports that record each call in order, and answer from scripted values."""

    def __init__(
        self,
        *,
        source: bytes,
        source_error: Exception | None = None,
        claim_error: Exception | None = None,
        probe: tuple[int, str] = PROBE_OK,
        child: ChildResult | None = None,
    ) -> None:
        self.events: list[str] = []
        self.published: list[bytes] = []
        self.claimed: list[bytes] = []
        self._source = source
        self._source_error = source_error
        self._claim_error = claim_error
        self._probe = probe
        self._child = child or ChildResult(ChildOutcome.COMPLETED, 0)

    def ports(self) -> Ports:
        recorder = self

        class Store:
            def create(self, name: str, body: bytes, headers) -> int:
                recorder.events.append("claim")
                recorder.claimed.append(body)
                if recorder._claim_error is not None:
                    raise recorder._claim_error
                return 1

            def read(self, name: str) -> bytes | None:
                return None

        def read_source(source) -> bytes:
            recorder.events.append("source")
            if recorder._source_error is not None:
                raise recorder._source_error
            return recorder._source

        def probe(argv: list[str]) -> tuple[int, str]:
            recorder.events.append("probe")
            return recorder._probe

        def run_worker(workspace: Path, max_runtime_seconds: int) -> ChildResult:
            recorder.events.append("worker")
            return recorder._child

        def publish(body: bytes) -> None:
            recorder.events.append("publish")
            recorder.published.append(body)

        return Ports(
            claim_store=Store(),
            read_source=read_source,
            probe_sandbox=probe,
            run_worker=run_worker,
            publish_result=publish,
        )


def _attempt(tmp_path: Path, recorder: Recorder, world, bootstrap, receipt, *, now=None):
    intake = tmp_path / "intake"
    intake.mkdir(exist_ok=True)
    return run_attempt(
        bootstrap,
        receipt,
        ports=recorder.ports(),
        runner_nonce="1f" * 16,
        now=now or world.clock.now(),
        intake_root=intake,
        clock=world.clock.now,
    )


# --- the attempt's order of operations ------------------------------------------------------


def test_a_reported_attempt_runs_claim_then_source_then_sandbox_then_worker_then_publish(
    tmp_path: Path,
) -> None:
    source = _source_archive(tmp_path)
    world, bootstrap, receipt = _started(source)
    recorder = Recorder(source=source)

    outcome = _attempt(tmp_path, recorder, world, bootstrap, receipt)

    assert outcome.status == "reported"
    assert recorder.events == ["claim", "source", "probe", "worker", "publish"]
    result = RunResultDocument.model_validate_json(recorder.published[0])
    assert result.outcome == "succeeded"
    assert (result.run_id, result.attempt, result.fence) == (
        bootstrap.run_id,
        bootstrap.attempt,
        bootstrap.fence,
    )
    assert result.claim_id == bootstrap.claim_id


def test_the_claim_written_is_the_planned_claim_for_this_attempt(tmp_path: Path) -> None:
    source = _source_archive(tmp_path)
    world, bootstrap, receipt = _started(source)
    recorder = Recorder(source=source)

    _attempt(tmp_path, recorder, world, bootstrap, receipt)

    claim = ClaimDocument.model_validate_json(recorder.claimed[0])
    assert (claim.run_id, claim.attempt, claim.fence) == (
        bootstrap.run_id,
        bootstrap.attempt,
        bootstrap.fence,
    )
    assert claim.runner_nonce == "1f" * 16


# --- refusals: nothing after the refusal runs -----------------------------------------------


def test_a_412_claim_refuses_and_never_reaches_the_source_sandbox_or_worker(
    tmp_path: Path,
) -> None:
    source = _source_archive(tmp_path)
    world, bootstrap, receipt = _started(source)
    recorder = Recorder(source=source, claim_error=ClaimConflict("exists"))

    outcome = _attempt(tmp_path, recorder, world, bootstrap, receipt)

    assert outcome.status == "refused"
    assert recorder.events == ["claim"]
    assert recorder.published == []


def test_a_source_that_fails_its_checksum_refuses_before_any_repository_code_runs(
    tmp_path: Path,
) -> None:
    source = _source_archive(tmp_path)
    world, bootstrap, receipt = _started(source)
    recorder = Recorder(source=b"not the uploaded archive")

    outcome = _attempt(tmp_path, recorder, world, bootstrap, receipt)

    assert outcome.status == "refused"
    assert recorder.events == ["claim", "source"]
    assert recorder.published == []


def test_a_bundle_tip_that_is_not_the_claimed_base_refuses_before_the_worker_runs(
    tmp_path: Path,
) -> None:
    """DPES-811 AC3: the worker verifies the bundle tip against the base the controller claimed."""
    source = _source_archive(tmp_path)
    world, bootstrap, receipt = _started(source, base_sha="c" * 40)
    recorder = Recorder(source=source)

    outcome = _attempt(tmp_path, recorder, world, bootstrap, receipt)

    assert outcome.status == "refused"
    assert recorder.events == ["claim", "source"]
    assert recorder.published == []


def test_an_unreadable_source_refuses_after_the_claim_and_publishes_nothing(
    tmp_path: Path,
) -> None:
    source = _source_archive(tmp_path)
    world, bootstrap, receipt = _started(source)
    recorder = Recorder(source=source, source_error=OSError("source object is gone"))

    outcome = _attempt(tmp_path, recorder, world, bootstrap, receipt)

    assert outcome.status == "refused"
    assert recorder.events == ["claim", "source"]
    assert recorder.published == []


def test_a_sandbox_that_cannot_be_qualified_refuses_before_the_worker_runs(
    tmp_path: Path,
) -> None:
    source = _source_archive(tmp_path)
    world, bootstrap, receipt = _started(source)
    recorder = Recorder(source=source, probe=PROBE_ROOT_WRITABLE)

    outcome = _attempt(tmp_path, recorder, world, bootstrap, receipt)

    assert outcome.status == "refused"
    assert recorder.events == ["claim", "source", "probe"]
    assert recorder.published == []


def test_a_deadline_that_has_passed_refuses_before_the_claim(tmp_path: Path) -> None:
    source = _source_archive(tmp_path)
    world, bootstrap, receipt = _started(source)
    recorder = Recorder(source=source)
    late = bootstrap.deadline_at + timedelta(seconds=1)

    outcome = _attempt(tmp_path, recorder, world, bootstrap, receipt, now=late)

    assert outcome.status == "refused"
    assert recorder.events == []


# --- results: a worker that did not complete cleanly is never published as success ----------


@pytest.mark.parametrize(
    ("child", "expected"),
    [
        (ChildResult(ChildOutcome.COMPLETED, 0), "succeeded"),
        (ChildResult(ChildOutcome.FAILED, 3), "failed"),
        (ChildResult(ChildOutcome.KILLED, -9), "failed"),
        (ChildResult(ChildOutcome.TIMED_OUT, 0), "failed"),
    ],
)
def test_only_a_clean_completion_is_published_as_succeeded(
    tmp_path: Path, child: ChildResult, expected: str
) -> None:
    source = _source_archive(tmp_path)
    world, bootstrap, receipt = _started(source)
    recorder = Recorder(source=source, child=child)

    _attempt(tmp_path, recorder, world, bootstrap, receipt)

    assert RunResultDocument.model_validate_json(recorder.published[0]).outcome == expected


# --- main: exit codes and the fail-closed production gap -------------------------------------


def test_exit_codes_are_distinct_and_fixed() -> None:
    assert (EXIT_REPORTED, EXIT_REFUSED, EXIT_UNREADABLE, EXIT_UNAVAILABLE) == (0, 1, 2, 3)


def test_unreadable_or_malformed_input_exits_two(tmp_path: Path) -> None:
    missing = tmp_path / "missing.json"
    assert main(["--bootstrap", str(missing), "--receipt", str(missing)]) == EXIT_UNREADABLE

    malformed = tmp_path / "malformed.json"
    malformed.write_text("{not json", encoding="utf-8")
    assert main(["--bootstrap", str(malformed), "--receipt", str(malformed)]) == EXIT_UNREADABLE


def test_main_runs_the_attempt_in_order_through_injected_ports_and_exits_reported(
    tmp_path: Path,
) -> None:
    source = _source_archive(tmp_path)
    world, bootstrap, receipt = _started(source)
    bootstrap_file = tmp_path / "bootstrap.json"
    receipt_file = tmp_path / "receipt.json"
    bootstrap_file.write_text(bootstrap.model_dump_json(), encoding="utf-8")
    receipt_file.write_text(receipt.model_dump_json(), encoding="utf-8")
    recorder = Recorder(source=source)

    code = main(
        [
            "--bootstrap",
            str(bootstrap_file),
            "--receipt",
            str(receipt_file),
            "--intake-root",
            str(tmp_path / "intake"),
        ],
        ports_factory=recorder.ports,
        clock=world.clock.now,
    )

    assert code == EXIT_REPORTED
    assert recorder.events == ["claim", "source", "probe", "worker", "publish"]


def test_absent_production_ports_fail_closed_with_exit_three_and_name_the_gap(
    tmp_path: Path, capsys
) -> None:
    source = _source_archive(tmp_path)
    world, bootstrap, receipt = _started(source)
    bootstrap_file = tmp_path / "bootstrap.json"
    receipt_file = tmp_path / "receipt.json"
    bootstrap_file.write_text(bootstrap.model_dump_json(), encoding="utf-8")
    receipt_file.write_text(receipt.model_dump_json(), encoding="utf-8")

    code = main(["--bootstrap", str(bootstrap_file), "--receipt", str(receipt_file)])

    assert code == EXIT_UNAVAILABLE
    err = capsys.readouterr().err
    assert "not built" in err
    assert "DPES-807" in err


def test_main_without_an_injected_clock_rereads_utc_after_intake(tmp_path: Path, monkeypatch) -> None:
    source = _source_archive(tmp_path)
    world, bootstrap, receipt = _started(source)
    bootstrap_file = tmp_path / "bootstrap.json"
    receipt_file = tmp_path / "receipt.json"
    bootstrap_file.write_text(bootstrap.model_dump_json(), encoding="utf-8")
    receipt_file.write_text(receipt.model_dump_json(), encoding="utf-8")
    recorder = Recorder(source=source)
    instants = iter([world.clock.now(), bootstrap.deadline_at + timedelta(seconds=1)])
    monkeypatch.setattr(trusted_bootstrap, "_utc_now", lambda: next(instants))

    code = main(
        ["--bootstrap", str(bootstrap_file), "--receipt", str(receipt_file),
         "--intake-root", str(tmp_path / "intake")],
        ports_factory=recorder.ports,
    )
    assert code == EXIT_REFUSED
    assert recorder.events == ["claim", "source", "probe"]
    assert recorder.published == []


def test_preparation_time_is_deducted_from_the_worker_budget(tmp_path: Path) -> None:
    source = _source_archive(tmp_path)
    world, bootstrap, receipt = _started(source)
    recorder = Recorder(source=source)
    ports = recorder.ports()
    original_read = ports.read_source
    budgets: list[float] = []

    def delayed_source(spec):
        data = original_read(spec)
        world.clock.advance(seconds=23)
        return data

    def worker(workspace: Path, remaining: float) -> ChildResult:
        budgets.append(remaining)
        return ChildResult(ChildOutcome.COMPLETED, 0)

    outcome = run_attempt(
        bootstrap, receipt, ports=replace(ports, read_source=delayed_source, run_worker=worker),
        runner_nonce="1f" * 16, now=world.clock.now(), intake_root=tmp_path / "intake",
        clock=world.clock.now,
    )
    assert outcome.status == "reported"
    assert budgets == [bootstrap.max_runtime_seconds - 23]
