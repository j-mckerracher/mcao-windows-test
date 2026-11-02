#!/usr/bin/env bash
# fm-mutation-classify.sh - classify each changed path of an MCAO-produced
# change by surface class and mutation disposition. bin/fm-mutation-surface-lib.sh
# owns the mapping; this file owns only the command-line contract.
#
# Usage:
#   fm-mutation-classify.sh <path>...
#   git diff --name-only <base>...HEAD | fm-mutation-classify.sh
#
# Output: one TAB-separated line per path: <path> <surface> <disposition> <detail>.
# Exit 0: every path matched a surface class.
# Exit 1: at least one path matched no surface class. Each such path is named on
#         stderr. Every path is still examined, and none is skipped, so the
#         classification step fails.
# Exit 2: usage error, including no paths at all, which is never a silent pass.
set -eu

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
# shellcheck source=bin/fm-mutation-surface-lib.sh
. "$SCRIPT_DIR/fm-mutation-surface-lib.sh"

usage() {
  echo "usage: fm-mutation-classify.sh <path>... | git diff --name-only <base>...HEAD | fm-mutation-classify.sh" >&2
}

case "${1:-}" in
  -h|--help)
    usage
    exit 0
    ;;
esac

if [ "$#" -eq 0 ]; then
  while IFS= read -r line; do
    [ -n "$line" ] || continue
    set -- "$@" "$line"
  done
fi

if [ "$#" -eq 0 ]; then
  echo "fm-mutation-classify: no changed paths given" >&2
  usage
  exit 2
fi

status=0
for path in "$@"; do
  if record=$(fm_mutation_classify_path "$path"); then
    printf '%s\t%s\n' "$path" "$record"
  else
    printf 'fm-mutation-classify: unclassified changed path (matches no surface class; refusing to skip): %s\n' "$path" >&2
    status=1
  fi
done
exit "$status"
