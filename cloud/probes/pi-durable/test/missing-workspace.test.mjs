// T5 / AC5: transcript survives a deleted checkout, but coding work cannot resume (DPES-868 section 8).
// This is a reported gap, not a pass: the recovery read fails with ENOENT and the probe says so.
import { test, before, after } from "node:test";
import assert from "node:assert/strict";
import { mkdtempSync, rmSync } from "node:fs";
import { tmpdir } from "node:os";
import { join } from "node:path";
import { runScenario } from "../lib/driver.mjs";

let tmp;
let facts;
let recovered;

before(async () => {
	tmp = mkdtempSync(join(tmpdir(), "dpes-868-t5-"));
	facts = await runScenario("workspace-delete", join(tmp, "workspace-delete"));
	recovered = facts.events.find((e) => e.phase === "second" && e.event === "recovered");
});

after(() => {
	rmSync(tmp, { recursive: true, force: true });
});

test("the checkout is deleted between the two processes", () => {
	assert.equal(facts.workspaceDeletedBetweenProcesses, true);
	assert.equal(recovered.workspaceExists, false);
});

test("the transcript survives: same root, submission settles done", () => {
	assert.equal(recovered.sameRoot, true);
	assert.equal(recovered.settledStatus, "done");
});

test("the read result committed before deletion is retained", () => {
	const [priorRead] = recovered.toolResults;
	assert.equal(priorRead.toolName, "read");
	assert.equal(priorRead.isError, false);
});

test("a read after deletion fails with ENOENT, so coding work cannot resume on this checkout", () => {
	const failing = recovered.toolResults.filter((t) => t.toolName === "read" && t.isError);
	assert.equal(failing.length, 1);
	assert.equal(failing[0].errorClass, "ENOENT");
});

test("no error text is recorded, only the error class token", () => {
	for (const result of recovered.toolResults) {
		assert.deepEqual(Object.keys(result).sort(), [
			"diagnosticCodes",
			"errorClass",
			"interruptedMarker",
			"isError",
			"streamedOutputCommitted",
			"toolName",
		]);
	}
});
