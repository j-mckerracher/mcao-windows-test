// Regressions (DPES-868 PR 145 review, findings 3 and 4). These feed hand-built facts to the
// check mapping, so each case isolates one rule in lib/checks.mjs.
//   AC5 (finding 3): coding-work recovery passes only when the checkout is present AND the
//       recovery read succeeds. An empty checkout must not pass.
//   AC6 (finding 4): the injected denial passes only when it is classified PERMISSION_DENIED.
import { test } from "node:test";
import assert from "node:assert/strict";
import { localChecks } from "../lib/checks.mjs";

const OK_READ = { toolName: "read", isError: false, diagnosticCodes: [], streamedOutputCommitted: false, interruptedMarker: false, errorClass: null };
const FAILED_READ = { ...OK_READ, isError: true, errorClass: "ENOENT" };
const EXITED_CLEANLY = { code: 0, signal: null };

function checksFor({ recovered = [], settled = [] } = {}) {
	return localChecks({
		sigkill: { events: [], ledger: [], firstExit: EXITED_CLEANLY, secondExit: null, killedAtBarrier: null },
		safe: { events: [], ledger: [], firstExit: null, secondExit: null },
		unsafe: { events: [], ledger: [], firstExit: null, secondExit: null },
		workspace: { events: recovered, ledger: [], firstExit: EXITED_CLEANLY, secondExit: EXITED_CLEANLY },
		denied: { events: settled, ledger: [], firstExit: EXITED_CLEANLY, secondExit: null },
	});
}

const find = (checks, id) => checks.find((c) => c.id === id);
const recovered = (fields) => ({ phase: "second", event: "recovered", sameRoot: true, settledStatus: "done", ...fields });
const settled = (fields) => ({ phase: "first", event: "settled", settledStatus: "unanswered", modelRequests: 1, denialClassification: "PERMISSION_DENIED", ...fields });

test("AC5: an empty checkout whose recovery read fails is not a recovered coding task", () => {
	const check = find(checksFor({ recovered: [recovered({ workspaceExists: true, toolResults: [OK_READ, FAILED_READ] })] }), "ac5-coding-work-recovery");
	assert.equal(check.status, "fail");
	assert.equal(check.errorCode, "unexpected_workspace_state");
});

test("AC5: a present checkout with no recovery read is not a recovered coding task", () => {
	const check = find(checksFor({ recovered: [recovered({ workspaceExists: true, toolResults: [OK_READ] })] }), "ac5-coding-work-recovery");
	assert.equal(check.status, "fail");
});

test("AC5: a present checkout whose recovery read succeeds passes", () => {
	const check = find(checksFor({ recovered: [recovered({ workspaceExists: true, toolResults: [OK_READ, OK_READ] })] }), "ac5-coding-work-recovery");
	assert.equal(check.status, "pass");
});

test("AC6: an unclassified provider failure is not a named denial", () => {
	const check = find(checksFor({ settled: [settled({ denialClassification: null })] }), "ac6-injected-provider-denial-named-no-fallback");
	assert.equal(check.status, "fail");
	assert.equal(check.errorCode, "denial_not_named");
});

test("AC6: a PERMISSION_DENIED classification with one model request passes", () => {
	const check = find(checksFor({ settled: [settled()] }), "ac6-injected-provider-denial-named-no-fallback");
	assert.equal(check.status, "pass");
});

test("AC6: a second model request is flagged as a fallback even when classified", () => {
	const check = find(checksFor({ settled: [settled({ modelRequests: 2 })] }), "ac6-injected-provider-denial-named-no-fallback");
	assert.equal(check.status, "fail");
	assert.equal(check.errorCode, "fallback_suspected");
});
