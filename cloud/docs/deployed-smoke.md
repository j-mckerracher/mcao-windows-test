# Deployed control API smoke (DPES-896)

Status: the live checks have not run. The control API is not deployed in `dpe-platform-d`, and the
pilot identities the live checks need are not provisioned. Every live acceptance criterion below is
unmet. None of them is recorded as passed.

## Scope

In scope: read-only live checks of caller identity, owner scope, and ingress header preservation
against a deployed control API, plus local guards that run on every suite pass.

Out of scope: IAM grants or revocations, infrastructure changes, creating, cancelling, or retrying
runs, launching cloud workloads, Terraform, network and quota readiness (DPES-804), and attestation
(DPES-819).

## Running the live checks

The live checks live in `cloud/tests/deployed_smoke_test.py`. Run from the repository root:

```bash
uv --project cloud run pytest cloud/tests/deployed_smoke_test.py -q
```

Without the environment below, the live checks skip. The local guards always run.

| Variable | Value |
| --- | --- |
| `MAO_SMOKE_BASE_URL` | `https://` base URL of the deployed external HTTPS load balancer |
| `MAO_SMOKE_OWNER_A_TOKEN` | ID token for a provisioned pilot owner, for the API audience, minted through that owner's invoker service account |
| `MAO_SMOKE_OWNER_B_TOKEN` | ID token for a second, distinct provisioned owner |
| `MAO_SMOKE_SCHEDULER_TOKEN` | ID token for the Scheduler identity |
| `MAO_SMOKE_FIXTURE_RUN_ID` | `run_id` of a run that owner A created under explicit authorization; the suite only reads it |

Mint a token into the shell variable, never into a file or log. Impersonation and the audience come
from the leaf command `gcloud auth print-identity-token`, which accepts `--impersonate-service-account`,
`--audiences`, and `--include-email`. The email claim is required, because the broker matches it
against the identity map.

The live checks never print a token. Their assertion messages carry only the HTTP status and the
layer that answered: the app's typed error code, or an edge refusal.

Assertion rules, one per live check:

- Scheduler on an owner route: it must get the app's typed `not_allowed` refusal with HTTP 403. An
  edge refusal (a non-JSON body, or a body without a typed error code) does not show that the owner
  route refused the caller. The check fails as an unmet prerequisite, not as a pass. The suite does
  not say which edge component refused the request.
- Cross-owner read of the fixture run: it must get the app's typed `not_found` with HTTP 404, and its
  body must equal the body for a run that does not exist. A generic 404 from the edge fails, even
  when both reads match.
- Own read of the fixture run: HTTP 200, and a JSON body whose `run_id` is the fixture run's id. A 200
  for another run, a body without that identity, or a body that is not JSON fails.
- Anonymous read of an owner route: HTTP 401 or 403, refused by either layer. Neither layer may
  accept it.

The local section of `deployed_smoke_test.py` runs fake responses through the same assertion helpers
the live checks use. The scheduler, cross-owner, and own-read rules each have failing cases, which
are the false passes this suite used to accept, and passing cases for the typed responses they
require. A local test checks that the invoker flow sends the one token in both headers.

## Read-only prerequisite check (2026-10-22)

Project `dpe-platform-d`, region `us-central1`, the candidate region in DPES-803. Each read used an
explicit project. Leaf help was read before each command.

- `gcloud run services list --project=dpe-platform-d --region=us-central1`
- `gcloud compute url-maps list --project=dpe-platform-d`
- `gcloud compute backend-services list --project=dpe-platform-d --global`
- `gcloud compute network-endpoint-groups list --project=dpe-platform-d`
- `gcloud compute ssl-certificates list --project=dpe-platform-d --global`
- `gcloud compute security-policies list --project=dpe-platform-d`
- `gcloud iam service-accounts list --project=dpe-platform-d`
- `gcloud iam service-accounts get-iam-policy mcao-cloud-inv-jmckerracher-d@dpe-platform-d.iam.gserviceaccount.com --project=dpe-platform-d`

Results:

- No control API Cloud Run service exists. The only `mcao-*` service is `mcao-trial-trigger`, which
  is DPES-803 trial infrastructure.
- No URL map, backend service, serverless NEG, or managed certificate exists for the control API.
  The shared `base-policy` Cloud Armor policy exists. Its rules were not inspected.
- Service accounts present: `mcao-cloud-controller-d`, `mcao-cloud-scheduler-d`,
  `mcao-cloud-worker-d`, `mcao-cloud-signer-d`, and one per-human invoker,
  `mcao-cloud-inv-jmckerracher-d`.
- The per-human invoker has no IAM bindings. It has no token-creator grant for its human and no
  invoker grant on the API.
- No second owner invoker exists.

Conclusion: the deployed API that the live checks target is absent, and the DPES-806 pilot identities
are incomplete. DPES-803 still records implementation and deployment as outstanding.

## Acceptance evidence

| Criterion | Live status | Evidence |
| --- | --- | --- |
| AC1: a deployed caller test and a cross-owner test pass against the real ingress, results recorded | Not run. No deployed API, and no second owner identity. | Live checks: `test_live_owner_reads_its_own_run_through_the_invoker_flow`, `test_live_other_owner_gets_the_same_refusal_as_a_missing_run`, `test_live_scheduler_is_refused_on_an_owner_route`. Fake-level and passing: the cross-owner and refusal cases in `cloud/tests/auth_routes_test.py`. |
| AC2: Authorization survives the ingress for the invoker service account flow, or a failure is recorded with the ingress change needed | Not run. No load balancer serves the control API. | Live check: `test_live_owner_reads_its_own_run_through_the_invoker_flow` sends the same token in `Authorization` and `X-Serverless-Authorization`, and passes only when the owner's read returns its own run. Local: `test_invoker_headers_send_one_token_as_both_authorization_headers` checks both headers on the request. The suite reports the status and the layer that answered, not the hop that dropped or refused a header. No ingress change is recorded yet, because the check has not run. |
| AC3: revocation and remap evidence recorded | Fake-level only. A live run needs IAM mutation or isolated fixtures, and neither is authorized for this task. | `cloud/tests/auth_revocation_test.py`: a revoked owner is refused on every owner route and its run is unchanged; revoking after create and before start is refused and leaves the run unstarted; a remapped identity cannot reach the run it created; `owner_id` never rebinds; a queued run still progresses under the scheduler after its owner is revoked; revocation takes effect on the next request; a refused caller is audited by subject, never by owner or token. |
| AC4: no anonymous endpoint, broad public invoker grant, or worker callback route is introduced | Local: passed. Live: not run, because no deployed service exists to inspect. | `deployed_smoke_test.py` pins the route table to the 13 reviewed routes and checks that each one refuses an anonymous caller with `not_allowed`. `broker.py` has no callback route. This change adds no route, IAM binding, or infrastructure. |

Implementation steps, numbered as in the Jira text:

| Step | Status |
| --- | --- |
| (1) Deployed caller and cross-owner smoke: an owner reads and cancels only its own runs; a second owner and the Scheduler are refused | Read and refusal checks are written and not run live. The cancel-own-run check is not in the suite, because it needs an authorized fixture run. |
| (2) External HTTPS load balancer, serverless NEG, managed certificate, DNS, and Cloud Armor preserve the `Authorization` header | Not possible yet. No control API ingress exists. No infrastructure was created here. |
| (3) DPES-808 revocation and remap against the deployed API: allow or refuse, audit identity, no rebind | Not run live. Fake-level evidence only, under AC3, including the create-to-start window. |

## Needed to close the live criteria

1. A deployed control API for `dpe-platform-d`: a Cloud Run service, a serverless NEG, a backend,
   a URL map, a managed certificate, and the Cloud Armor path. Deployment is owned outside this task.
2. DPES-806 completion: a second pilot owner identity, per-human invoker bindings, and a Scheduler
   invoke grant on the API.
3. Explicit authorization to create one fixture run for owner A. The live suite only reads that run.
   A live cancel-own-run check needs that fixture and a separate approval.
4. Approval to run the read-only live suite against the deployed API.
5. A live revocation and remap check needs an approved IAM change or isolated fixtures. Neither is
   authorized for DPES-896.

## Local verification

- `uv --project cloud run pytest cloud/tests/deployed_smoke_test.py -q`: 34 passed, 4 skipped.
  The skips are the live checks, because the environment is absent.
- Deployed smoke, revocation, routes, and inventory suites together:
  `deployed_smoke_test.py`, `auth_revocation_test.py`, `auth_routes_test.py`, `test_suite_inventory.py`:
  102 passed, 4 skipped. The skips are the live checks, because the environment is absent.
- Full cloud suite, `uv --project cloud run pytest cloud/tests -q`: 1914 passed, 4 skipped.
- Live checks were run against an unreachable `https://` address with dummy values in every
  variable: each one errors on the connection and fails, so none of them passes vacuously.
- Before the assertion fixes, the local regressions were run against the old live assertions. Three
  false passes reproduced: a Scheduler edge refusal, a generic identical 404 for the cross-owner
  read, and a 200 for another run. Each now fails as its rule requires.
