// Pure core of the fm_cloud native tool. It imports nothing from Pi or TypeBox,
// so node can test it directly. Every call, from the provider-facing callback
// or the firstmate:native-tools bus, goes through executeCloudAction, which
// parses the request at the boundary, asks governance, builds argv, runs the
// managed CLI as an argv array (never a shell), and returns redacted, bounded
// text or a typed error. Nothing is retried and nothing falls back to a local
// implementation.

export const CLOUD_PROGRAM = "mcao";

// `retry` is the DPES-879 restart request. It is parsed and governed like the other
// actions but stays disabled until the restart contract is published.
export const CLOUD_ACTIONS = ["doctor", "submit", "status", "logs", "cancel", "fetch", "retry"] as const;
export type CloudAction = (typeof CLOUD_ACTIONS)[number];

/**
 * The managed CLI surface this tool depends on. A null value means the CLI
 * owner (DPES-805/814) has not published that surface, so the action refuses
 * before any command runs. Flags are never guessed.
 */
export interface CloudContract {
  /** Arguments that make the CLI print machine-readable JSON on stdout. */
  readonly machineOutputArgs: readonly string[] | null;
  /** Flag that resumes a logs page from a cursor. */
  readonly logsCursorFlag: string | null;
}

/** Production contract: nothing is published yet, so every action is refused. */
export const CLOUD_CLI_CONTRACT: CloudContract = {
  machineOutputArgs: null,
  logsCursorFlag: null,
};

export const CLOUD_OUTPUT_LIMIT_BYTES = 64 * 1024;
export const CLOUD_LOGS_PAGE_LIMIT = 200;
const STDERR_EXCERPT_BYTES = 2048;

export type CloudErrorCode =
  | "invalid_request"
  | "unsupported_action"
  | "governance_denied"
  | "governance_unavailable"
  | "contract_blocked"
  | "cli_unavailable"
  | "cli_failed"
  | "machine_output_invalid";

export interface CloudError {
  readonly code: CloudErrorCode;
  readonly message: string;
}

export type CloudRequest =
  | { readonly action: "doctor" }
  | {
      readonly action: "submit";
      readonly repo: string;
      readonly ref: string;
      readonly task: string;
      readonly model: string;
      readonly profile: string;
      readonly requestId: string;
    }
  | { readonly action: "status"; readonly runId: string }
  | { readonly action: "logs"; readonly runId: string; readonly cursor?: string }
  | { readonly action: "cancel"; readonly runId: string }
  | { readonly action: "fetch"; readonly runId: string; readonly destination?: string }
  | { readonly action: "retry"; readonly runId: string; readonly requestId: string };

type Field<T> = { ok: true; value: T } | { ok: false; error: CloudError };
export type CloudParse = { ok: true; request: CloudRequest } | { ok: false; error: CloudError };

const RUN_ID = /^[A-Za-z0-9][A-Za-z0-9._-]{0,127}$/;
const TOKEN = /^[A-Za-z0-9][A-Za-z0-9._:-]{0,127}$/;
const COMMIT = /^[0-9a-f]{40}$/;
const UUID = /^[0-9a-f]{8}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{12}$/i;
const CURSOR = /^[^-\x00-\x1f\x7f][^\x00-\x1f\x7f]{0,511}$/;
const CONTROL = /[\x00-\x1f\x7f]/;

function isAbsolutePath(value: string): boolean {
  return value.length <= 4096 && value.startsWith("/") && !CONTROL.test(value);
}

// Field allowlists per action. Anything else is refused before a command runs,
// so for example `follow` on logs and `agent` on submit are unknown fields.
const FIELDS: Readonly<Record<CloudAction, readonly string[]>> = {
  doctor: [],
  submit: ["repo", "ref", "task", "model", "profile", "requestId"],
  status: ["runId"],
  logs: ["runId", "cursor"],
  cancel: ["runId"],
  fetch: ["runId", "destination"],
  retry: ["runId", "requestId"],
};

function failure(code: CloudErrorCode, message: string): { ok: false; error: CloudError } {
  return { ok: false, error: { code, message } };
}

function isRecord(value: unknown): value is Record<string, unknown> {
  return typeof value === "object" && value !== null && !Array.isArray(value);
}

function isCloudAction(value: string): value is CloudAction {
  return (CLOUD_ACTIONS as readonly string[]).includes(value);
}

// Messages name the field and the rule, never the rejected value, which may be a
// credential the caller pasted by mistake.
function requiredText(
  input: Record<string, unknown>,
  name: string,
  check: (value: string) => boolean,
  rule: string,
): Field<string> {
  const value = input[name];
  if (value === undefined) return failure("invalid_request", `${name} is required and must be ${rule}`);
  if (typeof value !== "string" || !check(value)) return failure("invalid_request", `${name} must be ${rule}`);
  return { ok: true, value };
}

function optionalText(
  input: Record<string, unknown>,
  name: string,
  check: (value: string) => boolean,
  rule: string,
): Field<string | undefined> {
  const value = input[name];
  if (value === undefined) return { ok: true, value: undefined };
  if (typeof value !== "string" || !check(value)) return failure("invalid_request", `${name} must be ${rule}`);
  return { ok: true, value };
}

export function parseCloudRequest(input: unknown): CloudParse {
  if (!isRecord(input)) return failure("invalid_request", "request must be an object");
  const action = input.action;
  if (typeof action !== "string" || !isCloudAction(action)) {
    return failure("unsupported_action", "action must be one of doctor, submit, status, logs, cancel, fetch, retry");
  }
  for (const key of Object.keys(input)) {
    if (key !== "action" && !FIELDS[action].includes(key)) {
      return failure("invalid_request", `field not allowed for action ${action}`);
    }
  }
  const runId = (): Field<string> => requiredText(input, "runId", (v) => RUN_ID.test(v), "a run ID");
  switch (action) {
    case "doctor":
      return { ok: true, request: { action } };
    case "submit": {
      const repo = requiredText(input, "repo", isAbsolutePath, "an absolute repository path");
      if (!repo.ok) return repo;
      const ref = requiredText(input, "ref", (v) => COMMIT.test(v), "a full 40-character commit SHA");
      if (!ref.ok) return ref;
      const task = requiredText(input, "task", isAbsolutePath, "an absolute task file path");
      if (!task.ok) return task;
      const model = requiredText(input, "model", (v) => TOKEN.test(v), "an approved model identifier");
      if (!model.ok) return model;
      const profile = requiredText(input, "profile", (v) => TOKEN.test(v), "a profile identifier");
      if (!profile.ok) return profile;
      const requestId = requiredText(input, "requestId", (v) => UUID.test(v), "a UUID");
      if (!requestId.ok) return requestId;
      return {
        ok: true,
        request: {
          action,
          repo: repo.value,
          ref: ref.value,
          task: task.value,
          model: model.value,
          profile: profile.value,
          requestId: requestId.value,
        },
      };
    }
    case "status":
    case "cancel": {
      const id = runId();
      if (!id.ok) return id;
      return { ok: true, request: { action, runId: id.value } };
    }
    case "logs": {
      const id = runId();
      if (!id.ok) return id;
      const cursor = optionalText(input, "cursor", (v) => CURSOR.test(v), "a cursor that does not start with -");
      if (!cursor.ok) return cursor;
      return cursor.value === undefined
        ? { ok: true, request: { action, runId: id.value } }
        : { ok: true, request: { action, runId: id.value, cursor: cursor.value } };
    }
    case "fetch": {
      const id = runId();
      if (!id.ok) return id;
      const destination = optionalText(input, "destination", isAbsolutePath, "an absolute destination path");
      if (!destination.ok) return destination;
      return destination.value === undefined
        ? { ok: true, request: { action, runId: id.value } }
        : { ok: true, request: { action, runId: id.value, destination: destination.value } };
    }
    case "retry": {
      const id = runId();
      if (!id.ok) return id;
      const requestId = requiredText(input, "requestId", (v) => UUID.test(v), "a UUID");
      if (!requestId.ok) return requestId;
      return { ok: true, request: { action, runId: id.value, requestId: requestId.value } };
    }
  }
}

export type CloudArgv = { ok: true; args: readonly string[] } | { ok: false; error: CloudError };

function blocked(detail: string): CloudArgv {
  return {
    ok: false,
    error: {
      code: "contract_blocked",
      message: `${detail} is not published by the managed cloud CLI yet (DPES-805/814); no command ran`,
    },
  };
}

// Argv after the program name. Only the DPES-803 CLI shape is used, plus the two
// contract values, which are null in production and so block every action.
export function buildCloudArgv(request: CloudRequest, contract: CloudContract): CloudArgv {
  const machine = contract.machineOutputArgs;
  if (machine === null) return blocked("machine-readable output mode");
  switch (request.action) {
    case "doctor":
      return { ok: true, args: ["cloud", "doctor", ...machine] };
    case "submit":
      return {
        ok: true,
        args: [
          "cloud",
          "submit",
          "--repo",
          request.repo,
          "--ref",
          request.ref,
          "--task",
          request.task,
          "--agent",
          "pi",
          "--model",
          request.model,
          "--profile",
          request.profile,
          "--request-id",
          request.requestId,
          ...machine,
        ],
      };
    case "status":
      return { ok: true, args: ["cloud", "status", request.runId, ...machine] };
    case "logs": {
      if (request.cursor === undefined) return { ok: true, args: ["cloud", "logs", request.runId, ...machine] };
      if (contract.logsCursorFlag === null) return blocked("the logs cursor flag");
      return {
        ok: true,
        args: ["cloud", "logs", request.runId, contract.logsCursorFlag, request.cursor, ...machine],
      };
    }
    case "cancel":
      return { ok: true, args: ["cloud", "cancel", request.runId, ...machine] };
    case "fetch": {
      const destination = request.destination === undefined ? [] : ["--destination", request.destination];
      return { ok: true, args: ["cloud", "fetch", request.runId, ...destination, ...machine] };
    }
    // Restart stays disabled: the request and its eligibility outcomes are
    // unpublished (DPES-805/870 Q1, DPES-869/870 Q2), so no argv is guessed.
    case "retry":
      return {
        ok: false,
        error: {
          code: "contract_blocked",
          message:
            "restart is disabled: the retry request contract (DPES-805/870 Q1) and the recovery policy (DPES-869/870 Q2) are not published; no command ran",
        },
      };
  }
}

// Ordered: a signed URL is replaced whole before its query parameters are
// looked at, so no part of it survives.
const REDACTIONS: readonly (readonly [RegExp, string])[] = [
  [/https?:\/\/[^\s"'<>]*X-Goog-[^\s"'<>]*/gi, "[redacted signed URL]"],
  [/\bBearer\s+[\w.~+/=-]+/gi, "Bearer [redacted]"],
  [/eyJ[A-Za-z0-9_-]+\.[A-Za-z0-9_-]+\.[A-Za-z0-9_-]*/g, "[redacted token]"],
  [/(X-Goog-(?:Signature|Credential)=)[^&\s"'<>]+/gi, "$1[redacted]"],
  [/((?:access_token|id_token|refresh_token)=)[^&\s"'<>]+/gi, "$1[redacted]"],
];

export function redactCloudText(text: string): string {
  return REDACTIONS.reduce((current, [pattern, replacement]) => current.replace(pattern, replacement), text);
}

function redactValue(value: unknown): unknown {
  if (typeof value === "string") return redactCloudText(value);
  if (Array.isArray(value)) return value.map(redactValue);
  if (isRecord(value)) return Object.fromEntries(Object.entries(value).map(([key, item]) => [key, redactValue(item)]));
  return value;
}

export function boundCloudText(
  text: string,
  limit: number = CLOUD_OUTPUT_LIMIT_BYTES,
): { text: string; truncated: boolean } {
  const encoded = new TextEncoder().encode(text);
  if (encoded.byteLength <= limit) return { text, truncated: false };
  const kept = new TextDecoder().decode(encoded.subarray(0, limit));
  return { text: `${kept}\n[truncated at ${limit} bytes]`, truncated: true };
}

type Shaped = { ok: true; output: Record<string, unknown> } | { ok: false; error: CloudError };

function invalidOutput(action: CloudAction, rule: string): Shaped {
  return { ok: false, error: { code: "machine_output_invalid", message: `${action} output ${rule}` } };
}

// Keeps only the fields each action's contract names, so the tool never echoes
// anything else the CLI prints. Field names are placeholders until DPES-805/814
// publish the machine schema.
function shapeOutput(action: CloudAction, value: unknown): Shaped {
  switch (action) {
    case "doctor":
      return isRecord(value) ? { ok: true, output: value } : invalidOutput(action, "must be a JSON object");
    case "submit":
    case "status":
    case "fetch": {
      if (!isRecord(value) || typeof value.run_id !== "string" || typeof value.state !== "string") {
        return invalidOutput(action, "must include run_id and state strings");
      }
      return { ok: true, output: { run_id: value.run_id, state: value.state } };
    }
    case "cancel": {
      if (
        !isRecord(value) ||
        typeof value.run_id !== "string" ||
        typeof value.state !== "string" ||
        typeof value.cancel_requested !== "boolean"
      ) {
        return invalidOutput(action, "must include run_id, state, and cancel_requested");
      }
      // The server's state is reported as-is. It is never mapped to "cancelled"
      // here, because cancellation is pending until the server confirms it.
      return {
        ok: true,
        output: { run_id: value.run_id, state: value.state, cancel_requested: value.cancel_requested },
      };
    }
    case "logs": {
      if (!isRecord(value) || !Array.isArray(value.entries)) {
        return invalidOutput(action, "must include an entries array");
      }
      if (value.entries.length > CLOUD_LOGS_PAGE_LIMIT) {
        return invalidOutput(action, `has more than ${CLOUD_LOGS_PAGE_LIMIT} entries`);
      }
      if (value.next_cursor !== null && typeof value.next_cursor !== "string") {
        return invalidOutput(action, "must give next_cursor as a string or null");
      }
      return { ok: true, output: { entries: value.entries, next_cursor: value.next_cursor } };
    }
    case "retry":
      return invalidOutput(action, "has no published response contract");
  }
}

export type CloudAuthorization = { readonly allowed: true } | { readonly allowed: false; readonly error: CloudError };

export interface CloudCommandOutput {
  readonly status: number | null;
  readonly stdout: string;
  readonly stderr: string;
}

export interface CloudDeps {
  readonly contract: CloudContract;
  /** Governance port. It runs before any command and must fail closed. */
  readonly authorize: (context: unknown, request: CloudRequest) => Promise<CloudAuthorization>;
  /** Runs the program with an argv array. Production wraps runCommandAsync. */
  readonly runCommand: (program: string, args: readonly string[]) => Promise<CloudCommandOutput>;
}

export type CloudOutcome =
  | { readonly ok: true; readonly action: CloudAction; readonly text: string; readonly truncated: boolean }
  | { readonly ok: false; readonly error: CloudError };

// DPES-874 owns the cloud governance decision and is not in this checkout, so the
// production port denies every action until that decision is wired in.
export async function denyUntilCloudGovernance(): Promise<CloudAuthorization> {
  return {
    allowed: false,
    error: {
      code: "governance_unavailable",
      message: "cloud governance (DPES-874) is not wired in this build; no cloud action ran",
    },
  };
}

export async function executeCloudAction(deps: CloudDeps, input: unknown, context: unknown): Promise<CloudOutcome> {
  const parsed = parseCloudRequest(input);
  if (!parsed.ok) return { ok: false, error: parsed.error };
  const request = parsed.request;

  let authorization: CloudAuthorization;
  try {
    authorization = await deps.authorize(context, request);
  } catch {
    authorization = {
      allowed: false,
      error: { code: "governance_unavailable", message: "cloud governance check failed; no cloud action ran" },
    };
  }
  if (!authorization.allowed) return { ok: false, error: authorization.error };

  const built = buildCloudArgv(request, deps.contract);
  if (!built.ok) return { ok: false, error: built.error };

  let output: CloudCommandOutput;
  try {
    output = await deps.runCommand(CLOUD_PROGRAM, built.args);
  } catch {
    return {
      ok: false,
      error: { code: "cli_unavailable", message: `${request.action}: the managed cloud CLI could not be started; nothing was retried` },
    };
  }
  if (output.status !== 0) {
    const status = output.status === null ? "without an exit status" : `with exit status ${output.status}`;
    const excerpt = boundCloudText(redactCloudText(output.stderr.trim()), STDERR_EXCERPT_BYTES).text;
    return {
      ok: false,
      error: {
        code: "cli_failed",
        message: `${request.action} failed ${status}${excerpt ? `: ${excerpt}` : ""}; nothing was retried`,
      },
    };
  }

  let value: unknown;
  try {
    value = JSON.parse(output.stdout);
  } catch {
    return {
      ok: false,
      error: { code: "machine_output_invalid", message: `${request.action} output was not JSON` },
    };
  }
  const shaped = shapeOutput(request.action, value);
  if (!shaped.ok) return { ok: false, error: shaped.error };

  const bounded = boundCloudText(JSON.stringify(redactValue(shaped.output), null, 2));
  return { ok: true, action: request.action, text: bounded.text, truncated: bounded.truncated };
}
