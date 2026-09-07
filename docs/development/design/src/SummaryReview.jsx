function SummaryView() {
  return (
    <div style={srStyles.wrap}>
      <Block label="Progress">
        <ul style={srStyles.list}>
          {SUMMARY.progress.map((p, i) => (
            <li key={i} style={srStyles.item}>
              <Tick on={p.done}/>
              <span style={{ color: p.done ? 'var(--ink)' : 'var(--ink-2)' }}>{p.text}</span>
            </li>
          ))}
        </ul>
      </Block>

      <Block label="Branch state">
        <div style={srStyles.kv}><span>Branch</span><span className="mono">{SUMMARY.branch.name}</span></div>
        <div style={srStyles.kv}><span>Base</span><span className="mono">{SUMMARY.branch.base}</span></div>
        <div style={srStyles.kv}><span>PR</span><span>{SUMMARY.branch.pr}</span></div>
        <div style={srStyles.kv}><span>Changes</span><span>{SUMMARY.branch.changes}</span></div>
      </Block>

      <Block label="Artifacts">
        {SUMMARY.artifacts.map((a, i) => (
          <div key={i} style={srStyles.artifact}>
            <span style={srStyles.tag}>{a.kind}</span>
            <span className="mono" style={{ fontSize: 11.5 }}>{a.id}</span>
            <span style={{ color: 'var(--ink-3)', fontSize: 11.5, marginLeft: 'auto' }}>{a.meta}</span>
          </div>
        ))}
      </Block>

      <Block label="To-dos">
        <ul style={srStyles.list}>
          {SUMMARY.todo.map((t, i) => (
            <li key={i} style={srStyles.item}>
              <Tick on={t.done}/>
              <span style={{ color: t.done ? 'var(--ink-3)' : 'var(--ink)', textDecoration: t.done ? 'line-through' : 'none' }}>{t.text}</span>
            </li>
          ))}
        </ul>
      </Block>
    </div>
  );
}

function ReviewView() {
  return (
    <div style={srStyles.wrap}>
      <div style={srStyles.reviewHead}>
        <span className="mono" style={{ fontSize: 11, color: 'var(--ink-3)' }}>
          {REVIEW.stats.files} files
        </span>
        <span className="mono" style={{ fontSize: 11, color: 'var(--good)' }}>+{REVIEW.stats.add}</span>
        <span className="mono" style={{ fontSize: 11, color: 'var(--bad)' }}>−{REVIEW.stats.del}</span>
      </div>
      {REVIEW.files.map(f => <FileDiff key={f.path} file={f}/>)}
    </div>
  );
}

function FileDiff({ file }) {
  const [open, setOpen] = React.useState(true);
  return (
    <div style={srStyles.file}>
      <button onClick={() => setOpen(o => !o)} style={srStyles.fileHead}>
        <div style={{ color: 'var(--ink-3)', transform: open ? 'rotate(90deg)' : 'none' }}>
          <IconChev size={12}/>
        </div>
        <span className="mono" style={{ fontSize: 11.5, flex: 1, textAlign: 'left', overflow: 'hidden', textOverflow: 'ellipsis', whiteSpace: 'nowrap' }}>{file.path}</span>
        <span className="mono" style={{ fontSize: 10.5, color: 'var(--good)' }}>+{file.add}</span>
        <span className="mono" style={{ fontSize: 10.5, color: 'var(--bad)' }}>−{file.del}</span>
      </button>
      {open && file.hunks.map((h, i) => (
        <div key={i} style={srStyles.hunk}>
          <div style={srStyles.hunkHead} className="mono">{h.header}</div>
          <div>
            {h.lines.map((ln, j) => <DiffLine key={j} line={ln}/>)}
          </div>
        </div>
      ))}
    </div>
  );
}

function DiffLine({ line }) {
  const bg = line.kind === 'add' ? 'color-mix(in oklch, var(--good) 10%, var(--paper))'
           : line.kind === 'del' ? 'color-mix(in oklch, var(--bad) 10%, var(--paper))'
           : 'transparent';
  const marker = line.kind === 'add' ? '+' : line.kind === 'del' ? '−' : ' ';
  return (
    <div style={{ display: 'flex', background: bg, fontFamily: "'JetBrains Mono', monospace", fontSize: 11, lineHeight: 1.5 }}>
      <span style={srStyles.gutter}>{line.n1 ?? ''}</span>
      <span style={srStyles.gutter}>{line.n2 ?? ''}</span>
      <span style={{ width: 14, textAlign: 'center', color: line.kind === 'add' ? 'var(--good)' : line.kind === 'del' ? 'var(--bad)' : 'var(--ink-3)' }}>{marker}</span>
      <span style={{ flex: 1, paddingRight: 10, whiteSpace: 'pre', color: 'var(--ink)' }}>{line.text}</span>
    </div>
  );
}

function Block({ label, children }) {
  return (
    <div style={srStyles.block}>
      <div style={srStyles.blockLabel}>{label}</div>
      {children}
    </div>
  );
}

function Tick({ on }) {
  return (
    <span style={{
      width: 14, height: 14, borderRadius: 3, flexShrink: 0,
      border: `1px solid ${on ? 'var(--accent)' : 'var(--rule-2)'}`,
      background: on ? 'var(--accent)' : 'var(--paper)',
      color: 'white', display: 'grid', placeItems: 'center', marginTop: 2,
    }}>{on && <IconCheck size={9} sw={3}/>}</span>
  );
}

const srStyles = {
  wrap: { padding: '16px 18px 24px' },
  block: { marginBottom: 20 },
  blockLabel: {
    fontSize: 10, letterSpacing: '0.08em', textTransform: 'uppercase',
    color: 'var(--ink-3)', fontWeight: 500, marginBottom: 8,
  },
  list: { margin: 0, padding: 0, listStyle: 'none' },
  item: {
    display: 'flex', gap: 8, alignItems: 'flex-start',
    padding: '5px 0', fontSize: 12.5, lineHeight: 1.5,
  },
  kv: {
    display: 'flex', justifyContent: 'space-between',
    padding: '5px 0', borderBottom: '1px solid var(--rule)',
    fontSize: 12, color: 'var(--ink-2)',
  },
  artifact: {
    display: 'flex', alignItems: 'center', gap: 8,
    padding: '6px 8px', marginBottom: 4,
    border: '1px solid var(--rule)', borderRadius: 5, background: 'var(--paper-2)',
  },
  tag: {
    fontSize: 9.5, fontFamily: "'JetBrains Mono', monospace",
    padding: '1px 5px', borderRadius: 3, background: 'var(--paper)',
    border: '1px solid var(--rule-2)', color: 'var(--ink-2)', letterSpacing: '0.04em',
  },
  reviewHead: {
    display: 'flex', alignItems: 'center', gap: 10,
    padding: '0 2px 10px', marginBottom: 4,
  },
  file: {
    border: '1px solid var(--rule)', borderRadius: 6,
    marginBottom: 10, overflow: 'hidden', background: 'var(--paper)',
  },
  fileHead: {
    display: 'flex', alignItems: 'center', gap: 8,
    width: '100%', padding: '8px 10px',
    background: 'var(--paper-2)', borderBottom: '1px solid var(--rule)',
  },
  hunk: {},
  hunkHead: {
    padding: '4px 10px', fontSize: 10.5,
    color: 'var(--ink-3)', background: 'var(--paper-2)',
    borderBottom: '1px solid var(--rule)',
  },
  gutter: {
    width: 36, textAlign: 'right', padding: '0 6px',
    color: 'var(--ink-3)', fontSize: 10,
    borderRight: '1px solid var(--rule)', flexShrink: 0,
  },
};

Object.assign(window, { SummaryView, ReviewView });
