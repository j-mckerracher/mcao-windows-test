#!/usr/bin/env bash
# tests/fm-mutation-classify.test.sh - the changed-path surface classifier
# (bin/fm-mutation-classify.sh over bin/fm-mutation-surface-lib.sh) through its
# executable interface: every surface class, every unsupported disposition with
# a stated reason, the loud failure for a path no class matches, and an explicit
# disposition record for every unmatched tracked path.
set -u

# shellcheck source=tests/lib.sh
. "$(dirname "${BASH_SOURCE[0]}")/lib.sh"

CLASSIFY="$ROOT/bin/fm-mutation-classify.sh"
TMP=$(fm_test_tmproot fm-mutation-classify)
ERR="$TMP/stderr"

# run_classify <path>...: sets OUT (stdout) and RC (exit code); stderr lands in $ERR.
run_classify() {
  OUT=$("$CLASSIFY" "$@" 2>"$ERR")
  RC=$?
}

# expect_record <path> <surface> <disposition> <detail>: the full TAB record.
expect_record() {
  run_classify "$1"
  expect_code 0 "$RC" "classify $1"
  assert_equals "$1"$'\t'"$2"$'\t'"$3"$'\t'"$4" "$OUT" "wrong classification for $1"
}

# expect_unsupported <path> <surface>: unsupported disposition with a stated reason.
expect_unsupported() {
  local p s d r
  run_classify "$1"
  expect_code 0 "$RC" "classify $1"
  IFS=$'\t' read -r p s d r <<<"$OUT"
  assert_equals "$1" "$p" "path column wrong for $1"
  assert_equals "$2" "$s" "surface wrong for $1"
  assert_equals "unsupported" "$d" "$1 must have an explicit unsupported disposition"
  [ -n "$r" ] || fail "$1 unsupported disposition must state a reason"
}

# --- mutate dispositions ----------------------------------------------------

expect_record bin/fm-session-start.sh shell mutate "shell-custom-mutator (report-only)"
expect_record bin/backends/herdr.sh shell mutate "shell-custom-mutator (report-only)"
pass "shell production scripts map to the report-only custom shell mutator"

expect_record bin/fm-mail.py python mutate mutmut-class
pass "python maps to the mutmut-class strategy"

expect_record bin/fm-extension.mjs javascript mutate stryker-class
expect_record bin/fm-probe.ts typescript mutate stryker-class
pass "javascript and typescript map to the stryker-class strategy"

# --- unsupported dispositions -----------------------------------------------

expect_unsupported tests/fm-lint.test.sh test-code
expect_unsupported tests/fm-backend-herdr-eventwait.test.py test-code
pass "test code, shell and python alike, is an explicit unsupported disposition and never mutated"

expect_unsupported .claude/mods/firstmate-calm/tests/calm.test.ts test-code
pass "test code in a nested tests directory is never mutated"

expect_record bin/tests-runner.sh shell mutate "shell-custom-mutator (report-only)"
pass "a path that merely contains the word tests is not test code"

expect_unsupported .github/workflows/ci.yml yaml
run_classify .github/workflows/ci.yml
assert_contains "$OUT" "never counted as mutation evidence" "yaml disposition must keep actionlint and schema evidence out of mutation counts"
pass "yaml is unsupported for mutation and its reason keeps actionlint and schema checks out of mutation evidence"

expect_unsupported docs/documentation-audiences.json json
pass "json is an explicit unsupported disposition with a stated reason"

expect_unsupported .tasks.toml toml
pass "toml is an explicit unsupported disposition with a stated reason"

expect_unsupported docs/scripts.md markdown
pass "markdown is an explicit unsupported disposition with a stated reason"

expect_unsupported .agents/skills/firstmate-coding-guidelines/SKILL.md skill-md
pass "SKILL.md text is an explicit unsupported disposition with a stated reason"

# --- unclassified paths fail loudly -----------------------------------------

run_classify bin/fm-lint.sh .gitignore
expect_code 1 "$RC" "a path matching no surface class must fail the classification step"
assert_contains "$(cat -- "$ERR")" ".gitignore" "stderr must name the unclassified path"
assert_not_contains "$OUT" ".gitignore" "an unclassified path must not appear as a classified record"
assert_contains "$OUT" "bin/fm-lint.sh"$'\t'"shell"$'\t'"mutate" "classifiable siblings are still reported beside a failure"
pass "an unclassified path fails the step, is named on stderr, and is never silently skipped"

run_classify docs/architecture.svg
expect_code 1 "$RC" "an image path must fail the classification step, not pass as skipped"
assert_contains "$(cat -- "$ERR")" "docs/architecture.svg" "stderr must name the image path"
pass "a non-surface file type fails the classification step"

# --- unmatched tracked paths: explicit records, never silently skipped -------

# The 19 tracked paths no surface class matches (DPES-794, DPES-830). Each keeps its own
# unsupported record with a stated reason, and the classifier still refuses it.
UNMATCHED_PATHS=(
  .agents/skills/bearings/assets/board-template.html
  .agents/skills/firstmate-calm
  .claude/skills
  .gitattributes
  .github/CODEOWNERS
  .gitignore
  assets/banner.png
  assets/birds-eye-view/app.css
  assets/birds-eye-view/index.html
  bin/fm-birds-eye-model.jq
  bin/fm-contributions.jq
  bin/fm-procevent-extension-capture.pl
  bin/mcao
  docs/examples/wedge-alarm
  docs/mutation-oracle-map.tsv
  docs/pi-supervision-branch-poster.svg
  docs/verification/mutation-toolchain/python-constraints.proposed
  docs/verification/mutation-toolchain/stryker-lock.proposed.tsv
  LICENSE
)

# shellcheck source=bin/fm-mutation-surface-lib.sh
. "$ROOT/bin/fm-mutation-surface-lib.sh"

for p in "${UNMATCHED_PATHS[@]}"; do
  record=$(fm_mutation_unmatched_disposition_of "$p") || fail "$p has no unmatched-path disposition record"
  IFS=$'\t' read -r d r <<<"$record"
  assert_equals "unsupported" "$d" "$p must have an explicit unsupported disposition"
  [ -n "$r" ] || fail "$p unsupported disposition must state a reason"
  run_classify "$p"
  expect_code 1 "$RC" "$p must still fail the classification step"
  assert_contains "$(cat -- "$ERR")" "$p" "stderr must name $p"
done
pass "each of the 19 unmatched tracked paths has its own unsupported disposition with a stated reason, and the classifier still refuses each by name"

if fm_mutation_unmatched_disposition_of bin/fm-session-start.sh >/dev/null; then
  fail "a path a surface class matches must not carry an unmatched-path record"
fi
pass "a path a surface class matches carries no unmatched-path record"

UNRECORDED=""
while IFS= read -r p; do
  fm_mutation_surface_of "$p" >/dev/null && continue
  fm_mutation_unmatched_disposition_of "$p" >/dev/null || UNRECORDED="$UNRECORDED $p"
done < <(git -C "$ROOT" ls-files)
assert_equals "" "$UNRECORDED" "tracked paths with no surface class and no disposition record"
pass "no tracked path is silently skipped: every unmatched tracked path has a record"

# --- equivalent-mutant sign-off ---------------------------------------------

assert_equals "Josh McKerracher" "$(fm_mutation_equivalence_signoff_owner)" "equivalent-mutant sign-off owner"
pass "equivalent-mutant sign-off belongs to Josh McKerracher until another owner is named"

# --- command-line contract ----------------------------------------------------

OUT=$("$CLASSIFY" </dev/null 2>"$ERR")
RC=$?
expect_code 2 "$RC" "no changed paths is a usage error, never a silent pass"
pass "no changed paths is a usage error"

OUT=$(printf 'bin/fm-session-start.sh\n\ndocs/scripts.md\n' | "$CLASSIFY" 2>"$ERR")
RC=$?
expect_code 0 "$RC" "paths read from stdin must classify"
assert_contains "$OUT" "bin/fm-session-start.sh"$'\t'"shell"$'\t'"mutate" "stdin path must classify as shell"
assert_contains "$OUT" "docs/scripts.md"$'\t'"markdown"$'\t'"unsupported" "stdin path must classify as markdown"
pass "changed paths piped on stdin are classified and blank lines are ignored"

echo "# fm-mutation-classify.test.sh: all assertions passed"
