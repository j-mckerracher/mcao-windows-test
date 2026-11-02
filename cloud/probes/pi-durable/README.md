# pi-durable compatibility and crash-recovery probe

DPES-868. This package checks the pinned `@earendil-works/pi-durable` 1.3.0 runtime for the behaviors the MCAO cloud pilot depends on: input identity across a SIGKILL, tool replay, a deleted checkout, and provider denial.

This is local fixture evidence. It is not production durability evidence, and it does not say the pilot is ready.

## Pins

Four packages are pinned exactly in `package.json` and locked in `package-lock.json`:

- `@earendil-works/pi-durable` 1.3.0
- `@earendil-works/pi-ai` 1.3.0
- `@earendil-works/chord` 1.3.0
- `@earendil-works/pi-codemode` 1.3.0

The probe requires Node 22.19.0 or later.

## Test command

Install the locked dependencies without running install scripts, then run the suite:

```sh
npm --prefix cloud/probes/pi-durable ci --ignore-scripts
npm --prefix cloud/probes/pi-durable test
```

The suite runs in about 15 seconds. It starts real child Node processes, sends them SIGKILL, and resumes them against the same SQLite store. It makes no network calls and no cloud calls.

## What the tests cover

| Test file | Requirement | What it asserts |
| --- | --- | --- |
| `test/evidence-schema.test.mjs` | Evidence record (AC1, section 5.6) | Versions and integrity recorded, every required check present, pilot readiness false, validator rejects malformed records |
| `test/sigkill-resume.test.mjs` | AC3 | SIGKILL after input commit; resume reopens the same root and submission; duplicate requestId dedups |
| `test/tool-replay.test.mjs` | AC4 | Replay-safe tool reruns; default tool does not; interrupted result and streamed output are committed |
| `test/missing-workspace.test.mjs` | AC5 | Transcript survives a deleted checkout; a read after deletion fails with ENOENT, reported as a gap |
| `test/sentinel-and-blocked.test.mjs` | AC6, T2 | Injected provider denial fails once with no fallback; sentinel scanner catches planted strings; live gate stays blocked |

Every local run uses an in-process faux model provider and no sandbox. The tests say so in their output and in the evidence record.

## Probe run and evidence

```sh
node cloud/probes/pi-durable/run-probe.mjs --out <evidence-dir> --scratch <run-dir>
```

`--out` receives `report.json`, `report.md`, and `scenarios/<name>/{events,ledger}.jsonl`. `--scratch` receives SQLite stores, workspaces, and logs. Those hold transcripts, so keep `--scratch` outside the evidence directory. Omit `--scratch` to use a fresh temporary directory.

Exit codes: 0 when no check fails, 1 when a check fails, 2 on usage or infrastructure errors. Blocked checks do not change the exit code. They appear in the report with their prerequisites.

The probe refuses to publish evidence when a sentinel pattern (credential, signed URL, prompt, or transcript text) appears in the report, the copied files, or the run logs.

## Blocked live path (AC2, T2)

Two checks need a live Vertex model call and an outside-workspace write denial under bubblewrap. Neither ran. `lib/gate.mjs` reports these prerequisites without touching the network:

- A Linux host with `bwrap` on `PATH`. The current development host is macOS.
- The approved Cloud Run worker image digest, supplied as `PROBE_IMAGE_DIGEST` (DPES-811).
- The approved Vertex model ID, supplied as `PROBE_VERTEX_MODEL` (DPES-813).
- The Vertex location, supplied as `PROBE_VERTEX_LOCATION`.
- Vertex metadata-server authentication and Cloud Run IAM, unverified (DPES-804).
- The live runner, which this change does not implement.

Even when every input is supplied, the gate reports not ready. The probe never falls back to an unsandboxed run or to another provider.

## Known limits of the local evidence

- SQLite in WAL mode with synchronous NORMAL survives a process crash, not a power loss.
- The library provides no cross-process locking. One process owns a store at a time, and nothing here enforces that across hosts.
- The one-minute periodic checkpoint cadence is not exercised. Barrier markers mark the crash points instead.
- A deleted checkout is not restored. The read after deletion fails, and the probe reports that as a gap for DPES-869.

The report's `unresolvedDecisions` section lists the owner for each open question.

## Layout

- `fixture/` runs one child process per phase and defines the scenarios.
- `lib/driver.mjs` starts the children, sends the SIGKILL, and reads the files they wrote.
- `lib/checks.mjs` maps observed facts to named checks.
- `lib/evidence.mjs` builds, validates, and renders the record.
- `lib/sentinels.mjs` scans evidence and logs.
- `lib/gate.mjs` reports live prerequisites.
- `lib/probe.mjs` runs the whole probe.
- `run-probe.mjs` is the command-line entry point.
