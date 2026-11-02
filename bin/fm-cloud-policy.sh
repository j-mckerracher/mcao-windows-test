#!/usr/bin/env bash
# fm-cloud-policy.sh - the single authorization owner for cloud assignment routes.
#
# Every cloud route must reach the shared cloud client only through this
# entrypoint, so one action gets one admission result whichever route asked:
#
#   fm-cloud-policy.sh run <action> [--task <id>] [--run <ref>] -- <command> [args...]
#
# Actions:
#   doctor          observation. Runs <command>; no binding, lock, or lease.
#   status, logs    observation. Need --run. No run-ownership source is wired, so
#                   they are refused rather than inferring ownership from the reference.
#   submit          assignment creation. Need --task. Runs <command> only while
#                   holding $STATE/.task-set.lock and only when admission passes.
#   cancel, fetch   mutation. Need --task and --run. Run binding is not wired, so
#                   they are refused after the supervision lease guard.
#
# Checks, in order. Every refusal exits before <command> runs:
#   1. Parse (exit 2): missing action, unknown flag, missing --, empty command,
#      or a flag the action requires.
#   2. Action (exit 1, unsupported-action): anything outside the six actions.
#   3. Gate (exit 3): submit, cancel, and fetch refuse inside a no-mistakes gate
#      context (bin/fm-gate-refuse-lib.sh). Observation is never gated. The gate
#      runs before the managed-context check so a gate worktree gets its explicit
#      refusal rather than a generic one.
#   4. Managed context (exit 1, unmanaged-context): $FM_ROOT must be a genuine
#      primary checkout whose state directory exists (bin/fm-primary-scope-lib.sh).
#   5. Malformed record (exit 1, malformed-context): submit, cancel, and fetch
#      refuse when an away-posture record is present but does not validate.
#      bin/fm-spawn.sh treats that case as no cap; this owner fails closed.
#   6. Supervision branch (exit 6): submit, cancel, and fetch pass only under the
#      away-posture record (fm_lease_forbid_branch --away-relocated).
#   7. Action admission:
#      submit: task id must be safe (invalid-task-id). Take the task-set lock
#        without waiting (admission-busy). Under the branch actor, the task must be
#        queued, unheld, and unblocked (queue-not-dispatchable). Under an away
#        spend cap, a local count already at the cap refuses (cap-reached); the
#        cloud term then fails closed (cloud-count-unavailable). Only then does
#        <command> run, under the held lock.
#      cancel, fetch: task id must be safe (invalid-task-id). Take the supervision
#        lease guard (exit 6 when the other actor holds the task). Run id must be
#        safe (invalid-run-binding). Then run-binding-unavailable.
#      status, logs: run id must be safe (invalid-run-binding), then
#        run-binding-unavailable.
#
# Ordering note: the per-task queued-work check runs before the spend cap, so an
# arbitrary task reports queue-not-dispatchable. bin/fm-spawn.sh checks the cap
# first. Both refuse; only the stated reason differs.
#
# Output: a denial prints one stderr line, `error: cloud <action> refused -
# <reason>: <detail>`, and exits 1. Gate and lease refusals keep their own exit
# codes (3 and 6). An allowed run exits with <command>'s own status and prints
# nothing else. The owner never prints the command's arguments or any credential,
# writes no records, and never falls back to another path.
#
# The cloud term of the spend cap fails closed until a cloud assignment source is
# wired (bin/fm-away-admission-lib.sh, fm_away_cloud_ordinary_count). Nothing here
# selects a runtime, adds a native tool, or wires fm-spawn or fm-control to this
# entrypoint; those routes belong to the DPES-805, 808, 814, 873, and 875 work.
set -u

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
FM_ROOT="${FM_ROOT_OVERRIDE:-$(cd "$SCRIPT_DIR/.." && pwd)}"
FM_HOME="${FM_HOME:-${FM_ROOT_OVERRIDE:-$FM_ROOT}}"
STATE="${FM_STATE_OVERRIDE:-$FM_HOME/state}"
DATA="${FM_DATA_OVERRIDE:-$FM_HOME/data}"

# shellcheck source=bin/fm-primary-scope-lib.sh
. "$SCRIPT_DIR/fm-primary-scope-lib.sh"
# shellcheck source=bin/fm-gate-refuse-lib.sh
. "$SCRIPT_DIR/fm-gate-refuse-lib.sh"
# shellcheck source=bin/fm-wake-lib.sh
. "$SCRIPT_DIR/fm-wake-lib.sh"
# shellcheck source=bin/fm-lease-lib.sh
. "$SCRIPT_DIR/fm-lease-lib.sh"
# shellcheck source=bin/fm-tasks-axi-lib.sh
. "$SCRIPT_DIR/fm-tasks-axi-lib.sh"
# shellcheck source=bin/fm-backlog-transition-lib.sh
. "$SCRIPT_DIR/fm-backlog-transition-lib.sh"
# shellcheck source=bin/fm-away-admission-lib.sh
. "$SCRIPT_DIR/fm-away-admission-lib.sh"

OWNER_TASK_SET_LOCK=
OWNER_TASK_SET_LOCK_HELD=0

owner_cleanup() {
  fm_lease_guard_release
  if [ "$OWNER_TASK_SET_LOCK_HELD" = 1 ]; then
    OWNER_TASK_SET_LOCK_HELD=0
    fm_lock_release "$OWNER_TASK_SET_LOCK" || true
  fi
}
trap owner_cleanup EXIT
trap 'exit 130' INT
trap 'exit 143' TERM

usage() {
  echo "usage: fm-cloud-policy.sh run <doctor|status|logs|submit|cancel|fetch> [--task <id>] [--run <ref>] -- <command> [args...]" >&2
}

usage_error() {
  echo "error: $1" >&2
  usage
  exit 2
}

deny() {  # <action> <reason> <detail>
  echo "error: cloud $1 refused - $2: $3" >&2
  exit 1
}

# run_command: run the caller's command and exit with its status. Not exec,
# because the EXIT trap must still release any lock or lease the owner holds.
run_command() {
  local status=0
  "$@" || status=$?
  exit "$status"
}

owner_submit() {
  local lock actor cap live
  fm_lease_valid_id "$task" || deny submit invalid-task-id "the task id is not a safe identifier"
  lock=$(fm_task_set_lock_path "$STATE") || {
    echo "error: could not resolve the task-set lock for $STATE" >&2
    exit 1
  }
  # Refuse rather than wait: a forced teardown holding this lock is enumerating
  # or removing tasks, and a fresh assignment must not race it.
  fm_lock_try_acquire "$lock" || deny submit admission-busy "this home's task set is locked by another operation (a forced teardown is enumerating or removing its tasks); refusing rather than racing it"
  OWNER_TASK_SET_LOCK=$lock
  OWNER_TASK_SET_LOCK_HELD=1

  actor=$(fm_lease_actor) || exit "$FM_LEASE_REFUSE_EXIT"
  if [ "$actor" = branch ]; then
    fm_away_task_queued_unblocked "$DATA" "$task" ||
      deny submit queue-not-dispatchable "the supervision branch under the away-posture record may submit only queued unblocked work; task $task has no dispatchable backlog item in this home"
  fi

  if fm_away_cap "$STATE"; then
    cap=$FM_AWAY_CAP
    fm_away_local_ordinary_count "$STATE"
    live=$FM_AWAY_LOCAL_COUNT
    # Local records alone at the cap are decisive: the cloud term can only add.
    if [ "$live" -ge "$cap" ]; then
      deny submit cap-reached "the away-posture record caps concurrent workers at $cap and $live ordinary task(s) are live in this home; task $task stays queued for the captain's return or for a worker to finish (spend cap: bin/fm-afk-contract.sh)"
    fi
    # Below the cap, the total is unknowable until a cloud source is wired, so
    # the submit fails closed. A wired source must add its count to $live here
    # before the comparison above is repeated.
    fm_away_cloud_ordinary_count "$STATE" ||
      deny submit cloud-count-unavailable "$FM_AWAY_CLOUD_ERROR; the spend cap cannot be proven under the away-posture record"
  fi

  run_command "$@"
}

[ "${1:-}" = run ] || usage_error "expected 'run' as the first argument"
[ "$#" -ge 2 ] || usage_error "missing action"
action=$2
shift 2
case "$action" in
doctor | status | logs | submit | cancel | fetch) ;;
*) deny "$action" unsupported-action "this owner authorizes only doctor, status, logs, submit, cancel, and fetch" ;;
esac

task=
run=
saw_separator=0
while [ "$#" -gt 0 ]; do
  case "$1" in
  --task)
    [ "$#" -ge 2 ] || usage_error "--task needs a value"
    task=$2
    shift 2
    ;;
  --run)
    [ "$#" -ge 2 ] || usage_error "--run needs a value"
    run=$2
    shift 2
    ;;
  --)
    saw_separator=1
    shift
    break
    ;;
  *) usage_error "unknown argument '$1' before --" ;;
  esac
done
[ "$saw_separator" = 1 ] || usage_error "missing -- before the command"
[ "$#" -ge 1 ] && [ -n "$1" ] || usage_error "empty command after --"

case "$action" in
status | logs) [ -n "$run" ] || usage_error "$action needs --run <ref>" ;;
submit) [ -n "$task" ] || usage_error "submit needs --task <id>" ;;
cancel | fetch) [ -n "$task" ] && [ -n "$run" ] || usage_error "$action needs --task <id> and --run <ref>" ;;
esac

case "$action" in
submit | cancel | fetch) fm_refuse_if_gate_agent "$FM_ROOT" ;;
esac

fm_primary_scope_matches "$FM_ROOT" "$STATE" ||
  deny "$action" unmanaged-context "this checkout is not a primary firstmate home with a state directory, so the cloud route is not available here"

case "$action" in
submit | cancel | fetch)
  if [ -e "$STATE/.afk-contract" ] || [ -L "$STATE/.afk-contract" ]; then
    FM_STATE_OVERRIDE="$STATE" "$SCRIPT_DIR/fm-afk-contract.sh" validate >/dev/null 2>&1 ||
      deny "$action" malformed-context "the away-posture record is present but does not validate, so this mutation cannot be judged against it"
  fi
  fm_lease_forbid_branch "cloud $action" --away-relocated
  ;;
esac

case "$action" in
doctor) run_command "$@" ;;
status | logs)
  fm_lease_valid_id "$run" || deny "$action" invalid-run-binding "the run reference is not a safe identifier"
  deny "$action" run-binding-unavailable "no run-ownership source is wired yet (DPES-808 pending), so this run cannot be proven owned by this home"
  ;;
cancel | fetch)
  fm_lease_valid_id "$task" || deny "$action" invalid-task-id "the task id is not a safe identifier"
  fm_lease_guard "$task" "cloud $action"
  fm_lease_valid_id "$run" || deny "$action" invalid-run-binding "the run reference is not a safe identifier"
  deny "$action" run-binding-unavailable "no run-ownership source is wired yet (DPES-808 pending), so this run cannot be proven owned by this home"
  ;;
submit) owner_submit "$@" ;;
esac
