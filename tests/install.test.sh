#!/usr/bin/env bash
# tests/install.test.sh - install.sh: prerequisite reporting, the ~/.local/bin/mcao link, re-runs,
# local-edit refusal, --uninstall, the setup prompt, and the files the installer must never touch.
#
# Every case runs the real install.sh under /bin/bash in a cleared environment (env -i) against a
# throwaway HOME and a throwaway git clone, with only the utilities it needs plus fake node, tmux,
# herdr, harness and no-mistakes binaries on PATH. No case reads or writes the developer's HOME,
# ~/.local/bin, ~/.motif, shell rc files, or a no-mistakes daemon.
set -u

# Inherited git and installer variables must not leak into the fixtures or the installer run.
unset GIT_DIR GIT_WORK_TREE GIT_INDEX_FILE MOA_INSTALL_DIR MOA_INSTALL_ASSUME_TTY

# shellcheck source=tests/fixtures.sh
. "$(dirname "${BASH_SOURCE[0]}")/fixtures.sh"

INSTALL="$ROOT/install.sh"
TMP_ROOT=$(fm_test_tmproot install)
NO_MISTAKES_INSTALL_LINE='curl -fsSL https://raw.githubusercontent.com/kunchenguid/no-mistakes/main/docs/install.sh | sh'

# tgit: git for fixture setup only, with no developer global or system config.
tgit() { GIT_CONFIG_GLOBAL=/dev/null GIT_CONFIG_NOSYSTEM=1 git "$@"; }

# make_toolbin <dir>: symlinks to the real utilities install.sh uses, nothing else.
make_toolbin() {
  local dir=$1 t path
  mkdir -p "$dir"
  for t in cat dirname git ln mkdir readlink rm; do
    path=$(command -v "$t") || fail "test host is missing $t"
    ln -sf "$path" "$dir/$t"
  done
}

# make_template: the fixture clone is built once per suite, because git init and commit are slow on
# the test host. The stub logs to the running case's own HOME, so one template serves every case.
make_template() {
  local t="$TMP_ROOT/template"
  mkdir -p "$t/bin"
  cat >"$t/bin/mcao" <<'EOF'
#!/bin/sh
echo "$*" >>"$HOME/fake-mcao.log"
EOF
  chmod +x "$t/bin/mcao"
  printf 'Motif fixture\n' >"$t/README.md"
  tgit -C "$t" init -q
  tgit -C "$t" add -A
  tgit -C "$t" -c user.name=test -c user.email=test@example.invalid commit -q -m fixture
}

# make_case <name>: a home, a fake dir, a tool dir, and a fresh copy of the template clone.
CASE=""
make_case() {
  local name=$1
  CASE="$TMP_ROOT/$name"
  mkdir -p "$CASE/home" "$CASE/fake"
  make_toolbin "$CASE/tools"
  cp -R "$TMP_ROOT/template" "$CASE/clone"
}

# fake_tool <name>: an executable that does nothing and succeeds.
fake_tool() {
  printf '#!/bin/sh\nexit 0\n' >"$CASE/fake/$1"
  chmod +x "$CASE/fake/$1"
}

# fake_node <version without the leading v>
fake_node() {
  cat >"$CASE/fake/node" <<EOF
#!/bin/sh
echo "v$1"
EOF
  chmod +x "$CASE/fake/node"
}

# fake_no_mistakes <version without the leading v>: logs every invocation so a test can prove what ran.
fake_no_mistakes() {
  cat >"$CASE/fake/no-mistakes" <<EOF
#!/bin/sh
echo "\$*" >>"$CASE/no-mistakes.log"
echo "no-mistakes version v$1 (4822244) 2026-09-26T08:31:07Z"
EOF
  chmod +x "$CASE/fake/no-mistakes"
}

# happy_prereqs: every required and optional prerequisite present at a passing version.
happy_prereqs() {
  fake_node 20.11.0
  fake_tool tmux
  fake_tool claude
  fake_no_mistakes 1.84.0
}

# run_install <stdin-text> [args...]: sets OUT, ERR and CODE. Stdin is a file, so it is never a terminal
# unless ASSUME_TTY=1 says so. PATH_OVERRIDE and INSTALL_CLONE let a case change the environment.
OUT=""
ERR=""
CODE=0
run_install() {
  local input=$1
  shift
  printf '%s' "$input" >"$CASE/stdin"
  OUT=$(env -i HOME="$CASE/home" PATH="${PATH_OVERRIDE:-$CASE/fake:$CASE/tools}" \
    MOA_INSTALL_DIR="${INSTALL_CLONE:-$CASE/clone}" \
    ${ASSUME_TTY:+MOA_INSTALL_ASSUME_TTY="$ASSUME_TTY"} \
    /bin/bash "$INSTALL" "$@" <"$CASE/stdin" 2>"$CASE/stderr")
  CODE=$?
  ERR=$(cat "$CASE/stderr")
}

# reset_run_env: clears per-case overrides between tests.
reset_run_env() {
  unset PATH_OVERRIDE INSTALL_CLONE ASSUME_TTY
}

link_points_at_clone() {
  [ -L "$CASE/home/.local/bin/mcao" ] && [ "$(readlink "$CASE/home/.local/bin/mcao")" = "$CASE/clone/bin/mcao" ]
}

# ---- install -------------------------------------------------------------------------------------

test_clean_install_links_mcao_into_local_bin() {
  make_case clean
  happy_prereqs
  run_install ''
  expect_code 0 "$CODE" "a clean install with every prerequisite should succeed: $OUT $ERR"
  link_points_at_clone || fail "\$HOME/.local/bin/mcao should link to the clone's bin/mcao"
  assert_contains "$OUT" "Installed. Next: run \`mcao setup\`" "the next step should name mcao setup"
  assert_absent "$CASE/home/fake-mcao.log" "an install must not run mcao setup without a terminal"
  reset_run_env
  pass "clean install links mcao into ~/.local/bin"
}

test_rerun_is_idempotent() {
  make_case rerun
  happy_prereqs
  run_install ''
  expect_code 0 "$CODE" "first run: $OUT $ERR"
  run_install ''
  expect_code 0 "$CODE" "second run should succeed: $OUT $ERR"
  link_points_at_clone || fail "a second run should leave the link pointing at the clone"
  assert_contains "$OUT" "already links to" "a second run should report the link as already in place"
  reset_run_env
  pass "re-running the installer is idempotent"
}

test_relinks_when_the_link_was_removed() {
  make_case relink
  happy_prereqs
  run_install ''
  expect_code 0 "$CODE" "first run: $OUT $ERR"
  rm -f "$CASE/home/.local/bin/mcao"
  run_install ''
  expect_code 0 "$CODE" "re-run after the link was removed: $OUT $ERR"
  link_points_at_clone || fail "a re-run should recreate the removed link"
  reset_run_env
  pass "re-run recreates a removed link"
}

test_replaces_a_stale_link_into_this_clone_only() {
  make_case stale
  happy_prereqs
  mkdir -p "$CASE/home/.local/bin"
  ln -s "$CASE/elsewhere/mcao" "$CASE/home/.local/bin/mcao"
  run_install ''
  expect_code 0 "$CODE" "a stale link to another path should be re-pointed: $OUT $ERR"
  link_points_at_clone || fail "the stale link should now point at this clone"
  reset_run_env
  pass "a stale link is re-pointed at this clone"
}

test_refuses_to_replace_a_regular_file_at_the_link() {
  make_case regular
  happy_prereqs
  mkdir -p "$CASE/home/.local/bin"
  printf 'keep me\n' >"$CASE/home/.local/bin/mcao"
  run_install ''
  expect_code 1 "$CODE" "a regular file at the link path should stop the install"
  assert_contains "$ERR" "is not a symlink" "the refusal should say why"
  assert_equals "keep me" "$(cat "$CASE/home/.local/bin/mcao")" "the existing file must not be overwritten"
  reset_run_env
  pass "a regular file at the link path is never overwritten"
}

test_warns_when_local_bin_is_not_on_path() {
  make_case path-warn
  happy_prereqs
  run_install ''
  expect_code 0 "$CODE" "install off PATH: $OUT $ERR"
  assert_contains "$OUT" "is not on your PATH" "a PATH gap should be reported"
  reset_run_env

  make_case path-ok
  happy_prereqs
  PATH_OVERRIDE="$CASE/home/.local/bin:$CASE/fake:$CASE/tools"
  run_install ''
  expect_code 0 "$CODE" "install with ~/.local/bin on PATH: $OUT $ERR"
  assert_not_contains "$OUT" "is not on your PATH" "no warning when ~/.local/bin is already on PATH"
  reset_run_env
  pass "PATH gap is reported, and only when it exists"
}

# ---- prerequisites -------------------------------------------------------------------------------

test_old_node_prints_upgrade_line_and_continues() {
  make_case node-old
  happy_prereqs
  fake_node 19.0.0
  run_install ''
  expect_code 0 "$CODE" "Node.js below 20 should not block the install: $OUT $ERR"
  assert_contains "$OUT" "Node.js >= 20 not found (found: v19.0.0)" "the Node.js shortfall should be named"
  assert_contains "$OUT" "https://nodejs.org/en/download" "the Node.js install instruction should be printed"
  link_points_at_clone || fail "the link should still be created when only Node.js is short"
  reset_run_env
  pass "Node.js below 20 prints an instruction and continues"
}

test_missing_node_prints_instruction_and_continues() {
  make_case node-missing
  fake_tool tmux
  fake_tool claude
  fake_no_mistakes 1.84.0
  run_install ''
  expect_code 0 "$CODE" "missing Node.js should not block the install: $OUT $ERR"
  assert_contains "$OUT" "Node.js >= 20 not found (found: none)" "a missing Node.js should be named"
  reset_run_env
  pass "missing Node.js prints an instruction and continues"
}

test_neither_tmux_nor_herdr_fails_before_linking() {
  make_case no-backend
  fake_node 20.11.0
  fake_tool claude
  fake_no_mistakes 1.84.0
  run_install ''
  expect_code 1 "$CODE" "without tmux or herdr the install should fail"
  assert_contains "$ERR" "tmux or herdr is required" "the missing session backend should be named on stderr"
  assert_absent "$CASE/home/.local/bin/mcao" "nothing should be linked when a required prerequisite is missing"
  reset_run_env
  pass "missing tmux and herdr fails loudly before linking"
}

test_herdr_alone_satisfies_the_session_backend() {
  make_case herdr-only
  fake_node 20.11.0
  fake_tool herdr
  fake_tool claude
  fake_no_mistakes 1.84.0
  run_install ''
  expect_code 0 "$CODE" "herdr without tmux should be enough: $OUT $ERR"
  assert_contains "$OUT" "ok: herdr" "herdr should be reported as the session backend"
  reset_run_env
  pass "herdr alone satisfies the session backend check"
}

test_no_harness_cli_fails_before_linking() {
  make_case no-harness
  fake_node 20.11.0
  fake_tool tmux
  fake_no_mistakes 1.84.0
  run_install ''
  expect_code 1 "$CODE" "without any harness CLI the install should fail"
  assert_contains "$ERR" "No harness CLI found" "the missing harness should be named on stderr"
  assert_absent "$CASE/home/.local/bin/mcao" "nothing should be linked without a harness CLI"
  reset_run_env
  pass "no harness CLI fails loudly before linking"
}

test_any_one_harness_cli_is_enough() {
  make_case codex-only
  fake_node 20.11.0
  fake_tool tmux
  fake_tool codex
  fake_no_mistakes 1.84.0
  run_install ''
  expect_code 0 "$CODE" "codex alone should satisfy the harness check: $OUT $ERR"
  assert_contains "$OUT" "ok: Codex (codex)" "codex should be detected as its own check"
  assert_contains "$OUT" "- Claude Code not found (claude)" "a missing harness should be reported separately"
  reset_run_env
  pass "any one harness CLI satisfies the harness check"
}

test_old_no_mistakes_prints_upstream_line_and_continues() {
  make_case nm-old
  happy_prereqs
  fake_no_mistakes 1.83.0
  run_install ''
  expect_code 0 "$CODE" "no-mistakes below v1.84.0 should not block the install: $OUT $ERR"
  assert_contains "$OUT" "no-mistakes >= v1.84.0 not found (found: 1.83.0)" "the shortfall should be named"
  assert_contains "$OUT" "$NO_MISTAKES_INSTALL_LINE" "the upstream installer line should be printed"
  link_points_at_clone || fail "the link should still be created when only no-mistakes is short"
  reset_run_env
  pass "no-mistakes below v1.84.0 prints the upstream line and continues"
}

test_missing_no_mistakes_prints_upstream_line_and_continues() {
  make_case nm-missing
  fake_node 20.11.0
  fake_tool tmux
  fake_tool claude
  run_install ''
  expect_code 0 "$CODE" "missing no-mistakes should not block the install: $OUT $ERR"
  assert_contains "$OUT" "found: none" "a missing no-mistakes should be named"
  assert_contains "$OUT" "$NO_MISTAKES_INSTALL_LINE" "the upstream installer line should be printed"
  reset_run_env
  pass "missing no-mistakes prints the upstream line and continues"
}

test_no_mistakes_daemon_is_never_administered() {
  make_case nm-daemon
  happy_prereqs
  run_install ''
  expect_code 0 "$CODE" "install: $OUT $ERR"
  assert_equals "--version" "$(cat "$CASE/no-mistakes.log")" "the installer may only ask no-mistakes its version, never restart or administer its daemon"
  reset_run_env
  pass "the installer never restarts or administers the no-mistakes daemon"
}

# ---- local edits ---------------------------------------------------------------------------------

test_local_edits_are_refused_without_force() {
  make_case dirty
  happy_prereqs
  printf 'local edit\n' >>"$CASE/clone/README.md"
  run_install ''
  expect_code 1 "$CODE" "a clone with local edits should be refused"
  assert_contains "$ERR" "Local edits detected" "the refusal should say why"
  assert_contains "$ERR" "Stash or commit them" "the refusal should say how to proceed"
  assert_absent "$CASE/home/.local/bin/mcao" "a refused install must not link anything"
  reset_run_env
  pass "local edits are refused without --force"
}

test_local_edits_install_with_force() {
  make_case dirty-force
  happy_prereqs
  printf 'local edit\n' >>"$CASE/clone/README.md"
  run_install '' --force
  expect_code 0 "$CODE" "--force should install over local edits: $OUT $ERR"
  link_points_at_clone || fail "--force should create the link"
  reset_run_env
  pass "--force installs over local edits"
}

test_untracked_files_are_not_local_edits() {
  make_case untracked
  happy_prereqs
  printf 'scratch\n' >"$CASE/clone/notes.txt"
  run_install ''
  expect_code 0 "$CODE" "an untracked file should not block the install: $OUT $ERR"
  reset_run_env
  pass "untracked files do not count as local edits"
}

test_clone_that_is_not_git_is_refused_without_force() {
  make_case not-git
  happy_prereqs
  rm -rf "$CASE/clone/.git"
  run_install ''
  expect_code 1 "$CODE" "a clone that is not a git checkout cannot be checked for edits"
  assert_contains "$ERR" "is not a git checkout" "the refusal should say why"
  reset_run_env
  pass "a non-git clone is refused without --force"
}

test_clone_without_entry_command_fails() {
  make_case incomplete
  happy_prereqs
  rm -f "$CASE/clone/bin/mcao"
  run_install '' --force
  expect_code 1 "$CODE" "a clone without bin/mcao should not be linked"
  assert_contains "$ERR" "is not a complete clone" "the incomplete clone should be named"
  assert_absent "$CASE/home/.local/bin/mcao" "nothing should be linked into an incomplete clone"
  reset_run_env
  pass "a clone without bin/mcao fails before linking"
}

test_missing_clone_directory_fails() {
  make_case no-clone
  happy_prereqs
  INSTALL_CLONE="$CASE/absent"
  run_install ''
  expect_code 1 "$CODE" "a clone directory that does not exist should fail"
  assert_contains "$ERR" "clone directory does not exist" "the missing directory should be named"
  reset_run_env
  pass "a missing clone directory fails loudly"
}

# ---- uninstall -----------------------------------------------------------------------------------

test_uninstall_removes_only_the_link_and_keeps_fleet_state() {
  make_case uninstall
  happy_prereqs
  run_install ''
  expect_code 0 "$CODE" "install before uninstall: $OUT $ERR"
  mkdir -p "$CASE/home/.motif/multi-agent"
  printf 'fleet record\n' >"$CASE/home/.motif/multi-agent/state.txt"
  run_install '' --uninstall
  expect_code 0 "$CODE" "--uninstall should succeed: $OUT $ERR"
  assert_absent "$CASE/home/.local/bin/mcao" "--uninstall should remove the link"
  assert_contains "$OUT" "Removed $CASE/home/.local/bin/mcao" "the removal should be reported"
  assert_equals "fleet record" "$(cat "$CASE/home/.motif/multi-agent/state.txt")" "\$HOME/.motif/multi-agent/ must survive --uninstall"
  reset_run_env
  pass "--uninstall removes only the link and keeps fleet state"
}

test_uninstall_does_not_need_prerequisites() {
  make_case uninstall-bare
  happy_prereqs
  run_install ''
  expect_code 0 "$CODE" "install before uninstall: $OUT $ERR"
  PATH_OVERRIDE="$CASE/tools"
  run_install '' --uninstall
  expect_code 0 "$CODE" "--uninstall should not check prerequisites: $OUT $ERR"
  assert_absent "$CASE/home/.local/bin/mcao" "--uninstall should remove the link without any prerequisite on PATH"
  reset_run_env
  pass "--uninstall needs no prerequisites on PATH"
}

test_uninstall_leaves_a_link_pointing_elsewhere() {
  make_case uninstall-foreign
  happy_prereqs
  mkdir -p "$CASE/home/.local/bin"
  ln -s "$CASE/elsewhere/mcao" "$CASE/home/.local/bin/mcao"
  run_install '' --uninstall
  expect_code 0 "$CODE" "--uninstall should not fail on a foreign link: $OUT $ERR"
  if [ ! -L "$CASE/home/.local/bin/mcao" ]; then
    fail "--uninstall must leave a link that points at another install"
  fi
  assert_contains "$OUT" "Left $CASE/home/.local/bin/mcao alone" "the foreign link should be reported and kept"
  reset_run_env
  pass "--uninstall leaves a link that points at another install"
}

test_uninstall_with_nothing_installed_exits_zero() {
  make_case uninstall-empty
  run_install '' --uninstall
  expect_code 0 "$CODE" "--uninstall with nothing installed should succeed: $OUT $ERR"
  assert_contains "$OUT" "Nothing to remove" "the empty state should be reported"
  reset_run_env
  pass "--uninstall with nothing installed exits zero"
}

# ---- setup prompt --------------------------------------------------------------------------------

test_non_terminal_run_never_prompts() {
  make_case no-tty
  happy_prereqs
  run_install $'y\n'
  expect_code 0 "$CODE" "a non-terminal install should exit zero: $OUT $ERR"
  assert_not_contains "$OUT" "Run setup now" "a non-terminal run must not prompt"
  assert_absent "$CASE/home/fake-mcao.log" "a non-terminal run must not run mcao setup"
  reset_run_env
  pass "a non-terminal run never prompts or runs setup"
}

test_terminal_prompt_yes_runs_setup() {
  make_case tty-yes
  happy_prereqs
  ASSUME_TTY=1
  run_install $'y\n'
  expect_code 0 "$CODE" "answering yes should run setup: $OUT $ERR"
  assert_contains "$OUT" "Run setup now? [Y/n]" "a terminal run should prompt"
  assert_equals "setup" "$(cat "$CASE/home/fake-mcao.log")" "answering yes should run the clone's mcao setup"
  reset_run_env
  pass "terminal prompt answered yes runs mcao setup"
}

test_terminal_prompt_default_runs_setup() {
  make_case tty-default
  happy_prereqs
  ASSUME_TTY=1
  run_install $'\n'
  expect_code 0 "$CODE" "an empty answer should default to yes: $OUT $ERR"
  assert_equals "setup" "$(cat "$CASE/home/fake-mcao.log")" "an empty answer should run mcao setup"
  reset_run_env
  pass "terminal prompt defaults to yes"
}

test_terminal_prompt_no_skips_setup() {
  make_case tty-no
  happy_prereqs
  ASSUME_TTY=1
  run_install $'n\n'
  expect_code 0 "$CODE" "declining setup should exit zero: $OUT $ERR"
  assert_contains "$OUT" "Skipped. Run \`mcao setup\` when you are ready." "declining should say how to run setup later"
  assert_absent "$CASE/home/fake-mcao.log" "declining must not run mcao setup"
  reset_run_env
  pass "terminal prompt answered no skips setup and exits zero"
}

# ---- files the installer must never touch --------------------------------------------------------

test_protected_and_shell_files_are_never_touched() {
  make_case protected
  happy_prereqs
  mkdir -p "$CASE/home/.claude" "$CASE/home/.codex"
  printf 'settings\n' >"$CASE/home/.claude/settings.json"
  printf 'guidance\n' >"$CASE/home/.claude/CLAUDE.md"
  printf 'codex\n' >"$CASE/home/.codex/config.toml"
  printf 'export PATH=x\n' >"$CASE/home/.zshrc"
  printf 'export PATH=y\n' >"$CASE/home/.bashrc"
  run_install ''
  expect_code 0 "$CODE" "install beside protected files: $OUT $ERR"
  assert_equals "settings" "$(cat "$CASE/home/.claude/settings.json")" "settings.json must not change"
  assert_equals "guidance" "$(cat "$CASE/home/.claude/CLAUDE.md")" "CLAUDE.md must not change"
  assert_equals "codex" "$(cat "$CASE/home/.codex/config.toml")" "harness config must not change"
  assert_equals "export PATH=x" "$(cat "$CASE/home/.zshrc")" ".zshrc must not change"
  assert_equals "export PATH=y" "$(cat "$CASE/home/.bashrc")" ".bashrc must not change"
  assert_absent "$CASE/home/.claude/skills" "no skill directory should be created for Claude"
  assert_absent "$CASE/home/.codex/skills" "no skill directory should be created for Codex"
  reset_run_env
  pass "protected instruction files, harness config and shell rc files are never touched"
}

test_fleet_state_directory_is_never_read_or_written() {
  make_case fleet-state
  happy_prereqs
  mkdir -p "$CASE/home/.motif/multi-agent/data"
  printf 'fleet record\n' >"$CASE/home/.motif/multi-agent/data/record.md"
  before=$(ls -AR "$CASE/home/.motif/multi-agent")
  run_install ''
  expect_code 0 "$CODE" "install beside fleet state: $OUT $ERR"
  assert_equals "$before" "$(ls -AR "$CASE/home/.motif/multi-agent")" "the installer must not add or remove anything under ~/.motif/multi-agent/"
  assert_equals "fleet record" "$(cat "$CASE/home/.motif/multi-agent/data/record.md")" "fleet records must not change"
  reset_run_env
  pass "the installer never writes ~/.motif/multi-agent/"
}

# ---- arguments -----------------------------------------------------------------------------------

test_purge_is_not_an_option() {
  make_case purge
  happy_prereqs
  mkdir -p "$CASE/home/.motif/multi-agent"
  printf 'fleet record\n' >"$CASE/home/.motif/multi-agent/state.txt"
  run_install '' --purge
  expect_code 2 "$CODE" "--purge should be rejected as an invalid argument"
  assert_contains "$ERR" "unknown argument: --purge" "the rejected flag should be named"
  assert_equals "fleet record" "$(cat "$CASE/home/.motif/multi-agent/state.txt")" "a rejected --purge must delete nothing"
  reset_run_env
  pass "--purge is rejected and deletes nothing"
}

test_positional_argument_is_rejected() {
  make_case positional
  happy_prereqs
  run_install '' stray
  expect_code 2 "$CODE" "a positional argument should be rejected"
  assert_absent "$CASE/home/.local/bin/mcao" "a rejected invocation must not link anything"
  reset_run_env
  pass "a positional argument exits 2"
}

test_help_exits_zero() {
  make_case help
  run_install '' --help
  expect_code 0 "$CODE" "--help should succeed: $ERR"
  assert_contains "$OUT" "Usage: install.sh" "help should print usage"
  reset_run_env
  pass "--help prints usage and exits zero"
}

make_template

test_clean_install_links_mcao_into_local_bin
test_rerun_is_idempotent
test_relinks_when_the_link_was_removed
test_replaces_a_stale_link_into_this_clone_only
test_refuses_to_replace_a_regular_file_at_the_link
test_warns_when_local_bin_is_not_on_path
test_old_node_prints_upgrade_line_and_continues
test_missing_node_prints_instruction_and_continues
test_neither_tmux_nor_herdr_fails_before_linking
test_herdr_alone_satisfies_the_session_backend
test_no_harness_cli_fails_before_linking
test_any_one_harness_cli_is_enough
test_old_no_mistakes_prints_upstream_line_and_continues
test_missing_no_mistakes_prints_upstream_line_and_continues
test_no_mistakes_daemon_is_never_administered
test_local_edits_are_refused_without_force
test_local_edits_install_with_force
test_untracked_files_are_not_local_edits
test_clone_that_is_not_git_is_refused_without_force
test_clone_without_entry_command_fails
test_missing_clone_directory_fails
test_uninstall_removes_only_the_link_and_keeps_fleet_state
test_uninstall_does_not_need_prerequisites
test_uninstall_leaves_a_link_pointing_elsewhere
test_uninstall_with_nothing_installed_exits_zero
test_non_terminal_run_never_prompts
test_terminal_prompt_yes_runs_setup
test_terminal_prompt_default_runs_setup
test_terminal_prompt_no_skips_setup
test_protected_and_shell_files_are_never_touched
test_fleet_state_directory_is_never_read_or_written
test_purge_is_not_an_option
test_positional_argument_is_rejected
test_help_exits_zero
