# Mutation oracle map

The map in `docs/mutation-oracle-map.tsv` records, for each in-scope Python, TypeScript and JavaScript production path, which executed test is the oracle for that path and what the reviewed result is.
`bin/fm-oracle-map-check.sh` fails when a path has no row, when a row names a path that is not in scope, or when a row is malformed.
Run it with `bin/fm-oracle-map-check.sh`, which exits 0 when the map covers every in-scope path.

## Scope

A path is in scope when it is a tracked file, symlinks included, whose surface class from `bin/fm-mutation-surface-lib.sh` is `python`, `typescript` or `javascript`.
Test code is never in scope: `tests/**` and `*/tests/**` are the oracles themselves, so they need no row.
Every other surface class, such as shell, skill, YAML, JSON, TOML and Markdown, is out of scope for this map.

## Row format

Each row has six TAB-separated columns: `map`, path, surface class, oracle, status and detail.
The oracle column names the oracle test command, candidate test paths for an `unresolved` row, or `-` when no oracle is known.
The detail column is free text with no tabs.
The file's leading `#` lines are comments and are skipped.

## Status

- `executed`: the named oracle was run and its result for this path is recorded in the detail column.
  Only this status counts as measured.
  An executed row must name its oracle, and the checker refuses an executed row with `-` or an empty oracle.
- `missing`: no test in the repository references the path, so there is no oracle.
- `skipping`: an oracle references the path, but it is skipped or gated out in the run that was judged.
- `zero-assertion`: an oracle runs over the path but asserts nothing about its behavior.
- `error`: an oracle fails to run far enough to judge the path.
- `unresolved`: a static reference suggests a candidate oracle, but it was not executed for this path.
  It is not measured.

The checker accepts only these six status values, so a result cannot read as passing through an unknown label.

## What the check proves

The check proves coverage and shape: every in-scope path has exactly one row, no row is stale, duplicated or malformed, and each status is from the owned vocabulary.
It does not re-run the oracles, so the truth of an `executed` row rests on the reviewed evidence that recorded it.
Changing a row's status is a deliberate review decision, not something the check infers.
