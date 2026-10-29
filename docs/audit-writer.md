# Audit writer

The audit writer is the per-home store that makes audit envelopes durable, sequenced, and verifiable.
`bin/fm-audit-writer.py`'s header owns the command-line usage, and this page owns the store contract.
[audit-envelope.md](audit-envelope.md) owns the record contract, and the writer accepts only envelopes that pass its validator.
Sanitization is upstream and is not performed here.

## Store layout

A store is one directory holding a checkpoint, numbered segments, and a lock file.
`checkpoint.json` holds canonical JSON with the members `last_hash`, `last_seq`, `segment`, and `v`, naming the last committed record.
Each `segment-<first seq as 20 digits>.jsonl` holds one canonical JSON record per line, each terminated by a newline.
A segment's name is the sequence number of its first record, so segment order needs no separate manifest.
Each record's `prev_hash` is `sha256:` followed by the SHA-256 of the previous stored line without its newline.
The first record has sequence `0` and a null `prev_hash`.
`.lock` is the store's advisory lock file, and verify creates it empty when it is missing.
Apart from that empty lock file, verify never writes a record, segment, or checkpoint.

## Appends

An append validates the envelope before it touches the store, so a rejected envelope creates no store and writes no segment.
A caller must not supply `seq` or `prev_hash`, because the writer assigns both.
A rejection is recorded through the envelope validator's rejection log, and identity values never enter that record.
Appends hold the store's exclusive lock, so concurrent producers take turns and each committed record gets a unique, increasing sequence number.
Before assigning a sequence number, an append verifies the whole store and applies only the recovery steps described below.
A segment is written in append mode and synced to stable storage before the checkpoint names it.
A new segment starts after `segment_records` records, which defaults to 1000, and the directory is synced so the new name is durable.

## Commit point

The checkpoint replace is the commit point, and a record is committed only once the checkpoint names it.
The checkpoint is published by writing `checkpoint.json.tmp`, syncing it, atomically renaming it over `checkpoint.json`, and then syncing the directory.
A failure before the rename rolls the record back by truncating the segment to its prior size and syncing that truncation.

## Sync policy

Durability uses `F_FULLFSYNC` where the platform provides it, which also flushes the drive cache on macOS.
Where full sync is unavailable, the writer falls back to `fsync`.
Success is reported only after every sync on the commit path returns without error, and a failed sync is reported as a failure.

## Failure reporting

A failed append exits 3 and never reports the event as recorded.
`may_be_durable` false means the record is not on disk, because the rollback succeeded or the record never reached the segment.
`may_be_durable` true means the record may already be on disk, because the rollback failed or the checkpoint was replaced before its directory sync failed.
A record reported as possibly durable may later be adopted by the next append, which surfaces it under `recovered.adopted`.

## Restart recovery

Every append and every verify checks the whole chain before it acts.
An unterminated trailing fragment in the last segment is truncated durably and reported as `torn_tail`.
Exactly one complete, valid record beyond the checkpoint is adopted by advancing the checkpoint to it, and it is reported as `adopted`.
Anything else is refused with its reason, sequence number, segment, paths, and the names of the intact segments, and nothing is repaired.

## Verify

Verify walks every segment and the checkpoint, and reports the first damage it finds with `corrupt`.
Refusal reasons are `hash_mismatch`, `seq_mismatch`, `segment_gap`, `segment_empty`, `unterminated_record` (in a closed segment), `malformed_line`, `checkpoint_invalid`, `checkpoint_missing`, `checkpoint_mismatch`, and `segment_invalid`, plus the envelope validator's reasons for a stored record.
`intact_segments` lists the segments that still validate, so an operator can see what survives.
Verify never repairs evidence, so a modified record is reported as corruption and the store is left unchanged.
Verify and every append read the whole store, so each call costs time in proportion to the number of records.

## Exit codes and results

`append` exits 0 when committed, 1 when the envelope is rejected and the rejection is recorded, 2 for a usage or input error or an unrecorded rejection, and 3 when the record is not committed.
`verify` exits 0 when the store is intact (a torn tail still exits 0), 1 when it is corrupt, and 2 for a usage error.
`project-ledger` exits 0 when it prints, 1 when the store is refused, and 2 for a usage error.
Result names are `committed`, `rejected`, `failed`, `refused`, `verified`, and `corrupt`.

## Fleet ledger v1 projection

`project-ledger` prints one canonical fleet-ledger v1 base record per committed envelope whose event is one of `task.dispatched`, `task.status`, `task.pr_ready`, `task.merged`, or `task.cleaned_up`.
Each record carries `v`, `ts` as integer epoch seconds, `event`, and `task`, plus `pr` when the envelope has one.
Envelopes without a `task_id` are skipped.
Members the envelope does not carry, such as `kind`, `project`, `state`, `key`, `text`, and `via`, are not reconstructed, and v1 readers ignore members they do not recognize.
Identity values never appear in the projection, and each projected line is checked against the `fleet_ledger_v1` definition before it is printed.

## Known limits

A coordinated rewrite of segments together with the checkpoint is undetectable without an external anchor.
Reverting the checkpoint by exactly one record is adopted as an ahead record, so that case also needs an external anchor to detect.
A record beyond the checkpoint is uncommitted and is not treated as evidence until it is adopted.
A failed append reported with `may_be_durable` true may later be adopted.
Verify cost grows with the store, and this design does not index segments.
