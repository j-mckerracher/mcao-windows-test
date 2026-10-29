// Deterministic fm-birds-eye-view.v1 and fm-birds-eye-capture.v1 fixtures for
// the dashboard UI harness. Every value is fixed so assertions can name exact
// text. The harness pins Date.now to NOW_EPOCH in the page.

export const NOW_EPOCH = 1792293600;
export const STALE_AFTER_SECONDS = 120;

const VIEW_SCHEMA = 'fm-birds-eye-view.v1';
const CAPTURE_SCHEMA = 'fm-birds-eye-capture.v1';

export const KEYS = {
  alpha: 'main/sup-alpha',
  w101: 'main/w-101',
  w102: 'main/w-102',
  w103: 'main/w-103',
  w105: 'main/w-105',
  w106: 'main/w-106',
  beta: 'main/sup-beta',
  w201: 'main/w-201',
  gamma: 'gamma/sec-gamma',
  w301: 'gamma/w-301',
  w302: 'gamma/w-302',
  sub1: 'gamma/sub-1',
  w303: 'gamma/w-303'
};

const iso = (epoch) => new Date(epoch * 1000).toISOString().replace('.000Z', 'Z');

const clone = (value) => JSON.parse(JSON.stringify(value));

function merge(base, over) {
  if (over === undefined) {
    return base;
  }
  if (base && typeof base === 'object' && !Array.isArray(base) && over && typeof over === 'object' && !Array.isArray(over)) {
    const out = { ...base };
    for (const name of Object.keys(over)) {
      out[name] = merge(base[name], over[name]);
    }
    return out;
  }
  return over;
}

// Every contract field is present, so a null in a fixture always means
// "Not recorded" or "Unavailable" rather than a missing key.
function entity(over) {
  const base = {
    key: null,
    kind: 'worker',
    task_id: null,
    home_id: 'main',
    name: null,
    role: null,
    project: null,
    parent_key: null,
    child_keys: [],
    contextual_in: [],
    spawn_gen: 'g1',
    state: {
      raw: null,
      display: null,
      source: 'status',
      detail: null,
      wait_reason: null,
      observed_at: iso(NOW_EPOCH - 40)
    },
    classification: null,
    timing: { elapsed_seconds: null, elapsed_basis: null, state_seconds: null, state_basis: null },
    observation: {
      quality: 'fresh',
      collected_at: iso(NOW_EPOCH - 30),
      source_event_at: iso(NOW_EPOCH - 40),
      root_id: 'main'
    },
    decision: { label: null, recipient: null, resolver: null, key: null, age_seconds: null },
    blocker: { reason: null, age_seconds: null },
    execution: {
      harness: 'claude',
      requested_model: null,
      requested_effort: null,
      observed_model: null,
      observed_effort: null,
      playbook: null,
      workflow_stage: null
    },
    artifacts: {
      worktree: null,
      branch: null,
      pr_url: null,
      report_path: null,
      output_available: true
    },
    prerequisites: [],
    latest_activity: { text: null, reported_by_agent: false, at: iso(NOW_EPOCH - 45) }
  };
  return merge(base, over);
}

function countsFor(entities) {
  const workers = {};
  const supervisors = {};
  let contextual = 0;
  for (const item of entities) {
    contextual += item.contextual_in.length;
    if (item.kind === 'worker') {
      const raw = item.state.raw === null ? 'unknown' : item.state.raw;
      workers[raw] = (workers[raw] || 0) + 1;
    } else {
      const classification = item.classification === null ? 'unknown' : item.classification;
      supervisors[classification] = (supervisors[classification] || 0) + 1;
    }
  }
  return {
    workers_by_state: workers,
    supervisors_by_classification: supervisors,
    unique_entities: entities.length,
    contextual_copies: contextual
  };
}

function view({ mode = 'live', generatedEpoch = NOW_EPOCH - 10, roots, entities, attention, summaries = [], unsupported = [] }) {
  return {
    schema: VIEW_SCHEMA,
    generated_at: iso(generatedEpoch),
    generated_epoch: generatedEpoch,
    mode,
    refresh: { interval_seconds: 10, stale_after_seconds: STALE_AFTER_SECONDS },
    roots: roots || [
      {
        root_id: 'main',
        home_path: '/fixture/main',
        source: 'configured',
        status: 'ok',
        error: null,
        collected_at: iso(generatedEpoch),
        last_good_at: iso(generatedEpoch),
        stale: false
      },
      {
        root_id: 'gamma',
        home_path: '/fixture/gamma',
        source: 'descendant',
        status: 'failed',
        error: 'schema mismatch in status file',
        collected_at: iso(generatedEpoch),
        last_good_at: iso(NOW_EPOCH - 600),
        stale: true
      }
    ],
    unsupported,
    entities,
    attention,
    counts: countsFor(entities),
    history: {
      limit: 1000,
      retained: entities.length,
      evicted: 0,
      limit_reached: false,
      restart_disclosure: 'History starts at the last service restart.',
      summaries
    }
  };
}

function mixedEntities() {
  return [
    entity({
      key: KEYS.alpha,
      kind: 'supervisor',
      name: 'Auth secondmate',
      role: 'Scope: auth',
      child_keys: [KEYS.w101, KEYS.w102, KEYS.w105],
      spawn_gen: 'g1',
      state: { raw: 'working', display: 'Running', detail: 'Three workers active' },
      classification: 'active_child_work',
      timing: { elapsed_seconds: 7200, elapsed_basis: 'spawn', state_seconds: 600, state_basis: 'status_event' },
      latest_activity: { text: 'Supervising three workers.', reported_by_agent: false, at: iso(NOW_EPOCH - 45) }
    }),
    entity({
      key: KEYS.w101,
      name: 'Payments adapter',
      task_id: 'DPES-101',
      project: 'payments',
      role: 'Implementation',
      parent_key: KEYS.alpha,
      spawn_gen: 'g1',
      state: { raw: 'blocked', display: 'Blocked', detail: 'Waiting on credentials', wait_reason: 'Waiting for the payments API key' },
      timing: { elapsed_seconds: 5400, elapsed_basis: 'spawn', state_seconds: 3600, state_basis: 'status_event' },
      blocker: { reason: 'Waiting for the payments API key', age_seconds: 3600 },
      execution: { requested_model: 'model-large', requested_effort: 'high' },
      artifacts: { worktree: '/worktrees/DPES-101', branch: 'fm/DPES-101', output_available: true },
      latest_activity: { text: 'Blocked on the payments API key.', reported_by_agent: true, at: iso(NOW_EPOCH - 90) }
    }),
    entity({
      key: KEYS.w102,
      name: 'Merge plan',
      task_id: 'DPES-102',
      project: 'payments',
      role: 'Review',
      parent_key: KEYS.alpha,
      state: { raw: 'parked', display: 'Parked', detail: 'Merge plan awaiting an answer' },
      classification: 'captain_decision',
      timing: { elapsed_seconds: 4000, elapsed_basis: 'spawn', state_seconds: 1800, state_basis: 'status_event' },
      decision: { label: 'Needs you', recipient: 'operator', resolver: 'operator', key: 'dec-17', age_seconds: 1800 },
      artifacts: { worktree: '/worktrees/DPES-102', branch: 'fm/DPES-102', pr_url: 'javascript:alert(1)' }
    }),
    entity({
      key: KEYS.w105,
      name: 'Search indexer',
      task_id: 'DPES-105',
      project: 'search',
      role: 'Implementation',
      parent_key: KEYS.alpha,
      state: { raw: 'paused', display: 'Paused', wait_reason: 'Paused until the reviewer returns' },
      classification: 'externally_held',
      timing: { elapsed_seconds: 2400, elapsed_basis: 'spawn', state_seconds: 900, state_basis: 'status_event' },
      decision: { label: 'Needs you', recipient: null, resolver: null, key: 'dec-18', age_seconds: 900 },
      observation: { quality: 'cached' }
    }),
    entity({
      key: KEYS.beta,
      kind: 'supervisor',
      name: 'Operations lead',
      role: 'Scope: operations',
      child_keys: [KEYS.w201, KEYS.w103, KEYS.gamma],
      state: { raw: 'idle', display: 'Idle' },
      classification: 'no_active_work'
    }),
    entity({
      key: KEYS.w201,
      name: '<img src=x onerror="window.__xss=1"> Search <b>index</b>',
      task_id: 'DPES-201',
      project: 'search',
      role: 'Implementation',
      parent_key: KEYS.beta,
      state: { raw: 'done', display: 'Completed', detail: 'Shipped' },
      artifacts: { branch: 'fm/DPES-201', pr_url: 'https://github.com/example/search/pull/42', output_available: true },
      latest_activity: { text: '<script>window.__xss=3</script>Shipped.', reported_by_agent: true, at: iso(NOW_EPOCH - 300) }
    }),
    entity({
      key: KEYS.w103,
      name: 'Deploy check',
      task_id: 'DPES-103',
      project: 'search',
      role: 'Verification',
      parent_key: KEYS.beta,
      state: { raw: 'failed', display: 'Failed', detail: 'Exit 3 from the deploy check', wait_reason: 'Deploy check failed' },
      timing: { elapsed_seconds: 6000, elapsed_basis: 'spawn', state_seconds: 3600, state_basis: 'status_event' },
      artifacts: { branch: 'fm/DPES-103', pr_url: 'http://insecure.example/pr/9', output_available: false },
      prerequisites: [{ key: KEYS.w201, source: 'dependency' }]
    }),
    entity({
      key: KEYS.gamma,
      kind: 'secondmate',
      home_id: 'gamma',
      name: 'Reports secondmate',
      role: 'Scope: reports',
      parent_key: KEYS.beta,
      child_keys: [KEYS.w301, KEYS.w302, KEYS.sub1],
      state: { raw: 'working', display: 'Running' },
      classification: 'active_child_work',
      observation: { root_id: 'gamma' }
    }),
    entity({
      key: KEYS.w301,
      home_id: 'gamma',
      name: 'Quarterly export',
      task_id: 'DPES-301',
      project: 'reports',
      role: 'Implementation',
      parent_key: KEYS.gamma,
      contextual_in: [KEYS.alpha],
      state: { raw: 'working', display: 'Running' },
      timing: { elapsed_seconds: 900, elapsed_basis: 'spawn', state_seconds: 120, state_basis: 'status_event' },
      observation: { root_id: 'gamma' }
    }),
    entity({
      key: KEYS.w302,
      home_id: 'gamma',
      name: 'Dashboard feed',
      task_id: 'DPES-302',
      project: 'reports',
      role: 'Implementation',
      parent_key: KEYS.gamma,
      state: { raw: null, display: null, source: null, observed_at: null },
      observation: { quality: 'unavailable', collected_at: null, source_event_at: null, root_id: 'gamma' }
    }),
    entity({
      key: KEYS.sub1,
      kind: 'supervisor',
      home_id: 'gamma',
      name: 'Export lead',
      role: 'Scope: exports',
      project: 'reports',
      parent_key: KEYS.gamma,
      child_keys: [KEYS.w303],
      state: { raw: 'waiting', display: 'Waiting' },
      classification: 'externally_held',
      observation: { root_id: 'gamma' }
    }),
    entity({
      key: KEYS.w303,
      home_id: 'gamma',
      name: 'Feed contract',
      task_id: 'DPES-303',
      project: 'reports',
      role: 'Implementation',
      parent_key: KEYS.sub1,
      state: { raw: 'blocked', display: 'Blocked', wait_reason: 'Waiting for a dependency from another team' },
      timing: { elapsed_seconds: 7000, elapsed_basis: 'spawn', state_seconds: 5400, state_basis: 'status_event' },
      blocker: { reason: 'Waiting for a dependency from another team', age_seconds: 5400 },
      observation: { root_id: 'gamma' }
    })
  ];
}

function mixedAttention() {
  return [
    {
      key: KEYS.w302,
      class: 'observation',
      priority: 4,
      reason: 'Observation is unavailable for this item',
      resolver: null,
      age_seconds: 900,
      project: 'reports'
    },
    {
      key: KEYS.w103,
      class: 'failure',
      priority: 2,
      reason: 'Deploy check failed',
      resolver: null,
      age_seconds: 3600,
      project: 'search'
    },
    {
      key: KEYS.w303,
      class: 'blocker',
      priority: 3,
      reason: 'Waiting for a dependency from another team',
      resolver: 'reports lead',
      age_seconds: 5400,
      project: 'reports'
    },
    {
      key: KEYS.w101,
      class: 'blocker',
      priority: 3,
      reason: 'Waiting for the payments API key',
      resolver: 'operator',
      age_seconds: 3600,
      project: 'payments'
    },
    {
      key: KEYS.w102,
      class: 'decision',
      priority: 1,
      reason: 'Merge plan needs a decision',
      resolver: 'operator',
      age_seconds: 1800,
      project: 'payments'
    },
    {
      key: KEYS.w105,
      class: 'decision',
      priority: 1,
      reason: 'Paused until a reviewer is named',
      resolver: null,
      age_seconds: 900,
      project: 'search'
    }
  ];
}

// Ten-entity-scale fixture for the mixed-scope tests. Used as the live view.
export function mixedView() {
  return view({
    entities: mixedEntities(),
    attention: mixedAttention(),
    unsupported: [{ category: 'home', reason: 'delta home reports an unsupported schema version' }]
  });
}

// Same identities as mixedView, with w101 unblocked, w103 gone (it vanished
// from the latest view), and w106 added. Used to test refresh preservation.
export function mixedRefreshView() {
  const entities = mixedEntities()
    .filter((item) => item.key !== KEYS.w103)
    .map((item) => {
      if (item.key === KEYS.w101) {
        return entity({
          ...item,
          state: { raw: 'working', display: 'Running', detail: 'Retrying with the new key', wait_reason: null },
          blocker: { reason: null, age_seconds: null }
        });
      }
      if (item.key === KEYS.beta) {
        return entity({ ...item, child_keys: [KEYS.w201, KEYS.gamma] });
      }
      if (item.key === KEYS.alpha) {
        return entity({ ...item, child_keys: [KEYS.w101, KEYS.w102, KEYS.w105, KEYS.w106] });
      }
      return item;
    });
  entities.push(
    entity({
      key: KEYS.w106,
      name: 'Token rotation',
      task_id: 'DPES-106',
      project: 'payments',
      role: 'Implementation',
      parent_key: KEYS.alpha,
      state: { raw: 'working', display: 'Running' },
      timing: { elapsed_seconds: 60, elapsed_basis: 'spawn', state_seconds: 60, state_basis: 'status_event' }
    })
  );
  const attention = mixedAttention().filter((item) => item.key !== KEYS.w101 && item.key !== KEYS.w103);
  return view({
    generatedEpoch: NOW_EPOCH - 5,
    entities,
    attention,
    summaries: [
      {
        key: KEYS.w103,
        generation: 'g1',
        last_state: 'failed',
        last_seen_at: iso(NOW_EPOCH - 60),
        historical: true
      }
    ],
    unsupported: [{ category: 'home', reason: 'delta home reports an unsupported schema version' }]
  });
}

// Returns the same view with a generated epoch chosen so the harness clock
// makes it stale (the harness moves its clock forward for that case).
export function withGeneratedEpoch(doc, generatedEpoch) {
  const copy = clone(doc);
  copy.generated_epoch = generatedEpoch;
  copy.generated_at = iso(generatedEpoch);
  return copy;
}

// Deterministic 200-entity, 10-project, 8-level fixture for the performance
// measurement. Each project holds one 8-deep chain plus 12 branches hung from
// the chain's middle levels, so no node is deeper than index 7.
export function perfView({ changedKey = null, changedRaw = 'blocked' } = {}) {
  const states = ['working', 'blocked', 'parked', 'paused', 'done', 'failed', 'working'];
  const entities = [];
  const attention = [];
  for (let p = 0; p < 10; p += 1) {
    const project = `proj-${String(p).padStart(2, '0')}`;
    const chain = [];
    for (let i = 0; i < 8; i += 1) {
      chain.push(`perf/${project}/c${i}`);
    }
    const members = [];
    for (let i = 0; i < 8; i += 1) {
      members.push({ key: chain[i], parent: i === 0 ? null : chain[i - 1], index: i });
    }
    for (let j = 0; j < 12; j += 1) {
      const parentIndex = 3 + (j % 4);
      const key = `perf/${project}/b${j}`;
      members.push({ key, parent: chain[parentIndex], index: 8 + j });
    }
    const childMap = new Map();
    members.forEach((member) => {
      if (member.parent) {
        if (!childMap.has(member.parent)) {
          childMap.set(member.parent, []);
        }
        childMap.get(member.parent).push(member.key);
      }
    });
    members.forEach((member, slot) => {
      const ordinal = p * 20 + slot;
      let raw = states[ordinal % states.length];
      if (member.key === changedKey) {
        raw = changedRaw;
      }
      const isChainNode = member.key.includes('/c');
      const name = `${isChainNode ? 'Stage' : 'Task'} ${project}-${String(slot).padStart(2, '0')} ${raw}`;
      const contextual = slot === 0 && p < 9 ? [`perf/proj-${String(p + 1).padStart(2, '0')}/c1`] : [];
      entities.push(
        entity({
          key: member.key,
          kind: 'worker',
          name,
          task_id: `DPES-${1000 + ordinal}`,
          project,
          role: 'Implementation',
          parent_key: member.parent,
          child_keys: childMap.get(member.key) || [],
          contextual_in: contextual,
          state: { raw, display: raw, wait_reason: raw === 'blocked' ? 'Waiting for a dependency' : null },
          timing: { elapsed_seconds: 3600 + ordinal, elapsed_basis: 'spawn', state_seconds: 60 + ordinal, state_basis: 'status_event' },
          blocker: { reason: raw === 'blocked' ? 'Waiting for a dependency' : null, age_seconds: raw === 'blocked' ? 60 : null },
          decision: raw === 'parked' ? { label: 'Needs you', recipient: 'operator', resolver: 'operator', key: `dec-${ordinal}`, age_seconds: 120 } : undefined
        })
      );
      if (raw === 'blocked' || raw === 'failed' || raw === 'parked') {
        const cls = raw === 'blocked' ? 'blocker' : raw === 'failed' ? 'failure' : 'decision';
        attention.push({
          key: member.key,
          class: cls,
          priority: 1,
          reason: `${raw} at ${project}`,
          resolver: 'operator',
          age_seconds: 300,
          project
        });
      }
    });
  }
  // Keep each perf entity's decision object complete when no decision applies.
  entities.forEach((item) => {
    if (!item.decision || item.decision.label === undefined) {
      item.decision = { label: null, recipient: null, resolver: null, key: null, age_seconds: null };
    }
  });
  return view({ entities, attention, roots: [
    {
      root_id: 'main',
      home_path: '/fixture/perf',
      source: 'configured',
      status: 'ok',
      error: null,
      collected_at: iso(NOW_EPOCH - 30),
      last_good_at: iso(NOW_EPOCH - 30),
      stale: false
    }
  ] });
}

function capture(key, over) {
  return {
    schema: CAPTURE_SCHEMA,
    key,
    task_id: null,
    spawn_gen_requested: 'g1',
    spawn_gen_current: 'g1',
    captured_at: iso(NOW_EPOCH - 5),
    source_backend: 'tmux',
    lines_requested: 40,
    lines_returned: 0,
    truncated: false,
    status: 'ok',
    reason: null,
    text: null,
    ...over
  };
}

// Output responses keyed by item. Each scenario exercises one status branch.
const CAPTURES = new Map([
  [
    KEYS.w101,
    capture(KEYS.w101, {
      task_id: 'DPES-101',
      lines_returned: 4,
      text: [
        'building payments adapter',
        '<script>window.__xss=2</script>',
        '\u001b[31m[ERR]\u001b[0m waiting for key: PAYMENTS_API_KEY\u001b]0;hijacked title\u0007\u0008',
        'https://example.invalid/not-linked'
      ].join('\n')
    })
  ],
  [
    KEYS.w201,
    capture(KEYS.w201, {
      task_id: 'DPES-201',
      spawn_gen_requested: 'g0',
      spawn_gen_current: 'g1',
      status: 'generation_mismatch',
      reason: 'The output belongs to an earlier run of this item.'
    })
  ],
  [
    KEYS.w302,
    capture(KEYS.w302, { task_id: 'DPES-302', status: 'unknown_identity', reason: 'No live worker matches this identity.' })
  ],
  [
    KEYS.w105,
    capture(KEYS.w105, { task_id: 'DPES-105', status: 'error', reason: 'capture backend timed out' })
  ],
  [
    KEYS.w303,
    capture(KEYS.w303, { task_id: 'DPES-303', status: 'unavailable', reason: 'The pane is not running.' })
  ]
]);

export function captureFor(key, lines) {
  const found = CAPTURES.get(key);
  const base = found || capture(key, { status: 'unknown_identity', reason: 'No live worker matches this identity.' });
  return { ...base, lines_requested: lines };
}

export const VIEW_SCHEMA_NAME = VIEW_SCHEMA;
