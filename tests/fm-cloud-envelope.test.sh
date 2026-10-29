#!/usr/bin/env bash
# Behavior tests for bin/fm-cloud-envelope.sh (DPES-878). Every refusal must name
# its gate on stderr, print nothing on stdout, and exit 2. The approved fixture
# must render an envelope that carries its limits and intent and no local paths.
set -u

# shellcheck source=tests/lib.sh
. "$(dirname "${BASH_SOURCE[0]}")/lib.sh"

ENVELOPE="$ROOT/bin/fm-cloud-envelope.sh"
TMP_ROOT=$(fm_test_tmproot fm-cloud-envelope)
BASE_SHA=0123456789abcdef0123456789abcdef01234567

file_digest() {  # <file> -> sha256:<hex>, computed here rather than by the script
  if command -v sha256sum >/dev/null 2>&1; then
    printf 'sha256:%s\n' "$(sha256sum "$1" | awk '{print $1}')"
  else
    printf 'sha256:%s\n' "$(shasum -a 256 "$1" | awk '{print $1}')"
  fi
}

new_case() {  # <name>: approved fixture in $TMP_ROOT/<name>; sets CASE_DIR
  local dir="$TMP_ROOT/$1"
  mkdir -p "$dir"
  printf '%s\n' '{"tools": ["read", "write", "edit", "bash"]}' > "$dir/manifest.json"
  cat > "$dir/readiness.env" <<EOF
finite_worker_policy=approved
finite_worker_decision=fixture-decision-1
runtime=pi-durable
runtime_version=1.3.0
runtime_qualified=yes
manifest_digest=$(file_digest "$dir/manifest.json")
profile=standard
deadline_seconds=7200
vcpu=2
memory_gib=4
EOF
  printf '%s\n' 'Add the pi-durable envelope gate.' > "$dir/intent.md"
  CASE_DIR=$dir
}

set_field() {  # <case-dir> <key> <value|-> : replace the key's line, or delete it for '-'
  local file="$1/readiness.env"
  awk -F= -v k="$2" -v v="$3" '$1 == k { if (v != "-") print k "=" v; next } { print }' "$file" > "$file.tmp" \
    && mv "$file.tmp" "$file"
}

run_envelope() {  # <case-dir> [runtime] [base-commit] [task-id]; sets OUT, ERR, STATUS
  local dir="$1"
  OUT=$(bash "$ENVELOPE" --readiness "$dir/readiness.env" --runtime "${2:-pi-durable}" \
    --manifest "$dir/manifest.json" --intent "$dir/intent.md" \
    --task-id "${4:-dpes-878-fixture}" --base-commit "${3:-$BASE_SHA}" 2>"$dir/stderr")
  STATUS=$?
  ERR=$(cat "$dir/stderr")
}

assert_refused() {  # <gate> <message>
  [ "$STATUS" -eq 2 ] || fail "$2: exit $STATUS, want 2 (stderr: $ERR)"
  [ -z "$OUT" ] || fail "$2: refusal printed stdout: $OUT"
  case $ERR in
    "refused: $1: "*) ;;
    *) fail "$2: stderr did not name gate $1: $ERR" ;;
  esac
  pass "$2"
}

test_approved_envelope_renders_limits_and_intent_without_local_paths() {
  new_case approved
  run_envelope "$CASE_DIR"
  [ "$STATUS" -eq 0 ] || fail "approved fixture was refused: $ERR"
  case $OUT in *"Base commit: $BASE_SHA"*) ;; *) fail "envelope lacks the base commit" ;; esac
  case $OUT in *"Runtime: pi-durable 1.3.0"*) ;; *) fail "envelope lacks the pinned runtime" ;; esac
  case $OUT in *"Total deadline: 7200 seconds"*) ;; *) fail "envelope lacks the total deadline" ;; esac
  case $OUT in *"Resources: 2 vCPU and 4 GiB"*) ;; *) fail "envelope lacks the resource limits" ;; esac
  case $OUT in *"Tool and instruction manifest: $(file_digest "$CASE_DIR/manifest.json") "*) ;; *) fail "envelope lacks the verified manifest digest" ;; esac
  case $OUT in *"Add the pi-durable envelope gate."*) ;; *) fail "envelope lacks the task intent verbatim" ;; esac
  local leak
  for leak in /Users/ FM_HOME .inbox .status; do
    case $OUT in *"$leak"*) fail "envelope leaks local path marker $leak" ;; esac
  done
  pass "approved fixture renders limits, intent, and digest with no local path markers"
}

test_plain_pi_selection_is_refused_even_with_an_approved_record() {
  new_case plain-pi
  run_envelope "$CASE_DIR" pi
  assert_refused plain-pi-fallback-refused "plain pi selection is refused with no fallback"
}

test_non_durable_runtime_is_refused() {
  new_case unknown-runtime
  run_envelope "$CASE_DIR" codex
  assert_refused runtime-unsupported "a runtime other than pi-durable is refused"
}

test_pending_finite_worker_policy_is_refused() {
  new_case policy-pending
  set_field "$CASE_DIR" finite_worker_policy pending
  run_envelope "$CASE_DIR"
  assert_refused finite-worker-policy-unapproved "a pending finite-worker policy blocks the envelope"
}

test_missing_finite_worker_policy_is_refused() {
  new_case policy-missing
  set_field "$CASE_DIR" finite_worker_policy -
  run_envelope "$CASE_DIR"
  assert_refused finite-worker-policy-unapproved "a missing finite-worker policy blocks the envelope"
}

test_unqualified_durable_runtime_is_refused() {
  new_case unqualified
  set_field "$CASE_DIR" runtime_qualified no
  run_envelope "$CASE_DIR"
  assert_refused durable-runtime-unavailable "an unqualified pi-durable runtime blocks the envelope"
}

test_manifest_digest_mismatch_is_refused() {
  new_case digest-drift
  printf '%s\n' '{"tools": ["read", "write", "edit", "bash", "extra"]}' > "$CASE_DIR/manifest.json"
  run_envelope "$CASE_DIR"
  assert_refused manifest-digest-mismatch "a manifest that drifted from its approved digest is refused"
}

test_unapproved_manifest_digest_is_refused() {
  new_case digest-missing
  set_field "$CASE_DIR" manifest_digest -
  run_envelope "$CASE_DIR"
  assert_refused manifest-unapproved "a manifest with no approved digest is refused"
}

test_missing_deadline_is_refused() {
  new_case limits-missing
  set_field "$CASE_DIR" deadline_seconds -
  run_envelope "$CASE_DIR"
  assert_refused limits-unapproved "a missing total deadline blocks the envelope"
}

test_zero_deadline_is_refused() {
  new_case limits-zero
  set_field "$CASE_DIR" deadline_seconds 0
  run_envelope "$CASE_DIR"
  assert_refused limits-unapproved "a zero total deadline blocks the envelope"
}

test_duplicate_readiness_key_is_refused() {
  new_case duplicate-key
  printf '%s\n' 'profile=other' >> "$CASE_DIR/readiness.env"
  run_envelope "$CASE_DIR"
  assert_refused readiness-malformed "a duplicated readiness key is refused"
}

test_unknown_readiness_key_is_refused() {
  new_case unknown-key
  printf '%s\n' 'image_tag=latest' >> "$CASE_DIR/readiness.env"
  run_envelope "$CASE_DIR"
  assert_refused readiness-malformed "an unknown readiness key is refused"
}

test_missing_readiness_record_is_refused() {
  new_case readiness-absent
  rm -f "$CASE_DIR/readiness.env"
  run_envelope "$CASE_DIR"
  assert_refused readiness-missing "a missing readiness record is refused"
}

test_empty_intent_is_refused() {
  new_case intent-empty
  : > "$CASE_DIR/intent.md"
  run_envelope "$CASE_DIR"
  assert_refused intent-missing "an empty intent file is refused"
}

test_short_base_commit_is_refused() {
  new_case base-short
  run_envelope "$CASE_DIR" pi-durable abc123
  assert_refused base-commit-invalid "a short base commit is refused"
}

test_path_like_task_id_is_refused() {
  new_case task-path
  run_envelope "$CASE_DIR" pi-durable "$BASE_SHA" "../escape"
  assert_refused task-id-invalid "a path-like task id is refused"
}

test_missing_option_is_a_usage_error() {
  new_case usage
  OUT=$(bash "$ENVELOPE" --readiness "$CASE_DIR/readiness.env" 2>/dev/null)
  STATUS=$?
  [ "$STATUS" -eq 64 ] || fail "missing options exited $STATUS, want 64"
  [ -z "$OUT" ] || fail "usage error printed stdout: $OUT"
  pass "missing options exit 64 with no envelope"
}

test_adjacent_joined_keys_are_refused_not_ignored() {
  new_case joined-key
  printf '%s\n' 'manifest_digest|profile=zzz' >> "$CASE_DIR/readiness.env"
  run_envelope "$CASE_DIR"
  assert_refused readiness-malformed "two known keys joined by a pipe are refused rather than ignored"
}

test_crlf_readiness_is_refused_as_malformed() {
  new_case crlf
  awk '{ printf "%s\r\n", $0 }' "$CASE_DIR/readiness.env" > "$CASE_DIR/readiness.crlf" \
    && mv "$CASE_DIR/readiness.crlf" "$CASE_DIR/readiness.env"
  run_envelope "$CASE_DIR"
  assert_refused readiness-malformed "a readiness record with CRLF line endings is refused as malformed"
}

test_readiness_directory_is_refused_as_missing() {
  new_case readiness-dir
  rm -f "$CASE_DIR/readiness.env"
  mkdir "$CASE_DIR/readiness.env"
  run_envelope "$CASE_DIR"
  assert_refused readiness-missing "a readiness path that is a directory is refused as missing"
}

test_manifest_directory_is_refused_as_missing() {
  new_case manifest-dir
  rm -f "$CASE_DIR/manifest.json"
  mkdir "$CASE_DIR/manifest.json"
  run_envelope "$CASE_DIR"
  assert_refused manifest-missing "a manifest path that is a directory is refused as missing"
}

test_intent_directory_is_refused_before_any_output() {
  new_case intent-dir
  rm -f "$CASE_DIR/intent.md"
  mkdir "$CASE_DIR/intent.md"
  run_envelope "$CASE_DIR"
  assert_refused intent-missing "an intent path that is a directory is refused before any envelope output"
}

test_envelope_states_nested_call_policy_and_bounded_bash() {
  new_case callable-policy
  run_envelope "$CASE_DIR"
  [ "$STATUS" -eq 0 ] || fail "approved fixture was refused: $ERR"
  case $OUT in *"nested tool calls"*) ;; *) fail "envelope does not state that the tool allowlist covers nested calls" ;; esac
  case $OUT in *"explicit timeout"*"remaining total deadline"*) ;; *) fail "envelope does not bound bash by the remaining total deadline" ;; esac
  case $OUT in *"allowlisted environment"*) ;; *) fail "envelope does not state the bash environment allowlist" ;; esac
  case $OUT in *"Firstmate spawn and control"*) ;; *) fail "envelope does not state the withheld local control surfaces" ;; esac
  pass "envelope states the nested-call tool policy, bounded bash, and withheld local surfaces"
}

test_approved_envelope_renders_limits_and_intent_without_local_paths
test_plain_pi_selection_is_refused_even_with_an_approved_record
test_non_durable_runtime_is_refused
test_pending_finite_worker_policy_is_refused
test_missing_finite_worker_policy_is_refused
test_unqualified_durable_runtime_is_refused
test_manifest_digest_mismatch_is_refused
test_unapproved_manifest_digest_is_refused
test_missing_deadline_is_refused
test_zero_deadline_is_refused
test_duplicate_readiness_key_is_refused
test_unknown_readiness_key_is_refused
test_missing_readiness_record_is_refused
test_empty_intent_is_refused
test_short_base_commit_is_refused
test_path_like_task_id_is_refused
test_missing_option_is_a_usage_error
test_adjacent_joined_keys_are_refused_not_ignored
test_crlf_readiness_is_refused_as_malformed
test_readiness_directory_is_refused_as_missing
test_manifest_directory_is_refused_as_missing
test_intent_directory_is_refused_before_any_output
test_envelope_states_nested_call_policy_and_bounded_bash

echo "all fm-cloud-envelope tests passed"
