"""Trusted bootstrap: one attempt's steps in order, its refusals, and its exit code.

DPES-811 AC2, AC3, AC5. Order: plan the claim (deadline and identity), take the create-only claim,
read and verify the source, materialize the checkout, qualify the sandbox, run the worker under
supervision, publish one result. Each refusal stops before the next step, so a refused attempt never
runs repository or model code and publishes nothing.

Effects arrive through Ports. Production adapters are not built: production_ports() raises until
signed object storage (DPES-807), the real sandbox (DPES-804), and model access (DPES-819) exist,
so main fails closed rather than simulating them.
"""

import argparse
import subprocess
import sys
from collections.abc import Callable
from dataclasses import dataclass
from datetime import UTC, datetime
from pathlib import Path
from typing import Literal

from pydantic import ValidationError

from mao_cloud.protocol import (
    AcceptedReceipt,
    BootstrapSource,
    ProtocolError,
    WorkerBootstrap,
)
from mao_cloud.runner import plan_claim, plan_result
from mao_cloud.trusted_bundle import (
    BundleLimits,
    BundleRefused,
    extract_source,
    materialize_checkout,
)
from mao_cloud.trusted_claim import (
    ClaimRefused,
    ClaimStore,
    claim_attempt,
    new_runner_nonce,
)
from mao_cloud.trusted_sandbox import SandboxUnavailable, qualify_sandbox
from mao_cloud.trusted_supervise import ChildOutcome, ChildResult

EXIT_REPORTED = 0
EXIT_REFUSED = 1
EXIT_UNREADABLE = 2
EXIT_UNAVAILABLE = 3

DEFAULT_INTAKE_ROOT = Path("/var/mao/intake")
WORKSPACE_DIR = "workspace"


class ProductionPortsUnavailable(Exception):
    """The production adapters are not built. The message names the ticket that builds each."""


@dataclass(frozen=True)
class Ports:
    """The effects an attempt needs. Each is injected so the order and refusals are testable."""

    claim_store: ClaimStore
    read_source: Callable[[BootstrapSource], bytes]
    probe_sandbox: Callable[[list[str]], tuple[int, str]]
    run_worker: Callable[[Path, float], ChildResult]
    publish_result: Callable[[bytes], None]


@dataclass(frozen=True)
class AttemptResult:
    """reported: one result was published. refused: nothing was published; reason says why."""

    status: Literal["reported", "refused"]
    reason: str = ""


def _refused(reason: str) -> AttemptResult:
    return AttemptResult(status="refused", reason=reason)


def _utc_now() -> datetime:
    return datetime.now(UTC)


def run_attempt(
    bootstrap: WorkerBootstrap,
    receipt: AcceptedReceipt,
    *,
    ports: Ports,
    runner_nonce: str,
    now: datetime,
    intake_root: Path,
    clock: Callable[[], datetime] | None = None,
) -> AttemptResult:
    """Run one attempt, rereading the live clock after intake to bound the worker budget."""
    try:
        planned = plan_claim(bootstrap, receipt, now=now, runner_nonce=runner_nonce)
    except ProtocolError as exc:
        return _refused(exc.message)
    try:
        claim_attempt(ports.claim_store, planned, runner_nonce=runner_nonce)
    except ClaimRefused as exc:
        return _refused(str(exc))

    try:
        archive = ports.read_source(bootstrap.source)
    except OSError:
        return _refused("the source object could not be read")
    limits = BundleLimits()
    workspace = intake_root / WORKSPACE_DIR
    try:
        bundle = extract_source(
            archive,
            expected_sha256=bootstrap.source.sha256,
            expected_size=bootstrap.source.size_bytes,
            limits=limits,
        )
        materialize_checkout(
            bundle, workspace, limits=limits, expected_base_sha=bootstrap.source.base_sha
        )
    except BundleRefused as exc:
        return _refused(str(exc))
    except (OSError, subprocess.SubprocessError):
        return _refused("the source could not be prepared in the intake directory")

    try:
        qualify_sandbox(ports.probe_sandbox, workspace)
    except SandboxUnavailable as exc:
        return _refused(str(exc))

    current = (clock or _utc_now)()
    remaining = (bootstrap.deadline_at - current).total_seconds()
    if remaining <= 0:
        return _refused("the deadline passed before the worker could start")
    child = ports.run_worker(workspace, min(float(bootstrap.max_runtime_seconds), remaining))

    try:
        result = plan_result(
            bootstrap,
            outcome="succeeded" if child.outcome is ChildOutcome.COMPLETED else "failed",
            exit_code=None if child.outcome is ChildOutcome.TIMED_OUT else child.returncode,
            commit_sha=None,
            artifacts=(),
        )
    except ProtocolError as exc:
        return _refused(exc.message)
    ports.publish_result(result.body)
    return AttemptResult(status="reported")


def production_ports() -> Ports:
    """The production adapters. Not built, so this always raises; main then exits EXIT_UNAVAILABLE."""
    raise ProductionPortsUnavailable(
        "production ports are not built: signed object storage is DPES-807, the real sandbox is "
        "DPES-804, and model access is DPES-819; the attempt did not run"
    )


def main(
    argv: list[str] | None = None,
    *,
    ports_factory: Callable[[], Ports] = production_ports,
    clock: Callable[[], datetime] | None = None,
) -> int:
    parser = argparse.ArgumentParser(
        prog="python3 -m mao_cloud.trusted_bootstrap",
        description="Run one trusted worker attempt from its bootstrap and accepted receipt.",
    )
    parser.add_argument("--bootstrap", type=Path, required=True, help="WorkerBootstrap JSON")
    parser.add_argument("--receipt", type=Path, required=True, help="AcceptedReceipt JSON")
    parser.add_argument("--intake-root", type=Path, default=DEFAULT_INTAKE_ROOT)
    args = parser.parse_args(argv)

    try:
        bootstrap = WorkerBootstrap.model_validate_json(args.bootstrap.read_bytes())
        receipt = AcceptedReceipt.model_validate_json(args.receipt.read_bytes())
    except (OSError, ValidationError):
        print("the bootstrap or receipt file is unreadable or malformed", file=sys.stderr)
        return EXIT_UNREADABLE

    try:
        ports = ports_factory()
    except ProductionPortsUnavailable as exc:
        print(f"trusted bootstrap cannot run: {exc}", file=sys.stderr)
        return EXIT_UNAVAILABLE

    live_clock = clock or _utc_now
    now = live_clock()
    result = run_attempt(
        bootstrap,
        receipt,
        ports=ports,
        runner_nonce=new_runner_nonce(),
        now=now,
        intake_root=args.intake_root,
        clock=live_clock,
    )
    if result.status == "refused":
        print(f"attempt refused: {result.reason}", file=sys.stderr)
        return EXIT_REFUSED
    return EXIT_REPORTED


if __name__ == "__main__":
    raise SystemExit(main())
