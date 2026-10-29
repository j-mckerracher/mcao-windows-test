#!/usr/bin/env bash
# Behavior tests for the awaiting-captain block: the sidecar registry of
# non-decision items, decisions derived from captain holds, rendering, the
# acknowledgement and deferral lifecycle, and the turn-end check. Every case
# drives bin/fm-awaiting-captain.sh or bin/fm-captain-hold.sh through their
# command lines against an isolated home.
set -u

# shellcheck source=tests/lib.sh
# shellcheck disable=SC1091
. "$(dirname "${BASH_SOURCE[0]}")/lib.sh"

AWAIT="$ROOT/bin/fm-awaiting-captain.sh"
HOLD="$ROOT/bin/fm-captain-hold.sh"
TMP_ROOT=$(fm_test_tmproot fm-awaiting-captain)
T1=2026-10-20T09:00:00Z
T2=2026-10-20T10:30:00Z

command -v jq >/dev/null 2>&1 || { echo "skip: jq not found"; exit 0; }
command -v python3 >/dev/null 2>&1 || { echo "skip: python3 not found"; exit 0; }
HAVE_TASKS_AXI=0
command -v tasks-axi >/dev/null 2>&1 && HAVE_TASKS_AXI=1

# make_home <name>: an empty home with no backlog file. Registry-only cases
# use it, so they never touch tasks-axi.
make_home() {
  local home="$TMP_ROOT/$1"
  mkdir -p "$home/data" "$home/state" "$home/config" "$home/projects"
  printf '%s\n' "$home"
}

# make_backlog_home <name> [queued-line...]: a home with a markdown backlog
# whose Queued section holds the given lines.
make_backlog_home() {
  local home line
  home=$(make_home "$1")
  fm_test_tasks_toml "$home/.tasks.toml"
  {
    printf '## In flight\n\n## Queued\n'
    shift
    for line in "$@"; do
      printf '%s\n' "$line"
    done
    printf '\n## Done\n'
  } > "$home/data/backlog.md"
  printf '%s\n' "$home"
}

run_await() {  # <home> <clock> <args...>
  local home=$1 clock=$2
  shift 2
  FM_HOME="$home" FM_ROOT_OVERRIDE="$ROOT" FM_STATE_OVERRIDE="$home/state" \
    FM_DATA_OVERRIDE="$home/data" FM_CONFIG_OVERRIDE="$home/config" \
    FM_AWAITING_NOW="$clock" bash "$AWAIT" "$@"
}

run_hold() {  # <home> <clock> <args...>
  local home=$1 clock=$2
  shift 2
  FM_HOME="$home" FM_ROOT_OVERRIDE="$ROOT" FM_STATE_OVERRIDE="$home/state" \
    FM_DATA_OVERRIDE="$home/data" FM_CONFIG_OVERRIDE="$home/config" \
    FM_CAPTAIN_HOLD_NOW="$clock" bash "$HOLD" "$@"
}

assert_rc() {  # <expected> <actual> <label>
  [ "$1" = "$2" ] || fail "$3: expected exit $1, got $2"
}

assert_same_bytes() {  # <file> <copy> <label>
  cmp -s "$1" "$2" || fail "$3: file bytes changed"
}

registry_of() {  # <home>
  printf '%s\n' "$1/data/awaiting-captain.json"
}

# --- registry behavior: no tasks-axi needed -----------------------------------

test_empty_registry_renders_nothing_and_add_lists_record() {
  local home out rc id json
  home=$(make_home empty)
  out=$(run_await "$home" "$T1" render 2>"$home/err"); rc=$?
  assert_rc 0 "$rc" "render with no registry"
  [ -z "$out" ] || fail "render with no registry printed: $out"

  id=$(run_await "$home" "$T1" add --kind pr-ready --summary "PR ready for review" \
    --url https://github.com/o/r/pull/1) || fail "valid pr-ready add was refused"
  [ -n "$id" ] || fail "add printed no id"
  json=$(run_await "$home" "$T1" list --json) || fail "list --json failed"
  printf '%s' "$json" | jq -e --arg id "$id" '
    length == 1 and .[0].id == $id and .[0].kind == "pr-ready"
    and .[0].state == "open" and .[0].url == "https://github.com/o/r/pull/1"
    and .[0].acknowledged_by == null
    and (.[0].raised_at | test("^[0-9]{4}-[0-9]{2}-[0-9]{2}T[0-9]{2}:[0-9]{2}:[0-9]{2}Z$"))' >/dev/null \
    || fail "list --json record fields are wrong: $json"
  pass "empty registry renders nothing; add creates one open pr-ready record"
}

test_invalid_adds_are_refused_and_leave_registry_unchanged() {
  local home reg long rc args
  home=$(make_home invalid)
  reg=$(registry_of "$home")
  long=$(printf '%161s' '' | tr ' ' a)

  run_await "$home" "$T1" add --kind pr-ready --summary "PR-ready" >/dev/null 2>&1; rc=$?
  assert_rc 2 "$rc" "pr-ready without --url"
  [ ! -e "$reg" ] || fail "refused add created a registry"

  run_await "$home" "$T1" add --kind blocker --summary "Seed-record" >/dev/null || fail "seed add failed"
  cp "$reg" "$home/before.json"

  for args in \
    "add --kind pr-ready --summary PR-ready --url http://example.com/x" \
    "add --kind unknown --summary x" \
    "add --kind blocker --summary $long" \
    "add --kind decision --summary Pick-retry-policy"; do
    # shellcheck disable=SC2086
    run_await "$home" "$T1" $args >/dev/null 2>&1; rc=$?
    assert_rc 2 "$rc" "invalid add: $args"
    assert_same_bytes "$reg" "$home/before.json" "invalid add: $args"
  done

  run_await "$home" "$T1" add --kind blocker --summary "$(printf 'line one\nline two')" >/dev/null 2>&1; rc=$?
  assert_rc 2 "$rc" "multi-line summary"
  assert_same_bytes "$reg" "$home/before.json" "multi-line summary"
  pass "invalid adds exit 2 and leave the registry byte-identical"
}

test_render_orders_by_raise_time_and_numbers_stably() {
  local home first second expected
  home=$(make_home order)
  run_await "$home" 2026-10-19T09:00:00Z add --kind blocker --summary "Beta item" >/dev/null || fail "add beta"
  run_await "$home" 2026-10-18T09:00:00Z add --kind blocker --summary "Alpha item" >/dev/null || fail "add alpha"
  expected=$(printf '%s\n' \
    "Still waiting on you:" \
    "1. [blocker] Alpha item  (since 2026-10-18)" \
    "2. [blocker] Beta item  (since 2026-10-19)")
  first=$(run_await "$home" "$T2" render) || fail "first render failed"
  second=$(run_await "$home" "$T2" render) || fail "second render failed"
  [ "$first" = "$expected" ] || fail "render order was wrong: $first"
  [ "$second" = "$first" ] || fail "second render in the same turn differed: $second"
  pass "render lists the earlier raise first and repeats numbering on a second render"
}

test_render_stamps_last_shown_and_prints_full_url() {
  local home out json
  home=$(make_home stamp)
  run_await "$home" "$T1" add --kind pr-ready --summary "PR ready for review" \
    --url https://github.com/o/r/pull/7 >/dev/null || fail "add pr-ready"
  out=$(run_await "$home" "$T2" render) || fail "render failed"
  printf '%s\n' "$out" | grep -qxF \
    "1. [pr-ready] PR ready for review https://github.com/o/r/pull/7  (since 2026-10-20)" \
    || fail "pr-ready line lacks the full URL or the since date: $out"
  json=$(run_await "$home" "$T2" list --json) || fail "list failed"
  printf '%s' "$json" | jq -e --arg now "$T2" '.[0].last_shown_at == $now' >/dev/null \
    || fail "last_shown_at was not stamped at render time: $json"
  pass "render prints the full URL and stamps last_shown_at"
}

test_add_dedupes_on_kind_and_url() {
  local home first second count
  home=$(make_home dedupe)
  first=$(run_await "$home" "$T1" add --kind pr-ready --summary "PR ready" \
    --url https://github.com/o/r/pull/3) || fail "first add failed"
  second=$(run_await "$home" "$T2" add --kind pr-ready --summary "PR ready again" \
    --url https://github.com/o/r/pull/3) || fail "second add failed"
  [ "$first" = "$second" ] || fail "re-recording the same PR made a new id: $first vs $second"
  count=$(run_await "$home" "$T2" list --json | jq 'length') || fail "list failed"
  [ "$count" = 1 ] || fail "dedupe left $count records"
  pass "re-recording the same kind and URL returns the existing id"
}

test_ack_variants_and_reopen() {
  local home reg id_noted id_defer rc out args
  home=$(make_home ack)
  reg=$(registry_of "$home")
  id_noted=$(run_await "$home" "$T1" add --kind blocker --summary "Noted item") || fail "add noted"
  id_defer=$(run_await "$home" "$T1" add --kind blocker --summary "Deferred item") || fail "add deferred"

  run_await "$home" "$T1" ack "$id_noted" --by noted >/dev/null || fail "ack noted refused"
  jq -e --arg id "$id_noted" '.records[] | select(.id == $id)
    | .state == "acknowledged" and .acknowledged_by == "noted"' "$reg" >/dev/null \
    || fail "noted record is not acknowledged"

  run_await "$home" "$T1" ack "$id_defer" --by defer --until 2026-10-21 >/dev/null \
    || fail "defer to tomorrow refused"
  out=$(run_await "$home" "$T1" render) || fail "render failed"
  [ -z "$out" ] || fail "render listed noted or deferred items: $out"
  run_await "$home" "$T1" list --json | jq -e --arg id "$id_defer" \
    '.[] | select(.id == $id) | .state == "deferred" and .deferred_until == "2026-10-21"' >/dev/null \
    || fail "deferred record did not keep its date"

  run_await "$home" "$T1" reopen "$id_noted" >/dev/null || fail "reopen refused"
  out=$(run_await "$home" "$T1" render) || fail "render after reopen failed"
  printf '%s\n' "$out" | grep -qF "Noted item" || fail "reopened item was not listed: $out"

  for args in \
    "ack $id_defer --by defer --until 2026-10-19" \
    "ack $id_defer --by defer --until 2026-10-20" \
    "ack $id_defer --by defer" \
    "ack $id_noted --by ok" \
    "ack ac-unknown000 --by noted"; do
    # shellcheck disable=SC2086
    run_await "$home" "$T1" $args >/dev/null 2>&1; rc=$?
    assert_rc 2 "$rc" "refused ack: $args"
  done

  run_await "$home" "$T1" ack "$id_noted" --by answer >/dev/null || fail "first answer refused"
  run_await "$home" "$T1" ack "$id_noted" --by answer >/dev/null 2>&1; rc=$?
  assert_rc 2 "$rc" "second ack of an acknowledged record"
  pass "ack variants, deferral date rules, reopen, and refusals behave as specified"
}

test_deferred_item_reopens_on_its_date() {
  local home id out
  home=$(make_home defer-date)
  id=$(run_await "$home" "$T1" add --kind blocker --summary "Come back later") || fail "add failed"
  run_await "$home" "$T1" ack "$id" --by defer --until 2026-10-25 >/dev/null || fail "defer refused"

  out=$(run_await "$home" 2026-10-24T23:59:00Z render) || fail "render before the date failed"
  [ -z "$out" ] || fail "deferred item listed before its date: $out"

  out=$(run_await "$home" 2026-10-25T00:00:00Z render) || fail "render on the date failed"
  printf '%s\n' "$out" | grep -qF "Come back later" || fail "deferred item not listed on its date: $out"
  run_await "$home" 2026-10-25T00:00:00Z list --json | jq -e --arg id "$id" \
    '.[] | select(.id == $id) | .state == "open"' >/dev/null \
    || fail "item on its date is not open"
  pass "a deferred item stays hidden until its date, then reopens on that date"
}

test_corrupt_registry_exits_3_and_is_left_alone() {
  local home reg rc err args
  home=$(make_home corrupt)
  reg=$(registry_of "$home")
  printf '{"version":1,' > "$reg"
  cp "$reg" "$home/before.json"

  for args in "list" "render" "add --kind blocker --summary x" "ack ac-nothing --by noted"; do
    # shellcheck disable=SC2086
    err=$(run_await "$home" "$T1" $args 2>&1 >/dev/null); rc=$?
    assert_rc 3 "$rc" "corrupt registry: $args"
    printf '%s' "$err" | grep -qF "$reg" || fail "corrupt-registry error did not name $reg: $err"
    assert_same_bytes "$reg" "$home/before.json" "corrupt registry: $args"
  done
  pass "a corrupt registry exits 3, names the file, and keeps its bytes"
}

# --- turn-end check (the Pi guard's entry point) ------------------------------

test_check_prepends_block_only_when_needed() {
  local home id out rc
  home=$(make_home check)
  id=$(run_await "$home" "$T1" add --kind pr-ready --summary "PR ready for review" \
    --url https://github.com/o/r/pull/9) || fail "add pr-ready"

  out=$(printf 'The fix is finished.\n' | run_await "$home" "$T1" check); rc=$?
  assert_rc 2 "$rc" "check on a message without the block"
  printf '%s\n' "$out" | grep -qxF "Still waiting on you:" || fail "check output lacks the block header"
  printf '%s\n' "$out" | grep -qF "https://github.com/o/r/pull/9" || fail "check output lacks the item"

  out=$(printf 'Still waiting on you:\n1. [pr-ready] PR ready\n\nThe fix is finished.\n' \
    | run_await "$home" "$T1" check); rc=$?
  assert_rc 0 "$rc" "check on a message that already begins with the block"
  [ -z "$out" ] || fail "check reprinted a block that was already present: $out"

  run_await "$home" "$T1" ack "$id" --by noted >/dev/null || fail "ack failed"
  out=$(printf 'Nothing waiting.\n' | run_await "$home" "$T1" check); rc=$?
  assert_rc 0 "$rc" "check with nothing open"
  [ -z "$out" ] || fail "check printed a block with nothing open: $out"
  pass "check prepends the block only when items are open and the message lacks it"
}

# --- decisions derived from captain holds (needs tasks-axi) -------------------

test_holds_drive_decisions_for_this_home_only() {
  local home other out answer_file
  if [ "$HAVE_TASKS_AXI" != 1 ]; then
    printf 'skip: decision tests need tasks-axi, which is not on PATH\n'
    return 0
  fi
  home=$(make_backlog_home holds '- [ ] task-beta - Beta decision (repo: x) (kind: captain) (since 2026-10-18)')
  other=$(make_backlog_home holds-other)
  run_hold "$home" 2026-10-19T21:00:00Z hold task-beta --reason "Need call on retry policy" >/dev/null \
    || fail "captain hold refused"

  out=$(run_await "$home" "$T1" render) || fail "render with a decision failed"
  printf '%s\n' "$out" | grep -qxF \
    "1. [decision] Need call on retry policy (task-beta)  (since 2026-10-19)" \
    || fail "decision line is wrong: $out"

  out=$(run_await "$other" "$T1" render) || fail "other home render failed"
  [ -z "$out" ] || fail "another home listed this home's decision: $out"

  answer_file="$home/answer.txt"
  printf 'Use the fixed retry policy.\n' > "$answer_file"
  run_hold "$home" "$T2" answer task-beta --decision-file "$answer_file" >/dev/null \
    || fail "captain answer refused"
  out=$(run_await "$home" "$T2" render) || fail "render after answer failed"
  [ -z "$out" ] || fail "answered decision still listed: $out"
  pass "a captain hold appears as a decision in its own home, and answering it clears the block"
}

test_dated_hold_is_deferred_until_its_date() {
  local home out
  if [ "$HAVE_TASKS_AXI" != 1 ]; then
    printf 'skip: dated-hold test needs tasks-axi, which is not on PATH\n'
    return 0
  fi
  home=$(make_backlog_home dated '- [ ] task-gamma - Gamma (repo: x) (kind: ship) (since 2026-10-18)')
  run_hold "$home" 2026-10-19T21:00:00Z hold task-gamma --reason "Dated call" --until 2026-10-25 >/dev/null \
    || fail "dated hold refused"

  out=$(run_await "$home" "$T1" render) || fail "render before the date failed"
  [ -z "$out" ] || fail "dated hold listed before its date: $out"

  out=$(run_await "$home" 2026-10-25T00:00:00Z render) || fail "render on the date failed"
  printf '%s\n' "$out" | grep -qF "Dated call (task-gamma)" \
    || fail "dated hold not listed on its date: $out"
  pass "a dated hold stays out of the block until its date"
}

test_noted_decision_stays_hidden_until_reopened() {
  local home out
  if [ "$HAVE_TASKS_AXI" != 1 ]; then
    printf 'skip: noted-decision test needs tasks-axi, which is not on PATH\n'
    return 0
  fi
  home=$(make_backlog_home noted '- [ ] task-delta - Delta (repo: x) (kind: ship) (since 2026-10-18)')
  run_hold "$home" 2026-10-19T21:00:00Z hold task-delta --reason "Weigh the trade-off" >/dev/null \
    || fail "hold refused"
  run_await "$home" "$T1" ack task-delta --by noted >/dev/null || fail "noted ack refused"
  out=$(run_await "$home" "$T1" render) || fail "render after noted failed"
  [ -z "$out" ] || fail "noted decision still listed: $out"

  run_await "$home" "$T1" reopen task-delta >/dev/null || fail "reopen refused"
  out=$(run_await "$home" "$T1" render) || fail "render after reopen failed"
  printf '%s\n' "$out" | grep -qF "Weigh the trade-off (task-delta)" \
    || fail "reopened decision not listed: $out"
  pass "a noted decision stays hidden until reopened"
}

test_decision_cannot_be_added_or_deferred() {
  local home rc
  if [ "$HAVE_TASKS_AXI" != 1 ]; then
    printf 'skip: decision-refusal test needs tasks-axi, which is not on PATH\n'
    return 0
  fi
  home=$(make_backlog_home refuse '- [ ] task-eps - Eps (repo: x) (kind: ship) (since 2026-10-18)')
  run_hold "$home" 2026-10-19T21:00:00Z hold task-eps --reason "Pick one" >/dev/null || fail "hold refused"

  run_await "$home" "$T1" add --kind decision --summary "Pick one" >/dev/null 2>&1; rc=$?
  assert_rc 2 "$rc" "add --kind decision"
  run_await "$home" "$T1" ack task-eps --by defer --until 2026-10-25 >/dev/null 2>&1; rc=$?
  assert_rc 2 "$rc" "defer of a decision through ack"
  pass "a decision is refused by add and by defer, so its hold stays the single owner"
}

test_non_asking_classes_stay_out_of_the_block() {
  local home out
  if [ "$HAVE_TASKS_AXI" != 1 ]; then
    printf 'skip: class test needs tasks-axi, which is not on PATH\n'
    return 0
  fi
  home=$(make_backlog_home classes \
    '- [ ] task-wait - Waits on upstream (repo: x) (kind: ship) (since 2026-10-18)' \
    '- [ ] task-book - Answer already recorded (repo: x) (kind: ship) (since 2026-10-18)' \
    '- [ ] task-ask - Real route call (repo: x) (kind: ship) (since 2026-10-18)')
  run_hold "$home" 2026-10-19T21:00:00Z hold task-wait --reason "Waiting on upstream" \
    --class dependency-wait >/dev/null || fail "dependency-wait hold refused"
  run_hold "$home" 2026-10-19T21:00:00Z hold task-book --reason "Answer recorded" \
    --class answered-bookkeeping >/dev/null || fail "answered-bookkeeping hold refused"
  run_hold "$home" 2026-10-19T21:00:00Z hold task-ask --reason "Pick a route" >/dev/null \
    || fail "decision hold refused"

  out=$(run_await "$home" "$T1" render) || fail "render with mixed classes failed"
  printf '%s\n' "$out" | grep -qF "Pick a route (task-ask)" \
    || fail "decision hold missing from the block: $out"
  if printf '%s\n' "$out" | grep -qF "task-wait" || printf '%s\n' "$out" | grep -qF "task-book"; then
    fail "a non-asking hold appeared in the block: $out"
  fi
  pass "dependency-wait and answered-bookkeeping holds stay out of the block"
}

test_ack_of_unknown_id_is_refused_without_backlog() {
  local home rc
  home=$(make_home noaxi)
  run_await "$home" "$T1" ack ac-missing --by noted >/dev/null 2>&1; rc=$?
  assert_rc 2 "$rc" "ack of an unknown id with no backlog"
  pass "ack of an unknown id is refused even with no backlog to read"
}
test_raise_monitoring_keeps_one_open_record_and_refreshes_summary() {
  local home id1 id2 json
  home=$(make_home raise-once)
  id1=$(run_await "$home" "$T1" raise-monitoring --summary "Monitoring down 8m 23s with 4 tasks in flight") \
    || fail "first raise-monitoring failed"
  id2=$(run_await "$home" "$T2" raise-monitoring --summary "Monitoring down 9m 0s with 4 tasks in flight") \
    || fail "second raise-monitoring failed"
  assert_equals "$id1" "$id2" "a repeat raise in the same outage reuses its record"
  json=$(run_await "$home" "$T2" list --json) || fail "list --json failed"
  assert_equals "1" "$(printf '%s' "$json" | jq 'length')" "one open record per outage"
  printf '%s' "$json" | jq -e --arg id "$id1" '
    .[0] | .id == $id and .kind == "monitoring-down" and .state == "open"
      and .summary == "Monitoring down 9m 0s with 4 tasks in flight"' >/dev/null \
    || fail "refresh did not update the open record's summary: $json"
  pass "raise-monitoring keeps one open record per outage and refreshes its summary"
}

test_restore_monitoring_acknowledges_as_noted_and_is_idempotent() {
  local home id acked_first acked_again
  home=$(make_home restore-once)
  id=$(run_await "$home" "$T1" raise-monitoring --summary "Monitoring down 8m 23s with 4 tasks in flight") \
    || fail "raise-monitoring failed"
  run_await "$home" "$T2" restore-monitoring || fail "restore-monitoring failed"
  jq -e --arg id "$id" '.records[] | select(.id == $id)
    | .state == "acknowledged" and .acknowledged_by == "noted"' "$home/data/awaiting-captain.json" >/dev/null \
    || fail "restore did not acknowledge the open record as noted"
  acked_first=$(jq -r --arg id "$id" '.records[] | select(.id == $id) | .acknowledged_at' "$home/data/awaiting-captain.json")
  run_await "$home" "$T2" restore-monitoring || fail "second restore-monitoring failed"
  acked_again=$(jq -r --arg id "$id" '.records[] | select(.id == $id) | .acknowledged_at' "$home/data/awaiting-captain.json")
  assert_equals "$acked_first" "$acked_again" "a repeat restore leaves the acknowledgement untouched"
  pass "restore-monitoring acknowledges the open record as noted, once"
}

test_restore_monitoring_without_open_record_creates_nothing() {
  local home
  home=$(make_home restore-none)
  run_await "$home" "$T1" restore-monitoring || fail "restore with no open record failed"
  assert_absent "$home/data/awaiting-captain.json" "a no-op restore must not create a registry file"
  pass "restore-monitoring with no open record is a silent no-op"
}

test_raise_monitoring_after_restore_opens_a_new_record() {
  local home id1 id2 total open
  home=$(make_home raise-again)
  id1=$(run_await "$home" "$T1" raise-monitoring --summary "Monitoring down 8m 23s with 4 tasks in flight") \
    || fail "first raise failed"
  run_await "$home" "$T1" restore-monitoring || fail "restore failed"
  id2=$(run_await "$home" "$T2" raise-monitoring --summary "Monitoring down 5m 0s with 1 task in flight") \
    || fail "raise after restore failed"
  assert_not_equals "$id1" "$id2" "a new outage after a restore gets its own record"
  total=$(jq '[.records[] | select(.kind == "monitoring-down")] | length' "$home/data/awaiting-captain.json")
  open=$(jq '[.records[] | select(.kind == "monitoring-down" and .state == "open")] | length' "$home/data/awaiting-captain.json")
  assert_equals "2" "$total" "the acknowledged outage stays on record"
  assert_equals "1" "$open" "exactly one open record for the new outage"
  pass "raise-monitoring after a restore opens a new record for a new outage"
}

test_raise_monitoring_refuses_bad_summaries_and_writes_nothing() {
  local home long exact rc
  home=$(make_home raise-bad)
  run_await "$home" "$T1" raise-monitoring --summary "" >/dev/null 2>&1; rc=$?
  assert_rc 2 "$rc" "an empty summary"
  run_await "$home" "$T1" raise-monitoring --summary "$(printf 'one\ntwo')" >/dev/null 2>&1; rc=$?
  assert_rc 2 "$rc" "a multi-line summary"
  long=$(printf '%161s' '' | tr ' ' x)
  run_await "$home" "$T1" raise-monitoring --summary "$long" >/dev/null 2>&1; rc=$?
  assert_rc 2 "$rc" "a 161-character summary"
  assert_absent "$home/data/awaiting-captain.json" "refused summaries must not create the registry"
  exact=$(printf '%160s' '' | tr ' ' x)
  run_await "$home" "$T1" raise-monitoring --summary "$exact" >/dev/null || fail "a 160-character summary must be accepted"
  pass "raise-monitoring refuses empty, multi-line, and over-limit summaries"
}

test_empty_registry_renders_nothing_and_add_lists_record
test_invalid_adds_are_refused_and_leave_registry_unchanged
test_render_orders_by_raise_time_and_numbers_stably
test_render_stamps_last_shown_and_prints_full_url
test_add_dedupes_on_kind_and_url
test_ack_variants_and_reopen
test_deferred_item_reopens_on_its_date
test_corrupt_registry_exits_3_and_is_left_alone
test_check_prepends_block_only_when_needed
test_holds_drive_decisions_for_this_home_only
test_dated_hold_is_deferred_until_its_date
test_noted_decision_stays_hidden_until_reopened
test_decision_cannot_be_added_or_deferred
test_non_asking_classes_stay_out_of_the_block
test_ack_of_unknown_id_is_refused_without_backlog
test_raise_monitoring_keeps_one_open_record_and_refreshes_summary
test_restore_monitoring_acknowledges_as_noted_and_is_idempotent
test_restore_monitoring_without_open_record_creates_nothing
test_raise_monitoring_after_restore_opens_a_new_record
test_raise_monitoring_refuses_bad_summaries_and_writes_nothing
