// Regression (DPES-868 PR 145 review, finding 1): scenario files are copied into the evidence
// directory only after every sentinel scan passes, so a refused publish leaves nothing shareable.
import { test, before, after } from "node:test";
import assert from "node:assert/strict";
import { appendFileSync, existsSync, mkdtempSync, rmSync } from "node:fs";
import { tmpdir } from "node:os";
import { join } from "node:path";
import { buildEvidence, collectFacts } from "../lib/probe.mjs";

let tmp;
let outDir;
let refusal;

before(async () => {
	tmp = mkdtempSync(join(tmpdir(), "dpes-868-scan-before-copy-"));
	const scratchDir = join(tmp, "scratch");
	outDir = join(tmp, "out");
	const facts = await collectFacts(scratchDir);
	// A credential-shaped token lands in a scenario event file. The publish must be refused.
	const planted = { phase: "first", event: "note", token: "ya29.a0AfH6SMBplantedtokenvalue" };
	appendFileSync(join(scratchDir, "tool-safe", "events.jsonl"), `${JSON.stringify(planted)}\n`);
	refusal = await buildEvidence({ facts, scratchDir, outDir }).then(
		() => null,
		(error) => error,
	);
});

after(() => {
	rmSync(tmp, { recursive: true, force: true });
});

test("a sentinel in a copied scenario file refuses the publish", () => {
	assert.ok(refusal, "buildEvidence must reject when a sentinel is present");
	assert.match(refusal.message, /refused to publish evidence; sentinel patterns matched: evidence:google-oauth-token/);
});

test("a refused publish writes nothing under the evidence directory, not even the scanned bytes", () => {
	assert.equal(existsSync(join(outDir, "scenarios")), false);
	assert.equal(existsSync(outDir), false);
});
