# Cost, retention and admissions runbook

Scope: DPES-818 and DPES-898. Each section states what the code does today. A step marked **not implemented**
cannot be performed from this repository. Gap numbers refer to the gap register at the end of
[`protocol-v1.md`](protocol-v1.md).

## Ownership and on-call

No owner or on-call rota is recorded in this repository. **Not implemented:** an owner for
alert routing (gap 42, gap 53). An owner is needed before the pilot runs.

## Admission limits

The `admission` block of the server policy sets every admission limit. All seven fields are
required positive integers:

- `version`: the document version, reported as `policy_version` by the admissions status route.
  Raise it on every change.
- `team_queue_depth` and `owner_queue_depth`: queued runs allowed for the team and for one owner.
  A full queue refuses with `queue_full` (HTTP 429).
- `team_worker_slots` and `owner_worker_slots`: runs that may hold a worker at once, team-wide and
  per owner.
- `team_daily_worker_seconds`: reserved worker seconds the team may hold for one UTC admission day.
- `queued_deadline_seconds`: how long a queued run may wait before it expires.

A missing block, a missing or unknown field, a non-integer, or a value below 1 refuses to load with
`PolicyError`. No built-in default fills the gap, and the repository holds no approved values.
The project owner supplies them.

- **Policy source:** the controller reads one policy file, named by `--policy <file>` when given,
  otherwise by the `MAO_CLOUD_POLICY_FILE` environment variable. The image carries no policy file,
  so the deployment mounts the approved file and sets that variable to its path. With neither set,
  the controller exits 2 naming `MAO_CLOUD_POLICY_FILE` and binds no port.
- **Policy content:** the file holds every required key listed in
  [`protocol-v1.md`](protocol-v1.md#14-server-policy), the seven `admission` values, and
  `api_audience`, the bare https origin of the control API that caller tokens must carry.
- **Start state:** `MAO_CLOUD_ADMISSIONS` is required, with `open` or `closed`, and has no default.
  First activation sets `closed`, so admissions stay drained until an operator resumes them.
- **Change a limit:** edit the policy document, then restart the broker. A running broker keeps the
  policy it loaded, and no reload exists.
- **Check a document:** `python3 -m mao_cloud.serve --policy <file>`, or the same with
  `MAO_CLOUD_POLICY_FILE` set and `--policy` omitted. It prints the `PolicyError` and exits 2 for an
  invalid document or one without `api_audience`, exits 2 when no file is named, and exits 2 for a
  `PORT` that is not a port number. With a valid document and a missing `MAO_CLOUD_*` setting, it
  exits 2 and names each missing setting. With a complete configuration the production clients open
  and the server binds `PORT`; a client that cannot open exits 3 with the refusing-to-start message and
  binds nothing. The signer client is wired at `MAO_CLOUD_SIGNER_URL`, and the deployed signer is
  unverified (DPES-899); live Firestore, Storage, and Jobs behavior is unverified.
- **Evidence:** `tests/test_admission_policy.py` proves each limit follows the document with no code
  change, and that an invalid or missing block fails closed. `tests/test_serve.py` proves the
  environment and argument sources, the no-source refusal, and that the environment-selected start
  comes up with admissions closed.

## Admissions switch and drain

While admissions are drained, the server refuses three operations before any write:

- create a run (`POST /v1/runs`), with `admissions_paused` (HTTP 503);
- start a run into the queue (`POST /v1/runs/{id}:start`), with `admissions_paused`;
- retry a run (`POST /v1/runs/{id}:retry`), with `admissions_paused`.

Replays of an earlier create still answer. Reads, status, logs, cancel and reconcile are not
affected. Admitted runs keep dispatching and running. A stopping run keeps its worker slot and daily
reservation until termination is confirmed.

The operator routes need the operator credential. Owner and scheduler credentials receive
`not_allowed` (HTTP 403).

- **Drain:** `POST /internal/v1/admissions:drain`. Returns `state: "drained"`. Repeating it is
  harmless.
- **Resume:** `POST /internal/v1/admissions:resume`. Returns `state: "open"`. The next request sees
  it, so no restart is needed.
- **Status:** `GET /internal/v1/admissions`. Returns `state`, `policy_version`, and `holding_runs`,
  the number of runs that hold a worker slot or reservation now.
- **Evidence:** `tests/test_admissions_drain.py` and `tests/test_admissions_switch.py` prove refusal
  before any write, that admitted work still dispatches, that cancel is not gated, and that resume
  takes effect on the next request.

The production switch takes its starting state from `MAO_CLOUD_ADMISSIONS` when the process starts
and changes only in that process, so it has no durable source (gap 55). A restart reads the
environment again and may reopen admissions, so check status after any restart and drain again if
needed.

## Cancel and reconcile

- **Cancel one run:** the owner calls `POST /v1/runs/{id}:cancel`. A run that has not launched
  becomes `cancelled`. A run that has launched becomes `cancel_requested` and keeps its worker slot
  and daily reservation until the launch adapter confirms its executions have stopped.
- **Reconcile one run:** the controller calls `POST /internal/v1/runs/{id}:reconcile`. Unknown or
  failed stop observations keep the reservation.
- **Reconcile due runs (batch):** the controller calls `POST /internal/v1/runs:reconcile` with the
  operator or scheduler credential and no body. `limit` takes 1 to 50 (default 50); any other value
  is refused as `invalid_request`. `after` takes a run id, and a malformed one is refused the same
  way. Pass the response's `next_after` back as `after` to continue; a `null` `next_after` means the
  walk is complete. The response's `reconciled` list has one entry per run acted on: `reconciled`
  with the state it now has, or `conflict`, `not_found` or `refused` with a code. A failed run does
  not stop the page. The route reads every run in the store on each call, and no Scheduler job in
  this repository calls it yet (DPES-807).
- **Plan a sweep (dry run):** `mao-cloud-sweeper --policy <file> --records <file>` prints the planned
  liveness and reconcile actions. It applies nothing.

## Bulk cancel

The operator cancels admitted work by selection through `POST /internal/v1/runs:bulk-cancel`, with
the operator credential and the body `{"protocol_version": "v1", "run_ids": ["run_..."]}`.

- **Selection** is an explicit list of 1 to 50 distinct run ids. There is no filter by owner or
  state.
- **Validation comes first.** An empty, duplicate, over-limit or malformed id, or an unknown field,
  returns 400 and changes nothing.
- **Each run follows the owner cancel rule.** A run that has not launched becomes `cancelled`. A
  launched run becomes `cancel_requested` and keeps its worker slot and daily reservation until
  termination is confirmed. Drain does not gate bulk cancel.
- **Outcomes** come back in request order, one per run: `cancelled`, `cancel_requested`,
  `already_terminal`, `not_found`, or `refused` with an error `code`. Each carries `state` and
  `holding_capacity`.
- **Nothing restarts.** Retry of a cancelled run is refused with HTTP 409.
- **Failure code:** an operator cancel records `owner_cancelled`, the same as an owner cancel.
- **Evidence:** `tests/test_bulk_cancel.py`.

## Drain and bulk cancel procedure

1. Drain (`POST /internal/v1/admissions:drain`) and confirm `state` is `drained`.
2. Bulk cancel the runs to stop, in batches of up to 50 ids.
3. Read `holding_runs` from status. A launched run leaves that count only after its termination is
   confirmed, which needs a production launcher that can stop and confirm executions (gap 35,
   DPES-807). The Cloud Run Jobs launcher refuses stops and never reports one confirmed, so launched
   runs keep their capacity.

## Unmet items for DPES-898

- Approved admission values and the deployed policy file. The repository holds no approved values,
  and no deployment has delivered a policy file yet. Blocking: DPES-898 itself until the project
  owner supplies the values, and DPES-896 for the deployed run (gap 59).
- Durable admissions source, so a drain survives a restart (gap 55). Blocking: DPES-807, DPES-804.
- Stop confirmation from a production launcher, so held capacity releases (gap 35). Blocking:
  DPES-807.
- Policy reload without a restart, and production broker wiring. Blocking: DPES-806, DPES-811.
- Audit record and operator-specific failure code for drain and bulk cancel. The audit log records
  credential checks only. Blocking: DPES-815 (durable status, logs and lifecycle events), mapped from
  that ticket's scope; the project owner decides whether it covers this.
- Bulk cancel selection by owner or state. Only explicit run ids are supported. Blocking: DPES-816
  (cancel and reconcile), mapped from that ticket's scope; the project owner decides whether it covers
  this.

## Data deletion and retention

- **Firestore queries, indexes and fields:** [`firestore-indexes-and-retention.md`](firestore-indexes-and-retention.md)
  states what each record class keeps today and the index and field changes the CAF side must make.
  Retention is not active.
- **Durations:** policy field `retention` holds four approved ages in days: `inputs_days`,
  `results_checkpoints_days`, `claims_status_state_days`, `logs_days`. A policy without the block has
  no approved durations, and nothing is due. The values are subject to G01 approval (gap 52). Do not
  add them to a shipped policy without that approval.
- **Eligibility:** every object class ages from the run's terminal-state timestamp (`state_entered_at`), not from admission or individual object creation.
  A class is due only after the run is terminal, its capacity reservation is settled, and that class's approved duration has elapsed from that timestamp.
  A live, queued, or uncertain run is never due.
- **Plan:** `mao_cloud.sweeper.plan_retention` lists runs with due classes. It is a library
  function. The sweeper CLI does not print it yet.
- **Delete:** **not implemented** (gap 51). Nothing records a cleanup, enumerates physical
  inventory, or reports owner-scoped cleanup status. Do not delete run objects by hand: a deletion
  that is not recorded cannot be verified, resumed, or audited.

## Backup and recovery

- **Restore reconciliation:** **not implemented** (gap 56). Nothing reconciles cancellation,
  fences, accepted results, reservations, and dedup decisions after a restore.
- **Until that exists:** keep admissions drained after any restore. Do not resume dispatch from a
  restored snapshot. Do not release an uncertain reservation by hand.

## Rollback

Drain admissions before any rollback. The rollback procedure belongs to DPES-819 and is not
defined in this runbook.

## Alerts

**Not implemented** in this repository (gaps 42 and 47). The conditions DPES-818 requires are:
control or Scheduler failures, uncertain dispatch, queue age, missing heartbeat or results, OOMs,
throttles, reservation leaks, and artifact growth. The Vertex AI usage and budget alerts for
`dpe-platform-d` are CAF configuration, not evidenced here.

## Reservation audit

**Not implemented** (gaps 43 and 44). Actual against reserved seconds is available per run through
`settle_capacity`, but no report is produced. Duplicate-container overhead has no field.
