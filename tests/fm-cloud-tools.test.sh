#!/usr/bin/env bash
# Registration checks for fm_cloud (.pi/extensions/lib/fm-cloud-tools.ts): one provider
# registration, the same schema and execute on the firstmate:native-tools bus, a
# truthful six-action schema that refuses extra fields, and production refusal
# through governance. The module runs from a temp fixture with a TypeBox stub, so
# no Pi install or package is needed. Primary-session wiring is checked in
# fm-pi-watch-extension.test.sh; branch sessions are pinned to BRANCH_TOOL_NAMES
# by fm-pi-branch-extension.test.sh.
set -u

# shellcheck source=tests/lib.sh
. "$(dirname "${BASH_SOURCE[0]}")/lib.sh"

TMP_ROOT=$(fm_test_tmproot fm-cloud-tools)
FIX="$TMP_ROOT/repo"
mkdir -p "$FIX/.pi/extensions/lib" "$FIX/node_modules/typebox"
cp "$ROOT/.pi/extensions/lib/fm-cloud-tools.ts" "$FIX/.pi/extensions/lib/fm-cloud-tools.ts"
for f in lib/fm-cloud-command.ts lib/fm-native-contract.ts lib/fm-async-exec.ts; do
  cp "$ROOT/.pi/extensions/$f" "$FIX/.pi/extensions/$f"
done
cat > "$FIX/node_modules/typebox/package.json" <<'JSON'
{"name":"typebox","type":"module","exports":"./index.js"}
JSON
cat > "$FIX/node_modules/typebox/index.js" <<'JS'
// Minimal stand-in for the TypeBox builders fm-cloud-tools.ts calls. Object copies
// its options, so the additionalProperties rule the module passes is observable.
export const Type = {
  Object(properties, options = {}) {
    return { type: "object", properties, ...options };
  },
  Union(schemas) {
    return { anyOf: schemas };
  },
  Literal(value) {
    return { const: value };
  },
  String(options = {}) {
    return { type: "string", ...options };
  },
  Optional(schema) {
    return { ...schema, optional: true };
  },
};
JS

JS_FILE="$TMP_ROOT/fm-cloud-tools-checks.mjs"
cat > "$JS_FILE" <<'JS'
import assert from "node:assert/strict";
import { pathToFileURL } from "node:url";

const { registerFmCloudTools } = await import(pathToFileURL(process.env.CLOUD_TOOLS).href);

const providerTools = [];
const busListeners = [];
const pi = {
  registerTool(tool) {
    providerTools.push(tool);
  },
  events: {
    on(name, listener) {
      if (name === "firstmate:native-tools") busListeners.push(listener);
    },
  },
};

// Default deps: governance is unwired and the CLI contract is unpublished, so no
// command can run under production wiring.
registerFmCloudTools(pi);

const providers = providerTools.filter((tool) => tool.name === "fm_cloud");
assert.equal(providers.length, 1, "fm_cloud must register exactly once with the provider");
const [provider] = providers;

const discovered = [];
for (const listener of busListeners) {
  listener({ register: (tool) => discovered.push(tool), allowMessageType() {} });
}
const bus = discovered.filter((tool) => tool.name === "fm_cloud");
assert.equal(bus.length, 1, "fm_cloud must be discoverable once on firstmate:native-tools");
assert.equal(bus[0].description, provider.description, "bus and provider share one description");
assert.equal(bus[0].inputSchema, provider.parameters, "bus and provider share one schema object");
assert.equal(bus[0].execute, provider.execute, "bus and provider share one execute");
console.log("ok - fm_cloud registers once on the provider and the native bus with one schema and execute");

const actions = provider.parameters.anyOf.map((variant) => variant.properties.action.const);
assert.deepEqual(actions, ["doctor", "submit", "status", "logs", "cancel", "fetch", "retry"]);
assert.ok(
  provider.parameters.anyOf.every((variant) => variant.additionalProperties === false),
  "every action variant must refuse fields it does not list",
);
console.log("ok - fm_cloud schema lists seven action variants that each refuse extra fields");

assert.ok(provider.description.includes("without waiting"), "submit must be described as non-waiting");
assert.ok(
  provider.description.includes("pending until the server confirms"),
  "cancel must be described as pending until the server confirms",
);
assert.ok(
  provider.description.includes("disabled until the restart contract is published"),
  "retry must be described as disabled until the restart contract is published",
);
console.log("ok - fm_cloud description states non-waiting submit, pending cancel, and disabled retry");

const ctx = { session: "fixture" };
const blocked = await provider.execute("call-blocked", { action: "doctor" }, undefined, undefined, ctx);
assert.equal(blocked.isError, true);
assert.equal(blocked.details.code, "governance_unavailable");
console.log("ok - fm_cloud refuses through the provider with governance_unavailable under production wiring");

const refused = await bus[0].execute("call-invalid", { action: "doctor", runId: "run-1" }, undefined, undefined, ctx);
assert.equal(refused.isError, true);
assert.equal(refused.details.code, "invalid_request");
console.log("ok - fm_cloud refuses an action-incompatible field through the native bus before any command");

const restart = await bus[0].execute(
  "call-retry",
  { action: "retry", runId: "run-1", requestId: "123e4567-e89b-42d3-a456-426614174000" },
  undefined,
  undefined,
  ctx,
);
assert.equal(restart.isError, true);
assert.equal(restart.details.code, "governance_unavailable", "restart stays behind governance under production wiring");
console.log("ok - fm_cloud retry is refused through the native bus with governance_unavailable under production wiring");
JS

CLOUD_TOOLS="$FIX/.pi/extensions/lib/fm-cloud-tools.ts" node "$JS_FILE" > "$TMP_ROOT/checks.out" 2>&1
status=$?
cat "$TMP_ROOT/checks.out"
[ "$status" -eq 0 ] || fail "fm_cloud registration checks failed (see above)"
