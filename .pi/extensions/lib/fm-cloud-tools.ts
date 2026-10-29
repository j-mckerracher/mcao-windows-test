import type { ExtensionAPI } from "@earendil-works/pi-coding-agent";
import { Type } from "typebox";
import { runCommandAsync } from "./fm-async-exec.ts";
import { registerFirstmateTool } from "./fm-native-contract.ts";
import {
  CLOUD_CLI_CONTRACT,
  denyUntilCloudGovernance,
  executeCloudAction,
  type CloudDeps,
} from "./fm-cloud-command.ts";

// One fm_cloud tool with seven action-discriminated schemas. Each variant lists
// only its own fields and refuses the rest, and the pure core validates the same
// rules again at the boundary. Both the provider callback and the native bus
// reach the same execute function through registerFirstmateTool.
const cloudParameters = Type.Union([
  Type.Object({ action: Type.Literal("doctor") }, { additionalProperties: false }),
  Type.Object(
    {
      action: Type.Literal("submit"),
      repo: Type.String({ description: "Absolute path of the repository to send." }),
      ref: Type.String({ description: "Full 40-character commit SHA to send." }),
      task: Type.String({ description: "Absolute path of the task file." }),
      model: Type.String({ description: "Approved model identifier." }),
      profile: Type.String({ description: "Approved worker profile." }),
      requestId: Type.String({ description: "Client-generated UUID for this submit request." }),
    },
    { additionalProperties: false },
  ),
  Type.Object(
    { action: Type.Literal("status"), runId: Type.String({ description: "Run ID from submit." }) },
    { additionalProperties: false },
  ),
  Type.Object(
    {
      action: Type.Literal("logs"),
      runId: Type.String({ description: "Run ID from submit." }),
      cursor: Type.Optional(Type.String({ description: "Cursor from the previous logs page." })),
    },
    { additionalProperties: false },
  ),
  Type.Object(
    { action: Type.Literal("cancel"), runId: Type.String({ description: "Run ID from submit." }) },
    { additionalProperties: false },
  ),
  Type.Object(
    {
      action: Type.Literal("fetch"),
      runId: Type.String({ description: "Run ID from submit." }),
      destination: Type.Optional(
        Type.String({ description: "Absolute path of a new, empty destination directory." }),
      ),
    },
    { additionalProperties: false },
  ),
  Type.Object(
    {
      action: Type.Literal("retry"),
      runId: Type.String({ description: "Run ID of a failed run to restart." }),
      requestId: Type.String({ description: "Client-generated UUID for this restart request." }),
    },
    { additionalProperties: false },
  ),
]);

const CLOUD_TOOL_DESCRIPTION = [
  "Operate cloud worker runs through the managed cloud CLI with seven actions.",
  "doctor checks the cloud setup and launches no worker.",
  "submit sends a committed ref and task file and returns the run ID and state without waiting for the run to finish.",
  "status reads a run's state.",
  "logs returns one bounded page per call; pass the returned cursor to read the next page.",
  "cancel requests cancellation, and cancel_requested stays pending until the server confirms it.",
  "fetch imports a finished run's result into a new destination directory.",
  "retry requests a restart of a failed run and is disabled until the restart contract is published.",
  "Each action accepts only its own fields.",
  "Returns a typed error without running anything while the cloud CLI contract or cloud governance is unavailable.",
].join(" ");

export const productionCloudDeps: CloudDeps = {
  contract: CLOUD_CLI_CONTRACT,
  authorize: denyUntilCloudGovernance,
  runCommand: (program, args) => runCommandAsync(program, args),
};

export function registerFmCloudTools(pi: ExtensionAPI, deps: CloudDeps = productionCloudDeps): void {
  registerFirstmateTool(pi, {
    name: "fm_cloud",
    label: "Cloud worker",
    description: CLOUD_TOOL_DESCRIPTION,
    promptSnippet: "Submit, check, cancel, or fetch cloud worker runs through the managed cloud CLI.",
    parameters: cloudParameters,
    execute: async (_toolCallId, params, _signal, _onUpdate, context) => {
      const outcome = await executeCloudAction(deps, params, context);
      if (!outcome.ok) {
        return {
          isError: true,
          content: [{ type: "text", text: `${outcome.error.code}: ${outcome.error.message}` }],
          details: { code: outcome.error.code },
        };
      }
      return {
        content: [{ type: "text", text: outcome.text }],
        details: { action: outcome.action, truncated: outcome.truncated },
      };
    },
  });
}
