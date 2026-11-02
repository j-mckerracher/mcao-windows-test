# Host container mutation baseline

This is a report-only baseline of the container mutation run on one operator host at a pinned commit. It records measured counts and timings for a subset of 14 shell scripts. It is not a pass or fail gate: no floor, threshold, or verdict is set by this document.

## Scope and status

- Pinned measured commit: `f17b7069f5e77cf984f6dd7080a969896ad513fc` (`fix(mutation): DPES-860 - Harden runner read, exit mapping and budget (#98)`).
- Document base: `origin/main` at `3a2563a`. Between the pin and this base the five mutation tools and the mutation-verdict workflow are byte-identical (see [Pinned inputs](#pinned-inputs)), so the pinned measurement applies to the tools as they stand on this base.
- Measured: 14 shell scripts (aggregate label `fast-oracle-14`), five sequential runs, each VALID.
- Result: 2126 mutants per run; 1134 killed, 668 survived, 15 timeout, 309 error. All 309 errors are parse failures of the mutant, counted as error by the runner. Every run produced identical records.
- Stability: all 87 measures are stable under the 120 percent rule. No measure was unstable, so no rerun was needed.
- Not a CI artifact. This baseline does not run in CI and is not attached to any PR check.

## What was not measured

- Python, TypeScript, JavaScript, JSON, YAML, Markdown, TOML, and skill files were not measured. Their classifier surfaces were not run.
- 211 of the 225 shell paths the classifier put under the shell surface were not measured. Only the 14 in the inclusion set below were run.
- The host itself was not run as a mutation surface. All measurements are inside the container.
- Native Linux was not tested. The image is `linux/amd64` on an `arm64` host, so the container ran emulated. Timings are emulated timings.
- Cgroup containment of CPU and memory was not checked beyond the container's own limits and the 30-second sampling described below.
- Scripts whose unmutated oracle failed in the image were not measured. Many of these failed because the image lacks `jq`, `tmux`, `shellcheck`, `ruby`, or `tasks-axi`.

## Pinned inputs

| Input | Value |
|---|---|
| Measured commit | `f17b7069f5e77cf984f6dd7080a969896ad513fc` |
| Document base | `origin/main` `3a2563a` |
| Wrapper blob at pin | `bin/fm-mutation-container-run.sh` `d936632fc77d` |
| Runner blob at pin | `bin/fm-mutation-run.sh` `451a6b4c6d20` |
| Classifier blob at pin | `bin/fm-mutation-classify.sh` `445931e2d4c1` |
| Exec library blob at pin | `bin/fm-mutation-exec-lib.sh` `71257dc82a58` |
| Tool diff, pin to `origin/main` | empty for the four files above, `bin/fm-mutation-verdict.sh`, and `.github/workflows/mutation-verdict.yml` |
| `python-constraints.proposed` SHA-256 | `6203bd220bf0fd298ab8a79a78ff429156156c301fc7b7fc4d38c66eb25181a2` (74 lines) |
| `stryker-lock.proposed.tsv` SHA-256 | `159efa86f7d57f45776ed7f1c5ecf0ec8856805561c57bfeff10cee728c00113` (173 lines) |
| `mutation-toolchain-lock.md` SHA-256 | `2380a3f40ead466f4e4640d33a03e1d329e363197fd91566ba8aeb178c2b97f8` (468 lines) |
| Operator-private manifest digest | `25f45ea05809bae596cd2a26b17631f6ff09aec091223650696fc73a53a0ff2d` (306 entries, `MANIFEST.sha256`) |
| Operator-private manifest addendum digest | `29bcf7cfc7fd8fa4eae634cc3eb203ccd38379e514e36b46ccdaad6893cdba53` (4 entries: `container-env.txt` and the three lock files above) |

The manifest, its addendum, and the raw run records are kept in the operator's private data directory and are not committed. The addendum covers the container probe output and the three lock files, which the main manifest does not list. The complete runner record artifact that a CI run would retain comes from a separate pull request, not this one. That artifact has not landed, so no CI run retains a record yet.

## Environment

| Item | Value |
|---|---|
| Host | macOS 26.7.1 (25G241), Apple M3 Pro (`arm64`), 11 logical CPUs, 36 GiB RAM |
| Container runtime | Docker 29.4.0 server on the OrbStack runtime, OrbStack app 2.2.3, `aarch64` Linux VM with 11 CPUs and about 16.8 GB memory, `runc` |
| Image | `node:24-bookworm`, `linux/amd64`, emulated on the `arm64` host, `--pull never` |
| Image ID and repository digest | ID `sha256:03905f546fa8c284c377142124957871d1e8b9ddd0d69e869e233b9eeffd7790`; digest `node@sha256:14078204b6bb268c107ceff6d3c43f45a888105e5c2b18b14c32b3409d93a1b0`; created 2026-10-14. Recorded from the local daemon at the time of the runs. No inspect output is saved in the operator's data directory, and the daemon is unreachable at the time of writing, so these values were not re-verified. |
| In-container tools | `git` 2.39.5, GNU `bash` 5.2.15 (`x86_64` in the emulated container), `python3`, `setsid`, `timeout`. `jq`, `tmux`, and `shellcheck` are absent (tool probe). `ruby` and `tasks-axi` are also absent (pre-check output; see [Disclosures](#disclosures)). |
| Platform warning | Every container run prints a platform-mismatch warning for `linux/amd64` on `linux/arm64/v8`. |

The in-container probe output is kept as `container-env.txt` in the operator's private data directory.

## Commands

The main run is invoked once per run number, against a fresh clone of the pinned commit. The operator's driver script for these runs is private and not committed; it runs the commands below with the same arguments for all five runs. The arguments are the wrapper's own:

```sh
frozen/bin/fm-mutation-container-run.sh --root <clone> \
  --container-timeout 15600 --budget 14400 --timeout 60 \
  --probe-timeout 10 --syntax-timeout 10 \
  < input-classified-14.tsv > record.tsv
```

The wrapper's container limits are unchanged from the pin: `--pull never --network none --cpus 1 --memory 512m --pids-limit 128`.

The seeded-fault control runs through the same wrapper from a separate scratch root, so the fixtures are not classified as test code in the real tree:

```sh
frozen/bin/fm-mutation-container-run.sh --root seeded-control/root \
  --container-timeout 600 --timeout 30 < seeded-control/classified.tsv > seeded.record.tsv
```

The oracle pre-check mirrors the wrapper's limits in a direct container, one unmutated run per oracle, sequentially. The wrapper discards oracle output, so the pre-check is the only place where a skip could be told apart from a clean pass. Its container flags are `--pull never --network none --cpus 1 --memory 512m --pids-limit 128`. Inside the container each oracle is bounded at 300 seconds (`timeout -k 5 300`), and the container run as a whole is bounded by a host timeout of 420 seconds.

The aggregation (`aggregate.sh`, operator-private and not committed) reads the five records, checks that each record is byte-identical to run 1, and writes `long.tsv` and `distribution.tsv`. Both TSVs are committed in `docs/verification/mutation-baseline/`. The method is set out in [Reproduction](#reproduction).

## Subset and inclusion rule

The subset is the set of oracle-bearing shell scripts whose unmutated oracle exits 0 in the container with no skip or tool-absent message, and whose unmutated wall time is at most 12 seconds. The rule was chosen by the operator after the duration gate (see [Duration gate](#duration-gate)).

Exactly 14 scripts meet the rule. Their paths and blob IDs at the pin are:

| Script | Script blob | Oracle | Oracle blob |
|---|---|---|---|
| `bin/fm-capability-schema.sh` | `d4f725846a4c` | `tests/fm-capability-schema.test.sh` | `70a8db1ce4bb` |
| `bin/fm-check-unregister.sh` | `d13fafb2428c` | `tests/fm-check-unregister.test.sh` | `bf30b0c931d7` |
| `bin/fm-ensure-agents-md.sh` | `7b5b4d51d685` | `tests/fm-ensure-agents-md.test.sh` | `b66c2553c51b` |
| `bin/fm-forge-detect.sh` | `c87830f4f255` | `tests/fm-forge-detect.test.sh` | `394fd6bc4636` |
| `bin/fm-mutation-verdict.sh` | `7e01f22272e5` | `tests/fm-mutation-verdict.test.sh` | `ab73213cb545` |
| `bin/fm-operational-input.sh` | `d0ce813cf9b1` | `tests/fm-operational-input.test.sh` | `e2a86e091084` |
| `bin/fm-panel-report.sh` | `6429cfcdfa48` | `tests/fm-panel-report.test.sh` | `fbe741cfca42` |
| `bin/fm-remote-delta-read.sh` | `84aef13d0512` | `tests/fm-remote-delta-read.test.sh` | `49c37fe920ad` |
| `bin/fm-remote-entrypoint.sh` | `59e86ddf9d1e` | `tests/fm-remote-entrypoint.test.sh` | `ccbaa1026014` |
| `bin/fm-resolve-toggles.sh` | `2cfd08b7d0ee` | `tests/fm-resolve-toggles.test.sh` | `cd394559c11f` |
| `bin/fm-secondmate-parent-migrate.sh` | `7c8537a429e7` | `tests/fm-secondmate-parent-migrate.test.sh` | `747d14730d37` |
| `bin/fm-supervision-instructions.sh` | `43dd1b6a0b8d` | `tests/fm-supervision-instructions.test.sh` | `706b180e6a0b` |
| `bin/fm-trace-context-lib.sh` | `ef49f005a2d9` | `tests/fm-trace-context-lib.test.sh` | `faf6debf23d8` |
| `bin/fm-transition-lib.sh` | `b6c49060d525` | `tests/fm-transition-lib.test.sh` | `ff31b65aa1b0` |

## Per-run results

Each run was VALID: the wrapper returned 0, the record had one published summary, no budget rows, no path rows, no malformed rows, no container left under the run's name, and the seeded control found all four named faults killed.

| Run | Start (UTC) | End (UTC) | Wall (s) | Wall (h) | Killed | Survived | Timeout | Error | Seeded named kills | Peak container memory (MiB, sampled) | Container CPU (s, approx.) | Host real (s) | Host user (s) | Host sys (s) | Host max RSS (MiB) |
|---:|---|---|---:|---:|---:|---:|---:|---:|---:|---:|---:|---:|---:|---:|---:|
| 1 | 2026-10-20T06:33:38Z | 2026-10-20T08:38:28Z | 7490 | 2.08 | 1134 | 668 | 15 | 309 | 4/4 | 386.8 | 6992 | 7489.77 | 8.91 | 19.28 | 45.2 |
| 2 | 2026-10-20T08:39:26Z | 2026-10-20T10:44:13Z | 7487 | 2.08 | 1134 | 668 | 15 | 309 | 4/4 | 402.4 | 6961 | 7486.78 | 8.99 | 19.53 | 45.1 |
| 3 | 2026-10-20T10:44:50Z | 2026-10-20T12:51:06Z | 7576 | 2.10 | 1134 | 668 | 15 | 309 | 4/4 | 325.4 | 7078 | 7575.92 | 8.94 | 19.36 | 44.9 |
| 4 | 2026-10-20T12:51:38Z | 2026-10-20T15:14:45Z | 8587 | 2.39 | 1134 | 668 | 15 | 309 | 4/4 | 277.4 | 7587 | 8587.79 | 5.74 | 14.36 | 45.2 |
| 5 | 2026-10-20T15:16:04Z | 2026-10-20T17:37:52Z | 8508 | 2.36 | 1134 | 668 | 15 | 309 | 4/4 | 393.0 | 7982 | 8507.52 | 7.17 | 17.20 | 45.1 |

Counts are identical across all five runs, and the records were compared byte for byte. Timing varies by run; run 4 is the slowest. The cause of the timing differences was not established.

Cost notes:

- Host user and sys times reflect the wrapper and Docker client processes, not the container's own work, so they are small. Container CPU is estimated from `docker stats` samples taken every 30 seconds and is approximate.
- Peak container memory is the highest sampled value, not a continuous peak. The container limit was 512 MiB in every run.
- In run 1 the seeded-fault control took 4.62 seconds of host real time.
- Containers left after each run: 0. The wrapper was not alive after each run.

## Per-script results

The counts below come from run 1's record. Runs 2 to 5 produced the same counts. Parse errors are a subset of error. Oracle wall is the unmutated pre-check wall time, not the mutation run.

| Script | Lines | Oracle wall (s) | Mutants | Killed | Survived | Timeout | Error | Parse errors |
|---|---:|---:|---:|---:|---:|---:|---:|---:|
| `bin/fm-capability-schema.sh` | 203 | 2.3 | 175 | 120 | 43 | 0 | 12 | 12 |
| `bin/fm-check-unregister.sh` | 52 | 3.4 | 42 | 25 | 14 | 0 | 3 | 3 |
| `bin/fm-ensure-agents-md.sh` | 269 | 9.4 | 161 | 67 | 62 | 0 | 32 | 32 |
| `bin/fm-forge-detect.sh` | 63 | 1.7 | 29 | 19 | 5 | 0 | 5 | 5 |
| `bin/fm-mutation-verdict.sh` | 183 | 3.7 | 132 | 99 | 20 | 0 | 13 | 13 |
| `bin/fm-operational-input.sh` | 435 | 6.4 | 359 | 197 | 117 | 0 | 45 | 45 |
| `bin/fm-panel-report.sh` | 289 | 3.4 | 200 | 129 | 33 | 1 | 37 | 37 |
| `bin/fm-remote-delta-read.sh` | 231 | 10.1 | 185 | 98 | 65 | 1 | 21 | 21 |
| `bin/fm-remote-entrypoint.sh` | 193 | 0.7 | 159 | 8 | 127 | 0 | 24 | 24 |
| `bin/fm-resolve-toggles.sh` | 124 | 4.1 | 78 | 60 | 10 | 0 | 8 | 8 |
| `bin/fm-secondmate-parent-migrate.sh` | 388 | 11.0 | 275 | 144 | 89 | 6 | 36 | 36 |
| `bin/fm-supervision-instructions.sh` | 289 | 7.3 | 191 | 92 | 41 | 7 | 51 | 51 |
| `bin/fm-trace-context-lib.sh` | 227 | 1.6 | 117 | 61 | 40 | 0 | 16 | 16 |
| `bin/fm-transition-lib.sh` | 103 | 0.8 | 23 | 15 | 2 | 0 | 6 | 6 |

## Distribution across five runs

Each row is min, median, and max over the five runs. `max_over_median` is shown only where the median is above zero. Stable means max is at most 1.2 times the median, or, when the median is zero, max is zero.

| Measure | Unit | Min | Median | Max | Max / median | Stable |
|---|---|---:|---:|---:|---:|---|
| container_cpu_s_approx | s | 6961.00 | 7078.00 | 7982.00 | 1.128 | yes |
| container_peak_mem_MiB | MiB | 277.40 | 386.80 | 402.40 | 1.040 | yes |
| error | count | 309 | 309 | 309 | 1.000 | yes |
| excluded | count | 0 | 0 | 0 | n/a (median 0) | yes |
| host_max_rss_MiB | MiB | 44.90 | 45.10 | 45.20 | 1.002 | yes |
| host_real_s | s | 7486.78 | 7575.92 | 8587.79 | 1.134 | yes |
| host_sys_s | s | 14.36 | 19.28 | 19.53 | 1.013 | yes |
| host_user_s | s | 5.74 | 8.91 | 8.99 | 1.009 | yes |
| invalid_mutants | count | 309 | 309 | 309 | 1.000 | yes |
| killed | count | 1134 | 1134 | 1134 | 1.000 | yes |
| mutants_total | count | 2126 | 2126 | 2126 | 1.000 | yes |
| seeded_named_killed | count | 4 | 4 | 4 | 1.000 | yes |
| survived | count | 668 | 668 | 668 | 1.000 | yes |
| timeout | count | 15 | 15 | 15 | 1.000 | yes |
| unresolved | count | 0 | 0 | 0 | n/a (median 0) | yes |
| valid_run | flag | 1 | 1 | 1 | 1.000 | yes |
| wall_s | s | 7487.00 | 7576.00 | 8587.00 | 1.133 | yes |

Surface measures and per-script counts are in `docs/verification/mutation-baseline/distribution.tsv` (87 rows). Raw per-run values are in `docs/verification/mutation-baseline/long.tsv` (435 rows).

## Oracle pre-check

The pre-check ran the unmutated oracle of each of the 96 oracle-bearing eligible shell scripts once, sequentially, in a container with the wrapper's limits. The table shows the exit code, the unmutated wall time, the line count, and the class assigned by the pre-check. Only the clean scripts with wall at most 12 seconds are in the subset.

Classes:

- `clean`: exit 0, no skip or tool-absent message.
- `fails-in-image`: oracle exited 1 in the image. Some of these fail because a tool the image lacks is missing, such as `jq` (rows 4 and 16 are examples; see the Detail column).
- `skip-by-tool`: the oracle reported that a required tool was absent. Two rows were relabelled from `skip-other` by hand because their skip message names `tasks-axi` as not found. This is a hand relabel and is disclosed as such.
- `skip-other`: exit 0 with a skip word in the text. Seven rows remain here. Their skip words appear only inside passing test names, so they were not reclassified (see [Disclosures](#disclosures)).
- `timeout`: exceeded the 300-second oracle bound inside the container.

Totals: 28 clean, 44 fails-in-image, 15 skip-by-tool (13 raw plus 2 relabelled), 7 skip-other, 2 timeout. These sum to 96.

| # | Script | Oracle exit | Oracle wall (s) | Lines | Class | Baseline | Detail |
|---:|---|---:|---:|---:|---|---|---|
| 1 | `bin/fm-afk-contract.sh` | 0 | 77.7 | 656 | clean | excluded: wall over 12 s |  |
| 2 | `bin/fm-afk-launch.sh` | 1 | 130.3 | 932 | fails-in-image | excluded: not clean | ok - failed start: preserves the posture record enter wrote |
| 3 | `bin/fm-afk-return.sh` | 1 | 3.7 | 930 | fails-in-image | excluded: not clean | not ok - Bearings should render behind the return catch-up gate: fm-bearings-snapshot: jq not found |
| 4 | `bin/fm-arm-pretool-check.sh` | 1 | 47.2 | 200 | fails-in-image | excluded: not clean | tests/fm-arm-pretool-check.test.sh: line 164: jq: command not found |
| 5 | `bin/fm-backend.sh` | 1 | 0.6 | 1107 | fails-in-image | excluded: not clean | fatal: not a git repository (or any of the parent directories): .git |
| 6 | `bin/fm-backlog-handoff.sh` | 0 | 0.3 | 1100 | skip-by-tool | excluded: not clean | skip: tasks-axi not found (required by the delegated handoff path); relabelled from skip-other: tasks-axi absent |
| 7 | `bin/fm-bearings-board.sh` | 0 | 0.4 | 459 | skip-by-tool | excluded: not clean | skip: jq not found |
| 8 | `bin/fm-bearings-snapshot.sh` | 0 | 0.4 | 740 | skip-by-tool | excluded: not clean | skip: jq not found |
| 9 | `bin/fm-birds-eye-capture.sh` | 0 | 0.3 | 346 | skip-by-tool | excluded: not clean | skip: jq not found |
| 10 | `bin/fm-birds-eye-export.sh` | 0 | 0.4 | 251 | skip-by-tool | excluded: not clean | skip: jq not found |
| 11 | `bin/fm-birds-eye-model.sh` | 0 | 0.4 | 344 | skip-by-tool | excluded: not clean | skip: jq not found |
| 12 | `bin/fm-birds-eye-view.sh` | 1 | 3.9 | 199 | fails-in-image | excluded: not clean | ok - a failing or missing exporter exits 1 and leaves no snapshot |
| 13 | `bin/fm-bootstrap.sh` | 1 | 121.7 | 1620 | fails-in-image | excluded: not clean | ok - bootstrap: unknown resolved backends fail closed with an actionable diagnostic |
| 14 | `bin/fm-brief.sh` | 0 | 81.1 | 836 | clean | excluded: wall over 12 s |  |
| 15 | `bin/fm-capability-schema.sh` | 0 | 2.3 | 203 | clean | included |  |
| 16 | `bin/fm-cd-pretool-check.sh` | 1 | 0.7 | 190 | fails-in-image | excluded: not clean | tests/fm-cd-pretool-check.test.sh: line 156: jq: command not found |
| 17 | `bin/fm-check-unregister.sh` | 0 | 3.4 | 52 | clean | included |  |
| 18 | `bin/fm-claude-stop-autoarm.sh` | 1 | 156.3 | 588 | fails-in-image | excluded: not clean | ok - auto-arm: bounded failure verification emits one automatic-mechanism alarm |
| 19 | `bin/fm-claude-trust.sh` | 1 | 31.1 | 593 | fails-in-image | excluded: not clean | ok - fm-claude-trust.sh: an exported CDPATH cannot defeat the scope refusal |
| 20 | `bin/fm-composer-lib.sh` | 0 | 64.2 | 1980 | skip-other | excluded: not clean | ok - a dialog note without a sink skips the match, and release leaves a caller RETURN trap |
| 21 | `bin/fm-contributions.sh` | 1 | 65.5 | 517 | fails-in-image | excluded: not clean | tests/fm-contributions.test.sh: line 32: jq: command not found |
| 22 | `bin/fm-control.sh` | 0 | 297.7 | 1182 | clean | excluded: wall over 12 s |  |
| 23 | `bin/fm-crew-state.sh` | 1 | 171.1 | 1356 | fails-in-image | excluded: not clean | ok - captured AXI failed status replays through crew-state |
| 24 | `bin/fm-dispatch-resolve.sh` | 1 | 0.9 | 520 | fails-in-image | excluded: not clean | ln: failed to create symbolic link '/tmp/fm-dispatch-resolve.YlddMv/no-curl-bin/jq' -> '': No such file or directory |
| 25 | `bin/fm-dod-lib.sh` | 0 | 21.6 | 749 | clean | excluded: wall over 12 s |  |
| 26 | `bin/fm-ensure-agents-md.sh` | 0 | 9.4 | 269 | clean | included |  |
| 27 | `bin/fm-fleet-ledger.sh` | 1 | 1.6 | 234 | fails-in-image | excluded: not clean | not ok - spawn failed: error: fm-spawn.sh on-lifecycle-t1: delivery mode 'local-only' is not supported for ship launches under the managed-context contract; the supported modes are no-mistakes and direct-PR (docs/managed-context.md) |
| 28 | `bin/fm-fleet-sync.sh` | 0 | 131.3 | 465 | skip-other | excluded: not clean | ok - no-origin clone is skipped (benign), not flagged STUCK |
| 29 | `bin/fm-forge-detect.sh` | 0 | 1.7 | 63 | clean | included |  |
| 30 | `bin/fm-git-strip-ai-trailers.sh` | 1 | 18.3 | 265 | fails-in-image | excluded: not clean | ok - a hook manager cannot displace the strip (skipped as root) |
| 31 | `bin/fm-herdr-lab.sh` | 1 | 0.5 | 593 | fails-in-image | excluded: not clean | ok - fm-herdr-lab: names fail closed and require the lab prefix |
| 32 | `bin/fm-herdr-session-cleanup.sh` | 1 | 1.7 | 337 | fails-in-image | excluded: not clean | not ok - Linux Herdr process argv array was not accepted |
| 33 | `bin/fm-home-summary-refresh.sh` | 0 | 0.4 | 257 | skip-by-tool | excluded: not clean | skip: jq not found |
| 34 | `bin/fm-host-mirror.sh` | 0 | 0.3 | 319 | skip-by-tool | excluded: not clean | skip: jq absent |
| 35 | `bin/fm-inactive-reconcile.sh` | 0 | 224.8 | 710 | skip-other | excluded: not clean | ok - a delivered ship done: skips the git gate on later polls |
| 36 | `bin/fm-inbox.sh` | 0 | 37.4 | 1159 | clean | excluded: wall over 12 s |  |
| 37 | `bin/fm-jev-mem-guard.sh` | 127 | 0.1 | 7 | fails-in-image | excluded: not clean | tests/fm-jev-mem-guard.test.sh: line 19: shellcheck: command not found |
| 38 | `bin/fm-lint-workflows.sh` | 1 | 0.5 | 143 | fails-in-image | excluded: not clean | fm-lint-workflows.sh: actionlint not found; install actionlint 1.7.12 with bin/fm-install-actionlint.sh `<destination-directory>` and put that directory on PATH. |
| 39 | `bin/fm-lint.sh` | 1 | 67.9 | 1408 | fails-in-image | excluded: not clean | not ok - canonical partition 1of2 failed: fm-lint.sh: ShellCheck 0.11.0 (pinned 0.11.0) |
| 40 | `bin/fm-live-lab.sh` | 0 | 0.4 | 869 | skip-by-tool | excluded: not clean | ok - skipped: tmux is not installed |
| 41 | `bin/fm-mail-check.sh` | 1 | 15.9 | 395 | fails-in-image | excluded: not clean | ok - fm-mail-check: a poll failure is reported once and re-reported after recovery |
| 42 | `bin/fm-mail.sh` | 1 | 38.6 | 650 | fails-in-image | excluded: not clean | ok - fm-mail: missing required configuration fails cleanly naming the variable |
| 43 | `bin/fm-mutation-classify.sh` | 0 | 2.8 | 55 | skip-other | excluded: not clean | ok - an unclassified path fails the step, is named on stderr, and is never silently skipped |
| 44 | `bin/fm-mutation-container-run.sh` | 0 | 21.3 | 303 | clean | excluded: wall over 12 s |  |
| 45 | `bin/fm-mutation-dispositions.sh` | 0 | 21.5 | 393 | clean | excluded: wall over 12 s |  |
| 46 | `bin/fm-mutation-run.sh` | 1 | 41.3 | 312 | fails-in-image | excluded: not clean | ok - timeouts and errors never count as killed, and unresolved paths are never recorded as passing |
| 47 | `bin/fm-mutation-verdict.sh` | 0 | 3.7 | 183 | clean | included |  |
| 48 | `bin/fm-on.sh` | 1 | 58.2 | 125 | fails-in-image | excluded: not clean | ok - fm-on rejects invalid dead-peer settings before launching ssh |
| 49 | `bin/fm-operational-input.sh` | 0 | 6.4 | 435 | clean | included |  |
| 50 | `bin/fm-oracle-map-check.sh` | 1 | 0.4 | 117 | fails-in-image | excluded: not clean | not ok - the committed oracle map must cover every in-scope path: expected exit 0, got 2 |
| 51 | `bin/fm-panel-report.sh` | 0 | 3.4 | 289 | clean | included |  |
| 52 | `bin/fm-panel-resolve.sh` | 0 | 3.2 | 133 | skip-other | excluded: not clean | ok - unsupported and unavailable models stay unresolved with the requested model and reason |
| 53 | `bin/fm-pipeline-spend.sh` | 1 | 1.5 | 298 | fails-in-image | excluded: not clean | tests/fm-pipeline-spend.test.sh: line 126: jq: command not found |
| 54 | `bin/fm-pr-merge.sh` | 1 | 0.3 | 1800 | fails-in-image | excluded: not clean | not ok - these tests read glab's JSON with the real jq, which was not found |
| 55 | `bin/fm-pr-reviewers.sh` | 1 | 0.4 | 101 | fails-in-image | excluded: not clean | not ok - these tests run the script's own jq programs over API-shaped JSON with the real jq, which was not found |
| 56 | `bin/fm-pr-state.sh` | 1 | 0.4 | 153 | fails-in-image | excluded: not clean | not ok - these tests run the script's own jq programs over API-shaped JSON with the real jq, which was not found |
| 57 | `bin/fm-procevent-quota.sh` | 1 | 0.5 | 328 | fails-in-image | excluded: not clean | ln: failed to create symbolic link '/tmp/fm-procevent-quota.144MUd/no-quota-bin/jq' -> '': No such file or directory |
| 58 | `bin/fm-procevent-when.sh` | 0 | 207.2 | 638 | clean | excluded: wall over 12 s |  |
| 59 | `bin/fm-procevent.sh` | 1 | 16.6 | 2570 | fails-in-image | excluded: not clean | check: process-event source src-one is registered but its launch did not prove it took the source's claim within FM_PROCEVENT_LAUNCH_CONFIRM_SECONDS, so nothing is confirmed to be collecting from it; reconcile reports that as failed= and keeps launching it every supervision cycle. If it stays that way, check the source command and the adapter binary the registration names, and run an attached bin/fm-procevent.sh start src-one to reproduce a refusal on its stderr - the detached launch discards it, and a hand-run reconcile only counts it as failed=. A later cycle that finds the source owned ends this episode on its own, so a runner that was merely slow to claim needs nothing from you. |
| 60 | `bin/fm-project-mode.sh` | 0 | 2.0 | 252 | skip-other | excluded: not clean | ok - fm-project-mode: pstack and no-mistakes toggles are skipped, never read as a mode |
| 61 | `bin/fm-pstack.sh` | 0 | 0.0 | 1194 | skip-by-tool | excluded: not clean | skip: jq not found |
| 62 | `bin/fm-public-followup.sh` | 0 | 0.4 | 1681 | skip-by-tool | excluded: not clean | skip: jq not found |
| 63 | `bin/fm-quota-choose.sh` | 1 | 0.4 | 402 | fails-in-image | excluded: not clean | error: invalid quota-axi snapshot |
| 64 | `bin/fm-remote-delta-read.sh` | 0 | 10.1 | 231 | clean | included |  |
| 65 | `bin/fm-remote-doctor.sh` | 0 | 0.3 | 949 | skip-by-tool | excluded: not clean | skip: jq not found (the herdr adapter parses its JSON) |
| 66 | `bin/fm-remote-entrypoint.sh` | 0 | 0.7 | 193 | clean | included |  |
| 67 | `bin/fm-remote-herdr-guard.sh` | 0 | 0.3 | 108 | skip-by-tool | excluded: not clean | skip: jq not found (the guard parses herdr's JSON, and jq is the holder process) |
| 68 | `bin/fm-remote-secondmate-relaunch.sh` | 1 | 0.6 | 95 | fails-in-image | excluded: not clean | error: remote command is not tracked by this Firstmate checkout: fm-remote-secondmate-control.sh: expected exit 0, got 1 |
| 69 | `bin/fm-resolve-toggles.sh` | 0 | 4.1 | 124 | clean | included |  |
| 70 | `bin/fm-review-diff.sh` | 0 | 42.4 | 182 | clean | excluded: wall over 12 s |  |
| 71 | `bin/fm-secondmate-parent-migrate.sh` | 0 | 11.0 | 388 | clean | included |  |
| 72 | `bin/fm-secondmate-reconcile.sh` | 0 | 0.3 | 588 | skip-by-tool | excluded: not clean | skip: jq not found |
| 73 | `bin/fm-secondmate-restart.sh` | 1 | 219.3 | 380 | fails-in-image | excluded: not clean | ok - T3 a runtime that cannot prove a restart falls back to the re-read message |
| 74 | `bin/fm-session-start.sh` | 1 | 6.2 | 1098 | fails-in-image | excluded: not clean | tests/fm-session-start.test.sh: line 789: jq: command not found |
| 75 | `bin/fm-sessionstart-nudge.sh` | 1 | 259.5 | 54 | fails-in-image | excluded: not clean | not ok - a failed state dir creation must still stand down without a digest, got:  |
| 76 | `bin/fm-startup-growth-check.sh` | 0 | 46.8 | 417 | clean | excluded: wall over 12 s |  |
| 77 | `bin/fm-startup-memory-budget.sh` | 0 | 39.4 | 94 | clean | excluded: wall over 12 s |  |
| 78 | `bin/fm-startup-network.sh` | 1 | 69.9 | 757 | fails-in-image | excluded: not clean | ok - fm-startup-network: wait fails when no deferred stage publishes before its deadline |
| 79 | `bin/fm-stow-cascade.sh` | 1 | 4.3 | 251 | fails-in-image | excluded: not clean | ok - each home is accounted against its own allowance instead of a fleet total |
| 80 | `bin/fm-subagent-pretool-check.sh` | 1 | 0.6 | 207 | fails-in-image | excluded: not clean | not ok - known delegation tool (Task) deny omitted Claude's permission decision: {"hookSpecificOutput":{"hookEventName":"PreToolUse","permissionDecision":"deny"},"systemMessage":"[subagent-dispatch] the firstmate primary dispatches through the fleet, not the harness's own delegation tools: work started that way has no durable fleet record, leaves every firstmate guard inert, and dies with this session. Instead, first classify the work under the AGENTS.md intake contract, then use bin/fm-brief.sh followed by bin/fm-spawn.sh for dispatched work (blocked tool: Task, delegation-shaped on \"task\"). Launch the session with FM_ALLOW_SUBAGENT=1 for a deliberate exception."} |
| 81 | `bin/fm-supervision-host.sh` | 1 | 15.7 | 1191 | fails-in-image | excluded: not clean | not ok - the host must hand its operator outcome to main (one quoted word changed, see Disclosures) |
| 82 | `bin/fm-supervision-instructions.sh` | 0 | 7.3 | 289 | clean | included |  |
| 83 | `bin/fm-tasks-axi.sh` | 0 | 2.1 | 154 | skip-by-tool | excluded: not clean | skip: tasks-axi not found; home-addressing cases not run; relabelled from skip-other: tasks-axi absent |
| 84 | `bin/fm-teardown.sh` | 1 | 53.3 | 3929 | fails-in-image | excluded: not clean | tests/fm-teardown.test.sh: line 715: jq: command not found |
| 85 | `bin/fm-test-isolation-proof.sh` | 0 | 21.4 | 589 | clean | excluded: wall over 12 s |  |
| 86 | `bin/fm-test-run.sh` | 124 | 300.2 | 2733 | timeout | excluded: not clean | oracle exceeded 300s bound |
| 87 | `bin/fm-timeout-lib.sh` | 0 | 12.8 | 285 | clean | excluded: wall over 12 s |  |
| 88 | `bin/fm-tool-update-check.sh` | 1 | 0.6 | 915 | fails-in-image | excluded: not clean | not ok - PATH skew was not reported as an update that is not in effect (missing: 'herdr update not in effect') |
| 89 | `bin/fm-trace-context-lib.sh` | 0 | 1.6 | 227 | clean | included |  |
| 90 | `bin/fm-transition-lib.sh` | 0 | 0.8 | 103 | clean | included |  |
| 91 | `bin/fm-turnend-guard.sh` | 1 | 2.6 | 553 | fails-in-image | excluded: not clean | ok - fm_supervision_unhealthy: false with no state/*.meta at all |
| 92 | `bin/fm-update.sh` | 0 | 88.5 | 246 | skip-other | excluded: not clean | ok - T4 dirty secondmate skipped, local edit preserved |
| 93 | `bin/fm-vendor-auth-probe.sh` | 0 | 55.2 | 190 | clean | excluded: wall over 12 s |  |
| 94 | `bin/fm-watch-arm.sh` | 124 | 300.8 | 795 | timeout | excluded: not clean | oracle exceeded 300s bound |
| 95 | `bin/fm-watch-checkpoint.sh` | 1 | 25.8 | 213 | fails-in-image | excluded: not clean | ok - quiet checkpoint exits 124 with a clean checkpoint line and no live lock |
| 96 | `bin/mcao-setup.sh` | 1 | 0.5 | 1385 | fails-in-image | excluded: not clean | not ok - test host is missing jq |

## Duration gate

The operator set a 4-hour cap per run (decision 002). The estimated time for one full run was computed as the sum over clean scripts of lines times 0.647 times the unmutated oracle wall. The factor 0.647 mutants per line is the pilot rate of 224 mutants over 346 lines.

| Inclusion cut-off (clean scripts, wall at most) | Scripts | Estimated one-run time (h) |
|---|---:|---:|
| 5 s | 9 | 0.63 |
| 12 s | 14 | 3.15 |
| 30 s | 19 | 11.64 |
| 60 s | 24 | 26.88 |
| 120 s | 26 | 48.22 |
| 300 s | 28 | 135.22 |

The strict clean set of 28 scripts estimated 135.22 hours per run, far above the 4-hour cap, and 676 hours for five runs. Counting the seven text-only skip rows as clean gives 35 scripts and an estimate of 201.85 hours; that set was not used.

The 12-second cut-off was chosen (decision 003). It gives 14 scripts and 3.15 estimated hours, and the actual run took 2.08 to 2.39 hours. The estimate for the 14 scripts was 1973 mutants; the run produced 2126, about 8 percent more. The estimate uses one pilot-average rate of 0.647 mutants per line for every script, so it is not a per-script prediction.

## Seeded-fault control

Four seeded faults with known names were run through the wrapper before each main run, from the scratch root `seeded-control/root`. That root is a separate Git repository outside the real tree; its scratch commit (`482540d` in the operator's run) is not part of the result. Each named fault must be recorded killed with oracle exit 1:

- `bin/seeded-eq-ne.sh` at `L4-eq-ne`
- `bin/seeded-and-or.sh` at `L4-and-or`
- `bin/seeded-status-flip.sh` at `L6-status-flip`
- `bin/seeded-drop-stmt.sh` at `L5-drop-stmt`

All four were killed in all five runs, and the seeded verdict was VALID in each. A run with fewer than four would have been marked INVALID.

## Containment review

The containment review (`data/dpes-822-containment-review/report.md`, section 6) set seven conditions for a container run to be counted. Each is mapped to the evidence observed in this baseline.

1. **Container flags and image.** The wrapper at the pin runs `node:24-bookworm` with `--pull never --network none --cpus 1 --memory 512m --pids-limit 128`. Recorded in [Environment](#environment) and [Commands](#commands). The image is emulated and the tools are as described.
2. **Per-name removal, none left.** Every run's verdict records `containers_left=0` and `wrapper_alive=no`, and each run's log records zero containers under the wrapper's prefix on the host after the run. The positive control also left zero containers.
3. **Outer bound with a positive control.** A control oracle starts a `setsid` loop and sleeps 200 seconds, run through the real wrapper with a 25-second outer bound. Before the bound, a sibling probe running with `--pid=host` found 1 marked process. The wrapper returned 1 at the bound with the message "container run reached the 25-second outer bound; container removal verified=1; no record written". The probe after the bound found 0 marked processes, zero containers were left, and the record was 0 bytes.
4. **Classifier input as a regular file.** Each run copies the classified subset to `input-classified-14.tsv` and passes it to the wrapper by file redirection.
5. **Exit mapping.** At the pin (`bin/fm-mutation-exec-lib.sh`, lines 506 to 512): exit 0 is survived; 124 is timeout (oracle's own timeout, not counted as killed); 126 and 127 are error; 128, 129, 130 to 199, and 200 to 259 are error (signal stop or stop code, not a verdict); every other value is killed. The records contain 15 timeouts and 309 errors, consistent with this mapping.
6. **Probe-timeout path.** Runs used `--probe-timeout 10`. Each record holds one tool row and zero path rows, in every run. No mutant was classified from the tool row's label. The tool row was read only as a record of the run.
7. **Claims scoped to reproduced cases.** Every number in this document describes these five runs, on this host, at this pin, for the 14 scripts listed. No general claim about the repository, its other scripts, other hosts, or CI is made.

## Disclosures

- **Probe and oracle timeouts.** The main run used `--timeout 60` for each oracle, not the wrapper default of 300 seconds. The pre-check used a 300-second bound. The 60-second value is a disclosed deviation from the default.
- **Seeded control scratch root.** The seeded fixtures are classified as test code in the real tree, so the control runs from `seeded-control/root` at commit `482540d`, outside the tree.
- **Quoted test-name wording.** One word in the Detail column of row 81 is quoted from the oracle's own test name and was changed from the original to "operator", per the naming rule for artifacts. No other row was edited.
- **Hand relabels.** Two `skip-other` rows (`bin/fm-backlog-handoff.sh`, `bin/fm-tasks-axi.sh`) were relabelled `skip-by-tool` by hand because their skip message names `tasks-axi` as not found. Neither is in the subset.
- **Seven text-only skip rows.** Rows 20, 28, 35, 43, 52, 60, and 92 exit 0 and contain the words "skip" or "unavailable" only inside passing test names. The literal rule excludes them from the subset, and they were not reclassified.
- **Verdict reason text.** Each verdict's reasons field carries "checkout dirty entries after run=0". That is a post-run check on the read-only checkout and is cosmetic. Every verdict remains VALID.
- **Estimate versus actual.** The duration estimate for the 14 scripts (1973 mutants) differs from the 2126 mutants the run produced. The factor is a pilot average and was not refit.
- **Emulation.** All timings are under `amd64` emulation on an `arm64` host.
- **Run 4 timing.** Run 4 was the slowest at 8587 seconds. Its counts match the other runs. No cause was established.
- **Tool absence in the image.** `jq`, `tmux`, and `shellcheck` are absent in the image, per the tool probe. `ruby` and `tasks-axi` are also absent: the pre-check's raw output reports that ruby is required for one oracle, and rows 6 and 83 report that `tasks-axi` was not found. The operator's pre-check notes count at least 16 of the 44 fails-in-image rows as carrying an absent-tool message, without attributing each row to a tool. Scripts that need these tools fail or skip in the image and are excluded, so this baseline says nothing about their behaviour on a normal host.
- **Scope limit: 28 of 96 clean in this image.** The pre-check ran the unmutated oracle of 96 oracle-bearing scripts. Only 28 had a clean oracle in the only allowed image, `node:24-bookworm`. The other 68 did not: 44 failed in the image, 15 skipped because a tool was absent, 7 skipped on text inside passing test names, and 2 timed out. Of the 28 clean scripts, 14 also met the 12-second wall limit and form the subset. A wider container baseline would need an image that carries `jq`, `tmux`, `shellcheck`, `ruby`, and `tasks-axi`. That image change, and any build, pull, or package install for it, is not approved and was not done. This baseline therefore speaks only for the 14-script fast-oracle clean subset: 14 of the 225 mutate-class shell scripts, about 6 percent. It does not speak for the repository.

## Reproduction

Work in an empty directory named `work/`. Paths below are relative to it.

Test status when this section was written: steps 1, 3, and 4 were run again in a scratch directory, with a local clone of the repository standing in for the GitHub clone in step 1. Their outputs matched the operator's private files byte for byte. The seeded-count check in step 5 was run on the operator's run-1 record and counted 4. Step 2 was not run, because the container daemon was unreachable. The container runs in step 5 and the aggregation in step 6 were not run.

1. **Pinned checkout.** `frozen/` is a checkout of the pinned commit. The wrapper, runner, classifier, and seeded fixtures are read from it. Every measured tree is a clone of it, never `frozen/` itself.

   ```sh
   git clone https://github.com/mc-innersource/motif-hve-multi-agent-orchestrator.git frozen
   git -C frozen -c advice.detachedHead=false checkout --detach f17b7069f5e77cf984f6dd7080a969896ad513fc
   test -z "$(git -C frozen status --porcelain)"
   ```

2. **Image check. STOP if it differs.** The wrapper runs with `--pull never`, so it uses whatever is cached under the tag `node:24-bookworm`. Confirm that the cached image is the measured one before any run. Do not pull to fix a mismatch.

   ```sh
   docker image inspect node:24-bookworm --format '{{.Id}} {{.Os}} {{.Architecture}}'
   docker image inspect node:24-bookworm --format '{{json .RepoDigests}}'
   ```

   The first command must print `sha256:03905f546fa8c284c377142124957871d1e8b9ddd0d69e869e233b9eeffd7790 linux amd64`. The second must list `node@sha256:14078204b6bb268c107ceff6d3c43f45a888105e5c2b18b14c32b3409d93a1b0`. If either differs, STOP. The runs would measure a different image, and the results in this document do not apply to it.

3. **Classified input for the 14 scripts.** The classifier reads paths on stdin and is run from the root of `frozen/`. The paths are the 14 in the [Subset](#subset-and-inclusion-rule) table.

   ```sh
   printf '%s\n' \
     bin/fm-capability-schema.sh bin/fm-check-unregister.sh bin/fm-ensure-agents-md.sh \
     bin/fm-forge-detect.sh bin/fm-mutation-verdict.sh bin/fm-operational-input.sh \
     bin/fm-panel-report.sh bin/fm-remote-delta-read.sh bin/fm-remote-entrypoint.sh \
     bin/fm-resolve-toggles.sh bin/fm-secondmate-parent-migrate.sh \
     bin/fm-supervision-instructions.sh bin/fm-trace-context-lib.sh bin/fm-transition-lib.sh > paths-14.txt
   (cd frozen && bin/fm-mutation-classify.sh < ../paths-14.txt > ../input-classified-14.tsv)
   ```

   The output has 14 rows, one per path, each with class `mutate`.

4. **Seeded control root.** Build it from the committed seeded fixtures at the pin, as `tests/fm-mutation-run.test.sh` lines 641 to 652 do. Then commit it, so that its work tree is clean before each run.

   ```sh
   mkdir -p seeded-control/root/bin seeded-control/root/tests
   git -C seeded-control/root init -q
   for s in seeded-eq-ne seeded-and-or seeded-status-flip seeded-drop-stmt; do
     cp "frozen/tests/fixtures/mutation-seeded/$s/bin/$s.sh" seeded-control/root/bin/
     cp "frozen/tests/fixtures/mutation-seeded/$s/tests/$s.test.sh" seeded-control/root/tests/
   done
   git -C seeded-control/root add bin tests
   git -C seeded-control/root -c user.name=repro -c user.email=repro@localhost commit -q -m "seeded control fixtures"
   printf '%s\n' bin/seeded-eq-ne.sh bin/seeded-and-or.sh bin/seeded-status-flip.sh bin/seeded-drop-stmt.sh > seeded-control/changed.txt
   (cd frozen && bin/fm-mutation-classify.sh < ../seeded-control/changed.txt > ../seeded-control/classified.tsv)
   ```

   The test at those lines builds the same files without a commit. This section commits them so the work tree is clean before each run. The commit belongs to the scratch repository and is not part of the result. In the scratch rebuild, the work tree was clean after the commit, and the files matched the operator's root.

5. **Runs.** Run the five runs one after another, not in parallel. For each run number `N` from 1 to 5, create the run directory and a fresh clone of the pin, then run the seeded control and the subset as in [Commands](#commands). Use absolute paths for `--root`, because the wrapper compares it with the work-tree top level resolved by `pwd -P`:

   ```sh
   W=$(pwd -P)
   mkdir -p runs/run-N
   git clone -q --no-hardlinks frozen runs/run-N/tree
   git -C runs/run-N/tree -c advice.detachedHead=false checkout -q --detach f17b7069f5e77cf984f6dd7080a969896ad513fc
   test -z "$(git -C runs/run-N/tree status --porcelain)"
   cp input-classified-14.tsv runs/run-N/
   frozen/bin/fm-mutation-container-run.sh --root "$W/seeded-control/root" \
     --container-timeout 600 --timeout 30 \
     < seeded-control/classified.tsv > runs/run-N/seeded.record.tsv
   frozen/bin/fm-mutation-container-run.sh --root "$W/runs/run-N/tree" \
     --container-timeout 15600 --budget 14400 --timeout 60 \
     --probe-timeout 10 --syntax-timeout 10 \
     < runs/run-N/input-classified-14.tsv > runs/run-N/record.tsv
   ```

   After each run, check the seeded control. All four named faults must be recorded killed with `oracle exit 1`, so the count must be 4:

   ```sh
   awk -F'\t' 'BEGIN { split("bin/seeded-eq-ne.sh L4-eq-ne|bin/seeded-and-or.sh L4-and-or|bin/seeded-status-flip.sh L6-status-flip|bin/seeded-drop-stmt.sh L5-drop-stmt", a, "|"); for (i in a) want[a[i]] = 1 } $1 == "mutant" && $5 == "killed" && $6 == "oracle exit 1" && (($2 " " $4) in want) { n++ } END { print n + 0 }' runs/run-N/seeded.record.tsv
   ```

   Also check that no container is left under the wrapper's name prefix (`docker ps -a -q --filter name=fm-mutation-run-` prints nothing). A run that fails either check is not a valid run.

   Each run took 2.1 to 2.4 hours on the operator's host, so five runs take about 11 hours in total.

6. **Compare and aggregate.** Each `runs/run-N/record.tsv` for N from 2 to 5 must be byte-identical to `runs/run-1/record.tsv`. The aggregation script is operator-private and is not committed. The method below is what produced `long.tsv` and `distribution.tsv`:
   - `long.tsv` has one row per run, surface, measure, unit, and value: 5 runs times 87 measures, 435 rows. The surface is the label `fast-oracle-14` for run-level measures, and the script path for per-script measures.
   - Run-level counts come from the `summary` row of `record.tsv`, field 6: `killed`, `survived`, `timeout`, `error`, `unresolved`, and `excluded`. `mutants_total` is killed plus survived plus timeout plus error. `invalid_mutants` is the number of `mutant` rows whose error text is `mutant does not parse`.
   - `valid_run` (1 when the verdict says `VALID`), `seeded_named_killed`, `wall_s`, `container_cpu_s_approx`, and `container_peak_mem_MiB` come from the verdict file that the operator's driver wrote for each run. `host_real_s`, `host_user_s`, `host_sys_s`, and `host_max_rss_MiB` come from the `/usr/bin/time -l` output for each run. Maximum resident set size is taken from bytes, divided by 1,048,576, to one decimal place.
   - Per-script counts are `killed`, `survived`, `timeout`, `error`, and `mutants_total`, counted from the `mutant` rows of `record.tsv` for each of the 14 paths.
   - `distribution.tsv` has one row per surface and measure, 87 rows. The five values are sorted. Min is the first, median is the third, and max is the fifth. For a median above zero, the measure is stable when max is at most 1.2 times the median, and `max_over_median` is shown. For a median of zero, the measure is stable only when max is zero, and `max_over_median` is shown as not applicable. Count and flag measures print as integers, and all others with two decimal places.
   - The aggregation lists any unstable measure on its error output. The operator's run listed none.

The TSVs are the results. This change does not re-run the measurement or the aggregation, and it does not re-derive the TSVs from raw records. Re-deriving them needs the five run records, which are operator-private and are not part of this change.

On another host or image, timings are not expected to match. Counts are expected to match only if the image, its tools, and the pin are identical. Neither expectation was tested.

## Relationship to CI

This document and its two TSV files record a measurement made outside CI. No workflow, script, or test in this repository reads them.

A separate, later change proposes an artifact upload for the mutation verdict record in `.github/workflows/mutation-verdict.yml`. That change affects only future pull request heads that run the workflow. It does not change this baseline's measurements or the pinned commit.
