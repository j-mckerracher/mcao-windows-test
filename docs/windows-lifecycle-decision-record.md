# Windows lifecycle decision record

This record covers the DPES-752 decisions for native Windows support in the Rust slice. It states what merged code and Jira facts establish, cites the evidence, and lists each decision that still needs an owner choice or a native run as `OPEN` with its options. It does not claim that native Windows or Claude Code behavior was exercised.

Evidence basis: `origin/DPES-751` at commit `49d7d24594a061181fae2e5326c5b26227ce909c`. File references are repository paths at that commit, except citations to `platform_leakage.rs` and the `mcao-backend-unix` manifest, which are at `79d38415`, the merged correction. Jira facts come from DPES-752 (requirements in section 5, acceptance criteria in section 7) and its comments dated 2026-10-23, 2026-10-23 13:22, 2026-10-26, 2026-11-01 12:09, 2026-11-01 14:56 and 2026-11-01 15:48. The 14:56 comment is an owner decision on the two platform-leakage residuals, recorded in section 10. Citations in sections 2 to 10 were re-read on `origin/DPES-751` at `257276e0`, the tip used for the section 9 decisions; each cited line resolves there.

Status: AC1 is not met. AC2 and AC5 are partly recorded; the open items are listed in section 11.

## 1. Status vocabulary

- `Confirmed`: merged code or CI configuration supports the claim as written.
- `Recorded`: a decision is written in merged code or in a dated owner decision in Jira. No native runtime proof exists.
- `Rejected`: a rejection is recorded in merged code, in the DPES-752 requirements, or by owner decision. A deferral alone is not a rejection; a deferred item is `OPEN`.
- `OPEN`: no decision is recorded. The options are listed, and an owner choice or an evidence run is needed.

## 2. Toolchain and targets

| Item | Status | Evidence |
|---|---|---|
| Stable toolchain pinned to `1.95.0`, profile `minimal`, components `rustfmt` and `clippy` | Recorded | `rust-toolchain.toml`; scaffold commit `023d78f2` |
| Workspace edition `2024` | Confirmed | `Cargo.toml` `[workspace.package]` |
| Pin rationale (runner availability, release cadence, DPES-752 section 5 item 1) | OPEN | Not written in the repository. Jira comment 2026-10-23 lists "toolchain pin rationale" as an unfiled follow-up. |
| `x86_64-pc-windows-msvc` as the Windows target | Confirmed | Jira section 10 records MSVC as confirmed user direction. `.github/workflows/rust-workspace-check.yml:63-64` checks it on `windows-latest`. |
| `aarch64-apple-darwin` and `x86_64-unknown-linux-gnu` as Unix targets | Confirmed | `.github/workflows/rust-workspace-check.yml:60,62,67` |
| `x86_64-pc-windows-gnu` | Rejected | DPES-752 section 5 item 2 records it as rejected (MinGW dependency incompatible with the MSVC-only CI environment). It is absent from the CI target list. |

CI runs `cargo check --workspace --locked --target <target>` for each pinned target (`.github/workflows/rust-workspace-check.yml:120-138`). This record does not restate CI results.

## 3. Terminal mechanism

Status: `Recorded` for the ConPTY path. Native behavior is not proven.

- The recorded mechanism is direct `windows-sys` ConPTY calls. The module header of `crates/mcao-backend-windows/src/conpty.rs:9-27` rejects `portable-pty` for three reasons: the child starts before it can be assigned to a Job Object (no `CREATE_SUSPENDED`), `nix` is declared unconditionally, and 13 extra crates plus `winapi` would enter the graph. Those statements about `portable-pty` 0.9.0 are the module author's. `portable-pty` is not in `Cargo.lock`, so they were not re-verified here.
- The probe's Windows backend does not use this mechanism. `crates/mcao-probe/src/native/windows.rs` starts the stub with no terminal. Its capture, send-text, interrupt and reconnect operations return `Unsupported` (`native/windows.rs:135,140,145,162`). `WindowsBackend` in `crates/mcao-backend-windows/src/backend.rs` has no consumer outside its own tests.
- Jira comment 2026-10-26 says the lifecycle probe was "wired to native backends, with a Windows run" (PRs 266, 267, 271). Merged code at this commit does not show that for the probe's Windows path. This record follows the code.

## 4. Process-tree ownership (Job Objects)

Status: `Recorded` as the implementation candidate in merged code. Native validation is `OPEN`. Jira section 5 item 8 asks to accept or defer the Job Object approach on a native run, and Jira section 10 lists "Job Objects own descendant trees and survive agent restart" as an unconfirmed assumption.

- `crates/mcao-backend-windows/src/process.rs:25,334` starts the child with `CREATE_SUSPENDED` through `CreateProcessW` (`process.rs:360`), so the child is assigned to a `JobObject` (`crates/mcao-backend-windows/src/job.rs:45`) before it runs. `conpty.rs:14-19` describes the same order.
- `TerminateJobObject` is the last interrupt rung (`crates/mcao-backend-windows/src/interrupt.rs:1-19`).
- No native run has shown grandchild containment or job teardown. Jira section 5 item 8 requires that run.

## 5. Lock ownership proof (HolderProbe)

Status: `OPEN`.

- Unix: `UnixHolderProbe` uses `lsof` (`crates/mcao-backend-unix/src/holder.rs:16,20`).
- Core contract: `HolderProbe` in `crates/mcao-core/src/lock/probe.rs:21`. The Windows answer is `UnknownHolderProbe` (`probe.rs:30-36`), which returns `Unknown` for every lock, so no lock is proven stale through it.
- `crates/mcao-backend-windows/src/lib.rs:11` states that no lock-holder code exists yet.
- Options from DPES-752 section 5: (a) Windows handle enumeration through `windows-sys`; (b) named-pipe per-opener tracking, which Jira says needs an architectural change. Jira also allows recording the item as "unresolved, blocked on further investigation". No option is chosen.

## 6. Herdr control-socket transport on Windows

Status: `OPEN`.

- The Unix herdr client uses `std::os::unix::net::UnixStream` (`crates/mcao-core/src/herdr/control_socket.rs:6`).
- `crates/mcao-backend-windows/src/pipe.rs` is the DPES-883 worker console pipe. It is not the herdr transport.
- Options from DPES-752 section 5: (a) named pipes `\\.\pipe\herdr-<id>`; (b) TCP loopback with port allocation; (c) the `interprocess` crate (row in section 9). Jira asks for one pilot transport and a recorded wire protocol, with JSON-RPC reuse expected. None is chosen.
- Jira lists a Windows herdr binary or buildable source as a known blocker. `.github/workflows/windows-herdr-spike.yml` is a manual measurement workflow, and no tracked document records its results.

## 7. Distribution channel

Status: direction `Recorded` (owner decision 2026-10-23). Signing is `OPEN`.

- Recorded, Jira section 5 item 4: signed GitHub Releases binaries are the primary direction. End users do not need a Rust toolchain. `cargo install --git` is a developer option only. winget is deferred from the initial delivery.
- `OPEN`: the organization-approved signing mechanism and its owner. Jira authorizes an internal inquiry only. It authorizes no unsigned distribution exception and no certificate purchase.
- `OPEN`: prerequisites (repository and download access on managed pilot laptops, installation permissions, signature verification method) and the update flow. Jira states that none is assumed to exist.
- No release packaging configuration exists in the workspace. No `Cargo.toml` declares `cargo-dist` metadata.

## 8. Pilot harness and interruption

Pilot harness: `Recorded`. Claude Code is the harness for the first native proof (owner decision 2026-10-23, a pilot assumption, Jira section 5 item 10). Other harnesses follow after the first proof.

- `OPEN`: the pinned Claude Code version for Windows, and its Windows version constraints.
- `OPEN`: the credential route for a live run. Jira states that a personal Claude subscription is not used. Until a live run with an approved route, live lifecycle and Escape behavior stay unverified.
- Probe state: `crates/mcao-probe/src/args.rs:8` accepts `--backend mock|native` and `--harness mock|stub`. No pilot harness and no herdr backend is wired.

Interruption timeouts: `OPEN`. No value is measured.

- Ladder rungs in order: single Escape, extended wait, Ctrl+C, `CTRL_BREAK_EVENT`, `TerminateJobObject` (`crates/mcao-backend-windows/src/interrupt.rs:1-19`).
- `LadderTimeouts` has no defaults, and its `validate` refuses zero or unbounded waits (`interrupt.rs` header). Jira section 5 item 11 requires each value to be measured on the Windows runner.
- `crates/mcao-backend-windows/src/backend.rs:22` sets `INTERRUPT_WAIT` to 500 ms and labels it a DPES-755 proposal, not a measurement. `MAX_RUNG_WAIT` (600 s, `interrupt.rs:48`) is an upper bound, not a measured value.

## 9. Crate confirmation table

Source list: DPES-752 section 5, "PROPOSED crate confirmation and rejection", items (a) to (l). Jira refers to "Appendix C", but neither the item body nor its comments contain that appendix. The rows below follow section 5 and add the three crates that are in the merged graph but not in that list (`sha2`, `libc`, `toml`).

Counts for the 21 section 5 names: `Confirmed` 4 (clap, serde, serde_json, windows-sys), `Rejected` 5 (portable-pty, tempfile, fd-lock, fs2, nix), `OPEN` 12 (sysinfo, interprocess, imap, lettre, rustls, thiserror, tracing, assert_cmd, predicates, insta, cargo-dist, winget). The table has 24 rows: `Confirmed` 7, `Rejected` 5, `OPEN` 12. The GNU target is also `Rejected` (section 2), but it is a target, not a crate row.

The four `Rejected` rows added for tempfile, fd-lock, fs2 and nix are firstmate decisions under the owner's delegation of the remaining DPES-752 decisions. No DPES-752 comment records them, and section 1 lists merged code, the requirements, or an owner decision as the basis for `Rejected`. The owner should confirm that basis.

| Crate | Jira item | Status | Reason | Evidence |
|---|---|---|---|---|
| clap | (a) | Confirmed | mcao-cli builds its command surface with clap derive at the version set in workspace dependencies. | `Cargo.toml` workspace `clap = 4.6.7`; `crates/mcao-cli/src/cli.rs:5`; `Cargo.lock` |
| serde | (b) | Confirmed | Evidence records in mcao-core derive `Serialize`. | `crates/mcao-core/src/evidence.rs:3,6` |
| serde_json | (b) | Confirmed | mcao-core canonical JSON and audit code, mcao-cli audit code, and the Unix herdr client use it. | `crates/mcao-core/src/canonical_json.rs`; `crates/mcao-cli/src/audit.rs`; `crates/mcao-backend-unix/src/herdr.rs:21` |
| tempfile | (c) | Rejected | Rejected for now, by firstmate decision under the owner's delegation: not in `Cargo.lock` or any manifest, and merged state writes use std `create_new` and `rename`, so no dependency is added. Revisit only on a demonstrated failure of the std path. The atomicity test in Jira item (c) was not run because no crate is adopted. | `Cargo.lock`; `crates/mcao-core/src/state/store.rs:315,341` |
| fd-lock | (c) | Rejected | Rejected for now, by firstmate decision under the owner's delegation: not in `Cargo.lock` or any manifest, and the merged sidecar lock uses std `File::lock`, so no dependency is added. Revisit only on a demonstrated failure of the std path. The lock-semantics comparison in Jira item (c) was not run. | `Cargo.lock`; `crates/mcao-core/src/state/lock.rs:46-52` |
| fs2 | (c) | Rejected | Rejected for now, by firstmate decision under the owner's delegation: not in `Cargo.lock` or any manifest; the same std `File::lock` path applies, so no dependency is added. Revisit only on a demonstrated failure of the std path. | `Cargo.lock`; `crates/mcao-core/src/state/lock.rs:52` |
| portable-pty | (d) | Rejected | The module header rejects it for job-ownership ordering, unconditional `nix`, 13 extra crates and `winapi`; those upstream claims are not re-verified here because the crate is not in `Cargo.lock`. | `crates/mcao-backend-windows/src/conpty.rs:9-27` |
| windows-sys | (d), (f) | Confirmed | Used directly for ConPTY, Job Object, pipe, process and registry APIs, gated to `cfg(windows)`; ConPTY runtime behavior on a native Windows run is not yet evidenced. | `Cargo.toml` workspace `windows-sys`; `crates/mcao-backend-windows/src/conpty.rs`; `crates/mcao-core/src/proc_info.rs:307` |
| sysinfo | (e) | OPEN | Used for host resource sampling. Its `libc` dependency is declared under `cfg(not(any(target_os = "unknown", target_arch = "wasm32")))` (sysinfo 0.39.6 `Cargo.toml:242`, crates.io source), so it is active on Windows; section 10 records that this libc is not by itself leakage. Missing for `Confirmed`: `SUPPORT_RECORDS` has no `sysinfo` entry (it covers `libc`, `windows-sys`, `serde` and `serde_json` only), and the liveness test that Jira item (e) requires has not been run. | `crates/mcao-core/src/resources/sys.rs:11`; `crates/mcao-core/Cargo.toml:15`; sysinfo 0.39.6 `Cargo.toml:242`; `crates/mcao-probe/tests/platform_leakage.rs:75-100`; section 10 |
| nix | (g) | Rejected | Rejected, by firstmate decision under the owner's delegation: `nix` is in no manifest, `Cargo.lock` or source. `mcao-backend-unix` declares no `libc` or `nix`, and Unix syscalls use `libc`. Adding `nix` would need a version-pinned `SUPPORT_RECORDS` entry and a stated reason, because the guard fails a family-gated third-party crate with no entry. | `crates/mcao-backend-unix/Cargo.toml:8-13`; `crates/mcao-core/Cargo.toml:17-20`; `crates/mcao-core/src/resources/sys.rs:203-211`; `crates/mcao-cli/src/herdr_lab_viewer.rs:244,333`; `crates/mcao-probe/tests/platform_leakage.rs:10-13,75-100,540` |
| interprocess | (h) | OPEN | Not in `Cargo.lock` or any manifest. Blocked on the herdr transport choice (section 6), and no transport is chosen; the Unix client still uses `UnixStream`. | section 6; `crates/mcao-core/src/herdr/control_socket.rs:6` |
| imap | (i) | OPEN | Not in `Cargo.lock` or any manifest. Circular: DPES-864 lists imap, lettre and rustls as blocking items from DPES-752, while this decision needs DPES-864's mail transport requirement first (its Windows IMAP/SMTP transport is OPEN per DPES-752). | Jira section 10 "Mail crates"; Jira DPES-864 "BLOCKING items from DPES-752" and "Cross-platform mail transport" |
| lettre | (i) | OPEN | Not in `Cargo.lock` or any manifest. Circular: DPES-864 lists imap, lettre and rustls as blocking items from DPES-752, while this decision needs DPES-864's mail transport requirement first (its Windows IMAP/SMTP transport is OPEN per DPES-752). | Jira section 10 "Mail crates"; Jira DPES-864 "BLOCKING items from DPES-752" and "Cross-platform mail transport" |
| rustls | (i) | OPEN | Not in `Cargo.lock` or any manifest. Circular: DPES-864 lists imap, lettre and rustls as blocking items from DPES-752, while this decision needs DPES-864's mail transport requirement first (its Windows IMAP/SMTP transport is OPEN per DPES-752). | Jira section 10 "Mail crates"; Jira DPES-864 "BLOCKING items from DPES-752" and "Cross-platform mail transport" |
| thiserror | (j) | OPEN | Not in `Cargo.lock` or any manifest, and no evidence run exists. | `Cargo.lock` |
| tracing | (j) | OPEN | Not in `Cargo.lock` or any manifest, and no evidence run exists. | `Cargo.lock` |
| assert_cmd | (k) | OPEN | Not in `Cargo.lock` or any manifest, and no evidence run exists. | `Cargo.lock` |
| predicates | (k) | OPEN | Not in `Cargo.lock` or any manifest, and no evidence run exists. | `Cargo.lock` |
| insta | (k) | OPEN | Not in `Cargo.lock` or any manifest, and no evidence run exists. | `Cargo.lock` |
| cargo-dist | (l) | OPEN | Not in any manifest. Release tooling depends on the open signing mechanism in section 7. | section 7 |
| winget | (l) | OPEN | Jira section 5 item 4 says "winget is deferred from the initial delivery" (owner decision 2026-10-23). A deferral is not a rejection, so the row stays OPEN. Options: the owner records a rejection, or keeps winget as a later channel. No winget packaging is merged. | Jira section 5 item 4 |
| sha2 | not in list | Confirmed | Pinned to `=0.10.9` because the audit hash chain uses this SHA-256. | `Cargo.toml` workspace comment; `crates/mcao-core/src/sha256.rs:6` |
| libc | not in list | Confirmed | Declared under `cfg(unix)` for the macOS sysctl in mcao-core and for openpty, fork and the signal calls of `mcao herdr lab-viewer` in mcao-cli. Its presence on Windows through sysinfo is section 10. | `crates/mcao-core/Cargo.toml` `[target.'cfg(unix)'.dependencies]`; `crates/mcao-cli/Cargo.toml` |
| toml | not in list | Confirmed | mcao-cli parses the Kimi configuration with it. | `crates/mcao-cli/Cargo.toml`; `crates/mcao-cli/src/util/kimi.rs:248` |

## 10. Platform leakage (owner decision recorded and implemented)

Status: `Recorded`, from the owner decision in Jira comment 2026-11-01 14:56. The correction is merged into `origin/DPES-751` as `79d38415` (PR 418), and this branch includes it. It does not close AC1.

Before that correction, at `49d7d245`, `cargo test --locked -p mcao-probe --test platform_leakage` gave 7 passed, 2 failed:

- `every_product_graph_excludes_the_other_platform_family`: `libc leaked into the mcao-cli graph on x86_64-pc-windows-msvc`.
- `platform_only_package_graph_excludes_its_family_on_the_other_platform`: `mcao-backend-unix declares third-party serde without gating it to Unix`.

At `79d38415` the same command gives 12 passed, 0 failed, on macOS (`cargo test --locked -p mcao-probe --test platform_leakage`, exit 0). Both previously failing tests now pass. The run used this branch's merge commit, whose `crates/` tree is identical to `79d38415`.

Owner decision (Jira comment 2026-11-01 14:56, DPES-752):

- Evaluate the target-selected dependency graph and each dependency's real platform support. A Unix-gated workspace declaration does not prove a dependency is Unix-only.
- `libc` in the Windows graph is not by itself leakage. It reaches the graph through sysinfo: `cargo tree --offline --locked --manifest-path Cargo.toml -p mcao-core --target x86_64-pc-windows-msvc -e normal -i libc` shows `libc v0.2.191`, then `sysinfo v0.39.6`, then `mcao-core`. The inspected libc version supports Windows, and sysinfo selects it.
- `serde` and `serde_json` in `crates/mcao-backend-unix/Cargo.toml` must be gated to the Unix target. A source-level `cfg(unix)` does not gate Cargo dependencies.
- The guard is corrected, not waived. Its classification and declaration-gate handling change, and checks for genuine leakage stay. No blanket crate-name exemption, and no weakening only to turn the suite green. The AC and guard documentation are amended to match. The corrected guard must be shown to detect real violations, not only to pass.
- Both failing tests stayed failing until that correction landed. PR 418 (`fm/dpes752-guard-correction`) implemented it and is merged as `79d38415`. The change is `serde` and `serde_json` under `[target.'cfg(unix)'.dependencies]` in `crates/mcao-backend-unix/Cargo.toml:11-13`, plus the guard in `crates/mcao-probe/tests/platform_leakage.rs`. This record changes no Rust, Cargo, or guard code.

What the merged guard does (`platform_leakage.rs` at `79d38415`):

- `SUPPORT_RECORDS` (`:75-100`) pins the real platform support of each third-party crate that a product gates or declares both ways: `libc` 0.2.191 on both families, `windows-sys` 0.61.2 on Windows, `serde` 1.0.229 and `serde_json` 1.0.154 on both families. A locked version that differs from the record fails the guard (`assert_locked_version`, `:228`).
- A workspace crate takes its family from its declarations (`workspace_families`, `:265`). A third-party crate with mixed gates and no record fails (`third_party_families`, `:212`). A gate other than `cfg(unix)` or `cfg(windows)` fails (`gate_of`, `:277`).
- The module header (`:1-21`) states these rules. The Jira AC text is not in this repository and is not amended or verified here.
- Synthetic tests (`:507-638`) cover the refusal paths and the recorded-libc path.

Residuals, recorded and not closed by the merge (from the PR 418 review; code reading only, not re-run here):

- A workspace backend crate declared only unconditionally (for example `mcao-backend-unix` without the `cfg(unix)` gate in `mcao-probe`) is classified on both families, so the exclusion checks skip it. Only the vacuity assertion in `every_product_graph_excludes_the_other_platform_family` (`:325-330`; each family needs a family-only crate) can catch it, and only when it leaves a family with none.
- A support record matches a crate's name and locked version, not its source. A source change within the same version is not detected.
- An unrecorded unconditional Unix-only third-party crate passes the guard. The backstop is CI's `cargo check --workspace --locked --target x86_64-pc-windows-msvc` (`.github/workflows/rust-workspace-check.yml:137` on Windows, `:126` on other hosts). That check fails on code that does not compile for Windows, not on a crate that compiles there.

AC1 is still open. The macOS run above does not replace the native Windows run that AC1 requires (section 11).

Jira comment 2026-11-01 12:09 reported the same two residuals before that decision.

`nix` is absent from every target in the graph: `cargo tree --offline --locked --workspace --target all -e normal` lists no `nix` package.

## 11. Open items for AC2 and AC5

- AC1 is not met. No native pilot-lifecycle run exists, no pilot harness is wired, and no approved credential route is in place. The `probe-windows` job (`.github/workflows/rust-workspace-check.yml:175-287`) runs the stub harness on `windows-latest` and records capture, send text, interrupt and reconnect as unsupported. It does not run Claude Code. AC1 is outside this record.
- AC2 open items: HolderProbe path (section 5); herdr transport (section 6); terminal mechanism, which is `Recorded` and needs a native run to be confirmed (section 3); Job Object ownership, which is `Recorded` pending a native run and an accept-or-defer decision (section 4); interruption timeouts (section 8); pinned Claude Code version and credential route (section 8); signing mechanism, its owner, prerequisites and update flow (section 7); 12 crate rows (section 9); toolchain pin rationale (section 2).
- AC5 open items: 12 of the 21 section 5 crate rows (section 9). Residual: the `Confirmed` rows cite use sites. Jira section 8 also asks for one API invocation in a passing test per crate, and this record cites no such test.

## 12. Discrepancies

- Jira comment 2026-10-26 says the probe was wired to native backends with a Windows run. Merged code at this commit does not show that (section 3).
- Jira refers to an "Appendix C" crate table that is not present in the item or its comments (section 9).
- Five `Cargo.toml` description fields still read `PROPOSED (DPES-752 unconfirmed)`: mcao-core, mcao-cli, mcao-backend-unix, mcao-backend-windows and mcao-probe. This record does not edit them.
- Jira section 8 cross-compile test (AC4) expects a build error for `mcao-backend-windows` on Linux. The `cfg` design compiles that crate empty on Linux, as Jira comment 2026-10-23 notes.
