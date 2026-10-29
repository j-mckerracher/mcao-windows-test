#!/usr/bin/env bash
# bin/fm-mutation-verdict.sh - the single owner of the mutation verdict. It reads the
# changed-path list, the classifier's TSV, and the runner's evidence record and says
# whether every changed production path has a measured result, with at least one mutant
# killed and nothing else left over. Verified is the only passing verdict. Every other
# outcome is not verified: a missing file, a path with no result, a changed path with no
# classification, an unsupported path, a survivor, a timeout, an error, an unresolved
# path, a malformed evidence record line, or zero killed mutants.
#
# Usage: fm-mutation-verdict.sh [--checks-row <candidate-sha>] <changed-paths> <classifier-tsv> <runner-record>
#   <changed-paths> is the list the classifier was given, one path per line (git diff
#   --name-only). The classifier output and the runner record come from
#   bin/fm-mutation-classify.sh and bin/fm-mutation-run.sh. Every changed path must have a
#   classifier row, so a path the classifier refused or dropped is named as unclassified.
#   The classifier's exit status is not read, so a refused path cannot pass by being absent.
#
# Stdout is one line. It starts with "passed:" only when verified, because that is the
# only first word proof v2 renders as a pass. Stderr names each path that has no
# classification or no result, so the job log shows what is missing.
#
# With --checks-row, stdout is one proof v2 Checks row for the candidate instead:
# "| mutation | bin/fm-mutation-verdict.sh classified record | <verdict> | <sha> |".
# fm-pstack.sh proof-check and render read it as an ordinary row, so a not-verified
# verdict renders with no pass icon and never fails proof-check; blocking stays a
# captain decision. The candidate must be a full 40- or 64-hex commit id, because the
# row's at column must equal it.
#
# Exit codes: 0 verified; 1 not verified; 2 usage.

set -u

TAB=$(printf '\t')

usage() {
  printf '%s\n' "usage: fm-mutation-verdict.sh [--checks-row <candidate-sha>] <changed-paths> <classifier-tsv> <runner-record>" >&2
  exit 2
}

CHECKS_SHA=
if [ "${1:-}" = "--checks-row" ]; then
  [ "$#" -ge 2 ] || usage
  CHECKS_SHA=$2
  shift 2
  [[ $CHECKS_SHA =~ ^([0-9a-f]{40}|[0-9a-f]{64})$ ]] || usage
fi
[ "$#" -eq 3 ] || usage
CHANGED=$1
CLASSIFIED=$2
RECORD=$3

# emit <exit-code> <verdict-line>: the one exit path, so the plain line and the Checks
# row can never disagree about the outcome.
emit() {
  if [ -n "$CHECKS_SHA" ]; then
    printf '| mutation | bin/fm-mutation-verdict.sh classified record | %s | %s |\n' "$2" "$CHECKS_SHA"
  else
    printf '%s\n' "$2"
  fi
  exit "$1"
}

reasons=()
[ -f "$CHANGED" ] || reasons+=("no changed-path list")
[ -f "$CLASSIFIED" ] || reasons+=("no classifier output")
[ -f "$RECORD" ] || reasons+=("no evidence record")

changed=0
rows=0
mutate=0
unsupported=0
malformed=0
badrecord=0
published=0
killed=0
missing=0
unclassified=0
statuses=()

if [ "${#reasons[@]}" -eq 0 ]; then
  # One pass over all three files. FILENAME tells the changed-path list, the classifier
  # rows, and the record rows apart, which keeps an empty file from being read as another.
  # Every changed path must appear among the classifier rows: one that does not is
  # unclassified, whatever the classifier's exit status was.
  STATS=$(awk -F "$TAB" -v chg="$CHANGED" -v cls="$CLASSIFIED" '
    FILENAME == chg {
      if ($0 != "") want_changed[$0] = 1
      next
    }
    FILENAME == cls {
      if (NF == 0) next
      rows++
      seen[$1] = 1
      if ($3 == "mutate") { mutate++; want[$1] = 1 }
      else if ($3 == "unsupported") unsupported++
      else malformed++
      next
    }
    NF == 0 { next }
    # Every evidence line is one of the five kinds the runner writes, with six columns.
    # Anything else is a malformed record, which is never passed over silently.
    $1 !~ /^(tool|mutant|path|exclusion|summary)$/ || NF != 6 {
      printf "badrecord\t%d\n", FNR
      next
    }
    $1 == "summary" && $5 == "published" { published++ }
    $1 == "mutant" || $1 == "path" {
      got[$2] = 1
      if ($5 == "killed") killed++
      else other[$5]++
    }
    END {
      for (p in want_changed) {
        changed++
        if (!(p in seen)) printf "unclassified\t%s\n", p
      }
      printf "changed\t%d\n", changed + 0
      printf "rows\t%d\n", rows + 0
      printf "mutate\t%d\n", mutate + 0
      printf "unsupported\t%d\n", unsupported + 0
      printf "malformed\t%d\n", malformed + 0
      printf "published\t%d\n", published + 0
      printf "killed\t%d\n", killed + 0
      for (p in want) if (!(p in got)) printf "missing\t%s\n", p
      for (s in other) printf "status\t%s\t%d\n", s, other[s]
    }' "$CHANGED" "$CLASSIFIED" "$RECORD") || {
    emit 1 "not verified: could not read the changed-path list, the classifier output, or the evidence record"
  }
  while IFS=$TAB read -r key name count; do
    case "$key" in
      changed) changed=$name ;;
      rows) rows=$name ;;
      mutate) mutate=$name ;;
      unsupported) unsupported=$name ;;
      malformed) malformed=$name ;;
      badrecord)
        badrecord=$((badrecord + 1))
        printf 'fm-mutation-verdict.sh: malformed evidence record at line %s\n' "$name" >&2
        ;;
      published) published=$name ;;
      killed) killed=$name ;;
      unclassified)
        unclassified=$((unclassified + 1))
        printf 'fm-mutation-verdict.sh: no classification for %s\n' "$name" >&2
        ;;
      missing)
        missing=$((missing + 1))
        printf 'fm-mutation-verdict.sh: no result for %s\n' "$name" >&2
        ;;
      status) statuses+=("$count $name") ;;
    esac
  done <<<"$STATS"
fi

if [ "${#reasons[@]}" -eq 0 ]; then
  [ "$changed" -gt 0 ] || reasons+=("no changed paths")
  [ "$rows" -gt 0 ] || reasons+=("no classified paths")
  [ "$unclassified" -eq 0 ] || reasons+=("$unclassified changed path(s) with no classification")
  [ "$mutate" -gt 0 ] || reasons+=("no path with a mutation surface")
  [ "$malformed" -eq 0 ] || reasons+=("$malformed classifier row(s) with an unknown disposition")
  [ "$unsupported" -eq 0 ] || reasons+=("$unsupported path(s) without a mutation surface")
  [ "$badrecord" -eq 0 ] || reasons+=("$badrecord malformed evidence record line(s)")
  [ "$published" -ge 1 ] || reasons+=("no published evidence record")
  [ "$missing" -eq 0 ] || reasons+=("$missing path(s) with no result")
  if [ "${#statuses[@]}" -gt 0 ]; then
    for s in "${statuses[@]}"; do
      reasons+=("$s")
    done
  fi
  [ "$killed" -ge 1 ] || reasons+=("no killed mutant")
fi

if [ "${#reasons[@]}" -eq 0 ]; then
  emit 0 "$(printf 'passed: %s mutant(s) killed across %s path(s); no survived, timeout, error, unresolved, unsupported, unclassified, or missing result' \
    "$killed" "$mutate")"
fi

line="not verified:"
sep=" "
for r in "${reasons[@]}"; do
  line="$line$sep$r"
  sep="; "
done
emit 1 "$line"
