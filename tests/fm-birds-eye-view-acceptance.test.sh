#!/usr/bin/env bash
# Acceptance proofs for the bird's-eye live service and its CLI (DPES-798, AC7/T3 and T12).
#
#   1. Fake clock (portable): the real collector loop runs on a virtual clock, so cycle
#      start spacing, overlap, and stop behavior are exact and take no real time.
#   2. CLI refusals (portable): an interval below the floor exits 2 and starts nothing.
#   3. Network isolation and real smoke (macOS sandbox-exec; default-on, FM_LIVE/
#      FM_BIRDS_EYE_SANDBOX_PROOF=0 to skip, =1 to require): the actual CLI runs under a
#      bare deny of outbound network with a loopback allowance. A raw socket probe to a
#      public control target succeeds without the sandbox and is refused by it. Proxy
#      variables are not used anywhere in this proof, and the probe does not read them.
#      A read-safety check (T7) runs the snapshot under the same network profile plus a
#      read-deny profile over synthetic credential locations, in a minimal environment.
#   4. Non-interference (same sandbox gate): an owned stand-in process, not a live agent,
#      keeps running, beating, and unsignalled while the observer runs and stops.
#
# Every check reads an exit status or an observable output, never source bytes.
set -u

# shellcheck source=tests/lib.sh
# shellcheck disable=SC1091
. "$(dirname "${BASH_SOURCE[0]}")/lib.sh"

CLI="$ROOT/bin/fm-birds-eye-view.sh"
SERVE_FIXTURES="$ROOT/tests/fixtures/birds-eye/serve"
PROFILE="$SERVE_FIXTURES/loopback-only.sb"
PROBE="$SERVE_FIXTURES/egress-probe.py"
TMP_ROOT=$(fm_test_tmproot fm-birds-eye-view-acceptance)

command -v python3 >/dev/null 2>&1 || { echo "skip: python3 not found"; exit 0; }

# --- 1. fake-clock collector schedule ---------------------------------------

clock_out="$TMP_ROOT/clock.out"
clock_rc=0
python3 -I "$SERVE_FIXTURES/virtual-clock.py" "$ROOT" >"$clock_out" 2>&1 || clock_rc=$?
cat "$clock_out"
expect_code 0 "$clock_rc" "fake-clock collector proof"
clock_ok=$(grep -c '^ok - ' "$clock_out" || true)
if [ "$clock_ok" -ne 5 ] || grep -q '^not ok - ' "$clock_out"; then
  fail "fake-clock collector proof passed $clock_ok of 5 scenarios"
fi
pass "fake-clock collector: $clock_ok of 5 schedule scenarios hold (cadence, overlap, catch-up, stop)"

# --- 2. CLI refusals ----------------------------------------------------------

expect_refused() {
  local label=$1 code=0
  shift
  "$CLI" "$@" >"$TMP_ROOT/refused.out" 2>&1 </dev/null || code=$?
  expect_code 2 "$code" "$label"
}

REFUSED_SNAP="$TMP_ROOT/refused/snap.html"
expect_refused "live interval below the floor is refused" --interval 4
expect_refused "snapshot interval below the floor is refused" --interval 4 --snapshot "$REFUSED_SNAP"
expect_refused "snapshot with a port is refused" --snapshot "$REFUSED_SNAP" --port 8080
[ ! -e "$REFUSED_SNAP" ] || fail "a refused snapshot left a file behind"
pass "CLI refuses an interval below five seconds and a snapshot with a listener option, exit 2"

# --- 3. network isolation and real smoke (macOS sandbox) -----------------------

fm_live_gate default-on FM_BIRDS_EYE_SANDBOX_PROOF sandbox-exec curl

CONTROL_TARGET=${FM_BIRDS_EYE_EGRESS_TARGET:-1.1.1.1:443}
CONTROL_HOST=${CONTROL_TARGET%:*}
CONTROL_PORT=${CONTROL_TARGET##*:}

# Control: the probe must reach the target without the sandbox. Without a working control,
# a refusal under the sandbox would prove nothing, so an unreachable control fails the proof.
control_out=$(python3 -I "$PROBE" "$CONTROL_HOST" "$CONTROL_PORT" 2>&1)
control_rc=$?
printf 'evidence: control, no sandbox: %s rc=%s\n' "$control_out" "$control_rc"
[ "$control_rc" -eq 0 ] \
  || fail "control target $CONTROL_TARGET is unreachable without the sandbox; the isolation proof is inconclusive"

# Denied egress: the same target, same probe, under the profile, must be refused by the sandbox.
denied_out=$(/usr/bin/sandbox-exec -f "$PROFILE" python3 -I "$PROBE" "$CONTROL_HOST" "$CONTROL_PORT" 2>&1)
denied_rc=$?
printf 'evidence: egress under profile: %s rc=%s\n' "$denied_out" "$denied_rc"
expect_code 3 "$denied_rc" "egress to the control target under the profile"
case "$denied_out" in
  *'errno=1 PermissionError'*) ;;
  *) fail "egress under the profile failed for a reason other than sandbox denial: $denied_out" ;;
esac
pass "egress to $CONTROL_TARGET succeeds without the sandbox and is refused by PermissionError under it"

# Snapshot CLI under the profile: the actual CLI writes one file with the sandbox in force.
SNAP_HOME="$TMP_ROOT/snap-home"
mkdir -p "$SNAP_HOME"
snap_home_before=$(find "$SNAP_HOME" | sort)
SNAP="$TMP_ROOT/sandbox/snap.html"
snap_out="$TMP_ROOT/sandbox-snap.out"
snap_rc=0
FM_HOME="$SNAP_HOME" /usr/bin/sandbox-exec -f "$PROFILE" "$CLI" --snapshot "$SNAP" >"$snap_out" 2>&1 </dev/null || snap_rc=$?
cat "$snap_out"
expect_code 0 "$snap_rc" "snapshot CLI under the profile"
[ -s "$SNAP" ] || fail "snapshot CLI under the profile wrote no file"
grep -F -q "snapshot written: $SNAP" "$snap_out" || fail "snapshot CLI did not report its output"
grep -F -q "connect-src 'none'" "$SNAP" || fail "sandboxed snapshot lacks the no-connect policy"
[ "$(find "$SNAP_HOME" | sort)" = "$snap_home_before" ] || fail "sandboxed snapshot changed the configured home"
pass "snapshot CLI under the profile exits 0, writes one file, and leaves the configured home unchanged"

# --- 3b. read safety (T7) -------------------------------------------------------
# The credential locations are synthetic and live under this test's temporary root, so no
# real home directory is read. Each holds a sentinel: an unsandboxed read shows it, and a read
# under the read-deny profile must fail. The observer runs under the combined network and read
# profile, with a minimal environment whose HOME is the synthetic home. This shows the observer
# completes without those paths; it does not record every file the observer reads.
T7_HOME="$TMP_ROOT/t7-home"
T7_SENTINEL="fm-t7-sentinel-$$"
T7_CREDS=(
  "$T7_HOME/.ssh/id_test"
  "$T7_HOME/.config/gh/hosts.yml"
  "$T7_HOME/.docker/config.json"
  "$T7_HOME/.netrc"
)
mkdir -p "$T7_HOME/.ssh" "$T7_HOME/.config/gh" "$T7_HOME/.docker"
for cred in "${T7_CREDS[@]}"; do
  printf '%s\n' "$T7_SENTINEL" >"$cred"
done
T7_OK="$TMP_ROOT/t7-ordinary.txt"
printf 'ordinary\n' >"$T7_OK"

# Seatbelt matches resolved paths, so the parameters name the real directories.
T7_REAL=$(cd "$T7_HOME" && pwd -P)
T7_PARAMS=(
  "-DHOME_SSH=$T7_REAL/.ssh"
  "-DHOME_GH=$T7_REAL/.config/gh"
  "-DHOME_DOCKER=$T7_REAL/.docker"
  "-DHOME_NETRC=$T7_REAL/.netrc"
)
T7_READ_PROFILE="$SERVE_FIXTURES/read-deny.sb"
T7_PROFILE="$TMP_ROOT/t7-combined.sb"
{
  printf '(version 1)\n'
  grep -v '^(version 1)' "$PROFILE"
  grep -v '^(version 1)' "$T7_READ_PROFILE"
} >"$T7_PROFILE"

# Control: the read-deny profile still allows an ordinary read, so a denial below is specific.
ctl_out=$(/usr/bin/sandbox-exec "${T7_PARAMS[@]}" -f "$T7_READ_PROFILE" /bin/cat "$T7_OK" 2>&1)
ctl_rc=$?
expect_code 0 "$ctl_rc" "ordinary read under the read-deny profile"
[ "$ctl_out" = 'ordinary' ] || fail "read control returned unexpected output: $ctl_out"

# Positive control: without the profile, each credential location yields its sentinel.
for cred in "${T7_CREDS[@]}"; do
  [ "$(cat "$cred")" = "$T7_SENTINEL" ] || fail "unsandboxed read of $cred did not return the sentinel; the denial would prove nothing"
done

# Denied reads: under the profile each credential location fails with EPERM and yields no sentinel.
for cred in "${T7_CREDS[@]}"; do
  d_out=$(/usr/bin/sandbox-exec "${T7_PARAMS[@]}" -f "$T7_READ_PROFILE" /bin/cat "$cred" 2>&1)
  d_rc=$?
  [ "$d_rc" -ne 0 ] || fail "read of $cred succeeded under the read-deny profile"
  case "$d_out" in
    *'Operation not permitted'*) ;;
    *) fail "read of $cred failed for a reason other than sandbox denial: $d_out" ;;
  esac
  case "$d_out" in
    *"$T7_SENTINEL"*) fail "read of $cred leaked its sentinel under the profile" ;;
  esac
done

# The observer under the combined profile. Its tools are found in a minimal PATH; the model
# needs jq 1.8 syntax, so the PATH puts that jq first.
T7_FM="$TMP_ROOT/t7-fleet"
T7_OUT_DIR="$TMP_ROOT/t7-out"
T7_SNAP="$T7_OUT_DIR/snap.html"
T7_OBS_OUT="$TMP_ROOT/t7-observer.out"
T7_OBS_ERR="$TMP_ROOT/t7-observer.err"
mkdir -p "$T7_FM" "$T7_OUT_DIR"
t7_fm_before=$(find "$T7_FM" | sort)
JQ_BIN=$(command -v jq) || fail "jq is not on PATH for the read-safety run"
JQ_VERSION=$("$JQ_BIN" --version 2>/dev/null)
case "$JQ_VERSION" in
  jq-1.8*) ;;
  *) fail "read-safety run needs jq 1.8 for the model's syntax; found '$JQ_VERSION' at $JQ_BIN" ;;
esac
obs_rc=0
env -i HOME="$T7_HOME" PATH="$(dirname "$JQ_BIN"):/usr/bin:/bin" FM_HOME="$T7_FM" \
  /usr/bin/sandbox-exec "${T7_PARAMS[@]}" -f "$T7_PROFILE" "$CLI" --snapshot "$T7_SNAP" \
  >"$T7_OBS_OUT" 2>"$T7_OBS_ERR" </dev/null || obs_rc=$?
expect_code 0 "$obs_rc" "snapshot under the combined network and read-deny profile"
[ -s "$T7_SNAP" ] || fail "snapshot under the read-deny profile wrote no file; stderr: $(head -c 400 "$T7_OBS_ERR")"
grep -F -q "snapshot written: $T7_SNAP" "$T7_OBS_OUT" || fail "snapshot under the read-deny profile did not report its output"
grep -F -q 'fm-birds-eye-view.v1' "$T7_SNAP" || fail "snapshot under the read-deny profile lacks the view schema"
if grep -F -q "$T7_SENTINEL" "$T7_OBS_OUT" "$T7_OBS_ERR" "$T7_SNAP"; then
  fail "a sentinel from a credential location appeared in the observer's output"
fi
[ "$(find "$T7_FM" | sort)" = "$t7_fm_before" ] || fail "the snapshot under the read-deny profile changed its configured home"

printf 'evidence: read-safety control=ok denied=%s of %s (Operation not permitted, no sentinel) observer_rc=%s sentinel_in_output=none configured_home_unchanged=yes\n' \
  "${#T7_CREDS[@]}" "${#T7_CREDS[@]}" "$obs_rc"
pass "read safety: an ordinary read succeeds under the read-deny profile, all ${#T7_CREDS[@]} synthetic credential locations are refused, and the observer completes without them"

# Live smoke under the profile. One shell owns the observer: its PID is $!, and its exit
# status comes from wait in this same shell. Descendants are read from the process table
# below that PID only, never by name.
LIVE_HOME="$TMP_ROOT/live-home"
LIVE_OUT="$TMP_ROOT/live.out"
LIVE_ERR="$TMP_ROOT/live.err"
LIVE_DESC="$TMP_ROOT/live-descendants.txt"
mkdir -p "$LIVE_HOME"
live_home_before=$(find "$LIVE_HOME" | sort)
live_pid=""
standin_pid=""

# shellcheck disable=SC2329 # Registered by the EXIT trap below.
stop_observer() {
  if [ -n "$live_pid" ] && kill -0 "$live_pid" 2>/dev/null; then
    kill -TERM "$live_pid" 2>/dev/null
    wait "$live_pid" 2>/dev/null
  fi
}

# shellcheck disable=SC2329 # Registered by the EXIT trap below.
stop_standin() {
  if [ -n "$standin_pid" ] && kill -0 "$standin_pid" 2>/dev/null; then
    kill -KILL "$standin_pid" 2>/dev/null
    wait "$standin_pid" 2>/dev/null
  fi
}
trap 'stop_observer; stop_standin; fm_test_cleanup' EXIT

# descendants <pid>: "<pid> <name>" for every process below <pid>, depth first. Names are
# captured here, while the processes live; the collector's children are short-lived.
descendants() {
  local parent=$1 child name
  while read -r child name; do
    printf '%s %s\n' "$child" "$name"
    descendants "$child"
  done < <(ps -A -o pid= -o ppid= -o comm= | awk -v p="$parent" '$2 == p { n = $3; sub(/.*\//, "", n); print $1, n }')
}

FM_HOME="$LIVE_HOME" /usr/bin/sandbox-exec -f "$PROFILE" "$CLI" --interval 5 >"$LIVE_OUT" 2>"$LIVE_ERR" </dev/null &
live_pid=$!

url=""
for _ in $(seq 1 300); do
  url=$(grep '^birds-eye view: ' "$LIVE_OUT" 2>/dev/null | head -n 1)
  url=${url#birds-eye view: }
  [ -n "$url" ] && break
  kill -0 "$live_pid" 2>/dev/null || break
  sleep 0.1
done
[ -n "$url" ] || fail "live service under the profile printed no URL; stderr: $(head -c 400 "$LIVE_ERR")"

api_code=000
for _ in $(seq 1 300); do
  api_code=$(curl --noproxy '*' -s --max-time 2 -o "$TMP_ROOT/api-view.json" -w '%{http_code}' "${url}api/view" || true)
  [ "$api_code" = 200 ] && break
  kill -0 "$live_pid" 2>/dev/null || break
  sleep 0.1
done
[ "$api_code" = 200 ] || fail "live /api/view under the profile returned $api_code, expected 200"
root_code=$(curl --noproxy '*' -s --max-time 2 -o /dev/null -w '%{http_code}' "$url" || true)
expect_code 200 "$root_code" "live dashboard page under the profile"
jq -e '.schema == "fm-birds-eye-view.v1"' "$TMP_ROOT/api-view.json" >/dev/null 2>&1 \
  || fail "live /api/view did not return an fm-birds-eye-view.v1 document"

# Sample the observer's descendants while the collector runs; every cycle spawns the model.
: >"$LIVE_DESC"
for _ in $(seq 1 100); do
  descendants "$live_pid" >>"$LIVE_DESC"
  kill -0 "$live_pid" 2>/dev/null || break
  sleep 0.1
done
sort -u "$LIVE_DESC" -o "$LIVE_DESC"
descendant_count=$(grep -c . "$LIVE_DESC" || true)
descendant_names=$(awk '{ print $2 }' "$LIVE_DESC" | sort -u | tr '\n' ' ')

# Sandbox in force for the live server, at the same moment: loopback reaches the service,
# and the external control target is refused.
loopback_port=${url#http://127.0.0.1:}
loopback_port=${loopback_port%/}
loopback_out=$(/usr/bin/sandbox-exec -f "$PROFILE" python3 -I "$PROBE" 127.0.0.1 "$loopback_port" 2>&1)
loopback_rc=$?
expect_code 0 "$loopback_rc" "loopback reaches the live service under the profile ($loopback_out)"
external_out=$(/usr/bin/sandbox-exec -f "$PROFILE" python3 -I "$PROBE" "$CONTROL_HOST" "$CONTROL_PORT" 2>&1)
external_rc=$?
expect_code 3 "$external_rc" "external egress refused while the live service runs"

# Stop the observer. Its exit status is recorded in this shell.
kill -TERM "$live_pid"
live_rc=0
wait "$live_pid" || live_rc=$?
live_pid_was=$live_pid
live_pid=""

stopped_line=$(tail -n 1 "$LIVE_OUT")
[ "$stopped_line" = 'birds-eye view stopped' ] || fail "live stdout did not end with the stop line: $stopped_line"
expect_code 0 "$live_rc" "live observer exit status after SIGTERM"

# Ownership: every recorded descendant must be gone once the observer has exited.
gone=yes
for _ in $(seq 1 30); do
  gone=yes
  while read -r p _; do
    [ -z "$p" ] && continue
    if kill -0 "$p" 2>/dev/null; then
      gone=no
    fi
  done <"$LIVE_DESC"
  [ "$gone" = yes ] && break
  sleep 0.1
done
[ "$gone" = yes ] || fail "a descendant of the live observer survived its exit"

working_line=$(grep '^working files: ' "$LIVE_OUT" || true)
working_dir=${working_line#working files: }
working_dir=${working_dir% (removed on exit)}
[ -n "$working_dir" ] && [ ! -e "$working_dir" ] || fail "live working files survived exit: $working_dir"

after_stop_code=$(curl --noproxy '*' -s --max-time 2 -o /dev/null -w '%{http_code}' "$url" || true)
expect_code 000 "$after_stop_code" "dashboard closed after stop"
[ "$(find "$LIVE_HOME" | sort)" = "$live_home_before" ] || fail "live run changed the configured home"

printf 'evidence: live observer pid=%s interval=5 exit_status=%s stop_line=%s\n' \
  "$live_pid_was" "$live_rc" "$stopped_line"
printf 'evidence: live api_view=%s root_page=%s descendants_observed=%s names=%s all_gone_after_exit=%s\n' \
  "$api_code" "$root_code" "$descendant_count" "$descendant_names" "$gone"
printf 'evidence: live loopback_probe=[%s] external_probe=[%s] working_files_removed=yes configured_home_unchanged=yes\n' \
  "$loopback_out" "$external_out"
pass "live service under the profile: exit $live_rc after SIGTERM, $descendant_count descendants observed and all gone, working files removed"

# --- 4. non-interference with an owned stand-in session (AC-17, T10) ----------------------

# The stand-in is a process this test starts and owns. It is not a live agent. The observer
# must leave it running, beating, and unsignalled through its run and its stop. A positive
# control first confirms that the signal record catches a signal sent to the stand-in.
NI_STANDIN="$SERVE_FIXTURES/standin-agent.py"
NI_BEAT="$TMP_ROOT/standin.beat"
NI_SIG="$TMP_ROOT/standin.signals"
NI_HOME="$TMP_ROOT/ni-home"
NI_OUT="$TMP_ROOT/ni.out"
NI_ERR="$TMP_ROOT/ni.err"
mkdir -p "$NI_HOME"

python3 -I "$NI_STANDIN" "$NI_BEAT" "$NI_SIG" </dev/null >/dev/null 2>&1 &
standin_pid=$!

# beat_now: the stand-in's current heartbeat, or 0 before its first beat.
beat_now() {
  local beat
  beat=$(cat "$NI_BEAT" 2>/dev/null || true)
  printf '%s\n' "${beat:-0}"
}

# advanced_past <value>: wait until the heartbeat is greater than <value>, up to five seconds.
advanced_past() {
  local _
  for _ in $(seq 1 50); do
    [ "$(beat_now)" -gt "$1" ] && return 0
    sleep 0.1
  done
  return 1
}

advanced_past 0 || fail "the stand-in owned by this test never beat; the non-interference proof is inconclusive"
kill -0 "$standin_pid" 2>/dev/null || fail "the stand-in exited before the observer started"

kill -USR1 "$standin_pid"
for _ in $(seq 1 50); do
  [ -s "$NI_SIG" ] && break
  sleep 0.1
done
[ -s "$NI_SIG" ] || fail "control: the stand-in did not record a signal sent to it, so the no-signal check would prove nothing"
: >"$NI_SIG"

beat_before=$(beat_now)
stat_before=$(ps -o stat= -p "$standin_pid" | tr -d ' ')
command_before=$(ps -o command= -p "$standin_pid")
case "$command_before" in
  *standin-agent.py*) ;;
  *) fail "the recorded stand-in process is not the test's stand-in: $command_before" ;;
esac

FM_HOME="$NI_HOME" /usr/bin/sandbox-exec -f "$PROFILE" "$CLI" --interval 5 >"$NI_OUT" 2>"$NI_ERR" </dev/null &
live_pid=$!
ni_url=""
for _ in $(seq 1 300); do
  ni_url=$(grep '^birds-eye view: ' "$NI_OUT" 2>/dev/null | head -n 1)
  [ -n "$ni_url" ] && break
  kill -0 "$live_pid" 2>/dev/null || break
  sleep 0.1
done
[ -n "$ni_url" ] || fail "observer for the non-interference run printed no URL; stderr: $(head -c 400 "$NI_ERR")"
advanced_past "$beat_before" || fail "the stand-in stopped beating while the observer was starting"
beat_running=$(beat_now)
signals_running=$(grep -c . "$NI_SIG" || true)

kill -TERM "$live_pid"
ni_rc=0
wait "$live_pid" || ni_rc=$?
live_pid=""
expect_code 0 "$ni_rc" "observer exit status for the non-interference run"
[ "$(tail -n 1 "$NI_OUT")" = 'birds-eye view stopped' ] || fail "non-interference run did not print its stop line"

advanced_past "$beat_running" || fail "the stand-in stopped beating after the observer stopped"
beat_after=$(beat_now)
stat_after=$(ps -o stat= -p "$standin_pid" | tr -d ' ')
signals_after=$(grep -c . "$NI_SIG" || true)
kill -0 "$standin_pid" 2>/dev/null || fail "the observer's stop ended the owned stand-in"
case "$stat_after" in
  T*) fail "the observer's run left the owned stand-in stopped (state $stat_after)" ;;
esac
[ "$signals_running" -eq 0 ] && [ "$signals_after" -eq 0 ] \
  || fail "the observer sent a signal to the owned stand-in ($signals_running while running, $signals_after after stop)"

printf 'evidence: non-interference stand-in (owned test process, not a live agent) beats before=%s during=%s after=%s state_before=%s state_after=%s signals=%s observer_exit=%s\n' \
  "$beat_before" "$beat_running" "$beat_after" "$stat_before" "$stat_after" "$((signals_running + signals_after))" "$ni_rc"
pass "an owned stand-in keeps running, beating, and unsignalled through the observer's run and stop"

# Stop the owned stand-in only, by the PID this test recorded.
kill -KILL "$standin_pid" 2>/dev/null || true
wait "$standin_pid" 2>/dev/null || true
standin_pid=""

exit 0
