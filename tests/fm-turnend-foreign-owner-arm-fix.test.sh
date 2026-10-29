#!/usr/bin/env bash

# Clear inherited operational-home path variables, before any bin/ script runs.
# Tests must be isolated and not read from or write to the caller's firstmate home. DPES-698.
for name in FM_HOME FM_BACKEND FM_ROOT_OVERRIDE FM_STATE_OVERRIDE FM_DATA_OVERRIDE FM_PROJECTS_OVERRIDE FM_CONFIG_OVERRIDE FM_CREW_STATE_META_OVERRIDE FM_CREW_STATE_STATUS_OVERRIDE FM_PENDING_REPLY_DIR_OVERRIDE FM_PROC_ROOT_OVERRIDE FM_TIMEOUT_MECHANISM_OVERRIDE; do
  unset "$name"
done

# Regression for the live foreign session-lock owner and non-owner Stop loop.
# The executable reproduction runs the real auto-arm and turn-end guard paths.
set -eu

python3 "$(dirname "${BASH_SOURCE[0]}")/fm-turnend-foreign-owner-repro.py"
