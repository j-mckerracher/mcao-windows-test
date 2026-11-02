# Birds-eye export verification record

Audience: maintainer verification.

This record holds the commands, exit statuses, environment, and disclosures behind the DPES-798 proofs.
The requirement-by-requirement map is in [the validation report](../birds-eye-view-validation.md).
It records no process identifiers and no host network addresses.

## Environment

- Date: 2026-10-19.
- Operating system: macOS 26.7.1 (25G241), Darwin 25.6.0, arm64.
- Hardware: model Mac15,6, Apple M3 Pro, 11 logical cores, 36 GiB memory.
- Node v26.8.1, Python 3.14.6.
- Google Chrome 155.0.8059.85 at `/Applications/Google Chrome.app/Contents/MacOS/Google Chrome`.
- ShellCheck 0.11.0, actionlint 1.7.12.
- Suites run under `bash` explicitly, because the default tool shell is zsh.

## Commands and exit statuses

Each command was run from the worktree root, and each exit status was captured directly.
The results were measured on the tree this record ships with, which differs from the tree it describes only by this file.

| Command | Exit | Result |
|---------|------|--------|
| `PATH=/opt/homebrew/bin:/usr/bin:/bin bash tests/fm-birds-eye-model.test.sh` | 0 | 34 ok lines, 0 not ok, jq-1.8.1 |
| `PATH=/usr/bin:/bin:/opt/homebrew/bin bash tests/fm-birds-eye-model.test.sh` | 0 | 34 ok lines, 0 not ok, jq-1.7.1-apple |
| `bash tests/fm-birds-eye-capture.test.sh` | 0 | 16 ok lines, 0 not ok |
| `bash tests/fm-birds-eye-view.test.sh` | 0 | 28 ok lines, 0 not ok |
| `bash tests/fm-birds-eye-export.test.sh` | 0 | 8 ok lines, 0 not ok |
| `FM_BIRDS_EYE_UI=1 bash tests/fm-birds-eye-ui.test.sh` | 0 | 41 ok lines, 0 not ok; dashboard 28 of 28, exported snapshot 11 of 11 |
| `bash tests/fm-birds-eye-view-acceptance.test.sh` | 1 | 6 ok lines, then `live interval below the floor is refused: expected exit 2, got 1`; the suite stops there |
| `HOME=<scratch dir containing .motif/multi-agent> bash tests/fm-birds-eye-view-acceptance.test.sh` | 0 | 12 ok lines, 0 not ok, including the sandbox, read-safety, and non-interference checks |
| `bin/fm-lint.sh` | 0 | ShellCheck 0.11.0 in changed-file mode, actionlint 1.7.12, 9 workflow files valid |
| `bin/fm-doc-audience-check.sh` | 1 | unclassified: `tests/fixtures/panel-assignments/candidate/evidence-output.txt`; not changed by this branch and failing on origin/main |

The UI suite finds Chrome at the standard install path and does not set `FM_BIRDS_EYE_UI_CHROME`.
The dashboard run checks each keyboard focus stop's outline against the surface it paints on, which is the focused element's parent, because the outline sits outside the element.
The focus check reports the weakest stop, 9.65:1 in this run on the normal Chrome launch, which keeps Chrome's own sandbox on and adds no flags.
The state-label check reads the text against the element it sits in.
Neither figure is measured under the network profile.
The acceptance suite is default-on because `sandbox-exec` and `curl` are installed.

All rows were measured on 2026-10-20 on this branch rebased onto current origin/main.
The acceptance suite fails on that base under the operator's real home because origin/main (DPES-682, #124) makes the default home `~/.motif/multi-agent`, and this Mac does not have that directory.
The check runs before the interval check, so the refusal case exits 1 instead of 2.
This branch does not change the CLI or the acceptance suite, and a clean export of origin/main fails the same way.
The 12-ok row above uses a scratch `HOME` that contains the directory, and it is the only run that reaches the sandbox sections on this Mac.
One run of the jq 1.7.1 model suite on the branch before the rebase failed the assertion that the live-producer test's two roots both report `ok`, at a load average near 20.
Run alone at that load, the producer took 149 seconds against the 30-second bound the model applies, so a timed-out root is the probable cause; this record does not observe the timeout directly.
An immediate rerun on the same tree passed 34 of 34 under jq 1.7.1, and the jq 1.8.1 run passed 34 of 34.
The failed run is not counted as a pass.
Setting `FM_BIRDS_EYE_SANDBOX_PROOF=0` skips it, and `FM_LIVE=1` forces it on.

The explicit-path form of `bin/fm-lint.sh` takes shell scripts only.
Passing a Python or JavaScript file to it fails with SC1071, so the fixture programs are not linted by it.

### Browser under the profile

No browser run under the network profile is counted.
Chrome's renderer sandbox does not start inside the Seatbelt profile, and the only combination that started was one that adds `--no-sandbox`.
That flag is not approved, so the browser's offline behavior under the profile is NOT PROVED, skipped by explicit decision, and no sandboxed route was found.
An earlier run with that flag passed 11 of 11; it is not repeated and is not counted as evidence.

### Documented snapshot command

The command documented in the internal skill was run unsandboxed.

```sh
bin/fm-birds-eye-view.sh --snapshot state/birds-eye/snapshot.html
```

It exited 0 and printed the two output lines the skill describes.
`state/` is gitignored and holds the private configured home, so its output file is not part of the change.

## Network isolation profile

The profile is `tests/fixtures/birds-eye/serve/loopback-only.sb`, and the acceptance suite applies it with `/usr/bin/sandbox-exec -f`.

```text
(version 1)
; Network-isolation profile for the bird's-eye live-service and snapshot proof (DPES-798, AC-19/AC-20, T12).
; Everything except the network is allowed. Outbound network is denied by default, and only
; loopback addresses and local UNIX sockets are allowed. A bare deny of network-outbound is
; the control that holds; a deny that names remote "*:*" does not hold on this platform.
(allow default)
(deny network-outbound)
(allow network-outbound (remote ip "localhost:*"))
(allow network-outbound (remote unix-socket))
(allow network-bind (local ip "localhost:*"))
(allow network-inbound (local ip "localhost:*"))
```

### Bare deny and the wildcard form

An earlier local run in this session used `(deny network-outbound (remote ip "*:*"))` in place of the bare deny.
An outbound connection to the control target still completed under that form on this platform.
That form was dropped, and the bare deny is the one the suite applies.
The output of that earlier run was not kept, so this observation is recorded from notes and was not re-run.

### Control and denied egress

The egress probe is a standard-library TCP connect with a 5 second timeout and no payload.
The acceptance suite first runs it without the sandbox as the control, then under the profile.

```text
evidence: control, no sandbox: connect ok host=1.1.1.1 port=443 rc=0
evidence: egress under profile: connect failed errno=1 PermissionError rc=3
```

The control is a live external contact.
Each run of the acceptance suite makes one unsandboxed TCP handshake to `1.1.1.1:443` and sends no payload.
The target can be changed with `FM_BIRDS_EYE_EGRESS_TARGET`.

### Local connections are not a control

Under the profile, a connection to the host's own LAN address succeeds.
That address is local to this machine, so the success shows nothing about outbound isolation.
The acceptance evidence uses the external target for the denial and the loopback address for the allowed case.

## Live service under the profile

The live smoke starts the observer with `--interval 5` in a synthetic empty home, under the profile.

```sh
FM_HOME="$LIVE_HOME" /usr/bin/sandbox-exec -f tests/fixtures/birds-eye/serve/loopback-only.sb bin/fm-birds-eye-view.sh --interval 5
```

The recorded evidence lines, with the process identifier and ephemeral port removed, were:

```text
evidence: live observer exit_status=0 stop_line=birds-eye view stopped
evidence: live api_view=200 root_page=200 descendants_observed=15 names=(bash) (jq) bash perl all_gone_after_exit=yes
evidence: live loopback_probe=[connect ok host=127.0.0.1] external_probe=[connect failed errno=1 PermissionError] working_files_removed=yes configured_home_unchanged=yes
```

The suite stops the observer with `kill -TERM` on the one PID it started, and it does not use a name pattern.

### Process ownership

The suite samples the observer's descendant processes at runtime, and the names are read in the same sample.
The sampler walks parent links from the observer's PID, so it sees only that observer's tree.
A child that exits between two samples can be missed, so `descendants_observed` is a count of what was seen, not a census.
Across runs this session the count was 14, 15, and 16, and the names included `jq`, `bash`, and `perl`.
After exit, every sampled descendant was gone and the working directory was removed.

### Shutdown behavior

The observer signals only the process groups it started, first with SIGTERM and then with SIGKILL after a grace period.
It prints `birds-eye view stopped` and exits 0 on a clean stop.

## Non-interference with an owned stand-in

The acceptance suite starts a test-owned stand-in process, `tests/fixtures/birds-eye/serve/standin-agent.py`, in its own session.
The stand-in is not a live agent.
It keeps a heartbeat counter current and appends the number of each signal it receives to a record file.
A positive control sends SIGUSR1 to the stand-in first and confirms the record catches it.
The observer then runs under the profile and stops, and the stand-in must keep beating, stay out of the stopped state, and record no signal.
The suite stops the stand-in with SIGKILL on the one PID it recorded.

```text
evidence: non-interference stand-in (owned test process, not a live agent) beats before=2 during=8 after=15 state_before=Ss state_after=Ss signals=0 observer_exit=0
```

This check does not prove behavior with a live agent session, so AC-17 and T10 remain partial.

## Read safety under the profile

The acceptance suite runs a read-safety check against synthetic credential locations under its temporary root, so no real credential is read.
The read-deny profile is `tests/fixtures/birds-eye/serve/read-deny.sb`, and the snapshot runs under it combined with the network profile, in a minimal environment.

```text
evidence: read-safety control=ok denied=4 of 4 (Operation not permitted, no sentinel) observer_rc=0 sentinel_in_output=none configured_home_unchanged=yes
```

The control reads an ordinary file under the read-deny profile, and each denied location is first read without the sandbox to confirm it yields its sentinel.
This shows the observer completes without those paths; it does not record every file the observer reads, so it is not a read-level trace.
The minimal environment sets `PATH` to the directory of a jq 1.8 binary followed by `/usr/bin` and `/bin`; the model runs under either jq version (see the jq 1.7 section), so this choice is not required for the model.

## Snapshot through the internal skill's default step

The internal skill was loaded through the Skill tool, and its default step was run against a populated two-project scratch home built with the real producer, under the network profile.
The command was `FM_HOME=<scratch home> /usr/bin/sandbox-exec -f tests/fixtures/birds-eye/serve/loopback-only.sb bin/fm-birds-eye-view.sh --snapshot <snapshot>`, and it exited 0.

```text
wrote <snapshot> | roots=1 entities=4 attention=1 unsupported=native_subagents captured_at=2026-10-19T18:45:21Z
snapshot written: <snapshot>
```

The snapshot holds 4 entities across projects `alpha` (1) and `beta` (3), contains no external references, and discloses native subagents as unsupported.
The skill's open step and an agent-driven invocation under the network block were not exercised.
The scratch home lives under `state/`, which is gitignored, so it is not part of the change.

## Blocker evidence for the open items

Recorded 2026-10-19.
Each item lists what was run and what it showed, and nothing here changes the model, the filter, or a version declaration.

### Skill invocation and the agent-driven flow

The skill was loaded through the Skill tool with the name `birds-eye-view`, and its body loaded in the session.
Its default step ran under the network profile in the same session, as recorded in the snapshot section above.
The agent's own turn cannot run under the network profile, because the profile would cut the session's model connection.
So the agent-driven invocation under the block is blocked by the session transport, and no tool error caused it.
The observer process is the part that the profile can contain, and the session transport is outside it.
The open step (`open <path>`) was withheld by choice, not because it failed, because it starts the default browser outside the owned process set and outside the profile.

### Observing the owned agent session

The session's agent process was sampled read-only in three separate tool calls, and no other process was read or signalled.
The observer was started in the scratch home under the profile, and it was stopped with `kill -TERM` on the one process id it started.

```text
evidence: agent-session before observer state=S+ direct_children=0
evidence: agent-session during observer state=S+ direct_children=0 observer_api_view=200 observer_direct_children=1 signals_sent_to_session=0
evidence: agent-session after stop state=S+ direct_children=0 observer_exit=0 stop_line=birds-eye view stopped listener_closed=yes working_files_removed=yes
```

The observer's parent became the system process after its launching command returned, so the session's child count stayed at zero at every sample.
The sample reads process state only, so it does not show whether a turn is running or blocked on a tool call.
It was taken from inside the observed session and is one run.

### jq 1.7 and the model filter

The versions are `/usr/bin/jq` `jq-1.7.1-apple` and `/opt/homebrew/bin/jq` `jq-1.8.1`.
The repository declares no minimum jq version, `docs/birds-eye-view-capture.md` lists `jq-1.8.1`, and `bin/fm-birds-eye-model.sh` runs `jq -n -f` without a version check.
The exact failure is the following, from `jq -n -f bin/fm-birds-eye-model.jq` under jq 1.7.1, exit 3.

```text
jq: error: syntax error, unexpected '+', expecting '}' (Unix shell quoting issues?) at <top-level>, line 502:
jq: error: syntax error, unexpected '+', expecting '}' (Unix shell quoting issues?) at <top-level>, line 506:
```

Lines 502 and 506 are the same construct, a binary `+` after an unparenthesized value inside an object's `reason:` field.
The same file under jq 1.8.1 has no syntax error, and its first error is `$config is not defined` at line 322, which is an undefined-variable error from compiling without the run-time arguments.
A minimal case, `{ k: [{c: 1, reason: (1 | tostring) + " tail"}] }`, fails under jq 1.7.1 and parses under jq 1.8.1.
The same case with the whole value wrapped, `reason: ((1 | tostring) + " tail")`, parses under both.
The failure does not depend on `PATH` or on the Seatbelt profile, because `env -i PATH=/usr/bin:/bin` resolves jq 1.7.1, the same parse error reproduces unsandboxed, and only the choice of binary changes with `PATH`.
The two reason values on lines 502 and 506 are now wrapped in one more pair of parentheses, and no other filter line changed.
The focused regression `test_reason_fields_render_under_every_host_jq` fails under jq 1.7.1 before that change and passes after it.
With full actual arguments on fixed inputs (a remote-home scenario, a deep ownership-cycle chain, and two roots), the model exits 0 with empty stderr under both jq 1.7.1 and jq 1.8.1, and the documents are byte-identical between the two versions.

### CI lane and run state

`bin/fm-test-run.sh --list --lane portable-serial` and `--list --lane portable-serial-9of9` both list `tests/fm-birds-eye-model.test.sh`, and the family is `snapshot-bearings`.
CI run 39305561534, on head `378898073d1dabb1c43a33211909facf49e152b4`, was still in progress when checked through `gh-axi run view`.
The wrapper refused its logs with `run 39305561534 is still in progress; logs will be available when it is complete`, so no job log was read through the wrapper.
On this Mac the model suite passes under jq 1.7.1, as the commands table records, so the regression is observed locally.
Whether the CI runner reaches the same result is still predicted, not observed, because run 39305561534's logs were not readable.

## Mutant controls

Two mutants were run during earlier work, each on a temporary copy of the tree.
The collector mutant replaces `self.stop.wait(remaining)` with `self.stop.wait(self.interval)`.
The fake-clock suite fails it with exit 1.
An exporter escaping mutant was also run during earlier work, and its result is not used as evidence in this record.
Neither mutant is part of the repository, so neither can be reproduced from the tree.
