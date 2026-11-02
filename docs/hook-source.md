# Session-start hook source

This doc is for contributors who change the Rust port of the session-start source decision in `crates/mcao-core/src/hook_source.rs`.
`bin/fm-sessionstart-run.sh` is the behavioral authority for that decision.
Routing per harness source name is owned by [`sessionstart-nudge.md`](sessionstart-nudge.md), and this doc does not restate it.

The port is pure decision logic.
`mcao hook session-start` is its first caller; the status of each wiring requirement is in [Wiring status](#wiring-status-mcao-hook-session-start) below.

## Authority

Where the DPES-893 text and the shell disagree, the shell wins.
The port does not change shell behavior, and a difference the shell itself contains is recorded below rather than fixed here.

| Topic | DPES-893 text | Shell behavior | Port decision |
|---|---|---|---|
| State directory | Default `FM_HOME/.state` | `STATE="${FM_STATE_OVERRIDE:-$FM_HOME/state}"` | Shell path; `FM_STATE_OVERRIDE` overrides it |
| Pi prerequisite | Inconsistent: the flag `--pi-prerequisite` in sections 2, 3, 5, and 10; the environment variable `FM_PI_PREREQUISITE` in the section 6 pseudocode and the section 7 AC4 | Argument `--pi-prerequisite` only | Argument, parsed by `parse_args`; AC4 follows the argument (see below) |
| Exit 3 | Only with the flag, and never on state-directory failure | `stand_down` exits 3 under the flag for gate, scope, and state-directory failures | `stand_down_exit_code` returns 3 under the flag; the caller must call stand-down on state-directory failure |
| Completion record | `.session-start-complete` exists | Regular file, not a symlink, lock owned by self, lock and completion PIDs are digits and equal | `session_start_completed` over facts the caller gathers |
| Source extraction | Serde parse first, awk fallback | Awk only; the source is never JSON-parsed | `extract_source_bytes` is awk only; serde is used only for the foreign-host predicate |
| Unparseable or absent source | Defaults to `startup` | Empty `--source ""` reaches the full-run branch; see the AC3 decision below | Empty sentinel routes to a full run; the baseline step stays the shell's and is not ported |
| Foreign host | Boolean `is_foreign_host` field | `jq -e` predicate on a string `cursor_version`; absent `jq` means not foreign | `payload_is_foreign_host`, run only when `jq` is on `PATH`; it matches `jq -e` on the measured classes and diverges on the classes listed below |
| Foreign-host exit | Silent on order | Exits 0 before any source extraction, regardless of the flag | Wiring requirement below |
| `resume`, `reload`, `fork` | Exits 0 as a no-op | Executes `fm-sessionstart-nudge.sh` | `SourceRoute::Nudge`; the caller must execute the nudge wrapper |

## Recorded wording decisions

Firstmate made these decisions for the DPES-987 follow-up.
They fix how this port reads the DPES-893 wording where that wording conflicts with the shell.
The DPES-893 text in Jira is not edited by this change; amending it is a separate step this record does not perform.

### AC3: default source and the empty-source baseline

Status: decided by firstmate. DPES-893 AC3 follows the shell behavior.
The shell does not apply that `startup` default.
The shell reads the payload whenever the source is empty (`[ -z "$SOURCE" ] && [ ! -t 0 ]` in `bin/fm-sessionstart-run.sh`, line 113).
That gate covers an absent `--source`, a bare trailing `--source`, and an explicit `--source ""`.
The payload value replaces the empty source only when the payload carries a `source` field.
An empty source remains when the payload is unparseable, absent, or keyless, and it reaches `fm-session-start.sh --source ""`.

That child runs the full digest.
It writes the start-of-session `AGENTS.md` baseline only when the source equals `startup`, so an empty source yields the same digest without that baseline record.
The wrapper's header says an unreadable or unrecognized source is treated as `startup`, and the code does not do that.

The port keeps the empty sentinel, because it is the shell's routing for an empty source.
It does not perform the baseline step, and it makes no equivalence claim for that step.
The decision is that DPES-893 AC3 follows the shell: an empty source runs the full digest without the `startup` default and without the baseline record.
Changing the shell to map an empty source to `startup` is not adopted.

### AC4: prerequisite flag or environment variable

Status: decided by firstmate. DPES-893 AC4 follows the `--pi-prerequisite` argument.
Its section 6 pseudocode and its section 7 AC4 name the environment variable `FM_PI_PREREQUISITE` instead.
The shell reads only the argument, so the port follows the argument.
The decision is that DPES-893 AC4 follows the flag; adopting the variable in the shell is not adopted.

## Scope limits

### ASCII separators and locale

The separator between `source` and its colon is ASCII only: space, tab, LF, VT, FF, and CR.
The shell matches the separator with awk `[[:space:]]`, so its behavior depends on locale.
Measured with the awk shipped on the development machine (BWK awk 20200816), U+00A0 and U+2003 around the colon behave as follows:

| Locale | Space, tab, LF, VT, FF, CR | U+00A0 | U+2003 |
|---|---|---|---|
| `C` | Accepted | Rejected | Rejected |
| `en_US.UTF-8` | Accepted | Accepted | Rejected |

The port implements the `C` contract.
Under the `en_US.UTF-8` locale measured here, the shell additionally accepts U+00A0 at that one position.
The difference is recorded rather than emulated, because it comes from the locale and is not a documented contract.

### JSON streams and the jq predicate

The source extractor works on any byte sequence, so a stream of several JSON values is searched as one text.
The foreign-host predicate follows the `jq -e` rule over the stream: every value must parse, and the last value decides.
Trailing garbage makes the payload not foreign, and whitespace-only input is not foreign.

### Foreign-host divergences from jq 1.8.1

The port is not equivalent to `jq -e` for every payload.
The rows below are measured on jq 1.8.1, and the reproduction is in [`verification/hook-source-differential.md`](verification/hook-source-differential.md).

| Class | Example | jq 1.8.1 | Port |
|---|---|---|---|
| BOM before the object | `EF BB BF` followed by `{"cursor_version":"x"}` | foreign | not foreign |
| `NaN` literal | `{"cursor_version":"x","n":NaN}` | foreign | not foreign |
| `Infinity` literal | `{"cursor_version":"x","n":Infinity}` | foreign | not foreign |
| Leading zero | `{"cursor_version":"x","n":01}` | foreign | not foreign |
| Leading plus | `{"cursor_version":"x","n":+1}` | foreign | not foreign |
| Leading dot | `{"cursor_version":"x","n":.5}` | foreign | not foreign |
| Out-of-range numbers | `1e999`, `-1e999`, `1.8e308` | foreign | not foreign |
| Lone low surrogate escape | `{"cursor_version":"x","n":"\uDC00"}` | foreign | not foreign |
| Nesting beyond 128 levels | `{"cursor_version":"x","a":` then 200 open brackets | foreign | not foreign |

Each divergence reads as not foreign in the port, so the port runs where jq would skip.
The shell header names that direction as the safe one, because a redundant run wastes work and a skipped run under the primary harness breaks supervision.
The port does not emulate these classes.
Emulating jq's number and escape grammar, and lifting serde's recursion limit, are separate decisions; lifting the limit would let a hook payload exhaust the stack.
The differential corpus found no payload that the port reads as foreign while jq reads it as not foreign.

### JSON escapes

The shell splits on every `"` byte and never decodes JSON escapes.
An escaped quote inside the value therefore ends the value, and the port does the same.

### Invalid UTF-8

Source extraction keeps value bytes unchanged, so a value may be invalid UTF-8.
The foreign-host predicate replaces invalid UTF-8 with U+FFFD before parsing, as `jq` does, so a string value with invalid bytes still counts as a string.
Routing takes `&str`, so the caller converts invalid bytes lossily for routing only; see wiring requirement 11.

## Empty-sentinel contract

`extract_source_bytes` returns an empty result when no source is found.
`parse_args` returns an empty source when `--source` is absent, bare, or explicit-empty.
`route` sends the empty source to the full run, with the source passed through unchanged.
The shell treats an absent and an empty source the same way at the routing step, which this port reproduces.
The baseline step is the shell's; see the AC3 section above.

## Rust style decisions

These decisions apply the guide in `google-rust-style`, with the exceptions recorded here.

- Pure query functions carry `#[must_use]`, because ignoring their result loses the decision.
- `route` takes a `Completion` enum instead of a boolean, because `route("clear", true)` does not say what `true` means.
- `stand_down_exit_code` takes `&SessionStartArgs` instead of a boolean for the same reason.
- `SourceRoute` is `#[non_exhaustive]`, because the wiring is expected to add routes.
- `Completion` is not `#[non_exhaustive]`, because it is a stable two-state set.
- `SessionStartArgs` and `CompletionFacts` keep public fields and `Default`, because they are small option bags with no consumer outside this crate; the guide's small-scope exception applies.
- The `&str` extractor exists only in the test module, because production callers pass bytes.

## Wiring requirements

Every requirement binds the caller that reads a session-start invocation.
A requirement marked `pure` has a unit test in `hook_source.rs` for the function it names.
A requirement marked `untested caller obligation` names behavior of the Rust caller that the pure functions cannot test. The caller `mcao hook session-start` now exists; the [Wiring status](#wiring-status-mcao-hook-session-start) section says which of those obligations its golden tests cover.
Where the shell reference behavior is pinned, the requirement names `tests/fm-sessionstart-nudge.test.sh` as the executable spec the caller must match.
The first wiring change must test each untested obligation end to end with isolated fixtures and no live session.

1. Read stdin as bytes, never as text (`untested caller obligation` for the read; the byte functions it feeds are `pure`).
2. Apply `normalize_command_substitution` to the payload, to the lock text, and to the completion text before any comparison, because bash discards NUL bytes and strips trailing newlines (`pure`).
3. Do not decode the payload as UTF-8 before extraction; the predicate decodes with replacement, as `jq` does (`pure` for both functions; the caller step is an `untested caller obligation`).
4. Skip reading stdin when it is a terminal, so an operator running the command by hand never blocks on a read (`untested caller obligation`).
5. Read the payload whenever the source is empty, whether `--source` is absent, bare, or explicit-empty, matching the shell gate `[ -z "$SOURCE" ] && [ ! -t 0 ]` (`parse_args` returns the empty source for each form and is `pure`; the shell reference is pinned by `test_run_empty_source_forms_read_the_payload`; the Rust stdin gate is an `untested caller obligation`; `parsed_empty_source_forms_route_to_the_full_run` pins the routing of each form).
6. Only when the payload was read, run the foreign-host check before extraction, and exit 0 when the payload is foreign, whether or not `--pi-prerequisite` is set; a missing `jq` means not foreign and the run continues (predicate `pure`; the shell reference is pinned by `test_run_foreign_host_payload_stands_down_silently` and `test_run_missing_jq_is_not_foreign_and_the_run_continues`; the Rust exit code and missing-`jq` path are `untested caller obligations`).
7. Treat a lock or completion file as recorded only when it is a regular file, not a symlink, and the lock is owned by the current process; compare the PIDs as whole texts after normalization (the decision from gathered facts is `pure`; the shell reference rejects a symlinked completion proof, pinned by `test_run_clear_rejects_a_symlinked_completion_proof`; the Rust reading of file types and ownership is an `untested caller obligation`).
8. If the state directory cannot be created, exit 3 under `--pi-prerequisite` and exit 0 otherwise; gate and scope stand-downs under the flag also exit 3 (`stand_down_exit_code` is `pure`; the shell reference is pinned by `test_run_state_dir_failure_exits_3_under_pi_prerequisite`; the Rust state-directory path is an `untested caller obligation`).
9. Route `resume`, `reload`, and `fork` to the nudge wrapper, and route `clear` and `compact` to a re-emit only with a recorded completion (the decision is `pure`; executing the nudge wrapper is an `untested caller obligation`).
10. Pass every other source through unchanged to a full run, including the empty sentinel (`pure`).
11. When the source is invalid UTF-8, convert it lossily only for `route`, and pass the original bytes to the shell child process, as the shell does (`untested caller obligation` for the byte handoff; `route_lossy_invalid_utf8_source_never_matches_a_named_route` pins the lossy routing decision).

## Caller obligation status

The first Rust caller is `mcao hook session-start`; the [Wiring status](#wiring-status-mcao-hook-session-start) section says which of these obligations it now tests.
The caller exists, so each obligation marked `untested caller obligation` is either covered by a golden parity case or still untested. The [Wiring status](#wiring-status-mcao-hook-session-start) section lists both sets from the test files.
Obligations still untested in this crate: requirement 4 (the terminal stdin skip) has no golden case, because it needs a pseudo-terminal that the golden harness does not provide.
Requirement 7 has no golden case for a Claude session id whose `CLAUDE_PID` is not a Claude ancestor, for a non-Claude harness under the same-session rule, or on Windows.
The non-Unix byte handoff for a payload source that is not UTF-8 is not covered (see Wiring status).

The DPES-987 criterion that ties these tests to the first caller stays open until every obligation above has a test.

## Verification

Run these from the repository root.

```sh
cargo test -p mcao-core --lib hook_source
cargo clippy -p mcao-core --all-targets
cargo fmt -p mcao-core -- --check
sh tests/fm-sessionstart-nudge.test.sh
```

The shell suite requires `jq` on the host, because its foreign-host cases call it.

The module tests pin the awk table and the separator classes, the trailing-quote and JSON-escape behavior, the jq stream and invalid-UTF-8 behavior, NUL normalization, and the completion and routing decisions.
The differential record in [`verification/hook-source-differential.md`](verification/hook-source-differential.md) covers 2000 generated payloads.
It found no awk source mismatches and 40 foreign-host mismatches, all in the safe direction.
That record does not cover the routing baseline step, the untested caller obligations, or any locale other than `C`.

## Wiring status: `mcao hook session-start`

The caller is `crates/mcao-cli/src/hook/session_start.rs`, tested by `crates/mcao-cli/tests/hook_session_start.rs` against `bin/fm-sessionstart-run.sh` with stub children.

- Met and covered by golden parity: 1 (bytes read from stdin, including an invalid UTF-8 payload), 2 (trailing newlines in the lock and completion text; NUL normalization is covered by the core unit tests only, since bash warns on stderr for NUL), 3 (invalid UTF-8 in the payload before the source key), 5 (absent, explicit-empty `--source ""`, and bare trailing `--source`), 6 (foreign host exits 0 when `jq` is on `PATH`, and the golden case prints `SKIP` without `jq`; a missing `jq` runs the full digest, covered on every host), 8 (exit 3 under the flag for state-directory failure and non-primary root), 9 (`resume`, `reload`, and `fork` run the nudge; `clear` and `compact` re-emit or run full), 10, and 11 (a non-UTF-8 `--source` argument reaches the child byte for byte).
- Argument grammar, not a numbered wiring requirement: `--help` and `-h` are ignored as the shell's `*)` arm ignores them, and golden cases cover both.
- Requirement 7, covered: a lock owned by a harness ancestor re-emits for `clear` and `compact`; a live Claude owner with a matching `.lock-session` re-emits; a different session id, a missing sidecar, and a dead pid run full; a symlinked lock and a symlinked completion run full; a zero-padded owner pid (`0` before the ancestor's pid) does not match the ancestor by text, as the shell requires; with a matching same-session sidecar it re-emits, as the shell does, pinned by `clear_with_a_zero_padded_owner_in_the_same_session_reemits_as_in_the_shell`; without one it runs full.
- Requirement 7, not covered: see the caller obligation status above.
- Not covered by a golden case: 4 (terminal stdin skip; needs a tty).
- Port-forced root: the root is `FM_ROOT_OVERRIDE` only. A binary's location in `target/`, including a checkout's own Cargo target directory, does not prove which checkout owns the scripts. Without the override, the port prints `no firstmate root: set FM_ROOT_OVERRIDE` on stderr, runs no script, and stands down. The working directory, binary path, symlink target, and build-machine path are not used to select a root. Set `FM_ROOT_OVERRIDE` to the intended script root for in-checkout, copied, installed, relocated, or symlinked binaries; the shell's own root remains its script location. Golden cases: `binary_in_checkout_requires_explicit_override`, `copied_binary_in_unrelated_cargo_target_runs_no_foreign_script`, `copied_binary_outside_its_checkout_runs_no_script`, `copied_binary_with_an_override_uses_the_override_root`, and `symlink_to_a_checkout_binary_requires_explicit_override`.
- Owner pid comparison: the lock text is compared with each ancestor pid as a string, as `fm_session_lock_owned_by_self` does. The numeric parse only validates the text and drives the liveness check; `0123` never matches ancestor `123` by ancestry.
- Cursor identity: `cursor_process_matches` mirrors `fm_cursor_process_matches` in `bin/fm-cursor-lib.sh`, which checks names and install paths and runs no probe. The port has no divergence there.
- Windows: the session-lock ancestry walk uses `ps` and `kill`, which Windows lacks. The walk finds no harness, so no lock is self-owned, and `clear` and `compact` run the full digest. As on other platforms, session-start requires `FM_ROOT_OVERRIDE`; Windows is cross-compiled only (`cargo clippy --target x86_64-pc-windows-msvc`), and no Windows runtime test exists.
- The state-directory error text drops Rust's `(os error N)` suffix; the golden cases compare it byte for byte for the errno values they produce (`File exists`), and other errno text can differ from `strerror`.
- Byte handoff is Unix-only. On Unix, a source taken from the payload keeps its bytes through `os_from_bytes`. On non-Unix, `os_from_bytes` converts through `from_utf8_lossy`, so a payload source that is not valid UTF-8 changes before the child receives it. A `--source` argument is passed as an `OsString` on every platform; the byte-for-byte golden case runs on Unix only.
- Foreign-host golden case availability: the case runs only when `jq` is on `PATH`. Without `jq`, it prints `SKIP` to stderr and returns. libtest captures that output on a passing run, so a pass on a host without `jq` does not show the case ran. The missing-`jq` case builds its own `PATH` without `jq` and runs on every host.
- `jq` detection mismatch: `jq_on_path` accepts any regular file named `jq` on `PATH` with any execute bit set. The shell's `command -v jq` asks whether the current user can execute it. A `jq` that only another user or group can execute counts as present in the port and absent in the shell. This narrow mismatch is kept and disclosed, not changed here.
- Not ported, still shell: the digest, the baseline step, and the nudge script itself run as the shell's own child scripts. The Rust caller only chooses and starts them.
