#!/usr/bin/env bash
# Resolve a task's two execution toggles, pstack and no-mistakes, each on|off.
# Usage: fm-resolve-toggles.sh [--brief-file <file>] <task-id> [<project-name>]
#
# Precedence per toggle, highest first: the task's brief, the project's registry
# entry, the machine default (both on, fixed, never prompted at setup).
# Prints exactly `pstack=<on|off>` then `no-mistakes=<on|off>` on stdout.
#
# Task level: a line at the start of the brief, `pstack: on|off` or
# `no-mistakes: on|off` (a trailing `# comment` is ignored). The first such line
# wins, and the scaffold writes the intake block first, so an override belongs in
# the intake block. The brief is data/<task-id>/brief.md unless --brief-file names
# another file, which bin/fm-brief.sh uses to resolve before the brief exists.
# Project level: `pstack=on|off` and `no-mistakes=on|off` tokens inside the
# registry entry's leading bracket annotation in data/projects.md, in any order
# beside the other tokens, e.g. `- name [pstack=off no-mistakes=on] - desc`.
# bin/fm-project-mode.sh ignores both tokens and docs/configuration.md owns the
# schema. The project is the second argument, else the brief's `project:` line;
# with neither, the project level is skipped.
#
# Values are case-insensitive and printed lowercase. Any other value, or an empty
# one, is refused at whichever level supplies the winning value. A missing or
# unreadable brief is refused too: the caller decides whether to defer the task
# or ask, because this script never guesses a safe default for a failure.
#
# Exit codes: 0 resolved, 1 brief missing or invalid value in the brief,
# 2 registry unreadable or invalid value in the registry, 64 wrong usage.
set -eu

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
FM_ROOT="${FM_ROOT_OVERRIDE:-$(cd "$SCRIPT_DIR/.." && pwd)}"
FM_HOME="${FM_HOME:-${FM_ROOT_OVERRIDE:-$FM_ROOT}}"
DATA="${FM_DATA_OVERRIDE:-$FM_HOME/data}"
REG="$DATA/projects.md"

USAGE="usage: fm-resolve-toggles.sh [--brief-file <file>] <task-id> [<project-name>]"
BRIEF_FILE=
if [ "${1:-}" = --brief-file ]; then
  [ "$#" -ge 2 ] || { echo "$USAGE" >&2; exit 64; }
  BRIEF_FILE=$2
  shift 2
fi
[ "$#" -ge 1 ] && [ "$#" -le 2 ] || { echo "$USAGE" >&2; exit 64; }
TASK_ID=$1
PROJECT=${2:-}
[ -n "$BRIEF_FILE" ] || BRIEF_FILE="$DATA/$TASK_ID/brief.md"

if [ ! -f "$BRIEF_FILE" ] || [ ! -r "$BRIEF_FILE" ]; then
  echo "error: brief for $TASK_ID not found or not readable at $BRIEF_FILE" >&2
  exit 1
fi

# brief_field <field>: the first `<field>:` line's value, comment and padding stripped.
brief_field() {
  awk -v f="$1" '
    index($0, f ":") == 1 {
      v = substr($0, length(f) + 2)
      sub(/#.*/, "", v)
      gsub(/^[ \t]+|[ \t]+$/, "", v)
      print v
      found = 1
      exit
    }
    END { if (!found) exit 3 }
  ' "$BRIEF_FILE"
}

# registry_token <project> <key>: `=<value>` for a `<key>=<value>` token in the
# project's leading bracket annotation (so an empty value is still reported).
# Prints nothing when the entry or token is absent. The name match is exact on the raw line, as bin/fm-project-mode.sh does.
registry_token() {
  awk -v n="$1" -v key="$2" '
    {
      prefix = "- " n
      if (substr($0, 1, length(prefix)) != prefix) next
      after = substr($0, length(prefix) + 1)
      if (substr(after, 1, 2) != " [") next
      s = after
      sub(/^ \[/, "", s)
      if (index(s, "]") == 0) next
      s = substr(s, 1, index(s, "]") - 1)
      k = split(s, a, " ")
      for (i = 1; i <= k; i++)
        if (index(a[i], key "=") == 1) { print "=" substr(a[i], length(key) + 2); exit }
      exit
    }
  ' "$REG"
}

if [ -z "$PROJECT" ]; then
  PROJECT=$(brief_field project || true)
fi
if [ -n "$PROJECT" ] && [ -e "$REG" ] && [ ! -r "$REG" ]; then
  echo "error: project registry $REG is not readable" >&2
  exit 2
fi

# resolve <toggle>: sets RESOLVED to the winning lowercase value.
resolve() {
  local toggle=$1 raw hit
  RESOLVED=on
  if raw=$(brief_field "$toggle"); then
    raw=$(printf '%s' "$raw" | tr '[:upper:]' '[:lower:]')
    case "$raw" in
      on|off) RESOLVED=$raw; return 0 ;;
      *) echo "error: invalid $toggle value '$raw' in $BRIEF_FILE; use on or off" >&2; exit 1 ;;
    esac
  fi
  [ -n "$PROJECT" ] && [ -f "$REG" ] || return 0
  hit=$(registry_token "$PROJECT" "$toggle")
  [ -n "$hit" ] || return 0
  raw=$(printf '%s' "${hit#=}" | tr '[:upper:]' '[:lower:]')
  case "$raw" in
    on|off) RESOLVED=$raw ;;
    *) echo "error: invalid $toggle value '$raw' for project $PROJECT in $REG; use on or off" >&2; exit 2 ;;
  esac
}

resolve pstack
PSTACK=$RESOLVED
resolve no-mistakes
NO_MISTAKES=$RESOLVED

printf 'pstack=%s\nno-mistakes=%s\n' "$PSTACK" "$NO_MISTAKES"
