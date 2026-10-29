#!/usr/bin/env bash
# tests/fm-parent-channel-scan-exclusion.test.sh - a parent-replies.status in an
# ordinary task home (a main home, or a local mate whose parent channel lives in
# the parent home) is an ordinary task log: it keeps folding and waking. Remote
# secondmate placement was removed, so no home's own status scans exclude a
# channel file.
#
# Covers the watcher scan (scan_signals) and the away-mode daemon's twin
# catch-all scan (fm-supervise-daemon.sh housekeeping), and fm-classify-lib.sh's
# fleet-wide fold, against main-home and local-mate fixtures.
set -u

# shellcheck source=tests/lib.sh
. "$(dirname "${BASH_SOURCE[0]}")/lib.sh"

TMP_ROOT=$(fm_test_tmproot fm-parent-channel-scan-exclusion)
mkdir -p "$TMP_ROOT"
TMP_ROOT=$(cd "$TMP_ROOT" && pwd -P)

# The real drain asserts watcher liveness through fm-guard.sh, whose tangle
# check warns when FM_ROOT sits on a feature branch; point it at a fresh
# non-git dir so the banner stays inert in this disposable worktree (the same
# trick tests/wake-helpers.sh installs for the drain suites).
FM_ROOT_OVERRIDE="$(fm_test_tmproot fm-parent-channel-scan-exclusion-root)"
export FM_ROOT_OVERRIDE
mkdir -p "$FM_ROOT_OVERRIDE"

cleanup() { rm -rf -- "$TMP_ROOT"; }
trap cleanup EXIT

# seed_plain_home <dir>: a main home (no secondmate identity marker) whose
# state dir carries a parent-replies.status that merely shares the name.
seed_plain_home() {  # <dir>
  local dir=$1
  mkdir -p "$dir/state"
  printf 'needs-decision [key=name-only]: an ordinary task file that shares the name\n' \
    > "$dir/state/parent-replies.status"
  printf 'needs-decision [key=other-task]: a genuine sibling task decision\n' \
    > "$dir/state/other-task.status"
}

# seed_local_mate <dir> <parent-home>: a LOCAL mate home - its parent channel
# lives in the parent home's state/<id>.status, so a parent-replies.status in
# its own state dir is an ordinary self-home file.
seed_local_mate() {  # <dir> <parent-home>
  local dir=$1 parent_home=$2
  mkdir -p "$dir/state"
  printf '%s\n' mate > "$dir/.fm-secondmate-home"
  printf 'schema=fm-secondmate-parent.v1\nroute=local\nparent_home=%s\n' "$parent_home" \
    > "$dir/.fm-secondmate-parent"
  printf 'needs-decision [key=local-shape]: still an ordinary self-home file\n' \
    > "$dir/state/parent-replies.status"
}

PLAIN="$TMP_ROOT/main-home"
LOCAL_MATE="$TMP_ROOT/local-mate"
seed_plain_home "$PLAIN"
seed_local_mate "$LOCAL_MATE" "$PLAIN"
PLAIN_STATE="$PLAIN/state"
LOCAL_STATE="$LOCAL_MATE/state"

# --- unit: the fleet-wide folds keep name-shared files in ordinary homes -----

test_name_shared_file_folds_in_a_main_home() {
  local out
  out=$(FM_TEST_LIB_SOURCED=1 bash -c '
    # shellcheck source=bin/fm-classify-lib.sh
    . "$1/bin/fm-classify-lib.sh"
    scan_open_decisions "$2"
  ' _ "$ROOT" "$PLAIN_STATE") || fail "the main-home fold failed"
  printf '%s\n' "$out" | grep -F 'name-only' >/dev/null \
    || fail "a main home's parent-replies.status must keep folding as an ordinary task: $out"
  printf '%s\n' "$out" | grep -F 'other-task' >/dev/null \
    || fail "the sibling task decision must keep folding: $out"
  pass "a parent-replies.status in a main home still folds"
}

test_name_shared_file_folds_in_a_local_mate() {
  local out
  out=$(FM_TEST_LIB_SOURCED=1 bash -c '
    # shellcheck source=bin/fm-classify-lib.sh
    . "$1/bin/fm-classify-lib.sh"
    scan_open_decisions "$2"
  ' _ "$ROOT" "$LOCAL_STATE") || fail "the local-mate fold failed"
  printf '%s\n' "$out" | grep -F 'local-shape' >/dev/null \
    || fail "a local mate's parent-replies.status must keep folding: $out"
  pass "a parent-replies.status in a local mate still folds"
}

# --- unit: the watcher's signal scan and heartbeat backstop -----------------

# Source the watcher once with an isolated state/home; its source guard returns
# before the lock/loop, so only the functions load. scan_signals and
# heartbeat_scan_finds_actionable read STATE at call time. FM_ROOT_OVERRIDE
# stays at the inert dir set above; the unit-called functions read STATE, not
# the repo root.
WATCH_STATE="$PLAIN_STATE"
export FM_STATE_OVERRIDE="$WATCH_STATE"
export FM_HOME="$PLAIN"
# Production modules are independently linted canonical roots. Keep this test's
# ShellCheck context local while preserving its unchanged runtime source path.
# shellcheck source=/dev/null
. "$ROOT/bin/fm-watch.sh"

test_watcher_scan_keeps_name_shared_files_in_ordinary_homes() {
  local out
  STATE="$PLAIN_STATE"
  out=$(scan_signals) || fail "scan_signals failed over the main-home state"
  printf '%s\n' "$out" | cut -f3 | grep -F 'parent-replies.status' >/dev/null \
    || fail "a main home's parent-replies.status must keep waking: $out"
  # shellcheck disable=SC2034 # read by the sourced watcher's scans at call time
  STATE="$LOCAL_STATE"
  out=$(scan_signals) || fail "scan_signals failed over the local-mate state"
  printf '%s\n' "$out" | cut -f3 | grep -F 'parent-replies.status' >/dev/null \
    || fail "a local mate's parent-replies.status must keep waking: $out"
  pass "scan_signals keeps parent-replies.status in main and local homes"
}

# --- unit: the away-mode daemon's heartbeat catch-all backstop --------------

# The daemon runs the watcher's twin catch-all scan while a home is away, so it
# Source it in a subshell - its BASH_SOURCE guard
# skips the main loop, and the isolation keeps its function table from
# colliding with the watcher already sourced above.
daemon_heartbeat_scan() {  # <home>
  local home=$1
  rm -f "$home/state/.subsuper-last-scan"
  FM_TEST_LIB_SOURCED=1 FM_HOME="$home" FM_STATE_OVERRIDE="$home/state" \
    bash -c '
      # shellcheck source=/dev/null
      . "$1/bin/fm-supervise-daemon.sh"
      housekeeping "$2"
    ' _ "$ROOT" "$home/state" >/dev/null 2>&1
}

test_daemon_heartbeat_backstop_keeps_name_shared_file_in_a_main_home() {
  local dir buffer
  dir="$TMP_ROOT/daemon-heartbeat-main"
  seed_plain_home "$dir/home"
  printf 'blocked [key=name-only]: an ordinary task file that shares the name\n' \
    > "$dir/home/state/parent-replies.status"
  daemon_heartbeat_scan "$dir/home"
  buffer=$(cat "$dir/home/state/.subsuper-escalations" 2>/dev/null || true)
  printf '%s\n' "$buffer" | grep -F 'parent-replies.status' >/dev/null \
    || fail "a main home's parent-replies.status must keep reaching the daemon backstop: $buffer"
  pass "the daemon's catch-all scan keeps parent-replies.status in a main home"
}

test_name_shared_file_folds_in_a_main_home
test_name_shared_file_folds_in_a_local_mate
test_watcher_scan_keeps_name_shared_files_in_ordinary_homes
test_daemon_heartbeat_backstop_keeps_name_shared_file_in_a_main_home
