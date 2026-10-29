#!/usr/bin/env bash
# Behavior tests for bin/fm-local-handoff.sh (DPES-880) custody records. A
# refusal names its gate on stderr, exits 2, and leaves every stored receipt
# byte-identical. Expected identities come from a literal logical-key string
# hashed here, never from the script under test.
set -u

# shellcheck source=tests/lib.sh
. "$(dirname "${BASH_SOURCE[0]}")/lib.sh"

HANDOFF="$ROOT/bin/fm-local-handoff.sh"
TMP_ROOT=$(fm_test_tmproot fm-local-handoff)

ORACLE_KEY='{"owner":"firstmate-pstack","result_digest":"sha256:0a1b","result_generation":"1","run_id":"run-7","task_id":"dpes-880-fixture","task_incarnation":"inc-1"}'
HEAD_SHA=0123456789abcdef0123456789abcdef01234567
OTHER_SHA=fedcba9876543210fedcba9876543210fedcba98

oracle_identity() {
  if command -v sha256sum >/dev/null 2>&1; then
    printf '%s' "$ORACLE_KEY" | sha256sum | awk '{print $1}'
  else
    printf '%s' "$ORACLE_KEY" | shasum -a 256 | awk '{print $1}'
  fi
}
EXPECTED_IDENTITY=$(oracle_identity)

write_binding() {  # <file>
  cat >"$1" <<JSON
{"owner":"firstmate-pstack","task_id":"dpes-880-fixture","task_incarnation":"inc-1",
 "run_id":"run-7","result_generation":"1","result_digest":"sha256:0a1b",
 "repo":"mc-innersource/motif-hve-multi-agent-orchestrator","base_commit":"1c4ce98",
 "source_ref":"refs/heads/main","import_receipt":"imp-1",
 "worktree":"/work/dpes-880-fixture","branch":"fm/DPES-880-fixture",
 "imported_head":"$HEAD_SHA","delivery_mode":"direct-PR","pr_base":"main"}
JSON
}

# hf <data-dir> <args...>: run the script under bash with an explicit data root.
hf() {
  local data=$1
  shift
  FM_DATA_OVERRIDE="$data" bash "$HANDOFF" "$@"
}

# New case directory with a valid binding at $dir/binding.json and data at $dir/data.
new_case() {  # <name> -> sets CASE_DIR, CASE_DATA, CASE_BINDING
  CASE_DIR="$TMP_ROOT/$1"
  CASE_DATA="$CASE_DIR/data"
  CASE_BINDING="$CASE_DIR/binding.json"
  mkdir -p "$CASE_DATA"
  write_binding "$CASE_BINDING"
}

# Refused case: rc must be 2, stderr must name the gate, and no handoff dir may exist.
expect_refusal() {  # <name> <binding-file> <gate-substring> <description>
  local err="$CASE_DIR/err.$1" rc
  hf "$CASE_DATA" record "$2" >/dev/null 2>"$err"
  rc=$?
  [ "$rc" -eq 2 ] || fail "$4: expected exit 2, got $rc"
  grep -q "refused: $3" "$err" || fail "$4: stderr did not name gate '$3': $(cat "$err")"
  [ ! -d "$CASE_DATA/handoffs" ] || fail "$4: a refused record left a handoffs directory"
  pass "$4"
}

# --- valid custody ----------------------------------------------------------

new_case valid
out=$(hf "$CASE_DATA" record "$CASE_BINDING") || fail "valid binding should record (exit $?)"
[ "$(printf '%s\n' "$out" | sed -n 1p)" = "$EXPECTED_IDENTITY" ] \
  || fail "identity must be the logical-key digest; got: $out"
[ "$(printf '%s\n' "$out" | sed -n 2p)" = "phase=pending_custody" ] \
  || fail "a new receipt starts in pending_custody; got: $out"
RECEIPT="$CASE_DATA/handoffs/$EXPECTED_IDENTITY.json"
[ "$(jq -r .phase "$RECEIPT")" = pending_custody ] || fail "stored phase is not pending_custody"
[ "$(jq -r .binding.imported_head "$RECEIPT")" = "$HEAD_SHA" ] \
  || fail "stored receipt must bind the imported head exactly"
[ "$(jq -r .hold "$RECEIPT")" = null ] || fail "a new receipt carries no hold"
pass "valid binding is durably recorded as pending_custody under its logical-key identity"

cp "$RECEIPT" "$CASE_DIR/receipt.before"
out=$(hf "$CASE_DATA" record "$CASE_BINDING") || fail "replay of the same binding must succeed (exit $?)"
[ "$(printf '%s\n' "$out" | sed -n 1p)" = "$EXPECTED_IDENTITY" ] || fail "replay must reuse the same identity"
cmp -s "$RECEIPT" "$CASE_DIR/receipt.before" || fail "replay must leave the stored receipt byte-identical"
pass "replaying the same binding converges on the same receipt without rewriting it"

# --- changed binding under the same logical key -----------------------------

new_case changed
jq --arg sha "$OTHER_SHA" '.imported_head = $sha' "$CASE_BINDING" >"$CASE_DIR/changed.json"
hf "$CASE_DATA" record "$CASE_BINDING" >/dev/null || fail "setup: original binding must record"
RECEIPT="$CASE_DATA/handoffs/$EXPECTED_IDENTITY.json"
cp "$RECEIPT" "$CASE_DIR/receipt.before"
err="$CASE_DIR/err.changed"
hf "$CASE_DATA" record "$CASE_DIR/changed.json" >/dev/null 2>"$err"
[ $? -eq 2 ] || fail "a changed binding under an existing identity must be refused with exit 2"
grep -q 'refused: identity: changed binding' "$err" \
  || fail "changed binding must name the identity gate; got: $(cat "$err")"
cmp -s "$RECEIPT" "$CASE_DIR/receipt.before" || fail "refused changed binding must leave the receipt byte-identical"
pass "a changed binding under the same logical key is refused and the receipt stays unchanged"

# --- fail-closed schema and delivery gates ----------------------------------

new_case local_only
jq '.delivery_mode = "local-only"' "$CASE_BINDING" >"$CASE_DIR/local.json"
expect_refusal local_only "$CASE_DIR/local.json" delivery-mode "local-only binding is refused before any state is written"

new_case missing_key
jq 'del(.pr_base)' "$CASE_BINDING" >"$CASE_DIR/missing.json"
expect_refusal missing_key "$CASE_DIR/missing.json" schema "binding missing a required key is refused"

new_case extra_key
jq '. + {"secret_url": "https://example.invalid/?token=x"}' "$CASE_BINDING" >"$CASE_DIR/extra.json"
expect_refusal extra_key "$CASE_DIR/extra.json" schema "binding with an unknown key is refused, never stored"

new_case empty_value
jq '.run_id = ""' "$CASE_BINDING" >"$CASE_DIR/empty.json"
expect_refusal empty_value "$CASE_DIR/empty.json" schema "binding with an empty value is refused"

new_case relative_worktree
jq '.worktree = "work/dpes-880-fixture"' "$CASE_BINDING" >"$CASE_DIR/relative.json"
expect_refusal relative_worktree "$CASE_DIR/relative.json" schema "relative worktree path is refused"

new_case not_object
printf '%s\n' '["not","an","object"]' >"$CASE_DIR/array.json"
expect_refusal not_object "$CASE_DIR/array.json" input "non-object binding input is refused"

# --- explicit location and usage --------------------------------------------

new_case no_home
err="$CASE_DIR/err.nohome"
if env -u FM_HOME -u FM_DATA_OVERRIDE bash "$HANDOFF" record "$CASE_BINDING" >/dev/null 2>"$err"; then
  fail "without FM_HOME or FM_DATA_OVERRIDE the script must fail closed"
fi
grep -q 'FM_HOME' "$err" || fail "fail-closed message must name FM_HOME; got: $(cat "$err")"
pass "without an explicit home the script fails closed instead of writing to a default location"

bash "$HANDOFF" >/dev/null 2>&1
[ $? -eq 64 ] || fail "no arguments is a usage error (exit 64)"
pass "no subcommand is a usage error"

# --- concurrent consumers ---------------------------------------------------

new_case race
RACE="$CASE_DIR/race"
mkdir -p "$RACE"
for i in 1 2 3 4 5 6 7 8; do
  (hf "$CASE_DATA" record "$CASE_BINDING" >"$RACE/out.$i" 2>"$RACE/err.$i"; echo $? >"$RACE/rc.$i") &
done
wait
for i in 1 2 3 4 5 6 7 8; do
  [ "$(cat "$RACE/rc.$i")" -eq 0 ] || fail "racing consumer $i must succeed as create-or-replay, got rc $(cat "$RACE/rc.$i")"
  [ "$(sed -n 1p "$RACE/out.$i")" = "$EXPECTED_IDENTITY" ] || fail "racing consumer $i reported a different identity"
done
[ "$(find "$CASE_DATA/handoffs" -mindepth 1 -maxdepth 1 | wc -l | tr -d ' ')" -eq 1 ] \
  || fail "racing consumers must leave exactly one receipt and no temp files; got: $(ls -A "$CASE_DATA/handoffs")"
pass "eight concurrent records of one binding create exactly one receipt"

# --- show -------------------------------------------------------------------

new_case show
hf "$CASE_DATA" record "$CASE_BINDING" >/dev/null || fail "setup: record for show"
[ "$(hf "$CASE_DATA" show "$EXPECTED_IDENTITY" | jq -r .identity)" = "$EXPECTED_IDENTITY" ] \
  || fail "show must print the stored receipt for its identity"
hf "$CASE_DATA" show "0000000000000000000000000000000000000000000000000000000000000000" >/dev/null 2>"$CASE_DIR/err.show"
[ $? -eq 2 ] || fail "show of an unknown identity must refuse with exit 2"
pass "show prints a stored receipt and refuses an unknown identity"

# --- advance: forward-only phases with evidence -----------------------------

# advance_refused <desc> <gate> <args...>: the refusal exits 2, names its gate,
# and leaves the stored receipt byte-identical.
advance_refused() {
  local desc=$1 gate=$2 err="$CASE_DIR/err.refused" rc
  shift 2
  cp "$RECEIPT" "$CASE_DIR/receipt.before"
  hf "$CASE_DATA" "$@" >/dev/null 2>"$err"
  rc=$?
  [ "$rc" -eq 2 ] || fail "$desc: expected exit 2, got $rc"
  grep -q "refused: $gate" "$err" || fail "$desc: stderr did not name gate '$gate': $(cat "$err")"
  cmp -s "$RECEIPT" "$CASE_DIR/receipt.before" || fail "$desc: refused call changed the receipt"
  pass "$desc"
}

# advance_ok <desc> <args...>: the call must exit 0.
advance_ok() {
  local desc=$1
  shift
  hf "$CASE_DATA" "$@" >/dev/null || fail "$desc: expected success (exit $?)"
  pass "$desc"
}

# ack_file <file> <head>: an adoption ack that matches the fixture binding.
ack_file() {
  printf '{"task_id":"dpes-880-fixture","task_incarnation":"inc-1","worktree":"/work/dpes-880-fixture","head":"%s"}\n' "$2" >"$1"
}

new_case advance
hf "$CASE_DATA" record "$CASE_BINDING" >/dev/null || fail "setup: record for advance"
RECEIPT="$CASE_DATA/handoffs/$EXPECTED_IDENTITY.json"

advance_refused "skipping straight to adoption_acknowledged is refused" "transition: cannot skip" \
  advance "$EXPECTED_IDENTITY" adoption_acknowledged
advance_refused "pending_custody is only set by record" "transition: pending_custody" \
  advance "$EXPECTED_IDENTITY" pending_custody
advance_refused "an unknown phase is refused" "transition: unknown phase" \
  advance "$EXPECTED_IDENTITY" bogus
advance_refused "assignment_intent without its evidence is refused" "evidence:" \
  advance "$EXPECTED_IDENTITY" assignment_intent
advance_refused "assignment_intent for a task other than the binding is refused" "evidence:" \
  advance "$EXPECTED_IDENTITY" assignment_intent --local-task other-task
advance_refused "evidence foreign to the phase is refused" "evidence:" \
  advance "$EXPECTED_IDENTITY" assignment_intent --local-task dpes-880-fixture --producer direct-PR
advance_refused "an unknown identity is refused" "identity:" \
  advance 0000 assignment_intent --local-task dpes-880-fixture
advance_refused "a repeated evidence option is refused" "input:" \
  advance "$EXPECTED_IDENTITY" assignment_intent --local-task dpes-880-fixture --local-task other-task

advance_ok "assignment_intent for the binding task advances the receipt" \
  advance "$EXPECTED_IDENTITY" assignment_intent --local-task dpes-880-fixture
[ "$(jq -r .phase "$RECEIPT")" = assignment_intent ] || fail "phase did not advance to assignment_intent"
[ "$(jq -c .evidence.assignment_intent "$RECEIPT")" = '{"local_task":"dpes-880-fixture"}' ] \
  || fail "assignment evidence was not stored canonically: $(jq -c .evidence "$RECEIPT")"
cp "$RECEIPT" "$CASE_DIR/receipt.before"
advance_ok "replaying the current phase with the same evidence converges" \
  advance "$EXPECTED_IDENTITY" assignment_intent --local-task dpes-880-fixture
cmp -s "$RECEIPT" "$CASE_DIR/receipt.before" || fail "a replay with the same evidence must not rewrite the receipt"

ack_file "$CASE_DIR/ack.ok.json" "$HEAD_SHA"
ack_file "$CASE_DIR/ack.wrong.json" "$OTHER_SHA"
jq '. + {"extra": "x"}' "$CASE_DIR/ack.ok.json" >"$CASE_DIR/ack.extra.json"
advance_refused "an adoption ack for another head is refused" "evidence:" \
  advance "$EXPECTED_IDENTITY" adoption_acknowledged --ack "$CASE_DIR/ack.wrong.json"
advance_refused "an adoption ack with an extra field is refused" "evidence:" \
  advance "$EXPECTED_IDENTITY" adoption_acknowledged --ack "$CASE_DIR/ack.extra.json"
advance_ok "an adoption ack matching the imported checkout advances the receipt" \
  advance "$EXPECTED_IDENTITY" adoption_acknowledged --ack "$CASE_DIR/ack.ok.json"

advance_refused "a producer other than the binding delivery mode is refused" "evidence:" \
  advance "$EXPECTED_IDENTITY" producer_running --producer no-mistakes
advance_ok "a producer matching the binding delivery mode advances the receipt" \
  advance "$EXPECTED_IDENTITY" producer_running --producer direct-PR
advance_refused "moving back to adoption_acknowledged is refused" "transition: cannot move backward" \
  advance "$EXPECTED_IDENTITY" adoption_acknowledged --ack "$CASE_DIR/ack.ok.json"

hf "$CASE_DATA" hold "$EXPECTED_IDENTITY" --reason "captain review pending" >/dev/null \
  || fail "hold with a reason must succeed"
[ "$(jq -r .hold.reason "$RECEIPT")" = "captain review pending" ] || fail "hold reason was not stored"
advance_refused "a held receipt accepts no advance" "hold:" \
  advance "$EXPECTED_IDENTITY" pr_intent --branch fm/DPES-880-fixture --base main --head "$HEAD_SHA"
hf "$CASE_DATA" hold "$EXPECTED_IDENTITY" >/dev/null 2>&1
[ $? -eq 64 ] || fail "hold without --reason or --clear is a usage error (exit 64)"
advance_ok "clearing the hold lets the receipt advance again" hold "$EXPECTED_IDENTITY" --clear
[ "$(jq -r .hold "$RECEIPT")" = null ] || fail "clearing the hold must leave no hold"

advance_refused "a PR branch other than the binding branch is refused" "evidence:" \
  advance "$EXPECTED_IDENTITY" pr_intent --branch main --base main --head "$HEAD_SHA"
advance_refused "a PR base other than the binding pr_base is refused" "evidence:" \
  advance "$EXPECTED_IDENTITY" pr_intent --branch fm/DPES-880-fixture --base develop --head "$HEAD_SHA"
advance_refused "a non-hex PR head is refused" "evidence:" \
  advance "$EXPECTED_IDENTITY" pr_intent --branch fm/DPES-880-fixture --base main --head not-a-sha
advance_ok "a PR intent matching the binding advances the receipt" \
  advance "$EXPECTED_IDENTITY" pr_intent --branch fm/DPES-880-fixture --base main --head "$HEAD_SHA"
advance_refused "a PR intent replay with a different head is refused" "evidence:" \
  advance "$EXPECTED_IDENTITY" pr_intent --branch fm/DPES-880-fixture --base main --head "$OTHER_SHA"
advance_refused "pr_observed waits for PR read-back evidence" "evidence:" \
  advance "$EXPECTED_IDENTITY" pr_observed

# pr_readback <file> <url> <number-json> <head> <base> <draft-json>: a forge read-back.
pr_readback() {
  printf '{"base":"%s","draft":%s,"head":"%s","number":%s,"url":"%s"}\n' "$5" "$6" "$4" "$3" "$2" >"$1"
}
PR_URL="https://github.com/mc-innersource/motif-hve-multi-agent-orchestrator/pull/42"
pr_readback "$CASE_DIR/pr.ok.json" "$PR_URL" 42 "$HEAD_SHA" main false
pr_readback "$CASE_DIR/pr.head.json" "$PR_URL" 42 "$OTHER_SHA" main false
pr_readback "$CASE_DIR/pr.base.json" "$PR_URL" 42 "$HEAD_SHA" develop false
pr_readback "$CASE_DIR/pr.repo.json" "https://github.com/mc-innersource/other-repo/pull/42" 42 "$HEAD_SHA" main false
pr_readback "$CASE_DIR/pr.number.json" "$PR_URL" '"42"' "$HEAD_SHA" main false
pr_readback "$CASE_DIR/pr.draft.json" "$PR_URL" 42 "$HEAD_SHA" main '"no"'
jq '. + {"state": "open"}' "$CASE_DIR/pr.ok.json" >"$CASE_DIR/pr.extra.json"

advance_refused "a read-back with another head than the PR intent is refused" "evidence:" \
  advance "$EXPECTED_IDENTITY" pr_observed --pr "$CASE_DIR/pr.head.json"
advance_refused "a read-back with another base than the PR intent is refused" "evidence:" \
  advance "$EXPECTED_IDENTITY" pr_observed --pr "$CASE_DIR/pr.base.json"
advance_refused "a read-back URL for another repo is refused" "evidence:" \
  advance "$EXPECTED_IDENTITY" pr_observed --pr "$CASE_DIR/pr.repo.json"
advance_refused "a read-back with a string PR number is refused" "evidence:" \
  advance "$EXPECTED_IDENTITY" pr_observed --pr "$CASE_DIR/pr.number.json"
advance_refused "a read-back with a non-boolean draft state is refused" "evidence:" \
  advance "$EXPECTED_IDENTITY" pr_observed --pr "$CASE_DIR/pr.draft.json"
advance_refused "a read-back with an extra field is refused" "evidence:" \
  advance "$EXPECTED_IDENTITY" pr_observed --pr "$CASE_DIR/pr.extra.json"
advance_ok "a read-back matching the PR intent records pr_observed" \
  advance "$EXPECTED_IDENTITY" pr_observed --pr "$CASE_DIR/pr.ok.json"
[ "$(jq -r .phase "$RECEIPT")" = pr_observed ] || fail "phase did not advance to pr_observed"
[ "$(jq -c .evidence.pr_observed "$RECEIPT")" = "$(jq -S -c . "$CASE_DIR/pr.ok.json")" ] \
  || fail "read-back evidence was not stored canonically: $(jq -c .evidence.pr_observed "$RECEIPT")"
cp "$RECEIPT" "$CASE_DIR/receipt.before"
advance_ok "replaying pr_observed with the same read-back converges" \
  advance "$EXPECTED_IDENTITY" pr_observed --pr "$CASE_DIR/pr.ok.json"
cmp -s "$RECEIPT" "$CASE_DIR/receipt.before" || fail "a replayed read-back must not rewrite the receipt"

new_case corrupt_phase
hf "$CASE_DATA" record "$CASE_BINDING" >/dev/null || fail "setup: record for corrupt phase"
RECEIPT="$CASE_DATA/handoffs/$EXPECTED_IDENTITY.json"
jq '.phase = "bogus"' "$RECEIPT" >"$CASE_DIR/corrupt.json" && mv "$CASE_DIR/corrupt.json" "$RECEIPT"
advance_refused "a receipt with an unknown stored phase is refused, not guessed past" "receipt:" \
  advance "$EXPECTED_IDENTITY" assignment_intent --local-task dpes-880-fixture

new_case advance_race
hf "$CASE_DATA" record "$CASE_BINDING" >/dev/null || fail "setup: record for advance race"
RACE="$CASE_DIR/race"
mkdir -p "$RACE"
for i in 1 2 3 4 5 6; do
  (hf "$CASE_DATA" advance "$EXPECTED_IDENTITY" assignment_intent --local-task dpes-880-fixture \
    >"$RACE/out.$i" 2>"$RACE/err.$i"; echo $? >"$RACE/rc.$i") &
done
wait
for i in 1 2 3 4 5 6; do
  [ "$(cat "$RACE/rc.$i")" -eq 0 ] || fail "racing advance $i must converge on success, got rc $(cat "$RACE/rc.$i"): $(cat "$RACE/err.$i")"
done
RECEIPT="$CASE_DATA/handoffs/$EXPECTED_IDENTITY.json"
[ "$(jq -r .phase "$RECEIPT")" = assignment_intent ] || fail "racing advances must leave the receipt at assignment_intent"
[ "$(ls -A "$CASE_DATA/handoffs")" = "$EXPECTED_IDENTITY.json" ] \
  || fail "racing advances must leave no lock or temp files; got: $(ls -A "$CASE_DATA/handoffs")"
pass "six concurrent advances to one phase converge without leaving lock or temp files"

# --- DPES-880 review findings (PR 142) ----------------------------------------
#
# Each case below failed on origin/main before the fix: line-anchored identity
# and head checks, exact-match comparisons that $(...) newline stripping
# weakened, a hash-tool failure that published under an empty identity, a
# replay that trusted an edited stored binding, a non-hex imported head, and
# custody acknowledged without a durable flush.

NL=$'\n'

# reach_producer_running: the current case's receipt through assignment,
# adoption, and producer evidence that match the fixture binding.
reach_producer_running() {
  ack_file "$CASE_DIR/ack.ok.json" "$HEAD_SHA"
  hf "$CASE_DATA" advance "$EXPECTED_IDENTITY" assignment_intent --local-task dpes-880-fixture >/dev/null \
    || fail "setup: assignment_intent"
  hf "$CASE_DATA" advance "$EXPECTED_IDENTITY" adoption_acknowledged --ack "$CASE_DIR/ack.ok.json" >/dev/null \
    || fail "setup: adoption_acknowledged"
  hf "$CASE_DATA" advance "$EXPECTED_IDENTITY" producer_running --producer direct-PR >/dev/null \
    || fail "setup: producer_running"
}

new_case multiline_identity
hf "$CASE_DATA" record "$CASE_BINDING" >/dev/null || fail "setup: record for multi-line identity"
RECEIPT="$CASE_DATA/handoffs/$EXPECTED_IDENTITY.json"
cp "$RECEIPT" "$CASE_DATA/handoffs/$EXPECTED_IDENTITY${NL}zz.json"
err="$CASE_DIR/err.multiline"
hf "$CASE_DATA" show "$EXPECTED_IDENTITY${NL}zz" >/dev/null 2>"$err"
[ $? -eq 2 ] || fail "a multi-line identity must be refused, not matched by its first line"
grep -q 'refused: identity: malformed' "$err" || fail "multi-line identity must name the identity gate; got: $(cat "$err")"
hf "$CASE_DATA" advance "$EXPECTED_IDENTITY${NL}zz" assignment_intent --local-task dpes-880-fixture >/dev/null 2>"$err"
[ $? -eq 2 ] || fail "advance must refuse a multi-line identity even when a planted file matches it"
pass "a multi-line identity is refused by show and advance"

new_case multiline_head
hf "$CASE_DATA" record "$CASE_BINDING" >/dev/null || fail "setup: record for multi-line head"
RECEIPT="$CASE_DATA/handoffs/$EXPECTED_IDENTITY.json"
reach_producer_running
advance_refused "a PR head with a trailing line is refused, not matched by its first line" "evidence:" \
  advance "$EXPECTED_IDENTITY" pr_intent --branch fm/DPES-880-fixture --base main --head "$HEAD_SHA${NL}zz"

new_case newline_pr_head
hf "$CASE_DATA" record "$CASE_BINDING" >/dev/null || fail "setup: record for newline PR head"
RECEIPT="$CASE_DATA/handoffs/$EXPECTED_IDENTITY.json"
reach_producer_running
advance_refused "a PR head with a trailing newline is refused, not stripped and accepted" "evidence:" \
  advance "$EXPECTED_IDENTITY" pr_intent --branch fm/DPES-880-fixture --base main --head "$HEAD_SHA$NL"

new_case newline_task
hf "$CASE_DATA" record "$CASE_BINDING" >/dev/null || fail "setup: record for newline task"
RECEIPT="$CASE_DATA/handoffs/$EXPECTED_IDENTITY.json"
advance_refused "a local task with a trailing newline is refused, not stripped and accepted" "evidence:" \
  advance "$EXPECTED_IDENTITY" assignment_intent --local-task "dpes-880-fixture$NL"

new_case non_hex_head
jq --arg sha "not-a-commit" '.imported_head = $sha' "$CASE_BINDING" >"$CASE_DIR/nonhex.json"
expect_refusal non_hex_head "$CASE_DIR/nonhex.json" schema "a non-hex imported_head is refused before any state is written"

new_case newline_head
jq --arg sha "$HEAD_SHA$NL" '.imported_head = $sha' "$CASE_BINDING" >"$CASE_DIR/nlhead.json"
expect_refusal newline_head "$CASE_DIR/nlhead.json" schema "an imported_head with a trailing newline is refused"

new_case newline_delivery
jq --arg m "direct-PR$NL" '.delivery_mode = $m' "$CASE_BINDING" >"$CASE_DIR/nldelivery.json"
expect_refusal newline_delivery "$CASE_DIR/nldelivery.json" delivery-mode "a delivery mode with a trailing newline is refused"

new_case hash_failure
hf "$CASE_DATA" record "$CASE_BINDING" >/dev/null || fail "setup: record before hash failure"
RECEIPT="$CASE_DATA/handoffs/$EXPECTED_IDENTITY.json"
cp "$RECEIPT" "$CASE_DIR/receipt.before"
jq --arg sha "$OTHER_SHA" '.imported_head = $sha' "$CASE_BINDING" >"$CASE_DIR/changed.json"
HASHFAIL="$CASE_DIR/hashfail"
mkdir -p "$HASHFAIL"
for t in shasum sha256sum; do
  printf '#!/bin/sh\necho "%s: simulated failure" >&2\nexit 1\n' "$t" >"$HASHFAIL/$t"
  chmod +x "$HASHFAIL/$t"
done
err="$CASE_DIR/err.hash"
env PATH="$HASHFAIL:$PATH" FM_DATA_OVERRIDE="$CASE_DATA" bash "$HANDOFF" \
  record "$CASE_DIR/changed.json" >/dev/null 2>"$err"
rc=$?
[ "$rc" -eq 2 ] || fail "record must refuse when the hash tool fails, got rc $rc"
grep -q 'refused: digest:' "$err" || fail "hash failure must name the digest gate; got: $(cat "$err")"
[ ! -e "$CASE_DATA/handoffs/.json" ] || fail "a hash failure published a receipt under an empty identity"
cmp -s "$RECEIPT" "$CASE_DIR/receipt.before" || fail "a hash failure changed the stored receipt"
pass "a failing hash tool refuses record and never publishes under an empty identity"

new_case tampered_receipt
hf "$CASE_DATA" record "$CASE_BINDING" >/dev/null || fail "setup: record for tampered receipt"
RECEIPT="$CASE_DATA/handoffs/$EXPECTED_IDENTITY.json"
jq --arg s "$OTHER_SHA" '.binding.imported_head = $s' "$RECEIPT" >"$CASE_DIR/tampered.json" \
  && mv "$CASE_DIR/tampered.json" "$RECEIPT"
cp "$RECEIPT" "$CASE_DIR/receipt.before"
err="$CASE_DIR/err.tampered"
hf "$CASE_DATA" record "$CASE_BINDING" >/dev/null 2>"$err"
[ $? -eq 2 ] || fail "a replay must refuse a receipt whose stored binding no longer matches its digest"
grep -q 'refused: receipt:' "$err" || fail "tampered receipt must name the receipt gate; got: $(cat "$err")"
cmp -s "$RECEIPT" "$CASE_DIR/receipt.before" || fail "a refused replay changed the tampered receipt"
advance_refused "an advance refuses a receipt whose stored binding no longer matches its digest" "receipt:" \
  advance "$EXPECTED_IDENTITY" assignment_intent --local-task dpes-880-fixture
pass "replay and advance refuse a stored receipt whose binding was edited under its digest"

new_case newline_readback
hf "$CASE_DATA" record "$CASE_BINDING" >/dev/null || fail "setup: record for newline read-back"
RECEIPT="$CASE_DATA/handoffs/$EXPECTED_IDENTITY.json"
reach_producer_running
advance_ok "a PR intent for the binding advances before the read-back check" \
  advance "$EXPECTED_IDENTITY" pr_intent --branch fm/DPES-880-fixture --base main --head "$HEAD_SHA"
jq -n --arg h "$HEAD_SHA$NL" --arg u "$PR_URL" '{base: "main", draft: false, head: $h, number: 42, url: $u}' \
  >"$CASE_DIR/pr.newline.json"
advance_refused "a read-back head with a trailing newline is refused, not stripped and accepted" "evidence:" \
  advance "$EXPECTED_IDENTITY" pr_observed --pr "$CASE_DIR/pr.newline.json"

new_case flush_failure
PYFAIL="$CASE_DIR/pyfail"
mkdir -p "$PYFAIL"
printf '#!/bin/sh\necho "python3: simulated flush failure" >&2\nexit 1\n' >"$PYFAIL/python3"
chmod +x "$PYFAIL/python3"
# hf_flush_fault <args...>: hf with python3 failing, so every durable flush fails.
hf_flush_fault() {
  env PATH="$PYFAIL:$PATH" FM_DATA_OVERRIDE="$CASE_DATA" bash "$HANDOFF" "$@"
}
err="$CASE_DIR/err.flush"
hf_flush_fault record "$CASE_BINDING" >/dev/null 2>"$err"
rc=$?
[ "$rc" -eq 2 ] || fail "record must refuse when custody cannot be flushed, got rc $rc"
grep -q 'refused: durability' "$err" || fail "unflushed custody must name the durability gate; got: $(cat "$err")"
[ ! -e "$CASE_DATA/handoffs/$EXPECTED_IDENTITY.json" ] || fail "an unflushed record left a receipt behind"
pass "record refuses when custody cannot be flushed and leaves no receipt"

hf "$CASE_DATA" record "$CASE_BINDING" >/dev/null || fail "setup: record with a working flush"
RECEIPT="$CASE_DATA/handoffs/$EXPECTED_IDENTITY.json"
cp "$RECEIPT" "$CASE_DIR/receipt.before"
hf_flush_fault record "$CASE_BINDING" >/dev/null 2>"$err"
rc=$?
[ "$rc" -eq 2 ] || fail "a replay must not acknowledge a receipt it cannot flush, got rc $rc"
grep -q 'refused: durability' "$err" || fail "unflushed replay must name the durability gate; got: $(cat "$err")"
cmp -s "$RECEIPT" "$CASE_DIR/receipt.before" || fail "an unflushed replay changed the receipt"
pass "a replay refuses when it cannot flush the existing receipt"

hf_flush_fault advance "$EXPECTED_IDENTITY" assignment_intent --local-task dpes-880-fixture >/dev/null 2>"$err"
rc=$?
[ "$rc" -eq 2 ] || fail "an advance must refuse when its receipt cannot be flushed, got rc $rc"
grep -q 'refused: durability' "$err" || fail "unflushed advance must name the durability gate; got: $(cat "$err")"
cmp -s "$RECEIPT" "$CASE_DIR/receipt.before" || fail "an unflushed advance changed the receipt"
pass "an advance refuses when its receipt cannot be flushed and leaves the receipt unchanged"
