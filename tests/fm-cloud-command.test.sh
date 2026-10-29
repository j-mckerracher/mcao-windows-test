#!/usr/bin/env bash
# Pure-core checks for the fm_cloud native tool (.pi/extensions/lib/fm-cloud-command.ts):
# request validation before any command runs, exact argv for each action,
# single-command execution with no polling or fallback, governance and contract
# refusals that run zero commands, redaction of credential sentinels, bounded
# output, and typed errors for failed, unavailable, or malformed CLI results.
# Node runs the module directly against a fake command runner, so no Pi install,
# cloud call, or credential is involved.
set -u

# shellcheck source=tests/lib.sh
. "$(dirname "${BASH_SOURCE[0]}")/lib.sh"

TMP_ROOT=$(fm_test_tmproot fm-cloud-command)

command -v node >/dev/null 2>&1 || fail "node is required for the fm_cloud pure-core test"
[ -f "$ROOT/.pi/extensions/lib/fm-cloud-command.ts" ] || fail "missing fm-cloud-command.ts"

# The core must run without Pi or TypeBox. Importing an isolated copy outside
# any node_modules tree proves that: a stray runtime import fails the node run.
ISOLATED="$TMP_ROOT/isolated"
mkdir -p "$ISOLATED"
cp "$ROOT/.pi/extensions/lib/fm-cloud-command.ts" "$ISOLATED/fm-cloud-command.ts"
MODULE="$ISOLATED/fm-cloud-command.ts"

JS_FILE="$TMP_ROOT/fm-cloud-command-checks.mjs"
cat > "$JS_FILE" <<'JS'
import assert from "node:assert/strict";
import { pathToFileURL } from "node:url";

const mod = await import(pathToFileURL(process.env.CLOUD_MODULE).href);
const {
  parseCloudRequest,
  buildCloudArgv,
  executeCloudAction,
  denyUntilCloudGovernance,
  boundCloudText,
  redactCloudText,
  CLOUD_CLI_CONTRACT,
  CLOUD_LOGS_PAGE_LIMIT,
  CLOUD_OUTPUT_LIMIT_BYTES,
} = mod;

// Fixture flags are deliberately fake. They exercise argv exactness without
// publishing a real CLI surface, which DPES-805/814 own.
const FIXTURE = { machineOutputArgs: ["--fixture-machine-mode"], logsCursorFlag: "--fixture-cursor" };
const M = ["--fixture-machine-mode"];
const COMMIT = "0123456789abcdef0123456789abcdef01234567";
const UUID = "123e4567-e89b-42d3-a456-426614174000";
// Shell metacharacters in a path must stay one argv element, never a shell.
const REPO = "/Users/dev/work tree/$(id);`touch x`|&/repo";
const TASK = "/Users/dev/tasks/task.md";
const RUN = "run-01";

const allow = async () => ({ allowed: true });
const deny = async () => ({
  allowed: false,
  error: { code: "governance_denied", message: "denied by fixture" },
});

function fakeRunner(responses = []) {
  const calls = [];
  const queue = [...responses];
  const runCommand = async (program, args) => {
    calls.push({ program, args: [...args] });
    const next = queue.shift() ?? { status: 0, stdout: "{}", stderr: "" };
    if (next instanceof Error) throw next;
    return next;
  };
  return { calls, runCommand };
}

const okOutput = (value) => ({
  status: 0,
  stdout: typeof value === "string" ? value : JSON.stringify(value),
  stderr: "",
});

function depsWith(runner, overrides = {}) {
  return { contract: FIXTURE, authorize: allow, runCommand: runner.runCommand, ...overrides };
}

function succeeded(outcome) {
  assert.equal(outcome.ok, true, `expected success, got ${JSON.stringify(outcome)}`);
  return outcome;
}

function refusedWith(outcome, code) {
  assert.equal(outcome.ok, false, `expected refusal ${code}, got ${JSON.stringify(outcome)}`);
  assert.equal(outcome.error.code, code, JSON.stringify(outcome.error));
  return outcome.error;
}

const SUBMIT = {
  action: "submit",
  repo: REPO,
  ref: COMMIT,
  task: TASK,
  model: "gemini-3.8-flash",
  profile: "standard",
  requestId: UUID,
};

const tests = [];
const test = (name, fn) => tests.push({ name, fn });

test("argv is exact for every action under the fixture contract", () => {
  const cases = [
    [{ action: "doctor" }, ["cloud", "doctor", ...M]],
    [
      SUBMIT,
      [
        "cloud", "submit",
        "--repo", REPO,
        "--ref", COMMIT,
        "--task", TASK,
        "--agent", "pi",
        "--model", "gemini-3.8-flash",
        "--profile", "standard",
        "--request-id", UUID,
        ...M,
      ],
    ],
    [{ action: "status", runId: RUN }, ["cloud", "status", RUN, ...M]],
    [{ action: "logs", runId: RUN }, ["cloud", "logs", RUN, ...M]],
    [
      { action: "logs", runId: RUN, cursor: "c2" },
      ["cloud", "logs", RUN, "--fixture-cursor", "c2", ...M],
    ],
    [{ action: "cancel", runId: RUN }, ["cloud", "cancel", RUN, ...M]],
    [{ action: "fetch", runId: RUN }, ["cloud", "fetch", RUN, ...M]],
    [
      { action: "fetch", runId: RUN, destination: "/abs/new dir;$(x)" },
      ["cloud", "fetch", RUN, "--destination", "/abs/new dir;$(x)", ...M],
    ],
  ];
  for (const [input, expected] of cases) {
    const parsed = parseCloudRequest(input);
    assert.equal(parsed.ok, true, JSON.stringify(parsed));
    const built = buildCloudArgv(parsed.request, FIXTURE);
    assert.equal(built.ok, true, JSON.stringify(built));
    assert.deepEqual([...built.args], expected, `argv for ${input.action}`);
  }
  const parsed = parseCloudRequest(SUBMIT);
  const built = buildCloudArgv(parsed.request, FIXTURE);
  assert.equal(built.args[built.args.indexOf("--repo") + 1], REPO, "metacharacter repo path is one element");
});

test("a valid command runs exactly once as the mcao program with that argv", async () => {
  const runner = fakeRunner([okOutput({ run_id: RUN, state: "awaiting_upload" })]);
  succeeded(await executeCloudAction(depsWith(runner), SUBMIT, {}));
  assert.equal(runner.calls.length, 1);
  assert.equal(runner.calls[0].program, "mcao");
  assert.deepEqual(runner.calls[0].args.slice(0, 2), ["cloud", "submit"]);
});

test("malformed, unknown, and action-incompatible inputs are refused before any command", async () => {
  const invalid = [
    ["a non-object request", "doctor"],
    ["a missing action", {}],
    ["an unknown action", { action: "start" }],
    ["a follow action", { action: "follow" }],
    ["doctor with a run ID", { action: "doctor", runId: RUN }],
    ["status without a run ID", { action: "status" }],
    ["status with a logs cursor", { action: "status", runId: RUN, cursor: "c" }],
    ["submit with an agent field", { ...SUBMIT, agent: "claude" }],
    ["submit with a branch name as ref", { ...SUBMIT, ref: "main" }],
    ["submit with a relative repo path", { ...SUBMIT, repo: "relative/repo" }],
    ["submit with a control character in the task path", { ...SUBMIT, task: "/x\n/y" }],
    ["submit with a non-UUID request ID", { ...SUBMIT, requestId: "not-a-uuid" }],
    ["submit without a profile", { ...SUBMIT, profile: undefined }],
    ["logs with a follow flag", { action: "logs", runId: RUN, follow: true }],
    ["logs with a flag-like cursor", { action: "logs", runId: RUN, cursor: "--all" }],
    ["logs with a null cursor", { action: "logs", runId: RUN, cursor: null }],
    ["a run ID that looks like a flag", { action: "status", runId: "--help" }],
    ["a run ID with a space", { action: "cancel", runId: "a b" }],
    ["fetch with a relative destination", { action: "fetch", runId: RUN, destination: "rel" }],
    ["retry without a request ID", { action: "retry", runId: RUN }],
    ["retry with a non-UUID request ID", { action: "retry", runId: RUN, requestId: "not-a-uuid" }],
    ["retry with a destination field", { action: "retry", runId: RUN, requestId: UUID, destination: "/abs/dest" }],
    ["retry with a flag-like run ID", { action: "retry", runId: "--force", requestId: UUID }],
  ];
  for (const [label, input] of invalid) {
    const runner = fakeRunner([okOutput({})]);
    const outcome = await executeCloudAction(depsWith(runner), input, {});
    assert.equal(outcome.ok, false, `${label} must be refused`);
    assert.ok(
      ["invalid_request", "unsupported_action"].includes(outcome.error.code),
      `${label}: ${JSON.stringify(outcome.error)}`,
    );
    assert.equal(runner.calls.length, 0, `${label} must run zero commands`);
  }
});

test("doctor issues one command and returns without polling", async () => {
  const runner = fakeRunner([okOutput({ ok: true, checks: ["auth"] })]);
  const outcome = succeeded(await executeCloudAction(depsWith(runner), { action: "doctor" }, {}));
  assert.deepEqual(runner.calls.map((call) => call.args), [["cloud", "doctor", ...M]]);
  assert.equal(outcome.action, "doctor");
});

test("submit returns the run ID and state with one command and no polling", async () => {
  const runner = fakeRunner([
    okOutput({ run_id: RUN, state: "awaiting_upload", extra_field: "not forwarded" }),
  ]);
  const outcome = succeeded(await executeCloudAction(depsWith(runner), SUBMIT, {}));
  assert.equal(runner.calls.length, 1, "submit must not poll");
  assert.deepEqual(JSON.parse(outcome.text), { run_id: RUN, state: "awaiting_upload" });
});

test("logs pages follow the returned cursor and the page bound holds", async () => {
  const runner = fakeRunner([
    okOutput({ entries: [{ seq: 1 }], next_cursor: "c2" }),
    okOutput({ entries: [{ seq: 2 }], next_cursor: null }),
    okOutput({ entries: new Array(CLOUD_LOGS_PAGE_LIMIT + 1).fill({ seq: 0 }), next_cursor: null }),
  ]);
  const first = succeeded(await executeCloudAction(depsWith(runner), { action: "logs", runId: RUN }, {}));
  assert.equal(JSON.parse(first.text).next_cursor, "c2");
  const second = succeeded(
    await executeCloudAction(depsWith(runner), { action: "logs", runId: RUN, cursor: "c2" }, {}),
  );
  assert.equal(JSON.parse(second.text).next_cursor, null);
  assert.deepEqual(runner.calls[1].args, ["cloud", "logs", RUN, "--fixture-cursor", "c2", ...M]);
  const oversized = await executeCloudAction(depsWith(runner), { action: "logs", runId: RUN }, {});
  refusedWith(oversized, "machine_output_invalid");
});

test("cancel reports cancel_requested with the server's state and never a local cancelled", async () => {
  const runner = fakeRunner([okOutput({ run_id: RUN, state: "running", cancel_requested: true })]);
  const outcome = succeeded(await executeCloudAction(depsWith(runner), { action: "cancel", runId: RUN }, {}));
  const body = JSON.parse(outcome.text);
  assert.equal(body.state, "running");
  assert.equal(body.cancel_requested, true);
  assert.equal(outcome.text.includes("cancelled"), false, "cancellation is not confirmed by this tool");
});

test("cancel without cancel_requested is a shape failure", async () => {
  const runner = fakeRunner([okOutput({ run_id: RUN, state: "running" })]);
  refusedWith(await executeCloudAction(depsWith(runner), { action: "cancel", runId: RUN }, {}), "machine_output_invalid");
  assert.equal(runner.calls.length, 1);
});

test("fetch passes the destination flag and returns the CLI state", async () => {
  const runner = fakeRunner([okOutput({ run_id: RUN, state: "imported" })]);
  const outcome = succeeded(
    await executeCloudAction(depsWith(runner), { action: "fetch", runId: RUN, destination: "/abs/dest" }, {}),
  );
  assert.deepEqual(runner.calls[0].args, ["cloud", "fetch", RUN, "--destination", "/abs/dest", ...M]);
  assert.deepEqual(JSON.parse(outcome.text), { run_id: RUN, state: "imported" });
});

test("governance denial and the default deny port run zero commands", async () => {
  const denied = fakeRunner([okOutput({})]);
  refusedWith(await executeCloudAction(depsWith(denied, { authorize: deny }), { action: "doctor" }, {}), "governance_denied");
  assert.equal(denied.calls.length, 0);

  const unwired = fakeRunner([okOutput({})]);
  const unwiredDeps = { contract: FIXTURE, authorize: denyUntilCloudGovernance, runCommand: unwired.runCommand };
  refusedWith(await executeCloudAction(unwiredDeps, SUBMIT, {}), "governance_unavailable");
  assert.equal(unwired.calls.length, 0);
});

test("a governance check that throws fails closed without running a command", async () => {
  const runner = fakeRunner([okOutput({})]);
  const throwing = async () => {
    throw new Error("policy service unreachable");
  };
  refusedWith(await executeCloudAction(depsWith(runner, { authorize: throwing }), { action: "doctor" }, {}), "governance_unavailable");
  assert.equal(runner.calls.length, 0);
});

test("production contract blocks every action before any command, even when governance allows", async () => {
  const requests = [
    { action: "doctor" },
    SUBMIT,
    { action: "status", runId: RUN },
    { action: "logs", runId: RUN },
    { action: "logs", runId: RUN, cursor: "c2" },
    { action: "cancel", runId: RUN },
    { action: "fetch", runId: RUN },
    { action: "retry", runId: RUN, requestId: UUID },
  ];
  for (const request of requests) {
    const runner = fakeRunner([okOutput({})]);
    const deps = { contract: CLOUD_CLI_CONTRACT, authorize: allow, runCommand: runner.runCommand };
    refusedWith(await executeCloudAction(deps, request, {}), "contract_blocked");
    assert.equal(runner.calls.length, 0, `${request.action} must run zero commands under the production contract`);
  }
});

test("retry is governed first and stays disabled: a valid restart request runs no command", async () => {
  const request = { action: "retry", runId: RUN, requestId: UUID };
  const parsed = parseCloudRequest(request);
  assert.equal(parsed.ok, true, JSON.stringify(parsed));

  const seen = [];
  const recording = async (_context, governed) => {
    seen.push(governed.action);
    return { allowed: true };
  };
  const fixtureRunner = fakeRunner([okOutput({})]);
  const blocked = refusedWith(
    await executeCloudAction(depsWith(fixtureRunner, { authorize: recording }), request, {}),
    "contract_blocked",
  );
  assert.match(blocked.message, /restart is disabled/);
  assert.deepEqual(seen, ["retry"], "governance must see the restart request before the contract refuses it");
  assert.equal(fixtureRunner.calls.length, 0, "restart must run zero commands under the fixture contract");

  const deniedRunner = fakeRunner([okOutput({})]);
  refusedWith(await executeCloudAction(depsWith(deniedRunner, { authorize: deny }), request, {}), "governance_denied");
  assert.equal(deniedRunner.calls.length, 0);

  const unwiredRunner = fakeRunner([okOutput({})]);
  const unwiredDeps = { contract: FIXTURE, authorize: denyUntilCloudGovernance, runCommand: unwiredRunner.runCommand };
  refusedWith(await executeCloudAction(unwiredDeps, request, {}), "governance_unavailable");
  assert.equal(unwiredRunner.calls.length, 0);
});

test("a logs cursor is blocked when only the cursor flag is unpublished", async () => {
  const runner = fakeRunner([okOutput({ entries: [], next_cursor: null })]);
  const contract = { machineOutputArgs: M, logsCursorFlag: null };
  const deps = { contract, authorize: allow, runCommand: runner.runCommand };
  succeeded(await executeCloudAction(deps, { action: "logs", runId: RUN }, {}));
  refusedWith(await executeCloudAction(deps, { action: "logs", runId: RUN, cursor: "c2" }, {}), "contract_blocked");
  assert.equal(runner.calls.length, 1, "only the cursor-free page ran");
});

test("credential sentinels never appear in returned text or refusal messages", async () => {
  const sentinels = [
    "ya29.a0AbCdEf-123_456",
    "eyJhbGciOiJIUzI1NiJ9",
    "0badc0ffee",
    "svc%40proj",
    "ya29.secretValue",
    "X-Goog-",
  ];
  const signedUrl =
    "https://storage.googleapis.com/bucket/run/out.tar?X-Goog-Algorithm=GOOG4" +
    "&X-Goog-Credential=svc%40proj.iam/20260101/auto/storage&X-Goog-Signature=0badc0ffee";
  const body = {
    entries: [
      { message: "Authorization: Bearer ya29.a0AbCdEf-123_456" },
      { token: "eyJhbGciOiJIUzI1NiJ9.eyJzdWIiOiJkZXYifQ.c2lnbmF0dXJl" }, // gitleaks:allow
      { url: signedUrl },
      { link: "https://example.test/x?access_token=ya29.secretValue" },
    ],
    next_cursor: null,
  };
  const runner = fakeRunner([okOutput(body)]);
  const outcome = succeeded(await executeCloudAction(depsWith(runner), { action: "logs", runId: RUN }, {}));
  for (const sentinel of sentinels) {
    assert.equal(outcome.text.includes(sentinel), false, `sentinel ${sentinel} leaked into output`);
  }
  assert.ok(outcome.text.includes("[redacted signed URL]"));

  const failing = fakeRunner([
    { status: 3, stdout: "", stderr: "auth failed for Bearer ya29.a0AbCdEf-123_456 eyJhbGciOiJIUzI1NiJ9.eyJzdWIiOiJkZXYifQ.c2lnbmF0dXJl" },
  ]);
  const error = refusedWith(await executeCloudAction(depsWith(failing), { action: "status", runId: RUN }, {}), "cli_failed");
  for (const sentinel of sentinels) {
    assert.equal(error.message.includes(sentinel), false, `sentinel ${sentinel} leaked into error`);
  }
});

test("redaction covers Bearer tokens, JWTs, signed URLs, X-Goog parameters, and access tokens", () => {
  assert.equal(redactCloudText("Bearer abc.def"), "Bearer [redacted]");
  assert.equal(redactCloudText("Bearer ya29.a0AbCdEf-123_456"), "Bearer [redacted]", "underscore is token text");
  assert.equal(
    redactCloudText("eyJhbGciOiJIUzI1NiJ9.eyJzdWIiOiJkZXYifQ.c2ln"),
    "[redacted token]",
  );
  assert.equal(
    redactCloudText("see https://storage.test/o?X-Goog-Signature=zz&q=1 now"),
    "see [redacted signed URL] now",
  );
  assert.equal(redactCloudText("X-Goog-Credential=cred&x=1"), "X-Goog-Credential=[redacted]&x=1");
  assert.equal(redactCloudText("refresh_token=r1&a=2"), "refresh_token=[redacted]&a=2");
  assert.equal(redactCloudText("plain status: running"), "plain status: running");
});

test("oversized output is bounded with an explicit truncation marker", async () => {
  const bounded = boundCloudText("a".repeat(CLOUD_OUTPUT_LIMIT_BYTES + 10));
  assert.equal(bounded.truncated, true);
  assert.ok(bounded.text.endsWith(`[truncated at ${CLOUD_OUTPUT_LIMIT_BYTES} bytes]`));
  assert.equal(boundCloudText("short").truncated, false);

  const runner = fakeRunner([okOutput({ blob: "x".repeat(CLOUD_OUTPUT_LIMIT_BYTES + 100) })]);
  const outcome = succeeded(await executeCloudAction(depsWith(runner), { action: "doctor" }, {}));
  assert.equal(outcome.truncated, true);
  assert.ok(outcome.text.includes(`[truncated at ${CLOUD_OUTPUT_LIMIT_BYTES} bytes]`));
});

test("a non-zero exit, a null status, and a runner failure each return one typed error with no retry", async () => {
  const cases = [
    [{ status: 2, stdout: "", stderr: "not signed in" }, "cli_failed"],
    [{ status: null, stdout: "", stderr: "" }, "cli_failed"],
    [new Error("spawn mcao ENOENT"), "cli_unavailable"],
  ];
  for (const [response, code] of cases) {
    const runner = fakeRunner([response, okOutput({ run_id: RUN, state: "x" })]);
    refusedWith(await executeCloudAction(depsWith(runner), { action: "status", runId: RUN }, {}), code);
    assert.equal(runner.calls.length, 1, `${code} must not trigger a second command`);
  }
});

test("invalid JSON and wrong output shapes return machine_output_invalid with no retry", async () => {
  const logsInput = { action: "logs", runId: RUN };
  const cases = [
    [SUBMIT, okOutput("not json at all")],
    [SUBMIT, okOutput({ run_id: RUN })],
    [SUBMIT, okOutput([1, 2, 3])],
    [logsInput, okOutput({ entries: "not-an-array", next_cursor: null })],
    [logsInput, okOutput({ entries: [], next_cursor: 7 })],
  ];
  for (const [input, response] of cases) {
    const runner = fakeRunner([response, okOutput({ run_id: RUN, state: "x" })]);
    refusedWith(await executeCloudAction(depsWith(runner), input, {}), "machine_output_invalid");
    assert.equal(runner.calls.length, 1, "a malformed result must not trigger a second command");
  }
});

test("production contract publishes no CLI surface", () => {
  assert.deepEqual(CLOUD_CLI_CONTRACT, { machineOutputArgs: null, logsCursorFlag: null });
});

let failed = 0;
for (const { name, fn } of tests) {
  try {
    await fn();
    console.log(`ok - ${name}`);
  } catch (error) {
    failed += 1;
    console.log(`not ok - ${name}: ${error.message}`);
  }
}
process.exitCode = failed === 0 ? 0 : 1;
JS

out_file="$TMP_ROOT/fm-cloud-command.out"
CLOUD_MODULE="$MODULE" node "$JS_FILE" > "$out_file" 2>&1
status=$?
cat "$out_file"
[ "$status" -eq 0 ] || fail "fm_cloud pure-core checks failed (see above)"
