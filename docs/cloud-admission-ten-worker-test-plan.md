# DPES-926 ten-simultaneous-admission test plan

Status: **plan only.** This document authorizes nothing. No Terraform apply, live worker launch, cloud write, secret access, or workflow dispatch was run to produce it. Read-only prerequisite results are recorded in section 3 and its verification log. Execution needs a separate, explicit go-ahead from the project owner after every blocker in section 3 is closed. The project owner has confirmed secret version 9 of `mcao-cloud-policy-d` as the version in force (Jira DPES-926 comment 76289, 2026-11-01). That confirmation closes only the version-in-force question (P2a). No agent reads a secret value. Jira DPES-926 comment 76289 item (1) still requires verifying what version 9 contains before any live test, and does not waive that; the develop-file comparison (P3) does not do it, so the verification remains required (section 9).

Parent: DPES-751. Work item: DPES-926. Upstream change: CAF pull request 580641 in `caf-dpe-platform-app-resources` (Azure DevOps, `dev.azure.com/mclm`, project `DPE_POC`), which edits `policies/mcao-cloud-develop.json` and targets `develop`.

## 1. Purpose and acceptance mapping

The deployed admission policy allowed one run to execute and one to queue (`team_worker_slots`, `owner_worker_slots`, `team_queue_depth`, `owner_queue_depth` all 1). DPES-926 raises those four values to 10 and proves, with live evidence, that ten runs can hold worker slots at once.

| Criterion (DPES-926) | Status in this plan |
| --- | --- |
| AC1: the four admission values are 10 and nothing else in the policy changes | Upstream PR 580641 is completed. Its description says the four values change from 1 to 10 and that `admission.version` stays 2 (under "Nothing else changes" for the policy file). The same PR also edits `docs/mcao-cloud-route-activation.md` (limits sentence) and `CHANGELOG.md`. Source-file check (P3, read-only): the policy file on develop has the four values at 10 and `admission.version` 2, and the PR changed no other policy field. Secret version 9 is the version in force (owner decision, Jira DPES-926 comment 76289, 2026-11-01); version 8 evidence is superseded (P2). No agent has read version 9's contents. AC1 is shown at source-file level only. The effective limits are NOT confirmed by any available read-only means: `GET /internal/v1/admissions` reports only `state`, `policy_version` and `holding_runs`, and no agent reads the secret. How to confirm them before a window is an open owner item (section 9). |
| AC2: Terraform tests, validate and repo policy checks pass, with the plan reported | Upstream PR body reports `terraform fmt -check`, `init -backend=false` and `validate` passing, and `terraform test` passing for six listed suites. The same run reports `grafana_cloud.tftest.hcl` **failing**; the PR says it fails the same way on base `80f5c8c`. That failure is not a pass and was not re-run here. The PR body states **"Terraform plan NOT run"**. Jira records a plan result (2 add, 9 change, 0 replace, 2 destroy) that the PR body does not contain. Unreconciled; see section 3a. Nothing was re-run here. The develop policy file is refused by the repo loader (P3); that is a source-file result, not a test run. AC2 is not shown as met. |
| AC3: the PR states admissions and launch stay closed on the normal controller, and the owner applies | Verified: the upstream PR description states it. Deployed `MAO_CLOUD_ADMISSIONS` reads `closed` (P4). |
| AC4: after the owner applies, one probe confirms ten simultaneous runs are admitted | **Not claimed.** Configuration alone does not satisfy AC4. Live execution evidence is a tracked dependency defined in section 6. |

## 2. Repository facts (checked at origin/DPES-751 `79d38415`; still true at origin/DPES-751 `25dbf8fe`, the head merged on this branch, since `cloud/` and `policies/` are unchanged between them)

- **Admission fields.** The `admission` block has seven required positive integers: `version`, `team_queue_depth`, `owner_queue_depth`, `team_worker_slots`, `owner_worker_slots`, `team_daily_worker_seconds`, `queued_deadline_seconds`. Source: `cloud/docs/runbook-cost-retention.md`, "Admission limits"; `cloud/src/mao_cloud/config.py`.
- **Policy load.** The controller reads the policy once at process start (`--policy` or `MAO_CLOUD_POLICY_FILE`). A limit change needs a broker restart. There is no reload.
- **Start state.** `MAO_CLOUD_ADMISSIONS` is required (`open` or `closed`) and has no default. The switch has no durable source (gap 55), so a restart can reopen admissions. Status must be checked after any restart.
- **Drain and status.** `POST /internal/v1/admissions:drain`, `POST /internal/v1/admissions:resume`, and `GET /internal/v1/admissions` (fields `state`, `policy_version`, `holding_runs`). Operator credential required. Source: `cloud/docs/runbook-cost-retention.md`.
- **Bulk cancel.** `POST /internal/v1/runs:bulk-cancel`, 1 to 50 explicit run ids, operator credential. Launched runs become `cancel_requested` and keep their worker slot and daily reservation until termination is confirmed.
- **Launch gate closed in production (repo code).** `CloudRunJobLauncher` defaults `intake_ready=False`, so its launch and stop paths are refused. `BOOTSTRAP_INTAKE_BLOCKER` in `cloud/src/mao_cloud/cloud_run_jobs.py` states that `jobs.run` is not called in production. Source: `cloud/docs/protocol-v1.md` gap 72.
- **Held capacity.** A run whose stop is never confirmed keeps its worker slot and daily reservation indefinitely. A nameless attempt cannot be observed, and nothing in this build releases it (gaps 35, 41). Live stop confirmation is DPES-807.
- **Repository doc on the control API.** `cloud/docs/deployed-smoke.md` says the control API is not deployed in `dpe-platform-d` and pilot identities are not provisioned (DPES-896). Section 3 shows a deployed control service now exists, so that statement is stale for the deployment; the identity part is unverified.
- **Profiles.** `STANDARD` (4 vCPU, 16 GiB) holds the declared disks (workspace 8 GiB, tmp 1 GiB). `SMALL` (2 vCPU, 8 GiB) does not, and is 1 GiB short (gap 32). `cloud/docs/protocol-v1.md` section 19.4.
- **Protocol timings.** Queued deadline 1800 s. Startup timeout 600 s by default. Runtime budget 60 s to 7200 s. `MAX_ATTEMPTS` 3. Source: `cloud/docs/protocol-v1.md` (run-limits table).
- **Retention and deletion.** Retention is not active. Object deletion is not implemented (gap 51). Do not delete run objects by hand.
- **Reservation audit.** Not implemented (gap 50).

## 3. Prerequisites and verification status

Status vocabulary: `verified` (checked read-only, result recorded), `reported-unverified` (stated by Jira or upstream, not checked here, or checked only in part), `blocked` (a repository or environment fact prevents the step).

| ID | Prerequisite | Status | Evidence |
| --- | --- | --- | --- |
| P1 | CAF PR 580641 completed, targets `develop`, and its description states admissions and launch stay closed and the owner applies | **verified** | `az repos pr show --id 580641`: `status` completed, `mergeStatus` succeeded, `closedDate` 2026-10-26T20:02:29Z, target `refs/heads/develop`. Description text read from the same call. Note the description records a **process waiver**: "No independent change review ran." |
| P2 | Policy secret version 8 holds 10 for the four admission values | **reported-unverified** (superseded by the owner decision in P2a; metadata verified; value not read) | `gcloud secrets versions describe 8 --secret=mcao-cloud-policy-d`: ENABLED, created 2026-10-26T20:19:13Z. The secret value was not accessed (no `versions access`). |
| P2a | Secret version in force is version 9 (owner decision). The effective limits are NOT confirmed by any available read-only means; how to confirm them before a window is an open owner item (section 9). | **reported-unverified** (version owner-confirmed; contents not checked by an agent) | **Owner decision** (Jira DPES-926 comment 76289, 2026-11-01): secret version 9 of `mcao-cloud-policy-d` is the version in force; version 8 evidence is superseded; the deployed revision already pins 9, so no re-pin is needed. Metadata checked earlier: `gcloud run revisions describe mcao-cloud-control-00067-zd6` (100% traffic, Ready) mounts item key **`9`** at `policy.json`. Version 9 was created 2026-10-29T02:18:43Z, one second before the revision (02:18:44Z), after CAF PR 580641 completed (2026-10-26T20:02:29Z), from a source not recorded in this repo or Jira. The timing therefore does not show that version 9 equals the develop policy file (P3). No agent has read version 9's contents, and no agent reads the secret value. Jira comment 76289 item (1) still requires verifying what version 9 contains before any live test, not waived; that verification remains open (section 9). |
| P3 | Policy document has the four values at 10, nothing else changed, `admission.version` 2 | **verified** (source file only; not the in-force secret version) | Read-only `az devops invoke` (git `items`, API 7.1) of `/policies/mcao-cloud-develop.json` at develop head `d1067355`, merge commit `a1348977` (PR 580641), and PR target `cd3fc49d`. The four values are 10 and `admission.version` is 2 at develop head and at the merge; the four values are 1 at `cd3fc49d`. The policy-file diff from `cd3fc49d` to `a1348977` changes only the four admission values. Develop head differs from the merge only at `profiles/standard/image_digest`. **Gap:** the file has no `admission.team_daily_worker_seconds`, which `cloud/src/mao_cloud/config.py` requires (origin/DPES-751). `policy_from_text` refuses both the develop head and the base with `Field required`, so the gap predates PR 580641. The deployed image's source commit is unverified (P6). If the deployed image uses this loader, a Ready revision means the in-force version 9 passed it, so version 9 would carry the field; that is an inference, not a check. |
| P4 | Normal admission is closed before any test window | **verified** (env value only) | `gcloud run services describe mcao-cloud-control`: `MAO_CLOUD_ADMISSIONS = closed`. The live `GET /internal/v1/admissions` route was not called (operator credential not available to this check). |
| P5 | Control API deployed in `dpe-platform-d` and ready | **verified** (deployed and Ready); pilot identities **reported-unverified** | `mcao-cloud-control` in `us-central1`: latest ready revision `mcao-cloud-control-00067-zd6`, Ready True, ingress `internal-and-cloud-load-balancing`. `mcao-cloud-signer` in `us-central1`: latest ready revision `mcao-cloud-signer-00006-ldq`, Ready True. No API route, token, or identity check was run. |
| P6 | Production launch path can start a worker | **blocked** | `BOOTSTRAP_INTAKE_BLOCKER` at repo head 79d38415 (the same `cloud/` tree at origin/DPES-751 8fce9911): `jobs.run` not called in production; gap 72 (DPES-807, DPES-804, DPES-819, DPES-899). The deployed revision's source commit was not verified against that head. |
| P7 | Stop confirmation releases held capacity | **blocked** | Gaps 35 and 41; DPES-807. Section 5 cleanup depends on it. |
| P8 | Deployed access mode and identities tie to the policy | **reported-unverified** | Gap 59; DPES-896. The deployed `access_mode` was not read. |
| P9 | Cost, quota and container-memory enforcement | **blocked** | DPES-804 (quota readiness, container memory). No dollar cap in repo or Jira. Owner must supply caps (section 4). |

### 3a. Discrepancies to resolve before any go-ahead

1. **Plan result.** Jira DPES-926 comment (2026-10-26) records "2 add, 9 change, 0 replace, 2 destroy". PR 580641 body says "Terraform plan NOT run". The Jira figure has no matching saved plan in the PR. Owner must reconcile.
2. **Verifying what version 9 contains is still required before any live test.** The owner has confirmed version 9 as the version in force (Jira DPES-926 comment 76289). Comment 76289 item (1) requires verifying its contents before any live test, and does not waive that. No agent reads a secret value. The develop-file comparison was done (P3): it matches the four values, but the file lacks `admission.team_daily_worker_seconds` (item 7), so it does not prove what version 9 contains. Verification therefore remains required, by an owner-only approved read or another route the owner approves (section 9). The pre-window baseline (section 6 step 0) reads `state`, `policy_version` and `holding_runs` only.
3. **Independent review.** The CAF PR completed under a waiver: "No independent change review ran." The owner must accept this for the deployed policy change.
4. **Stale repo doc.** `cloud/docs/deployed-smoke.md` describes the control API as undeployed. The read-only check shows a Ready deployment. The repo doc should be corrected by its own change, not by this plan.
5. **Tagged probe revisions.** The control service carries 13 tagged probe revisions at 0% traffic (`probe10`, `create-probe`, `fixture-probe`, and their `-2` to `-6` variants). Earlier probes may have left held capacity (gap 41). Under the section 6 step 0 baseline rule (proposed by the plan author; owner to confirm), `holding_runs` must read 0 before any run.
6. **Failing Terraform suite.** `grafana_cloud.tftest.hcl` fails in the upstream PR's test run (section 1, AC2). The upstream PR attributes it to base `80f5c8c`. Owner must confirm it is unrelated before relying on the upstream test result.
7. **Source policy file fails the repo loader (P3).** The develop-head `policies/mcao-cloud-develop.json` has no `admission.team_daily_worker_seconds`, and `policy_from_text` in `cloud/src/mao_cloud/config.py` (origin/DPES-751) refuses it with `Field required`. The base before PR 580641 is refused the same way. Whether the in-force version 9 carries the field is unknown. Owner should be told before the window, since the section 4 aggregate check depends on that value.

### Verification log (read-only; this pass)

Commands run from the worktree (no cloud write, no secret access, no login):

| Command | Result |
| --- | --- |
| `twg jira workitem get DPES-926 --output json --output-summary none` | passed; description and two comments read |
| `az repos pr show --id 580641 --org https://dev.azure.com/mclm --output json` | passed; completed, see P1; testing section read (six suites pass, `grafana_cloud.tftest.hcl` fails) |
| `gcloud config get-value project` | passed; `dpe-platform-d` |
| `gcloud secrets list --project=dpe-platform-d` | passed; `mcao-cloud-policy-d` listed |
| `gcloud secrets versions describe 8 --secret=mcao-cloud-policy-d --project=dpe-platform-d` | passed; ENABLED, metadata only |
| `gcloud secrets versions describe 9 --secret=mcao-cloud-policy-d --project=dpe-platform-d --format="json(name,state,createTime,destroyTime)"` | passed; ENABLED, created 2026-10-29T02:18:43.696488Z, metadata only |
| `gcloud secrets versions list mcao-cloud-policy-d --project=dpe-platform-d` | passed; versions 1 to 9 listed, all enabled; 9 newest |
| `gcloud run services list --project=dpe-platform-d` | passed; `mcao-cloud-control`, `mcao-cloud-signer` in `us-central1` among others |
| `gcloud run services describe mcao-cloud-control --region=us-central1 --project=dpe-platform-d` | passed; revision and Ready, and env values for `MAO_CLOUD_ADMISSIONS` and `MAO_CLOUD_POLICY_FILE` only |
| `gcloud run services describe mcao-cloud-control --region=us-central1 --project=dpe-platform-d --format="yaml(status.latestReadyRevisionName,status.latestCreatedRevisionName,status.traffic)"` | passed; `mcao-cloud-control-00067-zd6` at 100%; 13 tagged revisions with no traffic percent |
| `gcloud run revisions describe mcao-cloud-control-00067-zd6 --region=us-central1 --project=dpe-platform-d --format="yaml(metadata.name,metadata.creationTimestamp,spec.volumes,spec.containers[0].volumeMounts,status.conditions)"` | passed; secret volume `policy` item key `9` at `policy.json`; created 2026-10-29T02:18:44.351679Z; Ready True |
| `gcloud run services describe mcao-cloud-signer --region=us-central1 --project=dpe-platform-d` | passed; Ready True |
| `az repos pr show --id 580641 --org https://dev.azure.com/mclm --output json --query "{status:status,merge:mergeStatus,closed:closedDate,target:targetRefName,description:description}"` | passed; read again for the AC1 and AC2 wording |
| `git grep -n -e "^43\. " -e "^44\. " -e "^50\. " -- cloud/docs/protocol-v1.md` | passed; gap 43 pi argv, gap 44 pi event fields, gap 50 no reservation audit |
| `git diff --name-only 79d38415 origin/DPES-751 -- cloud policies` | passed; no changes under `cloud/` or `policies/` |
| `bin/fm-doc-audience-check.sh` | exit 1, same 28 unclassified paths as the base branch; the new plan is classified |
| `python3 -m json.tool docs/documentation-audiences.json` | exit 0 |
| `git diff --check origin/DPES-751` | exit 0 |
| `twg jira workitem get DPES-926 --output json --output-summary none` (this pass) | passed; comments 74348, 76278, 76288 and 76289 read; 76289 is the version-in-force owner decision |
| `az repos pr show --id 580641 --org https://dev.azure.com/mclm --query "{tgt:lastMergeTargetCommit.commitId,mrg:lastMergeCommit.commitId}"` | passed; target `cd3fc49d`, merge `a1348977` |
| `az devops invoke --org https://dev.azure.com/mclm --area git --resource commitDiffs` (base `cd3fc49d`, target `a1348977`) | passed; changed paths `CHANGELOG.md`, `docs/mcao-cloud-route-activation.md`, `policies/mcao-cloud-develop.json` |
| `az devops invoke --org https://dev.azure.com/mclm --area git --resource items` for `/policies/mcao-cloud-develop.json` at branch `develop`, commit `cd3fc49d` and commit `a1348977` | passed; content parsed in a local temp directory; only the admission block and a structural diff were inspected; no secret value involved |
| `git archive origin/DPES-751 cloud/src` into a local temp directory, then `policy_from_text` on the develop-head and base documents | develop head and base both refused: `admission.team_daily_worker_seconds` `Field required` |

Not run: `terraform` of any kind, `gcloud secrets versions access`, any login or credential command, any admissions route call (including the section 6 step 0 baseline read), any worker launch, any API token mint.

## 4. Caps (proposed values; owner confirmation required)

Neither the repository nor the Jira comments supply a per-worker, aggregate, cost, or runtime cap. The values below are **proposed** and are not approved. They apply only to the test window, and each must be confirmed by the project owner before any execution.

- **Runs:** exactly 10, all owned by one pilot owner so that `owner_worker_slots` = 10 is exercised with `team_worker_slots` = 10. No run beyond these 10, and no queued extras.
- **Retries:** 0. A retry of a test run is an abort condition.
- **Profile per run:** `STANDARD` (4 vCPU, 16 GiB). Aggregate for ten runs: 40 vCPU and 160 GiB.
- **Per-worker runtime budget:** 1800 s, inside the 60 s to 7200 s protocol range.
- **Aggregate worker-seconds ceiling:** 18000 (10 × 1800). The deployed `team_daily_worker_seconds` must be at least this value. The develop source file does not contain the field (P3, section 3a item 7), and no agent reads the in-force value; the owner must supply or confirm it.
- **Dollar cap:** not defined. The plan does not claim a cost bound. The owner must supply one, and it must be stated before execution.
- **Test window:** 60 minutes wall-clock from the first start request to the all-terminal check.

## 5. Timeout and cleanup

Timeouts:
- Queued deadline 1800 s (protocol); a run still queued after that expires.
- Startup timeout 600 s (protocol default).
- Runtime budget 1800 s per run (proposed, section 4).
- Test window 60 minutes; at expiry, run cleanup below regardless of state.

Cleanup order (execution only, after the go-ahead):
1. Drain admissions: `POST /internal/v1/admissions:drain`, confirm `state` is `drained`.
2. Bulk cancel the ten test run ids: `POST /internal/v1/runs:bulk-cancel` with the explicit ids. Do not filter by owner.
3. Read `holding_runs` from `GET /internal/v1/admissions` until it reaches 0.
4. If `holding_runs` stays above 0, stop. Do not release reservations by hand, do not delete objects by hand (gap 51), and escalate to the owner. Held capacity cannot be released in this build (gaps 35, 41).

## 6. Observable pass conditions

All of the following must be observed and recorded with timestamps, status responses, and run ids. Secrets, tokens, and signed URLs must not be logged.

0. **Baseline, taken immediately before the window opens and before any run.** Read `GET /internal/v1/admissions` (read-only; operator credential). Record `state`, `policy_version`, and `holding_runs` with a timestamp. This read is the pre-window check on live state. It reports `policy_version` (the document's `admission.version`) but not the four limit values, so it does not by itself confirm them (section 9). Pass requires `holding_runs` = 0 (proposed by the plan author; owner to confirm). Tagged probe revisions from earlier work (section 3a, item 5) may still hold capacity (gap 41). A nonzero baseline is not subtracted from the test: do not start, and escalate to the owner. Held capacity cannot be released in this build (gaps 35, 41).
1. Inside the window, after `admissions:resume` and before any run create: `state` is `open` and `policy_version` equals the document version the owner applied (expected `2`, per P3, to be confirmed). Normal admission stays closed outside the window.
2. Ten run creates succeed, and ten starts are admitted. No response is `queue_full`, `admissions_paused`, or a daily-seconds refusal.
3. Simultaneity: with the step 0 baseline at 0, `holding_runs` reaches 10 while all ten runs are non-terminal at the same sample. This is the evidence for AC4.
4. Each of the ten runs reaches a running state, and each produces a status record. A run that fails for a reason unrelated to admission is recorded as a failure, not a pass; the pass needs ten runs holding at once.
5. After cleanup, `holding_runs` is 0 and every test run id is terminal.

AC4 is met only when condition 3 is observed on a live deployment after the owner applies the change. A configuration read, a Terraform plan, or a local test does not meet it.

## 7. Abort conditions

Stop the test and run the cleanup in section 5 when any of these occurs:
- Any run is created or started that is not one of the ten test ids.
- The admissions policy version, or any admission limit, differs from the reviewed values, including if the pinned secret version 9 (P2a) holds values other than the reviewed ones (section 3a, item 2).
- Admissions reopen during the window without the drain procedure, for example after a broker restart (gap 55).
- Any run needs a retry, or any run passes its runtime budget.
- Observed spend, reservation, or daily worker seconds exceed the section 4 caps, or the owner's dollar cap.
- `holding_runs` does not fall after cancel.
- Any step needs a credential that is not already provisioned, or a login. Stop and report; do not create or change a credential.

## 8. Future execution commands (NOT EXECUTED)

These are the commands a separately approved run would use. Nothing here was run. Placeholders in angle brackets are filled only after the owner's go-ahead.

```bash
# Read-only pre-checks (section 3). These are safe to re-run.
az repos pr show --id 580641 --org https://dev.azure.com/mclm --output json
gcloud secrets versions describe 8 --secret=mcao-cloud-policy-d --project=dpe-platform-d --format=json
gcloud secrets versions describe 9 --secret=mcao-cloud-policy-d --project=dpe-platform-d --format=json
gcloud run services describe mcao-cloud-control --region=us-central1 --project=dpe-platform-d --format=json
gcloud run revisions describe <pinned-revision> --region=us-central1 --project=dpe-platform-d --format="yaml(metadata.creationTimestamp,spec.volumes)"

# Baseline (section 6, step 0): read-only, needs the operator credential the owner provides.
curl -sS -H "Authorization: Bearer <operator-token>" https://<control-api-host>/internal/v1/admissions

# Test window (requires owner go-ahead, P2a, P3, P6, P7 closed, and the step 0 baseline holding_runs 0).
curl -sS -X POST -H "Authorization: Bearer <operator-token>" https://<control-api-host>/internal/v1/admissions:resume  # NOT EXECUTED - requires owner go-ahead
# NOT EXECUTED - requires owner go-ahead: ten run create and start calls per the protocol-v1 request contract, one owner, no retries.
curl -sS -X POST -H "Authorization: Bearer <operator-token>" https://<control-api-host>/internal/v1/admissions:drain  # NOT EXECUTED - requires owner go-ahead
curl -sS -X POST -H "Authorization: Bearer <operator-token>" -H "Content-Type: application/json" --data '{"protocol_version": "v1", "run_ids": ["<run-id-1>", "...", "<run-id-10>"]}' https://<control-api-host>/internal/v1/runs:bulk-cancel  # NOT EXECUTED - requires owner go-ahead
```

Forbidden in every step: `gcloud secrets versions access`, `terraform apply`, any login or credential creation, and any object delete.

## 9. Execution needs a separate go-ahead

This plan does not authorize ten live worker launches. Execution requires a separate, explicit go-ahead from the project owner that names the caps, the owner, and the window. Until the owner has confirmed the items below and P6 and P7 are closed, the test cannot run from the production controller at the repository head. Admission stays closed, and AC4 is not claimed. The section 6 step 0 baseline must also read `holding_runs` 0 before the window opens.

Already confirmed by the owner (Jira DPES-926 comment 76289, 2026-11-01): secret version 9 of `mcao-cloud-policy-d` is the version in force, and version 8 evidence is superseded. No agent reads a secret value.

Items the owner must confirm before go-ahead:
- Verification of what secret version 9 contains. Jira DPES-926 comment 76289 item (1) requires it before any live test, and does not waive it. Route: an owner-only approved read, or another route the owner approves. The develop-file comparison (P3) was done and does not prove it, because the file lacks `admission.team_daily_worker_seconds`.
- The baseline rule: `holding_runs` = 0 before the window (section 6 step 0). Proposed by the plan author; owner to confirm.
- The caps and dollar cap (section 4). Proposed by the plan author; owner to confirm.
- How the four limit values are confirmed before the window. The status route reports `policy_version` but not `team_worker_slots`, `owner_worker_slots`, `team_queue_depth`, or `owner_queue_depth`, and the develop source file lacks `team_daily_worker_seconds` (section 3a, item 7).

Ten live launches still need a separate go-ahead after these items.

## 10. Residual risks

- Cost and resource caps are proposed, not approved.
- Live Cloud Run, Firestore, Storage and signer behavior is unverified (gaps 38, 57, DPES-899).
- Admission state has no durable source (gap 55); a restart can reopen admissions mid-test.
- Held capacity may not release after cancel (gaps 35, 41). Tagged probe revisions may hold capacity already; the step 0 baseline checks it, and that check needs the operator credential.
- Secret version 9 is the owner-confirmed version in force (P2a), but no agent has read its contents, and verifying them is required before any live test (section 9). The develop source file lacks `admission.team_daily_worker_seconds`, which the repo loader requires (P3, section 3a item 7). The status route does not report the four limit values (section 9).
- Upstream policy change completed without independent review (section 3a, item 3).
