#!/usr/bin/env bash
# Behavior tests for bin/fm-birds-eye-export.sh (DPES-798). Assertions read the exporter's
# exit status, its stdout line, the one file it writes, and the HTML that file contains.
# Producer input is the hostile recorded fixture in tests/fixtures/birds-eye/export through
# --fixture-dir. The browser proof of the same snapshot lives in fm-birds-eye-ui.test.sh.
set -u

# shellcheck source=tests/lib.sh
# shellcheck disable=SC1091
. "$(dirname "${BASH_SOURCE[0]}")/lib.sh"

EXPORT="$ROOT/bin/fm-birds-eye-export.sh"
FIXTURE="$ROOT/tests/fixtures/birds-eye/export"
TMP_ROOT=$(fm_test_tmproot fm-birds-eye-export)

command -v jq >/dev/null 2>&1 || { echo "skip: jq not found"; exit 0; }

# Observer clock. The hostile fixture was generated at 05:06:30Z, so NOW is 05:06:40Z.
NOW=1792300000
CAPTURED_AT=2026-10-18T05:06:40Z

# expect_usage <label> [args...]: the exporter refuses with status 2.
expect_usage() {
  local label=$1 code=0
  shift
  bash "$EXPORT" "$@" >"$TMP_ROOT/usage.out" 2>&1 || code=$?
  expect_code 2 "$code" "$label"
}

# --- usage and refusal ------------------------------------------------------

bash "$EXPORT" --help >"$TMP_ROOT/help.out" 2>&1
expect_code 0 "$?" "--help exits 0"
assert_grep "--output" "$TMP_ROOT/help.out" "--help names --output"

expect_usage "unknown argument exits 2" --bogus --output "$TMP_ROOT/never.html"
expect_usage "missing --output exits 2" --fixture-dir "$FIXTURE" --now "$NOW"
expect_usage "interval below five seconds is refused" \
  --fixture-dir "$FIXTURE" --now "$NOW" --interval 4 --output "$TMP_ROOT/below-floor.html"
[ ! -e "$TMP_ROOT/below-floor.html" ] || fail "a refused export left an output file"
pass "usage errors exit 2 and write nothing"

# A producer refusal is forwarded unchanged and must not disturb an existing snapshot.
mkdir -p "$TMP_ROOT/no-manifest"
printf 'PRIOR SNAPSHOT\n' >"$TMP_ROOT/prior.html"
code=0
bash "$EXPORT" --fixture-dir "$TMP_ROOT/no-manifest" --now "$NOW" --output "$TMP_ROOT/prior.html" \
  >"$TMP_ROOT/refused.out" 2>&1 || code=$?
expect_code 2 "$code" "producer refusal is forwarded unchanged"
[ "$(cat "$TMP_ROOT/prior.html")" = 'PRIOR SNAPSHOT' ] || fail "a failed export replaced the existing snapshot"
pass "a producer refusal exits with its status and leaves the existing snapshot intact"

# --- hostile snapshot -------------------------------------------------------

HOME_DIR="$TMP_ROOT/fm-home"
mkdir -p "$HOME_DIR" "$TMP_ROOT/tmpdir"
home_before=$(find "$HOME_DIR" | sort)
SNAP="$TMP_ROOT/out/nested/snap.html"

code=0
FM_HOME="$HOME_DIR" TMPDIR="$TMP_ROOT/tmpdir" bash "$EXPORT" \
  --fixture-dir "$FIXTURE" --now "$NOW" --output "$SNAP" >"$TMP_ROOT/export.out" 2>"$TMP_ROOT/export.err" || code=$?
expect_code 0 "$code" "hostile fixture exports"
[ -f "$SNAP" ] || fail "export wrote no snapshot at $SNAP"

expected_line="wrote $SNAP | roots=2 entities=4 attention=1 unsupported=native_subagents captured_at=$CAPTURED_AT"
[ "$(cat "$TMP_ROOT/export.out")" = "$expected_line" ] \
  || fail "export summary was $(cat "$TMP_ROOT/export.out"), expected $expected_line"
pass "export reports its counts and capture time on one line"

# Free text and links from the hostile producer never reach the page as raw text.
for marker in SECRET-DETAIL SECRET-WORKTREE SECRET-NOTE SECRET-JS; do
  if grep -F -q "$marker" "$SNAP"; then
    fail "snapshot leaked excluded marker $marker"
  fi
done
if grep -F -q '</style><b>SECRET-HOLD' "$SNAP"; then
  fail "decision text closed the style element in raw HTML"
fi
grep -F -q 'u003c/style' "$SNAP" || fail "decision text is not escaped in the embedded data"
pass "excluded producer fields and hostile markup never reach the page"

style_closers=$(grep -o '</style>' "$SNAP" | wc -l | tr -d ' ')
[ "$style_closers" -eq 1 ] || fail "snapshot has $style_closers </style> tags, expected exactly one"
if grep -F -q 'rel="stylesheet"' "$SNAP" || grep -F -q '<script src=' "$SNAP"; then
  fail "snapshot references an external stylesheet or script"
fi
pass "the page carries its own style and script, with no external references"

# The CSP must admit exactly the inline blocks the page carries, by hash, and nothing else.
python3 -I - "$SNAP" <<'PY' || fail "the Content-Security-Policy does not admit the page's inline blocks"
import base64
import hashlib
import re
import sys

html = open(sys.argv[1], encoding='utf-8').read()
meta = re.search(r'<meta http-equiv="Content-Security-Policy" content="([^"]*)">', html)
if meta is None:
    sys.exit('no Content-Security-Policy meta element')
policy = meta.group(1)
directives = {d.split(' ')[0]: d for d in policy.split('; ')}
for required in ("default-src 'none'", "connect-src 'none'", "img-src data:"):
    if required not in policy:
        sys.exit(f'policy lacks {required}')


def digest(text):
    return "'sha256-" + base64.b64encode(hashlib.sha256(text.encode('utf-8')).digest()).decode() + "'"


scripts = re.findall(r'<script>(.*?)</script>', html, re.S)
styles = re.findall(r'<style>(.*?)</style>', html, re.S)
if len(scripts) != 2 or len(styles) != 1:
    sys.exit(f'expected 2 inline scripts and 1 inline style, found {len(scripts)} and {len(styles)}')
script_sources = directives.get('script-src', '').split()[1:]
style_sources = directives.get('style-src', '').split()[1:]
if sorted(script_sources) != sorted(digest(s) for s in scripts):
    sys.exit('script-src does not list exactly the hashes of the inline scripts')
if style_sources != [digest(s) for s in styles]:
    sys.exit('style-src does not list exactly the hash of the inline style')
PY
pass "the Content-Security-Policy hashes match the inline script and style exactly"

# Same inputs and pinned clock give the same bytes; the exporter leaves nothing behind.
bash "$EXPORT" --fixture-dir "$FIXTURE" --now "$NOW" --output "$TMP_ROOT/again/snap.html" >/dev/null 2>&1 \
  || fail "second export with the same inputs failed"
cmp -s "$SNAP" "$TMP_ROOT/again/snap.html" || fail "two exports of the same inputs differ"
pass "same inputs and clock give byte-identical snapshots"

home_after=$(find "$HOME_DIR" | sort)
[ "$home_before" = "$home_after" ] || fail "export changed the configured home"
[ -z "$(ls -A "$TMP_ROOT/tmpdir")" ] || fail "export left working files in TMPDIR"
pass "export writes only its output file and removes its working files"

exit 0
