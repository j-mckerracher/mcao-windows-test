#!/usr/bin/env bash
# DPES-966 reference differential for watcher-arm command-position analysis.
#
# bin/fm-arm-command-policy.mjs is the reference. For every row of
# tests/fixtures/arm-wrapper-corpus.txt (whitespace-separated literal words, no quoting)
# it computes commandPosition; tests/fixtures/arm-wrapper-position.tsv holds those rows.
# This test fails when the reference no longer matches the table, and then runs the Rust
# port (crates/mcao-core/tests/arm_wrapper_differential.rs) against the same table.
#
# Usage:
#   tests/fm-arm-wrapper-differential.test.sh               verify reference and port
#   tests/fm-arm-wrapper-differential.test.sh --regenerate  rewrite the table from the reference
#
# No row is executed. The corpus is parsed as data only.
set -euo pipefail

ROOT=$(CDPATH='' cd -- "$(dirname -- "${BASH_SOURCE[0]}")/.." && pwd -P)
REFERENCE="$ROOT/bin/fm-arm-command-policy.mjs"
CORPUS="$ROOT/tests/fixtures/arm-wrapper-corpus.txt"
TABLE="$ROOT/tests/fixtures/arm-wrapper-position.tsv"

# Prints input and reference result columns for the corpus and the supplemental rows:
# index, command word, wrappers, prefix assignments, unresolved, payloads.
# Empty lists print as "-".
reference_table() {
  # Paths travel in the environment: a script argument would make the policy think it was run directly.
  FM_ARM_REFERENCE="$REFERENCE" FM_ARM_CORPUS="$CORPUS" node --input-type=module -e '
    import { readFileSync } from "node:fs";
    import { pathToFileURL } from "node:url";
    const policy = await import(pathToFileURL(process.env.FM_ARM_REFERENCE).href);
    const join = (items) => (items.length === 0 ? "-" : items.join(","));
    const supplemental = [
      "xargs --arg-f names.txt -I{} {}",
      "xargs --arg-f=names.txt -I{} {}",
      "xargs --no-run-if-em -I{} {}",
      "xargs --max 2 wc -l",
      "xargs --max-c 10 printf %s",
    ];
    const corpus = readFileSync(process.env.FM_ARM_CORPUS, "utf8").split("\n");
    for (const line of [...corpus, ...supplemental]) {
      const values = line.split(" ").filter(Boolean);
      if (values.length === 0) continue;
      const tokens = values.map((value) => ({ type: "word", value, literal: true, subs: [], quoted: false, unquotedExpansion: false }));
      const position = policy.commandPosition(tokens);
      const command = position.words[position.index];
      const result = [
        position.index,
        command ? command.value : "-",
        join(position.wrappers),
        position.prefixAssignments,
        position.unresolvedWrapperOption ? 1 : 0,
        join(position.wrapperPayloads),
      ].join("\t");
      process.stdout.write(`${line}\t${result}\n`);
    }
  '
}

if [ "${1:-}" = "--regenerate" ]; then
  reference_table > "$TABLE"
  echo "wrote $(wc -l < "$TABLE" | tr -d ' ') rows to ${TABLE#"$ROOT"/}"
  exit 0
fi

if [ "$#" -ne 0 ]; then
  echo "usage: $0 [--regenerate]" >&2
  exit 2
fi

WORK=$(mktemp -d)
trap 'rm -rf "$WORK"' EXIT

reference_table > "$WORK/actual.tsv"
if ! diff -u "$TABLE" "$WORK/actual.tsv" > "$WORK/reference.diff"; then
  echo "FAIL: reference policy no longer matches $(basename "$TABLE")" >&2
  cat "$WORK/reference.diff" >&2
  exit 1
fi
echo "ok - reference policy matches $(wc -l < "$TABLE" | tr -d ' ') differential rows"

if ! command -v cargo >/dev/null 2>&1; then
  echo "FAIL: cargo is required to check the Rust port against the differential table" >&2
  exit 1
fi
(cd "$ROOT" && cargo test --quiet -p mcao-core --test arm_wrapper_differential)
echo "ok - Rust port matches the differential table"
