#!/usr/bin/env node
// Browser proof for one exported bird's-eye snapshot (DPES-798: AC-13, AC-16, T10).
//
// Usage:
//   export-ui.mjs --chrome <path> --snapshot <file.html> --tmp <dir> [--chrome-arg <arg>]...
//
// Opens the exported file as a file:// page in real Chrome and runs the offline workflow:
// labels, request origin, hostile text, attention selection, search, filters, ancestor
// expansion, details, output refusal, and frozen freshness. Each scenario prints
// "ok - <name>" or "not ok - <name>: <detail>". The process exits 1 when any scenario fails.
//
// The driver starts no server and reads no file but the snapshot. The caller owns the
// export process and any network isolation around this browser; those are proven by the
// test that runs this driver, not by this file.
import { mkdtemp } from 'node:fs/promises';
import { join, resolve } from 'node:path';
import { pathToFileURL } from 'node:url';
import { launchBrowser } from '../ui/cdp.mjs';

const ENTITY_ROWS = '[role="treeitem"][data-key]';
const PRIMARY_ROW = (key) => `${ENTITY_ROWS}:not(.is-context-copy)[data-key=${JSON.stringify(key)}]`;
// Page expressions, evaluated inside the snapshot, for waits on the primary row of an entity.
const rowExists = (key) => `document.querySelector(${JSON.stringify(PRIMARY_ROW(key))}) !== null`;
const rowSelected = (key) => `document.querySelector(${JSON.stringify(PRIMARY_ROW(key))})?.getAttribute('aria-selected') === 'true'`;
const PARENT = 'home:/fleet/parent';
const PARENT_SHIP = 'home:/fleet/parent:p-ship';
const CHILD = 'home:/fleet/child';
const CHILD_SHIP = 'home:/fleet/child:c-ship-1';
const CAPTURED_AT = '2026-10-18T05:06:40Z';
const DECISION_TEXT = '</style><b>SECRET-HOLD</b>';

function parseArgs(argv) {
  const opts = { chrome: null, snapshot: null, tmp: null, chromeArgs: [] };
  for (let i = 0; i < argv.length; i += 2) {
    const flag = argv[i];
    const value = argv[i + 1];
    if (value === undefined) {
      throw new Error(`${flag} needs a value`);
    }
    if (flag === '--chrome') opts.chrome = value;
    else if (flag === '--snapshot') opts.snapshot = value;
    else if (flag === '--tmp') opts.tmp = value;
    else if (flag === '--chrome-arg') opts.chromeArgs.push(value);
    else throw new Error(`unknown argument: ${flag}`);
  }
  if (!opts.chrome || !opts.snapshot || !opts.tmp) {
    throw new Error('--chrome, --snapshot and --tmp are required');
  }
  return opts;
}

function expect(condition, message) {
  if (!condition) {
    throw new Error(message);
  }
}

function expectEqual(actual, expected, label) {
  const a = JSON.stringify(actual);
  const b = JSON.stringify(expected);
  expect(a === b, `${label}: expected ${b}, got ${a}`);
}

function expectIncludes(haystack, needle, label) {
  expect(typeof haystack === 'string' && haystack.includes(needle), `${label}: expected text to include ${JSON.stringify(needle)}, got ${JSON.stringify(haystack)}`);
}

function sleep(ms) {
  return new Promise((resolveSleep) => setTimeout(resolveSleep, ms));
}

function textOf(page, selector) {
  return page.evaluate(`(document.querySelector(${JSON.stringify(selector)})?.textContent ?? null)`);
}

function visibleKeys(page) {
  return page.evaluate(`[...document.querySelectorAll(${JSON.stringify(ENTITY_ROWS)})].map((e) => e.getAttribute('data-key'))`);
}

async function chooseOption(page, selectId, label) {
  await page.evaluate(`(() => {
    const select = document.getElementById(${JSON.stringify(selectId)});
    const wanted = ${JSON.stringify(label)};
    const option = [...select.options].find((o) => o.value === wanted || o.textContent.trim() === wanted);
    if (!option) {
      throw new Error('no option ' + wanted + ' in ' + ${JSON.stringify(selectId)});
    }
    select.value = option.value;
    select.dispatchEvent(new Event('change', { bubbles: true }));
    return true;
  })()`);
}

async function typeSearch(page, text) {
  await page.evaluate(`(() => {
    const input = document.getElementById('search');
    input.value = ${JSON.stringify(text)};
    input.dispatchEvent(new Event('input', { bubbles: true }));
    return true;
  })()`);
}

async function openSnapshot(browser, snapshotUrl) {
  const page = await browser.newPage();
  await page.setViewport({ width: 1280, height: 900, mobile: false });
  await page.navigate(snapshotUrl);
  await page.waitFor(`document.querySelectorAll('${ENTITY_ROWS}').length > 0`, { description: 'snapshot tree render' });
  return page;
}

// Each scenario gets a fresh page, so filters, search, and selection never leak between them.
const SCENARIOS = [
  {
    name: 'snapshot label, captured time, and frozen refresh controls',
    async run(page) {
      const badge = await textOf(page, '#mode-badge');
      expectIncludes(badge, 'Snapshot', 'mode badge');
      expect(!/live/i.test(badge), `mode badge must not claim live, got ${JSON.stringify(badge)}`);
      expectIncludes(await textOf(page, '#refresh-status'), `Generated ${CAPTURED_AT}`, 'captured time');
      expect(!/collection age/i.test(await textOf(page, '#refresh-status')), 'snapshot must not present a collection age');
      expectIncludes(await textOf(page, '#refresh-status'), 'no refresh', 'refresh status');
      const controls = await page.evaluate(`({
        refresh: document.getElementById('refresh-now').disabled,
        interval: document.getElementById('refresh-interval').disabled
      })`);
      expectEqual(controls, { refresh: true, interval: true }, 'refresh controls disabled');
    }
  },
  {
    name: 'page loads only its own file and carries no remote references',
    async run(page, snapshotUrl) {
      await sleep(1000);
      // A data: URI is inline content, so only references that leave the page count.
      const remote = await page.evaluate(`[...document.querySelectorAll('[src], link[href], img')]
        .map((e) => e.getAttribute('src') ?? e.getAttribute('href') ?? e.tagName)
        .filter((ref) => !ref.startsWith('data:'))`);
      expectEqual(remote, [], 'remote or external references in the page');
      const outside = page.requestUrls.filter((url) => !url.startsWith('file:'));
      expectEqual(outside, [], 'requests outside the file origin');
      expect(page.requestUrls.includes(snapshotUrl), 'the snapshot file must be the page request');
    }
  },
  {
    name: 'hostile metadata renders as inert text',
    async run(page) {
      const attention = await textOf(page, '#attention-list');
      expectIncludes(attention, DECISION_TEXT, 'hostile decision text as text');
      const executable = await page.evaluate(`[...document.querySelectorAll('img, [onerror], [onload], #attention-list b, #details-body b')].length`);
      expectEqual(executable, 0, 'elements created from hostile markup');
      const scripts = await page.evaluate(`document.querySelectorAll('script').length`);
      expectEqual(scripts, 2, 'only the two embedded blocks run');
    }
  },
  {
    name: 'attention selection expands the path, selects the node, and opens details',
    async run(page) {
      await page.clickSelector(`button.attention-item[data-key=${JSON.stringify(PARENT_SHIP)}]`);
      await page.waitFor(`${rowSelected(PARENT_SHIP)}`, {
        description: 'attention target selected'
      });
      const details = await textOf(page, '#details-body');
      expectIncludes(details, DECISION_TEXT, 'details show the decision summary as text');
    }
  },
  {
    name: 'details exclude raw worktree, pane, note, and unsafe link content',
    async run(page) {
      await page.clickSelector(PRIMARY_ROW(PARENT_SHIP));
      await page.waitFor(`document.querySelector('#details-body') !== null`, { description: 'details panel' });
      const details = await textOf(page, '#details-body');
      for (const marker of ['SECRET-WORKTREE', 'SECRET-DETAIL', 'SECRET-NOTE', 'SECRET-JS', 'javascript:', '/fleet/wt']) {
        expect(!details.includes(marker), `details must not show ${marker}`);
      }
      // The Worktree row is a label the dashboard always draws; its value must be the
      // missing-value text because the export allowlist drops artifacts.worktree.
      const worktreeRow = await page.evaluate(`(() => {
        const text = document.getElementById('details-body').innerText;
        const at = text.indexOf('Worktree');
        return at < 0 ? null : text.slice(at, at + 'Worktree\\nNot recorded'.length);
      })()`);
      expectEqual(worktreeRow, 'Worktree\nNot recorded', 'Worktree row shows the missing value, not a path');
      const links = await page.evaluate(`[...document.querySelectorAll('#details-body a')].map((a) => a.getAttribute('href'))`);
      expectEqual(links.filter((href) => href && !href.startsWith('https://')), [], 'non-https links in details');
    }
  },
  {
    name: 'search keeps the ancestors of a match',
    async run(page) {
      await typeSearch(page, 'Child ship');
      await page.waitFor(`${rowExists(CHILD_SHIP)}`, { description: 'search match' });
      const keys = await visibleKeys(page);
      expect(keys.includes(CHILD), `search must keep the ancestor ${CHILD}, got ${JSON.stringify(keys)}`);
      expect(!keys.includes(PARENT_SHIP), `search must hide ${PARENT_SHIP}, got ${JSON.stringify(keys)}`);
    }
  },
  {
    name: 'state filter keeps the ancestors of matching workers',
    async run(page) {
      await chooseOption(page, 'filter-state', 'Running');
      await page.waitFor(`${rowExists(PARENT_SHIP)}`, { description: 'state filter applied' });
      const keys = await visibleKeys(page);
      expect(keys.includes(PARENT) && keys.includes(CHILD), `state filter must keep ancestors, got ${JSON.stringify(keys)}`);
      expect(keys.includes(CHILD_SHIP) && keys.includes(PARENT_SHIP), `state filter must keep running workers, got ${JSON.stringify(keys)}`);
    }
  },
  {
    name: 'project filter keeps the Unassigned supervisor and hides other projects',
    async run(page) {
      await chooseOption(page, 'filter-project', 'Unassigned');
      await page.waitFor(`${rowExists(PARENT)}`, { description: 'project filter applied' });
      const keys = await visibleKeys(page);
      expect(keys.includes(PARENT), `Unassigned filter must keep ${PARENT}, got ${JSON.stringify(keys)}`);
      expect(!keys.includes(CHILD_SHIP), `Unassigned filter must hide ${CHILD_SHIP}, got ${JSON.stringify(keys)}`);
    }
  },
  {
    name: 'collapsed branches reveal their path from an attention item',
    async run(page) {
      await page.clickSelector('#collapse-all');
      await page.waitFor(`document.querySelectorAll('${ENTITY_ROWS}').length < 4`, { description: 'collapse applied' });
      expect(!(await visibleKeys(page)).includes(PARENT_SHIP), 'collapse-all must hide the worker below the branch');
      await page.clickSelector(`button.attention-item[data-key=${JSON.stringify(PARENT_SHIP)}]`);
      await page.waitFor(`${rowExists(PARENT_SHIP)}`, { description: 'path expanded' });
      expect((await visibleKeys(page)).includes(PARENT_SHIP), 'attention selection must reveal the worker');
    }
  },
  {
    name: 'output is refused in a snapshot and says raw output is excluded',
    async run(page) {
      await page.clickSelector(PRIMARY_ROW(CHILD_SHIP));
      await page.waitFor(`document.getElementById('output-load') !== null`, { description: 'output control' });
      expectEqual(await page.evaluate(`document.getElementById('output-load').disabled`), true, 'output capture disabled in a snapshot');
      expectIncludes(await textOf(page, '#output-status'), 'Raw output is excluded from this snapshot.', 'output explanation');
    }
  },
  {
    name: 'ages and tree do not advance after the snapshot loads',
    async run(page) {
      const before = await page.evaluate(`[document.getElementById('tree').textContent, document.getElementById('attention-list').textContent, document.getElementById('refresh-status').textContent]`);
      await sleep(2500);
      const after = await page.evaluate(`[document.getElementById('tree').textContent, document.getElementById('attention-list').textContent, document.getElementById('refresh-status').textContent]`);
      expectEqual(after, before, 'snapshot text across 2.5 seconds');
      // Visible text only: textContent would also count the embedded script source.
      const visible = await page.evaluate(`document.body.innerText`);
      expect(!/\blive\b/i.test(visible), `snapshot page must not show a live indicator in ${JSON.stringify(visible.slice(0, 200))}`);
    }
  }
];

async function main() {
  const opts = parseArgs(process.argv.slice(2));
  const snapshotUrl = pathToFileURL(resolve(opts.snapshot)).href;
  const userDataDir = await mkdtemp(join(resolve(opts.tmp), 'chrome-'));
  const browser = await launchBrowser({ chromePath: opts.chrome, userDataDir, extraArgs: opts.chromeArgs });
  let failures = 0;
  try {
    for (const scenario of SCENARIOS) {
      let page = null;
      try {
        page = await openSnapshot(browser, snapshotUrl);
        await scenario.run(page, snapshotUrl);
        console.log(`ok - ${scenario.name}`);
      } catch (error) {
        failures += 1;
        console.log(`not ok - ${scenario.name}: ${error.message}`);
      } finally {
        if (page) {
          await page.close();
        }
      }
    }
  } finally {
    await browser.close();
  }
  const total = SCENARIOS.length;
  console.log(`# ${total - failures} of ${total} snapshot scenarios passed`);
  process.exit(failures === 0 ? 0 : 1);
}

main().catch((error) => {
  console.log(`not ok - snapshot driver: ${error.message}`);
  process.exit(1);
});
