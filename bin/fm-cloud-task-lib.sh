#!/usr/bin/env bash
# fm-cloud-task-lib.sh - client-side gate for task records bound to a cloud target.
#
# A record whose metadata carries `cloud_target=` belongs to a remote run. Its
# authoritative state must be read through the cloud status contract before
# recovery or teardown may touch the record. That contract is not merged yet
# (DPES-815 status, DPES-816 cancel, DPES-817 verified result import), so this
# library cannot establish the state and refuses every such record.
#
# The refusal runs before any local endpoint, backend, worktree, lock, or process
# step, and --force does not bypass it. The record stays byte-identical. Nothing
# here cancels a run, treats a missing local pane as proof of termination, or
# deletes a remote artifact.
#
# BLOCKING (DPES-814): `cloud_target` is a provisional metadata key until the
# explicit cloud-target contract lands. No spawn path writes it yet.
# BLOCKING (DPES-815/816/817): the status, cancel, and result calls replace this
# unconditional refusal once those contracts are merged.
#
# Sourcing contract: bin/fm-backend.sh must be sourced first for fm_meta_get.
#
# Usage (sourced):
#   fm_cloud_task_is_cloud_record <meta-file>
#     Returns 0 silently when the record names a cloud target, including an
#     empty value. Returns 1 otherwise. Callers that must keep a cloud record
#     from being touched use this to refuse with their own context.
#   fm_cloud_task_refuse_teardown <meta-file> <task-id>
#     Returns 0 and prints one refusal line on stderr when the record names a
#     cloud target, including an empty value. Returns 1 and prints nothing for a
#     record that names no cloud target.

fm_cloud_task_is_cloud_record() {  # <meta-file>
  [ -f "$1" ] && grep -q '^cloud_target=' "$1"
}

fm_cloud_task_refuse_teardown() {  # <meta-file> <task-id>
  local target
  fm_cloud_task_is_cloud_record "$1" || return 1
  target=$(fm_meta_get "$1" cloud_target)
  echo "REFUSED: task $2 runs on cloud target ${target:-<empty>}; its authoritative run state cannot be established from this firstmate, so the record is kept and nothing was changed." >&2
  return 0
}
