#!/usr/bin/env bash
# Test stub for bin/fm-birds-eye-capture.sh, injected only by tests/fm-birds-eye-view.test.sh.
# Appends its argv to FM_STUB_CAPTURE_RECORD and answers with a fm-birds-eye-capture.v1
# document whose text echoes the task, generation, and line count it received, so a
# test can see exactly what the server forwarded. A generation of "stale" stands for a
# generation the worker no longer has; the stub answers generation_mismatch without
# text, as the real helper does.
set -u

task=""
gen=""
lines=""
printf '%s\n' "$*" >> "${FM_STUB_CAPTURE_RECORD:?}"
while [ "$#" -ge 2 ]; do
  case "$1" in
    --root) ;;
    --task) task=$2 ;;
    --spawn-gen) gen=$2 ;;
    --lines) lines=$2 ;;
    *)
      printf 'stub capture: unexpected argument %s\n' "$1" >&2
      exit 2
      ;;
  esac
  shift 2
done

if [ "$gen" = stale ]; then
  printf '{"schema":"fm-birds-eye-capture.v1","task_id":"%s","spawn_gen_requested":"%s","status":"generation_mismatch","lines_requested":%s,"text":""}\n' \
    "$task" "$gen" "$lines"
else
  printf '{"schema":"fm-birds-eye-capture.v1","task_id":"%s","spawn_gen_requested":"%s","status":"ok","lines_requested":%s,"text":"task=%s gen=%s lines=%s"}\n' \
    "$task" "$gen" "$lines" "$task" "$gen" "$lines"
fi
