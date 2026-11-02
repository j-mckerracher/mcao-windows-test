#!/usr/bin/env bash
# tests/seeded-status-flip.test.sh - oracle for the seeded-fault fixture bin/seeded-status-flip.sh.
ROOT=$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)
SELF="$ROOT/bin/seeded-status-flip.sh"
bash "$SELF" "$SELF" >/dev/null || exit 1
bash "$SELF" "$ROOT/tests/no-such-file" >/dev/null 2>&1 && exit 1
exit 0
