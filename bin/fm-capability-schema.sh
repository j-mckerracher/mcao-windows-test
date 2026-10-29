#!/usr/bin/env bash
# fm-capability-schema.sh - the single owner of the capability-source schema,
# approved as option A under approval record corr f425be7d1020d5f1: the front matter of a model-capabilities.md file, read by host-side panel
# resolution. The contract (fields, semantics, unresolved outcomes) lives in
# docs/panel-receipts.md; a field change is one edit in this file.
#
# Usage:
#   fm-capability-schema.sh validate <model-capabilities.md>
#   fm-capability-schema.sh lookup <model-capabilities.md> <model-id>
#   fm-capability-schema.sh --help
#
# validate exits 0 when the file holds a valid capability source, and 1 otherwise,
# naming the first problem on stderr.
# lookup exits 0 and prints one row when the model has a valid entry:
#   resolved<TAB>model_id<TAB>family<TAB>nested<TAB>available<TAB>identity_source<TAB>provider
# An absent provider prints as "-". lookup exits 1 and prints "unresolved: <reason>" when
# the file is missing or invalid, or the model has no entry. Exit 2 is a usage error.
#
# The file's front matter is a strict YAML subset: a top-level "models:" key followed by
# a list, with one "  - model_id: <value>" line per entry and "    <field>: <value>"
# lines after it. Any other shape is refused, so a malformed file never resolves partially.
set -u

usage() {
  printf '%s\n' \
    'usage: fm-capability-schema.sh validate <model-capabilities.md>' \
    '       fm-capability-schema.sh lookup <model-capabilities.md> <model-id>' \
    'contract: docs/panel-receipts.md'
}

FM_CAP_FIELDS="model_id family nested available identity_source provider"
FM_CAP_REQUIRED="model_id family nested available identity_source"

# Entry under construction, filled while the parser reads one list item.
e_seen=""
e_model_id=""
e_family=""
e_nested=""
e_available=""
e_identity=""
e_provider=""
ids=""
rows=""

fm_cap_reset_entry() {
  e_seen=""
  e_model_id=""
  e_family=""
  e_nested=""
  e_available=""
  e_identity=""
  e_provider=""
}

# fm_cap_flush: validate the entry under construction and append its row to rows.
# Returns 1 with the reason on stderr for any violation.
fm_cap_flush() {
  local field
  [ -n "$e_seen" ] || return 0
  for field in $FM_CAP_REQUIRED; do
    case " $e_seen " in
      *" $field "*) ;;
      *) echo "entry ${e_model_id:-?}: missing required field $field" >&2; return 1 ;;
    esac
  done
  case $e_model_id in
    -|*[[:space:]]*) echo "entry '$e_model_id': model_id must be one token and not '-'" >&2; return 1 ;;
  esac
  case " $ids " in
    *" $e_model_id "*) echo "entry $e_model_id: duplicate model_id" >&2; return 1 ;;
  esac
  case $e_nested in true|false) ;; *) echo "entry $e_model_id: nested must be true or false, got '$e_nested'" >&2; return 1 ;; esac
  case $e_available in true|false) ;; *) echo "entry $e_model_id: available must be true or false, got '$e_available'" >&2; return 1 ;; esac
  case $e_identity in catalog|observed|unknown) ;; *)
    echo "entry $e_model_id: identity_source must be catalog, observed, or unknown, got '$e_identity'" >&2; return 1 ;;
  esac
  if [ "$e_available" = true ] && [ "$e_nested" != true ]; then
    echo "entry $e_model_id: available true requires nested true" >&2
    return 1
  fi
  if [ "$e_identity" = unknown ] && [ "$e_family" != unknown ]; then
    echo "entry $e_model_id: family '$e_family' needs an identity_source of catalog or observed, not unknown" >&2
    return 1
  fi
  if [ "$e_family" = unknown ] && [ "$e_identity" != unknown ]; then
    echo "entry $e_model_id: family unknown must carry identity_source unknown" >&2
    return 1
  fi
  [ -n "$e_provider" ] || e_provider=-
  ids="$ids$e_model_id "
  rows="${rows}${e_model_id}"$'\t'"${e_family}"$'\t'"${e_nested}"$'\t'"${e_available}"$'\t'"${e_identity}"$'\t'"${e_provider}"$'\n'
  fm_cap_reset_entry
}

# fm_cap_parse <file>: read the front matter, filling rows or returning 1 with the reason.
fm_cap_parse() {
  local file=$1 line lineno=0 in_fm=0 closed=0 models_seen=0 entry_open=0 key val
  if [ ! -r "$file" ]; then
    echo "no capability source at $file" >&2
    return 1
  fi
  while IFS= read -r line || [ -n "$line" ]; do
    lineno=$((lineno + 1))
    if [ "$lineno" -eq 1 ]; then
      [ "$line" = "---" ] || { echo "$file: line 1 must open the front matter with ---" >&2; return 1; }
      in_fm=1
      continue
    fi
    if [ "$line" = "---" ]; then
      closed=1
      break
    fi
    case $line in
      '') continue ;;
      'models:') models_seen=1; continue ;;
      'models: []') models_seen=1; continue ;;
      '  - model_id: '*)
        fm_cap_flush || return 1
        val=${line#  - model_id: }
        [ -n "$val" ] || { echo "$file line $lineno: model_id has no value" >&2; return 1; }
        case $val in *$'\t'*) echo "$file line $lineno: tab in value of 'model_id'" >&2; return 1 ;; esac
        fm_cap_reset_entry
        e_model_id=$val
        e_seen=" model_id "
        entry_open=1
        continue
        ;;
      '    '*': '*)
        key=${line#    }
        key=${key%%: *}
        val=${line#    "$key": }
        ;;
      *)
        echo "$file line $lineno: unexpected line '$line'" >&2
        return 1
        ;;
    esac
    [ "$entry_open" -eq 1 ] || { echo "$file line $lineno: field '$key' appears before any model_id" >&2; return 1; }
    case " $FM_CAP_FIELDS " in
      *" $key "*) ;;
      *) echo "$file line $lineno: unknown field '$key'" >&2; return 1 ;;
    esac
    case " $e_seen " in
      *" $key "*) echo "$file line $lineno: duplicate field '$key' for entry $e_model_id" >&2; return 1 ;;
    esac
    [ -n "$val" ] && [ "$val" != - ] || { echo "$file line $lineno: field '$key' needs a value" >&2; return 1; }
    case $val in *$'\t'*) echo "$file line $lineno: tab in value of '$key'" >&2; return 1 ;; esac
    e_seen="$e_seen$key "
    case $key in
      model_id) ;;
      family) e_family=$val ;;
      nested) e_nested=$val ;;
      available) e_available=$val ;;
      identity_source) e_identity=$val ;;
      provider) e_provider=$val ;;
    esac
  done < "$file"
  [ "$in_fm" -eq 1 ] || { echo "$file: no front matter" >&2; return 1; }
  [ "$closed" -eq 1 ] || { echo "$file: front matter is not closed with ---" >&2; return 1; }
  [ "$models_seen" -eq 1 ] || { echo "$file: front matter has no models: key" >&2; return 1; }
  fm_cap_flush || return 1
  return 0
}

cmd_validate() {
  [ $# -eq 1 ] || { usage >&2; exit 2; }
  fm_cap_parse "$1" || exit 1
  exit 0
}

cmd_lookup() {
  [ $# -eq 2 ] || { usage >&2; exit 2; }
  [ -n "$2" ] || { usage >&2; exit 2; }
  local row id
  # Parse in this shell so rows survives; the parser's reason goes to stderr as is.
  if ! fm_cap_parse "$1"; then
    printf 'unresolved: %s is not a valid capability source\n' "$1"
    exit 1
  fi
  while IFS= read -r row; do
    [ -n "$row" ] || continue
    id=${row%%$'\t'*}
    if [ "$id" = "$2" ]; then
      printf 'resolved\t%s\n' "$row"
      exit 0
    fi
  done <<EOF
$rows
EOF
  printf 'unresolved: model %s has no entry in %s\n' "$2" "$1"
  exit 1
}

main() {
  case ${1:-} in
    validate) shift; cmd_validate "$@" ;;
    lookup) shift; cmd_lookup "$@" ;;
    -h|--help|help) usage ;;
    *) usage >&2; exit 2 ;;
  esac
}

main "$@"
