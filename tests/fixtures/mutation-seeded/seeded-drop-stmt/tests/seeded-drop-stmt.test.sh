#!/usr/bin/env bash
# tests/seeded-drop-stmt.test.sh - oracle for the seeded-fault fixture bin/seeded-drop-stmt.sh.
ROOT=$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)
TMP=$(mktemp -d) || exit 1
trap 'rm -rf -- "$TMP"' EXIT
bash "$ROOT/bin/seeded-drop-stmt.sh" "$TMP/out" hello || exit 1
[ "$(cat "$TMP/out")" = hello ] || exit 1
exit 0
