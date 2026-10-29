#!/usr/bin/env bash
# fm-panel-report.sh - the single owner of host-side HVE panel policy and receipts.
# The contract (seat fields, verdicts, Q2 report rules) lives in docs/panel-receipts.md.
#
# Usage:
#   fm-panel-report.sh policy <delivery-mode> <panel-kind> <granted-authority>
#   fm-panel-report.sh report [--min <role>=<n>]... <seats.tsv>
#   fm-panel-report.sh --help
#
# policy prints the launch verdict and exits 0 when the granted authority names the
# panel kind it proposes. It exits 2 on a policy conflict, and no panel launches then.
# It exits 1 on invalid input. The delivery mode must pass the managed-context contract
# (bin/fm-managed-context-lib.sh).
#
# report reads one seat per line from seats.tsv (twelve tab-separated fields, documented
# in docs/panel-receipts.md). It prints the panel status, receipts, coverage gaps,
# diversity claim, and completed outputs. It exits 1 on invalid seat records and 0
# otherwise, because an incomplete panel is a reported status, not a script failure.
#
# Panel resolution (which seat gets which model) is in bin/fm-panel-resolve.sh. This script
# reads the seat records that resolution and execution produce.
set -u

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
# shellcheck source=bin/fm-managed-context-lib.sh
. "$SCRIPT_DIR/fm-managed-context-lib.sh"

usage() {
  printf '%s\n' \
    'usage: fm-panel-report.sh policy <delivery-mode> <panel-kind> <granted-authority>' \
    '       fm-panel-report.sh report [--min <role>=<n>]... <seats.tsv>' \
    'contract: docs/panel-receipts.md'
}

die() {
  printf 'error: fm-panel-report: %s\n' "$*" >&2
  exit 1
}

cmd_policy() {
  [ $# -eq 3 ] || die "policy takes <delivery-mode> <panel-kind> <granted-authority>"
  local mode=$1 kind=$2 granted=$3
  fm_managed_context_mode_valid "$mode" "fm-panel-report policy" || exit 1
  case $kind in
    design-assist|delivery-review) ;;
    *) die "panel kind must be design-assist or delivery-review, got '$kind'" ;;
  esac
  case $granted in
    none|design-assist|delivery-review) ;;
    *) die "granted authority must be none, design-assist, or delivery-review, got '$granted'" ;;
  esac
  if [ "$granted" = "$kind" ]; then
    printf 'launch: %s panel under %s delivery mode, authority granted\n' "$kind" "$mode"
    return 0
  fi
  printf 'policy-conflict: %s panel proposed under %s delivery mode without matching authority (granted: %s); no panel launched; worker reports the conflict\n' \
    "$kind" "$mode" "$granted"
  return 2
}

# Seat table, filled by load_seats. Parallel arrays indexed by seat position.
S_ID=() S_ROLE=() S_KIND=() S_JUDGES=() S_REQ=() S_RES=() S_OUT=()
S_OMODEL=() S_OFAM=() S_EVID=() S_RSN=() S_OUTPUT=()

# load_seats <seats-file>: parse and validate every seat record, or exit 1 naming the line.
load_seats() {
  local file=$1 line lineno=0 i j
  local -a f
  while IFS= read -r line || [ -n "$line" ]; do
    lineno=$((lineno + 1))
    case $line in ''|'#'*) continue ;; esac
    IFS=$'\t' read -r -a f <<<"$line"
    [ "${#f[@]}" -eq 12 ] || die "$file line $lineno: expected 12 tab-separated fields, got ${#f[@]}"
    local id=${f[0]} role=${f[1]} kind=${f[2]} judges=${f[3]} req=${f[4]} res=${f[5]}
    local out=${f[6]} omodel=${f[7]} ofam=${f[8]} evid=${f[9]} rsn=${f[10]} output=${f[11]}

    for ((i = 0; i < ${#S_ID[@]}; i++)); do
      [ "${S_ID[i]}" = "$id" ] && die "$file line $lineno: duplicate seat_id '$id'"
    done
    case $kind in
      candidate)
        [ "$judges" = "-" ] || die "$file line $lineno: candidate seat '$id' must name no judged seat (judges must be -)" ;;
      judge)
        { [ "$judges" != "-" ] && [ "$judges" != "$id" ]; } \
          || die "$file line $lineno: judge seat '$id' must name a different seat in judges" ;;
      *) die "$file line $lineno: seat_kind must be candidate or judge, got '$kind'" ;;
    esac
    case $out in
      completed|failed|missing) ;;
      *) die "$file line $lineno: outcome must be completed, failed, or missing, got '$out'" ;;
    esac
    if [ "$res" = unresolved ]; then
      [ "$out" != completed ] || die "$file line $lineno: seat '$id' is completed but its resolved model is unresolved"
      [ "$rsn" != "-" ] || die "$file line $lineno: seat '$id' is unresolved and needs its reason in the reason column"
    elif [ "$req" != "$res" ]; then
      [ "$rsn" != "-" ] || die "$file line $lineno: seat '$id' resolved to '$res' instead of requested '$req' with no reason; silent fallback refused"
    fi
    if [ "$out" = failed ]; then
      [ "$rsn" != "-" ] || die "$file line $lineno: seat '$id' failed and needs its reason in the reason column"
    fi
    if [ "$out" = completed ] && [ "$req" = "$res" ] && [ "$rsn" != "-" ]; then
      die "$file line $lineno: seat '$id' completed on its requested model and gives a reason; a reason needs a fallback, an unresolved model, or a failed or missing outcome"
    fi
    if [ "$omodel" = unknown ]; then
      { [ "$ofam" = unknown ] && [ "$evid" = "-" ]; } \
        || die "$file line $lineno: seat '$id' has unknown observed model, so family must be unknown and evidence -"
    elif [ "$ofam" != unknown ] && [ "$evid" = "-" ]; then
      die "$file line $lineno: seat '$id' observed family '$ofam' needs supporting evidence"
    fi

    S_ID+=("$id") S_ROLE+=("$role") S_KIND+=("$kind") S_JUDGES+=("$judges")
    S_REQ+=("$req") S_RES+=("$res") S_OUT+=("$out") S_OMODEL+=("$omodel")
    S_OFAM+=("$ofam") S_EVID+=("$evid") S_RSN+=("$rsn") S_OUTPUT+=("$output")
  done < "$file"

  [ "${#S_ID[@]}" -gt 0 ] || die "$file has no seat records"
  for ((i = 0; i < ${#S_ID[@]}; i++)); do
    [ "${S_KIND[i]}" = judge ] || continue
    local found=0
    for ((j = 0; j < ${#S_ID[@]}; j++)); do
      [ "${S_ID[j]}" = "${S_JUDGES[i]}" ] && found=1
    done
    [ "$found" = 1 ] || die "$file: judge seat '${S_ID[i]}' names unknown judged seat '${S_JUDGES[i]}'"
  done
}

# index_of <seat-id>: echo the seat's index, or -1 when absent.
index_of() {
  local i
  for ((i = 0; i < ${#S_ID[@]}; i++)); do
    if [ "${S_ID[i]}" = "$1" ]; then echo "$i"; return 0; fi
  done
  echo -1
}

# identity_of <index>: echo verified only when observed model and family are both known.
identity_of() {
  if [ "${S_OMODEL[$1]}" != unknown ] && [ "${S_OFAM[$1]}" != unknown ]; then
    echo verified
  else
    echo unknown
  fi
}

# role_completed <role>: echo how many completed seats the role has.
role_completed() {
  local i c=0
  for ((i = 0; i < ${#S_ID[@]}; i++)); do
    if [ "${S_ROLE[i]}" = "$1" ] && [ "${S_OUT[i]}" = completed ]; then c=$((c + 1)); fi
  done
  echo "$c"
}

# diversity_claim: one line stating the only diversity claim the completed seats support.
diversity_claim() {
  local i k seen
  local -a comp=() fams=() models=()
  for ((i = 0; i < ${#S_ID[@]}; i++)); do
    [ "${S_OUT[i]}" = completed ] && comp+=("$i")
  done
  if [ "${#comp[@]}" -eq 0 ]; then
    echo "diversity: none, no completed seats"
    return 0
  fi
  local unknown_ids=""
  for i in "${comp[@]}"; do
    [ "$(identity_of "$i")" = verified ] || unknown_ids+="${S_ID[i]} "
  done
  if [ -n "$unknown_ids" ]; then
    echo "diversity: unknown, identity missing for seat(s) ${unknown_ids% }; no diversity claim"
    return 0
  fi
  for i in "${comp[@]}"; do
    seen=0
    for ((k = 0; k < ${#fams[@]}; k++)); do [ "${fams[k]}" = "${S_OFAM[i]}" ] && seen=1; done
    [ "$seen" = 1 ] || fams+=("${S_OFAM[i]}")
    seen=0
    for ((k = 0; k < ${#models[@]}; k++)); do [ "${models[k]}" = "${S_OMODEL[i]}" ] && seen=1; done
    [ "$seen" = 1 ] || models+=("${S_OMODEL[i]}")
  done
  if [ "${#comp[@]}" -eq 1 ]; then
    echo "diversity: none, one completed seat"
  elif [ "${#fams[@]}" -ge 2 ]; then
    local executed=""
    for i in "${comp[@]}"; do
      executed+="${S_ID[i]}=${S_OMODEL[i]}/${S_OFAM[i]} (evidence ${S_EVID[i]}) "
    done
    echo "diversity: cross-family across ${#fams[@]} families; executed models: ${executed% }"
  elif [ "${#models[@]}" -eq 1 ]; then
    echo "diversity: independent contexts on ${models[0]} across ${#comp[@]} completed seats; no cross-family claim"
  else
    echo "diversity: reduced, single family ${fams[0]} across ${#comp[@]} completed seats on ${#models[@]} models; reduced diversity, not a failure"
  fi
}

cmd_report() {
  local -a min_role=() min_need=()
  local r n
  while [ $# -gt 0 ]; do
    case $1 in
      --min)
        [ $# -ge 2 ] || die "--min needs <role>=<n>"
        case $2 in *=*) ;; *) die "--min needs <role>=<n>, got '$2'" ;; esac
        r=${2%%=*} n=${2#*=}
        [ -n "$r" ] || die "--min needs a role name, got '$2'"
        case $n in ''|*[!0-9]*) die "--min count must be a non-negative integer, got '$n'" ;; esac
        min_role+=("$r") min_need+=("$n")
        shift 2 ;;
      --) shift; break ;;
      -*) die "unknown option '$1'" ;;
      *) break ;;
    esac
  done
  [ $# -eq 1 ] || die "report takes one seats file"
  [ -r "$1" ] || die "cannot read seats file '$1'"
  load_seats "$1"

  local i k have ncand=0 njudge=0 ncomp=0 nmiss=0
  local -a gaps=() minlines=()
  for ((i = 0; i < ${#S_ID[@]}; i++)); do
    case ${S_KIND[i]} in candidate) ncand=$((ncand + 1)) ;; judge) njudge=$((njudge + 1)) ;; esac
    if [ "${S_OUT[i]}" = completed ]; then
      ncomp=$((ncomp + 1))
    else
      nmiss=$((nmiss + 1))
      local gap="coverage-gap: seat ${S_ID[i]} (role ${S_ROLE[i]}) outcome ${S_OUT[i]}"
      [ "${S_RSN[i]}" = "-" ] || gap+="; reason: ${S_RSN[i]}"
      gaps+=("$gap")
    fi
  done
  for ((k = 0; k < ${#min_role[@]}; k++)); do
    have=$(role_completed "${min_role[k]}")
    minlines+=("minimum: role ${min_role[k]} completed $have of ${min_need[k]} required")
    if [ "$have" -lt "${min_need[k]}" ]; then
      gaps+=("coverage-gap: role ${min_role[k]} has $have of ${min_need[k]} required completed seats")
    fi
  done

  local status=complete
  [ "${#gaps[@]}" -eq 0 ] || status=incomplete
  printf 'panel-status: %s\n' "$status"
  printf 'seats: candidate=%s judge=%s completed=%s missing=%s\n' "$ncand" "$njudge" "$ncomp" "$nmiss"
  for ((k = 0; k < ${#minlines[@]}; k++)); do printf '%s\n' "${minlines[k]}"; done
  for ((k = 0; k < ${#gaps[@]}; k++)); do printf '%s\n' "${gaps[k]}"; done
  diversity_claim

  local t
  for ((i = 0; i < ${#S_ID[@]}; i++)); do
    [ "${S_KIND[i]}" = judge ] && [ "${S_OUT[i]}" = completed ] || continue
    t=$(index_of "${S_JUDGES[i]}")
    if [ "${S_OUT[t]}" = completed ] && [ "$(identity_of "$i")" = verified ] && [ "$(identity_of "$t")" = verified ]; then
      if [ "${S_OFAM[i]}" = "${S_OFAM[t]}" ]; then
        printf 'judge-separation: reduced diversity: judge %s and judged seat %s share family %s; a different family is preferred for judges\n' \
          "${S_ID[i]}" "${S_JUDGES[i]}" "${S_OFAM[i]}"
      else
        printf 'judge-separation: judge %s family %s differs from judged seat %s family %s\n' \
          "${S_ID[i]}" "${S_OFAM[i]}" "${S_JUDGES[i]}" "${S_OFAM[t]}"
      fi
    else
      printf 'judge-separation: judge %s judging %s has unknown identity; family separation not proven\n' \
        "${S_ID[i]}" "${S_JUDGES[i]}"
    fi
  done

  for ((i = 0; i < ${#S_ID[@]}; i++)); do
    printf 'receipt: %s role=%s kind=%s judges=%s requested=%s resolved=%s outcome=%s observed-model=%s observed-family=%s identity=%s evidence=%s reason=%s output=%s\n' \
      "${S_ID[i]}" "${S_ROLE[i]}" "${S_KIND[i]}" "${S_JUDGES[i]}" "${S_REQ[i]}" "${S_RES[i]}" \
      "${S_OUT[i]}" "${S_OMODEL[i]}" "${S_OFAM[i]}" "$(identity_of "$i")" "${S_EVID[i]}" \
      "${S_RSN[i]}" "${S_OUTPUT[i]}"
  done

  for ((i = 0; i < ${#S_ID[@]}; i++)); do
    if [ "${S_OUT[i]}" = completed ] && [ "${S_OUTPUT[i]}" != "-" ]; then
      printf 'completed-output: %s %s\n' "${S_ID[i]}" "${S_OUTPUT[i]}"
    fi
  done
  return 0
}

main() {
  case ${1:-} in
    policy) shift; cmd_policy "$@" ;;
    report) shift; cmd_report "$@" ;;
    -h|--help|help) usage ;;
    *) usage >&2; exit 1 ;;
  esac
}

main "$@"
