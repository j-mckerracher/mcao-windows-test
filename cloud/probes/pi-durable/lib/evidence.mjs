// Evidence record for the pi-durable probe (DPES-868 Jira section 5.6).
//
// Builds the JSON record and the human-readable markdown from checks, validates the
// record shape (T1), and never carries prompts, tool output text, or credentials.

import { readFileSync, existsSync } from "node:fs";
import { join } from "node:path";
import { CHECK_STATUSES, REQUIRED_CHECK_IDS } from "./checks.mjs";

export const SCHEMA_VERSION = 1;
export const TASK = "DPES-868";
export const NODE_MINIMUM = "22.19.0";

// The four pins from DPES-868 section 5.1. Exact versions, no ranges.
export const PINS = {
	"@earendil-works/pi-durable": "1.3.0",
	"@earendil-works/pi-ai": "1.3.0",
	"@earendil-works/chord": "1.3.0",
	"@earendil-works/pi-codemode": "1.3.0",
};

export const LOCAL_PROVIDER_LOCATION = "local-in-process-faux";

// Open questions that the local probe cannot answer. Each maps to an owner, not to a guess.
export const UNRESOLVED_DECISIONS = [
	{
		id: "D1",
		question: "Which runtime and provider combination is approved for the live probe: Vertex model under bubblewrap on a Linux worker",
		requiredOwnerRole: "operator; DPES-804",
	},
	{
		id: "D2",
		question: "Storage atomicity and acknowledgement semantics for production; the SQLite WAL fixture survives process crash but not power loss",
		requiredOwnerRole: "DPES-869",
	},
	{
		id: "D3",
		question: "Separate workspace recovery: who restores a deleted checkout and how coding work resumes",
		requiredOwnerRole: "DPES-869",
	},
	{
		id: "D4",
		question: "Replay policy for replay-safe and default tools, and whether an interrupted non-replay-safe call is reported as interrupted, needs owner decision",
		requiredOwnerRole: "operator",
	},
	{
		id: "D5",
		question: "Cross-process ownership of one storage; the library provides no cross-process locking",
		requiredOwnerRole: "DPES-870",
	},
	{
		id: "D6",
		question: "Continuation after platform kill, 2-hour deadline, zero Job retries, budget, and cancellation",
		requiredOwnerRole: "operator; DPES-870",
	},
	{
		id: "D7",
		question: "Periodic one-minute checkpoint cadence was not exercised; the probe uses barrier markers instead",
		requiredOwnerRole: "operator",
	},
	{
		id: "D8",
		question: "Live Vertex and bubblewrap runs need the approved Cloud Run image digest and pinned Vertex model",
		requiredOwnerRole: "DPES-811, DPES-813",
	},
];

// Reads the lockfile entries for the pins. `installed` is the version under node_modules,
// which is what the probe actually loads.
export function readPackageVersions(probeRoot) {
	const lock = JSON.parse(readFileSync(join(probeRoot, "package-lock.json"), "utf8"));
	const packages = {};
	for (const name of Object.keys(PINS)) {
		const entry = lock.packages?.[`node_modules/${name}`];
		const installedManifest = join(probeRoot, "node_modules", name, "package.json");
		packages[name] = {
			version: entry?.version ?? null,
			integrity: entry?.integrity ?? null,
			installed: existsSync(installedManifest) ? JSON.parse(readFileSync(installedManifest, "utf8")).version : null,
		};
	}
	return packages;
}

// Compares dotted numeric versions. Returns true when `version` >= `minimum`.
export function versionAtLeast(version, minimum) {
	const a = version.split(".").map(Number);
	const b = minimum.split(".").map(Number);
	for (let i = 0; i < 3; i += 1) {
		if ((a[i] ?? 0) !== (b[i] ?? 0)) return (a[i] ?? 0) > (b[i] ?? 0);
	}
	return true;
}

export function buildRecord({ checks, packageVersions, nodeVersion, platform, modelId }) {
	const summary = { pass: 0, fail: 0, blocked: 0 };
	for (const c of checks) summary[c.status] += 1;
	return {
		schemaVersion: SCHEMA_VERSION,
		task: TASK,
		scope: "pi-durable 1.3.0 compatibility and crash-recovery probe; local fixture evidence only",
		pilotReady: false,
		runtime: { nodeVersion, nodeMinimum: NODE_MINIMUM, platform },
		packageVersions: {
			nodeVersion,
			nodeSupported: versionAtLeast(nodeVersion, NODE_MINIMUM),
			pins: PINS,
			packages: packageVersions,
		},
		model: { id: modelId, providerLocation: LOCAL_PROVIDER_LOCATION },
		sandbox: {
			kind: "none",
			note: "local runs are in-process and unsandboxed; no bubblewrap run exists in this change",
		},
		imageDigest: { value: null, status: "blocked", prerequisite: "DPES-811 approved Cloud Run image digest" },
		storage: {
			kind: "sqlite-fixture",
			note: "WAL with synchronous NORMAL: survives process crash, not power loss; not production durability evidence",
		},
		checks,
		unresolvedDecisions: UNRESOLVED_DECISIONS,
		summary,
	};
}

// Returns an array of human-readable problems. Empty array means the record is valid.
export function validateEvidence(record) {
	const errors = [];
	const need = (cond, msg) => {
		if (!cond) errors.push(msg);
	};
	need(record.schemaVersion === SCHEMA_VERSION, `schemaVersion must be ${SCHEMA_VERSION}`);
	need(record.task === TASK, `task must be ${TASK}`);
	need(record.pilotReady === false, "pilotReady must be false");
	need(typeof record.runtime?.nodeVersion === "string", "runtime.nodeVersion must be a string");
	need(typeof record.runtime?.platform === "string", "runtime.platform must be a string");
	need(typeof record.model?.id === "string", "model.id must be a string");
	need(typeof record.model?.providerLocation === "string", "model.providerLocation must be a string");

	const pv = record.packageVersions;
	need(pv && typeof pv.nodeSupported === "boolean", "packageVersions.nodeSupported must be a boolean");
	for (const name of Object.keys(PINS)) {
		const p = pv?.packages?.[name];
		need(p && "version" in p && "integrity" in p && "installed" in p, `packageVersions.packages missing fields for ${name}`);
		need(pv?.pins?.[name] === PINS[name], `packageVersions.pins mismatch for ${name}`);
	}

	need(Array.isArray(record.checks), "checks must be an array");
	const checks = Array.isArray(record.checks) ? record.checks : [];
	const ids = checks.map((c) => c.id);
	need(new Set(ids).size === ids.length, "check ids must be unique");
	for (const id of REQUIRED_CHECK_IDS) {
		need(ids.includes(id), `missing required check ${id}`);
	}
	for (const c of checks) {
		need(CHECK_STATUSES.includes(c.status), `check ${c.id} has invalid status ${c.status}`);
		need(typeof c.evidenceReference === "string" && c.evidenceReference.length > 0, `check ${c.id} needs evidenceReference`);
		need(typeof c.summary === "string" && c.summary.length > 0, `check ${c.id} needs summary`);
		if (c.status === "pass") need(c.errorCode === null, `pass check ${c.id} must have null errorCode`);
		if (c.status === "fail") need(typeof c.errorCode === "string", `fail check ${c.id} needs errorCode`);
		if (c.status === "blocked") {
			need(typeof c.errorCode === "string", `blocked check ${c.id} needs errorCode`);
			need(typeof c.prerequisite === "string" && c.prerequisite.length > 0, `blocked check ${c.id} needs prerequisite`);
		}
	}

	const counted = { pass: 0, fail: 0, blocked: 0 };
	for (const c of checks) if (counted[c.status] !== undefined) counted[c.status] += 1;
	need(JSON.stringify(record.summary) === JSON.stringify(counted), "summary does not match check statuses");

	need(Array.isArray(record.unresolvedDecisions) && record.unresolvedDecisions.length > 0, "unresolvedDecisions must be a non-empty array");
	for (const d of record.unresolvedDecisions ?? []) {
		need(d.id && d.question && d.requiredOwnerRole, `unresolved decision ${d.id ?? "?"} needs id, question, requiredOwnerRole`);
	}
	return errors;
}

const cell = (text) => String(text ?? "").replace(/\|/g, "\\|").replace(/\n/g, " ");

// Markdown for a reader. Carries only the check summaries, prerequisites, and pins.
export function renderMarkdown(record) {
	const lines = [];
	lines.push(`# ${record.task} pi-durable probe evidence`);
	lines.push("");
	lines.push(`Pilot ready: **${record.pilotReady ? "yes" : "no"}**. This is local fixture evidence for pi-durable ${PINS["@earendil-works/pi-durable"]}, not production durability evidence.`);
	lines.push("");
	lines.push(`Checks: ${record.summary.pass} pass, ${record.summary.fail} fail, ${record.summary.blocked} blocked.`);
	lines.push("");
	lines.push("## Checks");
	lines.push("");
	lines.push("| Check | Status | Error code | Evidence | Summary |");
	lines.push("| --- | --- | --- | --- | --- |");
	for (const c of record.checks) {
		lines.push(`| ${cell(c.id)} | ${cell(c.status)} | ${cell(c.errorCode ?? "-")} | ${cell(c.evidenceReference)} | ${cell(c.summary)} |`);
	}
	const blocked = record.checks.filter((c) => c.status === "blocked");
	if (blocked.length > 0) {
		lines.push("");
		lines.push("## Blocked prerequisites");
		lines.push("");
		for (const c of blocked) lines.push(`- ${c.id}: ${c.prerequisite}`);
	}
	lines.push("");
	lines.push("## Unresolved decisions");
	lines.push("");
	lines.push("| ID | Question | Owner |");
	lines.push("| --- | --- | --- |");
	for (const d of record.unresolvedDecisions) {
		lines.push(`| ${cell(d.id)} | ${cell(d.question)} | ${cell(d.requiredOwnerRole)} |`);
	}
	lines.push("");
	lines.push("## Environment");
	lines.push("");
	lines.push(`- Node ${record.runtime.nodeVersion} on ${record.runtime.platform} (minimum ${record.runtime.nodeMinimum})`);
	for (const [name, pin] of Object.entries(record.packageVersions.pins)) {
		const p = record.packageVersions.packages[name];
		lines.push(`- ${name} pinned ${pin}, lock ${p.version ?? "missing"}, installed ${p.installed ?? "missing"}`);
	}
	lines.push(`- Model: ${record.model.id} (${record.model.providerLocation})`);
	lines.push(`- Sandbox: ${record.sandbox.kind}. ${record.sandbox.note}`);
	lines.push(`- Image digest: blocked, needs ${record.imageDigest.prerequisite}`);
	lines.push(`- Storage: ${record.storage.kind}. ${record.storage.note}`);
	lines.push("");
	return lines.join("\n");
}
