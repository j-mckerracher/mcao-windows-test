# Mutation promotion criteria

This template defines what a named cohort must establish before its mutation results can switch from report-only to blocking.
It sets no numeric floor, threshold, or budget.
Every measured value, floor, threshold, budget number, and cohort name below is pending until it comes from a measured baseline and an approved decision.
A cohort record that fills every field in this template is the only input that can approve a promotion.

## Current state

Mutation testing is report-only.
`bin/fm-mutation-verdict.sh` decides whether changed paths are verified, and the Mutation Verdict check posts success or neutral, never failure.
GitHub, `bin/fm-pr-merge.sh`, and branch protection all treat neutral as non-blocking, so nothing in the current default blocks a merge.
Nothing in this document changes that default.

## Eligibility

- Only a named cohort can switch to blocking.
- A cohort is a named set of surfaces from `bin/fm-mutation-surface-lib.sh`, with a stated repository and a stated stage.
- Only a cohort whose approval record exists can switch to blocking, and that record must cite this document's version.
- Widening a cohort to another surface, another stage, or another repository needs its own authorization and its own cohort record.
- A timeout or an error never enters the killed count, in the baseline or in any later run.

## Preconditions

A cohort is not eligible until every precondition below has a recorded value.
A blank or pending value means the cohort is not eligible.

| Precondition | Recorded value |
|---|---|
| Frozen repeated baseline per surface | pending |
| Repeat count, selected by the approver | pending |
| Measurement window | pending |
| Acceptable variance across repeats | pending |
| Separately authorized stage that enables blocking | pending |
| Owner acceptance, by the named approver | pending |
| Proof compatibility with the proof record | pending |
| Rollback path back to report-only | pending |

## Score definition

The cohort record must define its score from the counts that `bin/fm-mutation-run.sh` publishes: `killed`, `survived`, `timeout`, `error`, `unresolved`, and `excluded`.
Only `killed` counts as killed.
The score formula, its numerator, and its denominator are pending.
Any denominator that removes a mutant may do so only through a signed, still-valid equivalent-mutant claim, and the cohort record must show the raw and the adjusted values side by side.

## Invalid-mutant policy

Pending.
The cohort record must state how a mutant that cannot be judged is recorded.
The policy must keep the runner's rule that a timeout or an error is never reclassified as killed.

## Retry policy

Pending.
The cohort record must state whether a timed-out or errored path may be retried, how many attempts are allowed, and how each attempt is recorded.
A retry must never reclassify an earlier timeout or error.

## Budgets

Pending.
The cohort record must state the per-oracle, whole-run, version-probe, and syntax-check bounds that it uses.
The defaults in `bin/fm-mutation-run.sh` are report-only defaults, not promotion budgets, and no budget value is set here.

## Proof linkage

Pending.
The cohort record must name the proof record that cites the cohort's verdict, and it must show that the cited verdict row is the one the baseline measured.
The linkage mechanism belongs to the proof record work, which this document does not define.

## Rollback

Pending.
The cohort record must name the change that returns the cohort to report-only, and the approver who authorizes that change.
A rollback restores report-only behavior and keeps the recorded baseline.

## Decision record

Blocking is switched on only by a signed decision that cites this document's version, the cohort's filled baseline, and the named approver.
The signature mechanism and the decision record format are pending.

## Limits

- Nothing in this document enables blocking for any surface.
- Nothing in this document sets a numeric floor or a threshold.
- A passing verdict on a changed path is not a promotion; promotion needs an approved cohort record.
- Results that are missing, errored, timed out, or unresolved are never read as verified.
