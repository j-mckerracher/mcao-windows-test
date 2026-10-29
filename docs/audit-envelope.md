# Audit envelope v1

The audit envelope is the versioned, machine-validated record shape for MCAO audit events, and this page owns its contract.
`docs/audit-envelope.v1.schema.json` is the machine-readable form of this contract.
`bin/fm-audit-envelope.py`'s header owns validator mechanics.
This version defines the envelope and its validator; producers and any shipping or query transport are later work, and the per-home store that holds envelopes is specified in [audit-writer.md](audit-writer.md).
The fleet activity ledger ([fleet-ledger.md](fleet-ledger.md)) remains a separate contract with its own version 1 record format.

## Versioning

Every envelope carries `"schema": "mcao.audit-envelope.v1"`.
Version 1 is closed: a member not listed below is a rejection at every level, so readers never guess at unknown fields.
Adding or changing a member requires a new schema version, `mcao.audit-envelope.v2`, with its own schema definition; v1 readers reject v2 records.
Fleet ledger records are not envelopes.
They are checked with `--definition fleet_ledger_v1` against the frozen `$defs/fleet_ledger_v1_record` snapshot in the same schema file, which keeps the base members that v1 ledger readers rely on.

## Members

| Member | Type | Rule |
| --- | --- | --- |
| `schema` | string | Constant `mcao.audit-envelope.v1`. |
| `event_id` | string | UUIDv7 in lowercase canonical form. |
| `seq` | integer | Per-home sequence number, `0` or greater. Gap detection is the writer's job, not the validator's. |
| `ts` | string | UTC time to the millisecond, `YYYY-MM-DDTHH:MM:SS.mmmZ`. |
| `prev_hash` | string or null | `sha256:` followed by 64 lowercase hex characters of the previous envelope. Null exactly when `seq` is `0`. Canonical serialization for hashing is the writer's rule. |
| `event` | string | Dotted lowercase name, such as `task.dispatched`. |
| `category` | string | One of `lifecycle`, `delivery`, `status`, `supervision`, `identity`. First cut for v1. |
| `outcome` | string | One of `success`, `failure`, `rejected`, `not_applicable`. First cut for v1. |
| `platform` | string | Constant `mcao`. |
| `home_id` | string | UUIDv4 in lowercase canonical form, created once per home and stored in the home's private config. See Identity. |
| `day_key` | string | `YYYY-MM-DD`, the calendar date of `ts` in `user.timezone`. See the day key section. |
| `session_id` | string | Non-empty. |
| `task_id` | string | Optional. Non-empty when present; the firstmate task id the event concerns. |
| `agent` | object | `role` (`firstmate`, `secondmate`, `ship`, or `scout`), `harness` (non-empty string), and `model` (string or null). |
| `actor` | object | `type` (`user`, `agent`, or `system`) and `id` (non-empty string). When `type` is `user`, `id` must equal `user.id`. |
| `repo` | string or null | `owner/name`. |
| `pr` | string or null | The PR's full `https://` URL. |
| `commit` | string or null | 40 lowercase hex characters. |
| `user` | object | `id` (`hmac-sha256:` plus 64 lowercase hex characters, see Identity), `identity_source`, and `timezone`, as described below. |

## Identity

`home_id` is a UUIDv4 generated once when a home is first configured and stored in that home's private config.
It is not derived from the machine or from any user identity.
`user.id` is `hmac-sha256:` followed by the lowercase hex of HMAC-SHA256 over the trimmed resolved identity, keyed by the home's private key.
The identity resolves in this order, and the first source that resolves wins: GitHub login, then git email, then local account name.
`user.identity_source` names the source that resolved: `github_login`, `git_email`, or `local_account`.
`user.timezone` is the runtime IANA timezone name used for `day_key`.
Raw identity values never appear in any envelope member, in validator output, or in rejection paths.
An event is accepted only when `user.id` is non-null and not the `PENDING` marker, and `user.identity_source` is non-null.
The literal `PENDING` is a placeholder and never an identity, so the gate rejects it as unresolved.
An unresolvable identity therefore rejects the event with reason `identity_unresolved`, and no partial identity is emitted.
Case folding of the identity before hashing is not yet decided and is deferred to the producer task.

### Home key lifecycle

The home's private key is 32 random bytes, generated once when the home's `home_id` is created and stored beside `home_id` in that home's private config with owner-only file permissions.
The key is never logged, exported, or written into any envelope member, validator output, or rejection record.
There is no rotation in this version.
Rotating the key would change `user.id` for the same resolved identity and break identity continuity across the ledger, so rotation is a follow-up that must define how continuity is kept before it ships.
The producer implements this lifecycle; this version defines it and ships no key code.

## Day key

`day_key` is the calendar date of `ts` converted to `user.timezone`, computed with Python `zoneinfo`.
No timezone is hardcoded.
The producer computes it from the runtime timezone, and the validator recomputes it and rejects any mismatch with reason `day_key_mismatch`.

## Validation

`bin/fm-audit-envelope.py validate [--definition envelope|fleet_ledger_v1] PATH|-` checks one JSON document.
Exit status is `0` when accepted, `1` when rejected and recorded, and `2` for a usage or input error such as malformed JSON, duplicate keys, an unreadable file, or a rejection that could not be recorded.
An accepted run prints one JSON result line and writes nothing.
Rejection reasons are `identity_unresolved`, `schema_invalid`, and `day_key_mismatch`.
Paths are JSON Pointers to the failing members, and no member value is echoed.
Checks run in this order, and the first failing check decides the reason:

1. The identity gate: `user` must be an object whose `identity_source` is non-null and whose `id` is non-null and not the `PENDING` marker; a failure is `identity_unresolved`, and the gate runs before schema checks, so it wins even when other members are also wrong.
2. The schema: every member and format in the tables above; a failure is `schema_invalid`.
3. The genesis rule: `prev_hash` is null exactly when `seq` is `0`; a failure is `schema_invalid` at `/prev_hash`.
4. The actor binding: when `actor.type` is `user`, `actor.id` must equal `user.id`; a failure is `schema_invalid` at `/actor/id`.
5. The timezone and day key: `user.timezone` must be a known IANA name, reported as `schema_invalid` at `/user/timezone` when it is not; a `ts` that is not a real calendar time, or whose conversion to that timezone is out of range, is `schema_invalid` at `/ts`; the recomputed `day_key` must match, reported as `day_key_mismatch` otherwise.

## Rejection records

Every rejection appends one line to `audit-rejections.jsonl` in the home's state directory: `FM_STATE_OVERRIDE` when set, otherwise `FM_HOME/state`.
With neither set, the validator refuses to record and exits `2`, so a rejection is never silent.
The record holds only `v` (`1`), `rejected_at` (epoch seconds), `definition`, `reason`, and `paths`.
Those are fixed enums and schema-defined JSON Pointers, so a record never carries an identity value, a timezone, or any other input value.
A rejection that cannot be recorded, such as a missing state directory, prints no rejected result, reports the cause on stderr, and exits `2`.
The record is a home-local diagnostic. It is not an audit event, and it does not join the envelope hash chain.

## Fleet ledger compatibility

The schema's `$defs/fleet_ledger_v1_record` snapshot requires `v` (constant `1`), `ts` (integer seconds), `event`, and `task`, and it accepts any other member, since v1 readers ignore members they do not recognize.
Ledger records carry no `schema` member, so the envelope definition rejects them.
A v2 ledger record fails the snapshot's `v` constant, which is the intended signal that a v1 reader must not interpret it.

## Deferred

- The producer, which implements the home key lifecycle above.
  Every event must pass the sanitization boundary in [audit-sanitization.md](audit-sanitization.md) before it is written or shipped; that boundary is defined, and wiring producers to it is later work.
- Identity case folding before hashing.
- Final category and outcome vocabularies, which may change before the first producer ships.
- Shipping, SIEM transport, and query consumers.
