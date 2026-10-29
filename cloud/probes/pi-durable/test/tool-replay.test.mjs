// T4 / AC4: replay-safe tools rerun, default tools are not rerun, streamed output survives (DPES-868 section 8).
import { test, before, after } from "node:test";
import assert from "node:assert/strict";
import { mkdtempSync, rmSync } from "node:fs";
import { tmpdir } from "node:os";
import { join } from "node:path";
import { runScenario } from "../lib/driver.mjs";

let tmp;
let safe;
let unsafe;

before(async () => {
	tmp = mkdtempSync(join(tmpdir(), "dpes-868-t4-"));
	safe = await runScenario("tool-safe", join(tmp, "tool-safe"));
	unsafe = await runScenario("tool-unsafe", join(tmp, "tool-unsafe"));
});

after(() => {
	rmSync(tmp, { recursive: true, force: true });
});

const recoveredOf = (facts) => facts.events.find((e) => e.phase === "second" && e.event === "recovered");

test("replay-safe tool reruns on recovery: effect ledger has two rows for one call", () => {
	assert.equal(safe.ledger.filter((l) => l.tool === "ledger_safe").length, 2);
	assert.deepEqual(
		safe.ledger.map((l) => [l.phase, l.tag]),
		[
			["first", "call-1"],
			["second", "call-1"],
		],
	);
});

test("replay-safe rerun produces a successful committed result and no scripted response is left", () => {
	const rec = recoveredOf(safe);
	assert.equal(rec.settledStatus, "done");
	assert.equal(rec.toolResults.length, 1);
	assert.equal(rec.toolResults[0].toolName, "ledger_safe");
	assert.equal(rec.toolResults[0].isError, false);
	assert.equal(rec.pendingScriptedResponses, 0);
});

test("non-replay-safe tool is not rerun on recovery: effect ledger has one row", () => {
	assert.equal(unsafe.ledger.filter((l) => l.tool === "ledger_unsafe").length, 1);
	assert.deepEqual(
		unsafe.ledger.map((l) => l.phase),
		["first"],
	);
});

test("non-replay-safe call is recorded as interrupted, not as a fresh result", () => {
	const rec = recoveredOf(unsafe);
	assert.equal(rec.settledStatus, "done");
	assert.equal(rec.toolResults.length, 1);
	assert.equal(rec.toolResults[0].toolName, "ledger_unsafe");
	assert.equal(rec.toolResults[0].isError, true);
	assert.deepEqual(rec.toolResults[0].diagnosticCodes, ["interrupted"]);
	assert.equal(rec.toolResults[0].interruptedMarker, true);
	assert.equal(rec.pendingScriptedResponses, 0);
});

test("output streamed before the kill is committed in the interrupted result", () => {
	const rec = recoveredOf(unsafe);
	assert.equal(rec.toolResults[0].streamedOutputCommitted, true);
});

test("replay-safe rerun commits its streamed output too", () => {
	assert.equal(recoveredOf(safe).toolResults[0].streamedOutputCommitted, true);
});
