#!/usr/bin/env bash
# Hand already-identified, in-scope backlog items off from the main firstmate
# backlog to a secondmate's own home backlog. Use this when a secondmate is
# created (or whenever an existing queued item should become its domain's work)
# so the secondmate owns its queue from day one instead of the item staying
# stranded in the main backlog.
#
# Scope-matching is firstmate's JUDGMENT: you pass the task-id keys you have
# already judged in-scope for the secondmate. This script performs only the
# fleet-level validation that the backlog backend cannot know, then DELEGATES
# the actual item move to `tasks-axi mv`, the single owner of the backlog
# format. Delegating the move is the durability end-state: it removes the awk
# that used to re-implement block extraction and insertion here, so the format
# has exactly one parser and cannot drift out of sync (the body-orphaning class
# of bug fixed in PR #401 was exactly that drift).
#
# What this script still owns (never delegated):
#   - resolving the secondmate home from data/secondmates.md;
#   - proving the destination is a genuine seeded secondmate home
#     (.fm-secondmate-home marker, AGENTS.md + bin/), never a project clone, the
#     active home, or the firstmate repo;
#   - moving only `## Queued` items, refusing `## In flight` and historical
#     `## Done` records, which must stay with their home for pruning or
#     archiving;
#   - the multi-key classification and idempotent per-key reporting: a key
#     already present in the secondmate backlog is reported and skipped, and if
#     any key matches neither backlog nothing is moved.
#
# What `tasks-axi mv <id>... --to <dest>` owns: moving each full item BLOCK
# byte-exact (header, body lines, blank separators, and indented pseudo-headings
# such as `  ## Intent`), preserving destination section placement, and moving a
# whole connected set (a blocker and its dependents) atomically with blocked-by
# links preserved. It refuses a move that would strand a dependency across the
# two files; that error is surfaced verbatim and nothing is moved.
#
# Item bodies must use at least two leading spaces. The helper refuses a selected
# item with a single-space or tab-indented continuation rather than risk leaving
# it orphaned, because tasks-axi treats only two-or-more-space lines as body.
# The move needs compatible `tasks-axi` on PATH, including atomic multi-ID `mv`
# support. Bootstrap requires a compatible build fleet-wide, so this works
# everywhere; the `config/backlog-backend=manual` knob only governs firstmate's
# own hand-editing of its own backlog, not this validated helper. Idempotent:
# re-running converges. Atomic: on any move failure nothing moves.
# Every move names its markdown backlog files explicitly and pins
# `--backend markdown`, so a `.tasks.toml` in the caller's working directory
# cannot redirect the move to another backend.
# See AGENTS.md project management and task lifecycle.
# Every newly durable backlog delivery attempts one marked wake to the receiving
# endpoint. The move lands directly in the destination backlog, and a missing or
# rejected wake makes this command fail with the move intact so rerunning the
# same handoff retries its prepared wake intent. A prepared wake is bound to the
# exact sorted requested-key batch; an unrelated handoff to that mate refuses
# until the original batch is retried, so it cannot discard wake intent for work
# that already moved. An undelivered pending wake is retried by the next handoff
# to that mate, which refuses until it is delivered. No two-phase journal exists.
# Usage: fm-backlog-handoff.sh <secondmate-id> <item-key>...
set -eu

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
FM_ROOT="${FM_ROOT_OVERRIDE:-$(cd "$SCRIPT_DIR/.." && pwd)}"
FM_HOME="${FM_HOME:-${FM_ROOT_OVERRIDE:-$FM_ROOT}}"
DATA="${FM_DATA_OVERRIDE:-$FM_HOME/data}"
STATE="${FM_STATE_OVERRIDE:-$FM_HOME/state}"
REG="$DATA/secondmates.md"
MAIN_BACKLOG="$DATA/backlog.md"
# shellcheck source=bin/fm-tasks-axi-lib.sh disable=SC1091
. "$SCRIPT_DIR/fm-tasks-axi-lib.sh"
# shellcheck source=bin/fm-secondmate-registry-lib.sh
. "$SCRIPT_DIR/fm-secondmate-registry-lib.sh"
# shellcheck source=bin/fm-wake-lib.sh
. "$SCRIPT_DIR/fm-wake-lib.sh"
# shellcheck source=bin/fm-pending-reply-lib.sh
. "$SCRIPT_DIR/fm-pending-reply-lib.sh"

RECEIVER_WAKE_MESSAGE='New routed work is in your backlog. Run bin/fm-session-start.sh now, then act on the routed task.'

ACTIVE_HANDOFF_LOCK=
ACTIVE_REGISTRY_LOCK=
release_handoff_locks() {
  if [ -n "$ACTIVE_HANDOFF_LOCK" ]; then
    fm_lock_release "$ACTIVE_HANDOFF_LOCK"
    ACTIVE_HANDOFF_LOCK=
  fi
  if [ -n "$ACTIVE_REGISTRY_LOCK" ]; then
    fm_lock_release "$ACTIVE_REGISTRY_LOCK"
    ACTIVE_REGISTRY_LOCK=
  fi
}
trap release_handoff_locks EXIT
trap 'exit 1' HUP INT TERM

[ "$#" -ge 2 ] || {
  echo "usage: fm-backlog-handoff.sh <secondmate-id> <item-key>..." >&2
  exit 1
}
ID=$1
case "$ID" in '' | *[!A-Za-z0-9._-]*)
  echo "error: unsafe secondmate id: $ID" >&2
  exit 1
  ;;
esac
shift

secondmate_home() {
  local id=$1 home
  [ -f "$REG" ] || {
    echo "error: no secondmate registry at $REG" >&2
    return 1
  }
  home=$(secondmate_registry_field "$REG" "$id" home || true)
  [ -n "$home" ] || {
    echo "error: secondmate $id has no home in $REG" >&2
    return 1
  }
  printf '%s\n' "$home"
}

path_is_ancestor_of() {
  local ancestor=$1 path=$2
  [ -n "$ancestor" ] || return 1
  [ -n "$path" ] || return 1
  [ "$ancestor" != "$path" ] || return 1
  case "$path" in
  "$ancestor"/*) return 0 ;;
  esac
  return 1
}

resolved_existing_dir() {
  local path=$1
  [ -d "$path" ] || {
    echo "error: firstmate home does not exist or is not a directory: $path" >&2
    return 1
  }
  cd "$path" && pwd -P
}

validate_operational_dirs() {
  local abs_home=$1 abs_active_home=$2 abs_root=$3 name dir abs_dir
  for name in data state config projects; do
    dir="$abs_home/$name"
    if [ -L "$dir" ] && [ ! -e "$dir" ]; then
      echo "error: secondmate $name directory must resolve inside the secondmate home: $dir" >&2
      return 1
    fi
    if [ -d "$dir" ]; then
      abs_dir=$(cd "$dir" && pwd -P)
    elif [ -e "$dir" ]; then
      echo "error: secondmate $name path is not a directory: $dir" >&2
      return 1
    else
      abs_dir="$abs_home/$name"
    fi
    if ! path_is_ancestor_of "$abs_home" "$abs_dir"; then
      echo "error: secondmate $name directory must resolve inside the secondmate home: $dir" >&2
      return 1
    fi
    if [ "$abs_dir" = "$abs_active_home" ] || path_is_ancestor_of "$abs_active_home" "$abs_dir"; then
      echo "error: secondmate $name directory cannot be inside the active firstmate home: $dir" >&2
      return 1
    fi
    if [ "$abs_dir" = "$abs_root" ] || path_is_ancestor_of "$abs_root" "$abs_dir"; then
      echo "error: secondmate $name directory cannot be inside the firstmate repo: $dir" >&2
      return 1
    fi
  done
}

validate_secondmate_home() {
  local id=$1 home=$2 abs_home abs_active_home abs_root marker_id
  abs_home=$(resolved_existing_dir "$home") || return 1
  abs_active_home=$(resolved_existing_dir "$FM_HOME")
  abs_root=$(resolved_existing_dir "$FM_ROOT")
  if [ "$abs_home" = "/" ]; then
    echo "error: secondmate home cannot be the filesystem root: $home" >&2
    return 1
  fi
  if [ "$abs_home" = "$abs_active_home" ]; then
    echo "error: secondmate home cannot be the active firstmate home: $home" >&2
    return 1
  fi
  if [ "$abs_home" = "$abs_root" ]; then
    echo "error: secondmate home cannot be the firstmate repo: $home" >&2
    return 1
  fi
  if path_is_ancestor_of "$abs_active_home" "$abs_home"; then
    echo "error: secondmate home cannot be inside the active firstmate home: $home" >&2
    return 1
  fi
  if path_is_ancestor_of "$abs_root" "$abs_home"; then
    echo "error: secondmate home cannot be inside the firstmate repo: $home" >&2
    return 1
  fi
  if path_is_ancestor_of "$abs_home" "$abs_active_home"; then
    echo "error: secondmate home cannot be an ancestor of the active firstmate home: $home" >&2
    return 1
  fi
  if path_is_ancestor_of "$abs_home" "$abs_root"; then
    echo "error: secondmate home cannot be an ancestor of the firstmate repo: $home" >&2
    return 1
  fi
  validate_operational_dirs "$abs_home" "$abs_active_home" "$abs_root" || return 1
  if [ ! -f "$abs_home/.fm-secondmate-home" ]; then
    echo "error: firstmate home $home is not a seeded secondmate home" >&2
    return 1
  fi
  marker_id=$(cat "$abs_home/.fm-secondmate-home" 2>/dev/null || true)
  if [ "$marker_id" != "$id" ]; then
    echo "error: firstmate home $home is marked for secondmate ${marker_id:-unknown}, expected $id" >&2
    return 1
  fi
  if [ ! -f "$abs_home/AGENTS.md" ]; then
    echo "error: $home is not a firstmate home (missing AGENTS.md)" >&2
    return 1
  fi
  if [ ! -d "$abs_home/bin" ]; then
    echo "error: $home is not a firstmate home (missing bin/)" >&2
    return 1
  fi
  printf '%s\n' "$abs_home"
}

validate_backlog_file() {
  local label=$1 path=$2
  if [ -L "$path" ]; then
    echo "error: $label must not be a symlink: $path" >&2
    return 1
  fi
  if [ -e "$path" ] && [ ! -f "$path" ]; then
    echo "error: $label is not a regular file: $path" >&2
    return 1
  fi
}

# Classify a single key by the section it lives under (## In flight /
# ## Queued / ## Done), or return non-zero if no `- [ ] <key>` / `- [x] <key>`
# header exists in the file. This reads only section headings and item header
# lines - never item bodies - so it drives the fleet-level classification (in-
# flight refusal, already-present idempotency, missing-key abort) without
# re-implementing the block/body move semantics that tasks-axi mv owns.
backlog_key_section() {
  local file=$1 key=$2
  [ -f "$file" ] || return 1
  awk -v key="$key" '
    BEGIN { section = "## Queued" }
    /^##[[:space:]]+/ {
      section = $0
      sub(/^##[[:space:]]+/, "## ", section)
      sub(/[[:space:]]+$/, "", section)
      next
    }
    /^- \[[ x]\] / {
      rest = $0
      sub(/^- \[[ x]\] +/, "", rest)
      id = rest
      sub(/[ \t].*/, "", id)
      if (id == key) { print section; found = 1; exit }
    }
    END { exit found ? 0 : 1 }
  ' "$file"
}

backlog_key_noncanonical_body_lines() {
  local file=$1 key=$2
  awk -v key="$key" '
    /^- \[[ x]\] / {
      rest = $0
      sub(/^- \[[ x]\] +/, "", rest)
      id = rest
      sub(/[ \t].*/, "", id)
      if (capturing) exit
      if (id == key) { capturing = 1 }
      next
    }
    capturing && /^##[[:space:]]+/ { exit }
    capturing && /^[[:space:]]/ && !/^  / && /[^[:space:]]/ { print }
  ' "$file"
}

# Wake a live receiver after its backlog has become durable. The marked message
# uses the normal endpoint route and verified submit. A
# failed wake fails that handoff with its move intact, and rerunning the handoff
# retries it. A
# seeded but not-yet-spawned home is a valid handoff destination, but its missing
# endpoint is reported rather than pretending the task was started.
receiver_wake_batch_id() { # <item-key>...
  local digest
  if command -v shasum >/dev/null 2>&1; then
    digest=$(printf '%s\n' "$@" | LC_ALL=C sort | shasum -a 256 2>/dev/null | awk '{print $1}')
  else
    digest=$(printf '%s\n' "$@" | LC_ALL=C sort | sha256sum 2>/dev/null | awk '{print $1}')
  fi
  printf '%s' "$digest" | grep -Eq '^[a-f0-9]{64}$' || return 1
  printf '%s' "${digest:0:16}"
}

receiver_wake_state_write() { # <secondmate-id> <state>
  local id=$1 value=$2 marker="$STATE/.backlog-handoff-$1.wake-pending" tmp
  case "$id" in '' | *[!A-Za-z0-9._-]*) return 1 ;; esac
  case "$value" in
  pending | confirmed) ;;
  prepared:*) printf '%s' "$value" | grep -Eq '^prepared:[a-f0-9]{16}:[a-f0-9]{16}$' || return 1 ;;
  pending:*) printf '%s' "$value" | grep -Eq '^pending:[a-f0-9]{16}$' || return 1 ;;
  confirmed:*) printf '%s' "$value" | grep -Eq '^confirmed:[a-f0-9]{16}$' || return 1 ;;
  *) return 1 ;;
  esac
  tmp=$(
    umask 077
    mktemp "$STATE/.backlog-handoff-wake.XXXXXX"
  ) || return 1
  if ! printf '%s\n' "$value" >"$tmp" || ! chmod 600 "$tmp" || ! mv -f -- "$tmp" "$marker"; then
    rm -f -- "$tmp"
    return 1
  fi
}

receiver_wake_mark() { # <secondmate-id> <prepared|pending> [batch-id]
  local id=$1 wake_phase=$2 batch=${3:-} marker="$STATE/.backlog-handoff-$1.wake-pending" value corr rec
  local wake_state
  case "$wake_phase" in prepared | pending) ;; *) return 1 ;; esac
  if [ -e "$marker" ] || [ -L "$marker" ]; then
    [ -f "$marker" ] && [ ! -L "$marker" ] || return 1
    value=$(cat "$marker" 2>/dev/null || true)
    case "$value" in
    prepared:*)
      corr=${value#*:}
      corr=${corr%%:*}
      rec=$(fm_pending_reply_path "$STATE" "$corr")
      [ -f "$rec" ] && [ ! -L "$rec" ] &&
        [ "$(fm_pending_reply_get "$rec" task_id)" = "$id" ]
      return $?
      ;;
    pending:*)
      receiver_wake_pending_valid "$id"
      return $?
      ;;
    pending) ;;
    *) return 1 ;;
    esac
  fi
  corr=$(fm_pending_reply_create "$FM_HOME" "$STATE" "$id" "$RECEIVER_WAKE_MESSAGE") || return 1
  wake_state="$wake_phase:$corr"
  if [ "$wake_phase" = prepared ]; then
    printf '%s' "$batch" | grep -Eq '^[a-f0-9]{16}$' || return 1
    wake_state="$wake_state:$batch"
  fi
  if ! receiver_wake_state_write "$id" "$wake_state"; then
    fm_pending_reply_discard_undelivered "$STATE" "$corr" || true
    return 1
  fi
}

receiver_wake_mark_pending() { # <secondmate-id>
  receiver_wake_mark "$1" pending
}

receiver_wake_mark_prepared() { # <secondmate-id> <batch-id>
  receiver_wake_mark "$1" prepared "$2"
}

receiver_wake_discard_prepared() { # <secondmate-id>
  local id=$1 marker="$STATE/.backlog-handoff-$1.wake-pending" value corr
  [ -f "$marker" ] && [ ! -L "$marker" ] || return 1
  value=$(cat "$marker" 2>/dev/null || true)
  case "$value" in
  prepared:*)
    corr=${value#prepared:}
    corr=${corr%%:*}
    ;;
  *) return 1 ;;
  esac
  fm_pending_reply_discard_undelivered "$STATE" "$corr" || return 1
  rm -f -- "$marker"
}

receiver_wake_promote_prepared() { # <secondmate-id> <batch-id>
  local id=$1 batch=$2 marker="$STATE/.backlog-handoff-$1.wake-pending" value corr
  [ -f "$marker" ] && [ ! -L "$marker" ] || return 1
  value=$(cat "$marker" 2>/dev/null || true)
  case "$value" in
  prepared:*:"$batch")
    corr=${value#prepared:}
    corr=${corr%%:*}
    ;;
  pending:*) return 0 ;;
  *) return 1 ;;
  esac
  receiver_wake_state_write "$id" "pending:$corr"
}

receiver_wake_pending_valid() { # <secondmate-id>
  local id=$1 marker="$STATE/.backlog-handoff-$1.wake-pending" value corr rec delivered
  [ -f "$marker" ] && [ ! -L "$marker" ] || return 1
  value=$(cat "$marker" 2>/dev/null || true)
  case "$value" in pending:*) corr=${value#pending:} ;; *) return 1 ;; esac
  printf '%s' "$corr" | grep -Eq '^[a-f0-9]{16}$' || return 1
  rec=$(fm_pending_reply_path "$STATE" "$corr")
  [ -f "$rec" ] && [ ! -L "$rec" ] &&
    [ "$(fm_pending_reply_get "$rec" task_id)" = "$id" ] || return 1
  delivered=$(fm_pending_reply_get "$rec" delivered_epoch)
  [ -z "$delivered" ] || return 1
  fm_pending_reply_corr_reusable "$STATE" "$corr" "$id"
}

wake_secondmate_receiver() { # <secondmate-id> <correlation-id>
  local id=$1 corr=$2 meta="$STATE/$1.meta" out rc=0
  if [ ! -f "$meta" ] || [ -L "$meta" ]; then
    printf 'error: handed off work to secondmate %s, but no live receiver endpoint is recorded; the destination backlog is durable and the receiver was not woken\n' "$id" >&2
    return 1
  fi
  [ "$(grep '^kind=' "$meta" | cut -d= -f2-)" = secondmate ] || {
    printf 'error: secondmate %s has non-secondmate endpoint metadata; backlog is durable but the receiver was not woken\n' "$id" >&2
    return 1
  }
  out=$(FM_HOME="$FM_HOME" FM_STATE_OVERRIDE="$STATE" FM_ROOT_OVERRIDE="$FM_ROOT" \
    FM_PENDING_REPLY_EXISTING_CORR="$corr" \
    "$SCRIPT_DIR/fm-send.sh" "$id" "$RECEIVER_WAKE_MESSAGE" 2>&1) || rc=$?
  if [ "$rc" -ne 0 ]; then
    [ -z "$out" ] || printf '%s\n' "$out" >&2
    printf 'error: backlog delivery to secondmate %s succeeded, but its receiver wake failed; rerun this handoff to retry the wake\n' "$id" >&2
    return 1
  fi
  [ -z "$out" ] || printf '%s\n' "$out"
}

wake_pending_secondmate_receiver() { # <secondmate-id>
  local id=$1 marker="$STATE/.backlog-handoff-$1.wake-pending" value corr rec delivered
  [ -e "$marker" ] || [ -L "$marker" ] || return 0
  if [ ! -f "$marker" ] || [ -L "$marker" ]; then
    printf 'error: receiver wake state for secondmate %s is unsafe or invalid\n' "$id" >&2
    return 1
  fi
  value=$(cat "$marker" 2>/dev/null || true)
  case "$value" in
  confirmed | confirmed:*) return 0 ;;
  prepared | prepared:*)
    printf 'error: receiver wake for secondmate %s was prepared before its backlog became durable\n' "$id" >&2
    return 1
    ;;
  pending)
    receiver_wake_mark_pending "$id" || return 1
    value=$(cat "$marker" 2>/dev/null || true)
    ;;
  esac
  case "$value" in pending:*) corr=${value#pending:} ;; *)
    printf 'error: receiver wake state for secondmate %s is unsafe or invalid\n' "$id" >&2
    return 1
    ;;
  esac
  rec=$(fm_pending_reply_path "$STATE" "$corr")
  [ -f "$rec" ] && [ ! -L "$rec" ] &&
    [ "$(fm_pending_reply_get "$rec" task_id)" = "$id" ] || return 1
  fm_pending_reply_reconcile_delivery "$STATE" "$corr" >/dev/null 2>&1 || true
  delivered=$(fm_pending_reply_get "$rec" delivered_epoch)
  if [ -z "$delivered" ]; then
    fm_pending_reply_corr_reusable "$STATE" "$corr" "$id" || {
      printf 'error: receiver wake delivery for secondmate %s is unresolved; refusing to resend correlation %s\n' "$id" "$corr" >&2
      return 1
    }
    wake_secondmate_receiver "$id" "$corr" || return 1
  fi
  rm -f -- "$marker" || {
    printf 'error: receiver wake for secondmate %s was confirmed, but pending state could not be cleared\n' "$id" >&2
    return 1
  }
}

ACTIVE_REGISTRY_LOCK=$(secondmate_registry_lock_path "$STATE")
fm_lock_acquire_wait "$ACTIVE_REGISTRY_LOCK"
ACTIVE_HANDOFF_LOCK="$STATE/.backlog-handoff-$ID.lock"
fm_lock_acquire_wait "$ACTIVE_HANDOFF_LOCK"
fm_lock_release "$ACTIVE_REGISTRY_LOCK"
ACTIVE_REGISTRY_LOCK=

RAW_HOME=$(secondmate_home "$ID") || exit 1
[ -n "$RAW_HOME" ] || {
  echo "error: secondmate $ID has no home in $REG" >&2
  exit 1
}
SUB_HOME=$(validate_secondmate_home "$ID" "$RAW_HOME") || exit 1
SUB_BACKLOG="$SUB_HOME/data/backlog.md"
validate_backlog_file "main backlog" "$MAIN_BACKLOG" || exit 1
validate_backlog_file "secondmate backlog" "$SUB_BACKLOG" || exit 1

# Classify every key before changing anything: move-from-main, already-in-sub, or
# missing. Abort with no changes if any key matches neither backlog.
TO_MOVE=()
ALREADY=()
MISSING=()
IN_FLIGHT=()
DONE=()
NOT_QUEUED=()
for key in "$@"; do
  if backlog_key_section "$SUB_BACKLOG" "$key" >/dev/null; then
    ALREADY+=("$key")
  elif section=$(backlog_key_section "$MAIN_BACKLOG" "$key"); then
    case "$section" in
    "## Queued") TO_MOVE+=("$key") ;;
    "## In flight") IN_FLIGHT+=("$key") ;;
    "## Done") DONE+=("$key") ;;
    *) NOT_QUEUED+=("$key") ;;
    esac
  else
    MISSING+=("$key")
  fi
done

FAILED=0
if [ "${#IN_FLIGHT[@]}" -gt 0 ]; then
  echo "error: refusing to hand off in-flight backlog items: ${IN_FLIGHT[*]}" >&2
  FAILED=1
fi
if [ "${#DONE[@]}" -gt 0 ]; then
  echo "error: refusing to hand off Done (historical) backlog items: ${DONE[*]}; handoffs move in-scope queued work only - Done records stay with their home and are pruned/archived." >&2
  FAILED=1
fi
if [ "${#NOT_QUEUED[@]}" -gt 0 ]; then
  echo "error: refusing to hand off non-queued backlog items: ${NOT_QUEUED[*]}; handoffs move in-scope queued work only." >&2
  FAILED=1
fi
if [ "${#MISSING[@]}" -gt 0 ]; then
  echo "error: no backlog item matched these keys in $MAIN_BACKLOG: ${MISSING[*]}" >&2
  FAILED=1
fi
if [ "$FAILED" -ne 0 ]; then
  echo "       nothing was moved." >&2
  exit 1
fi

REQUESTED_BATCH=$(receiver_wake_batch_id "$@") || {
  echo "error: receiver wake batch identity could not be recorded; nothing was moved" >&2
  exit 1
}

if [ "${#TO_MOVE[@]}" -eq 0 ]; then
  WAKE_PENDING_MARKER="$STATE/.backlog-handoff-$ID.wake-pending"
  case "$(cat "$WAKE_PENDING_MARKER" 2>/dev/null || true)" in
  prepared:*:"$REQUESTED_BATCH") receiver_wake_promote_prepared "$ID" "$REQUESTED_BATCH" || exit 1 ;;
  prepared:*)
    echo "error: a prepared receiver wake for secondmate $ID belongs to a different routed batch; retry that original handoff before handling ${ALREADY[*]}" >&2
    exit 1
    ;;
  esac
  echo "nothing to move: ${ALREADY[*]:-no keys} already present in $SUB_BACKLOG"
  wake_pending_secondmate_receiver "$ID" || exit 1
  exit 0
fi

FAILED=0
for key in "${TO_MOVE[@]}"; do
  while IFS= read -r line; do
    printf 'error: refusing to hand off %s: non-2-space continuation line: %s\n' \
      "$key" "$line" >&2
    FAILED=1
  done < <(backlog_key_noncanonical_body_lines "$MAIN_BACKLOG" "$key")
done
if [ "$FAILED" -ne 0 ]; then
  echo "       nothing was moved." >&2
  exit 1
fi

if ! fm_tasks_axi_compatible; then
  echo "error: a compatible tasks-axi with atomic multi-ID mv support is required to move backlog items; run bin/fm-bootstrap.sh for the required version" >&2
  exit 1
fi

WAKE_PENDING_MARKER="$STATE/.backlog-handoff-$ID.wake-pending"
if [ -e "$WAKE_PENDING_MARKER" ] || [ -L "$WAKE_PENDING_MARKER" ]; then
  case "$(cat "$WAKE_PENDING_MARKER" 2>/dev/null || true)" in
  prepared:*:"$REQUESTED_BATCH") receiver_wake_discard_prepared "$ID" || exit 1 ;;
  prepared:*)
    echo "error: a prepared receiver wake for secondmate $ID belongs to a different routed batch; retry that original handoff before moving ${TO_MOVE[*]}" >&2
    exit 1
    ;;
  *)
    wake_pending_secondmate_receiver "$ID" || {
      echo "error: previous receiver wake for secondmate $ID is unresolved; nothing new was moved" >&2
      exit 1
    }
    ;;
  esac
fi
receiver_wake_mark_prepared "$ID" "$REQUESTED_BATCH" || {
  echo "error: receiver wake state for secondmate $ID could not be recorded; nothing was moved" >&2
  exit 1
}

# Seed the destination with firstmate's standard three-section scaffold when it
# does not exist yet, so the moved item lands under the right section. (Left to
# create the file itself, tasks-axi mv writes its own `# Backlog` title format,
# which is not firstmate's home-backlog convention.)
mkdir -p "$SUB_HOME/data"
SUB_CREATED=0
if [ ! -f "$SUB_BACKLOG" ]; then
  printf '## In flight\n\n## Queued\n\n## Done\n' >"$SUB_BACKLOG"
  SUB_CREATED=1
fi

# Delegate the move to tasks-axi. Passing the whole in-scope set to one call is a
# single atomic transaction, so a connected set (blocker + dependents) moves
# together and, on any failure, neither backlog's content changes - the only
# cleanup is a scaffold we just created. tasks-axi writes both its success and
# error output to stdout, so capture it and surface it only on failure.
if ! MV_OUT=$(tasks-axi mv "${TO_MOVE[@]}" --file "$MAIN_BACKLOG" --to "$SUB_BACKLOG" --backend markdown 2>&1); then
  if [ "$SUB_CREATED" -eq 1 ]; then
    rm -f "$SUB_BACKLOG"
  fi
  receiver_wake_discard_prepared "$ID" || {
    echo "error: tasks-axi mv failed and receiver wake state could not be cleared" >&2
    exit 1
  }
  if [ -n "$MV_OUT" ]; then
    printf '%s\n' "$MV_OUT" >&2
  fi
  echo "error: tasks-axi mv failed; nothing was moved." >&2
  exit 1
fi

echo "handed off ${#TO_MOVE[@]} item(s) to $ID: ${TO_MOVE[*]}"
echo "  into $SUB_BACKLOG"
receiver_wake_promote_prepared "$ID" "$REQUESTED_BATCH" || {
  echo "error: handed off work to secondmate $ID, but durable receiver wake state could not be recorded" >&2
  exit 1
}
wake_pending_secondmate_receiver "$ID" || exit 1
if [ "${#ALREADY[@]}" -gt 0 ]; then
  echo "  already present (skipped): ${ALREADY[*]}"
fi
