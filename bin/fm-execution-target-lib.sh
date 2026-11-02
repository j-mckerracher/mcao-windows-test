#!/usr/bin/env bash
# fm-execution-target-lib.sh - the ONE owner of execution-target validation for
# a task record, the cloud-routed operation set, and the cloud client adapter.
#
# An execution target says WHERE a worker runs. `local` is the default when a
# record has no execution_target field, so legacy records stay local. `gcp` is
# a cloud run owned by the cloud client. A terminal backend (tmux, herdr) is a
# separate concept that hosts only a local worker. A gcp record has no backend,
# no window, no worktree, and no pane, so nothing here creates one.
#
# Every entry point that reads a record calls fm_exec_record_resolve before any
# side effect. A malformed, ambiguous, or unknown target is refused, never
# guessed into local or tmux. A failed cloud request is never retried locally.
#
# BLOCKING on DPES-805 and DPES-814, which are not merged. The record keys
# cloud_request_id and cloud_run_id, and the `mcao cloud` argv and exit codes in
# fm_exec_cloud_run, are the contract as the Jira text states it. They are
# unverified against a merged client. Change them only in this file.
#
# Sourcing this file has no side effects. Functions print refusals to stderr
# and return nonzero.

FM_EXEC_TARGET_LOCAL=local
FM_EXEC_TARGET_GCP=gcp

fm_exec_target_valid() {  # <value>
  case "${1-}" in
    local|gcp) return 0 ;;
  esac
  return 1
}

# fm_exec_meta_count: how many times <key> appears in <meta-file>. Prints 0
# when the file or key is absent, so the caller can tell absent from repeated.
fm_exec_meta_count() {  # <meta-file> <key>
  local n
  n=$(grep -c "^$2=" "$1" 2>/dev/null || true)
  printf '%s' "${n:-0}"
}

# fm_exec_meta_exact_value: the single non-empty value of <key>. Prints nothing
# and returns 1 when the key is absent, empty, or repeated. The read is never
# last-wins, so an ambiguous record cannot pick a value silently.
fm_exec_meta_exact_value() {  # <meta-file> <key>
  local meta=$1 key=$2 value
  [ "$(fm_exec_meta_count "$meta" "$key")" = 1 ] || return 1
  value=$(grep "^$key=" "$meta" | cut -d= -f2-)
  [ -n "$value" ] || return 1
  printf '%s' "$value"
}

# fm_exec_record_target: the execution target a record declares. Prints local
# or gcp. A missing file or an absent field is local. An empty, repeated, or
# unknown value returns 1 with a refusal.
fm_exec_record_target() {  # <meta-file>
  local meta=$1 n value
  n=$(fm_exec_meta_count "$meta" execution_target)
  case "$n" in
    0) printf '%s' "$FM_EXEC_TARGET_LOCAL"; return 0 ;;
    1) ;;
    *)
      echo "error: task record $(basename "$meta") repeats execution_target; refusing to guess a target" >&2
      return 1
      ;;
  esac
  value=$(fm_exec_meta_exact_value "$meta" execution_target) || {
    echo "error: task record $(basename "$meta") has an empty execution_target; refusing to guess a target" >&2
    return 1
  }
  fm_exec_target_valid "$value" || {
    echo "error: task record $(basename "$meta") has unknown execution_target '$value'; expected local or gcp" >&2
    return 1
  }
  printf '%s' "$value"
}

# fm_exec_record_cloud_ref_valid: a gcp record must carry exactly one
# cloud_request_id and exactly one cloud_run_id, each a plain identifier. A
# record written mid-submit lacks cloud_run_id and is refused here.
fm_exec_record_cloud_ref_valid() {  # <meta-file>
  local meta=$1 key value
  for key in cloud_request_id cloud_run_id; do
    value=$(fm_exec_meta_exact_value "$meta" "$key") || {
      echo "error: cloud task record $(basename "$meta") lacks exactly one $key; refusing" >&2
      return 1
    }
    case "$value" in
      *[!A-Za-z0-9._-]*)
        echo "error: cloud task record $(basename "$meta") has a malformed $key; refusing" >&2
        return 1
        ;;
    esac
  done
  return 0
}

# fm_exec_record_resolve: validate <meta-file> and print its target. A local
# or absent record prints local. A gcp record also needs a complete cloud
# reference. Any other result returns 1 before the caller acts.
fm_exec_record_resolve() {  # <meta-file>
  local meta=$1 target
  target=$(fm_exec_record_target "$meta") || return 1
  if [ "$target" = "$FM_EXEC_TARGET_GCP" ]; then
    fm_exec_record_cloud_ref_valid "$meta" || return 1
  fi
  printf '%s' "$target"
}

# fm_exec_meta_is_local: 0 only when the record resolves to the local target. A
# caller that reads a local terminal skips a gcp record, and an invalid reference
# returns 1 with its refusal on stderr, so it is skipped rather than read.
fm_exec_meta_is_local() {  # <meta-file>
  local target
  target=$(fm_exec_record_resolve "$1") || return 1
  [ "$target" = "$FM_EXEC_TARGET_LOCAL" ]
}

# The operations a gcp task routes to the cloud client. Submit runs only at
# spawn, through fm_spawn's cloud path, and doctor is a non-launching
# prerequisite check that no lifecycle verb calls.
fm_exec_cloud_op_supported() {  # <operation>
  case "${1-}" in
    status|logs|cancel|fetch) return 0 ;;
  esac
  return 1
}

# fm_exec_refuse_cloud_op: the refusal for a local-only operation requested on a
# gcp task. The caller returns before any checkpoint, pane, brief, or state
# write.
fm_exec_refuse_cloud_op() {  # <task-id> <operation>
  printf "error: '%s' is not supported for task %s on execution target gcp; nothing was changed and no local fallback runs. Supported cloud operations: status, logs, cancel, fetch. Interrupt, exit, relaunch, steering, key or slash input, attach, and promotion are local-only. Follow-up work on a cloud task is a new assignment with an explicit new source commit.\n" "$2" "$1" >&2
  return 1
}

# fm_exec_refuse_cloud_restart: restart of a failed gcp run is disabled. The
# retry request argv and response shape (DPES-805/814) and the recovery policy
# (DPES-869/870) are not published, so no cloud request is sent and nothing is
# recorded. A failed cloud run is never restarted on the local machine.
fm_exec_refuse_cloud_restart() {  # <task-id>
  printf "error: 'retry' (restart) is unavailable for task %s on execution target gcp: the restart contract is not published (cloud retry argv and response shape DPES-805/814, recovery policy DPES-869/870). Nothing was changed and no cloud request was sent. No local fallback runs.\n" "$1" >&2
  return 1
}

# fm_exec_refuse_local_retry: restart is defined only for a gcp task. A local
# task has no cloud run to restart, so this refuses rather than mapping it onto
# relaunch.
fm_exec_refuse_local_retry() {  # <task-id>
  printf "error: 'retry' is an unsupported operation for task %s on execution target local; restart applies only to gcp tasks. Use 'relaunch' to start a fresh agent with the brief and a progress note.\n" "$1" >&2
  return 1
}

# fm_exec_refuse_local_cancel: cancellation is defined only for a gcp task.
# A local task has no cancel operation, so this refuses rather than mapping it
# onto interrupt or exit.
fm_exec_refuse_local_cancel() {  # <task-id>
  printf "error: 'cancel' is an unsupported operation for task %s on execution target local; cancellation applies only to gcp tasks. Use 'interrupt' to stop the current turn or 'exit' to stop the agent.\n" "$1" >&2
  return 1
}

# fm_exec_cloud_client_check: refuse before any record is written when the cloud
# client is absent, so a missing client leaves nothing to reconcile.
fm_exec_cloud_client_check() {
  command -v mcao >/dev/null 2>&1 || {
    echo "error: cloud client mcao is not installed; cannot run this cloud operation. No local fallback runs." >&2
    return 1
  }
}

# fm_exec_cloud_run: run one `mcao cloud` operation and return its status.
# Output passes through. A missing client is an error, never a local fallback.
fm_exec_cloud_run() {  # <operation> [args...]
  local op=$1
  shift
  fm_exec_cloud_client_check || return 1
  mcao cloud "$op" "$@"
}

# fm_exec_cloud_run_timed: fm_exec_cloud_run under a hard bound. fm_run_timed
# (bin/fm-timeout-lib.sh, which the caller must source first) execs its argv, so
# a shell function cannot be bounded directly; this runs the same `mcao cloud`
# argv through it. A missing client fails before any run is contacted.
fm_exec_cloud_run_timed() {  # <seconds> <operation> [args...]
  local secs=$1
  shift
  fm_exec_cloud_client_check || return 1
  fm_run_timed "$secs" mcao cloud "$@"
}

# fm_exec_new_request_id: a lowercase UUID for the submit request. The spawn
# records it before submit, so a retried or reconciled submit is identifiable.
fm_exec_new_request_id() {
  local id
  id=$(uuidgen 2>/dev/null | tr '[:upper:]' '[:lower:]') || id=
  case "$id" in
    ????????-????-????-????-????????????) printf '%s' "$id"; return 0 ;;
  esac
  id=$(node -e 'process.stdout.write(require("node:crypto").randomUUID())' 2>/dev/null) || return 1
  case "$id" in
    ????????-????-????-????-????????????) printf '%s' "$id"; return 0 ;;
  esac
  return 1
}

# The pilot allowlist. It mirrors the merged protocol-v1 values: the model is
# cloud/src/mao_cloud/protocol.py MODEL_VALUES, and the profile is the pilot
# standard 4 vCPU / 16 GiB profile only (DPES-803 decision). An unapproved request
# is refused before any record or submit exists. The server allowlist stays
# authoritative, and DPES-805 owns the final approved set.
FM_EXEC_CLOUD_MODELS="gemini-3.8-flash"
FM_EXEC_CLOUD_PROFILES="pi-standard"

# fm_exec_cloud_request_allowed: refuse a model or profile that is off the pilot
# allowlist, naming the approved values. Writes to stderr and returns nonzero.
fm_exec_cloud_request_allowed() {  # <model> <profile>
  case " $FM_EXEC_CLOUD_MODELS " in
    *" $1 "*) ;;
    *)
      printf "error: --model '%s' is not on the approved cloud model allowlist (approved: %s); nothing was recorded or submitted\n" "$1" "$FM_EXEC_CLOUD_MODELS" >&2
      return 1
      ;;
  esac
  case " $FM_EXEC_CLOUD_PROFILES " in
    *" $2 "*) ;;
    *)
      printf "error: --profile '%s' is not on the approved cloud profile allowlist (approved: %s); nothing was recorded or submitted\n" "$2" "$FM_EXEC_CLOUD_PROFILES" >&2
      return 1
      ;;
  esac
  return 0
}

# fm_exec_cloud_submit: the only submit argv. Spawn-only. The cloud client takes
# `--agent pi`; the spawn flag that selects it is --harness pi, validated before
# this runs. The model and profile were checked by fm_exec_cloud_request_allowed.
fm_exec_cloud_submit() {  # <repo> <commit> <task-file> <model> <profile> <request-id>
  fm_exec_cloud_run submit --repo "$1" --ref "$2" --task "$3" --agent pi \
    --model "$4" --profile "$5" --request-id "$6"
}

# fm_exec_parse_run_id: the run id a submit printed. Exactly one non-empty line
# that is a plain identifier. Anything else returns 1 and the caller refuses.
fm_exec_parse_run_id() {  # <submit-output>
  local line run='' count=0
  while IFS= read -r line; do
    [ -n "$line" ] || continue
    count=$((count + 1))
    run=$line
  done <<EOF
$1
EOF
  [ "$count" -eq 1 ] || return 1
  case "$run" in
    '' | *[!A-Za-z0-9._-]*) return 1 ;;
  esac
  printf '%s' "$run"
}
