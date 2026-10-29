#!/usr/bin/env bash
# Behavior tests for bin/fm-panel-report.sh: the launch-policy verdict and the panel
# receipt report. Every case drives the executable with real seat records and asserts
# its stdout, stderr, and exit status. The contract is docs/panel-receipts.md.
set -u

# shellcheck source=tests/lib.sh
. "$(dirname "${BASH_SOURCE[0]}")/lib.sh"

SCRIPT="$ROOT/bin/fm-panel-report.sh"
TMP=$(fm_test_tmproot fm-panel-report)
ERR_FILE="$TMP/err"
OUT=""
ERR=""
CODE=0

# seat <seat_id> <role> <seat_kind> <judges> <requested> <resolved> <outcome>
#      <observed_model> <observed_family> <evidence> <reason> <output>
seat() {
  local IFS=$'\t'
  printf '%s\n' "$*"
}

write_seats() {  # <file> <line>...
  local file=$1 line
  shift
  : > "$file"
  for line in "$@"; do
    printf '%s\n' "$line" >> "$file"
  done
}

run_policy() {
  OUT=$(bash "$SCRIPT" policy "$@" 2>"$ERR_FILE")
  CODE=$?
  ERR=$(cat "$ERR_FILE")
}

run_report() {
  OUT=$(bash "$SCRIPT" report "$@" 2>"$ERR_FILE")
  CODE=$?
  ERR=$(cat "$ERR_FILE")
}

# --- launch policy ----------------------------------------------------------

run_policy no-mistakes design-assist design-assist
expect_code 0 "$CODE" "matching design-assist authority"
assert_contains "$OUT" "launch: design-assist panel" "matching authority launches the panel"

run_policy direct-PR delivery-review delivery-review
expect_code 0 "$CODE" "matching delivery-review authority under direct-PR"
assert_contains "$OUT" "launch: delivery-review panel" "delivery-review launches under direct-PR"

run_policy no-mistakes delivery-review none
expect_code 2 "$CODE" "panel proposed with no granted authority"
assert_contains "$OUT" "policy-conflict:" "missing authority is a policy conflict"
assert_contains "$OUT" "no panel launched" "a policy conflict launches nothing"

run_policy direct-PR design-assist delivery-review
expect_code 2 "$CODE" "authority granted for a different panel kind"
assert_contains "$OUT" "without matching authority (granted: delivery-review)" "conflict names the granted kind"

run_policy no-mistakes design-assist delivery-review
expect_code 2 "$CODE" "delivery-review authority does not cover a design-assist panel"

run_policy local-only design-assist design-assist
expect_code 1 "$CODE" "a delivery mode outside the managed-context contract"
assert_contains "$ERR" "fm-panel-report policy" "invalid mode names the caller"

run_policy no-mistakes bogus-kind none
expect_code 1 "$CODE" "an unknown panel kind"
assert_contains "$ERR" "panel kind must be" "unknown kind is refused"

run_policy no-mistakes design-assist bogus-authority
expect_code 1 "$CODE" "an unknown granted authority"
assert_contains "$ERR" "granted authority must be" "unknown authority is refused"

run_policy no-mistakes design-assist
expect_code 1 "$CODE" "policy with too few arguments"
pass "launch policy matrix: matching authority launches, mismatch and absence refuse, invalid input exits 1"

# --- receipts: complete panels and identity -----------------------------------

CROSS="$TMP/cross.tsv"
write_seats "$CROSS" \
  "$(seat c1 writer candidate - gpt-a gpt-a completed gpt-a openai ev-cat-1 - out/c1.md)" \
  "$(seat c2 writer candidate - claude-b claude-b completed claude-b anthropic ev-cat-2 - out/c2.md)" \
  "$(seat j1 reviewer judge c1 claude-b claude-b completed claude-b anthropic ev-cat-2 - -)"
run_report "$CROSS"
expect_code 0 "$CODE" "a valid cross-family panel"
assert_contains "$OUT" "panel-status: complete" "a panel with no gaps is complete"
assert_contains "$OUT" "seats: candidate=2 judge=1 completed=3 missing=0" "seat counts"
assert_contains "$OUT" "diversity: cross-family across 2 families; executed models:" "cross-family claim with evidence"
assert_contains "$OUT" "c1=gpt-a/openai (evidence ev-cat-1)" "cross-family claim names the executed models"
assert_contains "$OUT" "judge-separation: judge j1 family anthropic differs from judged seat c1 family openai" "judge in a different family"
assert_contains "$OUT" "receipt: c1 role=writer kind=candidate judges=- requested=gpt-a resolved=gpt-a outcome=completed observed-model=gpt-a observed-family=openai identity=verified evidence=ev-cat-1 reason=- output=out/c1.md" "receipt keeps requested, resolved, and observed identity"
assert_contains "$OUT" "completed-output: c1 out/c1.md" "completed output stays listed"
assert_contains "$OUT" "completed-output: c2 out/c2.md" "every completed seat keeps its output"
assert_not_contains "$OUT" "completed-output: j1" "a seat with no output records none"

SAME="$TMP/same.tsv"
write_seats "$SAME" \
  "$(seat c1 writer candidate - gpt-a gpt-a completed gpt-a openai ev-cat-1 - out/c1.md)" \
  "$(seat c2 writer candidate - gpt-a gpt-a completed gpt-a openai ev-cat-1 - out/c2.md)"
run_report "$SAME"
expect_code 0 "$CODE" "two contexts on one model"
assert_contains "$OUT" "diversity: independent contexts on gpt-a across 2 completed seats; no cross-family claim" "one model gives no cross-family claim"
assert_not_contains "$OUT" "cross-family across" "one model never claims cross-family"

UNKNOWN="$TMP/unknown.tsv"
write_seats "$UNKNOWN" \
  "$(seat c1 writer candidate - gpt-a gpt-a completed unknown unknown - - out/c1.md)"
run_report "$UNKNOWN"
expect_code 0 "$CODE" "a completed seat with missing identity"
assert_contains "$OUT" "receipt: c1 role=writer kind=candidate judges=- requested=gpt-a resolved=gpt-a outcome=completed observed-model=unknown observed-family=unknown identity=unknown" "unknown identity is reported as unknown"
assert_not_contains "$OUT" "observed-model=gpt-a" "the resolved model is not copied into observed identity"
assert_contains "$OUT" "diversity: unknown, identity missing for seat(s) c1" "unknown identity yields no diversity claim"
pass "receipts keep requested, resolved, and observed identity separate; missing identity stays unknown"

# --- coverage, minimums, and failed seats -------------------------------------

FAILED="$TMP/failed.tsv"
write_seats "$FAILED" \
  "$(seat c1 writer candidate - gpt-a gpt-a completed gpt-a openai ev-cat-1 - out/c1.md)" \
  "$(seat c2 writer candidate - gpt-b gpt-b failed unknown unknown - execution-failed -)"
run_report "$FAILED"
expect_code 0 "$CODE" "a failed seat is reported, not a script error"
assert_contains "$OUT" "panel-status: incomplete" "a failed seat makes the panel incomplete"
assert_contains "$OUT" "coverage-gap: seat c2 (role writer) outcome failed; reason: execution-failed" "the failed seat is named with its reason"
assert_contains "$OUT" "completed-output: c1 out/c1.md" "the completed seat's output survives the failure"

MINIMUM="$TMP/minimum.tsv"
write_seats "$MINIMUM" \
  "$(seat c1 writer candidate - gpt-a gpt-a completed gpt-a openai ev-cat-1 - out/c1.md)" \
  "$(seat j1 reviewer judge c1 gpt-b gpt-b completed gpt-b anthropic ev-cat-2 - -)"
run_report --min writer=2 "$MINIMUM"
expect_code 0 "$CODE" "a panel below its role minimum"
assert_contains "$OUT" "panel-status: incomplete" "below the role minimum the panel is incomplete"
assert_contains "$OUT" "minimum: role writer completed 1 of 2 required" "the minimum is reported"
assert_contains "$OUT" "coverage-gap: role writer has 1 of 2 required completed seats" "the shortfall is a coverage gap"

run_report --min writer=1 "$MINIMUM"
expect_code 0 "$CODE" "a panel that meets its role minimum"
assert_contains "$OUT" "panel-status: complete" "meeting the minimum makes the panel complete"
assert_contains "$OUT" "minimum: role writer completed 1 of 1 required" "the met minimum is reported"

run_report --min ghost=1 "$MINIMUM"
expect_code 0 "$CODE" "a role minimum with no seats"
assert_contains "$OUT" "coverage-gap: role ghost has 0 of 1 required completed seats" "a role with no seats cannot meet its minimum"

FALLBACK_OK="$TMP/fallback-ok.tsv"
write_seats "$FALLBACK_OK" \
  "$(seat c1 writer candidate - gpt-a gpt-b completed gpt-b openai ev-cat-1 gpt-a-retired out/c1.md)"
run_report "$FALLBACK_OK"
expect_code 0 "$CODE" "a fallback with a stated reason"
assert_contains "$OUT" "requested=gpt-a resolved=gpt-b" "the fallback keeps requested and resolved apart"
assert_contains "$OUT" "reason=gpt-a-retired" "the fallback reason is recorded"
assert_contains "$OUT" "panel-status: complete" "a recorded fallback does not by itself make the panel incomplete"

UNRESOLVED_OK="$TMP/unresolved-ok.tsv"
write_seats "$UNRESOLVED_OK" \
  "$(seat c1 writer candidate - gpt-a unresolved missing unknown unknown - no-capability-entry -)"
run_report "$UNRESOLVED_OK"
expect_code 0 "$CODE" "an unresolved seat that did not complete"
assert_contains "$OUT" "panel-status: incomplete" "an unresolved seat leaves the panel incomplete"
assert_contains "$OUT" "coverage-gap: seat c1 (role writer) outcome missing; reason: no-capability-entry" "the unresolved seat's reason is kept"
assert_contains "$OUT" "diversity: none, no completed seats" "no completed seat means no diversity claim"
pass "coverage: failed seats are named, minimums are enforced, fallbacks and unresolved seats need reasons"

# --- judges and diversity -----------------------------------------------------

SINGLE="$TMP/single.tsv"
write_seats "$SINGLE" \
  "$(seat c1 writer candidate - gpt-a gpt-a completed gpt-a openai ev-cat-1 - out/c1.md)" \
  "$(seat c2 writer candidate - gpt-b gpt-b completed gpt-b openai ev-cat-1 - out/c2.md)" \
  "$(seat j1 reviewer judge c1 gpt-c gpt-c completed gpt-c openai ev-cat-1 - -)"
run_report "$SINGLE"
expect_code 0 "$CODE" "a single-family panel"
assert_contains "$OUT" "panel-status: complete" "a single-family panel is not failed"
assert_contains "$OUT" "diversity: reduced, single family openai across 3 completed seats on 3 models; reduced diversity, not a failure" "single family is reduced diversity"
assert_contains "$OUT" "judge-separation: reduced diversity: judge j1 and judged seat c1 share family openai" "a judge in the judged seat's family is reduced"

UNKNOWN_JUDGE="$TMP/unknown-judge.tsv"
write_seats "$UNKNOWN_JUDGE" \
  "$(seat c1 writer candidate - gpt-a gpt-a completed gpt-a openai ev-cat-1 - out/c1.md)" \
  "$(seat j1 reviewer judge c1 gpt-b gpt-b completed unknown unknown - - -)"
run_report "$UNKNOWN_JUDGE"
expect_code 0 "$CODE" "a judge with unknown identity"
assert_contains "$OUT" "judge-separation: judge j1 judging c1 has unknown identity; family separation not proven" "unknown judge identity is not proven separate"
pass "judges: separate seat, separation reported, same family reduced, unknown identity not proven"

# --- invalid seat records are refused -----------------------------------------

check_refused() {  # <label> <expected-error-fragment> <line>...
  local label=$1 want=$2 file="$TMP/refused.tsv"
  shift 2
  write_seats "$file" "$@"
  run_report "$file"
  expect_code 1 "$CODE" "$label"
  assert_contains "$ERR" "$want" "$label names the problem"
  assert_equals "" "$OUT" "$label prints no report"
}

check_refused "silent fallback" "silent fallback refused" \
  "$(seat c1 writer candidate - gpt-a gpt-b completed gpt-b openai ev-cat-1 - out/c1.md)"

check_refused "completed seat with an unresolved model" "is completed but its resolved model is unresolved" \
  "$(seat c1 writer candidate - gpt-a unresolved completed unknown unknown - - out/c1.md)"

check_refused "unresolved seat with no reason" "needs its reason in the reason column" \
  "$(seat c1 writer candidate - gpt-a unresolved missing unknown unknown - - -)"

check_refused "failed seat with no reason" "failed and needs its reason in the reason column" \
  "$(seat c1 writer candidate - gpt-a gpt-a failed unknown unknown - - -)"

check_refused "completed seat on its requested model with a reason" "completed on its requested model and gives a reason" \
  "$(seat c1 writer candidate - gpt-a gpt-a completed gpt-a openai ev-cat-1 stray-reason out/c1.md)"

check_refused "judge judging itself" "judge seat 'j1' must name a different seat" \
  "$(seat c1 writer candidate - gpt-a gpt-a completed gpt-a openai ev-cat-1 - out/c1.md)" \
  "$(seat j1 reviewer judge j1 gpt-b gpt-b completed gpt-b anthropic ev-cat-2 - -)"

check_refused "judge naming an unknown seat" "names unknown judged seat 'ghost'" \
  "$(seat c1 writer candidate - gpt-a gpt-a completed gpt-a openai ev-cat-1 - out/c1.md)" \
  "$(seat j1 reviewer judge ghost gpt-b gpt-b completed gpt-b anthropic ev-cat-2 - -)"

check_refused "candidate naming a judged seat" "candidate seat 'c1' must name no judged seat" \
  "$(seat c1 writer candidate c2 gpt-a gpt-a completed gpt-a openai ev-cat-1 - out/c1.md)" \
  "$(seat c2 writer candidate - gpt-b gpt-b completed gpt-b anthropic ev-cat-2 - out/c2.md)"

check_refused "family without evidence" "needs supporting evidence" \
  "$(seat c1 writer candidate - gpt-a gpt-a completed gpt-a openai - - out/c1.md)"

check_refused "family with an unknown model" "family must be unknown" \
  "$(seat c1 writer candidate - gpt-a gpt-a completed unknown openai - - out/c1.md)"

check_refused "duplicate seat id" "duplicate seat_id 'c1'" \
  "$(seat c1 writer candidate - gpt-a gpt-a completed gpt-a openai ev-cat-1 - out/c1.md)" \
  "$(seat c1 writer candidate - gpt-b gpt-b completed gpt-b anthropic ev-cat-2 - out/c2.md)"

check_refused "short seat record" "expected 12 tab-separated fields, got 3" \
  "$(printf 'c1\twriter\tcandidate\n')"

check_refused "unknown outcome" "outcome must be completed, failed, or missing, got 'done'" \
  "$(seat c1 writer candidate - gpt-a gpt-a "done" gpt-a openai ev-cat-1 - out/c1.md)"

EMPTY="$TMP/empty.tsv"
: > "$EMPTY"
run_report "$EMPTY"
expect_code 1 "$CODE" "a seats file with no records"
assert_contains "$ERR" "has no seat records" "an empty seats file is refused"

run_report "$TMP/missing.tsv"
expect_code 1 "$CODE" "a missing seats file"
assert_contains "$ERR" "cannot read seats file" "a missing seats file is refused"
pass "invalid seat records are refused with the named problem and no report"
