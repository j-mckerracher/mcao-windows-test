#!/usr/bin/env bash
# Behavior tests for bin/fm-panel-resolve.sh: host-side panel seat resolution against a
# real model-capabilities.md source, and the chain from resolution through execution
# records into bin/fm-panel-report.sh. Every case drives the executables and asserts
# their stdout, stderr, and exit status. The contract is docs/panel-receipts.md.
# Execution is not performed here: a seat's outcome columns are written the way the
# executing worker would record them, so these tests cover the host-side mechanics only.
set -u

# shellcheck source=tests/lib.sh
. "$(dirname "${BASH_SOURCE[0]}")/lib.sh"

RESOLVE="$ROOT/bin/fm-panel-resolve.sh"
REPORT="$ROOT/bin/fm-panel-report.sh"
TMP=$(fm_test_tmproot fm-panel-resolve)
ERR_FILE="$TMP/err"
OUT=""
ERR=""
CODE=0

run_resolve() {
  OUT=$(bash "$RESOLVE" resolve "$@" 2>"$ERR_FILE")
  CODE=$?
  ERR=$(cat "$ERR_FILE")
}

run_report() {
  OUT=$(bash "$REPORT" report "$@" 2>"$ERR_FILE")
  CODE=$?
  ERR=$(cat "$ERR_FILE")
}

# field <record> <n>: the n-th tab-separated field of one seat record.
field() {
  printf '%s\n' "$1" | cut -f"$2"
}

entry() {  # <model_id> <family> <nested> <available> <identity_source> [provider]
  printf '  - model_id: %s\n    family: %s\n    nested: %s\n    available: %s\n    identity_source: %s\n' \
    "$1" "$2" "$3" "$4" "$5"
  [ -z "${6:-}" ] || printf '    provider: %s\n' "$6"
}

write_cap() {  # <file> <entry>...
  local file=$1
  shift
  printf '%s\n' '---' 'models:' > "$file"
  local e
  for e in "$@"; do printf '%s\n' "$e" >> "$file"; done
  printf '%s\n' '---' 'prose after the front matter' >> "$file"
}

write_request() {  # <file> <line>...
  local file=$1
  shift
  : > "$file"
  local line
  for line in "$@"; do printf '%s\n' "$line" >> "$file"; done
}

# The capability source every resolution case shares.
CAP="$TMP/model-capabilities.md"
write_cap "$CAP" \
  "$(entry gpt-a openai true true catalog acme)" \
  "$(entry gpt-b openai true true catalog acme)" \
  "$(entry gpt-root openai false false catalog acme)" \
  "$(entry claude-j anthropic true true catalog)" \
  "$(entry gpt-u unknown true true unknown)"

REQ="$TMP/request.tsv"
write_request "$REQ" \
  $'c1\twriter\tcandidate\t-\tgpt-a' \
  $'c2\twriter\tcandidate\t-\tgpt-root' \
  $'c3\twriter\tcandidate\t-\tgpt-x' \
  $'c4\twriter\tcandidate\t-\tgpt-u' \
  $'j1\treviewer\tjudge\tc1\tclaude-j'

# --- every requested seat gets an executable assignment or an explicit unresolved result

run_resolve "$CAP" "$REQ"
expect_code 0 "$CODE" "resolving a valid request against a valid source"
assert_equals "5" "$(printf '%s\n' "$OUT" | wc -l | tr -d ' ')" "one record per requested seat, none dropped"
assert_equals "c1 c2 c3 c4 j1" "$(printf '%s\n' "$OUT" | cut -f1 | tr '\n' ' ' | sed 's/ $//')" "records keep the request order"

C1=$(printf '%s\n' "$OUT" | sed -n 1p)
assert_equals "gpt-a" "$(field "$C1" 6)" "a catalogued, nested, available model is assigned"
assert_equals "-" "$(field "$C1" 11)" "an assigned seat has no reason"
assert_equals "missing" "$(field "$C1" 7)" "an assigned seat is missing until execution fills its outcome"
assert_equals "unknown" "$(field "$C1" 9)" "an assigned seat starts with unknown observed identity"

C4=$(printf '%s\n' "$OUT" | sed -n 4p)
assert_equals "gpt-u" "$(field "$C4" 6)" "an entry with unknown identity is still an executable assignment"
assert_equals "unknown" "$(field "$C4" 8)" "the catalogue never copies into observed identity"

J1=$(printf '%s\n' "$OUT" | sed -n 5p)
assert_equals "judge" "$(field "$J1" 3)" "a judge seat keeps its kind"
assert_equals "c1" "$(field "$J1" 4)" "a judge seat keeps the seat it judges"
assert_equals "claude-j" "$(field "$J1" 6)" "a judge seat gets its own assignment"
pass "every requested seat has an assignment or an explicit unresolved result, in request order"

# --- a root worker model record alone never proves nested availability ------------

C2=$(printf '%s\n' "$OUT" | sed -n 2p)
assert_equals "unresolved" "$(field "$C2" 6)" "a valid entry with nested and available false is not assigned"
assert_contains "$(field "$C2" 11)" "nested capability unavailable for model gpt-root: nested=false available=false" "the unavailable nested capability is the reason"
assert_equals "gpt-root" "$(field "$C2" 5)" "the requested model is kept on the unresolved seat"
pass "a model with no nested capability is unresolved even when its record is valid"

# --- unsupported models are unresolved with a reason, never substituted -------------

C3=$(printf '%s\n' "$OUT" | sed -n 3p)
assert_equals "unresolved" "$(field "$C3" 6)" "an unsupported model is unresolved"
assert_equals "no capability entry for model gpt-x" "$(field "$C3" 11)" "the missing entry is the reason"
assert_equals "gpt-x" "$(field "$C3" 5)" "the original request is recorded"
assert_equals "missing" "$(field "$C3" 7)" "an unresolved seat cannot complete"

# Invariant: no seat is ever assigned a model other than the one it requested.
SUBSTITUTED=""
while IFS=$'\t' read -r sid _ _ _ requested resolved _; do
  [ "$resolved" = unresolved ] || [ "$resolved" = "$requested" ] || SUBSTITUTED="$SUBSTITUTED $sid"
done <<EOF
$OUT
EOF
assert_equals "" "$SUBSTITUTED" "no seat is silently assigned a different model"
pass "unsupported and unavailable models stay unresolved with the requested model and reason"

# --- an invalid or missing capability source resolves nothing ----------------------

BAD_CAP="$TMP/bad-capabilities.md"
write_cap "$BAD_CAP" \
  "$(entry gpt-a openai true true catalog acme)" \
  "$(entry gpt-y openai maybe true catalog)"
run_resolve "$BAD_CAP" "$REQ"
expect_code 0 "$CODE" "an invalid source still produces reported results"
assert_equals "5" "$(printf '%s\n' "$OUT" | wc -l | tr -d ' ')" "an invalid source still yields a record for every seat"
C1=$(printf '%s\n' "$OUT" | sed -n 1p)
assert_equals "unresolved" "$(field "$C1" 6)" "one invalid entry resolves no entry, even a valid one beside it"
assert_contains "$(field "$C1" 11)" "capability source invalid: " "the invalid source is the reason"
assert_contains "$(field "$C1" 11)" "nested must be true or false, got 'maybe'" "the named problem is carried into the reason"

run_resolve "$TMP/absent-capabilities.md" "$REQ"
expect_code 0 "$CODE" "a missing source still produces reported results"
assert_equals "unresolved" "$(field "$(printf '%s\n' "$OUT" | sed -n 4p)" 6)" "a missing source resolves no seat"
assert_contains "$(printf '%s\n' "$OUT" | sed -n 4p)" "capability source invalid: " "a missing source is an explicit unresolved reason"
pass "an invalid or missing capability source resolves no seat and names the problem"

TAB_CAP="$TMP/tab-capabilities.md"
printf '%s\n' '---' 'models:' $'\t- model_id: gpt-a' '---' > "$TAB_CAP"
run_resolve "$TAB_CAP" "$REQ"
expect_code 0 "$CODE" "a source with a tab-indented entry still produces reported results"
while IFS= read -r record; do
  assert_equals "12" "$(printf '%s' "$record" | awk -F'\t' '{ print NF }')" "a tab in the source error keeps the seat record at twelve fields"
done <<RECORDS
$OUT
RECORDS
printf '%s\n' "$OUT" > "$TMP/tab-seats.tsv"
run_report "$TMP/tab-seats.tsv"
expect_code 0 "$CODE" "the report reads seats resolved from a source with a tab in its error"
pass "a tab in the capability-source error does not break the seat record"

# --- execution failure keeps the request and reason, and the panel reports incomplete --

# The executing worker records outcomes: c1 completes, c4 fails on its assigned model, j1 completes.
EXECUTED="$TMP/executed.tsv"
run_resolve "$CAP" "$REQ"
printf '%s\n' "$OUT" | awk -F'\t' -v OFS='\t' '
  $1 == "c1" { $7 = "completed"; $8 = "gpt-a"; $9 = "openai"; $10 = "run-log-c1"; $12 = "out/c1.md" }
  $1 == "c4" { $7 = "failed"; $11 = "turn-timeout" }
  $1 == "j1" { $7 = "completed"; $8 = "claude-j"; $9 = "anthropic"; $10 = "run-log-j1"; $12 = "out/j1.md" }
  { print }' > "$EXECUTED"

run_report --min writer=2 "$EXECUTED"
expect_code 0 "$CODE" "a report over resolved and executed seats"
assert_contains "$OUT" "panel-status: incomplete" "a failed seat below the writer minimum is incomplete"
assert_contains "$OUT" "coverage-gap: seat c4 (role writer) outcome failed; reason: turn-timeout" "the failed seat is named with its reason"
assert_contains "$OUT" "coverage-gap: seat c2 (role writer) outcome missing; reason: nested capability unavailable for model gpt-root: nested=false available=false" "the unresolved seat is a named coverage gap"
assert_contains "$OUT" "minimum: role writer completed 1 of 2 required" "the writer minimum is counted from completed seats"
assert_contains "$OUT" "receipt: c4 role=writer kind=candidate judges=- requested=gpt-u resolved=gpt-u outcome=failed" "the failed seat's receipt keeps its assigned model"
assert_contains "$OUT" "completed-output: c1 out/c1.md" "the completed output stays available beside the gap"
assert_contains "$OUT" "completed-output: j1 out/j1.md" "the completed judge output stays available"
pass "a failed seat keeps its request and reason, names its coverage gap, and keeps completed outputs"

# --- AC6 chain: one panel read end to end ------------------------------------------

assert_contains "$OUT" "seats: candidate=4 judge=1 completed=2 missing=3" "the synthesis separates candidate, judge, completed, and missing seats"
assert_contains "$OUT" "diversity: cross-family across 2 families; executed models: c1=gpt-a/openai (evidence run-log-c1) j1=claude-j/anthropic (evidence run-log-j1)" "two verified families make a cross-family claim"
assert_contains "$OUT" "judge-separation: judge j1 family anthropic differs from judged seat c1 family openai" "the judge is separate from the judged seat"
pass "resolution, execution records, and synthesis read consistently"

# --- invalid requests are refused before any record is printed ---------------------

check_refused() {  # <label> <expected-error-fragment> <line>...
  local label=$1 want=$2 file="$TMP/refused-request.tsv"
  shift 2
  write_request "$file" "$@"
  run_resolve "$CAP" "$file"
  expect_code 1 "$CODE" "$label"
  assert_contains "$ERR" "$want" "$label names the problem"
  assert_equals "" "$OUT" "$label prints no records"
}

check_refused "a request line with the wrong field count" "expected 5 tab-separated fields" \
  $'c1\twriter\tcandidate\tgpt-a'
check_refused "a duplicate seat id" "duplicate seat_id 'c1'" \
  $'c1\twriter\tcandidate\t-\tgpt-a' \
  $'c1\twriter\tcandidate\t-\tgpt-b'
check_refused "a candidate that names a judged seat" "candidate seat 'c1' must name no judged seat" \
  $'c1\twriter\tcandidate\tc2\tgpt-a'
check_refused "a judge that names no seat" "judge seat 'j1' must name the seat it judges" \
  $'j1\treviewer\tjudge\t-\tclaude-j'
check_refused "a judge that names its own seat" "judge seat 'j1' must name a different seat in judges" \
  $'c1\twriter\tcandidate\t-\tgpt-a' \
  $'j1\treviewer\tjudge\tj1\tclaude-j'
check_refused "a judge that names a seat absent from the request" "judge seat 'j1' names unknown judged seat 'c9'" \
  $'c1\twriter\tcandidate\t-\tgpt-a' \
  $'j1\treviewer\tjudge\tc9\tclaude-j'
check_refused "a seat kind that is neither candidate nor judge" "seat_kind must be candidate or judge" \
  $'c1\twriter\tobserver\t-\tgpt-a'
check_refused "a requested model named unresolved" "needs one requested model token" \
  $'c1\twriter\tcandidate\t-\tunresolved'
check_refused "a seat with no role" "seat 'c1' needs a role name" \
  $'c1\t-\tcandidate\t-\tgpt-a'

write_request "$TMP/empty-request.tsv"
run_resolve "$CAP" "$TMP/empty-request.tsv"
expect_code 1 "$CODE" "a request with no seats"
assert_contains "$ERR" "has no seat requests" "an empty request is refused"

run_resolve "$CAP" "$TMP/absent-request.tsv"
expect_code 1 "$CODE" "a request file that cannot be read"
assert_contains "$ERR" "cannot read panel request" "an unreadable request is refused"
pass "invalid requests exit 1, name the problem, and print nothing"

# --- usage -------------------------------------------------------------------------

run_resolve "$CAP"
expect_code 2 "$CODE" "resolve without a request file"

run_resolve "" "$REQ"
expect_code 2 "$CODE" "resolve with an empty capability path"

OUT=$(bash "$RESOLVE" frobnicate 2>"$ERR_FILE")
CODE=$?
expect_code 2 "$CODE" "an unknown subcommand"
pass "usage errors exit 2"
