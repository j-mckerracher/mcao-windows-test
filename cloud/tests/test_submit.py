"""Submit: export, record, create, upload, start, with each step safe to repeat after a lost reply.

The service is the real broker over in-memory storage (WorldTransport). Loss, expiry, and spy
wrappers change only what reaches the service or what comes back, so recovery is tested against the
same code that serves production requests. Every test passes one explicit request, so the request
UUID stays fixed across the submits it compares.
"""

import multiprocessing
import os
import stat
from pathlib import Path

import pytest

from fakes import EPOCH, PROFILE_ID, uuid7
from flows import make_world
from gitfixtures import commit_files, make_repo
from mao_cloud.run_record import RecordStore
from mao_cloud.source import export_snapshot
from mao_cloud.submit import Reply, SubmitRequest, SubmitResult, TransportError, submit_snapshot
from submit_fakes import ExpiredFirstPut, LossyTransport, SpyTransport, WorldTransport


def _request() -> SubmitRequest:
    return SubmitRequest(
        task_id="DPES-805",
        prompt_id="prompt-805",
        toolchain_profile=PROFILE_ID,
        target="cloud-pilot",
        harness="pi",
        model="gemini-3.8-flash",
        thinking="medium",
        max_runtime_seconds=600,
        workflow_correlation="wf-805",
        request_uuid=str(uuid7(EPOCH)),
    )


def _repo(tmp_path: Path, content: bytes = b"print('hi')\n") -> Path:
    return make_repo(tmp_path / "work", {"src/app.py": content})


def _submit(
    repo: Path,
    records: RecordStore,
    transport: object,
    request: SubmitRequest,
) -> SubmitResult:
    snapshot = export_snapshot(repo, "main")
    return submit_snapshot(snapshot, request, records=records, transport=transport)


def test_happy_path_starts_one_run(tmp_path: Path) -> None:
    world = make_world()
    records = RecordStore(tmp_path / "state")
    request = _request()

    result = _submit(_repo(tmp_path), records, WorldTransport(world), request)

    assert result.exit_code == 0
    assert result.run_id is not None
    assert world.store.run_count() == 1
    assert records.load(request.request_uuid).state == "started"


def test_record_is_durable_before_the_first_transport_call(tmp_path: Path) -> None:
    world = make_world()
    records = RecordStore(tmp_path / "state")
    request = _request()
    record_file = records.directory(request.request_uuid) / "record.json"
    first_call: dict[str, object] = {}

    def before(op: str) -> None:
        first_call.setdefault("op", op)
        first_call.setdefault("record_exists", record_file.exists())

    spy = SpyTransport(WorldTransport(world), before=before)
    _submit(_repo(tmp_path), records, spy, request)

    assert first_call == {"op": "create", "record_exists": True}


def test_identical_resubmit_returns_the_same_run_without_network(tmp_path: Path) -> None:
    world = make_world()
    records = RecordStore(tmp_path / "state")
    repo = _repo(tmp_path)
    request = _request()
    first = _submit(repo, records, WorldTransport(world), request)

    spy = SpyTransport(None)
    second = _submit(repo, records, spy, request)

    assert second.exit_code == 0
    assert second.run_id == first.run_id
    assert spy.calls == []
    assert world.store.run_count() == 1


def test_identical_request_after_losing_the_local_record_replays_the_same_run(
    tmp_path: Path,
) -> None:
    world = make_world()
    repo = _repo(tmp_path)
    request = _request()
    first = _submit(repo, RecordStore(tmp_path / "state-a"), WorldTransport(world), request)

    second = _submit(repo, RecordStore(tmp_path / "state-b"), WorldTransport(world), request)

    assert second.exit_code == 0
    assert second.run_id == first.run_id
    assert world.store.run_count() == 1


def test_lost_create_response_recovers_through_replay(tmp_path: Path) -> None:
    world = make_world()
    records = RecordStore(tmp_path / "state")
    repo = _repo(tmp_path)
    request = _request()
    lossy = LossyTransport(WorldTransport(world), lose_reply=("create",))

    first = _submit(repo, records, lossy, request)
    second = _submit(repo, records, WorldTransport(world), request)

    assert first.exit_code == 75
    assert second.exit_code == 0
    assert world.store.run_count() == 1


def test_lost_put_request_that_never_landed_recovers(tmp_path: Path) -> None:
    world = make_world()
    records = RecordStore(tmp_path / "state")
    repo = _repo(tmp_path)
    request = _request()
    lossy = LossyTransport(WorldTransport(world), lose_request=("put",))

    first = _submit(repo, records, lossy, request)
    second = _submit(repo, records, WorldTransport(world), request)

    assert first.exit_code == 75
    assert second.exit_code == 0
    assert world.store.run_count() == 1


def test_lost_put_reply_after_the_object_landed_refuses_then_a_new_uuid_starts(
    tmp_path: Path,
) -> None:
    world = make_world()
    records = RecordStore(tmp_path / "state")
    repo = _repo(tmp_path)
    request = _request()
    lossy = LossyTransport(WorldTransport(world), lose_reply=("put",))

    first = _submit(repo, records, lossy, request)
    again = _submit(repo, records, WorldTransport(world), request)
    fresh = _submit(repo, RecordStore(tmp_path / "state-fresh"), WorldTransport(world), _request())

    assert first.exit_code == 75
    assert again.exit_code == 1
    assert again.code == "upload_unknown"
    assert fresh.exit_code == 0
    assert world.store.run_count() == 2


def test_lost_start_response_recovers_through_the_recorded_receipt(tmp_path: Path) -> None:
    world = make_world()
    records = RecordStore(tmp_path / "state")
    repo = _repo(tmp_path)
    request = _request()
    lossy = LossyTransport(WorldTransport(world), lose_reply=("start",))

    first = _submit(repo, records, lossy, request)
    second = _submit(repo, records, WorldTransport(world), request)

    assert first.exit_code == 75
    assert second.exit_code == 0
    assert second.run_id is not None
    assert world.store.run_count() == 1


def test_expired_upload_url_is_reissued_once_then_succeeds(tmp_path: Path) -> None:
    world = make_world()
    records = RecordStore(tmp_path / "state")
    expired = ExpiredFirstPut(WorldTransport(world))

    result = _submit(_repo(tmp_path), records, expired, _request())

    assert result.exit_code == 0
    assert expired.creates == 2
    assert world.store.run_count() == 1


class _AlwaysExpiredPut(WorldTransport):
    def put(self, url: str, headers: dict[str, str], data: bytes) -> Reply:
        return Reply(403, {"error": {"code": "not_allowed", "message": "URL expired"}})


def test_upload_that_keeps_expiring_is_refused_after_one_reissue(tmp_path: Path) -> None:
    world = make_world()
    records = RecordStore(tmp_path / "state")

    result = _submit(_repo(tmp_path), records, _AlwaysExpiredPut(world), _request())

    assert result.exit_code == 1
    assert result.code == "upload_expired"
    assert world.store.run_count() == 1


def test_window_closed_before_resubmit_is_a_deadline_refusal(tmp_path: Path) -> None:
    world = make_world()
    records = RecordStore(tmp_path / "state")
    repo = _repo(tmp_path)
    request = _request()
    _submit(repo, records, LossyTransport(WorldTransport(world), lose_reply=("create",)), request)
    world.clock.advance(seconds=601)

    result = _submit(repo, records, WorldTransport(world), request)

    assert result.exit_code == 1
    assert result.code == "deadline_exceeded"
    assert world.store.run_count() == 1


def test_admissions_paused_keeps_the_record_and_resumes_when_reopened(tmp_path: Path) -> None:
    world = make_world()
    world.admissions.set_open(False)
    records = RecordStore(tmp_path / "state")
    repo = _repo(tmp_path)
    request = _request()

    first = _submit(repo, records, WorldTransport(world), request)
    assert first.exit_code == 75
    assert records.load(request.request_uuid).state == "planned"
    assert world.store.run_count() == 0

    world.admissions.set_open(True)
    second = _submit(repo, records, WorldTransport(world), request)

    assert second.exit_code == 0
    assert world.store.run_count() == 1


def test_same_request_uuid_with_different_content_is_refused_by_the_service(tmp_path: Path) -> None:
    world = make_world()
    request = _request()
    _submit(_repo(tmp_path), RecordStore(tmp_path / "state-a"), WorldTransport(world), request)

    other = make_repo(tmp_path / "other", {"src/app.py": b"print('different')\n"})
    result = _submit(other, RecordStore(tmp_path / "state-b"), WorldTransport(world), request)

    assert result.exit_code == 1
    assert result.code == "request_digest_conflict"
    assert world.store.run_count() == 1


def test_tampered_local_archive_is_refused_before_any_network_call(tmp_path: Path) -> None:
    world = make_world()
    records = RecordStore(tmp_path / "state")
    repo = _repo(tmp_path)
    request = _request()
    lost = LossyTransport(WorldTransport(world), lose_request=("create",))
    _submit(repo, records, lost, request)
    (records.directory(request.request_uuid) / "source.tar").write_bytes(b"tampered")

    spy = SpyTransport(None)
    result = _submit(repo, records, spy, request)

    assert result.exit_code == 1
    assert result.code == "archive_mismatch"
    assert spy.calls == []
    assert world.store.run_count() == 0


def test_changed_source_under_the_same_uuid_is_refused_locally(tmp_path: Path) -> None:
    world = make_world()
    records = RecordStore(tmp_path / "state")
    repo = _repo(tmp_path)
    request = _request()
    lost = LossyTransport(WorldTransport(world), lose_request=("create",))
    _submit(repo, records, lost, request)
    commit_files(repo, {"src/app.py": b"print('edited')\n"}, "edit")

    spy = SpyTransport(None)
    result = _submit(repo, records, spy, request)

    assert result.exit_code == 1
    assert result.code == "request_uuid_conflict"
    assert spy.calls == []
    assert world.store.run_count() == 0


def _concurrent_submit(snapshot, request, root, entered, finished, outcome) -> None:
    entered.set()
    try:
        outcome.put(
            submit_snapshot(snapshot, request, records=RecordStore(root), transport=SpyTransport(None))
        )
    finally:
        finished.set()


@pytest.mark.parametrize("stage", ["create", "put", "start"])
def test_submission_lock_covers_each_remote_stage_and_reloads_after_waiting(
    tmp_path: Path, stage: str
) -> None:
    world = make_world()
    records = RecordStore(tmp_path / "state")
    request = _request()
    snapshot = export_snapshot(_repo(tmp_path), "main")
    context = multiprocessing.get_context("spawn")
    entered, finished, outcome = context.Event(), context.Event(), context.Queue()
    contender = context.Process(
        target=_concurrent_submit,
        args=(snapshot, request, records.root, entered, finished, outcome),
    )

    def before(operation: str) -> None:
        if operation == stage:
            contender.start()
            assert entered.wait(5)
            assert not finished.wait(0.1)

    try:
        first = submit_snapshot(
            snapshot, request, records=records,
            transport=SpyTransport(WorldTransport(world), before=before),
        )
        contender.join(10)
        assert contender.exitcode == 0
        second = outcome.get(timeout=2)
        assert first.exit_code == second.exit_code == 0
        assert first.run_id == second.run_id
        assert records.load(request.request_uuid).state == "started"
        assert world.store.run_count() == 1
    finally:
        if contender.is_alive():
            contender.terminate()
            contender.join(5)


@pytest.mark.parametrize(
    ("state", "code", "retryable"),
    [("cancelled", "owner_cancelled", False), ("timed_out", "upload_expired", False),
     ("failed", "worker_failed", True)],
)
def test_lost_create_replay_with_a_prestart_terminal_state_is_a_typed_refusal(
    tmp_path: Path, state: str, code: str, retryable: bool
) -> None:
    world = make_world()
    records = RecordStore(tmp_path / "state")
    request = _request()
    repo = _repo(tmp_path)

    class TerminalReplay(WorldTransport):
        creates = 0

        def create(self, body) -> Reply:
            reply = super().create(body)
            self.creates += 1
            if self.creates == 1:
                raise TransportError("create reply lost")
            reply.body["upload"] = None
            reply.body["run"]["state"] = state
            reply.body["run"]["failure"] = {
                "code": code, "retryable": retryable, "message": "stopped before upload"
            }
            return Reply(200, reply.body)

        def put(self, url, headers, data) -> Reply:
            raise AssertionError("prestart terminal replay must not upload")

        def start(self, run_id, body) -> Reply:
            raise AssertionError("prestart terminal replay must not fabricate acceptance")

    transport = TerminalReplay(world)
    first = _submit(repo, records, transport, request)
    replay = _submit(repo, records, transport, request)
    refused = _submit(repo, records, SpyTransport(None), request)

    assert first.exit_code == 75
    assert replay.exit_code == refused.exit_code == 1
    assert replay.code == refused.code == code
    assert replay.message == "stopped before upload"
    assert records.load(request.request_uuid).state == "refused"


def test_directory_sync_failure_prevents_the_first_network_call(tmp_path: Path, monkeypatch) -> None:
    records = RecordStore(tmp_path / "state")
    request = _request()
    snapshot = export_snapshot(_repo(tmp_path), "main")
    original_fsync = os.fsync
    record_file = records.directory(request.request_uuid) / "record.json"

    def fsync(descriptor: int) -> None:
        if stat.S_ISDIR(os.fstat(descriptor).st_mode) and record_file.exists():
            raise OSError("directory sync failed")
        original_fsync(descriptor)

    monkeypatch.setattr(os, "fsync", fsync)
    transport = SpyTransport(None)
    with pytest.raises(OSError, match="directory sync failed"):
        submit_snapshot(snapshot, request, records=records, transport=transport)
    assert transport.calls == []
