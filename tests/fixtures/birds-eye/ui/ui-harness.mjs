#!/usr/bin/env node
// Real-browser behaviour harness for the birds-eye dashboard (DPES-798).
//
// Usage:
//   ui-harness.mjs --chrome <path> --assets <dir> --tmp <dir> [--only <substring>]
//   ui-harness.mjs --serve [--port <n>] [--assets <dir>]
//
// Each scenario prints "ok - <name>" or "not ok - <name>: <detail>". The process
// exits 1 when any scenario fails. Performance lines start with "# perf".
import { execFileSync } from 'node:child_process';
import { mkdir, mkdtemp } from 'node:fs/promises';
import { cpus, release, totalmem } from 'node:os';
import { dirname, join, resolve } from 'node:path';
import { fileURLToPath } from 'node:url';
import { launchBrowser } from './cdp.mjs';
import { startStub } from './stub-server.mjs';
import { KEYS, NOW_EPOCH, mixedRefreshView, mixedView, perfView, withGeneratedEpoch } from './views.mjs';

const HERE = dirname(fileURLToPath(import.meta.url));
const DEFAULT_ASSETS = resolve(HERE, '../../../../assets/birds-eye-view');
const DESKTOP = { width: 1280, height: 900, mobile: false };
const PERF_BUDGET_MS = 500;
const FIRST_RENDER_BUDGET_MS = 2000;

// Pins the page clock so ages and staleness are deterministic.
const PIN_SCRIPT = `Date.now = () => ${NOW_EPOCH * 1000};`;

// Records when the first tree row is painted, measured from navigation start.
const TIMING_SCRIPT = `(() => {
  const observer = new MutationObserver(() => {
    if (document.querySelector('[role="treeitem"]')) {
      observer.disconnect();
      requestAnimationFrame(() => requestAnimationFrame(() => { window.__firstRenderMs = performance.now(); }));
    }
  });
  observer.observe(document, { childList: true, subtree: true });
})();`;

const EXPECTED_WORKERS = { Blocked: '2', Parked: '1', Paused: '1', Completed: '1', Failed: '1', Running: '1', Unknown: '1' };
const EXPECTED_SUPERVISORS = { 'Active child work': '2', 'No active work': '1', 'Externally held': '1' };

function parseArgs(argv) {
  const options = { chrome: null, assets: DEFAULT_ASSETS, tmp: null, serve: false, port: 0, only: null };
  for (let i = 0; i < argv.length; i += 1) {
    const arg = argv[i];
    const value = () => {
      i += 1;
      if (i >= argv.length) {
        throw new Error(`${arg} needs a value`);
      }
      return argv[i];
    };
    if (arg === '--chrome') {
      options.chrome = value();
    } else if (arg === '--assets') {
      options.assets = resolve(value());
    } else if (arg === '--tmp') {
      options.tmp = value();
    } else if (arg === '--serve') {
      options.serve = true;
    } else if (arg === '--port') {
      options.port = Number(value());
    } else if (arg === '--only') {
      options.only = value();
    } else {
      throw new Error(`unknown argument ${arg}`);
    }
  }
  return options;
}

// ---------- assertions ----------

function expect(condition, message) {
  if (!condition) {
    throw new Error(message);
  }
}

function expectEqual(actual, expected, label) {
  const a = JSON.stringify(actual);
  const b = JSON.stringify(expected);
  if (a !== b) {
    throw new Error(`${label}: expected ${b}, got ${a}`);
  }
}

function expectIncludes(haystack, needle, label) {
  expect(typeof haystack === 'string' && haystack.includes(needle), `${label}: expected text to include ${JSON.stringify(needle)}, got ${JSON.stringify(haystack)}`);
}

function expectSameSet(actual, expected, label) {
  expectEqual([...actual].sort(), [...expected].sort(), label);
}

function sortedObject(value) {
  return Object.fromEntries(Object.entries(value).sort(([a], [b]) => (a < b ? -1 : a > b ? 1 : 0)));
}

function sleep(ms) {
  return new Promise((resolveSleep) => setTimeout(resolveSleep, ms));
}

async function waitNode(predicate, timeoutMs, description) {
  const deadline = Date.now() + timeoutMs;
  while (!predicate()) {
    if (Date.now() > deadline) {
      throw new Error(`timed out waiting for ${description}`);
    }
    await sleep(50);
  }
}

// ---------- page helpers ----------

// Entity rows only: project group rows carry data-group instead of data-key.
const ENTITY_ROWS = '[role="treeitem"][data-key]';
// The primary row of an entity, shown once in the group of its own project.
const rowSelector = (key) => `${ENTITY_ROWS}:not(.is-context-copy)[data-key=${JSON.stringify(key)}]`;
// Any row for the entity, including context copies, scoped to one project group.
const rowIn = (group, key) => `${ENTITY_ROWS}[data-path^=${JSON.stringify(`group:${group}|`)}][data-key=${JSON.stringify(key)}]`;
const groupSelector = (group) => `[role="treeitem"][data-group=${JSON.stringify(group)}]`;

async function openPage(ctx, { view, viewport = DESKTOP, initScripts = [], timing = false } = {}) {
  if (view !== false) {
    ctx.stub.setView(view ?? mixedView());
  }
  const page = await ctx.browser.newPage();
  ctx.pages.push(page);
  await page.setViewport(viewport);
  for (const source of initScripts) {
    await page.addInitScript(source);
  }
  if (timing) {
    await page.addInitScript(TIMING_SCRIPT);
  }
  await page.addInitScript(PIN_SCRIPT);
  await page.navigate(ctx.stub.url);
  await page.waitFor(`document.querySelectorAll('[role="treeitem"][data-key]').length > 0`, { description: 'first tree render' });
  return page;
}

async function closeAllPages(ctx) {
  for (const page of ctx.pages.splice(0)) {
    await page.close();
  }
}

function textOf(page, selector) {
  return page.evaluate(`(document.querySelector(${JSON.stringify(selector)})?.textContent ?? null)`);
}

function keysOf(page, selector) {
  return page.evaluate(`[...document.querySelectorAll(${JSON.stringify(selector)})].map((e) => e.getAttribute('data-key'))`);
}

function pairsOf(page, listId) {
  return page.evaluate(`(() => {
    const out = {};
    let label = null;
    for (const child of document.getElementById(${JSON.stringify(listId)}).children) {
      if (child.tagName === 'DT') {
        label = child.textContent.trim();
      } else if (child.tagName === 'DD' && label !== null) {
        out[label] = child.textContent.trim();
        label = null;
      }
    }
    return out;
  })()`);
}

async function clickRow(page, key) {
  await page.waitFor(`Boolean(document.querySelector(${JSON.stringify(rowSelector(key))}))`, { description: `row ${key}` });
  await page.evaluate(`document.querySelector(${JSON.stringify(rowSelector(key))}).click()`);
}

async function clickToggle(page, selector) {
  await page.evaluate(`document.querySelector(${JSON.stringify(selector)}).querySelector('.toggle').click()`);
}

async function chooseOption(page, selectId, value) {
  await page.evaluate(`(() => {
    const select = document.getElementById(${JSON.stringify(selectId)});
    const wanted = ${JSON.stringify(value)};
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

// In-page colour measurement. A canvas resolves any CSS colour, including oklch(), to the sRGB
// value the page paints, so the ratios are computed from rendered colours, not declared tokens.
const CONTRAST_HELPERS = `
  const ctx = document.createElement('canvas').getContext('2d', { willReadFrequently: true });
  const toRgb = (css) => {
    ctx.clearRect(0, 0, 1, 1);
    ctx.fillStyle = css;
    ctx.fillRect(0, 0, 1, 1);
    const [r, g, b, a] = ctx.getImageData(0, 0, 1, 1).data;
    return { r, g, b, a: a / 255 };
  };
  const over = (fg, bg) => ({
    r: fg.a * fg.r + (1 - fg.a) * bg.r,
    g: fg.a * fg.g + (1 - fg.a) * bg.g,
    b: fg.a * fg.b + (1 - fg.a) * bg.b
  });
  const luminance = (c) => {
    const lin = (v) => { const s = v / 255; return s <= 0.03928 ? s / 12.92 : ((s + 0.055) / 1.055) ** 2.4; };
    return 0.2126 * lin(c.r) + 0.7152 * lin(c.g) + 0.0722 * lin(c.b);
  };
  const ratio = (fg, bg) => {
    const [hi, lo] = [luminance(fg), luminance(bg)].sort((p, q) => q - p);
    return (hi + 0.05) / (lo + 0.05);
  };
  // The nearest opaque background at or above the element is the surface it is read against.
  const backdrop = (el) => {
    for (let node = el; node; node = node.parentElement) {
      const bg = toRgb(getComputedStyle(node).backgroundColor);
      if (bg.a === 1) return bg;
    }
    return toRgb('#ffffff');
  };
  // The focus outline is offset outside its element (app.css: outline-offset 2px), so it paints
  // on the element's parent, not on the element's own selected fill.
  const focusBackdrop = (el) => backdrop(el.parentElement ?? el);
`;

function contrastScript(body) {
  return `(() => {${CONTRAST_HELPERS}${body}})()`;
}

async function tabUntil(page, condition, maxPresses = 20) {
  for (let i = 0; i <= maxPresses; i += 1) {
    if (await page.evaluate(`Boolean(${condition})`)) {
      return i;
    }
    if (i < maxPresses) {
      await page.press('Tab');
    }
  }
  throw new Error(`focus did not reach ${condition} within ${maxPresses} Tab presses`);
}

// Runs an action in the page and returns the time until two animation frames have
// passed, which covers the synchronous render plus the next paint.
function timeAction(page, action) {
  return page.evaluate(`(async () => {
    const settle = () => new Promise((resolve) => requestAnimationFrame(() => requestAnimationFrame(() => resolve())));
    const start = performance.now();
    ${action}
    await settle();
    return performance.now() - start;
  })()`);
}

function median(values) {
  const sorted = [...values].sort((a, b) => a - b);
  const middle = Math.floor(sorted.length / 2);
  return sorted.length % 2 ? sorted[middle] : (sorted[middle - 1] + sorted[middle]) / 2;
}

function formatMs(value) {
  return value.toFixed(1);
}

function detailOf(page, label) {
  return page.evaluate(`(() => {
    const term = [...document.querySelectorAll('#details-body dt')].find((dt) => dt.textContent.trim() === ${JSON.stringify(label)});
    return term ? term.nextElementSibling.textContent.trim() : null;
  })()`);
}

function moveClock(page, secondsAfterNow) {
  return page.evaluate(`Date.now = () => ${(NOW_EPOCH + secondsAfterNow) * 1000}`);
}

async function refreshPage(ctx, page) {
  const before = ctx.stub.state.viewRequests;
  await page.evaluate(`document.getElementById('refresh-now').click()`);
  await waitNode(() => ctx.stub.state.viewRequests > before, 4000, 'refresh request');
}

function apiRequests(ctx, fromIndex) {
  return ctx.stub.state.requests.slice(fromIndex).filter((request) => request.path.startsWith('/api/'));
}

function captureRequests(ctx) {
  return ctx.stub.state.requests
    .filter((request) => request.path === '/api/capture')
    .map((request) => new URLSearchParams(request.search));
}

// ---------- scenarios ----------

async function scenarioEmbeddedSnapshot(ctx) {
  const live = await openPage(ctx, { view: mixedView() });
  const liveKeys = await keysOf(live, ENTITY_ROWS);
  await closeAllPages(ctx);

  const snapshot = { ...mixedView(), mode: 'snapshot' };
  const before = ctx.stub.state.requests.length;
  const page = await openPage(ctx, {
    view: false,
    initScripts: [`window.BIRDS_EYE_DATA = ${JSON.stringify(snapshot)};`]
  });
  await sleep(1500);

  expectEqual(apiRequests(ctx, before).length, 0, 'API requests while embedded');

  const badge = await textOf(page, '#mode-badge');
  expectIncludes(badge, 'Snapshot', 'mode badge');
  expect(!/live/i.test(badge), `mode badge must not show a live indicator, got ${JSON.stringify(badge)}`);

  const controls = await page.evaluate(`({
    refresh: document.getElementById('refresh-now').disabled,
    interval: document.getElementById('refresh-interval').disabled
  })`);
  expectEqual(controls, { refresh: true, interval: true }, 'refresh controls disabled in snapshot');
  expect(/no refresh/i.test(await textOf(page, '#refresh-status')), 'snapshot status must say no refresh runs');

  await page.clickSelector(rowSelector(KEYS.w101));
  expectEqual(await page.evaluate(`document.getElementById('output-load').disabled`), true, 'output capture disabled in snapshot');
  expectIncludes(await textOf(page, '#output-status'), 'Raw output is excluded', 'embedded output explanation');

  expectEqual(await keysOf(page, ENTITY_ROWS), liveKeys, 'snapshot tree keys match live tree keys');

  const age = () => textOf(page, `${rowSelector(KEYS.w301)} .age`);
  const frozen = await age();
  await moveClock(page, 600);
  await sleep(1300);
  expectEqual(await age(), frozen, 'snapshot row age stays frozen as the clock moves');
  return 'no /api/ requests, live indicator absent, ages frozen';
}

async function scenarioEmbeddedLiveMode(ctx) {
  const snapshot = withGeneratedEpoch({ ...mixedView(), mode: 'live' }, NOW_EPOCH - 500);
  const before = ctx.stub.state.requests.length;
  const page = await openPage(ctx, {
    view: false,
    initScripts: [`window.BIRDS_EYE_DATA = ${JSON.stringify(snapshot)};`]
  });
  await sleep(500);
  expectEqual(apiRequests(ctx, before).length, 0, 'API requests while embedded');
  expectEqual(await textOf(page, '#mode-badge'), 'Snapshot', 'embedded page marked Snapshot whatever the document mode');
  expectEqual(await page.evaluate(`document.getElementById('stale-badge').hidden`), true, 'embedded page shows no wall-clock stale badge');
  expect(!/collection age/.test(await textOf(page, '#refresh-status')), 'embedded page shows no advancing collection age');
  await page.clickSelector(rowSelector(KEYS.w101));
  expectEqual(await page.evaluate(`document.getElementById('output-load').disabled`), true, 'output disabled in an embedded page');
  expectIncludes(await textOf(page, '#output-status'), 'Raw output is excluded', 'embedded output explanation');
  return 'embedded state decides the badge, freshness, and output panel';
}

async function scenarioAttentionOrder(ctx) {
  const page = await openPage(ctx, { view: mixedView() });
  expectEqual(
    await keysOf(page, '#attention-list .attention-item'),
    [KEYS.w102, KEYS.w105, KEYS.w103, KEYS.w303, KEYS.w101, KEYS.w302],
    'attention order'
  );
  const pills = await page.evaluate(`[...document.querySelectorAll('#attention-list .attention-item .pill')].map((e) => e.textContent.trim())`);
  expectEqual(
    pills,
    ['Needs you', 'Decision pending', 'Failure', 'Blocker', 'Blocker', 'Observation problem'],
    'attention classes, with decisions labelled by recipient'
  );
  const tasks = await page.evaluate(`[...document.querySelectorAll('#attention-list .attention-item .attention-task')].map((e) => e.textContent.trim())`);
  expectEqual(
    tasks,
    ['Merge plan', 'Search indexer', 'Deploy check', 'Feed contract', 'Payments adapter', 'Dashboard feed'],
    'attention items name the task'
  );
  const tags = await page.evaluate(`(() => {
    const tag = (key) => document.querySelector(${JSON.stringify(ENTITY_ROWS)} + ':not(.is-context-copy)[data-key=' + JSON.stringify(key) + '] .tag')?.textContent ?? null;
    return { w102: tag(${JSON.stringify(KEYS.w102)}), w105: tag(${JSON.stringify(KEYS.w105)}) };
  })()`);
  expectEqual(tags, { w102: 'Needs you', w105: 'Decision pending' }, 'decision tag on tree rows');
  await clickRow(page, KEYS.w105);
  expectIncludes(await textOf(page, '#details-body'), 'Decision pending', 'decision label in details without a recipient');
  expect(!(await textOf(page, '#details-body')).includes('Needs you'), 'details must not say Needs you without a recipient');
  return 'decision, decision, failure, blocker, blocker, observation, each naming its task';
}

async function scenarioOverviewCounts(ctx) {
  const page = await openPage(ctx, { view: mixedView() });
  expectEqual(sortedObject(await pairsOf(page, 'counts-workers')), sortedObject(EXPECTED_WORKERS), 'worker counts');
  expectEqual(sortedObject(await pairsOf(page, 'counts-supervisors')), sortedObject(EXPECTED_SUPERVISORS), 'supervisor counts');
  expectEqual(await textOf(page, '#counts-unique'), 'Unique items: 12. Context copies: 1.', 'unique and context counts');

  const rootLines = await page.evaluate(`[...document.querySelectorAll('#roots-list li')].map((li) => li.textContent)`);
  const gamma = rootLines.find((line) => line.includes('gamma'));
  expect(gamma !== undefined, `gamma root missing from observed scope: ${JSON.stringify(rootLines)}`);
  expectIncludes(gamma, 'failed', 'gamma root status');
  expectIncludes(gamma, 'stale', 'gamma root staleness');

  const states = await page.evaluate(`[...document.querySelectorAll('[role="treeitem"][data-key]')].map((row) => (row.querySelector('.state')?.textContent ?? '').trim())`);
  expect(states.length > 0 && states.every((label) => label.length > 0), 'every row shows a text state label');
  return 'counts and root status match fixture';
}

async function scenarioStateLabels(ctx) {
  const page = await openPage(ctx, { view: mixedView() });
  await page.clickSelector('#expand-all');
  const labels = await page.evaluate(`Object.fromEntries([...document.querySelectorAll(${JSON.stringify(`${ENTITY_ROWS}:not(.is-context-copy)`)})].map((row) => [row.getAttribute('data-key'), row.querySelector('.state').textContent.trim()]))`);
  expectEqual(
    sortedObject(labels),
    sortedObject({
      [KEYS.alpha]: 'Running',
      [KEYS.w101]: 'Blocked',
      [KEYS.w102]: 'Parked',
      [KEYS.w105]: 'Paused',
      [KEYS.beta]: 'Unknown',
      [KEYS.w201]: 'Completed',
      [KEYS.w103]: 'Failed',
      [KEYS.gamma]: 'Running',
      [KEYS.w301]: 'Running',
      [KEYS.w302]: 'Unknown',
      [KEYS.sub1]: 'Unknown',
      [KEYS.w303]: 'Blocked'
    }),
    'state labels by item'
  );
  const options = await page.evaluate(`[...document.getElementById('filter-state').options].map((o) => o.textContent.trim())`);
  expectEqual(options, ['All states', 'Blocked', 'Completed', 'Failed', 'Parked', 'Paused', 'Running', 'Unknown'], 'state filter labels');

  // Each state is carried by its text, so its meaning survives without colour. The text
  // itself must also clear the 4.5:1 text minimum against the surface it is painted on.
  const stateSelector = JSON.stringify(`${ENTITY_ROWS}:not(.is-context-copy) .state`);
  const stateContrast = await page.evaluate(contrastScript(
    `return [...document.querySelectorAll(${stateSelector})].map((el) => {
      const bg = backdrop(el);
      return { label: el.textContent.trim(), ratio: ratio(over(toRgb(getComputedStyle(el).color), bg), bg) };
    });`
  ));
  expect(stateContrast.length > 0, 'state label contrast: no state labels were measured');
  const weakest = stateContrast.reduce((low, item) => (item.ratio < low.ratio ? item : low));
  expect(weakest.ratio >= 4.5, `state label "${weakest.label}" contrast ${weakest.ratio.toFixed(2)}:1 is below 4.5:1 for text`);

  await chooseOption(page, 'filter-state', 'unknown');
  const unknownKeys = await keysOf(page, `${ENTITY_ROWS}:not(.is-context-copy)`);
  expectSameSet(
    unknownKeys.filter((key) => [KEYS.beta, KEYS.sub1, KEYS.w302, KEYS.w101].includes(key)),
    [KEYS.beta, KEYS.sub1, KEYS.w302],
    'unsupported and missing states filter as Unknown'
  );
  return `working, parked, done, unknown, and unsupported values use the required labels; weakest state label "${weakest.label}" ${weakest.ratio.toFixed(2)}:1 across ${stateContrast.length} rows`;
}

async function scenarioProjectGroups(ctx) {
  const page = await openPage(ctx, { view: mixedView() });
  expectEqual(
    await page.evaluate(`[...document.querySelectorAll('[role="treeitem"][data-group]')].map((row) => row.getAttribute('data-group'))`),
    ['payments', 'reports', 'search', 'Unassigned'],
    'project groups, Unassigned last'
  );
  await page.clickSelector('#expand-all');
  const unassigned = await page.evaluate(`[...document.querySelectorAll('[role="treeitem"][data-path^="group:Unassigned|"]')].map((row) => row.getAttribute('data-key') + ':' + row.classList.contains('is-context-copy'))`);
  expectSameSet(unassigned, [`${KEYS.alpha}:false`, `${KEYS.beta}:false`, `${KEYS.gamma}:false`], 'records without a project');

  const copy = await page.evaluate(`(() => {
    const row = document.querySelector(${JSON.stringify(rowIn('payments', KEYS.alpha))});
    return row ? { context: row.classList.contains('is-context-copy'), text: row.textContent } : null;
  })()`);
  expect(copy !== null, 'supervisor missing from the payments group');
  expectEqual(copy.context, true, 'supervisor copy marked as context in payments');
  expectIncludes(copy.text, 'Context copy', 'context copy tag');

  const primary = await page.evaluate(`document.querySelectorAll(${JSON.stringify(`${ENTITY_ROWS}:not(.is-context-copy)`)}).length`);
  expectEqual(primary, 12, 'primary rows are unique entities');

  expectEqual(
    await page.evaluate(`[...document.getElementById('filter-project').options].map((o) => o.textContent.trim())`),
    ['All projects', 'payments', 'reports', 'search', 'Unassigned'],
    'project filter options'
  );
  await chooseOption(page, 'filter-project', 'Unassigned');
  expectSameSet(await keysOf(page, ENTITY_ROWS), [KEYS.alpha, KEYS.beta, KEYS.gamma], 'Unassigned project filter');
  await chooseOption(page, 'filter-project', '__all__');

  const projects = await textOf(page, '#projects-list');
  expectIncludes(projects, 'Unassigned', 'overview project groups');
  expect(!projects.includes('Not recorded'), `overview shows a bare Not recorded project: ${JSON.stringify(projects)}`);
  return 'explicit Unassigned group, project groups, context copies of supervisors';
}

async function scenarioAttentionReveals(ctx) {
  const page = await openPage(ctx, { view: mixedView() });
  await page.clickSelector(`button.attention-item[data-key=${JSON.stringify(KEYS.w303)}]`);
  await page.waitFor(`document.activeElement?.id === 'details-title'`, { description: 'focus on details' });
  expectEqual(
    await page.evaluate(`document.querySelector(${JSON.stringify(rowSelector(KEYS.sub1))}).getAttribute('aria-expanded')`),
    'true',
    'sub1 expanded by attention reveal'
  );
  expectEqual(
    await page.evaluate(`document.querySelector(${JSON.stringify(rowSelector(KEYS.w303))}).getAttribute('aria-selected')`),
    'true',
    'w303 selected'
  );
  expectIncludes(await textOf(page, '#details-body'), 'Waiting for a dependency from another team', 'details for w303');
  return 'path expanded, node selected, details focused';
}

async function scenarioDetails(ctx) {
  const page = await openPage(ctx, { view: mixedView() });

  await clickRow(page, KEYS.w101);
  let details = await textOf(page, '#details-body');
  expectIncludes(details, 'Waiting for the payments API key', 'wait reason');
  expectIncludes(details, 'Requested model', 'requested model label');
  expectIncludes(details, 'model-large', 'requested model value');
  expectIncludes(details, 'Requested effort', 'requested effort label');
  expectIncludes(details, 'high', 'requested effort value');
  expectIncludes(details, 'Observed model', 'observed model label');

  await clickRow(page, KEYS.w102);
  details = await textOf(page, '#details-body');
  expectIncludes(details, 'javascript:alert(1) (not an https link)', 'unsafe PR shown as text');
  expectEqual(
    await page.evaluate(`[...document.querySelectorAll('#details-body a[href]')].filter((a) => /^javascript:/i.test(a.getAttribute('href'))).length`),
    0,
    'javascript: anchors rendered'
  );

  await clickRow(page, KEYS.w201);
  details = await textOf(page, '#details-body');
  expectIncludes(details, '<script>window.__xss=3</script>Shipped.', 'latest activity shown as literal text');
  const prLink = await page.evaluate(`(() => {
    const a = document.querySelector('#details-body a[href]');
    return a ? { href: a.getAttribute('href'), target: a.getAttribute('target'), rel: a.getAttribute('rel') } : null;
  })()`);
  expect(prLink !== null, 'https PR link rendered as an anchor');
  expectEqual(prLink.href, 'https://github.com/example/search/pull/42', 'https PR href');
  expectEqual(prLink.target, '_blank', 'PR link target');
  expectIncludes(prLink.rel, 'noopener', 'PR link rel');

  const audit = await page.evaluate(`(() => ({
    hrefs: [...document.querySelectorAll('a[href]')].map((a) => a.getAttribute('href')),
    elements: document.querySelectorAll('img, script:not([src]), iframe, object, embed').length,
    bold: [...document.querySelectorAll('b')].filter((b) => b.textContent === 'index').length,
    injected: [typeof window.__xss, typeof window.__xss1].join(',')
  }))()`);
  expectEqual(audit.elements, 0, 'elements created from fixture text');
  expectEqual(audit.bold, 0, 'fixture markup rendered as a bold element');
  expectEqual(audit.injected, 'undefined,undefined', 'injected script executed');
  expectEqual(audit.hrefs.filter((href) => !href.startsWith('https://') && !href.startsWith('#')), [], 'non-https links on the page');
  return 'wait reason, execution metadata, safe links, text-only strings';
}

async function scenarioCollapsedCounts(ctx) {
  const page = await openPage(ctx, { view: mixedView() });
  const below = async (group, key) => {
    await clickToggle(page, rowIn(group, key));
    return textOf(page, `${rowIn(group, key)} .below`);
  };
  // Each count covers the unique attention-list identities in that group's own
  // projected subtree, with the blocked and failed descendants beside it.
  const alphaPayments = await below('payments', KEYS.alpha);
  expectEqual(alphaPayments, '2 needing attention below (1 blocked)', 'alpha collapsed in payments');
  const alphaSearch = await below('search', KEYS.alpha);
  expectEqual(alphaSearch, '1 needing attention below', 'alpha collapsed in search');
  const betaSearch = await below('search', KEYS.beta);
  expectEqual(betaSearch, '1 needing attention below (1 failed)', 'beta collapsed in search');
  const betaReports = await below('reports', KEYS.beta);
  expectEqual(betaReports, '2 needing attention below (1 blocked)', 'beta collapsed in reports');
  expectEqual(await keysOf(page, `${ENTITY_ROWS}[data-path^="group:reports|"]`), [KEYS.beta], 'collapsed beta hides its reports descendants');

  await clickToggle(page, rowIn('reports', KEYS.beta));
  const shownInReports = await keysOf(page, `${ENTITY_ROWS}[data-path^="group:reports|"]`);
  expect(shownInReports.includes(KEYS.w302) && shownInReports.includes(KEYS.sub1), `expanded beta shows its counted descendants, got ${JSON.stringify(shownInReports)}`);
  return `alpha: ${alphaPayments} | alpha in search: ${alphaSearch} | beta in search: ${betaSearch} | beta in reports: ${betaReports}`;
}

async function scenarioSearchAndFilters(ctx) {
  const page = await openPage(ctx, { view: mixedView() });

  await typeSearch(page, 'payments');
  let keys = await keysOf(page, ENTITY_ROWS);
  expect(keys.includes(KEYS.w101) && keys.includes(KEYS.w102), `search lost matches: ${JSON.stringify(keys)}`);
  expect(!keys.includes(KEYS.w201), 'search kept a non-matching item');
  expect(keys.includes(KEYS.alpha), 'search dropped the ancestor of a match');
  expect(
    await page.evaluate(`document.querySelector(${JSON.stringify(rowIn('payments', KEYS.alpha))}).classList.contains('is-ancestor')`),
    'ancestor row not marked as context'
  );
  expectIncludes(await textOf(page, '#match-summary'), 'filters active', 'summary during search');

  await typeSearch(page, '');
  await chooseOption(page, 'filter-state', 'blocked');
  keys = await keysOf(page, ENTITY_ROWS);
  expect(keys.includes(KEYS.w101) && keys.includes(KEYS.w303), `state filter lost blocked items: ${JSON.stringify(keys)}`);
  expect(keys.includes(KEYS.sub1) && keys.includes(KEYS.gamma), 'state filter dropped ancestors of w303');
  expect(!keys.includes(KEYS.w102), 'state filter kept a non-blocked item');

  await chooseOption(page, 'filter-state', '__all__');
  await chooseOption(page, 'filter-project', 'search');
  keys = await keysOf(page, ENTITY_ROWS);
  expect(keys.includes(KEYS.w105) && keys.includes(KEYS.w201) && keys.includes(KEYS.w103), `project filter lost items: ${JSON.stringify(keys)}`);
  expect(keys.includes(KEYS.alpha) && keys.includes(KEYS.beta), 'project filter dropped ancestors');
  expect(!keys.includes(KEYS.w101), 'project filter kept an item from another project');
  return 'ancestors kept under search, state, and project filters';
}

async function scenarioListView(ctx) {
  const page = await openPage(ctx, { view: mixedView() });
  await clickRow(page, KEYS.w101);
  await page.clickSelector('#view-list');
  await page.waitFor(`!document.getElementById('list').hidden`, { description: 'list view visible' });
  expectEqual(await page.evaluate(`document.getElementById('view-list').getAttribute('aria-pressed')`), 'true', 'list button pressed');

  const listKeys = await keysOf(page, '#list [role="option"]');
  expectEqual(listKeys.length, 12, 'list option count (unique items)');
  expectEqual(
    await page.evaluate(`document.querySelector('#list [role="option"][data-key=${JSON.stringify(KEYS.w101)}]').getAttribute('aria-selected')`),
    'true',
    'selection shared with list'
  );

  await typeSearch(page, 'payments');
  expectSameSet(await keysOf(page, '#list [role="option"]'), [KEYS.w101, KEYS.w102], 'list filtered by search');
  return 'list shares selection and search';
}

async function scenarioOutput(ctx) {
  const page = await openPage(ctx, { view: mixedView() });
  await clickRow(page, KEYS.w101);
  const captureBefore = captureRequests(ctx).length;
  await page.clickSelector('#output-load');
  await page.waitFor(`/^Showing 4 of 40 requested lines/.test(document.getElementById('output-status').textContent)`, { description: 'w101 output status' });

  const output = await page.evaluate(`(() => {
    const pre = document.getElementById('output-text');
    return { hidden: pre.hidden, text: pre.textContent, children: pre.children.length, xss: typeof window.__xss };
  })()`);
  expectEqual(output.hidden, false, 'output text hidden after a successful capture');
  expectIncludes(output.text, '<script>window.__xss=2</script>', 'captured output text');
  expectEqual(
    output.text,
    'building payments adapter\n<script>window.__xss=2</script>\n[ERR] waiting for key: PAYMENTS_API_KEY\nhttps://example.invalid/not-linked',
    'terminal control sequences stripped from captured output'
  );
  expectEqual(output.children, 0, 'element children inside captured output');
  expectEqual(output.xss, 'undefined', 'captured output executed');
  const w101Capture = captureRequests(ctx).slice(captureBefore).find((params) => params.get('key') === KEYS.w101);
  expect(w101Capture !== undefined, 'no capture request for w101');
  expectEqual(w101Capture.get('lines'), '40', 'default line count sent');
  expectEqual(w101Capture.get('spawn_gen'), 'g1', 'selected generation sent'); 
  expectEqual(
    await page.evaluate(`[...document.getElementById('output-lines').options].map((o) => o.value)`),
    ['20', '40', '100'],
    'output line options'
  );

  await chooseOption(page, 'output-lines', '20');
  await page.clickSelector('#output-load');
  await waitNode(() => captureRequests(ctx).some((params) => params.get('lines') === '20'), 3000, 'line count 20 request');

  await page.waitFor(`!document.getElementById('output-load').disabled`, { description: 'output load finished' });
  const beforeTampered = captureRequests(ctx).length;
  await page.evaluate(`(() => {
    const select = document.getElementById('output-lines');
    select.appendChild(new Option('500', '500'));
    select.value = '500';
    return true;
  })()`);
  await page.clickSelector('#output-load');
  await waitNode(() => captureRequests(ctx).length > beforeTampered, 3000, 'oversized line count request');
  expectEqual(captureRequests(ctx).slice(beforeTampered)[0].get('lines'), '100', 'line count clamped to the maximum');

  await clickRow(page, KEYS.w201);
  await page.clickSelector('#output-load');
  await page.waitFor(`document.getElementById('output-status').textContent.includes('different run of the item')`, { description: 'generation mismatch message' });

  await clickRow(page, KEYS.w302);
  await page.clickSelector('#output-load');
  await page.waitFor(`document.getElementById('output-status').textContent.includes('identity could not be confirmed')`, { description: 'unknown identity message' });

  await clickRow(page, KEYS.w105);
  await page.clickSelector('#output-load');
  await page.waitFor(
    `document.getElementById('output-status').textContent === 'Output capture failed: capture backend timed out.'`,
    { description: 'capture error message' }
  );

  await page.clickSelector(`button.attention-item[data-key=${JSON.stringify(KEYS.w303)}]`);
  await page.clickSelector('#output-load');
  await page.waitFor(
    `document.getElementById('output-status').textContent === 'Output is unavailable: The pane is not running.'`,
    { description: 'unavailable message' }
  );

  await clickRow(page, KEYS.w103);
  await page.waitFor(
    `document.getElementById('output-status').textContent.includes('No output is recorded for this item.')`,
    { description: 'no-output message' }
  );
  expectEqual(await page.evaluate(`document.getElementById('output-load').disabled`), true, 'output load disabled for w103');
  return 'text-only output, line count sent, per-status messages';
}

async function scenarioOutputGeneration(ctx) {
  const page = await openPage(ctx, { view: mixedView() });
  await clickRow(page, KEYS.w101);
  await page.clickSelector('#output-load');
  await page.waitFor(`/^Showing/.test(document.getElementById('output-status').textContent)`, { description: 'output loaded for g1' });
  expectEqual(await page.evaluate(`document.getElementById('output-text').hidden`), false, 'output visible for the current generation');

  const respawned = mixedView();
  respawned.entities.find((item) => item.key === KEYS.w101).spawn_gen = 'g2';
  ctx.stub.setView(respawned);
  const before = ctx.stub.state.viewRequests;
  await page.evaluate(`document.getElementById('refresh-now').click()`);
  await waitNode(() => ctx.stub.state.viewRequests > before, 4000, 'refresh request');
  await page.waitFor(`document.getElementById('output-text').hidden`, { description: 'superseded output dropped' });
  expectEqual(await page.evaluate(`document.getElementById('output-text').textContent`), '', 'superseded output text cleared');
  expectEqual(await textOf(page, '#output-status'), 'Load output to see the most recent lines.', 'status after the generation changed');

  const captureBefore = captureRequests(ctx).length;
  await page.clickSelector('#output-load');
  await page.waitFor(`document.getElementById('output-status').textContent.includes('different run of the item')`, { description: 'generation mismatch message' });
  expectEqual(captureRequests(ctx).slice(captureBefore)[0].get('spawn_gen'), 'g2', 'new generation sent');
  expectEqual(await page.evaluate(`document.getElementById('output-text').hidden`), true, 'mismatched output stays hidden');
  return 'output dropped on a new generation and the generation is sent';
}

async function scenarioRefreshKeepsState(ctx) {
  const page = await openPage(ctx, { view: mixedView() });
  await clickRow(page, KEYS.w101);
  await typeSearch(page, 'payments');
  const requestsBefore = ctx.stub.state.viewRequests;

  ctx.stub.setView(mixedRefreshView());
  await page.evaluate(`document.getElementById('refresh-now').click()`);
  await waitNode(() => ctx.stub.state.viewRequests > requestsBefore, 4000, 'refresh request');
  await page.waitFor(`document.getElementById('details-body').textContent.includes('Retrying with the new key')`, { description: 'refreshed details' });

  expectEqual(
    await page.evaluate(`document.querySelector(${JSON.stringify(rowSelector(KEYS.w101))}).getAttribute('aria-selected')`),
    'true',
    'selection survives refresh'
  );
  expectEqual(await page.evaluate(`document.getElementById('search').value`), 'payments', 'search survives refresh');

  await typeSearch(page, '');
  expect((await keysOf(page, ENTITY_ROWS)).includes(KEYS.w106), 'refreshed item w106 missing from tree');
  const levels = await page.evaluate(`({
    parent: Number(document.querySelector(${JSON.stringify(rowIn('payments', KEYS.alpha))}).getAttribute('aria-level')),
    child: Number(document.querySelector(${JSON.stringify(rowSelector(KEYS.w106))}).getAttribute('aria-level'))
  })`);
  expectEqual(levels.child, levels.parent + 1, 'w106 placed under alpha');
  return 'selection, search, and new child kept across refresh';
}

async function scenarioVanishedSelection(ctx) {
  const page = await openPage(ctx, { view: mixedView() });
  await clickRow(page, KEYS.w103);
  await moveClock(page, 600);
  ctx.stub.setView(mixedRefreshView());
  const requestsBefore = ctx.stub.state.viewRequests;
  await page.evaluate(`document.getElementById('refresh-now').click()`);
  await waitNode(() => ctx.stub.state.viewRequests > requestsBefore, 4000, 'refresh request');
  await page.waitFor(`!document.getElementById('notice').hidden`, { description: 'vanished notice' });

  expectIncludes(await textOf(page, '#notice'), 'left the latest view', 'vanished notice text');
  expectIncludes(await page.evaluate(`document.querySelector('.banner')?.textContent ?? ''`), 'Historical', 'historical banner');
  expectIncludes(await textOf(page, '#details-body'), 'Deploy check failed', 'last observed details kept');
  const lastState = () => page.evaluate(`(() => {
    const term = [...document.querySelectorAll('#details-body dt')].find((dt) => dt.textContent.trim() === 'Last state');
    return term ? term.nextElementSibling.textContent.trim() : null;
  })()`);
  expectEqual(await lastState(), 'Failed', 'history last state label');
  expectEqual(await detailOf(page, 'Time in current state'), '1 h 0 m (basis: status_event)', 'historical state duration frozen at last observation');
  expectEqual(await detailOf(page, 'Elapsed'), '1 h 40 m (basis: spawn)', 'historical elapsed frozen at last observation');
  await moveClock(page, 1200);
  await sleep(1300);
  expectEqual(await detailOf(page, 'Time in current state'), '1 h 0 m (basis: status_event)', 'historical state duration after the clock moves');

  const unsupported = mixedRefreshView();
  unsupported.history.summaries[0].last_state = 'future-state';
  ctx.stub.setView(unsupported);
  const afterFirst = ctx.stub.state.viewRequests;
  await page.evaluate(`document.getElementById('refresh-now').click()`);
  await waitNode(() => ctx.stub.state.viewRequests > afterFirst, 4000, 'second refresh request');
  await page.waitFor(`[...document.querySelectorAll('#details-body dt')].some((dt) => dt.nextElementSibling.textContent.trim() === 'Unknown')`, { description: 'unsupported history state' });
  expectEqual(await lastState(), 'Unknown', 'unsupported history state label');
  return 'selection explained and kept as historical';
}

async function scenarioRefreshKeepsOrder(ctx) {
  const page = await openPage(ctx, { view: mixedView() });
  await page.clickSelector('#expand-all');
  const rowsOf = (group) => keysOf(page, `${ENTITY_ROWS}[data-path^=${JSON.stringify(`group:${group}|`)}]`);
  const unassigned = await rowsOf('Unassigned');
  const payments = await rowsOf('payments');
  expectEqual(payments, [KEYS.alpha, KEYS.w101, KEYS.w102], 'payments order before the refresh');

  // Renames would re-sort every list, the delegation order is reversed, and a
  // newcomer sorts first: surviving siblings must still keep their order.
  const next = mixedView();
  const byKey = (key) => next.entities.find((item) => item.key === key);
  byKey(KEYS.alpha).name = 'Zulu lead';
  byKey(KEYS.w101).name = 'Zzz adapter';
  byKey(KEYS.alpha).child_keys = [KEYS.w105, KEYS.w102, KEYS.w101];
  next.entities.push({ ...structuredClone(byKey(KEYS.w101)), key: KEYS.w106, name: 'Aaa newcomer', task_id: 'DPES-106' });
  ctx.stub.setView(next);
  await refreshPage(ctx, page);
  await page.waitFor(`Boolean(document.querySelector(${JSON.stringify(rowSelector(KEYS.w106))}))`, { description: 'newcomer row' });

  expectEqual(await rowsOf('Unassigned'), unassigned, 'renamed root keeps its place among Unassigned rows');
  expectEqual(await rowsOf('payments'), [...payments, KEYS.w106], 'siblings keep their order and the newcomer follows them');
  return 'renames and a newcomer left the existing order alone';
}

async function scenarioClockTick(ctx) {
  const page = await openPage(ctx, { view: mixedView() });
  await chooseOption(page, 'refresh-interval', '0');
  await clickRow(page, KEYS.w101);
  expectEqual(await detailOf(page, 'Elapsed'), '1 h 30 m (basis: spawn)', 'elapsed before the tick');
  expectEqual(await detailOf(page, 'Time in current state'), '1 h 0 m (basis: status_event)', 'state duration before the tick');
  expectEqual(await textOf(page, `${rowSelector(KEYS.w301)} .age`), 'in state 2 m', 'row age before the tick');

  await page.evaluate(`(() => {
    const row = document.querySelector(${JSON.stringify(rowSelector(KEYS.w101))});
    row.focus();
    window.__row = row;
    window.__details = document.getElementById('details-body').firstElementChild;
    window.__attention = document.querySelector('#attention-list .attention-item');
    return true;
  })()`);
  await moveClock(page, 600);
  await page.waitFor(`document.querySelector(${JSON.stringify(`${rowSelector(KEYS.w301)} .age`)}).textContent === 'in state 12 m'`, { description: 'row age advanced by the tick' });

  expectEqual(await detailOf(page, 'Elapsed'), '1 h 40 m (basis: spawn)', 'elapsed after the tick');
  expectEqual(await detailOf(page, 'Time in current state'), '1 h 10 m (basis: status_event)', 'state duration after the tick');
  expectEqual(await detailOf(page, 'Blocker age'), '1 h 10 m', 'blocker age after the tick');
  expectIncludes(await textOf(page, `button.attention-item[data-key=${JSON.stringify(KEYS.w302)}]`), 'waiting 25 m', 'attention age after the tick');
  expectIncludes(await textOf(page, '#refresh-status'), '(collection age 10 m)', 'collection age after the tick');

  const same = await page.evaluate(`({
    row: window.__row === document.querySelector(${JSON.stringify(rowSelector(KEYS.w101))}),
    focused: document.activeElement === window.__row,
    details: window.__details === document.getElementById('details-body').firstElementChild,
    attention: window.__attention === document.querySelector('#attention-list .attention-item')
  })`);
  expectEqual(same, { row: true, focused: true, details: true, attention: true }, 'tick keeps elements, focus, and details in place');

  const live = await page.evaluate(`(() => {
    const selector = '[aria-live]:not([aria-live="off"]), [role="status"], [role="alert"], [role="log"]';
    return {
      status: Boolean(document.getElementById('refresh-status').closest(selector)),
      notice: Boolean(document.getElementById('notice').closest(selector))
    };
  })()`);
  expectEqual(live, { status: false, notice: true }, 'ticking status outside live regions, notice inside one');
  ctx.stub.state.failNextView = 1;
  await refreshPage(ctx, page);
  await page.waitFor(`!document.getElementById('notice').hidden`, { description: 'refresh problem notice' });
  expectIncludes(await textOf(page, '#notice'), 'Refresh problem', 'failure announced through the notice');
  return 'ages advance in place, ticking text is not announced, failures still are';
}

async function scenarioListEnterSelects(ctx) {
  const page = await openPage(ctx, { view: mixedView() });
  await clickRow(page, KEYS.w101);
  await page.clickSelector('#view-list');
  await page.waitFor(`!document.getElementById('list').hidden`, { description: 'list view visible' });
  const option = (key) => `#list [role="option"][data-key=${JSON.stringify(key)}]`;
  const selected = () => page.evaluate(`document.querySelector('#list [aria-selected="true"]')?.getAttribute('data-key') ?? null`);

  for (const [key, press, name] of [[KEYS.w102, 'Enter', 'Merge plan'], [KEYS.w105, 'Space', 'Search indexer']]) {
    await page.evaluate(`document.querySelector(${JSON.stringify(option(key))}).focus()`);
    await page.press(press);
    await page.waitFor(`document.activeElement?.id === 'details-title'`, { description: `${press} moves focus to details` });
    expectEqual(await selected(), key, `${press} selects the focused list row`);
    expectIncludes(await textOf(page, '#details-body'), name, `details after ${press}`);
    await page.press('Escape');
    await page.waitFor(`document.activeElement?.getAttribute('data-key') === ${JSON.stringify(key)}`, { description: 'Escape returns to the selected list row' });
  }
  return 'Enter and Space select the focused list row before opening details';
}

async function scenarioStaleAndFailure(ctx) {
  const stale = await openPage(ctx, { view: withGeneratedEpoch(mixedView(), NOW_EPOCH - 500) });
  await stale.waitFor(`!document.getElementById('stale-badge').hidden`, { description: 'stale badge' });
  expectIncludes(await textOf(stale, '#stale-badge'), 'Stale', 'stale badge text');
  expectIncludes(await textOf(stale, '#refresh-status'), 'older than the stale threshold', 'stale refresh status');
  await closeAllPages(ctx);

  const page = await openPage(ctx, { view: mixedView() });
  const treeBefore = (await keysOf(page, ENTITY_ROWS)).length;
  expectEqual(await textOf(page, `${rowSelector(KEYS.w301)} .age`), 'in state 2 m', 'row age before the clock moves');
  expectIncludes(await textOf(page, `button.attention-item[data-key=${JSON.stringify(KEYS.w302)}]`), 'waiting 15 m', 'attention age before the clock moves');
  await page.evaluate(`Date.now = () => ${(NOW_EPOCH + 600) * 1000}`);
  ctx.stub.state.failNextView = 1;
  const requestsBefore = ctx.stub.state.viewRequests;
  await page.evaluate(`document.getElementById('refresh-now').click()`);
  await waitNode(() => ctx.stub.state.viewRequests > requestsBefore, 4000, 'failing refresh request');
  await page.waitFor(`!document.getElementById('notice').hidden`, { description: 'refresh problem notice' });
  expectIncludes(await textOf(page, '#notice'), 'Refresh problem', 'refresh problem notice');
  expectIncludes(await textOf(page, '#refresh-status'), 'Last refresh failed (', 'failed refresh status');
  expectEqual((await keysOf(page, ENTITY_ROWS)).length, treeBefore, 'tree kept after failed refresh');
  expectEqual(await textOf(page, `${rowSelector(KEYS.w301)} .age`), 'in state 12 m', 'row age after a failed refresh');
  expectIncludes(await textOf(page, `button.attention-item[data-key=${JSON.stringify(KEYS.w302)}]`), 'waiting 25 m', 'attention age after a failed refresh');
  return 'stale threshold and failed refresh both reported, ages follow the clock';
}

async function scenarioStaleThreshold(ctx) {
  const page = await openPage(ctx, { view: withGeneratedEpoch(mixedView(), NOW_EPOCH - 100) });
  const stale = () => page.evaluate(`!document.getElementById('stale-badge').hidden`);
  expectIncludes(await textOf(page, '#refresh-status'), '(collection age 1 m)', 'collection age shown');
  expectEqual(await stale(), true, 'stale at a 10 s interval (threshold 30 s)');
  await chooseOption(page, 'refresh-interval', '60');
  expectEqual(await stale(), false, 'fresh at a 60 s interval (threshold 180 s)');
  await chooseOption(page, 'refresh-interval', '30');
  expectEqual(await stale(), true, 'stale at a 30 s interval (threshold 90 s)');
  await chooseOption(page, 'refresh-interval', '5');
  expectEqual(await stale(), true, 'stale at a 5 s interval (threshold 30 s)');
  return 'threshold is the greater of 30 s and three intervals';
}

async function scenarioOverlapGuard(ctx) {
  const page = await openPage(ctx, { view: mixedView() });
  await chooseOption(page, 'refresh-interval', '0');
  await sleep(300);
  ctx.stub.state.viewDelayMs = 900;
  const before = ctx.stub.state.viewRequests;
  await page.evaluate(`(() => {
    const button = document.getElementById('refresh-now');
    button.click();
    button.click();
    button.click();
    return true;
  })()`);
  await sleep(1800);
  ctx.stub.state.viewDelayMs = 0;
  expectEqual(ctx.stub.state.viewRequests - before, 1, 'requests started by three rapid refreshes');
  return 'three clicks, one request';
}

async function scenarioAutoRefresh(ctx) {
  const page = await openPage(ctx, { view: mixedView() });
  const before = ctx.stub.state.viewRequests;
  await chooseOption(page, 'refresh-interval', '5');
  await waitNode(() => ctx.stub.state.viewRequests > before, 7000, 'automatic refresh');
  expectEqual(await page.evaluate(`document.getElementById('refresh-interval').value`), '5', 'interval selection');
  return 'automatic refresh fired on the 5 s setting';
}

async function scenarioKeyboard(ctx) {
  const page = await openPage(ctx, { view: mixedView() });
  await tabUntil(page, `document.activeElement?.getAttribute('role') === 'treeitem'`);
  await page.press('ArrowDown');
  await page.press('ArrowDown');
  await page.waitFor(`document.activeElement?.getAttribute('data-key') === ${JSON.stringify(KEYS.w101)}`, { description: 'ArrowDown reaches w101' });
  await page.press('Enter');
  await page.waitFor(`document.activeElement?.id === 'details-title'`, { description: 'Enter moves focus to details' });
  await page.press('Escape');
  await page.waitFor(`document.activeElement?.getAttribute('data-key') === ${JSON.stringify(KEYS.w101)}`, { description: 'Escape returns focus to tree' });

  await tabUntil(page, `document.activeElement?.id === 'output-load'`, 6);
  await page.press('Enter');
  await page.waitFor(`/^Showing/.test(document.getElementById('output-status').textContent)`, { description: 'keyboard output load' });
  await tabUntil(page, `document.activeElement?.id === 'output-back'`, 6);
  await page.press('Enter');
  await page.waitFor(`document.activeElement?.getAttribute('data-key') === ${JSON.stringify(KEYS.w101)}`, { description: 'output back returns focus to tree' });

  const outline = await page.evaluate(`(() => {
    const style = getComputedStyle(document.activeElement);
    return style.outlineStyle + ' ' + style.outlineWidth;
  })()`);
  expectEqual(outline, 'solid 3px', 'focus outline on returned row');
  expectEqual(await page.evaluate(`document.activeElement.matches(':focus-visible')`), true, 'returned row matches :focus-visible');
  const focusRatio = await page.evaluate(contrastScript(
    `const el = document.activeElement; const bg = focusBackdrop(el); return ratio(over(toRgb(getComputedStyle(el).outlineColor), bg), bg);`
  ));
  expect(focusRatio >= 3, `focus outline contrast ${focusRatio.toFixed(2)}:1 is below the 3:1 non-text minimum`);
  return `arrow, enter, escape, output load, and back all by keyboard; focus outline ${focusRatio.toFixed(2)}:1`;
}

// Measures the focus stop the keyboard is on: a solid 3px outline that matches
// :focus-visible and clears the 3:1 non-text minimum against its backdrop. Returns
// the ratio so the caller can report the weakest stop.
async function assertFocusStop(page, label) {
  const stop = await page.evaluate(contrastScript(`
    const el = document.activeElement;
    const style = getComputedStyle(el);
    const bg = focusBackdrop(el);
    return {
      id: el.id,
      key: el.getAttribute('data-key'),
      outline: style.outlineStyle + ' ' + style.outlineWidth,
      visible: el.matches(':focus-visible'),
      ratio: ratio(over(toRgb(style.outlineColor), bg), bg)
    };
  `));
  const where = stop.key ? `row ${stop.key}` : `#${stop.id}`;
  expectEqual(stop.outline, 'solid 3px', `${label}: focus outline on ${where}`);
  expectEqual(stop.visible, true, `${label}: ${where} matches :focus-visible`);
  expect(stop.ratio >= 3, `${label}: focus outline on ${where} is ${stop.ratio.toFixed(2)}:1, below 3:1`);
  return stop.ratio;
}

// Keyboard-only path for AC-15: an attention item selects its node and opens details,
// output loads from the details pane, and Back returns focus to the tree row. Every
// focus stop is checked, and the state label is read as text, not colour.
async function scenarioAttentionKeyboard(ctx, viewport = DESKTOP) {
  const page = await openPage(ctx, { view: mixedView(), viewport });
  await tabUntil(page, `document.activeElement?.matches(${JSON.stringify(`#attention-list .attention-item[data-key="${KEYS.w101}"]`)})`, 40);
  const ratios = [await assertFocusStop(page, 'attention item')];

  await page.press('Enter');
  await page.waitFor(`document.activeElement?.id === 'details-title'`, { description: 'Enter on attention item moves focus to details' });
  ratios.push(await assertFocusStop(page, 'details heading'));
  expectIncludes(await textOf(page, '#details-body'), 'Blocked', 'details state label as text');

  await tabUntil(page, `document.activeElement?.id === 'output-load'`, 12);
  ratios.push(await assertFocusStop(page, 'output load'));
  await page.press('Enter');
  await page.waitFor(`/^Showing/.test(document.getElementById('output-status').textContent)`, { description: 'keyboard output load' });

  await tabUntil(page, `document.activeElement?.id === 'output-back'`, 12);
  ratios.push(await assertFocusStop(page, 'output back'));
  await page.press('Enter');
  await page.waitFor(`document.activeElement?.getAttribute('data-key') === ${JSON.stringify(KEYS.w101)}`, { description: 'Back returns focus to the tree row' });
  ratios.push(await assertFocusStop(page, 'returned row'));
  expectEqual(
    await page.evaluate(`document.querySelector(${JSON.stringify(rowSelector(KEYS.w101))}).getAttribute('aria-selected')`),
    'true',
    'returned row stays selected'
  );
  const weakest = Math.min(...ratios);
  return `attention, details, output, and back by keyboard across ${ratios.length} focus stops; weakest focus outline ${weakest.toFixed(2)}:1; details state label as text`;
}

async function scenarioNarrow(ctx) {
  const page = await openPage(ctx, {
    view: mixedView(),
    viewport: { width: 360, height: 740, mobile: true }
  });
  await clickRow(page, KEYS.w101);
  expectIncludes(await textOf(page, '#details-body'), 'Waiting for the payments API key', 'details on narrow viewport');
  const metrics = await page.evaluate(`(() => {
    const side = document.querySelector('.side-pane').getBoundingClientRect();
    const attention = document.getElementById('attention-list').getBoundingClientRect();
    return {
      width: window.innerWidth,
      scrollWidth: document.documentElement.scrollWidth,
      sideLeft: side.left,
      sideRight: side.right,
      attentionLeft: attention.left,
      attentionRight: attention.right
    };
  })()`);
  expect(metrics.scrollWidth <= metrics.width + 1, `horizontal page scroll: ${JSON.stringify(metrics)}`);
  expect(metrics.sideLeft >= -1 && metrics.sideRight <= metrics.width + 1, `details overflow the viewport: ${JSON.stringify(metrics)}`);
  expect(metrics.attentionLeft >= -1 && metrics.attentionRight <= metrics.width + 1, `attention overflows the viewport: ${JSON.stringify(metrics)}`);
  return `width ${metrics.width}, scroll width ${metrics.scrollWidth}`;
}

async function scenarioNetworkAudit(ctx) {
  const page = await openPage(ctx, { view: mixedView() });
  await clickRow(page, KEYS.w201);
  await typeSearch(page, 'payments');
  await typeSearch(page, '');
  await chooseOption(page, 'refresh-interval', '5');
  await page.clickSelector('#refresh-now');
  await page.clickSelector('#view-list');
  await sleep(300);

  const origin = new URL(ctx.stub.url).origin;
  const outside = page.requestUrls.filter((url) => !url.startsWith(origin) && !url.startsWith('data:'));
  expectEqual(outside, [], 'requests outside the local origin');
  expectEqual(await page.evaluate(`location.href`), ctx.stub.url, 'page navigated away');
  return `${page.requestUrls.length} requests, all local`;
}

async function scenarioPerformance(ctx) {
  ctx.stub.setView(perfView());
  const page = await openPage(ctx, { view: false, timing: true });
  await page.waitFor(`typeof window.__firstRenderMs === 'number'`, { description: 'first render timing' });
  const firstRender = await page.evaluate(`window.__firstRenderMs`);
  console.log(`# perf initial-render ms=${formatMs(firstRender)} budget=${FIRST_RENDER_BUDGET_MS}`);
  expect(firstRender <= FIRST_RENDER_BUDGET_MS, `initial render ${formatMs(firstRender)} ms exceeds ${FIRST_RENDER_BUDGET_MS} ms`);
  expectIncludes(await textOf(page, '#match-summary'), '200 of 200', 'perf match summary');

  // One untimed warm-up sample, then `samples` timed samples. `actionFor(i)` gives the
  // JavaScript for sample i, so every sample changes state differently. `setup` runs
  // untimed before each sample to put the page back in a known state.
  const measure = async (name, samples, actionFor, setup = null) => {
    const values = [];
    for (let i = 0; i <= samples; i += 1) {
      if (setup) {
        await page.evaluate(`(() => { ${setup} })()`);
      }
      const ms = await timeAction(page, actionFor(i));
      if (i > 0) {
        values.push(ms);
      }
    }
    const worst = Math.max(...values);
    console.log(`# perf ${name} median=${formatMs(median(values))}ms max=${formatMs(worst)}ms samples=${values.map(formatMs).join(',')} budget=${PERF_BUDGET_MS}ms`);
    expect(worst <= PERF_BUDGET_MS, `${name} settled in ${formatMs(worst)} ms, budget ${PERF_BUDGET_MS} ms`);
  };

  const queries = ['proj-01', 'proj-02', 'proj-03', 'c5', 'b7', 'proj-04'];
  await measure('search', queries.length - 1, (i) => `
    const input = document.getElementById('search');
    input.value = ${JSON.stringify(queries[i % queries.length])};
    input.dispatchEvent(new Event('input', { bubbles: true }));
  `);
  await typeSearch(page, '');

  const selectIndices = [1, 5, 9, 13, 17, 21];
  await measure('select', selectIndices.length - 1, (i) => `
    const rows = document.querySelectorAll('[role="treeitem"][data-key]');
    rows[Math.min(${selectIndices[i % selectIndices.length]}, rows.length - 1)].click();
  `);

  await measure('toggle', 5, (i) => `
    const rows = [...document.querySelectorAll('[role="treeitem"][aria-expanded]')];
    rows[Math.min(${i * 3}, rows.length - 1)].querySelector('.toggle').click();
  `);

  await measure('expand-all', 5, () => `document.getElementById('expand-all').click();`,
    `document.getElementById('collapse-all').click();`);
  await measure('collapse-all', 5, () => `document.getElementById('collapse-all').click();`,
    `document.getElementById('expand-all').click();`);
  await page.evaluate(`document.getElementById('expand-all').click()`);

  const deepest = await page.evaluate(`Math.max(...[...document.querySelectorAll('[role="treeitem"]')].map((e) => Number(e.getAttribute('aria-level'))))`);
  console.log(`# perf deepest-visible-level=${deepest}`);
  expect(deepest >= 8, `depth-8 chain not visible after expand-all (deepest ${deepest})`);

  // A refresh must keep the viewport and the selected row where they were. The work
  // pane is brought on screen first, a row fully inside its visible area is selected,
  // and the refresh carries a real state change for that row so the re-render does work.
  await page.evaluate(`document.getElementById('work').scrollIntoView({ block: 'start' })`);
  await page.evaluate(`document.getElementById('work-scroll').scrollTop = 600`);
  const scrolled = await page.evaluate(`document.getElementById('work-scroll').scrollTop`);
  expect(scrolled >= 500, `work pane did not scroll in the perf fixture (scrollTop ${scrolled})`);
  const picked = await page.evaluate(`(() => {
    const box = document.getElementById('work-scroll').getBoundingClientRect();
    const row = [...document.querySelectorAll('[role="treeitem"][data-key]')].find((candidate) => {
      const rect = candidate.getBoundingClientRect();
      return rect.top >= box.top + 20 && rect.bottom <= box.bottom - 20;
    });
    if (!row) {
      return null;
    }
    row.click();
    return row.getAttribute('data-key');
  })()`);
  expect(picked !== null, 'no tree row is fully visible in the work pane after scrolling');
  const probe = `(() => {
    const scroll = document.getElementById('work-scroll');
    const row = document.querySelector('[role="treeitem"][aria-selected="true"]');
    return {
      scrollTop: scroll.scrollTop,
      offset: row.getBoundingClientRect().top - scroll.getBoundingClientRect().top,
      key: row.getAttribute('data-key')
    };
  })()`;
  const before = await page.evaluate(probe);
  expectEqual(before.key, picked, 'selected row before refresh');
  expect(before.offset >= 0 && before.offset < 630, `selected row not on screen before refresh: offset ${before.offset}`);
  ctx.stub.setView(perfView({ changedKey: picked, changedRaw: 'failed' }));
  const requestsBefore = ctx.stub.state.viewRequests;
  await page.evaluate(`document.getElementById('refresh-now').click()`);
  await waitNode(() => ctx.stub.state.viewRequests > requestsBefore, 4000, 'perf refresh request');
  await page.waitFor(
    `/Failed/.test(document.querySelector('[role="treeitem"][aria-selected="true"] .state')?.textContent ?? '')`,
    { description: 'refreshed state of the selected row' }
  );
  const after = await page.evaluate(probe);
  expectEqual(after.key, before.key, 'selected row after refresh');
  expect(Math.abs(after.scrollTop - before.scrollTop) <= 1, `viewport moved on refresh: ${before.scrollTop} -> ${after.scrollTop}`);
  expect(Math.abs(after.offset - before.offset) <= 1, `selected row moved within the pane on refresh: ${before.offset} -> ${after.offset}`);
  console.log(`# perf refresh-preserves-viewport scrollTop=${before.scrollTop}->${after.scrollTop} rowOffset=${before.offset.toFixed(1)}->${after.offset.toFixed(1)}`);
  return `first render ${formatMs(firstRender)} ms, every interaction under ${PERF_BUDGET_MS} ms`;
}

const SCENARIOS = [
  ['embedded snapshot renders with no fetches and no live indicator', scenarioEmbeddedSnapshot],
  ['embedded page follows its embedded state, not the document mode', scenarioEmbeddedLiveMode],
  ['attention list is ordered by decisions, failures, blockers, and observation problems', scenarioAttentionOrder],
  ['overview shows counts, unique items, and root status', scenarioOverviewCounts],
  ['states use the required labels and unsupported values read Unknown', scenarioStateLabels],
  ['project groups include an explicit Unassigned group with context copies', scenarioProjectGroups],
  ['attention selection expands the path, selects the node, and opens details', scenarioAttentionReveals],
  ['details show execution metadata and refuse unsafe links', scenarioDetails],
  ['collapsed branches report blocked and failed descendants', scenarioCollapsedCounts],
  ['search and filters keep ancestor context', scenarioSearchAndFilters],
  ['list view shares selection and filters with the tree', scenarioListView],
  ['output is text only and reports each capture status', scenarioOutput],
  ['output from a superseded generation is dropped and the generation is requested', scenarioOutputGeneration],
  ['refresh keeps selection, search, and placement of new items', scenarioRefreshKeepsState],
  ['vanished selection stays visible as historical with frozen durations', scenarioVanishedSelection],
  ['refresh keeps the order of surviving siblings across renames and newcomers', scenarioRefreshKeepsOrder],
  ['clock tick advances ages in place and keeps ticking text out of live regions', scenarioClockTick],
  ['Enter and Space in the list select the focused row', scenarioListEnterSelects],
  ['stale view and failed refresh are reported without losing the tree', scenarioStaleAndFailure],
  ['stale threshold follows the refresh interval', scenarioStaleThreshold],
  ['overlapping refresh requests are suppressed', scenarioOverlapGuard],
  ['auto refresh polls at the chosen interval', scenarioAutoRefresh],
  ['keyboard-only traversal, selection, and return from output', scenarioKeyboard],
  ['keyboard-only attention, details, output, and back with visible focus', scenarioAttentionKeyboard],
  ['keyboard-only attention, details, output, and back on a narrow viewport', (ctx) => scenarioAttentionKeyboard(ctx, { width: 360, height: 740, mobile: true })],
  ['narrow viewport keeps details and attention within the screen', scenarioNarrow],
  ['page makes no requests beyond the local origin', scenarioNetworkAudit],
  ['200-entity, 10-project, depth-8 fixture meets render and interaction budgets', scenarioPerformance]
];

function environmentLines(chromeVersion, chromePath) {
  let model = 'unavailable';
  try {
    model = execFileSync('sysctl', ['-n', 'hw.model'], { encoding: 'utf8' }).trim();
  } catch {
    model = 'unavailable';
  }
  const cores = cpus();
  return [
    `# env platform=${process.platform} release=${release()} arch=${process.arch} model=${model}`,
    `# env cpu="${cores[0]?.model ?? 'unknown'}" logical_cores=${cores.length} memory_gib=${(totalmem() / 2 ** 30).toFixed(1)}`,
    `# env node=${process.version} chrome=${chromeVersion} chrome_path=${chromePath}`
  ];
}

async function runServe(options) {
  const stub = await startStub({ assetsDir: options.assets, initialView: mixedView(), stampNow: true, port: options.port });
  console.log(`serving ${stub.url}`);
  const stop = () => {
    stub.close().finally(() => process.exit(0));
  };
  process.on('SIGINT', stop);
  process.on('SIGTERM', stop);
  await new Promise(() => {});
}

async function runScenarios(options) {
  if (!options.chrome) {
    throw new Error('--chrome is required');
  }
  if (!options.tmp) {
    throw new Error('--tmp is required');
  }
  await mkdir(options.tmp, { recursive: true });
  const profileDir = await mkdtemp(join(options.tmp, 'profile-'));
  const stub = await startStub({ assetsDir: options.assets, initialView: mixedView() });
  let browser = null;
  let failures = 0;
  let ran = 0;
  try {
    browser = await launchBrowser({ chromePath: options.chrome, userDataDir: profileDir });
    for (const line of environmentLines(browser.version, options.chrome)) {
      console.log(line);
    }
    console.log(`# assets ${options.assets}`);

    const ctx = { browser, stub, pages: [] };
    for (const [name, run] of SCENARIOS) {
      if (options.only && !name.includes(options.only)) {
        continue;
      }
      ran += 1;
      try {
        const detail = await run(ctx);
        console.log(`ok - ${name}${detail ? ` (${detail})` : ''}`);
      } catch (error) {
        failures += 1;
        console.log(`not ok - ${name}: ${error.message}`);
      } finally {
        await closeAllPages(ctx);
        stub.setView(mixedView());
        stub.state.viewDelayMs = 0;
        stub.state.failNextView = 0;
      }
    }
  } finally {
    if (browser) {
      await browser.close();
    }
    await stub.close();
  }
  if (ran === 0) {
    throw new Error(`no scenario matches ${JSON.stringify(options.only)}`);
  }
  console.log(`# ${ran - failures} of ${ran} scenarios passed`);
  return failures;
}

async function main() {
  const options = parseArgs(process.argv.slice(2));
  if (options.serve) {
    await runServe(options);
    return;
  }
  const failures = await runScenarios(options);
  process.exitCode = failures === 0 ? 0 : 1;
}

main().catch((error) => {
  console.log(`not ok - harness: ${error.stack || error.message}`);
  process.exitCode = 1;
});
