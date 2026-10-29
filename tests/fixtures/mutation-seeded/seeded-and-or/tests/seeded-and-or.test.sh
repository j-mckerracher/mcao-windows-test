#!/usr/bin/env bash
# tests/seeded-and-or.test.sh - oracle for the seeded-fault fixture bin/seeded-and-or.sh.
ROOT=$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)
[ "$(bash "$ROOT/bin/seeded-and-or.sh" a b)" = both ] || exit 1
[ "$(bash "$ROOT/bin/seeded-and-or.sh" a "")" = partial ] || exit 1
exit 0
