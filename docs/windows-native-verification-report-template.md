# Native Windows verification report

Fill this file as `SUMMARY.md` in the results folder. Replace every `<...>` placeholder. Write `SKIPPED` rows with their reason. Do not delete a row. Do not add a PASS that the logs do not show. Companion packet: [`windows-native-verification.md`](windows-native-verification.md).

Redact before saving: Windows user name as `<user>`, machine name as `<host>`, OneDrive profile paths as `<onedrive>`, and any token, key, or password as `<redacted>`. Do not paste environment dumps.

## 1. Run identity

- Run ID: `<run-id>`
- Branch tested (`BRANCH`): `<branch>`
- Pinned base checked out (must equal `1086480770dc606b1a6fdc2832aa36a62e8f79f4`): `<full sha from git rev-parse HEAD>`
- Commit tested: `<same as the pinned base; full sha>`
- `git status --porcelain` on the clone after the pinned checkout: `<empty | list of lines>`
- `git status --porcelain` on the clone after the last row: `<empty | list of lines>`
- Clone path (user name redacted): `<path>`
- Results folder path (user name redacted): `<path>`
- Start and end time (local): `<start>` / `<end>`
- Operator who ran the packet: `<name or role>`
- Shell used for every row: `<pwsh <version> | powershell.exe <version>>`
- `gh` on PATH for the preflight rows: `<real gh | stub gh.exe in results folder | absent (W-33 SKIPPED)>`

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
| Authenticode status of `rustup-init.exe` (information only) | `<status>` |
| rustup-init SHA-256 matched the published value | `<yes | no | not published>` |
| Line endings (section 2.4): `crlf-text` and `lf-text` counts | `<crlf-text=n lf-text=n>` |

## 3. Summary table

One row per matrix row in `windows-native-verification.md` section 4 (W-01 to W-40, with W-39 as a reference line). Status values: `PASS`, `FAIL`, `FAIL (vacuous)`, `SKIPPED`, `KNOWN-FAILURE`. `FAIL (vacuous)` is a row whose log shows no `running N tests` line with N of at least 1, while the row expects tests. Use `KNOWN-FAILURE` only when the packet names the failure as expected for that row and the log shows that failure. The "Expected tests" column is copied from the packet; the "Tests run" column is what the log shows (`running N tests` lines summed, or `n/a` for non-Cargo rows).

| ID | Command | Expected tests | Tests run | Exit code | Duration (s) | Status | Evidence file | Notes |
| --- | --- | --- | --- | --- | --- | --- | --- | --- |
| W-01 | `rustup show active-toolchain`; `rustc -Vv` | n/a | n/a | `<n>` | `<n>` | `<status>` | `logs/W-01.log` | `<notes>` |
| W-02 | `cargo build -p mcao-cli --locked --bin mcao` | n/a | n/a | `<n>` | `<n>` | `<status>` | `logs/W-02.log` | `<notes>` |
| W-03 | `cargo build -p mcao-probe --locked --target x86_64-pc-windows-msvc` | n/a | n/a | `<n>` | `<n>` | `<status>` | `logs/W-03.log` | `<notes>` |
| W-04 | `cargo check --workspace --locked --target x86_64-pc-windows-msvc` | n/a | n/a | `<n>` | `<n>` | `<status>` | `logs/W-04.log` | `<notes>` |
| W-05 | `cargo clippy --workspace --all-targets --locked -- -D warnings` | n/a | n/a | `<n>` | `<n>` | `<status>` | `logs/W-05.log` | `<notes>` |
| W-06 | `cargo test --workspace --locked` | at least 1 per binary | `<n>` | `<n>` | `<n>` | `<status>` | `logs/W-06.log` | `<notes; list running 0 tests lines>` |
| W-07 | `cargo test -p mcao-core --locked --test error a_task_id_refused_at_creation` | 1 | `<n>` | `<n>` | `<n>` | `<status>` | `logs/W-07.log` | `<notes>` |
| W-08 | `cargo test -p mcao-core --locked --test state_encoding` | at least 1 | `<n>` | `<n>` | `<n>` | `<status>` | `logs/W-08.log` | `<notes>` |
| W-09 | `cargo test -p mcao-core --locked --test home` | at least 1 | `<n>` | `<n>` | `<n>` | `<status>` | `logs/W-09.log` | `<notes>` |
| W-10 | `cargo test -p mcao-core --locked --test windows_registry` | at least 1 | `<n>` | `<n>` | `<n>` | `<status>` | `logs/W-10.log` | `<notes>` |
| W-11 | `cargo test -p mcao-core --locked --test preflight` | at least 1 | `<n>` | `<n>` | `<n>` | `<status>` | `logs/W-11.log` | `<notes>` |
| W-12 | `cargo test -p mcao-core --locked --lib preflight` | at least 1 | `<n>` | `<n>` | `<n>` | `<status>` | `logs/W-12.log` | `<notes>` |
| W-13 | `cargo test -p mcao-cli --locked --test preflight` | at least 1 | `<n>` | `<n>` | `<n>` | `<status>` | `logs/W-13.log` | `<notes>` |
| W-14 | `cargo test -p mcao-cli --locked --test cli` | at least 1 | `<n>` | `<n>` | `<n>` | `<status>` | `logs/W-14.log` | `<notes>` |
| W-15 | `cargo test -p mcao-cli --locked --test toolchain` | at least 1 | `<n>` | `<n>` | `<n>` | `<status>` | `logs/W-15.log` | `<notes>` |
| W-16 | `cargo test -p mcao-cli --locked --test herdr_lab_viewer` | at least 1 | `<n>` | `<n>` | `<n>` | `<status>` | `logs/W-16.log` | `<notes>` |
| W-17 | `cargo test -p mcao-cli --locked --test audit_validate --test audit_activity_map` | at least 1 per binary | `<n>` | `<n>` | `<n>` | `<status>` | `logs/W-17.log` | `<notes; ok-line count; zone-bearing cases skipped by cfg>` |
| W-18 | `cargo check --workspace --locked --target x86_64-unknown-linux-gnu`, then `--target aarch64-apple-darwin` | n/a | n/a | `<n>` / `<n>` | `<n>` / `<n>` | `<status>` | `logs/W-18-linux.log`, `logs/W-18-darwin.log` | `<notes>` |
| W-19 | `cargo test -p mcao-backend-windows --locked --test job` | at least 1 | `<n>` | `<n>` | `<n>` | `<status>` | `logs/W-19.log` | `<notes>` |
| W-20 | `cargo test -p mcao-backend-windows --locked --test backend` | at least 1 | `<n>` | `<n>` | `<n>` | `<status>` | `logs/W-20.log` | `<notes>` |
| W-21 | `cargo test -p mcao-backend-windows --locked --test interrupt` | at least 1 | `<n>` | `<n>` | `<n>` | `<status>` | `logs/W-21.log` | `<notes; failing test names; gone-after-ladder yes/no>` |
| W-22 | `cargo test -p mcao-backend-windows --locked --test conpty` | at least 1 | `<n>` | `<n>` | `<n>` | `<status>` | `logs/W-22.log` | `<notes; console host type>` |
| W-23 | `cargo test -p mcao-backend-windows --locked --test console_pipe` | at least 1 | `<n>` | `<n>` | `<n>` | `<status>` | `logs/W-23.log` | `<notes>` |
| W-24 | `cargo test -p mcao-backend-windows --locked --test pipe` | at least 1 | `<n>` | `<n>` | `<n>` | `<status>` | `logs/W-24.log` | `<notes>` |
| W-25 | `cargo test -p mcao-backend-windows --locked --lib console_server` | 13 (host list) | `<n>` | `<n>` | `<n>` | `<status>` | `logs/W-25.log` | `<notes>` |
| W-26 | `cargo test -p mcao-backend-windows --locked --lib` | 18 (host list) | `<n>` | `<n>` | `<n>` | `<status>` | `logs/W-26.log` | `<notes>` |
| W-27 | `cargo test -p mcao-probe --locked --test native` | at least 1 | `<n>` | `<n>` | `<n>` | `<status>` | `logs/W-27.log` | `<notes>` |
| W-28 | probe via `bash-free-probe.ps1` (child shell, bash-free PATH): `mcao-probe.exe --backend native --harness stub --scenario cooperative` | not a Cargo test | n/a | `<n>` | `<n>` | `<status>` | `logs/W-28.log`, `probe-evidence.json`, `bash-free-path.json` | `<notes; per-operation support, in order spawn, capture, send_text, interrupt, agent_state, reconnect, cleanup>` |
| W-29 | Bash-free PATH counts from `bash-free-path.json` (`bash`, `sh`, `wsl`) | not a Cargo test | n/a | `<n/a>` | `<n>` | `<status>` | `bash-free-path.json` | `<counts only: removed_directories, kept_directories, names found>` |
| W-30 | `cargo test -p mcao-probe --locked --test platform_leakage` | 12 (host list) | `<n>` | `<n>` | `<n>` | `<status>` | `logs/W-30.log` | `<notes>` |
| W-31 | `mcao.exe --version` | not a Cargo test | n/a | `<n>` | `<n>` | `<status>` | `logs/W-31.log` | `<notes>` |
| W-32 | `mcao.exe --help` | not a Cargo test | n/a | `<n>` | `<n>` | `<status>` | `logs/W-32.log` | `<notes>` |
| W-33 | `mcao.exe preflight` (Git and real gh on PATH) | not a Cargo test | n/a | `<n>` | `<n>` | `<status>` | `logs/W-33.log` | `<notes; Gh line as printed; SKIPPED reason if gh absent>` |
| W-34 | `mcao.exe preflight` with Git removed from PATH (child, `-ExpectGitMissing`) | not a Cargo test | n/a | `<n>` | `<n>` | `<status>` | `logs/W-34.log` | `<notes; exact Git line>` |
| W-35 | `mcao.exe preflight` with spaces and non-ASCII FM_HOME | not a Cargo test | n/a | `<n>` | `<n>` | `<status>` | `logs/W-35.log` | `<notes; exact path line, user name redacted; gh stub or real gh>` |
| W-36 | `mcao.exe preflight` with UNC FM_HOME | not a Cargo test | n/a | `<n or n/a>` | `<n>` | `<status>` | `logs/W-36.log` | `<notes or SKIPPED reason>` |
| W-37 | `mcao.exe preflight` with OneDrive root, for `OneDrive`, `OneDriveCommercial`, `OneDriveConsumer` | not a Cargo test | n/a | `<n> / <n> / <n>` | `<n>` | `<status>` | `logs/W-37-OneDrive.log`, `logs/W-37-OneDriveCommercial.log`, `logs/W-37-OneDriveConsumer.log` | `<notes; OneDrive line; expected exit 3 for each>` |
| W-37b | `mcao.exe preflight` with a profile sync folder (warning only) | not a Cargo test | n/a | `<n>` | `<n>` | `<status>` | `logs/W-37b.log` | `<notes; warning line; expected exit 0>` |
| W-38 | `cargo test -p mcao-core --locked --test preflight the_default_home` | 2 | `<n>` | `<n>` | `<n>` | `<status>` | `logs/W-38.log` | `<notes>` |
| W-39 | Reference only: W-13 covers the overlong-home test | n/a | n/a | `<n/a>` | `<n/a>` | `<status>` | none | `<notes>` |
| W-40 | `cargo test -p mcao-core --locked --test state_encoding a_new_task_id_with_uppercase` | 1 | `<n>` | `<n>` | `<n>` | `<status>` | `logs/W-40.log` | `<notes>` |

## 4. Acceptance coverage (for the owner)

Record the result per Jira criterion, using the mapping in `windows-native-verification.md` section 6. Use `covered by PASS rows` only when every named row is `PASS` or `KNOWN-FAILURE` with the expected failure shown. Otherwise write `partial` or `not run`.

| Work item | Criterion | Rows | Result | Reason if not fully covered |
| --- | --- | --- | --- | --- |
| DPES-752 | AC1 (partial) | W-02, W-03, W-27, W-28, W-29 | `<covered / partial / not run>` | `<reason>` |
| DPES-752 | AC4 (partial) | W-04, W-18 | `<...>` | `<...>` |
| DPES-754 | AC2 | W-34 | `<...>` | `<...>` |
| DPES-754 | AC3 | W-35 | `<...>` | `<...>` |
| DPES-754 | AC5 | W-31, W-32, W-38 | `<...>` | `<...>` |
| DPES-755 | AC1, AC2, AC3 (partial) | W-19 to W-21 | `<...>` | `<...>` |
| DPES-755 | AC5 | W-18 | `<...>` | `<...>` |
| DPES-759 | AC1, AC2, AC4 (partial), AC5 | W-04, W-05, W-06, W-29 | `<...>` | `<...>` |
| DPES-883 | AC1-AC6, AC7-AC9, AC22 (partial) | W-23 to W-26 | `<...>` | `<...>` |
| DPES-885 | AC1-AC9 (partial, frame level) | W-22, W-23 | `<...>` | `<...>` |
| DPES-889 | AC1, AC2, AC3, AC5, AC6, AC7, AC10 (partial) | W-07, W-08, W-09, W-40 | `<...>` | `<...>` |
| DPES-892 | AC3 (registry, partial), AC4, AC5, AC8 (partial) | W-10, W-11, W-13, W-37, W-37b, W-38 | `<...>` | `<...>` |

Not covered by this packet, by design: DPES-752 AC2, AC3, AC5 (owner decisions); DPES-754 AC1 and AC4 second half (managed install and clean-host proof); DPES-755 AC4 (reconnect); DPES-883 AC10-AC21 live attach and `mcao peek`; DPES-885 live keystrokes, resize, and the Windows steer end-to-end test; DPES-889 AC11 (Unix side); DPES-892 AC1, AC2, AC6, AC7, AC9. Any claim beyond the coverage above is not supported by this run.

## 5. Full logs

Attach each row's log under `logs\`, one file per row, named as in section 3. Confirm the count:

- Rows run: `<n>`
- Rows PASS: `<n>`
- Rows FAIL (including FAIL (vacuous)): `<n>`
- Rows KNOWN-FAILURE: `<n>`
- Rows SKIPPED: `<n>` (list each with its reason below)

Skipped rows and reasons:

- `<ID>: <reason>`

## 6. Anomalies and questions

List every unexpected output, every failure that is not in the expected list, every timeout, every proxy or download block, every line that looked like a credential prompt, every `running 0 tests` line from W-06, every CRLF text file from section 2.4, and every question for the owner. Give the row ID, the exact error text (redacted), and what was done. Do not interpret causes beyond the log.

- `<ID>: <exact error line, redacted> - <what was done>`
- Questions for the owner: `<question or none>`

If there are no anomalies, write `none`.

## 7. Packaging

- Zip path (full, user name redacted): `<path to <run-id>.zip>`
- Zip size: `<bytes>`
- The zip was created with `Compress-Archive` from the results folder only: `<yes | no>`
- No file was copied outside the results folder, and nothing was uploaded: `<yes | no>`
- No commit, push, pull request, or Jira action was taken: `<yes | no>`
- The clone has no modified tracked files after the run (`git status --porcelain`): `<empty | list>`

Hand the operator the zip path only. The operator decides where it goes.
