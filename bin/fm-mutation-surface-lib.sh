#!/usr/bin/env bash
# fm-mutation-surface-lib.sh - the single owner of the mutation surface
# mapping: which surface class each changed path of an MCAO-produced change
# belongs to, and which mutation disposition that class receives. Source-safe,
# no side effects. bin/fm-mutation-classify.sh is the executable interface.
#
# Surface classes are matched in this order, and the first match wins:
#   test-code   tests/** or */tests/**         unsupported: test code is the oracle, never mutated
#   skill-md    basename SKILL.md              unsupported: agent instruction prose, not executable
#   shell       *.sh                           mutate: shell-custom-mutator, report-only start,
#                                              validated by the existing tests/*.test.sh suite
#   python      *.py                           mutate: mutmut-class
#   typescript  *.ts, *.tsx, *.mts, *.cts      mutate: stryker-class
#   javascript  *.js, *.jsx, *.mjs, *.cjs      mutate: stryker-class
#   yaml        *.yml, *.yaml                  unsupported: actionlint and schema checks are reported
#                                              separately and never counted as mutation evidence
#   json        *.json                         unsupported: declarative data, no mutation operator
#   toml        *.toml                         unsupported: declarative configuration, no mutation operator
#   markdown    *.md                           unsupported: prose documentation, no mutation operator
#
# A path that matches no class makes fm_mutation_surface_of return nonzero.
# Callers must fail the classification step on that result and never treat the
# path as skipped. Mutation execution, thresholds, and enforcement are separate
# stages and live elsewhere.
#
# Each unmatched tracked path has its own explicit unsupported record in
# fm_mutation_unmatched_disposition_of, with a stated reason. A record never
# makes the path classify: fm_mutation_surface_of still returns nonzero for it.
# fm_mutation_equivalence_signoff_owner names who signs off every
# equivalent-mutant claim, and each claim needs its own reviewable record.

# fm_mutation_surface_of <path>
#   Prints the surface class of <path>. Returns nonzero and prints nothing when
#   no surface class matches.
fm_mutation_surface_of() {
  local path=$1 base
  case "$path" in
    tests/*|*/tests/*) printf '%s\n' test-code; return 0 ;;
  esac
  base=${path##*/}
  case "$base" in
    SKILL.md) printf '%s\n' skill-md ;;
    *.sh) printf '%s\n' shell ;;
    *.py) printf '%s\n' python ;;
    *.ts|*.tsx|*.mts|*.cts) printf '%s\n' typescript ;;
    *.js|*.jsx|*.mjs|*.cjs) printf '%s\n' javascript ;;
    *.yml|*.yaml) printf '%s\n' yaml ;;
    *.json) printf '%s\n' json ;;
    *.toml) printf '%s\n' toml ;;
    *.md) printf '%s\n' markdown ;;
    *) return 1 ;;
  esac
}

# fm_mutation_disposition_of <surface>
#   Prints "<disposition>\t<detail>". Disposition is "mutate" with the strategy
#   as detail, or "unsupported" with the stated reason as detail. Returns
#   nonzero for an unknown surface class.
fm_mutation_disposition_of() {
  case "$1" in
    shell) printf 'mutate\tshell-custom-mutator (report-only)\n' ;;
    python) printf 'mutate\tmutmut-class\n' ;;
    typescript|javascript) printf 'mutate\tstryker-class\n' ;;
    test-code) printf 'unsupported\ttest code is the oracle the mutation run measures against; never mutated\n' ;;
    yaml) printf 'unsupported\tnot mutated; actionlint and schema checks are reported separately and never counted as mutation evidence\n' ;;
    json) printf 'unsupported\tdeclarative data; mutating values changes configuration rather than checked behavior, and no operator is defined\n' ;;
    toml) printf 'unsupported\tdeclarative configuration; no mutation operator is defined\n' ;;
    markdown) printf 'unsupported\tprose documentation, not executable; no mutation operator is defined\n' ;;
    skill-md) printf 'unsupported\tagent instruction prose loaded by agents, not executable; no mutation operator is defined\n' ;;
    *) return 1 ;;
  esac
}

# fm_mutation_classify_path <path>
#   Prints "<surface>\t<disposition>\t<detail>" for <path>. Returns nonzero and
#   prints nothing when <path> matches no surface class.
fm_mutation_classify_path() {
  local surface
  surface=$(fm_mutation_surface_of "$1") || return 1
  printf '%s\t%s\n' "$surface" "$(fm_mutation_disposition_of "$surface")"
}

# fm_mutation_unmatched_disposition_of <path>
#   Prints "unsupported\t<reason>" for an unmatched tracked path with an explicit
#   record. Returns nonzero for every other path, including paths a surface class
#   matches, so a record is never mistaken for a classification.
fm_mutation_unmatched_disposition_of() {
  case "$1" in
    .agents/skills/bearings/assets/board-template.html) printf 'unsupported\tstatic HTML template rendered by bin/fm-bearings-board.sh; no HTML mutation operator is defined, and rendered output is covered by tests/fm-bearings-board-render.test.sh\n' ;;
    .agents/skills/firstmate-calm) printf 'unsupported\tsymlink to ../../.claude/mods/firstmate-calm; the linked TypeScript files are classified under their own paths, so the link adds no mutation surface\n' ;;
    .claude/skills) printf 'unsupported\tsymlink to ../.agents/skills; the linked skill files are classified under their own paths, so the link adds no mutation surface\n' ;;
    .gitattributes) printf 'unsupported\tgit attribute declarations; declarative repository metadata, not executable, and no mutation operator is defined\n' ;;
    .github/CODEOWNERS) printf 'unsupported\tGitHub code-owner declarations; declarative review-routing data, not executable, and no mutation operator is defined\n' ;;
    .gitignore) printf 'unsupported\tgit ignore patterns; declarative repository metadata, not executable, and no mutation operator is defined\n' ;;
    LICENSE) printf 'unsupported\tlicense text; prose legal document, not executable, and no mutation operator is defined\n' ;;
    UPSTREAM) printf 'unsupported\tone-line upstream firstmate commit SHA record read by docs/RELEASE.md; declarative data, not executable, and no mutation operator is defined\n' ;;
    assets/birds-eye-view/app.css) printf 'unsupported\tstylesheet served by the birds-eye dashboard; declarative styling, no CSS mutation operator is defined, and rendered behavior is covered by tests/fm-birds-eye-ui.test.sh\n' ;;
    assets/birds-eye-view/index.html) printf 'unsupported\tstatic HTML page served by the birds-eye dashboard; no HTML mutation operator is defined, and rendered behavior is covered by tests/fm-birds-eye-ui.test.sh\n' ;;
    assets/banner.png) printf 'unsupported\tbinary image asset; not executable, and no image mutation operator is defined\n' ;;
    bin/fm-birds-eye-model.jq) printf 'unsupported\tjq aggregation module loaded by bin/fm-birds-eye-model.sh; no jq mutation operator is defined, and its behavior is covered by tests/fm-birds-eye-model.test.sh\n' ;;
    bin/fm-contributions.jq) printf 'unsupported\tjq library module loaded by bin/fm-contributions.sh; no jq mutation operator is defined, and its behavior is covered by tests/fm-contributions.test.sh\n' ;;
    bin/fm-procevent-extension-capture.pl) printf 'unsupported\tPerl executable loaded by bin/fm-procevent.sh; no Perl mutation operator is defined, so it gets no mutation evidence, and its behavior is covered by tests/fm-extension-binding.test.sh\n' ;;
    bin/mcao) printf 'unsupported\textensionless bash entry point; the suffix-based surface lookup does not map it to shell-custom-mutator, so it gets no mutation evidence, and mapping it is follow-up work outside this records-only change\n' ;;
    docs/examples/wedge-alarm) printf 'unsupported\textensionless example configuration copied to config/wedge-alarm; declarative configuration, and no mutation operator is defined\n' ;;
    docs/mutation-oracle-map.tsv) printf 'unsupported\ttab-separated oracle map read by bin/fm-oracle-map-check.sh; declarative data, not executable, and no mutation operator is defined, and its checks are covered by tests/fm-oracle-map-check.test.sh\n' ;;
    docs/pi-supervision-branch-poster.svg) printf 'unsupported\tSVG poster graphic referenced by docs/pi-supervision-branch.md; not executable, and no image mutation operator is defined\n' ;;
    docs/verification/mutation-baseline/distribution.tsv) printf 'unsupported\taggregated mutation-baseline distribution table documented in docs/verification/mutation-baseline.md; declarative measurement data, not executable, and no mutation operator is defined\n' ;;
    docs/verification/mutation-baseline/long.tsv) printf 'unsupported\taggregated per-run mutation-baseline measurements documented in docs/verification/mutation-baseline.md; declarative measurement data, not executable, and no mutation operator is defined\n' ;;
    docs/verification/mutation-toolchain/python-constraints.proposed) printf 'unsupported\tproposed pip requirements inventory, not installed or executed; declarative pins with no mutation operator, reviewed in docs/verification/mutation-toolchain-lock.md\n' ;;
    docs/verification/mutation-toolchain/stryker-lock.proposed.tsv) printf 'unsupported\tproposed npm package inventory, not installed or executed; declarative pins with no mutation operator, reviewed in docs/verification/mutation-toolchain-lock.md\n' ;;
    *) return 1 ;;
  esac
}

# fm_mutation_equivalence_signoff_owner
#   Prints the owner who signs off every equivalent-mutant claim. A claim is
#   valid only with an explicit reviewable record of the mutant, its path, and
#   the stated reason it is equivalent, signed off by this owner. Josh
#   McKerracher owns sign-off until another owner is named here.
fm_mutation_equivalence_signoff_owner() {
  printf '%s\n' 'Josh McKerracher'
}
