---
name: birds-eye-view
description: >-
  Produce the bird's-eye view of all agent work when the user invokes /birds-eye-view or asks for a bird's-eye view, an overview of every worker, or a view of all agent work.
  Writes one offline HTML snapshot of the configured home with bin/fm-birds-eye-view.sh --snapshot, opens it in the default browser when supported, and reports its path, counts, capture time, and coverage.
  Starts the live loopback service only when the user explicitly asks for live mode.
user-invocable: true
metadata:
  internal: true
---

# birds-eye-view

The bird's-eye view shows every observed worker, supervisor, and attention item in one page.
It only observes: it reads operational homes and changes nothing in them.

## Default: write a snapshot

1. Resolve scope.
   The configured home (`FM_HOME`, default the code root) is always observed, and the view covers every project in it.
   Do not narrow the view to the project you are working in.
   Add another home only when the user names its path, and pass it as `--root <home-dir>`, repeating the flag for more homes.
2. Write the snapshot.

   ```sh
   bin/fm-birds-eye-view.sh --snapshot state/birds-eye/snapshot.html [--root <home-dir>]...
   ```

   This starts no listener and no collector, and it exits 0 when the file is written.
   `state/` is gitignored and private to this home, and the exporter creates the parent directory when it is missing.
   The write is atomic, so a failed run leaves an earlier snapshot in place.
3. Read the output.
   A successful run prints two lines:

   ```text
   wrote <path> | roots=<N> entities=<N> attention=<N> unsupported=<categories> captured_at=<timestamp>
   snapshot written: <path>
   ```

   Use these values as printed; do not recount them.
4. Open it when the platform supports it.
   On macOS run `open <path>`; elsewhere run `xdg-open <path>`.
   If the opener is missing or fails, report the path and say it was not opened.
5. Report to the user in plain words.
   Give the snapshot path, whether it opened, the number of items in the view, the number that need attention, and the captured time.
   Describe `entities` as the items in the view and `attention` as items that need the user's decision or a look.
   List every `unsupported` category as not covered; native harness subagents are never reported as a count of zero.
   Say the page is a snapshot from the captured time and does not refresh.

## Live mode, only on an explicit request

- Run `bin/fm-birds-eye-view.sh [--root <home-dir>]... [--interval <seconds>] [--open]` only when the user asks for a live view.
- The interval defaults to 10 seconds, and a value below 5 is refused with exit 2, never raised to 5.
- The command prints `birds-eye view: <url>` and keeps running until it is stopped.
- The listener is bound to 127.0.0.1 only.
- Run it in a process you own, record its PID, and stop only that PID with `kill -TERM <pid>`.
- Never stop it by name pattern, because a pattern can match other homes' processes.
- Its working files are removed on exit, and it prints `birds-eye view stopped`.

## Failures

- Exit 2 is a usage error; fix the arguments and do not guess values.
- Exit 1 with `snapshot exporter is not available` means the exporter file is missing or not executable; report that the snapshot cannot be written, and do not switch to live mode on your own.
- Any other exit 1 is a runtime failure; report the error line in plain words and keep any earlier snapshot.
