#!/usr/bin/env bash
# fm-secondmate-parent-migrate.sh - primary-owned migration of registered local
# secondmate parent records onto the explicit effective layout.
#
# WHY. A secondmate's durable parent record (.fm-secondmate-parent, owned by
# bin/fm-secondmate-parent-lib.sh) was written as v1: one parent_home, from which
# the primary's state and data were derived. A primary that keeps its state,
# data, or config outside its code root (FM_STATE_OVERRIDE, FM_DATA_OVERRIDE,
# and kin) cannot be described by that layout, so teardown and promotion refused
# every cleanup as a mismatch. This migration records the primary's effective
# layout explicitly, so one resolver (fm_secondmate_parent_resolve) answers for
# seed, launch, relaunch, teardown, promotion, and parent-channel routing alike.
#
# WHAT IT CHECKS BEFORE IT WRITES. Any failed check refuses the whole run.
#   - FM_HOME is a main home: no .fm-secondmate-home identity marker.
#   - FM_HOME, STATE, and DATA are existing absolute directories.
#   - The registry lock is free, then the registry parses and binds each id and
#     home uniquely, with no home nested inside another
#     (secondmate_registry_validate_bindings).
#   - Each registered local secondmate <id> at <home>:
#     - <home> is its canonical path and its identity marker names <id>;
#     - STATE/<id>.meta is a regular file with kind=secondmate and home=<home>;
#     - <home>/.fm-secondmate-parent exists, is a local record, and has one of
#       the accepted bindings below.
#   Accepted existing bindings:
#     - v1 with parent_home = FM_HOME (the code-root spelling);
#     - v1 with parent_home = the state-owning directory, the parent of STATE
#       when that parent is named state and its data is DATA (the spelling the
#       split-layout seeds wrote);
#     - explicit, already naming FM_HOME as primary, STATE, and DATA, with
#       parent_home equal to FM_HOME or the state-owning directory (a repeat run).
#   A missing record, a missing metadata record, a foreign marker, or any other
#   binding refuses. The migration never creates a binding.
#
# WHAT IT WRITES. Only the registered local children's .fm-secondmate-parent
# records, and only when one is still v1. The new record is explicit:
#   - parent_home is the state-owning directory when one exists (the walker in
#     bin/fm-wake-lib.sh walks it for state/ and data/), else FM_HOME;
#   - parent_primary is FM_HOME, parent_state is STATE, parent_data is DATA;
#   - parent_consent keeps the consent home the v1 record already implied,
#     so no record-level behavior changes through the migration.
# Records are staged beside their originals, backed up, installed, and then
# verified through the shared resolver. Any failure restores every installed
# record from its backup byte for byte. Nothing else is read for writing or
# touched: no backlog, worktree, metadata, registry, or unrelated record.
#
# Repeat runs are safe: once every record is explicit, the run changes nothing.
#
# The registry lock (secondmate_registry_lock_path under STATE) is held for the
# whole run, so it never races a seed or another registry change; a held lock
# refuses rather than waiting.
#
# Usage: run from the primary, with the environment the primary launches with:
#   FM_HOME=<primary code root> [FM_STATE_OVERRIDE=...] [FM_DATA_OVERRIDE=...] \
#     bin/fm-secondmate-parent-migrate.sh
# Exit status: 0 migrated or already current, 1 refused (nothing left changed),
# 2 usage error.

set -eu

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
FM_ROOT="${FM_ROOT_OVERRIDE:-$(cd "$SCRIPT_DIR/.." && pwd)}"
FM_HOME="${FM_HOME:-${FM_ROOT_OVERRIDE:-$FM_ROOT}}"
DATA="${FM_DATA_OVERRIDE:-$FM_HOME/data}"
STATE="${FM_STATE_OVERRIDE:-$FM_HOME/state}"
REG="$DATA/secondmates.md"
SUB_HOME_PARENT_MARKER=".fm-secondmate-parent"
# shellcheck source=bin/fm-backend.sh
. "$SCRIPT_DIR/fm-backend.sh"
# shellcheck source=bin/fm-wake-lib.sh
. "$SCRIPT_DIR/fm-wake-lib.sh"
# shellcheck source=bin/fm-secondmate-registry-lib.sh
. "$SCRIPT_DIR/fm-secondmate-registry-lib.sh"
# shellcheck source=bin/fm-secondmate-parent-lib.sh
. "$SCRIPT_DIR/fm-secondmate-parent-lib.sh"
# shellcheck source=bin/fm-parent-channel-lib.sh
. "$SCRIPT_DIR/fm-parent-channel-lib.sh"

migrate_usage() {
  cat <<'EOF'
usage: fm-secondmate-parent-migrate.sh [--help]

Migrates every registered local secondmate's durable parent record onto the
explicit primary layout (primary, state, data, and consent recorded per record).
Run it from the primary with the environment the primary launches with: FM_HOME,
plus FM_STATE_OVERRIDE and FM_DATA_OVERRIDE where the primary uses them.
It validates the marker, metadata, registry uniqueness, and exact child path
before any write, refuses any binding it does not recognize, and restores every
record byte for byte if verification fails. Repeat runs are safe.
The script header owns the full checks and writes.
EOF
}

case "${1-}" in
  '') ;;
  -h|--help)
    migrate_usage
    exit 0
    ;;
  *)
    migrate_usage >&2
    exit 2
    ;;
esac
[ "$#" -le 1 ] || {
  migrate_usage >&2
  exit 2
}

migrate_refuse() {
  printf 'REFUSED: %s\n' "$*" >&2
  exit 1
}

# Staged, installed, and verified state. Indexed by registry entry position.
MIGRATE_LOCK=
MIGRATE_LOCK_HELD=0
MIGRATE_TMP=
MIGRATE_COMMITTED=0
MIGRATE_INSTALLED_COUNT=0
MIGRATE_RESTORE_FAILED=0
PRIMARY=
PRIMARY_STATE=
PRIMARY_DATA=
STATE_OWNER=
mig_count=0
mig_ids=()
mig_homes=()
mig_home_c=()
mig_action=()
mig_record=()
mig_parent_home=()
mig_consent=()
mig_cand=()
mig_installed=()

# Restore every installed record from its backup. Returns non-zero if any
# restore failed, in which case the backups stay in MIGRATE_TMP for recovery.
migrate_restore_installed() {
  local k=0 i failed=0
  while [ "$k" -lt "$MIGRATE_INSTALLED_COUNT" ]; do
    i=${mig_installed[$k]}
    if ! cp -p -- "$MIGRATE_TMP/$i" "${mig_record[$i]}.restore.$$" ||
       ! mv -f -- "${mig_record[$i]}.restore.$$" "${mig_record[$i]}"; then
      printf 'error: could not restore %s; its original bytes are kept at %s\n' \
        "${mig_record[$i]}" "$MIGRATE_TMP/$i" >&2
      failed=1
    fi
    k=$((k + 1))
  done
  if [ "$failed" -eq 0 ]; then
    MIGRATE_INSTALLED_COUNT=0
  else
    MIGRATE_RESTORE_FAILED=1
  fi
  return "$failed"
}

migrate_cleanup() {
  local i=0
  if [ "$MIGRATE_INSTALLED_COUNT" -gt 0 ] && [ "$MIGRATE_COMMITTED" -eq 0 ]; then
    migrate_restore_installed || true
  fi
  while [ "$i" -lt "$mig_count" ]; do
    [ -z "${mig_cand[$i]-}" ] || rm -f -- "${mig_cand[$i]}"
    i=$((i + 1))
  done
  if [ -n "$MIGRATE_TMP" ] && [ "$MIGRATE_RESTORE_FAILED" -eq 0 ]; then
    rm -rf -- "$MIGRATE_TMP"
  fi
  if [ "$MIGRATE_LOCK_HELD" -eq 1 ]; then
    fm_lock_release "$MIGRATE_LOCK" || true
    MIGRATE_LOCK_HELD=0
  fi
}
trap migrate_cleanup EXIT
trap 'exit 130' INT TERM

# True when <path> resolves to exactly <expected>, a canonical path.
migrate_same_path() {  # <path> <canonical-expected>
  local got
  [ -n "$2" ] || return 1
  got=$(fm_secondmate_parent_canonical "$1") || return 1
  [ "$got" = "$2" ]
}

# Classify one registered local secondmate and record its planned action.
migrate_plan_entry() {  # <index> <id> <registered-home>
  local i=$1 id=$2 home=$3 home_c marker_id rc=0 meta meta_home meta_home_c
  local rec legacy legacy_c

  home_c=$(fm_secondmate_parent_canonical "$home") ||
    migrate_refuse "secondmate $id home is unavailable: $home"
  [ "$home_c" = "$home" ] ||
    migrate_refuse "secondmate $id is registered at $home, not its canonical path $home_c"
  [ "$home_c" != "$PRIMARY" ] ||
    migrate_refuse "secondmate $id is registered at the primary home itself"
  marker_id=$(fm_parent_channel_home_id "$home_c") || rc=$?
  [ "$rc" -eq 0 ] && [ "$marker_id" = "$id" ] ||
    migrate_refuse "secondmate $id home $home_c does not carry an identity marker naming $id"

  meta="$PRIMARY_STATE/$id.meta"
  [ -f "$meta" ] && [ ! -L "$meta" ] ||
    migrate_refuse "parent metadata for secondmate $id is missing or unsafe: $meta"
  [ "$(fm_meta_get "$meta" kind)" = secondmate ] ||
    migrate_refuse "parent metadata for $id is not a secondmate record: $meta"
  meta_home=$(fm_meta_get "$meta" home)
  meta_home_c=$(fm_secondmate_parent_canonical "$meta_home") || meta_home_c=
  [ -n "$meta_home_c" ] && [ "$meta_home_c" = "$home_c" ] ||
    migrate_refuse "parent metadata for $id names ${meta_home:-no home}, not $home_c"

  secondmate_registry_validate_bindings "$REG" secondmate_registry_path_key "$id" "$home_c" ||
    migrate_refuse "${SECONDMATE_REGISTRY_ERROR:-registry does not bind $id to $home_c}"

  rec="$home_c/$SUB_HOME_PARENT_MARKER"
  mig_home_c[i]=$home_c
  mig_record[i]=$rec
  if [ ! -e "$rec" ] && [ ! -L "$rec" ]; then
    migrate_refuse "secondmate $id has no durable parent record at $rec; the migration does not create bindings"
  fi
  fm_secondmate_parent_record_parse "$rec" ||
    migrate_refuse "parent record for $id is malformed or unsafe: $rec"
  [ "$FM_SECONDMATE_PARENT_ROUTE" = local ] ||
    migrate_refuse "parent record for $id is not a local binding: $rec"

  if [ "$FM_SECONDMATE_PARENT_LAYOUT" = explicit ]; then
    if ! migrate_same_path "$FM_SECONDMATE_PARENT_PRIMARY" "$PRIMARY" ||
       ! migrate_same_path "$FM_SECONDMATE_PARENT_STATE" "$PRIMARY_STATE" ||
       ! migrate_same_path "$FM_SECONDMATE_PARENT_DATA" "$PRIMARY_DATA"; then
      migrate_refuse "unexpected prior binding for $id: its record names primary $FM_SECONDMATE_PARENT_PRIMARY, state $FM_SECONDMATE_PARENT_STATE, and data $FM_SECONDMATE_PARENT_DATA, not this primary's $PRIMARY, $PRIMARY_STATE, and $PRIMARY_DATA"
    fi
    if ! migrate_same_path "$FM_SECONDMATE_PARENT_HOME" "$PRIMARY" &&
       ! migrate_same_path "$FM_SECONDMATE_PARENT_HOME" "$STATE_OWNER"; then
      migrate_refuse "unexpected prior binding for $id: its parent_home $FM_SECONDMATE_PARENT_HOME is neither the primary home nor its state-owning directory"
    fi
    mig_action[i]=keep
    return 0
  fi

  legacy=$FM_SECONDMATE_PARENT_HOME
  if migrate_same_path "$legacy" "$PRIMARY"; then
    legacy_c=$PRIMARY
  elif migrate_same_path "$legacy" "$STATE_OWNER"; then
    legacy_c=$STATE_OWNER
  else
    migrate_refuse "unexpected prior binding for $id: parent_home $legacy is neither the primary home $PRIMARY nor its state-owning directory ${STATE_OWNER:-(none)}"
  fi
  mig_parent_home[i]=${STATE_OWNER:-$PRIMARY}
  mig_consent[i]=$legacy_c
  mig_action[i]=migrate
}

# Stage every candidate beside its original, backing up each original first.
migrate_stage() {
  local i=0
  MIGRATE_TMP=$(mktemp -d "${TMPDIR:-/tmp}/fm-secondmate-parent-migrate.XXXXXX") ||
    migrate_refuse "could not create the migration staging directory"
  while [ "$i" -lt "$mig_count" ]; do
    if [ "${mig_action[$i]}" = migrate ]; then
      mig_cand[i]="${mig_record[$i]}.migrate.$$"
      fm_secondmate_parent_record_write_local "${mig_cand[$i]}" \
        "${mig_parent_home[$i]}" "$PRIMARY" "$PRIMARY_STATE" "$PRIMARY_DATA" "${mig_consent[$i]}" ||
        migrate_refuse "could not stage the parent record for ${mig_ids[$i]}"
      cp -p -- "${mig_record[$i]}" "$MIGRATE_TMP/$i" ||
        migrate_refuse "could not back up the parent record for ${mig_ids[$i]}"
    fi
    i=$((i + 1))
  done
}

# Install staged records one at a time; each rename is atomic within its home.
migrate_install() {
  local i=0
  while [ "$i" -lt "$mig_count" ]; do
    if [ "${mig_action[$i]}" = migrate ]; then
      mv -f -- "${mig_cand[$i]}" "${mig_record[$i]}" ||
        migrate_refuse "could not install the parent record for ${mig_ids[$i]}"
      mig_installed[MIGRATE_INSTALLED_COUNT]=$i
      MIGRATE_INSTALLED_COUNT=$((MIGRATE_INSTALLED_COUNT + 1))
    fi
    i=$((i + 1))
  done
}

# Every registered local secondmate must now resolve to this primary's layout.
migrate_verify() {
  local i=0
  while [ "$i" -lt "$mig_count" ]; do
    if ! fm_secondmate_parent_resolve "${mig_home_c[$i]}" "${mig_ids[$i]}" "$FM_HOME" ||
       [ "$FM_SECONDMATE_PARENT_RESOLVED_HOME" != "$PRIMARY" ] ||
       [ "$FM_SECONDMATE_PARENT_RESOLVED_STATE" != "$PRIMARY_STATE" ] ||
       [ "$FM_SECONDMATE_PARENT_RESOLVED_DATA" != "$PRIMARY_DATA" ]; then
      printf 'verification failed for %s: %s\n' "${mig_ids[$i]}" \
        "${FM_SECONDMATE_PARENT_RESOLVE_ERROR:-resolved to another layout}" >&2
      return 1
    fi
    i=$((i + 1))
  done
}

# Main flow.
rc=0
fm_parent_channel_home_id "$FM_HOME" >/dev/null 2>&1 || rc=$?
[ "$rc" -eq 1 ] ||
  migrate_refuse "$FM_HOME is a secondmate home or has an unusable identity marker; run the migration from the primary"

PRIMARY=$(fm_secondmate_parent_canonical "$FM_HOME") ||
  migrate_refuse "primary home is unavailable or not absolute: $FM_HOME"
PRIMARY_STATE=$(fm_secondmate_parent_canonical "$STATE") ||
  migrate_refuse "primary state directory is unavailable: $STATE"
PRIMARY_DATA=$(fm_secondmate_parent_canonical "$DATA") ||
  migrate_refuse "primary data directory is unavailable: $DATA"
case "$PRIMARY_STATE" in
  */state)
    owner_candidate=${PRIMARY_STATE%/state}
    if [ -n "$owner_candidate" ] &&
       owner_data=$(fm_secondmate_parent_canonical "$owner_candidate/data") &&
       [ "$owner_data" = "$PRIMARY_DATA" ]; then
      STATE_OWNER=$owner_candidate
    fi
    ;;
esac

MIGRATE_LOCK=$(secondmate_registry_lock_path "$PRIMARY_STATE")
fm_lock_try_acquire "$MIGRATE_LOCK" ||
  migrate_refuse "another secondmate registry change holds $MIGRATE_LOCK; retry after it finishes"
MIGRATE_LOCK_HELD=1

if [ ! -e "$REG" ] && [ ! -L "$REG" ]; then
  echo "no secondmate registry at $REG; no local secondmate to migrate."
  exit 0
fi
secondmate_registry_validate_bindings "$REG" secondmate_registry_path_key ||
  migrate_refuse "${SECONDMATE_REGISTRY_ERROR:-secondmate registry is invalid}"

line=
while IFS= read -r line || [ -n "$line" ]; do
  case "$line" in
    "- "*)
      secondmate_registry_parse_line "$line" ||
        migrate_refuse "malformed secondmate registry entry: $line"
      mig_ids[mig_count]=$SECONDMATE_REGISTRY_ID
      mig_homes[mig_count]=$SECONDMATE_REGISTRY_HOME
      mig_count=$((mig_count + 1))
      ;;
  esac
done < "$REG"

if [ "$mig_count" -eq 0 ]; then
  echo "no local secondmate registered; nothing to migrate."
  exit 0
fi

i=0
while [ "$i" -lt "$mig_count" ]; do
  migrate_plan_entry "$i" "${mig_ids[$i]}" "${mig_homes[$i]}"
  i=$((i + 1))
done

pending=0
i=0
while [ "$i" -lt "$mig_count" ]; do
  [ "${mig_action[$i]}" != migrate ] || pending=$((pending + 1))
  i=$((i + 1))
done

if [ "$pending" -gt 0 ]; then
  migrate_stage
  migrate_install
fi
if ! migrate_verify; then
  if migrate_restore_installed; then
    migrate_refuse "verification through the shared resolver failed; every parent record was restored byte for byte"
  fi
  migrate_refuse "verification failed and restoring a parent record also failed; its backup is kept in $MIGRATE_TMP"
fi
MIGRATE_COMMITTED=1

i=0
while [ "$i" -lt "$mig_count" ]; do
  if [ "${mig_action[$i]}" = migrate ]; then
    printf 'migrated secondmate %s: parent record is explicit (primary %s)\n' "${mig_ids[$i]}" "$PRIMARY"
  else
    printf 'current secondmate %s: parent record already explicit\n' "${mig_ids[$i]}"
  fi
  i=$((i + 1))
done
