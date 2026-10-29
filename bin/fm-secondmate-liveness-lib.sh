#!/usr/bin/env bash
# shellcheck disable=SC2034 # Probe/relaunch output globals are read by sourcing callers.
# fm-secondmate-liveness-lib.sh - shared persistent-secondmate endpoint liveness
# probing and recovery. bin/fm-bootstrap.sh owns the session-start sweep and
# bin/fm-watch.sh owns the ordinary-supervision poll tick; both drive this
# library so classification handling and the guarded relaunch path stay
# single-sourced here.
#
# A secondmate's recorded endpoint is the tmux window or herdr pane it runs
# in. Probing classifies that endpoint through the owning backend adapter's
# fm_backend_agent_state, which returns one of:
#
#   alive       - a primary-agent runtime is positively running
#   dead        - the endpoint exists, but no agent is running in it
#   missing     - the endpoint itself is gone
#   ambiguous   - backend inventory could not prove either way
#   unreadable  - backend state exists but could not be parsed
#   unverified  - the endpoint is recorded under a session this home does not
#                 own, so probing is not authorized
#
# Only `dead` and `missing` are recovery-authorizing states: they prove the
# agent is not running, so relaunching cannot produce a duplicate endpoint.
# `ambiguous`, `unreadable`, and `unverified` leave the endpoint untouched -
# relaunching on inconclusive evidence could create a second endpoint beside a
# live one.
#
# Relaunch goes through `bin/fm-spawn.sh <id> --secondmate` with
# FM_SPAWN_NO_GUARD=1, the same guarded path every recovery uses. That path
# re-resolves placement from the task's own metadata and registry route.
#
# Records that still name a remote host are reported as skipped and are never
# probed, killed, or relaunched; their routes are left exactly as recorded.
#
# Callers pass <full|poll>. Local probes read the same state in both modes, so
# the argument is accepted for caller compatibility and does not change the
# verdict.
#
# Concurrency: fm_secondmate_liveness_lock serializes probe+kill+relaunch per
# task across the bootstrap sweep and the watcher tick, so a concurrent
# relaunch can never be observed mid-flight as a dead endpoint and killed.
# The attempt ledger (.secondmate-relaunch-<id>, one line per attempt plus one
# per outcome) is both the durable relaunch record and the input to the
# watcher's relaunch bound; teardown removes it.

set -u

FM_SM_LIVE_LIB_DIR="$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")" && pwd -P)"

# shellcheck source=bin/fm-backend.sh
. "$FM_SM_LIVE_LIB_DIR/fm-backend.sh"
# shellcheck source=bin/fm-timeout-lib.sh
. "$FM_SM_LIVE_LIB_DIR/fm-timeout-lib.sh"
# shellcheck source=bin/fm-resource-gate-lib.sh
. "$FM_SM_LIVE_LIB_DIR/fm-resource-gate-lib.sh"

# Per-task probe+kill+relaunch serialization. A busy lock means another
# supervisor (the other sweep, or a racing tick) is mid-episode on this mate;
# callers skip and let that episode finish rather than probe a moving target.
# The lock helpers live in bin/fm-wake-lib.sh, which creates the state
# directory when sourced; load it only when a lock is actually taken so that
# sourcing this library stays side-effect free for read-only bootstrap runs.
fm_sm_live_require_locks() {
  command -v fm_lock_try_acquire >/dev/null 2>&1 && return 0
  # shellcheck source=bin/fm-wake-lib.sh
  . "$FM_SM_LIVE_LIB_DIR/fm-wake-lib.sh"
}

fm_secondmate_liveness_lock() {  # <id>
  fm_sm_live_require_locks || return 1
  fm_lock_try_acquire "$STATE/.secondmate-liveness-$1.lock"
}

fm_secondmate_liveness_unlock() {  # <id>
  fm_sm_live_require_locks || return 0
  fm_lock_release "$STATE/.secondmate-liveness-$1.lock" 2>/dev/null || true
}

fm_sm_live_first_line() {
  printf '%s\n' "$1" | sed -n '1s/[[:space:]]\{1,\}/ /g;1p'
}

# One line per relaunch attempt and one per outcome, keyed by epoch, plus a
# `rearmed` row when a live probe lifts a parked mate. The watcher bound counts
# `attempt` rows inside its window and after the last `rearmed` row; the whole
# file is the durable per-mate relaunch record the captain can count to see
# frequency. Fails when the row cannot be appended.
fm_secondmate_liveness_ledger_add() {  # <id> <attempt|relaunched|failed|rearmed>
  printf '%s\t%s\n' "$(date +%s)" "$2" >> "$STATE/.secondmate-relaunch-$1" 2>/dev/null
}

# Count of attempt rows no older than <window-secs> that follow the last
# `rearmed` row. An absent ledger counts
# zero; an existing ledger that cannot be read fails rather than counting zero.
fm_secondmate_liveness_recent_attempts() {  # <id> <window-secs>
  local id=$1 window=$2 now cutoff ledger
  ledger="$STATE/.secondmate-relaunch-$id"
  if [ ! -e "$ledger" ] && [ ! -L "$ledger" ]; then
    printf '0\n'
    return 0
  fi
  now=$(date +%s)
  cutoff=$((now - window))
  awk -F '\t' -v cutoff="$cutoff" \
    '$2 == "rearmed" { n = 0; next } $1 ~ /^[0-9]+$/ && $1 >= cutoff && $2 == "attempt" { n++ } END { print n + 0 }' \
    "$ledger" 2>/dev/null
}

# fm_secondmate_liveness_probe <meta> <id> <full|poll>
#
# Read-only probe of one registered secondmate's recorded endpoint. Populates:
#
#   FM_SM_LIVE_STATUS  silent | alive | relaunchable | skipped | deferred
#                      (deferred is set only by fm_secondmate_liveness_relaunch,
#                      when host resources defer the start)
#   FM_SM_LIVE_STATE   the raw classifier/state word
#   FM_SM_LIVE_KILL    1 when relaunch must first kill a confirmed-dead local
#                      endpoint (its shell husk occupies the name)
#   FM_SM_LIVE_CAUSE   relaunch cause phrase, on relaunchable
#   FM_SM_LIVE_WHERE   backend=<b>, on relaunchable
#   FM_SM_LIVE_REASON  exact skip suffix, on skipped
#   FM_SM_LIVE_LINE    verbose already-live line body, on alive
#
# `silent` means the meta records no endpoint at all - that shape is owned by
# secondmate-provisioning recovery, not liveness.
#
# The caller must hold fm_secondmate_liveness_lock for <id> whenever a
# relaunchable verdict could be acted on.
# The <mode> argument is accepted from every caller; local probes do not
# vary by it.
fm_secondmate_liveness_probe() {  # <meta> <id> <full|poll>
  local meta=$1 id=$2 mode=$3
  FM_SM_LIVE_STATUS=skipped FM_SM_LIVE_STATE=unknown FM_SM_LIVE_KILL=0
  FM_SM_LIVE_CAUSE='' FM_SM_LIVE_WHERE='' FM_SM_LIVE_REASON='' FM_SM_LIVE_LINE=''
  local window harness remote_host agent_state
  window=$(fm_meta_get "$meta" window)
  [ -n "$window" ] || { FM_SM_LIVE_STATUS=silent; return 0; }
  harness=$(fm_meta_get "$meta" harness)
  remote_host=$(fm_meta_get "$meta" remote_host)
  if [ -n "$remote_host" ]; then
    # Remote secondmate placement is no longer supported. A record that still
    # names a remote host is refused here before any probe, kill, or relaunch.
    FM_SM_LIVE_STATUS=skipped
    FM_SM_LIVE_REASON="remote secondmate placement on $remote_host is no longer supported; route left as recorded"
    return 0
  fi

  local backend target
  backend=$(fm_backend_of_meta "$meta")
  target=$(fm_backend_target_of_meta "$meta")
  [ -n "$target" ] || target="$window"
  agent_state=$(fm_backend_agent_state "$backend" "$target" 2>/dev/null) || agent_state=unreadable
  case "$harness" in
    claude|codex|opencode|pi|pi-signed|grok|kimi|omp) ;;
    *)
      case "$agent_state" in dead|missing) agent_state=unverified-harness ;; esac
      ;;
  esac
  FM_SM_LIVE_STATE=$agent_state
  case "$agent_state" in
    alive)
      FM_SM_LIVE_STATUS=alive
      FM_SM_LIVE_LINE="secondmate $id already live (backend=$backend)"
      ;;
    dead|missing)
      FM_SM_LIVE_STATUS=relaunchable
      if [ "$agent_state" = dead ]; then
        FM_SM_LIVE_KILL=1
        FM_SM_LIVE_CAUSE="confirmed agent absence on existing endpoint"
      else
        FM_SM_LIVE_CAUSE="recorded endpoint confidently missing"
      fi
      FM_SM_LIVE_WHERE="backend=$backend"
      ;;
    ambiguous)
      FM_SM_LIVE_REASON="existing endpoint has ambiguous agent process (backend=$backend)"
      ;;
    unreadable)
      FM_SM_LIVE_REASON="endpoint probe unreadable (backend=$backend)"
      ;;
    unverified-harness)
      FM_SM_LIVE_REASON="recorded harness '$harness' is unverified for recovery (backend=$backend)"
      ;;
    *)
      FM_SM_LIVE_REASON="agent recovery classifier unverified (backend=$backend)"
      ;;
  esac
  return 0
}

# fm_secondmate_liveness_relaunch <meta> <id> [timeout-secs]
#
# Acts on a `relaunchable` probe verdict for <id>: kills a confirmed-dead local
# endpoint first (FM_SM_LIVE_KILL), records the attempt and its outcome in the
# per-mate ledger, then runs the guarded secondmate spawn. A positive timeout
# wraps the spawn in fm_run_timed so a watcher poll stays bounded; 124/137 mean
# the bound fired. Returns the spawn exit status; combined spawn output is in
# FM_SM_LIVE_OUT and the status in FM_SM_LIVE_RC. When the ledger cannot be
# read or the attempt row cannot be appended, nothing is killed or spawned: the verdict becomes
# FM_SM_LIVE_STATUS=skipped with FM_SM_LIVE_REASON set and this returns 1.
# Caller holds the liveness lock and owns reporting.
# Before any ledger row or kill, a local endpoint's relaunch asks the host-resource
# gate. A deferral sets FM_SM_LIVE_STATUS=deferred, FM_SM_LIVE_REASON, and
# FM_SM_LIVE_RC=75 and returns 1 with no attempt row, no kill, and no spawn, so the
# deferral cannot park the mate. A remote-placed record skips the gate.
fm_secondmate_liveness_relaunch() {  # <meta> <id> [timeout-secs]
  local meta=$1 id=$2 timeout=${3:-}
  FM_SM_LIVE_OUT='' FM_SM_LIVE_RC=0
  if [ -z "$(fm_meta_get "$meta" remote_host)" ]; then
    local gate_rc=0 gate_out
    gate_out=$(fm_resource_gate_admit secondmate "$id" "$FM_HOME" "${FM_CONFIG_OVERRIDE:-$FM_HOME/config}" "$STATE" 2>&1) || gate_rc=$?
    if [ "$gate_rc" -eq "$FM_RESOURCE_GATE_DEFER_EXIT" ]; then
      FM_SM_LIVE_OUT=$gate_out
      FM_SM_LIVE_RC=$gate_rc
      FM_SM_LIVE_STATUS=deferred
      FM_SM_LIVE_REASON=$(fm_sm_live_first_line "$gate_out")
      return 1
    fi
    # An admitted start can still carry the fail-open warning; keep it visible.
    [ -z "$gate_out" ] || printf '%s\n' "$gate_out" >&2
  fi
  if ! fm_secondmate_liveness_recent_attempts "$id" 0 >/dev/null; then
    FM_SM_LIVE_STATUS=skipped
    FM_SM_LIVE_REASON="relaunch ledger $STATE/.secondmate-relaunch-$id is unreadable; endpoint left $FM_SM_LIVE_STATE"
    FM_SM_LIVE_RC=1
    return 1
  fi
  if ! fm_secondmate_liveness_ledger_add "$id" attempt; then
    FM_SM_LIVE_STATUS=skipped
    FM_SM_LIVE_REASON="relaunch ledger $STATE/.secondmate-relaunch-$id is unwritable; endpoint left $FM_SM_LIVE_STATE"
    FM_SM_LIVE_RC=1
    return 1
  fi
  if [ "$FM_SM_LIVE_KILL" = 1 ]; then
    local backend target window
    backend=$(fm_backend_of_meta "$meta")
    target=$(fm_backend_target_of_meta "$meta")
    if [ -z "$target" ]; then
      window=$(fm_meta_get "$meta" window)
      target=$window
    fi
    [ -z "$target" ] || fm_backend_kill "$backend" "$target" 2>/dev/null || true
  fi
  local rc=0
  if [ -n "$timeout" ]; then
    FM_SM_LIVE_OUT=$(FM_SPAWN_NO_GUARD=1 FM_RESOURCE_PREADMITTED=1 fm_run_timed "$timeout" "$FM_ROOT/bin/fm-spawn.sh" "$id" --secondmate 2>&1) || rc=$?
  else
    FM_SM_LIVE_OUT=$(FM_SPAWN_NO_GUARD=1 FM_RESOURCE_PREADMITTED=1 "$FM_ROOT/bin/fm-spawn.sh" "$id" --secondmate 2>&1) || rc=$?
  fi
  FM_SM_LIVE_RC=$rc
  if [ "$rc" -eq 0 ]; then
    fm_secondmate_liveness_ledger_add "$id" relaunched || true
  else
    fm_secondmate_liveness_ledger_add "$id" failed || true
  fi
  return "$rc"
}
