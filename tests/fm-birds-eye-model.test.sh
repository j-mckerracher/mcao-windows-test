#!/usr/bin/env bash
# Behavior tests for the bird's-eye aggregator (fm-birds-eye-view.v1). Assertions read
# the command's exit status and its published document only. Scenarios replay recorded
# producer snapshots from tests/fixtures/birds-eye through --fixture-dir, except the one
# live smoke case, which runs the real producer against an empty temporary home.
set -u

# shellcheck source=tests/lib.sh
# shellcheck disable=SC1091
. "$(dirname "${BASH_SOURCE[0]}")/lib.sh"

MODEL="$ROOT/bin/fm-birds-eye-model.sh"
FIXTURES="$ROOT/tests/fixtures/birds-eye"
TMP_ROOT=$(fm_test_tmproot fm-birds-eye-model)

command -v jq >/dev/null 2>&1 || { echo "skip: jq not found"; exit 0; }

# Observer clock. The recorded producers generated at 05:06:30Z, which is GEN_EPOCH, so
# a default-interval run at NOW reports a collection age of 10 seconds.
NOW=1792300000
GEN_EPOCH=$((NOW - 10))

# model <out> <scenario> [args...]: replay one fixture scenario into <out>, stderr into
# <out>.err, and return the aggregator's exit status. Later args override the defaults.
model() {
  local out=$1 scenario=$2
  shift 2
  bash "$MODEL" --fixture-dir "$FIXTURES/$scenario" --now "$NOW" "$@" >"$out" 2>"$out.err"
}

# expect <label> <json-file> <jq-filter>: the filter must evaluate to a truthy result.
expect() {
  if ! jq -e "$3" "$2" >/dev/null 2>&1; then
    fail "$1 (filter: $3; got: $(jq -c "$3" "$2" 2>&1 | head -c 400))"
  fi
}

# expect_usage_error <label> <stderr-fragment> [args...]: exit 2 and a named refusal.
expect_usage_error() {
  local label=$1 fragment=$2 err="$TMP_ROOT/usage.err" code=0
  shift 2
  bash "$MODEL" --fixture-dir "$FIXTURES/two-roots" "$@" >/dev/null 2>"$err" || code=$?
  expect_code 2 "$code" "$label"
  assert_grep "$fragment" "$err" "$label must name the refusal"
}

test_usage_and_argument_errors() {
  local out="$TMP_ROOT/help.out" code=0
  bash "$MODEL" --help >"$out" 2>&1 || code=$?
  expect_code 0 "$code" "--help must exit 0"
  assert_grep "Usage: fm-birds-eye-model.sh" "$out" "--help must print usage"
  expect_usage_error "an unknown flag is refused" "unknown argument: --watch" --watch
  expect_usage_error "an interval below five seconds is refused" \
    "--interval must be at least 5 seconds (got 4)" --interval 4
  expect_usage_error "a non-numeric interval is refused" \
    "--interval must be a whole number of seconds" --interval abc
  expect_usage_error "a non-numeric clock is refused" "--now must be an epoch in seconds" --now abc
  expect_usage_error "a missing previous file is refused" "--previous file not found" \
    --previous "$TMP_ROOT/absent.json"
  expect_usage_error "a previous file that is not a view document is refused" \
    "--previous must name a fm-birds-eye-view.v1 document" --previous "$FIXTURES/snapshots/good.json"
  model "$TMP_ROOT/interval-5.json" two-roots --interval 5 || fail "interval 5 is the accepted minimum"
  expect "interval 5 is accepted with a 30-second staleness floor" "$TMP_ROOT/interval-5.json" \
    '.refresh == {interval_seconds: 5, stale_after_seconds: 30}'
  pass "usage errors exit 2 with a named refusal and interval 5 is accepted"
}

test_default_run_contract() {
  local out="$TMP_ROOT/two-roots.json"
  model "$out" two-roots || fail "two-roots scenario must render"
  expect "the document is the fm-birds-eye-view.v1 schema" "$out" \
    '.schema == "fm-birds-eye-view.v1"'
  expect "the default interval is 10 seconds with a 30-second staleness floor" "$out" \
    '.refresh == {interval_seconds: 10, stale_after_seconds: 30}'
  expect "both configured roots are read by default and reported explicitly" "$out" \
    '([.roots[].root_id] == ["/fleet/alpha", "/fleet/beta"]) and all(.roots[]; .status == "ok" and .error == null and .stale == false)'
  expect "native subagents are listed as unobserved and never counted" "$out" \
    '(any(.unsupported[]; .category == "native_subagents")) and (.counts | has("native_subagents") | not)'
  expect "every age is null or non-negative" "$out" \
    '[.entities[] | (.observation.collection_age_seconds, .observation.source_event_age_seconds, .decision.age_seconds, .blocker.age_seconds, .timing.state_seconds)] | map(select(. != null)) | all(. >= 0)'
  pass "default run reads both configured roots and reports the contract's refresh and unobserved categories"
}

test_entity_fields_match_the_contract() {
  local two="$TMP_ROOT/entity-two-roots.json" mates="$TMP_ROOT/entity-overlap.json" out
  model "$two" two-roots || fail "two-roots scenario must render"
  model "$mates" overlap || fail "overlap scenario must render"
  for out in "$two" "$mates"; do
    expect "the top-level keys are exactly the documented set" "$out" \
      '(keys | sort) == ["attention","counts","entities","generated_at","generated_epoch","history","refresh","roots","schema","unsupported"]'
    expect "generated_epoch is the observer clock and generated_at renders it in UTC" "$out" \
      ".generated_epoch == $NOW and .generated_at == (.generated_epoch | todate)"
    expect "every entity carries exactly the documented field set" "$out" \
      'all(.entities[]; (keys | sort) == ["artifacts","blocker","child_keys","classification","contextual_in","decision","decisions","execution","home_id","key","kind","latest_activity","name","observation","parent_key","prerequisites","project","role","spawn_gen","state","task_id","timing"])'
    expect "kind is worker, secondmate, or supervisor" "$out" \
      'all(.entities[]; .kind == "worker" or .kind == "secondmate" or .kind == "supervisor")'
    expect "a task key is its home id plus its task id" "$out" \
      'all(.entities[] | select(.kind == "worker"); .key == (.home_id + ":" + .task_id))'
    expect "child_keys lists exactly the entities whose parent_key is that entity" "$out" \
      ". as \$d | all(\$d.entities[]; . as \$e | ([\$d.entities[] | select(.parent_key == \$e.key) | .key] | sort) == (\$e.child_keys | sort))"
    expect "a home is named by the base name of its directory" "$out" \
      'all(.entities[] | select(.key == .home_id); .name == (.home_id | split("/") | last))'
    expect "a home has no prerequisites and no activity time" "$out" \
      'all(.entities[] | select(.key == .home_id); (.prerequisites == []) and (.latest_activity.at == null))'
    expect "reported_by_agent is always null because no source names the reporter" "$out" \
      'all(.entities[]; .latest_activity.reported_by_agent == null)'
    expect "every prerequisite is a key with the backlog source" "$out" \
      'all(.entities[].prerequisites[]?; (keys | sort) == ["key","source"] and .source == "backlog")'
  done
  expect "the two-roots fixture carries a prerequisite, so the prerequisite check is not vacuous" "$two" \
    '[.entities[].prerequisites[]] | length > 0'
  expect "the overlap fixture carries a secondmate, so the child_keys check covers a home that delegates" "$mates" \
    'any(.entities[]; .kind == "secondmate")'
  pass "entity fields, kinds, and linkage match the documented fm-birds-eye-view.v1 contract"
}

test_overlap_ownership_and_unique_counts() {
  local out="$TMP_ROOT/overlap.json"
  model "$out" overlap || fail "overlap scenario must render"
  expect "each canonical home is read once, including the shared secondmate" "$out" \
    '([.roots[].root_id] == ["/fleet/alpha", "/fleet/beta", "/fleet/shared"]) and ([.roots[].root_id] | unique | length) == 3'
  expect "the shared secondmate is discovered from its parent as a descendant" "$out" \
    '(.roots[] | select(.root_id == "/fleet/shared") | [.source, .parent_root_id]) == ["descendant", "/fleet/alpha"]'
  expect "a shared secondmate is one entity owned by the root that read it" "$out" \
    '([.entities[] | select(.key == "home:/fleet/shared")] | length) == 1 and (.entities[] | select(.key == "home:/fleet/shared") | .observation.root_id) == "/fleet/shared"'
  expect "contextual copies name the other homes that list the same secondmate" "$out" \
    '((.entities[] | select(.key == "home:/fleet/shared") | .contextual_in) == ["project:beta-proj", "root:/fleet/beta"]) and (.counts.contextual_copies == 2)'
  expect "global counts dedupe canonical identities" "$out" \
    '(.counts.unique_entities == 7) and ((.entities | length) == 7) and (((.counts.workers_by_state | add) + (.counts.supervisors_by_classification | add)) == 7)'
  expect "workers and supervisors are classified from their own state" "$out" \
    '(.counts.workers_by_state == {"Completed": 1, "Running": 3}) and (.counts.supervisors_by_classification == {"active_child_work": 2, "no_active_work": 1})'
  expect "projects group by name and rows that record none count as Unassigned" "$out" \
    '.counts.by_project == {"Unassigned": 2, "alpha-proj": 3, "beta-proj": 2}'
  expect "same display names across homes are not merged" "$out" \
    '([.entities[] | select(.name == "Same Name") | .key] | length) == 2 and ([.entities[] | select(.name == "Same Name") | .key] | unique | length) == 2'
  pass "overlapping roots dedupe to one owner per entity with contextual copies and unique counts"
}

test_failed_root_keeps_last_good_and_recovers() {
  local healthy="$TMP_ROOT/healthy.json" failed="$TMP_ROOT/beta-failed.json"
  local recovered="$TMP_ROOT/recovered.json" before after
  model "$healthy" two-roots || fail "healthy two-roots run must render"
  model "$failed" two-roots-beta-failed --previous "$healthy" || fail "failed beta run must still render"
  expect "a failing root is reported with its own error" "$failed" \
    '(.roots[] | select(.root_id == "/fleet/beta") | [.status, .error, .stale]) == ["failed", "producer exited 1", true]'
  expect "the other root keeps updating normally" "$failed" \
    '(.roots[] | select(.root_id == "/fleet/alpha") | [.status, .stale]) == ["ok", false]'
  expect "the failed root keeps its last-good records labelled cached and stale" "$failed" \
    '([.entities[] | select(.observation.root_id == "/fleet/beta")] | length) == 3 and all(.entities[] | select(.observation.root_id == "/fleet/beta"); .observation.quality == "cached" and .observation.stale == true)'
  expect "retained records keep their age and the failure surfaces as its own attention item" "$failed" \
    '((.entities[] | select(.key == "home:/fleet/beta") | .observation.collection_age_seconds) == 10) and ([.attention[] | select(.key == "root:/fleet/beta") | .class] == ["observation"])'

  model "$recovered" two-roots --previous "$failed" || fail "recovery run must render"
  expect "recovery clears the root error without any task transition" "$recovered" \
    '((.roots[] | select(.root_id == "/fleet/beta") | [.status, .error, .stale]) == ["ok", null, false]) and ([.attention[] | select(.class == "observation")] | length == 0)'
  before=$(jq -c '[.entities[] | [.key, .classification, .state.display, .state.raw]] | sort' "$healthy")
  after=$(jq -c '[.entities[] | [.key, .classification, .state.display, .state.raw]] | sort' "$recovered")
  assert_equals "$before" "$after" "recovery must reproduce the healthy classification and state exactly"
  pass "a failing root keeps last-good records and recovery clears the error without task transitions"
}

test_stale_output_is_rejected() {
  local out="$TMP_ROOT/stale.json" healthy="$TMP_ROOT/healthy.json"
  model "$healthy" two-roots || fail "healthy two-roots run must render"
  model "$out" stale-output --previous "$healthy" || fail "stale-output run must render"
  expect "an older snapshot cannot roll a root back and is reported failed" "$out" \
    '(.roots[] | select(.root_id == "/fleet/alpha") | [.status, .error, .last_good_at]) == ["failed", "stale output rejected: producer generated time is older than the last good collection", "2026-10-18T05:06:30Z"]'
  expect "the rejected root serves its last-good records as cached" "$out" \
    '([.entities[] | select(.observation.root_id == "/fleet/alpha") | .observation.quality] | length > 0) and all(.entities[] | select(.observation.root_id == "/fleet/alpha"); .observation.quality == "cached")'
  pass "a snapshot older than the last good collection is rejected as stale output"
}

test_staleness_boundaries_and_refresh_floor() {
  local out="$TMP_ROOT/staleness.json" pair interval stale_after
  for pair in 5:30 10:30 11:33 20:60; do
    interval=${pair%%:*}
    stale_after=${pair#*:}
    model "$out" two-roots --interval "$interval" || fail "interval $interval must render"
    expect "interval $interval gives staleness max(30, 3 x interval) = $stale_after" "$out" \
      ".refresh == {interval_seconds: $interval, stale_after_seconds: $stale_after}"
  done
  model "$out" two-roots --now $((GEN_EPOCH + 30)) || fail "age-30 run must render"
  expect "age 30 at interval 10 is not stale" "$out" \
    '(.roots[] | select(.root_id == "/fleet/alpha") | [.stale, .last_good_age_seconds]) == [false, 30]'
  model "$out" two-roots --now $((GEN_EPOCH + 31)) || fail "age-31 run must render"
  expect "age 31 at interval 10 is stale and raises an observation item" "$out" \
    '((.roots[] | select(.root_id == "/fleet/alpha") | [.stale, .last_good_age_seconds]) == [true, 31]) and any(.attention[]; .key == "root:/fleet/alpha" and .class == "observation")'
  model "$out" two-roots --interval 11 --now $((GEN_EPOCH + 33)) || fail "age-33 run must render"
  expect "age 33 at interval 11 is not stale" "$out" \
    '(.roots[] | select(.root_id == "/fleet/alpha") | [.stale, .last_good_age_seconds]) == [false, 33]'
  model "$out" two-roots --interval 11 --now $((GEN_EPOCH + 34)) || fail "age-34 run must render"
  expect "age 34 at interval 11 is stale" "$out" \
    '(.roots[] | select(.root_id == "/fleet/alpha") | [.stale, .last_good_age_seconds]) == [true, 34]'
  pass "staleness is max(30, 3 x interval) with exact boundaries at the default and a longer interval"
}

test_timestamp_and_state_faults() {
  local out="$TMP_ROOT/timestamps.json"
  model "$out" timestamps || fail "timestamps scenario must render"
  expect "missing, invalid, and future timestamps give null ages" "$out" \
    '((.entities[] | select(.key == "home:/fleet/ts:ts-missing-age") | .observation.source_event_age_seconds) == null) and ((.entities[] | select(.key == "home:/fleet/ts:ts-negative-age") | .observation.source_event_age_seconds) == null) and ((.entities[] | select(.key == "home:/fleet/ts:ts-string-age") | .observation.source_event_age_seconds) == null) and ((.entities[] | select(.key == "home:/fleet/ts:ts-future-observed") | .state.observed_at) == null) and ((.entities[] | select(.key == "home:/fleet/ts:ts-bad-observed") | .state.observed_at) == null)'
  expect "a usable event age is labelled as a status-event age" "$out" \
    '(.entities[] | select(.key == "home:/fleet/ts:ts-valid-age") | [.observation.source_event_age_seconds, .observation.source_event_age_basis]) == [70, "status_event"]'
  expect "a blocker's age comes from its status event" "$out" \
    '(.entities[] | select(.key == "home:/fleet/ts:ts-blocked") | .blocker | [.age_seconds, .age_basis]) == [130, "status_event"]'
  expect "a hold without a usable age stays Needs you with a null age" "$out" \
    '(.entities[] | select(.key == "home:/fleet/ts:ts-hold-negative") | .decision | [.label, .age_seconds, .age_basis]) == ["Needs you", null, null]'
  expect "an unset state is Not recorded rather than Unknown" "$out" \
    '((.entities[] | select(.key == "home:/fleet/ts:ts-null-state") | [.state.display, .observation.quality]) == [null, "unavailable"]) and (.counts.workers_by_state["Not recorded"] == 1)'
  expect "an unrecognised state keeps its raw value, shows Unknown, and stays unclassified" "$out" \
    '(.entities[] | select(.key == "home:/fleet/ts:ts-reviewing") | [.state.raw, .state.display, .classification, .observation.quality]) == ["reviewing", "Unknown", "unknown", "fresh"]'
  expect "a cached observation is labelled cached" "$out" \
    '(.entities[] | select(.key == "home:/fleet/ts:ts-cached") | .observation.quality) == "cached"'
  expect "an unsupported or unrecorded backend keeps its state but not a fresh quality" "$out" \
    '((.entities[] | select(.key == "home:/fleet/ts:ts-remote") | [.state.raw, .observation.quality]) == ["working", "unknown"]) and ((.entities[] | select(.key == "home:/fleet/ts:ts-blank-backend") | .observation.quality) == "unknown") and (any(.unsupported[]; .category == "backend:kiro")) and (any(.unsupported[]; .category == "backend:unrecorded"))'
  expect "a fresh unknown state stays unknown with its quality fresh" "$TMP_ROOT/timestamps.json" \
    '(.entities[] | select(.key == "home:/fleet/ts:ts-reviewing") | [.state.display, .classification]) == ["Unknown", "unknown"]'
  expect "no age anywhere is negative" "$out" \
    '[.entities[] | (.observation.collection_age_seconds, .observation.source_event_age_seconds, .decision.age_seconds, .blocker.age_seconds, .timing.state_seconds)] | map(select(. != null)) | all(. >= 0)'
  pass "timestamp and state faults degrade to null or Unknown without a crash"
}

test_generation_rollover() {
  local gold="$TMP_ROOT/gen-old.json" gnew="$TMP_ROOT/gen-new.json" out="$TMP_ROOT/gen-rollover.json"
  model "$gold" generation-old || fail "generation-old scenario must render"
  model "$gnew" generation-new || fail "generation-new scenario must render"
  model "$out" generation-new --previous "$gold" || fail "generation rollover must render"
  expect "a newer generation replaces the old record and keeps the earlier incarnation as history" "$out" \
    '(([.entities[] | select(.key == "home:/fleet/alpha:ship-x")] | length) == 1) and ((.entities[] | select(.key == "home:/fleet/alpha:ship-x") | [.spawn_gen, .observation.quality, .state.display]) == [2, "fresh", "Parked"]) and ([.history.summaries[] | select(.key == "home:/fleet/alpha:ship-x") | [.generation, .historical]] == [[1, true]])'
  model "$out" generation-old --previous "$gnew" || fail "older-generation run must render"
  expect "an older generation never overwrites a newer record" "$out" \
    '(([.entities[] | select(.key == "home:/fleet/alpha:ship-x")] | length) == 1) and ((.entities[] | select(.key == "home:/fleet/alpha:ship-x") | [.spawn_gen, .observation.quality]) == [2, "cached"]) and ([.history.summaries[] | select(.key == "home:/fleet/alpha:ship-x") | .generation] == [1])'
  pass "generation changes reject old incarnations and keep the earlier one as history"
}

test_requested_and_observed_execution_stay_distinct() {
  local out="$TMP_ROOT/execution.json"
  model "$out" two-roots || fail "two-roots scenario must render"
  expect "a recorded requested model and effort are reported as requested" "$out" \
    '(.entities[] | select(.key == "home:/fleet/alpha:ship-live") | .execution | [.requested_model, .requested_effort]) == ["claude-opus-x", "xhigh"]'
  expect "observed model, observed effort, playbook, and workflow stage are never invented" "$out" \
    'all(.entities[]; (.execution.observed_model == null) and (.execution.observed_effort == null) and (.execution.playbook == null) and (.execution.workflow_stage == null))'
  pass "requested execution is reported separately from the unobserved remainder"
}

test_attention_routing() {
  local out="$TMP_ROOT/attention.json" parent="$TMP_ROOT/parent.json"
  model "$out" two-roots || fail "two-roots scenario must render"
  expect "decision items precede failures, which precede blockers" "$out" \
    '[.attention[].class] == ["decision", "decision", "failure", "blocker"]'
  expect "a captain-held decision reads Needs you and a plain open decision reads Decision pending" "$out" \
    '([.attention[] | select(.key == "home:/fleet/alpha:ship-ask") | .label] == ["Needs you"]) and ([.attention[] | select(.key == "home:/fleet/alpha:ship-pending") | .label] == ["Decision pending"])'
  model "$parent" parent || fail "parent scenario must render"
  expect "a parent waiting on healthy children stays routine with no attention item" "$parent" \
    '(.attention == []) and (.entities[] | select(.key == "home:/fleet/parent") | [.classification, .decision]) == ["active_child_work", null]'
  pass "attention orders decisions, failures, and blockers and keeps routine parents quiet"
}

test_bad_roots_are_isolated() {
  local out="$TMP_ROOT/bad-roots.json"
  model "$out" bad-roots || fail "bad-roots scenario must render"
  expect "each invalid root reports its own error while the good root renders" "$out" \
    '((.roots[] | select(.root_id == "/fleet/good") | .status) == "ok") and ((.roots[] | select(.root_id == "/fleet/future") | [.status, .error]) == ["malformed", "producer output has no valid generated time, or it is in the future"]) and ((.roots[] | select(.root_id == "/fleet/schema") | [.status, .error]) == ["unsupported_schema", "unsupported schema: fm-fleet-snapshot.v2"]) and ((.roots[] | select(.root_id == "/fleet/idmismatch") | [.status, (.error | startswith("home identity mismatch"))]) == ["malformed", true]) and ((.roots[] | select(.root_id == "/fleet/badjson") | [.status, .error]) == ["malformed", "producer output is not valid JSON"]) and ((.roots[] | select(.root_id == "/fleet/missing") | [.status, .error]) == ["failed", "no recorded snapshot for this home"])'
  expect "failed and malformed roots are stale and contribute no records" "$out" \
    'all(.roots[] | select(.status != "ok"); .stale == true)'
  expect "only the good root contributes entities" "$out" \
    '[.entities[].observation.root_id] | unique == ["/fleet/good"]'
  pass "invalid roots are classified individually and never contribute records"
}

test_unreached_descendant_keeps_last_good() {
  local healthy="$TMP_ROOT/parent-healthy.json" failed="$TMP_ROOT/parent-failed.json"
  model "$healthy" parent || fail "parent scenario must render"
  model "$failed" parent-failed --previous "$healthy" || fail "parent-failed scenario must render"
  expect "a descendant whose parent could not be read is reported not reached" "$failed" \
    '(.roots[] | select(.root_id == "/fleet/child") | [.status, .error, .stale]) == ["failed", "not reached: parent root /fleet/parent could not be read", true]'
  expect "the unreached descendant keeps its last-good records as cached" "$failed" \
    '([.entities[] | select(.observation.root_id == "/fleet/child")] | length) > 0 and all(.entities[] | select(.observation.root_id == "/fleet/child"); .observation.quality == "cached")'
  pass "a descendant behind an unreadable parent is not reached and keeps last-good records"
}

test_fleet_scale_chain_and_depth() {
  local out="$TMP_ROOT/fleet-200.json"
  model "$out" fleet-200 || fail "fleet-200 scenario must render"
  expect "two hundred homes dedupe to two hundred entities across ten roots" "$out" \
    '(.counts.unique_entities == 200) and ((.entities | length) == 200) and ((.roots | length) == 10) and all(.roots[]; .status == "ok")'
  expect "descendants keep their parent chain past depth eight" "$out" \
    '((.roots[] | select(.root_id == "/fleet/h0") | .parent_root_id) == null) and ((.roots[] | select(.root_id == "/fleet/h8") | .parent_root_id) == "/fleet/h7") and ((.roots[] | select(.root_id == "/fleet/h9") | [.parent_root_id, .status]) == ["/fleet/h8", "ok"])'
  expect "no descendant depth bound is applied or listed" "$out" \
    '[.unsupported[].category] == ["native_subagents"]'
  expect "each secondmate's extra project is a contextual copy" "$out" \
    '.counts.contextual_copies == 9'
  expect "projects group across the fleet with Unassigned for supervisors" "$out" \
    '(.counts.by_project | (has("Unassigned") and has("p0") and has("p9"))) and (([.counts.by_project[]] | add) == 200)'
  expect "classification counts cover every supervisor and no worker" "$out" \
    '.counts.supervisors_by_classification == {"active_child_work": 8, "no_active_work": 1, "unknown": 1}'
  pass "a two-hundred-entity fleet aggregates across ten roots and nine descendant levels"
}

test_deep_chain_cycle_and_unreached() {
  local out="$TMP_ROOT/deep-chain.json" failed="$TMP_ROOT/deep-chain-failed.json"
  model "$out" deep-chain || fail "deep-chain scenario must render"
  expect "a fourteen-home chain is read to its last descendant, each home once" "$out" \
    '((.roots | length) == 14) and ([.roots[].root_id] | unique | length) == 14 and all(.roots[]; .status == "ok") and ((.roots[] | select(.root_id == "/fleet/d13") | [.source, .parent_root_id]) == ["descendant", "/fleet/d12"])'
  expect "the last home links back to the first and that link is refused, not followed" "$out" \
    '(.counts.unique_entities == 28) and any(.unsupported[]; .category == "ownership_cycle" and (.reason | startswith("1 home reference(s)")))'
  expect "the ownership chain stays acyclic from the first home to the deepest" "$out" \
    '((.entities[] | select(.key == "home:/fleet/d0") | .parent_key) == null) and ((.entities[] | select(.key == "home:/fleet/d13") | .parent_key) == "home:/fleet/d12") and ((.entities[] | select(.key == "home:/fleet/d13:w13") | .parent_key) == "home:/fleet/d13")'
  expect "no depth limit is listed" "$out" \
    'any(.unsupported[]; .category == "descendant_depth_limit") | not'
  model "$failed" deep-chain-failed --previous "$out" || fail "deep-chain-failed scenario must render"
  expect "every descendant behind the failed home is reported not reached, at any depth" "$failed" \
    '([.roots[] | select(.status == "failed") | .root_id] | length) == 11 and ((.roots[] | select(.root_id == "/fleet/d13") | [.status, .error, .stale]) == ["failed", "not reached: parent root /fleet/d12 could not be read", true])'
  expect "their last-good records stay as cached" "$failed" \
    '([.entities[] | select(.observation.root_id == "/fleet/d13")] | length) == 2 and all(.entities[] | select(.observation.root_id | IN("/fleet/d3", "/fleet/d4", "/fleet/d13")); .observation.quality == "cached" and .observation.stale == true)'
  pass "chains deeper than eight homes are read, cycles are refused, and unreached descendants keep last-good records"
}

test_root_cap_is_disclosed() {
  local out="$TMP_ROOT/root-cap.json" args=() i
  for i in $(seq 0 69); do
    args+=(--root "/fleet/x$i")
  done
  model "$out" two-roots "${args[@]}" || fail "over-cap roots must still render"
  expect "a run reads at most 64 roots" "$out" '.roots | length == 64'
  expect "the root limit names how many references were not read" "$out" \
    '[.unsupported[] | select(.category == "root_limit") | .reason] == ["8 home reference(s) not read: a run reads at most 64 roots"]'
  pass "the root cap is disclosed with the count of references not read"
}

test_history_eviction_keeps_protected_context() {
  local base="$TMP_ROOT/history-base.json" summaries="$TMP_ROOT/history-summaries.json"
  local prev="$TMP_ROOT/history-prev.json" first="$TMP_ROOT/history-first.json"
  local second="$TMP_ROOT/history-second.json" restart="$TMP_ROOT/history-restart.json"
  model "$base" two-roots || fail "two-roots scenario must render"
  # 1213 prior summaries: the prerequisite ship that the live alpha ship is blocked by,
  # protected with a 14-link ancestor chain above it (13 chain homes and an old home), and
  # 1198 inactive ships, oldest first.
  jq -n '
    [ {key: "home:/fleet/old-home", generation: null, last_state: "Completed",
       last_seen_at: "2026-09-29T00:00:00Z", historical: true, parent_key: null},
      {key: "home:/fleet/alpha:ship-old-0", generation: null, last_state: "Running",
       last_seen_at: "2026-09-30T00:00:00Z", historical: true, parent_key: "home:/fleet/chain-1"} ]
    + [ range(1; 14) as $c
        | {key: ("home:/fleet/chain-\($c)"), generation: null, last_state: "Completed",
           last_seen_at: "2026-09-01T00:00:00Z", historical: true,
           parent_key: (if $c == 13 then "home:/fleet/old-home" else "home:/fleet/chain-\($c + 1)" end)} ]
    + [ range(1; 1199) as $n
        | {key: ("home:/fleet/old:ship-\($n)"), generation: null, last_state: "Completed",
           last_seen_at: (("2026-10-01T00:00:00Z" | fromdateiso8601) + ($n * 60) | todate),
           historical: true, parent_key: "home:/fleet/old"} ]' > "$summaries" || fail "history fixture must build"
  jq --slurpfile s "$summaries" '.history.summaries = $s[0]' "$base" > "$prev" \
    || fail "previous document with history must build"

  model "$first" two-roots --previous "$prev" || fail "capped history run must render"
  expect "history is capped at 1000 with the eviction and the limit disclosed" "$first" \
    '((.history | {limit, retained, evicted, limit_reached}) == {limit: 1000, retained: 1015, evicted: 198, limit_reached: true})'
  expect "the oldest inactive summaries are evicted first" "$first" \
    '(any(.history.summaries[]; .key == "home:/fleet/old:ship-198") | not) and (any(.history.summaries[]; .key == "home:/fleet/old:ship-199"))'
  expect "a required prerequisite and its whole ancestor chain stay outside the allowance" "$first" \
    '(any(.history.summaries[]; .key == "home:/fleet/alpha:ship-old-0")) and (any(.history.summaries[]; .key == "home:/fleet/chain-13")) and (any(.history.summaries[]; .key == "home:/fleet/old-home"))'

  model "$second" two-roots --previous "$first" || fail "second capped run must render"
  expect "a second run over the capped history evicts nothing further" "$second" \
    '(.history | {retained, evicted, limit_reached}) == {retained: 1015, evicted: 0, limit_reached: true}'

  model "$restart" two-roots || fail "restart run must render"
  expect "a restart without previous output starts with no history and says so" "$restart" \
    '((.history | {retained, evicted, limit_reached}) == {retained: 0, evicted: 0, limit_reached: false}) and ((.history.restart_disclosure | length) > 0)'
  pass "history evicts the oldest inactive summaries past 1000 and protects required context"
}

# iso <epoch>: the observer's ISO-8601 form of an epoch.
iso() {
  jq -rn --argjson t "$1" '$t | todate'
}

# one_home_scenario <dir> <state> <spawn_gen> <generated-epoch>: a one-home fixture holding
# gen-1's single ship with the given raw state, spawn generation, and collection time.
one_home_scenario() {
  mkdir -p "$1"
  jq --arg state "$2" --arg gen "$3" --arg at "$(iso "$4")" '
    .generated = $at
    | .tasks[0].spawn_gen = $gen
    | .tasks[0].current_state.state = $state
    | .tasks[0].current_state.raw = $state
    | .tasks[0].current_state.observed_at = $at' "$FIXTURES/snapshots/gen-1.json" > "$1/snapshot.json" \
    || fail "one-home snapshot must build"
  printf '{"configured":["/fleet/alpha"],"homes":{"/fleet/alpha":{"file":"snapshot.json","exit":0}}}\n' > "$1/manifest.json"
}

# observe <out> <dir> <epoch> [previous]: one aggregation at the given observer time.
observe() {
  local out=$1 dir=$2 at=$3 prev=${4:-}
  if [ -n "$prev" ]; then
    bash "$MODEL" --fixture-dir "$dir" --now "$at" --previous "$prev" >"$out" 2>"$out.err"
  else
    bash "$MODEL" --fixture-dir "$dir" --now "$at" >"$out" 2>"$out.err"
  fi
}

test_observed_state_duration_and_transitions() {
  local work="$TMP_ROOT/observed" ship='.entities[] | select(.key == "home:/fleet/alpha:ship-x") | .timing'
  local run1="$work/run1.json" run2="$work/run2.json" run3="$work/run3.json" run4="$work/run4.json"
  local rolled="$work/rolled.json" skewed="$work/skewed.json" prev_skewed="$work/prev-skewed.json"
  local t1=$NOW t2=$((NOW + 60)) t3=$((NOW + 120)) t4=$((NOW + 180)) t5=$((NOW + 240))
  mkdir -p "$work"
  one_home_scenario "$work/working" working 1 $((t1 - 10))
  one_home_scenario "$work/paused" paused 1 $((t3 - 10))
  one_home_scenario "$work/gen2" paused 2 $((t5 - 10))

  observe "$run1" "$work/working" "$t1" || fail "first observation must render"
  expect "a first observation starts an observed duration and records no transition" "$run1" \
    "($ship | [.state_seconds, .state_basis, .observed_state, .observed_since, .observed_transition]) == [0, \"observed\", \"working\", \"$(iso "$t1")\", null]"

  observe "$run2" "$work/working" "$t2" "$run1" || fail "unchanged observation must render"
  expect "an unchanged state keeps its first-observed time and its duration grows" "$run2" \
    "($ship | [.state_seconds, .state_basis, .observed_since, .observed_transition]) == [60, \"observed\", \"$(iso "$t1")\", null]"

  observe "$run3" "$work/paused" "$t3" "$run2" || fail "changed observation must render"
  expect "a state change is a first observed change with a restarted observed duration" "$run3" \
    "($ship | [.state_seconds, .state_basis, .observed_state, .observed_since, .observed_transition]) == [0, \"observed\", \"paused\", \"$(iso "$t3")\", {from: \"working\", to: \"paused\", first_observed_at: \"$(iso "$t3")\", basis: \"first_observed_change\"}]"

  observe "$run4" "$work/paused" "$t4" "$run3" || fail "post-change observation must render"
  expect "the recorded change and its first-observed time persist while the state holds" "$run4" \
    "($ship | [.state_seconds, .observed_since, .observed_transition.from, .observed_transition.first_observed_at]) == [60, \"$(iso "$t3")\", \"working\", \"$(iso "$t3")\"]"

  observe "$rolled" "$work/gen2" "$t5" "$run4" || fail "generation rollover must render"
  expect "a new generation neither inherits the earlier duration nor records a transition" "$rolled" \
    "(.entities[] | select(.key == \"home:/fleet/alpha:ship-x\") | [.spawn_gen, (.timing | .state_seconds, .observed_since, .observed_transition)]) == [2, 0, \"$(iso "$t5")\", null]"

  observe "$work/restart.json" "$work/paused" "$t4" || fail "restart observation must render"
  expect "a restart without previous output starts observing again and claims no change" "$work/restart.json" \
    "($ship | [.state_seconds, .observed_since, .observed_transition]) == [0, \"$(iso "$t4")\", null]"

  jq '(.entities[] | select(.key == "home:/fleet/alpha:ship-x") | .timing.observed_since) = "2099-01-01T00:00:00Z"' "$run4" > "$prev_skewed" \
    || fail "skewed previous document must build"
  observe "$skewed" "$work/paused" "$t5" "$prev_skewed" || fail "future first-observed time must render"
  expect "a future first-observed time is never used to fabricate a duration" "$skewed" \
    "($ship | [.state_seconds, .observed_since]) == [0, \"$(iso "$t5")\"]"
  pass "observed duration and first observed changes carry across runs and reset on restart, generation change, or a bad clock"
}

test_authoritative_state_age_outranks_observed_duration() {
  local out="$TMP_ROOT/authoritative.json" healthy="$TMP_ROOT/auth-healthy.json" failed="$TMP_ROOT/auth-failed.json"
  model "$out" timestamps || fail "timestamps scenario must render"
  expect "a status-event age stays the time in state and is not replaced by an observed duration" "$out" \
    '(.entities[] | select(.key == "home:/fleet/ts:ts-blocked") | .timing | [.state_seconds, .state_basis]) == [130, "status_event"]'
  expect "a state with no authoritative age falls back to an observed duration labelled observed" "$out" \
    '(.entities[] | select(.key == "home:/fleet/ts:ts-valid-age") | .timing | [.state_basis, .observed_transition]) == ["observed", null]'
  expect "a missing state never gets an observed duration" "$out" \
    '(.entities[] | select(.key == "home:/fleet/ts:ts-null-state") | .timing | [.state_seconds, .state_basis, .observed_since]) == [null, null, null]'
  model "$healthy" two-roots || fail "two-roots scenario must render"
  model "$failed" two-roots-beta-failed --previous "$healthy" || fail "failed beta run must render"
  expect "a failed root's retained records keep their observed timing unchanged" "$failed" \
    "([.entities[] | select(.observation.root_id == \"/fleet/beta\") | [.key, .timing]] | sort) == $(jq -c '[.entities[] | select(.observation.root_id == "/fleet/beta") | [.key, .timing]] | sort' "$healthy")"
  pass "authoritative state ages win, missing states stay unobserved, and cached records keep their timing"
}

# silent_scenario <dir> <gen-epoch> <event-age> <state>: one ship in the given state whose last status
# event is <event-age> seconds old at collection, with the collection stamped at <gen-epoch>.
silent_scenario() {
  mkdir -p "$1"
  jq --arg state "$4" --arg at "$(iso "$2")" --argjson age "$3" '
    .generated = $at
    | .tasks[0].spawn_gen = "1"
    | .tasks[0].current_state.state = $state
    | .tasks[0].current_state.raw = $state
    | .tasks[0].current_state.observed_at = $at
    | .tasks[0].paths.status_log.last_event = {state: $state, note: "quiet", age_seconds: $age}
    | .tasks[0].hints.blocked_event = ($state == "blocked")
    | .tasks[0].hints.last_event_text = (if $state == "blocked" then "quiet" else null end)' \
    "$FIXTURES/snapshots/gen-1.json" > "$1/snapshot.json" || fail "silent snapshot must build"
  printf '{"configured":["/fleet/alpha"],"homes":{"/fleet/alpha":{"file":"snapshot.json","exit":0}}}\n' > "$1/manifest.json"
}

test_silent_running_task_is_not_reclassified() {
  local work="$TMP_ROOT/silent" ship='.entities[] | select(.key == "home:/fleet/alpha:ship-x")'
  local run1="$work/run1.json" run2="$work/run2.json" control="$work/control.json"
  local t1=$NOW t2=$((NOW + 60))
  silent_scenario "$work/quiet-1" "$((t1 - 10))" 10800 working
  silent_scenario "$work/quiet-2" "$((t2 - 10))" 10860 working
  silent_scenario "$work/blocked" "$((t1 - 10))" 10800 blocked

  observe "$run1" "$work/quiet-1" "$t1" || fail "silent first observation must render"
  expect "a working task silent for three hours stays Running with no transition" "$run1" \
    "($ship | [.state.display, .state.raw, .classification, .timing.state_basis, .timing.state_seconds, .timing.observed_transition]) == [\"Running\", \"working\", \"active_child_work\", \"observed\", 0, null]"
  expect "the silent task's collection age and status-event age are reported separately, the event age measured from its event time" "$run1" \
    "($ship | [.observation.collection_age_seconds, .observation.source_event_age_seconds]) == [10, 10810]"
  expect "silence raises no blocker, decision, or failure attention for the task" "$run1" \
    '[.attention[] | select(.key == "home:/fleet/alpha:ship-x")] | length == 0'

  observe "$run2" "$work/quiet-2" "$t2" "$run1" || fail "silent refresh must render"
  expect "a refresh with the task still silent keeps Running and grows only its observed duration" "$run2" \
    "($ship | [.state.display, .classification, .timing.state_seconds, .timing.observed_transition, .observation.collection_age_seconds, .observation.source_event_age_seconds]) == [\"Running\", \"active_child_work\", 60, null, 10, 10870]"
  expect "the silent task still raises no attention on refresh" "$run2" \
    '[.attention[] | select(.key == "home:/fleet/alpha:ship-x")] | length == 0'

  observe "$control" "$work/blocked" "$t1" || fail "blocked control must render"
  expect "the same age under a blocked status event is labelled Blocked, so the silent result is not vacuous" "$control" \
    "($ship | [.state.display, .classification, .blocker.age_seconds]) == [\"Blocked\", \"externally_held\", 10810]"
  pass "silence alone never reclassifies a running task, and only the recorded state label changes it"
}

# root_step <dir> <alpha-gen-epoch> <beta-gen-epoch> <beta-exit>: the two-root fixture with each
# root's producer collection time set for this step, and beta's producer exit status.
root_step() {
  mkdir -p "$1"
  jq --arg at "$(iso "$2")" '.generated = $at' "$FIXTURES/snapshots/two-roots-alpha.json" > "$1/alpha.json" \
    || fail "alpha snapshot must build"
  jq --arg at "$(iso "$3")" '.generated = $at' "$FIXTURES/snapshots/two-roots-beta.json" > "$1/beta.json" \
    || fail "beta snapshot must build"
  printf '{"configured":["/fleet/alpha","/fleet/beta"],"homes":{"/fleet/alpha":{"file":"alpha.json","exit":0},"/fleet/beta":{"file":"beta.json","exit":%s}}}\n' "$4" > "$1/manifest.json"
}

test_root_failures_age_independently_under_the_fake_clock() {
  local work="$TMP_ROOT/per-root" healthy="$TMP_ROOT/pr-healthy.json" failed="$TMP_ROOT/pr-failed.json"
  local recovered="$TMP_ROOT/pr-recovered.json" before after expected
  local t1=$NOW t2=$((NOW + 60)) t3=$((NOW + 120))
  root_step "$work/t1" "$((t1 - 10))" "$((t1 - 10))" 0
  root_step "$work/t2" "$((t2 - 10))" "$((t1 - 10))" 1
  root_step "$work/t3" "$((t3 - 10))" "$((t3 - 10))" 0

  observe "$healthy" "$work/t1" "$t1" || fail "healthy two-root observation must render"
  observe "$failed" "$work/t2" "$t2" "$healthy" || fail "beta failure observation must render"
  observe "$recovered" "$work/t3" "$t3" "$failed" || fail "beta recovery observation must render"

  expect "at t2 alpha is fresh at its own age while beta is failed and stale at its own age" "$failed" \
    '((.roots[] | select(.root_id == "/fleet/alpha") | [.status, .stale, .last_good_age_seconds]) == ["ok", false, 10]) and ((.roots[] | select(.root_id == "/fleet/beta") | [.status, .stale, .last_good_age_seconds, .error]) == ["failed", true, 70, "producer exited 1"])'
  expect "alpha's records stay fresh while beta's records are cached and aged from beta's last good collection" "$failed" \
    '(([.entities[] | select(.observation.root_id == "/fleet/alpha") | .observation | [.quality, .stale, .collection_age_seconds]] | unique) == [["fresh", false, 10]]) and (([.entities[] | select(.observation.root_id == "/fleet/beta") | .observation | [.quality, .stale, .collection_age_seconds]] | unique) == [["cached", true, 70]])'

  before=$(jq -c '[.entities[] | select(.observation.root_id == "/fleet/beta") | [.key, .observation.source_event_age_seconds]] | sort' "$healthy")
  after=$(jq -c '[.entities[] | select(.observation.root_id == "/fleet/beta") | [.key, .observation.source_event_age_seconds]] | sort' "$failed")
  expected=$(jq -c '[.[] | [.[0], (if .[1] == null then null else .[1] + 60 end)]]' <<<"$before")
  assert_equals "$expected" "$after" "beta's status-event ages advance with the clock while its collection age is held at its last good time"

  expect "the failure is one attention item for beta and creates no task transition" "$failed" \
    '([.attention[] | select(.key == "root:/fleet/beta") | .class] == ["observation"]) and ([.attention[] | select(.key == "root:/fleet/alpha")] | length == 0)'
  assert_equals "$(jq -c '[.entities[] | [.key, .classification, .state.display, .state.raw]] | sort' "$healthy")" \
    "$(jq -c '[.entities[] | [.key, .classification, .state.display, .state.raw]] | sort' "$failed")" \
    "a failing root must not change any task's classification or state"

  # ship-b2 carries the producer's own cached freshness label in the fixture, so it stays cached
  # after recovery; only the root's staleness and age must return to fresh.
  expect "recovery makes beta fresh at its own age and clears its error and observation attention" "$recovered" \
    '((.roots[] | select(.root_id == "/fleet/beta") | [.status, .stale, .error, .last_good_age_seconds]) == ["ok", false, null, 10]) and ([.entities[] | select(.observation.root_id == "/fleet/beta") | .observation.stale] | all(. == false)) and ([.entities[] | select(.observation.root_id == "/fleet/beta") | .observation.collection_age_seconds] | unique == [10]) and ((.entities[] | select(.key == "home:/fleet/beta:ship-b1") | .observation.quality) == "fresh") and ((.entities[] | select(.key == "home:/fleet/beta:ship-b2") | .observation.quality) == "cached") and ([.attention[] | select(.class == "observation")] | length == 0)'
  assert_equals "$(jq -c '[.entities[] | [.key, .classification, .state.display, .state.raw]] | sort' "$healthy")" \
    "$(jq -c '[.entities[] | [.key, .classification, .state.display, .state.raw]] | sort' "$recovered")" \
    "recovery must reproduce the healthy classification and state exactly"
  pass "failing and recovering roots age independently under the fake clock, keep last-good records, and make no task transitions"
}

# sm_scenario <dir> <parent-jq-filter> [child-exit] [child-jq-filter]: the parent/child fixture with
# the parent (and optionally the child) snapshot rewritten by the filter. The child's producer
# exits with child-exit (default 0).
sm_scenario() {
  mkdir -p "$1"
  jq "$2" "$FIXTURES/snapshots/parent.json" > "$1/parent.json" || fail "parent snapshot must build"
  jq "${4:-.}" "$FIXTURES/snapshots/child.json" > "$1/child.json" || fail "child snapshot must build"
  printf '{"configured":["/fleet/parent"],"homes":{"/fleet/parent":{"file":"parent.json","exit":0},"/fleet/child":{"file":"child.json","exit":%s}}}\n' "${3:-0}" > "$1/manifest.json"
}

SM='.entities[] | select(.key == "home:/fleet/child")'

test_secondmate_record_uses_producer_shape() {
  local work="$TMP_ROOT/sm-record" out="$TMP_ROOT/sm-record.json" quality
  mkdir -p "$work"
  model "$out" parent || fail "parent scenario must render"
  expect "a task row's spawn generation is read as the producer's digit string and reported as an integer" "$out" \
    '(.entities[] | select(.key == "home:/fleet/child:c-ship-1") | .spawn_gen) == 1'
  expect "a secondmate is classified from its record's current state and keeps its row's recorded state" "$out" \
    "($SM | [.kind, .classification, .state.raw, .state.display, .state.source, .spawn_gen]) == [\"secondmate\", \"active_child_work\", \"working\", \"Running\", \"pane\", 1]"
  for quality in fresh cached historical-event unknown; do
    sm_scenario "$work/$quality" ".secondmate_current.records[0].freshness.status = \"$quality\""
    bash "$MODEL" --fixture-dir "$work/$quality" --now "$NOW" > "$work/$quality.json" 2>/dev/null || fail "$quality record must render"
    expect "a record's $quality freshness is the secondmate's observation quality" "$work/$quality.json" \
      "($SM | .observation.quality) == \"$quality\""
  done
  sm_scenario "$work/bogus" '.secondmate_current.records[0].freshness.status = "from-the-future"'
  bash "$MODEL" --fixture-dir "$work/bogus" --now "$NOW" > "$work/bogus.json" 2>/dev/null || fail "bogus freshness must render"
  expect "an unrecognised record freshness degrades to unknown" "$work/bogus.json" \
    "($SM | .observation.quality) == \"unknown\""
  sm_scenario "$work/invalid" '.secondmate_current.records[0] |= (.current = {state: "unknown", reason: "structured home state invalid: ledger torn"} | .provenance.summary_valid = false)'
  bash "$MODEL" --fixture-dir "$work/invalid" --now "$NOW" > "$work/invalid.json" 2>/dev/null || fail "invalid record must render"
  expect "an invalid record is unknown and its recorded reason is the detail" "$work/invalid.json" \
    "($SM | [.classification, .state.detail]) == [\"unknown\", \"structured home state invalid: ledger torn\"]"
  pass "secondmate records are read through the producer's current, provenance, and freshness fields"
}

test_secondmate_record_decisions() {
  local work="$TMP_ROOT/sm-decisions"
  mkdir -p "$work"
  sm_scenario "$work/status" '.secondmate_current.records[0] |= (.current.state = "captain_decision" | .decisions_open = [{id: "c-ship-1", key: "k1", verb: "needs-decision", summary: "pick an API", reason: null, source: "status"}])'
  bash "$MODEL" --fixture-dir "$work/status" --now "$NOW" > "$work/status.json" 2>/dev/null || fail "status decision must render"
  expect "an open decision not addressed to the captain is Decision pending with no resolver" "$work/status.json" \
    "(($SM | [.classification, .decision.label, .decision.resolver, .decision.key, .decision.summary]) == [\"captain_decision\", \"Decision pending\", null, \"k1\", \"pick an API\"]) and ([.attention[] | select(.key == \"home:/fleet/child\") | .label] == [\"Decision pending\"])"
  sm_scenario "$work/hold" '.secondmate_current.records[0] |= (.current.state = "captain_decision" | .decisions_open = [{id: "c-ship-1", key: "c-ship-1", verb: "captain-hold", summary: "approve scope", reason: null, source: "backlog"}])'
  bash "$MODEL" --fixture-dir "$work/hold" --now "$NOW" > "$work/hold.json" 2>/dev/null || fail "captain hold must render"
  expect "a captain hold is Needs you with the captain as resolver" "$work/hold.json" \
    "(($SM | [.decision.label, .decision.recipient, .decision.resolver]) == [\"Needs you\", \"captain\", \"captain\"]) and ([.attention[] | select(.key == \"home:/fleet/child\") | .label] == [\"Needs you\"])"
  local variant
  for variant in 'captain-held:backlog' 'captain-held:status' 'blocked:backlog' 'captain-hold:status'; do
    sm_scenario "$work/variant" ".secondmate_current.records[0] |= (.current.state = \"captain_decision\" | .decisions_open = [{id: \"c-ship-1\", key: \"k2\", verb: \"${variant%%:*}\", summary: \"unaddressed\", reason: null, source: \"${variant#*:}\"}])"
    bash "$MODEL" --fixture-dir "$work/variant" --now "$NOW" > "$work/variant.json" 2>/dev/null || fail "decision $variant must render"
    expect "a decision with verb ${variant%%:*} and source ${variant#*:} is not a captain hold and stays Decision pending" "$work/variant.json" \
      "($SM | .decision | [.label, .recipient, .resolver]) == [\"Decision pending\", null, null]"
  done
  sm_scenario "$work/worker" '(.tasks[] | select(.id == "p-ship")) |= (.hints.open_decisions = [{key: "k3", verb: "captain-held", summary: "alias"}])'
  bash "$MODEL" --fixture-dir "$work/worker" --now "$NOW" > "$work/worker.json" 2>/dev/null || fail "worker alias scenario must render"
  expect "a worker's status-log decision named captain-held is only Decision pending" "$work/worker.json" \
    '(.entities[] | select(.key == "home:/fleet/parent:p-ship") | .decision | [.label, .recipient, .resolver]) == ["Decision pending", null, null]'
  sm_scenario "$work/worker-hold" '(.tasks[] | select(.id == "p-ship")) |= (.backlog.captain_actionable = true | .backlog.hold_reason = "approve scope")'
  bash "$MODEL" --fixture-dir "$work/worker-hold" --now "$NOW" > "$work/worker-hold.json" 2>/dev/null || fail "worker hold scenario must render"
  expect "a worker whose backlog row is captain-actionable is Needs you with the captain as resolver" "$work/worker-hold.json" \
    '(.entities[] | select(.key == "home:/fleet/parent:p-ship") | .decision | [.label, .recipient, .resolver]) == ["Needs you", "captain", "captain"]'
  pass "secondmate decisions come from the record's decisions_open and only a captain hold reads Needs you"
}

test_spawn_generation_must_be_a_digit_string() {
  local work="$TMP_ROOT/spawn-gen" bad
  mkdir -p "$work"
  for bad in '3' '"x1"' '"-1"' '"1.5"'; do
    sm_scenario "$work/bad" ".tasks[0].spawn_gen = $bad"
    bash "$MODEL" --fixture-dir "$work/bad" --now "$NOW" > "$work/bad.json" 2>/dev/null || fail "spawn_gen $bad must still render a document"
    expect "spawn_gen $bad makes the root malformed and contributes nothing" "$work/bad.json" \
      '(.roots[] | select(.root_id == "/fleet/parent") | [.status, .error]) == ["malformed", "task row with invalid spawn_gen"] and all(.entities[]; .observation.root_id != "/fleet/parent")'
  done
  sm_scenario "$work/padded" '.tasks[0].spawn_gen = "007"'
  bash "$MODEL" --fixture-dir "$work/padded" --now "$NOW" > "$work/padded.json" 2>/dev/null || fail "padded spawn_gen must render"
  expect "a digit string with leading zeros is the integer it spells" "$work/padded.json" \
    '(.entities[] | select(.key == "home:/fleet/child") | .spawn_gen) == 7'
  pass "spawn_gen is accepted only as the producer's digit string and reported as an integer"
}

test_remote_secondmate_home_is_never_read() {
  local work="$TMP_ROOT/remote" out="$TMP_ROOT/remote.json"
  sm_scenario "$work" '
    (.tasks[] | select(.id == "mate-child")) |= (.remote = {host: "ops-box", root: "/home/ops/fm"} | .paths.home = {path: "/home/ops/fm", present: null})
    | .secondmate_current.records[0] |= (.home = "/home/ops/fm" | .host = "ops-box" | .remote = true)'
  jq '.homes["/home/ops/fm"] = {file: "child.json", exit: 0}' "$work/manifest.json" > "$work/m.json" && mv "$work/m.json" "$work/manifest.json"
  bash "$MODEL" --fixture-dir "$work" --now "$NOW" > "$out" 2>/dev/null || fail "remote scenario must render"
  expect "a remote home path is never enumerated as a local root" "$out" \
    '[.roots[].root_id] == ["/fleet/parent"]'
  expect "remote homes are listed as unsupported with their count" "$out" \
    '[.unsupported[] | select(.category == "remote_home") | .reason] == ["1 remote secondmate home(s) are not read: only local homes are observed"]'
  expect "the remote secondmate stays visible as unknown with its recorded state" "$out" \
    '(.entities[] | select(.key == "home:/fleet/parent:mate-child") | [.kind, .classification, .state.raw, .state.detail]) == ["secondmate", "unknown", "working", "remote secondmate home is not observed"]'
  pass "a remote secondmate's home is reported unsupported, not read as a local directory"
}

# The two reason fields in the filter's unsupported[] builder must compile under every jq
# the host offers, because jq 1.7 rejects an unparenthesized binary + inside an object value.
# Each distinct jq version runs the same fixed inputs, and the documents must be identical.
test_reason_fields_render_under_every_host_jq() {
  local remote_work="$TMP_ROOT/jq-remote" versions="" ran=0 candidate version dir out
  sm_scenario "$remote_work" '
    (.tasks[] | select(.id == "mate-child")) |= (.remote = {host: "ops-box", root: "/home/ops/fm"} | .paths.home = {path: "/home/ops/fm", present: null})
    | .secondmate_current.records[0] |= (.home = "/home/ops/fm" | .host = "ops-box" | .remote = true)'
  jq '.homes["/home/ops/fm"] = {file: "child.json", exit: 0}' "$remote_work/manifest.json" > "$remote_work/m.json" && mv "$remote_work/m.json" "$remote_work/manifest.json"
  for candidate in "$(command -v jq)" /usr/bin/jq /opt/homebrew/bin/jq; do
    [ -x "$candidate" ] || continue
    version=$("$candidate" --version 2>/dev/null) || continue
    case "|$versions|" in *"|$version|"*) continue ;; esac
    versions="$versions|$version"
    dir="$TMP_ROOT/jq-host-$ran"
    mkdir -p "$dir" && ln -sf "$candidate" "$dir/jq"
    out="$TMP_ROOT/jq-host-$ran-remote.json"
    PATH="$dir:$PATH" bash "$MODEL" --fixture-dir "$remote_work" --now "$NOW" >"$out" 2>"$out.err" \
      || fail "the model must render its remote-home reason under $version: $(head -c 300 "$out.err")"
    expect "the remote-home reason text renders under $version" "$out" \
      '[.unsupported[] | select(.category == "remote_home") | .reason] == ["1 remote secondmate home(s) are not read: only local homes are observed"]'
    out="$TMP_ROOT/jq-host-$ran-cycle.json"
    PATH="$dir:$PATH" bash "$MODEL" --fixture-dir "$FIXTURES/deep-chain" --now "$NOW" >"$out" 2>"$out.err" \
      || fail "the model must render its ownership-cycle reason under $version: $(head -c 300 "$out.err")"
    expect "the ownership-cycle reason text renders under $version" "$out" \
      '[.unsupported[] | select(.category == "ownership_cycle") | .reason] == ["1 home reference(s) name an ancestor home as a delegate and are not treated as delegation"]'
    ran=$((ran + 1))
  done
  [ "$ran" -gt 0 ] || fail "no jq binary was available to run the reason-field regression"
  local i
  for ((i = 1; i < ran; i++)); do
    cmp -s "$TMP_ROOT/jq-host-0-remote.json" "$TMP_ROOT/jq-host-$i-remote.json" || fail "the remote-home document must be identical across jq versions"
    cmp -s "$TMP_ROOT/jq-host-0-cycle.json" "$TMP_ROOT/jq-host-$i-cycle.json" || fail "the ownership-cycle document must be identical across jq versions"
  done
  pass "the reason fields of the unsupported list render under $ran host jq version(s), with identical documents where more than one is present"
}

test_secondmate_survives_unread_home() {
  local work="$TMP_ROOT/unread" healthy="$TMP_ROOT/unread-healthy.json" out="$TMP_ROOT/unread.json" retained="$TMP_ROOT/unread-retained.json"
  model "$healthy" parent || fail "parent scenario must render"
  expect "a read child home replaces its parent's row" "$healthy" \
    '([.entities[] | select(.key == "home:/fleet/parent:mate-child")] | length) == 0 and ([.entities[] | select(.key == "home:/fleet/child")] | length) == 1'
  sm_scenario "$work" '.' 1
  bash "$MODEL" --fixture-dir "$work" --now "$NOW" > "$out" 2>/dev/null || fail "first-collection child failure must render"
  expect "a child home that fails on its first collection leaves the secondmate visible as unknown" "$out" \
    '(.roots[] | select(.root_id == "/fleet/child") | [.status, .error]) == ["failed", "producer exited 1"] and (.entities[] | select(.key == "home:/fleet/parent:mate-child") | [.kind, .classification, .state.raw, .state.display, .state.detail, .parent_key]) == ["secondmate", "unknown", "working", "Running", "secondmate home not read: producer exited 1", "home:/fleet/parent"]'
  bash "$MODEL" --fixture-dir "$work" --now "$NOW" --previous "$healthy" > "$retained" 2>/dev/null || fail "failure after a good read must render"
  expect "after a good read the child's cached home entity stands in and the row is not duplicated" "$retained" \
    '([.entities[] | select(.key == "home:/fleet/parent:mate-child")] | length) == 0 and ((.entities[] | select(.key == "home:/fleet/child") | [.observation.quality, .observation.stale]) == ["cached", true])'
  pass "a secondmate whose home cannot be read stays visible without a duplicate entity"
}

test_explicit_roots_add_to_the_configured_home() {
  local work="$TMP_ROOT/additive" out="$TMP_ROOT/additive.json"
  sm_scenario "$work" '.'
  jq '.fm_home = "/fleet/other"' "$work/child.json" > "$work/other.json"
  jq '.homes["/fleet/other"] = {file: "other.json", exit: 0}' "$work/manifest.json" > "$work/m.json" && mv "$work/m.json" "$work/manifest.json"
  bash "$MODEL" --fixture-dir "$work" --now "$NOW" --root /fleet/other > "$out" 2>/dev/null || fail "additive roots must render"
  expect "an explicit root is read in addition to the configured home and its descendants" "$out" \
    '([.roots[] | [.root_id, .source]] | sort) == [["/fleet/child", "descendant"], ["/fleet/other", "explicit"], ["/fleet/parent", "configured"]] and ([.roots[].root_id] | length) == 3'
  bash "$MODEL" --fixture-dir "$work" --now "$NOW" --root /fleet/parent > "$out" 2>/dev/null || fail "repeated configured root must render"
  expect "an explicit root naming the configured home is read once and stays configured" "$out" \
    '([.roots[] | [.root_id, .source]] | sort) == [["/fleet/child", "descendant"], ["/fleet/parent", "configured"]]'
  pass "explicit roots add to the configured home and a repeated home is read once"
}

test_empty_project_is_unassigned() {
  local work="$TMP_ROOT/empty-project" out="$TMP_ROOT/empty-project.json"
  sm_scenario "$work" '(.tasks[] | select(.id == "p-ship")) |= (.project = "" | .harness = "  ")'
  bash "$MODEL" --fixture-dir "$work" --now "$NOW" > "$out" 2>/dev/null || fail "empty project must render"
  expect "a task row whose project and harness are empty strings records neither" "$out" \
    '(.entities[] | select(.key == "home:/fleet/parent:p-ship") | [.project, .execution.harness]) == [null, null]'
  expect "an unrecorded project counts under Unassigned and never under an empty heading" "$out" \
    '(.counts.by_project | has("Unassigned") and (has("") | not))'
  pass "an empty project string is Unassigned"
}

test_supervisor_classification_follows_producer_order() {
  local work="$TMP_ROOT/classify" cls
  mkdir -p "$work"
  for cls in 'unknown:(.current_state.state = "unknown")' \
    'captain_decision:(.backlog.captain_actionable = true | .backlog.hold_reason = "approve")' \
    'captain_decision:(.hints.open_decisions = [{key: "k", verb: "needs-decision", summary: "pick"}])'; do
    sm_scenario "$work/c" "(.tasks[] | select(.id == \"p-ship\")) |= (${cls#*:})"
    bash "$MODEL" --fixture-dir "$work/c" --now "$NOW" > "$work/c.json" 2>/dev/null || fail "classification scenario must render"
    expect "a home with ${cls%%:*} work reads ${cls%%:*} even with healthy children beside it" "$work/c.json" \
      "(.entities[] | select(.key == \"home:/fleet/parent\") | .classification) == \"${cls%%:*}\""
  done
  sm_scenario "$work/mixed" '(.tasks[] | select(.id == "p-ship")) |= (.current_state.state = "unknown" | .backlog.captain_actionable = true)'
  bash "$MODEL" --fixture-dir "$work/mixed" --now "$NOW" > "$work/mixed.json" 2>/dev/null || fail "mixed scenario must render"
  expect "an unknown child outranks a captain decision, as the producer ranks them" "$work/mixed.json" \
    '(.entities[] | select(.key == "home:/fleet/parent") | .classification) == "unknown"'
  pass "supervisor classification follows the producer's order"
}

test_decisions_are_deduplicated_and_all_kept() {
  local work="$TMP_ROOT/decisions" out="$TMP_ROOT/decisions.json"
  sm_scenario "$work" '
    .secondmate_current.records[0] |= (.current.state = "captain_decision" | .decisions_open = [
      {id: "c-ship-1", key: "c-ship-1", verb: "captain-hold", summary: "approve scope", reason: null, source: "backlog"},
      {id: "queued-x", key: "queued-x", verb: "captain-hold", summary: "queued hold", reason: null, source: "backlog"},
      {id: "c-ship-1", key: "k-status", verb: "needs-decision", summary: "pick one", reason: null, source: "status"}])' 0 '
    (.tasks[] | select(.id == "c-ship-1")) |= (.backlog.captain_actionable = true | .backlog.hold_reason = "approve scope"
      | .hints.open_decisions = [{key: "k-status", verb: "needs-decision", summary: "pick one"}])'
  bash "$MODEL" --fixture-dir "$work" --now "$NOW" > "$out" 2>/dev/null || fail "decision scenario must render"
  expect "the same hold and decision reported by the home and by its task appear once in attention, owned by the task" "$out" \
    '[.attention[] | select(.class == "decision") | [.key, .label, .reason]] | sort == [["home:/fleet/child", "Needs you", "queued hold"], ["home:/fleet/child:c-ship-1", "Decision pending", "pick one"], ["home:/fleet/child:c-ship-1", "Needs you", "approve scope"]]'
  expect "every distinct outstanding decision stays on its entity" "$out" \
    '((.entities[] | select(.key == "home:/fleet/child") | .decisions | length) == 3) and ((.entities[] | select(.key == "home:/fleet/child:c-ship-1") | .decisions | map(.label) | sort) == ["Decision pending", "Needs you"]) and ((.entities[] | select(.key == "home:/fleet/child") | .decision.label) == "Needs you")'
  pass "one decision seen through several paths is listed once and distinct decisions are all kept"
}

test_live_producer_shapes() {
  local parent="$TMP_ROOT/live-parent" child="$TMP_ROOT/live-child" out="$TMP_ROOT/live-spawned.json" pc cc code=0
  mkdir -p "$parent/state" "$parent/data" "$parent/projects" "$parent/config" "$parent/wt" \
    "$child/state" "$child/data" "$child/projects" "$child/config"
  ln -s "$child" "$parent/child-link"
  pc=$(cd "$parent" && pwd -P)
  cc=$(cd "$child" && pwd -P)
  fm_write_meta "$parent/state/ship-a.meta" \
    "window=firstmate:fm-ship-a" "worktree=$parent/wt" "project=alpha" "harness=claude" \
    "kind=ship" "mode=ship" "yolo=off" "spawn_gen=3"
  printf 'working: building\n' > "$parent/state/ship-a.status"
  fm_write_meta "$parent/state/mate.meta" \
    "window=firstmate:fm-mate" "worktree=$child" "project=$child" "harness=codex" \
    "kind=secondmate" "mode=secondmate" "home=$parent/child-link" "projects=alpha, beta" "spawn_gen=2"
  printf 'working: watching\n' > "$parent/state/mate.status"
  FM_HOME="$parent" bash "$MODEL" --now "$NOW" >"$out" 2>"$out.err" || code=$?
  expect_code 0 "$code" "a live read of a home with a spawned task and a secondmate must render"
  expect "the producer's string spawn_gen is accepted and reported as an integer, and both homes are ok roots" "$out" \
    "([.roots[] | [.root_id, .status]] == [[\"$pc\", \"ok\"], [\"$cc\", \"ok\"]]) and ((.entities[] | select(.key == \"home:$pc:ship-a\") | .spawn_gen) == 3)"
  expect "a secondmate listed through a symlinked path is the one entity for the home that was read" "$out" \
    "([.entities[] | select(.kind == \"secondmate\")] | length) == 1 and (.entities[] | select(.key == \"home:$cc\") | [.kind, .spawn_gen, .parent_key, .contextual_in]) == [\"secondmate\", 2, \"home:$pc\", [\"project:beta\"]]"
  expect "the real record's freshness and reason reach the secondmate entity" "$out" \
    "(.entities[] | select(.key == \"home:$cc\") | [.observation.quality, (.state.detail | type)]) == [\"historical-event\", \"string\"]"
  pass "the real producer's spawn_gen, record, and symlinked home paths aggregate correctly"
}

test_live_empty_home_smoke() {
  local home="$TMP_ROOT/live-home" extra="$TMP_ROOT/live-extra" out="$TMP_ROOT/live.json" canon extra_canon code=0
  mkdir -p "$home" "$extra"
  canon=$(cd "$home" && pwd -P)
  extra_canon=$(cd "$extra" && pwd -P)
  FM_HOME="$home" bash "$MODEL" --root "$extra" --root "$home" --now "$NOW" >"$out" 2>"$out.err" || code=$?
  expect_code 0 "$code" "a live read of empty homes must render"
  expect "the configured home is read first, an explicit root is added, and a repeated home is read once" "$out" \
    "([.roots[] | [.root_id, .source, .status]]) == [[\"$canon\", \"configured\", \"ok\"], [\"$extra_canon\", \"explicit\", \"ok\"]]"
  expect "each empty home has one supervisor with no active work" "$out" \
    '(.entities | length) == 2 and all(.entities[]; .classification == "no_active_work") and (.counts.unique_entities == 2)'
  expect "native subagents stay listed as unobserved in the live path too" "$out" \
    'any(.unsupported[]; .category == "native_subagents")'
  pass "live empty homes render through the real producer with the configured home plus explicit roots"
}

# ac5_tmux_cleanup: stop the private tmux server the identity test started.
ac5_tmux_cleanup() {
  "$AC5_TMUX" -L "$AC5_SOCKET" kill-server >/dev/null 2>&1 || true
}

# test_tmux_identity_is_exact: a recorded tmux window is observed only when its exact
# name exists in its exact session. A missing window must not resolve to a prefix
# neighbor, and a dotted recorded target must not read a neighbor window. Every tmux
# call the real producer makes is forced onto a private server.
test_tmux_identity_is_exact() {
  local home="$TMP_ROOT/ac5-home" bin="$TMP_ROOT/ac5-bin" socket="fm-birds-eye-ac5-$$"
  local real_tmux out="$TMP_ROOT/ac5.json" code=0 canon before after
  real_tmux=$(command -v tmux 2>/dev/null) || { echo "skip: tmux not found"; return 0; }
  mkdir -p "$bin" "$home/state" "$home/data" "$home/projects" "$home/config"
  canon=$(cd "$home" && pwd -P)
  cat > "$bin/tmux" <<SH
#!/usr/bin/env bash
exec "$real_tmux" -L "$socket" "\$@"
SH
  chmod +x "$bin/tmux"
  printf '#!/usr/bin/env bash\nexit 0\n' > "$bin/no-mistakes"
  chmod +x "$bin/no-mistakes"
  AC5_TMUX=$real_tmux AC5_SOCKET=$socket
  trap 'ac5_tmux_cleanup; fm_test_cleanup' EXIT
  "$real_tmux" -L "$socket" new-session -d -s sx -n fm-task-a2 "sleep 600" || fail "private tmux server must start"
  "$real_tmux" -L "$socket" new-window -d -t sx -n fm-task-b "sleep 600"
  "$real_tmux" -L "$socket" new-window -d -t sx -n fm-dot-ab "sleep 600"
  "$real_tmux" -L "$socket" new-window -d -t sx -n fm-dot-cc "sleep 600"
  local id target
  for pair in ship-missing:sx:fm-task-a ship-exact:sx:fm-task-b ship-dot-missing:sx:fm-dot-a.0 ship-dot-neighbor:sx:fm-dot-c.0; do
    id=${pair%%:*}
    target=${pair#*:}
    fm_write_meta "$home/state/$id.meta" "window=$target" "worktree=$home" "project=alpha" \
      "harness=claude" "kind=ship" "mode=ship" "yolo=off" "spawn_gen=1"
    printf 'working: building\n' > "$home/state/$id.status"
  done
  before=$("$real_tmux" -L "$socket" list-windows -a -F '#{session_name}:#{window_name}')
  PATH="$bin:$PATH" FM_HOME="$home" bash "$MODEL" --now "$NOW" >"$out" 2>"$out.err" || code=$?
  after=$("$real_tmux" -L "$socket" list-windows -a -F '#{session_name}:#{window_name}')
  expect_code 0 "$code" "a live read with recorded tmux windows must render"
  expect "a recorded window that is missing does not resolve to its prefix neighbor" "$out" \
    "(.entities[] | select(.key == \"home:$canon:ship-missing\") | .artifacts.output_available) == false"
  expect "a recorded window that exists exactly is observed" "$out" \
    "(.entities[] | select(.key == \"home:$canon:ship-exact\") | .artifacts.output_available) == true"
  expect "a dotted recorded target whose window is missing does not read a neighbor pane" "$out" \
    "(.entities[] | select(.key == \"home:$canon:ship-dot-missing\") | .artifacts.output_available) == false"
  expect "a dotted recorded target with a neighbor window does not read that neighbor" "$out" \
    "(.entities[] | select(.key == \"home:$canon:ship-dot-neighbor\") | .artifacts.output_available) == false"
  [ "$before" = "$after" ] || fail "observing tmux identity must not change the window inventory"
  pass "the producer reads recorded tmux windows by exact name and never through a prefix neighbor"
}

test_usage_and_argument_errors
test_default_run_contract
test_entity_fields_match_the_contract
test_overlap_ownership_and_unique_counts
test_failed_root_keeps_last_good_and_recovers
test_stale_output_is_rejected
test_staleness_boundaries_and_refresh_floor
test_timestamp_and_state_faults
test_generation_rollover
test_requested_and_observed_execution_stay_distinct
test_attention_routing
test_bad_roots_are_isolated
test_unreached_descendant_keeps_last_good
test_fleet_scale_chain_and_depth
test_deep_chain_cycle_and_unreached
test_root_cap_is_disclosed
test_history_eviction_keeps_protected_context
test_observed_state_duration_and_transitions
test_authoritative_state_age_outranks_observed_duration
test_silent_running_task_is_not_reclassified
test_root_failures_age_independently_under_the_fake_clock
test_secondmate_record_uses_producer_shape
test_secondmate_record_decisions
test_spawn_generation_must_be_a_digit_string
test_remote_secondmate_home_is_never_read
test_reason_fields_render_under_every_host_jq
test_secondmate_survives_unread_home
test_explicit_roots_add_to_the_configured_home
test_empty_project_is_unassigned
test_supervisor_classification_follows_producer_order
test_decisions_are_deduplicated_and_all_kept
test_live_producer_shapes
test_live_empty_home_smoke
test_tmux_identity_is_exact
