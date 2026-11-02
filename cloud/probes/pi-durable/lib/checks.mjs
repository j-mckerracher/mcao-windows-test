// Maps observed run facts to named checks (DPES-868 AC1-AC6).
//
// Each check has exactly one status: pass, fail, or blocked. Facts come from the driver
// (lib/driver.mjs) and the live gate (lib/gate.mjs). Nothing here reads prompts, tool
// output text, or credentials; facts carry identifiers, counts, and error classes only.

export const CHECK_STATUSES = ["pass", "fail", "blocked"];

// Every check the probe must always report, in report order. T1 asserts this set.
export const REQUIRED_CHECK_IDS = [
	"ac1-versions-recorded",
	"ac2-live-vertex-tool-result",
	"ac2-live-outside-write-denied",
	"ac3-sigkill-root-input-identity",
	"ac3-duplicate-request-id-dedup",
	"ac4-safe-tool-rerun-count-2",
	"ac4-unsafe-tool-not-rerun-count-1",
	"ac4-no-fresh-tool-after-recovery",
	"ac4-streamed-output-committed",
	"ac5-transcript-survives-checkout-deleted",
	"ac5-coding-work-recovery",
	"ac6-injected-provider-denial-named-no-fallback",
	"ac6-sentinel-scan",
];

const find = (events, phase, event) => events.find((e) => e.phase === phase && e.event === event);
const count = (ledger, tool) => ledger.filter((l) => l.tool === tool).length;

function check(id, status, { evidenceReference = null, errorCode = null, summary, prerequisite = null }) {
	return { id, status, evidenceReference, errorCode, summary, prerequisite };
}

// versions: { nodeSupported: boolean, nodeVersion, packages: { name: { version, integrity, installed } } }
// `version` is the lockfile entry; `installed` is the version under node_modules, which is what loads.
export function versionCheck(versions, pins) {
	const mismatched = Object.entries(pins).filter(
		([name, version]) => {
			const pkg = versions.packages[name];
			return pkg?.version !== version || pkg?.installed !== version || !pkg?.integrity?.startsWith("sha512-");
		},
	);
	if (!versions.nodeSupported) {
		return check("ac1-versions-recorded", "fail", {
			evidenceReference: "report.json#/packageVersions",
			errorCode: "node_version_unsupported",
			summary: `Node ${versions.nodeVersion} is below the >=22.19.0 release requirement`,
		});
	}
	if (mismatched.length > 0) {
		return check("ac1-versions-recorded", "fail", {
			evidenceReference: "report.json#/packageVersions",
			errorCode: "version_mismatch",
			summary: `lockfile does not match pins for ${mismatched.map(([n]) => n).join(", ")}`,
		});
	}
	return check("ac1-versions-recorded", "pass", {
		evidenceReference: "report.json#/packageVersions",
		summary: `four pins locked with integrity; Node ${versions.nodeVersion}`,
	});
}

// liveGate: output of lib/gate.mjs liveGate(). Always blocked in this change.
export function liveChecks(liveGate) {
	const prerequisite = liveGate.missing.join("; ");
	return [
		check("ac2-live-vertex-tool-result", "blocked", {
			evidenceReference: "report.json#/checks",
			errorCode: "prerequisite_missing",
			summary: "live Vertex model call under bubblewrap not run",
			prerequisite,
		}),
		check("ac2-live-outside-write-denied", "blocked", {
			evidenceReference: "report.json#/checks",
			errorCode: "prerequisite_missing",
			summary: "outside-workspace write denial under bubblewrap not run; no unsandboxed fallback",
			prerequisite,
		}),
	];
}

// facts: { sigkill, safe, unsafe, workspace, denied } driver results from runScenario().
export function localChecks(facts) {
	const out = [];
	const { sigkill, safe, unsafe, workspace, denied } = facts;
	// Evidence references point at sanitized copies under the report directory (see lib/probe.mjs).
	const ref = (scenario, file) => `scenarios/${scenario}/${file}`;

	const sk = find(sigkill.events, "second", "recovered");
	const submitted = find(sigkill.events, "first", "submitted");
	const killed = sigkill.firstExit.signal === "SIGKILL" && sigkill.killedAtBarrier === "generation-started";
	const identityOk = Boolean(killed && sk && submitted && sk.sameRoot && sk.sameSubmission && sk.settledStatus === "done" && sigkill.secondExit?.code === 0);
	out.push(
		identityOk
			? check("ac3-sigkill-root-input-identity", "pass", {
					evidenceReference: ref("sigkill-input", "events.jsonl"),
					summary: "SIGKILL after input commit; fresh process resumed the same root and submission to done",
				})
			: check("ac3-sigkill-root-input-identity", "fail", {
					evidenceReference: ref("sigkill-input", "events.jsonl"),
					errorCode: killed ? "identity_mismatch" : "not_killed_at_barrier",
					summary: "root or submission identity did not survive the SIGKILL and resume",
				}),
	);

	const dedupOk = Boolean(sk && sk.duplicateIsSameSubmission && sk.userEntries === 1);
	out.push(
		dedupOk
			? check("ac3-duplicate-request-id-dedup", "pass", {
					evidenceReference: ref("sigkill-input", "events.jsonl"),
					summary: "same requestId returned the existing submission; one user input in the transcript",
				})
			: check("ac3-duplicate-request-id-dedup", "fail", {
					evidenceReference: ref("sigkill-input", "events.jsonl"),
					errorCode: "duplicate_submission",
					summary: "duplicate requestId created a second logical submission",
				}),
	);

	const sr = find(safe.events, "second", "recovered");
	const safeRerun = count(safe.ledger, "ledger_safe");
	const safeOk = Boolean(sr && safeRerun === 2 && sr.toolResults.some((t) => t.toolName === "ledger_safe" && !t.isError));
	out.push(
		safeOk
			? check("ac4-safe-tool-rerun-count-2", "pass", {
					evidenceReference: ref("tool-safe", "ledger.jsonl"),
					summary: "replay-safe tool reran on recovery; effect ledger count 2",
				})
			: check("ac4-safe-tool-rerun-count-2", "fail", {
					evidenceReference: ref("tool-safe", "ledger.jsonl"),
					errorCode: safeRerun === 2 ? "safe_result_missing" : "safe_effect_count_mismatch",
					summary: `replay-safe tool effect ledger count ${safeRerun}, expected 2`,
				}),
	);

	const ur = find(unsafe.events, "second", "recovered");
	const unsafeCount = count(unsafe.ledger, "ledger_unsafe");
	const interrupted = ur?.toolResults.find((t) => t.toolName === "ledger_unsafe");
	const unsafeOk = Boolean(unsafeCount === 1 && interrupted?.isError && interrupted.diagnosticCodes.includes("interrupted"));
	out.push(
		unsafeOk
			? check("ac4-unsafe-tool-not-rerun-count-1", "pass", {
					evidenceReference: ref("tool-unsafe", "ledger.jsonl"),
					summary: "non-replay-safe tool not rerun on recovery; effect ledger count 1; result is interrupted",
				})
			: check("ac4-unsafe-tool-not-rerun-count-1", "fail", {
					evidenceReference: ref("tool-unsafe", "ledger.jsonl"),
					errorCode: unsafeCount === 1 ? "interrupted_result_missing" : "unsafe_rerun",
					summary: `non-replay-safe tool effect ledger count ${unsafeCount}, expected 1`,
				}),
	);

	const noFresh = Boolean(
		sr && ur && sr.toolResults.length === 1 && ur.toolResults.length === 1 && sr.pendingScriptedResponses === 0 && ur.pendingScriptedResponses === 0,
	);
	out.push(
		noFresh
			? check("ac4-no-fresh-tool-after-recovery", "pass", {
					evidenceReference: ref("tool-unsafe", "events.jsonl"),
					summary: "recovered agent produced only the interrupted tool result; no fresh tool call",
				})
			: check("ac4-no-fresh-tool-after-recovery", "fail", {
					evidenceReference: ref("tool-unsafe", "events.jsonl"),
					errorCode: "fresh_tool_call",
					summary: "recovered agent issued a tool call beyond the interrupted one",
				}),
	);

	const safeResult = sr?.toolResults.find((t) => t.toolName === "ledger_safe");
	const streamed = Boolean(interrupted?.streamedOutputCommitted && safeResult?.streamedOutputCommitted);
	out.push(
		streamed
			? check("ac4-streamed-output-committed", "pass", {
					evidenceReference: ref("tool-unsafe", "events.jsonl"),
					summary: "streamed output committed before the kill is kept in the interrupted result",
				})
			: check("ac4-streamed-output-committed", "fail", {
					evidenceReference: ref("tool-unsafe", "events.jsonl"),
					errorCode: "streamed_output_not_committed",
					summary: "streamed output before the kill was not kept",
				}),
	);

	const wr = find(workspace.events, "second", "recovered");
	const priorRead = wr?.toolResults[0];
	const retained = Boolean(wr && wr.sameRoot && priorRead?.toolName === "read" && !priorRead.isError);
	out.push(
		retained
			? check("ac5-transcript-survives-checkout-deleted", "pass", {
					evidenceReference: ref("workspace-delete", "events.jsonl"),
					summary: "same root reopened; transcript kept the earlier read result after the checkout was deleted",
				})
			: check("ac5-transcript-survives-checkout-deleted", "fail", {
					evidenceReference: ref("workspace-delete", "events.jsonl"),
					errorCode: "transcript_not_retained",
					summary: "transcript did not survive the reopen",
				}),
	);

	const failingRead = wr?.toolResults.find((t) => t.toolName === "read" && t.isError);
	const missing = Boolean(wr && wr.workspaceExists === false && failingRead?.errorClass === "ENOENT");
	const recoveryRead = wr?.toolResults[1];
	const restored = Boolean(
		wr?.workspaceExists === true && wr.settledStatus === "done" && workspace.secondExit?.code === 0 && workspace.secondExit.signal === null && recoveryRead?.toolName === "read" && recoveryRead.isError === false,
	);
	out.push(
		missing
			? check("ac5-coding-work-recovery", "fail", {
					evidenceReference: ref("workspace-delete", "events.jsonl"),
					errorCode: "missing_workspace",
					summary: "transcript recovered but the checkout is gone; the file read fails with ENOENT, so coding work cannot resume",
				})
			: check("ac5-coding-work-recovery", restored ? "pass" : "fail", {
					evidenceReference: ref("workspace-delete", "events.jsonl"),
					errorCode: restored ? null : "unexpected_workspace_state",
					summary: restored ? "checkout present; recovery read succeeded" : "workspace state did not match the missing-workspace expectation",
				}),
	);

	const dn = find(denied.events, "first", "settled");
	const denialOk = Boolean(dn && dn.settledStatus === "unanswered" && dn.modelRequests === 1 && dn.denialClassification === "PERMISSION_DENIED" && denied.firstExit?.code === 0 && denied.firstExit.signal === null);
	out.push(
		denialOk
			? check("ac6-injected-provider-denial-named-no-fallback", "pass", {
					evidenceReference: ref("provider-denied", "events.jsonl"),
					summary: "negative control: injected denial ended the run as a named failure with one model attempt and no fallback",
				})
			: check("ac6-injected-provider-denial-named-no-fallback", "fail", {
					evidenceReference: ref("provider-denied", "events.jsonl"),
					errorCode: dn && dn.modelRequests !== 1 ? "fallback_suspected" : "denial_not_named",
					summary: "negative control failed: injected denial did not end as a named single-attempt failure",
				}),
	);

	return out;
}
