#!/usr/bin/env node
// Runs the pi-durable compatibility and crash-recovery probe and writes evidence (DPES-868).
//
// Usage: node run-probe.mjs --out <evidence-dir> [--scratch <run-dir>]
//
// --out      receives report.json, report.md, and scenarios/<name>/{events,ledger}.jsonl
// --scratch  receives per-scenario SQLite stores, workspaces, and logs. Those hold
//            transcripts, so keep it outside the evidence directory. Defaults to a fresh
//            temporary directory.
//
// Exit 0 when no check fails (blocked checks are reported, not failed). Exit 1 on any
// failing check. Exit 2 on usage or infrastructure errors, with no evidence written.

import { mkdtempSync } from "node:fs";
import { tmpdir } from "node:os";
import { join, resolve } from "node:path";
import { parseArgs } from "node:util";
import { runProbe } from "./lib/probe.mjs";

const { values } = parseArgs({
	options: {
		out: { type: "string" },
		scratch: { type: "string" },
	},
});

if (!values.out) {
	process.stderr.write("usage: node run-probe.mjs --out <evidence-dir> [--scratch <run-dir>]\n");
	process.exit(2);
}

const outDir = resolve(values.out);
const scratchDir = values.scratch ? resolve(values.scratch) : mkdtempSync(join(tmpdir(), "dpes-868-probe-runs-"));

try {
	const record = await runProbe({ outDir, scratchDir });
	const { pass, fail, blocked } = record.summary;
	process.stdout.write(`pass ${pass}, fail ${fail}, blocked ${blocked}; pilotReady ${record.pilotReady}\n`);
	process.stdout.write(`evidence: ${join(outDir, "report.md")}\n`);
	process.exit(fail > 0 ? 1 : 0);
} catch (error) {
	process.stderr.write(`probe error: ${error.message}\n`);
	process.exit(2);
}
