// Regression (DPES-868 PR 145 review, finding 2): evidence and scratch directories must be
// separate. The check runs before any probe work or directory creation.
import { test, before, after } from "node:test";
import assert from "node:assert/strict";
import { existsSync, mkdirSync, mkdtempSync, rmSync, symlinkSync } from "node:fs";
import { tmpdir } from "node:os";
import { join } from "node:path";
import { runProbe } from "../lib/probe.mjs";

const REFUSAL = /output and scratch directories must be separate and must not contain one another/;
let tmp;

before(() => {
	tmp = mkdtempSync(join(tmpdir(), "dpes-868-overlap-"));
});

after(() => {
	rmSync(tmp, { recursive: true, force: true });
});

test("evidence and scratch cannot be the same directory", { timeout: 60_000 }, async () => {
	const dir = join(tmp, "same");
	mkdirSync(dir);
	await assert.rejects(runProbe({ outDir: dir, scratchDir: dir }), REFUSAL);
});

test("evidence inside scratch is refused before anything is created", { timeout: 60_000 }, async () => {
	const scratch = join(tmp, "scratch-holds-out");
	const out = join(scratch, "evidence");
	await assert.rejects(runProbe({ outDir: out, scratchDir: scratch }), REFUSAL);
	assert.equal(existsSync(out), false);
});

test("scratch inside evidence is refused before anything is created", { timeout: 60_000 }, async () => {
	const out = join(tmp, "out-holds-scratch");
	const scratch = join(out, "scratch");
	await assert.rejects(runProbe({ outDir: out, scratchDir: scratch }), REFUSAL);
	assert.equal(existsSync(out), false);
});

test("an evidence symlink that resolves into scratch is refused", { timeout: 60_000 }, async () => {
	const scratch = join(tmp, "scratch-target");
	mkdirSync(scratch);
	const alias = join(tmp, "evidence-alias");
	symlinkSync(scratch, alias, "dir");
	await assert.rejects(runProbe({ outDir: alias, scratchDir: scratch }), REFUSAL);
});
