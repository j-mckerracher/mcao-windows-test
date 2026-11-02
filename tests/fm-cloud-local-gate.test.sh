#!/usr/bin/env bash
# Behavior tests for the cloud/local gate on every local-only path: the watcher's
# window enumeration (which feeds steering doorbells, stuck-pane inference, key
# presses, and endpoint probes), steering sends, and lifecycle control. A cloud
# run, or a record whose execution target is not recognised, must never reach a
# local endpoint, even when its record carries a window= that matches a live pane.
set -u

# shellcheck source=tests/lib.sh
# shellcheck disable=SC1091
. "$(dirname "${BASH_SOURCE[0]}")/lib.sh"

WATCH="$ROOT/bin/fm-watch.sh"
SEND="$ROOT/bin/fm-send.sh"
CONTROL="$ROOT/bin/fm-control.sh"
TMP_ROOT=$(fm_test_tmproot fm-cloud-local-gate)

command -v jq >/dev/null 2>&1 || { echo "skip: jq not found"; exit 0; }

# tmux shim that logs every invocation and succeeds, so a send or a lifecycle
# action that reaches the local endpoint is visible in the log.
logging_tmux() {  # <dir> <log> -> echoes the fake bin directory
  local fb
  fb=$(fm_fakebin "$1")
  cat > "$fb/tmux" <<SH
#!/usr/bin/env bash
printf '%s\n' "\$*" >> "$2"
exit 0
SH
  chmod +x "$fb/tmux"
  printf '%s\n' "$fb"
}

# Seed a home with one local task, one cloud run whose window would match a live
# pane, and one record whose execution target is not recognised.
seed_mixed_home() {  # <home>
  local state=$1/state
  mkdir -p "$state"
  fm_write_meta "$state/local-task.meta" \
    "window=firstmate:fm-local-task" "kind=ship" "harness=claude" "mode=no-mistakes"
  fm_write_meta "$state/cloud-task.meta" \
    "window=firstmate:fm-cloud-task" "kind=ship" "harness=claude" "mode=no-mistakes" \
    "execution_target=gcp" "cloud_request_id=req-cloud-task" "cloud_run_id=run-cloud-task"
  fm_write_meta "$state/odd-task.meta" \
    "window=firstmate:fm-odd-task" "kind=ship" "harness=claude" "mode=no-mistakes" \
    "execution_target=elsewhere"
}

# The watcher's window enumeration is the one choke point for the local-only
# probes. Sourcing the real watcher loads its functions through its own guard.
test_watcher_enumerates_only_local_windows() {
  local home state windows
  home=$TMP_ROOT/enumerate
  seed_mixed_home "$home"
  state=$home/state
  windows=$(FM_STATE_OVERRIDE="$state" FM_HOME="$home" bash -c '
    # shellcheck disable=SC1090
    . "$1"
    recorded_windows
  ' _ "$WATCH") || fail "recorded_windows could not be read from the real watcher"
  assert_equals "firstmate:fm-local-task" "$windows" \
    "recorded_windows must yield only the local window; a cloud run or an unrecognised target must not reach a probe"
  pass "the watcher probes only local endpoints: cloud and unrecognised records contribute no window"
}

# A steering send to a cloud run is refused before anything is typed into a pane.
test_steering_send_refuses_cloud_run_without_typing() {
  local home fb log out rc
  home=$TMP_ROOT/send
  seed_mixed_home "$home"
  log=$home/tmux.log
  fb=$(logging_tmux "$home" "$log")
  out=$(PATH="$fb:$PATH" FM_HOME="$home" "$SEND" cloud-task "steer the cloud run" 2>&1); rc=$?
  [ "$rc" -ne 0 ] || fail "a steering send to a cloud run must be refused: $out"
  assert_contains "$out" "'steer' is not supported for task cloud-task on execution target gcp" "the refusal must say why nothing was sent"
  assert_contains "$out" "no local fallback runs" "the refusal must say nothing ran locally"
  if [ -f "$log" ] && grep -q 'send-keys' "$log"; then
    fail "a steering send to a cloud run typed keys into a local pane: $(cat "$log")"
  fi
  pass "a steering send to a cloud run is refused and types nothing"
}

# A key press on a cloud run is refused as key input, not as steering, before any
# key reaches a pane. The refusal names the operation that was actually attempted.
test_key_send_refuses_cloud_run_without_typing() {
  local home fb log out rc
  home=$TMP_ROOT/send-key
  seed_mixed_home "$home"
  log=$home/tmux.log
  fb=$(logging_tmux "$home" "$log")
  out=$(PATH="$fb:$PATH" FM_HOME="$home" "$SEND" cloud-task --key Enter 2>&1); rc=$?
  [ "$rc" -ne 0 ] || fail "a key send to a cloud run must be refused: $out"
  assert_contains "$out" "'key' is not supported for task cloud-task on execution target gcp" "the refusal must name key input as the refused operation"
  if [ -f "$log" ] && grep -q "send-keys" "$log"; then
    fail "a key send to a cloud run reached a local pane: $(cat "$log")"
  fi
  pass "a key send to a cloud run is refused as key input and types nothing"
}

# A lifecycle action on a cloud run is refused before any local endpoint is signalled.
test_lifecycle_control_refuses_cloud_run_without_signalling() {
  local home fb log out rc action
  home=$TMP_ROOT/control
  seed_mixed_home "$home"
  log=$home/tmux.log
  fb=$(logging_tmux "$home" "$log")
  for action in interrupt exit relaunch attach; do
    out=$(PATH="$fb:$PATH" FM_HOME="$home" "$CONTROL" cloud-task "$action" 2>&1); rc=$?
    [ "$rc" -ne 0 ] || fail "$action on a cloud run must be refused: $out"
    assert_contains "$out" "'$action' is not supported for task cloud-task on execution target gcp" "$action refusal must say why nothing was applied"
  done
  if [ -f "$log" ] && grep -q 'fm-cloud-task' "$log"; then
    fail "a lifecycle action reached a cloud run's window: $(cat "$log")"
  fi
  pass "interrupt, exit, relaunch, and attach on a cloud run are refused and signal nothing"
}

test_watcher_enumerates_only_local_windows
test_steering_send_refuses_cloud_run_without_typing
test_key_send_refuses_cloud_run_without_typing
test_lifecycle_control_refuses_cloud_run_without_signalling
