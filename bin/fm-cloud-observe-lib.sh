#!/usr/bin/env bash
# fm-cloud-observe-lib.sh - the single owner of how a task that runs on the
# cloud backplane is classified and read: its execution target, the bounded
# shared status read, the freshness rules, and the crew-state projection.
#
# Sourced, never executed. Requires bin/fm-backend.sh (fm_meta_get),
# bin/fm-timeout-lib.sh (fm_run_timed), and bin/fm-execution-target-lib.sh (the
# cloud run id and the `mcao cloud` argv) to be sourced first. No side effects on
# source.
#
# Execution target. A record with execution_target=gcp runs on the cloud
# backplane and has no local window, pane, or worktree to consult, so local
# liveness checks must not run for it. A record without the key, or with
# execution_target=local, is local. Any other value classifies as invalid, and
# callers report it as unknown rather than reading it as local.
#
# Status read. fm_cloud_observe reads the record's cloud_run_id through
# fm_exec_cloud_run_timed, which owns the `mcao cloud status --run-id` argv. The
# response is the merged protocol-v1 StatusView
# (cloud/schemas/protocol-v1/status-view.json). Its run state, status_seq, and
# server-computed freshness are taken as the server reports them, and the
# heartbeat age is measured on the server clock, so a skewed local clock never
# creates or hides liveness. The client's argv is still unverified against a
# merged mcao client (see bin/fm-execution-target-lib.sh).
#
# Observation rules, applied by fm_cloud_observe:
#   - Terminal states (succeeded, failed, timed_out, cancelled) are monotonic. A
#     response naming a different state never replaces a stored terminal state.
#   - A response whose status_seq is below the stored sequence is out of order and
#     is ignored.
#   - A response naming a different run than the record's cloud_run_id is refused.
#   - A failed or invalid read keeps the stored observation and marks freshness
#     unknown. It never reads as idle, dead, or success.
#   - The server's freshness is fresh or stale; stale is telemetry, not termination.
#     clock_anomaly has unknown freshness, so it cannot create false liveness.
#   - StatusView carries no result. A succeeded run therefore never projects as
#     ready for local import and review; its crew state is unknown until a result
#     read is wired.
#
# Crew-state projection. fm_cloud_observe prints one JSON object whose crew_state
# uses the bin/fm-crew-state.sh vocabulary: working, parked, done, blocked,
# failed, or unknown. Nothing the status read reports is parked, done, or landed.

FM_CLOUD_READ_TIMEOUT_SECONDS=5

FM_CLOUD_STATES_JSON='["awaiting_upload","queued","dispatching","starting","running","collecting","reconciling","cancel_requested","succeeded","failed","timed_out","cancelled"]'

# shellcheck disable=SC2016  # jq program text: $states and $run are bound by --argjson and --arg.
FM_CLOUD_VALID_JQ='
  def utc_ts: type == "string" and test("^[0-9]{4}-[0-9]{2}-[0-9]{2}T[0-9]{2}:[0-9]{2}:[0-9]{2}(\\.[0-9]+)?Z$");
  type == "object"
  and (.status_seq | type == "number" and . >= 0 and floor == .)
  and (.run | type == "object")
  and (.run.run_id == $run)
  and (.run.state as $s | $states | any(. == $s))
  and (.run.freshness as $f | ["fresh","stale","clock_anomaly"] | any(. == $f))
  and (.run.server_time | utc_ts)
  and ((.run.last_seen_at == null) or (.run.last_seen_at | utc_ts))
  and ((.run.failure == null) or (.run.failure | type == "object"))
'

# shellcheck disable=SC2016  # jq program text: $prev and $now are jq variables bound by --argjson.
FM_CLOUD_FOLD_JQ='
  def terminal: . as $s | ["succeeded","failed","timed_out","cancelled"] | any(. == $s);
  def epoch: sub("\\.[0-9]+Z$"; "Z") | fromdateiso8601;
  ($prev | if type == "object" then . else {} end) as $p
  | .run as $r
  | if ($p.state != null) and ($p.state | terminal) and ($r.state != $p.state) then
      {stored: $p, accepted: false, diag: "terminal state \($p.state) is final; response \($r.state) ignored"}
    elif ($p.sequence != null) and (.status_seq < $p.sequence) then
      {stored: $p, accepted: false, diag: "out-of-order response ignored"}
    else
      {stored: {state: $r.state, sequence: .status_seq, freshness: $r.freshness,
                last_seen_at: ($r.last_seen_at // null),
                heartbeat_age: (if $r.last_seen_at == null then null
                                else ((.run.server_time | epoch) - ($r.last_seen_at | epoch)) end),
                failure: ($r.failure // null), observed_at: $now},
       accepted: true, diag: ""}
    end
'

# shellcheck disable=SC2016  # jq program text: $current and $diag are bound by --argjson and --arg.
FM_CLOUD_PROJECT_JQ='
  def terminal: . as $s | ["succeeded","failed","timed_out","cancelled"] | any(. == $s);
  . as $o
  | (if ($o.state // null) == null then "none" else $o.state end) as $state
  | ($o.heartbeat_age // null) as $age
  | (if $state == "none" then "unknown"
     elif ($state | terminal) then "final"
     elif ($current | not) then "unknown"
     elif ($o.freshness == "fresh" or $o.freshness == "stale") then $o.freshness
     else "unknown" end) as $freshness
  | (if $state == "none" then "unknown"
     elif $state == "succeeded" then "unknown"
     elif ($state == "failed" or $state == "timed_out" or $state == "cancelled") then "failed"
     elif $freshness == "fresh" then "working"
     else "unknown" end) as $crew
  | (if $state == "none" then "cloud status unavailable: \($diag)"
     elif $state == "succeeded" then "cloud run succeeded; the status view carries no result, so nothing is reported ready for import"
     elif $state == "failed" then "cloud run failed\(if $o.failure.code == null then "" else " (\($o.failure.code))" end)"
     elif $state == "timed_out" then "cloud run timed out"
     elif $state == "cancelled" then "cloud run cancelled"
     elif $freshness == "fresh" then "cloud run \($state)"
     else "last observed cloud run \($state); telemetry \($freshness)\(if $diag == "" then "" else ": \($diag)" end)" end) as $detail
  | {state: (if $state == "none" then null else $state end), crew_state: $crew, freshness: $freshness,
     heartbeat_age: $age, detail: $detail, diagnostic: $diag}
'

# fm_cloud_execution_target <meta-file>
# Prints local, gcp, or invalid. A missing record reads as local, as it always has.
fm_cloud_execution_target() {
  case "$(fm_meta_get "$1" execution_target)" in
    ''|local) printf 'local\n' ;;
    gcp) printf 'gcp\n' ;;
    *) printf 'invalid\n' ;;
  esac
}

# fm_cloud_observe <meta-file> <observation-file> <now-epoch>
# Reads the run's status once, folds a valid response into the stored
# observation, and prints the projection JSON. The observation file is rewritten
# only when a response is accepted. Returns 2 when <now-epoch> is not an integer.
fm_cloud_observe() {
  local meta=$1 obs=$2 now=$3 run='' raw='' rc=0 diag='' current=false stored='{}' fold accepted tmp
  case "$now" in ''|*[!0-9]*) return 2 ;; esac
  if [ -f "$obs" ] && jq -e 'type == "object"' "$obs" >/dev/null 2>&1; then
    stored=$(jq -c . "$obs")
  fi
  if ! run=$(fm_exec_meta_exact_value "$meta" cloud_run_id 2>/dev/null); then
    diag='no single cloud run id is recorded for this task'
  elif ! fm_exec_cloud_client_check 2>/dev/null; then
    diag='cloud client mcao is not installed'
  else
    raw=$(fm_exec_cloud_run_timed "$FM_CLOUD_READ_TIMEOUT_SECONDS" status --run-id "$run" 2>/dev/null) || rc=$?
    if [ "$rc" -ne 0 ]; then
      diag="status read failed (exit $rc)"
    elif ! jq -e --argjson states "$FM_CLOUD_STATES_JSON" --arg run "$run" "$FM_CLOUD_VALID_JQ" >/dev/null 2>&1 <<<"$raw"; then
      diag='status response failed validation'
    else
      fold=$(jq -c --argjson prev "$stored" --argjson now "$now" "$FM_CLOUD_FOLD_JQ" <<<"$raw") || return 1
      accepted=$(jq -r '.accepted' <<<"$fold")
      diag=$(jq -r '.diag' <<<"$fold")
      stored=$(jq -c '.stored' <<<"$fold")
      if [ "$accepted" = true ]; then
        current=true
        mkdir -p "$(dirname "$obs")" || return 1
        tmp="$obs.tmp.$$"
        printf '%s\n' "$stored" > "$tmp" && mv -f "$tmp" "$obs" || return 1
      fi
    fi
  fi
  jq -c --argjson current "$current" --arg diag "$diag" "$FM_CLOUD_PROJECT_JQ" <<<"$stored"
}
