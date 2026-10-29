// Scenario definitions for the pi-durable probe fixture (DPES-868).
//
// Each scenario runs in two child processes ("first" then "second"). The driver
// SIGKILLs the first child at a barrier marker written by the fixture, then starts
// the second child against the same retained SQLite store. All evidence the driver
// relies on is written outside the child's memory: marker files, the external
// effect ledger, and an append-only event log. The fixture never writes prompts,
// tool output, or credentials into those files; it writes identifiers and counts.

import { existsSync } from "node:fs";
import { Type } from "@earendil-works/pi-ai";
import { defineTool } from "@earendil-works/pi-durable";
import { fauxAssistantMessage, fauxText, fauxToolCall } from "@earendil-works/pi-ai/providers/faux";
import { BACKGROUND_CONTEXT as context } from "@earendil-works/chord/context";

export const REQUEST_ID = "dpes-868-request-1";
export const DEFAULT_PROMPT = "probe prompt";

// Never resolves. The keep-alive interval stops the process from exiting when the
// event loop is otherwise empty, so the parent can SIGKILL it mid-operation.
export function hangForever() {
	setInterval(() => {}, 1_000);
	return new Promise(() => {});
}

// A replay-safe tool and a default (non-replay-safe) tool. Each call appends one
// line to the external effect ledger and writes the effect marker, then either
// hangs (first phase: the parent kills the process) or completes (second phase).
function effectTool(ctx, { name, replay }) {
	const hold = ctx.phase === "first";
	return defineTool({
		name,
		description: `Fixture effect tool (${replay ?? "unsafe"} replay). Records one external effect.`,
		parameters: Type.Object({ tag: Type.String() }),
		...(replay ? { replay } : {}),
		execute: async (args, api) => {
			api.output(`${name} streamed chunk\n`);
			// Output is committed by a paced progress commit; details() resolves only after that commit,
				// so the chunk is durable before the effect is recorded.
				await api.details({ streamed: true }, context);
				ctx.ledger({ tool: name, tag: args.tag });
			ctx.marker("effect-marker");
			if (hold) return hangForever();
			return {};
		},
	});
}

const hangingGeneration = (ctx) => async () => {
	ctx.marker("generation-started");
	return hangForever();
};

export const SCENARIOS = {
	// AC3: SIGKILL while generation is in flight after the input commit; fresh child resumes.
	"sigkill-input": {
		barrier: "generation-started",
		tools: () => [],
		first: async (ctx) => {
			ctx.faux.setResponses([hangingGeneration(ctx)]);
			const submission = await ctx.root.submit(
				{ type: "input", content: process.env.PROBE_PROMPT ?? DEFAULT_PROMPT, requestId: REQUEST_ID },
				context,
			);
			ctx.record({ event: "submitted", submissionId: submission.id, rootId: ctx.root.id });
			ctx.marker("input-committed");
			await submission.wait(context);
		},
		second: async (ctx) => {
			const first = ctx.findEvent("first", "submitted");
			ctx.faux.setResponses([fauxAssistantMessage(fauxText("recovered answer"))]);
			const submission = await ctx.harness.submission(first.submissionId, context);
			ctx.harness.resume();
			const settled = await submission.wait(context);
			const duplicate = await ctx.root.submit(
				{ type: "input", content: process.env.PROBE_PROMPT ?? DEFAULT_PROMPT, requestId: REQUEST_ID },
				context,
			);
			ctx.record({ event: "recovered",
				rootId: ctx.root.id,
				sameRoot: ctx.root.id === first.rootId,
				submissionId: submission.id,
				sameSubmission: submission.id === first.submissionId,
				settledStatus: settled.status,
				duplicateSubmissionId: duplicate.id,
				duplicateIsSameSubmission: duplicate.id === first.submissionId,
				userEntries: await ctx.countEntries("user"),
				assistantEntries: await ctx.countEntries("assistant"),
			});
		},
	},

	// AC4 (safe): replay-safe tool is killed after its effect marker and reruns on recovery.
	"tool-safe": {
		barrier: "effect-marker",
		tools: (ctx) => [effectTool(ctx, { name: "ledger_safe", replay: "safe" })],
		first: async (ctx) => {
			ctx.faux.setResponses([fauxAssistantMessage(fauxToolCall("ledger_safe", { tag: "call-1" }), { stopReason: "toolUse" })]);
			const submission = await ctx.root.submit({ type: "input", content: DEFAULT_PROMPT, requestId: "dpes-868-tool-safe" }, context);
			ctx.record({ event: "submitted", submissionId: submission.id, rootId: ctx.root.id });
			await submission.wait(context);
		},
		second: async (ctx) => {
			const first = ctx.findEvent("first", "submitted");
			// Only a text answer is scripted: the recovered agent cannot issue a fresh tool call.
			ctx.faux.setResponses([fauxAssistantMessage(fauxText("safe tool recovered"))]);
			const submission = await ctx.harness.submission(first.submissionId, context);
			ctx.harness.resume();
			const settled = await submission.wait(context);
			ctx.record({ event: "recovered",
				rootId: ctx.root.id,
				sameRoot: ctx.root.id === first.rootId,
				settledStatus: settled.status,
				toolResults: await ctx.toolResults(),
				pendingScriptedResponses: ctx.faux.getPendingResponseCount(),
			});
		},
	},

	// AC4 (unsafe): default (non-replay-safe) tool is killed after its effect marker; recovery must not rerun it.
	"tool-unsafe": {
		barrier: "effect-marker",
		tools: (ctx) => [effectTool(ctx, { name: "ledger_unsafe" })],
		first: async (ctx) => {
			ctx.faux.setResponses([fauxAssistantMessage(fauxToolCall("ledger_unsafe", { tag: "call-1" }), { stopReason: "toolUse" })]);
			const submission = await ctx.root.submit({ type: "input", content: DEFAULT_PROMPT, requestId: "dpes-868-tool-unsafe" }, context);
			ctx.record({ event: "submitted", submissionId: submission.id, rootId: ctx.root.id });
			await submission.wait(context);
		},
		second: async (ctx) => {
			const first = ctx.findEvent("first", "submitted");
			ctx.faux.setResponses([fauxAssistantMessage(fauxText("unsafe tool interrupted"))]);
			const submission = await ctx.harness.submission(first.submissionId, context);
			ctx.harness.resume();
			const settled = await submission.wait(context);
			ctx.record({ event: "recovered",
				rootId: ctx.root.id,
				sameRoot: ctx.root.id === first.rootId,
				settledStatus: settled.status,
				toolResults: await ctx.toolResults(),
				pendingScriptedResponses: ctx.faux.getPendingResponseCount(),
			});
		},
	},

	// AC5: transcript survives, checkout is deleted by the driver between processes.
	"workspace-delete": {
		barrier: "generation-started",
		deleteWorkspaceBetween: true,
		tools: () => [],
		first: async (ctx) => {
			ctx.faux.setResponses([
				fauxAssistantMessage(fauxToolCall("read", { path: "hello.txt" }, { id: "call-read-1" }), { stopReason: "toolUse" }),
				hangingGeneration(ctx),
			]);
			const submission = await ctx.root.submit({ type: "input", content: DEFAULT_PROMPT, requestId: "dpes-868-workspace" }, context);
			ctx.record({ event: "submitted", submissionId: submission.id, rootId: ctx.root.id });
			await submission.wait(context);
		},
		second: async (ctx) => {
			const first = ctx.findEvent("first", "submitted");
			ctx.faux.setResponses([
				fauxAssistantMessage(fauxToolCall("read", { path: "hello.txt" }, { id: "call-read-2" }), { stopReason: "toolUse" }),
				fauxAssistantMessage(fauxText("could not read the checkout")),
			]);
			const submission = await ctx.harness.submission(first.submissionId, context);
			ctx.harness.resume();
			const settled = await submission.wait(context);
			ctx.record({ event: "recovered",
				rootId: ctx.root.id,
				sameRoot: ctx.root.id === first.rootId,
				settledStatus: settled.status,
				toolResults: await ctx.toolResults(),
				workspaceExists: existsSync(ctx.workspace),
			});
		},
	},

	// T6: provider denial must surface as a failed run with no second provider or model attempt.
	"provider-denied": {
		barrier: null,
		tools: () => [],
		first: async (ctx) => {
			ctx.faux.setResponses([
				fauxAssistantMessage("", { stopReason: "error", errorMessage: "simulated provider denial (PERMISSION_DENIED)" }),
			]);
			const submission = await ctx.root.submit({ type: "input", content: DEFAULT_PROMPT, requestId: "dpes-868-denied" }, context);
			const settled = await submission.wait(context);
			ctx.record({ event: "settled", settledStatus: settled.status, modelRequests: ctx.faux.state.callCount, denialClassification: await ctx.denialClassification() });
		},
		second: async () => {},
	},
};
