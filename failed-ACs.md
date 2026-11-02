# Failed Acceptance Criteria

This document records acceptance criteria that were not implemented or were only partially implemented when a DPES-751 pull request completed its allowed adversarial review cycle.

Entries in this document do not create new implementation work or authorize scope expansion. Each entry identifies the originating Jira item and pull request.

## Entries

### DPES-1005: Close PR369 third-review shell policy positional bypasses

- Jira: [DPES-1005](https://mccdh.atlassian.net/browse/DPES-1005)
- Pull request: [#399](https://github.com/mc-innersource/motif-hve-multi-agent-orchestrator/pull/399)
- Third-review commit: `4cdf5427f5f26af33e9754ef1273e0e083712e42`
- Recorded: 2026-10-31
- Status: Partially Implemented

#### Acceptance criteria not implemented

- Shell-policy protection must deny the relevant protected-command execution paths, including editor and fzf runners and parameter expansion, while allowing benign controls; relevant residual runner and substitution decisions must be recorded.
- The named pretool suite must be completely verified, alongside the specified shell and Rust differential checks, formatting and clippy.

#### Review finding

- Final adversarial review A1: formatted protected command text piped to an editor bypasses both hook prefilter and classifier.
- A2: editor input redirected from a file is allowed by the hook even though the classifier denies it.
- A3: named-variable parameter operators can carry a protected command into `eval`; this also occurred on the reviewed base.
- F19: an fzf execution binding supplied through `FZF_DEFAULT_OPTS` is allowed; this also occurred on the reviewed base. The existing command-substitution and copy/link-then-run residuals remain unaccepted.
- The full unsharded pretool suite was not verified; the review did not establish equivalence of the author's partitioned runs to one complete run.

#### Affected behavior or files

- `bin/fm-arm-pretool-check.sh` and `bin/fm-arm-command-policy.mjs`: editor stdin and prefilter delegation, named-variable expansion, and fzf environment options.
- `tests/fm-arm-pretool-check.test.sh`: full-suite verification and coverage of the identified bypasses.

#### Implemented portion

- The review verified the explicit fzf action argument cases, positional indirection and literal editor-stdin cases on the third-review commit. Shell/Rust differential checks and the focused Rust positional tests passed in the review.

#### Remaining work

- Detect or conservatively reject formatted editor input, file-redirected editor input and protected or unknown named-variable parameter operators, with benign controls. Address or explicitly disposition the fzf environment binding and the existing command-substitution and copy/link residuals.
- Establish a complete, reproducible result for the unsharded pretool suite without treating unverified partitions as an equivalent pass.

#### Known impact

- Protected watcher commands may evade the hook or classifier through the identified forms. The fzf environment binding requires an interactive TTY.

#### Verification status

- Third review: shell wrapper differential passed; Rust `shell_command_pos` passed 19 tests; Rust `arm_wrapper_differential` passed 2 tests. The unsharded pretool suite was not run by the reviewer; earlier runs exceeded the per-command limit. The review did not verify the author's partition equivalence.

### DPES-1006: Close PR401 third-review console cancellation and documentation gaps

- Jira: [DPES-1006](https://mccdh.atlassian.net/browse/DPES-1006)
- Pull request: [#401](https://github.com/mc-innersource/motif-hve-multi-agent-orchestrator/pull/401)
- Third-review commit: `ad38b605159870dc8b60d74df35ba2cba1507132`
- Recorded: 2026-10-31
- Status: Partially Implemented

#### Acceptance criteria not implemented

- Error and cleanup handling must preserve and accurately report the cause of the exit; all seven identified findings must be corrected or documented with evidence. A write-stall close still reports backlog overflow, and native test evidence does not distinguish the stall close from a true backlog overflow.
- Native partial-frame handling and wrong-SID refusal must have the tests or documented evidence specified by the task. Partial-frame cancellation is tested at shutdown, but idle partial-frame reads remain unbounded; wrong-SID refusal has no native test and requires a second Windows user account.

#### Review finding

- F1: the write-stall path returns `client backlog exceeded`, the same error as the backlog cap, so operators cannot identify the actual reason.
- F2: the native flood test asserts that same error and a 30-second bound; it passes whether the write-stall limit or backlog cap caused closure.
- F3: PR validation failed the branch-name convention. F4: repository lint and workspace Actions failed; the reviewer did not establish all failures on the same base. These Actions findings were not repository-enforced requirements on the integration branch.
- F5: partial-frame reads are not bounded by the write-stall limit; the hypothesized reader-readiness mechanism and wrong-SID refusal were not isolated by native tests. A healthy client with one write blocked longer than 10 seconds is also closed.
- F0: the review brief named `mcao-core` console session code, but this PR changes the Windows backend instead.

#### Affected behavior or files

- `crates/mcao-backend-windows/src/console_server.rs`: stall close error and read/write session behavior.
- `crates/mcao-backend-windows/tests/console_pipe.rs`: flood-test attribution, partial-frame cancellation, and native refusal evidence.
- PR validation and workspace/lint Actions reported in the third review.

#### Implemented portion

- The backend adds a 10-second write-stall limit and cleanup, plus native idle-disconnect and partial-frame-at-shutdown tests. A host mutation probe showed the stall regression hangs when the limit is disabled. A native concurrent console run on the third-review commit passed.

#### Remaining work

- Return a distinct stall error while preserving the backlog error, and make native tests distinguish the paths.
- Establish the intended behavior for idle partial-frame reads and obtain direct native wrong-SID and reader-readiness evidence where feasible; do not treat the passing flood test as proof of which close path fired.
- The branch-name validator and review-observed lint/workspace failures were not corrected in this PR; same-base causation was not fully established.

#### Known impact

- Operators may misdiagnose a stalled write as backlog overflow. A client can leave a partial-frame read blocked until disconnect, DETACH or shutdown. The 10-second bound can close a paused but otherwise healthy client.

#### Verification status

- Native concurrent console run [41159883244](https://github.com/mc-innersource/motif-hve-multi-agent-orchestrator/actions/runs/41159883244) passed on `ad38b605`; the review's host stall mutation probe and `mcao-core` console-session tests passed. No native test isolated the hypothesized reader-readiness mechanism or wrong-SID refusal. PR validation, lint and workspace Actions were red in the third review; not all were proven red on the same base.

### DPES-954, DPES-963: Record PR326 third-review watcher protection gaps

- Jira: [DPES-954](https://mccdh.atlassian.net/browse/DPES-954), [DPES-963](https://mccdh.atlassian.net/browse/DPES-963)
- Pull request: [#326](https://github.com/mc-innersource/motif-hve-multi-agent-orchestrator/pull/326)
- Third-review commit: `b664bda7faa8bdce0918f28a54909d22c5cc805e`
- Recorded: 2026-11-01
- Status: Partially Implemented

#### Acceptance criteria not implemented

- DPES-963 AC5: watcher launches hidden in command substitution must be fixed or receive an explicit policy disposition. `eval "$(printf 'bin/fm-watch-arm.sh')"`, `` eval `printf bin/fm-watch-arm.sh` `` and `eval "$(echo bin/fm-watch-arm.sh)"` remain allowed.
- DPES-954 AC3 and DPES-963 AC4: DPES-893 AC8 reconciliation for bare `fm-watch` versus `fm-watch.sh`, and reason codes, is incomplete. DPES-954 records AC8 as unmet as written.

#### Review finding

- Third review FAIL at `b664bda7`. Bare-name protections improved, but variable-split (`bin/fm-$X`) and pathname-glob (`bin/fm-watch-ar*.sh`) command-position launches and case-insensitive broad kills (`pkill -if`, `pkill -fi`, `pgrep -if ... | xargs kill`) remained bypasses (DPES-954 comment, 2026-10-28).
- DPES-963 AC5 also covers `xargs` and command-substitution launches and the `export PATH` sole-arm setup.

#### Affected behavior or files

- `bin/fm-arm-command-policy.mjs` (Node reference), `crates/mcao-core/src/shell_command_pos.rs` (Rust port), `bin/fm-arm-pretool-check.sh`.
- `docs/arm-pretool-check.md` (dynamic-name residuals, lines 276-279).
- `tests/fm-arm-pretool-check.test.sh` (sandbox-stub regression rows).

#### Implemented portion

- Classification probes on `d094a5c4` (policy only, nothing executed) deny the variable-split, glob, broad-kill, `xargs`, `export PATH` and bare `fm-watch` forms listed in the review. These are no longer listed as unmet.

#### Remaining work

- Fix or explicitly dispose of `eval` command-substitution launches (DPES-963 AC5). This residual is also named in the DPES-1005 entry and the #333, #342 and #369 entries; one disposition should cover all of them.
- Reconcile DPES-893 AC8 in the docs and record reason-code compatibility (DPES-954 AC3, DPES-963 AC4).

#### Known impact

- Watcher arm commands launched through `eval` with command substitution are allowed by the pretool hook and the Node policy.

#### Verification status

- Probe re-run on `d094a5c4` matches the audit taken at `d3395c4a`. Test suite not run. Rust coverage of the `eval` residual not verified.

### DPES-957: Record PR309 third-review canonical JSON gaps

- Jira: [DPES-957](https://mccdh.atlassian.net/browse/DPES-957)
- Pull request: [#309](https://github.com/mc-innersource/motif-hve-multi-agent-orchestrator/pull/309)
- Third-review commit: `32613b2f033cefd1de98373119b5f42073a73b48`
- Recorded: 2026-11-01
- Status: Partially Implemented

#### Acceptance criteria not implemented

- AC1: integer literal `-0` must match the accepted DPES-884 contract, with a documented input-boundary policy. Integer `-0` is still refused (`NegativeZero`, `crates/mcao-core/src/canonical_json.rs:50`) rather than matched to Python. DPES-957 records this as a temporary unmet parity limitation. DPES-1012 (Done) covers the boundary work; the parity gap stays open on DPES-957.
- AC2: committed golden fixture and manifest from the pinned pre-port baseline. The DPES-803 QA baseline was not found (DPES-957 comment, 2026-10-30 15:42). Owner decision (2026-11-01): main `d80bcd15f06b79fed639cf028e273306567ffbfd` is the replacement pre-port baseline. Fixtures and manifest are in `tests/fixtures/pre-port-baseline/`, generated from a clean checkout of that commit and independently regenerated. They are not evidence of DPES-803 equivalence.
- AC5: focused, integration and differential tests against the authoritative baseline. Not written; the replacement fixtures now exist, so these tests are the remaining work.

#### Review finding

- Third review did not pass (PR comment, 2026-10-27 23:36Z). Findings: integer `-0` and out-of-range literal parity, missing baseline fixture, silent impossible-state fallbacks, module known-limit documentation, Rust API hygiene.

#### Affected behavior or files

- `crates/mcao-core/src/canonical_json.rs` (literal handling, module docs, fallback branches).
- Baseline fixture and manifest: `tests/fixtures/pre-port-baseline/` (owner-designated replacement baseline, not DPES-803).

#### Implemented portion

- Out-of-range integer literals are refused; integers in `[i64::MIN, u64::MAX]` serialize exactly; module docs list the refusal boundaries; one fallback branch reports an invariant failure (`canonical_json.rs:52`).

#### Remaining work

- AC1: resolve `-0` parity or record the accepted boundary policy on DPES-957.
- AC2 and AC5: AC2 fixtures are produced from the replacement baseline (`tests/fixtures/pre-port-baseline/`). AC5 differential tests against the Rust implementation remain open. Do not present the replacement as recovery of the DPES-803 tag.
- AC3: audit the remaining serializer branches for silent fallbacks.
- AC4: not verified; listed as unresolved in the batch report.

#### Known impact

- Integer `-0` input is refused rather than canonicalized to Python's byte output, so cross-implementation hash verification for that input cannot be relied on.

#### Verification status

- Code lines re-read on `d094a5c4`. Baseline provenance search per the DPES-957 comment. No test suite run for this entry.

### DPES-965: Record PR321 third-review PII redaction gaps

- Jira: [DPES-965](https://mccdh.atlassian.net/browse/DPES-965)
- Pull request: [#321](https://github.com/mc-innersource/motif-hve-multi-agent-orchestrator/pull/321)
- Third-review commit: `413392684888290e962dd36721e715d649d70285`
- Recorded: 2026-11-01
- Status: Partially Implemented

#### Acceptance criteria not implemented

- AC1: non-approved matcher local-variable renames (`kw_end`) must be reverted or have explicit scope authorization recorded. The rename is still present (`crates/mcao-core/src/audit_redact_pii.rs:224` and `:281`); the DPES-965 comments record neither a reversion nor an authorization.
- AC4: determine the real pinned Python and Unicode baseline before claiming DPES-884 AC5. Not established; the DPES-803 baseline has no provenance (DPES-965 comment, 2026-10-30 15:42).

#### Review finding

- Third review FAIL at `41339268`. Findings: non-approved renames, documentation overstating `kw_end` and unit shape tests, duplicate Python whitespace predicate, unknown baseline version, table-size and dead-guard separation, test coverage.

#### Affected behavior or files

- `crates/mcao-core/src/audit_redact_pii.rs` (renames, module doc at line 9, table tests).
- `crates/mcao-core/src/mail_poll.rs` (duplicate whitespace predicate).

#### Implemented portion

- Word-boundary correction verified against CPython 3.14.6 / Unicode 16.0 in review. Approved ASCII digit and keyword case-fold deviations preserved. The missing baseline is recorded in Jira (AC5).

#### Remaining work

- AC1: revert or record the scope authorization for the renames.
- AC4 and AC5: blocked on the DPES-803 baseline.
- AC2, AC3 and AC6: not verified; listed as unresolved in the batch report.

#### Known impact

- Under-redaction relative to Python 3.12 and 3.13 baselines (5,004 and 4,382 code points differ, per DPES-965) is unverified.

#### Verification status

- Renamed identifiers confirmed present by grep on `d094a5c4`. Baseline provenance search negative per the DPES-965 comment. No test run.

### DPES-966, DPES-967: Record PR323 third-review command-position gaps

- Jira: [DPES-966](https://mccdh.atlassian.net/browse/DPES-966), [DPES-967](https://mccdh.atlassian.net/browse/DPES-967)
- Pull request: [#323](https://github.com/mc-innersource/motif-hve-multi-agent-orchestrator/pull/323)
- Third-review commit: `ab69c1470c10af7a5c17a7fc324504bdf7796a3c`
- Recorded: 2026-11-01
- Status: Partially Implemented

#### Acceptance criteria not implemented

- DPES-967 AC2: an independently runnable JS-versus-Rust differential covering all 96 wrapper-option entries in both directions, including detection of future unpinned additions. Not established: DPES-967 is Draft with no recorded implementation, and DPES-966 AC4 records 76 of 96 mutation cases as untested.

#### Review finding

- Third review FAIL at `ab69c147`: on macOS, `env --unset X <watcher>` allowed the watcher operand because the reference and port skip the operand that macOS runs. The Rust helper remains unwired as a runtime gate (PR comment, 2026-10-28 11:45Z).

#### Affected behavior or files

- `bin/fm-arm-command-policy.mjs` (live reference), `crates/mcao-core/src/shell_command_pos.rs` (Rust port), `tests/fixtures/arm-wrapper-corpus.txt` (209 lines), `docs/arm-pretool-check.md`.

#### Implemented portion

- Classification probes on `d094a5c4` deny `env --unset X bin/fm-watch-arm.sh`, `env -u X ...` and `env -P /usr/bin ...`. The corpus has rows for `nice`, `ionice`, `exec -l` and `exec -a`.

#### Remaining work

- AC2: the 96-entry bidirectional differential.
- macOS and GNU regression evidence for the `env` operand fix, with unverified platforms disclosed.
- DPES-967 AC3 through AC6 (known-gap documentation, waiver and PR metadata, API hazard and `non_exhaustive`, scans): not verified; listed as unresolved in the batch report.

#### Known impact

- Classifier output on macOS for `env` operands was not established to match the reference at the third-review commit.

#### Verification status

- Probes re-run on `d094a5c4`. Test suites not run. No owner waiver recorded.

### DPES-966, DPES-1001: Record PR333 third-review watcher-wrapper gaps

- Jira: [DPES-966](https://mccdh.atlassian.net/browse/DPES-966), [DPES-1001](https://mccdh.atlassian.net/browse/DPES-1001)
- Pull request: [#333](https://github.com/mc-innersource/motif-hve-multi-agent-orchestrator/pull/333)
- Third-review commit: `8991176c3dfa829a67cc91f4ab0c995e0eafed82`
- Recorded: 2026-11-01
- Status: Partially Implemented

#### Acceptance criteria not implemented

- DPES-966 AC4: every wrapper-option table entry exercised in committed tests, with an independently runnable reference differential, including the 76 of 96 mutation cases recorded as untested. Not established: the corpus has 209 lines, but full table coverage has not been counted.
- DPES-1001 AC4 (finding B): the explicit owner decision on the copy-or-link-then-run residual is not recorded. The residual is recorded in the DPES-1005 entry, which this entry links rather than repeats.
- DPES-1001 finding C (`eval "$(printf ...)"`): still allowed on `d094a5c4`. Recorded in the PR323 entry under DPES-963 AC5; not repeated here.
- DPES-1001 finding F (`alias a=bin/fm-watch-arm.sh; a`): allowed on `d094a5c4`; owner disposition not recorded.
- DPES-966 AC5 (classifier-failure transport): the owner decision of 2026-11-01 keeps fail-open behavior (DPES-994). This is a recorded decision, not an implemented fix.

#### Review finding

- Third review FAIL at `8991176c`. Ten bypasses still allowed: `yash`, `posh` and `ksh93` `-c`; `$SHELL -c`; `$'...'` inside a double-quoted payload; git `--exe` abbreviation; sed `e`; fd `--exec`; tmux `send-keys` and `send`; pnpm, yarn, bun and bunx exec; interpreter inline code; `source <(...)`. Non-blocking: the Pi and omp cd-guard argv (DPES-994), copy or link residual, `eval` (DPES-963), `xargs` reading names from stdin, the watcher lifecycle suite not run, alias.

#### Affected behavior or files

- `bin/fm-arm-command-policy.mjs`, `bin/fm-arm-pretool-check.sh`, `crates/mcao-core/src/shell_command_pos.rs`.
- `tests/fm-arm-pretool-check.test.sh`, `tests/fixtures/arm-wrapper-corpus.txt`, `docs/arm-pretool-check.md`.

#### Implemented portion

- All ten listed probes are denied on `d094a5c4` (classification only). Finding D (`xargs -I{} {} < names.txt`, `cat names.txt | xargs -I{} {}`) is denied. Controls allow.
- The attempt-3 record reports 384 arm-pretool checks passing at the third-review commit.

#### Remaining work

- Confirm each of the ten forms has a committed regression row, and that the Rust and JS differential agrees (DPES-1001 AC2). Not verified.
- Named test commands (DPES-1001 AC6), including the watcher lifecycle suite. Not run.
- Owner decisions for finding B and finding F; `eval` under DPES-963.

#### Known impact

- The ten forms are denied on `d094a5c4`. Still open: copy-then-run and alias forms, `eval` substitution, and classifier failure that lets protected commands proceed.

#### Verification status

- Probes on `d094a5c4` only. `exec -l` and `exec -a` with a protected final command are allowed by design (`finalProtectedAllowed` in `bin/fm-arm-command-policy.mjs`); documentation of that design was not verified. Test suites and the Rust differential not run.

### DPES-963, DPES-991: Record PR342 third-review watcher protection gaps

- Jira: [DPES-963](https://mccdh.atlassian.net/browse/DPES-963), [DPES-991](https://mccdh.atlassian.net/browse/DPES-991)
- Pull request: [#342](https://github.com/mc-innersource/motif-hve-multi-agent-orchestrator/pull/342)
- Third-review commit: `3ac51d01a16d149f2a3b4b8a9354232734c6fd72`
- Recorded: 2026-11-01
- Status: Partially Implemented

#### Acceptance criteria not implemented

- DPES-991 R1: prefix-assignment expansion order. `X=fm-watch-arm.sh; X=x bin/$X` and `X=fm-watch-arm.sh; X=x "bin/$X"` are allowed on `d094a5c4`, because the shell expands `$X` before the later assignment takes effect.
- DPES-991 R2: `+=` append assignments are not tracked. `X=watch-arm; X+=.sh; bin/fm-$X` is allowed on `d094a5c4`.
- DPES-991 acceptance: R1 to R3 closed in both the Node and Rust classifiers, each with a test that fails without the fix. Not verified; the Rust classifier was not probed.
- DPES-991 residual gaps (inherited-environment kill pattern, approved `FM_HOME` export, GNU `xargs -I{}` behavior, files loaded with `source`): closed or documented as explicit residuals. Not verified.

#### Review finding

- Third review FAIL at `3ac51d01` after FAILs at `91a11fb4` and `b17571e6`; merged under the three-review policy (PR comment, 2026-10-29 22:54Z). Findings R1 to R3 and the residual gaps are tracked in DPES-991.

#### Affected behavior or files

- `bin/fm-arm-command-policy.mjs`, `crates/mcao-core/src/shell_command_pos.rs`, `bin/fm-arm-pretool-check.sh`.
- `tests/fm-arm-pretool-check.test.sh`, `tests/fm-watch-arm.test.sh`.

#### Implemented portion

- R3 wrappers (`setsid`, `flock`, `ionice`, `watch`, `script`, `find -exec` with a bare or `sh -c` payload) are denied on `d094a5c4`.

#### Remaining work

- R1 and R2 fixes with tests in both classifiers; disposition of the four residual gaps; the named test commands in DPES-991.
- The `eval` command-substitution residual is recorded under DPES-963 (see the PR326 entry).

#### Known impact

- Watcher arm commands whose name is built by expansion-order or `+=` assignment are allowed by the Node policy on `d094a5c4`.

#### Verification status

- Probes only, on the Node policy. Test suites not run. Rust classifier not probed.

### DPES-999: Record PR350 third-review cloud routing gaps

- Jira: [DPES-999](https://mccdh.atlassian.net/browse/DPES-999)
- Pull request: [#350](https://github.com/mc-innersource/motif-hve-multi-agent-orchestrator/pull/350)
- Third-review commit: `dbf76c1073dc28cad0c48fafaecce2438295239c`
- Recorded: 2026-11-01
- Status: Partially Implemented

#### Acceptance criteria not implemented

- AC2: owner decision on gcp teardown, either a gate in the shell and Rust with a teardown test, or acceptance documented. Not recorded; the DPES-999 comment of 2026-10-30 06:19 states AC2 and AC5 need an owner decision.
- AC5: policy-bypass routes (`fm-control cancel`, `fm-cloud-observe-lib status`, `fm-peek logs`) routed through `fm-cloud-policy.sh` or documented as accepted exceptions. Not recorded.

#### Review finding

- Third review FAIL at `dbf76c10` (PR comment, 2026-10-30 07:29Z). Blocking: the gcp Teardown disclosure was false (the `cloud_target=` gate is never written; spawn writes `execution_target=gcp`); the cancel and fetch claims were false (`bin/fm-control.sh:364-366` runs cloud cancel directly, and fetch has no shell route). Also: `remedy_verbs_are_shipped_control_verbs` greps usage text only.

#### Affected behavior or files

- `crates/mcao-core/src/routing.rs` (GCP_ALLOWED comment, matrix tests).
- PR body Matrix section and Reviewer Focus items 1 and 3.

#### Implemented portion

- PR #367 (merged `c738dc0b`) corrected the premises; the DPES-999 comment states `fm-teardown.sh` refuses gcp only incidentally at line 947. `routing.rs:131` describes the `cloud_target=` gate. Remedy texts match the shell per the attempt-3 record.

#### Remaining work

- AC2 and AC5: owner decisions.
- AC3 (rename or split the gcp allowed-pairs test), AC4 (matrix doc), AC6 (real-dispatch check in `remedy_verbs_are_shipped_control_verbs`) and AC7 (test and clippy evidence): not verified; listed as unresolved in the batch report.

#### Known impact

- gcp teardown is allowed without a shell gate; refusal is incidental through endpoint validation. Policy-bypass shell routes remain.

#### Verification status

- Jira and PR comments only. Not re-run.

### DPES-1001: Record PR369 third-review watcher-wrapper follow-up gaps

- Jira: [DPES-1001](https://mccdh.atlassian.net/browse/DPES-1001); follow-up tracked in [DPES-1005](https://mccdh.atlassian.net/browse/DPES-1005)
- Pull request: [#369](https://github.com/mc-innersource/motif-hve-multi-agent-orchestrator/pull/369)
- Third-review commit: `6aa645e57e338659aacadc5c68bf0d0ba1a92cee`
- Recorded: 2026-11-01
- Status: Partially Implemented

#### Acceptance criteria not implemented

- Finding 5 (unlisted runners): `fzf --bind 'execute(bin/fm-watch-arm.sh)'` is allowed on `d094a5c4`. `lua5.4`, `perl5.36` and `vim -c` are denied. `ruby3.2`, `node20` and `nvim` were not probed. The `FZF_DEFAULT_OPTS` form is in the DPES-1005 entry.
- Finding 6 (command substitution in `eval`): `eval "$(printf %s bin/fm-watch.sh)"` and `eval "$(printf %s "$1")"` are allowed on `d094a5c4`. This is the DPES-963 residual recorded in the PR326 entry; not repeated here.
- Finding 7 (Rust long-option parity): `crates/mcao-core/src/shell_command_pos.rs` parses long options by exact name, so GNU `xargs --arg-f` diverges from the JS policy. Not verified on `d094a5c4`.
- Copy or link then run (DPES-1001 finding B): recorded as unaccepted in the DPES-1005 entry; linked, not repeated.

#### Review finding

- Third review FAIL at `6aa645e5` (PR comment, 2026-10-30 14:10Z; DPES-1001 comment, 2026-10-30 09:41). Blocking: findings 1 to 3 are positional-parameter bypasses (`set -eo pipefail` cluster replaces positional values; `${1:-x}`, `${1-x}`, `${1:0}`, `${@:1}` bypass tracking; positional values assigned and passed to `eval`). Finding 4: PR body and `docs/arm-pretool-check.md` overstated positional denial. Non-blocking: findings 5 to 7 above.

#### Affected behavior or files

- `bin/fm-arm-command-policy.mjs` (`applyPositionalBuiltin`, `expandWord`), `docs/arm-pretool-check.md` (lines 326-328), `crates/mcao-core/src/shell_command_pos.rs`, `tests/fm-arm-pretool-check.test.sh`.

#### Implemented portion

- Findings 1 to 3 are denied on `d094a5c4`, including `set -eo`, `set -oe`, `${1:-x}`, `${1-x}`, `${@:1}`, `x=$1; eval "$x"`, `x="$1"`, `x="$@"` and `set -o pipefail; x=$1; eval "$x"`. The benign control `set -- echo hi; eval "$1"` is allowed.
- Finding 4: `docs/arm-pretool-check.md:326-328` now describes positional tracking; the PR body was not re-read.
- Finding 5 for `lua5.4`, `perl5.36` and `vim -c` is denied.

#### Remaining work

- Finding 5: `fzf --bind` execute form; `ruby3.2`, `node20`, `nvim` not verified.
- Finding 6: `eval` substitution, under DPES-963.
- Finding 7: Rust long-option parity not verified.
- Named test commands and the unsharded pretool suite (DPES-1001 AC6): not run; listed as unresolved in the batch report.

#### Known impact

- On `d094a5c4`, `eval`-substitution launches and the `fzf --bind execute()` argument form remain allowed.

#### Verification status

- Probes on `d094a5c4` only. Test suites and the Rust differential not run.

### DPES-885, DPES-962, DPES-953: Record PR314 third-review steering gaps

- Jira: [DPES-885](https://mccdh.atlassian.net/browse/DPES-885), [DPES-962](https://mccdh.atlassian.net/browse/DPES-962), [DPES-953](https://mccdh.atlassian.net/browse/DPES-953)
- Pull request: [#314](https://github.com/mc-innersource/motif-hve-multi-agent-orchestrator/pull/314)
- Third-review commit: `c11b6a491b65a26411bbc7df5ac2fab5d4389318`
- Recorded: 2026-11-01
- Status: Partially Implemented

#### Acceptance criteria not implemented

- DPES-885 AC10 wording: the literal Jira text does not match the exact tmux selector `=fm-<id>` implemented in `crates/mcao-core/src/steer/attach.rs`. DPES-962 requires the wording reconciled with the verified behavior; DPES-979 AC5 keeps it open, and the Jira text was not changed.
- Windows runtime evidence: the failed Windows workspace check at the reviewed head was not reproduced, and the original panic log was not independently verified (DPES-979 description). Remains unverified, not a passing result.
- Steering mutation evidence: the `steer.rs` mutation result at the reviewed head is recorded only as neutral (DPES-979 finding L1). No later record establishes its coverage or verdict.

#### Review finding

- Third adversarial review FAIL at `c11b6a49` (PR comment; DPES-962 description). Unresolved findings: exact tmux target; initial RESIZE discarded before claim; failed Windows workspace check and missing steer mutation evidence; Herdr attach argv not verified against the pinned CLI; single-writer behavior with repeated connection IDs; zero-dimension resize; dotted task IDs; API hygiene.

#### Affected behavior or files

- `crates/mcao-core/src/steer.rs`, `crates/mcao-core/src/steer/attach.rs`, `crates/mcao-core/src/steer/decision.rs`, `crates/mcao-core/src/steer/slot.rs`.
- `crates/mcao-core/tests/steer_tmux_target.rs`: Windows gap and mutation coverage.

#### Implemented portion

- Exact `=fm-<id>` tmux target with exact-match tests (`crates/mcao-core/src/steer/tests.rs:942-992`).
- Initial RESIZE before claim is applied (`crates/mcao-core/src/steer/decision.rs:130`; `tests.rs:585`).
- Dotted task IDs are refused at attach and creation (`attach.rs:27-53`).
- Herdr attach is refused and documented as unverified (`attach.rs:37-38`; `steer.rs:42`).
- Private-terminal `attach-session` integration test exists (`steer_tmux_target.rs:121`, DPES-979 AC4).
- DPES-953 zero-dimension resize, repeated-ID release and re-claim, literal wire text, and framing close are recorded as met by PRs #331 and #345 (DPES-953 comment, 2026-10-29).

#### Verification status

- DPES-962 and DPES-953 are Done in Jira. DPES-962 comments state its remaining criteria moved to DPES-979 and DPES-992, and DPES-979 states the Windows runtime was not independently verified.
- Static reads on `origin/DPES-751` (`bc0b159d`). The steer test suite and Windows runs were not executed in this batch.

### DPES-908: Record PR238 third-review config and cloud workflow gaps

- Jira: [DPES-908](https://mccdh.atlassian.net/browse/DPES-908)
- Pull request: [#238](https://github.com/mc-innersource/motif-hve-multi-agent-orchestrator/pull/238)
- Third-review commit: `a05e308630f9683858ed732cb78c55bc905acbef`
- Recorded: 2026-11-01
- Status: Partially Implemented

#### Acceptance criteria not implemented

- AC1 (review findings 2 and 3): the cloud workflow changes are neither removed nor proved by a `workflow_dispatch` run. `.github/workflows/cloud-images.yml` still sets `secrets: inherit` in seven jobs (lines 32, 44, 56, 69, 87, 105, 125). `.github/workflows/cloud-record-digests.yml:47` maps `GS2_IMPERSONATION_ACCOUNT_SECRET`, which the third review recorded as present only in the nonprod environment. The owner decision on this mapping is open (DPES-908 comment, 2026-10-30 09:42).

#### Review finding

- Third review at `a05e3086`, merged into DPES-751 under the three-failure rule (DPES-908 description). Five findings: commit subject and agent co-author trailers; out-of-slice cloud workflow secret mappings; lost workflow secret-flow assertion; lint red from one zizmor finding; surviving mutants and documentation precision.

#### Affected behavior or files

- `.github/workflows/cloud-images.yml`, `.github/workflows/cloud-record-digests.yml`.
- `cloud/tests/test_release_workflows.py`, `crates/mcao-core/src/config.rs`, `crates/mcao-core/tests/config.rs`, `crates/mcao-core/tests/config_skipped.rs`.

#### Implemented portion

- Finding 3: `_assert_recorder_reads_only_the_impersonation_secret` pins the recorder secret read (`cloud/tests/test_release_workflows.py:103`; used at `:265` and `:422`).
- Finding 5: skipped-entry ordering and non-UTF-8 directory tests are present (`crates/mcao-core/tests/config.rs:886`, `:968`; `config_skipped.rs:31`, `:60`). DPES-908 records PR #381 merged with three caught mutants.

#### Verification status

- Static reads on `origin/DPES-751` (`bc0b159d`). Workflow runs, lint, and the commit-trailer scan were not established in this batch.

### DPES-883: Record PR376 third-review console gaps

- Jira: [DPES-883](https://mccdh.atlassian.net/browse/DPES-883); tracked in [DPES-1006](https://mccdh.atlassian.net/browse/DPES-1006)
- Pull request: [#376](https://github.com/mc-innersource/motif-hve-multi-agent-orchestrator/pull/376)
- Third-review commit: `066094c52912855a887b71143b7a505d97fb4b29` (merged as `b648de5e20f717614b3588eed47cf98ad377b717`)
- Recorded: 2026-11-01
- Status: Partially Implemented

#### Acceptance criteria not implemented

The following residuals of the `066094c5` review remain unmet on `origin/DPES-751`. The PR #401 entry under DPES-1006 carries the same residuals forward with its own review evidence and is the full record; they are not repeated there.

- Write-stall versus backlog attribution: `crates/mcao-backend-windows/src/console_server.rs:376` still constructs `client backlog exceeded`, and the stalled-writer tests assert that text (`:1164`, `:1196`).
- Idle partial-frame reads: the reader path (`console_server.rs:383`) is not bounded by the write-stall limit, so a partial frame stays blocked until disconnect, detach, or shutdown.
- Wrong-SID refusal: no native test exists. The reasoning is documented, but native evidence needs a second Windows user account.

#### Review finding

- Third review at `066094c5` (DPES-1006 description): blocking cleanup on error paths; blocking partial-frame reader; blocking PR inventory; missing wrong-SID test or limitation evidence; ignored `DuplicateHandle` failure; unverified idle-disconnect mapping; live AC3 reconciliation.

#### Implemented portion

- PR inventory corrected: the PR body lists `Cargo.toml` with `Win32_Security_Authorization`, `pipe.rs`, and `lib.rs` (`gh pr view 376`; DPES-1006 comment, 2026-10-30 15:02).
- Error cleanup routes through `finish(...)` (`console_server.rs:265`). `DuplicateHandle` and `CancelSynchronousIo` failures are surfaced and tested (`:1266`, `:1282`).
- Idle-disconnect native evidence: Windows run `41207855565` concluded success on `06140edd`, the final PR #401 head (DPES-1006 comment, 2026-10-31 09:02).
- Live AC3 reconciled to the ring `read_from` boundary tests (`cargo test -p mcao-core --locked console::ring`, 10 passed, macOS; DPES-1006 comment, 2026-10-30 15:02).

#### Verification status

- Jira DPES-1006 comments, `gh pr view 376`, `gh run view 41207855565`, and static reads on `origin/DPES-751` (`bc0b159d`). No Windows runtime execution in this batch.

### DPES-1009: DPES-893 follow-up: address PR #390 third-review findings

- Jira: [DPES-1009](https://mccdh.atlassian.net/browse/DPES-1009)
- Pull request: [#398](https://github.com/mc-innersource/motif-hve-multi-agent-orchestrator/pull/398)
- Third-review commit: `f6c6976dc473decf4235ad2b4b8dca9065b233f8`
- Recorded: 2026-11-01
- Status: Partially Implemented

#### Acceptance criteria not implemented

- AC1, dotted legacy IDs cannot control or relaunch an unintended tmux pane, with regression coverage: PARTIAL. The refusal exists on `DPES-751` (`bin/fm-control.sh:432-434`). The control-suite dotted-verb test `test_dotted_legacy_id_is_refused_on_every_tmux_verb` printed `ok` in an incomplete run on this head; `tests/fm-control.test.sh` has no completed result.
- AC3, Perl normalization failures are surfaced, no truncated brief is written, and a forced-failure regression test passes: PARTIAL. The code guard is met (`bin/fm-live-lab.sh:444` on `DPES-751`). The forced-failure fixture (`tests/fm-live-lab.test.sh`, near line 785) was not executed because the live suite stopped at an earlier failing test.
- AC4, focused tests pass on the final implementation with exact commands and results recorded: UNMET. `tests/fm-control.test.sh` has no completed result on this head, and `tests/fm-live-lab.test.sh` exited with RC=1 before reaching the forced-failure test.
- AC5, original PR link and this follow-up linked, with GitHub Actions not used as evidence: PARTIAL. The Jira item keeps the source PR #390 link, but PR body validation (`pr-validate`) failed in the third review, so the PR text is not compliant.

#### Review finding

- Third adversarial review FAIL on `f6c6976d` against `DPES-751`. The reviewer found the fold-lock code logic sound; the held-lock tests kill the removed-lock mutant.
- Blocking: neither suite had a completed result on the head (control: no RC; live: RC=1 at an earlier failing test). The head was behind `DPES-751`. Required PR checks were red: `pr-validate` (PR body validation) and `Lint repository` (zizmor `excessive-permissions`).
- Non-blocking: the concurrent case-fold test is refused by the task-set lock, not the folded lock (see Implemented portion).

#### Affected behavior or files

- On `DPES-751`, unchanged by this PR: `bin/fm-control.sh` (dotted-ID refusal for tmux verbs) and `bin/fm-live-lab.sh` (Perl exit status checked before the brief marker is appended).
- PR diff against `DPES-751`: `bin/fm-spawn.sh` (case-fold collision checks and folded spawn lock), `tests/fm-spawn-batch.test.sh` (three case-fold tests), `tests/fm-live-lab.test.sh` (forced-fold-failure shim now emits partial output), and `docs/architecture.md` (folded-lock paragraph).

#### Implemented portion

- AC2, case-fold collisions rejected before publication and under the spawn lock: MET. The collision check runs before the task lock (`bin/fm-spawn.sh:1528`) and again under it (`:1835`). The folded lock `spawn_acquire_case_fold_lock` (`bin/fm-spawn.sh:1531`) is taken before the task lock on the cloud (`:1733`) and local (`:1828`) paths. Three new tests passed on the head: `test_concurrent_case_fold_spawns_share_the_folded_lock`, `test_held_case_fold_lock_refuses_a_cloud_case_variant`, and `test_held_case_fold_lock_refuses_a_local_case_variant` (RC=0, 3 ok).
- Fold-lock design: the concurrent test is guarded by the task-set lock. Fresh spawns hold the task-set lock through publication, so that lock causes its refusal; the folded lock is defense-in-depth there. The two held-lock tests guard the fold lock itself: each holds the folded lock and asserts that the case-variant spawn is refused with the folded-lock message. In the third review, removing the local-path folded-lock call failed the local held-lock test.
- AC1: the dotted-ID refusal is on `DPES-751`. The batch file's dotted relaunch test exists; it was not re-run for this record.
- AC3: the code guard is present (`bin/fm-live-lab.sh:444` checks the Perl exit status before appending the marker and refuses the fill on failure).

#### Remaining work

- AC1, AC4: run `tests/fm-control.test.sh` to completion on the head; record the exact command, RC, and ok/not-ok counts.
- AC3, AC4: diagnose the `tests/fm-live-lab.test.sh` failure `down waited on a PID with a different start time` (line 563), run the suite to completion including the forced-fold-failure test (near line 785), and record the exact command, RC, and counts.
- AC4: run the full `tests/fm-spawn-batch.test.sh` on the head and record the exact command and result.
- AC5: make PR body validation pass, keeping the source PR #390 link and this follow-up link.
- Non-blocking: the third review judged the attempt-2 relaunch-message concern partly addressed for legacy case-variant records; not re-verified. The `Lint repository` failure is not established as base-level.

#### Known impact

- If the base refusal were bypassed, a dotted legacy ID could resolve to another task's tmux pane. The refusal is in code, but no completed suite on this head proves it.
- Until the suites complete, the PR has no completed acceptance evidence for AC1, AC3, or AC4. Merge follows the third-review policy; this entry records the unmet criteria.

#### Verification status

- `tests/fm-control.test.sh` (log `/tmp/pr398-gate-control.log`): still running when checked, about 36 minutes elapsed; 39 ok, 0 not ok; no RC recorded. The dotted-verb test printed `ok` in this run.
- `tests/fm-live-lab.test.sh` (log `/tmp/pr398-gate-live.log`): RC=1 after 21 ok and 1 not ok (`down waited on a PID with a different start time`, line 563). The run took about 26 minutes. The forced-fold-failure test was not reached.
- `tests/fm-spawn-batch.test.sh`, three new case-fold tests: RC=0, 3 ok on the head (third-review evidence). The full batch file was not re-run for this record.
- Live-lab failure on a clean `DPES-751` worktree: not established. The run takes about 26 minutes, beyond the optional five-minute check.
- Third-review PR checks: `pr-validate` failed (`PR body validation failed: body`); `Lint repository` failed (zizmor `excessive-permissions`). Base-level cause not established.
- Head merged with `origin/DPES-751` (`116602d7`) as merge commit `79b68480` before this entry. The merged diff against `DPES-751` names four files: `bin/fm-spawn.sh`, `docs/architecture.md`, `tests/fm-live-lab.test.sh`, and `tests/fm-spawn-batch.test.sh`.

### DPES-909: Record PR276 third-review zizmor suppression gap

- Jira: [DPES-909](https://mccdh.atlassian.net/browse/DPES-909); follow-up tracked in [DPES-920](https://mccdh.atlassian.net/browse/DPES-920)
- Pull request: [#276](https://github.com/mc-innersource/motif-hve-multi-agent-orchestrator/pull/276)
- Third-review commit: `8385c8fcfc4169f687667e81e7e82d2acdc37ec0` (merged as `303b8317e9f7794f91ca5a7e6f622a46ea099d00`)
- Recorded: 2026-11-01
- Status: Partially Implemented

#### Acceptance criteria not implemented

- DPES-909 AC4: the repository Lint check must show no finding introduced by DPES-751. On `origin/DPES-751`, seven zizmor `adhoc-packages` findings are active in `.github/workflows/ci.yml`, at lines 140, 145, 196, 201, 268, 300, and 552, each an ad-hoc global `npm install`. The suppressions in `.github/linters/zizmor.yaml` name lines 130, 135, 178, 183, 244, 252, and 511, which are not npm install lines. Only line 276 is both suppressed and an npm install.

#### Review finding

- F2 (blocking for AC4): seven zizmor `adhoc-packages` findings are live at the DPES-751 tip. The npm install lines in `ci.yml` moved, so the line-based suppressions point at the wrong lines.

#### Affected behavior or files

- `.github/workflows/ci.yml`: the ad-hoc global `npm install` steps at the lines above.
- `.github/linters/zizmor.yaml`: the `adhoc-packages` ignore list.

#### Implemented portion

- The suppression for the `npm install -g @earendil-works/pi-coding-agent` step at `ci.yml:276` matches its line.

#### Remaining work

- Re-point the `adhoc-packages` suppressions to the current npm install lines, or replace them with rule-scoped suppressions, then confirm that zizmor reports no unsuppressed `adhoc-packages` finding for `ci.yml`.

#### Known impact

- The integration branch carries seven unsuppressed zizmor findings. The Lint check on `main` run `40993579196` (2026-10-30) concluded failure, but its failing step was not extracted, so that failure is not attributed to these findings here.

#### Verification status

- Static read of `origin/DPES-751` at `116602d7`, and `zizmor 1.29.0 --offline --config .github/linters/zizmor.yaml .github/workflows/ci.yml` on that commit: seven low `adhoc-packages` findings active at lines 140, 145, 196, 201, 268, 300, and 552; summary `11 findings (1 ignored, 3 suppressed)`.
- Not established in this batch: DPES-909 AC1, the workflow_dispatch proof. The DPES-909 comments record it as deferred at merge (run `40236783008` failed at the worker image scan gate). A successful `workflow_dispatch` run of `cloud-images.yml` on `main` (run `40244468348`, head `b615c3c70`, passed through `record-digests`) predates PR 276's merge and is not referenced in the DPES-909 comments. Its bearing on AC1 is unresolved, so AC1 is not listed above.

### DPES-752: Record PR416 third-review decision record gaps

- Jira: [DPES-752](https://mccdh.atlassian.net/browse/DPES-752)
- Pull request: [#416](https://github.com/mc-innersource/motif-hve-multi-agent-orchestrator/pull/416)
- Third-review commit: `4d385d7b3c5be0ef13cd8b8bd230073bf9a26552`
- Recorded: 2026-11-01
- Status: Partially Implemented

#### Acceptance criteria not implemented

- AC1, native Windows pilot-lifecycle run: UNMET. No native run exists. The `probe-windows` job (`.github/workflows/rust-workspace-check.yml:175-287`) runs the stub harness on `windows-latest` and records capture, send text, interrupt and reconnect as unsupported. It does not run Claude Code. No pilot harness is wired and no approved credential route is in place.
- AC2, decision record with each item decided or evidenced: UNMET. Unmet parts:
  - HolderProbe path: `OPEN`. Windows uses `UnknownHolderProbe` (`crates/mcao-core/src/lock/probe.rs:30-36`), which returns `Unknown` for every lock, so no lock is proven stale. Neither option (Windows handle enumeration, named-pipe per-opener tracking) is chosen.
  - herdr transport: `OPEN`. The Unix client uses `UnixStream` (`crates/mcao-core/src/herdr/control_socket.rs:6`). No Windows transport (named pipes, TCP loopback, `interprocess`) is chosen.
  - Terminal mechanism: `Recorded` only. The ConPTY design is at `crates/mcao-backend-windows/src/conpty.rs:9-27`, but the probe's Windows path returns `Unsupported` (`crates/mcao-probe/src/native/windows.rs:135,140,145,162`). No native run confirms it.
  - Job Object accept or defer: undecided. The record lists Job Objects as `Recorded` implementation candidate (`crates/mcao-backend-windows/src/job.rs:45`). Jira section 5 item 8 requires a native run and an accept-or-defer decision.
  - Interruption timeouts: unmeasured. `crates/mcao-backend-windows/src/backend.rs:22` sets `INTERRUPT_WAIT` to 500 ms, labelled a DPES-755 proposal. `LadderTimeouts` has no defaults. Jira section 5 item 11 requires measurement on the Windows runner.
  - Pinned Claude Code version and credential route: `OPEN`. No Windows version is pinned. A personal Claude subscription is not used, and no approved route exists.
  - Signing mechanism, owner, prerequisites and update flow: `OPEN`. No release packaging configuration exists in the workspace.
  - Toolchain pin rationale: `OPEN`. `rust-toolchain.toml` pins `1.95.0`. The rationale is not written, and Jira lists it as an unfiled follow-up.
- AC5, crate table with each PROPOSED crate Confirmed or Rejected, included verbatim: UNMET. Unmet parts:
  - 16 of the 21 Jira-listed crate rows are `OPEN`: tempfile, fd-lock, fs2, sysinfo, nix, interprocess, imap, lettre, rustls, thiserror, tracing, assert_cmd, predicates, insta, cargo-dist, winget.
  - Table not verbatim: Jira AC5 refers to the "Appendix C crate table". That appendix is absent from the item description and its six comments. The record follows Jira section 5 items (a) to (l) (`docs/windows-lifecycle-decision-record.md` section 9).
  - The 7 `Confirmed` rows cite use sites. Jira section 8 requires one API invocation in a passing test per crate. No such test is cited for any row.

#### Review finding

- Third adversarial review FAIL on `4d385d7b`: AC1, AC2 and AC5 unmet, as listed above. No fourth review is run; the PR merges under the third-review policy with these items recorded here.
- Wrong statements, corrected in `7bda0f50`: the PR body said the diff against DPES-751 is the record only, but it also changes `docs/documentation-audiences.json`. Record section 9's libc row said mcao-cli uses libc for "durable utilities", which the cited `crates/mcao-cli/Cargo.toml` comment does not state; it now names openpty, fork and the lab-viewer signal calls.
- Incomplete dates, corrected in `7bda0f50`: the intro omitted Jira comments 2026-10-23 13:22 and 2026-11-01 15:48.
- Non-blocking, not corrected: section 10 quotes the requirement that the corrected guard detect real violations but cites no detection evidence. Section 4 labels Job Objects `Recorded` while Jira asks for accept or defer. Section 9's sysinfo row does not note that Jira's 12:09 "unconditional libc" wording is superseded.

#### Affected behavior or files

- `docs/windows-lifecycle-decision-record.md`: records the AC1, AC2 and AC5 items above.
- `crates/mcao-backend-windows/`, `crates/mcao-core/src/lock/probe.rs`, `crates/mcao-core/src/herdr/control_socket.rs` and `crates/mcao-probe/src/native/windows.rs`: the code paths behind the open or unproven items. No change in this PR.

#### Implemented portion

- AC2: pilot harness named (Claude Code, owner decision 2026-10-23). Distribution direction recorded (signed GitHub Releases). Signing remains OPEN.
- AC5: 7 of 24 table rows `Confirmed` (clap, serde, serde_json, windows-sys, sha2, libc, toml). `portable-pty` is `Rejected`. The GNU target is `Rejected` as a target row.
- Platform-leakage guard correction (PR 418, merged as `79d38415`) is in the branch. `cargo test --locked -p mcao-probe --test platform_leakage` passed 12 of 12 on macOS at `4d385d7b`.

#### Remaining work

- AC1: run the pilot lifecycle natively on Windows with Claude Code on an approved credential route. Wire a pilot harness and the herdr transport first.
- AC2: choose the HolderProbe path and the herdr transport. Run the terminal mechanism natively. Accept or defer Job Objects. Measure interruption timeouts on the Windows runner. Pin the Claude Code version and credential route. Record the signing mechanism, owner, prerequisites and update flow. Write the toolchain pin rationale. Resolve the 16 `OPEN` crate rows as a crate-table item.
- AC5: add the Appendix C table or record its absence as the accepted basis. Add one passing API-invocation test per PROPOSED crate and cite it for each row.

#### Known impact

- Windows lifecycle support is not proven. There is no native run, no lock-holder proof, no Windows herdr transport, and no measured interruption timeout. The record marks these `OPEN` or `Recorded`, not `Confirmed`.

#### Verification status

- Third review on `4d385d7b`: about 70 `file:line` cites read, none wrong. `cargo test --locked -p mcao-probe --test platform_leakage` passed 12 of 12 on macOS. `git diff --check` clean. `bin/fm-doc-audience-check.sh` exit 1 with 28 unclassified entries, unchanged.
- Not run in the third review: native Windows execution, the sysinfo liveness test (Jira item (e)), and guard mutant detection.
