#!/usr/bin/env bash
# tests/mcao-setup.test.sh - bin/mcao setup: questions, deferral, re-entry, dispatch rules,
# --answers, --show, --check, the weekly reminder, and the hve plugin install.
#
# Every case runs the real bin/mcao-setup.sh in a cleared environment (env -i) against a throwaway
# home, with only the utilities it needs plus fake harness and backend binaries on PATH, so the
# developer's own claude, codex and tmux never leak into a case.
set -u

# Clear inherited operational-home path variables, before any bin/ script runs.
# Tests must be isolated and not read from or write to the caller's firstmate home. DPES-698.
for name in FM_HOME FM_BACKEND FM_ROOT_OVERRIDE FM_STATE_OVERRIDE FM_DATA_OVERRIDE FM_PROJECTS_OVERRIDE FM_CONFIG_OVERRIDE FM_CREW_STATE_META_OVERRIDE FM_CREW_STATE_STATUS_OVERRIDE FM_PENDING_REPLY_DIR_OVERRIDE FM_PROC_ROOT_OVERRIDE FM_TIMEOUT_MECHANISM_OVERRIDE; do
  unset "$name"
done


# shellcheck source=tests/fixtures.sh
. "$(dirname "${BASH_SOURCE[0]}")/fixtures.sh"

SETUP="$ROOT/bin/mcao-setup.sh"
TMP_ROOT=$(fm_test_tmproot mcao-setup)

# make_toolbin <dir>: symlinks to the real utilities the script uses, nothing else.
make_toolbin() {
  local dir=$1 t path
  mkdir -p "$dir"
  for t in awk bash cat cp cut date dirname env grep head jq ls mkdir mktemp mv perl rm sed sort tail tr wc; do
    path=$(command -v "$t") || fail "test host is missing $t"
    ln -sf "$path" "$dir/$t"
  done
}

# make_case <name> <fake-binary>...: a home, a tool dir and a fake dir holding the named fakes.
CASE=""
make_case() {
  local name=$1 t
  shift
  CASE="$TMP_ROOT/$name"
  mkdir -p "$CASE/home" "$CASE/fake"
  make_toolbin "$CASE/tools"
  for t in "$@"; do
    printf '#!/bin/sh\nexit 0\n' >"$CASE/fake/$t"
    chmod +x "$CASE/fake/$t"
  done
}

CFG() { printf '%s' "$CASE/home/config"; }

# run_setup <stdin-text> <args...>: sets OUT and CODE. Stdin is a file, so it is never a terminal;
# MCAO_SETUP_ASSUME_TTY=1 stands in for one unless the case overrides it.
OUT=""
CODE=0
run_setup() {
  local input=$1
  shift
  printf '%s' "$input" >"$CASE/stdin"
  OUT=$(env -i HOME="$CASE" PATH="$CASE/fake:$CASE/tools" FM_HOME="$CASE/home" \
    MCAO_SETUP_ASSUME_TTY="${ASSUME_TTY:-1}" ${MCAO_SETUP_NOW:+MCAO_SETUP_NOW="$MCAO_SETUP_NOW"} \
    bash "$SETUP" "$@" <"$CASE/stdin" 2>&1)
  CODE=$?
}

state_has() { grep -qx -- "$1" "$(CFG)/setup-state"; }

# ---- first run ----------------------------------------------------------------------------------

test_first_run_writes_every_answer() {
  make_case first claude tmux
  # name, prefix, orchestrator, worker harness, model, effort, worker limit, backend, permissions, dispatch gate
  run_setup $'Josh\nfeature\n\n\n\n\n\n\nauto\n\n'
  expect_code 0 "$CODE" "a complete first run should succeed: $OUT"
  assert_equals Josh "$(cat "$(CFG)/developer-name")" "name"
  assert_equals "feature/" "$(cat "$(CFG)/branch-prefix")" "a prefix without a trailing slash gains one"
  assert_equals claude "$(cat "$(CFG)/orchestrator-harness")" "orchestrator defaults to the installed harness"
  assert_equals claude "$(cat "$(CFG)/crew-harness")" "crew-harness stays a bare adapter name"
  assert_equals tmux "$(cat "$(CFG)/backend")" "backend"
  assert_equals auto "$(cat "$(CFG)/claude-permission-mode")" "permission posture"
  assert_equals claude-haiku-5-5 "$(jq -r .default.model "$(CFG)/crew-dispatch.json")" "worker model lands in the default profile"
  assert_equals medium "$(jq -r .default.effort "$(CFG)/crew-dispatch.json")" "worker effort lands in the default profile"
  assert_equals "schema_version=1" "$(head -n 1 "$(CFG)/setup-state")" "state file starts with its schema version"
  state_has "identity.name answered" || fail "identity.name should be recorded"
  state_has "dispatch.configured answered" || fail "the dispatch gate answer should be recorded"
  assert_contains "$OUT" "Both are on by default" "the closing screen explains the safeguards"
  assert_contains "$OUT" "mcao setup <section>" "the closing screen explains re-running a section"
  pass "a first run writes every config file and records every answer"
}

# ---- interruption, deferral, re-entry -----------------------------------------------------------

test_interrupted_run_keeps_answers() {
  make_case interrupt claude tmux
  run_setup $'Josh\nmao/\n'
  expect_code 1 "$CODE" "input ending mid-run should stop the run"
  assert_contains "$OUT" "Answers given so far are saved" "an ended input says the answers are saved"
  assert_equals Josh "$(cat "$(CFG)/developer-name")" "the first answer survives"
  state_has "identity.branch-prefix answered" || fail "the second answer survives"
  ! state_has "harness.orchestrator answered" || fail "the unanswered question must not be recorded"
  run_setup $'\n\n\n\n\n\n\n\n'
  expect_code 0 "$CODE" "the second run should finish: $OUT"
  assert_not_contains "$OUT" "What should I call you?" "answered questions are not asked again"
  assert_contains "$OUT" "Orchestrator harness" "the interrupted question is asked"
  pass "an interrupted run keeps its answers and the next run asks only what is left"
}

test_deferral_writes_no_file() {
  make_case defer claude tmux
  run_setup $'Josh\nPrefer not to decide right now\n\n\n\n\n\n\n\n\n'
  expect_code 0 "$CODE" "deferring one question should not stop the run: $OUT"
  state_has "identity.branch-prefix deferred" || fail "the deferral is recorded"
  assert_absent "$(CFG)/branch-prefix" "a deferred answer writes no config file"
  assert_equals Josh "$(cat "$(CFG)/developer-name")" "other answers are unaffected"
  pass "a deferred answer is recorded and writes no config file"
}

test_section_rerun_asks_only_that_section() {
  make_case section claude tmux
  run_setup $'Josh\n\n\n\n\n\n\n\n\n\n'
  expect_code 0 "$CODE" "setup run: $OUT"
  run_setup $'codex\n\n\n\n' harness
  assert_not_contains "$OUT" "What should I call you?" "a section run skips other sections"
  assert_not_contains "$OUT" "Session backend" "a section run skips other sections"
  assert_contains "$OUT" "Orchestrator harness" "the section's own questions are asked"
  pass "mcao setup <section> re-asks only that section"
}

test_no_harness_installed_is_an_error() {
  make_case noharness tmux
  run_setup $'Josh\nmao/\n'
  expect_code 1 "$CODE" "no orchestrator harness should stop setup"
  assert_contains "$OUT" "No supported orchestrator harness installed. Please install Claude Code, Codex, or Oh My Pi (omp)." "actionable message"
  pass "setup stops with an actionable error when no orchestrator harness is installed"
}

test_omp_orchestrator() {
  make_case omp omp tmux
  printf 'identity.name=Josh\nidentity.branch-prefix=mao/\nharness.orchestrator=omp\nharness.worker-harness=claude\nbackend=tmux\npermissions.claude-permission-mode=bypass\nharness.worker-model=\nharness.worker-effort=\n' >"$CASE/ans"
  run_setup "" --answers "$CASE/ans"
  expect_code 0 "$CODE" "omp is accepted as the orchestrator when installed: $OUT"
  assert_equals omp "$(cat "$(CFG)/orchestrator-harness")" "omp orchestrator is saved"
  make_case ompmissing claude tmux
  printf 'harness.orchestrator=omp\n' >"$CASE/ans"
  run_setup "" --answers "$CASE/ans"
  expect_code 0 "$CODE" "validation errors are reported, not fatal: $OUT"
  assert_contains "$OUT" "harness.orchestrator invalid value: omp. Allowed: claude, codex, omp (installed: claude)" "omp is refused when it is not installed"
  assert_absent "$(CFG)/orchestrator-harness" "the refused value is not written"
  pass "omp is a supported orchestrator"
}

test_omp_interactive_default_and_worker_default() {
  make_case ompdefault omp tmux
  run_setup $'Josh\n\n\n\nanthropic\n\n\n\n\n\n\n\n'
  expect_code 0 "$CODE" "interactive setup with only omp installed: $OUT"
  assert_equals omp "$(cat "$(CFG)/orchestrator-harness")" "the only installed orchestrator is the default"
  assert_equals omp "$(cat "$(CFG)/crew-harness")" "the worker defaults to the orchestrator's own harness"
  pass "omp is the interactive orchestrator and worker default when it is the only harness installed"
}

test_non_tty_gate() {
  make_case nontty claude tmux
  ASSUME_TTY=0 run_setup ""
  expect_code 1 "$CODE" "a non-terminal run with unanswered questions should fail"
  assert_contains "$OUT" "Unanswered setup questions remain" "message"
  assert_contains "$OUT" "mcao setup --answers <file>" "message names the answers file"
  assert_absent "$(CFG)/developer-name" "no default is applied silently"
  printf 'identity.name=Josh\nidentity.branch-prefix=mao/\nharness.orchestrator=claude\nharness.worker-harness=claude\nbackend=tmux\npermissions.claude-permission-mode=bypass\nharness.worker-model=\nharness.worker-effort=\n' >"$CASE/ans"
  run_setup "" --answers "$CASE/ans"
  expect_code 0 "$CODE" "answers load: $OUT"
  ASSUME_TTY=0 run_setup ""
  expect_code 0 "$CODE" "a non-terminal run with everything answered should succeed: $OUT"
  pass "a non-terminal run fails while questions remain and succeeds once they are answered"
}

# ---- worker harness, model, effort --------------------------------------------------------------

test_effort_fallback_to_medium() {
  make_case effort claude tmux
  run_setup $'Josh\nmao/\n\n\n\nturbo\n\n\n\n\n'
  expect_code 0 "$CODE" "an unrecognized effort should warn, not fail: $OUT"
  assert_contains "$OUT" "Warning: effort 'turbo' is not accepted by claude" "the warning names the harness"
  assert_equals medium "$(jq -r .default.effort "$(CFG)/crew-dispatch.json")" "the effort falls back to medium"
  pass "an unrecognized effort warns and falls back to medium"
}

test_multi_provider_worker_asks_provider() {
  make_case provider claude tmux pi
  run_setup $'Josh\nmao/\n\npi\nanthropic\nclaude-sonnet-5-5\nhigh\n\n\n\n\n'
  expect_code 0 "$CODE" "a multi-provider worker should succeed: $OUT"
  assert_contains "$OUT" "Provider for pi" "the provider is asked for a multi-provider harness"
  assert_equals anthropic "$(jq -r .default.provider "$(CFG)/crew-dispatch.json")" "provider lands in the default profile"
  assert_equals pi "$(cat "$(CFG)/crew-harness")" "crew-harness"
  pass "a multi-provider worker harness is asked for its provider"
}

test_explicit_worker_model_is_kept() {
  make_case explicitmodel claude tmux
  run_setup $'Josh\nfeature\n\n\nclaude-sonnet-5-5\n\n\n\nauto\n\n'
  expect_code 0 "$CODE" "a first run with an explicit worker model should succeed: $OUT"
  assert_equals claude-sonnet-5-5 "$(jq -r .default.model "$(CFG)/crew-dispatch.json")" "an explicit worker model is not replaced by the default"
  run_setup $'\n\n\n\n' harness
  expect_code 0 "$CODE" "re-running the harness section should succeed: $OUT"
  assert_equals claude-sonnet-5-5 "$(jq -r .default.model "$(CFG)/crew-dispatch.json")" "accepting the default on a re-run keeps the explicit worker model"
  pass "an explicitly chosen worker model survives a first run and a harness re-run"
}

# ---- worker limit -------------------------------------------------------------------------------

test_max_workers_default_is_six() {
  make_case maxdefault claude tmux
  run_setup "${BASE}n"$'\n'
  expect_code 0 "$CODE" "$OUT"
  assert_contains "$OUT" "recommended: 6" "the prompt shows the recommendation"
  assert_equals 6 "$(cat "$(CFG)/max-workers")" "pressing Enter saves the default"
  state_has "workers.max answered" || fail "the answer should be recorded"
  run_setup "" --show
  printf '%s\n' "$OUT" | grep -Eq '^workers\.max[[:space:]]+6[[:space:]]+answered \(saved; not yet applied\)$' ||
    fail "--show labels the saved limit as not yet applied: $OUT"
  pass "the worker limit defaults to 6, is saved on Enter, and --show labels it not yet applied"
}

test_max_workers_saves_a_valid_limit() {
  make_case maxvalid claude tmux
  run_setup $'Josh\nmao/\n\n\n\n\n999999\n\nbypass\nn\n\n'
  expect_code 0 "$CODE" "$OUT"
  assert_equals 999999 "$(cat "$(CFG)/max-workers")" "the largest six-digit limit is saved"
  pass "a whole-number worker limit of up to six digits is saved to config/max-workers"
}

test_max_workers_drops_leading_zeros() {
  make_case maxzeros claude tmux
  run_setup $'Josh\nmao/\n\n\n\n\n007\n\nbypass\nn\n\n'
  expect_code 0 "$CODE" "$OUT"
  assert_equals 7 "$(cat "$(CFG)/max-workers")" "a leading zero is dropped on save"
  pass "a worker limit with leading zeros is saved without them"
}

test_max_workers_refuses_invalid_values_and_asks_again() {
  make_case maxinvalid claude tmux
  run_setup $'Josh\nmao/\n\n\n\n\n0\nabc\n-3\n1234567\n8\n\nbypass\nn\n\n'
  expect_code 0 "$CODE" "$OUT"
  assert_contains "$OUT" "'0' is not a worker limit" "zero is refused"
  assert_contains "$OUT" "'abc' is not a worker limit" "a word is refused"
  assert_contains "$OUT" "'-3' is not a worker limit" "a negative number is refused"
  assert_contains "$OUT" "'1234567' is not a worker limit" "more than six digits is refused"
  assert_equals 8 "$(cat "$(CFG)/max-workers")" "the question is asked again until a valid limit arrives"
  pass "invalid worker limits are refused with the allowed range and asked again"
}

test_max_workers_deferral_writes_no_file() {
  make_case maxdefer claude tmux
  run_setup $'Josh\nmao/\n\n\n\n\nPrefer not to decide right now\n\nbypass\nn\n\n'
  expect_code 0 "$CODE" "$OUT"
  state_has "workers.max deferred" || fail "the deferral should be recorded"
  assert_absent "$(CFG)/max-workers" "a deferred worker limit writes no config file"
  pass "deferring the worker limit records the deferral and writes no file"
}

test_max_workers_answers_file() {
  make_case maxanswers claude tmux
  printf '%s\n' 'workers.max=012' >"$CASE/ans"
  run_setup "" --answers "$CASE/ans"
  expect_code 0 "$CODE" "$OUT"
  assert_equals 12 "$(cat "$(CFG)/max-workers")" "an answers-file limit is saved without leading zeros"
  printf '%s\n' 'workers.max=deferred' >"$CASE/ans"
  run_setup "" --answers "$CASE/ans"
  expect_code 0 "$CODE" "$OUT"
  state_has "workers.max deferred" || fail "an answers-file deferral should be recorded"
  assert_absent "$(CFG)/max-workers" "an answers-file deferral removes the saved limit"
  make_case maxanswersbad claude tmux
  printf '%s\n' 'workers.max=0' 'workers.max=abc' 'workers.max=1234567' >"$CASE/ans"
  run_setup "" --answers "$CASE/ans"
  expect_code 0 "$CODE" "invalid lines are reported, not fatal: $OUT"
  assert_contains "$OUT" "workers.max invalid value: 0. Allowed: a whole number of at least 1, up to six digits (recommended 6)." "zero is refused with the allowed range"
  assert_contains "$OUT" "workers.max invalid value: abc." "a word is refused"
  assert_contains "$OUT" "3 validation errors" "each refused line is counted"
  assert_absent "$(CFG)/max-workers" "a refused limit is not written"
  pass "--answers saves, defers, and refuses worker limits the same way the questions do"
}

test_workers_section_asks_only_the_limit() {
  make_case maxsection claude tmux
  run_setup $'15\n' workers
  expect_code 0 "$CODE" "$OUT"
  assert_equals 15 "$(cat "$(CFG)/max-workers")" "the section saves the typed limit"
  assert_contains "$OUT" "Worker limit saved." "the section confirms the save"
  assert_not_contains "$OUT" "What should I call you?" "a section run skips the identity questions"
  assert_not_contains "$OUT" "Session backend" "a section run skips the backend question"
  pass "mcao setup workers re-asks only the worker limit"
}

# ---- dispatch -----------------------------------------------------------------------------------

# Answers for the full flow up to and including the worker, backend and permission questions.
BASE=$'Josh\nmao/\n\n\n\n\n\n\nbypass\n'

test_dispatch_gate_no_writes_only_default() {
  make_case gateno claude tmux
  run_setup "${BASE}n"$'\n'
  expect_code 0 "$CODE" "$OUT"
  assert_equals 0 "$(jq '.rules // [] | length' "$(CFG)/crew-dispatch.json")" "no rules"
  assert_equals claude "$(jq -r .default.harness "$(CFG)/crew-dispatch.json")" "default profile only"
  pass "declining the dispatch gate writes only the default profile"
}

test_dispatch_rules() {
  make_case rules claude tmux
  # gate yes; debugging: harness, model, effort override to low; the other seven classes all Enter;
  # then one custom class that changes nothing, one that changes effort.
  local debugging=$'\n\nlow\n' rest=""
  rest=$'\n\n\n\n\n\n\n\n\n\n\n\n\n\n\n\n\n\n\n\n\n'

  run_setup "${BASE}y"$'\n'"$debugging$rest"$'y\nurgent-bugs\nFix production bugs fast\n\n\n\nn\n'
  expect_code 0 "$CODE" "dispatch flow: $OUT"
  assert_equals 1 "$(jq '.rules | length' "$(CFG)/crew-dispatch.json")" "only the class that differs from the default is written, got: $(jq -c . "$(CFG)/crew-dispatch.json")"
  assert_equals "mao setup: debugging" "$(jq -r '.rules[0].why' "$(CFG)/crew-dispatch.json")" "rule owner marker"
  assert_equals low "$(jq -r '.rules[0].use.effort' "$(CFG)/crew-dispatch.json")" "override applied"
  jq -e . "$(CFG)/crew-dispatch.json" >/dev/null || fail "the written file must be valid JSON"
  pass "dispatch writes a rule only for classes that differ from the default"
}

test_dispatch_skip_and_foreign_rules_preserved() {
  make_case foreign claude tmux
  mkdir -p "$(CFG)"
  printf '%s\n' '{"rules":[{"when":"my own rule","use":{"harness":"codex"}}],"default":{"harness":"claude"}}' >"$(CFG)/crew-dispatch.json"
  run_setup "${BASE}y"$'\n'"skip"$'\n'$'\n\n\n\n\n\n\n\n\n\n\n\n\n\n\n\n\n\n\n\n\nn\n'
  expect_code 0 "$CODE" "dispatch flow: $OUT"
  assert_equals "my own rule" "$(jq -r '.rules[0].when' "$(CFG)/crew-dispatch.json")" "a rule setup did not write is kept"
  assert_equals 1 "$(jq '.rules | length' "$(CFG)/crew-dispatch.json")" "skip writes no rule"
  pass "skip leaves a class on the default and rules written by hand are preserved"
}

# ---- --answers, --show --------------------------------------------------------------------------

test_answers_file() {
  make_case answers claude tmux
  printf '%s\n' '# a comment' '' 'identity.name=Josh' 'bogus=1' 'backend=orca' 'harness.worker-effort=turbo' \
    'harness.worker-harness=claude' 'identity.branch-prefix=deferred' >"$CASE/ans"
  run_setup "" --answers "$CASE/ans"
  expect_code 0 "$CODE" "validation errors are reported, not fatal: $OUT"
  assert_contains "$OUT" "Unknown key: bogus" "unknown key"
  assert_contains "$OUT" "backend invalid value: orca. Allowed: tmux, herdr." "invalid value names the allowed set"
  assert_contains "$OUT" "harness.worker-effort invalid value: turbo" "invalid effort"
  assert_contains "$OUT" "3 validation errors" "summary counts errors"
  assert_equals Josh "$(cat "$(CFG)/developer-name")" "valid line applied"
  assert_absent "$(CFG)/backend" "an invalid value is not written"
  state_has "identity.branch-prefix deferred" || fail "explicit deferral recorded"
  assert_absent "$(CFG)/branch-prefix" "deferral writes no file"
  run_setup "" --answers "$CASE/missing"
  expect_code 1 "$CODE" "a missing file is fatal"
  assert_contains "$OUT" "File not found:" "message"
  pass "--answers loads valid lines, reports bad ones, honors deferral and fails on a missing file"
}

test_answers_never_overwrite_a_hand_written_default() {
  make_case handdefault claude tmux
  mkdir -p "$(CFG)"
  printf '%s\n' '{"default":[{"harness":"claude"},{"harness":"codex"}]}' >"$(CFG)/crew-dispatch.json"
  cp "$(CFG)/crew-dispatch.json" "$CASE/before.json"
  printf 'harness.worker-harness=claude\n' >"$CASE/ans"
  run_setup "" --answers "$CASE/ans"
  expect_code 0 "$CODE" "$OUT"
  assert_contains "$OUT" "hand-written default list" "the refusal is explained"
  cmp -s "$CASE/before.json" "$(CFG)/crew-dispatch.json" || fail "a hand-written default list must be left untouched"
  assert_absent "$(CFG)/crew-harness" "nothing is written for the refused answer"
  pass "--answers leaves a hand-written default list untouched"
}

test_show_roundtrip() {
  make_case show claude tmux
  run_setup "${BASE}n"$'\n'
  expect_code 0 "$CODE" "$OUT"
  run_setup "" --show
  expect_code 0 "$CODE" "$OUT"
  assert_contains "$OUT" "identity.name" "table lists the keys"
  assert_contains "$OUT" "saved; not yet applied" "settings nothing reads yet are labelled"
  run_setup "" --show --format answers
  printf '%s\n' "$OUT" >"$CASE/ans"
  assert_contains "$OUT" "backend=tmux" "answers format"
  assert_contains "$OUT" "workers.max=6" "answers format carries the worker limit"
  cp -R "$(CFG)" "$CASE/before"
  run_setup "" --answers "$CASE/ans"
  expect_code 0 "$CODE" "$OUT"
  for f in developer-name branch-prefix orchestrator-harness crew-harness max-workers backend claude-permission-mode crew-dispatch.json; do
    cmp -s "$CASE/before/$f" "$(CFG)/$f" || fail "round trip changed config/$f"
  done
  pass "--show prints the table and its answers format reloads to the same config"
}

# ---- hve plugin -----------------------------------------------------------------------------------

# make_plugin_fake <name>: a fake claude, copilot or codex whose plugin commands record each call in
# $CASE/calls and keep a small store under $CASE, laid out the way the real CLIs keep theirs: claude
# and copilot write enabledPlugins into their settings.json, codex lists the installed plugin root.
# FAKE_FAIL=<name> makes that fake's install step fail.
make_plugin_fake() {
  local name=$1
  cat >"$CASE/fake/$name" <<'SH'
#!/bin/bash
me=${0##*/}
printf '%s %s\n' "$me" "$*" >>"$CASE_DIR/calls"
case $me in
  claude) store=${CLAUDE_CONFIG_DIR:-$HOME/.claude} ;;
  copilot) store=${COPILOT_HOME:-$HOME/.copilot} ;;
  codex) store=${CODEX_HOME:-$HOME/.codex} ;;
esac
mkdir -p "$store"
case "$me $*" in
  "copilot plugin marketplace list --json")
    if [ -f "$store/mkt" ]; then printf '[{"name":"mayo-skills"}]\n'; else printf '[]\n'; fi ;;
  "copilot plugin marketplace add "*)
    [ ! -f "$store/mkt" ] || { echo 'Marketplace "mayo-skills" already registered' >&2; exit 1; }
    : >"$store/mkt" ;;
  *" plugin marketplace add "*) : >"$store/mkt" ;;
  "claude plugin install hve@mayo-skills" | "copilot plugin install hve@mayo-skills" | "codex plugin add hve@mayo-skills")
    [ "${FAKE_FAIL:-}" != "$me" ] || { echo "Error: authentication required for mc-innersource/mayo-skills" >&2; exit 1; }
    [ -f "$store/mkt" ] || { echo 'Marketplace "mayo-skills" not found' >&2; exit 1; }
    # The version record each real CLI keeps, at the path bin/fm-workflow-agent-lib.sh reads, holding a
    # version inside the supported range (>=0.2.0 <0.3.0). Without it setup reports the version unreadable.
    case $me in
      claude) mkdir -p "$store/plugins" && printf '%s\n' '{"version":2,"plugins":{"hve@mayo-skills":[{"scope":"user","version":"0.2.0"}]}}' >"$store/plugins/installed_plugins.json" ;;
      copilot) mkdir -p "$store/installed-plugins/mayo-skills/hve/.claude-plugin" && printf '%s\n' '{"name":"hve","version":"0.2.0"}' >"$store/installed-plugins/mayo-skills/hve/.claude-plugin/plugin.json" ;;
      codex) mkdir -p "$store/plugins/cache/mayo-skills/hve/0.2.0/.claude-plugin" && printf '%s\n' '{"name":"hve","version":"0.2.0"}' >"$store/plugins/cache/mayo-skills/hve/0.2.0/.claude-plugin/plugin.json" ;;
    esac
    if [ "$me" = codex ] || [ "$me" = claude ]; then
      mkdir -p "$store/plugin/agents" "$store/plugin/skills/workflow-router"
      printf -- '---\nname: workflow-agent\ndescription: x\n---\n\n# Workflow subagent\n\nRead the workflow-router skill first.\n' >"$store/plugin/agents/workflow-agent.md"
      printf 'router\n' >"$store/plugin/skills/workflow-router/SKILL.md"
    fi
    [ "$me" = codex ] || printf '{"enabledPlugins":{"hve@mayo-skills":true}}\n' >"$store/settings.json" ;;
  "claude plugin list --json")
    if [ -d "$store/plugin" ]; then
      printf '[{"id":"hve@mayo-skills","enabled":true,"installPath":"%s"}]\n' "$store/plugin"
    else
      printf '[]\n'
    fi ;;
  "codex plugin list --json")
    if [ -d "$store/plugin" ]; then
      printf '{"installed":[{"pluginId":"hve@mayo-skills","enabled":true,"source":{"path":"%s"}}]}\n' "$store/plugin"
    else
      printf '{"installed":[]}\n'
    fi ;;
esac
exit 0
SH
  chmod +x "$CASE/fake/$name"
}

# run_plugins <args...>: like run_setup, with the plugin stores under the case directory.
run_plugins() {
  OUT=$(env -i HOME="$CASE" PATH="$CASE/fake:$CASE/tools" FM_HOME="$CASE/home" CASE_DIR="$CASE" \
    ${FAKE_FAIL:+FAKE_FAIL="$FAKE_FAIL"} ${MCAO_SETUP_HVE_SOURCE:+MCAO_SETUP_HVE_SOURCE="$MCAO_SETUP_HVE_SOURCE"} \
    MCAO_SETUP_ASSUME_TTY=1 bash "$SETUP" "$@" </dev/null 2>&1)
  CODE=$?
}

calls() { cat "$CASE/calls" 2>/dev/null; }

test_plugins_installs_into_every_installed_harness() {
  make_case plugins tmux
  make_plugin_fake claude
  make_plugin_fake copilot
  make_plugin_fake codex
  run_plugins plugins
  expect_code 0 "$CODE" "installing into every installed harness should succeed: $OUT"
  assert_contains "$OUT" "claude: ready" "claude reported ready"
  assert_contains "$OUT" "copilot: ready" "copilot reported ready"
  assert_contains "$OUT" "codex: ready" "codex reported ready"
  assert_contains "$(calls)" "claude plugin marketplace add mc-innersource/mayo-skills" "claude adds the Mayo marketplace"
  assert_contains "$(calls)" "claude plugin install hve@mayo-skills" "claude installs the plugin"
  assert_contains "$(calls)" "copilot plugin install hve@mayo-skills" "copilot installs the plugin"
  assert_contains "$(calls)" "codex plugin add hve@mayo-skills" "codex installs the plugin"
  assert_contains "$(cat "$CASE/.codex/workflow-agent.config.toml")" 'developer_instructions = "\n# Workflow subagent\n\nRead the workflow-router skill first."' \
    "the codex profile carries the agent body without its front matter"
  pass "mcao setup plugins installs hve into every installed harness and reports each ready"
}

test_plugins_copy_agent_and_skills_for_pi_and_omp() {
  make_case plugins-copy tmux pi omp
  make_plugin_fake claude
  run_plugins plugins
  expect_code 0 "$CODE" "pi and omp copy from the claude install: $OUT"
  assert_contains "$OUT" "pi: ready" "pi reported ready"
  assert_contains "$OUT" "omp: ready" "omp reported ready"
  assert_equals $'\n# Workflow subagent\n\nRead the workflow-router skill first.' "$(cat "$CASE/.pi/agent/mao-workflow-agent/workflow-agent.md")" \
    "the pi prompt is the agent body without its front matter"
  [ -f "$CASE/.pi/agent/mao-workflow-agent/skills/workflow-router/SKILL.md" ] || fail "pi skills were not copied"
  assert_contains "$(cat "$CASE/.omp/agent/mao-workflow-agent/skills.yml")" "$CASE/.omp/agent/mao-workflow-agent/skills" "omp overlay names its skills folder"
  [ -f "$CASE/.omp/agent/mao-workflow-agent/skills/workflow-router/SKILL.md" ] || fail "omp skills were not copied"
  assert_absent "$CASE/.pi/agent/agents/workflow-agent.md" "nothing is placed in a Pi auto-discovery folder"
  assert_absent "$CASE/.omp/agent/agents" "nothing is placed in an omp auto-discovery folder"
  run_plugins plugins
  expect_code 0 "$CODE" "a rerun swaps the folder in again: $OUT"
  assert_equals 0 "$(find "$CASE/.pi/agent" -name '*.tmp.*' | wc -l | tr -d ' ')" "no temporary folder is left behind"
  pass "mcao setup plugins copies the workflow agent and skills for pi and omp, idempotently"
}

test_plugins_copy_follows_the_pi_account_pin() {
  make_case plugins-pipin tmux pi
  make_plugin_fake codex
  mkdir -p "$CASE/pinned-pi" "$CASE/home/config"
  printf '%s\nopenai-codex\n' "$CASE/pinned-pi" >"$CASE/home/config/pi-account"
  run_plugins plugins
  expect_code 0 "$CODE" "pi copies under the pinned root from the codex install: $OUT"
  [ -f "$CASE/pinned-pi/mao-workflow-agent/workflow-agent.md" ] || fail "the agent was not placed under the pinned Pi root"
  assert_absent "$CASE/.pi/agent/mao-workflow-agent" "the default Pi root is untouched"
  pass "the Pi copy follows config/pi-account"
}

test_plugins_copy_needs_an_installed_hve_plugin() {
  make_case plugins-nosrc tmux pi
  run_plugins plugins
  expect_code 1 "$CODE" "pi cannot be made ready without a claude or codex hve install"
  assert_contains "$OUT" "no enabled hve@mayo-skills plugin found for claude or codex" "the missing source is named"
  assert_contains "$OUT" "pi: not ready" "pi is reported not ready"
  pass "pi and omp are reported not ready when no hve plugin exists to copy"
}

test_omp_and_pi_are_valid_worker_answers() {
  make_case worker-omp claude tmux
  printf 'harness.worker-harness=omp\n' >"$CASE/ans"
  run_setup "" --answers "$CASE/ans"
  expect_code 0 "$CODE" "answers load: $OUT"
  assert_equals omp "$(cat "$(CFG)/crew-harness")" "omp is saved as the worker harness"
  pass "omp is a valid worker harness answer"
}

test_plugins_rerun_is_idempotent() {
  make_case plugins-again tmux
  make_plugin_fake copilot
  run_plugins plugins
  expect_code 0 "$CODE" "first run: $OUT"
  run_plugins plugins
  expect_code 0 "$CODE" "a second run should succeed rather than trip on the known marketplace: $OUT"
  assert_equals 1 "$(calls | grep -c 'copilot plugin marketplace add')" "copilot adds the marketplace once"
  assert_contains "$(calls)" "copilot plugin marketplace update mayo-skills" "a known copilot marketplace is refreshed instead"
  pass "a second mcao setup plugins run refreshes rather than re-adding"
}

test_plugins_failure_is_reported_and_check_names_it() {
  make_case plugins-fail tmux
  make_plugin_fake claude
  make_plugin_fake copilot
  FAKE_FAIL=claude run_plugins plugins
  expect_code 1 "$CODE" "a failed install should make the plugins step fail"
  assert_contains "$OUT" "installing hve@mayo-skills failed: Error: authentication required" "the vendor's error is shown"
  assert_contains "$OUT" "claude: not ready - the hve@mayo-skills plugin that provides hve:workflow-agent is not installed and enabled for claude" \
    "the shared check explains what is missing"
  assert_contains "$OUT" "copilot: ready" "one harness failing does not stop the others"
  assert_contains "$OUT" "gh auth setup-git" "the retry hint names the private-repository sign-in"
  assert_contains "$OUT" "then run 'mcao setup plugins'" "the retry hint names the command to rerun"
  OUT=$(env -i HOME="$CASE" PATH="$CASE/fake:$CASE/tools" FM_HOME="$CASE/home" CASE_DIR="$CASE" \
    NO_MISTAKES_TELEMETRY=0 bash "$SETUP" --check 2>&1)
  assert_contains "$OUT" "MISSING: claude workflow agent" "--check names the harness that cannot run the agent"
  assert_not_contains "$OUT" "MISSING: copilot workflow agent" "--check stays quiet for a ready harness"
  pass "a failed install is reported with the vendor's error, and --check names the harness"
}

test_plugins_leaves_a_hand_written_codex_profile() {
  make_case plugins-codex-own tmux
  make_plugin_fake codex
  mkdir -p "$CASE/.codex"
  printf 'developer_instructions = "mine"\n' >"$CASE/.codex/workflow-agent.config.toml"
  run_plugins plugins
  expect_code 0 "$CODE" "a hand-written profile still counts as ready: $OUT"
  assert_contains "$OUT" "was not written by mcao setup, so it was left as it is" "the skip is explained"
  assert_equals 'developer_instructions = "mine"' "$(cat "$CASE/.codex/workflow-agent.config.toml")" "the hand-written profile is untouched"
  pass "mcao setup plugins never overwrites a codex profile it did not write"
}

test_plugins_follows_the_claude_account_pin() {
  make_case plugins-pin tmux
  make_plugin_fake claude
  mkdir -p "$CASE/pinned" "$CASE/home/config"
  printf '%s\n' "$CASE/pinned" >"$CASE/home/config/claude-account"
  run_plugins plugins
  expect_code 0 "$CODE" "a pinned claude account should install into the pinned root: $OUT"
  assert_equals '{"enabledPlugins":{"hve@mayo-skills":true}}' "$(cat "$CASE/pinned/settings.json")" "the plugin lands in the pinned root"
  assert_absent "$CASE/.claude/settings.json" "the default store is not touched"
  pass "mcao setup plugins installs into the claude root the worker account pin selects"
}

test_plugins_local_source_skips_refresh() {
  make_case plugins-local tmux
  make_plugin_fake codex
  mkdir -p "$CASE/marketplace"
  MCAO_SETUP_HVE_SOURCE="$CASE/marketplace" run_plugins plugins
  expect_code 0 "$CODE" "a local marketplace source should install: $OUT"
  assert_contains "$(calls)" "codex plugin marketplace add $CASE/marketplace" "the local source is added"
  assert_not_contains "$(calls)" "marketplace upgrade" "a local source is not refreshed, because codex refuses to"
  pass "a local marketplace source installs without the refresh step"
}

# copilot keeps no installed-plugins record for a directory marketplace, so its version is read from
# settings.json: the enabled plugin, its directory marketplace, and the manifest that marketplace names.
test_copilot_version_read_from_its_directory_marketplace() {
  make_case copilot-directory
  local mkt="$CASE/mkt" home="$CASE/copilot" rc out
  mkdir -p "$mkt/.claude-plugin" "$mkt/hve/.claude-plugin" "$home"
  printf '%s\n' '{"name":"mayo-skills","owner":{"name":"fixture"},"plugins":[{"name":"hve","source":"./hve","version":"0.2.0"}]}' \
    >"$mkt/.claude-plugin/marketplace.json"
  printf '%s\n' '{"name":"hve","version":"0.2.0"}' >"$mkt/hve/.claude-plugin/plugin.json"
  printf '{"enabledPlugins":{"hve@mayo-skills":true},"extraKnownMarketplaces":{"mayo-skills":{"source":{"source":"directory","path":"%s"}}}}\n' \
    "$mkt" >"$home/settings.json"
  # shellcheck disable=SC2016 # positional params expand in the child shell.
  out=$(env -i PATH="$CASE/tools" COPILOT_HOME="$home" bash -c '. "$1"; fm_workflow_agent_check_version copilot' _ \
    "$ROOT/bin/fm-workflow-agent-lib.sh" 2>&1) && rc=0 || rc=$?
  assert_equals 0 "$rc" "a directory-marketplace copilot install at a supported version passes: $out"

  printf '%s\n' '{"name":"hve","version":"0.3.0"}' >"$mkt/hve/.claude-plugin/plugin.json"
  # shellcheck disable=SC2016 # positional params expand in the child shell.
  out=$(env -i PATH="$CASE/tools" COPILOT_HOME="$home" bash -c '. "$1"; fm_workflow_agent_check_version copilot' _ \
    "$ROOT/bin/fm-workflow-agent-lib.sh" 2>&1) && rc=0 || rc=$?
  assert_equals 1 "$rc" "a directory-marketplace copilot install outside the range is refused"
  assert_contains "$out" "plugin version 0.3.0 for copilot is outside the host's supported range" "the refusal names the version and range"
  pass "copilot's version is read from its directory marketplace and checked against the supported range"
}

test_full_run_installs_plugins_and_says_so() {
  make_case plugins-full tmux
  make_plugin_fake claude
  run_setup $'Josh\nmao/\n\n\n\n\n\n\nbypass\n\n'
  expect_code 0 "$CODE" "a full run with a working claude should succeed: $OUT"
  assert_contains "$OUT" "claude: ready" "the full run installs the plugin"
  assert_contains "$OUT" "mcao setup plugins" "the closing screen names the plugins section"
  pass "a full mcao setup run installs the hve plugin before the closing screen"
}

# ---- --check, --remind --------------------------------------------------------------------------

test_check_never_fails() {
  make_case check claude tmux
  OUT=$(env FM_HOME="$CASE/home" NO_MISTAKES_TELEMETRY= bash "$SETUP" --check 2>&1)
  CODE=$?
  expect_code 0 "$CODE" "--check always exits 0: $OUT"
  assert_contains "$OUT" "export NO_MISTAKES_TELEMETRY=0" "a shell without the telemetry switch gets the one-liner"
  OUT=$(env FM_HOME="$CASE/home" NO_MISTAKES_TELEMETRY=0 bash "$SETUP" --check 2>&1)
  assert_not_contains "$OUT" "NO_MISTAKES_TELEMETRY" "no note when telemetry is off"
  pass "--check reports diagnostics, flags a missing telemetry switch, and exits 0"
}

test_remind() {
  make_case remind claude tmux
  printf 'schema_version=1\nidentity.name deferred\nbackend deferred\n' >/dev/null
  mkdir -p "$(CFG)"
  printf 'schema_version=1\nidentity.name deferred\nbackend deferred\ndeferred_reminded_at=1000\n' >"$(CFG)/setup-state"
  MCAO_SETUP_NOW=$((1000 + 3 * 86400)) run_setup "" --remind
  assert_equals "" "$OUT" "a reminder is not due after 3 days"
  MCAO_SETUP_NOW=$((1000 + 8 * 86400)) run_setup "" --remind
  expect_code 0 "$CODE" "$OUT"
  assert_contains "$OUT" "2 setup choices deferred (identity.name, backend)" "due after 8 days"
  assert_equals $((1000 + 8 * 86400)) "$(sed -n 's/^deferred_reminded_at=//p' "$(CFG)/setup-state")" "timestamp updated"
  MCAO_SETUP_NOW=$((1000 + 9 * 86400)) run_setup "" --remind
  assert_equals "" "$OUT" "printed once per period"
  pass "the deferred-choices reminder prints once a week and updates its timestamp"
}

# ---- smoke test ---------------------------------------------------------------------------------
# The spawn world is real: bin/mcao-setup.sh runs the real bin/fm-brief.sh, bin/fm-spawn.sh and
# bin/fm-teardown.sh in a scratch home. Only the three outside programs are fakes. The fake tmux behaves like
# a pane: `treehouse get` enters a worktree of the pane's start directory, the staged launch runs in it, and
# kill-window ends whatever the launch started. The fake claude does the smoke task, or not, per FAKE_MODE.

make_smoke_case() { # <name>
  local tool
  make_case "$1"
  mkdir -p "$CASE/tmp" "$CASE/state" "$CASE/uh"
  fm_test_seed_workflow_agents "$CASE/uh"
  mkdir -p "$CASE/home/config"
  printf 'claude\n' >"$CASE/home/config/crew-harness"
  printf 'tmux\n' >"$CASE/home/config/backend"
  # Every other harness CLI on the host is shadowed, so setup's plugin install never reaches a real one or the network.
  for tool in treehouse codex copilot omp pi pi-signed grok kimi cursor-agent agy opencode gemini rovo devin muse gh; do
    printf '#!/bin/sh\nexit 0\n' >"$CASE/fake/$tool"
    chmod +x "$CASE/fake/$tool"
  done
  # Every wait is capped at 0.2s, so the spawn's fixed delays do not decide how long a case takes on a busy host.
  cat >"$CASE/fake/sleep" <<'SH'
#!/bin/sh
case "$1" in
  "" | *[!0-9.]*) exec "$FAKE_SLEEP" "$@" ;;
esac
exec "$FAKE_SLEEP" "$(awk -v t="$1" 'BEGIN { print (t > 0.2) ? 0.2 : t }')"
SH
  cat >"$CASE/fake/claude" <<'SH'
#!/bin/sh
# Only the worker launch passes --agent; every plugin command is a no-op.
case "$*" in *--agent*) ;; *) exit 0 ;; esac
find "$TMPDIR" -path '*/state/operational-inbox/*.msg' -exec cat {} + >"$FAKE_STATE/worker-envelope" 2>/dev/null
echo $$ >"$FAKE_STATE/claude.pid"
case "${FAKE_MODE:-commit}" in
  commit) printf 'smoke test\n' >test.txt ;;
  wrong) printf 'something else\n' >test.txt ;;
esac
if [ -f test.txt ]; then
  git add test.txt
  git -c user.name=fake -c user.email=fake@example.invalid commit -q -m 'smoke test'
fi
echo "fake claude worker finished"
exec "$FAKE_SLEEP" 600
SH
  cat >"$CASE/fake/tmux" <<'SH'
#!/bin/bash
S=$FAKE_STATE
printf '%s\n' "$*" >>"$S/calls"
case "$1" in
  list-windows) cat "$S/windows" 2>/dev/null; exit 0 ;;
  new-window)
    while [ $# -gt 0 ]; do
      case $1 in -n) name=$2; shift ;; -c) dir=$2; shift ;; esac
      shift
    done
    printf '%s\n' "$name" >>"$S/windows"
    printf '%s' "$dir" >"$S/start"
    printf '@1\n'
    exit 0
    ;;
  display-message)
    case "$*" in
      *pane_current_path*) cat "$S/pane" 2>/dev/null ;;
      *pane_id*) printf '%%1\n' ;;
      *) printf 'firstmate\n' ;;
    esac
    exit 0
    ;;
  capture-pane) cat "$S/pane.log" 2>/dev/null; exit 0 ;;
  send-keys)
    shift
    literal=""
    for a in "$@"; do
      case $a in
        -t | Enter) ;;
        -l) literal=1 ;;
        *)
          payload=$a
          ;;
      esac
    done
    if [ "$literal" != 1 ] && [ "${payload:-}" = "treehouse get" ]; then
      [ ! -e "$S/no-worktree" ] || exit 0
      git -C "$(cat "$S/start")" worktree add -q --detach "$(cat "$S/start")/.treehouse/fake" >>"$S/pane.log" 2>&1
      (cd "$(cat "$S/start")/.treehouse/fake" && pwd -P) >"$S/pane"
    elif [ "$literal" = 1 ]; then
      staged=${payload#". '"}
      staged=${staged%"'"}
      (cd "$(cat "$S/pane")" && exec bash -c ". '$staged'") >>"$S/pane.log" 2>&1 &
    fi
    exit 0
    ;;
  kill-window)
    [ ! -f "$S/claude.pid" ] || kill "$(cat "$S/claude.pid")" 2>/dev/null
    : >"$S/windows"
    exit 0
    ;;
esac
exit 0
SH
  chmod +x "$CASE/fake/treehouse" "$CASE/fake/claude" "$CASE/fake/tmux" "$CASE/fake/sleep"
}

# run_smoke <stdin> <args...>: sets OUT and CODE. FAKE_MODE and SMOKE_SECONDS come from the case.
run_smoke() {
  local input=$1
  shift
  printf '%s' "$input" >"$CASE/stdin"
  OUT=$(env -i HOME="$CASE/uh" PATH="$CASE/fake:$PATH" TMPDIR="$CASE/tmp" FM_HOME="$CASE/home" \
    FAKE_STATE="$CASE/state" FAKE_MODE="${FAKE_MODE:-commit}" FAKE_SLEEP="$(command -v sleep)" CLAUDE_CONFIG_DIR= \
    MCAO_SETUP_ASSUME_TTY="${ASSUME_TTY:-1}" MCAO_SETUP_SMOKE_SECONDS="${SMOKE_SECONDS:-60}" \
    MCAO_SETUP_SMOKE_SPAWN_SECONDS="${SMOKE_SPAWN_SECONDS:-240}" \
    bash "$SETUP" "$@" <"$CASE/stdin" 2>&1)
  CODE=$?
}

# assert_smoke_clean: no scratch directory, no pane, no worker process, and the teardown really ran.
assert_smoke_clean() {
  local pid
  assert_equals "" "$(ls -A "$CASE/tmp")" "the scratch directory under TMPDIR is removed: $OUT"
  assert_equals "" "$(cat "$CASE/state/windows" 2>/dev/null)" "no worker pane is left: $OUT"
  if [ -f "$CASE/state/claude.pid" ]; then
    pid=$(cat "$CASE/state/claude.pid")
    ! kill -0 "$pid" 2>/dev/null || fail "the worker process $pid is still running"
  fi
  assert_contains "$(cat "$CASE/state/calls" 2>/dev/null)" "kill-window" "cleanup closed the worker pane through teardown"
}

test_smoke_test_passes_and_cleans_up() {
  make_smoke_case smoke-pass
  FAKE_MODE=commit run_smoke "" --smoke-test
  expect_code 0 "$CODE" "a worker that commits test.txt passes the smoke test: $OUT"
  assert_contains "$OUT" "Smoke test passed" "the pass is reported"
  assert_contains "$(cat "$CASE/state/calls")" "new-window" "the worker was spawned through the product spawn"
  assert_smoke_clean
  pass "the smoke test spawns a worker, verifies its commit, and leaves nothing behind"
}

test_smoke_test_hands_the_worker_its_exact_instructions() {
  make_smoke_case smoke-instructions
  FAKE_MODE=commit run_smoke "" --smoke-test
  expect_code 0 "$CODE" "a worker that commits test.txt passes the smoke test: $OUT"
  assert_contains "$(cat "$CASE/state/worker-envelope")" "Run: printf 'smoke test\\n' > test.txt && git add test.txt && git commit -m 'smoke test'. Nothing else is needed." "the worker's brief carries the spec with its && operators intact"
  assert_not_contains "$(cat "$CASE/state/worker-envelope")" "{FIRSTMATE_SPEC}" "the spec placeholder is filled, not echoed"
  pass "the smoke test hands the worker the exact spec text it was written with"
}

test_smoke_test_reports_a_refused_spawn() {
  make_smoke_case smoke-refused
  rm -f "$CASE/uh/.claude/settings.json"
  run_smoke "" --smoke-test
  expect_code 1 "$CODE" "a spawn the product refuses fails the smoke test: $OUT"
  assert_contains "$OUT" "Smoke test failed: the worker did not launch" "the failure names the stage"
  assert_not_contains "$OUT" "Smoke test passed" "a refused spawn never passes"
  assert_equals "" "$(ls -A "$CASE/tmp")" "the scratch directory is removed after a refused spawn"
  assert_equals "" "$(cat "$CASE/state/windows" 2>/dev/null)" "a refused spawn leaves no pane"
  pass "a refused spawn fails the smoke test with the spawn output and cleans up"
}

test_smoke_test_times_out_when_the_worker_never_commits() {
  make_smoke_case smoke-timeout
  FAKE_MODE=idle SMOKE_SECONDS=5 run_smoke "" --smoke-test
  expect_code 1 "$CODE" "a worker that never commits fails the smoke test: $OUT"
  assert_contains "$OUT" "Smoke test timed out after 5 seconds: " "the bound is reported from the wait"
  assert_contains "$OUT" "test.txt does not exist" "the last verification reason is reported"
  assert_contains "$OUT" "fake claude worker finished" "the failure shows the worker pane"
  assert_smoke_clean
  pass "a worker that never commits times out at the bound, shows its pane, and is torn down"
}

test_smoke_test_bounds_the_spawn_itself() {
  make_smoke_case smoke-slow-spawn
  # A pane whose treehouse never enters a worktree: spawn waits for it, so only the bound can end the spawn.
  : >"$CASE/state/no-worktree"
  # The bound must outlast the spawn's pane-opening step on a busy host; the product sizes that step at tens of
  # seconds (bin/mcao-setup.sh SMOKE_SPAWN_SECONDS). Too short a bound ends the spawn before its pane opens.
  FAKE_MODE=idle SMOKE_SPAWN_SECONDS=30 run_smoke "" --smoke-test
  expect_code 1 "$CODE" "a spawn that outlasts the bound fails the smoke test: $OUT"
  assert_contains "$OUT" "Smoke test timed out after 30 seconds while launching the worker" "the spawn bound is reported"
  assert_equals "" "$(ls -A "$CASE/tmp")" "the scratch directory is removed after a spawn that ran out of time"
  assert_equals "" "$(cat "$CASE/state/windows" 2>/dev/null)" "no pane is left behind: $OUT"
  # The spawn was cut off before it recorded the task, so only the pane-by-name fallback can have closed its pane.
  assert_contains "$(cat "$CASE/state/calls")" "new-window" "the spawn opened its pane before the bound hit"
  assert_contains "$(cat "$CASE/state/calls")" "kill-window" "the pane no task record names was closed"
  pass "the bound also ends a spawn that is still launching, and nothing is left behind"
}

test_smoke_test_rejects_wrong_content() {
  make_smoke_case smoke-wrong
  FAKE_MODE=wrong SMOKE_SECONDS=5 run_smoke "" --smoke-test
  expect_code 1 "$CODE" "a worker that writes the wrong content fails: $OUT"
  assert_contains "$OUT" "test.txt does not contain 'smoke test'" "the content check names the problem"
  assert_smoke_clean
  pass "a worker that commits the wrong content fails verification and is torn down"
}

test_smoke_test_cleans_up_when_killed() {
  local pid child
  make_smoke_case smoke-killed
  printf '' >"$CASE/stdin"
  env -i HOME="$CASE/uh" PATH="$CASE/fake:$PATH" TMPDIR="$CASE/tmp" FM_HOME="$CASE/home" \
    FAKE_STATE="$CASE/state" FAKE_MODE=idle FAKE_SLEEP="$(command -v sleep)" CLAUDE_CONFIG_DIR= MCAO_SETUP_SMOKE_SECONDS=120 MCAO_SETUP_SMOKE_SPAWN_SECONDS=240 \
    bash "$SETUP" --smoke-test <"$CASE/stdin" >"$CASE/out" 2>&1 &
  pid=$!
  for _ in $(seq 1 60); do
    [ -f "$CASE/state/claude.pid" ] && break
    sleep 0.5
  done
  [ -f "$CASE/state/claude.pid" ] || { kill "$pid" 2>/dev/null; fail "the worker never started: $(cat "$CASE/out")"; }
  # A terminal's Ctrl-C reaches the whole foreground group; here the smoke test's own subshell is signalled.
  child=$(pgrep -P "$pid" | head -n 1)
  [ -n "$child" ] || fail "the smoke test subshell is not running"
  kill -TERM "$child"
  wait "$pid"
  CODE=$?
  OUT=$(cat "$CASE/out")
  expect_code 143 "$CODE" "a terminated smoke test reports the signal: $OUT"
  assert_smoke_clean
  pass "a killed smoke test still tears the worker down and removes its scratch directory"
}

test_setup_prompt_defaults_to_no_and_yes_runs_the_test() {
  make_smoke_case smoke-prompt
  printf 'identity.name=Josh\nidentity.branch-prefix=mao/\nharness.orchestrator=claude\nharness.worker-harness=claude\nbackend=tmux\npermissions.claude-permission-mode=bypass\nharness.worker-model=\nharness.worker-effort=\n' >"$CASE/ans"
  printf 'workers.max=6\n' >>"$CASE/ans"
  run_smoke "" --answers "$CASE/ans"
  expect_code 0 "$CODE" "answers load: $OUT"
  run_smoke $'\n'
  expect_code 0 "$CODE" "Enter at the smoke prompt keeps setup going: $OUT"
  assert_contains "$OUT" "Run 2-minute smoke test?" "the prompt is shown"
  assert_not_contains "$OUT" "Starting the smoke test" "Enter means no"
  assert_contains "$OUT" "Have fun!" "the closing screen still prints"
  assert_absent "$CASE/state/calls" "no worker was spawned"
  run_smoke ""
  assert_not_contains "$OUT" "Starting the smoke test" "input that ends at the prompt means no"
  assert_contains "$OUT" "Have fun!" "the closing screen prints after an ended input"
  run_smoke $'y\n'
  expect_code 0 "$CODE" "yes runs the test and setup completes: $OUT"
  assert_contains "$OUT" "Smoke test passed" "the smoke test ran"
  assert_contains "$OUT" "Have fun!" "the closing screen follows the test"
  assert_smoke_clean
  pass "the setup prompt defaults to no, and yes runs the smoke test before the closing screen"
}

test_smoke_test_failure_does_not_stop_setup() {
  make_smoke_case smoke-nonblocking
  printf 'identity.name=Josh\nidentity.branch-prefix=mao/\nharness.orchestrator=claude\nharness.worker-harness=claude\nbackend=tmux\npermissions.claude-permission-mode=bypass\nharness.worker-model=\nharness.worker-effort=\n' >"$CASE/ans"
  printf 'workers.max=6\n' >>"$CASE/ans"
  run_smoke "" --answers "$CASE/ans"
  # The first line answers the dispatch gate, which --answers leaves open; the second answers the smoke prompt.
  FAKE_MODE=idle SMOKE_SECONDS=5 run_smoke $'\ny\n'
  expect_code 0 "$CODE" "a failed smoke test never fails setup: $OUT"
  assert_contains "$OUT" "Smoke test timed out" "the failure is reported"
  assert_contains "$OUT" "mcao setup --smoke-test" "the retry command is named"
  assert_contains "$OUT" "Have fun!" "the closing screen still prints"
  assert_smoke_clean
  pass "a failed smoke test is reported and setup still reaches its closing screen"
}

test_no_terminal_never_prompts_or_spawns() {
  make_smoke_case smoke-no-tty
  printf 'identity.name=Josh\nidentity.branch-prefix=mao/\nharness.orchestrator=claude\nharness.worker-harness=claude\nbackend=tmux\npermissions.claude-permission-mode=bypass\nharness.worker-model=\nharness.worker-effort=\n' >"$CASE/ans"
  run_smoke "" --answers "$CASE/ans"
  ASSUME_TTY=0 run_smoke $'y\n'
  expect_code 0 "$CODE" "a finished setup without a terminal exits 0: $OUT"
  assert_not_contains "$OUT" "smoke test" "no prompt without a terminal"
  assert_absent "$CASE/state/calls" "no worker was spawned"
  pass "setup without a terminal never prompts for or runs the smoke test"
}

test_first_run_writes_every_answer
test_interrupted_run_keeps_answers
test_deferral_writes_no_file
test_section_rerun_asks_only_that_section
test_no_harness_installed_is_an_error
test_omp_orchestrator
test_omp_interactive_default_and_worker_default
test_non_tty_gate
test_effort_fallback_to_medium
test_multi_provider_worker_asks_provider
test_explicit_worker_model_is_kept
test_max_workers_default_is_six
test_max_workers_saves_a_valid_limit
test_max_workers_drops_leading_zeros
test_max_workers_refuses_invalid_values_and_asks_again
test_max_workers_deferral_writes_no_file
test_max_workers_answers_file
test_workers_section_asks_only_the_limit
test_dispatch_gate_no_writes_only_default
test_dispatch_rules
test_dispatch_skip_and_foreign_rules_preserved
test_answers_file
test_answers_never_overwrite_a_hand_written_default
test_show_roundtrip
test_check_never_fails
test_remind
test_plugins_installs_into_every_installed_harness
test_plugins_copy_agent_and_skills_for_pi_and_omp
test_plugins_copy_follows_the_pi_account_pin
test_plugins_copy_needs_an_installed_hve_plugin
test_omp_and_pi_are_valid_worker_answers
test_plugins_rerun_is_idempotent
test_plugins_failure_is_reported_and_check_names_it
test_plugins_leaves_a_hand_written_codex_profile
test_plugins_follows_the_claude_account_pin
test_plugins_local_source_skips_refresh
test_copilot_version_read_from_its_directory_marketplace
test_full_run_installs_plugins_and_says_so
test_smoke_test_passes_and_cleans_up
test_smoke_test_hands_the_worker_its_exact_instructions
test_smoke_test_reports_a_refused_spawn
test_smoke_test_times_out_when_the_worker_never_commits
test_smoke_test_bounds_the_spawn_itself
test_smoke_test_rejects_wrong_content
test_smoke_test_cleans_up_when_killed
test_setup_prompt_defaults_to_no_and_yes_runs_the_test
test_smoke_test_failure_does_not_stop_setup
test_no_terminal_never_prompts_or_spawns
