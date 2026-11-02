#!/usr/bin/env bash
# Behavior tests for bin/fm-birds-eye-capture.sh (DPES-798 observer-only capture).
# Every capture reads a REAL private tmux server (own -L socket) through a PATH
# shim, so support claims rest on real capture-pane output and never on a fake.
# The shim only logs tmux subcommands, and for one test rewrites a task's
# spawn_gen during the capture to simulate a relaunch. It never answers a capture.
# shellcheck disable=SC2016 # jq filters take their values through --arg, so $-references in single quotes are intended.
set -u

# Clear inherited operational-home path variables before any bin/ script runs.
for name in FM_HOME FM_BACKEND FM_ROOT_OVERRIDE FM_STATE_OVERRIDE FM_DATA_OVERRIDE \
  FM_CONFIG_OVERRIDE FM_PROJECTS_OVERRIDE FM_SNAPSHOT_CACHE_DIR FM_SNAPSHOT_BUDGET \
  FM_CREW_STATE_NO_FORGE; do
  unset "$name"
done

# shellcheck source=tests/lib.sh
# shellcheck disable=SC1091
. "$(dirname "${BASH_SOURCE[0]}")/lib.sh"

SCRIPT="$ROOT/bin/fm-birds-eye-capture.sh"
PANE_PROGRAM="$ROOT/tests/fixtures/birds-eye/capture/pane-program.sh"

command -v jq >/dev/null 2>&1 || { echo "skip: jq not found"; exit 0; }
REAL_TMUX=$(command -v tmux 2>/dev/null) || { echo "skip: tmux not found"; exit 0; }

fail() { printf 'not ok - %s\n' "$1" >&2; exit 1; }
pass() { printf 'ok - %s\n' "$1"; }

SOCKET="fm-birds-eye-capture-$$"
SESSION=fbe-cap
TMP_ROOT=$(fm_test_tmproot birds-eye-capture)
SHIM_DIR="$TMP_ROOT/shim"
SENTINEL="$TMP_ROOT/injected"
TEXT="$TMP_ROOT/text.txt"
OUT="$TMP_ROOT/out.json"
ERR="$TMP_ROOT/err.txt"
MAIN_HOME="$TMP_ROOT/home"
DEAD_HOME="$TMP_ROOT/dead-home"
HERDR_HOME="$TMP_ROOT/herdr-home"
SPARSE_HOME="$TMP_ROOT/sparse-home"
BIND_HOME="$TMP_ROOT/bind-home"
PRIV_HOME="$TMP_ROOT/priv-home"
PRIV_DIRS="$TMP_ROOT/priv-dirs"
OTHER_HOME="$TMP_ROOT/other-home"
LONG_ID=-leading-dash-and-long-$(printf 'x%.0s' $(seq 1 60))
GEN=s1792290000.4242.7001
RELAUNCH_OLD=s1792290001.4244.7003
RELAUNCH_NEW=s1792290999.4243.7002
HERDR_GEN=s1792290002.4245.7004
GONE_GEN=s1792290003.4246.7005
SPARSE_GEN=s1792290004.4247.7006

birds_eye_cleanup() {
  "$REAL_TMUX" -L "$SOCKET" kill-server >/dev/null 2>&1 || true
}
trap 'birds_eye_cleanup; fm_test_cleanup' EXIT

# Private tmux server. The shim forwards every tmux call here, so the host's own
# sessions are never touched.
"$REAL_TMUX" -L "$SOCKET" kill-server >/dev/null 2>&1 || true
mkdir -p "$SHIM_DIR"
cat > "$SHIM_DIR/tmux" <<'SH'
#!/usr/bin/env bash
# Test shim. Logs each tmux subcommand when FBE_TMUX_LOG is set. When a
# relaunch is staged, the first capture at the sentinel line count rewrites the
# task's spawn_gen once, then the real capture runs unchanged.
set -u
if [ -n "${FBE_TMUX_LOG:-}" ]; then
  printf '%s\n' "$*" >> "$FBE_TMUX_LOG"
fi
if [ -n "${FBE_RELAUNCH_META:-}" ] && [ -f "${FBE_RELAUNCH_TRIGGER:-/nonexistent}" ]; then
  case " $* " in
    *" -S -${FBE_RELAUNCH_LINES:-x} "*)
      rm -f "$FBE_RELAUNCH_TRIGGER"
      sed "s/^spawn_gen=.*/spawn_gen=$FBE_RELAUNCH_NEW_GEN/" "$FBE_RELAUNCH_META" \
        > "$FBE_RELAUNCH_META.next"
      mv "$FBE_RELAUNCH_META.next" "$FBE_RELAUNCH_META"
      ;;
  esac
fi
exec "$FBE_REAL_TMUX" -L "$FBE_SOCKET" "$@"
SH
chmod +x "$SHIM_DIR/tmux"
# The snapshot consults no-mistakes for ship tasks. The stub keeps this test
# away from any real pipeline state. It is not a capture or support stand-in.
printf '#!/usr/bin/env bash\nexit 0\n' > "$SHIM_DIR/no-mistakes"
chmod +x "$SHIM_DIR/no-mistakes"
export FBE_REAL_TMUX="$REAL_TMUX" FBE_SOCKET="$SOCKET"
PATH="$SHIM_DIR:$PATH"
export PATH

"$REAL_TMUX" -L "$SOCKET" new-session -d -s "$SESSION" -n fm-cap-task -x 120 -y 24 \
  "bash '$PANE_PROGRAM'" || fail "could not start the private tmux server"

wait_for_marker() {
  local i=0
  while [ "$i" -lt 100 ]; do
    if "$REAL_TMUX" -L "$SOCKET" capture-pane -p -t "$SESSION:fm-cap-task" -S -200 2>/dev/null \
      | grep -Fq fixture-end; then
      return 0
    fi
    sleep 0.1
    i=$((i + 1))
  done
  return 1
}
wait_for_marker || fail "pane program never printed its closing marker"

# make_home <dir>: an operational home with the state, data, projects, and config
# directories the snapshot reads. Each home holds only the rows its test needs,
# because a row with a slow probe (a herdr row) slows every snapshot in its home.
make_home() {
  mkdir -p "$1/state" "$1/data" "$1/projects/alpha-worktree" "$1/config"
  printf '## In flight\n\n## Queued\n\n## Done\n' > "$1/data/backlog.md"
}

# write_task <home> <id> <window> <spawn-gen> [backend]
write_task() {
  local home=$1 id=$2 window=$3 gen=$4 backend=${5:-}
  local -a kv
  kv=("window=$window" "worktree=$home/projects/alpha-worktree" "project=alpha" \
    "harness=claude" "kind=ship" "mode=ship" "yolo=off" "spawn_gen=$gen")
  if [ -n "$backend" ]; then
    kv=("backend=$backend" "${kv[@]}")
  fi
  fm_write_meta "$home/state/$id.meta" "${kv[@]}"
}

make_home "$MAIN_HOME"
write_task "$MAIN_HOME" cap-task "$SESSION:fm-cap-task" "$GEN"
write_task "$MAIN_HOME" relaunch-task "$SESSION:fm-relaunch-task" "$RELAUNCH_OLD"
"$REAL_TMUX" -L "$SOCKET" new-window -d -t "$SESSION" -n fm-relaunch-task "sleep 600" \
  || fail "could not start the relaunch pane"
make_home "$DEAD_HOME"
write_task "$DEAD_HOME" gone-task "$SESSION:fm-gone-task" "$GONE_GEN"
make_home "$HERDR_HOME"
write_task "$HERDR_HOME" herdr-task "workspace:surface" "$HERDR_GEN" herdr
make_home "$SPARSE_HOME"
write_task "$SPARSE_HOME" sparse-task "$SESSION:fm-sparse-task" "$SPARSE_GEN"

# run_capture <home> <task> <spawn-gen> [extra args...]: sets RC, leaves JSON in $OUT.
run_capture() {
  local home=$1 task=$2 gen=$3
  shift 3
  "$SCRIPT" --root "$home" --task "$task" --spawn-gen "$gen" "$@" >"$OUT" 2>"$ERR"
  RC=$?
}

# expect_json <label> <jq-filter> [jq args...]: the filter must hold on $OUT.
expect_json() {
  local label=$1 filter=$2
  shift 2
  if ! jq -e "$@" "$filter" "$OUT" >/dev/null 2>&1; then
    fail "$label: $(jq -c '{status,reason,lines_requested,lines_returned,truncated,spawn_gen_current}' "$OUT" 2>/dev/null || cat "$OUT") stderr=$(head -c 300 "$ERR")"
  fi
}

# Count bytes that are not plain text. Tab and newline are plain; every other
# C0 control, DEL, and ESC count as control bytes.
control_byte_count() {  # <text-file>
  LC_ALL=C tr -d '\t\n' < "$1" | LC_ALL=C grep -c '[[:cntrl:]]' || true
}

manifest() {  # <home>
  (cd "$1" && find state data config -type f -exec cksum {} + | LC_ALL=C sort)
}

# --- Default, 40 lines: real capture, fields, plain text, side-effect audit ---

manifest "$MAIN_HOME" > "$TMP_ROOT/manifest-before.txt"
TMUX_LOG="$TMP_ROOT/tmux.log"
: > "$TMUX_LOG"
export FBE_TMUX_LOG="$TMUX_LOG"
run_capture "$MAIN_HOME" cap-task "$GEN"
unset FBE_TMUX_LOG
manifest "$MAIN_HOME" > "$TMP_ROOT/manifest-after.txt"

[ "$RC" -eq 0 ] || fail "default capture exited $RC: $(head -c 300 "$ERR")"
expect_json "default capture" '
  .schema == "fm-birds-eye-capture.v1" and .status == "ok" and .reason == ""
  and .task_id == "cap-task" and .key != null
  and .lines_requested == 40 and .lines_returned == 40
  and .truncated == true and .source_backend == "tmux"
  and .spawn_gen_requested == $gen and .spawn_gen_current == $gen
' --arg gen "$GEN"
captured_at=$(jq -r .captured_at "$OUT")
printf '%s' "$captured_at" | grep -Eq '^[0-9]{4}-[0-9]{2}-[0-9]{2}T[0-9]{2}:[0-9]{2}:[0-9]{2}Z$' \
  || fail "captured_at is not a UTC timestamp: $captured_at"
jq -r .text "$OUT" > "$TEXT"
grep -Fq fixture-end "$TEXT" || fail "40-line capture lacks the newest pane line"
grep -Fq line-150 "$TEXT" || fail "40-line capture lacks line-150"
! grep -Fq line-080 "$TEXT" || fail "40-line capture reached past its tail into line-080"
[ "$(control_byte_count "$TEXT")" -eq 0 ] || fail "40-line capture returned control bytes"
cmp -s "$TMP_ROOT/manifest-before.txt" "$TMP_ROOT/manifest-after.txt" \
  || fail "capture changed state/data/config: $(diff "$TMP_ROOT/manifest-before.txt" "$TMP_ROOT/manifest-after.txt" | head -c 400)"
grep -Eqx 'capture-pane -p -t @[0-9]+ -S -40' "$TMUX_LOG" \
  || fail "tmux log lacks the expected read-only capture-pane call by window id (log: $(head -c 300 "$TMUX_LOG"))"
if grep -Eq '^(send|attach|detach|kill|new|respawn|pipe|set|paste|load|rename|select|split|run|resize|move|swap|link|unlink|break|join|source|bind|unbind|if|wait|start)' "$TMUX_LOG"; then
  fail "snapshot or capture issued a mutating tmux subcommand: $(head -c 400 "$TMUX_LOG")"
fi
pass "default capture: 40 lines, real pane, plain text, state unchanged, read-only tmux calls only"

# --- 100 lines: wider tail, same fields ---

run_capture "$MAIN_HOME" cap-task "$GEN" --lines 100
[ "$RC" -eq 0 ] || fail "100-line capture exited $RC: $(head -c 300 "$ERR")"
expect_json "100-line capture" '
  .status == "ok" and .reason == "" and .lines_requested == 100
  and .lines_returned == 100 and .truncated == true and .spawn_gen_current == $gen
' --arg gen "$GEN"
jq -r .text "$OUT" > "$TEXT"
grep -Fq line-080 "$TEXT" || fail "100-line capture lacks line-080"
! grep -Fq line-040 "$TEXT" || fail "100-line capture returned more than its 100-line tail"
[ "$(control_byte_count "$TEXT")" -eq 0 ] || fail "100-line capture returned control bytes"
pass "100-line capture: 100-line tail from the real pane"

# --- Excessive bounds clamp to 100 with an explicit reason ---

run_capture "$MAIN_HOME" cap-task "$GEN" --lines 101
expect_json "101 clamps" '
  .status == "ok" and .lines_requested == 100 and .lines_returned == 100
  and .reason == "requested lines clamped to 100 (v1 maximum)"
'
run_capture "$MAIN_HOME" cap-task "$GEN" --lines 99999
expect_json "99999 clamps" '
  .status == "ok" and .lines_requested == 100 and .lines_returned == 100
  and .reason == "requested lines clamped to 100 (v1 maximum)"
'
pass "excessive bounds: 101 and 99999 clamp to 100 with a stated reason"

# --- Malformed bounds and unknown options are invocation errors: exit 2, no JSON ---

for bad in 0 00 abc -5 1.5 '' 101x; do
  run_capture "$MAIN_HOME" cap-task "$GEN" --lines "$bad"
  [ "$RC" -eq 2 ] || fail "--lines '$bad' should exit 2, got $RC"
  [ ! -s "$OUT" ] || fail "--lines '$bad' should print no JSON"
done
run_capture "$MAIN_HOME" cap-task "$GEN" --backend herdr
[ "$RC" -eq 2 ] && [ ! -s "$OUT" ] || fail "--backend selector should be refused with exit 2"
run_capture "$MAIN_HOME" cap-task "$GEN" --pane %27
[ "$RC" -eq 2 ] && [ ! -s "$OUT" ] || fail "--pane selector should be refused with exit 2"
pass "malformed --lines values and backend or pane selectors exit 2 with no output"

# --- Stale generation: a superseded spawn_gen never shows current output ---

run_capture "$MAIN_HOME" cap-task s0.0.0
expect_json "stale generation" '
  .status == "generation_mismatch" and .spawn_gen_requested == "s0.0.0"
  and .spawn_gen_current == $gen and .text == "" and .lines_returned == 0
  and .source_backend == "tmux"
' --arg gen "$GEN"
pass "stale generation: generation_mismatch with the current spawn_gen and no text"

# --- Arbitrary identities never reach the snapshot or a shell ---

for id in "$SESSION:cap-task" "../state/cap-task" "cap-task;touch $SENTINEL" \
  "\$(touch $SENTINEL)" "%27" "*"; do
  run_capture "$MAIN_HOME" "$id" "$GEN"
  expect_json "arbitrary identity '$id'" '
    .status == "unknown_identity" and .text == "" and .spawn_gen_current == null
    and .source_backend == null
  '
done
[ ! -e "$SENTINEL" ] || fail "an identity argument was evaluated by a shell"
pass "arbitrary identities: unknown_identity, no text, nothing evaluated"

# --- A well-formed id absent from the snapshot is unknown, not guessed ---

run_capture "$MAIN_HOME" no-such-task "$GEN"
expect_json "unknown task" '
  .status == "unknown_identity" and .text == "" and .spawn_gen_current == null
'
pass "well-formed unknown id: unknown_identity from the snapshot"

# --- Unsupported backend and dead endpoint: explicit Output unavailable ---

run_capture "$HERDR_HOME" herdr-task "$HERDR_GEN"
expect_json "unsupported backend" '
  .status == "unavailable" and (.reason | startswith("Output unavailable"))
  and .source_backend == "herdr" and .text == ""
'
run_capture "$DEAD_HOME" gone-task "$GONE_GEN"
expect_json "dead endpoint" '
  .status == "unavailable" and (.reason | startswith("Output unavailable"))
  and .source_backend == "tmux" and .text == ""
'
pass "unsupported backend and dead endpoint: Output unavailable with a reason"

# --- Relaunch during the capture: output is discarded, never shown as current ---

TRIGGER="$TMP_ROOT/relaunch.trigger"
: > "$TRIGGER"
export FBE_RELAUNCH_META="$MAIN_HOME/state/relaunch-task.meta" \
  FBE_RELAUNCH_TRIGGER="$TRIGGER" FBE_RELAUNCH_LINES=37 FBE_RELAUNCH_NEW_GEN="$RELAUNCH_NEW"
run_capture "$MAIN_HOME" relaunch-task "$RELAUNCH_OLD" --lines 37
unset FBE_RELAUNCH_META FBE_RELAUNCH_TRIGGER FBE_RELAUNCH_LINES FBE_RELAUNCH_NEW_GEN
[ ! -e "$TRIGGER" ] || fail "relaunch was never injected during the capture"
expect_json "relaunch during capture" '
  .status == "generation_mismatch" and .spawn_gen_current == $new
  and .text == "" and (.reason | test("changed during the capture"))
' --arg new "$RELAUNCH_NEW"
pass "relaunch during capture: generation_mismatch, output discarded"

# --- Sparse pane: viewport padding is not content ---

"$REAL_TMUX" -L "$SOCKET" new-window -d -t "$SESSION" -n fm-sparse-task \
  "printf 'sparse-one\nsparse-two\nsparse-three\nsparse-end\n'; sleep 600" \
  || fail "could not start the sparse pane"
i=0
until "$REAL_TMUX" -L "$SOCKET" capture-pane -p -t "$SESSION:fm-sparse-task" 2>/dev/null | grep -Fq sparse-end; do
  i=$((i + 1))
  [ "$i" -lt 100 ] || fail "sparse pane never printed its closing marker"
  sleep 0.1
done
run_capture "$SPARSE_HOME" sparse-task "$SPARSE_GEN" --lines 10
expect_json "sparse pane within bound" '
  .status == "ok" and .lines_requested == 10 and .lines_returned == 4
  and .truncated == false
'
expect_json "sparse pane text rows" '
  (.text | rtrimstr("\n") | split("\n")) == ["sparse-one", "sparse-two", "sparse-three", "sparse-end"]
'
run_capture "$SPARSE_HOME" sparse-task "$SPARSE_GEN" --lines 2
expect_json "sparse pane past bound" '
  .status == "ok" and .lines_returned == 2 and .truncated == true
'
jq -r .text "$OUT" > "$TEXT"
grep -Fq sparse-end "$TEXT" || fail "sparse tail lacks the newest content row"
! grep -Fq sparse-one "$TEXT" || fail "sparse tail kept a row past its bound"
pass "sparse pane: padding rows are not counted, truncated only when content rows drop"

# --- Exact endpoint binding: a missing recorded window never prefix-matches a neighbor ---

# neighbor window "fm-ghost-task-neighbor" is live. The recorded window
# "fm-ghost-task" is not, and tmux would resolve that bare name to the neighbor.
"$REAL_TMUX" -L "$SOCKET" new-window -d -t "$SESSION" -n fm-ghost-task-neighbor \
  "printf 'neighbor-secret\n'; sleep 600" || fail "could not start the neighbor pane"
"$REAL_TMUX" -L "$SOCKET" new-window -d -t "$SESSION" -n fm-other-task \
  "printf 'other-secret\n'; sleep 600" || fail "could not start the other pane"
"$REAL_TMUX" -L "$SOCKET" new-window -d -t "$SESSION" -n "fm-$LONG_ID" \
  "printf 'long-id-line\n'; sleep 600" || fail "could not start the long-id pane"
until "$REAL_TMUX" -L "$SOCKET" capture-pane -p -t "=$SESSION:=fm-$LONG_ID" 2>/dev/null | grep -Fq long-id-line; do
  sleep 0.1
done
make_home "$BIND_HOME"
write_task "$BIND_HOME" ghost-task "$SESSION:fm-ghost-task" s1792290010.1.1
write_task "$BIND_HOME" miswired-task "$SESSION:fm-other-task" s1792290011.1.2
write_task "$BIND_HOME" "$LONG_ID" "$SESSION:fm-$LONG_ID" s1792290012.1.3
[ "$("$REAL_TMUX" -L "$SOCKET" display-message -p -t "$SESSION:fm-ghost-task" '#{window_name}')" = fm-ghost-task-neighbor ] \
  || fail "lab precondition: bare tmux target no longer prefix-matches the neighbor"

run_capture "$BIND_HOME" ghost-task s1792290010.1.1
expect_json "missing window must not read a prefix neighbor" '
  .status == "unavailable" and (.reason | startswith("Output unavailable"))
  and .text == "" and .lines_returned == 0
'
! grep -Fq neighbor-secret "$OUT" || fail "output of a prefix-matched neighbor window was returned"
pass "missing recorded window: unavailable, never a prefix-matched neighbor's output"

run_capture "$BIND_HOME" miswired-task s1792290011.1.2
expect_json "endpoint of another task" '
  .status == "unavailable" and (.reason | startswith("Output unavailable")) and .text == ""
'
! grep -Fq other-secret "$OUT" || fail "output of another task's window was returned"
pass "endpoint that is not fm-<task-id>: unavailable, no text"

# A legal id may contain a dot. tmux reads "fm-dot-a.0" as pane 0 of window
# "fm-dot-a", so a live window "fm-dot-a" must not answer for the missing window
# "fm-dot-a.0", and the exact window must answer once it exists.
"$REAL_TMUX" -L "$SOCKET" new-window -d -t "$SESSION" -n fm-dot-a \
  "printf 'dot-neighbor-secret\n'; sleep 600" || fail "could not start the dotted-neighbor pane"
write_task "$BIND_HOME" dot-a.0 "$SESSION:fm-dot-a.0" s1792290013.1.4
[ "$("$REAL_TMUX" -L "$SOCKET" display-message -p -t "$SESSION:fm-dot-a.0" '#{window_name}')" = fm-dot-a ] \
  || fail "lab precondition: dotted tmux target no longer resolves to pane 0 of the neighbor"
run_capture "$BIND_HOME" dot-a.0 s1792290013.1.4
expect_json "missing dotted window must not read its neighbor" '
  .status == "unavailable" and (.reason | startswith("Output unavailable")) and .text == ""
'
! grep -Fq dot-neighbor-secret "$OUT" || fail "dotted id read the neighbor window's output"
# Positive control: the exact undotted window captures on its own. tmux refuses a
# "." in a window name, so the dotted task above can never have a live window.
write_task "$BIND_HOME" dot-a "$SESSION:fm-dot-a" s1792290014.1.5
until "$REAL_TMUX" -L "$SOCKET" capture-pane -p -t "=$SESSION:=fm-dot-a" 2>/dev/null | grep -Fq dot-neighbor-secret; do
  sleep 0.1
done
run_capture "$BIND_HOME" dot-a s1792290014.1.5
expect_json "exact window beside a dotted-id task" '
  .status == "ok" and (.text | contains("dot-neighbor-secret"))
'
pass "dotted id: missing exact window is unavailable; the exact undotted window is captured"

# Duplicate window names leave no single endpoint.
"$REAL_TMUX" -L "$SOCKET" new-window -d -t "$SESSION" -n fm-dot-a \
  "printf 'dot-twin-line\n'; sleep 600" || fail "could not start the duplicate pane"
run_capture "$BIND_HOME" dot-a s1792290014.1.5
expect_json "ambiguous exact window name" '
  .status == "unavailable" and (.reason | test("ambiguous")) and .text == ""
'
pass "ambiguous window name: unavailable, no text"

# Legal ids include a leading dash and ids past 64 characters. A leading dot is not legal.
run_capture "$BIND_HOME" "$LONG_ID" s1792290012.1.3
expect_json "legal long id with leading dash" '
  .status == "ok" and .task_id == $id and (.text | contains("long-id-line"))
' --arg id "$LONG_ID"
run_capture "$BIND_HOME" .hidden s1792290012.1.3
expect_json "leading dot id" '.status == "unknown_identity" and .text == ""'
pass "source-legal ids reach the snapshot; a leading dot is refused"

# --- Configured private-directory layout: honored for its own home only ---

mkdir -p "$PRIV_HOME" "$PRIV_DIRS/state" "$PRIV_DIRS/data" "$PRIV_DIRS/projects/alpha-worktree" "$PRIV_DIRS/config"
printf '## In flight\n\n## Queued\n\n## Done\n' > "$PRIV_DIRS/data/backlog.md"
fm_write_meta "$PRIV_DIRS/state/priv-task.meta" \
  "window=$SESSION:fm-priv-task" "worktree=$PRIV_DIRS/projects/alpha-worktree" "project=alpha" \
  "harness=claude" "kind=ship" "mode=ship" "yolo=off" "spawn_gen=$GEN"
make_home "$OTHER_HOME"
"$REAL_TMUX" -L "$SOCKET" new-window -d -t "$SESSION" -n fm-priv-task \
  "printf 'priv-line\n'; sleep 600" || fail "could not start the private-layout pane"
until "$REAL_TMUX" -L "$SOCKET" capture-pane -p -t "=$SESSION:=fm-priv-task" 2>/dev/null | grep -Fq priv-line; do
  sleep 0.1
done
priv_capture() {  # <inherited FM_HOME>
  FM_HOME=$1 FM_STATE_OVERRIDE="$PRIV_DIRS/state" FM_DATA_OVERRIDE="$PRIV_DIRS/data" \
    FM_PROJECTS_OVERRIDE="$PRIV_DIRS/projects" FM_CONFIG_OVERRIDE="$PRIV_DIRS/config" \
    "$SCRIPT" --root "$PRIV_HOME" --task priv-task --spawn-gen "$GEN" >"$OUT" 2>"$ERR"
  RC=$?
}
priv_capture "$PRIV_HOME"
[ "$RC" -eq 0 ] || fail "private-layout capture exited $RC: $(head -c 300 "$ERR")"
expect_json "private-directory layout of its own home" '
  .status == "ok" and .spawn_gen_current == $gen and (.text | contains("priv-line"))
' --arg gen "$GEN"
priv_capture "$OTHER_HOME"
[ "$RC" -eq 0 ] || fail "cross-home capture exited $RC: $(head -c 300 "$ERR")"
expect_json "overrides inherited from another home" '.status == "unknown_identity" and .text == ""'
pass "private-directory layout: honored for its own home, cleared when inherited from another"

echo "all birds-eye capture checks passed"
