#!/usr/bin/env bash
# tests/fm-mutation-run.test.sh - the mutation runner (bin/fm-mutation-run.sh over
# bin/fm-mutation-exec-lib.sh) through its executable interface: the evidence record
# for a fixture change, the timeout and error rules, the report-only tool records,
# the scratch-copy guarantee, the refusal of malformed input, and the seeded shell
# faults in tests/fixtures/mutation-seeded, each of which must be recorded killed.
set -u

# shellcheck source=tests/lib.sh
. "$(dirname "${BASH_SOURCE[0]}")/lib.sh"

CLASSIFY="$ROOT/bin/fm-mutation-classify.sh"
RUN="$ROOT/bin/fm-mutation-run.sh"
TMP=$(fm_test_tmproot fm-mutation-run)
TAB=$(printf '\t')
FIX="$TMP/fixture"
ERR="$TMP/stderr"
SCRATCH_ROOT="$TMP/scratch-root"
mkdir -p "$SCRATCH_ROOT"

# A curated tool directory holds only what the runner and its fixtures call, so
# mutmut and stryker are absent by construction on every host.
NOTOOLS="$TMP/tools"
mkdir -p "$NOTOOLS"
for tool in bash env git awk sed cp rm mkdir head tail cat cmp cksum find sort sleep mktemp dirname chmod wc; do
  tool_path=$(command -v "$tool") || continue
  case "$tool_path" in
    /*) ln -s "$tool_path" "$NOTOOLS/$tool" ;;
  esac
done

# row <field>...: one record line, its fields joined by TAB.
row() {
  local out="" sep="" field
  for field in "$@"; do
    out="$out$sep$field"
    sep=$TAB
  done
  printf '%s\n' "$out"
}

# snapshot: a checksum of every file in the fixture, so any write anywhere shows.
snapshot() {
  find "$FIX" -path "$FIX/.git" -prune -o -type f -exec cksum {} + | sort
}

# expect_refused <label> <input> [runner args...]: malformed input or a usage error
# is exit 2, prints no record on stdout, and explains itself on stderr.
expect_refused() {
  local label=$1 input=$2 out rc
  shift 2
  out=$(printf '%s' "$input" | "$RUN" "$@" 2>"$ERR")
  rc=$?
  expect_code 2 "$rc" "$label"
  assert_equals "" "$out" "$label must print no record"
  [ -s "$ERR" ] || fail "$label must explain the refusal on stderr"
}

# --- fixture: a git checkout with one change of each interesting kind ---------

mkdir -p "$FIX/bin" "$FIX/tests" "$FIX/docs"
git -C "$FIX" init -q

# Line numbers matter: the expected mutant ids below name these exact lines.
cat > "$FIX/bin/demo.sh" <<'SH'
#!/usr/bin/env bash
# bin/demo.sh - fixture production script for the mutation runner test.
set -u

classify() {
  if [ "$1" -eq 0 ]; then
    echo zero
  else
    echo nonzero
  fi
}

i=0
while [ "$i" -lt 3 ] && [ -n "run" ]; do
  i=$((i + 1))
done

classify "$1"
SH

cat > "$FIX/tests/demo.test.sh" <<'SH'
#!/usr/bin/env bash
# tests/demo.test.sh - oracle for the fixture bin/demo.sh.
ROOT=$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)
[ "$(bash "$ROOT/bin/demo.sh" 0)" = "zero" ] || exit 1
[ "$(bash "$ROOT/bin/demo.sh" 5)" = "nonzero" ] || exit 1
exit 0
SH

# orphan: a production script with no oracle at all, so nothing measures it.
printf '%s\n' '#!/usr/bin/env bash' 'echo orphan' > "$FIX/bin/orphan.sh"

# broken: its oracle fails on the unmutated script, so nothing it reports is judged.
printf '%s\n' '#!/usr/bin/env bash' 'echo broken' > "$FIX/bin/broken.sh"
printf '%s\n' '#!/usr/bin/env bash' 'exit 1' > "$FIX/tests/broken.test.sh"

# bare: a comment-only script, so no operator applies to any line.
printf '%s\n' '#!/usr/bin/env bash' '# bin/bare.sh - comment only; nothing in this file is a statement.' > "$FIX/bin/bare.sh"
printf '%s\n' '#!/usr/bin/env bash' 'exit 0' > "$FIX/tests/bare.test.sh"

# slow: its oracle outlives the bound on the unmutated script.
printf '%s\n' '#!/usr/bin/env bash' 'echo slow' > "$FIX/bin/slow.sh"
printf '%s\n' '#!/usr/bin/env bash' 'sleep 30' > "$FIX/tests/slow.test.sh"

# linked: a symbolic link to a script, which a mutant must never be written through.
ln -s demo.sh "$FIX/bin/linked.sh"

printf '# scripts\n' > "$FIX/docs/scripts.md"

# The change set, classified by the real classifier, is the runner's only input.
printf '%s\n' bin/demo.sh bin/orphan.sh bin/broken.sh bin/bare.sh \
  tests/demo.test.sh docs/scripts.md bin/fm-mail.py bin/fm-probe.ts > "$TMP/changed"
"$CLASSIFY" < "$TMP/changed" > "$TMP/classified" 2>"$ERR"
RC=$?
expect_code 0 "$RC" "every fixture change must classify before the runner sees it"
DEMO_TEST_DETAIL=$(awk -F "$TAB" '$1 == "tests/demo.test.sh" { print $4 }' "$TMP/classified")
DOCS_DETAIL=$(awk -F "$TAB" '$1 == "docs/scripts.md" { print $4 }' "$TMP/classified")
[ -n "$DEMO_TEST_DETAIL" ] || fail "the classifier must give tests/demo.test.sh a stated exclusion reason"
[ -n "$DOCS_DETAIL" ] || fail "the classifier must give docs/scripts.md a stated exclusion reason"

# --- the evidence record for the fixture change -----------------------------

snapshot > "$TMP/before"
# The bound is generous: a bash startup can take seconds on a loaded host, and a
# passing oracle must finish inside it or its mutants are correctly not judged.
RUN_OUT=$(env TMPDIR="$SCRATCH_ROOT" PATH="$NOTOOLS" "$RUN" --root "$FIX" --timeout 30 < "$TMP/classified" 2>"$ERR")
RC=$?
expect_code 0 "$RC" "a published evidence record exits 0 whatever the mutants did"

snapshot > "$TMP/after"
cmp -s "$TMP/before" "$TMP/after" || fail "the runner must never write the work tree"
assert_equals "" "$(ls -A "$SCRATCH_ROOT")" "the runner must remove its scratch directory on exit"
pass "the work tree is byte-identical after a run and the scratch directory is removed"

bad_columns=$(printf '%s\n' "$RUN_OUT" | awk -F "$TAB" '{ if (NF != 6) bad++ } END { print bad + 0 }')
assert_equals "0" "$bad_columns" "every evidence record must have exactly six TAB-separated columns"
pass "every evidence record has the same six columns"

assert_contains "$RUN_OUT" "tool${TAB}fm-shell-mutator${TAB}-${TAB}-${TAB}builtin${TAB}version 1; report-only; operators=eq-ne,and-or,status-flip,drop-stmt; oracle bash " "the shell mutator's tool record must name its version, report-only mode, operators, and oracle shell"
pass "the tool record names the shell mutator's version, mode, operators, and oracle shell"

EXPECT=$(
  row tool mutmut - - not-installed "report-only; no result reader in this runner"
  row tool stryker - - not-installed "report-only; no result reader in this runner"
  row mutant bin/demo.sh shell L3-drop-stmt survived "oracle passed"
  row mutant bin/demo.sh shell L5-drop-stmt error "mutant does not parse"
  row mutant bin/demo.sh shell L6-eq-ne killed "oracle exit 1"
  row mutant bin/demo.sh shell L7-drop-stmt error "mutant does not parse"
  row mutant bin/demo.sh shell L9-drop-stmt error "mutant does not parse"
  row mutant bin/demo.sh shell L13-drop-stmt killed "oracle exit 1"
  row mutant bin/demo.sh shell L14-and-or timeout "oracle tests/demo.test.sh timed out after 30s (--timeout)"
  row mutant bin/demo.sh shell L15-drop-stmt error "mutant does not parse"
  row mutant bin/demo.sh shell L18-drop-stmt killed "oracle exit 1"
  row path bin/orphan.sh shell - unresolved "no oracle: tests/orphan.test.sh is absent, so nothing measures this script"
  row path bin/broken.sh shell - unresolved "baseline oracle tests/broken.test.sh exits 1 on the unmutated script; mutants not judged"
  row path bin/bare.sh shell - unresolved "no mutation sites: no operator applies to any line"
  row exclusion tests/demo.test.sh test-code - unsupported "$DEMO_TEST_DETAIL"
  row exclusion docs/scripts.md markdown - unsupported "$DOCS_DETAIL"
  row path bin/fm-mail.py python - error "mutation tool not installed: mutmut; nothing was measured"
  row path bin/fm-probe.ts typescript - error "mutation tool not installed: stryker; nothing was measured"
  row summary - - - published "killed=3 survived=1 timeout=1 error=6 unresolved=3 excluded=2"
)
ACTUAL=$(printf '%s\n' "$RUN_OUT" | grep -v "^tool${TAB}fm-shell-mutator${TAB}")
assert_equals "$EXPECT" "$ACTUAL" "the evidence record must list every tool, mutant, path, exclusion, and summary exactly"
pass "the record lists each mutant verdict, each unresolved path, each exclusion, and the counts"

assert_not_contains "$RUN_OUT" "L14-and-or${TAB}killed" "a timed-out mutant must never count as killed"
assert_not_contains "$RUN_OUT" "L5-drop-stmt${TAB}killed" "a mutant that does not parse must never count as killed"
pass "timeouts and errors never count as killed, and unresolved paths are never recorded as passing"

# --- external tools: absent is an error, present is still an error ----------

ONLY_PY=$(row bin/fm-mail.py python mutate mutmut-class)
STUB="$TMP/stub"
mkdir -p "$STUB"
printf '%s\n' '#!/usr/bin/env bash' 'echo "mutmut 3.1.0"' > "$STUB/mutmut"
chmod +x "$STUB/mutmut"
EXPECT_STUB=$(
  row tool mutmut - - installed "version mutmut 3.1.0; report-only; no result reader in this runner"
  row path bin/fm-mail.py python - error "mutmut is installed, but this runner has no result reader for it; nothing was measured"
  row summary - - - published "killed=0 survived=0 timeout=0 error=1 unresolved=0 excluded=0"
)
STUB_OUT=$(printf '%s\n' "$ONLY_PY" | env TMPDIR="$SCRATCH_ROOT" PATH="$STUB:$NOTOOLS" "$RUN" --root "$FIX" --timeout 2 2>"$ERR")
RC=$?
expect_code 0 "$RC" "an installed external tool with no result reader still publishes a record"
assert_equals "$EXPECT_STUB" "$STUB_OUT" "an installed external tool must be recorded as an error, never as green"
pass "an installed external tool is recorded with its version and still judged as an error"

# --- symbolic links are never mutated ---------------------------------------

LINK_OUT=$(printf '%s\n' "$(row bin/linked.sh shell mutate "shell-custom-mutator (report-only)")" | env TMPDIR="$SCRATCH_ROOT" PATH="$NOTOOLS" "$RUN" --root "$FIX" --timeout 2 2>"$ERR")
RC=$?
expect_code 0 "$RC" "a symbolic link in the change must still publish a record"
assert_contains "$LINK_OUT" "$(row path bin/linked.sh shell - unresolved "symbolic link; a mutant would be written through the link, so it is not judged")" "a symbolic link must be unresolved, never mutated through the link"
assert_contains "$LINK_OUT" "$(row summary - - - published "killed=0 survived=0 timeout=0 error=0 unresolved=1 excluded=0")" "the symbolic link must count as one unresolved path"
pass "a symbolic link source is recorded as unresolved and never written through"

# --- an oracle that outlives the bound is a timeout, never passing ----------

SLOW_OUT=$(row bin/slow.sh shell mutate "shell-custom-mutator (report-only)" | env TMPDIR="$SCRATCH_ROOT" PATH="$NOTOOLS" "$RUN" --root "$FIX" --timeout 1 2>"$ERR")
RC=$?
expect_code 0 "$RC" "an oracle that times out still publishes a record"
assert_contains "$SLOW_OUT" "$(row path bin/slow.sh shell - timeout "baseline oracle tests/slow.test.sh timed out after 1s (--timeout); mutants not judged")" "a baseline that outlives the bound must record a timeout that names the phase and the option"
assert_contains "$SLOW_OUT" "$(row summary - - - published "killed=0 survived=0 timeout=1 error=0 unresolved=0 excluded=0")" "the timed-out baseline must count as one timeout"
assert_not_contains "$SLOW_OUT" "${TAB}passed" "a timed-out path must never be recorded as passing"
assert_not_contains "$SLOW_OUT" "${TAB}killed${TAB}" "a timed-out path must never be recorded as killed"
pass "an oracle that outlives the bound is recorded as a timeout, never as passing or killed"

# --- the runner's own shell: each mutant is exactly the edited line -----------
# /bin/bash is the oldest shell this runner supports (bash 3.2 on macOS). Run the
# runner under it, and expect the verdicts that exact one-line edits give. A
# replacement that kept its quotes would make these mutants unparseable or wrong.

FLIP="$TMP/flip"
mkdir -p "$FLIP/bin" "$FLIP/tests"
git -C "$FLIP" init -q
cat > "$FLIP/bin/flip.sh" <<'SH'
#!/usr/bin/env bash
set -u
if [ "$1" -eq 0 ] && [ -n "ok" ]; then
  exit 0
fi
exit 1
SH
cat > "$FLIP/tests/flip.test.sh" <<'SH'
#!/usr/bin/env bash
ROOT=$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)
bash "$ROOT/bin/flip.sh" 0 || exit 1
bash "$ROOT/bin/flip.sh" 5 && exit 1
exit 0
SH
FLIP_OUT=$(row bin/flip.sh shell mutate "shell-custom-mutator (report-only)" | env TMPDIR="$SCRATCH_ROOT" PATH="$NOTOOLS" /bin/bash "$RUN" --root "$FLIP" --timeout 30 2>"$ERR")
RC=$?
expect_code 0 "$RC" "the runner under /bin/bash must publish a record"
EXPECT_FLIP=$(
  row mutant bin/flip.sh shell L2-drop-stmt survived "oracle passed"
  row mutant bin/flip.sh shell L3-eq-ne killed "oracle exit 1"
  row mutant bin/flip.sh shell L3-and-or killed "oracle exit 1"
  row mutant bin/flip.sh shell L4-status-flip killed "oracle exit 1"
  row mutant bin/flip.sh shell L4-drop-stmt error "mutant does not parse"
  row mutant bin/flip.sh shell L6-status-flip killed "oracle exit 1"
  row mutant bin/flip.sh shell L6-drop-stmt killed "oracle exit 1"
  row summary - - - published "killed=5 survived=1 timeout=0 error=1 unresolved=0 excluded=0"
)
ACTUAL_FLIP=$(printf '%s\n' "$FLIP_OUT" | grep -v "^tool${TAB}fm-shell-mutator${TAB}")
assert_equals "$EXPECT_FLIP" "$ACTUAL_FLIP" "under /bin/bash each mutant must be the exact edited line, so its verdict is the one a correct mutant gives"
pass "the runner under the system bash judges each one-line edit as written"

# --- comment lines are not mutation sites -------------------------------------
# A comment that mentions exit 0, && or == is prose. Mutating it parses, the oracle
# still passes, and the record would call it survived; the record must hold only
# the mutants of the code lines.

NOTE="$TMP/note"
mkdir -p "$NOTE/bin" "$NOTE/tests"
git -C "$NOTE" init -q
cat > "$NOTE/bin/note.sh" <<'SH'
#!/usr/bin/env bash
# note.sh - exit 0 when ok && ready, exit 1 when a == b.
set -u
  # indented: exit 0 && [ "$1" == 0 ]
exit 3
SH
cat > "$NOTE/tests/note.test.sh" <<'SH'
#!/usr/bin/env bash
ROOT=$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)
bash "$ROOT/bin/note.sh"
[ "$?" -eq 3 ] || exit 1
exit 0
SH
NOTE_OUT=$(row bin/note.sh shell mutate "shell-custom-mutator (report-only)" | env TMPDIR="$SCRATCH_ROOT" PATH="$NOTOOLS" "$RUN" --root "$NOTE" --timeout 30 2>"$ERR")
RC=$?
expect_code 0 "$RC" "a script with comment lines must still publish a record"
EXPECT_NOTE=$(
  row mutant bin/note.sh shell L3-drop-stmt survived "oracle passed"
  row mutant bin/note.sh shell L5-drop-stmt killed "oracle exit 1"
  row summary - - - published "killed=1 survived=1 timeout=0 error=0 unresolved=0 excluded=0"
)
ACTUAL_NOTE=$(printf '%s\n' "$NOTE_OUT" | grep -v "^tool${TAB}")
assert_equals "$EXPECT_NOTE" "$ACTUAL_NOTE" "comment lines must yield no mutant under any operator"
pass "comment lines that mention exit 0, && and == produce no mutants"

# --- the mutation verdict: verified only when every changed path is measured ----

VERDICT="$ROOT/bin/fm-mutation-verdict.sh"
CLS_A=$(row bin/a.sh shell mutate "shell-custom-mutator (report-only)")
CLS_B=$(row bin/b.sh shell mutate "shell-custom-mutator (report-only)")
CLS_DOC=$(row docs/a.md docs unsupported "no mutation surface")
KILLED_A=$(row mutant bin/a.sh shell L3-drop-stmt killed "oracle exit 1")
SURVIVED_A=$(row mutant bin/a.sh shell L4-drop-stmt survived "oracle passed")
TIMEOUT_A=$(row mutant bin/a.sh shell L5-drop-stmt timeout "timed out after 30s")
ERROR_A=$(row mutant bin/a.sh shell L6-drop-stmt error "mutant does not parse")
UNRESOLVED_A=$(row path bin/a.sh shell - unresolved "baseline oracle timed out")
PUBLISHED=$(row summary - - - published "killed=1 survived=0 timeout=0 error=0 unresolved=0 excluded=0")
PASS_LINE="passed: 1 mutant(s) killed across 1 path(s); no survived, timeout, error, unresolved, unsupported, unclassified, or missing result"

# The changed-path list is the verdict's first input: the paths the change touched, not
# the paths the classifier chose to print. Each case names its list, so a path the
# classifier dropped is still a path the verdict must account for.
CHG_NONE=""
CHG_DOC=docs/a.md
CHG_A=bin/a.sh
CHG_AB=$(printf '%s\n%s' bin/a.sh bin/b.sh)
CHG_AD=$(printf '%s\n%s' bin/a.sh docs/a.md)
CHG_TOOL=$(printf '%s\n%s' bin/a.sh bin/tool)
CHG_TOOLS=$(printf '%s\n%s\n%s' bin/a.sh bin/tool bin/other)

# verdict_case <label> <exit-code> <exact-line> <changed-paths> <classifier-tsv> <record-tsv>:
# the verdict prints exactly one line, and its exit status agrees with that line.
verdict_case() {
  local label=$1 code=$2 want=$3 out rc
  printf '%s\n' "$4" >"$TMP/verdict-changed"
  printf '%s\n' "$5" >"$TMP/verdict-classified"
  printf '%s\n' "$6" >"$TMP/verdict-record"
  out=$("$VERDICT" "$TMP/verdict-changed" "$TMP/verdict-classified" "$TMP/verdict-record" 2>"$ERR")
  rc=$?
  expect_code "$code" "$rc" "$label"
  assert_equals "$want" "$out" "$label must print exactly one verdict line"
}

verdict_case "one killed mutant on one path is verified" 0 "$PASS_LINE" \
  "$CHG_A" "$CLS_A" "$(printf '%s\n%s' "$KILLED_A" "$PUBLISHED")"
pass "a fully killed change is the only passing verdict, and it starts with passed"

verdict_case "a survivor is not verified" 1 "not verified: 1 survived" \
  "$CHG_A" "$CLS_A" "$(printf '%s\n%s\n%s' "$KILLED_A" "$SURVIVED_A" "$PUBLISHED")"
verdict_case "a timeout is not verified" 1 "not verified: 1 timeout" \
  "$CHG_A" "$CLS_A" "$(printf '%s\n%s\n%s' "$KILLED_A" "$TIMEOUT_A" "$PUBLISHED")"
verdict_case "an error is not verified" 1 "not verified: 1 error" \
  "$CHG_A" "$CLS_A" "$(printf '%s\n%s\n%s' "$KILLED_A" "$ERROR_A" "$PUBLISHED")"
verdict_case "an unresolved path with no killed mutant is not verified" 1 \
  "not verified: 1 unresolved; no killed mutant" \
  "$CHG_A" "$CLS_A" "$(printf '%s\n%s' "$UNRESOLVED_A" "$PUBLISHED")"
pass "survived, timeout, error, and unresolved results are each named and never verified"

TWO_PATHS=$(printf '%s\n%s' "$CLS_A" "$CLS_B")
verdict_case "a changed path with no record is not verified" 1 \
  "not verified: 1 path(s) with no result" \
  "$CHG_AB" "$TWO_PATHS" "$(printf '%s\n%s' "$KILLED_A" "$PUBLISHED")"
assert_contains "$(cat "$ERR")" "no result for bin/b.sh" "the missing path must be named on stderr"
pass "a mutate path with no mutant or path record is named and never silently passes"

# A changed path with no classifier row was dropped, whatever the classifier's exit status
# was. It is named as unclassified, never read as absent, so it cannot pass by omission.
verdict_case "a changed path with no classifier row is not verified" 1 \
  "not verified: 1 changed path(s) with no classification" \
  "$CHG_TOOL" "$CLS_A" "$(printf '%s\n%s' "$KILLED_A" "$PUBLISHED")"
assert_contains "$(cat "$ERR")" "no classification for bin/tool" "the dropped path must be named on stderr"
verdict_case "every changed path with no classifier row is counted" 1 \
  "not verified: 2 changed path(s) with no classification" \
  "$CHG_TOOLS" "$CLS_A" "$(printf '%s\n%s' "$KILLED_A" "$PUBLISHED")"
pass "a changed path the classifier dropped is named as unclassified and never verified"

verdict_case "an unsupported path is not verified" 1 \
  "not verified: 1 path(s) without a mutation surface" \
  "$CHG_AD" "$(printf '%s\n%s' "$CLS_A" "$CLS_DOC")" "$(printf '%s\n%s' "$KILLED_A" "$PUBLISHED")"
verdict_case "a change with no mutation surface and no killed mutant is not verified" 1 \
  "not verified: no path with a mutation surface; 1 path(s) without a mutation surface; no killed mutant" \
  "$CHG_DOC" "$CLS_DOC" "$PUBLISHED"
verdict_case "an empty changed-path list is not verified" 1 \
  "not verified: no changed paths" \
  "$CHG_NONE" "$CLS_A" "$(printf '%s\n%s' "$KILLED_A" "$PUBLISHED")"
verdict_case "an empty classifier output is not verified" 1 \
  "not verified: no classified paths; 1 changed path(s) with no classification; no path with a mutation surface" \
  "$CHG_A" "" "$(printf '%s\n%s' "$KILLED_A" "$PUBLISHED")"
verdict_case "an unknown classifier disposition is not verified" 1 \
  "not verified: no path with a mutation surface; 1 classifier row(s) with an unknown disposition" \
  "$CHG_A" "$(row bin/a.sh shell skip reason)" "$(printf '%s\n%s' "$KILLED_A" "$PUBLISHED")"
verdict_case "a record without its published summary is not verified" 1 \
  "not verified: no published evidence record" \
  "$CHG_A" "$CLS_A" "$KILLED_A"
pass "unsupported, zero-surface, empty, malformed, and unfinished records are never verified"

ABSENT="$TMP/no-such-file"
MISSING_ALL=$("$VERDICT" "$ABSENT" "$ABSENT" "$ABSENT" 2>"$ERR")
RC=$?
expect_code 1 "$RC" "missing changed-path list, classifier output, and record must not verify"
assert_equals "not verified: no changed-path list; no classifier output; no evidence record" "$MISSING_ALL" \
  "missing inputs must be named, not read as an empty pass"
pass "a missing changed-path list, classifier output, or evidence record is not verified"

PRESENT_CLS="$TMP/present-classifier"
PRESENT_REC="$TMP/present-record"
printf '%s\n' "$CLS_A" >"$PRESENT_CLS"
printf '%s\n%s\n' "$KILLED_A" "$PUBLISHED" >"$PRESENT_REC"
MISSING_CHANGED=$("$VERDICT" "$ABSENT" "$PRESENT_CLS" "$PRESENT_REC" 2>"$ERR")
RC=$?
expect_code 1 "$RC" "a missing changed-path list must not verify, even with a complete record"
assert_equals "not verified: no changed-path list" "$MISSING_CHANGED" \
  "the verdict must not treat an absent change list as no changed paths"
pass "a complete record is not verified when the changed-path list is missing"

FIX_RECORD="$TMP/verdict-fixture-record"
printf '%s\n' "$RUN_OUT" >"$FIX_RECORD"
FIX_VERDICT=$("$VERDICT" "$TMP/changed" "$TMP/classified" "$FIX_RECORD" 2>"$ERR")
RC=$?
expect_code 1 "$RC" "the fixture change must not be verified"
assert_contains "$FIX_VERDICT" "not verified:" "the fixture change's verdict must name the failure"
pass "the fixture change's real runner record is not verified"

VERDICT_USAGE=$("$VERDICT" "$TMP/classified" 2>/dev/null)
RC=$?
expect_code 2 "$RC" "a verdict with one argument must be a usage error"
assert_equals "" "$VERDICT_USAGE" "a usage error must print no verdict"
VERDICT_USAGE2=$("$VERDICT" "$TMP/changed" "$TMP/classified" 2>/dev/null)
RC=$?
expect_code 2 "$RC" "a verdict with no record argument must be a usage error"
assert_equals "" "$VERDICT_USAGE2" "a usage error with no record must print no verdict"
pass "the verdict refuses a wrong argument count with exit 2 and no verdict line"

# --- the proof v2 Checks row: the same verdict, as one row for the candidate ----

ROW_SHA=0123456789abcdef0123456789abcdef01234567
ROW_CMD="bin/fm-mutation-verdict.sh classified record"

# checks_case <label> <exit-code> <exact-row> <changed-paths> <classifier-tsv> <record-tsv>:
# with --checks-row the verdict prints exactly one Checks row, and its exit status agrees.
checks_case() {
  local label=$1 code=$2 want=$3 out rc
  printf '%s\n' "$4" >"$TMP/verdict-changed"
  printf '%s\n' "$5" >"$TMP/verdict-classified"
  printf '%s\n' "$6" >"$TMP/verdict-record"
  out=$("$VERDICT" --checks-row "$ROW_SHA" "$TMP/verdict-changed" "$TMP/verdict-classified" "$TMP/verdict-record" 2>"$ERR")
  rc=$?
  expect_code "$code" "$rc" "$label"
  assert_equals "$want" "$out" "$label must print exactly one Checks row"
}

checks_case "a verified change renders a passing row for the candidate" 0 \
  "| mutation | $ROW_CMD | $PASS_LINE | $ROW_SHA |" \
  "$CHG_A" "$CLS_A" "$(printf '%s\n%s' "$KILLED_A" "$PUBLISHED")"
checks_case "a survivor renders a not-verified row with no pass word" 1 \
  "| mutation | $ROW_CMD | not verified: 1 survived | $ROW_SHA |" \
  "$CHG_A" "$CLS_A" "$(printf '%s\n%s\n%s' "$KILLED_A" "$SURVIVED_A" "$PUBLISHED")"
checks_case "a path with no result renders a not-verified row" 1 \
  "| mutation | $ROW_CMD | not verified: 1 path(s) with no result | $ROW_SHA |" \
  "$CHG_AB" "$TWO_PATHS" "$(printf '%s\n%s' "$KILLED_A" "$PUBLISHED")"
checks_case "a dropped changed path renders a not-verified row, never a pass" 1 \
  "| mutation | $ROW_CMD | not verified: 1 changed path(s) with no classification | $ROW_SHA |" \
  "$CHG_TOOL" "$CLS_A" "$(printf '%s\n%s' "$KILLED_A" "$PUBLISHED")"

CHECKS_MISSING=$("$VERDICT" --checks-row "$ROW_SHA" "$ABSENT" "$ABSENT" "$ABSENT" 2>"$ERR")
RC=$?
expect_code 1 "$RC" "missing inputs must still produce a row, not a pass"
assert_equals "| mutation | $ROW_CMD | not verified: no changed-path list; no classifier output; no evidence record | $ROW_SHA |" \
  "$CHECKS_MISSING" "missing inputs must render as a not-verified row"
pass "the Checks row carries the same verdict as the plain line, for verified and unverified changes"

# expect_checks_usage <label> <args...>: a bad --checks-row call is a usage error.
expect_checks_usage() {
  local label=$1 out rc
  shift
  out=$("$VERDICT" "$@" 2>/dev/null)
  rc=$?
  expect_code 2 "$rc" "$label"
  assert_equals "" "$out" "$label must print no Checks row"
}
expect_checks_usage "a short candidate sha is a usage error" \
  --checks-row abc123 "$TMP/verdict-classified" "$TMP/verdict-record"
expect_checks_usage "an uppercase candidate sha is a usage error" \
  --checks-row ABCDEF0123456789ABCDEF0123456789ABCDEF01 "$TMP/verdict-classified" "$TMP/verdict-record"
expect_checks_usage "a flag with no candidate is a usage error" --checks-row
expect_checks_usage "a flag with no record is a usage error" \
  --checks-row "$ROW_SHA" "$TMP/verdict-classified"
pass "the Checks row refuses a bad candidate sha or a missing argument with exit 2 and no row"

# --- the workflow's chain, end to end: a dropped changed path is never verified --
# The workflow runs the real classifier and runner with their exit statuses ignored,
# then the verdict over the changed-path list. The classifier refuses a path that
# matches no surface class and prints no row for it. Control: a change whose only path
# is measured and killed verifies. The same chain with an unmatched path added must not.

E2E="$TMP/e2e"
mkdir -p "$E2E/bin" "$E2E/tests"
git -C "$E2E" init -q
cat >"$E2E/bin/ok.sh" <<'EOF'
#!/usr/bin/env bash
[ "$1" -eq 0 ] && echo zero || echo nonzero
EOF
cat >"$E2E/tests/ok.test.sh" <<'EOF'
#!/usr/bin/env bash
ROOT=$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)
[ "$(bash "$ROOT/bin/ok.sh" 0)" = "zero" ] || exit 1
[ "$(bash "$ROOT/bin/ok.sh" 5)" = "nonzero" ] || exit 1
exit 0
EOF

# e2e_chain <changed-paths>: sets E2E_OUT and E2E_RC to the verdict's line and status.
e2e_chain() {
  printf '%s\n' "$1" >"$TMP/e2e-changed"
  "$CLASSIFY" <"$TMP/e2e-changed" >"$TMP/e2e-classified" 2>"$ERR" || true
  env TMPDIR="$SCRATCH_ROOT" PATH="$NOTOOLS" "$RUN" --root "$E2E" --timeout 30 \
    <"$TMP/e2e-classified" >"$TMP/e2e-record" 2>"$ERR" || true
  E2E_OUT=$("$VERDICT" "$TMP/e2e-changed" "$TMP/e2e-classified" "$TMP/e2e-record" 2>"$ERR")
  E2E_RC=$?
}

e2e_chain "bin/ok.sh"
expect_code 0 "$E2E_RC" "a change whose only path is measured and killed must verify"
assert_equals "passed: 3 mutant(s) killed across 1 path(s); no survived, timeout, error, unresolved, unsupported, unclassified, or missing result" \
  "$E2E_OUT" "the control change must verify through the real classifier, runner, and verdict"
pass "the control change verifies through the real classifier, runner, and verdict"

e2e_chain "$(printf '%s\n%s' bin/ok.sh bin/tool)"
expect_code 1 "$E2E_RC" "a change with an unmatched path must not verify, even though its measured path is killed"
assert_equals "not verified: 1 changed path(s) with no classification" "$E2E_OUT" \
  "the unmatched path must be the only reason, and it must be named"
assert_contains "$(cat "$ERR")" "no classification for bin/tool" "the unmatched path must be named on stderr"
pass "a changed path the classifier refuses is never verified, even when every measured path is killed"

# --- the workflow's own steps, run as written ---------------------------------
# The classify, runner, and verdict steps of the Mutation Verdict workflow are loaded as
# YAML and run by bash in a fixture repository whose base commit holds the real scripts
# and the oracle (test code is unsupported, so it cannot be a changed path) and whose head
# commit adds bin/ok.sh. A stub gh records the check run
# the last step posts. A head that also adds bin/tool, which the classifier cannot match,
# must post neutral and name that path. Without bin/tool the same steps post success.

command -v ruby >/dev/null 2>&1 || fail "ruby is required to parse the mutation verdict workflow as YAML"
WORKFLOW="$ROOT/.github/workflows/mutation-verdict.yml"
WF="$TMP/wf"
WF_STEPS="$TMP/wf-steps"
WF_GH="$TMP/wf-gh"
WF_POSTED="$TMP/wf-posted"
mkdir -p "$WF_STEPS" "$WF_GH"

for step in "Classify the changed paths" "Run the mutation runner on the classified paths" "Post the verdict"; do
  ruby -ryaml -e '
steps = YAML.load_file(ARGV[0]).fetch("jobs").fetch("mutation-verdict-run").fetch("steps")
step = steps.find { |s| s["name"] == ARGV[1] } or raise "no step named #{ARGV[1]}"
puts step.fetch("run")
' "$WORKFLOW" "$step" >"$WF_STEPS/$step.sh" || fail "could not read the workflow step: $step"
done

cat >"$WF_GH/gh" <<'SH'
#!/usr/bin/env bash
: >"$WF_POSTED.conclusion"
: >"$WF_POSTED.summary"
while [ "$#" -gt 0 ]; do
  case "$1" in
    -f)
      case "$2" in
        conclusion=*) printf '%s' "${2#conclusion=}" >"$WF_POSTED.conclusion" ;;
        "output[summary]="*) printf '%s' "${2#output\[summary\]=}" >"$WF_POSTED.summary" ;;
      esac
      shift
      ;;
  esac
  shift
done
SH
chmod +x "$WF_GH/gh"

mkdir -p "$WF/bin" "$WF/tests"
git -C "$WF" init -q
cp "$ROOT"/bin/fm-mutation-*.sh "$WF/bin/"
cp "$E2E/tests/ok.test.sh" "$WF/tests/ok.test.sh"
git -C "$WF" add -A
git -C "$WF" -c user.name=fixture -c user.email=fixture@example.com commit -q -m base
WF_BASE=$(git -C "$WF" rev-parse HEAD)
cp "$E2E/bin/ok.sh" "$WF/bin/ok.sh"
git -C "$WF" add -A
git -C "$WF" -c user.name=fixture -c user.email=fixture@example.com commit -q -m head-ok
WF_HEAD_OK=$(git -C "$WF" rev-parse HEAD)
printf '%s\n' '#!/usr/bin/env bash' 'echo tool' >"$WF/bin/tool"
git -C "$WF" add -A
git -C "$WF" -c user.name=fixture -c user.email=fixture@example.com commit -q -m head-tool
WF_HEAD_TOOL=$(git -C "$WF" rev-parse HEAD)

# run_workflow <head-sha>: runs the three steps in order, sets WF_CONCLUSION and WF_SUMMARY.
run_workflow() {
  local head=$1 step run_tmp="$TMP/wf-run-temp"
  rm -rf "$run_tmp"
  mkdir -p "$run_tmp"
  : >"$TMP/wf-step-summary"
  git -C "$WF" checkout -q "$head"
  for step in "Classify the changed paths" "Run the mutation runner on the classified paths" "Post the verdict"; do
    (
      cd "$WF" || exit 1
      env TMPDIR="$SCRATCH_ROOT" PATH="$WF_GH:$NOTOOLS" WF_POSTED="$WF_POSTED" \
        RUNNER_TEMP="$run_tmp" GITHUB_STEP_SUMMARY="$TMP/wf-step-summary" \
        GITHUB_REPOSITORY=org/repo GH_TOKEN=token BASE_SHA="$WF_BASE" HEAD_SHA="$head" \
        bash -c "$(cat "$WF_STEPS/$step.sh")"
    ) >"$TMP/wf-step-out" 2>&1 || fail "workflow step '$step' must finish: $(cat "$TMP/wf-step-out")"
  done
  WF_CONCLUSION=$(cat "$WF_POSTED.conclusion")
  WF_SUMMARY=$(cat "$WF_POSTED.summary")
  WF_STEP_SUMMARY=$(cat "$TMP/wf-step-summary")
}

run_workflow "$WF_HEAD_OK"
assert_equals "success" "$WF_CONCLUSION" "a change whose every path is measured and killed must post success"
run_workflow "$WF_HEAD_TOOL"
assert_equals "neutral" "$WF_CONCLUSION" "a change with an unmatched bin/tool must post neutral, never success"
assert_contains "$WF_SUMMARY" "not verified: 1 changed path(s) with no classification" \
  "the posted summary must carry the verdict line"
assert_contains "$WF_SUMMARY" "no classification for bin/tool" "the posted summary must name the unmatched path"
assert_contains "$WF_STEP_SUMMARY" "no classification for bin/tool" "the step summary must name the unmatched path"
pass "the workflow steps post success without bin/tool and neutral naming bin/tool with it"

# --- command-line contract and malformed input -------------------------------

HELP=$("$RUN" --help 2>/dev/null)
RC=$?
expect_code 0 "$RC" "--help must exit 0"
assert_contains "$HELP" "fm-mutation-run.sh" "--help must name the runner"
pass "--help prints usage and exits 0"

VALID=$(row bin/demo.sh shell mutate "shell-custom-mutator (report-only)")
expect_refused "three fields" "bin/demo.sh${TAB}shell${TAB}mutate"
expect_refused "empty surface" "bin/demo.sh${TAB}${TAB}mutate${TAB}shell-custom-mutator (report-only)"
expect_refused "unknown disposition" "bin/demo.sh${TAB}shell${TAB}skip${TAB}shell-custom-mutator (report-only)"
expect_refused "unknown strategy" "bin/demo.sh${TAB}shell${TAB}mutate${TAB}made-up-strategy"
expect_refused "parent-directory path" "../escape.sh${TAB}shell${TAB}mutate${TAB}shell-custom-mutator (report-only)"
expect_refused "absolute path" "/tmp/escape.sh${TAB}shell${TAB}mutate${TAB}shell-custom-mutator (report-only)"
expect_refused "tab inside the detail" "bin/demo.sh${TAB}shell${TAB}unsupported${TAB}reason${TAB}extra"
expect_refused "no classifier lines" "" --root "$FIX"
expect_refused "timeout of zero" "$VALID" --root "$FIX" --timeout 0
expect_refused "non-numeric timeout" "$VALID" --root "$FIX" --timeout soon
expect_refused "budget of zero" "$VALID" --root "$FIX" --budget 0
expect_refused "non-numeric budget" "$VALID" --root "$FIX" --budget soon
expect_refused "budget without a value" "$VALID" --root "$FIX" --budget
expect_refused "probe timeout of zero" "$VALID" --root "$FIX" --probe-timeout 0
expect_refused "non-numeric probe timeout" "$VALID" --root "$FIX" --probe-timeout soon
expect_refused "syntax timeout of zero" "$VALID" --root "$FIX" --syntax-timeout 0
expect_refused "non-numeric syntax timeout" "$VALID" --root "$FIX" --syntax-timeout soon
expect_refused "bound beyond nine digits" "$VALID" --root "$FIX" --budget 1234567890
expect_refused "unknown argument" "$VALID" --root "$FIX" --bogus
pass "malformed classifier lines, empty input, and bad options are refused with exit 2 and no record"

# --- seeded shell faults: one deliberate fault per fixture -------------------
# tests/fixtures/mutation-seeded is the shell regression set: the runner must
# record each seeded mutant killed. Python and TypeScript seeds have no tool
# here, so they stay error rows and are never claimed killed.

SEED_FIX="$ROOT/tests/fixtures/mutation-seeded"
SEED="$TMP/seeded"
mkdir -p "$SEED/bin" "$SEED/tests"
git -C "$SEED" init -q
for seeded in seeded-eq-ne seeded-and-or seeded-status-flip seeded-drop-stmt; do
  cp "$SEED_FIX/$seeded/bin/$seeded.sh" "$SEED/bin/$seeded.sh"
  cp "$SEED_FIX/$seeded/tests/$seeded.test.sh" "$SEED/tests/$seeded.test.sh"
done

printf '%s\n' bin/seeded-eq-ne.sh bin/seeded-and-or.sh bin/seeded-status-flip.sh \
  bin/seeded-drop-stmt.sh > "$TMP/seeded-changed"
"$CLASSIFY" < "$TMP/seeded-changed" > "$TMP/seeded-classified" 2>"$ERR"
RC=$?
expect_code 0 "$RC" "every seeded change must classify before the runner sees it"

seed_sums() { find "$SEED" -path "$SEED/.git" -prune -o -type f -exec cksum {} + | sort; }
seed_sums > "$TMP/seed-before"
SEED_OUT=$(env TMPDIR="$SCRATCH_ROOT" PATH="$NOTOOLS" "$RUN" --root "$SEED" --timeout 30 < "$TMP/seeded-classified" 2>"$ERR")
RC=$?
expect_code 0 "$RC" "a seeded change set must publish an evidence record"
seed_sums > "$TMP/seed-after"
cmp -s "$TMP/seed-before" "$TMP/seed-after" || fail "the runner must never write the seeded work tree"
assert_equals "" "$(ls -A "$SCRATCH_ROOT")" "the seeded run must leave no scratch directory behind"

EQ_KILLED=$(row mutant bin/seeded-eq-ne.sh shell L4-eq-ne killed "oracle exit 1")
AND_KILLED=$(row mutant bin/seeded-and-or.sh shell L4-and-or killed "oracle exit 1")
FLIP_KILLED=$(row mutant bin/seeded-status-flip.sh shell L6-status-flip killed "oracle exit 1")
DROP_KILLED=$(row mutant bin/seeded-drop-stmt.sh shell L5-drop-stmt killed "oracle exit 1")
assert_contains "$SEED_OUT" "$EQ_KILLED" "the seeded eq-ne fault must be recorded killed"
assert_contains "$SEED_OUT" "$AND_KILLED" "the seeded and-or fault must be recorded killed"
assert_contains "$SEED_OUT" "$FLIP_KILLED" "the seeded status-flip fault must be recorded killed"
assert_contains "$SEED_OUT" "$DROP_KILLED" "the seeded drop-stmt fault must be recorded killed"
pass "each seeded shell fault is recorded killed"

SEED_SHELL_PATHS=$(printf '%s\n' "$SEED_OUT" | awk -F "$TAB" '$1 == "path" && $2 ~ /\.sh$/ { print }')
assert_equals "" "$SEED_SHELL_PATHS" "no seeded shell script may end as an unresolved or error path record"
pass "no seeded shell script was left unjudged"

# --- oracle exit codes: a stop is never a verified kill -----------------------
# Each production script is one line, exit 0. Its status-flip mutant turns that line into
# exit 1, and only the mutant's oracle exits with the status this case names. The oracle
# passes the unmutated script, so every mutant is judged. Exit 1 stays killed; 124 and
# 128+N are not kills; 126 and 127 stay errors.
EXIT_MAP="$TMP/exit-map"
mkdir -p "$EXIT_MAP/bin" "$EXIT_MAP/tests"
git -C "$EXIT_MAP" init -q
write_exit_oracle() {  # <name> <status>
  printf '%s\n' '#!/usr/bin/env bash' 'exit 0' >"$EXIT_MAP/bin/$1.sh"
  # shellcheck disable=SC2016 # the single quotes write oracle source text, not expansions
  {
    printf '%s\n' '#!/usr/bin/env bash'
    printf '%s\n' 'ROOT=$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)'
    printf '%s\n' 'while IFS= read -r line || [ -n "$line" ]; do'
    printf '%s\n' "  [ \"\$line\" = 'exit 1' ] && exit $2"
    printf '%s\n' 'done <"$ROOT/bin/'"$1"'.sh"' 'exit 0'
  } >"$EXIT_MAP/tests/$1.test.sh"
}
write_exit_oracle x1 1
write_exit_oracle x124 124
write_exit_oracle x127 127
write_exit_oracle x130 130
write_exit_oracle x143 143
printf '%s\n' bin/x1.sh bin/x124.sh bin/x127.sh bin/x130.sh bin/x143.sh >"$TMP/exit-changed"
"$CLASSIFY" <"$TMP/exit-changed" >"$TMP/exit-classified" 2>"$ERR"
expect_code 0 "$?" "the exit-code fixture must classify before the runner sees it"

EXIT_OUT=$(env TMPDIR="$SCRATCH_ROOT" PATH="$NOTOOLS" "$RUN" --root "$EXIT_MAP" --timeout 30 <"$TMP/exit-classified" 2>"$ERR")
RC=$?
expect_code 0 "$RC" "the exit-code fixture must publish an evidence record"
assert_equals "" "$(ls -A "$SCRATCH_ROOT")" "the exit-code run must leave no scratch directory behind"

assert_contains "$EXIT_OUT" "mutant${TAB}bin/x1.sh${TAB}shell${TAB}L2-status-flip${TAB}killed${TAB}" \
  "an oracle exit of 1 must stay a kill"
assert_contains "$EXIT_OUT" "mutant${TAB}bin/x124.sh${TAB}shell${TAB}L2-status-flip${TAB}timeout${TAB}" \
  "an oracle exit of 124 must be a timeout, never a kill"
assert_contains "$EXIT_OUT" "mutant${TAB}bin/x130.sh${TAB}shell${TAB}L2-status-flip${TAB}error${TAB}" \
  "an oracle exit of 130 (128+2) must be an error, never a kill"
assert_contains "$EXIT_OUT" "mutant${TAB}bin/x143.sh${TAB}shell${TAB}L2-status-flip${TAB}error${TAB}" \
  "an oracle exit of 143 (128+15) must be an error, never a kill"
assert_contains "$EXIT_OUT" "mutant${TAB}bin/x127.sh${TAB}shell${TAB}L2-status-flip${TAB}error${TAB}" \
  "an oracle exit of 127 must stay an error"
assert_contains "$EXIT_OUT" "summary${TAB}-${TAB}-${TAB}-${TAB}published${TAB}killed=1 survived=5 timeout=1 error=3 " \
  "the summary must count one kill, five survivors (the drop-stmt mutants), one timeout, and three errors"
pass "exit 1 is killed; 124, 128+N, and 143 are never kills; 126 and 127 stay errors"

printf '%s\n' bin/x1.sh bin/x124.sh bin/x127.sh bin/x130.sh bin/x143.sh >"$TMP/exit-changed"
printf '%s\n' "$EXIT_OUT" >"$TMP/exit-record"
EXIT_VERDICT=$("$VERDICT" "$TMP/exit-changed" "$TMP/exit-classified" "$TMP/exit-record" 2>"$ERR")
case "$EXIT_VERDICT" in
  "not verified:"*) ;;
  *) fail "a run with a timeout and errors must not verify, got: $EXIT_VERDICT" ;;
esac
assert_contains "$EXIT_VERDICT" "timeout" "the verdict must name the timeout status"
assert_contains "$EXIT_VERDICT" "error" "the verdict must name the error status"
pass "the verdict does not verify a run whose stops were not kills"

# Defect 4: the whole-run budget covers judging. A script with 200 judgeable lines and
# a fast oracle would take far longer than 3 s to judge, so the budget must stop
# judging, record the rest as not judged, and end well inside its bound.
BUDGET_MAP="$TMP/budget-map"
mkdir -p "$BUDGET_MAP/bin" "$BUDGET_MAP/tests"
git -C "$BUDGET_MAP" init -q
printf '%s\n' '#!/usr/bin/env bash' 'exit 0' >"$BUDGET_MAP/tests/long.test.sh"
{
  printf '%s\n' '#!/usr/bin/env bash'
  i=0
  while [ "$i" -lt 200 ]; do
    printf '%s\n' "echo line$i"
    i=$((i + 1))
  done
} >"$BUDGET_MAP/bin/long.sh"
printf '%s\n' bin/long.sh >"$TMP/budget-changed"
"$CLASSIFY" <"$TMP/budget-changed" >"$TMP/budget-classified" 2>"$ERR"
expect_code 0 "$?" "the budget fixture must classify before the runner sees it"

BUDGET_START=$SECONDS
BUDGET_OUT=$(env TMPDIR="$SCRATCH_ROOT" PATH="$NOTOOLS" "$RUN" --root "$BUDGET_MAP" --budget 3 --timeout 30 <"$TMP/budget-classified" 2>"$ERR")
RC=$?
BUDGET_ELAPSED=$((SECONDS - BUDGET_START))
expect_code 0 "$RC" "a run whose budget ends during judging must still publish a record"
assert_equals "" "$(ls -A "$SCRATCH_ROOT")" "the judging-budget run must leave no scratch directory behind"
BUDGET_JUDGED=$(printf '%s\n' "$BUDGET_OUT" | grep -c "^mutant${TAB}")
[ "$BUDGET_JUDGED" -lt 200 ] || fail "the budget must stop judging before all 200 mutants are judged; judged $BUDGET_JUDGED"
assert_contains "$BUDGET_OUT" "whole-run budget of 3s (--budget) was exhausted; mutants not judged" \
  "the path that met the budget must say that its remaining mutants were not judged"
[ "$BUDGET_ELAPSED" -le 20 ] || fail "a 3 s budget must end the run well inside 20 s, took ${BUDGET_ELAPSED}s"
printf '%s\n' "$BUDGET_OUT" >"$TMP/budget-record"
BUDGET_VERDICT=$("$VERDICT" "$TMP/budget-changed" "$TMP/budget-classified" "$TMP/budget-record" 2>"$ERR")
case "$BUDGET_VERDICT" in
  "not verified:"*) ;;
  *) fail "a run that stopped judging at its budget must not verify, got: $BUDGET_VERDICT" ;;
esac
pass "a budget that ends during judging stops the judging, records the rest as not judged, and is never verified"

# Defect 4, copy: the budget ends during the tree copy. The per-file check stops the
# copy, so no mutant is judged on the incomplete tree, and the run ends inside its bound.
COPY_MAP="$TMP/copy-map"
mkdir -p "$COPY_MAP/bin" "$COPY_MAP/tests" "$COPY_MAP/filler"
git -C "$COPY_MAP" init -q
printf '%s\n' '#!/usr/bin/env bash' 'exit 0' >"$COPY_MAP/tests/long.test.sh"
printf '%s\n' '#!/usr/bin/env bash' 'echo one' >"$COPY_MAP/bin/long.sh"
i=0
while [ "$i" -lt 6000 ]; do
  : >"$COPY_MAP/filler/f$i"
  i=$((i + 1))
done
printf '%s\n' bin/long.sh >"$TMP/copy-changed"
"$CLASSIFY" <"$TMP/copy-changed" >"$TMP/copy-classified" 2>"$ERR"
expect_code 0 "$?" "the copy-budget fixture must classify before the runner sees it"

COPY_START=$SECONDS
COPY_OUT=$(env TMPDIR="$SCRATCH_ROOT" PATH="$NOTOOLS" "$RUN" --root "$COPY_MAP" --budget 1 --timeout 30 <"$TMP/copy-classified" 2>"$ERR")
RC=$?
COPY_ELAPSED=$((SECONDS - COPY_START))
expect_code 0 "$RC" "a run whose budget ends during the tree copy must still publish a record"
assert_equals "" "$(ls -A "$SCRATCH_ROOT")" "the copy-budget run must leave no scratch directory behind"
assert_contains "$COPY_OUT" "whole-run budget of 1s (--budget) was exhausted; mutants not judged" \
  "a tree copy stopped by the budget must record its mutate path as not judged"
assert_not_contains "$COPY_OUT" "mutant${TAB}" "a tree copy stopped by the budget must judge no mutant"
[ "$COPY_ELAPSED" -le 20 ] || fail "a 1 s budget must end a copy of 6000 files well inside 20 s, took ${COPY_ELAPSED}s"
pass "a budget that ends during the tree copy judges no mutant and ends inside its bound"

echo "# fm-mutation-run.test.sh: all assertions passed"
