#!/usr/bin/env bash
# Behavior tests for the assignment-profile decision record (docs/assignment-profile-policy.md):
# the validator bin/fm-assignment-profile.py, the producer library
# bin/fm-assignment-profile-lib.sh, and the ship launch path in bin/fm-spawn.sh.
#
# The validator is exercised through its command line with real records, and every
# refusal is asserted by its reason, not only its exit code. The launch cases use
# the same spawn world as the managed-context suite, so each refusal is checked to
# stop before a worktree endpoint, a task record, or a launch exists.
set -u

# shellcheck source=tests/fixtures.sh
. "$(dirname "${BASH_SOURCE[0]}")/fixtures.sh"

VALIDATOR="$ROOT/bin/fm-assignment-profile.py"
TMP_ROOT=$(fm_test_tmproot fm-assignment-profile)
GOOD_BASE=0123456789abcdef0123456789abcdef01234567

# validate <record-file> [pin args...]: runs the validator from the code root with
# the isolated Python mode, and sets RC and OUT (stdout and stderr together).
validate() {
  local record=$1
  shift
  OUT=$(python3 -I "$VALIDATOR" validate --code-root "$ROOT" --record "$record" "$@" 2>&1)
  RC=$?
}

# record_with <file> <python-expression-on-dict-d>: a good high-risk record edited
# by a Python expression that mutates dict d. Used to break exactly one field.
record_with() {
  local file=$1 edit=$2
  fm_test_assignment_profile_record T-1 "$GOOD_BASE" >"$file.src"
  python3 - "$file.src" "$file" "$edit" <<'PY'
import json, sys
src, dst, edit = sys.argv[1], sys.argv[2], sys.argv[3]
d = json.load(open(src, encoding="utf-8"))
exec(edit, {"d": d})
open(dst, "w", encoding="utf-8").write(json.dumps(d, indent=2) + "\n")
PY
}

# profile_record <profile> <task> <comparison-base-commit> <file>: a provisional decision
# record for one profile, with the stage set and evidence obligations that
# docs/assignment-profile-policy.md lists for it. The high-risk record is the fixture's.
profile_record() {
  local profile=$1 task=$2 base=$3 file=$4
  fm_test_assignment_profile_record "$task" "$base" >"$file.src"
  python3 - "$file.src" "$file" "$profile" <<'PY'
import json, sys
src, dst, profile = sys.argv[1], sys.argv[2], sys.argv[3]
stages = {
    "documentation-only": ["ground-scope", "implement", "focused-review", "verify", "change-reassessment", "delivery-handoff"],
    "small-low-risk": ["ground-scope", "short-plan", "implement", "focused-review", "verify", "change-reassessment", "delivery-handoff"],
    "standard": ["ground-scope", "design", "implement", "focused-review", "verify", "change-reassessment", "delivery-handoff"],
    "high-risk": ["ground-scope", "design", "design-critique", "implement", "independent-change-review", "risk-verify", "change-reassessment", "delivery-handoff"],
}
d = json.load(open(src, encoding="utf-8"))
d["profile"] = profile
d["required_stages"] = stages[profile]
if profile != "high-risk":
    d["risk_triggers"] = []
    d["evidence_obligations"] = {"tests_required": profile != "documentation-only", "independent_change_review_required": False}
open(dst, "w", encoding="utf-8").write(json.dumps(d, indent=2) + "\n")
PY
}

# --- the validator accepts a well-formed record and prints its canonical form ---

test_valid_record_is_accepted_and_canonical() {
  local dir=$TMP_ROOT/valid rec
  mkdir -p "$dir"
  rec="$dir/record.json"
  fm_test_assignment_profile_record T-1 "$GOOD_BASE" >"$rec"
  validate "$rec" --task T-1 --target-branch main --base-commit "$GOOD_BASE" --require-current
  expect_code 0 "$RC" "a valid high-risk record must be accepted ($OUT)"
  assert_contains "$OUT" '"decision_id": "T-1/1"' "the canonical form must carry the decision id"
  pass "fm-assignment-profile.py: a valid high-risk record is accepted and printed in canonical form"
}

test_canonical_form_is_stable_across_key_order() {
  local dir=$TMP_ROOT/canon first second
  mkdir -p "$dir"
  fm_test_assignment_profile_record T-1 "$GOOD_BASE" >"$dir/a.json"
  python3 -c '
import json, sys
d = json.load(open(sys.argv[1]))
print(json.dumps(dict(reversed(list(d.items()))), indent=4))
' "$dir/a.json" >"$dir/b.json"
  validate "$dir/a.json" --task T-1
  first=$OUT
  validate "$dir/b.json" --task T-1
  second=$OUT
  assert_equals "$first" "$second" "key order must not change the canonical record"
  pass "fm-assignment-profile.py: the canonical record is independent of the input's key order"
}

# --- schema refusals, one field each (R-01, R-02, R-04, R-06, R-10, R-12, R-13, R-14) ---

test_unknown_profile_is_refused() {  # R-01
  local dir=$TMP_ROOT/r01
  mkdir -p "$dir"
  record_with "$dir/r.json" 'd["profile"] = "docs-only"'
  validate "$dir/r.json" --task T-1
  expect_code 1 "$RC" "an unknown profile must be refused"
  assert_contains "$OUT" "assignment profile refused" "the refusal must use the refusal prefix"
  pass "fm-assignment-profile.py: R-01 an unknown profile id is refused, not coerced"
}

test_unsupported_contract_version_is_refused() {  # R-02
  local dir=$TMP_ROOT/r02
  mkdir -p "$dir"
  record_with "$dir/r.json" 'd["contract_version"] = 2'
  validate "$dir/r.json" --task T-1
  expect_code 1 "$RC" "contract_version 2 must be refused"
  assert_contains "$OUT" "'/contract_version' must be 1" "the refusal must name the field"
  pass "fm-assignment-profile.py: R-02 an unsupported contract version is refused and names its field"
}

test_final_record_with_unresolved_classification_is_refused() {  # R-04
  local dir=$TMP_ROOT/r04
  mkdir -p "$dir"
  record_with "$dir/r.json" 'd["decision_stage"] = "final"; d["classification_status"] = "unresolved"'
  validate "$dir/r.json" --task T-1
  expect_code 1 "$RC" "a final record with unresolved classification must be refused"
  pass "fm-assignment-profile.py: R-04 a final record cannot carry unresolved classification"
}

test_missing_comparison_base_is_refused() {  # R-06
  local dir=$TMP_ROOT/r06
  mkdir -p "$dir"
  record_with "$dir/r.json" 'del d["comparison_base_commit"]'
  validate "$dir/r.json" --task T-1
  expect_code 1 "$RC" "a record without comparison_base_commit must be refused"
  assert_contains "$OUT" "comparison_base_commit" "the refusal must name the missing field"
  pass "fm-assignment-profile.py: R-06 a missing comparison_base_commit is refused"
}

test_final_record_without_change_identity_is_refused() {  # R-10
  local dir=$TMP_ROOT/r10
  mkdir -p "$dir"
  record_with "$dir/r.json" 'd["decision_stage"] = "final"; d["classification_status"] = "resolved"; d.pop("evaluated_change_identity", None)'
  validate "$dir/r.json" --task T-1
  expect_code 1 "$RC" "a final record without an evaluated change identity must be refused"
  pass "fm-assignment-profile.py: R-10 a final record must name its evaluated change identity"
}

test_risk_triggers_on_non_high_risk_profile_are_refused() {  # R-12
  local dir=$TMP_ROOT/r12
  mkdir -p "$dir"
  record_with "$dir/r.json" 'd["profile"] = "standard"; d["decision_stage"] = "final"; d["classification_status"] = "resolved"; d["required_stages"] = ["ground-scope","design","implement","focused-review","verify","change-reassessment","delivery-handoff"]; d["evidence_obligations"]["independent_change_review_required"] = False'
  validate "$dir/r.json" --task T-1
  expect_code 1 "$RC" "a risk trigger on a standard record must be refused"
  pass "fm-assignment-profile.py: R-12 any high-risk trigger selects high-risk, never standard"
}

test_unresolved_on_non_provisional_high_risk_is_refused() {  # R-13
  local dir=$TMP_ROOT/r13
  mkdir -p "$dir"
  record_with "$dir/r.json" 'd["decision_stage"] = "final"; d["classification_status"] = "unresolved"'
  validate "$dir/r.json" --task T-1
  expect_code 1 "$RC" "unresolved classification on final high-risk must be refused"
  pass "fm-assignment-profile.py: R-13 unresolved classification is provisional high-risk only"
}

test_unknown_top_level_key_is_refused() {  # R-14
  local dir=$TMP_ROOT/r14
  mkdir -p "$dir"
  record_with "$dir/r.json" 'd["authority_override"] = "merge"'
  validate "$dir/r.json" --task T-1
  expect_code 1 "$RC" "an unknown top-level key must be refused"
  assert_contains "$OUT" "authority_override" "the refusal must name the unknown key"
  pass "fm-assignment-profile.py: R-14 an unknown top-level key is refused by name"
}

# --- documentation-only and standard evidence rules (R-11, D-01, D-09) ---

test_documentation_only_with_tests_is_refused() {  # R-11
  local dir=$TMP_ROOT/r11
  mkdir -p "$dir"
  record_with "$dir/r.json" 'd["profile"] = "documentation-only"; d["decision_stage"] = "final"; d["classification_status"] = "resolved"; d["risk_triggers"] = []; d["required_stages"] = ["ground-scope","implement","focused-review","verify","change-reassessment","delivery-handoff"]; d["evidence_obligations"] = {"tests_required": True, "independent_change_review_required": False}; d["evaluated_change_identity"] = "sha256:0"'
  validate "$dir/r.json" --task T-1
  expect_code 1 "$RC" "documentation-only with tests_required must be refused"
  pass "fm-assignment-profile.py: R-11 documentation-only requires zero tests"
}

test_documentation_only_profile_is_accepted() {  # D-01 (shape)
  local dir=$TMP_ROOT/d01
  mkdir -p "$dir"
  record_with "$dir/r.json" 'd["profile"] = "documentation-only"; d["decision_stage"] = "final"; d["classification_status"] = "resolved"; d["risk_triggers"] = []; d["required_stages"] = ["ground-scope","implement","focused-review","verify","change-reassessment","delivery-handoff"]; d["evidence_obligations"] = {"tests_required": False, "independent_change_review_required": False}; d["evaluated_change_identity"] = "sha256:0"'
  validate "$dir/r.json" --task T-1
  expect_code 0 "$RC" "a documentation-only record with zero tests must be accepted ($OUT)"
  pass "fm-assignment-profile.py: D-01 a documentation-only record with zero tests is accepted"
}

test_small_low_risk_profile_is_accepted() {  # D-03 (shape)
  local dir=$TMP_ROOT/d03
  mkdir -p "$dir"
  record_with "$dir/r.json" 'd["profile"] = "small-low-risk"; d["decision_stage"] = "final"; d["classification_status"] = "resolved"; d["risk_triggers"] = []; d["required_stages"] = ["ground-scope","short-plan","implement","focused-review","verify","change-reassessment","delivery-handoff"]; d["evidence_obligations"] = {"tests_required": True, "independent_change_review_required": False}; d["evaluated_change_identity"] = "sha256:0"'
  validate "$dir/r.json" --task T-1
  expect_code 0 "$RC" "a small-low-risk record with tests must be accepted ($OUT)"
  pass "fm-assignment-profile.py: D-03 a small bounded behavior change is accepted as small-low-risk"
}

# --- change coverage (D-07): incomplete collection cannot complete as documentation-only ---

test_incomplete_change_coverage_is_refused_for_final() {  # D-07
  local dir=$TMP_ROOT/d07
  mkdir -p "$dir"
  record_with "$dir/r.json" 'd["decision_stage"] = "final"; d["change_coverage"]["complete"] = False'
  validate "$dir/r.json" --task T-1
  expect_code 1 "$RC" "a final record with incomplete change coverage must be refused"
  pass "fm-assignment-profile.py: D-07 incomplete change coverage cannot be final"
}

# --- duplicates, NaN, and malformed input (the parser refuses, it does not guess) ---

test_duplicate_key_is_refused() {
  local dir=$TMP_ROOT/dup
  mkdir -p "$dir"
  fm_test_assignment_profile_record T-1 "$GOOD_BASE" | sed 's/"profile": "high-risk",/"profile": "high-risk", "profile": "standard",/' >"$dir/r.json"
  validate "$dir/r.json" --task T-1
  expect_code 1 "$RC" "a duplicate key must be refused"
  assert_contains "$OUT" "duplicate key 'profile'" "the refusal must name the duplicate key"
  pass "fm-assignment-profile.py: a duplicate key is refused by name, not last-wins"
}

test_nan_constant_is_refused() {
  local dir=$TMP_ROOT/nan
  mkdir -p "$dir"
  fm_test_assignment_profile_record T-1 "$GOOD_BASE" | sed 's/"contract_version": 1/"contract_version": NaN/' >"$dir/r.json"
  validate "$dir/r.json" --task T-1
  expect_code 1 "$RC" "a NaN constant must be refused"
  pass "fm-assignment-profile.py: a NaN constant is refused"
}

test_unreadable_record_cannot_run_is_exit_two() {
  local dir=$TMP_ROOT/unrun
  mkdir -p "$dir"
  printf 'not json\n' >"$dir/r.json"
  validate "$dir/r.json" --task T-1
  expect_code 1 "$RC" "a record that is not JSON must be refused as a record problem"
  pass "fm-assignment-profile.py: a malformed record is refused as a record problem"
}

# --- pins: task, target branch, base commit, and the current revision ---

test_task_id_mismatch_is_refused() {
  local dir=$TMP_ROOT/pin-task
  mkdir -p "$dir"
  fm_test_assignment_profile_record T-1 "$GOOD_BASE" >"$dir/r.json"
  validate "$dir/r.json" --task T-2
  expect_code 1 "$RC" "a record for another task must be refused"
  assert_contains "$OUT" "contradictory decision" "the refusal must call the contradiction out"
  pass "fm-assignment-profile.py: a record for another task id is refused"
}

test_target_branch_pin_mismatch_is_refused() {
  local dir=$TMP_ROOT/pin-target
  mkdir -p "$dir"
  fm_test_assignment_profile_record T-1 "$GOOD_BASE" >"$dir/r.json"
  validate "$dir/r.json" --task T-1 --target-branch release
  expect_code 1 "$RC" "a target branch that differs from the host pin must be refused"
  assert_contains "$OUT" "target_branch is 'main' but the host pins 'release'" "the refusal must state both branches"
  pass "fm-assignment-profile.py: R-05 a target branch that differs from the host pin is refused"
}

test_base_commit_pin_mismatch_is_refused() {
  local dir=$TMP_ROOT/pin-base other
  mkdir -p "$dir"
  other=ffffffffffffffffffffffffffffffffffffffff
  fm_test_assignment_profile_record T-1 "$GOOD_BASE" >"$dir/r.json"
  validate "$dir/r.json" --task T-1 --base-commit "$other"
  expect_code 1 "$RC" "a base commit that differs from the host pin must be refused"
  assert_contains "$OUT" "the decision was written against another base" "the refusal must explain the base mismatch"
  pass "fm-assignment-profile.py: a base commit that differs from the host pin is refused"
}

test_unapproved_revision_is_refused() {
  local dir=$TMP_ROOT/rev-unknown
  mkdir -p "$dir"
  record_with "$dir/r.json" 'd["pinned_policy_revision"] = "motif-assignment-profile-r9"'
  validate "$dir/r.json" --task T-1
  expect_code 1 "$RC" "a revision absent from the registry must be refused"
  assert_contains "$OUT" "is not in the approved revision registry" "the refusal must say the revision is unapproved"
  pass "fm-assignment-profile.py: R-09 a revision absent from the registry blocks routing"
}

test_require_current_refuses_a_non_current_revision() {
  local dir=$TMP_ROOT/rev-current
  mkdir -p "$dir"
  record_with "$dir/r.json" 'd["pinned_policy_revision"] = "motif-assignment-profile-r1"'
  # Point the registry's current at a revision that is not r1 in a copy of the tree,
  # so require-current has to refuse even though r1 is approved.
  mkdir -p "$dir/tree/docs"
  cp -- "$ROOT/docs/assignment-profile-policy.md" "$ROOT/docs/assignment-profile-decision.v1.schema.json" "$dir/tree/docs/"
  python3 - "$ROOT/docs/assignment-profile-revisions.json" "$dir/tree/docs/assignment-profile-revisions.json" <<'PY'
import json, sys
d = json.load(open(sys.argv[1], encoding="utf-8"))
d["current"] = "motif-assignment-profile-r2"
json.dump(d, open(sys.argv[2], "w", encoding="utf-8"))
PY
  OUT=$(python3 -I "$VALIDATOR" validate --code-root "$dir/tree" --record "$dir/r.json" --task T-1 --require-current 2>&1)
  RC=$?
  expect_code 1 "$RC" "require-current must refuse a record that does not pin the current revision"
  assert_contains "$OUT" "is not the host-selected current revision" "the refusal must name the current revision"
  pass "fm-assignment-profile.py: a new launch must pin the registry's current revision"
}

test_policy_material_change_breaks_the_pinned_revision() {  # R-07 (tamper detection)
  local dir=$TMP_ROOT/tamper
  mkdir -p "$dir/tree/docs"
  cp -- "$ROOT/docs/assignment-profile-policy.md" "$ROOT/docs/assignment-profile-decision.v1.schema.json" "$ROOT/docs/assignment-profile-revisions.json" "$dir/tree/docs/"
  printf '\n<!-- tampered -->\n' >>"$dir/tree/docs/assignment-profile-policy.md"
  fm_test_assignment_profile_record T-1 "$GOOD_BASE" >"$dir/r.json"
  OUT=$(python3 -I "$VALIDATOR" validate --code-root "$dir/tree" --record "$dir/r.json" --task T-1 2>&1)
  RC=$?
  expect_code 1 "$RC" "a changed policy file must break its pinned revision"
  assert_contains "$OUT" "differs from the approved content of revision" "the refusal must name the changed material"
  pass "fm-assignment-profile.py: R-07 changed policy material refuses the pinned revision"
}

# --- the producer library: block extraction and the one-block rule ---

test_brief_without_a_block_is_refused_by_the_library() {
  local dir=$TMP_ROOT/lib-none out rc
  mkdir -p "$dir"
  printf '# Task\nno block here\n' >"$dir/brief.md"
  out=$(bash -c '. "$1"; fm_assignment_profile_load "$2" fm-test T-1 0' _ "$ROOT/bin/fm-assignment-profile-lib.sh" "$dir/brief.md" 2>&1); rc=$?
  expect_code 1 "$rc" "a brief with no assignment-profile block must be refused"
  assert_contains "$out" "carries no assignment-profile block" "the refusal must name the missing block"
  pass "fm-assignment-profile-lib.sh: a ship brief without its record is refused by name"
}

test_brief_with_two_blocks_is_refused_by_the_library() {
  local dir=$TMP_ROOT/lib-two out rc
  mkdir -p "$dir"
  fm_test_assignment_profile_section T-1 "$GOOD_BASE" >"$dir/brief.md"
  fm_test_assignment_profile_section T-1 "$GOOD_BASE" >>"$dir/brief.md"
  out=$(bash -c '. "$1"; fm_assignment_profile_load "$2" fm-test T-1 0' _ "$ROOT/bin/fm-assignment-profile-lib.sh" "$dir/brief.md" 2>&1); rc=$?
  expect_code 1 "$rc" "a brief with two assignment-profile blocks must be refused"
  assert_contains "$out" "more than one assignment-profile block" "the refusal must name the duplicate block"
  pass "fm-assignment-profile-lib.sh: exactly one assignment-profile block is allowed"
}

test_section_renders_obligations_and_the_carried_block() {
  local dir=$TMP_ROOT/lib-section out rc
  mkdir -p "$dir"
  fm_test_assignment_profile_record T-1 "$GOOD_BASE" >"$dir/r.json"
  out=$(bash -c '. "$1"; fm_assignment_profile_section "$2" fm-test T-1' _ "$ROOT/bin/fm-assignment-profile-lib.sh" "$dir/r.json" 2>&1); rc=$?
  expect_code 0 "$rc" "a current high-risk record must render its section ($out)"
  assert_contains "$out" '```assignment-profile' "the section must carry the fenced record"
  assert_contains "$out" "Policy revision \`motif-assignment-profile-r2\`" "the obligations must name the pinned revision"
  pass "fm-assignment-profile-lib.sh: the brief section renders obligations and the fenced record"
}

test_persisted_record_restores_only_when_unchanged() {
  local dir=$TMP_ROOT/lib-restore brief persisted sha canon out rc
  mkdir -p "$dir"
  brief="$dir/brief.md"
  persisted="$dir/assignment-profile.json"
  fm_test_assignment_profile_section T-1 "$GOOD_BASE" >"$brief"
  canon=$(bash -c '. "$1"; fm_assignment_profile_load "$2" fm-test T-1 1' _ "$ROOT/bin/fm-assignment-profile-lib.sh" "$brief") || fail "a fresh record must load"
  bash -c '. "$1"; fm_assignment_profile_persist "$2" "$3"' _ "$ROOT/bin/fm-assignment-profile-lib.sh" "$persisted" "$canon" || fail "persist must succeed"
  sha=$(bash -c '. "$1"; fm_assignment_profile_sha "$2"' _ "$ROOT/bin/fm-assignment-profile-lib.sh" "$persisted")
  out=$(bash -c '. "$1"; fm_assignment_profile_restore_check "$2" "$3" "$4" T-1 fm-test' _ "$ROOT/bin/fm-assignment-profile-lib.sh" "$persisted" "$sha" "$brief" 2>&1); rc=$?
  expect_code 0 "$rc" "an unchanged persisted record must restore ($out)"

  printf 'tampered\n' >>"$persisted"
  out=$(bash -c '. "$1"; fm_assignment_profile_restore_check "$2" "$3" "$4" T-1 fm-test' _ "$ROOT/bin/fm-assignment-profile-lib.sh" "$persisted" "$sha" "$brief" 2>&1); rc=$?
  expect_code 1 "$rc" "a persisted record whose hash changed must refuse to relaunch"
  assert_contains "$out" "no longer matches its recorded hash" "the refusal must name the hash mismatch"
  pass "fm-assignment-profile-lib.sh: a relaunch restores an unchanged record and refuses a changed one"
}

test_relaunch_refuses_a_brief_with_a_different_record() {
  local dir=$TMP_ROOT/lib-brief-diff brief persisted sha canon out rc other
  mkdir -p "$dir"
  brief="$dir/brief.md"
  persisted="$dir/assignment-profile.json"
  fm_test_assignment_profile_section T-1 "$GOOD_BASE" >"$brief"
  canon=$(bash -c '. "$1"; fm_assignment_profile_load "$2" fm-test T-1 1' _ "$ROOT/bin/fm-assignment-profile-lib.sh" "$brief") || fail "a fresh record must load"
  bash -c '. "$1"; fm_assignment_profile_persist "$2" "$3"' _ "$ROOT/bin/fm-assignment-profile-lib.sh" "$persisted" "$canon" || fail "persist must succeed"
  sha=$(bash -c '. "$1"; fm_assignment_profile_sha "$2"' _ "$ROOT/bin/fm-assignment-profile-lib.sh" "$persisted")
  other="$dir/other.brief.md"
  fm_test_assignment_profile_record T-1 ffffffffffffffffffffffffffffffffffffffff >"$dir/other.json"
  fm_test_assignment_profile_section T-1 ffffffffffffffffffffffffffffffffffffffff >"$other"
  out=$(bash -c '. "$1"; fm_assignment_profile_restore_check "$2" "$3" "$4" T-1 fm-test' _ "$ROOT/bin/fm-assignment-profile-lib.sh" "$persisted" "$sha" "$other" 2>&1); rc=$?
  expect_code 1 "$rc" "a brief whose record differs from the persisted one must refuse to relaunch"
  assert_contains "$out" "differs from the one task T-1 launched with" "the refusal must name the brief mismatch"
  pass "fm-assignment-profile-lib.sh: a relaunch refuses a brief that carries a different record"
}

test_missing_persisted_record_blocks_relaunch() {
  local dir=$TMP_ROOT/lib-missing out rc
  mkdir -p "$dir"
  fm_test_assignment_profile_section T-1 "$GOOD_BASE" >"$dir/brief.md"
  out=$(bash -c '. "$1"; fm_assignment_profile_restore_check "$2" "" "$3" T-1 fm-test' _ "$ROOT/bin/fm-assignment-profile-lib.sh" "$dir/none.json" "$dir/brief.md" 2>&1); rc=$?
  expect_code 1 "$rc" "a ship without a persisted record must stay blocked on relaunch"
  assert_contains "$out" "has no persisted assignment profile" "the refusal must say the record is absent"
  pass "fm-assignment-profile-lib.sh: a legacy ship without a persisted record stays blocked"
}

# --- fm-brief.sh: the flag is ship-only and the record must be current ---

test_brief_refuses_a_record_for_another_task() {
  local home=$TMP_ROOT/brief-other-task rec out rc
  mkdir -p "$home"
  rec=$(fm_test_ship_profile_file "$home/profiles" T-OTHER)
  out=$(FM_HOME="$home" "$ROOT/bin/fm-brief.sh" T-1 some-proj --mode no-mistakes --assignment-profile "$rec" 2>&1); rc=$?
  expect_code 1 "$rc" "a brief must refuse a record written for another task"
  assert_contains "$out" "contradictory decision" "the refusal must name the task contradiction"
  [ ! -e "$home/data/T-1/brief.md" ] || fail "a refused brief must leave no brief behind"
  pass "fm-brief.sh: a record for another task refuses the brief and leaves none behind"
}

test_brief_refuses_the_flag_on_a_scout() {
  local home=$TMP_ROOT/brief-scout rec out rc
  mkdir -p "$home"
  rec=$(fm_test_ship_profile_file "$home/profiles" T-S)
  out=$(FM_HOME="$home" "$ROOT/bin/fm-brief.sh" T-S some-proj --scout --assignment-profile "$rec" 2>&1); rc=$?
  expect_code 1 "$rc" "a scout must refuse the assignment-profile flag"
  assert_contains "$out" "applies to ship briefs only" "the refusal must name the ship-only rule"
  pass "fm-brief.sh: the assignment-profile flag is refused on a scout"
}

# --- spawn: the record is checked before any endpoint, and relaunch restores it ---

# spawn_world <case-dir>: the fakebin a ship spawn needs, built the way the
# managed-context suite builds it (a no-op timeout, since this host lacks one).
spawn_world() {
  local fakebin
  fakebin=$(fm_test_make_spawn_fakebin "$1/fake")
  cat > "$fakebin/timeout" <<'SH'
#!/usr/bin/env bash
shift
exec "$@"
SH
  chmod +x "$fakebin/timeout"
  # Every endpoint the spawn creates is logged to <case>/endpoint.log, so a refusal
  # is checked to stop before one exists. The stub behind the wrapper answers the
  # spawn's other tmux calls unchanged.
  mv "$fakebin/tmux" "$fakebin/tmux-stub"
  cat > "$fakebin/tmux" <<SH
#!/usr/bin/env bash
for a in "\$@"; do
  case "\$a" in new-session|new-window) printf '%s\n' "\$*" >>"$1/endpoint.log" ;; esac
done
exec "$fakebin/tmux-stub" "\$@"
SH
  chmod +x "$fakebin/tmux"
  printf '%s\n' "$fakebin"
}

test_ship_spawn_without_a_record_refuses_before_any_launch() {
  local case_dir home proj wt fakebin launchlog out rc id=T-NOREC
  case_dir=$TMP_ROOT/spawn-no-record
  home="$case_dir/home"
  proj="$case_dir/project"
  wt="$case_dir/wt"
  launchlog="$case_dir/launch.log"
  fakebin=$(spawn_world "$case_dir")
  fm_test_spawn_home "$home" codex
  fm_git_worktree "$proj" "$wt" "wt-spawn-no-record"
  # The managed-context block is valid, so the only missing piece is the record.
  fm_test_spawn_brief "$home" "$id"
  fm_test_managed_block "$id" no-mistakes >>"$home/data/$id/brief.md"
  : >"$launchlog"
  out=$(FM_TEST_NO_ASSIGNMENT_PROFILE=1 fm_test_run_spawn "$home" "$wt" "$fakebin" "$id" "$proj" --mode no-mistakes --yolo off --branch-prefix fm/); rc=$?
  expect_code 1 "$rc" "a ship spawn with no record must refuse ($out)"
  assert_contains "$out" "carries no assignment-profile block" "the refusal must name the missing record"
  [ ! -s "$launchlog" ] || fail "a refused spawn must not launch a worker"
  [ ! -e "$home/state/$id.meta" ] || fail "a refused spawn must not write a task record"
  [ ! -s "$case_dir/endpoint.log" ] || fail "a refused spawn must not create an endpoint"
  pass "fm-spawn.sh: a ship spawn without its record refuses before any launch or task record"
}

test_ship_spawn_with_a_record_pinned_to_another_base_refuses() {
  local case_dir home proj wt fakebin launchlog out rc id=T-BASE
  case_dir=$TMP_ROOT/spawn-base
  home="$case_dir/home"
  proj="$case_dir/project"
  wt="$case_dir/wt"
  launchlog="$case_dir/launch.log"
  fakebin=$(spawn_world "$case_dir")
  fm_test_spawn_home "$home" codex
  fm_git_worktree "$proj" "$wt" "wt-spawn-base"
  fm_test_spawn_brief "$home" "$id"
  fm_test_assignment_profile_section "$id" "$GOOD_BASE" >>"$home/data/$id/brief.md"
  fm_test_managed_block "$id" no-mistakes >>"$home/data/$id/brief.md"
  : >"$launchlog"
  out=$(FM_TEST_NO_ASSIGNMENT_PROFILE=1 fm_test_run_spawn "$home" "$wt" "$fakebin" "$id" "$proj" --mode no-mistakes --yolo off --branch-prefix fm/); rc=$?
  expect_code 1 "$rc" "a record pinned to a base other than the worktree HEAD must refuse ($out)"
  assert_contains "$out" "the decision was written against another base" "the refusal must name the base pin"
  [ ! -s "$launchlog" ] || fail "a base-mismatched spawn must not launch a worker"
  pass "fm-spawn.sh: a record pinned to another comparison base refuses before launch"
}

test_ship_spawn_with_another_target_refuses_before_any_endpoint() {
  local case_dir home proj wt fakebin launchlog out rc id=T-TARGET
  case_dir=$TMP_ROOT/spawn-target
  home="$case_dir/home"
  proj="$case_dir/project"
  wt="$case_dir/wt"
  launchlog="$case_dir/launch.log"
  fakebin=$(spawn_world "$case_dir")
  fm_test_spawn_home "$home" codex
  fm_git_worktree "$proj" "$wt" "wt-spawn-target"
  fm_test_spawn_brief "$home" "$id"
  # The brief and the spawn agree on release-x; only the record pins the default target.
  {
    printf '%s\n' 'You are in a disposable git worktree of project, at a detached HEAD on a clean copy of its base branch.' 'Base branch: release-x'
    fm_test_assignment_profile_section "$id" "$GOOD_BASE"
    fm_test_managed_block "$id" no-mistakes
  } >>"$home/data/$id/brief.md"
  : >"$launchlog"
  out=$(FM_TEST_NO_ASSIGNMENT_PROFILE=1 fm_test_run_spawn "$home" "$wt" "$fakebin" "$id" "$proj" --mode no-mistakes --yolo off --branch-prefix fm/ --base-branch release-x); rc=$?
  expect_code 1 "$rc" "a record pinned to another target than the launch's must refuse ($out)"
  assert_contains "$out" "contradictory decision" "the refusal must name the contradiction"
  assert_contains "$out" "release-x" "the refusal must name the host's target pin"
  [ ! -s "$launchlog" ] || fail "a target-mismatched spawn must not launch a worker"
  [ ! -s "$case_dir/endpoint.log" ] || fail "a target-mismatched spawn must not create an endpoint"
  pass "fm-spawn.sh: a record pinned to another target refuses before any worker launch"
}

test_ship_spawn_with_the_pinned_record_persists_and_records_the_decision() {
  local case_dir home proj wt fakebin launchlog out rc id=T-PERSIST base meta persisted
  case_dir=$TMP_ROOT/spawn-persist
  home="$case_dir/home"
  proj="$case_dir/project"
  wt="$case_dir/wt"
  launchlog="$case_dir/launch.log"
  fakebin=$(spawn_world "$case_dir")
  fm_test_spawn_home "$home" codex
  fm_git_worktree "$proj" "$wt" "wt-spawn-persist"
  base=$(git -C "$wt" rev-parse HEAD)
  fm_test_spawn_brief "$home" "$id"
  fm_test_assignment_profile_section "$id" "$base" >>"$home/data/$id/brief.md"
  fm_test_managed_block "$id" no-mistakes >>"$home/data/$id/brief.md"
  : >"$launchlog"
  out=$(FM_TEST_NO_ASSIGNMENT_PROFILE=1 fm_test_run_spawn "$home" "$wt" "$fakebin" "$id" "$proj" --mode no-mistakes --yolo off --branch-prefix fm/); rc=$?
  expect_code 0 "$rc" "a ship spawn with its pinned record must launch ($out)"
  persisted="$home/data/$id/assignment-profile.json"
  [ -f "$persisted" ] || fail "a launched ship must persist its canonical record at $persisted"
  meta="$home/state/$id.meta"
  grep -qx 'profile_decision_id=T-PERSIST/1' "$meta" || fail "the task record must carry profile_decision_id"
  grep -qx 'profile_policy_revision=motif-assignment-profile-r2' "$meta" || fail "the task record must carry profile_policy_revision"
  grep -q '^profile_record_sha256=[0-9a-f]\{64\}$' "$meta" || fail "the task record must carry the record hash"
  pass "fm-spawn.sh: a launched ship persists its record and writes the profile keys into its task record"
}

# Each profile's brief is rendered by bin/fm-brief.sh from its own record, and the launch
# accepts that brief: the obligations a worker reads state what the profile requires, the
# brief and the record agree before any endpoint, and the decision is persisted with its name.
test_each_profile_brief_agrees_with_its_launch() {
  local profile case_dir home proj wt fakebin out rc base id rec brief
  for profile in documentation-only small-low-risk standard high-risk; do
    id="T-$profile"
    case_dir=$TMP_ROOT/profile-$profile
    home="$case_dir/home"
    proj="$case_dir/project"
    wt="$case_dir/wt"
    fakebin=$(spawn_world "$case_dir")
    fm_test_spawn_home "$home" codex
    fm_git_worktree "$proj" "$wt" "wt-profile-$profile"
    base=$(git -C "$wt" rev-parse HEAD)
    rec="$case_dir/record.json"
    profile_record "$profile" "$id" "$base" "$rec"
    out=$(FM_HOME="$home" "$ROOT/bin/fm-brief.sh" "$id" --assignment-profile "$rec" some-proj --mode no-mistakes --branch-prefix fm/ 2>&1); rc=$?
    expect_code 0 "$rc" "fm-brief.sh must render a $profile brief ($out)"
    brief="$home/data/$id/brief.md"
    sed "s/{TASK}/Exercise the $profile agreement./; s/{FIRSTMATE_SPEC}/Exercise the spawn behavior under test./" \
      "$brief" >"$brief.filled" && mv "$brief.filled" "$brief"
    if [ "$profile" = documentation-only ]; then
      grep -q 'this profile requires zero tests executed' "$brief" || fail "$profile: the brief must say the profile runs no tests"
    else
      grep -q 'required by this profile' "$brief" || fail "$profile: the brief must require tests"
    fi
    out=$(FM_TEST_NO_ASSIGNMENT_PROFILE=1 fm_test_run_spawn "$home" "$wt" "$fakebin" "$id" "$proj" --mode no-mistakes --yolo off --branch-prefix fm/); rc=$?
    expect_code 0 "$rc" "a $profile brief must launch on the record it was rendered from ($out)"
    [ -s "$case_dir/endpoint.log" ] || fail "$profile: a launched brief must create its worker endpoint"
    grep -qx "profile_name=$profile" "$home/state/$id.meta" || fail "$profile: the task record must name the profile"
    pass "fm-brief.sh and fm-spawn.sh agree for the $profile profile: the brief renders its record and the launch accepts it"
  done
}

# A brief's Assignment profile section and its definition-of-done copy both state the
# obligations the record lists; bin/fm-assignment-profile-lib.sh renders each one. A brief
# whose copy disagrees with its record must refuse before any endpoint, task record, or
# launch, because the worker would otherwise read obligations the decision does not carry.
test_ship_spawn_with_a_brief_that_disagrees_with_its_record_refuses_before_any_endpoint() {
  local case_dir mode home proj wt fakebin launchlog out rc base want id=T-AGREE
  for mode in section dod; do
    case_dir=$TMP_ROOT/spawn-agree-$mode
    home="$case_dir/home"
    proj="$case_dir/project"
    wt="$case_dir/wt"
    launchlog="$case_dir/launch.log"
    fakebin=$(spawn_world "$case_dir")
    fm_test_spawn_home "$home" codex
    fm_git_worktree "$proj" "$wt" "wt-spawn-agree-$mode"
    base=$(git -C "$wt" rev-parse HEAD)
    fm_test_spawn_brief "$home" "$id"
    fm_test_assignment_profile_section "$id" "$base" >>"$home/data/$id/brief.md"
    fm_test_managed_block "$id" no-mistakes >>"$home/data/$id/brief.md"
    # Tamper one copy only. The record lists the tests obligation as required; the
    # tampered copy says the profile runs none. The section is the first copy in the
    # brief and the definition-of-done copy the last, so pick by mode.
    tamper_obligation_copy "$home/data/$id/brief.md" "$mode"
    : >"$launchlog"
    out=$(FM_TEST_NO_ASSIGNMENT_PROFILE=1 fm_test_run_spawn "$home" "$wt" "$fakebin" "$id" "$proj" --mode no-mistakes --yolo off --branch-prefix fm/); rc=$?
    if [ "$mode" = section ]; then want="Assignment profile section"; else want="definition-of-done copy"; fi
    expect_code 1 "$rc" "a brief whose $mode copy disagrees with its record must refuse ($out)"
    assert_contains "$out" "$want does not state the obligations its decision record lists" \
      "the refusal must name the copy that disagrees"
    [ ! -s "$launchlog" ] || fail "a brief that disagrees with its record must not launch a worker"
    [ ! -s "$case_dir/endpoint.log" ] || fail "a brief that disagrees with its record must not create an endpoint: $(cat "$case_dir/endpoint.log")"
    [ ! -e "$home/state/$id.meta" ] || fail "a brief that disagrees with its record must not write a task record"
  done
  pass "fm-spawn.sh: a brief whose section or definition-of-done copy disagrees with its record refuses before any endpoint, task record, or launch"
}

test_ship_spawn_with_a_malformed_record_refuses_before_any_endpoint() {
  local case_dir home proj wt fakebin launchlog out rc id=T-MALFORMED
  case_dir=$TMP_ROOT/spawn-malformed
  home="$case_dir/home"
  proj="$case_dir/project"
  wt="$case_dir/wt"
  launchlog="$case_dir/launch.log"
  fakebin=$(spawn_world "$case_dir")
  fm_test_spawn_home "$home" codex
  fm_git_worktree "$proj" "$wt" "wt-spawn-malformed"
  fm_test_spawn_brief "$home" "$id"
  # A truncated record: the block is present and fenced, but its JSON never closes.
  {
    printf '%s\n' '# Assignment profile' '```assignment-profile' '{ "contract_version": 1,' '```'
    fm_test_managed_block "$id" no-mistakes
  } >>"$home/data/$id/brief.md"
  : >"$launchlog"
  out=$(FM_TEST_NO_ASSIGNMENT_PROFILE=1 fm_test_run_spawn "$home" "$wt" "$fakebin" "$id" "$proj" --mode no-mistakes --yolo off --branch-prefix fm/); rc=$?
  expect_code 1 "$rc" "a malformed assignment profile must refuse ($out)"
  assert_contains "$out" "assignment profile refused" "the refusal must come from the record validator"
  [ ! -s "$launchlog" ] || fail "a malformed record must not launch a worker"
  [ ! -s "$case_dir/endpoint.log" ] || fail "a malformed record must not create an endpoint: $(cat "$case_dir/endpoint.log")"
  [ ! -e "$home/state/$id.meta" ] || fail "a malformed record must not write a task record"
  pass "fm-spawn.sh: a malformed assignment profile refuses before any endpoint, task record, or launch"
}

# A relaunch onto a tmux endpoint that reads missing always refuses: tmux cannot
# prove absence from a task record (bin/fm-control-lib.sh, fm_control_endpoint_absence_verdict),
# and the fixture here has no live endpoint to relaunch onto. So this test asserts
# the order the contract requires: the persisted record restores first, and only
# then does the endpoint proof stop the relaunch. A successful relaunch is covered
# by fm-control-relaunch.test.sh, which owns the live-endpoint fixture.
test_relaunch_checks_the_persisted_record_before_the_endpoint() {
  local case_dir home proj wt fakebin launchlog out rc id=T-RELAUNCH base
  case_dir=$TMP_ROOT/spawn-relaunch
  home="$case_dir/home"
  proj="$case_dir/project"
  wt="$case_dir/wt"
  launchlog="$case_dir/launch.log"
  fakebin=$(spawn_world "$case_dir")
  fm_test_spawn_home "$home" codex
  fm_git_worktree "$proj" "$wt" "wt-spawn-relaunch"
  base=$(git -C "$wt" rev-parse HEAD)
  fm_test_spawn_brief "$home" "$id"
  fm_test_assignment_profile_section "$id" "$base" >>"$home/data/$id/brief.md"
  fm_test_managed_block "$id" no-mistakes >>"$home/data/$id/brief.md"
  : >"$launchlog"
  out=$(FM_TEST_NO_ASSIGNMENT_PROFILE=1 fm_test_run_spawn "$home" "$wt" "$fakebin" "$id" "$proj" --mode no-mistakes --yolo off --branch-prefix fm/); rc=$?
  expect_code 0 "$rc" "the first launch must succeed ($out)"
  : >"$launchlog"
  out=$(FM_TEST_NO_ASSIGNMENT_PROFILE=1 fm_test_run_spawn "$home" "$wt" "$fakebin" "$id" --relaunch --harness codex); rc=$?
  expect_code 1 "$rc" "a relaunch onto an unprovable tmux endpoint must stop ($out)"
  assert_not_contains "$out" "assignment profile" "an intact persisted record must pass the record check before the endpoint proof"
  assert_contains "$out" "cannot be proven" "the relaunch must stop at the endpoint proof, after the record check"
  [ ! -s "$launchlog" ] || fail "a relaunch that stops at the endpoint proof must not launch a worker"
  pass "fm-spawn.sh: a relaunch checks the persisted record before the endpoint proof"
}

# Rewrite the tests obligation in one copy of the brief at <brief>, so that copy
# disagrees with the record it renders from. <mode> picks the copy: section is the
# first copy in a brief and dod the last.
tamper_obligation_copy() {  # <brief> <section|dod>
  python3 - "$1" "$2" <<'PY'
import sys
path, mode = sys.argv[1], sys.argv[2]
text = open(path, encoding="utf-8").read()
old = "- Tests: required by this profile; run the tests that cover your change."
new = ("- Tests: this profile requires zero tests executed; run only the applicable non-test checks, "
       "and do not run or add tests to satisfy it.")
at = text.find(old) if mode == "section" else text.rfind(old)
assert at >= 0, "the tests obligation is not in the brief"
open(path, "w", encoding="utf-8").write(text[:at] + new + text[at + len(old):])
PY
}

test_relaunch_refuses_a_brief_whose_obligation_copy_disagrees_before_the_endpoint() {
  local case_dir mode home proj wt fakebin launchlog out rc base want id=T-RELAUNCH-AGREE
  for mode in section dod; do
    case_dir=$TMP_ROOT/spawn-relaunch-agree-$mode
    home="$case_dir/home"
    proj="$case_dir/project"
    wt="$case_dir/wt"
    launchlog="$case_dir/launch.log"
    fakebin=$(spawn_world "$case_dir")
    fm_test_spawn_home "$home" codex
    fm_git_worktree "$proj" "$wt" "wt-relaunch-agree-$mode"
    base=$(git -C "$wt" rev-parse HEAD)
    fm_test_spawn_brief "$home" "$id"
    fm_test_assignment_profile_section "$id" "$base" >>"$home/data/$id/brief.md"
    fm_test_managed_block "$id" no-mistakes >>"$home/data/$id/brief.md"
    : >"$launchlog"
    out=$(FM_TEST_NO_ASSIGNMENT_PROFILE=1 fm_test_run_spawn "$home" "$wt" "$fakebin" "$id" "$proj" --mode no-mistakes --yolo off --branch-prefix fm/); rc=$?
    expect_code 0 "$rc" "the first launch must succeed ($out)"
    # The persisted record stays intact; only one obligation copy in the brief changes.
    tamper_obligation_copy "$home/data/$id/brief.md" "$mode"
    : >"$launchlog"
    out=$(FM_TEST_NO_ASSIGNMENT_PROFILE=1 fm_test_run_spawn "$home" "$wt" "$fakebin" "$id" --relaunch --harness codex); rc=$?
    if [ "$mode" = section ]; then want="Assignment profile section"; else want="definition-of-done copy"; fi
    expect_code 1 "$rc" "a relaunch whose brief $mode copy disagrees with the restored record must refuse ($out)"
    assert_contains "$out" "$want does not state the obligations its decision record lists" \
      "the relaunch refusal must name the copy that disagrees"
    assert_not_contains "$out" "cannot be proven" "the brief check must refuse before the endpoint proof"
    [ ! -s "$launchlog" ] || fail "a relaunch whose brief disagrees with its record must not launch a worker"
  done
  pass "fm-spawn.sh: a relaunch refuses a brief whose section or definition-of-done copy disagrees with the restored record before the endpoint"
}

test_relaunch_refuses_a_tampered_persisted_record() {
  local case_dir home proj wt fakebin launchlog out rc id=T-TAMPER base
  case_dir=$TMP_ROOT/spawn-tamper
  home="$case_dir/home"
  proj="$case_dir/project"
  wt="$case_dir/wt"
  launchlog="$case_dir/launch.log"
  fakebin=$(spawn_world "$case_dir")
  fm_test_spawn_home "$home" codex
  fm_git_worktree "$proj" "$wt" "wt-spawn-tamper"
  base=$(git -C "$wt" rev-parse HEAD)
  fm_test_spawn_brief "$home" "$id"
  fm_test_assignment_profile_section "$id" "$base" >>"$home/data/$id/brief.md"
  fm_test_managed_block "$id" no-mistakes >>"$home/data/$id/brief.md"
  : >"$launchlog"
  out=$(FM_TEST_NO_ASSIGNMENT_PROFILE=1 fm_test_run_spawn "$home" "$wt" "$fakebin" "$id" "$proj" --mode no-mistakes --yolo off --branch-prefix fm/); rc=$?
  expect_code 0 "$rc" "the first launch must succeed ($out)"
  printf '\n' >>"$home/data/$id/assignment-profile.json"
  : >"$launchlog"
  out=$(FM_TEST_NO_ASSIGNMENT_PROFILE=1 fm_test_run_spawn "$home" "$wt" "$fakebin" "$id" --relaunch --harness codex); rc=$?
  expect_code 1 "$rc" "a relaunch over a changed persisted record must refuse ($out)"
  assert_contains "$out" "no longer matches its recorded hash" "the refusal must name the hash mismatch"
  [ ! -s "$launchlog" ] || fail "a refused relaunch must not launch a worker"
  pass "fm-spawn.sh: a relaunch over a changed persisted record refuses before launch"
}

# Remove the definition-of-done copy of the record at <record> from <brief>, leaving the
# Assignment profile section: the shape a brief has when a promotion rewrites it, or when
# it was launched before that copy existed.
remove_dod_copy() {  # <brief> <record> <task>
  local copy
  copy=$(. "$ROOT/bin/fm-assignment-profile-lib.sh" && fm_assignment_profile_dod_copy "$2" fixture "$3") || return 1
  python3 - "$1" "$copy" <<'PY'
import sys
path, copy = sys.argv[1], sys.argv[2]
text = open(path, encoding="utf-8").read()
assert text.count(copy) == 1, "the definition-of-done copy must appear exactly once in the brief"
open(path, "w", encoding="utf-8").write(text.replace(copy, "", 1))
PY
}

# A brief promoted, or launched before the definition-of-done copy existed, carries the
# section copy alone. Its relaunch must pass the brief check and reach the endpoint proof.
test_relaunch_accepts_a_section_only_brief_from_before_the_definition_of_done_copy() {
  local case_dir home proj wt fakebin launchlog out rc base id=T-RELAUNCH-OLD
  case_dir=$TMP_ROOT/spawn-relaunch-old
  home="$case_dir/home"
  proj="$case_dir/project"
  wt="$case_dir/wt"
  launchlog="$case_dir/launch.log"
  fakebin=$(spawn_world "$case_dir")
  fm_test_spawn_home "$home" codex
  fm_git_worktree "$proj" "$wt" "wt-spawn-relaunch-old"
  base=$(git -C "$wt" rev-parse HEAD)
  fm_test_spawn_brief "$home" "$id"
  fm_test_assignment_profile_section "$id" "$base" >>"$home/data/$id/brief.md"
  fm_test_managed_block "$id" no-mistakes >>"$home/data/$id/brief.md"
  : >"$launchlog"
  out=$(FM_TEST_NO_ASSIGNMENT_PROFILE=1 fm_test_run_spawn "$home" "$wt" "$fakebin" "$id" "$proj" --mode no-mistakes --yolo off --branch-prefix fm/); rc=$?
  expect_code 0 "$rc" "the first launch must succeed ($out)"
  remove_dod_copy "$home/data/$id/brief.md" "$home/data/$id/assignment-profile.json" "$id" \
    || fail "could not remove the definition-of-done copy from the launched brief"
  : >"$launchlog"
  out=$(FM_TEST_NO_ASSIGNMENT_PROFILE=1 fm_test_run_spawn "$home" "$wt" "$fakebin" "$id" --relaunch --harness codex); rc=$?
  assert_not_contains "$out" "does not state the obligations" \
    "a section-only brief from before the definition-of-done copy must pass the brief check on relaunch ($out)"
  assert_contains "$out" "cannot be proven" "the relaunch must reach the endpoint proof after the brief check"
  expect_code 1 "$rc" "the relaunch still stops at the endpoint proof ($out)"
  [ ! -s "$launchlog" ] || fail "a relaunch that stops at the endpoint proof must not launch a worker"
  pass "fm-spawn.sh: a relaunch accepts a section-only brief from before the definition-of-done copy and reaches the endpoint proof"
}

# A fresh launch still needs both copies, so the older-shape relaxation never reaches a new brief.
test_fresh_launch_still_refuses_a_section_only_brief() {
  local case_dir home proj wt fakebin launchlog out rc base id=T-FRESH-SECTION-ONLY
  case_dir=$TMP_ROOT/spawn-fresh-section-only
  home="$case_dir/home"
  proj="$case_dir/project"
  wt="$case_dir/wt"
  launchlog="$case_dir/launch.log"
  fakebin=$(spawn_world "$case_dir")
  fm_test_spawn_home "$home" codex
  fm_git_worktree "$proj" "$wt" "wt-spawn-fresh-section-only"
  base=$(git -C "$wt" rev-parse HEAD)
  fm_test_spawn_brief "$home" "$id"
  fm_test_assignment_profile_record "$id" "$base" >"$case_dir/record.json"
  fm_test_assignment_profile_section "$id" "$base" >>"$home/data/$id/brief.md"
  remove_dod_copy "$home/data/$id/brief.md" "$case_dir/record.json" "$id" || fail "could not remove the definition-of-done copy"
  fm_test_managed_block "$id" no-mistakes >>"$home/data/$id/brief.md"
  : >"$launchlog"
  out=$(FM_TEST_NO_ASSIGNMENT_PROFILE=1 fm_test_run_spawn "$home" "$wt" "$fakebin" "$id" "$proj" --mode no-mistakes --yolo off --branch-prefix fm/); rc=$?
  expect_code 1 "$rc" "a fresh launch of a section-only brief must refuse ($out)"
  assert_contains "$out" "definition-of-done copy does not state the obligations its decision record lists" \
    "the fresh-launch refusal must name the missing definition-of-done copy"
  [ ! -s "$launchlog" ] || fail "a fresh launch of a section-only brief must not launch a worker"
  [ ! -s "$case_dir/endpoint.log" ] || fail "a fresh launch of a section-only brief must not create an endpoint"
  [ ! -e "$home/state/$id.meta" ] || fail "a fresh launch of a section-only brief must not write a task record"
  pass "fm-spawn.sh: a fresh launch still refuses a section-only brief before any endpoint, task record, or launch"
}

# Only text outside fenced blocks states obligations. A four-backtick example that repeats
# both correct copies must not satisfy the check for the copy the brief itself gets wrong.
# The example's inner fences are shorter than its own, so they must not end it early.
test_fenced_example_cannot_change_the_obligations_at_launch() {
  local case_dir mode home proj wt fakebin launchlog out rc base want example id=T-FENCED-EXAMPLE
  for mode in section dod; do
    case_dir=$TMP_ROOT/spawn-fenced-example-$mode
    home="$case_dir/home"
    proj="$case_dir/project"
    wt="$case_dir/wt"
    launchlog="$case_dir/launch.log"
    fakebin=$(spawn_world "$case_dir")
    fm_test_spawn_home "$home" codex
    fm_git_worktree "$proj" "$wt" "wt-spawn-fenced-example-$mode"
    base=$(git -C "$wt" rev-parse HEAD)
    fm_test_spawn_brief "$home" "$id"
    fm_test_assignment_profile_record "$id" "$base" >"$case_dir/record.json"
    fm_test_assignment_profile_section "$id" "$base" >>"$home/data/$id/brief.md"
    fm_test_managed_block "$id" no-mistakes >>"$home/data/$id/brief.md"
    # The real copy is wrong; the example repeats the correct lead-ins and obligations
    # without the record fence, which would otherwise be a second decision block.
    tamper_obligation_copy "$home/data/$id/brief.md" "$mode"
    example=$(
      . "$ROOT/bin/fm-assignment-profile-lib.sh" || exit 1
      obligations=$(fm_assignment_profile_obligations "$case_dir/record.json" fixture "$id") || exit 1
      printf '%s\n\n%s\n\n%s\n\n%s\n' "$FM_ASSIGNMENT_PROFILE_SECTION_LEAD" "$obligations" \
        "$FM_ASSIGNMENT_PROFILE_DOD_LEAD" "$obligations"
    ) || fail "could not render the example copies"
    # shellcheck disable=SC2016 # the backticks are literal Markdown fence text, so single quotes are intended
    printf '````markdown\n%s\n````\n' "$example" >>"$home/data/$id/brief.md"
    : >"$launchlog"
    out=$(FM_TEST_NO_ASSIGNMENT_PROFILE=1 fm_test_run_spawn "$home" "$wt" "$fakebin" "$id" "$proj" --mode no-mistakes --yolo off --branch-prefix fm/); rc=$?
    if [ "$mode" = section ]; then want="Assignment profile section"; else want="definition-of-done copy"; fi
    expect_code 1 "$rc" "a fenced example must not satisfy the $mode copy check ($out)"
    assert_contains "$out" "$want does not state the obligations its decision record lists" \
      "the refusal must name the real $mode copy, not the example"
    [ ! -s "$launchlog" ] || fail "a brief whose real $mode copy is wrong must not launch a worker"
    [ ! -s "$case_dir/endpoint.log" ] || fail "a brief whose real $mode copy is wrong must not create an endpoint"
    [ ! -e "$home/state/$id.meta" ] || fail "a brief whose real $mode copy is wrong must not write a task record"
  done
  pass "fm-spawn.sh: a fenced example that repeats correct obligations cannot change the obligation check"
}

# A line that starts with an inline code span is prose: a backtick fence's info string may
# not hold a backtick. The obligations after it must still count, not be hidden.
test_inline_code_span_at_line_start_is_prose_not_a_fence() {
  local dir=$TMP_ROOT/inline-span-prose id=T-INLINE-SPAN record out rc
  mkdir -p "$dir"
  fm_test_assignment_profile_record "$id" "$GOOD_BASE" >"$dir/record.json"
  record=$(cat "$dir/record.json")
  {
    printf "\`\`\`x\`\`\` is an inline code span, not a fence\n\n"
    fm_test_assignment_profile_section "$id" "$GOOD_BASE"
  } >"$dir/brief.md"
  out=$(bash -c '. "$1"; fm_assignment_profile_brief_check "$2" "$3" "$4" fixture 1' _ \
    "$ROOT/bin/fm-assignment-profile-lib.sh" "$dir/brief.md" "$record" "$id" 2>&1); rc=$?
  expect_code 0 "$rc" "an inline code span at a line start must not hide the obligations after it ($out)"
  pass "fm-assignment-profile-lib.sh: an inline code span at a line start is prose, so the brief's obligations still count"
}

# The same misreading must not expose a fenced example: a bare fence opened by the example
# would close the bogus fence and make the example's text count as the brief's own.
test_inline_code_span_cannot_expose_a_fenced_example_at_launch() {
  local dir=$TMP_ROOT/inline-span-example id=T-INLINE-EXAMPLE example record out rc
  mkdir -p "$dir"
  fm_test_assignment_profile_record "$id" "$GOOD_BASE" >"$dir/record.json"
  record=$(cat "$dir/record.json")
  fm_test_assignment_profile_section "$id" "$GOOD_BASE" >"$dir/brief.md"
  tamper_obligation_copy "$dir/brief.md" section
  example=$(
    . "$ROOT/bin/fm-assignment-profile-lib.sh" || exit 1
    obligations=$(fm_assignment_profile_obligations "$dir/record.json" fixture "$id") || exit 1
    printf '%s\n\n%s\n' "$FM_ASSIGNMENT_PROFILE_SECTION_LEAD" "$obligations"
  ) || fail "could not render the example copy"
  printf "\`\`\`x\`\`\` example note\n\`\`\`\n%s\`\`\`\n" "$example" >>"$dir/brief.md"
  out=$(bash -c '. "$1"; fm_assignment_profile_brief_check "$2" "$3" "$4" fixture 1' _ \
    "$ROOT/bin/fm-assignment-profile-lib.sh" "$dir/brief.md" "$record" "$id" 2>&1); rc=$?
  expect_code 1 "$rc" "a fenced example opened after an inline code span must not satisfy the section check ($out)"
  assert_contains "$out" "Assignment profile section does not state the obligations" \
    "the refusal must name the real section, not the example"
  pass "fm-assignment-profile-lib.sh: an inline code span cannot make a fenced example count as the brief's obligations"
}

test_valid_record_is_accepted_and_canonical
test_canonical_form_is_stable_across_key_order
test_unknown_profile_is_refused
test_unsupported_contract_version_is_refused
test_final_record_with_unresolved_classification_is_refused
test_missing_comparison_base_is_refused
test_final_record_without_change_identity_is_refused
test_risk_triggers_on_non_high_risk_profile_are_refused
test_unresolved_on_non_provisional_high_risk_is_refused
test_unknown_top_level_key_is_refused
test_documentation_only_with_tests_is_refused
test_documentation_only_profile_is_accepted
test_small_low_risk_profile_is_accepted
test_incomplete_change_coverage_is_refused_for_final
test_duplicate_key_is_refused
test_nan_constant_is_refused
test_unreadable_record_cannot_run_is_exit_two
test_task_id_mismatch_is_refused
test_target_branch_pin_mismatch_is_refused
test_base_commit_pin_mismatch_is_refused
test_unapproved_revision_is_refused
test_require_current_refuses_a_non_current_revision
test_policy_material_change_breaks_the_pinned_revision
test_brief_without_a_block_is_refused_by_the_library
test_brief_with_two_blocks_is_refused_by_the_library
test_section_renders_obligations_and_the_carried_block
test_persisted_record_restores_only_when_unchanged
test_relaunch_refuses_a_brief_with_a_different_record
test_missing_persisted_record_blocks_relaunch
test_brief_refuses_a_record_for_another_task
test_brief_refuses_the_flag_on_a_scout
test_ship_spawn_without_a_record_refuses_before_any_launch
test_ship_spawn_with_a_record_pinned_to_another_base_refuses
test_ship_spawn_with_another_target_refuses_before_any_endpoint
test_ship_spawn_with_the_pinned_record_persists_and_records_the_decision
test_each_profile_brief_agrees_with_its_launch
test_ship_spawn_with_a_brief_that_disagrees_with_its_record_refuses_before_any_endpoint
test_ship_spawn_with_a_malformed_record_refuses_before_any_endpoint
test_relaunch_checks_the_persisted_record_before_the_endpoint
test_relaunch_refuses_a_brief_whose_obligation_copy_disagrees_before_the_endpoint
test_relaunch_refuses_a_tampered_persisted_record
test_relaunch_accepts_a_section_only_brief_from_before_the_definition_of_done_copy
test_fresh_launch_still_refuses_a_section_only_brief
test_fenced_example_cannot_change_the_obligations_at_launch
test_inline_code_span_at_line_start_is_prose_not_a_fence
test_inline_code_span_cannot_expose_a_fenced_example_at_launch

echo "# all fm-assignment-profile tests passed"
