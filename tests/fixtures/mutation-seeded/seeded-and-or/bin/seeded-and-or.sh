#!/usr/bin/env bash
# bin/seeded-and-or.sh - fixture production script: names whether both arguments are non-empty.
set -u
if [ -n "$1" ] && [ -n "$2" ]; then
  echo both
else
  echo partial
fi
