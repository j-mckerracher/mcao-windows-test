#!/usr/bin/env bash
# fm-oracle-map-check.sh - fail when an in-scope Python, TypeScript or JavaScript
# production path has no valid row in the path-to-oracle map.
#
# Usage:
#   bin/fm-oracle-map-check.sh [--root <repo>] [--map <file>]
#
# In-scope paths are the tracked files (git ls-files, symlinks included) of the
# repository whose surface class from bin/fm-mutation-surface-lib.sh is python,
# typescript or javascript. tests/** and */tests/** are test code, the oracle
# itself, and are never in scope. Every other surface class is out of scope.
#
# Each in-scope path needs exactly one row of six TAB-separated columns:
#   map  <path>  <surface>  <oracle>  <status>  <detail>
# Status is one of executed, missing, skipping, zero-assertion, error or
# unresolved. An executed row must name its oracle. A row whose path is not an
# in-scope tracked path is stale and fails. The map's meaning is owned by
# docs/mutation-oracle-map.md.
#
# Exit codes: 0 every in-scope path has one valid row; 1 at least one row is
# missing, stale, duplicated, malformed or invalid (each named on stderr);
# 2 usage error or unreadable repository or map.
set -u

SELF_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd -P)"
ROOT="$(cd "$SELF_DIR/.." && pwd -P)"
MAP=""

usage() {
  echo "usage: bin/fm-oracle-map-check.sh [--root <repo>] [--map <file>]" >&2
}

while [ $# -gt 0 ]; do
  case "$1" in
    --root)
      [ $# -ge 2 ] || { usage; exit 2; }
      ROOT=$(cd -P -- "$2" 2>/dev/null && pwd -P) || { echo "fm-oracle-map-check: no such directory: $2" >&2; exit 2; }
      shift 2
      ;;
    --map)
      [ $# -ge 2 ] || { usage; exit 2; }
      MAP=$2
      shift 2
      ;;
    -h | --help)
      usage
      exit 0
      ;;
    *)
      usage
      exit 2
      ;;
  esac
done

[ -n "$MAP" ] || MAP="$ROOT/docs/mutation-oracle-map.tsv"

git -C "$ROOT" rev-parse --show-toplevel >/dev/null 2>&1 || {
  echo "fm-oracle-map-check: not a git repository: $ROOT" >&2
  exit 2
}
[ -r "$MAP" ] || {
  echo "fm-oracle-map-check: map not readable: $MAP" >&2
  exit 2
}

# shellcheck source=bin/fm-mutation-surface-lib.sh
. "$SELF_DIR/fm-mutation-surface-lib.sh"

TMP=$(mktemp -d "${TMPDIR:-/tmp}/fm-oracle-map-check.XXXXXX") || exit 2
trap 'rm -rf "$TMP"' EXIT

# In-scope paths: tracked files whose surface class is python, typescript or javascript.
git -C "$ROOT" ls-files -z | while IFS= read -r -d '' p; do
  s=$(fm_mutation_surface_of "$p") || continue
  case "$s" in
    python | typescript | javascript) printf '%s\n' "$p" ;;
  esac
done | LC_ALL=C sort -u > "$TMP/scope"

# Map rows: report every malformed, duplicate or invalid row, and list the valid paths.
awk -F'\t' -v mapped="$TMP/mapped" '
  /^#/ || /^$/ { next }
  NF != 6 || $1 != "map" {
    print "malformed row at line " NR ": expected six tab-separated columns starting with map"
    next
  }
  $2 == "" {
    print "row at line " NR " names no path"
    next
  }
  seen[$2]++ {
    print "duplicate row for " $2 " at line " NR
    next
  }
  $5 !~ /^(executed|missing|skipping|zero-assertion|error|unresolved)$/ {
    print "row for " $2 " has unknown status \"" $5 "\""
    next
  }
  $5 == "executed" && ($4 == "" || $4 == "-") {
    print "executed row for " $2 " names no oracle"
    next
  }
  { print $2 > mapped }
' "$MAP" > "$TMP/problems"
[ -f "$TMP/mapped" ] || : > "$TMP/mapped"
LC_ALL=C sort -u "$TMP/mapped" > "$TMP/mapped.sorted"

LC_ALL=C comm -23 "$TMP/scope" "$TMP/mapped.sorted" | sed 's/^/no map row for in-scope path: /' >> "$TMP/problems"
LC_ALL=C comm -13 "$TMP/scope" "$TMP/mapped.sorted" | sed 's/^/map row for a path that is not in scope: /' >> "$TMP/problems"

if [ -s "$TMP/problems" ]; then
  sed 's/^/fm-oracle-map-check: /' "$TMP/problems" >&2
  exit 1
fi

echo "oracle map: $(wc -l < "$TMP/scope" | tr -d ' ') in-scope paths, each with one valid row"
