#!/usr/bin/env bash
# shellcheck disable=SC1091,SC2016,SC2088
# Behavior tests for the watcher-arm PreToolUse seatbelt (docs/arm-pretool-check.md).
#
# bin/fm-arm-command-policy.mjs is the single owner of command classification.
# This suite drives the stable shell transport through all five harness entry
# forms and asserts the per-harness wiring contract without spawning a harness.
# Empirical harness evidence lives in docs/arm-pretool-check.md.
set -u

# shellcheck source=tests/lib.sh
. "$(dirname "${BASH_SOURCE[0]}")/lib.sh"

CHECK="$ROOT/bin/fm-arm-pretool-check.sh"
POLICY="$ROOT/bin/fm-arm-command-policy.mjs"

# --- full cross-harness acceptance matrix ----------------------------------

MATRIX_IDS=()
MATRIX_EXPECTED=()
MATRIX_COMMANDS=()

matrix_case() {
  MATRIX_IDS+=("$1")
  MATRIX_EXPECTED+=("$2")
  MATRIX_COMMANDS+=("$3")
}

matrix_case A01 allow 'bin/fm-watch-arm.sh'
matrix_case A02 allow './bin/fm-watch-arm.sh --restart'
matrix_case A03 allow 'exec bin/fm-watch-arm.sh'
matrix_case A04 allow 'bin/fm-watch-checkpoint.sh --seconds 180'
matrix_case A05 allow 'exec bin/fm-watch-checkpoint.sh --seconds 180'
matrix_case A06 allow "$ROOT/bin/fm-watch-checkpoint.sh --seconds 180"
matrix_case A07 allow "cd '$ROOT'; exec bin/fm-watch-arm.sh"
# DPES-963 review: a setup cd whose target is not the code root cannot vouch for a relative
# protected path, so this relative cd target now denies as watcher-bundled.
matrix_case A08 deny "cd '../firstmate'; bin/fm-watch-checkpoint.sh --seconds 180"
matrix_case A09 allow "export FM_HOME='$ROOT'; bin/fm-watch-checkpoint.sh --seconds 180"
matrix_case A15 allow "cd $ROOT && exec bin/fm-watch-arm.sh"
matrix_case A16 allow "export FM_HOME=$ROOT && bin/fm-watch-checkpoint.sh --seconds 180"
matrix_case A17 allow 'exec fm-watch-arm.sh'
matrix_case A18 allow "cd '$ROOT' && fm-watch-checkpoint.sh --seconds 180"
matrix_case A19 allow 'exec FM-WATCH-ARM.SH'

matrix_case R01 allow "pgrep -fl '/bin/fm-watch.sh' || true"
matrix_case R02 allow "ps aux | rg '/bin/fm-watch.sh'"
matrix_case R03 allow "rg -n 'fm-watch-arm.sh &' docs tests"
matrix_case R04 allow "rg -n 'bin/fm-watch-arm.sh; echo bad' docs"
matrix_case R05 allow "git grep 'fm-watch-checkpoint.sh && echo bad'"
matrix_case R06 allow "sed -n '/fm-watch-checkpoint.sh/p' docs/arm-pretool-check.md"
matrix_case R07 allow 'assert_contains "$content" '\''fm-watch-arm.sh &'\'''
matrix_case R08 allow "printf '%s\\n' 'bin/fm-watch-checkpoint.sh --seconds 180 >/tmp/out'"
# DPES-1001: a protected name carried into tmux send-keys or inline interpreter code is watcher-nested (findings 7, 9).
matrix_case R09 deny "tmux send-keys -t isolated-pi-lab 'bin/fm-watch-arm.sh &' Enter"
matrix_case R10 deny "tmux send-keys -t isolated-pi-lab \"printf '%s\\n' 'bin/fm-watch-arm.sh &'\"; tmux send-keys -t isolated-pi-lab Enter"
matrix_case R11 deny "python3 -c 'print(\"bin/fm-watch-arm.sh; echo data\")'"
matrix_case R12 allow "bash -lc \"rg -n 'fm-watch-arm.sh &' docs\""
matrix_case R13 allow "echo 'pkill -f fm-watch'"
matrix_case R14 allow "rg -n 'pkill -f fm-watch' docs tests"
matrix_case R15 allow "echo ok # bin/fm-watch-arm.sh &"
matrix_case R16 allow $'# bin/fm-watch-arm.sh &\necho ok'
matrix_case R17 allow "printf '%s\\n' 'fm-watch.sh; a && b || c > out' | sed -n '1p'"
matrix_case R18 deny "sh -c 'tmux send-keys -t lab \"bin/fm-watch-arm.sh &\" Enter'"
matrix_case R19 allow "eval 'printf \"%s\\n\" \"bin/fm-watch-arm.sh &\"'"
matrix_case R20 allow "bash -s sentinel <<< 'echo fm-watch.sh'"
matrix_case R21 allow "bash -- -s sentinel <<< 'bin/fm-watch.sh'"

matrix_case D01 deny 'bin/fm-watch-arm.sh &'
matrix_case D02 deny 'nohup bin/fm-watch-arm.sh'
matrix_case D03 deny 'bin/fm-watch-arm.sh & disown'
matrix_case D04 deny '(bin/fm-watch-arm.sh) &'
matrix_case D05 deny "bash -lc 'bin/fm-watch-arm.sh &'"
matrix_case D06 deny '$(bin/fm-watch-arm.sh)'
matrix_case D07 deny 'echo "$(bin/fm-watch-checkpoint.sh --seconds 180)"'
matrix_case D08 deny 'cat <(bin/fm-watch-arm.sh)'
matrix_case D09 deny 'bin/fm-watch-arm.sh >/tmp/out'
matrix_case D10 deny 'bin/fm-watch-checkpoint.sh --seconds 180 </dev/null'
matrix_case D11 deny 'bin/fm-watch-arm.sh 2>&1 | head -2'
matrix_case D12 deny 'bin/fm-watch-arm.sh | cat'
matrix_case D13 deny 'bin/fm-watch-checkpoint.sh --seconds 180 | timeout 1 cat'
matrix_case D14 deny 'echo before; bin/fm-watch-arm.sh'
matrix_case D15 deny 'bin/fm-watch-checkpoint.sh --seconds 180; echo after'

# Command wrappers outside the option tables (DPES-966): the protected name stays denied,
# and an ordinary operand under the same wrapper stays allowed.
matrix_case W01 deny 'nice bin/fm-watch-arm.sh'
matrix_case W02 deny 'env --unset bin/fm-watch-arm.sh true'
matrix_case W03 deny 'echo x | xargs fm-watch-arm.sh'
matrix_case W04 deny 'env -P /tmp bin/fm-watch-arm.sh'
matrix_case W05 allow 'nice echo ok'
matrix_case W06 allow 'echo bin/fm-watch-arm.sh'
# DPES-966 review (PR 333 attempt 1): wrapper-classification gaps. Each protected form denies
# through all five entry forms; each benign form allows.
matrix_case V01 deny "bash -o pipefail -c 'bin/fm-watch-arm.sh &'"
matrix_case V02 deny "bash --rcfile /dev/null -c 'bin/fm-watch-checkpoint.sh --seconds 180'"
matrix_case V03 deny "env -i bash -o errexit -c 'bin/fm-watch-arm.sh &'"
matrix_case V04 deny "find . -exec bin/fm-watch-arm.sh ';'"
matrix_case V05 deny "git -c alias.x='!bin/fm-watch-arm.sh' x"
matrix_case V06 deny "tmux new-window -d 'bin/fm-watch-arm.sh'"
matrix_case V07 deny 'flock /tmp/lk bin/fm-watch-arm.sh'
matrix_case V08 deny "script -qc 'bin/fm-watch-arm.sh' /dev/null"
matrix_case V09 deny 'env -P /tmp bin/fm-watch-arm.sh'
matrix_case V10 deny 'lsof -t bin/fm-watch.sh | xargs kill'
matrix_case V11 deny 'xargs -I {} bin/fm-watch-arm.sh'
matrix_case V12 deny 'watch -n 2 bin/fm-watch-arm.sh'
matrix_case V13 deny 'env --chdir /tmp bin/fm-watch-arm.sh'
matrix_case V20 allow 'nice -n 19 rg fm-watch docs'
matrix_case V21 allow "watch -n 2 'pgrep -fl fm-watch'"
matrix_case V22 allow 'git ls-files | xargs grep -l fm-watch'
matrix_case V23 allow 'stdbuf -oL tail -f state/fm-watch.log'
matrix_case V24 allow "bash -o pipefail -c 'echo ok'"
matrix_case V25 allow 'env -i A=1 -u B ls'
matrix_case V26 allow 'lsof -t docs/readme.md | xargs cat'
matrix_case D16 deny 'true && bin/fm-watch-arm.sh'
matrix_case D17 deny 'bin/fm-watch-checkpoint.sh --seconds 180 || true'
matrix_case D18 deny $'bin/fm-watch-arm.sh\nbin/fm-watch-checkpoint.sh --seconds 180'
matrix_case D19 deny "pkill -f '/bin/fm-watch.sh'"
matrix_case D20 deny "command pkill -f '/bin/fm-watch.sh'"
matrix_case D21 deny "/usr/bin/pkill -f '/bin/fm-watch.sh'"
matrix_case D22 deny "sudo pkill -f '/bin/fm-watch.sh'"
matrix_case D23 deny 'kill "$(pgrep -f '\''/bin/fm-watch.sh'\'')"'
matrix_case D24 deny $'bin/fm-watc\\\nh-arm.sh &'
matrix_case D25 deny 'sudo -u root bin/fm-watch-arm.sh &'
matrix_case D26 deny 'env -u PATH bin/fm-watch-arm.sh &'
matrix_case D27 deny "bash -c \$'bin/fm-watch-arm.sh &'"
matrix_case D28 deny $'bash <<\'EOF\'\nbin/fm-watch-arm.sh &\nEOF'
matrix_case D29 deny "WATCHER='bin/fm-watch-arm.sh &' bash -c 'eval \"\$WATCHER\"'"
matrix_case D30 deny "p=\$(pgrep -f '/bin/fm-watch.sh'); kill \"\$p\""
matrix_case D31 deny "env -S 'bin/fm-watch-arm.sh &'"
matrix_case D32 deny "env --split-string='$ROOT/bin/fm-watch-arm.sh &'"
matrix_case D33 deny 'bin/fm-"watch-arm.sh" &'
matrix_case D34 deny "WATCHER='bin/fm-watch-arm.sh'; \"\$WATCHER\" &"
matrix_case D35 deny "bash -c -- 'bin/fm-watch-arm.sh &'"
matrix_case D36 deny 'bash bin/fm-watch-arm.sh &'
matrix_case D37 deny '. bin/fm-watch-arm.sh &'
matrix_case D38 deny "bash <<< 'bin/fm-watch-arm.sh &'"
matrix_case D39 deny "eval 'true;' 'bin/fm-watch-arm.sh &'"
matrix_case D40 deny 'timeout 30 bin/fm-watch-arm.sh &'
matrix_case D41 deny 'gtimeout 30 bin/fm-watch-arm.sh &'
matrix_case D42 deny 'bin/fm-watch-{arm,checkpoint}.sh &'
matrix_case D43 deny 'bin/fm-watch-arm.sh* &'
matrix_case D44 deny "pattern='fm-watch'; pkill -f \"\$pattern\""
matrix_case D45 deny "p=\$(pgrep -f '/bin/fm-watch.sh'); q=\$p; kill \$q"
matrix_case D46 deny '$FM_HOME/bin/fm-watch-arm.sh &'
matrix_case D47 deny '$HOME/firstmate/bin/fm-watch-arm.sh | cat'
matrix_case D48 deny '~/firstmate/bin/fm-watch-arm.sh &'
matrix_case D49 deny 'bin/fm-watch.sh'
matrix_case D50 deny '$FM_HOME/bin/fm-watch.sh'
matrix_case D51 deny '~/firstmate/bin/fm-watch.sh --restart'
matrix_case D52 deny "bin/fm-\$'\x77'atch-arm.sh &"
matrix_case D53 deny 'bin/fm-$"watch"-arm.sh &'
matrix_case D54 deny 'bin/fm-watch-$"arm".sh &'
matrix_case D55 deny 'while true; do pkill -f fm-watch; done'
matrix_case D56 deny 'for x in 1; do pkill -f fm-watch; done'
matrix_case D57 deny 'case x in x) pkill -f fm-watch ;; esac'
matrix_case D58 deny 'until false; do kill $(pgrep -f fm-watch); done'
matrix_case D59 deny $'bash -s sentinel <<\'EOF\'\nbin/fm-watch.sh\nEOF'
matrix_case D60 deny "bash -s sentinel <<< 'bin/fm-watch.sh'"
matrix_case D61 deny "bash -s -- one two <<< 'bin/fm-watch-arm.sh &'"
matrix_case D62 deny "bash -xs sentinel <<< 'bin/fm-watch.sh'"
matrix_case D63 deny "sh -s sentinel <<< 'bin/fm-watch.sh'"
matrix_case D64 deny 'bash -s bin/fm-watch.sh'
matrix_case D65 deny "bash -s -- -c harmless <<< 'bin/fm-watch.sh'"
matrix_case D66 deny 'fm-watch-arm.sh &'
matrix_case D67 deny 'fm-watch-arm.sh | cat'
matrix_case D68 deny 'PATH=/tmp/fake/bin:$PATH fm-watch-arm.sh | cat'
matrix_case D69 deny 'PATH=/tmp/fake/bin:$PATH fm-watch-checkpoint.sh --seconds 180'
matrix_case D70 deny 'export PATH=/tmp/fake/bin:$PATH; fm-watch-arm.sh | cat'
matrix_case D71 deny 'nohup fm-watch-arm.sh'
matrix_case D72 deny 'fm-watch-arm.sh > /tmp/out'
matrix_case D73 deny 'fm-watch.sh'
matrix_case D74 deny './fm-watch.sh | cat'
matrix_case D75 deny 'bash fm-watch-arm.sh'
matrix_case D76 deny 'fm-watch-arm.sh && echo done'
matrix_case D77 deny '(fm-watch-arm.sh) &'
matrix_case D78 deny 'PATH=/tmp/fake/bin:$PATH fm-watch-arm.sh &'
matrix_case D79 deny 'exec fm-watch-checkpoint.sh --seconds 180 | cat'
matrix_case D80 deny 'export PATH=/tmp/fake/bin:$PATH; ${X:-fm-watch-arm.sh} | cat'
matrix_case D81 deny 'export PATH=/tmp/fake/bin:$PATH; "${X:-fm-watch-arm.sh}" | cat'
matrix_case D82 deny 'export PATH=/tmp/fake/bin:$PATH; ${X:-fm-watch-arm.sh} &'
matrix_case D83 deny 'export PATH=/tmp/fake/bin:$PATH; ${X:-fm-watch.sh}'
matrix_case D84 deny 'export PATH=/tmp/fake/bin:$PATH; ${X:-fm-watch-checkpoint.sh} --seconds 180 | cat'
matrix_case D85 deny 'export PATH=/tmp/fake/bin:$PATH; ${X:+fm-watch-arm.sh} | cat'
matrix_case D86 deny 'bin/${X:-fm-watch-arm.sh} | cat'
matrix_case D87 deny '$FM_HOME/bin/${X:-fm-watch-arm.sh} | cat'
matrix_case D88 deny 'export PATH=/tmp/fake/bin:$PATH; ${X:-fm}-watch-arm.sh | cat'
matrix_case D89 deny '$X-watch-arm.sh | cat'
matrix_case D90 deny 'FM-WATCH-ARM.SH | cat'
matrix_case D91 deny 'Fm-Watch-Arm.sh | cat'
matrix_case D92 deny 'export PATH=/tmp/fake/bin:$PATH; FM-WATCH-ARM.SH | cat'
matrix_case D93 deny 'bin/FM-WATCH-ARM.SH | cat'
matrix_case D94 deny 'FM-WATCH-ARM.SH | cat "'
matrix_case D95 deny 'FM-WATCH-ARM.SH &'
matrix_case D96 deny 'FM-WATCH.SH'

matrix_case E01 allow "bin/fm-watch-checkpoint.sh --seconds '180;still-one-arg'"
matrix_case E02 allow "bin/fm-watch-checkpoint.sh --label 'fm-watch-arm.sh; literal argument'"
matrix_case E03 allow 'bin/fm-watch-arm.sh # output > file &'
matrix_case E05 deny "FM_HOME=$ROOT bin/fm-watch-checkpoint.sh --seconds 180"
matrix_case E06 deny "env FM_HOME=$ROOT bin/fm-watch-arm.sh"
matrix_case E08 deny "bash -lc 'bin/fm-watch-checkpoint.sh --seconds 180'"
matrix_case E09 deny '(bin/fm-watch-checkpoint.sh --seconds 180)'
matrix_case E10 deny "eval 'bin/fm-watch-arm.sh &'"
matrix_case E11 deny "exec bash -lc 'bin/fm-watch-arm.sh &'"
matrix_case E12 allow 'bash -lc "$WATCHER_COMMAND" # fm-watch-arm.sh'
matrix_case E13 allow "printf '%s\\n' 'argument has ; and fm-watch-arm.sh and &&'"
matrix_case E14 allow '$FM_HOME/bin/fm-teardown.sh &'
matrix_case E15 allow '$FM_HOME/bin/fm-watch-arm.sh'
matrix_case E16 allow '~/firstmate/bin/fm-watch-checkpoint.sh --seconds 180'
matrix_case E17 allow 'for f in 1; do echo fm-watch; done'
matrix_case E18 allow 'cat fm-watch-arm.sh'
matrix_case E19 allow 'echo fm-watch.sh | cat'
matrix_case E20 allow 'grep -n fm-watch-checkpoint.sh docs/arm-pretool-check.md'
matrix_case E21 allow 'echo ${X:-fm-watch-arm.sh} | cat'
matrix_case E22 allow 'ls "$HOME/fm-watch-notes" | cat'
# DPES-963: variable-split, glob, brace, command-substitution, xargs, PATH-export,
# case-insensitive broad-kill and bare-name shapes. Each denied shape is also a
# sandbox-stub control below: the same text reaches the stub when unguarded.
matrix_case F01 deny 'X=watch-arm.sh; fm-$X | cat'
matrix_case F02 deny 'X=wat; Y=ch-arm.sh; fm-$X$Y | cat'
matrix_case F03 deny 'X=fm-watch-arm.sh; $X | cat'
matrix_case F04 deny 'X=watch-arm.sh; fm-$X'
matrix_case F08 deny 'X=$(echo fm-watch-arm.sh); $X | cat'
matrix_case F09 deny 'X=$(echo fm-watch-arm.sh); $X'
matrix_case F10 deny 'X=$(echo wat); fm-${X}ch-arm.sh | cat'
matrix_case F11 allow 'X=$(date); echo $X'
matrix_case F12 allow 'X=foo; fm-$X | cat'
matrix_case F05 allow 'echo $HOME'
matrix_case F06 allow 'printf "%s\n" "fm-$X"'
matrix_case F07 allow 'cat "$EDITOR"'
matrix_case G01 deny 'bin/*-arm.sh | cat'
matrix_case G02 deny 'bin/fm-w*h-arm.sh &'
matrix_case G03 deny 'cd bin && ./fm-w*h-arm.sh | cat'
matrix_case G04 deny 'bin/*-arm.sh'
matrix_case G05 allow 'ls bin/*.md'
matrix_case G06 allow 'printf "%s\n" bin/*-arm.sh'
matrix_case B01 deny 'echo x | xargs fm-watch-arm.sh'
matrix_case B02 deny 'echo x | xargs -I{} fm-watch-arm.sh {}'
matrix_case B03 deny 'xargs bin/fm-watch-arm.sh < /dev/null'
matrix_case B04 allow 'echo x | xargs wc -l'
matrix_case S01 deny 'bin/$(echo fm-watch-arm.sh) | cat'
matrix_case S02 deny '$(echo bin/fm-watch-arm.sh)'
matrix_case S03 deny 'bin/`echo fm-watch-arm.sh` | cat'
matrix_case S04 deny 'bin/{fm-watch-arm.sh,x} | cat'
matrix_case S05 allow 'echo {a,b}'
matrix_case P01 deny 'export PATH=/tmp/x:$PATH; fm-watch-arm.sh'
matrix_case K01 deny 'pkill -if FM-WATCH'
matrix_case K02 deny 'pkill -fi FM-WATCH'
matrix_case K03 deny 'pgrep -if FM-WATCH | xargs kill'
matrix_case K04 deny "pkill -f 'fm-w.*tch'"
matrix_case K05 allow 'pkill -f node'
matrix_case W01 deny 'fm-watch > /tmp/out'
matrix_case W02 deny 'fm-watch'
# DPES-963 review findings: variables set by non-prefix builtins and constructs (F1), kill-family
# shapes (F2), setup cd and loader or exec variables (F3). Each deny is a closed bypass.
matrix_case H01 deny 'read X <<< fm-watch.sh; bin/$X &'
matrix_case H02 deny 'printf -v X fm-watch.sh; bin/$X &'
matrix_case H03 deny 'P=FM-WAT; Q=ch; pkill -if "$P$Q"'
matrix_case H04 deny 'P=fm-wat; Q=ch-arm.sh; pkill -f "$P$Q"'
matrix_case H05 deny 'killall fm-watch.sh'
matrix_case H06 deny 'ps aux | grep fm-watch.sh | xargs kill'
matrix_case H07 deny 'kill -9 $(ps -ef | grep fm-watch | grep -v grep)'
matrix_case H08 deny "pkill -f 'fm-wat\ch'"
matrix_case H09 deny 'export PERL5OPT=-I/tmp/x; bin/fm-watch-arm.sh'
matrix_case H10 deny 'export NODE_OPTIONS=--require=/tmp/x.js; bin/fm-watch-arm.sh'
matrix_case H11 deny 'export GIT_EXEC_PATH=/tmp/x; bin/fm-watch-arm.sh'
matrix_case H12 deny 'export PYTHONSTARTUP=/tmp/x.py; bin/fm-watch-arm.sh'
matrix_case H13 deny 'export HOME=/tmp/x; ~/firstmate/bin/fm-watch-arm.sh'
matrix_case H14 deny 'cd /tmp/evil && bin/fm-watch-arm.sh'
matrix_case H15 deny 'for X in watch-arm.sh; do fm-$X; done'
matrix_case H16 deny "eval 'X=fm-watch-arm.sh'; bin/\$X &"
matrix_case H17 deny '${X:=fm-watch-arm.sh}; bin/$X &'
matrix_case H18 deny 'read X; $X'
matrix_case H19 deny '{ X=fm-watch-arm.sh; }; bin/$X &'
matrix_case H20 deny 'declare X; X=fm-watch-arm.sh; bin/$X &'
matrix_case H21 allow 'for f in *.md; do cat $f; done'
matrix_case H22 allow 'echo "read x"'
matrix_case H23 allow 'ps aux | grep node | xargs kill'
matrix_case H24 allow "pkill -f 'node\.js'"
matrix_case H25 allow 'pkill -a node'
matrix_case H26 allow "cd '$ROOT' && bin/fm-watch-arm.sh"
matrix_case H27 allow 'export FM_HOME=/tmp/x; bin/fm-watch-checkpoint.sh --seconds 180'
matrix_case H28 deny 'cd /tmp/evil; bin/fm-watch-checkpoint.sh --seconds 180'
# DPES-963 review round 2: ${...} expansions the classifier cannot resolve (B1), shell launches fed by
# a substitution, xargs, pipe, or file (B2), unlisted wrappers and negation (B3), and fuser (N1).
matrix_case H29 deny 'X=fm-wat_ch-arm.sh; bin/${X/_/} &'
matrix_case H30 deny 'X=fm-watch-arm.sh; N=X; bin/${!N} &'
matrix_case H31 deny 'mapfile -t A <<< fm-watch-arm.sh; bin/${A[0]} &'
matrix_case H32 deny "printf 'bin/fm-watch.sh' | xargs sh -c"
matrix_case H33 deny 'sh -c "$(printf bin/fm-watch.sh)"'
matrix_case H34 deny "bash <(printf 'bin/fm-watch.sh')"
matrix_case H35 deny 'printf "%s" "bin/fm-w" "atch-arm.sh" | sh'
matrix_case H36 deny "printf 'bin/fm-watch-arm.sh\n' | bash"
matrix_case H37 deny 'printf "%s" "bin/fm-wat" | sh'
matrix_case H38 deny 'bash < x.sh'
matrix_case H39 deny '! bin/fm-watch.sh'
matrix_case H40 deny 'nice -n 5 bin/fm-watch.sh'
matrix_case H41 deny 'builtin exec bin/fm-watch.sh'
matrix_case H42 deny 'stdbuf -o0 bin/fm-watch.sh'
matrix_case H43 deny 'caffeinate -i bin/fm-watch.sh'
matrix_case H44 deny 'nice -n 5 bin/fm-watch-arm.sh'
matrix_case H45 deny '! bin/fm-watch-arm.sh'
matrix_case H46 deny 'nice -x bin/fm-watch.sh'
matrix_case H47 deny 'fuser -k bin/fm-watch.sh'
matrix_case H48 allow 'nice -n 5 ls bin'
matrix_case H49 allow '! grep -q x docs/arm-pretool-check.md'
matrix_case H50 allow 'ps aux | grep node | xargs kill'

# DPES-966 attempt 3: shells outside sh/bash/zsh, executors, git/sed operands, and xargs replacement commands.
matrix_case X01 deny "dash -c 'bin/fm-watch-arm.sh'"
matrix_case X02 deny "ksh -c 'bin/fm-watch-arm.sh'"
matrix_case X03 deny "mksh -c 'bin/fm-watch-arm.sh'"
matrix_case X04 deny "fish -c 'bin/fm-watch-arm.sh'"
matrix_case X05 deny "csh -c 'bin/fm-watch-arm.sh'"
matrix_case X06 deny "busybox sh -c 'bin/fm-watch-arm.sh'"
matrix_case X07 deny "git rebase --exec 'bin/fm-watch-arm.sh' HEAD~1"
matrix_case X08 deny "git rebase -x 'bin/fm-watch-arm.sh' main"
matrix_case X09 deny "git submodule foreach 'bin/fm-watch-arm.sh'"
matrix_case X10 deny "git bisect run bin/fm-watch-arm.sh"
matrix_case X11 deny "sed '1e bin/fm-watch-arm.sh' file"
matrix_case X12 deny "systemd-run --user bin/fm-watch-arm.sh"
matrix_case X13 deny "unshare -r bin/fm-watch-arm.sh"
matrix_case X14 deny "npm exec -- bin/fm-watch-arm.sh"
matrix_case X15 deny "printf 'bin/fm-watch-arm.sh' | xargs -I{} {}"
matrix_case X16 deny "echo bin/fm-watch-arm.sh | xargs -I{} {}"
matrix_case X17 deny "find . -name fm-watch-arm.sh | xargs -I{} {}"
matrix_case X18 deny "echo bin/fm-watch-arm.sh | xargs -I{} sh -c {}"
matrix_case X19 deny "numactl --cpunodebind=0 bin/fm-watch-arm.sh"
matrix_case X20 deny "valgrind bin/fm-watch-arm.sh"
matrix_case X21 deny "perf record bin/fm-watch-arm.sh"
matrix_case X22 deny "prlimit --nofile=10 bin/fm-watch-arm.sh"
matrix_case X23 deny "cgexec -g memory:x bin/fm-watch-arm.sh"
matrix_case X24 deny "launchctl submit -l x -- bin/fm-watch-arm.sh"
matrix_case X25 allow "printf x | xargs -I{} {}"
matrix_case X26 allow "git rebase main"
matrix_case X27 allow "sed -n 1,3p docs/arm-pretool-check.md"
matrix_case X28 allow "npm test"
matrix_case X29 allow "dash -c 'echo ok'"
# DPES-1001 (PR 333 third review): remaining watcher-wrapper bypasses. Each probe denies, one row per finding.
matrix_case Y01 deny "yash -c 'bin/fm-watch-arm.sh'"                       # finding 1
matrix_case Y02 deny "posh -c 'bin/fm-watch-arm.sh'"
matrix_case Y03 deny "ksh93 -c 'bin/fm-watch-arm.sh'"
matrix_case Y04 deny "\$SHELL -c 'bin/fm-watch-arm.sh'"                     # finding 2
matrix_case Y05 deny "sh -c \"\$'bin/fm-watch-arm.sh'\""                    # finding 3
matrix_case Y06 deny "bash -c \"\$'bin/fm-watch-arm.sh'\""
matrix_case Y07 deny "git rebase --exe 'bin/fm-watch-arm.sh' main"          # finding 4
matrix_case Y08 deny "sed 's/x/y/;e bin/fm-watch-arm.sh' f"                 # finding 5
matrix_case Y09 deny "sed -e 's/x/y/' -e 'e bin/fm-watch-arm.sh' f"
matrix_case Y10 deny "sed 'y/a/b/;e bin/fm-watch-arm.sh' f"
matrix_case Y11 deny "fd --exec sh -c 'bin/fm-watch-arm.sh'"                # finding 6
matrix_case Y12 deny "fdfind --exec-batch bin/fm-watch-arm.sh ."
matrix_case Y13 deny "tmux send-keys -t x 'bin/fm-watch-arm.sh' Enter"      # finding 7
matrix_case Y14 deny "tmux -L x send-keys -t x 'bin/fm-watch-arm.sh' Enter"
matrix_case Y15 deny "tmux send -t x 'bin/fm-watch-arm.sh' Enter"
matrix_case Y16 deny "pnpm exec bin/fm-watch-arm.sh"                        # finding 8
matrix_case Y17 deny "yarn exec bin/fm-watch-arm.sh"
matrix_case Y18 deny "bunx bin/fm-watch-arm.sh"
matrix_case Y19 deny "pnpx bin/fm-watch-arm.sh"
matrix_case Y20 deny "python3 -c 'import os; os.system(\"bin/fm-watch-arm.sh\")'"   # finding 9
matrix_case Y21 deny "perl -e 'system(\"bin/fm-watch-arm.sh\")'"
matrix_case Y22 deny "awk 'BEGIN{system(\"bin/fm-watch-arm.sh\")}'"
matrix_case Y23 deny "node -e 'require(\"child_process\").execSync(\"bin/fm-watch-arm.sh\")'"
matrix_case Y24 deny "ruby -e 'system(\"bin/fm-watch-arm.sh\")'"
matrix_case Y25 deny "tclsh <<< 'exec bin/fm-watch-arm.sh'"
matrix_case Y26 deny "source <(printf bin/fm-watch-arm.sh)"                 # finding 10
matrix_case Y27 deny ". <(printf bin/fm-watch-arm.sh)"
matrix_case Y28 deny "xargs -I{} {} < names.txt"                            # finding D
matrix_case Y29 deny "cat names.txt | xargs -I{} {}"
matrix_case Y30 allow "bash -c \"echo \$'ok'\""
# DPES-1001 attempt 1 FAIL (PR 369): findings 1-9. Each deny row is a bypass that was allowed before this fix.
# Finding 1: options after -c, including -- and -s.
matrix_case Z01 deny "sh -c -e 'bin/fm-watch.sh'"
matrix_case Z02 deny "sh -c -s 'bin/fm-watch.sh'"
matrix_case Z03 deny "sh -c -ec -- 'bin/fm-watch.sh'"
matrix_case Z04 deny "bash -c -l 'bin/fm-watch-arm.sh &'"
matrix_case Z05 allow "sh -c -e 'echo hi'"
# Finding 2: operands run through $0, $@, $N, eval, or exec.
matrix_case Z06 deny "sh -c '\"\$@\"' sh bin/fm-watch.sh"
matrix_case Z07 deny "sh -c 'exec \"\$0\"' bin/fm-watch-arm.sh"
matrix_case Z08 deny "sh -c 'eval \"\$1\"' _ bin/fm-watch.sh"
matrix_case Z09 allow "sh -c 'echo \"\$0\"' ok"
# Finding 3: trap actions run as shell text.
matrix_case Z10 deny "trap 'bin/fm-watch.sh' EXIT"
matrix_case Z11 deny "trap -- 'bin/fm-watch.sh' EXIT"
matrix_case Z12 deny "trap 'sh -c \"bin/fm-watch-arm.sh\"' EXIT"
matrix_case Z13 allow "trap 'echo hi' EXIT"
# Finding 4: nested $'..' escapes are decoded by the inner shell.
matrix_case Z14 deny "sh -c \"\$'\\x62in/fm-watch.sh'\""
matrix_case Z15 deny "bash -c \"\$'\\142in/fm-watch-arm.sh'\""
# Finding 5: unknown runners and the inline-code interpreters they launch.
matrix_case Z16 deny "uv run python -c 'import os; os.system(\"bin/fm-watch.sh\")'"
matrix_case Z17 deny "poetry run python -c 'import os; os.system(\"bin/fm-watch.sh\")'"
matrix_case Z18 deny "bundle exec ruby -e 'system(\"bin/fm-watch.sh\")'"
matrix_case Z19 deny "mise exec -- bin/fm-watch.sh"
matrix_case Z20 deny "corepack pnpm exec bin/fm-watch.sh"
matrix_case Z21 deny "pypy3 -c 'import os; os.system(\"bin/fm-watch.sh\")'"
matrix_case Z22 deny "ipython -c 'import os; os.system(\"bin/fm-watch.sh\")'"
matrix_case Z23 deny "luajit -e 'os.execute(\"bin/fm-watch.sh\")'"
matrix_case Z24 deny "Rscript -e 'system(\"bin/fm-watch.sh\")'"
matrix_case Z25 deny "julia -e 'println(\"bin/fm-watch.sh\")'"
matrix_case Z26 deny "groovy -e 'println(\"bin/fm-watch.sh\")'"
matrix_case Z27 deny "guile -c '(system \"bin/fm-watch.sh\")'"
matrix_case Z28 deny "ts-node -e 'bin/fm-watch.sh'"
matrix_case Z29 deny "tsx -e 'bin/fm-watch.sh'"
matrix_case Z30 deny "coffee -e 'bin/fm-watch.sh'"
matrix_case Z31 deny "erl -eval 'os:cmd(\"bin/fm-watch.sh\")'"
matrix_case Z32 deny "elixir -e 'System.cmd(\"bin/fm-watch.sh\", [])'"
matrix_case Z33 deny "scala -e 'bin/fm-watch.sh'"
matrix_case Z34 allow "uv run python -c 'print(1)'"
# Finding 6: unlisted shells and executors.
matrix_case Z35 deny "elvish -c 'bin/fm-watch.sh'"
matrix_case Z36 deny "nu -c 'bin/fm-watch.sh'"
matrix_case Z37 deny "xonsh -c 'bin/fm-watch.sh'"
matrix_case Z38 deny "osh -c 'bin/fm-watch.sh'"
matrix_case Z39 deny "ion -c 'bin/fm-watch.sh'"
matrix_case Z40 deny "pwsh -c 'bin/fm-watch.sh'"
matrix_case Z41 deny "xterm -e 'bin/fm-watch.sh'"
matrix_case Z42 deny "direnv exec . bin/fm-watch.sh"
matrix_case Z43 deny "nix run . -- bin/fm-watch.sh"
matrix_case Z44 deny "watchexec bin/fm-watch.sh"
matrix_case Z45 deny "entr sh -c 'bin/fm-watch.sh'"
matrix_case Z46 deny "setpriv --reuid=1000 bin/fm-watch.sh"
matrix_case Z47 allow "elvish -c 'echo ok'"
# Finding 7: git filter-branch and difftool -x run their operand.
matrix_case Z48 deny "git filter-branch --tree-filter 'bin/fm-watch.sh'"
matrix_case Z49 deny "git difftool -x 'bin/fm-watch.sh'"
matrix_case Z50 allow "git filter-branch --tree-filter 'echo hi'"
# Finding 8: xargs reads its arguments from an arg file.
matrix_case Z51 deny "xargs -a names.txt -I{} {}"
matrix_case Z52 deny "xargs --arg-file=names.txt -I{} {}"
matrix_case Z53 allow "xargs -a names.txt wc -l"
# Finding 9: GNU sed e command inside an address range or a { block, and -e with a split script.
matrix_case Z54 deny "sed '/a/,/b/e bin/fm-watch.sh' f"
matrix_case Z55 deny "sed '1{e bin/fm-watch.sh' -e '}' f"
matrix_case Z56 allow "sed -n '/a/,/b/p' docs/arm-pretool-check.md"
matrix_case Z57 allow "sed -n -e '/a/,/b/p' docs/arm-pretool-check.md"
# DPES-1001 attempt 2 FAIL (PR 369): positional parameters set by set -- or shift, GNU xargs long-option
# prefixes, and the matrix rows for R -e, git filter-repo, and git difftool --extcmd.
matrix_case Z58 deny "sh -c 'set -- bin/fm-watch.sh; eval \"\$1\"'"
matrix_case Z59 deny "sh -c 'set -- bin/fm-watch.sh; eval \"\$@\"'"
matrix_case Z60 deny "sh -c 'set -- \"bin/fm-watch.sh\"; exec \"\$1\"'"
matrix_case Z61 deny "sh -c 'set -- x bin/fm-watch.sh; shift; \"\$1\"'"
matrix_case Z62 deny "bash -c 'set -- bin/fm-watch-arm.sh; eval \"\$1\" &'"
matrix_case Z63 allow "sh -c 'set -- echo hi; eval \"\$1\"'"
matrix_case Z64 deny "sh -c 'set -- bin/fm-watch.sh; \"\$*\"'"
matrix_case Z65 deny "xargs --arg-f names.txt -I{} {}"
matrix_case Z66 deny "xargs --arg-f=names.txt -I{} {}"
matrix_case Z67 deny "xargs --ar=names.txt -I{} {}"
matrix_case Z68 deny "xargs --null --arg-f names.txt -I{} {}"
matrix_case Z69 deny "R -e 'system(\"bin/fm-watch.sh\")'"
matrix_case Z70 deny "git filter-repo --tree-filter 'bin/fm-watch.sh'"
matrix_case Z71 deny "git difftool --extcmd 'bin/fm-watch.sh'"
matrix_case Z72 deny "sh -c 'set -- bin/fm-watch.sh; set -eo pipefail; eval \"\$1\"'"
matrix_case Z73 deny "sh -c 'set -- bin/fm-watch.sh; set -oe pipefail; eval \"\$1\"'"
matrix_case Z74 allow "sh -c 'set -- echo hi; set -eo pipefail; eval \"\$1\"'"
matrix_case Z75 allow "sh -c 'set -- echo hi; set -oe pipefail; eval \"\$1\"'"
matrix_case Z76 deny "bash -c 'set -- bin/fm-watch.sh; eval \"\${1:-x}\"'"
matrix_case Z77 deny "bash -c 'set -- bin/fm-watch.sh; eval \"\${1-x}\"'"
matrix_case Z78 deny "bash -c 'set -- bin/fm-watch.sh; eval \"\${1:0}\"'"
matrix_case Z79 deny "bash -c 'set -- bin/fm-watch.sh; eval \"\${@:1}\"'"
matrix_case Z80 deny "sh -c 'set -- bin/fm-watch.sh; . \"\${1:-x}\"'"
matrix_case Z81 deny "sh -c 'set -- bin/fm-watch.sh; exec \"\${1-x}\"'"
matrix_case Z82 deny "sh -c 'set -- bin/fm-watch.sh; x=\$1; eval \"\$x\"'"
matrix_case Z83 deny "sh -c 'set -- bin/fm-watch.sh; x=\$@; eval \"\$x\"'"
matrix_case Z84 deny "sh -c 'set -- bin/fm-watch.sh; x=\$*; eval \"\$x\"'"
matrix_case Z85 allow "sh -c 'set -- echo hi; x=\$1; eval \"\$x\"'"
matrix_case Z86 deny 'eval "$1"'
matrix_case Z87 deny 'exec "$1"'
matrix_case Z88 deny '. "$1"'
matrix_case Z89 deny "lua5.4 -e 'os.execute(\"bin/fm-watch.sh\")'"
matrix_case Z90 deny "perl5.36 -e 'system \"bin/fm-watch.sh\"'"
matrix_case Z91 deny "ruby3.2 -e 'system(\"bin/fm-watch.sh\")'"
matrix_case Z92 deny "node20 -e 'require(\"child_process\").exec(\"bin/fm-watch.sh\")'"
matrix_case Z93 allow "node20 -e 'console.log(1)'"
matrix_case Z94 deny "vim -c '!bin/fm-watch.sh'"
matrix_case Z95 deny "nvim -S bin/fm-watch.sh"
matrix_case Z96 allow 'vim docs/arm-pretool-check.md'
matrix_case Z97 allow "nvim -c 'set number'"
matrix_case Z98 deny "fzf --preview 'bin/fm-watch.sh'"
matrix_case Z99 deny "fzf --bind 'ctrl-x:execute(bin/fm-watch.sh)'"
matrix_case Z100 deny "fzf --bind 'ctrl-r:reload(bin/fm-watch.sh)'"
matrix_case Z101 allow "fzf --preview 'echo ready'"
matrix_case Z102 allow "fzf --bind 'ctrl-x:execute(echo ok)'"
matrix_case Z103 allow "eval \"\$(printf '%s' 'echo hi')\""
matrix_case Z104 deny "x=\$1; eval \"\$x\""
matrix_case Z105 deny "fzf --bind 'ctrl-p:change-preview(bin/fm-watch.sh)'"
matrix_case Z106 deny "fzf --bind 'ctrl-p:become(bin/fm-watch.sh)'"
matrix_case Z107 allow "fzf --preview 'echo fm-watch-arm.sh'"
# DPES-1005 review 1 (PR 399): editor family, fzf bracket delimiters, versioned runners, glob and brace positionals.
matrix_case Z108 deny "vi -c '!bin/fm-watch.sh'"
matrix_case Z109 deny "view -c '!bin/fm-watch.sh'"
matrix_case Z110 deny "ex -c '!bin/fm-watch.sh'"
matrix_case Z111 deny "gvim -c '!bin/fm-watch.sh'"
matrix_case Z112 deny "vimdiff -c '!bin/fm-watch.sh'"
matrix_case Z113 deny "vim.basic -c '!bin/fm-watch.sh'"
matrix_case Z114 deny "emacs --batch --eval '(shell-command \"bin/fm-watch.sh\")'"
matrix_case Z115 allow 'vi docs/arm-pretool-check.md'
matrix_case Z116 allow 'emacs --version'
matrix_case Z117 deny "fzf --bind 'ctrl-x:execute[bin/fm-watch.sh]'"
matrix_case Z118 deny "fzf --bind 'ctrl-x:execute~bin/fm-watch.sh~'"
matrix_case Z119 deny "fzf --bind 'ctrl-x:execute!bin/fm-watch.sh!'"
matrix_case Z120 deny "fzf --bind 'ctrl-x:execute-silent[bin/fm-watch.sh]'"
matrix_case Z121 allow "fzf --bind 'ctrl-x:execute[echo ok]'"
matrix_case Z122 deny "luajit2.1 -e 'os.execute(\"bin/fm-watch.sh\")'"
matrix_case Z123 deny "php8.2 -r 'system(\"bin/fm-watch.sh\");'"
matrix_case Z124 allow "php8.2 --version"
matrix_case Z125 deny "sh -c 'set -- bin/fm-w*.sh; eval \"\$1\"'"
matrix_case Z126 deny "sh -c 'set -- bin/fm-watch.{sh,}; eval \"\$1\"'"
matrix_case Z127 deny "sh -c 'set -- bin/fm-watch.[s]h; eval \"\$1\"'"
matrix_case Z128 deny "sh -c 'set -- bin/fm-w*.sh; x=\$1; eval \"\$x\"'"
matrix_case Z129 deny "sh -c 'x=bin/fm-w*.sh; eval \"\$x\"'"
matrix_case Z130 allow "sh -c 'set -- echo hi; eval \"\$1*\"'"
matrix_case Z131 allow 'x=echo; eval "$x hi"'
# DPES-1005 review 2 (correction check): fzf @, #, $, and paren executing actions; top-level glob, brace, and quoted-glob positionals.
matrix_case Z132 deny "fzf --bind 'ctrl-x:execute@bin/fm-watch.sh@'"
matrix_case Z133 deny "fzf --bind 'ctrl-x:execute#bin/fm-watch.sh#'"
matrix_case Z134 deny "fzf --bind 'ctrl-x:execute\$bin/fm-watch.sh\$'"
matrix_case Z135 deny "fzf --bind 'ctrl-x:execute-silent(bin/fm-watch.sh)'"
matrix_case Z136 allow "fzf --bind 'ctrl-x:execute@echo ok@'"
matrix_case Z137 allow "fzf --bind 'ctrl-x:execute-silent(echo ok)'"
matrix_case Z138 deny 'set -- bin/fm-w*.sh; eval "$1"'
matrix_case Z139 deny 'set -- bin/fm-watch.{sh,}; eval "$1"'
matrix_case Z140 deny 'set -- bin/fm-watch.[s]h; eval "$1"'
matrix_case Z141 allow 'set -- echo hi; eval "$1"'
matrix_case Z142 deny "sh -c 'set -- \"bin/fm-w*.sh\"; eval \"\$1\"'"
matrix_case Z143 deny 'x="bin/fm-w*.sh"; eval "$x"'
# DPES-1005 review 3 (attempt 2 review): fzf action names and delimiters, positional indirection, stdin-fed editors.
matrix_case Z144 deny "fzf --bind 'ctrl-x:transform-border-label(bin/fm-watch.sh)'"
matrix_case Z145 deny "fzf --bind 'ctrl-x:transform-ghost(bin/fm-watch.sh)'"
matrix_case Z146 deny "fzf --bind 'ctrl-x:transform-header-label(bin/fm-watch.sh)'"
matrix_case Z147 deny "fzf --bind 'ctrl-x:transform-header-lines(bin/fm-watch.sh)'"
matrix_case Z148 deny "fzf --bind 'ctrl-x:transform-input-label(bin/fm-watch.sh)'"
matrix_case Z149 deny "fzf --bind 'ctrl-x:transform-list-label(bin/fm-watch.sh)'"
matrix_case Z150 deny "fzf --bind 'ctrl-x:transform-nth(bin/fm-watch.sh)'"
matrix_case Z151 deny "fzf --bind 'ctrl-x:transform-with-nth(bin/fm-watch.sh)'"
matrix_case Z152 deny "fzf --bind 'ctrl-x:transform-pointer(bin/fm-watch.sh)'"
matrix_case Z153 deny "fzf --bind 'ctrl-x:transform-search(bin/fm-watch.sh)'"
matrix_case Z154 deny "fzf --bind 'ctrl-x:transform-footer(bin/fm-watch.sh)'"
matrix_case Z155 deny "fzf --bind 'ctrl-x:bg-transform-query(bin/fm-watch.sh)'"
matrix_case Z156 deny "fzf --bind 'ctrl-x:preview(bin/fm-watch.sh)'"
matrix_case Z157 deny "fzf --bind 'ctrl-x:transform-ghost[bin/fm-watch.sh]'"
matrix_case Z158 deny "fzf --bind 'ctrl-x:execute:bin/fm-watch.sh'"
matrix_case Z159 deny "fzf --bind 'ctrl-x:execute|bin/fm-watch.sh|'"
matrix_case Z160 deny "fzf --bind 'ctrl-x:accept+transform-ghost(bin/fm-watch.sh)'"
matrix_case Z161 allow "fzf --bind 'ctrl-x:transform-ghost(echo ok)'"
matrix_case Z162 allow "fzf --bind 'ctrl-x:execute:echo ok'"
matrix_case Z163 deny "sh -c 'P=bin/fm-watch.sh; set -- P; eval \"\${!1}\"'"
matrix_case Z164 allow "sh -c 'P=echo; set -- P; echo \"\${!1}\"'"
matrix_case Z165 deny "printf '!bin/fm-watch.sh\\n' | ex -s"
matrix_case Z166 deny "printf '!bin/fm-watch.sh\\n' | ed -s"
matrix_case Z167 allow "printf '!echo ok\\n' | ex -s"

MATRIX_TMP=$(mktemp -d "${TMPDIR:-/tmp}/fm-arm-policy-matrix.XXXXXX")
FM_TEST_CLEANUP_DIRS+=("$MATRIX_TMP")
trap fm_test_cleanup EXIT

run_matrix_entry() {
  local id=$1 expected=$2 entry=$3 cmd=$4 payload out_file err_file rc
  out_file="$MATRIX_TMP/$id-$entry.out"
  err_file="$MATRIX_TMP/$id-$entry.err"

  case "$entry" in
    codex)
      payload=$(jq -cn --arg command "$cmd" '{tool_name:"Bash",tool_input:{command:$command}}')
      printf '%s' "$payload" | "$CHECK" >"$out_file" 2>"$err_file"
      rc=$?
      ;;
    claude)
      payload=$(jq -cn --arg command "$cmd" '{tool_name:"Bash",tool_input:{command:$command}}')
      printf '%s' "$payload" | "$CHECK" --claude >"$out_file" 2>"$err_file"
      rc=$?
      ;;
    grok)
      payload=$(jq -cn --arg command "$cmd" '{toolName:"run_terminal_command",toolInput:{command:$command}}')
      printf '%s' "$payload" | "$CHECK" >"$out_file" 2>"$err_file"
      rc=$?
      ;;
    opencode|pi)
      printf '%s' "$cmd" | "$CHECK" --command-stdin >"$out_file" 2>"$err_file"
      rc=$?
      ;;
    *)
      fail "unknown matrix entry form: $entry"
      ;;
  esac

  if [ "$expected" = allow ]; then
    [ "$rc" -eq 0 ] || fail "$id via $entry must allow, got exit $rc: $(cat "$err_file")"
    [ ! -s "$out_file" ] || fail "$id via $entry allow must leave stdout empty: $(cat "$out_file")"
    [ ! -s "$err_file" ] || fail "$id via $entry allow must leave stderr empty: $(cat "$err_file")"
    return
  fi

  [ "$rc" -eq 2 ] || fail "$id via $entry must deny, got exit $rc"
  jq -e '.hookSpecificOutput.permissionDecision == "deny" and (.systemMessage | test("\\[(watcher-(background|pipeline|redirection|bundled|nested|direct)|broad-watcher-kill|unclassifiable-protected-command)\\]"))' "$err_file" >/dev/null 2>&1 \
    || fail "$id via $entry deny must carry a stable reason code on stderr: $(cat "$err_file")"
  if [ "$entry" = claude ]; then
    [ ! -s "$out_file" ] || fail "$id via claude deny must leave stdout empty: $(cat "$out_file")"
  elif [ "$entry" = grok ]; then
    jq -e '.decision == "deny"' "$out_file" >/dev/null 2>&1 \
      || fail "$id via grok deny must carry decision=deny on stdout: $(cat "$out_file")"
  fi
}

test_full_acceptance_matrix() {
  local i entry
  for ((i = 0; i < ${#MATRIX_IDS[@]}; i++)); do
    for entry in codex claude grok opencode pi; do
      run_matrix_entry "${MATRIX_IDS[$i]}" "${MATRIX_EXPECTED[$i]}" "$entry" "${MATRIX_COMMANDS[$i]}"
    done
    pass "matrix ${MATRIX_IDS[$i]}: ${MATRIX_EXPECTED[$i]} through all five entry forms"
  done
}

assert_policy() {
  local id=$1 expected=$2 command=$3 output
  output=$(node "$POLICY" --root "$ROOT" --home "$ROOT" --command "$command") \
    || fail "$id direct policy invocation failed"
  case "$output" in
    "$expected"|"$expected"$'\t'*) : ;;
    *) fail "$id direct policy expected $expected, got: $output" ;;
  esac
  pass "direct policy $id: $expected"
}

test_direct_policy_contract() {
  local heredoc_data heredoc_watcher
  assert_policy direct-data-pkill allow "echo 'pkill -f fm-watch'"
  assert_policy direct-broad-pkill $'deny\tbroad-watcher-kill' "pkill -f '/bin/fm-watch.sh'"
  assert_policy direct-loop-broad-pkill $'deny\tbroad-watcher-kill' 'while true; do pkill -f fm-watch; done'
  assert_policy direct-loop-broad-kill-pgrep $'deny\tbroad-watcher-kill' 'until false; do kill $(pgrep -f fm-watch); done'
  assert_policy direct-loop-no-kill-allowed allow 'for f in 1; do echo fm-watch; done'
  assert_policy direct-pipeline $'deny\twatcher-pipeline' 'bin/fm-watch-arm.sh | cat'
  assert_policy direct-leading-redirection $'deny\twatcher-redirection' '>/tmp/out bin/fm-watch-arm.sh'
  assert_policy direct-unclassifiable $'deny\tunclassifiable-protected-command' "bin/fm-watch-arm.sh 'unterminated"
  assert_policy direct-unsupported $'deny\tunclassifiable-protected-command' 'if true; then bin/fm-watch-arm.sh; fi'
  assert_policy direct-constructed-payload $'deny\twatcher-nested' "WATCHER='bin/fm-watch-arm.sh &'; bash -lc \"\$WATCHER\""
  assert_policy direct-parameter-export allow 'export FM_HOME=${HOME}; bin/fm-watch-checkpoint.sh --seconds 180'
  assert_policy direct-expanded-arm-blessed allow '$FM_HOME/bin/fm-watch-arm.sh'
  assert_policy direct-expanded-arm-background $'deny\twatcher-background' '$FM_HOME/bin/fm-watch-arm.sh &'
  assert_policy direct-expanded-arm-pipeline $'deny\twatcher-pipeline' '$HOME/firstmate/bin/fm-watch-arm.sh | cat'
  assert_policy direct-watch-not-blessed $'deny\twatcher-direct' 'bin/fm-watch.sh'
  assert_policy direct-watch-expanded $'deny\twatcher-direct' '$FM_HOME/bin/fm-watch.sh'
  assert_policy direct-watch-safe-shape $'deny\twatcher-direct' 'cd /tmp; bin/fm-watch.sh'
  heredoc_data=$'cat <<\'EOF\'\nbin/fm-watch-arm.sh &\nEOF'
  heredoc_watcher=$'bin/fm-watch-arm.sh <<\'EOF\'\ndata only\nEOF'
  assert_policy direct-heredoc-data allow "$heredoc_data"
  assert_policy direct-heredoc-watcher $'deny\twatcher-redirection' "$heredoc_watcher"
  assert_policy direct-bare-watch $'deny\twatcher-direct' 'fm-watch.sh'
  assert_policy direct-bare-pipeline $'deny\twatcher-pipeline' 'fm-watch-arm.sh | cat'
  assert_policy direct-bare-prefixed-pipeline $'deny\twatcher-pipeline' 'PATH=/tmp/fake/bin:$PATH fm-watch-arm.sh | cat'
  assert_policy direct-bare-prefixed-nested $'deny\twatcher-nested' 'PATH=/tmp/fake/bin:$PATH fm-watch-checkpoint.sh --seconds 180'
  assert_policy direct-bare-background $'deny\twatcher-background' 'fm-watch-arm.sh &'
  assert_policy direct-bare-blessed-chain allow 'cd /tmp && exec fm-watch-arm.sh'
  assert_policy direct-expansion-default-pipeline $'deny\tunclassifiable-protected-command' 'export PATH=/tmp/fake/bin:$PATH; ${X:-fm-watch-arm.sh} | cat'
  assert_policy direct-expansion-split-pipeline $'deny\tunclassifiable-protected-command' 'export PATH=/tmp/fake/bin:$PATH; ${X:-fm}-watch-arm.sh | cat'
  assert_policy direct-uppercase-pipeline $'deny\twatcher-pipeline' 'FM-WATCH-ARM.SH | cat'
  assert_policy direct-uppercase-sole-blessed allow 'FM-WATCH-ARM.SH'
}

# --- command wrappers (DPES-966) --------------------------------------------

# Every wrapper option-table entry, written out literally so a dropped or mistyped
# entry fails here. Format: owner option takes-argument (false or true).
WRAPPER_TABLE_ENTRIES='command -p false
command -v false
command -V false
env -0 false
env -i false
env -P true
env -v false
env -a true
env -C true
env -S true
env -u true
exec -c false
exec -l false
exec -a true
sudo -A false
sudo -B false
sudo -b false
sudo -E false
sudo -e false
sudo -H false
sudo -i false
sudo -K false
sudo -k false
sudo -l false
sudo -N false
sudo -n false
sudo -P false
sudo -S false
sudo -s false
sudo -v false
sudo -V false
sudo -C true
sudo -D true
sudo -g true
sudo -h true
sudo -p true
sudo -r true
sudo -R true
sudo -t true
sudo -T true
sudo -u true
sudo -U true
timeout -f false
timeout -p false
timeout -v false
timeout -k true
timeout -s true
command --help false
command --version false
env --ignore-environment false
env --null false
env --help false
env --version false
env --argv0 true
env --block-signal true
env --chdir true
env --default-signal true
env --ignore-signal true
env --split-string true
env --unset true
nohup --help false
nohup --version false
sudo --askpass false
sudo --background false
sudo --bell false
sudo --edit false
sudo --help false
sudo --login false
sudo --non-interactive false
sudo --preserve-env false
sudo --preserve-groups false
sudo --remove-timestamp false
sudo --reset-timestamp false
sudo --set-home false
sudo --shell false
sudo --stdin false
sudo --validate false
sudo --version false
sudo --chdir true
sudo --chroot true
sudo --close-from true
sudo --command-timeout true
sudo --group true
sudo --host true
sudo --other-user true
sudo --prompt true
sudo --role true
sudo --type true
sudo --user true
timeout --foreground false
timeout --preserve-status false
timeout --verbose false
timeout --help false
timeout --version false
timeout --kill-after true
timeout --signal true'

# Builds the forms of one wrapper invocation: short options take their argument as a
# separate word or inline, long options as a separate word or with `=`. A timeout
# invocation carries its duration operand before the command.
wrapper_forms() {
  local owner=$1 option=$2 takes=$3 operand=$4 pre=""
  [ "$owner" = timeout ] && pre="5 "
  if [ "$takes" = false ]; then
    printf '%s\n' "$owner $option ${pre}$operand"
  elif [[ "$option" == --* ]]; then
    printf '%s\n' "$owner $option ARG ${pre}$operand" "$owner $option=ARG ${pre}$operand"
  else
    printf '%s\n' "$owner $option ARG ${pre}$operand" "$owner ${option}ARG ${pre}$operand"
  fi
}

# Every wrapper form must reach the protected operand (deny) and must allow the same wrapper
# over an ordinary command. A missed option entry shifts the operand and fails one side.
# exec's protected final form is the blessed `exec` wrapper and stays allowed.
test_wrapper_option_entries() {
  local owner option takes form expected failures=() checked=0
  while read -r owner option takes; do
    [ -n "$owner" ] || continue
    case "$owner" in
      exec) expected=$'allow' ;;
      nohup) expected=$'deny\twatcher-background' ;;
      *) expected=$'deny\twatcher-nested' ;;
    esac
    while IFS= read -r form; do
      checked=$((checked + 1))
      local got
      got=$(node "$POLICY" --root "$ROOT" --home "$ROOT" --command "$form") || got="policy-error"
      case "$got" in
        "$expected"|"$expected"$'\t'*) : ;;
        *) failures+=("protected: $form expected $expected got $got") ;;
      esac
      form=${form//bin\/fm-watch-arm.sh/printf ok}
      got=$(node "$POLICY" --root "$ROOT" --home "$ROOT" --command "$form") || got="policy-error"
      [ "$got" = allow ] || failures+=("control: $form expected allow got $got")
    done < <(wrapper_forms "$owner" "$option" "$takes" bin/fm-watch-arm.sh)
  done <<< "$WRAPPER_TABLE_ENTRIES"
  if [ "${#failures[@]}" -gt 0 ]; then
    fail "wrapper option entries: ${#failures[@]} of $checked forms wrong: $(printf '%s; ' "${failures[@]}")"
  fi
  pass "wrapper option entries: $checked forms classified against their protected operand and control"
}

# Wrappers outside the option tables and the macOS env forms (DPES-966). BSD env reads
# `--unset X` as `-u nset` with X as the utility, so the operand position is platform-dependent
# and the node text is scanned instead.
test_unlisted_wrappers_and_env_forms() {
  assert_policy unlisted-nice $'deny\twatcher-nested' 'nice bin/fm-watch-arm.sh'
  assert_policy unlisted-nice-option $'deny\twatcher-nested' 'nice -n 5 bin/fm-watch-arm.sh'
  assert_policy unlisted-ionice $'deny\twatcher-nested' 'ionice -c 3 bin/fm-watch-arm.sh'
  assert_policy unlisted-setsid $'deny\twatcher-nested' 'setsid bin/fm-watch-arm.sh'
  assert_policy unlisted-stdbuf $'deny\twatcher-nested' 'stdbuf -oL bin/fm-watch-arm.sh'
  assert_policy unlisted-caffeinate $'deny\twatcher-nested' 'caffeinate -i bin/fm-watch-arm.sh'
  assert_policy unlisted-doas $'deny\twatcher-nested' 'doas -u root bin/fm-watch-arm.sh'
  assert_policy unlisted-chroot $'deny\twatcher-nested' 'chroot / bin/fm-watch-arm.sh'
  assert_policy unlisted-xargs-option $'deny\twatcher-nested' 'xargs -I {} bin/fm-watch-arm.sh'
  assert_policy unlisted-xargs-bare $'deny\twatcher-pipeline' 'echo x | xargs fm-watch-arm.sh'
  assert_policy unlisted-xargs-watch-fm $'deny\twatcher-pipeline' 'echo x | xargs fm-watch-arm.sh --restart'
  assert_policy unlisted-builtin-exec $'deny\twatcher-nested' 'builtin exec bin/fm-watch-arm.sh'
  assert_policy unlisted-builtin-eval $'deny\twatcher-nested' "builtin eval 'bin/fm-watch-arm.sh'"
  assert_policy unlisted-path-nice $'deny\twatcher-nested' '/usr/bin/nice -n 5 bin/fm-watch-arm.sh'
  assert_policy unlisted-nice-broad-kill $'deny\tbroad-watcher-kill' 'nice pkill -f fm-watch'
  assert_policy unlisted-nice-kill-pgrep $'deny\tbroad-watcher-kill' 'nice kill $(pgrep -f fm-watch)'
  assert_policy unlisted-nice-direct-watch $'deny\twatcher-direct' 'nice bin/fm-watch.sh'
  assert_policy unlisted-nice-unclassifiable $'deny\tunclassifiable-protected-command' "nice bin/fm-watch-arm.sh 'unterminated"
  assert_policy unlisted-nice-echo allow 'nice echo ok'
  assert_policy unlisted-data-echo allow 'echo bin/fm-watch-arm.sh'
  assert_policy unlisted-builtin-cd-file allow './builtin cd projects/foo'
  assert_policy env-macos-P $'deny\twatcher-nested' 'env -P /tmp bin/fm-watch-arm.sh'
  assert_policy env-macos-unset $'deny\twatcher-nested' 'env --unset bin/fm-watch-arm.sh true'
  assert_policy env-gnu-unset-operand $'deny\twatcher-nested' 'env --unset X bin/fm-watch-arm.sh'
  assert_policy env-bare-dash $'deny\twatcher-nested' 'env - bin/fm-watch-arm.sh'
  assert_policy env-dash-after-i $'deny\twatcher-nested' 'env -i - bin/fm-watch-arm.sh'
  assert_policy env-dash-utility-after-assignment allow 'env -i A=1 -u B ls'
  # Fail closed: a dash word after an assignment is the utility on GNU but may be read as an option on BSD.
  assert_policy env-assignment-then-dash-protected $'deny\tunclassifiable-protected-command' 'env A=1 -u bin/fm-watch-arm.sh ls'
  assert_policy unlisted-variable-operand $'deny\twatcher-nested' 'W=bin/fm-watch-arm.sh; nice -n 5 "$W"'
  assert_policy unlisted-variable-braced $'deny\twatcher-nested' 'W=bin/fm-watch-arm.sh; nice -n 5 "${W}"'
  # Prefix-assignment expansion order (DPES-991 residual, not changed by DPES-966): bash expands "$W" before
  # the prefix assignment is in effect, so this runs nice with an empty operand.
  assert_policy unlisted-variable-prefixed allow 'W=bin/fm-watch-arm.sh nice -n 5 "$W"'
  assert_policy unlisted-variable-data allow 'W=bin/fm-watch-arm.sh; echo "$W"'
  assert_policy unlisted-variable-benign allow 'W=ls; nice -n 5 "$W"'
  assert_policy pipeline-echo-into-sh $'deny\tunclassifiable-protected-command' 'echo bin/fm-watch-arm.sh | sh'
  assert_policy pipeline-printf-into-bash $'deny\tunclassifiable-protected-command' "printf 'bin/fm-watch-arm.sh\\n' | bash"
  assert_policy pipeline-xargs-into-sh $'deny\tunclassifiable-protected-command' 'echo bin/fm-watch-arm.sh | xargs sh'
  assert_policy pipeline-nice-into-sh $'deny\tunclassifiable-protected-command' 'echo bin/fm-watch-arm.sh | nice sh'
  assert_policy pipeline-data-echo-cat allow 'echo ok | cat'
  assert_policy pipeline-data-xargs-cat allow 'grep -rl fm-watch docs | xargs cat'
}

# --- bare watcher names resolved through PATH (DPES-954) ---------------------
# A bare name runs whatever PATH resolves it to. The stubs below stand in for
# the watcher and record each run; the real watcher is never invoked here.
test_bare_watcher_names_through_path_stub() {
  local sandbox marker name cmd rc
  sandbox=$(fm_test_tmproot fm-arm-bare-names)
  marker="$sandbox/ran"
  mkdir -p "$sandbox/bin"
  for name in fm-watch-arm.sh fm-watch.sh; do
    printf '#!/usr/bin/env bash\necho %s >> %s\n' "$name" "$marker" > "$sandbox/bin/$name"
    chmod +x "$sandbox/bin/$name"
  done
  for cmd in \
    "PATH=$sandbox/bin:\$PATH fm-watch-arm.sh | cat" \
    "export PATH=$sandbox/bin:\$PATH; fm-watch-arm.sh | cat" \
    "PATH=$sandbox/bin:\$PATH fm-watch.sh | cat" \
    "PATH=$sandbox/bin:\$PATH fm-watch-arm.sh &"; do
    "$CHECK" --command "$cmd" >/dev/null 2>&1
    rc=$?
    [ "$rc" -eq 2 ] || fail "bare watcher name must be denied, got exit $rc: $cmd"
    [ ! -e "$marker" ] || fail "denied bare watcher command reached the sandbox stub: $cmd"
  done
  # Control: the same PATH resolution runs the stub when nothing gates it, so
  # the denials above are what keep the stub from running.
  bash -c "PATH='$sandbox/bin':\$PATH fm-watch-arm.sh | cat" >/dev/null 2>&1
  [ -e "$marker" ] || fail "control: unguarded PATH resolution must reach the sandbox stub"
  pass "bare watcher names resolved through PATH are denied before the sandbox stub runs"
}

# DPES-963: a dynamic command-position word (variable split, glob, brace, command
# substitution, backtick, or xargs-launched name) can assemble a protected script name
# without literal watch bytes. Each shape must be denied before the sandbox stub runs,
# and each control proves the same shape reaches the stub when nothing gates it.
test_dynamic_command_position_names_through_path_stub() {
  local sandbox marker cmd rc
  sandbox=$(fm_test_tmproot fm-arm-dynamic-names)
  marker="$sandbox/ran"
  mkdir -p "$sandbox/bin" "$sandbox/cwd"
  printf '#!/usr/bin/env bash\necho fm-watch-arm.sh >> %s\n' "$marker" > "$sandbox/bin/fm-watch-arm.sh"
  chmod +x "$sandbox/bin/fm-watch-arm.sh"
  cp "$sandbox/bin/fm-watch-arm.sh" "$sandbox/cwd/fm-watch-arm.sh"
  for cmd in \
    "export PATH=$sandbox/bin:\$PATH; X=watch-arm.sh; fm-\$X | cat" \
    "export PATH=$sandbox/bin:\$PATH; X=wat; Y=ch-arm.sh; fm-\$X\$Y | cat" \
    "export PATH=$sandbox/bin:\$PATH; X=watch-arm.sh; fm-\$X &" \
    "cd $sandbox/cwd && ./fm-w*h-arm.sh | cat" \
    "cd $sandbox/cwd && ./fm-w*h-arm.sh &" \
    "echo x | PATH=$sandbox/bin:\$PATH xargs fm-watch-arm.sh" \
    "export PATH=$sandbox/bin:\$PATH; fm-{watch-arm.sh,nope} | cat" \
    "export PATH=$sandbox/bin:\$PATH; \$(echo fm-watch-arm.sh) | cat" \
    "export PATH=$sandbox/bin:\$PATH; X=\$(echo fm-watch-arm.sh); \$X | cat" \
    "export PATH=$sandbox/bin:\$PATH; \`echo fm-watch-arm.sh\` | cat"; do
    "$CHECK" --command "$cmd" >/dev/null 2>&1
    rc=$?
    [ "$rc" -eq 2 ] || fail "dynamic command-position watcher name must be denied, got exit $rc: $cmd"
    [ ! -e "$marker" ] || fail "denied dynamic command-position watcher reached the sandbox stub: $cmd"
  done
  for cmd in \
    "export PATH='$sandbox/bin':\$PATH; X=watch-arm.sh; fm-\$X | cat" \
    "cd '$sandbox/cwd' && ./fm-w*h-arm.sh | cat" \
    "echo x | PATH='$sandbox/bin':\$PATH xargs fm-watch-arm.sh" \
    "export PATH='$sandbox/bin':\$PATH; fm-{watch-arm.sh,nope} | cat" \
    "export PATH='$sandbox/bin':\$PATH; \$(echo fm-watch-arm.sh) | cat" \
    "export PATH='$sandbox/bin':\$PATH; X=\$(echo fm-watch-arm.sh); \$X | cat"; do
    rm -f "$marker"
    bash -c "$cmd" >/dev/null 2>&1
    [ -e "$marker" ] || fail "control: unguarded dynamic command-position resolution must reach the sandbox stub: $cmd"
  done
  pass "dynamic command-position watcher names (split, glob, brace, substitution, xargs) are denied before the sandbox stub runs"
}

# --- CLI parsing -------------------------------------------------------------

# A default or alternate expansion can supply the watcher name. The stub stands in
# for the watcher and records each run; the real watcher is never invoked here.
test_expansion_and_case_watcher_names_through_path_stub() {
  local sandbox marker cmd rc
  sandbox=$(fm_test_tmproot fm-arm-expansion-names)
  marker="$sandbox/ran"
  mkdir -p "$sandbox/bin"
  printf '#!/usr/bin/env bash\necho fm-watch-arm.sh >> %s\n' "$marker" > "$sandbox/bin/fm-watch-arm.sh"
  chmod +x "$sandbox/bin/fm-watch-arm.sh"
  for cmd in \
    "export PATH=$sandbox/bin:\$PATH; \${X:-fm-watch-arm.sh} | cat" \
    "export PATH=$sandbox/bin:\$PATH; \"\${X:-fm-watch-arm.sh}\" | cat" \
    "export PATH=$sandbox/bin:\$PATH; \${X:-fm}-watch-arm.sh | cat" \
    "export PATH=$sandbox/bin:\$PATH; FM-WATCH-ARM.SH | cat"; do
    "$CHECK" --command "$cmd" >/dev/null 2>&1
    rc=$?
    [ "$rc" -eq 2 ] || fail "expansion or case-folded watcher name must be denied, got exit $rc: $cmd"
    [ ! -e "$marker" ] || fail "denied expansion or case-folded watcher command reached the sandbox stub: $cmd"
  done
  # Control: the same expansion resolves through PATH and runs the stub when unguarded.
  bash -c "export PATH='$sandbox/bin':\$PATH; \${X:-fm-watch-arm.sh} | cat" >/dev/null 2>&1
  [ -e "$marker" ] || fail "control: unguarded default-expansion resolution must reach the sandbox stub"
  pass "default-expansion and case-folded watcher names are denied before the sandbox stub runs"
}

# DPES-963 review fixes: each shape the review found to bypass the policy is denied before a
# sandbox stub can run, and each control proves the same text reaches the stub when unguarded.
# Stubs stand in for the watcher; the kill-family shapes are only ever denied, never executed.
test_review_bypasses_through_path_stub() {
  local sandbox marker cmd rc
  sandbox=$(fm_test_tmproot fm-arm-review-bypass)
  marker="$sandbox/ran"
  mkdir -p "$sandbox/bin" "$sandbox/evil/bin"
  printf '#!/usr/bin/env bash\necho fm-watch-arm.sh >> %s\n' "$marker" > "$sandbox/bin/fm-watch-arm.sh"
  printf '#!/usr/bin/env bash\necho evil-fm-watch-arm.sh >> %s\n' "$marker" > "$sandbox/evil/bin/fm-watch-arm.sh"
  chmod +x "$sandbox/bin/fm-watch-arm.sh" "$sandbox/evil/bin/fm-watch-arm.sh"
  local runnable=(
    "export PATH=$sandbox/bin:\$PATH; read X <<< fm-watch-arm.sh; \$X | cat"
    "export PATH=$sandbox/bin:\$PATH; printf -v X fm-watch-arm.sh; \$X | cat"
    "export PATH=$sandbox/bin:\$PATH; for X in fm-watch-arm.sh; do \$X | cat; done"
    "export PATH=$sandbox/bin:\$PATH; eval 'X=fm-watch-arm.sh'; \$X | cat"
    "export PATH=$sandbox/bin:\$PATH; \${X:=fm-watch-arm.sh}; \$X | cat"
    "export PATH=$sandbox/bin:\$PATH; { X=fm-watch-arm.sh; }; \$X | cat"
    "export PATH=$sandbox/bin:\$PATH; X=\$(echo fm-watch-arm.sh); \$X | cat"
    "cd $sandbox/evil && bin/fm-watch-arm.sh"
    "cd $sandbox && X=fm-wat_ch-arm.sh; bin/\${X/_/} | cat"
    "cd $sandbox && printf 'bin/fm-watch-arm.sh\n' | bash"
    "cd $sandbox && printf 'bin/fm-watch-arm.sh\n' | xargs sh -c"
    "cd $sandbox && sh -c \"\$(printf bin/fm-watch-arm.sh)\""
    "cd $sandbox && ! bin/fm-watch-arm.sh"
    "cd $sandbox && nice -n 5 bin/fm-watch-arm.sh"
  )
  for cmd in "${runnable[@]}"; do
    "$CHECK" --command "$cmd" >/dev/null 2>&1
    rc=$?
    [ "$rc" -eq 2 ] || fail "review bypass must be denied, got exit $rc: $cmd"
    [ ! -e "$marker" ] || fail "denied review bypass reached the sandbox stub: $cmd"
  done
  # Kill-family shapes are denied without ever being run; no stub stands in for pkill or killall.
  for cmd in \
    "P=fm-wat; Q=ch-arm.sh; pkill -f \"\$P\$Q\"" \
    "killall fm-watch.sh" \
    "ps aux | grep fm-watch.sh | xargs kill" \
    "pkill -f 'fm-wat\\ch'"; do
    "$CHECK" --command "$cmd" >/dev/null 2>&1
    rc=$?
    [ "$rc" -eq 2 ] || fail "review kill bypass must be denied, got exit $rc: $cmd"
  done
  # Controls: the same text runs the stub when nothing gates it, so the denials are what hold it.
  for cmd in "${runnable[@]}"; do
    rm -f "$marker"
    bash -c "$cmd" >/dev/null 2>&1
    [ -e "$marker" ] || fail "control: unguarded review bypass must reach the stub: $cmd"
  done
  # The bare name gets its own reason; bin/fm-watch.sh keeps its file-path reason.
  "$CHECK" --command 'fm-watch' 2>&1 >/dev/null | grep -q 'bare name fm-watch resolves to the watcher' \
    || fail "bare fm-watch must report the bare-name reason"
  "$CHECK" --command 'bin/fm-watch.sh' 2>&1 >/dev/null | grep -q 'bin/fm-watch.sh must not be run directly' \
    || fail "bin/fm-watch.sh must keep its file-path reason"
  pass "DPES-963 review bypasses (variable builtins, loops, eval, group, command-substitution names, cd to a non-root tree, kill-family shapes) are denied before the sandbox stub runs"
}

test_command_equals_form() {
  "$CHECK" --command='bin/fm-watch-arm.sh &' >/dev/null 2>&1
  [ "$?" -eq 2 ] || fail "--command=<val> form must parse the same as --command <val>"
  pass "--command=<val> equals-form parses correctly"
}

test_background_flag_accepted_and_non_gating() {
  local rc_bg rc_nobg
  "$CHECK" --command 'exec bin/fm-watch-arm.sh' --background true >/dev/null 2>&1
  rc_bg=$?
  "$CHECK" --command 'exec bin/fm-watch-arm.sh' >/dev/null 2>&1
  rc_nobg=$?
  [ "$rc_bg" -eq 0 ] || fail "--background true must not change the allow decision on its own, got exit $rc_bg"
  [ "$rc_bg" -eq "$rc_nobg" ] || fail "--background flag must be accepted without altering the decision"
  pass "--background is accepted for interface parity and is never itself a deny signal"
}

test_unknown_flag_errors() {
  "$CHECK" --bogus-flag >/dev/null 2>&1
  [ "$?" -eq 2 ] || fail "an unrecognized flag must exit non-zero, not silently allow"
  pass "unknown CLI flag is rejected"
}

# --- stdin JSON mode ----------------------------------------------------------

test_stdin_grok_schema_deny() {
  local out rc
  out=$(printf '%s' '{"toolInput":{"command":"bin/fm-watch-arm.sh &","background":false},"toolName":"run_terminal_command"}' | "$CHECK" 2>/dev/null)
  rc=$?
  [ "$rc" -eq 2 ] || fail "grok toolInput.command schema must be read and denied, got exit $rc"
  printf '%s' "$out" | jq -e '.decision == "deny"' >/dev/null 2>&1 || fail "stdout must carry Grok's {\"decision\":\"deny\",...} shape: $out"
  pass "stdin grok schema (toolInput.command): denied with Grok-shaped stdout JSON"
}

test_stdin_claude_codex_schema_allow() {
  local rc
  printf '%s' '{"tool_input":{"command":"exec bin/fm-watch-arm.sh"},"tool_name":"Bash"}' | "$CHECK" >/dev/null 2>&1
  rc=$?
  [ "$rc" -eq 0 ] || fail "claude/codex tool_input.command schema must be read and allowed for the blessed shape, got exit $rc"
  pass "stdin claude/codex schema (tool_input.command): blessed shape allowed"
}

test_stdin_claude_codex_schema_deny() {
  local rc
  printf '%s' '{"tool_input":{"command":"bin/fm-watch-arm.sh &"},"tool_name":"Bash"}' | "$CHECK" >/dev/null 2>&1
  rc=$?
  [ "$rc" -eq 2 ] || fail "claude/codex tool_input.command schema must be denied for the backgrounded shape, got exit $rc"
  pass "stdin claude/codex schema (tool_input.command): backgrounded shape denied"
}

test_stdin_unrelated_command_allowed() {
  local rc
  printf '%s' '{"tool_input":{"command":"ls -la"},"tool_name":"Bash"}' | "$CHECK" >/dev/null 2>&1
  rc=$?
  [ "$rc" -eq 0 ] || fail "an unrelated command must pass through allowed, got exit $rc"
  pass "stdin: unrelated command is a fast allow"
}

test_prefilter_is_strict_superset() {
  local rc
  # A command with no fm-watch substring is fast-allowed by the transport
  # prefilter without ever invoking the classifier.
  "$CHECK" --command 'ls -la /bin && echo done' >/dev/null 2>&1
  rc=$?
  [ "$rc" -eq 0 ] || fail "a command with no fm-watch substring must be fast-allowed, got exit $rc"
  # A deniable protected execution carries the fm-watch bytes, so the prefilter
  # must delegate to the classifier and the deny must survive.
  "$CHECK" --command 'bin/fm-watch-arm.sh &' >/dev/null 2>&1
  rc=$?
  [ "$rc" -eq 2 ] || fail "prefilter must delegate a deniable fm-watch command, not fast-allow it, got exit $rc"
  # A broad watcher kill also contains the fm-watch bytes and must still deny.
  "$CHECK" --command "pkill -f '/bin/fm-watch.sh'" >/dev/null 2>&1
  rc=$?
  [ "$rc" -eq 2 ] || fail "prefilter must delegate a broad watcher kill, not fast-allow it, got exit $rc"
  # Obfuscated protected paths lose the literal fm-watch bytes (a line
  # continuation or a quote splits them), yet the classifier reconstructs them.
  # The prefilter normalizes those bytes first, so both must still delegate and
  # deny rather than slip through as a fast allow.
  "$CHECK" --command "$(printf 'bin/fm-watc\\\nh-arm.sh &')" >/dev/null 2>&1
  rc=$?
  [ "$rc" -eq 2 ] || fail "prefilter must delegate a line-continuation-split protected path, not fast-allow it, got exit $rc"
  "$CHECK" --command 'bin/fm-"watch-arm.sh" &' >/dev/null 2>&1
  rc=$?
  [ "$rc" -eq 2 ] || fail "prefilter must delegate a quote-split protected path, not fast-allow it, got exit $rc"
  # A quoting-decoder marker ($' ANSI-C or $" locale) hides the fm-watch bytes
  # from the cheap byte strip but the classifier reconstructs them, so the
  # prefilter must delegate on the marker rather than fast-allow. Without this
  # the byte strip loses the encoded character and slips the command through.
  "$CHECK" --command "bin/fm-\$'\x77'atch-arm.sh &" >/dev/null 2>&1
  rc=$?
  [ "$rc" -eq 2 ] || fail "prefilter must delegate an ANSI-C-encoded protected path, not fast-allow it, got exit $rc"
  "$CHECK" --command 'bin/fm-$"watch"-arm.sh &' >/dev/null 2>&1
  rc=$?
  [ "$rc" -eq 2 ] || fail "prefilter must delegate a locale-string-encoded protected path, not fast-allow it, got exit $rc"
  # The marker is specifically $ followed by a quote, not any $ expansion: an
  # ordinary $VAR that is not a watcher reference still takes the fast path.
  "$CHECK" --command '$FM_HOME/bin/fm-teardown.sh &' >/dev/null 2>&1
  rc=$?
  [ "$rc" -eq 0 ] || fail "a benign \$VAR non-watcher command must still be allowed, got exit $rc"
  # Watcher identity is case-folded and a parameter expansion can supply part of
  # the name, so neither may fast-allow on a missing lowercase fm-watch substring.
  "$CHECK" --command 'FM-WATCH-ARM.SH &' >/dev/null 2>&1
  rc=$?
  [ "$rc" -eq 2 ] || fail "an uppercase watcher name must delegate and deny, got exit $rc"
  "$CHECK" --command '$X-watch-arm.sh | cat' >/dev/null 2>&1
  rc=$?
  [ "$rc" -eq 2 ] || fail "a variable-prefixed watcher name must delegate and deny, got exit $rc"
  "$CHECK" --command '${X:-fm}-watch-arm.sh | cat' >/dev/null 2>&1
  rc=$?
  [ "$rc" -eq 2 ] || fail "a default-expansion watcher name must delegate and deny, got exit $rc"
  "$CHECK" --command 'echo "$HOME/scratch" && ls -la' >/dev/null 2>&1
  rc=$?
  [ "$rc" -eq 0 ] || fail "a benign \$HOME command must still be allowed, got exit $rc"
  # A benign command that only mentions fm-watch as data still reaches the
  # classifier and is allowed there, proving the prefilter owns no verdict.
  "$CHECK" --command "echo 'pkill -f fm-watch'" >/dev/null 2>&1
  rc=$?
  [ "$rc" -eq 0 ] || fail "a benign fm-watch-substring command must be classified and allowed, got exit $rc"
  pass "transport prefilter is a strict superset: non-fm-watch fast-allows, every fm-watch and quoting-decoder-marker command reaches the classifier"
}

# --- fail-open ----------------------------------------------------------------

test_failopen_empty_stdin() {
  local rc
  printf '' | "$CHECK" >/dev/null 2>&1
  rc=$?
  [ "$rc" -eq 0 ] || fail "empty stdin must fail open (exit 0), got exit $rc"
  pass "fail-open: empty stdin"
}

test_failopen_garbage_stdin() {
  local rc
  printf 'not json at all {{{' | "$CHECK" >/dev/null 2>&1
  rc=$?
  [ "$rc" -eq 0 ] || fail "unparseable stdin must fail open (exit 0), got exit $rc"
  pass "fail-open: unparseable JSON on stdin"
}

test_failopen_missing_jq() {
  local dir fakebin rc real
  dir=$(fm_test_tmproot fm-arm-pretool-check)
  fakebin="$dir/fakebin"
  mkdir -p "$fakebin"
  local tool
  for tool in bash grep sed tr; do
    real=$(command -v "$tool")
    ln -sf "$real" "$fakebin/$tool"
  done
  PATH="$fakebin" bash -c "printf '%s' '{\"tool_input\":{\"command\":\"bin/fm-watch-arm.sh &\"}}' | '$CHECK'" >/dev/null 2>&1
  rc=$?
  [ "$rc" -eq 0 ] || fail "missing jq must fail open (exit 0) rather than crash-deny, got exit $rc"
  pass "fail-open: missing jq on stdin path"
}

test_failopen_missing_node() {
  local dir fakebin rc real tool
  dir=$(fm_test_tmproot fm-arm-pretool-node)
  fakebin="$dir/fakebin"
  mkdir -p "$fakebin"
  for tool in bash dirname; do
    real=$(command -v "$tool")
    ln -sf "$real" "$fakebin/$tool"
  done
  PATH="$fakebin" "$CHECK" --command 'bin/fm-watch-arm.sh &' >/dev/null 2>&1
  rc=$?
  [ "$rc" -eq 0 ] || fail "missing node must fail open (exit 0), got exit $rc"
  pass "fail-open: missing classifier runtime"
}

# --- --claude output shaping ---------------------------------------------------

test_claude_mode_stdout_empty_on_deny() {
  local out err rc stderr_file
  # Keep stderr capture under TMPDIR so concurrent isolation-proof workers do
  # not share a fixed global /tmp path.
  stderr_file=$(mktemp "${TMPDIR:-/tmp}/fm-arm-pretool-check-claude-stderr.XXXXXX")
  out=$("$CHECK" --claude --command 'bin/fm-watch-arm.sh &' 2>"$stderr_file")
  rc=$?
  err=$(cat "$stderr_file" 2>/dev/null)
  rm -f "$stderr_file"
  [ "$rc" -eq 2 ] || fail "--claude deny must still exit 2, got $rc"
  [ -z "$out" ] || fail "--claude deny must leave stdout EMPTY (Claude Code only honors a stderr-only deny), got: $out"
  printf '%s' "$err" | jq -e '.hookSpecificOutput.permissionDecision == "deny"' >/dev/null 2>&1 \
    || fail "--claude deny must put hookSpecificOutput.permissionDecision=deny on stderr: $err"
  pass "--claude: stdout empty, stderr carries hookSpecificOutput deny JSON"
}

test_default_mode_stdout_has_grok_json_on_deny() {
  local out rc
  out=$("$CHECK" --command 'bin/fm-watch-arm.sh &' 2>/dev/null)
  rc=$?
  [ "$rc" -eq 2 ] || fail "default deny must exit 2, got $rc"
  printf '%s' "$out" | jq -e '.decision == "deny"' >/dev/null 2>&1 \
    || fail "default (non-claude) deny must put Grok's decision JSON on stdout: $out"
  pass "default mode: stdout carries Grok-shaped decision JSON on deny"
}

test_allow_is_silent_both_modes() {
  local out1 out2
  out1=$("$CHECK" --command 'exec bin/fm-watch-arm.sh' 2>&1)
  out2=$("$CHECK" --claude --command 'exec bin/fm-watch-arm.sh' 2>&1)
  [ -z "$out1" ] || fail "default allow must be silent, got: $out1"
  [ -z "$out2" ] || fail "--claude allow must be silent, got: $out2"
  pass "allow is silent on both stdout and stderr in default and --claude mode"
}

# --- harness wiring: adapters send the command on stdin (DPES-966 F1) ----------

test_adapters_send_command_on_stdin() {
  local out
  command -v node >/dev/null 2>&1 || { pass "node not installed, skipping OpenCode adapter stdin check"; return; }
  out=$(FM_ROOT="$ROOT" node --input-type=module -e '
    const { FmPrimaryPretoolCheck } = await import(process.env.FM_ROOT + "/.opencode/plugins/fm-primary-pretool-check.js");
    const hook = (await FmPrimaryPretoolCheck({ worktree: process.env.FM_ROOT }))["tool.execute.before"];
    const padding = "x".repeat(1200000);
    for (const [label, command] of [["watch", `echo ${padding}; bin/fm-watch-arm.sh`], ["benign", `echo ${padding}`]]) {
      try {
        await hook({ tool: "bash" }, { args: { command } });
        console.log(label, "allowed");
      } catch {
        console.log(label, "denied");
      }
    }
  ' 2>&1) || fail "OpenCode adapter check failed to run: $out"
  [ "$out" = "$(printf 'watch denied\nbenign allowed')" ] \
    || fail "OpenCode adapter must deny a 1.2 MB protected command and allow a benign one, got: $out"
  pass "OpenCode adapter sends a 1.2 MB command on stdin: protected denied, benign allowed"

  local adapter
  for adapter in .pi/extensions/fm-primary-turnend-guard.ts .omp/extensions/fm-primary-turnend-guard.ts; do
    grep -Fq 'runChecker("fm-arm-pretool-check.sh", ["--command-stdin"], command)' "$ROOT/$adapter" \
      || fail "$adapter must pass the command to the arm check on stdin, not argv"
  done
  pass "Pi and omp adapters pass the command to the arm check on stdin"
}

# --- shellcheck (belt-and-suspenders; CI/CONTRIBUTING.md also runs this) -----
#
# Delegated to bin/fm-lint.sh rather than calling shellcheck directly, because
# that script is the single owner of the lint definition - the file set, the
# pinned version, and the options, including --external-sources. Calling the
# linter directly here would be a second, weaker copy of that definition, and it
# disagreed with the owner the moment this checker sourced a shared library.

# DPES-966 AC5 (PR 333 attempt 1, F1): a protected command larger than the argument-length limit must still
# be classified. The command reaches the classifier on stdin, so the protected 1.2 MB command denies, and an
# equally large benign command stays silently allowed.
test_oversized_protected_command_stdin() {
  local sandbox payload out err rc
  sandbox=$(fm_test_tmproot fm-arm-oversized)
  payload="$sandbox/payload.txt"
  out="$sandbox/out"
  err="$sandbox/err"
  printf 'bin/fm-watch-arm.sh & # ' > "$payload"
  head -c 1200000 /dev/zero | tr '\0' x >> "$payload"
  printf '\n' >> "$payload"
  jq -Rn --rawfile command "$payload" '{tool_name:"Bash",tool_input:{command:$command}}' | "$CHECK" >"$out" 2>"$err"
  rc=$?
  [ "$rc" -eq 2 ] || fail "oversized protected command must deny (exit 2), got exit $rc"
  jq -e '.hookSpecificOutput.permissionDecision == "deny"' "$err" >/dev/null 2>&1 \
    || fail "oversized protected command deny must carry the deny decision on stderr: $(cat "$err")"
  pass "oversized protected command (1.2 MB) denies on stdin"

  printf 'echo ok # ' > "$payload"
  head -c 1200000 /dev/zero | tr '\0' x >> "$payload"
  printf '\n' >> "$payload"
  jq -Rn --rawfile command "$payload" '{tool_name:"Bash",tool_input:{command:$command}}' | "$CHECK" >"$out" 2>"$err"
  rc=$?
  [ "$rc" -eq 0 ] || fail "oversized benign command must allow (exit 0), got exit $rc"
  [ ! -s "$out" ] && [ ! -s "$err" ] || fail "oversized benign command must allow silently"
  pass "oversized benign command (1.2 MB) allows silently"
}

test_shellcheck_clean() {
  local out
  command -v shellcheck >/dev/null 2>&1 || { pass "shellcheck not installed, skipping"; return; }
  out=$("$ROOT/bin/fm-lint.sh" "$CHECK" 2>&1)     || fail "bin/fm-arm-pretool-check.sh is not lint-clean under the pinned definition: $out"
  pass "bin/fm-arm-pretool-check.sh is clean under bin/fm-lint.sh"
}

test_full_acceptance_matrix
test_direct_policy_contract
test_wrapper_option_entries
test_unlisted_wrappers_and_env_forms
test_bare_watcher_names_through_path_stub
test_expansion_and_case_watcher_names_through_path_stub
test_dynamic_command_position_names_through_path_stub
test_review_bypasses_through_path_stub
test_command_equals_form
test_background_flag_accepted_and_non_gating
test_unknown_flag_errors
test_stdin_grok_schema_deny
test_stdin_claude_codex_schema_allow
test_stdin_claude_codex_schema_deny
test_stdin_unrelated_command_allowed
test_prefilter_is_strict_superset
test_failopen_empty_stdin
test_failopen_garbage_stdin
test_failopen_missing_jq
test_failopen_missing_node
test_oversized_protected_command_stdin
test_adapters_send_command_on_stdin
test_claude_mode_stdout_empty_on_deny
test_default_mode_stdout_has_grok_json_on_deny
test_allow_is_silent_both_modes
test_shellcheck_clean
