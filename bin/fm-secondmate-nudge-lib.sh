# shellcheck shell=bash disable=SC2034
# Durable secondmate reread-nudge marker helpers. Source only.
#
# Local tracked-file convergence publishes a bounded record before delivery. A failed send leaves the
# record for the locked bootstrap retry; a successful send removes it.

FM_SECOND_MATE_NUDGE_MESSAGE='firstmate was updated to the latest - please re-read your AGENTS.md to pick up the new instructions.'

fm_secondmate_nudge_marker_path() { # <state-dir> <id>
  local state=$1 id=$2
  case "$id" in *[!/A-Za-z0-9._-]*|''|*/*) return 1 ;; esac
  printf '%s/.secondmate-nudge-pending/%s.pending\n' "$state" "$id"
}

fm_secondmate_nudge_write() { # <state> <id> <home> <commit> <instructions> <message>
  local state=$1 id=$2 home=$3 commit=$4 instructions=$5 message=$6
  local marker parent tmp
  case "$home$commit$instructions$message" in *$'\n'*|*$'\r'*) return 1 ;; esac
  marker=$(fm_secondmate_nudge_marker_path "$state" "$id") || return 1
  parent=${marker%/*}
  if [ -e "$parent" ] || [ -L "$parent" ]; then
    [ -d "$parent" ] && [ ! -L "$parent" ] || return 1
  else
    mkdir -p "$parent" || return 1
  fi
  [ ! -L "$marker" ] || return 1
  tmp=$(umask 077; mktemp "$parent/.nudge.XXXXXX" 2>/dev/null) || return 1
  {
    printf 'id=%s\n' "$id"
    printf 'selector=fm-%s\n' "$id"
    printf 'home=%s\n' "$home"
    printf 'commit=%s\n' "$commit"
    printf 'instructions=%s\n' "$instructions"
    printf 'message=%s\n' "$message"
  } > "$tmp" || { rm -f -- "$tmp"; return 1; }
  chmod 600 "$tmp" || { rm -f -- "$tmp"; return 1; }
  mv -f -- "$tmp" "$marker" || { rm -f -- "$tmp"; return 1; }
}
