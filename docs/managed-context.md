# Managed context

The managed context is the block a ship launch carries so the hve workflow agent knows the assignment Motif routes by.
[`bin/fm-managed-context-lib.sh`](../bin/fm-managed-context-lib.sh) is the single owner of the field list, the closed value sets, and the refusal wording; every script that reads or writes the block sources it.
This page describes the contract those scripts enforce.

## Block

A ship brief carries one fenced `managed-context` block inside its `# Managed context` section.
`bin/fm-brief.sh` scaffolds the block for a ship, and `bin/fm-promote.sh` writes it into a promoted scout's brief.
Each line of the block is one `field: value` pair, and each field appears exactly once.

| Field | Value |
| --- | --- |
| `ownership` | `motif`, the only supported owner. |
| `task_id` | The task this spawn launches. Any other id is a contradiction and is refused. |
| `worktree` | The placeholder `{WORKTREE}`. The spawn binds the worktree it allocates; a literal path is refused because it contradicts that allocation. |
| `delivery_mode` | `no-mistakes` or `direct-PR`, matching the spawn's `--mode`. |
| `helper_allowance` | A comma-separated list drawn from `read-only-checks` and `isolated-scratch`, with no repeats and no spaces. |

The helper allowance grants read-only checks and isolated scratch output only.
The host records the allowance and does not run helpers on its behalf.
A second real checkout is reported to Motif and is never run.

## Launch

A ship spawn validates the block before anything exists: no endpoint, no worktree, no task record, and no composed launch brief.
After the spawn allocates the worktree, it binds that path into the launch brief `data/<id>/launch-brief.md`; the source brief keeps the placeholder.
The task record `state/<id>.meta` then carries `managed_ownership`, `managed_task_id`, `managed_worktree`, `managed_delivery_mode`, and `managed_helper_allowance`.

A ship whose block is missing, duplicated, malformed, or contradictory is refused, and the refusal names the block or the field at fault.
No standalone workflow-agent launch stands in for a refused block.

## Modes and merge posture

A ship spawn accepts only `no-mistakes` and `direct-PR`.
`local-only`, `no-mistakes-prod-only`, and any other value are refused by name.
`--yolo on` is refused for a ship spawn, so every ship spawn takes `--yolo off`.
Relaunch and promotion run the same shared validation as a new spawn, so they refuse retired modes and briefs that carry no managed-context block.
Whether a relaunch exemption for retired modes is added is a separate, later decision.

## Supported harnesses

A ship or scout runs only on `claude`, `codex`, `pi`, `pi-signed`, `omp`, or `copilot`.
[`bin/fm-workflow-agent-lib.sh`](../bin/fm-workflow-agent-lib.sh) declares that list and owns the workflow-agent check each harness runs.

## Workflow-agent version range

`bin/fm-workflow-agent-lib.sh` declares the supported `hve@mayo-skills` range in `FM_WORKFLOW_AGENT_HVE_MIN` and `FM_WORKFLOW_AGENT_HVE_BEFORE`, currently `>=0.2.0 <0.3.0`.
Each harness supplies the installed version from its own record: claude from `installed_plugins.json`, codex and copilot from their plugin manifests, and pi and omp from the record their copied agent came from (claude first, then codex).
An installed version outside the range refuses the launch and names the version and the range.
A version that cannot be read refuses the launch and names the range.
The range applies to scouts as well as ships.
Secondmate launches carry no managed context and do not check the range.
Moving the range is a deliberate edit to the host's declaration, not a runtime override.

## Standalone scouts

A scout carries no managed-context block and records no managed fields.
Its launch is otherwise unchanged, subject to the version range above.

## Promotion

`bin/fm-promote.sh <task-id> --mode <no-mistakes|direct-PR> --yolo <on|off> --assignment-profile <record.json>` promotes a scout to a ship.
Promotion refuses a missing mode, merge posture, or assignment-profile record before it writes anything, and the record's contract is owned by [`assignment-profile-policy.md`](assignment-profile-policy.md).
On success it rewrites the scout's brief with the managed block, and it writes the managed fields into the task record together with the scout's worktree.

## Scope

This contract does not decide review panels, verification readiness, retained delivery policy, or merge guards; those belong to their own owners.
The assignment profile decision in [`assignment-profile-policy.md`](assignment-profile-policy.md) is a separate record beside this block and never adds fields to it.
