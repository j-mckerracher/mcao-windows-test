#!/usr/bin/env bash
# fm-away-admission-lib.sh - the away-posture admission predicates shared by
# the fresh-spawn gate (bin/fm-spawn.sh) and the cloud assignment owner
# (bin/fm-cloud-policy.sh), so the spend cap and the queued-work entitlement
# have exactly one implementation.
#
# Sourced only. No side effects on source, no exit, no locks, no refusal text.
# The caller owns locks, messages, and exit codes; the record and the backlog
# probe keep their own owners (bin/fm-afk-contract.sh and
# bin/fm-backlog-transition-lib.sh).
#
# Results come back through globals, never stdout: a command substitution runs
# in a subshell and would drop the detail the caller must print.
#
#   fm_away_cap <state-dir>
#       Return 0 and set FM_AWAY_CAP iff an away-posture record applies with a
#       numeric, nonzero spend_max_concurrent_workers. Otherwise return 1 with
#       FM_AWAY_CAP empty. A quiet-mode record never applies, and a record that
#       does not validate never applies here (callers that must fail closed on
#       a malformed record check that themselves).
#
#   fm_away_local_ordinary_count <state-dir>
#       Set FM_AWAY_LOCAL_COUNT to the number of state/*.meta records whose last
#       kind= is not secondmate, the same count the return brief uses. Returns 0.
#
#   fm_away_cloud_ordinary_count <state-dir>
#       NARROW ADAPTER for the cloud term of the spend cap. No cloud assignment
#       source is wired yet (DPES-805/808 pending), so this always returns 1 and
#       sets FM_AWAY_CLOUD_ERROR. Callers treat that as fail closed. Wire a real
#       source here, and only here, when one exists.
#
#   fm_away_task_queued_unblocked <data-dir> <task-id>
#       Return 0 iff the backlog row for <task-id> is queued, not held, and not
#       blocked. The caller must have sourced bin/fm-backlog-transition-lib.sh.

FM_AWAY_LIB_DIR="$(d=${BASH_SOURCE[0]%/*}; [ "$d" != "${BASH_SOURCE[0]}" ] || d=.; cd "${d:-/}" && pwd)"

fm_away_cap() {
  local state=$1 cap
  FM_AWAY_CAP=
  [ -f "$state/.afk-contract" ] || return 1
  FM_STATE_OVERRIDE="$state" "$FM_AWAY_LIB_DIR/fm-afk-contract.sh" validate >/dev/null 2>&1 || return 1
  [ "$(FM_STATE_OVERRIDE="$state" "$FM_AWAY_LIB_DIR/fm-afk-contract.sh" mode 2>/dev/null)" = away ] || return 1
  cap=$(FM_STATE_OVERRIDE="$state" "$FM_AWAY_LIB_DIR/fm-afk-contract.sh" field spend_max_concurrent_workers 2>/dev/null || true)
  case "$cap" in
  '' | *[!0-9]* | 0) return 1 ;;
  esac
  # shellcheck disable=SC2034 # Output global, read by the sourcing caller.
  FM_AWAY_CAP=$cap
}

fm_away_local_ordinary_count() {
  local state=$1 meta live=0
  FM_AWAY_LOCAL_COUNT=0
  for meta in "$state"/*.meta; do
    [ -f "$meta" ] || continue
    [ "$(grep '^kind=' "$meta" 2>/dev/null | tail -1 | cut -d= -f2-)" != secondmate ] || continue
    live=$((live + 1))
  done
  # shellcheck disable=SC2034 # Output global, read by the sourcing caller.
  FM_AWAY_LOCAL_COUNT=$live
}

fm_away_cloud_ordinary_count() {
  local state=$1
  # shellcheck disable=SC2034 # Output global, read by the sourcing caller.
  FM_AWAY_CLOUD_ERROR="no cloud assignment source is wired for $state (DPES-805/808 pending)"
  return 1
}

fm_away_task_queued_unblocked() {  # <data-dir> <task-id>
  fm_backlog_row_probe "$1" "$2" && [ "$FM_BACKLOG_ROW_STATE" = "queued no no" ]
}
