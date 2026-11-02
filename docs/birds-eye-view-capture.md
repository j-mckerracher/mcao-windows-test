# Birds-eye view output capture verification

This record verifies `bin/fm-birds-eye-capture.sh` (schema `fm-birds-eye-capture.v1`, DPES-798).
It covers the tmux backend only, which is the one backend with a validated capture path on this machine.
Recorded 2026-10-18 UTC on macOS Darwin 25.6.0 (arm64).

## Versions

| Tool | Version |
| --- | --- |
| bash | GNU bash 5.3.20(1)-release (aarch64-apple-darwin25.6.0) |
| tmux | 3.7 |
| jq | jq-1.8.1 |
| perl | v5.34.1 |
| ShellCheck | 0.11.0 (pinned by `bin/fm-lint.sh`) |

Herdr is not installed on this machine, so it has no capture path here and reports `unavailable`.

## Behavior under test

- One read-only `tmux list-windows` call and one read-only `tmux capture-pane -p` call per capture. The capture primitive is the same as `fm_backend_tmux_capture` in `bin/backends/tmux.sh`.
- The identity comes only from one fleet snapshot, run with the home pinned by `--root`. Inherited `FM_*_OVERRIDE` private directories apply only when the inherited `FM_HOME` is that same home (the code-repo-as-`FM_HOME` layout in `docs/configuration.md`); overrides inherited from any other home are cleared.
- Task ids use the lifecycle rule `fm_task_id_path_safe` (letters, digits, `.`, `_`, `-`, no leading dot), so a leading dash or an id past 64 characters is legal; anything else is `unknown_identity` before the snapshot runs.
- The recorded tmux endpoint must be exactly `<session>:fm-<task-id>`, the binding `fm_backend_validate_task_endpoint` requires. The capture lists the session's windows (`=<session>`), keeps those whose whole name equals `fm-<task-id>`, and captures that window by its unique tmux window id (`@N`). No name is ever handed to `capture-pane`: tmux would prefix-match a bare name (`fm-<task-id>` reads `fm-<task-id>-2`) and would read a dotted id such as `fm-a.0` as pane 0 of window `fm-a`. A missing window, or several windows with that name, is `unavailable` with no text.
- The task's `spawn_gen` is re-read from its state meta, in the state directory the snapshot reported, right before and right after the capture. A changed value returns `generation_mismatch` with no text.
- Requests above 100 lines clamp to 100 with a stated reason.
- Malformed line counts, backend selectors, and pane or session selectors exit 2 with no JSON.
- Control sequences are stripped, and the returned text contains no control bytes.
- Trailing blank viewport rows that `capture-pane` pads onto a sparse pane are dropped, so `lines_returned` and `truncated` count content rows only.

## Portable regression

Command, run from the worktree:

    bash bin/fm-test-run.sh tests/fm-birds-eye-capture.test.sh

Output:

    ok - default capture: 40 lines, real pane, plain text, state unchanged, read-only tmux calls only
    ok - 100-line capture: 100-line tail from the real pane
    ok - excessive bounds: 101 and 99999 clamp to 100 with a stated reason
    ok - malformed --lines values and backend or pane selectors exit 2 with no output
    ok - stale generation: generation_mismatch with the current spawn_gen and no text
    ok - arbitrary identities: unknown_identity, no text, nothing evaluated
    ok - well-formed unknown id: unknown_identity from the snapshot
    ok - unsupported backend and dead endpoint: Output unavailable with a reason
    ok - relaunch during capture: generation_mismatch, output discarded
    ok - sparse pane: padding rows are not counted, truncated only when content rows drop
    ok - missing recorded window: unavailable, never a prefix-matched neighbor's output
    ok - endpoint that is not fm-<task-id>: unavailable, no text
    ok - dotted id: missing exact window is unavailable; the exact undotted window is captured
    ok - ambiguous window name: unavailable, no text
    ok - source-legal ids reach the snapshot; a leading dot is refused
    ok - private-directory layout: honored for its own home, cleared when inherited from another
    all birds-eye capture checks passed
    FM_TEST_END 2026-10-18T15:49:29Z tests/fm-birds-eye-capture.test.sh exit=0 duration_ms=101839 gate_skip=false
    FM_TEST_SUMMARY total=1 failed=0 skipped_gate=0 duration_ms=102136
    runner_rc=0

The test runs a real tmux server on a private socket behind a PATH shim that logs each tmux subcommand.
The pane program, `tests/fixtures/birds-eye/capture/pane-program.sh`, prints numbered lines, one with an SGR color, then an OSC title and a BEL.
Each capture runs one fleet snapshot plus two targeted `spawn_gen` reads of the task's state meta.

## Per-capture latency

Command, run from the worktree against a one-task throwaway home with the pane program above on a private tmux socket, with a PATH shim for `tmux` and a stub `no-mistakes`:

    /usr/bin/time -p bin/fm-birds-eye-capture.sh --root <home> --task cap-task --spawn-gen <gen>

Three consecutive default (40-line) captures on 2026-10-18 UTC took real 5.54 s, 9.97 s, and 16.90 s, each with status `ok`, 40 lines returned, and `truncated` true.
The single snapshot dominates, and its time varied with other load on this machine, so these numbers are a sample and not a bound.
The targeted `spawn_gen` reads are file reads and add no measurable time.

## Live validation

The live checks ran against this lane's own pane, the only live agent this lane could observe without touching another lane.
Commands had this form, with the current spawn generation passed as `--spawn-gen`:

    bin/fm-birds-eye-capture.sh --root <firstmate-home> --task birds-eye-capture --spawn-gen <current-gen> [--lines N]

Each row records the exit code, the status and reason fields from the JSON, and the number of changed lines in the live home's manifest across the call.

| Case | Arguments | Exit | Status | Requested | Returned | Reason | Live diff lines |
| --- | --- | --- | --- | --- | --- | --- | --- |
| default | (none) | 0 | ok | 40 | 31 | (empty) | 12 |
| lines40 | `--lines 40` | 0 | ok | 40 | 31 | (empty) | 20 |
| lines100 | `--lines 100` | 0 | ok | 100 | 31 | (empty) | 13 |
| lines101 | `--lines 101` | 0 | ok | 100 | 31 | requested lines clamped to 100 (v1 maximum) | 12 |
| lines99999 | `--lines 99999` | 0 | ok | 100 | 31 | requested lines clamped to 100 (v1 maximum) | 17 |
| stale | `--spawn-gen s0.0.0` | 0 | generation_mismatch | 40 | 0 | requested spawn_gen is not the current incarnation; no output returned | 7 |
| unknown | `--task no-such-task` | 0 | unknown_identity | 40 | 0 | task is not in the current fleet snapshot | 12 |
| pane-id-identity | `--task %27` | 0 | unknown_identity | 40 | 0 | task id is not a valid identity; nothing was resolved | 0 |
| session-window-identity | `--task firstmate:fm-birds-eye-capture` | 0 | unknown_identity | 40 | 0 | task id is not a valid identity; nothing was resolved | 4 |
| lines0 | `--lines 0` | 2 | no JSON | - | - | usage error on stderr | 0 |
| linesabc | `--lines abc` | 2 | no JSON | - | - | usage error on stderr | 0 |
| backend-selector | `--backend herdr` | 2 | no JSON | - | - | usage error on stderr | 2 |

The control run held the same window with no capture for 35 seconds, and its manifest changed on 8 lines.

### Returned line count

The live pane returned 31 lines for every request that succeeded.
The pane holds 24 visible rows plus 7 history lines in alternate-screen mode, so each capture returned everything the pane held, and `truncated` stayed false.
The 100-line tail on this lane's own pane is proven by fixture only because the pane held 31 lines; the disposable-lab drive below returned a real 100-line tail.

### Live-home diff attribution

The live-home diff lines name only this lane's watcher, lock-owner, hash, status, and turn-end records.
The no-capture control shows the same classes of churn.
The script's own write set is proven on an isolated copy instead, as described below.

## Boundary slip

During setup, the shell inherited `FM_STATE_OVERRIDE`, `FM_CONFIG_OVERRIDE`, `FM_DATA_OVERRIDE`, and `FM_PROJECTS_OVERRIDE` pointing at another home, `agent-tools/firstmate-pstack`.
One fleet-snapshot probe read that other home's state.
Its only documented write, the summary cache, was recorded as unchanged afterward.
No further probes went to that home, and the capture path pins its roots from `--root` explicitly.

## Side-effect audit

| Wrapper or path | Decision | Reason |
| --- | --- | --- |
| `bin/fm-peek.sh` | Not reused | It writes canonical guard and state records. |
| `bin/fm-guard.sh` | Not reused | It writes guard state. |
| `bin/fm-wake-lib.sh` | Not sourced | It writes lock and guard state. |
| `bin/fm-fleet-snapshot.sh` | Reused, once per capture | Overrides inherited from another home are cleared, and its summary cache is redirected to a private temp directory removed on exit. |
| `state/<task>.meta` | Read only | Two reads of the `spawn_gen` line, before and after the capture. |
| `tmux list-windows`, `tmux capture-pane -p` | The only tmux calls | Both are read-only, and the test log rejects any mutating tmux subcommand. |

### Isolated write-set proof

A copy of the home's `state`, `data`, and `config` trees, 121 files in total, had its watcher artifacts removed so nothing in the copy churns.
The default, 100-line, stale, and unknown cases each ran against that copy.
Each case changed zero files, with `copy_state_diff_lines=0` for all four.

## Disposable-lab live drive (head cd77e69)

The real `bin/fm-birds-eye-capture.sh` ran on head cd77e69 against real tmux panes on a private tmux socket, in a throwaway home under `$TMPDIR`, with a PATH shim that logged every `tmux` call and a stub `no-mistakes`.
Nothing touched the live home, no package was installed, and no backend other than tmux was used.
Commands had this form, with `<home>` the throwaway home:

    bin/fm-birds-eye-capture.sh --root <home> --task <id> --spawn-gen <gen> [--lines N]

| Case | Pane | Arguments | Exit | Status | Requested | Returned | Truncated | Reason |
| --- | --- | --- | --- | --- | --- | --- | --- | --- |
| default | 61 rows | (none) | 0 | ok | 40 | 40 | true | (empty) |
| lines100 | 61 rows | `--lines 100` | 0 | ok | 100 | 61 | false | (empty) |
| lines5 | 61 rows | `--lines 5` | 0 | ok | 5 | 5 | true | (empty) |
| lines101 | 61 rows | `--lines 101` | 0 | ok | 100 | 61 | false | requested lines clamped to 100 (v1 maximum) |
| lines99999 | 61 rows | `--lines 99999` | 0 | ok | 100 | 61 | false | requested lines clamped to 100 (v1 maximum) |
| 100-line tail | 151 rows | `--lines 100` | 0 | ok | 100 | 100 | true | (empty) |
| 100-line clamp | 151 rows | `--lines 5000` | 0 | ok | 100 | 100 | true | requested lines clamped to 100 (v1 maximum) |
| sparse | 3 rows | `--lines 10` | 0 | ok | 10 | 3 | false | (empty) |
| stale | 61 rows | `--spawn-gen sOLD` | 0 | generation_mismatch | 40 | 0 | false | requested spawn_gen is not the current incarnation; no output returned |
| unknown | - | `--task nope` | 0 | unknown_identity | 40 | 0 | false | task is not in the current fleet snapshot |
| pane-id identity | - | `--task %1` | 0 | unknown_identity | 40 | 0 | false | task id is not a valid identity; nothing was resolved |
| dead endpoint (tmux) | window absent | default | 0 | unavailable | 40 | 0 | false | Output unavailable: tmux could not read the endpoint |
| `--lines 0` | - | `--lines 0` | 2 | no JSON | - | - | - | `--lines must be at least 1` on stderr |
| `--lines abc` | - | `--lines abc` | 2 | no JSON | - | - | - | `--lines must be a whole number from 1 to 100` on stderr |
| `--lines -5` | - | `--lines -5` | 2 | no JSON | - | - | - | `--lines must be a whole number from 1 to 100` on stderr |
| backend selector | - | `--backend herdr` | 2 | no JSON | - | - | - | `unrecognized argument: --backend` on stderr |
| pane selector | - | `--target %1` | 2 | no JSON | - | - | - | `unrecognized argument: --target` on stderr |

The returned text of `--lines 5` was `tick-057` to `tick-060` plus `colored`, with no escape bytes.
The sparse pane returned `a`, `b`, `c` and no padding rows.
The 151-row pane returned `tick-052` through `colored`, which is the last 100 rows.
The stale, unknown, pane-id, and dead-endpoint results carried no text.

Observer-only proof: a `cksum` manifest of every file under the home's `state`, `data`, and `config` was identical before and after the cases (`manifest unchanged`), in each drive.
Every `tmux` call across the drive was `capture-pane`, `display-message`, or `list-windows`, all read-only.
Per-capture time in the first drive ranged from 15 s to 46 s on a loaded machine, and from 6 s to 9 s in the last drive, so the figures under Per-capture latency stay a sample and not a bound.

## Acceptance criteria

| Criterion | Status | Evidence |
| --- | --- | --- |
| AC1: on-demand; 40 default; 100 maximum; reports time, backend, truncation, and unavailable reason | Met for 40 lines on this lane's own live pane; the 100-line tail is proven by fixture and on a 151-row disposable-lab pane, not on a live agent pane. | Live default, lines40, lines100, lines101, and lines99999 cases; fixture 100-line test; disposable-lab 100-line tail and clamp rows. |
| AC2: observer-only; side-effect audit | Met | Side-effect audit table; isolated write-set proof; tmux log check in the test. |
| AC3: superseded generation rejected | Met | Live stale case; relaunch-during-capture test. |
| AC4: unsupported backends unavailable; tmux claimed only after a live capture succeeds | Met for tmux on this machine | Live captures of this lane's own pane succeeded; the test reports an unsupported backend as unavailable. |
| AC5: arbitrary identities, paths, and selectors rejected | Met | Live pane-id, session-window, malformed, and backend-selector cases; arbitrary identity test. |
| AC6: control sequences stripped and text plain | Met | The default test asserts zero control bytes on the returned text; the fixture emits SGR, OSC, and BEL. |
| AC7: commands and results recorded in this document | Met by this record; the live-pane 100-line request returned 31 lines, so the 100-line tail is proven by fixture and the disposable lab, not on a live agent pane. | This document. |

## Verification commands

    bash bin/fm-test-run.sh tests/fm-birds-eye-capture.test.sh
    bin/fm-lint.sh bin/fm-birds-eye-capture.sh tests/fm-birds-eye-capture.test.sh tests/fixtures/birds-eye/capture/pane-program.sh
    bash bin/fm-doc-audience-check.sh

Lint output: `fm-lint.sh: ShellCheck 0.11.0 (pinned 0.11.0)`, `fm-lint.sh: full ShellCheck extended analysis enabled`, exit 0.

## Limits

Two scenarios were not driven live and are recorded as untested, not as live passes:

- A `spawn_gen` change during a capture, rejected as `generation_mismatch`.
- An unsupported backend (Herdr), reported `unavailable` with a reason.

The repo test proves both on a real tmux server with fixture rows, using an injected meta change for the first.
It does not prove them against a live agent.
The dead-endpoint case was driven live on tmux only, with a missing window, and is in the disposable-lab table.
The disposable-lab panes ran shell loops, not a live agent.
This document makes no Herdr support claim; Herdr stays unverified.
On tmux 3.7 a window name cannot contain `.`, so a task id with a dot (legal as an id) never has a live tmux window there. The dotted-id regression proves that its live neighbor is not read; it cannot show a dotted window being captured.

## Open items

- The test takes about two minutes (109 s on the last run). No serial-shard placement hint is set, and none has been measured.
- Herdr capture is unverified because Herdr is not installed on this machine, so it stays `unavailable` here.
- The 100-line tail was proven live only on a disposable-lab pane of 151 rows, not on a live agent pane.
