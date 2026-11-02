# Legacy tmux adapter fixture: the pre-exact-selector window close.
#
# Appended to the current bin/backends/tmux.sh by
# tests/fm-backend.test.sh test_teardown_conformance_old_vs_new, so this
# definition replaces fm_backend_tmux_kill and every other adapter function
# stays current. It reproduces the historical permissive target, handed to tmux
# as given with no `=` exactness markers, so the case can prove teardown still
# reaches tmux window cleanup alongside an adapter that has not adopted exact
# selectors. Checked in rather than resolved from git history because history
# may not carry the old adapter (a squashed or freshly imported repository).
fm_backend_tmux_kill() {  # <target>
  tmux kill-window -t "$1" 2>/dev/null
}
