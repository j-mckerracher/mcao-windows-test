#!/usr/bin/env bash
# tests/seeded-eq-ne.test.sh - oracle for the seeded-fault fixture bin/seeded-eq-ne.sh.
ROOT=$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)
[ "$(bash "$ROOT/bin/seeded-eq-ne.sh" 0)" = zero ] || exit 1
[ "$(bash "$ROOT/bin/seeded-eq-ne.sh" 5)" = nonzero ] || exit 1
exit 0
