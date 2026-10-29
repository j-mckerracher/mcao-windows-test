#!/usr/bin/env bash
# tests/mcao-setup-hve-live-e2e.test.sh - default-on live guard proving that
# `mcao setup plugins` still installs the hve plugin through every INSTALLED
# claude, copilot and codex, and that each vendor's own lookup agrees with the
# record bin/fm-workflow-agent-lib.sh checks before a spawn.
#
# Why this file exists: the install commands, the settings record the spawn
# check reads, and the `hve:workflow-agent` agent name are all vendor surfaces.
# A stubbed CLI can only confirm what the stub already assumes, so this guard
# runs the real CLIs against a local fixture marketplace in throwaway stores.
# It submits no prompt and spends no model tokens:
#   - claude and copilot are asked for an agent that does not exist; both
#     refuse before any model call and list the agents they can resolve, which
#     must include hve:workflow-agent.
#   - codex has no named agents; `codex debug prompt-input` renders the prompt
#     locally and must carry the profile setup wrote only when -p selects it.
# The portable counterpart in tests/mcao-setup.test.sh pins setup's own logic.
# Run this guard after any claude, copilot or codex upgrade.
set -u

# shellcheck source=tests/lib.sh
. "$(dirname "${BASH_SOURCE[0]}")/lib.sh"

fm_live_gate default-on FM_MCAO_SETUP_HVE_LIVE jq

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
LAB=$(mktemp -d "${TMPDIR:-/tmp}/mcao-setup-hve-live.XXXXXX")
cleanup_all() { [ -n "${LAB:-}" ] && rm -rf "$LAB"; }
trap cleanup_all EXIT
note() { printf '# %s\n' "$1"; }

# A fixture marketplace with the same names as the real one, so the install
# needs no network or credentials and the agent body is known.
MKT="$LAB/marketplace"
mkdir -p "$MKT/.claude-plugin" "$MKT/hve/.claude-plugin" "$MKT/hve/agents"
printf '%s\n' '{"name":"mayo-skills","owner":{"name":"fixture"},"plugins":[{"name":"hve","source":"./hve","version":"0.2.0","description":"fixture"}]}' \
  >"$MKT/.claude-plugin/marketplace.json"
printf '%s\n' '{"name":"hve","version":"0.2.0","description":"fixture"}' >"$MKT/hve/.claude-plugin/plugin.json"
printf -- '---\nname: workflow-agent\ndescription: fixture workflow agent\n---\n\nMCAO_HVE_LIVE_MARKER read the workflow-router skill first.\n' \
  >"$MKT/hve/agents/workflow-agent.md"

mkdir -p "$LAB/home/config" "$LAB/work" "$LAB/claude-store" "$LAB/copilot-store" "$LAB/codex-store"
# One run installs into every installed harness, each into its own throwaway
# store, so no developer store is touched.
SETUP_OUT=$(cd "$LAB/work" && CLAUDE_CONFIG_DIR="$LAB/claude-store" COPILOT_HOME="$LAB/copilot-store" \
  CODEX_HOME="$LAB/codex-store" FM_HOME="$LAB/home" MCAO_SETUP_HVE_SOURCE="$MKT" \
  bash "$ROOT/bin/mcao-setup.sh" plugins 2>&1)

CHECKED=0
SKIPPED=

for h in claude copilot codex; do
  if ! command -v "$h" >/dev/null 2>&1; then
    SKIPPED="$SKIPPED $h"
    note "skip: $h is not installed on this machine, so its plugin install is unverified here"
    continue
  fi
  version=$("$h" --version 2>/dev/null | head -1 | tr -d '\r')
  [ -n "$version" ] || version=unknown
  store="$LAB/$h-store"
  assert_contains "$SETUP_OUT" "$h: ready" "$h ($version): mcao setup plugins did not report ready"$'\n'"$SETUP_OUT"

  case $h in
  claude)
    resolved=$(cd "$LAB/work" && CLAUDE_CONFIG_DIR="$store" claude --agent mcao-hve-live-no-such-agent -p x 2>&1 || true)
    assert_contains "$resolved" "hve:workflow-agent" \
      "claude ($version): the enabled settings record no longer means claude resolves hve:workflow-agent"$'\n'"$resolved"
    ;;
  copilot)
    resolved=$(cd "$LAB/work" && COPILOT_HOME="$store" copilot --no-auto-update --agent mcao-hve-live-no-such-agent -p x 2>&1 || true)
    assert_contains "$resolved" "hve:workflow-agent" \
      "copilot ($version): the enabled settings record no longer means copilot resolves hve:workflow-agent"$'\n'"$resolved"
    ;;
  codex)
    with=$(cd "$LAB/work" && CODEX_HOME="$store" codex -p workflow-agent debug prompt-input x 2>&1 || true)
    without=$(cd "$LAB/work" && CODEX_HOME="$store" codex debug prompt-input x 2>&1 || true)
    assert_contains "$with" "MCAO_HVE_LIVE_MARKER" \
      "codex ($version): the workflow-agent profile setup wrote no longer reaches the prompt"
    assert_not_contains "$without" "MCAO_HVE_LIVE_MARKER" \
      "codex ($version): the marker appeared without -p, so the profile check proves nothing"
    ;;
  esac
  pass "mcao setup plugins: $h $version installs hve and its own lookup agrees with the spawn check"
  CHECKED=$((CHECKED + 1))
done

[ "$CHECKED" -gt 0 ] || fail \
  "none of claude, copilot or codex is installed, so this run proved nothing; install one before trusting a pass"
[ -z "$SKIPPED" ] || note "unverified on this machine (not installed):$SKIPPED"
note "checked $CHECKED installed harness(es)"
