# Bird's-eye view service

`bin/fm-birds-eye-view.sh` serves the bird's-eye view dashboard on a loopback address and refreshes its fleet view on an interval.
The service only observes: it reads operational homes and never changes them.
The page, its assets, and the view and capture routes are served by a small standard-library Python helper, `bin/fm-birds-eye-view-serve.py`.

## Running the live service

```sh
bin/fm-birds-eye-view.sh
```

The command prints a URL of the form `birds-eye view: http://127.0.0.1:<port>/` and keeps running until it receives SIGINT or SIGTERM.
Stop it with Ctrl-C or with `kill -TERM <pid>`.

## Arguments

- `--root <home-dir>` observes an additional operational home.
- Repeat it for more homes.
- The configured home is always observed first, and a home named more than once is observed once.
- `--interval <seconds>` sets the refresh interval in whole seconds.
- The default is 10.
- The minimum is 5, and a lower value is refused with exit 2, never clamped.
- `--port <port>` binds a fixed port from 1 to 65535.
- Without it, the operating system chooses a free port.
- A port already in use is an error, and the process holding it is left running.
- `--open` opens the dashboard in the default browser.
- When no opener is available or it fails, the URL is still printed with a note, and the service keeps running.
- `--snapshot <path>` writes one self-contained HTML snapshot to `<path>` through `bin/fm-birds-eye-export.sh` and exits.
- The page embeds a projection of the `fm-birds-eye-view.v1` document.
- A snapshot starts no listener and no collector.
- `--snapshot` cannot be combined with `--port` or `--open`.
- `-h` or `--help` prints the usage and exits 0.

## Environment

- `FM_HOME` selects the configured home that is observed first.
- It defaults to the code root.
- The `FM_BIRDS_EYE_MODEL_OVERRIDE`, `FM_BIRDS_EYE_CAPTURE_OVERRIDE`, and `FM_BIRDS_EYE_EXPORT_OVERRIDE` variables replace the sibling helpers for interface tests only.
- Production runs must not set them.

## Refresh and staleness

One collector runs one cycle at a time.
Each cycle starts after the previous cycle has finished, and no earlier than one interval after the previous start.
A cycle that runs longer than the interval delays the next cycle, so cycles never overlap.
A cycle that does not finish within 120 seconds is stopped and counted as failed.

Each cycle passes the interval and the roots to `bin/fm-birds-eye-model.sh`, which owns staleness and per-root outcomes.
A root is stale when its last good collection is older than `max(30, 3 x interval)` seconds.
A root that cannot be read keeps its last good records and is marked stale, while the other roots render normally.

When a whole cycle fails, times out, or returns output that is not an `fm-birds-eye-view.v1` document, the server keeps serving the last good view.
The failure is logged on stderr with the words `keeping the last good view`.
The next cycle runs on schedule and replaces the view once it succeeds.
Until the first cycle completes, `/api/view` answers 503.

## Output capture

Output capture runs only when the dashboard requests it, never during refresh.
A request names a worker by key and the spawn generation the view showed.
A key that is not in the current view returns `unknown_identity` without text.
A missing or empty generation returns `generation_mismatch` without text, and the helper is not invoked.
The generation is forwarded to `bin/fm-birds-eye-capture.sh` unchanged.
The default is 40 lines.
A requested line count above 100 is limited to 100.
A line count that is not a whole number of at least 1 is refused with 400.

## HTTP surface

The service listens only on `127.0.0.1`.
It accepts GET only, and every other method returns 405.
The `Host` header must be exactly `127.0.0.1:<port>`, and any other value returns 403.
An `Origin` header, when sent, must be exactly `http://127.0.0.1:<port>`, and any other value returns 403.
Chunked bodies and non-zero request bodies are refused.
Only `/`, `/app.css`, `/app.js`, `/api/view`, and `/api/capture` are served.
Any other path returns 404.
The asset paths take no query, and a query on them returns 400.
Every response carries a Content-Security-Policy, `X-Content-Type-Options: nosniff`, `Cache-Control: no-store`, `Cross-Origin-Resource-Policy: same-origin`, and `Referrer-Policy: no-referrer`.
The dashboard loads only its own local assets, and it makes no requests to external hosts.

## Shutdown

SIGINT or SIGTERM stops the HTTP listener and the collector.
The service signals only the process groups that it started itself, first with SIGTERM and then with SIGKILL after a five second grace period.
It removes its temporary working directory, prints `birds-eye view stopped`, and exits 0.
It does not signal any other process, and it does not touch agent sessions.

## Exit codes

- 0: the live service stopped normally, a snapshot was written, or help was printed.
- 1: a runtime failure.
- Runtime failures include a missing model or capture helper in live mode, a missing or failed exporter for `--snapshot`, a port already in use, a bind failure, and unreadable dashboard assets.
- 2: a usage error.
- Usage errors include an unknown flag, a bad value, an interval below 5, a port outside 1 to 65535, and `--snapshot` combined with `--port` or `--open`.

## Dependencies

- bash.
- python3, standard library only, with no packages to install.
- `bin/fm-birds-eye-model.sh` and `bin/fm-birds-eye-capture.sh`, both on main.
- `bin/fm-birds-eye-export.sh`, which writes the snapshot page.
- The dashboard assets in `assets/birds-eye-view/`.

## Current status

- Live mode uses the aggregation model and the capture helper from main.
- `--snapshot` exits 1 with `snapshot exporter is not available` only when `bin/fm-birds-eye-export.sh` is missing or not executable, and it writes no file in that case.
- `--open` uses `open` on macOS and `xdg-open` elsewhere.
- The macOS path is the verified platform.
- The `xdg-open` path has not been run on another platform.

## Verification

- `tests/fm-birds-eye-view.test.sh` runs the CLI and the server against stand-in helpers at the three override points.
- Those tests cover argument refusals, the HTTP surface, capture forwarding, busy and fixed ports, refresh spacing, overlap, last-good carry-forward, `--open` outcomes, and shutdown.
- These tests do not exercise the real aggregation model or capture helper.
- `tests/fm-birds-eye-export.test.sh` covers the snapshot exporter's output, escaping, and Content-Security-Policy.
- `tests/fm-birds-eye-view-acceptance.test.sh` runs the collector on a fake clock, and it runs the CLI and the live service with the real aggregation model and capture helper under a network-isolation profile.
- `docs/birds-eye-view-validation.md` maps each acceptance criterion and test plan item to its evidence and status.
