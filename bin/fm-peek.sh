#!/usr/bin/env bash
# Print the tail of a crewmate endpoint (bounded, for cheap diagnosis).
# Usage: fm-peek.sh <target> [lines=40]
#   <target> may be an exact task id, a legacy fm-<id> task label resolved
#   through this home's state/<id>.meta, or an explicit backend target.
#   A gcp task has no terminal: its tail is the last <N> lines of the recorded
#   run's logs from the cloud client (contract: bin/fm-execution-target-lib.sh).
set -eu

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
FM_ROOT="${FM_ROOT_OVERRIDE:-$(cd "$SCRIPT_DIR/.." && pwd)}"
FM_HOME="${FM_HOME:-${FM_ROOT_OVERRIDE:-$FM_ROOT}}"
STATE="${FM_STATE_OVERRIDE:-$FM_HOME/state}"

# shellcheck source=bin/fm-backend.sh
. "$SCRIPT_DIR/fm-backend.sh"
# shellcheck source=bin/fm-execution-target-lib.sh
. "$SCRIPT_DIR/fm-execution-target-lib.sh"

"$SCRIPT_DIR/fm-guard.sh" || true

RAW_TARGET=$1
N=${2:-40}

REMOTE_META=$(fm_backend_meta_for_selector "$RAW_TARGET" "$STATE" 2>/dev/null || true)
if [ -n "$REMOTE_META" ] && [ -n "$(fm_meta_get "$REMOTE_META" remote_host)" ]; then
  echo "error: remote secondmate placement is not supported; cannot peek remote pane" >&2
  exit 1
fi

# The cloud read is bounded locally: the client's full log output is captured and
# only its last N lines are printed. A failed read exits with the client's status.
PEEK_META=$(fm_backend_meta_for_selector "$RAW_TARGET" "$STATE" 2>/dev/null || true)
if [ -n "$PEEK_META" ]; then
  PEEK_TARGET=$(fm_exec_record_resolve "$PEEK_META") || exit 1
  if [ "$PEEK_TARGET" = "$FM_EXEC_TARGET_GCP" ]; then
    PEEK_RUN=$(fm_meta_get "$PEEK_META" cloud_run_id)
    PEEK_LOGS=$(fm_exec_cloud_run logs --run-id "$PEEK_RUN") || exit $?
    printf '%s\n' "$PEEK_LOGS" | tail -n "$N"
    exit 0
  fi
fi

T=$(fm_backend_resolve_selector "$RAW_TARGET" "$STATE")

BACKEND=$(fm_backend_of_selector "$RAW_TARGET" "$T" "$STATE")
EXPECTED_LABEL=$(fm_backend_expected_label_of_selector "$RAW_TARGET" "$STATE")

fm_backend_capture "$BACKEND" "$T" "$N" "$EXPECTED_LABEL"
