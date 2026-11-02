# shellcheck shell=bash
# Ship-branch prefix owner: the single reader of config/branch-prefix.
# Usage: . bin/fm-branch-prefix-lib.sh
#
# fm_branch_prefix prints the configured prefix (first line, surrounding
# whitespace trimmed), or FM_DEFAULT_BRANCH_PREFIX when config/branch-prefix
# is absent, unreadable, or empty. The config directory follows the scripts'
# own rule: FM_CONFIG_OVERRIDE when set, otherwise the effective home's config/.
# The effective home is FM_HOME, FM_ROOT_OVERRIDE, FM_ROOT, or this code root,
# in that order.
#
# FM_LEGACY_BRANCH_PREFIX is the historical "fm/" prefix. Only a meta record
# that predates its branch= field falls back to it, because that record's
# branch was created under "fm/" whatever the config says now.

FM_DEFAULT_BRANCH_PREFIX='moa/'
# shellcheck disable=SC2034 # Read by the sourcing callers, not this lib.
FM_LEGACY_BRANCH_PREFIX='fm/'

fm_branch_prefix() {
  local file='' prefix='' home
  if [ -n "${FM_CONFIG_OVERRIDE:-}" ]; then
    file=$FM_CONFIG_OVERRIDE/branch-prefix
  else
    home=${FM_HOME:-${FM_ROOT_OVERRIDE:-${FM_ROOT:-}}}
    if [ -z "$home" ]; then
      home=$(CDPATH='' cd -- "$(dirname -- "${BASH_SOURCE[0]}")/.." && pwd) || return 1
    fi
    file=$home/config/branch-prefix
  fi
  if [ -n "$file" ] && [ -r "$file" ]; then
    prefix=$(head -n 1 "$file" 2>/dev/null | sed 's/^[[:space:]]*//; s/[[:space:]]*$//')
  fi
  printf '%s\n' "${prefix:-$FM_DEFAULT_BRANCH_PREFIX}"
}
