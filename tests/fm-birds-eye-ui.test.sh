#!/usr/bin/env bash
# Behavior tests for the birds-eye observer dashboard (assets/birds-eye-view/,
# DPES-798). The page runs in a real Chrome against a local stub that serves the
# assets and the fm-birds-eye-view.v1 and fm-birds-eye-capture.v1 endpoints; the
# scenarios live in tests/fixtures/birds-eye/ui/ui-harness.mjs. Assertions are on
# rendered text, DOM state, network requests, and timings, never on source text.
#
# Chrome is found through FM_BIRDS_EYE_UI_CHROME, then the standard install
# paths. FM_BIRDS_EYE_UI=0 skips. FM_BIRDS_EYE_UI=1 turns a missing Chrome or
# Node.js into a failure, so a run that checked nothing cannot pass. Without it,
# a missing tool prints a "skip:" line that the runner records as a gate skip.
set -u

# shellcheck source=tests/lib.sh
# shellcheck disable=SC1091
. "$(dirname "${BASH_SOURCE[0]}")/lib.sh"

HARNESS="$ROOT/tests/fixtures/birds-eye/ui/ui-harness.mjs"
ASSETS="$ROOT/assets/birds-eye-view"
TMP_ROOT=$(fm_test_tmproot fm-birds-eye-ui)

find_chrome() {
  local candidate
  if [ -n "${FM_BIRDS_EYE_UI_CHROME:-}" ]; then
    if [ -x "$FM_BIRDS_EYE_UI_CHROME" ]; then
      printf '%s\n' "$FM_BIRDS_EYE_UI_CHROME"
      return 0
    fi
    return 1
  fi
  for candidate in \
    "/Applications/Google Chrome.app/Contents/MacOS/Google Chrome" \
    "/Applications/Chromium.app/Contents/MacOS/Chromium"; do
    if [ -x "$candidate" ]; then
      printf '%s\n' "$candidate"
      return 0
    fi
  done
  for candidate in google-chrome google-chrome-stable chromium chromium-browser; do
    if command -v "$candidate" >/dev/null 2>&1; then
      command -v "$candidate"
      return 0
    fi
  done
  return 1
}

mode=${FM_BIRDS_EYE_UI:-}
case "$mode" in
  0)
    printf 'skip: birds-eye UI: disabled by FM_BIRDS_EYE_UI=0\n'
    exit 0
    ;;
  '' | 1) ;;
  *) fail "FM_BIRDS_EYE_UI must be 0 or 1, got '$mode'" ;;
esac

missing=''
if ! command -v node >/dev/null 2>&1; then
  missing='Node.js'
elif ! node -e 'process.exit(typeof WebSocket === "function" && typeof fetch === "function" ? 0 : 1)' >/dev/null 2>&1; then
  missing='Node.js 22 or later (global WebSocket and fetch)'
fi

# A browser that cannot start must fail the harness and let it exit: the stub
# server keeps the process alive unless the harness closes it. `false` exits at
# once, so launching it as Chrome is a startup failure that needs no real browser.
if [ -z "$missing" ]; then
  broken_chrome=$(command -v false) || fail "no 'false' executable to stand in for a broken browser"
  node "$HARNESS" --chrome "$broken_chrome" --assets "$ASSETS" --tmp "$TMP_ROOT/startup-failure" \
    >"$TMP_ROOT/startup-failure.out" 2>&1 &
  harness_pid=$!
  waited=0
  while kill -0 "$harness_pid" 2>/dev/null && [ "$waited" -lt 150 ]; do
    sleep 0.1
    waited=$((waited + 1))
  done
  if kill -0 "$harness_pid" 2>/dev/null; then
    kill "$harness_pid" 2>/dev/null
    wait "$harness_pid" 2>/dev/null
    fail "harness kept running after the browser failed to start"
  fi
  startup_rc=0
  wait "$harness_pid" || startup_rc=$?
  [ "$startup_rc" -ne 0 ] || fail "harness exited 0 after the browser failed to start"
  grep -q '^not ok - harness: ' "$TMP_ROOT/startup-failure.out" \
    || fail "harness did not report the browser startup failure: $(cat "$TMP_ROOT/startup-failure.out")"
fi

chrome=''
if [ -z "$missing" ]; then
  chrome=$(find_chrome) || chrome=''
  [ -n "$chrome" ] || missing='Chrome or Chromium (set FM_BIRDS_EYE_UI_CHROME to its path)'
fi

if [ -n "$missing" ]; then
  if [ "$mode" = 1 ]; then
    fail "FM_BIRDS_EYE_UI=1 requires $missing"
  fi
  printf 'skip: birds-eye UI: %s not found\n' "$missing"
  exit 0
fi

out="$TMP_ROOT/harness.out"
rc=0
node "$HARNESS" --chrome "$chrome" --assets "$ASSETS" --tmp "$TMP_ROOT/run" >"$out" 2>&1 || rc=$?
cat "$out"
[ "$rc" -eq 0 ] || fail "birds-eye UI harness exited $rc"

ok_count=$(grep -c '^ok - ' "$out" || true)
summary=$(grep -E '^# [0-9]+ of [0-9]+ scenarios passed$' "$out" || true)
[[ "$summary" =~ ^#\ ([0-9]+)\ of\ ([0-9]+)\ scenarios ]] \
  || fail "birds-eye UI harness printed no scenario summary"
passed=${BASH_REMATCH[1]}
total=${BASH_REMATCH[2]}
[ "$total" -gt 0 ] && [ "$passed" -eq "$total" ] && [ "$ok_count" -eq "$total" ] \
  || fail "birds-eye UI harness passed $ok_count of $total scenarios"

pass "birds-eye dashboard: $total browser scenarios passed in $chrome"

# The exported snapshot is the artifact a reader opens offline, so it gets its own browser
# proof: the real exporter writes the hostile fixture, and real Chrome runs the driver over
# that file:// page with no server.
EXPORT_SNAP="$TMP_ROOT/export/snap.html"
mkdir -p "$TMP_ROOT/export/run"
bash "$ROOT/bin/fm-birds-eye-export.sh" --fixture-dir "$ROOT/tests/fixtures/birds-eye/export" \
  --now 1792300000 --output "$EXPORT_SNAP" >"$TMP_ROOT/export/export.out" 2>&1 \
  || fail "birds-eye exporter could not write the snapshot for the browser proof"
export_out="$TMP_ROOT/export/driver.out"
export_rc=0
node "$ROOT/tests/fixtures/birds-eye/export/export-ui.mjs" --chrome "$chrome" --snapshot "$EXPORT_SNAP" \
  --tmp "$TMP_ROOT/export/run" >"$export_out" 2>&1 || export_rc=$?
cat "$export_out"
[ "$export_rc" -eq 0 ] || fail "exported snapshot browser proof exited $export_rc"

export_ok=$(grep -c '^ok - ' "$export_out" || true)
export_summary=$(grep -E '^# [0-9]+ of [0-9]+ snapshot scenarios passed$' "$export_out" || true)
[[ "$export_summary" =~ ^#\ ([0-9]+)\ of\ ([0-9]+)\ snapshot\ scenarios ]] \
  || fail "exported snapshot browser proof printed no scenario summary"
export_passed=${BASH_REMATCH[1]}
export_total=${BASH_REMATCH[2]}
[ "$export_total" -gt 0 ] && [ "$export_passed" -eq "$export_total" ] && [ "$export_ok" -eq "$export_total" ] \
  || fail "exported snapshot browser proof passed $export_ok of $export_total scenarios"

pass "exported snapshot: $export_total browser scenarios passed in $chrome"
