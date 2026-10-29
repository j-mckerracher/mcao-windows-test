# Birds-eye observer dashboard verification

Audience: maintainer verification.

This record holds the dated evidence for the observer dashboard in `assets/birds-eye-view/`.
It names the commands that re-prove each guarantee, so a refresh replaces the numbers rather than the claims.
The page consumes the frozen `fm-birds-eye-view.v1` and `fm-birds-eye-capture.v1` documents, and this record does not restate their fields.
Task chronology, branch names, and temporary paths remain in the private task report.

## Environment

Recorded on 2026-10-17.

- Host: macOS 25.6.0 (Darwin), arm64, model Mac15,6, Apple M3 Pro, 11 logical cores, 36.0 GiB memory.
- Node.js v26.8.1, which provides the global `WebSocket` and `fetch` the harness uses.
- Google Chrome 155.0.8059.85 at `/Applications/Google Chrome.app/Contents/MacOS/Google Chrome`.
- chrome-devtools-axi 0.1.35, with chrome-devtools-mcp 1.11.0 supplied through `CHROME_DEVTOOLS_AXI_MCP_PATH` and `CHROME_DEVTOOLS_AXI_BRIDGE_TIMEOUT_MS=90000`.
- ShellCheck 0.11.0 and actionlint 1.7.12, the pinned versions `bin/fm-lint.sh` enforces.

## Commands

The enforced browser suite fails when Chrome or Node.js is missing.

```sh
FM_BIRDS_EYE_UI=1 bash tests/fm-birds-eye-ui.test.sh
```

The harness runs the same scenarios with an explicit Chrome path and output directory.

```sh
node tests/fixtures/birds-eye/ui/ui-harness.mjs \
  --chrome "/Applications/Google Chrome.app/Contents/MacOS/Google Chrome" \
  --assets assets/birds-eye-view \
  --tmp "$(mktemp -d)"
```

The gate paths are the disabled switch, a forced missing browser, and the default missing browser.

```sh
FM_BIRDS_EYE_UI=0 bash tests/fm-birds-eye-ui.test.sh
FM_BIRDS_EYE_UI=1 FM_BIRDS_EYE_UI_CHROME=/nonexistent/chrome bash tests/fm-birds-eye-ui.test.sh
FM_BIRDS_EYE_UI_CHROME=/nonexistent/chrome bash tests/fm-birds-eye-ui.test.sh
```

The lint and classification commands are these.

```sh
bin/fm-lint.sh tests/fm-birds-eye-ui.test.sh
bin/fm-lint.sh
bin/fm-test-run.sh --list-scheduled --family unclassified
bin/fm-test-run.sh --check-coverage
bin/fm-doc-audience-check.sh
```

## Browser scenarios

The suite runs 26 scenarios in a real Chrome against a local stub server that serves the assets and the fixture responses.
The fixture clock is fixed (`NOW_EPOCH` in `tests/fixtures/birds-eye/ui/views.mjs`), so rendered state does not depend on wall time.
Assertions read rendered text, DOM state, network requests, and timings, never source bytes.

Exact output of `FM_BIRDS_EYE_UI=1 bash tests/fm-birds-eye-ui.test.sh` on 2026-10-18, exit 0.
Before the scenarios, the test also starts the harness with a browser that exits at once and requires a nonzero exit and a reported startup failure, so a browser that cannot start cannot leave the stub server holding the process open.
The `# assets` line prints the absolute path of the worktree, shown here as `<repo>`.

```text
# env platform=darwin release=25.6.0 arch=arm64 model=Mac15,6
# env cpu="Apple M3 Pro" logical_cores=11 memory_gib=36.0
# env node=v26.8.1 chrome=Chrome/155.0.8059.85 chrome_path=/Applications/Google Chrome.app/Contents/MacOS/Google Chrome
# assets <repo>/assets/birds-eye-view
ok - embedded snapshot renders with no fetches and no live indicator (no /api/ requests, live indicator absent, ages frozen)
ok - embedded page follows its embedded state, not the document mode (embedded state decides the badge, freshness, and output panel)
ok - attention list is ordered by decisions, failures, blockers, and observation problems (decision, decision, failure, blocker, blocker, observation, each naming its task)
ok - overview shows counts, unique items, and root status (counts and root status match fixture)
ok - states use the required labels and unsupported values read Unknown (working, parked, done, unknown, and unsupported values use the required labels)
ok - project groups include an explicit Unassigned group with context copies (explicit Unassigned group, project groups, context copies of supervisors)
ok - attention selection expands the path, selects the node, and opens details (path expanded, node selected, details focused)
ok - details show execution metadata and refuse unsafe links (wait reason, execution metadata, safe links, text-only strings)
ok - collapsed branches report blocked and failed descendants (alpha: 2 needing attention below (1 blocked) | alpha in search: 1 needing attention below | beta in search: 1 needing attention below (1 failed) | beta in reports: 2 needing attention below (1 blocked))
ok - search and filters keep ancestor context (ancestors kept under search, state, and project filters)
ok - list view shares selection and filters with the tree (list shares selection and search)
ok - output is text only and reports each capture status (text-only output, line count sent, per-status messages)
ok - output from a superseded generation is dropped and the generation is requested (output dropped on a new generation and the generation is sent)
ok - refresh keeps selection, search, and placement of new items (selection, search, and new child kept across refresh)
ok - vanished selection stays visible as historical with frozen durations (selection explained and kept as historical)
ok - refresh keeps the order of surviving siblings across renames and newcomers (renames and a newcomer left the existing order alone)
ok - clock tick advances ages in place and keeps ticking text out of live regions (ages advance in place, ticking text is not announced, failures still are)
ok - Enter and Space in the list select the focused row (Enter and Space select the focused list row before opening details)
ok - stale view and failed refresh are reported without losing the tree (stale threshold and failed refresh both reported, ages follow the clock)
ok - stale threshold follows the refresh interval (threshold is the greater of 30 s and three intervals)
ok - overlapping refresh requests are suppressed (three clicks, one request)
ok - auto refresh polls at the chosen interval (automatic refresh fired on the 5 s setting)
ok - keyboard-only traversal, selection, and return from output (arrow, enter, escape, output load, and back all by keyboard)
ok - narrow viewport keeps details and attention within the screen (width 360, scroll width 360)
ok - page makes no requests beyond the local origin (5 requests, all local)
# perf initial-render ms=351.1 budget=2000
# perf search median=134.5ms max=265.9ms samples=40.9,265.9,198.7,134.5,118.5 budget=500ms
# perf select median=126.1ms max=154.2ms samples=126.1,131.3,84.3,154.2,97.8 budget=500ms
# perf toggle median=92.1ms max=135.2ms samples=92.1,115.2,76.8,135.2,79.4 budget=500ms
# perf expand-all median=102.3ms max=240.5ms samples=102.3,88.6,240.5,80.7,233.1 budget=500ms
# perf collapse-all median=191.2ms max=226.7ms samples=195.9,72.3,191.2,226.7,67.6 budget=500ms
# perf deepest-visible-level=9
# perf refresh-preserves-viewport scrollTop=600->600 rowOffset=67.1->67.1
ok - 200-entity, 10-project, depth-8 fixture meets render and interaction budgets (first render 351.1 ms, every interaction under 500 ms)
# 26 of 26 scenarios passed
ok - birds-eye dashboard: 26 browser scenarios passed in /Applications/Google Chrome.app/Contents/MacOS/Google Chrome
```

## Performance on the 200-entity fixture

The fixture has 10 projects, each an 8-deep chain with 12 branches, for 200 entities.
Each interaction is measured as five samples after one warm-up, and the budget applies to the maximum sample.
Initial render has a 2000 ms budget, and every interaction has a 500 ms budget.

- Initial render on 2026-10-18: 351.1 ms in the run above.
- Search: median 134.5 ms, maximum 265.9 ms.
- Select: median 126.1 ms, maximum 154.2 ms.
- Toggle: median 92.1 ms, maximum 135.2 ms.
- Expand all: median 102.3 ms, maximum 240.5 ms.
- Collapse all: median 191.2 ms, maximum 226.7 ms.
- Deepest visible level: 9, which counts the project group row above the 8-deep chain.
- Refresh with a changed row keeps the viewport: scroll position 600 to 600, selected row offset 67.1 to 67.1.

Earlier runs on the same day, while the host reported a load average of about 22 on 11 logical cores, recorded initial renders of 248.1, 393.0, and 276.4 ms and a worst interaction sample of 267.7 ms, so the timings vary with host load.
Shared or slower hardware may exceed the budgets, and the harness prints every sample so a failure shows its margin.

## Browser pass through chrome-devtools-axi

Recorded on 2026-10-17 against the same stub-served page.

- Attention to details to output to back: selecting an attention item expanded its path, selected its node, and opened details, output loaded from the capture endpoint and rendered as text, and back returned focus to the tree.
- Keyboard: Tab from the toolbar reaches the tree, and ArrowDown, Enter, Escape, output load, and return all work without a pointer, with a visible 3 px solid focus outline.
- Narrow viewport: `chrome-devtools-axi emulate --viewport "360x740x1,mobile,touch"` measured `innerWidth` 360 and `scrollWidth` 360, so the page has no horizontal scroll.
- `chrome-devtools-axi resize 360 740` did not change `innerWidth` (measured 500), so it is not used as evidence.
- Text only: a script tag in agent output rendered as literal text and did not run.
- Network: the CDP audit records five requests, all to the local origin, and the chrome-devtools-axi pass observed only the page's own assets and the view endpoint.
- State labels: every row carries a glyph and a text label (Running, Parked, Blocked, Paused, Completed, Failed, Unknown), so state is not conveyed by colour alone.
- Desktop screenshot at 1200 by 2029 pixels was reviewed.
  The image is kept outside the repository.

## Known limits

- The mobile and grayscale screenshots were not saved, because chrome-devtools-mcp 1.11.0 did not report a saved path. The 360 px layout and colour independence rest on the scenario assertions and the text labels.
- In this bridge version, `chrome-devtools-axi click` with a snapshot uid returned STALE_REF on every attempt, because the bridge advances its generation between calls.
  Mouse-equivalent steps used `chrome-devtools-axi eval` with `.click()`, keyboard steps used real key presses, and the CDP harness drives real mouse input.
- On desktop, the attention list is capped at `min(26rem, 50vh)` with its own scroll, so the overview is about 1,204 px tall at 1200 px width and the work pane begins below it.
  The "Skip to work" link reaches the work pane directly.
- On desktop, a blank band appears under the attention list because the attention block spans two grid rows.
  This is cosmetic and known.
- The performance budgets are measured on one machine, so they are a regression signal, not a portable guarantee.
- Without `FM_BIRDS_EYE_UI=1`, a missing Chrome or Node.js prints a `skip:` line that the runner counts as a gate skip.
  Lanes that must prove the UI should set `FM_BIRDS_EYE_UI=1`.

## Gate paths

Recorded on 2026-10-17.

Disabled switch, exit 0:

```text
skip: birds-eye UI: disabled by FM_BIRDS_EYE_UI=0
```

Forced missing browser, exit 1:

```text
not ok - FM_BIRDS_EYE_UI=1 requires Chrome or Chromium (set FM_BIRDS_EYE_UI_CHROME to its path)
```

Default missing browser, exit 0:

```text
skip: birds-eye UI: Chrome or Chromium (set FM_BIRDS_EYE_UI_CHROME to its path) not found
```

## Lint and classification

Recorded on 2026-10-17.

- `bin/fm-lint.sh tests/fm-birds-eye-ui.test.sh` exited 0 with ShellCheck 0.11.0 full extended analysis enabled.
- The new test is in the `unclassified` family, which `bin/fm-test-run.sh --list-scheduled --family unclassified` lists, and that family runs serially until it is proven concurrent-safe.
- `bin/fm-lint.sh` in default changed-file mode reported no changed lint targets in this worktree state, and it checked the workflows with actionlint 1.7.12.
  The changed-file mode does not cover the new test, so the explicit-path command above is the evidence for it.
