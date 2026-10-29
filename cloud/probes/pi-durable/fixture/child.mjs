#!/usr/bin/env node
// One OS process of the pi-durable probe fixture (DPES-868). Not a production entrypoint.
//
// Usage: node fixture/child.mjs --scenario <name> --phase first|second --run-dir <dir>
//
// Opens the SQLite store retained in <run-dir>, installs the built-in CodingTools plus
// the scenario's fixture tools, runs one phase, appends evidence to <run-dir>/events.jsonl
// and external markers to <run-dir>/markers/, then closes the Harness and exits 0.
// A SIGKILL from the driver ends the process mid-phase; that is the crash under test.

import { appendFileSync, mkdirSync, openSync, closeSync, fsyncSync, writeFileSync, readFileSync, existsSync } from "node:fs";
import { join } from "node:path";
import { parseArgs } from "node:util";
import { BACKGROUND_CONTEXT as context } from "@earendil-works/chord/context";
import { createModels } from "@earendil-works/pi-ai/models";
import { fauxProvider } from "@earendil-works/pi-ai/providers/faux";
import { Harness, createRegistry, defineExtension } from "@earendil-works/pi-durable";
import { openNodeSqliteStorage } from "@earendil-works/pi-durable/storage/sqlite/node";
import { createCodingTools } from "@earendil-works/pi-durable/tools";
import { NodeExecutionEnv } from "@earendil-works/pi-durable/env/node";
import { SCENARIOS, hangForever } from "./scenarios.mjs";

const { values } = parseArgs({
	options: {
		scenario: { type: "string" },
		phase: { type: "string" },
		"run-dir": { type: "string" },
	},
});

const scenario = SCENARIOS[values.scenario];
const phase = values.phase;
const runDir = values["run-dir"];
if (!scenario || !["first", "second"].includes(phase) || !runDir) {
	process.stderr.write("usage: child.mjs --scenario <name> --phase first|second --run-dir <dir>\n");
	process.exit(2);
}

const workspace = join(runDir, "workspace");
const eventsFile = join(runDir, "events.jsonl");
const ledgerFile = join(runDir, "ledger.jsonl");
const markersDir = join(runDir, "markers");
mkdirSync(markersDir, { recursive: true });

// Each helper writes to a file outside this process before returning, so the driver
// observes progress even if this process is killed immediately afterwards.
function appendDurable(file, line) {
	const fd = openSync(file, "a");
	try {
		appendFileSync(fd, `${JSON.stringify(line)}\n`);
		fsyncSync(fd);
	} finally {
		closeSync(fd);
	}
}

function readEvents() {
	if (!existsSync(eventsFile)) return [];
	return readFileSync(eventsFile, "utf8")
		.split("\n")
		.filter(Boolean)
		.map((line) => JSON.parse(line));
}

const ctxBase = {
	phase,
	workspace,
	record(data) {
		appendDurable(eventsFile, { phase, event: data.event ?? "record", ...data });
	},
	marker(name) {
		writeFileSync(join(markersDir, `${phase}-${name}`), String(process.pid));
		appendDurable(eventsFile, { phase, event: "marker", marker: name, pid: process.pid });
	},
	ledger(entry) {
		appendDurable(ledgerFile, { ...entry, phase, pid: process.pid });
	},
	findEvent(eventPhase, event) {
		const found = readEvents().find((e) => e.phase === eventPhase && e.event === event);
		if (!found) throw new Error(`missing event ${eventPhase}/${event}`);
		return found;
	},
};

const faux = fauxProvider();
const models = createModels();
models.setProvider(faux.provider);
const model = faux.getModel();

const registry = createRegistry();
registry.install(createCodingTools());

const ctx = { ...ctxBase, faux };
const fixtureTools = scenario.tools(ctx);
if (fixtureTools.length > 0) {
	registry.install(defineExtension({ name: "fixture-effects", tools: fixtureTools }));
}

async function allEntries(root) {
	const out = [];
	let cursor;
	do {
		const page = await root.entries({ order: "ascending" }, 100, cursor, context);
		out.push(...page.items);
		cursor = page.next;
	} while (cursor !== undefined && cursor !== null);
	return out;
}

ctx.countEntries = async (role) => {
	const entries = await allEntries(ctx.root);
	return entries.filter((entry) => entry.model?.[0]?.role === role).length;
};

// publish only an allowlisted class observed in persisted runtime messages.
ctx.denialClassification = async () => {
	const entries = await allEntries(ctx.root);
	return entries.some((entry) => (entry.model ?? []).some((message) =>
		message.role === "assistant" && message.stopReason === "error" &&
		typeof message.errorMessage === "string" && /\bPERMISSION_DENIED\b/.test(message.errorMessage),
	)) ? "PERMISSION_DENIED" : null;
};

ctx.toolResults = async () => {
	const entries = await allEntries(ctx.root);
	return entries
		.filter((entry) => entry.model?.[0]?.role === "toolResult")
		.map((entry) => {
			const message = entry.model[0];
			const text = (message.content ?? []).map((part) => part.text ?? "").join("");
			return {
				toolName: message.toolName,
				isError: message.isError === true,
				diagnosticCodes: (entry.data?.diagnostics ?? []).map((d) => d.code ?? d.severity),
				streamedOutputCommitted: text.includes("streamed chunk"),
				interruptedMarker: text.includes("interrupted"),
				// Only the error class token is kept; the error text itself is never recorded.
				errorClass: text.match(/\bE[A-Z]{3,}\b/)?.[0] ?? null,
			};
		});
};

const harness = await Harness.open(
	await openNodeSqliteStorage(join(runDir, "store.sqlite")),
	{
		models,
		registry,
		env: ({ cwd }) => new NodeExecutionEnv({ cwd: cwd ?? workspace }),
	},
	context,
);
ctx.harness = harness;
ctx.root = await harness.root(context, { agent: { model: { provider: model.provider, modelId: model.id }, cwd: workspace } });

// Failures go to stderr (captured per run, scanned for sentinels) and to a
// message-free event so the driver can tell a failed phase from a killed one.
function failPhase(error) {
	process.stderr.write(`${error?.stack ?? String(error)}\n`);
	appendDurable(eventsFile, { phase, event: "failed", name: error?.name ?? "Error", code: error?.code ?? null });
	process.exit(1);
}
process.on("uncaughtException", failPhase);
process.on("unhandledRejection", failPhase);

// Test-only seam for the phase-deadline regression: PROBE_TEST_STALL names a phase that hangs
// once its harness is open. Only tests set it, through the driver's extraEnv. The self-exit
// bounds the process if a driver ever fails to enforce its deadline.
const STALL_SAFETY_EXIT_MS = 30_000;

try {
	if (process.env.PROBE_TEST_STALL === phase) {
		ctx.marker("stalled");
		setTimeout(() => process.exit(3), STALL_SAFETY_EXIT_MS);
		await hangForever();
	}
	await scenario[phase](ctx);
	await harness.close(context);
	appendDurable(eventsFile, { phase, event: "closed" });
} catch (error) {
	failPhase(error);
}
