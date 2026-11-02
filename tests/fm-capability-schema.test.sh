#!/usr/bin/env bash
# Behavior tests for bin/fm-capability-schema.sh: the capability-source schema,
# approved as option A under approval record corr f425be7d1020d5f1. Every case drives
# the executable with a real model-capabilities.md file and asserts its stdout, stderr,
# and exit status. The contract is docs/panel-receipts.md.
# These tests pin the module's behavior only and assert nothing about AC1, AC4, or
# AC6 completion.
set -u

# shellcheck source=tests/lib.sh
. "$(dirname "${BASH_SOURCE[0]}")/lib.sh"

SCHEMA="$ROOT/bin/fm-capability-schema.sh"
TMP=$(fm_test_tmproot fm-capability-schema)
ERR_FILE="$TMP/err"
OUT=""
ERR=""
CODE=0

run_cap() {
  OUT=$(bash "$SCHEMA" "$@" 2>"$ERR_FILE")
  CODE=$?
  ERR=$(cat "$ERR_FILE")
}

# entry <model_id> <family> <nested> <available> <identity_source> [provider]
entry() {
  printf '  - model_id: %s\n    family: %s\n    nested: %s\n    available: %s\n    identity_source: %s\n' \
    "$1" "$2" "$3" "$4" "$5"
  [ -z "${6:-}" ] || printf '    provider: %s\n' "$6"
}

write_doc() {  # <file> <front-matter-body>
  printf '%s\n' '---' 'models:' "$2" '---' 'prose after the front matter' > "$1"
}

GOOD_A=$(entry gpt-a openai true true catalog acme)
GOOD_U=$(entry gpt-u unknown false false unknown)
VALID="$TMP/valid.md"
write_doc "$VALID" "$GOOD_A"$'\n'"$GOOD_U"

# --- valid source and lookup ---------------------------------------------------

run_cap validate "$VALID"
expect_code 0 "$CODE" "a valid capability source"
assert_equals "" "$OUT" "validate prints nothing on success"

run_cap lookup "$VALID" gpt-a
expect_code 0 "$CODE" "lookup of a catalogued model"
assert_equals $'resolved\tgpt-a\topenai\ttrue\ttrue\tcatalog\tacme' "$OUT" "lookup prints the exact resolved row"

run_cap lookup "$VALID" gpt-u
expect_code 0 "$CODE" "lookup of an entry with unknown identity"
assert_equals $'resolved\tgpt-u\tunknown\tfalse\tfalse\tunknown\t-' "$OUT" "an omitted provider prints as - and unknown family stays unknown"

run_cap lookup "$VALID" nope
expect_code 1 "$CODE" "lookup of a model with no entry"
assert_contains "$OUT" "unresolved: model nope has no entry" "a missing entry is an explicit unresolved result"

run_cap lookup "$TMP/absent.md" gpt-a
expect_code 1 "$CODE" "lookup against a missing file"
assert_contains "$OUT" "unresolved: $TMP/absent.md is not a valid capability source" "a missing file is an explicit unresolved result"
assert_contains "$ERR" "no capability source at $TMP/absent.md" "the missing file is named on stderr"
pass "a valid source validates and resolves; a missing model or file is an explicit unresolved result"

# --- invalid sources are refused ------------------------------------------------

check_invalid() {  # <label> <expected-fragment> <front-matter-body>
  local label=$1 want=$2 file="$TMP/invalid.md"
  write_doc "$file" "$3"
  run_cap validate "$file"
  expect_code 1 "$CODE" "$label"
  assert_contains "$ERR" "$want" "$label names the problem"
  run_cap lookup "$file" gpt-x
  expect_code 1 "$CODE" "$label does not resolve"
  assert_contains "$OUT" "unresolved: " "$label reports an unresolved lookup"
}

check_invalid "nested not boolean" "nested must be true or false, got 'maybe'" \
  "$(entry gpt-x openai maybe false catalog)"

check_invalid "available not boolean" "available must be true or false, got 'maybe'" \
  "$(entry gpt-x openai true maybe catalog)"

check_invalid "available without nested" "available true requires nested true" \
  "$(entry gpt-x openai false true catalog)"

check_invalid "known family without identity" "needs an identity_source of catalog or observed, not unknown" \
  "$(entry gpt-x openai true true unknown)"

check_invalid "unknown family with identity" "family unknown must carry identity_source unknown" \
  "$(entry gpt-x unknown false false catalog)"

check_invalid "unknown identity source" "identity_source must be catalog, observed, or unknown, got 'sideways'" \
  "$(entry gpt-x openai true true sideways)"

check_invalid "dash as model id" "model_id must be one token and not '-'" \
  "$(entry - openai true true catalog)"

check_invalid "duplicate model id" "duplicate model_id" \
  "$(entry gpt-x openai true true catalog)"$'\n'"$(entry gpt-x anthropic true true catalog)"

check_invalid "missing required field" "missing required field identity_source" \
  "  - model_id: gpt-x"$'\n'"    family: openai"$'\n'"    nested: true"$'\n'"    available: true"

check_invalid "unknown field" "unknown field 'color'" \
  "$(entry gpt-x openai true true catalog)"$'\n'"    color: red"

check_invalid "duplicate field in one entry" "duplicate field 'family'" \
  "$(entry gpt-x openai true true catalog)"$'\n'"    family: openai"

check_invalid "field before any model" "field 'family' appears before any model_id" \
  "    family: openai"

check_invalid "tab in a value" "tab in value of 'family'" \
  "  - model_id: gpt-x"$'\n'"    family: open"$'\t'"ai"$'\n'"    nested: true"$'\n'"    available: true"$'\n'"    identity_source: catalog"

NO_MODELS="$TMP/no-models.md"
printf '%s\n' '---' '---' 'prose' > "$NO_MODELS"
run_cap validate "$NO_MODELS"
expect_code 1 "$CODE" "front matter with no models key"
assert_contains "$ERR" "has no models: key" "a source without the models key is refused"

NO_FRONT="$TMP/no-front.md"
printf '%s\n' 'just prose' > "$NO_FRONT"
run_cap validate "$NO_FRONT"
expect_code 1 "$CODE" "a file with no front matter"
assert_contains "$ERR" "line 1 must open the front matter with ---" "a file without front matter is refused"

UNCLOSED="$TMP/unclosed.md"
printf '%s\n' '---' 'models:' "$GOOD_A" > "$UNCLOSED"
run_cap validate "$UNCLOSED"
expect_code 1 "$CODE" "unclosed front matter"
assert_contains "$ERR" "front matter is not closed with ---" "unclosed front matter is refused"
pass "invalid sources are refused with the named problem and never resolve"

# --- one bad entry resolves nothing --------------------------------------------

PARTIAL="$TMP/partial.md"
write_doc "$PARTIAL" "$GOOD_A"$'\n'"  - model_id: gpt-y"$'\n'"    family: openai"
run_cap lookup "$PARTIAL" gpt-a
expect_code 1 "$CODE" "a valid entry beside an invalid one"
assert_contains "$OUT" "unresolved: " "the valid entry does not resolve when its file is invalid"
assert_not_contains "$OUT" $'resolved\tgpt-a' "no partial resolution from an invalid file"
pass "a file with one invalid entry resolves no entry"

# --- usage ---------------------------------------------------------------------

run_cap lookup "$VALID"
expect_code 2 "$CODE" "lookup without a model id"

run_cap frobnicate
expect_code 2 "$CODE" "an unknown subcommand"
pass "usage errors exit 2"
