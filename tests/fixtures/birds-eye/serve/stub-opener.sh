#!/usr/bin/env bash
# Test stand-in for the platform opener (open or xdg-open), reached through symlinks
# placed first on PATH by tests/fm-birds-eye-view.test.sh. Logs the URL it was given
# and exits with FM_STUB_OPEN_EXIT (default 0). It never launches a browser.
set -u

printf '%s\n' "${1:-}" >> "${FM_STUB_OPEN_RECORD:?}"
exit "${FM_STUB_OPEN_EXIT:-0}"
