"""Runner-side planning: the decisions a worker makes before it writes anything.

The plan functions never touch storage. Each returns the object name, the exact bytes, and the
headers the write must carry, so every write can be checked against the signed-object rules
before a network call exists. Production writes arrive with DPES-808.
"""

import argparse
import json
import secrets
import sys
from collections.abc import Sequence
from dataclasses import dataclass
from datetime import UTC, datetime
from pathlib import Path
from typing import Literal

from pydantic import BaseModel

from mao_cloud.documents import InputError, load_model, parse_instant
from mao_cloud.protocol import (
    CAPABILITIES,
    PROTOCOL_VERSION,
    AcceptedReceipt,
    ArtifactRef,
    ClaimDocument,
    ErrorCode,
    LogEntry,
    ObjectKind,
    ProtocolError,
    RunResultDocument,
    StatusReport,
    WorkerBootstrap,
    canonical_bytes,
    deadline_passed,
    object_name,
    required_headers,
    sha256_hex,
    verify_bootstrap,
)


@dataclass(frozen=True)
class PlannedWrite:
    """One write the runner would make: where it goes, what it carries, and its precondition."""

    name: str
    method: Literal["PUT"]
    headers: dict[str, str]
    body: bytes

    def describe(self) -> dict[str, object]:
        return {
            "object_name": self.name,
            "method": self.method,
            "headers": self.headers,
            "body": json.loads(self.body),
        }


def _write(
    kind: ObjectKind,
    document: BaseModel,
    *,
    run_id: str,
    attempt: int | None = None,
    seq: int | None = None,
    expected_generation: int | None = None,
) -> PlannedWrite:
    capability = CAPABILITIES[(kind, "PUT")]
    if capability.blocking is not None:
        raise ProtocolError(
            ErrorCode.BLOCKING_FIELD,
            f"{kind.value} writes are blocked until {capability.blocking} resolves",
        )
    body = canonical_bytes(document.model_dump(mode="json"))
    if capability.max_bytes is not None and len(body) > capability.max_bytes:
        raise ProtocolError(
            ErrorCode.INVALID_REQUEST,
            f"{kind.value} body exceeds the {capability.max_bytes}-byte capability limit",
        )
    headers = required_headers(
        kind,
        "PUT",
        sha256_hex_value=sha256_hex(body) if capability.sha256_metadata else None,
        expected_generation=expected_generation,
    )
    return PlannedWrite(
        name=object_name(kind, run_id=run_id, attempt=attempt, seq=seq),
        method="PUT",
        headers=headers,
        body=body,
    )


def plan_claim(
    bootstrap: WorkerBootstrap,
    receipt: AcceptedReceipt,
    *,
    now: datetime,
    runner_nonce: str,
) -> PlannedWrite:
    """Claim the attempt. Refuses unless the bootstrap matches the receipt and time remains.

    runner_nonce must be unpredictable and unique to this runner process; the claim carries it so
    a later exact-key readback can prove which process owns the attempt.
    """
    verify_bootstrap(bootstrap, receipt)
    if deadline_passed(now, bootstrap.deadline_at):
        raise ProtocolError(
            ErrorCode.DEADLINE_EXCEEDED,
            "bootstrap deadline has passed; the runner must not claim",
        )
    claim = ClaimDocument(
        protocol_version=PROTOCOL_VERSION,
        run_id=bootstrap.run_id,
        attempt=bootstrap.attempt,
        fence=bootstrap.fence,
        claim_id=bootstrap.claim_id,
        runner_nonce=runner_nonce,
    )
    return _write(ObjectKind.CLAIM, claim, run_id=bootstrap.run_id, attempt=bootstrap.attempt)


def plan_status(
    bootstrap: WorkerBootstrap,
    *,
    seq: int,
    phase: Literal["starting", "running", "collecting"],
    observed_at: datetime,
    log_watermark: int,
    message: str,
    expected_generation: int,
) -> PlannedWrite:
    """Overwrite the status object. generation_match means a stale writer cannot clobber it."""
    report = StatusReport(
        protocol_version=PROTOCOL_VERSION,
        run_id=bootstrap.run_id,
        attempt=bootstrap.attempt,
        fence=bootstrap.fence,
        claim_id=bootstrap.claim_id,
        seq=seq,
        phase=phase,
        observed_at=observed_at,
        log_watermark=log_watermark,
        message=message,
    )
    return _write(
        ObjectKind.STATUS,
        report,
        run_id=bootstrap.run_id,
        attempt=bootstrap.attempt,
        expected_generation=expected_generation,
    )


def plan_log_entry(bootstrap: WorkerBootstrap, *, seq: int, lines: Sequence[str]) -> PlannedWrite:
    """One create-only log object per entry; the object name carries the seq."""
    entry = LogEntry(protocol_version=PROTOCOL_VERSION, seq=seq, lines=tuple(lines))
    return _write(
        ObjectKind.LOG, entry, run_id=bootstrap.run_id, attempt=bootstrap.attempt, seq=seq
    )


def plan_result(
    bootstrap: WorkerBootstrap,
    *,
    outcome: Literal["succeeded", "failed"],
    exit_code: int | None,
    commit_sha: str | None,
    artifacts: Sequence[ArtifactRef],
) -> PlannedWrite:
    """The single create-only result. Artifacts must already be written under their names."""
    document = RunResultDocument(
        protocol_version=PROTOCOL_VERSION,
        run_id=bootstrap.run_id,
        attempt=bootstrap.attempt,
        fence=bootstrap.fence,
        claim_id=bootstrap.claim_id,
        outcome=outcome,
        exit_code=exit_code,
        commit_sha=commit_sha,
        artifacts=tuple(artifacts),
        redacted=True,
    )
    return _write(ObjectKind.RESULT, document, run_id=bootstrap.run_id, attempt=bootstrap.attempt)


def main(argv: Sequence[str] | None = None) -> int:
    parser = argparse.ArgumentParser(
        prog="mao-cloud-runner",
        description="Plan the claim for a dispatched attempt. Dry run: nothing is written.",
    )
    parser.add_argument("--bootstrap", required=True, type=Path, help="worker bootstrap JSON")
    parser.add_argument("--receipt", required=True, type=Path, help="accepted receipt JSON")
    parser.add_argument("--now", type=parse_instant, help="ISO 8601 time with offset; default now")
    args = parser.parse_args(argv)
    now = args.now or datetime.now(UTC)
    try:
        bootstrap = load_model(args.bootstrap, WorkerBootstrap)
        receipt = load_model(args.receipt, AcceptedReceipt)
        write = plan_claim(bootstrap, receipt, now=now, runner_nonce=secrets.token_hex(16))
    except InputError as exc:
        print(f"mao-cloud-runner: {exc}", file=sys.stderr)
        return 2
    except (ProtocolError, ValueError) as exc:
        print(f"mao-cloud-runner: refused: {exc}", file=sys.stderr)
        return 1
    print(json.dumps(write.describe(), indent=2, sort_keys=True))
    print("mao-cloud-runner: dry run; the claim was planned and not written", file=sys.stderr)
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
