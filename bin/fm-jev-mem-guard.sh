#!/usr/bin/env bash
# fm-jev-mem-guard.sh - Wrapper for Jev Multi-Agent Memory RSS & Swap Thrashing Guard (Pattern 46)
#
# Runs `mcao mem-guard`, the Rust port of the Python reference (DPES-864). FM_MCAO_BIN wins;
# otherwise mcao is looked up on PATH.
set -euo pipefail

mcao=${FM_MCAO_BIN:-}
[ -n "$mcao" ] || mcao=$(command -v mcao || true)
[ -n "$mcao" ] || {
  echo "fm-jev-mem-guard: mcao is required (set FM_MCAO_BIN or put mcao on PATH)." >&2
  exit 2
}

exec "$mcao" mem-guard "$@"
