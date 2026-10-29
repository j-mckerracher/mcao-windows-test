#!/usr/bin/env bash
# tests/mcao-pi-omp-workflow-agent-live-e2e.test.sh - opt-in live guard proving that
# `mcao setup plugins` makes the hve workflow agent usable by every INSTALLED Pi and
# omp: the agent prompt reaches the system prompt and the plugin's skills load,
# through the same flags bin/fm-spawn.sh launches a ship or scout with.
#
# Why this file exists: Pi's --skill, omp's --config skills overlay and both
# --append-system-prompt flags are vendor surfaces. A stubbed CLI can only confirm
# what the stub already assumes, so this guard runs the real CLIs.
# It SUBMITS a short prompt to a model per harness, so it is opt-in:
# FM_MCAO_PI_OMP_LIVE=1 (or FM_LIVE=1) turns it on, and an absent tool then fails.
#   - setup copies from a throwaway claude install of a fixture marketplace, under
#     a throwaway HOME, so no developer store is touched.
#   - the model is asked to repeat a marker that only the appended prompt carries
#     and to name a skill that only the copied skills folder carries.
# FM_MCAO_PI_OMP_LIVE_PI_MODEL picks the Pi model (default github-copilot/claude-haiku-4.5);
# omp uses its own default model.
# The portable counterpart in tests/mcao-setup.test.sh pins setup's own logic.
# Run this guard after any pi or omp upgrade.
set -u

# shellcheck source=tests/lib.sh
. "$(dirname "${BASH_SOURCE[0]}")/lib.sh"

fm_live_gate opt-in FM_MCAO_PI_OMP_LIVE jq claude

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
LAB=$(mktemp -d "${TMPDIR:-/tmp}/mcao-pi-omp-live.XXXXXX")
cleanup_all() { [ -n "${LAB:-}" ] && rm -rf "$LAB"; }
trap cleanup_all EXIT
note() { printf '# %s\n' "$1"; }

MARKER=MCAO_HVE_LIVE_MARKER_4417
SKILL=mcao-live-probe-skill
PI_MODEL=${FM_MCAO_PI_OMP_LIVE_PI_MODEL:-github-copilot/claude-haiku-4.5}

# A fixture marketplace with the same names as the real one: no network or credentials.
MKT="$LAB/marketplace"
mkdir -p "$MKT/.claude-plugin" "$MKT/hve/.claude-plugin" "$MKT/hve/agents" "$MKT/hve/skills/$SKILL"
printf '%s\n' '{"name":"mayo-skills","owner":{"name":"fixture"},"plugins":[{"name":"hve","source":"./hve","version":"0.0.1","description":"fixture"}]}' \
  >"$MKT/.claude-plugin/marketplace.json"
printf '%s\n' '{"name":"hve","version":"0.0.1","description":"fixture"}' >"$MKT/hve/.claude-plugin/plugin.json"
printf -- '---\nname: workflow-agent\ndescription: fixture workflow agent\n---\n\nThe workflow marker is %s.\n' "$MARKER" \
  >"$MKT/hve/agents/workflow-agent.md"
printf -- '---\nname: %s\ndescription: Probe skill used by the live guard.\n---\nProbe.\n' "$SKILL" \
  >"$MKT/hve/skills/$SKILL/SKILL.md"

mkdir -p "$LAB/uh" "$LAB/home/config" "$LAB/work" "$LAB/claude-store"
SETUP_OUT=$(cd "$LAB/work" && HOME="$LAB/uh" CLAUDE_CONFIG_DIR="$LAB/claude-store" FM_HOME="$LAB/home" \
  MCAO_SETUP_HVE_SOURCE="$MKT" bash "$ROOT/bin/mcao-setup.sh" plugins 2>&1)

PROMPT="Reply with exactly two items separated by a comma: the workflow marker from your instructions, then the name of the skill whose name starts with mcao-live-probe. Nothing else."
CHECKED=0
SKIPPED=

for h in pi omp; do
  if ! command -v "$h" >/dev/null 2>&1; then
    SKIPPED="$SKIPPED $h"
    note "skip: $h is not installed on this machine, so its workflow agent is unverified here"
    continue
  fi
  version=$("$h" --version 2>/dev/null | head -1 | tr -d '\r')
  [ -n "$version" ] || version=unknown
  assert_contains "$SETUP_OUT" "$h: ready" "$h ($version): mcao setup plugins did not report ready"$'\n'"$SETUP_OUT"

  case $h in
  pi)
    dir="$LAB/uh/.pi/agent/mao-workflow-agent"
    out=$(cd "$LAB/work" && pi -p --no-context-files --model "$PI_MODEL" \
      --append-system-prompt "$dir/workflow-agent.md" --skill "$dir/skills" "$PROMPT" 2>&1 || true)
    ;;
  omp)
    dir="$LAB/uh/.omp/agent/mao-workflow-agent"
    out=$(cd "$LAB/work" && OMP_SKIP_SETUP=1 omp -p --config "$dir/skills.yml" \
      --append-system-prompt "$dir/workflow-agent.md" "$PROMPT" 2>&1 || true)
    ;;
  esac
  assert_contains "$out" "$MARKER" "$h ($version): the appended workflow-agent prompt did not reach the model"$'\n'"$out"
  assert_contains "$out" "$SKILL" "$h ($version): the copied hve skills did not load"$'\n'"$out"
  pass "mcao setup plugins: $h $version runs the copied workflow agent with its skills loaded"
  CHECKED=$((CHECKED + 1))
done

[ "$CHECKED" -gt 0 ] || fail \
  "neither pi nor omp is installed, so this run proved nothing; install one before trusting a pass"
[ -z "$SKIPPED" ] || note "unverified on this machine (not installed):$SKIPPED"
note "checked $CHECKED installed harness(es)"
