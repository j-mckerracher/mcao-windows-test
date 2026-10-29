#!/usr/bin/env bash
# fm-resource-gate-lib.sh - whether a local agent start may proceed now, given how
# loaded this host is.
#
# A host short on memory, CPU, or disk cannot take another agent without pushing
# every running agent toward exhaustion. A local start therefore asks the shared
# Rust core first: `mcao resources admit` samples the host, evaluates the limits in
# config/resource-thresholds, and exits 75 when a limit is breached. Exit 75 defers
# the start the way a project-capacity deferral does: nothing is created, the
# backlog item stays queued, a running agent is untouched, and no liveness attempt
# is counted.
#
# This file is the single owner of how a Bash start asks that question and of what
# a missing or failing mcao does. It fails open with one warning line, because a
# broken gate must not silently stop every start on the machine. The verdict, the
# thresholds format, and the comparison rules belong to mcao; docs/configuration.md
# "Host resource gate" is the operator reference.
#
# Call sites: bin/fm-spawn.sh gates every local start, bin/fm-control.sh gates a
# relaunch before the running agent is stopped, and bin/fm-secondmate-liveness-lib.sh
# gates secondmate recovery before its attempt row and its kill.
#
# Environment:
#   FM_MCAO_BIN              path to the mcao binary. Unset means the mcao on PATH.
#   FM_RESOURCE_PREADMITTED  1 when the caller already admitted this start. The gate
#                            reads it and unsets it, so one start consumes it and no
#                            agent it launches inherits it.
#
# Nothing loads at source time. fm_resource_gate_admit sources its dependencies on
# first use, and the callers that need the gate already have them loaded.

# Exit status of a start deferred by host resources: the sysexits "temporary
# failure" code, shared with the project-capacity deferral so one caller check
# covers both.
# shellcheck disable=SC2034 # read by the callers after sourcing.
FM_RESOURCE_GATE_DEFER_EXIT=75

FM_RESOURCE_GATE_LIB_DIR="$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")" && pwd -P)"

fm_resource_gate_require_libs() {
  command -v fm_local_firstmate_state_dirs >/dev/null 2>&1 || {
    # shellcheck source=bin/fm-wake-lib.sh
    . "$FM_RESOURCE_GATE_LIB_DIR/fm-wake-lib.sh"
  }
  command -v fm_project_capacity_config_dir >/dev/null 2>&1 || {
    # shellcheck source=bin/fm-project-capacity-lib.sh
    . "$FM_RESOURCE_GATE_LIB_DIR/fm-project-capacity-lib.sh"
  }
}

# Prints the mcao binary to run, or returns 1 when none is installed.
fm_resource_gate_binary() {
  local bin=${FM_MCAO_BIN:-}
  [ -n "$bin" ] || bin=$(command -v mcao 2>/dev/null) || return 1
  [ -n "$bin" ] && [ -x "$bin" ] || return 1
  printf '%s\n' "$bin"
}

# Sets FM_RESOURCE_GATE_ARGS to the arguments both verbs share: the thresholds
# directory, the disk to measure, and every local home's state directory. The
# thresholds directory is the root home's config, resolved the same way the
# project-capacity declaration is. When the local homes cannot be enumerated, no
# --state-dir is passed and mcao reports the running-agent count as unknown.
fm_resource_gate_common_args() {  # <home> <config-dir> <first-state-dir>
  local home=$1 config=$2 first=$3 root state
  fm_resource_gate_require_libs
  root=$(fm_project_capacity_config_dir "$home" "$config") || return 1
  FM_RESOURCE_GATE_ARGS=(--config-dir "$root" --disk-path "$(dirname "$root")")
  if fm_local_firstmate_state_dirs "$first"; then
    for state in "${FM_LOCAL_FIRSTMATE_STATES[@]}"; do
      FM_RESOURCE_GATE_ARGS+=(--state-dir "$state")
    done
  fi
  return 0
}

# fm_resource_gate_admit <kind> <task-id> <home> <config-dir> <first-state-dir>
#
# Returns 0 to admit, including when the gate fails open, and returns
# FM_RESOURCE_GATE_DEFER_EXIT to defer. On exits 0 and 75 mcao's stderr passes through
# unchanged: on 75 it carries the `deferred:` line, and on 0 it carries any warning
# about the thresholds file, such as a bad line that falls back to the defaults. On
# every other exit mcao's output is dropped and the caller gets exactly one
# `warning: resource gate skipped:` line.
fm_resource_gate_admit() {
  local kind=$1 id=$2 home=$3 config=$4 first=$5 bin err='' rc=0
  local pre=${FM_RESOURCE_PREADMITTED:-}
  unset FM_RESOURCE_PREADMITTED
  [ "$pre" != 1 ] || return 0
  bin=$(fm_resource_gate_binary) || {
    echo "warning: resource gate skipped: the mcao binary was not found (set FM_MCAO_BIN)" >&2
    return 0
  }
  fm_resource_gate_common_args "$home" "$config" "$first" || {
    echo "warning: resource gate skipped: cannot resolve the root Firstmate home" >&2
    return 0
  }
  err=$("$bin" resources admit --kind "$kind" --task "$id" "${FM_RESOURCE_GATE_ARGS[@]}" 2>&1 >/dev/null) || rc=$?
  case "$rc" in
    0 | "$FM_RESOURCE_GATE_DEFER_EXIT")
      [ -z "$err" ] || printf '%s\n' "$err" >&2
      return "$rc"
      ;;
    *) echo "warning: resource gate skipped: mcao exited $rc" >&2; return 0 ;;
  esac
}

# fm_resource_gate_show <home> <config-dir> <first-state-dir>
#
# Prints the host's resource snapshot as JSON and returns non-zero when no
# snapshot is available. Callers treat that as state unknown.
fm_resource_gate_show() {
  local bin
  bin=$(fm_resource_gate_binary) || return 1
  fm_resource_gate_common_args "$1" "$2" "$3" || return 1
  "$bin" resources show --json "${FM_RESOURCE_GATE_ARGS[@]}"
}
