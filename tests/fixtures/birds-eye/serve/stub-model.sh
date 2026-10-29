#!/usr/bin/env bash
# Test stub for bin/fm-birds-eye-model.sh, injected only by tests/fm-birds-eye-view.test.sh.
# Each invocation appends a "start <epoch> <argv>" line to FM_STUB_RECORD, then prints
# view.json with the home path substituted. Controls, read from FM_STUB_STATE:
#   fail-now  present: record a "fail" line and exit 1 without printing a document.
#   sleep     seconds to hold the cycle open before printing (default 0).
#   running   marker held for the cycle; a cycle that starts while it is present is
#             recorded as an "overlap" line.
# FM_STUB_HANG=1 instead starts a long sleep, writes its pid to FM_STUB_PIDFILE, and
# waits, which models a collection child still running at shutdown.
set -u

here=$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)
state=${FM_STUB_STATE:?}
record=${FM_STUB_RECORD:?}

epoch() {
  python3 -I -c 'import time; print(time.time())'
}

if [ -e "$state/fail-now" ]; then
  printf 'fail %s\n' "$(epoch)" >> "$record"
  printf 'stub model: failure requested\n' >&2
  exit 1
fi

printf 'start %s %s\n' "$(epoch)" "$*" >> "$record"
if [ -e "$state/running" ]; then
  printf 'overlap %s\n' "$(epoch)" >> "$record"
fi
: > "$state/running"
trap 'rm -f "$state/running"' EXIT

if [ "${FM_STUB_HANG:-0}" = 1 ]; then
  sleep 300 &
  printf '%s\n' "$!" > "${FM_STUB_PIDFILE:?}"
  wait
  exit 0
fi

delay=0
if [ -f "$state/sleep" ]; then
  delay=$(cat "$state/sleep")
fi
sleep "$delay"
sed "s|__HOME__|${FM_STUB_HOME:?}|g" "$here/view.json"
