#!/usr/bin/env bash
# tests/fm-wake-drain-undelivered.test.sh - end-to-end behavior tests for the
# undelivered-decision ledger (DPES-855). These drive the REAL bin/fm-wake-drain.sh
# across successive invocations, the REAL bin/fm-undelivered.sh CLI, and the REAL
# bin/fm-send.sh over a stubbed tmux, then assert the printed output, the exit
# status, and the documented ledger fields. tests/fm-undelivered-lib.test.sh covers
# the ledger functions and the CLI usage contract directly.
set -u

# shellcheck source=tests/lib.sh
. "$(dirname "${BASH_SOURCE[0]}")/lib.sh"

DRAIN="$ROOT/bin/fm-wake-drain.sh"
SEND="$ROOT/bin/fm-send.sh"
CLI="$ROOT/bin/fm-undelivered.sh"
LEDGER=.undelivered-decisions.jsonl

TMP_ROOT=$(fm_test_tmproot fm-wake-drain-undelivered)
NOW=$(date +%s)
RAISED=$((NOW - 300))

new_home() {  # <name> -> echoes a fresh home with state/, cfg/, and dat/
  local home="$TMP_ROOT/$1"
  mkdir -p "$home/state" "$home/cfg" "$home/dat"
  printf '%s\n' "$home"
}

iso_of() {  # <epoch> -> local ISO-8601 time, the form the section prints
  date -r "$1" '+%Y-%m-%dT%H:%M:%S' 2>/dev/null || date -d "@$1" '+%Y-%m-%dT%H:%M:%S'
}

drain_to() {  # <home> <label> [VAR=value ...]: run the real drain; output lands in <home>/<label>.out
  local home=$1 label=$2
  shift 2
  env -i HOME="$HOME" PATH="$PATH" FM_TEST_SEAM=1 FM_TEST_HARNESS=claude \
    FM_ROOT_OVERRIDE="$home" FM_HOME="$home" FM_STATE_OVERRIDE="$home/state" \
    FM_CONFIG_OVERRIDE="$home/cfg" FM_DATA_OVERRIDE="$home/dat" "$@" \
    bash "$DRAIN" < /dev/null > "$home/$label.out" 2> "$home/$label.err"
}

mark_delivered() {  # <home> <task> <key>: run the real delivery CLI
  env -i HOME="$HOME" PATH="$PATH" FM_ROOT_OVERRIDE="$1" FM_HOME="$1" \
    FM_STATE_OVERRIDE="$1/state" bash "$CLI" mark-delivered "$2" "$3"
}

ledger_count() {  # <home> -> number of ledger records
  jq -s 'length' "$1/state/$LEDGER" 2>/dev/null
}

ledger_field() {  # <home> <jq-filter> -> the filter's value for each record
  jq -r "$2" "$1/state/$LEDGER"
}

# Stub tmux and sleep for the fm-send leg. The composer reads as empty, so the
# doorbell path reaches a clean verdict; typed text is logged to FM_SEND_LOG.
make_send_stubs() {  # <dir> -> echoes the fake bin dir
  local fb="$1/fakebin"
  mkdir -p "$fb"
  cat > "$fb/tmux" <<'SH'
#!/usr/bin/env bash
set -u
case "${1:-}" in
  send-keys)
    shift
    literal=0
    while [ $# -gt 0 ]; do
      case "$1" in
        -t) shift 2 ;;
        -l) literal=1; shift ;;
        *) break ;;
      esac
    done
    if [ "$literal" = 1 ]; then printf '%s' "${1:-}" >> "$FM_SEND_LOG"; fi
    exit 0 ;;
  display-message)
    for a in "$@"; do case "$a" in *cursor_y*) printf '1\n'; exit 0 ;; esac; done
    printf 'fakepane\n'; exit 0 ;;
  capture-pane) printf '╭────╮\n│    │\n╰────╯\n'; exit 0 ;;
  list-windows) printf '%s\n' fm-t1; exit 0 ;;
esac
exit 0
SH
  chmod +x "$fb/tmux"
  printf '#!/usr/bin/env bash\nexit 0\n' > "$fb/sleep"
  chmod +x "$fb/sleep"
  printf '%s\n' "$fb"
}

test_raise_repeat_threshold_and_delivery_clear_the_listing() {
  local home out
  home=$(new_home lifecycle)
  printf 'needs-decision [key=api-shape] [at=%s]: pick REST or RPC\n' "$RAISED" > "$home/state/task1.status"

  drain_to "$home" d1 || fail "the first drain should exit 0; stderr: $(cat "$home/d1.err")"
  out=$(cat "$home/d1.out")
  assert_contains "$out" 'UNDELIVERED DECISIONS (raised while not delivered to the captain; new):' \
    "a decision raised while undelivered should get its own section"
  assert_contains "$out" "task1 [key=api-shape] needs-decision: pick REST or RPC | raised $(iso_of "$RAISED") | age 0h5m" \
    "the entry should show the task, key, note, raise time, and age from the status line's at= stamp"
  assert_not_contains "$out" 'DELIVERY FAILURE' "a five-minute-old decision is inside the default threshold"
  assert_contains "$out" 'task1 [key=api-shape]' "the decision should still list under OPEN DECISIONS"
  assert_equals 1 "$(ledger_count "$home")" "the first drain should record exactly one ledger entry"
  assert_equals "$RAISED" "$(ledger_field "$home" '.raised_at_epoch')" "the raise time should come from the at= stamp"
  assert_equals at "$(ledger_field "$home" '.raised_source')" "a stamped line should be flagged as raised at its stamp"
  assert_equals null "$(ledger_field "$home" '.delivered_at_epoch')" "a listed decision should start undelivered"

  drain_to "$home" d2 FM_UNDELIVERED_THRESHOLD=1 || fail "the second drain should exit 0"
  out=$(cat "$home/d2.out")
  assert_contains "$out" 'task1 [key=api-shape]' "an undelivered decision should stay listed on the next drain, not drop silently"
  assert_contains "$out" "DELIVERY FAILURE: 1 decision(s) not delivered to the captain; oldest raised $(iso_of "$RAISED") (0h5m ago)" \
    "an entry past the threshold should name the count and the oldest raise time"

  mark_delivered "$home" task1 api-shape || fail "recording delivery of the listed decision should exit 0"

  drain_to "$home" d3 FM_UNDELIVERED_THRESHOLD=1 || fail "the drain after delivery should exit 0"
  out=$(cat "$home/d3.out")
  assert_not_contains "$out" 'UNDELIVERED DECISIONS' "a delivered decision should leave the undelivered listing"
  assert_not_contains "$out" 'DELIVERY FAILURE' "no delivery failure should name a delivered decision"
  assert_contains "$out" 'task1 [key=api-shape]' "delivery must not close the decision; OPEN DECISIONS still owns it"
  assert_not_equals null "$(ledger_field "$home" '.delivered_at_epoch')" "the delivery event should be recorded in the ledger"
  pass "a raised decision lists with its age, fails past the threshold, and clears once delivery is recorded"
}

test_answer_through_fm_send_records_delivery_and_clears_the_listing() {
  local home dir fb log out rc
  home=$(new_home answer)
  dir="$TMP_ROOT/answer"
  fb=$(make_send_stubs "$dir")
  log="$dir/send.log"
  mkdir -p "$dir"
  fm_write_meta "$home/state/task1.meta" "window=sess:fm-t1" "kind=ship"
  printf 'needs-decision [key=api-shape] [at=%s]: pick REST or RPC\n' "$RAISED" > "$home/state/task1.status"

  drain_to "$home" d1 || fail "the first drain should exit 0"
  assert_contains "$(cat "$home/d1.out")" 'UNDELIVERED DECISIONS' \
    "precondition: the decision should be listed undelivered before the answer"

  env PATH="$fb:$PATH" FM_ROOT_OVERRIDE="$home" FM_HOME="$home" FM_SEND_LOG="$log" FM_SEND_SETTLE=0 \
    "$SEND" task1 --resolve-key api-shape "go with REST" >/dev/null 2>&1
  rc=$?
  expect_code 0 "$rc" "an answer sent with --resolve-key should succeed"

  drain_to "$home" d2 || fail "the drain after the answer should exit 0"
  out=$(cat "$home/d2.out")
  assert_not_contains "$out" 'UNDELIVERED DECISIONS' "an answered decision should leave the undelivered listing"
  assert_not_contains "$out" 'DELIVERY FAILURE' "an answered decision should not be reported as undelivered"
  assert_not_contains "$out" 'task1 [key=api-shape]' "the answered decision should no longer list under OPEN DECISIONS"
  assert_not_equals null "$(ledger_field "$home" '.delivered_at_epoch')" \
    "an answer through the steer command is a recorded delivery (ANSWERS.md D)"
  assert_equals null "$(ledger_field "$home" '.closed_at_epoch')" \
    "a delivered answer should not also be recorded as a closure"
  pass "an answer sent with --resolve-key records delivery and clears the undelivered listing"
}

test_unreadable_ledger_reports_failure_and_keeps_the_decision_open() {
  local home out
  if [ "$(id -u)" -eq 0 ]; then
    printf 'skip - the unreadable-ledger case needs a non-root user, since chmod 000 does not stop root\n'
    return 0
  fi
  home=$(new_home unreadable)
  printf 'needs-decision [key=api-shape] [at=%s]: pick REST or RPC\n' "$RAISED" > "$home/state/task1.status"
  drain_to "$home" d1 || fail "the first drain should exit 0"
  chmod 000 "$home/state/$LEDGER"

  drain_to "$home" d2 || fail "an unreadable ledger must not change the drain's exit status"
  out=$(cat "$home/d2.out")
  assert_contains "$out" 'DELIVERY FAILURE: undelivered-decision record unreadable; decisions may be undelivered' \
    "an unreadable ledger should surface as a delivery failure, not as an empty list"
  assert_contains "$out" 'task1 [key=api-shape]' "the OPEN DECISIONS listing should be unchanged by an unreadable ledger"
  pass "an unreadable ledger reports a delivery failure and leaves OPEN DECISIONS intact"
}

test_over_cap_listing_ends_with_the_omitted_count() {
  local home out note i key total=15 shown omitted last
  home=$(new_home capped)
  printf -v note '%*s' 190 ''
  note=${note// /w}
  for i in $(seq 1 "$total"); do
    key=$(printf 'k%02d' "$i")
    printf 'needs-decision [key=%s] [at=%s]: %s\n' "$key" "$((NOW - 60))" "$note" > "$home/state/task$i.status"
  done

  drain_to "$home" d1 || fail "an over-cap drain should exit 0"
  out=$(cat "$home/d1.out")
  assert_contains "$out" 'UNDELIVERED DECISIONS (raised' "an over-cap set should still print the undelivered section"
  omitted=$(printf '%s\n' "$out" | sed -n 's/^UNDELIVERED DECISIONS: \([0-9]*\) more omitted (byte cap)$/\1/p')
  [ -n "$omitted" ] || fail "an over-cap drain should end with the omitted-count line: $(tail -n 1 "$home/d1.out")"
  last=$(tail -n 1 "$home/d1.out")
  assert_equals "UNDELIVERED DECISIONS: $omitted more omitted (byte cap)" "$last" \
    "the omitted-count line should be the drain's final output"
  shown=$(sed -n '/^UNDELIVERED DECISIONS (raised/,$p' "$home/d1.out" | grep -c '^task[0-9]* \[key=k')
  assert_equals "$total" "$((shown + omitted))" \
    "the listed and omitted decisions should account for every undelivered decision"
  pass "an over-cap listing ends with the exact omitted count"
}

test_no_undelivered_entries_adds_no_output_and_no_ledger() {
  local home
  home=$(new_home empty)
  drain_to "$home" d1 || fail "an empty drain should exit 0"
  [ ! -s "$home/d1.out" ] || fail "an empty drain should print nothing new: $(cat "$home/d1.out")"
  [ ! -e "$home/state/$LEDGER" ] || fail "an empty drain should not create a ledger"
  pass "a drain with no decisions prints nothing new and creates no ledger"
}

test_secondmate_and_branch_actor_homes_never_record_or_report_undelivered_decisions() {
  local home rc
  home=$(new_home gate-control)
  printf 'needs-decision [key=api-shape] [at=%s]: pick REST or RPC\n' "$RAISED" > "$home/state/task1.status"
  drain_to "$home" d1 || fail "the control drain should exit 0"
  assert_contains "$(cat "$home/d1.out")" 'UNDELIVERED DECISIONS' \
    "control: the main home should report the undelivered decision"

  home=$(new_home gate-secondmate)
  printf 'needs-decision [key=api-shape] [at=%s]: pick REST or RPC\n' "$RAISED" > "$home/state/task1.status"
  printf 'parent\n' > "$home/.fm-secondmate-parent"
  drain_to "$home" d1 || fail "the secondmate drain should exit 0"
  assert_not_contains "$(cat "$home/d1.out")" 'UNDELIVERED DECISIONS' \
    "a secondmate home reports through its parent channel, not this section"
  [ ! -e "$home/state/$LEDGER" ] || fail "a secondmate home should not create a ledger"

  # A branch-actor drain without its handoff snapshot refuses before any section
  # runs (fm-wake-drain.sh's require_branch_eligible_rows), so the ledger gate is
  # defense in depth: the observable contract is the refusal plus no record.
  home=$(new_home gate-branch)
  printf 'needs-decision [key=api-shape] [at=%s]: pick REST or RPC\n' "$RAISED" > "$home/state/task1.status"
  drain_to "$home" d1 FM_SUPERVISION_ACTOR=branch; rc=$?
  expect_code 1 "$rc" "a branch-actor drain without a handoff snapshot should refuse"
  assert_contains "$(cat "$home/d1.err")" 'no branch-eligible row snapshot' \
    "the branch refusal should name the missing handoff snapshot"
  assert_not_contains "$(cat "$home/d1.out")" 'UNDELIVERED DECISIONS' \
    "a refused branch-actor drain should not print the undelivered section"
  [ ! -e "$home/state/$LEDGER" ] || fail "a branch-actor drain should not create a ledger"
  pass "secondmate and branch-actor homes neither record nor print undelivered decisions"
}

test_reopened_decision_is_raised_again_after_its_earlier_instance_closes() {
  local home out
  home=$(new_home reopen)
  printf 'needs-decision [key=api-shape] [at=%s]: pick REST or RPC\n' "$RAISED" > "$home/state/task1.status"
  drain_to "$home" d1 || fail "the first drain should exit 0"

  printf 'resolved [key=api-shape] [at=%s]: answered elsewhere\n' "$((NOW - 200))" >> "$home/state/task1.status"
  drain_to "$home" d2 || fail "the drain after the resolved line should exit 0"
  assert_not_contains "$(cat "$home/d2.out")" 'UNDELIVERED DECISIONS' \
    "a decision the fold closes should leave the undelivered listing"
  assert_equals 1 "$(jq -s '[.[] | select(.closed_at_epoch != null)] | length' "$home/state/$LEDGER")" \
    "the closed instance should be recorded as closed"

  printf 'needs-decision [key=api-shape] [at=%s]: pick again\n' "$((NOW - 10))" >> "$home/state/task1.status"
  drain_to "$home" d3 || fail "the drain after the reopen should exit 0"
  out=$(cat "$home/d3.out")
  assert_contains "$out" 'task1 [key=api-shape] needs-decision: pick again' \
    "a reopened decision should be raised again as a new undelivered entry"
  assert_equals 2 "$(ledger_count "$home")" "the reopened decision should add a second ledger record"
  assert_equals 1 "$(jq -s '[.[] | select(.delivered_at_epoch == null and .closed_at_epoch == null)] | length' "$home/state/$LEDGER")" \
    "only the reopened instance should still be undelivered"
  pass "a closed decision that reopens is raised again as a new entry"
}

test_raise_repeat_threshold_and_delivery_clear_the_listing
test_answer_through_fm_send_records_delivery_and_clears_the_listing
test_unreadable_ledger_reports_failure_and_keeps_the_decision_open
test_over_cap_listing_ends_with_the_omitted_count
test_no_undelivered_entries_adds_no_output_and_no_ledger
test_secondmate_and_branch_actor_homes_never_record_or_report_undelivered_decisions
test_reopened_decision_is_raised_again_after_its_earlier_instance_closes
