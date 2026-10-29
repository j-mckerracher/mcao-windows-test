# shellcheck shell=bash
# fm-undelivered-lib.sh - the durable ledger of decisions raised to this home
# but not yet delivered to the captain (DPES-855).
#
# Sourced by bin/fm-wake-drain.sh, which reports the ledger, and by
# bin/fm-undelivered.sh and bin/fm-send.sh, which record delivery. The caller
# sources fm-wake-lib.sh, fm-classify-lib.sh, fm-line-cap-lib.sh, and
# fm-timeout-lib.sh first. This file owns no fold, cursor, or key semantics:
# OPEN DECISIONS stays the sole owner of what is open, and this ledger only
# remembers what was raised and whether the captain received it.
#
# CONTRACT (this header is the one owner of the ledger format).
#   - Store: $STATE/.undelivered-decisions.jsonl, one JSON object per line:
#       task, key, verb, note, raised_at_epoch, raised_source
#       (at | mtime | observed), raise_line (the status line that opened the
#       decision, verbatim, or null), status_offset (its byte offset when the
#       status log had not rotated, or null), status_ident (the status log's
#       identity at raise time), delivered_at_epoch (null until delivered),
#       closed_at_epoch (null unless the fold closed the decision while it was
#       still undelivered).
#   - Creation: a drain creates an entry for each OPEN DECISIONS row unless the
#     ledger already holds, for the same task and key, either an entry with the
#     same raise_line (any delivery state) or an undelivered, unclosed entry.
#     The raise_line match replaces a byte-offset triple because rotation moves
#     offsets while copying lines verbatim, so a delivered decision that stays
#     open is never re-raised after rotation, and a reopened decision (new raise
#     line) is raised again once its earlier instance was delivered or closed.
#   - Delivery: only a recorded delivery event sets delivered_at_epoch, through
#     fm_undelivered_mark_delivered, which bin/fm-undelivered.sh and an answer
#     through bin/fm-send.sh --resolve-key both call. A presentation never sets it.
#   - Closure: an undelivered entry whose (task, key) the fold no longer lists
#     is marked closed_at_epoch, but only while the status log still has the
#     identity recorded at raise time. Closure without delivery is never a
#     delivery, and a closed entry is no longer reported. A missing or rotated
#     status log keeps its entry visible.
#   - Reporting: a drain prints UNDELIVERED DECISIONS for undelivered, unclosed
#     entries, and a DELIVERY FAILURE line once an entry is older than
#     FM_UNDELIVERED_THRESHOLD seconds (default 1800), or when the ledger cannot
#     be read, locked, or written. Nothing prints when no entry is undelivered,
#     so output stays byte-identical to the pre-ledger drain. Reporting never
#     changes the drain's exit status.
#   - Concurrency: every read-modify-write holds $STATE/.undelivered-decisions.lock
#     and replaces the ledger with an atomic rename.

_FM_UNDELIVERED_LOCK_SECONDS=10

fm_undelivered_path() {  # <state> -> ledger path
  printf '%s/.undelivered-decisions.jsonl' "$1"
}

fm_undelivered_threshold() {  # -> seconds before an undelivered decision is a DELIVERY FAILURE
  local v=${FM_UNDELIVERED_THRESHOLD:-1800}
  case "$v" in ''|*[!0-9]*|0) v=1800 ;; esac
  printf '%s' "$v"
}

_fm_undelivered_lock() {  # <state>
  fm_lock_acquire_wait_bounded "$1/.undelivered-decisions.lock" "$_FM_UNDELIVERED_LOCK_SECONDS" >/dev/null 2>&1
}

_fm_undelivered_unlock() {  # <state>
  fm_lock_release "$1/.undelivered-decisions.lock" >/dev/null 2>&1 || true
}

# Print the ledger text, one JSON object per line. A missing ledger is empty.
# An existing ledger that is unreadable, a symlink, or not a stream of JSON
# objects fails.
_fm_undelivered_load() {  # <file>
  local f=$1 text
  [ -e "$f" ] || return 0
  [ -r "$f" ] && [ ! -L "$f" ] || return 1
  text=$(LC_ALL=C command cat -- "$f" 2>/dev/null) || return 1
  printf '%s' "$text" | jq -e -s 'all(.[]; type == "object")' >/dev/null 2>&1 || return 1
  printf '%s' "$text" | jq -c .
}

# Atomically replace the ledger with <text>.
_fm_undelivered_write() {  # <state> <file> <text>
  local state=$1 file=$2 text=$3 tmp
  tmp=$(mktemp "$state/.undelivered-decisions.XXXXXX") || return 1
  if ! printf '%s\n' "$text" > "$tmp" || ! mv -f -- "$tmp" "$file"; then
    rm -f -- "$tmp"
    return 1
  fi
}

_fm_undelivered_iso() {  # <epoch> -> local ISO-8601 time
  date -r "$1" '+%Y-%m-%dT%H:%M:%S' 2>/dev/null \
    || date -d "@$1" '+%Y-%m-%dT%H:%M:%S' 2>/dev/null \
    || printf '%s' "$1"
}

_fm_undelivered_age() {  # <seconds> -> <h>h<m>m, clamped at zero
  local s=$1
  [ "$s" -ge 0 ] || s=0
  printf '%dh%dm' $((s / 3600)) $(((s % 3600) / 60))
}

# Print "<byte-offset>\t<line>" for the last needs-decision or blocked line of
# <status-file> whose decision key is <key>: the line that opened the decision
# the fold shows open now. Fails when no such line exists.
_fm_undelivered_raise_line() {  # <status-file> <key>
  local f=$1 key=$2 hits hit off line found=''
  hits=$(LC_ALL=C grep -b -E '^(needs-decision|blocked)([ :]|$)' "$f" 2>/dev/null) || return 1
  while IFS= read -r hit; do
    [ -n "$hit" ] || continue
    off=${hit%%:*}
    line=${hit#*:}
    [ "$(_fm_decision_key "$line" default)" = "$key" ] && found="$off"$'\t'"$line"
  done <<EOF
$hits
EOF
  [ -n "$found" ] || return 1
  printf '%s' "$found"
}

# Print "<task>\t<identity>" for every task that holds an undelivered, unclosed
# entry and whose status log still exists. A missing log has no identity, so its
# entries cannot be closed.
_fm_undelivered_idents() {  # <state> <text>
  local state=$1 text=$2 task ident
  while IFS= read -r task; do
    [ -n "$task" ] || continue
    ident=$(_fm_open_decisions_file_ident "$state/$task.status") || continue
    printf '%s\t%s\n' "$task" "$ident"
  done < <(printf '%s' "$text" | jq -r 'select(.delivered_at_epoch == null and .closed_at_epoch == null) | .task' | sort -u) \
    | jq -R -s -c 'split("\n") | map(select(length > 0) | split("\t") | {(.[0]): .[1]}) | add // {}'
}

# Print the updated ledger text for one drain. <open-set> is the OPEN DECISIONS
# fold as "<task>\t<key>\t<verb>\t<note>" lines. Closure runs only when the fold
# was read in full (<open-trusted> 1), so a failed fold never closes an entry.
_fm_undelivered_sync() {  # <state> <text> <open-set> <open-trusted 0|1> <now>
  local state=$1 text=$2 open=$3 trusted=$4 now=$5
  local open_json idents_json task key verb note f raise off line raised src ident entry
  open_json=$(printf '%s' "$open" | jq -R -s -c \
    'split("\n") | map(select(length > 0) | split("\t") | {task: .[0], key: .[1]})') || return 1
  if [ "$trusted" = 1 ] && [ -n "$text" ]; then
    idents_json=$(_fm_undelivered_idents "$state" "$text") || return 1
    text=$(printf '%s' "$text" | jq -c --argjson open "$open_json" --argjson idents "$idents_json" --argjson now "$now" '
      . as $r
      | if $r.delivered_at_epoch == null and $r.closed_at_epoch == null
           and ([$open[] | select(.task == $r.task and .key == $r.key)] | length == 0)
           and ($idents[$r.task] != null and $idents[$r.task] == $r.status_ident)
        then .closed_at_epoch = $now
        else . end') || return 1
  fi
  while IFS=$'\t' read -r task key verb note; do
    [ -n "$task" ] || continue
    f="$state/$task.status"
    raise=$(_fm_undelivered_raise_line "$f" "$key") || raise=''
    off=''
    line=''
    if [ -n "$raise" ]; then
      off=${raise%%$'\t'*}
      line=${raise#*$'\t'}
    fi
    if printf '%s' "$text" | jq -e -s --arg t "$task" --arg k "$key" --arg l "$line" '
        any(.[]; .task == $t and .key == $k
                 and ((.raise_line // "") == $l
                      or (.delivered_at_epoch == null and .closed_at_epoch == null)))' \
        >/dev/null 2>&1; then
      continue
    fi
    raised=''
    src=''
    if [ -n "$line" ] && raised=$(status_line_at_epoch "$line"); then
      src='at'
    elif raised=$(_fm_status_file_mtime "$f") && [ -n "$raised" ]; then
      src=mtime
    else
      raised=$now
      src=observed
    fi
    ident=$(_fm_open_decisions_file_ident "$f") || ident=''
    entry=$(jq -cn --arg task "$task" --arg key "$key" --arg verb "$verb" --arg note "$note" \
      --argjson raised "$raised" --arg src "$src" --arg line "$line" --arg off "$off" --arg ident "$ident" '
      {task: $task, key: $key, verb: $verb, note: $note,
       raised_at_epoch: $raised, raised_source: $src,
       raise_line: (if $line == "" then null else $line end),
       status_offset: (if $off == "" then null else ($off | tonumber) end),
       status_ident: $ident, delivered_at_epoch: null, closed_at_epoch: null}') || return 1
    text="${text}${text:+$'\n'}${entry}"
  done <<EOF
$open
EOF
  printf '%s' "$text"
}

# Print the UNDELIVERED DECISIONS section for <text>. Prints nothing when no
# entry is undelivered and unclosed.
_fm_undelivered_print() {  # <now> <threshold> <text>
  local now=$1 threshold=$2 text=$3 rows task key verb note raised age line
  local over=0 oldest='' shown=0 omitted=0 used=0 bytes item_bytes=220 global_bytes=2000
  rows=$(printf '%s' "$text" | jq -r \
    'select(.delivered_at_epoch == null and .closed_at_epoch == null)
     | [.task, .key, .verb, .note, (.raised_at_epoch | tostring)] | join("\u001f")') || return 0
  [ -n "$rows" ] || return 0
  while IFS=$'\037' read -r task key verb note raised; do
    [ -n "$task" ] || continue
    age=$((now - raised))
    [ "$age" -le "$threshold" ] || over=$((over + 1))
    if [ -z "$oldest" ] || [ "$raised" -lt "$oldest" ]; then oldest=$raised; fi
  done <<EOF
$rows
EOF
  printf 'UNDELIVERED DECISIONS (raised while not delivered to the captain; new):\n' || return 0
  if [ "$over" -gt 0 ]; then
    printf 'DELIVERY FAILURE: %d decision(s) not delivered to the captain; oldest raised %s (%s ago)\n' \
      "$over" "$(_fm_undelivered_iso "$oldest")" "$(_fm_undelivered_age $((now - oldest)))" || return 0
  fi
  while IFS=$'\037' read -r task key verb note raised; do
    [ -n "$task" ] || continue
    line="$task [key=$key] $verb: $note | raised $(_fm_undelivered_iso "$raised") | age $(_fm_undelivered_age $((now - raised)))"
    fm_cap_line_var "$line" $((item_bytes - 1))
    line=$FM_LINE_CAP_LINE
    bytes=$((${#line} + 1))
    if [ $((used + bytes)) -gt "$global_bytes" ]; then
      omitted=$((omitted + 1))
      continue
    fi
    printf '%s\n' "$line" || return 0
    used=$((used + bytes))
    shown=$((shown + 1))
  done <<EOF
$rows
EOF
  if [ "$omitted" -gt 0 ]; then
    printf 'UNDELIVERED DECISIONS: %d more omitted (byte cap)\n' "$omitted" || return 0
  fi
}

# Report the ledger for one drain: sync it against the fold, persist the result,
# then print. Always returns 0; each failure is printed instead.
#   <state> <open-set> <open-trusted 0|1> <now>
fm_undelivered_section() {
  local state=$1 open=$2 trusted=$3 now=$4 file orig='' text='' write_failed=0 threshold
  file=$(fm_undelivered_path "$state")
  threshold=$(fm_undelivered_threshold)
  if ! _fm_undelivered_lock "$state"; then
    printf 'DELIVERY FAILURE: undelivered-decision record could not be locked; decisions may be undelivered\n'
    return 0
  fi
  if ! orig=$(_fm_undelivered_load "$file"); then
    _fm_undelivered_unlock "$state"
    printf 'DELIVERY FAILURE: undelivered-decision record unreadable; decisions may be undelivered\n'
    return 0
  fi
  text=$orig
  if synced=$(_fm_undelivered_sync "$state" "$orig" "$open" "$trusted" "$now"); then
    if [ "$synced" != "$orig" ]; then
      if _fm_undelivered_write "$state" "$file" "$synced"; then
        text=$synced
      else
        write_failed=1
      fi
    fi
  else
    write_failed=1
  fi
  _fm_undelivered_unlock "$state"
  if [ "$write_failed" -eq 1 ]; then
    printf 'DELIVERY FAILURE: undelivered-decision record could not be updated; decisions may be undelivered\n'
  fi
  _fm_undelivered_print "$now" "$threshold" "$text"
  return 0
}

# Record that <task>'s decision <key> reached the captain: every undelivered,
# unclosed entry for it gets delivered_at_epoch. Idempotent: an absent ledger,
# or no matching undelivered entry, changes nothing. Returns 1 only when the
# ledger exists and cannot be read, locked, or written.
fm_undelivered_mark_delivered() {  # <state> <task> <key> <now>
  local state=$1 task=$2 key=$3 now=$4 file orig text rc=0
  file=$(fm_undelivered_path "$state")
  [ -e "$file" ] || return 0
  _fm_undelivered_lock "$state" || return 1
  if ! orig=$(_fm_undelivered_load "$file"); then
    _fm_undelivered_unlock "$state"
    return 1
  fi
  if text=$(printf '%s' "$orig" | jq -c --arg t "$task" --arg k "$key" --argjson now "$now" '
      if .task == $t and .key == $k and .delivered_at_epoch == null and .closed_at_epoch == null
        then .delivered_at_epoch = $now
        else . end'); then
    if [ "$text" != "$orig" ]; then
      _fm_undelivered_write "$state" "$file" "$text" || rc=1
    fi
  else
    rc=1
  fi
  _fm_undelivered_unlock "$state"
  return "$rc"
}
