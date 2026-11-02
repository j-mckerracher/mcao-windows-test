#!/usr/bin/env bash
# tests/fm-test-env-isolation.test.sh - regression test for environment variable isolation
# Verifies that tests do not inherit operative home paths from the caller's environment.

set -u

# shellcheck source=tests/lib.sh
. "$(dirname "${BASH_SOURCE[0]}")/lib.sh"

# SCRUB_LIST: variables that must be unset before any test helper runs
# Computed per Step 2 of DPES-698: base set plus any FM_*_OVERRIDE from bin/
SCRUB_LIST="FM_HOME FM_BACKEND FM_ROOT_OVERRIDE FM_STATE_OVERRIDE FM_DATA_OVERRIDE FM_PROJECTS_OVERRIDE FM_CONFIG_OVERRIDE FM_CREW_STATE_META_OVERRIDE FM_CREW_STATE_STATUS_OVERRIDE FM_PENDING_REPLY_DIR_OVERRIDE FM_PROC_ROOT_OVERRIDE FM_TIMEOUT_MECHANISM_OVERRIDE"

TMP_ROOT=$(fm_test_tmproot "env-isolation")

test_library_clears_inherited_variables() {
  # AC 1: Given the six variables of Step 8 exported to a directory OUTER,
  # when a shell sources tests/lib.sh, then every name in SCRUB_LIST is unset afterwards.

  local outer="$TMP_ROOT/outer-lib"
  mkdir -p "$outer/state" "$outer/data" "$outer/config" "$outer/projects"

  local repo_root="$ROOT"

  # Source lib.sh in an environment with all SCRUB_LIST variables set to a dummy value
  # and verify they are unset after sourcing. Use a temp script to avoid quoting hell.
  local checkscript="$outer/check.sh"
  cat > "$checkscript" << 'CHECK_EOF'
#!/bin/bash
. "$1/tests/lib.sh" 2>/dev/null
for name in FM_HOME FM_BACKEND FM_ROOT_OVERRIDE FM_STATE_OVERRIDE FM_DATA_OVERRIDE FM_PROJECTS_OVERRIDE FM_CONFIG_OVERRIDE FM_CREW_STATE_META_OVERRIDE FM_CREW_STATE_STATUS_OVERRIDE FM_PENDING_REPLY_DIR_OVERRIDE FM_PROC_ROOT_OVERRIDE FM_TIMEOUT_MECHANISM_OVERRIDE; do
  val="${!name-UNSET}"
  printf '%s=%s\n' "$name" "$val"
done
CHECK_EOF
  chmod +x "$checkscript"

  local output
  output=$(
    export FM_HOME="$outer"
    export FM_BACKEND="tmux"
    export FM_ROOT_OVERRIDE="$repo_root"
    export FM_STATE_OVERRIDE="$outer/state"
    export FM_DATA_OVERRIDE="$outer/data"
    export FM_CONFIG_OVERRIDE="$outer/config"
    export FM_PROJECTS_OVERRIDE="$outer/projects"
    export FM_CREW_STATE_META_OVERRIDE="$outer/crew-meta"
    export FM_CREW_STATE_STATUS_OVERRIDE="$outer/crew-status"
    export FM_PENDING_REPLY_DIR_OVERRIDE="$outer/pending"
    export FM_PROC_ROOT_OVERRIDE="$outer/proc"
    export FM_TIMEOUT_MECHANISM_OVERRIDE="timeout"
    bash "$checkscript" "$repo_root"
  )

  # Check that each variable is reported as UNSET
  local failed=0
  for name in $SCRUB_LIST; do
    if ! echo "$output" | grep -q "^$name=UNSET$"; then
      printf 'not ok - Variable %s was not unset by tests/lib.sh\n' "$name" >&2
      failed=1
    fi
  done

  if [ "$failed" -eq 0 ]; then
    pass "Library unsets all inherited environment variables"
  else
    exit 1
  fi
}

test_library_clears_inherited_variables
