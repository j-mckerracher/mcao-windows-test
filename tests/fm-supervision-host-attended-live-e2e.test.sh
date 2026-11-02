#!/usr/bin/env bash
# Opt-in credentialed live guard for an attended hand-back to an idle Claude
# primary (bin/fm-supervision-host.sh main-only pass-through,
# bin/fm-claude-stop-autoarm.sh, docs/supervision-host.md "Attended").
#
# Proves against the real installed Claude Code, in an isolated lab copy of this
# checkout opted into the supervision host (never a live fleet home), that an
# interactive primary sitting idle at its prompt - nobody types after its setup
# prompt - is woken by the tracked Stop hook for every close the host hands to
# main, across repeated hand-offs:
#   1. the primary is idle with the tracked Stop hook registered and the host
#      parked on a live watcher;
#   2. a main-only status event passes through the host and leaves a live
#      successor watcher, and the hook's rewake (ledger outcome=rewake, banner
#      delivered) starts a primary turn that drains and acknowledges it;
#   3. that turn's end arms onto the successor, and a second main-only event is
#      delivered the same way; it closes that successor, so the successor's own
#      close is read instead of left in an unread capture;
#   4. a routine close on another task that the host accepts for the
#      supervision session, and hands to its successor as handling, but that
#      turns main-only (a decision lands) before its turn starts, is handed
#      back to main and delivered the same way, with no engine turn.
# With FM_SUPERVISION_HOST_ATTENDED_LIVE_CONTROL_REF=<git ref>, the scenario
# first runs on that ref's host as a negative control and must show the idle
# primary NOT woken by the first event, so the scenario is proven able to catch
# a dropped hand-back. Evidence lines start with "# ".
#
#   FM_SUPERVISION_HOST_ATTENDED_LIVE_E2E=1 tests/fm-supervision-host-attended-live-e2e.test.sh
#
# FM_SUPERVISION_HOST_ATTENDED_LIVE_MODEL (default haiku) picks the primary's
# model. Claude keeps its existing managed authentication; the lab path gets a
# workspace-trust entry and a project transcript directory in Claude's own store.
# shellcheck disable=SC2016 # single-quoted scripts expand inside their own shells
set -u

# shellcheck source=tests/lib.sh
. "$(dirname "${BASH_SOURCE[0]}")/lib.sh"
# shellcheck source=bin/fm-tmux-lib.sh
. "$ROOT/bin/fm-tmux-lib.sh"

fm_live_gate opt-in FM_SUPERVISION_HOST_ATTENDED_LIVE_E2E claude tmux jq node perl git

CLAUDE_VERSION=$(claude --version 2>/dev/null | head -n 1)
MODEL=${FM_SUPERVISION_HOST_ATTENDED_LIVE_MODEL:-haiku}
CONTROL_REF=${FM_SUPERVISION_HOST_ATTENDED_LIVE_CONTROL_REF:-}
LAB=$(fm_test_tmproot fm-sh-attended-live)
LAB=$(cd -P "$LAB" && pwd -P)
SOCKET="fmshal-$$"
PROJECTS="${CLAUDE_CONFIG_DIR:-$HOME/.claude}/projects"
TURN_POLLS=${FM_SUPERVISION_HOST_ATTENDED_LIVE_POLLS:-1800}
CONTROL_QUIET_SECONDS=${FM_SUPERVISION_HOST_ATTENDED_LIVE_CONTROL_SECONDS:-90}
unset FM_HOME FM_ROOT_OVERRIDE FM_STATE_OVERRIDE FM_CONFIG_OVERRIDE FM_DATA_OVERRIDE TMUX TMUX_PANE PI_CODING_AGENT NO_MISTAKES_GATE
# Claude Code keeps no transcript for a session that inherits another session's
# markers, so the lab primary starts without the invoking session's.
while IFS= read -r name; do
  unset "$name"
done < <(env | grep -E '^(CLAUDECODE|CLAUDE_CODE_[A-Z_]+|CLAUDE_PID|CLAUDE_EFFORT)=' | cut -d= -f1 | sort -u)

evidence() { printf '# %s %s\n' "$(date '+%H:%M:%S')" "$*"; }

stop_lab() {  # <lab>
  local lab=$1 fm=$1/fm pid
  tmux -L "$SOCKET-$(basename "$lab")" kill-server >/dev/null 2>&1 || true
  sleep 1
  if [ -f "$fm/state/.supervision-host" ]; then
    pid=$(awk -F '\t' '$1 == "host" { print $2; exit }' "$fm/state/.supervision-host")
    [ -z "$pid" ] || kill -TERM "$pid" 2>/dev/null || true
  fi
  pid=$(cat "$fm/state/.watch.lock/pid" 2>/dev/null || true)
  [ -z "$pid" ] || kill -TERM "$pid" 2>/dev/null || true
  FM_HOME="$fm" FM_PROCEVENT_CLAIM_ROOT="$lab/claims" "$fm/bin/fm-procevent.sh" sweep-home >/dev/null 2>&1 || true
  rm -rf "${PROJECTS:?}/$(printf '%s' "$fm" | sed 's/[^A-Za-z0-9]/-/g')"
}
cleanup() {
  local lab
  for lab in "$LAB"/*/; do
    [ -d "$lab/fm" ] && stop_lab "${lab%/}"
  done
  fm_test_cleanup
}
trap cleanup EXIT
# An interrupted run still stops its labs before tests/lib.sh removes them.
trap 'exit 130' INT
trap 'exit 143' TERM
trap 'exit 129' HUP

wait_until() {  # <polls of 0.1s> <command...>
  local limit=$1 i=0
  shift
  while [ "$i" -lt "$limit" ]; do
    "$@" && return 0
    sleep 0.1
    i=$((i + 1))
  done
  return 1
}

# --- one lab ------------------------------------------------------------------

make_lab() {  # <name> [host-ref]
  local lab="$LAB/$1" ref=${2:-} fm remote
  fm="$lab/fm"
  remote="$lab/remote"
  mkdir -p "$fm" "$lab/bin" "$lab/claims"
  git -C "$ROOT" ls-files -z -co --exclude-standard \
    | (cd "$ROOT" && tar --null -T - -cf -) | (cd "$fm" && tar -xf -)
  if [ -n "$ref" ]; then
    git -C "$ROOT" show "$ref:bin/fm-supervision-host.sh" > "$fm/bin/fm-supervision-host.sh" \
      || fail "control: could not read bin/fm-supervision-host.sh at $ref"
  fi
  git -C "$fm" init -q -b main
  git -C "$fm" add -A >/dev/null
  git -C "$fm" -c user.name=fmtest -c user.email=fmtest@example.invalid commit -q -m lab
  mkdir -p "$fm/state" "$fm/config" "$fm/data"
  : > "$fm/config/supervision-host"
  printf 'project=demo\nwindow=fm-demo\nharness=claude\n' > "$fm/state/demo.meta"
  : > "$fm/state/demo.status"
  printf 'project=demo2\nwindow=fm-demo2\nharness=claude\n' > "$fm/state/demo2.meta"
  : > "$fm/state/demo2.status"
  # An unreachable tmux: the watcher reads no endpoint, so the only wakes are
  # the events this guard appends.
  printf '#!/usr/bin/env bash\nexit 1\n' > "$lab/bin/tmux"
  chmod +x "$lab/bin/tmux"
  cat > "$lab/env" <<ENV
export FM_HOME='$fm'
export FM_POLL=1 FM_SIGNAL_GRACE=0 FM_CHECK_INTERVAL=999999 FM_HEARTBEAT=999999
export FM_PROCEVENT_CLAIM_ROOT='$lab/claims'
export FM_SSH_BIN='$lab/bin/ssh'
export CLAUDE_CODE_ENABLE_PROMPT_SUGGESTION=false CLAUDE_CODE_SEND_FEEDBACK=0
export PATH='$lab/bin':"\$PATH"
ENV
  # shellcheck source=/dev/null
  printf '%s\n' "$lab"
}

# Claude's own project transcript for the lab checkout.
transcript() {  # <lab>
  local dir
  dir="$PROJECTS/$(printf '%s' "$1/fm" | sed 's/[^A-Za-z0-9]/-/g')"
  find "$dir" -maxdepth 1 -name '*.jsonl' -print 2>/dev/null | head -n 1
}
# Epochs of rewake deliveries (Claude's queued "Stop hook feedback") at or after <epoch>.
rewakes_since() {  # <lab> <epoch>
  local t
  t=$(transcript "$1")
  [ -n "$t" ] || return 0
  jq -r --argjson since "$2" '
    select(.type == "queue-operation" and .operation == "enqueue")
    | select((.content // "" | tostring) | contains("firstmate watcher wake"))
    | (.timestamp | sub("\\.[0-9]+Z$"; "Z") | fromdateiso8601) as $at
    | select($at >= $since) | $at' "$t" 2>/dev/null
}
rewoke_since() { [ -n "$(rewakes_since "$1" "$2")" ]; }
# Bash commands the primary ran at or after <epoch>.
commands_since() {  # <lab> <epoch>
  local t
  t=$(transcript "$1")
  [ -n "$t" ] || return 0
  jq -r --argjson since "$2" '
    select(.type == "assistant")
    | (.timestamp | sub("\\.[0-9]+Z$"; "Z") | fromdateiso8601) as $at
    | select($at >= $since)
    | .message.content[]? | select(.type == "tool_use" and .name == "Bash") | .input.command' "$t" 2>/dev/null
}
acked_since() {  # <lab> <epoch>: a turn drained and ran its generation-bound acknowledgement
  commands_since "$1" "$2" | grep -q 'fm-wake-drain.sh --ack-through [0-9]* --recovery-generation'
}
turn_idle() {  # <lab> <after-epoch>: a turn ended at or after the epoch
  local t
  t=$(transcript "$1")
  [ -n "$t" ] || return 1
  jq -e --argjson since "$2" '
    select(.type == "system" and .subtype == "turn_duration")
    | select((.timestamp | sub("\\.[0-9]+Z$"; "Z") | fromdateiso8601) >= $since)' "$t" >/dev/null 2>&1
}
host_log_since() {  # <lab> <epoch> <regex>
  awk -F '\t' -v t="$2" '$1 >= t' "$1/fm/state/.supervision-host.log" 2>/dev/null | grep -E -- "$3"
}
host_live() {
  local pid
  pid=$(awk -F '\t' '$1 == "host" { print $2; exit }' "$1/fm/state/.supervision-host" 2>/dev/null)
  [ -n "$pid" ] && kill -0 "$pid" 2>/dev/null
}
watcher_pid() { cat "$1/fm/state/.watch.lock/pid" 2>/dev/null; }
watcher_live() { local pid; pid=$(watcher_pid "$1") && [ -n "$pid" ] && kill -0 "$pid" 2>/dev/null; }
ledger() { head -n 1 "$1/fm/state/.claude-autoarm-epoch" 2>/dev/null; }
marker() { cat "$1/fm/state/.watcher-down" 2>/dev/null; }
captain_prompts() { jq -r 'select(.tag == "captain") | .seq' "$1/fm/state/.host-mirror.jsonl" 2>/dev/null | wc -l | tr -d ' '; }
diagnose() {  # <lab>
  printf -- '--- host log\n%s\n--- cycle exits\n%s\n--- queue\n%s\n--- ledger: %s\n--- marker: %s\n--- screen\n%s\n' \
    "$(tail -n 8 "$1/fm/state/.supervision-host.log" 2>/dev/null)" \
    "$(tail -n 4 "$1/fm/state/.watch-cycle-exits.log" 2>/dev/null | cut -f1-8)" \
    "$(cat "$1/fm/state/.wake-queue" 2>/dev/null)" "$(ledger "$1")" "$(marker "$1")" \
    "$(tmux -L "$SOCKET-$(basename "$1")" capture-pane -p -t primary 2>/dev/null | tail -n 25)"
}

# Answer one first-run dialog in the lab by moving its cursor to <option> and
# confirming, whether the options are numbered or not.
choose() {  # <socket> <screen> <option>
  local cursor target moves key
  cursor=$(printf '%s\n' "$2" | grep -n '❯' | head -n 1 | cut -d: -f1)
  target=$(printf '%s\n' "$2" | grep -nF -- "$3" | head -n 1 | cut -d: -f1)
  [ -n "$cursor" ] && [ -n "$target" ] || return 0
  moves=$((target - cursor))
  key=Down
  [ "$moves" -ge 0 ] || { key=Up; moves=$((0 - moves)); }
  while [ "$moves" -gt 0 ]; do
    tmux -L "$1" send-keys -t primary "$key"
    sleep 0.3
    moves=$((moves - 1))
  done
  tmux -L "$1" send-keys -t primary Enter
  sleep 3
}

# A bare tmux launch has no native agent-idle event. The Claude-scoped busy
# read and shared composer classifier must both report an idle input surface.
claude_ready_state() {  # <socket> <target> -> diagnostic state; success when ready
  local socket=$1 target=$2 busy composer
  tmux() { command tmux -L "$socket" "$@"; }
  busy=$(fm_pane_busy_state "$target" claude)
  composer=$(fm_tmux_composer_state "$target")
  printf 'busy=%s composer=%s' "$busy" "$composer"
  [ "$busy" = idle ] && [ "$composer" = empty ]
}

# Start the primary interactively in a private tmux server, answer the lab's
# first-run dialogs, submit the one setup prompt, and wait until it sits idle
# with the host parked on a live watcher.
start_primary() {  # <lab>
  local lab=$1 sock screen i started prompt readiness=unknown last_seen=none
  sock="$SOCKET-$(basename "$lab")"
  prompt='This is an isolated Firstmate test lab, not a real fleet. Reply with exactly READY now and use no tools. Later, whenever a "Stop hook feedback" message wakes you, do exactly this and nothing else: run `bin/fm-wake-drain.sh` once with the Bash tool, then run the exact `bin/fm-wake-drain.sh --ack-through ...` command that its WAKE_ACK_REQUIRED line prints, then reply with exactly ACKED. Never run any other command, never run bin/fm-watch-arm.sh, and never use any other tool.'
  started=$(date +%s)
  tmux -L "$sock" new-session -d -s primary -x 220 -y 50 -c "$lab/fm" \
    "sh -c '. \"$lab/env\"; printf \"%s\\n\" \"\$\$\" > state/.lock; exec claude --model $MODEL --effort low --dangerously-skip-permissions'" \
    || fail "$(basename "$lab"): the tmux session did not start"
  i=0
  while [ "$i" -lt 90 ]; do
    screen=$(tmux -L "$sock" capture-pane -p -t primary 2>/dev/null)
    last_seen=$(printf '%s\n' "$screen" | grep -v '^[[:space:]]*$' | tail -n 6)
    [ -n "$last_seen" ] || last_seen='empty screen'
    case "$screen" in
      *'Yes, I trust this folder'*) choose "$sock" "$screen" 'Yes, I trust this folder' ;;
      *'Yes, I accept'*) choose "$sock" "$screen" 'Yes, I accept' ;;
      *'external CLAUDE.md'*|*'external imports'*) choose "$sock" "$screen" 'Yes, allow external imports' ;;
      *) readiness=$(claude_ready_state "$sock" primary) && break ;;
    esac
    sleep 1
    i=$((i + 1))
  done
  [ "$i" -lt 90 ] || fail "$(basename "$lab"): Claude never reached an idle empty composer (last readiness: $readiness; last screen: $last_seen)"$'\n'"$(diagnose "$lab")"
  sleep 2
  tmux -L "$sock" send-keys -t primary -l "$prompt"
  sleep 1
  tmux -L "$sock" send-keys -t primary Enter
  wait_until "$TURN_POLLS" host_live "$lab" \
    || fail "$(basename "$lab"): the setup turn's Stop hook never started the supervision host"$'\n'"$(diagnose "$lab")"
  wait_until 300 watcher_live "$lab" || fail "$(basename "$lab"): the host never started a watcher"$'\n'"$(diagnose "$lab")"
  wait_until "$TURN_POLLS" turn_idle "$lab" "$started" || fail "$(basename "$lab"): the setup turn never ended"$'\n'"$(diagnose "$lab")"
  jq -e '[.hooks.Stop[]?.hooks[]? | select(.type == "command" and .asyncRewake == true and (.command | endswith("/bin/fm-claude-stop-autoarm.sh") or endswith("/bin/fm-claude-stop-autoarm.sh\"")))] | length == 1' \
    "$lab/fm/.claude/settings.json" >/dev/null \
    || fail "$(basename "$lab"): the lab lacks the tracked Stop hook registration"
  evidence "$(basename "$lab") step 1: primary idle (claude pid $(cat "$lab/fm/state/.lock"), $CLAUDE_VERSION, model $MODEL); tracked Stop hook registered; config/supervision-host present; host pid $(awk -F '\t' '$1 == "host" { print $2; exit }' "$lab/fm/state/.supervision-host") parked on watcher $(watcher_pid "$lab"); captain prompts so far: $(captain_prompts "$lab")"
  evidence "$(basename "$lab") transcript: $(transcript "$lab")"
}

# Append one main-only event and wait for the host's pass-through of it.
fire() {  # <lab> <status-file> <key> <text>
  local at
  at=$(date +%s)
  printf 'needs-decision [at=%s] [key=%s]: %s\n' "$at" "$3" "$4" >> "$2"
  printf '%s\n' "$at"
}

# Append <line> to <status-file> the moment the recovery marker turns to
# handling: the host has accepted the close for the supervision session and
# confirmed its successor's handling handoff, but not yet re-checked the close
# at its turn's start. Prints when it saw that and the marker it saw.
decide_at_handoff() { # <lab> <status-file> <line>
  perl -MTime::HiRes=time,sleep -e '
    my ($marker, $status, $line, $limit) = @ARGV;
    my $until = time + $limit;
    while (time < $until) {
      if (open my $in, "<", $marker) {
        my $token = <$in> // "";
        close $in;
        chomp $token;
        if ($token =~ /^(pending|announced):handling:/) {
          open my $out, ">>", $status or exit 2;
          print $out "$line\n";
          close $out;
          printf "%d %s\n", time, $token;
          exit 0;
        }
      }
      sleep 0.002;
    }
    exit 1' "$1/fm/state/.watcher-down" "$2" "$3" 600
}

# Steps 2-5 on the host under test: every hand-off reaches the idle primary.

# The negative control: the same first event on the control ref's host must
# leave the idle primary asleep.
run_control() {
  local lab e1
  lab=$(make_lab control "$CONTROL_REF")
  start_primary "$lab"
  e1=$(fire "$lab" "$lab/fm/state/demo.status" lab-e1 'pick export format A or B')
  evidence "control ($CONTROL_REF): event 1 appended at $e1"
  wait_until "$TURN_POLLS" host_log_since "$lab" "$e1" '	pass-through	attended	main-only	' >/dev/null \
    || fail "control: event 1 was not a main-only pass-through, so the control proves nothing"$'\n'"$(diagnose "$lab")"
  evidence "control: host log: $(host_log_since "$lab" "$e1" '	pass-through	' | head -n 1 | cut -f1-4)"
  sleep "$CONTROL_QUIET_SECONDS"
  if [ -n "$(rewakes_since "$lab" "$e1")" ] || acked_since "$lab" "$e1"; then
    fail "control: the idle primary WAS woken on $CONTROL_REF, so this scenario cannot catch the dropped hand-back"$'\n'"$(diagnose "$lab")"
  fi
  evidence "control: after ${CONTROL_QUIET_SECONDS}s no rewake and no primary command; ledger: $(ledger "$lab"); marker: $(marker "$lab"); queued rows: $(wc -l < "$lab/fm/state/.wake-queue" | tr -d ' ')"
  stop_lab "$lab"
  pass "attended live control ($CLAUDE_VERSION): on $CONTROL_REF the idle primary is not woken, so the scenario catches the bug"
}

if [ -n "$CONTROL_REF" ]; then
  run_control
else
  printf 'skip: control: set FM_SUPERVISION_HOST_ATTENDED_LIVE_CONTROL_REF to a pre-fix ref to run the negative control\n'
fi
