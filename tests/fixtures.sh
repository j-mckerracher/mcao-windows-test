#!/usr/bin/env bash
# tests/fixtures.sh - shared fake-toolchain and spawn-world builders.
#
# Source this from a test file:
#   # shellcheck source=tests/fixtures.sh
#   . "$(dirname "${BASH_SOURCE[0]}")/fixtures.sh"
#
# Generic reporters, temp roots, git fixtures, and fail/pass/fm_test_cleanup
# come from tests/lib.sh, pulled in below. This file owns the shared fake
# no-mistakes, gh, gh-axi, tmux, ssh, and spawn-world helpers. Wake-queue mocks
# stay in wake-helpers.sh; secondmate-lifecycle mocks stay in
# secondmate-helpers.sh.
#
# FM_TEST_NO_MISTAKES_VERSION is the single default version for the shared fake
# no-mistakes banner. Override a single case with FM_FAKE_NO_MISTAKES_VERSION
# rather than editing a stub body.

# shellcheck source=tests/lib.sh
. "$(dirname "${BASH_SOURCE[0]}")/lib.sh"
# shellcheck source=bin/fm-branch-prefix-lib.sh
. "$(dirname "${BASH_SOURCE[0]}")/../bin/fm-branch-prefix-lib.sh"
# shellcheck source=tests/assignment-profile-helpers.sh
. "$(dirname "${BASH_SOURCE[0]}")/assignment-profile-helpers.sh"

if [ -n "${FM_TEST_FIXTURES_SOURCED:-}" ]; then
  return 0
fi
FM_TEST_FIXTURES_SOURCED=1

# Production floor lives in bin/fm-bootstrap.sh (NO_MISTAKES_MIN). Keep this
# equal to that floor so a bump is one constant here plus that production pin.
export FM_TEST_NO_MISTAKES_VERSION=1.46.0
export FM_TEST_NO_MISTAKES_FAKE_VERSION="no-mistakes version v${FM_TEST_NO_MISTAKES_VERSION} (fake)"
export FM_TEST_NO_MISTAKES_FAKE_VERSION_TS="${FM_TEST_NO_MISTAKES_FAKE_VERSION} 2026-06-27T00:02:18Z"
export FM_TEST_GH_AXI_VERSION=0.1.29

# --- fake no-mistakes -------------------------------------------------------

# fm_test_fake_no_mistakes <fakebin>
# Drops a no-mistakes stub that answers --version with
# FM_TEST_NO_MISTAKES_FAKE_VERSION (or FM_FAKE_NO_MISTAKES_VERSION when set)
# and exits 0 for every other invocation.
fm_test_fake_no_mistakes() {
  local fakebin=$1
  cat > "$fakebin/no-mistakes" <<SH
#!/usr/bin/env bash
if [ "\${1:-}" = --version ]; then
  printf '%s\\n' "\${FM_FAKE_NO_MISTAKES_VERSION:-$FM_TEST_NO_MISTAKES_FAKE_VERSION}"
  exit 0
fi
exit 0
SH
  chmod +x "$fakebin/no-mistakes"
}

# fm_test_fake_no_mistakes_init_doctor <fakebin>
# Secondmate-lifecycle stub: init/doctor touch marker files; other verbs exit 2.
# Does not answer --version (those suites never probe the floor).
fm_test_fake_no_mistakes_init_doctor() {
  local fakebin=$1
  cat > "$fakebin/no-mistakes" <<'SH'
#!/usr/bin/env bash
set -eu
case "${1:-}" in
  init) touch .no-mistakes-init ;;
  doctor) touch .no-mistakes-doctor ;;
  *) exit 2 ;;
esac
SH
  chmod +x "$fakebin/no-mistakes"
}

# --- fake gh / gh-axi -------------------------------------------------------

# fm_test_fake_gh <fakebin>
# Authenticates (`gh auth status` exits 0) and otherwise exits 0.
fm_test_fake_gh() {
  local fakebin=$1
  cat > "$fakebin/gh" <<'SH'
#!/usr/bin/env bash
if [ "${1:-}" = auth ] && [ "${2:-}" = status ]; then
  exit 0
fi
exit 0
SH
  chmod +x "$fakebin/gh"
}

# fm_test_fake_gh_axi <fakebin>
# Answers --version with FM_FAKE_GH_AXI_VERSION or FM_TEST_GH_AXI_VERSION.
fm_test_fake_gh_axi() {
  local fakebin=$1
  fm_fake_version_tool "$fakebin" gh-axi FM_FAKE_GH_AXI_VERSION "$FM_TEST_GH_AXI_VERSION"
}

# --- fake tmux / ssh / sleep ------------------------------------------------

# fm_test_fake_tmux_spawn <fakebin>
# Spawn-world tmux: pane_current_path from FM_FAKE_PANE_PATH, session named
# firstmate, window ops succeed, send-keys succeed. When FM_FAKE_LAUNCH_LOG is
# set, each send-keys -l payload is appended one per line. When FM_FAKE_PANE_LOG
# is set, each send-keys TEXT-LINE payload (the pre-launch pane exports, which
# carry no -l) is appended there instead, one per line in send order. Optional
# FM_FAKE_DUPLICATE_WINDOW is printed from list-windows.
#
# The pane path defaults to empty when FM_FAKE_PANE_PATH is unset. Window
# cleanup and option operations are no-ops. Launch logging is env-gated, so
# suites that do not set FM_FAKE_LAUNCH_LOG keep a silent send-keys.
fm_test_fake_tmux_spawn() {
  local fakebin=$1
  cat > "$fakebin/tmux" <<'SH'
#!/usr/bin/env bash
set -u
case "$*" in
  *"#{pane_current_path}"*) printf '%s\n' "${FM_FAKE_PANE_PATH:-}"; exit 0 ;;
esac
case "${1:-}" in
  display-message) printf 'firstmate\n'; exit 0 ;;
  list-windows)
    if [ -n "${FM_FAKE_DUPLICATE_WINDOW:-}" ]; then
      printf '%s\n' "$FM_FAKE_DUPLICATE_WINDOW"
    fi
    exit 0
    ;;
  has-session|new-session|new-window|kill-window|set-window-option) exit 0 ;;
  send-keys)
    if [ -n "${FM_FAKE_LAUNCH_LOG:-}" ]; then
      prev=
      for a in "$@"; do
        if [ "$prev" = "-l" ]; then
          # A spawn types a short line sourcing its staged launch file; log
          # the staged command itself so suites assert what the pane runs.
          # Direct literals past the terminal line buffer are truncated, so a
          # long launch only survives when it arrived through that short source.
          case "$a" in
            ". '"*"'")
              staged=${a#". '"}
              staged=${staged%"'"}
              if [ -f "$staged" ]; then
                a=$(cat "$staged")
              elif [ "${#a}" -gt 1024 ]; then
                a=${a:0:1024}
              fi
              ;;
            *)
              if [ "${#a}" -gt 1024 ]; then
                a=${a:0:1024}
              fi
              ;;
          esac
          printf '%s\n' "$a" >> "$FM_FAKE_LAUNCH_LOG"
        fi
        prev=$a
      done
    fi
    # The pre-launch pane exports ride the text-line form
    # (`send-keys -t <target> <text> Enter`), which carries no -l flag, so a
    # suite that asserts on what the pane shell received opts in with its own
    # log. Skip the flags, the target, and the trailing key so only the payload
    # is recorded, one per line, in send order.
    if [ -n "${FM_FAKE_PANE_LOG:-}" ]; then
      shift
      skip_next=
      literal=
      for a in "$@"; do
        if [ -n "$skip_next" ]; then skip_next=; continue; fi
        case "$a" in
          -t) skip_next=1; continue ;;
          -l) literal=1; continue ;;
          Enter|C-m) continue ;;
          *) [ -n "$literal" ] || printf '%s\n' "$a" >> "$FM_FAKE_PANE_LOG" ;;
        esac
      done
    fi
    exit 0
    ;;
esac
exit 0
SH
  chmod +x "$fakebin/tmux"
}

# fm_test_fake_tmux_send <fakebin>
# Send-world tmux: logs send-keys -l payloads to FM_SEND_LOG, reports a numeric
# cursor_y, and renders an empty bordered composer so the submit path reads
# empty. Env knobs:
#   FM_FAKE_TMUX_SEND_FAIL=1  send-keys exits 1
#   FM_FAKE_TMUX_COMPOSER=pending  capture-pane shows leftover composer text
fm_test_fake_tmux_send() {
  local fakebin=$1
  cat > "$fakebin/tmux" <<'SH'
#!/usr/bin/env bash
set -u
case "${1:-}" in
  send-keys)
    [ "${FM_FAKE_TMUX_SEND_FAIL:-0}" = 1 ] && exit 1
    shift
    literal=0
    while [ $# -gt 0 ]; do
      case "$1" in
        -t) shift 2 ;;
        -l) literal=1; shift ;;
        *) break ;;
      esac
    done
    if [ "$literal" = 1 ]; then
      printf '%s' "${1:-}" >> "${FM_SEND_LOG:-/dev/null}"
    fi
    exit 0
    ;;
  display-message)
    for a in "$@"; do
      case "$a" in *cursor_y*) printf '1\n'; exit 0 ;; esac
    done
    printf 'fakepane\n'
    exit 0
    ;;
  capture-pane)
    if [ "${FM_FAKE_TMUX_COMPOSER:-}" = pending ]; then
      printf '╭──────────────╮\n│ leftover txt │\n╰──────────────╯\n'
    else
      printf '╭────╮\n│    │\n╰────╯\n'
    fi
    exit 0
    ;;
  list-windows) exit 0 ;;
esac
exit 0
SH
  chmod +x "$fakebin/tmux"
}

# fm_test_fake_ssh <fakebin> [name]
# Records argv to FM_SSH_LOG, consumes stdin, exits FM_FAKE_SSH_RC (default 0).
# Default name is fake-ssh so tests can point FM_SSH_BIN at it without
# shadowing a real ssh on PATH.
fm_test_fake_ssh() {
  local fakebin=$1 name=${2:-fake-ssh}
  cat > "$fakebin/$name" <<'SH'
#!/usr/bin/env bash
cat > /dev/null
printf '%s\n' "$*" >> "${FM_SSH_LOG:-/dev/null}"
exit "${FM_FAKE_SSH_RC:-0}"
SH
  chmod +x "$fakebin/$name"
}

# fm_test_fake_sleep_noop <fakebin>
fm_test_fake_sleep_noop() {
  local fakebin=$1
  cat > "$fakebin/sleep" <<'SH'
#!/usr/bin/env bash
exit 0
SH
  chmod +x "$fakebin/sleep"
}

# fm_test_fake_sleep_log <fakebin>
# Records each requested duration to FM_SLEEP_LOG instead of sleeping.
fm_test_fake_sleep_log() {
  local fakebin=$1
  cat > "$fakebin/sleep" <<'SH'
#!/usr/bin/env bash
printf '%s\n' "${1:-}" >> "${FM_SLEEP_LOG:-/dev/null}"
exit 0
SH
  chmod +x "$fakebin/sleep"
}

# --- spawn-world ------------------------------------------------------------

# fm_test_spawn_home <home> [harness]
# Minimal firstmate home layout plus watcher-liveness beat. Optional harness
# pin is written to config/crew-harness.
fm_test_spawn_home() {
  local home=$1 harness=${2-}
  mkdir -p "$home/data" "$home/projects" "$home/state" "$home/config"
  touch "$home/state/.last-watcher-beat"
  if [ -n "$harness" ]; then
    printf '%s\n' "$harness" > "$home/config/crew-harness"
  fi
}

# fm_test_spawn_brief <home> <id> [captain-intent]
fm_test_spawn_brief() {
  local home=$1 id=$2 intent=${3:-brief for $2}
  mkdir -p "$home/data/$id"
  cat > "$home/data/$id/brief.md" <<EOF
# Task
## Captain's intent
$intent

## Firstmate spec
Exercise the spawn behavior under test.
EOF
}

# fm_test_managed_block <id> <mode>
# Prints the managed-context section a ship brief carries (docs/managed-context.md)
# for task <id> launched with <mode>. The worktree stays the placeholder the launch
# binds, and <mode> must match the spawn's --mode. Append it to a ship brief.
fm_test_managed_block() {
  printf '%s\n' '# Managed context' '```managed-context' \
    'ownership: motif' "task_id: $1" 'worktree: {WORKTREE}' \
    "delivery_mode: $2" 'helper_allowance: read-only-checks, isolated-scratch' \
    '```'
}

# fm_test_spawn_ship_brief <home> <id> <mode> [captain-intent]
# A ship brief: fm_test_spawn_brief plus the managed-context block that every
# ship launch requires. See fm_test_managed_block.
# fm_test_ship_branch_line <home> <id>
# Appends the "Ship branch:" record that bin/fm-brief.sh writes into every ship
# brief. fm-spawn refuses a ship brief without it unless the spawn's branch is
# the legacy fm/<id>, so fixtures record the default prefix a spawn selects.
fm_test_ship_branch_line() {
  # shellcheck disable=SC2031 # read-only use of the constant set by the library sourced at the top of this file
  printf 'Ship branch: %s%s\n' "$FM_DEFAULT_BRANCH_PREFIX" "$2" >>"$1/data/$2/brief.md"
}

fm_test_spawn_ship_brief() {
  local home=$1 id=$2 mode=$3 intent=${4:-brief for $2}
  fm_test_spawn_brief "$home" "$id" "$intent"
  fm_test_ship_branch_line "$home" "$id"
  fm_test_managed_block "$id" "$mode" >>"$home/data/$id/brief.md"
}

# fm_test_ensure_ship_managed_block <home> <id> <spawn-args...>
# Appends the managed-context block to an already-written brief when the spawn
# arguments describe a ship launch: an explicit --mode with neither --scout nor
# --secondmate. Suites that scaffold plain briefs use it so their ship spawns
# satisfy the launch contract; a scout or a block already present is left alone.
# Relaunches preserve an absent branch record so the recorded branch can be reused.
fm_test_ensure_ship_managed_block() {
  local home=$1 id=$2 brief mode="" prev="" arg relaunch=0
  shift 2
  for arg in "$@"; do
    case "$arg" in
      --scout|--secondmate) return 0 ;;
      --relaunch) relaunch=1 ;;
    esac
    [ "$prev" = --mode ] && mode=$arg
    prev=$arg
  done
  case "$mode" in no-mistakes|direct-PR) ;; *) return 0 ;; esac
  brief="$home/data/$id/brief.md"
  [ -f "$brief" ] || return 0
  if [ "$relaunch" = 0 ]; then
    grep -q '^Ship branch: ' "$brief" || fm_test_ship_branch_line "$home" "$id"
  fi
  grep -qx '```managed-context' "$brief" && return 0
  fm_test_managed_block "$id" "$mode" >>"$brief"
}

# fm_test_make_spawn_fakebin <dir> [extra-exit0-tool...]
# Creates <dir>/fakebin with the spawn tmux stub, a no-op treehouse, and any
# extra exit-0 tools. Echoes the fakebin path.
fm_test_make_spawn_fakebin() {
  local dir=$1 fakebin
  shift
  fakebin=$(fm_fakebin "$dir")
  fm_test_fake_tmux_spawn "$fakebin"
  fm_fake_exit0 "$fakebin" treehouse "$@"
  printf '%s\n' "$fakebin"
}

# Drop-in name used by the spawn suites. Extra args are additional exit-0 tools
# (gh, gh-axi, pi, ...).
make_spawn_fakebin() {
  fm_test_make_spawn_fakebin "$@"
}

# fm_test_seed_workflow_agents <user-home> [<claude-config-dir>]
# Records the workflow agent as installed for every harness that runs ships and
# scouts, at the default location under <user-home> (claude's moves to
# <claude-config-dir> when one is given): the hve plugin enabled in the claude
# and copilot user settings, the codex profile, and the Pi and omp agent folders. Layout
# owner: bin/fm-workflow-agent-lib.sh. Each install also records an hve version
# in the record its check reads (claude's installed_plugins.json, the codex and
# copilot plugin manifests), at FM_TEST_HVE_VERSION or 0.2.0, the in-range
# version the host declares.
fm_test_seed_workflow_agents() {
  local uh=$1 claude_dir=${2:-} enabled='{"enabledPlugins":{"hve@mayo-skills":true}}'
  local hve_version=${FM_TEST_HVE_VERSION:-0.2.0}
  [ -n "$claude_dir" ] || claude_dir=$uh/.claude
  mkdir -p "$claude_dir/plugins" "$uh/.codex/plugins/cache/mayo-skills/hve/$hve_version/.claude-plugin" \
    "$uh/.copilot/installed-plugins/mayo-skills/hve/.claude-plugin" \
    "$uh/.pi/agent/mao-workflow-agent/skills" "$uh/.omp/agent/mao-workflow-agent/skills" "$uh/.copilot"
  printf '%s\n' "$enabled" >"$claude_dir/settings.json"
  printf '{"version":2,"plugins":{"hve@mayo-skills":[{"scope":"user","version":"%s"}]}}\n' "$hve_version" \
    >"$claude_dir/plugins/installed_plugins.json"
  printf '%s\n' "$enabled" >"$uh/.copilot/settings.json"
  printf '{"name":"hve","version":"%s"}\n' "$hve_version" \
    >"$uh/.copilot/installed-plugins/mayo-skills/hve/.claude-plugin/plugin.json"
  printf '{"name":"hve","version":"%s"}\n' "$hve_version" \
    >"$uh/.codex/plugins/cache/mayo-skills/hve/$hve_version/.claude-plugin/plugin.json"
  printf 'developer_instructions = "Test workflow agent."\n' >"$uh/.codex/workflow-agent.config.toml"
  printf 'Test workflow agent.\n' >"$uh/.pi/agent/mao-workflow-agent/workflow-agent.md"
  printf 'Test workflow agent.\n' >"$uh/.omp/agent/mao-workflow-agent/workflow-agent.md"
  printf 'skills:\n  customDirectories:\n    - %s/.omp/agent/mao-workflow-agent/skills\n' "$uh" >"$uh/.omp/agent/mao-workflow-agent/skills.yml"
}

# fm_test_run_spawn <home> <pane-path> <fakebin> [fm-spawn args...]
# Common spawn env. Extra variables in the caller (GROK_HOME, FM_FAKE_LAUNCH_LOG,
# CLAUDE_CONFIG_DIR, ...) are inherited. Does not add --mode/--yolo; ship tests
# that need a delivery contract pass those flags themselves.
fm_test_run_spawn() {
  local home=$1 pane=$2 fakebin=$3
  shift 3
  # A claude spawn pre-registers workspace trust in the launching user's own
  # store (bin/fm-claude-trust.sh), so every spawn here runs against a throwaway
  # HOME; without it the suite would write the developer's real ~/.claude.json.
  # CLAUDE_CONFIG_DIR must be pinned too, and pinned EMPTY: the script resolves
  # the store as ${CLAUDE_CONFIG_DIR:-${HOME:-}}, so a value inherited from the
  # developer's shell would beat the throwaway HOME and the sandbox would not
  # hold, while an empty value falls through to it. Empty rather than a path
  # because bin/fm-spawn.sh prefixes the launch only when the value is non-empty,
  # so every launch-shape assertion in the suite keeps reading the same command.
  # A test that needs the set case opts in through FM_TEST_CLAUDE_CONFIG_DIR.
  local spawn_home=$home/user-home base
  mkdir -p "$spawn_home"
  # A ship launch carries its assignment-profile record, pinned to the worktree
  # HEAD it starts from. A test of the refusal sets FM_TEST_NO_ASSIGNMENT_PROFILE=1.
  if [ "${FM_TEST_NO_ASSIGNMENT_PROFILE:-0}" != 1 ] && base=$(git -C "$pane" rev-parse --verify --quiet HEAD); then
    fm_test_ensure_ship_assignment_profile "$home" "$1" "$base" "$@"
  fi
  # Ships and scouts refuse to spawn without their workflow-agent definition
  # (bin/fm-workflow-agent-lib.sh), so seed one per supported harness inside the
  # throwaway home. A test of the refusal sets FM_TEST_NO_WORKFLOW_AGENT=1.
  # CODEX_HOME, COPILOT_HOME, and PI_CODING_AGENT_DIR are pinned empty for the
  # same reason as CLAUDE_CONFIG_DIR: the developer's shell must not leak in.
  if [ "${FM_TEST_NO_WORKFLOW_AGENT:-0}" != 1 ]; then
    fm_test_seed_workflow_agents "$spawn_home" "${FM_TEST_CLAUDE_CONFIG_DIR:-}"
  fi
  # Suites exercise spawn behavior, not the verification readiness gate, so their
  # spawns are explicit per-run requests and carry --attended (bin/fm-verification-
  # readiness-lib.sh). A suite that tests the gate sets FM_TEST_UNATTENDED=1. A
  # relaunch or secondmate spawn is never gated, and --attended is refused there.
  local attend=() arg
  if [ "${FM_TEST_UNATTENDED:-0}" != 1 ]; then
    attend=(--attended)
    for arg in "$@"; do
      case "$arg" in
        --relaunch | --secondmate) attend=() ;;
      esac
    done
  fi
  FM_ROOT_OVERRIDE='' FM_HOME="$home" HOME="$spawn_home" \
    CODEX_HOME='' COPILOT_HOME='' PI_CODING_AGENT_DIR='' \
    CLAUDE_CONFIG_DIR="${FM_TEST_CLAUDE_CONFIG_DIR:-}" \
    FM_STATE_OVERRIDE="$home/state" FM_DATA_OVERRIDE="$home/data" \
    FM_PROJECTS_OVERRIDE="$home/projects" FM_CONFIG_OVERRIDE="$home/config" \
    FM_SPAWN_NO_GUARD=1 FM_FAKE_PANE_PATH="$pane" TMUX="${TMUX:-fake,1,0}" \
    PATH="$fakebin:$PATH" \
    "$ROOT/bin/fm-spawn.sh" ${attend[@]+"${attend[@]}"} "$@" 2>&1
}

# --- send-world stubs -------------------------------------------------------

# make_stubs <dir>
# Send-world fakebin: send tmux + no-op sleep. Echoes the fakebin path.
# Suites that need recording sleep, herdr, or ssh add those on top of this
# fakebin (or replace sleep via fm_test_fake_sleep_log).
make_stubs() {
  local dir=$1 fakebin
  fakebin=$(fm_fakebin "$dir")
  fm_test_fake_tmux_send "$fakebin"
  fm_test_fake_sleep_noop "$fakebin"
  printf '%s\n' "$fakebin"
}
