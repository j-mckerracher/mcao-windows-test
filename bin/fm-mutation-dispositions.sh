#!/usr/bin/env bash
# bin/fm-mutation-dispositions.sh - the consumer of mutation disposition records. It reads
# the runner's published evidence record and the disposition records committed on a git
# ref, then prints the raw counts and the adjusted counts side by side. Only a valid,
# signed equivalent-mutant claim removes a survived mutant from the survivor count. A valid,
# signed unsupported-surface claim is shown beside the excluded count and never changes it.
# Pending, lapsed, unsigned, malformed, unmatched, and duplicate claims adjust nothing, and a
# timeout or an error is never reclassified. Report-only: bin/fm-mutation-verdict.sh and the
# PR check do not read this output.
#
# docs/mutation-dispositions.md owns the record format, the status rules, and the trust
# basis. This file owns the command line and the checks that enforce that contract.
#
# Usage: fm-mutation-dispositions.sh [--root <dir>] [--ref <git-ref>] [--records <dir>] <runner-record>
#   --root     the git work tree whose source files a claim's digest is checked against (default: .)
#   --ref      the git ref the records are read from (default: origin/main). Records are read
#              from the ref's objects, so an uncommitted or unmerged record adjusts nothing.
#   --records  the records directory inside the ref (default: docs/mutation-dispositions)
#
# Output (stdout):
#   records   ref=<ref> dir=<dir> files=<n>
#   raw       killed=N survived=N timeout=N error=N unresolved=N excluded=N
#   adjusted  killed=N survived=N timeout=N error=N unresolved=N excluded=N equivalent-removed=N unsupported-signed=N
#   claims    applied=N pending=N lapsed=N unsigned=N malformed=N unmatched=N not-survivor=N duplicate=N
#   record <file>: <outcome>: <reason>      one line per record file, in name order
#
# Exit codes: 0 printed; 1 the evidence record, the ref, or a record could not be read;
# 2 usage. A malformed or non-adjusting claim is reported on stdout and exits 0.

set -u

SCRIPT_DIR=$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)
# shellcheck source=bin/fm-mutation-surface-lib.sh
. "$SCRIPT_DIR/fm-mutation-surface-lib.sh"

TAB=$(printf '\t')
FORMAT_ID=fm-mutation-disposition/1
MUTANT_ID_RE='^L[0-9][0-9]*-.+$'
DATE_RE='^[0-9][0-9][0-9][0-9]-(0[1-9]|1[0-2])-(0[1-9]|[12][0-9]|3[01])$'
OWNER=$(fm_mutation_equivalence_signoff_owner)
ROOT=.
REF=origin/main
RECDIR=docs/mutation-dispositions

usage() {
  printf '%s\n' "usage: fm-mutation-dispositions.sh [--root <dir>] [--ref <git-ref>] [--records <dir>] <runner-record>" >&2
  exit 2
}

refuse() {
  printf 'fm-mutation-dispositions.sh: %s\n' "$*" >&2
  exit 2
}

die() {
  printf 'fm-mutation-dispositions.sh: %s\n' "$*" >&2
  exit 1
}

while [ "$#" -gt 0 ]; do
  case "$1" in
    --root)
      [ "$#" -ge 2 ] || usage
      ROOT=$2
      shift 2
      ;;
    --ref)
      [ "$#" -ge 2 ] || usage
      REF=$2
      shift 2
      ;;
    --records)
      [ "$#" -ge 2 ] || usage
      RECDIR=$2
      shift 2
      ;;
    -h|--help)
      printf '%s\n' "usage: fm-mutation-dispositions.sh [--root <dir>] [--ref <git-ref>] [--records <dir>] <runner-record>"
      exit 0
      ;;
    --)
      shift
      break
      ;;
    -*)
      usage
      ;;
    *)
      break
      ;;
  esac
done
[ "$#" -eq 1 ] || usage
EVIDENCE=$1
case "$RECDIR" in
  ''|/*|*..*) refuse "--records must be a relative directory inside the ref: $RECDIR" ;;
esac

[ -f "$EVIDENCE" ] || die "no evidence record: $EVIDENCE"
TOP=$(git -C "$ROOT" rev-parse --show-toplevel 2>/dev/null) || die "not inside a git work tree: $ROOT"
git -C "$TOP" rev-parse --verify --quiet "$REF^{commit}" >/dev/null 2>&1 ||
  die "ref not readable: $REF; no records were read, so nothing is adjusted"

SCRATCH=$(mktemp -d "${TMPDIR:-/tmp}/fm-mutation-dispositions.XXXXXX") || die "could not create a scratch directory"
trap 'rm -rf -- "$SCRATCH"' EXIT
RESULTS="$SCRATCH/results"
: >"$RESULTS" || die "could not create the result file"

# sha256_file <file> / sha256_stdin: the digest the record format uses, lowercase hex.
sha256_file() {
  if command -v shasum >/dev/null 2>&1; then
    shasum -a 256 "$1" | awk '{ print $1 }'
  else
    sha256sum "$1" | awk '{ print $1 }'
  fi
}

sha256_stdin() {
  if command -v shasum >/dev/null 2>&1; then
    shasum -a 256 | awk '{ print $1 }'
  else
    sha256sum | awk '{ print $1 }'
  fi
}

# is_sha256 <value>: exactly 64 lowercase hex digits.
is_sha256() {
  case "$1" in
    ''|*[!0-9a-f]*) return 1 ;;
  esac
  [ "${#1}" -eq 64 ]
}

# is_date <value>: a YYYY-MM-DD date with a plausible month and day.
is_date() {
  [[ $1 =~ $DATE_RE ]]
}

# emit <outcome> <kind> <path> <mutant> <name> <reason>: one result line. An empty field
# becomes "-", because tab is whitespace to read and would merge empty fields.
emit() {
  printf '%s\t%s\t%s\t%s\t%s\t%s\n' "$1" "${2:--}" "${3:--}" "${4:--}" "$5" "$6" >>"$RESULTS"
}

# Digest of the runner's tool rows: the configuration the claim is bound to. A record with
# no tool row has no configuration to bind to, so it is refused rather than digested as empty.
[ -n "$(awk -F "$TAB" '$1 == "tool"' "$EVIDENCE")" ] ||
  die "evidence record has no tool row, so there is no configuration to bind a claim to: $EVIDENCE"
CONFIG_SHA=$(awk -F "$TAB" '$1 == "tool"' "$EVIDENCE" | sha256_stdin) || die "could not digest the evidence record's tool rows"

# The runner's published summary is the raw count source.
SUMMARY=$(awk -F "$TAB" '
  $1 == "summary" && $5 == "published" { n++; line = $6 }
  END { if (n != 1) exit 1; print line }' "$EVIDENCE") ||
  die "evidence record must hold exactly one published summary: $EVIDENCE"
RAW_KILLED='' RAW_SURVIVED='' RAW_TIMEOUT='' RAW_ERROR='' RAW_UNRESOLVED='' RAW_EXCLUDED=''
for kv in $SUMMARY; do
  case "$kv" in
    killed=*) RAW_KILLED=${kv#killed=} ;;
    survived=*) RAW_SURVIVED=${kv#survived=} ;;
    timeout=*) RAW_TIMEOUT=${kv#timeout=} ;;
    error=*) RAW_ERROR=${kv#error=} ;;
    unresolved=*) RAW_UNRESOLVED=${kv#unresolved=} ;;
    excluded=*) RAW_EXCLUDED=${kv#excluded=} ;;
  esac
done
for v in "$RAW_KILLED" "$RAW_SURVIVED" "$RAW_TIMEOUT" "$RAW_ERROR" "$RAW_UNRESOLVED" "$RAW_EXCLUDED"; do
  case "$v" in
    ''|*[!0-9]*) die "published summary has a missing or non-numeric count: $SUMMARY" ;;
  esac
done

# Record fields. parse_record fills these from key=value lines; validate_record checks them.
PARSE_ERROR=''
SEEN=" "
R_format='' R_kind='' R_status='' R_source_path='' R_source_sha256='' R_tool_versions_sha256=''
R_rationale='' R_evidence_command='' R_evidence_result='' R_signer='' R_signed_on=''
R_mutant_id='' R_operator='' R_surface=''

has_key() {  # <key>: whether the record set this key
  case "$SEEN" in
    *" $1 "*) return 0 ;;
  esac
  return 1
}

parse_record() {  # <file>: fills R_* from key=value lines; returns 1 with PARSE_ERROR when malformed
  local line key value
  PARSE_ERROR=''
  SEEN=" "
  R_format='' R_kind='' R_status='' R_source_path='' R_source_sha256='' R_tool_versions_sha256=''
  R_rationale='' R_evidence_command='' R_evidence_result='' R_signer='' R_signed_on=''
  R_mutant_id='' R_operator='' R_surface=''
  while IFS= read -r line || [ -n "$line" ]; do
    case "$line" in
      *=*) ;;
      *) PARSE_ERROR="a line is not key=value"; return 1 ;;
    esac
    key=${line%%=*}
    value=${line#*=}
    case "$SEEN" in
      *" $key "*) PARSE_ERROR="repeated key: $key"; return 1 ;;
    esac
    SEEN="$SEEN$key "
    case "$key" in
      format) R_format=$value ;;
      kind) R_kind=$value ;;
      status) R_status=$value ;;
      source_path) R_source_path=$value ;;
      source_sha256) R_source_sha256=$value ;;
      tool_versions_sha256) R_tool_versions_sha256=$value ;;
      rationale) R_rationale=$value ;;
      evidence_command) R_evidence_command=$value ;;
      evidence_result) R_evidence_result=$value ;;
      signer) R_signer=$value ;;
      signed_on) R_signed_on=$value ;;
      mutant_id) R_mutant_id=$value ;;
      operator) R_operator=$value ;;
      surface) R_surface=$value ;;
      *) PARSE_ERROR="unknown key: $key"; return 1 ;;
    esac
  done <"$1"
  return 0
}

validate_record() {  # sets PARSE_ERROR and returns 1 when the parsed record is malformed
  local key req foreign
  [ "$R_format" = "$FORMAT_ID" ] || { PARSE_ERROR="format is not $FORMAT_ID"; return 1; }
  case "$R_kind" in
    equivalent-mutant) req="mutant_id operator"; foreign="surface" ;;
    unsupported-surface) req="surface"; foreign="mutant_id operator" ;;
    *) PARSE_ERROR="kind is not equivalent-mutant or unsupported-surface"; return 1 ;;
  esac
  case "$R_status" in
    pending|signed|lapsed) ;;
    *) PARSE_ERROR="status is not pending, signed, or lapsed"; return 1 ;;
  esac
  for key in format kind status source_path source_sha256 tool_versions_sha256 \
    rationale evidence_command evidence_result signer signed_on $req; do
    has_key "$key" || { PARSE_ERROR="missing key: $key"; return 1; }
  done
  for key in $foreign; do
    if has_key "$key"; then
      PARSE_ERROR="key $key does not apply to kind $R_kind"
      return 1
    fi
  done
  case "$R_source_path" in
    ''|/*|*..*) PARSE_ERROR="source_path is empty, absolute, or has a parent reference"; return 1 ;;
  esac
  is_sha256 "$R_source_sha256" || { PARSE_ERROR="source_sha256 is not 64 lowercase hex digits"; return 1; }
  is_sha256 "$R_tool_versions_sha256" || { PARSE_ERROR="tool_versions_sha256 is not 64 lowercase hex digits"; return 1; }
  [ -n "$R_rationale" ] || { PARSE_ERROR="rationale is empty"; return 1; }
  [ -n "$R_evidence_command" ] || { PARSE_ERROR="evidence_command is empty"; return 1; }
  [ -n "$R_evidence_result" ] || { PARSE_ERROR="evidence_result is empty"; return 1; }
  if [ "$R_kind" = equivalent-mutant ]; then
    [[ $R_mutant_id =~ $MUTANT_ID_RE ]] || { PARSE_ERROR="mutant_id is not L<line>-<operator>"; return 1; }
    [ -n "$R_operator" ] && [ "${R_mutant_id#L*-}" = "$R_operator" ] ||
      { PARSE_ERROR="operator does not match mutant_id"; return 1; }
  else
    [ -n "$R_surface" ] || { PARSE_ERROR="surface is empty"; return 1; }
  fi
  case "$R_status" in
    pending)
      [ -z "$R_signer" ] && [ -z "$R_signed_on" ] ||
        { PARSE_ERROR="a pending record carries no signer or signed_on"; return 1; }
      ;;
    *)
      is_date "$R_signed_on" || { PARSE_ERROR="signed_on is not a YYYY-MM-DD date"; return 1; }
      ;;
  esac
  return 0
}

# judge <name> <record-file>: appends exactly one result line for the record.
judge() {
  local name=$1 file=$2 row surface
  if ! parse_record "$file"; then
    emit malformed "" "" "" "$name" "$PARSE_ERROR"
    return 0
  fi
  if ! validate_record; then
    emit malformed "$R_kind" "$R_source_path" "$R_mutant_id" "$name" "$PARSE_ERROR"
    return 0
  fi
  case "$R_status" in
    pending)
      emit pending "$R_kind" "$R_source_path" "$R_mutant_id" "$name" "not signed yet; the survivor stays unresolved"
      return 0
      ;;
    lapsed)
      emit lapsed "$R_kind" "$R_source_path" "$R_mutant_id" "$name" "recorded as lapsed; nothing adjusts"
      return 0
      ;;
  esac
  if [ "$R_signer" != "$OWNER" ]; then
    emit unsigned "$R_kind" "$R_source_path" "$R_mutant_id" "$name" "signer is not the sign-off owner; nothing adjusts"
    return 0
  fi
  if [ "$R_kind" = equivalent-mutant ]; then
    row=$(awk -F "$TAB" -v p="$R_source_path" -v m="$R_mutant_id" \
      '$1 == "mutant" && $2 == p && $4 == m { print $5; exit }' "$EVIDENCE")
    if [ -z "$row" ]; then
      emit unmatched "$R_kind" "$R_source_path" "$R_mutant_id" "$name" "no mutant row for this mutant in the evidence record"
      return 0
    fi
    if [ "$row" != survived ]; then
      emit not-survivor "$R_kind" "$R_source_path" "$R_mutant_id" "$name" "the runner recorded $row for this mutant; only survived can be adjusted"
      return 0
    fi
  else
    surface=$(awk -F "$TAB" -v p="$R_source_path" '$1 == "exclusion" && $2 == p { print $3; exit }' "$EVIDENCE")
    if [ -z "$surface" ]; then
      emit unmatched "$R_kind" "$R_source_path" "" "$name" "no exclusion row for this path in the evidence record"
      return 0
    fi
    if [ "$surface" != "$R_surface" ]; then
      emit unmatched "$R_kind" "$R_source_path" "" "$name" "the exclusion row's surface is $surface, not $R_surface"
      return 0
    fi
  fi
  if [ ! -f "$TOP/$R_source_path" ] || [ "$(sha256_file "$TOP/$R_source_path")" != "$R_source_sha256" ]; then
    emit lapsed "$R_kind" "$R_source_path" "$R_mutant_id" "$name" "source changed or missing since it was signed; nothing adjusts"
    return 0
  fi
  if [ "$R_tool_versions_sha256" != "$CONFIG_SHA" ]; then
    emit lapsed "$R_kind" "$R_source_path" "$R_mutant_id" "$name" "tool or configuration changed since it was signed; nothing adjusts"
    return 0
  fi
  emit applied "$R_kind" "$R_source_path" "$R_mutant_id" "$name" "signed by the sign-off owner; source and configuration match"
}

# Read every record from the ref's objects. A non-regular entry is malformed, and a file
# that is not a *.disposition is not a record and is skipped.
FILES=0
LISTING="$SCRATCH/listing"
if git -C "$TOP" cat-file -e "$REF:$RECDIR" 2>/dev/null; then
  git -C "$TOP" ls-tree -z "$REF:$RECDIR" >"$LISTING" 2>/dev/null ||
    die "could not list $RECDIR at $REF"
  while IFS=$TAB read -r -d '' meta name; do
    case "$name" in
      *.disposition) ;;
      *) continue ;;
    esac
    FILES=$((FILES + 1))
    read -r mode type _sha <<<"$meta"
    if [ "$mode" != 100644 ] || [ "$type" != blob ]; then
      emit malformed "" "" "" "$name" "not a regular file in the ref"
      continue
    fi
    git -C "$TOP" show "$REF:$RECDIR/$name" >"$SCRATCH/record" 2>/dev/null ||
      die "could not read $RECDIR/$name at $REF"
    judge "$name" "$SCRATCH/record"
  done <"$LISTING"
fi

# A second claim on the same mutant or path counts once; the later one is a duplicate.
awk -F "$TAB" -v OFS="$TAB" '
  $1 == "applied" {
    k = $2 SUBSEP $3 SUBSEP $4
    if (k in seen) {
      $1 = "duplicate"
      $6 = "the same claim was already applied from an earlier record"
    } else {
      seen[k] = 1
    }
  }
  { print }' "$RESULTS" >"$SCRATCH/final" || die "could not tally the claims"

COUNTS=$(awk -F "$TAB" '
  { outcome[$1]++ }
  $1 == "applied" { if ($2 == "equivalent-mutant") eq++; else uns++ }
  END {
    printf "%d %d %d %d %d %d %d %d %d %d\n", eq + 0, uns + 0, outcome["duplicate"] + 0,
      outcome["pending"] + 0, outcome["lapsed"] + 0, outcome["unsigned"] + 0,
      outcome["malformed"] + 0, outcome["unmatched"] + 0, outcome["not-survivor"] + 0, 0
  }' "$SCRATCH/final") || die "could not count the claims"
read -r EQ UNS DUP PENDING LAPSED UNSIGNED MALFORMED UNMATCHED NOT_SURVIVOR _unused <<<"$COUNTS"

[ "$EQ" -le "$RAW_SURVIVED" ] ||
  die "more signed equivalent-mutant claims ($EQ) than survived mutants ($RAW_SURVIVED); the evidence record is inconsistent"

printf 'records   ref=%s dir=%s files=%s\n' "$REF" "$RECDIR" "$FILES"
printf 'raw       killed=%s survived=%s timeout=%s error=%s unresolved=%s excluded=%s\n' \
  "$RAW_KILLED" "$RAW_SURVIVED" "$RAW_TIMEOUT" "$RAW_ERROR" "$RAW_UNRESOLVED" "$RAW_EXCLUDED"
printf 'adjusted  killed=%s survived=%s timeout=%s error=%s unresolved=%s excluded=%s equivalent-removed=%s unsupported-signed=%s\n' \
  "$RAW_KILLED" "$((RAW_SURVIVED - EQ))" "$RAW_TIMEOUT" "$RAW_ERROR" "$RAW_UNRESOLVED" "$RAW_EXCLUDED" "$EQ" "$UNS"
printf 'claims    applied=%s pending=%s lapsed=%s unsigned=%s malformed=%s unmatched=%s not-survivor=%s duplicate=%s\n' \
  "$((EQ + UNS))" "$PENDING" "$LAPSED" "$UNSIGNED" "$MALFORMED" "$UNMATCHED" "$NOT_SURVIVOR" "$DUP"
while IFS=$TAB read -r outcome _kind _path _mutant name reason; do
  printf 'record %s: %s: %s\n' "$name" "$outcome" "$reason"
done <"$SCRATCH/final"
exit 0
