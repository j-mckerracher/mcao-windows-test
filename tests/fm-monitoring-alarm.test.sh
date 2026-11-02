#!/usr/bin/env bash
# Behavior tests for the monitoring-down captain alarm (DPES-854).
#
# Library cases drive bin/fm-monitoring-alarm-lib.sh's tick against the real
# awaiting-captain registry in an isolated home. The beat age is pinned by
# replacing only the production age helper inside that one shell, so the grace
# boundary is exact rather than a race against the wall clock. Guard cases run
# bin/fm-guard.sh end to end on the real clock, with ages far from every boundary.
set -u

# shellcheck source=tests/lib.sh
# shellcheck disable=SC1091
. "$(dirname "${BASH_SOURCE[0]}")/lib.sh"

TMP_ROOT=$(fm_test_tmproot fm-monitoring-alarm)
T1=2026-10-20T09:00:00Z

command -v jq >/dev/null 2>&1 || { echo "skip: jq not found"; exit 0; }

# new_home <name>: an isolated home with the directories the registry and guard use.
new_home() {
  local home="$TMP_ROOT/$1"
  mkdir -p "$home/data" "$home/state" "$home/config" "$home/projects"
  printf '%s\n' "$home"
}

# monitoring_records <home>: a JSON array of every monitoring-down record.
monitoring_records() {
  local file="$1/data/awaiting-captain.json"
  if [ -e "$file" ]; then
    jq -c '[.records[] | select(.kind == "monitoring-down")]' "$file"
  else
    printf '[]\n'
  fi
}

open_monitoring_count() {
  monitoring_records "$1" | jq 'map(select(.state == "open")) | length'
}

# lib_tick_from <code-root> <home> <age|missing> <in-flight> <healthy> <queue-pending>
# Runs one library tick in a fresh shell. The beat file exists unless age is
# "missing", and the production age helper returns the pinned age in that shell.
lib_tick_from() {
  local code=$1 home=$2 age=$3 in_flight=$4 healthy=$5 queue=$6
  if [ "$age" = missing ]; then
    rm -f "$home/state/.last-watcher-beat"
  else
    : > "$home/state/.last-watcher-beat"
  fi
  FM_HOME="$home" FM_STATE_OVERRIDE="$home/state" FM_DATA_OVERRIDE="$home/data" \
    FM_CONFIG_OVERRIDE="$home/config" FM_ROOT_OVERRIDE="$code" PINNED_AGE="$age" \
    bash -c '
      . "$1/bin/fm-wake-lib.sh"
      fm_path_age() { printf "%s\n" "$PINNED_AGE"; }
      . "$1/bin/fm-monitoring-alarm-lib.sh"
      monitoring_alarm_tick "$2" "$2/state" "$2/config" 300 "$3" "$4" "$5"
    ' _ "$code" "$home" "$in_flight" "$healthy" "$queue"
}

lib_tick() {  # <home> <age|missing> <in-flight> <healthy> <queue-pending>
  lib_tick_from "$ROOT" "$@"
}

# long_repair_root <name>: a copy of bin/ whose supervision-instructions script
# prints a 300-character repair line, so the library's own caps are exercised.
long_repair_root() {
  local code="$TMP_ROOT/$1"
  mkdir -p "$code"
  cp -R "$ROOT/bin" "$code/bin" || fail "could not copy bin/ for the long-repair case"
  cat > "$code/bin/fm-supervision-instructions.sh" <<'EOF'
#!/usr/bin/env bash
printf '%0300d\nsecond line\n' 0
EOF
  chmod +x "$code/bin/fm-supervision-instructions.sh"
  printf '%s\n' "$code"
}

summary_of() {  # <home> <index>
  monitoring_records "$1" | jq -r --argjson i "$2" '.[$i].summary'
}

# make_guard_case <name>: a home with one in-flight task, for the guard runs.
make_guard_case() {
  local dir="$TMP_ROOT/$1"
  mkdir -p "$dir/home/state" "$dir/home/config" "$dir/home/data" "$dir/root"
  fm_write_meta "$dir/home/state/task.meta" "window=firstmate:fm-task" "kind=ship"
  printf '%s\n' "$dir"
}

touch_beat_age() {  # <home> <seconds-old>
  fm_touch_epoch "$(( $(date +%s) - $2 ))" "$1/state/.last-watcher-beat"
}

# run_guard <dir> [actor]: one guard run on the persistent model, grace 300.
run_guard() {
  local dir=$1 actor=${2:-main}
  FM_ROOT_OVERRIDE="$dir/root" FM_HOME="$dir/home" \
    FM_STATE_OVERRIDE="$dir/home/state" FM_DATA_OVERRIDE="$dir/home/data" \
    FM_CONFIG_OVERRIDE="$dir/home/config" FM_GUARD_GRACE=300 \
    FM_SUPERVISION_MODEL=persistent FM_SUPERVISION_ACTOR="$actor" \
    "$ROOT/bin/fm-guard.sh" 2>&1
}

run_guard_read_only() {
  FM_GUARD_READ_ONLY=1 run_guard "$1"
}

count_text() {
  local haystack=$1 needle=$2
  awk -v needle="$needle" 'index($0, needle) { c++ } END { print c + 0 }' <<EOF
$haystack
EOF
}

# record_live_watcher <dir> <pid>: the persistent watcher lock a live watcher leaves.
record_live_watcher() {
  local dir=$1 pid=$2 home identity
  home="$dir/home"
  identity=$(FM_STATE_OVERRIDE="$home/state" bash -c '. "$1"; fm_pid_identity "$2"' _ "$ROOT/bin/fm-wake-lib.sh" "$pid") || return 1
  mkdir -p "$home/state/.watch.lock"
  printf '%s\n' "$pid" > "$home/state/.watch.lock/pid"
  printf '%s\n' "$home" > "$home/state/.watch.lock/fm-home"
  printf '%s\n' "$ROOT/bin/fm-watch.sh" > "$home/state/.watch.lock/watcher-path"
  printf '%s\n' "$identity" > "$home/state/.watch.lock/pid-identity"
}

BANNER='WATCHER DOWN - SUPERVISION IS OFF'
ALARM_FAILED='monitoring-alarm: FAILED'

test_grace_boundary_is_strict() {
  local home
  home=$(new_home grace-299)
  lib_tick "$home" 299 1 false false || fail "tick failed at 299s"
  assert_equals "0" "$(open_monitoring_count "$home")" "a beat 299s old is inside grace"

  home=$(new_home grace-300)
  lib_tick "$home" 300 1 false false || fail "tick failed at 300s"
  assert_equals "0" "$(open_monitoring_count "$home")" "a beat exactly at grace is not yet down"

  home=$(new_home grace-301)
  lib_tick "$home" 301 1 false false || fail "tick failed at 301s"
  assert_equals "1" "$(open_monitoring_count "$home")" "a beat 301s old is down"
  pass "monitoring-down starts strictly after the grace boundary"
}

test_outage_raises_one_record_and_refresh_updates_elapsed() {
  local home id_first id_second summary
  home=$(new_home outage)
  lib_tick "$home" 503 4 false false || fail "first tick failed"
  assert_equals "1" "$(monitoring_records "$home" | jq 'length')" "one record on first detection"
  id_first=$(monitoring_records "$home" | jq -r '.[0].id')
  summary=$(summary_of "$home" 0)
  case "$summary" in
    "Monitoring down 8m 23s with 4 tasks in flight; repair: "*) : ;;
    *) fail "first summary has the wrong elapsed text or count: $summary" ;;
  esac

  lib_tick "$home" 540 4 false false || fail "second tick failed"
  assert_equals "1" "$(monitoring_records "$home" | jq 'length')" "a later detection in the same outage adds no record"
  id_second=$(monitoring_records "$home" | jq -r '.[0].id')
  assert_equals "$id_first" "$id_second" "the outage keeps its record id"
  summary=$(summary_of "$home" 0)
  case "$summary" in
    "Monitoring down 9m 0s with 4 tasks in flight; repair: "*) : ;;
    *) fail "refreshed summary did not show the new elapsed time: $summary" ;;
  esac
  pass "one monitoring-down record per outage, with its elapsed time refreshed"
}

test_restore_on_healthy_watcher_is_acknowledged_once() {
  local home state by first again
  home=$(new_home restore)
  lib_tick "$home" 503 4 false false || fail "raise tick failed"
  lib_tick "$home" 503 4 true false || fail "restore tick failed"
  state=$(monitoring_records "$home" | jq -r '.[0].state')
  by=$(monitoring_records "$home" | jq -r '.[0].acknowledged_by')
  assert_equals "acknowledged" "$state" "restore acknowledges the open record"
  assert_equals "noted" "$by" "restore is recorded as noted"
  first=$(monitoring_records "$home" | jq -r '.[0].acknowledged_at')
  lib_tick "$home" 503 4 true false || fail "repeat restore tick failed"
  again=$(monitoring_records "$home" | jq -r '.[0].acknowledged_at')
  assert_equals "$first" "$again" "a repeat restore does not restate the acknowledgement"
  assert_equals "0" "$(open_monitoring_count "$home")" "nothing is left open after restore"
  pass "a healthy watcher restores monitoring once, then stays quiet"
}

test_no_work_restores_and_never_raises() {
  local home
  home=$(new_home idle)
  lib_tick "$home" 999 0 false false || fail "idle tick failed"
  assert_equals "0" "$(monitoring_records "$home" | jq 'length')" "no in-flight work means no alarm"
  assert_absent "$home/data/awaiting-captain.json" "an idle healthy tick must not create the registry"

  lib_tick "$home" 503 1 false false || fail "raise tick failed"
  lib_tick "$home" 503 0 false false || fail "drained tick failed"
  assert_equals "acknowledged" "$(monitoring_records "$home" | jq -r '.[0].state')" \
    "dropping to no in-flight work restores the open record"
  pass "no in-flight work raises nothing, and drained work restores the open record"
}

test_missing_beat_reports_unknown_age_not_a_fabricated_one() {
  local home summary
  home=$(new_home missing-beat)
  lib_tick "$home" missing 1 false false || fail "missing-beat tick failed"
  summary=$(summary_of "$home" 0)
  case "$summary" in
    "Monitoring down, last beat time unknown with 1 task in flight; repair: "*) : ;;
    *) fail "missing beat did not report unknown age: $summary" ;;
  esac
  pass "a missing beat file alarms with an unknown age instead of a made-up one"
}

test_summary_and_repair_are_bounded() {
  local code home summary
  code=$(long_repair_root repair-cap)
  home=$(new_home repair-cap)
  lib_tick_from "$code" "$home" 503 4 false false || fail "long-repair tick failed"
  summary=$(summary_of "$home" 0)
  [ "${#summary}" -le 160 ] || fail "summary exceeds 160 characters (${#summary}): $summary"
  case "$summary" in
    *...) : ;;
    *) fail "an over-long repair must end in an ellipsis: $summary" ;;
  esac
  case "$summary" in
    *"second line"*) fail "only the first repair line may reach the summary: $summary" ;;
  esac
  pass "the summary stays within 160 characters and carries only the first repair line"
}

test_captain_text_has_no_path_or_internal_word_in_the_outage_lead() {
  local home summary lead
  home=$(new_home plain-text)
  lib_tick "$home" 503 4 false false || fail "tick failed"
  summary=$(summary_of "$home" 0)
  lead=${summary%%; repair:*}
  case "$summary" in
    *'`'*) fail "summary must not carry a backtick: $summary" ;;
    */*) fail "summary must not carry a path: $summary" ;;
  esac
  for word in beat lock worktree 'task id' branch; do
    case "$lead" in
      *"$word"*) fail "outage lead uses internal word '$word': $lead" ;;
    esac
  done
  pass "the outage lead and the summary carry no path, backtick, or internal word"
}

test_registry_write_failure_reports_and_leaves_the_file_alone() {
  local home err rc
  home=$(new_home write-fail)
  printf 'not json\n' > "$home/data/awaiting-captain.json"
  cp "$home/data/awaiting-captain.json" "$TMP_ROOT/write-fail.before"
  err="$TMP_ROOT/write-fail.err"
  lib_tick "$home" 503 4 false false 2>"$err"; rc=$?
  assert_equals "0" "$rc" "a rejected registry write must not fail the tick"
  assert_contains "$(cat "$err")" "$ALARM_FAILED - registry write rejected" "the failure is reported on stderr"
  cmp -s "$TMP_ROOT/write-fail.before" "$home/data/awaiting-captain.json" \
    || fail "a corrupt registry must be left exactly as found"
  pass "a rejected registry write prints one stderr line and leaves the registry alone"
}

test_guard_raises_one_record_and_keeps_the_model_banner() {
  local dir home out1 out2 render
  dir=$(make_guard_case guard-raise)
  home="$dir/home"
  touch_beat_age "$home" 600
  out1=$(run_guard "$dir")
  assert_equals "1" "$(count_text "$out1" "$BANNER")" "the model-facing banner still prints"
  assert_not_contains "$out1" "Monitoring down" "the captain alarm stays out of model output"
  assert_not_contains "$out1" "$ALARM_FAILED" "a successful raise prints nothing"

  out2=$(run_guard "$dir")
  assert_not_contains "$out2" "Monitoring down" "a repeat run stays out of model output"
  assert_absent "$home/state/.wake-queue" "a captain alarm is never queued as a wake"
  assert_equals "1" "$(open_monitoring_count "$home")" "two guard runs in one outage keep one open record"
  assert_equals "1" "$(monitoring_records "$home" | jq 'length')" "two guard runs in one outage keep one record in total"
  case "$(summary_of "$home" 0)" in
    "Monitoring down 10m "*) : ;;
    *) fail "guard summary has the wrong elapsed time: $(summary_of "$home" 0)" ;;
  esac
  render=$(FM_HOME="$home" FM_ROOT_OVERRIDE="$ROOT" FM_STATE_OVERRIDE="$home/state" \
    FM_DATA_OVERRIDE="$home/data" FM_CONFIG_OVERRIDE="$home/config" FM_AWAITING_NOW="$T1" \
    bash "$ROOT/bin/fm-awaiting-captain.sh" render) || fail "render failed"
  assert_equals "1" "$(count_text "$render" "Monitoring down")" "the awaiting list shows the outage once"
  pass "a guard run raises one record through the awaiting list and keeps the banner unchanged"
}

test_guard_restores_when_the_watcher_recovers() {
  local dir home pid healthy state by
  dir=$(make_guard_case guard-restore)
  home="$dir/home"
  touch_beat_age "$home" 600
  run_guard "$dir" >/dev/null
  assert_equals "1" "$(open_monitoring_count "$home")" "the outage is open before recovery"

  sleep 60 &
  pid=$!
  record_live_watcher "$dir" "$pid" || fail "could not record the live watcher"
  touch_beat_age "$home" 0
  healthy=$(run_guard "$dir")
  kill "$pid" 2>/dev/null || true
  wait "$pid" 2>/dev/null || true
  assert_not_contains "$healthy" "$BANNER" "a healthy watcher prints no banner"
  state=$(monitoring_records "$home" | jq -r '.[0].state')
  by=$(monitoring_records "$home" | jq -r '.[0].acknowledged_by')
  assert_equals "acknowledged" "$state" "a healthy guard run restores the outage"
  assert_equals "noted" "$by" "the restore is recorded as noted"
  pass "a healthy watcher on the guard restores the open outage"
}

test_branch_actor_never_raises() {
  local dir home
  dir=$(make_guard_case guard-branch)
  home="$dir/home"
  touch_beat_age "$home" 600
  run_guard "$dir" branch >/dev/null
  assert_absent "$home/data/awaiting-captain.json" "the branch actor must never write the registry"
  pass "the branch actor never raises monitoring-down"
}

test_read_only_guard_never_raises() {
  local dir home
  dir=$(make_guard_case guard-read-only)
  home="$dir/home"
  touch_beat_age "$home" 600
  run_guard_read_only "$dir" >/dev/null
  assert_absent "$home/data/awaiting-captain.json" "a read-only guard run must not write the registry"
  pass "a read-only guard run never raises monitoring-down"
}

test_guard_without_in_flight_work_raises_nothing() {
  local dir home
  dir=$(make_guard_case guard-idle)
  home="$dir/home"
  rm -f "$home/state/task.meta"
  touch_beat_age "$home" 600
  run_guard "$dir" >/dev/null
  assert_absent "$home/data/awaiting-captain.json" "no in-flight work means no alarm on the guard"
  pass "the guard raises nothing when no work is in flight"
}

test_healthy_idle_guard_creates_no_registry() {
  local dir home
  dir=$(make_guard_case guard-healthy-idle)
  home="$dir/home"
  rm -f "$home/state/task.meta"
  touch_beat_age "$home" 0
  run_guard "$dir" >/dev/null
  assert_absent "$home/data/awaiting-captain.json" "a healthy idle guard run must not create the registry"
  pass "a healthy idle guard run creates no registry"
}

test_grace_boundary_is_strict
test_outage_raises_one_record_and_refresh_updates_elapsed
test_restore_on_healthy_watcher_is_acknowledged_once
test_no_work_restores_and_never_raises
test_missing_beat_reports_unknown_age_not_a_fabricated_one
test_summary_and_repair_are_bounded
test_captain_text_has_no_path_or_internal_word_in_the_outage_lead
test_registry_write_failure_reports_and_leaves_the_file_alone
test_guard_raises_one_record_and_keeps_the_model_banner
test_guard_restores_when_the_watcher_recovers
test_branch_actor_never_raises
test_read_only_guard_never_raises
test_guard_without_in_flight_work_raises_nothing
test_healthy_idle_guard_creates_no_registry
