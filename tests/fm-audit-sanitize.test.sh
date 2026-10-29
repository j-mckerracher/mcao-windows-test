#!/usr/bin/env bash
# tests/fm-audit-sanitize.test.sh - the audit sanitization boundary, driven through
# bin/fm-audit-sanitize.py. docs/audit-sanitization.md owns the contract. Every seeded
# secret and PHI value below is synthetic and is built here at runtime.
set -u

# shellcheck source=tests/lib.sh
. "$(dirname "${BASH_SOURCE[0]}")/lib.sh"

TMP_ROOT=$(fm_test_tmproot fm-audit-sanitize)
SANITIZER="$ROOT/bin/fm-audit-sanitize.py"
HOME_KEY="5e8f1c2a7b9d4e6f0a3b5c7d9e1f2a4b6c8d0e2f4a6b8c0d1e3f5a7b9c1d2e3f" # gitleaks:allow - synthetic test key, not a credential

# Seeded synthetic values. Each one matches exactly one redaction class.
GH_TOKEN="ghp_SyntheticTokenAbcdefghij0123"
GITHUB_PAT="github_pat_SyntheticPatAbcdefghij0123"
AWS_KEY="AKIASYNTHETIC0123456" # gitleaks:allow - synthetic seeded value, not a credential
GOOGLE_KEY="AIzaSyntheticGoogleKeyForTests000000000" # gitleaks:allow - synthetic seeded value, not a credential
SLACK_TOKEN="xoxb-1234567890-SyntheticSlackToken" # gitleaks:allow - synthetic seeded value, not a credential
JWT="eyJhbGciOiJIUzI1NiJ9.eyJzdWIiOiJzeW50aGV0aWMifQ.c3ludGhldGljLXNpZw" # gitleaks:allow - synthetic seeded value, not a credential
BEARER="Bearer SyntheticBearerValue0000"
PRIVATE_KEY="-----BEGIN RSA PRIVATE KEY-----"$'\n'"SYNTHETICKEYBODYLINE1"$'\n'"-----END RSA PRIVATE KEY-----"
CREDENTIAL="password=SyntheticPassw0rd"
EMAIL="synthetic.patient@example.test"
SSN="987-65-4321"
PHONE="555-555-0142"
DOB="DOB: 1979-03-14"
MRN="MRN: 00123456"

# config <name> <json>: write one config file and print its path.
config() {
  local file="$TMP_ROOT/$1.json"
  printf '%s\n' "$2" > "$file" || fail "could not write config $1"
  printf '%s\n' "$file"
}

# event <name> <json>: write one event file and print its path.
event() {
  local file="$TMP_ROOT/$1.json"
  printf '%s\n' "$2" > "$file" || fail "could not write event $1"
  printf '%s\n' "$file"
}

# run_sanitize <config> <event>: one sanitizer run; sets RC, OUT, and ERR.
run_sanitize() {
  local err_file="$TMP_ROOT/last.err"
  OUT=$(python3 "$SANITIZER" sanitize --config "$1" "$2" 2>"$err_file")
  RC=$?
  ERR=$(cat "$err_file")
}

VALID_CONFIG=$(config valid "$(jq -cn --arg k "$HOME_KEY" '{home_key: $k}')")

# seeded_event: every seeded value in a free-text position: a top-level leaf, a nested leaf, an
# array item, and an object key. Each leaf stays under the 256-character cut, so every redaction
# class is visible in the output rather than truncated away.
seeded_event() {
  jq -cn \
    --arg gh "$GH_TOKEN" --arg pat "$GITHUB_PAT" --arg aws "$AWS_KEY" \
    --arg google "$GOOGLE_KEY" --arg slack "$SLACK_TOKEN" --arg jwt "$JWT" \
    --arg bearer "$BEARER" --arg pk "$PRIVATE_KEY" --arg cred "$CREDENTIAL" \
    --arg email "$EMAIL" --arg ssn "$SSN" --arg phone "$PHONE" --arg dob "$DOB" --arg mrn "$MRN" \
    '{
      event_id: "018f3b7e-9c1a-7d2e-8f3a-4b5c6d7e8f90",
      seq: 0,
      ts: "2026-10-16T02:00:00.000Z",
      prev_hash: null,
      event: "task.status",
      status: ([$gh, $pat, $aws, $google, $slack] | join(" ")),
      detail: {
        note: ("key block " + $pk),
        auth: ([$jwt, $bearer] | join(" ")),
        nested: {contact: ("reach " + $email + " or " + $phone)}
      },
      notes: [("slack " + $slack), ("ssn " + $ssn), ($dob + " " + $mrn), ("cred " + $cred)],
      ($gh): "value under a secret-bearing key",
      ($mrn): ("record " + $mrn)
    }'
}

test_seeded_secrets_and_phi_never_reach_output_or_diagnostics() {
  local file seed label
  # The AWS and Google patterns are exact-length, so a seed of the wrong length would leave a
  # fragment behind and the whole-seed assertions below would pass for the wrong reason.
  assert_equals 20 "${#AWS_KEY}" "synthetic AWS key must be exactly the pattern length"
  assert_equals 39 "${#GOOGLE_KEY}" "synthetic Google key must be exactly the pattern length"
  file=$(event seeded "$(seeded_event)")
  run_sanitize "$VALID_CONFIG" "$file"
  expect_code 0 "$RC" "seeded event must sanitize"
  assert_equals sanitized "$(printf '%s' "$OUT" | jq -r .result)" "seeded event result"

  for seed in "$GH_TOKEN" "$GITHUB_PAT" "$AWS_KEY" "$GOOGLE_KEY" "$SLACK_TOKEN" "$JWT" \
    "$BEARER" "SYNTHETICKEYBODYLINE1" "BEGIN RSA PRIVATE KEY" "SyntheticPassw0rd" \
    "$EMAIL" "$SSN" "$PHONE" "1979-03-14" "00123456"; do
    assert_not_contains "$OUT" "$seed" "seeded value must not appear in output: ${seed:0:12}"
    assert_not_contains "$ERR" "$seed" "seeded value must not appear in diagnostics: ${seed:0:12}"
  done

  for label in github_token aws_access_key google_api_key slack_token jwt bearer_token \
    private_key credential_assignment email ssn phone date_of_birth medical_record_number; do
    assert_contains "$OUT" "[REDACTED:$label]" "seeded value must be replaced by its class: $label"
  done
  pass "seeded secrets and synthetic PHI in every string position never reach output or diagnostics"
}

test_default_config_drops_bodies_and_keeps_metadata() {
  local file
  file=$(event bodies "$(jq -cn '{
    event_id: "018f3b7e-9c1a-7d2e-8f3a-4b5c6d7e8f90", seq: 0, ts: "2026-10-16T02:00:00.000Z",
    prev_hash: null, event: "agent.output", category: "delivery", outcome: "success",
    prompt: "PROMPT-BODY-SYNTHETIC summarize the chart",
    model_output: "MODEL-OUTPUT-BODY-SYNTHETIC answer",
    retry_body: "RETRY-BODY-SYNTHETIC", exception_body: "EXCEPTION-BODY-SYNTHETIC",
    env: {VAR: "ENV-VALUE-SYNTHETIC"}, environment: {VAR: "ENVIRONMENT-VALUE-SYNTHETIC"},
    file_contents: "FILE-CONTENTS-SYNTHETIC",
    detail: {prompt: "NESTED-PROMPT-SYNTHETIC"}
  }')")
  run_sanitize "$VALID_CONFIG" "$file"
  expect_code 0 "$RC" "default config must sanitize a body-bearing event"

  assert_equals '["category","detail","event","event_id","outcome","prev_hash","seq","ts"]' \
    "$(printf '%s' "$OUT" | jq -c '.event | keys')" "only approved metadata and the emptied detail container remain"
  assert_equals '["/detail/prompt","/env","/environment","/exception_body","/file_contents","/model_output","/prompt","/retry_body"]' \
    "$(printf '%s' "$OUT" | jq -c '.field_dropped | sort')" "every dropped body member is recorded by pointer"
  assert_equals 0 "$(printf '%s' "$OUT" | jq '.event.seq')" "seq is retained"
  assert_equals "agent.output" "$(printf '%s' "$OUT" | jq -r '.event.event')" "event name is retained"
  assert_not_contains "$OUT" "SYNTHETIC" "no body, env, or file content value may appear in output"
  pass "default config drops prompt, output, retry, exception, env, and file bodies and keeps metadata"
}

test_typed_objects_keep_digests_not_values() {
  local file expected_argv expected_args
  file=$(event typed "$(jq -cn --arg gh "$GH_TOKEN" --arg home "/tmp/mcao-wt" '{
    event_id: "018f3b7e-9c1a-7d2e-8f3a-4b5c6d7e8f90", seq: 0, ts: "2026-10-16T02:00:00.000Z",
    prev_hash: null, event: "task.dispatched",
    cmd: {"$kind": "shell_command", argv: ["gh", "pr", "create", "--title", ("note " + $gh)]},
    call: {"$kind": "tool_call", tool: "Bash", args: {command: ("curl " + $gh), timeout: 5}},
    local_path: {"$kind": "path", path: ($home + "/src/app.py"), root: $home},
    outside: {"$kind": "path", path: "/Users/someone/private/notes.txt", root: $home},
    sink: {"$kind": "destination", url: "https://user:pw@siem.example.test:8443/ingest?token=abc"}
  }')")
  run_sanitize "$VALID_CONFIG" "$file"
  expect_code 0 "$RC" "typed objects must sanitize"

  assert_equals '["argv_digest"]' "$(printf '%s' "$OUT" | jq -c '.event.cmd | keys')" \
    "shell command keeps only argv_digest while the executable allowlist is empty"
  expected_argv=$(python3 -c 'import hashlib, hmac, json, sys
key = bytes.fromhex(sys.argv[1])
msg = json.dumps(json.loads(sys.argv[2]), sort_keys=True, separators=(",", ":"), ensure_ascii=True).encode("ascii")
print("hmac-sha256:" + hmac.new(key, msg, hashlib.sha256).hexdigest())' "$HOME_KEY" \
    "[\"gh\",\"pr\",\"create\",\"--title\",\"note $GH_TOKEN\"]")
  assert_equals "$expected_argv" "$(printf '%s' "$OUT" | jq -r .event.cmd.argv_digest)" \
    "argv_digest is HMAC-SHA256 over the normalized argv, keyed by the home key"

  assert_equals '["args_digest","tool"]' "$(printf '%s' "$OUT" | jq -c '.event.call | keys')" \
    "tool call keeps the tool name and args_digest only"
  assert_equals "Bash" "$(printf '%s' "$OUT" | jq -r .event.call.tool)" "tool name is retained"
  expected_args=$(python3 -c 'import hashlib, hmac, json, sys
key = bytes.fromhex(sys.argv[1])
msg = json.dumps(json.loads(sys.argv[2]), sort_keys=True, separators=(",", ":"), ensure_ascii=True).encode("ascii")
print("hmac-sha256:" + hmac.new(key, msg, hashlib.sha256).hexdigest())' "$HOME_KEY" \
    "{\"command\":\"curl $GH_TOKEN\",\"timeout\":5}")
  assert_equals "$expected_args" "$(printf '%s' "$OUT" | jq -r .event.call.args_digest)" \
    "args_digest is HMAC-SHA256 over the canonical arguments"

  assert_equals "src/app.py" "$(printf '%s' "$OUT" | jq -r .event.local_path.repo_path)" \
    "a path inside the task worktree is repo-relative"
  # shellcheck disable=SC2088  # The literal ~ is the expected output text, not a path to expand.
  assert_equals "~/notes.txt" "$(printf '%s' "$OUT" | jq -r .event.outside.basename)" \
    "a path outside the worktree keeps only a ~-relative basename"
  assert_equals '["basename","path_digest"]' "$(printf '%s' "$OUT" | jq -c '.event.outside | keys')" \
    "an outside path carries a path digest and no directory"
  assert_equals '{"host":"siem.example.test","scheme":"https"}' \
    "$(printf '%s' "$OUT" | jq -c .event.sink)" "a destination keeps scheme and host only"
  assert_not_contains "$OUT" "/Users/someone" "an outside directory must not appear"
  assert_not_contains "$OUT" "token=abc" "destination query must not appear"
  assert_not_contains "$OUT" "user:pw@" "destination credentials must not appear"
  assert_not_contains "$OUT" "8443" "destination port must not appear"
  assert_not_contains "$OUT" "curl" "raw tool arguments must not appear"
  pass "typed objects keep keyed digests and approved parts, never raw argv, arguments, or full paths"
}

test_sanitizer_error_drops_the_field_and_records_it() {
  local file
  file=$(event surrogate '{"event_id":"018f3b7e-9c1a-7d2e-8f3a-4b5c6d7e8f90","seq":0,"ts":"2026-10-16T02:00:00.000Z","prev_hash":null,"event":"task.status","status":"\ud800 password=SyntheticPassw0rd","kept":"plain"}') # gitleaks:allow - synthetic seeded value, not a credential
  run_sanitize "$VALID_CONFIG" "$file"
  expect_code 0 "$RC" "an unparseable field must not refuse the whole event"
  assert_equals '["/status"]' "$(printf '%s' "$OUT" | jq -c .field_dropped)" \
    "unparseable text drops that field and records its pointer"
  assert_equals "plain" "$(printf '%s' "$OUT" | jq -r .event.kept)" "other fields are still sanitized and kept"
  assert_not_contains "$OUT" "SyntheticPassw0rd" "dropped field must not leak through a fallback"

  file=$(event badtyped "$(jq -cn --arg c "$CREDENTIAL" '{
    event_id: "018f3b7e-9c1a-7d2e-8f3a-4b5c6d7e8f90", seq: 0, ts: "2026-10-16T02:00:00.000Z",
    prev_hash: null, event: "task.status",
    bad_cmd: {"$kind": "shell_command", argv: ("not-a-list " + $c)},
    unknown_kind: {"$kind": "raw_text", value: $c}
  }')")
  run_sanitize "$VALID_CONFIG" "$file"
  expect_code 0 "$RC" "malformed typed objects must not refuse the whole event"
  assert_equals '["/bad_cmd","/unknown_kind"]' "$(printf '%s' "$OUT" | jq -c '.field_dropped | sort')" \
    "malformed and unknown typed objects are dropped and recorded"
  assert_not_contains "$OUT" "SyntheticPassw0rd" "malformed typed object must not leak its value"

  file=$(event collide "$(jq -cn --arg a "$GH_TOKEN" --arg b "ghp_SyntheticOtherTokenAbcdef0123" '{
    event_id: "018f3b7e-9c1a-7d2e-8f3a-4b5c6d7e8f90", seq: 0, ts: "2026-10-16T02:00:00.000Z",
    prev_hash: null, event: "task.status", ($a): 1, ($b): 2
  }')")
  run_sanitize "$VALID_CONFIG" "$file"
  expect_code 0 "$RC" "keys that collide after redaction must not refuse the event"
  assert_equals 1 "$(printf '%s' "$OUT" | jq '[.event | keys[] | select(startswith("[REDACTED"))] | length')" \
    "keys that collide after redaction keep one entry"
  assert_equals 1 "$(printf '%s' "$OUT" | jq '.field_dropped | length')" "the colliding key is recorded as dropped"
  pass "an unparseable field, a malformed typed object, and a colliding key drop without a raw fallback"
}

test_redaction_happens_before_truncation() {
  local file status
  file=$(event straddle "$(jq -cn --arg gh "$GH_TOKEN" --arg pad "$(printf 'x%.0s' $(seq 1 250))" '{
    event_id: "018f3b7e-9c1a-7d2e-8f3a-4b5c6d7e8f90", seq: 0, ts: "2026-10-16T02:00:00.000Z",
    prev_hash: null, event: "task.status", status: ($pad + $gh)
  }')")
  run_sanitize "$VALID_CONFIG" "$file"
  expect_code 0 "$RC" "straddling secret must sanitize"
  status=$(printf '%s' "$OUT" | jq -r .event.status)
  assert_equals 256 "${#status}" "text is truncated to 256 characters"
  assert_not_contains "$OUT" "SyntheticTokenAbcdef" "a secret straddling the cut must not leak its tail"

  file=$(event long "$(jq -cn --arg pad "$(printf 'y%.0s' $(seq 1 300))" '{
    event_id: "018f3b7e-9c1a-7d2e-8f3a-4b5c6d7e8f90", seq: 0, ts: "2026-10-16T02:00:00.000Z",
    prev_hash: null, event: "task.status", status: $pad
  }')")
  run_sanitize "$VALID_CONFIG" "$file"
  assert_equals 256 "$(printf '%s' "$OUT" | jq -r '.event.status | length')" "a long plain string keeps 256 characters"
  pass "redaction runs before the 256-character cut, so a secret cannot survive as its tail"
}

test_invalid_config_refuses_and_echoes_nothing() {
  local seeded bad
  seeded=$(event refusal-event "$(seeded_event)")

  expect_refused() {
    run_sanitize "$2" "$seeded"
    expect_code 1 "$RC" "$1: invalid config must refuse"
    assert_equals refused "$(printf '%s' "$OUT" | jq -r .result)" "$1: result"
    assert_equals config_invalid "$(printf '%s' "$OUT" | jq -r .reason)" "$1: reason"
    assert_equals '' "$(printf '%s' "$OUT" | jq -r '.event // empty')" "$1: refusal carries no event"
    assert_not_contains "$OUT" "$GH_TOKEN" "$1: refusal must not echo a seeded secret"
    assert_not_contains "$ERR" "$GH_TOKEN" "$1: refusal diagnostics must not echo a seeded secret"
    assert_not_contains "$OUT" "$HOME_KEY" "$1: refusal must not echo the home key"
  }

  expect_refused missing-file "$TMP_ROOT/does-not-exist.json"
  expect_refused malformed-json "$(config malformed "{\"home_key\": \"$HOME_KEY\"")"
  expect_refused non-object "$(config non-object '["x"]')"
  expect_refused missing-home-key "$(config missing '{}')"

  bad=$(config short-key "$(jq -cn '{home_key: "abc123"}')")
  expect_refused short-key "$bad"
  run_sanitize "$bad" "$seeded"
  assert_equals '["/home_key"]' "$(printf '%s' "$OUT" | jq -c .paths)" "a bad home key is reported by pointer only"

  expect_refused uppercase-key "$(config upper "$(jq -cn --arg k "$(printf '%s' "$HOME_KEY" | tr a-f A-F)" '{home_key: $k}')")"
  expect_refused allowlist-injection "$(config allowlist "$(jq -cn --arg k "$HOME_KEY" '{home_key: $k, executable_allowlist: ["gh"]}')")"
  expect_refused duplicate-key "$(config duplicate "{\"home_key\":\"$HOME_KEY\",\"home_key\":\"$HOME_KEY\"}")"
  pass "every invalid config refuses with config_invalid, echoes no secret or key, and carries no event"
}

test_refusals_and_outputs_persist_nothing() {
  local dir="$TMP_ROOT/persist" before after bad seeded
  mkdir -p "$dir/state" || fail "could not create persistence fixture"
  seeded=$(event persist-event "$(seeded_event)")
  bad=$(config persist-bad '{"home_key":"not-a-key"}')
  before=$(cd "$dir" && find . | LC_ALL=C sort)

  (cd "$dir" && FM_STATE_OVERRIDE="$dir/state" python3 "$SANITIZER" sanitize --config "$bad" "$seeded" >/dev/null 2>&1)
  expect_code 1 "$?" "refused run must exit 1"
  (cd "$dir" && FM_STATE_OVERRIDE="$dir/state" python3 "$SANITIZER" sanitize --config "$VALID_CONFIG" "$seeded" >/dev/null 2>&1)
  expect_code 0 "$?" "sanitized run must exit 0"

  after=$(cd "$dir" && find . | LC_ALL=C sort)
  assert_equals "$before" "$after" "neither a refusal nor a sanitized event may write any file"
  pass "neither a refused nor a sanitized event writes any file"
}

test_refused_event_and_input_errors_stay_quiet_about_values() {
  local event_file dup
  event_file=$(event toplevel-kind "$(jq -cn --arg gh "$GH_TOKEN" '{"$kind": "shell_command", argv: [$gh]}')")
  run_sanitize "$VALID_CONFIG" "$event_file"
  expect_code 1 "$RC" "a top-level typed object is an event refusal"
  assert_equals event_invalid "$(printf '%s' "$OUT" | jq -r .reason)" "event refusal reason"
  assert_not_contains "$OUT" "$GH_TOKEN" "event refusal must not echo a seeded secret"

  event_file=$(event broken-json "{\"status\":\"$GH_TOKEN\"")
  run_sanitize "$VALID_CONFIG" "$event_file"
  expect_code 2 "$RC" "malformed event JSON is an input error"
  assert_not_contains "$ERR" "$GH_TOKEN" "input-error diagnostics must not echo the event"

  dup=$(event dup-event "{\"status\":\"$GH_TOKEN\",\"status\":\"x\"}")
  run_sanitize "$VALID_CONFIG" "$dup"
  expect_code 2 "$RC" "a duplicate key in the event is an input error"
  assert_not_contains "$ERR" "$GH_TOKEN" "duplicate-key diagnostics must not echo the event"

  run_sanitize "$VALID_CONFIG" "$TMP_ROOT/absent-event.json"
  expect_code 2 "$RC" "an unreadable event is an input error"

  python3 "$SANITIZER" >/dev/null 2>&1
  expect_code 2 "$?" "no arguments is a usage error"
  python3 "$SANITIZER" sanitize --config - "$VALID_CONFIG" >/dev/null 2>&1
  expect_code 2 "$?" "a config read from stdin is a usage error"
  pass "refused events and input errors exit with the right status and never echo a seeded value"
}

test_redaction_patterns_are_pinned() {
  local file
  file=$(event pins "$(jq -cn '{
    event_id: "018f3b7e-9c1a-7d2e-8f3a-4b5c6d7e8f90", seq: 0, ts: "2026-10-16T02:00:00.000Z",
    prev_hash: null, event: "task.status", status: "gh_short AKIAshort ghp_tooShort"
  }')")
  run_sanitize "$VALID_CONFIG" "$file"
  expect_code 0 "$RC" "near-miss text must sanitize"
  assert_equals "gh_short AKIAshort ghp_tooShort" "$(printf '%s' "$OUT" | jq -r .event.status)" \
    "text below each pattern's minimum length is not redacted"
  pass "patterns require their documented minimum shape, so near-miss text is kept"
}

test_long_unbroken_leaf_sanitizes_in_linear_time() {
  local file started elapsed status
  file=$(event long-run "$(jq -cn --arg run "$(head -c 100000 /dev/zero | tr '\0' 'a')" --arg email "$EMAIL" '{
    event_id: "018f3b7e-9c1a-7d2e-8f3a-4b5c6d7e8f90", seq: 0, ts: "2026-10-16T02:00:00.000Z",
    prev_hash: null, event: "task.status", status: ($run + " " + $email)
  }')")
  started=$SECONDS
  run_sanitize "$VALID_CONFIG" "$file"
  elapsed=$((SECONDS - started))
  expect_code 0 "$RC" "a 100000-character leaf must sanitize"
  [ "$elapsed" -lt 10 ] || fail "a 100000-character leaf took ${elapsed}s to sanitize"
  status=$(printf '%s' "$OUT" | jq -r .event.status)
  assert_equals 256 "${#status}" "the long leaf is truncated to 256 characters"

  file=$(event long-run-email "$(jq -cn --arg email "$EMAIL" '{
    event_id: "018f3b7e-9c1a-7d2e-8f3a-4b5c6d7e8f90", seq: 0, ts: "2026-10-16T02:00:00.000Z",
    prev_hash: null, event: "task.status", status: ("contact " + $email + " now")
  }')")
  run_sanitize "$VALID_CONFIG" "$file"
  assert_not_contains "$OUT" "$EMAIL" "an email after the left boundary is still redacted"
  pass "a 100000-character unbroken leaf sanitizes quickly and email redaction is kept"
}

test_seeded_secrets_and_phi_never_reach_output_or_diagnostics
test_default_config_drops_bodies_and_keeps_metadata
test_typed_objects_keep_digests_not_values
test_sanitizer_error_drops_the_field_and_records_it
test_redaction_happens_before_truncation
test_invalid_config_refuses_and_echoes_nothing
test_refusals_and_outputs_persist_nothing
test_refused_event_and_input_errors_stay_quiet_about_values
test_redaction_patterns_are_pinned
test_long_unbroken_leaf_sanitizes_in_linear_time
