#!/usr/bin/env bash
# Regression tests for cleanup endpoint and worktree-slot identity validation.
set -u

# shellcheck source=tests/lib.sh
. "$(dirname "${BASH_SOURCE[0]}")/lib.sh"

TEARDOWN="$ROOT/bin/fm-teardown.sh"
TMP_ROOT=$(fm_test_tmproot fm-teardown-endpoint-safety)
REAL_TMUX=$(command -v tmux || true)

make_case() {  # <name>
  local dir=$1
  mkdir -p "$TMP_ROOT/$dir/home/state" "$TMP_ROOT/$dir/home/data" \
    "$TMP_ROOT/$dir/home/config" "$TMP_ROOT/$dir/fakebin" \
    "$TMP_ROOT/$dir/worktree" "$TMP_ROOT/$dir/project"
  git init -q "$TMP_ROOT/$dir/project"
  : > "$TMP_ROOT/$dir/worktree/sentinel"
  : > "$TMP_ROOT/$dir/runtime.log"
  cat > "$TMP_ROOT/$dir/fakebin/tmux" <<'SH'
#!/usr/bin/env bash
printf 'tmux' >> "${FM_RUNTIME_LOG:?}"
printf ' <%s>' "$@" >> "${FM_RUNTIME_LOG:?}"
printf '\n' >> "${FM_RUNTIME_LOG:?}"
exit 0
SH
  cat > "$TMP_ROOT/$dir/fakebin/treehouse" <<'SH'
#!/usr/bin/env bash
printf 'treehouse' >> "${FM_RUNTIME_LOG:?}"
printf ' <%s>' "$@" >> "${FM_RUNTIME_LOG:?}"
printf '\n' >> "${FM_RUNTIME_LOG:?}"
exit 0
SH
  chmod +x "$TMP_ROOT/$dir/fakebin/tmux" "$TMP_ROOT/$dir/fakebin/treehouse"
  printf '%s\n' "$TMP_ROOT/$dir"
}

mark_case_as_treehouse_pool() {  # <case>
  local dir=$1
  rm -rf "$dir/worktree"
  mkdir -p "$dir/pool/1"
  git -C "$dir/project" -c user.name=test -c user.email=test@example.invalid \
    commit --allow-empty -qm pool-fixture
  git -C "$dir/project" worktree add -q --detach "$dir/pool/1/project"
  ln -s "pool/1/project" "$dir/worktree"
  printf '{"worktrees":[{"name":"1","path":"%s"}]}\n' \
    "$dir/pool/1/project" > "$dir/pool/treehouse-state.json"
  : > "$dir/worktree/sentinel"
}

claim_pool_slot() {  # <case> <task-id> [home]
  local dir=$1 id=$2 home=${3:-$1/home}
  printf 'task=%s\nhome=%s\n' "$id" "$home" > "$dir/pool/1/.fm-slot-owner"
}

run_case() {  # <case> <id>
  local dir=$1 id=$2
  FM_HOME="$dir/home" FM_ROOT_OVERRIDE="$ROOT" \
  FM_RUNTIME_LOG="$dir/runtime.log" PATH="$dir/fakebin:$PATH" \
    "$TEARDOWN" "$id" --force
}

assert_refused_without_mutation() {  # <case> <id> <description>
  local dir=$1 id=$2 description=$3 rc
  set +e
  run_case "$dir" "$id" > "$dir/stdout" 2> "$dir/stderr"
  rc=$?
  set -e
  [ "$rc" -ne 0 ] || fail "$description: teardown unexpectedly succeeded"
  assert_present "$dir/home/state/$id.meta" "$description: metadata changed before refusal"
  assert_present "$dir/worktree/sentinel" "$description: worktree changed before refusal"
  [ ! -s "$dir/runtime.log" ] || fail "$description: runtime command ran before refusal: $(cat "$dir/runtime.log")"
}

test_invalid_endpoint_records_refuse_before_mutation() {
  local dir id=endpoint-a

  dir=$(make_case missing)
  fm_write_meta "$dir/home/state/$id.meta" \
    "worktree=$dir/worktree" "project=$dir/project" "kind=scout"
  assert_refused_without_mutation "$dir" "$id" "missing endpoint"

  dir=$(make_case empty)
  fm_write_meta "$dir/home/state/$id.meta" \
    "window=" "worktree=$dir/worktree" "project=$dir/project" "kind=scout"
  assert_refused_without_mutation "$dir" "$id" "empty endpoint"

  dir=$(make_case malformed)
  fm_write_meta "$dir/home/state/$id.meta" \
    "window=ambient-current-window" "worktree=$dir/worktree" \
    "project=$dir/project" "kind=scout"
  assert_refused_without_mutation "$dir" "$id" "malformed endpoint"

  dir=$(make_case mismatched)
  fm_write_meta "$dir/home/state/$id.meta" \
    "window=isolated:fm-other-task" "endpoint_task_id=other-task" \
    "worktree=$dir/worktree" "project=$dir/project" "kind=scout"
  assert_refused_without_mutation "$dir" "$id" "task-mismatched endpoint"

  dir=$(make_case empty-binding)
  fm_write_meta "$dir/home/state/$id.meta" \
    "window=isolated:fm-$id" "endpoint_task_id=" \
    "worktree=$dir/worktree" "project=$dir/project" "kind=scout"
  assert_refused_without_mutation "$dir" "$id" "empty task binding"

  dir=$(make_case duplicate-binding)
  fm_write_meta "$dir/home/state/$id.meta" \
    "window=isolated:fm-$id" "endpoint_task_id=$id" "endpoint_task_id=$id" \
    "worktree=$dir/worktree" "project=$dir/project" "kind=scout"
  assert_refused_without_mutation "$dir" "$id" "duplicate task binding"

  pass "fm-teardown: missing, empty, malformed, ambiguous, and task-mismatched endpoints refuse before every mutation or runtime call"
}

test_control_lock_contention_refuses_before_mutation() {
  local dir id=locked-task lock holder i=0 rc
  dir=$(make_case control-lock)
  fm_write_meta "$dir/home/state/$id.meta" \
    "window=isolated:fm-$id" "endpoint_task_id=$id" \
    "worktree=$dir/worktree" "project=$dir/project" "kind=scout"
  lock="$dir/home/state/.control-$id.lock"
  (
    # shellcheck source=/dev/null
    . "$ROOT/bin/fm-wake-lib.sh"
    fm_lock_try_acquire "$lock" || exit 1
    sleep 30
  ) &
  holder=$!
  while [ ! -e "$lock" ] && [ "$i" -lt 100 ]; do
    sleep 0.1
    i=$((i + 1))
  done
  [ -e "$lock" ] || {
    kill "$holder" 2>/dev/null || true
    wait "$holder" 2>/dev/null || true
    fail "could not stage a held lifecycle lock"
  }
  fm_write_meta "$dir/home/state/$id.meta" \
    "window=isolated:fm-$id" "endpoint_task_id=other-task" \
    "worktree=$dir/worktree" "project=$dir/project" "kind=scout"

  set +e
  run_case "$dir" "$id" > "$dir/stdout" 2> "$dir/stderr"
  rc=$?
  set -e
  [ "$rc" -ne 0 ] || fail "teardown unexpectedly succeeded under lifecycle lock contention"
  assert_present "$dir/home/state/$id.meta" "contended teardown removed task metadata"
  assert_present "$dir/worktree/sentinel" "contended teardown changed the worktree"
  assert_present "$lock" "contended teardown removed another action's lock"
  [ ! -s "$dir/runtime.log" ] \
    || fail "contended teardown reached the runtime: $(cat "$dir/runtime.log")"
  assert_contains "$(cat "$dir/stderr")" "another lifecycle action is already running" \
    "contended teardown should serialize before reading mutable task metadata"
  kill "$holder" 2>/dev/null || true
  wait "$holder" 2>/dev/null || true
  pass "fm-teardown: a concurrent lifecycle action refuses before mutation"
}

test_non_pool_teardown_ignores_task_set_lock() {
  local dir id=non-pool-task lock ready holder i=0
  dir=$(make_case non-pool-task-set-lock)
  fm_write_meta "$dir/home/state/$id.meta" \
    "window=isolated:fm-$id" "endpoint_task_id=$id" \
    "worktree=$dir/missing-worktree" "project=$dir/project" "kind=scout"
  lock="$dir/home/state/.task-set.lock"
  ready="$dir/task-set-lock-ready"
  (
    # shellcheck source=/dev/null
    . "$ROOT/bin/fm-wake-lib.sh"
    fm_lock_try_acquire "$lock" || exit 1
    trap 'fm_lock_release "$lock"' EXIT
    : > "$ready"
    sleep 30
  ) &
  holder=$!
  while [ ! -e "$ready" ] && [ "$i" -lt 100 ]; do
    sleep 0.1
    i=$((i + 1))
  done
  [ -e "$ready" ] || {
    kill "$holder" 2>/dev/null || true
    wait "$holder" 2>/dev/null || true
    fail "could not stage an in-progress task publication"
  }

  run_case "$dir" "$id" > "$dir/stdout" 2> "$dir/stderr" \
    || fail "non-pool teardown was blocked by an unrelated task publication: $(cat "$dir/stderr")"
  assert_absent "$dir/home/state/$id.meta" "non-pool teardown left task metadata"
  assert_present "$lock" "non-pool teardown removed the publisher's lock"
  kill "$holder" 2>/dev/null || true
  wait "$holder" 2>/dev/null || true
  pass "fm-teardown: non-pool cleanup ignores unrelated task publication locks"
}

test_metadata_lock_serializes_destructive_cleanup() {
  local dir id=metadata-locked-task lock ready release holder teardown_pid i=0 rc
  dir=$(make_case metadata-lock)
  fm_write_meta "$dir/home/state/$id.meta" \
    "window=isolated:fm-$id" "endpoint_task_id=$id" \
    "worktree=$dir/worktree" "project=$dir/project" "kind=scout"
  lock="$dir/home/state/.meta-$id.lock"
  ready="$dir/meta-lock-ready"
  release="$dir/meta-lock-release"
  (
    # shellcheck source=/dev/null
    . "$ROOT/bin/fm-wake-lib.sh"
    fm_lock_try_acquire "$lock" || exit 1
    trap 'fm_lock_release "$lock"' EXIT
    : > "$ready"
    while [ ! -e "$release" ]; do
      sleep 0.01
    done
  ) &
  holder=$!
  while [ ! -e "$ready" ] && [ "$i" -lt 100 ]; do
    sleep 0.1
    i=$((i + 1))
  done
  [ -e "$ready" ] || {
    kill "$holder" 2>/dev/null || true
    wait "$holder" 2>/dev/null || true
    fail "could not stage a held metadata lock"
  }

  run_case "$dir" "$id" > "$dir/stdout" 2> "$dir/stderr" &
  teardown_pid=$!
  sleep 0.2
  if ! kill -0 "$teardown_pid" 2>/dev/null; then
    : > "$release"
    wait "$holder" 2>/dev/null || true
    wait "$teardown_pid" 2>/dev/null || true
    fail "teardown did not wait for the shared metadata writer lock"
  fi
  assert_present "$dir/home/state/$id.meta" "metadata-lock contention removed task metadata"
  assert_present "$dir/worktree/sentinel" "metadata-lock contention changed the worktree"
  [ ! -s "$dir/runtime.log" ] \
    || fail "metadata-lock contention reached the runtime: $(cat "$dir/runtime.log")"

  : > "$release"
  wait "$holder" || fail "metadata lock holder failed"
  wait "$teardown_pid"; rc=$?
  expect_code 0 "$rc" "teardown should complete after the metadata writer releases"
  assert_absent "$dir/home/state/$id.meta" \
    "serialized teardown left a task record that a completed writer could resurrect"
  pass "fm-teardown: destructive cleanup serializes with metadata writers"
}

test_supported_backend_endpoint_records_validate() {
  local dir id backend target
  dir=$(make_case valid-backends)
  # shellcheck source=/dev/null
  . "$ROOT/bin/fm-backend.sh"

  id=tmux-task
  fm_write_meta "$dir/home/state/$id.meta" \
    "window=firstmate:fm-$id" "worktree=$dir/worktree" "project=$dir/project"
  fm_backend_validate_task_endpoint "$dir/home/state/$id.meta" "$id" || fail "valid tmux endpoint refused"
  [ "$FM_BACKEND_VALIDATED_BACKEND:$FM_BACKEND_VALIDATED_TARGET" = "tmux:firstmate:fm-$id" ] || fail "tmux endpoint validation returned wrong identity"

  id=tmux-spaced-session
  fm_write_meta "$dir/home/state/$id.meta" \
    "window=team work:fm-$id" "worktree=$dir/worktree" "project=$dir/project"
  fm_backend_validate_task_endpoint "$dir/home/state/$id.meta" "$id" || fail "valid tmux endpoint with a spaced session name refused"
  [ "$FM_BACKEND_VALIDATED_TARGET" = "team work:fm-$id" ] || fail "tmux validation changed the spaced session identity"

  id=herdr-task
  fm_write_meta "$dir/home/state/$id.meta" \
    "window=lab:w1:p2" "endpoint_task_id=$id" "worktree=$dir/worktree" "project=$dir/project" \
    "backend=herdr" "herdr_session=lab" "herdr_workspace_id=w1" "herdr_tab_id=w1:t2" "herdr_pane_id=w1:p2"
  fm_backend_validate_task_endpoint "$dir/home/state/$id.meta" "$id" || fail "valid Herdr endpoint refused"

  # Removed backends (zellij, orca, cmux) have no adapter. Their records are
  # refused as an unknown backend identity so teardown preserves them rather
  # than sourcing a deleted adapter file.
  for id in zellij-task orca-task cmux-task; do
    case "$id" in
      zellij-task)
        fm_write_meta "$dir/home/state/$id.meta" \
          "window=lab:7" "endpoint_task_id=$id" "worktree=$dir/worktree" "project=$dir/project" \
          "backend=zellij" "zellij_session=lab" "zellij_tab_id=3" "zellij_pane_id=7"
        ;;
      orca-task)
        fm_write_meta "$dir/home/state/$id.meta" \
          "window=fm-$id" "endpoint_task_id=$id" "terminal=term-7" \
          "worktree=$dir/worktree" "project=$dir/project" "backend=orca" "orca_worktree_id=worktree-9::/orca/worktree-9"
        ;;
      cmux-task)
        fm_write_meta "$dir/home/state/$id.meta" \
          "window=workspace-1:surface-2" "endpoint_task_id=$id" "worktree=$dir/worktree" "project=$dir/project" \
          "backend=cmux" "cmux_workspace_id=workspace-1" "cmux_surface_id=surface-2"
        ;;
    esac
    if fm_backend_validate_task_endpoint "$dir/home/state/$id.meta" "$id" 2>"$dir/refuse.err"; then
      fail "removed-backend endpoint $id was accepted"
    fi
    assert_contains "$(cat "$dir/refuse.err")" "unknown backend" "removed-backend endpoint $id was not refused as an unknown backend"
  done

  for backend in tmux herdr zellij orca cmux; do
    set +e
    fm_backend_kill "$backend" "" >/dev/null 2>&1
    target=$?
    set -e
    [ "$target" -ne 0 ] || fail "$backend generic kill accepted an empty target"
  done
  pass "cleanup identity: valid tmux and Herdr records validate, removed-backend records are refused, and every empty backend target refuses"
}

test_tmux_empty_target_refuses_without_invocation() {
  local dir rc
  dir=$(make_case direct-empty)
  set +e
  FM_RUNTIME_LOG="$dir/runtime.log" PATH="$dir/fakebin:$PATH" \
    bash -c '. "$1/bin/fm-backend.sh"; fm_backend_source tmux; fm_backend_tmux_kill ""' _ "$ROOT" \
    > "$dir/stdout" 2> "$dir/stderr"
  rc=$?
  set -e
  [ "$rc" -ne 0 ] || fail "direct empty tmux target unexpectedly succeeded"
  [ ! -s "$dir/runtime.log" ] || fail "direct empty tmux target invoked tmux"
  pass "tmux backend: direct empty target returns nonzero without invoking tmux"
}

test_recorded_process_identity_cleanup_is_exact() {
  local dir target_pid control_pid target_record control_record live_command
  dir=$(make_case recorded-process)
  sleep 30 &
  control_pid=$!
  sleep 30 &
  target_pid=$!
  printf '%s\n' "$control_pid" > "$dir/control.pid"
  printf '%s\n' "$target_pid" > "$dir/target.pid"
  target_record=$(cat "$dir/target.pid")
  control_record=$(cat "$dir/control.pid")
  [ "$target_record" = "$target_pid" ] && [ "$control_record" = "$control_pid" ] \
    || fail "recorded process identity changed before cleanup"
  live_command=$(ps -p "$target_record" -o comm= 2>/dev/null | tr -d '[:space:]')
  case "$live_command" in sleep) ;; *) fail "recorded target pid no longer belongs to the expected child" ;; esac
  kill -TERM "$target_record"
  wait "$target_record" 2>/dev/null || true
  kill -0 "$target_record" 2>/dev/null && fail "exact target pid survived cleanup"
  kill -0 "$control_record" 2>/dev/null || fail "independent control process was disturbed"
  kill -TERM "$control_record"
  wait "$control_record" 2>/dev/null || true
  pass "process cleanup: creation-time PID identity removes only the exact child and preserves the control child"
}

isolated_tmux_window_exists() {  # <dir> <socket> <session> <window>
  ( cd "$1" && "$REAL_TMUX" -S "$2" list-windows -t "$3" -F '#{window_name}' 2>/dev/null ) \
    | grep -Fqx "$4"
}

test_isolated_tmux_invalid_and_valid_cleanup() {
  local dir socket socket_id session='endpoint safety' target_id=target control=control target=fm-target
  local prefix_target=fm-prefix prefix_survivor=fm-prefix2 rc
  [ -n "$REAL_TMUX" ] || { echo "skip - tmux not installed"; return 0; }
  dir=$(make_case isolated-real)
  socket=dedicated.sock
  socket_id="$dir/$socket"
  ( cd "$dir" && env -u TMUX -u TMUX_PANE "$REAL_TMUX" -S "$socket" new-session -d -s "$session" -n "$control" )
  ( cd "$dir" && env -u TMUX -u TMUX_PANE "$REAL_TMUX" -S "$socket" new-window -d -t "$session:" -n "$target" )
  printf '%s\n' "$socket_id" > "$dir/socket.identity"
  cat > "$dir/fakebin/tmux" <<SH
#!/usr/bin/env bash
set -eu
[ -z "\${TMUX:-}" ] && [ -z "\${TMUX_PANE:-}" ] || exit 91
[ "\${FM_TEST_TMUX_SOCKET:-}" = '$socket_id' ] || exit 92
[ "\$(cat '$dir/socket.identity')" = '$socket_id' ] || exit 93
printf 'tmux' >> "\${FM_RUNTIME_LOG:?}"
printf ' <%s>' "\$@" >> "\${FM_RUNTIME_LOG:?}"
printf '\n' >> "\${FM_RUNTIME_LOG:?}"
cd '$dir'
exec '$REAL_TMUX' -S '$socket' "\$@"
SH
  chmod +x "$dir/fakebin/tmux"

  fm_write_meta "$dir/home/state/invalid.meta" \
    "window=" "worktree=$dir/worktree" "project=$dir/project" "kind=scout"
  set +e
  env -u TMUX -u TMUX_PANE FM_TEST_TMUX_SOCKET="$socket_id" \
    FM_HOME="$dir/home" FM_ROOT_OVERRIDE="$ROOT" FM_RUNTIME_LOG="$dir/runtime.log" \
    PATH="$dir/fakebin:$PATH" "$TEARDOWN" invalid --force \
    > "$dir/invalid.out" 2> "$dir/invalid.err"
  rc=$?
  set -e
  [ "$rc" -ne 0 ] || fail "isolated invalid endpoint unexpectedly succeeded"
  [ ! -s "$dir/runtime.log" ] || fail "isolated invalid endpoint reached tmux"
  isolated_tmux_window_exists "$dir" "$socket" "$session" "$control" || fail "invalid cleanup removed control window"
  isolated_tmux_window_exists "$dir" "$socket" "$session" "$target" || fail "invalid cleanup removed target window"

  set +e
  # shellcheck disable=SC2016 # $1 expands inside the isolated child shell.
  env -u TMUX -u TMUX_PANE FM_TEST_TMUX_SOCKET="$socket_id" FM_RUNTIME_LOG="$dir/runtime.log" \
    PATH="$dir/fakebin:$PATH" bash -c \
    '. "$1/bin/fm-backend.sh"; fm_backend_source tmux; fm_backend_tmux_kill ""' _ "$ROOT" \
    > "$dir/empty.out" 2> "$dir/empty.err"
  rc=$?
  set -e
  [ "$rc" -ne 0 ] || fail "isolated direct empty target unexpectedly succeeded"
  [ ! -s "$dir/runtime.log" ] || fail "isolated direct empty target reached tmux"
  isolated_tmux_window_exists "$dir" "$socket" "$session" "$control" || fail "direct empty cleanup removed control window"
  isolated_tmux_window_exists "$dir" "$socket" "$session" "$target" || fail "direct empty cleanup removed target window"

  ( cd "$dir" && env -u TMUX -u TMUX_PANE "$REAL_TMUX" -S "$socket" new-window -d -t "=$session:" -n "$prefix_survivor" )
  # shellcheck disable=SC2016 # $1 and $2 expand inside the isolated child shell.
  env -u TMUX -u TMUX_PANE FM_TEST_TMUX_SOCKET="$socket_id" FM_RUNTIME_LOG="$dir/runtime.log" \
    PATH="$dir/fakebin:$PATH" bash -c \
    '. "$1/bin/fm-backend.sh"; fm_backend_source tmux; fm_backend_tmux_kill "$2"' _ "$ROOT" "$session:$prefix_target"
  isolated_tmux_window_exists "$dir" "$socket" "$session" "$prefix_survivor" \
    || fail "missing exact target cleanup removed its prefix-matched neighbor"

  fm_write_meta "$dir/home/state/$target_id.meta" \
    "window=$session:$target" "endpoint_task_id=$target_id" \
    "worktree=$dir/nonexistent-worktree" "project=$dir/nonexistent-project" \
    "kind=scout" "mode=no-mistakes"
  env -u TMUX -u TMUX_PANE FM_TEST_TMUX_SOCKET="$socket_id" \
    FM_HOME="$dir/home" FM_ROOT_OVERRIDE="$ROOT" FM_RUNTIME_LOG="$dir/runtime.log" \
    PATH="$dir/fakebin:$PATH" "$TEARDOWN" "$target_id" --force \
    > "$dir/valid.out" 2> "$dir/valid.err" \
    || fail "isolated valid endpoint teardown failed: $(cat "$dir/valid.err")"
  isolated_tmux_window_exists "$dir" "$socket" "$session" "$target" \
    && fail "valid cleanup did not remove the exact target window"
  isolated_tmux_window_exists "$dir" "$socket" "$session" "$control" \
    || fail "valid cleanup removed the independent control window"
  grep -Fqx "tmux <kill-window> <-t> <=$session:=$target>" "$dir/runtime.log" \
    || fail "valid cleanup did not invoke exactly the recorded target: $(cat "$dir/runtime.log")"

  ( cd "$dir" && env -u TMUX -u TMUX_PANE "$REAL_TMUX" -S "$socket" kill-server 2>/dev/null ) || true
  pass "fm-teardown: exact tmux cleanup preserves invalid and prefix-matched neighbors while removing only the recorded target"
}

test_bare_relative_origin_shares_project_lock_with_clone() {
  local dir second_project primary_lock clone_lock
  dir=$(make_case bare-relative-origin-lock)
  git -C "$dir/project" -c user.name=test -c user.email=test@example.invalid \
    commit --allow-empty -qm lock-fixture
  mkdir -p "$dir/project/remotes"
  git clone -q --bare "$dir/project" "$dir/project/remotes/origin.git"
  git -C "$dir/project" remote add origin remotes/origin.git
  second_project="$dir/second-project"
  git clone -q "$dir/project/remotes/origin.git" "$second_project"

  primary_lock=$(FM_HOME="$dir/home" bash -c \
    '. "$1"; fm_treehouse_project_lock_path "$2"' _ \
    "$ROOT/bin/fm-wake-lib.sh" "$dir/project") \
    || fail "could not resolve the primary project's bare-origin lock"
  clone_lock=$(FM_HOME="$dir/home" bash -c \
    '. "$1"; fm_treehouse_project_lock_path "$2"' _ \
    "$ROOT/bin/fm-wake-lib.sh" "$second_project") \
    || fail "could not resolve the clone project's absolute-origin lock"
  [ "$primary_lock" = "$clone_lock" ] \
    || fail "bare and absolute forms of the same local origin resolved different project locks"

  pass "Treehouse locking resolves a bare local origin against its source project, matching the provisioned clone"
}

test_reused_pool_slot_refuses_before_touching_the_other_task() {
  local dir id=stale-task other=live-task worker rc

  dir=$(make_case slot-reuse)
  mark_case_as_treehouse_pool "$dir"
  # The reuse collision: the pool slot recorded for a finished task has already
  # been handed to another task, whose worker is live in it right now.
  fm_write_meta "$dir/home/state/$id.meta" \
    "window=firstmate:fm-$id" "endpoint_task_id=$id" \
    "worktree=$dir/worktree" "project=$dir/project" "kind=scout"
  fm_write_meta "$dir/home/state/$other.meta" \
    "window=firstmate:fm-$other" "endpoint_task_id=$other" \
    "worktree=$dir/worktree" "project=$dir/project" "kind=scout"
  # Staged in this shell, not a command substitution: a background child of a
  # $(...) subshell does not outlive it, and the point of this worker is to be
  # alive in the slot while teardown runs.
  ( cd "$dir/worktree" && exec sleep 30 ) &
  worker=$!

  set +e
  run_case "$dir" "$id" > "$dir/stdout" 2> "$dir/stderr"
  rc=$?
  set -e

  [ "$rc" -ne 0 ] || fail "teardown returned a pool slot a second task record still holds"
  kill -0 "$worker" 2>/dev/null || fail "teardown killed the worker holding the reused pool slot"
  assert_present "$dir/worktree/sentinel" "teardown reset a pool slot a second task record still holds"
  assert_present "$dir/home/state/$other.meta" "teardown removed the live task's record"
  assert_present "$dir/home/state/$id.meta" "teardown removed the stale task's record before refusing"
  [ ! -s "$dir/runtime.log" ] \
    || fail "teardown reached the runtime on a contested pool slot: $(cat "$dir/runtime.log")"
  assert_contains "$(cat "$dir/stderr")" "$other" \
    "refusal should name the other task holding the slot"
  kill "$worker" 2>/dev/null || true
  wait "$worker" 2>/dev/null || true

  # The same collision recorded on a secondmate home field rather than a task
  # worktree is the same slot, and refuses the same way.
  dir=$(make_case slot-reuse-home)
  mark_case_as_treehouse_pool "$dir"
  fm_write_meta "$dir/home/state/$id.meta" \
    "window=firstmate:fm-$id" "endpoint_task_id=$id" \
    "worktree=$dir/worktree" "project=$dir/project" "kind=scout"
  fm_write_meta "$dir/home/state/$other.meta" \
    "window=firstmate:fm-$other" "endpoint_task_id=$other" \
    "worktree=$dir/worktree" "home=$dir/worktree" \
    "project=$dir/project" "kind=secondmate"
  set +e
  run_case "$dir" "$id" > "$dir/stdout" 2> "$dir/stderr"
  rc=$?
  set -e
  [ "$rc" -ne 0 ] || fail "teardown returned a pool slot a secondmate home record still holds"
  assert_present "$dir/worktree/sentinel" "teardown reset a pool slot a secondmate home record still holds"
  assert_present "$dir/home/state/$other.meta" "teardown removed the secondmate record"
  [ ! -s "$dir/runtime.log" ] \
    || fail "teardown reached the runtime on a slot held by a secondmate home: $(cat "$dir/runtime.log")"

  # A second task record that is a hardlink of this one is still a second
  # claim on the slot, not this record reached through another spelling.
  dir=$(make_case slot-reuse-hardlink)
  mark_case_as_treehouse_pool "$dir"
  fm_write_meta "$dir/home/state/$id.meta" \
    "window=firstmate:fm-$id" "endpoint_task_id=$id" \
    "worktree=$dir/worktree" "project=$dir/project" "kind=scout"
  ln "$dir/home/state/$id.meta" "$dir/home/state/$other.meta"
  set +e
  run_case "$dir" "$id" > "$dir/stdout" 2> "$dir/stderr"
  rc=$?
  set -e
  [ "$rc" -ne 0 ] || fail "teardown returned a pool slot a hardlinked second task record still holds"
  assert_present "$dir/worktree/sentinel" "teardown reset a pool slot a hardlinked second task record still holds"
  assert_contains "$(cat "$dir/stderr")" "$other" \
    "hardlink refusal should name the other task record"

  pass "fm-teardown: a pool slot named by a second task record is never returned, killed, or reset"
}

test_cross_home_pool_slot_collision_refuses() {
  local dir id=stale-task other=secondmate-task second_home second_project rc
  dir=$(make_case slot-reuse-cross-home)
  mark_case_as_treehouse_pool "$dir"
  printf 'fixture\n' > "$dir/project/tracked"
  git -C "$dir/project" add tracked
  git -C "$dir/project" -c user.name=test -c user.email=test@example.invalid commit -qm fixture
  second_home="$dir/secondmate-home"
  second_project="$second_home/projects/project"
  mkdir -p "$second_home/projects" "$second_home/state" "$second_home/data"
  git clone -q "$dir/project" "$second_project"
  printf '%s\n' "- mate - fixture (home: $second_home; scope: test; projects: project; added 2026-01-01)" \
    > "$dir/home/data/secondmates.md"
  fm_write_meta "$dir/home/state/$id.meta" \
    "window=firstmate:fm-$id" "endpoint_task_id=$id" \
    "worktree=$dir/worktree" "project=$dir/project" "kind=scout"
  fm_write_meta "$second_home/state/$other.meta" \
    "window=firstmate:fm-$other" "endpoint_task_id=$other" \
    "worktree=$dir/worktree" "project=$second_project" "kind=scout"

  set +e
  run_case "$dir" "$id" > "$dir/stdout" 2> "$dir/stderr"
  rc=$?
  set -e
  [ "$rc" -ne 0 ] || fail "teardown returned a pool slot held by another firstmate home"
  assert_present "$dir/home/state/$id.meta" "cross-home collision removed stale metadata"
  assert_present "$second_home/state/$other.meta" "cross-home collision removed live metadata"
  assert_present "$dir/worktree/sentinel" "cross-home collision reset the shared slot"
  [ ! -s "$dir/runtime.log" ] \
    || fail "cross-home collision reached the runtime: $(cat "$dir/runtime.log")"
  assert_contains "$(cat "$dir/stderr")" "$other" \
    "cross-home refusal should name the task holding the slot"
  pass "fm-teardown: a pool slot held by another firstmate home is never returned"
}

test_sole_slot_record_still_tears_down() {
  local dir id=sole-task worker

  dir=$(make_case slot-sole)
  mark_case_as_treehouse_pool "$dir"
  fm_write_meta "$dir/home/state/$id.meta" \
    "window=firstmate:fm-$id" "endpoint_task_id=$id" \
    "worktree=$dir/worktree" "project=$dir/project" "kind=scout"
  # A neighbouring task on its OWN slot must not look like a collision.
  mkdir -p "$dir/other-worktree"
  fm_write_meta "$dir/home/state/neighbour.meta" \
    "window=firstmate:fm-neighbour" "endpoint_task_id=neighbour" \
    "worktree=$dir/other-worktree" "project=$dir/project" "kind=scout"
  ( cd "$dir/other-worktree" && exec sleep 30 ) &
  worker=$!

  run_case "$dir" "$id" > "$dir/stdout" 2> "$dir/stderr" \
    || fail "teardown of a task that solely holds its slot failed: $(cat "$dir/stderr")"
  assert_absent "$dir/home/state/$id.meta" "uncontested teardown left the task record"
  assert_present "$dir/home/state/neighbour.meta" "uncontested teardown removed the neighbour's record"
  kill -0 "$worker" 2>/dev/null || fail "uncontested teardown killed a worker in a different slot"
  grep -Fq "treehouse <return>" "$dir/runtime.log" \
    || fail "uncontested teardown did not return its own pool slot: $(cat "$dir/runtime.log")"
  kill "$worker" 2>/dev/null || true
  wait "$worker" 2>/dev/null || true
  pass "fm-teardown: a task that solely holds its slot still returns it"
}

test_recorded_endpoint_that_changed_directory_still_tears_down() {
  local dir id=moved-task

  dir=$(make_case slot-endpoint-moved)
  mark_case_as_treehouse_pool "$dir"
  mkdir -p "$dir/other-directory"
  # The exact recorded worker may legitimately cd outside its worktree. Its
  # endpoint identity still owns the lifecycle; cwd alone must not brick it.
  cat > "$dir/fakebin/tmux" <<SH
#!/usr/bin/env bash
if [ "\${1:-}" = display-message ]; then
  printf '%s\n' '$dir/other-directory'
  exit 0
fi
printf 'tmux' >> "\${FM_RUNTIME_LOG:?}"
printf ' <%s>' "\$@" >> "\${FM_RUNTIME_LOG:?}"
printf '\n' >> "\${FM_RUNTIME_LOG:?}"
exit 0
SH
  chmod +x "$dir/fakebin/tmux"
  fm_write_meta "$dir/home/state/$id.meta" \
    "window=firstmate:fm-$id" "endpoint_task_id=$id" \
    "worktree=$dir/worktree" "project=$dir/project" "kind=scout"

  run_case "$dir" "$id" > "$dir/stdout" 2> "$dir/stderr" \
    || fail "teardown refused its recorded endpoint after it changed directory: $(cat "$dir/stderr")"
  assert_absent "$dir/home/state/$id.meta" "moved-endpoint teardown left the task record"
  grep -Fq "tmux <kill-window> <-t> <=firstmate:=fm-$id>" "$dir/runtime.log" \
    || fail "moved-endpoint teardown did not stop the exact recorded worker: $(cat "$dir/runtime.log")"
  grep -Fq "treehouse <return>" "$dir/runtime.log" \
    || fail "moved-endpoint teardown did not return its uncontested pool slot: $(cat "$dir/runtime.log")"

  pass "fm-teardown: an exact recorded endpoint still tears down after changing cwd outside its worktree"
}

# --- Treehouse project-lock anchoring across home layouts --------------------
#
# The lock is anchored at the local root home, so every home on this machine
# that can reach the same pool must derive the identical file.

write_local_parent_record() {  # <home> <parent-home>
  cat > "$1/.fm-secondmate-parent" <<REC
schema=fm-secondmate-parent.v1
route=local
parent_home=$2
REC
}

make_home() {  # <path>
  mkdir -p "$1/state" "$1/data" "$1/config" "$1/projects"
}

resolve_project_lock() {  # <home> <project>
  FM_HOME="$1" bash -c '. "$1"; fm_treehouse_project_lock_path "$2"' _ \
    "$ROOT/bin/fm-wake-lib.sh" "$2"
}

test_project_lock_anchors_at_the_local_root_across_home_layouts() {
  local dir main_home main_project local_mate main_lock mate_lock orphan_lock rc
  dir=$(make_case project-lock-anchoring)
  git -C "$dir/project" -c user.name=test -c user.email=test@example.invalid \
    commit --allow-empty -qm anchor-fixture

  # Main-home layout: a root home and a local secondmate beneath it.
  main_home="$dir/home"
  main_project="$main_home/projects/project"
  make_home "$main_home"
  git clone -q "$dir/project" "$main_project"
  local_mate="$dir/local-mate"
  make_home "$local_mate"
  write_local_parent_record "$local_mate" "$main_home"
  git clone -q "$dir/project" "$local_mate/projects/project"

  main_lock=$(resolve_project_lock "$main_home" "$main_project") \
    || fail "the root home could not resolve its project lock"
  mate_lock=$(resolve_project_lock "$local_mate" "$local_mate/projects/project") \
    || fail "a local secondmate home could not resolve its project lock"

  [ "$main_lock" = "$mate_lock" ] \
    || fail "the root home and its local secondmate derived different project locks"
  case "$main_lock" in
    "$main_home/state/"*) ;;
    *) fail "the root home anchored its project lock outside its own state: $main_lock" ;;
  esac

  # An origin-less local-only project still resolves, keyed on its worktree top.
  git init -q "$local_mate/projects/local-only"
  orphan_lock=$(resolve_project_lock "$local_mate" "$local_mate/projects/local-only") \
    || fail "an origin-less local-only project could not resolve its lock in a local secondmate home"
  [ "$orphan_lock" != "$mate_lock" ] \
    || fail "an origin-less project shared the lock identity of an unrelated origin"

  # Any parent route other than local still fails closed.
  printf 'schema=fm-secondmate-parent.v1\nroute=sideways\n' \
    > "$local_mate/.fm-secondmate-parent"
  set +e
  resolve_project_lock "$local_mate" "$local_mate/projects/project" >/dev/null 2>&1
  rc=$?
  set -e
  [ "$rc" -ne 0 ] || fail "an unsupported parent route resolved a project lock instead of refusing"

  pass "Treehouse project locking anchors at the local root for main-home and local-secondmate layouts"
}

# The slot-reuse sequence with only ONE discoverable record: the finished task's
# worker exited, its slot was granted to another task, and that task leaves no
# record this home can enumerate. Nothing in the record scan contradicts the
# stale worktree= line, so the slot's own owner claim is the only evidence that
# it was reassigned. The slot is no longer this task's, so teardown finishes the
# task's own cleanup and leaves the slot - its worker, its copy, its claim -
# exactly as it found it.
assert_reassigned_slot_left_alone() {  # <case> <id> <other> <description>
  local dir=$1 id=$2 other=$3 description=$4
  assert_absent "$dir/home/state/$id.meta" "$description: the stale task's own record was not removed"
  assert_present "$dir/pool/1/.fm-slot-owner" "$description: another task's slot claim was removed"
  assert_contains "$(cat "$dir/pool/1/.fm-slot-owner")" "task=$other" \
    "$description: another task's slot claim was rewritten"
  assert_present "$dir/pool/1/project/.git" "$description: the reassigned slot's checkout was removed"
  ! grep -Fq "treehouse <return>" "$dir/runtime.log" \
    || fail "$description: the reassigned slot was returned to the pool: $(cat "$dir/runtime.log")"
  assert_contains "$(cat "$dir/stderr")" "$other" \
    "$description: the warning should name the task the slot was reassigned to"
  assert_contains "$(cat "$dir/stderr")" "reassigned" \
    "$description: the warning should name the reassignment as the cause"
}

test_reassigned_pool_slot_finishes_own_cleanup_without_touching_the_slot() {
  local dir id=stale-task other=reassigned-task worker rc

  # Dirty slot, --force, and a live worker inside it: --force authorizes
  # discarding this task's unlanded work, which is already gone with the slot,
  # never the other task's live work.
  dir=$(make_case slot-reassigned)
  mark_case_as_treehouse_pool "$dir"
  fm_write_meta "$dir/home/state/$id.meta" \
    "window=firstmate:fm-$id" "endpoint_task_id=$id" \
    "worktree=$dir/worktree" "project=$dir/project" "kind=scout"
  claim_pool_slot "$dir" "$other" "$dir/other-home"
  # Staged in this shell, not a command substitution: a background child of a
  # $(...) subshell does not outlive it, and the point of this worker is to be
  # alive in the slot while teardown runs.
  ( cd "$dir/worktree" && exec sleep 30 ) &
  worker=$!

  set +e
  run_case "$dir" "$id" > "$dir/stdout" 2> "$dir/stderr"
  rc=$?
  set -e

  [ "$rc" -eq 0 ] || fail "teardown of a task whose slot was reassigned failed: $(cat "$dir/stderr")"
  kill -0 "$worker" 2>/dev/null || fail "teardown killed the worker holding the reassigned pool slot"
  assert_present "$dir/worktree/sentinel" "teardown reset a pool slot another task had claimed"
  assert_reassigned_slot_left_alone "$dir" "$id" "$other" "dirty reassigned slot with --force"
  assert_contains "$(cat "$dir/stderr")" "$dir/other-home" \
    "the warning should name the claimant's home"
  kill "$worker" 2>/dev/null || true
  wait "$worker" 2>/dev/null || true

  # The same reassignment on a CLEAN slot: a landed ship task torn down without
  # --force, which is the shape of the real incident. A clean, fully landed copy
  # passes every unlanded-work check, so only the ownership determination can
  # keep this slot out of the pool; a guard keyed off dirtiness would return it
  # and destroy the live task's copy.
  dir=$(make_case slot-reassigned-clean)
  mark_case_as_treehouse_pool "$dir"
  rm -f "$dir/worktree/sentinel"
  [ -z "$(git -C "$dir/worktree" status --porcelain)" ] \
    || fail "clean-slot fixture is not clean: $(git -C "$dir/worktree" status --porcelain)"
  fm_write_meta "$dir/home/state/$id.meta" \
    "window=firstmate:fm-$id" "endpoint_task_id=$id" \
    "worktree=$dir/worktree" "project=$dir/project" "kind=ship"
  claim_pool_slot "$dir" "$other" "$dir/other-home"
  ( cd "$dir/worktree" && exec sleep 30 ) &
  worker=$!

  set +e
  FM_HOME="$dir/home" FM_ROOT_OVERRIDE="$ROOT" \
  FM_RUNTIME_LOG="$dir/runtime.log" PATH="$dir/fakebin:$PATH" \
    "$TEARDOWN" "$id" > "$dir/stdout" 2> "$dir/stderr"
  rc=$?
  set -e
  [ "$rc" -eq 0 ] || fail "teardown of a clean ship task whose slot was reassigned failed: $(cat "$dir/stderr")"
  kill -0 "$worker" 2>/dev/null || fail "teardown killed the worker holding the clean reassigned pool slot"
  assert_reassigned_slot_left_alone "$dir" "$id" "$other" "clean reassigned slot without --force"
  kill "$worker" 2>/dev/null || true
  wait "$worker" 2>/dev/null || true

  # A claim that exists but cannot be read as a claim proves nothing either way,
  # so it refuses rather than guessing the slot is still this task's.
  dir=$(make_case slot-claim-unreadable)
  mark_case_as_treehouse_pool "$dir"
  fm_write_meta "$dir/home/state/$id.meta" \
    "window=firstmate:fm-$id" "endpoint_task_id=$id" \
    "worktree=$dir/worktree" "project=$dir/project" "kind=scout"
  printf 'not-a-claim\n' > "$dir/pool/1/.fm-slot-owner"

  set +e
  run_case "$dir" "$id" > "$dir/stdout" 2> "$dir/stderr"
  rc=$?
  set -e
  [ "$rc" -ne 0 ] || fail "teardown returned a pool slot whose claim could not be read"
  assert_present "$dir/worktree/sentinel" "teardown reset a pool slot whose claim could not be read"
  assert_present "$dir/pool/1/.fm-slot-owner" "teardown removed an unreadable slot claim"
  assert_present "$dir/home/state/$id.meta" "teardown removed the task record on an unreadable claim"
  [ ! -s "$dir/runtime.log" ] \
    || fail "teardown reached the runtime on an unreadable slot claim: $(cat "$dir/runtime.log")"
  assert_contains "$(cat "$dir/stderr")" "$dir/pool/1/.fm-slot-owner" \
    "unreadable-claim refusal should name the claim file to inspect"

  pass "fm-teardown: a pool slot claimed by another task is left alone while the task's own cleanup finishes"
}

# The reuse collision where BOTH records survive: the stale task's record still
# names the slot the pool handed on, and the claimant's own record names it too.
# The claim proves the stale record's teardown is records-only, so the record
# scan must not refuse it; once it is gone, the claimant tears down normally.
test_stale_record_on_claimed_slot_retires_then_claimant_tears_down() {
  local dir id=stale-task other=live-task rc

  dir=$(make_case slot-reassigned-both-records)
  mark_case_as_treehouse_pool "$dir"
  fm_write_meta "$dir/home/state/$id.meta" \
    "window=firstmate:fm-$id" "endpoint_task_id=$id" \
    "worktree=$dir/worktree" "project=$dir/project" "kind=scout"
  fm_write_meta "$dir/home/state/$other.meta" \
    "window=firstmate:fm-$other" "endpoint_task_id=$other" \
    "worktree=$dir/worktree" "project=$dir/project" "kind=scout"
  claim_pool_slot "$dir" "$other"

  set +e
  run_case "$dir" "$id" > "$dir/stdout" 2> "$dir/stderr"
  rc=$?
  set -e
  [ "$rc" -eq 0 ] || fail "records-only teardown of a stale record on a claimed slot failed: $(cat "$dir/stderr")"
  assert_reassigned_slot_left_alone "$dir" "$id" "$other" "stale record beside the claimant's record"
  assert_present "$dir/worktree/sentinel" "records-only teardown reset the claimant's slot"
  assert_present "$dir/home/state/$other.meta" "records-only teardown removed the claimant's record"

  : > "$dir/runtime.log"
  run_case "$dir" "$other" > "$dir/stdout" 2> "$dir/stderr" \
    || fail "claimant teardown failed after the stale record retired: $(cat "$dir/stderr")"
  assert_absent "$dir/home/state/$other.meta" "claimant teardown left its record"
  assert_absent "$dir/pool/1/.fm-slot-owner" "claimant teardown left its spent slot claim behind"
  grep -Fq "treehouse <return>" "$dir/runtime.log" \
    || fail "claimant teardown did not return its pool slot: $(cat "$dir/runtime.log")"

  pass "fm-teardown: a stale record on a claimed slot retires, then the claimant tears down"
}

# The two states that must never become a false refusal: the task's own claim,
# and no claim at all (a slot taken before claims existed, or already returned).
test_own_and_absent_slot_claims_still_tear_down() {
  local dir id=owned-task

  dir=$(make_case slot-claim-own)
  mark_case_as_treehouse_pool "$dir"
  fm_write_meta "$dir/home/state/$id.meta" \
    "window=firstmate:fm-$id" "endpoint_task_id=$id" \
    "worktree=$dir/worktree" "project=$dir/project" "kind=scout"
  claim_pool_slot "$dir" "$id"

  run_case "$dir" "$id" > "$dir/stdout" 2> "$dir/stderr" \
    || fail "teardown of a task holding its own slot claim failed: $(cat "$dir/stderr")"
  assert_absent "$dir/home/state/$id.meta" "own-claim teardown left the task record"
  assert_absent "$dir/pool/1/.fm-slot-owner" "own-claim teardown left its spent slot claim behind"
  grep -Fq "treehouse <return>" "$dir/runtime.log" \
    || fail "own-claim teardown did not return its own pool slot: $(cat "$dir/runtime.log")"

  dir=$(make_case slot-claim-absent)
  mark_case_as_treehouse_pool "$dir"
  fm_write_meta "$dir/home/state/$id.meta" \
    "window=firstmate:fm-$id" "endpoint_task_id=$id" \
    "worktree=$dir/worktree" "project=$dir/project" "kind=scout"

  run_case "$dir" "$id" > "$dir/stdout" 2> "$dir/stderr" \
    || fail "teardown of an unclaimed slot failed: $(cat "$dir/stderr")"
  assert_absent "$dir/home/state/$id.meta" "unclaimed-slot teardown left the task record"
  grep -Fq "treehouse <return>" "$dir/runtime.log" \
    || fail "unclaimed-slot teardown did not return its pool slot: $(cat "$dir/runtime.log")"

  pass "fm-teardown: a task's own slot claim, and an unclaimed slot, both still tear down"
}

# The tmux shim used by the endpoint-close tests below: every subcommand
# reaches the real isolated server, so presence is always read from real tmux.
# When FM_TEST_BLOCK_KILL is set, `kill-window` alone fails without forwarding,
# which is a close that genuinely could not do its job - the recorded window is
# demonstrably still there afterwards. Real tmux cannot be made to accept a
# kill-window and leave the window alive, so blocking the call is the only way
# to reach that state against a real endpoint.
# When FM_TEST_UNREADABLE_LIST is set, `list-windows` fails with a response
# that is NOT one of tmux's definitive missing-session/server answers, which is
# the transient-server and tmux-absent-from-PATH shape: the read never happened,
# so it proves nothing about whether the window survived.
# The socket stays a RELATIVE name reached from <dir>, matching the isolated
# case above: this fixture's absolute path is longer than a unix socket path
# may be on macOS.
write_close_failing_tmux_shim() {  # <dir> <socket-name> <real-tmux>
  local dir=$1 socket=$2 real=$3
  cat > "$dir/fakebin/tmux" <<SH
#!/usr/bin/env bash
set -u
printf 'tmux' >> "\${FM_RUNTIME_LOG:?}"
printf ' <%s>' "\$@" >> "\${FM_RUNTIME_LOG:?}"
printf '\n' >> "\${FM_RUNTIME_LOG:?}"
if [ -n "\${FM_TEST_BLOCK_KILL:-}" ] && [ "\${1:-}" = kill-window ]; then
  echo "can't find window" >&2
  exit 1
fi
if [ -n "\${FM_TEST_UNREADABLE_LIST:-}" ] && [ "\${1:-}" = list-windows ]; then
  echo "lost server" >&2
  exit 1
fi
cd '$dir'
exec '$real' -S '$socket' "\$@"
SH
  chmod +x "$dir/fakebin/tmux"
}

# write_endpoint_close_meta: a task record whose worktree and project do not
# exist, which keeps the cases below on the endpoint close itself - the pool
# return and its own refusals are covered elsewhere in this file.
write_endpoint_close_meta() {  # <case-dir> <id> <window>
  fm_write_meta "$1/home/state/$2.meta" \
    "window=$3" "endpoint_task_id=$2" \
    "worktree=$1/nonexistent-worktree" "project=$1/nonexistent-project" \
    "kind=ship" "mode=no-mistakes"
}

test_failed_endpoint_close_refuses_before_removing_the_record() {
  local dir socket session='close failure' id=strand-task rc
  [ -n "$REAL_TMUX" ] || { echo "skip - tmux not installed"; return 0; }
  dir=$(make_case close-failure)
  socket=dedicated.sock
  ( cd "$dir" && env -u TMUX -u TMUX_PANE "$REAL_TMUX" -S "$socket" new-session -d -s "$session" -n control )
  ( cd "$dir" && env -u TMUX -u TMUX_PANE "$REAL_TMUX" -S "$socket" new-window -d -t "=$session:" -n "fm-$id" )
  write_close_failing_tmux_shim "$dir" "$socket" "$REAL_TMUX"
  isolated_tmux_window_exists "$dir" "$socket" "$session" "fm-$id" \
    || fail "fixture did not create the task window"

  write_endpoint_close_meta "$dir" "$id" "$session:fm-$id"

  set +e
  env -u TMUX -u TMUX_PANE FM_TEST_BLOCK_KILL=1 \
    FM_HOME="$dir/home" FM_ROOT_OVERRIDE="$ROOT" FM_RUNTIME_LOG="$dir/runtime.log" \
    PATH="$dir/fakebin:$PATH" "$TEARDOWN" "$id" \
    > "$dir/failed.out" 2> "$dir/failed.err"
  rc=$?
  set -e
  [ "$rc" -ne 0 ] || fail "teardown reported success after a close that failed: $(cat "$dir/failed.err")"
  assert_no_grep "teardown $id complete" "$dir/failed.out" \
    "teardown announced a completed cleanup after a close that failed"
  assert_grep "kill-window" "$dir/runtime.log" "teardown never attempted the recorded close"
  assert_grep "is still present after its close" "$dir/failed.err" \
    "the backend's own close failure was swallowed instead of reported"
  assert_grep "could not be closed" "$dir/failed.err" \
    "teardown did not refuse on the reported close failure"
  # The refusal exists so the endpoint is not STRANDED: the record is the only
  # thing naming what survived, so it has to outlive the refusal.
  assert_present "$dir/home/state/$id.meta" \
    "teardown deleted the only durable record naming an endpoint it could not close"
  # That retention is this run's, not a durable one - a task carrying a backlog
  # transition has the next session's pending-close replay remove the retained
  # record - so the refusal has to say so instead of sending the operator away
  # trusting it.
  assert_grep "not durable across a session start" "$dir/failed.err" \
    "the refusal promised a retention teardown does not own"
  isolated_tmux_window_exists "$dir" "$socket" "$session" "fm-$id" \
    || fail "the surviving endpoint disappeared, so this case no longer proves the hazard"
  isolated_tmux_window_exists "$dir" "$socket" "$session" control \
    || fail "the refused cleanup removed an independent window"

  # Same task, same records, with the close working again: the retained record
  # is what lets the rerun finish, so the refusal is recoverable, not terminal.
  env -u TMUX -u TMUX_PANE \
    FM_HOME="$dir/home" FM_ROOT_OVERRIDE="$ROOT" FM_RUNTIME_LOG="$dir/runtime.log" \
    PATH="$dir/fakebin:$PATH" "$TEARDOWN" "$id" \
    > "$dir/rerun.out" 2> "$dir/rerun.err" \
    || fail "the rerun after a recovered close still failed: $(cat "$dir/rerun.err")"
  assert_absent "$dir/home/state/$id.meta" "the recovered rerun left the task record behind"
  isolated_tmux_window_exists "$dir" "$socket" "$session" "fm-$id" \
    && fail "the recovered rerun did not close the recorded endpoint"
  isolated_tmux_window_exists "$dir" "$socket" "$session" control \
    || fail "the recovered rerun removed an independent window"

  ( cd "$dir" && env -u TMUX -u TMUX_PANE "$REAL_TMUX" -S "$socket" kill-server 2>/dev/null ) || true
  pass "fm-teardown: a close that genuinely failed refuses and keeps the record naming the surviving endpoint, and the same teardown finishes once the close works"
}

test_forced_teardown_continues_past_a_close_it_could_not_make() {
  local dir socket session='forced close failure' id=forced-task rc
  [ -n "$REAL_TMUX" ] || { echo "skip - tmux not installed"; return 0; }
  dir=$(make_case forced-close-failure)
  socket=dedicated.sock
  ( cd "$dir" && env -u TMUX -u TMUX_PANE "$REAL_TMUX" -S "$socket" new-session -d -s "$session" -n control )
  ( cd "$dir" && env -u TMUX -u TMUX_PANE "$REAL_TMUX" -S "$socket" new-window -d -t "=$session:" -n "fm-$id" )
  write_close_failing_tmux_shim "$dir" "$socket" "$REAL_TMUX"
  write_endpoint_close_meta "$dir" "$id" "$session:fm-$id"

  # Exactly the same case run twice, so the only difference is the operator's
  # explicit authority. Unforced it still refuses, which is what makes --force
  # an override rather than the absence of a gate.
  set +e
  env -u TMUX -u TMUX_PANE FM_TEST_BLOCK_KILL=1 \
    FM_HOME="$dir/home" FM_ROOT_OVERRIDE="$ROOT" FM_RUNTIME_LOG="$dir/runtime.log" \
    PATH="$dir/fakebin:$PATH" "$TEARDOWN" "$id" \
    > "$dir/unforced.out" 2> "$dir/unforced.err"
  rc=$?
  set -e
  [ "$rc" -ne 0 ] || fail "the unforced run did not refuse a close that failed: $(cat "$dir/unforced.err")"
  assert_present "$dir/home/state/$id.meta" "the unforced refusal removed the task record"
  grep -qF -- "--force" "$dir/unforced.err" \
    || fail "the refusal did not name the override that lets an operator through"

  env -u TMUX -u TMUX_PANE FM_TEST_BLOCK_KILL=1 \
    FM_HOME="$dir/home" FM_ROOT_OVERRIDE="$ROOT" FM_RUNTIME_LOG="$dir/runtime.log" \
    PATH="$dir/fakebin:$PATH" "$TEARDOWN" "$id" --force \
    > "$dir/forced.out" 2> "$dir/forced.err" \
    || fail "--force did not get past a close that failed: $(cat "$dir/forced.err")"
  assert_grep "teardown $id complete" "$dir/forced.out" "the forced cleanup did not finish"
  assert_absent "$dir/home/state/$id.meta" "the forced cleanup kept the task record"
  # Forced cleanup is the case that leaves nothing on disk naming the endpoint,
  # so the operator who forced it has to be told exactly what may survive.
  assert_grep "tmux" "$dir/forced.err" "the forced run did not name the backend it could not close"
  assert_grep "$session:fm-$id" "$dir/forced.err" \
    "the forced run did not name the endpoint it could not close"
  assert_grep "could not be closed" "$dir/forced.err" \
    "the forced run hid the close failure it continued past"
  isolated_tmux_window_exists "$dir" "$socket" "$session" "fm-$id" \
    || fail "the forced run closed the window after all, so this case no longer proves the override"
  isolated_tmux_window_exists "$dir" "$socket" "$session" control \
    || fail "the forced cleanup removed an independent window"

  ( cd "$dir" && env -u TMUX -u TMUX_PANE "$REAL_TMUX" -S "$socket" kill-server 2>/dev/null ) || true
  pass "fm-teardown: --force continues past a close it could not make while still reporting it, and the same case refuses without --force"
}

test_unreadable_close_read_refuses_while_a_definitive_absence_completes() {
  local dir socket='dedicated.sock' session='unreadable read' id=unreadable-task rc
  [ -n "$REAL_TMUX" ] || { echo "skip - tmux not installed"; return 0; }

  # A close that failed, followed by an inventory read that could not run at
  # all. Nothing here shows the window absent, so treating it as closed would
  # strand exactly the endpoint the refusal exists to keep named.
  dir=$(make_case unreadable-close-read)
  ( cd "$dir" && env -u TMUX -u TMUX_PANE "$REAL_TMUX" -S "$socket" new-session -d -s "$session" -n control )
  ( cd "$dir" && env -u TMUX -u TMUX_PANE "$REAL_TMUX" -S "$socket" new-window -d -t "=$session:" -n "fm-$id" )
  write_close_failing_tmux_shim "$dir" "$socket" "$REAL_TMUX"
  write_endpoint_close_meta "$dir" "$id" "$session:fm-$id"

  set +e
  env -u TMUX -u TMUX_PANE FM_TEST_BLOCK_KILL=1 FM_TEST_UNREADABLE_LIST=1 \
    FM_HOME="$dir/home" FM_ROOT_OVERRIDE="$ROOT" FM_RUNTIME_LOG="$dir/runtime.log" \
    PATH="$dir/fakebin:$PATH" "$TEARDOWN" "$id" \
    > "$dir/unreadable.out" 2> "$dir/unreadable.err"
  rc=$?
  set -e
  [ "$rc" -ne 0 ] || fail "an unreadable inventory passed for proof the window closed: $(cat "$dir/unreadable.err")"
  assert_grep "could not be read after its close" "$dir/unreadable.err" \
    "the refusal did not come from the close re-read that could not run"
  assert_no_grep "teardown $id complete" "$dir/unreadable.out" \
    "teardown announced a cleanup it never verified"
  assert_present "$dir/home/state/$id.meta" \
    "teardown deleted the only durable record naming an endpoint it never saw close"
  isolated_tmux_window_exists "$dir" "$socket" "$session" "fm-$id" \
    || fail "the unread endpoint disappeared, so this case no longer proves the hazard"
  ( cd "$dir" && env -u TMUX -u TMUX_PANE "$REAL_TMUX" -S "$socket" kill-server 2>/dev/null ) || true

  # The other direction, twice: tmux answering DEFINITIVELY that the session,
  # or its whole server, is absent is proof the window is gone, so an endpoint
  # that outlived its session is still ordinary silent cleanup.
  dir=$(make_case missing-session-close-read)
  ( cd "$dir" && env -u TMUX -u TMUX_PANE "$REAL_TMUX" -S "$socket" new-session -d -s survivor -n control )
  write_close_failing_tmux_shim "$dir" "$socket" "$REAL_TMUX"
  write_endpoint_close_meta "$dir" "$id" "gone session:fm-$id"
  env -u TMUX -u TMUX_PANE FM_TEST_BLOCK_KILL=1 \
    FM_HOME="$dir/home" FM_ROOT_OVERRIDE="$ROOT" FM_RUNTIME_LOG="$dir/runtime.log" \
    PATH="$dir/fakebin:$PATH" "$TEARDOWN" "$id" \
    > "$dir/missing-session.out" 2> "$dir/missing-session.err" \
    || fail "a definitively absent session refused its own cleanup: $(cat "$dir/missing-session.err")"
  assert_grep "teardown $id complete" "$dir/missing-session.out" \
    "an endpoint whose session is definitively gone did not complete cleanup"
  assert_no_grep "could not be closed" "$dir/missing-session.err" \
    "an endpoint whose session is definitively gone produced a close refusal"
  assert_absent "$dir/home/state/$id.meta" \
    "an endpoint whose session is definitively gone left its task record behind"
  isolated_tmux_window_exists "$dir" "$socket" survivor control \
    || fail "cleaning up an absent session disturbed a live one"
  ( cd "$dir" && env -u TMUX -u TMUX_PANE "$REAL_TMUX" -S "$socket" kill-server 2>/dev/null ) || true

  dir=$(make_case missing-server-close-read)
  write_close_failing_tmux_shim "$dir" "$socket" "$REAL_TMUX"
  write_endpoint_close_meta "$dir" "$id" "$session:fm-$id"
  env -u TMUX -u TMUX_PANE FM_TEST_BLOCK_KILL=1 \
    FM_HOME="$dir/home" FM_ROOT_OVERRIDE="$ROOT" FM_RUNTIME_LOG="$dir/runtime.log" \
    PATH="$dir/fakebin:$PATH" "$TEARDOWN" "$id" \
    > "$dir/missing-server.out" 2> "$dir/missing-server.err" \
    || fail "a definitively absent server refused its own cleanup: $(cat "$dir/missing-server.err")"
  assert_grep "teardown $id complete" "$dir/missing-server.out" \
    "an endpoint whose server is definitively gone did not complete cleanup"
  assert_no_grep "could not be closed" "$dir/missing-server.err" \
    "an endpoint whose server is definitively gone produced a close refusal"
  assert_absent "$dir/home/state/$id.meta" \
    "an endpoint whose server is definitively gone left its task record behind"

  pass "fm-teardown: a close re-read that could not run refuses, while a definitively absent session or server still completes silently"
}

test_forced_secondmate_child_close_failure_still_refuses() {
  local dir socket='dedicated.sock' session='child close failure' mate parent=mate-task child=child-task rc
  [ -n "$REAL_TMUX" ] || { echo "skip - tmux not installed"; return 0; }
  dir=$(make_case secondmate-child-close-failure)
  mate="$dir/mate"
  mkdir -p "$mate/state" "$mate/data" "$mate/config"
  printf '%s' "$parent" > "$mate/.fm-secondmate-home"
  ( cd "$dir" && env -u TMUX -u TMUX_PANE "$REAL_TMUX" -S "$socket" new-session -d -s "$session" -n control )
  ( cd "$dir" && env -u TMUX -u TMUX_PANE "$REAL_TMUX" -S "$socket" new-window -d -t "=$session:" -n "fm-$child" )
  write_close_failing_tmux_shim "$dir" "$socket" "$REAL_TMUX"
  fm_write_meta "$dir/home/state/$parent.meta" \
    "window=$session:fm-$parent" "endpoint_task_id=$parent" \
    "worktree=$mate" "project=$mate" "home=$mate" \
    "kind=secondmate" "mode=secondmate" "harness=echo" "yolo=off" "projects=alpha"
  fm_write_meta "$mate/state/$child.meta" \
    "window=$session:fm-$child" "endpoint_task_id=$child" \
    "worktree=$dir/nonexistent-worktree" "project=$dir/nonexistent-project" \
    "kind=ship" "harness=echo"

  # Forced secondmate cleanup is the ONLY way into the child close path, so
  # --force cannot also be the way past it: honoring force here would delete
  # the refusal rather than override it, and discard a child home whose
  # endpoint is still live.
  set +e
  env -u TMUX -u TMUX_PANE FM_TEST_BLOCK_KILL=1 \
    FM_HOME="$dir/home" FM_ROOT_OVERRIDE="$ROOT" FM_RUNTIME_LOG="$dir/runtime.log" \
    PATH="$dir/fakebin:$PATH" "$TEARDOWN" "$parent" --force \
    > "$dir/child.out" 2> "$dir/child.err"
  rc=$?
  set -e
  [ "$rc" -ne 0 ] || fail "forced secondmate cleanup continued past a child close that failed: $(cat "$dir/child.err")"
  assert_grep "child $child" "$dir/child.err" \
    "the refusal did not name the child whose endpoint could not be closed"
  assert_grep "could not be closed" "$dir/child.err" \
    "forced secondmate cleanup swallowed the child close failure"
  assert_no_grep "teardown $parent complete" "$dir/child.out" \
    "forced secondmate cleanup reported a cleanup it stopped short of"
  assert_present "$mate/state/$child.meta" \
    "forced secondmate cleanup removed the record naming a child endpoint it could not close"
  assert_present "$dir/home/state/$parent.meta" \
    "forced secondmate cleanup removed the secondmate's own record after refusing"
  isolated_tmux_window_exists "$dir" "$socket" "$session" "fm-$child" \
    || fail "the surviving child endpoint disappeared, so this case no longer proves the hazard"

  ( cd "$dir" && env -u TMUX -u TMUX_PANE "$REAL_TMUX" -S "$socket" kill-server 2>/dev/null ) || true
  pass "fm-teardown: forced secondmate cleanup still refuses on a child endpoint close that failed"
}

test_forced_secondmate_cloud_child_refuses_before_any_child_change() {
  local dir mate parent=mate-task child=cloud-child rc
  dir=$(make_case secondmate-cloud-child)
  mate="$dir/mate"
  mkdir -p "$mate/state" "$mate/data" "$mate/config"
  printf '%s' "$parent" > "$mate/.fm-secondmate-home"
  fm_write_secondmate_meta "$dir/home/state/$parent.meta" "$mate"
  fm_write_meta "$mate/state/$child.meta" \
    "cloud_target=example-target" "worktree=$dir/worktree" \
    "project=$dir/project" "kind=ship"
  cp "$mate/state/$child.meta" "$dir/child.before"
  cp "$dir/home/state/$parent.meta" "$dir/parent.before"

  # --force discards a secondmate's child work, so it must not discard a child
  # whose cloud run this client cannot establish. The refusal happens before
  # any child validation, lock, endpoint, or removal step.
  set +e
  run_case "$dir" "$parent" > "$dir/stdout" 2> "$dir/stderr"
  rc=$?
  set -e
  [ "$rc" -ne 0 ] || fail "forced secondmate cleanup succeeded over a cloud-target child"
  assert_contains "$(cat "$dir/stderr")" "cloud-target child $child" \
    "forced secondmate cleanup did not name the cloud-target child it refused"
  cmp -s "$dir/child.before" "$mate/state/$child.meta" \
    || fail "forced secondmate cleanup changed the cloud-target child record"
  cmp -s "$dir/parent.before" "$dir/home/state/$parent.meta" \
    || fail "forced secondmate cleanup changed the secondmate's own record"
  assert_present "$mate/.fm-secondmate-home" \
    "forced secondmate cleanup removed the home that holds a cloud-target child"
  [ ! -s "$dir/runtime.log" ] \
    || fail "forced secondmate cleanup ran a runtime command before refusing: $(cat "$dir/runtime.log")"
  pass "fm-teardown: forced secondmate cleanup refuses a cloud-target child before changing any child"
}

test_forced_secondmate_cloud_grandchild_refuses_the_whole_tree() {
  local dir mate inner parent=mate-task inner_id=inner-mate grand=cloud-grand rc
  dir=$(make_case secondmate-cloud-grandchild)
  mate="$dir/mate"
  inner="$dir/inner-home"
  mkdir -p "$mate/state" "$mate/data" "$mate/config" \
    "$inner/state" "$inner/data" "$inner/config"
  printf '%s' "$parent" > "$mate/.fm-secondmate-home"
  printf '%s' "$inner_id" > "$inner/.fm-secondmate-home"
  fm_write_secondmate_meta "$dir/home/state/$parent.meta" "$mate"
  fm_write_secondmate_meta "$mate/state/$inner_id.meta" "$inner"
  fm_write_meta "$inner/state/$grand.meta" \
    "cloud_target=example-target" "worktree=$dir/worktree" \
    "project=$dir/project" "kind=ship"
  cp "$mate/state/$inner_id.meta" "$dir/inner.before"
  cp "$inner/state/$grand.meta" "$dir/grand.before"

  # A cloud-target record under a nested secondmate is still a descendant of the
  # forced parent, so the refusal must reach it and keep the whole tree.
  set +e
  run_case "$dir" "$parent" > "$dir/stdout" 2> "$dir/stderr"
  rc=$?
  set -e
  [ "$rc" -ne 0 ] || fail "forced secondmate cleanup succeeded over a nested cloud-target child"
  assert_contains "$(cat "$dir/stderr")" "cloud-target child $grand" \
    "forced secondmate cleanup did not reach the nested cloud-target child"
  cmp -s "$dir/grand.before" "$inner/state/$grand.meta" \
    || fail "forced secondmate cleanup changed the nested cloud-target record"
  cmp -s "$dir/inner.before" "$mate/state/$inner_id.meta" \
    || fail "forced secondmate cleanup changed the nested secondmate's record"
  assert_present "$inner/.fm-secondmate-home" \
    "forced secondmate cleanup removed the nested home that holds a cloud-target child"
  assert_present "$mate/.fm-secondmate-home" \
    "forced secondmate cleanup removed the parent home after refusing"
  [ ! -s "$dir/runtime.log" ] \
    || fail "forced secondmate cleanup ran a runtime command before refusing: $(cat "$dir/runtime.log")"
  pass "fm-teardown: forced secondmate cleanup refuses a nested cloud-target child and keeps the whole tree"
}

# A cyclic home reference makes the descendant walk recurse without end on the
# unfixed code, so these cases run under a hard alarm: a hang fails the case
# instead of stalling the suite. A healthy run is not instant: teardown does
# its metadata and backend preflight, then the advisory fm-guard probe, before
# the walk, and that probe alone can take 20 seconds or more in a dirty
# worktree. The bound clears that path; it exists only to catch unbounded
# recursion.
run_case_bounded() {  # <case> <id>
  local dir=$1 id=$2
  FM_HOME="$dir/home" FM_ROOT_OVERRIDE="$ROOT" \
  FM_RUNTIME_LOG="$dir/runtime.log" PATH="$dir/fakebin:$PATH" \
    perl -e 'alarm shift; exec @ARGV or die "exec: $!"' 120 \
    "$TEARDOWN" "$id" --force
}

assert_cyclic_home_refusal() {  # <case> <id> <description>
  local dir=$1 id=$2 description=$3 rc
  set +e
  run_case_bounded "$dir" "$id" > "$dir/stdout" 2> "$dir/stderr"
  rc=$?
  set -e
  [ "$rc" -ne 142 ] \
    || fail "$description: forced teardown never returned, so the cyclic walk recursed without end"
  [ "$rc" -eq 1 ] \
    || fail "$description: forced teardown exited $rc instead of refusing with status 1: $(cat "$dir/stderr")"
  assert_contains "$(cat "$dir/stderr")" "cyclic home reference" \
    "$description: forced teardown did not name the cyclic home reference"
  [ ! -s "$dir/runtime.log" ] \
    || fail "$description: forced teardown ran a runtime command before refusing: $(cat "$dir/runtime.log")"
}

test_forced_secondmate_self_cycle_refuses_before_recursing() {
  local dir mate parent=mate-task child=self-loop
  dir=$(make_case secondmate-self-cycle)
  mate="$dir/mate"
  mkdir -p "$mate/state" "$mate/data" "$mate/config"
  printf '%s' "$parent" > "$mate/.fm-secondmate-home"
  fm_write_secondmate_meta "$dir/home/state/$parent.meta" "$mate"
  # The child's home is its parent's own home, so the walk never reaches a leaf.
  fm_write_secondmate_meta "$mate/state/$child.meta" "$mate"
  cp "$mate/state/$child.meta" "$dir/child.before"
  cp "$dir/home/state/$parent.meta" "$dir/parent.before"

  assert_cyclic_home_refusal "$dir" "$parent" "forced secondmate self-cycle"
  cmp -s "$dir/child.before" "$mate/state/$child.meta" \
    || fail "forced secondmate self-cycle changed the cyclic child record"
  cmp -s "$dir/parent.before" "$dir/home/state/$parent.meta" \
    || fail "forced secondmate self-cycle changed the secondmate's own record"
  assert_present "$mate/.fm-secondmate-home" \
    "forced secondmate self-cycle removed the home it refused"
  pass "fm-teardown: forced secondmate cleanup refuses a self-referencing home before recursing"
}

test_forced_secondmate_nested_cycle_refuses_the_whole_tree() {
  local dir mate inner parent=mate-task inner_id=inner-mate back=back-mate
  dir=$(make_case secondmate-nested-cycle)
  mate="$dir/mate"
  inner="$dir/inner-home"
  mkdir -p "$mate/state" "$mate/data" "$mate/config" \
    "$inner/state" "$inner/data" "$inner/config"
  printf '%s' "$parent" > "$mate/.fm-secondmate-home"
  printf '%s' "$inner_id" > "$inner/.fm-secondmate-home"
  fm_write_secondmate_meta "$dir/home/state/$parent.meta" "$mate"
  fm_write_secondmate_meta "$mate/state/$inner_id.meta" "$inner"
  # The nested secondmate points back at the parent home, closing a loop two homes deep.
  fm_write_secondmate_meta "$inner/state/$back.meta" "$mate"
  cp "$mate/state/$inner_id.meta" "$dir/inner.before"
  cp "$inner/state/$back.meta" "$dir/back.before"

  assert_cyclic_home_refusal "$dir" "$parent" "forced secondmate nested cycle"
  cmp -s "$dir/inner.before" "$mate/state/$inner_id.meta" \
    || fail "forced secondmate nested cycle changed the nested secondmate's record"
  cmp -s "$dir/back.before" "$inner/state/$back.meta" \
    || fail "forced secondmate nested cycle changed the record that closes the loop"
  assert_present "$inner/.fm-secondmate-home" \
    "forced secondmate nested cycle removed the nested home"
  assert_present "$mate/.fm-secondmate-home" \
    "forced secondmate nested cycle removed the parent home after refusing"
  pass "fm-teardown: forced secondmate cleanup refuses a nested home cycle and keeps the whole tree"
}

test_forced_secondmate_cycle_back_to_the_root_home_refuses() {
  local dir mate parent=mate-task back=back-root
  dir=$(make_case secondmate-root-cycle)
  mate="$dir/mate"
  mkdir -p "$mate/state" "$mate/data" "$mate/config"
  printf '%s' "$parent" > "$mate/.fm-secondmate-home"
  fm_write_secondmate_meta "$dir/home/state/$parent.meta" "$mate"
  # The child's home is the root firstmate home that owns the parent record.
  fm_write_secondmate_meta "$mate/state/$back.meta" "$dir/home"
  cp "$dir/home/state/$parent.meta" "$dir/parent.before"
  cp "$mate/state/$back.meta" "$dir/back.before"

  assert_cyclic_home_refusal "$dir" "$parent" "forced secondmate root cycle"
  cmp -s "$dir/parent.before" "$dir/home/state/$parent.meta" \
    || fail "forced secondmate root cycle changed the secondmate's own record"
  cmp -s "$dir/back.before" "$mate/state/$back.meta" \
    || fail "forced secondmate root cycle changed the record that loops back to the root"
  assert_present "$mate/.fm-secondmate-home" \
    "forced secondmate root cycle removed the parent home after refusing"
  pass "fm-teardown: forced secondmate cleanup refuses a child that loops back to the root home"
}

test_forced_secondmate_cycle_through_a_symlink_refuses() {
  local dir mate alias parent=mate-task child=alias-loop
  dir=$(make_case secondmate-symlink-cycle)
  mate="$dir/mate"
  alias="$dir/mate-alias"
  mkdir -p "$mate/state" "$mate/data" "$mate/config"
  printf '%s' "$parent" > "$mate/.fm-secondmate-home"
  ln -s "$mate" "$alias"
  fm_write_secondmate_meta "$dir/home/state/$parent.meta" "$mate"
  # The child names the same home through a symlink, so only physical paths reveal the loop.
  fm_write_secondmate_meta "$mate/state/$child.meta" "$alias"
  cp "$mate/state/$child.meta" "$dir/child.before"
  cp "$dir/home/state/$parent.meta" "$dir/parent.before"

  assert_cyclic_home_refusal "$dir" "$parent" "forced secondmate symlink cycle"
  cmp -s "$dir/child.before" "$mate/state/$child.meta" \
    || fail "forced secondmate symlink cycle changed the aliased child record"
  cmp -s "$dir/parent.before" "$dir/home/state/$parent.meta" \
    || fail "forced secondmate symlink cycle changed the secondmate's own record"
  assert_present "$mate/.fm-secondmate-home" \
    "forced secondmate symlink cycle removed the home it refused"
  pass "fm-teardown: forced secondmate cleanup refuses a child that reaches its own home through a symlink"
}

test_forced_secondmate_shared_child_is_not_a_cycle() {
  local dir mate shared parent=mate-task
  dir=$(make_case secondmate-shared-child)
  mate="$dir/mate"
  shared="$dir/shared-home"
  mkdir -p "$mate/state" "$mate/data" "$mate/config" \
    "$shared/state" "$shared/data" "$shared/config"
  printf '%s' "$parent" > "$mate/.fm-secondmate-home"
  printf '%s' shared-mate > "$shared/.fm-secondmate-home"
  fm_write_secondmate_meta "$dir/home/state/$parent.meta" "$mate"
  # Two sibling records naming one home share a child without forming a cycle.
  # The walk must pass them through to child validation, which then refuses the
  # duplicate claim on one home marker; the walk itself must not call it a cycle.
  fm_write_secondmate_meta "$mate/state/left.meta" "$shared"
  fm_write_secondmate_meta "$mate/state/right.meta" "$shared"

  set +e
  run_case_bounded "$dir" "$parent" > "$dir/stdout" 2> "$dir/stderr"
  local rc=$?
  set -e
  [ "$rc" -ne 142 ] \
    || fail "forced shared-child teardown never returned"
  assert_contains "$(cat "$dir/stderr")" "child firstmate home" \
    "forced shared-child teardown did not reach child validation"
  case "$(cat "$dir/stderr")" in
    *"cyclic home reference"*)
      fail "forced shared-child teardown refused a shared child as a cycle: $(cat "$dir/stderr")" ;;
  esac
  pass "fm-teardown: forced secondmate cleanup does not treat a shared child home as a cycle"
}

test_removed_backend_record_refuses_teardown_even_under_force() {
  local dir id=removed-strand rc
  dir=$(make_case removed-backend-teardown)
  # An Orca record names a backend that no longer exists. Teardown must refuse
  # it at the shared validator, before any runtime call, with the record kept,
  # and --force must not change that.
  fm_write_meta "$dir/home/state/$id.meta" \
    "window=fm-$id" "endpoint_task_id=$id" "terminal=term-7" \
    "worktree=$dir/nonexistent-worktree" "project=$dir/nonexistent-project" \
    "backend=orca" "orca_worktree_id=worktree-9::/orca/worktree-9" "kind=ship" "mode=no-mistakes"

  set +e
  env -u TMUX -u TMUX_PANE \
    FM_HOME="$dir/home" FM_ROOT_OVERRIDE="$ROOT" FM_RUNTIME_LOG="$dir/runtime.log" \
    PATH="$dir/fakebin:$PATH" "$TEARDOWN" "$id" --force \
    > "$dir/removed-forced.out" 2> "$dir/removed-forced.err"
  rc=$?
  set -e
  [ "$rc" -ne 0 ] || fail "a forced teardown of a removed-backend record completed: $(cat "$dir/removed-forced.err")"
  assert_contains "$(cat "$dir/removed-forced.err")" "unknown backend" \
    "the forced removed-backend refusal did not name the unknown backend"
  [ ! -s "$dir/runtime.log" ] \
    || fail "a removed-backend record reached a runtime call under --force: $(cat "$dir/runtime.log")"
  assert_present "$dir/home/state/$id.meta" \
    "the forced removed-backend refusal removed the only durable record naming the endpoint"

  set +e
  env -u TMUX -u TMUX_PANE \
    FM_HOME="$dir/home" FM_ROOT_OVERRIDE="$ROOT" FM_RUNTIME_LOG="$dir/runtime.log" \
    PATH="$dir/fakebin:$PATH" "$TEARDOWN" "$id" \
    > "$dir/removed-unforced.out" 2> "$dir/removed-unforced.err"
  rc=$?
  set -e
  [ "$rc" -ne 0 ] || fail "an unforced teardown of a removed-backend record completed: $(cat "$dir/removed-unforced.err")"
  [ ! -s "$dir/runtime.log" ] \
    || fail "a removed-backend record reached a runtime call without --force: $(cat "$dir/runtime.log")"
  assert_present "$dir/home/state/$id.meta" \
    "the unforced removed-backend refusal removed the only durable record naming the endpoint"

  pass "fm-teardown: a removed-backend endpoint record refuses even under --force, with no runtime call and its record preserved"
}

test_already_gone_endpoint_still_completes_without_a_refusal() {
  local dir socket session='already gone' id=gone-task
  [ -n "$REAL_TMUX" ] || { echo "skip - tmux not installed"; return 0; }
  dir=$(make_case already-gone)
  socket=dedicated.sock
  ( cd "$dir" && env -u TMUX -u TMUX_PANE "$REAL_TMUX" -S "$socket" new-session -d -s "$session" -n control )
  write_close_failing_tmux_shim "$dir" "$socket" "$REAL_TMUX"
  # The whole point of this case: the recorded window has already exited, so
  # its close cannot succeed and must still be the ordinary silent cleanup.
  isolated_tmux_window_exists "$dir" "$socket" "$session" "fm-$id" \
    && fail "the already-gone fixture unexpectedly has its task window"

  write_endpoint_close_meta "$dir" "$id" "$session:fm-$id"

  env -u TMUX -u TMUX_PANE \
    FM_HOME="$dir/home" FM_ROOT_OVERRIDE="$ROOT" FM_RUNTIME_LOG="$dir/runtime.log" \
    PATH="$dir/fakebin:$PATH" "$TEARDOWN" "$id" \
    > "$dir/gone.out" 2> "$dir/gone.err" \
    || fail "an already-exited endpoint refused cleanup: $(cat "$dir/gone.err")"
  assert_grep "teardown $id complete" "$dir/gone.out" \
    "an already-exited endpoint did not report a completed cleanup"
  assert_no_grep "could not be closed" "$dir/gone.err" \
    "an already-exited endpoint produced a close refusal"
  assert_no_grep "is still present after its close" "$dir/gone.err" \
    "an already-exited endpoint was reported as a surviving endpoint"
  assert_absent "$dir/home/state/$id.meta" \
    "an already-exited endpoint left its task record behind"

  # A server that is already gone entirely is the same ordinary case, and the
  # adapter is driven directly so no other teardown refusal can stand in for it.
  ( cd "$dir" && env -u TMUX -u TMUX_PANE "$REAL_TMUX" -S "$socket" kill-server 2>/dev/null ) || true
  # shellcheck disable=SC2016 # $1 and $2 expand inside the isolated child shell.
  env -u TMUX -u TMUX_PANE FM_RUNTIME_LOG="$dir/runtime.log" PATH="$dir/fakebin:$PATH" \
    bash -c '. "$1/bin/fm-backend.sh"; fm_backend_kill tmux "$2"' _ "$ROOT" "$session:fm-$id" \
    > "$dir/deadserver.out" 2> "$dir/deadserver.err" \
    || fail "closing an endpoint whose whole server is gone reported a failure: $(cat "$dir/deadserver.err")"
  [ ! -s "$dir/deadserver.err" ] \
    || fail "closing an endpoint whose whole server is gone was not silent: $(cat "$dir/deadserver.err")"

  pass "fm-teardown: an already-exited endpoint, and a server that is already gone, still complete cleanup silently"
}

test_cloud_target_record_refuses_before_any_local_step() {
  local dir id=cloud-task

  # A cloud record that still names a local endpoint must not reach the endpoint
  # probe, the fake runtime, or any removal, even under --force.
  dir=$(make_case cloud-with-endpoint)
  fm_write_meta "$dir/home/state/$id.meta" \
    "cloud_target=example-target" "window=isolated:fm-$id" "endpoint_task_id=$id" \
    "worktree=$dir/worktree" "project=$dir/project" "kind=ship"
  assert_refused_without_mutation "$dir" "$id" "cloud record with a local endpoint"
  assert_contains "$(cat "$dir/stderr")" "cloud target example-target" \
    "cloud record with a local endpoint was not refused for its cloud state"

  # A cloud run has no local pane, so a record without an endpoint is refused for
  # its cloud state rather than for the missing endpoint.
  dir=$(make_case cloud-without-endpoint)
  fm_write_meta "$dir/home/state/$id.meta" \
    "cloud_target=example-target" "worktree=$dir/worktree" \
    "project=$dir/project" "kind=ship"
  assert_refused_without_mutation "$dir" "$id" "cloud record without a local endpoint"
  assert_contains "$(cat "$dir/stderr")" "cloud target example-target" \
    "cloud record without a local endpoint was not refused for its cloud state"

  # A present but empty cloud_target is still a cloud-target record, so it refuses.
  dir=$(make_case cloud-empty-target)
  fm_write_meta "$dir/home/state/$id.meta" \
    "cloud_target=" "worktree=$dir/worktree" "project=$dir/project" "kind=ship"
  assert_refused_without_mutation "$dir" "$id" "cloud record with an empty target"
  assert_contains "$(cat "$dir/stderr")" "cloud target <empty>" \
    "cloud record with an empty target was not refused as a cloud record"

  pass "fm-teardown: a cloud-target record refuses before any endpoint, runtime, or removal step, even under --force"
}

test_cloud_target_record_refuses_before_any_local_step
test_invalid_endpoint_records_refuse_before_mutation
test_control_lock_contention_refuses_before_mutation
test_non_pool_teardown_ignores_task_set_lock
test_metadata_lock_serializes_destructive_cleanup
test_supported_backend_endpoint_records_validate
test_tmux_empty_target_refuses_without_invocation
test_recorded_process_identity_cleanup_is_exact
test_isolated_tmux_invalid_and_valid_cleanup
test_failed_endpoint_close_refuses_before_removing_the_record
test_forced_teardown_continues_past_a_close_it_could_not_make
test_unreadable_close_read_refuses_while_a_definitive_absence_completes
test_forced_secondmate_child_close_failure_still_refuses
test_forced_secondmate_cloud_child_refuses_before_any_child_change
test_forced_secondmate_cloud_grandchild_refuses_the_whole_tree
test_forced_secondmate_self_cycle_refuses_before_recursing
test_forced_secondmate_nested_cycle_refuses_the_whole_tree
test_forced_secondmate_cycle_back_to_the_root_home_refuses
test_forced_secondmate_cycle_through_a_symlink_refuses
test_forced_secondmate_shared_child_is_not_a_cycle
test_removed_backend_record_refuses_teardown_even_under_force
test_already_gone_endpoint_still_completes_without_a_refusal
test_bare_relative_origin_shares_project_lock_with_clone
test_reused_pool_slot_refuses_before_touching_the_other_task
test_cross_home_pool_slot_collision_refuses
test_sole_slot_record_still_tears_down
test_reassigned_pool_slot_finishes_own_cleanup_without_touching_the_slot
test_stale_record_on_claimed_slot_retires_then_claimant_tears_down
test_own_and_absent_slot_claims_still_tear_down
test_recorded_endpoint_that_changed_directory_still_tears_down
test_project_lock_anchors_at_the_local_root_across_home_layouts
