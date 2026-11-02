#!/usr/bin/env bash
# Test stub for bin/fm-session-start.sh: records its argv instead of running the digest.
printf "FULL"
for arg in "$@"; do printf " [%s]" "$arg"; done
printf "\n"
