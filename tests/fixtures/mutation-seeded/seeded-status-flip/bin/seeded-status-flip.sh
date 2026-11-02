#!/usr/bin/env bash
# bin/seeded-status-flip.sh - fixture production script: prints a readable file and fails on any other path.
set -u
if [ ! -r "$1" ]; then
  echo "unreadable: $1" >&2
  exit 1
fi
cat "$1"
