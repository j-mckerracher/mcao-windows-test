// T1: evidence record shape, pins, and rendering (DPES-868 section 8).
import { test, before, after } from "node:test";
import assert from "node:assert/strict";
import { mkdtempSync, rmSync } from "node:fs";
import { tmpdir } from "node:os";
import { join } from "node:path";
import { buildEvidence, collectFacts } from "../lib/probe.mjs";
import { REQUIRED_CHECK_IDS } from "../lib/checks.mjs";
import { PINS, renderMarkdown, validateEvidence } from "../lib/evidence.mjs";

let tmp;
let result;

before(async () => {
	tmp = mkdtempSync(join(tmpdir(), "dpes-868-t1-"));
	const scratchDir = join(tmp, "scratch");
	const outDir = join(tmp, "out");
	const facts = await collectFacts(scratchDir);
	result = await buildEvidence({ facts, scratchDir, outDir });
});

after(() => {
	rmSync(tmp, { recursive: true, force: true });
});

test("record validates and lists every required check in order", () => {
	assert.deepEqual(validateEvidence(result.record), []);
	assert.deepEqual(
		result.record.checks.map((c) => c.id),
		REQUIRED_CHECK_IDS,
	);
});

test("pilot readiness is false and the probe is not production durability evidence", () => {
	assert.equal(result.record.pilotReady, false);
	assert.equal(result.record.storage.kind, "sqlite-fixture");
	assert.match(result.record.storage.note, /not production durability evidence/);
});

test("the four pins are recorded with lock and installed versions", () => {
	for (const [name, pin] of Object.entries(PINS)) {
		assert.equal(result.record.packageVersions.packages[name].version, pin);
		assert.equal(result.record.packageVersions.packages[name].installed, pin);
		assert.match(result.record.packageVersions.packages[name].integrity, /^sha512-/);
	}
	assert.equal(result.record.packageVersions.pins["@earendil-works/pi-durable"], "1.3.0");
});

test("image digest and live provider are blocked, not invented", () => {
	assert.equal(result.record.imageDigest.value, null);
	assert.equal(result.record.imageDigest.status, "blocked");
	assert.equal(result.record.model.providerLocation, "local-in-process-faux");
	assert.equal(result.record.sandbox.kind, "none");
});

test("summary counts equal the check statuses", () => {
	const statuses = result.record.checks.map((c) => c.status);
	assert.deepEqual(result.record.summary, {
		pass: statuses.filter((s) => s === "pass").length,
		fail: statuses.filter((s) => s === "fail").length,
		blocked: statuses.filter((s) => s === "blocked").length,
	});
	assert.equal(result.record.summary.blocked, 2);
});

test("validator rejects a record missing a required check", () => {
	const broken = structuredClone(result.record);
	broken.checks = broken.checks.filter((c) => c.id !== "ac3-sigkill-root-input-identity");
	const errors = validateEvidence(broken);
	assert.ok(errors.includes("missing required check ac3-sigkill-root-input-identity"));
});

test("validator rejects pilotReady true and a blocked check without a prerequisite", () => {
	const broken = structuredClone(result.record);
	broken.pilotReady = true;
	const blocked = broken.checks.find((c) => c.status === "blocked");
	blocked.prerequisite = null;
	const errors = validateEvidence(broken);
	assert.ok(errors.includes("pilotReady must be false"));
	assert.ok(errors.includes(`blocked check ${blocked.id} needs prerequisite`));
});

test("validator rejects duplicate check ids and a summary that does not match", () => {
	const broken = structuredClone(result.record);
	broken.checks.push({ ...broken.checks[0] });
	const errors = validateEvidence(broken);
	assert.ok(errors.includes("check ids must be unique"));
	assert.ok(errors.includes("summary does not match check statuses"));
});

test("markdown lists every check id and states pilot readiness", () => {
	const md = renderMarkdown(result.record);
	for (const id of REQUIRED_CHECK_IDS) assert.ok(md.includes(`| ${id} |`), `missing row ${id}`);
	assert.ok(md.includes("Pilot ready: **no**"));
	assert.ok(md.includes("## Unresolved decisions"));
});
