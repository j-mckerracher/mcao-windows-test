# Birds-eye view validation report

Audience: maintainer verification.

This report maps the DPES-798 acceptance criteria and test plan items to their evidence on branch `fm/DPES-798-birds-eye-export`.
The branch builds on main, which includes the live service and snapshot CLI from PR 82.
It adds the offline snapshot exporter, the internal `birds-eye-view` skill, the fake-clock, network-isolation, and non-interference proofs, and the browser checks for the exported page and the dashboard.
It does not claim full Feature completion.
Each item marked partial names the reason in the gaps section.
Browser behavior under the network profile is NOT PROVED, skipped by explicit decision, because normal Chrome cannot start inside this host sandbox and no sandboxed browser route was found.

The exact commands, exit statuses, and raw evidence lines are recorded in [the export verification record](verification/birds-eye-export.md).

## Environment

- Date: 2026-10-19.
- Hardware: Apple M3 Pro, 11 logical cores, 36 GiB memory, model Mac15,6.
- Operating system: macOS 26.7.1 (25G241), Darwin 25.6.0, arm64.
- Node v26.8.1, Python 3.14.6, Google Chrome 155.0.8059.85.
- ShellCheck 0.11.0 and actionlint 1.7.12.

## Proof results

| ID | Proof | Command | Exit | Result |
|----|-------|---------|------|--------|
| P1 | Fake-clock collector | `python3 -I tests/fixtures/birds-eye/serve/virtual-clock.py`, run by the acceptance suite | 0 | 5 of 5 schedule scenarios hold: cadence, no overlap, catch-up, stop mid-cycle, and the 5 second floor |
| P2 | CLI refusals | acceptance suite and view suite | 0 | Interval 4 and a snapshot combined with `--port` exit 2 before anything starts |
| P3 | Network control | acceptance suite | 0 | Unsandboxed TCP connect to 1.1.1.1:443 succeeds; under the profile it fails with PermissionError, errno 1 |
| P4 | Snapshot CLI under the profile | acceptance suite | 0 | One file is written and the configured home is unchanged |
| P5 | Live service under the profile | acceptance suite | 0 | Serves `/` and `/api/view` with 200, stops on SIGTERM with exit 0, and leaves no owned process or working files |
| P6 | Exporter | `bash tests/fm-birds-eye-export.test.sh` | 0 | 8 of 8 scenarios |
| P7 | Dashboard in Chrome | `FM_BIRDS_EYE_UI=1 bash tests/fm-birds-eye-ui.test.sh` | 0 | 28 of 28 dashboard scenarios, including the render and interaction budgets, the keyboard attention path at desktop and 360 px width with five focus stops per run, a weakest focus outline of 9.65:1 against the surface it paints on, and state labels whose weakest measured contrast is 7.93:1 |
| P8 | Exported snapshot in Chrome | UI suite, unsandboxed | 0 | 11 of 11; no sandboxed run under the network profile exists (disclosure 2) |
| P9 | Aggregation, capture, view, and export | model, capture, view, and export suites | 0 | Model 32 of 32, capture 16 of 16, view 28 of 28, export 8 of 8 |
| P10 | Non-interference with an owned stand-in | acceptance suite | 0 | The owned stand-in beats before, during, and after the observer run and stop, is never stopped, and receives no signal; it is not a live agent |
| P11 | Read safety under the profile | acceptance suite | 0 | An ordinary read succeeds under the read-deny profile, four synthetic credential locations are refused with Operation not permitted, and the observer snapshot completes under the combined profile with no sentinel in its output; no read-level trace of the observer exists |
| P12 | Internal skill's default step with a populated two-project scratch home | `bin/fm-birds-eye-view.sh --snapshot` under the profile, against a home built with the real producer | 0 | roots=1 entities=4 attention=1 unsupported=native_subagents; no external references; the skill was loaded through the Skill tool in this session, and the agent-driven invocation under the network block is not exercised |

### P5 process ownership

The acceptance suite starts the observer with `--interval 5` in a synthetic empty home, under the profile.
`/api/view` and `/` both return 200 from loopback.
The loopback probe connects, and the external probe is refused with PermissionError.
SIGTERM stops the observer with exit status 0 and the stop line `birds-eye view stopped`.
The suite samples the observer's descendant processes while it runs and sees 15 of them in the recorded run, named `bash`, `jq`, and `perl`.
After exit, all of them are gone and the working directory is removed.
Sampling happens at runtime, so a short-lived child can exit between samples and be missed.
Earlier runs in this session observed 14 and 16 descendants, so the count is evidence of what was seen, not a full census.
The suite signals only the observer PID it started, and the observer signals only its own process groups.

## Acceptance criteria map

Status is `proved` when the cited evidence exercises the criterion as written, and `partial` when a stated part is not exercised.

| AC | Requirement, short form | Evidence | Status |
|----|-------------------------|----------|--------|
| AC-01 | Both projects appear by default, and root coverage is explicit | Model default run reads both configured roots and reports unobserved categories; UI project groups include Unassigned; the two-root tests use stored producer snapshots, and the real producer has run in snapshot mode on a populated two-project scratch home under the profile (P12), but not as a live run; the agent-driven skill invocation is not exercised | partial |
| AC-02 | Ownership stays correct, contextual copies are marked, counts deduplicate | Model overlapping-roots and real-producer scenarios; UI project groups show context copies | proved |
| AC-03 | Collapsed branches show descendant attention, and selection expands the path | UI collapsed-branch scenario, attention selection expansion scenario, and collapsed branches revealing their path | proved |
| AC-04 | Parent waits stay routine, and only an explicitly held decision reads Needs you | Model routine-parent scenario and model scenario for explicitly held decisions | proved |
| AC-05 | Silence alone does not change a running task to blocked, failed, or stalled | Model scenario `silence alone never reclassifies a running task`: a working task whose last status event is 10810 seconds old stays Running with active child work, state basis observed, and no transition or attention at two fake-clock times 60 seconds apart, with the source event age reading 10810 and then 10870; a blocked control with the same event age reads Blocked and externally held | proved |
| AC-06 | A failed root keeps last-good records, and recovery clears the error without transitions | Model scenarios for a failing root keeping last-good records, recovery without transitions, and a descendant behind an unreadable parent | proved |
| AC-07 | Task state and observation quality stay distinct, and caching does not change semantics | Model scenarios for authoritative state ages, missing states, and cached timing | proved |
| AC-08 | Unknown ages render as unknown, durations are labeled observed, no negative ages | Model timestamp-fault scenario; observed-duration scenario covering restart, generation change, and bad clock | proved |
| AC-09 | Earlier incarnation data is kept as history, and old-generation evidence is rejected | Model generation scenario; view generation-mismatch and forwarding scenarios; UI scenario that keeps a vanished selection as historical | proved |
| AC-10 | Requested values are labeled requested, the playbook reads Not recorded, nothing is invented | Model requested-execution scenario and null playbook when unobserved; UI execution-metadata scenario | proved |
| AC-11 | Capture is on demand, defaults to 40 lines, caps at 100, is read-only, and leaves state unchanged | Capture default scenario uses a real pane, checks plain text, compares the state manifest before and after, and confirms read-only tmux calls; view scenarios cover the default, the cap, and refused bounds | proved |
| AC-12 | Unsupported categories are explicit, and zero native subagents is never an observed count | Model default-run scenario reports the unobserved categories; the snapshot summary line reads `unsupported=native_subagents` and gives no count; UI scenario reads unsupported state values as Unknown | proved |
| AC-13 | Hostile text is inert, and terminal control sequences are stripped | Capture pane fixture with color and title sequences; UI hostile-metadata scenario; export escaping scenarios | proved |
| AC-14 | 200 entities, 10 projects, depth 8: first render within 2 s, interactions within 500 ms, refresh keeps the viewport | UI performance on the recorded hardware: first render 49.5 ms, every interaction under 500 ms with medians between 21.5 and 32.6 ms, viewport kept at 600; this is local render time only, and remote collection latency is unobserved, not zero, and no remote target is used | partial |
| AC-15 | Keyboard-only traversal from attention item to output and back, with visible focus and no color-only cues | UI keyboard scenarios at desktop and 360 px width assert each of five focus stops, from attention item through details, output load, output back, and the returned row, matches focus-visible with a 3 px outline; the outline is measured against its parent surface, which is where it paints, and the weakest measured in the normal Chrome launch was 9.65:1; the browser was not run under the network profile; the state-label scenario asserts each of 12 rendered state labels is text and measured at 4.5:1 or more, with the weakest at 7.93:1; color-coded cues other than state labels, such as freshness and attention badges, are not enumerated, and dark-mode tokens are not measured | partial |
| AC-16 | The exported file works offline, shows its capture time and snapshot status, and sends no live requests | Unsandboxed export browser run, 11 of 11, covering the snapshot label, frozen refresh controls, and the page loading only its own file with no remote references; browser behavior under the network profile is NOT PROVED, skipped by explicit decision, because the only run under that profile needed `--no-sandbox`, which is not approved (disclosure 2) | partial |
| AC-17 | Observer stop leaves agent sessions unchanged and leaves no collector | Live run under the profile exits 0 and leaves no descendants; view suite shows other processes left running; the non-interference check runs an owned stand-in process, not a live agent, which keeps beating, is never stopped, and receives no signal through the observer's run and stop; a sample of the agent process that ran the observer, read-only and taken in separate tool calls before, during, and after an owned observer run under the profile, found it in state S+ and sent it no signal; the sample reads process state only, so it cannot show whether a turn is running or blocked on a tool call | partial |
| AC-18 | Oldest inactive summaries are evicted past 1000, ancestry is protected, and the limit is disclosed | Model history-eviction scenario with protected context and disclosed limit | proved |
| AC-19 | With network blocked, local assets and local observations work without LLM calls, CDN requests, installs, or telemetry | Snapshot CLI and live service run under the deny-all-outbound profile and succeed; the feature makes no model or network call, and the profile denies outbound connections, as the external probe shows; the exported page under the profile is NOT PROVED, skipped by explicit decision (disclosure 2) | partial |
| AC-20 | The internal skill resolves scope, calls the script, opens the result, and reports coverage | The Skill tool loaded the birds-eye-view skill, and its default snapshot step was run under the profile against a populated two-project scratch home built with the real producer (P12): exit 0, roots=1, entities=4 across projects alpha (1) and beta (3), attention=1, native subagents disclosed as unsupported, and no external references in the output; the agent-driven invocation under the network block cannot run in-session, because the agent's own turn (its Skill call, reasoning, and reporting) runs outside the profile and wrapping it would cut its model transport; the open step (`open <path>`) was withheld, not failed, because it starts the default browser outside the owned process set and outside the profile; see the blocker evidence section of the verification record | partial |

## Test plan map

| T | Requirement, short form | Evidence | Status |
|---|-------------------------|----------|--------|
| T1 | Overlapping roots, shared supervisors, duplicate display names, unknown projects | Model overlapping-roots, supervisor-classification, and empty-project scenarios; UI project-group scenario | proved |
| T2 | Mixed states, collapsed failures, healthy-child waits, human decisions, quiet output | Model routine-parent, held-decision, and silent-running scenarios; UI collapsed-branch and attention scenarios | proved |
| T3 | Fake-clock per-root failure and recovery, minimum interval, staleness, separate ages | Virtual-clock suite for cadence, no overlap, and stop; model scenario in which failing and recovering roots age independently under the fake clock, keep last-good records, and make no task transitions, with separate event and collection ages; interval refusal exits 2 | proved |
| T4 | Missing or future timestamps, generation changes, disappeared selection | Model timestamp-fault and generation scenarios; UI vanished-selection scenario | proved |
| T5 | Requested-only and observed metadata, unsupported categories | Model requested-execution and unobserved-categories scenarios; UI label scenarios | proved |
| T6 | Capture bounds, provenance, stale generations, arbitrary identities, side effects | Capture default scenario with manifest comparison and read-only tmux calls; view capture scenarios for bounds, generation, and unknown identity | proved |
| T7 | Hostile content, unexpected Host and Origin, path and command requests | View scenarios for path refusal, foreign Host, and cross-origin requests; capture escape-sequence scenario; UI hostile-metadata scenario; read safety (P11): an ordinary read succeeds under the read-deny profile, four synthetic credential locations under the test root are refused with Operation not permitted, and the observer snapshot completes under the combined profile with no sentinel in its output; no read-level trace of the observer exists, and environment reads are not asserted (gap) | partial |
| T8 | 200+ entities on recorded hardware, with remote latency separate | UI performance on the recorded hardware: first render 49.5 ms with interactions under 500 ms; remote latency is unobserved, not zero, and no remote target is used | partial |
| T9 | Keyboard traversal, narrow viewport, no color cues | UI keyboard scenarios at desktop and 360 px width measuring each focus stop's outline against its parent surface, with the weakest at 9.65:1 in the normal Chrome launch; state-label scenario measuring 12 labels with the weakest at 7.93:1; color cues other than state labels are not enumerated | partial |
| T10 | Export, stop observer, block network, open HTML | Live stop with exit 0 and no leftover descendants; an owned stand-in process, not a live agent, runs through the observer's run and stop with no signal (AC-17); the browser run under the profile is NOT PROVED, skipped by explicit decision (disclosure 2) | partial |
| T11 | Over 1000 inactive summaries, restart, eviction and disclosure | Model history-eviction scenario | proved |
| T12 | Actual internal skill with network blocked | CLI under the profile is proved, including a populated two-project scratch home (P12); the agent's own skill invocation under the network block is not exercised (AC-20) | partial |

Totals: 13 of 20 acceptance criteria are proved, and 7 of 12 test plan items are proved.

## Disclosures

1. Numbering: the requirement's T12 covers both AC-19, network-blocked local operation, and AC-20, internal skill invocation.
   This report maps them separately.
2. `--no-sandbox`: Chrome's renderer sandbox does not start inside the Seatbelt profile, and a 2026-10-19 decision does not approve `--no-sandbox` or any other protection-disabling flag for browser runs.
   An earlier run of the exported-page driver under the profile passed 11 of 11 with `--no-sandbox`; that result is not counted, it is not repeated, and it is not evidence of offline browser behavior.
   The browser driver no longer adds `--no-sandbox` on its own when run as root, so a root run fails instead of silently weakening the check.
   The browser proof under the network profile is NOT PROVED, skipped by explicit decision, and no sandboxed browser route was found; it is not counted as passed or as an unapproved run.
   AC-16, AC-19, and T10 stay partial for the rest of their criteria.
3. External contact: each run of the acceptance suite makes one unsandboxed TCP handshake to 1.1.1.1:443 and sends no payload, as the control that the profile is then compared against.
   The target can be changed with `FM_BIRDS_EYE_EGRESS_TARGET`.
4. Self-connection: under the profile, a connection to the host's own LAN address succeeds, because it is local.
   It is not a control and is not used as evidence.
5. Deny rule: a bare `(deny network-outbound)` with loopback allowances enforces on this platform.
   `(deny network-outbound (remote ip "*:*"))` did not enforce in an earlier local run, which was not repeated, so the profile uses the bare deny.
6. Real-model smoke: the requirement's real-model smoke is taken to mean the real local producer, `bin/fm-birds-eye-model.sh`, and the real capture helper.
   The feature makes no LLM call, and no model was invoked during validation.
7. Populated fleet: the live smoke used a synthetic empty home, and the two-root tests read stored producer snapshots through `--fixture-dir`, not a run of the real producer on a populated home.
   The configured home snapshot shows one entity.
   The real producer ran in snapshot mode on a populated two-project scratch home (P12), but no live run on a populated home was made.
8. Mutant controls: the collector mutant (`wait(interval)` in place of `wait(remaining)`) fails the fake-clock suite with exit 1.
   It was run on a temporary copy, so it is not reproducible from the repository.
   An exporter escaping mutant was also run on a temporary copy during earlier work, and it is not used as evidence here.
9. Platform: the `xdg-open` path has not been run, because the verified platform is macOS.
10. Non-interference: the stand-in is an owned test process that stands in for an agent session, so it shows the observer does not signal or stop a process it does not own, not how a live agent session behaves.
11. Environment dependency, fixed in the filter: `bin/fm-birds-eye-model.jq` failed to parse under jq 1.7.1 with `syntax error, unexpected '+', expecting '}'` at lines 502 and 506, where an unparenthesized binary `+` sat inside an object value.
   The two reason fields are now parenthesized, and the model writes identical documents under jq 1.7.1 and jq 1.8.1 on the same fixed inputs.
   The repository still declares no minimum jq version, the capture tool table states jq 1.8.1, and the model does not check the version.
   The read-safety run puts a jq 1.8 directory first on `PATH`; the model's version handling is not changed here.
12. Agent observation: the AC-17 observation is one run of a sample of the agent process, taken from inside the session it observed, and is not part of the acceptance suite.

## Gaps

- AC-20, T12: the agent's own turn cannot be placed under the network block without cutting its model transport, so the agent-driven invocation under the block is blocked by design; the skill was loaded and its snapshot step run under the profile (P12), and the open step is not run.
- AC-01: the two-root tests use stored producer snapshots, and the real producer has run in snapshot mode only on a populated two-project scratch home (P12).
- AC-14, T8: the design asserts in the model suite that it does not perform concurrent collection.
- AC-15, T9: measure color-coded cues other than state labels, such as freshness and attention badges, and the dark color scheme; visible focus and state-label contrast are measured in the browser's default color scheme.
- AC-16, AC-19, T10: browser behavior under the network profile is NOT PROVED, skipped by explicit decision; no sandboxed route is known, because Chrome's renderer sandbox cannot start inside `sandbox-exec`, and no run with `--no-sandbox` is counted.
- AC-17: repeat the sample of the agent process over several turns and record whether a turn is in progress at each sample; the owned stand-in is not a live agent, and the sample in disclosure 12 is one run that reads process state only.
- T7: the read-deny run refuses four synthetic credential locations and the observer completes without them, but no read-level trace of the observer exists; the macOS unified log showed no sandbox denial records from this process, and the `(with report)` modifier is rejected on deny rules, so environment reads are not asserted.
- Platform: run the `xdg-open` path on Linux.
- Populated fleet: run the live service against a populated two-project home.

Feature completion is not claimed.
