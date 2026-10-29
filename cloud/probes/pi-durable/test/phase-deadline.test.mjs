// Regression (DPES-868 PR 145 review, finding 5): every child phase has a deadline. A phase that
// stalls is killed at the deadline, reported by name, and its process is reaped.
// PROBE_TEST_STALL (fixture/child.mjs) makes the named phase hang after its harness opens. The
// fixture's own safety exit fires 30 s later, so a deadline that never kills the child still
// rejects with the deadline message, and only the elapsed-time bound tells the two apart.
import { test } from "node:test";
import assert from "node:assert/strict";
import { mkdtempSync, readFileSync, rmSync } from "node:fs";
import { tmpdir } from "node:os";
import { join } from "node:path";
import { runScenario } from "../lib/driver.mjs";

// Node startup and any earlier phase fit in this slack; it stays well under the 30 s safety exit.
const DEADLINE_SLACK_MS = 15_000;

function assertKilledAtDeadline(startedAt, timeoutMs) {
	const elapsed = Date.now() - startedAt;
	assert.ok(
		elapsed < timeoutMs + DEADLINE_SLACK_MS,
		`the stalled phase must be killed at its ${timeoutMs} ms deadline, not by the fixture's 30 s safety exit (took ${elapsed} ms)`,
	);
}

function stalledPid(runDir) {
	return Number(readFileSync(join(runDir, "markers", "second-stalled"), "utf8"));
}

test("a second phase that stalls after its barrier is killed at the deadline and reaped", { timeout: 60_000 }, async () => {
	const tmp = mkdtempSync(join(tmpdir(), "dpes-868-deadline-second-"));
	try {
		const runDir = join(tmp, "sigkill-input");
		const startedAt = Date.now();
		await assert.rejects(
			runScenario("sigkill-input", runDir, { extraEnv: { PROBE_TEST_STALL: "second" }, timeoutMs: 8_000 }),
			/sigkill-input\/second exceeded 8000 ms/,
		);
		assertKilledAtDeadline(startedAt, 8_000);
		const pid = stalledPid(runDir);
		assert.ok(Number.isInteger(pid) && pid > 0, "the stalled child recorded its pid before the deadline");
		assert.throws(() => process.kill(pid, 0), { code: "ESRCH" });
	} finally {
		rmSync(tmp, { recursive: true, force: true });
	}
});

test("a first phase with no barrier that stalls is killed at the deadline and reaped", { timeout: 60_000 }, async () => {
	const tmp = mkdtempSync(join(tmpdir(), "dpes-868-deadline-first-"));
	try {
		const runDir = join(tmp, "provider-denied");
		const startedAt = Date.now();
		await assert.rejects(
			runScenario("provider-denied", runDir, { extraEnv: { PROBE_TEST_STALL: "first" }, timeoutMs: 5_000 }),
			/provider-denied\/first exceeded 5000 ms/,
		);
		assertKilledAtDeadline(startedAt, 5_000);
		const pid = Number(readFileSync(join(runDir, "markers", "first-stalled"), "utf8"));
		assert.ok(Number.isInteger(pid) && pid > 0, "the stalled child recorded its pid before the deadline");
		assert.throws(() => process.kill(pid, 0), { code: "ESRCH" });
	} finally {
		rmSync(tmp, { recursive: true, force: true });
	}
});
