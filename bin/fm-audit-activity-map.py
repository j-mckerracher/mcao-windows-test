#!/usr/bin/env python3
"""fm-audit-activity-map.py - project one audit envelope v1 record to a SIEM audit.activity event.

docs/siem-event-format.md in mc-innersource/agentic-workflow-data-api (DPES-788) owns the event:
section 3.1 is the audit.activity event and section 4 is its Cloud Logging LogEntry. This script
implements that projection. docs/audit-envelope.md owns the input record, which is checked by
bin/fm-audit-envelope.py's verify(). Redaction belongs to docs/audit-sanitization.md and
bin/fm-audit-sanitize.py: this script runs that boundary over the projected event and defines no
redaction of its own.

Usage:
  fm-audit-activity-map.py map --config PATH PATH|-

Reads the closed sanitizer config from PATH (the file fm-audit-sanitize.py reads) and one envelope
record from PATH, or from stdin for "-". Prints one JSON result line:
  {"result": "mapped", "changed": [...], "event": {...}, "log_entry": {...}}
  {"result": "refused", "reason": ..., "paths": [...]}

changed lists the JSON Pointer of each member the boundary redacted or cut to 256 characters.
The event carries the boundary's output for that member, never the raw value. A member the
boundary drops refuses the whole record, because section 3.1 has no way to omit a member.
A refusal carries a fixed reason and JSON Pointers only, and it never emits a partial event.

Exit status: 0 mapped, 1 refused, 2 usage or input error. The script writes nothing.
"""

import importlib.util
import json
import sys
from pathlib import Path

HERE = Path(__file__).resolve().parent
# The script writes nothing, so the sibling modules it loads must not write bytecode caches either.
sys.dont_write_bytecode = True


def load_sibling(module_name, filename):
    spec = importlib.util.spec_from_file_location(module_name, HERE / filename)
    module = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(module)
    return module


ENV = load_sibling("fm_audit_envelope", "fm-audit-envelope.py")
SAN = load_sibling("fm_audit_sanitize", "fm-audit-sanitize.py")

USAGE = "usage: fm-audit-activity-map.py map --config PATH PATH|-"
EVENT_SCHEMA = "mcao.siem.event/v1"
EVENT_TYPE = "audit.activity"
SOURCE_ROUTE = "mcao.audit-envelope.v1"
LOG_NAME = "projects/dpe-platform-d/logs/mcao-audit"
RESOURCE_TYPE = "cloud_run_revision"


class Refused(Exception):
    """The record cannot be projected; paths names members by JSON Pointer, never by value."""

    def __init__(self, reason, paths):
        super().__init__(reason)
        self.reason = reason
        self.paths = paths


def project(record, schema):
    """Return the section 3.1 event for a record that the envelope validator accepts.

    Only the members named in the section 3.1 table are copied. The validator runs first, so
    every copied member has already been checked against its schema.
    """
    verdict = ENV.verify(record, "envelope", schema)
    if verdict is not None:
        raise Refused(*verdict)
    agent, actor, user = record["agent"], record["actor"], record["user"]
    return {
        "schema": EVENT_SCHEMA,
        "event_type": EVENT_TYPE,
        "event_id": "audit.activity:" + record["event_id"],
        "occurred_at_utc": record["ts"],
        "source_route": SOURCE_ROUTE,
        "lineage": {
            "event_id": record["event_id"],
            "seq": record["seq"],
            "prev_hash": record["prev_hash"],
        },
        "data": {
            "event": record["event"],
            "category": record["category"],
            "outcome": record["outcome"],
            "platform": record["platform"],
            "home_id": record["home_id"],
            "day_key": record["day_key"],
            "session_id": record["session_id"],
            # section 3.1: null when the envelope omits task_id, which the validator allows.
            "task_id": record.get("task_id"),
            "agent": {"role": agent["role"], "harness": agent["harness"], "model": agent["model"]},
            "actor": {"type": actor["type"], "id": actor["id"]},
            "repo": record["repo"],
            "pr": record["pr"],
            "commit": record["commit"],
            "user": {"id": user["id"], "identity_source": user["identity_source"]},
        },
    }


def differing_pointers(before, after, base=""):
    """Return the JSON Pointer of each leaf the boundary changed, matched by key and never by content."""
    if isinstance(before, dict) and isinstance(after, dict):
        changed = []
        for key, value in before.items():
            pointer = ENV.pointer(base, key)
            if key in after:
                changed.extend(differing_pointers(value, after[key], pointer))
            else:
                changed.append(pointer)
        return changed
    return [] if before == after else [base]


def sanitize(event, home_key):
    """Run the DPES-737 boundary over the projected event and return (sanitized event, changed pointers)."""
    sanitizer = SAN.Sanitizer(home_key)
    sanitized = sanitizer.event(event)
    if sanitizer.dropped:
        raise Refused("sanitization_dropped", list(sanitizer.dropped))
    return sanitized, differing_pointers(event, sanitized)


def log_entry(event):
    """Return the section 4 Cloud Logging LogEntry that wraps one audit.activity event."""
    return {
        "logName": LOG_NAME,
        "resource": {"type": RESOURCE_TYPE},
        "jsonPayload": event,
        "timestamp": event["occurred_at_utc"],
        "insertId": event["event_id"],
        "severity": "DEFAULT",
    }


def map_record(record, home_key, schema):
    event = project(record, schema)
    sanitized, changed = sanitize(event, home_key)
    return {
        "result": "mapped",
        "changed": changed,
        "event": sanitized,
        "log_entry": log_entry(sanitized),
    }


def parse_args(argv):
    if argv in (["-h"], ["--help"]):
        return None
    if len(argv) != 4 or argv[0] != "map" or argv[1] != "--config":
        raise ENV.UsageError(USAGE)
    if argv[2] == "-":
        raise ENV.UsageError("the config must be a file path, not stdin")
    return argv[2], argv[3]


def refuse(reason, paths):
    print(json.dumps({"result": "refused", "reason": reason, "paths": paths}, sort_keys=True))
    return 1


def main(argv):
    try:
        parsed = parse_args(argv)
        if parsed is None:
            print(USAGE)
            return 0
        config_path, record_path = parsed
        try:
            home_key = SAN.load_home_key(config_path)
        except SAN.ConfigRefused as refusal:
            return refuse("config_invalid", refusal.paths)
        record = ENV.load_document(record_path)
        schema = json.loads(ENV.SCHEMA_PATH.read_text(encoding="utf-8"))
        result = map_record(record, home_key, schema)
    except ENV.UsageError as exc:
        print(f"fm-audit-activity-map: {exc}", file=sys.stderr)
        print(USAGE, file=sys.stderr)
        return 2
    except ENV.SchemaError as exc:
        print(f"fm-audit-activity-map: schema cannot be evaluated: {exc}", file=sys.stderr)
        return 2
    except Refused as refusal:
        return refuse(refusal.reason, refusal.paths)
    print(json.dumps(result, sort_keys=True))
    return 0


if __name__ == "__main__":
    sys.exit(main(sys.argv[1:]))
