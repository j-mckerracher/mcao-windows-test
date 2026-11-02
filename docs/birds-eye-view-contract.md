# Bird's-eye view contract (fm-birds-eye-view.v1)

`bin/fm-birds-eye-model.sh` prints one `fm-birds-eye-view.v1` JSON document on stdout.
It is observer-only: it reads homes through their own producers and never changes task state.
Its aggregation rules live in `bin/fm-birds-eye-model.jq`, and its behavior is pinned by `tests/fm-birds-eye-model.test.sh`.

## Top-level document

`schema` is always `fm-birds-eye-view.v1`.
`generated_at` is the observer clock as UTC ISO-8601, and `generated_epoch` is the same instant in epoch seconds.
Top-level keys are `schema`, `generated_at`, `generated_epoch`, `refresh`, `roots`, `entities`, `attention`, `counts`, `history`, and `unsupported`.

## Running it

The configured root is always read first, which is `FM_HOME` when set and otherwise this checkout.
Each `--root <home-dir>` adds an explicit root after it, so repeat it to read several more homes.
A `--root` that names a home already read, whether the configured home or another explicit root, is read once and keeps its first `source`.
`--interval <seconds>` sets the refresh interval, which defaults to 10 and must be at least 5.
`--now <epoch>` pins the observer clock and defaults to the current time.
`--previous <file>` is additive: it names a prior `fm-birds-eye-view.v1` document whose last-good records and history carry forward.
`--fixture-dir <dir>` is test-only and replays recorded producer snapshots instead of running producers.
Exit status is 0 when a document is rendered, 2 on a usage error, and 1 when the aggregation cannot run.
A usage error includes an interval below 5 seconds, which is refused and never clamped.

## Roots

Each root is keyed by its canonical path (`cd <dir> && pwd -P`), which is its `root_id`.
Under `--fixture-dir` the path is kept exactly as the manifest writes it.
Descendant homes are followed only through the producer's own relationships: local secondmate task rows with a home path, and local `secondmate_current.records[].home`.
A home path the producer reports through a symlink is matched to the canonical root it was read as.
Nothing is found by scanning the filesystem.
Each canonical home is read once per run, sequentially, so an overlapping home is never read twice.
A run reads at most 64 roots and follows descendants to any depth.
A home reached again through a second path or a cycle is read once.
Roots past the 64-root bound are listed in `unsupported[]` with a count and are never silently dropped.
Each root is read with the producer `fm-fleet-snapshot.sh --json` under FM_HOME set to that root and the observer clock pinned, bounded to 30 seconds.
The producer refreshes its own `state/secondmate-summary-cache` copies; those writes are the only side effect of this aggregator.

## Root status

`roots[]` reports every read home with `root_id`, `home_path`, `source`, `parent_root_id`, `status`, `error`, `stale`, `collected_at`, `last_good_at`, `last_good_age_seconds`, and `age_basis`.
`source` is `configured`, `explicit`, or `descendant`.
`status` is `ok`, `failed`, `malformed`, or `unsupported_schema`.
`collected_at` is the observer time of the attempt, and `last_good_at` is the producer's own `generated` time for the last good collection.
A root whose producer exits nonzero, times out, or is missing reports `failed` with its own `error`.
Malformed output, an unsupported schema, or an identity mismatch reports `malformed` or `unsupported_schema` with its own `error`.
A root that cannot be read keeps its last-good records from `--previous`, labelled `cached` and marked `stale`, while other roots render normally.
A root that recovers clears its `error` without any task transition.
A descendant whose parent root could not be read reports `failed` with `not reached: parent root <id> could not be read` and keeps its last-good records the same way, at any depth.
Output whose `generated` time is older than the root's last good collection is rejected as `failed` with `stale output rejected`, and the last-good records are kept.

## Staleness

A root is stale once its last good collection is older than `max(30, 3 x interval)` seconds.
`refresh.interval_seconds` and `refresh.stale_after_seconds` report the values in force.
A stale root raises an `observation` attention item keyed `root:<root_id>`.

## Entities

`entities[]` holds one record per canonical identity, keyed `home:<root_id>` for a supervisor home and `home:<root_id>:<task-id>` for a worker.
A secondmate is one entity owned by the root that read it, and every other root that lists it appears in `contextual_in`.
A secondmate whose home is not read this run keeps its parent's task row as an entity keyed `home:<parent root_id>:<task-id>` with classification `unknown`, the row's own recorded state, and a `state.detail` naming why the home is unread.
That row is replaced by the child's home entity once the home is read, and it is not duplicated while a cached home entity stands in for a root that failed after a good read.
`contextual_in` entries are `project:<name>` for another project a supervisor is registered under and `root:<root_id>` for another root that lists the same secondmate.
`counts.contextual_copies` counts those contextual copies and is not added to the unique total.
Two entities with the same display name in different homes stay distinct and are never merged.
The ownership tree is acyclic: a home that names one of its own ancestors as a delegate keeps its place and the link is refused, counted under `ownership_cycle`.

Each entity carries `key`, `kind`, `task_id`, `home_id`, `name`, `role`, `project`, `parent_key`, `child_keys`, `contextual_in`, `spawn_gen`, `artifacts`, `prerequisites`, `latest_activity`, `classification`, `state`, `observation`, `timing`, `execution`, `decision`, `decisions`, and `blocker`.
`key` is `home:<root_id>` for a home and `home:<root_id>:<task-id>` for a task, and `home_id` is the owning home's `home:<root_id>`.
`kind` is `worker`, `secondmate`, or `supervisor`.
`task_id` is the id of the task row that backs the entity, and `null` when no row does.
A secondmate home's `task_id` is the id of its parent's row that names it, so its `key` does not end in that id.
`name` is the backlog title when one is recorded and otherwise the task id, and a home's name is the base name of its directory.
`role` is the task kind the producer recorded, `secondmate` when a secondmate's row records none, and `null` for a supervisor or when no kind is recorded.
`parent_key` is the owning home's key for a task and the parent home's key for a home, and it is `null` for the root supervisor.
`child_keys` lists the keys of the entities whose `parent_key` is this entity, and it is `[]` for a task.
`prerequisites` lists the tasks that a task's backlog row names as blocking it, each as `{key, source}`, with `source` currently always `backlog`, and it is `[]` for a home.
`artifacts` holds `worktree` and `report_path`, which are set only when the producer reports them present, plus `branch` and `pr_url`, beside `output_available`.
`latest_activity.text` is the latest status note the producer recorded, `null` when none is recorded, and `latest_activity.at` is that note's event time for a task and `null` for a home.
`latest_activity.reported_by_agent` is always `null`, because no source names the reporting agent.
`project` is `null` when the producer records no project, including when it records an empty string, and `execution.harness` follows the same rule.
`classification` is `captain_decision`, `active_child_work`, `externally_held`, `no_active_work`, or `unknown`.
A secondmate's classification is its record's `current.state`, its `state.detail` is the record's `current.reason`, and its `state.raw`, `state.display`, and `state.source` are the parent row's recorded task state.
Its decision comes from the record's `decisions_open`, where an item with verb `captain-hold` and `source: backlog`, which is how the producer reports a captain-actionable hold, reads `Needs you` and any other item, including an ordinary `source: backlog` item, reads `Decision pending`.
`spawn_gen` is the integer the producer's digit string spells, or `null`, and any other form makes the root `malformed`.
A worker in `Parked` with a captain-actionable hold is `captain_decision`.
A worker in `Running` is `active_child_work`.
A worker in `Paused`, `Parked`, or `Blocked` without a captain decision is `externally_held`.
A worker in `Completed` or `Failed` is `no_active_work`.
Anything else, including an unrecognised raw state, is `unknown` and keeps its raw value.
A supervisor home read by this aggregator takes the producer's order over its children: `unknown` when any child's state is unknown, then `captain_decision` when any child has an open decision of either label, then `active_child_work`, `externally_held`, and `no_active_work`.
A supervisor with no children is `no_active_work`.

## State and observation quality

`state.raw` is the producer's value, and `state.display` maps `working` to `Running`, `parked` to `Parked`, `done` to `Completed`, `blocked` to `Blocked`, `paused` to `Paused`, `failed` to `Failed`, and everything else to `Unknown`.
A missing state is `null` and counts under `Not recorded`, never under `Unknown`.
`observation.quality` is `fresh`, `cached`, `historical-event`, `unknown`, or `unavailable`.
`fresh` means the record came from a live collection in this run.
`cached` means it came from `--previous` after its root could not be read, or the source itself reported a cached copy.
`unknown` means the backend is not one of `tmux` or `herdr`, or the backend is not recorded, so the state is shown but not trusted as fresh.
`historical-event` means the source reported only a past status event.
A secondmate takes `fresh`, `cached`, `historical-event`, or `unknown` from its record's `freshness.status`, with any other value read as `unknown`, and `unavailable` when its parent lists no record.
`unavailable` means the producer recorded no state for the entity.
A fresh unknown state stays `unknown` and is never upgraded.
`artifacts.output_available` is the producer's endpoint-presence read, and it is `null` when the producer recorded no boolean.
A tmux endpoint counts as present only when its exact recorded window name is in the exact session's window list, so a missing window never reads a prefix neighbor and a dotted recorded target never reads a neighbor window.

## Ages

Every age is either `null` or a non-negative integer number of seconds, and an age is never negative.
An age is `null` when its timestamp is missing, invalid, or in the future relative to the observer clock.
`observation.collection_age_seconds` is the observer clock minus the producer's collection time, with basis `observed`.
`observation.source_event_age_seconds` is the observer clock minus the time of the latest status event, with basis `status_event`.
`decision.age_seconds` is taken from the hold's age in days with basis `hold_age_days`, or from its status event with basis `status_event`, and is otherwise `null`.
`blocker.age_seconds` is taken from its status event with basis `status_event`, and is otherwise `null`.
`timing.state_seconds` is the time since the latest status event, with basis `status_event`, when that event matches the displayed state.
Without one it is the observer's own duration in the state, with basis `observed`, and it is `null` when the entity has no recorded state.
An observed duration is a lower bound, because it starts at the observer's first sight of the state and never claims the state began then.
`timing.observed_state` is the state the observer last saw, which is the raw task state or, for a home, its classification.
`timing.observed_since` is the observer time of the first collection that saw `observed_state` for this spawn generation, and it is `null` without a recorded state.
`timing.observed_transition` is `{from, to, first_observed_at, basis}` with basis `first_observed_change` when the observer saw `observed_state` change between two collections of the same spawn generation, and is otherwise `null`.
A first observed change is when the observer noticed the change, never an authoritative transition time.
The three observed fields carry only through `--previous`.
A restart without that file, a new spawn generation, a different state, or an unusable `observed_since` (missing, invalid, or in the future) starts the observation again at the current collection and records no transition across the gap.
A `cached` record keeps its observed fields unchanged.
`timing.elapsed_seconds` is always `null`, because no spawn timestamp is recorded.
A `cached` record keeps the age from the collection that produced it, so its age keeps growing across failed runs.

## Decisions and attention

`decision.label` is `Needs you` only for a captain-actionable hold: a worker whose backlog row has `captain_actionable` true, or a secondmate record decision with verb `captain-hold` and `source: backlog`.
The `recipient` and `resolver` of a `Needs you` decision are `captain`.
`decision` is the entity's first decision, with `Needs you` first, or `null`, and `decisions` lists every distinct outstanding decision of the entity, each with the fields of `decision` plus a `ref` that names the hold or decision by owning home, task id, and key.
A home's `decisions` come from its record's `decisions_open`, so a hold that its own task row also reports shares one `ref` and is listed on both entities.
Every other open decision, including a worker's status-log decision and any verb or source the producer does not define, is `Decision pending`, with `recipient` and `resolver` set to `null`.
`attention[]` lists items to look at, each with `class`, `label`, `priority`, `key`, `project`, `reason`, `resolver`, and `age_seconds`.
`class` is `decision`, `failure`, `blocker`, or `observation`, and `priority` is the item's 1-based position in `attention[]`.
Decisions come first, then failures, then blockers, then observations, and within a class items are ordered oldest age first, with null ages last, then by key.
Each decision `ref` raises one `decision` item, owned by the task when the task row and the home both report it, so a hold seen through several paths is listed once and decisions with different refs are all listed.
A parent waiting on healthy children is routine and raises no attention item.

## Counts

`counts.unique_entities` counts canonical identities once.
`counts.workers_by_state` counts workers by `state.display`, with `Not recorded` for missing states.
`counts.supervisors_by_classification` counts supervisors by `classification`.
`counts.by_project` groups entities by project, and entities with no project count as `Unassigned`.
`counts.contextual_copies` counts the `contextual_in` entries across entities.

## Execution

`execution.harness` reports the worker runtime the producer recorded.
`execution.requested_model` and `execution.requested_effort` report the model and effort the task was dispatched with, and are `null` when unrecorded or recorded as `default`.
`execution.observed_model`, `execution.observed_effort`, `execution.playbook`, and `execution.workflow_stage` are always `null`, because no observation source for them exists yet.
A requested value is never presented as an observed one.

## Unsupported and disclosed limits

`unsupported[]` lists every category of thing the view does not count or observe, with a `category` and a `reason`.
`native_subagents` is always listed, because native subagents are not observed and no count is reported for them.
`backend:<name>` is listed for each backend outside the supported set, and `backend:unrecorded` for a backend that is not recorded.
`root_limit` names how many home references were not read because a run reads at most 64 roots.
`ownership_cycle` names how many home references name an ancestor home as a delegate and were not treated as delegation.

## History

`history` holds the historical summaries of inactive entities, with `limit`, `retained`, `evicted`, `limit_reached`, `summaries`, and `restart_disclosure`.
At most 1000 historical summaries are retained, and the oldest inactive ones are evicted first.
Required active ancestors and their prerequisites stay outside that allowance, so `retained` can exceed `limit`.
`limit_reached` is true when the allowance was reached and `evicted` counts the summaries removed for it.
History survives only through a caller-supplied `--previous` document, and a run without one starts with empty history.
`restart_disclosure` states that limit in words so a consumer never reads an empty history as a complete one.
Each summary carries `key`, `generation`, `last_state`, `last_seen_at`, `historical`, and `parent_key`, so a consumer can follow the ancestor chain.

## Verification

`tests/fm-birds-eye-model.test.sh` replays the recorded scenarios in `tests/fixtures/birds-eye/` through `--fixture-dir`, and runs live cases against empty homes and a home with a spawned task.
The fixtures cover two projects with a shared secondmate, overlapping roots, equal display names across homes, a failing root after a good collection, a stale output, generation changes, missing, invalid, and future timestamps, unknown states, a 200-entity fleet reaching depth 9, a 14-home chain with a cycle and an unreadable ancestor, observed duration and first observed changes across runs, and over 1000 historical summaries with an ancestor chain longer than 12.
The staleness floor and the 5-second interval refusal are asserted at the model level.
