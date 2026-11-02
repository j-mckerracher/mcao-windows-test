#!/usr/bin/env python3
"""fm-audit-envelope.py - validate audit envelope v1 and fleet ledger v1 records.

docs/audit-envelope.md owns the envelope contract, docs/audit-envelope.v1.schema.json
is its machine-readable form, and docs/fleet-ledger.md owns the ledger record contract.

Usage:
  fm-audit-envelope.py validate [--definition envelope|fleet_ledger_v1] PATH|-

Reads one JSON document from PATH, or from stdin for "-", and prints one JSON result line:
  {"result": "accepted", ...} or {"result": "rejected", "reason": ..., "paths": [...], ...}

Exit status: 0 accepted, 1 rejected and recorded, 2 usage, input, or unrecorded-rejection error.
A rejection is appended as one sanitized line to audit-rejections.jsonl under FM_STATE_OVERRIDE,
or else FM_HOME/state. Accepted documents write nothing, and output never echoes input values.

The schema is evaluated by a small stdlib-only evaluator, because the repo has no JSON
Schema dependency. It implements only the keywords the schema uses and fails closed on
anything else: an unknown keyword or an unanchored pattern is a usage-level error (exit 2),
never a pass. Patterns are anchored `^...$` and match the whole string.
"""

import json
import os
import re
import sys
import time
from datetime import datetime, timezone
from pathlib import Path
from zoneinfo import ZoneInfo

SCHEMA_PATH = Path(__file__).resolve().parent.parent / "docs" / "audit-envelope.v1.schema.json"
DEFINITIONS = {
    "envelope": None,
    "fleet_ledger_v1": {"$ref": "#/$defs/fleet_ledger_v1_record"},
}
USAGE = "usage: fm-audit-envelope.py validate [--definition envelope|fleet_ledger_v1] PATH|-"
REJECTIONS_FILE = "audit-rejections.jsonl"
PENDING_USER_ID = "PENDING"
ANNOTATIONS = {"$schema", "$id", "title", "description", "$defs"}
VALIDATING = {"$ref", "type", "const", "enum", "pattern", "minLength", "minimum", "required",
              "properties", "additionalProperties"}


class SchemaError(Exception):
    """The schema uses a construct this validator does not implement."""


class UsageError(Exception):
    """The command line or the input document cannot be processed."""


def pointer(base, key):
    return base + "/" + str(key).replace("~", "~0").replace("/", "~1")


def same(left, right):
    if isinstance(left, bool) or isinstance(right, bool):
        return type(left) is type(right) and left == right
    return left == right


def is_number(value):
    return isinstance(value, (int, float)) and not isinstance(value, bool)


def is_type(name, value):
    if name == "integer":
        return isinstance(value, int) and not isinstance(value, bool)
    if name == "number":
        return is_number(value)
    if name == "string":
        return isinstance(value, str)
    if name == "boolean":
        return isinstance(value, bool)
    if name == "null":
        return value is None
    if name == "object":
        return isinstance(value, dict)
    if name == "array":
        return isinstance(value, list)
    raise SchemaError("unknown type name")


def fullmatch(pattern, value):
    if len(pattern) < 2 or pattern[0] != "^" or pattern[-1] != "$" or pattern.endswith("\\$"):
        raise SchemaError("pattern must be anchored with ^ and $")
    return re.fullmatch(pattern[1:-1], value) is not None


def resolve_ref(root, ref):
    if not ref.startswith("#/"):
        raise SchemaError("only local references are supported")
    node = root
    for part in ref[2:].split("/"):
        part = part.replace("~1", "/").replace("~0", "~")
        if not isinstance(node, dict) or part not in node:
            raise SchemaError("unresolvable reference")
        node = node[part]
    return node


def evaluate(schema, value, root, ptr, errors):
    """Append the JSON Pointer of every failing member to errors; never a value."""
    if not isinstance(schema, dict):
        raise SchemaError("schema node is not an object")
    unknown = set(schema) - VALIDATING - ANNOTATIONS
    if unknown:
        raise SchemaError("unsupported schema keyword: " + sorted(unknown)[0])
    additional = schema.get("additionalProperties", True)
    if additional is not True and additional is not False:
        raise SchemaError("only boolean additionalProperties is supported")
    if "$ref" in schema:
        evaluate(resolve_ref(root, schema["$ref"]), value, root, ptr, errors)
    if "type" in schema:
        names = schema["type"] if isinstance(schema["type"], list) else [schema["type"]]
        if not any(is_type(name, value) for name in names):
            errors.append(ptr)
            return
    if "const" in schema and not same(value, schema["const"]):
        errors.append(ptr)
    if "enum" in schema and not any(same(value, option) for option in schema["enum"]):
        errors.append(ptr)
    if isinstance(value, str):
        if "minLength" in schema and len(value) < schema["minLength"]:
            errors.append(ptr)
        if "pattern" in schema and not fullmatch(schema["pattern"], value):
            errors.append(ptr)
    if is_number(value) and "minimum" in schema and value < schema["minimum"]:
        errors.append(ptr)
    if isinstance(value, dict):
        properties = schema.get("properties", {})
        for name in schema.get("required", []):
            if name not in value:
                errors.append(pointer(ptr, name))
        for key, member in value.items():
            if key in properties:
                evaluate(properties[key], member, root, pointer(ptr, key), errors)
        if additional is False and any(key not in properties for key in value):
            errors.append(ptr)


def identity_gaps(doc):
    """Pointers of unresolved identity members; empty when the identity resolves."""
    user = doc.get("user")
    if not isinstance(user, dict):
        return ["/user"]
    gaps = []
    if user.get("id") in (None, PENDING_USER_ID):
        gaps.append("/user/id")
    if user.get("identity_source") is None:
        gaps.append("/user/identity_source")
    return gaps


def verify(doc, definition, schema):
    """Return None when accepted, else a (reason, paths) rejection."""
    if definition == "envelope" and isinstance(doc, dict):
        gaps = identity_gaps(doc)
        if gaps:
            return "identity_unresolved", gaps
    target = schema if definition == "envelope" else DEFINITIONS[definition]
    errors = []
    evaluate(target, doc, schema, "", errors)
    if errors:
        return "schema_invalid", sorted(set(errors))
    if definition != "envelope":
        return None
    if (doc["seq"] == 0) != (doc["prev_hash"] is None):
        return "schema_invalid", ["/prev_hash"]
    if doc["actor"]["type"] == "user" and doc["actor"]["id"] != doc["user"]["id"]:
        return "schema_invalid", ["/actor/id"]
    try:
        zone = ZoneInfo(doc["user"]["timezone"])
    except (LookupError, ValueError, OSError):
        return "schema_invalid", ["/user/timezone"]
    try:
        when = datetime.strptime(doc["ts"], "%Y-%m-%dT%H:%M:%S.%fZ").replace(tzinfo=timezone.utc)
        local_day = when.astimezone(zone).date().isoformat()
    except (ValueError, OverflowError):
        return "schema_invalid", ["/ts"]
    if local_day != doc["day_key"]:
        return "day_key_mismatch", ["/day_key"]
    return None


def rejection_log_path():
    state = os.environ.get("FM_STATE_OVERRIDE")
    if not state and os.environ.get("FM_HOME"):
        state = os.path.join(os.environ["FM_HOME"], "state")
    if not state:
        raise UsageError("set FM_HOME or FM_STATE_OVERRIDE so the rejection can be recorded")
    return Path(state) / REJECTIONS_FILE


def record_rejection(result):
    record = {"v": 1, "rejected_at": int(time.time()), "definition": result["definition"],
              "reason": result["reason"], "paths": result["paths"]}
    with rejection_log_path().open("a", encoding="utf-8") as handle:
        handle.write(json.dumps(record, sort_keys=True) + "\n")


def reject_constant(name):
    raise UsageError("input contains a non-finite number")


def no_duplicate_keys(pairs):
    obj = {}
    for key, value in pairs:
        if key in obj:
            raise UsageError("input contains a duplicate object key")
        obj[key] = value
    return obj


def load_document(path):
    try:
        text = sys.stdin.read() if path == "-" else Path(path).read_text(encoding="utf-8")
    except (OSError, UnicodeDecodeError) as exc:
        raise UsageError("cannot read input: " + type(exc).__name__) from None
    try:
        return json.loads(text, object_pairs_hook=no_duplicate_keys, parse_constant=reject_constant)
    except json.JSONDecodeError:
        raise UsageError("input is not valid JSON") from None


def parse_args(argv):
    if argv in (["-h"], ["--help"]):
        return None
    if not argv or argv[0] != "validate":
        raise UsageError(USAGE)
    rest = argv[1:]
    definition = "envelope"
    if rest[:1] == ["--definition"]:
        if len(rest) < 2 or rest[1] not in DEFINITIONS:
            raise UsageError(USAGE)
        definition, rest = rest[1], rest[2:]
    if len(rest) != 1:
        raise UsageError(USAGE)
    return definition, rest[0]


def main(argv):
    try:
        parsed = parse_args(argv)
        if parsed is None:
            print(USAGE)
            return 0
        definition, path = parsed
        doc = load_document(path)
        schema = json.loads(SCHEMA_PATH.read_text(encoding="utf-8"))
        verdict = verify(doc, definition, schema)
    except UsageError as exc:
        print(f"fm-audit-envelope: {exc}", file=sys.stderr)
        print(USAGE, file=sys.stderr)
        return 2
    except SchemaError as exc:
        print(f"fm-audit-envelope: schema cannot be evaluated: {exc}", file=sys.stderr)
        return 2
    if verdict is None:
        print(json.dumps({"result": "accepted", "definition": definition}))
        return 0
    reason, paths = verdict
    result = {"result": "rejected", "definition": definition, "reason": reason, "paths": paths}
    try:
        record_rejection(result)
    except UsageError as exc:
        print(f"fm-audit-envelope: rejection not recorded: {exc}", file=sys.stderr)
        return 2
    except OSError as exc:
        print(f"fm-audit-envelope: rejection not recorded: {type(exc).__name__}", file=sys.stderr)
        return 2
    print(json.dumps(result))
    return 1


if __name__ == "__main__":
    sys.exit(main(sys.argv[1:]))
