#!/usr/bin/env bash
# tests/fm-audit-envelope.test.sh - the audit envelope v1 contract, driven through
# bin/fm-audit-envelope.py. docs/audit-envelope.md owns the contract and
# docs/audit-envelope.v1.schema.json is its machine-readable form. The fleet ledger
# records come from the real writer, bin/fm-fleet-ledger.sh, which docs/fleet-ledger.md owns.
set -u

# shellcheck source=tests/lib.sh
. "$(dirname "${BASH_SOURCE[0]}")/lib.sh"

TMP_ROOT=$(fm_test_tmproot fm-audit-envelope)
export FM_STATE_OVERRIDE="$TMP_ROOT/state"
mkdir -p "$FM_STATE_OVERRIDE"
VALIDATOR="$ROOT/bin/fm-audit-envelope.py"
# user.id and home_id carry their pinned formats, so these fixtures are well-formed pinned values.
USER_ID="hmac-sha256:0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef"
OTHER_USER_ID="hmac-sha256:$(printf '%064d' 0 | tr 0 f)"
HOME_ID="5f1c2b3a-4d5e-4f60-8a71-92b3c4d5e6f7"
HASH="sha256:fedcba9876543210fedcba9876543210fedcba9876543210fedcba9876543210"

# Chicago is on CDT (UTC-5) in October 2026, so 02:00Z on 2026-10-16 is still the 15th there.
BASE=$(jq -cn --arg user "$USER_ID" --arg home "$HOME_ID" '{
  schema: "mcao.audit-envelope.v1",
  event_id: "018f3b7e-9c1a-7d2e-8f3a-4b5c6d7e8f90",
  seq: 0,
  ts: "2026-10-16T02:00:00.000Z",
  prev_hash: null,
  event: "task.dispatched",
  category: "lifecycle",
  outcome: "success",
  platform: "mcao",
  home_id: $home,
  day_key: "2026-10-15",
  session_id: "sess-1",
  task_id: "fix-login",
  agent: {role: "firstmate", harness: "claude", model: null},
  actor: {type: "user", id: $user},
  repo: "mc-innersource/motif-hve-multi-agent-orchestrator",
  pr: null,
  commit: null,
  user: {id: $user, identity_source: "github_login", timezone: "America/Chicago"}
}')

# envelope <name> [jq-filter]: write BASE transformed by the filter, and print its path.
envelope() {
  local file="$TMP_ROOT/$1.json"
  printf '%s\n' "$BASE" | jq -c "${2:-.}" > "$file" || fail "could not build fixture $1"
  printf '%s\n' "$file"
}

# check <file> [definition]: one validator run; sets RC and OUT.
check() {
  OUT=$(python3 "$VALIDATOR" validate --definition "${2:-envelope}" "$1" 2>&1)
  RC=$?
}

# expect_accepted <name> <file> [definition]
expect_accepted() {
  check "$2" "${3:-envelope}"
  assert_equals 0 "$RC" "$1: expected accept (rc $RC): $OUT"
  assert_equals accepted "$(printf '%s' "$OUT" | jq -r .result)" "$1: result"
}

# expect_rejected <name> <file> <reason> <paths, comma-joined, or * to skip> [definition]
expect_rejected() {
  check "$2" "${5:-envelope}"
  assert_equals 1 "$RC" "$1: expected reject (rc $RC): $OUT"
  assert_equals "$3" "$(printf '%s' "$OUT" | jq -r .reason)" "$1: reason"
  if [ "$4" != "*" ]; then
    assert_equals "$4" "$(printf '%s' "$OUT" | jq -r '.paths | join(",")')" "$1: paths"
  fi
}

test_identity_gate_rejects_unresolved_identity() {
  expect_rejected "null user id" "$(envelope no-user-id '.user.id = null')" identity_unresolved /user/id
  assert_not_contains "$OUT" "$USER_ID" "rejection must not emit identity values"
  expect_rejected "PENDING user id is unresolved" "$(envelope pending-id '.user.id = "PENDING"')" \
    identity_unresolved /user/id
  expect_rejected "null identity source" "$(envelope no-source '.user.identity_source = null')" \
    identity_unresolved /user/identity_source
  expect_rejected "missing user" "$(envelope no-user 'del(.user)')" identity_unresolved /user
  expect_rejected "non-object user" "$(envelope text-user '.user = "jdoe"')" identity_unresolved /user
  expect_rejected "identity gate runs before schema checks" \
    "$(envelope gate-first '.user.id = null | .day_key = "1999-01-01" | .extra = 1')" \
    identity_unresolved /user/id
  pass "identity gate rejects unresolved or partial identity before schema checks"
}

test_raw_identity_never_passes() {
  expect_rejected "raw login member" "$(envelope raw-login '.user.login = "jdoe"')" schema_invalid /user
  assert_not_contains "$OUT" jdoe "raw login must not be echoed"
  expect_rejected "raw identity as actor id" "$(envelope raw-actor '.actor.id = "jdoe"')" schema_invalid /actor/id
  assert_not_contains "$OUT" jdoe "raw identity must not be echoed"
  expect_accepted "base envelope" "$(envelope base)"
  assert_not_contains "$OUT" "$USER_ID" "accepted output must not echo identity"
  pass "raw identity values never pass and are never echoed"
}

test_schema_is_versioned_and_closed() {
  expect_accepted "base envelope" "$(envelope base)"
  expect_rejected "v2 schema name" "$(envelope v2 '.schema = "mcao.audit-envelope.v2"')" schema_invalid /schema
  expect_rejected "unknown top-level member" "$(envelope top-extra '.raw = 1')" schema_invalid ""
  expect_rejected "unknown agent member" "$(envelope agent-extra '.agent.extra = 1')" schema_invalid /agent
  expect_rejected "unknown actor member" "$(envelope actor-extra '.actor.extra = 1')" schema_invalid /actor
  expect_rejected "unknown user member" "$(envelope user-extra '.user.extra = 1')" schema_invalid /user
  pass "schema is versioned to mcao.audit-envelope.v1 and closed at every level"
}

test_real_ledger_records_validate_as_ledger_v1_only() {
  local home="$TMP_ROOT/ledger-home" ledger dispatched pr_ready
  mkdir -p "$home/state" "$home/config"
  : > "$home/config/fleet-ledger"
  ledger_write() {
    env FM_HOME="$home" FM_STATE_OVERRIDE="$home/state" FM_CONFIG_OVERRIDE="$home/config" \
      "$ROOT/bin/fm-fleet-ledger.sh" "$@" || fail "ledger write failed: $*"
  }
  ledger_write dispatched fix-login ship webapp claude ""
  ledger_write pr_ready fix-login https://github.com/acme/webapp/pull/7
  ledger="$home/state/fleet-ledger.jsonl"
  dispatched="$TMP_ROOT/ledger-dispatched.json"
  pr_ready="$TMP_ROOT/ledger-pr-ready.json"
  sed -n 1p "$ledger" > "$dispatched"
  sed -n 2p "$ledger" > "$pr_ready"
  expect_accepted "ledger dispatched record" "$dispatched" fleet_ledger_v1
  expect_accepted "ledger pr_ready record" "$pr_ready" fleet_ledger_v1
  expect_rejected "ledger record is not an envelope" "$dispatched" identity_unresolved /user
  jq -c '.v = 2' "$dispatched" > "$TMP_ROOT/ledger-v2.json"
  expect_rejected "ledger v2 record" "$TMP_ROOT/ledger-v2.json" schema_invalid /v fleet_ledger_v1
  jq -c '.future_member = {"ok": true}' "$dispatched" > "$TMP_ROOT/ledger-future.json"
  expect_accepted "ledger record with an unknown member" "$TMP_ROOT/ledger-future.json" fleet_ledger_v1
  jq -c 'del(.task)' "$dispatched" > "$TMP_ROOT/ledger-no-task.json"
  expect_rejected "ledger record without task" "$TMP_ROOT/ledger-no-task.json" schema_invalid /task fleet_ledger_v1
  pass "real fleet ledger records validate under fleet_ledger_v1 and nowhere else"
}

test_validator_usage_and_input_errors_exit_2() {
  local out rc file
  out=$(python3 "$VALIDATOR" 2>&1); rc=$?
  assert_equals 2 "$rc" "no subcommand must exit 2: $out"
  file=$(envelope usage)
  out=$(python3 "$VALIDATOR" validate --definition bogus "$file" 2>&1); rc=$?
  assert_equals 2 "$rc" "unknown definition must exit 2: $out"
  printf '{"schema":' > "$TMP_ROOT/malformed.json"
  out=$(python3 "$VALIDATOR" validate "$TMP_ROOT/malformed.json" 2>&1); rc=$?
  assert_equals 2 "$rc" "malformed JSON must exit 2: $out"
  printf '{"schema":"a","schema":"a"}' > "$TMP_ROOT/duplicate.json"
  out=$(python3 "$VALIDATOR" validate "$TMP_ROOT/duplicate.json" 2>&1); rc=$?
  assert_equals 2 "$rc" "duplicate keys must exit 2: $out"
  out=$(python3 "$VALIDATOR" validate "$TMP_ROOT/absent.json" 2>&1); rc=$?
  assert_equals 2 "$rc" "unreadable input must exit 2: $out"
  out=$(python3 "$VALIDATOR" validate - < "$file" 2>&1); rc=$?
  assert_equals 0 "$rc" "stdin input must validate: $out"
  pass "usage, malformed, duplicate-key, and unreadable input exit 2; stdin validates"
}

test_schema_evaluator_fails_closed() {
  if ! python3 - "$VALIDATOR" <<'PY'
import importlib.util
import sys

spec = importlib.util.spec_from_file_location("validator", sys.argv[1])
module = importlib.util.module_from_spec(spec)
spec.loader.exec_module(module)
for schema in ({"items": {}}, {"pattern": "abc"}, {"additionalProperties": {}}):
    try:
        module.evaluate(schema, "abc", {}, "", [])
    except module.SchemaError:
        continue
    sys.exit("construct accepted without an error: %r" % (schema,))
PY
  then
    fail "schema evaluator must fail closed on unsupported constructs"
  fi
  pass "schema evaluator fails closed on unknown keywords, unanchored patterns, and open additionalProperties"
}

test_every_required_member_is_enforced() {
  local name
  for name in schema event_id seq ts prev_hash event category outcome platform home_id \
    day_key session_id agent actor repo pr commit; do
    expect_rejected "missing $name" "$(envelope "missing-$name" "del(.$name)")" schema_invalid "*"
  done
  pass "every required envelope member is enforced"
}

test_member_formats_and_enums() {
  expect_accepted "task_id is optional" "$(envelope no-task 'del(.task_id)')"
  expect_rejected "empty task_id" "$(envelope empty-task '.task_id = ""')" schema_invalid /task_id
  expect_rejected "platform literal" "$(envelope platform '.platform = "other"')" schema_invalid /platform
  expect_rejected "event_id is not UUIDv7" \
    "$(envelope event-v4 '.event_id = "018f3b7e-9c1a-4d2e-8f3a-4b5c6d7e8f90"')" schema_invalid /event_id
  expect_rejected "event_id uppercase" \
    "$(envelope event-upper '.event_id = "018F3B7E-9C1A-7D2E-8F3A-4B5C6D7E8F90"')" schema_invalid /event_id
  expect_rejected "ts without milliseconds" "$(envelope ts-short '.ts = "2026-10-16T02:00:00Z"')" schema_invalid /ts
  expect_rejected "negative seq" "$(envelope seq-neg '.seq = -1')" schema_invalid /seq
  expect_rejected "fractional seq" "$(envelope seq-frac '.seq = 1.5')" schema_invalid /seq
  expect_rejected "event name shape" "$(envelope event-name '.event = "Task Dispatched"')" schema_invalid /event
  expect_rejected "category enum" "$(envelope category '.category = "other"')" schema_invalid /category
  expect_rejected "outcome enum" "$(envelope outcome '.outcome = "ok"')" schema_invalid /outcome
  expect_rejected "agent role enum" "$(envelope role '.agent.role = "admin"')" schema_invalid /agent/role
  expect_rejected "actor type enum" "$(envelope actor-type '.actor.type = "robot"')" schema_invalid /actor/type
  expect_rejected "empty session_id" "$(envelope session-empty '.session_id = ""')" schema_invalid /session_id
  expect_rejected "repo without owner" "$(envelope repo '.repo = "no-owner"')" schema_invalid /repo
  expect_accepted "https PR URL" "$(envelope pr '.pr = "https://github.com/acme/webapp/pull/7"')"
  expect_rejected "http PR URL" "$(envelope pr-http '.pr = "http://github.com/acme/webapp/pull/7"')" schema_invalid /pr
  expect_accepted "full commit" "$(envelope commit '.commit = "0123456789abcdef0123456789abcdef01234567"')"
  expect_rejected "short commit" "$(envelope commit-short '.commit = "abc123"')" schema_invalid /commit
  expect_accepted "system actor needs no user binding" \
    "$(envelope system-actor '.actor = {type: "system", id: "scheduler"}')"
  pass "member formats, enums, and optional members match the contract"
}

test_genesis_and_chain_fields() {
  expect_accepted "later record with a previous hash" "$(envelope chained ".seq = 3 | .prev_hash = \"$HASH\"")"
  expect_rejected "seq 0 with a previous hash" "$(envelope genesis-hash ".prev_hash = \"$HASH\"")" \
    schema_invalid /prev_hash
  expect_rejected "later record without a previous hash" "$(envelope chain-null '.seq = 3')" \
    schema_invalid /prev_hash
  expect_rejected "malformed previous hash" "$(envelope hash-bad '.seq = 3 | .prev_hash = "sha256:xyz"')" \
    schema_invalid /prev_hash
  pass "prev_hash is null exactly at genesis and well-formed otherwise"
}

test_actor_binding_to_user_id() {
  expect_rejected "user actor carrying another identity" \
    "$(envelope actor-other ".actor.id = \"$OTHER_USER_ID\"")" schema_invalid /actor/id
  pass "a user actor must carry the envelope's own user.id"
}

test_day_key_is_recomputed_in_the_user_timezone() {
  # 02:00Z on 2026-10-16 is 2026-10-15 in Chicago (CDT) and already 2026-10-16 in Kiritimati (UTC+14).
  expect_accepted "Chicago day key" "$(envelope chicago)"
  expect_rejected "Kiritimati with the Chicago date" \
    "$(envelope kiritimati-old '.user.timezone = "Pacific/Kiritimati"')" day_key_mismatch /day_key
  expect_accepted "Kiritimati day key" \
    "$(envelope kiritimati '.user.timezone = "Pacific/Kiritimati" | .day_key = "2026-10-16"')"
  expect_rejected "UTC with the Chicago date" "$(envelope utc '.user.timezone = "UTC"')" \
    day_key_mismatch /day_key
  expect_rejected "unknown timezone" "$(envelope bad-zone '.user.timezone = "Mars/Olympus"')" \
    schema_invalid /user/timezone
  expect_rejected "path-like timezone" "$(envelope path-zone '.user.timezone = "../etc/localtime"')" \
    schema_invalid /user/timezone
  expect_rejected "impossible calendar date" "$(envelope bad-ts '.ts = "2026-02-30T02:00:00.000Z"')" \
    schema_invalid /ts
  pass "day_key is recomputed from ts in the user's timezone and never hardcoded"
}

test_out_of_range_dates_are_rejected_and_recorded() {
  local rec="$TMP_ROOT/range-state" log="$TMP_ROOT/range-state/audit-rejections.jsonl"
  mkdir -p "$rec"
  env FM_STATE_OVERRIDE="$rec" python3 "$VALIDATOR" validate \
    "$(envelope min-date '.ts = "0001-01-01T00:00:00.000Z" | .day_key = "0001-01-01"')" >"$TMP_ROOT/min.out" 2>&1
  RC=$?
  assert_equals 1 "$RC" "earliest date in a western zone must exit 1: $(cat "$TMP_ROOT/min.out")"
  assert_equals schema_invalid "$(jq -r .reason "$TMP_ROOT/min.out")" "earliest date: printed reason"
  env FM_STATE_OVERRIDE="$rec" python3 "$VALIDATOR" validate \
    "$(envelope max-date '.ts = "9999-12-31T23:59:59.999Z" | .day_key = "9999-12-31" | .user.timezone = "Pacific/Kiritimati"')" \
    >"$TMP_ROOT/max.out" 2>&1
  RC=$?
  assert_equals 1 "$RC" "latest date in an eastern zone must exit 1: $(cat "$TMP_ROOT/max.out")"
  assert_equals schema_invalid "$(jq -r .reason "$TMP_ROOT/max.out")" "latest date: printed reason"
  assert_equals 2 "$(wc -l < "$log" | tr -d ' ')" "each out-of-range rejection appends one record"
  jq -e '.definition == "envelope" and .reason == "schema_invalid" and .paths == ["/ts"]' "$log" >/dev/null \
    || fail "out-of-range rejections not recorded as expected: $(cat "$log")"
  pass "dates outside the representable range are rejected and recorded, not a traceback"
}

test_user_id_and_home_id_formats() {
  expect_accepted "pinned user id and home_id" "$(envelope pinned-ids)"
  expect_rejected "empty user id" "$(envelope empty-id '.user.id = ""')" schema_invalid /user/id
  expect_rejected "truncated user id" "$(envelope short-hmac '.user.id = "hmac-sha256:abc"')" \
    schema_invalid /user/id
  expect_rejected "raw identity as user id" "$(envelope raw-id '.user.id = "jdoe"')" schema_invalid /user/id
  assert_not_contains "$OUT" jdoe "raw identity must not be echoed"
  expect_rejected "empty home_id" "$(envelope home-empty '.home_id = ""')" schema_invalid /home_id
  expect_rejected "non-string home_id" "$(envelope home-num '.home_id = 5')" schema_invalid /home_id
  expect_rejected "home_id that is UUIDv7" "$(envelope home-v7 '.home_id = "018f3b7e-9c1a-7d2e-8f3a-4b5c6d7e8f90"')" \
    schema_invalid /home_id
  expect_rejected "home_id that is not a UUID" "$(envelope home-text '.home_id = "home-1"')" \
    schema_invalid /home_id
  expect_accepted "git email identity source" "$(envelope git-email '.user.identity_source = "git_email"')"
  expect_rejected "unknown identity source" "$(envelope source '.user.identity_source = "email"')" \
    schema_invalid /user/identity_source
  pass "user.id is HMAC-SHA256 shaped, identity_source is closed, and home_id is UUIDv4"
}

test_schema_alone_refuses_unresolved_identity() {
  local out
  out=$(python3 - "$VALIDATOR" "$ROOT/docs/audit-envelope.v1.schema.json" "$(envelope schema-only)" <<'PY'
import importlib.machinery, importlib.util, json, sys
loader = importlib.machinery.SourceFileLoader("envelope_validator", sys.argv[1])
module = importlib.util.module_from_spec(importlib.util.spec_from_loader(loader.name, loader))
loader.exec_module(module)
schema = json.load(open(sys.argv[2]))
base = json.load(open(sys.argv[3]))
def paths(mutate):
    doc = json.loads(json.dumps(base))
    mutate(doc)
    errors = []
    module.evaluate(schema, doc, schema, "", errors)
    return ",".join(sorted(set(errors)))
print("base=" + paths(lambda d: None))
print("id=" + paths(lambda d: d["user"].update(id=None)))
print("source=" + paths(lambda d: d["user"].update(identity_source=None)))
PY
  ) || fail "schema-only evaluation failed: $out"
  assert_contains "$out" "base=
" "resolved identity must pass the schema alone"
  assert_contains "$out" "id=/user/id" "schema alone must refuse a null user.id"
  assert_contains "$out" "source=/user/identity_source" "schema alone must refuse a null identity_source"
  pass "the schema alone refuses an unresolved identity, matching the identity gate"
}

test_rejections_are_recorded_without_identity() {
  local rec="$TMP_ROOT/record-state" log="$TMP_ROOT/record-state/audit-rejections.jsonl" before after
  mkdir -p "$rec"
  env FM_STATE_OVERRIDE="$rec" python3 "$VALIDATOR" validate "$(envelope rec-gate '.user.id = null')" \
    >/dev/null 2>&1; RC=$?
  assert_equals 1 "$RC" "identity rejection must exit 1"
  jq -e '.v == 1 and .definition == "envelope" and .reason == "identity_unresolved"
    and .paths == ["/user/id"] and (.rejected_at | type == "number")' "$log" >/dev/null \
    || fail "identity rejection not recorded as expected: $(cat "$log" 2>/dev/null)"
  env FM_STATE_OVERRIDE="$rec" python3 "$VALIDATOR" validate \
    "$(envelope rec-day '.user.timezone = "UTC"')" >/dev/null 2>&1
  env FM_STATE_OVERRIDE="$rec" python3 "$VALIDATOR" validate \
    "$(envelope rec-raw '.actor.id = "jdoe"')" >/dev/null 2>&1
  assert_equals 3 "$(wc -l < "$log" | tr -d ' ')" "each rejection appends one record"
  assert_not_contains "$(cat "$log")" "$USER_ID" "record must not contain identity values"
  assert_not_contains "$(cat "$log")" jdoe "record must not contain raw identity"
  before=$(wc -l < "$log")
  env FM_STATE_OVERRIDE="$rec" python3 "$VALIDATOR" validate "$(envelope rec-ok)" >/dev/null 2>&1
  RC=$?
  after=$(wc -l < "$log")
  assert_equals 0 "$RC" "base envelope must still be accepted"
  assert_equals "$before" "$after" "accepted documents must write nothing"

  env -u FM_HOME -u FM_STATE_OVERRIDE python3 "$VALIDATOR" validate \
    "$(envelope rec-unset '.user.id = null')" >"$TMP_ROOT/unset.out" 2>"$TMP_ROOT/unset.err"
  RC=$?
  assert_equals 2 "$RC" "unrecordable rejection must exit 2"
  grep -q "rejection not recorded" "$TMP_ROOT/unset.err" || fail "unrecordable rejection must say so"
  assert_equals "" "$(cat "$TMP_ROOT/unset.out")" "unrecorded rejection must not print a rejected result"

  env FM_STATE_OVERRIDE="$TMP_ROOT/no-such-dir" python3 "$VALIDATOR" validate \
    "$(envelope rec-missing '.user.id = null')" >/dev/null 2>"$TMP_ROOT/missing.err"
  RC=$?
  assert_equals 2 "$RC" "rejection into a missing state directory must exit 2"
  pass "rejections append one sanitized record each and an unrecordable rejection exits 2"
}

test_real_ledger_records_validate_as_ledger_v1_only
test_identity_gate_rejects_unresolved_identity
test_raw_identity_never_passes
test_schema_is_versioned_and_closed
test_validator_usage_and_input_errors_exit_2
test_schema_evaluator_fails_closed
test_every_required_member_is_enforced
test_member_formats_and_enums
test_genesis_and_chain_fields
test_actor_binding_to_user_id
test_day_key_is_recomputed_in_the_user_timezone
test_out_of_range_dates_are_rejected_and_recorded
test_user_id_and_home_id_formats
test_schema_alone_refuses_unresolved_identity
test_rejections_are_recorded_without_identity
