"""Durable local record of one submit request, keyed by its request UUID.

The request UUID, canonical digest, create body, and source archive are written before the first
network call, so a lost reply can be retried with the same bytes. A record never changes its digest:
the same UUID with different content is a conflict, not a replacement.

Layout: <root>/<request uuid>/record.json and source.tar. The root and request directories are
owner-only (0700); the files are owner read-write (0600). Writes go through a temporary file and
os.replace, so a crash leaves either the old record or the new one.
"""

import fcntl
import hashlib
import json
import os
import tempfile
import threading
import uuid
from collections.abc import Iterator
from contextlib import contextmanager
from dataclasses import asdict, dataclass
from pathlib import Path
from typing import Any, Literal

RecordState = Literal["planned", "created", "uploaded", "started", "refused"]

_DIR_MODE = 0o700
_FILE_MODE = 0o600
_RECORD_NAME = "record.json"
_ARCHIVE_NAME = "source.tar"
_LOCK_NAME = "submission.lock"


class RecordConflict(Exception):
    """The request UUID already holds a record with a different canonical digest."""


@dataclass
class RunRecord:
    request_uuid: str
    canonical_digest: str
    create_body: dict[str, Any]
    archive_sha256: str
    archive_size: int
    state: RecordState = "planned"
    run_id: str | None = None
    upload: dict[str, Any] | None = None
    generation: int | None = None
    receipt: dict[str, Any] | None = None
    refusal: dict[str, str] | None = None


class RecordStore:
    def __init__(self, root: Path) -> None:
        self.root = root
        self._held = threading.local()

    @contextmanager
    def lock(self, request_uuid: str) -> Iterator[None]:
        """Serialize a UUID's complete load/advance/save transaction across Unix processes.

        Nested operations on this store in the same thread reuse the held lock. The lock file
        remains at a stable inode for the request's lifetime and is never replaced or removed.
        """
        key = str(uuid.UUID(request_uuid))
        held = getattr(self._held, "keys", frozenset())
        if key in held:
            yield
            return
        self._make_directory(self.root)
        directory = self.directory(key)
        self._make_directory(directory)
        descriptor = os.open(directory / _LOCK_NAME, os.O_CREAT | os.O_RDWR, _FILE_MODE)
        try:
            fcntl.flock(descriptor, fcntl.LOCK_EX)
            os.fchmod(descriptor, _FILE_MODE)
            os.fsync(descriptor)
            _sync_directory(directory)
            self._held.keys = held | {key}
            try:
                yield
            finally:
                self._held.keys = held
                fcntl.flock(descriptor, fcntl.LOCK_UN)
        finally:
            os.close(descriptor)

    def directory(self, request_uuid: str) -> Path:
        return self.root / str(uuid.UUID(request_uuid))

    def open_or_create(
        self,
        request_uuid: str,
        *,
        canonical_digest: str,
        create_body: dict[str, Any],
        archive: bytes,
    ) -> RunRecord:
        """Return the record for this request, creating it (and its archive) on first use."""
        with self.lock(request_uuid):
            key = str(uuid.UUID(request_uuid))
            if (self.directory(key) / _RECORD_NAME).exists():
                record = self.load(key)
                if record.canonical_digest != canonical_digest:
                    raise RecordConflict(
                        f"request {key} was first submitted with different content; "
                        "use a new request UUID for changed content"
                    )
                return record
            self._make_directory(self.root)
            directory = self.directory(key)
            self._make_directory(directory)
            _write_private(directory / _ARCHIVE_NAME, archive)
            record = RunRecord(
                request_uuid=key,
                canonical_digest=canonical_digest,
                create_body=create_body,
                archive_sha256=_sha256(archive),
                archive_size=len(archive),
            )
            self.save(record)
            return record

    def load(self, request_uuid: str) -> RunRecord:
        path = self.directory(request_uuid) / _RECORD_NAME
        return RunRecord(**json.loads(path.read_text(encoding="utf-8")))

    def save(self, record: RunRecord) -> None:
        with self.lock(record.request_uuid):
            directory = self.directory(record.request_uuid)
            payload = json.dumps(asdict(record), sort_keys=True, indent=2).encode("utf-8") + b"\n"
            _write_private(directory / _RECORD_NAME, payload)

    def archive(self, record: RunRecord) -> bytes:
        return (self.directory(record.request_uuid) / _ARCHIVE_NAME).read_bytes()

    def _make_directory(self, path: Path) -> None:
        missing: list[Path] = []
        current = path
        while not current.exists():
            missing.append(current)
            current = current.parent
        for directory in reversed(missing):
            try:
                directory.mkdir(mode=_DIR_MODE)
            except FileExistsError:
                pass
            os.chmod(directory, _DIR_MODE)
            _sync_directory(directory)
            _sync_directory(directory.parent)
        os.chmod(path, _DIR_MODE)
        _sync_directory(path)


def _write_private(path: Path, data: bytes) -> None:
    """Replace path atomically with data, owner read-write only."""
    descriptor, temporary = tempfile.mkstemp(dir=path.parent, prefix=f".{path.name}.")
    try:
        with os.fdopen(descriptor, "wb") as handle:
            handle.write(data)
            handle.flush()
            os.fsync(handle.fileno())
        os.chmod(temporary, _FILE_MODE)
        os.replace(temporary, path)
        _sync_directory(path.parent)
    except BaseException:
        if os.path.exists(temporary):
            os.unlink(temporary)
        raise


def _sync_directory(path: Path) -> None:
    descriptor = os.open(path, os.O_RDONLY | os.O_DIRECTORY)
    try:
        os.fsync(descriptor)
    finally:
        os.close(descriptor)


def _sha256(data: bytes) -> str:
    return hashlib.sha256(data).hexdigest()
