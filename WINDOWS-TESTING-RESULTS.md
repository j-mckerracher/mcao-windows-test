# Windows test results

Results of running the commands in `WINDOWS-TESTING.md` on this machine, plus a full
account of the run (including environment problems encountered while driving it).

- Date: 2026-10-29
- Branch: `main` (tracking `origin/main`), clean tree before the run
- Snapshot under test: `WINDOWS-TESTING.md` description — DPES-751 integration branch at
  commit `a3d3d0116223e7a0d5b4f0b7e8549bc0960e56ce` (squash)

## Environment

| Item | Value |
|---|---|
| OS | Windows 10.0.26200, x64 |
| Rust toolchain | stable, **1.95.0** (cargo 1.95.0 / rustc 1.95.0) |
| Host triple | `x86_64-pc-windows-msvc` (MSVC toolchain) |
| Active toolchain | `1.95.0-x86_64-pc-windows-msvc`, selected by this repo's `rust-toolchain.toml` override |

The MSVC Build Tools C++ workload is present; linking succeeded (all five test binaries
built and ran).

## Commands run, in the documented order

```
cargo test -p mcao-backend-windows --locked
cargo clippy -p mcao-backend-windows --all-targets --locked -- -D warnings
cargo test -p mcao-core --locked
```

Plus one follow-up invocation, explained below:

```
cargo test -p mcao-backend-windows --test job --locked
```

## Summary

| Command | Exit code | Outcome |
|---|---|---|
| `cargo test -p mcao-backend-windows --locked` | **101** | **FAILED — 1 test failed** (`interrupt` binary) |
| `cargo clippy -p mcao-backend-windows --all-targets --locked -- -D warnings` | 0 | ✅ Clean, no warnings |
| `cargo test -p mcao-core --locked` | 0 | ✅ All suites and doc-tests passed |
| `cargo test -p mcao-backend-windows --test job --locked` (follow-up) | 0 | ✅ Passed |

## Per test binary (exact crate names, as requested: lib, backend, conpty, interrupt, job)

| Test binary (`cargo test` target) | Result |
|---|---|
| lib — unittests `src\lib.rs` (`mcao_backend_windows-6ea77684a73fc9b5.exe`) | ✅ **76 passed, 0 failed, 1 ignored** — 1.00s |
| backend — `tests\backend.rs` (`backend-998af95011437b78.exe`) | ✅ **10 passed, 0 failed, 4 ignored** — 0.69s |
| conpty — `tests\conpty.rs` (`conpty-90e3b442bbc81f86.exe`) | ✅ **20 passed, 0 failed, 5 ignored** — 0.67s |
| interrupt — `tests\interrupt.rs` (`interrupt-d636516869f1b660.exe`) | ❌ **14 passed, 1 failed, 7 ignored** — 10.39s |
| job — `tests\job.rs` (`job-e9107551aa061290.exe`) | ✅ **4 passed, 0 failed, 3 ignored** — 0.07s |

The ignored tests are self-re-executing stubs (`backend_stub*`, `conpty_stub*`,
`interrupt_stub*`, `stub_child*`, `stub_grandchild*`, `stub_probe_job_membership*)`) that
run only as children of the real tests; they report `ignored` in their parent binary and
are not failures.

### The lib binary detail (in case per-test names matter for DPES-751 triage)

All 77 unit-test names are in the raw log (`test-run.log`). Groups touched: `conpty`
(+ `init_sets_the_utf8_output_code_page_when_a_console_exists`), `ctrl_break`,
`interrupt` (ladder, marker, write-thread, deadline semantics), `job` (named jobs,
limits, local-name-prefix refusals), `process` (quoting/backslash/NTFS-stream/batch
refusals, suspended-create, failed-assignment/resume termination),
`session` (ring-buffer, mark, poisoned buffer).

## The failure

```
failures:
    escalation_sends_a_second_ctrl_c_then_terminates_the_tree_at_the_wait

---- escalation_sends_a_second_ctrl_c_then_terminates_the_tree_at_the_wait stdout ----

thread 'escalation_sends_a_second_ctrl_c_then_terminates_the_tree_at_the_wait' (56976)
panicked at crates\mcao-backend-windows\tests\interrupt.rs:523:5:
assertion `left == right` failed
  left: Exited
 right: TerminatedJob
```

- Single failure; every other test in this binary and every other binary passed.
- Cargo then stops before running `job` — `cargo test -p mcao-backend-windows --locked`
  exits 101 without compiling/executing `tests\job.rs`. `job` had to be run separately
  (and passed). This is a load-bearing detail for CI: one flaky test hides all
  alphabetically-later test targets.

### Flakiness evidence (solo rerun)

```
cargo test -p mcao-backend-windows --test interrupt --locked \
    escalation_sends_a_second_ctrl_c_then_terminates_the_tree_at_the_wait
→ exit 0, 1 passed, 5.09s
```

The same test passed when rerun alone, so this is timing-sensitive / flaky, not
deterministically broken. The mismatch is: the child exited on the second Ctrl-C before
the tree-termination rung could claim it, so the observed outcome was `Exited` where the
assertion demands `TerminatedJob`. This is inside the escalation-ladder behavior the
test binary is specifically probing (its other ladder tests all passed, including the
three-run unconditional escalation to terminate).

## `mcao-core` detail

`cargo test -p mcao-core --locked` — compiled clean for the MSVC host and every test
binary passed with 0 failures, then `Doc-tests mcao_core` (0 tests) — exit 0 overall.
Two suite-level caveats from the raw log, both informational:

- `test concurrent_appends_keep_every_event_whole has been running for over 60 seconds`
  — the test still completed `ok`.
- `test readers_during_concurrent_appends_get_no_error` also took >60s and completed `ok`
  (this one abuts known follow-up DPES-983 territory: long-running audit/lifecycle tests
  under Herdr — no behavior failure, just slow).

## Raw evidence

- `test-run.log` — the complete uncapped stdout/stderr of steps 1–3 in documented order
  (test → clippy → core), each section delimited `===== N/3 …: <command> =====` and
  terminated by its `exit code:` line.

## Everything that happened during the run (process log)

Recorded verbatim so nobody has to reconstruct why the run looks the way it does.

1. Pre-flight: surveyed the repo layout (`crates/mcao-…`, no `mcao-backend-windows`
   present in the macOS-review-host snapshot description, but present here), confirmed
   toolchain via `cargo --version`, `rustc --version`, and `rustup show` (MSVC host,
   `rust-toolchain.toml` override active, 1.95.0).
2. Smoke-tested that `cmd /c echo hello` spawns correctly before committing to a long
   build (the first full probe had wedged the helper kernel rather than erroring; a retry
   in a fresh kernel returned cleanly, so no fix was needed — noted in case the wedge
   recurs).
3. Kicked off step 1 (`cargo test -p mcao-backend-windows --locked`) as a background
   thread streaming stdout/stderr into `test-run.log`, so the complete per-line transcript
   of the documented commands, not just summaries, is retained.
4. Step 1 completed: built and ran all of lib, backend, conpty, interrupt — 1 failure
   (`interrupt`), exit 101.
5. With step 1 done, steps 2 and 3 ran to completion cleanly (clippy 0 warnings, exit 0;
   core all-green, exit 0). The log confirms clippy's `Finished dev profile` and core's
   doc-test stage.
6. Because cargo skips remaining test binaries after the interrupt failure, step 4 was
   run manually (`--test job` only) to capture the `job` binary's pass/fail record —
   it passed (4 passed, 3 ignored sf-stubs).
7. Re-ran only the one failing test in isolation to test flakiness vs determinism. It
   passed solo (5.09s), so declared flaky rather than broken.
8. Checked git state afterwards: clean tree before the run, `test-run.log` the only
   untracked artifact.

## Known follow-ups referenced by WINDOWS-TESTING.md

Still open as of this run:

- **DPES-984** — minimum input window (relevant to
  `a_short_input_wait_is_refused_and_leaves_later_input_open` and
  `a_timed_out_input_write_stalls_all_later_input`, both of which passed, so no new
  evidence against it from this run).
- **DPES-983** — long-running audit/lifecycle tests (the >60s concurrent-append tests
  above affirm these stay in scope).
