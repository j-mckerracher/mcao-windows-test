// Orchestrates one probe run: every scenario as real processes, then checks, then evidence.
//
// Refuses to publish evidence when a sentinel pattern appears in the report or in any
// file copied into it. Run logs are scanned too, but they stay out of the evidence directory.

import { existsSync, mkdirSync, lstatSync, readlinkSync, realpathSync, readdirSync, readFileSync, rmSync, writeFileSync } from "node:fs";
import { join, dirname, resolve, relative, isAbsolute, sep } from "node:path";
import { fileURLToPath } from "node:url";
import { fauxProvider } from "@earendil-works/pi-ai/providers/faux";
import { SCENARIO_NAMES, runScenario } from "./driver.mjs";
import { liveChecks, localChecks, versionCheck } from "./checks.mjs";
import { liveGate } from "./gate.mjs";
import { NODE_MINIMUM, PINS, buildRecord, readPackageVersions, renderMarkdown, validateEvidence, versionAtLeast } from "./evidence.mjs";
import { scanForSentinels } from "./sentinels.mjs";

export const PROBE_ROOT = join(dirname(fileURLToPath(import.meta.url)), "..");

// Runs every scenario and returns the facts keyed by scenario name.
export async function collectFacts(scratchDir) {
	const facts = {};
	for (const name of SCENARIO_NAMES) {
		facts[name] = await runScenario(name, join(scratchDir, name));
	}
	return facts;
}

function mapFacts(facts) {
	return {
		sigkill: facts["sigkill-input"],
		safe: facts["tool-safe"],
		unsafe: facts["tool-unsafe"],
		workspace: facts["workspace-delete"],
		denied: facts["provider-denied"],
	};
}

function readLogTexts(scratchDir) {
	const texts = [];
	for (const name of SCENARIO_NAMES) {
		const logDir = join(scratchDir, name, "logs");
		if (!existsSync(logDir)) continue;
		for (const file of readdirSync(logDir)) {
			texts.push(readFileSync(join(logDir, file), "utf8"));
		}
	}
	return texts;
}

// resolve existing ancestors before creating either directory.
function canonicalPath(path) {
	const absolute = resolve(path);
	try {
		return realpathSync(absolute);
	} catch (error) {
		if (error.code !== "ENOENT") throw error;
		try {
			if (lstatSync(absolute).isSymbolicLink()) {
				return canonicalPath(resolve(dirname(absolute), readlinkSync(absolute)));
			}
		} catch (linkError) {
			if (linkError.code !== "ENOENT") throw linkError;
		}
		const parent = dirname(absolute);
		if (parent === absolute) throw error;
		return join(canonicalPath(parent), absolute.slice(parent.length));
	}
}

function requireSeparateDirectories(outDir, scratchDir) {
	const out = canonicalPath(outDir);
	const scratch = canonicalPath(scratchDir);
	const contains = (parent, child) => {
		const path = relative(parent, child);
		return path === "" || (!isAbsolute(path) && path !== ".." && !path.startsWith(`..${sep}`));
	};
	if (contains(out, scratch) || contains(scratch, out)) {
		throw new Error("output and scratch directories must be separate and must not contain one another");
	}
}

// retain the exact candidate bytes until every scan has passed.
function scenarioFiles(scratchDir) {
	const files = [];
	for (const name of SCENARIO_NAMES) {
		for (const file of ["events.jsonl", "ledger.jsonl"]) {
			const source = join(scratchDir, name, file);
			if (existsSync(source)) files.push({ name, file, bytes: readFileSync(source) });
		}
	}
	return files;
}

// Returns { record, reportJson, reportMarkdown }. Throws, writing nothing, if sentinels appear.
export async function buildEvidence({ facts, scratchDir, outDir }) {
	const nodeVersion = process.versions.node;
	const modelId = fauxProvider().getModel().id;
	const versions = {
		nodeSupported: versionAtLeast(nodeVersion, NODE_MINIMUM),
		nodeVersion,
		packages: readPackageVersions(PROBE_ROOT),
	};
	const checks = [versionCheck(versions, PINS), ...liveChecks(liveGate()), ...localChecks(mapFacts(facts))];
	const base = { checks, packageVersions: versions.packages, nodeVersion, platform: process.platform, modelId };

	requireSeparateDirectories(outDir, scratchDir);
	const candidates = scenarioFiles(scratchDir);
	const draft = buildRecord(base);
	const draftJson = `${JSON.stringify(draft, null, 2)}\n`;
	const draftMd = renderMarkdown(draft);
	const logTexts = readLogTexts(scratchDir);

	const hits = [
		...[draftJson, draftMd, ...candidates.map(({ bytes }) => bytes.toString("utf8"))].flatMap((text) => scanForSentinels(text).map((name) => `evidence:${name}`)),
		...logTexts.flatMap((text) => scanForSentinels(text).map((name) => `log:${name}`)),
	];
	if (hits.length > 0) {
		throw new Error(`refused to publish evidence; sentinel patterns matched: ${[...new Set(hits)].join(", ")}`);
	}

	const sentinelCheck = {
		id: "ac6-sentinel-scan",
		status: "pass",
		evidenceReference: "report.json#/checks",
		errorCode: null,
		summary: "no credential, signed-URL, prompt, or transcript sentinel in the report, copied scenario files, or run logs",
		prerequisite: null,
	};
	const record = buildRecord({ ...base, checks: [...checks, sentinelCheck] });
	const reportJson = `${JSON.stringify(record, null, 2)}\n`;
	const reportMarkdown = renderMarkdown(record);
	const finalHits = [...scanForSentinels(reportJson), ...scanForSentinels(reportMarkdown)];
	if (finalHits.length > 0) {
		throw new Error(`refused to publish report; sentinel patterns matched: ${[...new Set(finalHits)].join(", ")}`);
	}
	const errors = validateEvidence(record);
	if (errors.length > 0) {
		throw new Error(`evidence failed validation: ${errors.join("; ")}`);
	}
	mkdirSync(outDir, { recursive: true });
	rmSync(join(outDir, "scenarios"), { recursive: true, force: true });
	for (const { name, file, bytes } of candidates) {
		const target = join(outDir, "scenarios", name);
		mkdirSync(target, { recursive: true });
		writeFileSync(join(target, file), bytes);
	}
	return { record, reportJson, reportMarkdown };
}

// Full run: facts, evidence, and files written under outDir. Returns the record.
export async function runProbe({ outDir, scratchDir }) {
	requireSeparateDirectories(outDir, scratchDir);
	mkdirSync(scratchDir, { recursive: true });
	const facts = await collectFacts(scratchDir);
	const { record, reportJson, reportMarkdown } = await buildEvidence({ facts, scratchDir, outDir });
	writeFileSync(join(outDir, "report.json"), reportJson);
	writeFileSync(join(outDir, "report.md"), reportMarkdown);
	return record;
}
