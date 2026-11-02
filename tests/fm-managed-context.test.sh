#!/usr/bin/env bash
# Behavior tests for the managed-context contract (docs/managed-context.md) on the
# ship launch path: bin/fm-spawn.sh validates the block a ship brief carries before
# any endpoint, worktree, or task record exists, binds the allocated worktree into
# the launch brief, and records the managed fields in state/<id>.meta. The same
# suite covers the supported workflow-agent version range (bin/fm-workflow-agent-lib.sh),
# the promotion gate in bin/fm-promote.sh, and the standalone scout launch, which
# carries no block and stays as it was.
#
# Every refusal here must stop before a launch, a task record, or a composed launch
# brief exists, so the assertions check those three artifacts, not only the exit.
set -u

# shellcheck source=tests/fixtures.sh
. "$(dirname "${BASH_SOURCE[0]}")/fixtures.sh"

PROMOTE="$ROOT/bin/fm-promote.sh"
TMP_ROOT=$(fm_test_tmproot fm-managed-context)
unset FM_TEST_HVE_VERSION

make_pi_probe() {
  local fakebin=$1 tool=$2
  cat > "$fakebin/$tool" <<'SH'
#!/usr/bin/env bash
set -u
if [ "${1:-}" = --help ]; then
  printf '%s\n' 'Pi 0.84.0' 'Options: --help --tui-mode <mode> --approve'
fi
exit 0
SH
  chmod +x "$fakebin/$tool"
}

# make_case <name> <harness>: a throwaway firstmate home, a git project, and an
# isolated worktree, as the spawn suites build them. Sets the case variables.
make_case() {
  local name=$1 harness=$2
  CASE_DIR="$TMP_ROOT/$name"
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
  make_pi_probe "$FAKEBIN_DIR" pi
  make_pi_probe "$FAKEBIN_DIR" pi-signed
  # spawn resolves omp on PATH before launch (fm-spawn.sh omp case); a stub is enough.
  make_pi_probe "$FAKEBIN_DIR" omp
  fm_test_spawn_home "$HOME_DIR" "$harness"
  fm_git_worktree "$PROJ_DIR" "$WT_DIR" "wt-$name"
}

# run_case <id> [fm-spawn args...]: one spawn through the suite's common env.
run_case() {
  local id=$1
  shift
  : > "$LAUNCH_LOG"
  FM_FAKE_LAUNCH_LOG="$LAUNCH_LOG" FM_FAKE_PI_VERSION=0.84.0 \
    fm_test_run_spawn "$HOME_DIR" "$WT_DIR" "$FAKEBIN_DIR" "$id" "$@"
}

# good_block <task-id> <mode>: the block a ship brief carries, one field per line.
good_block() {
  printf '%s\n' "ownership: motif" "task_id: $1" "worktree: {WORKTREE}" \
    "delivery_mode: $2" "helper_allowance: read-only-checks, isolated-scratch"
}

# write_brief_with_body <home> <id> <body>: the fixture brief plus one managed
# block whose body is exactly <body>, so a case can break one field at a time.
write_brief_with_body() {
  local home=$1 id=$2 body=$3
  fm_test_spawn_brief "$home" "$id"
  {
    printf '%s\n' '# Managed context' '```managed-context'
    printf '%s\n' "$body"
    printf '%s\n' '```'
  } >>"$home/data/$id/brief.md"
}

meta_value() {  # <meta-file> <key>
  sed -n "s/^$2=//p" "$1" | head -n 1
}

set_hve_version() {  # <version-or-empty>
  if [ -n "$1" ]; then
    FM_TEST_HVE_VERSION=$1
    export FM_TEST_HVE_VERSION
  else
    unset FM_TEST_HVE_VERSION
  fi
}

# expect_refused <label> <expected-substring> <id> [fm-spawn args...]: the spawn
# must exit 1, name the expected field or value, and leave no launch, no task
# record, and no composed launch brief behind.
expect_refused() {
  local label=$1 expected=$2 id=$3 out status
  shift 3
  out=$(run_case "$id" "$PROJ_DIR" "$@")
  status=$?
  expect_code 1 "$status" "$label should refuse"$'\n'"$out"
  assert_contains "$out" "$expected" "$label refusal did not name '$expected'"$'\n'"$out"
  [ ! -s "$LAUNCH_LOG" ] || fail "$label launched despite the refusal"
  [ ! -e "$HOME_DIR/state/$id.meta" ] || fail "$label published a task record despite the refusal"
  [ ! -e "$HOME_DIR/data/$id/launch-brief.md" ] || fail "$label composed a launch brief despite the refusal"
}

# --- supported launches ----------------------------------------------------

test_each_supported_harness_binds_the_managed_context_into_its_launch() {
  local h id out status meta launch_brief recorded_wt
  for h in claude codex pi pi-signed omp copilot; do
    id=mc-ok-$h
    make_case "ok-$h" "$h"
    fm_test_spawn_ship_brief "$HOME_DIR" "$id" no-mistakes
    out=$(run_case "$id" "$PROJ_DIR" --mode no-mistakes --yolo off)
    status=$?
    expect_code 0 "$status" "$h ship spawn with a valid managed context should launch"$'\n'"$out"
    [ -s "$LAUNCH_LOG" ] || fail "$h ship spawn with a valid managed context did not launch"

    meta="$HOME_DIR/state/$id.meta"
    recorded_wt=$(meta_value "$meta" worktree)
    [ -n "$recorded_wt" ] || fail "$h task record has no worktree="
    assert_grep "managed_ownership=motif" "$meta" "$h task record is missing managed_ownership=motif"
    assert_grep "managed_task_id=$id" "$meta" "$h task record is missing managed_task_id=$id"
    assert_grep "managed_worktree=$recorded_wt" "$meta" "$h task record does not record the allocated worktree"
    assert_grep "managed_delivery_mode=no-mistakes" "$meta" "$h task record is missing managed_delivery_mode=no-mistakes"
    assert_grep "managed_helper_allowance=read-only-checks,isolated-scratch" "$meta" \
      "$h task record is missing the helper allowance"

    launch_brief="$HOME_DIR/data/$id/launch-brief.md"
    assert_grep "worktree: $recorded_wt" "$launch_brief" "$h launch brief does not name the allocated worktree"
    assert_no_grep "worktree: {WORKTREE}" "$launch_brief" "$h launch brief kept the placeholder"
    assert_grep "worktree: {WORKTREE}" "$HOME_DIR/data/$id/brief.md" \
      "$h binding rewrote the source brief instead of the launch brief"
  done
  pass "claude, codex, pi, pi-signed, omp, and copilot ship launches bind the managed context and record its fields"
}

test_direct_pr_ship_records_its_mode() {
  local id=mc-direct-pr
  make_case direct-pr claude
  fm_test_spawn_ship_brief "$HOME_DIR" "$id" direct-PR
  out=$(run_case "$id" "$PROJ_DIR" --mode direct-PR --yolo off)
  status=$?
  expect_code 0 "$status" "direct-PR ship spawn should launch"$'\n'"$out"
  assert_grep "managed_delivery_mode=direct-PR" "$HOME_DIR/state/$id.meta" "direct-PR ship did not record its mode"
  pass "a direct-PR ship launch records managed_delivery_mode=direct-PR"
}

# --- standalone scout: unchanged --------------------------------------------

test_standalone_scout_launch_carries_no_managed_context() {
  local id=mc-scout
  make_case scout claude
  fm_test_spawn_brief "$HOME_DIR" "$id"
  out=$(run_case "$id" "$PROJ_DIR" --scout)
  status=$?
  expect_code 0 "$status" "a scout launch with no managed context should still launch"$'\n'"$out"
  [ -s "$LAUNCH_LOG" ] || fail "scout did not launch"
  assert_grep "kind=scout" "$HOME_DIR/state/$id.meta" "scout task record lost kind=scout"
  assert_no_grep "managed_" "$HOME_DIR/state/$id.meta" "scout task record gained managed fields"
  assert_no_grep "# Managed context" "$HOME_DIR/data/$id/launch-brief.md" "scout launch brief gained a managed-context block"
  pass "a standalone scout launch is unchanged and records no managed context"
}

# --- refusals before launch --------------------------------------------------

test_retired_delivery_modes_and_yolo_on_are_refused_by_name() {
  local id=mc-retired
  make_case retired claude
  write_brief_with_body "$HOME_DIR" "$id" "$(good_block "$id" no-mistakes)"
  expect_refused "--mode local-only" "delivery mode 'local-only' is not supported for ship launches" \
    "$id" --mode local-only --yolo off
  expect_refused "--yolo on with no-mistakes" "--yolo on is refused for ship $id" \
    "$id" --mode no-mistakes --yolo on
  pass "a ship spawn refuses local-only and --yolo on by name before launch or task record"
}

test_yolo_on_refusal_holds_for_direct_pr() {
  local id=mc-yolo-direct
  make_case yolo-direct claude
  write_brief_with_body "$HOME_DIR" "$id" "$(good_block "$id" direct-PR)"
  expect_refused "--yolo on with direct-PR" "--yolo on is refused for ship $id" \
    "$id" --mode direct-PR --yolo on
  pass "a direct-PR ship spawn refuses --yolo on as well"
}

test_absent_managed_context_refuses() {
  local id=mc-absent
  make_case absent claude
  fm_test_spawn_brief "$HOME_DIR" "$id"
  expect_refused "an absent block" "$id/brief.md has no managed-context block" \
    "$id" --mode no-mistakes --yolo off
  pass "a ship brief with no managed-context block refuses, naming the block"
}

test_managed_context_defects_name_their_field() {
  local id body
  make_case defects claude
  id=mc-dup
  body=$(good_block "$id" no-mistakes)
  fm_test_spawn_ship_brief "$HOME_DIR" "$id" no-mistakes
  {
    printf '%s\n' '# Managed context' '```managed-context'
    printf '%s\n' "$body"
    printf '%s\n' '```'
  } >>"$HOME_DIR/data/$id/brief.md"
  expect_refused "two blocks" "carries more than one managed-context block" \
    "$id" --mode no-mistakes --yolo off

  id=mc-missing
  write_brief_with_body "$HOME_DIR" "$id" "$(good_block "$id" no-mistakes | sed '/^delivery_mode:/d')"
  expect_refused "a missing delivery_mode" "managed-context is missing field 'delivery_mode'" \
    "$id" --mode no-mistakes --yolo off

  id=mc-contra-task
  write_brief_with_body "$HOME_DIR" "$id" "$(good_block mc-other no-mistakes)"
  expect_refused "a contradictory task_id" "managed-context field 'task_id' is 'mc-other' but this spawn launches task '$id'" \
    "$id" --mode no-mistakes --yolo off

  id=mc-contra-mode
  write_brief_with_body "$HOME_DIR" "$id" "$(good_block "$id" direct-PR)"
  expect_refused "a contradictory delivery_mode" "managed-context field 'delivery_mode' is 'direct-PR' but this spawn launches mode=no-mistakes" \
    "$id" --mode no-mistakes --yolo off

  id=mc-block-local
  write_brief_with_body "$HOME_DIR" "$id" "$(good_block "$id" local-only)"
  expect_refused "a block that carries local-only" "delivery mode 'local-only' is not supported" \
    "$id" --mode no-mistakes --yolo off

  id=mc-literal-wt
  write_brief_with_body "$HOME_DIR" "$id" "$(good_block "$id" no-mistakes | sed 's|^worktree: .*|worktree: /tmp/elsewhere|')"
  expect_refused "a literal worktree path" "managed-context field 'worktree' is '/tmp/elsewhere' but the brief must carry {WORKTREE}" \
    "$id" --mode no-mistakes --yolo off

  id=mc-owner
  write_brief_with_body "$HOME_DIR" "$id" "$(good_block "$id" no-mistakes | sed 's|^ownership: .*|ownership: someone-else|')"
  expect_refused "a foreign ownership" "managed-context field 'ownership' is 'someone-else'" \
    "$id" --mode no-mistakes --yolo off

  id=mc-helper-bad
  write_brief_with_body "$HOME_DIR" "$id" "$(good_block "$id" no-mistakes | sed 's|^helper_allowance: .*|helper_allowance: network-access|')"
  expect_refused "an unknown helper" "managed-context field 'helper_allowance' names 'network-access', which is not allowed" \
    "$id" --mode no-mistakes --yolo off

  id=mc-helper-space
  write_brief_with_body "$HOME_DIR" "$id" "$(good_block "$id" no-mistakes | sed 's|^helper_allowance: .*|helper_allowance: read-only-checks isolated-scratch|')"
  expect_refused "a helper list without commas" "managed-context field 'helper_allowance' entry 'read-only-checks isolated-scratch' contains a space" \
    "$id" --mode no-mistakes --yolo off

  id=mc-helper-repeat
  write_brief_with_body "$HOME_DIR" "$id" "$(good_block "$id" no-mistakes | sed 's|^helper_allowance: .*|helper_allowance: read-only-checks,read-only-checks|')"
  expect_refused "a repeated helper" "managed-context field 'helper_allowance' repeats 'read-only-checks'" \
    "$id" --mode no-mistakes --yolo off

  id=mc-helper-empty
  write_brief_with_body "$HOME_DIR" "$id" "$(good_block "$id" no-mistakes | sed 's|^helper_allowance: .*|helper_allowance:|')"
  expect_refused "an empty helper allowance" "managed-context field 'helper_allowance' is empty" \
    "$id" --mode no-mistakes --yolo off

  id=mc-unknown-field
  write_brief_with_body "$HOME_DIR" "$id" "$(good_block "$id" no-mistakes; printf '%s\n' 'region: eu')"
  expect_refused "an unknown field" "managed-context field 'region' is not a managed-context field" \
    "$id" --mode no-mistakes --yolo off

  id=mc-repeat-field
  write_brief_with_body "$HOME_DIR" "$id" "$(good_block "$id" no-mistakes; printf '%s\n' 'delivery_mode: no-mistakes')"
  expect_refused "a repeated field" "managed-context field 'delivery_mode' appears more than once" \
    "$id" --mode no-mistakes --yolo off

  id=mc-malformed
  write_brief_with_body "$HOME_DIR" "$id" "$(good_block "$id" no-mistakes; printf '%s\n' 'just words')"
  expect_refused "a malformed line" "managed-context line 'just words' is not a 'field: value' pair" \
    "$id" --mode no-mistakes --yolo off

  pass "every managed-context defect refuses before launch and names the field, value, or block it contradicts"
}

# --- supported workflow-agent version range ----------------------------------

test_hve_version_outside_range_refuses_and_names_the_range() {
  local id=mc-hve-high
  make_case hve-high claude
  fm_test_spawn_ship_brief "$HOME_DIR" "$id" no-mistakes
  set_hve_version 0.3.0
  expect_refused "an hve version above the range" "plugin version 0.3.0 for claude is outside the host's supported range >=0.2.0 <0.3.0" \
    "$id" --mode no-mistakes --yolo off

  id=mc-hve-low
  make_case hve-low claude
  fm_test_spawn_ship_brief "$HOME_DIR" "$id" no-mistakes
  set_hve_version 0.1.9
  expect_refused "an hve version below the range" "plugin version 0.1.9 for claude is outside the host's supported range >=0.2.0 <0.3.0" \
    "$id" --mode no-mistakes --yolo off
  set_hve_version ""
  pass "an hve version outside >=0.2.0 <0.3.0 refuses before launch and names the version and range"
}

test_hve_version_inside_range_launches() {
  local id=mc-hve-in
  make_case hve-in claude
  fm_test_spawn_ship_brief "$HOME_DIR" "$id" no-mistakes
  set_hve_version 0.2.9
  out=$(run_case "$id" "$PROJ_DIR" --mode no-mistakes --yolo off)
  status=$?
  set_hve_version ""
  expect_code 0 "$status" "an in-range hve version should launch"$'\n'"$out"
  [ -s "$LAUNCH_LOG" ] || fail "an in-range hve version did not launch"
  pass "an hve version inside the declared range launches"
}

test_missing_hve_version_record_refuses() {
  local id=mc-hve-missing
  make_case hve-missing claude
  fm_test_spawn_ship_brief "$HOME_DIR" "$id" no-mistakes
  # Seed the stores once, then drop the claude install record: the run helper
  # reseeds on every call, so this case skips the reseed and keeps the removal.
  fm_test_seed_workflow_agents "$HOME_DIR/user-home"
  rm -f "$HOME_DIR/user-home/.claude/plugins/installed_plugins.json"
  out=$(FM_TEST_NO_WORKFLOW_AGENT=1 run_case "$id" "$PROJ_DIR" --mode no-mistakes --yolo off)
  status=$?
  expect_code 1 "$status" "a missing hve version record should refuse"$'\n'"$out"
  assert_contains "$out" "could not read the hve@mayo-skills plugin version for claude (supported range >=0.2.0 <0.3.0)" \
    "missing version record refusal did not name the plugin and range"
  [ ! -s "$LAUNCH_LOG" ] || fail "a ship launched without a readable hve version"
  pass "a ship refuses when the hve version cannot be read, naming the range"
}

test_out_of_range_hve_version_also_refuses_a_scout() {
  local id=mc-scout-hve
  make_case scout-hve claude
  fm_test_spawn_brief "$HOME_DIR" "$id"
  set_hve_version 0.3.0
  out=$(run_case "$id" "$PROJ_DIR" --scout)
  status=$?
  set_hve_version ""
  expect_code 1 "$status" "a scout on an out-of-range hve version should refuse"$'\n'"$out"
  assert_contains "$out" "plugin version 0.3.0 for claude is outside the host's supported range >=0.2.0 <0.3.0" \
    "scout refusal did not name the version and range"
  [ ! -s "$LAUNCH_LOG" ] || fail "a scout launched on an out-of-range hve version"
  pass "the version range applies to scouts too"
}

# --- promotion ---------------------------------------------------

# promote_profile <case-dir> <id>: the assignment-profile record a promotion of
# this scout carries, pinned to the scout worktree HEAD promotion checks against.
promote_profile() {
  fm_test_ship_profile_file "$1/profiles" "$2" "$(git -C "$WT_DIR" rev-parse --verify --quiet HEAD)"
}

test_promotion_refuses_without_an_assignment_profile() {
  local id=mc-promote-noprofile meta out status
  make_case promote-noprofile claude
  fm_test_spawn_brief "$HOME_DIR" "$id"
  meta="$HOME_DIR/state/$id.meta"
  printf 'window=fm-%s\nkind=scout\nworktree=%s\n' "$id" "$WT_DIR" > "$meta"

  out=$(FM_HOME="$HOME_DIR" FM_STATE_OVERRIDE="$HOME_DIR/state" "$PROMOTE" "$id" --mode direct-PR --yolo off 2>&1)
  status=$?
  expect_code 1 "$status" "a promotion without its assignment-profile record should refuse"$'\n'"$out"
  assert_contains "$out" "promotion requires --assignment-profile" "the refusal should name the missing record"
  assert_grep "kind=scout" "$meta" "a refused promotion must leave the scout as it was"
  [ ! -e "$HOME_DIR/data/$id/assignment-profile.json" ] || fail "a refused promotion must persist no record"
  pass "a promotion without an assignment-profile record refuses before it writes anything"
}

test_promotion_writes_the_managed_record_and_a_relaunchable_brief() {
  local id=mc-promote-ok meta out status profile
  make_case promote-ok claude
  fm_test_spawn_brief "$HOME_DIR" "$id"
  meta="$HOME_DIR/state/$id.meta"
  printf 'window=fm-%s\nkind=scout\nworktree=%s\n' "$id" "$WT_DIR" > "$meta"
  profile=$(promote_profile "$CASE_DIR" "$id")

  out=$(FM_HOME="$HOME_DIR" FM_STATE_OVERRIDE="$HOME_DIR/state" "$PROMOTE" "$id" --mode direct-PR --yolo off --assignment-profile "$profile" 2>&1)
  status=$?
  expect_code 0 "$status" "promotion to direct-PR with yolo off should succeed"$'\n'"$out"
  assert_grep "profile_decision_id=$id/1" "$meta" "promotion did not record the decision id"
  assert_grep "profile_policy_revision=motif-assignment-profile-r2" "$meta" "promotion did not record the policy revision"
  [ -f "$HOME_DIR/data/$id/assignment-profile.json" ] || fail "promotion did not persist the record a relaunch restores"
  assert_grep "kind=ship" "$meta" "promotion did not flip the task to ship"
  assert_grep "mode=direct-PR" "$meta" "promotion did not record the mode"
  assert_grep "managed_ownership=motif" "$meta" "promotion did not record managed_ownership"
  assert_grep "managed_task_id=$id" "$meta" "promotion did not record managed_task_id"
  assert_grep "managed_worktree=$WT_DIR" "$meta" "promotion did not record the scout's worktree"
  assert_grep "managed_delivery_mode=direct-PR" "$meta" "promotion did not record managed_delivery_mode"
  assert_grep "managed_helper_allowance=read-only-checks,isolated-scratch" "$meta" \
    "promotion did not record the helper allowance"

  assert_grep "# Managed context" "$HOME_DIR/data/$id/brief.md" "the promoted brief lacks the managed-context block"
  assert_grep "delivery_mode: direct-PR" "$HOME_DIR/data/$id/brief.md" "the promoted block names the wrong mode"
  assert_grep "worktree: {WORKTREE}" "$HOME_DIR/data/$id/brief.md" "the promoted block does not keep the placeholder"
  pass "a promotion records the managed fields and writes a brief that carries the managed block"
}

test_promoted_brief_validates_under_its_recorded_mode() {
  local id=mc-promote-validate meta profile
  make_case promote-validate claude
  fm_test_spawn_brief "$HOME_DIR" "$id"
  meta="$HOME_DIR/state/$id.meta"
  printf 'window=fm-%s\nkind=scout\nworktree=%s\n' "$id" "$WT_DIR" > "$meta"
  profile=$(promote_profile "$CASE_DIR" "$id")
  FM_HOME="$HOME_DIR" FM_STATE_OVERRIDE="$HOME_DIR/state" "$PROMOTE" "$id" --mode direct-PR --yolo off --assignment-profile "$profile" >/dev/null 2>&1 \
    || fail "setup promotion failed"
  # The promoted brief is the one a relaunch composes from. Validate it with the
  # same contract the spawn runs, under the mode the promotion recorded.
  (
    . "$ROOT/bin/fm-managed-context-lib.sh"
    fm_managed_context_validate "$HOME_DIR/data/$id/brief.md" "$id" direct-PR "promote-test $id"
  ) || fail "the promoted brief does not validate under its recorded mode"
  pass "a promoted brief validates under the mode the promotion recorded"
}

test_bind_rewrites_every_worktree_line_form_validate_accepts() {
  local variant id=0 launch_brief recorded_wt
  for variant in 'worktree : {WORKTREE}' '  worktree: {WORKTREE}' '	worktree	:	{WORKTREE}  '; do
    id=$((id + 1))
    make_case "bind-ws-$id" claude
    write_brief_with_body "$HOME_DIR" "mc-bind-ws-$id" "$(good_block "mc-bind-ws-$id" no-mistakes | sed "s|^worktree: .*|$variant|")"
    out=$(run_case "mc-bind-ws-$id" "$PROJ_DIR" --mode no-mistakes --yolo off)
    status=$?
    expect_code 0 "$status" "a worktree line written as '$variant' should launch"$'\n'"$out"
    recorded_wt=$(meta_value "$HOME_DIR/state/mc-bind-ws-$id.meta" worktree)
    launch_brief="$HOME_DIR/data/mc-bind-ws-$id/launch-brief.md"
    assert_grep "worktree: $recorded_wt" "$launch_brief" "a worktree line written as '$variant' was not bound to the allocated worktree"
    assert_no_grep "{WORKTREE}" "$launch_brief" "a worktree line written as '$variant' kept the placeholder in the launch brief"
  done
  pass "a worktree line with whitespace around its key or value is bound to the allocated worktree"
}

test_each_refusal_leaves_no_partial_launch_brief() {
  # A refused fresh ship must not leave an earlier launch's brief behind either:
  # the composition that writes it runs only after the block validates.
  local id=mc-no-partial
  make_case no-partial claude
  write_brief_with_body "$HOME_DIR" "$id" "$(good_block "$id" no-mistakes | sed 's|^task_id: .*|task_id: mc-other|')"
  expect_refused "a contradictory task_id before composition" "task_id' is 'mc-other'" \
    "$id" --mode no-mistakes --yolo off
  pass "a refused ship leaves no composed launch brief"
}

# The helper-class contract is a pure library check: each class maps to one allowance
# token, and a helper is allowed only when the brief's helper_allowance grants it.
test_helper_classes_map_to_allowance_tokens() {
  local out
  . "$ROOT/bin/fm-managed-context-lib.sh"
  assert_equals read-only-checks "$(fm_managed_context_helper_token grounding)" "grounding needs read-only-checks"
  assert_equals read-only-checks "$(fm_managed_context_helper_token design-critique)" "design critique needs read-only-checks"
  assert_equals read-only-checks "$(fm_managed_context_helper_token bounded-check-execution)" "bounded check execution needs read-only-checks"
  assert_equals isolated-scratch "$(fm_managed_context_helper_token design-sketch)" "a design sketch needs isolated-scratch"
  assert_equals isolated-scratch "$(fm_managed_context_helper_token candidate-scratch-output)" "candidate scratch output needs isolated-scratch"
  fm_managed_context_helper_token shell-exec >/dev/null 2>&1 && fail "an unknown helper class must map to no token"
  pass "each helper class maps to the one allowance token it needs, and an unknown class maps to none"

  fm_managed_context_helper_check 'read-only-checks, isolated-scratch' grounding fixture >/dev/null 2>&1 ||
    fail "a grounding helper under the full allowance must be allowed"
  fm_managed_context_helper_check 'isolated-scratch' design-sketch fixture >/dev/null 2>&1 ||
    fail "a design sketch under isolated-scratch must be allowed"
  pass "a helper is allowed when its allowance token is granted"

  out=$(fm_managed_context_helper_check '' grounding fixture 2>&1) && fail "an absent helper_allowance must deny a helper"
  assert_contains "$out" "needs the 'read-only-checks' allowance" "an absent helper_allowance denies and names the token"
  out=$(fm_managed_context_helper_check 'read-only-checks' design-sketch fixture 2>&1) && fail "a design sketch without isolated-scratch must be denied"
  assert_contains "$out" "needs the 'isolated-scratch' allowance" "a missing token denies and names the token it lacks"
  out=$(fm_managed_context_helper_check 'read-only-checks, isolated-scratch' shell-exec fixture 2>&1) && fail "an unknown helper class must be denied even under the full allowance"
  assert_contains "$out" "helper class 'shell-exec' is unknown" "an unknown class denies and is named"
  pass "a helper is denied when its token is absent or its class is unknown"
}

test_each_supported_harness_binds_the_managed_context_into_its_launch
test_direct_pr_ship_records_its_mode
test_standalone_scout_launch_carries_no_managed_context
test_retired_delivery_modes_and_yolo_on_are_refused_by_name
test_yolo_on_refusal_holds_for_direct_pr
test_absent_managed_context_refuses
test_managed_context_defects_name_their_field
test_hve_version_outside_range_refuses_and_names_the_range
test_hve_version_inside_range_launches
test_missing_hve_version_record_refuses
test_out_of_range_hve_version_also_refuses_a_scout
test_promotion_refuses_without_an_assignment_profile
test_promotion_writes_the_managed_record_and_a_relaunchable_brief
test_promoted_brief_validates_under_its_recorded_mode
test_bind_rewrites_every_worktree_line_form_validate_accepts
test_each_refusal_leaves_no_partial_launch_brief
test_helper_classes_map_to_allowance_tokens

echo "# all fm-managed-context tests passed"
