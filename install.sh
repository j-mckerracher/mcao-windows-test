#!/usr/bin/env bash
# install.sh - idempotent local install of the Motif multi-agent orchestrator on macOS, Linux and WSL2.
#
# Usage: install.sh [--force | --uninstall | --help]
#   (no flag)     check prerequisites, then link bin/mcao into ~/.local/bin/mcao
#   --force       install even though the clone has uncommitted changes to tracked files
#   --uninstall   remove ~/.local/bin/mcao when it points at this clone; keeps ~/.motif/multi-agent/
#
# The clone is MOA_INSTALL_DIR when set, otherwise the directory that holds this script.
# The only path this script writes is the ~/.local/bin/mcao link. It never installs a prerequisite,
# never edits shell rc files, harness configuration or the protected instruction files, and never
# reads or writes ~/.motif/multi-agent/, which the product owns at runtime. It never restarts the
# shared no-mistakes daemon. The hve plugin and its skills come from `mcao setup`, not from here.
#
# A missing Node.js or no-mistakes prints its upstream install line and the install continues.
# A missing tmux-or-herdr or harness CLI stops the install before anything is linked.
# Exit codes: 0 success, 1 fatal error, 2 invalid arguments.
#
# Environment: MOA_INSTALL_DIR selects the clone. MOA_INSTALL_ASSUME_TTY=1 treats stdin and stdout
# as a terminal (tests only).
#
# Bash 3.2 compatible: macOS ships /bin/bash 3.2, so do not use bash 4-only features here.
set -u

NODE_MIN_MAJOR=20
NODE_INSTALL_HINT='https://nodejs.org/en/download'
NO_MISTAKES_MIN=1.84.0
NO_MISTAKES_INSTALL_LINE='curl -fsSL https://raw.githubusercontent.com/kunchenguid/no-mistakes/main/docs/install.sh | sh'
BIN_DIR="$HOME/.local/bin"
LINK_NAME=mcao

MODE=install
FORCE=0
MISSING_REQUIRED=0
HARNESS_FOUND=0

usage() {
  cat <<'USAGE'
Usage: install.sh [--force | --uninstall | --help]
  (no flag)     check prerequisites, then link mcao into ~/.local/bin
  --force       install even when the clone has uncommitted changes to tracked files
  --uninstall   remove the ~/.local/bin/mcao link if it points at this clone
USAGE
}

fatal() {
  printf 'install.sh: %s\n' "$1" >&2
  exit 1
}

for arg in "$@"; do
  case $arg in
    --force) FORCE=1 ;;
    --uninstall) MODE=uninstall ;;
    -h | --help)
      usage
      exit 0
      ;;
    *)
      printf 'install.sh: unknown argument: %s\n' "$arg" >&2
      usage >&2
      exit 2
      ;;
  esac
done

SCRIPT_DIR=$(CDPATH='' cd -P -- "$(dirname "${BASH_SOURCE[0]}")" && pwd)
CLONE_SOURCE=${MOA_INSTALL_DIR:-$SCRIPT_DIR}
CLONE_DIR=$(CDPATH='' cd -P -- "$CLONE_SOURCE" 2>/dev/null && pwd) || fatal "clone directory does not exist: $CLONE_SOURCE"
ENTRY_TARGET="$CLONE_DIR/bin/$LINK_NAME"
LINK="$BIN_DIR/$LINK_NAME"

# version_at_least <have> <want>: both MAJOR.MINOR.PATCH.
version_at_least() {
  local ha hi hp wa wi wp
  IFS=. read -r ha hi hp <<<"$1"
  IFS=. read -r wa wi wp <<<"$2"
  if [ "$ha" -ne "$wa" ]; then
    [ "$ha" -gt "$wa" ]
    return
  fi
  if [ "$hi" -ne "$wi" ]; then
    [ "$hi" -gt "$wi" ]
    return
  fi
  [ "$hp" -ge "$wp" ]
}

check_node() {
  local version=none major
  if command -v node >/dev/null 2>&1; then
    version=$(node --version 2>/dev/null) || version=none
  fi
  major=${version#v}
  major=${major%%.*}
  if [[ $major =~ ^[0-9]+$ ]] && [ "$major" -ge "$NODE_MIN_MAJOR" ]; then
    printf 'ok: Node.js %s\n' "$version"
  else
    printf '⚠ Node.js >= %s not found (found: %s). Install via: %s\n' \
      "$NODE_MIN_MAJOR" "$version" "$NODE_INSTALL_HINT"
  fi
}

check_session_backend() {
  if command -v tmux >/dev/null 2>&1; then
    printf 'ok: tmux\n'
  elif command -v herdr >/dev/null 2>&1; then
    printf 'ok: herdr\n'
  else
    printf '✗ tmux or herdr is required. Install tmux (for example: brew install tmux) or herdr, then re-run.\n' >&2
    MISSING_REQUIRED=1
  fi
}

# probe_harness <label> <command>: each harness CLI is its own check; any one of them satisfies the install.
probe_harness() {
  if command -v "$2" >/dev/null 2>&1; then
    printf 'ok: %s (%s)\n' "$1" "$2"
    HARNESS_FOUND=$((HARNESS_FOUND + 1))
  else
    printf -- '- %s not found (%s)\n' "$1" "$2"
  fi
}

check_harnesses() {
  probe_harness "Claude Code" claude
  probe_harness "Codex" codex
  probe_harness "Copilot CLI" copilot
  if [ "$HARNESS_FOUND" -eq 0 ]; then
    printf '✗ No harness CLI found. Install Claude Code, Codex or Copilot CLI, then re-run.\n' >&2
    MISSING_REQUIRED=1
  fi
}

check_no_mistakes() {
  local out="" found="none"
  if command -v no-mistakes >/dev/null 2>&1; then
    out=$(no-mistakes --version 2>/dev/null) || out=""
  fi
  if [[ $out =~ v([0-9]+\.[0-9]+\.[0-9]+) ]]; then
    found=${BASH_REMATCH[1]}
  fi
  if [ "$found" != none ] && version_at_least "$found" "$NO_MISTAKES_MIN"; then
    printf 'ok: no-mistakes v%s\n' "$found"
  else
    printf '⚠ no-mistakes >= v%s not found (found: %s). Install via: %s\n' \
      "$NO_MISTAKES_MIN" "$found" "$NO_MISTAKES_INSTALL_LINE"
  fi
}

uninstall() {
  if [ -L "$LINK" ] && { [ "$(readlink "$LINK")" = "$ENTRY_TARGET" ] || [ "$LINK" -ef "$ENTRY_TARGET" ]; }; then
    rm -f -- "$LINK" || fatal "could not remove $LINK"
    printf 'Removed %s\n' "$LINK"
  elif [ -e "$LINK" ] || [ -L "$LINK" ]; then
    printf 'Left %s alone: it does not point at this clone (%s)\n' "$LINK" "$ENTRY_TARGET"
  else
    printf 'Nothing to remove: %s does not exist\n' "$LINK"
  fi
  printf 'Your fleet state in ~/.motif/multi-agent/ was not touched.\n'
  exit 0
}

if [ "$MODE" = uninstall ]; then
  uninstall
fi

# Refuse to install over local edits unless --force, so a half-edited clone is never silently updated.
if [ "$FORCE" -eq 0 ]; then
  command -v git >/dev/null 2>&1 || fatal "git is required to check the clone for local edits (or pass --force)"
  dirty=$(git -C "$CLONE_DIR" status --porcelain --untracked-files=no 2>/dev/null) ||
    fatal "$CLONE_DIR is not a git checkout, so local edits cannot be checked (or pass --force)"
  if [ -n "$dirty" ]; then
    printf 'Local edits detected in %s.\n' "$CLONE_DIR" >&2
    printf 'Stash or commit them, then re-run. Use --force to install anyway.\n' >&2
    exit 1
  fi
fi

[ -f "$ENTRY_TARGET" ] && [ -x "$ENTRY_TARGET" ] ||
  fatal "$ENTRY_TARGET is missing or not executable; $CLONE_DIR is not a complete clone"

check_node
check_session_backend
check_harnesses
check_no_mistakes

if [ "$MISSING_REQUIRED" -ne 0 ]; then
  fatal "required prerequisites are missing; nothing was linked"
fi

mkdir -p -- "$BIN_DIR" || fatal "could not create $BIN_DIR"
if [ -L "$LINK" ] && [ "$(readlink "$LINK")" = "$ENTRY_TARGET" ]; then
  printf 'ok: %s already links to %s\n' "$LINK" "$ENTRY_TARGET"
elif [ -e "$LINK" ] && [ ! -L "$LINK" ]; then
  fatal "$LINK exists and is not a symlink; move it aside and re-run"
else
  ln -sfn -- "$ENTRY_TARGET" "$LINK" || fatal "could not link $LINK"
  printf 'Linked %s -> %s\n' "$LINK" "$ENTRY_TARGET"
fi

case ":$PATH:" in
  *":$BIN_DIR:"*) ;;
  *)
    printf '⚠ %s is not on your PATH. Add it to your shell rc file yourself; this installer does not edit shell rc files.\n' "$BIN_DIR"
    ;;
esac

printf "Installed. Next: run \`%s setup\` to install the hve plugin and answer the setup questions.\n" "$LINK_NAME"

if [ "${MOA_INSTALL_ASSUME_TTY:-}" = 1 ] || { [ -t 0 ] && [ -t 1 ]; }; then
  printf 'Run setup now? [Y/n] '
  reply=
  IFS= read -r reply || reply=
  printf '\n'
  case $reply in
    '' | y | Y | yes | YES | Yes) exec "$ENTRY_TARGET" setup ;;
    *) printf "Skipped. Run \`%s setup\` when you are ready.\n" "$LINK_NAME" ;;
  esac
fi

exit 0
