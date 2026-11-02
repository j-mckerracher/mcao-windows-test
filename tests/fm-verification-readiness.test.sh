#!/usr/bin/env bash
# Behavior tests for the verification readiness gate (bin/fm-verification-readiness-lib.sh,
# docs/configuration.md "Verification readiness") on the ship and scout launch path of
# bin/fm-spawn.sh: the run classification, the declaration grammar, the assignment-profile
# decision, the check verdicts, the refusal shape, and the record each gated run leaves.
#
# Every refusal here must stop before a launch, a task record, or a composed launch
# brief exists, so the assertions check those three artifacts, not only the exit.
# Suites elsewhere mark their spawns --attended through tests/fixtures.sh; this suite
# sets FM_TEST_UNATTENDED=1 so each spawn is classified by its own arguments and posture.
set -u

# shellcheck source=tests/fixtures.sh
. "$(dirname "${BASH_SOURCE[0]}")/fixtures.sh"

TMP_ROOT=$(fm_test_tmproot fm-verification-readiness)
export FM_TEST_UNATTENDED=1

# new_case <name>: a throwaway home on the claude harness, one project clone, and the
# worktree the fake pane points at. Sets the case variables the helpers below read.
new_case() {
  CASE_DIR="$TMP_ROOT/$1"
  HOME_DIR="$CASE_DIR/home"
  PROJ_DIR="$CASE_DIR/project"
  WT_DIR="$CASE_DIR/wt"
  LAUNCH_LOG="$CASE_DIR/launch.log"
  FAKEBIN_DIR=$(fm_test_make_spawn_fakebin "$CASE_DIR/fake")
  cat > "$FAKEBIN_DIR/timeout" <<'SH'
#!/usr/bin/env bash
shift
exec "$@"
SH
  chmod +x "$FAKEBIN_DIR/timeout"
  fm_test_spawn_home "$HOME_DIR" claude
  fm_git_worktree "$PROJ_DIR" "$WT_DIR" "wt-$1"
}

# run_case <id> [fm-spawn args...]: one spawn through the suite's common env.
run_case() {
  local id=$1
  shift
  : > "$LAUNCH_LOG"
  FM_FAKE_LAUNCH_LOG="$LAUNCH_LOG" \
    fm_test_run_spawn "$HOME_DIR" "$WT_DIR" "$FAKEBIN_DIR" "$id" "$@"
}

# gate_record <id> <jq-filter>: one field of the readiness record the gate wrote.
gate_record() {
  jq -r "$2" "$HOME_DIR/data/$1/verification-readiness.json"
}

# install_driver <project>: an existing project driver that writes CLI output under the
# evidence directory the gate hands it, so the pass path needs no package or UI capture.
install_driver() {
  mkdir -p "$1/scripts"
  cat > "$1/scripts/verify.sh" <<'SH'
#!/usr/bin/env bash
set -eu
printf 'cli report: ok\n' > "$FM_VERIFY_EVIDENCE_DIR/cli-report.txt"
SH
}

# declare_check <project> <check-id> <command> <evidence> [env-name] [fixture-path]:
# writes a one-check hve-verification.yml in the documented key order.
declare_check() {
  {
    printf '%s\n' 'contract_version: 1' 'checks:' \
      "  - id: $2" "    command: $3" "    evidence: $4"
    if [ -n "${5:-}" ]; then
      printf '%s\n' 'environment:' "  - $5"
    else
      printf '%s\n' 'environment: []'
    fi
    if [ -n "${6:-}" ]; then
      printf '%s\n' 'fixtures:' "  - $6"
    else
      printf '%s\n' 'fixtures: []'
    fi
    printf '%s\n' 'cleanup: none'
  } > "$1/hve-verification.yml"
}

# standard_project <project>: the passing declaration the happy-path cases use.
standard_project() {
  install_driver "$1"
  declare_check "$1" cli-smoke 'bash scripts/verify.sh' cli-report.txt
}

# decision_json <task-id> <tests-required true|false> <classification resolved|unresolved>
# One assignment-profile record that satisfies docs/assignment-profile-decision.v1.schema.json.
decision_json() {
  printf '%s\n' '{' \
    '"contract_version": 1,' \
    "\"decision_id\": \"$1/1\"," \
    "\"task_id\": \"$1\"," \
    '"assignment_intent": "Verification readiness fixture.",' \
    '"expected_surfaces": ["cli"],' \
    '"pinned_policy_revision": "fixture",' \
    '"profile": "standard",' \
    '"decision_stage": "final",' \
    "\"classification_status\": \"$3\"," \
    '"risk_triggers": [],' \
    '"change_coverage": {"complete": true, "categories": ["committed"]},' \
    '"reasons": ["fixture reason"],' \
    '"required_stages": ["implement", "verify"],' \
    "\"evidence_obligations\": {\"tests_required\": $2, \"independent_change_review_required\": false}," \
    '"target_branch": "main",' \
    '"comparison_base_commit": "0000000000000000000000000000000000000001",' \
    '"evaluated_change_identity": "fixture"' \
    '}'
}

# decision_text <kind> <task-id>: the assignment-profile text a brief carries for <kind>.
#   required | not-required | unresolved | duplicate | malformed | none
decision_text() {
  case "$1" in
  required)
    printf '%s\n' '```assignment-profile' "$(decision_json "$2" true resolved)" '```'
    ;;
  not-required)
    printf '%s\n' '```assignment-profile' "$(decision_json "$2" false resolved)" '```'
    ;;
  unresolved)
    printf '%s\n' '```assignment-profile' "$(decision_json "$2" false unresolved)" '```'
    ;;
  duplicate)
    printf '%s\n' '```assignment-profile' "$(decision_json "$2" true resolved)" '```' \
      '```assignment-profile' "$(decision_json "$2" true resolved)" '```'
    ;;
  malformed)
    printf '%s\n' '```assignment-profile' '{"contract_version": 1' '```'
    ;;
  none) : ;;
  esac
}

# write_ship_brief <id> <mode> <decision-kind>: a ship brief with the managed-context
# block a ship launch requires, plus the decision text for <decision-kind>.
write_ship_brief() {
  local id=$1 mode=$2 kind=$3
  fm_test_spawn_brief "$HOME_DIR" "$id" "Verification readiness fixture for $id."
  # A ship brief records its delivery contract and ship branch; the spawn refuses a
  # brief without a recorded branch. The branch is the default prefix plus the id.
  {
    fm_test_managed_block "$id" "$mode"
    printf 'Delivery contract: mode=%s\nShip branch: moa/%s\n' "$mode" "$id"
    decision_text "$kind" "$id"
  } >>"$HOME_DIR/data/$id/brief.md"
}

# write_scout_brief <id> <decision-kind>: a scout brief, which carries no managed block.
write_scout_brief() {
  local id=$1 kind=$2
  fm_test_spawn_brief "$HOME_DIR" "$id" "Verification readiness scout fixture for $id."
  decision_text "$kind" "$id" >>"$HOME_DIR/data/$id/brief.md"
}

# expect_gate_refusal <label> <expected-substring> <id> [spawn args...]: the spawn must
# exit 1, name the expected capability or reason, leave no launch, no task record, and
# no composed launch brief, and leave a refused readiness record for the run.
expect_gate_refusal() {
  local label=$1 expected=$2 id=$3 out status
  shift 3
  out=$(run_case "$id" "$PROJ_DIR" "$@")
  status=$?
  expect_code 1 "$status" "$label should refuse"$'\n'"$out"
  assert_contains "$out" "$expected" "$label refusal did not name '$expected'"$'\n'"$out"
  [ ! -s "$LAUNCH_LOG" ] || fail "$label launched despite the refusal"
  [ ! -e "$HOME_DIR/state/$id.meta" ] || fail "$label published a task record despite the refusal"
  [ ! -e "$HOME_DIR/data/$id/launch-brief.md" ] || fail "$label composed a launch brief despite the refusal"
  assert_equals refused "$(gate_record "$id" .outcome)" "$label should leave a refused readiness record"
}

# --- classification --------------------------------------------------------

test_firstmate_run_without_attended_is_unattended_and_passes_the_gate() {
  local id=vr-gap out status
  new_case gap
  standard_project "$PROJ_DIR"
  write_ship_brief "$id" direct-PR required
  out=$(run_case "$id" "$PROJ_DIR" --mode direct-PR --yolo off)
  status=$?
  expect_code 0 "$status" "a firstmate-initiated run with a passing declaration should launch"$'\n'"$out"
  [ -s "$LAUNCH_LOG" ] || fail "the gap-case run did not launch"
  assert_equals unattended "$(gate_record "$id" .run_class)" "a run without --attended is unattended"
  assert_equals admitted "$(gate_record "$id" .outcome)" "the gap-case run should be admitted"
  assert_equals cli-smoke "$(gate_record "$id" '.checks[0].id')" "the record should name the declared check"
  assert_equals pass "$(gate_record "$id" '.checks[0].status')" "the record should keep the observed pass"
  pass "a firstmate-initiated run with no attended flag is classified unattended and passes the gate before launch"
}

test_attended_run_launches_without_a_declaration() {
  local id=vr-attended out status
  new_case attended
  write_ship_brief "$id" direct-PR none
  out=$(run_case "$id" "$PROJ_DIR" --attended --mode direct-PR --yolo off)
  status=$?
  expect_code 0 "$status" "an attended run should launch without a declaration"$'\n'"$out"
  [ -s "$LAUNCH_LOG" ] || fail "the attended run did not launch"
  assert_equals attended "$(gate_record "$id" .run_class)" "an explicit request while present is attended"
  assert_equals exempt "$(gate_record "$id" .outcome)" "an attended run records that the gate did not apply"
  pass "an attended run with no declaration launches and records its exemption"
}

test_away_posture_makes_an_explicit_request_unattended() {
  local id=vr-away
  new_case away
  write_ship_brief "$id" direct-PR required
  : > "$HOME_DIR/state/.afk"
  expect_gate_refusal "an attended request during an away posture" \
    "no hve-verification.yml declaration at the project root" "$id" \
    --attended --mode direct-PR --yolo off
  assert_equals unattended "$(gate_record "$id" .run_class)" "an away posture overrides --attended"
  pass "an away posture reclassifies an explicit request as unattended and refuses it without a declaration"
}

test_fan_out_pair_is_gated_and_passes_even_when_each_worker_would_be_attended() {
  local out status
  new_case fanout
  standard_project "$PROJ_DIR"
  write_ship_brief vr-fan-a direct-PR required
  write_ship_brief vr-fan-b direct-PR required
  : > "$LAUNCH_LOG"
  out=$(FM_FAKE_LAUNCH_LOG="$LAUNCH_LOG" \
    fm_test_run_spawn "$HOME_DIR" "$WT_DIR" "$FAKEBIN_DIR" \
    "vr-fan-a=$PROJ_DIR" "vr-fan-b=$PROJ_DIR" --mode direct-PR --yolo off)
  status=$?
  expect_code 0 "$status" "a two-worker dispatch with a passing declaration should launch both"$'\n'"$out"
  for id in vr-fan-a vr-fan-b; do
    assert_equals fanout "$(gate_record "$id" .run_class)" "$id is part of a two-worker dispatch"
    assert_equals admitted "$(gate_record "$id" .outcome)" "$id should be admitted"
    [ -e "$HOME_DIR/state/$id.meta" ] || fail "$id did not publish its task record after admission"
  done
  pass "a two-worker dispatch is classified fan-out, gated, and admitted for each worker"
}

test_fan_out_stays_gated_under_attended_and_names_the_capability() {
  local out status id
  new_case fanout-refuse
  write_ship_brief vr-fan-c direct-PR required
  write_ship_brief vr-fan-d direct-PR required
  : > "$LAUNCH_LOG"
  out=$(FM_FAKE_LAUNCH_LOG="$LAUNCH_LOG" \
    fm_test_run_spawn "$HOME_DIR" "$WT_DIR" "$FAKEBIN_DIR" \
    "vr-fan-c=$PROJ_DIR" "vr-fan-d=$PROJ_DIR" --attended --mode direct-PR --yolo off)
  status=$?
  [ "$status" -ne 0 ] || fail "a fan-out dispatch with no declaration should refuse even when attended"
  assert_contains "$out" "not verification-ready for this fanout run" "fan-out refusal did not name the run class"$'\n'"$out"
  assert_contains "$out" "capability: verification declaration hve-verification.yml" "fan-out refusal did not name the capability"$'\n'"$out"
  [ ! -s "$LAUNCH_LOG" ] || fail "a refused fan-out dispatch launched a worker"
  for id in vr-fan-c vr-fan-d; do
    [ ! -e "$HOME_DIR/state/$id.meta" ] || fail "$id published a task record despite the fan-out refusal"
    [ ! -e "$HOME_DIR/data/$id/launch-brief.md" ] || fail "$id composed a launch brief despite the fan-out refusal"
    assert_equals refused "$(gate_record "$id" .outcome)" "$id should leave a refused readiness record"
    assert_equals fanout "$(gate_record "$id" .run_class)" "$id should record the fan-out class"
  done
  pass "a fan-out dispatch stays gated under --attended and names the missing capability"
}

test_relaunch_of_a_run_in_flight_is_not_gated() {
  local id=vr-inflight out status
  new_case inflight
  write_ship_brief "$id" direct-PR none
  out=$(run_case "$id" "$PROJ_DIR" --attended --mode direct-PR --yolo off)
  status=$?
  expect_code 0 "$status" "the run that is in flight should launch as attended"$'\n'"$out"
  # The relaunch carries no --attended and the project has no declaration, so a
  # gated run would be refused here by name. Its output must not name the gate.
  out=$(run_case "$id" --relaunch)
  case "$out" in
  *verification-ready* | *hve-verification.yml*) fail "a relaunch of a run in flight was named by the readiness gate"$'\n'"$out" ;;
  esac
  assert_equals attended "$(gate_record "$id" .run_class)" "a relaunch must not reclassify the run in flight"
  assert_equals exempt "$(gate_record "$id" .outcome)" "a relaunch must not rewrite the run's own readiness record"
  pass "a relaunch of a run in flight is not gated and leaves the run's own record in place"
}

test_scout_run_without_attended_is_gated() {
  local id=vr-scout
  new_case scout
  write_scout_brief "$id" required
  expect_gate_refusal "an unattended scout with no declaration" \
    "no hve-verification.yml declaration at the project root" "$id" --scout
  assert_equals unattended "$(gate_record "$id" .run_class)" "an unattended scout is gated"
  pass "an unattended scout is gated and refused without a declaration"
}

test_attended_is_refused_for_relaunch_and_secondmate() {
  local out status
  new_case flags
  out=$(run_case vr-flag-relaunch --relaunch --attended)
  status=$?
  expect_code 1 "$status" "--attended with --relaunch should be refused"$'\n'"$out"
  assert_contains "$out" "does not apply to --relaunch or --secondmate" "--attended --relaunch refusal did not name the rule"
  out=$(run_case vr-flag-secondmate --secondmate --attended)
  status=$?
  expect_code 1 "$status" "--attended with --secondmate should be refused"$'\n'"$out"
  assert_contains "$out" "does not apply to --relaunch or --secondmate" "--attended --secondmate refusal did not name the rule"
  pass "--attended is refused for a relaunch and for a secondmate"
}

# --- decision record -------------------------------------------------------

test_missing_decision_record_refuses() {
  new_case nodecision
  standard_project "$PROJ_DIR"
  write_ship_brief vr-nodec direct-PR none
  expect_gate_refusal "an unattended run with no decision record" \
    "carries 0 assignment-profile blocks" vr-nodec --mode direct-PR --yolo off
  pass "a run whose brief carries no assignment-profile decision refuses and names the count"
}

test_duplicate_decision_record_refuses() {
  new_case dupdecision
  standard_project "$PROJ_DIR"
  write_ship_brief vr-dupdec direct-PR duplicate
  expect_gate_refusal "an unattended run with two decision records" \
    "carries 2 assignment-profile blocks" vr-dupdec --mode direct-PR --yolo off
  pass "a brief with two assignment-profile blocks refuses and names the count"
}

test_malformed_decision_record_refuses() {
  new_case baddecision
  standard_project "$PROJ_DIR"
  write_ship_brief vr-baddec direct-PR malformed
  expect_gate_refusal "an unattended run with a malformed decision record" \
    "assignment-profile block is not valid JSON" vr-baddec --mode direct-PR --yolo off
  pass "a malformed assignment-profile block refuses rather than granting a skip"
}

test_not_required_decision_skips_the_driver_and_records_no_pass() {
  local id=vr-notreq out status
  new_case notreq
  install_driver "$PROJ_DIR"
  declare_check "$PROJ_DIR" cli-smoke 'exit 1' cli-report.txt
  write_ship_brief "$id" direct-PR not-required
  out=$(run_case "$id" "$PROJ_DIR" --mode direct-PR --yolo off)
  status=$?
  expect_code 0 "$status" "a resolved decision that tests are not required should launch without running the driver"$'\n'"$out"
  [ -s "$LAUNCH_LOG" ] || fail "the not-required run did not launch"
  assert_equals admitted "$(gate_record "$id" .outcome)" "the not-required run should be admitted"
  assert_equals 0 "$(gate_record "$id" '.checks | length')" "no driver ran, so no check is recorded"
  assert_equals false "$(gate_record "$id" .decision.evidence_obligations.tests_required)" \
    "the record should preserve the decision that tests are not required"
  assert_contains "$(gate_record "$id" .reason)" "tests not required" "the record should preserve the decision reason"
  pass "a resolved not-required decision preserves its reason, runs no driver, and records no pass"
}

test_unresolved_decision_still_requires_verification() {
  local id=vr-unresolved out status
  new_case unresolved
  install_driver "$PROJ_DIR"
  declare_check "$PROJ_DIR" cli-smoke 'exit 1' cli-report.txt
  write_ship_brief "$id" direct-PR unresolved
  expect_gate_refusal "an unresolved decision with a failing check" \
    "check cli-smoke failed (exit 1)" "$id" --mode direct-PR --yolo off
  pass "an unresolved decision is never a skip; the required check runs and its failure refuses"
}

# --- declaration and checks ------------------------------------------------

test_passing_existing_driver_launches_in_both_delivery_modes() {
  local mode id out status meta
  for mode in no-mistakes direct-PR; do
    id=vr-pass-$mode
    new_case "pass-$mode"
    standard_project "$PROJ_DIR"
    write_ship_brief "$id" "$mode" required
    out=$(run_case "$id" "$PROJ_DIR" --mode "$mode" --yolo off)
    status=$?
    expect_code 0 "$status" "$mode ship with a passing declaration should launch"$'\n'"$out"
    [ -s "$LAUNCH_LOG" ] || fail "$mode ship with a passing declaration did not launch"
    meta="$HOME_DIR/state/$id.meta"
    assert_grep "managed_delivery_mode=$mode" "$meta" "$mode run did not keep its delivery mode"
    assert_equals admitted "$(gate_record "$id" .outcome)" "$mode run should be admitted"
    assert_equals cli-smoke "$(gate_record "$id" '.checks[0].id')" "$mode run should name the existing driver's check"
    assert_equals pass "$(gate_record "$id" '.checks[0].status')" "$mode run should record the observed pass"
    assert_equals 0 "$(gate_record "$id" '.checks[0].exit_code')" "$mode run should record the exit code"
    assert_equals cli-report.txt "$(basename "$(gate_record "$id" '.checks[0].evidence')")" \
      "$mode run should point at the declared CLI evidence"
    [ -s "$HOME_DIR/data/$id/verification/cli-report.txt" ] || fail "$mode run lost its CLI evidence"
    assert_equals none "$(gate_record "$id" .cleanup)" "$mode run should record the declared cleanup"
  done
  pass "an existing CLI driver passes in both no-mistakes and direct-PR, keeps its evidence, and launches"
}

test_failed_check_stops_at_the_first_non_pass_and_refuses() {
  local id=vr-failfirst out status
  new_case failfirst
  install_driver "$PROJ_DIR"
  cat > "$PROJ_DIR/hve-verification.yml" <<'YAML'
contract_version: 1
checks:
  - id: a-fail
    command: exit 1
    evidence: a.txt
  - id: b-pass
    command: bash scripts/verify.sh
    evidence: cli-report.txt
environment: []
fixtures: []
cleanup: none
YAML
  write_ship_brief "$id" direct-PR required
  expect_gate_refusal "a failing first check" "check a-fail failed (exit 1)" "$id" --mode direct-PR --yolo off
  assert_equals failed "$(gate_record "$id" '.checks[0].status')" "the failing check's status should be kept"
  assert_equals 1 "$(gate_record "$id" '.checks[0].exit_code')" "the failing check's exit code should be kept"
  assert_equals 1 "$(gate_record "$id" '.checks | length')" "a later check must not run after a non-pass"
  [ ! -e "$HOME_DIR/data/$id/verification/cli-report.txt" ] || fail "the check after a failure ran"
  pass "a failing check stops the run at that check, keeps its outcome, and runs no later check"
}

test_unavailable_driver_refuses_naming_the_missing_capability() {
  local code id
  for code in 69 127; do
    id="vr-unavailable-$code"
    new_case "unavailable-$code"
    declare_check "$PROJ_DIR" cli-smoke "exit $code" cli-report.txt
    write_ship_brief "$id" direct-PR required
    expect_gate_refusal "a driver that exits $code" \
      "check cli-smoke could not run (exit $code; its driver or tool is unavailable)" \
      "$id" --mode direct-PR --yolo off
    assert_equals unavailable "$(gate_record "$id" '.checks[0].status')" \
      "exit $code should be recorded as unavailable"
  done
  pass "exit 69 and 127 are recorded as an unavailable capability and refuse by name"
}

test_inconclusive_driver_refuses_with_its_exit() {
  local id=vr-inconclusive
  new_case inconclusive
  declare_check "$PROJ_DIR" cli-smoke 'exit 75' cli-report.txt
  write_ship_brief "$id" direct-PR required
  expect_gate_refusal "an inconclusive driver" "check cli-smoke was inconclusive (exit 75)" "$id" \
    --mode direct-PR --yolo off
  assert_equals inconclusive "$(gate_record "$id" '.checks[0].status')" "exit 75 should be recorded as inconclusive"
  pass "exit 75 is preserved as inconclusive and never becomes a pass"
}

test_zero_exit_without_evidence_fails() {
  local id=vr-noevidence
  new_case noevidence
  declare_check "$PROJ_DIR" cli-smoke 'true' cli-report.txt
  write_ship_brief "$id" direct-PR required
  expect_gate_refusal "a driver that exits 0 without evidence" \
    "check cli-smoke exited 0 without its evidence file cli-report.txt" "$id" --mode direct-PR --yolo off
  assert_equals failed "$(gate_record "$id" '.checks[0].status')" "a missing evidence file is a failure, not a pass"
  pass "exit 0 without the declared evidence file is recorded as failed"
}

test_missing_declared_environment_refuses_and_a_set_value_is_never_recorded() {
  local id=vr-env-set out status
  new_case env
  standard_project "$PROJ_DIR"
  declare_check "$PROJ_DIR" cli-smoke 'bash scripts/verify.sh' cli-report.txt FM_TEST_VERIFY_TOKEN
  write_ship_brief vr-env direct-PR required
  unset FM_TEST_VERIFY_TOKEN
  expect_gate_refusal "an unset declared environment name" \
    "required environment FM_TEST_VERIFY_TOKEN is not set" vr-env --mode direct-PR --yolo off
  write_ship_brief "$id" direct-PR required
  export FM_TEST_VERIFY_TOKEN=s3cr3t-value-782
  out=$(run_case "$id" "$PROJ_DIR" --mode direct-PR --yolo off)
  status=$?
  unset FM_TEST_VERIFY_TOKEN
  expect_code 0 "$status" "a set declared environment name should launch"$'\n'"$out"
  assert_no_grep "s3cr3t-value-782" "$HOME_DIR/data/$id/verification-readiness.json" \
    "the readiness record kept the environment value"
  case "$out" in
  *s3cr3t-value-782*) fail "the spawn output printed the environment value" ;;
  esac
  pass "a declared environment name must be set, and its value is never written to the record"
}

test_missing_declared_fixture_refuses_until_it_is_restored() {
  local id=vr-fixture-ok out status
  new_case fixture
  install_driver "$PROJ_DIR"
  declare_check "$PROJ_DIR" cli-smoke 'bash scripts/verify.sh' cli-report.txt "" fixtures/app.json
  write_ship_brief vr-fixture direct-PR required
  expect_gate_refusal "a missing declared fixture" \
    "required fixture fixtures/app.json is missing" vr-fixture --mode direct-PR --yolo off
  mkdir -p "$PROJ_DIR/fixtures"
  printf '{}\n' > "$PROJ_DIR/fixtures/app.json"
  write_ship_brief "$id" direct-PR required
  out=$(run_case "$id" "$PROJ_DIR" --mode direct-PR --yolo off)
  status=$?
  expect_code 0 "$status" "the restored fixture should let the run launch"$'\n'"$out"
  pass "a missing declared fixture refuses by name and launches once it is restored"
}

test_required_verification_without_declared_checks_refuses() {
  local id=vr-nochecks
  new_case nochecks
  printf '%s\n' 'contract_version: 1' 'checks: []' 'environment: []' 'fixtures: []' 'cleanup: none' \
    > "$PROJ_DIR/hve-verification.yml"
  write_ship_brief "$id" direct-PR required
  expect_gate_refusal "a required verification with no checks" \
    "verification is required but hve-verification.yml declares no checks" "$id" --mode direct-PR --yolo off
  pass "a required verification whose declaration lists no checks refuses rather than passing"
}

test_malformed_declaration_names_the_line() {
  local id=vr-grammar
  new_case grammar
  cat > "$PROJ_DIR/hve-verification.yml" <<'YAML'
contract_version: 1
checks:
  - id: Bad-Id
    command: bash scripts/verify.sh
    evidence: cli-report.txt
environment: []
fixtures: []
cleanup: none
YAML
  write_ship_brief "$id" direct-PR required
  expect_gate_refusal "a declaration that breaks the grammar" \
    "does not match the declaration grammar: line 3: check id 'Bad-Id' must be lowercase letters, digits, and hyphens" \
    "$id" --mode direct-PR --yolo off
  pass "a declaration outside the documented grammar refuses and names the first bad line"
}

# --- admission boundaries --------------------------------------------------

test_dispatch_size_must_be_a_whole_number() {
  local out status
  new_case dispatch-size
  out=$(run_case vr-dispatch --dispatch-size many --mode direct-PR --yolo off)
  status=$?
  expect_code 2 "$status" "a non-numeric dispatch size should be refused"$'\n'"$out"
  assert_contains "$out" "requires a whole number of workers" "dispatch-size refusal did not name the rule"
  pass "a non-numeric dispatch size is refused before any gate runs"
}

test_refusal_names_the_missing_declaration() {
  local id=vr-refusal
  new_case refusal
  write_ship_brief "$id" direct-PR required
  expect_gate_refusal "an unattended run with no declaration" \
    "no hve-verification.yml declaration at the project root" "$id" --mode direct-PR --yolo off
  assert_equals "verification declaration hve-verification.yml" \
    "$(jq -r '.capability' "$HOME_DIR/data/$id/verification-readiness.json")" \
    "the refusal record did not name the capability"
  pass "an unattended run with no declaration refuses and names the missing capability"
}

test_refusal_leaves_the_record_for_a_later_spawn_of_the_same_id() {
  local id=vr-recover out status
  new_case recover
  write_ship_brief "$id" direct-PR required
  expect_gate_refusal "the first unattended attempt" \
    "no hve-verification.yml declaration at the project root" "$id" --mode direct-PR --yolo off
  standard_project "$PROJ_DIR"
  out=$(run_case "$id" "$PROJ_DIR" --mode direct-PR --yolo off)
  status=$?
  expect_code 0 "$status" "a later spawn of the same id should launch once the declaration exists"$'\n'"$out"
  assert_equals admitted "$(gate_record "$id" .outcome)" "the later spawn should overwrite the refused record with its admission"
  pass "a refused record does not block a later spawn of the same id once the project is ready"
}

test_firstmate_run_without_attended_is_unattended_and_passes_the_gate
test_attended_run_launches_without_a_declaration
test_away_posture_makes_an_explicit_request_unattended
test_fan_out_pair_is_gated_and_passes_even_when_each_worker_would_be_attended
test_fan_out_stays_gated_under_attended_and_names_the_capability
test_relaunch_of_a_run_in_flight_is_not_gated
test_scout_run_without_attended_is_gated
test_attended_is_refused_for_relaunch_and_secondmate
test_missing_decision_record_refuses
test_duplicate_decision_record_refuses
test_malformed_decision_record_refuses
test_not_required_decision_skips_the_driver_and_records_no_pass
test_unresolved_decision_still_requires_verification
test_passing_existing_driver_launches_in_both_delivery_modes
test_failed_check_stops_at_the_first_non_pass_and_refuses
test_unavailable_driver_refuses_naming_the_missing_capability
test_inconclusive_driver_refuses_with_its_exit
test_zero_exit_without_evidence_fails
test_missing_declared_environment_refuses_and_a_set_value_is_never_recorded
test_missing_declared_fixture_refuses_until_it_is_restored
test_required_verification_without_declared_checks_refuses
test_malformed_declaration_names_the_line
test_dispatch_size_must_be_a_whole_number
test_refusal_names_the_missing_declaration
test_refusal_leaves_the_record_for_a_later_spawn_of_the_same_id

echo "# all fm-verification-readiness tests passed"
