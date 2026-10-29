#!/usr/bin/env bash
# tests/fm-audit-writer.test.sh - the per-home audit store, driven through
# bin/fm-audit-writer.py. docs/audit-writer.md owns the store contract and
# docs/audit-envelope.md owns the record contract. Failure paths run for real: a driver
# process patches the writer's fsync, checkpoint, and rollback seams so each injected
# fault reaches the same code the production path uses.
set -u

# shellcheck source=tests/lib.sh
. "$(dirname "${BASH_SOURCE[0]}")/lib.sh"

TMP_ROOT=$(fm_test_tmproot fm-audit-writer)
# Pin both homes so no test can reach a real home's state, even when the caller has one set.
export FM_HOME="$TMP_ROOT/home"
export FM_STATE_OVERRIDE="$TMP_ROOT/state"
mkdir -p "$FM_STATE_OVERRIDE"
WRITER="$ROOT/bin/fm-audit-writer.py"
ENVELOPE="$ROOT/bin/fm-audit-envelope.py"
DRIVER="$TMP_ROOT/fault-driver.py"
# user.id and home_id carry their pinned formats, so these fixtures are well-formed pinned values.
USER_ID="hmac-sha256:0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef"
HOME_ID="5f1c2b3a-4d5e-4f60-8a71-92b3c4d5e6f7"
# Epoch seconds of 2026-10-16T02:00:00Z, the ts of every fixture.
LEDGER_TS=1792116000
TORN_TAIL='{"schema":"mcao.audit-env'

# Chicago is on CDT (UTC-5) in October 2026, so 02:00Z on 2026-10-16 is still the 15th there.
BASE=$(jq -cn --arg user "$USER_ID" --arg home "$HOME_ID" '{
  schema: "mcao.audit-envelope.v1",
  event_id: "018f3b7e-9c1a-7d2e-8f3a-000000000000",
  ts: "2026-10-16T02:00:00.000Z",
  event: "task.dispatched",
  category: "lifecycle",
  outcome: "success",
  platform: "mcao",
  home_id: $home,
  day_key: "2026-10-15",
  session_id: "sess-1",
  task_id: "fix-login",
  agent: {role: "firstmate", harness: "claude", model: null},
  actor: {type: "user", id: $user},
  repo: "mc-innersource/motif-hve-multi-agent-orchestrator",
  pr: null,
  commit: null,
  user: {id: $user, identity_source: "github_login", timezone: "America/Chicago"}
}')

# The fault driver loads the writer in-process, arms the faults named by its second argument,
# and runs main. Faults are "point:N" (fail call N of that seam) or "rollback_fails".
cat >"$DRIVER" <<'PY'
import errno
import importlib.util
import sys

writer, faults, argv = sys.argv[1], sys.argv[2], sys.argv[3:]
spec = importlib.util.spec_from_file_location("fm_audit_writer", writer)
mod = importlib.util.module_from_spec(spec)
spec.loader.exec_module(mod)

plan = {}
for item in faults.split(","):
    point, _, nth = item.partition(":")
    plan[point] = int(nth) if nth else True
calls = {}


def hit(point):
    calls[point] = calls.get(point, 0) + 1
    return plan.get(point) == calls[point]


def wrap(point, original):
    def wrapped(*args):
        if hit(point):
            raise OSError(errno.EIO, "injected " + point)
        return original(*args)
    return wrapped


mod.sync_fd = wrap("sync_fd", mod.sync_fd)
mod.sync_dir = wrap("sync_dir", mod.sync_dir)
original_publish = mod.AuditStore._publish_checkpoint


def publish(self, state):
    if hit("publish"):
        raise OSError(errno.EIO, "injected checkpoint write failure")
    return original_publish(self, state)


mod.AuditStore._publish_checkpoint = publish
if "rollback_fails" in plan:
    mod.rollback = lambda path, size: False
raise SystemExit(mod.main(argv))
PY

# record <name> <n> [jq-filter]: write one envelope fixture with a unique event_id.
record() {
  local id
  id=$(printf '018f3b7e-9c1a-7d2e-8f3a-%012x' "$2")
  printf '%s\n' "$BASE" | jq -c --arg id "$id" ".event_id = \$id | ${3:-.}" >"$TMP_ROOT/$1.json" \
    || fail "could not build fixture $1"
}

# run_writer <args...>: one writer run; sets RC, OUT, and ERR.
run_writer() {
  OUT=$(python3 "$WRITER" "$@" 2>"$TMP_ROOT/err")
  RC=$?
  ERR=$(cat "$TMP_ROOT/err")
}

# run_faulted <faults> <args...>: one writer run with faults armed; sets RC, OUT, and ERR.
run_faulted() {
  local faults=$1
  shift
  OUT=$(python3 "$DRIVER" "$WRITER" "$faults" "$@" 2>"$TMP_ROOT/err")
  RC=$?
  ERR=$(cat "$TMP_ROOT/err")
}

# json_field <json> <jq-expr>: read one raw value from a writer result line.
json_field() {
  printf '%s' "$1" | jq -r "$2"
}

# segname <first-seq>: the segment file name the layout documents.
segname() {
  printf 'segment-%020d.jsonl' "$1"
}

# build_store <store> <count> [segment-records]: append <count> fixtures into a fresh store.
build_store() {
  local store=$1 count=$2 segment_records=${3:-1000} i name
  name=$(basename "$store")
  for i in $(seq 0 $((count - 1))); do
    record "$name-$i" "$i"
    run_writer append --store "$store" --segment-records "$segment_records" "$TMP_ROOT/$name-$i.json"
    assert_equals 0 "$RC" "building $name record $i must commit: $OUT $ERR"
  done
}

# store_manifest <store>: name and SHA-256 of every file, so a test can prove nothing changed.
store_manifest() {
  python3 - "$1" <<'PY'
import hashlib
import os
import sys

root = sys.argv[1]
for name in sorted(os.listdir(root)):
    path = os.path.join(root, name)
    if os.path.isfile(path):
        print(name, hashlib.sha256(open(path, "rb").read()).hexdigest())
PY
}

# check_chain <store>: re-derive the chain from the on-disk layout, independent of verify.
# It requires every sequence number and prev_hash link to agree, and the checkpoint to name the tip.
check_chain() {
  python3 - "$1" <<'PY'
import glob
import hashlib
import json
import os
import sys

store = sys.argv[1]
prev, seq = None, 0
for path in sorted(glob.glob(os.path.join(store, "segment-*.jsonl"))):
    for line in open(path, "rb").read().split(b"\n")[:-1]:
        doc = json.loads(line)
        if doc["seq"] != seq or doc["prev_hash"] != prev:
            sys.exit(f"link broken at seq {seq}")
        prev = "sha256:" + hashlib.sha256(line).hexdigest()
        seq += 1
ckpt = json.load(open(os.path.join(store, "checkpoint.json")))
if ckpt["last_seq"] != seq - 1 or ckpt["last_hash"] != prev:
    sys.exit("checkpoint does not name the chain tip")
print(seq)
PY
}

# tamper_line <file> <line-number> <old> <new>: edit one stored line in place.
tamper_line() {
  python3 - "$1" "$2" "$3" "$4" <<'PY'
import sys

path, number, old, new = sys.argv[1], int(sys.argv[2]), sys.argv[3], sys.argv[4]
lines = open(path, "rb").read().split(b"\n")
lines[number - 1] = lines[number - 1].replace(old.encode(), new.encode(), 1)
open(path, "wb").write(b"\n".join(lines))
PY
}

# drop_line <file> <line-number>: delete one stored line, as a lost write would.
drop_line() {
  python3 - "$1" "$2" <<'PY'
import sys

path, number = sys.argv[1], int(sys.argv[2])
lines = open(path, "rb").read().split(b"\n")
del lines[number - 1]
open(path, "wb").write(b"\n".join(lines))
PY
}

# forge_ahead <store> <fixture> <count>: add well-chained records past the checkpoint.
# A single writer never leaves more than one unpublished record, so this simulates damage.
forge_ahead() {
  python3 - "$1" "$2" "$3" <<'PY'
import hashlib
import json
import os
import sys

store, fixture, count = sys.argv[1], sys.argv[2], int(sys.argv[3])
path = os.path.join(store, sorted(n for n in os.listdir(store) if n.startswith("segment-"))[-1])
lines = open(path, "rb").read().split(b"\n")[:-1]
seq = len(lines)
prev = "sha256:" + hashlib.sha256(lines[-1]).hexdigest()
base = json.load(open(fixture))
with open(path, "ab") as out:
    for _ in range(count):
        doc = dict(base, seq=seq, prev_hash=prev, event_id="018f3b7e-9c1a-7d2e-8f3a-%012x" % (0xf00 + seq))
        line = json.dumps(doc, sort_keys=True, separators=(",", ":"), ensure_ascii=True,
                          allow_nan=False).encode("ascii")
        out.write(line + b"\n")
        prev = "sha256:" + hashlib.sha256(line).hexdigest()
        seq += 1
PY
}

test_append_assigns_sequence_and_hash_chain() {
  local i store="$TMP_ROOT/chain"
  for i in 0 1 2; do
    record "chain-$i" "$i"
    run_writer append --store "$store" "$TMP_ROOT/chain-$i.json"
    assert_equals 0 "$RC" "append $i must commit: $OUT $ERR"
    assert_equals committed "$(json_field "$OUT" .result)" "append $i must report committed"
    assert_equals "$i" "$(json_field "$OUT" .seq)" "append $i must take the next sequence number"
  done
  run_writer verify --store "$store"
  assert_equals 0 "$RC" "a clean store must verify: $OUT"
  assert_equals 3 "$(json_field "$OUT" .records)" "verify must count every committed record"
  assert_equals 2 "$(json_field "$OUT" .last_seq)" "verify must report the tip"
  check_chain "$store" >/dev/null || fail "the on-disk chain must validate link by link"
  pass "append assigns sequence numbers and every hash link validates"
}

test_concurrent_appends_commit_unique_increasing_sequence() {
  local store="$TMP_ROOT/concurrent" n=20 i pid
  local pids=()
  for i in $(seq 1 "$n"); do
    record "par$i" "$i"
  done
  for i in $(seq 1 "$n"); do
    python3 "$WRITER" append --store "$store" "$TMP_ROOT/par$i.json" \
      >"$TMP_ROOT/par$i.out" 2>"$TMP_ROOT/par$i.err" &
    pids+=("$!")
  done
  for pid in "${pids[@]}"; do
    wait "$pid" || fail "a concurrent append failed: $(cat "$TMP_ROOT"/par*.err)"
  done
  assert_equals "$(seq 0 $((n - 1)))" "$(cat "$TMP_ROOT"/par*.out | jq -r .seq | sort -n)" \
    "concurrent appends must commit each sequence number exactly once"
  run_writer verify --store "$store"
  assert_equals 0 "$RC" "the concurrent store must verify: $OUT"
  assert_equals "$n" "$(json_field "$OUT" .records)" "every concurrent append must be committed once"
  check_chain "$store" >/dev/null || fail "concurrent appends must leave a valid chain in file order"
  pass "concurrent appends get unique, increasing sequence numbers and a valid chain"
}

test_rotation_creates_segments_and_verifies() {
  local store="$TMP_ROOT/rotate"
  build_store "$store" 5 2
  run_writer verify --store "$store"
  assert_equals 0 "$RC" "a rotated store must verify: $OUT"
  assert_equals "$(segname 0) intact 2,$(segname 2) intact 2,$(segname 4) intact 1" \
    "$(json_field "$OUT" '[.segments[] | "\(.name) \(.status) \(.records)"] | join(",")')" \
    "rotation must start a new segment every two records"
  check_chain "$store" >/dev/null || fail "the rotated chain must validate across segments"
  pass "rotation creates segments named by their first sequence number and verifies across them"
}

test_fsync_failure_reports_failure_and_leaves_no_record() {
  local store="$TMP_ROOT/fsync"
  record fsync-0 0
  run_writer append --store "$store" "$TMP_ROOT/fsync-0.json"
  assert_equals 0 "$RC" "setup append must commit: $OUT $ERR"
  record fsync-1 1
  run_faulted "sync_fd:1" append --store "$store" "$TMP_ROOT/fsync-1.json"
  assert_equals 3 "$RC" "a failed fsync must exit 3, never report success: $OUT"
  assert_equals failed "$(json_field "$OUT" .result)" "a failed fsync must report failed"
  assert_equals false "$(json_field "$OUT" .may_be_durable)" "a rolled-back record must not be reported as possibly durable"
  assert_not_contains "$OUT" committed "a failed fsync must never be reported as committed"
  run_writer verify --store "$store"
  assert_equals 1 "$(json_field "$OUT" .records)" "the failed record must not remain in the chain"
  record fsync-2 2
  run_writer append --store "$store" "$TMP_ROOT/fsync-2.json"
  assert_equals 1 "$(json_field "$OUT" .seq)" "the next append must take the sequence number the failed one left free"
  check_chain "$store" >/dev/null || fail "the chain must stay valid after a rolled-back failure"
  pass "a failed fsync reports failure and leaves no record behind"
}

test_checkpoint_write_failure_reports_failure() {
  local store="$TMP_ROOT/publish"
  record publish-0 0
  run_writer append --store "$store" "$TMP_ROOT/publish-0.json"
  assert_equals 0 "$RC" "setup append must commit: $OUT $ERR"
  record publish-1 1
  run_faulted "publish:1" append --store "$store" "$TMP_ROOT/publish-1.json"
  assert_equals 3 "$RC" "a failed checkpoint write must exit 3: $OUT"
  assert_equals failed "$(json_field "$OUT" .result)" "a failed checkpoint write must report failed"
  assert_equals false "$(json_field "$OUT" .may_be_durable)" "a checkpoint that never replaced must roll the record back"
  assert_absent "$store/checkpoint.json.tmp" "a failed checkpoint write must not leave a temporary file"
  run_writer verify --store "$store"
  assert_equals 1 "$(json_field "$OUT" .records)" "the record must be rolled back when its checkpoint never published"
  pass "a failed checkpoint write reports failure and rolls the record back"
}

test_directory_sync_after_commit_reports_maybe_durable() {
  local store="$TMP_ROOT/dirsync"
  build_store "$store" 1
  record dirsync-1 1
  run_faulted "sync_dir:1" append --store "$store" "$TMP_ROOT/dirsync-1.json"
  assert_equals 3 "$RC" "a failed directory sync after commit must exit 3: $OUT"
  assert_equals true "$(json_field "$OUT" .may_be_durable)" "a replaced checkpoint must be reported as possibly durable"
  run_writer verify --store "$store"
  assert_equals 2 "$(json_field "$OUT" .records)" "the replaced checkpoint commits the record even though the caller was told failure"
  pass "a directory sync failure after commit reports maybe-durable and keeps the committed record"
}

test_unconfirmed_append_is_adopted_by_the_next_append() {
  local store="$TMP_ROOT/adopt"
  build_store "$store" 1
  record adopt-1 1
  run_faulted "sync_fd:1,rollback_fails" append --store "$store" "$TMP_ROOT/adopt-1.json"
  assert_equals 3 "$RC" "an unconfirmed append must exit 3: $OUT"
  assert_equals true "$(json_field "$OUT" .may_be_durable)" "an append that could not roll back must be reported as possibly durable"
  run_writer verify --store "$store"
  assert_equals 0 "$RC" "an unpublished record is reported, not failed: $OUT"
  assert_equals 1 "$(json_field "$OUT" .uncommitted_seq)" "verify must report the unpublished record"
  record adopt-2 2
  run_writer append --store "$store" "$TMP_ROOT/adopt-2.json"
  assert_equals 0 "$RC" "the next append must succeed: $OUT $ERR"
  assert_equals 1 "$(json_field "$OUT" .recovered.adopted.seq)" "the next append must report the record it adopted"
  assert_equals 2 "$(json_field "$OUT" .seq)" "the new record must follow the adopted one"
  check_chain "$store" >/dev/null || fail "the adopted chain must validate"
  pass "an unconfirmed record is reported, then adopted and surfaced by the next append"
}

test_restart_truncates_and_reports_a_torn_tail() {
  local store="$TMP_ROOT/torn" seg torn_bytes before
  build_store "$store" 2
  seg="$store/$(segname 0)"
  printf '%s' "$TORN_TAIL" >>"$seg"
  torn_bytes=${#TORN_TAIL}
  before=$(store_manifest "$store")
  run_writer verify --store "$store"
  assert_equals 0 "$RC" "a torn tail is reported, not failed: $OUT"
  assert_equals "$torn_bytes" "$(json_field "$OUT" .torn_tail.bytes)" "verify must report the torn tail length"
  assert_equals "$before" "$(store_manifest "$store")" "verify must not repair a torn tail"
  record torn-2 2
  run_writer append --store "$store" "$TMP_ROOT/torn-2.json"
  assert_equals 0 "$RC" "append must recover a torn tail: $OUT $ERR"
  assert_equals "$torn_bytes" "$(json_field "$OUT" .recovered.torn_tail.bytes)" "the next append must report what it truncated"
  assert_equals 2 "$(json_field "$OUT" .seq)" "the next append must continue the chain after the last whole record"
  run_writer verify --store "$store"
  assert_equals null "$(json_field "$OUT" .torn_tail)" "the recovered store must have no torn tail"
  check_chain "$store" >/dev/null || fail "the recovered chain must validate"
  pass "restart truncates a torn tail, reports it, and continues the chain"
}

test_unterminated_record_in_closed_segment_is_refused() {
  local store="$TMP_ROOT/closed-torn" before
  build_store "$store" 3 2
  printf '%s' "$TORN_TAIL" >>"$store/$(segname 0)"
  run_writer verify --store "$store"
  assert_equals 1 "$RC" "a torn record in a closed segment is corruption: $OUT"
  assert_equals unterminated_record "$(json_field "$OUT" .reason)" "a closed segment must not be truncated as if it were the tail"
  before=$(store_manifest "$store")
  record closed-3 3
  run_writer append --store "$store" --segment-records 2 "$TMP_ROOT/closed-3.json"
  assert_equals 3 "$RC" "append must refuse the store: $OUT"
  assert_equals "$before" "$(store_manifest "$store")" "a refused append must change no file"
  pass "a torn record in a closed segment is refused and left in place"
}

test_modified_record_is_detected_and_never_repaired() {
  local store="$TMP_ROOT/modified" tip="$TMP_ROOT/modified-tip" before
  build_store "$store" 4
  tamper_line "$store/$(segname 0)" 2 sess-1 sess-9
  run_writer verify --store "$store"
  assert_equals 1 "$RC" "a modified record must fail verification: $OUT"
  assert_equals corrupt "$(json_field "$OUT" .result)" "a modified record must be reported corrupt"
  assert_equals hash_mismatch "$(json_field "$OUT" .reason)" "the link after a modified record must fail"
  assert_equals 2 "$(json_field "$OUT" .seq)" "verify must name the first record whose link no longer matches"
  before=$(store_manifest "$store")
  record modified-4 4
  run_writer append --store "$store" "$TMP_ROOT/modified-4.json"
  assert_equals 3 "$RC" "append must refuse a corrupt store"
  assert_equals refused "$(json_field "$OUT" .result)" "a refused append must say refused"
  assert_equals "$before" "$(store_manifest "$store")" "a refused append must not repair or change any file"
  run_writer project-ledger --store "$store"
  assert_equals 1 "$RC" "the ledger projection must refuse a corrupt store"

  build_store "$tip" 4
  tamper_line "$tip/$(segname 0)" 4 sess-1 sess-9
  run_writer verify --store "$tip"
  assert_equals checkpoint_mismatch "$(json_field "$OUT" .reason)" "a modified tip must disagree with the checkpoint"
  pass "a modified record is detected, and refusal leaves the evidence untouched"
}

test_deleted_record_is_detected() {
  local middle="$TMP_ROOT/deleted-middle" tip="$TMP_ROOT/deleted-tip"
  build_store "$middle" 4
  drop_line "$middle/$(segname 0)" 2
  run_writer verify --store "$middle"
  assert_equals 1 "$RC" "a deleted middle record must fail verification: $OUT"
  assert_equals seq_mismatch "$(json_field "$OUT" .reason)" "a deleted middle record must break sequence numbering"
  build_store "$tip" 4
  drop_line "$tip/$(segname 0)" 4
  run_writer verify --store "$tip"
  assert_equals checkpoint_mismatch "$(json_field "$OUT" .reason)" "a deleted tip must disagree with the checkpoint"
  pass "a deleted record is detected at the middle and at the tip"
}

test_deleted_segment_is_detected_and_intact_segments_are_named() {
  local store="$TMP_ROOT/rotated-gap"
  build_store "$store" 5 2
  unlink "$store/$(segname 2)"
  run_writer verify --store "$store"
  assert_equals 1 "$RC" "a deleted middle segment must fail verification: $OUT"
  assert_equals segment_gap "$(json_field "$OUT" .reason)" "a deleted middle segment must break the sequence"
  assert_equals "$(segname 0)" "$(json_field "$OUT" '.intact_segments | join(",")')" \
    "verify must name the segments that still validate"
  pass "a deleted segment is detected and the intact segments are named"
}

test_checkpoint_damage_is_refused_and_never_repaired() {
  local store="$TMP_ROOT/checkpoint" missing="$TMP_ROOT/checkpoint-missing" before
  build_store "$store" 2
  printf '%s\n' '{"v":1}' >"$store/checkpoint.json"
  run_writer verify --store "$store"
  assert_equals checkpoint_invalid "$(json_field "$OUT" .reason)" "a malformed checkpoint must be refused"
  before=$(store_manifest "$store")
  record checkpoint-2 2
  run_writer append --store "$store" "$TMP_ROOT/checkpoint-2.json"
  assert_equals 3 "$RC" "append must refuse a store with a malformed checkpoint"
  assert_equals "$before" "$(store_manifest "$store")" "a refused append must not rewrite the checkpoint"

  build_store "$missing" 2
  unlink "$missing/checkpoint.json"
  run_writer verify --store "$missing"
  assert_equals checkpoint_missing "$(json_field "$OUT" .reason)" "segments without a checkpoint must not restart as a new chain"
  pass "checkpoint damage is refused, and refusal never rewrites the checkpoint"
}

test_more_than_one_record_beyond_the_checkpoint_is_refused() {
  local store="$TMP_ROOT/ahead"
  build_store "$store" 2
  forge_ahead "$store" "$TMP_ROOT/ahead-0.json" 2
  run_writer verify --store "$store"
  assert_equals 1 "$RC" "two unpublished records cannot come from one writer and must be refused: $OUT"
  assert_equals checkpoint_mismatch "$(json_field "$OUT" .reason)" "records past the checkpoint beyond one must be refused"
  record ahead-4 4
  run_writer append --store "$store" "$TMP_ROOT/ahead-4.json"
  assert_equals 3 "$RC" "append must refuse a store with more than one unpublished record"
  pass "more than one record beyond the checkpoint is refused, not adopted"
}

test_rejected_input_is_recorded_and_writes_nothing() {
  local store="$TMP_ROOT/rejected" last
  record rej-seq 7 '.seq = 3'
  run_writer append --store "$store" "$TMP_ROOT/rej-seq.json"
  assert_equals 1 "$RC" "a caller-supplied seq must be rejected and recorded: $OUT $ERR"
  assert_equals rejected "$(json_field "$OUT" .result)" "a caller-supplied seq must report rejected"
  assert_equals schema_invalid "$(json_field "$OUT" .reason)" "a caller-supplied seq must be a schema rejection"
  assert_equals /seq "$(json_field "$OUT" '.paths | join(",")')" "the rejection must point at the member"
  assert_absent "$store" "a rejected envelope must not create a store"
  last=$(tail -n 1 "$FM_STATE_OVERRIDE/audit-rejections.jsonl")
  assert_contains "$last" '"reason": "schema_invalid"' "the rejection must be recorded"

  record rej-user 8 'del(.user)'
  run_writer append --store "$store" "$TMP_ROOT/rej-user.json"
  assert_equals 1 "$RC" "an envelope without a resolved identity must be rejected and recorded: $OUT"
  assert_equals identity_unresolved "$(json_field "$OUT" .reason)" "a missing identity must be an identity rejection"
  last=$(tail -n 1 "$FM_STATE_OVERRIDE/audit-rejections.jsonl")
  assert_not_contains "$last" "$USER_ID" "a rejection record must not carry identity values"
  assert_absent "$store" "a rejected envelope must not create a store"
  pass "rejected input is recorded without identity values and writes nothing"
}

test_unrecordable_rejection_exits_2() {
  local store="$TMP_ROOT/unrecorded" out
  record unrec 9 '.user = null'
  out=$(env -u FM_STATE_OVERRIDE -u FM_HOME python3 "$WRITER" append --store "$store" "$TMP_ROOT/unrec.json" \
    2>"$TMP_ROOT/unrec.err")
  RC=$?
  assert_equals 2 "$RC" "an unrecordable rejection must exit 2"
  assert_equals "" "$out" "an unrecorded rejection must print no rejected result"
  assert_contains "$(cat "$TMP_ROOT/unrec.err")" "rejection not recorded" "the unrecorded rejection must say so"
  assert_absent "$store" "an unrecorded rejection must not create a store"
  out=$(FM_STATE_OVERRIDE="$TMP_ROOT/no-such-state" python3 "$WRITER" append --store "$store" "$TMP_ROOT/unrec.json" \
    2>"$TMP_ROOT/unrec.err")
  RC=$?
  assert_equals 2 "$RC" "a rejection into a missing state directory must exit 2"
  assert_equals "" "$out" "a rejection into a missing state directory must print no rejected result"
  pass "an unrecordable rejection exits 2 and prints no rejected result"
}

test_v1_projection_validates_as_fleet_ledger_v1() {
  local store="$TMP_ROOT/ledger" expected line i
  record led-0 0
  record led-1 1 '.event = "identity.resolved" | .category = "identity"'
  record led-2 2 '.event = "task.pr_ready" | .pr = "https://github.com/mc-innersource/motif-hve-multi-agent-orchestrator/pull/70"'
  for i in 0 1 2; do
    run_writer append --store "$store" "$TMP_ROOT/led-$i.json"
    assert_equals 0 "$RC" "ledger setup append $i must commit: $OUT $ERR"
  done
  run_writer project-ledger --store "$store"
  assert_equals 0 "$RC" "a clean store must project: $ERR"
  expected=$(printf '%s\n' \
    "{\"event\":\"task.dispatched\",\"task\":\"fix-login\",\"ts\":$LEDGER_TS,\"v\":1}" \
    "{\"event\":\"task.pr_ready\",\"pr\":\"https://github.com/mc-innersource/motif-hve-multi-agent-orchestrator/pull/70\",\"task\":\"fix-login\",\"ts\":$LEDGER_TS,\"v\":1}")
  assert_equals "$expected" "$OUT" "the projection must carry only ledger v1 events and their base members"
  assert_not_contains "$OUT" "$USER_ID" "the projection must not carry identity values"
  while IFS= read -r line; do
    printf '%s\n' "$line" | python3 "$ENVELOPE" validate --definition fleet_ledger_v1 - >/dev/null \
      || fail "projected line must validate as fleet_ledger_v1: $line"
  done <<<"$OUT"
  pass "the v1 projection carries only ledger events and validates as fleet_ledger_v1"
}

test_usage_errors_exit_2() {
  python3 "$WRITER" >/dev/null 2>&1
  assert_equals 2 "$?" "no subcommand must exit 2"
  python3 "$WRITER" append --store "$TMP_ROOT/usage" >/dev/null 2>&1
  assert_equals 2 "$?" "append without an input must exit 2"
  record usage-0 0
  run_writer append --store "$TMP_ROOT/usage" --segment-records 0 "$TMP_ROOT/usage-0.json"
  assert_equals 2 "$RC" "a segment size below one must exit 2"
  assert_absent "$TMP_ROOT/usage" "a usage error must not create a store"
  run_writer append --store "$TMP_ROOT/usage" "$TMP_ROOT/no-such-file.json"
  assert_equals 2 "$RC" "an unreadable input must exit 2"
  assert_absent "$TMP_ROOT/usage" "an unreadable input must not create a store"
  run_writer verify --store "$TMP_ROOT/no-such-store"
  assert_equals 2 "$RC" "verify of a missing store must exit 2"
  run_writer project-ledger --store "$TMP_ROOT/no-such-store"
  assert_equals 2 "$RC" "projection of a missing store must exit 2"
  pass "usage and input errors exit 2"
}

test_append_assigns_sequence_and_hash_chain
test_concurrent_appends_commit_unique_increasing_sequence
test_rotation_creates_segments_and_verifies
test_fsync_failure_reports_failure_and_leaves_no_record
test_checkpoint_write_failure_reports_failure
test_directory_sync_after_commit_reports_maybe_durable
test_unconfirmed_append_is_adopted_by_the_next_append
test_restart_truncates_and_reports_a_torn_tail
test_unterminated_record_in_closed_segment_is_refused
test_modified_record_is_detected_and_never_repaired
test_deleted_record_is_detected
test_deleted_segment_is_detected_and_intact_segments_are_named
test_checkpoint_damage_is_refused_and_never_repaired
test_more_than_one_record_beyond_the_checkpoint_is_refused
test_rejected_input_is_recorded_and_writes_nothing
test_unrecordable_rejection_exits_2
test_v1_projection_validates_as_fleet_ledger_v1
test_usage_errors_exit_2
