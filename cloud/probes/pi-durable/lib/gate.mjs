// Live-environment prerequisite gate for the probe (DPES-868 AC2 / T2).
//
// Reads only local facts: platform, bubblewrap on PATH, and operator-supplied identifiers.
// It makes no network or cloud calls, because this task forbids cloud calls. Anything
// missing is reported as a blocked prerequisite; nothing falls back to another provider
// or to an unsandboxed run.

import { accessSync, constants } from "node:fs";
import { delimiter, join } from "node:path";

export const LIVE_CHECK_IDS = ["ac2-live-vertex-tool-result", "ac2-live-outside-write-denied"];

// Gap that stays true for this change even when every operator input is present.
const RUNNER_GAP = "live Vertex and bubblewrap runner is not implemented in this change; it needs the approved image digest and pinned Vertex model from DPES-811 and DPES-813 before it can be wired";

function onPath(binary, env) {
	for (const dir of (env.PATH ?? "").split(delimiter)) {
		if (!dir) continue;
		try {
			accessSync(join(dir, binary), constants.X_OK);
			return true;
		} catch {
			// keep searching
		}
	}
	return false;
}

// Returns { ready, missing[] }. `ready` is always false in this change because of RUNNER_GAP.
export function liveGate({ platform = process.platform, env = process.env } = {}) {
	const missing = [];
	if (platform !== "linux") {
		missing.push(`Linux host with bubblewrap required; this host reports platform "${platform}"`);
	} else if (!onPath("bwrap", env)) {
		missing.push("bubblewrap (bwrap) binary not found on PATH");
	}
	if (!env.PROBE_IMAGE_DIGEST) {
		missing.push("approved Cloud Run worker image digest (DPES-811) not supplied as PROBE_IMAGE_DIGEST");
	}
	if (!env.PROBE_VERTEX_MODEL) {
		missing.push("approved Vertex model ID (DPES-813) not supplied as PROBE_VERTEX_MODEL");
	}
	if (!env.PROBE_VERTEX_LOCATION) {
		missing.push("Vertex location not supplied as PROBE_VERTEX_LOCATION");
	}
	missing.push("Vertex metadata-server authentication and Cloud Run IAM (DPES-804) unverified; not checked because cloud calls are forbidden for this task");
	missing.push(RUNNER_GAP);
	return { ready: false, missing };
}
