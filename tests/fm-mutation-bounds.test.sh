#!/usr/bin/env bash
# tests/fm-mutation-bounds.test.sh - containment and bounds of the mutation runner
# (bin/fm-mutation-run.sh over bin/fm-mutation-exec-lib.sh). Every phase the runner
# starts is bounded, every process group it starts is stopped or recorded as left
# running, and a tripped bound is recorded as timeout or error, never as killed.
# Each adversarial case prints evidence: the PID and process group of the bounded
# process, its process state, every member of its group while it ran, the time taken
# to settle, and whether any member remained afterwards. A detached descendant leaves
# its group by design, so this suite does not run that case; see case 10.
#
# Bounds are whole seconds (the shell's SECONDS counter), so a bound of N trips between
# N-1 and N seconds after its process starts. A case that must see its bounded process
# start uses at least 3 seconds, so process startup cannot race the bound.
set -u

# shellcheck source=tests/lib.sh
. "$(dirname "${BASH_SOURCE[0]}")/lib.sh"

RUN="$ROOT/bin/fm-mutation-run.sh"
LIB="$ROOT/bin/fm-mutation-exec-lib.sh"
TMP=$(fm_test_tmproot fm-mutation-bounds)
TAB=$(printf '\t')
RUNNER_TMP="$TMP/runner-tmp"
GROUP_LIST="$TMP/groups"
PS=$(command -v ps) || { printf 'not ok - ps is required to read process state\n' >&2; exit 1; }
export FMB_PS=$PS
mkdir -p "$RUNNER_TMP"
: >"$GROUP_LIST"

# note_group <pgid>: lists a process group this test made, for the failure backstop.
note_group() { printf '%s\n' "$1" >>"$GROUP_LIST"; }

# Backstop for failure exits only. A passing run has already settled every group it
# started, so it signals nothing; a failing run stops any group still listed, because
# a group left running would outlive the test.
cleanup() {
  local rc=$? g
  if [ "$rc" -ne 0 ]; then
    while read -r g; do
      [ -n "$g" ] || continue
      kill -0 -- "-$g" 2>/dev/null && kill -KILL -- "-$g" 2>/dev/null
    done <"$GROUP_LIST"
    # A stalled classifier input has no end. Its producer must stop, or a reader left
    # behind would keep it open after the test exits.
    [ -z "${PRODUCER:-}" ] || kill -KILL "$PRODUCER" 2>/dev/null
  fi
  fm_test_cleanup
}
trap cleanup EXIT
trap 'exit 130' INT
trap 'exit 143' TERM
trap 'exit 129' HUP
trap 'exit 131' QUIT

# A curated tool directory holds the runner's own tools and no mutation tool. ps is
# left out on purpose: oracles and fakes reach it by absolute path through FMB_PS.
NOTOOLS="$TMP/tools"
mkdir -p "$NOTOOLS"
for tool in bash env git awk sed cp rm mkdir head tail cat cmp cksum find sort sleep mktemp dirname chmod wc tr; do
  tool_path=$(command -v "$tool") || continue
  case "$tool_path" in
    /*) ln -s "$tool_path" "$NOTOOLS/$tool" ;;
  esac
done

# row <field>...: one record line, its fields joined by TAB.
row() {
  local out="" sep="" field
  for field in "$@"; do
    out="$out$sep$field"
    sep=$TAB
  done
  printf '%s\n' "$out"
}

# proc_stat <pid>: the state ps reports for the process; empty when none is listed.
proc_stat() { "$PS" -o stat= -p "$1" 2>/dev/null | tr -d ' '; }

# members_pids <pgid>: "pid(state)" for each process in the group, or "none".
members_pids() {
  "$PS" -axo pid=,pgid=,stat= 2>/dev/null |
    awk -v g="$1" '$2 == g { s = s (s == "" ? "" : " ") $1 "(" $3 ")" } END { print (s == "" ? "none" : s) }'
}

# evidence <case> <phase> <pid> <pgid> [note]: one line of process evidence. The state
# is gone when ps no longer lists the PID; Z marks a zombie not yet collected.
evidence() {
  local state note=${5:-}
  state=$(proc_stat "$3")
  [ -n "$state" ] || state=gone
  printf 'evidence: case=%s phase=%s pid=%s pgid=%s state=%s group=%s%s\n' \
    "$1" "$2" "$3" "$4" "$state" "$(members_pids "$4")" "${note:+ $note}"
}

# gone <pid> <seconds>: 0 once ps no longer lists the process within the window.
# Sets WAITED to the tenths of a second spent waiting.
gone() {
  local i=0 max=$(($2 * 10))
  while [ -n "$(proc_stat "$1")" ]; do
    if [ "$i" -ge "$max" ]; then
      WAITED=$i
      return 1
    fi
    sleep 0.1
    i=$((i + 1))
  done
  WAITED=$i
  return 0
}

# settle <case> <pid> <pgid> <seconds>: waits up to the window for the process to be
# gone, prints its evidence with the time taken, and returns 0 once gone.
settle() {
  local case=$1 pid=$2 pgid=$3 rc=0
  gone "$pid" "$4" || rc=$?
  evidence "$case" settled "$pid" "$pgid" "waited=$((WAITED * 100))ms"
  return "$rc"
}

# await_file <file> <seconds>: 0 once the file holds a line.
await_file() {
  local i=0 max=$(($2 * 10))
  while [ ! -s "$1" ]; do
    [ "$i" -ge "$max" ] && return 1
    sleep 0.1
    i=$((i + 1))
  done
}

# start_runner <dir> <search-path> <checkout> <classifier> [runner args...]
# Starts the runner in the background with its record in <dir>/record and its
# stderr in <dir>/stderr. The classifier arrives on stdin, as in production. Sets RP
# to the runner's PID.
start_runner() {
  local dir=$1 search=$2 checkout=$3 cls=$4
  shift 4
  mkdir -p "$dir"
  TMPDIR="$RUNNER_TMP" PATH="$search" /bin/bash "$RUN" --root "$checkout" "$@" \
    <"$cls" >"$dir/record" 2>"$dir/stderr" &
  RP=$!
}

# await_start <label> <pid-file> <dir>: waits for the bounded process to write its
# PID file. On timeout it stops the runner and fails with the runner's own stderr.
await_start() {
  if ! await_file "$2" 10; then
    kill -KILL "$RP" 2>/dev/null
    fail "$1: the bounded process never started; runner stderr: $(cat "$3/stderr" 2>/dev/null); record: $(cat "$3/record" 2>/dev/null); case dir: $(find "$3" -mindepth 1 -maxdepth 1 -print 2>/dev/null | tr '\n' ' ')"
  fi
  read -r OPID OPGID <"$2"
  note_group "$OPGID"
}

# finish_runner <label> <seconds>: waits for the runner to exit and sets RC to its
# status. A runner still running after the window is killed and fails the test.
finish_runner() {
  local i=0 max=$(($2 * 10)) state
  while :; do
    state=$(proc_stat "$RP")
    case "$state" in '' | Z*) break ;; esac
    if [ "$i" -ge "$max" ]; then
      kill -KILL "$RP" 2>/dev/null
      fail "$1: the runner (pid $RP) was still running $2s after its case ended"
    fi
    sleep 0.1
    i=$((i + 1))
  done
  wait "$RP" 2>/dev/null
  RC=$?
}

# new_checkout <dir>: a git checkout with one production shell script holding a single
# simple statement, so exactly one mutant exists, and one Python script for the
# external-tool path.
new_checkout() {
  mkdir -p "$1/bin" "$1/tests"
  printf '%s\n' '#!/usr/bin/env bash' 'echo demo' >"$1/bin/demo.sh"
  printf '%s\n' 'print("demo")' >"$1/bin/demo.py"
  git -C "$1" init -q || fail "could not create the fixture checkout $1"
}

# write_oracle <checkout> <body-line>...: the oracle first writes its PID and its
# process group to $FMB_PIDF, then runs the body lines. The body reaches the file
# unexpanded, so it runs only when the oracle does.
write_oracle() {
  local file="$1/tests/demo.test.sh"
  shift
  {
    printf '%s\n' '#!/usr/bin/env bash'
    # shellcheck disable=SC2016 # the oracle body expands when the oracle runs, not here.
    printf '%s\n' 'echo "$$ $("$FMB_PS" -o pgid= -p $$)" >"$FMB_PIDF"'
    printf '%s\n' "$@"
  } >"$file"
  chmod +x "$file"
}

# Case 1: a CPU loop as the baseline oracle, stopped by --timeout.
D="$TMP/cpu-loop"
new_checkout "$D/checkout"
write_oracle "$D/checkout" 'while :; do :; done'
row bin/demo.sh shell mutate "shell-custom-mutator (report-only)" >"$D/classified"
export FMB_PIDF="$D/pid"
start_runner "$D" "$NOTOOLS" "$D/checkout" "$D/classified" --timeout 3
await_start cpu-loop "$FMB_PIDF" "$D"
evidence cpu-loop running "$OPID" "$OPGID"
assert_equals "$OPID" "$OPGID" "cpu-loop: the oracle must lead its own process group"
finish_runner cpu-loop 30
RECORD=$(cat "$D/record")
assert_contains "$RECORD" "$(row path bin/demo.sh shell - timeout "baseline oracle tests/demo.test.sh timed out after 3s (--timeout); mutants not judged")" "cpu-loop: a bounded baseline must record a timeout naming its phase and option"
assert_not_contains "$RECORD" "${TAB}killed${TAB}" "cpu-loop: a timed-out run must never be recorded as killed"
settle cpu-loop "$OPID" "$OPGID" 5 || fail "cpu-loop: the oracle outlived its bound (pid $OPID)"
assert_equals none "$(members_pids "$OPGID")" "cpu-loop: no process may remain in the oracle's group"
pass "cpu-loop: stopped at its bound, recorded as a timeout, no process left"

# Case 2: a grandchild that outlives the probe. Its only descriptor on the probe's
# output is the capture file, never the runner's own pipe, because every bounded
# process is redirected away from it. The record is read through a real pipe, so a
# grandchild that held the pipe would stall the read; the record must arrive promptly.
D="$TMP/grandchild-output"
new_checkout "$D/checkout"
mkdir -p "$D/fake-bin"
cat >"$D/fake-bin/mutmut" <<'EOF'
#!/usr/bin/env bash
sleep 300 &
echo "$$ $! $("$FMB_PS" -o pgid= -p $$)" >"$FMB_PIDF"
echo "mutmut 3.1.0"
EOF
chmod +x "$D/fake-bin/mutmut"
row bin/demo.py python mutate mutmut-class >"$D/classified"
export FMB_PIDF="$D/pid"
START=$SECONDS
TMPDIR="$RUNNER_TMP" PATH="$D/fake-bin:$NOTOOLS" \
  "$(command -v perl)" -e 'alarm(shift); exec @ARGV' 60 /bin/bash "$RUN" \
  --root "$D/checkout" --probe-timeout 10 <"$D/classified" | cat >"$D/record" 2>"$D/stderr"
ELAPSED=$((SECONDS - START))
RECORD=$(cat "$D/record")
read -r LEAD CHILD GPGID <"$FMB_PIDF"
note_group "$GPGID"
evidence grandchild-output after "$LEAD" "$GPGID"
evidence grandchild-output after "$CHILD" "$GPGID"
[ "$ELAPSED" -lt 30 ] || fail "grandchild-output: the record took ${ELAPSED}s, so a grandchild stalled it"
assert_contains "$RECORD" "version probe exited but left processes in its process group; they were stopped" "grandchild-output: a probe that leaves a grandchild must be recorded as left processes"
assert_not_contains "$RECORD" "${TAB}killed${TAB}" "grandchild-output: a leftover group must never be recorded as killed"
settle grandchild-output "$LEAD" "$GPGID" 5 || fail "grandchild-output: the probe leader outlived its run (pid $LEAD)"
settle grandchild-output "$CHILD" "$GPGID" 5 || fail "grandchild-output: the grandchild outlived its run (pid $CHILD)"
assert_equals none "$(members_pids "$GPGID")" "grandchild-output: no process may remain in the probe's group"
pass "grandchild-output: record arrived in ${ELAPSED}s through a pipe, grandchild stopped"

# Case 3: a child that ignores TERM, so only the KILL escalation can stop it.
D="$TMP/term-resistant"
new_checkout "$D/checkout"
write_oracle "$D/checkout" "trap '' TERM" 'sleep 300'
row bin/demo.sh shell mutate "shell-custom-mutator (report-only)" >"$D/classified"
export FMB_PIDF="$D/pid"
start_runner "$D" "$NOTOOLS" "$D/checkout" "$D/classified" --timeout 3
await_start term-resistant "$FMB_PIDF" "$D"
sleep 0.5
evidence term-resistant running "$OPID" "$OPGID"
START=$SECONDS
finish_runner term-resistant 30
ELAPSED=$((SECONDS - START))
RECORD=$(cat "$D/record")
assert_contains "$RECORD" "$(row path bin/demo.sh shell - timeout "baseline oracle tests/demo.test.sh timed out after 3s (--timeout); mutants not judged")" "term-resistant: a TERM-ignoring oracle must still record a timeout"
assert_not_contains "$RECORD" "${TAB}killed${TAB}" "term-resistant: a TERM-resistant timeout must never be recorded as killed"
settle term-resistant "$OPID" "$OPGID" 5 || fail "term-resistant: the TERM-ignoring oracle survived KILL (pid $OPID)"
assert_equals none "$(members_pids "$OPGID")" "term-resistant: no process may remain in the oracle's group"
pass "term-resistant: TERM ignored, KILL escalation stopped it in ${ELAPSED}s, no process left"

# Case 4: the runner is SIGKILLed while its oracle runs. Nothing in the runner can
# clean up, so the watcher must notice the owner's death and stop the group.
D="$TMP/early-parent-exit"
new_checkout "$D/checkout"
write_oracle "$D/checkout" 'while :; do :; done'
row bin/demo.sh shell mutate "shell-custom-mutator (report-only)" >"$D/classified"
export FMB_PIDF="$D/pid"
start_runner "$D" "$NOTOOLS" "$D/checkout" "$D/classified" --timeout 300 --budget 300
await_start early-parent-exit "$FMB_PIDF" "$D"
evidence early-parent-exit running "$OPID" "$OPGID"
kill -KILL "$RP"
finish_runner early-parent-exit 10
evidence early-parent-exit at-runner-exit "$OPID" "$OPGID"
settle early-parent-exit "$OPID" "$OPGID" 10 || fail "early-parent-exit: the oracle outlived its SIGKILLed runner (pid $OPID)"
assert_equals none "$(members_pids "$OPGID")" "early-parent-exit: no process may remain in the oracle's group"
pass "early-parent-exit: runner SIGKILLed, watcher stopped the oracle group, no process left"

# Case 5: the runner is cancelled with TERM. Its signal handler must stop the active
# group and collect it before the runner exits, so the oracle is gone at exit.
D="$TMP/runner-cancellation"
new_checkout "$D/checkout"
write_oracle "$D/checkout" 'while :; do :; done'
row bin/demo.sh shell mutate "shell-custom-mutator (report-only)" >"$D/classified"
export FMB_PIDF="$D/pid"
start_runner "$D" "$NOTOOLS" "$D/checkout" "$D/classified" --timeout 300 --budget 300
await_start runner-cancellation "$FMB_PIDF" "$D"
evidence runner-cancellation running "$OPID" "$OPGID"
kill -TERM "$RP"
finish_runner runner-cancellation 30
evidence runner-cancellation at-runner-exit "$OPID" "$OPGID"
assert_equals 1 "$RC" "runner-cancellation: a cancelled runner must exit 1"
assert_contains "$(cat "$D/stderr")" "interrupted" "runner-cancellation: the cancellation must be named on stderr"
[ ! -s "$D/record" ] || fail "runner-cancellation: a cancelled run must publish no record"
settle runner-cancellation "$OPID" "$OPGID" 0 || fail "runner-cancellation: the oracle was still alive when the runner exited (pid $OPID)"
assert_equals none "$(members_pids "$OPGID")" "runner-cancellation: no process may remain in the oracle's group at exit"
pass "runner-cancellation: TERM stopped the oracle before the runner exited, no record published"

# Case 6: the version probe of an external tool hangs. The probe must be stopped at
# --probe-timeout and recorded as a timeout, never as an installed-and-measured tool.
D="$TMP/hanging-probe"
new_checkout "$D/checkout"
mkdir -p "$D/fake-bin"
cat >"$D/fake-bin/mutmut" <<'EOF'
#!/usr/bin/env bash
echo "$$ $("$FMB_PS" -o pgid= -p $$)" >"$FMB_PIDF"
sleep 300
EOF
chmod +x "$D/fake-bin/mutmut"
row bin/demo.py python mutate mutmut-class >"$D/classified"
export FMB_PIDF="$D/pid"
start_runner "$D" "$D/fake-bin:$NOTOOLS" "$D/checkout" "$D/classified" --probe-timeout 3
await_start hanging-probe "$FMB_PIDF" "$D"
evidence hanging-probe running "$OPID" "$OPGID"
START=$SECONDS
finish_runner hanging-probe 30
ELAPSED=$((SECONDS - START))
RECORD=$(cat "$D/record")
assert_contains "$RECORD" "$(row tool mutmut - - timeout "version unknown; version probe timed out after 3s (--probe-timeout); report-only; no result reader in this runner")" "hanging-probe: a hung version probe must be recorded as a timeout on the tool row"
assert_not_contains "$RECORD" "tool${TAB}mutmut${TAB}-${TAB}-${TAB}installed" "hanging-probe: a probe that timed out must never leave its tool row reading installed"
assert_not_contains "$RECORD" "${TAB}killed${TAB}" "hanging-probe: a hung probe must never be recorded as killed"
settle hanging-probe "$OPID" "$OPGID" 5 || fail "hanging-probe: the probe outlived its bound (pid $OPID)"
assert_equals none "$(members_pids "$OPGID")" "hanging-probe: no process may remain in the probe's group"
pass "hanging-probe: stopped at --probe-timeout in ${ELAPSED}s, recorded as timeout, no process left"

# Case 7: the whole-run budget runs out during a baseline whose own bound is far
# larger. The budget must win, and the record must name it as the budget.
D="$TMP/budget"
new_checkout "$D/checkout"
write_oracle "$D/checkout" 'while :; do :; done'
row bin/demo.sh shell mutate "shell-custom-mutator (report-only)" >"$D/classified"
export FMB_PIDF="$D/pid"
start_runner "$D" "$NOTOOLS" "$D/checkout" "$D/classified" --timeout 300 --budget 3
await_start budget "$FMB_PIDF" "$D"
evidence budget running "$OPID" "$OPGID"
finish_runner budget 30
RECORD=$(cat "$D/record")
assert_contains "$RECORD" "$(row path bin/demo.sh shell - timeout "baseline oracle tests/demo.test.sh stopped: the whole-run budget of 3s (--budget) was exhausted; mutants not judged")" "budget: an exhausted whole-run budget must record a timeout naming the budget"
assert_not_contains "$RECORD" "${TAB}killed${TAB}" "budget: a budget stop must never be recorded as killed"
settle budget "$OPID" "$OPGID" 5 || fail "budget: the oracle outlived the whole-run budget (pid $OPID)"
assert_equals none "$(members_pids "$OPGID")" "budget: no process may remain in the oracle's group"
pass "budget: whole-run budget stopped the oracle, recorded as a timeout, no process left"

# Case 8: the syntax check of a mutant hangs. The mutant's oracle must never run, and
# the stopped syntax check must be recorded as a timeout, not as a killed mutant. The
# shim stands in for bash only for the syntax-check call; every other call runs bash.
D="$TMP/syntax-bound"
new_checkout "$D/checkout"
# shellcheck disable=SC2016 # the oracle body expands when the oracle runs, not here.
write_oracle "$D/checkout" 'echo ran >>"$FMB_RAN"'
mkdir -p "$D/shim"
cat >"$D/shim/bash" <<'EOF'
#!/bin/sh
if [ "$1" = "-n" ]; then
  echo "$$" >"$FMB_SYNPID"
  exec sleep 300
fi
exec /bin/bash "$@"
EOF
chmod +x "$D/shim/bash"
row bin/demo.sh shell mutate "shell-custom-mutator (report-only)" >"$D/classified"
export FMB_PIDF="$D/pid" FMB_SYNPID="$D/syntax-pid" FMB_RAN="$D/oracle-runs"
start_runner "$D" "$D/shim:$NOTOOLS" "$D/checkout" "$D/classified" --syntax-timeout 3
await_file "$FMB_SYNPID" 30 || { kill -KILL "$RP" 2>/dev/null; fail "syntax-bound: the syntax check never started; runner stderr: $(cat "$D/stderr" 2>/dev/null)"; }
read -r SPID <"$FMB_SYNPID"
note_group "$SPID"
evidence syntax-bound running "$SPID" "$SPID"
finish_runner syntax-bound 60
RECORD=$(cat "$D/record")
assert_contains "$RECORD" "timed out after 3s (--syntax-timeout); oracle not run" "syntax-bound: a hung syntax check must be recorded as a timeout and say the oracle did not run"
assert_not_contains "$RECORD" "${TAB}killed${TAB}" "syntax-bound: a hung syntax check must never be recorded as killed"
assert_equals 1 "$(wc -l <"$D/oracle-runs" | tr -d ' ')" "syntax-bound: only the baseline oracle may run; the mutant's oracle must not"
settle syntax-bound "$SPID" "$SPID" 5 || fail "syntax-bound: the syntax check outlived its bound (pid $SPID)"
assert_equals none "$(members_pids "$SPID")" "syntax-bound: no process may remain in the syntax check's group"
pass "syntax-bound: hung syntax check stopped, recorded as timeout, mutant oracle never ran"

# Case 9: the process group cannot be set up. The lib must run nothing ungrouped: the
# command never starts, the trip is setup, and no group is left behind. Shadowing set
# disables set -m, which is the only way the group is ever created.
SETUP="$TMP/setup-failure"
mkdir -p "$SETUP"
# The set shadow is the failure seam: with set -m disabled the lib never creates the
# group. The globals below are read by the lib, so shellcheck cannot see their use.
# shellcheck disable=SC2329,SC2034
SETUP_OUT=$(
  set() { :; }
  # shellcheck source=bin/fm-mutation-exec-lib.sh
  . "$LIB"
  FM_MUTATION_SCRATCH=$SETUP
  FM_MUTATION_FLAG="$SETUP/flag"
  FM_MUTATION_BUDGET=30
  FM_MUTATION_DEADLINE=$((SECONDS + 30))
  # shellcheck disable=SC2016 # the command is run by sh -c; $1 is its own argument.
  fm_mutation_run_bounded 5 "$SETUP" /dev/null sh -c ': >"$1"' sh "$SETUP/ran"
  printf 'trip=%s\n' "$FM_MUTATION_TRIP"
  printf 'pgid=%s\n' "$FM_MUTATION_LAST_PID"
  printf 'status=%s\n' "$(fm_mutation_trip_status)"
  printf 'detail=%s\n' "$(fm_mutation_trip_detail 'oracle tests/demo.test.sh' --timeout)"
)
SETUP_PGID=$(printf '%s\n' "$SETUP_OUT" | sed -n 's/^pgid=//p')
assert_contains "$SETUP_OUT" "trip=setup" "setup-failure: an ungroupable command must trip setup"
assert_contains "$SETUP_OUT" "status=error" "setup-failure: a setup failure must be recorded as an error, not a timeout or a kill"
assert_contains "$SETUP_OUT" "process group could not be set up, so nothing ran" "setup-failure: the record must say nothing ran"
[ ! -e "$SETUP/ran" ] || fail "setup-failure: the command ran although its group was never set up"
evidence setup-failure after "$SETUP_PGID" "$SETUP_PGID"
assert_equals none "$(members_pids "$SETUP_PGID")" "setup-failure: no process may remain in the abandoned group"
pass "setup-failure: ungroupable command never ran, recorded as error, no process left"

# Case 10: a detached descendant. Once it calls setsid it is outside every process
# group the runner can reach, so this suite cannot contain it on any host. It is not run
# here and no pass is claimed for it. A disposable-container probe ran it once; its
# descendant survived the runner with its own process group and session (see
# data/dpes-822-bound-execution/detached-descendant-container-run1.out), so the residual
# escape is recorded as unresolved, not as contained.
printf 'skip - detached-descendant not run by this suite: a setsid descendant leaves every process group the runner reaches; the one disposable-container probe left it running (unresolved, see data/dpes-822-bound-execution/detached-descendant-container-run1.out)\n'

# stalled_input <fifo> <gone-file>: holds the FIFO's write end open and writes a line every
# 0.1s, so the classifier input never reaches its end. A failed write means no reader is
# left, and the producer then writes <gone-file>. Sets PRODUCER to the producer's PID.
# SIGPIPE is ignored so the failed write returns instead of killing the producer.
stalled_input() {
  (
    trap '' PIPE
    exec 7>"$1" || exit 1
    while printf 'x\n' >&7 2>/dev/null; do sleep 0.1; done
    printf 'gone\n' >"$2"
  ) &
  PRODUCER=$!
}

# await_input <case-dir>: 0 once the runner's classifier reader has written to its input
# file. The case gives the runner its own TMPDIR, so its scratch directory is the only one.
await_input() {
  local i=0 file
  while :; do
    for file in "$1"/tmp/fm-mutation-run.*/input; do
      [ -s "$file" ] && return 0
    done
    [ "$i" -ge 100 ] && return 1
    sleep 0.1
    i=$((i + 1))
  done
}

# scratch_left <base>: prints each fm-mutation-run scratch directory left under <base>.
scratch_left() {
  local dir
  for dir in "$1"/fm-mutation-run.*; do
    [ -d "$dir" ] && printf '%s\n' "$dir"
  done
  return 0
}

# Case 11: the runner is cancelled with TERM while it reads a classifier input that never
# ends. The read must not hold the signal: the runner exits 1 at once, publishes nothing,
# and no process is left reading the input.
D="$TMP/read-cancellation"
new_checkout "$D/checkout"
mkdir -p "$D/tmp"
mkfifo "$D/classified"
stalled_input "$D/classified" "$D/gone"
TMPDIR="$D/tmp" PATH="$NOTOOLS" /bin/bash "$RUN" --root "$D/checkout" --timeout 300 --budget 300 \
  <"$D/classified" >"$D/record" 2>"$D/stderr" &
RP=$!
await_input "$D" || { kill -KILL "$RP" "$PRODUCER" 2>/dev/null; fail "read-cancellation: the runner never read the classifier input; stderr: $(cat "$D/stderr" 2>/dev/null)"; }
kill -TERM "$RP"
finish_runner read-cancellation 5
assert_equals 1 "$RC" "read-cancellation: a cancelled runner must exit 1"
assert_contains "$(cat "$D/stderr")" "interrupted" "read-cancellation: the cancellation must be named on stderr"
[ ! -s "$D/record" ] || fail "read-cancellation: a cancelled run must publish no record"
if ! await_file "$D/gone" 5; then
  kill -KILL "$PRODUCER" 2>/dev/null
  fail "read-cancellation: a process still reads the classifier input after the runner exited"
fi
wait "$PRODUCER" 2>/dev/null
pass "read-cancellation: TERM stopped the classifier reader, runner exited 1 within 5s, no reader left"

# Case 12: the runner is SIGKILLed while it reads a classifier input that never ends. The
# runner cannot clean up, so the reader's watcher must notice the owner's death and stop
# the reader group; nothing may keep reading the input.
D="$TMP/read-kill"
new_checkout "$D/checkout"
mkdir -p "$D/tmp"
mkfifo "$D/classified"
stalled_input "$D/classified" "$D/gone"
TMPDIR="$D/tmp" PATH="$NOTOOLS" /bin/bash "$RUN" --root "$D/checkout" --timeout 300 --budget 300 \
  <"$D/classified" >"$D/record" 2>"$D/stderr" &
RP=$!
await_input "$D" || { kill -KILL "$RP" "$PRODUCER" 2>/dev/null; fail "read-kill: the runner never read the classifier input; stderr: $(cat "$D/stderr" 2>/dev/null)"; }
kill -KILL "$RP"
finish_runner read-kill 5
if ! await_file "$D/gone" 10; then
  kill -KILL "$PRODUCER" 2>/dev/null
  fail "read-kill: the classifier reader outlived its SIGKILLed runner"
fi
wait "$PRODUCER" 2>/dev/null
[ ! -s "$D/record" ] || fail "read-kill: a killed run must publish no record"
pass "read-kill: runner SIGKILLed, watcher stopped the classifier reader, no reader left"

# Case 13: the whole-run budget runs out while the classifier input is still being read.
# The read is bounded by the budget: the runner stops the reader, exits 1, publishes
# nothing, and names the budget.
D="$TMP/read-budget"
new_checkout "$D/checkout"
mkdir -p "$D/tmp"
mkfifo "$D/classified"
stalled_input "$D/classified" "$D/gone"
TMPDIR="$D/tmp" PATH="$NOTOOLS" /bin/bash "$RUN" --root "$D/checkout" --timeout 300 --budget 3 \
  <"$D/classified" >"$D/record" 2>"$D/stderr" &
RP=$!
finish_runner read-budget 30
if ! await_file "$D/gone" 10; then
  kill -KILL "$PRODUCER" 2>/dev/null
  fail "read-budget: the classifier reader outlived the whole-run budget"
fi
wait "$PRODUCER" 2>/dev/null
assert_equals 1 "$RC" "read-budget: an exhausted budget during the input read must exit 1"
assert_contains "$(cat "$D/stderr")" "whole-run budget of 3s" "read-budget: the budget must be named on stderr"
[ ! -s "$D/record" ] || fail "read-budget: a budget stop during the read must publish no record"
pass "read-budget: whole-run budget stopped the classifier reader, runner exited 1, no reader left"

# Case 14: the runner is SIGKILLed while it reads the classifier input. Its EXIT trap cannot
# run, so its scratch directory is left behind. The owner is dead, so the next run in the
# same TMPDIR must sweep that directory, and must still run to completion.
D="$TMP/scratch-kill"
new_checkout "$D/checkout"
mkdir -p "$D/tmp"
mkfifo "$D/classified"
stalled_input "$D/classified" "$D/gone"
TMPDIR="$D/tmp" PATH="$NOTOOLS" /bin/bash "$RUN" --root "$D/checkout" --timeout 300 --budget 300 \
  <"$D/classified" >"$D/record" 2>"$D/stderr" &
RP=$!
await_input "$D" || { kill -KILL "$RP" "$PRODUCER" 2>/dev/null; fail "scratch-kill: the runner never read the classifier input; stderr: $(cat "$D/stderr" 2>/dev/null)"; }
kill -KILL "$RP"
finish_runner scratch-kill 5
if ! await_file "$D/gone" 10; then
  kill -KILL "$PRODUCER" 2>/dev/null
  fail "scratch-kill: the classifier reader outlived its SIGKILLed runner"
fi
wait "$PRODUCER" 2>/dev/null
[ -n "$(scratch_left "$D/tmp")" ] || fail "scratch-kill: precondition failed, the SIGKILLed runner left no scratch directory to sweep"
row README.md docs unsupported "scratch sweep fixture" >"$D/second-classified"
TMPDIR="$D/tmp" PATH="$NOTOOLS" /bin/bash "$RUN" --root "$D/checkout" --timeout 300 --budget 300 \
  <"$D/second-classified" >"$D/second-record" 2>"$D/second-stderr" && SECOND_RC=0 || SECOND_RC=$?
assert_equals 0 "$SECOND_RC" "scratch-kill: a clean run after a SIGKILLed run must complete; stderr: $(cat "$D/second-stderr")"
[ -z "$(scratch_left "$D/tmp")" ] || fail "scratch-kill: the next run left the SIGKILLed run's scratch directory behind: $(scratch_left "$D/tmp")"
pass "scratch-kill: the SIGKILLed run's scratch was swept by the next run, which completed"

# Case 15: a second run starts while the first is still alive in the same TMPDIR. The first
# run's owner is alive, so the sweep must leave its scratch directory alone, and the first
# run must still be reading its input afterwards.
D="$TMP/scratch-live"
new_checkout "$D/checkout"
mkdir -p "$D/tmp"
mkfifo "$D/classified"
stalled_input "$D/classified" "$D/gone"
TMPDIR="$D/tmp" PATH="$NOTOOLS" /bin/bash "$RUN" --root "$D/checkout" --timeout 300 --budget 300 \
  <"$D/classified" >"$D/record" 2>"$D/stderr" &
RP=$!
await_input "$D" || { kill -KILL "$RP" "$PRODUCER" 2>/dev/null; fail "scratch-live: the runner never read the classifier input; stderr: $(cat "$D/stderr" 2>/dev/null)"; }
row README.md docs unsupported "scratch sweep fixture" >"$D/second-classified"
TMPDIR="$D/tmp" PATH="$NOTOOLS" /bin/bash "$RUN" --root "$D/checkout" --timeout 300 --budget 300 \
  <"$D/second-classified" >"$D/second-record" 2>"$D/second-stderr" && SECOND_RC=0 || SECOND_RC=$?
if [ -z "$(scratch_left "$D/tmp")" ] || ! kill -0 "$RP" 2>/dev/null; then
  kill -TERM "$RP" 2>/dev/null
  kill -KILL "$PRODUCER" 2>/dev/null
  fail "scratch-live: a later run removed the scratch of a live run, or the live run stopped"
fi
kill -TERM "$RP"
finish_runner scratch-live 5
kill -KILL "$PRODUCER" 2>/dev/null
wait "$PRODUCER" 2>/dev/null
assert_equals 0 "$SECOND_RC" "scratch-live: the run started alongside a live run must complete; stderr: $(cat "$D/second-stderr")"
pass "scratch-live: a later run kept the scratch of a live run, which kept reading its input"
