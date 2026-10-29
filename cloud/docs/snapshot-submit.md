# Snapshot submit: source export and run submission (DPES-810)

This document covers `mao-cloud doctor` and `mao-cloud submit`, the source exporter they share, the
durable local record, and the two-step upload and start protocol. Everything here is built and
tested against the protocol, broker, and storage fakes; no cloud resource is touched. Anything that
needs real Cloud Storage, a deployed control API, or a container daemon is listed under
[Gaps and blocking steps](#gaps-and-blocking-steps).

Code: `cloud/src/mao_cloud/source.py` (exporter), `doctor.py`, `submit.py` (plan and step machine),
`run_record.py` (local record), and `cli.py`. Tests: `cloud/tests/test_source_export.py`,
`test_doctor.py`, `test_run_record.py`, `test_submit.py`, and `test_submit_cli.py`.

## Commands

`doctor` is a local preflight. It runs the same export that `submit` runs, so a passing doctor and a
submittable branch cannot disagree. It sends nothing and runs no model.

```bash
mao-cloud doctor --repo <path> --ref <branch>
```

`submit` plans by default. Without `--execute` it exports the branch, builds the create body, and
prints the request UUID and canonical digest. It writes no record and sends nothing.

```bash
mao-cloud submit --repo <path> --ref <branch> --state-dir <dir> \
  --task-id <id> --prompt-id <id> --profile <id> --target <id> --harness <name> --model <name> \
  --thinking <level> --max-runtime-seconds <n> --workflow-correlation <id> \
  [--request-uuid <uuid>] [--execute]
```

- `--request-uuid` resumes a request. Without it a new RFC 9562 version 7 UUID is generated. After
  an exit 75, rerun the same command with the same `--request-uuid`.
- `--execute` needs a transport. The shipped CLI has none, so `--execute` exits 2 and writes no
  record until a service transport is wired in (see the gaps table).

Exit codes: 0 success or plan; 1 refused (the input, the source, the service, or a local conflict);
2 the input cannot be read or used, or `--execute` has no transport; 75 retry the same command later.

## Source export

The exporter reads committed history only. It never reads the working tree, HOME, the user's git
configuration, or any environment variable beginning with `GIT_`. Git runs as argv-only subprocesses
(`git -C <repo> ...`), never through a shell, so the repository path and the branch name are data.

Refusals happen before any network call, in this order:

1. Ref. Only a branch name is accepted. A raw commit SHA, a tag, a missing branch, or an invalid
   name is refused with guidance.
2. Committed tree. A symlink, a gitlink (submodule), or a `.gitattributes` that declares
   `filter=lfs` is refused. Submodules and LFS objects are not materialized offline, so the
   exporter rejects them rather than sending incomplete content.
3. Working tree. Any tracked change or untracked file is refused by name. The status read uses
   `--no-optional-locks` so it does not rewrite the user's index.
4. Secret scan. Five patterns are searched in the tip tree of the branch: a private-key header,
   an AWS access key ID, a GitHub token, a Google API key, and a Slack token. A refusal names the
   path and the pattern, never the matched text. The scan covers the tip tree only (see the gaps).
5. Bundle. `git bundle create` writes `refs/heads/<branch>` with its full history.
6. Archive size. The archive must not exceed 200 MiB (see the limits table).
7. Worker intake. The archive is then run through the worker's own intake code
   (`extract_source` and `materialize_checkout` in `trusted_bundle.py`). The local verdict and the
   worker's verdict therefore share code and cannot diverge. `materialize_checkout` requires the
   claimed base SHA and refuses a bundle whose tip differs from it; the worker passes the base it
   was bootstrapped with, and export passes the base it just resolved.

### What is in the bundle

The bundle holds the branch's reachable committed history and nothing else. A bundle is a
history transfer: every commit reachable from the branch is sent, and the worker refuses a bundle
with prerequisites, so no partial history is possible. The bundle contains no ignored or untracked
files, no working-tree state, no HOME or configuration, and no remote URL, so credentials in a
remote URL never reach it.

The archive is a plain USTAR tar with one root member, `source.bundle`. Its mtime, mode, owner, and
names are fixed, so equal inputs produce equal bytes. The source digest is the SHA-256 of the
bundle bytes, and the archive hash and size are carried separately in `source_upload`.

Tests: `test_committed_branch_exports_as_one_verified_bundle`,
`test_archive_holds_one_root_member_named_source_bundle`, `test_history_is_included_in_the_bundle`,
`test_ignored_and_laptop_state_is_not_exported`, `test_remote_url_credentials_never_reach_the_bundle`,
`test_export_needs_no_network_even_when_the_remote_is_unreachable`,
`test_identical_repository_exports_identical_bytes`, and
`test_repository_path_with_shell_metacharacters_is_data`.

## Limits

| Limit | Value | Where it is set | Status |
| --- | --- | --- | --- |
| Source archive, compressed | 200 MiB | `SOURCE_LIMIT_BYTES` in `trusted_profiles.py`, set to `MAX_ADMITTED_SOURCE_BYTES` (200 MiB) in `protocol.py` | Enforced by the exporter and the worker intake alike |
| Unpacked tree | 1 GiB | `BundleLimits.max_unpacked_bytes` in `trusted_bundle.py` | Enforced by the worker intake. Decided by the project owner on 2026-10-22; the 2 GiB request in DPES-810 is superseded (see the gaps) |
| Tree entries | 50,000 | `BundleLimits.max_tree_entries` | Enforced by the worker intake |
| Path length | 1024 bytes | `BundleLimits.max_path_bytes` | Enforced by the worker intake |
| Source upload URL | 600 s | protocol-v1 capability table (input PUT) | Issued by the broker. The client reissues once on refusal |
| Result document | 1 MiB | `MAX_RESULT_BYTES` in `protocol.py` | Unchanged by this work |

The separate 200 MiB cap on returned work (a patch or changed-files bundle) is `MAX_RETURNED_WORK_BYTES`
in `protocol.py`. The result archive enforces it at build and again on fetch, as protocol-v1 section 15.6
records. It does not apply to the source archive or the result document.

Tests: `test_archive_over_the_source_limit_is_refused` and
`test_unpacked_content_over_the_limit_is_refused`.

## Digests

| Name | Definition | Verified by the worker |
| --- | --- | --- |
| `base_sha` | The commit at the branch tip, from `git rev-parse --verify` | Yes. Carried as `BootstrapSource.base_sha`; the bundle tip must equal it before any repository code runs (DPES-811 AC3) |
| `source_digest` | SHA-256 of the bundle bytes | No. Carried in the manifest only; the worker checks the archive hash and size, and the bundle through `git fsck` and checkout |
| `source_upload.sha256`, `size_bytes` | SHA-256 and size of the archive | Yes, against the bootstrap |
| `task_digest` | SHA-256 of canonical JSON `{"task_id", "prompt_id"}` | No. Client-defined, not in protocol-v1 |
| `config_digest` | SHA-256 of canonical JSON `{"harness", "model", "target", "thinking", "toolchain_profile"}` | No. Client-defined, not in protocol-v1 |
| `canonical_digest` | Protocol `manifest_digest` of the `CreateRunManifest` | Yes, the service checks it against the manifest |

Canonical JSON is UTF-8, keys sorted, with no whitespace. `task_digest` and `config_digest` are
defined here because protocol-v1 does not name them. The worker does not check them, so they are
recorded here as client-side bindings only.

## Local record and the step machine

Before the first network call, `submit` writes a record keyed by the request UUID. The record holds
the canonical digest, the create body, the archive hash and size, and the state reached so far. The
archive itself is stored beside it. Layout: `<state-dir>/<request uuid>/record.json` and
`source.tar`. Directories are owner-only (0700) and files are owner read-write (0600). Writes go
through a temporary file and `os.replace`, so a crash leaves either the old record or the new one.

A record never changes its digest. The same UUID with different content is refused locally and
sends nothing.

The record advances through these states, and each step is safe to repeat:

```text
planned --create--> created --PUT upload--> uploaded --start--> started
```

- `create` (`POST /v1/runs`): 201 for a new run, or 200 for a replay. A replay carries the upload
  capability while the run is still awaiting upload. A run already past upload returns no
  capability, and the record moves straight to `started`.
- `upload`: a signed PUT of the stored archive, with the broker's headers. A 200 returns the
  `x-goog-generation`, which must be present and numeric. The record stores it.
- `start` (`POST /v1/runs/{run_id}:start`): carries the canonical digest and a source receipt of
  generation, SHA-256, and size. A replay returns the recorded accepted receipt.

Before each attempt the stored archive is re-hashed against the record. A mismatch is refused and
sends nothing.

| Situation | What happens | Exit |
| --- | --- | --- |
| Create reply lost | The same request UUID replays the same run | 75, then resumes |
| PUT request lost, object never landed | Record stays `created`. Rerun repeats the PUT | 75, then resumes |
| PUT reply lost after the object landed | The retry gets 412. Refused as `upload_unknown`; the service offers no generation read in v1, so submit again with a new UUID | 1 |
| Upload URL refused (401 or 403) | One create replay asks for a fresh capability, then one more PUT. A second refusal stops without recording a refusal, so a later attempt can succeed | 1 (`upload_expired`) |
| Start reply lost | The start replays and returns the recorded receipt | 75, then resumes |
| Admissions paused (503), queue full (429), server fault (5xx) | Record kept | 75, then resumes |
| Create replayed after its window closed | Service answers `deadline_exceeded`. Recorded as refused | 1 |
| Other service refusal | Recorded with its code; a later run repeats the refusal without a call | 1 |
| Same UUID, different content | Refused locally by the record (`request_uuid_conflict`); nothing sent | 1 |
| Stored archive altered | `archive_mismatch`; nothing sent | 1 |
| Transport error (request or reply lost) | Record kept | 75, then resumes |

Tests: `test_record_is_durable_before_the_first_transport_call`,
`test_happy_path_starts_one_run`, `test_identical_resubmit_returns_the_same_run_without_network`,
`test_identical_request_after_losing_the_local_record_replays_the_same_run`,
`test_lost_create_response_recovers_through_replay`,
`test_lost_put_request_that_never_landed_recovers`,
`test_lost_put_reply_after_the_object_landed_refuses_then_a_new_uuid_starts`,
`test_lost_start_response_recovers_through_the_recorded_receipt`,
`test_expired_upload_url_is_reissued_once_then_succeeds`,
`test_upload_that_keeps_expiring_is_refused_after_one_reissue`,
`test_window_closed_before_resubmit_is_a_deadline_refusal`,
`test_admissions_paused_keeps_the_record_and_resumes_when_reopened`,
`test_same_request_uuid_with_different_content_is_refused_by_the_service`,
`test_tampered_local_archive_is_refused_before_any_network_call`, and
`test_changed_source_under_the_same_uuid_is_refused_locally`.

Input handling: `task_id` and the other flags are validated by the protocol models before any
record or network call. Invalid input, including shell metacharacters, is refused with the invalid
field names only (`test_task_id_with_shell_metacharacters_is_refused_before_any_call`).

## Gaps and blocking steps

| Gap | Status | Ticket |
| --- | --- | --- |
| Real Cloud Storage upload, and reading the object generation back from storage. The client checks only that the storage reply names a numeric generation. | Not proven | DPES-807 |
| A deployed control API. No transport ships, so `--execute` exits 2 in the CLI. | Not built | DPES-896, DPES-807 |
| Any proof against real GCP or a container daemon. | Not proven | DPES-804, DPES-807, DPES-819, DPES-871 |
| `doctor` checks only the source. Identity, API reachability, profile, model, config, and toolchain are not checked. | Partial | DPES-808 (identity), DPES-896 (API) |
| Unpacked tree limit is 1 GiB, the worker's limit. The 2 GiB request in DPES-810 is superseded. | Decided by the project owner on 2026-10-22. The limit stays at 1 GiB for the pilot. Raising it is a separate change to `BundleLimits` and the bootstrap profile together. | DPES-810 |
| Repositories that need package downloads at build or test time are not rejected. | Not built | no ticket identified; the requirement waits for an approved package-registry route |
| Secret scan covers the tip tree only. Branch history is transferred but not scanned. No approved scanner runs locally. | Partial | no ticket identified |
| `task_digest` and `config_digest` are client-defined and not verified by the worker. | Open; protocol decision | no ticket identified |
| `mcao` and `bin/fm-cloud.sh` wiring. Outside this change's scope. | Not in scope | no ticket identified |

Decisions needed from the project owner before activation:

- Whether a package-registry route is approved. Until it is, repositories that download packages at
  build or test time stay unsubmittable, and the rejection is not yet implemented.
- Whether `task_digest` and `config_digest` should move into protocol-v1 and be verified there.
