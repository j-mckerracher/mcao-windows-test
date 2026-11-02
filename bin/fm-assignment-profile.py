#!/usr/bin/env python3
"""fm-assignment-profile.py - validate and render the assignment profile decision.

docs/assignment-profile-policy.md owns the contract, docs/assignment-profile-decision.v1.schema.json
is its machine-readable shape, and docs/assignment-profile-revisions.json is the committed registry
of approved policy revisions. Every path is read from the code root given on the command line, never
from a worker's worktree, because an assignment cannot choose or weaken its own obligations.

Usage:
  fm-assignment-profile.py validate --code-root DIR [--require-current] [--task ID --target-branch B --base-commit SHA] [--record FILE|-]
  fm-assignment-profile.py render   --code-root DIR [--record FILE|-]
  fm-assignment-profile.py meta     --code-root DIR [--record FILE|-]

validate prints the canonical record JSON on stdout and exits 0 when the record is accepted.
A refusal prints one line naming the reason on stderr and exits 1. A usage, input, or unsupported
schema error exits 2. The pin options are optional; a new launch passes all of them and sets
--require-current so the record must name the registry's host-selected current revision.
Relaunch omits --require-current, because a launch stays pinned to its original revision as long as
that revision remains approved and unchanged.
render validates the record the same way and prints the obligations as Markdown bullet lines.

The schema is evaluated by a stdlib-only evaluator, because the repo has no JSON Schema dependency.
It implements only the keywords the schema uses and fails closed on anything else: an unknown keyword
is a usage-level error (exit 2), never a pass. Duplicate object keys and non-standard constants are
refused before the schema is consulted, since JSON parsers otherwise silently keep the last value.
"""

import hashlib
import json
import re
import sys
from pathlib import Path

SCHEMA_REL = "docs/assignment-profile-decision.v1.schema.json"
REGISTRY_REL = "docs/assignment-profile-revisions.json"
REGISTRY_SCHEMA = "assignment-profile-revisions.v1"
CONTRACT_VERSION = 1
ANNOTATIONS = {"$schema", "$id", "title", "description"}
VALIDATING = {"$ref", "type", "const", "enum", "pattern", "minLength", "minItems", "uniqueItems",
              "items", "required", "properties", "additionalProperties", "allOf", "if", "then"}
USAGE = ("usage: fm-assignment-profile.py validate|render|meta --code-root DIR "
         "[--task ID --target-branch B --base-commit SHA --require-current] [--record FILE|-]")


class Refuse(Exception):
    """A record or registry the contract does not accept; the message names the reason."""


class Unsupported(Exception):
    """The schema or input uses a construct this evaluator does not implement."""


def strict_object(pairs):
    out = {}
    for key, value in pairs:
        if key in out:
            raise Refuse("duplicate key '%s' in the record" % key)
        out[key] = value
    return out


def refuse_constant(name):
    raise Refuse("non-standard JSON constant '%s' in the record" % name)


def parse_strict(text, what):
    try:
        return json.loads(text, object_pairs_hook=strict_object, parse_constant=refuse_constant)
    except Refuse:
        raise
    except ValueError as err:
        raise Refuse("%s is not valid JSON: %s" % (what, err.msg))


def same(a, b):
    if isinstance(a, bool) or isinstance(b, bool):
        return type(a) is type(b) and a == b
    if isinstance(a, dict) and isinstance(b, dict):
        return a.keys() == b.keys() and all(same(a[k], b[k]) for k in a)
    if isinstance(a, list) and isinstance(b, list):
        return len(a) == len(b) and all(same(x, y) for x, y in zip(a, b))
    if isinstance(a, (dict, list)) or isinstance(b, (dict, list)):
        return False
    return a == b


def is_type(name, value):
    if name == "object":
        return isinstance(value, dict)
    if name == "array":
        return isinstance(value, list)
    if name == "string":
        return isinstance(value, str)
    if name == "boolean":
        return isinstance(value, bool)
    if name == "integer":
        return isinstance(value, int) and not isinstance(value, bool)
    raise Unsupported("unsupported type name: %s" % name)


def resolve(root, ref):
    if not ref.startswith("#/"):
        raise Unsupported("unsupported reference: %s" % ref)
    node = root
    for part in ref[2:].split("/"):
        node = node[part.replace("~1", "/").replace("~0", "~")]
    return node


def evaluate(schema, value, root, path, errors):
    if not isinstance(schema, dict):
        raise Unsupported("schema node is not an object")
    unknown = set(schema) - VALIDATING - ANNOTATIONS
    if unknown:
        raise Unsupported("unsupported schema keyword: %s" % sorted(unknown)[0])
    if "then" in schema and "if" not in schema:
        raise Unsupported("schema has then without if")
    if "$ref" in schema:
        evaluate(resolve(root, schema["$ref"]), value, root, path, errors)
    if "type" in schema:
        names = schema["type"] if isinstance(schema["type"], list) else [schema["type"]]
        if not any(is_type(name, value) for name in names):
            errors.append((path, "is not a %s" % "/".join(names)))
            return
    if "const" in schema and not same(value, schema["const"]):
        errors.append((path, "must be %s" % json.dumps(schema["const"])))
    if "enum" in schema and not any(same(value, option) for option in schema["enum"]):
        errors.append((path, "is not one of the allowed values"))
    if isinstance(value, str):
        if "pattern" in schema and not re.search(schema["pattern"], value):
            errors.append((path, "does not match the required form"))
        if "minLength" in schema and len(value) < schema["minLength"]:
            errors.append((path, "is shorter than %d" % schema["minLength"]))
    if isinstance(value, list):
        if "minItems" in schema and len(value) < schema["minItems"]:
            errors.append((path, "needs at least %d item(s)" % schema["minItems"]))
        if schema.get("uniqueItems"):
            seen = [json.dumps(item, sort_keys=True) for item in value]
            if len(set(seen)) != len(seen):
                errors.append((path, "repeats an entry"))
        if "items" in schema:
            for index, item in enumerate(value):
                evaluate(schema["items"], item, root, "%s/%d" % (path, index), errors)
    if isinstance(value, dict):
        for key in schema.get("required", []):
            if key not in value:
                errors.append((path + "/" + key, "is required"))
        props = schema.get("properties", {})
        for key, sub in props.items():
            if key in value:
                evaluate(sub, value[key], root, path + "/" + key, errors)
        if "additionalProperties" in schema:
            extra = schema["additionalProperties"]
            for key in value:
                if key in props:
                    continue
                if extra is False:
                    errors.append((path + "/" + key, "is not a field of this record"))
                elif isinstance(extra, dict):
                    evaluate(extra, value[key], root, path + "/" + key, errors)
    for sub in schema.get("allOf", []):
        evaluate(sub, value, root, path, errors)
    if "if" in schema:
        probe = []
        evaluate(schema["if"], value, root, path, probe)
        if not probe and "then" in schema:
            evaluate(schema["then"], value, root, path, errors)


def sha256_file(path):
    try:
        return hashlib.sha256(Path(path).read_bytes()).hexdigest()
    except OSError as err:
        raise Refuse("policy material %s is unreadable: %s" % (path, err.strerror))


def load_registry(code_root):
    path = Path(code_root) / REGISTRY_REL
    try:
        text = path.read_text(encoding="utf-8")
    except OSError as err:
        raise Refuse("the revision registry %s is unreadable: %s" % (REGISTRY_REL, err.strerror))
    return parse_strict(text, "the revision registry")


def check_revision(code_root, revision_id, require_current):
    registry = load_registry(code_root)
    if not isinstance(registry, dict) or registry.get("schema") != REGISTRY_SCHEMA:
        raise Refuse("the revision registry does not declare schema %s" % REGISTRY_SCHEMA)
    entries = registry.get("revisions")
    if not isinstance(entries, list):
        raise Refuse("the revision registry has no revisions list")
    if require_current and registry.get("current") != revision_id:
        raise Refuse("pinned_policy_revision '%s' is not the host-selected current revision '%s'; "
                     "new launches must pin the current revision" % (revision_id, registry.get("current")))
    matches = [e for e in entries if isinstance(e, dict) and e.get("id") == revision_id]
    if not matches:
        raise Refuse("pinned_policy_revision '%s' is not in the approved revision registry; "
                     "routing is blocked and no replacement profile is created" % revision_id)
    if len(matches) > 1:
        raise Refuse("the revision registry lists '%s' more than once" % revision_id)
    entry = matches[0]
    if entry.get("contract_version") != CONTRACT_VERSION:
        raise Refuse("revision '%s' is for an unsupported contract version" % revision_id)
    if entry.get("status") != "approved":
        raise Refuse("revision '%s' is not approved in the registry" % revision_id)
    files = entry.get("files")
    if not isinstance(files, dict) or not files:
        raise Refuse("revision '%s' pins no policy material" % revision_id)
    for rel, digest in sorted(files.items()):
        if sha256_file(Path(code_root) / rel) != digest:
            raise Refuse("policy material %s differs from the approved content of revision '%s'; "
                         "a changed revision needs its own registry approval" % (rel, revision_id))


def canonical(record):
    return json.dumps(record, indent=2, sort_keys=True, ensure_ascii=False) + "\n"


def validate(code_root, text, pins):
    schema_path = Path(code_root) / SCHEMA_REL
    try:
        schema = parse_strict(schema_path.read_text(encoding="utf-8"), "the decision schema")
    except OSError as err:
        raise Unsupported("the decision schema %s is unreadable: %s" % (SCHEMA_REL, err.strerror))
    record = parse_strict(text, "the assignment profile record")
    errors = []
    evaluate(schema, record, schema, "", errors)
    if errors:
        where, what = errors[0]
        raise Refuse("record field '%s' %s" % (where or "(root)", what))
    check_revision(code_root, record["pinned_policy_revision"], pins.get("require_current", False))
    task = pins.get("task")
    if task is not None:
        if record["task_id"] != task:
            raise Refuse("task_id is '%s' but this spawn launches task '%s'; contradictory decision"
                         % (record["task_id"], task))
        if record["decision_id"].split("/", 1)[0] != task:
            raise Refuse("decision_id '%s' does not begin with the task id '%s'" % (record["decision_id"], task))
    if pins.get("target_branch") is not None and record["target_branch"] != pins["target_branch"]:
        raise Refuse("target_branch is '%s' but the host pins '%s'; contradictory decision"
                     % (record["target_branch"], pins["target_branch"]))
    if pins.get("base_commit") is not None and record["comparison_base_commit"] != pins["base_commit"]:
        raise Refuse("comparison_base_commit is %s but the host pins %s; the decision was written against another base"
                     % (record["comparison_base_commit"], pins["base_commit"]))
    return record


def render(record):
    ob = record["evidence_obligations"]
    triggers = record["risk_triggers"]
    lines = [
        "- Profile `%s`, decision stage `%s`, classification `%s`, decision `%s`." % (
            record["profile"], record["decision_stage"], record["classification_status"], record["decision_id"]),
        "- Policy revision `%s`; target branch `%s`; comparison base `%s`." % (
            record["pinned_policy_revision"], record["target_branch"], record["comparison_base_commit"]),
        "- Required stages, in order: %s." % ", ".join(record["required_stages"]),
    ]
    if ob["tests_required"]:
        lines.append("- Tests: required by this profile; run the tests that cover your change.")
    else:
        lines.append("- Tests: this profile requires zero tests executed; run only the applicable non-test checks, "
                     "and do not run or add tests to satisfy it.")
    if ob["independent_change_review_required"]:
        lines.append("- Independent change review: required; the selected delivery path supplies it, with no extra reviewer.")
    else:
        lines.append("- Independent change review: not required by this profile.")
    if triggers:
        lines.append("- Risk triggers: %s." % ", ".join(triggers))
    lines.append("- Reasons: %s." % "; ".join(record["reasons"]))
    if "evaluated_change_identity" in record:
        lines.append("- Evaluated change identity: `%s`." % record["evaluated_change_identity"])
    return "\n".join(lines) + "\n"


def meta_lines(record):
    # The task record keys a launch persists beside the canonical decision file
    # (bin/fm-assignment-profile-lib.sh owns the key names and their use).
    return "".join("%s=%s\n" % (key, record[field]) for key, field in (
        ("profile_decision_id", "decision_id"),
        ("profile_policy_revision", "pinned_policy_revision"),
        ("profile_name", "profile"),
        ("profile_decision_stage", "decision_stage"),
        ("profile_classification_status", "classification_status"),
        ("profile_target_branch", "target_branch"),
        ("profile_comparison_base_commit", "comparison_base_commit"),
    ))


def parse_args(argv):
    if not argv or argv[0] not in ("validate", "render", "meta"):
        raise Unsupported(USAGE)
    opts = {"cmd": argv[0], "record": "-", "code_root": None, "require_current": False,
            "task": None, "target_branch": None, "base_commit": None}
    flags = {"--code-root": "code_root", "--record": "record", "--task": "task",
             "--target-branch": "target_branch", "--base-commit": "base_commit"}
    rest = argv[1:]
    while rest:
        flag = rest.pop(0)
        if flag == "--require-current":
            opts["require_current"] = True
            continue
        if flag not in flags or not rest:
            raise Unsupported(USAGE)
        opts[flags[flag]] = rest.pop(0)
    if not opts["code_root"]:
        raise Unsupported(USAGE)
    return opts


def main(argv):
    try:
        opts = parse_args(argv)
        text = sys.stdin.read() if opts["record"] == "-" else Path(opts["record"]).read_text(encoding="utf-8")
        pins = {"task": opts["task"], "target_branch": opts["target_branch"],
                "base_commit": opts["base_commit"], "require_current": opts["require_current"]}
        record = validate(opts["code_root"], text, pins)
    except Refuse as err:
        print("error: assignment profile refused: %s" % err, file=sys.stderr)
        return 1
    except (Unsupported, OSError, KeyError, TypeError) as err:
        print("error: assignment profile check could not run: %s" % err, file=sys.stderr)
        return 2
    if opts["cmd"] == "render":
        sys.stdout.write(render(record))
    elif opts["cmd"] == "meta":
        sys.stdout.write(meta_lines(record))
    else:
        sys.stdout.write(canonical(record))
    return 0


if __name__ == "__main__":
    sys.exit(main(sys.argv[1:]))
