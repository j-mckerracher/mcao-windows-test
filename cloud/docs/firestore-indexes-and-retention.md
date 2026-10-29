# Firestore queries, indexes, fields and retention

Scope: DPES-809. This states what the controller's Firestore code does at this commit, for the CAF
infrastructure side (DPES-900) to match. The layout itself is owned by the docstring of
`mao_cloud.firestore_store`; this file lists the queries, fields and retention status derived from it.
Gap numbers refer to the gap register at the end of [`protocol-v1.md`](protocol-v1.md).

**Retention is NOT active.** No code writes an expiry field, and no code deletes a Firestore document.

Nothing here is evidence of a live Firestore backend. Every result comes from the in-memory fake and
the SDK adapter tested against a fake client. `mao_cloud.production` composes the SDK client, but no
live Firestore database has been exercised. A real backend is DPES-807 work.

## 1. Queries and scans

Each query is one equality filter on one field. The port (`FirestoreTransaction.query`) takes no
ordering, range, limit or second filter, so the code cannot issue a query that needs a composite index.

| Operation | Source | Collection | Filter | Composite index |
|---|---|---|---|---|
| Admission snapshot | `FirestoreRunStore.admission_snapshot` | `runs` | `holds == true` | no |
| Admission snapshot | same | `runs` | `state == "queued"` | no |
| Admission snapshot, once per distinct attribution day of a queued run | same | `runs` | `attribution_day == <ISO date>` | no |
| Admission snapshot | same | `owners` | full scan | no |
| Batch reconcile route (`POST /internal/v1/runs:reconcile`) | `FirestoreRunStore.list_runs` | `runs` | full scan | no |

Point reads by document id, in a transaction, need no index:

- `runs/{run_id}`, `idempotency/{key}`, `requests/{key}`, `owners/{key}`, `admission_layout/current`,
  `meta/restore_floor`.
- `admission_shards/{nnn}`: all 16 in each admission snapshot and each compare-and-set that passes an
  expected revision; one per run mutation otherwise.
- `runs/{run_id}` for each owner document the snapshot scans (the owner index).

Required index posture: automatic single-field indexing must stay on `runs.holds`, `runs.state` and
`runs.attribution_day`. Without it, the three admission queries fail.

## 2. Collections and fields written

`key` is the lowercase hex SHA-256 of `owner_id` and `request_uuid` joined by the unit separator, as
produced by `_digest`. `requests/{key}` and `idempotency/{key}` share this key for one create.

| Document | Written by | Fields |
|---|---|---|
| `runs/{run_id}` | create, compare-and-set | `record` (RunRecord as JSON), `state` (string), `holds` (bool), `attribution_day` (ISO date string) |
| `idempotency/{key}` | create; compare-and-set when given an idempotency record | IdempotencyRecord as JSON. Timestamps are ISO strings, including `expires_at` |
| `requests/{key}` | create only | `run_id` |
| `owners/{digest(owner_id)}` | create or compare-and-set, when the run's dispatch wins | `owner_id`, `run_id`, `dispatch_order` (int), `dispatched_at` (ISO string or null) |
| `admission_shards/{nnn}` | create, compare-and-set (one shard per mutation) | `revision` (int). The shared revision is the sum of the 16 shards |
| `admission_layout/current` | once per database, by `python3 -m mao_cloud.provision` (which calls `provision_admission_layout`), run by an operator identity and not at service start | `shards` (int) |
| `meta/restore_floor` | not written by this module | `floor` (ISO string), read by `restore_floor()` |

Collections the code does not use: `attempts`, `reservations`, `cleanup_inventory`. No code path
reads or writes them.

## 3. Retention status by record class

Retention is not active because:

- The retention durations are not approved. The shipped policy has no `retention` block (runbook,
  gap 52).
- Deletion is not implemented. Nothing records or verifies a cleanup (gap 51).
- `mao_cloud.sweeper.plan_retention` is a planning function. Nothing applies its output.
- Deleting a run while `owners/{digest(owner_id)}` still names it breaks admission. The snapshot raises
  `FirestoreStoreError: owner index names a run that is not stored` (`firestore_store.py`, snapshot).

| Class | Documents | Retention now | Constraint for any future deletion |
|---|---|---|---|
| Runs | `runs/{run_id}` | kept indefinitely | Not deletable while `owners/{digest}` names it. Eligible only once terminal and settled (runbook, data deletion and retention) |
| Idempotency | `idempotency/{key}` | kept indefinitely; no delete path | `expires_at` is the logical end of replay: creation plus 7 days (`IDEMPOTENCY_TTL`). Replay past it is refused as `IDEMPOTENCY_EXPIRED`. |
| Requests | `requests/{key}` | kept indefinitely | Orphan lookup when the idempotency record is missing. Must not be deleted while its request UUID is inside the admission window, or a duplicate create is possible. |
| Owners | `owners/{digest}` | one document per owner, overwritten, does not grow with history | Must never name a deleted run |
| Shards, layout, meta | `admission_shards`, `admission_layout`, `meta` | never expire | Required for the shared revision and the shard-count check |
| Attempts, reservations, cleanup inventory | not created | n/a | none |
| Object storage classes (inputs, results and checkpoints, claims, status and state, logs) | not in Firestore | no Firestore age in code | `RetentionClass` in `sweeper.py` is planning-only |

The DPES-807 lifecycle durations (7, 14, 30 and 14 days) are not approved in code.

## 4. Field and index diffs for the CAF side

| Item | Code at this commit | Change the CAF side must make |
|---|---|---|
| `expire_at` field | No writer. The string does not appear in `cloud/src` or `cloud/tests`. | Leave the TTL policy on `expire_at` inert, or remove it. Do not describe status retention as active. |
| `idempotency.expires_at` | Written as an ISO string, not a Firestore timestamp. | Do not point a TTL policy at it. Firestore TTL acts on timestamp-typed fields (documented behaviour, not verified here). Any future deletion needs a separate decision. |
| Composite indexes on `runs`, `attempts`, `reservations`, `idempotency`, `cleanup_inventory` | None needed. | Remove these declarations (DPES-804 report, gap G-5). Create no composite indexes for this code. The exact field lists are in CAF's storage document, which is not in this repository. |
| Single-field index on `runs.holds`, `runs.state`, `runs.attribution_day` | Required by all three admission queries. | Keep automatic indexing on all three. Do not exempt them. |
| Collections `attempts`, `reservations`, `cleanup_inventory` | Not created or read. | Do not create them for this code. |

## 5. Evidence

- `cloud/tests/test_firestore_query_contract.py`:
  - the admission snapshot issues only the three equality queries on `runs` and one scan of `owners`;
  - `list_runs` is one unfiltered scan of `runs`;
  - no module under `mao_cloud` mentions the expiry field.
- `cloud/tests/test_firestore_sdk.py::test_query_matches_one_equality_filter_and_reads_inside_the_transaction`:
  the SDK adapter sends one equality `FieldFilter` and reads inside the transaction.
- `cloud/tests/test_firestore_run_store.py::test_snapshot_reads_capacity_and_day_records_not_history`:
  the snapshot reads capacity and attribution-day records, not history.
- `cloud/tests/test_reconcile_batch.py`: the batch route pages over `list_runs` output.

## 6. Not evidenced

- No Firestore backend has run these queries. The tests use the in-memory fake.
- The Firestore engine's handling of these indexes is not tested. DPES-807 owns that.
- CAF's storage document is not in this repository. Its index field lists were not inspected.
- TTL behaviour on string-typed fields is documented Firestore behaviour. It has not been checked
  against the project.
- Deletion cost and read cost at scale are not measured. The batch reconcile route reads every run on
  each call.
