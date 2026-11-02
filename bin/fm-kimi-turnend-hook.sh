#!/usr/bin/env bash
# Install or remove Firstmate's guarded Kimi crew turn-end hook.
#
# This command is the sole owner of the text-level edit to
# $HOME/.kimi-code/config.toml. It validates the existing TOML but never
# serializes it: install adds or replaces one marker-delimited Firstmate region,
# and remove excises only that region. Missing, malformed, symlinked, partially
# marked, or otherwise surprising config is refused without a config write.
#
# The installed Stop hook always exits 0 and stays silent. It reads cwd from the
# hook payload, checks for a .fm-kimi-turnend pointer before registry work, and
# touches a task turn-end marker only when the pointer names a Firstmate-created
# token in $HOME/.kimi-code/fm-turn-end.d/.
#
# Usage:
#   fm-kimi-turnend-hook.sh install
#   fm-kimi-turnend-hook.sh remove
set -u

case "${1:-}" in
  install|remove) ACTION=$1 ;;
  -h|--help)
    sed -n '2,18{s/^# \{0,1\}//;p;}' "$0"
    exit 0
    ;;
  *)
    printf 'usage: %s install|remove\n' "${0##*/}" >&2
    exit 2
    ;;
esac

if [ -z "${HOME:-}" ]; then
  printf 'fm-kimi-turnend-hook: refused: HOME is unset.\n' >&2
  exit 1
fi
kimi_mcao() {
  local bin=${FM_MCAO_BIN:-}
  [ -n "$bin" ] || bin=$(command -v mcao 2>/dev/null) || return 1
  [ -n "$bin" ] && [ -x "$bin" ] || return 1
  printf '%s\n' "$bin"
}

if ! MCAO=$(kimi_mcao); then
  printf 'fm-kimi-turnend-hook: refused: mcao is required to validate config.toml (set FM_MCAO_BIN or put mcao on PATH).\n' >&2
  exit 1
fi
if [ "$ACTION" = install ] && ! command -v jq >/dev/null 2>&1; then
  printf 'fm-kimi-turnend-hook: refused: jq is required by the installed Kimi turn-end hook.\n' >&2
  exit 1
fi

"$MCAO" util kimi-turnend "$ACTION" "$HOME/.kimi-code"
