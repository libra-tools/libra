// Git-branch-style timeline for the Workflow pane's left rail.
// Renders a vertical trunk for the main thread, a fork for execution runs,
// and per-commit metadata (phase/run/time).

function GitTimeline({ onOpen, activeDetail }) {
  const commits = buildCommits();

  // SVG geometry
  const ROW_H = 30;     // vertical spacing between commits
  const LANE_W = 16;    // x distance between lanes
  const X0 = 18;        // trunk x
  const width = 80;     // svg width
  const height = commits.length * ROW_H + 12;

  function laneX(lane) {
    return X0 + lane * LANE_W;
  }

  return (
    <aside style={tlStyles.wrap}>
      <div style={tlStyles.head}>
        <IconBranch size={12}/>
        <span>Thread graph</span>
      </div>
      <div style={tlStyles.scroll}>
        <svg width={width} height={height} style={{ display: 'block' }}>
          {/* Edges */}
          {commits.map((c, i) => {
            if (!c.parents) return null;
            return c.parents.map((p, k) => {
              const parent = commits.find(x => x.id === p);
              if (!parent) return null;
              const y1 = parent.y;
              const y2 = c.y;
              const x1 = laneX(parent.lane);
              const x2 = laneX(c.lane);
              if (x1 === x2) {
                return (
                  <line key={`${c.id}-${k}`}
                    x1={x1} y1={y1} x2={x2} y2={y2}
                    stroke={c.lane === 0 ? 'var(--ink-2)' : 'var(--accent)'}
                    strokeWidth="1.5"
                  />
                );
              }
              // curved connector (branch or merge)
              const midY = (y1 + y2) / 2;
              const d = `M ${x1} ${y1} C ${x1} ${midY}, ${x2} ${midY}, ${x2} ${y2}`;
              return (
                <path key={`${c.id}-${k}`} d={d} fill="none"
                  stroke={c.kind === 'merge' ? 'var(--ink-2)' : 'var(--accent)'}
                  strokeWidth="1.5"
                />
              );
            });
          })}

          {/* Nodes */}
          {commits.map(c => {
            const cx = laneX(c.lane);
            const cy = c.y;
            const col = nodeColor(c);
            const isActive = isNodeActive(c, activeDetail);
            return (
              <g key={c.id} onClick={() => c.onClick && c.onClick(onOpen)}
                style={{ cursor: c.onClick ? 'pointer' : 'default' }}>
                {isActive && (
                  <circle cx={cx} cy={cy} r="8"
                    fill="none" stroke="var(--accent)" strokeWidth="1.5" opacity="0.5"/>
                )}
                <circle cx={cx} cy={cy} r="5"
                  fill={c.filled ? col : 'var(--paper)'}
                  stroke={col} strokeWidth="1.5"/>
                {c.running && (
                  <circle cx={cx} cy={cy} r="2" fill="var(--paper)"/>
                )}
              </g>
            );
          })}
        </svg>

        {/* Labels positioned absolutely so text doesn't distort the graph */}
        <div style={tlStyles.labels}>
          {commits.map(c => (
            <button key={c.id}
              onClick={() => c.onClick && c.onClick(onOpen)}
              style={{
                ...tlStyles.label,
                top: c.y - 7,
                left: laneX(c.lane) + 10,
                opacity: c.onClick ? 1 : 0.75,
                cursor: c.onClick ? 'pointer' : 'default',
                color: isNodeActive(c, activeDetail) ? 'var(--accent)' : 'var(--ink)',
              }}
              title={c.title}
            >
              <span style={tlStyles.labelText}>{c.title}</span>
            </button>
          ))}
        </div>
      </div>
      <div style={tlStyles.foot}>
        <span className="mono" style={{ color: 'var(--accent)' }}>HEAD</span>
        <span style={{ color: 'var(--ink-3)', marginLeft: 4 }}>→ agent/optimistic-mutate</span>
      </div>
    </aside>
  );
}

function nodeColor(c) {
  if (c.phase === 2) return 'var(--accent)';
  if (c.lane > 0) return 'var(--accent)';
  if (c.kind === 'queued') return 'var(--ink-3)';
  return 'var(--ink)';
}

function isNodeActive(c, detail) {
  if (!detail) return false;
  if (c.detailKind === detail.kind) {
    if (detail.kind === 'run') return c.runId === detail.data?.id;
    if (detail.kind === 'plan-step') return c.stepId === detail.data?.step?.id;
    return true;
  }
  return false;
}

/**
 * Build a commit graph that traces the agent's progress through the phases.
 * Lane 0 = main trunk. Lane 1 = execution branch (fork at Phase 2).
 */
function buildCommits() {
  let y = 14;
  const step = 30;
  const rows = [];

  const push = (o) => {
    const row = { ...o, y };
    rows.push(row);
    y += step;
    return row;
  };

  // Phase 0: intent
  const c0 = push({
    id: 'c0', lane: 0, hash: 'a81f',
    title: 'intent: confirm', ago: '10:44',
    filled: true, phase: 0,
    parents: [],
    detailKind: 'intent',
    onClick: (open) => open({ kind: 'intent', data: WORKFLOW.intent }),
  });

  // Phase 1: plan
  const c1 = push({
    id: 'c1', lane: 0, hash: 'b3d2',
    title: 'plan: exec + test', ago: '10:45',
    filled: true, phase: 1,
    parents: ['c0'],
  });

  // Phase 2: fork off main into execution branch
  const fork = push({
    id: 'c2', lane: 0, hash: 'c902',
    title: 'phase 2: start', ago: '10:46',
    filled: true, phase: 2,
    parents: ['c1'],
  });

  // Execution runs on branch lane 1
  const execPlan = WORKFLOW.plans.execution;
  const runs = WORKFLOW.runs;
  // Each run becomes a commit on lane 1
  let prevRunId = fork.id;
  let firstRun = true;
  runs.forEach((r, i) => {
    const step = execPlan.steps.find(s => s.id === r.step);
    const running = r.result === 'running';
    const c = push({
      id: `r${i}`, lane: 1,
      hash: shortHash(r.id),
      title: step ? step.label : r.step,
      ago: r.ago,
      filled: !running,
      running,
      phase: 2,
      kind: running ? 'running' : 'run',
      parents: [prevRunId],
      runId: r.id,
      stepId: r.step,
      detailKind: 'run',
      onClick: (open) => open({ kind: 'run', data: r }),
    });
    if (firstRun) {
      // Curve from trunk fork into branch on first run
      c.parents = [fork.id];
      firstRun = false;
    }
    prevRunId = c.id;
  });

  // Queued steps on branch (dashed-look via filled: false)
  const doneStepIds = new Set(runs.map(r => r.step));
  execPlan.steps
    .filter(s => !doneStepIds.has(s.id) && s.status !== 'running')
    .forEach((s, i) => {
      const c = push({
        id: `q${i}`, lane: 1,
        hash: '····',
        title: s.label,
        ago: 'queued',
        filled: false,
        phase: 2,
        kind: 'queued',
        parents: [prevRunId],
        stepId: s.id,
        detailKind: 'plan-step',
        onClick: (open) => open({ kind: 'plan-step', data: { step: s, planKind: 'execution', planId: execPlan.id } }),
      });
      prevRunId = c.id;
    });

  // Phase 3: validation (back on main)
  const validate = push({
    id: 'c3', lane: 0, hash: '····',
    title: 'validation', ago: 'pending',
    filled: false, phase: 3,
    kind: 'queued',
    parents: [fork.id], // stays on main trunk; will merge branch later
    detailKind: 'validation',
    onClick: (open) => open({ kind: 'validation' }),
  });

  // Phase 4: release
  push({
    id: 'c4', lane: 0, hash: '····',
    title: 'release', ago: 'pending',
    filled: false, phase: 4,
    kind: 'queued',
    parents: ['c3'],
    detailKind: 'release',
    onClick: (open) => open({ kind: 'release' }),
  });

  return rows;
}

function shortHash(id) {
  // id like run-11 -> deterministic short hash look
  let h = 0;
  for (let i = 0; i < id.length; i++) h = (h * 31 + id.charCodeAt(i)) >>> 0;
  return h.toString(16).slice(0, 4);
}

const tlStyles = {
  wrap: {
    width: 200, flexShrink: 0,
    borderRight: '1px solid var(--rule)',
    background: 'var(--paper-2)',
    display: 'flex', flexDirection: 'column',
    minHeight: 0,
  },
  head: {
    height: 32, flexShrink: 0,
    display: 'flex', alignItems: 'center', gap: 6,
    padding: '0 12px', color: 'var(--ink-2)',
    fontSize: 11, fontWeight: 600, letterSpacing: '0.02em',
    borderBottom: '1px solid var(--rule)',
  },
  scroll: {
    flex: 1, overflowY: 'auto', position: 'relative',
    padding: '0 0 16px 0',
  },
  labels: { position: 'absolute', top: 0, left: 0, right: 0, height: '100%' },
  label: {
    position: 'absolute',
    display: 'flex', flexDirection: 'column',
    gap: 0, padding: '2px 4px', borderRadius: 3,
    background: 'transparent', border: 'none', textAlign: 'left',
    fontSize: 11,
    maxWidth: 130,
  },
  hash: {
    fontSize: 9.5, color: 'var(--ink-3)',
    letterSpacing: '0.02em', lineHeight: 1.2,
  },
  labelText: {
    fontSize: 10.5, color: 'inherit', lineHeight: 1.2,
    whiteSpace: 'nowrap', overflow: 'hidden', textOverflow: 'ellipsis',
    maxWidth: 128,
  },
  labelTime: {
    fontSize: 9, color: 'var(--ink-3)', marginTop: 1,
  },
  foot: {
    flexShrink: 0, padding: '8px 12px',
    borderTop: '1px solid var(--rule)',
    fontSize: 10, fontFamily: "'JetBrains Mono', monospace",
    background: 'var(--paper-2)',
  },
};

window.GitTimeline = GitTimeline;
