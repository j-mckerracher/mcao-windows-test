#!/usr/bin/env bash
# bin/fm-execution-target-lib.sh contract: record target resolution, the
# cloud-routed operation set, refusal wording, and the cloud client adapter.
# Every case runs the library's public functions against real record files and
# a stub `mcao` on PATH. No terminal daemon and no cloud service is needed.
set -u

# shellcheck source=tests/lib.sh
. "$(dirname "${BASH_SOURCE[0]}")/lib.sh"

TMP_ROOT=$(fm_test_tmproot fm-execution-target)
FAKEBIN=$(fm_fakebin "$TMP_ROOT")
export FM_FAKE_MCAO_LOG="$TMP_ROOT/mcao.log"

# shellcheck source=bin/fm-execution-target-lib.sh
. "$ROOT/bin/fm-execution-target-lib.sh"

cat > "$FAKEBIN/mcao" <<'SH'
#!/usr/bin/env bash
printf '%s\n' "$*" >> "$FM_FAKE_MCAO_LOG"
if [ "${2:-}" = logs ]; then
  printf 'log-1\nlog-2\nlog-3\n'
  exit "${FM_FAKE_MCAO_RC:-0}"
fi
echo "cancel_requested=true"
exit "${FM_FAKE_MCAO_RC:-0}"
SH
chmod +x "$FAKEBIN/mcao"
PATH="$FAKEBIN:$PATH"

write_record() {  # <name> <line>... -> echoes the record path
  local name=$1 path
  shift
  path="$TMP_ROOT/$name.meta"
  printf '%s\n' "$@" > "$path"
  printf '%s' "$path"
}

expect_target() {  # <label> <meta> <expected-target>
  local got rc
  got=$(fm_exec_record_resolve "$2" 2>"$TMP_ROOT/err")
  rc=$?
  [ "$rc" -eq 0 ] && [ "$got" = "$3" ] \
    || fail "$1: expected target $3, got rc=$rc value='$got' stderr='$(cat "$TMP_ROOT/err")'"
  pass "$1"
}

expect_refused() {  # <label> <meta>
  local got rc
  got=$(fm_exec_record_resolve "$2" 2>"$TMP_ROOT/err")
  rc=$?
  [ "$rc" -ne 0 ] && [ -z "$got" ] && [ -s "$TMP_ROOT/err" ] \
    || fail "$1: expected refusal with stderr, got rc=$rc value='$got'"
  pass "$1"
}

# Record resolution. An absent or legacy record is local, and every ambiguous,
# unknown, or incomplete explicit reference is refused rather than defaulted.
expect_target "absent record resolves to local" "$TMP_ROOT/no-such.meta" local
expect_target "legacy record without execution_target resolves to local" \
  "$(write_record legacy 'backend=tmux' 'window=firstmate:fm-legacy')" local
expect_target "explicit local record resolves to local" \
  "$(write_record explicit-local 'execution_target=local' 'backend=tmux')" local
expect_target "complete gcp record resolves to gcp" \
  "$(write_record gcp-ok 'execution_target=gcp' 'cloud_request_id=req-1' 'cloud_run_id=run.1_a')" gcp
expect_refused "unknown execution_target is refused" \
  "$(write_record unknown 'execution_target=ssh' 'cloud_run_id=run-1')"
expect_refused "empty execution_target is refused" \
  "$(write_record empty 'execution_target=' 'backend=tmux')"
expect_refused "repeated execution_target is refused" \
  "$(write_record repeated 'execution_target=local' 'execution_target=gcp')"
expect_refused "gcp record without cloud_run_id is refused" \
  "$(write_record no-run 'execution_target=gcp' 'cloud_request_id=req-1')"
expect_refused "gcp record with repeated cloud_run_id is refused" \
  "$(write_record dup-run 'execution_target=gcp' 'cloud_request_id=req-1' 'cloud_run_id=a' 'cloud_run_id=b')"
expect_refused "gcp record with a path-shaped cloud_run_id is refused" \
  "$(write_record bad-run 'execution_target=gcp' 'cloud_request_id=req-1' 'cloud_run_id=run/1')"

# The cloud-routed operation set. Submit is spawn-only and doctor is not routed,
# so neither is supported by a lifecycle verb.
for op in status logs cancel fetch; do
  fm_exec_cloud_op_supported "$op" || fail "$op should be a supported cloud operation"
done
pass "status, logs, cancel, and fetch are the supported cloud operations"
for op in interrupt exit relaunch send key slash attach promote submit doctor; do
  fm_exec_cloud_op_supported "$op" && fail "$op must not route to the cloud client"
done
pass "interrupt, exit, relaunch, steering, input, attach, promote, submit, and doctor are not routed"

# Refusal wording names the task, the target, the operation, the alternatives,
# and the follow-up rule. It writes to stderr only.
refusal=$(fm_exec_refuse_cloud_op task-7 interrupt 2>&1 >/dev/null)
rc=$?
[ "$rc" -ne 0 ] || fail "refuse_cloud_op must return nonzero"
case "$refusal" in
  *"'interrupt' is not supported for task task-7 on execution target gcp"*"no local fallback"*"new source commit"*) ;;
  *) fail "refuse_cloud_op wording lost a required element: $refusal" ;;
esac
pass "cloud refusal names the operation, task, target, and follow-up rule"

refusal=$(fm_exec_refuse_local_cancel task-7 2>&1 >/dev/null)
rc=$?
[ "$rc" -ne 0 ] || fail "refuse_local_cancel must return nonzero"
case "$refusal" in
  *"'cancel' is an unsupported operation for task task-7 on execution target local"*) ;;
  *) fail "refuse_local_cancel wording lost a required element: $refusal" ;;
esac
pass "local cancel is refused as an unsupported local operation"

# The cloud adapter runs once, passes output through, and preserves the client's
# exit status so a failed or pending cancel is never reported as success.
: > "$FM_FAKE_MCAO_LOG"
out=$(fm_exec_cloud_run cancel --run-id run-1)
rc=$?
[ "$rc" -eq 0 ] && [ "$out" = "cancel_requested=true" ] \
  || fail "cloud cancel should pass output and rc 0, got rc=$rc out='$out'"
[ "$(cat "$FM_FAKE_MCAO_LOG")" = "cloud cancel --run-id run-1" ] \
  || fail "cloud cancel must invoke mcao exactly once with the run id, got: $(cat "$FM_FAKE_MCAO_LOG")"
pass "cloud cancel invokes the client once with the run id and passes output through"

: > "$FM_FAKE_MCAO_LOG"
out=$(FM_FAKE_MCAO_RC=7 fm_exec_cloud_run cancel --run-id run-1)
rc=$?
[ "$rc" -eq 7 ] || fail "a client failure must propagate its status, got rc=$rc"
[ "$(wc -l < "$FM_FAKE_MCAO_LOG" | tr -d ' ')" -eq 1 ] || fail "a failed cancel must still run exactly once"
pass "a client failure propagates its status and is never retried locally"

missing_path=$(mktemp -d "$TMP_ROOT/nomcao.XXXXXX")
out=$(PATH="$missing_path" fm_exec_cloud_run status --run-id run-1 2>&1)
rc=$?
[ "$rc" -ne 0 ] || fail "a missing cloud client must fail, got rc=0"
case "$out" in
  *"mcao is not installed"*"No local fallback"*) ;;
  *) fail "a missing cloud client must say there is no local fallback: $out" ;;
esac
pass "a missing cloud client fails with no local fallback"

# The watcher's local-terminal gate passes only records that resolve to local.
fm_exec_meta_is_local "$(write_record gate-legacy 'backend=tmux' 'window=firstmate:fm-legacy')" \
  || fail "a legacy record must pass the local-terminal gate"
fm_exec_meta_is_local "$(write_record gate-gcp 'execution_target=gcp' 'cloud_request_id=req-1' 'cloud_run_id=run-1')" \
  && fail "a gcp record must not pass the local-terminal gate"
fm_exec_meta_is_local "$(write_record gate-bad 'execution_target=ssh' 'cloud_run_id=run-1')" 2>/dev/null \
  && fail "an unknown execution target must not pass the local-terminal gate"
pass "the local-terminal gate passes legacy records and skips gcp and invalid references"

# fm-peek on a gcp task reads the recorded run's logs once through the cloud
# client and prints only the last N lines. No terminal is read, and an incomplete
# gcp reference is named without reaching the client.
PEEK_HOME="$TMP_ROOT/peek-home"
mkdir -p "$PEEK_HOME/state"
printf 'execution_target=gcp\ncloud_request_id=req-p\ncloud_run_id=run-p\nkind=ship\n' > "$PEEK_HOME/state/cloud-peek.meta"
: > "$FM_FAKE_MCAO_LOG"
out=$(FM_HOME="$PEEK_HOME" FM_STATE_OVERRIDE="$PEEK_HOME/state" "$ROOT/bin/fm-peek.sh" cloud-peek 2 2>"$TMP_ROOT/peek.err")
rc=$?
[ "$rc" -eq 0 ] || fail "peek of a gcp task should succeed, got rc=$rc stderr='$(cat "$TMP_ROOT/peek.err")'"
[ "$out" = "$(printf 'log-2\nlog-3')" ] || fail "peek should print only the last 2 cloud log lines, got: '$out'"
[ "$(cat "$FM_FAKE_MCAO_LOG")" = "cloud logs --run-id run-p" ] \
  || fail "peek must read the recorded run's logs exactly once, got: $(cat "$FM_FAKE_MCAO_LOG")"
pass "fm-peek reads a gcp task's bounded log tail through the cloud client"

printf 'execution_target=gcp\ncloud_request_id=req-q\nkind=ship\n' > "$PEEK_HOME/state/cloud-no-run.meta"
: > "$FM_FAKE_MCAO_LOG"
out=$(FM_HOME="$PEEK_HOME" FM_STATE_OVERRIDE="$PEEK_HOME/state" "$ROOT/bin/fm-peek.sh" cloud-no-run 2>"$TMP_ROOT/peek.err")
rc=$?
[ "$rc" -ne 0 ] || fail "peek of a gcp record without cloud_run_id should fail"
case "$(cat "$TMP_ROOT/peek.err")" in
  *"lacks exactly one cloud_run_id"*) ;;
  *) fail "an incomplete gcp peek must name the missing reference: $(cat "$TMP_ROOT/peek.err")" ;;
esac
[ ! -s "$FM_FAKE_MCAO_LOG" ] || fail "an incomplete gcp reference must not reach the cloud client"
pass "fm-peek names an incomplete gcp reference and never reaches the cloud client"
