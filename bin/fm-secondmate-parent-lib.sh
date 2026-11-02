#!/usr/bin/env bash
# shellcheck disable=SC2034 # parsed fields output globals sourcing callers.
# Parse, write, and resolve the durable parent binding of a seeded secondmate home.
#
# fm-secondmate-parent.v1 record contains exactly one schema and one route.
# A local route contains exactly one absolute parent_home and no parent_host.
# parent_home keeps its v1 meaning: the Firstmate root home whose state/ and
# data/ fm_firstmate_root_home walks (bin/fm-wake-lib.sh).
# A local record may also carry the primary's explicit effective layout:
# parent_primary (the FM_HOME the primary launches secondmates with),
# parent_state, parent_data, and parent_consent (the home the parent recorded as
# its consent home; a field kept so existing records keep parsing). All four keys are present together or not at all; a record
# without them resolves to parent_home, parent_home/state, parent_home/data,
# and parent_home as its layout, which is the unchanged v1 meaning.
# A remote route is refused: remote secondmate placement was removed, so a record
# with route=remote, or any parent_host key, fails closed.
# Unknown fields are reserved forward-compatible additions.
# Duplicate fields, partial layout, a malformed local binding, a NUL-bearing
# record, and a symlinked record fail closed.
# Writers publish the record before the .fm-secondmate-home identity marker,
# which remains the seed-completion point.
#
# Requires bin/fm-backend.sh (fm_meta_get) and bin/fm-secondmate-registry-lib.sh
# (secondmate_registry_validate_bindings) to be sourced before the resolver runs.

fm_secondmate_parent_record_parse() {
  local file=$1 line schema='' route='' parent_home=''
  local parent_primary='' parent_state='' parent_data='' parent_consent=''
  local schema_count=0 route_count=0 parent_home_count=0 parent_host_count=0
  local primary_count=0 state_count=0 data_count=0 consent_count=0 value

  FM_SECONDMATE_PARENT_ROUTE=
  FM_SECONDMATE_PARENT_HOME=
  FM_SECONDMATE_PARENT_PRIMARY=
  FM_SECONDMATE_PARENT_STATE=
  FM_SECONDMATE_PARENT_DATA=
  FM_SECONDMATE_PARENT_CONSENT=
  FM_SECONDMATE_PARENT_LAYOUT=

  [ -f "$file" ] && [ ! -L "$file" ] || return 1
  # bash drops NUL bytes and bash generations disagree about what remains, so a
  # NUL-bearing record is rejected whole instead of letting the interpreter pick
  # which bytes name the home.
  [ "$(wc -c < "$file")" -eq "$(LC_ALL=C tr -d '\0' < "$file" | wc -c)" ] || return 1
  while IFS= read -r line || [ -n "$line" ]; do
    case "$line" in
      schema=*)
        schema_count=$((schema_count + 1))
        schema=${line#schema=}
        ;;
      route=*)
        route_count=$((route_count + 1))
        route=${line#route=}
        ;;
      parent_home=*)
        parent_home_count=$((parent_home_count + 1))
        parent_home=${line#parent_home=}
        ;;
      parent_host=*)
        parent_host_count=$((parent_host_count + 1))
        ;;
      parent_primary=*)
        primary_count=$((primary_count + 1))
        parent_primary=${line#parent_primary=}
        ;;
      parent_state=*)
        state_count=$((state_count + 1))
        parent_state=${line#parent_state=}
        ;;
      parent_data=*)
        data_count=$((data_count + 1))
        parent_data=${line#parent_data=}
        ;;
      parent_consent=*)
        consent_count=$((consent_count + 1))
        parent_consent=${line#parent_consent=}
        ;;
    esac
  done < "$file"

  [ "$schema_count" -eq 1 ] || return 1
  [ "$route_count" -eq 1 ] || return 1
  [ "$schema" = fm-secondmate-parent.v1 ] || return 1

  case "$route" in
    local)
      [ "$parent_home_count" -eq 1 ] || return 1
      [ "$parent_host_count" -eq 0 ] || return 1
      case "$parent_home" in /*) ;; *) return 1 ;; esac
      if [ $((primary_count + state_count + data_count + consent_count)) -eq 0 ]; then
        FM_SECONDMATE_PARENT_LAYOUT=v1
        FM_SECONDMATE_PARENT_PRIMARY=$parent_home
        FM_SECONDMATE_PARENT_STATE=$parent_home/state
        FM_SECONDMATE_PARENT_DATA=$parent_home/data
        FM_SECONDMATE_PARENT_CONSENT=$parent_home
      else
        [ "$primary_count" -eq 1 ] || return 1
        [ "$state_count" -eq 1 ] || return 1
        [ "$data_count" -eq 1 ] || return 1
        [ "$consent_count" -eq 1 ] || return 1
        for value in "$parent_primary" "$parent_state" "$parent_data" "$parent_consent"; do
          case "$value" in /*) ;; *) return 1 ;; esac
        done
        FM_SECONDMATE_PARENT_LAYOUT=explicit
        FM_SECONDMATE_PARENT_PRIMARY=$parent_primary
        FM_SECONDMATE_PARENT_STATE=$parent_state
        FM_SECONDMATE_PARENT_DATA=$parent_data
        FM_SECONDMATE_PARENT_CONSENT=$parent_consent
      fi
      FM_SECONDMATE_PARENT_HOME=$parent_home
      ;;
    *) return 1 ;;
  esac

  FM_SECONDMATE_PARENT_ROUTE=$route
}

# Publish a local record with an explicit layout, atomically, then re-parse it
# so a writer can never publish bytes the parser would refuse.
# Usage: fm_secondmate_parent_record_write_local <record-file> <parent_home>
#        <parent_primary> <parent_state> <parent_data> <parent_consent>
fm_secondmate_parent_record_write_local() {
  local file=$1 tmp="$1.tmp.$$"
  {
    printf 'schema=fm-secondmate-parent.v1\n'
    printf 'route=local\n'
    printf 'parent_home=%s\n' "$2"
    printf 'parent_primary=%s\n' "$3"
    printf 'parent_state=%s\n' "$4"
    printf 'parent_data=%s\n' "$5"
    printf 'parent_consent=%s\n' "$6"
  } > "$tmp" || { rm -f -- "$tmp"; return 1; }
  if ! fm_secondmate_parent_record_parse "$tmp" || [ "$FM_SECONDMATE_PARENT_LAYOUT" != explicit ]; then
    rm -f -- "$tmp"
    return 1
  fi
  mv -f -- "$tmp" "$file" || { rm -f -- "$tmp"; return 1; }
}

# Canonical absolute path of an existing directory, or non-zero.
fm_secondmate_parent_canonical() {  # <path>
  case "$1" in /*) ;; *) return 1 ;; esac
  CDPATH='' cd -- "$1" 2>/dev/null && pwd -P
}

# Resolve the durable parent binding of secondmate home <child> with id <id> to
# the primary's effective layout, proving it against the primary's own metadata
# and registry. <live> is the primary home the caller names (spawn passes its
# FM_HOME), or empty when none is set. <meta-mode> is
# "require" (default) or "fresh": the latter accepts an absent state/<id>.meta
# only for a fresh launch, whose own publish writes it with the exact child
# home; a metadata file that exists is always proven. <registry-mode> is
# "strict" (default) or "absent-ok": the latter skips the registry proof only
# when the primary has no secondmates.md at all, which is the spawn intake's
# unchanged tolerance; a registry that exists is always proven. Any other value
# is strict.
#
# On success sets FM_SECONDMATE_PARENT_RESOLVED_HOME, _STATE, _DATA, and _CONSENT
# and returns 0. Returns 2 when the home has no durable record and no live
# binding either, so the caller keeps its legacy no-binding behavior. Otherwise
# returns 1 and sets FM_SECONDMATE_PARENT_RESOLVE_ERROR to the first failed proof.
fm_secondmate_parent_resolve() {  # <child-home> <id> [live-primary-home] [require|fresh] [strict|absent-ok]
  local child=$1 id=$2 live=${3:-} meta_mode=${4:-require} registry_mode=${5:-strict} child_c primary_c live_c state_c data_c consent_c
  local record meta meta_home_c reg
  FM_SECONDMATE_PARENT_RESOLVED_HOME=
  FM_SECONDMATE_PARENT_RESOLVED_STATE=
  FM_SECONDMATE_PARENT_RESOLVED_DATA=
  FM_SECONDMATE_PARENT_RESOLVED_CONSENT=
  FM_SECONDMATE_PARENT_RESOLVE_ERROR=

  case "$id" in
    ''|*[!A-Za-z0-9._-]*)
      FM_SECONDMATE_PARENT_RESOLVE_ERROR="invalid secondmate id: $id"
      return 1
      ;;
  esac
  child_c=$(fm_secondmate_parent_canonical "$child") || {
    FM_SECONDMATE_PARENT_RESOLVE_ERROR="secondmate home is unavailable: $child"
    return 1
  }

  record="$child_c/.fm-secondmate-parent"
  if [ -e "$record" ] || [ -L "$record" ]; then
    fm_secondmate_parent_record_parse "$record" || {
      FM_SECONDMATE_PARENT_RESOLVE_ERROR="secondmate parent record is malformed or unsafe: $record"
      return 1
    }
    [ "$FM_SECONDMATE_PARENT_ROUTE" = local ] || {
      FM_SECONDMATE_PARENT_RESOLVE_ERROR="secondmate parent record has an unknown route: $record"
      return 1
    }
    primary_c=$(fm_secondmate_parent_canonical "$FM_SECONDMATE_PARENT_PRIMARY") || {
      FM_SECONDMATE_PARENT_RESOLVE_ERROR="recorded parent home is unavailable: $FM_SECONDMATE_PARENT_PRIMARY"
      return 1
    }
    state_c=$(fm_secondmate_parent_canonical "$FM_SECONDMATE_PARENT_STATE") || {
      FM_SECONDMATE_PARENT_RESOLVE_ERROR="recorded parent state is unavailable: $FM_SECONDMATE_PARENT_STATE"
      return 1
    }
    data_c=$(fm_secondmate_parent_canonical "$FM_SECONDMATE_PARENT_DATA") || {
      FM_SECONDMATE_PARENT_RESOLVE_ERROR="recorded parent data is unavailable: $FM_SECONDMATE_PARENT_DATA"
      return 1
    }
    consent_c=$(fm_secondmate_parent_canonical "$FM_SECONDMATE_PARENT_CONSENT") || {
      FM_SECONDMATE_PARENT_RESOLVE_ERROR="recorded parent consent home is unavailable: $FM_SECONDMATE_PARENT_CONSENT"
      return 1
    }
  elif [ -n "$live" ]; then
    # A home seeded before the durable record existed: its launch-time binding
    # is the only evidence, so the primary's layout is the v1 one.
    primary_c=$(fm_secondmate_parent_canonical "$live") || {
      FM_SECONDMATE_PARENT_RESOLVE_ERROR="launch binding is unavailable: $live"
      return 1
    }
    state_c=$(fm_secondmate_parent_canonical "$primary_c/state") || {
      FM_SECONDMATE_PARENT_RESOLVE_ERROR="parent state is unavailable: $primary_c/state"
      return 1
    }
    data_c=$(fm_secondmate_parent_canonical "$primary_c/data") || {
      FM_SECONDMATE_PARENT_RESOLVE_ERROR="parent data is unavailable: $primary_c/data"
      return 1
    }
    consent_c=$primary_c
  else
    return 2
  fi

  if [ -n "$live" ]; then
    live_c=$(fm_secondmate_parent_canonical "$live") || {
      FM_SECONDMATE_PARENT_RESOLVE_ERROR="launch binding is unavailable: $live"
      return 1
    }
    [ "$live_c" = "$primary_c" ] || {
      FM_SECONDMATE_PARENT_RESOLVE_ERROR="launch binding $live_c does not match the durable parent $primary_c"
      return 1
    }
  fi
  [ "$primary_c" != "$child_c" ] || {
    FM_SECONDMATE_PARENT_RESOLVE_ERROR="parent home is the secondmate home itself: $child_c"
    return 1
  }

  meta="$state_c/$id.meta"
  if [ "$meta_mode" != fresh ] || [ -e "$meta" ] || [ -L "$meta" ]; then
    [ -f "$meta" ] && [ ! -L "$meta" ] || {
      FM_SECONDMATE_PARENT_RESOLVE_ERROR="parent metadata for secondmate $id is missing or unsafe: $meta"
      return 1
    }
    [ "$(fm_meta_get "$meta" kind)" = secondmate ] || {
      FM_SECONDMATE_PARENT_RESOLVE_ERROR="parent metadata for $id is not a secondmate record: $meta"
      return 1
    }
    meta_home_c=$(fm_secondmate_parent_canonical "$(fm_meta_get "$meta" home)") || {
      FM_SECONDMATE_PARENT_RESOLVE_ERROR="parent metadata for $id names no usable home: $meta"
      return 1
    }
    [ "$meta_home_c" = "$child_c" ] || {
      FM_SECONDMATE_PARENT_RESOLVE_ERROR="parent metadata for $id names $meta_home_c, not $child_c"
      return 1
    }
  fi

  reg="$data_c/secondmates.md"
  # absent-ok skips the proof only when the registry file is absent; a present
  # registry, including a symlink, is always validated.
  if [ "$registry_mode" != absent-ok ] || [ -e "$reg" ] || [ -L "$reg" ]; then
    secondmate_registry_validate_bindings "$reg" secondmate_registry_path_key "$id" "$child_c" || {
      FM_SECONDMATE_PARENT_RESOLVE_ERROR="${SECONDMATE_REGISTRY_ERROR:-parent registry does not bind $id to $child_c}"
      return 1
    }
  fi

  FM_SECONDMATE_PARENT_RESOLVED_HOME=$primary_c
  FM_SECONDMATE_PARENT_RESOLVED_STATE=$state_c
  FM_SECONDMATE_PARENT_RESOLVED_DATA=$data_c
  FM_SECONDMATE_PARENT_RESOLVED_CONSENT=$consent_c
  return 0
}
