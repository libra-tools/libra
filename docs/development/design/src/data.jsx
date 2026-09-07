// Conversation threads shown in the sidebar
const THREADS = [
  { id: 't1', title: 'Add optimistic updates to useMutation', ago: '1m',  active: true,  phase: 2 },
  { id: 't2', title: 'Refactor auth provider to context v3',   ago: '18m', phase: 4 },
  { id: 't3', title: 'Migrate Table to virtualized rows',      ago: '1h',  phase: 2 },
  { id: 't4', title: 'Fix SSR hydration warning in <Toast>',   ago: '3h',  phase: 4 },
  { id: 't5', title: 'Audit zod schemas for form module',      ago: '1d',  phase: 3 },
  { id: 't6', title: 'Dark mode tokens for Settings panel',    ago: '2d',  phase: 4 },
  { id: 't7', title: 'Type-narrow router params helper',       ago: '4d',  phase: 0 },
];

// The active thread's chat transcript
const MESSAGES = [
  { role: 'user', time: '10:42', body:
    "Let's add optimistic updates to useMutation in src/lib/query.ts. The current mutate() waits for the server — I want rollback on failure and automatic reconciliation when the real response lands."
  },
  { role: 'assistant', time: '10:42', body:
    "I read src/lib/query.ts, src/hooks/useMutation.ts, and the test in __tests__/useMutation.test.ts. Here's what I'll do:\n\n— Snapshot cache state before the mutation fires\n— Apply the optimistic patch synchronously so subscribers see it immediately\n— On success, replace the optimistic entry with the server response\n— On failure, roll back to the snapshot and surface the error via onError\n\nI'll thread a new optimistic field through MutationOptions<T> so existing callers keep working. Draft plan ready on the right — confirm the intent or revise."
  },
  { role: 'user', time: '10:44', body:
    "Looks right. One thing — the rollback has to preserve ordering for any other mutations that landed between snapshot and rollback. Don't clobber them."
  },
  { role: 'assistant', time: '10:44', body:
    "Good catch. I'll keep a per-key revision counter and only revert entries whose revision still matches the snapshot. Added that to the execution plan as a checkpoint between step 2 and 3."
  },
  { role: 'assistant', time: '10:46', streaming: true, body:
    "Executing Phase 2 — wiring the optimistic patch into the cache layer now. About to run the test DAG once the execution DAG settles."
  }
];

// The agent's current workflow state (left to right across the doc)
const WORKFLOW = {
  currentPhase: 2,
  intent: {
    title: 'Add optimistic updates to useMutation',
    revision: 'r2',
    summary: 'Introduce optimistic cache patching with rollback-on-error and reconciliation when the server response lands. Preserve ordering across concurrent mutations.',
    constraints: [
      'Do not break MutationOptions<T> public shape',
      'Keep rollback safe under concurrent mutations',
      'Cover happy + error path with tests',
    ],
    confirmed: true,
  },
  plans: {
    execution: {
      id: 'plan-exec-04',
      steps: [
        { id: 's1', label: 'Snapshot cache at mutate() entry', status: 'done' },
        { id: 's2', label: 'Apply optimistic patch to subscribers', status: 'done' },
        { id: 's3', label: 'Per-key revision counter for safe rollback', status: 'running' },
        { id: 's4', label: 'Reconcile server response into cache', status: 'queued' },
        { id: 's5', label: 'Surface onError with rollback context', status: 'queued' },
      ],
    },
    test: {
      id: 'plan-test-02',
      steps: [
        { id: 't1', label: 'Happy-path optimistic update reflects immediately', status: 'queued' },
        { id: 't2', label: 'Failure rolls back and preserves concurrent writes', status: 'queued' },
        { id: 't3', label: 'Reconciliation replaces optimistic entry', status: 'queued' },
      ],
    },
  },
  runs: [
    { id: 'run-11', step: 's1', result: 'pass', ago: '2m', patch: '+12 −0' },
    { id: 'run-12', step: 's2', result: 'pass', ago: '2m', patch: '+34 −7' },
    { id: 'run-13', step: 's3', result: 'running', ago: 'now', patch: '…' },
  ],
  evidence: [
    { kind: 'tool', label: 'read src/lib/query.ts', meta: '214 lines' },
    { kind: 'tool', label: 'read src/hooks/useMutation.ts', meta: '88 lines' },
    { kind: 'tool', label: 'grep "MutationOptions"', meta: '9 matches in 4 files' },
    { kind: 'frame', label: 'ContextFrame cf-0418', meta: 'cache shape captured' },
    { kind: 'patch', label: 'PatchSet ps-07', meta: '+46 −7 across 2 files' },
  ],
};

// Libra's five-phase pipeline
const PHASES = [
  { n: 0, key: 'intent',    label: 'Phase 0',  name: 'Intent',     blurb: 'Draft & confirm' },
  { n: 1, key: 'plan',      label: 'Phase 1',  name: 'Plan',       blurb: 'Analyze & confirm' },
  { n: 2, key: 'execution', label: 'Phase 2',  name: 'Execution',  blurb: 'Stage-gated DAG' },
  { n: 3, key: 'validate',  label: 'Phase 3',  name: 'Validation', blurb: 'Audit & evidence' },
  { n: 4, key: 'release',   label: 'Phase 4',  name: 'Release',    blurb: 'Decision' },
];

// Summary tab
const SUMMARY = {
  progress: [
    { done: true,  text: 'Read src/lib/query.ts and snapshot current cache shape' },
    { done: true,  text: 'Design MutationOptions<T> extension with optional optimistic field' },
    { done: true,  text: 'Implement cache.snapshot() + cache.rollback() primitives' },
    { done: false, text: 'Wire per-key revision counter so rollback preserves ordering' },
    { done: false, text: 'Cover happy + error path with tests in __tests__/useMutation.test.ts' },
  ],
  branch: {
    name: 'agent/optimistic-mutate',
    base: 'main',
    pr: 'No pull request',
    changes: '2 files changed, 1 untracked',
  },
  artifacts: [
    { kind: 'PatchSet', id: 'ps-07', meta: '+46 −7 across 2 files' },
    { kind: 'Frame',    id: 'cf-0418', meta: 'cache shape captured' },
  ],
  todo: [
    { done: true,  text: 'Snapshot cache at mutate() entry' },
    { done: true,  text: 'Apply optimistic patch to subscribers' },
    { done: false, text: 'Per-key revision counter for safe rollback' },
    { done: false, text: 'Reconcile server response into cache' },
    { done: false, text: 'Surface onError with rollback context' },
    { done: false, text: 'Update MutationOptions<T> JSDoc' },
  ],
};

// Review tab — grouped diff hunks
const REVIEW = {
  stats: { files: 2, add: 46, del: 7 },
  files: [
    {
      path: 'src/lib/query.ts',
      add: 34, del: 7,
      hunks: [{
        header: '@@ -214,10 +214,23 @@ export function useMutation<T>(',
        lines: [
          { kind: 'ctx', n1: 214, n2: 214, text: '    const [state, setState] = React.useState<State<T>>({ idle: true });' },
          { kind: 'ctx', n1: 215, n2: 215, text: '' },
          { kind: 'ctx', n1: 216, n2: 216, text: '    async function mutate(input: TInput) {' },
          { kind: 'del', n1: 217,           text: '      const result = await fetcher(input);' },
          { kind: 'del', n1: 218,           text: '      cache.set(key, result);' },
          { kind: 'del', n1: 219,           text: '      setState({ idle: false, data: result });' },
          { kind: 'add',           n2: 217, text: '      const snap = cache.snapshot(key);' },
          { kind: 'add',           n2: 218, text: '      if (options.optimistic) {' },
          { kind: 'add',           n2: 219, text: '        cache.patch(key, options.optimistic(input));' },
          { kind: 'add',           n2: 220, text: '      }' },
          { kind: 'add',           n2: 221, text: '      try {' },
          { kind: 'add',           n2: 222, text: '        const result = await fetcher(input);' },
          { kind: 'add',           n2: 223, text: '        cache.reconcile(key, snap.rev, result);' },
          { kind: 'add',           n2: 224, text: '        setState({ idle: false, data: result });' },
          { kind: 'add',           n2: 225, text: '      } catch (err) {' },
          { kind: 'add',           n2: 226, text: '        cache.rollback(key, snap);' },
          { kind: 'add',           n2: 227, text: '        options.onError?.(err, { rolledBack: true });' },
          { kind: 'add',           n2: 228, text: '        throw err;' },
          { kind: 'add',           n2: 229, text: '      }' },
          { kind: 'ctx', n1: 220, n2: 230, text: '    }' },
          { kind: 'ctx', n1: 221, n2: 231, text: '' },
          { kind: 'ctx', n1: 222, n2: 232, text: '    return { state, mutate };' },
        ],
      }],
    },
    {
      path: 'src/lib/cache.ts',
      add: 12, del: 0,
      hunks: [{
        header: '@@ -88,3 +88,15 @@ export class Cache {',
        lines: [
          { kind: 'ctx', n1: 88, n2: 88, text: '  set(key: Key, value: Value) {' },
          { kind: 'ctx', n1: 89, n2: 89, text: '    this.store.set(key, { value, rev: ++this.rev });' },
          { kind: 'ctx', n1: 90, n2: 90, text: '  }' },
          { kind: 'add',         n2: 91, text: '' },
          { kind: 'add',         n2: 92, text: '  snapshot(key: Key): Snap {' },
          { kind: 'add',         n2: 93, text: '    const entry = this.store.get(key);' },
          { kind: 'add',         n2: 94, text: '    return { key, rev: entry?.rev ?? 0, value: entry?.value };' },
          { kind: 'add',         n2: 95, text: '  }' },
          { kind: 'add',         n2: 96, text: '' },
          { kind: 'add',         n2: 97, text: '  rollback(key: Key, snap: Snap) {' },
          { kind: 'add',         n2: 98, text: '    const current = this.store.get(key);' },
          { kind: 'add',         n2: 99, text: '    if (current && current.rev !== snap.rev + 1) return;' },
          { kind: 'add',         n2: 100, text: '    this.store.set(key, { value: snap.value, rev: ++this.rev });' },
          { kind: 'add',         n2: 101, text: '  }' },
        ],
      }],
    },
  ],
};

// Storage / Objects view — Libra's SQLite-backed git object store.
// Mirrors the `libra graph` TUI: an object DAG of commits → trees → blobs.
const OBJECTS = {
  store: {
    backend: 'SQLite',
    path: '.libra/libra.db',
    count: 47,
    size: '1.24 MB',
    loose: 12,
    packed: 35,
  },
  refs: [
    { name: 'HEAD', symbolic: 'agent/optimistic-mutate' },
    { name: 'agent/optimistic-mutate', oid: 'a81f3c2', kind: 'branch', head: true },
    { name: 'main', oid: '3d2e8a0', kind: 'branch' },
    { name: 'v0.3.0', oid: 'b3d2f01', kind: 'tag' },
  ],
  // Commit DAG, newest first. `rail` is the precomputed TUI graph gutter.
  commits: [
    { oid: 'a81f3c2', rail: '●', lane: 0, parents: ['7f3a9e1'], tree: 'f4e1b09',
      refs: ['HEAD', 'agent/optimistic-mutate'],
      msg: 'wire revision guard for safe rollback', author: 'libra-agent', when: '10:46', stat: '+12 −0' },
    { oid: '7f3a9e1', rail: '●', lane: 0, parents: ['c902b13'], tree: 'b7c2d51',
      msg: 'add snapshot/rollback primitives to Cache', author: 'libra-agent', when: '10:45', stat: '+34 −7' },
    { oid: 'c902b13', rail: '●', lane: 0, parents: ['b3d2f01'], tree: 'a1f8e22',
      msg: 'extend MutationOptions<T> with optimistic', author: 'libra-agent', when: '10:45', stat: '+8 −1' },
    { oid: '3d2e8a0', rail: '│ ●', lane: 1, parents: ['b3d2f01'], tree: 'd5a0c43',
      refs: ['main'],
      msg: 'docs: clarify cache eviction policy', author: 'erin', when: '2d', stat: '+21 −3' },
    { oid: 'b3d2f01', rail: '●─╯', lane: 0, parents: [], tree: 'c0ffee1',
      refs: ['v0.3.0'],
      msg: 'baseline: query cache module', author: 'erin', when: '4d', stat: '+412 −0' },
  ],
  // Tree contents keyed by tree oid.
  trees: {
    'f4e1b09': [
      { type: 'tree', oid: 'e7b1a04', name: 'src/', entries: 4 },
      { type: 'blob', oid: '9c2a4f1', name: 'Cargo.toml', size: '512 B' },
      { type: 'blob', oid: '2d8f013', name: 'README.md', size: '3.4 KB' },
    ],
    'e7b1a04': [
      { type: 'tree', oid: 'aa31c90', name: 'lib/', entries: 3 },
      { type: 'tree', oid: 'bb77e12', name: 'hooks/', entries: 2 },
    ],
    'aa31c90': [
      { type: 'blob', oid: '9c2a4f1', name: 'query.ts', size: '4.1 KB', changed: true },
      { type: 'blob', oid: '2d8f013', name: 'cache.ts', size: '1.8 KB', changed: true },
      { type: 'blob', oid: '5e1b8a2', name: 'mod.ts', size: '128 B' },
    ],
  },
  // Object bodies for the detail panel (git cat-file -p style).
  bodies: {
    'a81f3c2': `tree f4e1b09\nparent 7f3a9e1\nauthor libra-agent <agent@web3infra.io> 1777009560 +0000\ncommitter libra-agent <agent@web3infra.io> 1777009560 +0000\n\nwire revision guard for safe rollback\n\nOnly revert cache entries whose revision still matches the\nsnapshot, so concurrent mutations are preserved.`,
    'f4e1b09': `040000 tree e7b1a04    src\n100644 blob 9c2a4f1    Cargo.toml\n100644 blob 2d8f013    README.md`,
    '9c2a4f1': `export function useMutation<T>(key: Key, opts: MutationOptions<T>) {\n  const snap = cache.snapshot(key);\n  if (opts.optimistic) cache.patch(key, opts.optimistic(input));\n  // …\n}`,
  },
};

Object.assign(window, { THREADS, MESSAGES, WORKFLOW, PHASES, SUMMARY, REVIEW, OBJECTS });
