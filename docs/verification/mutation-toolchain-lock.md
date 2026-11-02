# Mutation toolchain lock verification record

Audience: maintainer verification.

This record holds the DPES-824 evidence for scratch-only installs of mutmut 3.8.0 and @stryker-mutator/core 10.0.0. It covers sources, compatibility verdicts, lock artifact hashes, the install procedure, result-classification rules, the seeded acceptance set, the npm lock comparison, the cost table, and the residuals.

Python half: installed and exercised in scratch on seeded fixtures. npm half: installed in scratch under option B (overrides pinning the reviewed versions), with the regenerated lock matching all 169 reviewed rows. Stryker command-runner runs on scratch fixtures are reported in the Stryker section, and only what those runs show is claimed there.

## Status

- Status: PARTIAL. Python and npm halves are installed in scratch. Stryker seeded results and cost are recorded in the Stryker section. PARTIAL because AC1 holds on scratch fixtures only and AC3 has no real-project measurements.
- Python half: installed and exercised in scratch on 2026-10-19 (MDT).
- npm half: installed in scratch on 2026-10-19 (MDT) under option B. The regenerated lock (169 unique entries) matches the reviewed inventory (169 rows) on name, version, and integrity. `npm ci --ignore-scripts` ran and the installed set matches the lock.
- Reviewed artifacts: the inventory (`stryker-lock.proposed.tsv`) was not edited. The option A lock is kept in scratch as superseded.
- Stryker 10.0.0: installed as part of the npm set. Seeded command-runner set run on scratch fixtures; results in the Stryker section.
- Round 2 (2026-10-20 to 2026-10-21, one run per item; each section gives its own dates):
  - Advisories: OSV lookup for all 169 npm rows and 19 PyPI pins found no hits, dated 2026-10-21 04:35:17 UTC (see "Advisory lookup, round 2").
  - Seeded cases not exercised in round 1 were run on the repository reader and wrapper, or stated unreachable (seeded set rows 8 to 16). Injected and derived cases are labelled.
  - No pin, lock, or inventory artifact changed.
  - Round-2 brief criteria map to: the advisory section (lookup); seeded rows 8 to 16 (each case run or stated unreachable); the no-change check (no pin, lock, or inventory edited); and the non-green rule in rows 8 to 16. The AC table below keeps its own labels.

| Criterion | Status | Evidence |
|-----------|--------|----------|
| AC1, compatibility proven or declined | mutmut 3.8.0 proven on fixtures on this host. Stryker 10.0.0 command runner exercised on scratch fixtures: the record-based harness verdict matched the expected result in all 8 cases, and Stryker's own status disagreed with that verdict in 4 cases. Stryker's score counts timeouts as detected. Not proven for real projects. | Compatibility verdicts; seeded set; Stryker section |
| AC2, reproducible install from reviewed artifacts | pip: met for the 19 pins. npm: met under option B; the regenerated lock matches all 169 reviewed rows, and the installed set matches the lock. | Python half; npm half |
| AC3, cost data | Partial. Python install, npm lock and install, Stryker dry run and per-case timings on the fixtures, and mutmut fixture timings measured. Per-mutant time on real code, mutation-run wall on real code, and local repository lanes not measured. | Cost measurements; Stryker section |
| AC4, non-green results never counted as killed or green | Proven for timeout, reader-error (missing, malformed, unwritable, cross-check-rejected, exit-1 record with collected or failed below 1, duplicate records, internal-error exit 3, and harness-startup failure in the import, startup-raise, and missing-record-directory forms), and zero-test dry-run abort. Skipped (exit 34) exercised only with an injected exit code. | Seeded set, rows 1 to 16 |

## Environment

- Date: 2026-10-19 (MDT) for round 1. Round 2 ran 2026-10-20 22:46 MDT to 2026-10-21 07:24 MDT on the same host; the advisory lookup was 2026-10-21 04:35:17 UTC.
- Host: macOS 26.7.1, Darwin 25.6.0, arm64.
- Python 3.14.6, Node v26.8.1, npm 11.19.0, each as reported by its own version command.
- `timeout(1)` is not installed on this host. Bounded runs used `fm_run_timed` from `bin/fm-timeout-lib.sh`.
- `.github/workflows/` pins no Node or Python version, so the CI runtime versions are not recorded here.

## Isolation

Scratch root (`<scratch>`) is `data/dpes-824-toolchain-lock/` in the operator's firstmate home. It is outside the repository and not committed.

Every install and mutation-tool command ran under this prefix, inside `fm_run_timed <bound>`:

```text
env -i HOME=<scratch>/home PATH=/opt/homebrew/bin:/usr/bin:/bin:/usr/sbin:/sbin LANG=en_US.UTF-8 TMPDIR=<scratch>/tmp XDG_CACHE_HOME=<scratch>/xdg-cache XDG_CONFIG_HOME=<scratch>/xdg-config XDG_DATA_HOME=<scratch>/xdg-data PIP_CONFIG_FILE=/dev/null npm_config_userconfig=<scratch>/npmrc-empty npm_config_globalconfig=<scratch>/npmrc-global npm_config_cache=<scratch>/npm-cache
```

Watchdog: `fm_run_timed` runs each command in its own process group and sends TERM, then KILL, to the group when the bound is reached.

- Post-hoc validation: a non-exec `bash -c` wrapper around `sleep` with a 3 s bound returned rc 124 after 3 s, with pid equal to pgid and no surviving processes. A 10 s control completed with rc 0. The validation ran after the install steps had already used the watchdog.
- Bounds used: 300 s venv creation and pip install; 60 s freeze; 600 s npm lock; 120 s fixture baselines; 600 s mutmut runs.
- After each logged run, a process check listed processes whose command line names the scratch path, excluding the check's own ancestors. No leftover process was reported.
- Every step's return code, wall time, and nonce is appended to `logs/index.txt` in scratch.

## Python half

### Install

Venv created in scratch at `<scratch>/install-py/venv` (CPython 3.14.6).

Install, run from the worktree root against the reviewed constraints file:

```bash
<scratch>/install-py/venv/bin/pip install --isolated --require-hashes --only-binary=:all: --cache-dir <scratch>/pip-cache -r docs/verification/mutation-toolchain/python-constraints.proposed
```

- Result: rc 0. 19 packages installed, including mutmut 3.8.0, pytest 9.1.1, textual 8.2.8, linkify-it-py 2.2.0, and markdown-it-py 4.2.0.
- Freeze check: `pip freeze` matches the 19 pins after name normalization. Three names differ only in case or separator (pins `pygments`, `pyyaml`, `typing-extensions`; freeze `Pygments`, `PyYAML`, `typing_extensions`). No version differs.
- The installed set is the 19 pins. `--only-binary=:all:` allows no source distributions.

### Compatibility verdicts

- mutmut 3.8.0: proven on this host, CPython 3.14.6 on darwin-arm64, through the seeded set below, including its re-run against the repository reader. Not proven on Linux x86_64 (not run).
- Stryker 10.0.0: exercised on scratch fixtures only (Stryker section). The harness verdict matched the expected result in 8 of 8 cases. Stryker's own status disagreed in 4 cases, so it is never used as the kill signal. Not proven for real projects. Registry engines field is `>=22.0.0`; see the Node floor section.

### Test-command wrapper and reader

mutmut 3.8.0 has no external test-command option. The wrapper is therefore a pytest plugin, `docs/verification/mutation-toolchain/seeded/tooling/dpes824_wrap.py`, loaded through `pytest_add_cli_args = ["-p", "dpes824_wrap"]`.

- The plugin never changes the exit status. Zero-test selection aborts through pytest's own exit 5 during the dry run.
- At session end the plugin writes one JSON run record under `DPES824_RECORD_DIR`, outside the mutated tree. The record holds the mutant key (`MUTANT_UNDER_TEST`), the run nonce, collected and failed counts, the exit status, the process id, and a sentinel string that repeats those values.
- The sentinel lives in the record, not on stdout. mutmut's fork workers send stdout to devnull, so a stdout sentinel never reaches the log.
- The reader, `docs/verification/mutation-toolchain/seeded/tooling/dpes824_reader.py`, is run as `python3 -I dpes824_reader.py <fixture-dir> <record-dir> <nonce> [mutmut-rc]`. It has no explicit exit call, so it exits 0 on normal completion, and it prints a verdict per mutant.
- The repository reader (sha256 `551ba8…`, see the lock artifacts table) validates each record before use. A record is malformed, and ignored, when a required key is missing, when `nonce`, `mutant`, or `sentinel` is not a string, when `collected`, `failed`, `exit`, or `pid` is not an integer, when `collected` or `failed` is negative, or when `pid` is below 1. Malformed records are listed and never used as evidence.
- The reader counts a mutant as killed only when all of these hold: a valid record with the same nonce and mutant key exists; the record exit is 1; collected is at least 1; failed is at least 1; and the sentinel matches the record fields.
- A mutant that exits 0 counts as survived only when a valid record with the same nonce and mutant key exists and its exit is 0. Without one, the result is reader-error (no run record). This check came from a Copilot change on PR 93; the earlier scratch reader (`87e2ab…`) counted such a survivor as survived.
- A killed-without-record result becomes reader-error.
- Open residual: the reader does not require collected of at least 1 for an exit-0 survivor.
- Round 2 (rows 8 to 10) exercised the exit-1 record with collected or failed below 1, and the duplicate-record branch, on derived records. Both give reader-error. Neither is a run that produced the record.

### Seeded fixtures

Five scratch-only fixtures, each under `seeded/fixtures/<case>/`: `kill`, `survive`, `equivalent`, `zero`, and `timeout`. Each has a `calc.py` and a `tests/test_calc.py`. The `pyproject.toml` for the kill case is:

```toml
[tool.mutmut]
pytest_add_cli_args = ["-p", "dpes824_wrap"]
source_paths = ["calc.py"]
pytest_add_cli_args_test_selection = ["tests/"]
use_git_change_detection = false
process_isolation = "fork"
timeout_multiplier = 3.0
timeout_constant = 1.0
```

The other fixtures differ in source code and, for `zero`, in a selection that matches no test (`-k no_such_test_name`). Each fixture's `pyproject.toml` is the authority.

Fixture sources:

- `kill`: `def add(a, b): return a + b`, tested for `add(2, 2) == 4`.
- `survive`: `def label(x): return x * 2`; its test calls `label(3)` with no assertion.
- `equivalent`: `def clamp(x): return 0 if x < 0 else x`, tested for `clamp(-3) == 0` and `clamp(5) == 5`.
- `zero`: uses `add`, with the test `add(2, 2) == 4`; the selection adds `-k no_such_test_name`, which matches no test.
- `timeout`: `def spin(n): i = 0; while i < n: i += 1; return i`; its test asserts `spin(3) == 3`.

### Seeded acceptance set

This table is the re-run against the repository reader and wrapper: `dpes824_reader.py` (`551ba8…`) and `dpes824_wrap.py` (`d21767…`), both byte-identical to origin/main `fe12229` (see the lock artifacts table). The run is `run/main551_seeded.sh` in scratch. Each mutmut run had a 600 s watchdog, each reader run a 120 s watchdog, and every record directory was scratch-only. A leftover check ran after each mutmut run and after each reader pair; none found a leftover process (`logs/main551-run.txt`). Mutant counts are per fixture. The row 7 variants are derived from this run's row 1 kill record.

| # | Case | Fixture run | Mutants and verdicts | Expected | Met |
|---|------|-------------|----------------------|----------|-----|
| 1 | Known kill | `kill` | 1 mutant: killed (exit 1, record matched) | killed | Yes |
| 2 | Known survive | `survive` | 2 mutants: both survived (exit 0) | survived | Yes |
| 3 | Equivalent mutant | `equivalent` | 5 mutants: 3 killed (`_1` to `_3`, not equivalent); 2 survived (`_4` equivalent for all inputs, `_5` equivalent over integers) | `_4` and `_5` survived, never killed | Yes |
| 4 | Test harness unavailable | `kill` | 1 mutant generated, not run: the wrapper could not be imported at startup, so the run ended with rc 1 before the dry run. reader-error. | reader-error, never killed | Yes, for the import-failure form only (see residuals) |
| 5 | Selection matches no tests | `zero` | dry run aborted (stats collected 0, exit 5); 1 mutant not counted | aborted, nothing counted | Yes |
| 6 | Mutant exceeds its bound | `timeout` | 6 mutants: 3 killed (`_1`, `_3`, `_6`); 1 survived (`_2`); 2 timeout (`_4`, `_5`, exit -24) | timeout, never killed for the two long mutants | Yes |
| 7 | Missing or malformed run record | `kill` record variants | v1 record missing: reader-error. v2 truncated JSON: reader-error. v3 sentinel disagrees with record: reader-error. v4 record exit 0 while mutmut exit 1: reader-error. Unwritable record directory: reader-error. | reader-error, never killed | Yes |
| 8 | Exit-1 record with `collected` 0 (round 2, derived) | `kill`; records copied from the round-2 kill run, `collected` set to 0, sentinel rewritten | 1 mutant: `reader-error (record does not show a kill)`, mutmut exit 1 | never killed | Yes, derived |
| 9 | Exit-1 record with `failed` 0 (round 2, derived) | `kill`; as row 8, with `failed` set to 0 | 1 mutant: `reader-error (record does not show a kill)`, mutmut exit 1 | never killed | Yes, derived |
| 10 | Duplicate run records (round 2, derived) | `kill`; as row 8, with a second identical record file for the mutant | 1 mutant: `reader-error (duplicate run records)`; the mutant is listed under `mapping_duplicate_records` | never killed | Yes, derived |
| 11 | Clean control, no mutant (round 2) | `clean`: the fixture test run unmutated through pytest with the wrapper, under the kill run's nonce, in its own record directory | Control record: collected 1, failed 0, exit 0; `control_clean [1]`. Reader on this record alone (kill fixture): `reader-error (no run record)` for `calc.x_add__mutmut_1`, which mutmut lists as exit 1 with no record. | no mutant killed from a clean run | Yes |
| 12 | Skipped, exit 34 (round 2, injected) | `skip34`: kill fixture with a `conftest.py` that sets the session exit to 34 in mutant runs only | mutmut records exit 34 for `calc.x_add__mutmut_1`; reader: `skipped`, `mutmut_exit=34` | skipped, never killed or green | Yes, injected (residual 4) |
| 13 | Internal test-runner error, exit 3 (round 2, injected) | `exit3`: as row 12, with exit 3 in mutant runs only | mutmut records exit 3; reader: `reader-error (internal test-runner error)`, `mutmut_exit=3` | reader-error, never killed | Yes, injected (residual 4) |
| 14 | Harness startup failure, second form: plugin loads, then raises in `pytest_configure` before the session starts (round 2, planted) | `raise-wrap`: a planted `dpes824_wrap.py` that raises `RuntimeError`, shadowing the repository wrapper through `PYTHONPATH`; mutmut traceback as `INTERNALERROR` | mutmut rc 1; no dry run, no stats record, no mutant record; reader: `reader-error (run ended rc 1 before dry run; no stats record)` | reader-error, never killed | Yes, planted (residual 3) |
| 15 | Harness startup failure, second form: record directory missing (round 2) | `nodir`: repository wrapper with `DPES824_RECORD_DIR` pointing at a directory that does not exist (checked absent after the run) | mutmut rc 0 with the mutant listed as exit 1; no record written; reader: `reader-error (no run record)`, mutant under `mapping_killed_without_record` | reader-error, never killed | Yes |
| 16 | Kill record with the clean control in the same directory (round 2) | `clean-plus-kill`: the kill run's records plus the clean control record, one nonce | `calc.x_add__mutmut_1` killed; `control_clean [1]`; no mapping arrays non-empty | killed only from the matching kill record; the control does not change the verdict | Yes |

Rows 8 to 16 ran one case at a time, each mutmut run under a 600 s watchdog and each reader or pytest run under a 120 s watchdog, with a process check after every run; the checks found no stray mutation process. Row 11's first attempt ran without the fixture on `PYTHONPATH`, so the test failed to import `calc` (rc 2); it was re-run, and only the re-run is reported. Rows 12 and 13's first attempt applied the injected exit to mutmut's stats dry run too, so mutmut reported a failed stats run and the reader gave `reader-error (mutant generated, not run)`. The conftest now sets the exit only when `__mutmut_` is in `MUTANT_UNDER_TEST`; only the corrected runs are reported, and the first attempt's records are kept in scratch. The reader's dry-run check reports no stats record in every round-2 case, as in round 1 (residual 6), and it changed no verdict. Scripts are in scratch: `run/r2_cases.sh`, `run/r2_fix.sh`, `run/r2_derive.py`, `run/r2_clean_only.sh`.

On `551ba8…`, the reader results record `control_forced_fail_ok: true` for the kill, survive, equivalent, and timeout fixtures. The zero fixture has no control entry in its output, because its dry run aborted with exit 5. The variant cases have no control entry, because they are derived records and not runs. The unwritable case has none, because its record directory accepted no writes. No clean control (a run with no mutant) was captured in round 1; round 2 captured one (row 11).

Change against the earlier run, which used reader `87e2ab…`:

- Rows 1 to 7, the four variants, and the unwritable case: no verdict changed. The verdict lines and the JSON output of the two readers are byte-identical on every one of these record directories. The verdict strings also equal the earlier run's strings for the same cases.
- Probe P1 (the survivor's `calc.x_label__mutmut_1` run record removed, `probes/p1-norecord`): the verdict changed. On the same record, `87e2ab…` gives `survived`, and `551ba8…` gives `reader-error (no run record)`. This is the intended effect of the Copilot change: an exit-0 survivor now needs its own valid, matching record.
- Probe P2 (the kill record with `collected` stored as a string, `probes/p2-typed`): `551ba8…` gives `reader-error (no run record)`, and the record is listed under `records_malformed`, so the label is misleading: the record exists and was rejected by the type check. `87e2ab…` crashes with a `TypeError` (rc 1) and gives no verdict. Neither reader counts this mutant as killed.
- Round 2 exercised the exit-1 record with `collected` or `failed` below 1, and the duplicate-record branch, on `551ba8…` (rows 8 to 10, derived records). Both reject the record: neither counts as killed.

### Run-record-to-mutant mapping

The record directory must map to mutmut's keys with no duplicates, no unmapped records, no killed-without-record, and no timeout-with-record.

- For the five clean runs (kill, survive, equivalent, zero, timeout), all four arrays were empty.
- For the malformed variants, the killed-without-record array was non-empty by design, because the record was removed or invalid. For v3 and v4, the mapping arrays were empty; the verdict came from the sentinel and exit checks.
- On `551ba8…`, the unwritable case also lists its mutant under killed-without-record. Probe P1 has empty mapping arrays, because a mutant that exits 0 is outside the killed-without-record check; its verdict comes from the exit-0 record check. Probe P2 lists its mutant under killed-without-record and its record under `records_malformed`.
- Round 2 (rows 8 to 16): the derived duplicate (row 10) lists its mutant under `mapping_duplicate_records`. The no-record cases (row 11 reader run on the clean record alone, row 15) list `calc.x_add__mutmut_1` under `mapping_killed_without_record`, because mutmut lists that mutant as exit 1. The skip (row 12), internal-error (row 13), and clean-plus-kill (row 16) reader runs have empty mapping arrays. Row 14 has no records, so it has no mapping. Rows 8 and 9 have empty mapping arrays and the reader verdict comes from the record check.

### Fixture cost data

Each fixture's mutmut run was measured with `/usr/bin/time -l` on 2026-10-19, macOS 26.7.1 arm64. MB here is 10^6 bytes. The kill row is from the last kill rerun, because the detailed log was overwritten.

| Fixture | Mutants generated | Wall (real) | User CPU | Sys CPU | Max RSS | Peak footprint |
|---------|-------------------|-------------|----------|---------|---------|----------------|
| kill | 1 | 0.83 s | 0.53 s | 0.14 s | 75.2 MB | 60.7 MB |
| survive | 2 | 0.76 s | 0.46 s | 0.12 s | 75.1 MB | 60.5 MB |
| equivalent | 5 | 0.79 s | 0.55 s | 0.16 s | 75.4 MB | 60.8 MB |
| zero | 1 | 0.61 s | 0.42 s | 0.10 s | 74.0 MB | 59.5 MB |
| timeout | 6 | 3.85 s | 6.52 s | 0.20 s | 75.6 MB | 61.0 MB |

Each run goes through `run/mutmut_case.sh`, from the fixture directory, under the isolation prefix and the watchdog, with `/usr/bin/time -l` around `<scratch>/install-py/venv/bin/mutmut run`.

Baseline fixture tests (unmutated, venv pytest): 0.35 s, 0.32 s, 0.34 s, 0.34 s, and 0.31 s real for kill, survive, equivalent, zero, and timeout. Each had about 42 MB max RSS. The exact argv was not logged.

Per-mutant time: not measured separately, because mutmut runs every mutant of a fixture in one process. Per-fixture wall times for the re-run are in `logs/index.txt` (`main551-mm-*` lines).

### Python install cost

Date 2026-10-19, macOS 26.7.1 arm64. Cold scratch caches (new scratch caches created at install time).

| Step | Command shape | Wall (real) | User CPU | Sys CPU | Max RSS | Peak footprint |
|------|---------------|-------------|----------|---------|---------|----------------|
| venv creation | create venv in scratch | 4.18 s | 1.51 s | 0.41 s | 95.9 MB | 13.2 MB |
| pip install (19 pins, 28 hashes) | command in Install | 14.26 s | 2.75 s | 1.15 s | 142.5 MB | 122.6 MB |
| pip freeze | `pip freeze` in the venv | 0.58 s | 0.17 s | 0.06 s | 45.5 MB | 33.4 MB |

## npm half (option B, installed)

### Lock generation

The scratch `package.json` in `<scratch>/install-npm/` declares `@stryker-mutator/core` at exact `10.0.0`. Its `overrides` entry pins 14 packages to their reviewed inventory versions and keeps `qs` at 6.16.0: the 12 `@inquirer` packages in the table below at their reviewed versions, `fast-uri` 3.1.8, and `get-intrinsic` 1.3.1. The pinned versions were read from the reviewed TSV by script (`run/make_overrides_b.py`, scratch), not typed by hand.

Command shape, under the isolation prefix with the 600 s watchdog:

```bash
npm install --package-lock-only --ignore-scripts
```

- Option B result: rc 0, 9.7 s wall, 201.4 MB max RSS, 165.5 MB peak memory footprint, no leftover processes. Output: `package-lock.json` in scratch (not committed), sha256 `f26cec9ccb40e622cf88b4f23681f66ef74f2c829f2311523d3dcdeeb20db3e1`, 169 unique (name, version, integrity) entries.
- Option A result, superseded and not installed: no overrides beyond `qs`; rc 0, 8.93 s wall, 262.0 MB max RSS, 227.0 MB peak footprint; sha256 `a10d396adfff1ddd9f4e686b415a7587624c047ccdc1161b2c44bbfb64049f93`, 166 unique entries. Kept as scratch `package-lock.A.json`.
- `--package-lock-only` installs nothing, and `--ignore-scripts` runs no install scripts. The step logs hold npm's output and resource lines, not argv, and have no audit summary line. Whether npm's default audit request ran is not established.

### Lock comparison against the reviewed inventory

Option B result: 169 of 169 lock entries match reviewed rows on (name, version, integrity). Zero rows are only in the lock and zero only in the TSV. The `qs` 6.16.0 row matches. Script output: `RESULT MATCH` (`run/compare_lock.py`, read-only).

Option A comparison, superseded and kept for history. The reviewed inventory is `docs/verification/mutation-toolchain/stryker-lock.proposed.tsv` (169 rows). Comparison key: name, version, integrity.

- Matched: 152.
- TSV rows not in the lock: 17 (14 changed versions and 3 absent rows).
- Lock entries not in the TSV: 14 (12 `@inquirer` packages plus `fast-uri` and `get-intrinsic`).
- Every lock-only name has a TSV row. The `qs` 6.16.0 override matched on (name, version, integrity).

| Package | Reviewed (TSV) | Generated lock | Change |
|---------|----------------|----------------|--------|
| @inquirer/checkbox | 5.2.6 | 5.2.7 | patch |
| @inquirer/confirm | 6.3.3 | 6.3.4 | patch |
| @inquirer/core | 12.0.4 | 12.0.5 | patch |
| @inquirer/editor | 5.3.4 | 5.3.5 | patch |
| @inquirer/expand | 5.1.6 | 5.1.7 | patch |
| @inquirer/input | 5.1.7 | 5.1.8 | patch |
| @inquirer/number | 4.2.4 | 4.2.5 | patch |
| @inquirer/password | 5.2.3 | 5.2.4 | patch |
| @inquirer/prompts | 8.7.3 | 8.7.4 | patch |
| @inquirer/rawlist | 5.3.6 | 5.3.7 | patch |
| @inquirer/search | 4.3.4 | 4.3.5 | patch |
| @inquirer/select | 5.2.6 | 5.2.7 | patch |
| fast-uri | 3.1.8 | 3.1.9 | patch |
| get-intrinsic | 1.3.1 | 1.3.0 | downgrade (minor) |
| async-function | 1.0.0 | absent | removed |
| async-generator-function | 1.0.0 | absent | removed |
| generator-function | 2.0.1 | absent | removed |

### Registry review of the changed and absent versions

Registry metadata only, read on 2026-10-19 (UTC times below). Full per-version review is kept outside the repository.

- `@inquirer` (12 packages): reviewed versions published 2026-10-04 17:41:40 to 17:42:07 UTC. Generated versions published 2026-10-19 21:42:34 to 21:43:04 UTC. Both sets carry GitHub Actions provenance. `@inquirer/prompts` 8.7.4 declares the ten new caret ranges that the lock contains. `@stryker-mutator/core` 10.0.0 declares `@inquirer/prompts` `^8.0.0`, which 8.7.4 satisfies. License MIT, engines unchanged, no install script, no deprecation.
- `fast-uri` 3.1.8 to 3.1.9: 3.1.8 published 2026-09-15 by a maintainer account with no provenance. 3.1.9 published 2026-10-19 23:05:26 UTC with GitHub Actions provenance. Both satisfy `ajv`'s `^3.0.1`. BSD-3-Clause, no install script, no deprecation.
- `get-intrinsic` 1.3.1 to 1.3.0: 1.3.1 published 2025-09-29 23:20:07 UTC; 1.3.0 published 2025-02-22 20:54:20 UTC. Neither has provenance. Both satisfy every consumer range (`call-bound` `^1.3.0`; `side-channel-map` and `side-channel-weakmap` `^1.2.5`). 1.3.1 declares `async-function` `^1.0.0`, `async-generator-function` `^1.0.0`, and `generator-function` `^2.0.0`; 1.3.0 declares none of them. MIT, no install script, no deprecation.
- Absent rows: `async-function` 1.0.0 (2025-01-23 UTC), `async-generator-function` 1.0.0 (2025-02-14 UTC), `generator-function` 2.0.1 (2025-09-30 UTC). All published by the same maintainer as `get-intrinsic`, MIT, no install script, no deprecation.
- Integrity: each reviewed and generated (name, version, integrity) triple matches the registry `dist.integrity` for its version.
- Advisories: not checked in round 1. `npm audit` was not run, because it sends the package list to the audit endpoint, which is outside the registry-metadata-only limit for this step. Round 2 checked all 169 entries through OSV (see "Advisory lookup, round 2"): none found. `npm audit` was still not run.

### Why npm chose get-intrinsic 1.3.0

npm's resolver takes the registry's `dist-tags.latest` when it satisfies the range, is not deprecated, passes the engine check, and is not restricted (`npm-pick-manifest/lib/index.js`, lines 118 to 132). The registry's `latest` tag for `get-intrinsic` is 1.3.0, so npm selected it.

1.3.1 is not unpublished, not deprecated, and carries the integrity recorded in the inventory. The packument's last change (2025-09-30 16:34 UTC) is about 17 hours after 1.3.1 was published, which fits a `latest` tag move. The packument does not keep tag history, so the tag move is an inference, not a confirmed fact. The reviewed inventory does not record why 1.3.1 was chosen.

### Decision (option B, applied in scratch)

Option B was chosen by the approver and supersedes the earlier option A choice. It was applied in scratch only: overrides pin the 14 packages, the lock was regenerated and compared (169 of 169 match), and `npm ci --ignore-scripts` ran.

- Option (a), accept the generated lock with `get-intrinsic` 1.3.0 and 166 entries, was not applied.
- Option (b), keep the inventory's 1.3.1 by override and regenerate, was applied. It yields 169 entries, including the three helper packages (`async-function`, `async-generator-function`, `generator-function`), which 1.3.1 declares. The helpers were reviewed at registry-metadata level in the registry review above.

The reviewed inventory was not replaced. It matches the installed lock exactly, so the approval procedure's comparison passed without a new inventory.

npm's registry-latest choice does not affect the installed tree now, because the override fixes the versions. The generator still depends on the registry state at generation time, so any future regeneration must repeat the comparison.

Recency, for the decision: all 12 new `@inquirer` versions were published within 30 seconds on 2026-10-19, and `fast-uri` 3.1.9 about 80 minutes later. A minimum release-age rule would exclude them. That is a policy choice for the approver.

### npm install

`npm ci --ignore-scripts` ran in scratch under option B: rc 0, 13.0 s wall, 208.7 MB max RSS, 175.2 MB peak memory footprint, no leftover processes, and scratch-only writes (`install-npm/node_modules`, scratch home, scratch npm cache). The lock digest was the same before and after (`f26cec9c…`). The runner recorded the worktree as clean at HEAD `0187241` before and after this step.

Installed-set check, read-only over `install-npm/node_modules`: 169 installed packages against 169 lock entries, 0 missing, 0 extra, 0 version mismatches. The `@stryker-mutator/core` package is 10.0.0. No lock entry has an install script, and none is deprecated.

The reviewed inventory was not edited. The `qs` 6.16.0 override is in `package.json`, and its row in the TSV is unchanged from the merged record.

## Stryker command-runner seeded set

Scratch fixtures under `data/dpes-824-toolchain-lock/fixtures/stryker/`, run against `@stryker-mutator/core` 10.0.0 from the option B install (`install-npm/node_modules`). Runner: `run/stryker_cases.sh`. Each Stryker run went through the isolation prefix, a 300 s process-group watchdog, and a leftover check. The harness verdict comes from a record-based scratch reader, `fixtures/stryker/tooling/stryker_reader.cjs` (sha256 `240ad39a1c878da2a7bb42b00999f907d17cbb2730b0d114afa8c572fe88f70c`). It is a scratch reader for this check, not the repository reader. Stryker's own per-mutant status is kept beside the harness verdict and is never used alone.

The fixtures inject their faults through the environment. A missing executable, a missing or malformed record, a timeout, and a zero-test dry run are constructed cases, not failures of a real tool. The equivalent mutant (`n + 0` to `n - 0`) is equivalent by construction.

| Case | Expected | Harness verdict | Stryker status | Agreement |
|------|----------|-----------------|----------------|-----------|
| kill | both mutants killed | killed x2 | Killed x2 | match |
| survive | one survivor, the rest killed | killed x4, survived x1 (EqualityOperator) | Killed x4, Survived x1 | match |
| equivalent (`n + 0` to `n - 0`) | not killed | survived x1 (ArithmeticOperator); the BlockStatement mutant killed | Survived x1, Killed x1 | match |
| missing executable (injected) | reader-error, not killed | reader-error x2 | Killed x2 | harness correct; Stryker status would count killed |
| zero tests | dry run aborts, nothing counted | dry run aborted (zero tests collected); not counted x2 | Survived x2 | harness correct; only the record catches this |
| timeout | not killed | timeout x2, not killed | Timeout x2 | harness correct; Stryker's score counts Timeout as detected (`totalDetected` = timeout + killed) |
| no run record | reader-error, not killed | reader-error x2 | Killed x2 | harness correct; Stryker status would count killed |
| malformed run record | reader-error, not killed | reader-error x2 | Killed x2 | harness correct; Stryker status would count killed |

Results:

- Harness verdicts match the expected result in all 8 cases.
- Stryker's own per-mutant status disagrees with the harness verdict in 4 cases: missing executable, no run record, malformed run record (Stryker Killed, harness reader-error), and zero tests (Stryker Survived, harness not counted). A Stryker-only classification of these would be wrong.
- Timeouts are not killed by the harness. Stryker's score counts them as detected, so the two scores differ on a timeout.
- Stryker's dry run passed in the zero-test case and marked both mutants Survived. The run record's dry-run check was the only guard against that.
- Each case left no Stryker or node process. The leftover check after the last case still listed the runner's own `tee` pipe (pid 80805, parent 80796), which was still open when the check ran. It is not a stray Stryker process.

Cost (scratch, this host; the wall times are from the run's step index, the memory figures from `/usr/bin/time -l` in each case log):

| Step | Wall | Max RSS | Peak memory footprint |
|------|------|---------|-----------------------|
| `npm ci --ignore-scripts` (option B) | 13 s | 208.7 MB | 175.2 MB |
| Stryker dry run (kill fixture, 2 mutants) | 7.03 s | 146.1 MB | 102.1 MB |
| kill, 2 mutants | 1.08 s | 163.2 MB | 126.5 MB |
| survive, 5 mutants | 1.13 s | 163.2 MB | 126.6 MB |
| equivalent, 2 mutants | 1.08 s | 163.0 MB | 126.1 MB |
| timeout, 2 mutants | 5.29 s | 163.1 MB | 126.6 MB |
| zero tests, 2 mutants | 1.00 s | 163.2 MB | 126.3 MB |
| missing executable, 2 mutants | 1.55 s | 164.0 MB | 127.4 MB |
| no run record, 2 mutants | 0.96 s | 164.0 MB | 126.4 MB |
| malformed run record, 2 mutants | 1.05 s | 164.2 MB | 127.6 MB |

Not measured: per-mutant time on a real project, mutation-run wall time on a real project, CPU on a real project, and mutants generated on a real project. The fixture mutant counts are 2 and 5, so they say nothing about a real project.

Logs: `data/dpes-824-toolchain-lock/logs/stryker-cases-run.out`, `logs/stryker-<case>.log`, `logs/stryker-<case>-verdict.txt`, and `logs/index.txt`. Step digests before and after: `install-npm/package.json` `d1ae726b…`; `install-npm/package-lock.json` `f26cec9c…`; both unchanged by the Stryker runs.

## Lock artifacts

| Artifact | sha256 | Contents |
|----------|--------|----------|
| `docs/verification/mutation-toolchain/python-constraints.proposed` | `6203bd220bf0fd298ab8a79a78ff429156156c301fc7b7fc4d38c66eb25181a2` | 19 pins, 28 `--hash=` lines |
| `docs/verification/mutation-toolchain/stryker-lock.proposed.tsv` | `159efa86f7d57f45776ed7f1c5ecf0ec8856805561c57bfeff10cee728c00113` | 169 npm rows |
| `<scratch>/install-npm/package.json` (option B overrides, not committed) | `d1ae726b802cf19698ee6b947ef24fbdc8c7e928ba6efb44d70a8b597e6d434d` | `@stryker-mutator/core` 10.0.0; 14 override pins; `qs` 6.16.0 |
| `<scratch>/install-npm/package-lock.json` (option B, generated, not committed) | `f26cec9ccb40e622cf88b4f23681f66ef74f2c829f2311523d3dcdeeb20db3e1` | 169 unique entries, matched to the reviewed inventory |
| `<scratch>/install-npm/package.A.json` (option A, superseded) | `88f74ef527cbb192e74400b1ce190c3daf5b3069a8aee27b4a7d29b205053f99` | `qs` override only |
| `<scratch>/install-npm/package-lock.A.json` (option A, superseded) | `a10d396adfff1ddd9f4e686b415a7587624c047ccdc1161b2c44bbfb64049f93` | 166 unique entries |
| `docs/verification/mutation-toolchain/seeded/tooling/dpes824_reader.py` (repository Python reader) | `551ba8ecda39928570a0a2fc78bbec1a8902c5251719739039667b70f3786fc4` | byte-identical to origin/main `fe12229`; used by the re-run in the seeded set table |
| `docs/verification/mutation-toolchain/seeded/tooling/dpes824_wrap.py` (repository pytest wrapper) | `d21767059c61345570dcc3420fe7f5148be8231b00327cf3a9f36ce7dbd4299f` | byte-identical to origin/main `fe12229`; the wrapper never changes exit status |
| `<scratch>/fixtures/tooling/dpes824_reader.py` (earlier scratch reader) | `87e2ab8014f11d0de9f9add85a6345c6e049047dd49e4995a5ba5eaf210f8676` | the reader of the first seeded run; run alongside the repository reader in the re-run as a comparison |
| `<scratch>/fixtures/stryker/tooling/stryker_reader.cjs` (Stryker harness reader, scratch only) | `240ad39a1c878da2a7bb42b00999f907d17cbb2730b0d114afa8c572fe88f70c` | the Stryker seeded set's reader; does not read the Python reader or wrapper |

Scratch generators (not committed): `emit_locks.mjs` `b4569a31a9731c73194150f3a70962a0cf1628b21d58026b3ab491a6424e29ec`; `resolve_pypi.py` `8ef0d944f5369d06f0ec3e1a85cbc690b344da971d37b929775758b9984a8205`; `resolve_npm.mjs` `d02e0cb39f4557f8c078d2d20c926124fa3babf939b3708df742f30833a88391`; `npm_gap_check.mjs` `6d95386fbf1dda5333e574e60d65c9ab8cc6e2cc73a999dde810153418d6bc45`; `delta.py` `dcf327ecfed2ea34bfacc084c6eeda50a023ab60d520e927991bc4f4a027becc`.

- The Python constraints file supersedes an earlier version with 18 packages and 27 hashes. That version omitted `linkify-it-py` 2.2.0.
- Re-run scripts (scratch, not committed): `make_variants.py` `ed83726f144a855c5ae2aaa50889024504ac111f73578f66c6e91fb0db6be714` builds the malformed variants and probes from one run's records; `main551_seeded.sh` `b5792db3a6a6b8db5199d15034a307753c6090bf00da2dabfecd2bc470671e75` runs the seeded set against the repository reader, with the isolation prefix, watchdogs, and leftover checks.
- Index: `--index-url https://pypi.org/simple`, `--only-binary :all:`, `--require-hashes`.
- Each TSV row carries registry integrity, tarball URL, engines, license, attestation presence, install-script flag, os and cpu filters, deprecation flag, needed-by, and targets.

## Node floor

- 37 TSV rows declare `^22.18.0 || >=24.11.0`. The effective floor is 22.18.0 on the 22 line or 24.11.0 on the 24 line. Node 22.0 through 22.17, 23.x, and 24.0 through 24.10 do not satisfy those rows.
- 42 rows declare no engines field.
- `@stryker-mutator/core` 10.0.0 declares `>=22.0.0`.
- Host Node v26.8.1 satisfies the declared engines. CI Node is not pinned in `.github/workflows/` and was not checked.

## qs advisory decision

Carried from the merged record (PR 88), unchanged.

- `typed-rest-client` 2.3.1, which `@stryker-mutator/core` needs, pins `qs` to exactly 6.15.1.
- Three OSV advisories apply to `qs` 6.15.1.

| Advisory | Introduced | Fixed in | Cleared by `qs` 6.15.2 |
|----------|------------|----------|------------------------|
| GHSA-4mjr-xmp4-gh2g | 2.2.5 | 6.16.0 | No |
| GHSA-q8mj-m7cp-5q26 | 6.11.1 | 6.15.2 | Yes |
| GHSA-x5fp-wj9c-mxmx | 6.14.2 | 6.16.0 | No |

- Option (b) is selected in the merged record: an npm `overrides` entry pins `qs` to 6.16.0, which clears all three advisories.
- The `qs` 6.16.0 release is BSD-3-Clause, has no install script, and depends only on `side-channel` `^1.1.1` and `es-define-property` `^1.0.1`, both already in the closure.
- Selecting option (b) does not approve an install.

## Advisory lookup, round 2

- Date: 2026-10-21 04:35:17 UTC.
- Source: OSV, `POST https://api.osv.dev/v1/querybatch`. One call, 188 queries, HTTP 200, no pagination token.
- Queries: the 169 rows of `docs/verification/mutation-toolchain/stryker-lock.proposed.tsv` (ecosystem npm) and the 19 pins of `docs/verification/mutation-toolchain/python-constraints.proposed` (ecosystem PyPI), each with its exact version. The query list equals the reviewed TSV and pins, set for set.
- Result, npm (169 rows): none found. No row has an advisory, so none is affected at its pinned version. This includes `qs` 6.16.0.
- Result, PyPI (19 pins): none found. None is affected at its pinned version.
- No `GET /v1/vulns/<id>` call was made, because the batch had no hits.
- Control: the same endpoint and request shape, with one batch of three known-vulnerable pins (`qs` 6.15.1, `lodash` 4.17.20, `jinja2` 2.4.1), returned hits. `qs` 6.15.1 gave the same three advisories as the qs section above (GHSA-4mjr-xmp4-gh2g, GHSA-q8mj-m7cp-5q26, GHSA-x5fp-wj9c-mxmx). `lodash` 4.17.20 gave five advisories and `jinja2` 2.4.1 gave eighteen. The empty main result therefore comes from a lookup that returns hits when they exist.
- Not run: `npm audit` and `pip-audit`. They send the package list to an audit endpoint, which this step excludes. No package manager was run.
- No pin, lock, or inventory artifact was changed. No advisory affects a pinned package, so there is no finding for a decision.
- Caveat: an advisory database can be incomplete. Only OSV was queried. None found is not a proof of safety.

Raw files, kept in the firstmate home outside the repository under `data/dpes-824-residuals/osv/`:

| File | sha256 |
|------|--------|
| `querybatch.json` (request, 188 queries) | `0d54ccff14aef7024144cd9e4d8665b36ae89407fc822826ddbfda09d68a51cb` |
| `querybatch-response.json` (188 results, all empty) | `4efe6b9cb33995f0632e7c419c9a8e7b676fd0d059ee65935d5e45aa5149ee5b` |
| `control/control-querybatch.json` | `ee840713d26b5a03b91f6f85797ce24082cf56d4d54e3c745727e9728009eb2b` |
| `control/control-querybatch-response.json` | `cc205ee68623b29e08c57b733281bbda0f81a068dc4245c7cbf352a45ec5fb2c` |
| `query-list.tsv` | `ead0458930fa909ad547ae8ad335025dc14763b83aff36e8657e1ccce6fd18c5` |
| `summary.tsv` (188 rows, one per query, `affected_pinned` `no` on each) | `60108704aa5498a08af4246832e14113e800e87a85089b0469b22db1328c90e2` |
| `endpoints.txt` | `1d0de24b7e1bda1c5fb702dd8391df941919779ffe91e8d91c888d4c84786ca6` |
| `query-utc.txt` | `f159aa376e205d96b484cf163c3dc4934ed12c619a30c79f1a27230b8bb4458b` |

## Result classification

### mutmut 3.8.0

From `stats.py` and `runners/harness.py` at tag 3.8.0, as read:

- exit 1: killed. exit 0: survived.
- exit 5 or 33: no tests. exit 34: skipped.
- exit 36, -24, 24, 152: timeout. exit 255: timeout in the source, but the source has no comment, so unverified.
- exit -9 or -11: segfault.
- exit 3 (internal error): counted killed by `stats.py`. The reader overrides this to reader-error.
- exit 4 (usage): the harness raises `BadTestExecutionCommandsException`.
- Any other exit code: suspicious.
- mutmut's own killed count is not trusted. The reader's per-mutant verdict is the count.

pytest 9.1.1 exit codes, for reference: 0 OK, 1 tests failed, 2 interrupted, 3 internal error, 4 usage error, 5 no tests collected, 6 max-warnings error.

### Stryker command runner (source reading, checked by the Stryker seeded set)

From Stryker v10.0.0 source (`packages/core/src/test-runner/command-test-runner.ts`, `packages/core/src/process/3-dry-run-executor.ts`, `packages/core/src/mutants/mutant-test-planner.ts`, `packages/core/src/config/options-validator.ts`):

- The command runner sets a mutant's status from the process outcome: exit 0 is Survived; any other non-zero exit is Killed; a spawn error is Error; a timeout is Timeout.
- Measured in the Stryker command-runner seeded set above: the injected missing executable (exit 127) was counted Killed by Stryker for both mutants. The record-based harness reports reader-error for the same runs. The open risk is confirmed for this injected form. Other forms, such as exit 126 or a shell's own not-found path, are not exercised.

mutation-testing-metrics `calculateMetrics` counts timeouts in `totalDetected`, so a timeout raises the reported score (source read; reporting only).

### Outcome table

| Outcome | mutmut signal | Stryker signal | Counted as | Killed? | Green? |
|---------|---------------|----------------|------------|---------|--------|
| Tests fail on mutant | exit 1, with a matching run record | non-zero exit | killed | Yes, only after the reader confirms the record | No |
| Tests pass on mutant | exit 0 | exit 0 | survived | No | No |
| Timeout | exit 36, -24, 24, 152 (255 unverified) | Timeout | timeout | No | No |
| Tool not found or not executable | import or startup failure (exit 1 seen); exit 127 from a missing executable, measured; exit 126 not exercised | non-zero exit, counted Killed (measured for exit 127) | reader-error | No | No |
| Zero tests collected | exit 5 in the dry run (measured); exit 33 not exercised | Survived (measured for the injected zero-test case; Stryker's dry run passed) | dry run aborts; nothing counted | No | No |
| Skipped | exit 34 | not applicable | skipped | No | No |
| Internal test-runner error | exit 3 (overridden by the reader) | not applicable | reader-error | No | No |
| Usage or configuration error | exit 4 | not applicable | abort | No | No |
| Missing, malformed, or unwritable record | any | any | reader-error | No | No |
| Unknown or suspicious exit | any unlisted code | not applicable | reader-error | No | No |
| Stryker spawn error | not applicable | spawn error | Error | No | No |

## Test discovery

- The repository has no root pytest configuration. No workflow, script, or `bin/` command runs pytest.
- The repository's test files are `tests/*.test.py`, which pytest's default patterns (`test_*.py`, `*_test.py`) do not match.
- The seeded fixtures add five `tests/test_calc.py` files under `docs/verification/mutation-toolchain/seeded/fixtures/*/tests/`. Those match the default pattern, so a future root `pytest` run would collect them. Not mitigated in this change. Follow-up.
- Scratch mutmut runs pass an explicit `tests/` selection (`pytest_add_cli_args_test_selection`).

## Cost measurements

### Repository lanes (CI, not local)

Source: main CI run 39056686694 (head `03a12cd`, 2026-10-17, all 18 jobs success). Durations are job start to job completion, read through `gh api` job timestamps. These are CI-runner durations, not measured on this host, and are not comparable to the local runs above. The repository suites were not run locally.

Main's latest run, 39346926687 (head `77127fd7`), is red. Its durations are not used.

| Job | Duration |
|-----|----------|
| Behavior portable serial 5 | 19m46s |
| Behavior portable serial 1 | 19m16s |
| Behavior portable serial 9 | 18m27s |
| Behavior portable serial 7 | 18m24s |
| Behavior portable serial 8 | 18m02s |
| Behavior portable serial 6 | 17m10s |
| Lint 1 | 17m08s |
| Behavior portable serial 4 | 17m06s |
| Behavior portable parallel 2 | 16m37s |
| Behavior portable serial 3 | 15m56s |
| Behavior portable parallel 1 | 14m10s |
| Lint 2 | 14m04s |
| Behavior tests (Herdr) | 11m43s |
| Behavior portable serial 2 | 11m27s |
| Stock macOS Bash snapshot compatibility | 3m38s |
| Test coverage guard | 0m22s |
| Repo invariants | 0m07s |
| Behavior timing aggregate | 0m07s |

### Not measured

- npm `npm ci` cold install: done under option B (13 s wall, 175.2 MB peak footprint). Cold-cache timing for a second run is not measured.
- Stryker per-mutant time, mutation-run wall, CPU, and memory on a real project, and mutants generated on a real project: not measured. Fixture timings are in the Stryker section.
- Per-mutant time for mutmut: not measured separately (see above).
- Local repository lane durations: not measured. The brief asked for CI durations from main instead.
- Linux x86_64 numbers: not run.
- Clean-control baseline argv: not logged.

## Residuals and deviations

1. npm: the installed versions come from overrides pinned to the reviewed inventory, not from a regenerated inventory. The lock is scratch-only and not committed. Advisories for the 169 entries were checked in round 2 through OSV, with none found (see "Advisory lookup, round 2"); that is not a proof of safety. Stryker's command-runner classification from source reading is confirmed by the Stryker seeded set for the injected cases in that set only. Real shell wrappers and real projects are not covered.
1a. Reader revision: the seeded set was first run against reader sha256 `87e2ab8014f11d0de9f9add85a6345c6e049047dd49e4995a5ba5eaf210f8676` (scratch copy under `data/dpes-824-toolchain-lock/fixtures/tooling/`). The repository reader on this branch, `docs/verification/mutation-toolchain/seeded/tooling/dpes824_reader.py`, is `551ba8ecda39928570a0a2fc78bbec1a8902c5251719739039667b70f3786fc4`. The difference is 18 insertions and 2 deletions, added by two Copilot commits (dcf125b, 1e6bdea) on PR 93 after the seeded run. Those commits add schema checks and a reader-error branch for a missing record. The seeded acceptance set was re-run against the repository reader and its wrapper (`d21767…`, byte-identical to origin/main `fe12229`), and the seeded set table reports that re-run. Result: the 11 subcases of rows 1 to 7 (the four malformed variants and the unwritable case included) gave the same verdicts as the earlier run, and both readers produced identical full output on those record directories. Probe P1 changed: `survived` on 87e2ab, `reader-error (no run record)` on 551ba8. Probe P2 changed from a crash on 87e2ab to a reader-error on 551ba8 with a misleading label. The Stryker harness does not use the repository reader or the wrapper, so it needed no re-run.
2. Stryker missing-test-command risk: tested on the scratch fixture. A missing executable gives harness reader-error, while Stryker's own status is Killed. Stryker's status alone must not be used as the kill signal. The fixture uses an injected fault, so this does not cover a real shell wrapper.
3. Harness-unavailable case: round 1 exercised only the import-failure form at startup (rc 1, before the dry run). Round 2 added two more forms: a planted wrapper that raises in `pytest_configure` before the session starts (row 14), and a missing record directory (row 15). Both give reader-error, never killed. Both are constructed, not natural failures of the repository wrapper. The exit 126 or 127 path was not exercised by the mutmut harness. Stryker's injected missing executable (exit 127) was exercised; see residual 2.
4. Skipped (exit 34) was exercised in round 2 only with an injected exit code, set by a fixture `conftest.py` in mutant runs (row 12). mutmut records the code and the reader reports `skipped`. Whether mutmut's own harness produces exit 34 on a real failure was not established. The exit 3 reader override was exercised in round 2 the same way (row 13). Exit 255 remains unverified.
5. The reader does not require collected of at least 1 for an exit-0 survivor.
6. Four of the five clean fixture runs had no stats record; the zero fixture's dry run had one (collected 0, exit 5). Cause not established.
7. Round 1 captured no clean control (no-mutant) record. Round 2 captured one (row 11), with the wrapper and reader. The round-1 baseline argv was not logged.
8. Per-mutant time not measured.
9. Per-run process id, process group, and session were not logged. Only the leftover check after each run was logged, with no leftovers found. Requested per-run evidence is therefore incomplete.
10. The watchdog was validated after the install steps had used it.
11. The step logs hold tool output and resource lines, not argv. The pip and npm command shapes above come from the session transcript. The pip form with `--cache-dir` ran 48 times in that transcript, and the form without it ran 3 times.
12. Snapshot before the first install was not written to disk; the session-start worktree status was used as the before-state.
13. Home-cache timing: entries newer than the install start appeared under `~/.cache/uv`, `~/.cache/codeburn`, `~/.config/twg`, and some `~/Library/Caches` locations (`com.apple.*`, GeoServices). These appeared by timing only; no process audit was run, so they are not attributed to an install step. `~/.npm` and `~/Library/Caches/pip` showed no new entries, and `~/.cache/pip` was absent.
14. Linux x86_64 not run. Node and Python runtime pins for CI are not recorded in `.github/workflows/`.
15. Repository lane durations are CI-runner job durations from main, not local measurements.
16. The npm install step's audit behaviour is not established from its log (see the npm section). `npm audit` was not run as a command.
17. Round 2 injected cases (rows 12 and 13) and the planted wrapper (row 14) force an exit code or a startup failure in a fixture or a shadowing file. They show how the reader treats each code and failure. They do not show that mutmut or the repository wrapper produces the same code on real code.
18. Round 2 derived rows (8 to 10) are copies of one real kill run's records, changed by a script. They test the reader's checks on those record shapes, not a run that produced them.
19. The advisory lookup queries OSV only, by exact name and version, as a point-in-time result on 2026-10-21 UTC. The OSV database can be incomplete, and none found is not proof of safety.

## Sources

- Stryker v10.0.0 source: `packages/core/src/process/3-dry-run-executor.ts`, `packages/core/src/test-runner/command-test-runner.ts`, `packages/core/src/mutants/mutant-test-planner.ts`, `packages/core/src/config/options-validator.ts`.
- mutmut tag 3.8.0: `src/mutmut/stats.py`, `src/mutmut/runners/harness.py`.
- mutation-testing-metrics `calculateMetrics`.
- pytest 9.1.1 `ExitCode`.
- Registry metadata: PyPI JSON for the Python pins; npm packuments for each TSV row.
- OSV advisory records for `qs` (carried from the merged record).
- OSV querybatch API, `https://api.osv.dev/v1/querybatch`, for the round-2 lookup (see "Advisory lookup, round 2").

## Checks run

Run from the worktree root on 2026-10-19 against the tree that contains the re-run results above. Exit statuses were captured from each command.

- `bash bin/fm-lint.sh`: rc 0. Reported no changed lint targets: this change touches Markdown and TSV only, not shell.
- `bash bin/fm-lint-workflows.sh`: rc 0. actionlint 1.7.12. 9 workflow files valid.
- `bash bin/fm-test-run.sh --check-coverage`: rc 0. `FM_TEST_COVERAGE ok total=278 parallel=24 serial=238 serial_shards=9 herdr=16`.
- `bash bin/fm-doc-audience-check.sh`: rc 0. `ok surfaces=141 local_links=799`.
- `bash bin/fm-oracle-map-check.sh`: rc 1. The only finding is `no map row for in-scope path: bin/fm-audit-activity-map.py`. That path is not part of this change and has no row on main either. The seven seeded-fixture and tooling rows added here are not reported, so they pass.

Round 2 (2026-10-21). Each check ran once, in sequence, from the worktree root, under `fm_run_timed` with its own bound. HEAD was `02faaed`, the branch base; `origin/main` is `0d97c31`, ahead of it. The only modified tracked file was this record. Logs stay in scratch.

- `bash bin/fm-lint.sh`: rc 0. ShellCheck 0.11.0 reported no changed lint targets. The run also covered `fm-lint-workflows.sh`: actionlint 1.7.12, 9 workflow files valid.
- `bash bin/fm-doc-audience-check.sh`: rc 1. The only finding is `unclassified: tests/fixtures/panel-assignments/candidate/evidence-output.txt`. That fixture came in with `e1db471` (DPES-781), and this change does not touch it. `docs/verification/birds-eye-export.md` on `origin/main` already records this finding as failing on main. Round 1 recorded rc 0 on 2026-10-19.
- `bash bin/fm-test-run.sh --check-coverage`: rc 1 after 675 s, within the 900 s bound, so not a timeout. The only output is `coverage guard: largest portable serial shard packs 1251198ms above the 1200000ms target`, with the hint to rebalance shards. Round 1 recorded rc 0. This change touches no shard, hint, or test-runner input. The check was not rerun on `origin/main`, so whether main is red for this guard is not verified here.
- `bash bin/fm-oracle-map-check.sh`: rc 1. The only printed finding is `no map row for in-scope path: bin/fm-assignment-reassess.py`, from `3a2563a` (DPES-765). Round 1 printed a different path, `bin/fm-audit-activity-map.py`. Neither path is part of this change.
