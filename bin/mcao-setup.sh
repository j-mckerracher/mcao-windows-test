#!/usr/bin/env bash
# mcao-setup.sh - first-run and re-entrant setup for the local operating choices.
# Usage: mcao-setup.sh                         ask every question not yet answered (deferred ones included)
#        mcao-setup.sh <section>               re-ask one section: identity | harness | workers | backend | permissions | dispatch
#        mcao-setup.sh plugins                 install or refresh the hve plugin for each installed claude, copilot, codex
#        mcao-setup.sh --smoke-test            launch one throwaway worker, check it commits a file, tear it down; exits 1 on failure
#        mcao-setup.sh --check                 print setup diagnostics; never prompts, always exits 0
#        mcao-setup.sh --show [--format answers]
#                                             print the current answers as a table, or as an --answers file
#        mcao-setup.sh --answers <file>        load key=value answers from a file without prompting
#        mcao-setup.sh --remind                print the weekly deferred-choices reminder when it is due
#
# Reached as `mcao setup` through bin/mcao. docs/configuration.md "Setup" owns the user-facing contract.
#
# Every answer is written to its own config file first and recorded in config/setup-state second,
# each by temp file plus rename, so an interrupted run keeps every answer already given.
# config/setup-state: line 1 is `schema_version=1`, then `<key> answered|deferred` and `name=value` metadata.
# A key absent from the file is unanswered. The exact phrase "Prefer not to decide right now" records a
# deferral: the key's config file is removed, and the built-in default applies.
#
# Key -> file:
#   identity.name                        config/developer-name          (saved, not yet read by anything)
#   identity.branch-prefix               config/branch-prefix           (read by bin/fm-branch-prefix-lib.sh)
#   harness.orchestrator                 config/orchestrator-harness    (saved, not yet read by anything)
#   harness.worker-harness               config/crew-harness            (a bare adapter name, as bin/fm-harness.sh reads it)
#   harness.worker-model/-effort/-provider  the `default` profile of config/crew-dispatch.json
#   workers.max                          config/max-workers             (saved, not yet read by anything)
#   backend                              config/backend
#   permissions.claude-permission-mode   config/claude-permission-mode
#   dispatch.configured                  the rules in config/crew-dispatch.json whose `why` starts with "mao setup:"
# Rules in config/crew-dispatch.json that setup did not write are never touched.
#
# The hve plugin is not a question. A full run and `plugins` install it from the Mayo skills marketplace
# into every installed claude, copilot and codex, using each vendor's own plugin commands, then report
# whether each one can run the workflow agent. bin/fm-workflow-agent-lib.sh owns the plugin's names and
# that check; this file owns only the install steps. Codex has no named agents, so setup also writes
# the plugin agent's instructions into the codex profile, and leaves a profile it did not write alone.
# A failed install never discards answers; it prints what failed and how to retry.
#
# Environment: FM_HOME, FM_ROOT_OVERRIDE and FM_CONFIG_OVERRIDE select the config directory exactly as
# bin/fm-harness.sh does. MCAO_SETUP_ASSUME_TTY=1 treats stdin and stdout as a terminal (tests only).
# MCAO_SETUP_NOW overrides the clock used by --remind (tests only).
# MCAO_SETUP_HVE_SOURCE replaces the marketplace source, for example a local fixture directory (tests only).
set -u

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
FM_ROOT="${FM_ROOT_OVERRIDE:-$(cd "$SCRIPT_DIR/.." && pwd)}"
FM_HOME="${FM_HOME:-${FM_ROOT_OVERRIDE:-$FM_ROOT}}"
CONFIG="${FM_CONFIG_OVERRIDE:-$FM_HOME/config}"
STATE_FILE="$CONFIG/setup-state"
DISPATCH_FILE="$CONFIG/crew-dispatch.json"

SCHEMA_VERSION=1
DEFER_PHRASE="Prefer not to decide right now"
RULE_PREFIX="mao setup:"
REMIND_SECONDS=$((7 * 86400))
DEFAULT_MODEL="claude-haiku-5-5"
DEFAULT_EFFORT="medium"
DEFAULT_MAX_WORKERS=6
SINGLE_PROVIDER_HARNESSES="claude codex grok kimi cursor agy muse"
SEEDED_CLASSES="debugging implementation research planning review testing documentation maintenance"
ANSWER_KEYS="identity.name identity.branch-prefix harness.orchestrator harness.worker-harness harness.worker-model harness.worker-effort harness.worker-provider workers.max backend permissions.claude-permission-mode"
CODEX_PROFILE_MARK="# mao setup: generated from the hve@mayo-skills workflow agent; 'mao setup plugins' rewrites this file."

# shellcheck source=bin/fm-branch-prefix-lib.sh
. "$SCRIPT_DIR/fm-branch-prefix-lib.sh"
# shellcheck source=bin/fm-workflow-agent-lib.sh
. "$SCRIPT_DIR/fm-workflow-agent-lib.sh"
# shellcheck source=bin/fm-worker-account-lib.sh
. "$SCRIPT_DIR/fm-worker-account-lib.sh"
# shellcheck source=bin/fm-timeout-lib.sh
. "$SCRIPT_DIR/fm-timeout-lib.sh"
VERIFIED_HARNESSES=$(fm_workflow_agent_harnesses | tr '\n' ' ' | sed 's/ $//')

# ---- small helpers --------------------------------------------------------

err() { printf 'error: %s\n' "$*" >&2; }
die() { err "$*"; exit 1; }
have() { command -v "$1" >/dev/null 2>&1; }
lower() { printf '%s' "$1" | tr '[:upper:]' '[:lower:]'; }
trim() { printf '%s' "$1" | sed 's/^[[:space:]]*//;s/[[:space:]]*$//'; }
now() { printf '%s' "${MCAO_SETUP_NOW:-$(date +%s)}"; }
is_tty() { [ "${MCAO_SETUP_ASSUME_TTY:-0}" = 1 ] || { [ -t 0 ] && [ -t 1 ]; }; }

need_jq() {
  have jq || die "jq is required for setup (install: brew install jq, or apt install jq)"
}

class_description() {
  case $1 in
    debugging) printf 'Diagnosing and reproducing bugs and failures' ;;
    implementation) printf 'Writing or changing code to deliver a defined feature or fix' ;;
    research) printf 'Investigating questions by reading code and docs without changing them' ;;
    planning) printf 'Designing an approach and breaking work into steps' ;;
    review) printf 'Reviewing diffs, pull requests, or designs for correctness and risk' ;;
    testing) printf 'Writing, running, or fixing tests' ;;
    documentation) printf 'Writing or updating documentation and comments' ;;
    maintenance) printf 'Dependency bumps and cleanup with no behavior change' ;;
    *) printf '%s' "$1" ;;
  esac
}

harness_known() { case " $VERIFIED_HARNESSES " in *" $1 "*) return 0 ;; esac; return 1; }
harness_single_provider() { case " $SINGLE_PROVIDER_HARNESSES " in *" $1 "*) return 0 ;; esac; return 1; }
harness_takes_effort() { case $1 in opencode | kimi | cursor) return 1 ;; esac; return 0; }

# Mirrors the effort_ok rules in bin/fm-dispatch-resolve.sh.
effort_ok() { # <harness> <model> <effort>
  local h=$1 m=$2 e=$3
  if [ "$e" = ultra ]; then
    case $h in
      pi | pi-signed) case $m in codex-native/?*) return 0 ;; esac ;;
    esac
    return 1
  fi
  case $h in
    claude) case $e in low | medium | high | xhigh | max) return 0 ;; esac ;;
    codex)
      case $e in
        low | medium | high | xhigh) return 0 ;;
        max) [ "$m" = gpt-5.6-luna ] && return 0 ;;
      esac
      ;;
    grok | agy) case $e in low | medium | high) return 0 ;; esac ;;
    pi | pi-signed | omp | muse) case $e in low | medium | high | xhigh | max) return 0 ;; esac ;;
    rovo) case $e in low | medium | high | max) return 0 ;; esac ;;
    opencode | kimi | cursor) return 1 ;;
    *) case $e in low | medium | high | xhigh | max | ultra) return 0 ;; esac ;;
  esac
  return 1
}

effort_allowed_text() { # <harness>
  case $1 in
    claude) printf 'low, medium, high, xhigh, max' ;;
    codex) printf 'low, medium, high, xhigh (max only with model gpt-5.6-luna)' ;;
    grok | agy) printf 'low, medium, high' ;;
    pi | pi-signed) printf 'low, medium, high, xhigh, max (ultra only with a codex-native/<id> model)' ;;
    omp | muse) printf 'low, medium, high, xhigh, max' ;;
    rovo) printf 'low, medium, high, max' ;;
    opencode | kimi | cursor) printf 'none (this harness takes no effort)' ;;
    *) printf 'low, medium, high, xhigh, max, ultra' ;;
  esac
}

installed_orchestrators() {
  have claude && printf 'claude\n'
  have codex && printf 'codex\n'
  have omp && printf 'omp\n'
  return 0
}

# ---- files and state ------------------------------------------------------

atomic_write() { # <path> <content>
  local path=$1 content=$2 tmp
  mkdir -p "$(dirname "$path")" || return 1
  tmp=$(mktemp "$path.tmp.XXXXXX") || return 1
  if ! printf '%s\n' "$content" >"$tmp"; then rm -f "$tmp"; return 1; fi
  mv -f "$tmp" "$path" || { rm -f "$tmp"; return 1; }
}

cfg_read() { # <file under config/> -> first non-empty line, trimmed; empty when absent
  [ -f "$CONFIG/$1" ] || return 0
  grep -v '^[[:space:]]*$' "$CONFIG/$1" 2>/dev/null | head -n 1 | tr -d '\r' | sed 's/^[[:space:]]*//;s/[[:space:]]*$//'
}

state_status() { # <key> -> answered | deferred | (empty)
  [ -f "$STATE_FILE" ] || return 0
  awk -v k="$1" '$1 == k && ($2 == "answered" || $2 == "deferred") { print $2; exit }' "$STATE_FILE"
}

state_meta() { # <name> -> value
  [ -f "$STATE_FILE" ] || return 0
  sed -n "s/^$1=//p" "$STATE_FILE" | head -n 1
}

# Replace the line for <key> (a status line or a name=value line) in place, or append <newline> when absent.
state_rewrite() { # <key> <newline>
  local key=$1 newline=$2 body content
  if [ -f "$STATE_FILE" ]; then
    body=$(awk -v k="$key" -v n="$newline" '
      NR == 1 && /^schema_version=/ { next }
      $1 == k || index($0, k "=") == 1 { if (!done) { print n; done = 1 } next }
      { print }
      END { if (!done) print n }' "$STATE_FILE")
  else
    body=$newline
  fi
  content="schema_version=$SCHEMA_VERSION"$'\n'"$body"
  atomic_write "$STATE_FILE" "$content"
}

state_set() { state_rewrite "$1" "$1 $2"; } # <key> answered|deferred
state_set_meta() { state_rewrite "$1" "$1=$2"; } # <name> <value>

# ---- crew-dispatch.json ---------------------------------------------------

dispatch_valid() { [ -f "$DISPATCH_FILE" ] && jq -e 'type == "object"' "$DISPATCH_FILE" >/dev/null 2>&1; }

dispatch_default_field() { # <field>
  dispatch_valid || return 0
  jq -r --arg f "$1" '(.default // empty) | (if type == "array" then .[0] else . end) | .[$f] // empty' "$DISPATCH_FILE" 2>/dev/null
}

dispatch_default_is_foreign() {
  dispatch_valid || return 1
  jq -e '(.default | type) == "array" and (.default | length) > 1' "$DISPATCH_FILE" >/dev/null 2>&1
}

json_write() { # <path> <json>
  printf '%s' "$2" | jq -e 'type == "object"' >/dev/null 2>&1 || return 1
  atomic_write "$1" "$(printf '%s' "$2" | jq .)"
}

dispatch_base() {
  if [ -f "$DISPATCH_FILE" ]; then
    dispatch_valid || { err "config/crew-dispatch.json is not a JSON object; fix or remove it, setup will not overwrite it"; return 1; }
    cat "$DISPATCH_FILE"
  else
    printf '{}'
  fi
}

write_worker_default() { # <harness> <model> <effort> <provider>
  local base out
  base=$(dispatch_base) || return 1
  out=$(printf '%s' "$base" | jq --arg h "$1" --arg m "$2" --arg e "$3" --arg p "$4" '
    .default = ({harness: $h}
      + (if $m != "" then {model: $m} else {} end)
      + (if $e != "" then {effort: $e} else {} end)
      + (if $p != "" then {provider: $p} else {} end))') || return 1
  json_write "$DISPATCH_FILE" "$out"
}

setup_rules() { # -> JSON array of the rules this script wrote
  dispatch_valid || { printf '[]'; return 0; }
  jq -c --arg p "$RULE_PREFIX" '[(.rules // [])[] | select((.why // "") | startswith($p))]' "$DISPATCH_FILE" 2>/dev/null || printf '[]'
}

write_rules() { # <JSON array of setup-owned rules>
  local base out
  base=$(dispatch_base) || return 1
  out=$(printf '%s' "$base" | jq --argjson mine "$1" --arg p "$RULE_PREFIX" '
    .rules = ([(.rules // [])[] | select((.why // "") | startswith($p) | not)] + $mine)
    | if (.rules | length) == 0 then del(.rules) else . end') || return 1
  json_write "$DISPATCH_FILE" "$out"
}

# ---- prompting ------------------------------------------------------------

LINE=""
A_KIND=""
A_VALUE=""

input_ended() {
  printf '\n' >&2
  printf "Input ended. Answers given so far are saved; run 'mcao setup' to continue.\n" >&2
  exit 1
}

read_line() {
  LINE=""
  IFS= read -r LINE || input_ended
  LINE=$(printf '%s' "$LINE" | tr -d '\r')
}

# ask <label> <current> <default> [optional]
# Sets A_KIND (value | defer) and A_VALUE. Enter keeps the current value, or accepts the shown default.
# With <optional> = 1, typing `none` clears the value.
ask() {
  local label=$1 cur=$2 def=$3 optional=${4:-0} shown fallback
  if [ -n "$cur" ]; then
    shown="current: $cur"; fallback=$cur
  elif [ -n "$def" ]; then
    shown="default: $def"; fallback=$def
  else
    shown="not set"; fallback=""
  fi
  printf '%s\n  %s\n  [Enter to keep / type a new value%s / "%s"]: ' "$label" "$shown" \
    "$([ "$optional" = 1 ] && printf ' / none to clear')" "$DEFER_PHRASE"
  read_line
  if [ "$LINE" = "$DEFER_PHRASE" ]; then A_KIND=defer; A_VALUE=""; return 0; fi
  A_KIND=value
  if [ -z "$LINE" ]; then
    A_VALUE=$fallback
  elif [ "$optional" = 1 ] && [ "$(lower "$LINE")" = none ]; then
    A_VALUE=""
  else
    A_VALUE=$(trim "$LINE")
  fi
}

ask_yn() { # <prompt> -> 0 yes, 1 no, 2 defer
  printf '%s [y/N] ' "$1"
  read_line
  [ "$LINE" = "$DEFER_PHRASE" ] && return 2
  case $(lower "$LINE") in y | yes) return 0 ;; esac
  return 1
}

commit_value() { # <key> <config-file> <value>
  atomic_write "$CONFIG/$2" "$3" || die "could not write config/$2"
  state_set "$1" answered || die "could not record $1 in config/setup-state"
}

commit_defer() { # <key> <config-file-or-empty>
  [ -z "$2" ] || rm -f "$CONFIG/$2"
  state_set "$1" deferred || die "could not record $1 in config/setup-state"
}

# ---- questions ------------------------------------------------------------

q_name() {
  local v
  ask "What should I call you?" "$(cfg_read developer-name)" "Developer"
  if [ "$A_KIND" = defer ]; then commit_defer identity.name developer-name; return 0; fi
  v=$A_VALUE; [ -n "$v" ] || v=Developer
  commit_value identity.name developer-name "$v"
}

normalize_prefix() { # -> normalized prefix on stdout, or nonzero when unusable
  local p=$1
  case $p in /* | *//* | *..*) return 1 ;; esac
  printf '%s' "$p" | grep -Eq '^[A-Za-z0-9._/-]+$' || return 1
  case $p in */) ;; *) p="$p/" ;; esac
  printf '%s' "$p"
}

q_branch_prefix() {
  local v norm
  while :; do
    ask "Branch prefix" "$(cfg_read branch-prefix)" "$FM_DEFAULT_BRANCH_PREFIX"
    if [ "$A_KIND" = defer ]; then commit_defer identity.branch-prefix branch-prefix; return 0; fi
    v=$A_VALUE; [ -n "$v" ] || v=$FM_DEFAULT_BRANCH_PREFIX
    if norm=$(normalize_prefix "$v"); then
      commit_value identity.branch-prefix branch-prefix "$norm"
      return 0
    fi
    printf "'%s' is not a usable branch prefix. Use letters, digits, '.', '_', '-' and '/', for example moa/.\n" "$v"
  done
}

q_orchestrator() {
  local installed first v
  installed=$(installed_orchestrators)
  [ -n "$installed" ] || die "No supported orchestrator harness installed. Please install Claude Code, Codex, or Oh My Pi (omp)."
  first=$(printf '%s\n' "$installed" | head -n 1)
  while :; do
    ask "Orchestrator harness (Claude Code, Codex, or Oh My Pi). Easy change later via 'mcao setup harness'." \
      "$(cfg_read orchestrator-harness)" "$first"
    if [ "$A_KIND" = defer ]; then commit_defer harness.orchestrator orchestrator-harness; return 0; fi
    v=$(lower "$A_VALUE")
    [ "$v" != "claude code" ] || v=claude
    case $v in "oh my pi" | "oh-my-pi") v=omp ;; esac
    if printf '%s\n' "$installed" | grep -qx "$v"; then
      commit_value harness.orchestrator orchestrator-harness "$v"
      return 0
    fi
    printf "'%s' is not available. Installed: %s.\n" "$A_VALUE" "$(printf '%s' "$installed" | tr '\n' ' ')"
  done
}

defer_worker() {
  rm -f "$CONFIG/crew-harness"
  local k
  for k in harness.worker-harness harness.worker-model harness.worker-effort; do
    state_set "$k" deferred || die "could not record $k in config/setup-state"
  done
}

# Effort for a harness: returns the accepted value, falling back to medium with a warning.
resolve_effort() { # <harness> <model> <effort>
  local h=$1 m=$2 e=$3
  [ -n "$e" ] || return 0
  if effort_ok "$h" "$m" "$e"; then printf '%s' "$e"; return 0; fi
  printf "Warning: effort '%s' is not accepted by %s (accepted: %s); using %s.\n" \
    "$e" "$h" "$(effort_allowed_text "$h")" "$DEFAULT_EFFORT" >&2
  printf '%s' "$DEFAULT_EFFORT"
}

q_worker() {
  local default_h cur_h h provider="" model="" effort="" cur_model cur_effort cur_provider
  if dispatch_default_is_foreign; then
    printf "config/crew-dispatch.json has a hand-written default list; leaving the worker default to you.\n"
    return 0
  fi
  if have claude; then default_h=claude; else default_h=$(installed_orchestrators | head -n 1); fi
  [ -n "$default_h" ] || default_h=claude
  cur_h=$(cfg_read crew-harness)
  while :; do
    ask "Default worker harness. [Note: the latest Claude Haiku on xhigh works for clear, well-defined work.]" "$cur_h" "$default_h"
    [ "$A_KIND" != defer ] || { defer_worker; return 0; }
    h=$(lower "$A_VALUE")
    harness_known "$h" && break
    printf "'%s' is not a verified harness. Verified: %s.\n" "$A_VALUE" "$VERIFIED_HARNESSES"
  done
  have "$h" || printf "Note: '%s' was not found on PATH.\n" "$h"
  cur_model=""; cur_effort=""; cur_provider=""
  if [ "$h" = "$cur_h" ]; then
    cur_model=$(dispatch_default_field model); cur_effort=$(dispatch_default_field effort); cur_provider=$(dispatch_default_field provider)
  fi
  if ! harness_single_provider "$h"; then
    while :; do
      ask "Provider for $h (it serves more than one provider, for example anthropic)" "$cur_provider" ""
      [ "$A_KIND" != defer ] || { defer_worker; return 0; }
      provider=$(lower "$A_VALUE")
      printf '%s' "$provider" | grep -Eq '^[a-z0-9]+(-[a-z0-9]+)*$' && break
      printf "A provider id is lowercase letters, digits and single dashes, for example anthropic.\n"
    done
  fi
  local model_default=""
  [ "$h" != claude ] || model_default=$DEFAULT_MODEL
  ask "Default model (none = the harness's own default)" "$cur_model" "$model_default" 1
  [ "$A_KIND" != defer ] || { defer_worker; return 0; }
  model=$A_VALUE
  if harness_takes_effort "$h"; then
    ask "Default effort (accepted: $(effort_allowed_text "$h"))" "$cur_effort" "$DEFAULT_EFFORT" 1
    [ "$A_KIND" != defer ] || { defer_worker; return 0; }
    effort=$(resolve_effort "$h" "$model" "$A_VALUE")
  fi
  commit_worker "$h" "$model" "$effort" "$provider"
}

commit_worker() { # <harness> <model> <effort> <provider>
  local k
  atomic_write "$CONFIG/crew-harness" "$1" || die "could not write config/crew-harness"
  write_worker_default "$1" "$2" "$3" "$4" || die "could not write config/crew-dispatch.json"
  for k in harness.worker-harness harness.worker-model harness.worker-effort; do
    state_set "$k" answered || die "could not record $k in config/setup-state"
  done
}

# ---- worker limit ---------------------------------------------------------

# max_workers_valid <value>: 0 for a positive whole number of at most six digits, the same shape
# bin/fm-project-capacity-lib.sh accepts for a capacity. Leading zeros are allowed and dropped on save.
max_workers_valid() {
  case $1 in '' | *[!0-9]*) return 1 ;; esac
  [ "${#1}" -le 6 ] || return 1
  [ $((10#$1)) -ge 1 ]
}

q_max_workers() {
  local v
  while :; do
    ask "Maximum worker agents running at once (recommended: $DEFAULT_MAX_WORKERS; a whole number of at least 1)" \
      "$(cfg_read max-workers)" "$DEFAULT_MAX_WORKERS"
    if [ "$A_KIND" = defer ]; then commit_defer workers.max max-workers; return 0; fi
    v=$A_VALUE
    if max_workers_valid "$v"; then commit_value workers.max max-workers "$((10#$v))"; return 0; fi
    printf "'%s' is not a worker limit. Enter a whole number of at least 1, up to six digits; the recommendation is %s.\n" \
      "$v" "$DEFAULT_MAX_WORKERS"
  done
}

q_backend() {
  local default_b=tmux v
  if have tmux; then default_b=tmux; elif have herdr; then default_b=herdr; else
    printf "Note: neither tmux nor herdr was found on PATH.\n"
  fi
  while :; do
    ask "Session backend (tmux or herdr)" "$(cfg_read backend)" "$default_b"
    if [ "$A_KIND" = defer ]; then commit_defer backend backend; return 0; fi
    v=$(lower "$A_VALUE")
    case $v in tmux | herdr) commit_value backend backend "$v"; return 0 ;; esac
    printf "'%s' is not a backend setup knows. Allowed: tmux, herdr.\n" "$A_VALUE"
  done
}

q_permissions() {
  local v
  if have codex; then
    printf "Codex permissions: not yet configurable (workers launch with --dangerously-bypass-approvals-and-sandbox).\n"
  fi
  if ! have claude; then
    printf "Claude Code was not found; its permission posture is not asked.\n"
    return 0
  fi
  while :; do
    printf "Claude Code worker permissions:\n"
    printf "  bypass - workers run with --dangerously-skip-permissions: no approval prompts, fastest, no guard rails.\n"
    printf "  auto   - workers run with --permission-mode auto: the model decides which actions need approval.\n"
    ask "Permission posture for Claude Code workers" "$(cfg_read claude-permission-mode)" "bypass"
    if [ "$A_KIND" = defer ]; then commit_defer permissions.claude-permission-mode claude-permission-mode; return 0; fi
    v=$(lower "$A_VALUE")
    case $v in bypass | auto) commit_value permissions.claude-permission-mode claude-permission-mode "$v"; return 0 ;; esac
    printf "'%s' is not an accepted posture. Allowed: bypass, auto.\n" "$A_VALUE"
  done
}

# ---- dispatch (question 7) -----------------------------------------------

rule_field() { # <rule-json> <field>
  printf '%s' "$1" | jq -r --arg f "$2" '.use | (if type == "array" then .[0] else . end) | .[$f] // empty'
}

# ask_class <class> <description> <existing-rule-json-or-empty>
# Prints the rule JSON on stdout when the class needs one; prints nothing when it stays on the default.
# Prompts go to stderr so command substitution captures only the rule.
ask_class() {
  local class=$1 desc=$2 existing=$3 dh dm de dp ch cm ce cp h m e p same
  dh=$(cfg_read crew-harness); dm=$(dispatch_default_field model); de=$(dispatch_default_field effort); dp=$(dispatch_default_field provider)
  ch=$dh; cm=$dm; ce=$de; cp=$dp
  if [ -n "$existing" ]; then
    ch=$(rule_field "$existing" harness); cm=$(rule_field "$existing" model); ce=$(rule_field "$existing" effort); cp=$(rule_field "$existing" provider)
  fi
  {
    printf -- '--- Configuring: %s ---\n  %s\n  Enter keeps the shown value; type skip to leave this class on the default.\n' "$class" "$desc"
  } >&2
  while :; do
    ask "Harness for $class work" "$ch" "" >&2
    [ "$A_KIND" != defer ] || return 0
    [ "$(lower "$A_VALUE")" != skip ] || return 0
    h=$(lower "$A_VALUE")
    harness_known "$h" && break
    printf "'%s' is not a verified harness. Verified: %s.\n" "$A_VALUE" "$VERIFIED_HARNESSES" >&2
  done
  if [ "$h" != "$ch" ]; then cm=""; ce=""; cp=""; fi
  p=""
  if ! harness_single_provider "$h"; then
    while :; do
      ask "Provider for $class work ($h serves more than one provider)" "$cp" "" >&2
      [ "$A_KIND" != defer ] || return 0
      [ "$(lower "$A_VALUE")" != skip ] || return 0
      p=$(lower "$A_VALUE")
      printf '%s' "$p" | grep -Eq '^[a-z0-9]+(-[a-z0-9]+)*$' && break
      printf "A provider id is lowercase letters, digits and single dashes.\n" >&2
    done
  fi
  ask "Model for $class work (none = the harness's own default)" "$cm" "" 1 >&2
  [ "$A_KIND" != defer ] || return 0
  [ "$(lower "$A_VALUE")" != skip ] || return 0
  m=$A_VALUE
  e=""
  if harness_takes_effort "$h"; then
    ask "Effort for $class work (accepted: $(effort_allowed_text "$h"))" "$ce" "" 1 >&2
    [ "$A_KIND" != defer ] || return 0
    [ "$(lower "$A_VALUE")" != skip ] || return 0
    e=$A_VALUE
    [ -z "$e" ] || e=$(resolve_effort "$h" "$m" "$e")
  fi
  same=0
  [ "$h" = "$dh" ] && [ "$m" = "$dm" ] && [ "$e" = "$de" ] && [ "$p" = "$dp" ] && same=1
  [ "$same" = 0 ] || return 0
  jq -nc --arg w "$desc" --arg why "$RULE_PREFIX $class" --arg h "$h" --arg m "$m" --arg e "$e" --arg p "$p" '
    {when: $w, why: $why, use: ({harness: $h}
      + (if $m != "" then {model: $m} else {} end)
      + (if $e != "" then {effort: $e} else {} end)
      + (if $p != "" then {provider: $p} else {} end))}'
}

existing_rule_for() { # <class>
  dispatch_valid || return 0
  jq -c --arg w "$RULE_PREFIX $1" '[(.rules // [])[] | select(.why == $w)] | .[0] // empty' "$DISPATCH_FILE" 2>/dev/null
}

summarize_rules() {
  printf '%s' "$(setup_rules)" | jq -r '.[] | "  \(.why | sub("^mao setup: "; "")): \(.use | (if type == "array" then .[0] else . end) | [.harness, .model, .effort] | map(select(. != null)) | join(" "))"'
}

q_dispatch() { # <full | section>
  local mode=$1 rc mine='[]' rule class desc
  need_jq
  if [ -z "$(cfg_read crew-harness)" ] || [ "$(state_status harness.worker-harness)" != answered ]; then
    printf "Answer the worker question first ('mcao setup harness'); dispatch rules start from its default.\n"
    return 0
  fi
  if [ "$(state_status dispatch.configured)" = answered ] && [ "$mode" = section ]; then
    printf "Current dispatch rules written by setup:\n"
    summarize_rules
    ask_yn "Modify dispatch rules?" && rc=0 || rc=$?
    [ "$rc" = 0 ] || return 0
  else
    ask_yn "Would you like to set custom harness, provider, model, effort combinations for certain kinds of work?" && rc=0 || rc=$?
    if [ "$rc" = 2 ]; then state_set dispatch.configured deferred || die "could not record dispatch.configured"; return 0; fi
    if [ "$rc" != 0 ]; then
      state_set dispatch.configured answered || die "could not record dispatch.configured"
      return 0
    fi
  fi
  for class in $SEEDED_CLASSES; do
    rule=$(ask_class "$class" "$(class_description "$class")" "$(existing_rule_for "$class")")
    [ -z "$rule" ] || mine=$(printf '%s' "$mine" | jq -c --argjson r "$rule" '. + [$r]')
  done
  # Custom classes written earlier are offered again, then new ones.
  local prior_custom
  prior_custom=$(setup_rules | jq -c --arg seeded "$SEEDED_CLASSES" '[.[] | select((.why | sub("^mao setup: "; "")) as $c | ($seeded | split(" ") | index($c)) == null)] | .[]')
  while IFS= read -r rule; do
    [ -n "$rule" ] || continue
    class=$(printf '%s' "$rule" | jq -r '.why | sub("^mao setup: "; "")')
    desc=$(printf '%s' "$rule" | jq -r '.when')
    rule=$(ask_class "$class" "$desc" "$rule")
    [ -z "$rule" ] || mine=$(printf '%s' "$mine" | jq -c --argjson r "$rule" '. + [$r]')
  done <<EOF
$prior_custom
EOF
  while :; do
    ask_yn "Add another class?" && rc=0 || rc=$?
    [ "$rc" = 0 ] || break
    printf 'Class name (lowercase letters, digits, dashes; blank to cancel): '
    read_line
    class=$(lower "$(trim "$LINE")")
    [ -n "$class" ] || continue
    if ! printf '%s' "$class" | grep -Eq '^[a-z0-9][a-z0-9-]*$'; then
      printf "A class name is lowercase letters, digits and dashes.\n"; continue
    fi
    printf 'One-line description (matched in natural language by the orchestrator): '
    read_line
    desc=$(trim "$LINE")
    [ -n "$desc" ] || { printf "A description is required.\n"; continue; }
    rule=$(ask_class "$class" "$desc" "")
    [ -z "$rule" ] || mine=$(printf '%s' "$mine" | jq -c --argjson r "$rule" '. + [$r]')
  done
  write_rules "$mine" || die "could not write config/crew-dispatch.json"
  state_set dispatch.configured answered || die "could not record dispatch.configured"
  printf 'Dispatch rules written: %s.\n' "$(printf '%s' "$mine" | jq 'length')"
}

# ---- hve plugin -----------------------------------------------------------

hve_source() { printf '%s' "${MCAO_SETUP_HVE_SOURCE:-$FM_WORKFLOW_AGENT_MARKETPLACE_SOURCE}"; }

# A local marketplace directory is loaded live or cannot be refreshed, so the refresh steps skip it.
hve_source_is_local() { [ -d "$(hve_source)" ]; }

# step <description> <command...>: runs quietly and prints the vendor's last lines when it fails.
step() {
  local what=$1 out
  shift
  out=$("$@" 2>&1) && return 0
  printf '  %s failed: %s\n' "$what" "$(printf '%s' "$out" | tail -n 3 | tr '\n' ' ' | sed 's/[[:space:]]*$//')"
  return 1
}

# The Claude store the workers read: ambient, a pinned root, or the vendor default (config/claude-account).
# Sets globals, so call it directly, never inside $(...); a refusal goes to CLAUDE_STORE_ERROR.
CLAUDE_STORE_MODE=ambient
CLAUDE_STORE_ROOT=""
CLAUDE_STORE_ERROR=""
resolve_claude_store() {
  local out rest
  CLAUDE_STORE_MODE=ambient
  CLAUDE_STORE_ROOT=""
  CLAUDE_STORE_ERROR=""
  [ -e "$CONFIG/claude-account" ] || return 0
  if ! out=$(fm_worker_account_resolve claude "$CONFIG" 2>&1); then
    CLAUDE_STORE_ERROR=${out#error: }
    return 1
  fi
  [ -n "$out" ] || return 0
  rest=${out#*$'\t'}
  CLAUDE_STORE_ROOT=${rest%%$'\t'*}
  if [ -n "$CLAUDE_STORE_ROOT" ]; then CLAUDE_STORE_MODE=root; else CLAUDE_STORE_MODE=default; fi
}

claude_store() { # <command...>: runs under the store resolve_claude_store picked
  case $CLAUDE_STORE_MODE in
    root) CLAUDE_CONFIG_DIR=$CLAUDE_STORE_ROOT "$@" ;;
    default) env -u CLAUDE_CONFIG_DIR "$@" ;;
    *) "$@" ;;
  esac
}

# hve_require <harness>: the shared check, run against the store that harness's workers read.
hve_require() {
  case $1 in
    claude)
      if [ "$CLAUDE_STORE_MODE" = default ]; then
        (unset CLAUDE_CONFIG_DIR; fm_workflow_agent_require claude "")
      else
        fm_workflow_agent_require claude "$CLAUDE_STORE_ROOT"
      fi
      ;;
    *) fm_workflow_agent_require "$1" ;;
  esac
}

install_hve_claude() {
  local mkt=$FM_WORKFLOW_AGENT_MARKETPLACE plugin=$FM_WORKFLOW_AGENT_PLUGIN
  step "adding the $mkt marketplace" claude_store claude plugin marketplace add "$(hve_source)" || return 1
  if ! hve_source_is_local; then
    step "refreshing the $mkt marketplace" claude_store claude plugin marketplace update "$mkt" || return 1
  fi
  step "installing $plugin" claude_store claude plugin install "$plugin" || return 1
  hve_source_is_local || step "updating $plugin" claude_store claude plugin update "$plugin"
}

install_hve_copilot() {
  local mkt=$FM_WORKFLOW_AGENT_MARKETPLACE plugin=$FM_WORKFLOW_AGENT_PLUGIN
  # copilot refuses to add a marketplace twice, so a known one is refreshed instead.
  if ! copilot plugin marketplace list --json 2>/dev/null | jq -e --arg m "$mkt" 'any(.[]; .name == $m)' >/dev/null 2>&1; then
    step "adding the $mkt marketplace" copilot plugin marketplace add "$(hve_source)" || return 1
  elif ! hve_source_is_local; then
    step "refreshing the $mkt marketplace" copilot plugin marketplace update "$mkt" || return 1
  fi
  step "installing $plugin" copilot plugin install "$plugin" || return 1
  hve_source_is_local || step "updating $plugin" copilot plugin update "$plugin"
}

install_hve_codex() {
  local mkt=$FM_WORKFLOW_AGENT_MARKETPLACE plugin=$FM_WORKFLOW_AGENT_PLUGIN
  step "adding the $mkt marketplace" codex plugin marketplace add "$(hve_source)" || return 1
  if ! hve_source_is_local; then
    step "refreshing the $mkt marketplace" codex plugin marketplace upgrade "$mkt" || return 1
  fi
  step "installing $plugin" codex plugin add "$plugin" || return 1
  write_codex_profile
}

# Codex has no named agents, so the plugin agent's instructions become the profile -p workflow-agent layers.
write_codex_profile() {
  local profile root agent body
  profile=$(fm_workflow_agent_path codex)
  if [ -f "$profile" ] && ! grep -qxF "$CODEX_PROFILE_MARK" "$profile"; then
    printf '  %s was not written by mcao setup, so it was left as it is.\n' "$profile"
    return 0
  fi
  root=$(codex plugin list --json 2>/dev/null | jq -r --arg p "$FM_WORKFLOW_AGENT_PLUGIN" \
    '[.installed[]? | select(.pluginId == $p and .enabled) | .source.path // empty][0] // empty' 2>/dev/null)
  agent="$root/agents/$FM_WORKFLOW_AGENT_NAME.md"
  if [ -z "$root" ] || [ ! -f "$agent" ]; then
    printf '  writing the codex profile failed: codex does not list an enabled %s with %s\n' \
      "$FM_WORKFLOW_AGENT_PLUGIN" "agents/$FM_WORKFLOW_AGENT_NAME.md"
    return 1
  fi
  # The profile carries the agent body without its YAML front matter.
  body=$(awk 'NR == 1 && /^---[[:space:]]*$/ { fm = 1; next } fm && /^---[[:space:]]*$/ { fm = 0; next } !fm' "$agent")
  atomic_write "$profile" "$CODEX_PROFILE_MARK"$'\n'"developer_instructions = $(printf '%s' "$body" | jq -Rs .)" || {
    printf '  writing the codex profile failed: could not write %s\n' "$profile"
    return 1
  }
}

# pi_worker_root: the Pi root the workers read: the config/pi-account pin, else empty (environment or default).
pi_worker_root() {
  local out rest
  [ -e "$CONFIG/pi-account" ] || return 0
  out=$(fm_worker_account_resolve pi "$CONFIG" 2>/dev/null) || return 1
  [ -n "$out" ] || return 0
  rest=${out#*$'\t'}
  printf '%s' "${rest%%$'\t'*}"
}

# hve_plugin_root: where the enabled hve plugin is installed for claude or codex, the source Pi and omp copy from.
hve_plugin_root() {
  local root=""
  if have claude && have jq && resolve_claude_store; then
    root=$(claude_store claude plugin list --json 2>/dev/null | jq -r --arg p "$FM_WORKFLOW_AGENT_PLUGIN" \
      '[.[]? | select(.id == $p and .enabled) | .installPath // empty][0] // empty' 2>/dev/null)
  fi
  if [ -z "$root" ] && have codex && have jq; then
    root=$(codex plugin list --json 2>/dev/null | jq -r --arg p "$FM_WORKFLOW_AGENT_PLUGIN" \
      '[.installed[]? | select(.pluginId == $p and .enabled) | .source.path // empty][0] // empty' 2>/dev/null)
  fi
  [ -n "$root" ] && [ -f "$root/agents/$FM_WORKFLOW_AGENT_NAME.md" ] && [ -d "$root/skills" ] || return 1
  printf '%s' "$root"
}

# place_workflow_agent <harness>: Pi and omp cannot install the plugin, so copy its agent (front matter stripped)
# and skills into the mao-owned folder the launch reads, then swap the folder in whole so a rerun is idempotent.
place_workflow_agent() {
  local h=$1 plugin dir pi_root="" tmp
  if ! plugin=$(hve_plugin_root); then
    printf '  copying the workflow agent failed: no enabled %s plugin found for claude or codex; run its install first\n' "$FM_WORKFLOW_AGENT_PLUGIN"
    return 1
  fi
  case $h in pi | pi-signed) pi_root=$(pi_worker_root) || { printf '  copying the workflow agent failed: config/pi-account is invalid\n'; return 1; } ;; esac
  dir=$(fm_workflow_agent_dir "$h" "$pi_root")
  mkdir -p "$(dirname "$dir")"
  tmp=$(mktemp -d "$dir.tmp.XXXXXX") || { printf '  copying the workflow agent failed: could not write under %s\n' "$(dirname "$dir")"; return 1; }
  if ! { awk 'NR == 1 && /^---[[:space:]]*$/ { fm = 1; next } fm && /^---[[:space:]]*$/ { fm = 0; next } !fm' \
      "$plugin/agents/$FM_WORKFLOW_AGENT_NAME.md" >"$tmp/$FM_WORKFLOW_AGENT_NAME.md" \
    && cp -R "$plugin/skills" "$tmp/skills" \
    && { [ "$h" != omp ] || printf 'skills:\n  customDirectories:\n    - %s/skills\n' "$dir" >"$tmp/skills.yml"; } \
    && rm -rf "$dir" && mv "$tmp" "$dir"; }; then
    rm -rf "$tmp"
    printf '  copying the workflow agent failed: could not write %s\n' "$dir"
    return 1
  fi
}

# install_plugins: installs or refreshes the hve plugin into every installed claude, copilot and codex,
# then copies the agent and skills for pi and omp, and reports each through the shared check. Returns 1 when any of them cannot run the agent.
install_plugins() {
  local h err failed=0 any=0 root
  need_jq
  printf '\nInstalling the %s plugin, which provides the workflow agent every worker runs as:\n' "$FM_WORKFLOW_AGENT_PLUGIN"
  for h in $(fm_workflow_agent_plugin_harnesses); do
    have "$h" || continue
    any=1
    if [ "$h" = claude ] && ! resolve_claude_store; then
      printf '  claude: skipped, the worker account pin is invalid: %s\n' "$CLAUDE_STORE_ERROR"
      failed=1
      continue
    fi
    "install_hve_$h" || true
    if err=$(hve_require "$h" 2>&1 >/dev/null); then
      printf '  %s: ready\n' "$h"
    else
      printf '  %s: not ready - %s\n' "$h" "${err#error: }"
      failed=1
    fi
  done
  [ "$any" = 1 ] || printf '  None of claude, copilot or codex is installed, so there is nothing to install it into.\n'
  for h in $(fm_workflow_agent_copy_harnesses); do
    have "$h" || continue
    place_workflow_agent "$h" || true
    if [ "$h" = pi ] || [ "$h" = pi-signed ]; then root=$(pi_worker_root) || root=""; else root=""; fi
    if err=$(fm_workflow_agent_require "$h" "$root" 2>&1 >/dev/null); then
      printf '  %s: ready\n' "$h"
    else
      printf '  %s: not ready - %s\n' "$h" "${err#error: }"
      failed=1
    fi
  done
  if [ "$failed" = 1 ]; then
    printf "The marketplace repository is private. If a step could not sign in, run 'gh auth login' and 'gh auth setup-git'.\n"
    printf "Fix what is shown above, then run 'mcao setup plugins'.\n"
  fi
  [ "$any" = 1 ] && [ "$failed" = 0 ]
}

do_plugins() {
  install_plugins
  exit $?
}

# ---- flows ----------------------------------------------------------------

required_keys() {
  printf '%s\n' identity.name identity.branch-prefix harness.orchestrator harness.worker-harness \
    harness.worker-model harness.worker-effort backend
  ! have claude || printf '%s\n' permissions.claude-permission-mode
  return 0
}

unanswered_required() {
  local k
  while IFS= read -r k; do
    [ -n "$(state_status "$k")" ] || printf '%s\n' "$k"
  done <<EOF
$(required_keys)
EOF
}

not_answered() { [ "$(state_status "$1")" != answered ]; }

# ---- smoke test -----------------------------------------------------------

# One throwaway worker, launched through bin/fm-brief.sh and bin/fm-spawn.sh exactly as the product launches a
# ship, in a scratch firstmate home and a scratch git project under $TMPDIR. The worker creates test.txt and
# commits it on its own branch (delivery mode direct-PR: the retained ship mode that needs no no-mistakes pipeline,
# since local-only is refused for ship launches under docs/managed-context.md). The harness and backend are the ones
# setup saved, so a spawn that cannot launch them is the failure this test exists to find.
# Everything the test makes lives under one scratch directory; smoke_cleanup tears the worker down through
# bin/fm-teardown.sh, then removes that directory, and runs on every exit path (pass, fail, deadline, signal).
# The worker gets SMOKE_SECONDS (two minutes) to do its task once the spawn has returned, and the spawn itself
# gets SMOKE_SPAWN_SECONDS, because a spawn alone takes tens of seconds on a busy host. Both bounds use
# fm_exec_timed or a deadline, so no `timeout` binary is needed.
SMOKE_TASK_TEXT="Create a file named test.txt whose content is exactly: smoke test"
SMOKE_SPEC_TEXT="Run: printf 'smoke test\\n' > test.txt && git add test.txt && git commit -m 'smoke test'. Nothing else is needed."

SMOKE_GRACE=15
SMOKE_SECONDS="${MCAO_SETUP_SMOKE_SECONDS:-120}"
SMOKE_SPAWN_SECONDS="${MCAO_SETUP_SMOKE_SPAWN_SECONDS:-120}"

# TERM at the bound and KILL only after SMOKE_GRACE more seconds, so a spawn cut off mid-launch still runs its
# own abort cleanup and closes the worker pane it opened (fm_run_timed would KILL after a fraction of a second).
smoke_fm() { # <command...> bounded by $SMOKE_LEFT seconds, run against the scratch home
  ( fm_exec_timed "$SMOKE_LEFT" "$SMOKE_GRACE" env FM_HOME="$SMOKE_HOME" FM_ROOT_OVERRIDE= FM_STATE_OVERRIDE= \
    FM_DATA_OVERRIDE= FM_PROJECTS_OVERRIDE= FM_CONFIG_OVERRIDE= FM_SPAWN_NO_GUARD=1 "$@" )
}

smoke_verify() { # <worktree> -> 0 when the worker's commit and file are there; sets SMOKE_REASON otherwise
  local wt=$1
  SMOKE_REASON=""
  [ -f "$wt/test.txt" ] || { SMOKE_REASON="test.txt does not exist in the worker's worktree"; return 1; }
  [ "$(cat "$wt/test.txt")" = "smoke test" ] || { SMOKE_REASON="test.txt does not contain 'smoke test'"; return 1; }
  [ "$(git -C "$wt" log -1 --format=%s 2>/dev/null)" = "smoke test" ] || { SMOKE_REASON="the worker's last commit is not 'smoke test'"; return 1; }
  [ -z "$(git -C "$wt" status --porcelain 2>/dev/null)" ] || { SMOKE_REASON="the worker left uncommitted changes"; return 1; }
  return 0
}

# A spawn that dies after opening its pane and before recording the task (an error that says "inspect window",
# or the bound cutting it off) leaves a pane no task record names, so teardown cannot find it. The tmux pane is
# always `firstmate:fm-<id>`; other backends name theirs by an id only the spawn knew, so those are reported.
smoke_close_orphan_pane() {
  local backend
  backend=$(cfg_read backend)
  case ${backend:-tmux} in
    tmux)
      have tmux || return 0
      if tmux list-windows -t firstmate -F '#{window_name}' 2>/dev/null | grep -qx "fm-$SMOKE_ID"; then
        tmux kill-window -t "=firstmate:=fm-$SMOKE_ID" 2>/dev/null ||
          printf 'warning: could not close the smoke-test pane fm-%s in tmux session firstmate; close it by hand.\n' "$SMOKE_ID"
      fi
      ;;
    *)
      [ "${SMOKE_SPAWN_FAILED:-0}" = 1 ] &&
        printf 'warning: the smoke-test spawn failed before it recorded the worker; if a pane named fm-%s is open on %s, close it by hand.\n' "$SMOKE_ID" "$backend"
      ;;
  esac
  return 0
}

smoke_cleanup() {
  if [ -n "$SMOKE_ID" ] && [ -e "$SMOKE_HOME/state/$SMOKE_ID.meta" ]; then
    SMOKE_LEFT=60
    smoke_fm "$SCRIPT_DIR/fm-teardown.sh" "$SMOKE_ID" --force >"$SMOKE_DIR/teardown.log" 2>&1 \
      || { printf 'warning: could not tear down the smoke-test worker %s; its endpoint may still be running:\n' "$SMOKE_ID"; tail -n 5 "$SMOKE_DIR/teardown.log"; }
  fi
  [ -z "$SMOKE_ID" ] || smoke_close_orphan_pane
  [ -z "$SMOKE_DIR" ] || rm -rf "$SMOKE_DIR"
}

smoke_fail() { # <reason>
  printf 'Smoke test failed: %s\n' "$1"
  return 1
}

smoke_body() { # runs in a subshell so its traps and variables never reach the setup flow
  local proj wt meta deadline log rc f brief body harness backend
  SMOKE_DIR="" SMOKE_ID="" SMOKE_HOME="" SMOKE_REASON="" SMOKE_SPAWN_FAILED=0
  trap smoke_cleanup EXIT
  trap 'exit 130' INT
  trap 'exit 143' TERM
  have git || { smoke_fail "git is not on PATH"; return; }
  SMOKE_DIR=$(mktemp -d "${TMPDIR:-/tmp}/mcao-smoke-test.XXXXXX") || { smoke_fail "could not create a scratch directory under ${TMPDIR:-/tmp}"; return; }
  SMOKE_ID="smoke-$$"
  SMOKE_HOME="$SMOKE_DIR/home"
  proj=$SMOKE_HOME/projects/smoke
  SMOKE_LEFT=$SMOKE_SPAWN_SECONDS

  mkdir -p "$SMOKE_HOME/data" "$SMOKE_HOME/state" "$SMOKE_HOME/config" "$proj" || { smoke_fail "could not create the scratch home"; return; }
  # The harness, backend and permission posture setup saved; nothing else is carried over.
  for f in crew-harness backend crew-dispatch.json claude-permission-mode; do
    [ ! -f "$CONFIG/$f" ] || cp "$CONFIG/$f" "$SMOKE_HOME/config/$f"
  done
  printf '%s\n' "- smoke [direct-PR] throwaway project for 'mcao setup' (added $(date +%Y-%m-%d))" >"$SMOKE_HOME/data/projects.md"
  # The pool lives inside the scratch project, so removing the directory removes every worktree.
  {
    git -C "$proj" init -q &&
      printf 'root = "."\n' >"$proj/treehouse.toml" &&
      printf '.treehouse/\n' >>"$proj/.git/info/exclude" &&
      git -C "$proj" add treehouse.toml &&
      git -C "$proj" -c user.name=mcao-setup -c user.email=mcao-setup@example.invalid commit -q -m "scratch project"
  } >"$SMOKE_DIR/git.log" 2>&1 || { smoke_fail "could not create the scratch git project: $(tail -n 3 "$SMOKE_DIR/git.log")"; return; }

  # The brief placeholders are the orchestrator's to fill; the smoke test fills them with the one trivial task.
  smoke_fm "$SCRIPT_DIR/fm-brief.sh" "$SMOKE_ID" smoke --mode direct-PR >"$SMOKE_DIR/brief.log" 2>&1 \
    || { smoke_fail "could not scaffold the worker brief: $(tail -n 3 "$SMOKE_DIR/brief.log")"; return; }
  brief=$SMOKE_HOME/data/$SMOKE_ID/brief.md
  body=$(cat "$brief") || { smoke_fail "could not read the worker brief"; return; }
  # Quoted replacements: bash 5.2+ expands a bare & in an unquoted pattern-substitution replacement to the matched text,
  # so the spec's && operators became {FIRSTMATE_SPEC} under Homebrew bash 5.3 (the shebang's default on macOS).
  body=${body//\{TASK\}/"$SMOKE_TASK_TEXT"}
  body=${body//\{FIRSTMATE_SPEC\}/"$SMOKE_SPEC_TEXT"}
  printf '%s\n' "$body" >"$brief" || { smoke_fail "could not fill the worker brief"; return; }

  local args=("$SMOKE_ID" "$proj" --mode direct-PR --yolo off)
  harness=$(cfg_read crew-harness)
  [ -z "$harness" ] || args+=(--harness "$harness")
  backend=$(cfg_read backend)
  [ -z "$backend" ] || args+=(--backend "$backend")

  printf 'Starting the smoke test: one throwaway worker (%s on %s), %s seconds for its task.\n' "${harness:-default harness}" "${backend:-default backend}" "$SMOKE_SECONDS"
  log=$SMOKE_DIR/spawn.log
  smoke_fm "$SCRIPT_DIR/fm-spawn.sh" "${args[@]}" >"$log" 2>&1
  rc=$?
  if [ "$rc" -ne 0 ]; then
    SMOKE_SPAWN_FAILED=1
    if fm_timed_out "$rc"; then
      printf 'Smoke test timed out after %s seconds while launching the worker.\n' "$SMOKE_SPAWN_SECONDS"
    else
      printf 'Smoke test failed: the worker did not launch. Last lines of the spawn output:\n'
    fi
    tail -n 20 "$log"
    return 1
  fi

  meta=$SMOKE_HOME/state/$SMOKE_ID.meta
  wt=$(sed -n 's/^worktree=//p' "$meta" 2>/dev/null | head -n 1)
  [ -n "$wt" ] || { smoke_fail "the spawn recorded no worktree for the worker"; return; }
  deadline=$(($(date +%s) + SMOKE_SECONDS))
  while ! smoke_verify "$wt"; do
    if [ "$(date +%s)" -ge "$deadline" ]; then
      printf 'Smoke test timed out after %s seconds: %s. Last lines of the worker pane:\n' "$SMOKE_SECONDS" "$SMOKE_REASON"
      SMOKE_LEFT=10
      smoke_fm "$SCRIPT_DIR/fm-peek.sh" "$SMOKE_ID" 20 2>/dev/null || printf '(the worker pane could not be read)\n'
      return 1
    fi
    sleep 1
  done
  printf 'Smoke test passed: the worker launched, created test.txt and committed it.\n'
}

run_smoke_test() { # 0 passed, 1 failed or timed out; always leaves nothing behind
  ( smoke_body )
}

prompt_smoke_test() {
  local resp=""
  printf '\nRun 2-minute smoke test? It launches one throwaway worker with the harness and backend above. [y/N] '
  IFS= read -r resp || resp=""
  printf '\n'
  case $(lower "$(trim "$resp")") in
    y | yes) run_smoke_test || printf 'Setup continues; run '"'"'mcao setup --smoke-test'"'"' to retry.\n' ;;
  esac
  return 0
}

closing_screen() {
  cat <<'EOF'

Setup is saved. Changes apply on the next worker launch; nothing needs a restart.

The hve plugin keeps the workflow honest: each worker runs as its workflow agent, which picks a playbook for the task.
no-mistakes runs an AI review, test and lint pipeline on the work before it reaches a pull request.
Both are on by default. Leaving them on is recommended for robust, verified work.
You can turn them off per-project or per-task if needed.

You can re-run `mcao setup` any time, or `mcao setup <section>` to change one part.
`mcao setup plugins` installs or refreshes the hve plugin on its own.
Add projects through the orchestrator (normal workflow); setup doesn't configure them.

To start, run your harness of choice in this repository:
`claude`, `codex`, or `omp`

Have fun!
EOF
}

on_interrupt() {
  printf "\nInterrupted. Answers given so far are saved; run 'mcao setup' to continue.\n" >&2
  exit 130
}

do_setup() { # <section or empty>
  local section=$1
  need_jq
  if ! is_tty; then
    local missing
    missing=$(unanswered_required)
    if [ -n "$missing" ]; then
      printf "Error: Unanswered setup questions remain. Run 'mcao setup' interactively, or 'mcao setup --answers <file>'\n" >&2
      printf '  unanswered: %s\n' "$(printf '%s' "$missing" | tr '\n' ' ')" >&2
      exit 1
    fi
    printf 'Setup is already complete.\n'
    exit 0
  fi
  mkdir -p "$CONFIG" || die "could not create $CONFIG"
  trap on_interrupt INT TERM
  case $section in
    "")
      not_answered identity.name && q_name
      not_answered identity.branch-prefix && q_branch_prefix
      not_answered harness.orchestrator && q_orchestrator
      if not_answered harness.worker-harness || not_answered harness.worker-model || not_answered harness.worker-effort; then q_worker; fi
      not_answered workers.max && q_max_workers
      not_answered backend && q_backend
      if have claude && not_answered permissions.claude-permission-mode; then q_permissions; fi
      not_answered dispatch.configured && q_dispatch full
      install_plugins || true
      prompt_smoke_test
      closing_screen
      ;;
    identity) q_name; q_branch_prefix; printf "Identity saved.\n" ;;
    harness) q_orchestrator; q_worker; printf "Harness choices saved.\n" ;;
    workers) q_max_workers; printf "Worker limit saved.\n" ;;
    backend) q_backend; printf "Backend saved.\n" ;;
    permissions) q_permissions; printf "Permissions saved.\n" ;;
    dispatch) q_dispatch section; printf "Dispatch saved.\n" ;;
  esac
}

# ---- --check --------------------------------------------------------------

do_check() {
  local problems=0 out missing t h err root
  out=$(FM_BOOTSTRAP_DETECT_ONLY=1 FM_BOOTSTRAP_NETWORK=skip "$SCRIPT_DIR/fm-bootstrap.sh" 2>&1 || true)
  if [ -n "$out" ]; then printf '%s\n' "$out"; problems=1; fi
  if [ -z "$(installed_orchestrators)" ]; then
    printf 'MISSING: no supported orchestrator harness on PATH (install Claude Code, Codex, or Oh My Pi (omp))\n'; problems=1
  fi
  if ! have tmux && ! have herdr; then
    printf 'MISSING: neither tmux nor herdr on PATH\n'; problems=1
  fi
  have jq || { printf 'MISSING: jq (setup needs it to write config/crew-dispatch.json)\n'; problems=1; }
  for h in $(fm_workflow_agent_plugin_harnesses); do
    have "$h" || continue
    if [ "$h" = claude ] && ! resolve_claude_store; then
      printf 'NOTE: claude worker account pin is invalid: %s\n' "$CLAUDE_STORE_ERROR"; problems=1; continue
    fi
    err=$(hve_require "$h" 2>&1 >/dev/null) || { printf 'MISSING: %s workflow agent: %s\n' "$h" "${err#error: }"; problems=1; }
  done
  for h in $(fm_workflow_agent_copy_harnesses); do
    have "$h" || continue
    root=""
    case $h in pi | pi-signed) root=$(pi_worker_root) || root="" ;; esac
    err=$(fm_workflow_agent_require "$h" "$root" 2>&1 >/dev/null) || { printf 'MISSING: %s workflow agent: %s\n' "$h" "${err#error: }"; problems=1; }
  done
  t=$(lower "${NO_MISTAKES_TELEMETRY:-}")
  case $t in
    0 | false | off) ;;
    *)
      printf 'NOTE: NO_MISTAKES_TELEMETRY is not off in this shell. Workers launched by firstmate always set it; for no-mistakes run by hand: export NO_MISTAKES_TELEMETRY=0\n'
      problems=1
      ;;
  esac
  missing=$(unanswered_required)
  if [ -n "$missing" ]; then
    printf "NOTE: %s setup question(s) unanswered (%s): run 'mcao setup'\n" \
      "$(printf '%s\n' "$missing" | grep -c .)" "$(printf '%s' "$missing" | tr '\n' ' ' | sed 's/ $//')"
    problems=1
  fi
  [ "$problems" = 1 ] || printf 'setup check: all good\n'
  exit 0
}

# ---- --show ---------------------------------------------------------------

key_file_value() { # <key> -> current stored value
  case $1 in
    identity.name) cfg_read developer-name ;;
    identity.branch-prefix) cfg_read branch-prefix ;;
    harness.orchestrator) cfg_read orchestrator-harness ;;
    harness.worker-harness) cfg_read crew-harness ;;
    harness.worker-model) dispatch_default_field model ;;
    harness.worker-effort) dispatch_default_field effort ;;
    harness.worker-provider) dispatch_default_field provider ;;
    workers.max) cfg_read max-workers ;;
    backend) cfg_read backend ;;
    permissions.claude-permission-mode) cfg_read claude-permission-mode ;;
  esac
}

key_status() { # <key> -> answered | deferred | (empty); the provider is answered whenever it is set
  if [ "$1" = harness.worker-provider ]; then
    [ -z "$(key_file_value "$1")" ] || printf 'answered'
    return 0
  fi
  state_status "$1"
}

key_default() { # <key> -> what applies while the key is deferred or unanswered
  case $1 in
    identity.name) printf 'Developer' ;;
    identity.branch-prefix) printf '%s' "$FM_DEFAULT_BRANCH_PREFIX" ;;
    harness.orchestrator) printf 'the harness running it' ;;
    harness.worker-harness) printf "the orchestrator's own harness" ;;
    harness.worker-model | harness.worker-effort | harness.worker-provider) printf "the harness's own default" ;;
    workers.max) printf '%s' "$DEFAULT_MAX_WORKERS" ;;
    backend) if have tmux; then printf 'tmux'; elif have herdr; then printf 'herdr'; else printf 'tmux'; fi ;;
    permissions.claude-permission-mode) printf 'bypass' ;;
  esac
}

key_not_applied() { # keys saved but not yet read by any script
  case $1 in identity.name | harness.orchestrator | workers.max) return 0 ;; esac
  return 1
}

show_keys() {
  local k
  for k in $ANSWER_KEYS; do
    case $k in
      harness.worker-provider) [ -n "$(key_status "$k")" ] || continue ;;
      permissions.claude-permission-mode) have claude || [ -n "$(key_status "$k")" ] || continue ;;
    esac
    printf '%s\n' "$k"
  done
}

do_show() {
  local format=table k status value source rules
  need_jq
  while [ $# -gt 0 ]; do
    case $1 in
      --format) [ $# -ge 2 ] || { err "--format needs a value (table or answers)"; exit 2; }; format=$2; shift 2 ;;
      *) err "unknown option for --show: $1"; exit 2 ;;
    esac
  done
  case $format in table | answers) ;; *) err "unknown format: $format (table or answers)"; exit 2 ;; esac
  if [ "$format" = answers ]; then
    for k in $(show_keys); do
      status=$(key_status "$k")
      case $status in
        answered) printf '%s=%s\n' "$k" "$(key_file_value "$k")" ;;
        deferred) printf '%s=deferred\n' "$k" ;;
      esac
    done
    exit 0
  fi
  printf '%-38s %-30s %s\n' key value source
  printf '%-38s %-30s %s\n' "--------------------------------------" "------------------------------" "------------------------------"
  for k in $(show_keys); do
    status=$(key_status "$k")
    case $status in
      answered)
        value=$(key_file_value "$k"); [ -n "$value" ] || value="(harness default)"
        source=answered
        ;;
      deferred) value="(not set)"; source="deferred -> default $(key_default "$k")" ;;
      *) value="(not set)"; source="unanswered (default $(key_default "$k"))" ;;
    esac
    if key_not_applied "$k" && [ "$status" = answered ]; then source="$source (saved; not yet applied)"; fi
    printf '%-38s %-30s %s\n' "$k" "$value" "$source"
  done
  status=$(state_status dispatch.configured)
  rules=$(setup_rules | jq 'length')
  case $status in
    answered) value="$rules custom rule(s)"; source=answered ;;
    deferred) value="(not set)"; source="deferred -> default no custom rules" ;;
    *) value="(not set)"; source="unanswered (default no custom rules)" ;;
  esac
  printf '%-38s %-30s %s\n' dispatch.configured "$value" "$source"
  if have codex; then
    printf '%-38s %-30s %s\n' permissions.codex-permission-mode "(not configurable)" "workers launch with full bypass"
  fi
  exit 0
}

# ---- --answers ------------------------------------------------------------

AN_KEYS=()
AN_VALS=()

an_lookup() { # <key> -> sets AN_FOUND and AN_VALUE (the last occurrence wins)
  local i
  AN_FOUND=0; AN_VALUE=""
  for ((i = 0; i < ${#AN_KEYS[@]}; i++)); do
    if [ "${AN_KEYS[$i]}" = "$1" ]; then AN_FOUND=1; AN_VALUE=${AN_VALS[$i]}; fi
  done
}

# validate_answer <key> <value> [harness-for-worker-checks] -> 0 ok; else sets ALLOWED
ALLOWED=""
validate_answer() {
  local key=$1 v=$2 norm
  ALLOWED=""
  case $key in
    identity.name) return 0 ;;
    identity.branch-prefix)
      [ -n "$v" ] || return 0
      normalize_prefix "$v" >/dev/null && return 0
      ALLOWED="letters, digits, '.', '_', '-' and '/', for example moa/"; return 1 ;;
    harness.orchestrator)
      norm=$(lower "$v")
      case $norm in claude | codex | omp) have "$norm" && return 0; ALLOWED="claude, codex, omp (installed: $(installed_orchestrators | tr '\n' ' ' | sed 's/ $//'))"; return 1 ;; esac
      ALLOWED="claude, codex, omp"; return 1 ;;
    harness.worker-harness)
      harness_known "$(lower "$v")" && return 0
      ALLOWED="$VERIFIED_HARNESSES"; return 1 ;;
    harness.worker-model) return 0 ;;
    harness.worker-effort) return 0 ;; # checked against the harness when applied
    harness.worker-provider)
      [ -z "$v" ] && return 0
      printf '%s' "$v" | grep -Eq '^[a-z0-9]+(-[a-z0-9]+)*$' && return 0
      ALLOWED="lowercase letters, digits and single dashes"; return 1 ;;
    workers.max)
      max_workers_valid "$v" && return 0
      ALLOWED="a whole number of at least 1, up to six digits (recommended $DEFAULT_MAX_WORKERS)"; return 1 ;;
    backend)
      case $(lower "$v") in tmux | herdr) return 0 ;; esac
      ALLOWED="tmux, herdr"; return 1 ;;
    permissions.claude-permission-mode)
      case $(lower "$v") in bypass | auto) return 0 ;; esac
      ALLOWED="bypass, auto"; return 1 ;;
  esac
  return 1
}

do_answers() {
  local file=${1:-} line key value loaded=0 errors=0 i k norm
  need_jq
  [ -n "$file" ] || { err "--answers needs a file"; exit 2; }
  [ -e "$file" ] || { printf 'File not found: %s\n' "$file" >&2; exit 1; }
  [ -r "$file" ] && [ -f "$file" ] || { printf 'Cannot read file: %s\n' "$file" >&2; exit 1; }
  while IFS= read -r line || [ -n "$line" ]; do
    line=$(printf '%s' "$line" | tr -d '\r')
    case $(trim "$line") in "" | "#"*) continue ;; esac
    case $line in *=*) ;; *) printf 'Not a key=value line: %s\n' "$line"; errors=$((errors + 1)); continue ;; esac
    key=$(trim "${line%%=*}"); value=$(trim "${line#*=}")
    case " $ANSWER_KEYS " in *" $key "*) ;; *) printf 'Unknown key: %s\n' "$key"; errors=$((errors + 1)); continue ;; esac
    if [ "$value" != deferred ] && ! validate_answer "$key" "$value"; then
      printf '%s invalid value: %s. Allowed: %s.\n' "$key" "$value" "$ALLOWED"
      errors=$((errors + 1)); continue
    fi
    AN_KEYS+=("$key"); AN_VALS+=("$value")
  done <"$file"
  mkdir -p "$CONFIG" || die "could not create $CONFIG"

  for k in identity.name identity.branch-prefix harness.orchestrator workers.max backend permissions.claude-permission-mode; do
    an_lookup "$k"; [ "$AN_FOUND" = 1 ] || continue
    case $k in
      identity.name) apply_simple "$k" developer-name "$AN_VALUE" Developer ;;
      identity.branch-prefix)
        norm=$AN_VALUE
        if [ "$norm" != deferred ]; then norm=$(normalize_prefix "${norm:-$FM_DEFAULT_BRANCH_PREFIX}"); fi
        apply_simple "$k" branch-prefix "$norm" ""
        ;;
      harness.orchestrator) apply_simple "$k" orchestrator-harness "$(lower "$AN_VALUE")" "" ;;
      workers.max)
        if [ "$AN_VALUE" = deferred ]; then commit_defer "$k" max-workers; else commit_value "$k" max-workers "$((10#$AN_VALUE))"; fi
        ;;
      backend) apply_simple "$k" backend "$(lower "$AN_VALUE")" "" ;;
      permissions.claude-permission-mode) apply_simple "$k" claude-permission-mode "$(lower "$AN_VALUE")" "" ;;
    esac
    loaded=$((loaded + 1))
  done
  apply_worker_answers
  loaded=$((loaded + WORKER_LOADED))
  errors=$((errors + WORKER_ERRORS))
  printf '%s answers loaded, %s validation errors.\n' "$loaded" "$errors"
  exit 0
}

apply_simple() { # <key> <file> <value> <blank-default>
  local key=$1 file=$2 value=$3 blank=$4
  if [ "$value" = deferred ]; then commit_defer "$key" "$file"; return 0; fi
  [ -n "$value" ] || value=$blank
  commit_value "$key" "$file" "$value"
}

WORKER_LOADED=0
WORKER_ERRORS=0
apply_worker_answers() {
  local hf hv mf mv ef ev pf pv h m e p cur_h
  WORKER_LOADED=0; WORKER_ERRORS=0
  an_lookup harness.worker-harness; hf=$AN_FOUND; hv=$AN_VALUE
  an_lookup harness.worker-model; mf=$AN_FOUND; mv=$AN_VALUE
  an_lookup harness.worker-effort; ef=$AN_FOUND; ev=$AN_VALUE
  an_lookup harness.worker-provider; pf=$AN_FOUND; pv=$AN_VALUE
  [ $((hf + mf + ef + pf)) -gt 0 ] || return 0
  if dispatch_default_is_foreign; then
    printf 'config/crew-dispatch.json has a hand-written default list; the worker answers were not applied.\n'
    WORKER_ERRORS=$((hf + mf + ef + pf)); return 0
  fi
  if [ "$hf" = 1 ] && [ "$hv" = deferred ]; then
    defer_worker; WORKER_LOADED=$((mf + ef + pf + 1)); return 0
  fi
  cur_h=$(cfg_read crew-harness)
  if [ "$hf" = 1 ]; then h=$(lower "$hv"); else h=$cur_h; fi
  if [ -z "$h" ]; then
    printf 'harness.worker-model, -effort and -provider need harness.worker-harness to be answered first.\n'
    WORKER_ERRORS=$((mf + ef + pf)); return 0
  fi
  m=""; e=""; p=""
  if [ "$h" = "$cur_h" ]; then m=$(dispatch_default_field model); e=$(dispatch_default_field effort); p=$(dispatch_default_field provider); fi
  [ "$mf" != 1 ] || { if [ "$mv" = deferred ]; then m=""; else m=$mv; fi; }
  [ "$pf" != 1 ] || { if [ "$pv" = deferred ]; then p=""; else p=$pv; fi; }
  if [ "$ef" = 1 ]; then
    if [ "$ev" = deferred ]; then
      e=""
    elif [ -z "$ev" ] || effort_ok "$h" "$m" "$ev"; then
      e=$ev
    else
      printf 'harness.worker-effort invalid value: %s. Allowed: %s.\n' "$ev" "$(effort_allowed_text "$h")"
      WORKER_ERRORS=$((WORKER_ERRORS + 1)); ef=0
    fi
  fi
  harness_takes_effort "$h" || e=""
  atomic_write "$CONFIG/crew-harness" "$h" || die "could not write config/crew-harness"
  write_worker_default "$h" "$m" "$e" "$p" || die "could not write config/crew-dispatch.json"
  state_set harness.worker-harness answered || die "could not record harness.worker-harness"
  WORKER_LOADED=$((WORKER_LOADED + hf))
  worker_state harness.worker-model "$mf" "$mv"
  worker_state harness.worker-effort "$ef" "$ev"
  [ "$pf" != 1 ] || WORKER_LOADED=$((WORKER_LOADED + 1))
}

worker_state() { # <key> <found> <value>
  [ "$2" = 1 ] || return 0
  if [ "$3" = deferred ]; then state_set "$1" deferred; else state_set "$1" answered; fi
  WORKER_LOADED=$((WORKER_LOADED + 1))
}

# ---- --remind -------------------------------------------------------------

do_remind() {
  local deferred count last at keys
  [ -f "$STATE_FILE" ] || exit 0
  deferred=$(awk '$2 == "deferred" { print $1 }' "$STATE_FILE")
  [ -n "$deferred" ] || exit 0
  last=$(state_meta deferred_reminded_at)
  case $last in '' | *[!0-9]*) last=0 ;; esac
  at=$(now)
  [ $((at - last)) -ge "$REMIND_SECONDS" ] || exit 0
  count=$(printf '%s\n' "$deferred" | grep -c .)
  keys=$(printf '%s' "$deferred" | tr '\n' ',' | sed 's/,$//; s/,/, /g')
  printf "%s setup choices deferred (%s): run 'mcao setup' to finalize.\n" "$count" "$keys"
  state_set_meta deferred_reminded_at "$at" 2>/dev/null || true
  exit 0
}

# ---- entry ----------------------------------------------------------------

usage() {
  cat <<'EOF'
Usage: mcao setup                       ask every setup question not yet answered
       mcao setup <section>             re-ask one section: identity | harness | workers | backend | permissions | dispatch
       mcao setup plugins               install or refresh the hve plugin the workers run as
       mcao setup --smoke-test          launch one throwaway worker and check it commits a file (up to 2 minutes)
       mcao setup --check               print setup diagnostics (never prompts, always exits 0)
       mcao setup --show [--format answers]
       mcao setup --answers <file>      load key=value answers without prompting
EOF
}

case "${1:-}" in
  -h | --help | help) usage; exit 0 ;;
  --check) shift; do_check ;;
  --show) shift; do_show "$@" ;;
  --answers) shift; do_answers "$@" ;;
  --remind) do_remind ;;
  plugins) do_plugins ;;
  --smoke-test) run_smoke_test; exit $? ;;
  "") do_setup "" ;;
  identity | harness | workers | backend | permissions | dispatch) do_setup "$1" ;;
  *) err "unknown argument: $1"; usage >&2; exit 2 ;;
esac
