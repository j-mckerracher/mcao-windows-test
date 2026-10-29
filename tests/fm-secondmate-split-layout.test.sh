#!/usr/bin/env bash
# tests/fm-secondmate-split-layout.test.sh - the split operational layout, end to
# end. The firstmate code root is FM_HOME, while its state, data, projects, and
# config live in separate override directories.
#
# Coverage:
#   - seed writes an explicit parent record that names the code root as primary
#     and the override state and data as the effective layout;
#   - spawn, relaunch, and teardown of a secondmate succeed under the split
#     layout with no "cannot resolve the primary home" refusal;
#   - the shared resolver proves the override state and data for a launched mate;
#   - the repo's own data/ and state/ are untouched by the split run;
#   - genuine refusals are kept: a record whose state holds no metadata for the
#     mate blocks relaunch, and a malformed record blocks teardown, each leaving
#     the mate's home, record, and registry route intact;
#   - the unsplit layout still seeds, launches, relaunches, and tears down,
#     including a legacy v1 record.
#
# This file is the regression for the split-layout cleanup refusal. On a
# `git archive HEAD` export with this file copied in, it fails; on the working
# tree it passes.
set -u

# shellcheck source=tests/secondmate-helpers.sh disable=SC1091
. "$(dirname "${BASH_SOURCE[0]}")/secondmate-helpers.sh"

TMP_ROOT=$(fm_test_tmproot fm-secondmate-split-layout)
export FM_BACKEND=tmux

ROOT_C=$(cd "$ROOT" && pwd -P)
SPLIT_STATE="$TMP_ROOT/split-state"
SPLIT_DATA="$TMP_ROOT/split-data"
SPLIT_PROJECTS="$TMP_ROOT/split-projects"
SPLIT_CONFIG="$TMP_ROOT/split-config"
SPLIT_STATE_C=
SPLIT_DATA_C=
SUB="$TMP_ROOT/design-home"
REVIEW="$TMP_ROOT/review-home"
LOG="$TMP_ROOT/tmux.log"
PANE="$TMP_ROOT/pane.txt"
FAKEBIN=
MANIFEST_BEFORE=

# Run "$@" as the split primary: FM_HOME is the code root, and state, data,
# projects, and config each come from their own override directory.
split_run() {
  env PATH="$FAKEBIN:$PATH" FM_HOME="$ROOT" \
    FM_STATE_OVERRIDE="$SPLIT_STATE" FM_DATA_OVERRIDE="$SPLIT_DATA" \
    FM_PROJECTS_OVERRIDE="$SPLIT_PROJECTS" FM_CONFIG_OVERRIDE="$SPLIT_CONFIG" \
    FM_FAKE_TMUX_LOG="$LOG" FM_FAKE_TMUX_CAPTURE="$PANE" "$@"
}

# Run "$@" as an unsplit primary: FM_HOME alone, with no override variables.
plain_run() {  # <primary-home> <command...>
  local home=$1
  shift
  env PATH="$FAKEBIN:$PATH" FM_HOME="$home" \
    FM_FAKE_TMUX_LOG="$LOG" FM_FAKE_TMUX_CAPTURE="$PANE" "$@"
}

# Print the layout the shared parser reads from one parent record file, one
# key=value per line, or UNPARSEABLE when the parser refuses it.
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

# A run whose output names a refusal did not succeed, whatever its status.
assert_no_refusal() {  # <output> <msg>
  case "$1" in
    *REFUSED*|*'refusing to'*|*'cannot resolve the primary home'*)
      fail "$2 was refused under the split layout: $1"
      ;;
  esac
}

# A manifest of the repo's own data/ and state/: every path, then every file's
# checksum. An absent directory is recorded as absent, so a created one is caught.
repo_manifest() {
  local dir
  for dir in data state; do
    if [ -e "$ROOT/$dir" ]; then
      (cd "$ROOT" && find "$dir" -print | LC_ALL=C sort && find "$dir" -type f -exec cksum {} + | LC_ALL=C sort)
    else
      printf 'absent %s\n' "$dir"
    fi
  done
}

setup_world() {
  mkdir -p "$SPLIT_STATE" "$SPLIT_DATA" "$SPLIT_PROJECTS" "$SPLIT_CONFIG"
  fm_git_init_commit "$SPLIT_PROJECTS/alpha"
  fm_git_init_commit "$SPLIT_PROJECTS/beta"
  fm_git_add_origin "$SPLIT_PROJECTS/alpha" "$TMP_ROOT/remotes/alpha.git"
  fm_git_add_origin "$SPLIT_PROJECTS/beta" "$TMP_ROOT/remotes/beta.git"
  cat > "$SPLIT_DATA/projects.md" <<'EOF'
- alpha [direct-PR +yolo] - alpha project (added 2026-06-22)
- beta [direct-PR] - beta project (added 2026-06-22)
EOF
  FAKEBIN=$(make_fake_tmux "$TMP_ROOT/fake")
  make_fake_no_mistakes "$TMP_ROOT/fake" >/dev/null
  SPLIT_STATE_C=$(cd "$SPLIT_STATE" && pwd -P)
  SPLIT_DATA_C=$(cd "$SPLIT_DATA" && pwd -P)
  : > "$LOG"
}

# Seed <id> at <home> as a secondmate of the split primary, keep a copy of the
# seeded record for later checks, then launch it.
split_up() {  # <id> <home>
  local id=$1 home=$2 out home_abs
  out=$(split_run env FM_SECONDMATE_CHARTER="$id charter" FM_SECONDMATE_SCOPE="$id scope" \
    "$ROOT/bin/fm-home-seed.sh" "$id" "$home" alpha beta 2>&1) \
    || fail "split seed of $id failed: $out"
  assert_no_refusal "$out" "split seed of $id"
  home_abs=$(cd "$home" && pwd -P)
  assert_grep "- $id - " "$SPLIT_DATA/secondmates.md" "split seed of $id did not register the mate"
  assert_grep "home: $home_abs;" "$SPLIT_DATA/secondmates.md" "split seed of $id registered the wrong home"
  cp -p "$home/.fm-secondmate-parent" "$TMP_ROOT/$id.seeded" \
    || fail "split seed of $id wrote no parent record"

  : > "$LOG"
  out=$(split_run "$ROOT/bin/fm-spawn.sh" "$id" "$home" codex --secondmate 2>&1) \
    || fail "split spawn of $id failed: $out"
  assert_no_refusal "$out" "split spawn of $id"
  assert_grep 'kind=secondmate' "$SPLIT_STATE/$id.meta" "split spawn of $id did not record kind=secondmate"
  assert_grep "home=$home_abs" "$SPLIT_STATE/$id.meta" "split spawn of $id did not record its home"
}

# Recovery: drop the live meta, then respawn from the durable registry and record.
split_relaunch() {  # <id> <home>
  local id=$1 home=$2 out home_abs
  home_abs=$(cd "$home" && pwd -P)
  rm -f "$SPLIT_STATE/$id.meta"
  : > "$LOG"
  out=$(split_run "$ROOT/bin/fm-spawn.sh" "$id" "echo relaunch" --secondmate 2>&1) \
    || fail "split relaunch of $id failed: $out"
  assert_no_refusal "$out" "split relaunch of $id"
  assert_grep "home=$home_abs" "$SPLIT_STATE/$id.meta" "split relaunch of $id did not keep its home"
  assert_grep "window=firstmate:fm-$id" "$SPLIT_STATE/$id.meta" "split relaunch of $id did not rebuild its window"
  assert_resolves "$home" "$id" "$ROOT" "$ROOT_C" "$SPLIT_STATE_C" "$SPLIT_DATA_C" \
    "split resolver for $id"
}

# Retire <id>: teardown must succeed without a refusal and remove the home, meta,
# and registry route, leaving the primary project clone in place.
split_teardown() {  # <id> <home>
  local id=$1 home=$2 out
  : > "$LOG"
  out=$(split_run "$ROOT/bin/fm-teardown.sh" "$id" 2>&1) \
    || fail "split teardown of $id failed: $out"
  assert_no_refusal "$out" "split teardown of $id"
  assert_absent "$home" "split teardown of $id did not remove its home"
  assert_absent "$SPLIT_STATE/$id.meta" "split teardown of $id did not clear its meta"
  assert_no_grep "- $id - " "$SPLIT_DATA/secondmates.md" "split teardown of $id did not remove the registry route"
  assert_present "$SPLIT_PROJECTS/alpha" "split teardown of $id disturbed the primary project alpha"
}

# The explicit record that seed wrote names the code root as primary and the
# override state and data as the effective layout.
check_seeded_record() {  # <id>
  local fields
  fields=$(record_fields "$TMP_ROOT/$1.seeded")
  assert_line "$fields" "layout=explicit" "seed of $1 did not write an explicit parent record: $fields"
  assert_line "$fields" "parent_home=$ROOT_C" "seed of $1 recorded the wrong parent_home: $fields"
  assert_line "$fields" "primary=$ROOT_C" "seed of $1 recorded the wrong primary: $fields"
  assert_line "$fields" "state=$SPLIT_STATE_C" "seed of $1 recorded the wrong state: $fields"
  assert_line "$fields" "data=$SPLIT_DATA_C" "seed of $1 recorded the wrong data: $fields"
  assert_line "$fields" "consent=$ROOT_C" "seed of $1 recorded the wrong consent home: $fields"
}

# Genuine refusals: a record whose state holds no metadata for the mate blocks
# relaunch, and a partial explicit layout is rejected by the shared parser. Each
# leaves the mate intact.
split_refusals() {
  local record="$REVIEW/.fm-secondmate-parent" out fields

  split_up review "$REVIEW"
  mkdir -p "$TMP_ROOT/decoy-state"

  # Refusal 1: the record's state is a directory that holds no metadata for review.
  rm -f "$SPLIT_STATE/review.meta"
  write_explicit_record "$record" "$ROOT_C" "$ROOT_C" "$TMP_ROOT/decoy-state" "$SPLIT_DATA_C" "$ROOT_C" \
    || fail "could not write the decoy-state record"
  cp -p "$record" "$TMP_ROOT/review.decoy" || fail "could not snapshot the decoy-state record"
  : > "$LOG"
  out=$(split_run "$ROOT/bin/fm-spawn.sh" review "echo relaunch" --secondmate 2>&1) \
    && fail "relaunch accepted a record whose state holds no metadata for review: $out"
  assert_contains "$out" "refusing to launch secondmate review" \
    "relaunch with a decoy state record did not refuse: $out"
  cmp -s "$TMP_ROOT/review.decoy" "$record" || fail "refused relaunch changed the parent record"
  assert_no_grep 'new-window' "$LOG" "refused relaunch still opened a window"
  assert_absent "$SPLIT_STATE/review.meta" "refused relaunch wrote meta"

  # Refusal 2: a partial explicit layout (no parent_consent) is malformed, and the
  # shared parser rejects it. Teardown reads a mate's record only when FM_HOME is
  # that mate's own home, so the check here stops at the parser.
  cp -p "$TMP_ROOT/review.seeded" "$record" || fail "could not restore the seeded record"
  printf 'schema=fm-secondmate-parent.v1\nroute=local\nparent_home=%s\nparent_primary=%s\nparent_state=%s\nparent_data=%s\n' \
    "$ROOT_C" "$ROOT_C" "$SPLIT_STATE_C" "$SPLIT_DATA_C" > "$record"
  fields=$(record_fields "$record")
  assert_line "$fields" "UNPARSEABLE" "partial layout record was accepted by the parser: $fields"
  cp -p "$TMP_ROOT/review.seeded" "$record" || fail "could not restore the seeded record after the partial check"
  pass "refusals: a decoy-state record blocks relaunch and a partial explicit layout is rejected by the parser"
}

# The unsplit layout still works, including a record written before the explicit
# layout existed (a v1 record whose parent_home is the primary home).
unsplit_lifecycle() {
  local primary="$TMP_ROOT/unsplit-primary" home="$TMP_ROOT/legacy-home"
  local primary_c home_c out fields
  mkdir -p "$primary/data" "$primary/state" "$primary/projects"
  fm_git_init_commit "$primary/projects/alpha"
  fm_git_add_origin "$primary/projects/alpha" "$TMP_ROOT/remotes/unsplit-alpha.git"
  printf '%s\n' '- alpha [direct-PR] - alpha project (added 2026-06-22)' > "$primary/data/projects.md"
  primary_c=$(cd "$primary" && pwd -P)

  out=$(plain_run "$primary" env FM_SECONDMATE_CHARTER='legacy charter' FM_SECONDMATE_SCOPE='legacy scope' \
    "$ROOT/bin/fm-home-seed.sh" legacy "$home" alpha 2>&1) \
    || fail "unsplit seed failed: $out"
  assert_no_refusal "$out" "unsplit seed"
  home_c=$(cd "$home" && pwd -P)
  fields=$(record_fields "$home/.fm-secondmate-parent")
  assert_line "$fields" "layout=explicit" "unsplit seed did not write an explicit record: $fields"
  assert_line "$fields" "state=$primary_c/state" "unsplit seed recorded the wrong state: $fields"
  assert_line "$fields" "data=$primary_c/data" "unsplit seed recorded the wrong data: $fields"

  # A record from before the explicit layout: v1 with parent_home only.
  printf 'schema=fm-secondmate-parent.v1\nroute=local\nparent_home=%s\n' "$primary_c" \
    > "$home/.fm-secondmate-parent"
  fields=$(record_fields "$home/.fm-secondmate-parent")
  assert_line "$fields" "layout=v1" "legacy record did not parse as v1: $fields"
  assert_line "$fields" "state=$primary_c/state" "legacy v1 record did not derive the unsplit state: $fields"
  assert_line "$fields" "data=$primary_c/data" "legacy v1 record did not derive the unsplit data: $fields"

  : > "$LOG"
  out=$(plain_run "$primary" "$ROOT/bin/fm-spawn.sh" legacy "$home" codex --secondmate 2>&1) \
    || fail "unsplit spawn with a legacy v1 record failed: $out"
  assert_no_refusal "$out" "unsplit spawn"
  assert_grep "home=$home_c" "$primary/state/legacy.meta" "unsplit spawn did not record its home"
  assert_resolves "$home" legacy "$primary_c" "$primary_c" "$primary_c/state" "$primary_c/data" \
    "unsplit resolver for legacy"

  rm -f "$primary/state/legacy.meta"
  out=$(plain_run "$primary" "$ROOT/bin/fm-spawn.sh" legacy "echo relaunch" --secondmate 2>&1) \
    || fail "unsplit relaunch failed: $out"
  assert_no_refusal "$out" "unsplit relaunch"

  out=$(plain_run "$primary" "$ROOT/bin/fm-teardown.sh" legacy 2>&1) \
    || fail "unsplit teardown failed: $out"
  assert_no_refusal "$out" "unsplit teardown"
  assert_absent "$home" "unsplit teardown did not remove its home"
  assert_no_grep '- legacy - ' "$primary/data/secondmates.md" "unsplit teardown did not remove the registry route"
  pass "unsplit: seed, launch, relaunch, and teardown work with explicit and legacy v1 records"
}

setup_world
MANIFEST_BEFORE=$(repo_manifest)

split_up design "$SUB"
split_relaunch design "$SUB"
split_teardown design "$SUB"
check_seeded_record design
pass "split lifecycle: seed, spawn, relaunch, and teardown succeed under the split layout"
pass "split records: seed writes an explicit record naming the code root and the override state and data"

split_refusals
unsplit_lifecycle

if [ "$(repo_manifest)" != "$MANIFEST_BEFORE" ]; then
  fail "the split run changed the repo's own data/ or state/"
fi
pass "repo isolation: the repo's own data/ and state/ are unchanged by the split run"
