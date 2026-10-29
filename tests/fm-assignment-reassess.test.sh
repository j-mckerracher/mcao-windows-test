#!/usr/bin/env bash
# tests/fm-assignment-reassess.test.sh - the reassessment core of the assignment profile policy,
# driven through bin/fm-assignment-reassess.py against real git worktrees. docs/assignment-profile-policy.md
# owns the profiles, precedence, path classes, and the D-01..D-09 and R-01..R-14 fixtures;
# docs/assignment-profile-decision.v1.schema.json owns the record shape. Base pinning and the
# completion gate are outside this core (DPES-763 and DPES-764 own their wiring).
set -u

# shellcheck source=tests/lib.sh
. "$(dirname "${BASH_SOURCE[0]}")/lib.sh"

TMP_ROOT=$(fm_test_tmproot fm-assignment-reassess)
TOOL="$ROOT/bin/fm-assignment-reassess.py"
TASK=dpes-765-demo
TRIGGER=security-authorization-secrets-or-sensitive-data
ALL_CONDITIONS='{"single_bounded_behavior_change":true,"no_security_or_authorization_change":true,"no_sensitive_data_change":true,"no_destructive_operation":true,"no_merge_delivery_authority_change":true,"no_dispatch_lifecycle_worktree_or_proof_gate_change":true,"no_public_contract_incompatibility":true,"no_data_migration":true,"no_unresolved_cross_component_impact":true,"relevant_focused_verification_identified":true}'
ONE_BOUNDED_FALSE='{"single_bounded_behavior_change":false,"no_security_or_authorization_change":true,"no_sensitive_data_change":true,"no_destructive_operation":true,"no_merge_delivery_authority_change":true,"no_dispatch_lifecycle_worktree_or_proof_gate_change":true,"no_public_contract_incompatibility":true,"no_data_migration":true,"no_unresolved_cross_component_impact":true,"relevant_focused_verification_identified":true}'
DOC_STAGES='["ground-scope","implement","focused-review","verify","change-reassessment","delivery-handoff"]'
HIGH_STAGES='["ground-scope","design","design-critique","implement","independent-change-review","risk-verify","change-reassessment","delivery-handoff"]'
UNRESOLVABLE_SHA=$(printf '%040d' 0 | tr 0 1)
REAL_GIT=$(command -v git)

# --- assertion helpers ------------------------------------------------------

expect() {  # expect <label> <expected> <actual>
  if [ "$2" = "$3" ]; then
    pass "$1"
  else
    fail "$1: expected [$2] got [$3]"
  fi
}

expect_prefix() {  # expect_prefix <label> <prefix> <actual>
  case "$3" in
    "$2"*) pass "$1" ;;
    *) fail "$1: [$3] does not start with [$2]" ;;
  esac
}

contains() {  # contains <label> <haystack> <needle>
  case "$2" in
    *"$3"*) pass "$1" ;;
    *) fail "$1: [$3] not found in [$2]" ;;
  esac
}

# --- JSON readers over CLI output -------------------------------------------

# field <json-file> <dotted.path>: scalars as text, objects and lists as compact JSON.
field() {
  python3 -I -c '
import json, sys
value = json.load(open(sys.argv[1], encoding="utf-8"))
for part in sys.argv[2].split("."):
    try:
        value = value[int(part)] if isinstance(value, list) else value[part]
    except (KeyError, IndexError, TypeError, ValueError):
        print("<missing>")
        sys.exit(0)
print(value if isinstance(value, str) else json.dumps(value, sort_keys=True, separators=(",", ":")))
' "$1" "$2"
}

# path_field <collect-json> <path> <key>: one changed-path entry from `collect` output.
path_field() {
  python3 -I -c '
import json, sys
for entry in json.load(open(sys.argv[1], encoding="utf-8"))["paths"]:
    if entry["path"] == sys.argv[2]:
        print(json.dumps(entry[sys.argv[3]]))
        sys.exit(0)
print("<absent>")
' "$1" "$2" "$3"
}

expect_decision() {  # expect_decision <label> <result-json> <profile> <stage> <status>
  expect "$1: profile" "$3" "$(field "$2" decision.profile)"
  expect "$1: stage" "$4" "$(field "$2" decision.decision_stage)"
  expect "$1: status" "$5" "$(field "$2" decision.classification_status)"
}

expect_obligations() {  # expect_obligations <label> <result-json> <tests_required> <independent_review>
  expect "$1: tests_required" "$3" "$(field "$2" decision.evidence_obligations.tests_required)"
  expect "$1: independent review" "$4" "$(field "$2" decision.evidence_obligations.independent_change_review_required)"
}

# --- fixture builders -------------------------------------------------------

# tgit <repo> <git args...>: git with a fixed test identity and no signing.
tgit() {
  local repo=$1
  shift
  git -C "$repo" -c user.name=fm-test -c user.email=fm-test@example.invalid -c commit.gpgsign=false "$@"
}

# replace_in <file> <old> <new>: exactly one textual replacement, else the case fails.
replace_in() {
  python3 -I -c '
import sys
path, old, new = sys.argv[1:4]
text = open(path, encoding="utf-8").read()
if text.count(old) != 1:
    sys.exit(f"{path}: {text.count(old)} matches for the replaced text")
open(path, "w", encoding="utf-8").write(text.replace(old, new))
' "$1" "$2" "$3" || fail "replace_in could not edit $1"
}

# new_case <name>: a fresh copy of the base repository, echoed as its path.
new_case() {
  local dir="$TMP_ROOT/case-$1"
  cp -R "$BASE_REPO" "$dir" || fail "could not copy base repository for $1"
  printf '%s\n' "$dir"
}

# write_request <out> <base-commit> <triggers-json> <conditions-json|omit> [previous-result-json]
write_request() {
  python3 -I -c '
import json, sys
out, base, triggers, conditions, previous = sys.argv[1:6]
request = {
    "task_id": "dpes-765-demo",
    "assignment_intent": "Reassess the profile from the actual change set.",
    "expected_surfaces": ["README.md", "docs/guide.md", "bin/tool.sh"],
    "pinned_policy_revision": "rev-test-1",
    "target_branch": "main",
    "comparison_base_commit": base,
    "risk_triggers": json.loads(triggers),
}
if conditions != "omit":
    request["small_low_risk_conditions"] = json.loads(conditions)
if previous:
    request["previous"] = json.load(open(previous, encoding="utf-8"))["decision"]
with open(out, "w", encoding="utf-8") as handle:
    json.dump(request, handle, indent=2)
' "$1" "$2" "$3" "$4" "${5:-}"
}

# mutate_previous <result-json> <out> <python statement over dict d>: a tampered prior decision wrapper.
mutate_previous() {
  python3 -I -c '
import json, sys
source, out, statement = sys.argv[1:4]
d = json.load(open(source, encoding="utf-8"))["decision"]
exec(statement, {"d": d})
json.dump({"decision": d}, open(out, "w", encoding="utf-8"), indent=2)
' "$1" "$2" "$3"
}

# reassess <repo> <request> <out>: runs the CLI and echoes its exit code.
reassess() {
  "$TOOL" reassess --repo "$1" --request "$2" > "$3"
  printf '%s\n' "$?"
}

# expect_blocked <label> <request> <reason fragment>: refused with no decision record.
expect_blocked() {
  local out="$2.result.json" rc
  rc=$(reassess "$D_REPO" "$2" "$out")
  expect "$1: exits 1" 1 "$rc"
  expect "$1: status blocked" blocked "$(field "$out" status)"
  expect "$1: no record emitted" "<missing>" "$(field "$out" decision)"
  contains "$1: reason" "$(field "$out" reasons)" "$3"
}

BASE_REPO="$TMP_ROOT/base"
mkdir -p "$BASE_REPO/bin" "$BASE_REPO/docs/examples"
printf '# Demo\n\nRun the demo.\n' > "$BASE_REPO/README.md"
printf '# Guide\n\nUse the guide.\n' > "$BASE_REPO/docs/guide.md"
printf '# Maintainer\n\nOwnership notes.\n\n%s\necho owner\n%s\n' '```sh' '```' > "$BASE_REPO/docs/maintainer.md"
printf '# Agent\n\nDispatch rules.\n' > "$BASE_REPO/docs/agent.md"
printf '{"task": "demo"}\n' > "$BASE_REPO/docs/examples/crew.json"
printf '#!/usr/bin/env bash\nauthorize() {\n  return 0\n}\necho tool\n' > "$BASE_REPO/bin/tool.sh"
printf 'entry point\n' > "$BASE_REPO/bin/fm-entry"
cat > "$BASE_REPO/docs/documentation-audiences.json" <<'JSON'
{
  "version": 1,
  "surfaces": [
    {"path": "README.md", "audience": "public-product"},
    {"path": "docs/guide.md", "audience": "public-product"},
    {"path": "docs/maintainer.md", "audience": "maintainer-architecture"},
    {"path": "docs/agent.md", "audience": "agent-runtime"},
    {"path": "docs/examples/crew.json", "audience": "operator-example"}
  ]
}
JSON
git init -q "$BASE_REPO"
tgit "$BASE_REPO" add -A
tgit "$BASE_REPO" commit -q -m "base"
BASE_SHA=$(git -C "$BASE_REPO" rev-parse HEAD)

# A git shim that fails only the untracked listing, so collection is genuinely incomplete.
FAIL_BIN=$(fm_fakebin "$TMP_ROOT/fail-listing")
cat > "$FAIL_BIN/git" <<SH
#!/usr/bin/env bash
for arg in "\$@"; do
  if [ "\$arg" = ls-files ]; then
    echo "fake git: untracked listing unavailable" >&2
    exit 128
  fi
done
exec "$REAL_GIT" "\$@"
SH
chmod +x "$FAIL_BIN/git"

# === A. change collection: coverage, identity, and path states =============

repo=$(new_case clean)
"$TOOL" collect --repo "$repo" --base-commit "$BASE_SHA" > "$TMP_ROOT/collect-clean.json"
expect "clean worktree collects complete coverage" true "$(field "$TMP_ROOT/collect-clean.json" complete)"
expect "clean worktree lists no changed paths" "[]" "$(field "$TMP_ROOT/collect-clean.json" paths)"

repo=$(new_case renamed-deleted)
tgit "$repo" mv bin/tool.sh bin/tool-renamed.sh
tgit "$repo" commit -q -m "rename tool"
tgit "$repo" rm -q docs/guide.md
tgit "$repo" commit -q -m "delete guide"
"$TOOL" collect --repo "$repo" --base-commit "$BASE_SHA" > "$TMP_ROOT/collect-rd.json"
expect "renamed and deleted paths keep full coverage" true "$(field "$TMP_ROOT/collect-rd.json" complete)"
expect "rename source exists in base only" true "$(path_field "$TMP_ROOT/collect-rd.json" bin/tool.sh in_base)"
expect "rename source is gone from worktree" false "$(path_field "$TMP_ROOT/collect-rd.json" bin/tool.sh in_worktree)"
expect "rename destination exists in worktree only" false "$(path_field "$TMP_ROOT/collect-rd.json" bin/tool-renamed.sh in_base)"
expect "rename destination is in worktree" true "$(path_field "$TMP_ROOT/collect-rd.json" bin/tool-renamed.sh in_worktree)"
expect "deleted path is reported as absent" '"absent"' "$(path_field "$TMP_ROOT/collect-rd.json" docs/guide.md kind)"

repo=$(new_case untracked)
printf '# New\n' > "$repo/docs/new.md"
"$TOOL" collect --repo "$repo" --base-commit "$BASE_SHA" > "$TMP_ROOT/collect-untracked.json"
expect "untracked file keeps full coverage" true "$(field "$TMP_ROOT/collect-untracked.json" complete)"
expect "untracked file is collected" true "$(path_field "$TMP_ROOT/collect-untracked.json" docs/new.md in_worktree)"
expect_prefix "complete collection carries a change identity" "sha256:" "$(field "$TMP_ROOT/collect-untracked.json" identity)"

repo=$(new_case failed-listing)
replace_in "$repo/README.md" "Run the demo." "Run the demo again."
PATH="$FAIL_BIN:$PATH" "$TOOL" collect --repo "$repo" --base-commit "$BASE_SHA" > "$TMP_ROOT/collect-failed.json"
expect "failed untracked listing is incomplete" false "$(field "$TMP_ROOT/collect-failed.json" complete)"
expect "failed listing is not reported as covered" '["committed","tracked-working-tree","renamed","deleted"]' "$(field "$TMP_ROOT/collect-failed.json" categories)"
contains "failed listing names the gap" "$(field "$TMP_ROOT/collect-failed.json" gaps)" "untracked"
expect "incomplete collection has no identity" "null" "$(field "$TMP_ROOT/collect-failed.json" identity)"

PLAIN="$TMP_ROOT/plain-dir"
mkdir -p "$PLAIN"
"$TOOL" collect --repo "$PLAIN" --base-commit "$BASE_SHA" > "$TMP_ROOT/collect-plain.json"
expect "non-repository collects with exit 0" 0 "$?"
expect "non-repository has no resolvable base" false "$(field "$TMP_ROOT/collect-plain.json" base_ok)"
contains "non-repository names the gap" "$(field "$TMP_ROOT/collect-plain.json" gaps)" "not a git worktree"

# === B. classification fixtures and path rules =============================

ALL_TESTS_DOC_D01=$(new_case d01)
D_REPO="$ALL_TESTS_DOC_D01"
replace_in "$D_REPO/README.md" "Run the demo." "Run the demo twice."
tgit "$D_REPO" commit -q -am "docs: demo wording"
replace_in "$D_REPO/docs/guide.md" "Use the guide." "Use the guide carefully."
write_request "$TMP_ROOT/b-d01-req.json" "$BASE_SHA" '[]' omit
rc=$(reassess "$D_REPO" "$TMP_ROOT/b-d01-req.json" "$TMP_ROOT/b-d01.json")
expect "D-01 records with exit 0" 0 "$rc"
expect "D-01 record is first in the chain" "$TASK/1" "$(field "$TMP_ROOT/b-d01.json" decision.decision_id)"
expect_decision "D-01 prose-only" "$TMP_ROOT/b-d01.json" documentation-only final resolved
expect_obligations "D-01 prose-only" "$TMP_ROOT/b-d01.json" false false
expect "D-01 has no risk triggers" "[]" "$(field "$TMP_ROOT/b-d01.json" decision.risk_triggers)"
expect "D-01 coverage is complete" true "$(field "$TMP_ROOT/b-d01.json" decision.change_coverage.complete)"
expect "D-01 required stages are the documentation stages" "$DOC_STAGES" "$(field "$TMP_ROOT/b-d01.json" decision.required_stages)"
expect_prefix "D-01 final record carries identity" "sha256:" "$(field "$TMP_ROOT/b-d01.json" decision.evaluated_change_identity)"

repo=$(new_case d02)
replace_in "$repo/bin/tool.sh" "  return 0" "  return 1"
write_request "$TMP_ROOT/b-d02-req.json" "$BASE_SHA" "[\"$TRIGGER\"]" omit
rc=$(reassess "$repo" "$TMP_ROOT/b-d02-req.json" "$TMP_ROOT/b-d02.json")
expect "D-02 records with exit 0" 0 "$rc"
expect_decision "D-02 one-line authorization change with trigger" "$TMP_ROOT/b-d02.json" high-risk final resolved
expect_obligations "D-02 one-line authorization change" "$TMP_ROOT/b-d02.json" true true
expect "D-02 trigger is recorded" "[\"$TRIGGER\"]" "$(field "$TMP_ROOT/b-d02.json" decision.risk_triggers)"
expect "D-02 required stages are the high-risk stages" "$HIGH_STAGES" "$(field "$TMP_ROOT/b-d02.json" decision.required_stages)"

repo=$(new_case d03)
replace_in "$repo/README.md" "Run the demo." "Run the demo twice."
replace_in "$repo/bin/tool.sh" "  return 0" "  return 1"
write_request "$TMP_ROOT/b-d03-req.json" "$BASE_SHA" '[]' "$ALL_CONDITIONS"
reassess "$repo" "$TMP_ROOT/b-d03-req.json" "$TMP_ROOT/b-d03.json" > /dev/null
expect_decision "D-03 mixed prose and bounded behavior" "$TMP_ROOT/b-d03.json" small-low-risk final resolved
expect_obligations "D-03 mixed prose and bounded behavior" "$TMP_ROOT/b-d03.json" true false

repo=$(new_case d04)
replace_in "$repo/docs/agent.md" "Dispatch rules." "Dispatch rules, reworded."
write_request "$TMP_ROOT/b-d04-req.json" "$BASE_SHA" '[]' "$ALL_CONDITIONS"
reassess "$repo" "$TMP_ROOT/b-d04-req.json" "$TMP_ROOT/b-d04.json" > /dev/null
expect_decision "D-04 agent instruction is behavioral, not prose" "$TMP_ROOT/b-d04.json" small-low-risk final resolved
expect_obligations "D-04 agent instruction" "$TMP_ROOT/b-d04.json" true false

repo=$(new_case d05)
replace_in "$repo/docs/maintainer.md" "echo owner" "echo owned"
write_request "$TMP_ROOT/b-d05-req.json" "$BASE_SHA" '[]' omit
reassess "$repo" "$TMP_ROOT/b-d05-req.json" "$TMP_ROOT/b-d05.json" > /dev/null
expect_decision "D-05 changed fenced command in prose" "$TMP_ROOT/b-d05.json" standard final resolved
expect_obligations "D-05 changed fenced command" "$TMP_ROOT/b-d05.json" true false
contains "D-05 names the executable prose change" "$(field "$TMP_ROOT/b-d05.json" decision.reasons)" "executable change in prose"

repo=$(new_case d06)
replace_in "$repo/bin/fm-entry" "entry point" "entry point v2"
write_request "$TMP_ROOT/b-d06-req.json" "$BASE_SHA" '[]' omit
reassess "$repo" "$TMP_ROOT/b-d06-req.json" "$TMP_ROOT/b-d06.json" > /dev/null
expect_decision "D-06 extensionless entry point is unknown" "$TMP_ROOT/b-d06.json" high-risk provisional unresolved
contains "D-06 names the unknown path" "$(field "$TMP_ROOT/b-d06.json" decision.reasons)" "unknown path: bin/fm-entry"

repo=$(new_case d07)
replace_in "$repo/README.md" "Run the demo." "Run the demo again."
write_request "$TMP_ROOT/b-d07-req.json" "$BASE_SHA" '[]' omit
PATH="$FAIL_BIN:$PATH" reassess "$repo" "$TMP_ROOT/b-d07-req.json" "$TMP_ROOT/b-d07.json" > /dev/null
expect_decision "D-07 incomplete collection blocks documentation-only" "$TMP_ROOT/b-d07.json" high-risk provisional unresolved
expect "D-07 coverage is incomplete" false "$(field "$TMP_ROOT/b-d07.json" decision.change_coverage.complete)"
contains "D-07 names the missing category" "$(field "$TMP_ROOT/b-d07.json" decision.reasons)" "missing: untracked"

repo=$(new_case d08)
replace_in "$repo/README.md" "Run the demo." "Run the demo again."
write_request "$TMP_ROOT/b-d08-req.json" "$UNRESOLVABLE_SHA" '[]' omit
reassess "$repo" "$TMP_ROOT/b-d08-req.json" "$TMP_ROOT/b-d08.json" > /dev/null
expect_decision "D-08 unresolvable comparison base blocks documentation-only" "$TMP_ROOT/b-d08.json" high-risk provisional unresolved
contains "D-08 names the unresolvable base" "$(field "$TMP_ROOT/b-d08.json" decision.reasons)" "comparison base is not resolvable"
expect "D-08 has no identity without a base" "<missing>" "$(field "$TMP_ROOT/b-d08.json" decision.evaluated_change_identity)"

repo=$(new_case d09)
replace_in "$repo/bin/tool.sh" "  return 0" "  return 1"
replace_in "$repo/bin/tool.sh" "echo tool" "echo changed"
write_request "$TMP_ROOT/b-d09-req.json" "$BASE_SHA" '[]' "$ONE_BOUNDED_FALSE"
reassess "$repo" "$TMP_ROOT/b-d09-req.json" "$TMP_ROOT/b-d09.json" > /dev/null
expect_decision "D-09 two independent behavior changes" "$TMP_ROOT/b-d09.json" standard final resolved
expect_obligations "D-09 two independent behavior changes" "$TMP_ROOT/b-d09.json" true false

repo=$(new_case renamed-prose)
tgit "$repo" mv docs/guide.md docs/guide-renamed.md
write_request "$TMP_ROOT/b-rename-req.json" "$BASE_SHA" '[]' omit
reassess "$repo" "$TMP_ROOT/b-rename-req.json" "$TMP_ROOT/b-rename.json" > /dev/null
expect_decision "renamed prose lands on an unmapped path" "$TMP_ROOT/b-rename.json" high-risk provisional unresolved
contains "renamed prose names its new unmapped path" "$(field "$TMP_ROOT/b-rename.json" decision.reasons)" "unknown path: docs/guide-renamed.md"

repo=$(new_case deleted-prose)
tgit "$repo" rm -q docs/guide.md
write_request "$TMP_ROOT/b-delete-req.json" "$BASE_SHA" '[]' omit
reassess "$repo" "$TMP_ROOT/b-delete-req.json" "$TMP_ROOT/b-delete.json" > /dev/null
expect_decision "deleted prose classifies by its removed lines" "$TMP_ROOT/b-delete.json" documentation-only final resolved

repo=$(new_case inventory-edit)
replace_in "$repo/docs/agent.md" "Dispatch rules." "Dispatch rules, reworded."
replace_in "$repo/docs/documentation-audiences.json" '{"path": "docs/agent.md", "audience": "agent-runtime"}' '{"path": "docs/agent.md", "audience": "public-product"}'
write_request "$TMP_ROOT/b-inventory-req.json" "$BASE_SHA" '[]' omit
reassess "$repo" "$TMP_ROOT/b-inventory-req.json" "$TMP_ROOT/b-inventory.json" > /dev/null
expect "worktree inventory edit cannot lower a behavioral path" "standard" "$(field "$TMP_ROOT/b-inventory.json" decision.profile)"
contains "behavioral path is classified from the base inventory" "$(field "$TMP_ROOT/b-inventory.json" decision.reasons)" "docs/agent.md: behavioral change"

repo=$(new_case untracked-prose)
replace_in "$repo/README.md" "Run the demo." "Run the demo again."
printf '# New\n' > "$repo/docs/new.md"
write_request "$TMP_ROOT/b-untracked-req.json" "$BASE_SHA" '[]' omit
reassess "$repo" "$TMP_ROOT/b-untracked-req.json" "$TMP_ROOT/b-untracked.json" > /dev/null
expect_decision "AC3 untracked prose outside the inventory" "$TMP_ROOT/b-untracked.json" high-risk provisional unresolved
contains "AC3 names the untracked unmapped path" "$(field "$TMP_ROOT/b-untracked.json" decision.reasons)" "unknown path: docs/new.md"

# === C. escalation, supersession, and no-downgrade =========================

repo=$(new_case docs-then-code)
replace_in "$repo/README.md" "Run the demo." "Run the demo twice."
write_request "$TMP_ROOT/c1-req1.json" "$BASE_SHA" '[]' omit
reassess "$repo" "$TMP_ROOT/c1-req1.json" "$TMP_ROOT/c1-d1.json" > /dev/null
replace_in "$repo/bin/tool.sh" "  return 0" "  return 1"
write_request "$TMP_ROOT/c1-req2.json" "$BASE_SHA" '[]' omit "$TMP_ROOT/c1-d1.json"
rc=$(reassess "$repo" "$TMP_ROOT/c1-req2.json" "$TMP_ROOT/c1-d2.json")
expect "AC1 escalation records with exit 0" 0 "$rc"
expect_decision "AC1 code after docs escalates to standard" "$TMP_ROOT/c1-d2.json" standard final resolved
expect_obligations "AC1 code after docs" "$TMP_ROOT/c1-d2.json" true false
expect "AC1 transition records the escalation" true "$(field "$TMP_ROOT/c1-d2.json" transition.escalated)"
expect "AC1 adds only the design stage" '["design"]' "$(field "$TMP_ROOT/c1-d2.json" transition.newly_required_stages)"
expect "AC1 record advances the sequence" "$TASK/2" "$(field "$TMP_ROOT/c1-d2.json" decision.decision_id)"
expect "AC1 record names the superseded decision" "$TASK/1" "$(field "$TMP_ROOT/c1-d2.json" decision.supersedes.decision_id)"
contains "AC1 supersession carries escalation reasons" "$(field "$TMP_ROOT/c1-d2.json" decision.supersedes.escalation_reasons)" "profile escalated from documentation-only to standard"
contains "AC1 keeps the prior decision reasons" "$(field "$TMP_ROOT/c1-d2.json" decision.reasons)" "every changed path is prose"
expect "AC1 changed identity invalidates prior evidence" true "$(field "$TMP_ROOT/c1-d2.json" transition.evidence_invalidated)"
identity_before=$(field "$TMP_ROOT/c1-d1.json" decision.evaluated_change_identity)
identity_after=$(field "$TMP_ROOT/c1-d2.json" decision.evaluated_change_identity)
expect_prefix "AC1 final record carries the new change identity" "sha256:" "$identity_after"
if [ "$identity_before" = "$identity_after" ]; then
  fail "AC1 identity did not change with the added code change"
fi
pass "AC1 identity changed with the added code change"

repo=$(new_case trigger-after-docs)
replace_in "$repo/README.md" "Run the demo." "Run the demo twice."
write_request "$TMP_ROOT/c2-req1.json" "$BASE_SHA" '[]' omit
reassess "$repo" "$TMP_ROOT/c2-req1.json" "$TMP_ROOT/c2-d1.json" > /dev/null
write_request "$TMP_ROOT/c2-req2.json" "$BASE_SHA" "[\"$TRIGGER\"]" omit "$TMP_ROOT/c2-d1.json"
rc=$(reassess "$repo" "$TMP_ROOT/c2-req2.json" "$TMP_ROOT/c2-d2.json")
expect "trigger escalation records with exit 0" 0 "$rc"
expect_decision "a trigger after docs-only selects high-risk" "$TMP_ROOT/c2-d2.json" high-risk final resolved
expect "trigger escalation adds the independent stages" '["design","design-critique","independent-change-review","risk-verify"]' "$(field "$TMP_ROOT/c2-d2.json" transition.newly_required_stages)"
expect "trigger is carried in the record" "[\"$TRIGGER\"]" "$(field "$TMP_ROOT/c2-d2.json" decision.risk_triggers)"

repo=$(new_case no-downgrade)
replace_in "$repo/bin/tool.sh" "  return 0" "  return 1"
replace_in "$repo/README.md" "Run the demo." "Run the demo twice."
write_request "$TMP_ROOT/c3-req1.json" "$BASE_SHA" "[\"$TRIGGER\"]" omit
reassess "$repo" "$TMP_ROOT/c3-req1.json" "$TMP_ROOT/c3-d1.json" > /dev/null
expect_decision "AC4 trigger starts high-risk" "$TMP_ROOT/c3-d1.json" high-risk final resolved
tgit "$repo" checkout -q -- bin/tool.sh
write_request "$TMP_ROOT/c3-req2.json" "$BASE_SHA" '[]' omit "$TMP_ROOT/c3-d1.json"
rc=$(reassess "$repo" "$TMP_ROOT/c3-req2.json" "$TMP_ROOT/c3-d2.json")
expect "AC4 smaller diff records with exit 0" 0 "$rc"
expect_decision "AC4 smaller final diff keeps high-risk" "$TMP_ROOT/c3-d2.json" high-risk final resolved
expect "AC4 does not report a downgrade" false "$(field "$TMP_ROOT/c3-d2.json" transition.escalated)"
expect "AC4 keeps the inherited trigger" "[\"$TRIGGER\"]" "$(field "$TMP_ROOT/c3-d2.json" decision.risk_triggers)"
contains "AC4 keeps the prior trigger reason" "$(field "$TMP_ROOT/c3-d2.json" decision.reasons)" "risk trigger: $TRIGGER"
expect "AC4 recheck uses the smaller identity" true "$(field "$TMP_ROOT/c3-d2.json" transition.evidence_invalidated)"

repo=$(new_case unchanged)
replace_in "$repo/README.md" "Run the demo." "Run the demo twice."
write_request "$TMP_ROOT/c4-req1.json" "$BASE_SHA" '[]' omit
reassess "$repo" "$TMP_ROOT/c4-req1.json" "$TMP_ROOT/c4-d1.json" > /dev/null
write_request "$TMP_ROOT/c4-req2.json" "$BASE_SHA" '[]' omit "$TMP_ROOT/c4-d1.json"
rc=$(reassess "$repo" "$TMP_ROOT/c4-req2.json" "$TMP_ROOT/c4-d2.json")
expect "unchanged chain exits 0" 0 "$rc"
expect "unchanged change set does not append a record" unchanged "$(field "$TMP_ROOT/c4-d2.json" status)"
expect "unchanged chain keeps the decision id" "$TASK/1" "$(field "$TMP_ROOT/c4-d2.json" decision.decision_id)"

repo=$(new_case sticky-unresolved)
replace_in "$repo/README.md" "Run the demo." "Run the demo twice."
write_request "$TMP_ROOT/c5-req1.json" "$BASE_SHA" '[]' omit
PATH="$FAIL_BIN:$PATH" reassess "$repo" "$TMP_ROOT/c5-req1.json" "$TMP_ROOT/c5-d1.json" > /dev/null
expect_decision "fail-closed listing is unresolved" "$TMP_ROOT/c5-d1.json" high-risk provisional unresolved
write_request "$TMP_ROOT/c5-req2.json" "$BASE_SHA" '[]' omit "$TMP_ROOT/c5-d1.json"
rc=$(reassess "$repo" "$TMP_ROOT/c5-req2.json" "$TMP_ROOT/c5-d2.json")
expect "resolved chain records with exit 0" 0 "$rc"
expect_decision "unresolved high-risk does not lower on resolution" "$TMP_ROOT/c5-d2.json" high-risk final resolved
expect "resolution is not an escalation" false "$(field "$TMP_ROOT/c5-d2.json" transition.escalated)"

repo=$(new_case no-lowering)
replace_in "$repo/bin/tool.sh" "  return 0" "  return 1"
write_request "$TMP_ROOT/c6-req1.json" "$BASE_SHA" '[]' omit
reassess "$repo" "$TMP_ROOT/c6-req1.json" "$TMP_ROOT/c6-d1.json" > /dev/null
expect_decision "standard decision before the lowering attempt" "$TMP_ROOT/c6-d1.json" standard final resolved
tgit "$repo" checkout -q -- bin/tool.sh
replace_in "$repo/README.md" "Run the demo." "Run the demo twice."
write_request "$TMP_ROOT/c6-req2.json" "$BASE_SHA" '[]' omit "$TMP_ROOT/c6-d1.json"
reassess "$repo" "$TMP_ROOT/c6-req2.json" "$TMP_ROOT/c6-d2.json" > /dev/null
expect_decision "docs-only change cannot lower standard" "$TMP_ROOT/c6-d2.json" standard final resolved
expect "lowering attempt records no escalation" false "$(field "$TMP_ROOT/c6-d2.json" transition.escalated)"

# === D. refusal of contradictory or invalid inputs =========================

# A valid prior decision to tamper with, one contradiction per case.
repo=$D_REPO
write_request "$TMP_ROOT/d-base-req.json" "$BASE_SHA" '[]' omit
reassess "$repo" "$TMP_ROOT/d-base-req.json" "$TMP_ROOT/d-base.json" > /dev/null
expect_decision "validation base decision is a valid prior" "$TMP_ROOT/d-base.json" documentation-only final resolved

mutate_previous "$TMP_ROOT/d-base.json" "$TMP_ROOT/d-prev-r01.json" 'd["profile"] = "docs-only"'
write_request "$TMP_ROOT/d-r01-req.json" "$BASE_SHA" '[]' omit "$TMP_ROOT/d-prev-r01.json"
expect_blocked "R-01 unknown profile id" "$TMP_ROOT/d-r01-req.json" "not a known profile id"

mutate_previous "$TMP_ROOT/d-base.json" "$TMP_ROOT/d-prev-r02.json" 'd["contract_version"] = 2'
write_request "$TMP_ROOT/d-r02-req.json" "$BASE_SHA" '[]' omit "$TMP_ROOT/d-prev-r02.json"
expect_blocked "R-02 unsupported contract version" "$TMP_ROOT/d-r02-req.json" "contract_version must be 1"

mutate_previous "$TMP_ROOT/d-base.json" "$TMP_ROOT/d-prev-r04.json" 'd["classification_status"] = "unresolved"'
write_request "$TMP_ROOT/d-r04-req.json" "$BASE_SHA" '[]' omit "$TMP_ROOT/d-prev-r04.json"
expect_blocked "R-04 final decision that is unresolved" "$TMP_ROOT/d-r04-req.json" "final decision must be resolved"

mutate_previous "$TMP_ROOT/d-base.json" "$TMP_ROOT/d-prev-r05.json" 'd["target_branch"] = "release"'
write_request "$TMP_ROOT/d-r05-req.json" "$BASE_SHA" '[]' omit "$TMP_ROOT/d-prev-r05.json"
expect_blocked "R-05 target branch differs from the pin" "$TMP_ROOT/d-r05-req.json" "differs from the host-pinned target_branch"

cat > "$TMP_ROOT/d-r06-req.json" <<'JSON'
{
  "task_id": "dpes-765-demo",
  "assignment_intent": "Reassess the profile from the actual change set.",
  "expected_surfaces": ["README.md"],
  "pinned_policy_revision": "rev-test-1",
  "target_branch": "main",
  "risk_triggers": []
}
JSON
expect_blocked "R-06 missing comparison base" "$TMP_ROOT/d-r06-req.json" "request is missing comparison_base_commit"

mutate_previous "$TMP_ROOT/d-base.json" "$TMP_ROOT/d-prev-r10.json" 'del d["evaluated_change_identity"]'
write_request "$TMP_ROOT/d-r10-req.json" "$BASE_SHA" '[]' omit "$TMP_ROOT/d-prev-r10.json"
expect_blocked "R-10 final without change identity" "$TMP_ROOT/d-r10-req.json" "requires evaluated_change_identity"

mutate_previous "$TMP_ROOT/d-base.json" "$TMP_ROOT/d-prev-r11.json" 'd["evidence_obligations"]["tests_required"] = True'
write_request "$TMP_ROOT/d-r11-req.json" "$BASE_SHA" '[]' omit "$TMP_ROOT/d-prev-r11.json"
expect_blocked "R-11 documentation-only with tests required" "$TMP_ROOT/d-r11-req.json" "evidence_obligations do not match"

mutate_previous "$TMP_ROOT/d-base.json" "$TMP_ROOT/d-prev-r12.json" "d[\"risk_triggers\"] = [\"$TRIGGER\"]"
write_request "$TMP_ROOT/d-r12-req.json" "$BASE_SHA" '[]' omit "$TMP_ROOT/d-prev-r12.json"
expect_blocked "R-12 trigger on a non-high-risk profile" "$TMP_ROOT/d-r12-req.json" "require profile high-risk"

mutate_previous "$TMP_ROOT/d-base.json" "$TMP_ROOT/d-prev-r13.json" 'd["decision_stage"] = "provisional"; d["classification_status"] = "unresolved"'
write_request "$TMP_ROOT/d-r13-req.json" "$BASE_SHA" '[]' omit "$TMP_ROOT/d-prev-r13.json"
expect_blocked "R-13 unresolved on a non-high-risk profile" "$TMP_ROOT/d-r13-req.json" "must be provisional high-risk"

mutate_previous "$TMP_ROOT/d-base.json" "$TMP_ROOT/d-prev-r14.json" 'd["authority"] = "merge"'
write_request "$TMP_ROOT/d-r14-req.json" "$BASE_SHA" '[]' omit "$TMP_ROOT/d-prev-r14.json"
expect_blocked "R-14 unknown top-level key in a prior record" "$TMP_ROOT/d-r14-req.json" "unknown keys"

cat > "$TMP_ROOT/d-pin-req.json" <<JSON
{
  "task_id": "dpes-765-demo",
  "assignment_intent": "Reassess the profile from the actual change set.",
  "expected_surfaces": ["README.md"],
  "pinned_policy_revision": "rev-test-1",
  "target_branch": "main",
  "comparison_base_commit": "$BASE_SHA",
  "risk_triggers": [],
  "profile": "documentation-only"
}
JSON
expect_blocked "a request cannot pin a profile" "$TMP_ROOT/d-pin-req.json" "unknown keys: profile"

"$TOOL" reassess --repo "$repo" --request "$TMP_ROOT/no-such-request.json" > /dev/null 2>&1
expect "unreadable request exits 2" 2 "$?"
"$TOOL" > /dev/null 2>&1
expect "missing subcommand exits 2" 2 "$?"

pass "fm-assignment-reassess core: collection, classification, supersession, and refusal"
