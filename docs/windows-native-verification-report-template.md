# Native Windows verification report

Fill this file as `<RUN_ID>-SUMMARY.md` in the run folder (`%LOCALAPPDATA%\mcao-winverify\<RUN_ID>\`), then publish it to `results/` of the results checkout as described in [`windows-native-verification.md`](windows-native-verification.md) section 2.4. Replace every `<...>` placeholder. Write `SKIPPED` rows with their reason. Do not delete a row. Do not add a PASS that the logs do not show. Companion packet: [`windows-native-verification.md`](windows-native-verification.md).

Redact before saving: Windows user name as `<user>`, machine name as `<host>`, OneDrive profile paths as `<onedrive>`, and any token, key, or password as `<redacted>`. Do not paste environment dumps.

## 1. Run identity

- Request id: `<RUN_ID>`
- Request fields as read from the packet's `Current request` block: `MODE: <full | targeted>`, `ROWS: <ALL | list>`, `NOTES: <text or none>`
- Branch fetched (`origin/windows-verification`) and detached at: `<full sha from git rev-parse HEAD>`
- Commit tested: `<same full sha; the tip of origin/windows-verification fetched for this run>`
- Commit published as `results/<RUN_ID>.started`: `<UTC time and the same sha>`
- `git status --porcelain` on the test checkout after the detached checkout: `<empty | list of lines>`
- `git status --porcelain` on the test checkout after the last row: `<empty | list of lines>`
- Test checkout path (user name redacted): `<path>`
- Results checkout path (user name redacted): `<path>`
- Build directory (`CARGO_TARGET_DIR`, user name redacted): `<path>`
- Run folder path (user name redacted): `<path>`
- Start and end time (local and UTC): `<start>` / `<end>`
- Operator who ran the packet: `<name or role>`
- Shell used for every row: `<pwsh <version> | powershell.exe <version>>`
- `gh` on PATH for the preflight rows: `<real gh | stub gh.exe in run folder | absent (W-33 SKIPPED)>`
- Results published (commit sha on `windows-verification`, each push attempt count): `<sha; attempts n>`

## 2. Environment block

| Field | Value |
| --- | --- |
| Windows version (`VersionString`) | `<value>` |
| Windows edition (`Caption`) | `<value>` |
| Windows build (`BuildNumber`) | `<value>` |
| Architecture (`PROCESSOR_ARCHITECTURE`) | `<value>` |
| PowerShell version and edition | `<value> <Desktop or Core>` |
| git | `<git --version output>` |
| rustc (`rustc -Vv`, host line only) | `<value>` |
| cargo (`cargo -V`) | `<value>` |
| Active toolchain (`rustup show active-toolchain`) | `<value>` |
| Rust targets installed (`rustup target list --installed`) | `<value>` |
| MSVC Build Tools found (section 2.1 of the packet) | `<yes, path redacted to <vs> | no>` |
| `link.exe` outside the system folder (section 2.1) | `<yes | no>` |
| Proxy in use (value never printed) | `<yes | no>` |
| OneDrive-redirected profile | `<yes | no | unknown, with the check used>` |
| Running elevated (administrator) | `<yes | no>` |
| Long paths: `core.longpaths` and `LongPathsEnabled` (read back) | `<value / value>` |
| rustup present and the installed toolchain used (section 2.2; no installer run) | `<yes | no, with the blocker>` |

## 3. Summary table

One row per matrix row in `windows-native-verification.md` section 4 (W-01 to W-49, with W-39 as a reference line).

Status values for a run, the same six that packet section 2.4 step 7 lists: `PASS`, `FAIL`, `FAIL (vacuous)`, `SKIPPED`, `TIMEOUT`, `SPEC-ONLY observed`. `FAIL (vacuous)` is a row whose log shows no `running N tests` line with N of at least 1, while the row expects tests. A row the packet marks UNRESOLVED (W-21, and W-49) is recorded as `FAIL` when its run fails; the packet's UNRESOLVED label stays in the Packet status column. W-49 is `PASS` only when all 20 runs pass. It is `TIMEOUT` when the packet's 30-minute row budget ends its loop before run 20 (a run killed at its 10-minute limit counts as a failed run); otherwise it is `FAIL`, with the counts in Notes. W-49 is `SKIPPED`, reason `not requested`, unless the request `NOTES` key line is exactly `NOTES: rerun W-21` (packet section 4.8); naming W-49 in `ROWS` does not trigger it. `SPEC-ONLY observed` is a row whose expectation the packet marks SPEC-ONLY and whose log shows the actual result; record the result, not a pass. `TIMEOUT` is a row stopped at the packet's 30-minute limit, or a W-49 run killed at its 10-minute limit (recorded as that run's exit `TIMEOUT` in `rows.tsv`). `SKIPPED` rows give their reason in Notes.

Columns:

- `Expected tests` is copied from the packet.
- `Tests run` is what the log shows (`running N tests` lines summed, or `n/a` for non-Cargo rows).
- `Packet status` is the row's evidence label in the packet's section 4, from the labels in packet section 1.1 (`VERIFIED-ON-WINDOWS (run 20261102-150124)`, `OBSERVED-FAIL`, `UNRESOLVED`, `SPEC-ONLY`, `LISTED-ON-HOST`, `VERIFIED-BY-READING`).
- `New since run 20261102-150124` is `yes` when the row did not exist in that run or its command or expected result changed after it, and `no` otherwise.

| ID | Command | Expected tests | Tests run | Exit code | Duration (s) | Status | Packet status | New since run 20261102-150124 | Evidence file | Notes |
| --- | --- | --- | --- | --- | --- | --- | --- | --- | --- | --- |
| W-01 | `rustup show active-toolchain`; `rustc -Vv` | n/a | n/a | `<n>` | `<n>` | `<status>` | VERIFIED-ON-WINDOWS | `<yes | no>` | `logs/W-01.log` | `<notes>` |
| W-02 | `cargo build -p mcao-cli --locked --bin mcao` | n/a | n/a | `<n>` | `<n>` | `<status>` | VERIFIED-ON-WINDOWS | `<yes | no>` | `logs/W-02.log` | `<notes>` |
| W-03 | `cargo build -p mcao-probe --locked --target x86_64-pc-windows-msvc` | n/a | n/a | `<n>` | `<n>` | `<status>` | VERIFIED-ON-WINDOWS | `<yes | no>` | `logs/W-03.log` | `<notes>` |
| W-04 | `cargo check --workspace --locked --target x86_64-pc-windows-msvc` | n/a | n/a | `<n>` | `<n>` | `<status>` | VERIFIED-ON-WINDOWS | `<yes | no>` | `logs/W-04.log` | `<notes>` |
| W-05 | `cargo clippy --workspace --all-targets --locked -- -D warnings` | n/a | n/a | `<n>` | `<n>` | `<status>` | OBSERVED-FAIL; current expectation SPEC-ONLY | `<yes | no>` | `logs/W-05.log` | `<notes; exact clippy lines>` |
| W-06 | `cargo test --workspace --locked --no-fail-fast` | at least one `running N tests` line with N of at least 1 across the binaries | `<n>` | `<n>` | `<n>` | `<status>` | SPEC-ONLY | `<yes | no>` | `logs/W-06.log`, `W-06-summary.txt` | `<notes; every failing binary by its Running line; every running 0 tests line>` |
| W-07 | `cargo test -p mcao-core --locked --test error a_task_id_refused_at_creation` | 1 | `<n>` | `<n>` | `<n>` | `<status>` | VERIFIED-ON-WINDOWS | `<yes | no>` | `logs/W-07.log` | `<notes>` |
| W-08 | `cargo test -p mcao-core --locked --test state_encoding` | 46 | `<n>` | `<n>` | `<n>` | `<status>` | VERIFIED-ON-WINDOWS | `<yes | no>` | `logs/W-08.log` | `<notes>` |
| W-09 | `cargo test -p mcao-core --locked --test home` | 22 | `<n>` | `<n>` | `<n>` | `<status>` | VERIFIED-ON-WINDOWS | `<yes | no>` | `logs/W-09.log` | `<notes>` |
| W-10 | `cargo test -p mcao-core --locked --test windows_registry` | 3 | `<n>` | `<n>` | `<n>` | `<status>` | VERIFIED-ON-WINDOWS | `<yes | no>` | `logs/W-10.log` | `<notes>` |
| W-11 | `cargo test -p mcao-core --locked --test preflight` | 36 | `<n>` | `<n>` | `<n>` | `<status>` | VERIFIED-ON-WINDOWS | `<yes | no>` | `logs/W-11.log` | `<notes>` |
| W-12 | `cargo test -p mcao-core --locked --lib preflight` | 13 | `<n>` | `<n>` | `<n>` | `<status>` | VERIFIED-ON-WINDOWS | `<yes | no>` | `logs/W-12.log` | `<notes>` |
| W-13 | `cargo test -p mcao-cli --locked --test preflight` | 13 | `<n>` | `<n>` | `<n>` | `<status>` | VERIFIED-ON-WINDOWS | `<yes | no>` | `logs/W-13.log` | `<notes>` |
| W-14 | `cargo test -p mcao-cli --locked --test cli` | 8 | `<n>` | `<n>` | `<n>` | `<status>` | VERIFIED-ON-WINDOWS | `<yes | no>` | `logs/W-14.log` | `<notes>` |
| W-15 | `cargo test -p mcao-cli --locked --test toolchain` | 8 | `<n>` | `<n>` | `<n>` | `<status>` | VERIFIED-ON-WINDOWS | `<yes | no>` | `logs/W-15.log` | `<notes>` |
| W-16 | `cargo test -p mcao-cli --locked --test herdr_lab_viewer` | 4 | `<n>` | `<n>` | `<n>` | `<status>` | VERIFIED-ON-WINDOWS | `<yes | no>` | `logs/W-16.log` | `<notes>` |
| W-17 | `cargo test -p mcao-cli --locked --test audit_validate --test audit_activity_map` | audit_validate 7; audit_activity_map 8 | `<n>` | `<n>` | `<n>` | `<status>` | OBSERVED-FAIL; current expectation SPEC-ONLY | `<yes | no>` | `logs/W-17.log` | `<notes; ok-line count; zone cases skipped by cfg; per-binary counts>` |
| W-18 | `cargo check --workspace --locked --target x86_64-unknown-linux-gnu`, then `--target aarch64-apple-darwin` | n/a | n/a | `<n>` / `<n>` | `<n>` / `<n>` | `<status>` | VERIFIED-ON-WINDOWS | `<yes | no>` | `logs/W-18-linux.log`, `logs/W-18-darwin.log` | `<notes>` |
| W-19 | `cargo test -p mcao-backend-windows --locked --test job` | 4 running lines totalling 10 tests (7+1+1+1) | `<n>` | `<n>` | `<n>` | `<status>` | VERIFIED-ON-WINDOWS | `<yes | no>` | `logs/W-19.log` | `<notes>` |
| W-20 | `cargo test -p mcao-backend-windows --locked --test backend` | 14 | `<n>` | `<n>` | `<n>` | `<status>` | VERIFIED-ON-WINDOWS | `<yes | no>` | `logs/W-20.log` | `<notes>` |
| W-21 | `cargo test -p mcao-backend-windows --locked --test interrupt` | 23 | `<n>` | `<n>` | `<n>` | `<status>` | UNRESOLVED | `<yes | no>` | `logs/W-21.log` | `<notes; failing test name; left and right values; interrupt.rs line>` |
| W-22 | `cargo test -p mcao-backend-windows --locked --test conpty` | 25 | `<n>` | `<n>` | `<n>` | `<status>` | VERIFIED-ON-WINDOWS | `<yes | no>` | `logs/W-22.log` | `<notes; console host type>` |
| W-23 | `cargo test -p mcao-backend-windows --locked --test console_pipe` | 7 | `<n>` | `<n>` | `<n>` | `<status>` | VERIFIED-ON-WINDOWS | `<yes | no>` | `logs/W-23.log` | `<notes>` |
| W-24 | `cargo test -p mcao-backend-windows --locked --test pipe` | 3 | `<n>` | `<n>` | `<n>` | `<status>` | VERIFIED-ON-WINDOWS | `<yes | no>` | `logs/W-24.log` | `<notes>` |
| W-25 | `cargo test -p mcao-backend-windows --locked --lib console_server` | 13 | `<n>` | `<n>` | `<n>` | `<status>` | VERIFIED-ON-WINDOWS | `<yes | no>` | `logs/W-25.log` | `<notes>` |
| W-26 | `cargo test -p mcao-backend-windows --locked --lib` | 88 | `<n>` | `<n>` | `<n>` | `<status>` | VERIFIED-ON-WINDOWS | `<yes | no>` | `logs/W-26.log` | `<notes>` |
| W-27 | `cargo test -p mcao-probe --locked --test native` | 6 | `<n>` | `<n>` | `<n>` | `<status>` | VERIFIED-ON-WINDOWS | `<yes | no>` | `logs/W-27.log` | `<notes>` |
| W-28 | probe via `bash-free-probe.ps1` (child shell, bash-free PATH): `mcao-probe.exe --backend native --harness stub --scenario cooperative` | not a Cargo test | n/a | `<n>` | `<n>` | `<status>` | VERIFIED-ON-WINDOWS | `<yes | no>` | `logs/W-28.log`, `probe-evidence.json`, `bash-free-path.json` | `<notes; per-operation support, in order spawn, capture, send_text, interrupt, agent_state, reconnect, cleanup>` |
| W-29 | Bash-free PATH counts from `bash-free-path.json` (`bash`, `sh`, `wsl`) | not a Cargo test | n/a | `<n/a>` | `<n>` | `<status>` | VERIFIED-ON-WINDOWS | `<yes | no>` | `bash-free-path.json` | `<counts only: removed_directories, kept_directories, names found>` |
| W-30 | `cargo test -p mcao-probe --locked --test platform_leakage` | 12 | `<n>` | `<n>` | `<n>` | `<status>` | VERIFIED-ON-WINDOWS | `<yes | no>` | `logs/W-30.log` | `<notes>` |
| W-31 | `mcao.exe --version` | not a Cargo test | n/a | `<n>` | `<n>` | `<status>` | VERIFIED-ON-WINDOWS | `<yes | no>` | `logs/W-31.log` | `<notes>` |
| W-32 | `mcao.exe --help` | not a Cargo test | n/a | `<n>` | `<n>` | `<status>` | VERIFIED-ON-WINDOWS | `<yes | no>` | `logs/W-32.log` | `<notes>` |
| W-33 | `mcao.exe preflight` (Git and real gh on PATH) | not a Cargo test | n/a | `<n>` | `<n>` | `<status>` | VERIFIED-ON-WINDOWS | `<yes | no>` | `logs/W-33.log` | `<notes; Gh line as printed; SKIPPED reason if gh absent>` |
| W-34 | `mcao.exe preflight` with Git removed from PATH (child, `-ExpectGitMissing`) | not a Cargo test | n/a | `<n>` | `<n>` | `<status>` | VERIFIED-ON-WINDOWS | `<yes | no>` | `logs/W-34.log` | `<notes; exact Git line>` |
| W-35 | `mcao.exe preflight` with spaces and non-ASCII FM_HOME | not a Cargo test | n/a | `<n>` | `<n>` | `<status>` | VERIFIED-ON-WINDOWS | `<yes | no>` | `logs/W-35.log` | `<notes; exact path line, user name redacted; gh stub or real gh>` |
| W-36 | `mcao.exe preflight` with UNC FM_HOME (`\\localhost\c$\Users\<user>\AppData\Local\Temp\mcao-winverify-unc`; no folder created) | not a Cargo test | n/a | `<n or n/a>` | `<n>` | `<status>` | SPEC-ONLY (owner ruling pending) | `<yes | no>` | `logs/W-36.log` | `<notes: exit code, ok lines, any UNC warning line as observation, or SKIPPED reason admin share unreachable>` |
| W-37 | `mcao.exe preflight` with OneDrive root, for `OneDrive`, `OneDriveCommercial`, `OneDriveConsumer` | not a Cargo test | n/a | `<n> / <n> / <n>` | `<n>` | `<status>` | VERIFIED-ON-WINDOWS | `<yes | no>` | `logs/W-37-OneDrive.log`, `logs/W-37-OneDriveCommercial.log`, `logs/W-37-OneDriveConsumer.log` | `<notes; OneDrive line; expected exit 3 for each>` |
| W-37b | `mcao.exe preflight` with a profile sync folder (warning only) | not a Cargo test | n/a | `<n>` | `<n>` | `<status>` | VERIFIED-ON-WINDOWS | `<yes | no>` | `logs/W-37b.log` | `<notes; warning line; expected exit 0>` |
| W-38 | `cargo test -p mcao-core --locked --test preflight the_default_home` | 2 | `<n>` | `<n>` | `<n>` | `<status>` | VERIFIED-ON-WINDOWS | `<yes | no>` | `logs/W-38.log` | `<notes>` |
| W-39 | Reference only: W-13 covers the overlong-home test | n/a | n/a | `<n/a>` | `<n/a>` | `SKIPPED` | VERIFIED-BY-READING | `<yes | no>` | none | `<notes: reference only, no run>` |
| W-40 | `cargo test -p mcao-core --locked --test state_encoding a_new_task_id_with_uppercase` | 1 | `<n>` | `<n>` | `<n>` | `<status>` | VERIFIED-ON-WINDOWS | `<yes | no>` | `logs/W-40.log` | `<notes>` |
| W-41 | `cargo test -p mcao-cli --locked --no-fail-fast --test audit_writer_cli` | 0 (`#![cfg(unix)]`) | `<n>` | `<n>` | `<n>` | `<status>` | SPEC-ONLY | yes | `logs/W-41.log` | `<notes>` |
| W-42 | `cargo test -p mcao-cli --locked --no-fail-fast --test audit_writer_parity` | 0 (`#![cfg(unix)]`) | `<n>` | `<n>` | `<n>` | `<status>` | SPEC-ONLY | yes | `logs/W-42.log` | `<notes>` |
| W-43 | `cargo test -p mcao-cli --locked --no-fail-fast --test hook_onedrive_guard` | 1 | `<n>` | `<n>` | `<n>` | `<status>` | SPEC-ONLY | yes | `logs/W-43.log` | `<notes>` |
| W-44 | `cargo test -p mcao-cli --locked --no-fail-fast --test onedrive_guard` | 6 | `<n>` | `<n>` | `<n>` | `<status>` | SPEC-ONLY | yes | `logs/W-44.log` | `<notes; the unset-home text is not checked on Windows>` |
| W-45 | `cargo test -p mcao-cli --locked --no-fail-fast --test hook_host_mirror` | 13 | `<n>` | `<n>` | `<n>` | `<status>` | SPEC-ONLY | yes | `logs/W-45.log` | `<notes>` |
| W-46 | `cargo test -p mcao-cli --locked --no-fail-fast --test audit_baseline_parity` | 0 (`#![cfg(unix)]`) | `<n>` | `<n>` | `<n>` | `<status>` | SPEC-ONLY | yes | `logs/W-46.log` | `<notes>` |
| W-47 | `mcao.exe audit append`, `audit verify`, `audit project-ledger` with a missing `--store` | not a Cargo test | n/a | `<n> / <n> / <n>` | `<n>` | `<status>` | SPEC-ONLY | yes | `logs/W-47-append.log`, `logs/W-47-verify.log`, `logs/W-47-project-ledger.log` | `<notes; exact stderr line; store directory absent afterwards>` |
| W-48 | `mcao.exe audit validate` with FM_HOME and FM_STATE_OVERRIDE unset, scratch LOCALAPPDATA: case A (state directory present), case B (absent) | not a Cargo test | n/a | `<n> / <n>` | `<n>` | `<status>` | SPEC-ONLY | yes | `logs/W-48-a.log`, `logs/W-48-b.log` | `<notes; stdout line; rejection file present in A and absent in B>` |
| W-49 | `cargo test -p mcao-backend-windows --locked --test interrupt escalation_sends_a_second_ctrl_c_then_terminates_the_tree_at_the_wait -- --exact`, 20 runs | 1 per run | `<passes> / 20` | `<n>` per run | `<n>` | `<status>` | UNRESOLVED (rerun) | yes | `logs/W-49-rerun.txt`, `W-49-summary.txt`, `rows.tsv` (W-49-run lines) | `<notes; passes, failures, count of run TIMEOUTs, row TIMEOUT if the 30-minute budget ended the loop before run 20, panic text of each failure>` |

## 4. Acceptance coverage (for the owner)

Record the result per Jira criterion, using the mapping in `windows-native-verification.md` section 6. Use `covered by PASS rows` only when every named row is `PASS`. Otherwise write `partial`, `unresolved`, or `not run`.

| Work item | Criterion | Rows | Result | Reason if not fully covered |
| --- | --- | --- | --- | --- |
| DPES-752 | AC1 (partial) | W-02, W-03, W-27, W-28, W-29 | `<covered / partial / not run>` | `<reason>` |
| DPES-752 | AC3 | W-21, W-49 | `<covered / partial / unresolved / not run>` | `<reason>` |
| DPES-752 | AC4 (partial) | W-04, W-18 | `<...>` | `<...>` |
| DPES-753 | writer refusal on Windows | W-41, W-42, W-46, W-47 | `<...>` | `<...>` |
| DPES-754 | AC2 | W-34 | `<...>` | `<...>` |
| DPES-754 | AC3 | W-35 | `<...>` | `<...>` |
| DPES-754 | AC5 | W-31, W-32, W-38 | `<...>` | `<...>` |
| DPES-755 | AC1, AC2, AC3 (partial) | W-19 to W-21, W-49 | `<...>` | `<...>` |
| DPES-755 | AC5 | W-18 | `<...>` | `<...>` |
| DPES-759 | AC1, AC2, AC4 (partial), AC5 | W-04, W-05, W-06, W-29 | `<...>` | `<...>` |
| DPES-883 | AC1-AC6, AC7-AC9, AC22 (partial) | W-23 to W-26 | `<...>` | `<...>` |
| DPES-884 | validator on Windows | W-17, W-48 | `<...>` | `<...>` |
| DPES-885 | AC1-AC9 (partial, frame level) | W-22, W-23 | `<...>` | `<...>` |
| DPES-889 | AC1, AC2, AC3, AC5, AC6, AC7, AC10 (partial) | W-07, W-08, W-09, W-40 | `<...>` | `<...>` |
| DPES-892 | AC3 (registry, partial), AC4, AC5, AC8 (partial) | W-10, W-11, W-13, W-37, W-37b, W-38, W-43, W-44 | `<...>` | `<...>` |
| DPES-892 | UNC home (owner decision, not yet implemented) | W-36 | `<recorded only>` | `<the warning and exit 0 are not implemented at this packet's commit>` |

Not covered by this packet, by design: DPES-752 AC2, AC5 (owner decisions); DPES-754 AC1 and AC4 second half (managed install and clean-host proof); DPES-755 AC4 (reconnect); DPES-883 AC10-AC21 live attach and `mcao peek`; DPES-885 live keystrokes, resize, and the Windows steer end-to-end test; DPES-889 AC11 (Unix side); DPES-892 AC1, AC2, AC6, AC7, AC9. Any claim beyond the coverage above is not supported by this run.

## 5. Full logs

Attach each row's log under `logs\`, one file per row, named as in section 3. Confirm the count:

- Rows run: `<n>`
- Rows PASS: `<n>`
- Rows FAIL (including FAIL (vacuous)): `<n>`
- Rows SPEC-ONLY observed: `<n>`
- Rows TIMEOUT: `<n>`
- Rows SKIPPED: `<n>` (list each with its reason below)

Skipped rows and reasons:

- `<ID>: <reason>`

## 6. Anomalies and questions

List every unexpected output, every failure that is not in the expected list, every timeout, every proxy or download block, every line that looked like a credential prompt, every `running 0 tests` line from W-06, including those from doc-test and `cfg`-gated binaries (the packet's W-06 row lists each one as an anomaly), and every question for the owner. Give the row ID, the exact error text (redacted), and what was done. Do not interpret causes beyond the log. A `running 0 tests` line from W-41, W-42, or W-46 is the expected result for those rows and is not an anomaly.

- `<ID>: <exact error line, redacted> - <what was done>`
- Questions for the owner: `<question or none>`

If there are no anomalies, write `none`.

## 7. Packaging and publish

- Zip path (full, user name redacted): `<path to <RUN_ID>.zip in %LOCALAPPDATA%\mcao-winverify>`
- Zip size: `<bytes>`
- The zip was created with `Compress-Archive` from the run folder only: `<yes | no>`
- The run folder was redacted before zipping: `<yes | no>`
- Published to `windows-verification` under `results/` only: `<files list>`, commit `<sha>`
- Publish attempts: `<n>`; any push error with credentials removed: `<none | text>`
- No commit or push outside `results/`, no pull request, no tag, no branch, no Jira action: `<yes | no>`
- The test checkout has no modified tracked files after the run (`git status --porcelain`): `<empty | list>`
