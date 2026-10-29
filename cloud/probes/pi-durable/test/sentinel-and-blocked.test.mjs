// T6 / AC6: injected provider denial ends as a named failure with no fallback, the sentinel
// scanner catches what it should, and the live path stays blocked without cloud calls
// (DPES-868 section 8).
import { test, before, after } from "node:test";
import assert from "node:assert/strict";
import { mkdtempSync, readdirSync, readFileSync, rmSync } from "node:fs";
import { tmpdir } from "node:os";
import { join } from "node:path";
import { runScenario } from "../lib/driver.mjs";
import { liveGate, LIVE_CHECK_IDS } from "../lib/gate.mjs";
import { liveChecks } from "../lib/checks.mjs";
import { scanForSentinels } from "../lib/sentinels.mjs";

let tmp;
let facts;

before(async () => {
	tmp = mkdtempSync(join(tmpdir(), "dpes-868-t6-"));
	facts = await runScenario("provider-denied", join(tmp, "provider-denied"));
});

after(() => {
	rmSync(tmp, { recursive: true, force: true });
});

test("injected denial settles unanswered after exactly one model request, with no fallback", () => {
	const settled = facts.events.find((e) => e.event === "settled");
	assert.equal(settled.settledStatus, "unanswered");
	assert.equal(settled.modelRequests, 1);
	assert.equal(facts.secondExit, null);
});

test("the injected-denial process still exits cleanly, so the failure is in the record, not in a crash", () => {
	assert.deepEqual(facts.firstExit, { code: 0, signal: null });
});

test("sentinel scan is clean over the real run's events and logs", () => {
	const texts = [];
	for (const file of ["events.jsonl", "ledger.jsonl"]) {
		try {
			texts.push(readFileSync(join(facts.runDir, file), "utf8"));
		} catch {
			// a missing ledger is fine for this scenario
		}
	}
	for (const file of readdirSync(join(facts.runDir, "logs"))) {
		texts.push(readFileSync(join(facts.runDir, "logs", file), "utf8"));
	}
	assert.deepEqual(texts.flatMap((t) => scanForSentinels(t)), []);
});

test("negative control: planted credential and signed-URL shapes are caught", () => {
	assert.ok(scanForSentinels("token ya29.a0AfH6SMBabcdefghijkl").includes("google-oauth-token"));
	assert.ok(scanForSentinels("https://storage.example/o?X-Goog-Signature=abcdef12").includes("signed-url-goog"));
	assert.ok(scanForSentinels("key -----BEGIN RSA PRIVATE KEY-----").includes("private-key-block"));
	assert.ok(scanForSentinels("Authorization: Bearer abcdefghijklmnopqrstuv").includes("bearer-token"));
});

test("negative control: a fixture prompt or tool output string is caught even with extra spacing", () => {
	assert.ok(scanForSentinels("sent  probe prompt here").includes("fixture-literal-0"));
	assert.ok(scanForSentinels("ledger_safe streamed chunk").includes("fixture-literal-1"));
});

test("an ordinary report sentence is not a sentinel", () => {
	assert.deepEqual(scanForSentinels("pass 10, fail 1, blocked 2; pilotReady false"), []);
});

test("live gate is never ready here and names every missing prerequisite on this host", () => {
	const gate = liveGate({ platform: "darwin", env: {} });
	assert.equal(gate.ready, false);
	assert.ok(gate.missing.some((m) => m.includes('platform "darwin"')));
	assert.ok(gate.missing.some((m) => m.includes("PROBE_IMAGE_DIGEST")));
	assert.ok(gate.missing.some((m) => m.includes("PROBE_VERTEX_MODEL")));
	assert.ok(gate.missing.some((m) => m.includes("PROBE_VERTEX_LOCATION")));
	assert.ok(gate.missing.some((m) => m.includes("DPES-804")));
});

test("live gate stays not ready even when every operator input is supplied", () => {
	const gate = liveGate({
		platform: "linux",
		env: {
			PATH: "/usr/bin",
			PROBE_IMAGE_DIGEST: "sha256:0000",
			PROBE_VERTEX_MODEL: "example-model",
			PROBE_VERTEX_LOCATION: "us-central1",
		},
	});
	assert.equal(gate.ready, false);
	assert.ok(gate.missing.some((m) => m.includes("runner is not implemented")));
});

test("both live checks are blocked with prerequisite_missing and a prerequisite, never pass or fail", () => {
	const checks = liveChecks(liveGate({ platform: "darwin", env: {} }));
	assert.deepEqual(
		checks.map((c) => c.id),
		LIVE_CHECK_IDS,
	);
	for (const c of checks) {
		assert.equal(c.status, "blocked");
		assert.equal(c.errorCode, "prerequisite_missing");
		assert.ok(c.prerequisite.length > 0);
	}
});
