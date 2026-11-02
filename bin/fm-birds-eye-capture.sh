#!/usr/bin/env bash
# fm-birds-eye-capture.sh - observer-only, on-demand output capture for one
# identity from the fleet snapshot (birds-eye-view, DPES-798).
#
# Usage: fm-birds-eye-capture.sh --root <home-dir> --task <task-id> --spawn-gen <gen> [--lines <1..100>]
#
#   --root       operational home to observe. It is passed to
#                bin/fm-fleet-snapshot.sh as FM_HOME. Inherited FM_*_OVERRIDE
#                private directories are honored only when the inherited FM_HOME
#                is this same home (docs/configuration.md, code repo as FM_HOME);
#                otherwise they are cleared so the snapshot reads this home only.
#   --task       logical task id. It must appear in the current fleet snapshot,
#                and its recorded tmux endpoint must be exactly <session>:fm-<id>.
#   --spawn-gen  incarnation the caller expects. It must equal the current one,
#                both before and after the capture.
#   --lines      bounded tail, default 40. A value above 100 clamps to 100 and
#                the reason says so.
#
# Output: one JSON object on stdout, schema fm-birds-eye-capture.v1:
#   {schema, key, task_id, spawn_gen_requested, spawn_gen_current, captured_at,
#    source_backend, lines_requested, lines_returned, truncated, status, reason, text}
#   status is ok, unavailable, generation_mismatch, unknown_identity, or error.
#   Only status ok carries text; every other status carries "".
#   truncated means the capture held more lines than the returned tail.
#   Exit 0 whenever a JSON result is emitted (read status, not exit code).
#   Exit 2 for invocation or environment errors, with usage on stderr and no JSON.
#
# Safety: the only capture call is the read-only `tmux capture-pane -p`, the
# same primitive as fm_backend_tmux_capture in bin/backends/tmux.sh. The script
# never sends keys, attaches, or writes to the home. It does not source
# bin/fm-peek.sh, bin/fm-guard.sh, or bin/fm-wake-lib.sh, which write guard and
# lock state. Its only writes go to a private temp directory removed on exit.
# Only tmux has a validated capture path; other backends report unavailable.
# The recorded window is resolved by whole-name equality among the session's
# windows and captured through its unique tmux window id. A name or window target
# is never handed to capture-pane, because tmux would prefix-match it or read a
# dotted id such as fm-a.0 as pane 0 of window fm-a, and so could read a neighbor.

set -eu

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
# shellcheck source=bin/fm-pr-lib.sh
. "$SCRIPT_DIR/fm-pr-lib.sh"
SCHEMA=fm-birds-eye-capture.v1
DEFAULT_LINES=40
MAX_LINES=100
# Add a backend only after a real capture from a real live agent succeeds on this
# machine; see docs/birds-eye-view-capture.md for the recorded evidence.
SUPPORTED_BACKENDS="tmux"

usage() {
  cat <<'EOF'
usage: fm-birds-eye-capture.sh --root <home-dir> --task <task-id> --spawn-gen <gen> [--lines <1..100>]
EOF
}

usage_error() {
  printf 'fm-birds-eye-capture.sh: %s\n' "$1" >&2
  usage >&2
  exit 2
}

ROOT=""
TASK=""
GEN=""
LINES_RAW=""
HAVE_LINES=0
while [ "$#" -gt 0 ]; do
  case "$1" in
    --root|--task|--spawn-gen|--lines)
      [ "$#" -ge 2 ] || usage_error "$1 needs a value"
      case "$1" in
        --root) ROOT=$2 ;;
        --task) TASK=$2 ;;
        --spawn-gen) GEN=$2 ;;
        --lines) LINES_RAW=$2; HAVE_LINES=1 ;;
      esac
      shift 2
      ;;
    -h|--help) usage; exit 0 ;;
    *) usage_error "unrecognized argument: $1" ;;
  esac
done

[ -n "$ROOT" ] || usage_error "--root is required"
[ -n "$TASK" ] || usage_error "--task is required"
[ -n "$GEN" ] || usage_error "--spawn-gen is required"
[ -d "$ROOT" ] || usage_error "--root is not a directory"

LINES=$DEFAULT_LINES
CLAMPED=0
if [ "$HAVE_LINES" = 1 ]; then
  case "$LINES_RAW" in
    ''|*[!0-9]*) usage_error "--lines must be a whole number from 1 to $MAX_LINES" ;;
  esac
  significant=${LINES_RAW#"${LINES_RAW%%[!0]*}"}
  if [ -z "$significant" ]; then
    usage_error "--lines must be at least 1"
  elif [ "${#significant}" -gt 3 ]; then
    LINES=$MAX_LINES
    CLAMPED=1
  else
    LINES=$((significant))
    if [ "$LINES" -gt "$MAX_LINES" ]; then
      LINES=$MAX_LINES
      CLAMPED=1
    fi
  fi
fi

command -v jq >/dev/null 2>&1 || { printf 'fm-birds-eye-capture.sh: jq is required\n' >&2; exit 2; }

ROOT_CANON=$(cd "$ROOT" && pwd -P)
KEY="$ROOT_CANON#$TASK"

# Private-directory layout: overrides inherited from a different home never
# apply here. They are kept only when the inherited FM_HOME is this home.
CLEAR_ENV=(-u FM_ROOT_OVERRIDE)
if [ -z "${FM_HOME:-}" ] || [ ! -d "$FM_HOME" ] || [ "$(cd "$FM_HOME" && pwd -P)" != "$ROOT_CANON" ]; then
  CLEAR_ENV+=(-u FM_STATE_OVERRIDE -u FM_DATA_OVERRIDE -u FM_CONFIG_OVERRIDE -u FM_PROJECTS_OVERRIDE)
fi

WORK=$(mktemp -d "${TMPDIR:-/tmp}/fm-birds-eye-capture.XXXXXX") || {
  printf 'fm-birds-eye-capture.sh: could not create a private temp directory\n' >&2
  exit 2
}
trap 'rm -rf "$WORK"' EXIT
: > "$WORK/empty.txt"

# Result fields consumed by emit. Defaults describe a result with no text.
E_STATUS=error
E_REASON=""
E_BACKEND=""
E_GEN_CURRENT=""
E_TEXT_FILE="$WORK/empty.txt"
E_LINES_RETURNED=0
E_TRUNCATED=false
E_CAPTURED_AT=""

emit() {
  local captured=${E_CAPTURED_AT:-$(date -u +%Y-%m-%dT%H:%M:%SZ)}
  jq -n -c \
    --arg schema "$SCHEMA" \
    --arg key "$KEY" \
    --arg task_id "$TASK" \
    --arg gen_requested "$GEN" \
    --arg gen_current "$E_GEN_CURRENT" \
    --arg captured_at "$captured" \
    --arg backend "$E_BACKEND" \
    --argjson lines_requested "$LINES" \
    --argjson lines_returned "$E_LINES_RETURNED" \
    --argjson truncated "$E_TRUNCATED" \
    --arg status "$E_STATUS" \
    --arg reason "$E_REASON" \
    --rawfile text "$E_TEXT_FILE" \
    '{
      schema: $schema,
      key: $key,
      task_id: $task_id,
      spawn_gen_requested: $gen_requested,
      spawn_gen_current: (if $gen_current == "" then null else $gen_current end),
      captured_at: $captured_at,
      source_backend: (if $backend == "" then null else $backend end),
      lines_requested: $lines_requested,
      lines_returned: $lines_returned,
      truncated: $truncated,
      status: $status,
      reason: $reason,
      text: $text
    }'
  exit 0
}

fail_result() {  # <status> <reason>
  E_STATUS=$1
  E_REASON=$2
  emit
}

# Reads the fleet snapshot for this home into $1 and resolves the task row from
# it. Sets SNAP_STATUS and SNAP_REASON; on ok also sets ROW_GEN, ROW_BACKEND,
# ROW_TARGET, and ROW_EXISTS. Identity comes only from this output.
read_identity() {  # <snapshot-out-file>
  local out=$1 rc matches count row
  SNAP_STATUS=ok
  SNAP_REASON=""
  set +e
  env "${CLEAR_ENV[@]}" \
    FM_HOME="$ROOT_CANON" FM_CREW_STATE_NO_FORGE=1 FM_SNAPSHOT_CACHE_DIR="$WORK/cache" \
    "$SCRIPT_DIR/fm-fleet-snapshot.sh" --json >"$out" 2>"$WORK/snapshot.err"
  rc=$?
  set -e
  if [ "$rc" -ne 0 ]; then
    SNAP_STATUS=error
    SNAP_REASON="fleet snapshot exited $rc; identity could not be verified"
    return 0
  fi
  if ! jq -e '.schema == "fm-fleet-snapshot.v1" and (.tasks | type == "array")' "$out" >/dev/null 2>&1; then
    SNAP_STATUS=error
    SNAP_REASON="fleet snapshot output is not fm-fleet-snapshot.v1; identity could not be verified"
    return 0
  fi
  if ! matches=$(jq -c --arg id "$TASK" '[.tasks[]? | select(type == "object" and .id == $id)]' "$out" 2>/dev/null); then
    SNAP_STATUS=error
    SNAP_REASON="fleet snapshot task rows could not be read"
    return 0
  fi
  count=$(printf '%s' "$matches" | jq 'length')
  case "$count" in
    0)
      SNAP_STATUS=unknown_identity
      SNAP_REASON="task is not in the current fleet snapshot"
      return 0
      ;;
    1) ;;
    *)
      SNAP_STATUS=error
      SNAP_REASON="snapshot lists this task more than once; identity is ambiguous"
      return 0
      ;;
  esac
  row=$(printf '%s' "$matches" | jq -c '.[0]')
  ROW_GEN=$(printf '%s' "$row" | jq -r '.spawn_gen // ""')
  ROW_BACKEND=$(printf '%s' "$row" | jq -r '.backend // ""')
  ROW_TARGET=$(printf '%s' "$row" | jq -r '.endpoint.target // ""')
  ROW_EXISTS=$(printf '%s' "$row" | jq -r '.endpoint.exists // false')
}

# Targeted incarnation re-read: the last spawn_gen= line of the task's state meta,
# in the state directory the snapshot reported. Empty when the meta is gone.
read_meta_gen() {
  local line value=""
  [ -f "$STATE_DIR/$TASK.meta" ] || { printf '%s' "$value"; return 0; }
  while IFS= read -r line || [ -n "$line" ]; do
    case "$line" in
      spawn_gen=*) value=${line#*=} ;;
    esac
  done < "$STATE_DIR/$TASK.meta" 2>/dev/null || true
  printf '%s' "$value"
}

# Refuse anything that is not a legal task id before any state is read. The rule
# is the one every lifecycle script applies (fm_task_id_path_safe), so selectors,
# paths, pane ids, and shell text never reach the snapshot, and every id the
# fleet can hold stays reachable. A legal id still resolves only against the snapshot.
if ! fm_task_id_path_safe "$TASK"; then
  fail_result unknown_identity "task id is not a valid identity; nothing was resolved"
fi

read_identity "$WORK/snapshot-before.json"
if [ "$SNAP_STATUS" != ok ]; then
  fail_result "$SNAP_STATUS" "$SNAP_REASON"
fi
E_GEN_CURRENT=$ROW_GEN
E_BACKEND=$ROW_BACKEND
STATE_DIR=$(jq -r '.roots.state // ""' "$WORK/snapshot-before.json")

if [ -z "$ROW_GEN" ]; then
  fail_result unavailable "Output unavailable: the snapshot records no spawn_gen for this task, so its incarnation cannot be verified"
fi
if [ "$GEN" != "$ROW_GEN" ]; then
  fail_result generation_mismatch "requested spawn_gen is not the current incarnation; no output returned"
fi

case " $SUPPORTED_BACKENDS " in
  *" $ROW_BACKEND "*) ;;
  *) fail_result unavailable "Output unavailable: backend '$ROW_BACKEND' has no validated observational capture on this machine" ;;
esac

if [ "$ROW_EXISTS" != true ] || [ -z "$ROW_TARGET" ]; then
  fail_result unavailable "Output unavailable: the snapshot reports no live endpoint for this task"
fi
# The recorded endpoint must be this task's own window, <session>:fm-<id>, the
# same binding fm_backend_validate_task_endpoint requires for tmux records.
ENDPOINT_SESSION=${ROW_TARGET%%:*}
if [ -z "$ENDPOINT_SESSION" ] || [ "$ENDPOINT_SESSION" = "$ROW_TARGET" ] \
  || [ "${ROW_TARGET#*:}" != "fm-$TASK" ]; then
  fail_result unavailable "Output unavailable: the recorded endpoint is not this task's own tmux window"
fi
command -v tmux >/dev/null 2>&1 || fail_result unavailable "Output unavailable: tmux is not installed"

if [ -z "$STATE_DIR" ]; then
  fail_result error "fleet snapshot reports no state directory; identity could not be re-verified"
fi
GEN_BEFORE=$(read_meta_gen)
if [ "$GEN_BEFORE" != "$GEN" ]; then
  E_GEN_CURRENT=$GEN_BEFORE
  fail_result generation_mismatch "spawn_gen changed before the capture; no output returned"
fi

# Resolve the recorded window by exact name. A session that cannot be listed, no
# window with that name, or several of them leave no single endpoint to read.
WINDOW_IDS=$(tmux list-windows -t "=$ENDPOINT_SESSION" -F '#{window_id} #{window_name}' 2>"$WORK/tmux.err" \
  | awk -v name="fm-$TASK" 'index($0, " ") && substr($0, index($0, " ") + 1) == name { print substr($0, 1, index($0, " ") - 1) }')
case "$WINDOW_IDS" in
  @[0-9]*) ;;
  *) fail_result unavailable "Output unavailable: the recorded window is missing" ;;
esac
case "$WINDOW_IDS" in
  *$'\n'*) fail_result unavailable "Output unavailable: the recorded window name is ambiguous" ;;
esac

RAW="$WORK/raw.txt"
E_CAPTURED_AT=$(date -u +%Y-%m-%dT%H:%M:%SZ)
if ! tmux capture-pane -p -t "$WINDOW_IDS" -S "-$LINES" >"$RAW" 2>"$WORK/tmux.err"; then
  fail_result unavailable "Output unavailable: tmux could not read the endpoint"
fi

# Re-verify the incarnation after the capture. A relaunch during the capture
# must not leave output from the old incarnation labelled as current.
GEN_AFTER=$(read_meta_gen)
if [ "$GEN_AFTER" != "$GEN" ]; then
  E_GEN_CURRENT=$GEN_AFTER
  fail_result generation_mismatch "spawn_gen changed during the capture; output discarded"
fi

# Strip terminal control sequences and leave plain text. Covers CSI, OSC, DCS,
# SOS/PM/APC, other ESC sequences, bare ESC, C1 controls (UTF-8 encoded), CR,
# NUL, and remaining C0 controls and DEL. Newline and tab are kept.
SANITIZED="$WORK/sanitized.txt"
perl -0777 -pe '
  s/\x1b\[[0-?]*[ -\/]*[@-~]//g;
  s/\x1b\][^\x07\x1b]*(?:\x07|\x1b\\)//g;
  s/\x1b[PX^_][^\x1b]*\x1b\\//g;
  s/\x1b[ -\/]*[0-~]//g;
  s/\xc2[\x80-\x9f]//g;
  tr/\x00-\x08\x0b\x0c\x0d\x0e-\x1f\x7f//d;
' "$RAW" >"$WORK/sanitized-padded.txt"

# capture-pane pads the viewport with empty rows. Drop trailing blank rows so
# the line bound and the truncation flag count content rows only.
awk '{ row[NR] = $0 } NF { last = NR } END { for (i = 1; i <= last; i++) print row[i] }' \
  "$WORK/sanitized-padded.txt" >"$SANITIZED"

AVAILABLE=$(awk 'END { print NR }' "$SANITIZED")
tail -n "$LINES" "$SANITIZED" >"$WORK/text.txt"
E_TEXT_FILE="$WORK/text.txt"
E_LINES_RETURNED=$(awk 'END { print NR }' "$WORK/text.txt")
if [ "$AVAILABLE" -gt "$LINES" ]; then
  E_TRUNCATED=true
fi
if [ "$CLAMPED" = 1 ]; then
  E_REASON="requested lines clamped to $MAX_LINES (v1 maximum)"
fi
E_STATUS=ok
emit
