#!/usr/bin/env bash
# Behavior tests for bin/fm-cloud-policy.sh, the single authorization owner for
# cloud assignment routes (DPES-874).
#
# Every case runs the executable and asserts on its exit status, its typed
# denial reason on stderr, and whether a stub cloud client actually ran. Nothing
# here greps the script's source.
#
# Coverage map (DPES-874 acceptance criteria, partial by design):
#   AC2  attended branch refused for mutations; invalid task and run ids refused;
#        a foreign or unknown run refused as unbound; observation never widened
#   AC3  no-mistakes gate refusal for submit, cancel, and fetch; doctor allowed
#   AC4  spend cap counts local ordinary records only, the cloud term fails
#        closed, the task-set lock refuses without waiting, and the branch's
#        queued-work entitlement is enforced
#   AC5  partial: doctor runs with clean stdout and no prompt; unmanaged roots
#        are refused before the client runs
# Not covered: the shared cloud client, run ownership, the native tool, and the
# mcao route, none of which exist on this branch (their DPES-805, 808, 814, 873,
# and 875 dependencies are reported as blockers in the PR body).
set -u

# shellcheck source=tests/fixtures.sh
. "$(dirname "${BASH_SOURCE[0]}")/fixtures.sh"

POLICY="$ROOT/bin/fm-cloud-policy.sh"
AFK="$ROOT/bin/fm-afk-contract.sh"

TMP=$(fm_test_tmproot fm-cloud-policy)
fm_git_identity fmtest fmtest@example.invalid

# Keep the ambient supervision, gate, and home-relocation environment out of
# every case; each case sets exactly what it needs.
unset NO_MISTAKES_GATE FM_GATE_REFUSE_BYPASS FM_SUPERVISION_ACTOR FM_HOME \
  FM_ROOT_OVERRIDE FM_STATE_OVERRIDE FM_DATA_OVERRIDE FM_CONFIG_OVERRIDE \
  FM_PROJECTS_OVERRIDE

# The stub cloud client records that it ran (to a fixed log) and prints one
# line, so a test can prove both that the command ran and what stdout it made.
RAN_LOG="$TMP/ran.log"
STUB="$TMP/stub/cloud-client"
mkdir -p "$TMP/stub"
cat > "$STUB" <<SH
#!/usr/bin/env bash
printf '%s\n' "\$*" >> "$RAN_LOG"
printf 'stub-out\n'
exit "\${STUB_STATUS:-0}"
SH
chmod +x "$STUB"

# make_home <name>: sets FX_ROOT (a primary checkout: committed AGENTS.md and a
# bin symlink to this checkout) and FX_HOME (state/, data/, config/, and a manual
# backlog with one queued and one in-flight task).
make_home() {
  FX_ROOT="$TMP/$1-root"
  FX_HOME="$TMP/$1-home"
  mkdir -p "$FX_ROOT" "$FX_HOME/state" "$FX_HOME/data" "$FX_HOME/config"
  git init -q -b main "$FX_ROOT"
  printf '# fixture primary\n' > "$FX_ROOT/AGENTS.md"
  git -C "$FX_ROOT" add AGENTS.md
  git -C "$FX_ROOT" commit -q -m init
  ln -s "$ROOT/bin" "$FX_ROOT/bin"
  fm_test_tasks_toml "$FX_HOME/.tasks.toml"
  printf 'manual\n' > "$FX_HOME/config/backlog-backend"
  cat > "$FX_HOME/data/backlog.md" <<'EOF'
## In flight
- [ ] task-inflight - orphaned in-flight work

## Queued
- [ ] task-queued - already queued work

## Done
EOF
}

# enter_away <home> <spend>: put the home under an away-posture record.
enter_away() {
  FM_HOME="$1" "$AFK" enter --spend "$2" >/dev/null 2>&1 || fail "away entry failed for spend $2"
}

# owner_run <root> <home> <args...>: run the owner against a fixture. Stdout and
# stderr are captured separately, the stub's run log is reset for this case, and
# the exit status is kept. Ambient env prefixes on the caller apply here too.
owner_run() {
  local root=$1 home=$2
  shift 2
  rm -f "$RAN_LOG" "$TMP/last.err"
  RUN_STATUS=0
  RUN_OUT=$(FM_HOME="$home" FM_ROOT_OVERRIDE="$root" "$POLICY" "$@" 2>"$TMP/last.err") || RUN_STATUS=$?
  RUN_ERR=$(cat "$TMP/last.err")
}

# expect_denied <label> <status> <action> <reason>: the owner refused with the
# typed reason, wrote nothing to stdout, and the stub client never ran.
expect_denied() {
  [ "$RUN_STATUS" -eq "$2" ] || fail "$1 exited $RUN_STATUS, not $2: $RUN_ERR"
  assert_contains "$RUN_ERR" "error: cloud $3 refused - $4:" "$1 lost its typed reason"
  [ -z "$RUN_OUT" ] || fail "$1 wrote to stdout: $RUN_OUT"
  [ ! -e "$RAN_LOG" ] || fail "$1 ran the cloud client despite refusing"
}

# expect_usage <label>: the owner rejected the invocation at parse time.
expect_usage() {
  [ "$RUN_STATUS" -eq 2 ] || fail "$1 exited $RUN_STATUS, not 2: $RUN_ERR"
  [ ! -e "$RAN_LOG" ] || fail "$1 ran the cloud client despite a usage error"
}

# hold_task_set_lock <state-dir>: a background holder takes the home's task-set
# lock and sleeps. Sets HOLDER_PID; the caller must kill it.
hold_task_set_lock() {
  local lock=$1/.task-set.lock i=0
  (
    # shellcheck source=/dev/null
    . "$ROOT/bin/fm-wake-lib.sh"
    fm_lock_try_acquire "$lock" || exit 1
    exec sleep 30
  ) >/dev/null 2>&1 &
  HOLDER_PID=$!
  while [ ! -e "$lock" ] && [ "$i" -lt 100 ]; do
    sleep 0.1
    i=$((i + 1))
  done
  [ -e "$lock" ] || {
    kill "$HOLDER_PID" 2>/dev/null || true
    wait "$HOLDER_PID" 2>/dev/null || true
    fail "could not stage a held task-set lock"
  }
}

# --- classification and parse -----------------------------------------------

test_doctor_runs_with_clean_output_and_no_prompt() {
  make_home observe
  owner_run "$FX_ROOT" "$FX_HOME" run doctor -- "$STUB"
  [ "$RUN_STATUS" -eq 0 ] || fail "doctor exited $RUN_STATUS: $RUN_ERR"
  [ "$RUN_OUT" = stub-out ] || fail "doctor stdout was not exactly the client's output: $RUN_OUT"
  [ -z "$RUN_ERR" ] || fail "doctor wrote to stderr: $RUN_ERR"
  [ -f "$RAN_LOG" ] || fail "doctor did not run the client"
  pass "doctor runs the client with clean output and no prompt"
}

test_unknown_action_is_an_unsupported_denial() {
  make_home unknown
  owner_run "$FX_ROOT" "$FX_HOME" run frobnicate -- "$STUB"
  expect_denied "an unknown action" 1 frobnicate unsupported-action
  pass "an unknown action is refused as unsupported and never reaches the client"
}

test_parse_errors_exit_two() {
  make_home parse
  owner_run "$FX_ROOT" "$FX_HOME" run doctor "$STUB"
  expect_usage "a missing --"
  owner_run "$FX_ROOT" "$FX_HOME" run doctor --
  expect_usage "an empty command"
  owner_run "$FX_ROOT" "$FX_HOME" run doctor --bogus -- "$STUB"
  expect_usage "an unknown flag"
  owner_run "$FX_ROOT" "$FX_HOME" run status -- "$STUB"
  expect_usage "status without --run"
  owner_run "$FX_ROOT" "$FX_HOME" run submit -- "$STUB"
  expect_usage "submit without --task"
  owner_run "$FX_ROOT" "$FX_HOME" run cancel --task task-x -- "$STUB"
  expect_usage "cancel without --run"
  pass "parse errors exit 2 before any check and never reach the client"
}

# --- managed context --------------------------------------------------------

test_unmanaged_roots_are_refused_before_the_client_runs() {
  make_home unmanaged
  local plain="$TMP/plain-root"
  mkdir -p "$plain"
  git init -q -b main "$plain"
  git -C "$plain" commit -q --allow-empty -m init
  ln -s "$ROOT/bin" "$plain/bin"
  owner_run "$plain" "$FX_HOME" run doctor -- "$STUB"
  expect_denied "a root without AGENTS.md" 1 doctor unmanaged-context

  git -C "$FX_ROOT" worktree add -q "$TMP/linked-wt" -b linked-wt
  ln -s "$ROOT/bin" "$TMP/linked-wt/bin"
  owner_run "$TMP/linked-wt" "$FX_HOME" run submit --task task-queued -- "$STUB"
  expect_denied "a linked worktree submit" 1 submit unmanaged-context
  pass "unmanaged roots and linked worktrees are refused before the client runs"
}

# --- gate (AC3) --------------------------------------------------------------

test_gate_refuses_mutations_and_keeps_doctor() {
  make_home gate
  NO_MISTAKES_GATE=1 owner_run "$FX_ROOT" "$FX_HOME" run doctor -- "$STUB"
  [ "$RUN_STATUS" -eq 0 ] || fail "doctor under the gate marker exited $RUN_STATUS: $RUN_ERR"
  [ -f "$RAN_LOG" ] || fail "doctor under the gate marker did not run the client"

  NO_MISTAKES_GATE=1 owner_run "$FX_ROOT" "$FX_HOME" run submit --task task-queued -- "$STUB"
  [ "$RUN_STATUS" -eq 3 ] || fail "submit under the gate marker exited $RUN_STATUS, not 3: $RUN_ERR"
  assert_contains "$RUN_ERR" "NO_MISTAKES_GATE set" "the gate refusal lost its reason"
  [ ! -e "$RAN_LOG" ] || fail "submit under the gate marker ran the client"

  NO_MISTAKES_GATE=1 owner_run "$FX_ROOT" "$FX_HOME" run cancel --task task-x --run run-1 -- "$STUB"
  [ "$RUN_STATUS" -eq 3 ] || fail "cancel under the gate marker exited $RUN_STATUS, not 3"
  [ ! -e "$RAN_LOG" ] || fail "cancel under the gate marker ran the client"

  NO_MISTAKES_GATE=1 owner_run "$FX_ROOT" "$FX_HOME" run fetch --task task-x --run run-1 -- "$STUB"
  [ "$RUN_STATUS" -eq 3 ] || fail "fetch under the gate marker exited $RUN_STATUS, not 3"
  [ ! -e "$RAN_LOG" ] || fail "fetch under the gate marker ran the client"
  pass "the gate marker refuses submit, cancel, and fetch and leaves doctor allowed"
}

test_gate_worktree_path_refuses_without_the_marker() {
  make_home gate-path
  local gate_root="$TMP/gate-path-nm" id=016d88035d58 run=01KXC3SD5NZYMERGDS68Z1C8ER seed
  mkdir -p "$gate_root/.no-mistakes/repos"
  git init -q --bare "$gate_root/origin.git"
  seed="$TMP/gate-path-seed"
  git init -q -b main "$seed"
  git -C "$seed" commit -q --allow-empty -m init
  git -C "$seed" push -q "$gate_root/origin.git" HEAD:refs/heads/main
  git clone -q --bare "$gate_root/origin.git" "$gate_root/.no-mistakes/repos/$id.git"
  git -C "$gate_root/.no-mistakes/repos/$id.git" worktree add --detach \
    "$gate_root/.no-mistakes/worktrees/$id/$run" main >/dev/null 2>&1 \
    || fail "could not stage the gate worktree"
  owner_run "$gate_root/.no-mistakes/worktrees/$id/$run" "$FX_HOME" \
    run submit --task task-queued -- "$STUB"
  [ "$RUN_STATUS" -eq 3 ] || fail "a gate worktree submit exited $RUN_STATUS, not 3: $RUN_ERR"
  assert_contains "$RUN_ERR" "no-mistakes gate worktree" "the path backstop lost its reason"
  [ ! -e "$RAN_LOG" ] || fail "a gate worktree submit ran the client"
  pass "a gate worktree is refused by the path backstop even without the marker"
}

# --- attended branch, ids, and binding (AC2) ---------------------------------

test_attended_branch_is_refused_for_every_mutation() {
  make_home branch
  FM_SUPERVISION_ACTOR=branch owner_run "$FX_ROOT" "$FX_HOME" run submit --task task-queued -- "$STUB"
  [ "$RUN_STATUS" -eq 6 ] || fail "branch submit exited $RUN_STATUS, not 6: $RUN_ERR"
  [ ! -e "$RAN_LOG" ] || fail "branch submit ran the client"
  FM_SUPERVISION_ACTOR=branch owner_run "$FX_ROOT" "$FX_HOME" run cancel --task task-x --run run-1 -- "$STUB"
  [ "$RUN_STATUS" -eq 6 ] || fail "branch cancel exited $RUN_STATUS, not 6"
  [ ! -e "$RAN_LOG" ] || fail "branch cancel ran the client"
  FM_SUPERVISION_ACTOR=branch owner_run "$FX_ROOT" "$FX_HOME" run fetch --task task-x --run run-1 -- "$STUB"
  [ "$RUN_STATUS" -eq 6 ] || fail "branch fetch exited $RUN_STATUS, not 6"
  [ ! -e "$RAN_LOG" ] || fail "branch fetch ran the client"
  pass "an attended branch is refused for submit, cancel, and fetch"
}

test_invalid_ids_and_malformed_run_refuse() {
  make_home ids
  owner_run "$FX_ROOT" "$FX_HOME" run submit --task 'bad/id' -- "$STUB"
  expect_denied "a submit with an unsafe task id" 1 submit invalid-task-id
  owner_run "$FX_ROOT" "$FX_HOME" run cancel --task 'bad/id' --run run-1 -- "$STUB"
  expect_denied "a cancel with an unsafe task id" 1 cancel invalid-task-id
  owner_run "$FX_ROOT" "$FX_HOME" run status --run 'bad/ref' -- "$STUB"
  expect_denied "a status with a malformed run" 1 status invalid-run-binding
  owner_run "$FX_ROOT" "$FX_HOME" run cancel --task task-x --run 'bad/ref' -- "$STUB"
  expect_denied "a cancel with a malformed run" 1 cancel invalid-run-binding
  pass "unsafe task ids and malformed run references are refused before the client runs"
}

test_foreign_or_unknown_run_is_unbound() {
  make_home unbound
  owner_run "$FX_ROOT" "$FX_HOME" run status --run run-1 -- "$STUB"
  expect_denied "an unknown run status" 1 status run-binding-unavailable
  owner_run "$FX_ROOT" "$FX_HOME" run logs --run run-1 -- "$STUB"
  expect_denied "an unknown run logs" 1 logs run-binding-unavailable
  owner_run "$FX_ROOT" "$FX_HOME" run cancel --task task-x --run run-1 -- "$STUB"
  expect_denied "an unknown run cancel" 1 cancel run-binding-unavailable
  owner_run "$FX_ROOT" "$FX_HOME" run fetch --task task-x --run run-1 -- "$STUB"
  expect_denied "an unknown run fetch" 1 fetch run-binding-unavailable
  pass "an unknown run is refused as unbound for status, logs, cancel, and fetch"
}

test_lease_held_by_the_other_actor_refuses_cancel() {
  make_home lease
  printf '%s\n' "$$" > "$FX_HOME/state/.lock"
  printf 'branch\t%s\t%s\n' "$$" "$(date +%s)" > "$FX_HOME/state/.lease-task-x"
  owner_run "$FX_ROOT" "$FX_HOME" run cancel --task task-x --run run-1 -- "$STUB"
  [ "$RUN_STATUS" -eq 6 ] || fail "cancel under a branch lease exited $RUN_STATUS, not 6: $RUN_ERR"
  assert_contains "$RUN_ERR" "leased to the branch" "the lease refusal lost its reason"
  [ ! -e "$RAN_LOG" ] || fail "cancel under a branch lease ran the client"
  pass "cancel is refused by a live lease held by the other supervision actor"
}

# --- submit admission (AC4) --------------------------------------------------

test_attended_main_submit_runs_under_the_lock_and_releases_it() {
  make_home attended
  STUB_STATUS=7 owner_run "$FX_ROOT" "$FX_HOME" run submit --task task-new -- "$STUB"
  [ "$RUN_STATUS" -eq 7 ] || fail "attended submit did not propagate the client's status 7: $RUN_STATUS $RUN_ERR"
  [ "$RUN_OUT" = stub-out ] || fail "attended submit stdout was not the client's output"
  [ -f "$RAN_LOG" ] || fail "attended submit did not run the client"
  [ ! -e "$FX_HOME/state/.task-set.lock" ] || fail "the task-set lock outlived the submit"
  pass "an attended main submit runs the client, propagates its status, and releases the lock"
}

test_busy_task_set_lock_refuses_without_waiting() {
  make_home busy
  hold_task_set_lock "$FX_HOME/state"
  owner_run "$FX_ROOT" "$FX_HOME" run submit --task task-new -- "$STUB"
  kill "$HOLDER_PID" 2>/dev/null || true
  wait "$HOLDER_PID" 2>/dev/null || true
  expect_denied "a submit under a held task-set lock" 1 submit admission-busy
  pass "a submit refuses rather than waiting when another operation holds the task-set lock"
}

test_spend_cap_counts_local_ordinary_records_and_fails_closed_on_the_cloud_term() {
  make_home cap
  enter_away "$FX_HOME" 2
  printf 'kind=ordinary\n' > "$FX_HOME/state/a.meta"
  printf 'kind=secondmate\n' > "$FX_HOME/state/b.meta"
  owner_run "$FX_ROOT" "$FX_HOME" run submit --task task-new -- "$STUB"
  expect_denied "a submit below the cap with a secondmate record" 1 submit cloud-count-unavailable
  assert_contains "$RUN_ERR" "no cloud assignment source" "the cloud refusal lost its reason"

  printf 'kind=ordinary\n' > "$FX_HOME/state/c.meta"
  owner_run "$FX_ROOT" "$FX_HOME" run submit --task task-new -- "$STUB"
  expect_denied "a submit at the cap" 1 submit cap-reached
  assert_contains "$RUN_ERR" "caps concurrent workers at 2 and 2 ordinary task(s) are live" \
    "the cap refusal lost its count"
  pass "the spend cap counts local ordinary records, refuses at the cap, and fails closed on the cloud term"
}

test_branch_submit_under_the_away_record_needs_queued_work() {
  make_home queue
  enter_away "$FX_HOME" 2
  FM_SUPERVISION_ACTOR=branch owner_run "$FX_ROOT" "$FX_HOME" run submit --task task-inflight -- "$STUB"
  expect_denied "a branch submit of in-flight work" 1 submit queue-not-dispatchable
  FM_SUPERVISION_ACTOR=branch owner_run "$FX_ROOT" "$FX_HOME" run submit --task task-arbitrary -- "$STUB"
  expect_denied "a branch submit of an arbitrary id" 1 submit queue-not-dispatchable

  FM_SUPERVISION_ACTOR=branch owner_run "$FX_ROOT" "$FX_HOME" run submit --task task-queued -- "$STUB"
  expect_denied "a branch submit of queued work still fails closed on the cloud term" 1 submit cloud-count-unavailable
  pass "the branch's queued-work entitlement is enforced, and queued work still fails closed on the cloud term"
}

test_malformed_record_refuses_mutations_but_not_observation() {
  make_home malformed
  printf 'this is not a contract\n' > "$FX_HOME/state/.afk-contract"
  owner_run "$FX_ROOT" "$FX_HOME" run submit --task task-new -- "$STUB"
  expect_denied "a submit under a malformed record" 1 submit malformed-context
  owner_run "$FX_ROOT" "$FX_HOME" run cancel --task task-x --run run-1 -- "$STUB"
  expect_denied "a cancel under a malformed record" 1 cancel malformed-context
  owner_run "$FX_ROOT" "$FX_HOME" run doctor -- "$STUB"
  [ "$RUN_STATUS" -eq 0 ] || fail "doctor under a malformed record exited $RUN_STATUS"
  pass "a malformed away record refuses mutations and leaves observation alone"
}

test_attended_branch_stays_refused_without_the_record() {
  make_home unrelated
  FM_SUPERVISION_ACTOR=branch owner_run "$FX_ROOT" "$FX_HOME" run submit --task task-queued -- "$STUB"
  [ "$RUN_STATUS" -eq 6 ] || fail "a branch submit with no record exited $RUN_STATUS, not 6"
  assert_contains "$RUN_ERR" "never performs this action" "a no-record branch lost the partition reason"
  pass "without the away record the branch never relocates into a submit"
}

test_doctor_runs_with_clean_output_and_no_prompt
test_unknown_action_is_an_unsupported_denial
test_parse_errors_exit_two
test_unmanaged_roots_are_refused_before_the_client_runs
test_gate_refuses_mutations_and_keeps_doctor
test_gate_worktree_path_refuses_without_the_marker
test_attended_branch_is_refused_for_every_mutation
test_invalid_ids_and_malformed_run_refuse
test_foreign_or_unknown_run_is_unbound
test_lease_held_by_the_other_actor_refuses_cancel
test_attended_main_submit_runs_under_the_lock_and_releases_it
test_busy_task_set_lock_refuses_without_waiting
test_spend_cap_counts_local_ordinary_records_and_fails_closed_on_the_cloud_term
test_branch_submit_under_the_away_record_needs_queued_work
test_malformed_record_refuses_mutations_but_not_observation
test_attended_branch_stays_refused_without_the_record
