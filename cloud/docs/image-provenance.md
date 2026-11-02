# Image provenance and attestation read-back (DPES-819)

This record holds what the GitHub Actions logs and job steps show for the cloud images. It was
read with `gh` only. No cloud call, registry read, or GCP change was made to write it. It changes
no workflow, code, or CAF file. Its tests are in `cloud/tests/test_image_provenance_record.py`.

## Runs read

| Run | Event | Branch | Head commit | Created (UTC) | Conclusion |
|-----|-------|--------|-------------|---------------|------------|
| 39828880739 | workflow_dispatch | `fm/DPES-819-dpes-819-build-fix` | `0c369553a2654c5161463d1fac1e0e946dd90190` | 2026-10-22T18:09:26Z | success |
| 39852080399 | push | `main` | `fe230a3270122b4ac899d1b0c6e2289e8b974a29` | 2026-10-22T20:48:57Z | success |

Run 39828880739 is **not the merged build**. Its head commit is diverged from `main`: the
compare `0c369553...fe230a3` reports ahead 11, behind 5. Four build inputs differ between the two
commits: `.github/workflows/cloud-images.yml`, `cloud/Dockerfile`, `cloud/Dockerfile.controller`
and `cloud/toolchain.json`. Its digests describe that pre-merge build only.

Run 39852080399 is the merged-main build at `fe230a3`. It is the record for what `main` published at that commit.

`origin/main` has moved past `fe230a3` since then. Commits `5f433af` and `e1f65a7` changed cloud
source files (`cloud/src/mao_cloud/checkpoint_publish.py`, `results.py`, `storage.py`). Their push
runs are 39853060668 (`5f433af`) and 39853097654 (`e1f65a7`), both `success`. Their digests are not
read here. The digests below describe `fe230a3` only.

## Pushed digests

Both images are pushed to `us-central1-docker.pkg.dev/dpe-platform-d/artifact-repository`, tagged
by commit SHA. The digests below come from the `digest:` output of the push step in each run's
logs. The same `@sha256:` value appears in the `docker push` reference lines of that run.

| Image | Run 39828880739 (pre-merge) | Run 39852080399 (main) |
|-------|-----------------------------|------------------------|
| `mao-controller` | `sha256:ac35ee8a8e6800da80f457283c5adf32611f166b86d59dafefcf9c21b5ffc89f` | `sha256:c3d735e948b6ab22559ba4a2cfcc3f7ec2e5eb62c40a5767c67dbb1e094451e6` |
| `mao-worker` | `sha256:9e4b20b14021cbaf1421a7fb79ca4f44b8cfb0b0ad993de3a8423e03404f90e6` | `sha256:09c20f10d933b38f1b050f0b201a5c99eb80042911142ed87c7aeb184fba363e` |

The `record-digests` job reads the same digests from Artifact Registry and writes them only to its
job summary. The logs API does not expose that summary, so it was not read. Its step conclusion
was `success` in both runs.

## Scan

- `Get Image Scan Results` succeeded for both images in both runs.
- The scan reported `Scan Status: FINISHED_SUCCESS` for both images in run 39828880739.
- The lines `Scan Results Critical:` and `Scan Results High:` printed **empty values** for both
  images. The gate passes when no count is returned.
- **Result: a zero-finding scan is not proven by these logs.** The pass only shows that the gate step ran. The registry read under
  [Current candidate](#current-candidate) is the evidence for the newest digests.
- The gate logic is in `mclm-dps/reusable-workflow-push-gcr-image`, pinned at
  `b56e3b7dec2f90c9de0b35fec9b68c232b6e63c8`. It is not in this repository.

## Attestation read-back

Proven for run 39828880739, both images, in both push jobs:

- `Generate artifact attestation` succeeded. The log shows `Attestation signed using certificate
  from GitHub Sigstore instance`, and the `subject-digest` equals the pushed digest: `sha256:ac35ee8a…`
  for `mao-controller` and `sha256:9e4b20b1…` for `mao-worker`.
- `Generate sbom attestation via syft sbom` succeeded.
- The attestation actions used were `actions/attest-build-provenance` and `actions/attest-sbom`,
  plus `actions/attest`, each at a pinned full SHA.

Run 39852080399 (main): the artifact attestation and SBOM attestation steps show `success` in both
push jobs. Subject digests were not re-read for this run.

**Not proven: `mc-vuln-attestor`.** No step, action input or log line in either run names
`mc-vuln-attestor`. The pinned reusable workflow exposes only `generate_attestation`, which produces
the GitHub build-provenance and SBOM attestations above. Binary Authorization does not read those.
Neither the reusable workflows nor the CAF Terraform and pipeline contain a step that signs as
`mc-vuln-attestor` (read-only scouts, 2026-10-22). Mayo platform documents say automation outside both
signs a digest after Artifact Registry's scan finishes clean. That is documented behavior; the
automation was not found and no attestation record was read, because the attestor project read is
denied. The CAF note in `caf-dpe-platform-app-resources/docs/claude-code-trial.md` (line 40), which
says the mclm-dps reusable workflows produce an `mc-vuln-attestor` attestation, is not supported.

## Current candidate

Pushed by the newest main build that changed cloud sources. Commit `c94f44d` changed only
`hve-verification.yml` at the repository root, so it published no images. Read-only `gcloud artifacts docker
images describe --show-all-metadata` reads on 2026-10-22 at 22:49Z.

| Item | `mao-controller` | `mao-worker` |
|------|------------------|--------------|
| Source commit and run | `ddce90acb68f37c7e4c42c8c0b73ed8d4d1414ab`, run 39857722683 (push, started 21:29:50Z) | same |
| Digest | `sha256:3658015d8ce36ba62ce70d29f81b8d550e13ffff092d91740b9bf5e63433f077` | `sha256:3cf4c489faea0fa3137c4765add639456626467b779f377d9d80f08b79ebcbfe` |
| Tag in the registry | `mao-controller:ddce90acb68f37c7e4c42c8c0b73ed8d4d1414ab` | `mao-worker:ddce90acb68f37c7e4c42c8c0b73ed8d4d1414ab` |
| Discovery record created (UTC) | 2026-10-22T21:31:50Z | 2026-10-22T21:32:09Z |
| Scan status | `FINISHED_SUCCESS`, continuous analysis `ACTIVE` | `FINISHED_SUCCESS`, continuous analysis `ACTIVE` |
| Last scan (UTC) | 2026-10-22T21:32:01Z | 2026-10-22T21:32:22Z |
| Vulnerability summary | none returned | none returned |
| `slsa_build_level` | `unknown` | `unknown` |

The run's jobs all concluded `success`: both builds, both pushes, both scan reads and `record-digests`.
The registry shows no Critical or High finding for either digest at the read time. Continuous analysis can
add findings later, so the read must be repeated at deployment.
The GitHub scan step's empty counts remain unexplained; the registry read is the evidence used here.
GitHub provenance is not Binary Authorization evidence and is not used as such.

`cloud/src/mao_cloud/release_evidence.py` repeats the check from saved describe output, for example, after one describe per image:
`gcloud artifacts docker images describe <image>@<digest> --show-all-metadata --format=json --quiet > controller.json`
(the same for the worker and signer images, into `worker.json` and `signer.json`), then
`uv run --frozen python -m mao_cloud.release_evidence --controller-describe controller.json --controller-digest <controller-digest> --worker-describe worker.json --worker-digest <worker-digest> --signer-describe signer.json --signer-digest <signer-digest>`.
It exits nonzero when a scan has not finished, a Critical or High finding is listed, or a digest differs from the one asked for.
It cannot read an attestation, and its output says so.

A tool-registry digest is part of a release identity (`release-and-rollback.md`), but this repository publishes no
standalone tool-registry artifact. The worker image carries `cloud/toolchain.json`. What the registry digest names, and
where it is retained, is undecided. The compatibility matrix therefore stays empty and no release is a supported identity yet.

## CAF edit still needed

The CAF change is digest pins only. Nothing in the CAF pipeline or Terraform creates an attestation, so no
pipeline step is needed to obtain one.

1. **Image names.** CAF `develop` at `9680ddf84729fe966b418882253184467772bdff` (read 2026-10-22 at 23:01Z) already has separate
   `mcao_cloud_controller_image_digest` and `mcao_cloud_worker_image_digest` variables. Their repository variables default to
   `artifact-repository/mao-controller` and `artifact-repository/mao-worker`, and an empty digest creates nothing.
   Its Terraform consumes them (`mcao-cloud-control.tf` and `mcao-cloud-jobs.tf`, see `release-and-rollback.md`).
   The digest lines in `vars/develop.tfvars` are commented out and show the older `fe230a3` digests.
2. **Digest values.** Each pin must be the digest read back at deployment time, not a value copied from this record. Every push to `main`
   that changes `cloud/**` publishes new digests that need their own scan.
3. **Attestor.** Binary Authorization admits a digest only with an `mc-vuln-attestor` attestation. Its producer is outside CAF and outside
   the GitHub reusables, and is not named. The dev deployment is the live check; see `release-and-rollback.md`.

## Gaps

| Gap | Status | Blocking item |
|-----|--------|---------------|
| Zero Critical/High scan result | Registry read shows none for the `ddce90a` digests at 22:49Z; repeat at deployment because continuous analysis can add findings | DPES-819 |
| `mc-vuln-attestor` attestation | Not read; producer outside CAF and the pinned reusables and unnamed; the approved dev apply is the live Binary Authorization check | DPES-819 |
| Controller and worker names versus the CAF image variables | Resolved: CAF `develop` `9680ddf` has split controller and worker variables that default to the Motif image names | DPES-819 |
| CAF digest pins in `vars/develop.tfvars` | Separate CAF change; values must be read back at deployment | DPES-819 |
| Impersonated deploy service account read and Binary Authorization enforcement | Needs the approval-gated dev apply | DPES-819 |
| Live rollout order, Jobs, Scheduler and pilot flag | Needs a real deploy | DPES-804, DPES-807 |
| Live rollback without deleting active resources | Needs a real deploy | DPES-804, DPES-871 |
| Availability of the pinned old image and tool registry in Artifact Registry | Image reads work (this record); the tool-registry artifact is undefined | DPES-804 |
| Container run of the controller and worker images | Built and smoke-tested without network in Cloud CI (run 39857721361, `success`); not run on Cloud Run | DPES-819 |
| Container run of the signer image | Build and smoke test added to Cloud CI in this change; no run yet; not run on Cloud Run | DPES-819 |
| Signer digest and scan state | No signer image has been pushed from `main` yet; the signer is absent from every digest record above | DPES-819 |
