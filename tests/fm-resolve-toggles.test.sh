#!/usr/bin/env bash
# Behavior tests for bin/fm-resolve-toggles.sh: the pstack and no-mistakes toggles
# resolve per task from the brief, else the project's registry entry, else the
# machine default of both on, with a refusal on any invalid or unreadable input.
# The wiring into bin/fm-brief.sh is covered in tests/fm-brief.test.sh.
set -u

# shellcheck source=tests/lib.sh
. "$(dirname "${BASH_SOURCE[0]}")/lib.sh"

RESOLVE="$ROOT/bin/fm-resolve-toggles.sh"
TMP_ROOT=$(fm_test_tmproot fm-resolve-toggles)
DATA="$TMP_ROOT/data"
mkdir -p "$DATA"

cat > "$DATA/projects.md" <<'REG'
## Projects
- plain - no annotation (added 2026-01-01)
- ps-off [pstack=off] - one toggle (added 2026-01-01)
- both [no-mistakes=off pstack=off] - both toggles, reversed order (added 2026-01-01)
- mixed [direct-PR +yolo pstack=off branch=x/] - beside other tokens (added 2026-01-01)
- upper [PSTACK=OFF] - the key is lowercase only (added 2026-01-01)
- caps [pstack=OFF no-mistakes=Off] - values are case-insensitive (added 2026-01-01)
- bad [pstack=maybe] - invalid registry value (added 2026-01-01)
- empty [no-mistakes=] - empty registry value (added 2026-01-01)
- ps-off-longer [pstack=off] - longer name must not match a prefix (added 2026-01-01)
REG

brief() {  # <id> <body>
  mkdir -p "$DATA/$1"
  printf '%s\n' "$2" > "$DATA/$1/brief.md"
}
run() {  # <args...>; sets OUT and RC
  OUT=$(FM_DATA_OVERRIDE="$DATA" "$RESOLVE" "$@" 2>&1); RC=$?
}
expect() {  # <expected stdout, newline-joined> <label>
  expect_code 0 "$RC" "$2: exit code ($OUT)"
  [ "$OUT" = "$1" ] || fail "$2: expected [$1] got [$OUT]"
}
ON_ON=$'pstack=on\nno-mistakes=on'

test_machine_default_is_both_on() {
  brief t-default $'title: x\nask: y'
  run t-default; expect "$ON_ON" "no override anywhere"
  run t-default plain; expect "$ON_ON" "registered project without toggles"
  run t-default not-registered; expect "$ON_ON" "unregistered project"
  pass "machine default is both on"
}

test_project_overrides_machine_default() {
  brief t-proj 'title: x'
  run t-proj ps-off; expect $'pstack=off\nno-mistakes=on' "one project toggle"
  run t-proj both; expect $'pstack=off\nno-mistakes=off' "both project toggles in reverse order"
  run t-proj mixed; expect $'pstack=off\nno-mistakes=on' "toggle beside mode, yolo and branch tokens"
  run t-proj caps; expect $'pstack=off\nno-mistakes=off' "registry values are case-insensitive"
  run t-proj upper; expect "$ON_ON" "an upper-case key is not a toggle"
  run t-proj ps-off-longer; expect $'pstack=off\nno-mistakes=on' "exact whole-name match"
  run t-proj ps-of; expect "$ON_ON" "a name prefix does not match a longer entry"
  pass "project registry overrides the machine default"
}

test_project_comes_from_the_brief_when_not_passed() {
  brief t-from-brief $'project: ps-off\ntitle: x'
  run t-from-brief; expect $'pstack=off\nno-mistakes=on' "project: line in the brief"
  run t-from-brief plain; expect "$ON_ON" "an explicit project argument beats the brief's project line"
  pass "project falls back to the brief's project line"
}

test_brief_overrides_project() {
  brief t-brief-on $'pstack: on\nno-mistakes: ON'
  run t-brief-on both; expect "$ON_ON" "brief on beats project off, case-insensitive"
  brief t-brief-off $'pstack: off # skip the playbook\nproject: plain'
  run t-brief-off; expect $'pstack=off\nno-mistakes=on' "brief off beats the machine default, comment ignored"
  brief t-brief-one $'no-mistakes: on'
  run t-brief-one both; expect $'pstack=off\nno-mistakes=on' "an override of one toggle leaves the other to the project"
  brief t-brief-fence $'```moa-intake\ntitle: x\npstack: off\n```'
  run t-brief-fence; expect $'pstack=off\nno-mistakes=on' "an override inside the intake block"
  brief t-indented $'    pstack: off\ntitle: x'
  run t-indented; expect "$ON_ON" "an indented example is not an override"
  pass "brief overrides project overrides machine default"
}

test_invalid_values_are_refused() {
  brief t-bad-brief 'pstack: maybe'
  run t-bad-brief; expect_code 1 "$RC" "invalid brief value"
  assert_contains "$OUT" "invalid pstack value 'maybe'" "invalid brief value not named"
  [[ "$OUT" != pstack=* ]] || fail "invalid brief value still printed a resolution"
  brief t-bad-brief2 'no-mistakes: yes'
  run t-bad-brief2; expect_code 1 "$RC" "yes is not on or off"
  brief t-empty-brief 'pstack:'
  run t-empty-brief; expect_code 1 "$RC" "empty brief value"
  brief t-good 'title: x'
  run t-good bad; expect_code 2 "$RC" "invalid registry value"
  assert_contains "$OUT" "invalid pstack value 'maybe' for project bad" "invalid registry value not named"
  run t-good empty; expect_code 2 "$RC" "empty registry value"
  brief t-over 'pstack: on'
  run t-over bad; expect_code 0 "$RC" "a brief value wins over an invalid registry value of the same toggle"
  assert_contains "$OUT" "pstack=on" "winning brief value not printed"
  pass "invalid values are refused at the level that supplies them"
}

test_unreadable_input_is_refused() {
  run no-such-task; expect_code 1 "$RC" "missing brief"
  assert_contains "$OUT" "not found or not readable" "missing brief reason not stated"
  run; expect_code 64 "$RC" "no arguments"
  run a b c; expect_code 64 "$RC" "too many arguments"
  brief t-noreg 'project: plain'
  local empty="$TMP_ROOT/empty-data"
  mkdir -p "$empty/t-noreg"
  cp "$DATA/t-noreg/brief.md" "$empty/t-noreg/brief.md"
  OUT=$(FM_DATA_OVERRIDE="$empty" "$RESOLVE" t-noreg 2>&1); RC=$?
  expect "$ON_ON" "absent registry falls back to the machine default"
  pass "missing brief and wrong usage are refused, absent registry falls back"
}

test_brief_file_flag_reads_another_file() {
  printf 'pstack: off\n' > "$TMP_ROOT/staged.md"
  run --brief-file "$TMP_ROOT/staged.md" never-written ps-off
  expect $'pstack=off\nno-mistakes=on' "explicit brief file for a task with no brief yet"
  run --brief-file "$TMP_ROOT/absent.md" never-written; expect_code 1 "$RC" "missing explicit brief file"
  pass "--brief-file resolves from a staged file"
}

test_machine_default_is_both_on
test_project_overrides_machine_default
test_project_comes_from_the_brief_when_not_passed
test_brief_overrides_project
test_invalid_values_are_refused
test_unreadable_input_is_refused
test_brief_file_flag_reads_another_file
