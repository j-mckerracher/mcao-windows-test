#!/usr/bin/env bash
# fm-birds-eye-view.sh - loopback Bird's Eye View live service and snapshot CLI.
#
# Usage:
#   fm-birds-eye-view.sh [--root <home-dir>]... [--interval <seconds>] [--port <port>] [--open]
#   fm-birds-eye-view.sh --snapshot <path> [--root <home-dir>]... [--interval <seconds>]
#
# Live mode serves a dashboard on 127.0.0.1 until SIGINT or SIGTERM. The configured
# home (FM_HOME, default the code root) is always observed, and each --root adds
# another operational home.
#   --root <home-dir>   Extra operational home to observe. Repeatable.
#   --interval <s>      Refresh interval in whole seconds. Default 10. Minimum 5;
#                       a lower value is refused, never clamped.
#   --port <port>       Fixed loopback port, 1-65535. Default: a free port chosen by
#                       the OS. A port already in use is an error; nothing else is
#                       replaced.
#   --open              Open the dashboard URL in the default browser. When no opener
#                       is available, the URL is printed with a note and the service
#                       keeps running.
#   --snapshot <path>   Write one fm-birds-eye-view.v1 document through
#                       bin/fm-birds-eye-export.sh and exit. Starts no listener and
#                       no collector.
#   -h, --help          Print this usage and exit 0.
#
# Output: the live URL and a working-files line on stdout. The working files are a
# temporary directory removed on exit. A snapshot is written to <path> as given.
#
# Exit codes:
#   0  live service stopped normally, snapshot written, or help printed.
#   1  runtime failure: model helper missing (live mode), exporter missing or failed
#      (snapshot), port in use, bind failure, or dashboard assets unreadable.
#   2  usage error: unknown flag, bad value, interval below 5, port out of range, or
#      --snapshot combined with --port or --open.
#
# Shutdown: SIGINT or SIGTERM stops the HTTP listener and the collector, signals only
# the observer's own child process groups, and removes the temporary directory. Agent
# sessions are never touched.
#
# Environment:
#   FM_HOME                       Configured home observed first. Default: the code root.
#   FM_BIRDS_EYE_MODEL_OVERRIDE   Test seam: replaces bin/fm-birds-eye-model.sh.
#   FM_BIRDS_EYE_CAPTURE_OVERRIDE Test seam: replaces bin/fm-birds-eye-capture.sh.
#   FM_BIRDS_EYE_EXPORT_OVERRIDE  Test seam: replaces bin/fm-birds-eye-export.sh.
#   The override variables exist for tests of the interfaces only. Production runs
#   must not set them.
#
# Dependencies: bash, python3 (standard library only), and the sibling helpers
# bin/fm-birds-eye-model.sh, bin/fm-birds-eye-capture.sh, and bin/fm-birds-eye-export.sh.
# Reads: bin/fm-birds-eye-view-serve.py and assets/birds-eye-view/.

set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
CODE_ROOT="$(cd "$SCRIPT_DIR/.." && pwd)"
ASSETS="$CODE_ROOT/assets/birds-eye-view"
SERVE="$SCRIPT_DIR/fm-birds-eye-view-serve.py"
MODEL="${FM_BIRDS_EYE_MODEL_OVERRIDE:-$SCRIPT_DIR/fm-birds-eye-model.sh}"
CAPTURE="${FM_BIRDS_EYE_CAPTURE_OVERRIDE:-$SCRIPT_DIR/fm-birds-eye-capture.sh}"
EXPORT="${FM_BIRDS_EYE_EXPORT_OVERRIDE:-$SCRIPT_DIR/fm-birds-eye-export.sh}"

usage() {
  cat <<'EOF'
usage: fm-birds-eye-view.sh [--root <home-dir>]... [--interval <seconds>] [--port <port>] [--open]
       fm-birds-eye-view.sh --snapshot <path> [--root <home-dir>]... [--interval <seconds>]

  --root <home-dir>   extra operational home to observe; repeatable
  --interval <s>      refresh interval, whole seconds, default 10, minimum 5
  --port <port>       fixed loopback port 1-65535; default a free port
  --open              open the dashboard in the default browser when available
  --snapshot <path>   write one document through the exporter and exit
  -h, --help          print this usage

Exit: 0 success, 1 runtime failure, 2 usage error.
EOF
}

usage_error() {
  printf 'fm-birds-eye-view: %s\n' "$1" >&2
  usage >&2
  exit 2
}

fail() {
  printf 'fm-birds-eye-view: %s\n' "$1" >&2
  exit 1
}

canonical_dir() {
  if [ -d "$1" ]; then
    (cd "$1" && pwd -P)
  else
    printf '%s' "$1"
  fi
}

add_root() {
  local canon existing
  [ -n "$1" ] || usage_error "--root needs a non-empty directory"
  canon="$(canonical_dir "$1")"
  for existing in "${ROOTS[@]}"; do
    if [ "$existing" = "$canon" ]; then
      return 0
    fi
  done
  ROOTS+=("$canon")
}

FM_HOME_RAW="${FM_HOME:-$CODE_ROOT}"
[ -d "$FM_HOME_RAW" ] || fail "configured home is not a directory: $FM_HOME_RAW"
ROOTS=("$(canonical_dir "$FM_HOME_RAW")")

INTERVAL=10
PORT=""
OPEN=0
SNAPSHOT=""
while [ "$#" -gt 0 ]; do
  case "$1" in
    --root)
      [ "$#" -ge 2 ] || usage_error "--root needs a value"
      add_root "$2"
      shift 2
      ;;
    --interval)
      [ "$#" -ge 2 ] || usage_error "--interval needs a value"
      INTERVAL=$2
      shift 2
      ;;
    --port)
      [ "$#" -ge 2 ] || usage_error "--port needs a value"
      PORT=$2
      shift 2
      ;;
    --snapshot)
      [ "$#" -ge 2 ] || usage_error "--snapshot needs a value"
      SNAPSHOT=$2
      shift 2
      ;;
    --open)
      OPEN=1
      shift
      ;;
    -h|--help)
      usage
      exit 0
      ;;
    *)
      usage_error "unrecognized argument: $1"
      ;;
  esac
done

case "$INTERVAL" in
  ''|*[!0-9]*) usage_error "--interval must be a whole number of seconds; got $INTERVAL" ;;
esac
if [ "${#INTERVAL}" -gt 6 ] || [ "$INTERVAL" -lt 5 ] || [ "$INTERVAL" -gt 999999 ]; then
  usage_error "--interval must be from 5 to 999999 seconds; got $INTERVAL"
fi

if [ -n "$PORT" ]; then
  case "$PORT" in
    ''|*[!0-9]*) usage_error "--port must be a whole number from 1 to 65535; got $PORT" ;;
  esac
  if [ "${#PORT}" -gt 5 ] || [ "$PORT" -lt 1 ] || [ "$PORT" -gt 65535 ]; then
    usage_error "--port must be a whole number from 1 to 65535; got $PORT"
  fi
fi

if [ -n "$SNAPSHOT" ]; then
  if [ -n "$PORT" ] || [ "$OPEN" = 1 ]; then
    usage_error "--snapshot starts no listener; drop --port and --open"
  fi
  if [ ! -f "$EXPORT" ] || [ ! -x "$EXPORT" ]; then
    fail "snapshot exporter is not available at $EXPORT; the snapshot cannot be written"
  fi
  EXPORT_ARGS=()
  for root in "${ROOTS[@]}"; do
    EXPORT_ARGS+=(--root "$root")
  done
  if ! "$EXPORT" "${EXPORT_ARGS[@]}" --interval "$INTERVAL" --output "$SNAPSHOT"; then
    fail "snapshot export failed; see the exporter message above"
  fi
  printf 'snapshot written: %s\n' "$SNAPSHOT"
  exit 0
fi

command -v python3 >/dev/null 2>&1 || fail "python3 is not available; the live view cannot run"

SERVE_ARGS=(--assets "$ASSETS" --model "$MODEL" --capture "$CAPTURE" --interval "$INTERVAL")
for root in "${ROOTS[@]}"; do
  SERVE_ARGS+=(--root "$root")
done
if [ -n "$PORT" ]; then
  SERVE_ARGS+=(--port "$PORT")
fi
if [ "$OPEN" = 1 ]; then
  SERVE_ARGS+=(--open)
fi

exec python3 -I "$SERVE" "${SERVE_ARGS[@]}"
