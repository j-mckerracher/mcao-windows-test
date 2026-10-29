"""Runner planning, sweeper planning, and the mao-cloud CLI, exercised through their entry points."""

import json
import shutil
from datetime import timedelta
from pathlib import Path
from typing import Any

import pytest
from pydantic import ValidationError

from fakes import (
    CLAIM_ID,
    RUNNER_NONCE,
    DEFAULT_MAX_RUNTIME_SECONDS,
    EPOCH,
    IMAGE_DIGEST,
    RUN_ID,
    bootstrap_source,
    create_payload,
    make_policy,
    policy_document,
    refusal_code,
    run_record,
    start_payload,
    tool_compatibility,
    worker_bootstrap,
)
from mao_cloud import cli, runner, sweeper
from mao_cloud.protocol import (
    TERMINAL_STATES,
    ArtifactRef,
    ClaimDocument,
    ErrorCode,
    FailureCode,
    ObjectKind,
    ProtocolError,
    RunState,
    build_receipt,
    sha256_hex,
)

OTHER_RUN_ID = "run_" + "c" * 32
DEADLINE = EPOCH + timedelta(seconds=DEFAULT_MAX_RUNTIME_SECONDS)
POLICY = make_policy()
COMMITTED_SCHEMA_DIR = Path(__file__).resolve().parents[1] / "schemas" / "protocol-v1"


def _at(seconds: float) -> Any:
    return EPOCH + timedelta(seconds=seconds)


def _receipt() -> Any:
    return build_receipt(run_record(), source=bootstrap_source(), compatibility=tool_compatibility())


def _json_file(directory: Path, name: str, payload: Any) -> Path:
    path = directory / name
    path.write_text(json.dumps(payload), encoding="utf-8")
    return path


# --- Runner: claim planning ---------------------------------------------------------------------


def test_claim_is_planned_at_its_attempt_scoped_name() -> None:
    write = runner.plan_claim(worker_bootstrap(), _receipt(), now=EPOCH, runner_nonce=RUNNER_NONCE)
    assert write.name == f"runs/{RUN_ID}/attempts/000001/claim.json"
    assert write.method == "PUT"


def test_claim_for_a_retry_is_named_by_its_attempt() -> None:
    bootstrap = worker_bootstrap(attempt=2, fence=2)
    write = runner.plan_claim(bootstrap, _receipt(), now=EPOCH, runner_nonce=RUNNER_NONCE)
    assert write.name == f"runs/{RUN_ID}/attempts/000002/claim.json"


def test_claim_upload_is_create_only_json() -> None:
    write = runner.plan_claim(worker_bootstrap(), _receipt(), now=EPOCH, runner_nonce=RUNNER_NONCE)
    assert write.headers == {
        "content-type": "application/json",
        "x-goog-if-generation-match": "0",
    }


def test_claim_body_is_the_claim_document_for_this_attempt() -> None:
    write = runner.plan_claim(worker_bootstrap(), _receipt(), now=EPOCH, runner_nonce=RUNNER_NONCE)
    expected = ClaimDocument(
        protocol_version="v1",
        run_id=RUN_ID,
        attempt=1,
        fence=1,
        claim_id=CLAIM_ID,
        runner_nonce=RUNNER_NONCE,
    )
    assert json.loads(write.body) == expected.model_dump(mode="json")


def test_claim_body_carries_the_runner_nonce_that_proves_ownership() -> None:
    write = runner.plan_claim(worker_bootstrap(), _receipt(), now=EPOCH, runner_nonce=RUNNER_NONCE)
    assert json.loads(write.body)["runner_nonce"] == RUNNER_NONCE


def test_claim_refuses_a_runner_nonce_that_is_not_32_lowercase_hex() -> None:
    for nonce in ("", "1f" * 15, "1F" * 16, "zz" * 16, "1f" * 16 + "1f"):
        with pytest.raises(ValidationError):
            runner.plan_claim(worker_bootstrap(), _receipt(), now=EPOCH, runner_nonce=nonce)


def test_claim_document_without_a_runner_nonce_is_invalid() -> None:
    with pytest.raises(ValidationError):
        ClaimDocument.model_validate(
            {"protocol_version": "v1", "run_id": RUN_ID, "attempt": 1, "fence": 1, "claim_id": CLAIM_ID}
        )


def test_claim_is_refused_once_the_deadline_has_passed() -> None:
    code = refusal_code(
        lambda: runner.plan_claim(worker_bootstrap(), _receipt(), now=DEADLINE, runner_nonce=RUNNER_NONCE)
    )
    assert code is ErrorCode.DEADLINE_EXCEEDED


def test_claim_is_refused_for_a_bootstrap_that_does_not_match_the_receipt() -> None:
    bootstrap = worker_bootstrap(max_runtime_seconds=900)
    code = refusal_code(lambda: runner.plan_claim(bootstrap, _receipt(), now=EPOCH, runner_nonce=RUNNER_NONCE))
    assert code is ErrorCode.BOOTSTRAP_MISMATCH


def test_claim_is_refused_for_a_bootstrap_naming_another_run() -> None:
    bootstrap = worker_bootstrap(run_id=OTHER_RUN_ID)
    code = refusal_code(lambda: runner.plan_claim(bootstrap, _receipt(), now=EPOCH, runner_nonce=RUNNER_NONCE))
    assert code is ErrorCode.BOOTSTRAP_MISMATCH


# --- Runner: status, log, and result planning ----------------------------------------------------


def _status(**overrides: Any) -> Any:
    values: dict[str, Any] = {
        "seq": 1,
        "phase": "running",
        "observed_at": EPOCH,
        "log_watermark": 0,
        "message": "Bearer abc.def-123",
        "expected_generation": 7,
    }
    values.update(overrides)
    return runner.plan_status(worker_bootstrap(), **values)


def test_status_overwrites_the_status_object_under_generation_match() -> None:
    write = _status()
    assert write.name == f"runs/{RUN_ID}/attempts/000001/status.json"
    assert write.headers == {
        "content-type": "application/json",
        "x-goog-if-generation-match": "7",
    }


def test_status_carries_no_sha256_metadata_header() -> None:
    assert "x-goog-meta-sha256" not in _status().headers


def test_status_message_is_redacted_before_the_bytes_exist() -> None:
    message = json.loads(_status().body)["message"]
    assert "abc.def" not in message
    assert message == "Bearer [REDACTED]"


def test_status_sequence_starts_at_one() -> None:
    with pytest.raises(ValidationError):
        _status(seq=0)


def test_log_entry_is_one_create_only_object_named_by_its_sequence() -> None:
    write = runner.plan_log_entry(worker_bootstrap(), seq=1, lines=["step one"])
    assert write.name == f"runs/{RUN_ID}/attempts/000001/logs/0000000001.ndjson"
    assert write.headers == {
        "content-type": "application/x-ndjson",
        "x-goog-if-generation-match": "0",
    }


def test_log_lines_are_redacted_in_the_stored_bytes() -> None:
    write = runner.plan_log_entry(worker_bootstrap(), seq=2, lines=["token=abc123"])
    assert json.loads(write.body)["lines"] == ["token=[REDACTED]"]


def test_log_line_over_the_protocol_limit_is_not_planned() -> None:
    with pytest.raises(ValidationError):
        runner.plan_log_entry(worker_bootstrap(), seq=1, lines=["x" * 4097])


def test_result_is_the_single_create_only_result_object_with_a_body_checksum() -> None:
    write = runner.plan_result(
        worker_bootstrap(),
        outcome="succeeded",
        exit_code=0,
        commit_sha="b" * 40,
        artifacts=[],
    )
    assert write.name == f"runs/{RUN_ID}/attempts/000001/result.json"
    assert write.headers == {
        "content-type": "application/json",
        "x-goog-if-generation-match": "0",
        "x-goog-meta-sha256": sha256_hex(write.body),
    }


def test_result_body_lists_its_artifacts_by_name_and_checksum() -> None:
    artifact = ArtifactRef(name="reports/out.txt", sha256="a" * 64, size_bytes=3)
    write = runner.plan_result(
        worker_bootstrap(),
        outcome="failed",
        exit_code=2,
        commit_sha=None,
        artifacts=[artifact],
    )
    body = json.loads(write.body)
    assert body["outcome"] == "failed"
    assert body["artifacts"] == [{"name": "reports/out.txt", "sha256": "a" * 64, "size_bytes": 3}]


def test_checkpoint_writes_are_blocked_until_dpes_869_resolves() -> None:
    claim = ClaimDocument(
        protocol_version="v1",
        run_id=RUN_ID,
        attempt=1,
        fence=1,
        claim_id=CLAIM_ID,
        runner_nonce=RUNNER_NONCE,
    )
    with pytest.raises(ProtocolError) as caught:
        runner._write(ObjectKind.CHECKPOINT, claim, run_id=RUN_ID, attempt=1, seq=1)
    assert caught.value.code is ErrorCode.BLOCKING_FIELD
    assert "DPES-869" in caught.value.message


# --- Runner: command line -----------------------------------------------------------------------


def test_runner_main_plans_the_claim_and_writes_nothing(tmp_path: Path, capsys: Any) -> None:
    bootstrap = _json_file(tmp_path, "bootstrap.json", worker_bootstrap().model_dump(mode="json"))
    receipt = _json_file(tmp_path, "receipt.json", _receipt().model_dump(mode="json"))
    code = runner.main(
        ["--bootstrap", str(bootstrap), "--receipt", str(receipt), "--now", EPOCH.isoformat()]
    )
    captured = capsys.readouterr()
    assert code == 0
    plan = json.loads(captured.out)
    assert plan["object_name"] == f"runs/{RUN_ID}/attempts/000001/claim.json"
    assert plan["method"] == "PUT"
    assert "dry run; the claim was planned and not written" in captured.err


def test_runner_main_refuses_a_mismatched_receipt(tmp_path: Path, capsys: Any) -> None:
    bootstrap = _json_file(
        tmp_path, "bootstrap.json", worker_bootstrap(max_runtime_seconds=900).model_dump(mode="json")
    )
    receipt = _json_file(tmp_path, "receipt.json", _receipt().model_dump(mode="json"))
    code = runner.main(
        ["--bootstrap", str(bootstrap), "--receipt", str(receipt), "--now", EPOCH.isoformat()]
    )
    assert code == 1
    assert "refused" in capsys.readouterr().err


def test_runner_main_refuses_once_the_deadline_has_passed(tmp_path: Path, capsys: Any) -> None:
    bootstrap = _json_file(tmp_path, "bootstrap.json", worker_bootstrap().model_dump(mode="json"))
    receipt = _json_file(tmp_path, "receipt.json", _receipt().model_dump(mode="json"))
    code = runner.main(
        ["--bootstrap", str(bootstrap), "--receipt", str(receipt), "--now", DEADLINE.isoformat()]
    )
    assert code == 1
    assert "deadline" in capsys.readouterr().err


def test_runner_main_reports_a_missing_file_as_unusable_input(tmp_path: Path) -> None:
    bootstrap = _json_file(tmp_path, "bootstrap.json", worker_bootstrap().model_dump(mode="json"))
    code = runner.main(
        [
            "--bootstrap",
            str(bootstrap),
            "--receipt",
            str(tmp_path / "missing.json"),
            "--now",
            EPOCH.isoformat(),
        ]
    )
    assert code == 2


def test_runner_main_rejects_a_naive_now_during_argument_parsing(tmp_path: Path) -> None:
    bootstrap = _json_file(tmp_path, "bootstrap.json", worker_bootstrap().model_dump(mode="json"))
    receipt = _json_file(tmp_path, "receipt.json", _receipt().model_dump(mode="json"))
    with pytest.raises(SystemExit) as caught:
        runner.main(
            ["--bootstrap", str(bootstrap), "--receipt", str(receipt), "--now", "2026-10-21T12:00:00"]
        )
    assert caught.value.code == 2


# --- Sweeper: planning --------------------------------------------------------------------------


def _sweep(*records: Any, now: Any) -> list[Any]:
    return sweeper.plan_sweep(records, now=now, policy=POLICY)


def test_fresh_running_run_gets_a_reconcile_and_no_transition() -> None:
    record = run_record(
        state=RunState.RUNNING,
        claim_id=CLAIM_ID,
        last_seen_at=_at(100),
        state_entered_at=EPOCH,
        version=3,
    )
    (action,) = _sweep(record, now=_at(110))
    assert (action.run_id, action.reason, action.to, action.failure) == (
        RUN_ID,
        "reconcile",
        None,
        None,
    )


def test_stale_running_run_is_moved_to_reconciling_by_liveness() -> None:
    record = run_record(
        state=RunState.RUNNING,
        claim_id=CLAIM_ID,
        last_seen_at=EPOCH,
        state_entered_at=EPOCH,
        version=3,
    )
    (action,) = _sweep(record, now=_at(121))
    assert action.reason == "liveness"
    assert action.to is RunState.RECONCILING
    assert action.failure is None


def test_fresh_queued_run_gets_a_reconcile() -> None:
    record = run_record(state=RunState.QUEUED, state_entered_at=EPOCH)
    (action,) = _sweep(record, now=_at(60))
    assert action.reason == "reconcile"


def test_queued_run_past_its_window_times_out_as_queued_expired() -> None:
    record = run_record(state=RunState.QUEUED, state_entered_at=EPOCH)
    (action,) = _sweep(record, now=_at(1800))
    assert action.reason == "liveness"
    assert action.to is RunState.TIMED_OUT
    assert action.failure is not None
    assert action.failure.code is FailureCode.QUEUED_EXPIRED


def test_awaiting_upload_inside_its_window_gets_no_action() -> None:
    record = run_record(state=RunState.AWAITING_UPLOAD, version=1, last_seen_at=None)
    assert _sweep(record, now=_at(60)) == []


def test_awaiting_upload_past_its_window_times_out_as_upload_expired() -> None:
    record = run_record(state=RunState.AWAITING_UPLOAD, version=1, last_seen_at=None)
    (action,) = _sweep(record, now=_at(600))
    assert action.to is RunState.TIMED_OUT
    assert action.failure is not None
    assert action.failure.code is FailureCode.UPLOAD_EXPIRED


@pytest.mark.parametrize("state", sorted(TERMINAL_STATES, key=lambda item: item.value))
def test_terminal_runs_are_skipped(state: RunState) -> None:
    record = run_record(state=state, last_seen_at=EPOCH)
    assert _sweep(record, now=_at(DEFAULT_MAX_RUNTIME_SECONDS * 10)) == []


def test_plan_is_sorted_by_run_id_regardless_of_input_order() -> None:
    later = run_record(run_id=OTHER_RUN_ID)
    earlier = run_record()
    actions = _sweep(later, earlier, now=_at(60))
    assert [action.run_id for action in actions] == [RUN_ID, OTHER_RUN_ID]


# --- Sweeper: command line ----------------------------------------------------------------------


def test_sweeper_main_plans_without_applying(tmp_path: Path, capsys: Any) -> None:
    record = run_record(
        state=RunState.RUNNING,
        claim_id=CLAIM_ID,
        last_seen_at=EPOCH,
        state_entered_at=EPOCH,
        version=3,
    )
    policy = _json_file(tmp_path, "policy.json", policy_document())
    records = _json_file(tmp_path, "records.json", [record.model_dump(mode="json")])
    code = sweeper.main(
        ["--policy", str(policy), "--records", str(records), "--now", _at(121).isoformat()]
    )
    captured = capsys.readouterr()
    assert code == 0
    assert json.loads(captured.out) == [
        {"run_id": RUN_ID, "reason": "liveness", "to": "reconciling", "failure": None}
    ]
    assert "dry run; no action was applied" in captured.err


def test_sweeper_main_reports_an_unreadable_policy_as_unusable_input(tmp_path: Path) -> None:
    records = _json_file(tmp_path, "records.json", [])
    code = sweeper.main(
        [
            "--policy",
            str(tmp_path / "absent.json"),
            "--records",
            str(records),
            "--now",
            EPOCH.isoformat(),
        ]
    )
    assert code == 2


def test_sweeper_main_rejects_records_that_are_not_an_array(tmp_path: Path) -> None:
    policy = _json_file(tmp_path, "policy.json", policy_document())
    records = _json_file(tmp_path, "records.json", {})
    code = sweeper.main(
        ["--policy", str(policy), "--records", str(records), "--now", EPOCH.isoformat()]
    )
    assert code == 2


def test_sweeper_main_rejects_an_invalid_policy(tmp_path: Path) -> None:
    policy = _json_file(tmp_path, "policy.json", {})
    records = _json_file(tmp_path, "records.json", [])
    code = sweeper.main(
        ["--policy", str(policy), "--records", str(records), "--now", EPOCH.isoformat()]
    )
    assert code == 2


def test_sweeper_main_rejects_an_invalid_record(tmp_path: Path) -> None:
    policy = _json_file(tmp_path, "policy.json", policy_document())
    records = _json_file(tmp_path, "records.json", [{}])
    code = sweeper.main(
        ["--policy", str(policy), "--records", str(records), "--now", EPOCH.isoformat()]
    )
    assert code == 2


def test_sweeper_main_rejects_a_naive_now_during_argument_parsing(tmp_path: Path) -> None:
    policy = _json_file(tmp_path, "policy.json", policy_document())
    records = _json_file(tmp_path, "records.json", [])
    with pytest.raises(SystemExit) as caught:
        sweeper.main(
            ["--policy", str(policy), "--records", str(records), "--now", "2026-10-21T12:00:00"]
        )
    assert caught.value.code == 2


# --- CLI: validate ------------------------------------------------------------------------------


def test_validate_create_accepts_a_well_formed_request(tmp_path: Path, capsys: Any) -> None:
    path = _json_file(tmp_path, "create.json", create_payload())
    assert cli.main(["validate", "create", str(path)]) == 0
    assert "create document is valid" in capsys.readouterr().out


def test_validate_create_refuses_a_client_controlled_field(tmp_path: Path, capsys: Any) -> None:
    payload = create_payload()
    payload["image_digest"] = IMAGE_DIGEST
    path = _json_file(tmp_path, "create.json", payload)
    assert cli.main(["validate", "create", str(path)]) == 1
    assert "client_controlled_field" in capsys.readouterr().err


def test_validate_create_refuses_an_unknown_field(tmp_path: Path, capsys: Any) -> None:
    payload = create_payload()
    payload["surprise"] = True
    path = _json_file(tmp_path, "create.json", payload)
    assert cli.main(["validate", "create", str(path)]) == 1
    assert "unknown_field" in capsys.readouterr().err


def test_validate_create_refuses_the_blocked_restart_budget(tmp_path: Path, capsys: Any) -> None:
    payload = create_payload()
    payload["budget"]["restart_budget"] = 2
    path = _json_file(tmp_path, "create.json", payload)
    assert cli.main(["validate", "create", str(path)]) == 1
    assert "blocking_field" in capsys.readouterr().err


def test_validate_bootstrap_accepts_a_worker_bootstrap(tmp_path: Path, capsys: Any) -> None:
    path = _json_file(tmp_path, "bootstrap.json", worker_bootstrap().model_dump(mode="json"))
    assert cli.main(["validate", "bootstrap", str(path)]) == 0
    assert "bootstrap document is valid" in capsys.readouterr().out


def test_validate_bootstrap_refuses_a_populated_blocked_field(tmp_path: Path, capsys: Any) -> None:
    payload = worker_bootstrap().model_dump(mode="json")
    payload["durable_root"] = "/var/lib/mao"
    path = _json_file(tmp_path, "bootstrap.json", payload)
    assert cli.main(["validate", "bootstrap", str(path)]) == 1
    assert "blocking_field" in capsys.readouterr().err


def test_validate_start_accepts_a_well_formed_start_request(tmp_path: Path, capsys: Any) -> None:
    path = _json_file(tmp_path, "start.json", start_payload(create_payload(), generation=1))
    assert cli.main(["validate", "start", str(path)]) == 0
    assert "start document is valid" in capsys.readouterr().out


def test_validate_reports_a_missing_file_as_unusable_input(tmp_path: Path) -> None:
    assert cli.main(["validate", "create", str(tmp_path / "missing.json")]) == 2


def test_validate_reports_invalid_json_as_unusable_input(tmp_path: Path) -> None:
    path = tmp_path / "broken.json"
    path.write_text("{not json", encoding="utf-8")
    assert cli.main(["validate", "create", str(path)]) == 2


# --- CLI: schema --------------------------------------------------------------------------------


def test_schema_export_writes_every_schema(tmp_path: Path, capsys: Any) -> None:
    out = tmp_path / "out"
    assert cli.main(["schema", "export", "--out", str(out)]) == 0
    assert len(list(out.glob("*.json"))) == 21
    assert "wrote 21 schema files" in capsys.readouterr().out


def test_schema_check_passes_on_the_committed_directory(capsys: Any) -> None:
    assert cli.main(["schema", "check", "--dir", str(COMMITTED_SCHEMA_DIR)]) == 0
    assert "matches the protocol-v1 schemas" in capsys.readouterr().out


def test_schema_check_fails_on_a_stale_directory(tmp_path: Path, capsys: Any) -> None:
    target = tmp_path / "protocol-v1"
    shutil.copytree(COMMITTED_SCHEMA_DIR, target)
    (target / "worker-bootstrap.json").write_text("{}\n", encoding="utf-8")
    assert cli.main(["schema", "check", "--dir", str(target)]) == 1
    assert "stale worker-bootstrap.json" in capsys.readouterr().err


def test_schema_check_fails_on_a_directory_that_does_not_exist(tmp_path: Path) -> None:
    assert cli.main(["schema", "check", "--dir", str(tmp_path / "absent")]) == 1


def test_a_command_is_required() -> None:
    with pytest.raises(SystemExit) as caught:
        cli.main([])
    assert caught.value.code == 2


@pytest.mark.parametrize("state", sorted(TERMINAL_STATES, key=lambda value: value.value))
def test_terminal_run_with_an_outstanding_reservation_still_gets_reconciled(state: RunState) -> None:
    held = run_record(state=state, reserved_seconds=600, dispatched_at=EPOCH)
    (action,) = _sweep(held, now=_at(1000))
    assert (action.run_id, action.reason, action.to) == (RUN_ID, "reconcile", None)
    released = held.model_copy(update={"reserved_seconds": 0})
    assert _sweep(released, now=_at(1000)) == []
