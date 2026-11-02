#!/usr/bin/env bash
# bin/fm-mutation-run.sh - the mutation runner: judges one change and publishes a
# report-only evidence record. Input is the classifier's TSV on stdin
# (bin/fm-mutation-classify.sh); the record goes to stdout. Each row runs through
# the strategy the classifier assigned. The shell custom mutator judges every
# mutant inside a scratch copy. The external strategies (mutmut-class and
# stryker-class) are reported as errors, because this runner has no result reader
# for either tool. Nothing here fails or gates a PR check.
#
# Usage: fm-mutation-run.sh [--root <dir>] [--timeout <seconds>] [--budget <seconds>]
#                           [--probe-timeout <seconds>] [--syntax-timeout <seconds>] < classifier-tsv
#   --root            the git work tree to judge (default: the git repository of the current directory)
#   --timeout         bound for one oracle run (default: 300)
#   --budget          bound for the whole run, including the file listing, the tree copy, and judging (default: 1800)
#   --probe-timeout   bound for one external tool's version probe (default: 10)
#   --syntax-timeout  bound for one bash -n syntax check of a mutant (default: 10)
#   Every bound is in whole seconds, at least 1.
#
# Exit codes: 0 record published; 1 scratch directory, listing, or copy failed, or the
# run was interrupted; 2 usage or malformed input.
#
# Record contract (stdout; every record has six TAB-separated columns):
#   tool       <tool>  -  -  builtin|installed|not-installed|timeout|error  <version or configuration>
#   mutant     <path>  <surface>  L<n>-<operator>  killed|survived|timeout|error  <detail>
#   path       <path>  <surface>  -  timeout|error|unresolved  <reason>
#   exclusion  <path>  <surface>  -  unsupported  <reason from the classifier>
#   summary    -  -  -  published  killed=N survived=N timeout=N error=N unresolved=N excluded=N
#
# Only killed counts as killed. Timeouts and errors never count as killed, and an
# unresolved path is never recorded as passing. Path timeouts count toward timeout,
# path errors toward error, and unresolved paths toward unresolved.
#
# Bounds: a run that exceeds a bound is stopped with its whole process group and
# recorded as timeout, and its detail names the phase, the option, and the reason.
# A run that cannot be contained is recorded as error, never as killed. Every oracle's
# process group is stopped when the runner exits, even by a signal; a descendant that
# leaves that group is not reached, as the Limits below say.
# The classifier input is read through the same bounded reader, so a stalled read is
# stopped by the whole-run budget or a signal, and the run exits 1 with no record.
# Limits: bounds have whole-second resolution; a detached descendant that leaves its
# process group is not reached; a group left running after its command exits is an
# error; copying one file is not interrupted mid-call, so the budget is checked between
# files. The judging loop checks the budget before each source line and each mutant,
# so a path the budget ends during is recorded as timeout, mutants not judged.
#
# The classifier exits 1 when a path matches no surface class, and it still prints
# every classified row. This runner judges only the rows it is given, so a caller
# that publishes this record must check the classifier's exit status first.
#
# Every mutant is written only inside the scratch copy, which is removed on exit. A SIGKILL
# skips that removal, so each start removes the scratch of a dead run: a directory under
# TMPDIR named fm-mutation-run.* whose recorded owner PID no longer exists.
# The work tree is never written.

set -u

SCRIPT_DIR=$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)
# shellcheck source=bin/fm-mutation-exec-lib.sh
. "$SCRIPT_DIR/fm-mutation-exec-lib.sh"

TAB=$(printf '\t')
ROOT=""
TIMEOUT=300
BUDGET=1800
PROBE_TIMEOUT=10
SYNTAX_TIMEOUT=10

usage() {
  printf '%s\n' "usage: fm-mutation-run.sh [--root <dir>] [--timeout <seconds>] [--budget <seconds>]" \
    "       [--probe-timeout <seconds>] [--syntax-timeout <seconds>] < classifier-tsv"
}

refuse() {
  printf 'fm-mutation-run.sh: %s\n' "$*" >&2
  exit 2
}

# require_seconds <option> <value>
#   Refuses unless <value> is a whole number of seconds, at least 1 and at most nine
#   digits, so the arithmetic on it cannot overflow.
require_seconds() {
  case "$2" in
    ''|*[!0-9]*) refuse "$1 must be a whole number of seconds: $2" ;;
  esac
  [ "${#2}" -le 9 ] || refuse "$1 is too large: $2"
  [ "$2" -ge 1 ] || refuse "$1 must be at least 1 second"
}

die() {
  printf 'fm-mutation-run.sh: %s\n' "$*" >&2
  exit 1
}

# sweep_dead_scratch <base>
#   Removes the scratch directories a SIGKILLed run left in <base>. A run records its PID in
#   <scratch>/owner. A directory is removed only when it is a real directory owned by this
#   user, its owner file holds a PID, and that PID no longer exists. A live run's directory
#   is never touched. A PID reused by another process keeps the directory, which is a leak,
#   not a loss. No ps call is needed.
sweep_dead_scratch() {
  local base=$1 dir pid
  for dir in "$base"/fm-mutation-run.*; do
    [ -d "$dir" ] && [ ! -L "$dir" ] && [ -O "$dir" ] || continue
    [ -f "$dir/owner" ] || continue
    IFS= read -r pid <"$dir/owner" || continue
    case "$pid" in ''|*[!0-9]*) continue ;; esac
    kill -0 "$pid" 2>/dev/null && continue
    rm -rf -- "$dir"
  done
}

while [ "$#" -gt 0 ]; do
  case "$1" in
    --root)
      [ "$#" -ge 2 ] || refuse "--root needs a directory"
      ROOT=$2
      shift 2
      ;;
    --timeout)
      [ "$#" -ge 2 ] || refuse "--timeout needs a number of seconds"
      TIMEOUT=$2
      shift 2
      ;;
    --budget)
      [ "$#" -ge 2 ] || refuse "--budget needs a number of seconds"
      BUDGET=$2
      shift 2
      ;;
    --probe-timeout)
      [ "$#" -ge 2 ] || refuse "--probe-timeout needs a number of seconds"
      PROBE_TIMEOUT=$2
      shift 2
      ;;
    --syntax-timeout)
      [ "$#" -ge 2 ] || refuse "--syntax-timeout needs a number of seconds"
      SYNTAX_TIMEOUT=$2
      shift 2
      ;;
    -h|--help)
      usage
      exit 0
      ;;
    *)
      refuse "unknown argument: $1"
      ;;
  esac
done

require_seconds --timeout "$TIMEOUT"
require_seconds --budget "$BUDGET"
require_seconds --probe-timeout "$PROBE_TIMEOUT"
require_seconds --syntax-timeout "$SYNTAX_TIMEOUT"
# Decimal, so a leading zero is not read as octal.
TIMEOUT=$((10#$TIMEOUT))
BUDGET=$((10#$BUDGET))
PROBE_TIMEOUT=$((10#$PROBE_TIMEOUT))
SYNTAX_TIMEOUT=$((10#$SYNTAX_TIMEOUT))

[ -n "$ROOT" ] || ROOT=.
[ -d "$ROOT" ] || refuse "--root is not a directory: $ROOT"
TOP=$(git -C "$ROOT" rev-parse --show-toplevel 2>/dev/null) || refuse "not inside a git work tree: $ROOT"

# A SIGKILLed run cannot remove its scratch, so each start sweeps the scratch of dead runs.
sweep_dead_scratch "${TMPDIR:-/tmp}"
SCRATCH=$(mktemp -d "${TMPDIR:-/tmp}/fm-mutation-run.XXXXXX") || die "could not create a scratch directory"
trap 'fm_mutation_stop_active; rm -rf -- "$SCRATCH"' EXIT
printf '%s\n' "$$" >"$SCRATCH/owner" || die "could not record the scratch owner"
trap 'fm_mutation_on_signal' HUP INT TERM
FM_MUTATION_SCRATCH=$SCRATCH
FM_MUTATION_FLAG="$SCRATCH/bound-trip"
FM_MUTATION_BUDGET=$BUDGET
FM_MUTATION_DEADLINE=$((SECONDS + BUDGET))

# Buffer the classifier input through the bounded reader. A foreground cat would hold a
# TERM until its input ends, and a SIGKILLed runner would leave it reading. The reader
# runs in its own group under the whole-run budget, so the signal handler and the deadline
# both stop it, and it cannot outlive the runner.
exec 4<&0 || die "could not read the classifier input"
FM_MUTATION_INPUT_FD=4
fm_mutation_run_bounded "$BUDGET" "$SCRATCH" "$SCRATCH/input" cat
FM_MUTATION_INPUT_FD=""
exec 4<&-
case "$FM_MUTATION_TRIP" in
  '') ;;
  bound | budget) die "classifier input was not read to its end within the whole-run budget of ${BUDGET}s" ;;
  *) die "could not contain the classifier reader: $FM_MUTATION_TRIP" ;;
esac
[ "$FM_MUTATION_RC" -eq 0 ] || die "could not buffer the classifier input"

# Validate every row before any judging starts, so malformed input publishes nothing.
LINES=0
NEED_SHELL=0
NEED_MUTMUT=0
NEED_STRYKER=0
while IFS= read -r -u 3 line || [ -n "$line" ]; do
  [ -n "$line" ] || continue
  LINES=$((LINES + 1))
  IFS=$TAB read -r path surface disposition detail <<<"$line"
  [ "$path$TAB$surface$TAB$disposition$TAB$detail" = "$line" ] ||
    refuse "classifier line $LINES does not have four TAB-separated fields"
  case "$detail" in
    *"$TAB"*) refuse "classifier line $LINES has a TAB inside its detail" ;;
  esac
  case "$path" in
    ''|/*|*..*) refuse "classifier line $LINES has an empty, absolute, or parent-directory path: $path" ;;
  esac
  [ -n "$surface" ] || refuse "classifier line $LINES has no surface"
  case "$disposition" in
    unsupported)
      [ -n "$detail" ] || refuse "classifier line $LINES is unsupported without a stated reason"
      ;;
    mutate)
      strategy=${detail%% *}
      fm_mutation_strategy_known "$strategy" ||
        refuse "classifier line $LINES names an unknown strategy: $strategy"
      case "$strategy" in
        shell-custom-mutator) NEED_SHELL=1 ;;
        mutmut-class) NEED_MUTMUT=1 ;;
        stryker-class) NEED_STRYKER=1 ;;
      esac
      ;;
    *)
      refuse "classifier line $LINES has an unknown disposition: $disposition"
      ;;
  esac
done 3<"$SCRATCH/input"
[ "$LINES" -gt 0 ] || refuse "no classifier lines on stdin"

# Copy the work tree's files into the scratch tree. The listing runs under the
# whole-run budget, and the budget is checked before each file is copied. A copy
# stopped by the budget leaves the tree incomplete, so no mutant is judged on it;
# each mutate path records why instead. One file's cp already started is not
# interrupted, so a single very large file can finish past the budget. Deleted
# tracked files are skipped so one missing file cannot fail the copy, and symbolic
# links are kept as links.
TREE="$SCRATCH/tree"
mkdir "$TREE" || die "could not create the scratch tree"
TREE_COMPLETE=1
TREE_STATUS=""
TREE_NOTE=""
fm_mutation_run_bounded "$BUDGET" "$TOP" "$SCRATCH/files" git ls-files -z -c -o --exclude-standard
if [ -n "$FM_MUTATION_TRIP" ]; then
  TREE_COMPLETE=0
  TREE_STATUS=$(fm_mutation_trip_status)
  TREE_NOTE=$(fm_mutation_trip_detail "work tree listing" --budget)
elif [ "$FM_MUTATION_RC" -ne 0 ]; then
  die "could not list the work tree files"
fi
if [ "$TREE_COMPLETE" -eq 1 ]; then
  while IFS= read -r -d '' f; do
    if fm_mutation_budget_spent; then
      FM_MUTATION_TRIP=budget
      TREE_COMPLETE=0
      TREE_STATUS=$(fm_mutation_trip_status)
      TREE_NOTE=$(fm_mutation_trip_detail "tree copy" --budget)
      break
    fi
    if [ -f "$TOP/$f" ] || [ -L "$TOP/$f" ]; then
      case "$f" in
        */*) mkdir -p "$TREE/${f%/*}" || die "could not create a scratch directory for $f" ;;
      esac
      cp -P "$TOP/$f" "$TREE/$f" || die "could not copy $f into the scratch tree"
    fi
  done <"$SCRATCH/files"
fi

RECORDS="$SCRATCH/records"
: >"$RECORDS" || die "could not create the record file"

if [ "$NEED_SHELL" -eq 1 ]; then
  fm_mutation_tool_record shell-custom-mutator >>"$RECORDS"
fi
if [ "$NEED_MUTMUT" -eq 1 ]; then
  fm_mutation_tool_record mutmut-class "$SCRATCH" "$PROBE_TIMEOUT" >>"$RECORDS"
fi
if [ "$NEED_STRYKER" -eq 1 ]; then
  fm_mutation_tool_record stryker-class "$SCRATCH" "$PROBE_TIMEOUT" >>"$RECORDS"
fi

while IFS= read -r -u 3 line || [ -n "$line" ]; do
  [ -n "$line" ] || continue
  IFS=$TAB read -r path surface disposition detail <<<"$line"
  if [ "$disposition" = unsupported ]; then
    printf 'exclusion\t%s\t%s\t-\tunsupported\t%s\n' "$path" "$surface" "$detail" >>"$RECORDS"
    continue
  fi
  strategy=${detail%% *}
  case "$strategy" in
    shell-custom-mutator)
      if [ "$TREE_COMPLETE" -eq 0 ]; then
        printf 'path\t%s\t%s\t-\t%s\t%s; mutants not judged\n' "$path" "$surface" "$TREE_STATUS" "$TREE_NOTE" >>"$RECORDS"
      else
        fm_mutation_shell_run "$SCRATCH" "$TREE" "$path" "$surface" "$TIMEOUT" "$SYNTAX_TIMEOUT" >>"$RECORDS" ||
          die "could not judge $path"
      fi
      ;;
    mutmut-class|stryker-class)
      fm_mutation_external_path "$strategy" "$path" "$surface" "$SCRATCH" >>"$RECORDS"
      ;;
  esac
done 3<"$SCRATCH/input"

COUNTS=$(awk -F "$TAB" '
  $1 == "mutant" || $1 == "path" { status[$5]++ }
  $1 == "exclusion" { excluded++ }
  END {
    printf "killed=%d survived=%d timeout=%d error=%d unresolved=%d excluded=%d",
      status["killed"] + 0, status["survived"] + 0, status["timeout"] + 0,
      status["error"] + 0, status["unresolved"] + 0, excluded + 0
  }' "$RECORDS") || die "could not count the evidence records"
printf 'summary\t-\t-\t-\tpublished\t%s\n' "$COUNTS" >>"$RECORDS"
cat "$RECORDS"
exit 0
