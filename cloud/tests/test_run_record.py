"""Run record: the request UUID, digest, and archive are durable before any network call.

A record is keyed by request UUID. The same request reloads the same record; a different digest under
the same UUID is a conflict that must never silently replace the first request.
"""

import json
import multiprocessing
import os
import stat
from pathlib import Path
from typing import Any

import pytest

from fakes import EPOCH, uuid7
from mao_cloud.run_record import RecordConflict, RecordStore

ARCHIVE = b"plain tar bytes for the record test"
DIGEST = "a" * 64
OTHER_DIGEST = "b" * 64
BODY: dict[str, Any] = {"protocol_version": "v1", "harness": "pi"}


def _request_uuid() -> str:
    return str(uuid7(EPOCH))


def _open(records: RecordStore, request_uuid: str, *, digest: str = DIGEST) -> Any:
    return records.open_or_create(
        request_uuid,
        canonical_digest=digest,
        create_body=BODY,
        archive=ARCHIVE,
    )


def test_record_and_archive_are_written_with_uuid_and_digest(tmp_path: Path) -> None:
    records = RecordStore(tmp_path / "state")
    request_uuid = _request_uuid()

    record = _open(records, request_uuid)

    assert record.state == "planned"
    assert record.request_uuid == request_uuid
    on_disk = json.loads((records.directory(request_uuid) / "record.json").read_text("utf-8"))
    assert on_disk["request_uuid"] == request_uuid
    assert on_disk["canonical_digest"] == DIGEST
    assert records.archive(record) == ARCHIVE


def test_identical_request_reloads_the_same_record_without_resetting_it(tmp_path: Path) -> None:
    records = RecordStore(tmp_path / "state")
    request_uuid = _request_uuid()
    first = _open(records, request_uuid)
    first.state = "created"
    first.run_id = "run-1"
    records.save(first)

    again = _open(records, request_uuid)

    assert again.state == "created"
    assert again.run_id == "run-1"


def test_conflicting_digest_under_the_same_uuid_raises(tmp_path: Path) -> None:
    records = RecordStore(tmp_path / "state")
    request_uuid = _request_uuid()
    _open(records, request_uuid)

    with pytest.raises(RecordConflict):
        _open(records, request_uuid, digest=OTHER_DIGEST)


def test_saved_state_persists_across_store_instances(tmp_path: Path) -> None:
    root = tmp_path / "state"
    request_uuid = _request_uuid()
    record = _open(RecordStore(root), request_uuid)
    record.state = "started"
    record.run_id = "run-42"
    RecordStore(root).save(record)

    reloaded = RecordStore(root).load(request_uuid)

    assert reloaded.state == "started"
    assert reloaded.run_id == "run-42"


def test_directories_are_owner_only_and_files_are_owner_read_write(tmp_path: Path) -> None:
    root = tmp_path / "state"
    records = RecordStore(root)
    request_uuid = _request_uuid()
    _open(records, request_uuid)
    directory = records.directory(request_uuid)

    for path in (root, directory):
        assert stat.S_IMODE(path.stat().st_mode) == 0o700
    for name in ("record.json", "source.tar"):
        assert stat.S_IMODE((directory / name).stat().st_mode) == 0o600


def _conflicting_open(root, request_uuid, entered, finished, outcome) -> None:
    entered.set()
    try:
        _open(RecordStore(root), request_uuid, digest=OTHER_DIGEST)
        outcome.put("overwritten")
    except RecordConflict:
        outcome.put("conflict")
    finally:
        finished.set()


def test_conflicting_process_reopens_the_record_after_acquiring_its_uuid_lock(tmp_path: Path) -> None:
    records = RecordStore(tmp_path / "state")
    request_uuid = _request_uuid()
    context = multiprocessing.get_context("spawn")
    entered, finished, outcome = context.Event(), context.Event(), context.Queue()
    contender = context.Process(
        target=_conflicting_open,
        args=(records.root, request_uuid, entered, finished, outcome),
    )
    try:
        with records.lock(request_uuid):
            first = _open(records, request_uuid)
            contender.start()
            assert entered.wait(5)
            assert not finished.wait(0.1)
            first.state = "uploaded"
            first.generation = 41
            records.save(first)
        contender.join(10)
        assert contender.exitcode == 0
        assert outcome.get(timeout=2) == "conflict"
        saved = records.load(request_uuid)
        assert saved.canonical_digest == DIGEST
        assert saved.state == "uploaded"
        assert saved.generation == 41
        assert records.archive(saved) == ARCHIVE
    finally:
        if contender.is_alive():
            contender.terminate()
            contender.join(5)


def test_replacement_and_new_directory_entries_are_synced_before_return(
    tmp_path: Path, monkeypatch
) -> None:
    events: list[tuple[str, int]] = []
    original_fsync, original_replace = os.fsync, os.replace

    def fsync(descriptor: int) -> None:
        info = os.fstat(descriptor)
        if stat.S_ISDIR(info.st_mode):
            events.append(("sync", info.st_ino))
        original_fsync(descriptor)

    def replace(source, target) -> None:
        original_replace(source, target)
        events.append(("replace", Path(target).parent.stat().st_ino))

    monkeypatch.setattr(os, "fsync", fsync)
    monkeypatch.setattr(os, "replace", replace)
    records = RecordStore(tmp_path / "new-parent" / "state")
    request_uuid = _request_uuid()
    _open(records, request_uuid)

    for index, (kind, inode) in enumerate(events):
        if kind == "replace":
            assert ("sync", inode) in events[index + 1:]
    for directory in (tmp_path, tmp_path / "new-parent", records.root, records.directory(request_uuid)):
        assert ("sync", directory.stat().st_ino) in events
    lock_file = records.directory(request_uuid) / "submission.lock"
    inode = lock_file.stat().st_ino
    with records.lock(request_uuid):
        records.save(records.load(request_uuid))
    assert lock_file.stat().st_ino == inode
    assert stat.S_IMODE(lock_file.stat().st_mode) == 0o600
