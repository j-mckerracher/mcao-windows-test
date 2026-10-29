#!/usr/bin/env bash
# fm-awaiting-captain.sh - the awaiting-captain block: every item still waiting
# on the captain, shown at the top of each captain-facing message until the
# captain answers, declines, defers, or notes it.
#
# Two kinds of source feed one list, so no captain call is stored twice:
#   - decision: a captain-held backlog task. bin/fm-captain-hold.sh owns the
#     hold and its answer, and this script reads the holds from tasks-axi on
#     every call. Only "noted" is recorded here, in the sidecar, keyed by task
#     id and hold-set stamp, so a fresh hold of the same task raises again.
#     Deferral is the hold's own hold --until DATE, not a record here.
#     A hold of class dependency-wait or answered-bookkeeping asks the captain
#     nothing, so it is left out of this list; Bearings shows it as waiting.
#   - pr-ready, review-comment, credential, monitoring-down, blocker: records
#     in the sidecar $DATA/awaiting-captain.json, written by add, ack,
#     raise-monitoring, and restore-monitoring.
#
# Commands:
#   add --kind <kind> --summary <text> [--url <https-url>] [--source-task <id>]
#       Prints the item id. A decision is refused: hold the task instead with
#       bin/fm-captain-hold.sh hold. A pr-ready or review-comment already
#       recorded for the same URL prints that record's id and adds nothing.
#   raise-monitoring --summary <text>
#       Keeps one open monitoring-down record per outage: refreshes the summary
#       of the open record, or adds one when none is open. Prints its id.
#   restore-monitoring
#       Acknowledges the open monitoring-down record as noted. Prints nothing,
#       and does nothing when none is open.
#   list [--json]
#       Non-acknowledged items, open and deferred. Text is tab-separated.
#   render
#       Prints the block for this turn and stamps last_shown_at on each item
#       printed. Prints nothing when no item is open.
#   check
#       Reads a captain-facing final message on stdin. Prints the block to
#       lead the next message and exits 2 when items are open and the message
#       does not begin with the block. Prints nothing and exits 0 otherwise.
#   ack <id> --by <answer|decline|defer|noted> [--until YYYY-MM-DD]
#       answer, decline, and noted acknowledge. defer requires a date later
#       than today. A decision accepts only noted here.
#   reopen <id>
#       Restores an item to open.
#
# Deferred items return to open on the first call on or after deferred_until.
# FM_AWAITING_NOW (ISO-8601 UTC) replaces the clock for tests.
# Exit codes: 0 success; 2 invalid use or a refused acknowledgement, and for
# check the block-required result; 3 the sidecar is unreadable or corrupt, and
# its bytes are left unchanged; 4 the backlog cannot be read.
# Requires jq and python3.
set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
FM_ROOT="${FM_ROOT_OVERRIDE:-$(cd "$SCRIPT_DIR/.." && pwd)}"
FM_HOME="${FM_HOME:-${FM_ROOT_OVERRIDE:-$FM_ROOT}}"
STATE="${FM_STATE_OVERRIDE:-$FM_HOME/state}"
DATA="${FM_DATA_OVERRIDE:-$FM_HOME/data}"
REGISTRY="$DATA/awaiting-captain.json"
LOCK="$STATE/.awaiting-captain.lock"
SUMMARY_MAX=160
BLOCK_HEADER="Still waiting on you:"
LOCK_HELD=0

# shellcheck source=bin/fm-tasks-axi-lib.sh
# shellcheck disable=SC1091
. "$SCRIPT_DIR/fm-tasks-axi-lib.sh"
# shellcheck source=bin/fm-backlog-transition-lib.sh
# shellcheck disable=SC1091
. "$SCRIPT_DIR/fm-backlog-transition-lib.sh"
# shellcheck source=bin/fm-wake-lib.sh
# shellcheck disable=SC1091
. "$SCRIPT_DIR/fm-wake-lib.sh"

die() {  # <exit-code> <message...>
  local code=$1
  shift
  printf 'fm-awaiting-captain: %s\n' "$*" >&2
  exit "$code"
}

usage() {
  awk 'NR > 1 && /^#/ { sub(/^# ?/, ""); print; next } NR > 1 { exit }' "$0"
}

now_utc() {
  if [ -n "${FM_AWAITING_NOW:-}" ]; then
    case "$FM_AWAITING_NOW" in
      [0-9][0-9][0-9][0-9]-[0-9][0-9]-[0-9][0-9]T[0-9][0-9]:[0-9][0-9]:[0-9][0-9]Z) ;;
      *) die 2 "FM_AWAITING_NOW must be a UTC YYYY-MM-DDTHH:MM:SSZ timestamp" ;;
    esac
    printf '%s\n' "$FM_AWAITING_NOW"
  else
    date -u +%Y-%m-%dT%H:%M:%SZ
  fi
}

valid_date() {  # <YYYY-MM-DD>
  local month day
  case "$1" in
    [0-9][0-9][0-9][0-9]-[0-9][0-9]-[0-9][0-9]) ;;
    *) return 1 ;;
  esac
  month=$((10#${1:5:2}))
  day=$((10#${1:8:2}))
  [ "$month" -ge 1 ] && [ "$month" -le 12 ] && [ "$day" -ge 1 ] && [ "$day" -le 31 ]
}

release_registry_lock() {
  if [ "$LOCK_HELD" = 1 ]; then
    fm_lock_release "$LOCK" || true
    LOCK_HELD=0
  fi
}
trap release_registry_lock EXIT

lock_registry() {
  mkdir -p "$STATE" || die 3 "cannot create state directory: $STATE"
  fm_lock_acquire_wait "$LOCK"
  LOCK_HELD=1
}

# Prints the registry, or an empty one when the file is absent. Exit 3 leaves
# a corrupt file alone so the captain's record is never overwritten.
read_registry() {
  if [ ! -e "$REGISTRY" ] && [ ! -L "$REGISTRY" ]; then
    printf '{"version":1,"records":[]}\n'
    return 0
  fi
  if ! jq -e 'type == "object" and (.records | type == "array")' "$REGISTRY" >/dev/null 2>&1; then
    printf 'fm-awaiting-captain: registry is not valid JSON, left unchanged: %s\n' "$REGISTRY" >&2
    return 3
  fi
  jq -c . "$REGISTRY"
}

save_registry() {  # <registry-json>
  local tmp
  mkdir -p "$DATA" || die 3 "cannot create data directory: $DATA"
  tmp=$(mktemp "$DATA/.awaiting-captain.XXXXXX") || die 3 "cannot create a temporary registry in $DATA"
  if ! printf '%s\n' "$1" | jq . > "$tmp"; then
    rm -f -- "$tmp"
    die 3 "cannot serialize the registry for $REGISTRY"
  fi
  if ! mv -f -- "$tmp" "$REGISTRY"; then
    rm -f -- "$tmp"
    die 3 "cannot write $REGISTRY"
  fi
}

# Reads captain holds from the backlog and prints a JSON array of every held
# task that is not done, with its hold-set stamp and reason. tasks-axi cannot
# filter past-dated holds out of list output, so every held row is read here.
read -r -d '' ROWS_PY <<'PY' || true
import json, re, sys

def split_row(s):
    out, i, n = [], 0, len(s)
    while True:
        if i < n and s[i] == '"':
            j = i + 1
            while j < n and s[j] != '"':
                j += 2 if s[j] == "\\" else 1
            token = s[i:j + 1]
            try:
                out.append(json.loads(token))
            except ValueError:
                out.append(token[1:-1])
            i = j + 1
        else:
            j = s.find(",", i)
            if j < 0:
                j = n
            out.append(s[i:j])
            i = j
        if i >= n:
            return out
        i += 1

text = sys.stdin.read()
header = None
rows = []
for line in text.split("\n"):
    if header is None:
        m = re.match(r"^tasks\[[0-9]+\]\{([^}]*)\}:$", line)
        if m:
            header = m.group(1).split(",")
        continue
    if line.startswith("  "):
        rows.append(split_row(line[2:]))
    else:
        break
if header is None:
    if re.search(r"^count: 0$", text, re.M):
        sys.exit(0)
    sys.stderr.write("fm-awaiting-captain: backlog list output was not recognised\n")
    sys.exit(4)
for values in rows:
    row = dict(zip(header, values))
    if row.get("hold_kind") == "captain" and row.get("state") != "done":
        print(row.get("id", ""))
PY

# Reads one `tasks-axi show --full` record and prints one JSON object for a
# captain hold that is not done, or nothing.
read -r -d '' SHOW_PY <<'PY' || true
import base64, json, re, sys

fields = {}
for line in sys.stdin.read().split("\n"):
    m = re.match(r"^  ([a-z_]+): (.*)$", line)
    if m and m.group(1) not in fields:
        fields[m.group(1)] = m.group(2)

def value(name):
    raw = fields.get(name)
    if raw is None:
        return None
    if raw.startswith('"'):
        try:
            return json.loads(raw)
        except ValueError:
            return raw
    return raw

def clean(v):
    return None if v in (None, "", "-", "none") else v

if value("hold_kind") != "captain" or value("state") == "done":
    sys.exit(0)

# The class is read only from the hold header run, as the fleet snapshot does.
hold_class = None
for line in (value("body") or "").split("\n"):
    if line.strip() == "":
        continue
    if not re.match(r"^Captain hold (set|origin|class):", line):
        break
    c = re.match(r"^Captain hold class:[ \t]*([^ \t]+)$", line)
    if c and hold_class is None:
        hold_class = c.group(1)
if hold_class in ("dependency-wait", "answered-bookkeeping"):
    sys.exit(0)

reason = clean(value("hold_reason")) or ""
if reason.startswith("fm-hold-v1:"):
    try:
        reason = base64.b64decode(reason[len("fm-hold-v1:"):]).decode("utf-8")
    except (ValueError, UnicodeDecodeError):
        reason = ""

title = value("title") or ""
summary = " ".join((reason or title).split()) or (value("id") or "")
if len(summary) > 160:
    summary = summary[:157].rstrip() + "..."

body_first = (value("body") or "").split("\n", 1)[0]
m = re.match(r"^Captain hold set: ([0-9]{4}-[0-9]{2}-[0-9]{2}(?:T[0-9]{2}:[0-9]{2}:[0-9]{2}Z)?)$", body_first)
hold_set = m.group(1) if m else (value("created") or "unknown")
raised_at = hold_set if "T" in hold_set else hold_set + "T00:00:00Z"

print(json.dumps({
    "id": value("id"),
    "summary": summary,
    "hold_set": hold_set,
    "raised_at": raised_at,
    "hold_until": clean(value("hold_until")),
}))
PY

collect_decisions() {
  local data file rows status=0 ids id show objects=() object
  if [ ! -d "$DATA" ]; then
    printf '[]\n'
    return 0
  fi
  data=$(fm_backlog_data_absolute "$DATA") \
    || { printf 'fm-awaiting-captain: data directory cannot be resolved: %s\n' "$DATA" >&2; return 4; }
  file=$(fm_backlog_file "$data") \
    || { printf 'fm-awaiting-captain: %s\n' "${FM_BACKLOG_TRANSITION_ERROR:-backlog path unresolved}" >&2; return 4; }
  if [ ! -e "$file" ]; then
    printf '[]\n'
    return 0
  fi
  rows=$(fm_backlog_row_list "$data" --fields hold_kind,hold_until) || status=$?
  if [ "$status" -ne 0 ]; then
    printf 'fm-awaiting-captain: cannot list the backlog: %s\n' "$(printf '%s\n' "$rows" | sed -n 1p)" >&2
    return 4
  fi
  ids=$(printf '%s\n' "$rows" | python3 -I -c "$ROWS_PY") \
    || { printf 'fm-awaiting-captain: backlog rows could not be parsed\n' >&2; return 4; }
  while IFS= read -r id; do
    [ -n "$id" ] || continue
    if ! show=$(fm_backlog_row_show "$data" "$id" --full 2>&1); then
      printf 'fm-awaiting-captain: cannot read captain hold %s: %s\n' "$id" "$(printf '%s\n' "$show" | sed -n 1p)" >&2
      return 4
    fi
    object=$(printf '%s\n' "$show" | python3 -I -c "$SHOW_PY") \
      || { printf 'fm-awaiting-captain: captain hold %s could not be parsed\n' "$id" >&2; return 4; }
    if [ -n "$object" ]; then
      objects+=("$object")
    fi
  done <<<"$ids"
  if [ "${#objects[@]}" -eq 0 ]; then
    printf '[]\n'
  else
    printf '%s\n' "${objects[@]}" | jq -s -c .
  fi
}

# Jq helper: upserts the sidecar record for one decision (task id plus hold-set
# stamp), creating it when absent.
read -r -d '' JQ_DEFS <<'JQ' || true
def touch_decision($id; $hold; $patch):
  if any(.records[]; .kind == "decision" and .source_task_id == $id and .hold_set == $hold)
  then .records |= map(if .kind == "decision" and .source_task_id == $id and .hold_set == $hold then . + $patch else . end)
  else .records += [{id: $id, kind: "decision", summary: null, url: null, raised_at: null, last_shown_at: null,
       state: "open", deferred_until: null, source_task_id: $id, hold_set: $hold,
       acknowledged_by: null, acknowledged_at: null} + $patch]
  end;
JQ

# Input: {registry, decisions}. Output: {registry, items}. Decision rows whose
# hold has gone are pruned, due deferrals reopen, and items lists every
# non-acknowledged or acknowledged record, open, deferred, or not.
read -r -d '' RECON_FILTER <<'JQ' || true
. as $in
| $in.decisions as $ds
| [$in.registry.records[]
    | select(.kind != "decision"
             or (. as $r | any($ds[]; .id == $r.source_task_id and .hold_set == $r.hold_set)))
    | if .state == "deferred" and .deferred_until != null and .deferred_until <= $today
      then . + {state: "open", deferred_until: null, acknowledged_by: null, acknowledged_at: null}
      else . end] as $records
| [$records[] | select(.kind == "decision")] as $decided
| ([$ds[] as $d
    | ([$decided[] | select(.source_task_id == $d.id and .hold_set == $d.hold_set)][0]) as $rec
    | (if $rec != null and $rec.state == "acknowledged" then "acknowledged"
       elif $d.hold_until != null and $d.hold_until > $today then "deferred"
       else "open" end) as $state
    | {id: $d.id, kind: "decision", summary: $d.summary, url: null, raised_at: $d.raised_at,
       last_shown_at: ($rec.last_shown_at // null), state: $state,
       deferred_until: (if $state == "deferred" then $d.hold_until else null end),
       source_task_id: $d.id, hold_set: $d.hold_set,
       acknowledged_by: (if $state == "acknowledged" then $rec.acknowledged_by else null end),
       acknowledged_at: (if $state == "acknowledged" then $rec.acknowledged_at else null end)}
  ]) as $decisions
| {registry: ($in.registry + {records: $records}),
   items: (([$records[] | select(.kind != "decision")] + $decisions) | sort_by(.raised_at, .id))}
JQ

read -r -d '' OPEN_FILTER <<'JQ' || true
[.items[] | select(.state == "open")] | to_entries[]
| "\(.key + 1). [\(.value.kind)] \(.value.summary)\(if .value.kind == "decision" then " (\(.value.id))" else "" end)\(if .value.url then " " + .value.url else "" end)  (since \(.value.raised_at[0:10]))"
JQ

read -r -d '' MARK_FILTER <<'JQ' || true
. as $top
| reduce ($top.items[] | select(.state == "open")) as $it ($top.registry;
    if $it.kind == "decision"
    then touch_decision($it.id; $it.hold_set; {last_shown_at: $now})
    else .records |= map(if .id == $it.id then . + {last_shown_at: $now} else . end)
    end)
JQ

# Sets RECON (registry and items) and OPEN (numbered lines for open items).
build_reconciliation() {
  local registry decisions input
  registry=$(read_registry) || exit $?
  decisions=$(collect_decisions) || exit $?
  input=$(jq -n -c --argjson registry "$registry" --argjson decisions "$decisions" \
    '{registry: $registry, decisions: $decisions}') || die 3 "registry could not be read"
  RECON=$(jq -c --arg today "$TODAY" "$JQ_DEFS$RECON_FILTER" <<<"$input") \
    || die 3 "registry could not be reconciled"
  OPEN=$(printf '%s\n' "$RECON" | jq -r "$OPEN_FILTER")
}

persist_reconciled() {
  if [ -e "$REGISTRY" ] || [ "$(printf '%s' "$RECON" | jq '.registry.records | length')" -gt 0 ]; then
    save_registry "$(printf '%s' "$RECON" | jq -c .registry)"
  fi
}

mark_shown() {
  local updated
  updated=$(jq -c --arg now "$NOW" "$JQ_DEFS$MARK_FILTER" <<<"$RECON") || die 3 "registry could not be updated"
  save_registry "$updated"
}

new_id() {  # <registry-json>
  local id
  while :; do
    id="ac-$(od -An -N4 -tx1 /dev/urandom | tr -d ' \n')"
    if ! printf '%s' "$1" | jq -e --arg id "$id" 'any(.records[]; .id == $id)' >/dev/null; then
      printf '%s\n' "$id"
      return 0
    fi
  done
}

cmd_add() {
  local kind='' summary='' url='' source='' registry existing id updated len
  while [ $# -gt 0 ]; do
    case "$1" in
      --kind) [ $# -ge 2 ] || die 2 "--kind needs a value"; kind=$2; shift 2 ;;
      --summary) [ $# -ge 2 ] || die 2 "--summary needs a value"; summary=$2; shift 2 ;;
      --url) [ $# -ge 2 ] || die 2 "--url needs a value"; url=$2; shift 2 ;;
      --source-task) [ $# -ge 2 ] || die 2 "--source-task needs a value"; source=$2; shift 2 ;;
      *) die 2 "unknown add argument: $1" ;;
    esac
  done
  case "$kind" in
    decision)
      die 2 "a decision is a captain hold; hold the task with bin/fm-captain-hold.sh hold <id> --reason \"...\"" ;;
    pr-ready|review-comment|credential|monitoring-down|blocker) ;;
    *) die 2 "--kind must be one of: pr-ready review-comment credential monitoring-down blocker" ;;
  esac
  [ -n "$summary" ] || die 2 "--summary must not be empty"
  case "$summary" in
    *$'\n'*) die 2 "--summary must be one line" ;;
  esac
  len=$(jq -n --arg s "$summary" '$s | length')
  [ "$len" -le "$SUMMARY_MAX" ] || die 2 "--summary is $len characters; the limit is $SUMMARY_MAX"
  if [ -n "$url" ]; then
    case "$url" in
      https://?*) ;;
      *) die 2 "--url must start with https://" ;;
    esac
    case "$url" in
      *[[:space:]]*) die 2 "--url must not contain whitespace" ;;
    esac
  fi
  case "$kind" in
    pr-ready|review-comment)
      [ -n "$url" ] || die 2 "--kind $kind needs --url" ;;
  esac
  lock_registry
  registry=$(read_registry) || exit $?
  if [ -n "$url" ]; then
    existing=$(printf '%s' "$registry" | jq -r --arg kind "$kind" --arg url "$url" \
      '[.records[] | select(.kind == $kind and .url == $url)][0].id // empty')
    if [ -n "$existing" ]; then
      printf '%s\n' "$existing"
      exit 0
    fi
  fi
  id=$(new_id "$registry")
  updated=$(printf '%s' "$registry" | jq -c \
    --arg id "$id" --arg kind "$kind" --arg summary "$summary" --arg url "$url" \
    --arg source "$source" --arg now "$NOW" '
      .records += [{id: $id, kind: $kind, summary: $summary,
        url: (if $url == "" then null else $url end),
        raised_at: $now, last_shown_at: null, state: "open", deferred_until: null,
        source_task_id: (if $source == "" then null else $source end),
        hold_set: null, acknowledged_by: null, acknowledged_at: null}]')
  save_registry "$updated"
  printf '%s\n' "$id"
}

cmd_list() {
  local json=0 items
  while [ $# -gt 0 ]; do
    case "$1" in
      --json) json=1; shift ;;
      *) die 2 "unknown list argument: $1" ;;
    esac
  done
  lock_registry
  build_reconciliation
  persist_reconciled
  items=$(printf '%s\n' "$RECON" | jq -c '[.items[] | select(.state != "acknowledged")]')
  if [ "$json" = 1 ]; then
    printf '%s\n' "$items" | jq .
  else
    printf '%s' "$items" | jq -r '.[] | [.id, .kind, .state, (.deferred_until // "-"), .summary, (.url // "-")] | @tsv'
  fi
}

cmd_render() {
  [ $# -eq 0 ] || die 2 "render takes no arguments"
  lock_registry
  build_reconciliation
  if [ -z "$OPEN" ]; then
    persist_reconciled
    return 0
  fi
  mark_shown
  printf '%s\n%s\n' "$BLOCK_HEADER" "$OPEN"
}

cmd_check() {
  local text first
  [ $# -eq 0 ] || die 2 "check takes no arguments"
  text=$(cat)
  first=''
  while IFS= read -r line; do
    line=${line#"${line%%[![:space:]]*}"}
    line=${line%"${line##*[![:space:]]}"}
    if [ -n "$line" ]; then
      first=$line
      break
    fi
  done <<<"$text"
  [ -n "$first" ] || return 0
  [ "$first" != "$BLOCK_HEADER" ] || return 0
  lock_registry
  build_reconciliation
  if [ -z "$OPEN" ]; then
    persist_reconciled
    return 0
  fi
  mark_shown
  printf 'Begin your next final message to the captain with this block, then your reply:\n\n%s\n%s\n' "$BLOCK_HEADER" "$OPEN"
  return 2
}

cmd_ack() {
  local id='' by='' until_date='' target kind state hold updated
  while [ $# -gt 0 ]; do
    case "$1" in
      --by) [ $# -ge 2 ] || die 2 "--by needs a value"; by=$2; shift 2 ;;
      --until) [ $# -ge 2 ] || die 2 "--until needs a value"; until_date=$2; shift 2 ;;
      -*) die 2 "unknown ack argument: $1" ;;
      *)
        [ -z "$id" ] || die 2 "ack takes one id"
        id=$1
        shift
        ;;
    esac
  done
  [ -n "$id" ] || die 2 "ack needs an id"
  case "$by" in
    answer|decline|defer|noted) ;;
    *) die 2 "--by must be answer, decline, defer, or noted" ;;
  esac
  if [ "$by" = defer ]; then
    [ -n "$until_date" ] || die 2 "--by defer needs --until YYYY-MM-DD"
    valid_date "$until_date" || die 2 "--until must be a YYYY-MM-DD date"
    [[ "$until_date" > "$TODAY" ]] || die 2 "--until must be later than today ($TODAY)"
  elif [ -n "$until_date" ]; then
    die 2 "--until applies only to --by defer"
  fi
  lock_registry
  build_reconciliation
  target=$(printf '%s\n' "$RECON" | jq -c --arg id "$id" '[.items[] | select(.id == $id)][0] // empty')
  [ -n "$target" ] || die 2 "no awaiting item named $id"
  kind=$(printf '%s' "$target" | jq -r .kind)
  state=$(printf '%s' "$target" | jq -r .state)
  [ "$state" != acknowledged ] || die 2 "$id is already acknowledged"
  if [ "$kind" = decision ]; then
    [ "$by" = noted ] || die 2 "decision $id is answered or deferred through bin/fm-captain-hold.sh (answer, or hold --until DATE); only noted is recorded here"
    hold=$(printf '%s' "$target" | jq -r .hold_set)
    updated=$(printf '%s\n' "$RECON" | jq -c --arg id "$id" --arg hold "$hold" --arg now "$NOW" \
      "$JQ_DEFS"'.registry | touch_decision($id; $hold; {state: "acknowledged", acknowledged_by: "noted", acknowledged_at: $now})') \
      || die 3 "registry could not be updated"
  else
    updated=$(printf '%s\n' "$RECON" | jq -c --arg id "$id" --arg by "$by" --arg until_date "$until_date" --arg now "$NOW" '
      .registry | .records |= map(if .id == $id then
        (if $by == "defer"
         then . + {state: "deferred", deferred_until: $until_date, acknowledged_by: "defer", acknowledged_at: $now}
         else . + {state: "acknowledged", deferred_until: null, acknowledged_by: $by, acknowledged_at: $now}
         end)
        else . end)') || die 3 "registry could not be updated"
  fi
  save_registry "$updated"
}

cmd_reopen() {
  local id='' target kind updated
  while [ $# -gt 0 ]; do
    case "$1" in
      -*) die 2 "unknown reopen argument: $1" ;;
      *)
        [ -z "$id" ] || die 2 "reopen takes one id"
        id=$1
        shift
        ;;
    esac
  done
  [ -n "$id" ] || die 2 "reopen needs an id"
  lock_registry
  build_reconciliation
  target=$(printf '%s\n' "$RECON" | jq -c --arg id "$id" '[.items[] | select(.id == $id)][0] // empty')
  [ -n "$target" ] || die 2 "no awaiting item named $id"
  kind=$(printf '%s' "$target" | jq -r .kind)
  if [ "$kind" = decision ]; then
    updated=$(printf '%s\n' "$RECON" | jq -c --arg id "$id" --arg hold "$(printf '%s' "$target" | jq -r .hold_set)" \
      "$JQ_DEFS"'.registry | touch_decision($id; $hold; {state: "open", acknowledged_by: null, acknowledged_at: null})') \
      || die 3 "registry could not be updated"
  else
    updated=$(printf '%s\n' "$RECON" | jq -c --arg id "$id" '
      .registry | .records |= map(if .id == $id
        then . + {state: "open", deferred_until: null, acknowledged_by: null, acknowledged_at: null}
        else . end)') || die 3 "registry could not be updated"
  fi
  save_registry "$updated"
}

# raise-monitoring keeps one open monitoring-down record per outage. Under the
# registry lock it refreshes the open record's summary, or adds the record when
# none is open, so repeated guard runs in one outage never add a second record.
cmd_raise_monitoring() {
  local summary='' registry existing id updated len
  while [ $# -gt 0 ]; do
    case "$1" in
      --summary) [ $# -ge 2 ] || die 2 "--summary needs a value"; summary=$2; shift 2 ;;
      *) die 2 "unknown raise-monitoring argument: $1" ;;
    esac
  done
  [ -n "$summary" ] || die 2 "--summary must not be empty"
  case "$summary" in
    *$'\n'*) die 2 "--summary must be one line" ;;
  esac
  len=$(jq -n --arg s "$summary" '$s | length')
  [ "$len" -le "$SUMMARY_MAX" ] || die 2 "--summary is $len characters; the limit is $SUMMARY_MAX"
  lock_registry
  registry=$(read_registry) || exit $?
  existing=$(printf '%s' "$registry" | jq -r \
    '[.records[] | select(.kind == "monitoring-down" and .state == "open")][0].id // empty')
  if [ -n "$existing" ]; then
    updated=$(printf '%s' "$registry" | jq -c --arg id "$existing" --arg summary "$summary" \
      '.records |= map(if .id == $id then . + {summary: $summary} else . end)')
    save_registry "$updated"
    printf '%s\n' "$existing"
    exit 0
  fi
  id=$(new_id "$registry")
  updated=$(printf '%s' "$registry" | jq -c \
    --arg id "$id" --arg summary "$summary" --arg now "$NOW" '
      .records += [{id: $id, kind: "monitoring-down", summary: $summary, url: null,
        raised_at: $now, last_shown_at: null, state: "open", deferred_until: null,
        source_task_id: null, hold_set: null, acknowledged_by: null, acknowledged_at: null}]')
  save_registry "$updated"
  printf '%s\n' "$id"
}

# restore-monitoring acknowledges the open monitoring-down record as noted once
# monitoring is back. With no open record it changes nothing and creates no
# registry file.
cmd_restore_monitoring() {
  local registry open_count updated
  [ $# -eq 0 ] || die 2 "restore-monitoring takes no arguments"
  lock_registry
  registry=$(read_registry) || exit $?
  open_count=$(printf '%s' "$registry" | jq \
    '[.records[] | select(.kind == "monitoring-down" and .state == "open")] | length')
  [ "$open_count" -gt 0 ] || exit 0
  updated=$(printf '%s' "$registry" | jq -c --arg now "$NOW" '
    .records |= map(if .kind == "monitoring-down" and .state == "open"
      then . + {state: "acknowledged", acknowledged_by: "noted", acknowledged_at: $now}
      else . end)')
  save_registry "$updated"
}

main() {
  local cmd=${1:-}
  if [ $# -gt 0 ]; then
    shift
  fi
  case "$cmd" in
    add|list|render|check|ack|reopen|raise-monitoring|restore-monitoring) ;;
    -h|--help|help) usage; exit 0 ;;
    *) usage >&2; exit 2 ;;
  esac
  NOW=$(now_utc)
  TODAY=${NOW:0:10}
  "cmd_${cmd//-/_}" "$@"
}

main "$@"
