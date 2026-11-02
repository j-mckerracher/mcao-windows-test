# Panel receipts

This page is the contract for host-side HVE panel policy and receipts.
[`bin/fm-panel-report.sh`](../bin/fm-panel-report.sh) owns the launch policy verdict and the receipt report.
[`bin/fm-panel-resolve.sh`](../bin/fm-panel-resolve.sh) owns seat resolution.
[`bin/fm-capability-schema.sh`](../bin/fm-capability-schema.sh) owns the capability-source schema.
Each script's header and `--help` output carry its command mechanics.

## Status

The capability-source schema is approved under approval record corr f425be7d1020d5f1.
It lives in one module, so a change to any field is a one-edit change.

Seat resolution assigns each requested seat a model or an explicit unresolved reason.
The report consumes the seat records that resolution and execution produce.

## Launch policy

A panel launches only when the granted authority names the panel kind it proposes.

| Panel kind | Granted authority that launches it |
| --- | --- |
| `design-assist` | `design-assist` |
| `delivery-review` | `delivery-review` |

Any other grant, including `none`, is a policy conflict, and no panel launches.
The delivery mode must pass the managed-context contract in [`bin/fm-managed-context-lib.sh`](../bin/fm-managed-context-lib.sh).

## Seat records

A seats file holds one seat per line, with twelve tab-separated fields in this order.

| Field | Meaning |
| --- | --- |
| `seat_id` | Unique id within the panel. |
| `role` | The role the seat fills, used for minimums. |
| `seat_kind` | `candidate` or `judge`. |
| `judges` | For a judge, the one other seat it judges. `-` for a candidate. |
| `requested_model` | The model the seat asked for. |
| `resolved_model` | The model the host assigned, or `unresolved`. |
| `outcome` | `completed`, `failed`, or `missing`. |
| `observed_model` | The model the seat reported, or `unknown`. |
| `observed_family` | The family the seat reported, or `unknown`. |
| `evidence` | The support for the observed identity, or `-`. |
| `reason` | Why the seat is not on its requested model or did not complete: a fallback, an unresolved model, or a failure. `-` when none applies. |
| `output` | The completed output's path, or `-`. |

An empty field is written as `-`, because tab-separated input cannot carry an empty field.

## Seat resolution

A panel request names each seat before it runs, with five tab-separated fields: `seat_id`, `role`, `seat_kind`, `judges`, and `requested_model`.
`bin/fm-panel-resolve.sh resolve <model-capabilities.md> <request.tsv>` prints one seat record per request in the twelve-field format.
Resolution sets the outcome to `missing`, because execution fills the outcome and the observed identity.
A judge that names its own seat, or a seat absent from the request, makes the request invalid, so resolution exits 1 before any seat runs.

A seat is assigned its requested model only when the capability source has an entry for that model with `nested` and `available` both `true`.
Every other seat is `unresolved`, and its `reason` names the cause: an invalid or missing source, no entry for the model, or an unavailable nested capability.
A seat is never assigned a different model, and no seat is dropped.
A root worker model record never proves nested availability, because only a capability-source entry sets `nested` or `available`.

## Identity

Requested, resolved, and observed identity are separate fields and are never merged.
The resolved model is never copied into the observed identity.
An observed family is never inferred from a model or source name.
A known observed family needs evidence, and an unknown observed model must carry an unknown family.
A completed seat whose resolved model is `unresolved` is refused.

## Fallbacks and unresolved seats

A resolved model that differs from the requested model needs a `reason`.
Without one, the seat is refused as a silent fallback.
A seat whose resolved model is `unresolved` needs a `reason` and cannot complete.
A failed seat needs a `reason`.
A completed seat whose resolved model equals its requested model takes no `reason`.

## Coverage

A seat that did not complete is named in a coverage gap with its outcome and any reason.
Completed outputs stay listed beside the gap.
A role may declare a minimum number of completed seats with `--min <role>=<n>`.
A panel below any declared minimum reports `incomplete`, not `complete`.
A failed seat makes the panel incomplete, and the script still exits 0, because an incomplete panel is a reported status and not a script failure.

## Judges

A judge is a seat separate from the seats it judges.
A judge in the same family as the seat it judges is reported as reduced diversity.
A judge whose identity is unknown is reported as not proven separate.
A different family is preferred for judges.

## Diversity claims

The report makes only the claim its completed seats support.

- No completed seats: none.
- A completed seat with unknown identity: unknown, with no diversity claim.
- One model across several contexts: independent contexts, with no cross-family claim.
- Several families, each with evidence: cross-family.
- One family across several models or seats: reduced diversity.

A single-family panel is reported as reduced diversity, not as failed.

## Capability source

The capability source is a `model-capabilities.md` file whose front matter holds a `models:` list.
Each entry has these fields.

| Field | Required | Meaning |
| --- | --- | --- |
| `model_id` | yes | The requested or catalogued model id, one token. |
| `family` | yes | The model family, or `unknown`. Never inferred from the name. |
| `nested` | yes | Whether the capability is exposed to nested workflow-agent seats. |
| `available` | yes | `true` only when the nested capability is exposed. |
| `identity_source` | yes | `catalog`, `observed`, or `unknown`. |
| `provider` | no | The provider, omitted when unknown. |

A missing file, an invalid file, or a model with no entry is an explicit `unresolved` result for that seat.
The file is read as a whole, so one invalid entry resolves no entry.
Nothing is guessed from a model or source name.

## Worked example

This example is a synthetic illustration only. It is not AC8 or T9 evidence.
A reviewer traces one panel from configuration to synthesis.
The configuration requests two writer candidates and one judge of the first candidate.

| Stage | `c1` candidate | `c2` candidate | `j1` judge of `c1` |
| --- | --- | --- | --- |
| Request | `gpt-a` | `gpt-root` | `claude-j` |
| Resolution | assigned `gpt-a` | unresolved: nested capability unavailable | assigned `claude-j` |
| Execution | completed, observed `gpt-a`, family `openai`, evidence cited | missing, no run | completed, observed `claude-j`, family `anthropic`, evidence cited |
| Synthesis | completed candidate | missing candidate, named as a coverage gap | completed judge, separate family |

The synthesis for that panel reads as follows, without its per-seat `receipt:` lines.

```text
panel-status: incomplete
seats: candidate=2 judge=1 completed=2 missing=1
coverage-gap: seat c2 (role writer) outcome missing; reason: nested capability unavailable for model gpt-root: nested=false available=false
diversity: cross-family across 2 families; executed models: c1=gpt-a/openai (evidence run-log-c1) j1=claude-j/anthropic (evidence run-log-j1)
judge-separation: judge j1 family anthropic differs from judged seat c1 family openai
completed-output: c1 out/c1.md
completed-output: j1 out/j1.md
```

The unresolved seat is never replaced by another model, so the panel does not claim it completed.
The completed outputs stay listed beside the gap.

## Verification

The behavior tests are `tests/fm-panel-report.test.sh`, `tests/fm-panel-resolve.test.sh`, and `tests/fm-capability-schema.test.sh`.
All three run in the standalone lane of `bin/fm-test-run.sh`.
