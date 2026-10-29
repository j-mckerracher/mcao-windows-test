#!/usr/bin/env bash
# bin/seeded-drop-stmt.sh - fixture production script: writes its second argument to the file named by its first.
set -u
out=$1
printf '%s\n' "$2" >"$out"
