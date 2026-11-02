#!/usr/bin/env bash
# Behavior tests for bin/fm-pstack.sh, the single owner of the pstack worker
# workflow's availability resolution and its deterministic proof record.
# Covers the resolve report and every refusal exit code, the template command,
# the render command, and the proof-check self-check's happy path plus every
# failure line, including the --check-body and --check-comment equality checks.
set -u

command -v jq >/dev/null 2>&1 || { echo "skip: jq not found"; exit 0; }

# shellcheck source=tests/lib.sh
. "$(dirname "${BASH_SOURCE[0]}")/lib.sh"
# shellcheck source=tests/pstack-plugin-fixture.sh
. "$(dirname "${BASH_SOURCE[0]}")/pstack-plugin-fixture.sh"

TMP_ROOT=$(fm_test_tmproot fm-pstack)
fm_git_identity fmtest fmtest@example.invalid

HOME_DIR="$TMP_ROOT/home"
CONF="$HOME_DIR/config"
STATE="$HOME_DIR/state"
DATA="$HOME_DIR/data"
FAKEBIN="$TMP_ROOT/fakebin"
mkdir -p "$CONF" "$STATE" "$DATA" "$FAKEBIN"

# Resolves to a valid fixture plugin under $TMP_ROOT; each resolve case points
# $CONF/pstack-plugin where it needs.
plugin_resolve() {  # [<harness>]: run resolve against the fixture home
  out=$(env FM_ROOT_OVERRIDE="$ROOT" FM_HOME="$HOME_DIR" \
    "$ROOT/bin/fm-pstack.sh" resolve --harness "${1:-claude}" 2>&1)
}

plugin_supported() {
  out=$(env FM_ROOT_OVERRIDE="$ROOT" FM_HOME="$HOME_DIR" \
    "$ROOT/bin/fm-pstack.sh" supported-harnesses 2>&1)
}

set_cfg() {  # <value>
  printf '%s\n' "$1" > "$CONF/pstack-plugin"
}

rm_cfg() {
  rm -f "$CONF/pstack-plugin"
}

VALID_PLUGIN="$TMP_ROOT/plugin-valid"

test_supported_harnesses_is_claude_and_codex() {
  plugin_supported
  assert_equals "$(printf 'claude\ncodex')" "$out" "supported-harnesses did not print exactly claude and codex"
  pass "supported-harnesses prints exactly claude and codex"
}

test_resolve_success_report() {
  pstack_write_plugin "$VALID_PLUGIN" pstack
  set_cfg "$VALID_PLUGIN"
  plugin_resolve
  expect_code 0 "$?" "valid resolve"
  assert_contains "$out" "workflow=pstack" "resolve report lost workflow"
  assert_contains "$out" "harness=claude" "resolve report lost harness"
  assert_contains "$out" "plugin_dir=" "resolve report lost plugin_dir"
  assert_contains "$out" "plugin=pstack" "resolve report lost plugin name"
  assert_contains "$out" "entry=pstack:poteto-mode" "resolve did not form the entry"
  assert_contains "$out" "load=plugin-dir" "resolve report lost load"
  assert_contains "$out" "hooks=none" "clean plugin resolved hooks=present"
  assert_contains "$out" "mcp=none" "clean plugin resolved mcp=present"
  pass "valid resolve prints the full report"
}

test_resolve_codex_success_report() {
  pstack_write_plugin "$VALID_PLUGIN" pstack
  set_cfg "$VALID_PLUGIN"
  plugin_resolve codex
  expect_code 0 "$?" "valid codex resolve"
  assert_contains "$out" "workflow=pstack" "codex resolve report lost workflow"
  assert_contains "$out" "harness=codex" "codex resolve report lost harness"
  assert_contains "$out" "entry=pstack:poteto-mode" "codex resolve did not form the entry"
  pass "codex resolve prints the report for the codex harness"
}

test_resolve_absent_config_is_exit_4() {
  rm_cfg
  plugin_resolve
  expect_code 4 "$?" "missing config exit code"
  assert_contains "$out" "error: no pstack plugin is configured" "missing config error message"
  assert_contains "$out" "next:" "missing config refusal carries no next line"
  pass "absent config refuses with exit 4"
}

test_resolve_empty_config_is_exit_3() {
  set_cfg ""
  plugin_resolve
  expect_code 3 "$?" "empty config exit code"
  assert_contains "$out" "error:" "empty config error message"
  pass "empty config refuses with exit 3"
}

test_resolve_multiline_config_is_exit_3() {
  set_cfg "$VALID_PLUGIN
$VALID_PLUGIN"
  plugin_resolve
  expect_code 3 "$?" "multiline config exit code"
  assert_contains "$out" "must hold exactly one path" "multiline config error message"
  pass "multiline config refuses with exit 3"
}

test_resolve_relative_path_is_exit_3() {
  set_cfg "relative/plugin"
  plugin_resolve
  expect_code 3 "$?" "relative config exit code"
  assert_contains "$out" "not an absolute path" "relative path error message"
  pass "relative path refuses with exit 3"
}

test_resolve_missing_dir_is_exit_4() {
  set_cfg "$TMP_ROOT/plugin-absent"
  plugin_resolve
  expect_code 4 "$?" "missing plugin dir exit code"
  assert_contains "$out" "does not exist" "missing dir error message"
  pass "missing plugin directory refuses with exit 4"
}

test_resolve_folder_of_plugins_is_exit_4() {
  pstack_plugins_folder "$TMP_ROOT/plugins-folder"
  set_cfg "$TMP_ROOT/plugins-folder"
  plugin_resolve
  expect_code 4 "$?" "folder of plugins exit code"
  assert_contains "$out" "not a Claude plugin root" "folder-of-plugins error message"
  pass "a folder of plugins refuses with exit 4"
}

test_resolve_malformed_json_is_exit_3() {
  pstack_write_plugin_malformed "$TMP_ROOT/plugin-malformed"
  set_cfg "$TMP_ROOT/plugin-malformed"
  plugin_resolve
  expect_code 3 "$?" "malformed plugin.json exit code"
  assert_contains "$out" "not parseable JSON" "malformed json error message"
  pass "malformed plugin.json refuses with exit 3"
}

test_resolve_no_name_is_exit_3() {
  pstack_write_plugin_no_name "$TMP_ROOT/plugin-noname"
  set_cfg "$TMP_ROOT/plugin-noname"
  plugin_resolve
  expect_code 3 "$?" "no-name plugin.json exit code"
  assert_contains "$out" "no usable \"name\"" "no-name error message"
  pass "plugin.json without a name refuses with exit 3"
}

test_resolve_missing_entry_skill_is_exit_4() {
  pstack_write_plugin "$TMP_ROOT/plugin-noskill" pstack
  rm -rf "$TMP_ROOT/plugin-noskill/skills"
  set_cfg "$TMP_ROOT/plugin-noskill"
  plugin_resolve
  expect_code 4 "$?" "missing entry skill exit code"
  assert_contains "$out" "the pstack entry skill is missing" "missing skill error message"
  pass "missing poteto-mode skill refuses with exit 4"
}

test_resolve_disabled_entry_is_exit_4() {
  pstack_write_plugin_disabled "$TMP_ROOT/plugin-disabled"
  set_cfg "$TMP_ROOT/plugin-disabled"
  plugin_resolve
  expect_code 4 "$?" "disabled entry exit code"
  assert_contains "$out" "not model-invocable" "disabled entry error message"
  assert_contains "$out" "harness-neutral pstack port" "disabled entry next line"
  pass "disable-model-invocation entry refuses with exit 4"
}

test_resolve_entry_without_frontmatter_is_exit_4() {
  pstack_write_plugin "$TMP_ROOT/plugin-nofm" pstack
  printf 'plain body\n' > "$TMP_ROOT/plugin-nofm/skills/poteto-mode/SKILL.md"
  set_cfg "$TMP_ROOT/plugin-nofm"
  plugin_resolve
  expect_code 4 "$?" "no-frontmatter entry exit code"
  assert_contains "$out" "not model-invocable" "no-frontmatter error message"
  pass "an entry skill without frontmatter refuses with exit 4"
}

test_resolve_hooks_and_mcp_detection() {
  pstack_write_plugin_hooks "$TMP_ROOT/plugin-hooks" pstack
  pstack_write_plugin_mcp_file "$TMP_ROOT/plugin-mcpfile" pstack
  pstack_write_plugin_mcp_json "$TMP_ROOT/plugin-mcpjson" pstack
  set_cfg "$TMP_ROOT/plugin-hooks"
  plugin_resolve
  assert_contains "$out" "hooks=present" "hooks/ directory not detected"
  assert_contains "$out" "mcp=none" "hooks-only plugin resolved mcp=present"
  set_cfg "$TMP_ROOT/plugin-mcpfile"
  plugin_resolve
  assert_contains "$out" "mcp=present" "plugin-root .mcp.json not detected"
  assert_contains "$out" "hooks=none" "mcp-file plugin resolved hooks=present"
  set_cfg "$TMP_ROOT/plugin-mcpjson"
  plugin_resolve
  assert_contains "$out" "mcp=present" "plugin.json mcpServers not detected"
  pass "hooks and mcp are reported from both sources"
}

test_resolve_unsupported_harness_is_exit_5() {
  local harness
  for harness in copilot prime-agent pa-daemon; do
    set_cfg "$VALID_PLUGIN"
    plugin_resolve "$harness"
    expect_code 5 "$?" "$harness unsupported pstack harness exit code"
    assert_contains "$out" "not supported on harness" "$harness unsupported harness error message"
    assert_contains "$out" "pstack ships on claude and codex only" "$harness refusal does not name the matrix"
    # The harness matrix is checked before any configuration read: no config file.
    rm_cfg
    plugin_resolve "$harness"
    expect_code 5 "$?" "$harness unsupported harness exit code without config"
  done
  pass "unsupported harness refuses with exit 5 before reading config"
}

test_resolve_usage_is_exit_2() {
  out=$(env FM_ROOT_OVERRIDE="$ROOT" FM_HOME="$HOME_DIR" \
    "$ROOT/bin/fm-pstack.sh" resolve --harness claude extra 2>&1)
  expect_code 2 "$?" "resolve with extra args exit code"
  rm_cfg
  out=$(env FM_ROOT_OVERRIDE="$ROOT" FM_HOME="$HOME_DIR" \
    "$ROOT/bin/fm-pstack.sh" resolve 2>&1)
  expect_code 2 "$?" "resolve without --harness exit code"
  out=$(env FM_ROOT_OVERRIDE="$ROOT" FM_HOME="$HOME_DIR" \
    "$ROOT/bin/fm-pstack.sh" resolve --harness 2>&1)
  expect_code 2 "$?" "resolve without a harness value exit code"
  out=$(env FM_ROOT_OVERRIDE="$ROOT" FM_HOME="$HOME_DIR" "$ROOT/bin/fm-pstack.sh" bogus 2>&1)
  expect_code 2 "$?" "unknown command exit code"
  pass "wrong usage refuses with exit 2"
}

test_template_prints_the_proof_format() {
  set_cfg "$VALID_PLUGIN"
  out=$(env FM_ROOT_OVERRIDE="$ROOT" FM_HOME="$HOME_DIR" \
    "$ROOT/bin/fm-pstack.sh" template pp-task 2>&1)
  expect_code 0 "$?" "template exit code"
  assert_contains "$out" "moa-proof: v2" "template header line"
  assert_contains "$out" "task: pp-task" "template task line"
  assert_contains "$out" "entry: pstack:poteto-mode" "template entry line not resolved from config"
  assert_contains "$out" "brief: <path-to-brief>" "template brief field"
  assert_contains "$out" "source: <url or 'pasted'>" "template source field"
  assert_contains "$out" "## Reproduction or baseline" "template reproduction section"
  assert_contains "$out" "## Checks" "template checks section"
  assert_contains "$out" "## Acceptance criteria" "template acceptance criteria section"
  assert_contains "$out" "## AI review" "template AI review section"
  assert_contains "$out" "## Scope" "template scope section"
  assert_contains "$out" "## Not run remaining uncertainty" "template not run section"
  pass "template prints the v2 proof skeleton with the resolved entry"
}

test_template_refuses_like_resolve() {
  rm_cfg
  out=$(env FM_ROOT_OVERRIDE="$ROOT" FM_HOME="$HOME_DIR" \
    "$ROOT/bin/fm-pstack.sh" template pp-task 2>&1)
  expect_code 4 "$?" "template with absent config exit code"
  out=$(env FM_ROOT_OVERRIDE="$ROOT" FM_HOME="$HOME_DIR" \
    "$ROOT/bin/fm-pstack.sh" template pp/task 2>&1)
  expect_code 2 "$?" "template with a slash id exit code"
  pass "template reuses resolve's refusals and rejects a slash id"
}

# --- proof-check -------------------------------------------------------------

# pc_setup <task-id> <branch>: write the fixture repo, worktree, meta, fake
# no-mistakes (zero-run), and return nothing. Sets PC_REPO/PC_WT/PC_ID/PC_BRANCH/
# PC_BASE/PC_PROOF, and makes a first commit (base) plus a second (candidate) on
# the branch with HEAD there.
pc_setup() {
  PC_ID=$1
  PC_BRANCH=$2
  PC_REPO="$TMP_ROOT/pc-repo"
  PC_WT="$TMP_ROOT/pc-wt"
  rm -rf "$PC_REPO" "$PC_WT" "$TMP_ROOT/pc-repo.origin.git"
  fm_git_worktree "$PC_REPO" "$PC_WT" "$PC_BRANCH"
  printf '%s\n' change > "$PC_WT/fixed.txt"
  git -C "$PC_WT" add fixed.txt
  git -C "$PC_WT" commit -qm 'the proof candidate'
  PC_BASE=$(git -C "$PC_WT" rev-parse HEAD~1)
  PC_CAND=$(git -C "$PC_WT" rev-parse HEAD)
  PC_META="$STATE/$PC_ID.meta"
  PC_PROOF="$DATA/$PC_ID/moa-proof.md"
  PC_BRIEF="$DATA/$PC_ID/brief.md"
  mkdir -p "$DATA/$PC_ID"
  fm_write_meta "$PC_META" "kind=ship" "workflow=pstack" "branch=$PC_BRANCH" "worktree=$PC_WT"
}

# pc_write_brief: a brief whose intake block lists AC1..AC<n> (default 3).
pc_write_brief() {  # [count]
  local i n=${1:-3}
  mkdir -p "$DATA/$PC_ID"
  {
    printf '# Intake\n\n```moa-intake\ntitle: Fix the thing\nask: The thing is broken\nsource: pasted\nacceptance_criteria:\n'
    for i in $(seq 1 "$n"); do printf -- '- AC%s [confirmed]: criterion %s\n' "$i" "$i"; done
    printf '```\n\n# Task\n'
  } > "$PC_BRIEF"
}

# pc_write_proof: write a well-formed v2 proof for the current PC_* variables.
# A test varies one section by setting PC_CHECKS, PC_ACS, PC_AIREV, PC_SCOPE or
# PC_NOTRUN first; a variable set to the empty string empties that section.
pc_write_proof() {  # [task] [candidate] [base] [supersedes]
  local cand=${2:-$PC_CAND} nl=$'\n' checks acs airev scope notrun
  checks="| check | command | result | at |${nl}|---|---|---|---|${nl}| unit | bash tests/fixed.test.sh | pass | $cand |"
  acs="| id | criterion | evidence |${nl}|---|---|---|${nl}| AC1 | criterion 1 | tests/fixed.test.sh |${nl}| AC2 | criterion 2 | tests/fixed.test.sh |${nl}| AC3 | criterion 3 | tests/fixed.test.sh |"
  airev="- F1 (low) rename the helper fixed"
  scope="declared: fixed.txt${nl}actual: 1 file, +1 -0"
  notrun="The flaky timer path is not exercised."
  {
    printf 'moa-proof: v2\ntask: %s\nentry: pstack:poteto-mode\nplaybook: one-run-diagnosis\nbase: %s\ncandidate: %s\nsupersedes: %s\nbrief: data/%s/brief.md\nsource: pasted\nno-mistakes: pass\n' \
      "${1:-$PC_ID}" "${3:-$PC_BASE}" "$cand" "${4:-<40-hex>}" "$PC_ID"
    printf '## Reproduction or baseline\nReproduction run before the change failed with the observed trace.\n\n'
    printf '## Checks\n%s\n\n' "${PC_CHECKS-$checks}"
    printf '## Acceptance criteria\n%s\n\n' "${PC_ACS-$acs}"
    printf '## AI review\n%s\n\n' "${PC_AIREV-$airev}"
    printf '## Scope\n%s\n\n' "${PC_SCOPE-$scope}"
    printf '## Not run remaining uncertainty\n%s\n' "${PC_NOTRUN-$notrun}"
  } > "$PC_PROOF"
}

pc_run() {  # <task-id>: proof-check under the fixture home with the fake axi
  out=$(env FM_ROOT_OVERRIDE="$ROOT" FM_HOME="$HOME_DIR" \
    PATH="$FAKEBIN:$PATH" \
    "$ROOT/bin/fm-pstack.sh" proof-check "$1" 2>&1)
}

pc_orphan_commit() {  # make a non-ancestor orphan commit; echo its sha
  git -C "$PC_WT" checkout -q --orphan pc-orphan
  git -C "$PC_WT" rm -qrf --ignore-unmatch . >/dev/null 2>&1 || true
  printf 'orphan\n' > "$PC_WT/orphan.txt"
  git -C "$PC_WT" add orphan.txt
  git -C "$PC_WT" commit -qm 'orphan'
  git -C "$PC_WT" rev-parse HEAD
}

test_proof_check_happy_path() {
  set_cfg "$VALID_PLUGIN"
  pc_setup pc1 fm/pc1
  pstack_fake_axi "$FAKEBIN" "$PC_WT" "0 of 0 total"
  pc_write_brief
  pc_write_proof
  pc_run pc1
  expect_code 0 "$?" "happy-path proof-check"
  assert_contains "$out" "ok candidate=$PC_CAND" "happy path ok line"
  pass "well-formed proof anchored at HEAD passes"
}

test_proof_check_unverified_mutation_row_is_report_only() {
  set_cfg "$VALID_PLUGIN"
  pc_setup pc1 fm/pc1
  pstack_fake_axi "$FAKEBIN" "$PC_WT" "0 of 0 total"
  pc_write_brief
  PC_CHECKS=$(printf '| check | command | result | at |\n|---|---|---|---|\n| unit | bash tests/fixed.test.sh | pass | %s |\n| mutation | bin/fm-mutation-verdict.sh classified record | not verified: 1 survived | %s |' "$PC_CAND" "$PC_CAND") pc_write_proof
  pc_run pc1
  expect_code 0 "$?" "a not-verified mutation row must not fail proof-check"
  assert_contains "$out" "ok candidate=$PC_CAND" "a not-verified mutation row keeps the proof ok"
  pass "a not-verified mutation row is report-only and does not fail proof-check"
}

# pc_mutation_candidate [shell|mixed|docs]: rewrite the candidate commit in the fixture
# copy only. shell changes bin/fixed.sh, mixed adds docs/notes.md, docs changes only
# docs/notes.md. PC_CAND is updated to the new commit; PC_BASE is unchanged.
pc_mutation_candidate() {
  local kind=${1:-shell}
  git -C "$PC_WT" reset -q --hard "$PC_BASE"
  if [ "$kind" != docs ]; then
    mkdir -p "$PC_WT/bin"
    printf '%s\n' 'echo change' > "$PC_WT/bin/fixed.sh"
    git -C "$PC_WT" add bin/fixed.sh
  fi
  if [ "$kind" != shell ]; then
    mkdir -p "$PC_WT/docs"
    printf '%s\n' 'a note' > "$PC_WT/docs/notes.md"
    git -C "$PC_WT" add docs/notes.md
  fi
  git -C "$PC_WT" commit -qm 'the proof candidate'
  PC_CAND=$(git -C "$PC_WT" rev-parse HEAD)
}

# pc_mutation_case <id> [shell|mixed|docs]: a fresh ship task whose candidate has the
# given change and a 3-AC brief. The caller writes the proof with pc_write_proof or
# pc_mutation_row_proof.
pc_mutation_case() {
  local kind=${2:-shell} scope
  set_cfg "$VALID_PLUGIN"
  pc_setup "$1" "fm/$1"
  pstack_fake_axi "$FAKEBIN" "$PC_WT" "0 of 0 total"
  pc_write_brief
  pc_mutation_candidate "$kind"
  case "$kind" in
    shell) scope=$(printf 'declared: bin/fixed.sh\nactual: 1 file, +1 -0') ;;
    mixed) scope=$(printf 'declared: bin/fixed.sh, docs/notes.md\nactual: 2 files, +2 -0') ;;
    docs) scope=$(printf 'declared: docs/notes.md\nactual: 1 file, +1 -0') ;;
  esac
  PC_MUT_SCOPE=$scope
}

# pc_mutation_proof: the proof for the case's change, with the default Checks table, which
# has no mutation row. The scope is passed per call, so no global PC_SCOPE is left set.
pc_mutation_proof() {
  PC_SCOPE=$PC_MUT_SCOPE pc_write_proof
}

# pc_mutation_row_proof <command> <result> [<at>]: the case's proof with a passing unit row
# and one mutation row, at the candidate unless <at> is given.
pc_mutation_row_proof() {
  local checks
  checks=$(printf '| check | command | result | at |\n|---|---|---|---|\n| unit | bash tests/fixed.test.sh | pass | %s |\n| mutation | %s | %s | %s |' \
    "$PC_CAND" "$1" "$2" "${3:-$PC_CAND}")
  PC_SCOPE=$PC_MUT_SCOPE PC_CHECKS=$checks pc_write_proof
}

# pc_run_required <task-id>: proof-check with --require-mutation-row under the fixture
# home; the exit status is proof-check's.
pc_run_required() {
  out=$(env FM_ROOT_OVERRIDE="$ROOT" FM_HOME="$HOME_DIR" \
    PATH="$FAKEBIN:$PATH" \
    "$ROOT/bin/fm-pstack.sh" proof-check "$1" --require-mutation-row 2>&1)
}

MUT_CMD='bin/fm-mutation-verdict.sh classified record'
MUT_PASS='passed: 2 mutant(s) killed across 1 path(s); no survived, timeout, error, unresolved, unsupported, unclassified, or missing result'

test_proof_check_mutation_gate_is_report_only_by_default() {
  pc_mutation_case pc-mg0
  pc_mutation_proof
  pc_run pc-mg0
  expect_code 0 "$?" "an applicable change with no mutation row, without the flag"
  assert_contains "$out" "ok candidate=$PC_CAND" "report-only default keeps the proof ok"
  pass "without --require-mutation-row an applicable change with no mutation row still passes"
}

test_proof_check_mutation_gate_requires_a_row() {
  pc_mutation_case pc-mg1
  pc_mutation_proof
  pc_run_required pc-mg1
  expect_code 1 "$?" "an applicable change with no mutation row under the flag"
  assert_contains "$out" "error: the change has a mutation surface, so the Checks table needs one mutation row" \
    "a missing mutation row is named"
  pass "under --require-mutation-row an applicable change without a mutation row fails"
}

test_proof_check_mutation_gate_accepts_valid_rows_without_a_score_floor() {
  pc_mutation_case pc-mg2
  pc_mutation_row_proof "$MUT_CMD" "$MUT_PASS"
  pc_run_required pc-mg2
  expect_code 0 "$?" "a passed verdict row under the flag"
  assert_contains "$out" "ok candidate=$PC_CAND" "a passed verdict row keeps the proof ok"
  pc_mutation_row_proof "$MUT_CMD" "not verified: 1 survived"
  pc_run_required pc-mg2
  expect_code 0 "$?" "a survivor's not-verified row under the flag"
  assert_contains "$out" "ok candidate=$PC_CAND" "a valid not-verified row is accepted, with no score floor"
  pc_mutation_row_proof "$MUT_CMD" "not verified: 1 timeout; 1 error"
  pc_run_required pc-mg2
  expect_code 0 "$?" "a timeout or tool-error not-verified row under the flag"
  pass "under --require-mutation-row a valid passed or not-verified row is accepted, with no score floor"
}

test_proof_check_mutation_gate_refuses_stale_and_malformed_rows() {
  pc_mutation_case pc-mg3
  pc_mutation_row_proof "$MUT_CMD" "$MUT_PASS" "$PC_BASE"
  pc_run_required pc-mg3
  expect_code 1 "$?" "a stale mutation row under the flag"
  assert_contains "$out" "error: Checks table row for mutation shows 'at' $PC_BASE, not candidate $PC_CAND" \
    "a row for an older commit is stale"
  pc_mutation_row_proof "make mutation" "$MUT_PASS"
  pc_run_required pc-mg3
  expect_code 1 "$?" "a mutation row not written by the verdict"
  assert_contains "$out" "error: the mutation row's command is not bin/fm-mutation-verdict.sh" \
    "a row with another command is refused"
  pc_mutation_row_proof "$MUT_CMD" "looks fine"
  pc_run_required pc-mg3
  expect_code 1 "$?" "a mutation row whose result is not a verdict"
  assert_contains "$out" "error: the mutation row's result is not a verdict" "a row with a non-verdict result is refused"
  PC_SCOPE=$PC_MUT_SCOPE PC_CHECKS=$(printf '| check | command | result | at |\n|---|---|---|---|\n| unit | bash tests/fixed.test.sh | pass | %s |\n| mutation | %s | %s | %s |\n| mutation | %s | %s | %s |' \
    "$PC_CAND" "$MUT_CMD" "$MUT_PASS" "$PC_CAND" "$MUT_CMD" "not verified: 1 survived" "$PC_CAND") pc_write_proof
  pc_run_required pc-mg3
  expect_code 1 "$?" "two mutation rows under the flag"
  assert_contains "$out" "error: the Checks table has 2 mutation rows; it needs exactly one" "two mutation rows are refused"
  pass "under --require-mutation-row a stale, foreign or malformed mutation row fails"
}

test_proof_check_mutation_gate_refuses_a_classifier_failure() {
  set_cfg "$VALID_PLUGIN"
  pc_setup pc-mg4 fm/pc-mg4
  pstack_fake_axi "$FAKEBIN" "$PC_WT" "0 of 0 total"
  pc_write_brief
  pc_write_proof
  pc_run_required pc-mg4
  expect_code 1 "$?" "a changed path the classifier refuses under the flag"
  assert_contains "$out" "error: the mutation gate could not classify every changed path" \
    "a refused path stops the gate rather than passing as not applicable"
  pass "under --require-mutation-row a changed path the classifier refuses fails proof-check"
}

test_proof_check_mutation_gate_refuses_passed_over_an_unsupported_path() {
  pc_mutation_case pc-mg5 mixed
  pc_mutation_row_proof "$MUT_CMD" "$MUT_PASS"
  pc_run_required pc-mg5
  expect_code 1 "$?" "a passed row over a change with an unsupported path"
  assert_contains "$out" "error: the mutation row says passed, but 1 changed path(s) have no mutation surface" \
    "a passed verdict cannot hold for a change with an unsupported path"
  pc_mutation_row_proof "$MUT_CMD" "not verified: 1 path(s) without a mutation surface"
  pc_run_required pc-mg5
  expect_code 0 "$?" "the honest not-verified row over the same change"
  assert_contains "$out" "ok candidate=$PC_CAND" "an honest not-verified row over the same change is accepted"
  pass "a passed mutation row is refused when the change holds a path the verdict never passes"
}

test_proof_check_mutation_gate_skips_a_change_without_a_mutation_surface() {
  pc_mutation_case pc-mg6 docs
  pc_mutation_proof
  pc_run_required pc-mg6
  expect_code 0 "$?" "a documentation-only change under the flag"
  assert_contains "$out" "ok candidate=$PC_CAND" "a change with no mutate path needs no mutation row"
  pass "under --require-mutation-row a change with no mutation surface needs no mutation row"
}

test_proof_check_missing_meta_is_refusal() {
  mv "$PC_META" "$PC_META.keep"
  pc_run pc1
  expect_code 1 "$?" "missing meta exit code"
  assert_contains "$out" "next:" "missing meta carries no next line"
  mv "$PC_META.keep" "$PC_META"
  pass "proof-check without a task record refuses"
}

test_proof_check_missing_proof_is_refusal() {
  mv "$PC_PROOF" "$PC_PROOF.keep"
  pc_run pc1
  expect_code 1 "$?" "missing proof exit code"
  assert_contains "$out" "write it first with the exact template command" "missing proof next line"
  mv "$PC_PROOF.keep" "$PC_PROOF"
  pass "proof-check without a proof record refuses with the template pointer"
}

test_proof_check_meta_not_ship_or_not_pstack() {
  fm_write_meta "$PC_META" "kind=scan" "workflow=standard" "branch=$PC_BRANCH" "worktree=$PC_WT"
  pc_run pc1
  expect_code 1 "$?" "non-ship meta exit code"
  assert_contains "$out" "error: the task record for $PC_ID is not kind=ship" "non-ship meta error"
  assert_contains "$out" "error: the task record for $PC_ID does not carry workflow=pstack" "non-pstack meta error"
  fm_write_meta "$PC_META" "kind=ship" "workflow=pstack" "branch=$PC_BRANCH" "worktree=$PC_WT"
  pass "meta gates kind=ship and workflow=pstack"
}

test_proof_check_field_failures() {
  printf 'moa-proof: v2\ntask: other\nentry: notentry\nplaybook: \nbase: %s\ncandidate: nope\n' "$PC_BASE" > "$PC_PROOF"
  printf '## Reproduction or baseline\n## Checks\n## Acceptance criteria\n## AI review\n## Scope\n## Not run remaining uncertainty\n' >> "$PC_PROOF"
  pc_run pc1
  expect_code 1 "$?" "field failure exit code"
  # Every failure is listed in one round, one error: line each.
  for needle in \
    "the proof record's task field does not name $PC_ID" \
    'is not a <plugin>:<skill> entry' \
    'playbook field is missing' \
    'candidate is missing or not a full commit sha' \
    "the proof record's brief field is empty" \
    "the proof record's source field is empty" \
    "the proof record's no-mistakes field is empty" \
    '"## Reproduction or baseline" section is empty' \
    '"## Checks" section is empty' \
    '"## Acceptance criteria" section is empty' \
    '"## AI review" section is empty' \
    '"## Scope" section is empty' \
    '"## Not run remaining uncertainty" section is empty'; do
    assert_contains "$out" "$needle" "field failure batch missed: $needle"
  done
  pass "field failures are all listed in one round"
}

test_proof_check_git_anchoring_failures() {
  pc_write_proof
  # Candidate not HEAD: the proof names the base commit while HEAD is the fix.
  pc_write_proof "$PC_ID" "$PC_BASE" "$PC_BASE" "<40-hex>"
  pc_run pc1
  expect_code 1 "$?" "wrong candidate exit code"
  assert_contains "$out" "is not the worktree HEAD $PC_CAND" "candidate-not-head error"
  # Dirty tracked tree.
  pc_write_proof
  printf 'dirty\n' >> "$PC_WT/fixed.txt"
  pc_run pc1
  assert_contains "$out" "tracked uncommitted changes" "dirty worktree error"
  git -C "$PC_WT" checkout -q -- fixed.txt
  # Off the ship branch entirely.
  git -C "$PC_WT" checkout -q --detach HEAD~0
  pc_run pc1
  assert_contains "$out" "not on the ship branch $PC_BRANCH" "off-branch error"
  git -C "$PC_WT" checkout -q "$PC_BRANCH"
  pass "git anchoring failures are reported"
}

test_proof_check_base_and_supersedes_rules() {
  # base == candidate is never a baseline.
  pc_write_proof "$PC_ID" "$PC_CAND" "$PC_CAND" "<40-hex>"
  pc_run pc1
  assert_contains "$out" "a baseline is a strict ancestor" "base==candidate error"
  # base not a commit this copy has.
  pc_write_proof "$PC_ID" "$PC_CAND" "0000000000000000000000000000000000000000" "<40-hex>"
  pc_run pc1
  assert_contains "$out" "not a commit this copy has" "unreachable base error"
  # supersedes pointing at an ancestor commit is a contradiction.
  pc_write_proof "$PC_ID" "$PC_CAND" "$PC_BASE" "$PC_BASE"
  pc_run pc1
  assert_contains "$out" "supersedes sha $PC_BASE is an ancestor of the candidate" "supersedes-ancestor error"
  # supersedes pointing at a discarded (non-ancestor) commit is accepted.
  PC_SUP=$(pc_orphan_commit)
  git -C "$PC_WT" checkout -q "$PC_BRANCH"
  pc_write_proof "$PC_ID" "$PC_CAND" "$PC_BASE" "$PC_SUP"
  pc_run pc1
  assert_contains "$out" "ok candidate=$PC_CAND" "non-ancestor supersedes was refused"
  # supersedes not a commit at all.
  pc_write_proof "$PC_ID" "$PC_CAND" "$PC_BASE" "1234567890123456789012345678901234567890"
  pc_run pc1
  assert_contains "$out" "supersedes sha 1234567890123456789012345678901234567890 is not a commit this copy has" "unreachable supersedes error"
  pass "base and supersedes rules hold end to end"
}

test_proof_check_run_state_gate() {
  pc_write_proof
  # An active run must fail.
  pstack_fake_axi "$FAKEBIN" "$PC_WT" "1 of 1 total" \
    "r-1,$PC_BRANCH,pending,$PC_CAND,\"\""
  pc_run pc1
  expect_code 1 "$?" "active run exit code"
  assert_contains "$out" "already active on branch $PC_BRANCH" "active-run error"
  # A terminal run is fine.
  pstack_fake_axi "$FAKEBIN" "$PC_WT" "1 of 1 total" \
    "r-1,$PC_BRANCH,completed,$PC_CAND,\"\""
  pc_run pc1
  assert_contains "$out" "ok candidate=$PC_CAND" "terminal run was not accepted"
  # A garbage overview fails closed with a next line.
  cat > "$FAKEBIN/no-mistakes" <<'SH'
#!/bin/bash
case "$1" in
  axi) printf 'garbage output\n' ;;
esac
SH
  chmod +x "$FAKEBIN/no-mistakes"
  pc_run pc1
  expect_code 1 "$?" "garbage overview exit code"
  assert_contains "$out" "no-mistakes run state for branch $PC_BRANCH could not be read" "garbage overview error"
  assert_contains "$out" "next:" "garbage overview next line"
  # A missing binary fails closed too.
  rm -f "$FAKEBIN/no-mistakes"
  pc_run pc1
  expect_code 1 "$?" "missing binary exit code"
  assert_contains "$out" "the axi overview failed or was empty" "missing binary error"
  assert_contains "$out" "next:" "missing binary next line"
  pass "run-state gate passes terminal, refuses active, fails closed on unreadable"
}

test_proof_check_usage_id_shape() {
  pc_run pp/1
  expect_code 2 "$?" "slash id exit code"
  pc_run ''
  expect_code 2 "$?" "empty id exit code"
  pass "proof-check rejects ids that cannot name a record"
}

# pc_case <id>: a fresh ship task with a clean fake axi, a 3-AC brief and a valid
# v2 proof, ready for one variation.
pc_case() {
  set_cfg "$VALID_PLUGIN"
  pc_setup "$1" "fm/$1"
  pstack_fake_axi "$FAKEBIN" "$PC_WT" "0 of 0 total"
  pc_write_brief
  pc_write_proof
}

pc_expect_ok() {  # <message>
  pc_run "$PC_ID"
  expect_code 0 "$?" "$1 exit code"
  assert_contains "$out" "ok candidate=$PC_CAND" "$1"
}

pc_expect_fail() {  # <error text> <message>
  pc_run "$PC_ID"
  expect_code 1 "$?" "$2 exit code"
  assert_contains "$out" "error: $1" "$2"
}

# pc_set_header <line>: replace the proof's opening format line.
pc_set_header() {
  { printf '%s\n' "$1"; sed '1d' "$PC_PROOF"; } > "$PC_PROOF.new" && mv "$PC_PROOF.new" "$PC_PROOF"
}

# pc_set_no_mistakes <value>: replace the proof's no-mistakes field.
pc_set_no_mistakes() {
  sed "s/^no-mistakes: .*/no-mistakes: $1/" "$PC_PROOF" > "$PC_PROOF.new" && mv "$PC_PROOF.new" "$PC_PROOF"
}

# The no-mistakes field carries one of the spec labels, so a free-form value such
# as the old yes/no placeholder is refused rather than recorded as a verdict.
test_proof_no_mistakes_label_set() {
  local label
  pc_case pv-nml
  for label in "pass" "failed" "off" "skipped: unverified on copilot"; do
    pc_set_no_mistakes "$label"
    pc_expect_ok "no-mistakes: $label is a spec label"
  done
  for label in "yes" "passed" "skipped"; do
    pc_set_no_mistakes "$label"
    pc_expect_fail "the proof record's no-mistakes field must be pass, failed, skipped: <reason>, or off (got: $label)" "no-mistakes: $label must be refused"
  done
  pc_set_no_mistakes "skipped:"
  pc_expect_fail "the proof record's no-mistakes field says skipped with no reason" "a skipped label needs a reason"
  pc_set_no_mistakes "<pass|failed|skipped: <reason>|off>"
  pc_expect_fail "the proof record's no-mistakes field is an unfilled placeholder" "the template placeholder must be filled before proof-check passes"
  pass "proof-check accepts only the spec no-mistakes labels"
}

test_proof_v2_header_format() {
  pc_case pv-hdr
  pc_expect_ok "a moa-proof: v2 header passes"
  pc_set_header "pstack-proof: v1"
  pc_expect_fail "the proof record does not open with moa-proof: v2" "a pstack-proof: v1 record is rejected"
  pc_set_header "moa-proof: v1"
  pc_expect_fail "the proof record does not open with moa-proof: v2" "a moa-proof: v1 record is rejected"
  pass "only a moa-proof: v2 record is accepted"
}

test_proof_v2_brief_must_exist() {
  pc_case pv-brief
  rm -f "$PC_BRIEF"
  pc_expect_fail "the proof record's brief data/pv-brief/brief.md does not exist" "a missing brief is refused"
  pass "proof-check refuses a record whose brief file is missing"
}

test_proof_v2_ac_coverage() {
  pc_case pv-ac
  pc_expect_ok "all three brief criteria in the table pass"
  PC_ACS=$'| id | criterion | evidence |\n|---|---|---|\n| AC1 | c1 | t |\n| AC3 | c3 | t |' pc_write_proof
  pc_expect_fail "acceptance criterion AC2 from brief is not in the proof record's AC table" "a missing criterion is named"
  PC_ACS=$'| id | criterion | evidence |\n|---|---|---|\n| ac1 | c1 | t |\n| AC2 | c2 | t |\n| AC3 | c3 | t |' pc_write_proof
  pc_expect_fail "acceptance criterion AC1 from brief is not in the proof record's AC table" "a wrong-format id does not count"
  # AC10 must not satisfy AC1.
  pc_write_brief 10
  PC_ACS=$'| id | criterion | evidence |\n|---|---|---|\n| AC2 | c | t |\n| AC3 | c | t |\n| AC4 | c | t |\n| AC5 | c | t |\n| AC6 | c | t |\n| AC7 | c | t |\n| AC8 | c | t |\n| AC9 | c | t |\n| AC10 | c | t |' pc_write_proof
  pc_expect_fail "acceptance criterion AC1 from brief is not in the proof record's AC table" "AC10 does not stand in for AC1"
  # A gap in the brief's ids cannot be traced.
  # shellcheck disable=SC2016  # the backticks are the literal intake fence
  printf '```moa-intake\nacceptance_criteria:\n- AC1 [confirmed]: a\n- AC3 [confirmed]: c\n```\n' > "$PC_BRIEF"
  pc_write_proof
  pc_expect_fail "the brief's acceptance criteria cannot be read: acceptance criteria ids are not sequential from AC1" "a gap in brief ids is refused"
  pass "every brief criterion must have its own row in the proof table"
}

test_proof_v2_not_verified_echo() {
  pc_case pv-nv
  local table=$'| id | criterion | evidence |\n|---|---|---|\n| AC1 | c1 | t |\n| AC2 | c2 | t |\n| AC3 | c3 | NOT VERIFIED |'
  PC_ACS=$table PC_NOTRUN="AC3 needs a live device, which this host lacks." pc_write_proof
  pc_expect_ok "a NOT VERIFIED criterion named in Not run does not block"
  PC_ACS=$table pc_write_proof
  pc_expect_fail "acceptance criterion AC3 is marked NOT VERIFIED but not mentioned in Not run" "an unechoed NOT VERIFIED fails"
  # AC30 in Not run must not echo AC3.
  PC_ACS=$table PC_NOTRUN="AC30 is not covered." pc_write_proof
  pc_expect_fail "acceptance criterion AC3 is marked NOT VERIFIED but not mentioned in Not run" "AC30 does not echo AC3"
  pass "NOT VERIFIED must be echoed in Not run, and never blocks by itself"
}

test_proof_v2_checks_table() {
  pc_case pv-ck
  pc_expect_ok "all rows at the candidate pass"
  local hdr=$'| check | command | result | at |\n|---|---|---|---|\n'
  PC_CHECKS="${hdr}| unit | bash t.sh | pass | $PC_CAND |"$'\n'"| lint | shellcheck x | pass | $PC_BASE |" pc_write_proof
  pc_expect_fail "Checks table row for lint shows 'at' $PC_BASE, not candidate $PC_CAND" "a row recorded at base fails"
  PC_CHECKS="${hdr}| unit |  | pass | $PC_CAND |" pc_write_proof
  pc_expect_fail "Checks table row for unit has empty command" "an empty command fails"
  PC_CHECKS="${hdr}| unit | bash t.sh |  | $PC_CAND |" pc_write_proof
  pc_expect_fail "Checks table row for unit has empty result" "an empty result fails"
  PC_CHECKS="${hdr}| unit | bash t.sh |  pass |  |" pc_write_proof
  pc_expect_fail "Checks table row for unit shows 'at' , not candidate $PC_CAND" "an empty at fails"
  PC_CHECKS="${hdr}| unit | cat a | grep b | pass | $PC_CAND |" pc_write_proof
  pc_expect_ok "a command containing a pipe is read correctly"
  pass "every Checks row needs a name, command, result and the candidate sha"
}

test_proof_v2_scope() {
  pc_case pv-sc
  pc_expect_ok "scope counts matching git diff pass"
  PC_SCOPE=$'declared: fixed.txt\nactual: 5 files, +1 -0' pc_write_proof
  pc_expect_fail "Scope section reports 5 files, but git diff shows 1" "a wrong file count fails"
  PC_SCOPE=$'declared: fixed.txt\nactual: 1 file, +100 -20' pc_write_proof
  pc_expect_fail "Scope section reports +100 lines, but git diff shows +1" "wrong added lines fail"
  assert_contains "$out" "error: Scope section reports -20 lines, but git diff shows -0" "wrong removed lines fail"
  PC_SCOPE="declared: fixed.txt" pc_write_proof
  pc_expect_fail "the Scope section needs an 'actual:' line" "a missing actual line fails"
  pass "the Scope actual line must match git diff --numstat base..candidate"
}

test_proof_v2_ai_review_disposition() {
  pc_case pv-ai
  PC_AIREV=$'- F1 (low) rename the helper fixed\n- F2 (med) extra loop rejected: not in scope\n- F3 (low) docs gap deferred: DPES-1234' pc_write_proof
  pc_expect_ok "fixed, rejected with a reason and deferred with a link pass"
  PC_AIREV="none" pc_write_proof
  pc_expect_ok "a review with no findings passes"
  PC_AIREV=$'- F1 (low) rename the helper fixed\n- F2 (high) null deref in the parser' pc_write_proof
  pc_expect_fail "AI review finding does not have a disposition: - F2 (high) null deref in the parser" "a finding without a disposition fails"
  PC_AIREV="- F1 (low) typo resolved:" pc_write_proof
  pc_expect_fail "AI review finding does not have a disposition: - F1 (low) typo resolved:" "a misspelled disposition fails"
  PC_AIREV="- F1 (low) typo rejected:" pc_write_proof
  pc_expect_fail "AI review finding does not have a disposition: - F1 (low) typo rejected:" "rejected without a reason fails"
  PC_AIREV="- F1 (low) typo deferred:" pc_write_proof
  pc_expect_fail "AI review finding does not have a disposition: - F1 (low) typo deferred:" "deferred without a link fails"
  pass "every AI review finding ends in fixed, rejected: <reason> or deferred: <link>"
}

# --- render ------------------------------------------------------------------

RD_CAND=0123456789abcdef0123456789abcdef01234567
RD_BASE=1111111111111111111111111111111111111111

# rd_write <id>: write a complete v2 record under $DATA/<id>. A test varies one
# part by setting RD_SOURCE, RD_NM (the whole no-mistakes line, so it can be
# empty or absent), RD_CHECKS, RD_ACS, RD_AIREV, RD_SCOPE or RD_NOTRUN first; a
# variable set to the empty string empties that part.
rd_write() {
  local id=$1 nl=$'\n' checks acs airev scope notrun
  checks="| check | command | result | at |${nl}|---|---|---|---|${nl}| tests | bash t.sh | pass: 214 passed, 0 failed | $RD_CAND |${nl}| lint | shellcheck x | clean | $RD_CAND |${nl}| coverage | cov | 91% | $RD_CAND |"
  acs="| id | criterion | evidence |${nl}|---|---|---|${nl}| AC1 | first | tests/a.test.sh |${nl}| AC2 | second | tests/b.test.sh |${nl}| AC3 | third | NOT VERIFIED |"
  airev="- F1 (low) rename the helper fixed${nl}- F2 (high) CSV cell injection fixed${nl}- F3 (med) extra loop rejected: out of scope${nl}- F4 (low) docs gap deferred: DPES-1240"
  scope="declared: lib/${nl}actual: 6 files, +212 -9"
  notrun="- AC3 needs a live device.${nl}Risk: the permission check reuses the cache."
  mkdir -p "$DATA/$id"
  {
    printf 'moa-proof: v2\ntask: %s\nentry: pstack:poteto-mode\nplaybook: one-run-diagnosis\nbase: %s\ncandidate: %s\nbrief: data/%s/brief.md\nsource: %s\n' \
      "$id" "$RD_BASE" "$RD_CAND" "$id" "${RD_SOURCE-https://github.com/acme/widgets/issues/42}"
    printf '%s\n' "${RD_NM-no-mistakes: pass}"
    printf '## Reproduction or baseline\nfailed before.\n\n'
    printf '## Checks\n%s\n\n' "${RD_CHECKS-$checks}"
    printf '## Acceptance criteria\n%s\n\n' "${RD_ACS-$acs}"
    printf '## AI review\n%s\n\n' "${RD_AIREV-$airev}"
    printf '## Scope\n%s\n\n' "${RD_SCOPE-$scope}"
    printf '## Not run remaining uncertainty\n%s\n' "${RD_NOTRUN-$notrun}"
  } > "$DATA/$id/moa-proof.md"
}

# rd_render <id>: run render, leaving stdout in $out (never mixed with stderr)
# and the exit code in $rc.
rd_render() {
  out=$(env FM_ROOT_OVERRIDE="$ROOT" FM_HOME="$HOME_DIR" \
    "$ROOT/bin/fm-pstack.sh" render "${1-}" 2>"$TMP_ROOT/render.err")
  rc=$?
}

# rd_lines <text> <pattern>: how many lines of <text> are exactly <pattern>.
rd_lines() {
  printf '%s\n' "$1" | grep -cFx -- "$2" || true
}

test_render_golden_body() {
  rd_write rd-gold
  env FM_ROOT_OVERRIDE="$ROOT" FM_HOME="$HOME_DIR" "$ROOT/bin/fm-pstack.sh" render rd-gold > "$TMP_ROOT/rd-gold.body"
  expect_code 0 "$?" "render of a complete record"
  cat > "$TMP_ROOT/rd-gold.expected" <<'EOF'
**Verified locally at `0123456`** · 2 of 3 acceptance criteria proven · 1 not verified · no-mistakes: pass
Source: [#42](https://github.com/acme/widgets/issues/42) · Proof record: `data/rd-gold/moa-proof.md` (attached)
Refs acme/widgets#42

### Read first
- ⚠️ **AC3 not verified:** AC3 needs a live device.
- ⚠️ Risk: the permission check reuses the cache.

### Acceptance criteria
| | criterion | evidence |
|---|---|---|
| ✅ | AC1 first | tests/a.test.sh |
| ✅ | AC2 second | tests/b.test.sh |
| ❌ | AC3 third | not verified |

### Checks (all at `0123456`)
✅ tests pass: 214 passed, 0 failed · ✅ lint clean · coverage 91%

### AI review (fresh-session reviewer)
4 findings: 2 fixed, 1 rejected, 1 deferred (DPES-1240). High finding CSV cell injection, fixed.

### Scope
6 files, +212 −9. All inside brief's declared paths.

<details><summary>Full proof record</summary>

(Attached as separate PR comment.)

</details>
EOF
  cmp -s "$TMP_ROOT/rd-gold.expected" "$TMP_ROOT/rd-gold.body" \
    || fail "rendered body differs from the golden body:"$'\n'"$(diff "$TMP_ROOT/rd-gold.expected" "$TMP_ROOT/rd-gold.body")"
  pass "render produces the golden body byte for byte"
}

test_render_is_deterministic() {
  rd_write rd-det
  local a="$TMP_ROOT/rd-det.a" b="$TMP_ROOT/rd-det.b" c="$TMP_ROOT/rd-det.c"
  env FM_ROOT_OVERRIDE="$ROOT" FM_HOME="$HOME_DIR" "$ROOT/bin/fm-pstack.sh" render rd-det > "$a"
  # A different clock, zone, locale and working directory must not leak in.
  (cd "$TMP_ROOT" && env FM_ROOT_OVERRIDE="$ROOT" FM_HOME="$HOME_DIR" TZ=Pacific/Auckland LC_ALL=en_US.UTF-8 LANG=en_US.UTF-8 \
    "$ROOT/bin/fm-pstack.sh" render rd-det > "$b")
  env FM_ROOT_OVERRIDE="$ROOT" FM_HOME="$HOME_DIR" TZ=UTC LC_ALL=C "$ROOT/bin/fm-pstack.sh" render rd-det > "$c"
  [ -s "$a" ] || fail "render printed nothing"
  cmp -s "$a" "$b" || fail "render output changed with the locale, zone or directory"
  cmp -s "$a" "$c" || fail "render output changed between runs"
  # Output changes only when the record changes.
  RD_SCOPE=$'declared: lib/\nactual: 7 files, +212 -9' rd_write rd-det
  env FM_ROOT_OVERRIDE="$ROOT" FM_HOME="$HOME_DIR" "$ROOT/bin/fm-pstack.sh" render rd-det > "$c"
  cmp -s "$a" "$c" && fail "a changed record rendered the same body"
  pass "the same record renders byte-identical bodies regardless of environment"
}

test_render_verdict_and_source_lines() {
  RD_ACS=$'| id | criterion | evidence |\n|---|---|---|\n| AC1 | a | t |\n| AC2 | b | t |\n| AC3 | c | NOT VERIFIED |' RD_NOTRUN="AC3 is not covered." rd_write rd-v
  rd_render rd-v
  assert_contains "$out" "· 2 of 3 acceptance criteria proven · 1 not verified · no-mistakes: pass" "2 of 3 verdict"
  RD_ACS='' rd_write rd-v
  rd_render rd-v
  expect_code 0 "$rc" "a record with no criteria still renders"
  assert_contains "$out" "· 0 of 0 acceptance criteria proven · 0 not verified ·" "0 of 0 verdict"
  RD_NM="no-mistakes:" rd_write rd-v
  rd_render rd-v
  assert_contains "$out" "· no-mistakes: off" "an empty no-mistakes field renders off"
  RD_NM="skipped-line: x" rd_write rd-v
  rd_render rd-v
  assert_contains "$out" "· no-mistakes: off" "an absent no-mistakes field renders off"
  RD_NM="no-mistakes: skipped: unverified on copilot" rd_write rd-v
  rd_render rd-v
  assert_contains "$out" "· no-mistakes: skipped: unverified on copilot" "a no-mistakes status is quoted whole"
  assert_contains "$out" "Verified locally at \`0123456\`" "the candidate is shortened to 7 characters"
  pass "the verdict line carries the sha, the AC counts and the no-mistakes status"
}

test_render_source_forms_and_refs_line() {
  RD_SOURCE=pasted rd_write rd-s
  rd_render rd-s
  assert_contains "$out" "Source: pasted · Proof record: \`data/rd-s/moa-proof.md\` (attached)" "pasted source"
  assert_not_contains "$out" "Refs " "a pasted source gets no Refs line"
  RD_SOURCE=https://mccdh.atlassian.net/browse/DPES-1234 rd_write rd-s
  rd_render rd-s
  assert_contains "$out" "Source: [DPES-1234](https://mccdh.atlassian.net/browse/DPES-1234) · Proof record:" "Jira source"
  assert_not_contains "$out" "Refs " "a Jira source gets no Refs line"
  RD_SOURCE=https://github.com/owner/repo/issues/42 rd_write rd-s
  rd_render rd-s
  assert_contains "$out" "Source: [#42](https://github.com/owner/repo/issues/42) · Proof record:" "GitHub Issue source"
  assert_equals 1 "$(rd_lines "$out" "Refs owner/repo#42")" "a GitHub Issue source gets exactly one full-form Refs line"
  # The Refs line directly follows the source line and precedes Read first.
  local refs_at src_at read_at
  refs_at=$(printf '%s\n' "$out" | grep -n '^Refs ' | cut -d: -f1)
  src_at=$(printf '%s\n' "$out" | grep -n '^Source: ' | cut -d: -f1)
  read_at=$(printf '%s\n' "$out" | grep -n '^### Read first' | cut -d: -f1)
  assert_equals "$((src_at + 1))" "$refs_at" "the Refs line is directly after the source line"
  [ "$refs_at" -lt "$read_at" ] || fail "the Refs line is not before Read first"
  RD_SOURCE=https://github.com/o-w_n/r.e-p_o/issues/7 rd_write rd-s
  rd_render rd-s
  assert_equals 1 "$(rd_lines "$out" "Refs o-w_n/r.e-p_o#7")" "owner and repo with punctuation are kept whole"
  RD_SOURCE=https://dev.azure.com/org/proj/_workitems/edit/9 rd_write rd-s
  rd_render rd-s
  assert_contains "$out" "Source: [https://dev.azure.com/org/proj/_workitems/edit/9](https://dev.azure.com/org/proj/_workitems/edit/9) · Proof" "an Azure DevOps URL is linked by itself"
  assert_not_contains "$out" "Refs " "an Azure DevOps source gets no Refs line"
  RD_SOURCE=https://github.com/owner/repo/pull/42 rd_write rd-s
  rd_render rd-s
  assert_not_contains "$out" "Refs " "a GitHub PR URL is not an Issue source"
  pass "source forms render as specified and only a GitHub Issue gets the full-form Refs line"
}

test_render_read_first_section() {
  RD_ACS=$'| id | criterion | evidence |\n|---|---|---|\n| AC1 | a | t |' RD_NOTRUN="Nothing is left." rd_write rd-r
  rd_render rd-r
  assert_not_contains "$out" "Read first" "no unverified AC and no risk omits Read first"
  RD_ACS=$'| id | criterion | evidence |\n|---|---|---|\n| AC1 | a | t |\n| AC2 | b | NOT VERIFIED |' RD_NOTRUN="- AC2 needs a device." rd_write rd-r
  rd_render rd-r
  assert_equals 1 "$(rd_lines "$out" "- ⚠️ **AC2 not verified:** AC2 needs a device.")" "an unverified AC is a warning bullet"
  assert_not_contains "$out" "Risk" "no risk line, no risk bullet"
  RD_ACS=$'| id | criterion | evidence |\n|---|---|---|\n| AC1 | a | t |' RD_NOTRUN=$'Plain note.\n- Risk: the cache is shared\nrisk: lower-case also counts' rd_write rd-r
  rd_render rd-r
  assert_equals 1 "$(rd_lines "$out" "### Read first")" "a risk alone opens Read first"
  assert_equals 1 "$(rd_lines "$out" "- ⚠️ Risk: the cache is shared")" "a bulleted Risk line is quoted"
  assert_equals 1 "$(rd_lines "$out" "- ⚠️ risk: lower-case also counts")" "a Risk line without a bullet is quoted"
  assert_not_contains "$out" "Plain note" "other Not run text stays out of Read first"
  # AC1 must not pick up the Not run line that names AC10.
  RD_ACS=$'| id | criterion | evidence |\n|---|---|---|\n| AC1 | a | NOT VERIFIED |' RD_NOTRUN="AC10 is elsewhere." rd_write rd-r
  rd_render rd-r
  assert_contains "$out" "**AC1 not verified:** NOT VERIFIED" "an unmentioned AC falls back to its own evidence text"
  pass "Read first appears only for unverified criteria or Risk lines, and quotes them"
}

test_render_acceptance_table_escaping() {
  RD_ACS=$'| id | criterion | evidence |\n|---|---|---|\n| AC1 | accepts a \\| b | cat x \\| grep y |\n| AC2 | has | bare | pipes | t |' rd_write rd-a
  rd_render rd-a
  assert_equals 1 "$(rd_lines "$out" '| ✅ | AC1 accepts a \| b | cat x \| grep y |')" "an escaped pipe stays escaped in its cell"
  assert_equals 1 "$(rd_lines "$out" '| ✅ | AC2 has\|bare\|pipes | t |')" "bare pipes in the criterion are escaped so the table keeps three columns"
  # A record whose table has no separator row has no header to skip.
  RD_ACS=$'| AC1 | only | t |' rd_write rd-a
  rd_render rd-a
  assert_equals 1 "$(rd_lines "$out" '| ✅ | AC1 only | t |')" "a headerless table keeps its first row"
  assert_contains "$out" "· 1 of 1 acceptance criteria proven" "a headerless table counts its first row"
  pass "criterion and evidence cells cannot break the rendered table"
}

test_render_checks_line() {
  rd_write rd-c
  rd_render rd-c
  assert_equals 1 "$(rd_lines "$out" "✅ tests pass: 214 passed, 0 failed · ✅ lint clean · coverage 91%")" "checks render on one inline line"
  RD_CHECKS=$'| check | command | result | at |\n|---|---|---|---|\n| unit | cat a | grep b | pass | x |\n| lint | shellcheck x | failed: 2 errors | x |\n| types | tsc | 0 failed | x |' rd_write rd-c
  rd_render rd-c
  assert_equals 1 "$(rd_lines "$out" "✅ unit pass · ❌ lint failed: 2 errors · types 0 failed")" "a pipe in the command is read from both ends and an unrecognized result gets no icon"
  RD_CHECKS=$'| check | command | result | at |\n|---|---|---|---|\n| a | c | passport check | x |\n| b | c | OK | x |' rd_write rd-c
  rd_render rd-c
  assert_equals 1 "$(rd_lines "$out" "a passport check · ✅ b OK")" "pass must be a whole word to count as a pass"
  RD_CHECKS='' rd_write rd-c
  rd_render rd-c
  assert_equals 1 "$(rd_lines "$out" "No checks recorded.")" "an empty Checks section says so"
  pass "the Checks section renders inline, without crediting an unrecognized result"
}

test_render_mutation_row() {
  local mut_pass="passed: 1 mutant(s) killed across 1 path(s); no survived, timeout, error, unresolved, unsupported, or missing result"
  RD_CHECKS=$(printf '| check | command | result | at |\n|---|---|---|---|\n| unit | bash t.sh | pass | %s |\n| mutation | bin/fm-mutation-verdict.sh classified record | %s | %s |' "$RD_CAND" "$mut_pass" "$RD_CAND") rd_write rd-m
  rd_render rd-m
  assert_equals 1 "$(rd_lines "$out" "✅ unit pass · ✅ mutation $mut_pass")" "a verified mutation row renders with a pass icon"
  RD_CHECKS=$(printf '| check | command | result | at |\n|---|---|---|---|\n| unit | bash t.sh | pass | %s |\n| mutation | bin/fm-mutation-verdict.sh classified record | not verified: 1 survived | %s |' "$RD_CAND" "$RD_CAND") rd_write rd-m
  rd_render rd-m
  assert_equals 1 "$(rd_lines "$out" "✅ unit pass · mutation not verified: 1 survived")" "a not-verified mutation row renders with no pass icon"
  RD_CHECKS=$(printf '| check | command | result | at |\n|---|---|---|---|\n| unit | bash t.sh | pass | %s |\n| mutation | bin/fm-mutation-verdict.sh classified record | looks fine | %s |' "$RD_CAND" "$RD_CAND") rd_write rd-m
  rd_render rd-m
  assert_equals 1 "$(rd_lines "$out" "✅ unit pass · mutation looks fine")" "a malformed mutation row renders with no pass icon"
  pass "the mutation verdict row renders as a pass only when verified"
}

test_render_ai_review_line() {
  rd_write rd-i
  rd_render rd-i
  assert_equals 1 "$(rd_lines "$out" "4 findings: 2 fixed, 1 rejected, 1 deferred (DPES-1240). High finding CSV cell injection, fixed.")" "4 findings with tally, link and highest severity"
  RD_AIREV=$'- F1 (low) a fixed\n- F2 (low) b deferred: DPES-1\n- F3 (low) c deferred: DPES-1\n- F4 (med) d deferred: https://x.test/2' rd_write rd-i
  rd_render rd-i
  assert_equals 1 "$(rd_lines "$out" "4 findings: 1 fixed, 0 rejected, 3 deferred (DPES-1, https://x.test/2). Med finding d, deferred.")" "tracking links are listed once each, in order"
  RD_AIREV=$'- F1 (high) first fixed\n- F2 (high) second fixed' rd_write rd-i
  rd_render rd-i
  assert_contains "$out" "High finding first, fixed." "the first finding wins a severity tie"
  RD_AIREV=$'- F1 (high) strange case rejected: it is by design' rd_write rd-i
  rd_render rd-i
  assert_equals 1 "$(rd_lines "$out" "1 findings: 0 fixed, 1 rejected, 0 deferred. High finding strange case, rejected.")" "a rejected finding is quoted without its reason"
  RD_AIREV=none rd_write rd-i
  rd_render rd-i
  assert_equals 1 "$(rd_lines "$out" "0 findings.")" "a review with no findings says so"
  pass "the AI review line tallies dispositions and quotes the highest-severity finding"
}

test_render_scope_line() {
  rd_write rd-sc
  rd_render rd-sc
  assert_equals 1 "$(rd_lines "$out" "6 files, +212 −9. All inside brief's declared paths.")" "scope numbers come from the actual line"
  RD_SCOPE=$'declared: a\nactual: 1 file, +1 -0' rd_write rd-sc
  rd_render rd-sc
  assert_contains "$out" "1 file, +1 −0. All inside" "a single file is not pluralized"
  RD_SCOPE=$'declared: a\nactual: 3 files, +4 -5\noutside-scope: docs/x.md (renamed a heading)' rd_write rd-sc
  rd_render rd-sc
  assert_equals 1 "$(rd_lines "$out" "3 files, +4 −5. Outside-scope changes: docs/x.md (renamed a heading)")" "outside-scope changes are appended verbatim"
  RD_SCOPE=$'declared: a\nactual: 3 files, +4 -5\noutside-scope: none' rd_write rd-sc
  rd_render rd-sc
  assert_contains "$out" "3 files, +4 −5. All inside brief's declared paths." "outside-scope: none means all inside"
  pass "the Scope line reports the actual counts and any outside-scope changes"
}

test_render_refusals() {
  rd_render rd-missing
  expect_code 1 "$rc" "render of a missing record"
  assert_contains "$out" "error: no proof record at $DATA/rd-missing/moa-proof.md" "missing record error"
  assert_contains "$out" "next: " "missing record next line"
  rd_write rd-bad
  sed -i.bak '/^candidate:/d' "$DATA/rd-bad/moa-proof.md"
  rd_render rd-bad
  expect_code 1 "$rc" "render of a record without a candidate"
  assert_contains "$out" "error: the proof record has no candidate field to render" "missing candidate error"
  assert_not_contains "$out" "Verified locally" "a record that cannot render leaves no partial body"
  RD_SOURCE='' rd_write rd-bad
  rd_render rd-bad
  expect_code 1 "$rc" "render of a record without a source"
  assert_contains "$out" "error: the proof record has no source field to render" "missing source error"
  rd_render
  expect_code 2 "$rc" "render without a task id"
  assert_contains "$out" "usage: fm-pstack.sh render <task-id>" "render usage"
  rd_render ../x
  expect_code 2 "$rc" "render of an id that cannot name a record"
  pass "render refuses a missing or unrenderable record without printing a partial body"
}

# --- proof-check --check-body and --check-comment ----------------------------

# pc_render_to <file>: the exact body `render` prints for the current task.
pc_render_to() {
  env FM_ROOT_OVERRIDE="$ROOT" FM_HOME="$HOME_DIR" "$ROOT/bin/fm-pstack.sh" render "$PC_ID" > "$1"
}

# pc_run_args <task-id> <args...>: proof-check with extra flags.
pc_run_args() {
  local id=$1
  shift
  out=$(env FM_ROOT_OVERRIDE="$ROOT" FM_HOME="$HOME_DIR" PATH="$FAKEBIN:$PATH" \
    "$ROOT/bin/fm-pstack.sh" proof-check "$id" "$@" 2>&1)
}

# pc_rerun_case <task-id> <row>...: pc_case, then a proof whose Checks table has
# one row per argument, each `check | command | result` cell text, anchored at
# the candidate.
pc_rerun_case() {
  local id=$1 row table nl=$'\n'
  shift
  pc_case "$id"
  table="| check | command | result | at |${nl}|---|---|---|---|"
  for row in "$@"; do
    table="${table}${nl}| ${row} | ${PC_CAND} |"
  done
  PC_CHECKS=$table pc_write_proof
}

test_proof_check_body_equality() {
  pc_case pb-body
  local body="$PC_PROOF.body"
  pc_render_to "$body"
  pc_run_args "$PC_ID" --check-body "$body"
  expect_code 0 "$?" "a body equal to render(record)"
  assert_contains "$out" "ok candidate=$PC_CAND" "matching body passes"
  # Not passing the flag skips the check: the body file does not even need to exist.
  rm -f "$body"
  pc_expect_ok "no flag, no body check"
  pc_run_args "$PC_ID" --check-body "$body"
  expect_code 1 "$?" "a missing body file"
  assert_contains "$out" "error: the PR body file $body does not exist" "missing body file is named"

  # Every kind of drift fails and names where: trailing space, extra blank line,
  # missing final newline, CRLF endings, a hand-edited count and a hand-edited
  # verdict while the record keeps its NOT VERIFIED row.
  pc_render_to "$body"
  sed '1s/$/ /' "$body" > "$body.t"
  pc_run_args "$PC_ID" --check-body "$body.t"
  expect_code 1 "$?" "a trailing space on the verdict line"
  assert_contains "$out" "error: PR body does not match render(record): line 1 expected" "trailing space fails at line 1"
  { cat "$body"; printf '\n'; } > "$body.t"
  pc_run_args "$PC_ID" --check-body "$body.t"
  expect_code 1 "$?" "an extra trailing blank line"
  assert_contains "$out" "error: PR body does not match render(record): the file has extra content from line" "extra content is located"
  printf '%s' "$(cat "$body")" > "$body.t"
  pc_run_args "$PC_ID" --check-body "$body.t"
  expect_code 1 "$?" "a missing final newline"
  assert_contains "$out" "error: PR body does not match render(record): only line endings or the final newline differ" "final newline difference is named"
  sed 's/$/\r/' "$body" > "$body.t"
  pc_run_args "$PC_ID" --check-body "$body.t"
  expect_code 1 "$?" "CRLF line endings"
  assert_contains "$out" "error: PR body does not match render(record)" "CRLF fails"
  sed '1s/[0-9] of [0-9] acceptance criteria proven/9 of 9 acceptance criteria proven/' "$body" > "$body.t"
  pc_run_args "$PC_ID" --check-body "$body.t"
  expect_code 1 "$?" "a hand-edited AC count"
  assert_contains "$out" "9 of 9 acceptance criteria proven" "the edited line is shown"
  head -n 3 "$body" > "$body.t"
  pc_run_args "$PC_ID" --check-body "$body.t"
  expect_code 1 "$?" "a truncated body"
  assert_contains "$out" "the file ends before line 4" "a short body is located"
  : > "$body.t"
  pc_run_args "$PC_ID" --check-body "$body.t"
  expect_code 1 "$?" "an empty body"

  # The record is the source of truth: a body rendered before the record turned
  # a criterion NOT VERIFIED no longer matches the record.
  pc_render_to "$body"
  PC_ACS=$'| id | criterion | evidence |\n|---|---|---|\n| AC1 | criterion 1 | tests/fixed.test.sh |\n| AC2 | criterion 2 | tests/fixed.test.sh |\n| AC3 | criterion 3 | NOT VERIFIED |' \
    PC_NOTRUN="AC3 needs a live device." pc_write_proof
  pc_run_args "$PC_ID" --check-body "$body"
  expect_code 1 "$?" "a stale all-pass body against a record with a NOT VERIFIED row"
  assert_contains "$out" "error: PR body does not match render(record)" "stale body fails"
  pc_render_to "$body"
  pc_run_args "$PC_ID" --check-body "$body"
  expect_code 0 "$?" "the body re-rendered from the changed record"
  pass "--check-body passes only for a byte-identical render(record) and names the first difference"
}

test_proof_check_body_reports_other_failures_too() {
  pc_case pb-both
  local body="$PC_PROOF.body"
  pc_render_to "$body"
  sed '1s/$/ /' "$body" > "$body.t"
  PC_SCOPE=$'declared: fixed.txt\nactual: 5 files, +1 -0' pc_write_proof
  pc_run_args "$PC_ID" --check-body "$body.t"
  expect_code 1 "$?" "body drift plus a scope failure"
  assert_contains "$out" "error: Scope section reports 5 files, but git diff shows 1" "the ordinary failure is still listed"
  assert_contains "$out" "error: PR body does not match render(record)" "the body failure is listed with it"
  pass "--check-body adds to the failure list rather than replacing it"
}

test_proof_check_comment_equality() {
  pc_case pb-cmt
  local comment="$PC_PROOF.comment"
  cp "$PC_PROOF" "$comment"
  pc_run_args "$PC_ID" --check-comment "$comment"
  expect_code 0 "$?" "a comment equal to the record"
  assert_contains "$out" "ok candidate=$PC_CAND" "matching comment passes"
  printf ' ' >> "$comment"
  pc_run_args "$PC_ID" --check-comment "$comment"
  expect_code 1 "$?" "a comment with one extra byte"
  assert_contains "$out" "error: PR comment does not match record file" "extra byte fails"
  sed 's/criterion 2/criterion two/' "$PC_PROOF" > "$comment"
  pc_run_args "$PC_ID" --check-comment "$comment"
  expect_code 1 "$?" "an edited comment"
  assert_contains "$out" "error: PR comment does not match record file" "edited comment fails"
  rm -f "$comment"
  pc_run_args "$PC_ID" --check-comment "$comment"
  expect_code 1 "$?" "a missing comment file"
  assert_contains "$out" "error: the PR comment file $comment does not exist" "missing comment file is named"
  pc_expect_ok "no flag, no comment check"
  # Both flags together, each independently checked.
  pc_render_to "$PC_PROOF.body"
  cp "$PC_PROOF" "$comment"
  pc_run_args "$PC_ID" --check-body "$PC_PROOF.body" --check-comment "$comment"
  expect_code 0 "$?" "both checks pass together"
  pc_run_args "$PC_ID" --check-comment "$comment" --check-body "$PC_PROOF.body"
  expect_code 0 "$?" "flag order does not matter"
  printf 'x\n' >> "$comment"
  pc_run_args "$PC_ID" --check-body "$PC_PROOF.body" --check-comment "$comment"
  expect_code 1 "$?" "a good body with a bad comment"
  assert_not_contains "$out" "PR body does not match" "the good body is not blamed"
  assert_contains "$out" "error: PR comment does not match record file" "only the comment fails"
  pass "--check-comment passes only for a byte-identical record copy"
}

test_proof_check_flag_usage() {
  pc_case pb-use
  pc_run_args "$PC_ID" --check-body
  expect_code 2 "$?" "--check-body without a file"
  assert_contains "$out" "error: usage: fm-pstack.sh proof-check <task-id> [--rerun] [--require-mutation-row] [--check-body <file>] [--check-comment <file>]" "usage names every flag"
  assert_contains "$out" "next: --check-body needs the path of the file to compare" "missing file is explained"
  pc_run_args "$PC_ID" --check-comment ""
  expect_code 2 "$?" "--check-comment with an empty path"
  pc_run_args "$PC_ID" --bogus
  expect_code 2 "$?" "an unknown flag"
  pc_run_args "$PC_ID" extra
  expect_code 2 "$?" "a stray argument"
  pass "proof-check refuses malformed flag usage with exit 2"
}

test_proof_check_rerun_flag_either_position() {
  pc_rerun_case rr-pos "lint | true | pass: ran"
  pc_run_args "$PC_ID" --rerun
  expect_code 0 "$?" "--rerun after the task id"
  assert_contains "$out" "ok candidate=$PC_CAND (--rerun: all cheap checks match recorded)" "rerun ok line after the id"
  out=$(env FM_ROOT_OVERRIDE="$ROOT" FM_HOME="$HOME_DIR" PATH="$FAKEBIN:$PATH" \
    "$ROOT/bin/fm-pstack.sh" proof-check --rerun "$PC_ID" 2>&1)
  expect_code 0 "$?" "--rerun before the task id"
  assert_contains "$out" "ok candidate=$PC_CAND (--rerun:" "rerun ok line before the id"
  pass "--rerun is accepted before or after the task id"
}

test_proof_check_rerun_needs_a_task_id() {
  out=$(env FM_ROOT_OVERRIDE="$ROOT" FM_HOME="$HOME_DIR" PATH="$FAKEBIN:$PATH" \
    "$ROOT/bin/fm-pstack.sh" proof-check --rerun 2>&1)
  expect_code 2 "$?" "--rerun without a task id"
  assert_contains "$out" "error: usage: fm-pstack.sh proof-check <task-id> [--rerun]" "usage names --rerun"
  pass "--rerun without a task id refuses with exit 2"
}

test_proof_check_rerun_executes_lint_and_type_checks() {
  local lint="$TMP_ROOT/rr-lint" ty="$TMP_ROOT/rr-type" by_cmd="$TMP_ROOT/rr-cmd"
  rm -f "$lint" "$ty" "$by_cmd"
  pc_rerun_case rr-exec \
    "lint | touch $lint | pass: ran" \
    "type | pwd -P > $ty | pass: ran" \
    "static | echo mypy > $by_cmd | pass: ran"
  pc_run_args "$PC_ID" --rerun
  expect_code 0 "$?" "lint, type, and command-identified rows"
  [ -f "$lint" ] || fail "the lint row command was not re-run"
  [ -f "$by_cmd" ] || fail "a row whose command names mypy was not re-run"
  assert_equals "$(cd "$PC_WT" && pwd -P)" "$(cat "$ty")" "the type row runs in the candidate worktree"
  assert_contains "$out" "Check: lint" "the lint row is reported"
  assert_contains "$out" "--rerun summary: 3 checked, 3 passed, 0 failed" "all three cheap rows compared"
  pass "lint and type rows are re-executed, identified by name or command"
}

test_proof_check_rerun_skips_tests_and_coverage() {
  # Relative markers keep the command text free of the temp path, which must
  # not contain a cheap-check word for this case to mean anything.
  pc_rerun_case rr-skip "test | touch rr-test-ran | pass: ran" "coverage | touch rr-cov-ran | pass: ran"
  pc_run_args "$PC_ID" --rerun
  expect_code 0 "$?" "test and coverage rows with nothing to compare"
  [ ! -e "$PC_WT/rr-test-ran" ] || fail "the test row was re-run"
  [ ! -e "$PC_WT/rr-cov-ran" ] || fail "the coverage row was re-run"
  assert_contains "$out" "not re-run (not a lint or type check)" "the test row is reported not re-run"
  assert_contains "$out" "info: no cheap checks to re-run" "no cheap row is reported"
  assert_contains "$out" "--rerun summary: 0 checked, 0 passed, 0 failed" "nothing is counted"
  pass "test and coverage rows are not re-run"
}

test_proof_check_rerun_runs_the_whole_command_with_pipes() {
  local m="$TMP_ROOT/rr-pipe"
  rm -f "$m"
  pc_rerun_case rr-pipe "lint | true | touch $m | pass: ran"
  pc_run_args "$PC_ID" --rerun
  expect_code 0 "$?" "a command with an unescaped pipe"
  [ -f "$m" ] || fail "the right side of the piped command was not re-run"
  assert_contains "$out" "Command:  true | touch $m" "the command is reported whole"
  pass "a command containing a pipe is re-run whole"
}

test_proof_check_rerun_unescapes_escaped_pipes() {
  local m="$TMP_ROOT/rr-escaped-pipe"
  rm -f "$m"
  pc_rerun_case rr-esc "lint | true \| touch $m | pass: ran"
  pc_run_args "$PC_ID" --rerun
  expect_code 0 "$?" "a command with an escaped pipe"
  [ -f "$m" ] || fail "the escaped pipe reached the shell literally, so the right side did not run"
  assert_contains "$out" "Command:  true | touch $m" "the escaped pipe is shown as a pipe"
  pass "an escaped pipe in a Checks cell runs as a pipe"
}

test_proof_check_rerun_matches_recorded_pass_and_fail() {
  pc_rerun_case rr-match "lint | true | pass: ran" "type | false | fail: ran"
  pc_run_args "$PC_ID" --rerun
  expect_code 0 "$?" "pass-vs-pass and fail-vs-fail"
  assert_contains "$out" "Recorded: fail" "a fail row is recorded as fail"
  assert_contains "$out" "Re-run:   fail" "a failing re-run is reported"
  assert_equals 2 "$(printf '%s\n' "$out" | grep -c '✅ match')" "both rows report a match"
  assert_contains "$out" "--rerun summary: 2 checked, 2 passed, 0 failed" "the summary counts both"
  pass "pass-vs-pass and fail-vs-fail rows match"
}

test_proof_check_rerun_mismatch_fails_and_leaves_record_unchanged() {
  pc_rerun_case rr-mis "lint | false | pass: ran" "type | true | fail: ran"
  cp "$PC_PROOF" "$PC_PROOF.before"
  pc_run_args "$PC_ID" --rerun
  expect_code 1 "$?" "a pass-vs-fail and a fail-vs-pass mismatch"
  assert_contains "$out" "Check: lint" "the mismatched check is named"
  assert_contains "$out" "Recorded: pass" "the recorded result is shown"
  assert_contains "$out" "Re-run:   fail" "the re-run result is shown"
  assert_equals 2 "$(printf '%s\n' "$out" | grep -c '❌ mismatch')" "each mismatch is labelled"
  assert_contains "$out" "--rerun summary: 2 checked, 0 passed, 2 failed" "the summary counts the mismatches"
  assert_contains "$out" "error: --rerun detected discrepancies between recorded and re-run results" "the verdict names the discrepancy"
  cmp -s "$PC_PROOF" "$PC_PROOF.before" || fail "--rerun changed the proof record"
  pass "--rerun reports mismatches, exits 1, and leaves the record unchanged"
}

test_proof_check_rerun_skips_informational_rows() {
  pc_rerun_case rr-info "lint | false | info: not compared" "type | true | pass: ran"
  pc_run_args "$PC_ID" --rerun
  expect_code 0 "$?" "an info row is not compared"
  assert_contains "$out" "skipped (informational, not compared)" "the info row is reported skipped"
  assert_not_contains "$out" "Re-run:   fail" "the info row is never re-run"
  assert_contains "$out" "--rerun summary: 1 checked, 1 passed, 0 failed" "the info row is not counted"
  pass "info rows are skipped and not counted"
}

test_proof_check_rerun_reports_missing_command_and_continues() {
  local m="$TMP_ROOT/rr-after-missing"
  rm -f "$m"
  pc_rerun_case rr-miss \
    "lint | definitely-missing-lint-tool-xyz --check | pass: ran" \
    "type | touch $m | pass: ran"
  pc_run_args "$PC_ID" --rerun
  expect_code 1 "$?" "a command that cannot run"
  assert_contains "$out" "error: command not found or failed to execute: definitely-missing-lint-tool-xyz --check" "the missing command is named"
  assert_contains "$out" "Re-run:   not executed" "the missing command is reported not executed"
  [ -f "$m" ] || fail "the run stopped before the next row"
  assert_contains "$out" "--rerun summary: 2 checked, 1 passed, 1 failed" "the missing command counts as a failure"
  pass "a command that cannot execute is a failure and the run continues"
}

test_proof_check_rerun_unrecognized_result_is_a_failed_comparison() {
  pc_rerun_case rr-unk "lint | true | maybe"
  pc_run_args "$PC_ID" --rerun
  expect_code 1 "$?" "an unrecognized recorded result"
  assert_contains "$out" "error: result field format unrecognized: maybe" "the unrecognized result is named"
  assert_contains "$out" "Recorded: unrecognized" "the recorded status is shown"
  assert_contains "$out" "❌ mismatch" "the unrecognized result is a mismatch"
  assert_contains "$out" "--rerun summary: 1 checked, 0 passed, 1 failed" "the summary counts it"
  pass "an unrecognized recorded result is a failed comparison"
}

test_proof_check_rerun_missing_worktree_is_exit_3() {
  pc_case rr-gone
  fm_write_meta "$PC_META" "kind=ship" "workflow=pstack" "branch=$PC_BRANCH" "worktree=$TMP_ROOT/rr-gone-wt"
  pc_run_args "$PC_ID" --rerun
  expect_code 3 "$?" "--rerun with a missing worktree"
  assert_contains "$out" "error: worktree not available; skipping re-run" "the missing worktree is named"
  pass "--rerun with no usable worktree refuses with exit 3"
}

test_proof_check_without_rerun_does_not_rerun() {
  local m="$TMP_ROOT/rr-plain"
  rm -f "$m"
  pc_rerun_case rr-plain "lint | touch $m | fail: ran"
  pc_run_args "$PC_ID"
  expect_code 0 "$?" "proof-check without --rerun"
  assert_contains "$out" "ok candidate=$PC_CAND" "the ordinary ok line"
  assert_not_contains "$out" "Check:" "no rerun block without the flag"
  [ ! -e "$m" ] || fail "proof-check without --rerun re-ran a check"
  pass "proof-check without --rerun is unchanged and re-runs nothing"
}

test_supported_harnesses_is_claude_and_codex
test_resolve_success_report
test_resolve_codex_success_report
test_resolve_absent_config_is_exit_4
test_resolve_empty_config_is_exit_3
test_resolve_multiline_config_is_exit_3
test_resolve_relative_path_is_exit_3
test_resolve_missing_dir_is_exit_4
test_resolve_folder_of_plugins_is_exit_4
test_resolve_malformed_json_is_exit_3
test_resolve_no_name_is_exit_3
test_resolve_missing_entry_skill_is_exit_4
test_resolve_disabled_entry_is_exit_4
test_resolve_entry_without_frontmatter_is_exit_4
test_resolve_hooks_and_mcp_detection
test_resolve_unsupported_harness_is_exit_5
test_resolve_usage_is_exit_2
test_template_prints_the_proof_format
test_template_refuses_like_resolve
test_proof_check_happy_path
test_proof_check_unverified_mutation_row_is_report_only
test_proof_check_missing_meta_is_refusal
test_proof_check_missing_proof_is_refusal
test_proof_check_meta_not_ship_or_not_pstack
test_proof_check_field_failures
test_proof_check_git_anchoring_failures
test_proof_check_base_and_supersedes_rules
test_proof_check_run_state_gate
test_proof_check_usage_id_shape
test_proof_no_mistakes_label_set
test_proof_v2_header_format
test_proof_v2_brief_must_exist
test_proof_v2_ac_coverage
test_proof_v2_not_verified_echo
test_proof_v2_checks_table
test_proof_v2_scope
test_proof_v2_ai_review_disposition
test_render_golden_body
test_render_is_deterministic
test_render_verdict_and_source_lines
test_render_source_forms_and_refs_line
test_render_read_first_section
test_render_acceptance_table_escaping
test_render_checks_line
test_render_mutation_row
test_render_ai_review_line
test_render_scope_line
test_render_refusals
test_proof_check_body_equality
test_proof_check_body_reports_other_failures_too
test_proof_check_comment_equality
test_proof_check_flag_usage
test_proof_check_mutation_gate_is_report_only_by_default
test_proof_check_mutation_gate_requires_a_row
test_proof_check_mutation_gate_accepts_valid_rows_without_a_score_floor
test_proof_check_mutation_gate_refuses_stale_and_malformed_rows
test_proof_check_mutation_gate_refuses_a_classifier_failure
test_proof_check_mutation_gate_refuses_passed_over_an_unsupported_path
test_proof_check_mutation_gate_skips_a_change_without_a_mutation_surface
test_proof_check_rerun_flag_either_position
test_proof_check_rerun_needs_a_task_id
test_proof_check_rerun_executes_lint_and_type_checks
test_proof_check_rerun_skips_tests_and_coverage
test_proof_check_rerun_runs_the_whole_command_with_pipes
test_proof_check_rerun_unescapes_escaped_pipes
test_proof_check_rerun_matches_recorded_pass_and_fail
test_proof_check_rerun_mismatch_fails_and_leaves_record_unchanged
test_proof_check_rerun_skips_informational_rows
test_proof_check_rerun_reports_missing_command_and_continues
test_proof_check_rerun_unrecognized_result_is_a_failed_comparison
test_proof_check_rerun_missing_worktree_is_exit_3
test_proof_check_without_rerun_does_not_rerun

echo "all fm-pstack tests passed"