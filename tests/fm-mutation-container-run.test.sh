#!/usr/bin/env bash
# tests/fm-mutation-container-run.test.sh - the container-supervised mutation wrapper
# (bin/fm-mutation-container-run.sh) through its executable interface, with a stub
# docker first on PATH. The stub models containers as marker files, logs every call,
# and runs nothing real. Covered: the container limits and read-only mounts, the --rm flag
# and the platform pin, read-back of the limits before start, unique naming, removal on
# every exit path and after a KILL of the wrapper, the outer bound, refusal without a
# runtime or image, no host fallback, verification failure, malformed records, no touching
# of foreign containers, runner option pass-through, and exit codes.
set -u

# shellcheck source=tests/lib.sh
. "$(dirname "${BASH_SOURCE[0]}")/lib.sh"

WRAP="$ROOT/bin/fm-mutation-container-run.sh"
TMP=$(fm_test_tmproot fm-mutation-container-run)
STUBDIR="$TMP/stubbin"
REPO="$TMP/repo"
INPUT="$TMP/classifier.tsv"
ERR="$TMP/stderr"
CASES="$TMP/cases"
mkdir -p "$STUBDIR" "$CASES"
fm_git_init_commit "$REPO"
printf 'bin/target.sh\tshell\tmutate\tshell-custom-mutator (report-only)\n' >"$INPUT"

# The stub docker. Containers are marker files under $FAKE_STATE (c-<name>); removal
# deletes the marker and leaves removed-<name> so a hanging stub run can exit. Every call
# is appended to $FAKE_STATE/calls, and a run's argv goes one per line to run.argv.
cat >"$STUBDIR/docker" <<'SH'
#!/usr/bin/env bash
set -u
S=${FAKE_STATE:?stub docker: FAKE_STATE unset}
printf '%s\n' "$*" >>"$S/calls"
case "$1" in
  info)
    [ -z "${STUB_INFO_FAIL:-}" ] ;;
  image)
    [ -z "${STUB_NO_IMAGE:-}" ] ;;
  ps)
    filter=""
    for a in "$@"; do
      case "$a" in name=*) filter=$a ;; esac
    done
    name=${filter#name=^}
    name=${name%\$}
    if [ -n "${STUB_ALL_PRESENT:-}" ] || [ -e "$S/c-$name" ]; then
      printf '%s\n' "$name"
    fi ;;
  run)
    shift
    : >"$S/run.argv"
    name="" prev=""
    for a in "$@"; do
      printf '%s\n' "$a" >>"$S/run.argv"
      case "$prev" in
        --name) name=$a ;;
        -v) case "$a" in *:/work/input:ro) cp -- "${a%:/work/input:ro}" "$S/input.copy" ;; esac ;;
      esac
      prev=$a
    done
    : >"$S/c-$name"
    if [ -n "${STUB_RUN_HANG:-}" ]; then
      n=0
      while [ ! -e "$S/removed-$name" ]; do
        n=$((n + 1))
        [ "$n" -lt 300 ] || exit 99
        sleep 0.1
      done
      exit 137
    fi
    cat "${STUB_RECORD:?stub docker: STUB_RECORD unset}"
    exit "${STUB_RUN_RC:-0}" ;;
  create)
    shift
    : >"$S/run.argv"
    name="" prev=""
    for a in "$@"; do
      printf '%s\n' "$a" >>"$S/run.argv"
      case "$prev" in
        --name) name=$a ;;
        -v) case "$a" in *:/work/input:ro) cp -- "${a%:/work/input:ro}" "$S/input.copy" ;; esac ;;
      esac
      prev=$a
    done
    : >"$S/c-$name"
    exit 0 ;;
  inspect)
    [ -z "${STUB_INSPECT_FAIL:-}" ] || exit 1
    printf '%s\n' "${STUB_INSPECT:-1000000000 536870912 128 none}" ;;
  start)
    last=""
    for a in "$@"; do last=$a; done
    if [ -n "${STUB_RUN_HANG:-}" ]; then
      n=0
      while [ ! -e "$S/removed-$last" ]; do
        n=$((n + 1))
        [ "$n" -lt 300 ] || exit 99
        sleep 0.1
      done
      exit 137
    fi
    cat "${STUB_RECORD:?stub docker: STUB_RECORD unset}"
    exit "${STUB_RUN_RC:-0}" ;;
  rm)
    last=""
    for a in "$@"; do last=$a; done
    [ -z "${STUB_RM_FAIL:-}" ] || exit 1
    /bin/rm -f "$S/c-$last"
    : >"$S/removed-$last"
    exit 0 ;;
  *)
    printf 'stub docker: unexpected subcommand %s\n' "$1" >&2
    exit 97 ;;
esac
SH
chmod +x "$STUBDIR/docker"

# good_record: a runner record the wrapper must forward, with the supervision token
# added to the published summary only.
good_record() {
  printf 'tool\tfm-shell-mutator\t-\t-\tbuiltin\tversion 1; report-only\n'
  printf 'mutant\tbin/target.sh\tshell\tL2-eq-ne\tkilled\toracle exit 1\n'
  printf 'summary\t-\t-\t-\tpublished\tkilled=1 survived=0 timeout=0 error=0 unresolved=0 excluded=0\n'
}

# new_case <name>: a fresh stub state for one case, with the good record by default.
new_case() {
  CASE=$CASES/$1
  STATE=$CASE/state
  mkdir -p "$STATE"
  unset STUB_INFO_FAIL STUB_NO_IMAGE STUB_ALL_PRESENT STUB_RUN_HANG STUB_RUN_RC STUB_RM_FAIL
  unset STUB_INSPECT STUB_INSPECT_FAIL
  export FAKE_STATE=$STATE
  good_record >"$CASE/good.record"
  export STUB_RECORD=$CASE/good.record
  CALLS=$STATE/calls
  RUNARGV=$STATE/run.argv
}

# run_wrap [wrapper-arg...]: the wrapper against the fixture with the stub first on PATH.
# Sets OUT (stdout), ERR (stderr file), and RC.
run_wrap() {
  OUT=$(PATH="$STUBDIR:$PATH" bash "$WRAP" --root "$REPO" "$@" <"$INPUT" 2>"$ERR")
  RC=$?
}

# run_name: the --name value the wrapper gave its container on the last run.
run_name() {
  awk 'p { print; exit } $0 == "--name" { p = 1 }' "$RUNARGV"
}

# has_call <extended-regex>: the stub logged a call matching the pattern.
has_call() {
  grep -q -E -- "$1" "$CALLS" 2>/dev/null
}

# expect_removed <name>: the wrapper removed <name> by exact name, the container is gone,
# and no rm call targeted any other name.
expect_removed() {
  local n=$1 bad
  grep -q -F -x -- "rm -f $n" "$CALLS" 2>/dev/null || fail "wrapper never removed its container $n"
  [ ! -e "$STATE/c-$n" ] || fail "container $n still present after the run"
  bad=$(grep '^rm ' "$CALLS" | grep -v -F -x -- "rm -f $n" || true)
  [ -z "$bad" ] || fail "wrapper removed a container it did not create: $bad"
}

# wait_for_container <state-dir>: block until the stub has created a container marker.
wait_for_container() {
  local i=0
  until ls "$1"/c-* >/dev/null 2>&1; do
    i=$((i + 1))
    [ "$i" -le 100 ] || return 1
    sleep 0.1
  done
}

# --- argument construction ---------------------------------------------------

new_case args
run_wrap --timeout 5 --budget 120
expect_code 0 "$RC" "a clean stub run must exit 0"
ARGV=$(tr '\n' ' ' <"$RUNARGV")
assert_contains "$ARGV" "--pull never --network none --cpus 1 --memory 512m --pids-limit 128" "container limits must be exactly the four documented flags"
assert_contains "$ARGV" " node:24-bookworm " "the pinned image must be the one run"
assert_contains "$ARGV" ":/src/tree:ro" "the judged work tree must be mounted read-only"
assert_contains "$ARGV" ":/src/gitdir:ro" "the git common directory must be mounted read-only"
assert_contains "$ARGV" ":/src/bin:ro" "the runner's bin directory must be mounted read-only"
assert_contains "$ARGV" "GIT_OPTIONAL_LOCKS=0" "git must not take optional locks inside the container"
assert_contains "$ARGV" "GIT_WORK_TREE=/src/tree" "git must reach the work tree at its container path"
assert_contains "$ARGV" "--root /src/tree --timeout 5 --budget 120" "runner options must pass through unchanged"
case "$ARGV" in
  *"--privileged"*|*"--network host"*|*"--pid host"*|*"--ipc host"*|*"docker.sock"*|*"--cap-add"*)
    fail "container must never be privileged, share host network, pid, or ipc, mount the docker socket, or add capabilities: $ARGV" ;;
esac
MOUNTS=0 prev=""
while IFS= read -r a; do
  if [ "$prev" = "-v" ]; then
    MOUNTS=$((MOUNTS + 1))
    case "$a" in *:ro) : ;; *) fail "every mount must be read-only, got: $a" ;; esac
  fi
  prev=$a
done <"$RUNARGV"
[ "$MOUNTS" -eq 4 ] || fail "expected four read-only mounts (tree, gitdir, bin, input), got $MOUNTS"
cmp -s "$INPUT" "$STATE/input.copy" || fail "the classifier TSV must reach the runner as a buffered read-only input file"

# --- classifier input under each timeout mechanism ----------------------------------
# On a host with GNU timeout, the timed helper launches its command in the background,
# which gives that command /dev/null as stdin, so the classifier TSV must travel on an
# explicit descriptor. The launcher stand-in has timeout's argument shape ("-k D N
# command...") and bounds nothing, so only the input path is exercised.
new_case input-timeout-launcher
mkdir -p "$CASE/launcher"
printf '#!/usr/bin/env bash\nshift 3\nexec "$@"\n' >"$CASE/launcher/timeout"
chmod +x "$CASE/launcher/timeout"
PATH="$CASE/launcher:$STUBDIR:$PATH" bash "$WRAP" --root "$REPO" --timeout 5 --budget 120 <"$INPUT" >/dev/null 2>"$ERR"
RC=$?
expect_code 0 "$RC" "the wrapper must run when timeout is the launcher"
cmp -s "$INPUT" "$STATE/input.copy" || fail "the classifier TSV was lost when timeout launched the read; the runner gets no input"

new_case input-bash-mechanism
FM_TIMEOUT_MECHANISM_OVERRIDE=bash PATH="$STUBDIR:$PATH" bash "$WRAP" --root "$REPO" --timeout 5 --budget 120 <"$INPUT" >/dev/null 2>"$ERR"
RC=$?
expect_code 0 "$RC" "the wrapper must run under the bash timeout mechanism"
cmp -s "$INPUT" "$STATE/input.copy" || fail "the classifier TSV was lost under the bash timeout mechanism"
pass "container argv carries the four limits, four read-only mounts, git environment, and runner options, and never privilege or host namespaces"

# --- container flags and read-back before the runner starts ------------------

new_case rm-flag
run_wrap
expect_code 0 "$RC" "a clean stub run must exit 0"
grep -q -F -x -- "--rm" "$RUNARGV" || fail "the container must be created with --rm so the runtime reaps it when the runner exits"
expect_removed "$(run_name)"
pass "the container is created with --rm and is still removed by name"

new_case platform-pin
run_wrap
expect_code 0 "$RC" "a clean stub run must exit 0"
ARGV=$(tr '\n' ' ' <"$RUNARGV")
assert_contains "$ARGV" "--platform linux/amd64" "the image platform must be pinned so every host runs the same image"
expect_removed "$(run_name)"
pass "the container pins its platform to linux/amd64"

new_case platform-note
run_wrap
expect_code 0 "$RC" "a clean stub run must exit 0"
assert_contains "$(cat "$ERR")" "pinned container platform to linux/amd64" "the platform pin must be stated on stderr"
expect_removed "$(run_name)"
pass "the platform pin is stated on stderr on a clean run"

new_case limits-readback-match
run_wrap
expect_code 0 "$RC" "limits that read back as pinned must run the container"
ORDER=$(grep -E '^(inspect|start) ' "$CALLS" | awk '{ printf "%s ", $1 }')
assert_equals "inspect start " "$ORDER" "the limits must be read back from the created container before it starts"
expect_removed "$(run_name)"
pass "the limits are read back from the created container before it starts"

new_case limits-readback-mismatch
export STUB_INSPECT="1000000000 1073741824 128 none"
run_wrap
expect_code 1 "$RC" "limits that do not read back as pinned must refuse"
assert_contains "$(cat "$ERR")" "container limits did not read back as pinned; no record written" "a limit mismatch must name the cause"
[ -z "$OUT" ] || fail "a refused run must write no record"
has_call '^start ' && fail "a container whose limits do not match the pin must never start"
expect_removed "$(run_name)"
pass "a limit that does not read back as pinned refuses before the runner starts"

new_case limits-readback-fails
export STUB_INSPECT_FAIL=1
run_wrap
expect_code 1 "$RC" "a limit read-back that fails must refuse"
assert_contains "$(cat "$ERR")" "container limits could not be read back; no record written" "a failed read-back must name the cause"
[ -z "$OUT" ] || fail "a refused run must write no record"
has_call '^start ' && fail "a container whose limits could not be read back must never start"
expect_removed "$(run_name)"
pass "a limit read-back that fails refuses before the runner starts"

# --- unique naming and removal on normal exit --------------------------------

new_case naming
run_wrap
expect_code 0 "$RC" "a clean stub run must exit 0"
NAME=$(run_name)
re='^fm-mutation-run-[0-9]+-[0-9]+-[0-9]+$'
if ! [[ "$NAME" =~ $re ]]; then
  fail "container name must be fm-mutation-run-<pid>-<epoch>-<random>, got: $NAME"
fi
expect_removed "$NAME"
: >"$CALLS"
run_wrap
SECOND=$(run_name)
[ "$NAME" != "$SECOND" ] || fail "two runs must never share a container name"
expect_removed "$SECOND"
pass "each run gets its own run-prefixed name and is removed by that name on normal exit"

new_case foreign
: >"$STATE/c-unrelated-container"
: >"$STATE/c-fm-mutation-run-99999-1-1"
run_wrap
expect_code 0 "$RC" "a clean stub run must exit 0"
expect_removed "$(run_name)"
[ -e "$STATE/c-unrelated-container" ] || fail "a foreign container was removed"
[ -e "$STATE/c-fm-mutation-run-99999-1-1" ] || fail "a foreign run-prefixed container was removed"
pass "foreign containers, including run-prefixed ones, are never removed"

new_case collision
export STUB_ALL_PRESENT=1
run_wrap
expect_code 1 "$RC" "a name already in use must refuse"
assert_contains "$(cat "$ERR")" "already in use" "a name collision must name the cause"
[ -z "$OUT" ] || fail "a refused run must write no record"
has_call '^(run|create|start) ' && fail "a colliding name must never start a container"
has_call '^rm ' && fail "a colliding name must never remove a container the wrapper did not confirm absent"
pass "a name collision refuses before any container starts or any removal"

# --- removal on the other exit paths -----------------------------------------

new_case error-exit
export STUB_RUN_RC=1
run_wrap
expect_code 1 "$RC" "a failed container run must refuse"
assert_contains "$(cat "$ERR")" "container run exited 1; no record written" "a failed run must say no record was written"
[ -z "$OUT" ] || fail "a refused run must write no record"
expect_removed "$(run_name)"
pass "a failed container run is refused and its container is still removed"

new_case signal-exit
export STUB_RUN_HANG=1
PATH="$STUBDIR:$PATH" bash "$WRAP" --root "$REPO" --container-timeout 120 <"$INPUT" >"$CASE/out" 2>"$ERR" &
WPID=$!
wait_for_container "$STATE" || fail "stub run never started"
kill -TERM "$WPID"
wait "$WPID"
RC=$?
expect_code 1 "$RC" "an interrupted run must refuse"
assert_contains "$(cat "$ERR")" "interrupted" "an interrupted run must say so"
[ ! -s "$CASE/out" ] || fail "an interrupted run must write no record"
expect_removed "$(run_name)"
pass "an interrupted run removes its container before it exits and writes no record"

new_case outer-bound
export STUB_RUN_HANG=1
run_wrap --container-timeout 2
expect_code 1 "$RC" "a run past its outer bound must refuse"
assert_contains "$(cat "$ERR")" "reached the 2-second outer bound; container removal verified=1; no record written" "the outer bound must name verified removal"
[ -z "$OUT" ] || fail "a run past its bound must write no record"
expect_removed "$(run_name)"
pass "the outer bound removes the container by name and refuses with verified removal"

new_case sigkill-wrapper
export STUB_RUN_HANG=1
PATH="$STUBDIR:$PATH" bash "$WRAP" --root "$REPO" --container-timeout 120 <"$INPUT" >"$CASE/out" 2>"$ERR" &
WPID=$!
wait_for_container "$STATE" || fail "stub run never started"
NAME=$(run_name)
kill -KILL "$WPID"
wait "$WPID" 2>/dev/null || true
i=0
until [ -e "$STATE/removed-$NAME" ]; do
  i=$((i + 1))
  [ "$i" -le 100 ] || fail "a wrapper killed by KILL left its container $NAME unremoved"
  sleep 0.1
done
expect_removed "$NAME"
pass "a wrapper killed by KILL still has its container removed by name"

# --- refusal without a runtime or image, no host fallback --------------------

new_case no-runtime
export STUB_INFO_FAIL=1
run_wrap
expect_code 1 "$RC" "no usable runtime must refuse"
assert_contains "$(cat "$ERR")" "no usable container runtime" "a missing runtime must name the cause"
[ -z "$OUT" ] || fail "a refused run must write no record"
has_call '^(run|create|start) ' && fail "no container may start without a usable runtime"
pass "no usable runtime refuses without starting anything or falling back to the host"

new_case no-image
export STUB_NO_IMAGE=1
run_wrap
expect_code 1 "$RC" "a missing local image must refuse"
assert_contains "$(cat "$ERR")" "is not present locally" "a missing image must name the cause"
[ -z "$OUT" ] || fail "a refused run must write no record"
has_call '^pull' && fail "the image must never be pulled"
has_call '^build' && fail "the image must never be built here"
has_call '^(run|create|start) ' && fail "no container may start without the pinned image"
pass "a missing image refuses without pulling, building, or starting a container"

# --- verification, malformed records, forwarding -----------------------------

new_case rm-fails
export STUB_RM_FAIL=1
run_wrap
expect_code 1 "$RC" "a removal that cannot be verified must refuse"
assert_contains "$(cat "$ERR")" "container removal could not be verified; no record written" "unverified removal must refuse"
[ -z "$OUT" ] || fail "unverified removal must write no record"
pass "a removal that cannot be verified refuses and forwards no record"

new_case malformed-rows
printf 'tool\tfm-shell-mutator\t-\t-\tbuiltin\n' >"$CASE/bad.record"
printf 'summary\t-\t-\t-\tpublished\tkilled=1\n' >>"$CASE/bad.record"
export STUB_RECORD=$CASE/bad.record
run_wrap
expect_code 1 "$RC" "a record with a short row must refuse"
assert_contains "$(cat "$ERR")" "runner record has malformed rows; no record written" "a short row must refuse as malformed"
[ -z "$OUT" ] || fail "a malformed record must not be forwarded"
pass "a record with a row that is not six columns is refused and not forwarded"

new_case no-published
printf 'tool\tfm-shell-mutator\t-\t-\tbuiltin\tversion 1\n' >"$CASE/bad.record"
printf 'summary\t-\t-\t-\tkilled\tkilled=1\n' >>"$CASE/bad.record"
export STUB_RECORD=$CASE/bad.record
run_wrap
expect_code 1 "$RC" "a record without a published summary must refuse"
assert_contains "$(cat "$ERR")" "runner record has no single published summary; no record written" "a missing published summary must refuse"
[ -z "$OUT" ] || fail "a record without a published summary must not be forwarded"
pass "a record without exactly one published summary is refused and not forwarded"

new_case forward
run_wrap
expect_code 0 "$RC" "a clean stub run must exit 0"
EXPECTED=$(printf 'tool\tfm-shell-mutator\t-\t-\tbuiltin\tversion 1; report-only\nmutant\tbin/target.sh\tshell\tL2-eq-ne\tkilled\toracle exit 1\nsummary\t-\t-\t-\tpublished\tkilled=1 survived=0 timeout=0 error=0 unresolved=0 excluded=0 supervision=container')
assert_equals "$EXPECTED" "$OUT" "the forwarded record must equal the runner's, with only the published summary token added"
has_call '^pull' && fail "a clean run must never pull an image"
expect_removed "$(run_name)"
pass "a valid record is forwarded with the supervision token on the published summary only"

# --- usage and runner-rejection exit codes ------------------------------------

new_case runner-rejects
export STUB_RUN_RC=2
run_wrap --bogus-runner-option
expect_code 2 "$RC" "a runner that rejects its arguments must exit 2"
assert_contains "$(cat "$ERR")" "the runner rejected its arguments" "a rejection must say so"
[ -z "$OUT" ] || fail "a rejected run must write no record"
grep -q -F -x -- "--bogus-runner-option" "$RUNARGV" || fail "runner options must reach the runner unchanged"
pass "a runner rejection exits 2 and the runner option reaches the runner unchanged"

new_case usage
for bad in "0" "abc" ""; do
  OUT=$(bash "$WRAP" --container-timeout "$bad" <"$INPUT" 2>"$ERR")
  RC=$?
  expect_code 2 "$RC" "--container-timeout '$bad' must be a usage error"
done
OUT=$(bash "$WRAP" --container-timeout 2>"$ERR" </dev/null)
RC=$?
expect_code 2 "$RC" "--container-timeout with no value must be a usage error"
OUT=$(bash "$WRAP" --help 2>"$ERR" </dev/null)
RC=$?
expect_code 0 "$RC" "--help must exit 0"
assert_contains "$OUT" "usage: fm-mutation-container-run.sh" "--help must print usage"
pass "usage errors exit 2 before any runtime is probed, and --help exits 0"

echo "# fm-mutation-container-run.test.sh: all assertions passed"
