# Mutation container supervision

The container-supervised mode runs the unchanged mutation runner inside one disposable container.
In the cases reproduced for this mode, a detached descendant that leaves the runner's process group ends with the container's process namespace instead of outliving the run.
`bin/fm-mutation-container-run.sh` owns the exact arguments, limits, naming, and exit codes in its header and `--help`; this page states what the mode contains, what it does not, and the conditions it depends on.
The host runner, `bin/fm-mutation-run.sh`, is unchanged and gives no containment.

## Scope

- The mode judges one work tree per invocation and passes its runner options through unchanged.
- It does not change the runner, the classifier, the verdict, the dispositions, or the Mutation verdict check.
- It needs a container runtime and an image already present locally, and it never pulls or builds an image.
- It refuses with no record when the runtime, the image, the read-back of the container limits, or a verified removal is unavailable, and it never falls back to host execution.

## Running it

Run the same chain as [the local run in CONTRIBUTING](../CONTRIBUTING.md#run-locally), with the wrapper in place of the runner:

```sh
bin/fm-mutation-container-run.sh --root . <"$mut/classified" >"$mut/record"
```

- Check the classifier's exit status first, exactly as for the bare runner.
- The wrapper buffers its classifier input into a regular file before the runner reads it, so an open pipe is never used.
- `--container-timeout` sets the outer bound on the whole container run, and `--help` gives its default.

## Supervision token

A forwarded record carries the token `supervision=container` at the end of its published summary row's detail column.
Only this mode adds the token, so a record without it came from the host runner and makes no containment claim.
The verdict and dispositions parsers read the published flag and the result tokens only, so the token changes neither.
The host runner's record has no explicit host-mode statement, because the runner stays unchanged, so the missing token is the only marker of a host run.
This is weaker than an explicit statement, and a reader must treat a record without the token as making no containment claim.

## Process behavior

The runner is PID 1 in the container, so a detached descendant reparents to PID 1.
When PID 1 exits, the kernel tears down the container's process namespace and ends the descendant with it.
The live test, `tests/fm-mutation-container-run-live.test.sh`, checks this on a real container; it is opt-in and was not run for this change.
The residual therefore ends at runner exit, which comes before the container removal step; removal still runs and is verified by name.
A wrapper killed with KILL runs no exit path, so its watchdog removes the container by name once the wrapper is gone; nothing reports that removal, and the wrapper's scratch directory is left behind.

## Conditions

Each condition states what the mode does and whether this change verified it.
A claim with no status here is not made.

1. Limits and image: the wrapper creates the container pinned to `linux/amd64` with `--pull never --network none --cpus 1 --memory 512m --pids-limit 128` on `node:24-bookworm`, reads those limits back from the created container before it starts, and refuses when the image is absent or a limit does not read back.
   The wrapper states the pinned platform on stderr before it creates the container.
   The stub test covers the refusals and that stderr line; the read-back against a real runtime was not run for this change.
   On an arm64 host the pinned amd64 image runs under emulation; that was not run for this change either.
2. Removal by name: every run has a unique name, the container is removed by that exact name on every exit path, and the wrapper checks by name that nothing remains.
   A removal that cannot be verified writes no record.
   The container is also created with `--rm`, so the runtime reaps it when the runner exits.
3. Outer bound: the wrapper's watchdog runs `docker rm -f` on the named container when the outer bound expires, because a timeout on the docker client alone does not stop the container.
   The stub test covers the outer-bound removal path; a real-container check of the outer bound was not run for this change.
4. Classifier input: the input is a regular file, never an open pipe, as described above.
5. Stop codes: the shared oracle mapping in `bin/fm-mutation-exec-lib.sh` records oracle exit 124 as a timeout and exit codes 126, 127, and 128 through 255 as errors, never as kills.
   Exit code 1 and the codes 2 through 123 and 125 remain kills; an oracle that exits with one of those is taken as a verdict.
   This mode runs the same mapping and does not change it.
6. Probe timeout: a reader of a probe timeout reads the path row and ignores the `tool` row's `installed` label, as for the host runner.
7. Scope of claims: a containment claim covers only the cases reproduced for this mode, not native Linux, not cgroup containment, not a real process-group setup failure, and not a reap that survives KILL.
8. Between oracle runs: a detached descendant that leaves an oracle's process group is not stopped when that oracle ends; it lives until the runner exits, and no claim covers the window in between.

## Verification

- `bash tests/fm-mutation-container-run.test.sh` uses a stub `docker` on `PATH` and needs no runtime; it runs under `bin/fm-test-run.sh` as a pure contract unit.
- `FM_LIVE_MUTATION_CONTAINER=1 bash tests/fm-mutation-container-run-live.test.sh` runs a real container with a detached sleep in the oracle.
  It fails rather than skips once requested, and a default run prints a skip line.

## Boundary

This mode is evidenced and has not been independently reviewed.
Its claims stop at the cases its tests and containment evidence reproduce.
The baseline work tracked as DPES-825 stays container-only until an independent reviewer verifies this boundary, and nothing in this page or in the tests certifies it.
