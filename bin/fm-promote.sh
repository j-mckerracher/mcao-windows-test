#!/usr/bin/env bash
# Promote a scout task to a ship task in place: the crewmate keeps its window,
# worktree, and loaded context; only the contract changes. Flips kind= to ship in
# state/<task-id>.meta so fm-teardown.sh applies the full ship-task teardown protection
# again. Promotion also writes the crewmate's ship instructions to
# data/<task-id>/ship-instructions.md, appends that same superseding contract to
# data/<task-id>/brief.md for future relaunches, and prints the fm-send.sh command
# that delivers it to the current worker. Those instructions carry the
# scratch-state inventory, the clean
# default-branch base, the immutable ship branch, and - rendered from
# bin/fm-dod-lib.sh, the single owner an ordinary ship brief also uses - the
# mode-specific Definition of done, so a promoted worker receives exactly the same
# delivery contract as a briefed one, including the no-mistakes mode's ask-user
# escalation rule and --yes ban. The instructions also carry `# Task` with
# `## Captain's intent` preserved from the scout brief and promotion's ship-time
# instructions under `## Firstmate spec`; the scout-time spec remains context but
# is not relabeled as the ship spec. Promotion refuses leftover `{TASK}` /
# `{FIRSTMATE_SPEC}` placeholders and a `## Captain's intent` line opening with
# a Captain label or address (bin/fm-dod-lib.sh). A pre-subsection scout
# brief contributes only Task lines explicitly marked as captain words to intent,
# read outside fenced blocks and indented examples so a quoted `Captain:` sample
# never passes the provenance gate as the ask (bin/fm-dod-lib.sh).
# A scout records no delivery posture, so promotion is where this task's delivery
# contract is decided: --mode, --yolo, and the ship branch resolved from
# --branch-prefix are written into the meta alongside the kind= flip. Firstmate resolves all three at promotion time, having just
# read the scout's report (AGENTS.md section 7); data/projects.md holds the
# captain's standing posture as context, and this script never looks that posture
# up. The registry is read for two project facts, neither a per-task decision: the
# forge binding, which promotion takes from there instead of asking firstmate to
# remember it, and the no-mistakes execution toggle, which bin/fm-resolve-toggles.sh
# owns and which refuses --mode no-mistakes when it resolves to off.
# no-mistakes-prod-only is a registry policy rather than a task mode and is refused.
# A scout spawned on a named base records base_branch= in its meta; promotion
# keeps that base as the ship's starting point and pull-request target, and
# refuses a mode that cannot carry one (bin/fm-dod-lib.sh fm_base_branch_valid).
# There is no --forge flag here: the binding comes from the registry, and for a
# task record naming no project it is none. bin/fm-brief.sh takes --forge instead
# because that script has no registry access at all, and bin/fm-spawn.sh checks
# its value against the registry; bin/fm-project-mode.sh's header owns the
# binding and bin/fm-dod-lib.sh owns what it changes for the worker, including
# the refusal of a forge on local-only.
# The promoted brief gains the managed-context block a relaunch validates
# (docs/managed-context.md, bin/fm-managed-context-lib.sh), and the meta gains
# the managed_* fields a spawn writes. --assignment-profile <record.json> is
# required: the promoted brief also gains the assignment-profile block, the record
# is persisted beside the task, and its profile_* keys join the meta. The record's
# target and comparison base are pinned to the scout worktree's HEAD, because
# promotion keeps that worktree and spawn never runs for it.
# A gcp task refuses promotion before any lock or meta write (bin/fm-execution-target-lib.sh).
# Usage: fm-promote.sh <task-id> --mode <no-mistakes|direct-PR|local-only> --yolo <on|off> --assignment-profile <record.json> [--branch-prefix <prefix>]
set -eu

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
FM_ROOT="${FM_ROOT_OVERRIDE:-$(cd "$SCRIPT_DIR/.." && pwd)}"
FM_HOME="${FM_HOME:-${FM_ROOT_OVERRIDE:-$FM_ROOT}}"
STATE="${FM_STATE_OVERRIDE:-$FM_HOME/state}"
DATA="${FM_DATA_OVERRIDE:-$FM_HOME/data}"

# shellcheck source=bin/fm-branch-prefix-lib.sh
. "$SCRIPT_DIR/fm-branch-prefix-lib.sh"
# shellcheck source=bin/fm-dod-lib.sh
. "$SCRIPT_DIR/fm-dod-lib.sh"
# shellcheck source=bin/fm-managed-context-lib.sh
. "$SCRIPT_DIR/fm-managed-context-lib.sh"
# shellcheck source=bin/fm-pr-lib.sh
. "$SCRIPT_DIR/fm-pr-lib.sh"
# shellcheck source=bin/fm-wake-lib.sh
. "$SCRIPT_DIR/fm-wake-lib.sh"
# shellcheck source=bin/fm-tasks-axi-lib.sh
. "$SCRIPT_DIR/fm-tasks-axi-lib.sh"
# shellcheck source=bin/fm-backlog-transition-lib.sh
. "$SCRIPT_DIR/fm-backlog-transition-lib.sh"
# shellcheck source=bin/fm-backend.sh
. "$SCRIPT_DIR/fm-backend.sh"
# shellcheck source=bin/fm-execution-target-lib.sh
. "$SCRIPT_DIR/fm-execution-target-lib.sh"
# shellcheck source=bin/fm-secondmate-parent-lib.sh
. "$SCRIPT_DIR/fm-secondmate-parent-lib.sh"
# shellcheck source=bin/fm-secondmate-registry-lib.sh
. "$SCRIPT_DIR/fm-secondmate-registry-lib.sh"
# shellcheck source=bin/fm-ff-lib.sh
. "$SCRIPT_DIR/fm-ff-lib.sh"
# shellcheck source=bin/fm-assignment-profile-lib.sh
. "$SCRIPT_DIR/fm-assignment-profile-lib.sh"

MODE=
YOLO=
BRANCH_PREFIX=$(fm_branch_prefix)
PROFILE_RECORD=
PROFILE_SET=0
MODE_SET=0
YOLO_SET=0
FORGE=none
POS=()
want_value=
for a in "$@"; do
  if [ -n "$want_value" ]; then
    case "$a" in
      --*) echo "error: --$want_value requires a value" >&2; exit 1 ;;
    esac
    case "$want_value" in
      mode) MODE=$a; MODE_SET=1 ;;
      yolo) YOLO=$a; YOLO_SET=1 ;;
      branch-prefix) BRANCH_PREFIX=$a ;;
      assignment-profile) PROFILE_RECORD=$a; PROFILE_SET=1 ;;
    esac
    want_value=
    continue
  fi
  case "$a" in
    --mode) want_value=mode ;;
    --mode=*) MODE=${a#--mode=}; MODE_SET=1 ;;
    --yolo) want_value=yolo ;;
    --yolo=*) YOLO=${a#--yolo=}; YOLO_SET=1 ;;
    --branch-prefix) want_value="branch-prefix" ;;
    --branch-prefix=*) BRANCH_PREFIX=${a#--branch-prefix=} ;;
    --assignment-profile) want_value=assignment-profile ;;
    --assignment-profile=*) PROFILE_RECORD=${a#--assignment-profile=}; PROFILE_SET=1 ;;
    *) POS+=("$a") ;;
  esac
done
[ -z "$want_value" ] || { echo "error: --$want_value requires a value" >&2; exit 1; }
[ "${#POS[@]}" -ge 1 ] || { echo "usage: fm-promote.sh <task-id> --mode <no-mistakes|direct-PR|local-only> --yolo <on|off> --assignment-profile <record.json>" >&2; exit 1; }
[ "$PROFILE_SET" -eq 1 ] || {
  echo "error: promotion requires --assignment-profile <record.json>; a promoted ship relaunches only from a persisted decision record (docs/assignment-profile-policy.md)" >&2
  exit 1
}
[ "$MODE_SET" -eq 1 ] || {
  echo "error: promotion requires --mode <no-mistakes|direct-PR|local-only>; decide it now from the scout's findings and the project's registered posture in data/projects.md" >&2
  exit 1
}
[ "$YOLO_SET" -eq 1 ] || {
  echo "error: promotion requires --yolo <on|off>; it is this task's merge authority, not a project lookup" >&2
  exit 1
}
case "$MODE" in
  no-mistakes|direct-PR|local-only) ;;
  no-mistakes-prod-only)
    echo "error: no-mistakes-prod-only is a registry policy, not a task mode; classify this task's surface and resolve it to no-mistakes or direct-PR" >&2
    exit 1 ;;
  *) echo "error: --mode must be one of no-mistakes, direct-PR, local-only (got '$MODE')" >&2; exit 1 ;;
esac
case "$YOLO" in
  on|off) ;;
  *) echo "error: --yolo must be on or off (got '$YOLO')" >&2; exit 1 ;;
esac
# A posture this forge cannot carry is refused once the registry binding has been
# read. Merge authority on a Gerrit forge is refused rather than quietly dropped,
# on the captain's decision of 2026-09-15 (bin/fm-project-mode.sh's header carries
# it). The call right below the definition is kept deliberately as a guard on the
# mode and yolo posture; it cannot refuse on the forge, which stays none until the
# registry supplies it after the lock, so the post-registry call is the one that
# fires.
refuse_impossible_forge_posture() {
  fm_forge_valid_for_mode "$FORGE" "$MODE" fm-promote.sh || return 1
  if [ "$FORGE" = gerrit ] && [ "$YOLO" = on ]; then
    echo "error: --yolo on is refused for forge=gerrit: a Code-Review+2 is a positive attributed claim that a named human approved and firstmate must not manufacture one (captain's decision 2026-09-15); promote with --yolo off and take any landing on a current explicit captain instruction naming that concrete change" >&2
    return 1
  fi
  return 0
}
refuse_impossible_forge_posture || exit 1

ID=${POS[0]}
fm_task_id_creation_valid "$ID" || { echo "error: invalid task id" >&2; exit 2; }
BRANCH="$BRANCH_PREFIX$ID"
if ! git check-ref-format --branch "$BRANCH" >/dev/null 2>&1; then
  echo "error: --branch-prefix and task id must form a valid git branch (got '$BRANCH')" >&2
  exit 1
fi
printf -v BRANCH_Q '%q' "$BRANCH"
# A gcp task runs on no local terminal and has no local worktree to promote in.
# Refuse before the control lock, the guard, or any meta write
# (contract: bin/fm-execution-target-lib.sh).
PROMOTE_META="$STATE/$ID.meta"
if [ -f "$PROMOTE_META" ]; then
  PROMOTE_TARGET=$(fm_exec_record_resolve "$PROMOTE_META") || exit 1
  if [ "$PROMOTE_TARGET" = "$FM_EXEC_TARGET_GCP" ]; then
    fm_exec_refuse_cloud_op "$ID" promote || exit 1
  fi
fi
CONTROL_LOCK="$STATE/.control-$ID.lock"
CONTROL_LOCK_HELD=0
META_LOCK=
META_LOCK_HELD=0
TMP=
META=
SCOUT_BRIEF=
BRIEF_ORIGINAL=
BRIEF_REPLACEMENT=
PROMOTE_INTAKE=
promote_cleanup() {
  local status=$?
  [ -z "$TMP" ] || rm -f -- "$TMP" 2>/dev/null || true
  [ -z "$BRIEF_REPLACEMENT" ] || rm -f -- "$BRIEF_REPLACEMENT" 2>/dev/null || true
  [ -z "$PROMOTE_INTAKE" ] || rm -f -- "$PROMOTE_INTAKE" 2>/dev/null || true
  if [ -n "$BRIEF_ORIGINAL" ] && [ -e "$BRIEF_ORIGINAL" ]; then
    mv -f -- "$BRIEF_ORIGINAL" "$SCOUT_BRIEF" 2>/dev/null || true
  fi
  if [ "$META_LOCK_HELD" = 1 ]; then
    META_LOCK_HELD=0
    fm_lock_release "$META_LOCK" || true
  fi
  if [ "$CONTROL_LOCK_HELD" = 1 ]; then
    CONTROL_LOCK_HELD=0
    fm_lock_release "$CONTROL_LOCK" || true
  fi
  return "$status"
}
trap promote_cleanup EXIT
fm_lock_try_acquire "$CONTROL_LOCK" || {
  echo "error: another lifecycle action is already running for task $ID; nothing was changed" >&2
  exit 1
}
CONTROL_LOCK_HELD=1
"$FM_ROOT/bin/fm-guard.sh" || true
META="$STATE/$ID.meta"
[ -d "$STATE" ] || { echo "error: state dir not found: $STATE" >&2; exit 1; }
META_LOCK=$(fm_meta_lock_path "$META") || exit 1
fm_lock_acquire_wait "$META_LOCK"
META_LOCK_HELD=1
if ! fm_backlog_record_present "$META" "task record" "$STATE"; then
  echo "error: task record for $ID is unsafe or missing ($FM_BACKLOG_TRANSITION_ERROR)" >&2
  exit 1
fi
grep -qx 'kind=scout' "$META" || { echo "error: task $ID is not a scout task (kind=scout not in meta)" >&2; exit 1; }
# The promoted ship's managed_worktree is the worktree this scout was spawned into,
# which promotion keeps; a record without one cannot carry the managed-context fields.
PROMOTE_WORKTREE=$(sed -n 's/^worktree=//p' "$META" | head -n 1)
[ -n "$PROMOTE_WORKTREE" ] || { echo "error: task record for $ID names no worktree; a promoted ship needs its worktree recorded (managed_worktree)" >&2; exit 1; }

# Unlike the mode and yolo above, the forge is not a per-task decision: it is the
# captain's project binding, so promotion takes it from the registry rather than
# from a flag firstmate must remember.
PROMOTE_PROJECT=$(sed -n 's/^project=//p' "$META" | head -n 1)
if [ -n "$PROMOTE_PROJECT" ]; then
  PROMOTE_PROJECT_NAME=$(basename "$PROMOTE_PROJECT")
  if ! PROMOTE_STANDING_FORGE=$("$FM_ROOT/bin/fm-project-mode.sh" --forge "$PROMOTE_PROJECT_NAME"); then
    echo "error: $ID cannot promote: the registry entry for $PROMOTE_PROJECT_NAME does not resolve to a delivery posture (see the refusal above); correct data/projects.md and promote again" >&2
    exit 1
  fi
  FORGE=${PROMOTE_STANDING_FORGE:-none}
  refuse_impossible_forge_posture || exit 1
fi
BASE_BRANCH=$(sed -n 's/^base_branch=//p' "$META" | head -n 1)
fm_base_branch_valid "$BASE_BRANCH" "$MODE" "$FORGE" "fm-promote.sh $ID" || exit 1
# Promotion keeps the scout's worktree, so the promoted ship starts from that
# worktree's HEAD. Pin the record to the same target and base spawn would pin, and
# refuse before anything is written.
PROMOTE_TARGET=$BASE_BRANCH
if [ -z "$PROMOTE_TARGET" ]; then
  PROMOTE_TARGET=$(default_branch "$PROMOTE_WORKTREE") || {
    echo "error: $ID cannot promote: no default branch resolves in $PROMOTE_WORKTREE to pin the assignment profile's target" >&2
    exit 1
  }
fi
PROMOTE_BASE=$(git -C "$PROMOTE_WORKTREE" rev-parse --verify --quiet HEAD) || {
  echo "error: $ID cannot promote: its worktree $PROMOTE_WORKTREE has no HEAD commit to pin as the assignment profile's comparison base" >&2
  exit 1
}
PROFILE_CANON=$(fm_assignment_profile_validate "$PROFILE_RECORD" "fm-promote.sh $ID" --task "$ID" --require-current) || exit 1
fm_assignment_profile_pin_check "$PROFILE_CANON" "$PROMOTE_TARGET" "$PROMOTE_BASE" "fm-promote.sh $ID" || exit 1
PROFILE_SECTION=$(fm_assignment_profile_section "$PROFILE_RECORD" "fm-promote.sh $ID" "$ID") || exit 1
PROFILE_OBLIGATIONS=$(fm_assignment_profile_obligations "$PROFILE_RECORD" "fm-promote.sh $ID" "$ID") || exit 1
PROMOTE_BASE_WORDS='default-branch base'
[ -z "$BASE_BRANCH" ] || PROMOTE_BASE_WORDS="copy of the base branch \`$BASE_BRANCH\`"
# An unbound project keeps the exact wording it always had.
PROMOTE_FORGE_WORDS=
[ "$FORGE" = none ] || PROMOTE_FORGE_WORDS=" forge=$FORGE"

SCOUT_BRIEF="$DATA/$ID/brief.md"
if fm_brief_task_placeholders_present "$SCOUT_BRIEF"; then
  echo "error: $SCOUT_BRIEF still contains {TASK} or {FIRSTMATE_SPEC}; preserve the original ask in ## Captain's intent and fill the scout-time ## Firstmate spec; promotion generates a separate ship-time spec" >&2
  exit 1
fi
if ! fm_brief_task_content_valid "$SCOUT_BRIEF"; then
  echo "error: $SCOUT_BRIEF must contain nonempty ## Captain's intent and ## Firstmate spec subsections (or a nonempty legacy # Task body) before promotion" >&2
  exit 1
fi
if ADDRESS_LINE=$(fm_brief_intent_address_line "$SCOUT_BRIEF"); then
  echo "error: $SCOUT_BRIEF ## Captain's intent has an operator-address line: $ADDRESS_LINE; write the captain's actual words without a Captain label or address before promotion, since the heading already records provenance" >&2
  exit 1
fi
if fm_brief_task_heading_present "$SCOUT_BRIEF" "## Captain's intent"; then
  INTENT_BODY=$(fm_brief_task_heading_body "$SCOUT_BRIEF" "## Captain's intent")
else
  TASK_BODY=$(fm_brief_heading_body "$SCOUT_BRIEF" "# Task")
  INTENT_BODY=$(fm_brief_marked_captain_words "$TASK_BODY")
fi
if [ -z "$(printf '%s' "$INTENT_BODY" | tr -d '[:space:]')" ]; then
  echo "error: $SCOUT_BRIEF has no provenance-marked Captain's intent; add the captain's actual words before promotion" >&2
  exit 1
fi

# The no-mistakes toggle gates the same mode bin/fm-brief.sh gates for a ship brief:
# the scout's intake block or the project's registry entry may switch it off, and
# promotion must refuse that mode before any write. The resolver owns the precedence,
# and a resolution that fails stops promotion rather than guessing a value.
# The resolver reads every toggle line in the file it is given, so it gets only the
# scout's intake block, as bin/fm-brief.sh passes it for a ship. Quoted configuration
# in the intent section or elsewhere in the brief must not decide the toggle.
PROMOTE_INTAKE=$(mktemp "${TMPDIR:-/tmp}/fm-promote-intake.XXXXXX")
awk '
  /^```moa-intake$/ && !seen { blk = 1; seen = 1; next }
  blk && /^```$/ { exit }
  blk { print }
' "$SCOUT_BRIEF" > "$PROMOTE_INTAKE"
PROMOTE_TOGGLES=$(FM_HOME="$FM_HOME" FM_DATA_OVERRIDE="$DATA" "$FM_ROOT/bin/fm-resolve-toggles.sh" --brief-file "$PROMOTE_INTAKE" "$ID" "${PROMOTE_PROJECT_NAME:-}") || {
  echo "error: cannot resolve the execution toggles for $ID; correct the intake block or the registry entry and promote again" >&2
  exit 1
}
rm -f -- "$PROMOTE_INTAKE"
PROMOTE_INTAKE=
if [ "$MODE" = no-mistakes ] && [ "${PROMOTE_TOGGLES#*no-mistakes=}" = off ]; then
  echo "error: --mode no-mistakes conflicts with no-mistakes=off set for $ID (scout intake block or the registry entry for ${PROMOTE_PROJECT_NAME:-}); promote as direct-PR or local-only, or turn the toggle on and promote again" >&2
  exit 1
fi

# The promoted worker must receive the same delivery contract an ordinary ship
# brief carries, so the mode-specific Definition of done is rendered from its
# single owner (bin/fm-dod-lib.sh) rather than summarised into a hint line. A
# promoted no-mistakes worker that never received the ask-user escalation rule or
# the --yes ban is the delivery hole this file used to leave open.
INSTRUCTIONS="$DATA/$ID/ship-instructions.md"
PROMOTION_ASK_USER_BLOCK=
if [ "$MODE" = no-mistakes ]; then
  PROMOTION_ASK_USER_BLOCK=$(fm_ask_user_escalation_block "$DATA" "$ID")
fi
IFS= read -r -d '' PROMOTION_SHIP_SPEC <<EOF || true
If these promotion steps were already completed before a relaunch, preserve the existing \`$BRANCH_Q\` branch and continue from its current state; do not repeat them destructively.
1. **Verify isolation before anything else.** Run \`pwd -P\` and \`git rev-parse --show-toplevel\`; both must resolve to the disposable task worktree you were launched in, such as a treehouse pool path, not the primary checkout firstmate operates from. If either does not resolve to the worktree you were launched in, stop and escalate to firstmate.
2. Inventory this worktree's scratch state with \`git status\` and \`git log\` before changing anything.
3. Return to a clean $PROMOTE_BASE_WORDS, then create your branch: \`git checkout -b $BRANCH_Q --\`.
4. Carry over only the intended fix changes. Leave scratch commits, debug edits, and experiment files behind.
5. If you reproduced a bug, turn that reproduction into a regression test.
6. Treat the scout-time Firstmate spec and any unmarked legacy \`# Task\` text as investigation context, not captain intent or current ship-time instructions.
7. Everything else in your original instructions carries over unchanged: the status protocol; the instruction inbox and its acknowledgement; the escalation rules, including ask-user; and every safety rule, except where the current delivery contract below explicitly replaces scout-only delivery rules.
EOF
promote_delivery_contract() {
  cat <<EOF
# Current delivery mode contract
This task is now kind=ship with mode=$MODE$PROMOTE_FORGE_WORDS.
This section supersedes every earlier brief instruction about delivery mode.
These current ship instructions supersede the scout delivery rules and report-based Definition of done.
Any earlier "Never push" or scout-only delivery language in this file is superseded.
This replaces the scout rule limiting outside-worktree writes to the report and status file.
Keep project edits inside this worktree; keep proof and scratch output outside it, under \`$DATA/$ID/\` or a temporary directory.
Outside the worktree, write only that task material and the status and steering-inbox records authorized below.
Leave the worktree clean before reporting done.
The mode-specific Definition of done below is the current delivery contract.

# Current ship safety rule
EOF
  fm_ship_rule_one "$MODE" "$ID" "$BRANCH" "$FORGE" "$BASE_BRANCH"
  if [ -n "$PROMOTION_ASK_USER_BLOCK" ]; then
    printf '\nThe no-mistakes ask-user escalation below supersedes the scout rule 6 escalation shape.\n'
    printf '%s\n' "$PROMOTION_ASK_USER_BLOCK"
  fi
  printf '\n'
  fm_dod_block "$MODE" "$ID" "$BRANCH" "$FORGE" "$BASE_BRANCH"
  # The lead-in is the lib's shared constant, so the promoted brief's copy cannot drift
  # from the copy a launch check reads back (bin/fm-assignment-profile-lib.sh).
  printf '\n\n%s\n\n%s\n' "$FM_ASSIGNMENT_PROFILE_DOD_LEAD" "$PROFILE_OBLIGATIONS"
}
mkdir -p "$DATA/$ID"
[ ! -d "$INSTRUCTIONS" ] || { echo "error: ship instructions path is a directory: $INSTRUCTIONS" >&2; exit 1; }
TMP="$DATA/$ID/.ship-instructions.md.${BASHPID:-$$}"
{
  cat <<EOF
Your scout task has been promoted to a ship task, mode=$MODE. Your window, worktree, and context stay as they are; only the contract below changes.

# Task
## Captain's intent
EOF
  printf '%s\n' "$INTENT_BODY"
  cat <<EOF

## Firstmate spec
$PROMOTION_SHIP_SPEC

EOF
  promote_delivery_contract
} > "$TMP" || { echo "error: could not render ship instructions for mode=$MODE" >&2; exit 1; }
mv "$TMP" "$INSTRUCTIONS"
TMP=
[ -f "$INSTRUCTIONS" ] && [ -r "$INSTRUCTIONS" ] || { echo "error: ship instructions were not published as a readable file: $INSTRUCTIONS" >&2; exit 1; }

# The current worker receives the instructions through fm-send, but a replacement
# worker is launched from brief.md. Publish the same explicit precedence contract
# there so a later relaunch cannot revive the original scout delivery rules.
BRIEF_REPLACEMENT="$DATA/$ID/.brief.md.promote.${BASHPID:-$$}"
{
  cat "$SCOUT_BRIEF"
  printf '\n\n'
  printf '# Current ship Firstmate spec\n%s\n\n' "$PROMOTION_SHIP_SPEC"
  promote_delivery_contract
  printf '\n'
  fm_managed_context_section "$ID" "$MODE"
  printf '\n'
  printf '%s\n' "$PROFILE_SECTION"
} > "$BRIEF_REPLACEMENT" || {
  echo "error: could not render the promoted brief for mode=$MODE" >&2
  exit 1
}
# A relaunch composes from this brief, so it must carry blocks the launch accepts.
fm_managed_context_validate "$BRIEF_REPLACEMENT" "$ID" "$MODE" "fm-promote.sh $ID" || exit 1
PROMOTE_MANAGED_OWNERSHIP=$(fm_managed_context_field "$BRIEF_REPLACEMENT" ownership)
PROMOTE_MANAGED_TASK_ID=$(fm_managed_context_field "$BRIEF_REPLACEMENT" task_id)
PROMOTE_MANAGED_HELPER_ALLOWANCE=$(fm_managed_context_field "$BRIEF_REPLACEMENT" helper_allowance | tr -d ' ')
# Persist the canonical record before the brief and meta name it, so a relaunch
# never finds a hash for a file that does not exist.
PROFILE_PATH=$(fm_assignment_profile_path "$DATA/$ID")
fm_assignment_profile_persist "$PROFILE_PATH" "$PROFILE_CANON" || exit 1
PROFILE_SHA=$(fm_assignment_profile_sha "$PROFILE_PATH") || exit 1
PROFILE_META=$(fm_assignment_profile_meta "$PROFILE_CANON" "$PROFILE_SHA") || exit 1
BRIEF_ORIGINAL="$DATA/$ID/.brief.md.scout.${BASHPID:-$$}"
mv "$SCOUT_BRIEF" "$BRIEF_ORIGINAL" || {
  echo "error: could not stage the scout brief for promotion: $SCOUT_BRIEF" >&2
  exit 1
}
if ! mv "$BRIEF_REPLACEMENT" "$SCOUT_BRIEF"; then
  if mv "$BRIEF_ORIGINAL" "$SCOUT_BRIEF" 2>/dev/null; then
    BRIEF_ORIGINAL=
  fi
  echo "error: could not publish the promoted brief: $SCOUT_BRIEF" >&2
  exit 1
fi
BRIEF_REPLACEMENT=

TMP="$STATE/.$ID.meta.promote.${BASHPID:-$$}"
grep -v -e '^kind=' -e '^mode=' -e '^yolo=' -e '^branch=' -e '^managed_' -e '^profile_' "$META" > "$TMP"
{
  echo "kind=ship"
  echo "mode=$MODE"
  echo "yolo=$YOLO"
  echo "branch=$BRANCH"
  echo "managed_ownership=$PROMOTE_MANAGED_OWNERSHIP"
  echo "managed_task_id=$PROMOTE_MANAGED_TASK_ID"
  echo "managed_worktree=$PROMOTE_WORKTREE"
  echo "managed_delivery_mode=$MODE"
  echo "managed_helper_allowance=$PROMOTE_MANAGED_HELPER_ALLOWANCE"
  printf '%s\n' "$PROFILE_META"
} >> "$TMP"
if ! fm_backlog_atomic_transition publish "$TMP" "$META" "task record" "$STATE"; then
  rm -f -- "$TMP"
  TMP=
  echo "error: task record for $ID could not be published ($FM_BACKLOG_TRANSITION_ERROR)" >&2
  exit 1
fi
TMP=
rm -f -- "$BRIEF_ORIGINAL" 2>/dev/null || true
BRIEF_ORIGINAL=
fm_lock_release "$META_LOCK"
META_LOCK_HELD=0

HOME_Q=$(printf '%q' "$FM_HOME")
INSTRUCTIONS_Q=$(printf '%q' "$INSTRUCTIONS")
echo "promoted $ID to ship mode=$MODE yolo=$YOLO$PROMOTE_FORGE_WORDS (teardown protection restored)"
echo "wrote ship instructions for mode=$MODE$PROMOTE_FORGE_WORDS: $INSTRUCTIONS"
echo "next: FM_HOME=$HOME_Q bin/fm-send.sh fm-$ID \"\$(cat $INSTRUCTIONS_Q)\""

promote_warn_parent_unresolved() {
  echo "warning: could not resolve the parent home for secondmate $1; promotion succeeded." >&2
}

if [ -f "$FM_HOME/.fm-secondmate-home" ]; then
  PROMOTE_MATE_ID=$(sed -n '1p' "$FM_HOME/.fm-secondmate-home" 2>/dev/null || true)
  PROMOTE_PARENT_RECORD=absent
  PROMOTE_PARENT_ROUTE=
  if [ -e "$FM_HOME/.fm-secondmate-parent" ] || [ -L "$FM_HOME/.fm-secondmate-parent" ]; then
    PROMOTE_PARENT_RECORD=invalid
    if fm_secondmate_parent_record_parse "$FM_HOME/.fm-secondmate-parent"; then
      PROMOTE_PARENT_RECORD=valid
      PROMOTE_PARENT_ROUTE=$FM_SECONDMATE_PARENT_ROUTE
    fi
  fi
  if [ "$PROMOTE_PARENT_RECORD" = invalid ]; then
    promote_warn_parent_unresolved "$PROMOTE_MATE_ID"
  elif [ "$PROMOTE_PARENT_ROUTE" = local ]; then
    # Local parent: shared resolver for consent home and state
    if fm_secondmate_parent_resolve "$FM_HOME" "$PROMOTE_MATE_ID" ""; then
      : # Local parent resolved
    else
      promote_warn_parent_unresolved "$PROMOTE_MATE_ID"
    fi
  fi
fi
