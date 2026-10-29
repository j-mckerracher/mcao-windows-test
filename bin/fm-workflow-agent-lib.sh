#!/usr/bin/env bash
# The `workflow-agent` launch contract for ships and scouts.
# Sourced by bin/fm-spawn.sh and bin/mcao-setup.sh. This file is sourced by
# scripts and has no side effects on source.
#
# Why one owner: every ship or scout Firstmate dispatches runs as the workflow
# agent from the `hve` plugin in the Mayo skills marketplace, and each
# supported harness finds that agent its own way. This file owns the plugin
# identity, the supported-harness list, the record each harness's check reads,
# and the refusal, so the spawn guard and the `mcao setup plugins` install agree
# on one set of names and locations. bin/mcao-setup.sh owns the install steps.
#
#   harness       record the check reads                                    launch flag
#   claude        ${CLAUDE_CONFIG_DIR:-$HOME/.claude}/settings.json          --agent hve:workflow-agent
#   copilot       ${COPILOT_HOME:-$HOME/.copilot}/settings.json              --agent hve:workflow-agent
#   codex         ${CODEX_HOME:-$HOME/.codex}/workflow-agent.config.toml     -p workflow-agent
#   pi, pi-signed <pi root>/mao-workflow-agent/workflow-agent.md             --append-system-prompt <file> --skill <dir>
#   omp           $HOME/.omp/agent/mao-workflow-agent/workflow-agent.md      --append-system-prompt <file> --config <skills.yml>
#
# <pi root> is the worker account pin's root, else ${PI_CODING_AGENT_DIR:-$HOME/.pi/agent}.
#
# Claude and Copilot install the plugin natively and expose its agent as
# `hve:workflow-agent`; the bare name does not resolve on Copilot. Their check
# reads the vendor's own enablement record, `enabledPlugins["hve@mayo-skills"]`
# in the user settings file, which both write as true on install and false on
# disable. The live guard tests/mcao-setup-hve-live-e2e.test.sh proves that
# record and the vendor's own agent lookup still agree.
#
# Codex installs the plugin's skills but has no named agents, so setup writes
# the plugin agent's instructions into a profile that -p layers over the base
# config. Codex falls back silently on a missing profile, so the check proves
# the profile exists; setup writes it only after the plugin is enabled.
#
# Pi and omp cannot install `hve` as a plugin, so setup copies from the plugin
# already installed for claude or codex into one mao-owned folder per root: the
# agent body without its front matter (appended to the system prompt) and the
# plugin's skills (Pi loads them with --skill, omp through a skills.yml overlay
# naming them under skills.customDirectories, since omp has no --skill flag).
# The folder is not an auto-discovery path, so the captain's own Pi and omp
# sessions never see it. Passing any --append-system-prompt turns off Pi's
# APPEND_SYSTEM.md auto-discovery.
#
# Why Firstmate checks at all: when the agent is missing, claude and copilot
# print an error inside the pane, codex runs without its role, and Pi and omp append
# the path string as prompt text. None of those stops a worker from running
# without its role, so the spawn refuses first.
#
# A project-level agent or settings file in the worktree can still change what
# the harness loads at launch.
#
# Secondmates never use this contract; callers apply it to ships and scouts only.

# shellcheck disable=SC2034 # Names read by the sourcing scripts, fm-spawn.sh and mcao-setup.sh.
{
  FM_WORKFLOW_AGENT_NAME=workflow-agent
  FM_WORKFLOW_AGENT_PLUGIN=hve@mayo-skills
  FM_WORKFLOW_AGENT_MARKETPLACE=mayo-skills
  FM_WORKFLOW_AGENT_MARKETPLACE_SOURCE=mc-innersource/mayo-skills
  FM_WORKFLOW_AGENT_ID=hve:workflow-agent
  # The one declared hve plugin range: a version is supported when it is at least
  # FM_WORKFLOW_AGENT_HVE_MIN and below FM_WORKFLOW_AGENT_HVE_BEFORE. A routine bump
  # inside the range needs no host change; a breaking change moves the range here.
  FM_WORKFLOW_AGENT_HVE_MIN=0.2.0
  FM_WORKFLOW_AGENT_HVE_BEFORE=0.3.0
}

# Harnesses that can run a ship or scout as the workflow agent.
fm_workflow_agent_harnesses() {
  printf '%s\n' claude codex pi pi-signed omp copilot
}

fm_workflow_agent_supported() { # <harness>
  case "$1" in
  claude | codex | pi | pi-signed | omp | copilot) return 0 ;;
  *) return 1 ;;
  esac
}

# Harnesses whose workflow agent `mcao setup plugins` installs from the plugin.
fm_workflow_agent_plugin_harnesses() {
  printf '%s\n' claude copilot codex
}

# Harnesses whose workflow agent and skills `mcao setup plugins` copies out of the
# plugin installed for claude or codex.
fm_workflow_agent_copy_harnesses() {
  printf '%s\n' pi pi-signed omp
}

# fm_workflow_agent_dir <harness> [<root>]
# Prints the mao-owned folder holding the copied agent and skills for a
# copy harness. <root> is as for fm_workflow_agent_path. Returns 1 otherwise.
fm_workflow_agent_dir() {
  local harness=$1 root=${2:-}
  case "$harness" in
  pi | pi-signed) printf '%s/mao-workflow-agent\n' "${root:-${PI_CODING_AGENT_DIR:-$HOME/.pi/agent}}" ;;
  omp) printf '%s/.omp/agent/mao-workflow-agent\n' "$HOME" ;;
  *) return 1 ;;
  esac
}

# fm_workflow_agent_path <harness> [<root>]
# Prints the record the check reads for the harness (table above). <root> is
# the directory the launch will select when a worker account pin names one
# (claude and pi; omp has none); it replaces the environment-or-default lookup. Returns 1 for
# an unsupported harness.
fm_workflow_agent_path() {
  local harness=$1 root=${2:-}
  case "$harness" in
  claude) printf '%s/settings.json\n' "${root:-${CLAUDE_CONFIG_DIR:-$HOME/.claude}}" ;;
  copilot) printf '%s/settings.json\n' "${COPILOT_HOME:-$HOME/.copilot}" ;;
  codex) printf '%s/%s.config.toml\n' "${CODEX_HOME:-$HOME/.codex}" "$FM_WORKFLOW_AGENT_NAME" ;;
  pi | pi-signed | omp) printf '%s/%s.md\n' "$(fm_workflow_agent_dir "$harness" "$root")" "$FM_WORKFLOW_AGENT_NAME" ;;
  *) return 1 ;;
  esac
}

# fm_workflow_agent_manifest_versions <plugin.json>
# Prints "<path>\t<version>" for a plugin manifest that names a version. Prints
# nothing when the file is absent or unreadable, so the caller refuses.
fm_workflow_agent_manifest_versions() {
  local file=$1
  [ -f "$file" ] || return 0
  jq -r --arg f "$file" 'select(.version) | "\($f)\t\(.version)"' "$file" 2>/dev/null
}

# fm_workflow_agent_claude_versions <installed_plugins.json>
# Prints "<path>\t<version>" for each install of the hve plugin in the claude
# install record, one line per scope entry.
fm_workflow_agent_claude_versions() {
  local file=$1
  [ -f "$file" ] || return 0
  jq -r --arg p "$FM_WORKFLOW_AGENT_PLUGIN" --arg f "$file" \
    '.plugins[$p][]? | select(.version) | "\($f)\t\(.version)"' "$file" 2>/dev/null
}

# fm_workflow_agent_codex_versions
# Prints "<path>\t<version>" for each hve plugin manifest in the codex plugin cache.
fm_workflow_agent_codex_versions() {
  local short=${FM_WORKFLOW_AGENT_PLUGIN%@*} manifest
  for manifest in "${CODEX_HOME:-$HOME/.codex}/plugins/cache/$FM_WORKFLOW_AGENT_MARKETPLACE/$short"/*/.claude-plugin/plugin.json; do
    [ -f "$manifest" ] || continue
    fm_workflow_agent_manifest_versions "$manifest"
  done
}

# fm_workflow_agent_copilot_live_versions <settings.json>
# Prints "<manifest path>\t<version>" for the hve plugin copilot runs live from a
# local directory marketplace. Copilot keeps no copy under installed-plugins/ for
# those, so the record is settings.json: the plugin is enabled and its marketplace
# is a directory source; that directory's marketplace entry names the plugin's
# source directory, whose manifest holds the version. Prints nothing otherwise.
fm_workflow_agent_copilot_live_versions() {
  local settings=$1 short=${FM_WORKFLOW_AGENT_PLUGIN%@*} dir rel
  [ -f "$settings" ] || return 0
  dir=$(jq -r --arg p "$FM_WORKFLOW_AGENT_PLUGIN" --arg m "$FM_WORKFLOW_AGENT_MARKETPLACE" \
    'if (.enabledPlugins[$p] == true) and (.extraKnownMarketplaces[$m].source.source == "directory")
     then .extraKnownMarketplaces[$m].source.path else empty end' "$settings" 2>/dev/null) || return 0
  [ -n "$dir" ] || return 0
  rel=$(jq -r --arg s "$short" \
    '.plugins[]? | select(.name == $s and (.source | type) == "string") | .source' \
    "$dir/.claude-plugin/marketplace.json" 2>/dev/null | head -n 1) || return 0
  [ -n "$rel" ] || return 0
  fm_workflow_agent_manifest_versions "$dir/$rel/.claude-plugin/plugin.json"
}

# fm_workflow_agent_versions <harness> [<root>]
# Prints "<record path>\t<version>" for every installed hve version the harness
# runs, read from the install record without running the harness CLI. Pi and omp
# copy the agent from the plugin claude or codex installed (bin/mcao-setup.sh
# hve_plugin_root), so they read the claude record and fall back to codex's cache.
# Prints nothing when no version is readable. Returns 1 for an unsupported harness.
fm_workflow_agent_versions() {
  local harness=$1 root=${2:-} short out
  short=${FM_WORKFLOW_AGENT_PLUGIN%@*}
  case "$harness" in
  claude) fm_workflow_agent_claude_versions "${root:-${CLAUDE_CONFIG_DIR:-$HOME/.claude}}/plugins/installed_plugins.json" ;;
  copilot)
    local copilot_home=${COPILOT_HOME:-$HOME/.copilot}
    out=$(fm_workflow_agent_manifest_versions "$copilot_home/installed-plugins/$FM_WORKFLOW_AGENT_MARKETPLACE/$short/.claude-plugin/plugin.json")
    [ -n "$out" ] || out=$(fm_workflow_agent_copilot_live_versions "$copilot_home/settings.json")
    [ -z "$out" ] || printf '%s\n' "$out"
    ;;
  codex) fm_workflow_agent_codex_versions ;;
  pi | pi-signed | omp)
    out=$(fm_workflow_agent_claude_versions "${CLAUDE_CONFIG_DIR:-$HOME/.claude}/plugins/installed_plugins.json")
    [ -n "$out" ] || out=$(fm_workflow_agent_codex_versions)
    [ -z "$out" ] || printf '%s\n' "$out"
    ;;
  *) return 1 ;;
  esac
}

# fm_workflow_agent_version_key <major.minor.patch>
# Prints a sortable integer for a plain three-part version and returns 1 for
# anything else, so an unparseable version is refused rather than compared.
fm_workflow_agent_version_key() {
  local major minor patch extra
  case "$1" in
  '' | *[!0-9.]* | .* | *. | *..*) return 1 ;;
  esac
  IFS=. read -r major minor patch extra <<<"$1"
  [ -n "$major" ] && [ -n "$minor" ] && [ -n "$patch" ] && [ -z "$extra" ] || return 1
  printf '%d\n' $((10#$major * 1000000000000 + 10#$minor * 1000000 + 10#$patch))
}

# fm_workflow_agent_check_version <harness> [<root>]
# Refuses (prints one error, returns 1) unless every installed hve version for the
# harness is inside the declared range. The error names the version, the checked
# record, and the range.
fm_workflow_agent_check_version() {
  local harness=$1 root=${2:-} range lo hi where version key rows line
  range=">=$FM_WORKFLOW_AGENT_HVE_MIN <$FM_WORKFLOW_AGENT_HVE_BEFORE"
  lo=$(fm_workflow_agent_version_key "$FM_WORKFLOW_AGENT_HVE_MIN") || return 1
  hi=$(fm_workflow_agent_version_key "$FM_WORKFLOW_AGENT_HVE_BEFORE") || return 1
  if ! command -v jq >/dev/null 2>&1; then
    echo "error: jq is required to read the $FM_WORKFLOW_AGENT_PLUGIN plugin version for $harness (supported range $range)" >&2
    return 1
  fi
  rows=$(fm_workflow_agent_versions "$harness" "$root")
  if [ -z "$rows" ]; then
    echo "error: could not read the $FM_WORKFLOW_AGENT_PLUGIN plugin version for $harness (supported range $range); run 'mcao setup plugins'" >&2
    return 1
  fi
  while IFS= read -r line; do
    where=${line%%$'\t'*}
    version=${line#*$'\t'}
    if ! key=$(fm_workflow_agent_version_key "$version"); then
      echo "error: $FM_WORKFLOW_AGENT_PLUGIN plugin version '$version' for $harness is not a plain major.minor.patch version (checked $where; supported range $range)" >&2
      return 1
    fi
    if [ "$key" -lt "$lo" ] || [ "$key" -ge "$hi" ]; then
      echo "error: $FM_WORKFLOW_AGENT_PLUGIN plugin version $version for $harness is outside the host's supported range $range (checked $where); install a version in that range, or move the range in bin/fm-workflow-agent-lib.sh for a deliberate upgrade" >&2
      return 1
    fi
  done <<<"$rows"
}

# fm_workflow_agent_require <harness> [<root>]
# Refuses (prints one error, returns 1) when the harness cannot run a ship or
# scout as the workflow agent, its agent is not installed, or the installed hve
# version is outside the declared range. Prints the checked record's path on
# success; the Pi and omp launches append that file.
fm_workflow_agent_require() {
  local harness=$1 root=${2:-} path dir need
  if ! fm_workflow_agent_supported "$harness"; then
    echo "error: ship and scout dispatch supports only claude, codex, pi, pi-signed, omp, and copilot (got '$harness'); select a supported harness" >&2
    return 1
  fi
  path=$(fm_workflow_agent_path "$harness" "$root")
  case "$harness" in
  claude | copilot)
    if ! command -v jq >/dev/null 2>&1; then
      echo "error: jq is required to check that the $FM_WORKFLOW_AGENT_PLUGIN plugin is enabled for $harness" >&2
      return 1
    fi
    if ! jq -e --arg p "$FM_WORKFLOW_AGENT_PLUGIN" '.enabledPlugins[$p] == true' "$path" >/dev/null 2>&1; then
      echo "error: the $FM_WORKFLOW_AGENT_PLUGIN plugin that provides $FM_WORKFLOW_AGENT_ID is not installed and enabled for $harness (checked $path); run 'mcao setup plugins'" >&2
      return 1
    fi
    ;;
  codex)
    if [ ! -f "$path" ]; then
      echo "error: $FM_WORKFLOW_AGENT_NAME profile missing at $path; run 'mcao setup plugins'" >&2
      return 1
    fi
    ;;
  pi | pi-signed | omp)
    dir=$(fm_workflow_agent_dir "$harness" "$root")
    for need in "$path" "$dir/skills"; do
      if [ ! -e "$need" ]; then
        echo "error: $FM_WORKFLOW_AGENT_NAME definition missing at $need; run 'mcao setup plugins' to copy it from the $FM_WORKFLOW_AGENT_PLUGIN plugin" >&2
        return 1
      fi
    done
    if [ "$harness" = omp ] && [ ! -f "$dir/skills.yml" ]; then
      echo "error: $FM_WORKFLOW_AGENT_NAME skills overlay missing at $dir/skills.yml; run 'mcao setup plugins'" >&2
      return 1
    fi
    ;;
  esac
  fm_workflow_agent_check_version "$harness" "$root" || return 1
  printf '%s\n' "$path"
}
