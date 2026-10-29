#!/usr/bin/env bash
# bin/seeded-eq-ne.sh - fixture production script: names whether its argument is zero.
set -u
if [ "$1" -eq 0 ]; then
  echo zero
else
  echo nonzero
fi
