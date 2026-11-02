// T3 / AC3: SIGKILL after the input commit, then resume in a fresh process (DPES-868 section 8).
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
	tmp = mkdtempSync(join(tmpdir(), "dpes-868-t3-"));
	facts = await runScenario("sigkill-input", join(tmp, "sigkill-input"));
	recovered = facts.events.find((e) => e.phase === "second" && e.event === "recovered");
});

after(() => {
	rmSync(tmp, { recursive: true, force: true });
});

test("the first process was killed with SIGKILL at the generation barrier", () => {
	assert.equal(facts.firstExit.signal, "SIGKILL");
	assert.equal(facts.killedAtBarrier, "generation-started");
});

test("the second process exits cleanly after resuming", () => {
	assert.deepEqual(facts.secondExit, { code: 0, signal: null });
});

test("resume reopens the same root and the same submission, which settles done", () => {
	assert.equal(recovered.sameRoot, true);
	assert.equal(recovered.sameSubmission, true);
	assert.equal(recovered.settledStatus, "done");
});

test("a duplicate requestId returns the existing submission and adds no user entry", () => {
	assert.equal(recovered.duplicateIsSameSubmission, true);
	assert.equal(recovered.userEntries, 1);
});

test("exactly one assistant answer is recorded after recovery", () => {
	assert.equal(recovered.assistantEntries, 1);
});
