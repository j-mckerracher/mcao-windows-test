#!/usr/bin/env python3
"""Regenerates the parity fixtures that crates/mcao-core/tests/assignment_profile.rs compares against.

Run from any directory, with the stdlib only:

    python3 -I crates/mcao-core/tests/fixtures/assignment_profile/generate.py

It copies the decision schema from docs/, writes a self-contained code root (a stub policy file and
a registry that pins it), writes each case's record, and records what bin/fm-assignment-profile.py
prints for it: stdout, stderr and the exit code. The Rust tests read those files and never run Python.
The record edits below are the same Python expressions tests/fm-assignment-profile.test.sh uses.
"""

import json
import hashlib
import shutil
import subprocess
import sys
from pathlib import Path

HERE = Path(__file__).resolve().parent
REPO = HERE.parents[4]
VALIDATOR = REPO / "bin" / "fm-assignment-profile.py"
SCHEMA_REL = "docs/assignment-profile-decision.v1.schema.json"
GOOD_BASE = "0123456789abcdef0123456789abcdef01234567"
OTHER_BASE = "fedcba9876543210fedcba9876543210fedcba98"
POLICY_REL = "docs/assignment-profile-policy.md"
POLICY = b"# Fixture policy material\n\nA stub that stands in for the policy text, so the parity fixtures do not depend on it.\n"
POLICY_CHANGED = POLICY + b"Edited after approval.\n"
POLICY_R1 = b"# Fixture policy material, revision 1\n"
GOOD_TASK = "T-1"
DOCS_ONLY_STAGES = ["ground-scope", "implement", "focused-review", "verify", "change-reassessment", "delivery-handoff"]
STAGES = {
    "documentation-only": DOCS_ONLY_STAGES,
    "small-low-risk": ["ground-scope", "short-plan", "implement", "focused-review", "verify", "change-reassessment", "delivery-handoff"],
    "standard": ["ground-scope", "design", "implement", "focused-review", "verify", "change-reassessment", "delivery-handoff"],
    "high-risk": ["ground-scope", "design", "design-critique", "implement", "independent-change-review", "risk-verify", "change-reassessment", "delivery-handoff"],
}


def sha256(data: bytes) -> str:
    return hashlib.sha256(data).hexdigest()


def base_record():
    """The high-risk record tests/assignment-profile-helpers.sh prints for task T-1."""
    return {
        "contract_version": 1,
        "decision_id": "T-1/1",
        "task_id": "T-1",
        "assignment_intent": "Exercise the launch behavior under test",
        "expected_surfaces": ["bin/fm-spawn.sh"],
        "pinned_policy_revision": "motif-assignment-profile-r2",
        "profile": "high-risk",
        "decision_stage": "provisional",
        "classification_status": "resolved",
        "risk_triggers": ["dispatch-lifecycle-worktree-delivery-merge-or-proof-gate-authority"],
        "change_coverage": {
            "complete": True,
            "categories": ["committed", "tracked-working-tree", "untracked", "renamed", "deleted"],
        },
        "reasons": ["spawn lifecycle and relaunch restoration change"],
        "required_stages": STAGES["high-risk"],
        "evidence_obligations": {"tests_required": True, "independent_change_review_required": True},
        "target_branch": "main",
        "comparison_base_commit": GOOD_BASE,
    }


def dumped(record) -> str:
    return json.dumps(record, indent=2) + "\n"


def edited(edit: str) -> str:
    """Applies a test-script edit expression (operating on dict d) to the base record."""
    scope = {"d": base_record()}
    exec(edit, scope)
    return dumped(scope["d"])


def base_text() -> str:
    return dumped(base_record())


DOC_ONLY = (
    'd["profile"] = "documentation-only"; d["decision_stage"] = "final"; d["classification_status"] = "resolved"; '
    'd["risk_triggers"] = []; d["required_stages"] = ["ground-scope","implement","focused-review","verify","change-reassessment","delivery-handoff"]; '
    'd["evidence_obligations"] = {"tests_required": %s, "independent_change_review_required": False}; '
    'd["evaluated_change_identity"] = "sha256:0"'
)
SMALL = (
    'd["profile"] = "small-low-risk"; d["decision_stage"] = "final"; d["classification_status"] = "resolved"; d["risk_triggers"] = []; '
    'd["required_stages"] = ["ground-scope","short-plan","implement","focused-review","verify","change-reassessment","delivery-handoff"]; '
    'd["evidence_obligations"] = {"tests_required": True, "independent_change_review_required": False}; d["evaluated_change_identity"] = "sha256:0"'
)
FULL_PINS = {"task": GOOD_TASK, "target_branch": "main", "base_commit": GOOD_BASE, "require_current": True}
RELAUNCH_PINS = {"task": GOOD_TASK}


def case(name, command, record, pins=None, code_root="code_root"):
    return {"name": name, "command": command, "record": record, "pins": pins or {}, "code_root": code_root}


def build_cases():
    cases = [
        case("valid_high_risk_new_launch", "validate", base_text(), FULL_PINS),
        case("valid_high_risk_relaunch", "validate", base_text(), RELAUNCH_PINS),
        case("valid_reversed_key_order", "validate",
             dumped(dict(reversed(list(base_record().items())))), RELAUNCH_PINS),
        case("valid_unicode_task_id_is_not_escaped", "validate",
             edited('d["task_id"] = "T-\u00fc"; d["decision_id"] = "T-\u00fc/1"'), {}),
        case("valid_control_characters_are_escaped", "validate",
             edited('d["reasons"] = ["tab\\there\\u0001"]'), {}),
        case("valid_float_one_contract_version", "validate",
             base_text().replace('"contract_version": 1,', '"contract_version": 1.0,'), {}),
        case("valid_documentation_only_zero_tests", "validate", edited(DOC_ONLY % "False"), RELAUNCH_PINS),
        case("valid_small_low_risk_with_tests", "validate", edited(SMALL), RELAUNCH_PINS),
        case("valid_trailing_newline_in_comparison_base", "validate",
             edited('d["comparison_base_commit"] = "%s\\n"' % GOOD_BASE), {}),
        case("valid_trailing_newline_in_decision_id", "validate",
             edited('d["decision_id"] = "T-1/1\\n"'), RELAUNCH_PINS),
        case("refuse_unknown_profile", "validate", edited('d["profile"] = "docs-only"'), RELAUNCH_PINS),
        case("refuse_contract_version_two", "validate", edited('d["contract_version"] = 2'), RELAUNCH_PINS),
        case("refuse_contract_version_true", "validate", edited('d["contract_version"] = True'), RELAUNCH_PINS),
        case("refuse_final_unresolved", "validate",
             edited('d["decision_stage"] = "final"; d["classification_status"] = "unresolved"'), RELAUNCH_PINS),
        case("refuse_missing_comparison_base", "validate", edited('del d["comparison_base_commit"]'), RELAUNCH_PINS),
        case("refuse_final_without_identity", "validate",
             edited('d["decision_stage"] = "final"; d["classification_status"] = "resolved"; d.pop("evaluated_change_identity", None)'),
             RELAUNCH_PINS),
        case("refuse_risk_trigger_on_standard", "validate",
             edited('d["profile"] = "standard"; d["decision_stage"] = "final"; d["classification_status"] = "resolved"; '
                    'd["required_stages"] = ["ground-scope","design","implement","focused-review","verify","change-reassessment","delivery-handoff"]; '
                    'd["evidence_obligations"]["independent_change_review_required"] = False'), RELAUNCH_PINS),
        case("refuse_unresolved_on_final_high_risk", "validate",
             edited('d["decision_stage"] = "final"; d["classification_status"] = "unresolved"'), RELAUNCH_PINS),
        case("refuse_unknown_top_level_key", "validate", edited('d["authority_override"] = "merge"'), RELAUNCH_PINS),
        case("refuse_documentation_only_with_tests", "validate", edited(DOC_ONLY % "True"), RELAUNCH_PINS),
        case("refuse_incomplete_coverage_for_final", "validate",
             edited('d["decision_stage"] = "final"; d["change_coverage"]["complete"] = False'), RELAUNCH_PINS),
        case("refuse_bad_decision_id_pattern", "validate", edited('d["decision_id"] = "T-1/0"'), RELAUNCH_PINS),
        case("refuse_bad_comparison_base_pattern", "validate",
             edited('d["comparison_base_commit"] = "%s"' % GOOD_BASE.upper()), RELAUNCH_PINS),
        case("refuse_duplicate_key", "validate",
             base_text().replace('"profile": "high-risk",', '"profile": "high-risk", "profile": "standard",'), RELAUNCH_PINS),
        case("refuse_nan_constant", "validate", base_text().replace('"contract_version": 1,', '"contract_version": NaN,'), RELAUNCH_PINS),
        case("refuse_infinity_constant", "validate", base_text().replace('"contract_version": 1,', '"contract_version": Infinity,'), RELAUNCH_PINS),
        case("refuse_negative_infinity_constant", "validate", base_text().replace('"contract_version": 1,', '"contract_version": -Infinity,'), RELAUNCH_PINS),
        case("refuse_not_json", "validate", "not json\n", RELAUNCH_PINS),
        case("refuse_empty_record", "validate", "", RELAUNCH_PINS),
        case("refuse_extra_data", "validate", base_text() + "x", RELAUNCH_PINS),
        case("refuse_byte_order_mark", "validate", "\ufeff" + base_text(), RELAUNCH_PINS),
        case("refuse_trailing_comma_in_array", "validate",
             base_text().replace('"bin/fm-spawn.sh"\n  ]', '"bin/fm-spawn.sh",\n  ]'), RELAUNCH_PINS),
        case("refuse_trailing_comma_in_object", "validate",
             base_text().replace('"target_branch": "main"', '"target_branch": "main",'), RELAUNCH_PINS),
        case("refuse_non_string_key", "validate", '{1: 2}', RELAUNCH_PINS),
        case("refuse_unterminated_string", "validate", '{"a": "b', RELAUNCH_PINS),
        case("refuse_invalid_escape", "validate", '["\\q"]', RELAUNCH_PINS),
        case("refuse_control_character_in_string", "validate", '["a\nb"]', RELAUNCH_PINS),
        case("refuse_task_mismatch", "validate", base_text(), {"task": "T-2"}),
        case("refuse_decision_for_other_task", "validate",
             edited('d["decision_id"] = "T-9/1"'), RELAUNCH_PINS),
        case("refuse_target_branch_pin", "validate", base_text(),
             {"task": GOOD_TASK, "target_branch": "release"}),
        case("refuse_base_commit_pin", "validate", base_text(),
             {"task": GOOD_TASK, "base_commit": OTHER_BASE}),
        case("refuse_unapproved_revision", "validate",
             edited('d["pinned_policy_revision"] = "motif-assignment-profile-r9"'), RELAUNCH_PINS),
        case("refuse_revoked_revision", "validate",
             edited('d["pinned_policy_revision"] = "motif-assignment-profile-r0"'), RELAUNCH_PINS),
        case("refuse_superseded_revision_on_relaunch", "validate",
             edited('d["pinned_policy_revision"] = "motif-assignment-profile-r1"'), RELAUNCH_PINS),
        case("refuse_require_current_on_superseded", "validate",
             edited('d["pinned_policy_revision"] = "motif-assignment-profile-r1"'), FULL_PINS),
        case("refuse_changed_policy_material", "validate", base_text(), RELAUNCH_PINS, "code_root_policy_changed"),
        case("refuse_unsupported_schema_keyword", "validate", base_text(), RELAUNCH_PINS, "code_root_bad_schema"),
        case("render_high_risk", "render", base_text(), RELAUNCH_PINS),
        case("render_documentation_only", "render", edited(DOC_ONLY % "False"), RELAUNCH_PINS),
        case("render_refused", "render", edited('d["profile"] = "docs-only"'), RELAUNCH_PINS),
        case("meta_high_risk", "meta", base_text(), RELAUNCH_PINS),
        case("meta_small_low_risk", "meta", edited(SMALL), RELAUNCH_PINS),
    ]
    return cases


def registry_text(schema_bytes: bytes) -> str:
    pinned = {SCHEMA_REL: sha256(schema_bytes)}
    return dumped({
        "schema": "assignment-profile-revisions.v1",
        "current": "motif-assignment-profile-r2",
        "revisions": [
            {"id": "motif-assignment-profile-r0", "contract_version": 1, "status": "revoked",
             "files": {**pinned, POLICY_REL: sha256(POLICY)}},
            {"id": "motif-assignment-profile-r1", "contract_version": 1, "status": "approved",
             "files": {**pinned, POLICY_REL: sha256(POLICY_R1)}},
            {"id": "motif-assignment-profile-r2", "contract_version": 1, "status": "approved",
             "files": {**pinned, POLICY_REL: sha256(POLICY)}},
        ],
    })


def write_code_root(name: str, schema_bytes: bytes, policy: bytes):
    root = HERE / name
    (root / "docs").mkdir(parents=True)
    (root / SCHEMA_REL).write_bytes(schema_bytes)
    (root / POLICY_REL).write_bytes(policy)
    (root / "docs" / "assignment-profile-revisions.json").write_text(registry_text(schema_bytes), encoding="utf-8")


def run_validator(command: str, code_root: Path, record: Path, pins: dict):
    argv = [sys.executable, "-I", str(VALIDATOR), command, "--code-root", str(code_root), "--record", str(record)]
    if pins.get("task") is not None:
        argv += ["--task", pins["task"]]
    if pins.get("target_branch") is not None:
        argv += ["--target-branch", pins["target_branch"]]
    if pins.get("base_commit") is not None:
        argv += ["--base-commit", pins["base_commit"]]
    if pins.get("require_current"):
        argv.append("--require-current")
    return subprocess.run(argv, capture_output=True, check=False)


def main():
    schema_bytes = (REPO / SCHEMA_REL).read_bytes()
    for stale in ("cases", "code_root", "code_root_policy_changed", "code_root_bad_schema"):
        shutil.rmtree(HERE / stale, ignore_errors=True)
    write_code_root("code_root", schema_bytes, POLICY)
    write_code_root("code_root_policy_changed", schema_bytes, POLICY_CHANGED)
    write_code_root("code_root_bad_schema", json.dumps(
        {**json.loads(schema_bytes), "format": "uuid"}, indent=2).encode("utf-8") + b"\n", POLICY)

    for entry in build_cases():
        directory = HERE / "cases" / entry["name"]
        directory.mkdir(parents=True)
        record_path = directory / "record.json"
        record_path.write_bytes(entry["record"].encode("utf-8"))
        (directory / "case.json").write_text(json.dumps({
            "command": entry["command"],
            "code_root": entry["code_root"],
            "pins": entry["pins"],
        }, indent=2, sort_keys=True) + "\n", encoding="utf-8")
        result = run_validator(entry["command"], HERE / entry["code_root"], record_path, entry["pins"])
        (directory / "expected.stdout").write_bytes(result.stdout)
        (directory / "expected.stderr").write_bytes(result.stderr)
        (directory / "expected.exit").write_text(f"{result.returncode}\n", encoding="utf-8")
        print(f"{entry['name']}: exit {result.returncode}")


if __name__ == "__main__":
    sys.exit(main())
