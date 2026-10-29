// Parent test driver for the pi-durable probe (DPES-868).
//
// Runs one scenario as real OS processes: a first child that is SIGKILLed at a
// committed barrier marker, an optional driver-side mutation between processes, and a
// fresh second child against the same retained SQLite store. The driver never shares
// memory with the children; it reads only the files they wrote.

import { spawn } from "node:child_process";
import { existsSync, mkdirSync, openSync, closeSync, readFileSync, rmSync, writeFileSync } from "node:fs";
import { join, dirname } from "node:path";
import { fileURLToPath } from "node:url";

const PROBE_ROOT = join(dirname(fileURLToPath(import.meta.url)), "..");
const CHILD = join(PROBE_ROOT, "fixture", "child.mjs");
export const SCENARIO_NAMES = ["sigkill-input", "tool-safe", "tool-unsafe", "workspace-delete", "provider-denied"];

export async function loadScenarios() {
	const { SCENARIOS } = await import("../fixture/scenarios.mjs");
	return SCENARIOS;
}

// Fresh run directory: retained store, checkout with one tracked file, and a private HOME.
export function prepareRunDir(runDir) {
	rmSync(runDir, { recursive: true, force: true });
	mkdirSync(join(runDir, "workspace"), { recursive: true });
	mkdirSync(join(runDir, "home"), { recursive: true });
	mkdirSync(join(runDir, "logs"), { recursive: true });
	writeFileSync(join(runDir, "workspace", "hello.txt"), "fixture checkout line\n");
	return runDir;
}

function spawnPhase(runDir, scenario, phase, extraEnv) {
	const logFd = openSync(join(runDir, "logs", `${phase}-${scenario}.log`), "w");
	const child = spawn(
		process.execPath,
		[CHILD, "--scenario", scenario, "--phase", phase, "--run-dir", runDir],
		{
			// Minimal environment: nothing from the operator's shell reaches the fixture.
			env: { PATH: process.env.PATH, HOME: join(runDir, "home"), ...extraEnv },
			stdio: ["ignore", logFd, logFd],
		},
	);
	closeSync(logFd);
	const exited = new Promise((resolve, reject) => {
		child.once("error", reject);
		child.once("close", (code, signal) => resolve({ code, signal }));
	});
	return { child, exited };
}

// every phase has a deadline and owns cleanup of its child.
async function runPhase(runDir, scenario, phase, extraEnv, timeoutMs, operation) {
	const process = spawnPhase(runDir, scenario, phase, extraEnv);
	let finished = false;
	process.exited.then(() => { finished = true; }, () => { finished = true; });
	let timer;
	try {
		return await Promise.race([
			operation(process),
			new Promise((_, reject) => {
				timer = setTimeout(() => reject(new Error(`${scenario}/${phase} exceeded ${timeoutMs} ms`)), timeoutMs);
			}),
		]);
	} finally {
		clearTimeout(timer);
		if (!finished) process.child.kill("SIGKILL");
		await process.exited.catch(() => {});
	}
}

async function waitForFile(path, { timeoutMs, phaseExited }) {
	const deadline = Date.now() + timeoutMs;
	for (;;) {
		if (existsSync(path)) return;
		const early = await Promise.race([phaseExited, new Promise((r) => setTimeout(() => r(null), 20))]);
		if (early) throw new Error(`child exited before barrier ${path}: ${JSON.stringify(early)}`);
		if (Date.now() > deadline) throw new Error(`barrier not reached within ${timeoutMs} ms: ${path}`);
	}
}

export function readEvents(runDir) {
	const file = join(runDir, "events.jsonl");
	if (!existsSync(file)) return [];
	return readFileSync(file, "utf8").split("\n").filter(Boolean).map((line) => JSON.parse(line));
}

export function readLedger(runDir) {
	const file = join(runDir, "ledger.jsonl");
	if (!existsSync(file)) return [];
	return readFileSync(file, "utf8").split("\n").filter(Boolean).map((line) => JSON.parse(line));
}

// Runs one scenario end to end and returns observed facts. Facts are data only; the
// check mapping in lib/checks.mjs decides pass, fail, or blocked.
export async function runScenario(name, runDir, { extraEnv = {}, timeoutMs = 60_000 } = {}) {
	const scenarios = await loadScenarios();
	const scenario = scenarios[name];
	if (!scenario) throw new Error(`unknown scenario ${name}`);
	prepareRunDir(runDir);

	const facts = { scenario: name, runDir, firstExit: null, secondExit: null, killedAtBarrier: null };
	facts.firstExit = await runPhase(runDir, name, "first", extraEnv, timeoutMs, async (first) => {
		if (scenario.barrier) {
			const marker = join(runDir, "markers", `first-${scenario.barrier}`);
			await waitForFile(marker, { timeoutMs, phaseExited: first.exited });
			first.child.kill("SIGKILL");
			facts.killedAtBarrier = scenario.barrier;
		}
		return first.exited;
	});

	if (scenario.deleteWorkspaceBetween) {
		rmSync(join(runDir, "workspace"), { recursive: true, force: true });
		facts.workspaceDeletedBetweenProcesses = !existsSync(join(runDir, "workspace"));
	}

	if (scenario.barrier) {
		facts.secondExit = await runPhase(runDir, name, "second", extraEnv, timeoutMs, (second) => second.exited);
	}

	facts.events = readEvents(runDir);
	facts.ledger = readLedger(runDir);
	return facts;
}
