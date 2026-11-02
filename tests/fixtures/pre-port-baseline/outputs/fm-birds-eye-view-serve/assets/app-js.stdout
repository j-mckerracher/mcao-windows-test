/*
 * Bird's-eye view dashboard.
 *
 * Vanilla JavaScript with no framework, no build step, and no network use
 * beyond the same-origin view and output endpoints. Every value from the
 * service is written with textContent, and only https links become anchors.
 */
(function () {
  'use strict';

  var VIEW_SCHEMA = 'fm-birds-eye-view.v1';
  var CAPTURE_SCHEMA = 'fm-birds-eye-capture.v1';
  var VIEW_URL = 'api/view';
  var CAPTURE_URL = 'api/capture';
  var MIN_INTERVAL_S = 5;
  var DEFAULT_INTERVAL_S = 10;
  var DEFAULT_OPEN_DEPTH = 3;
  var MIN_STALE_S = 30;
  var STALE_INTERVALS = 3;
  var DEFAULT_OUTPUT_LINES = 40;
  var MAX_OUTPUT_LINES = 100;
  var UNASSIGNED = 'Unassigned';
  var CLOCK_TICK_MS = 1000;
  var ALL = null;
  var ALL_VALUE = '__all__';

  var STATE_LABELS = {
    working: { label: 'Running', glyph: '●' },
    parked: { label: 'Parked', glyph: '◆' },
    blocked: { label: 'Blocked', glyph: '■' },
    paused: { label: 'Paused', glyph: '‖' },
    done: { label: 'Completed', glyph: '✓' },
    failed: { label: 'Failed', glyph: '✕' },
    unknown: { label: 'Unknown', glyph: '?' }
  };

  var ATTENTION_CLASSES = {
    decision: { rank: 0, label: 'Decision' },
    failure: { rank: 1, label: 'Failure' },
    blocker: { rank: 2, label: 'Blocker' },
    observation: { rank: 3, label: 'Observation problem' }
  };

  var ui = {
    doc: null,
    model: null,
    embedded: false,
    error: null,
    inFlight: false,
    timer: null,
    intervalS: DEFAULT_INTERVAL_S,
    query: '',
    stateFilter: ALL,
    projectFilter: ALL,
    view: 'tree',
    open: new Map(),
    selectedKey: null,
    focusPath: null,
    historical: null,
    rows: [],
    listRows: [],
    output: null,
    outputBusy: false,
    outputSeq: 0
  };

  /* ---------- small helpers ---------- */

  function $(id) {
    return document.getElementById(id);
  }

  function has(object, key) {
    return Object.prototype.hasOwnProperty.call(object, key);
  }

  function isObject(value) {
    return value !== null && typeof value === 'object' && !Array.isArray(value);
  }

  function asArray(value) {
    return Array.isArray(value) ? value : [];
  }

  function stringOrNull(value) {
    return typeof value === 'string' && value !== '' ? value : null;
  }

  function numberOrNull(value) {
    return typeof value === 'number' && isFinite(value) ? value : null;
  }

  function compare(a, b) {
    return a < b ? -1 : (a > b ? 1 : 0);
  }

  function nowSeconds() {
    return Date.now() / 1000;
  }

  function errorText(error) {
    return error && error.message ? error.message : String(error);
  }

  function humanize(token) {
    var text = String(token).replace(/[_-]+/g, ' ').trim();
    return text.charAt(0).toUpperCase() + text.slice(1);
  }

  function el(tag, attrs, children) {
    var node = document.createElement(tag);
    if (attrs) {
      Object.keys(attrs).forEach(function (name) {
        var value = attrs[name];
        if (value === null || value === undefined || value === false) {
          return;
        }
        if (name === 'text') {
          node.textContent = String(value);
        } else {
          node.setAttribute(name, value === true ? '' : String(value));
        }
      });
    }
    (children || []).forEach(function (child) {
      if (child === null || child === undefined || child === false) {
        return;
      }
      node.appendChild(typeof child === 'string' ? document.createTextNode(child) : child);
    });
    return node;
  }

  function addField(list, label, value) {
    list.appendChild(el('dt', { text: label }));
    var dd = el('dd');
    if (value instanceof Node) {
      dd.appendChild(value);
    } else {
      dd.textContent = value === null || value === undefined ? '' : String(value);
    }
    list.appendChild(dd);
  }

  function sectionOf(title, pairs) {
    var list = el('dl', { class: 'kv-list' });
    pairs.forEach(function (pair) {
      addField(list, pair[0], pair[1]);
    });
    return el('section', { class: 'detail-section' }, [el('h3', { text: title }), list]);
  }

  /* ---------- value formatting ---------- */

  function missingText(entity) {
    return entity && entity.observation && entity.observation.quality === 'unavailable'
      ? 'Unavailable'
      : 'Not recorded';
  }

  function textOr(value, entity) {
    if (value === null || value === undefined || value === '') {
      return missingText(entity);
    }
    return String(value);
  }

  function yesNoOr(value, entity) {
    if (value === true) {
      return 'Yes';
    }
    if (value === false) {
      return 'No';
    }
    return missingText(entity);
  }

  function fmtAge(seconds) {
    var value = numberOrNull(seconds);
    if (value === null || value < 0) {
      return null;
    }
    var whole = Math.floor(value);
    if (whole < 60) {
      return whole + ' s';
    }
    if (whole < 3600) {
      return Math.floor(whole / 60) + ' m';
    }
    if (whole < 86400) {
      return Math.floor(whole / 3600) + ' h ' + Math.floor((whole % 3600) / 60) + ' m';
    }
    return Math.floor(whole / 86400) + ' d ' + Math.floor((whole % 86400) / 3600) + ' h';
  }

  // Ages in the view are measured at generation time; add the time since then so
  // a view that stopped refreshing does not understate them. Snapshots are
  // frozen, so their ages never advance.
  function fmtAged(seconds) {
    var value = numberOrNull(seconds);
    if (value === null || value < 0) {
      return null;
    }
    var generated = liveServiceAvailable() ? numberOrNull(ui.doc.generated_epoch) : null;
    return fmtAge(generated === null ? value : value + Math.max(0, nowSeconds() - generated));
  }

  // An age in its own span. A live age carries its measured seconds so the clock
  // tick can rewrite just this text; a frozen age (a historical item's last
  // observation, or a snapshot) is plain text that never changes.
  function agedSpan(className, seconds, prefix, suffix, frozen) {
    var text = frozen ? fmtAge(seconds) : fmtAged(seconds);
    if (text === null) {
      return null;
    }
    var attrs = { class: className, text: prefix + text + suffix };
    if (!frozen && liveServiceAvailable()) {
      attrs['data-age-seconds'] = seconds;
      attrs['data-age-prefix'] = prefix;
      attrs['data-age-suffix'] = suffix;
    }
    return el('span', attrs);
  }

  function tickAges() {
    var nodes = document.querySelectorAll('[data-age-seconds]');
    for (var i = 0; i < nodes.length; i += 1) {
      var text = fmtAged(Number(nodes[i].getAttribute('data-age-seconds')));
      if (text !== null) {
        setText(nodes[i], nodes[i].getAttribute('data-age-prefix') + text + nodes[i].getAttribute('data-age-suffix'));
      }
    }
  }

  function ageOr(seconds, entity, frozen) {
    return agedSpan(null, seconds, '', '', frozen) || missingText(entity);
  }

  function stampText(value, entity) {
    if (!stringOrNull(value)) {
      return missingText(entity);
    }
    var parsed = ui.embedded ? NaN : Date.parse(value);
    var age = isFinite(parsed) ? fmtAge(nowSeconds() - parsed / 1000) : null;
    return age === null ? value : value + ' (' + age + ' ago)';
  }

  function durationOr(seconds, basis, entity, frozen) {
    return agedSpan(null, seconds, '', ' (basis: ' + (stringOrNull(basis) || 'not recorded') + ')', frozen) ||
      missingText(entity);
  }

  function httpsHref(value) {
    if (typeof value !== 'string' || value === '') {
      return null;
    }
    try {
      var url = new URL(value);
      return url.protocol === 'https:' ? url.href : null;
    } catch (error) {
      return null;
    }
  }

  function linkOrText(value, entity) {
    var href = httpsHref(value);
    if (href) {
      return el('a', { href: href, target: '_blank', rel: 'noopener noreferrer', text: value });
    }
    if (stringOrNull(value)) {
      return value + ' (not an https link)';
    }
    return missingText(entity);
  }

  /* ---------- entity accessors ---------- */

  function projectOf(entity) {
    return stringOrNull(entity.project) || '';
  }

  function groupOf(entity) {
    return projectOf(entity) || UNASSIGNED;
  }

  function compareGroups(a, b) {
    return compare(a === UNASSIGNED ? 1 : 0, b === UNASSIGNED ? 1 : 0) || compare(a, b);
  }

  function stateRawOf(entity) {
    return (entity.state && stringOrNull(entity.state.raw)) || '';
  }

  function categoryOf(entity) {
    var raw = stateRawOf(entity);
    return has(STATE_LABELS, raw) ? raw : 'unknown';
  }

  function stateLabelOf(raw) {
    return STATE_LABELS[has(STATE_LABELS, raw) ? raw : 'unknown'].label;
  }

  function stateView(entity) {
    var category = categoryOf(entity);
    return {
      category: category,
      label: STATE_LABELS[category].label,
      glyph: STATE_LABELS[category].glyph
    };
  }

  function hasDecision(entity) {
    var decision = entity.decision;
    return isObject(decision) && [decision.label, decision.recipient, decision.resolver, decision.key].some(function (value) {
      return stringOrNull(value) !== null;
    });
  }

  function decisionLabel(entity) {
    if (!hasDecision(entity)) {
      return null;
    }
    return stringOrNull(entity.decision.recipient) ? 'Needs you' : 'Decision pending';
  }

  function generationOf(entity) {
    return stringOrNull(entity.spawn_gen);
  }

  function nameOf(entity) {
    return stringOrNull(entity.name) || stringOrNull(entity.task_id) || entity.key;
  }

  function classificationText(value, entity) {
    return stringOrNull(value) ? humanize(value) : missingText(entity);
  }

  function compareEntities(a, b) {
    return compare(projectOf(a), projectOf(b)) ||
      compare(stringOrNull(a.name) || '', stringOrNull(b.name) || '') ||
      compare(a.key, b.key);
  }

  /* ---------- model ---------- */

  function buildModel(doc, previous) {
    var entities = asArray(doc.entities).filter(function (entity) {
      return isObject(entity) && typeof entity.key === 'string' && entity.key !== '';
    });
    var byKey = new Map();
    entities.forEach(function (entity) {
      if (!byKey.has(entity.key)) {
        byKey.set(entity.key, entity);
      }
    });

    var sortedKeys = Array.from(byKey.keys()).sort(function (a, b) {
      return compareEntities(byKey.get(a), byKey.get(b));
    });

    var childrenOf = new Map();
    var rootCandidates = [];
    sortedKeys.forEach(function (key) {
      var parent = stringOrNull(byKey.get(key).parent_key);
      if (parent && parent !== key && byKey.has(parent)) {
        if (!childrenOf.has(parent)) {
          childrenOf.set(parent, []);
        }
        childrenOf.get(parent).push(key);
      } else {
        rootCandidates.push(key);
      }
    });

    // Survivors keep the relative order they had in the previous model, so a
    // rename or a newcomer never reorders existing identities; newcomers follow.
    var order = new Map();
    var keepOrder = function (keys) {
      if (!previous) {
        return keys;
      }
      var known = keys.filter(function (key) { return previous.order.has(key); })
        .sort(function (a, b) { return previous.order.get(a) - previous.order.get(b); });
      return known.concat(keys.filter(function (key) { return !previous.order.has(key); }));
    };

    var orderedChildren = function (key) {
      var wanted = new Set(childrenOf.get(key) || []);
      var placed = new Set();
      var ordered = [];
      asArray(byKey.get(key).child_keys).forEach(function (childKey) {
        if (wanted.has(childKey) && !placed.has(childKey)) {
          placed.add(childKey);
          ordered.push(childKey);
        }
      });
      Array.from(childrenOf.get(key) || []).forEach(function (childKey) {
        if (!placed.has(childKey)) {
          placed.add(childKey);
          ordered.push(childKey);
        }
      });
      return keepOrder(ordered);
    };

    var visited = new Set();

    var build = function (key) {
      visited.add(key);
      order.set(key, order.size);
      var node = {
        key: key,
        entity: byKey.get(key),
        children: []
      };
      orderedChildren(key).forEach(function (childKey) {
        if (!visited.has(childKey)) {
          node.children.push(build(childKey));
        }
      });
      return node;
    };

    var templateRoots = [];
    keepOrder(rootCandidates).forEach(function (key) {
      if (!visited.has(key)) {
        templateRoots.push(build(key));
      }
    });
    // Entities whose parent chain loops never reach a root; surface them as roots.
    keepOrder(sortedKeys).forEach(function (key) {
      if (!visited.has(key)) {
        templateRoots.push(build(key));
      }
    });

    // Project groups are presentation only. Each group shows the ownership
    // forest cut down to its own entities; their ancestors appear as context
    // copies, and every entity is a primary row only in its own group.
    var nodeByKey = new Map();
    var nodeByPath = new Map();
    var roots = [];
    var groupByName = new Map();
    var groupNames = new Set();
    byKey.forEach(function (entity) {
      groupNames.add(groupOf(entity));
    });

    // Identities named by the attention list, counted once per projected subtree.
    var attentionKeys = new Set();
    asArray(doc.attention).forEach(function (item) {
      if (isObject(item) && byKey.has(item.key)) {
        attentionKeys.add(item.key);
      }
    });

    var project = function (template, name, parent) {
      var own = groupOf(template.entity) === name;
      var node = {
        key: template.key,
        entity: template.entity,
        path: parent.path + '|' + template.key,
        parent: parent,
        depth: parent.depth + 1,
        group: null,
        children: [],
        context: !own,
        matches: true,
        subtreeMatches: true,
        attentionBelow: 0,
        blockedBelow: 0,
        failedBelow: 0
      };
      template.children.forEach(function (child) {
        var projected = project(child, name, node);
        if (projected) {
          var category = categoryOf(projected.entity);
          node.children.push(projected);
          node.attentionBelow += projected.attentionBelow + (attentionKeys.has(projected.key) ? 1 : 0);
          node.blockedBelow += projected.blockedBelow + (category === 'blocked' ? 1 : 0);
          node.failedBelow += projected.failedBelow + (category === 'failed' ? 1 : 0);
        }
      });
      if (!own && !node.children.length) {
        return null;
      }
      nodeByPath.set(node.path, node);
      if (!node.context) {
        nodeByKey.set(node.key, node);
      }
      return node;
    };

    Array.from(groupNames).sort(compareGroups).forEach(function (name) {
      var groupNode = {
        key: 'group:' + name,
        entity: null,
        path: 'group:' + name,
        parent: null,
        depth: 0,
        group: name,
        children: [],
        context: false,
        matches: false,
        subtreeMatches: true,
        items: 0,
        attentionBelow: 0,
        blockedBelow: 0,
        failedBelow: 0
      };
      templateRoots.forEach(function (template) {
        var projected = project(template, name, groupNode);
        if (projected) {
          groupNode.children.push(projected);
        }
      });
      groupByName.set(name, groupNode);
      nodeByPath.set(groupNode.path, groupNode);
      roots.push(groupNode);
    });
    byKey.forEach(function (entity) {
      var groupNode = groupByName.get(groupOf(entity));
      var category = categoryOf(entity);
      groupNode.items += 1;
      groupNode.attentionBelow += attentionKeys.has(entity.key) ? 1 : 0;
      groupNode.blockedBelow += category === 'blocked' ? 1 : 0;
      groupNode.failedBelow += category === 'failed' ? 1 : 0;
    });

    return {
      roots: roots,
      byKey: byKey,
      nodeByKey: nodeByKey,
      nodeByPath: nodeByPath,
      order: order
    };
  }

  /* ---------- filtering ---------- */

  function currentFilter() {
    return { query: ui.query, state: ui.stateFilter, project: ui.projectFilter };
  }

  function isFiltering() {
    return ui.query !== '' || ui.stateFilter !== ALL || ui.projectFilter !== ALL;
  }

  function matchesFilter(entity, filter) {
    if (filter.state !== ALL && categoryOf(entity) !== filter.state) {
      return false;
    }
    if (filter.project !== ALL && groupOf(entity) !== filter.project) {
      return false;
    }
    if (filter.query) {
      return [entity.name, entity.task_id, entity.project].some(function (field) {
        return typeof field === 'string' && field.toLowerCase().indexOf(filter.query) !== -1;
      });
    }
    return true;
  }

  function evaluate(model, filter) {
    var walk = function (node) {
      node.matches = node.entity !== null && !node.context && matchesFilter(node.entity, filter);
      var any = node.matches;
      node.children.forEach(function (child) {
        if (walk(child)) {
          any = true;
        }
      });
      node.subtreeMatches = any;
      return any;
    };
    model.roots.forEach(walk);
  }

  function isOpen(node) {
    if (isFiltering()) {
      return true;
    }
    var stored = ui.open.get(node.path);
    return stored === undefined ? node.depth < DEFAULT_OPEN_DEPTH : stored;
  }

  function computeTreeRows(model) {
    var rows = [];
    var filtering = isFiltering();
    var walk = function (nodes, depth) {
      var shown = filtering
        ? nodes.filter(function (node) { return node.subtreeMatches; })
        : nodes;
      shown.forEach(function (node, index) {
        var childCount = filtering
          ? node.children.filter(function (child) { return child.subtreeMatches; }).length
          : node.children.length;
        var open = childCount > 0 && isOpen(node);
        rows.push({
          node: node,
          key: node.key,
          path: node.path,
          depth: depth,
          posinset: index + 1,
          setsize: shown.length,
          hasChildren: childCount > 0,
          open: open
        });
        if (open) {
          walk(node.children, depth + 1);
        }
      });
    };
    walk(model.roots, 0);
    return rows;
  }

  function computeListRows(model) {
    var rows = [];
    var walk = function (nodes) {
      nodes.forEach(function (node) {
        if (node.group === null && !node.context && node.matches) {
          rows.push(node);
        }
        walk(node.children);
      });
    };
    walk(model.roots);
    return rows;
  }

  /* ---------- data acceptance and refresh ---------- */

  function pollAllowed() {
    return !ui.embedded && (!ui.doc || ui.doc.mode === 'live');
  }

  function liveServiceAvailable() {
    return !ui.embedded && Boolean(ui.doc) && ui.doc.mode === 'live';
  }

  function freshness(doc) {
    var generated = doc && !ui.embedded ? numberOrNull(doc.generated_epoch) : null;
    var age = generated === null ? null : nowSeconds() - generated;
    if (age === null || age < 0) {
      return { age: null, stale: false };
    }
    var threshold = Math.max(MIN_STALE_S, STALE_INTERVALS * (ui.intervalS || DEFAULT_INTERVAL_S));
    return { age: age, stale: age > threshold };
  }

  function normalizeInterval(value) {
    var parsed = parseInt(value, 10);
    if (!isFinite(parsed) || parsed <= 0) {
      return 0;
    }
    return Math.max(MIN_INTERVAL_S, parsed);
  }

  function acceptView(doc) {
    if (!isObject(doc) || doc.schema !== VIEW_SCHEMA) {
      throw new Error('the response is not a ' + VIEW_SCHEMA + ' view');
    }
    var previous = ui.model;
    var model = buildModel(doc, previous);
    ui.doc = doc;
    ui.model = model;
    ui.error = null;
    reconcileSelection(previous);
  }

  function reconcileSelection(previous) {
    var key = ui.selectedKey;
    if (key === null) {
      ui.historical = null;
      return;
    }
    if (ui.model.byKey.has(key)) {
      ui.historical = null;
      return;
    }
    if (!ui.historical || ui.historical.key !== key) {
      var last = previous ? previous.byKey.get(key) : undefined;
      ui.historical = { key: key, entity: last || null };
    }
  }

  function scheduleNext() {
    clearTimeout(ui.timer);
    ui.timer = null;
    if (ui.inFlight || !pollAllowed() || ui.intervalS === 0) {
      return;
    }
    ui.timer = setTimeout(refreshNow, ui.intervalS * 1000);
  }

  function refreshNow() {
    if (!pollAllowed() || ui.inFlight) {
      return;
    }
    clearTimeout(ui.timer);
    ui.timer = null;
    ui.inFlight = true;
    renderHeader();
    fetch(VIEW_URL, { cache: 'no-store', headers: { Accept: 'application/json' } })
      .then(function (response) {
        if (!response.ok) {
          throw new Error('HTTP ' + response.status);
        }
        return response.json();
      })
      .then(function (doc) {
        acceptView(doc);
      })
      .catch(function (error) {
        ui.error = errorText(error);
      })
      .then(function () {
        ui.inFlight = false;
        renderAll();
        scheduleNext();
      });
  }

  /* ---------- selection ---------- */

  function setSelection(key) {
    if (ui.selectedKey !== key) {
      ui.output = null;
      ui.outputBusy = false;
      ui.outputSeq += 1;
    }
    ui.selectedKey = key;
    var entity = ui.model ? ui.model.byKey.get(key) : undefined;
    ui.historical = entity ? null : { key: key, entity: null };
  }

  function currentEntity() {
    if (!ui.selectedKey || !ui.model) {
      return null;
    }
    return ui.model.byKey.get(ui.selectedKey) || null;
  }

  function rowToken(path) {
    return (ui.view === 'tree' ? 'row:' : 'opt:') + path;
  }

  function selectPath(path, options) {
    var node = ui.model ? ui.model.nodeByPath.get(path) : null;
    if (!node) {
      return;
    }
    ui.focusPath = path;
    if (node.group === null) {
      setSelection(node.key);
    }
    renderAll({ focus: options.focus ? rowToken(path) : null, scroll: options.scroll });
  }

  function revealKey(key) {
    var model = ui.model;
    var node = model ? model.nodeByKey.get(key) : null;
    if (!node) {
      return;
    }
    if (!matchesFilter(node.entity, currentFilter())) {
      ui.query = '';
      ui.stateFilter = ALL;
      ui.projectFilter = ALL;
      $('search').value = '';
    }
    for (var parent = node.parent; parent; parent = parent.parent) {
      ui.open.set(parent.path, true);
    }
    ui.focusPath = node.path;
    setSelection(key);
    renderAll({ focus: 'details', scroll: true });
  }

  function focusToken(token, scroll) {
    var nodes = document.querySelectorAll('[data-focus-token]');
    for (var i = 0; i < nodes.length; i += 1) {
      if (nodes[i].getAttribute('data-focus-token') === token) {
        nodes[i].focus({ preventScroll: !scroll });
        if (scroll && nodes[i].scrollIntoView) {
          nodes[i].scrollIntoView({ block: 'nearest' });
        }
        return true;
      }
    }
    return false;
  }

  function focusSelectedRow() {
    var rows = ui.view === 'tree' ? ui.rows : ui.listRows;
    var target = null;
    if (ui.focusPath) {
      target = rows.find(function (row) { return row.path === ui.focusPath; }) || null;
    }
    if (!target) {
      target = rows.find(function (row) { return row.key === ui.selectedKey; }) || null;
    }
    if (target && focusToken(rowToken(target.path), true)) {
      return;
    }
    if (rows.length) {
      focusToken(rowToken(rows[0].path), true);
    }
  }

  function focusFirstRow() {
    var rows = ui.view === 'tree' ? ui.rows : ui.listRows;
    if (rows.length) {
      focusToken(rowToken(rows[0].path), true);
    }
  }

  function openDetails() {
    renderAll({ focus: 'details', scroll: true });
  }

  function indexOfPath(rows, path) {
    for (var i = 0; i < rows.length; i += 1) {
      if (rows[i].path === path) {
        return i;
      }
    }
    return -1;
  }

  function tabStopPath(rows) {
    if (ui.focusPath && rows.some(function (row) { return row.path === ui.focusPath; })) {
      return ui.focusPath;
    }
    var selected = rows.find(function (row) { return row.key === ui.selectedKey; });
    if (selected) {
      return selected.path;
    }
    return rows.length ? rows[0].path : null;
  }

  function stepIndex(key, index, length) {
    if (key === 'ArrowDown') {
      return Math.min(index + 1, length - 1);
    }
    if (key === 'ArrowUp') {
      return Math.max(index - 1, 0);
    }
    if (key === 'Home') {
      return 0;
    }
    if (key === 'End') {
      return length - 1;
    }
    return -1;
  }

  function parentRowIndex(rows, index) {
    for (var i = index - 1; i >= 0; i -= 1) {
      if (rows[i].depth === rows[index].depth - 1) {
        return i;
      }
    }
    return -1;
  }

  /* ---------- rendering ---------- */

  function renderAll(options) {
    var opts = options || {};
    var active = document.activeElement;
    var activeToken = active && active.getAttribute ? active.getAttribute('data-focus-token') : null;
    var scrollBox = $('work-scroll');
    var scrollTop = scrollBox.scrollTop;
    var pageY = window.pageYOffset;
    renderHeader();
    renderOverview();
    renderToolbar();
    renderWork();
    renderDetails();
    renderOutputPanel();
    scrollBox.scrollTop = scrollTop;
    window.scrollTo(window.pageXOffset, pageY);
    var token = opts.focus || activeToken;
    if (token) {
      focusToken(token, Boolean(opts.scroll));
    }
  }

  function refreshStatusText(info) {
    var parts = [];
    if (!ui.doc) {
      parts.push(ui.error ? 'Could not load the view: ' + ui.error + '.' : 'Loading the view.');
    } else {
      var collectionAge = ui.embedded ? null : fmtAge(info.age);
      parts.push('Generated ' + textOr(ui.doc.generated_at) +
        (collectionAge === null ? '' : ' (collection age ' + collectionAge + ')') + '.');
      if (info.stale) {
        parts.push('Stale: older than the stale threshold.');
      }
      if (ui.error) {
        parts.push('Last refresh failed (' + ui.error + '); showing the last good view.');
      }
    }
    if (ui.inFlight) {
      parts.push('Refreshing.');
    }
    if (ui.embedded) {
      parts.push('Embedded view: no refresh.');
    } else if (!ui.doc || ui.doc.mode === 'live') {
      parts.push(ui.intervalS ? 'Auto refresh every ' + ui.intervalS + ' s.' : 'Auto refresh is off.');
    } else {
      parts.push('Snapshot: no refresh.');
    }
    return parts.join(' ');
  }

  // The once-per-second clock tick only writes text that really changed.
  function setText(node, text) {
    if (node.textContent !== text) {
      node.textContent = text;
    }
  }

  function renderHeader() {
    var doc = ui.doc;
    var badge = $('mode-badge');
    if (!doc) {
      setText(badge, ui.error ? 'Unavailable' : 'Loading');
      badge.className = 'mode-badge';
    } else if (!ui.embedded && doc.mode === 'live') {
      setText(badge, 'Live');
      badge.className = 'mode-badge live';
    } else {
      setText(badge, 'Snapshot');
      badge.className = 'mode-badge snapshot';
    }

    var info = freshness(doc);
    $('stale-badge').hidden = !info.stale;

    if (doc) {
      var counts = isObject(doc.counts) ? doc.counts : {};
      var rootCount = asArray(doc.roots).length;
      setText($('scope-line'), 'Observed ' + rootCount + (rootCount === 1 ? ' root' : ' roots') +
        ', ' + textOr(counts.unique_entities) + ' unique items, ' +
        textOr(counts.contextual_copies) + ' context copies.');
    } else {
      setText($('scope-line'), ui.error ? 'No view is available yet.' : 'Loading the first view.');
    }

    setText($('refresh-status'), refreshStatusText(info));

    var notice = $('notice');
    if (ui.error && doc) {
      setText(notice, 'Refresh problem: ' + ui.error + '.');
      notice.hidden = false;
    } else if (ui.historical) {
      setText(notice, 'The selected item left the latest view. Its last observed details are shown as historical.');
      notice.hidden = false;
    } else {
      setText(notice, '');
      notice.hidden = true;
    }

    var canPoll = pollAllowed();
    $('refresh-now').disabled = !canPoll || ui.inFlight;
    var intervalSelect = $('refresh-interval');
    intervalSelect.disabled = !canPoll;
    if (intervalSelect.value !== String(ui.intervalS)) {
      intervalSelect.value = String(ui.intervalS);
    }
  }

  function sortedAttention(doc) {
    return asArray(doc.attention)
      .filter(isObject)
      .map(function (item, index) {
        var rank = has(ATTENTION_CLASSES, item.class) ? ATTENTION_CLASSES[item.class].rank : 4;
        return { item: item, index: index, rank: rank };
      })
      .sort(function (a, b) { return a.rank - b.rank || a.index - b.index; })
      .map(function (entry) { return entry.item; });
  }

  function attentionProject(item, entity) {
    return stringOrNull(item.project) || (entity ? groupOf(entity) : UNASSIGNED);
  }

  function attentionEntry(item, index) {
    var key = stringOrNull(item.key);
    var known = key !== null && ui.model.byKey.has(key);
    var entity = known ? ui.model.byKey.get(key) : null;
    var classInfo = has(ATTENTION_CLASSES, item.class) ? ATTENTION_CLASSES[item.class] : null;
    var label = classInfo
      ? classInfo.label
      : (stringOrNull(item.class) ? humanize(item.class) : 'Attention');
    if (item.class === 'decision') {
      label = (entity && decisionLabel(entity)) || 'Decision pending';
    }
    var waiting = agedSpan('attention-meta', item.age_seconds, 'waiting ', '');
    var meta = [
      attentionProject(item, entity),
      stringOrNull(item.resolver) ? 'resolver ' + item.resolver : null
    ].filter(Boolean).join(' · ');
    return el('li', null, [
      el('button', {
        type: 'button',
        class: 'attention-item',
        'data-key': key || '',
        'data-focus-token': 'attn:' + index + ':' + (key || ''),
        disabled: known ? null : 'disabled'
      }, [
        el('span', { class: 'pill', text: label }),
        el('span', { class: 'attention-task', text: entity ? nameOf(entity) : (key || 'Task not recorded') }),
        el('span', { class: 'attention-reason', text: stringOrNull(item.reason) || 'Reason not recorded' }),
        el('span', { class: 'attention-meta', text: meta }),
        waiting,
        known ? null : el('span', { class: 'attention-meta', text: 'Not in the tree; cannot be selected here.' })
      ])
    ]);
  }

  function fillCounts(list, value) {
    list.replaceChildren();
    if (!isObject(value)) {
      addField(list, 'Counts', 'Not recorded');
      return;
    }
    var keys = Object.keys(value).sort(compare);
    if (!keys.length) {
      addField(list, 'Counts', 'None recorded');
      return;
    }
    keys.forEach(function (key) {
      addField(list, humanize(key), textOr(value[key]));
    });
  }

  function workerStateCounts(value) {
    if (!isObject(value)) {
      return value;
    }
    var totals = {};
    Object.keys(value).forEach(function (raw) {
      var label = stateLabelOf(raw);
      totals[label] = (totals[label] || 0) + (numberOrNull(value[raw]) || 0);
    });
    return totals;
  }

  function rootItems(doc) {
    return asArray(doc.roots).filter(isObject).map(function (root) {
      var staleText = root.stale === true ? 'stale' : (root.stale === false ? 'current' : 'Not recorded');
      return el('li', null, [
        el('strong', { text: textOr(root.root_id) }),
        el('div', { class: 'muted', text: 'Status ' + textOr(root.status) + ' · source ' + textOr(root.source) + ' · ' + staleText }),
        el('div', { class: 'muted', text: 'Home ' + textOr(root.home_path) }),
        el('div', { class: 'muted', text: 'Collected ' + textOr(root.collected_at) + ' · last good ' + textOr(root.last_good_at) }),
        stringOrNull(root.error) ? el('div', { class: 'warn', text: 'Error: ' + root.error }) : null
      ]);
    });
  }

  function unsupportedItems(doc) {
    var items = asArray(doc.unsupported).filter(isObject).map(function (entry) {
      return el('li', { text: textOr(entry.category) + ': ' + textOr(entry.reason) });
    });
    return items.length ? items : [el('li', { class: 'muted', text: 'None recorded.' })];
  }

  function projectGroupItems(doc, model) {
    var groups = new Map();
    var ensure = function (project) {
      if (!groups.has(project)) {
        groups.set(project, { project: project, items: 0, attention: 0, blocked: 0, failed: 0 });
      }
      return groups.get(project);
    };
    model.byKey.forEach(function (entity) {
      var group = ensure(groupOf(entity));
      group.items += 1;
      var category = categoryOf(entity);
      if (category === 'blocked') {
        group.blocked += 1;
      }
      if (category === 'failed') {
        group.failed += 1;
      }
    });
    asArray(doc.attention).filter(isObject).forEach(function (item) {
      var entity = stringOrNull(item.key) ? model.byKey.get(item.key) : undefined;
      ensure(attentionProject(item, entity)).attention += 1;
    });
    return Array.from(groups.values())
      .sort(function (a, b) { return compareGroups(a.project, b.project); })
      .map(function (group) {
        return el('li', null, [
          el('strong', { text: group.project }),
          el('div', {
            class: 'muted',
            text: group.items + ' items · ' + group.attention + ' need attention · ' +
              group.blocked + ' blocked · ' + group.failed + ' failed'
          })
        ]);
      });
  }

  function renderOverview() {
    var doc = ui.doc;
    var model = ui.model;
    var attentionList = $('attention-list');
    var attentionEmpty = $('attention-empty');
    if (!doc || !model) {
      attentionList.replaceChildren();
      attentionEmpty.textContent = 'Waiting for the first view.';
      attentionEmpty.hidden = false;
      $('roots-list').replaceChildren();
      $('unsupported-list').replaceChildren();
      $('projects-list').replaceChildren();
      $('history-line').textContent = '';
      fillCounts($('counts-workers'), null);
      fillCounts($('counts-supervisors'), null);
      $('counts-unique').textContent = '';
      return;
    }
    var attention = sortedAttention(doc);
    attentionList.replaceChildren.apply(attentionList, attention.map(attentionEntry));
    attentionEmpty.textContent = 'Nothing needs attention in this view.';
    attentionEmpty.hidden = attention.length > 0;

    var counts = isObject(doc.counts) ? doc.counts : {};
    fillCounts($('counts-workers'), workerStateCounts(counts.workers_by_state));
    fillCounts($('counts-supervisors'), counts.supervisors_by_classification);
    $('counts-unique').textContent = 'Unique items: ' + textOr(counts.unique_entities) +
      '. Context copies: ' + textOr(counts.contextual_copies) + '.';

    var roots = $('roots-list');
    roots.replaceChildren.apply(roots, rootItems(doc));
    var unsupported = $('unsupported-list');
    unsupported.replaceChildren.apply(unsupported, unsupportedItems(doc));

    var history = isObject(doc.history) ? doc.history : {};
    $('history-line').textContent = 'History: ' + textOr(history.retained) + ' retained, ' +
      textOr(history.evicted) + ' evicted, limit ' + textOr(history.limit) +
      (history.limit_reached === true ? ', limit reached' : '') + '. ' +
      (stringOrNull(history.restart_disclosure) || '');

    var projects = $('projects-list');
    projects.replaceChildren.apply(projects, projectGroupItems(doc, model));
  }

  function renderToolbar() {
    var model = ui.model;
    var stateOptions = [{ value: ALL_VALUE, label: 'All states' }];
    var projectOptions = [{ value: ALL_VALUE, label: 'All projects' }];
    if (model) {
      var states = new Set();
      var projects = new Set();
      model.byKey.forEach(function (entity) {
        states.add(categoryOf(entity));
        projects.add(groupOf(entity));
      });
      Array.from(states).sort(function (a, b) {
        return compare(STATE_LABELS[a].label, STATE_LABELS[b].label);
      }).forEach(function (value) {
        stateOptions.push({ value: value, label: STATE_LABELS[value].label });
      });
      Array.from(projects).sort(compareGroups).forEach(function (value) {
        projectOptions.push({ value: value, label: value });
      });
      if (ui.stateFilter !== ALL && !states.has(ui.stateFilter)) {
        stateOptions.push({ value: ui.stateFilter, label: STATE_LABELS[ui.stateFilter].label });
      }
      if (ui.projectFilter !== ALL && !projects.has(ui.projectFilter)) {
        projectOptions.push({ value: ui.projectFilter, label: ui.projectFilter });
      }
    }
    fillSelect($('filter-state'), ui.stateFilter === ALL ? ALL_VALUE : ui.stateFilter, stateOptions);
    fillSelect($('filter-project'), ui.projectFilter === ALL ? ALL_VALUE : ui.projectFilter, projectOptions);
    $('view-tree').setAttribute('aria-pressed', ui.view === 'tree' ? 'true' : 'false');
    $('view-list').setAttribute('aria-pressed', ui.view === 'list' ? 'true' : 'false');
  }

  function fillSelect(select, currentValue, options) {
    var signature = options.map(function (option) { return option.value; }).join('\u0001');
    if (select.getAttribute('data-signature') !== signature) {
      select.replaceChildren.apply(select, options.map(function (option) {
        return el('option', { value: option.value, text: option.label });
      }));
      select.setAttribute('data-signature', signature);
    }
    if (select.value !== currentValue) {
      select.value = currentValue;
    }
  }

  function belowText(row) {
    if (!row.hasChildren || row.open) {
      return null;
    }
    var kinds = [];
    if (row.node.blockedBelow) {
      kinds.push(row.node.blockedBelow + ' blocked');
    }
    if (row.node.failedBelow) {
      kinds.push(row.node.failedBelow + ' failed');
    }
    if (row.node.attentionBelow) {
      var head = row.node.attentionBelow + ' needing attention below';
      return kinds.length ? head + ' (' + kinds.join(', ') + ')' : head;
    }
    return kinds.length ? kinds.join(', ') + ' below' : null;
  }

  function groupItem(row, isTabStop) {
    var node = row.node;
    var below = belowText(row);
    var item = el('div', {
      role: 'treeitem',
      class: 'node project-group',
      'data-group': node.group,
      'data-path': row.path,
      'data-focus-token': 'row:' + row.path,
      'aria-level': row.depth + 1,
      'aria-setsize': row.setsize,
      'aria-posinset': row.posinset,
      'aria-selected': 'false',
      'aria-expanded': row.hasChildren ? (row.open ? 'true' : 'false') : null,
      tabindex: isTabStop ? '0' : '-1'
    });
    item.style.setProperty('--depth', String(row.depth));
    item.appendChild(el('div', { class: 'node-line' }, [
      el('span', { class: 'toggle', 'aria-hidden': 'true', text: row.hasChildren ? (row.open ? '▾' : '▸') : '' }),
      el('span', { class: 'name', text: node.group }),
      el('span', { class: 'meta', text: 'Project group · ' + node.items + (node.items === 1 ? ' item' : ' items') }),
      below ? el('span', { class: 'below', text: below }) : null
    ]));
    return item;
  }

  function treeItem(row, index, isTabStop) {
    var node = row.node;
    if (node.group !== null) {
      return groupItem(row, isTabStop);
    }
    var entity = node.entity;
    var view = stateView(entity);
    var classes = ['node'];
    if (node.context) {
      classes.push('is-context-copy');
    }
    if (isFiltering() && !node.matches) {
      classes.push('is-ancestor');
    }
    var item = el('div', {
      role: 'treeitem',
      class: classes.join(' '),
      'data-state': view.category,
      'data-path': row.path,
      'data-key': row.key,
      'data-focus-token': 'row:' + row.path,
      'aria-level': row.depth + 1,
      'aria-setsize': row.setsize,
      'aria-posinset': row.posinset,
      'aria-selected': row.key === ui.selectedKey && !node.context ? 'true' : 'false',
      'aria-expanded': row.hasChildren ? (row.open ? 'true' : 'false') : null,
      tabindex: isTabStop ? '0' : '-1'
    });
    item.style.setProperty('--depth', String(row.depth));

    var metaParts = [
      entity.kind ? humanize(entity.kind) : null,
      stringOrNull(entity.task_id),
      projectOf(entity) || null,
      stringOrNull(entity.role)
    ].filter(Boolean).join(' · ');

    var waitReason = (entity.state && stringOrNull(entity.state.wait_reason)) ||
      (entity.blocker && stringOrNull(entity.blocker.reason)) || null;
    var stateAge = agedSpan('age', entity.timing ? entity.timing.state_seconds : null, 'in state ', '');
    var decision = decisionLabel(entity);
    var below = belowText(row);

    item.appendChild(el('div', { class: 'node-line' }, [
      el('span', { class: 'toggle', 'aria-hidden': 'true', text: row.hasChildren ? (row.open ? '▾' : '▸') : '' }),
      el('span', { class: 'glyph', 'aria-hidden': 'true', text: view.glyph }),
      el('span', { class: 'name', text: nameOf(entity) }),
      el('span', { class: 'meta', text: metaParts }),
      el('span', { class: 'state', text: view.label }),
      stateAge,
      waitReason ? el('span', { class: 'wait', text: waitReason }) : null,
      decision ? el('span', { class: 'tag', text: decision }) : null,
      below ? el('span', { class: 'below', text: below }) : null,
      node.context ? el('span', { class: 'tag', text: 'Context copy' }) : null,
      !node.context && isFiltering() && !node.matches ? el('span', { class: 'tag', text: 'Context' }) : null
    ]));
    return item;
  }

  function listItem(node, index, isTabStop) {
    var entity = node.entity;
    var view = stateView(entity);
    var stateAge = agedSpan('age', entity.timing ? entity.timing.state_seconds : null, 'in state ', '');
    var waitReason = (entity.state && stringOrNull(entity.state.wait_reason)) ||
      (entity.blocker && stringOrNull(entity.blocker.reason)) || null;
    var parentText = node.parent.group !== null ? 'in ' + node.parent.group : 'under ' + nameOf(node.parent.entity);
    var item = el('div', {
      role: 'option',
      class: 'node list-item',
      'data-state': view.category,
      'data-path': node.path,
      'data-key': node.key,
      'data-focus-token': 'opt:' + node.path,
      'aria-selected': node.key === ui.selectedKey ? 'true' : 'false',
      tabindex: isTabStop ? '0' : '-1'
    });
    item.appendChild(el('div', { class: 'node-line' }, [
      el('span', { class: 'glyph', 'aria-hidden': 'true', text: view.glyph }),
      el('span', { class: 'name', text: nameOf(entity) }),
      el('span', { class: 'meta', text: [stringOrNull(entity.task_id), projectOf(entity) || null].filter(Boolean).join(' · ') }),
      el('span', { class: 'state', text: view.label }),
      stateAge,
      waitReason ? el('span', { class: 'wait', text: waitReason }) : null,
      el('span', { class: 'meta', text: parentText })
    ]));
    return item;
  }

  function renderWork() {
    var model = ui.model;
    var summary = $('match-summary');
    var treeEl = $('tree');
    var listEl = $('list');
    treeEl.hidden = ui.view !== 'tree';
    listEl.hidden = ui.view !== 'list';
    if (!model) {
      summary.textContent = 'Waiting for the first view.';
      treeEl.replaceChildren();
      listEl.replaceChildren();
      ui.rows = [];
      ui.listRows = [];
      return;
    }
    var filter = currentFilter();
    evaluate(model, filter);
    var shown = 0;
    model.byKey.forEach(function (entity) {
      if (matchesFilter(entity, filter)) {
        shown += 1;
      }
    });
    summary.textContent = shown + ' of ' + model.byKey.size + ' items shown' +
      (isFiltering() ? ', filters active' : '') + '.';

    if (ui.view === 'tree') {
      ui.rows = computeTreeRows(model);
      ui.listRows = [];
      var stop = tabStopPath(ui.rows);
      treeEl.replaceChildren.apply(treeEl, ui.rows.map(function (row, index) {
        return treeItem(row, index, row.path === stop);
      }));
      listEl.replaceChildren();
    } else {
      ui.listRows = computeListRows(model);
      ui.rows = [];
      var stopList = tabStopPath(ui.listRows);
      listEl.replaceChildren.apply(listEl, ui.listRows.map(function (node, index) {
        return listItem(node, index, node.path === stopList);
      }));
      treeEl.replaceChildren();
    }
  }

  function renderDetails() {
    var body = $('details-body');
    var key = ui.selectedKey;
    if (key === null) {
      body.replaceChildren(el('p', {
        class: 'muted',
        text: 'Select an item in the tree or list, or open one from Needs attention.'
      }));
      return;
    }
    var live = ui.model ? ui.model.byKey.get(key) : undefined;
    var historical = !live;
    var entity = live || (ui.historical && ui.historical.entity) || null;
    if (!entity) {
      body.replaceChildren(el('p', {
        text: 'The selected item is no longer in the latest view, and no last observed details were kept.'
      }));
      return;
    }
    var nodes = [];
    if (historical) {
      nodes.push(el('p', {
        class: 'banner',
        role: 'status',
        text: 'Historical: this item is no longer in the latest view. The values below are its last observed details.'
      }));
    }
    nodes.push(el('p', { class: 'detail-title', text: nameOf(entity) }));
    nodes.push(identitySection(entity));
    nodes.push(stateSection(entity));
    nodes.push(timingSection(entity, historical));
    nodes.push(observationSection(entity));
    nodes.push(decisionSection(entity, historical));
    nodes.push(executionSection(entity));
    nodes.push(artifactSection(entity));
    nodes.push(relationSection(entity));
    nodes.push(activitySection(entity));
    if (historical) {
      nodes.push(historySection(key));
    }
    body.replaceChildren.apply(body, nodes.filter(Boolean));
  }

  function identitySection(entity) {
    return sectionOf('Identity', [
      ['Responsibility', textOr(entity.role, entity)],
      ['Kind', entity.kind ? humanize(entity.kind) : missingText(entity)],
      ['Task ID', textOr(entity.task_id, entity)],
      ['Canonical identity', textOr(entity.key, entity)],
      ['Owning home', textOr(entity.home_id, entity)],
      ['Project', groupOf(entity)],
      ['Generation', textOr(entity.spawn_gen, entity)]
    ]);
  }

  function stateSection(entity) {
    var st = isObject(entity.state) ? entity.state : {};
    return sectionOf('State', [
      ['State', stateView(entity).label],
      ['Raw state', textOr(st.raw, entity)],
      ['Display', textOr(st.display, entity)],
      ['Source', textOr(st.source, entity)],
      ['Detail', textOr(st.detail, entity)],
      ['Wait reason', textOr(st.wait_reason, entity)],
      ['Observed at', textOr(st.observed_at, entity)],
      ['Classification', classificationText(entity.classification, entity)]
    ]);
  }

  function timingSection(entity, frozen) {
    var timing = isObject(entity.timing) ? entity.timing : {};
    return sectionOf('Timing', [
      ['Elapsed', durationOr(timing.elapsed_seconds, timing.elapsed_basis, entity, frozen)],
      ['Time in current state', durationOr(timing.state_seconds, timing.state_basis, entity, frozen)]
    ]);
  }

  function observationSection(entity) {
    var observation = isObject(entity.observation) ? entity.observation : {};
    return sectionOf('Observation', [
      ['Quality', textOr(observation.quality, entity)],
      ['Collected', stampText(observation.collected_at, entity)],
      ['Source event', stampText(observation.source_event_at, entity)],
      ['Root', textOr(observation.root_id, entity)]
    ]);
  }

  function decisionSection(entity, frozen) {
    var decision = isObject(entity.decision) ? entity.decision : {};
    var blocker = isObject(entity.blocker) ? entity.blocker : {};
    return sectionOf('Decision and blocker', [
      ['Decision', decisionLabel(entity) || missingText(entity)],
      ['Recipient', textOr(decision.recipient, entity)],
      ['Resolver', textOr(decision.resolver, entity)],
      ['Decision key', textOr(decision.key, entity)],
      ['Decision age', ageOr(decision.age_seconds, entity, frozen)],
      ['Blocker reason', textOr(blocker.reason, entity)],
      ['Blocker age', ageOr(blocker.age_seconds, entity, frozen)]
    ]);
  }

  function executionSection(entity) {
    var execution = isObject(entity.execution) ? entity.execution : {};
    return sectionOf('Execution', [
      ['Requested model', textOr(execution.requested_model, entity)],
      ['Requested effort', textOr(execution.requested_effort, entity)],
      ['Observed model', textOr(execution.observed_model, entity)],
      ['Observed effort', textOr(execution.observed_effort, entity)],
      ['Harness', textOr(execution.harness, entity)],
      ['Playbook', textOr(execution.playbook, entity)],
      ['Workflow stage', textOr(execution.workflow_stage, entity)]
    ]);
  }

  function artifactSection(entity) {
    var artifacts = isObject(entity.artifacts) ? entity.artifacts : {};
    return sectionOf('Artifacts', [
      ['Worktree', textOr(artifacts.worktree, entity)],
      ['Branch', textOr(artifacts.branch, entity)],
      ['Pull request', linkOrText(artifacts.pr_url, entity)],
      ['Report path', textOr(artifacts.report_path, entity)],
      ['Output available', yesNoOr(artifacts.output_available, entity)]
    ]);
  }

  function namesFor(keys) {
    return keys.map(function (key) {
      var related = ui.model ? ui.model.byKey.get(key) : undefined;
      return related ? nameOf(related) + ' (' + key + ')' : String(key);
    }).join('; ');
  }

  function relationSection(entity) {
    var parentKey = stringOrNull(entity.parent_key);
    var parentText = parentKey ? namesFor([parentKey]) : missingText(entity);
    var childKeys = Array.isArray(entity.child_keys) ? entity.child_keys : null;
    var childText = childKeys === null
      ? missingText(entity)
      : (childKeys.length ? namesFor(childKeys) : 'None recorded');
    var contextKeys = Array.isArray(entity.contextual_in) ? entity.contextual_in : null;
    var contextText = contextKeys === null
      ? missingText(entity)
      : (contextKeys.length ? namesFor(contextKeys) : 'None recorded');
    var prerequisites = Array.isArray(entity.prerequisites)
      ? entity.prerequisites.filter(isObject)
      : null;
    var prerequisiteText = prerequisites === null
      ? missingText(entity)
      : (prerequisites.length
        ? prerequisites.map(function (p) { return textOr(p.key, entity) + ' (' + textOr(p.source, entity) + ')'; }).join('; ')
        : 'None recorded');
    return sectionOf('Relations', [
      ['Parent', parentText],
      ['Children', childText],
      ['Also shown under', contextText],
      ['Prerequisites', prerequisiteText]
    ]);
  }

  function activitySection(entity) {
    var activity = isObject(entity.latest_activity) ? entity.latest_activity : {};
    return sectionOf('Latest activity', [
      ['Text', textOr(activity.text, entity)],
      ['Reported by agent', yesNoOr(activity.reported_by_agent, entity)],
      ['Activity at', textOr(activity.at, entity)]
    ]);
  }

  function historySection(key) {
    var history = ui.doc && isObject(ui.doc.history) ? ui.doc.history : {};
    var found = asArray(history.summaries).filter(isObject).find(function (summary) {
      return summary.key === key;
    });
    if (!found) {
      return null;
    }
    return sectionOf('History', [
      ['Last state', stateLabelOf(found.last_state)],
      ['Last seen', textOr(found.last_seen_at)],
      ['Generation', textOr(found.generation)],
      ['Historical', found.historical === true ? 'Yes' : 'No']
    ]);
  }

  function renderOutputPanel() {
    var key = ui.selectedKey;
    var entity = currentEntity();
    var button = $('output-load');
    var pre = $('output-text');
    var status = $('output-status');
    var result = ui.output && entity && ui.output.key === key && ui.output.gen === generationOf(entity)
      ? ui.output
      : null;

    var blocked = null;
    if (key === null) {
      blocked = 'Select an item to see its output.';
    } else if (!entity) {
      blocked = 'This item is no longer in the latest view, so its output cannot be loaded.';
    } else if (!liveServiceAvailable()) {
      blocked = ui.embedded
        ? 'Raw output is excluded from this snapshot.'
        : 'Output is not part of this snapshot.';
    } else if (entity.artifacts && entity.artifacts.output_available === false) {
      blocked = 'No output is recorded for this item.';
    }

    var message = blocked || (result && result.message) || 'Load output to see the most recent lines.';
    if (!blocked && ui.outputBusy) {
      message = 'Loading output.';
    }
    button.disabled = Boolean(blocked) || ui.outputBusy;

    var showText = !blocked && result !== null && typeof result.text === 'string';
    pre.hidden = !showText;
    pre.textContent = showText ? result.text : '';
    status.textContent = message;
  }

  /* ---------- output ---------- */

  var TERMINAL_SEQUENCES = new RegExp([
    '\\u001b\\][^\\u0007\\u001b]*(?:\\u0007|\\u001b\\\\)?',
    '\\u001b[P^_X][^\\u001b]*(?:\\u001b\\\\)?',
    '\\u001b\\[[0-?]*[ -/]*[@-~]',
    '\\u001b[@-Z\\\\-_]'
  ].join('|'), 'g');
  var TERMINAL_CONTROLS = /[\u0000-\u0008\u000b-\u001f\u007f-\u009f]/g;

  function stripControl(text) {
    return text.replace(/\r\n/g, '\n').replace(TERMINAL_SEQUENCES, '').replace(TERMINAL_CONTROLS, '');
  }

  // Ends a reason with a full stop unless it already ends with terminal punctuation.
  function sentence(text) {
    return /[.!?]$/.test(text) ? text : text + '.';
  }

  function interpretCapture(capture, key, entity) {
    if (!isObject(capture) || capture.schema !== CAPTURE_SCHEMA) {
      return { text: null, message: 'The output response was not recognized, so nothing is shown.' };
    }
    if (capture.key !== key) {
      return { text: null, message: 'The output response was for a different item, so nothing is shown.' };
    }
    var requested = capture.spawn_gen_requested;
    var current = capture.spawn_gen_current;
    var generationMismatch = capture.status === 'generation_mismatch' ||
      requested !== current ||
      (entity.spawn_gen !== null && entity.spawn_gen !== undefined &&
        (requested !== entity.spawn_gen || current !== entity.spawn_gen));
    if (generationMismatch) {
      return { text: null, message: 'This output belongs to a different run of the item, so it is not shown.' };
    }
    if (capture.status === 'unknown_identity') {
      return { text: null, message: 'The item identity could not be confirmed, so no output is shown.' };
    }
    if (capture.status === 'unavailable') {
      return { text: null, message: 'Output is unavailable: ' + sentence(stringOrNull(capture.reason) || 'no reason recorded') };
    }
    if (capture.status === 'error') {
      return { text: null, message: 'Output capture failed: ' + sentence(stringOrNull(capture.reason) || 'no reason recorded') };
    }
    if (capture.status !== 'ok' || typeof capture.text !== 'string') {
      return { text: null, message: 'The output response had no readable text.' };
    }
    var summary = 'Showing ' + textOr(capture.lines_returned) + ' of ' +
      textOr(capture.lines_requested) + ' requested lines';
    if (capture.truncated === true) {
      summary += ', truncated';
    }
    summary += ', captured ' + textOr(capture.captured_at) + ', source ' + textOr(capture.source_backend) + '.';
    return { text: stripControl(capture.text), message: summary };
  }

  function loadOutput() {
    var key = ui.selectedKey;
    var entity = currentEntity();
    if (key === null || !entity || !liveServiceAvailable()) {
      return;
    }
    if (entity.artifacts && entity.artifacts.output_available === false) {
      return;
    }
    var seq = ui.outputSeq + 1;
    ui.outputSeq = seq;
    ui.outputBusy = true;
    var gen = generationOf(entity);
    ui.output = { key: key, gen: gen, text: null, message: null };
    renderOutputPanel();
    var requested = parseInt($('output-lines').value, 10);
    var lines = requested > 0 ? Math.min(requested, MAX_OUTPUT_LINES) : DEFAULT_OUTPUT_LINES;
    var url = CAPTURE_URL + '?key=' + encodeURIComponent(key) + '&lines=' + encodeURIComponent(lines) +
      (gen === null ? '' : '&spawn_gen=' + encodeURIComponent(gen));
    fetch(url, { cache: 'no-store', headers: { Accept: 'application/json' } })
      .then(function (response) {
        if (!response.ok) {
          throw new Error('HTTP ' + response.status);
        }
        return response.json();
      })
      .then(function (capture) {
        return interpretCapture(capture, key, entity);
      })
      .catch(function (error) {
        return { text: null, message: 'Output request failed: ' + errorText(error) + '.' };
      })
      .then(function (result) {
        if (seq !== ui.outputSeq) {
          return;
        }
        ui.outputBusy = false;
        if (ui.selectedKey === key) {
          ui.output = { key: key, gen: gen, text: result.text, message: result.message };
        }
        renderOutputPanel();
      });
  }

  /* ---------- events ---------- */

  function toggleGroup(row, scroll) {
    if (row.hasChildren && !isFiltering()) {
      ui.open.set(row.path, !row.open);
    }
    renderAll({ focus: rowToken(row.path), scroll: scroll });
  }

  function onTreeKeydown(event) {
    var target = event.target.closest ? event.target.closest('[role="treeitem"]') : null;
    if (!target) {
      return;
    }
    var rows = ui.rows;
    var index = indexOfPath(rows, target.getAttribute('data-path'));
    if (index < 0) {
      return;
    }
    var row = rows[index];
    var handled = true;
    if (event.key === 'ArrowRight') {
      if (row.hasChildren && !row.open) {
        ui.open.set(row.path, true);
        renderAll({ focus: rowToken(row.path), scroll: true });
      } else if (row.hasChildren) {
        selectPath(rows[index + 1].path, { focus: true, scroll: true });
      }
    } else if (event.key === 'ArrowLeft') {
      if (row.hasChildren && row.open && !isFiltering()) {
        ui.open.set(row.path, false);
        renderAll({ focus: rowToken(row.path), scroll: true });
      } else {
        var parent = parentRowIndex(rows, index);
        if (parent >= 0) {
          selectPath(rows[parent].path, { focus: true, scroll: true });
        }
      }
    } else if (event.key === 'Enter' || event.key === ' ') {
      if (row.node.group !== null) {
        toggleGroup(row, true);
      } else {
        setSelection(row.key);
        ui.focusPath = row.path;
        openDetails();
      }
    } else {
      var next = stepIndex(event.key, index, rows.length);
      if (next < 0) {
        handled = false;
      } else {
        selectPath(rows[next].path, { focus: true, scroll: true });
      }
    }
    if (handled) {
      event.preventDefault();
    }
  }

  function onListKeydown(event) {
    var target = event.target.closest ? event.target.closest('[role="option"]') : null;
    if (!target) {
      return;
    }
    var rows = ui.listRows;
    var index = indexOfPath(rows, target.getAttribute('data-path'));
    if (index < 0) {
      return;
    }
    if (event.key === 'Enter' || event.key === ' ') {
      event.preventDefault();
      setSelection(rows[index].key);
      ui.focusPath = rows[index].path;
      openDetails();
      return;
    }
    var next = stepIndex(event.key, index, rows.length);
    if (next >= 0) {
      event.preventDefault();
      selectPath(rows[next].path, { focus: true, scroll: true });
    }
  }

  function onRowClick(event) {
    var row = event.target.closest ? event.target.closest('[data-path]') : null;
    if (!row) {
      return;
    }
    var path = row.getAttribute('data-path');
    var node = ui.model ? ui.model.nodeByPath.get(path) : null;
    if (node && node.group !== null) {
      toggleGroup(ui.rows[indexOfPath(ui.rows, path)], false);
      return;
    }
    var onToggle = event.target.classList && event.target.classList.contains('toggle');
    if (onToggle && ui.view === 'tree' && !isFiltering()) {
      var index = indexOfPath(ui.rows, path);
      if (index >= 0 && ui.rows[index].hasChildren) {
        ui.open.set(path, !ui.rows[index].open);
        renderAll({ focus: rowToken(path), scroll: false });
        return;
      }
    }
    selectPath(path, { focus: true, scroll: false });
  }

  function onAttentionClick(event) {
    var button = event.target.closest ? event.target.closest('button[data-key]') : null;
    if (!button || button.disabled) {
      return;
    }
    revealKey(button.getAttribute('data-key'));
  }

  function onSearchKeydown(event) {
    if (event.key === 'Escape') {
      event.preventDefault();
      focusSelectedRow();
    } else if (event.key === 'ArrowDown') {
      event.preventDefault();
      focusFirstRow();
    }
  }

  function onPaneKeydown(event) {
    if (event.key === 'Escape') {
      event.preventDefault();
      focusSelectedRow();
    }
  }

  function onGlobalKeydown(event) {
    if (event.key !== '/' || event.ctrlKey || event.metaKey || event.altKey) {
      return;
    }
    var target = event.target;
    if (target && (target.isContentEditable || /^(INPUT|SELECT|TEXTAREA)$/.test(target.tagName))) {
      return;
    }
    event.preventDefault();
    $('search').focus();
  }

  function setAllOpen(open) {
    if (!ui.model) {
      return;
    }
    ui.model.nodeByPath.forEach(function (node) {
      if (node.children.length) {
        ui.open.set(node.path, open);
      }
    });
    renderAll();
  }

  function setView(view) {
    ui.view = view;
    renderAll();
  }

  function wire() {
    $('refresh-now').addEventListener('click', function () { refreshNow(); });
    $('refresh-interval').addEventListener('change', function (event) {
      ui.intervalS = normalizeInterval(event.target.value);
      scheduleNext();
      renderHeader();
    });
    $('search').addEventListener('input', function (event) {
      ui.query = event.target.value.trim().toLowerCase();
      renderAll();
    });
    $('search').addEventListener('keydown', onSearchKeydown);
    $('filter-state').addEventListener('change', function (event) {
      ui.stateFilter = event.target.value === ALL_VALUE ? ALL : event.target.value;
      renderAll();
    });
    $('filter-project').addEventListener('change', function (event) {
      ui.projectFilter = event.target.value === ALL_VALUE ? ALL : event.target.value;
      renderAll();
    });
    $('view-tree').addEventListener('click', function () { setView('tree'); });
    $('view-list').addEventListener('click', function () { setView('list'); });
    $('expand-all').addEventListener('click', function () { setAllOpen(true); });
    $('collapse-all').addEventListener('click', function () { setAllOpen(false); });
    $('tree').addEventListener('keydown', onTreeKeydown);
    $('tree').addEventListener('click', onRowClick);
    $('list').addEventListener('keydown', onListKeydown);
    $('list').addEventListener('click', onRowClick);
    $('attention-list').addEventListener('click', onAttentionClick);
    $('output-load').addEventListener('click', loadOutput);
    $('output-back').addEventListener('click', focusSelectedRow);
    document.querySelector('.side-pane').addEventListener('keydown', onPaneKeydown);
    document.addEventListener('keydown', onGlobalKeydown);
  }

  function init() {
    $('details-title').setAttribute('data-focus-token', 'details');
    wire();
    var embedded = window.BIRDS_EYE_DATA;
    if (embedded !== null && typeof embedded === 'object') {
      ui.embedded = true;
      try {
        acceptView(embedded);
      } catch (error) {
        ui.error = errorText(error);
      }
      renderAll();
    } else {
      renderAll();
      refreshNow();
    }
    // Embedded snapshots run no timers at all; the clock only serves live pages.
    if (!ui.embedded) {
      window.setInterval(function () {
        if (ui.doc && pollAllowed()) {
          renderHeader();
          tickAges();
        }
      }, CLOCK_TICK_MS);
    }
  }

  if (document.readyState === 'loading') {
    document.addEventListener('DOMContentLoaded', init);
  } else {
    init();
  }
})();
