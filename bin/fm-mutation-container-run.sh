#!/usr/bin/env bash
# bin/fm-mutation-container-run.sh - the container-supervised mutation run. It runs the
# unchanged mutation runner (bin/fm-mutation-run.sh) inside one disposable container,
# so a detached descendant that leaves the runner's process group ends with the
# container's PID namespace instead of outliving the run. The runner is not edited.
#
# Usage: fm-mutation-container-run.sh [--root <dir>] [--container-timeout <seconds>] [runner options...] < classifier-tsv
#   --root               the git work tree to judge; must be the work tree's top level
#                        (default: the work tree of the current directory)
#   --container-timeout  outer bound for the whole container run (default: 2400). When it
#                        is reached, the container is removed by its own name and no
#                        record is written.
#   runner options       passed to bin/fm-mutation-run.sh unchanged (--timeout, --budget,
#                        --probe-timeout, --syntax-timeout)
#
# The classifier TSV is buffered into a regular file on the host and read by the runner
# from that file, never from an open pipe. Callers must check the classifier's exit
# status first, exactly as for the bare runner.
#
# Container contract: the image must already be present locally, and it is never pulled
# or built here. The container is created with --rm and --platform linux/amd64, which the
# wrapper states on stderr, and runs with --pull never --network none --cpus 1 --memory 512m
# --pids-limit 128. The wrapper reads those limits back from the created container and
# starts it only when they match.
# The judged work tree is mounted read-only at /src/tree,
# the git common directory read-only at /src/gitdir, and this checkout's bin/ read-only
# at /src/bin. Git reaches the work tree through GIT_DIR and GIT_WORK_TREE, because a
# linked worktree's git file names a host path the container cannot see. The runner's
# scratch copy lives in the container's own filesystem.
#
# Removal: each run has a unique name, fm-mutation-run-<pid>-<epoch>-<random>. The wrapper
# removes that container by exact name on every exit path (normal, error, signal, and the
# outer bound), then verifies by name that it is gone. Only a verified removal with a
# clean runner exit lets a record be forwarded. A containment failure, including an
# unverifiable removal, writes no record, so a host-mode or uncontained run can never
# read as contained. The wrapper never touches a container it did not create.
# A wrapper killed with KILL runs no exit path and writes no record. A watchdog, started
# before the container is created, then removes that container by name once the wrapper is
# gone. That removal is not verified and nothing reports it.
#
# Output: on success the runner's record goes to stdout with one addition, the token
# supervision=container appended to the published summary row's detail column. The
# verdict and dispositions parsers read only the published flag and the killed/survived/
# timeout/error/unresolved/excluded tokens, so the added token changes neither.
#
# Exit codes: 0 record published and forwarded; 1 refused, failed, interrupted, or removal
# not verified, with no record written and the reason on stderr; 2 usage, or the runner
# rejected its arguments.

set -u

SCRIPT_DIR=$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)
CODE_BIN=$SCRIPT_DIR
# shellcheck source=bin/fm-timeout-lib.sh
. "$SCRIPT_DIR/fm-timeout-lib.sh"

IMAGE=node:24-bookworm
PLATFORM=linux/amd64
NAME_PREFIX=fm-mutation-run
LIMITS=(--pull never --network none --cpus 1 --memory 512m --pids-limit 128)
# The same limits as docker inspect reports them: NanoCpus, Memory in bytes, PidsLimit, NetworkMode.
EXPECTED_LIMITS="1000000000 536870912 128 none"
PROBE_TIMEOUT=30
REMOVE_TIMEOUT=60
DEFAULT_CONTAINER_TIMEOUT=2400

ROOT=""
OUTER=$DEFAULT_CONTAINER_TIMEOUT
RUNNER_ARGS=()
SCRATCH=""
NAME=""
RUN_PID=""
WATCHDOG=""
INTERRUPTED=0
BOUND=0
REMOVED=0
RC=0

usage() {
  printf '%s\n' "usage: fm-mutation-container-run.sh [--root <dir>] [--container-timeout <seconds>]" \
    "       [runner options...] < classifier-tsv"
}

# usage_error <message>: exit 2, the caller's arguments were wrong.
usage_error() {
  printf 'fm-mutation-container-run.sh: %s\n' "$*" >&2
  exit 2
}

# fail <message>: exit 1 after the EXIT trap removes anything this run created.
fail() {
  printf 'fm-mutation-container-run.sh: %s\n' "$*" >&2
  exit 1
}

# require_seconds <option> <value>
#   Refuses unless <value> is a whole number of seconds, at least 1 and at most nine digits.
require_seconds() {
  case "$2" in
    ''|*[!0-9]*) usage_error "$1 must be a whole number of seconds: $2" ;;
  esac
  [ "${#2}" -le 9 ] || usage_error "$1 is too large: $2"
  [ "$2" -ge 1 ] || usage_error "$1 must be at least 1 second"
}

# container_state <name>
#   Prints present, absent, or unknown for <name>, by exact-name filter. unknown means the
#   lookup itself failed, which is never read as absent.
container_state() {
  local out rc
  out=$(fm_run_timed "$PROBE_TIMEOUT" docker ps -a --filter "name=^$1\$" --format '{{.Names}}' 2>/dev/null)
  rc=$?
  if [ "$rc" -ne 0 ]; then
    printf 'unknown\n'
    return 0
  fi
  case "$out" in
    '') printf 'absent\n' ;;
    *) printf 'present\n' ;;
  esac
}

# remove_container
#   Removes this run's container by its exact name, retries once, then verifies by name.
#   Sets REMOVED=1 only when the container is verified absent. Idempotent.
remove_container() {
  [ -n "$NAME" ] || return 0
  fm_run_timed "$REMOVE_TIMEOUT" docker rm -f "$NAME" >/dev/null 2>&1 || true
  if [ "$(container_state "$NAME")" != absent ]; then
    fm_run_timed "$REMOVE_TIMEOUT" docker rm -f "$NAME" >/dev/null 2>&1 || true
  fi
  if [ "$(container_state "$NAME")" = absent ]; then
    REMOVED=1
  else
    REMOVED=0
  fi
}

# finish_run
#   Ends the run's CLI process if it is still alive after removal, so waiting cannot hang,
#   then removes the container and verifies it.
finish_run() {
  remove_container
  if [ -n "$RUN_PID" ] && kill -0 "$RUN_PID" 2>/dev/null; then
    kill -KILL "$RUN_PID" 2>/dev/null || true
  fi
}

# shellcheck disable=SC2329 # Registered by the signal traps below.
on_interrupt() {
  INTERRUPTED=1
  finish_run
}

# shellcheck disable=SC2329 # Registered by the EXIT trap below.
on_exit() {
  local status=$?
  if [ -n "$WATCHDOG" ]; then
    kill "$WATCHDOG" 2>/dev/null || true
    wait "$WATCHDOG" 2>/dev/null || true
  fi
  if [ -n "$NAME" ] && [ "$REMOVED" -ne 1 ]; then
    finish_run
  fi
  [ -z "$SCRATCH" ] || rm -rf -- "$SCRATCH"
  exit "$status"
}

trap on_exit EXIT
trap 'on_interrupt' INT TERM HUP

while [ $# -gt 0 ]; do
  case "$1" in
    --root)
      [ $# -ge 2 ] || usage_error "--root needs a directory"
      ROOT=$2
      shift 2
      ;;
    --container-timeout)
      [ $# -ge 2 ] || usage_error "--container-timeout needs a number of seconds"
      require_seconds --container-timeout "$2"
      OUTER=$2
      shift 2
      ;;
    -h|--help)
      usage
      exit 0
      ;;
    *)
      RUNNER_ARGS+=("$1")
      shift
      ;;
  esac
done

[ -n "$ROOT" ] || ROOT=$PWD
TREE=$(git -C "$ROOT" rev-parse --show-toplevel 2>/dev/null) || usage_error "not a git work tree: $ROOT"
TREE=$(cd "$TREE" && pwd -P)
ROOT_ABS=$(cd "$ROOT" && pwd -P)
[ "$ROOT_ABS" = "$TREE" ] || usage_error "--root must be the work tree's top level: $TREE"
GIT_DIR_HOST=$(cd "$TREE" && cd "$(git rev-parse --absolute-git-dir)" && pwd -P) || fail "could not resolve the work tree's git directory"
COMMON_HOST=$(cd "$TREE" && cd "$(git rev-parse --git-common-dir)" && pwd -P) || fail "could not resolve the git common directory"
BIN_HOST=$(cd "$CODE_BIN" && pwd -P)

if [ "$GIT_DIR_HOST" = "$COMMON_HOST" ]; then
  GIT_DIR_IN=/src/gitdir
else
  case "$GIT_DIR_HOST" in
    "$COMMON_HOST"/*) GIT_DIR_IN="/src/gitdir/${GIT_DIR_HOST#"$COMMON_HOST"/}" ;;
    *) fail "the work tree's git directory is outside the git common directory; no record written" ;;
  esac
fi

SCRATCH=$(mktemp -d /tmp/fm-mutation-container.XXXXXX) || fail "could not create a scratch directory"
RECORD="$SCRATCH/record"
STDERR_FILE="$SCRATCH/stderr"
INPUT="$SCRATCH/input"
BOUND_FLAG="$SCRATCH/bound"

# Buffer the classifier TSV into a regular file, bounded like every other host command.
# fm_run_timed launches its command in the background, and a background job gets /dev/null
# as stdin unless it has its own input redirection, so the input travels on descriptor 4.
exec 4<&0
# shellcheck disable=SC2016  # $1 is deliberately expanded by the child shell.
fm_run_timed "$OUTER" sh -c 'cat <&4 >"$1"' _ "$INPUT"
rc=$?
exec 4<&-
if [ "$rc" -eq 124 ]; then
  fail "reading the classifier input hit the $OUTER-second bound; no record written"
fi
[ "$rc" -eq 0 ] || fail "reading the classifier input failed; no record written"

fm_run_timed "$PROBE_TIMEOUT" docker info >/dev/null 2>&1 \
  || fail "no usable container runtime (docker info failed or timed out); no record written"
fm_run_timed "$PROBE_TIMEOUT" docker image inspect "$IMAGE" >/dev/null 2>&1 \
  || fail "image $IMAGE is not present locally; it is never pulled or built here, so no record written"

# NAME is assigned only after this run has confirmed no container holds the name, so the
# exit path can never remove a container this run did not create.
CANDIDATE="$NAME_PREFIX-$$-$(date +%s)-$RANDOM"
[ "$(container_state "$CANDIDATE")" = absent ] || fail "container name $CANDIDATE is already in use; no record written"
NAME=$CANDIDATE

[ "$INTERRUPTED" -eq 0 ] || fail "interrupted before the container started; no record written"

DEADLINE=$((SECONDS + OUTER))
WRAPPER_PID=$$

# The watchdog removes the container by name at the outer bound, even when the CLI hangs. It
# starts before the container exists and also acts once the wrapper itself is gone, so a wrapper
# killed with KILL, which runs no exit path, still leaves no container behind. Its removal is not
# verified and reports nothing; a live wrapper's own verified removal decides the outcome.
(
  while kill -0 "$WRAPPER_PID" 2>/dev/null; do
    if [ "$SECONDS" -ge "$DEADLINE" ]; then
      : >"$BOUND_FLAG"
      break
    fi
    sleep 1
  done
  fm_run_timed "$REMOVE_TIMEOUT" docker rm -f "$NAME" >/dev/null 2>&1 || true
) </dev/null >/dev/null 2>&1 &
WATCHDOG=$!

# Create the container without starting it, so its limits are read back before any runner code runs.
printf 'fm-mutation-container-run.sh: pinned container platform to %s\n' "$PLATFORM" >&2
fm_run_timed "$PROBE_TIMEOUT" docker create --rm --name "$NAME" --platform "$PLATFORM" "${LIMITS[@]}" \
  -v "$TREE:/src/tree:ro" \
  -v "$COMMON_HOST:/src/gitdir:ro" \
  -v "$BIN_HOST:/src/bin:ro" \
  -v "$INPUT:/work/input:ro" \
  -e "GIT_DIR=$GIT_DIR_IN" \
  -e GIT_WORK_TREE=/src/tree \
  -e GIT_OPTIONAL_LOCKS=0 \
  "$IMAGE" \
  bash -c 'exec bash /src/bin/fm-mutation-run.sh "$@" </work/input' fm-mutation-run.sh \
  --root /src/tree ${RUNNER_ARGS[@]+"${RUNNER_ARGS[@]}"} \
  >/dev/null 2>"$STDERR_FILE" </dev/null \
  || { cat "$STDERR_FILE" >&2; fail "the container could not be created; no record written"; }

READ_LIMITS=$(fm_run_timed "$PROBE_TIMEOUT" docker inspect \
  --format '{{.HostConfig.NanoCpus}} {{.HostConfig.Memory}} {{.HostConfig.PidsLimit}} {{.HostConfig.NetworkMode}}' \
  "$NAME" 2>/dev/null) || fail "container limits could not be read back; no record written"
[ "$READ_LIMITS" = "$EXPECTED_LIMITS" ] || fail "container limits did not read back as pinned; no record written"

[ "$INTERRUPTED" -eq 0 ] || fail "interrupted before the container started; no record written"

docker start -a "$NAME" >"$RECORD" 2>"$STDERR_FILE" </dev/null &
RUN_PID=$!

wait "$RUN_PID" 2>/dev/null
RC=$?
if [ "$INTERRUPTED" -eq 1 ]; then
  wait "$RUN_PID" 2>/dev/null || true
fi
[ -e "$BOUND_FLAG" ] && BOUND=1

kill "$WATCHDOG" 2>/dev/null || true
wait "$WATCHDOG" 2>/dev/null || true
WATCHDOG=""
finish_run

if [ -s "$STDERR_FILE" ]; then
  cat "$STDERR_FILE" >&2
fi
if [ "$INTERRUPTED" -eq 1 ]; then
  fail "interrupted; container removal verified=$REMOVED; no record written"
fi
if [ "$BOUND" -eq 1 ]; then
  fail "container run reached the $OUTER-second outer bound; container removal verified=$REMOVED; no record written"
fi
if [ "$RC" -eq 2 ]; then
  printf 'fm-mutation-container-run.sh: the runner rejected its arguments\n' >&2
  exit 2
fi
if [ "$RC" -ne 0 ]; then
  fail "container run exited $RC; no record written"
fi
[ "$REMOVED" -eq 1 ] || fail "container removal could not be verified; no record written"

# A forwarded record must be exactly the runner's shape: six columns on every row and one
# published summary row.
BAD_ROWS=$(awk -F '\t' 'NF != 6 { n++ } END { print n + 0 }' "$RECORD")
PUBLISHED=$(awk -F '\t' '$1 == "summary" && $5 == "published" { n++ } END { print n + 0 }' "$RECORD")
[ "$BAD_ROWS" -eq 0 ] || fail "runner record has malformed rows; no record written"
[ "$PUBLISHED" -eq 1 ] || fail "runner record has no single published summary; no record written"

awk -F '\t' -v OFS='\t' '$1 == "summary" && $5 == "published" { $6 = $6 " supervision=container" } { print }' "$RECORD"
exit 0
