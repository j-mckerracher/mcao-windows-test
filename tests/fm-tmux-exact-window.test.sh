#!/usr/bin/env bash
# tests/fm-tmux-exact-window.test.sh - regression for DPES-831: the shared tmux
# lifecycle probes must match a recorded session and window EXACTLY.
#
# A bare tmux `session:window` target prefix-matches when no exact name exists,
# so task fm-foo was read as the live neighbour fm-foo-r2, and a bare session
# prefix (fmli) was read as the live session fmlive. Runs REAL tmux on a private
# socket (`-L`) behind a `tmux` shim on PATH, so the host's sessions are never
# touched, and needs no harness or credentials.
set -u

# shellcheck source=tests/lib.sh
. "$(dirname "${BASH_SOURCE[0]}")/lib.sh"

fail() { printf 'not ok - %s\n' "$1" >&2; cleanup_all; exit 1; }
pass() { printf 'ok - %s\n' "$1"; }

command -v tmux >/dev/null 2>&1 || { echo "skip: tmux not found"; exit 0; }
unset TMUX
REAL_TMUX=$(command -v tmux)
SOCKET="fm-exact-window-$$"
LAB=$(mktemp -d "${TMPDIR:-/tmp}/fm-exact-window.XXXXXX")

cleanup_all() {
  "$REAL_TMUX" -L "$SOCKET" kill-server >/dev/null 2>&1 || true
  [ -n "${LAB:-}" ] && rm -rf "$LAB"
  fm_test_cleanup
}
trap cleanup_all EXIT

# A `tmux` shim so bin/backends/tmux.sh's bare `tmux` calls reach the private
# socket and never the host's real server.
mkdir -p "$LAB/shim" "$LAB/wt"
cat > "$LAB/shim/tmux" <<SH
#!/usr/bin/env bash
exec "$REAL_TMUX" -L "$SOCKET" "\$@"
SH
chmod +x "$LAB/shim/tmux"
PATH="$LAB/shim:$PATH"
export PATH

# shellcheck source=/dev/null
. "$ROOT/bin/fm-backend.sh"
fm_backend_source tmux || fail "fm_backend_source tmux failed"

# One live session holding ONLY the neighbour window. The task window
# fm-foo is deliberately absent: it is the recorded task whose window is gone.
# Each window runs its shell directly, so no case depends on prompt readiness.
"$REAL_TMUX" -L "$SOCKET" new-session -d -s fmlive -n fm-foo-r2 -c "$LAB/wt" -- /bin/sh \
  || fail "could not start private tmux server"
"$REAL_TMUX" -L "$SOCKET" set-window-option -g automatic-rename off >/dev/null 2>&1 || true

live_windows() {  # <session> -> the exact live window names, one per line
  "$REAL_TMUX" -L "$SOCKET" list-windows -t "=$1" -F '#{window_name}' 2>/dev/null
}

[ "$(live_windows fmlive)" = "fm-foo-r2" ] \
  || fail "setup: expected only fm-foo-r2 live in fmlive, got [$(live_windows fmlive | tr '\n' ' ')]"

# --- existence: a neighbour's window must not answer for the task ----------
if fm_backend_target_exists tmux "fmlive:fm-foo"; then
  fail "a task window absent from its session must not exist because neighbour fm-foo-r2 is live"
fi
pass "tmux probe: task fm-foo is absent when only neighbour fm-foo-r2 is live"

fm_backend_target_exists tmux "fmlive:fm-foo-r2" \
  || fail "the exact live window fm-foo-r2 must exist"
pass "tmux probe: the exact live window still exists"

# --- existence: a bare session prefix must not answer for the session ------
if fm_backend_target_exists tmux "fmli:fm-foo-r2"; then
  fail "session fmli is not live; a prefix of live session fmlive must not answer for it"
fi
pass "tmux probe: a bare session prefix does not match a live session"

# --- agent state: the recorded window identity, not a neighbour's ----------
got=$(fm_backend_tmux_agent_state "fmlive:fm-foo")
[ "$got" = missing ] \
  || fail "agent state of absent fm-foo must be missing while neighbour fm-foo-r2 is live, got '$got'"
pass "tmux agent state: absent fm-foo reads missing beside neighbour fm-foo-r2"

got=$(fm_backend_tmux_agent_state "fmli:fm-foo-r2")
[ "$got" = missing ] \
  || fail "agent state under absent session fmli must be missing even though live fmlive has fm-foo-r2, got '$got'"
pass "tmux agent state: a bare session prefix reads missing"

# --- kill: removing an absent task must leave its neighbour alive ----------
fm_backend_tmux_kill "fmlive:fm-foo" || true
[ "$(live_windows fmlive)" = "fm-foo-r2" ] \
  || fail "killing absent task fm-foo must not touch neighbour fm-foo-r2"
pass "tmux kill: absent task fm-foo leaves neighbour fm-foo-r2 running"

cleanup_all
trap - EXIT
