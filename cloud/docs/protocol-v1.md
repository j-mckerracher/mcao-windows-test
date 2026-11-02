# mao-cloud protocol v1

Maintainer contract for the versioned cloud execution protocol. It states what the
`mao_cloud` package enforces, which module owns each rule, and which rules are not yet
specified. The Python source is authoritative: `cloud/src/mao_cloud/protocol.py` defines
the wire models, the state machine, and the time, idempotency, object, cursor, screening,
and teardown rules. The JSON Schemas under `cloud/schemas/protocol-v1/` are generated from
the same models, and a drift check fails when they diverge.

Tests are cited by file and function name under `cloud/tests/`. A cited test proves the
rule it names; a rule with no citation is listed under [Gaps](#gaps).

## 1. Ownership

| Module | Owns |
| --- | --- |
| `protocol.py` | Wire models, enumerations, transition table, CAS, typed failures, idempotency decisions, time rules, object capabilities, cursors, client screening, redaction, canonical digests, teardown outcomes, field origins. |
| `config.py` | Server policy: target, harness, model, thinking, and toolchain allowlists, the timing bounds, the caller identity map, and the access mode of the policy file. |
| `broker.py` | Controller API (FastAPI routes), the `Broker` use cases, and the injected ports `Clock`, `RunStore`, `ObjectStore`, `Launcher`, plus `Ports` for owner and controller identity. |
| `runner.py` | Worker-side plans: claim, status, log entry, and result, each returned as a `PlannedWrite`. It performs no I/O. |
| `sweeper.py` | `plan_sweep` and `plan_sweep_page`: pure planners that return one `SweepAction` per run that needs one, sorted by run ID. The page form takes a limit and a cursor and returns `next_after`. Each action is a `liveness` transition or a `reconcile` step. |
| `documents.py`, `schemas.py` | Read and load JSON, parse instants, and render, export, and check the JSON Schemas. |
| `cli.py` | `mao-cloud schema export\|check` and `mao-cloud validate create\|start\|bootstrap`. Exit 0 on success, 1 on a refused document or schema drift, 2 when input cannot be read. argparse usage errors also exit 2. |
| `trusted_*.py` (DPES-811) | Worker-side trusted slice, section 19: claim, source intake, sandbox, supervision, profiles, and the attempt bootstrap. Runs inside the worker image and makes local process and git calls only. Imports the `protocol.py` and `runner.py` contracts and adds no second one. |
| `agents/pi.py` (DPES-813) | pi headless adapter, section 21: the approved argv, the model and thinking allowlists, the event-stream verdict, and the `PiWorker` run port. Imports the trusted slice and the replay classes and adds no second contract. |
| `results.py`, `result_paths.py`, `result_import.py` (DPES-817) | Result archive and its parts (section 15.6), the return-bundle header check, the checkpoint plan and commit, the destination-profile check, and the owned local import transaction. Imports `protocol.py`, `trusted_bundle.py`, and `storage.py`; adds no second fence, identity, or redaction rule. |

Console scripts are declared in `cloud/pyproject.toml`: `mao-cloud` (`cli.py`), `mao-cloud-broker` (`serve.py`, which builds and serves the `broker.py` app), `mao-cloud-runner` (`runner.py`), and `mao-cloud-sweeper` (`sweeper.py`).
The `runner` extra adds only `google-auth`; the `controller` extra carries the broker's packages.
`test_packaging.py` covers the script set, importable entry points, core and extra dependency sets, and agreement between `uv.lock` and `pyproject.toml`.
It also imports the CLI, runner, and sweeper with the controller packages blocked.
It builds no wheel or image, so it does not prove that a built artifact installs.

Controller, runner, and sweeper logic is pure over injected ports. No module in this package
calls Google Cloud. Real storage, launch, and identity adapters are outside this contract and
must satisfy the port docstrings in `broker.py`.

Clients address the controller only through the HTTP routes in [section 5](#5-http-api).
Owner identity comes from the verified token, through `Ports.owner_of` and `Ports.is_controller`;
a client never names its owner in a body. [Section 5](#caller-identity-and-access-modes) states
the caller rules.

## 2. Enumerations and wire constants

| Item | Value |
| --- | --- |
| Protocol version | `v1` only (`SUPPORTED_PROTOCOL_VERSIONS`). Any other value is `unsupported_version`. |
| Cursor version | `1` only (`SUPPORTED_CURSOR_VERSIONS`). |
| Harness | `pi` only. |
| Model | `gemini-3.8-flash` only. |
| Thinking | `low`, `medium`, `high`. |
| Resource class | `standard` only. |
| Attempts | `MAX_ATTEMPTS = 3`. |
| Idempotency lifetime | `IDEMPOTENCY_TTL = 7 days`. |
| Queued deadline | 1800 s. The run times out if it is still queued after this. |
| Upload window | 600 s (`UPLOAD_WINDOW_SECONDS`), capped by the run deadline. |
| Startup timeout | 600 s default (`STARTUP_TIMEOUT_SECONDS`); policy may set it to 1 s or more. |
| Runtime budget | 60 s to 7200 s (`MIN_RUNTIME_SECONDS`, `MAX_RUNTIME_SECONDS`). |
| Claim TTL | 1200 s. Claim objects are at most 4096 bytes. |
| Input read TTL | 1200 s. |
| Attempt capability TTL | 10800 s (status, checkpoint, log, result, control). |
| Source upload | at most 209715200 bytes (200 MiB) admitted; the schema bound is 536870912 bytes (512 MiB). `application/x-tar`. |
| Checkpoint harness state | at most 67108864 bytes (64 MiB), checked before upload. |
| Checkpoint workspace | at most 1073741824 bytes (1 GiB), checked before upload. |
| Result | at most 1048576 bytes (1 MiB), the result document. |
| Returned artifact (patch or bundle) | at most 209715200 bytes (200 MiB) each. Separate from the result document (section 15.5 item 8). |
| Status document | at most 65536 bytes. Status message at most 512 characters. |
| Log object | at most 1048576 bytes. At most 500 lines per entry, 4096 characters per line. |
| Log page limit | 1 to 500 entries (`MAX_LOG_LIMIT`), default 100. An out-of-range or non-integer limit is `invalid_request`. |
| Run ID | `^run_[0-9a-f]{32}$`. |
| Claim ID | `^claim_[0-9a-f]{32}$`. |
| Runner nonce | `^[0-9a-f]{32}$`. Runner-written on the claim document only; not an identity field (section 19.1). |

Wire models are Pydantic v2 models that are frozen and set `extra="forbid"`. An unknown
field is refused as `unknown_field`, not ignored. Test: `test_schemas.py` (schema export and
drift), `test_screening.py::test_request_refuses_every_client_controlled_top_level_key`.

Request models are `CreateRunRequest`, `StartRunRequest`, `RetryRequest`, and `ActionRequest`
(cancel). Document models are `AcceptedReceipt`, `WorkerBootstrap`, `ClaimDocument`, `StatusReport`,
`RunResultDocument`, `RunView`, `StatusView`, `LogEntry`, `LogPage`, `CreateRunResponse`,
`TeardownStatus`, and their nested models such as `Budget` and `ToolCompatibility`. The
`ResourceClass` type is the literal `standard`. The 21 schemas in `cloud/schemas/protocol-v1/`
are the export of `SCHEMA_MODELS`.

## 3. Run states and transitions

The states are `awaiting_upload`, `queued`, `dispatching`, `starting`, `running`,
`collecting`, `reconciling`, `cancel_requested`, `succeeded`, `failed`, `timed_out`, and
`cancelled`. `succeeded`, `failed`, `timed_out`, and `cancelled` are terminal.

`ALLOWED_TRANSITIONS` is the complete edge set for one attempt:

| From | To |
| --- | --- |
| `awaiting_upload` | `queued`, `cancelled`, `timed_out` |
| `queued` | `dispatching`, `cancelled`, `timed_out` |
| `dispatching` | `starting`, `reconciling`, `cancel_requested`, `failed` |
| `starting` | `running`, `reconciling`, `collecting`, `cancel_requested`, `failed` |
| `running` | `collecting`, `reconciling`, `cancel_requested`, `failed` |
| `collecting` | `succeeded`, `reconciling`, `cancel_requested`, `failed` |
| `reconciling` | `starting`, `running`, `collecting`, `cancel_requested`, `failed` |
| `cancel_requested` | `cancelled`, `failed`, `timed_out` |
| `succeeded`, `failed`, `timed_out`, `cancelled` | none |

Properties checked by the table tests (`test_state_machine.py`):
every state has an entry; terminal states have no outgoing edge; no state transitions to
itself; every non-terminal state can reach a terminal state.

**An owner cancel completes only on confirmed termination.** An owner cancel of a claimed,
non-terminal run moves it to `cancel_requested` with `owner_cancelled`. The run reaches
`cancelled` only when the launch adapter confirms that every execution for the run, attempt,
fence, and claim has stopped (`Launcher.execution_stopped`). Neither the clock nor worker silence
completes it. Until confirmation, the run keeps its worker slot and daily reservation, and each
reconcile or cancel call repeats the idempotent platform stop request (`Launcher.request_stop`),
so a failed request is retried on the next call. The first call that opens the cancel also writes
the attempt's `cancel.json` marker (section 8), create-only. A run still in `awaiting_upload` or
`queued` is cancelled at once and never launches, so it gets neither a stop request nor a marker.
A cancel opened by the deadline (`deadline_exceeded`) is not an owner cancel. Its run times out or
fails under the rules in `liveness_decision` (`protocol.py`), and it still holds its capacity until
the stop is confirmed.
Tests: `test_cancel_reconcile.py`.

**Retry is not an edge.** `retry()` is a per-run operation. It is valid only when the run is
`failed`, the failure is retryable, the attempt budget is not exhausted, and the original
deadline has not passed. It opens attempt N+1 in `queued`, assigns a new fence, keeps the
original `deadline_at`, and records the retry UUID in `retry_request_uuids`. Because
`failed` has no outgoing edge, the table cannot express retry, and the state-machine tests
exercise `retry()` directly.

**Retry needs released capacity.** A retryable `launch_uncertain` failure is not enough by itself.
`retry()` also refuses while the attempt still holds `reserved_seconds` (`retry_not_allowed`), so a
failed attempt whose execution may still be live cannot open attempt N+1 or launch a second
execution. The reservation is released only when the launch adapter confirms absence or
termination (see [Shared admission and execution capacity](#shared-admission-and-execution-capacity)).
A failed state alone is not proof that execution has stopped. Tests: `test_unknown_launch_fence.py`.

### Compare-and-set order

`transition()` and `retry()` check, in this order:

1. `expected_attempt` against the current attempt. A mismatch is `stale_attempt`.
2. `expected_version` against the current version. A mismatch is `version_conflict`.
3. For `transition()`: a terminal source is `terminal_state`, and a source with no edge to the target is `illegal_transition`.
4. For `retry()`, in order: the run must be `failed` with a recorded failure (`retry_not_allowed`); the attempt must hold no capacity reservation (`retry_not_allowed`); the failure must be retryable (`retry_not_allowed`); the original deadline must not have passed (`deadline_exceeded`); and `attempt < max_attempts` (`retry_not_allowed`).

`transition()` also rejects a failure attached to a target other than a terminal state or
`cancel_requested` with a `ValueError`. A transition to any other state clears the recorded
failure. A transition to a terminal state or `cancel_requested` keeps the recorded failure
unless a new one is supplied.

Tests: `test_state_machine.py::test_wrong_attempt_on_a_terminal_run_is_stale_attempt_first`,
`test_wrong_version_on_a_terminal_run_is_version_conflict`,
`test_matching_terminal_run_is_terminal_state`,
`test_matching_non_terminal_run_off_the_table_is_illegal_transition`.
A successful transition bumps the version and stamps the entry time.

### Typed failures

A failure carries a `FailureCode`, a `retryable` flag, and a redacted message truncated to
512 characters. `retryable` is derived from the `RETRYABLE` table and is never accepted from
a caller. A flag that contradicts the table is rejected at construction.

| Code | Retryable |
| --- | --- |
| `startup_timeout` | yes |
| `heartbeat_lost` | yes |
| `launch_uncertain` | yes |
| `worker_failed` | yes |
| `upload_expired` | no |
| `queued_expired` | no |
| `deadline_exceeded` | no |
| `owner_cancelled` | no |
| `checksum_mismatch` | no |
| `missing_result` | no |

`missing_result` is set when the execution has stopped while the attempt is still `collecting`
and no valid result object exists for that attempt. The controller cannot verify output it never
received, so replaying the attempt would only spend budget. An execution that is still live keeps
the run `collecting`. A launcher error while asking leaves the run unchanged rather than failing
it. An artifact that is present but has the wrong size or checksum is still `checksum_mismatch`
(`test_missing_result.py`).

A failure carried through `cancel_requested` survives to `cancelled`. Tests: the
`test_state_machine.py` failure tests, including `test_retryable_follows_the_failure_code_table`,
`test_failure_survives_cancel_requested_to_cancelled`, and `test_failure_message_is_redacted`.

## 4. Typed errors

Every refusal is a `ProtocolError` with an `ErrorCode`, a message, and optional detail. The
HTTP status is taken from `HTTP_STATUS`.

| Status | Codes |
| --- | --- |
| 400 | `invalid_request`, `unsupported_version`, `unknown_field`, `client_controlled_field`, `blocking_field`, `digest_mismatch`, `cursor_invalid`, `cursor_version_unsupported` |
| 403 | `not_allowed` |
| 404 | `not_found` |
| 409 | `request_digest_conflict`, `illegal_transition`, `terminal_state`, `version_conflict`, `stale_attempt`, `sequence_regressed`, `retry_not_allowed`, `deadline_exceeded`, `source_mismatch`, `upload_mismatch`, `checksum_mismatch`, `bootstrap_mismatch`, `result_fenced`, `result_not_ready` |
| 410 | `idempotency_expired` |
| 429 | `queue_full` |
| 503 | `admissions_paused` |

`BLOCKING` fields raise `blocking_field` with the ticket named in the message. Test:
`test_schemas.py::test_blocking_markers_are_exactly_the_ones_the_brief_names`.

Another owner's run is reported as `not_found`, the same refusal as a missing run, so an
owner cannot probe for other run IDs (`broker.py::_owned`;
`test_broker_lifecycle.py::test_owner_cannot_see_another_owners_run`).

## 5. HTTP API

| Method and path | Purpose | Notes |
| --- | --- | --- |
| `POST /v1/runs` | Create a run from a manifest. | Idempotent on (owner, request UUID). Digest checked by `parse_create_request`. |
| `POST /v1/runs/{run_id}:start` | Bind the verified source upload and queue the run. | Replay returns the recorded receipt. |
| `POST /v1/runs/{run_id}:cancel` | Request cancellation. | `awaiting_upload` or `queued` becomes `cancelled` at once. Any other non-terminal state becomes `cancel_requested` with an `owner_cancelled` failure, writes the attempt's `cancel.json` marker, and requests a platform stop. The run reaches `cancelled` only after the stop is confirmed (section 3). Repeated cancel is idempotent. A terminal run's state is unchanged, but the call still repeats the stop request while a terminal attempt holds its capacity. |
| `POST /v1/runs/{run_id}:retry` | Open attempt N+1 for a retryable failure. | Idempotent on (owner, request UUID). Body carries `expected_attempt` and `expected_version`. |
| `GET /v1/runs/{run_id}` | Owner-filtered run view. | Server-derived fields only. |
| `GET /v1/runs/{run_id}/status` | Status snapshot. | A snapshot, never a history. No cursor. |
| `GET /v1/runs/{run_id}/logs` | Log page. | Cursor and `limit`. See [section 7](#7-cursors-status-and-logs). |
| `GET /v1/runs/{run_id}/result` | Result document. | `result_not_ready` until the run is terminal. |
| `POST /internal/v1/runs/{run_id}:reconcile` | Controller reconcile of one run. | Requires `Ports.is_controller`. Not a client route. |
| `POST /internal/v1/runs:reconcile` | Controller batch reconcile, one bounded page of due runs. | Requires `Ports.is_controller` (operator or scheduler). No body. Query `limit` (1 to 50, default 50) and `after` (a run id cursor, from the prior `next_after`). Returns `{protocol_version, reconciled, next_after}`; each entry names its run and reports `reconciled`, `conflict`, `not_found` or `refused`. Section 20. Not a client route. |

Every request needs an owner identity. A request with none is refused as `not_allowed`
(`test_broker_lifecycle.py::test_missing_owner_identity_is_refused`).
Reconcile without controller identity is refused
(`test_internal_reconcile_requires_controller_identity`). The batch route refuses the same way
(`test_reconcile_batch.py::test_a_caller_without_the_controller_identity_is_refused_and_no_run_moves`).

### Caller identity and access modes

`BearerIdentity` in `broker.py` resolves a caller from the bearer token in `Authorization`. google-auth
verifies the signature against Google's published certificates and checks the issuer, the audience
(`api_audience`), and the expiry. Cloud Run's invoker check reads `X-Serverless-Authorization`, which
carries the same token. The broker never identifies a caller from that header. A token that fails
verification resolves to no caller and is audited as `token_invalid` or `verifier_unavailable`.

A verified subject is resolved under the policy's `access_mode` ([section 14](#14-server-policy)). The
access mode and the identity map are read together from one snapshot per request, so a request never
combines the mode of one policy with the map of another.

| Verified subject | `allowlist` (default) | `mayo-wide` |
| --- | --- | --- |
| In the identity map | Admitted when the token email equals the provisioned email and `email_verified` is true. The identity's role applies. | Same rule. |
| Not in the identity map | Refused as `unknown_subject`. | Admitted as an owner when the subject is 1 to 30 ASCII digits, `email_verified` is true, and the email is exactly `<local>@mayo.edu`. Otherwise refused as `invalid_subject`, `email_unverified`, or `not_mayo_email`. |

A subject in the identity map never falls through to the open rule. A provisioned identity whose email
is mismatched or unverified is refused in either mode.

**Derived owner.** A mayo-wide caller's owner is its own subject, validated as 1 to 30 ASCII digits, and its role is always owner.
Provisioned owner ids start with a letter, so a derived owner cannot equal a provisioned owner. A derived caller is never an operator or a scheduler, so it cannot reach
reconciliation. The owner always comes from the token. A body that names an owner is refused and
creates nothing (`auth_mayo_wide_test.py::test_the_owner_comes_from_the_token_so_a_body_owner_is_refused_and_creates_nothing`).

**Exact domain.** `is_mayo_email` admits only `<local>@mayo.edu`. The local part is lowercase letters,
digits, and `._+-`, up to 64 characters. Subdomains (`cdh.mayo.edu`), lookalike domains (`evilmayo.edu`,
`mayo.edu.example.test`), a trailing dot, uppercase letters, and a second `@` are refused.
Test: `auth_policy_test.py::test_is_mayo_email_refuses_every_other_address`.

**Mapped-grant removal and derived access.** The access mode and identity map are read on every request.
Removing an identity removes its mapped grant on the next request, but does not revoke derived-owner access while `mayo-wide` remains enabled.
A now-unmapped subject with a qualifying verified Mayo email can create new subject-owned runs and access its earlier subject-owned runs.
It cannot access runs owned by its former provisioned owner ID.
Closing `mayo-wide` removes this fallback on the next request for all unmapped callers.
There is no per-caller denylist for derived access.
An upstream change can prevent new qualifying credentials, but does not invalidate an already issued token before its expiry.
The policy is:

- No denylist, and no immediate invalidation of issued tokens. An issued token keeps verifying until its
  own expiry, but it grants nothing once its subject is refused.
- A caller refused by both the current identity map and the derived-owner rule is refused on every owner route, reads included. Reads are not split from mutations
  because an owner-scoped read is a disclosure, so it stops with the grant. An accepted decision stays
  on its run record and is not replayed to a revoked caller
  (`auth_mayo_wide_test.py::test_closing_the_mode_refuses_a_mayo_owner_on_every_owner_route_and_its_run_is_unchanged`).
- Revocation never changes a run. Closing the mode cancels nothing and rebinds nothing. Reopening it
  restores the same owner's access to the same run
  (`auth_mayo_wide_test.py::test_reopening_the_mode_restores_the_mayo_owner_its_same_run`).
- Queued and running work continues under the scheduler, a separately provisioned identity that can only
  reconcile (`auth_mayo_wide_test.py::test_a_queued_mayo_run_keeps_progressing_under_the_scheduler_after_the_mode_closes`).
- A run's `owner_id` is fixed at creation. A subject later remapped to another owner gets the same
  answer as a missing run. A subject later remapped to the scheduler can reconcile but owns nothing
  (`auth_mayo_wide_test.py::test_a_mayo_subject_later_mapped_to_another_owner_cannot_reach_its_derived_run`,
  `auth_mayo_wide_test.py::test_a_mayo_subject_later_mapped_as_scheduler_reconciles_but_owns_nothing`).

**Audit.** `mao_cloud.audit` writes one line per decision: `accepted` with the subject, role, and owner,
or `refused` with one reason: `no_credential`, `token_invalid`, `verifier_unavailable`,
`email_mismatch`, `email_unverified`, `unknown_subject`, `invalid_subject`, or `not_mayo_email`. No line carries a token,
signature, email, or claim (`auth_mayo_wide_test.py::test_mayo_admission_and_refusal_are_audited_by_subject_without_email_or_token`).

Tests: `auth_identity_test.py`, `auth_mayo_wide_test.py`, `auth_policy_test.py`, `auth_revocation_test.py`,
`auth_refresh_test.py`, `auth_routes_test.py`.

## 6. Idempotency

### Keys and digests

A dedup record is keyed by (owner, request UUID). The request UUID must be a UUIDv7 whose
embedded millisecond timestamp is the request time (`test_idempotency.py`:
`test_uuid7_timestamp_round_trips_the_stamped_instant`,
`test_non_v7_request_uuid_is_invalid_request`).

- A create's digest is the canonical manifest digest.
- A retry's digest is the action digest over `run_id`, `action`, `expected_attempt`, and `expected_version`.

Each record expires `IDEMPOTENCY_TTL` after creation (`test_new_record_expires_seven_days_after_creation`).

### Admission decision

`decide_admission` evaluates in this order. A refusal never creates anything.

1. **Live record exists.** A different digest is `request_digest_conflict` (409), even when the record has expired (`test_different_digest_conflicts_even_when_the_record_has_expired`). An identical digest past `expires_at` is `idempotency_expired` with reason `ttl` (410). An identical digest before expiry is `REPLAY`.
2. **No record, but the request UUID already produced a run** (an orphan, detected through `find_run_by_request` for creates or `retry_request_uuids` for retries). The request is refused as `idempotency_expired` with reason `record_missing_run_present` (410). No new run is created (`test_orphan_run_without_a_dedup_record_is_refused_not_recreated`).
3. **No record and no orphan.** The UUID timestamp must not be more than `skew_seconds` ahead of server time (`invalid_request`). It must not be earlier than the admission floor (`idempotency_expired`, reason `before_admission_floor`). Otherwise the request is admitted.

### Admission floor and restore

The admission floor is `now - IDEMPOTENCY_TTL`, raised to the restore floor when a restore has
completed. A UUID stamped before the floor is expired even if its record is still inside the
TTL (`test_request_before_a_restore_floor_is_expired_even_inside_the_ttl`). A UUID stamped at
or after the floor is admitted (`test_restore_floor_does_not_block_a_request_stamped_after_it`).
An earlier restore floor never lowers the window (`test_an_earlier_restore_floor_does_not_lower_the_window`).

### Replay outcomes

- **Create replay** returns the existing run and its upload descriptor (`test_create_replay_returns_existing_run_with_its_upload`). A changed manifest with the same UUID conflicts (`test_create_with_changed_digest_conflicts`). Replay after seven days is expired (`test_create_replay_after_seven_days_is_expired`). Replay with a lost dedup record is refused and not recreated (`test_create_replay_with_lost_dedup_record_is_refused_not_recreated`).
- **Start replay** returns the recorded receipt (`test_start_replay_returns_the_same_receipt`; `test_state_machine.py::test_replay_start_returns_the_recorded_receipt`). A different digest conflicts. A different generation or different bytes is `source_mismatch`.
- **Retry replay** returns the current run view and opens no attempt. The retry digest is `action_digest` over `run_id`, `action="retry"`, and the body `{expected_attempt, expected_version}`. The retry's dedup record is stored under its own (owner, request UUID) key. It never replaces the create record of the run it acts on, as `RunStore.compare_and_set` requires in `broker.py`. Tests: `test_broker_api.py::test_retry_replay_with_the_same_request_uuid_does_not_open_another_attempt`, `test_create_replay_after_a_retry_still_returns_the_same_run`, and `test_retry_reusing_a_request_uuid_with_a_different_body_is_a_digest_conflict`.

Restore from backup is modelled by the restore floor only. No test restores a snapshot that
still holds dedup rows (see [Gaps](#gaps)).

## 7. Cursors, status, and logs

- Only logs are paged. Status is a snapshot with no cursor (`CursorStream` docstring: "Only logs are paged by cursor. Status is a snapshot, never a history.").
- `GET .../status` returns the run view, an `effective` block (`model`, `resource_class`, and the server-resolved `image_digest` pinned at admission), `result_available`, and the latest accepted `status_seq` and `log_watermark`. `result_available` is true only for an accepted result (`worker_failed`, `checksum_mismatch`, or succeeded) on the current attempt. It does not promise the artifacts can be downloaded. `GET .../result` verifies them. A `missing_result` failure reports `result_available: false`. Checkpoint availability is not in this view; its wire field is BLOCKING on DPES-869 (`WorkerBootstrap`).
- Follow semantics are client-side (`mao_cloud.log_follow.follow_logs`). The engine fetches one page at a time with the caller's `limit` (1 to 500). It yields entries in sequence and drops any seq it has already yielded, so a replayed page cannot duplicate an entry. A gap is yielded once, covering only the range the follower never saw. Empty polls back off from 1s, doubling, capped at 30s, and reset when entries arrive. A transient fetch failure is retried on the same schedule and surfaces after `max_failures`. A non-retryable `ProtocolError` (for example `cursor_invalid`) surfaces immediately. The run ends with an end event once it is terminal and every durable entry has been yielded. Tests: `test_log_follow.py`. The HTTP transport and the `mao-cloud logs --follow` command are not wired yet (DPES-804 and DPES-807).
- Child output for pi (`message_end`, `tool_execution_start`, `tool_execution_end`, `agent_end`) is mapped by `mao_cloud.pi_events.map_pi_lines`. Only a fixed whitelist is kept: the event type, a safe tool name, and the error boolean. Payload, arguments, results, unknown types, and lines over 1 MiB are dropped and counted. The four event names are the ones this repo's pi extension hooks already reference. No pinned pi event schema was checked, so the field shapes (`toolName`, `isError`) are unverified against the pinned pi version. The runner does not call this mapping yet (DPES-807).
- A cursor is a base64url token without padding over canonical JSON with keys `v` (version), `r` (run), `a` (attempt), `s` (stream), and `q` (sequence). It is deterministic (`test_cursors.py::test_encoding_is_deterministic`, `test_token_payload_is_canonical_json_with_short_keys`).
- A cursor names one attempt. After a retry, a cursor from the previous attempt is refused as `cursor_invalid` with the previous and current attempt in the detail. Log object names include the attempt, so sequence numbers are per attempt (`object_name` in section 8). The refusal is covered by `test_broker_api.py::test_cursor_from_a_previous_attempt_is_refused_after_retry`.
- A cursor for another run is `cursor_invalid` (`test_cursor_for_another_run_is_invalid`).
- The page is bounded by the durable watermark. A cursor at watermark + 1 yields an empty page that stays put. A cursor beyond that is `cursor_invalid` with the watermark in the detail (`test_cursor_beyond_the_watermark_plus_one_is_invalid`).
- Trimmed history is disclosed, not hidden. A cursor below the first retained sequence returns the page from the first retained entry and reports the gap as `(requested, first_retained - 1)` (`plan_page`; `test_cursor_into_trimmed_history_discloses_the_gap`). A cursor at the first retained entry is not trimmed (`test_cursor_at_the_first_retained_entry_is_not_trimmed`).
- An unsupported cursor version is `cursor_version_unsupported` and names the version. The run remains readable (`test_broker_api.py::test_unsupported_cursor_version_is_refused_and_run_stays_readable`).
- `limit` must be an integer from 1 to 500. Otherwise the request is `invalid_request` (`test_limit_outside_the_allowed_range_is_refused`, `test_limit_at_the_maximum_is_accepted`).

## 8. Object names, capabilities, and headers

### Names

Object names are derived by `object_name` and `artifact_object_name`. Both raise `ValueError` for a
run_id outside the protocol pattern, and `object_name` also raises for attempt 0 or a missing attempt on an attempt-scoped kind. `object_name` raises for `checkpoint` as well: checkpoint components are named by `checkpoint_object_names`. Run-scoped objects carry no attempt. Attempt-scoped objects carry a six-digit zero-padded attempt number. Log objects carry a ten-digit zero-padded sequence. Checkpoint objects carry no sequence: a checkpoint id (`CHECKPOINT_ID_PATTERN`, `^ck-[0-9]{4,12}$`) names one directory that holds exactly three components (`test_objects.py`:
`test_attempt_number_is_zero_padded_to_six_digits`,
`test_checkpoint_objects_are_never_named_by_sequence`,
`test_log_object_is_sequenced_and_zero_padded`).

| Kind | Name shape |
| --- | --- |
| input | `runs/{run_id}/input/source.tar` |
| claim | `runs/{run_id}/attempts/{attempt:06}/claim.json` |
| control | `runs/{run_id}/attempts/{attempt:06}/control.json` |
| cancel | `runs/{run_id}/attempts/{attempt:06}/cancel.json` |
| status | `runs/{run_id}/attempts/{attempt:06}/status.json` |
| checkpoint | `runs/{run_id}/attempts/{attempt:06}/checkpoints/{checkpoint_id}/` holding `harness.bin`, `workspace.tar.gz`, and `manifest.json`, named by `checkpoint_object_names` |
| log | `runs/{run_id}/attempts/{attempt:06}/logs/{seq:010}.ndjson` |
| result | `runs/{run_id}/attempts/{attempt:06}/result.json` |
| artifact | attempt-scoped, named by `artifact_object_name`; at most 209715200 bytes (200 MiB) each |

The cancel marker is written only by the controller, through the object store port, and only
once per attempt: a create-only write, so a retried write is a no-op and the bytes are never
replaced. Its `CancelDocument` names the attempt, fence, and claim it stops, and it grants the
runner read access alone (`CAPABILITIES[(cancel, GET)]`). The worker-side gate
(`mao_cloud.worker_control`) checks it before each model call, but no production reader or in-flight
poll exists yet (gap 36). Until the runner loop exists, the platform stop request is what stops a
worker.

A retry therefore never reads an earlier attempt's bytes (`artifact_object_name` docstring;
`test_artifact_object_is_scoped_to_its_attempt`). Attempt 0 and malformed run IDs are refused
(`test_attempt_zero_is_refused`, `test_malformed_run_id_is_refused`).

### Capabilities

`CAPABILITIES` is the complete table of signed-object capabilities, keyed by (kind, method).

| Kind | Method | Holder | TTL | Precondition | Max bytes | Content type | sha256 metadata | Redacted | Blocking |
| --- | --- | --- | --- | --- | --- | --- | --- | --- | --- |
| input | PUT | client | 600 s | create_only | 200 MiB admitted (512 MiB schema bound) | `application/x-tar` | yes | no | none |
| input | GET | runner | 1200 s | none | | | no | no | none |
| claim | PUT | runner | 1200 s | create_only | 4096 | `application/json` | no | no | none |
| control | GET | runner | 10800 s | none | | | no | no | none |
| cancel | GET | runner | 10800 s | none | | | no | no | none |
| status | PUT | runner | 10800 s | generation_match | 65536 | `application/json` | no | yes | none |
| checkpoint | PUT | runner | 10800 s | create_only | 64 MiB harness; 1 GiB workspace | `application/octet-stream` | yes | no | DPES-869 |
| log | PUT | runner | 10800 s | create_only | 1 MiB | `application/x-ndjson` | no | yes | none |
| result | PUT | runner | 10800 s | create_only | 1 MiB | `application/json` | yes | yes | none |

Properties checked by `test_objects.py`: only the input upload is held by the client
(`test_only_the_input_upload_is_held_by_the_client`); every upload has a size bound and a content
type; the checksum-bound uploads are exactly input, checkpoint, and result
(`test_checksum_bound_uploads_are_exactly_input_checkpoint_and_result`); reads carry no
precondition or content type (`test_reads_carry_no_precondition_or_content_type`).

### Expiry

A capability expires after its holder TTL, clamped to the attempt deadline. No capability is
issued after the attempt deadline (`test_capability_expires_after_its_holder_ttl`,
`test_capability_expiry_is_clamped_to_the_attempt_deadline`,
`test_no_capability_is_issued_after_the_attempt_deadline`).

### Signing

Only `SignerService` (`cloud/src/mao_cloud/signer.py`) signs capabilities. The controller holds no
signing method: it sends a `CapabilityRequest` under its own identity, and the service signs only
when the request matches accepted run state. The checks run in this order, and each failure is a
`RefusalCode` whose message names no run data:

1. the caller is the controller identity;
2. the (kind, method) pair exists in `CAPABILITIES`;
3. the run exists and belongs to the requested owner;
4. the attempt, fence, and claim identity match the accepted run, with stale values refused and
   future values treated as forged;
5. the object name is the one the protocol derives for that run and attempt: for a checkpoint PUT, one of the three components of the `checkpoint_id` the request carries (`CapabilityRequest.checkpoint_id`), and otherwise the one derived from the sequence;
6. the run state is one the capability allows (a pair with no entry is refused);
7. the capability is not blocked (checkpoint PUT is blocked by DPES-869);
8. the attempt deadline has not passed;
9. the requested expiry is in the future and no later than the holder TTL clamped to the deadline;
10. the preconditions and checksum headers match the protocol.

Only then does the backend sign.
The controller reaches the service through `HttpSignerClient` (`cloud/src/mao_cloud/signer_client.py`): one HTTPS call per capability to the configured signer origin.
It presents the same Google ID token, with that origin as its audience, in `Authorization` and `X-Serverless-Authorization`.
Cloud Run checks the invoker header while the signer service verifies `Authorization`.
Redirects are not followed.
The signer service (`mao_cloud.signer_serve`) signs through IAM V4 signed URLs (`gcs_sdk.py`), so it holds no private key.
A refusal keeps the signer's code.
A token that cannot be fetched sends no request, so nothing is signed; the caller still receives the uniform `UNAVAILABLE` refusal.
Any other transport or answer-shape failure after a request was sent is also `UNAVAILABLE`, which leaves the signing outcome unknown to the caller.
Signing may have succeeded before a timeout or unusable response.
Production wires the client at `MAO_CLOUD_SIGNER_URL`.
The deployed signer, its IAM, and the live call are unverified (DPES-899).
Tests (fakes only): `cloud/tests/test_signer.py`, `test_signer_serve.py`, `test_signer_client.py`,
`test_gcs_signing.py`, and `test_signer_image.py`. `test_signer.py` includes
`"sign_put" not in dir(ObjectStore)`, which fails if the controller regains a signing method.

### Headers

- `create_only` sets `x-goog-if-generation-match: 0`.
- `generation_match` sets `x-goog-if-generation-match` to the expected generation. Without an expected generation, `required_headers` raises `ValueError` (`test_status_upload_without_an_expected_generation_is_refused`).
- Checksum-bound uploads set `x-goog-meta-sha256`. Without a checksum, `required_headers` raises `ValueError`.
- Reads set neither precondition nor content type.

### Checksums

Result and artifact checksums are verified with a constant-time comparison. A missing artifact
is `checksum_mismatch` (`test_missing_artifact_is_a_checksum_mismatch`). Artifacts that match
their references verify; a result with no artifacts verifies trivially
(`test_artifacts_that_match_their_references_verify`,
`test_a_result_with_no_artifacts_verifies_trivially`). The result write carries its checksum and
its create-only precondition (`test_broker_lifecycle.py::test_result_write_carries_its_checksum_and_create_only_precondition`).

Signed URLs are redacted from any document text (`test_objects.py`, the signed-URL redaction
test near line 335, and the `X-Goog-Signature` rule in [section 9](#9-screening-and-redaction)).

## 9. Screening and redaction

### Requests

`parse_request` runs in this order:

1. The body must be a JSON object (`invalid_request`).
2. `protocol_version` must be present (`invalid_request`) and supported (`unsupported_version`).
3. **Client-controlled screen.** Every key at any depth, including keys inside lists, is compared with `CLIENT_CONTROLLED_FIELDS`. A match is `client_controlled_field`, with the dotted path as detail. Every string at any depth is then checked for credential material (`redact()` changes it) and for local paths. Either is `client_controlled_field`.
4. Strict Pydantic validation. An unknown key is `unknown_field`; other shape errors are `invalid_request`.
5. For a create, the canonical digest must equal the digest of the manifest (`digest_mismatch`).

The screen runs before strict validation, so a client-controlled name is reported as such even
when it is also unknown (`test_screening.py::test_the_screen_runs_before_strict_validation`).

`CLIENT_CONTROLLED_FIELDS` includes identity (`owner`, `owner_id`, `actor`, `principal`,
`server_id`, `run_id`, `attempt`, `attempt_number`, `fence`, `fence_token`, `claim`, `claim_id`,
`claim_nonce`), control (`control`), images (`image`, `image_ref`, `image_digest`,
`container_image`), identity-bearing service accounts (`service_account`,
`service_account_email`, `runtime_identity`), secrets and credentials (`secret`, `secrets`,
`secret_name`, `secret_ref`, `credential`, `credentials`, `token`, `access_token`, `id_token`),
URLs (`signed_url`, `upload_url`, `download_url`, `url`), paths and commands (`local_path`,
`path`, `argv`, `command`, `env`, `environment`), and raw bytes (`source_bytes`,
`source_content`, `body`).

Local paths are strings that start with `/`, `~/`, or `file:`. Credential material is any
string that matches a redaction rule in [section 9, redaction rules](#redaction-rules). Tests:
`test_screening.py`, which includes the credential samples in `CREDENTIAL_SAMPLES` (Bearer,
password assignment, Google OAuth token, and a signed URL with `X-Goog-Signature`) and the
local-path samples in `LOCAL_PATH_SAMPLES`. A relative path such as `wf/805/relative` is accepted.

Request models never declare a client-controlled name, so a client can always set each field
the model declares (`test_no_request_field_is_a_name_the_screen_refuses`). Every request field
has origin `client-manifest` (`test_every_request_field_is_client_manifest_origin`).

### Documents

`parse_document` runs for runner- and controller-written documents. It checks the version, then
refuses only top-level keys in `DOCUMENT_FORBIDDEN_FIELDS` (client-controlled names minus
`IDENTITY_FIELDS`, which are `run_id`, `attempt`, `fence`, and `claim_id`). Identity fields are
legitimate in documents. Nested keys are governed by each model's `extra="forbid"`, because a
controller document may carry a name such as `image_digest` inside its pinned compatibility
tuple (`test_document_permits_a_forbidden_name_nested_in_a_pinned_tuple`).

Status messages, failure messages, and log lines are redacted, not refused. The redaction runs
at the model boundary, so storage never holds the credential text
(`test_a_document_redacts_credential_text_instead_of_refusing_it`). Local-path text in a
document is kept as written (`test_a_document_keeps_local_path_text_as_written`).

### Redaction rules

`REDACTION_RULES` apply in order: signed URLs with `X-Goog-Signature`, PEM private keys, `Bearer`
tokens, `ya29.` OAuth tokens, JWTs beginning `eyJ`, and `password|passwd|secret|token|api_key`
assignments. `redact()` is idempotent: `redact(redact(x).text).text == redact(x).text`.

### Field origins

`FIELD_ORIGINS` records who may set each field of each model. The four origins are
`client-manifest`, `server-derived`, `controller-assigned`, and `runner-reported`. Identity fields
on documents are `controller-assigned`, and `RunView` fields are `server-derived`. Tests in
`test_screening.py` enforce the two properties that matter for requests: no request field
has a name the screen refuses, and no identity field is ever `client-manifest`. The origins are
not emitted into the JSON Schemas (see [Gaps](#gaps)).

## 10. Canonical JSON and digests

`canonical_bytes` uses sorted keys, compact separators, UTF-8 without ASCII escaping, and
rejects NaN and infinity.

- `manifest_digest` is the SHA-256 of the canonical manifest bytes with `None` values excluded. It does not remove `canonical_digest`, because that field is on the request, not the manifest model. `parse_create_request` builds the manifest by dropping `canonical_digest` from the request, then refuses a mismatch as `digest_mismatch`.
- `action_digest` is the SHA-256 of `{run_id, action, body}`.
- The receipt's `projection_digest` is the SHA-256 of its projection bytes. It is recomputable from the receipt (`test_receipt_projection_digest_is_recomputable_from_the_receipt`).
- A worker bootstrap verifies against its receipt. Attempt, fence, and claim are excluded from the identity, so a retry's bootstrap matches the same receipt identity (`test_attempt_fence_and_claim_are_excluded_from_the_identity`, `test_bootstrap_for_a_retry_carries_the_new_attempt_fence_and_claim`). Any difference in run, source, compatibility, deadline instant, or max runtime is `bootstrap_mismatch`.
- Equal instants written with different UTC offsets produce identical projection bytes (`test_equal_instants_in_different_offsets_produce_identical_projection_bytes`).
- `build_bootstrap` refuses with `illegal_transition` unless the run is `dispatching` with a receipt and a claim. A bootstrap for a run in any other state, or missing either field, is never produced (`test_receipt_bootstrap.py`).

## 11. Admission and source

Admission gates a manifest against the server policy in this order: target, harness, model,
thinking, toolchain profile. Each refusal is `not_allowed` (HTTP 403). The target is checked
before the harness (`test_policy.py::test_the_target_is_checked_before_the_harness`). The
toolchain profile names the pinned `ToolCompatibility` tuple: image digest, profile ID,
toolchain manifest digest, and instruction digest.

The source is an immutable upload. `start` binds it only if the stored object generation,
bytes, and manifest agree (`state_machine` tests `test_admit_start_refuses_source_bytes_that_differ_from_the_manifest`,
`test_admit_start_refuses_store_objects_that_differ_from_the_upload`,
`test_admit_start_refuses_after_the_upload_window_closes`). Start refuses a manifest digest that
differs from the create, and a run that is not `awaiting_upload`.

## 12. Time rules

The controller's clock is the only authority for liveness and deadlines. Worker timestamps are
recorded for information only.

### Liveness

`freshness()` decides from server-received time:

- No heartbeat received: `stale`.
- `last_seen` more than `skew_seconds` in the future: `clock_anomaly`. It changes nothing for the run (`test_clock_anomaly_changes_nothing_for_a_running_run`).
- Age greater than `stale_after_seconds`: `stale`. Exactly `stale_after_seconds` old is still `fresh` (`test_heartbeat_exactly_stale_after_seconds_old_is_still_fresh`).
- Otherwise `fresh`.

Tests: `test_time.py`, including `test_heartbeat_from_the_future_beyond_skew_is_a_clock_anomaly`
and `test_heartbeat_at_the_skew_limit_is_not_an_anomaly`.

The 90-second window is a policy value (`stale_after_seconds`), not a constant. The 15-second
worker heartbeat cadence is a runner duty that is not implemented yet (DPES-807).
`test_status_reconciliation.py::test_a_ninety_second_policy_marks_a_silent_run_stale_just_past_ninety_seconds`
pins the boundary at 90 seconds.

### Scheduler progress

`scheduler_health.scheduler_freshness()` judges the Scheduler from the time of its last successful
pass only. It reuses `freshness()` with the same skew and stale window. A missing pass is `stale`.
A pass recorded beyond skew in the future is `clock_anomaly`, and so is a last pass that moves
backward from the previous one. Per-run reads are not an input, so healthy run reads cannot mask a
silently stalled Scheduler (`test_scheduler_stall.py`). No pass record is written or exposed yet.
The sweeper module plans and records nothing (DPES-804 and DPES-807). The batch reconcile route in section 5 applies the reconcile actions it plans, one page per call, through the same compare-and-set path as the single-run route.

### Worker timestamps

`classify_worker_timestamp()` returns one of three verdicts:

- `future`: the observed time is more than `skew_seconds` ahead of server time.
- `backward`: the observed time is earlier than the previous one.
- `accepted`: otherwise. An equal time is accepted.

Verdicts are informational. They never move liveness, deadlines, or freshness (the docstring
of `classify_worker_timestamp`; `test_worker_timestamp_far_ahead_of_server_time_is_future`,
`test_worker_timestamp_earlier_than_the_previous_one_is_backward`,
`test_worker_timestamp_equal_to_the_previous_one_is_accepted`).

### Deadlines

- A run's deadline is admission plus its runtime budget (`test_admission_deadline_is_admission_plus_the_runtime_budget`).
- The upload window closes after 600 s, or at the deadline if that is earlier (`test_upload_window_is_capped_by_a_shorter_run_deadline`).
- A deadline passes at its exact instant, not one second early (`test_deadline_is_not_passed_one_second_early`, `test_deadline_is_passed_exactly_at_its_instant`).
- A queued run times out at 1800 s (`test_queued_run_times_out_at_thirty_minutes`).
- A dispatching or starting run past the startup budget is reconciled, not left to age into staleness (`test_dispatching_past_the_startup_budget_is_reconciled`, `test_starting_run_past_startup_fails_before_it_is_considered_stale`).
- Retry never extends the original deadline (`retry()`; `test_retry_after_launch_uncertain_reopens_attempt_under_the_original_deadline`).

### Skew

`skew_seconds` is policy, must be non-negative, and may be 0. It is the tolerance applied to
future-dated request UUIDs, future heartbeats, and future worker timestamps. Client or worker
wall clocks cannot extend an allowance or create liveness, because only server-received time is
compared against deadlines and staleness.

## 13. Teardown outcomes

`classify_cleanup(CleanupFacts)` returns one `CleanupOutcome`. The first matching rule wins:

| Order | Condition | Outcome |
| --- | --- | --- |
| 1 | Store not reachable | `unavailable` |
| 2 | Retention exception in force | `retained-exception` |
| 3 | Execution not terminal | `running` |
| 4 | Capability expiry not yet passed | `fenced` |
| 5 | No data and records past retention | `expired` |
| 6 | No data | `complete` |
| 7 | Data present and deletion started | `incomplete` |
| 8 | Data present, deletion not started | `eligible` |

`PENDING_DELETION_OUTCOMES` is `{fenced, eligible, incomplete}`. These are the outcomes where
durable data still exists and will be deleted. `running` is excluded because the run is live, and
`retained-exception` is excluded because deletion is deliberately off.

`teardown_status(record, facts)` returns `TeardownStatus` with `run_id`, `execution_state`
(the run's state, which is normal terminal execution for a finished run), `data_cleanup`,
`pending_deletion`, `preserved_records`, and `fresh_run_implied`. Keeping `execution_state`
separate from `data_cleanup` is what lets a response distinguish a succeeded run whose data is
still pending deletion from a run that has no data left. `preserved_records` defaults to
`idempotency_record` and `audit_record`, the records teardown keeps. `fresh_run_implied` is fixed
to `False`, so a teardown response never implies a new run.

Tests: `test_teardown.py`. It covers each precedence rule, exhaustive totality over all
128 fact combinations, and that `pending_deletion` is true exactly for the pending set.

`GET /v1/runs/{run_id}/teardown` returns `TeardownStatus` for the owner's run. It is read-only: it deletes nothing and does not change the run. `_cleanup_facts` in `broker.py` gives each fact its source, or a stated default:

- `execution_terminal` comes from the run state. `capability_expiry_passed` comes from the run deadline, because every capability is clamped to it.
- `records_past_retention` comes from the policy's approved durations, so nothing is due while they are unset.
- `store_reachable` is true because the record was just read. `retention_exception` is false because no retention-exception model exists. `deletion_started` is false because no deletion path exists.
- `data_present` is reported true. The object store has no listing, and checkpoint ids are not recorded durably, so absence cannot be proven. Overstating presence keeps data pending rather than claiming it is gone.

The live route therefore reaches `running`, `fenced`, and `eligible`. `complete`, `expired`, `incomplete`, `retained-exception`, and `unavailable` need the sources named in [Gaps](#gaps). Tests: `test_teardown_route.py`.

## 14. Server policy

The controller reads the policy file named by `--policy`, or by `MAO_CLOUD_POLICY_FILE` when `--policy` is absent (`serve.policy_path_from`). Neither refuses at startup. The image carries no policy file, and the values are deployment data.

`config.py` loads `policy.json` as `_PolicyDocument`, which forbids unknown keys. Required keys
are `server_id`, `targets`, `harnesses`, `models`, `thinking`, `profiles`,
`stale_after_seconds` (at least 1), and `skew_seconds` (at least 0). Optional keys are
`startup_timeout_seconds` (at least 1), `max_attempts` (1 to `MAX_ATTEMPTS`), `api_audience`,
`identities`, and `access_mode` (`allowlist` when omitted, or `mayo-wide`; see
[section 5](#caller-identity-and-access-modes)).

Server invariants enforced by the frozen `Policy` dataclass:

- `server_id` matches `^[A-Za-z0-9][A-Za-z0-9._:-]{0,63}$`.
- Each allowlist is non-empty.
- Harnesses, models, and thinking levels are subsets of the protocol enumerations. A policy cannot extend them (`test_a_policy_cannot_extend_the_protocol_enumerations`).
- Target IDs match `^[a-z][a-z0-9-]{2,62}$`. Profile IDs match `^[a-z][a-z0-9-]{1,62}$`.
- A profile must pin its own `profile_id`.
- `identities` require `api_audience`, and `mayo-wide` requires `api_audience`. Each refuses startup without it.
- `access_mode` accepts only `allowlist` and `mayo-wide`. Any other value, including an empty string, refuses startup.
- An omitted `access_mode` is `allowlist`, the fail-closed mode. A deployment that wants `mayo-wide` must set it in its policy file.

Clients may name only entries present in the policy. Images, service accounts, and secrets come
from the server, so a manifest that supplies them is refused by the screen before policy runs.
Tests: `test_policy.py`, `auth_policy_test.py`.

## 15. Addenda

Each addendum from the protocol-v1 design is stated here with its mechanism and its evidence.
Status is one of **covered**, **partial** (mechanism exists, evidence is incomplete), or **gap**.

### 15.1 Edge-case addendum

| Requirement | Mechanism | Evidence | Status |
| --- | --- | --- | --- |
| Approved idempotency lifetime, and outcome after a dedup record expires | 7-day TTL; `decide_admission` (sections 6.1 and 6.2). | `test_idempotency.py` TTL and decision tests; `test_broker_api.py::test_create_replay_after_seven_days_is_expired`; `test_edge_replay.py::test_retry_replayed_after_seven_days_is_expired_and_opens_no_attempt`. | covered for create and retry over HTTP |
| Outcome after a dedup record is restored from backup | Restore floor raises the admission floor to restore completion. | `test_idempotency.py::test_a_later_restore_floor_raises_the_admission_floor`, `test_request_before_a_restore_floor_is_expired_even_inside_the_ttl`; `test_edge_replay.py::test_restored_dedup_row_still_replays_the_stored_run_after_restore_completes`, `test_edge_replay.py::test_retry_applied_before_backup_replays_the_same_attempt_after_restore`; `test_edge_replay.py::test_retry_applied_after_backup_and_lost_is_refused_past_the_restore_floor`. | partial: floor, restored-row replay, and retry replay after restore are covered against the in-memory `StoreBackup` copy only; there is no Firestore backup or restore drill, so real restore behaviour is unverified |
| Replayed original or retry UUID returns its result, an expired refusal, or a conflict, never a new root, run, or attempt | Digest check, TTL check, orphan refusal, and retry-UUID recording. | `test_broker_api.py::test_retry_replay_with_the_same_request_uuid_does_not_open_another_attempt`; `test_broker_api.py::test_create_replay_after_a_retry_still_returns_the_same_run`; `test_edge_replay.py::test_retry_with_a_changed_body_after_expiry_is_a_digest_conflict_not_an_expiry`; `test_edge_replay.py::test_retry_replay_whose_dedup_row_is_lost_is_refused_and_not_applied_again`. | covered for create and retry replay; partial-cleanup replay is the next row |
| Replay after partial cleanup | Create and start replay return the stored run and receipt after the input and log objects are deleted; missing objects read as typed `not_found`. | `test_edge_replay.py::test_replay_after_partial_cleanup_returns_the_same_run_and_receipt`. | covered |
| Expired or trimmed cursor returns an authoritative snapshot, watermark, and disclosed gap | `plan_page` and cursor watermark. | `test_cursors.py` gap tests; `test_broker_api.py` cursor tests. | covered for the gap; the snapshot is status, which is separate |
| Same canonical identity for supported older payload versions; unsupported versions refuse and are not recanonicalized | Version is part of the token; only version 1 exists. | `test_cursors.py::test_unsupported_cursor_version_names_the_version_not_a_generic_failure`. | partial (no older supported version exists to test) |
| Authoritative time source; future and backward timestamps do not extend allowances or create liveness | Server clock; `freshness`; `classify_worker_timestamp`. | `test_time.py`. | covered |
| Acceptance evidence: old/new client recovery | Typed refusal for an unsupported version; a corrected v1 request then recovers on the same run and owner with identity preserved. | `test_edge_recovery.py::test_refused_start_then_corrected_v1_start_recovers_with_identity_preserved`; `test_edge_recovery.py::test_refused_create_opens_nothing_and_the_corrected_create_opens_one_run`; `test_broker_api.py::test_unsupported_cursor_version_is_refused_and_run_stays_readable`. | partial (only version 1 exists, so no run by an earlier supported client and no recovery against a newer controller state) |

### 15.2 pi-durable teardown addendum

| Requirement | Mechanism | Evidence | Status |
| --- | --- | --- | --- |
| Typed cleanup outcomes: eligible, fenced, running, incomplete, complete, retained-exception, expired, unavailable | `CleanupOutcome`, `classify_cleanup`. | `test_teardown.py`; `test_teardown_route.py`. | partial: the classifier is covered at unit level; the live route reaches running, fenced, and eligible only. complete, incomplete, expired, retained-exception, and unavailable are not reachable from the route, because presence and deletion have no source (see the `_cleanup_facts` notes above and [Gaps](#gaps)) |
| Response distinguishes normal terminal execution from data pending deletion | `TeardownStatus.execution_state` and `pending_deletion`. | `test_teardown.py::test_teardown_status_marks_pending_deletion_exactly_for_the_set`; `test_teardown_route.py`. | covered at unit level and through the route |
| Preserve the request/dedup and audit records without implying a fresh run | `TeardownStatus.preserved_records` (default `idempotency_record`, `audit_record`) and `fresh_run_implied: Literal[False]`. | `test_teardown.py::test_teardown_status_keeps_the_request_and_audit_records_and_implies_no_fresh_run`. | partial (the route returns the fields and `test_teardown_route.py` checks them; no test that stored records survive a deletion, because no deletion exists) |

### 15.3 Cloud Run request contract addendum

Three documents are versioned and machine-readable: the create request, the start request, and
the worker bootstrap. Their field origins and forbidden inputs are in section 9.

| Requirement | Mechanism | Evidence | Status |
| --- | --- | --- | --- |
| Create carries protocol version, request UUID, canonical digest, target, harness, model, thinking, toolchain profile, source-upload intent, budget and capacity intent, and assignment references | `CreateRunRequest` fields, all `client-manifest`. | `test_schemas.py`; `test_screening.py::test_a_well_formed_create_request_passes_the_screen`. | covered |
| Start references the server-issued run and verified source descriptor; accepts no fresh bytes, changed manifest, or client-selected attempt, fence, or claim | `StartRunRequest` has only version, canonical digest, and source upload; identity keys are refused by the screen. | `test_screening.py::test_start_request_refuses_a_client_controlled_key`; `test_state_machine.py` start tests. | covered |
| Worker bootstrap carries only accepted identity, source descriptor, compatibility tuple, deadline and budget, and durable references | `WorkerBootstrap`; `durable_root`, `checkpoint_ref`, and `continuation_budget` are BLOCKING. | `test_receipt_bootstrap.py`. | covered for the fields that are specified; the three BLOCKING fields are gaps (section 16) |
| Final schema identifies server-derived, controller-assigned, client-provided, and forbidden fields | `FIELD_ORIGINS` (Python), emitted as `x-origin`; forbidden names emitted as `x-forbidden-inputs`. | `test_schema_authority.py`; `test_screening.py` origin tests. | covered |
| No bearer credentials, signed URLs, local paths, raw source, or caller argv or environment in the initial request | Client screen (section 9). | `test_screening.py`. | covered |
| Schema fixtures validate a successful create, upload, and start sequence | `tests/fixtures/protocol-v1/accept/` (create request and response, start request, accepted receipt, worker bootstrap), generated once from the in-memory broker; `flows.py::admit_and_start`; `test_broker_lifecycle.py::test_lifecycle_runs_from_admission_to_succeeded`. | `test_protocol_fixtures.py::test_success_sequence_validates_through_the_models`. Validated through the pydantic models; no JSON Schema validator, because `jsonschema` is not a cloud dependency. | covered for the pydantic models |
| Reject changed manifest or source, unknown version, local path, client actor, attempt, fence, or claim, signed URL, and credential | Digest check; version gate; screen; source checks. | `tests/fixtures/protocol-v1/reject/` through `test_protocol_fixtures.py`; `test_screening.py`; `test_state_machine.py` source-mismatch tests. | covered |
| Worker bootstrap matches the accepted receipt byte-for-byte where applicable | Projection digest. | `test_protocol_fixtures.py::test_worker_bootstrap_matches_the_accepted_receipt_byte_for_byte`; `test_receipt_bootstrap.py::test_matching_bootstrap_verifies_against_its_receipt`, `test_equal_instants_in_different_offsets_verify_against_the_receipt`. | covered |

### 15.4 DPES-869 durable recovery addendum

DPES-869 is partly specified. The approved DPES-803 durable-recovery decisions (project owner decisions
a to d, 2026-10-21) and the DPES-869 decisions Q1 to Q4 (project owner answers, 2026-10-22, recorded
outside this repository) fix the storage model, checkpoint cadence and limits, the six checkpoint
error codes, the replay classes, and the integration contract. Platform and MCAO approval of the
versioned contract is still required, so this section is not final until that approval is
recorded. The pi-durable pilot stays disabled until DPES-868 produces real Vertex and bubblewrap
evidence. The operation list and any integration interface beyond the version field remain open.

| Requirement | Mechanism | Evidence | Status |
| --- | --- | --- | --- |
| Interrupted tool replay: read and workspace edits replay; shell, network, and unknown tools do not | `tool_replay_outcome` and `replay_outcome` in `mao_cloud/replay.py`: `read`, `edit`, and `write` replay; `bash` and every other name are reconciliation-required. `plan_resume` splits interrupted calls into a replay list and a held list, keeping each held call's `INTERRUPTED_NEEDS_OWNER` outcome; it executes nothing | `cloud/tests/test_replay.py` | partial: pinned CodingTools only; Grep, Glob, LS, and metadata (named replay-safe by Q3) are not in the pinned set, so they stay unclassified and are never replayed; planner level only: no harness wiring, no persisted interrupted-call record, no replay error code |
| Checkpoint storage: Firestore holds ownership and lifecycle metadata; one GCS bucket holds payloads and checkpoint objects behind run-prefix capabilities; two-step commit (immutable objects, then pointer and commit marker in one transaction) | Decided (Q1). Commit-marker and manifest identity model in `mao_cloud/checkpoint.py`. `Storage` port in `mao_cloud/storage.py`; two-step publish, selection read, and result publish in `mao_cloud/checkpoint_publish.py`; in-memory fake in `cloud/tests/storage_fakes.py` models all-or-none batches (transcript entries, Documents, tasks), create-only objects, and CAS transactions | `cloud/tests/test_storage_fake.py`, `cloud/tests/test_checkpoint_publish.py` (fake level only); `cloud/tests/test_checkpoint.py` (model) | partial, AC1 at fake level only: no GCS or Firestore adapter, so the upstream storage conformance suite has not run; no orphan deletion; no run-prefix read capability (gap 15); T1 open |
| Checkpoint cadence: every minute, only at a tool-effect boundary with no tool in flight | Decided (Q2) | none | gap: no runner loop or tick |
| Checkpoint content: schema v1 manifest (identity, source and base digest, pinned harness and image, harness state and workspace references, generation and checksum per object, commit marker); limits 64 MiB harness state, 1 GiB compressed workspace per checkpoint, 2 GiB per run | `CheckpointManifest`, `CommitMarker`, `check_component_limits`, and `check_run_budget` in `mao_cloud/checkpoint.py`; `CAPABILITIES[(checkpoint, PUT)]` stays BLOCKING | `cloud/tests/test_checkpoint.py`; `cloud/tests/test_checkpoint_publish.py` (read-back, retry, size refusal, and budget: `test_publish_refuses_a_checkpoint_past_the_run_budget_before_any_object_is_created`, `test_a_run_may_spend_its_budget_exactly`, `test_a_retry_that_adopts_its_leftover_components_does_not_count_them_twice`, `test_replaying_a_committed_checkpoint_at_the_budget_limit_is_not_refused`, `test_a_failed_publish_frees_nothing_for_a_new_checkpoint_id`); `cloud/tests/test_storage_fake.py` (`checkpoint_component_bytes`) | partial: pure model, not exported to the JSON schemas; publish checks component sizes with `check_component_sizes` before any upload, then atomically admits component names and sizes through `Storage.reserve_checkpoint_components` before uploads (stored components and pending allocations count once per name, failed reservations remain charged, and same-name retries reuse them); the budget is fake-only until a real adapter durably coordinates these reservations across processes (DPES-807); publish adopts an identical component left by an earlier try, refuses differing bytes with `checkpoint_integrity_failed`, and verifies generation, size, and SHA-256 on read-back; no workspace tarball writer |
| Restore an isolated checkout from source and a validated checkpoint; preserve dirty, deleted, and mode-changed files; refuse with typed errors | `select_checkpoint` (selection gate: fence and CAS, manifest digest, component identity, one checkpoint directory) and `admit_restore` (bootstrap gate: manifest gate, then component identity, then one launch) in `mao_cloud/checkpoint.py`. `validate_for_restore` runs run identity, compatibility, component limits, then safe relative paths, with symlink entries refused as `restore_unsafe_path`. A fault at any `PublicationBoundary` leaves no marker, so nothing is selected | `cloud/tests/test_checkpoint.py`; `cloud/tests/test_checkpoint_publish.py` | partial: pure gates with fault and bad-input fixtures; `read_selection` reads the pointer, marker, and manifest through the `Storage` port (fake only); not wired to bootstrap; no checkout restore and no restored dirty, deleted, or mode state |
| Pi-durable qualification and Node/Python integration interface (Q4) | Adapter contract stays versioned JSON over stdio; Q4 adds only a version field, not yet added (no Pi adapter exists in this tree); pinned probe in `cloud/probes/pi-durable` | probe suite (local fixture evidence only) | gap: version field not added; DPES-868 real Vertex and bubblewrap evidence absent (AC2 waits on DPES-804 and DPES-811); production `--no-session` path unchanged; pilot stays disabled |
| Teardown inventory and fenced, resumable deletion | Decided: delete every per-run GCS object, local profile copy, and per-run payload field; keep one content-free tombstone per run | none | gap: BLOCKING pi-durable teardown addendum; no deletion code |
| Edge-case layout classification and retention reconciliation with DPES-818 | none | none | gap |

### 15.5 Shared fence and identity contract

DPES-869 owns this contract. DPES-870 consumes it and defines no second fence protocol (its
decisions, Q1). Changing any rule below is a versioned change coordinated through main, not a
local edit by either ticket.

1. **Identity.** A logical run is `run_id`. An attempt is `attempt`, from 1 to `max_attempts`.
   A fence is `fence`, an integer of at least 1. A claim is `claim_id`, set by dispatch. The four
   together are `IDENTITY_FIELDS` (`protocol.py`).
2. **Current identity is the controller's run record alone.** Only its `attempt`, `fence`, and
   `claim_id` are accepted. No worker-written document or client request creates or changes them.
3. **Lifecycle.** Admission opens attempt 1 with fence 1. Dispatch sets `claim_id`. Owner retry
   opens attempt N+1 with fence N+1, clears `claim_id`, and keeps the original `deadline_at`.
   Attempt and fence always move together; no code path changes only one.
4. **Acceptance of worker documents.** A claim, status report, or result counts only when its
   `attempt`, `fence`, and `claim_id` equal the record's. `accept_claim`, `accept_status`, and
   `accept_result` refuse a mismatch with `stale_attempt`. Reconcile's ingest of status
   telemetry ignores a mismatch as fenced telemetry, so it never moves liveness or deadlines.
5. **Namespace.** Every attempt-scoped object is written under its own attempt prefix.
   Write preconditions are object-specific (section 8): claim, control, cancel, checkpoint, log, and result
   uploads are create-only; status updates use generation matching. A stale attempt can write
   only into its own namespace and cannot replace bytes that belong to a later attempt.
6. **Checkpoint documents** (DPES-869) must carry the same four identity fields as the claim and
   result documents. The checkpoint manifest and commit marker (`mao_cloud/checkpoint.py`, schema
   v1) carry `run_id`, `attempt`, `fence`, and `claim_id`; `check_commit_matches` refuses a commit
   marker that names another run, attempt, fence, or claim. Publishing a checkpoint selection is
   accepted only when the document names the current identity and the compare-and-set
   preconditions (`expected_attempt`, `expected_version`) match the run record, in the order of
   section 3. `check_selection_publishable` (`protocol.py`) and `select_checkpoint`
   (`checkpoint.py`) implement that check and refuse with `stale_attempt` or `version_conflict`
   before any selection is made. Q1 stores the selection as a pointer written with the commit
   marker in one transaction. `publish_checkpoint` (`checkpoint_publish.py`) does that against the
   `Storage` port: it checks identity before any upload and again inside the commit transaction, and
   it never bumps the run version, so a caller holding a stale `expected_version` gets
   `version_conflict` and must re-read and retry. This is verified against the in-memory fake only;
   no Firestore transaction exists.
7. **No cross-process lock.** The fence and the compare-and-set are the only mutual exclusion
   between an attempt and its replacement.
8. **Checkpoint and result publication rules** (DPES-897). Each rule below is a change to the
   versioned contract, and each is listed so that it is reviewed through main. The four decisions
   were delegated by the project owner and are recorded in the DPES-897 pull request.
   - **Layout (decision 3).** A checkpoint is one directory per checkpoint id,
     `runs/{run_id}/attempts/{attempt:06}/checkpoints/{checkpoint_id}/`, holding `harness.bin`,
     `workspace.tar.gz`, and `manifest.json`, all named by `checkpoint_object_names`. The sequenced
     name `checkpoints/{seq:010}.json` is retired. `select_checkpoint` already requires one
     checkpoint directory.
   - **Retry after a partial upload (decision 1).** A component that already exists with identical
     bytes is adopted. A component with different bytes is refused with `checkpoint_integrity_failed`,
     and no commit marker or pointer is written. A retry with different bytes uses a new checkpoint
     id.
   - **Size check before upload (decision 2).** `check_component_sizes` refuses an oversized harness
     state or workspace with `checkpoint_limit_exceeded` before any object is written.
   - **Result retry.** An identical stored result is adopted. Different bytes are refused with
     `checksum_mismatch`, and the run stays open. A result document above 1 MiB is refused with
     `invalid_request` before any write.
   - **Returned artifacts (decision 4).** A returned artifact (patch or bundle) is at most 200 MiB
     (`MAX_RESULT_ARTIFACT_BYTES`, `ArtifactRef.size_bytes`). The result document keeps its 1 MiB
     cap. The 200 MiB value is the source limit in the DPES-811 text.
   - **Signer.** `CapabilityRequest` gains `checkpoint_id`. A checkpoint PUT must name one of that
     id's components, or the signer refuses with `object_mismatch`.
   - **Schema.** `run-result.json` carries `maximum: 209715200` on the artifact `size_bytes`.
   - **Version.** `protocol_version` stays `v1`. No checkpoint object has ever been writable,
     because `CAPABILITIES[(checkpoint, PUT)]` is blocked by DPES-869, and no artifact has been
     published (gap 33). Platform approval through main is still required (gap 19).

Behavior pinned by existing tests: `test_state_machine.py::test_retry_of_a_retryable_failure_opens_the_next_attempt`
(fence and attempt both advance, claim clears), `test_status_for_another_fence_is_stale`,
`test_accept_claim_for_another_attempt_is_stale`, `test_status_for_another_claim_is_stale`,
`test_result_for_another_attempt_is_stale`, `test_result_for_another_claim_is_stale`,
`test_broker_api.py::test_superseded_attempt_result_at_the_new_path_is_ignored`,
`test_receipt_bootstrap.py::test_bootstrap_for_a_retry_carries_the_new_attempt_fence_and_claim`,
`test_objects.py::test_artifact_object_is_scoped_to_its_attempt`.

Gap: the pure selection gate and its stale-fence fixtures exist (`test_checkpoint.py`). The
publish path and its compare-and-set inside the commit transaction exist against the in-memory
fake (`test_checkpoint_publish.py`). The Firestore transaction does not exist yet, so the
compare-and-set is not yet checked against a real backend; that stays open with AC1 and T1.

### 15.6 DPES-817 local result import addendum

DPES-817 owns the result archive, the destination check, and the local import. It reuses
`accept_result`, `verify_artifacts`, `artifact_object_name`, `redact`, and `check_path`, and it
adds no second fence, identity, or redaction rule. The gates in section 15.5 apply unchanged.

1. **Archive.** A succeeded attempt is published as the archive `mao-result-archive/v1`, with five
   required parts under the attempt prefix: `manifest.json`, `return.bundle`, `agent-final.json`,
   `run.log`, and `tests.json`. The manifest names the subject (run, attempt, fence, claim,
   repository, base and final SHA, image digest, CLI version, model, profile), the other four parts
   by SHA-256 and size, `complete` with `incomplete_reason`, and the tests as `self-reported`.
   The result document lists every part as an `ArtifactRef`, so the parts are checksummed by
   `verify_artifacts` (`test_archive_names_every_part_and_its_checksum_and_size`,
   `test_self_reported_tests_are_labelled_and_never_independent`,
   `test_logs_are_redacted_before_they_are_stored`). The archive carries no moa-proof evidence part
   (gap 68). The parts are named by `artifact_object_name`.
   No `CAPABILITIES` row covers them and `ObjectKind` has no artifact kind, so no runner can upload
   them yet (gap 62).
2. **Gate.** `accept_archived_result` is the result gate for archived results. It runs
   `accept_result`, requires the five parts in the document, verifies each stored part against its
   checksum, and then publishes the canonical `RunResultDocument` bytes through `publish_result`.
   A succeeded result without the archive is refused and nothing is written
   (`test_success_without_a_result_archive_is_refused_and_nothing_is_written`,
   `test_gate_refuses_an_archive_object_missing_from_storage`,
   `test_gate_refuses_a_checksum_that_does_not_match_the_stored_bytes`,
   `test_stale_attempt_is_refused_before_any_artifact_is_read`,
   `test_a_cancelled_run_fences_the_archived_result`). The legacy `publish_result` still accepts a
   body with no artifacts, and the local import refuses that body as `no-result-archive`. The
   controller route still calls `accept_result` directly (gap 70).
3. **Return bundle.** The bundle is a git v2 bundle holding exactly one branch above the base,
   made with `git bundle create <tmp> refs/heads/<branch> ^<base>`. The header check requires
   exactly one base prerequisite, no capability lines, exactly one `refs/heads/` ref, and a base
   equal to the request. The trusted intake in section 19.2 refuses prerequisites; the return path
   requires exactly one (`test_header_names_one_base_prerequisite_and_one_branch_ref`,
   `test_bundle_names_only_the_work_branch_above_base`,
   `test_self_contained_bundle_is_refused_as_wrong_base`,
   `test_malformed_or_multi_ref_headers_are_refused`).
4. **Size.** The result document keeps `MAX_RESULT_BYTES` (1 MiB). The return bundle has its own
   versioned cap, `MAX_RETURNED_WORK_BYTES` (200 MiB), checked at build and again on fetch
   (`test_returned_work_has_its_own_cap_separate_from_the_result_document`,
   `test_archive_refuses_returned_work_over_the_cap_at_build`,
   `test_parse_refuses_returned_work_over_the_cap_on_the_fetch_side`,
   `test_oversize_returned_work_is_refused_on_fetch`).
5. **Checkpoint.** At normal finish, a checkpoint commits eligible nonignored dirty files on the run
   branch. It refuses more than 10,000 files, more than `MAX_CHECKPOINT_BYTES`, unscannable
   content, and any content the `redact` screen flags, and a refused checkpoint commits nothing.
   Ignored files are skipped, and a rename is recorded as a delete plus an add
   (`test_checkpoint_commits_nonignored_changes_and_skips_ignored_files`,
   `test_checkpoint_records_a_rename_as_a_delete_and_an_add`,
   `test_checkpoint_refuses_a_secret_and_commits_nothing`). No runner calls it yet (gap 71).
6. **Destination.** Before any local ref exists, the whole returned tree is checked against one
   explicit named profile: `case-sensitive`, `case-insensitive-nfc`, or `case-insensitive-no-symlinks`.
   A profile is an assumption the operator names. Nothing probes the volume, and no uniform macOS
   behavior is claimed. Anything the profile cannot represent is refused whole and never renamed:
   unsafe paths, gitlinks (always), symlinks on a profile without them, names equal after case
   folding or NFC normalization, and file-versus-directory collisions
   (`test_compatible_control_case_only_names_pass_on_a_case_sensitive_profile`,
   `test_incompatible_control_case_equivalent_names_are_refused_not_renamed`,
   `test_file_versus_directory_collision_is_refused`,
   `test_gitlink_submodule_entries_are_always_refused`,
   `test_unsafe_tree_paths_are_refused_before_any_destination_check`,
   `test_a_three_way_case_collision_is_refused_as_one_group`).
7. **Import transaction.** The steps run in this order. (a) Read-only checks against storage: owner,
   run state, attempt, archive, repository, base, and bundle header. (b) Fetch the objects with no
   refs written and `fsck` on. (c) Validate the destination profile. (d) Under a non-blocking
   `flock` on `<git-common-dir>/mao-import/import.lock`, a busy lock refuses as `import-busy`. This is
   the import's own lock, not a project lock (gap 69).
   (e) Write the journal, which is the proof of ownership, before any ref or worktree. (f) Create
   the staging ref `refs/mao-import/<id>`, then the branch `cloud-import/<run>-a<attempt>-<id>` and
   its worktree at a fresh path. (g) Check the materialized tree: HEAD at the returned tip, clean
   status, and the exact file set. (h) Write the receipt atomically. (i) Release the staging ref.
   Hooks are disabled. The boundaries are `lock-held`, `journal-staged`, `staging-ref-created`,
   `worktree-created`, `materialized`, `receipt-written`, and `staging-released`
   (`test_a_fault_at_each_boundary_is_recovered_by_a_repeat_fetch`, parametrized over every
   boundary, `test_import_never_writes_to_the_source`).
8. **Repeat fetch.** A written receipt is returned unchanged, with no second worktree. A journal with
   no receipt resumes the same owned partial. A journal that names another import is refused as
   `journal-mismatch`. An unknown partial, or one with foreign content, is refused as `unknown-partial`
   and left in place. A published worktree that was removed is refused as
   `published-worktree-missing` and not recreated
   (`test_an_owned_partial_worktree_with_foreign_content_is_neither_overwritten_nor_removed`,
   `test_a_materialized_tree_that_differs_from_the_returned_tree_is_not_published`,
   `test_a_published_import_whose_worktree_was_removed_is_not_recreated`,
   `test_repeat_fetch_returns_the_same_receipt_without_a_second_worktree`).
9. **Refusals before local mutation.** `not-owner`, `not-succeeded`, `no-result-archive`,
   `stale-attempt`, `checksum-mismatch`, `wrong-base`, `wrong-repository`,
   `returned-work-too-large`, `unsupported-mode`, and `destination-collision` leave no refs,
   worktrees, or journal. Fetched objects stay unreferenced
   (`test_an_owner_other_than_the_run_owner_is_refused_and_nothing_is_created`,
   `test_a_changed_base_is_refused_and_nothing_is_created`; `_assert_nothing_created` checks the same
   state for each refusal). A destination that exists, or a live branch with
   the import name, is refused and left alone (`test_a_destination_that_already_exists_is_refused_and_left_alone`,
   `test_a_live_branch_with_the_import_name_is_never_overwritten`). A held lock and a second import
   during the first are refused and not interleaved (`test_a_held_project_lock_refuses_the_import_without_touching_anything`,
   `test_a_second_import_during_the_first_is_refused_not_interleaved`).
10. **Partial, executables, links.** A partial archive imports with receipt status `partial` and its
    reason (`test_a_partial_archive_imports_visibly_incomplete`). Receipts always carry
    `tests_source: self-reported` and `independent_verification: false`
    (`test_a_self_reported_test_result_is_never_claimed_as_independent`). Returned files are written and
    never run (`test_returned_executable_files_are_never_run`). A symlink is imported as a link only on
    a profile that holds links (`test_a_symlink_is_imported_as_a_link_on_a_profile_that_supports_it`).

## 16. BLOCKING markers

A BLOCKING field exists in the schema, has a documented meaning, and has no specified value.
Any non-null value is refused as `blocking_field`. The marker is a `BeforeValidator` in Python
and an `x-blocking` annotation in the JSON Schema.

| Field | Ticket | Python | Schema | Test |
| --- | --- | --- | --- | --- |
| `WorkerBootstrap.durable_root` | DPES-869 | yes | `worker-bootstrap.json` | `test_schemas.py::test_blocking_markers_are_exactly_the_ones_the_brief_names` |
| `WorkerBootstrap.checkpoint_ref` | DPES-869 | yes | `worker-bootstrap.json` | same |
| `WorkerBootstrap.continuation_budget` | DPES-870 | yes | `worker-bootstrap.json` | same |
| `Budget.restart_budget` (in the create request) | DPES-870 | yes | `create-run-request.json` | same |
| `WorkerIntake.bootstrap` carries the three `WorkerBootstrap` fields above | DPES-869, DPES-870 | yes (inherited) | `worker-intake.json` under `$defs/WorkerBootstrap` | same |
| Checkpoint object capability (`CAPABILITIES[(checkpoint, PUT)]`) | DPES-869 | yes (`blocking="DPES-869"`) | no | `test_objects.py::test_only_the_checkpoint_write_is_blocked_and_it_names_its_ticket` |

Blocked behavior is specified only as refusal. A caller cannot select durable recovery, a
checkpoint reference, a continuation budget, or a restart budget until DPES-869 or DPES-870
defines their values. The runner refuses a checkpoint write with `blocking_field`
(`runner.py`, `_write`; `test_runner_sweeper_cli.py`).

## 17. Schema export and evolution

`mao-cloud schema export --out DIR` writes the 21 schemas: accepted-receipt, cancel-document,
cancel-request, claim-document, control-document, create-run-request, create-run-response,
error-body, log-entry, log-page, retry-request, run-event, run-result, run-view,
start-run-request, status-report, status-view, teardown-status, upload-capability,
worker-bootstrap, and worker-intake. `mao-cloud schema check --dir DIR`
compares a directory with the rendered output and exits non-zero on drift. Each file is
written as indented, sorted JSON with a trailing newline (`test_schemas.py`).

Schema evolution is currently limited to this drift check, the BLOCKING parity test, and the
rule that a protocol change requires a new protocol version. No compatibility matrix exists
between revisions of v1 (see [Gaps](#gaps)).

## 18. Evidence index

| Area | Test file |
| --- | --- |
| Transitions, CAS, typed failures, start replay | `cloud/tests/test_state_machine.py` |
| Idempotency, TTL, admission floor, restore floor | `cloud/tests/test_idempotency.py`, `test_broker_api.py` |
| HTTP API, owner visibility, controller identity, retry, cursors | `cloud/tests/test_broker_api.py`, `test_broker_lifecycle.py` |
| Time, liveness, worker timestamps, deadlines | `cloud/tests/test_time.py`, `test_state_machine.py` |
| Cursors and trimmed history | `cloud/tests/test_cursors.py` |
| Object names, capabilities, headers, redaction | `cloud/tests/test_objects.py` |
| Bootstrap and receipt projection | `cloud/tests/test_receipt_bootstrap.py` |
| Screening, redaction, field origins | `cloud/tests/test_screening.py` |
| Server policy | `cloud/tests/test_policy.py` |
| Teardown outcomes | `cloud/tests/test_teardown.py` |
| Runner, sweeper, CLI | `cloud/tests/test_runner_sweeper_cli.py` |
| Schema export, drift, BLOCKING parity | `cloud/tests/test_schemas.py` |
| Interrupted tool replay policy (DPES-869) | `cloud/tests/test_replay.py` |
| Checkpoint manifest schema v1 and restore gate (DPES-869) | `cloud/tests/test_checkpoint.py` |
| Owner cancel: confirmed-stop completion, platform stop, cancel marker (DPES-816) | `cloud/tests/test_cancel_reconcile.py` |
| Trusted claim, runner nonce, ambiguous-create readback (DPES-811) | `cloud/tests/test_trusted_claim.py` |
| Source intake, bundle header, tree and path checks, hooks off (DPES-811) | `cloud/tests/test_trusted_bundle.py` |
| Sandbox argv and qualification probe (DPES-811) | `cloud/tests/test_trusted_sandbox.py` |
| Supervision, heartbeat, checkpoint cadence, redaction, outcomes (DPES-811) | `cloud/tests/test_trusted_supervise.py` |
| Profiles, workspace and tmp accounting, limits (DPES-811) | `cloud/tests/test_trusted_profiles.py` |
| Attempt order, refusals, exit codes, absent ports (DPES-811) | `cloud/tests/test_trusted_bootstrap.py` |
| Worker image Dockerfile, static checks only (DPES-811) | `cloud/tests/test_worker_image.py` |

Run the suite from the repository root:

```sh
uv --project cloud run pytest cloud/tests -q
uv --project cloud run mao-cloud schema check --dir cloud/schemas/protocol-v1
```

## 19. Trusted worker slice (DPES-811)

The trusted bootstrap is the code the worker image runs before any repository or model code.
It consumes `WorkerBootstrap` and `AcceptedReceipt`, and reuses `plan_claim`, `plan_result`,
`redact`, and `object_name` from `protocol.py` and `runner.py`. It adds no second contract.
Effects enter through the `Ports` dataclass in `trusted_bootstrap.py`, so each step is tested
with injected fakes. Real adapters are outside this slice (gaps 23 to 28).

### 19.1 Claim and runner nonce

- `ClaimDocument.runner_nonce` is 32 lowercase hex characters from `secrets` (16 bytes),
  unpredictable per runner process. It is runner-written and is not in `IDENTITY_FIELDS` or
  `CLIENT_CONTROLLED_FIELDS`. Tests: `test_trusted_claim.py::test_a_new_runner_nonce_is_32_lowercase_hex_and_unpredictable`,
  `test_a_new_runner_nonce_is_accepted_by_the_claim_document`.
- The claim is one create-only write (`x-goog-if-generation-match: 0`). The `ClaimStore` port has
  no delete method, so a claim is never deleted or replaced. A 412 refuses before any model or
  repository code runs and is not retried (`test_a_412_refuses_and_is_never_retried_or_deleted`,
  `test_the_create_carries_the_generation_zero_precondition`).
- After an ambiguous create response, the attempt proceeds only when an exact-key readback
  carries this process's nonce and the same identity fields as the planned claim. The
  comparison uses `secrets.compare_digest`. Any other readback refuses
  (`test_an_ambiguous_create_proceeds_when_the_readback_carries_our_nonce`;
  `test_an_ambiguous_create_refuses_when_the_readback_names_another_runner`,
  `..._when_no_claim_is_visible`, `..._when_the_readback_cannot_complete`,
  `..._a_malformed_readback`, `..._a_readback_for_another_attempt`). An ambiguous create is not
  retried (`test_an_ambiguous_create_is_not_retried`).
- A refused claim never reaches the source, sandbox, or worker
  (`test_trusted_bootstrap.py::test_a_412_claim_refuses_and_never_reaches_the_source_sandbox_or_worker`).

### 19.2 Source intake

- The upload is plain tar (`mode="r:"`) with exactly one regular member, `source.bundle`, at the
  root. Size and SHA-256 are checked against the bootstrap record before the archive is parsed
  (`test_trusted_bundle.py::test_a_hash_mismatch_is_refused_before_the_archive_is_read`,
  `test_a_size_mismatch_is_refused`, `test_a_compressed_archive_is_refused_as_not_plain_tar`,
  `test_the_archive_must_hold_exactly_one_source_bundle_at_the_root`,
  `test_a_symlink_member_is_refused`).
- Source limit: 200 MiB (`MAX_ADMITTED_SOURCE_BYTES`, exposed as `SOURCE_LIMIT_BYTES`). The
  controller refuses a larger source at admission, before any attempt is claimed, and the bundle
  intake refuses it too. The protocol's 512 MiB upload bound (section 2) is only the schema
  backstop; it does not widen admission
  (`auth_policy_test.py::test_default_admission_refuses_the_source_the_worker_would_refuse`,
  `test_trusted_profiles.py::test_the_source_limit_is_the_stricter_of_the_brief_and_the_protocol`).
- The bundle is parsed by this module, not by git. It must be a v2 bundle with no prerequisites,
  no capability lines, and exactly one ref (`test_a_bundle_with_prerequisites_is_refused`,
  `test_bytes_that_are_not_a_v2_bundle_are_refused`, `test_a_truncated_header_is_refused`).
- Git runs with `core.hooksPath=/dev/null`, `--template=`, no system or global config, and
  `transfer.fsckObjects=true`. Objects pass `fsck --strict --no-dangling`. Repository hooks and
  templates never run (`test_repository_hooks_and_templates_never_run`).
- Tree checks refuse symlinks, gitlinks, modes other than `100644` and `100755`, absolute,
  backslash, traversal, and `.git` path components, more than 50,000 entries, and more than 1 GiB
  unpacked (`test_a_symlink_in_the_tree_is_refused`, `test_a_submodule_gitlink_in_the_tree_is_refused`,
  `test_unsafe_tree_paths_are_refused`, `test_a_tree_over_the_entry_limit_is_refused`,
  `test_unpacked_content_over_the_limit_is_refused`). The tip is checked out detached
  (`test_a_clean_bundle_materializes_as_the_committed_tree`).
- The bundle's single tip must equal the claimed base SHA in `BootstrapSource.base_sha`, and this is
  checked before staging, clone, or any git process runs. A mismatch is refused
  (`test_a_bundle_whose_tip_is_not_the_claimed_base_is_refused_before_any_clone`). The worker
  attempt refuses the same way (`test_a_bundle_tip_that_is_not_the_claimed_base_refuses_before_the_worker_runs`),
  and the controller export runs the identical check (`test_source_export.py`).

### 19.3 Sandbox

- The worker runs as one bubblewrap process. The root is bound read-only (`--ro-bind / /`). `/tmp`
  is a 1 GiB tmpfs. The intake workspace is bound at `/workspace`, the only writable mount.
  The environment is cleared (`--clearenv`) and set from a fixed allowlist: `PATH`, `HOME=/tmp`,
  `LANG`, `PI_OFFLINE=1`, `PI_SKIP_VERSION_CHECK=1`, `PI_TELEMETRY=0`. `--die-with-parent`,
  `--new-session`, and `--unshare-pid`, `--unshare-ipc`, `--unshare-uts` are set
  (`test_the_root_filesystem_is_mounted_read_only`, `test_only_the_workspace_is_writable_from_the_host`,
  `test_the_environment_is_cleared_before_the_worker_variables_are_set`,
  `test_the_worker_environment_is_a_fixed_offline_allowlist`).
- `qualify_sandbox` runs a probe inside the sandbox. It refuses when bwrap cannot start, the root
  is writable or its read-only state is not proven, or the workspace is not writable or its
  writability is not proven (`test_a_writable_root_is_refused`, `test_an_unwritable_workspace_is_refused`,
  `test_a_sandbox_that_does_not_start_is_refused`, `test_a_missing_bwrap_binary_is_refused`,
  `test_probe_output_that_proves_nothing_is_refused`). Nothing falls back to an unsandboxed run.
- **Known limitation.** The sandbox does not unshare the network. Sandboxed commands can reach the
  metadata server and mint the worker service account token. This is not a control and must not be
  described as isolation. It is qualified under DPES-804
  (`test_the_network_is_shared_so_the_metadata_server_stays_reachable` pins the shared network so the
  limitation stays visible; gap 25).

### 19.4 Profiles and accounting

- Two profiles: 2 vCPU with 8 GiB (`SMALL`, "2/8") and 4 vCPU with 16 GiB (`STANDARD`, "4/16")
  (`test_the_two_profiles_are_two_and_four_vcpus_with_eight_and_sixteen_gib`).
- Declared disks: workspace 8 GiB and tmp 1 GiB, 9 GiB in total
  (`test_the_workspace_is_eight_gib_and_tmp_is_one_gib`).
- `4/16` holds the declared disks with 7 GiB to spare
  (`test_the_standard_profile_holds_the_workspace_and_tmp`).
- `2/8` does not hold them. It is 1 GiB short
  (`test_the_small_profile_does_not_hold_the_workspace_and_tmp_as_declared`). The profile is not
  changed to pass. This is gap 32.
- Result document limit: 1 MiB, the protocol value (`MAX_RESULT_BYTES`) (`test_the_result_document_limit_is_the_protocol_one_mebibyte`; gap 31, decided).
- Each returned artifact is capped at 200 MiB (`MAX_RESULT_ARTIFACT_BYTES`) (`test_the_returned_artifact_limit_is_200_mib_not_the_document_limit`).
- Returned work bundle limit: 200 MiB, a separate versioned cap (`MAX_RETURNED_WORK_BYTES`), set by the project owner's decision of 2026-10-22.
  It is enforced at archive build and on fetch (section 15.6; gap 31).
- Container memory enforcement is DPES-804.

### 19.5 Supervision

- The child runs in its own process group (`start_new_session`) with stdin closed and stderr
  merged into stdout (`test_the_child_leads_its_own_process_group`).
- Heartbeat every 15 s (`test_the_production_heartbeat_is_fifteen_seconds`). Checkpoints are due
  only when the workspace fingerprint has changed and at most every 300 s
  (`test_checkpoint_is_due_only_when_changed_and_the_interval_has_passed`,
  `test_an_unchanged_workspace_never_checkpoints`,
  `test_checkpoints_follow_changes_and_never_come_closer_than_the_interval`,
  `test_the_production_checkpoint_cadence_is_at_most_every_five_minutes`). Nothing publishes a
  checkpoint yet (gap 28).
- On the deadline the group gets SIGTERM for a bounded flush (30 s), then SIGKILL. The outcome is
  `timed_out` (`test_the_deadline_flushes_with_sigterm_and_reports_timed_out_not_completed`,
  `test_a_child_that_ignores_sigterm_is_killed_after_the_grace`,
  `test_the_whole_process_group_is_stopped_including_grandchildren`).
- Exit 0 is `completed`. A nonzero exit is `failed`. A signal, including the kernel OOM killer's
  SIGKILL, is `killed`. Only `completed` reports `succeeded`
  (`test_a_zero_exit_is_completed_and_its_output_reaches_the_sink`,
  `test_a_nonzero_exit_is_failed_never_completed`,
  `test_a_child_killed_by_a_signal_is_killed_never_completed`). An actual OOM kill is not observed
  (gap 24).
- Output is split into lines capped at 4096 characters (`MAX_LOG_LINE_CHARS`). An over-long line
  keeps its head and drops the rest up to its newline, so the buffer stays bounded
  (`test_a_long_line_is_capped_at_the_protocol_limit`,
  `test_a_line_with_no_newline_cannot_grow_the_buffer_and_later_lines_still_arrive`). Every line is
  redacted before it reaches the sink (`test_output_is_redacted_before_the_sink_receives_it`).

### 19.6 Attempt order, refusals, and exit codes

`run_attempt` runs these steps in order. A refusal stops before the next step and publishes
nothing:

1. Plan the claim, which checks identity and the deadline (`plan_claim`).
2. Take the create-only claim (section 19.1).
3. Read the source object.
4. Verify and materialize the source into `workspace` (section 19.2).
5. Qualify the sandbox (section 19.3).
6. Compute the remaining budget and refuse if the deadline has passed.
7. Run the worker under supervision (section 19.5), with the budget capped at the runtime.
8. Plan one create-only result and publish it.

Tests: `test_a_reported_attempt_runs_claim_then_source_then_sandbox_then_worker_then_publish`,
`test_a_source_that_fails_its_checksum_refuses_before_any_repository_code_runs`,
`test_a_sandbox_that_cannot_be_qualified_refuses_before_the_worker_runs`,
`test_a_deadline_that_has_passed_refuses_before_the_claim`,
`test_only_a_clean_completion_is_published_as_succeeded`.

Exit codes are fixed: 0 reported, 1 refused, 2 unreadable or malformed input, 3 production ports
not built (`test_exit_codes_are_distinct_and_fixed`, `test_unreadable_or_malformed_input_exits_two`,
`test_main_runs_the_attempt_in_order_through_injected_ports_and_exits_reported`).

`production_ports()` always raises. Object storage (DPES-807), the real sandbox (DPES-804), and
model access (DPES-819) are not built, so the worker image exits 3 and cannot run a real attempt
(`test_absent_production_ports_fail_closed_with_exit_three_and_name_the_gap`). Results carry no
commit or artifacts in this slice (gap 33).

### 19.7 Worker image

`cloud/Dockerfile` pins `alpine:3.24.2` by digest
(`sha256:294b683cb724975bec92580e1e685676bd4b50bda910ddb8c51d4cabeaec77e6`), recorded in
`toolchain.json` (`test_base_digest_is_the_one_recorded_in_the_toolchain_manifest`). It runs
`apk upgrade` before the pinned package install, guards Node at build time to 22.19 or later,
installs `@earendil-works/pi-coding-agent@1.3.0` from the locked `cloud/config/pi` lockfile. `cloud/config/pi/check-undici.js` fails the build unless every `undici` copy in the installed tree is 8.12.2 (CVE-2026-109869). The image then removes `npm`. The worker
is numeric UID and GID 10001 and runs as `USER 10001:10001`. The entrypoint is
`python3 -m mao_cloud.worker_entry`, which makes the image's only startup fetch (section 19.8) and
then runs `mao_cloud.trusted_bootstrap`. The code is root-owned and not writable by the worker.
The image bakes in no credentials
(`test_base_image_is_alpine_3_24_2_pinned_by_digest`, `test_apk_upgrade_runs_before_the_pinned_package_install`,
`test_node_is_guarded_at_build_time_to_at_least_22_19`, `test_pi_is_installed_from_the_lockfile_at_the_exact_pinned_version`,
`test_the_pi_lockfile_resolves_undici_only_to_the_fixed_version`, `test_every_locked_package_has_a_registry_url_and_an_integrity_hash`,
`test_npm_is_removed_after_the_pi_install_and_absent_at_the_end`,
`test_undici_check_accepts_a_tree_whose_every_copy_is_the_fixed_version`, `test_undici_check_rejects_a_nested_old_copy_even_when_the_top_level_is_fixed`, `test_undici_check_rejects_an_old_copy_nested_under_undici_itself`, `test_undici_check_rejects_an_npm_alias_whose_package_is_the_old_copy`, `test_undici_check_rejects_a_symlinked_old_copy`, `test_undici_check_accepts_a_symlinked_fixed_copy_and_npm_bin_links`, `test_undici_check_ignores_other_packages_and_undici_named_folders_in_packages`, `test_undici_check_fails_closed_on_a_dangling_symlink`, `test_undici_check_terminates_on_a_symlink_cycle`, `test_undici_check_rejects_a_tree_with_no_undici`,
`test_the_image_runs_as_a_numeric_non_root_user`, `test_the_bootstrap_code_is_root_owned_and_not_writable_by_the_worker`,
`test_the_entrypoint_is_the_worker_entry_and_its_only_fetch_is_the_intake`,
`test_no_network_fetch_or_credential_material_is_baked_in`, `test_no_broad_copy_of_the_build_context_is_made`).
The Dockerfile tests read the Dockerfile text. The checker tests run `cloud/config/pi/check-undici.js` on fixture trees. No image is built here (gap 23).

### 19.8 Worker entry and per-attempt intake (DPES-809, gap 72)

- The controller writes one create-only object per attempt, `runs/{run}/attempts/{attempt:06d}/intake.json`,
  holding a `WorkerIntake`: the `WorkerBootstrap` and the `AcceptedReceipt`. It is written after the
  control document and before `jobs.run`, and it is never replaced.
- The controller then mints a read-only capability for that object through the signer contract
  (`CAPABILITIES[(INTAKE, GET)]`, runner-held, lifetime `STARTUP_TIMEOUT_SECONDS`, never past the
  attempt deadline). A refused capability stops the launch before `jobs.run`; the attempt keeps its
  reservation and is never relaunched.
- `jobs.run` receives exactly three container overrides: `MAO_RUN_ID`, `MAO_ATTEMPT_ID`, and
  `MAO_INTAKE_URL`. The URL is a credential. It is never logged, quoted in an error, or printed in
  a test assertion.
- `mao_cloud.worker_entry` is the image entrypoint. It reads and removes `MAO_INTAKE_URL` before
  anything else, so child processes do not inherit it. It makes one HTTPS GET with the standard
  library (60-second timeout, no redirects, body capped at 64 KiB), parses the object, and checks
  that its run and attempt match the environment and that `verify_bootstrap` accepts it. Every
  failure refuses before the claim: an unreachable or non-200 fetch exits 3, a malformed object or a
  missing URL exits 2, and an identity or binding failure exits 1. No retry is made.
- On success it writes the two documents to a private temporary directory (mode 0600, created
  exclusively, removed on exit) and calls `trusted_bootstrap.main`, which runs the attempt order in
  section 19.6.
- Tests: `test_worker_entry.py` composes the real launcher overrides with the worker entry on fakes.
  It includes an altered bootstrap and an altered receipt digest, both refused before the claim.
  `test_admission_dispatch.py` covers the write-before-launch order and a refused capability.

Residual risks, not closed by this slice:

- The worker checks structure and binding only. An object altered in both documents consistently
  still verifies. Detecting that needs a digest the controller holds, which would be a fourth
  override, so it is not built.
- The signed URL is visible to anyone who can read the Cloud Run execution metadata. It is not
  pinned to a storage generation.
- The temporary files are readable by the worker's own uid, the same uid as the child processes
  that run inside the attempt.

## 20. Operator procedures: cancel and reconcile (DPES-816)

These procedures use only the routes and records in section 5. None of them edits a record,
an object, or a capacity counter by hand.

**Cancel a run.** Send `POST /v1/runs/{run_id}:cancel` with the owner identity and the body
`{"protocol_version": "v1"}`. Read the returned state:

- `cancelled`: the run had not launched (`awaiting_upload` or `queued`). Nothing else is needed.
- `cancel_requested`: the cancel is open and a platform stop has been requested. Do not retry
  the run and do not write `cancel.json` by hand.

**Finish an open cancel.** Only the controller can reconcile: `POST /internal/v1/runs/{run_id}:reconcile`
requires controller identity and is not a client route. Each call re-runs liveness, repeats any
open stop request, and settles capacity once the launch adapter confirms the stop. Then read the
run again. It reads `cancelled` with failure `owner_cancelled` after confirmation.

**Read a cancel that will not close.** If the run stays `cancel_requested` after reconcile, the
platform has not confirmed the stop. This is the intended result: the run keeps its worker slot
and daily reservation. A retry is refused with `retry_not_allowed` ("still holds capacity pending
confirmed execution termination"). Two warnings in the `mao_cloud.audit` log name the cause:
`platform stop not accepted` (the adapter refused or failed the request) and `cancel marker not
written` (the `cancel.json` write failed). Both are retried on every call, so they clear on their
own once the adapter or the store recovers.

**Do not release capacity by hand.** Capacity is never released on the clock or by an operator
override. The only release is an authoritative stop observation from the launch adapter (gap 41).
Escalate a cancel that stays open to the platform owner with the run id and attempt.

**No retry after an owner cancel.** `owner_cancelled` is not retryable. A cancelled run is not
reopened. Its work is redone by creating a new run.

## 21. pi headless adapter (DPES-813)

`agents/pi.py` is the pi port for `Ports.run_worker`. It reuses `trusted_sandbox`,
`trusted_supervise`, and `replay`, and adds no second contract. The pinned tool tuple is checked
against `TOOL_REPLAY_CLASSES` by test. Real pi, Vertex, and bubblewrap runs are not proved in this
section (gaps 43 to 48).

### 21.1 Argv, environment, and configuration

- `pi_args` emits the approved contract: `--mode json --provider google-vertex --model <model>
  --thinking <level> --no-session --no-extensions --no-mcp --no-skills --no-prompt-templates
  --no-themes --no-context-files --approve --tools read,write,edit,bash -- <task>`. The task is
  always after `--` (`test_the_argv_is_the_approved_pi_contract_with_the_task_last`,
  `test_every_allowlisted_thinking_level_reaches_the_argv`).
- The model must be in `VertexModel` (`gemini-3.8-flash`), and the thinking level in `Thinking`
  (`test_a_model_outside_the_vertex_allowlist_is_refused`,
  `test_a_thinking_level_outside_the_allowlist_is_refused`).
- A task that is empty, starts with `-`, contains NUL, or exceeds 64 KiB (`MAX_TASK_BYTES`) is
  refused before it reaches argv (`test_a_task_that_could_read_as_a_flag_or_is_empty_or_binary_is_refused`,
  `test_an_oversized_task_is_refused_before_it_reaches_argv`).
- `PI_VERSION` is `1.3.0`, the version the image installs
  (`test_the_pi_version_is_the_version_the_image_installs`).
- The child environment is `worker_env()`: offline, telemetry off, and no credential-like names
  (`test_the_worker_environment_is_offline_and_holds_no_model_credentials`).
- `cloud/config/pi/models.json` sets `providers.google-vertex.apiKey` to the sentinel
  `gcp-vertex-credentials`. The sentinel is not a secret. It makes pi use the SDK's default
  credential chain, the worker service account through the metadata server, instead of an ADC file
  (upstream `earendil-works/pi#5323`). The file is checked for exact content
  (`test_models_json_routes_google_vertex_through_the_default_credential_chain`). gap 45 covers its
  placement.

### 21.2 Verdict from the event stream

pi exits 0 when a model call fails, so the verdict comes from the JSON-mode stream, not the exit code.

- The last assistant `message_end` decides. `stopReason` `stop` is completed. `error` is classified
  from `errorMessage`. `length`, `aborted`, and any other value fail the turn
  (`test_the_last_assistant_message_decides_not_an_earlier_error`,
  `test_turns_that_did_not_stop_cleanly_are_failed`).
- Error classification checks throttle markers first (429, `resource_exhausted`, rate limit, too
  many requests, quota exceeded), then auth markers (401, 403, unauthenticated, unauthorized,
  permission denied, forbidden, credentials), and otherwise reports a model error. Raw provider
  text never leaves the judge (`test_vertex_authentication_failures_are_classified_as_auth`,
  `test_vertex_throttling_is_classified_as_throttled`,
  `test_other_model_errors_are_model_error_and_the_raw_text_never_leaves_the_judge`).
- A tool outside `read`, `write`, `edit`, `bash` is a policy violation, even when the turn stops
  cleanly (`test_a_tool_outside_the_allowlist_is_a_policy_violation_even_when_the_turn_stops_cleanly`).
  If the last `bash` result is an error, the turn fails
  (`test_a_last_shell_command_that_failed_is_not_reported_as_success`). A later passing `bash` clears
  it (`test_a_shell_command_that_failed_and_was_then_followed_by_a_passing_one_succeeds`).
- A malformed line fails the stream even when the turn stops cleanly. Malformed means non-JSON, not
  an object, no string `type`, a tool event without `toolName`, or a tool end without a boolean
  `isError` (`test_a_malformed_line_is_a_malformed_stream_even_when_the_turn_stops_cleanly`).
- Effective model and provider come from the last assistant message. Token usage comes from
  `message.usage.input` and `message.usage.output` when they are integers, and is `None` otherwise
  (`test_usage_is_none_when_the_stream_reports_none`). The shapes are unverified (gap 44).

### 21.3 Process integration

- `PiWorker` runs under `supervise` with `line_limit=MAX_PI_EVENT_BYTES` (1 MiB). Supervision's
  default 4096-character cap would truncate a `message_end` event and fail a valid run. The cap is
  raised for this caller only. The verdict judges each raw line. The log receives only the records
  `pi_events.map_pi_lines` keeps for that line (`pi message_end`, `pi tool_execution_start tool=bash`,
  and similar), so model text and tool payloads never reach the log. Each record is capped and
  redacted by that mapping (`test_the_log_carries_only_pi_event_records_never_model_or_tool_text`,
  `test_the_log_is_redacted_and_capped_and_the_raw_event_is_not_logged`). Verdict and log use
  separate caps: `MAX_PI_EVENT_BYTES` bounds the judged line in bytes, and `pi_events` bounds each
  log record in characters (`test_a_long_event_line_arrives_whole_so_its_stop_reason_is_judged`,
  `test_an_event_line_beyond_the_event_limit_fails_closed_as_malformed`,
  `test_a_raised_line_limit_lets_a_structured_event_arrive_whole` in `test_trusted_supervise.py`).
- A supervised `completed` outcome with a failed verdict becomes `failed`, so a zero exit is never
  success. A nonzero exit is `failed`, and the deadline is `timed_out`
  (`test_a_zero_exit_with_a_failed_verdict_is_failed_not_completed`,
  `test_a_nonzero_exit_is_failed_even_when_the_stream_looks_clean`,
  `test_the_deadline_stops_a_hung_worker_and_reports_timed_out`).
- The default wrapper is `bwrap_argv`, so the worker runs inside the sandbox
  (`test_the_worker_runs_under_bubblewrap_with_the_pi_command`). Through the bootstrap, only a clean
  verdict publishes `succeeded` (`test_a_clean_pi_run_publishes_one_succeeded_result_through_the_bootstrap`,
  `test_a_policy_violating_pi_run_publishes_failed_through_the_bootstrap`).
- Process tests use a fake pi, a Python script that records its argv and environment and prints
  scripted events. No pi binary runs in this repository (gap 43).

## Gaps

These gaps are known at the time of this revision. Each one is either a missing test, a missing
mechanism, or a BLOCKING dependency.

1. **Teardown reaches three of its eight outcomes live.** The route serves `running`, `fenced`, and `eligible` (`test_teardown_route.py`). `complete`, `expired`, `incomplete`, `retained-exception`, and `unavailable` are covered by unit tests only. Data presence cannot be proven absent, because the object store has no listing and checkpoint ids are not recorded durably. Partial cleanup is not exercised live, because `deletion_started` has no source. Blocking: an object listing or durable checkpoint-id index, a retention-exception source, and gap 51 for deletion.
2. **Restore from backup is modelled, not tested.** The restore floor is tested. No test restores a snapshot that still holds dedup rows and checks replay of an original or retry UUID.
3. **Old and new client recovery is not tested.** The version gate is tested. No test runs a client on a previous supported version against a newer controller.
4. **Only cursor version 1 exists.** The rule that an older supported version keeps its canonical identity has no case to exercise.
5. **Closed in this revision.** `FIELD_ORIGINS` is emitted as `x-origin` on each classified property, and each model's forbidden names as `x-forbidden-inputs`. `test_schema_authority.py` checks both against the parsers' screens.
6. **Closed in this revision.** Each BLOCKING field (`durable_root`, `checkpoint_ref`, `continuation_budget`) carries `x-blocking` naming its ticket in the committed JSON Schemas.
7. **Schema evolution is limited.** There is a drift check and a BLOCKING parity test. There is no compatibility rule between revisions.
8. **Closed in this revision.** The local-path screen refuses POSIX absolute, home-relative, `file:`, UNC, backslash-led, drive-letter, and leading-traversal forms. Relative names that only look like traversal, such as `..notes`, are accepted. `test_screening.py` pins both sides.
9. **Retry is outside the transition table.** The table tests cannot see `retry()`. Its refusal from every state except failed, including with a matching compare-and-set, is pinned by `test_state_machine.py::test_retry_is_refused_from_every_state_but_failed_even_with_matching_cas`. The allowed retry is covered only by direct state-machine and API tests.
10. **No live storage or launch adapter is covered.** All broker tests use in-memory stores, a manual clock, and a recording launcher. Signed-URL generation and generation preconditions are not checked against real Cloud Storage.
11. **Pre-existing audience-inventory gap, not introduced here.** `tests/fixtures/panel-assignments/candidate/evidence-output.txt` is tracked on `main` and absent from `docs/documentation-audiences.json`, so `bin/fm-doc-audience-check.sh` fails on it. This change does not classify it.
12. **Test-client deprecation.** `starlette.testclient` warns that `httpx` is deprecated. The warning is not a failure. It is a dependency follow-up.
13. **DPES-869 and DPES-870 are not implemented.** Durable root, checkpoint reference, continuation budget, restart budget, and the checkpoint capability remain refused until those tickets define them. DPES-869 has the replay classes for the pinned tools, a pure checkpoint manifest and restore gate, a `Storage` port with an in-memory fake, and checkpoint and result publication against that port, but no real storage adapter, runner, or restore wiring; the rest of its scope is listed in section 15.4 as gaps.
14. **The checkpoint manifest is not in the exported schemas.** `CheckpointManifest` and `CommitMarker` are Python-only. Promoting them into the protocol is a versioned change coordinated with DPES-870 under section 15.5.
15. **Checkpoint names are attempt-scoped, but restore needs a prior attempt's checkpoint.** `checkpoint_object_names()` places each checkpoint in its own directory under `runs/{run_id}/attempts/{attempt:06}/checkpoints/{checkpoint_id}/`. A resumed attempt must read the last committed checkpoint of an earlier attempt. That needs a run-prefix read capability (Q1), which is not specified or implemented.
16. **The manifest object has no byte cap.** Q2 limits the harness state and the workspace checkpoint, not the manifest itself.
17. **`checkpoint_commit_failed` is raised only against the in-memory fake.** `publish_checkpoint` raises it when a storage write or the commit transaction fails. The only storage implementation so far is the in-memory fake.
18. **Grep, Glob, LS, and metadata tools are unclassified.** Q3 names them replay-safe. They are not in the pinned pi-durable CodingTools (`read`, `write`, `edit`, `bash`), so they are never replayed until the pinned set exposes them and `replay.py` classifies them. Keeping them unreplayed deviates from the Q3 wording and is disclosed here.
19. **Platform and MCAO approval of the versioned DPES-869 contract is pending.** The Q1 to Q4 answers are not yet an approved contract. Record the approval here when it exists. The pilot stays disabled until then and until DPES-868 evidence exists.
20. **The Q4 version field is not added, and DPES-868 evidence is absent.** The pi adapter (section 21) does not carry the field. The real Vertex and bubblewrap run (AC2) waits on DPES-804 and DPES-811.
21. **Resolved by DPES-897 (decision 3): one checkpoint layout.** Each checkpoint is one directory per checkpoint id, named by `checkpoint_object_names` (section 8 and section 15.5 item 8). `object_name` raises for `checkpoint`, so the sequenced name is no longer derivable.
22. **Resolved by DPES-897 (decision 1): retry after a partial upload.** An identical checkpoint component is adopted. Differing bytes are refused with `checkpoint_integrity_failed`, and no marker or pointer is written. A result retry adopts identical bytes and refuses other bytes with `checksum_mismatch`, leaving the run open. Objects left by a failed attempt are still not deleted (see the checkpoint storage row in section 15.4).
23. **The worker image is not built or run.** No container daemon is available here. `test_worker_image.py` checks the Dockerfile text only. The build, the package install, and a smoke run are DPES-804.
24. **The sandbox is qualified only through an injected runner.** `qualify_sandbox` is tested with fake runners. No Linux bubblewrap run, no read-only root proof on a real kernel, and no observed out-of-memory kill. DPES-804.
25. **The sandbox shares the network.** Sandboxed commands can reach the metadata server and mint the worker service account token (section 19.3). This is a documented limitation, not a control. Qualification is DPES-804.
26. **No live or signed object storage.** The claim create-only write, the source read, and the result publish are the `ClaimStore`, `read_source`, and `publish_result` ports, tested with in-memory fakes. `mao_cloud.gcs_objects` and `mao_cloud.gcs_sdk` implement the `ObjectStore` port over Cloud Storage with the generation-zero create precondition, tested with mocks only; live precondition enforcement is unverified (DPES-807). The signer service and the controller's client are built and tested on fakes (DPES-899), but no deployed signer is proven, so live capability signing is unverified.
27. **No model access.** The pi adapter (section 21) is tested only with a fake pi process. The Vertex path through the metadata-server token is unbuilt, and no production code starts pi. DPES-819.
28. **No checkpoint writer.** The supervisor's checkpoint cadence is tested, but nothing publishes a checkpoint, and the checkpoint capability stays blocked. DPES-869.
29. **Resolved by DPES-811 AC3: the base SHA is carried and verified.** `BootstrapSource.base_sha` carries the claimed base into the projection the receipt and bootstrap share, and the worker refuses a bundle whose tip differs before any repository code runs (section 19.2). The field is added inside v1, which is not deployed, so the v1 projection digest changes. A bootstrap issued before this change has no `base_sha` and is refused by the schema, never run unverified. Decision for the project owner: whether v1 may change in place before deployment, or must bump to v2 first.
30. **Worker service account grants are not in this repository.** `roles/aiplatform.user` alone, with no control-plane or data-plane permissions, is an infrastructure requirement. No ticket named in the brief covers it, so it is flagged for decision.
31. **The result limits are decided (DPES-897 decision 4 and DPES-817).**
    The result document keeps its 1 MiB cap (`MAX_RESULT_BYTES`), enforced before write.
    Each returned artifact has its own 200 MiB cap (`MAX_RESULT_ARTIFACT_BYTES`).
    The returned work bundle also has a separate 200 MiB cap (`MAX_RETURNED_WORK_BYTES`), enforced at archive build and on fetch as the project owner decided on 2026-10-22 (section 15.6).
    The brief's 200 MiB figure applies to returned artifacts, not to the result document.
32. **The 2/8 profile does not hold the declared disks.** Workspace 8 GiB plus tmp 1 GiB needs 9 GiB; the 2/8 profile has 8 GiB, so it is 1 GiB short. The brief's accounting counts the workspace as RAM. The workspace is a host bind mount, so it is RAM only if the intake volume is. Decision needed: resize the small profile, or back the workspace with disk.
33. **Attempts publish no commit or artifacts.** `commit_sha` and `artifacts` are empty in this slice. No ticket named in the brief covers worker output.
34. **DPES-871 is not referenced in this repository.** The brief names it among the tickets for stated gaps. Its scope is unknown here, so Firstmate must confirm it.
35. **Platform stop and its confirmation are built for the stored execution, behind the worker intake gate, and unverified live.** `CloudRunJobLauncher` refuses every launch and every stop until the launch gate opens (gap 72), so production never calls `jobs.run` and held capacity stays reserved. With the gate open (`intake_ready=True`, set by tests only), `jobs.run` returns the execution name in its Operation metadata. The broker stores it on the attempt as `RunRecord.execution_name`, by compare-and-set, and no public view or receipt carries it. `request_stop` posts `:cancel` to that one execution and returns without waiting. `execution_stopped` reads it: a 404 is absence, and a completed execution with no running task and no reconciling flag is termination. The Operation metadata shape, the cancel and get endpoints, and 404 as absence are unverified against live Cloud Run (`test_cloud_run_jobs.py`, `test_execution_handle.py`). An attempt whose launch was ambiguous, or whose answer named no execution, has no stored name. Its stop is refused and it is never reported stopped, so its capacity stays reserved under the unknown-launch alert. Cloud Run has no run or attempt filter, so that execution cannot be found from here. The planner's reconcile schedule and live confirmation are DPES-807.
36. **The worker does not poll the cancel marker in flight.** `cancel.json` is written. `mao_cloud.worker_control.ControlGate` reads it before each model call, at most every 15 s, through an injected reader, and refuses on a stop, a missing control object, or an identity mismatch (`test_worker_control.py`). No production reader exists, and nothing calls the gate during a model call, because the runner loop that would poll in flight is not in this tree (`runner.py` is planning only). That loop is DPES-807 with DPES-819.
37. **Bounded checkpoint on SIGTERM is not implemented.** A worker stopped by the platform does not upload a bounded checkpoint before it exits. DPES-871.
38. **Real GCP state, storage, and readiness are unverified.** The cancel path is checked only against the in-memory store, the in-memory object store, the manual clock, and the recording launcher. DPES-804.
39. **AC2 failure codes are missing.** `FailureCode` has no codes for out-of-memory, crash, auth, missing output, or provider transient failures, and no observation source produces them. AC2 typed-failure classification is therefore partial. DPES-819, or DPES-871 where it concerns shutdown.
40. **Kill-switch coverage with cancel is partial.** `test_continuation.py::test_cancel_requested_never_restarts_even_with_kill_switch_engaged` covers the continuation path. `test_admissions_switch.py` covers the admissions switch with cancel. Neither exercises the controller restarting or racing continuation with a switch activated between acceptance, allocation, and launch, which the DPES-818 acceptance evidence requires. That evidence needs DPES-807 and DPES-870.
41. **Held capacity is released only by confirmation, and never for an attempt with no execution name.** A run whose stop is never confirmed keeps its worker slot and daily reservation indefinitely, as section 15.5 and the capacity section require. For an attempt with a stored execution name, the launch adapter's confirmation of absence or termination releases it, once the attempt is terminal. An attempt that never reports reaches terminal through the existing liveness timeout first. An attempt with no stored name cannot be observed, so nothing in this build releases its capacity. Live confirmation is DPES-807. Releasing a nameless attempt needs a decision on how its execution is found, and is not built.
42. **Alert recorded, routing not owned.** An unknown launch past ten minutes sets `unknown_launch_alerted_at` once per attempt and writes an audit error line (see "Shared admission and execution capacity"; `test_unknown_launch_fence.py`). The run stays fenced throughout. Delivery to a person or pager is not built, and no ticket in DPES-816 owns that routing, so it needs an owner.
43. **Real pi 1.3.0 is not run.** The argv, including the `--` terminator and the comma-separated `--tools` list, and the `--no-*` flags are not verified against an installed pi. No pi binary runs in this repository. Live qualification is DPES-871, and the image build is DPES-804.
44. **The pi event fields beyond the observed set are unverified.** `message.model`, `message.provider`, and `message.usage.input` and `output` are read as reported. Absent or wrong-typed values are reported as unknown, and no real stream has confirmed the shapes. DPES-871.
45. **models.json is not placed where pi reads it.** The worker image does not copy `cloud/config/pi/models.json` to a location pi reads. `HOME` is `/tmp`, and pi's configuration directory is not in the fixed environment, so the sentinel is not yet in effect. DPES-804 for the image and DPES-819 for the credential path.
46. **Vertex location `us` is not delivered to pi.** The adapter passes no location, and the fixed environment has no location variable. The location mechanism is unverified. DPES-819.
47. **The pi port has no production wiring.** The task text is a constructor argument, and merged code defines no production source for it. The heartbeat is a no-op, and no log sink is attached. `production_ports()` still raises. DPES-807 and DPES-804 own the production ports. The task source needs a decision.
48. **Live verdict behavior is unproven.** The verdict rules are tested against fake streams only. Live code change, test run, cancel, and auth or throttle behavior are not observed. DPES-871.
49. **Closed by DPES-898: admission limits are policy data, and the controller takes its policy from the environment.** Team and owner worker slots, queue depths, the team daily worker seconds, and the queued deadline are the required `admission` block of the server policy. A missing or invalid block refuses to load with `PolicyError`. The controller reads the file named by `--policy` or `MAO_CLOUD_POLICY_FILE`, and exits 2 when neither names one. The repository holds no approved values; the project owner supplies them, and the deployed file is unverified (DPES-896). Changing a limit takes a broker restart, because no reload exists. Tests: `test_admission_policy.py`, `test_serve.py`.
50. **No reservation audit.** Nothing reports actual against reserved seconds per run, and duplicate-container overhead is not recorded anywhere because `RunRecord` has no field for it. Blocking: DPES-807, which supplies measured execution data.
51. **Retention is planned, not executed.** `sweeper.plan_retention` is dry-run planning. No code deletes objects, writes a cleanup record, enumerates physical inventory, or quarantines late cancelled uploads. The owner-scoped teardown route reports status only. Blocking: DPES-807 (storage conformance and adapter inventory).
52. **Retention durations are not approved.** The DPES-818 values (inputs 7d, results and checkpoints 14d, claims, status and state 30d, logs 14d) are subject to G01 approval. No shipped policy file sets them, so nothing is due in any deployment until they are configured. Owner: G01 approval.
53. **Alerts and Vertex budget monitoring are outside this repo.** Alert routing for control or Scheduler failures, uncertain dispatch, queue age, missing heartbeat or results, OOMs, throttles, reservation leaks, and artifact growth lives in the CAF monitoring configuration. The Vertex AI budget and usage monitoring for `dpe-platform-d` is CAF configuration too. None is evidenced here. Blocking: DPES-804 and DPES-871.
54. **Closed by DPES-898: operator drain and bulk cancel exist.** Drain and resume close and reopen the admissions switch (`POST /internal/v1/admissions:drain`, `:resume`, and `GET /internal/v1/admissions`). Bulk cancel is `POST /internal/v1/runs:bulk-cancel` over up to 50 explicit run ids, with typed per-run outcomes, and reuses the owner cancel path, so operator cancels record `owner_cancelled`. Tests: `test_admissions_drain.py`, `test_bulk_cancel.py`. The switch's durability remains gap 55.
55. **The admissions switch has no durable source.** `Ports.admissions` is a required `AdmissionSwitch`. The production `EnvironmentAdmissionSwitch` takes its starting state from `MAO_CLOUD_ADMISSIONS` when the process starts and changes only in that process. A drain therefore does not survive a restart, and a restart reads the environment again, which can reopen admissions. Durable storage, stale-cache behavior, and the operator interface are unverified. Blocking: DPES-807 and DPES-804.
56. **Restore reconciliation is not implemented.** Nothing reconciles cancellation, fences, accepted results, reservations, and dedup decisions after a restore before dispatch resumes. Blocking: DPES-807 (restore path).
57. **Real Google verification and the deployed API are untested.** Tokens are verified through google-auth against in-process test certificates (`auth_support.py`). No test fetches Google's live certificates, mints a token through IAM `generateIdToken`, or calls the deployed API, so the deployed caller and cross-owner smoke test is not run. Blocking: DPES-896.
58. **Ingress header preservation is unverified.** The invoker-service-account flow needs the external HTTPS load balancer to pass `Authorization` through to the service. The DPES-803 container trial did not exercise it. Blocking: DPES-806, which the follow-up story carrying this check depends on, and DPES-896 for the deployed run.
59. **The deployed access mode is not tied to a policy file in this repo.** The CAF variable `mcao_cloud_access_mode` is set outside this repo, and no policy file in this repo sets `access_mode`. Whether the deployed policy carries the same value is unverified. Until it does, an omitted value resolves to `allowlist`. Blocking: DPES-896.
60. **No aggregate cap on returned artifacts per result.** Each artifact is capped at 200 MiB, but nothing caps the total of all artifacts in one result. A project owner decision is needed on an aggregate limit.
61. **`publish_result` does not wrap a raw storage failure from its commit transaction.** `publish_checkpoint` maps a commit failure to `checkpoint_commit_failed` (gap 17). `publish_result` does not, so a raw `StorageFailure` can reach the caller.

62. **Archive parts have no capability row.** The five parts (`manifest.json`, `return.bundle`, `agent-final.json`, `run.log`, and `tests.json`) are named by `artifact_object_name`. `ObjectKind` has no artifact kind, and `CAPABILITIES` has no PUT row for the parts, so a runner has no signed upload capability for them. The capability shape needs a decision. Blocking: DPES-807 (storage conformance).
63. **The import stops at the receipt.** The import does not re-run local checks or trusted CI on the materialized tree, and it does not call the delivery handoff. An imported worktree is therefore not yet proof of a delivered change (AC5 partial). Which step owns those checks needs a decision.
64. **No explicit rebase path.** A bundle built on a base other than the request's base is refused as `wrong-base`. A reviewed rebase onto a changed base is not implemented (AC4 partial). Whether a rebase belongs in the import or in the runner needs a decision.
65. **Destination profiles are assumptions, not probes.** The three named profiles (`case-sensitive`, `case-insensitive-nfc`, and `case-insensitive-no-symlinks`) state what the operator assumes about a destination. Nothing probes a volume, and no uniform macOS behavior is claimed. Pilot destination evidence is needed before a profile is trusted. Blocking: DPES-871.
66. **No run against real object storage.** The archive, the gate, and the import run against in-memory storage and local git only. Blocking: DPES-807 (storage adapter) and DPES-804 (real GCP reachability and state).
67. **Fault injection is in-process.** Each import boundary raises a named injected fault, and a repeat fetch recovers from it (section 15.6, item 7). No test kills the process, cuts power, or fails between filesystem calls. Blocking: DPES-871 (cross-execution durability tests).

68. **The archive omits moa-proof evidence.** DPES-817 AC1 asks for the available moa-proof v2 evidence in the result archive. The archive has five parts and no proof part, so no moa-proof evidence is carried to the import. Blocking: DPES-819 (the runner produces the evidence), and a decision on the proof part's shape.
69. **The import lock is not a project lock.** The import takes its own non-blocking `flock` on `<git-common-dir>/mao-import/import.lock`. The cloud package has no existing project or worktree lock helper to reuse (`run_record.py` takes its own `flock`). Whether the repository has a project lock that this import must use is unverified. Needs a decision.
70. **The controller route does not use the archive gate.** `broker.py` still calls `accept_result` for a finished attempt. That call does not verify the archive parts, so a succeeded result with no archive is accepted on this route. `accept_archived_result` is the gate and is tested alone (section 15.6, item 2). AC2 of DPES-817 is partial until the route changes. Blocking: DPES-807 (result publication wiring).
71. **Nothing produces the archive or the checkpoint commit.** No runner module calls `build_archive`, `store_archive`, or `commit_checkpoint`. `runner.py` is planning only (gap 36), so a live attempt publishes no archive and no return bundle. Blocking: DPES-819 (the runner produces the archive) and DPES-807 (the runner loop).
72. **Worker intake is built behind the launch gate, and production keeps the gate closed.** The project owner chose option (a): a per-attempt intake object in the run's Cloud Storage prefix, fetched by the worker through a short-lived, read-only signed URL. That URL is the only container override besides the run and attempt identifiers. The controller writes the object create-only before `jobs.run`, mints the capability through the signer contract, and passes `MAO_RUN_ID`, `MAO_ATTEMPT_ID`, and `MAO_INTAKE_URL` (`cloud_run_jobs.py`). `mao_cloud.worker_entry` is the image entrypoint. It fetches and verifies the object, then runs the existing bootstrap (section 19.8). The path is tested end to end on fakes, including an altered-document negative (`test_worker_entry.py`). The gate stays closed in production (`CloudRunJobLauncher(intake_ready=False)`), so `jobs.run` is never called and no execution exists. Opening it needs a live-verified signer that can mint the read capability (DPES-899: the signer and client are built and tested on fakes, and the deployed call is unverified), object storage and the production worker ports (DPES-807 and DPES-804), and model access (DPES-819). Until the gate opens, a dispatched attempt cannot run, and dispatch must not be enabled in any deployment. The residual risks are in section 19.8.

## Shared admission and execution capacity

While the admissions switch is closed, create, start (queue entry), and retry are refused with `admissions_paused` (503) before any write.
The broker reads the switch once per such request, so reopening it takes effect on the next request.
Replays of an earlier create still answer, and reads, status, logs, and cancel are not gated.
Closing the switch never cancels or changes an admitted run; cancel stays an explicit owner or operator action.
The switch is a new protocol contract point; its durable source is an open gap (item 55).
Tests: `test_admissions_switch.py`.

Admission limits come from the server policy's `admission` block, so queue depths, worker slots, the team daily worker seconds, and the queued deadline are policy data rather than code constants. A missing or invalid block fails closed with `PolicyError`. Tests: `test_admission_policy.py`. The policy file is named by `--policy` or `MAO_CLOUD_POLICY_FILE` (gap 49).

The operator routes drain and resume the switch and report its status: `POST /internal/v1/admissions:drain`, `POST /internal/v1/admissions:resume`, and `GET /internal/v1/admissions`. They require the operator identity; the owner and the scheduler receive 403.
Bulk cancel is `POST /internal/v1/runs:bulk-cancel`. It validates the whole selection of up to 50 distinct run ids before any change, applies the owner cancel rule to each run, and returns a typed outcome per run. A cancelled run is never restarted, and its retry is refused.
Tests: `test_admissions_drain.py`, `test_bulk_cancel.py`.

Queue admission, retry admission, and dispatch selection use one consistent run snapshot and a shared store revision.
The store checks that revision and the run version in the same transaction that publishes the state change, reservation, and any retry deduplication record.
Every run write and restore advances the shared revision, so concurrent decisions for different runs cannot overbook queue depth, worker slots, or daily reservations.
Store restoration must also keep the revision above all persisted dispatch orders.
A conflicting admission must reread the snapshot before selecting or admitting again.

Dispatch selection ignores queued runs whose original deadline or queued timeout has passed before choosing each owner's FIFO head.
Each dispatch records a monotonic order allocated with the guarded admission commit, so owners take turns even when dispatch timestamps are equal.
The last dispatch order survives retry until the next dispatch replaces it.

A terminal lifecycle state, stale heartbeat, expired deadline, or uploaded result does not establish that all platform executions have stopped.
Such a run retains its worker slot and daily reservation, and cannot retry, until the launch adapter authoritatively confirms all executions for its run, attempt, fence, and claim have terminated or are conclusively absent, with no pending launch able to start later.
Unknown or failed observations retain capacity.
A dispatched execution that has never reported ten minutes after dispatch (`UNKNOWN_LAUNCH_ALERT_SECONDS`, equal to the startup timeout) gets a one-time alert on its attempt: `unknown_launch_alerted_at`, set by compare-and-set, with an audit error line.
The alert records a fact only. It does not release the reservation, extend the deadline, or authorize a launch or retry, and a new attempt clears it.
Periodic sweeps continue reconciling terminal records with outstanding reservations.
A sweep plans one page at a time, in run-id order, with a continuation cursor (`plan_sweep_page`, `next_after`). The page bounds the actions returned, not the runs read: a run that needs no action does not count against the limit, so a caller that drops the cursor still reaches the first due runs instead of stalling on an idle prefix. `None` means the walk is complete.
When a pass cannot confirm termination, the broker records `unsettled_reconciles` and `next_reconcile_at`. The planner waits for that time before hinting another reconcile, using a deterministic backoff of 30 seconds doubling to a 900-second cap, less up to a quarter of a per-run jitter.
Backoff delays only those reconcile hints. Liveness decisions and the unknown-launch alert are never delayed, and an explicit reconcile still repeats the stop request.
A confirmed release clears both fields. Tests: `test_reconcile_schedule.py` (planner paging and backoff) and `test_reconcile_batch.py` (the route).
The batch route (section 5) accepts the cursor and runs each planned reconcile through `Broker.reconcile`. It reads every run in the store on each call; a storage query that bounds those reads, and the Scheduler job that carries the cursor, are DPES-807. No public route serves sweeps.
After confirmation, a separate versioned settlement releases the reservation once and charges elapsed time through confirmation, capped at the reserved budget.
This is conservative admission accounting, not a measurement of billed platform execution time.
