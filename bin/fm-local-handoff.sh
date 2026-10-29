#!/usr/bin/env bash
# fm-local-handoff.sh - durable local custody for a verified cloud import (DPES-880).
#
# One receipt per logical handoff lives at
# ${FM_DATA_OVERRIDE:-$FM_HOME/data}/handoffs/<identity>.json.
#
# The identity is a digest of the logical key: owner, task, incarnation, run,
# result generation, and result digest. The binding digest covers the whole
# immutable binding. Recording the same identity with a different binding
# digest is refused, and the stored receipt stays byte-identical. Identity never
# comes from timestamps or a fresh random value, so a replayed event converges
# on the same receipt.
#
# A receipt moves forward one phase at a time, each step backed by evidence
# checked against the binding:
#   pending_custody -> assignment_intent -> adoption_acknowledged
#     -> producer_running -> pr_intent -> pr_observed
# Re-running the current phase with the same evidence succeeds without a write.
# A hold on the receipt refuses every advance until it is cleared.
#
# Usage:
#   fm-local-handoff.sh record <binding.json>                    publish custody, print identity
#   fm-local-handoff.sh show <identity>                          print the stored receipt
#   fm-local-handoff.sh advance <identity> <phase> [evidence]    move the receipt one phase
#   fm-local-handoff.sh hold <identity> --reason <text> | --clear hold or release the receipt
#
# Evidence options, each valid only for its own phase:
#   assignment_intent       --local-task <task-id>
#   adoption_acknowledged   --ack <ack.json>   exactly {task_id, task_incarnation,
#                           worktree, head}, where head is the binding's imported_head
#   producer_running        --producer <direct-PR|no-mistakes>, the binding's delivery_mode
#   pr_intent               --branch <name> --base <name> --head <40-hex>, where
#                           branch and base equal the binding's branch and pr_base
#   pr_observed             --pr <readback.json>, exactly {base, draft, head, number,
#                           url} as read back from the forge; url must be the canonical
#                           URL for the binding repo and number, and head and base must
#                           equal the recorded pr_intent
#
# Exit codes: 0 ok (a replay is also 0), 2 refused with
# `refused: <gate>: <reason>` on stderr, 64 usage error. Gates: input, schema,
# delivery-mode, identity, digest, receipt, durability, hold, transition, evidence,
# lock.
#
# Custody is durable before record exits 0. Callers acknowledge the import
# event only on exit 0, never on a refusal.
#
# Advances and holds serialize per receipt on the wake library's bounded lock.
# Sourcing that library creates its state directory, so FM_STATE_OVERRIDE points
# it at the handoff data root and never into a checkout.
#
# BLOCKING (DPES-817): nothing calls record or advance yet. The verified-import
# publication that should trigger them, and the import receipt fields it binds,
# are unmerged.
# BLOCKING (DPES-805): binding field names are this slice's contract, not the
# merged cloud protocol. Reconcile them before any production enablement.
# BLOCKING (adoption): nothing writes the adoption ack yet. The adoption producer
# that writes it is not implemented.
# BLOCKING (forge read-back): nothing writes the pr_observed read-back yet. This
# script never queries GitHub; the caller supplies the read-back file.
set -u
set -o pipefail

SCRIPT_DIR=$(cd "$(dirname "$0")" && pwd)
BINDING_KEYS='["base_commit","branch","delivery_mode","import_receipt","imported_head","owner","pr_base","repo","result_digest","result_generation","run_id","source_ref","task_id","task_incarnation","worktree"]'
LOGICAL_KEY_FILTER='{owner, task_id, task_incarnation, run_id, result_generation, result_digest}'
PHASE_ORDER=(pending_custody assignment_intent adoption_acknowledged producer_running pr_intent pr_observed)
LOCK_WAIT_SECONDS=10
# Whole-value 40-hex commit id test. It compares the JSON value, so a trailing
# newline cannot be stripped into a match the way $(...) would strip it.
SHA40_JQ='type == "string" and length == 40 and test("^[0-9a-f]{40}$")'

usage() {
  printf '%s\n' 'usage: fm-local-handoff.sh record <binding.json>' \
    '       fm-local-handoff.sh show <identity>' \
    '       fm-local-handoff.sh advance <identity> <phase> [evidence options]' \
    '       fm-local-handoff.sh hold <identity> --reason <text> | --clear' >&2
  exit 64
}

refuse() {  # <gate> <reason>; always returns 2 so callers can propagate it
  printf 'refused: %s: %s\n' "$1" "$2" >&2
  return 2
}

handoff_dir() {
  printf '%s/handoffs\n' "${FM_DATA_OVERRIDE:-${FM_HOME:?FM_HOME or FM_DATA_OVERRIDE must be set}/data}"
}

sha256_stdin() {
  if command -v shasum >/dev/null 2>&1; then
    shasum -a 256 | awk '{print $1}'
  else
    sha256sum | awk '{print $1}'
  fi
}

# durable_sync <path>: flushes a file, then its directory entry, to stable storage.
# Same policy as bin/fm-audit-writer.py: F_FULLFSYNC on macOS, which also drains
# the drive cache, with fsync as the fallback. Fails unless both flushes succeed.
durable_sync() {  # <path>
  python3 -I -c '
import errno, fcntl, os, sys

def sync_fd(fd):
    full = getattr(fcntl, "F_FULLFSYNC", None)
    if full is None:
        os.fsync(fd)
        return
    try:
        fcntl.fcntl(fd, full)
    except OSError as exc:
        if exc.errno not in (errno.ENOTSUP, errno.EINVAL, errno.ENOTTY):
            raise
        os.fsync(fd)

path = sys.argv[1]
for target in (path, os.path.dirname(os.path.abspath(path))):
    fd = os.open(target, os.O_RDONLY)
    try:
        sync_fd(fd)
    finally:
        os.close(fd)
' "$1"
}

# A receipt is addressed by its identity, so a malformed one is refused before
# it can name any path. Whole-string match: a line-anchored grep would accept
# "<64 hex>\n<anything>".
identity_ok() {  # <identity>
  case "$1" in
    *[!0-9a-f]* | "") refuse identity "malformed identity: $1"; return ;;
  esac
  [ "${#1}" -eq 64 ] || refuse identity "malformed identity: $1"
}

# Strict binding schema: exactly the known keys, each a non-empty string, a
# delivery mode that can open a PR, and an absolute worktree path. Anything
# else is refused before any state is written.
validate_binding() {  # <binding.json>
  local file=$1
  if ! jq -e 'type == "object"' "$file" >/dev/null 2>&1; then
    refuse input "binding is not a JSON object: $file"
    return
  fi
  if ! jq -e --argjson keys "$BINDING_KEYS" '(keys | sort) == ($keys | sort)' "$file" >/dev/null; then
    refuse schema "binding must contain exactly the required keys"
    return
  fi
  if ! jq -e '[.[] | type == "string" and length > 0] | all' "$file" >/dev/null; then
    refuse schema "every binding value must be a non-empty string"
    return
  fi
  # Exact JSON checks: $(jq -r ...) would strip a trailing newline and accept
  # "direct-PR\n" as direct-PR.
  if ! jq -e '.delivery_mode == "no-mistakes" or .delivery_mode == "direct-PR"' "$file" >/dev/null; then
    refuse delivery-mode "local-only cannot open a PR; the handoff needs no-mistakes or direct-PR"
    return
  fi
  if ! jq -e '.worktree | startswith("/")' "$file" >/dev/null; then
    refuse schema "worktree must be an absolute path"
    return
  fi
  if ! jq -e ".imported_head | $SHA40_JQ" "$file" >/dev/null; then
    refuse schema "imported_head must be a 40-character lowercase commit id"
    return
  fi
  return 0
}

cmd_record() {  # <binding.json>
  local src=$1 canon key identity binding_digest dir target tmp
  validate_binding "$src" || return
  canon=$(jq -S -c . "$src") || return 2
  key=$(jq -S -c "$LOGICAL_KEY_FILTER" <<<"$canon") || return 2
  identity=$(printf '%s' "$key" | sha256_stdin) \
    || {
      refuse digest "could not compute the identity digest; nothing recorded"
      return
    }
  binding_digest=$(printf '%s' "$canon" | sha256_stdin) \
    || {
      refuse digest "could not compute the binding digest; nothing recorded"
      return
    }
  dir=$(handoff_dir)
  target="$dir/$identity.json"
  mkdir -p "$dir" || return 1

  if [ -e "$target" ]; then
    compare_existing "$target" "$identity" "$binding_digest"
    return
  fi

  tmp=$(mktemp "$dir/.$identity.XXXXXX") || return 1
  if ! jq -n --arg identity "$identity" --arg digest "$binding_digest" --argjson binding "$canon" \
    '{version: 1, identity: $identity, binding_digest: $digest, binding: $binding,
      phase: "pending_custody", hold: null, evidence: {}}' >"$tmp"; then
    rm -f "$tmp"
    return 1
  fi
  if ! durable_sync "$tmp"; then
    rm -f "$tmp"
    refuse durability "receipt is not flushed to stable storage; nothing recorded"
    return
  fi
  # ln fails when the target already exists, so creation is atomic and
  # create-once even when two consumers race for the same identity.
  if ln "$tmp" "$target" 2>/dev/null; then
    rm -f "$tmp"
    # Linked but not yet flushed: refuse rather than acknowledge. A replay flushes
    # the linked receipt before it acknowledges.
    durable_sync "$target" || {
      refuse durability "receipt is linked but not flushed; not acknowledged"
      return
    }
    printf '%s\n' "$identity"
    printf 'phase=pending_custody\n'
    return 0
  fi
  rm -f "$tmp"
  compare_existing "$target" "$identity" "$binding_digest"
}

# verify_receipt <receipt>: the stored binding must still hash to its stored digest.
# A receipt edited under its original digest is refused, never replayed or advanced.
verify_receipt() {
  local binding actual
  jq -e '.binding_digest | type == "string"' "$1" >/dev/null 2>&1 \
    || {
      refuse receipt "stored receipt is not a readable custody record"
      return
    }
  # Hash the canonical bytes record hashed: compact, no trailing newline.
  binding=$(jq -S -c .binding "$1" 2>/dev/null) \
    || {
      refuse receipt "stored binding is not readable JSON"
      return
    }
  actual=$(printf '%s' "$binding" | sha256_stdin) \
    || {
      refuse digest "could not digest the stored binding; receipt left unchanged"
      return
    }
  jq -e --arg d "$actual" '.binding_digest == $d' "$1" >/dev/null 2>&1 \
    || {
      refuse receipt "stored binding no longer matches its digest; receipt left unchanged"
      return
    }
}

compare_existing() {  # <target> <identity> <binding-digest>
  verify_receipt "$1" || return
  if ! jq -e --arg d "$3" '.binding_digest == $d' "$1" >/dev/null; then
    refuse identity "changed binding under identity $2; stored receipt left unchanged"
    return
  fi
  # A replay acknowledges only a receipt that is durable now, so a receipt linked
  # by an earlier call whose flush failed is flushed before it is acknowledged.
  durable_sync "$1" || {
    refuse durability "receipt is not flushed to stable storage; not acknowledged"
    return
  }
  printf '%s\n' "$2"
  printf 'phase=%s\n' "$(jq -r .phase "$1")"
  return 0
}

cmd_show() {  # <identity>
  local target
  identity_ok "$1" || return
  target="$(handoff_dir)/$1.json"
  if [ ! -f "$target" ]; then
    refuse identity "no receipt for $1"
    return
  fi
  jq . "$target"
}

# --- advance and hold --------------------------------------------------------

phase_rank() {  # <phase>: prints its position in PHASE_ORDER; fails for an unknown phase
  local rank=0 p
  for p in "${PHASE_ORDER[@]}"; do
    if [ "$p" = "$1" ]; then
      printf '%s\n' "$rank"
      return 0
    fi
    rank=$((rank + 1))
  done
  return 1
}

# Atomic, durable replace of a receipt: the new bytes are flushed before the
# rename, and the rename is flushed before the write is reported. The caller
# holds the receipt lock.
# shellcheck disable=SC2329 # Called by the lock-guarded advance and hold paths.
receipt_write() {  # <target> <json>
  local tmp
  tmp=$(mktemp "${1%/*}/.receipt.XXXXXX") || return 1
  if ! printf '%s\n' "$2" >"$tmp" || ! durable_sync "$tmp"; then
    rm -f "$tmp"
    refuse durability "receipt is not flushed to stable storage; nothing advanced"
    return
  fi
  mv -f "$tmp" "$1" || {
    rm -f "$tmp"
    return 1
  }
  durable_sync "$1" || refuse durability "advance is renamed but not flushed; not acknowledged"
}

evidence_add() {  # <supplied-json> <key> <value>: merged evidence; refuses a repeated option
  if jq -e --arg k "$2" 'has($k)' <<<"$1" >/dev/null; then
    refuse input "option given more than once: $2"
    return
  fi
  jq -c --arg k "$2" --arg v "$3" '. + {($k): $v}' <<<"$1"
}

# evidence_for <phase> <binding-json> <supplied-json> <prior-evidence-json>: prints
# the normalized evidence for the phase. Refuses when the supplied options are not
# exactly that phase's evidence, or when they contradict the binding or the
# evidence already recorded for earlier phases.
# shellcheck disable=SC2329 # Called from advance_locked, which runs under with_receipt_lock.
evidence_for() {
  local phase=$1 binding=$2 supplied=$3 prior=$4 want ack actual expected
  local prfile observed
  case "$phase" in
    assignment_intent) want='["local_task"]' ;;
    adoption_acknowledged) want='["ack"]' ;;
    producer_running) want='["producer"]' ;;
    pr_intent) want='["base","branch","head"]' ;;
    pr_observed) want='["pr"]' ;;
    *)
      refuse transition "unknown phase: $phase"
      return
      ;;
  esac
  if ! jq -e --argjson want "$want" 'keys == ($want | sort)' <<<"$supplied" >/dev/null; then
    refuse evidence "$phase takes only its own evidence options; got: $(jq -r 'if length == 0 then "none" else keys | join(",") end' <<<"$supplied")"
    return
  fi
  case "$phase" in
    assignment_intent)
      jq -e -n --argjson s "$supplied" --argjson b "$binding" '$s.local_task == $b.task_id' >/dev/null \
        || {
          refuse evidence "local task does not match the binding task"
          return
        }
      jq -S -c . <<<"$supplied"
      ;;
    adoption_acknowledged)
      ack=$(jq -r .ack <<<"$supplied")
      [ -f "$ack" ] || {
        refuse evidence "adoption ack file not found: $ack"
        return
      }
      expected=$(jq -S -c '{task_id, task_incarnation, worktree, head: .imported_head}' <<<"$binding")
      actual=$(jq -S -c . "$ack" 2>/dev/null) || {
        refuse evidence "adoption ack is not JSON"
        return
      }
      [ "$actual" = "$expected" ] || {
        refuse evidence "adoption ack must match the imported checkout exactly"
        return
      }
      printf '%s\n' "$expected"
      ;;
    producer_running)
      jq -e -n --argjson s "$supplied" --argjson b "$binding" '$s.producer == $b.delivery_mode' >/dev/null \
        || {
          refuse evidence "producer must be the binding delivery mode"
          return
        }
      jq -S -c . <<<"$supplied"
      ;;
    pr_intent)
      jq -e -n --argjson s "$supplied" --argjson b "$binding" '$s.branch == $b.branch' >/dev/null \
        || {
          refuse evidence "PR branch must equal the binding branch"
          return
        }
      jq -e -n --argjson s "$supplied" --argjson b "$binding" '$s.base == $b.pr_base' >/dev/null \
        || {
          refuse evidence "PR base must equal the binding pr_base"
          return
        }
      jq -e ".head | $SHA40_JQ" <<<"$supplied" >/dev/null \
        || {
          refuse evidence "PR head must be a 40-character lowercase commit id"
          return
        }
      jq -S -c . <<<"$supplied"
      ;;
    pr_observed)
      prfile=$(jq -r .pr <<<"$supplied")
      [ -f "$prfile" ] || {
        refuse evidence "PR read-back file not found: $prfile"
        return
      }
      observed=$(jq -S -c . "$prfile" 2>/dev/null) || {
        refuse evidence "PR read-back is not JSON"
        return
      }
      jq -e 'keys == ["base","draft","head","number","url"]' <<<"$observed" >/dev/null \
        || {
          refuse evidence "PR read-back must contain exactly base, draft, head, number, and url"
          return
        }
      jq -e '(.number | type == "number") and .number > 0 and .number == (.number | floor)' <<<"$observed" >/dev/null \
        || {
          refuse evidence "PR number must be a positive integer"
          return
        }
      jq -e --argjson b "$binding" '.url == ("https://github.com/" + $b.repo + "/pull/" + (.number | tostring))' <<<"$observed" >/dev/null \
        || {
          refuse evidence "PR url must be the canonical URL for the binding repo and PR number"
          return
        }
      jq -e '.draft | type == "boolean"' <<<"$observed" >/dev/null \
        || {
          refuse evidence "PR draft state must be a boolean"
          return
        }
      jq -e --argjson p "$prior" '.head == $p.pr_intent.head' <<<"$observed" >/dev/null \
        || {
          refuse evidence "PR head must equal the recorded pr_intent head"
          return
        }
      jq -e --argjson p "$prior" '.base == $p.pr_intent.base' <<<"$observed" >/dev/null \
        || {
          refuse evidence "PR base must equal the recorded pr_intent base"
          return
        }
      printf '%s\n' "$observed"
      ;;
  esac
}

# advance_locked <target> <identity> <phase> <supplied-json>: the caller holds the receipt lock.
# shellcheck disable=SC2329 # Passed by name to with_receipt_lock.
advance_locked() {
  local target=$1 identity=$2 phase=$3 supplied=$4 cur_phase hold binding
  local cur_rank to_rank evidence stored updated prior
  verify_receipt "$target" || return
  cur_phase=$(jq -r .phase "$target") || return 1
  hold=$(jq -c .hold "$target") || return 1
  if [ "$hold" != null ]; then
    refuse hold "receipt is held ($(jq -r .reason <<<"$hold")); clear the hold before advancing"
    return
  fi
  cur_rank=$(phase_rank "$cur_phase") || {
    refuse receipt "stored phase is not a known phase: $cur_phase"
    return
  }
  to_rank=$(phase_rank "$phase") || return 1
  if [ "$to_rank" -lt "$cur_rank" ]; then
    refuse transition "cannot move backward from $cur_phase to $phase"
    return
  fi
  if [ "$to_rank" -gt $((cur_rank + 1)) ]; then
    refuse transition "cannot skip from $cur_phase to $phase"
    return
  fi
  binding=$(jq -c .binding "$target") || return 1
  prior=$(jq -c .evidence "$target") || return 1
  evidence=$(evidence_for "$phase" "$binding" "$supplied" "$prior") || return
  if [ "$to_rank" -eq "$cur_rank" ]; then
    stored=$(jq -S -c --arg p "$phase" '.evidence[$p]' "$target") || return 1
    if [ "$stored" = "$evidence" ]; then
      durable_sync "$target" || {
        refuse durability "receipt is not flushed to stable storage; not acknowledged"
        return
      }
      printf '%s\n' "$identity"
      printf 'phase=%s\n' "$cur_phase"
      return 0
    fi
    refuse evidence "$phase is already recorded with different evidence; stored receipt left unchanged"
    return
  fi
  updated=$(jq --arg p "$phase" --argjson ev "$evidence" '.phase = $p | .evidence[$p] = $ev' "$target") || return 1
  receipt_write "$target" "$updated" || return
  printf '%s\n' "$identity"
  printf 'phase=%s\n' "$phase"
}

# hold_locked <target> <identity> <hold-json>: the caller holds the receipt lock.
# shellcheck disable=SC2329 # Passed by name to with_receipt_lock.
hold_locked() {
  local target=$1 identity=$2 updated
  verify_receipt "$target" || return
  updated=$(jq --argjson h "$3" '.hold = $h' "$target") || return 1
  receipt_write "$target" "$updated" || return
  printf '%s\n' "$identity"
  printf 'phase=%s\n' "$(jq -r .phase "$target")"
}

# with_receipt_lock <lockdir> <function> [args...]: runs the function holding the
# receipt lock, refusing rather than waiting past LOCK_WAIT_SECONDS.
with_receipt_lock() {
  local lock=$1 rc
  shift
  if ! fm_lock_acquire_wait_max "$lock" "$LOCK_WAIT_SECONDS"; then
    refuse lock "receipt is busy; retry"
    return
  fi
  "$@"
  rc=$?
  fm_lock_release "$lock" || true
  return $rc
}

# The wake library creates its state directory when sourced. Pointing it at the
# handoff data root keeps that write out of the checkout.
load_lock_lib() {  # <handoff-dir>
  FM_STATE_OVERRIDE=$1
  export FM_STATE_OVERRIDE
  # shellcheck source=bin/fm-wake-lib.sh
  . "$SCRIPT_DIR/fm-wake-lib.sh"
}

cmd_advance() {  # <identity> <phase> [evidence options]
  local identity=$1 phase=$2 supplied='{}' dir target
  shift 2
  while [ $# -gt 0 ]; do
    [ $# -ge 2 ] || {
      refuse input "$1 needs a value"
      return
    }
    case "$1" in
      --local-task) supplied=$(evidence_add "$supplied" local_task "$2") || return ;;
      --ack) supplied=$(evidence_add "$supplied" ack "$2") || return ;;
      --producer) supplied=$(evidence_add "$supplied" producer "$2") || return ;;
      --branch) supplied=$(evidence_add "$supplied" branch "$2") || return ;;
      --base) supplied=$(evidence_add "$supplied" base "$2") || return ;;
      --head) supplied=$(evidence_add "$supplied" head "$2") || return ;;
      --pr) supplied=$(evidence_add "$supplied" pr "$2") || return ;;
      *)
        refuse input "unknown advance option: $1"
        return
        ;;
    esac
    shift 2
  done
  if [ "$phase" = pending_custody ]; then
    refuse transition "pending_custody is only set by record"
    return
  fi
  phase_rank "$phase" >/dev/null || {
    refuse transition "unknown phase: $phase"
    return
  }
  identity_ok "$identity" || return
  dir=$(handoff_dir) || return
  target="$dir/$identity.json"
  [ -f "$target" ] || {
    refuse identity "no receipt for $identity"
    return
  }
  load_lock_lib "$dir"
  with_receipt_lock "$dir/.$identity.lock" advance_locked "$target" "$identity" "$phase" "$supplied"
}

cmd_hold() {  # <identity> (--reason <text> | --clear)
  local identity=$1 hold dir target
  shift
  case "${1:-}" in
    --reason)
      [ $# -eq 2 ] && [ -n "$2" ] || usage
      hold=$(jq -cn --arg r "$2" '{reason: $r}') || return 1
      ;;
    --clear)
      [ $# -eq 1 ] || usage
      hold=null
      ;;
    *) usage ;;
  esac
  identity_ok "$identity" || return
  dir=$(handoff_dir) || return
  target="$dir/$identity.json"
  [ -f "$target" ] || {
    refuse identity "no receipt for $identity"
    return
  }
  load_lock_lib "$dir"
  with_receipt_lock "$dir/.$identity.lock" hold_locked "$target" "$identity" "$hold"
}

[ $# -ge 1 ] || usage
case "$1" in
  record)
    [ $# -eq 2 ] || usage
    cmd_record "$2"
    exit $?
    ;;
  show)
    [ $# -eq 2 ] || usage
    cmd_show "$2"
    exit $?
    ;;
  advance)
    [ $# -ge 3 ] || usage
    shift
    cmd_advance "$@"
    exit $?
    ;;
  hold)
    [ $# -ge 2 ] || usage
    shift
    cmd_hold "$@"
    exit $?
    ;;
  *) usage ;;
esac
