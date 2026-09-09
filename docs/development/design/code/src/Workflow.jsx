function Workflow({ width = 660 }) {
  const [tab, setTab] = React.useState('pipeline'); // pipeline | evidence
  const [detail, setDetail] = React.useState(null);
  // detail: { kind: 'intent'|'plan-step'|'run'|'validation'|'release', data }

  return (
    <section style={{ ...wfStyles.wrap, width }}>
      <header style={wfStyles.header}>
        <div style={{ display: 'flex', gap: 2 }}>
          <TabBtn active={tab === 'pipeline'} onClick={() => setTab('pipeline')}>
            <IconGit size={13}/> Workflow
          </TabBtn>
          <TabBtn active={tab === 'summary'} onClick={() => setTab('summary')}>
            <IconCheck size={13}/> Summary
          </TabBtn>
          <TabBtn active={tab === 'diff'} onClick={() => setTab('diff')}>
            <IconDiff size={13}/> Diff
          </TabBtn>
          <TabBtn active={tab === 'objects'} onClick={() => setTab('objects')}>
            <IconDatabase size={13}/> Objects
          </TabBtn>
        </div>
        <div style={{ display: 'flex', alignItems: 'center', gap: 6, color: 'var(--ink-3)' }}>
          <span style={wfStyles.tokenPill} title="Tokens consumed in this thread">
            <svg width="11" height="11" viewBox="0 0 24 24" fill="none" stroke="currentColor" strokeWidth="2" strokeLinecap="round" strokeLinejoin="round">
              <circle cx="12" cy="12" r="3"/>
              <path d="M12 2v3M12 19v3M2 12h3M19 12h3M5 5l2 2M17 17l2 2M5 19l2-2M17 7l2-2"/>
            </svg>
            <span className="mono">48.2k</span>
            <span style={wfStyles.tokenUnit}>Token</span>
          </span>
        </div>
      </header>

      <div style={{ flex: 1, display: 'flex', minHeight: 0, overflow: 'hidden' }}>
        {tab === 'pipeline' && <GitTimeline onOpen={setDetail} activeDetail={detail}/>}
        <div style={wfStyles.scroll}>
          {tab === 'pipeline' && <PipelineView onOpen={setDetail} activeDetail={detail}/>}
          {tab === 'summary'  && <SummaryView/>}
          {tab === 'diff'     && <ReviewView/>}
          {tab === 'objects'  && <ObjectsView onOpen={setDetail} activeDetail={detail}/>}
        </div>
      </div>

      <footer style={wfStyles.footer}>
        <div style={{ fontSize: 11, color: 'var(--ink-3)' }}>
          <span className="mono">thread-t1</span> · 5 events · 2 PatchSets
        </div>
        <div style={{ display: 'flex', gap: 6 }}>
          <button style={wfStyles.fBtn}>Pause</button>
          <button style={wfStyles.fBtnPrimary}>
            <IconPlay size={11}/> Continue
          </button>
        </div>
      </footer>

      <DetailPanel detail={detail} onClose={() => setDetail(null)}/>
    </section>
  );
}

function TabBtn({ active, onClick, children }) {
  return (
    <button onClick={onClick} style={{
      display: 'flex', alignItems: 'center', gap: 6,
      padding: '6px 10px', fontSize: 12, fontWeight: 500,
      color: active ? 'var(--ink)' : 'var(--ink-3)',
      borderBottom: `1.5px solid ${active ? 'var(--ink)' : 'transparent'}`,
      marginBottom: -1,
    }}>{children}</button>
  );
}

function PipelineView({ onOpen, activeDetail }) {
  return (
    <div>
      <PhaseStrip current={WORKFLOW.currentPhase}/>
      <IntentCard intent={WORKFLOW.intent} onOpen={onOpen} activeDetail={activeDetail}/>
      <PlanCard
        phaseBadge="Phase 1 · Exec"
        title="Execution Plan"
        subtitle={WORKFLOW.plans.execution.id}
        icon={<IconSpark size={12}/>}
        plan={WORKFLOW.plans.execution}
        planKind="execution"
        active
        onOpen={onOpen}
        activeDetail={activeDetail}
      />
      <PlanCard
        phaseBadge="Phase 1 · Test"
        title="Test Plan"
        subtitle={WORKFLOW.plans.test.id}
        icon={<IconFlask size={12}/>}
        plan={WORKFLOW.plans.test}
        planKind="test"
        gated
        onOpen={onOpen}
        activeDetail={activeDetail}
      />
      <RunsCard runs={WORKFLOW.runs} onOpen={onOpen} activeDetail={activeDetail}/>
      <ValidationCard onOpen={onOpen} activeDetail={activeDetail}/>
      <ReleaseCard onOpen={onOpen} activeDetail={activeDetail}/>
    </div>
  );
}

function PhaseStrip({ current }) {
  return (
    <div style={wfStyles.phaseStrip}>
      {PHASES.map((p, i) => {
        const done = i < current;
        const active = i === current;
        return (
          <React.Fragment key={p.n}>
            <div style={wfStyles.phaseCol}>
              <div style={{
                ...wfStyles.phaseDot,
                background: done ? 'var(--ink)' : active ? 'var(--accent)' : 'var(--paper)',
                borderColor: done ? 'var(--ink)' : active ? 'var(--accent)' : 'var(--rule-2)',
                color: done || active ? 'white' : 'var(--ink-3)',
              }}>
                {done ? <IconCheck size={10} sw={2.5}/> : p.n}
              </div>
              <div style={{
                fontSize: 10.5, fontWeight: active ? 600 : 500,
                color: active ? 'var(--ink)' : done ? 'var(--ink-2)' : 'var(--ink-3)',
                marginTop: 6, letterSpacing: '-0.01em',
              }}>{p.name}</div>
              <div style={{ fontSize: 9.5, color: 'var(--ink-3)', marginTop: 1 }}>{p.blurb}</div>
            </div>
            {i < PHASES.length - 1 && (
              <div style={{
                flex: 1, height: 1, marginTop: 10,
                background: done ? 'var(--ink)' : 'var(--rule)',
              }}/>
            )}
          </React.Fragment>
        );
      })}
    </div>
  );
}

function Card({ badge, title, subtitle, icon, children, tone = 'default', defaultOpen = true, onClickHead, selected }) {
  const [open, setOpen] = React.useState(defaultOpen);
  const tones = {
    default: { bg: 'var(--paper)', border: 'var(--rule)' },
    active:  { bg: 'var(--paper)', border: 'var(--accent-line)' },
    muted:   { bg: 'var(--paper-2)', border: 'var(--rule)' },
  };
  const t = tones[tone] || tones.default;
  return (
    <div style={{
      border: `1px solid ${selected ? 'var(--accent)' : t.border}`,
      background: t.bg,
      borderRadius: 8, marginBottom: 10, overflow: 'hidden',
      transition: 'border-color 120ms',
    }}>
      <div style={{ display: 'flex', alignItems: 'stretch' }}>
        <button onClick={() => setOpen(o => !o)} style={{
          ...wfStyles.cardHead, flex: 1, borderRight: onClickHead ? '1px solid var(--rule)' : 'none',
        }}>
          <div style={{ display: 'flex', alignItems: 'center', gap: 8, minWidth: 0 }}>
            {badge && <span style={wfStyles.phaseBadge}>{badge}</span>}
            {icon && <span style={{ color: 'var(--ink-3)' }}>{icon}</span>}
            <span style={{ fontWeight: 600, fontSize: 12.5 }}>{title}</span>
            {subtitle && <span className="mono" style={{ fontSize: 10.5, color: 'var(--ink-3)' }}>{subtitle}</span>}
          </div>
          <div style={{ color: 'var(--ink-3)', transform: open ? 'rotate(90deg)' : 'none', transition: 'transform 120ms' }}>
            <IconChev size={14}/>
          </div>
        </button>
        {onClickHead && (
          <button onClick={onClickHead} style={wfStyles.cardDetailBtn} title="Open details">
            <IconArrow size={12}/>
          </button>
        )}
      </div>
      {open && <div style={wfStyles.cardBody}>{children}</div>}
    </div>
  );
}

function IntentCard({ intent, onOpen, activeDetail }) {
  const selected = activeDetail?.kind === 'intent';
  return (
    <Card
      badge="Phase 0"
      title="Intent"
      subtitle={intent.revision + ' · confirmed'}
      icon={<IconSpark size={12}/>}
      onClickHead={() => onOpen({ kind: 'intent', data: intent })}
      selected={selected}
    >
      <button onClick={() => onOpen({ kind: 'intent', data: intent })} style={wfStyles.intentThumb}>
        <div style={{ fontWeight: 500, fontSize: 13, marginBottom: 4 }}>{intent.title}</div>
        <div style={{
          color: 'var(--ink-2)', fontSize: 12.5, lineHeight: 1.5,
          display: '-webkit-box', WebkitLineClamp: 2, WebkitBoxOrient: 'vertical',
          overflow: 'hidden',
        }}>
          {intent.summary}
        </div>
        <div style={wfStyles.viewMore}>
          View details <IconArrow size={11}/>
        </div>
      </button>
    </Card>
  );
}

function PlanCard({ phaseBadge, title, subtitle, icon, plan, planKind, active, gated, onOpen, activeDetail }) {
  return (
    <Card
      badge={phaseBadge}
      title={title}
      subtitle={`${subtitle} · ${plan.steps.length} steps`}
      icon={icon}
      tone={active ? 'active' : gated ? 'muted' : 'default'}
    >
      {gated && (
        <div style={wfStyles.gateBanner}>
          <IconClock size={11}/> Stage barrier — runs after execution DAG settles
        </div>
      )}
      <ol style={{ margin: 0, padding: 0, listStyle: 'none' }}>
        {plan.steps.map((s, i) => {
          const selected = activeDetail?.kind === 'plan-step' && activeDetail.data?.step?.id === s.id;
          return (
            <StepRow
              key={s.id}
              step={s}
              i={i + 1}
              last={i === plan.steps.length - 1}
              selected={selected}
              onClick={() => onOpen({ kind: 'plan-step', data: { step: s, planKind, planId: plan.id } })}
            />
          );
        })}
      </ol>
    </Card>
  );
}

function StepRow({ step, i, last, onClick, selected }) {
  return (
    <li style={{ display: 'flex', gap: 10, position: 'relative' }}>
      <div style={{ position: 'relative', flexShrink: 0, width: 18, display: 'flex', flexDirection: 'column', alignItems: 'center' }}>
        <div style={{
          width: 18, height: 18, borderRadius: '50%',
          background: 'var(--paper)', color: 'var(--ink-3)',
          display: 'grid', placeItems: 'center',
          border: '1px solid var(--rule-2)', marginTop: 2,
          fontSize: 9, fontFamily: "'JetBrains Mono', monospace", fontWeight: 600,
        }}>
          {i}
        </div>
        {!last && <div style={{ flex: 1, width: 1, background: 'var(--rule)', marginTop: 2 }}/>}
      </div>
      <button onClick={onClick} style={{
        flex: 1, textAlign: 'left',
        background: selected ? 'var(--accent-soft)' : 'transparent',
        borderRadius: 4,
        padding: selected ? '4px 6px' : '2px 0',
        marginLeft: selected ? -4 : 0,
        marginBottom: last ? 0 : 6,
        transition: 'background 120ms',
      }}>
        <div style={{ fontSize: 12.5, color: 'var(--ink)', lineHeight: 1.4 }}>
          {step.label}
        </div>
      </button>
    </li>
  );
}

function statusMeta(status) {
  switch (status) {
    case 'done':    return { icon: <IconCheck size={10} sw={2.5}/>, color: 'var(--good)',  bg: 'var(--good-soft)', label: 'DONE' };
    case 'running': return { icon: <Spinner/>,                      color: 'var(--accent)', bg: 'var(--accent-soft)', label: 'RUNNING' };
    case 'failed':  return { icon: <IconX size={10} sw={2.5}/>,     color: 'var(--bad)',   bg: 'var(--bad-soft)',  label: 'FAILED' };
    default:        return { icon: <span style={{ width: 5, height: 5, borderRadius: '50%', background: 'var(--ink-3)' }}/>, color: 'var(--ink-3)', bg: 'var(--paper-2)', label: 'QUEUED' };
  }
}

function Spinner() {
  return (
    <span style={{
      width: 9, height: 9, borderRadius: '50%',
      border: '1.5px solid currentColor', borderTopColor: 'transparent',
      animation: 'spin 0.8s linear infinite', display: 'inline-block',
    }}/>
  );
}

function RunsCard({ runs, onOpen, activeDetail }) {
  const execPlan = WORKFLOW.plans.execution;
  // Group runs by step id
  const runsByStep = {};
  runs.forEach(r => {
    (runsByStep[r.step] = runsByStep[r.step] || []).push(r);
  });

  return (
    <Card badge="Phase 2" title="Execution Runs" subtitle={`${execPlan.id} · stage-gated DAG`} icon={<IconPlay size={12}/>} tone="active" defaultOpen>
      <div style={{ display: 'flex', flexDirection: 'column', gap: 10 }}>
        {execPlan.steps.map((step, i) => {
          const stepRuns = runsByStep[step.id] || [];
          return (
            <StepRunGroup
              key={step.id}
              step={step}
              i={i + 1}
              runs={stepRuns}
              onOpen={onOpen}
              activeDetail={activeDetail}
            />
          );
        })}
      </div>
    </Card>
  );
}

function StepRunGroup({ step, i, runs, onOpen, activeDetail }) {
  const meta = statusMeta(step.status);
  return (
    <div style={wfStyles.stepGroup}>
      <div style={wfStyles.stepGroupHead}>
        <div style={{
          width: 18, height: 18, borderRadius: '50%',
          background: meta.bg, color: meta.color,
          display: 'grid', placeItems: 'center',
          border: `1px solid ${meta.color}`, flexShrink: 0,
        }}>{meta.icon}</div>
        <span className="mono" style={{ fontSize: 10.5, color: 'var(--ink-3)', flexShrink: 0 }}>
          {step.id}
        </span>
        <span style={{ fontSize: 12, color: 'var(--ink)', flex: 1, minWidth: 0, whiteSpace: 'nowrap', overflow: 'hidden', textOverflow: 'ellipsis' }}>
          {step.label}
        </span>
        <span style={{
          fontSize: 9.5, fontFamily: "'JetBrains Mono', monospace",
          color: meta.color, letterSpacing: '0.05em',
          padding: '1px 6px', borderRadius: 3, background: meta.bg,
          flexShrink: 0,
        }}>{meta.label}</span>
      </div>
      {runs.length > 0 ? (
        <div style={{ display: 'flex', flexDirection: 'column', gap: 4, paddingLeft: 26 }}>
          {runs.map(r => {
            const selected = activeDetail?.kind === 'run' && activeDetail.data?.id === r.id;
            return (
              <button
                key={r.id}
                onClick={() => onOpen({ kind: 'run', data: r })}
                style={{
                  ...wfStyles.runRow,
                  borderColor: selected ? 'var(--accent)' : 'var(--rule)',
                  background: selected ? 'var(--accent-soft)' : 'var(--paper)',
                }}
              >
                <span className="mono" style={{ fontSize: 10.5, color: 'var(--ink-3)', width: 50, textAlign: 'left' }}>{r.id}</span>
                <span style={{
                  fontSize: 10, fontFamily: "'JetBrains Mono', monospace",
                  padding: '1px 6px', borderRadius: 3,
                  background: r.result === 'pass' ? 'var(--good-soft)' : r.result === 'running' ? 'var(--accent-soft)' : 'var(--bad-soft)',
                  color: r.result === 'pass' ? 'var(--good)' : r.result === 'running' ? 'var(--accent)' : 'var(--bad)',
                }}>{r.result.toUpperCase()}</span>
                <span className="mono" style={{ fontSize: 10.5, color: 'var(--ink-2)', marginLeft: 'auto' }}>{r.patch}</span>
                <span style={{ fontSize: 10.5, color: 'var(--ink-3)', width: 30, textAlign: 'right' }}>{r.ago}</span>
                <IconArrow size={11} style={{ color: 'var(--ink-3)' }}/>
              </button>
            );
          })}
        </div>
      ) : (
        <div style={{ paddingLeft: 26, fontSize: 11, color: 'var(--ink-3)', fontStyle: 'italic' }}>
          no runs yet
        </div>
      )}
    </div>
  );
}

function ValidationCard({ onOpen, activeDetail }) {
  const selected = activeDetail?.kind === 'validation';
  return (
    <Card
      badge="Phase 3"
      title="Validation"
      icon={<IconShield size={12}/>}
      tone="muted"
      defaultOpen={false}
      onClickHead={() => onOpen({ kind: 'validation' })}
      selected={selected}
    >
      <div style={{ color: 'var(--ink-3)', fontSize: 12 }}>
        Waiting for execution DAG to settle, then SAST / SCA / compatibility checks will run and emit Evidence records.
      </div>
    </Card>
  );
}

function ReleaseCard({ onOpen, activeDetail }) {
  const selected = activeDetail?.kind === 'release';
  return (
    <Card
      badge="Phase 4"
      title="Release"
      icon={<IconBranch size={12}/>}
      tone="muted"
      defaultOpen={false}
      onClickHead={() => onOpen({ kind: 'release' })}
      selected={selected}
    >
      <div style={{ color: 'var(--ink-3)', fontSize: 12 }}>
        Low-risk → auto-merge. High-risk → human review. Decision & IntentEvent recorded.
      </div>
    </Card>
  );
}

function EvidenceView() {
  return (
    <div>
      <div style={{ fontSize: 10.5, letterSpacing: '0.08em', textTransform: 'uppercase', color: 'var(--ink-3)', margin: '4px 2px 10px' }}>
        Append-only execution facts
      </div>
      {WORKFLOW.evidence.map((e, i) => (
        <div key={i} style={wfStyles.evRow}>
          <span className="mono" style={{ fontSize: 10, color: 'var(--ink-3)', width: 52 }}>
            {e.kind === 'tool' ? 'TOOL' : e.kind === 'frame' ? 'FRAME' : 'PATCH'}
          </span>
          <div style={{ flex: 1, minWidth: 0 }}>
            <div style={{ fontSize: 12.5, color: 'var(--ink)', whiteSpace: 'nowrap', overflow: 'hidden', textOverflow: 'ellipsis' }}>
              {e.label}
            </div>
            <div className="mono" style={{ fontSize: 10.5, color: 'var(--ink-3)' }}>{e.meta}</div>
          </div>
        </div>
      ))}
    </div>
  );
}

/* ------------------- Detail Panel ------------------- */

function DetailPanel({ detail, onClose }) {
  // Keep the last detail around while closing so content doesn't vanish mid-animation.
  const [mounted, setMounted] = React.useState(!!detail);
  const [shown, setShown] = React.useState(false);
  const [current, setCurrent] = React.useState(detail);

  React.useEffect(() => {
    if (detail) {
      setCurrent(detail);
      setMounted(true);
      // Flip to shown on a timer so the transform animates from off-screen.
      // (setTimeout fires even when rAF is throttled in backgrounded iframes.)
      const id = setTimeout(() => setShown(true), 20);
      return () => clearTimeout(id);
    } else {
      setShown(false);
      const t = setTimeout(() => setMounted(false), 240);
      return () => clearTimeout(t);
    }
  }, [detail]);

  if (!mounted) return null;

  return (
    <>
      <div
        onClick={onClose}
        style={{
          position: 'absolute', inset: 0,
          background: 'rgba(20,18,14,0.12)',
          opacity: shown ? 1 : 0,
          transition: 'opacity 180ms ease',
          zIndex: 10,
        }}
      />
      <div style={{
        position: 'absolute', top: 0, right: 0, bottom: 0,
        width: 400, background: 'var(--paper)',
        borderLeft: '1px solid var(--rule-2)',
        boxShadow: '-12px 0 30px -12px rgba(0,0,0,0.12)',
        transform: shown ? 'translateX(0px)' : 'translateX(400px)',
        transition: 'transform 240ms cubic-bezier(0.22, 0.61, 0.36, 1)',
        zIndex: 11,
        display: 'flex', flexDirection: 'column',
      }}>
        {current && <DetailContent detail={current} onClose={onClose}/>}
      </div>
    </>
  );
}

function DetailContent({ detail, onClose }) {
  const meta = detailMeta(detail);
  return (
    <>
      <header style={dpStyles.head}>
        <div style={{ display: 'flex', alignItems: 'center', gap: 8, minWidth: 0 }}>
          <span style={wfStyles.phaseBadge}>{meta.badge}</span>
          <span style={{ fontWeight: 600, fontSize: 13 }}>{meta.title}</span>
          {meta.subtitle && (
            <span className="mono" style={{ fontSize: 10.5, color: 'var(--ink-3)' }}>{meta.subtitle}</span>
          )}
        </div>
        <button onClick={onClose} style={wfStyles.iconBtn} title="Close">
          <IconX size={14}/>
        </button>
      </header>
      <div style={dpStyles.body}>
        {detail.kind === 'intent'      && <IntentDetail intent={detail.data}/>}
        {detail.kind === 'plan-step'   && <PlanStepDetail data={detail.data}/>}
        {detail.kind === 'run'         && <RunDetail run={detail.data}/>}
        {detail.kind === 'validation'  && <ValidationDetail/>}
        {detail.kind === 'release'     && <ReleaseDetail/>}
        {detail.kind === 'object'      && <ObjectDetail obj={detail.data}/>}
      </div>
    </>
  );
}

function detailMeta(d) {
  switch (d.kind) {
    case 'intent':     return { badge: 'Phase 0', title: 'Intent', subtitle: d.data.revision };
    case 'plan-step':  return { badge: 'Phase 1', title: d.data.planKind === 'test' ? 'Test step' : 'Execution step', subtitle: d.data.step.id };
    case 'run':        return { badge: 'Phase 2', title: 'Run', subtitle: d.data.id };
    case 'validation': return { badge: 'Phase 3', title: 'Validation', subtitle: 'audit' };
    case 'release':    return { badge: 'Phase 4', title: 'Release', subtitle: 'decision' };
    case 'object':     return { badge: (d.data.type || 'object').toUpperCase(), title: d.data.name || d.data.oid, subtitle: d.data.oid };
    default:           return { badge: '', title: '' };
  }
}

function Section({ label, children, mono }) {
  return (
    <div style={{ marginBottom: 18 }}>
      <div style={dpStyles.sectionLabel}>{label}</div>
      <div style={mono ? dpStyles.monoBlock : undefined}>{children}</div>
    </div>
  );
}

function KV({ k, v }) {
  return (
    <div style={{ display: 'flex', padding: '5px 0', borderBottom: '1px solid var(--rule)', fontSize: 12 }}>
      <span style={{ color: 'var(--ink-3)', width: 110, flexShrink: 0 }}>{k}</span>
      <span className="mono" style={{ color: 'var(--ink)', flex: 1, fontSize: 11.5 }}>{v}</span>
    </div>
  );
}

function IntentDetail({ intent }) {
  const md = [
    `# ${intent.title}`,
    '',
    intent.summary,
    '',
    '## Constraints',
    '',
    ...intent.constraints.map(c => `- ${c}`),
    '',
    '## Context',
    '',
    `The caller surface is \`useMutation<T>\` in \`src/hooks/useMutation.ts\`, which today awaits \`fetcher(input)\` before touching the cache. Subscribers don't see the write until the round-trip finishes, so the UI feels sluggish on slow links.`,
    '',
    '## Approach',
    '',
    `1. Snapshot the cache entry under a per-key revision counter before the mutation fires.`,
    `2. Apply the optimistic patch synchronously so subscribers rerender immediately.`,
    `3. On success, reconcile the server response against the snapshot's revision — if a concurrent write landed first, keep the newer value.`,
    `4. On error, roll back to the snapshot and rethrow to \`onError\`.`,
    '',
    '## Out of scope',
    '',
    `- Changes to \`MutationOptions<T>\`'s public shape beyond adding an optional \`optimistic\` field.`,
    `- Server-driven cache invalidation — that stays in \`queryClient.invalidate\`.`,
  ].join('\n');

  return <Markdown source={md}/>;
}

/* ------------------- Minimal Markdown ------------------- */
// Supports: # / ## / ###, paragraphs, - bullets, 1. ordered lists,
// `inline code`, **bold**, *italic*. Enough for intent docs.

function Markdown({ source }) {
  const blocks = React.useMemo(() => parseMarkdown(source), [source]);
  return <div style={mdStyles.root}>{blocks.map((b, i) => renderBlock(b, i))}</div>;
}

function parseMarkdown(src) {
  const lines = src.split('\n');
  const out = [];
  let i = 0;
  while (i < lines.length) {
    const line = lines[i];
    if (!line.trim()) { i++; continue; }

    const h = /^(#{1,3})\s+(.*)$/.exec(line);
    if (h) { out.push({ type: 'h', level: h[1].length, text: h[2] }); i++; continue; }

    if (/^\s*-\s+/.test(line)) {
      const items = [];
      while (i < lines.length && /^\s*-\s+/.test(lines[i])) {
        items.push(lines[i].replace(/^\s*-\s+/, ''));
        i++;
      }
      out.push({ type: 'ul', items });
      continue;
    }

    if (/^\s*\d+\.\s+/.test(line)) {
      const items = [];
      while (i < lines.length && /^\s*\d+\.\s+/.test(lines[i])) {
        items.push(lines[i].replace(/^\s*\d+\.\s+/, ''));
        i++;
      }
      out.push({ type: 'ol', items });
      continue;
    }

    // paragraph: accumulate until blank or structural line
    const para = [line];
    i++;
    while (i < lines.length && lines[i].trim() && !/^(#{1,3}\s|\s*-\s|\s*\d+\.\s)/.test(lines[i])) {
      para.push(lines[i]);
      i++;
    }
    out.push({ type: 'p', text: para.join(' ') });
  }
  return out;
}

function renderBlock(b, key) {
  if (b.type === 'h') {
    const Tag = `h${b.level}`;
    return <Tag key={key} style={mdStyles[`h${b.level}`]}>{renderInline(b.text)}</Tag>;
  }
  if (b.type === 'p') {
    return <p key={key} style={mdStyles.p}>{renderInline(b.text)}</p>;
  }
  if (b.type === 'ul') {
    return (
      <ul key={key} style={mdStyles.ul}>
        {b.items.map((it, i) => <li key={i} style={mdStyles.li}>{renderInline(it)}</li>)}
      </ul>
    );
  }
  if (b.type === 'ol') {
    return (
      <ol key={key} style={mdStyles.ol}>
        {b.items.map((it, i) => <li key={i} style={mdStyles.li}>{renderInline(it)}</li>)}
      </ol>
    );
  }
  return null;
}

// Inline: `code`, **bold**, *italic*
function renderInline(text) {
  const parts = [];
  const regex = /(`[^`]+`)|(\*\*[^*]+\*\*)|(\*[^*]+\*)/g;
  let last = 0, m, k = 0;
  while ((m = regex.exec(text)) !== null) {
    if (m.index > last) parts.push(text.slice(last, m.index));
    const tok = m[0];
    if (tok.startsWith('`')) {
      parts.push(<code key={k++} style={mdStyles.code}>{tok.slice(1, -1)}</code>);
    } else if (tok.startsWith('**')) {
      parts.push(<strong key={k++} style={mdStyles.strong}>{tok.slice(2, -2)}</strong>);
    } else {
      parts.push(<em key={k++}>{tok.slice(1, -1)}</em>);
    }
    last = m.index + tok.length;
  }
  if (last < text.length) parts.push(text.slice(last));
  return parts;
}

function PlanStepDetail({ data }) {
  const { step, planKind, planId } = data;
  const { label, color } = statusMeta(step.status);
  return (
    <>
      <div style={{ fontSize: 15, fontWeight: 600, letterSpacing: '-0.01em', marginBottom: 6 }}>
        {step.label}
      </div>
      <div style={{ marginBottom: 18 }}>
        <span style={{
          fontSize: 10, fontFamily: "'JetBrains Mono', monospace",
          color, background: `color-mix(in oklch, ${color} 12%, var(--paper))`,
          padding: '3px 8px', borderRadius: 3, letterSpacing: '0.05em',
        }}>{label}</span>
      </div>

      <Section label="Metadata">
        <KV k="Step ID"    v={step.id}/>
        <KV k="Plan"       v={planId}/>
        <KV k="Kind"       v={planKind === 'test' ? 'test' : 'execution'}/>
        <KV k="Status"     v={step.status}/>
      </Section>

      <Section label="Purpose">
        <div style={{ fontSize: 12.5, color: 'var(--ink-2)', lineHeight: 1.55 }}>
          {planKind === 'test'
            ? 'Verification step — asserts behavior after the execution DAG settles. Failures route back into a new plan revision.'
            : 'Execution step — mutates cache/code inside the sandbox. Output is captured as an append-only PatchSet bound to the parent plan.'}
        </div>
      </Section>

      {step.status !== 'queued' && (
        <Section label="Tool calls">
          <ToolCall name="read" arg="src/lib/query.ts" result="214 lines"/>
          <ToolCall name="edit" arg="src/lib/query.ts" result="patchset ps-07"/>
          {step.status === 'running' && <ToolCall name="test" arg="useMutation.test.ts" result="running…" running/>}
        </Section>
      )}

      <Section label="Sibling steps">
        <div style={{ fontSize: 12, color: 'var(--ink-3)' }}>
          Linked into the plan DAG. Downstream gates won't open until this node reports DONE.
        </div>
      </Section>
    </>
  );
}

function RunDetail({ run }) {
  const { label, color } = statusMeta(run.result === 'pass' ? 'done' : run.result === 'running' ? 'running' : 'failed');
  return (
    <>
      <div style={{ display: 'flex', alignItems: 'center', gap: 10, marginBottom: 12 }}>
        <div style={{ fontSize: 15, fontWeight: 600 }} className="mono">{run.id}</div>
        <span style={{
          fontSize: 10, fontFamily: "'JetBrains Mono', monospace",
          color, background: `color-mix(in oklch, ${color} 12%, var(--paper))`,
          padding: '3px 8px', borderRadius: 3, letterSpacing: '0.05em',
        }}>{label}</span>
      </div>

      <Section label="Metadata">
        <KV k="Run ID"      v={run.id}/>
        <KV k="Step"        v={run.step}/>
        <KV k="Result"      v={run.result}/>
        <KV k="Patch"       v={run.patch}/>
        <KV k="Finished"    v={run.ago}/>
        <KV k="Sandbox"     v="libra-sbx-04 · rw"/>
      </Section>

      <Section label="Output" mono>
        <pre style={dpStyles.pre}>{`$ cargo test --lib optimistic
   Compiling libra-cache v0.3.1
    Finished test [unoptimized + debuginfo]
     Running tests/useMutation.test.ts
  ✓ snapshot captures prior cache state
  ✓ optimistic patch visible synchronously
  ${run.result === 'running' ? '… revision-guarded rollback' : '✓ revision-guarded rollback'}
  ${run.result === 'pass' ? 'ok. 3 passed; 0 failed' : ''}`}</pre>
      </Section>

      <Section label="Patch">
        <div style={dpStyles.diffFile}>
          <div style={dpStyles.diffHead}>src/lib/query.ts · <span className="mono" style={{ color: 'var(--ink-3)' }}>{run.patch}</span></div>
          <pre style={dpStyles.pre}>{`@@ useMutation ()
- const result = await fetcher(input);
- cache.set(key, result);
+ const snap = cache.snapshot(key);
+ cache.patch(key, optimistic);
+ try {
+   const result = await fetcher(input);
+   cache.reconcile(key, snap.rev, result);
+ } catch (err) {
+   cache.rollback(key, snap);
+   throw err;
+ }`}</pre>
        </div>
      </Section>
    </>
  );
}

function ValidationDetail() {
  return (
    <>
      <div style={{ fontSize: 15, fontWeight: 600, marginBottom: 10 }}>Validation gate</div>
      <div style={{ color: 'var(--ink-2)', fontSize: 12.5, lineHeight: 1.6, marginBottom: 18 }}>
        Phase 3 runs after the execution DAG settles. It audits the resulting PatchSet against policy and collects the evidence needed for release.
      </div>

      <Section label="Checks">
        <CheckRow name="SAST · static analysis"      status="queued"/>
        <CheckRow name="SCA · dependency advisories" status="queued"/>
        <CheckRow name="Type-check"                  status="queued"/>
        <CheckRow name="Test plan · full run"        status="queued"/>
        <CheckRow name="Compatibility · API surface" status="queued"/>
      </Section>

      <Section label="Output">
        <div style={{ fontSize: 12, color: 'var(--ink-3)', lineHeight: 1.6 }}>
          Each check appends an Evidence record (kind = <span className="mono">audit</span>) to the thread's
          append-only log. The aggregate verdict determines whether Release auto-merges or escalates to human review.
        </div>
      </Section>
    </>
  );
}

function ReleaseDetail() {
  return (
    <>
      <div style={{ fontSize: 15, fontWeight: 600, marginBottom: 10 }}>Release decision</div>
      <div style={{ color: 'var(--ink-2)', fontSize: 12.5, lineHeight: 1.6, marginBottom: 18 }}>
        Phase 4 is the final decision. Libra classifies the PatchSet by risk, then either auto-merges or requests human review — producing a signed IntentEvent either way.
      </div>

      <Section label="Risk classification">
        <KV k="Policy"       v="web3infra/default"/>
        <KV k="Surface"      v="internal hook · 2 callers"/>
        <KV k="Blast radius" v="low"/>
        <KV k="Reversibility" v="clean revert"/>
      </Section>

      <Section label="Path">
        <div style={{ display: 'flex', gap: 6, alignItems: 'center', fontSize: 12.5, marginBottom: 8 }}>
          <span style={{ padding: '2px 8px', borderRadius: 3, background: 'var(--good-soft)', color: 'var(--good)', fontFamily: "'JetBrains Mono', monospace", fontSize: 10.5 }}>LOW</span>
          <IconArrow size={11} style={{ color: 'var(--ink-3)' }}/>
          <span>Auto-merge to <span className="mono">main</span></span>
        </div>
        <div style={{ display: 'flex', gap: 6, alignItems: 'center', fontSize: 12.5, color: 'var(--ink-3)' }}>
          <span style={{ padding: '2px 8px', borderRadius: 3, background: 'var(--warn-soft)', color: 'var(--warn)', fontFamily: "'JetBrains Mono', monospace", fontSize: 10.5 }}>HIGH</span>
          <IconArrow size={11}/>
          <span>Open review for erin@web3infra</span>
        </div>
      </Section>

      <Section label="Output">
        <div style={{ fontSize: 12, color: 'var(--ink-3)', lineHeight: 1.6 }}>
          Decision is sealed as an <span className="mono">IntentEvent</span> on the thread and mirrored to the git provider. No phase can run past Release without a decision record.
        </div>
      </Section>
    </>
  );
}

function ObjectDetail({ obj }) {
  const body = OBJECTS.bodies[obj.oid];
  const type = obj.type || 'object';
  const typeColor = type === 'commit' ? 'var(--accent)'
    : type === 'tree' ? 'var(--warn)'
    : type === 'tag' ? 'var(--good)' : 'var(--ink-3)';
  return (
    <>
      <div style={{ display: 'flex', alignItems: 'center', gap: 10, marginBottom: 12 }}>
        <span style={{
          fontFamily: "'JetBrains Mono', monospace", fontSize: 10,
          color: typeColor, background: `color-mix(in oklch, ${typeColor} 12%, var(--paper))`,
          padding: '3px 8px', borderRadius: 3, letterSpacing: '0.05em', textTransform: 'uppercase', fontWeight: 600,
        }}>{type}</span>
        <div className="mono" style={{ fontSize: 15, fontWeight: 600 }}>{obj.oid}</div>
      </div>

      <Section label="Metadata">
        <KV k="OID"   v={obj.oid}/>
        <KV k="Type"  v={type}/>
        {obj.name && <KV k="Name" v={obj.name}/>}
        {obj.size && <KV k="Size" v={obj.size}/>}
        {obj.entries != null && <KV k="Entries" v={String(obj.entries)}/>}
        {obj.tree && <KV k="Tree" v={obj.tree}/>}
        {obj.parents && obj.parents.length > 0 && <KV k="Parent" v={obj.parents.join(', ')}/>}
        {obj.author && <KV k="Author" v={obj.author}/>}
        {obj.when && <KV k="When" v={obj.when}/>}
      </Section>

      {obj.refs && obj.refs.length > 0 && (
        <Section label="Refs">
          <div style={{ display: 'flex', gap: 6, flexWrap: 'wrap' }}>
            {obj.refs.map(r => (
              <span key={r} className="mono" style={{
                fontSize: 10.5, padding: '2px 8px', borderRadius: 999,
                color: 'var(--accent)', background: 'var(--accent-soft)',
                border: '1px solid var(--accent-line)',
              }}>{r}</span>
            ))}
          </div>
        </Section>
      )}

      <Section label={`libra cat-file -p ${obj.oid}`} mono>
        <pre style={dpStyles.pre}>{body || '(binary or unloaded object — body not cached)'}</pre>
      </Section>
    </>
  );
}

function ToolCall({ name, arg, result, running }) {
  return (
    <div style={dpStyles.toolRow}>
      <span className="mono" style={{ fontSize: 10.5, color: 'var(--accent)', fontWeight: 600 }}>{name}</span>
      <span className="mono" style={{ fontSize: 11, color: 'var(--ink)', flex: 1, overflow: 'hidden', textOverflow: 'ellipsis', whiteSpace: 'nowrap' }}>{arg}</span>
      <span className="mono" style={{ fontSize: 10.5, color: running ? 'var(--accent)' : 'var(--ink-3)' }}>
        {running ? <Spinner/> : null} {result}
      </span>
    </div>
  );
}

function CheckRow({ name, status }) {
  const m = statusMeta(status);
  return (
    <div style={{ display: 'flex', alignItems: 'center', gap: 8, padding: '7px 0', borderBottom: '1px solid var(--rule)' }}>
      <div style={{
        width: 16, height: 16, borderRadius: '50%', background: m.bg, color: m.color,
        display: 'grid', placeItems: 'center', border: `1px solid ${m.color}`,
      }}>{m.icon}</div>
      <span style={{ fontSize: 12.5, flex: 1 }}>{name}</span>
      <span className="mono" style={{ fontSize: 10, color: m.color }}>{m.label}</span>
    </div>
  );
}

function Timeline({ items }) {
  return (
    <ol style={{ margin: 0, padding: 0, listStyle: 'none' }}>
      {items.map((it, i) => (
        <li key={i} style={{ display: 'flex', gap: 10, padding: '4px 0', fontSize: 12 }}>
          <span className="mono" style={{ color: 'var(--ink-3)', width: 44, fontSize: 10.5 }}>{it.t}</span>
          <span style={{ color: 'var(--ink-2)', flex: 1 }}>{it.body}</span>
        </li>
      ))}
    </ol>
  );
}

/* ------------------- Styles ------------------- */

const wfStyles = {
  wrap: {
    flexShrink: 0, display: 'flex', flexDirection: 'column',
    borderLeft: '1px solid var(--rule)', background: 'var(--paper)',
    minWidth: 0, position: 'relative', overflow: 'hidden',
  },
  header: {
    height: 48, flexShrink: 0, display: 'flex', alignItems: 'center',
    justifyContent: 'space-between', padding: '0 14px 0 16px',
    borderBottom: '1px solid var(--rule)',
  },
  iconBtn: {
    width: 28, height: 28, display: 'grid', placeItems: 'center',
    borderRadius: 5, color: 'var(--ink-3)',
  },
  tokenPill: {
    display: 'inline-flex', alignItems: 'center', gap: 5,
    padding: '4px 9px', borderRadius: 4,
    border: '1px solid var(--rule-2)',
    background: 'var(--paper-2)',
    color: 'var(--ink-2)',
    fontSize: 11,
  },
  tokenUnit: {
    fontSize: 10, color: 'var(--ink-3)', letterSpacing: '0.04em',
  },
  scroll: { flex: 1, overflowY: 'auto', padding: '14px 16px 8px' },
  phaseStrip: {
    display: 'flex', alignItems: 'flex-start', gap: 0,
    padding: '4px 4px 18px', marginBottom: 4,
  },
  phaseCol: { display: 'flex', flexDirection: 'column', alignItems: 'center', textAlign: 'center', minWidth: 48 },
  phaseDot: {
    width: 22, height: 22, borderRadius: '50%',
    display: 'grid', placeItems: 'center',
    border: '1px solid', fontSize: 10,
    fontFamily: "'JetBrains Mono', monospace", fontWeight: 600,
  },
  cardHead: {
    display: 'flex', alignItems: 'center',
    justifyContent: 'space-between', padding: '9px 12px',
    textAlign: 'left', background: 'transparent',
  },
  cardDetailBtn: {
    width: 36, display: 'grid', placeItems: 'center',
    color: 'var(--ink-3)',
  },
  phaseBadge: {
    fontFamily: "'JetBrains Mono', monospace", fontSize: 9.5,
    padding: '2px 5px', borderRadius: 3, letterSpacing: '0.04em',
    background: 'var(--paper-2)', color: 'var(--ink-3)',
    border: '1px solid var(--rule-2)', fontWeight: 600,
    whiteSpace: 'nowrap',
  },
  cardBody: {
    padding: '12px 14px 12px', borderTop: '1px solid var(--rule)',
  },
  bullets: { margin: 0, padding: 0, listStyle: 'none' },
  bulletLi: {
    display: 'flex', gap: 8, alignItems: 'flex-start',
    padding: '4px 0', fontSize: 12.5, color: 'var(--ink-2)',
  },
  bulletDot: {
    width: 3, height: 3, borderRadius: '50%',
    background: 'var(--ink-3)', marginTop: 8, flexShrink: 0,
  },
  gateBanner: {
    display: 'flex', alignItems: 'center', gap: 6,
    fontSize: 11, color: 'var(--ink-3)',
    padding: '6px 8px', background: 'var(--paper-2)',
    borderRadius: 5, marginBottom: 10,
    fontFamily: "'JetBrains Mono', monospace",
  },
  runRow: {
    display: 'flex', alignItems: 'center', gap: 10,
    padding: '7px 10px', border: '1px solid var(--rule)',
    borderRadius: 5, background: 'var(--paper-2)',
    cursor: 'pointer', textAlign: 'left',
    transition: 'background 120ms, border-color 120ms',
  },
  stepGroup: {
    padding: '10px 10px 10px 10px',
    border: '1px solid var(--rule)',
    borderRadius: 6,
    background: 'var(--paper-2)',
  },
  stepGroupHead: {
    display: 'flex', alignItems: 'center', gap: 8,
    marginBottom: 8,
  },
  evRow: {
    display: 'flex', alignItems: 'center', gap: 10,
    padding: '8px 0', borderBottom: '1px solid var(--rule)',
  },
  footer: {
    height: 44, flexShrink: 0, display: 'flex', alignItems: 'center',
    justifyContent: 'space-between', padding: '0 14px',
    borderTop: '1px solid var(--rule)',
  },
  fBtn: {
    padding: '5px 10px', borderRadius: 5, fontSize: 11.5,
    border: '1px solid var(--rule-2)', color: 'var(--ink-2)',
    background: 'var(--paper)',
  },
  fBtnPrimary: {
    display: 'inline-flex', alignItems: 'center', gap: 5,
    padding: '5px 10px', borderRadius: 5, fontSize: 11.5, fontWeight: 500,
    background: 'var(--ink)', color: 'var(--paper)',
  },
  intentThumb: {
    display: 'block', textAlign: 'left', width: '100%',
    background: 'transparent', padding: 0,
  },
  viewMore: {
    display: 'inline-flex', alignItems: 'center', gap: 4,
    marginTop: 10, fontSize: 11, color: 'var(--accent)',
    fontFamily: "'JetBrains Mono', monospace",
  },
};

const dpStyles = {
  head: {
    height: 48, flexShrink: 0, display: 'flex', alignItems: 'center',
    justifyContent: 'space-between', padding: '0 14px',
    borderBottom: '1px solid var(--rule)',
  },
  body: {
    flex: 1, overflowY: 'auto', padding: '18px 18px 28px',
  },
  sectionLabel: {
    fontSize: 10, letterSpacing: '0.08em', textTransform: 'uppercase',
    color: 'var(--ink-3)', fontWeight: 500, marginBottom: 8,
  },
  monoBlock: { fontFamily: "'JetBrains Mono', monospace" },
  pre: {
    margin: 0, padding: '10px 12px',
    background: 'var(--paper-2)', borderRadius: 5,
    border: '1px solid var(--rule)',
    fontSize: 11, lineHeight: 1.55, color: 'var(--ink)',
    whiteSpace: 'pre-wrap', wordBreak: 'break-word',
    fontFamily: "'JetBrains Mono', monospace",
  },
  diffFile: {
    border: '1px solid var(--rule)', borderRadius: 5, overflow: 'hidden',
  },
  diffHead: {
    padding: '6px 10px', fontSize: 11, borderBottom: '1px solid var(--rule)',
    background: 'var(--paper-2)', fontFamily: "'JetBrains Mono', monospace",
  },
  toolRow: {
    display: 'flex', alignItems: 'center', gap: 8,
    padding: '6px 0', borderBottom: '1px solid var(--rule)',
  },
};

window.Workflow = Workflow;

const mdStyles = {
  root: { color: 'var(--ink)', fontSize: 13, lineHeight: 1.65 },
  h1: {
    fontSize: 19, fontWeight: 600, letterSpacing: '-0.015em',
    margin: '0 0 10px', lineHeight: 1.25,
  },
  h2: {
    fontSize: 13, fontWeight: 600, letterSpacing: '-0.005em',
    margin: '22px 0 8px', lineHeight: 1.3,
    paddingBottom: 4, borderBottom: '1px solid var(--rule)',
  },
  h3: {
    fontSize: 12.5, fontWeight: 600, letterSpacing: '0',
    margin: '18px 0 6px', lineHeight: 1.3, color: 'var(--ink-2)',
  },
  p: {
    margin: '0 0 10px', color: 'var(--ink-2)',
    fontSize: 13, lineHeight: 1.65,
  },
  ul: { margin: '0 0 12px', padding: '0 0 0 18px' },
  ol: { margin: '0 0 12px', padding: '0 0 0 20px' },
  li: {
    margin: '3px 0', color: 'var(--ink-2)',
    fontSize: 13, lineHeight: 1.6,
  },
  code: {
    fontFamily: "'JetBrains Mono', monospace",
    fontSize: 11.5, padding: '1px 5px', borderRadius: 3,
    background: 'var(--paper-2)', border: '1px solid var(--rule)',
    color: 'var(--ink)',
  },
  strong: { fontWeight: 600, color: 'var(--ink)' },
};
