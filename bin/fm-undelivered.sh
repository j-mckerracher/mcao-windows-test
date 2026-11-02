#!/usr/bin/env bash
# fm-undelivered.sh - record that a decision reached the captain (DPES-855).
#
# The undelivered-decision ledger ($STATE/.undelivered-decisions.jsonl, format
# owned by bin/fm-undelivered-lib.sh) keeps every decision raised to this home
# until a recorded delivery event. This command is that event. Supervising
# firstmate calls it after relaying a decision to the captain. bin/fm-send.sh
# calls it after an answer closes a decision with --resolve-key.
#
# Usage:
#   fm-undelivered.sh mark-delivered <task> <key>
#   fm-undelivered.sh --help
#
# mark-delivered is idempotent: an absent ledger, or a decision with no
# undelivered entry, succeeds without writing. Exit status: 0 on success, 1
# when the ledger exists and cannot be read, locked, or written, 2 on bad usage.
# It never closes a decision; OPEN DECISIONS and the fold own that.
set -eu

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
FM_ROOT="${FM_ROOT_OVERRIDE:-$(cd "$SCRIPT_DIR/.." && pwd)}"
FM_HOME="${FM_HOME:-${FM_ROOT_OVERRIDE:-$FM_ROOT}}"
STATE="${FM_STATE_OVERRIDE:-$FM_HOME/state}"

usage() {
  cat <<'EOF'
Usage:
  fm-undelivered.sh mark-delivered <task> <key>
  fm-undelivered.sh --help
EOF
}

case "${1:-}" in
  -h|--help|help) usage; exit 0 ;;
  mark-delivered) shift ;;
  '') usage >&2; exit 2 ;;
  *) echo "fm-undelivered: unknown command '$1'" >&2; usage >&2; exit 2 ;;
esac

[ "$#" -eq 2 ] || { usage >&2; exit 2; }
task=$1
key=$2
case "$task" in ''|*/*|*[[:space:]]*) echo "fm-undelivered: task '$task' is not a status-log name" >&2; exit 2 ;; esac
[ -n "$key" ] || { echo "fm-undelivered: key must be nonempty" >&2; exit 2; }

# shellcheck source=bin/fm-wake-lib.sh
# shellcheck disable=SC1091
. "$SCRIPT_DIR/fm-wake-lib.sh"
# shellcheck source=bin/fm-classify-lib.sh
# shellcheck disable=SC1091
. "$SCRIPT_DIR/fm-classify-lib.sh"
# shellcheck source=bin/fm-line-cap-lib.sh
# shellcheck disable=SC1091
. "$SCRIPT_DIR/fm-line-cap-lib.sh"
# shellcheck source=bin/fm-timeout-lib.sh
# shellcheck disable=SC1091
. "$SCRIPT_DIR/fm-timeout-lib.sh"
# shellcheck source=bin/fm-undelivered-lib.sh
# shellcheck disable=SC1091
. "$SCRIPT_DIR/fm-undelivered-lib.sh"

now=$(date +%s)
if ! fm_undelivered_mark_delivered "$STATE" "$task" "$key" "$now"; then
  echo "fm-undelivered: could not record delivery of $task [key=$key] in $(fm_undelivered_path "$STATE"); the decision stays undelivered and the drain will report it" >&2
  exit 1
fi
