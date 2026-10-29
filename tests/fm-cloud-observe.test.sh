#!/usr/bin/env bash
# Behavior tests for cloud task observation: which records are cloud, and what a
# bounded read of the run's StatusView does to the stored state, freshness, and
# crew-state projection (bin/fm-cloud-observe-lib.sh owns those rules).
#
# Every case calls the real library through its public functions. The read goes
# through the owner path (fm_exec_cloud_run_timed -> `mcao cloud status --run-id`),
# and a fake mcao on PATH answers with a staged protocol-v1 StatusView, so every
# assertion is on the projection a caller receives.
set -u

# shellcheck source=tests/lib.sh
. "$(dirname "${BASH_SOURCE[0]}")/lib.sh"

TMP=$(fm_test_tmproot fm-cloud-observe)
# shellcheck source=/dev/null
. "$ROOT/bin/fm-backend.sh"
# shellcheck source=/dev/null
. "$ROOT/bin/fm-timeout-lib.sh"
# shellcheck source=/dev/null
. "$ROOT/bin/fm-execution-target-lib.sh"
# shellcheck source=/dev/null
. "$ROOT/bin/fm-cloud-observe-lib.sh"

RUN=run_0123456789abcdef0123456789abcdef
MCAO_BIN="$TMP/bin"
RESP="$TMP/response.json"
export PATH="$MCAO_BIN:$PATH"

NOW=1000000
SEEN=2026-10-21T19:59:30Z

# meta_for <name> [extra record lines...] -> path of a task record with a cloud run
meta_for() {
  local path="$TMP/$1.meta"
  shift
  printf 'kind=ship\ncloud_request_id=req-1\ncloud_run_id=%s\n' "$RUN" > "$path"
  local line
  for line in "$@"; do printf '%s\n' "$line" >> "$path"; done
  printf '%s\n' "$path"
}

# respond <status-view-json> [exit-status] -> the next status read answers with it
respond() {
  printf '%s' "$1" > "$RESP"
  fm_test_mcao_stub "$MCAO_BIN" "$RESP" "${2:-}"
}

# observe <meta> <observation-file> <now> -> projection JSON
observe() {
  fm_cloud_observe "$1" "$2" "$3"
}

field() {
  jq -r ".$2" <<<"$1"
}

# no_mcao_path: the current PATH with every directory that holds an mcao removed
no_mcao_path() {
  local d out='' IFS=:
  for d in $PATH; do
    if [ -n "$d" ] && [ ! -x "$d/mcao" ]; then out="${out:+$out:}$d"; fi
  done
  printf '%s' "$out"
}

# --- execution target --------------------------------------------------------

assert_equals local "$(fm_cloud_execution_target "$(meta_for target-absent)")" "a record without execution_target is local"
assert_equals gcp "$(fm_cloud_execution_target "$(meta_for target-gcp execution_target=gcp)")" "execution_target=gcp is cloud"
assert_equals local "$(fm_cloud_execution_target "$(meta_for target-local execution_target=local)")" "execution_target=local is local"
assert_equals invalid "$(fm_cloud_execution_target "$(meta_for target-bogus execution_target=vm)")" "an unknown execution_target is invalid, not local"
pass "execution target classifies absent, gcp, local, and unknown values"

# --- no client and no run id -------------------------------------------------

META=$(meta_for no-client execution_target=gcp)
OBS="$TMP/no-client.cloud.json"
PROJ=$(PATH="$(no_mcao_path)"; observe "$META" "$OBS" "$NOW")
assert_equals unknown "$(field "$PROJ" crew_state)" "no installed client projects unknown"
assert_equals null "$(field "$PROJ" state)" "no installed client stores no observed state"
assert_contains "$(field "$PROJ" detail)" "cloud status unavailable" "no installed client says the status is unavailable"
assert_equals "cloud client mcao is not installed" "$(field "$PROJ" diagnostic)" "a missing client is named, not read as a run state"
assert_absent "$OBS" "no installed client writes no observation"
pass "a task with no installed status client reads as unknown without writing an observation"

META="$TMP/no-run.meta"
printf 'kind=ship\ncloud_request_id=req-1\nexecution_target=gcp\n' > "$META"
PROJ=$(observe "$META" "$TMP/no-run.cloud.json" "$NOW")
assert_equals "no single cloud run id is recorded for this task" "$(field "$PROJ" diagnostic)" "a record without a run id is refused, never read"
assert_absent "$TMP/no-run.cloud.json" "a record without a run id writes no observation"
pass "a record without a single cloud run id is never read"

# --- fresh running state, server clock ---------------------------------------

META=$(meta_for running execution_target=gcp)
OBS="$TMP/running.cloud.json"
respond "$(fm_test_cloud_status_view running 3 fresh "$SEEN")"
PROJ=$(observe "$META" "$OBS" "$NOW")
assert_equals working "$(field "$PROJ" crew_state)" "a running task with server-fresh telemetry is working"
assert_equals fresh "$(field "$PROJ" freshness)" "the server's fresh verdict is reported as fresh"
assert_equals 30 "$(field "$PROJ" heartbeat_age)" "heartbeat age is server_time minus last_seen_at"
assert_present "$OBS" "a valid response is stored"
pass "a running task with server-fresh telemetry projects as working"

PROJ=$(observe "$META" "$OBS" "$((NOW + 100000))")
assert_equals 30 "$(field "$PROJ" heartbeat_age)" "heartbeat age does not depend on the local clock"
pass "heartbeat age is measured on the server clock, not the local clock"

# --- stale and clock-anomaly telemetry ---------------------------------------

respond "$(fm_test_cloud_status_view running 4 stale "$SEEN")"
PROJ=$(observe "$META" "$OBS" "$NOW")
assert_equals stale "$(field "$PROJ" freshness)" "the server's stale verdict is reported as stale"
assert_equals unknown "$(field "$PROJ" crew_state)" "stale telemetry is unknown, never working or idle"
assert_equals running "$(field "$PROJ" state)" "stale telemetry keeps the server-observed state"
pass "stale telemetry keeps the state and reads as unknown, not terminated"

respond "$(fm_test_cloud_status_view running 5 clock_anomaly "$SEEN")"
PROJ=$(observe "$META" "$OBS" "$NOW")
assert_equals unknown "$(field "$PROJ" freshness)" "a clock anomaly has unknown freshness"
assert_equals unknown "$(field "$PROJ" crew_state)" "a clock anomaly cannot create false liveness"
pass "a server clock anomaly is unknown, not fresh"

# --- failed and invalid reads keep the last observation ----------------------

respond '' 7
PROJ=$(observe "$META" "$OBS" "$NOW")
assert_equals running "$(field "$PROJ" state)" "a failed read keeps the stored state"
assert_equals unknown "$(field "$PROJ" freshness)" "a failed read marks freshness unknown"
assert_equals unknown "$(field "$PROJ" crew_state)" "a failed read is not translated to working, idle, dead, or success"
assert_equals "status read failed (exit 7)" "$(field "$PROJ" diagnostic)" "a failed read names its exit status"
pass "an authentication or network failure keeps the last observation and marks it unknown"

respond 'not json'
PROJ=$(observe "$META" "$OBS" "$NOW")
assert_equals running "$(field "$PROJ" state)" "an invalid response keeps the stored state"
assert_equals "status response failed validation" "$(field "$PROJ" diagnostic)" "an invalid response is reported as such"
pass "an invalid status response is refused and keeps the last observation"

respond "$(fm_test_cloud_status_view exploded 6 fresh "$SEEN")"
PROJ=$(observe "$META" "$OBS" "$NOW")
assert_equals "status response failed validation" "$(field "$PROJ" diagnostic)" "an unknown protocol state fails validation"
pass "a response naming a state outside protocol-v1 fails validation"

respond "$(fm_test_cloud_status_view running 6 fresh "$SEEN" run_ffffffffffffffffffffffffffffffff)"
PROJ=$(observe "$META" "$OBS" "$NOW")
assert_equals "status response failed validation" "$(field "$PROJ" diagnostic)" "a response for a different run fails validation"
assert_equals running "$(field "$PROJ" state)" "a response for a different run never replaces the stored observation"
pass "a response naming another run is refused"

respond "$(fm_test_cloud_status_view running 6 fresh '2026-10-21 19:59:30')"
PROJ=$(observe "$META" "$OBS" "$NOW")
assert_equals "status response failed validation" "$(field "$PROJ" diagnostic)" "a timestamp outside the UTC form fails validation"
pass "a non-UTC timestamp is refused rather than guessed"

# --- terminal states are monotonic and out-of-order responses are ignored ----

META=$(meta_for terminal execution_target=gcp)
OBS="$TMP/terminal.cloud.json"
respond "$(fm_test_cloud_status_view failed 5 stale -)"
observe "$META" "$OBS" "$NOW" >/dev/null
respond "$(fm_test_cloud_status_view running 6 fresh "$SEEN")"
PROJ=$(observe "$META" "$OBS" "$NOW")
assert_equals failed "$(field "$PROJ" state)" "a later running response never replaces a stored failure"
assert_equals failed "$(field "$PROJ" crew_state)" "a stored failure stays failed"
assert_equals final "$(field "$PROJ" freshness)" "a terminal state is final, not stale"
pass "terminal states never regress to running"

META=$(meta_for out-of-order execution_target=gcp)
OBS="$TMP/out-of-order.cloud.json"
respond "$(fm_test_cloud_status_view running 4 fresh "$SEEN")"
observe "$META" "$OBS" "$NOW" >/dev/null
respond "$(fm_test_cloud_status_view queued 3 fresh -)"
PROJ=$(observe "$META" "$OBS" "$NOW")
assert_equals running "$(field "$PROJ" state)" "a lower status_seq response is out of order and ignored"
assert_equals "out-of-order response ignored" "$(field "$PROJ" diagnostic)" "an out-of-order response is reported"
assert_equals unknown "$(field "$PROJ" freshness)" "an ignored response does not vouch for freshness"
pass "an out-of-order response cannot move a task backwards"

# --- supervision states are shown faithfully ---------------------------------

META=$(meta_for states execution_target=gcp)
OBS="$TMP/states.cloud.json"
respond "$(fm_test_cloud_status_view cancel_requested 2 fresh "$SEEN")"
PROJ=$(observe "$META" "$OBS" "$NOW")
assert_equals working "$(field "$PROJ" crew_state)" "cancel_requested with fresh telemetry is working"
assert_contains "$(field "$PROJ" detail)" "cancel_requested" "cancel_requested is shown by name"
pass "a fresh cancel_requested run is shown faithfully as working"

respond "$(fm_test_cloud_status_view queued 1 fresh -)"
PROJ=$(observe "$META" "$OBS" "$NOW")
assert_equals cancel_requested "$(field "$PROJ" state)" "a stale status_seq cannot downgrade cancel_requested"
pass "a queued response after cancel_requested is out of order"

# --- succeeded runs are never projected as ready -----------------------------

META=$(meta_for succeeded execution_target=gcp)
OBS="$TMP/succeeded.cloud.json"
respond "$(fm_test_cloud_status_view succeeded 8 fresh "$SEEN")"
PROJ=$(observe "$META" "$OBS" "$NOW")
assert_equals succeeded "$(field "$PROJ" state)" "a succeeded run is reported as succeeded"
assert_equals unknown "$(field "$PROJ" crew_state)" "a succeeded run is unknown without a result read, never parked or done"
assert_contains "$(field "$PROJ" detail)" "no result" "the detail says the status carries no result"
assert_contains "$(field "$PROJ" detail)" "nothing is reported ready" "a succeeded run is not reported ready for import"
pass "a succeeded run is never projected as ready for import from the status read alone"

for terminal in timed_out cancelled; do
  respond "$(fm_test_cloud_status_view "$terminal" 3 stale -)"
  PROJ=$(observe "$(meta_for "terminal-$terminal" execution_target=gcp)" "$TMP/terminal-$terminal.cloud.json" "$NOW")
  assert_equals failed "$(field "$PROJ" crew_state)" "$terminal projects as failed"
done
pass "timed out and cancelled runs project as failed"

respond "$(fm_test_cloud_status_view failed 4 stale - | jq -c '.run.failure = {code: "worker_failed", retryable: false, message: ""}')"
PROJ=$(observe "$(meta_for failed-code execution_target=gcp)" "$TMP/failed-code.cloud.json" "$NOW")
assert_equals failed "$(field "$PROJ" crew_state)" "a failed run projects as failed"
assert_contains "$(field "$PROJ" detail)" "worker_failed" "the typed failure code is named in the detail"
pass "a failed run names its typed failure code"
