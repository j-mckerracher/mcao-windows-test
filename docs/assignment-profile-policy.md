# Assignment profile policy

This document is the shared assignment-profile policy and decision contract for Motif-managed work.
It defines the four profiles, their eligibility, exclusions, and required evidence, the precedence rules that choose among them, the trusted policy revision, the serialized decision record, and the reviewed fixtures that exercise those rules.
The producer side enforces it at launch: `bin/fm-assignment-profile.py` validates the record, and `bin/fm-assignment-profile-lib.sh` binds it to the launch brief and the task record.
Nothing here claims that an adapter, a version pair, or the shared fixture matrix is verified.
The machine-readable shape of the decision record is [`assignment-profile-decision.v1.schema.json`](assignment-profile-decision.v1.schema.json).

## Ownership

Each part of this contract has one owner, and the other tickets consume it rather than restate it.

| Ticket | Owns |
| --- | --- |
| DPES-762 | This document, the decision schema, eligibility and precedence, the trusted revision rules, unresolved and compatibility rules, and the fixtures |
| DPES-763 | Producer integration: brief rendering, spawn agreement checks, launch-brief binding, persistence, relaunch, and pre-endpoint validation |
| DPES-764 | Canonical HVE consumer: validation before playbook selection, the profile-to-stage mapping, and standalone separation |
| DPES-765 | Full actual-change collection, reassessment, evidence invalidation, the change identity algorithm, and the completion boundary |
| DPES-766 | Zero-test proof representation accepted by the external pipeline |
| DPES-769 | Shared behavioral matrix that executes these fixtures against runtime code |

The five-field managed-context block stays owned by [`managed-context.md`](managed-context.md).
The profile decision travels beside that block as its own record and never adds fields to it.

## Current constraints

Local verification, HVE playbooks, DoD instructions, and CI each make their own proportional decisions today.
This contract defines the shared record they will consume and does not change those decisions in this change.
Prose classification is owned by [`documentation-audiences.json`](documentation-audiences.json), whose tracked patterns are `*.md`, `*.mdx`, `*.rst`, `*.txt`, and `docs/examples/*`.
`AGENTS.md` and `CLAUDE.md` are classified `agent-runtime`, so they are behavioral.
`bin/fm-test-run.sh --changed` over-selects related families rather than under-selecting, and this contract does not change that selection.
The mutation classifier refuses a changed file that matches no surface class, so an unknown path cannot reach a documentation-only verdict in that pipeline, and a documentation-only pull request reports neutral there because no measured shell script changed ([`CONTRIBUTING.md`](../CONTRIBUTING.md#mutation-testing)).

## Vocabulary

- Contract version: `1`, the only supported version.
- Profile ids: `documentation-only`, `small-low-risk`, `standard`, and `high-risk`.
- Decision stages: `provisional` and `final`.
- Classification statuses: `resolved` and `unresolved`.
- Valid uncertainty is stage `provisional`, status `unresolved`, and effective profile `high-risk`.
- Completion requires stage `final`, status `resolved`, and every obligation satisfied.
- Policy revision: a host-selected identifier, distinct from the contract version.
- Unknown ids and unsupported versions are rejected and never coerced to a nearby value.

## Trusted policy source

- The host selects one immutable policy revision at launch and records its identifier in the decision.
- The revision registry is [`assignment-profile-revisions.json`](assignment-profile-revisions.json).
  It lists each revision with its approval status and the SHA-256 of every policy file that revision covers, and its `current` field names the revision new launches pin.
- A launch must pin the registry's `current` revision, and a relaunch keeps the revision pinned at the original launch while that revision stays approved and every covered file still matches its recorded hash.
- The provenance of a revision is repository history plus the captain's merge approval, and this contract makes no cryptographic authenticity claim.
- An unavailable, unapproved, or changed revision blocks managed routing, reports the reason, and creates no replacement profile.
- The assignment's worktree copy of any policy material is untrusted input and is never read to choose, weaken, or waive an obligation.
- A change to the policy material itself (this document, the schema, or the registry) is a change to proof-gate authority and selects `high-risk` under the high-risk triggers.
- A new revision takes effect only through an explicit host-approved transition that adds a registry entry, records the old and new identifiers, and preserves or increases every obligation.

## Profiles

Each profile lists its eligibility, exclusions, and required evidence.
Known work that fits none of the narrower profiles defaults to `standard`, and file or line counts never select a profile.

### documentation-only

Eligibility requires every condition below:

- every changed path is a prose path under the path-class rules below;
- every changed line is verified non-behavioral content, and the reasons record that verification;
- no high-risk trigger applies;
- the change set is fully collected and every path is classified.

Exclusions:

- behavioral Markdown, including `agent-runtime` entries such as `AGENTS.md` and `CLAUDE.md`, agent skill prompts, and operational or supervision instructions;
- configuration in any format, including TOML, YAML, and JSON;
- executable examples, including copyable examples under `docs/examples/*` and any changed fenced command or code block;
- code and generated code;
- unknown or unclassified paths.

A change that also needs a test is not documentation-only.

Required evidence:

- zero tests executed, recorded as zero;
- applicable non-test checks recorded separately, for example `bin/fm-doc-audience-check.sh` in this repository;
- focused review of the content and diff;
- a final decision that names the evaluated change identity.

### small-low-risk

Eligibility requires every condition below:

- a single bounded behavior change with understood impact;
- no security or authorization change;
- no sensitive-data handling change;
- no destructive operation;
- no merge or delivery authority change;
- no dispatch, lifecycle, worktree, or proof-gate authority change;
- no public-contract incompatibility;
- no data migration;
- no unresolved cross-component impact;
- relevant focused verification identified.

Exclusions: any unmet condition, and any unknown or incomplete evidence.
Required evidence: a short plan, focused review, relevant verification with tests required, and change reassessment.

### standard

Eligibility: normal implementation with relevant design, review, and verification.
Exclusions: any high-risk trigger, and any unresolved uncertainty, which yields provisional `high-risk` instead.
Required evidence: design appropriate to the affected boundaries, focused review, and relevant verification with tests required.

### high-risk

Eligibility: any high-risk trigger applies regardless of size, or uncertainty is unresolved.
Any one of these triggers selects `high-risk`:

- security, authorization, secrets, or sensitive data;
- destructive operations or data migrations;
- dispatch, lifecycle, worktree, delivery, merge, or proof-gate authority;
- breaking public contracts;
- cross-component changes affecting shared state, concurrency, or recovery;
- broad changes that need coordinated design and verification.

Exclusions: none, because risk overrides size and a one-line authority change qualifies.
Required evidence: ground scope and risk, explicit design alternatives, adversarial design review, implementation, independent change review satisfied through the selected delivery path, risk-focused verification, and tests required.

## Precedence

Apply these rules in order and stop at the first one that decides.

1. Invalid, missing, incompatible, or contradictory inputs block routing, and no profile is created.
2. Any high-risk trigger selects `high-risk`.
3. Unknown paths, incomplete change collection, a missing or unresolvable comparison base, or unresolved risk select provisional `high-risk` with status `unresolved`.
4. `documentation-only` applies only when every eligibility condition holds.
5. `small-low-risk` applies only when every eligibility condition holds.
6. Everything else selects `standard`.

## Path classes

- A prose path is a `*.md`, `*.mdx`, `*.rst`, or `*.txt` path whose entry in [`documentation-audiences.json`](documentation-audiences.json) is classified `public-product`, `operator-current`, `maintainer-architecture`, or `maintainer-verification`.
- An entry classified `agent-runtime` is behavioral and never prose.
- Copyable examples under `docs/examples/*` are executable-example material and never prose for this purpose.
- Any other path, including a file with no inventory entry, an extensionless entry point, and a JSON, TOML, YAML, or script file, is unknown or non-prose and cannot satisfy `documentation-only`.
- An audience classification alone does not prove non-behavioral content, so the changed lines still decide.

## Provisional, final, and unresolved decisions

- A decision made before the full change set is collected is provisional.
- A final decision is made on the evaluated change set and names its change identity.
- An unresolved classification is valid only as provisional `high-risk`, and it blocks completion until evidence resolves it.
- Completion requires a final, resolved decision whose evidence covers the evaluated change identity and whose obligations all hold.
- Reassessment covers the full actual change set, not the initial intent.
- Reassessment preserves or increases obligations, and no automatic downgrade occurs.
- Newly required stages route through the stage mapping, and earlier insufficient evidence never satisfies a new obligation.
- A smaller final diff never reduces requirements.
- Docs-to-code loses documentation-only eligibility, and docs-to-high-risk adds the newly applicable design and review stages rather than only more tests.
- A superseding decision names the prior decision and its escalation reasons.
- Version 1 defines no path that lowers a profile, so any lowering needs a separately approved decision (open item 4).

## Change set and comparison base

- The host pins the target branch and a resolved comparison-base commit at launch and records both in the decision.
- The worker cannot choose a narrower base.
- Collection covers committed changes from base to HEAD, tracked working-tree changes, untracked files, renames, and deletions.
- The record states whether collection was complete and which categories it covered in `change_coverage`.
- A missing or no longer resolvable base, or incomplete collection, blocks completion and holds the decision at provisional `high-risk`.
- A rebase, or a target or base change, invalidates the affected classification, review, and proof until they are revalidated.
- The change identity algorithm is owned by the header of `bin/fm-assignment-reassess.py`.
- This contract requires only that the record carry the identity.

## Stage mapping

Stage order is `ground-scope`, `short-plan`, `design`, `design-critique`, `implement`, `focused-review`, `verify`, `independent-change-review`, `risk-verify`, `change-reassessment`, and `delivery-handoff`.
The `required_stages` value in each record must equal the set for its profile and list the stages in this order.

| Profile | Required stages |
| --- | --- |
| documentation-only | ground-scope, implement, focused-review, verify, change-reassessment, delivery-handoff |
| small-low-risk | ground-scope, short-plan, implement, focused-review, verify, change-reassessment, delivery-handoff |
| standard | ground-scope, design, implement, focused-review, verify, change-reassessment, delivery-handoff |
| high-risk | ground-scope, design, design-critique, implement, independent-change-review, risk-verify, change-reassessment, delivery-handoff |

- Every profile includes change reassessment and the selected host delivery handoff.
- For `documentation-only`, `verify` runs applicable non-test checks only.
- Helpers are optional unless an approved stage requires independence, and missing required independence blocks completion and never relaxes permission controls.

## Review and delivery boundary

- The selected delivery path supplies required change review, and this contract adds no parallel HVE delivery-review chain.
- Qualifying review evidence from the selected delivery path is reused.
- Required independent change review is performed by a reviewer distinct from the author, with fresh context, read-only access to the full relevant diff, findings tied to the reviewed change identity, and no publish, merge, or release-gate authority.
- A different model or provider is encouraged but not required.
- A material change invalidates the affected review evidence.
- When direct-PR cannot satisfy the obligations, the worker stops and asks the host whether to switch to no-mistakes, and the worker never switches modes itself.
- Bounded design critique is permitted assistance and is not a release gate.
- This contract does not change delivery mode, merge authority, or the `yolo` posture.

## Decision record

### Serialization

The serialization below is a proposed choice for review (see the proposed-choices list).

- The record is a JSON object encoded as UTF-8 under RFC 8259, with one object per decision.
- Unknown keys and duplicate keys are rejected.
- The record travels in the bound launch brief as one fenced block whose info string is `assignment-profile`.
- That block is separate from the `managed-context` block.

### Fields

The fields `assignment_intent`, `expected_surfaces`, `risk_triggers`, and `change_coverage` come from the DPES-762 scope, not from the approved information list, and they are proposed for review.

| Field | Type | Rule |
| --- | --- | --- |
| `contract_version` | integer | Must be `1`. |
| `decision_id` | string | `<task_id>/<sequence>`, where the sequence starts at 1 and increments with each supersession. |
| `task_id` | string | Must equal the managed-context `task_id`. |
| `assignment_intent` | string | The host-supplied summary of the ask the decision classifies. |
| `expected_surfaces` | array of strings | Paths or path classes expected before reassessment; no duplicates. |
| `pinned_policy_revision` | string | The host-selected revision identifier. |
| `profile` | enum | One of the four profile ids. |
| `decision_stage` | enum | `provisional` or `final`. |
| `classification_status` | enum | `resolved` or `unresolved`. |
| `risk_triggers` | array of enum | Triggers from the high-risk list above that apply, using the names `security-authorization-secrets-or-sensitive-data`, `destructive-operation-or-data-migration`, `dispatch-lifecycle-worktree-delivery-merge-or-proof-gate-authority`, `breaking-public-contract`, `cross-component-shared-state-concurrency-or-recovery`, and `broad-change-needing-coordinated-design-and-verification`; any entry requires profile `high-risk`. |
| `change_coverage` | object | `complete` boolean and `categories`, a list drawn from `committed`, `tracked-working-tree`, `untracked`, `renamed`, and `deleted`. |
| `reasons` | array of strings | At least one reason. |
| `required_stages` | array of enum | Equals the stage set for `profile`, in stage order, with no repeats. |
| `evidence_obligations` | object | `tests_required` and `independent_change_review_required` booleans. |
| `target_branch` | string | Must equal the host-pinned branch. |
| `comparison_base_commit` | string | 40 lowercase hexadecimal characters, equal to the host-pinned commit. |
| `evaluated_change_identity` | string | Required when `decision_stage` is `final`; its algorithm belongs to DPES-765. |
| `supersedes` | object | Present when superseding; holds `decision_id` and a non-empty `escalation_reasons` array. |

### Validation

- The host checks syntax, exactly one block, schema conformance, a supported contract version, an available pinned revision, the `task_id` match, and the pinned branch and base before endpoint creation (DPES-763).
- The canonical HVE consumer checks that obligations equal the pinned revision's mapping for the profile, that no obligation is lighter, that the stage and status rules hold, and that the record agrees with the managed context before playbook selection (DPES-764).
- Completion checks that the decision is final and resolved, that its obligations hold, that change coverage is complete, and that its evidence covers the evaluated change identity (DPES-765).
- The schema enforces these rules: every required field is present, the exact `required_stages` list for each profile, `tests_required` false for `documentation-only`, `independent_change_review_required` true only for `high-risk`, a final decision carries a change identity, an unresolved decision is provisional and `high-risk`, any risk trigger requires `high-risk` and comes from the six P-03 names, incomplete change coverage is unresolved, complete change coverage lists all five categories, and unknown top-level keys are rejected.
- The schema does not enforce `tests_required` for the other profiles, because that derivation is open item 3.
- The schema does not enforce completion, which DPES-765 owns.

### Producer integration

- A ship brief carries its record as the one `assignment-profile` block beside its `managed-context` block, and a ship launch without a valid record does not start.
- Before a worktree or endpoint is bound, the launch checks the record's syntax, schema, approved revision, and `task_id`, and a new launch also requires the registry's current revision.
- After the base is resolved and before binding, the launch checks the record's target branch and comparison-base commit against the values the host pinned.
- A launch persists the canonical record with its SHA-256 and the decision keys that its task record carries, and the script headers own those names and paths.
- A relaunch restores the persisted record and refuses when its hash no longer matches, when the brief carries a different record, or when the pinned revision is no longer approved.
- The producer scripts own the exact commands and refusal wording, and `docs/managed-context.md` keeps its five-field block unchanged.

## Compatibility

- Contract version `1` is the only supported version.
- The supported Motif and HVE version pair and the list of supported contract versions stay unset until implementation verifies them.
- An unsupported combination reports the required and supported versions and blocks routing.
- A legacy ship that lacks the required context stays blocked until an explicit host-approved migration.
- No version is inferred automatically, and no standalone fallback is taken.

## Fixtures

Each fixture is evaluated against the rules above, and its expected outcome is derived from them.
These are reviewed policy examples, and DPES-769 executes them against runtime code later.
Rows D-01 through D-09 cover the acceptance criteria for prose-only, one-line authorization, mixed, unknown, and incomplete-evidence inputs.
Rows R-01 through R-14 are contradictory or invalid inputs that must be rejected or blocked.
Rows marked "schema" were also checked against the schema with a scratch Draft 2020-12 run, and rows marked "reassessment", "host", or "policy" are checked by the owner named in the Validation section.
`tests/fm-assignment-profile.test.sh` runs the schema, host, and policy rows that the producer decides, and DPES-769 owns the shared matrix that runs the rest.

| ID | Input | Expected outcome | Reasons |
| --- | --- | --- | --- |
| D-01 | Ten classified prose Markdown files changed, with no executable block and no instruction surface touched | `documentation-only`, `final` once collected, `resolved`, `tests_required` false, `risk_triggers` empty, coverage complete | Every path is prose and every changed line is non-behavioral; AC2 |
| D-02 | One-line change to an authorization check that alters who may approve a merge | `high-risk`, `final` once collected, `resolved`, independent review required, triggers recorded | Merge approval is an authority change and the one-line size does not matter; AC2 |
| D-03 | Mixed change: prose edits plus one bounded behavior change in a shell script | `small-low-risk`, `final` once collected, `resolved`, tests required | Mixed change is not documentation-only; the behavior change meets every small condition |
| D-04 | Edit to an agent skill instruction that changes no dispatch, lifecycle, delivery, or proof authority and contains no executable example | `small-low-risk`, `final` once collected, `resolved`, tests required | Agent instructions are behavioral Markdown and are excluded from documentation-only |
| D-05 | Prose change plus a changed fenced shell command in a maintainer document | `standard`, `final` once collected, `resolved`, tests required | A changed executable example is excluded from documentation-only, and with no bounded behavior change the work defaults to `standard` |
| D-06 | Change to a path that matches no path class, such as an extensionless `bin/` entry point | Provisional `high-risk`, `unresolved`, completion blocked | Unknown path and uncertain scope; AC3 |
| D-07 | Prose-only change, but untracked files were not collected (`change_coverage.complete` false) | Provisional `high-risk`, `unresolved`, completion blocked | Incomplete collection; no documentation-only completion; AC3 |
| D-08 | Prose-only change whose pinned comparison base no longer resolves at reassessment (for example after history was rewritten) | Provisional `high-risk`, `unresolved`, completion blocked | Unresolvable base blocks completion; AC3 |
| D-09 | Two independent behavior changes in one module, with no authority effect and no migration | `standard`, `final` once collected, `resolved`, tests required | Not a single bounded behavior change and no high-risk trigger applies |
| R-01 | Record with `profile` set to `docs-only` (schema) | Rejected | Unknown profile id; no coercion |
| R-02 | Record with `contract_version` set to `2` (schema) | Rejected | Unsupported version; the required and supported versions are reported |
| R-03 | `documentation-only` record whose change set includes a code path (reassessment) | Rejected as documentation-only; reassessment selects a non-documentation profile | Classification contradicts the change |
| R-04 | `final` record with `classification_status` set to `unresolved` (schema) | Rejected | Valid uncertainty is provisional only |
| R-05 | Record whose `target_branch` differs from the host-pinned branch (host) | Rejected before endpoint creation | Inconsistent with the pinned launch input |
| R-06 | Record missing `comparison_base_commit` (schema) | Rejected | A missing required item rejects the record |
| R-07 | Assignment edits the pinned policy material in its own worktree to lower an obligation (policy) | Worktree copy ignored, obligations stay at the pinned revision, and the edit is a proof-gate authority change that selects `high-risk` | The trusted revision comes from the host, and an assignment cannot weaken its own obligations |
| R-08 | Superseding record that lowers `standard` to `small-low-risk` (policy) | Rejected | Version 1 has no lowering path (open item 4) |
| R-09 | Pinned revision absent or incompatible (host) | Routing blocked; no profile created | Trusted revision unavailable; the reason is reported |
| R-10 | `final` record without `evaluated_change_identity` (schema) | Rejected | Final evidence must cover an evaluated change identity |
| R-11 | `documentation-only` record with `tests_required` true, or with independent review required (schema) | Rejected | Documentation-only requires zero tests and no independent change review |
| R-12 | Record with a non-empty `risk_triggers` and any profile other than `high-risk` (schema) | Rejected | Any high-risk trigger selects `high-risk` |
| R-13 | `unresolved` record on a profile other than provisional `high-risk` (schema) | Rejected | Valid uncertainty is provisional `high-risk` only |
| R-14 | Record with an unknown top-level key, such as an added authority field (schema) | Rejected | Unknown keys are rejected |

## Open items

These points are not decided by this contract, and each needs a named owner or approval before it is relied on.

1. The production path-class table for each project needs approval before any revision enables `documentation-only` there (owner: decision owner).
2. P-08 lists independent change review and risk-focused verification for `high-risk` without a focused-review or `verify` stage; this document transcribes that list literally, and the decision owner should confirm it.
3. The rule that non-documentation profiles require `tests_required` to be true is derived from P-02, P-08, and P-11 and needs confirmation, and the schema does not enforce it yet.
4. The rule that version 1 has no profile-lowering path needs confirmation.
5. Treating a change to the policy material as proof-gate authority, which selects `high-risk`, is a reading of P-03 that needs confirmation.
6. Machine-readable reason codes are not defined, so reasons are free text in version 1.
7. The supported Motif and HVE version pair and the rollout order stay unset until implementation verifies them.
8. The evidence schema and the zero-test proof representation (DPES-766) remain open; the change identity algorithm is implemented in `bin/fm-assignment-reassess.py`.
9. The storage layout of the revision registry and the provenance and integrity mechanism for revisions remain open for DPES-763 and DPES-764.

## Proposed choices for review

These choices shape the record but were not among the approved policy decisions, so the reviewer should confirm each one.

- JSON as the serialization, with the `assignment-profile` fenced block as the carrier.
- Rejection of unknown and duplicate keys.
- The `<task_id>/<sequence>` decision identifier format.
- Free-text reasons and free-text `expected_surfaces` in version 1.
- The `assignment_intent`, `expected_surfaces`, `risk_triggers`, and `change_coverage` fields and their vocabularies.
- The short slug names for the high-risk triggers.
- The `tests_required` and `independent_change_review_required` derivations.
- The stage identifiers and their canonical order.
- The `supersedes` object shape.
