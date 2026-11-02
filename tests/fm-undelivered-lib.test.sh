#!/usr/bin/env bash
# tests/fm-undelivered-lib.test.sh - behavior tests for the undelivered-decision
# ledger (DPES-855): bin/fm-undelivered-lib.sh's entry points and the
# bin/fm-undelivered.sh mark-delivered CLI. Every assertion goes through the
# printed section, the delivery entry points, the CLI exit status, or the
# documented ledger fields in the lib header's CONTRACT. The integration file
# tests/fm-wake-drain-undelivered.test.sh covers the same ledger through the
# real drain and fm-send.
set -u

# shellcheck source=tests/lib.sh
. "$(dirname "${BASH_SOURCE[0]}")/lib.sh"

TMP_ROOT=$(fm_test_tmproot fm-undelivered-lib-tests)
CLI="$ROOT/bin/fm-undelivered.sh"
NOW=$(date +%s)

# lib_run <fn> [args...]: run one ledger entry point with the production
# libraries sourced first, in the same order the drain and CLI source them.
lib_run() {
  bash -c '
    BIN=$1; shift
    . "$BIN/fm-wake-lib.sh"
    . "$BIN/fm-classify-lib.sh"
    . "$BIN/fm-line-cap-lib.sh"
    . "$BIN/fm-timeout-lib.sh"
    . "$BIN/fm-undelivered-lib.sh"
    "$@"
  ' _ "$ROOT/bin" "$@"
}

new_state() {  # <name> -> echoes a fresh state dir
  local dir="$TMP_ROOT/$1/state"
  mkdir -p "$dir"
  printf '%s\n' "$dir"
}

open_row() {  # <task> <key> <verb> <note> -> one OPEN DECISIONS fold row
  printf '%s\t%s\t%s\t%s\n' "$1" "$2" "$3" "$4"
}

ledger_rows() {  # <state> -> ledger JSON lines (empty when absent)
  cat "$1/.undelivered-decisions.jsonl" 2>/dev/null
}

test_raise_lists_decision_and_repeat_drain_does_not_duplicate_it() {
  local state out rows raised
  state=$(new_state raise)
  printf 'needs-decision [key=api-shape] [at=%s]: pick REST or RPC\n' "$((NOW - 120))" > "$state/task1.status"

  out=$(lib_run fm_undelivered_section "$state" "$(open_row task1 api-shape needs-decision 'pick REST or RPC')" 1 "$NOW") \
    || fail "the first section call should succeed"
  assert_contains "$out" 'UNDELIVERED DECISIONS (raised while not delivered to the captain; new):' \
    "a decision raised while undelivered should be listed"
  assert_contains "$out" 'task1 [key=api-shape] needs-decision: pick REST or RPC | raised ' \
    "the listing should name the task, key, verb, note, and raise time"
  assert_contains "$out" ' | age 0h2m' \
    "the age should be the presentation time minus the raise time"

  out=$(lib_run fm_undelivered_section "$state" "$(open_row task1 api-shape needs-decision 'pick REST or RPC')" 1 "$NOW") \
    || fail "the repeat section call should succeed"
  assert_contains "$out" 'task1 [key=api-shape]' "an undelivered decision should keep being listed on repeat drains"

  rows=$(ledger_rows "$state")
  assert_equals 1 "$(printf '%s\n' "$rows" | grep -c .)" "repeat drains should not add a second ledger record"
  raised=$(printf '%s\n' "$rows" | jq -r '.raised_at_epoch')
  assert_equals "$((NOW - 120))" "$raised" "the raise time should come from the status line's at= stamp"
  pass "a raised decision is listed on every drain without duplicate ledger records"
}

test_closure_requires_a_trusted_fold_and_the_same_status_log() {
  local state out
  state=$(new_state closure)
  printf 'needs-decision [key=api-shape] [at=%s]: pick REST or RPC\n' "$((NOW - 120))" > "$state/task1.status"
  lib_run fm_undelivered_section "$state" "$(open_row task1 api-shape needs-decision 'pick REST or RPC')" 1 "$NOW" >/dev/null

  out=$(lib_run fm_undelivered_section "$state" "" 0 "$NOW") || fail "an untrusted empty fold should still report"
  assert_contains "$out" 'task1 [key=api-shape]' "a failed or partial fold must never close an undelivered decision"

  mv "$state/task1.status" "$state/task1.status.rotated"
  printf 'working: a fresh log after rotation\n' > "$state/task1.status"
  out=$(lib_run fm_undelivered_section "$state" "" 1 "$NOW") || fail "a rotated status log should still report"
  assert_contains "$out" 'task1 [key=api-shape]' "a rotated status log must not close the decision it no longer holds"

  mv "$state/task1.status.rotated" "$state/task1.status"
  out=$(lib_run fm_undelivered_section "$state" "" 1 "$NOW") || fail "a trusted empty fold should report"
  assert_not_contains "$out" 'UNDELIVERED DECISIONS' \
    "a trusted fold that no longer lists an undelivered decision on its unchanged log should close it"
  assert_equals 'null' "$(ledger_rows "$state" | jq -r '.delivered_at_epoch')" \
    "closure without a delivery event must leave the decision undelivered in the record"
  assert_not_equals 'null' "$(ledger_rows "$state" | jq -r '.closed_at_epoch')" \
    "a closed decision should be marked closed in the record"
  pass "closure needs a trusted fold and the status log the decision was raised from"
}

test_delivery_clears_only_the_named_decision_and_is_idempotent() {
  local state out first
  state=$(new_state delivery)
  printf 'needs-decision [key=api-shape] [at=%s]: pick REST or RPC\n' "$((NOW - 120))" > "$state/task1.status"
  printf 'blocked [key=migration] [at=%s]: pick the rollout plan\n' "$((NOW - 60))" > "$state/task2.status"
  lib_run fm_undelivered_section "$state" "$(open_row task1 api-shape needs-decision 'pick REST or RPC')
$(open_row task2 migration blocked 'pick the rollout plan')" 1 "$NOW" >/dev/null

  lib_run fm_undelivered_mark_delivered "$state" task1 api-shape "$NOW" \
    || fail "marking a listed decision delivered should succeed"
  out=$(lib_run fm_undelivered_section "$state" "$(open_row task1 api-shape needs-decision 'pick REST or RPC')
$(open_row task2 migration blocked 'pick the rollout plan')" 1 "$NOW")
  assert_not_contains "$out" 'task1 [key=api-shape]' "a delivered decision must leave the undelivered listing"
  assert_contains "$out" 'task2 [key=migration] blocked: pick the rollout plan' \
    "delivering one decision must not clear a different undelivered decision"
  assert_not_contains "$out" 'DELIVERY FAILURE' "a delivered decision must not produce a delivery failure"

  first=$(ledger_rows "$state" | jq -r 'select(.key == "api-shape") | .delivered_at_epoch')
  assert_equals "$NOW" "$first" "delivery should record the time of the delivery event"
  lib_run fm_undelivered_mark_delivered "$state" task1 api-shape "$((NOW + 5))" \
    || fail "a repeated delivery mark should succeed"
  assert_equals "$NOW" "$(ledger_rows "$state" | jq -r 'select(.key == "api-shape") | .delivered_at_epoch')" \
    "a repeated delivery mark must keep the first delivery time"
  pass "a delivery event clears only its own decision and repeats without changing the record"
}

test_mark_on_an_absent_ledger_succeeds_and_creates_nothing() {
  local state
  state=$(new_state absent)
  lib_run fm_undelivered_mark_delivered "$state" task1 api-shape "$NOW" \
    || fail "marking delivery with no ledger should succeed"
  [ ! -e "$state/.undelivered-decisions.jsonl" ] || fail "marking delivery with no ledger must not create one"
  pass "delivery marks are idempotent on an absent ledger and create no record"
}

test_delivery_failure_appears_only_past_the_threshold() {
  local aged fresh out
  aged=$(new_state aged)
  printf 'needs-decision [key=api-shape] [at=%s]: pick REST or RPC\n' "$((NOW - 3600))" > "$aged/task1.status"
  lib_run fm_undelivered_section "$aged" "$(open_row task1 api-shape needs-decision 'pick REST or RPC')" 1 "$NOW" >/dev/null
  out=$(lib_run fm_undelivered_section "$aged" "$(open_row task1 api-shape needs-decision 'pick REST or RPC')" 1 "$NOW")
  assert_contains "$out" 'DELIVERY FAILURE: 1 decision(s) not delivered to the captain; oldest raised ' \
    "a decision older than the default threshold should be a delivery failure"
  assert_contains "$out" '(1h0m ago)' "the delivery failure should state the oldest raise age"

  fresh=$(new_state fresh)
  printf 'needs-decision [key=api-shape] [at=%s]: pick REST or RPC\n' "$((NOW - 120))" > "$fresh/task1.status"
  out=$(lib_run fm_undelivered_section "$fresh" "$(open_row task1 api-shape needs-decision 'pick REST or RPC')" 1 "$NOW")
  assert_not_contains "$out" 'DELIVERY FAILURE' "a two-minute-old decision is inside the default threshold"

  out=$(FM_UNDELIVERED_THRESHOLD=60 lib_run fm_undelivered_section "$fresh" \
    "$(open_row task1 api-shape needs-decision 'pick REST or RPC')" 1 "$NOW")
  assert_contains "$out" 'DELIVERY FAILURE: 1 decision(s) not delivered' \
    "FM_UNDELIVERED_THRESHOLD should lower the threshold"
  pass "a delivery failure is reported only once a decision exceeds the threshold"
}

test_unreadable_ledger_reports_failure_and_refuses_the_delivery_mark() {
  local state out rc
  state=$(new_state unreadable)
  # A directory in the ledger's place is unreadable as a record for every user,
  # including root, unlike a permission-stripped file.
  mkdir "$state/.undelivered-decisions.jsonl"
  out=$(lib_run fm_undelivered_section "$state" "$(open_row task1 api-shape needs-decision 'pick REST or RPC')" 1 "$NOW") \
    || fail "an unreadable ledger must not change the section's exit status"
  assert_contains "$out" 'DELIVERY FAILURE: undelivered-decision record unreadable; decisions may be undelivered' \
    "an unreadable ledger should surface as a delivery failure"

  lib_run fm_undelivered_mark_delivered "$state" task1 api-shape "$NOW"; rc=$?
  expect_code 1 "$rc" "a delivery mark against an unreadable ledger should report failure"
  pass "an unreadable ledger is reported as a delivery failure and refuses a delivery mark"
}

test_listing_is_capped_and_reports_the_omitted_count() {
  local state note out shown omitted total=15 i key
  state=$(new_state capped)
  printf -v note '%*s' 190 ''
  note=${note// /w}
  local rows=''
  for i in $(seq 1 "$total"); do
    key=$(printf 'k%02d' "$i")
    printf 'needs-decision [key=%s] [at=%s]: %s\n' "$key" "$((NOW - 60))" "$note" > "$state/task$i.status"
    rows+="$(open_row "task$i" "$key" needs-decision "$note")"$'\n'
  done
  out=$(lib_run fm_undelivered_section "$state" "$rows" 1 "$NOW") || fail "a capped listing should still succeed"
  shown=$(printf '%s\n' "$out" | grep -c '^task[0-9]* \[key=k')
  omitted=$(printf '%s\n' "$out" | tail -n 1 | sed -n 's/^UNDELIVERED DECISIONS: \([0-9]*\) more omitted (byte cap)$/\1/p')
  [ -n "$omitted" ] || fail "a listing over the byte cap should end with the omitted-count line: $(printf '%s\n' "$out" | tail -n 1)"
  [ "$shown" -ge 1 ] || fail "at least one decision should still be listed under the cap"
  assert_equals "$total" "$((shown + omitted))" "the listed and omitted decisions should account for every undelivered decision"
  pass "an over-cap listing shows what fits and reports the exact omitted count"
}

test_cli_usage_and_absent_ledger_exit_statuses() {
  local state rc out
  state=$(new_state cli)
  out=$(FM_ROOT_OVERRIDE="$ROOT" FM_STATE_OVERRIDE="$state" bash "$CLI" --help 2>&1); rc=$?
  expect_code 0 "$rc" "--help should exit 0"
  assert_contains "$out" 'mark-delivered <task> <key>' "--help should document the mark-delivered usage"

  FM_ROOT_OVERRIDE="$ROOT" FM_STATE_OVERRIDE="$state" bash "$CLI" >/dev/null 2>&1; rc=$?
  expect_code 2 "$rc" "running with no command should be a usage error"

  FM_ROOT_OVERRIDE="$ROOT" FM_STATE_OVERRIDE="$state" bash "$CLI" mark-delivered task1 >/dev/null 2>&1; rc=$?
  expect_code 2 "$rc" "mark-delivered without a key should be a usage error"

  FM_ROOT_OVERRIDE="$ROOT" FM_STATE_OVERRIDE="$state" bash "$CLI" mark-delivered 'a/b' api-shape >/dev/null 2>&1; rc=$?
  expect_code 2 "$rc" "a task name with a path separator should be a usage error"

  FM_ROOT_OVERRIDE="$ROOT" FM_STATE_OVERRIDE="$state" bash "$CLI" mark-delivered task1 api-shape >/dev/null 2>&1; rc=$?
  expect_code 0 "$rc" "mark-delivered with no ledger should succeed"
  [ ! -e "$state/.undelivered-decisions.jsonl" ] || fail "mark-delivered with no ledger must not create one"
  pass "the delivery CLI reports usage errors with exit 2 and succeeds idempotently with no ledger"
}

test_raise_lists_decision_and_repeat_drain_does_not_duplicate_it
test_closure_requires_a_trusted_fold_and_the_same_status_log
test_delivery_clears_only_the_named_decision_and_is_idempotent
test_mark_on_an_absent_ledger_succeeds_and_creates_nothing
test_delivery_failure_appears_only_past_the_threshold
test_unreadable_ledger_reports_failure_and_refuses_the_delivery_mark
test_listing_is_capped_and_reports_the_omitted_count
test_cli_usage_and_absent_ledger_exit_statuses
