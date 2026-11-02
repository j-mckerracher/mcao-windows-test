#!/usr/bin/env bash
# tests/fm-secondmate-parent-migrate.test.sh - the primary-owned migration of
# registered local secondmate parent records onto the explicit layout, run by
# bin/fm-secondmate-parent-migrate.sh.
#
# Coverage:
#   - a legacy v1 record naming the code root migrates to the explicit layout,
#     and the shared resolver then proves the primary's effective state and data;
#   - a legacy v1 record naming the state-owning directory migrates with its
#     parent_home and relay consent home unchanged;
#   - a repeat run reports "already explicit", exits 0, and changes no byte;
#   - a run that meets an unexpected binding, a missing record or metadata, a
#     foreign identity marker, or a malformed record exits 1 and leaves every
#     record and the registry byte-identical, including a valid sibling in the
#     same run;
#   - --help exits 0 and a bogus argument exits 2.
set -u

# shellcheck source=tests/lib.sh disable=SC1091
. "$(dirname "${BASH_SOURCE[0]}")/lib.sh"

TMP_ROOT=$(fm_test_tmproot fm-secondmate-parent-migrate)
SNAP_ROOT="$TMP_ROOT/snapshots"
mkdir -p "$SNAP_ROOT"

MIG_RC=0
MIG_OUT=
RW_DIR=
RW_PRIMARY=
RW_STATE=
RW_DATA=
RW_ELSEWHERE=

# Canonical absolute path of an existing directory.
canon() {  # <dir>
  (cd "$1" && pwd -P)
}

# Print the layout the shared parser reads from one record, or UNPARSEABLE.
record_fields() {  # <record-file>
  bash -c '
    . "$1" || exit 1
    if ! fm_secondmate_parent_record_parse "$2"; then
      echo UNPARSEABLE
      exit 0
    fi
    printf "layout=%s\nparent_home=%s\nprimary=%s\nstate=%s\ndata=%s\nconsent=%s\n" \
      "$FM_SECONDMATE_PARENT_LAYOUT" "$FM_SECONDMATE_PARENT_HOME" "$FM_SECONDMATE_PARENT_PRIMARY" \
      "$FM_SECONDMATE_PARENT_STATE" "$FM_SECONDMATE_PARENT_DATA" "$FM_SECONDMATE_PARENT_CONSENT"
  ' _ "$ROOT/bin/fm-secondmate-parent-lib.sh" "$1"
}

# Write one explicit local record through the library writer, which re-parses it.
write_explicit_record() {  # <record-file> <parent_home> <primary> <state> <data> <consent>
  bash -c '. "$1" && fm_secondmate_parent_record_write_local "$2" "$3" "$4" "$5" "$6" "$7"' _ \
    "$ROOT/bin/fm-secondmate-parent-lib.sh" "$1" "$2" "$3" "$4" "$5" "$6"
}

# Assert that <text> contains <line> as a whole line.
assert_line() {  # <text> <line> <msg>
  printf '%s\n' "$1" | grep -Fxq -- "$2" || fail "$3 (missing line '$2')"
}

# Assert that the record in <home> is explicit and names exactly these layout
# values. parent_home is the v1 field the migration must leave unchanged.
assert_explicit() {  # <home> <parent_home> <primary> <state> <data> <consent> <msg>
  local fields
  fields=$(record_fields "$1/.fm-secondmate-parent")
  assert_line "$fields" "layout=explicit" "$7: record is not explicit: $fields"
  assert_line "$fields" "parent_home=$2" "$7: parent_home changed: $fields"
  assert_line "$fields" "primary=$3" "$7: wrong primary: $fields"
  assert_line "$fields" "state=$4" "$7: wrong state: $fields"
  assert_line "$fields" "data=$5" "$7: wrong data: $fields"
  assert_line "$fields" "consent=$6" "$7: wrong consent home: $fields"
}

# Resolve <child> as secondmate <id> through the shared resolver with live primary
# <live>, and assert the primary, state, and data it proves.
assert_resolves() {  # <child> <id> <live> <primary> <state> <data> <msg>
  local got
  got=$(bash -c '
    . "$1" || exit 1
    . "$2" || exit 1
    . "$3" || exit 1
    rc=0
    fm_secondmate_parent_resolve "$4" "$5" "$6" || rc=$?
    printf "rc=%s\nhome=%s\nstate=%s\ndata=%s\nerror=%s\n" "$rc" \
      "$FM_SECONDMATE_PARENT_RESOLVED_HOME" "$FM_SECONDMATE_PARENT_RESOLVED_STATE" \
      "$FM_SECONDMATE_PARENT_RESOLVED_DATA" "$FM_SECONDMATE_PARENT_RESOLVE_ERROR"
  ' _ "$ROOT/bin/fm-backend.sh" "$ROOT/bin/fm-secondmate-registry-lib.sh" \
    "$ROOT/bin/fm-secondmate-parent-lib.sh" "$1" "$2" "$3") \
    || fail "$7: the shared resolver could not be loaded"
  assert_line "$got" "rc=0" "$7: the shared resolver refused: $got"
  assert_line "$got" "home=$4" "$7: the shared resolver named the wrong primary: $got"
  assert_line "$got" "state=$5" "$7: the shared resolver named the wrong state: $got"
  assert_line "$got" "data=$6" "$7: the shared resolver named the wrong data: $got"
}

# Register local secondmate <id> at <home>, with its identity marker, a
# kind=secondmate meta under <state>, a registry line under <data>, and a v1
# record naming <record-home>, or no record when <record-home> is "-".
child_add() {  # <state> <data> <home> <id> <record-home|->
  local state=$1 data=$2 home=$3 id=$4 record_home=$5 home_c
  mkdir -p "$home"
  printf '%s\n' "$id" > "$home/.fm-secondmate-home"
  home_c=$(canon "$home")
  fm_write_secondmate_meta "$state/$id.meta" "$home_c"
  printf -- '- %s - %s secondmate (home: %s; scope: %s scope; projects: alpha; added 2026-06-22)\n' \
    "$id" "$id" "$home_c" "$id" >> "$data/secondmates.md"
  if [ "$record_home" != - ]; then
    printf 'schema=fm-secondmate-parent.v1\nroute=local\nparent_home=%s\n' "$record_home" \
      > "$home/.fm-secondmate-parent"
  fi
}

# Run the migration as the primary launches with: FM_HOME is the code root, and
# the state and data directories come from their overrides. Output lands in
# MIG_OUT and the exit status in MIG_RC; extra arguments pass through.
migrate() {  # <primary> <state> <data> [args...]
  local primary=$1 state=$2 data=$3
  shift 3
  MIG_RC=0
  MIG_OUT=$(env FM_HOME="$primary" FM_STATE_OVERRIDE="$state" FM_DATA_OVERRIDE="$data" \
    "$ROOT/bin/fm-secondmate-parent-migrate.sh" ${@+"$@"} 2>&1) || MIG_RC=$?
}

# Keep a byte copy of <file> under <name> for a later byte comparison.
keep() {  # <file> <name>
  cp -p "$1" "$SNAP_ROOT/$2" || fail "could not keep a copy of $1"
}

# Assert that <file> still holds the bytes kept under <name>.
assert_kept() {  # <file> <name> <msg>
  cmp -s "$SNAP_ROOT/$2" "$1" || fail "$3"
}

# A repeat run must report every listed secondmate as already explicit and exit 0.
# Callers compare the kept record bytes afterwards with assert_kept.
assert_repeat_is_noop() {  # <primary> <state> <data> <id>...
  local primary=$1 state=$2 data=$3 id
  shift 3
  migrate "$primary" "$state" "$data"
  expect_code 0 "$MIG_RC" "repeat run"
  for id in "$@"; do
    assert_contains "$MIG_OUT" "current secondmate $id: parent record already explicit" \
      "repeat run did not report $id as already explicit: $MIG_OUT"
  done
}

# Legacy v1 records naming the code root migrate to the explicit layout. The
# state and data are split-named so the code root is the only spelling in play.
test_code_root_migration() {
  local w="$TMP_ROOT/code-root" primary state data alpha beta
  mkdir -p "$w/code" "$w/split-state" "$w/split-data" "$w/homes"
  primary=$(canon "$w/code")
  state=$(canon "$w/split-state")
  data=$(canon "$w/split-data")
  child_add "$state" "$data" "$w/homes/alpha" alpha "$primary"
  child_add "$state" "$data" "$w/homes/beta" beta "$primary"
  alpha=$(canon "$w/homes/alpha")
  beta=$(canon "$w/homes/beta")

  migrate "$primary" "$state" "$data"
  expect_code 0 "$MIG_RC" "code-root migration"
  assert_contains "$MIG_OUT" "migrated secondmate alpha: parent record is explicit (primary $primary)" \
    "code-root migration did not report alpha as migrated: $MIG_OUT"
  assert_contains "$MIG_OUT" "migrated secondmate beta" \
    "code-root migration did not report beta as migrated: $MIG_OUT"
  assert_explicit "$alpha" "$primary" "$primary" "$state" "$data" "$primary" "code-root alpha"
  assert_explicit "$beta" "$primary" "$primary" "$state" "$data" "$primary" "code-root beta"
  assert_resolves "$alpha" alpha "$primary" "$primary" "$state" "$data" "code-root resolver for alpha"
  assert_resolves "$beta" beta "$primary" "$primary" "$state" "$data" "code-root resolver for beta"

  keep "$alpha/.fm-secondmate-parent" code-root-alpha
  keep "$beta/.fm-secondmate-parent" code-root-beta
  assert_repeat_is_noop "$primary" "$state" "$data" alpha beta
  assert_kept "$alpha/.fm-secondmate-parent" code-root-alpha "repeat run changed alpha's parent record"
  assert_kept "$beta/.fm-secondmate-parent" code-root-beta "repeat run changed beta's parent record"
  pass "code-root migration: legacy v1 records become explicit, resolve to the primary's layout, and a repeat run changes nothing"
}

# Legacy v1 records naming the state-owning directory (the parent of a state
# directory whose data is the primary's data) migrate with parent_home unchanged,
# and the relay consent home stays the state owner the record already implied.
test_state_owner_migration() {
  local w="$TMP_ROOT/state-owner" primary owner state data gamma
  mkdir -p "$w/code" "$w/owner/state" "$w/owner/data" "$w/homes"
  primary=$(canon "$w/code")
  owner=$(canon "$w/owner")
  state=$(canon "$w/owner/state")
  data=$(canon "$w/owner/data")
  child_add "$state" "$data" "$w/homes/gamma" gamma "$owner"
  gamma=$(canon "$w/homes/gamma")

  migrate "$primary" "$state" "$data"
  expect_code 0 "$MIG_RC" "state-owner migration"
  assert_contains "$MIG_OUT" "migrated secondmate gamma: parent record is explicit (primary $primary)" \
    "state-owner migration did not report gamma as migrated: $MIG_OUT"
  assert_explicit "$gamma" "$owner" "$primary" "$state" "$data" "$owner" "state-owner gamma"
  assert_resolves "$gamma" gamma "$primary" "$primary" "$state" "$data" "state-owner resolver for gamma"

  keep "$gamma/.fm-secondmate-parent" state-owner-gamma
  assert_repeat_is_noop "$primary" "$state" "$data" gamma
  assert_kept "$gamma/.fm-secondmate-parent" state-owner-gamma "repeat run changed gamma's parent record"
  pass "state-owner migration: parent_home stays the state owner, the layout becomes explicit, and a repeat run changes nothing"
}

# An empty refusal world: a code root with split state and data, a directory that
# is neither the primary nor its state owner, and a homes directory for children.
refusal_world() {  # <label>
  RW_DIR="$TMP_ROOT/refuse-$1"
  mkdir -p "$RW_DIR/code" "$RW_DIR/split-state" "$RW_DIR/split-data" "$RW_DIR/homes" \
    "$RW_DIR/elsewhere/state" "$RW_DIR/elsewhere/data"
  RW_PRIMARY=$(canon "$RW_DIR/code")
  RW_STATE=$(canon "$RW_DIR/split-state")
  RW_DATA=$(canon "$RW_DIR/split-data")
  RW_ELSEWHERE=$(canon "$RW_DIR/elsewhere")
}

# Run the migration over the current refusal world and require a refusal: exit 1,
# the named proof in the output, and every byte of the world unchanged. Each id
# listed also has its parent record compared byte for byte with cmp.
expect_refusal() {  # <label> <fragment> [record-id...]
  local label=$1 fragment=$2 id changed
  shift 2
  rm -rf "$SNAP_ROOT/world"
  cp -Rp "$RW_DIR" "$SNAP_ROOT/world" || fail "$label: could not snapshot the world"

  migrate "$RW_PRIMARY" "$RW_STATE" "$RW_DATA"
  expect_code 1 "$MIG_RC" "$label"
  assert_contains "$MIG_OUT" "$fragment" "$label: refusal did not name the failed proof: $MIG_OUT"
  for id in ${@+"$@"}; do
    cmp -s "$SNAP_ROOT/world/homes/$id/.fm-secondmate-parent" \
      "$RW_DIR/homes/$id/.fm-secondmate-parent" \
      || fail "$label: refused run changed the parent record of $id"
  done
  changed=$(diff -r "$SNAP_ROOT/world" "$RW_DIR" 2>&1) \
    || fail "$label: refused run changed the world: $changed"
  pass "refused, exit 1 with no byte changed: $label"
}

case_unexpected_binding() {
  refusal_world unexpected-binding
  child_add "$RW_STATE" "$RW_DATA" "$RW_DIR/homes/alpha" alpha "$RW_ELSEWHERE"
  expect_refusal "unexpected binding" "unexpected prior binding for alpha" alpha
}

case_missing_record() {
  refusal_world missing-record
  child_add "$RW_STATE" "$RW_DATA" "$RW_DIR/homes/alpha" alpha -
  expect_refusal "missing record" "has no durable parent record"
}

case_missing_meta() {
  refusal_world missing-meta
  child_add "$RW_STATE" "$RW_DATA" "$RW_DIR/homes/alpha" alpha "$RW_PRIMARY"
  rm -f "$RW_STATE/alpha.meta"
  expect_refusal "missing metadata" "parent metadata for secondmate alpha is missing or unsafe" alpha
}

case_foreign_marker() {
  refusal_world foreign-marker
  child_add "$RW_STATE" "$RW_DATA" "$RW_DIR/homes/alpha" alpha "$RW_PRIMARY"
  printf 'beta\n' > "$RW_DIR/homes/alpha/.fm-secondmate-home"
  expect_refusal "foreign identity marker" "does not carry an identity marker naming alpha" alpha
}

case_malformed_record() {
  refusal_world malformed-record
  child_add "$RW_STATE" "$RW_DATA" "$RW_DIR/homes/alpha" alpha "$RW_PRIMARY"
  printf 'route=local\nparent_home=%s\n' "$RW_PRIMARY" > "$RW_DIR/homes/alpha/.fm-secondmate-parent"
  expect_refusal "malformed record" "parent record for alpha is malformed or unsafe" alpha
}

case_explicit_other_primary() {
  refusal_world explicit-other-primary
  child_add "$RW_STATE" "$RW_DATA" "$RW_DIR/homes/alpha" alpha "$RW_PRIMARY"
  write_explicit_record "$RW_DIR/homes/alpha/.fm-secondmate-parent" \
    "$RW_ELSEWHERE" "$RW_ELSEWHERE" "$RW_ELSEWHERE/state" "$RW_ELSEWHERE/data" "$RW_ELSEWHERE" \
    || fail "could not write the explicit record naming another primary"
  expect_refusal "explicit record naming another primary" "unexpected prior binding for alpha" alpha
}

# A valid sibling must not be migrated when another registered secondmate is
# refused: the plan refuses the whole run before any record is staged.
case_mixed_sibling() {
  refusal_world mixed-sibling
  child_add "$RW_STATE" "$RW_DATA" "$RW_DIR/homes/alpha" alpha "$RW_PRIMARY"
  child_add "$RW_STATE" "$RW_DATA" "$RW_DIR/homes/beta" beta "$RW_ELSEWHERE"
  expect_refusal "valid sibling with a refused secondmate" "unexpected prior binding for beta" alpha beta
}

test_usage() {
  local out rc
  out=$(env FM_HOME="$TMP_ROOT/usage-home" "$ROOT/bin/fm-secondmate-parent-migrate.sh" --help 2>&1)
  rc=$?
  expect_code 0 "$rc" "--help"
  assert_contains "$out" "usage: fm-secondmate-parent-migrate.sh [--help]" "--help did not print the usage"

  out=$(env FM_HOME="$TMP_ROOT/usage-home" "$ROOT/bin/fm-secondmate-parent-migrate.sh" --bogus 2>&1)
  rc=$?
  expect_code 2 "$rc" "bogus argument"
  assert_contains "$out" "usage: fm-secondmate-parent-migrate.sh" "bogus argument did not print the usage"
  pass "usage: --help exits 0 and a bogus argument exits 2"
}

test_code_root_migration
test_state_owner_migration
case_unexpected_binding
case_missing_record
case_missing_meta
case_foreign_marker
case_malformed_record
case_explicit_other_primary
case_mixed_sibling
test_usage
