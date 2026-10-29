#!/usr/bin/env bash
# Format and seating checks for the candidate panel-assignment fixtures in
# tests/fixtures/panel-assignments/candidate/. The fixtures are consumed unchanged by
# bin/fm-capability-schema.sh and bin/fm-panel-resolve.sh. These cases prove the files
# parse, carry approved-as-authored status, and seat exactly the selected panel roles.
# They do not prove a panel ran, and they do not by themselves satisfy DPES-781 AC8 or
# T9. The contract is docs/panel-receipts.md.
set -u

# shellcheck source=tests/lib.sh
. "$(dirname "${BASH_SOURCE[0]}")/lib.sh"

CAND="$ROOT/tests/fixtures/panel-assignments/candidate"
CAP="$CAND/model-capabilities.md"
MANIFEST="$CAND/manifest.json"
CAPTOOL="$ROOT/bin/fm-capability-schema.sh"
RESOLVE="$ROOT/bin/fm-panel-resolve.sh"
TMP=$(fm_test_tmproot fm-panel-assignments)
ERR_FILE="$TMP/err"

# --- the shared capability source is a valid capability source ---------------------

OUT=$(bash "$CAPTOOL" validate "$CAP" 2>"$ERR_FILE")
CODE=$?
expect_code 0 "$CODE" "the candidate capability source validates"

# --- every fixture is approved as authored and never satisfies AC8 or T9 ----------

TOTAL=$(jq -r '.fixtures | length' "$MANIFEST")
APPROVED=$(jq -r '[.fixtures[] | select(.approval_status == "approved-as-authored" and .satisfies_ac8_t9 == false)] | length' "$MANIFEST")
assert_equals "$TOTAL" "$APPROVED" "every manifest fixture is approved as authored and does not satisfy AC8 or T9"

# --- every fixture that names a panel request has a request that resolves ----------

REQUESTS=0
while IFS= read -r rel; do
  [ -n "$rel" ] || continue
  REQUESTS=$((REQUESTS + 1))
  req="$CAND/$rel"
  [ -r "$req" ] || fail "manifest names a panel request that is not readable: $rel"
  OUT=$(bash "$RESOLVE" resolve "$CAP" "$req" 2>"$ERR_FILE")
  CODE=$?
  expect_code 0 "$CODE" "$rel resolves against the candidate capability source"
  BAD=$(printf '%s\n' "$OUT" | awk -F'\t' 'NF != 12' | wc -l | tr -d ' ')
  assert_equals "0" "$BAD" "$rel emits only twelve-field seat records"
done < <(jq -r '.fixtures[] | select(.panel_request != null) | .panel_request' "$MANIFEST")

[ "$REQUESTS" -gt 0 ] || fail "the manifest names no panel request to check"

# --- T9: the resolver seats exactly the selected panel roles, and capability availability adds no review stage ---

SELECTED="$CAND/small-low-risk/panel-request.tsv"
SELECTED_SEATS=$(awk 'END { print NR }' "$SELECTED")
SELECTED_ROLES=$(awk -F'\t' '{ print $2 }' "$SELECTED" | paste -sd, -)

OUT=$(bash "$RESOLVE" resolve "$CAP" "$SELECTED" 2>"$ERR_FILE")
CODE=$?
expect_code 0 "$CODE" "T9: the selected request resolves against the available capability source"
assert_equals "$SELECTED_SEATS" "$(printf '%s\n' "$OUT" | wc -l | tr -d ' ')" "T9: the resolver emits one seat per selected seat and no others"
assert_equals "$SELECTED_ROLES" "$(printf '%s\n' "$OUT" | awk -F'\t' '{ print $2 }' | paste -sd, -)" "T9: the resolver seats exactly the selected panel roles"
# t9_seats_match_request <request> <resolver-output>: exits 0 only when every seat in the
# resolver output resolves to the model that the selected request names for that seat. The
# request is the oracle, so a resolver whose own requested and resolved columns agree with
# each other but name the wrong model cannot pass.
t9_seats_match_request() {
  local matched
  matched=$(awk -F'\t' 'NR == FNR { want[$1] = $5; next } $6 == want[$1] { n++ } END { print n + 0 }' "$1" <(printf '%s\n' "$2"))
  [ "$matched" -eq "$(awk 'END { print NR }' "$1")" ]
}

t9_seats_match_request "$SELECTED" "$OUT"
CODE=$?
expect_code 0 "$CODE" "T9: each selected seat resolves to the model its selected request names, when the capability is available"

# The swapped output moves the writer and reviewer models into each other's seats in both model
# columns. Its columns still agree row by row, so only the selected request can reveal the swap.
WRITER_MODEL=$(printf '%s\n' "$OUT" | awk -F'\t' '$2 == "writer" { print $5 }')
REVIEWER_MODEL=$(printf '%s\n' "$OUT" | awk -F'\t' '$2 == "reviewer" { print $5 }')
SWAPPED=$(printf '%s\n' "$OUT" | awk -F'\t' -v OFS='\t' -v w="$WRITER_MODEL" -v r="$REVIEWER_MODEL" '
  $2 == "writer" { $5 = r; $6 = r }
  $2 == "reviewer" { $5 = w; $6 = w }
  { print }')

t9_seats_match_request "$SELECTED" "$SWAPPED"
CODE=$?
expect_code 1 "$CODE" "T9: a swapped writer and reviewer model in the resolver output fails the selected-request check"

UNAVAILABLE_CAP="$TMP/model-capabilities-unavailable.md"
sed 's/available: true/available: false/' "$CAP" > "$UNAVAILABLE_CAP"
OUT=$(bash "$RESOLVE" resolve "$UNAVAILABLE_CAP" "$SELECTED" 2>"$ERR_FILE")
CODE=$?
expect_code 0 "$CODE" "T9: the selected request resolves against an unavailable capability source"
assert_equals "$SELECTED_SEATS" "$(printf '%s\n' "$OUT" | wc -l | tr -d ' ')" "T9: capability unavailability adds no seat"
assert_equals "$SELECTED_ROLES" "$(printf '%s\n' "$OUT" | awk -F'\t' '{ print $2 }' | paste -sd, -)" "T9: capability unavailability adds no review stage"
assert_equals "$SELECTED_SEATS" "$(printf '%s\n' "$OUT" | awk -F'\t' '$6 == "unresolved" && $11 ~ /^nested capability unavailable/' | wc -l | tr -d ' ')" "T9: capability unavailability leaves every selected seat unresolved for that reason"

pass "candidate panel-assignment fixtures parse through the existing consumers and seat exactly the selected roles"
