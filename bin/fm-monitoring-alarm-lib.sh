#!/usr/bin/env bash
# fm-monitoring-alarm-lib.sh - captain alarm for supervision that is down while
# work is in flight. Sourced only by bin/fm-guard.sh.
#
# Monitoring is down when at least one task is in flight, the guard's own
# watcher verdict is unhealthy (so this alarm and the model-addressed banner
# agree), and the last beat is older than the grace, or no beat file exists.
# Restoration fires when nothing is in flight or the verdict turns healthy.
# bin/fm-awaiting-captain.sh owns the registry; raise-monitoring keeps one open
# monitoring-down record per outage, and the registry renders it into every
# captain-facing message until restore-monitoring acknowledges it.
#
# This file only records. It never arms, restarts, or repairs the watcher, and
# the repair line it stores is the repair the supervision protocol names.
# Registry failures print one stderr line and never change the guard's exit.
#
# Caller contract: the guard sets FM_HOME-derived state, config, and grace, and
# calls monitoring_alarm_tick before its own early exits. Each call takes the
# registry lock only when it has something to write.

MONITORING_ALARM_LIB_DIR=$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)

# Must match SUMMARY_MAX in bin/fm-awaiting-captain.sh, the registry's own limit.
MONITORING_SUMMARY_MAX=160
MONITORING_REPAIR_MAX=240

# monitoring_elapsed_text <seconds>: "8m 23s" under an hour, "1h 2m" from one.
monitoring_elapsed_text() {
  local seconds=$1 hours minutes
  hours=$((seconds / 3600))
  minutes=$(((seconds % 3600) / 60))
  if [ "$hours" -gt 0 ]; then
    printf '%dh %dm\n' "$hours" "$minutes"
  else
    printf '%dm %ds\n' "$minutes" "$((seconds % 60))"
  fi
}

# monitoring_alarm_registry <home> <state> <awaiting-subcommand> [args...]
# Runs one registry subcommand against this home. Its own stderr is dropped:
# the caller prints the single FAILED line the guard contract promises.
monitoring_alarm_registry() {
  local home=$1 state=$2
  shift 2
  FM_HOME="$home" \
    FM_STATE_OVERRIDE="$state" \
    FM_DATA_OVERRIDE="${FM_DATA_OVERRIDE:-$home/data}" \
    "$MONITORING_ALARM_LIB_DIR/fm-awaiting-captain.sh" "$@" >/dev/null 2>&1
}

# monitoring_alarm_restore <home> <state>: acknowledge the open record. Skips
# the registry tooling entirely when no registry file exists yet.
monitoring_alarm_restore() {
  local home=$1 state=$2 data
  data=${FM_DATA_OVERRIDE:-$home/data}
  [ -e "$data/awaiting-captain.json" ] || return 0
  if ! monitoring_alarm_registry "$home" "$state" restore-monitoring; then
    printf 'monitoring-alarm: FAILED - registry restore rejected\n' >&2
  fi
  return 0
}

# monitoring_repair_line <state> <config> <queue-pending true|false>: the first
# line of the supervision protocol's repair line for this home, same flags the
# guard's banner passes. A missing script falls back to the banner's own text.
monitoring_repair_line() {
  local state=$1 config=$2 queue_pending=$3 afk=0 queue_arg=0 line
  if [ -e "$state/.afk" ]; then afk=1; fi
  if [ "$queue_pending" = true ]; then queue_arg=1; fi
  line=$("$MONITORING_ALARM_LIB_DIR/fm-supervision-instructions.sh" \
    --read-only 0 \
    --afk "$afk" \
    --afk-mode "$(fm_afk_mode "$state")" \
    --queue-pending "$queue_arg" \
    --repair-line 2>/dev/null) \
    || line='Repair missing watcher supervision according to the session-start operating block.'
  line=${line%%$'\n'*}
  printf '%s\n' "${line:0:$MONITORING_REPAIR_MAX}"
}

# monitoring_alarm_tick <home> <state> <config> <grace> <in-flight> <watcher-healthy> <queue-pending>
# One guard run's worth of alarm work. Raises or refreshes the open record while
# monitoring is down past grace, and restores it once monitoring is up or nothing
# is in flight. A beat within grace while unhealthy changes nothing.
monitoring_alarm_tick() {
  local home=$1 state=$2 config=$3 grace=$4 in_flight=$5 healthy=$6 queue_pending=$7
  local beat="$state/.last-watcher-beat" age=0 known=0 down tasks repair summary

  if [ "$in_flight" -lt 1 ] || [ "$healthy" = true ]; then
    monitoring_alarm_restore "$home" "$state"
    return 0
  fi

  if [ -e "$beat" ]; then
    age=$(fm_path_age "$beat")
    if [ "$age" -le "$grace" ]; then
      return 0
    fi
    known=1
  fi

  if [ "$known" -eq 1 ]; then
    down="Monitoring down $(monitoring_elapsed_text "$age")"
  else
    down='Monitoring down, last beat time unknown'
  fi
  if [ "$in_flight" -eq 1 ]; then
    tasks='1 task'
  else
    tasks="$in_flight tasks"
  fi
  repair=$(monitoring_repair_line "$state" "$config" "$queue_pending")
  summary="$down with $tasks in flight; repair: $repair"
  if [ "${#summary}" -gt "$MONITORING_SUMMARY_MAX" ]; then
    summary="${summary:0:$((MONITORING_SUMMARY_MAX - 3))}..."
  fi

  if ! monitoring_alarm_registry "$home" "$state" raise-monitoring --summary "$summary"; then
    printf 'monitoring-alarm: FAILED - registry write rejected\n' >&2
  fi
  return 0
}
