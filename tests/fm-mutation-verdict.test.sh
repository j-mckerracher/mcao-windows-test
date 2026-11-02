#!/usr/bin/env bash
# tests/fm-mutation-verdict.test.sh - the mutation verdict (bin/fm-mutation-verdict.sh)
# through its command line: verified is the only passing outcome, every negative case
# the mutation evidence contract names is not verified with its reason, a malformed
# evidence line never passes, and the --checks-row output is one proof v2 Checks row
# that carries the candidate it was given.
set -u

# shellcheck source=tests/lib.sh
. "$(dirname "${BASH_SOURCE[0]}")/lib.sh"

VERDICT="$ROOT/bin/fm-mutation-verdict.sh"
CLASSIFY="$ROOT/bin/fm-mutation-classify.sh"
TMP=$(fm_test_tmproot fm-mutation-verdict)
ERR="$TMP/stderr"
TAB=$(printf '\t')
SHA=0123456789abcdef0123456789abcdef01234567
OTHER=fedcba9876543210fedcba9876543210fedcba98

# line <column>...: one runner record line, its columns joined by TAB.
line() {
  local IFS=$TAB
  printf '%s\n' "$*"
}

# write_record <file> <line>...: a record file holding the given lines.
write_record() {
  local out=$1
  shift
  printf '%s\n' "$@" > "$out"
}

# good_record <file>: the runner's record for bin/a.sh with two killed mutants and a
# published summary.
good_record() {
  write_record "$1" \
    "$(line tool shell-custom-mutator - - builtin bash)" \
    "$(line mutant bin/a.sh shell L3-negate killed 'caught by the oracle')" \
    "$(line mutant bin/a.sh shell L5-flip killed 'caught by the oracle')" \
    "$(line summary - - - published 'killed=2 survived=0 timeout=0 error=0 unresolved=0 excluded=0')"
}

# status_record <file> <status>: the good record with the second mutant's status replaced.
status_record() {
  write_record "$1" \
    "$(line tool shell-custom-mutator - - builtin bash)" \
    "$(line mutant bin/a.sh shell L3-negate killed 'caught by the oracle')" \
    "$(line mutant bin/a.sh shell L5-flip "$2" 'the oracle reported this status')" \
    "$(line summary - - - published 'killed=1 survived=0 timeout=0 error=0 unresolved=0 excluded=0')"
}

# fixture <name>: a fresh directory holding a changed-path list for bin/a.sh, the
# classifier's real output for it, and the good record. Sets D to the directory.
fixture() {
  D="$TMP/$1"
  rm -rf "$D"
  mkdir -p "$D"
  printf '%s\n' bin/a.sh > "$D/paths"
  "$CLASSIFY" bin/a.sh > "$D/classified"
  good_record "$D/record"
}

# run_verdict <dir> [<candidate-sha>]: the verdict over a fixture, with a checks row
# when a candidate is given. Sets OUT, RC and ERR (stderr).
run_verdict() {
  local d=$1
  if [ -n "${2:-}" ]; then
    OUT=$("$VERDICT" --checks-row "$2" "$d/paths" "$d/classified" "$d/record" 2>"$ERR")
  else
    OUT=$("$VERDICT" "$d/paths" "$d/classified" "$d/record" 2>"$ERR")
  fi
  RC=$?
}

# expect_not_verified <message> <dir> <reason>: exit 1, and the not verified line names
# the reason.
expect_not_verified() {
  run_verdict "$2"
  expect_code 1 "$RC" "$1 exit code"
  case "$OUT" in
    "not verified: "*"$3"*) ;;
    *) fail "$1: expected a not verified line naming '$3', got: $OUT" ;;
  esac
  pass "$1"
}

# --- verified is the only passing outcome ----------------------------------------

fixture verified
run_verdict "$D"
expect_code 0 "$RC" "a fully measured change with killed mutants"
case "$OUT" in
  "passed: "*) ;;
  *) fail "verified line must start with 'passed: ', got: $OUT" ;;
esac
pass "a fully measured change with killed mutants is passed"

run_verdict "$D" "$SHA"
expect_code 0 "$RC" "a verified checks row exit code"
assert_contains "$OUT" "| mutation | bin/fm-mutation-verdict.sh classified record | passed: " \
  "a verified checks row names the verdict command and the passed result"
assert_contains "$OUT" " | $SHA |" "a checks row ends with the candidate in its at column"
pass "--checks-row emits one proof v2 row for the verified candidate"

# --- every negative case is not verified -----------------------------------------

fixture survived
status_record "$D/record" survived
expect_not_verified "a survivor is not verified" "$D" "1 survived"

fixture timeout
status_record "$D/record" timeout
expect_not_verified "a mutant timeout is not verified" "$D" "1 timeout"

fixture error
status_record "$D/record" error
expect_not_verified "a mutant tool error is not verified" "$D" "1 error"

fixture equivalent
status_record "$D/record" equivalent
expect_not_verified "an equivalent-mutant claim is not a killed result" "$D" "1 equivalent"

fixture unresolved
write_record "$D/record" \
  "$(line tool shell-custom-mutator - - builtin bash)" \
  "$(line mutant bin/a.sh shell L3-negate killed 'caught by the oracle')" \
  "$(line path bin/a.sh shell - unresolved 'the path could not be judged')" \
  "$(line summary - - - published 'killed=1 survived=0 timeout=0 error=0 unresolved=1 excluded=0')"
expect_not_verified "an unresolved path is not verified" "$D" "1 unresolved"

fixture nokill
write_record "$D/record" \
  "$(line tool shell-custom-mutator - - builtin bash)" \
  "$(line mutant bin/a.sh shell L3-negate survived 'the oracle accepted it')" \
  "$(line summary - - - published 'killed=0 survived=1 timeout=0 error=0 unresolved=0 excluded=0')"
expect_not_verified "zero killed mutants is not verified" "$D" "no killed mutant"

fixture unsupported
printf '%s\n' bin/a.sh docs/x.md > "$D/paths"
"$CLASSIFY" bin/a.sh docs/x.md > "$D/classified"
expect_not_verified "an unsupported path beside a measured path is not verified" "$D" "without a mutation surface"

fixture unclassified
printf '%s\n' bin/a.sh weird.xyz > "$D/paths"
"$CLASSIFY" bin/a.sh weird.xyz > "$D/classified" 2>/dev/null
expect_not_verified "a changed path the classifier refused is not verified" "$D" "1 changed path(s) with no classification"
assert_contains "$(cat "$ERR")" "no classification for weird.xyz" "the refused path is named on stderr"
pass "the refused path is named, so a dropped path cannot pass by being absent"

fixture noclassified
: > "$D/classified"
expect_not_verified "a classifier that printed nothing is not verified" "$D" "no classified paths"

fixture unknowndisposition
printf 'bin/a.sh\tshell\tmaybe\tnot a disposition\n' > "$D/classified"
expect_not_verified "a classifier row with an unknown disposition is not verified" "$D" "unknown disposition"

fixture nochangedlist
rm -f "$D/paths"
run_verdict "$D"
expect_code 1 "$RC" "a missing changed-path list exit code"
assert_contains "$OUT" "no changed-path list" "a missing changed-path list is named"
pass "a missing changed-path list is not verified"

fixture noclassifier
rm -f "$D/classified"
run_verdict "$D"
expect_code 1 "$RC" "a missing classifier output exit code"
assert_contains "$OUT" "no classifier output" "a missing classifier output is named"
pass "a missing classifier output is not verified"

fixture norecord
rm -f "$D/record"
run_verdict "$D"
expect_code 1 "$RC" "a missing evidence record exit code"
assert_contains "$OUT" "no evidence record" "a missing evidence record is named"
pass "an absent evidence record is not verified"

fixture nosummary
write_record "$D/record" \
  "$(line tool shell-custom-mutator - - builtin bash)" \
  "$(line mutant bin/a.sh shell L3-negate killed 'caught by the oracle')"
expect_not_verified "a record with no published summary is not verified" "$D" "no published evidence record"

# --- malformed evidence never passes ---------------------------------------------

fixture shortkill
write_record "$D/record" \
  "$(line tool shell-custom-mutator - - builtin bash)" \
  "mutant${TAB}bin/a.sh${TAB}shell${TAB}L3-negate${TAB}killed" \
  "$(line mutant bin/a.sh shell L5-flip killed 'caught by the oracle')" \
  "$(line summary - - - published 'killed=2 survived=0 timeout=0 error=0 unresolved=0 excluded=0')"
run_verdict "$D"
expect_code 1 "$RC" "a five-column killed line exit code"
assert_contains "$(cat "$ERR")" "malformed evidence record at line 2" "the five-column line is named on stderr"
case "$OUT" in
  *"1 malformed evidence record line(s)"*) ;;
  *) fail "a five-column killed line must be a malformed record, got: $OUT" ;;
esac
pass "a killed line with five columns is malformed and never counts as a kill"

fixture unknownkind
write_record "$D/record" \
  "$(line tool shell-custom-mutator - - builtin bash)" \
  "$(line verdict bin/a.sh shell L3-negate killed 'caught by the oracle')" \
  "$(line mutant bin/a.sh shell L5-flip killed 'caught by the oracle')" \
  "$(line summary - - - published 'killed=2 survived=0 timeout=0 error=0 unresolved=0 excluded=0')"
expect_not_verified "a record line of an unknown kind is not verified" "$D" "1 malformed evidence record line(s)"

fixture garbageline
write_record "$D/record" \
  "$(line tool shell-custom-mutator - - builtin bash)" \
  "this is not a record" \
  "$(line mutant bin/a.sh shell L5-flip killed 'caught by the oracle')" \
  "$(line summary - - - published 'killed=1 survived=0 timeout=0 error=0 unresolved=0 excluded=0')"
expect_not_verified "a line that is not a record is not verified" "$D" "1 malformed evidence record line(s)"

# --- command-line contract --------------------------------------------------------

fixture usage
OUT=$("$VERDICT" --checks-row abc "$D/paths" "$D/classified" "$D/record" 2>"$ERR")
RC=$?
expect_code 2 "$RC" "a checks-row candidate that is not a full commit id is a usage error"
assert_equals "" "$OUT" "a usage error prints no verdict"
pass "a checks-row candidate that is not a full commit id is refused"

OUT=$("$VERDICT" "$D/paths" "$D/classified" 2>"$ERR")
RC=$?
expect_code 2 "$RC" "two positional arguments are a usage error"
pass "a call with missing arguments is a usage error"

# A checks row carries exactly the candidate it was given. The producer cannot know the
# head, so proof-check compares this column with the candidate.
fixture stamped
run_verdict "$D" "$OTHER"
assert_contains "$OUT" " | $OTHER |" "a checks row carries the candidate it was given"
case "$OUT" in
  *" | $SHA |"*) fail "a checks row for $OTHER must not carry another candidate" ;;
esac
pass "a checks row carries exactly the candidate it was given"

echo "# fm-mutation-verdict.test.sh: all assertions passed"
