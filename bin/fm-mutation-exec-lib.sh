#!/usr/bin/env bash
# fm-mutation-exec-lib.sh - the single owner of how the mutation runner judges
# each mapped change: the shell custom mutator's operators and per-mutant
# verdicts, the bounded oracle run every verdict depends on, and the
# report-only records for the Python and TypeScript/JavaScript strategies.
# Source-safe, no side effects. bin/fm-mutation-run.sh owns the command line,
# the scratch tree, and the evidence record shape; bin/fm-mutation-surface-lib.sh
# owns which strategy each surface receives.
#
# Shell custom mutator (version 1), report-only:
#   Each mutant changes exactly one line of one production script, inside a
#   scratch copy of the tree; the work tree is never written. The mapped oracle
#   is tests/<basename>.test.sh and must pass on the unmutated copy first. When
#   it does not, the path is unresolved and none of its mutants is judged.
#   Operators, each applied to one line at a time; comment lines (first non-blank
#   character #) receive no mutant from any operator:
#     eq-ne        the first -eq/-ne or ==/!= on the line, swapped
#     and-or       the first && or || on the line, swapped
#     status-flip  the first "exit 0/1" or "return 0/1" on the line, swapped
#     drop-stmt    the whole line deleted, when it is a simple statement;
#                  control keywords and braces are never dropped
#   Verdict per mutant:
#     syntax check exceeds its bound  timeout, the oracle never runs
#     syntax check does not parse     error, the oracle never runs
#     oracle exceeds its bound        timeout
#     oracle exits 124                timeout, the oracle reported its own timeout
#     oracle exits 126 or 127         error, the oracle could not run
#     oracle exits 128 through 255    error, a signal stop or a stop code, not a verdict
#     oracle exits 1 through 123      killed
#     oracle exits 125                killed
#     oracle exits zero               survived
#   Exit 124 and the 128-255 range are never a verified kill: an oracle that
#   stops itself with them has not shown that the mutant changes behaviour.
#   Every bounded run is also capped by the whole-run budget. A run that trips
#   a bound, the budget, or a containment check names the phase, the bound, and
#   the reason in its record. A trip records timeout or error, never killed.
#   Only killed counts as killed.
#
# Containment (fm_mutation_run_bounded): every bounded command runs in its own
#   process group. Its group is verified before the command may run, so a failed
#   setup runs nothing. When the bound, the budget, or the owner's exit ends the
#   run, the whole group is stopped: TERM, a two-second grace, then KILL. A
#   group that survives KILL, or that is left running after the command exits,
#   is an error. A detached descendant that leaves the group is not reached; see
#   the runner's header for that limit.
#
# External strategies (mutmut-class, stryker-class), report-only:
#   Each names an external tool. This runner has no result reader for either
#   tool, so every path of that strategy is an error: the tool is missing, or it
#   is present and nothing was measured. Neither is green. A version probe that
#   trips its bound is recorded as timeout or error, on the tool row and on each
#   path of that strategy, never as installed.
#
# Callers set FM_MUTATION_SCRATCH, FM_MUTATION_FLAG, FM_MUTATION_DEADLINE (a
# SECONDS value), and FM_MUTATION_BUDGET before any bounded run. See
# fm_mutation_run_bounded for what it sets.

FM_MUTATION_SHELL_MUTATOR_VERSION=1
FM_MUTATION_SHELL_OPERATORS="eq-ne and-or status-flip drop-stmt"

# fm_mutation_strategy_known <strategy>
#   Returns 0 for a strategy token this runner can judge or report.
fm_mutation_strategy_known() {
  case "$1" in
    shell-custom-mutator|mutmut-class|stryker-class) return 0 ;;
    *) return 1 ;;
  esac
}

# fm_mutation_external_tool <strategy>
#   Prints the external tool name for an external strategy.
fm_mutation_external_tool() {
  case "$1" in
    mutmut-class) printf '%s\n' mutmut ;;
    stryker-class) printf '%s\n' stryker ;;
    *) return 1 ;;
  esac
}

# fm_mutation_tool_record <strategy> <scratch> <probe-seconds>
#   Prints the tool record for a strategy: its name, its availability, and its
#   version and configuration. Tools belong to a strategy, not to one path, so the
#   surface column is "-". A missing external tool is not-installed. The version
#   probe is a bounded run whose output goes to a file under <scratch>, never a
#   pipe, so a descendant that holds the output cannot stall the record. When the
#   probe trips, the external path records for this strategy are written to
#   <scratch>/probe-<strategy> as two lines: the status, then the detail.
fm_mutation_tool_record() {
  local tool version capture note detail
  case "$1" in
    shell-custom-mutator)
      printf 'tool\tfm-shell-mutator\t-\t-\tbuiltin\tversion %s; report-only; operators=%s; oracle bash %s\n' \
        "$FM_MUTATION_SHELL_MUTATOR_VERSION" "${FM_MUTATION_SHELL_OPERATORS// /,}" "$BASH_VERSION"
      ;;
    mutmut-class|stryker-class)
      tool=$(fm_mutation_external_tool "$1") || return 1
      if command -v "$tool" >/dev/null 2>&1; then
        capture="$2/version-$1"
        note="$2/probe-$1"
        fm_mutation_run_bounded "$3" "$2" "$capture" "$tool" --version
        if [ -n "$FM_MUTATION_TRIP" ]; then
          detail=$(fm_mutation_trip_detail "version probe" --probe-timeout)
          printf '%s\n%s; nothing was measured\n' "$(fm_mutation_trip_status)" "$detail" >"$note"
          printf 'tool\t%s\t-\t-\t%s\tversion unknown; %s; report-only; no result reader in this runner\n' \
            "$tool" "$(fm_mutation_trip_status)" "$detail"
        else
          version=$(head -n 1 "$capture" 2>/dev/null) || version=""
          [ -n "$version" ] || version=unknown
          printf 'tool\t%s\t-\t-\tinstalled\tversion %s; report-only; no result reader in this runner\n' \
            "$tool" "$version"
        fi
      else
        printf 'tool\t%s\t-\t-\tnot-installed\treport-only; no result reader in this runner\n' "$tool"
      fi
      ;;
    *) return 1 ;;
  esac
}

# fm_mutation_external_path <strategy> <path> <surface> <scratch>
#   Prints the path record for an external strategy. Never green: the tool is
#   either missing or present without a result reader, and both are errors. A
#   version probe that tripped its bound is reported with its own status.
fm_mutation_external_path() {
  local tool status detail
  tool=$(fm_mutation_external_tool "$1") || return 1
  if [ -f "$4/probe-$1" ]; then
    status=$(head -n 1 "$4/probe-$1")
    detail=$(tail -n +2 "$4/probe-$1")
    printf 'path\t%s\t%s\t-\t%s\t%s\n' "$2" "$3" "$status" "$detail"
  elif command -v "$tool" >/dev/null 2>&1; then
    printf 'path\t%s\t%s\t-\terror\t%s is installed, but this runner has no result reader for it; nothing was measured\n' \
      "$2" "$3" "$tool"
  else
    printf 'path\t%s\t%s\t-\terror\tmutation tool not installed: %s; nothing was measured\n' \
      "$2" "$3" "$tool"
  fi
}

# fm_mutation_oracle_for <path>
#   Prints the oracle path for a production script: tests/<basename>.test.sh,
#   relative to the tree root. Existence is the caller's check.
fm_mutation_oracle_for() {
  local base=${1##*/}
  printf 'tests/%s.test.sh\n' "${base%.sh}"
}

# fm_mutation_reap_group <pgid>
#   Stops every process in process group <pgid>: TERM, a two-second grace, then
#   KILL. Returns 0 when the group was already empty, 2 when this call stopped
#   members, and 1 when members survive KILL. A leader that has exited but has
#   not yet been waited for is a zombie, not a member, so it does not hold the
#   reap open.
fm_mutation_reap_group() {
  local pgid=$1 i=0
  kill -0 -- "-$pgid" 2>/dev/null || return 0
  kill -TERM -- "-$pgid" 2>/dev/null || true
  while [ "$i" -lt 20 ] && kill -0 -- "-$pgid" 2>/dev/null; do
    sleep 0.1
    i=$((i + 1))
  done
  if kill -0 -- "-$pgid" 2>/dev/null; then
    kill -KILL -- "-$pgid" 2>/dev/null || true
    i=0
    while [ "$i" -lt 20 ] && kill -0 -- "-$pgid" 2>/dev/null; do
      sleep 0.1
      i=$((i + 1))
    done
    kill -0 -- "-$pgid" 2>/dev/null && return 1
  fi
  return 2
}

# fm_mutation_trip_status
#   Prints the record status for the trip the last bounded run recorded: timeout
#   for a bound or the whole-run budget, error for every containment failure.
#   A timeout is never recorded as killed.
fm_mutation_trip_status() {
  case "${FM_MUTATION_TRIP:-}" in
    bound|budget) printf 'timeout\n' ;;
    *) printf 'error\n' ;;
  esac
}

# fm_mutation_budget_spent
#   Returns 0 once the whole-run deadline has passed, the same test that
#   fm_mutation_run_bounded uses. The tree copy and the judging loop check it
#   before each step, so a run stops at its budget instead of working through
#   the rest of the tree with every bounded call already tripped.
fm_mutation_budget_spent() {
  [ "$SECONDS" -ge "$FM_MUTATION_DEADLINE" ]
}

# fm_mutation_judging_stopped <path> <surface>
#   Writes the one path record for a judging loop that met the whole-run budget.
#   Its mutants already judged keep their records; the rest are not judged, so
#   the path is a timeout and never a verdict.
fm_mutation_judging_stopped() {
  printf 'path\t%s\t%s\t-\ttimeout\twhole-run budget of %ss (--budget) was exhausted; mutants not judged\n' \
    "$1" "$2" "$FM_MUTATION_BUDGET"
}

# fm_mutation_trip_detail <phase> <bound-option>
#   Prints why the last bounded run tripped, naming the phase, the bound and the
#   runner option that sets it, and the reason. Prints nothing when nothing
#   tripped.
fm_mutation_trip_detail() {
  case "${FM_MUTATION_TRIP:-}" in
    bound) printf '%s timed out after %ss (%s)' "$1" "$FM_MUTATION_SECS" "$2" ;;
    budget) printf '%s stopped: the whole-run budget of %ss (--budget) was exhausted' "$1" "$FM_MUTATION_BUDGET" ;;
    setup) printf '%s not run: its process group could not be set up, so nothing ran' "$1" ;;
    reap) printf '%s left processes that survived KILL in its process group; containment failed' "$1" ;;
    leftover) printf '%s exited but left processes in its process group; they were stopped' "$1" ;;
  esac
}

# fm_mutation_gated <gate> <owner> <dir> <command> [args...]
#   The body of a bounded process. It waits until <gate> exists, which the runner
#   creates only after the process group is verified and the watcher runs, then
#   execs command in <dir>. It exits at once when <owner> is gone before the gate
#   opens, so nothing starts for a runner that died.
fm_mutation_gated() {
  local gate=$1 owner=$2 dir=$3
  shift 3
  while [ ! -e "$gate" ]; do
    kill -0 "$owner" 2>/dev/null || exit 1
    sleep 0.05
  done
  cd "$dir" && exec "$@"
}

# fm_mutation_run_bounded <seconds> <dir> <capture> <command> [args...]
#   Runs command in <dir> in its own process group, with stdout to <capture> and
#   stderr to /dev/null. Stdin is /dev/null unless FM_MUTATION_INPUT_FD names an
#   open descriptor, which the command then reads: the runner uses this for the
#   classifier input, so that read is bounded and stoppable like any oracle. The
#   run stops at <seconds> or at the whole-run deadline FM_MUTATION_DEADLINE,
#   whichever is sooner. The
#   command is held at a gate until its group is verified and the watcher is
#   running, so nothing runs ungrouped or unwatched. The watcher stops the group
#   when the deadline passes or when this shell exits, even by SIGKILL, so a
#   killed runner cannot orphan a group. Sets FM_MUTATION_TRIP (empty when the
#   command ended on its own; otherwise bound, budget, setup, reap, or
#   leftover), FM_MUTATION_RC, FM_MUTATION_SECS, and FM_MUTATION_LAST_PID (the
#   group id, kept after the run as evidence). Always returns 0; read the
#   variables.
fm_mutation_run_bounded() {
  local secs=$1 dir=$2 capture=$3 owner=$$ left kind deadline gate pid watcher rc=0 reaped=0
  shift 3
  FM_MUTATION_TRIP=""
  FM_MUTATION_RC=0
  FM_MUTATION_SECS=$secs
  FM_MUTATION_LAST_PID=""
  left=$((FM_MUTATION_DEADLINE - SECONDS))
  if [ "$left" -le 0 ]; then
    FM_MUTATION_TRIP=budget
    return 0
  fi
  kind=bound
  if [ "$left" -lt "$secs" ]; then
    kind=budget
    FM_MUTATION_SECS=$left
  fi
  deadline=$((SECONDS + FM_MUTATION_SECS))
  gate="$FM_MUTATION_SCRATCH/gate"
  rm -f -- "$gate" "$FM_MUTATION_FLAG"

  set -m
  if [ -n "${FM_MUTATION_INPUT_FD:-}" ]; then
    fm_mutation_gated "$gate" "$owner" "$dir" "$@" <&"$FM_MUTATION_INPUT_FD" >"$capture" 2>/dev/null &
  else
    fm_mutation_gated "$gate" "$owner" "$dir" "$@" </dev/null >"$capture" 2>/dev/null &
  fi
  pid=$!
  set +m
  FM_MUTATION_PID=$pid
  # shellcheck disable=SC2034 # FM_MUTATION_LAST_PID is the documented output that callers read after a run.
  FM_MUTATION_LAST_PID=$pid
  if ! kill -0 -- "-$pid" 2>/dev/null; then
    kill -KILL "$pid" 2>/dev/null || true
    wait "$pid" 2>/dev/null || true
    FM_MUTATION_PID=""
    FM_MUTATION_TRIP=setup
    return 0
  fi

  set -m
  (
    while kill -0 "$owner" 2>/dev/null; do
      if [ "$SECONDS" -ge "$deadline" ]; then
        printf '%s\n' "$kind" >"$FM_MUTATION_FLAG"
        break
      fi
      sleep 0.1
    done
    fm_mutation_reap_group "$pid" >/dev/null || true
  ) </dev/null >/dev/null 2>&1 &
  watcher=$!
  set +m
  FM_MUTATION_WATCHER=$watcher
  : >"$gate"

  wait "$pid" 2>/dev/null || rc=$?
  if [ -e "$FM_MUTATION_FLAG" ]; then
    wait "$watcher" 2>/dev/null || true
    read -r FM_MUTATION_TRIP <"$FM_MUTATION_FLAG" || true
  else
    kill -KILL -- "-$watcher" 2>/dev/null || true
    wait "$watcher" 2>/dev/null || true
  fi
  FM_MUTATION_RC=$rc
  fm_mutation_reap_group "$pid" >/dev/null || reaped=$?
  case $reaped in
    1) FM_MUTATION_TRIP=reap ;;
    2) [ -n "$FM_MUTATION_TRIP" ] || FM_MUTATION_TRIP=leftover ;;
  esac
  FM_MUTATION_PID=""
  FM_MUTATION_WATCHER=""
  return 0
}

# fm_mutation_stop_active
#   Stops the process group and the watcher of the bounded run in progress, if
#   any. Idempotent. The runner calls it from its signal and exit traps, so an
#   interrupted run leaves no group behind.
fm_mutation_stop_active() {
  if [ -n "${FM_MUTATION_PID:-}" ]; then
    fm_mutation_reap_group "$FM_MUTATION_PID" >/dev/null || true
    wait "$FM_MUTATION_PID" 2>/dev/null || true
    FM_MUTATION_PID=""
  fi
  if [ -n "${FM_MUTATION_WATCHER:-}" ]; then
    kill -KILL -- "-$FM_MUTATION_WATCHER" 2>/dev/null || true
    wait "$FM_MUTATION_WATCHER" 2>/dev/null || true
    FM_MUTATION_WATCHER=""
  fi
  return 0
}

# fm_mutation_on_signal
#   Trap body for HUP, INT, and TERM: stops the active run and exits 1. Nothing
#   is published; the runner's exit trap then removes the scratch directory.
fm_mutation_on_signal() {
  fm_mutation_stop_active
  printf 'fm-mutation-run.sh: interrupted; the active oracle was stopped and no record was published\n' >&2
  exit 1
}

# fm_mutation_first_sub <line> <from> <to>
#   Prints <line> with the first literal occurrence of <from> replaced by <to>.
#   Returns 1 when <from> does not occur. Built from prefix and suffix removal,
#   not ${1/pattern/replacement}, because bash 3.2 keeps the quotes of a quoted
#   replacement inside double quotes and would write them into the mutant.
fm_mutation_first_sub() {
  local before after
  case "$1" in
    *"$2"*) ;;
    *) return 1 ;;
  esac
  before=${1%%"$2"*}
  after=${1#*"$2"}
  printf '%s%s%s\n' "$before" "$3" "$after"
}

# fm_mutation_is_comment <line>
#   Returns 0 when the first non-blank character of the line is #. A comment is
#   not code, so no operator mutates it.
fm_mutation_is_comment() {
  local trimmed
  trimmed=${1#"${1%%[![:space:]]*}"}
  case "$trimmed" in
    '#'*) return 0 ;;
  esac
  return 1
}

# fm_mutation_is_statement <line>
#   Returns 0 when the line is a simple statement that drop-stmt may delete.
fm_mutation_is_statement() {
  local trimmed word
  fm_mutation_is_comment "$1" && return 1
  trimmed=${1#"${1%%[![:space:]]*}"}
  case "$trimmed" in
    '') return 1 ;;
  esac
  word=${trimmed%%[[:space:]]*}
  case "$word" in
    if|elif|else|then|fi|for|while|until|do|done|case|'esac'|in|select|function) return 1 ;;
    '{'|'}'|'('|'(('|'[['|';;') return 1 ;;
  esac
  return 0
}

# fm_mutation_replace <operator> <line>
#   Prints the line after <operator>'s single replacement, or returns 1 when
#   the operator has no site on the line. drop-stmt is not a replacement; the
#   caller deletes the line instead.
fm_mutation_replace() {
  case "$1" in
    eq-ne)
      fm_mutation_first_sub "$2" ' -eq ' ' -ne ' ||
        fm_mutation_first_sub "$2" ' -ne ' ' -eq ' ||
        fm_mutation_first_sub "$2" ' == ' ' != ' ||
        fm_mutation_first_sub "$2" ' != ' ' == '
      ;;
    and-or)
      fm_mutation_first_sub "$2" '&&' '||' || fm_mutation_first_sub "$2" '||' '&&'
      ;;
    status-flip)
      fm_mutation_first_sub "$2" 'exit 0' 'exit 1' ||
        fm_mutation_first_sub "$2" 'exit 1' 'exit 0' ||
        fm_mutation_first_sub "$2" 'return 0' 'return 1' ||
        fm_mutation_first_sub "$2" 'return 1' 'return 0'
      ;;
    *) return 1 ;;
  esac
}

# fm_mutation_shell_run <scratch> <tree> <path> <surface> <timeout>
#   Judges every mutant of <path> inside the scratch <tree> and prints one
#   record per mutant, or one path record when nothing can be measured. Writes
#   only under <scratch>, and restores the mutated file after every mutant.
# The head and tail reads below take the original copy in <scratch>, and only
# the separate mutant file is written, so the same-file read and write warning
# does not apply.
# shellcheck disable=SC2094
fm_mutation_shell_run() {
  local scratch=$1 tree=$2 path=$3 surface=$4 timeout=$5 syntax=$6
  local src oracle orig mutant n=0 op line repl id sites=0

  src="$tree/$path"
  if [ -L "$src" ]; then
    printf 'path\t%s\t%s\t-\tunresolved\tsymbolic link; a mutant would be written through the link, so it is not judged\n' \
      "$path" "$surface"
    return 0
  fi
  if [ ! -f "$src" ]; then
    printf 'path\t%s\t%s\t-\tunresolved\tsource file not present in the tree\n' "$path" "$surface"
    return 0
  fi
  oracle=$(fm_mutation_oracle_for "$path")
  if [ ! -f "$tree/$oracle" ]; then
    printf 'path\t%s\t%s\t-\tunresolved\tno oracle: %s is absent, so nothing measures this script\n' \
      "$path" "$surface" "$oracle"
    return 0
  fi

  orig="$scratch/original"
  mutant="$scratch/mutant"
  cp "$src" "$orig" || return 1

  fm_mutation_run_bounded "$timeout" "$tree" /dev/null bash "$oracle"
  if [ -n "$FM_MUTATION_TRIP" ]; then
    printf 'path\t%s\t%s\t-\t%s\t%s; mutants not judged\n' "$path" "$surface" \
      "$(fm_mutation_trip_status)" "$(fm_mutation_trip_detail "baseline oracle $oracle" --timeout)"
    return 0
  fi
  if [ "$FM_MUTATION_RC" -ne 0 ]; then
    printf 'path\t%s\t%s\t-\tunresolved\tbaseline oracle %s exits %s on the unmutated script; mutants not judged\n' \
      "$path" "$surface" "$oracle" "$FM_MUTATION_RC"
    return 0
  fi

  while IFS= read -r line || [ -n "$line" ]; do
    if fm_mutation_budget_spent; then
      fm_mutation_judging_stopped "$path" "$surface"
      return 0
    fi
    n=$((n + 1))
    fm_mutation_is_comment "$line" && continue
    for op in $FM_MUTATION_SHELL_OPERATORS; do
      if fm_mutation_budget_spent; then
        fm_mutation_judging_stopped "$path" "$surface"
        return 0
      fi
      if [ "$op" = drop-stmt ]; then
        fm_mutation_is_statement "$line" || continue
        { head -n $((n - 1)) "$orig"; tail -n +$((n + 1)) "$orig"; } >"$mutant"
      else
        repl=$(fm_mutation_replace "$op" "$line") || continue
        { head -n $((n - 1)) "$orig"; printf '%s\n' "$repl"; tail -n +$((n + 1)) "$orig"; } >"$mutant"
      fi
      sites=$((sites + 1))
      id="L${n}-${op}"

      fm_mutation_run_bounded "$syntax" "$tree" /dev/null bash -n "$mutant"
      if [ -n "$FM_MUTATION_TRIP" ]; then
        printf 'mutant\t%s\t%s\t%s\t%s\t%s; oracle not run\n' "$path" "$surface" "$id" \
          "$(fm_mutation_trip_status)" "$(fm_mutation_trip_detail "syntax check" --syntax-timeout)"
        continue
      fi
      if [ "$FM_MUTATION_RC" -ne 0 ]; then
        printf 'mutant\t%s\t%s\t%s\terror\tmutant does not parse\n' "$path" "$surface" "$id"
        continue
      fi

      cp "$mutant" "$src" || return 1
      fm_mutation_run_bounded "$timeout" "$tree" /dev/null bash "$oracle"
      cp "$orig" "$src" || return 1

      if [ -n "$FM_MUTATION_TRIP" ]; then
        printf 'mutant\t%s\t%s\t%s\t%s\t%s\n' "$path" "$surface" "$id" \
          "$(fm_mutation_trip_status)" "$(fm_mutation_trip_detail "oracle $oracle" --timeout)"
        continue
      fi
      case "$FM_MUTATION_RC" in
        0) printf 'mutant\t%s\t%s\t%s\tsurvived\toracle passed\n' "$path" "$surface" "$id" ;;
        124) printf 'mutant\t%s\t%s\t%s\ttimeout\toracle exit 124 (its own timeout); not counted as killed\n' "$path" "$surface" "$id" ;;
        126|127) printf 'mutant\t%s\t%s\t%s\terror\toracle could not run (exit %s)\n' "$path" "$surface" "$id" "$FM_MUTATION_RC" ;;
        12[89]|1[3-9][0-9]|2[0-5][0-9]) printf 'mutant\t%s\t%s\t%s\terror\toracle exit %s is a signal stop or stop code, not a verdict; not counted as killed\n' "$path" "$surface" "$id" "$FM_MUTATION_RC" ;;
        *) printf 'mutant\t%s\t%s\t%s\tkilled\toracle exit %s\n' "$path" "$surface" "$id" "$FM_MUTATION_RC" ;;
      esac
    done
  done <"$orig"

  if [ "$sites" -eq 0 ]; then
    printf 'path\t%s\t%s\t-\tunresolved\tno mutation sites: no operator applies to any line\n' "$path" "$surface"
  fi
}
