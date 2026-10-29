#!/usr/bin/env bash
# fm-cloud-envelope.sh renders the pi-durable cloud worker envelope (DPES-878).
#
# Usage: fm-cloud-envelope.sh --readiness FILE --runtime pi-durable \
#          --manifest FILE --intent FILE --task-id ID --base-commit SHA
#
# Stdout gets the envelope only when every gate passes. A refusal prints
# `refused: <gate>: <reason>` on stderr, prints nothing on stdout, and exits 2.
# Usage errors exit 64. Gates run in this order: runtime selection, readiness
# record, finite-worker policy, durable runtime, manifest digest, limits,
# intent, then task and base identity.
#
# The readiness record is a key=value file. Its schema belongs to DPES-804 and
# DPES-805, which are not merged, so this script reads only the keys named in
# CLOUD_KEYS and refuses unknown, duplicate, or malformed lines instead of
# guessing. The envelope carries no local paths, steering records, or status
# records. Nothing launches here: this script never calls Cloud Run and never
# uploads source.
set -u
set -o pipefail

CLOUD_KEYS='|finite_worker_policy|finite_worker_decision|runtime|runtime_version|runtime_qualified|manifest_digest|profile|deadline_seconds|vcpu|memory_gib|'

usage() {
  printf '%s\n' 'usage: fm-cloud-envelope.sh --readiness FILE --runtime pi-durable --manifest FILE' \
    '                              --intent FILE --task-id ID --base-commit SHA'
}

die_usage() {
  printf 'fm-cloud-envelope: %s\n' "$1" >&2
  usage >&2
  exit 64
}

refuse() {  # <gate> <reason>
  printf 'refused: %s: %s\n' "$1" "$2" >&2
  exit 2
}

field() {  # <readiness> <key>: the value, empty when absent
  awk -F= -v k="$2" '$1 == k { sub(/^[^=]*=/, ""); print; exit }' "$1"
}

readiness_defect() {  # <readiness>: first unknown, duplicate, or malformed line
  awk -F= -v keys="$CLOUD_KEYS" '
    BEGIN { n = split(keys, parts, "|"); for (i = 1; i <= n; i++) if (parts[i] != "") known[parts[i]] = 1 }
    /^$/ { next }
    $0 !~ /^[a-z0-9_]+=[^[:space:]]*$/ || !($1 in known) { print "line " NR ": " $0; exit }
    seen[$1]++ { print "line " NR ": duplicate " $1; exit }
  ' "$1"
}

token_ok() { [[ $1 =~ ^[A-Za-z0-9][A-Za-z0-9._-]*$ ]]; }
int_ok() { [[ $1 =~ ^[1-9][0-9]*$ ]]; }
sha_ok() { [[ $1 =~ ^[0-9a-f]{40}$ ]]; }
digest_ok() { [[ $1 =~ ^sha256:[0-9a-f]{64}$ ]]; }

sha256_of() {  # <file>: prints sha256:<hex>; nonzero when no digest tool exists
  local line
  if command -v shasum >/dev/null 2>&1; then
    line=$(shasum -a 256 "$1") || return 1
  elif command -v sha256sum >/dev/null 2>&1; then
    line=$(sha256sum "$1") || return 1
  else
    return 1
  fi
  printf 'sha256:%s\n' "${line%% *}"
}

readiness='' runtime='' manifest='' intent='' task_id='' base_commit=''
while [ $# -gt 0 ]; do
  case $1 in
    --readiness|--runtime|--manifest|--intent|--task-id|--base-commit)
      [ $# -ge 2 ] || die_usage "$1 needs a value"
      case $1 in
        --readiness) readiness=$2 ;;
        --runtime) runtime=$2 ;;
        --manifest) manifest=$2 ;;
        --intent) intent=$2 ;;
        --task-id) task_id=$2 ;;
        --base-commit) base_commit=$2 ;;
      esac
      shift 2
      ;;
    -h|--help) usage; exit 0 ;;
    *) die_usage "unknown argument: $1" ;;
  esac
done
[ -n "$readiness" ] && [ -n "$runtime" ] && [ -n "$manifest" ] && [ -n "$intent" ] \
  && [ -n "$task_id" ] && [ -n "$base_commit" ] \
  || die_usage "every option is required"

case $runtime in
  pi-durable) ;;
  pi|plain-pi) refuse plain-pi-fallback-refused "runtime '$runtime' has no cloud fallback; only pi-durable may run in the cloud" ;;
  *) refuse runtime-unsupported "runtime '$runtime' is not pi-durable" ;;
esac

[ -f "$readiness" ] && [ -r "$readiness" ] || refuse readiness-missing "readiness record is not a readable file"
defect=$(readiness_defect "$readiness")
[ -z "$defect" ] || refuse readiness-malformed "$defect"

[ "$(field "$readiness" finite_worker_policy)" = approved ] \
  || refuse finite-worker-policy-unapproved "finite_worker_policy is not approved"
decision=$(field "$readiness" finite_worker_decision)
token_ok "$decision" || refuse finite-worker-policy-unapproved "finite_worker_decision is missing or invalid"

[ "$(field "$readiness" runtime)" = pi-durable ] \
  || refuse durable-runtime-unavailable "readiness record does not pin pi-durable"
[ "$(field "$readiness" runtime_qualified)" = yes ] \
  || refuse durable-runtime-unavailable "pi-durable is not qualified in the readiness record"
version=$(field "$readiness" runtime_version)
token_ok "$version" || refuse durable-runtime-unavailable "runtime_version is missing or invalid"

[ -f "$manifest" ] && [ -r "$manifest" ] || refuse manifest-missing "manifest is not a readable file"
expected=$(field "$readiness" manifest_digest)
digest_ok "$expected" || refuse manifest-unapproved "manifest_digest is missing or invalid"
actual=$(sha256_of "$manifest") || refuse digest-tool-missing "neither shasum nor sha256sum is available"
[ "$actual" = "$expected" ] \
  || refuse manifest-digest-mismatch "manifest digest $actual does not match approved $expected"

profile=$(field "$readiness" profile)
token_ok "$profile" || refuse limits-unapproved "profile is missing or invalid"
deadline=$(field "$readiness" deadline_seconds)
int_ok "$deadline" || refuse limits-unapproved "deadline_seconds is missing or not a positive integer"
vcpu=$(field "$readiness" vcpu)
int_ok "$vcpu" || refuse limits-unapproved "vcpu is missing or not a positive integer"
memory_gib=$(field "$readiness" memory_gib)
int_ok "$memory_gib" || refuse limits-unapproved "memory_gib is missing or not a positive integer"

[ -f "$intent" ] && [ -r "$intent" ] && [ -s "$intent" ] \
  || refuse intent-missing "intent is not a readable, non-empty file"
intent_text=$(cat "$intent") || refuse intent-missing "intent file cannot be read"
token_ok "$task_id" || refuse task-id-invalid "task id must be a plain identifier"
sha_ok "$base_commit" || refuse base-commit-invalid "base commit must be a full 40-character lowercase SHA"

cat <<EOF
# Cloud worker envelope

Task: $task_id
Base commit: $base_commit
Runtime: pi-durable $version
Profile: $profile
Finite worker policy: approved under $decision
Tool and instruction manifest: $expected (matches the approved digest)

## Limits

- Total deadline: $deadline seconds for the whole run. Replacing the worker does not reset it.
- Resources: $vcpu vCPU and $memory_gib GiB.
- Bash: each command has an explicit timeout no longer than the remaining total deadline, and runs with an allowlisted environment only.

## Obligations

- Work only in the provided workspace. Write code and run approved tests and builds there.
- Call only tools named in the approved manifest. The allowlist covers nested tool calls as well as direct calls.
- Return changes and proof evidence through the runner's result packaging.
- Do not push, open, approve, merge, or deploy anything. The worker holds no GitHub authority.
- Local Firstmate spawn and control surfaces, MCP tools, subscription credentials, and the local home are not provided to this run.
- No local steering, status, or inbox records exist for this run. Do not search for them.

## Local delivery

The local supervisor imports the returned result, runs its own checks, and delivers any pull request.

## Task intent

EOF
printf '%s\n' "$intent_text"
