#!/usr/bin/env bash
# tests/fm-audit-activity-map.test.sh - the audit.activity projection, driven through
# bin/fm-audit-activity-map.py. The event and LogEntry shape belong to sections 3.1 and 4 of the
# SIEM format document, the input record to docs/audit-envelope.md, and redaction to
# docs/audit-sanitization.md. Every value below is synthetic and is built in this file.
# jq filters bind --arg variables by name, so a literal $name inside a single-quoted filter is intended.
# shellcheck disable=SC2016
set -u

# shellcheck source=tests/lib.sh
. "$(dirname "${BASH_SOURCE[0]}")/lib.sh"

TMP_ROOT=$(fm_test_tmproot fm-audit-activity-map)
MAPPER="$ROOT/bin/fm-audit-activity-map.py"
HOME_KEY="5e8f1c2a7b9d4e6f0a3b5c7d9e1f2a4b6c8d0e2f4a6b8c0d1e3f5a7b9c1d2e3f" # gitleaks:allow - synthetic test key, not a credential
USER_ID="hmac-sha256:0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef"
PREV_HASH="sha256:0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef"
COMMIT="abcdef0123456789abcdef0123456789abcdef01"
EVENT_ID="018f3b7e-9c1a-7d2e-8f3a-4b5c6d7e8f90"
GH_TOKEN="ghp_SyntheticTokenAbcdefghij0123" # gitleaks:allow - synthetic seeded value, not a credential
EMAIL="synthetic.patient@example.test"

# config <name> <json>: write one sanitizer config file and print its path.
config() {
  local file="$TMP_ROOT/config-$1.json"
  printf '%s\n' "$2" > "$file" || fail "could not write config $1"
  printf '%s\n' "$file"
}

VALID_CONFIG=$(config valid "$(jq -cn --arg k "$HOME_KEY" '{home_key: $k}')")

# base_record: a valid envelope v1 task.dispatched record that every case starts from.
base_record() {
  jq -cn --arg user "$USER_ID" '{
    schema: "mcao.audit-envelope.v1",
    event_id: "018f3b7e-9c1a-7d2e-8f3a-4b5c6d7e8f90",
    seq: 0,
    ts: "2026-10-16T02:00:00.000Z",
    prev_hash: null,
    event: "task.dispatched",
    category: "lifecycle",
    outcome: "success",
    platform: "mcao",
    home_id: "5f1c2b3a-4d5e-4f60-8a71-92b3c4d5e6f7",
    day_key: "2026-10-15",
    session_id: "sess-1",
    task_id: "fix-login",
    agent: {role: "firstmate", harness: "claude", model: null},
    actor: {type: "user", id: $user},
    repo: "mc-innersource/motif-hve-multi-agent-orchestrator",
    pr: null,
    commit: null,
    user: {id: $user, identity_source: "github_login", timezone: "America/Chicago"}
  }'
}

# record <name> [jq args...] <jq program>: write the base record with the program applied; print path.
record() {
  local name=$1 file
  shift
  file="$TMP_ROOT/record-$name.json"
  base_record | jq -c "$@" > "$file" || fail "could not write record $name"
  printf '%s\n' "$file"
}

# run_map <config> <record>: one mapper run; sets RC, OUT, and ERR.
run_map() {
  local err_file="$TMP_ROOT/last.err"
  OUT=$(python3 "$MAPPER" map --config "$1" "$2" 2>"$err_file")
  RC=$?
  ERR=$(cat "$err_file")
}

# expect_refusal <label> <reason> <paths-json> <record>: the validator or boundary refuses with no event.
expect_refusal() {
  run_map "$VALID_CONFIG" "$4"
  expect_code 1 "$RC" "$1 must refuse"
  assert_equals "refused" "$(jq -r .result <<<"$OUT")" "$1 must report a refusal"
  assert_equals "$2" "$(jq -r .reason <<<"$OUT")" "$1 must name its reason"
  assert_equals "$3" "$(jq -cS .paths <<<"$OUT")" "$1 must name its JSON Pointers"
  assert_equals "false" "$(jq -c 'has("event")' <<<"$OUT")" "$1 must not emit an event"
  pass "$1 refuses with $2"
}

test_valid_record_maps_to_activity_event_and_log_entry() {
  local file
  file=$(record valid '.')
  run_map "$VALID_CONFIG" "$file"
  expect_code 0 "$RC" "a valid record must map"
  assert_equals "" "$ERR" "a mapped record must write no diagnostics"
  assert_equals "mapped" "$(jq -r .result <<<"$OUT")" "the result must name a mapped event"
  assert_equals "[]" "$(jq -c .changed <<<"$OUT")" "an ordinary record must redact nothing"
  assert_equals "mcao.siem.event/v1" "$(jq -r .event.schema <<<"$OUT")" "the event carries the SIEM schema"
  assert_equals "audit.activity" "$(jq -r .event.event_type <<<"$OUT")" "the event type is audit.activity"
  assert_equals "audit.activity:$EVENT_ID" "$(jq -r .event.event_id <<<"$OUT")" "the event id is the prefixed envelope id"
  assert_equals "2026-10-16T02:00:00.000Z" "$(jq -r .event.occurred_at_utc <<<"$OUT")" "occurred_at_utc is the envelope ts"
  assert_equals "mcao.audit-envelope.v1" "$(jq -r .event.source_route <<<"$OUT")" "source_route names the envelope schema"
  assert_equals "{\"event_id\":\"$EVENT_ID\",\"prev_hash\":null,\"seq\":0}" "$(jq -cS .event.lineage <<<"$OUT")" "lineage is the closed envelope triple"
  assert_equals '["actor","agent","category","commit","day_key","event","home_id","outcome","platform","pr","repo","session_id","task_id","user"]' \
    "$(jq -c '.event.data | keys' <<<"$OUT")" "data holds exactly the section 3.1 members"
  assert_equals '["id","identity_source"]' "$(jq -c '.event.data.user | keys' <<<"$OUT")" "data.user drops the timezone"
  assert_equals '{"harness":"claude","model":null,"role":"firstmate"}' "$(jq -cS .event.data.agent <<<"$OUT")" "data.agent maps role, harness, and model"
  assert_equals '{"id":"'"$USER_ID"'","type":"user"}' "$(jq -cS .event.data.actor <<<"$OUT")" "data.actor maps type and id"
  assert_equals "task.dispatched" "$(jq -r .event.data.event <<<"$OUT")" "data.event carries the envelope event"
  assert_equals "fix-login" "$(jq -r .event.data.task_id <<<"$OUT")" "data.task_id carries the envelope task"
  assert_equals "projects/dpe-platform-d/logs/mcao-audit" "$(jq -r .log_entry.logName <<<"$OUT")" "the LogEntry names the audit log"
  assert_equals "cloud_run_revision" "$(jq -r .log_entry.resource.type <<<"$OUT")" "the LogEntry resource type is cloud_run_revision"
  assert_equals "true" "$(jq -c '.log_entry.jsonPayload == .event' <<<"$OUT")" "jsonPayload is the audit.activity event"
  assert_equals "2026-10-16T02:00:00.000Z" "$(jq -r .log_entry.timestamp <<<"$OUT")" "the LogEntry timestamp is occurred_at_utc"
  assert_equals "audit.activity:$EVENT_ID" "$(jq -r .log_entry.insertId <<<"$OUT")" "the LogEntry insertId is the event id"
  assert_equals "DEFAULT" "$(jq -r .log_entry.severity <<<"$OUT")" "the LogEntry severity is DEFAULT"
  pass "a valid record maps to the section 3.1 event and the section 4 LogEntry"
}

test_optional_fields_and_chain_carry_through() {
  local file
  file=$(record chained --arg prev "$PREV_HASH" --arg commit "$COMMIT" \
    '.seq = 1 | .prev_hash = $prev | .event = "task.pr_ready" | .category = "delivery" | .pr = "https://github.com/mc-innersource/motif-hve-multi-agent-orchestrator/pull/90" | .commit = $commit | .agent.model = "synthetic-model"')
  run_map "$VALID_CONFIG" "$file"
  expect_code 0 "$RC" "a chained record with set optional members must map"
  assert_equals "[]" "$(jq -c .changed <<<"$OUT")" "chain hashes, commit, and PR URL must pass the boundary unchanged"
  assert_equals "{\"event_id\":\"$EVENT_ID\",\"prev_hash\":\"$PREV_HASH\",\"seq\":1}" "$(jq -cS .event.lineage <<<"$OUT")" "lineage keeps the sequence and previous hash"
  assert_equals "$COMMIT" "$(jq -r .event.data.commit <<<"$OUT")" "data.commit carries the commit"
  assert_equals "https://github.com/mc-innersource/motif-hve-multi-agent-orchestrator/pull/90" "$(jq -r .event.data.pr <<<"$OUT")" "data.pr carries the PR URL"
  assert_equals "synthetic-model" "$(jq -r .event.data.agent.model <<<"$OUT")" "data.agent.model carries the model"
  pass "a chained record keeps its lineage, commit, PR, and model"
}

test_absent_task_id_projects_null() {
  local file
  file=$(record no-task-id 'del(.task_id)')
  run_map "$VALID_CONFIG" "$file"
  expect_code 0 "$RC" "a valid record without task_id must map"
  assert_equals "null" "$(jq -c '.event.data.task_id' <<<"$OUT")" "an absent task_id must project as null"
  assert_equals "mapped" "$(jq -r .result <<<"$OUT")" "an absent task_id must not refuse the record"
  pass "an absent task_id projects as null, as section 3.1 requires"
}

test_validator_refusals_pass_through_without_an_event() {
  expect_refusal "an unresolved identity source" identity_unresolved '["/user/identity_source"]' \
    "$(record no-identity '.user.identity_source = null')"
  expect_refusal "a non-genesis record without a previous hash" schema_invalid '["/prev_hash"]' \
    "$(record no-chain '.seq = 1')"
  expect_refusal "a user actor whose id differs from the user" schema_invalid '["/actor/id"]' \
    "$(record actor-mismatch '.actor.id = "someone-else"')"
  expect_refusal "a day_key outside the user's timezone" day_key_mismatch '["/day_key"]' \
    "$(record wrong-day '.day_key = "2026-10-16"')"

  local file
  file=$(record unknown-member '. + {extra: "x"}')
  run_map "$VALID_CONFIG" "$file"
  expect_code 1 "$RC" "an envelope with an unknown member must refuse"
  assert_equals "schema_invalid" "$(jq -r .reason <<<"$OUT")" "an unknown member is a schema refusal"
  assert_equals "false" "$(jq -c 'has("event")' <<<"$OUT")" "a schema refusal must not emit an event"
  pass "validator refusals pass through with the validator's reason and no event"
}

test_free_text_redaction_is_listed_by_pointer() {
  local file
  file=$(record redaction --arg gh "$GH_TOKEN" --arg email "$EMAIL" '.task_id = $gh | .session_id = $email')
  run_map "$VALID_CONFIG" "$file"
  expect_code 0 "$RC" "a record with secrets in free text must still map"
  assert_not_contains "$OUT" "$GH_TOKEN" "the raw token must not reach the event"
  assert_not_contains "$OUT" "$EMAIL" "the raw email must not reach the event"
  assert_contains "$OUT" "[REDACTED:github_token]" "the token is redacted through the boundary"
  assert_contains "$OUT" "[REDACTED:email]" "the email is redacted through the boundary"
  assert_equals '["/data/session_id","/data/task_id"]' "$(jq -c '.changed | sort' <<<"$OUT")" "changed lists the redacted pointers and no values"
  pass "free-text secrets are redacted through the boundary and listed by pointer"
}

test_long_free_text_is_cut_and_listed() {
  local long file
  long=$(printf 'a%.0s' {1..300})
  file=$(record long-leaf --arg t "$long" '.task_id = $t')
  run_map "$VALID_CONFIG" "$file"
  expect_code 0 "$RC" "a record with an over-long field must still map"
  assert_equals "256" "$(jq -r '.event.data.task_id | length' <<<"$OUT")" "the boundary cuts the field to 256 characters"
  assert_equals '["/data/task_id"]' "$(jq -c .changed <<<"$OUT")" "the cut field is listed by pointer"
  pass "an over-long free-text field is cut to 256 characters and listed"
}

test_dropped_member_refuses_the_record() {
  local file
  file="$TMP_ROOT/lone-surrogate.json"
  base_record | sed 's/"task_id":"fix-login"/"task_id":"\\ud800"/' > "$file" || fail "could not write lone-surrogate record"
  expect_refusal "a task_id the boundary must drop" sanitization_dropped '["/data/task_id"]' "$file"
}

test_invalid_config_refuses() {
  local bad file
  bad=$(config bad-key '{"home_key":"not-a-key"}')
  file=$(record config-case '.')
  run_map "$bad" "$file"
  expect_code 1 "$RC" "an invalid sanitizer config must refuse"
  assert_equals "config_invalid" "$(jq -r .reason <<<"$OUT")" "the refusal names the config"
  assert_not_contains "$OUT" "not-a-key" "the refusal must not echo the config value"
  pass "an invalid sanitizer config refuses and echoes nothing"
}

test_input_errors_exit_two() {
  local file garbage
  file=$(record input-errors '.')
  garbage="$TMP_ROOT/garbage.json"
  printf 'not json\n' > "$garbage" || fail "could not write garbage input"

  python3 "$MAPPER" >/dev/null 2>&1
  expect_code 2 "$?" "no arguments is a usage error"
  python3 "$MAPPER" map --config "$VALID_CONFIG" >/dev/null 2>&1
  expect_code 2 "$?" "a missing record path is a usage error"
  python3 "$MAPPER" map --config - "$file" >/dev/null 2>&1
  expect_code 2 "$?" "a config read from stdin is a usage error"
  python3 "$MAPPER" map --config "$VALID_CONFIG" "$garbage" >/dev/null 2>&1
  expect_code 2 "$?" "input that is not JSON is a usage error"
  python3 "$MAPPER" map --config "$VALID_CONFIG" "$TMP_ROOT/absent.json" >/dev/null 2>&1
  expect_code 2 "$?" "an unreadable record path is a usage error"
  pass "missing arguments and unreadable or non-JSON input exit 2"
}

test_stdin_record_maps_like_a_file() {
  local file out rc
  file=$(record stdin-case '.')
  out=$(python3 "$MAPPER" map --config "$VALID_CONFIG" - < "$file")
  rc=$?
  expect_code 0 "$rc" "a record read from stdin must map"
  assert_equals "audit.activity:$EVENT_ID" "$(jq -r .event.event_id <<<"$out")" "stdin maps the same event as a file"
  pass "a record read from stdin maps like a file"
}

test_mapping_is_deterministic() {
  local file first second
  file=$(record determinism '.')
  run_map "$VALID_CONFIG" "$file"
  first=$OUT
  run_map "$VALID_CONFIG" "$file"
  second=$OUT
  assert_equals "$first" "$second" "the same record must map to the same output"
  pass "the same record maps to byte-identical output"
}

test_mapping_writes_nothing() {
  local dir="$TMP_ROOT/persist" before after valid refused
  mkdir -p "$dir" || fail "could not create persistence fixture"
  valid=$(record persist-valid '.')
  refused=$(config persist-bad '{"home_key":"not-a-key"}')
  before=$(cd "$dir" && find . | LC_ALL=C sort)

  (cd "$dir" && PYTHONPYCACHEPREFIX="$TMP_ROOT/pycache" python3 "$MAPPER" map --config "$VALID_CONFIG" "$valid" >/dev/null 2>&1)
  expect_code 0 "$?" "the persistence run must map"
  (cd "$dir" && PYTHONPYCACHEPREFIX="$TMP_ROOT/pycache" python3 "$MAPPER" map --config "$refused" "$valid" >/dev/null 2>&1)
  expect_code 1 "$?" "the persistence run must refuse a bad config"

  after=$(cd "$dir" && find . | LC_ALL=C sort)
  assert_equals "$before" "$after" "neither a mapped nor a refused record may write a file"
  # Interpreter startup caches the standard library before the script runs, so the check is scoped
  # to the sibling audit modules that the mapper itself loads.
  assert_equals "0" "$(find "$TMP_ROOT/pycache" -name 'fm-audit-*' 2>/dev/null | wc -l | tr -d ' ')" \
    "loading the sibling audit modules must not write their bytecode"
  pass "mapping and refusing write no file and no sibling bytecode"
}

test_valid_record_maps_to_activity_event_and_log_entry
test_optional_fields_and_chain_carry_through
test_absent_task_id_projects_null
test_validator_refusals_pass_through_without_an_event
test_free_text_redaction_is_listed_by_pointer
test_long_free_text_is_cut_and_listed
test_dropped_member_refuses_the_record
test_invalid_config_refuses
test_input_errors_exit_two
test_stdin_record_maps_like_a_file
test_mapping_is_deterministic
test_mapping_writes_nothing
