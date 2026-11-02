#!/usr/bin/env python3
"""fm-audit-writer.py - durable, sequenced, hash-chained store for audit envelope v1 records.

docs/audit-writer.md owns the store layout, commit point, and recovery contract.
docs/audit-envelope.md owns the record contract. Every record is checked with
bin/fm-audit-envelope.py's verify(), which this module imports instead of redefining.

Usage:
  fm-audit-writer.py append --store DIR [--segment-records N] PATH|-
  fm-audit-writer.py verify --store DIR
  fm-audit-writer.py project-ledger --store DIR

append takes one envelope without seq or prev_hash; the writer assigns both. It prints
one JSON result line. Exit status for append: 0 committed, 1 envelope rejected and
recorded, 2 usage or input error or unrecorded rejection, 3 not committed. On 3 the
caller must not report the event as recorded, and "may_be_durable" true means the record
may already be on disk.
verify walks the whole store read-only and never repairs: 0 intact, 1 corrupt, 2 usage.
A torn unterminated tail is reported under "torn_tail" and still exits 0.
project-ledger prints fleet-ledger v1 base records for committed envelopes, one per line:
0 printed, 1 store refused, 2 usage.
"""

import argparse
import calendar
import errno
import fcntl
import hashlib
import importlib.util
import json
import os
import re
import sys
import time
from collections import namedtuple
from contextlib import contextmanager, suppress
from pathlib import Path

HERE = Path(__file__).resolve().parent
_ENVELOPE_SPEC = importlib.util.spec_from_file_location(
    "fm_audit_envelope", HERE / "fm-audit-envelope.py")
ENV = importlib.util.module_from_spec(_ENVELOPE_SPEC)
_ENVELOPE_SPEC.loader.exec_module(ENV)

SEGMENT_RECORDS = 1000
CHECKPOINT = "checkpoint.json"
CHECKPOINT_TMP = "checkpoint.json.tmp"
CHECKPOINT_KEYS = frozenset({"v", "last_seq", "last_hash", "segment"})
LOCK = ".lock"
SEGMENT_RE = re.compile(r"segment-([0-9]{20})\.jsonl")
HASH_RE = re.compile(r"sha256:[0-9a-f]{64}")
LEDGER_V1_EVENTS = frozenset({
    "task.dispatched", "task.status", "task.pr_ready", "task.merged", "task.cleaned_up",
})
PARSE_ERRORS = (ValueError, ENV.UsageError)

Checkpoint = namedtuple("Checkpoint", ["last_seq", "last_hash", "segment"])
GENESIS = Checkpoint(-1, None, None)


class StoreRefused(Exception):
    """The store cannot be trusted as history, so nothing is repaired or appended."""

    def __init__(self, reason, seq=None, segment=None, paths=None):
        super().__init__(reason)
        self.reason = reason
        self.seq = seq
        self.segment = segment
        self.paths = list(paths or [])
        self.intact = []


class Rejected(Exception):
    """The envelope does not conform to the v1 contract, so nothing is written."""

    def __init__(self, reason, paths):
        super().__init__(reason)
        self.reason = reason
        self.paths = paths


class WriteFailed(Exception):
    """An append did not commit. may_be_durable True means the record may already be on disk."""

    def __init__(self, may_be_durable):
        super().__init__("append not committed")
        self.may_be_durable = may_be_durable


class _CommitOutcomeUnknown(Exception):
    """The checkpoint was replaced, but the directory sync that makes it durable failed."""


def canonical(value):
    return json.dumps(value, sort_keys=True, separators=(",", ":"),
                      allow_nan=False).encode("ascii")


def digest(line):
    """The prev_hash of the record after a line: sha256 of the stored line without newline."""
    return "sha256:" + hashlib.sha256(line).hexdigest()


def segment_name(seq):
    return f"segment-{seq:020d}.jsonl"


def first_seq_of(name):
    return int(SEGMENT_RE.fullmatch(name).group(1))


def sync_fd(fd):
    """Flush fd to stable storage. F_FULLFSYNC on macOS also flushes the drive cache."""
    full = getattr(fcntl, "F_FULLFSYNC", None)
    if full is None:
        os.fsync(fd)
        return
    try:
        fcntl.fcntl(fd, full)
    except OSError as exc:
        if exc.errno not in (errno.ENOTSUP, errno.EINVAL, errno.ENOTTY):
            raise
        os.fsync(fd)


def sync_dir(path):
    fd = os.open(path, os.O_RDONLY)
    try:
        sync_fd(fd)
    finally:
        os.close(fd)


def write_all(fd, data):
    view = memoryview(data)
    while view:
        view = view[os.write(fd, view):]


def truncate_durably(path, size):
    fd = os.open(path, os.O_WRONLY)
    try:
        os.ftruncate(fd, size)
        sync_fd(fd)
    finally:
        os.close(fd)


def rollback(path, size):
    """Truncate a failed append back to size. True only when the truncation is durable."""
    try:
        truncate_durably(path, size)
    except OSError:
        return False
    return True


def require_dir(directory):
    if not directory.is_dir():
        raise ENV.UsageError("store directory does not exist")


def split_segment(raw):
    """Return the newline-terminated lines of one segment and its unterminated tail bytes."""
    cut = raw.rfind(b"\n") + 1
    return raw[:cut].split(b"\n")[:-1], raw[cut:]


def parse_record(line, segment, seq):
    try:
        doc = json.loads(line.decode("utf-8"), object_pairs_hook=ENV.no_duplicate_keys,
                         parse_constant=ENV.reject_constant)
    except PARSE_ERRORS:
        raise StoreRefused("malformed_line", seq=seq, segment=segment) from None
    if not isinstance(doc, dict) or canonical(doc) != line:
        raise StoreRefused("malformed_line", seq=seq, segment=segment)
    return doc


def checkpoint_fields_ok(doc):
    seq, last_hash, segment = doc["last_seq"], doc["last_hash"], doc["segment"]
    if type(doc["v"]) is not int or doc["v"] != 1 or type(seq) is not int or seq < -1:
        return False
    if seq == -1:
        return last_hash is None and segment is None
    return (isinstance(last_hash, str) and HASH_RE.fullmatch(last_hash) is not None
            and isinstance(segment, str) and SEGMENT_RE.fullmatch(segment) is not None)


def parse_checkpoint(raw):
    try:
        doc = json.loads(raw.decode("utf-8"), object_pairs_hook=ENV.no_duplicate_keys,
                         parse_constant=ENV.reject_constant)
    except PARSE_ERRORS:
        raise StoreRefused("checkpoint_invalid", segment=CHECKPOINT) from None
    if (not isinstance(doc, dict) or set(doc) != CHECKPOINT_KEYS
            or canonical(doc) + b"\n" != raw or not checkpoint_fields_ok(doc)):
        raise StoreRefused("checkpoint_invalid", segment=CHECKPOINT)
    return Checkpoint(doc["last_seq"], doc["last_hash"], doc["segment"])


def input_verdict(document, schema):
    """Check the caller's envelope before any store is touched.

    The writer assigns seq and prev_hash, so a caller must not supply them. No other
    member depends on the chain position, so a placeholder genesis position runs the full
    v1 check here, and the assigned position is consistent with it by construction.
    """
    if isinstance(document, dict):
        for member in ("seq", "prev_hash"):
            if member in document:
                return "schema_invalid", ["/" + member]
        document = dict(document, seq=0, prev_hash=None)
    return ENV.verify(document, "envelope", schema)


def ledger_v1_record(doc):
    """Project a committed envelope to fleet-ledger v1 base members, or None for other events.

    Members the envelope does not carry (kind, project, state, key, text, via) are not
    reconstructed. v1 readers require only the base members and ignore the rest.
    """
    if doc["event"] not in LEDGER_V1_EVENTS or "task_id" not in doc:
        return None
    record = {
        "v": 1,
        "ts": calendar.timegm(time.strptime(doc["ts"][:19], "%Y-%m-%dT%H:%M:%S")),
        "event": doc["event"],
        "task": doc["task_id"],
    }
    if doc.get("pr") is not None:
        record["pr"] = doc["pr"]
    return record


class Walk:
    """One read-only pass over every segment and the checkpoint."""

    def __init__(self, ckpt, present):
        self.ckpt = ckpt
        self.present = present
        self.last_seq = -1
        self.last_hash = None
        self.last_segment = None
        self.ahead = 0
        self.checkpoint_seen = False
        self.torn = None
        self.listing = []
        self.records = []


class AuditStore:
    """One hash-chained store directory. Mutating calls hold the store's exclusive lock."""

    def __init__(self, directory, segment_records=SEGMENT_RECORDS):
        self.dir = Path(directory)
        self.segment_records = segment_records
        self.schema = json.loads(ENV.SCHEMA_PATH.read_text(encoding="utf-8"))

    @contextmanager
    def _locked(self, shared=False):
        fd = os.open(self.dir / LOCK, os.O_RDWR | os.O_CREAT, 0o600)
        try:
            fcntl.flock(fd, fcntl.LOCK_SH if shared else fcntl.LOCK_EX)
            yield
        finally:
            os.close(fd)

    def append(self, envelope):
        verdict = input_verdict(envelope, self.schema)
        if verdict is not None:
            raise Rejected(*verdict)
        self.dir.mkdir(mode=0o700, parents=True, exist_ok=True)
        with self._locked():
            state, recovered = self._repair()
            record = dict(envelope, seq=state.last_seq + 1, prev_hash=state.last_hash)
            self._commit(record, state)
        result = {"result": "committed", "seq": record["seq"], "event_id": record["event_id"]}
        if recovered:
            result["recovered"] = recovered
        return result

    def verify(self):
        require_dir(self.dir)
        with self._locked(shared=True):
            walk = self._walk()
        torn = walk.torn
        return {
            "result": "verified",
            "records": walk.last_seq + 1,
            "committed_seq": walk.ckpt.last_seq,
            "last_seq": walk.last_seq,
            "segments": walk.listing,
            "torn_tail": None if torn is None else {"segment": torn["segment"],
                                                    "bytes": torn["bytes"]},
            "uncommitted_seq": walk.last_seq if walk.ahead else None,
        }

    def ledger_v1_lines(self):
        require_dir(self.dir)
        with self._locked(shared=True):
            walk = self._walk()
        lines = []
        for doc in walk.records:
            if doc["seq"] > walk.ckpt.last_seq:
                continue
            projected = ledger_v1_record(doc)
            if projected is None:
                continue
            if ENV.verify(projected, "fleet_ledger_v1", self.schema) is not None:
                raise StoreRefused("ledger_projection_invalid")
            lines.append(canonical(projected).decode("ascii"))
        return lines

    def _repair(self):
        """Under the exclusive lock: verify, truncate a torn tail, adopt one ahead record."""
        walk = self._walk()
        report = {}
        state = Checkpoint(walk.last_seq, walk.last_hash, walk.last_segment)
        try:
            if not walk.present:
                self._publish_checkpoint(GENESIS)
            if walk.torn is not None:
                truncate_durably(self.dir / walk.torn["segment"], walk.torn["good_size"])
                report["torn_tail"] = {"segment": walk.torn["segment"],
                                       "bytes": walk.torn["bytes"]}
            if walk.ahead == 1:
                self._publish_checkpoint(state)
                report["adopted"] = {"seq": state.last_seq, "segment": state.segment}
        except _CommitOutcomeUnknown as exc:
            # Repair writes no record for this call, so the caller's record is not on disk.
            raise WriteFailed(may_be_durable=False) from exc
        return state, report

    def _commit(self, record, state):
        seq = record["seq"]
        line = canonical(record) + b"\n"
        rotate = state.segment is None or seq - first_seq_of(state.segment) >= self.segment_records
        name = segment_name(seq) if rotate else state.segment
        path = self.dir / name
        before = None
        try:
            fd = os.open(path, os.O_WRONLY | os.O_APPEND | os.O_CREAT, 0o600)
            try:
                before = os.fstat(fd).st_size
                write_all(fd, line)
                sync_fd(fd)
            finally:
                os.close(fd)
            if rotate:
                sync_dir(self.dir)
        except OSError as exc:
            raise WriteFailed(
                may_be_durable=before is not None and not rollback(path, before)) from exc
        try:
            self._publish_checkpoint(Checkpoint(seq, digest(line[:-1]), name))
        except _CommitOutcomeUnknown as exc:
            raise WriteFailed(may_be_durable=True) from exc
        except OSError as exc:
            raise WriteFailed(may_be_durable=not rollback(path, before)) from exc

    def _publish_checkpoint(self, state):
        """Replace the checkpoint atomically. The replace is the commit point."""
        data = canonical({"v": 1, "last_seq": state.last_seq, "last_hash": state.last_hash,
                          "segment": state.segment}) + b"\n"
        tmp = self.dir / CHECKPOINT_TMP
        try:
            fd = os.open(tmp, os.O_WRONLY | os.O_CREAT | os.O_TRUNC, 0o600)
            try:
                write_all(fd, data)
                sync_fd(fd)
            finally:
                os.close(fd)
            os.replace(tmp, self.dir / CHECKPOINT)
        except OSError:
            with suppress(OSError):
                os.unlink(tmp)
            raise
        try:
            sync_dir(self.dir)
        except OSError as exc:
            raise _CommitOutcomeUnknown() from exc

    def _segments(self):
        found = []
        for entry in os.scandir(self.dir):
            if SEGMENT_RE.fullmatch(entry.name) is None:
                continue
            if not entry.is_file(follow_symlinks=False):
                raise StoreRefused("segment_invalid", segment=entry.name)
            found.append((first_seq_of(entry.name), entry.name))
        return sorted(found)

    def _checkpoint(self, segments):
        try:
            raw = (self.dir / CHECKPOINT).read_bytes()
        except FileNotFoundError:
            if segments:
                raise StoreRefused("checkpoint_missing", segment=CHECKPOINT) from None
            return GENESIS, False
        return parse_checkpoint(raw), True

    def _walk(self):
        """Verify every segment and the checkpoint. Raises StoreRefused with .intact set."""
        segments = self._segments()
        ckpt, present = self._checkpoint(segments)
        walk = Walk(ckpt, present)
        damage = None
        next_seq, link = 0, None
        last_index = len(segments) - 1
        for index, (first, name) in enumerate(segments):
            raw = (self.dir / name).read_bytes()
            lines, tail = split_segment(raw)
            entry = {"name": name, "records": 0, "status": "intact"}
            walk.listing.append(entry)
            try:
                self._scan_segment(walk, entry, first, raw, lines, tail,
                                   index == last_index, next_seq, link)
            except StoreRefused as exc:
                entry["status"] = "damaged"
                if damage is None:
                    damage = exc
            next_seq = first + len(lines)
            link = digest(lines[-1]) if lines else None
        if damage is None:
            damage = self._checkpoint_refusal(walk)
        if damage is not None:
            damage.intact = [e["name"] for e in walk.listing if e["status"] == "intact"]
            raise damage
        return walk

    def _scan_segment(self, walk, entry, first, raw, lines, tail, last, next_seq, link):
        name = entry["name"]
        if first != next_seq:
            raise StoreRefused("segment_gap", seq=next_seq, segment=name)
        if tail and not last:
            raise StoreRefused("unterminated_record", seq=first + len(lines), segment=name)
        if not lines and not last:
            raise StoreRefused("segment_empty", seq=first, segment=name)
        if tail:
            walk.torn = {"segment": name, "good_size": len(raw) - len(tail),
                         "bytes": len(tail)}
        for offset, line in enumerate(lines):
            seq = first + offset
            doc = parse_record(line, name, seq)
            if type(doc.get("seq")) is not int or doc["seq"] != seq:
                raise StoreRefused("seq_mismatch", seq=seq, segment=name, paths=["/seq"])
            if seq == 0:
                expected = None
            elif offset == 0:
                expected = link
            else:
                expected = digest(lines[offset - 1])
            if doc.get("prev_hash") != expected:
                raise StoreRefused("hash_mismatch", seq=seq, segment=name, paths=["/prev_hash"])
            verdict = ENV.verify(doc, "envelope", self.schema)
            if verdict is not None:
                raise StoreRefused(verdict[0], seq=seq, segment=name, paths=verdict[1])
            walk.last_seq, walk.last_hash, walk.last_segment = seq, digest(line), name
            walk.records.append(doc)
            entry["records"] += 1
            if seq == walk.ckpt.last_seq:
                if digest(line) != walk.ckpt.last_hash or name != walk.ckpt.segment:
                    raise StoreRefused("checkpoint_mismatch", seq=seq, segment=name)
                walk.checkpoint_seen = True
            elif seq > walk.ckpt.last_seq:
                walk.ahead += 1

    @staticmethod
    def _checkpoint_refusal(walk):
        ckpt = walk.ckpt
        if ckpt.last_seq >= 0 and not walk.checkpoint_seen:
            return StoreRefused("checkpoint_mismatch", seq=ckpt.last_seq, segment=ckpt.segment)
        if walk.ahead > 1:
            return StoreRefused("checkpoint_mismatch", seq=walk.last_seq,
                                segment=walk.last_segment)
        return None


def refusal_fields(exc):
    return {"reason": exc.reason, "seq": exc.seq, "segment": exc.segment,
            "paths": exc.paths, "intact_segments": exc.intact}


def emit(obj):
    print(json.dumps(obj, sort_keys=True))


def record_rejection(rejected):
    result = {"result": "rejected", "definition": "envelope",
              "reason": rejected.reason, "paths": rejected.paths}
    try:
        ENV.record_rejection(result)
    except ENV.UsageError as exc:
        raise ENV.UsageError(f"rejection not recorded: {exc}") from None
    except OSError as exc:
        raise ENV.UsageError(f"rejection not recorded: {type(exc).__name__}") from None
    emit(result)
    return 1


def run_append(args):
    if args.segment_records < 1:
        raise ENV.UsageError("--segment-records must be at least 1")
    document = ENV.load_document(args.input)
    store = AuditStore(args.store, args.segment_records)
    try:
        result = store.append(document)
    except Rejected as exc:
        return record_rejection(exc)
    except StoreRefused as exc:
        emit({"result": "refused", **refusal_fields(exc)})
        return 3
    except WriteFailed as exc:
        emit({"result": "failed", "may_be_durable": exc.may_be_durable})
        return 3
    except OSError as exc:
        emit({"result": "failed", "may_be_durable": False, "error": type(exc).__name__})
        return 3
    emit(result)
    return 0


def run_verify(args):
    store = AuditStore(args.store)
    try:
        result = store.verify()
    except StoreRefused as exc:
        emit({"result": "corrupt", **refusal_fields(exc)})
        return 1
    emit(result)
    return 0


def run_project_ledger(args):
    store = AuditStore(args.store)
    try:
        lines = store.ledger_v1_lines()
    except StoreRefused as exc:
        emit({"result": "refused", **refusal_fields(exc)})
        return 1
    for line in lines:
        print(line)
    return 0


def build_parser():
    parser = argparse.ArgumentParser(prog="fm-audit-writer.py")
    sub = parser.add_subparsers(dest="command", required=True)
    append = sub.add_parser("append")
    append.add_argument("--store", required=True)
    append.add_argument("--segment-records", type=int, default=SEGMENT_RECORDS)
    append.add_argument("input", metavar="PATH|-")
    verify = sub.add_parser("verify")
    verify.add_argument("--store", required=True)
    ledger = sub.add_parser("project-ledger")
    ledger.add_argument("--store", required=True)
    return parser


def main(argv):
    args = build_parser().parse_args(argv)
    try:
        if args.command == "append":
            return run_append(args)
        if args.command == "verify":
            return run_verify(args)
        return run_project_ledger(args)
    except ENV.UsageError as exc:
        print(f"fm-audit-writer: {exc}", file=sys.stderr)
        return 2
    except ENV.SchemaError as exc:
        print(f"fm-audit-writer: schema cannot be evaluated: {exc}", file=sys.stderr)
        return 2
    except OSError as exc:
        print(f"fm-audit-writer: cannot read store: {type(exc).__name__}", file=sys.stderr)
        return 2


if __name__ == "__main__":
    sys.exit(main(sys.argv[1:]))
