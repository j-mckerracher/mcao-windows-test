#!/usr/bin/env bash
# Test stub for bin/fm-birds-eye-export.sh, injected only by tests/fm-birds-eye-view.test.sh.
# Logs its arguments to FM_STUB_EXPORT_RECORD, fails when FM_STUB_EXPORT_FAIL=1, and
# otherwise writes a marker fm-birds-eye-view.v1 document to the --output path.
set -u

printf '%s\n' "$*" >> "${FM_STUB_EXPORT_RECORD:?}"
if [ "${FM_STUB_EXPORT_FAIL:-0}" = 1 ]; then
  printf 'stub export: failure requested\n' >&2
  exit 1
fi

output=""
while [ "$#" -ge 2 ]; do
  case "$1" in
    --output) output=$2 ;;
  esac
  shift 2
done
if [ -z "$output" ]; then
  printf 'stub export: no --output given\n' >&2
  exit 2
fi
printf '{"schema":"fm-birds-eye-view.v1","stub":true}\n' > "$output"
