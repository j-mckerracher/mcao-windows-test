#!/usr/bin/env bash
# tests/assignment-profile-helpers.sh - assignment-profile decision-record fixtures.
#
# Source this from a test file after ROOT is set:
#   # shellcheck source=tests/assignment-profile-helpers.sh
#   . "$ROOT/tests/assignment-profile-helpers.sh"
#
# Standalone on purpose: it defines no traps and sources only bin/fm-branch-prefix-lib.sh
# (for its default prefix constant), so a suite that owns its EXIT trap (the real-Herdr
# suites) can use these builders without pulling in tests/lib.sh. tests/fixtures.sh
# sources it too, so fixtures users see the same names. The builders read ROOT (the
# firstmate repo root) and return non-zero when a fixture cannot be built;
# fm_test_persist_ship_profile is the one that calls the caller's fail.

# shellcheck source=bin/fm-branch-prefix-lib.sh
. "$(dirname "${BASH_SOURCE[0]}")/../bin/fm-branch-prefix-lib.sh"

# fm_test_assignment_profile_record <task-id> <comparison-base-commit>
# Prints a valid assignment-profile decision record (docs/assignment-profile-policy.md)
# for task <task-id>: the high-risk shape, pinned to the current policy revision and
# target branch main. Launches check the base; bin/fm-brief.sh does not.
fm_test_assignment_profile_record() {
  cat <<EOF
{
  "contract_version": 1,
  "decision_id": "$1/1",
  "task_id": "$1",
  "assignment_intent": "Exercise the launch behavior under test",
  "expected_surfaces": ["bin/fm-spawn.sh"],
  "pinned_policy_revision": "motif-assignment-profile-r2",
  "profile": "high-risk",
  "decision_stage": "provisional",
  "classification_status": "resolved",
  "risk_triggers": ["dispatch-lifecycle-worktree-delivery-merge-or-proof-gate-authority"],
  "change_coverage": {
    "complete": true,
    "categories": ["committed", "tracked-working-tree", "untracked", "renamed", "deleted"]
  },
  "reasons": ["spawn lifecycle and relaunch restoration change"],
  "required_stages": [
    "ground-scope",
    "design",
    "design-critique",
    "implement",
    "independent-change-review",
    "risk-verify",
    "change-reassessment",
    "delivery-handoff"
  ],
  "evidence_obligations": {
    "tests_required": true,
    "independent_change_review_required": true
  },
  "target_branch": "main",
  "comparison_base_commit": "$2"
}
EOF
}

# fm_test_assignment_profile_section <task-id> <comparison-base-commit>
# Prints the assignment-profile section a ship brief carries beside its
# managed-context block (see fm_test_managed_block), then the definition-of-done
# copy of the same obligations, as fm-brief.sh writes them. See
# fm_test_assignment_profile_record.
fm_test_assignment_profile_section() {
  local record
  record=$(mktemp "${TMPDIR:-/tmp}/fm-test-assignment-record.XXXXXX") || return 1
  fm_test_assignment_profile_record "$1" "$2" >"$record"
  # The section and the definition-of-done copy come from the producer library, so
  # a fixture brief carries the same text bin/fm-brief.sh writes for that record.
  (
    . "$ROOT/bin/fm-assignment-profile-lib.sh"
    fm_assignment_profile_section "$record" fixture "$1" || exit 1
    printf '\n'
    fm_assignment_profile_dod_copy "$record" fixture "$1"
  )
  local rc=$?
  rm -f -- "$record"
  return "$rc"
}

# fm_test_ship_profile_file <dir> <task-id> [comparison-base-commit]
# Writes a record for <task-id> under <dir> and prints its path, for
# bin/fm-brief.sh --assignment-profile. Scaffolding checks the record shape and
# revision but not its base; pass the base a later spawn will pin anyway, so the
# same record also launches.
fm_test_ship_profile_file() {
  local dir=$1 id=$2 base=${3:-0123456789abcdef0123456789abcdef01234567} file
  mkdir -p "$dir"
  file="$dir/$id.assignment-profile.json"
  fm_test_assignment_profile_record "$id" "$base" >"$file"
  printf '%s\n' "$file"
}

# fm_test_ensure_ship_assignment_profile <home> <id> <comparison-base-commit> <spawn-args...>
# The assignment-profile counterpart of fm_test_ensure_ship_managed_block: appends the
# section to an already-written brief when the spawn arguments describe a ship launch.
# A spawn's base is its worktree HEAD, so pass that commit (git -C <worktree> rev-parse HEAD).
fm_test_ensure_ship_assignment_profile() {
  local home=$1 id=$2 base=$3 brief mode="" prev="" arg
  shift 3
  for arg in "$@"; do
    case "$arg" in
      --scout|--secondmate) return 0 ;;
    esac
    [ "$prev" = --mode ] && mode=$arg
    prev=$arg
  done
  case "$mode" in no-mistakes|direct-PR) ;; *) return 0 ;; esac
  brief="$home/data/$id/brief.md"
  [ -f "$brief" ] || return 0
  grep -qx '```assignment-profile' "$brief" && return 0
  fm_test_assignment_profile_section "$id" "$base" >>"$brief"
}

# fm_test_ship_launch_lines <brief> <task-id>
# Adds the two brief lines a ship launch reads and bin/fm-brief.sh writes: the
# "Delivery contract: mode=" line and the "Ship branch: " line. A spawn refuses a ship
# brief with no Ship branch line unless its branch is the legacy fm/<id>, so the
# hand-written suite briefs need it. The branch is the library's default prefix plus the
# task id, which is what a home with no config/branch-prefix launches on, and the herdr
# suites' scratch config directories hold no such file. Every herdr suite spawns its
# ship with --mode no-mistakes. Idempotent.
fm_test_ship_launch_lines() {
  local brief=$1 id=$2
  grep -q '^Delivery contract: mode=' "$brief" || printf 'Delivery contract: mode=no-mistakes\n' >>"$brief"
  # shellcheck disable=SC2031 # read-only use of the constant set by the library sourced above
  grep -q '^Ship branch: ' "$brief" || printf 'Ship branch: %s%s\n' "$FM_DEFAULT_BRANCH_PREFIX" "$id" >>"$brief"
}

# fm_test_attach_ship_profile <brief> <task-id> <project>
# Makes <brief> carry what a ship launch checks before any endpoint: the launch lines
# (fm_test_ship_launch_lines) and the assignment-profile section, pinned to <project>'s
# HEAD: a fresh launch's worktree starts there, and the spawn gate pins the record's
# base to that worktree HEAD. A brief that does not exist yet is left alone, so the
# spawn still refuses it with its own message. The check and append are not atomic, so
# call it once per brief before any spawn of that brief starts.
fm_test_attach_ship_profile() {
  local brief=$1 id=$2 project=$3 base
  [ -f "$brief" ] || return 0
  fm_test_ship_launch_lines "$brief" "$id"
  grep -qx '```assignment-profile' "$brief" && return 0
  base=$(git -C "$project" rev-parse --verify --quiet HEAD) || return 1
  fm_test_assignment_profile_section "$id" "$base" >>"$brief"
}

# fm_test_persist_ship_profile <home> <id> <base-commit>
# Mirrors what a launched ship leaves behind: the brief carries the
# assignment-profile section, the canonical record is persisted beside the task
# data, and the task record carries the profile keys. A relaunch restores that
# record and refuses without it (bin/fm-assignment-profile-lib.sh). Call it after the
# task's meta file is written, since the profile keys are appended to it.
fm_test_persist_ship_profile() {
  local home=$1 id=$2 base=$3 brief persisted canon sha lib="$ROOT/bin/fm-assignment-profile-lib.sh"
  brief="$home/data/$id/brief.md"
  persisted="$home/data/$id/assignment-profile.json"
  fm_test_assignment_profile_section "$id" "$base" >>"$brief"
  canon=$(bash -c '. "$1"; fm_assignment_profile_load "$2" relaunch-fixture "$3" 1' _ "$lib" "$brief" "$id") \
    || fail "fixture brief for $id carries no loadable assignment profile"
  bash -c '. "$1"; fm_assignment_profile_persist "$2" "$3"' _ "$lib" "$persisted" "$canon" \
    || fail "fixture could not persist the assignment profile for $id"
  sha=$(bash -c '. "$1"; fm_assignment_profile_sha "$2"' _ "$lib" "$persisted") \
    || fail "fixture could not hash the persisted profile for $id"
  bash -c '. "$1"; fm_assignment_profile_meta "$2" "$3"' _ "$lib" "$canon" "$sha" >>"$home/state/$id.meta" \
    || fail "fixture could not write the profile keys for $id"
}
