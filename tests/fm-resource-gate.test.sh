#!/usr/bin/env bash
# Behavior tests for the host resource gate (bin/fm-resource-gate-lib.sh). A local
# agent start is meant to ask `mcao resources admit` before it proceeds. That subcommand is
# not yet wired into the mcao CLI (DPES-866), so these tests use a fake mcao. A deferral
# (exit 75) leaves nothing behind, an admitted start proceeds, a pre-admitted start skips
# the question entirely, and a missing or failing mcao fails open with one warning.
#
# Every case drives the real bin/ entry points (fm-spawn, fm-control, the secondmate
# liveness library, fm-bootstrap, and the two snapshot projections) against a fake
# mcao whose verdicts come from sidecar files, plus logging tmux and treehouse fakes
# that record every call so a deferral can be proved to have touched nothing.
set -u

# shellcheck source=tests/lib.sh
. "$(dirname "${BASH_SOURCE[0]}")/lib.sh"
# shellcheck source=tests/fixtures.sh
. "$(dirname "${BASH_SOURCE[0]}")/fixtures.sh"

unset TASKS_AXI_BACKEND || :

CONTROL="$ROOT/bin/fm-control.sh"
BOOTSTRAP="$ROOT/bin/fm-bootstrap.sh"
FLEET="$ROOT/bin/fm-fleet-snapshot.sh"
BEARINGS="$ROOT/bin/fm-bearings-snapshot.sh"
BASE_PATH=$PATH
DEFER_EXIT=75
TMP_ROOT=$(fm_test_tmproot fm-resource-gate)
HAVE_TASKS_AXI=0
command -v tasks-axi >/dev/null 2>&1 && HAVE_TASKS_AXI=1

# --- fake mcao --------------------------------------------------------------

# write_mcao <dir>: a fake mcao whose verdicts come from sidecar files beside it. It stands
# in for `resources admit` and `resources show --json`, which are not yet wired into the mcao
# CLI (DPES-866). `resources admit` prints admit.stderr to stderr and exits with admit.exit;
# `resources show --json` prints show.json, or exits 1 when none is written. Any other argv
# exits 64 so a drifted caller fails loudly. Every call is appended to mcao.calls, which is how
# a test proves whether the gate was asked at all.
write_mcao() {
  local dir=$1
  mkdir -p "$dir"
  : > "$dir/mcao.calls"
  cat > "$dir/mcao" <<'SH'
#!/usr/bin/env bash
D=${0%/*}
printf '%s\n' "$*" >> "$D/mcao.calls"
case "$1 $2" in
  "resources admit")
    [ ! -s "$D/admit.stderr" ] || cat "$D/admit.stderr" >&2
    exit "$(cat "$D/admit.exit")"
    ;;
  "resources show")
    [ -f "$D/show.json" ] || exit 1
    cat "$D/show.json"
    ;;
  *) exit 64 ;;
esac
SH
  chmod +x "$dir/mcao"
  set_admit "$dir" 0
}

# set_admit <mcao-dir> <exit> [stderr-line]: the verdict the next admit returns.
set_admit() {
  printf '%s\n' "$2" > "$1/admit.exit"
  if [ -n "${3:-}" ]; then
    printf '%s\n' "$3" > "$1/admit.stderr"
  else
    : > "$1/admit.stderr"
  fi
}

admit_calls() {  # <mcao-dir> [pattern]: how many admit calls matched the pattern
  grep -c "^resources admit ${2:-}" "$1/mcao.calls" || :
}

# --- fixtures ---------------------------------------------------------------

# write_brief <home> <id>: a ship brief with the delivery contract the spawn reads.
write_brief() {
  mkdir -p "$1/data/$2"
  cat > "$1/data/$2/brief.md" <<EOF
# Task
## Captain's intent
Exercise the host resource gate for $2.

## Firstmate spec
Start the agent only when the host has room for it.

# Definition of done
Delivery contract: mode=no-mistakes
Ship branch: moa/$2
EOF
}

# make_home <home> [task-id...]: a Firstmate home with a brief for every task and,
# when tasks-axi is installed, a markdown backlog holding each task as queued.
make_home() {
  local home=$1 id
  shift
  mkdir -p "$home/state" "$home/config" "$home/data" "$home/projects"
  touch "$home/state/.last-watcher-beat"
  printf '%s\n' codex > "$home/config/crew-harness"
  if [ "$HAVE_TASKS_AXI" = 1 ]; then
    printf '%s\n' '# Backlog' '' '## In flight' '' '## Queued' '' '## Done' \
      > "$home/data/backlog.md"
    printf '%s\n' 'backend = "markdown"' '' '[markdown]' 'path = "data/backlog.md"' \
      > "$home/.tasks.toml"
  fi
  for id in "$@"; do
    write_brief "$home" "$id"
    fm_test_ensure_ship_managed_block "$home" "$id" --mode no-mistakes --yolo off
    if [ "$HAVE_TASKS_AXI" = 1 ]; then
      tasks-axi add "$id" "item for $id" --kind ship --file "$home/data/backlog.md" >/dev/null
    fi
  done
}

# gate_case <name> [task-id...]: a case directory with a home, a project clone that
# has an origin, a fake mcao, and logging tmux and treehouse fakes. Each fake appends
# its argv and the FM_RESOURCE_PREADMITTED value it inherited to calls.log.
gate_case() {
  local name=$1 case_dir fakebin
  shift
  case_dir="$TMP_ROOT/$name"
  fakebin=$(fm_fakebin "$case_dir")
  : > "$case_dir/calls.log"
  write_mcao "$case_dir/mcao-bin"
  # The workflow-agent gate runs before the resource gate, so every case seeds its
  # own throwaway HOME (fm_test_run_spawn points HOME there).
  fm_test_seed_workflow_agents "$case_dir/user-home"
  cat > "$fakebin/tmux" <<'SH'
#!/usr/bin/env bash
printf 'tmux %s env=%s\n' "$*" "${FM_RESOURCE_PREADMITTED-unset}" >> "$FM_FAKE_CALL_LOG"
case "$*" in
  *"#{pane_current_path}"*) printf '%s\n' "${FM_FAKE_PANE_PATH:-}"; exit 0 ;;
esac
case "${1:-}" in display-message) printf 'firstmate\n' ;; esac
exit 0
SH
  cat > "$fakebin/treehouse" <<'SH'
#!/usr/bin/env bash
printf 'treehouse %s env=%s\n' "$*" "${FM_RESOURCE_PREADMITTED-unset}" >> "$FM_FAKE_CALL_LOG"
exit 0
SH
  chmod +x "$fakebin/tmux" "$fakebin/treehouse"
  fm_fake_exit0 "$fakebin" gh gh-axi no-mistakes
  fm_git_init_commit "$case_dir/project"
  fm_git_add_origin "$case_dir/project" "$case_dir/project.origin.git"
  make_home "$case_dir/home" "$@"
  printf '%s\n' "$case_dir"
}

# use_case <case-dir>: point this shell at the case's fake mcao and call log.
use_case() {
  export FM_MCAO_BIN="$1/mcao-bin/mcao"
  export FM_FAKE_CALL_LOG="$1/calls.log"
}

# task_worktree <case-dir> <id>: a project worktree for <id>, distinct from the
# primary checkout, which stands in for the treehouse slot a real spawn leases.
task_worktree() {
  local wt="$1/wt-$2"
  git -C "$1/project" worktree add --quiet -b "task-$2" "$wt" >/dev/null
  printf '%s\n' "$wt"
}

call_count() { wc -l < "$1/calls.log" | tr -d ' '; }

worktree_list() { git -C "$1/project" worktree list --porcelain; }

row_state() {  # <home> <id>
  tasks-axi show "$2" --file "$1/data/backlog.md" 2>/dev/null |
    sed -n 's/^  state: *//p' | head -1
}

# state_snapshot <dir>: every file's checksum under a directory, so a test can prove
# a refused operation left the directory exactly as it found it.
state_snapshot() {
  find "$1" -type f -exec cksum {} \; | sort
}

# assert_nothing_created <case-dir> <home> <id> <calls-before> <worktrees-before>
assert_nothing_created() {
  local case_dir=$1 home=$2 id=$3 before=$4 worktrees=$5
  assert_absent "$home/state/$id.meta" "a deferred start published a record for $id"
  assert_absent "$home/data/$id/launch-brief.md" "a deferred start rendered a launch brief for $id"
  assert_equals "$before" "$(call_count "$case_dir")" "a deferred start touched tmux or treehouse for $id"
  assert_equals "$worktrees" "$(worktree_list "$case_dir")" "a deferred start allocated a worktree for $id"
  if [ "$HAVE_TASKS_AXI" = 1 ]; then
    assert_equals queued "$(row_state "$home" "$id")" "a deferred start moved $id's backlog item"
  fi
}

# add_relaunch_record <case-dir> <id>: a running ship's record, its worktree, and its
# brief, shaped the way fm-spawn --relaunch and fm-control relaunch expect to find them.
add_relaunch_record() {
  local case_dir=$1 id=$2 home proj wt
  home="$case_dir/home"; proj="$case_dir/project"; wt="$case_dir/wt-$id"
  git -C "$proj" worktree add --quiet -b "task-$id" "$wt"
  write_brief "$home" "$id"
  fm_test_ensure_ship_managed_block "$home" "$id" --relaunch --mode no-mistakes --yolo off
  {
    echo "window=fmses:fm-$id"
    echo "endpoint_task_id=$id"
    echo "worktree=$wt"
    echo "project=$proj"
    echo "harness=codex"
    echo "kind=ship"
    echo "mode=no-mistakes"
    echo "yolo=off"
    echo "tasktmp=/tmp/fm-$id"
    echo "model=default"
    echo "effort=default"
  } > "$home/state/$id.meta"
  fm_test_persist_ship_profile "$home" "$id" "$(git -C "$wt" rev-parse HEAD)"
}

# run_control <case-dir> <args...>: fm-control.sh under a throwaway HOME, with the
# case's fake tmux and treehouse first on PATH.
run_control() {
  local dir=$1
  shift
  env -u HERDR_ENV -u HERDR_PANE_ID -u HERDR_SESSION -u HERDR_SOCKET_PATH \
    -u HERDR_TAB_ID -u HERDR_WORKSPACE_ID \
    PATH="$dir/fakebin:$BASE_PATH" FM_HOME="$dir/home" \
    HOME="$dir/user-home" CLAUDE_CONFIG_DIR='' \
    FM_SPAWN_NO_GUARD=1 GROK_HOME="$dir/grokhome" \
    FM_CONTROL_POLL=0.01 FM_CONTROL_EXIT_WAIT=0.05 FM_CONTROL_LAUNCH_WAIT=0.05 \
    "$CONTROL" "$@" 2>&1
}

# --- secondmate fixtures ----------------------------------------------------

# sm_world <name>: a scratch primary home with state/, data/, and config/.
sm_world() {
  local w="$TMP_ROOT/$1"
  mkdir -p "$w/home/state" "$w/home/data" "$w/home/config"
  touch "$w/home/state/.last-watcher-beat"
  printf 'codex\n' > "$w/home/config/crew-harness"
  printf '%s\n' "$w"
}

# add_sm_mate <world> <id> <window>: a plain secondmate home and its registry meta.
add_sm_mate() {
  local w=$1 id=$2 window=$3 home
  home="$w/$id"
  mkdir -p "$home/bin" "$home/data" "$home/state" "$home/config" "$home/projects"
  printf '%s\n' "$id" > "$home/.fm-secondmate-home"
  printf '# Firstmate\n' > "$home/AGENTS.md"
  printf 'charter\n' > "$home/data/charter.md"
  printf '%s\n' 'projects/' 'state/' 'data/' 'config/' '.no-mistakes/' > "$home/.gitignore"
  git -C "$home" init -q -b main
  {
    printf 'window=%s\n' "$window"
    printf 'kind=secondmate\n'
    printf 'harness=claude\n'
    printf 'home=%s\n' "$home"
  } > "$w/home/state/$id.meta"
}

# make_sm_tmux <world>: a tmux fake for a dead secondmate. The pane runs a shell, the
# window is listed until it is killed, and every window call is logged.
make_sm_tmux() {
  local fb
  fb=$(fm_fakebin "$1")
  cat > "$fb/tmux" <<'SH'
#!/usr/bin/env bash
set -u
case "${1:-}" in
  display-message)
    for a in "$@"; do
      case "$a" in *pane_current_command*) printf '%s\n' zsh; exit 0 ;; esac
    done
    exit 0
    ;;
  list-windows)
    [ -e "$FM_TMUX_CALL_LOG.killed" ] || printf '%s\n' fm-sm1
    exit 0
    ;;
  new-window|kill-window)
    printf '%s\n' "$*" >> "$FM_TMUX_CALL_LOG"
    [ "${1:-}" = kill-window ] && : > "$FM_TMUX_CALL_LOG.killed"
    [ "${1:-}" = new-window ] && rm -f "$FM_TMUX_CALL_LOG.killed"
    exit 0
    ;;
esac
exit 0
SH
  chmod +x "$fb/tmux"
  printf '%s\n' "$fb"
}

# make_toolchain <world>: the tools bootstrap's readiness sweep expects, all stubbed.
make_toolchain() {
  local dir=$1 fb
  fb=$(fm_fakebin "$dir")
  fm_fake_exit0 "$fb" node chrome-devtools-axi pi-signed
  fm_fake_version_tool "$fb" lavish-axi FM_FAKE_LAVISH_AXI_VERSION 0.1.80
  cat > "$fb/gh-axi" <<'SH'
#!/usr/bin/env bash
if [ "${1:-}" = --version ]; then printf '%s\n' '0.1.29'; exit 0; fi
exit 0
SH
  cat > "$fb/gh" <<'SH'
#!/usr/bin/env bash
exit 0
SH
  cat > "$fb/treehouse" <<'SH'
#!/usr/bin/env bash
if [ "${1:-}" = get ] && [ "${2:-}" = --help ]; then printf '%s\n' 'Usage: treehouse get [--lease]'; fi
exit 0
SH
  cat > "$fb/no-mistakes" <<'SH'
#!/usr/bin/env bash
if [ "${1:-}" = --version ]; then printf '%s\n' 'no-mistakes version v1.46.0 (fake)'; fi
exit 0
SH
  cat > "$fb/tasks-axi" <<'SH'
#!/usr/bin/env bash
case "${1:-} ${2:-}" in
  "--version ") printf '%s\n' '0.2.6' ;;
  "update --help") printf '%s\n' 'usage: tasks-axi update <id> [flags]' '  --archive-body' ;;
  "mv --help") printf '%s\n' 'usage: tasks-axi mv <id> [<id>...] --to <path-or-dir>' ;;
esac
exit 0
SH
  cat > "$fb/quota-axi" <<'SH'
#!/usr/bin/env bash
if [ "${1:-}" = --version ]; then printf '%s\n' '0.1.51'; fi
exit 0
SH
  chmod +x "$fb"/*
  printf '%s\n' "$fb"
}

# --- cases: fresh ship, secondmate, and relaunch spawns ----------------------

test_fresh_ship_deferred_by_host_resources_creates_nothing() {
  local case_dir home out rc=0 before worktrees wt
  case_dir=$(gate_case defer-ship task-a)
  home="$case_dir/home"
  wt=$(task_worktree "$case_dir" task-a)
  use_case "$case_dir"
  set_admit "$case_dir/mcao-bin" "$DEFER_EXIT" "deferred: host resources over limit: disk free below threshold"
  before=$(call_count "$case_dir")
  worktrees=$(worktree_list "$case_dir")
  out=$(fm_test_run_spawn "$home" "$wt" "$case_dir/fakebin" \
    task-a "$case_dir/project" --mode no-mistakes --yolo off) || rc=$?
  assert_equals "$DEFER_EXIT" "$rc" "a host-resource deferral must exit $DEFER_EXIT"
  assert_contains "$out" "deferred:" "the deferral must name its cause"
  assert_contains "$(cat "$case_dir/mcao-bin/mcao.calls")" "resources admit --kind ship --task task-a" \
    "a fresh ship must ask the gate before it starts"
  assert_nothing_created "$case_dir" "$home" task-a "$before" "$worktrees"
  unset FM_MCAO_BIN FM_FAKE_CALL_LOG
  pass "fresh ship: a host-resource deferral exits 75, names its cause, and creates no record, brief, endpoint, worktree, or backlog change"
}

test_secondmate_spawn_deferred_creates_nothing() {
  local case_dir home out rc=0 before
  case_dir=$(gate_case defer-secondmate)
  home="$case_dir/home"
  use_case "$case_dir"
  export FM_TEST_NO_ASSIGNMENT_PROFILE=1
  set_admit "$case_dir/mcao-bin" "$DEFER_EXIT" "deferred: host resources over limit: memory used above threshold"
  before=$(call_count "$case_dir")
  out=$(fm_test_run_spawn "$home" "$case_dir/project" "$case_dir/fakebin" \
    sm-a "$case_dir/sm-a" --secondmate) || rc=$?
  assert_equals "$DEFER_EXIT" "$rc" "a deferred secondmate spawn must exit $DEFER_EXIT"
  assert_contains "$out" "deferred:" "the secondmate deferral must name its cause"
  assert_contains "$(cat "$case_dir/mcao-bin/mcao.calls")" "--task sm-a" \
    "a secondmate spawn must ask the gate before it starts"
  assert_absent "$case_dir/sm-a" "a deferred secondmate spawn created its home"
  assert_absent "$home/state/sm-a.meta" "a deferred secondmate spawn published a record"
  assert_equals "$before" "$(call_count "$case_dir")" "a deferred secondmate spawn touched tmux or treehouse"
  unset FM_TEST_NO_ASSIGNMENT_PROFILE FM_MCAO_BIN FM_FAKE_CALL_LOG
  pass "secondmate spawn: a host-resource deferral exits 75 and creates no home, record, or endpoint"
}

test_relaunch_deferred_leaves_record_byte_identical() {
  local case_dir home meta out rc=0 before worktrees
  case_dir=$(gate_case defer-relaunch)
  add_relaunch_record "$case_dir" task-r
  home="$case_dir/home"
  meta="$home/state/task-r.meta"
  cp "$meta" "$case_dir/task-r.meta.before"
  use_case "$case_dir"
  export FM_TEST_NO_ASSIGNMENT_PROFILE=1
  set_admit "$case_dir/mcao-bin" "$DEFER_EXIT" "deferred: host resources over limit: running agents at capacity"
  before=$(call_count "$case_dir")
  worktrees=$(worktree_list "$case_dir")
  out=$(fm_test_run_spawn "$home" "$case_dir/wt-task-r" "$case_dir/fakebin" \
    task-r --relaunch) || rc=$?
  assert_equals "$DEFER_EXIT" "$rc" "a deferred relaunch must exit $DEFER_EXIT"
  assert_contains "$out" "deferred:" "the relaunch deferral must name its cause"
  assert_contains "$(cat "$case_dir/mcao-bin/mcao.calls")" "resources admit --kind relaunch --task task-r" \
    "a relaunch must ask the gate with the relaunch kind"
  cmp -s "$case_dir/task-r.meta.before" "$meta" ||
    fail "a deferred relaunch rewrote the task record"
  assert_equals "$before" "$(call_count "$case_dir")" "a deferred relaunch touched tmux or treehouse"
  assert_equals "$worktrees" "$(worktree_list "$case_dir")" "a deferred relaunch changed the worktree pool"
  unset FM_TEST_NO_ASSIGNMENT_PROFILE FM_MCAO_BIN FM_FAKE_CALL_LOG
  pass "relaunch spawn: a host-resource deferral exits 75 and leaves the record byte-identical"
}

test_control_relaunch_deferred_stops_nothing() {
  local case_dir home rc=0 out before state_before
  case_dir=$(gate_case defer-control)
  add_relaunch_record "$case_dir" task-c
  home="$case_dir/home"
  use_case "$case_dir"
  set_admit "$case_dir/mcao-bin" "$DEFER_EXIT" "deferred: host resources over limit: disk free below threshold"
  before=$(call_count "$case_dir")
  state_before=$(state_snapshot "$home/state")
  out=$(run_control "$case_dir" task-c relaunch --note "replace the agent") || rc=$?
  assert_equals "$DEFER_EXIT" "$rc" "a deferred control relaunch must exit $DEFER_EXIT"
  assert_contains "$out" "deferred:" "the control deferral must name its cause"
  assert_contains "$(cat "$case_dir/mcao-bin/mcao.calls")" "resources admit --kind relaunch --task task-c" \
    "a control relaunch must ask the gate before it stops the agent"
  assert_not_contains "$(cat "$case_dir/calls.log")" "/exit" \
    "a deferred control relaunch must not stop the running agent"
  assert_not_contains "$(cat "$case_dir/calls.log")" "kill-window" \
    "a deferred control relaunch must not kill the endpoint"
  assert_equals "$state_before" "$(state_snapshot "$home/state")" \
    "a deferred control relaunch changed the durable state"
  assert_equals "$before" "$(call_count "$case_dir")" "a deferred control relaunch touched tmux or treehouse"
  unset FM_MCAO_BIN FM_FAKE_CALL_LOG
  pass "control relaunch: a host-resource deferral exits 75 before the agent is stopped, with no journal or state change"
}

# --- cases: the marker and the admitted start --------------------------------

test_admitted_start_asks_the_gate_once() {
  local case_dir home rc=0 out wt
  case_dir=$(gate_case admit-once task-b)
  home="$case_dir/home"
  wt=$(task_worktree "$case_dir" task-b)
  use_case "$case_dir"
  set_admit "$case_dir/mcao-bin" 0
  out=$(fm_test_run_spawn "$home" "$wt" "$case_dir/fakebin" \
    task-b "$case_dir/project" --mode no-mistakes --yolo off) || rc=$?
  assert_equals 0 "$rc" "an admitted start must proceed, got: $out"
  assert_present "$home/state/task-b.meta" "an admitted start must publish its record"
  assert_equals 1 "$(admit_calls "$case_dir/mcao-bin" "--kind ship --task task-b")" \
    "an admitted start must ask the gate exactly once"
  unset FM_MCAO_BIN FM_FAKE_CALL_LOG
  pass "admitted start: the gate is asked once and the start proceeds"
}

test_preadmitted_start_skips_the_gate_and_hands_no_marker_on() {
  local case_dir home rc=0 line out wt
  case_dir=$(gate_case preadmitted task-m)
  home="$case_dir/home"
  wt=$(task_worktree "$case_dir" task-m)
  use_case "$case_dir"
  # The gate would defer if it were asked, so a skipped question is what lets the start through.
  set_admit "$case_dir/mcao-bin" "$DEFER_EXIT" "deferred: host resources over limit"
  export FM_RESOURCE_PREADMITTED=1
  out=$(fm_test_run_spawn "$home" "$wt" "$case_dir/fakebin" \
    task-m "$case_dir/project" --mode no-mistakes --yolo off) || rc=$?
  unset FM_RESOURCE_PREADMITTED
  assert_equals 0 "$rc" "a pre-admitted start must not be deferred by the gate, got: $out"
  assert_present "$home/state/task-m.meta" "a pre-admitted start must publish its record"
  assert_equals 0 "$(admit_calls "$case_dir/mcao-bin")" "a pre-admitted start must not call mcao at all"
  assert_present "$case_dir/calls.log" "the start must reach tmux or treehouse"
  line=$(grep -v 'env=unset$' "$case_dir/calls.log" || :)
  [ -z "$line" ] || fail "a command launched after the admitted start inherited the marker: $line"
  unset FM_MCAO_BIN FM_FAKE_CALL_LOG
  pass "pre-admitted start: mcao is not asked, and no launched command inherits FM_RESOURCE_PREADMITTED"
}

test_preadmitted_marker_with_another_value_still_asks_the_gate() {
  local case_dir home rc=0 out wt
  case_dir=$(gate_case marker-other task-q)
  home="$case_dir/home"
  wt=$(task_worktree "$case_dir" task-q)
  use_case "$case_dir"
  set_admit "$case_dir/mcao-bin" "$DEFER_EXIT" "deferred: host resources over limit"
  export FM_RESOURCE_PREADMITTED=yes
  out=$(fm_test_run_spawn "$home" "$wt" "$case_dir/fakebin" \
    task-q "$case_dir/project" --mode no-mistakes --yolo off) || rc=$?
  unset FM_RESOURCE_PREADMITTED
  assert_equals "$DEFER_EXIT" "$rc" "a marker value other than 1 must not bypass the gate, got: $out"
  assert_equals 1 "$(admit_calls "$case_dir/mcao-bin")" "a marker value other than 1 must still ask mcao once"
  assert_absent "$home/state/task-q.meta" "a deferred start must publish no record"
  unset FM_MCAO_BIN FM_FAKE_CALL_LOG
  pass "marker with another value: the gate is still asked, and a deferral creates nothing"
}

# --- cases: fail-open -------------------------------------------------------

test_missing_mcao_fails_open_with_one_warning() {
  local case_dir home rc=0 out wt
  case_dir=$(gate_case no-binary task-n)
  home="$case_dir/home"
  wt=$(task_worktree "$case_dir" task-n)
  use_case "$case_dir"
  export FM_MCAO_BIN="$case_dir/no-such-dir/mcao"
  out=$(fm_test_run_spawn "$home" "$wt" "$case_dir/fakebin" \
    task-n "$case_dir/project" --mode no-mistakes --yolo off) || rc=$?
  assert_equals 0 "$rc" "a missing mcao must fail open and let the start proceed"
  assert_present "$home/state/task-n.meta" "a fail-open start must publish its record"
  assert_equals 1 "$(printf '%s\n' "$out" | grep -c 'warning: resource gate skipped' || :)" \
    "a missing mcao must produce exactly one warning"
  assert_contains "$out" "FM_MCAO_BIN" "the warning must name the setting that fixes it"
  unset FM_MCAO_BIN FM_FAKE_CALL_LOG
  pass "missing mcao: the start proceeds with exactly one warning"
}

test_failing_mcao_fails_open_with_one_warning() {
  local case_dir home rc=0 out wt
  case_dir=$(gate_case failing-mcao task-f)
  home="$case_dir/home"
  wt=$(task_worktree "$case_dir" task-f)
  use_case "$case_dir"
  set_admit "$case_dir/mcao-bin" 1 "mcao: internal error while sampling"
  out=$(fm_test_run_spawn "$home" "$wt" "$case_dir/fakebin" \
    task-f "$case_dir/project" --mode no-mistakes --yolo off) || rc=$?
  assert_equals 0 "$rc" "an mcao that exits non-zero, other than 75, must fail open"
  assert_present "$home/state/task-f.meta" "a fail-open start must publish its record"
  assert_equals 1 "$(printf '%s\n' "$out" | grep -c 'warning: resource gate skipped: mcao exited 1' || :)" \
    "a failing mcao must produce exactly one warning"
  assert_not_contains "$out" "internal error while sampling" \
    "a fail-open start must not pass mcao's own stderr through"
  unset FM_MCAO_BIN FM_FAKE_CALL_LOG
  pass "failing mcao: the start proceeds with exactly one warning and no mcao stderr"
}

test_admitted_start_shows_a_thresholds_warning_once_and_proceeds() {
  local case_dir home rc=0 out wt
  case_dir=$(gate_case admit-warning task-w)
  home="$case_dir/home"
  wt=$(task_worktree "$case_dir" task-w)
  use_case "$case_dir"
  # A bad thresholds line: mcao admits on the built-in defaults and says so on stderr.
  set_admit "$case_dir/mcao-bin" 0 "warning: $case_dir/config/resource-thresholds:3: unknown key; built-in defaults apply"
  out=$(fm_test_run_spawn "$home" "$wt" "$case_dir/fakebin" \
    task-w "$case_dir/project" --mode no-mistakes --yolo off) || rc=$?
  assert_equals 0 "$rc" "an admit with a thresholds warning must let the start proceed, got: $out"
  assert_present "$home/state/task-w.meta" "an admitted start must publish its record"
  assert_equals 1 "$(printf '%s\n' "$out" | grep -c 'unknown key; built-in defaults apply' || :)" \
    "a thresholds warning from an admitted start must show exactly once"
  unset FM_MCAO_BIN FM_FAKE_CALL_LOG
  pass "admitted start with a thresholds warning: the warning shows once and the start proceeds"
}

# --- cases: secondmate liveness ---------------------------------------------

test_secondmate_liveness_deferral_counts_no_attempt() {
  local w home ledger out run
  w=$(sm_world liveness-defer)
  home="$w/home"
  add_sm_mate "$w" sm1 firstmate:fm-sm1
  write_mcao "$w/mcao-bin"
  set_admit "$w/mcao-bin" "$DEFER_EXIT" "deferred: host resources over limit: memory used above threshold"
  ledger="$home/state/.secondmate-relaunch-sm1"
  for run in 1 2 3 4 5; do
    # STATE and CONFIG are the caller globals fm-watch and fm-bootstrap define before
    # they call the library; the library reads them under set -u.
    out=$(FM_HOME="$home" STATE="$home/state" CONFIG="$home/config" \
      FM_MCAO_BIN="$w/mcao-bin/mcao" bash -c '
      . "$0/bin/fm-secondmate-liveness-lib.sh"
      rc=0
      fm_secondmate_liveness_relaunch "$1" sm1 || rc=$?
      printf "%s|%s|%s\n" "$rc" "$FM_SM_LIVE_STATUS" "$FM_SM_LIVE_REASON"
    ' "$ROOT" "$home/state/sm1.meta")
    assert_contains "$out" "1|deferred|" "deferral run $run must report status deferred and exit 1"
    assert_contains "$out" "deferred: host resources" "deferral run $run must carry the gate's reason"
  done
  assert_equals 5 "$(admit_calls "$w/mcao-bin" "--kind secondmate --task sm1")" \
    "every liveness run must ask the gate, as a secondmate start, before its attempt"
  assert_absent "$home/state/.secondmate-relaunch-bound-sm1" \
    "a deferred liveness relaunch must not arm the attempt bound"
  if [ -e "$ledger" ]; then
    [ ! -s "$ledger" ] || fail "a deferred liveness relaunch wrote an attempt row: $(cat "$ledger")"
  fi
  pass "secondmate liveness: five deferred runs write no attempt row and never arm the bound"
}

test_bootstrap_reports_a_deferred_secondmate_relaunch() {
  local w fb tmuxfb log out
  w=$(sm_world bootstrap-defer)
  add_sm_mate "$w" sm1 firstmate:fm-sm1
  fb=$(make_toolchain "$w")
  tmuxfb=$(make_sm_tmux "$w")
  write_mcao "$w/mcao-bin"
  set_admit "$w/mcao-bin" "$DEFER_EXIT" "deferred: host resources over limit: disk free below threshold"
  log="$w/calls.log"
  : > "$log"
  out=$(PATH="$tmuxfb:$fb:$BASE_PATH" TMUX='' FM_BACKEND=tmux FM_HOME="$w/home" \
    FM_TEST_PANE_CMD=zsh FM_TMUX_CALL_LOG="$log" FM_MCAO_BIN="$w/mcao-bin/mcao" \
    "$BOOTSTRAP" 2>&1)
  assert_contains "$out" "SECONDMATE_LIVENESS: secondmate sm1: relaunch deferred: deferred: host resources" \
    "bootstrap must report the deferred relaunch and its reason"
  assert_not_contains "$out" "SECONDMATE_LIVENESS: secondmate sm1: respawned" \
    "a deferred relaunch must not report a respawn"
  assert_not_contains "$(cat "$log")" "kill-window" "a deferred relaunch must not kill the stale endpoint"
  assert_not_contains "$(cat "$log")" "new-window" "a deferred relaunch must not open a new endpoint"
  assert_absent "$w/home/state/.secondmate-relaunch-bound-sm1" \
    "a deferred bootstrap relaunch must not arm the attempt bound"
  pass "bootstrap: a deferred secondmate relaunch is reported with its reason and touches no endpoint"
}

# --- cases: snapshots -------------------------------------------------------

SHOW_JSON='{"schema":"fm-resource-snapshot.v1","state":"ok","sampled_at":"2026-10-25T12:00:00Z","readings":[{"kind":"memory_used_pct","value":42,"limit":90,"level":"ok"},{"kind":"disk_free_pct","value":71,"limit":10,"level":"ok"}],"breaches":[]}'

test_fleet_snapshot_reports_the_host_resource_view() {
  local case_dir home out got want
  case_dir=$(gate_case fleet-resources)
  home="$case_dir/home"
  printf '%s\n' "$SHOW_JSON" > "$case_dir/mcao-bin/show.json"
  out=$(env PATH="$case_dir/fakebin:$BASE_PATH" FM_HOME="$home" \
    FM_MCAO_BIN="$case_dir/mcao-bin/mcao" "$FLEET" --json 2>/dev/null) || true
  got=$(printf '%s' "$out" | jq -S .resources)
  want=$(printf '%s\n' "$SHOW_JSON" | jq -S .)
  assert_equals "$want" "$got" "the fleet snapshot must carry mcao's resource view unchanged"

  rm -f "$case_dir/mcao-bin/show.json"
  out=$(env PATH="$case_dir/fakebin:$BASE_PATH" FM_HOME="$home" \
    FM_MCAO_BIN="$case_dir/no-such-dir/mcao" "$FLEET" --json 2>/dev/null) || true
  assert_equals unknown "$(printf '%s' "$out" | jq -r .resources.state)" \
    "a fleet snapshot without mcao must report the resource state as unknown"
  assert_equals "mcao unavailable" "$(printf '%s' "$out" | jq -r .resources.reason)" \
    "a fleet snapshot without mcao must say why"
  pass "fleet snapshot: the resource view matches mcao's, and reads unknown when mcao is absent"
}

test_bearings_projects_the_host_resource_view() {
  local case_dir home out
  case_dir=$(gate_case bearings-resources)
  home="$case_dir/home"
  printf '%s\n' "$SHOW_JSON" > "$case_dir/mcao-bin/show.json"
  out=$(env PATH="$case_dir/fakebin:$BASE_PATH" FM_HOME="$home" \
    FM_BEARINGS_NOW=2026-10-25T12:00:00Z NET_LOG="$home/net.log" \
    FM_MCAO_BIN="$case_dir/mcao-bin/mcao" "$BEARINGS" --json 2>/dev/null) || true
  assert_equals ok "$(printf '%s' "$out" | jq -r .resources.state)" \
    "bearings must project the resource state beside in_flight"
  assert_equals 2 "$(printf '%s' "$out" | jq -r '.resources.readings | length')" \
    "bearings must project every reading"
  assert_equals '[]' "$(printf '%s' "$out" | jq -c .resources.breaches)" \
    "bearings must project the breach list"

  rm -f "$case_dir/mcao-bin/show.json"
  out=$(env PATH="$case_dir/fakebin:$BASE_PATH" FM_HOME="$home" \
    FM_BEARINGS_NOW=2026-10-25T12:00:00Z NET_LOG="$home/net.log" \
    FM_MCAO_BIN="$case_dir/no-such-dir/mcao" "$BEARINGS" --json 2>/dev/null) || true
  assert_equals unknown "$(printf '%s' "$out" | jq -r .resources.state)" \
    "bearings must read the resource state as unknown when mcao is absent"
  pass "bearings: the resource state, readings, and breaches project beside in_flight, and read unknown when mcao is absent"
}

test_fleet_snapshot_reads_unknown_for_a_misshapen_resource_view() {
  local case_dir home out value
  case_dir=$(gate_case fleet-misshapen)
  home="$case_dir/home"
  for value in '42' '[]' 'null' '{"a":1} {"b":2}'; do
    printf '%s\n' "$value" > "$case_dir/mcao-bin/show.json"
    out=$(env PATH="$case_dir/fakebin:$BASE_PATH" FM_HOME="$home" \
      FM_MCAO_BIN="$case_dir/mcao-bin/mcao" "$FLEET" --json 2>/dev/null) \
      || fail "a fleet snapshot must survive mcao printing $value as its resource view"
    assert_equals unknown "$(printf '%s' "$out" | jq -r .resources.state)" \
      "a resource view of $value is not exactly one JSON object and must read as unknown"
  done
  pass "fleet snapshot: a resource view that is not exactly one JSON object reads as unknown"
}

# A caller that sources only the wake library and the gate, as the relaunch path in
# bin/fm-control.sh does, must still reach every registered local home. The registry parser
# belongs to bin/fm-secondmate-registry-lib.sh, which that chain does not load.
test_gate_passes_registered_local_homes_from_a_caller_without_the_registry_library() {
  local case_dir home child child_state out rc=0
  case_dir=$(gate_case gate-registry)
  home="$case_dir/home"
  child="$case_dir/child-sm"
  mkdir -p "$child/state"
  child_state="$(CDPATH='' cd -- "$child" && pwd -P)/state"
  printf '%s\n' "- sm1 - child summary (home: $child; scope: child scope; projects: none; added 2026-10-01)" \
    > "$home/data/secondmates.md"
  # shellcheck disable=SC2016 # positional params expand in the child shell.
  out=$(env PATH="$case_dir/fakebin:$BASE_PATH" FM_HOME="$home" FM_MCAO_BIN="$case_dir/mcao-bin/mcao" \
    bash -c '. "$1/bin/fm-wake-lib.sh"; . "$1/bin/fm-resource-gate-lib.sh"; \
      fm_resource_gate_admit relaunch task-1 "$FM_HOME" "$FM_HOME/config" "$FM_HOME/state"' bash "$ROOT" 2>&1) || rc=$?
  assert_equals 0 "$rc" "an admitted relaunch must return 0, got: $out"
  assert_equals "" "$(printf '%s' "$out" | grep 'command not found' || :)" \
    "the gate must load the registry parser itself: $out"
  assert_equals 1 "$(grep -cF -- "--state-dir $child_state" "$case_dir/mcao-bin/mcao.calls")" \
    "the registered local home's state directory must reach mcao"
  pass "resource gate: a relaunch from a caller without the registry library counts every registered local home"
}

# A caller that sources only the wake library and the gate, with no mcao binary installed, must
# still walk every registered local home. The missing binary fails open, and that return must not
# skip loading the registry parser the walk needs.
test_gate_loads_the_registry_parser_when_mcao_is_absent() {
  local case_dir home child child_state out rc=0
  case_dir=$(gate_case gate-registry-no-mcao)
  home="$case_dir/home"
  child="$case_dir/child-sm"
  mkdir -p "$child/state"
  child_state="$(CDPATH='' cd -- "$child" && pwd -P)/state"
  printf '%s\n' "- sm1 - child summary (home: $child; scope: child scope; projects: none; added 2026-10-01)" \
    > "$home/data/secondmates.md"
  # shellcheck disable=SC2016 # positional params expand in the child shell.
  out=$(env PATH="$case_dir/fakebin:$BASE_PATH" FM_HOME="$home" FM_MCAO_BIN="$case_dir/no-such-dir/mcao" \
    bash -c '. "$1/bin/fm-wake-lib.sh"; . "$1/bin/fm-resource-gate-lib.sh"; \
      fm_resource_gate_admit relaunch task-1 "$FM_HOME" "$FM_HOME/config" "$FM_HOME/state" || exit $?; \
      fm_local_firstmate_state_dirs "$FM_HOME/state" && printf "%s\n" "${FM_LOCAL_FIRSTMATE_STATES[@]}"' bash "$ROOT" 2>&1) || rc=$?
  assert_equals 0 "$rc" "an absent mcao must fail open and the registry walk must succeed, got: $out"
  assert_equals 1 "$(printf '%s\n' "$out" | grep -c 'resource gate skipped' || :)" \
    "an absent mcao must fail open with exactly one warning: $out"
  assert_equals "" "$(printf '%s' "$out" | grep 'command not found' || :)" \
    "the gate must load the registry parser before a missing mcao returns: $out"
  assert_equals 1 "$(printf '%s\n' "$out" | grep -cxF -- "$child_state" || :)" \
    "the registered local home's state directory must be walked: $out"
  pass "resource gate: a missing mcao fails open and the registry walk still counts every registered local home"
}

test_fresh_ship_deferred_by_host_resources_creates_nothing
test_secondmate_spawn_deferred_creates_nothing
test_relaunch_deferred_leaves_record_byte_identical
test_control_relaunch_deferred_stops_nothing
test_admitted_start_asks_the_gate_once
test_preadmitted_start_skips_the_gate_and_hands_no_marker_on
test_preadmitted_marker_with_another_value_still_asks_the_gate
test_missing_mcao_fails_open_with_one_warning
test_failing_mcao_fails_open_with_one_warning
test_admitted_start_shows_a_thresholds_warning_once_and_proceeds
test_secondmate_liveness_deferral_counts_no_attempt
test_bootstrap_reports_a_deferred_secondmate_relaunch
test_fleet_snapshot_reports_the_host_resource_view
test_fleet_snapshot_reads_unknown_for_a_misshapen_resource_view
test_bearings_projects_the_host_resource_view
test_gate_passes_registered_local_homes_from_a_caller_without_the_registry_library
test_gate_loads_the_registry_parser_when_mcao_is_absent
