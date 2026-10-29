# Cloud release, rollout, and rollback (DPES-819)

This document covers the immutable build, attestation, and digest-addressed deploy path for the
cloud controller, worker and signer, plus the rollback procedure. Decisions are fake-level and tested in
`cloud/tests/test_release_matrix.py`. Anything that needs real GCP or a container daemon is listed
under [Gaps and blocking steps](#gaps-and-blocking-steps).

## What ships

Three images, all built from the repository root as build context. Each Dockerfile has its own
allowlist ignore file, so only the sources it copies enter the context.

| Image | Dockerfile | Allowlist | Contents |
| --- | --- | --- | --- |
| controller | `cloud/Dockerfile.controller` | `cloud/Dockerfile.controller.dockerignore` | the `mao_cloud.serve` entrypoint, which builds the broker app, and hash-locked controller dependencies from `cloud/requirements/controller.txt` |
| worker | `cloud/Dockerfile` | `cloud/Dockerfile.dockerignore` | trusted bootstrap and pinned pi 1.3.0 runtime; Node 22.19 or newer |
| signer | `cloud/Dockerfile.signer` | `cloud/Dockerfile.signer.dockerignore` | the `mao_cloud.signer_serve` entrypoint, the only process that signs object capabilities, with the controller's hash-locked dependencies |

All three images pin `alpine:3.24.2` by digest and run as numeric non-root users. The controller and
signer carry no worker runtime: no pi, Node, npm, or sandbox tooling. The worker removes npm after the
pi install.

The controller requirements are exported from `cloud/uv.lock` with hashes. CI fails if the
committed file drifts from the lock.

## Workflows

- `.github/workflows/cloud-ci.yml` runs on pull requests and `main`. It runs the Python suite
  (including the test-inventory guard), checks the controller requirements against the lock, and
  lints every workflow with the repo-pinned actionlint. It also builds all three images without pushing
  and without secrets, so a broken image definition fails on the pull request instead of on `main`.
- `.github/workflows/cloud-images.yml` runs on `main` and manually. It builds all three images, pushes
  them to the nonprod Artifact Registry through the reference reusable workflows, and enforces the
  critical and high vulnerability gate (never disabled). All three pushes generate an SBOM and an
  attestation. Then it calls the digest recorder below.
- `.github/workflows/cloud-record-digests.yml` reads each pushed digest and its scan state back with
  `gcloud artifacts docker images describe --show-all-metadata`, gates the three-image set through
  `release_evidence.py`, and writes the digests and scan state to the job summary. The reusable push
  workflows expose no digest output, so this is the only digest source. It does not read attestations.
- `cloud/src/mao_cloud/release_evidence.py` checks saved `gcloud artifacts docker images describe` output for a
  candidate set of controller, worker and signer: finished scan, no listed Critical or High finding, no two images
  sharing a digest, and, for an operator check, the digest that was asked for. It does not read
  attestations. See [Current candidate](image-provenance.md#current-candidate).

Nothing in these workflows deploys. Pull requests build and smoke-test all three images without pushing. Image publication and registry digest reads run only on main or through manual dispatch.

The Cloud CI images job runs each built container without network access as its configured non-root user. It checks command resolution, starts pi in the worker, confirms npm is absent, imports the serve entrypoint in the controller, and imports the signer entrypoint in the signer.

## Release identity and the compatibility matrix

A release identity is the exact controller digest, worker digest, tool-registry digest, worker
profile, trusted entrypoint, protocol version, checkpoint schema version, and harness version. The
recorded identities live in `cloud/release/compatibility-matrix.json`. The pure decision logic is
`cloud/src/mao_cloud/release_matrix.py`.

The shipped matrix is empty (`"supported": []`). Every plan is therefore refused until a reviewed
change records a built, scanned, pushed, and smoke-checked identity. The digest step does not edit
the matrix.

A plan is refused as a whole, with no steps and no dispatch or resume, when:

- the source is not a recorded supported identity,
- the target is not a supported identity,
- the worker profile is not the pilot profile `4/16`,
- the worker entrypoint is not the trusted worker entry `mao_cloud.worker_entry` (a plain pi fallback is refused), or
- any target digest is not in the retained artifact set.

Every run and checkpoint touched by a refused plan is preserved as it was.

Maintain the matrix this way:

- Add an identity only after all three images are built, pushed, digest-read, and passing the nonprod
  smoke checks. Keep it in the same reviewed change that records its digests.
- Keep every identity that a rollback could target. Never remove a retained artifact that a
  supported identity still needs.
- Never reuse a checkpoint schema version for an incompatible change.

## Rollout order

Enforced as data by `rollout_order()`:

1. deploy identities and state
2. deploy controller and worker images by digest
3. run service, Jobs, and auth smoke checks
4. enable Scheduler and monitoring
5. enable the pilot flag

Images are always addressed by digest, never by a mutable tag.

## Rollback

1. Disable new admissions through the durable DPES-818 admissions switch on every broker before collecting the affected runs and checkpoints.
   This must make `Ports.admissions.is_open()` return false for create, start, and retry, while preserving replay, cancellation, and reconciliation.
   Wait for in-flight admitting requests to finish and confirm new non-replay requests receive `admissions_paused` before capturing the snapshot.
   Kill switches control allocation and dispatch; they do not enforce this admission pause.
2. Keep admissions disabled and capture the affected runs and checkpoints, then pass `admissions_disabled=True` to `plan_rollback` only for that quiesced snapshot.
   Without this explicit confirmation, the planner refuses all dispatch and resume actions.
   The planner is pure data and does not implement or operate the durable switch.
3. Engage every kill-switch class in `KillSwitchClass` and choose drain or cancel for in-flight runs explicitly.
   There is no default for that choice.
4. Restore the exact target controller image, worker image, and tool-registry digests.
5. Confirm the Cloud Run service, Jobs, and Scheduler resources are intact.
   Rollback never deletes a resource.
6. Release the kill switches only after the target release is ready and the dispatch and resume decisions are recorded, then re-enable admissions.

Rollback is blocked until DPES-818's durable admissions reader and operator controls are wired to every serving broker and can prove the pause and quiescence described above.
No production adapter for those controls ships in this change.

Queued runs admitted under the target worker are dispatched. Runs admitted under any other worker
are preserved as admitted and are not reinterpreted.

With the drain choice, active checkpoints resume only under the exact identity that wrote them: the same checkpoint schema, worker image digest, and harness version.
Any other checkpoint is preserved and is not reinterpreted.
With the cancel choice, all affected active checkpoints are preserved without authorizing resume, even when their identity matches the target.
This extends the active-checkpoint identity rule stated as `ACTIVE_CHECKPOINT_RULE`.

## Dev deployment handoff

Scope is the dev project `dpe-platform-d` only. The project owner accepted a dev deployment without a proven zero-finding scan or a
read `mc-vuln-attestor` attestation (Jira DPES-819 comment, 2026-10-22). Production activation needs both closed.
No step below is performed by this repository: the apply stays approval-gated in the CAF Azure DevOps pipeline.

Required CAF change, made in a CAF pull request and not from this repository.
CAF `develop` at `9680ddf84729fe966b418882253184467772bdff` (read 2026-10-22 at 23:01Z) declares `mcao_cloud_controller_image_digest` and
`mcao_cloud_worker_image_digest`. Its Terraform consumes them: the control service image is `local.mcao_cloud_controller_image` in
`mcao-cloud-control.tf` and the worker Jobs image is `local.mcao_cloud_worker_image` in `mcao-cloud-jobs.tf`. An empty digest creates nothing.
The digest lines in `vars/develop.tfvars` are commented out, and the comments also name `mcao_cloud_control_domain`.
The change sets only those digest values.
It adds no pipeline step: nothing in the CAF pipeline or Terraform produces or checks an attestation.

**Deployment prerequisites, not met.** The controller image cannot serve. Its entrypoint, `python3 -m mao_cloud.serve`, reads `PORT`,
reads the policy file named by `--policy` or `MAO_CLOUD_POLICY_FILE`, validates it and `api_audience`, and then builds the production adapters from the `MAO_CLOUD_*` settings. A missing or
invalid setting exits with status 2 and names the setting or settings, and binds no port. A client that cannot open exits with status 3
and "refusing to start", and binds no port. `mao_cloud.production` composes the Firestore run store, Cloud Storage object store, and
Cloud Run Jobs launcher, but their live behavior is unverified. The signer client is `HttpSignerClient` at `MAO_CLOUD_SIGNER_URL`: it calls
the signer service with the controller's ID token and signs nothing itself. The deployed signer and the live call are unverified (DPES-899). The launcher refuses every launch while the production launch gate is closed, so no execution is started and no stop is
possible (gap 72 in `protocol-v1.md`). Stop and confirmation of the named execution are built behind that gate and are unverified live (gap
35). An attempt whose launch named no execution keeps its capacity reserved (DPES-807). The admissions switch is per process and does not
survive a restart (gap 55 in `protocol-v1.md`). `test_controller_image.py` and `test_serve.py` pin these refusals.

**Infrastructure must set these; this repository sets none of them.**

1. `MAO_CLOUD_POLICY_FILE` to the path where the deployment mounts the owner-approved policy JSON, or pass `--policy <path>` as the container argument. The image carries no policy file, and a missing one exits 2.
2. `MAO_CLOUD_ADMISSIONS=closed` for first activation. The setting is required, has no default, and an omitted value exits 2. Admissions open only when an operator resumes them.
3. In the policy file: the seven `admission` values, `api_audience` (the bare https origin of the control API that caller tokens carry), and the identities that map callers. The project owner supplies every value, and none is in this repository. The deployed file is unverified (DPES-896).

Terraform and CAF changes are outside this repository.

The worker job cannot run an attempt yet. The intake delivery path is built: the launcher passes the run and attempt identifiers and one
signed intake URL, and the worker entrypoint fetches and verifies the intake (gap 72 in `protocol-v1.md`). The production launch gate stays
closed until DPES-899, DPES-807, DPES-804, and DPES-819 close. Do not enable dispatch in any deployment until that gate opens.
The stored `RunRecord` omits `execution_name` while it is unset, preserving reads by older controllers that forbid unknown fields.
Populated execution names require an upgraded reader and remain stored because stop and termination confirmation depend on them.
Before opening the launch gate, upgrade every serving controller and record compatible rollback targets that accept this field.
Do not roll back to a controller that forbids `execution_name` while any retained record contains it, and do not remove recorded names to force such a rollback.

The Firestore admission layout must exist before the first store operation, or every operation refuses. Provision it once per database,
with `MAO_CLOUD_PROJECT` set (and `MAO_CLOUD_FIRESTORE_DATABASE` when the database is not the default `mcao-cloud`):

    python3 -m mao_cloud.provision

Run it from an operator identity, not the serving identity, and not at service start. A Cloud Run service may start many instances at
once, and provisioning from each would give every instance write rights it does not otherwise need. A shard-count change would also refuse
every instance together. The command exits 0 when the layout is provisioned for the serving shard count, 2 when the project is missing, 3
when the write was not confirmed (run it again), and 4 when the database already holds a different shard count. Its live behavior is
unverified (`test_provision.py` covers the exit codes against the in-memory fake). Until the live behavior is proven and the signer and execution controls are built, this handoff is limited to preparing
digest pins.
Do not approve an apply that creates the control service. This release-evidence work does not implement those adapters.

Preparing the pins:

1. Pick the build to activate. Only a push to `main` that changes `cloud/**` publishes images; the newest one when this was written is in
   [Current candidate](image-provenance.md#current-candidate). Each new digest needs its own scan, and the trial image was admitted
   about 14 minutes after its push while platform documents say to allow about 25.
2. Read the controller, worker and signer digests from Artifact Registry again and run the evidence check on all three. Stop on any blocker.
3. Prepare the CAF digest values for all three images in a CAF pull request and review the Terraform plan for unrelated changes and secret values.

Only after the prerequisites above pass:

1. Approve the existing apply. It is the live Binary Authorization check. If Cloud Run refuses with "not authorized by policy", stop. Do not add a
   break-glass annotation or change policy. Send the owner of the attestor project the digest, push time, scan state and exact refusal text.

Record at deployment:

- Source commit and workflow run, both digests, and the registry read timestamps and scan state.
- The CAF pull request, the pipeline build, the plan summary and the approver.
- The effective digest on each Cloud Run Job and service after the apply, read back from Cloud Run.
- Binary Authorization `AdmissionReview` audit entries for the apply
  (`serviceName="binaryauthorization.googleapis.com"` in `dpe-platform-d` Cloud Logging). They show an evaluation ran, not the verdict, so
  record admission as inferred from the successful create or update, not as a read attestation.
- Config versions, the rollout order step reached, and the previous digests and registry reads that a rollback would restore.

Rollback needs the previous controller, worker, signer and tool-registry digests recorded before the apply and still present in Artifact Registry.
The first deployment has no previous release, and the compatibility matrix is empty until a deployed identity is recorded by a reviewed change.

## Gaps and blocking steps

| Gap | Status | Ticket |
| --- | --- | --- |
| Rollout and rollback against real Cloud Run, Jobs, and Scheduler. Only the fake-level plan contents and the no-delete check are proven here. | Not proven | DPES-804, DPES-807, DPES-819, DPES-871 |
| Container build of all three images and in-container checks (pi starts, npm absent, the controller imports the serve entrypoint, the signer imports the signer entrypoint). Controller and worker ran in Cloud CI without network (run 39857721361). The signer build and checks were added in this change and have not run in CI. Running any of them on Cloud Run is not proven. | Controller and worker proven in CI; signer unverified until its first CI run | DPES-819 |
| Impersonated service account can read Artifact Registry digests, and Binary Authorization accepts the attestations. | Not proven | DPES-819 |
| The `mc-vuln-attestor` attestation that Binary Authorization requires. The producer is outside CAF and the pinned reusable workflows and is unnamed. No attestation record was read. `generate_attestation` produces GitHub attestations, which the policy does not read. | Not proven; the approved dev apply is the live check | DPES-819 |
| Controller cannot serve against live services. `mao_cloud.serve` composes the production adapters from `MAO_CLOUD_*` settings: missing settings exit with status 2, and a client that cannot open exits with status 3. The live Firestore, Cloud Storage, and Cloud Run Jobs adapters are unverified. The signer client calls the signer service at `MAO_CLOUD_SIGNER_URL`; the deployed signer and the live call are unverified (DPES-899). Launch is refused while the production launch gate is closed (gap 72); stop and confirmation are built behind that gate and are unverified live, and attempts with no execution name keep their capacity reserved. A durable admissions source is not built (DPES-807). The Firestore admission layout must be provisioned once per database with `python3 -m mao_cloud.provision`. Cloud Run `PORT` wiring is in the entrypoint; the Scheduler reconcile job is deployment configuration. | Open | DPES-807, DPES-809, DPES-899 |
| CAF digest pins (see Dev deployment handoff). No CAF pipeline change is needed to obtain an attestation. | Separate CAF pull request | DPES-819 |

BLOCKING human steps, which the repository admin must complete before the image workflows can run
against nonprod:

- Create the GitHub environment `nonprod`.
- Add the secret `GS2_IMPERSONATION_ACCOUNT_SECRET` to that environment. It authenticates the push
  and digest-read steps, which impersonate `gsa5-caf-mgmt@dpe-platform-d.iam.gserviceaccount.com`.
