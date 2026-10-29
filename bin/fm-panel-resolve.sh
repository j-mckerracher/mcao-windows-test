#!/usr/bin/env bash
# fm-panel-resolve.sh - the single owner of host-side HVE panel seat resolution: which
# requested model each seat can execute, or why it cannot. The contract (request fields,
# unresolved outcomes, the no-substitution rule) lives in docs/panel-receipts.md.
# The capability source is read through bin/fm-capability-schema.sh and is never parsed here.
#
# Usage:
#   fm-panel-resolve.sh resolve <model-capabilities.md> <panel-request.tsv>
#   fm-panel-resolve.sh --help
#
# panel-request.tsv holds one requested seat per line, with five tab-separated fields:
#   seat_id  role  seat_kind  judges  requested_model
# judges is - for a candidate and the judged seat's id for a judge.
#
# resolve prints one seat record per request, in the twelve-field seats format that
# bin/fm-panel-report.sh reads. A seat whose requested model has a valid entry with
# nested and available both true is assigned that model. Every other seat is marked
# unresolved with a reason. A seat is never assigned a different model, and no seat is
# dropped. Each record starts as missing, because execution fills its outcome and
# observed identity. An invalid or missing capability source resolves no seat and
# gives every seat the same reason. Exit 0 when the request is valid, whatever the
# resolutions, because unresolved seats are reported results. Exit 1 on an invalid
# request, and exit 2 on a usage error.
set -u

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
CAP="$SCRIPT_DIR/fm-capability-schema.sh"

usage() {
  printf '%s\n' \
    'usage: fm-panel-resolve.sh resolve <model-capabilities.md> <panel-request.tsv>' \
    'contract: docs/panel-receipts.md'
}

die() {
  printf 'error: fm-panel-resolve: %s\n' "$*" >&2
  exit 1
}

# Request table, filled by load_request. Parallel arrays indexed by request position.
R_ID=() R_ROLE=() R_KIND=() R_JUDGES=() R_REQ=()

# load_request <file>: parse and validate every request line, or exit 1 naming the line.
load_request() {
  local file=$1 line lineno=0 i
  local -a f
  while IFS= read -r line || [ -n "$line" ]; do
    lineno=$((lineno + 1))
    case $line in ''|'#'*) continue ;; esac
    IFS=$'\t' read -r -a f <<<"$line"
    [ "${#f[@]}" -eq 5 ] || die "$file line $lineno: expected 5 tab-separated fields, got ${#f[@]}"
    local id=${f[0]} role=${f[1]} kind=${f[2]} judges=${f[3]} req=${f[4]}

    case $id in ''|-) die "$file line $lineno: seat_id must be a name, got '$id'" ;; esac
    case $role in ''|-) die "$file line $lineno: seat '$id' needs a role name" ;; esac
    for ((i = 0; i < ${#R_ID[@]}; i++)); do
      [ "${R_ID[i]}" = "$id" ] && die "$file line $lineno: duplicate seat_id '$id'"
    done
    case $kind in
      candidate)
        [ "$judges" = "-" ] || die "$file line $lineno: candidate seat '$id' must name no judged seat (judges must be -)" ;;
      judge)
        [ "$judges" != "-" ] || die "$file line $lineno: judge seat '$id' must name the seat it judges" ;;
      *) die "$file line $lineno: seat_kind must be candidate or judge, got '$kind'" ;;
    esac
    case $req in
      ''|-|unresolved|*' '*) die "$file line $lineno: seat '$id' needs one requested model token, got '$req'" ;;
    esac

    R_ID+=("$id") R_ROLE+=("$role") R_KIND+=("$kind") R_JUDGES+=("$judges") R_REQ+=("$req")
  done < "$file"

  [ "${#R_ID[@]}" -gt 0 ] || die "$file has no seat requests"
  local j found
  for ((i = 0; i < ${#R_ID[@]}; i++)); do
    [ "${R_KIND[i]}" = judge ] || continue
    [ "${R_JUDGES[i]}" != "${R_ID[i]}" ] || die "$file: judge seat '${R_ID[i]}' must name a different seat in judges"
    found=0
    for ((j = 0; j < ${#R_ID[@]}; j++)); do
      [ "${R_ID[j]}" = "${R_JUDGES[i]}" ] && found=1
    done
    [ "$found" = 1 ] || die "$file: judge seat '${R_ID[i]}' names unknown judged seat '${R_JUDGES[i]}'"
  done
}

cmd_resolve() {
  [ $# -eq 2 ] || { usage >&2; exit 2; }
  [ -n "$1" ] && [ -n "$2" ] || { usage >&2; exit 2; }
  local cap=$1 request=$2 i model reason resolved row cap_err cap_ok cap_problem=""
  local _ nested avail
  [ -r "$request" ] || die "cannot read panel request '$request'"
  load_request "$request"

  if cap_err=$(bash "$CAP" validate "$cap" 2>&1 >/dev/null); then
    cap_ok=1
  else
    cap_ok=0
    cap_problem=${cap_err%%$'\n'*}
    cap_problem=$(printf '%s' "$cap_problem" | tr -s '[:cntrl:] ' ' ')
    [ -n "$cap_problem" ] || cap_problem="validation failed"
  fi

  for ((i = 0; i < ${#R_ID[@]}; i++)); do
    model=${R_REQ[i]}
    reason=-
    if [ "$cap_ok" = 0 ]; then
      reason="capability source invalid: $cap_problem"
    elif row=$(bash "$CAP" lookup "$cap" "$model" 2>/dev/null); then
      # A resolved row is: resolved, model_id, family, nested, available, identity_source, provider.
      IFS=$'\t' read -r _ _ _ nested avail _ _ <<<"$row"
      if [ "$nested" != true ] || [ "$avail" != true ]; then
        reason="nested capability unavailable for model $model: nested=$nested available=$avail"
      fi
    else
      reason="no capability entry for model $model"
    fi

    if [ "$reason" = - ]; then resolved=$model; else resolved=unresolved; fi
    printf '%s\t%s\t%s\t%s\t%s\t%s\tmissing\tunknown\tunknown\t-\t%s\t-\n' \
      "${R_ID[i]}" "${R_ROLE[i]}" "${R_KIND[i]}" "${R_JUDGES[i]}" "$model" "$resolved" "$reason"
  done
  return 0
}

main() {
  case ${1:-} in
    resolve) shift; cmd_resolve "$@" ;;
    -h|--help|help) usage ;;
    *) usage >&2; exit 2 ;;
  esac
}

main "$@"
