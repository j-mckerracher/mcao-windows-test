#!/usr/bin/env bash
# Behavior tests for fm-spawn.sh batch dispatch (`id=repo` pairs).
#
# These exercise argument routing only: each spawn attempt fails fast at the
# missing-brief check, which is reached before any tmux/treehouse side effect, so
# the tests create no windows or worktrees. FM_SPAWN_NO_GUARD=1 keeps them off the
# live watcher guard / state. Parser and path-scoping cases are table-driven; the
# only behavior asserted on its own is "a multi-pair batch does not stop after the
# first failure". The cloud (execution target gcp) cases run in isolated homes
# against a logging `mcao` stub and terminal tools that fail if invoked.
set -u

# shellcheck source=tests/lib.sh
. "$(dirname "${BASH_SOURCE[0]}")/lib.sh"
# shellcheck source=tests/fixtures.sh
. "$(dirname "${BASH_SOURCE[0]}")/fixtures.sh"

SPAWN="$ROOT/bin/fm-spawn.sh"
TMP_ROOT=$(fm_test_tmproot fm-spawn-batch)
export FM_BACKEND=tmux
# The launch gate reads the workflow-agent records under HOME
# (bin/fm-workflow-agent-lib.sh). Every spawn here runs under a seeded throwaway
# HOME, so routing cases never depend on the developer's installed plugins.
export HOME="$TMP_ROOT/user-home"
export CODEX_HOME='' COPILOT_HOME='' PI_CODING_AGENT_DIR='' CLAUDE_CONFIG_DIR=''
fm_test_seed_workflow_agents "$HOME"

# Clear ambient firstmate overrides so the behavior test owns its environment.
run_spawn() {
  FM_ROOT_OVERRIDE='' \
    FM_HOME='' \
    FM_STATE_OVERRIDE='' \
    FM_DATA_OVERRIDE='' \
    FM_PROJECTS_OVERRIDE='' \
    FM_CONFIG_OVERRIDE='' \
    FM_SPAWN_NO_GUARD=1 \
    "$SPAWN" "$@" 2>&1
}

# Ship spawns carry an explicit delivery contract (AGENTS.md section 7); the
# batch path takes one shared pair of flags for every pair.
run_ship_spawn() {
  run_spawn "$@" --mode no-mistakes --yolo off
}

# Every pair in a batch is dispatched even though the first one fails; the loop
# must not stop early. This is the load-bearing batch guarantee, kept explicit.
test_batch_dispatches_every_pair() {
  local out status
  out=$(run_ship_spawn nope-batch-a-z1=projects/none-a nope-batch-b-z2=projects/none-b)
  status=$?
  [ "$status" -ne 0 ] || fail "batch with missing briefs should exit non-zero"
  printf '%s\n' "$out" | grep -F 'batch: FAILED to spawn nope-batch-a-z1 (projects/none-a)' >/dev/null \
    || fail "first pair was not dispatched/reported"
  printf '%s\n' "$out" | grep -F 'batch: FAILED to spawn nope-batch-b-z2 (projects/none-b)' >/dev/null \
    || fail "second pair was not dispatched/reported (loop stopped early?)"
  pass "batch dispatch re-execs and reports every id=repo pair"
}

# Boundary cases for batch detection. Each row:
#   <label>|<batch yes/no>|<expect substring>|<args>
# batch=yes -> a 'batch:' line must appear; batch=no -> it must not.
test_batch_mode_boundaries() {
  local label batch expect args out status
  while IFS='|' read -r label batch expect args; do
    [ -n "$label" ] || continue
    # shellcheck disable=SC2086  # args is an intentional word-split arg list
    out=$(run_ship_spawn $args)
    status=$?
    [ "$status" -ne 0 ] || fail "$label: expected non-zero exit"
    if [ -n "$expect" ]; then
      printf '%s\n' "$out" | grep -F "$expect" >/dev/null || fail "$label: missing '$expect'"
    fi
    case "$batch" in
      yes) printf '%s\n' "$out" | grep -F 'batch:' >/dev/null || fail "$label: did not enter batch dispatch" ;;
      no)  printf '%s\n' "$out" | grep -F 'batch:' >/dev/null && fail "$label: wrongly entered batch dispatch" ;;
    esac
  done <<'ROWS'
single id=repo pair routes through batch|yes|batch: FAILED to spawn nope-batch-solo-z3 (projects/none-solo)|nope-batch-solo-z3=projects/none-solo
non-pair arg in batch is rejected|yes|batch dispatch expects every argument as id=repo; got 'bogus-no-equals'|nope-batch-mix-z5=projects/none-mix bogus-no-equals
plain '<id> <repo>' is single-task|no||nope-single-z4 projects/none-single
id part containing '/' is not a pair|no||weird/id-z6=projects/none projects/none
ROWS
  pass "batch detection: single pair batches, non-pair rejected, single-task and slash-id stay single"
}

# A projects/ path is resolved through the firstmate home, never the caller cwd,
# before the missing-brief check. One row per home-scoping override.
test_projects_path_scoping() {
  local label use_override id home projects out status expected
  while IFS='|' read -r label use_override id; do
    [ -n "$label" ] || continue
    home="$TMP_ROOT/$id home"
    projects="$TMP_ROOT/$id projects"
    mkdir -p "$home/data" "$projects/alpha"
    git -C "$projects/alpha" init -q || fail "$label: could not initialize project fixture"
    if [ "$use_override" = yes ]; then
      out=$(FM_ROOT_OVERRIDE='' FM_STATE_OVERRIDE='' FM_DATA_OVERRIDE='' FM_CONFIG_OVERRIDE='' \
        FM_HOME="$home" FM_PROJECTS_OVERRIDE="$projects" FM_SPAWN_NO_GUARD=1 \
        "$SPAWN" "$id" projects/alpha codex --mode no-mistakes --yolo off 2>&1)
    else
      mkdir -p "$home/projects/alpha"
      git -C "$home/projects/alpha" init -q || fail "$label: could not initialize home project fixture"
      out=$(FM_ROOT_OVERRIDE='' FM_STATE_OVERRIDE='' FM_DATA_OVERRIDE='' FM_PROJECTS_OVERRIDE='' FM_CONFIG_OVERRIDE='' \
        FM_HOME="$home" FM_SPAWN_NO_GUARD=1 \
        "$SPAWN" "$id" projects/alpha codex --mode no-mistakes --yolo off 2>&1)
    fi
    status=$?
    [ "$status" -ne 0 ] || fail "$label: spawn with missing brief should fail"
    expected="error: task $id has no brief at inaccessible data path $home/data/$id/brief.md"
    printf '%s\n' "$out" | grep -F "$expected" >/dev/null \
      || fail "$label: projects/alpha was not resolved through the home before the brief check"
    printf '%s\n' "$out" | grep -F 'cd: projects/alpha' >/dev/null \
      && fail "$label: spawn resolved projects/alpha from the caller cwd"
  done <<'ROWS'
FM_HOME scopes projects/|no|nope-home-z7
FM_PROJECTS_OVERRIDE scopes projects/|yes|nope-override-z8
ROWS
  pass "projects/ paths are scoped through the firstmate home for single-task spawn"
}

# A ship batch carries one shared delivery contract. Missing flags must stop the
# whole batch before any pair is dispatched, so a batch can never launch workers
# whose delivery posture was never decided.
test_batch_requires_the_shared_delivery_contract() {
  local out status
  out=$(run_spawn nope-batch-nomode-z9=projects/none-a nope-batch-nomode-z10=projects/none-b)
  status=$?
  [ "$status" -ne 0 ] || fail "a ship batch without --mode should exit non-zero"
  printf '%s\n' "$out" | grep -F 'ship spawns require --mode' >/dev/null \
    || fail "batch refusal did not name the missing delivery mode"
  printf '%s\n' "$out" | grep -F 'batch:' >/dev/null \
    && fail "batch dispatched pairs despite an undecided delivery contract"

  out=$(run_spawn nope-batch-noyolo-z11=projects/none-a --mode direct-PR)
  status=$?
  [ "$status" -ne 0 ] || fail "a ship batch without --yolo should exit non-zero"
  printf '%s\n' "$out" | grep -F 'ship spawns require --yolo' >/dev/null \
    || fail "batch refusal did not name the missing merge posture"
  pass "batch dispatch requires the shared ship delivery contract before any pair runs"
}

# A scout batch has no delivery contract to share, so the flags are refused rather
# than accepted and ignored.
test_scout_batch_refuses_delivery_flags() {
  local out status
  out=$(run_spawn nope-batch-scout-z12=projects/none-a --scout --mode direct-PR --yolo on)
  status=$?
  [ "$status" -ne 0 ] || fail "a scout batch carrying delivery flags should exit non-zero"
  printf '%s\n' "$out" | grep -F 'applies only to ship spawns' >/dev/null \
    || fail "scout batch did not refuse the delivery flags"
  pass "scout batch refuses ship delivery flags instead of ignoring them"
}

# Cloud (execution target gcp) scout spawns. A valid spawn publishes one record
# and submits exactly once through the cloud client. A refused or failed spawn
# reaches neither the client nor a terminal. Every case runs in its own isolated
# home, never this checkout's state/.
CLOUD_BIN="$TMP_ROOT/cloud-bin"
CLOUD_LOG="$TMP_ROOT/cloud-mcao.log"
TERMINAL_LOG="$TMP_ROOT/terminal-invocations.log"
mkdir -p "$CLOUD_BIN"
for tool in tmux herdr treehouse; do
  cat > "$CLOUD_BIN/$tool" <<'SH'
#!/usr/bin/env bash
printf '%s %s\n' "$(basename "$0")" "$*" >> "$FM_FAKE_TERMINAL_LOG"
exit 99
SH
  chmod +x "$CLOUD_BIN/$tool"
done
cat > "$CLOUD_BIN/mcao" <<'SH'
#!/usr/bin/env bash
printf '%s\n' "$*" >> "$FM_FAKE_MCAO_LOG"
[ "${FM_FAKE_SUBMIT_RC:-0}" -eq 0 ] || exit "$FM_FAKE_SUBMIT_RC"
printf '%s\n' "${FM_FAKE_SUBMIT_OUTPUT-run-cloud-1}"
SH
chmod +x "$CLOUD_BIN/mcao"

reset_cloud_logs() {
  : > "$CLOUD_LOG"
  : > "$TERMINAL_LOG"
}

# new_cloud_case <name>: an isolated home whose projects/alpha is a git repo with
# one commit, plus a task file. Echoes the home path.
new_cloud_case() {
  local home="$TMP_ROOT/cloud-$1"
  fm_test_spawn_home "$home"
  mkdir -p "$home/projects/alpha"
  git -C "$home/projects/alpha" init -q -b main || fail "$1: could not initialize the project fixture"
  git -C "$home/projects/alpha" -c user.name=test -c user.email=test@example.invalid \
    -c commit.gpgsign=false commit -q --allow-empty -m init || fail "$1: could not commit the source fixture"
  printf '# Task\nScout the cloud path.\n' > "$home/task.md"
  printf '%s\n' "$home"
}

# run_cloud_spawn <home> <spawn-args...>: spawn against <home> with the logging
# stubs first on PATH.
run_cloud_spawn() {
  local home=$1
  shift
  PATH="$CLOUD_BIN:$PATH" \
    FM_ROOT_OVERRIDE='' \
    FM_HOME="$home" \
    FM_STATE_OVERRIDE='' \
    FM_DATA_OVERRIDE='' \
    FM_PROJECTS_OVERRIDE='' \
    FM_CONFIG_OVERRIDE='' \
    FM_SPAWN_NO_GUARD=1 \
    FM_FAKE_MCAO_LOG="$CLOUD_LOG" \
    FM_FAKE_TERMINAL_LOG="$TERMINAL_LOG" \
    "$SPAWN" "$@" 2>&1
}

# set_valid_cloud_args <home> <id>: the complete gcp scout request, in CLOUD_ARGS.
set_valid_cloud_args() {
  CLOUD_ARGS=("$2" projects/alpha --scout --harness pi --execution-target gcp
    --task "$1/task.md" --ref "$(git -C "$1/projects/alpha" rev-parse HEAD)"
    --model gemini-3.8-flash --profile pi-standard)
}

# spawn_valid_cloud <home> <id>: spawn the complete gcp scout request.
spawn_valid_cloud() {
  set_valid_cloud_args "$1" "$2"
  run_cloud_spawn "$1" "${CLOUD_ARGS[@]}"
}

# expect_cloud_refusal <label> <expected-text> <home> <spawn-args...>: the spawn
# must refuse, name <expected-text>, and reach neither the cloud client nor a
# terminal.
expect_cloud_refusal() {
  local label=$1 expect=$2 home=$3 out status
  shift 3
  reset_cloud_logs
  out=$(run_cloud_spawn "$home" "$@")
  status=$?
  [ "$status" -ne 0 ] || fail "$label: expected a refusal, got success: $out"
  printf '%s\n' "$out" | grep -F -e "$expect" >/dev/null \
    || fail "$label: refusal did not name '$expect': $out"
  [ ! -s "$CLOUD_LOG" ] || fail "$label: a refused request reached the cloud client: $(cat "$CLOUD_LOG")"
  [ ! -s "$TERMINAL_LOG" ] || fail "$label: a refused request reached a terminal: $(cat "$TERMINAL_LOG")"
  pass "$label"
}

# A valid gcp scout publishes one record carrying the cloud reference, submits
# once with the contract argv and a correlation id, and touches no terminal, pane,
# or worktree.
test_cloud_scout_spawn_submits_once_and_records_the_run() {
  local home id=cloud-valid-z20 meta out status proj_real ref rid
  home=$(new_cloud_case valid)
  proj_real=$(cd "$home/projects/alpha" && pwd -P)
  ref=$(git -C "$home/projects/alpha" rev-parse HEAD)
  meta="$home/state/$id.meta"
  reset_cloud_logs
  out=$(spawn_valid_cloud "$home" "$id")
  status=$?
  [ "$status" -eq 0 ] || fail "a valid gcp scout spawn should succeed, got $status: $out"
  printf '%s\n' "$out" | grep -F "spawned scout $id on execution target gcp (cloud run run-cloud-1)" >/dev/null \
    || fail "spawn did not report the cloud run: $out"
  [ "$(wc -l < "$CLOUD_LOG" | tr -d ' ')" -eq 1 ] \
    || fail "the cloud client must run exactly once, got: $(cat "$CLOUD_LOG")"
  [ ! -s "$TERMINAL_LOG" ] || fail "a gcp spawn reached a terminal: $(cat "$TERMINAL_LOG")"
  [ "$(git -C "$home/projects/alpha" worktree list --porcelain | grep -c '^worktree ')" -eq 1 ] \
    || fail "a gcp spawn created a worktree"
  rid=$(sed -n 's/^cloud_request_id=//p' "$meta")
  case "$rid" in
    ????????-????-????-????-????????????) ;;
    *) fail "the request id is not a UUID: '$rid'" ;;
  esac
  [ "$(cat "$CLOUD_LOG")" = "cloud submit --repo $proj_real --ref $ref --task $home/task.md --agent pi --model gemini-3.8-flash --profile pi-standard --request-id $rid" ] \
    || fail "submit argv differs from the contract: $(cat "$CLOUD_LOG")"
  grep -qx 'execution_target=gcp' "$meta" || fail "record does not declare execution_target=gcp"
  grep -qx 'cloud_run_id=run-cloud-1' "$meta" || fail "record does not carry the cloud run id"
  if grep -E '^(backend|window|worktree|pane|wt|remote_host)=' "$meta" >/dev/null; then
    fail "a gcp record carries terminal or worktree fields: $(cat "$meta")"
  fi
  pass "a valid gcp scout spawn submits once with correlation and touches no terminal or worktree"
}

# The laptop refuses a model or profile that is off the pilot allowlist before any
# record, submit, or terminal exists. The cloud server allowlist remains the authority.
test_cloud_spawn_refuses_unapproved_model_and_profile() {
  local home ref id=cloud-allow-q7 t
  home=$(new_cloud_case allowlist)
  ref=$(git -C "$home/projects/alpha" rev-parse HEAD)
  t="$home/task.md"
  expect_cloud_refusal "a gcp request naming a model off the approved allowlist is refused" \
    "--model 'claude-haiku-5-5' is not on the approved cloud model allowlist" "$home" \
    "$id" projects/alpha --scout --harness pi --execution-target gcp --task "$t" --ref "$ref" --model claude-haiku-5-5 --profile pi-standard
  expect_cloud_refusal "a gcp request naming a profile off the pilot allowlist is refused" \
    "--profile 'pi-large' is not on the approved cloud profile allowlist" "$home" \
    "$id" projects/alpha --scout --harness pi --execution-target gcp --task "$t" --ref "$ref" --model gemini-3.8-flash --profile pi-large
  [ ! -e "$home/state/$id.meta" ] || fail "a refused allowlist request must write no record"
  pass "an unapproved gcp model or profile is refused before any record, submit, or terminal"
}

# Every refused request names its reason and reaches neither the client nor a
# terminal. A refusal that names an incomplete or invalid reference never
# defaults to a local terminal either.
test_cloud_spawn_refusals_reach_no_client_or_terminal() {
  local home ref id=cloud-refuse-z21 t
  home=$(new_cloud_case refusals)
  ref=$(git -C "$home/projects/alpha" rev-parse HEAD)
  t="$home/task.md"
  expect_cloud_refusal "an unknown execution target is refused, never defaulted" \
    "--execution-target must be local or gcp (got 'ssh')" "$home" \
    "$id" projects/alpha --scout --harness pi --execution-target ssh --task "$t" --ref "$ref" --model gemini-3.8-flash --profile pi-standard
  expect_cloud_refusal "a gcp request with --relaunch is refused" \
    "--relaunch is not supported on execution target gcp" "$home" \
    --relaunch "$id" --harness pi --execution-target gcp --task "$t" --ref "$ref" --model gemini-3.8-flash --profile pi-standard
  expect_cloud_refusal "a gcp request without --scout is refused, since a cloud run cannot enforce a ship's delivery mode" \
    "gcp runs scouts only" "$home" \
    "$id" projects/alpha --harness pi --execution-target gcp --mode no-mistakes --yolo off --task "$t" --ref "$ref" --model gemini-3.8-flash --profile pi-standard
  expect_cloud_refusal "a gcp request naming a terminal backend is refused" \
    "--backend applies to local terminal spawns" "$home" \
    "$id" projects/alpha --scout --harness pi --execution-target gcp --backend tmux --task "$t" --ref "$ref" --model gemini-3.8-flash --profile pi-standard
  expect_cloud_refusal "a gcp request naming a base branch is refused" \
    "--base-branch does not apply to execution target gcp" "$home" \
    "$id" projects/alpha --scout --harness pi --execution-target gcp --base-branch main --task "$t" --ref "$ref" --model gemini-3.8-flash --profile pi-standard
  expect_cloud_refusal "a gcp request with a launch command is refused" \
    "a gcp scout takes exactly <task-id> <project-dir>" "$home" \
    "$id" projects/alpha extra-arg --scout --harness pi --execution-target gcp --task "$t" --ref "$ref" --model gemini-3.8-flash --profile pi-standard
  expect_cloud_refusal "a gcp request with a non-pi harness is refused" \
    "requires --harness pi" "$home" \
    "$id" projects/alpha --scout --harness codex --execution-target gcp --task "$t" --ref "$ref" --model gemini-3.8-flash --profile pi-standard
  expect_cloud_refusal "a gcp request without --task is refused" \
    "requires --task FILE" "$home" \
    "$id" projects/alpha --scout --harness pi --execution-target gcp --ref "$ref" --model gemini-3.8-flash --profile pi-standard
  expect_cloud_refusal "a gcp request without --ref is refused" \
    "requires --ref <commit>" "$home" \
    "$id" projects/alpha --scout --harness pi --execution-target gcp --task "$t" --model gemini-3.8-flash --profile pi-standard
  expect_cloud_refusal "a gcp request without --model is refused" \
    "requires --model" "$home" \
    "$id" projects/alpha --scout --harness pi --execution-target gcp --task "$t" --ref "$ref" --profile pi-standard
  expect_cloud_refusal "a gcp request without --profile is refused" \
    "requires --profile" "$home" \
    "$id" projects/alpha --scout --harness pi --execution-target gcp --task "$t" --ref "$ref" --model gemini-3.8-flash
  expect_cloud_refusal "a gcp request with an unreadable task file is refused" \
    "is not a readable file" "$home" \
    "$id" projects/alpha --scout --harness pi --execution-target gcp --task "$home/absent.md" --ref "$ref" --model gemini-3.8-flash --profile pi-standard
  expect_cloud_refusal "a gcp request with a malformed profile is refused" \
    "--profile must be a plain profile name" "$home" \
    "$id" projects/alpha --scout --harness pi --execution-target gcp --task "$t" --ref "$ref" --model gemini-3.8-flash --profile 'bad/name'
  expect_cloud_refusal "a gcp request whose ref names no commit is refused before any record" \
    "does not name a commit" "$home" \
    "$id" projects/alpha --scout --harness pi --execution-target gcp --task "$t" --ref 0000000000000000000000000000000000000000 --model gemini-3.8-flash --profile pi-standard
  expect_cloud_refusal "a local request carrying gcp-only flags is refused" \
    "apply only to --execution-target gcp" "$home" \
    "$id" projects/alpha --scout --harness pi --execution-target local --task "$t"
  [ ! -e "$home/state/$id.meta" ] || fail "a refused cloud request left a record behind"
  pass "every refused cloud request names its reason and leaves no record"
}

# A cloud spawn never replaces an existing record: a local record for the id is
# left byte-identical, and an existing gcp record refuses both a fresh spawn and a
# relaunch by target name.
test_cloud_spawn_never_overwrites_an_existing_record() {
  local home local_meta gcp_meta before after
  home=$(new_cloud_case overwrite)
  local_meta="$home/state/cloud-local-z22.meta"
  gcp_meta="$home/state/cloud-gcp-z23.meta"
  printf 'id=cloud-local-z22\nkind=scout\nbackend=tmux\nwindow=firstmate:fm-cloud-local-z22\n' > "$local_meta"
  before=$(cat "$local_meta")
  reset_cloud_logs
  if spawn_valid_cloud "$home" cloud-local-z22 >/dev/null; then
    fail "a cloud spawn over a local record should be refused"
  fi
  after=$(cat "$local_meta")
  [ "$before" = "$after" ] || fail "a cloud spawn overwrote a local record"
  [ ! -s "$CLOUD_LOG" ] || fail "a cloud spawn over a local record reached the cloud client"

  printf 'id=cloud-gcp-z23\nkind=scout\nexecution_target=gcp\ncloud_request_id=req-old\ncloud_run_id=run-old\n' > "$gcp_meta"
  before=$(cat "$gcp_meta")
  expect_cloud_refusal "a fresh spawn cannot replace an existing gcp record" \
    "already has an execution target gcp record" "$home" \
    cloud-gcp-z23 projects/alpha --scout --harness pi --execution-target gcp \
    --task "$home/task.md" --ref "$(git -C "$home/projects/alpha" rev-parse HEAD)" --model gemini-3.8-flash --profile pi-standard
  expect_cloud_refusal "a relaunch of an existing gcp task is refused" \
    "already has an execution target gcp record" "$home" \
    --relaunch cloud-gcp-z23
  after=$(cat "$gcp_meta")
  [ "$before" = "$after" ] || fail "a refused spawn or relaunch changed an existing gcp record"
  pass "a cloud spawn never overwrites an existing record of either target"
}

# A failed submit keeps its request-id record with no run id, is never retried,
# and blocks a respawn of the same id.
test_cloud_submit_failure_keeps_the_request_record_and_never_retries() {
  local home id=cloud-fail-z24 meta out status
  home=$(new_cloud_case submit-fail)
  meta="$home/state/$id.meta"
  reset_cloud_logs
  export FM_FAKE_SUBMIT_RC=5
  out=$(spawn_valid_cloud "$home" "$id")
  status=$?
  unset FM_FAKE_SUBMIT_RC
  [ "$status" -ne 0 ] || fail "a failed submit must exit non-zero: $out"
  printf '%s\n' "$out" | grep -F 'cloud submit failed' >/dev/null \
    || fail "a failed submit did not say so: $out"
  printf '%s\n' "$out" | grep -F 'nothing was retried' >/dev/null \
    || fail "a failed submit did not say nothing was retried: $out"
  [ "$(wc -l < "$CLOUD_LOG" | tr -d ' ')" -eq 1 ] || fail "a failed submit must run once, got: $(cat "$CLOUD_LOG")"
  [ -s "$meta" ] || fail "the request record must survive a failed submit"
  grep -q '^cloud_request_id=' "$meta" || fail "the failed-submit record lost its request id"
  grep -q '^cloud_run_id=' "$meta" && fail "a failed submit recorded a run id"
  [ ! -s "$TERMINAL_LOG" ] || fail "a failed gcp submit reached a terminal"

  out=$(spawn_valid_cloud "$home" "$id")
  status=$?
  [ "$status" -ne 0 ] || fail "a respawn over a failed-submit record must be refused"
  printf '%s\n' "$out" | grep -F 'already has an execution target gcp record' >/dev/null \
    || fail "a respawn refusal did not name the existing gcp record: $out"
  [ "$(wc -l < "$CLOUD_LOG" | tr -d ' ')" -eq 1 ] || fail "a refused respawn reached the cloud client"
  pass "a failed submit keeps its request record, is never retried, and blocks a respawn"
}

# A submit that prints no single run id keeps its request record for reconciliation.
test_cloud_submit_without_one_run_id_keeps_the_request_record() {
  local home id=cloud-norun-z26 meta out status
  home=$(new_cloud_case no-run)
  meta="$home/state/$id.meta"
  reset_cloud_logs
  export FM_FAKE_SUBMIT_OUTPUT=''
  out=$(spawn_valid_cloud "$home" "$id")
  status=$?
  unset FM_FAKE_SUBMIT_OUTPUT
  [ "$status" -ne 0 ] || fail "a submit with no run id must exit non-zero: $out"
  printf '%s\n' "$out" | grep -F 'returned no single run id' >/dev/null \
    || fail "a submit with no run id did not say so: $out"
  grep -q '^cloud_request_id=' "$meta" || fail "the no-run-id record lost its request id"
  grep -q '^cloud_run_id=' "$meta" && fail "a submit with no run id recorded a run id"
  [ "$(wc -l < "$CLOUD_LOG" | tr -d ' ')" -eq 1 ] || fail "a no-run-id submit must run once"
  pass "a submit that prints no run id keeps its request record and is not retried"
}

# The legacy local path: a spawn with no execution target still reaches its own
# missing-brief check, and no cloud client or terminal runs before it.
test_local_spawn_without_an_execution_target_keeps_the_legacy_path() {
  local home out status
  home=$(new_cloud_case legacy)
  reset_cloud_logs
  out=$(run_cloud_spawn "$home" cloud-legacy-z25 projects/alpha --scout --harness pi)
  status=$?
  [ "$status" -ne 0 ] || fail "a local scout with no brief should fail at the brief check"
  printf '%s\n' "$out" | grep -F 'has no brief at' >/dev/null \
    || fail "a local spawn did not reach its legacy brief check: $out"
  [ ! -s "$CLOUD_LOG" ] || fail "a local spawn reached the cloud client"
  pass "a local spawn with no execution target reaches the legacy brief check and no cloud client"
}

test_batch_dispatches_every_pair
test_batch_mode_boundaries
test_batch_requires_the_shared_delivery_contract
test_scout_batch_refuses_delivery_flags
test_projects_path_scoping
test_cloud_scout_spawn_submits_once_and_records_the_run
test_cloud_spawn_refusals_reach_no_client_or_terminal
test_cloud_spawn_never_overwrites_an_existing_record
test_cloud_submit_failure_keeps_the_request_record_and_never_retries
test_cloud_submit_without_one_run_id_keeps_the_request_record
test_cloud_spawn_refuses_unapproved_model_and_profile
test_local_spawn_without_an_execution_target_keeps_the_legacy_path
