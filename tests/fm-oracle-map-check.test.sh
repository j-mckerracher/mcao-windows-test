#!/usr/bin/env bash
# tests/fm-oracle-map-check.test.sh - the path-to-oracle map coverage check
# (bin/fm-oracle-map-check.sh) through its executable interface: the committed
# map covers every in-scope Python, TypeScript and JavaScript path, test code is
# never in scope, and a fixture repository fails loudly for a missing row, a stale
# row, an executed row with no oracle, an unknown status and a duplicate row.
set -u

# shellcheck source=tests/lib.sh
. "$(dirname "${BASH_SOURCE[0]}")/lib.sh"

CHECK="$ROOT/bin/fm-oracle-map-check.sh"
TMP=$(fm_test_tmproot fm-oracle-map-check)
ERR="$TMP/stderr"

# run_check [args...]: sets OUT (stdout) and RC (exit code); stderr lands in $ERR.
run_check() {
  OUT=$("$CHECK" "$@" 2>"$ERR")
  RC=$?
}

# --- the committed map ------------------------------------------------------

run_check
expect_code 0 "$RC" "the committed oracle map must cover every in-scope path"
assert_contains "$OUT" "in-scope paths, each with one valid row" "committed map summary"
pass "the committed map has a valid row for every in-scope Python, TypeScript and JavaScript path"

# --- fixture repository ------------------------------------------------------

F="$TMP/fixture"
mkdir -p "$F/bin" "$F/tests" "$F/.claude/mods/x/lib" "$F/docs"
printf 'x = 1\n' > "$F/bin/tool.py"
printf 'export const v = 1;\n' > "$F/.claude/mods/x/lib/mod.ts"
printf 'print(1)\n' > "$F/tests/only-test.py"
printf '# notes\n' > "$F/README.md"
git -C "$F" init -q
git -C "$F" add -A
MAP="$F/docs/map.tsv"
printf '# columns: kind path surface oracle status detail\n' > "$MAP"
printf 'map\tbin/tool.py\tpython\tbash tests/x.sh\texecuted\tran under the test\n' >> "$MAP"
printf 'map\t.claude/mods/x/lib/mod.ts\ttypescript\t-\tmissing\tno test references it\n' >> "$MAP"

run_check --root "$F" --map "$MAP"
expect_code 0 "$RC" "a complete fixture map must pass; stderr: $(cat "$ERR")"
pass "test code under tests/ needs no row, and a complete map passes"

# A tracked in-scope path with no row fails and is named.
grep -v 'mod.ts' "$MAP" > "$MAP.partial"
run_check --root "$F" --map "$MAP.partial"
expect_code 1 "$RC" "a map missing an in-scope row must fail"
assert_contains "$(cat "$ERR")" "no map row for in-scope path: .claude/mods/x/lib/mod.ts" "missing row must be named"
pass "an in-scope path with no map row fails and is named"

# A row for a path that is not in scope is stale and fails.
cp "$MAP" "$MAP.stale"
printf 'map\tbin/ghost.py\tpython\t-\tmissing\tno such file\n' >> "$MAP.stale"
run_check --root "$F" --map "$MAP.stale"
expect_code 1 "$RC" "a stale row must fail"
assert_contains "$(cat "$ERR")" "map row for a path that is not in scope: bin/ghost.py" "stale row must be named"
pass "a row for a path that is not in scope fails as stale"

# An executed row that names no oracle is not proof and fails.
printf 'map\tbin/tool.py\tpython\t-\texecuted\tno oracle named\n' > "$MAP.noroacle"
printf 'map\t.claude/mods/x/lib/mod.ts\ttypescript\t-\tmissing\tno test references it\n' >> "$MAP.noroacle"
run_check --root "$F" --map "$MAP.noroacle"
expect_code 1 "$RC" "an executed row without an oracle must fail"
assert_contains "$(cat "$ERR")" "executed row for bin/tool.py names no oracle" "executed-without-oracle must be named"
pass "an executed row that names no oracle fails"

# A status outside the owned vocabulary fails, so a result can never read as passing.
printf 'map\tbin/tool.py\tpython\tbash tests/x.sh\tpassing\tlooks fine\n' > "$MAP.status"
printf 'map\t.claude/mods/x/lib/mod.ts\ttypescript\t-\tmissing\tno test references it\n' >> "$MAP.status"
run_check --root "$F" --map "$MAP.status"
expect_code 1 "$RC" "an unknown status must fail"
assert_contains "$(cat "$ERR")" 'row for bin/tool.py has unknown status "passing"' "unknown status must be named"
pass "a status outside the owned vocabulary fails"

# Two rows for one path fail rather than letting one shadow the other.
cp "$MAP" "$MAP.dup"
printf 'map\tbin/tool.py\tpython\tbash tests/x.sh\texecuted\tsecond row\n' >> "$MAP.dup"
run_check --root "$F" --map "$MAP.dup"
expect_code 1 "$RC" "a duplicate row must fail"
assert_contains "$(cat "$ERR")" "duplicate row for bin/tool.py" "duplicate row must be named"
pass "a duplicate row for one path fails"

# Usage and unreadable input are distinct from coverage failures.
run_check --root "$F" --map "$TMP/does-not-exist.tsv"
expect_code 2 "$RC" "an unreadable map must exit 2"
pass "an unreadable map exits 2, distinct from a coverage failure"

echo "# fm-oracle-map-check.test.sh: all assertions passed"
