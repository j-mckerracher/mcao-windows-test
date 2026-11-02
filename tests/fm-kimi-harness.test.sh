#!/usr/bin/env bash
# Behavior tests for the verified Kimi Code CLI crewmate adapter.
set -u

# shellcheck source=tests/lib.sh
. "$(dirname "${BASH_SOURCE[0]}")/lib.sh"

# bin/fm-harness.sh answers from environment markers and process ancestry. A
# suite run from inside Cursor, Claude, Pi, or Grok inherits those markers and
# its own real ancestry, either of which can decide a case the detection cases
# meant to control. Drop the ambient markers so the asserted verdict does not
# depend on which harness launched the suite.
unset CLAUDECODE PI_CODING_AGENT FM_PI_HARNESS GROK_AGENT CURSOR_AGENT CURSOR_INVOKED_AS

SPAWN="$ROOT/bin/fm-spawn.sh"
KIMI_HOOK="$ROOT/bin/fm-kimi-turnend-hook.sh"
TMP_ROOT=$(fm_test_tmproot fm-kimi-harness)
KIMI_RUNTIME_TASK_TMP=
KIMI_RUNTIME_LAUNCH_DIR=
PYTHON_BIN=$(command -v python3) || fail "test needs python3"
PYTHON_BIN_DIR=$(dirname "$PYTHON_BIN")
JQ_BIN=$(command -v jq) || fail "test needs jq"
BASE_PATH=${FM_TEST_BASE_PATH:-$PYTHON_BIN_DIR:/usr/bin:/bin:/usr/sbin:/sbin}
# The Kimi hook validates and edits config.toml through the real mcao; lib.sh pins FM_MCAO_BIN to `true`.
FM_MCAO_BIN=${FM_TEST_MCAO_BIN:-$ROOT/target/debug/mcao}
[ -x "$FM_MCAO_BIN" ] || fail "build mcao first (cargo build -p mcao-cli --locked) or set FM_TEST_MCAO_BIN"
export FM_MCAO_BIN

task_inbox_export() {  # <home> <id>
  local state
  state=$(CDPATH='' cd -- "$1/state" && pwd -P) || fail "cannot resolve state dir $1/state"
  printf "export FM_TASK_INBOX='%s'; " "$state/$2.inbox"
}

ai_trailer_hooks_prefix() {  # <home> <id>
  local state
  state=$(CDPATH='' cd -- "$1/state" && pwd -P) || fail "cannot resolve state dir $1/state"
  printf "export GIT_CONFIG_COUNT=1 GIT_CONFIG_KEY_0=core.hooksPath GIT_CONFIG_VALUE_0='%s'; " "$state/$2.git-hooks"
}

cleanup_kimi_harness() {
  [ -z "$KIMI_RUNTIME_TASK_TMP" ] || fm_test_remove_tree "$KIMI_RUNTIME_TASK_TMP"
  [ -z "$KIMI_RUNTIME_LAUNCH_DIR" ] || fm_test_remove_tree "$KIMI_RUNTIME_LAUNCH_DIR"
  fm_test_remove_tree "$TMP_ROOT"
}
trap cleanup_kimi_harness EXIT

make_spawn_fakebin() {
  local dir=$1 fakebin
  fakebin=$(fm_fakebin "$dir")
  cat > "$fakebin/tmux" <<'SH'
#!/usr/bin/env bash
set -u
printf '%s\n' "$*" >> "$FM_FAKE_TMUX_CALL_LOG"
state=$(cat "$FM_FAKE_KIMI_STATE" 2>/dev/null || true)
fake_screen() {
  case "$state" in
    ready)
      printf 'Welcome to Kimi Code!\ncontext: 0%% (0/256k)\n╭────────────────────────────────╮\n│ >                              │\n╰────────────────────────────────╯\n'
      ;;
    trust)
      printf '╭─ Trust this folder? ─╮\n│ ↑↓ navigate · Enter select · Esc exit │\n│ %s │\n│ ❯ Trust this folder │\n│   Don'"'"'t trust │\n╰──────────────────────────────╯\n' "$FM_FAKE_PANE_PATH"
      ;;
    trust-decoy)
      printf 'Trust this folder?\n%s\n❯ Trust this folder\nDon'"'"'t trust\n' "$FM_FAKE_PANE_PATH"
      ;;
    trust-partial)
      printf 'Welcome to Kimi Code!\nTrust this folder?\n%s\nDon'"'"'t trust\n' "$FM_FAKE_PANE_PATH"
      ;;
    booting)
      printf 'shell starting\n$ \n'
      ;;
    banner-only|banner-first)
      printf 'Welcome to Kimi Code!\nstarting in %s\n' "$FM_FAKE_PANE_PATH"
      ;;
    blank-frame)
      ;;
    trust-wrapped)
      printf '╭─ Trust this folder? ─╮\n│ ↑↓ navigate ·        │\n│ Enter select · Esc   │\n│ exit                 │\n│ %s │\n│ ❯ Trust this folder  │\n│   Don'"'"'t trust         │\n╰──────────────────────╯\n' "$FM_FAKE_PANE_PATH"
      ;;
    pointer-typed)
      printf 'context: 0%% (0/256k)\n╭────────────────────────────────╮\n│ > Read the brief and follow it │\n│                                │\n╰────────────────────────────────╯\n'
      ;;
    delivered)
      printf '✨ Read the brief at %s and follow it exactly.\ncontext: 1%% (2k/256k)\n╭────────────────────────────────╮\n│ >                              │\n╰────────────────────────────────╯\n' "$FM_FAKE_BRIEF_REAL"
      ;;
    *)
      printf 'shell starting\n$ \n'
      ;;
  esac
}
fake_history() {
  if [ "${FM_FAKE_KIMI_HISTORY_KEEPS_DIALOG:-no}" = yes ] \
     && [ -s "$FM_FAKE_KIMI_TRUST_ENTER_LOG" ]; then
    printf '╭─ Trust this folder? ─╮\n│ ↑↓ navigate · Enter select · Esc exit │\n│ ❯ Trust this folder │\n│   Don'"'"'t trust │\n╰──────────────────────╯\n'
  fi
}
fake_cursor_y() {
  case "$state" in
    pointer-typed) printf '3\n' ;;
    ready|delivered) printf '3\n' ;;
    *) printf '1\n' ;;
  esac
}
case "$*" in
  *"#{pane_current_path}"*) printf '%s\n' "$FM_FAKE_PANE_PATH"; exit 0 ;;
  *"#{cursor_y}"*) fake_cursor_y; exit 0 ;;
esac
case "${1:-}" in
  display-message) printf 'firstmate\n'; exit 0 ;;
  list-windows) exit 0 ;;
  has-session|new-session|new-window|kill-window) exit 0 ;;
  send-keys)
    prev=
    literal=
    for arg in "$@"; do
      if [ "$prev" = -l ]; then literal=$arg; break; fi
      prev=$arg
    done
    if [ -n "$literal" ]; then
      case "$literal" in
        ". '"*"'") staged=${literal#". '"}; staged=${staged%"'"}; [ ! -f "$staged" ] || literal=$(cat "$staged") ;;
      esac
      case "$literal" in
        *' --auto')
          printf '%s\n' "$literal" >> "$FM_FAKE_LAUNCH_LOG"
          printf 'launched\n' > "$FM_FAKE_KIMI_STATE"
          ;;
        *)
          printf '%s\n' "$literal" >> "$FM_FAKE_POINTER_LOG"
          case "$state" in
            trust|trust-wrapped|trust-partial|trust-decoy|booting|banner-only|banner-first|blank-frame) ;;
            *) printf 'pointer-typed\n' > "$FM_FAKE_KIMI_STATE" ;;
          esac
          ;;
      esac
      exit 0
    fi
    case " $* " in
      *' Enter '*)
        case "$state" in
          launched)
            if [ "${FM_FAKE_KIMI_READY:-yes}" = yes ]; then
              case "${FM_FAKE_KIMI_TRUST:-remembered}" in
                fresh) printf 'trust\n' > "$FM_FAKE_KIMI_STATE" ;;
                decoy) printf 'trust-decoy\n' > "$FM_FAKE_KIMI_STATE" ;;
                partial) printf 'trust-partial\n' > "$FM_FAKE_KIMI_STATE" ;;
                late) printf 'booting\n' > "$FM_FAKE_KIMI_STATE" ;;
                blink) printf 'banner-first\n' > "$FM_FAKE_KIMI_STATE" ;;
                wrapped) printf 'trust-wrapped\n' > "$FM_FAKE_KIMI_STATE" ;;
                *) printf 'ready\n' > "$FM_FAKE_KIMI_STATE" ;;
              esac
            fi
            ;;
          trust|trust-wrapped)
            printf 'enter\n' >> "$FM_FAKE_KIMI_TRUST_ENTER_LOG"
            trust_enters=$(wc -l < "$FM_FAKE_KIMI_TRUST_ENTER_LOG" | tr -d ' ')
            case "${FM_FAKE_KIMI_TRUST_CLEARS:-yes}" in
              yes) printf 'ready\n' > "$FM_FAKE_KIMI_STATE" ;;
              after-second)
                [ "$trust_enters" -lt 2 ] || printf 'ready\n' > "$FM_FAKE_KIMI_STATE"
                ;;
            esac
            ;;
          ready|delivered)
            printf 'enter\n' >> "$FM_FAKE_KIMI_STRAY_ENTER_LOG"
            ;;
          pointer-typed)
            if [ "${FM_FAKE_KIMI_DELIVERY:-yes}" = yes ]; then
              if [ "${FM_FAKE_KIMI_SWALLOW_FIRST:-no}" = yes ] \
                 && [ ! -f "$FM_FAKE_KIMI_SWALLOWED" ]; then
                : > "$FM_FAKE_KIMI_SWALLOWED"
              else
                printf 'delivered\n' > "$FM_FAKE_KIMI_STATE"
              fi
            else
              printf 'ready\n' > "$FM_FAKE_KIMI_STATE"
            fi
            ;;
        esac
        ;;
    esac
    exit 0
    ;;
  capture-pane)
    start= end= prev=
    for arg in "$@"; do
      case "$prev" in
        -S) start=$arg ;;
        -E) end=$arg ;;
      esac
      case "$arg" in -S|-E) prev=$arg ;; *) prev= ;; esac
    done
    if [ "$start" = -0 ] && [ "${FM_FAKE_TMUX_VISIBLE_FAILS:-no}" = yes ]; then
      echo "can't find pane" >&2
      exit 1
    fi
    case "$start" in
      -0|-120)
        case "$state" in
          booting) printf 'banner-only\n' > "$FM_FAKE_KIMI_STATE" ;;
          banner-first) printf 'blank-frame\n' > "$FM_FAKE_KIMI_STATE" ;;
          blank-frame) printf 'banner-only\n' > "$FM_FAKE_KIMI_STATE" ;;
          banner-only) printf 'trust\n' > "$FM_FAKE_KIMI_STATE" ;;
        esac
        ;;
    esac
    if [ "$start" = -0 ] && [ "${FM_FAKE_KIMI_BLANK_AFTER_TRUST:-no}" = yes ] \
       && [ -s "$FM_FAKE_KIMI_TRUST_ENTER_LOG" ] && [ ! -f "$FM_FAKE_KIMI_BLANKED" ]; then
      : > "$FM_FAKE_KIMI_BLANKED"
      exit 0
    fi
    [ "$start" != -120 ] || fake_history
    case "$start:$end" in
      *[!0-9:]*|'':*|*:'') fake_screen ;;
      *) fake_screen | awk -v start="$start" -v end="$end" \
           'NR - 1 >= start && NR - 1 <= end' ;;
    esac
    exit 0
    ;;
esac
exit 0
SH
  chmod +x "$fakebin/tmux"
  fm_fake_exit0 "$fakebin" treehouse gh-axi gh
  fm_fake_exit0 "$fakebin" kimi
  ln -s "$JQ_BIN" "$fakebin/jq"
  printf '%s\n' "$fakebin"
}

make_spawn_case() {
  local name=$1 id=$2 case_dir home proj wt fakebin
  case_dir="$TMP_ROOT/$name"
  home="$case_dir/home"
  proj="$case_dir/project"
  wt="$case_dir/wt"
  fakebin=$(make_spawn_fakebin "$case_dir/fake")
  mkdir -p "$home/data/$id" "$home/projects" "$home/state" "$home/config" "$home/.kimi-code"
  printf '# Kimi test config\ndefault_model = "test"\n' > "$home/.kimi-code/config.toml"
  cat > "$home/data/$id/brief.md" <<'EOF'
# Task
## Captain's intent
Exercise Kimi dispatch.

## Firstmate spec
Verify launch and delivery behavior.
EOF
  printf 'kimi\n' > "$home/config/crew-harness"
  fm_git_worktree "$proj" "$wt" "wt-$name"
  touch "$home/state/.last-watcher-beat"
  : > "$case_dir/launch.log"
  : > "$case_dir/pointer.log"
  : > "$case_dir/kimi.state"
  : > "$case_dir/trust-enter.log"
  : > "$case_dir/stray-enter.log"
  : > "$case_dir/tmux-calls.log"
  printf '%s\n' "$case_dir|$home|$proj|$wt|$fakebin"
}

run_spawn() {
  local case_dir=$1 home=$2 proj=$3 wt=$4 fakebin=$5 id=$6
  shift 6
  HOME="$home" FM_ROOT_OVERRIDE='' FM_HOME="$home" \
    FM_STATE_OVERRIDE="$home/state" FM_DATA_OVERRIDE="$home/data" \
    FM_PROJECTS_OVERRIDE="$home/projects" FM_CONFIG_OVERRIDE="$home/config" \
    FM_SPAWN_NO_GUARD=1 FM_FAKE_PANE_PATH="$wt" TMUX="fake,1,0" \
    FM_FAKE_LAUNCH_LOG="$case_dir/launch.log" \
    FM_FAKE_POINTER_LOG="$case_dir/pointer.log" \
    FM_FAKE_KIMI_STATE="$case_dir/kimi.state" \
    FM_FAKE_KIMI_TRUST_ENTER_LOG="$case_dir/trust-enter.log" \
    FM_FAKE_KIMI_TRUST="${FM_FAKE_KIMI_TRUST:-remembered}" \
    FM_FAKE_KIMI_TRUST_CLEARS="${FM_FAKE_KIMI_TRUST_CLEARS:-yes}" \
    FM_FAKE_KIMI_HISTORY_KEEPS_DIALOG="${FM_FAKE_KIMI_HISTORY_KEEPS_DIALOG:-no}" \
    FM_FAKE_KIMI_STRAY_ENTER_LOG="$case_dir/stray-enter.log" \
    FM_FAKE_KIMI_BLANK_AFTER_TRUST="${FM_FAKE_KIMI_BLANK_AFTER_TRUST:-no}" \
    FM_FAKE_KIMI_BLANKED="$case_dir/kimi.blanked" \
    FM_FAKE_TMUX_VISIBLE_FAILS="${FM_FAKE_TMUX_VISIBLE_FAILS:-no}" \
    FM_FAKE_KIMI_SWALLOWED="$case_dir/kimi.swallowed" \
    FM_FAKE_KIMI_SWALLOW_FIRST="${FM_FAKE_KIMI_SWALLOW_FIRST:-no}" \
    FM_FAKE_TMUX_CALL_LOG="$case_dir/tmux-calls.log" \
    FM_FAKE_BRIEF_REAL="$(cd "$home/data/$id" && pwd -P)/launch-brief.md" \
    FM_KIMI_READY_POLLS="${FM_KIMI_READY_POLLS:-2}" FM_KIMI_DELIVERY_POLLS=2 FM_KIMI_POLL_INTERVAL=0 \
    PATH="$fakebin:$BASE_PATH" \
    "$SPAWN" "$id" "$proj" --harness kimi --mode no-mistakes --yolo off "$@" 2>&1
}

path_mode() {
  stat -c %a "$1" 2>/dev/null || stat -f %Lp "$1" 2>/dev/null
}

kimi_launch_dir() {
  local id=$1 home=$2 root hash
  root=$(cd "$home" 2>/dev/null && pwd -P) || root=$home
  if command -v shasum >/dev/null 2>&1; then
    hash=$(printf '%s' "$root" | shasum -a 256 | awk '{print $1}')
  elif command -v sha256sum >/dev/null 2>&1; then
    hash=$(printf '%s' "$root" | sha256sum | awk '{print $1}')
  else
    fail "test needs shasum or sha256sum"
  fi
  printf '/tmp/fm-%s+%s' "$id" "$hash"
}

kimi_typed_launch_file() {
  local log=$1 src
  src=$(grep -o "\. '/tmp/fm-[^']*'" "$log" | tail -1)
  src=${src#". '"}
  src=${src%"'"}
  [ -n "$src" ] || fail "spawn did not type a staged launch source line"
  printf '%s' "$src"
}

test_kimi_hook_install_is_surgical_idempotent_and_removable() {
  local home config original once stripped count
  home="$TMP_ROOT/config-surgery"
  config="$home/.kimi-code/config.toml"
  original="$home/original.toml"
  once="$home/once.toml"
  stripped="$home/stripped.toml"
  mkdir -p "$home/.kimi-code"
  cat > "$config" <<'EOF'
# Captain's leading comment stays exactly here.

[ui]
theme = "night" # inline comment
show_usage = true

# Foreign hook with intentionally unusual key ordering.
[[hooks]]
timeout=17
command = "printf foreign"
matcher=""
event = "Stop"

[providers.example]
model = "some/model"
# Final comment and blank line follow.

EOF
  cp "$config" "$original"

  HOME="$home" "$KIMI_HOOK" install || fail "Kimi hook install refused a realistic config"
  cp "$config" "$once"
  HOME="$home" "$KIMI_HOOK" install || fail "second Kimi hook install failed"
  cmp -s "$once" "$config" || fail "second Kimi hook install changed config bytes"
  count=$(grep -c '^# BEGIN FIRSTMATE KIMI TURN-END HOOK' "$config")
  [ "$count" -eq 1 ] || fail "idempotent install left $count Firstmate regions"

  HOME="$home" "$KIMI_HOOK" remove || fail "Kimi hook removal failed"
  cp "$config" "$stripped"
  cmp -s "$original" "$stripped" \
    || fail "config with the Firstmate region excised was not byte-identical to the original"
  assert_absent "$home/.kimi-code/fm-turn-end.sh" "removal left the Firstmate hook script"
  assert_absent "$home/.kimi-code/fm-turn-end.d" "removal left the Firstmate registry"
  pass "Kimi hook install is idempotent and removal restores every foreign config byte"
}

test_kimi_hook_remove_preserves_owned_newline_boundary() {
  local appended config expected home original
  home="$TMP_ROOT/config-owned-newline"
  config="$home/.kimi-code/config.toml"
  original="$home/original.toml"
  expected="$home/expected.toml"
  appended="$home/appended.toml"
  mkdir -p "$home/.kimi-code"
  printf 'default_model = "test"' > "$config"
  cp "$config" "$original"

  HOME="$home" "$KIMI_HOOK" install || fail "Kimi hook install refused config without a final newline"
  HOME="$home" "$KIMI_HOOK" remove || fail "Kimi hook removal failed without appended config"
  cmp -s "$original" "$config" \
    || fail "pristine removal did not restore the absent final newline byte-identically"

  HOME="$home" "$KIMI_HOOK" install || fail "second Kimi hook install refused config without a final newline"
  printf '[captain]\nenabled = true\n' > "$appended"
  cat "$appended" >> "$config"
  HOME="$home" "$KIMI_HOOK" remove || fail "Kimi hook removal joined config appended after its region"
  {
    cat "$original"
    printf '\n'
    cat "$appended"
  } > "$expected"
  cmp -s "$expected" "$config" \
    || fail "removal did not preserve appended captain config on its own line"
  "$PYTHON_BIN" - "$config" <<'PY' || fail "config with appended captain TOML did not parse after removal"
import sys
import tomllib

with open(sys.argv[1], "rb") as stream:
    tomllib.load(stream)
PY
  pass "Kimi hook removal preserves owned newline boundaries and pristine bytes"
}

test_kimi_hook_fails_closed_on_missing_malformed_or_partial_config() {
  local missing malformed partial out rc
  missing="$TMP_ROOT/config-missing"
  malformed="$TMP_ROOT/config-malformed"
  partial="$TMP_ROOT/config-partial"
  mkdir -p "$missing/.kimi-code" "$malformed/.kimi-code" "$partial/.kimi-code"

  rc=0
  out=$(HOME="$missing" "$KIMI_HOOK" install 2>&1) || rc=$?
  [ "$rc" -ne 0 ] || fail "missing Kimi config was accepted"
  assert_contains "$out" "Kimi config is missing" "missing config refusal lacked its concrete reason"
  assert_absent "$missing/.kimi-code/fm-turn-end.sh" "missing config refusal wrote the hook script"

  printf '[broken\n' > "$malformed/.kimi-code/config.toml"
  cp "$malformed/.kimi-code/config.toml" "$malformed/before"
  rc=0
  out=$(HOME="$malformed" "$KIMI_HOOK" install 2>&1) || rc=$?
  [ "$rc" -ne 0 ] || fail "malformed Kimi config was accepted"
  assert_contains "$out" "malformed TOML" "malformed config refusal lacked its concrete reason"
  cmp -s "$malformed/before" "$malformed/.kimi-code/config.toml" \
    || fail "malformed config refusal changed config bytes"
  assert_absent "$malformed/.kimi-code/fm-turn-end.sh" "malformed config refusal wrote the hook script"

  printf '# BEGIN FIRSTMATE KIMI TURN-END HOOK\n' > "$partial/.kimi-code/config.toml"
  cp "$partial/.kimi-code/config.toml" "$partial/before"
  rc=0
  out=$(HOME="$partial" "$KIMI_HOOK" install 2>&1) || rc=$?
  [ "$rc" -ne 0 ] || fail "partial Firstmate marker was accepted"
  assert_contains "$out" "partial, duplicated, or altered" "partial marker refusal lacked its concrete reason"
  cmp -s "$partial/before" "$partial/.kimi-code/config.toml" \
    || fail "partial marker refusal changed config bytes"
  pass "Kimi hook install refuses missing, malformed, and surprising config without writing"
}

test_kimi_hook_install_refuses_without_jq() {
  local home config before fakebin out rc
  home="$TMP_ROOT/config-no-jq"
  config="$home/.kimi-code/config.toml"
  before="$home/config-before.toml"
  fakebin=$(fm_fakebin "$home/no-jq")
  mkdir -p "$home/.kimi-code"
  printf '# Captain config\nmodel = "test"\n' > "$config"
  cp "$config" "$before"
  ln -s "$(command -v bash)" "$fakebin/bash"

  rc=0
  out=$(HOME="$home" PATH="$fakebin" "$KIMI_HOOK" install 2>&1) || rc=$?
  [ "$rc" -ne 0 ] || fail "Kimi hook install succeeded without jq"
  assert_contains "$out" "jq is required" "missing-jq refusal did not name jq"
  cmp -s "$before" "$config" || fail "missing-jq refusal changed config bytes"
  assert_absent "$home/.kimi-code/fm-turn-end.sh" "missing-jq refusal wrote the hook script"
  assert_absent "$home/.kimi-code/fm-turn-end.d" "missing-jq refusal wrote the registry"
  pass "Kimi hook install refuses without jq before any config write"
}

test_kimi_detection_uses_ancestry_after_markers() {
  local dir fakebin cfg out
  dir="$TMP_ROOT/detection"
  fakebin=$(fm_fakebin "$dir")
  cfg="$dir/config"
  mkdir -p "$cfg"
  cat > "$fakebin/ps" <<'SH'
#!/usr/bin/env bash
set -u
field=
pid=
prev=
for arg in "$@"; do
  [ "$prev" = -o ] && field=$arg
  [ "$prev" = -p ] && pid=$arg
  prev=$arg
done
case "$field:$pid" in
  comm=:4242) printf '/opt/kimi/bin/kimi\n' ;;
  comm=:*) printf '/bin/bash\n' ;;
  ppid=:4242) printf '1\n' ;;
  ppid=:*) printf '4242\n' ;;
  args=:*) printf 'bash\n' ;;
esac
SH
  chmod +x "$fakebin/ps"

  out=$(env -u CLAUDECODE -u PI_CODING_AGENT -u FM_PI_HARNESS -u GROK_AGENT \
    -u CURSOR_AGENT -u CURSOR_INVOKED_AS -u GEMINI_CLI \
    PATH="$fakebin:$BASE_PATH" FM_CONFIG_OVERRIDE="$cfg" "$ROOT/bin/fm-harness.sh")
  [ "$out" = kimi ] || fail "kimi ancestry detection returned '$out'"
  # Kimi publishes no identity marker, so an inherited CLAUDECODE used to rename
  # it outright. A structural kimi ancestor now outranks that marker;
  # tests/fm-harness-precedence.test.sh owns the general boundary.
  out=$(env -u CURSOR_AGENT -u CURSOR_INVOKED_AS -u GEMINI_CLI \
    CLAUDECODE=1 PATH="$fakebin:$BASE_PATH" FM_CONFIG_OVERRIDE="$cfg" "$ROOT/bin/fm-harness.sh")
  [ "$out" = kimi ] || fail "an inherited CLAUDECODE renamed markerless kimi, got '$out'"
  pass "fm-harness: markerless kimi keeps its ancestry identity under an inherited marker"
}

test_kimi_session_lock_identity() {
  local home fakebin out
  home="$TMP_ROOT/session-lock-home"
  fakebin=$(fm_fakebin "$TMP_ROOT/session-lock-fake")
  mkdir -p "$home/state"
  cat > "$fakebin/ps" <<'SH'
#!/usr/bin/env bash
case "$*" in
  *"comm="*) printf '%s\n' '/opt/kimi/bin/kimi'; exit 0 ;;
  *"args="*) printf '%s\n' 'kimi'; exit 0 ;;
esac
exit 1
SH
  chmod +x "$fakebin/ps"

  FM_HOME="$home" PATH="$fakebin:$BASE_PATH" "$ROOT/bin/fm-lock.sh" \
    || fail "fm-lock did not acquire from Kimi ancestry"
  case "$(cat "$home/state/.lock")" in
    ''|*[!0-9]*) fail "fm-lock did not record the Kimi harness ancestor" ;;
  esac
  printf '%s\n' "$$" > "$home/state/.lock"
  out=$(FM_HOME="$home" PATH="$fakebin:$BASE_PATH" "$ROOT/bin/fm-lock.sh" status)
  assert_contains "$out" "lock: held by live harness pid" \
    "fm-lock did not recognize Kimi as a live holder"
  pass "fm-lock recognizes Kimi ancestry and live lock holders"
}

test_kimi_busy_signature_is_scoped_to_spinner_lines() {
  local capture
  # shellcheck source=/dev/null
  . "$ROOT/bin/fm-tmux-lib.sh"
  unset FM_BUSY_REGEX
  capture="$TMP_ROOT/busy-pane"
  tmux() {
    case "${1:-}" in
      capture-pane) cat "$capture" ;;
      *) return 0 ;;
    esac
  }
  # These fixtures reproduce the observed spinner shape rather than byte-exact
  # transcriptions. Leading whitespace is deliberately varied; separator whitespace
  # follows the captured contract.
  local phase
  for phase in 🌑 🌒 🌓 🌔 🌕 🌖 🌗 🌘; do
    printf '  %s · Tip: Kimi is working\n│ > │\n' "$phase" > "$capture"
    fm_pane_is_busy fake kimi || fail "Kimi spinner phase $phase was not recognized as busy"
  done
  printf 'ordinary response ending with 🌕\n│ > │\n' > "$capture"
  if fm_pane_is_busy fake kimi; then
    fail "a moon outside Kimi's spinner-line shape was misread as busy"
  fi
  printf '🌕 Full moon details\n│ > │\n' > "$capture"
  if fm_pane_is_busy fake kimi; then
    fail "moon-led Kimi output without the middot separator was misread as busy"
  fi
  printf '  🌗 · Tip: /plugins: manage plugins ...\n│ > │\n' > "$capture"
  if fm_pane_is_busy fake codex; then
    fail "Kimi's real spinner signature leaked into another harness"
  fi
  printf 'tip: ctrl+c: cancel\n│ > │\n' > "$capture"
  if fm_pane_is_busy fake kimi; then
    fail "kimi's independently rotating idle tip was misread as busy"
  fi
  printf 'Ctrl+c:cancel\n│ > │\n' > "$capture"
  if fm_pane_is_busy fake kimi; then
    fail "Grok's exact busy token leaked into Kimi's harness-scoped matcher"
  fi
  printf 'auto  K2.7 Coding thinking  /some/path\n│ > │\n' > "$capture"
  if fm_pane_is_busy fake kimi; then
    fail "Kimi's idle thinking-effort status label was misread as busy"
  fi
  pass "busy detection: real Kimi moon-plus-middot captures require its harness while idle labels stay idle"
}

test_watcher_never_classifies_kimi_from_its_spinner() (
  local state="$TMP_ROOT/watch-state" busy_capture='  🌑 · Tip: ask Kimi to schedule tasks, e.g. "remind me at 5pm"'
  mkdir -p "$state"
  printf 'window=fake\nharness=kimi\n' > "$state/kimi-watch.meta"
  unset FM_BUSY_REGEX
  FM_HOME="$TMP_ROOT/watch-home"
  FM_STATE_OVERRIDE="$state"
  export FM_HOME FM_STATE_OVERRIDE
  # shellcheck source=/dev/null
  . "$ROOT/bin/fm-watch.sh"
  # shellcheck disable=SC2329 # Runtime override called by the sourced watcher.
  fm_backend_busy_state() { printf 'unknown'; }
  # Standalone Kimi has no verified semantic busy source, so it classifies
  # unknown - and unknown is never working. Its moon-phase spinner is
  # deliberately not a state source: the approved redesign forbids inventing a
  # Kimi UI signature, and that glyph set is locale- and emoji-font-sensitive.
  if window_is_busy fake "$busy_capture"; then
    fail "fm-watch classified a Kimi task busy from its spinner instead of unknown"
  fi
  [ "$(fm_busy_classify tmux fake kimi kimi-watch "$state" "$busy_capture")" = "unknown kimi-unverified" ] \
    || fail "a Kimi task must classify unknown kimi-unverified"
  printf 'window=fake\nharness=codex\n' > "$state/kimi-watch.meta"
  if window_is_busy fake "$busy_capture"; then
    fail "fm-watch applied Kimi's spinner to a recorded Codex task"
  fi
  printf 'window=fake\nharness=grok\n' > "$state/kimi-watch.meta"
  if window_is_busy fake "$busy_capture"; then
    fail "Kimi's spinner classified a recorded Grok task through its isolated fallback"
  fi
  window_is_busy fake 'Ctrl+c:cancel' \
    || fail "Grok's own verified token must still classify a recorded Grok task busy"
  pass "fm-watch classifies Kimi as unknown rather than from its spinner, and Grok's fallback stays isolated"
)

test_kimi_bordered_prompt_needs_no_override() {
  local out
  # shellcheck source=/dev/null
  . "$ROOT/bin/fm-composer-lib.sh"
  out=$(fm_composer_classify_content 1 '>')
  [ "$out" = empty ] || fail "kimi's bordered bare > composer should read empty, got '$out'"
  out=$(fm_composer_classify_content 0 '>')
  [ "$out" = unknown ] || fail "an unbordered dead-shell > must stay unknown, got '$out'"
  pass "composer classifier: kimi's existing bordered > shape is already safe without an override"
}

test_kimi_hook_install_is_surgical_idempotent_and_removable
test_kimi_hook_remove_preserves_owned_newline_boundary
test_kimi_hook_fails_closed_on_missing_malformed_or_partial_config
test_kimi_hook_install_refuses_without_jq
test_kimi_detection_uses_ancestry_after_markers
test_kimi_session_lock_identity
test_kimi_busy_signature_is_scoped_to_spinner_lines
test_watcher_never_classifies_kimi_from_its_spinner
test_kimi_bordered_prompt_needs_no_override
