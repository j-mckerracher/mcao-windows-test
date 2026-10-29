#!/usr/bin/env bash
# fm-birds-eye-model.sh - observer-only bird's-eye aggregation for fm-birds-eye-view.v1.
#
# Usage:
#   fm-birds-eye-model.sh [--root <home-dir>]... [--interval <seconds>]
#                         [--now <epoch>] [--previous <file>]
#                         [--fixture-dir <dir>]
#   fm-birds-eye-model.sh -h | --help
#
# Prints one fm-birds-eye-view.v1 JSON document on stdout. The output contract is
# docs/birds-eye-view-contract.md; the aggregation rules live in
# bin/fm-birds-eye-model.jq, which this script feeds one wrapper per root attempt.
#
# Roots. The configured home (FM_HOME, else this checkout) is always read first. Each --root
# adds an explicit root after it; a root that names the same home is read once.
# Descendant homes are followed only through the producer's own relationships:
# local secondmate task rows whose paths.home.path is set, and local
# secondmate_current.records[].home. A remote secondmate's home is a path on another
# machine, so it is never read here and is reported in unsupported[] as remote_home.
# Nothing is found by scanning the filesystem.
# Each home is read once, keyed by its canonical path, which is also its root_id.
#
# Bounds. A run reads at most 64 roots. Descendants are followed to any depth; a home
# reached again through a cycle or a second path is read once. Roots past the bound are
# listed in unsupported[] with a count, never silently dropped.
#
# Producer. Each root is read by running fm-fleet-snapshot.sh --json with FM_HOME set
# to that root and the observation clock pinned to --now, bounded to 30 seconds by
# bin/fm-timeout-lib.sh. That producer refreshes its own observational
# state/secondmate-summary-cache copies; those writes are the only side effect of
# this aggregator. Nothing else is written, and no raw producer output is cached.
#
# Failure and last-good records. A root that cannot be read (missing directory,
# nonzero exit, timeout), returns malformed output, an unsupported schema, or an
# identity mismatch is reported in roots[] with its own error. Its last-good
# records come from --previous, marked stale with their age, while other roots
# render normally. A root that recovers clears its error without any task
# transition. A descendant whose parent root could not be read is reported as not
# reached and keeps its last-good records the same way.
#
# Interval. --interval (default 10, minimum 5) sets refresh.interval_seconds. A root
# is stale once its last good collection is older than max(30, 3 * interval)
# seconds.
#
# --previous <file> is additive: it names a prior fm-birds-eye-view.v1 output whose
# last-good records and history carry forward. It is the only cross-run memory.
# History does not survive a restart unless the caller supplies that file.
#
# Exit status: 0 when a document is rendered (failed roots are reported inside it),
# 2 on a usage error (including --interval below 5 seconds), 1 when the aggregation
# itself cannot run.
#
# --fixture-dir <dir> is test-only. It replaces the producer with recorded snapshots
# listed in <dir>/manifest.json:
#   {"configured": ["<home>", ...],
#    "homes": {"<home>": {"file": "<snapshot-file>", "exit": 0}}}
# A home missing from "homes" is reported as unreadable. A nonzero "exit" reports a
# failed producer run with that status.

set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
# shellcheck source=bin/fm-timeout-lib.sh
. "$SCRIPT_DIR/fm-timeout-lib.sh"

MAX_ROOTS=64
PRODUCER_TIMEOUT_SECONDS=30
NL=$'\n'

usage() {
  cat <<'EOF'
Usage: fm-birds-eye-model.sh [--root <home-dir>]... [--interval <seconds>]
                             [--now <epoch>] [--previous <file>]
                             [--fixture-dir <dir>]
Prints one fm-birds-eye-view.v1 document on stdout.
EOF
}

die_usage() {
  printf 'fm-birds-eye-model: %s\n' "$1" >&2
  printf 'Run fm-birds-eye-model.sh --help for usage.\n' >&2
  exit 2
}

is_uint() {
  case "$1" in
    ''|*[!0-9]*) return 1 ;;
  esac
  [ "${#1}" -le 15 ]
}

EXPLICIT_ROOTS=()
INTERVAL=10
NOW=""
PREVIOUS=""
FIXTURE_DIR=""

while [ "$#" -gt 0 ]; do
  case "$1" in
    --root)
      [ "$#" -ge 2 ] || die_usage "--root requires a directory"
      EXPLICIT_ROOTS+=("$2")
      shift 2
      ;;
    --interval)
      [ "$#" -ge 2 ] || die_usage "--interval requires a value"
      INTERVAL=$2
      shift 2
      ;;
    --now)
      [ "$#" -ge 2 ] || die_usage "--now requires an epoch in seconds"
      NOW=$2
      shift 2
      ;;
    --previous)
      [ "$#" -ge 2 ] || die_usage "--previous requires a file"
      PREVIOUS=$2
      shift 2
      ;;
    --fixture-dir)
      [ "$#" -ge 2 ] || die_usage "--fixture-dir requires a directory"
      FIXTURE_DIR=$2
      shift 2
      ;;
    -h|--help)
      usage
      exit 0
      ;;
    *)
      die_usage "unknown argument: $1"
      ;;
  esac
done

is_uint "$INTERVAL" || die_usage "--interval must be a whole number of seconds (got '$INTERVAL')"
if [ "$INTERVAL" -lt 5 ]; then
  die_usage "--interval must be at least 5 seconds (got $INTERVAL)"
fi
if [ -z "$NOW" ]; then
  NOW=$(date +%s)
fi
is_uint "$NOW" || die_usage "--now must be an epoch in seconds (got '$NOW')"

if [ -n "$PREVIOUS" ]; then
  [ -f "$PREVIOUS" ] || die_usage "--previous file not found: $PREVIOUS"
  jq -e 'type == "object" and .schema == "fm-birds-eye-view.v1"' "$PREVIOUS" >/dev/null 2>&1 \
    || die_usage "--previous must name a fm-birds-eye-view.v1 document: $PREVIOUS"
fi

MANIFEST=""
if [ -n "$FIXTURE_DIR" ]; then
  MANIFEST="$FIXTURE_DIR/manifest.json"
  [ -f "$MANIFEST" ] || die_usage "--fixture-dir has no manifest.json: $FIXTURE_DIR"
fi

TMP_DIR=$(mktemp -d "${TMPDIR:-/tmp}/fm-birds-eye.XXXXXX")
trap 'rm -rf "$TMP_DIR"' EXIT
: > "$TMP_DIR/roots.jsonl"

# Queue of root attempts, processed breadth-first in discovery order.
Q_PATH=()
Q_SOURCE=()
Q_PARENT=()
SEEN="$NL"
ROOT_SKIPPED=0
ENQ_CANON=""

canonical_path() {
  if [ -n "$FIXTURE_DIR" ]; then
    printf '%s\n' "$1"
  elif [ -d "$1" ]; then
    (cd "$1" && pwd -P)
  else
    printf '%s\n' "$1"
  fi
}

# enqueue <raw-path> <source> <parent-root-id-or-empty>
# Every reference is recorded in SEEN before the root bound is checked, so a home that is
# referenced again (including through a cycle) or is out of bounds is counted once.
# Discovery is breadth-first, so a home's first reference is always its shallowest one.
enqueue() {
  local canon
  canon=$(canonical_path "$1")
  ENQ_CANON=$canon
  case "$SEEN" in
    *"$NL$canon$NL"*) return 0 ;;
  esac
  SEEN="$SEEN$canon$NL"
  if [ "${#Q_PATH[@]}" -ge "$MAX_ROOTS" ]; then
    ROOT_SKIPPED=$((ROOT_SKIPPED + 1))
    return 0
  fi
  Q_PATH+=("$canon")
  Q_SOURCE+=("$2")
  Q_PARENT+=("$3")
}

# Each reader sets READ_RC and READ_ERR and writes producer output to its second argument.
READ_RC=0
READ_ERR=""

read_fixture() {  # <home-path> <out>
  local entry file exit_code
  entry=$(jq -r --arg p "$1" '.homes[$p] | select(. != null) | "\(.file)\t\(.exit // 0)"' "$MANIFEST" 2>/dev/null || true)
  if [ -z "$entry" ]; then
    READ_RC=1
    READ_ERR="no recorded snapshot for this home"
    return 0
  fi
  file=${entry%%$'\t'*}
  exit_code=${entry#*$'\t'}
  if [ "$exit_code" != 0 ]; then
    READ_RC=$exit_code
    READ_ERR="producer exited $exit_code"
    return 0
  fi
  if cp "$FIXTURE_DIR/$file" "$2" 2>/dev/null; then
    READ_RC=0
    READ_ERR=""
  else
    READ_RC=1
    READ_ERR="recorded snapshot unreadable"
  fi
}

read_live() {  # <home-path> <out>
  local now_iso rc detail
  if [ ! -d "$1" ]; then
    READ_RC=1
    READ_ERR="home directory not found"
    return 0
  fi
  now_iso=$(jq -rn --argjson t "$NOW" '$t | todate')
  if fm_run_timed "$PRODUCER_TIMEOUT_SECONDS" env \
      -u FM_ROOT_OVERRIDE -u FM_STATE_OVERRIDE -u FM_DATA_OVERRIDE -u FM_CONFIG_OVERRIDE \
      -u FM_PROJECTS_OVERRIDE -u FM_SNAPSHOT_NOW -u FM_SNAPSHOT_NOW_EPOCH \
      FM_HOME="$1" FM_SNAPSHOT_NOW="$now_iso" FM_SNAPSHOT_NOW_EPOCH="$NOW" \
      bash "$SCRIPT_DIR/fm-fleet-snapshot.sh" --json >"$2" 2>"$2.err"; then
    READ_RC=0
    READ_ERR=""
  else
    rc=$?
    READ_RC=$rc
    if fm_timed_out "$rc"; then
      READ_ERR="producer timed out after $PRODUCER_TIMEOUT_SECONDS seconds"
    else
      detail=$(head -n 1 "$2.err" 2>/dev/null | cut -c1-200 || true)
      READ_ERR="producer exited $rc"
      if [ -n "$detail" ]; then
        READ_ERR="$READ_ERR: $detail"
      fi
    fi
  fi
}

# Discover descendants from a readable producer snapshot. Only the producer's own
# secondmate relationships are followed. Each reference is also written to
# refs-<index>.jsonl as {raw, canonical}, so the aggregation can match the producer's
# home path to the canonical root it was read as.
expand_root() {  # <queue-index> <out>
  local i=$1 desc p
  desc="$TMP_DIR/desc-$i.txt"
  jq -r '((.tasks[]? | select(.kind == "secondmate" and .remote == null) | .paths.home.path? | strings),
          (.secondmate_current.records[]? | select(.remote != true) | .home? | strings))
         | select(. != "")' "$2" > "$desc" 2>/dev/null || true
  while IFS= read -r p; do
    enqueue "$p" descendant "${Q_PATH[$i]}"
    jq -cn --arg raw "$p" --arg canonical "$ENQ_CANON" '{raw:$raw, canonical:$canonical}' >> "$TMP_DIR/refs-$i.jsonl"
  done < "$desc"
}

read_root() {  # <queue-index>
  local i=$1 out="$TMP_DIR/root-$1.json" transport=ok terr="" body=/dev/null raw=false
  if [ -n "$FIXTURE_DIR" ]; then
    read_fixture "${Q_PATH[$i]}" "$out"
  else
    read_live "${Q_PATH[$i]}" "$out"
  fi
  if [ "$READ_RC" -ne 0 ]; then
    transport=failed
    terr=$READ_ERR
  elif jq -e . "$out" >/dev/null 2>&1; then
    raw=true
    body=$out
  fi
  : > "$TMP_DIR/refs-$i.jsonl"
  if [ "$transport" = ok ] && [ "$raw" = true ]; then
    expand_root "$i" "$out"
  fi
  jq -c -n \
    --arg root_id "${Q_PATH[$i]}" \
    --arg home "${Q_PATH[$i]}" \
    --arg source "${Q_SOURCE[$i]}" \
    --arg parent "${Q_PARENT[$i]}" \
    --arg transport "$transport" \
    --arg terr "$terr" \
    --argjson raw "$raw" \
    --slurpfile body "$body" \
    --slurpfile refs "$TMP_DIR/refs-$i.jsonl" \
    '{root_id:$root_id, home_path:$home, source:$source,
      parent_root_id:($parent | if . == "" then null else . end),
      transport_status:$transport,
      transport_error:($terr | if . == "" then null else . end),
      raw_valid:$raw, payload:($body[0] // null), refs:$refs}' >> "$TMP_DIR/roots.jsonl"
}

if [ -n "$FIXTURE_DIR" ]; then
  while IFS= read -r r; do
    enqueue "$r" configured ""
  done < <(jq -r '.configured[]?' "$MANIFEST")
else
  enqueue "${FM_HOME:-$SCRIPT_DIR/..}" configured ""
fi
for r in ${EXPLICIT_ROOTS[@]+"${EXPLICIT_ROOTS[@]}"}; do
  enqueue "$r" explicit ""
done

i=0
while [ "$i" -lt "${#Q_PATH[@]}" ]; do
  read_root "$i"
  i=$((i + 1))
done

SHELL_UNSUP='[]'
if [ "$ROOT_SKIPPED" -gt 0 ]; then
  SHELL_UNSUP=$(jq -cn --argjson a "$SHELL_UNSUP" --argjson n "$ROOT_SKIPPED" --argjson m "$MAX_ROOTS" \
    '$a + [{category:"root_limit", reason:"\($n) home reference(s) not read: a run reads at most \($m) roots"}]')
fi

CONFIG=$(jq -cn --argjson now "$NOW" --argjson interval "$INTERVAL" --argjson shell "$SHELL_UNSUP" \
  '{now:$now, interval:$interval, shell_unsupported:$shell}')

PREV_FILE="$TMP_DIR/previous.json"
if [ -n "$PREVIOUS" ]; then
  PREV_FILE=$PREVIOUS
else
  printf 'null\n' > "$PREV_FILE"
fi

jq -n -f "$SCRIPT_DIR/fm-birds-eye-model.jq" \
  --argjson config "$CONFIG" \
  --slurpfile roots "$TMP_DIR/roots.jsonl" \
  --slurpfile prev "$PREV_FILE"
