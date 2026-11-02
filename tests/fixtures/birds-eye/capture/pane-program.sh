#!/usr/bin/env bash
# Pane program for tests/fm-birds-eye-capture.test.sh. Prints 150 numbered
# lines, one with SGR color, then an OSC title and a BEL, then a closing marker,
# and holds the pane open so the capture reads a live, settled pane.
set -u
i=1
while [ "$i" -le 150 ]; do
  printf 'line-%03d \033[31mred\033[0m\n' "$i"
  i=$((i + 1))
done
printf '\033]0;fixture-title\007'
printf 'fixture-end\n'
sleep 600
