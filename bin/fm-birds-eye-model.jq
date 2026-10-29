# fm-birds-eye-model.jq - aggregation core for fm-birds-eye-view.v1.
#
# Pure program: bin/fm-birds-eye-model.sh runs it once per invocation and it does no I/O.
#   jq -n -f bin/fm-birds-eye-model.jq --argjson config '<json>' \
#     --slurpfile roots <roots.jsonl> --slurpfile prev <previous-or-null.json>
#
# $config  {now: <epoch>, interval: <seconds>, shell_unsupported: [{category, reason}]}
# $roots   one wrapper per root attempt: {root_id, home_path, source, parent_root_id,
#          transport_status, transport_error, raw_valid, payload}
# $prev    [previous fm-birds-eye-view.v1 object, or null]; the only carrier of
#          last-good records and history between runs.
#
# null means Not recorded or Unavailable. An age is null unless it is numeric,
# non-negative, and not in the future relative to the observer clock.

def supported_backends: ["tmux", "herdr"];

def display_of:
  if type != "string" then "Unknown"
  else ({"working": "Running", "parked": "Parked", "done": "Completed", "blocked": "Blocked",
         "paused": "Paused", "failed": "Failed", "unknown": "Unknown"} as $m | $m[.]) // "Unknown"
  end;

def iso_epoch:
  if type == "string" and test("^[0-9]{4}-[0-9]{2}-[0-9]{2}T[0-9]{2}:[0-9]{2}:[0-9]{2}Z$")
  then (try fromdateiso8601 catch null)
  else null end;

def iso_of: if type == "number" then todate else null end;
def age_of($now): if type == "number" and . <= $now then ($now - .) else null end;
def nonneg: if type == "number" and . >= 0 then . else null end;
def base: split("/") | .[-1];
def nz: if type == "string" and test("^\\s*$") then null else . end;
def norm_gen: if type == "string" and test("^[0-9]{1,15}$") then tonumber else null end;
def gen_of: (.spawn_gen // -1);

def norm_class:
  if type == "string" and (. == "captain_decision" or . == "active_child_work"
                           or . == "externally_held" or . == "unknown" or . == "no_active_work")
  then . else "unknown" end;

def rank_of:
  if . == "active_child_work" then 0
  elif . == "externally_held" then 1
  else 2 end;

# Supervisor classification, in the producer's order: any child of unknown state is unknown, then
# any open decision is captain_decision, then active, held, and no active work.
def derive_class($children):
  ([$children[] | .classification | norm_class]) as $cs
  | if ($cs | length) == 0 then "no_active_work"
    elif any($cs[]; . == "unknown") or any($children[]; .state.display == "Unknown") then "unknown"
    elif any($children[]; .decision != null) or any($cs[]; . == "captain_decision") then "captain_decision"
    else ($cs | min_by(rank_of)) end;

# Validation of one root wrapper. Identity and schema problems exclude the root's new
# records but never erase its last-good records (handled by the caller).
def validate($r; $now):
  $r.payload as $p
  | (if ($p | type) == "object" then $p.generated else null end | iso_epoch) as $gen
  | if $r.transport_status != "ok" then {status: "failed", error: ($r.transport_error // "unreadable")}
    elif $r.raw_valid != true then {status: "malformed", error: "producer output is not valid JSON"}
    elif ($p | type) != "object" then {status: "malformed", error: "producer output is not a JSON object"}
    elif $p.schema != "fm-fleet-snapshot.v1" then
      {status: "unsupported_schema", error: ("unsupported schema: " + (($p.schema // "missing") | tostring))}
    elif $p.fm_home != $r.home_path then
      {status: "malformed", error: ("home identity mismatch: producer reported fm_home " + (($p.fm_home // "missing") | tostring))}
    elif $gen == null or $gen > $now then
      {status: "malformed", error: "producer output has no valid generated time, or it is in the future"}
    elif (($p.tasks | type) != "array") or (($p.secondmate_current | type) != "object") then
      {status: "malformed", error: "producer output lacks tasks or secondmate_current"}
    elif any($p.tasks[]; (type != "object") or ((.id | type) != "string") or (.id == "")) then
      {status: "malformed", error: "task row without a task id"}
    elif (($p.tasks | map(.id) | unique | length) != ($p.tasks | length)) then
      {status: "malformed", error: "duplicate task id within home"}
    elif any($p.tasks[]; .spawn_gen != null and (.spawn_gen | norm_gen) == null) then
      {status: "malformed", error: "task row with invalid spawn_gen"}
    else {status: "ok", error: null} end;

# Retained last-good record: same content, observation relabelled cached and stale.
def retained($now):
  (.observation.collected_at | iso_epoch | age_of($now)) as $ca
  | (.observation.source_event_at | iso_epoch | age_of($now)) as $sa
  | .observation = {quality: "cached", collected_at: .observation.collected_at,
                    source_event_at: .observation.source_event_at, root_id: .observation.root_id,
                    collection_age_seconds: $ca,
                    collection_age_basis: (if $ca == null then null else "observed" end),
                    source_event_age_seconds: $sa,
                    source_event_age_basis: (if $sa == null then null else "status_event" end),
                    stale: true};

# $ctx for one owning root: {now, stale_after, coll_iso, coll_epoch}, where coll_* is the
# producer's collection time. Observation labels and ages are computed from it.
def observation_of($ctx; $root_id; $quality; $ev_at; $ev_age):
  ($ctx.coll_epoch | age_of($ctx.now)) as $ca
  | {quality: $quality, collected_at: $ctx.coll_iso, source_event_at: $ev_at, root_id: $root_id,
     collection_age_seconds: $ca,
     collection_age_basis: (if $ca == null then null else "observed" end),
     source_event_age_seconds: $ev_age,
     source_event_age_basis: (if $ev_age == null then null else "status_event" end),
     stale: ($ca == null or $ca > $ctx.stale_after)};

# One worker task row from an owning root.
def worker_entity($R; $t; $kind; $ctx):
  ($R.home_id + ":" + $t.id) as $key
  | ($t.current_state // null) as $cs
  | ($t.backlog // {}) as $bl
  | ($t.hints // {}) as $h
  | ($t.paths.status_log.last_event // null) as $le
  | ($t.execution // {}) as $ex
  | (($le.age_seconds | nonneg) as $age
     | if $age == null or $ctx.coll_epoch == null then null else ($ctx.coll_epoch - $age) end) as $ev0
  | (if $ev0 != null and $ev0 <= $ctx.now then $ev0 else null end) as $ev
  | ($ev | age_of($ctx.now)) as $ev_age
  | ($cs.state | if type == "string" then . else null end) as $raw
  | (if $raw == null then null else ($raw | display_of) end) as $display
  | ($bl.captain_actionable == true) as $captain
  | (($bl.hold_age_days | nonneg) as $d
     | if $captain and $d != null then ($d * 86400) else null end) as $hold_age
  | ([ (if $captain then
          {label: "Needs you", recipient: "captain", resolver: "captain", key: $t.id,
           age_seconds: $hold_age,
           age_basis: (if $hold_age == null then null else "hold_age_days" end),
           summary: ($bl.hold_reason // $bl.title // null),
           ref: ($key + "#" + $t.id)}
        else empty end),
       (($h.open_decisions // []) | to_entries[] | .key as $i | (.value | select(type == "object")) as $od
        | {label: "Decision pending", recipient: null, resolver: null,
           key: ($od.key // null),
           age_seconds: (if $i == 0 and $le.state == "needs-decision" then $ev_age else null end),
           age_basis: (if $i == 0 and $le.state == "needs-decision" and $ev_age != null then "status_event" else null end),
           summary: ($od.summary // null),
           ref: ($key + "#" + ($od.key // ""))}),
       (if $h.pending_decision == true and (($h.open_decisions // []) | length) == 0 and ($captain | not)
        then {label: "Decision pending", recipient: null, resolver: null, key: null,
              age_seconds: null, age_basis: null, summary: null, ref: ($key + "#")}
        else empty end) ]) as $decisions
  | ($decisions[0] | if . == null then null else del(.ref) end) as $decision
  | (if $h.blocked_event == true then
       {reason: ($bl.blocked_reason // $le.note // $h.last_event_text // null),
        age_seconds: (if $le.state == "blocked" then $ev_age else null end),
        age_basis: (if $le.state == "blocked" and $ev_age != null then "status_event" else null end)}
     else null end) as $blocker
  | (if ($display == "Blocked" and $le.state == "blocked")
        or ($display == "Parked" and $le.state == "needs-decision")
     then $ev_age else null end) as $state_age
  | ($t.backend // "") as $backend
  | (if $cs == null then "unavailable"
     elif (supported_backends | any(. == $backend) | not) then "unknown"
     elif $cs.freshness == "fresh" then "fresh"
     elif $cs.freshness == "cached" then "cached"
     else "unknown" end) as $quality
  | (if any($decisions[]; .label == "Needs you") then "captain_decision"
     elif $display == "Running" then "active_child_work"
     elif $display == "Parked" or $display == "Paused" or $display == "Blocked" then "externally_held"
     elif $display == "Completed" or $display == "Failed" then "no_active_work"
     else "unknown" end) as $class
  | ($ev | iso_of) as $ev_at
  | ($cs.observed_at | iso_epoch) as $oe
  | (if $oe != null and $oe <= $ctx.now then $cs.observed_at else null end) as $obs_at
  | {key: $key, kind: $kind, task_id: $t.id, home_id: $R.home_id,
     name: ($bl.title // $t.id), role: ($t.kind // null), project: ($t.project | nz),
     parent_key: $R.home_id, child_keys: [], contextual_in: [],
     spawn_gen: ($t.spawn_gen | norm_gen),
     state: {raw: $raw, display: $display, source: ($cs.source // null), detail: ($cs.detail // null),
             wait_reason: (if $captain then ($bl.hold_reason // null)
                           elif $blocker != null then $blocker.reason else null end),
             observed_at: $obs_at},
     classification: $class,
     timing: {elapsed_seconds: null, elapsed_basis: null, state_seconds: $state_age,
              state_basis: (if $state_age == null then null else "status_event" end),
              observed_state: null, observed_since: null, observed_transition: null},
     observation: observation_of($ctx; $R.root_id; $quality; $ev_at; $ev_age),
     decision: $decision,
     decisions: $decisions,
     blocker: $blocker,
     execution: {harness: ($t.harness | nz), requested_model: ($ex.requested_model // null),
                 requested_effort: ($ex.requested_effort // null), observed_model: null,
                 observed_effort: null, playbook: null, workflow_stage: null},
     artifacts: {worktree: (if $t.paths.worktree.present == true then $t.paths.worktree.path else null end),
                 branch: ($t.branch // null), pr_url: ($t.pr.url // null),
                 report_path: (if $t.paths.report.present == true then $t.paths.report.path else null end),
                 output_available: (if ($t.endpoint.exists | type) == "boolean" then $t.endpoint.exists else null end)},
     prerequisites: [($bl.blocked_by_ids // [])[] | select(type == "string")
                     | {key: ($R.home_id + ":" + .), source: "backlog"}],
     latest_activity: {text: ($le.note // null), reported_by_agent: null, at: $ev_at}};

# A secondmate row whose home is not read this run (no path recorded, remote, or its root failed)
# stays visible as unknown, with its own recorded state and the reason the home is unread.
def unread_sm_entity($R; $t; $ctx; $detail):
  worker_entity($R; $t; "secondmate"; $ctx)
  | .classification = "unknown"
  | .state.detail = $detail;

# A home entity (supervisor or secondmate). $sm carries the parent's row and record, when listed.
def home_entity($key; $name; $root_id; $sm; $kind; $parent; $ctx):
  ($sm.row // null) as $row
  | ($sm.record // null) as $rec
  | ((($row.secondmate_projects // []) | map(select(type == "string" and . != "")) | sort)) as $projs
  | ($row.current_state // null) as $rcs
  | ($rcs.state | if type == "string" then . else null end) as $raw
  | (($rcs.observed_at | iso_epoch) as $oe
     | if $oe != null and $oe <= $ctx.now then $rcs.observed_at else null end) as $row_obs
  | (if $kind != "secondmate" then "fresh"
     elif $rec == null then "unavailable"
     else ($rec.freshness.status
           | if . == "fresh" or . == "cached" or . == "historical-event" or . == "unknown" then . else "unknown" end)
     end) as $quality
  | (if $kind == "secondmate" and ($rec.decisions_open | type) == "array"
     then [$rec.decisions_open[] | select(type == "object")] else [] end) as $dec_all
  | ([ $dec_all[]
       | (.verb == "captain-hold" and .source == "backlog") as $hold
       | {label: (if $hold then "Needs you" else "Decision pending" end),
          recipient: (if $hold then "captain" else null end),
          resolver: (if $hold then "captain" else null end),
          key: (.key // null), age_seconds: null, age_basis: null, summary: (.summary // null),
          ref: ($key + ":" + ((.id // "") | tostring) + "#" + ((.key // "") | tostring))} ]
     | (map(select(.label == "Needs you")) + map(select(.label != "Needs you")))) as $decisions
  | {key: $key, kind: $kind, task_id: ($row.id // null), home_id: $key, name: $name,
     role: (if $kind == "secondmate" then ($row.kind // "secondmate") else null end),
     project: ($projs[0] // null), parent_key: $parent,
     child_keys: [],
     contextual_in: ((($projs[1:]) | map("project:" + .)) + (($sm.extra_roots // []) | map("root:" + .))),
     spawn_gen: ($row.spawn_gen | norm_gen),
     state: {raw: $raw, display: (if $raw == null then null else ($raw | display_of) end),
             source: (if $kind == "secondmate" then ($rcs.source // "secondmate_record") else "children" end),
             detail: (if $kind != "secondmate" then null
                      elif $rec == null then "parent secondmate record unavailable"
                      else ($rec.current.reason // $rcs.detail // null) end),
             wait_reason: null, observed_at: (if $row != null then $row_obs else $ctx.coll_iso end)},
     classification: (if $kind != "secondmate" then "__derive__"
                      elif $rec == null then "unknown"
                      else ($rec.current.state | norm_class) end),
     timing: {elapsed_seconds: null, elapsed_basis: null, state_seconds: null, state_basis: null,
              observed_state: null, observed_since: null, observed_transition: null},
     observation: observation_of($ctx; $root_id; $quality; null; null),
     decision: ($decisions[0] | if . == null then null else del(.ref) end),
     decisions: $decisions,
     blocker: null,
     execution: {harness: ($row.harness | nz), requested_model: ($row.execution.requested_model // null),
                 requested_effort: ($row.execution.requested_effort // null), observed_model: null,
                 observed_effort: null, playbook: null, workflow_stage: null},
     artifacts: {worktree: (if $row.paths.worktree.present == true then $row.paths.worktree.path else null end),
                 branch: ($row.branch // null), pr_url: ($row.pr.url // null),
                 report_path: (if $row.paths.report.present == true then $row.paths.report.path else null end),
                 output_available: (if ($row.endpoint.exists | type) == "boolean" then $row.endpoint.exists else null end)},
     prerequisites: [],
     latest_activity: {text: ($row.paths.status_log.last_event.note // null), reported_by_agent: null, at: null}};

# Previously known descendants whose parent root could not be read, to a fixed point so a chain
# of any depth resolves.
def unreached($prev_roots; $now; $now_iso):
  . as $cur
  | ([ $prev_roots[] as $pr
       | select(([ $cur[] | .root_id ] | index($pr.root_id)) == null)
       | select($pr.parent_root_id != null)
       | select(any($cur[]; .root_id == $pr.parent_root_id and .status != "ok"))
       | ($pr.last_good_at // null) as $lg
       | ($lg | iso_epoch | age_of($now)) as $lga
       | {root_id: $pr.root_id, home_path: $pr.home_path, source: $pr.source,
          status: "failed",
          error: ("not reached: parent root " + $pr.parent_root_id + " could not be read"),
          collected_at: $now_iso, last_good_at: $lg, stale: true,
          parent_root_id: $pr.parent_root_id, last_good_age_seconds: $lga,
          age_basis: "observed"} ]
     | unique_by(.root_id)) as $add
  | if ($add | length) == 0 then $cur else ($cur + $add | unreached($prev_roots; $now; $now_iso)) end;

# Grow a set of protected keys with the parent of every protected node, to a fixed point.
def close_ancestry($nodes):
  . as $P
  | ([ $nodes[] | select(.parent_key != null and ($P[.key] == true)) | {(.parent_key): true} ]
     | add // {}) as $new
  | ($P + $new) as $n
  | if ($n | length) == ($P | length) then $P else ($n | close_ancestry($nodes)) end;

# The state an observer first saw: the raw task state, or a home's classification.
def observed_token:
  .state.raw // (if .kind == "worker" then null else .classification end);

# Observer-measured time in state, carried through the previous document. A prior record of
# the same incarnation in the same state keeps its first-observed time; a different state is
# a first observed change, stamped with this collection; no usable prior starts the
# observation now. An authoritative status-event age, when present, is left in charge of
# timing.state_seconds.
def observe_timing($P; $now; $now_iso):
  . as $e
  | ($e | observed_token) as $t
  | (if $P != null and (($P | gen_of) == ($e | gen_of)) then ($P.timing // {}) else {} end) as $pt
  | ($pt.observed_state // null) as $ps
  | (($pt.observed_since // null) | iso_epoch | age_of($now)) as $pa
  | (if $t == null then {state: null, since: null, transition: null}
     elif $ps == $t and $pa != null
       then {state: $t, since: $pt.observed_since, transition: ($pt.observed_transition // null)}
     elif $ps != null and $ps != $t
       then {state: $t, since: $now_iso,
             transition: {from: $ps, to: $t, first_observed_at: $now_iso, basis: "first_observed_change"}}
     else {state: $t, since: $now_iso, transition: null} end) as $o
  | .timing.observed_state = $o.state
  | .timing.observed_since = $o.since
  | .timing.observed_transition = $o.transition
  | if .timing.state_seconds == null and $o.since != null
    then .timing.state_seconds = ($now - ($o.since | iso_epoch)) | .timing.state_basis = "observed"
    else . end;

# Does walking up the accepted parent links from $from reach $target?
def climbs($acc; $from; $target):
  if $from == null then false
  elif $from == $target then true
  else climbs($acc; $acc[$from]; $target) end;

def summary_of($e):
  {key: $e.key, generation: ($e.spawn_gen // null),
   last_state: ($e.state.display // $e.state.raw // (if $e.classification == "__derive__" then null else $e.classification end)),
   last_seen_at: ($e.observation.collected_at // null), historical: true,
   parent_key: ($e.parent_key // null)};

def sort_summaries:
  sort_by([(.last_seen_at != null), (.last_seen_at // ""), .key, (.generation // -1)]);

# ---------------------------------------------------------------------------
($config.now) as $now
| ($config.interval) as $interval
| ([30, 3 * $interval] | max) as $stale_after
| ($now | todate) as $now_iso
| ($prev[0]) as $P
| ($P.roots // []) as $prev_roots
| ($P.entities // []) as $prev_entities
| ($P.history.summaries // []) as $prev_summaries
| ($config.shell_unsupported // []) as $shell_unsupported
| (reduce $prev_entities[] as $p ({}; if has($p.key) then . else . + {($p.key): $p} end)) as $pm
| (reduce $prev_roots[] as $pr ({}; if has($pr.root_id) then . else . + {($pr.root_id): $pr} end)) as $prmap

# Roots: validated wrappers.
# A readable snapshot older than the last good collection is rejected as stale output, so an
# out-of-order producer result cannot roll a root's records back.
| ([ $roots | to_entries[] as $e | $e.value as $r | validate($r; $now) as $v0
     | ((($prmap[$r.root_id].last_good_at) // null) | iso_epoch) as $prev_gen
     | (if $v0.status == "ok" and $prev_gen != null and (($r.payload.generated | iso_epoch) < $prev_gen)
        then {status: "failed", error: "stale output rejected: producer generated time is older than the last good collection"}
        else $v0 end) as $v
     | {root_id: $r.root_id, home_path: $r.home_path, home_id: ("home:" + $r.home_path),
        source: $r.source, parent_root_id: ($r.parent_root_id // null),
        status: $v.status, error: $v.error,
        payload: (if $v.status == "ok" then $r.payload else null end)} ]) as $vr
| ($vr | map(select(.status == "ok"))) as $OK
# last_good_at is the producer's own collection time for a successful read, so a
# snapshot older than stale_after is stale even when it parsed cleanly.
| ([ $vr[] as $x
     | ($prmap[$x.root_id]) as $pr
     | (if $x.status == "ok" then $x.payload.generated else ($pr.last_good_at // null) end) as $lg_at
     | ($lg_at | iso_epoch | age_of($now)) as $lg_age
     | {root_id: $x.root_id, home_path: $x.home_path, source: $x.source, status: $x.status,
        error: $x.error, collected_at: $now_iso, last_good_at: $lg_at,
        stale: ($x.status != "ok" or $lg_age == null or $lg_age > $stale_after),
        parent_root_id: $x.parent_root_id, last_good_age_seconds: $lg_age,
        age_basis: "observed"} ]) as $roots_base
# A previously known descendant whose parent could not be read is not reached this run:
# report it failed, carrying its last-good records.
| ($roots_base | unreached($prev_roots; $now; $now_iso)) as $roots_out
| (reduce $roots_out[] as $ro ({}; . + {($ro.root_id): $ro})) as $romap

# The producer reports a descendant home as written; refs maps each such path to the canonical
# root it was read as, so a home listed through a symlinked path still meets its own root.
| (reduce ($roots[].refs[]?) as $r ({}; . + {($r.raw): $r.canonical})) as $refmap

# Secondmate listings: a parent's row and/or record that names a home.
| ([ $OK[] as $R
     | ( ($R.payload.tasks[]
          | select(.kind == "secondmate" and .remote == null)
          | select(((.paths.home.path? // null) | type) == "string")
          | {key: ("home:" + ($refmap[.paths.home.path] // .paths.home.path)), parent: {root_id: $R.root_id, home_id: $R.home_id},
             row: ., record: null}),
         ($R.payload.secondmate_current.records[]?
          | select(.remote != true)
          | {key: (if (.home | type) == "string" then ("home:" + ($refmap[.home] // .home)) else null end),
             parent: {root_id: $R.root_id, home_id: $R.home_id}, row: null, record: .}) ) ]
   | map(select(.key != null))) as $sm_list
| (reduce ($sm_list | group_by(.key))[] as $g ({};
     ($g[0].key) as $k
     | ([$g[] | select(.row != null)][0]) as $rl
     | ([$g[] | select(.record != null)][0]) as $cl
     | (($rl // $cl) | .parent) as $pp
     | . + {($k): {key: $k, parent_root_id: $pp.root_id, parent_home_id: $pp.home_id,
                   row: ($rl.row // null), record: ($cl.record // null),
                   extra_roots: ([$g[] | .parent.root_id] | unique | map(select(. != $pp.root_id)))}})) as $sm_all

# A home that lists one of its own ancestors is a cycle, not a delegation. Links are accepted in
# discovery order and one that would close a cycle is refused and counted.
| ($OK | map(.root_id)) as $okids
| (reduce ($sm_all | to_entries | map(.value) | sort_by([(.parent_root_id as $p | $okids | index($p)), .key]))[] as $e
     ({acc: {}, refused: 0};
      if climbs(.acc; $e.parent_home_id; $e.key) then .refused += 1
      else .acc += {($e.key): $e.parent_home_id} end)) as $edges
| ($sm_all | with_entries(select($edges.acc[.key] != null))) as $sm_map
| ($OK | map(.home_id)) as $okh

# Fresh entities from ok roots: one home entity per root, plus its task rows.
| ([ $OK[] as $R
     | ({now: $now, stale_after: $stale_after, coll_iso: $R.payload.generated,
         coll_epoch: ($R.payload.generated | iso_epoch)}) as $ctx
     | ($sm_map[$R.home_id]) as $sm
     | ($pm[$R.home_id]) as $pe
     | (if $sm != null then "secondmate"
        elif ($pe.kind // "") == "secondmate" then "secondmate"
        else "supervisor" end) as $kind
     | (if $sm != null then $sm.parent_home_id
        elif $kind == "secondmate" then ($pe.parent_key // null)
        else null end) as $parent
     | home_entity($R.home_id; ($R.home_path | base); $R.root_id; $sm; $kind; $parent; $ctx),
       ( $R.payload.tasks[] as $t
         | if $t.kind == "secondmate" then
             (($t.paths.home.path? // null) as $hp0
              | (if ($hp0 | type) == "string" then ($refmap[$hp0] // $hp0) else $hp0 end) as $hp
              | if ($hp | type) != "string" then unread_sm_entity($R; $t; $ctx; "secondmate home path not recorded")
                elif $t.remote != null then unread_sm_entity($R; $t; $ctx; "remote secondmate home is not observed")
                elif ($okh | index("home:" + $hp)) != null then empty
                elif ($pm["home:" + $hp] != null
                      and (($romap[($pm["home:" + $hp].observation.root_id // "")] // {status: "ok"}).status) != "ok")
                  then empty
                else unread_sm_entity($R; $t; $ctx;
                       "secondmate home not read: " + ($romap[$hp].error // "not read this run")) end)
           else worker_entity($R; $t; "worker"; $ctx) end )
   ]) as $fresh_own

# A secondmate whose home is not read keeps its parent's row as an unknown entity; no record is
# ever built from unread output.
| $fresh_own as $fresh
| (reduce $fresh[] as $f ({}; . + {($f.key): $f})) as $fmap

# Per-key selection between the fresh record and the prior record.
| ([ $fresh[].key ] + [ $prev_entities[].key ] | unique) as $allkeys
| ([ $allkeys[] as $k
     | ($fmap[$k]) as $F
     | ($pm[$k]) as $P
     | (if $P != null then ($romap[($P.observation.root_id // "")]) else null end) as $PR
     | (if $F != null and $P != null then
          (if ($P | gen_of) > ($F | gen_of) then {w: ($P | retained($now)), l: [$F]}
           elif ($P | gen_of) == ($F | gen_of) then {w: $F, l: []}
           else {w: $F, l: [$P]} end)
        elif $F != null then {w: $F, l: []}
        elif $P != null and $PR != null and $PR.status != "ok" then {w: ($P | retained($now)), l: []}
        elif $P != null then {w: null, l: [$P]}
        else {w: null, l: []} end) ]) as $picks
| ([ $picks[] | select(.w != null) | .w ]) as $E0
| ([ $picks[] | .l[] ]) as $losers

# Pass 2: supervisor classification and child keys.
| ($E0 | map(. as $e
     | (if $e.kind == "supervisor" and $e.classification == "__derive__"
        then ($E0 | map(select(.parent_key == $e.key))) as $ch
             | $e | .classification = derive_class($ch)
        else $e end)
     | (if ($fmap[$e.key] != null) and (($fmap[$e.key] | gen_of) == ($e | gen_of))
        then observe_timing($pm[$e.key]; $now; $now_iso)
        else . end)
     | .child_keys = ([ $E0[] | select(.parent_key == $e.key) | .key ] | sort))
   | sort_by(.key)) as $E1

# History: losers and disappeared records become historical summaries.
| ([ $losers[] | summary_of(.) ]) as $loser_sums
| ([ $prev_summaries[] | select(type == "object" and has("key")) ]) as $prev_sums
| ([ ($loser_sums + $prev_sums)[]
     | select(. as $s | any($E1[]; .key == $s.key and gen_of == (($s.generation // -1))) | not) ]
   | unique_by([.key, (.generation // -1)])) as $S
| ([ $E1[] | select(.state.display != "Completed")
     | (.parent_key, (.prerequisites[]?.key)) | select(. != null) ]
   | reduce .[] as $k ({}; . + {($k): true})) as $seed
| ($seed | close_ancestry($E1 + $S)) as $prot
| ($S | map(select(($prot[.key] // false) == true)) | sort_summaries) as $S_prot
| ($S | map(select(($prot[.key] // false) != true)) | sort_summaries) as $S_free
| ($S_free | length) as $free_n
| ([$free_n - 1000, 0] | max) as $evicted
| ($S_free[$evicted:]) as $S_kept
| (($S_prot + $S_kept) | sort_summaries) as $summaries
| {limit: 1000, retained: ($summaries | length), evicted: $evicted,
   limit_reached: ($free_n >= 1000),
   restart_disclosure: "observer history is held for one run only; it does not survive a restart and carries forward only through a caller-supplied previous output",
   summaries: $summaries} as $history

# Unsupported categories: never counted, only listed.
| ([ $OK[] as $R | ($R.payload.tasks[]?
                   | (if (.backend | type) == "string" and .backend != "" then .backend else "unrecorded" end))
   ] | unique) as $backends
| ([ $backends[] as $b
     | select(any(supported_backends[]; . == $b) | not)
     | {category: ("backend:" + $b),
        reason: (if $b == "unrecorded" then "task rows record no backend; their live state is not observed"
                 else "backend is not in the observer's supported set; its live state is not observed" end)} ]) as $unsup_backends
| ([ $OK[] | (.payload.secondmate_current.truncated // 0) as $tr
     | select(($tr | type) == "number" and $tr > 0)
     | {category: "secondmate_records_truncated",
        reason: ("producer truncated " + ($tr | tostring) + " secondmate records in " + .home_path)} ]) as $unsup_trunc
| ([{category: "native_subagents",
     reason: "native subagents are not observed by this aggregator; no count is reported"}]) as $native
| ([ $OK[] as $R
     | ( ($R.payload.tasks[] | select(.kind == "secondmate" and .remote != null) | .id),
         ($R.payload.secondmate_current.records[]? | select(.remote == true) | .id) )
     | ($R.root_id + ":" + (. | tostring)) ] | unique | length) as $remote_n
| (if $remote_n > 0
   then [{category: "remote_home",
          reason: (($remote_n | tostring) + " remote secondmate home(s) are not read: only local homes are observed")}]
   else [] end) as $unsup_remote
| (if $edges.refused > 0
   then [{category: "ownership_cycle",
          reason: (($edges.refused | tostring) + " home reference(s) name an ancestor home as a delegate and are not treated as delegation")}]
   else [] end) as $unsup_cycle
| (($shell_unsupported + $native + $unsup_backends + $unsup_trunc + $unsup_cycle + $unsup_remote) | unique) as $unsupported

# Attention: decision > failure > blocker > observation; oldest age first, then key.
| ([ ([ $E1[] as $e
        | ($e.decisions // (if $e.decision != null then [$e.decision] else [] end))[]
        | . + {owner: $e.key, rank: (if $e.kind == "worker" then 0 else 1 end), project: $e.project,
               ref: (.ref // $e.key)} ]
      | group_by(.ref)
      | map(sort_by([.rank, (if .label == "Needs you" then 0 else 1 end), .owner]) | .[0])
      | .[]
      | {key: .owner, class: "decision", label: .label,
         reason: (.summary // .label), resolver: .resolver,
         age_seconds: .age_seconds, project: .project}),
     ($E1[] | select(.state.display == "Failed")
       | {key: .key, class: "failure", label: null, reason: (.state.detail // "work failed"),
          resolver: null, age_seconds: null, project: .project}),
     ($E1[] | select(.blocker != null)
       | {key: .key, class: "blocker", label: null, reason: (.blocker.reason // "explicit blocker"),
          resolver: null, age_seconds: .blocker.age_seconds, project: .project}),
     ($roots_out[] | select(.stale == true)
       | {key: ("root:" + .root_id), class: "observation", label: null,
          reason: (.error // "last-good records are stale"), resolver: null,
          age_seconds: .last_good_age_seconds, project: null})
     ]
   | sort_by([({"decision": 0, "failure": 1, "blocker": 2, "observation": 3}[.class]),
              (if .age_seconds == null then 1 else 0 end),
              (-(.age_seconds // 0)), .key])
   | to_entries | map(.value + {priority: (.key + 1)})) as $attention

# Counts: unique entities; contextual copies counted separately.
| {workers_by_state: ([ $E1[] | select(.kind == "worker") | (.state.display // "Not recorded") ]
                       | group_by(.) | map({key: .[0], value: length}) | from_entries),
   supervisors_by_classification: ([ $E1[] | select(.kind != "worker")
                                     | ((.classification // "null") | tostring) ]
                                   | group_by(.) | map({key: .[0], value: length}) | from_entries),
   unique_entities: ($E1 | length),
   contextual_copies: ([ $E1[] | (.contextual_in | length) ] | add // 0),
   by_project: ([ $E1[] | (.project // "Unassigned") ]
                | group_by(.) | map({key: .[0], value: length}) | from_entries)} as $counts

| {schema: "fm-birds-eye-view.v1",
   generated_at: $now_iso,
   generated_epoch: $now,
   refresh: {interval_seconds: $interval, stale_after_seconds: $stale_after},
   roots: $roots_out,
   unsupported: $unsupported,
   entities: $E1,
   attention: $attention,
   counts: $counts,
   history: $history}
