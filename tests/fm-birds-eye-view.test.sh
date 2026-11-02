#!/usr/bin/env bash
# Behavior tests for bin/fm-birds-eye-view.sh and its loopback server
# (bin/fm-birds-eye-view-serve.py, DPES-798). The tests drive the real CLI and the real
# server. The sibling model, capture, and export helpers are replaced through the
# FM_BIRDS_EYE_*_OVERRIDE test seams with stubs from tests/fixtures/birds-eye/serve/, so
# the interfaces are exercised without claiming the production helpers work. Assertions
# use exit codes, printed output, HTTP responses, recorded helper invocations, and
# process state, never source text. Refresh, overlap, and shutdown run on the real clock
# because the interval floor is 5 seconds, so this file takes roughly a minute.
set -u

# shellcheck source=tests/lib.sh
# shellcheck disable=SC1091
. "$(dirname "${BASH_SOURCE[0]}")/lib.sh"

CLI="$ROOT/bin/fm-birds-eye-view.sh"
FIXTURES="$ROOT/tests/fixtures/birds-eye/serve"
TMP_ROOT=$(fm_test_tmproot fm-birds-eye-view)
SERVER_PID=""
SERVER_PORT=""
SERVER_OUT=""
SERVER_ERR=""
OCCUPANT_PID=""
OCCUPANT_PORT=""
SENTINEL_PID=""
CLI_STATUS=""
CLI_OUT=""
CLI_ERR=""
FETCH_CODE=""
FETCH_BODY=""
FETCH_HEADERS=""

# --- helpers ----------------------------------------------------------------

wait_until() {
  local limit=$1 i
  shift
  for ((i = 0; i < limit * 10; i++)); do
    if "$@"; then
      return 0
    fi
    sleep 0.1
  done
  return 1
}

# process_gone: true when the pid no longer runs. A zombie counts as gone, because a
# background child of this shell stays a zombie until it is waited for.
process_gone() {
  local state
  state=$(ps -o stat= -p "$1" 2>/dev/null) || return 0
  [ -z "$state" ] || [ "${state#Z}" != "$state" ]
}

# new_dir <name>: creates a test directory and prints its physical path. macOS temp
# paths sit behind /var -> /private/var, and the CLI canonicalizes roots, so assertions
# on root paths must use the same spelling.
new_dir() {
  mkdir -p "$TMP_ROOT/$1" || fail "cannot create $1"
  (cd "$TMP_ROOT/$1" && pwd -P)
}

# use_stubs <home> <state-dir>: exports the configured home and the stub seams.
use_stubs() {
  local home=$1 state=$2
  mkdir -p "$state"
  export FM_HOME="$home"
  export FM_STUB_HOME="$home"
  export FM_STUB_STATE="$state"
  export FM_STUB_RECORD="$state/model.log"
  export FM_STUB_CAPTURE_RECORD="$state/capture.log"
  export FM_STUB_EXPORT_RECORD="$state/export.log"
  export FM_STUB_PIDFILE="$state/hang.pid"
  export FM_BIRDS_EYE_MODEL_OVERRIDE="$FIXTURES/stub-model.sh"
  export FM_BIRDS_EYE_CAPTURE_OVERRIDE="$FIXTURES/stub-capture.sh"
  export FM_BIRDS_EYE_EXPORT_OVERRIDE="$FIXTURES/stub-export.sh"
  unset FM_STUB_HANG FM_STUB_EXPORT_FAIL FM_STUB_OPEN_EXIT
}

# run_cli <args...>: runs the CLI to completion and sets CLI_STATUS, CLI_OUT, CLI_ERR.
run_cli() {
  CLI_OUT="$TMP_ROOT/cli.out"
  CLI_ERR="$TMP_ROOT/cli.err"
  "$CLI" "$@" >"$CLI_OUT" 2>"$CLI_ERR"
  CLI_STATUS=$?
}

expect_status() {
  [ "$CLI_STATUS" = "$1" ] || fail "$2: expected exit $1, got $CLI_STATUS: $(cat "$CLI_ERR")"
}

# start_server <name> [args...]: runs the live CLI in the background and waits for its
# URL line. Sets SERVER_PID, SERVER_PORT, SERVER_OUT, and SERVER_ERR.
start_server() {
  local name=$1
  shift
  SERVER_OUT="$TMP_ROOT/$name.out"
  SERVER_ERR="$TMP_ROOT/$name.err"
  "$CLI" "$@" >"$SERVER_OUT" 2>"$SERVER_ERR" &
  SERVER_PID=$!
  if ! wait_until 20 grep -q '^birds-eye view: http://127.0.0.1:' "$SERVER_OUT"; then
    fail "$name: no URL printed; stderr: $(cat "$SERVER_ERR")"
  fi
  SERVER_PORT=$(sed -n 's|^birds-eye view: http://127.0.0.1:\([0-9]*\)/$|\1|p' "$SERVER_OUT" | head -1)
  [ -n "$SERVER_PORT" ] || fail "$name: URL line is not a loopback URL"
}

stop_server() {
  if [ -n "$SERVER_PID" ]; then
    if ! process_gone "$SERVER_PID"; then
      kill -TERM "$SERVER_PID" 2>/dev/null || true
      if ! wait_until 20 process_gone "$SERVER_PID"; then
        kill -KILL "$SERVER_PID" 2>/dev/null || true
      fi
    fi
    wait "$SERVER_PID" 2>/dev/null || true
  fi
  SERVER_PID=""
}

# fetch <path> [curl args...]: one GET against the running server. Sets FETCH_CODE,
# FETCH_BODY, and FETCH_HEADERS.
fetch() {
  local path=$1
  shift
  FETCH_BODY="$TMP_ROOT/fetch.body"
  FETCH_HEADERS="$TMP_ROOT/fetch.headers"
  FETCH_CODE=$(curl -sS --path-as-is -o "$FETCH_BODY" -D "$FETCH_HEADERS" -w '%{http_code}' \
    "$@" "http://127.0.0.1:$SERVER_PORT$path" 2>/dev/null)
}

expect_fetch() {
  [ "$FETCH_CODE" = "$1" ] || fail "$2: expected HTTP $1, got $FETCH_CODE"
}

view_ready() {
  fetch /api/view
  [ "$FETCH_CODE" = 200 ]
}

count_starts() {
  awk '/^start /{ n++ } END { print n + 0 }' "$FM_STUB_RECORD" 2>/dev/null || echo 0
}

at_least_starts() {
  [ "$(count_starts)" -ge "$1" ]
}

count_overlaps() {
  awk '/^overlap /{ n++ } END { print n + 0 }' "$FM_STUB_RECORD" 2>/dev/null || echo 0
}

capture_calls() {
  if [ -f "$FM_STUB_CAPTURE_RECORD" ]; then
    wc -l < "$FM_STUB_CAPTURE_RECORD" | tr -d ' '
  else
    echo 0
  fi
}

# first_model_roots <record>: the --root values of the first model invocation.
first_model_roots() {
  awk '/^start /{ for (i = 1; i < NF; i++) if ($i == "--root") print $(i + 1); exit }' "$1"
}

# min_gap_after <skip>: prints "<count> <smallest gap>" between model starts after the
# first <skip> of them, using the epochs the stub recorded.
min_gap_after() {
  python3 -I -c '
import sys
skip = int(sys.argv[2])
times = []
for line in open(sys.argv[1], encoding="utf-8"):
    if line.startswith("start "):
        times.append(float(line.split()[1]))
times = times[skip:]
gaps = [b - a for a, b in zip(times, times[1:])]
print(len(times), min(gaps, default=0))
' "$FM_STUB_RECORD" "$1"
}

# start_occupant: holds a loopback port with a plain listener this test owns.
start_occupant() {
  local port_file="$TMP_ROOT/occupant.port"
  python3 -I -c '
import socket, time
listener = socket.socket()
listener.bind(("127.0.0.1", 0))
listener.listen(5)
print(listener.getsockname()[1], flush=True)
time.sleep(120)
' > "$port_file" &
  OCCUPANT_PID=$!
  wait_until 10 test -s "$port_file" || fail "occupant listener did not start"
  OCCUPANT_PORT=$(cat "$port_file")
}

cleanup() {
  stop_server
  for pid in "$OCCUPANT_PID" "$SENTINEL_PID"; do
    if [ -n "$pid" ]; then
      kill "$pid" 2>/dev/null || true
    fi
  done
  fm_test_cleanup
}
trap cleanup EXIT

# --- argument and refusal behavior -----------------------------------------

test_refusals() {
  local home state value
  home=$(new_dir refuse-home)
  state=$(new_dir refuse-state)
  use_stubs "$home" "$state"

  run_cli --interval 4
  expect_status 2 "--interval 4"
  grep -Fq 'from 5 to 999999' "$CLI_ERR" || fail "--interval 4 refusal does not state the minimum"
  for value in abc 0 1000000 -1; do
    run_cli --interval "$value"
    expect_status 2 "--interval $value"
  done
  for value in 0 65536 70000 abc -1; do
    run_cli --port "$value"
    expect_status 2 "--port $value"
  done
  run_cli --interval
  expect_status 2 "--interval without a value"
  run_cli --snapshot "$TMP_ROOT/refuse.json" --port 10000
  expect_status 2 "--snapshot with --port"
  run_cli --snapshot "$TMP_ROOT/refuse.json" --open
  expect_status 2 "--snapshot with --open"
  run_cli --bogus
  expect_status 2 "unknown flag"
  [ ! -e "$TMP_ROOT/refuse.json" ] || fail "a refused snapshot wrote a file"
  pass "bad interval, port, and flag combinations exit 2 before anything starts"

  run_cli --help
  expect_status 0 "--help"
  grep -Fq -- '--interval' "$CLI_OUT" || fail "--help does not document --interval"
  pass "--help documents the arguments and exits 0"
}

test_snapshot() {
  local home state extra out
  home=$(new_dir snap-home)
  state=$(new_dir snap-state)
  extra=$(new_dir snap-extra)
  use_stubs "$home" "$state"
  mkdir -p "$TMP_ROOT/snapshots"
  out="$TMP_ROOT/snapshots/view.json"

  run_cli --snapshot "$out" --root "$extra" --interval 15
  expect_status 0 "snapshot"
  [ -s "$out" ] || fail "snapshot file was not written"
  grep -Fq -- "--output $out" "$FM_STUB_EXPORT_RECORD" || fail "exporter did not receive --output"
  grep -Fq -- "--interval 15" "$FM_STUB_EXPORT_RECORD" || fail "exporter did not receive the interval"
  grep -Fq -- "--root $home" "$FM_STUB_EXPORT_RECORD" || fail "exporter did not receive the configured home"
  grep -Fq -- "--root $extra" "$FM_STUB_EXPORT_RECORD" || fail "exporter did not receive the extra root"
  if grep -Fq 'birds-eye view:' "$CLI_OUT"; then
    fail "snapshot printed a listener URL"
  fi
  [ ! -s "$state/model.log" ] || fail "snapshot started a collector"
  pass "snapshot passes the roots, interval, and output to the exporter and starts no listener or collector"

  export FM_STUB_EXPORT_FAIL=1
  run_cli --snapshot "$TMP_ROOT/snapshots/failed.json"
  expect_status 1 "snapshot with a failing exporter"
  [ ! -e "$TMP_ROOT/snapshots/failed.json" ] || fail "failed export left a snapshot file"
  unset FM_STUB_EXPORT_FAIL

  export FM_BIRDS_EYE_EXPORT_OVERRIDE="$TMP_ROOT/no-exporter.sh"
  run_cli --snapshot "$TMP_ROOT/snapshots/missing.json"
  expect_status 1 "snapshot with no exporter"
  grep -Fq 'snapshot exporter is not available' "$CLI_ERR" || fail "missing exporter is not reported"
  [ ! -e "$TMP_ROOT/snapshots/missing.json" ] || fail "missing exporter left a snapshot file"
  pass "a failing or missing exporter exits 1 and leaves no snapshot"
}

test_live_refused_without_model() {
  local home state
  home=$(new_dir nomodel-home)
  state=$(new_dir nomodel-state)
  use_stubs "$home" "$state"
  export FM_BIRDS_EYE_MODEL_OVERRIDE="$TMP_ROOT/no-model.sh"

  run_cli
  expect_status 1 "live mode without a model"
  grep -Fq 'the model helper is not available' "$CLI_ERR" || fail "missing model is not reported"
  pass "live mode without the aggregation model exits 1 and says so"
}

# --- running server -----------------------------------------------------------

test_live_contract() {
  local home state extra before
  home=$(new_dir live-home)
  state=$(new_dir live-state)
  extra=$(new_dir live-extra)
  use_stubs "$home" "$state"
  start_server live --root "$extra" --root "$home"

  wait_until 20 view_ready || fail "/api/view never returned a document; stderr: $(cat "$SERVER_ERR")"
  [ "$(jq -r .schema "$FETCH_BODY")" = fm-birds-eye-view.v1 ] || fail "/api/view is not an fm-birds-eye-view.v1 document"
  pass "/api/view serves the aggregation document after the first collection"

  [ "$(first_model_roots "$FM_STUB_RECORD" | grep -Fxc "$home")" = 1 ] || fail "configured home was not passed exactly once"
  [ "$(first_model_roots "$FM_STUB_RECORD" | grep -Fxc "$extra")" = 1 ] || fail "extra root was not passed"
  grep -Eq '^start [0-9.]+ .*--interval 10( |$)' "$FM_STUB_RECORD" || fail "default interval 10 was not passed"
  pass "collection gets the configured home once, the extra root, and the default 10 second interval"

  fetch /
  expect_fetch 200 "dashboard page"
  grep -qi '^content-type: text/html' "$FETCH_HEADERS" || fail "dashboard page is not served as HTML"
  grep -qi "^content-security-policy: default-src 'none'" "$FETCH_HEADERS" || fail "dashboard page has no restrictive CSP"
  grep -qi '^x-content-type-options: nosniff' "$FETCH_HEADERS" || fail "nosniff header missing"
  fetch /app.js
  expect_fetch 200 "app.js"
  fetch /app.css
  expect_fetch 200 "app.css"
  pass "the page and its two local assets are served with CSP and nosniff"

  fetch /nope
  expect_fetch 404 "unlisted path"
  fetch /etc/passwd
  expect_fetch 404 "absolute file path"
  fetch /../../etc/passwd
  expect_fetch 404 "traversal path"
  fetch "/?x=1"
  expect_fetch 400 "query on the page"
  pass "unlisted, traversal, and file paths are not served"

  fetch / -X POST
  expect_fetch 405 "POST"
  fetch /api/view -X DELETE
  expect_fetch 405 "DELETE"
  [ "$(curl -sS -I -o /dev/null -w '%{http_code}' "http://127.0.0.1:$SERVER_PORT/" 2>/dev/null)" = 405 ] \
    || fail "HEAD was not refused with 405"
  pass "non-GET methods are refused with 405"

  fetch / -H "Host: evil.example"
  expect_fetch 403 "foreign Host"
  fetch / -H "Host: localhost:$SERVER_PORT"
  expect_fetch 403 "localhost Host name"
  fetch /api/view -H "Host: 127.0.0.1:$SERVER_PORT.evil.example"
  expect_fetch 403 "Host with a suffix"
  fetch /api/view -H "Origin: http://evil.example"
  expect_fetch 403 "cross-origin request"
  fetch /api/view -H "Origin: http://127.0.0.1:$SERVER_PORT"
  expect_fetch 200 "same-origin request"
  pass "foreign Host and cross-origin requests are refused while the bound origin is served"

  before=$(capture_calls)
  [ "$before" = 0 ] || fail "capture ran during collection ($before calls)"
  pass "refresh does not run output capture"

  fetch "/api/capture?key=home:home:task-1&spawn_gen=gen-1"
  expect_fetch 200 "capture"
  [ "$(jq -r .status "$FETCH_BODY")" = ok ] || fail "capture of a current worker is not ok"
  [ "$(jq -r .text "$FETCH_BODY")" = "task=task-1 gen=gen-1 lines=40" ] \
    || fail "capture default is not 40 lines with the requested generation: $(jq -r .text "$FETCH_BODY")"
  pass "capture returns the default 40 lines for the requested generation"

  fetch "/api/capture?key=home:home:task-1&spawn_gen=gen-1&lines=250"
  expect_fetch 200 "capture over the maximum"
  [ "$(jq -r .text "$FETCH_BODY")" = "task=task-1 gen=gen-1 lines=100" ] \
    || fail "capture above the maximum was not limited to 100 lines"
  pass "capture above 100 lines is limited to 100"

  before=$(capture_calls)
  fetch "/api/capture?key=home:home:task-1"
  expect_fetch 200 "capture without generation"
  [ "$(jq -r .status "$FETCH_BODY")" = generation_mismatch ] || fail "missing generation was not a mismatch"
  [ "$(jq -r .text "$FETCH_BODY")" = "" ] || fail "missing generation returned text"
  fetch "/api/capture?key=home:home:task-1&spawn_gen="
  [ "$(jq -r .status "$FETCH_BODY")" = generation_mismatch ] || fail "empty generation was not a mismatch"
  [ "$(capture_calls)" = "$before" ] || fail "a capture without generation reached the helper"
  pass "missing or empty generation returns a mismatch without text and never reaches the helper"

  fetch "/api/capture?key=home:home:task-1&spawn_gen=stale"
  expect_fetch 200 "stale generation"
  [ "$(jq -r .status "$FETCH_BODY")" = generation_mismatch ] || fail "helper's generation mismatch was not returned"
  [ "$(jq -r .text "$FETCH_BODY")" = "" ] || fail "stale generation returned text"
  grep -Fq -- "--spawn-gen stale" "$FM_STUB_CAPTURE_RECORD" || fail "requested generation was not forwarded unchanged"
  pass "the requested generation is forwarded unchanged, and a stale one returns no text"

  fetch "/api/capture?key=home:home:task-9&spawn_gen=gen-1"
  expect_fetch 200 "unknown worker"
  [ "$(jq -r .status "$FETCH_BODY")" = unknown_identity ] || fail "key outside the current view was not refused"
  [ "$(jq -r .text "$FETCH_BODY")" = "" ] || fail "unknown key returned text"
  pass "a key outside the current view returns unknown_identity without text"

  before=$(capture_calls)
  for query in \
    "/api/capture" \
    "/api/capture?spawn_gen=gen-1" \
    "/api/capture?key=home:home:task-1&spawn_gen=gen-1&cmd=ls" \
    "/api/capture?key=home:home:task-1&spawn_gen=gen-1&path=/etc/passwd" \
    "/api/capture?key=home:home:task-1&spawn_gen=gen-1&lines=0" \
    "/api/capture?key=home:home:task-1&spawn_gen=gen-1&lines=abc" \
    "/api/capture?key=home:home:task-1&key=home:home:task-1&spawn_gen=gen-1" \
    "/api/capture?key=home:home:task-1&spawn_gen=gen-1&spawn_gen=gen-2"; do
    fetch "$query"
    expect_fetch 400 "capture rejection $query"
  done
  [ "$(capture_calls)" = "$before" ] || fail "a rejected capture request reached the helper"
  pass "capture refuses extra selectors, repeated parameters, and bad line counts before the helper runs"

  stop_server
  pass "live server stops on SIGTERM"
}

test_busy_port_is_refused() {
  local home state
  home=$(new_dir busy-home)
  state=$(new_dir busy-state)
  use_stubs "$home" "$state"
  start_occupant

  run_cli --port "$OCCUPANT_PORT"
  expect_status 1 "busy port"
  grep -Fq 'already in use' "$CLI_ERR" || fail "busy port is not reported: $(cat "$CLI_ERR")"
  ! process_gone "$OCCUPANT_PID" || fail "refusing a busy port killed the process using it"
  [ ! -s "$state/model.log" ] || fail "a refused server started a collector"
  pass "a busy port exits 1 and leaves the process using it running"
}

test_fixed_port_is_used() {
  local home state port
  home=$(new_dir fixed-home)
  state=$(new_dir fixed-state)
  use_stubs "$home" "$state"
  port=$OCCUPANT_PORT
  kill "$OCCUPANT_PID" 2>/dev/null || true
  wait "$OCCUPANT_PID" 2>/dev/null || true
  OCCUPANT_PID=""

  start_server fixed --port "$port"
  [ "$SERVER_PORT" = "$port" ] || fail "server bound port $SERVER_PORT instead of $port"
  stop_server
  pass "an explicit free port is used as given"
}

# --- refresh, overlap, and last good view (real clock) ---------------------

test_refresh_overlap_and_last_good() {
  local home state before gap
  home=$(new_dir cycle-home)
  state=$(new_dir cycle-state)
  use_stubs "$home" "$state"
  start_server cycles --interval 5

  wait_until 20 at_least_starts 2 || fail "fewer than two collections ran at interval 5"
  read -r _ gap <<< "$(min_gap_after 0)"
  awk -v g="$gap" 'BEGIN { exit !(g >= 4.5) }' || fail "interval 5 ran collections $gap seconds apart"
  pass "collections run at the requested interval"

  printf '6\n' > "$state/sleep"
  before=$(count_starts)
  wait_until 30 at_least_starts $((before + 2)) || fail "slow collections did not run"
  read -r _ gap <<< "$(min_gap_after "$before")"
  [ "$(count_overlaps)" = 0 ] || fail "a collection started while another was still running"
  awk -v g="$gap" 'BEGIN { exit !(g >= 5.8) }' \
    || fail "collections that outlast the interval started $gap seconds apart; a cycle overlapped"
  pass "no collection starts while another runs, even when a cycle outlasts the interval"

  : > "$state/fail-now"
  wait_until 30 grep -Fq 'keeping the last good view' "$SERVER_ERR" \
    || fail "a failed collection was not reported; stderr: $(cat "$SERVER_ERR")"
  fetch /api/view
  expect_fetch 200 "view after a failed collection"
  [ "$(jq -r .schema "$FETCH_BODY")" = fm-birds-eye-view.v1 ] || fail "last good view was replaced by something else"
  pass "a failed collection keeps serving the last good view"

  stop_server
}

# --- opening the dashboard ----------------------------------------------------

test_open_reports_outcome() {
  local home state fake old_path name
  home=$(new_dir open-home)
  state=$(new_dir open-state)
  fake=$(new_dir open-bin)
  for name in open xdg-open; do
    ln -s "$FIXTURES/stub-opener.sh" "$fake/$name"
  done
  use_stubs "$home" "$state"
  export FM_STUB_OPEN_RECORD="$state/open.log"

  old_path=$PATH
  PATH="$fake:$PATH"
  start_server opened --open
  PATH=$old_path
  wait_until 10 grep -Fq 'opened the dashboard' "$SERVER_OUT" || fail "--open did not report success"
  wait_until 10 grep -Fxq "http://127.0.0.1:$SERVER_PORT/" "$FM_STUB_OPEN_RECORD" \
    || fail "--open did not hand the dashboard URL to the opener"
  stop_server
  pass "--open hands the dashboard URL to the platform opener and reports it"

  export FM_STUB_OPEN_EXIT=1
  PATH="$fake:$PATH"
  start_server open-failed --open
  PATH=$old_path
  wait_until 10 grep -Fq 'open failed' "$SERVER_OUT" || fail "a failed opener was not reported"
  fetch /
  expect_fetch 200 "service after a failed opener"
  stop_server
  pass "a failed opener is reported and the service keeps running"
}

# --- shutdown -----------------------------------------------------------------

test_shutdown_stops_only_observer_children() {
  local home state grandchild status
  home=$(new_dir stop-home)
  state=$(new_dir stop-state)
  use_stubs "$home" "$state"
  export FM_STUB_HANG=1

  sleep 300 &
  SENTINEL_PID=$!
  start_server stopping --interval 5
  wait_until 20 test -s "$FM_STUB_PIDFILE" || fail "collection child never started"
  grandchild=$(cat "$FM_STUB_PIDFILE")

  kill -TERM "$SERVER_PID"
  wait_until 20 process_gone "$SERVER_PID" || fail "server did not exit after SIGTERM"
  wait "$SERVER_PID"
  status=$?
  SERVER_PID=""
  [ "$status" = 0 ] || fail "shutdown exited $status, not 0"
  grep -Fq 'birds-eye view stopped' "$SERVER_OUT" || fail "shutdown did not report completion"
  process_gone "$grandchild" || fail "the observer's collection child survived shutdown"
  ! process_gone "$SENTINEL_PID" || fail "shutdown stopped a process the observer does not own"
  pass "SIGTERM exits 0, stops the observer's collection child, and leaves other processes running"
}

test_refusals
test_snapshot
test_live_refused_without_model
test_live_contract
test_busy_port_is_refused
test_fixed_port_is_used
test_refresh_overlap_and_last_good
test_open_reports_outcome
test_shutdown_stops_only_observer_children

pass "birds-eye view CLI and server behavior checks passed"
