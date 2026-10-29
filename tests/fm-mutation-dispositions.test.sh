#!/usr/bin/env bash
# tests/fm-mutation-dispositions.test.sh - the disposition consumer
# (bin/fm-mutation-dispositions.sh) through its command line. Every fixture is a temp git
# repository built here, holding its source, its runner evidence, and its disposition
# records. No real mutant is claimed. A signed fixture names the sign-off owner the surface
# lib owns, because this test checks how the consumer reads a record, not any signature.
set -u

# shellcheck source=tests/lib.sh
. "$(dirname "${BASH_SOURCE[0]}")/lib.sh"

CONSUMER="$ROOT/bin/fm-mutation-dispositions.sh"
TMP=$(fm_test_tmproot fm-mutation-dispositions)
ERR="$TMP/stderr"
TAB=$(printf '\t')
FORMAT=fm-mutation-disposition/1
REC=docs/mutation-dispositions
OWNER=$(. "$ROOT/bin/fm-mutation-surface-lib.sh" && fm_mutation_equivalence_signoff_owner)

# line <column>...: one runner record line, its columns joined by TAB.
line() {
  local IFS=$TAB
  printf '%s\n' "$*"
}

# sha256_of <file>: the digest the record format uses, lowercase hex.
sha256_of() {
  if command -v shasum >/dev/null 2>&1; then
    shasum -a 256 "$1" | awk '{ print $1 }'
  else
    sha256sum "$1" | awk '{ print $1 }'
  fi
}

# tool_digest <evidence>: the configuration digest a claim binds to, over the tool rows.
tool_digest() {
  awk -F "$TAB" '$1 == "tool"' "$1" > "$TMP/tool-rows"
  sha256_of "$TMP/tool-rows"
}

# evidence <file> <mutant-status> [<tool-version>]: the runner's record for bin/a.sh with
# mutant L3-eq-ne in <mutant-status> and mutant L5-and-or killed. The published summary
# counts follow the status.
evidence() {
  local status=$2 version=${3:-bash-5.2} killed=1 survived=0 timeout=0 error=0
  case "$status" in
    killed) killed=2 ;;
    survived) survived=1 ;;
    timeout) timeout=1 ;;
    error) error=1 ;;
  esac
  {
    line tool shell-custom-mutator - - builtin "$version"
    line mutant bin/a.sh shell L3-eq-ne "$status" 'the oracle reported this status'
    line mutant bin/a.sh shell L5-and-or killed 'caught by the oracle'
    line summary - - - published "killed=$killed survived=$survived timeout=$timeout error=$error unresolved=0 excluded=0"
  } > "$1"
}

# evidence_excluded <file>: the record for a run whose changed path bin/mcao has no
# mutation surface, so the runner writes an exclusion row for it.
evidence_excluded() {
  {
    line tool shell-custom-mutator - - builtin bash-5.2
    line mutant bin/a.sh shell L3-eq-ne survived 'the oracle accepted it'
    line exclusion bin/mcao shell - unsupported 'extensionless bash entry point'
    line summary - - - published 'killed=0 survived=1 timeout=0 error=0 unresolved=0 excluded=1'
  } > "$1"
}

# write_eq <file> <status> <signer> <signed-on> <source-sha> <config-sha> [<mutant-id> <operator>]
# writes an equivalent-mutant record. A pending record passes empty signer and signed-on.
write_eq() {
  mkdir -p "$(dirname "$1")"
  printf '%s\n' \
    "format=$FORMAT" \
    "kind=equivalent-mutant" \
    "status=$2" \
    "source_path=bin/a.sh" \
    "source_sha256=$5" \
    "tool_versions_sha256=$6" \
    "rationale=the negated comparison returns the same result on every reachable input" \
    "evidence_command=bash tests/a.test.sh" \
    "evidence_result=the suite passes with the mutant applied" \
    "signer=$3" \
    "signed_on=$4" \
    "mutant_id=${7:-L3-eq-ne}" \
    "operator=${8:-eq-ne}" > "$1"
}

# write_unsup <file> <status> <signer> <signed-on> <source-sha> <config-sha> <surface>
# writes an unsupported-surface record for bin/mcao.
write_unsup() {
  mkdir -p "$(dirname "$1")"
  printf '%s\n' \
    "format=$FORMAT" \
    "kind=unsupported-surface" \
    "status=$2" \
    "source_path=bin/mcao" \
    "source_sha256=$5" \
    "tool_versions_sha256=$6" \
    "rationale=the entry point has no mutation surface the runner can measure" \
    "evidence_command=bash tests/mcao.test.sh" \
    "evidence_result=the entry point is covered by its own suite" \
    "signer=$3" \
    "signed_on=$4" \
    "surface=$7" > "$1"
}

# commit_all <repo> <message>: stage everything and commit it on the current branch.
commit_all() {
  git -C "$1" add -A || fail "fixture stage failed: $2"
  git -C "$1" -c user.name=fixture -c user.email=fixture@example.invalid -c commit.gpgsign=false \
    commit -q -m "$2" || fail "fixture commit failed: $2"
}

# new_repo <name>: a git repository on main holding bin/a.sh and bin/mcao. Sets D to the
# repository and EV to its evidence path, which sits outside the repository.
new_repo() {
  D="$TMP/$1"
  EV="$TMP/$1.evidence"
  rm -rf "$D" "$EV"
  mkdir -p "$D/bin"
  git -C "$D" init -q || fail "git init failed for $1"
  git -C "$D" symbolic-ref HEAD refs/heads/main || fail "could not name main for $1"
  printf '%s\n' 'echo original' > "$D/bin/a.sh"
  printf '%s\n' 'echo mcao' > "$D/bin/mcao"
  commit_all "$D" "base"
}

# consume [<option>...]: runs the consumer on the current fixture ($D, $EV). Sets OUT, RC
# and ERR (stderr). Options go before the evidence path.
consume() {
  OUT=$("$CONSUMER" --root "$D" "$@" "$EV" 2>"$ERR")
  RC=$?
}

# field <label> <key>: the value of <key> on the output line that starts with <label>.
field() {
  printf '%s\n' "$OUT" | awk -v label="$1" -v key="$2" '
    $1 == label {
      for (i = 2; i <= NF; i++) {
        n = split($i, kv, "=")
        if (kv[1] == key) { print kv[2]; exit }
      }
    }'
}

# record_line <file>: the output line for one record file.
record_line() {
  printf '%s\n' "$OUT" | awk -v prefix="record $1: " 'index($0, prefix) == 1 { print; exit }'
}

# outcome_of <file>: the outcome word on a record's output line.
outcome_of() {
  record_line "$1" | awk -F ': ' '{ print $2 }'
}

# --- valid, pending, and the printed counts --------------------------------------

new_repo valid
evidence "$EV" survived
write_eq "$D/$REC/eq.disposition" signed "$OWNER" 2026-10-01 \
  "$(sha256_of "$D/bin/a.sh")" "$(tool_digest "$EV")"
printf 'not a record\n' > "$D/$REC/notes.txt"
commit_all "$D" "record the equivalent mutant"
consume --ref main
expect_code 0 "$RC" "a valid signed record exits 0"
assert_equals 1 "$(field raw survived)" "raw survived is the runner's published count"
assert_equals 1 "$(field raw killed)" "raw killed is the runner's published count"
assert_equals 0 "$(field adjusted survived)" "adjusted survived drops the signed equivalent mutant"
assert_equals 1 "$(field adjusted equivalent-removed)" "one equivalent mutant is removed from survivors"
assert_equals 0 "$(field adjusted unsupported-signed)" "no unsupported claim is counted"
assert_equals 1 "$(field claims applied)" "one claim applies"
assert_equals 1 "$(field records files)" "only *.disposition files are read as records"
assert_equals applied "$(outcome_of eq.disposition)" "a signed record from main applies"
printf '%s\n' "$OUT" | awk '$1 !~ /^(records|raw|adjusted|claims|record)$/ { bad = 1 } END { exit bad }'
expect_code 0 "$?" "stdout holds only the consumer's report lines"
pass "a valid signed record adjusts survivors, and both raw and adjusted counts are printed"

new_repo pending
evidence "$EV" survived
write_eq "$D/$REC/eq.disposition" pending "" "" \
  "$(sha256_of "$D/bin/a.sh")" "$(tool_digest "$EV")"
commit_all "$D" "record a pending claim"
consume --ref main
expect_code 0 "$RC" "a pending record exits 0"
assert_equals pending "$(outcome_of eq.disposition)" "a pending record is reported as pending"
assert_equals 1 "$(field adjusted survived)" "a pending claim leaves the survivor unadjusted"
assert_equals 0 "$(field adjusted equivalent-removed)" "a pending claim removes nothing"
assert_equals 1 "$(field claims pending)" "one pending claim is counted"
pass "a pending record adjusts nothing"

# --- validity is bound to the source digest and the configuration ----------------

new_repo source-change
evidence "$EV" survived
write_eq "$D/$REC/eq.disposition" signed "$OWNER" 2026-10-01 \
  "$(sha256_of "$D/bin/a.sh")" "$(tool_digest "$EV")"
commit_all "$D" "record the equivalent mutant"
printf '%s\n' 'echo changed after signing' > "$D/bin/a.sh"
consume --ref main
expect_code 0 "$RC" "a source change after signing exits 0"
assert_equals lapsed "$(outcome_of eq.disposition)" "a record lapses when its source changes"
assert_contains "$(record_line eq.disposition)" "source changed" "the lapse names the source change"
assert_equals 1 "$(field adjusted survived)" "a lapsed claim adjusts nothing"
pass "a claim lapses when its source digest no longer matches"

new_repo config-change
evidence "$EV" survived bash-5.2
write_eq "$D/$REC/eq.disposition" signed "$OWNER" 2026-10-01 \
  "$(sha256_of "$D/bin/a.sh")" "$(tool_digest "$EV")"
commit_all "$D" "record the equivalent mutant"
evidence "$EV" survived bash-5.3
consume --ref main
expect_code 0 "$RC" "a configuration change exits 0"
assert_equals lapsed "$(outcome_of eq.disposition)" "a record lapses when the configuration changes"
assert_contains "$(record_line eq.disposition)" "tool or configuration changed" \
  "the lapse names the configuration change"
assert_equals 1 "$(field adjusted survived)" "a configuration-lapsed claim adjusts nothing"
pass "a claim lapses when its configuration digest no longer matches"

new_repo explicit-lapsed
evidence "$EV" survived
write_eq "$D/$REC/eq.disposition" lapsed "$OWNER" 2026-10-01 \
  "$(sha256_of "$D/bin/a.sh")" "$(tool_digest "$EV")"
commit_all "$D" "record a lapsed claim"
consume --ref main
expect_code 0 "$RC" "an explicitly lapsed record exits 0"
assert_equals lapsed "$(outcome_of eq.disposition)" "an explicitly lapsed record is reported as lapsed"
assert_equals 1 "$(field adjusted survived)" "an explicitly lapsed claim adjusts nothing"
pass "an explicitly lapsed record adjusts nothing"

new_repo unsigned
evidence "$EV" survived
write_eq "$D/$REC/eq.disposition" signed "Someone Else" 2026-10-01 \
  "$(sha256_of "$D/bin/a.sh")" "$(tool_digest "$EV")"
commit_all "$D" "record with another signer"
consume --ref main
expect_code 0 "$RC" "a record from a non-owner signer exits 0"
assert_equals unsigned "$(outcome_of eq.disposition)" "a signer other than the sign-off owner is unsigned"
assert_equals 1 "$(field adjusted survived)" "an unsigned claim adjusts nothing"
pass "a record whose signer is not the sign-off owner adjusts nothing"

# --- malformed records never adjust ----------------------------------------------

new_repo malformed
evidence "$EV" survived
SRC=$(sha256_of "$D/bin/a.sh")
CFG=$(tool_digest "$EV")
BASE="$TMP/malformed-base"
write_eq "$BASE" signed "$OWNER" 2026-10-01 "$SRC" "$CFG"
CASES="$TMP/malformed-cases"
mkdir -p "$D/$REC"
cat > "$CASES" <<CASES_EOF
format|format is not fm-mutation-disposition/1
missing|missing key: rationale
digest|source_sha256 is not 64 lowercase hex digits
date|signed_on is not a YYYY-MM-DD date
operator|operator does not match mutant_id
unknown|unknown key: extra
status|status is not pending, signed, or lapsed
pending|a pending record carries no signer or signed_on
repeat|repeated key: status
blank|a line is not key=value
kind|kind is not equivalent-mutant or unsupported-surface
surface|key surface does not apply to kind equivalent-mutant
CASES_EOF
sed 's|^format=.*|format=fm-mutation-disposition/2|' "$BASE" > "$D/$REC/format.disposition"
sed '/^rationale=/d' "$BASE" > "$D/$REC/missing.disposition"
sed 's|^source_sha256=.*|source_sha256=NOTHEX|' "$BASE" > "$D/$REC/digest.disposition"
sed 's|^signed_on=.*|signed_on=2026-13-45|' "$BASE" > "$D/$REC/date.disposition"
sed 's|^operator=.*|operator=and-or|' "$BASE" > "$D/$REC/operator.disposition"
{ cat "$BASE"; printf 'extra=1\n'; } > "$D/$REC/unknown.disposition"
sed 's|^status=.*|status=maybe|' "$BASE" > "$D/$REC/status.disposition"
sed 's|^status=.*|status=pending|' "$BASE" > "$D/$REC/pending.disposition"
{ cat "$BASE"; printf 'status=signed\n'; } > "$D/$REC/repeat.disposition"
{ head -n 1 "$BASE"; printf '\n'; tail -n +2 "$BASE"; } > "$D/$REC/blank.disposition"
sed 's|^kind=.*|kind=equivalent|' "$BASE" > "$D/$REC/kind.disposition"
{ cat "$BASE"; printf 'surface=shell\n'; } > "$D/$REC/surface.disposition"
commit_all "$D" "record malformed claims"
consume --ref main
expect_code 0 "$RC" "malformed records exit 0 and are reported, not fatal"
assert_equals "$(awk 'END { print NR }' "$CASES")" "$(field claims malformed)" \
  "every malformed record is counted as malformed"
assert_equals 0 "$(field claims applied)" "no malformed record applies"
assert_equals 1 "$(field adjusted survived)" "malformed records adjust nothing"
while IFS='|' read -r name reason; do
  assert_equals malformed "$(outcome_of "$name.disposition")" "$name record is malformed"
  assert_contains "$(record_line "$name.disposition")" "$reason" "$name record names its defect"
done <"$CASES"
pass "malformed records are named with their defect and adjust nothing"

# --- records are read from the ref, never the working tree or a PR branch ---------

new_repo not-on-main
evidence "$EV" survived
SRC=$(sha256_of "$D/bin/a.sh")
CFG=$(tool_digest "$EV")
git -C "$D" checkout -q -b pr || fail "could not create the PR branch"
write_eq "$D/$REC/pr.disposition" signed "$OWNER" 2026-10-01 "$SRC" "$CFG"
commit_all "$D" "record on the PR branch"
git -C "$D" checkout -q main || fail "could not return to main"
write_eq "$D/$REC/loose.disposition" signed "$OWNER" 2026-10-01 "$SRC" "$CFG"
consume --ref main
expect_code 0 "$RC" "records absent from main exit 0"
assert_equals 0 "$(field records files)" "a record on a PR branch or in the working tree is not read"
assert_equals 0 "$(field claims applied)" "no claim applies from outside main"
assert_equals 1 "$(field adjusted survived)" "survivors are not adjusted from outside main"
pass "a record on a PR branch or in the working tree adjusts nothing"

new_repo default-ref
evidence "$EV" survived
SRC=$(sha256_of "$D/bin/a.sh")
CFG=$(tool_digest "$EV")
REMOTE="$TMP/default-ref.git"
rm -rf "$REMOTE"
git init -q --bare "$REMOTE" || fail "could not create the bare remote"
git -C "$D" remote add origin "$REMOTE" || fail "could not add the remote"
git -C "$D" push -q origin main || fail "could not push main"
git -C "$D" checkout -q -b pr || fail "could not create the PR branch"
write_eq "$D/$REC/pr.disposition" signed "$OWNER" 2026-10-01 "$SRC" "$CFG"
commit_all "$D" "record on the pushed PR branch"
git -C "$D" push -q origin pr || fail "could not push the PR branch"
git -C "$D" checkout -q main || fail "could not return to main"
write_eq "$D/$REC/loose.disposition" signed "$OWNER" 2026-10-01 "$SRC" "$CFG"
consume
expect_code 0 "$RC" "the default ref exits 0"
assert_contains "$OUT" "ref=origin/main" "the default ref is origin/main"
assert_equals 0 "$(field records files)" "a pushed PR branch is not read by default"
assert_equals 1 "$(field adjusted survived)" "an unmerged PR cannot adjust survivors by default"
git -C "$D" -c user.name=fixture -c user.email=fixture@example.invalid -c commit.gpgsign=false \
  merge -q --no-ff -m "merge the record" pr || fail "could not merge the PR branch"
git -C "$D" push -q origin main || fail "could not push the merged main"
consume
assert_equals 1 "$(field records files)" "a record merged to origin/main is read"
assert_equals 1 "$(field adjusted equivalent-removed)" "a merged signed record adjusts survivors"
pass "only a record merged to origin/main adjusts survivors by default"

# --- timeout and error are never reclassified -------------------------------------

for status in timeout error; do
  new_repo "$status-mutant"
  evidence "$EV" "$status"
  write_eq "$D/$REC/eq.disposition" signed "$OWNER" 2026-10-01 \
    "$(sha256_of "$D/bin/a.sh")" "$(tool_digest "$EV")"
  commit_all "$D" "record a claim over a $status"
  consume --ref main
  expect_code 0 "$RC" "a signed claim over a $status exits 0"
  assert_equals not-survivor "$(outcome_of eq.disposition)" "a claim over a $status is not a survivor"
  assert_equals 0 "$(field adjusted equivalent-removed)" "a $status is never reclassified as equivalent"
  assert_equals 1 "$(field adjusted $status)" "the $status count stays as the runner reported it"
  assert_equals 0 "$(field adjusted survived)" "the survivor count is unchanged by a $status"
done
pass "timeout and error mutants are never reclassified"

# --- unmatched, duplicate, and unsupported-surface claims ------------------------

new_repo unmatched
evidence "$EV" survived
write_eq "$D/$REC/eq.disposition" signed "$OWNER" 2026-10-01 \
  "$(sha256_of "$D/bin/a.sh")" "$(tool_digest "$EV")" L9-eq-ne eq-ne
commit_all "$D" "record a mutant the runner did not measure"
consume --ref main
expect_code 0 "$RC" "an unmatched claim exits 0"
assert_equals unmatched "$(outcome_of eq.disposition)" "a claim for an unmeasured mutant is unmatched"
assert_equals 1 "$(field adjusted survived)" "an unmatched claim adjusts nothing"
pass "a claim for a mutant absent from the evidence is unmatched"

new_repo duplicate
evidence "$EV" survived
SRC=$(sha256_of "$D/bin/a.sh")
CFG=$(tool_digest "$EV")
write_eq "$D/$REC/first.disposition" signed "$OWNER" 2026-10-01 "$SRC" "$CFG"
write_eq "$D/$REC/second.disposition" signed "$OWNER" 2026-10-02 "$SRC" "$CFG"
commit_all "$D" "record the same mutant twice"
consume --ref main
expect_code 0 "$RC" "duplicate claims exit 0"
assert_equals applied "$(outcome_of first.disposition)" "the first claim on a mutant applies"
assert_equals duplicate "$(outcome_of second.disposition)" "a second claim on the same mutant is a duplicate"
assert_equals 1 "$(field adjusted equivalent-removed)" "a mutant is removed once, however many claims name it"
assert_equals 0 "$(field adjusted survived)" "the survivor count never drops below zero"
pass "a repeated claim on one mutant counts once"

new_repo unsupported
evidence_excluded "$EV"
SRC=$(sha256_of "$D/bin/mcao")
CFG=$(tool_digest "$EV")
write_unsup "$D/$REC/mcao.disposition" signed "$OWNER" 2026-10-01 "$SRC" "$CFG" shell
commit_all "$D" "record an unsupported surface"
consume --ref main
expect_code 0 "$RC" "a signed unsupported-surface claim exits 0"
assert_equals applied "$(outcome_of mcao.disposition)" "a signed unsupported-surface claim applies"
assert_equals 1 "$(field adjusted unsupported-signed)" "the unsupported claim is shown beside the excluded count"
assert_equals 1 "$(field adjusted excluded)" "the excluded count is unchanged by an unsupported claim"
assert_equals 0 "$(field adjusted equivalent-removed)" "an unsupported claim removes no survivor"
assert_equals 1 "$(field adjusted survived)" "the survivor count is unchanged by an unsupported claim"
pass "a signed unsupported-surface claim is shown beside the excluded count and changes nothing else"

new_repo unsupported-surface
evidence_excluded "$EV"
SRC=$(sha256_of "$D/bin/mcao")
CFG=$(tool_digest "$EV")
write_unsup "$D/$REC/mcao.disposition" signed "$OWNER" 2026-10-01 "$SRC" "$CFG" docs
commit_all "$D" "record an unsupported surface with the wrong class"
consume --ref main
expect_code 0 "$RC" "an unsupported claim with the wrong surface exits 0"
assert_equals unmatched "$(outcome_of mcao.disposition)" "an unsupported claim with the wrong surface is unmatched"
assert_contains "$(record_line mcao.disposition)" "the exclusion row's surface is shell, not docs" \
  "the unmatched surface is named"
pass "an unsupported claim whose surface does not match the exclusion row is unmatched"

# --- non-regular records and refusals --------------------------------------------

new_repo symlink
evidence "$EV" survived
mkdir -p "$D/$REC"
ln -s ../../bin/a.sh "$D/$REC/link.disposition"
commit_all "$D" "record a symlink"
consume --ref main
expect_code 0 "$RC" "a symlink record exits 0"
assert_equals malformed "$(outcome_of link.disposition)" "a symlink in the records directory is malformed"
assert_contains "$(record_line link.disposition)" "not a regular file in the ref" \
  "the symlink is named as not a regular file"
pass "a non-regular record in the ref is malformed"

new_repo usage
evidence "$EV" survived
OUT=$("$CONSUMER" 2>"$ERR")
expect_code 2 "$?" "no arguments is a usage error"
OUT=$("$CONSUMER" --root "$D" --records ../escape "$EV" 2>"$ERR")
expect_code 2 "$?" "a records path that escapes the ref is a usage error"
assert_equals "" "$OUT" "a usage error prints no report"
pass "a call without a runner record or with an escaping records path is refused"

new_repo missing-evidence
consume --ref main
expect_code 1 "$RC" "a missing runner record exits 1"
assert_equals "" "$OUT" "a missing runner record prints no report, so nothing is adjusted"
pass "an unreadable runner record exits 1 with no report"

new_repo bad-ref
evidence "$EV" survived
consume --ref no-such-ref
expect_code 1 "$RC" "an unreadable ref exits 1"
assert_equals "" "$OUT" "an unreadable ref prints no report, so nothing is adjusted"
assert_contains "$(cat "$ERR")" "nothing is adjusted" "an unreadable ref says nothing was adjusted"
pass "an unreadable ref exits 1 with no report"

new_repo no-summary
line tool shell-custom-mutator - - builtin bash-5.2 > "$EV"
line mutant bin/a.sh shell L3-eq-ne survived 'the oracle accepted it' >> "$EV"
consume --ref main
expect_code 1 "$RC" "a runner record with no published summary exits 1"
assert_equals "" "$OUT" "a runner record with no published summary prints no report"
pass "a runner record without exactly one published summary is refused"

new_repo no-tool
line mutant bin/a.sh shell L3-eq-ne survived 'the oracle accepted it' > "$EV"
line summary - - - published 'killed=0 survived=1 timeout=0 error=0 unresolved=0 excluded=0' >> "$EV"
consume --ref main
expect_code 1 "$RC" "a runner record with no tool row exits 1"
assert_equals "" "$OUT" "a runner record with no tool row prints no report"
pass "a runner record without a tool row is refused, never digested as empty"

echo "# fm-mutation-dispositions.test.sh: all assertions passed"
