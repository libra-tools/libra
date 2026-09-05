// Terminal panel — sandbox output viewer.
// Shows sandbox metadata, a tab-switchable log stream, and a prompt-style footer.

const TERMINAL_LINES = [
  { kind: 'meta',   text: 'libra sandbox v0.4.2 · image rust:1.81-slim · net=off · fs=rw(tmp)' },
  { kind: 'meta',   text: 'mount: /workspace → agent/optimistic-mutate @ 7f3a9e1' },
  { kind: 'prompt', text: 'cargo test --lib optimistic' },
  { kind: 'stdout', text: '   Compiling libra-cache v0.3.1 (/workspace)' },
  { kind: 'stdout', text: '   Compiling libra-hooks v0.1.4 (/workspace)' },
  { kind: 'stdout', text: '    Finished test [unoptimized + debuginfo] target(s) in 3.42s' },
  { kind: 'stdout', text: '     Running unittests src/lib.rs (target/debug/deps/libra_cache-7c8f1a)' },
  { kind: 'stdout', text: '' },
  { kind: 'stdout', text: 'running 3 tests' },
  { kind: 'pass',   text: 'test optimistic::snapshot_before_mutate ... ok' },
  { kind: 'pass',   text: 'test optimistic::patch_visible_synchronously ... ok' },
  { kind: 'run',    text: 'test optimistic::rollback_preserves_concurrent ... running' },
  { kind: 'stdout', text: '' },
  { kind: 'info',   text: '[agent] capturing PatchSet ps-07 (+46 −7 across 2 files)' },
  { kind: 'info',   text: '[agent] revision guard open: cache key "users:42" rev=4→5' },
  { kind: 'warn',   text: 'warning: unused variable `prev_snapshot` (will be used once rollback lands)' },
];

function Terminal({ height, onResize, onClose }) {
  const [tab, setTab] = React.useState('sandbox'); // sandbox | tools | agent
  const [cmd, setCmd]   = React.useState('');
  const [history, setHistory] = React.useState(TERMINAL_LINES);
  const scrollRef = React.useRef(null);

  React.useEffect(() => {
    if (scrollRef.current) scrollRef.current.scrollTop = scrollRef.current.scrollHeight;
  }, [history]);

  function submit(e) {
    e.preventDefault();
    if (!cmd.trim()) return;
    const entered = { kind: 'prompt', text: cmd };
    const reply   = { kind: 'stdout', text: stubShellReply(cmd) };
    setHistory(h => [...h, entered, reply]);
    setCmd('');
  }

  const visibleLines = history.filter(l => tabFilter(l, tab));

  return (
    <div style={{ ...termStyles.wrap, height }}>
      <header style={termStyles.header}>
        <div style={termStyles.tabs}>
          <TermTab active={tab==='sandbox'} onClick={() => setTab('sandbox')}>
            <IconTerm size={12}/> Sandbox
          </TermTab>
          <TermTab active={tab==='tools'} onClick={() => setTab('tools')}>
            <IconTool size={12}/> Tools
          </TermTab>
          <TermTab active={tab==='agent'} onClick={() => setTab('agent')}>
            <IconSpark size={12}/> Agent
          </TermTab>
        </div>
        <div style={termStyles.headerMeta}>
          <span style={termStyles.statusDot}/>
          <span className="mono" style={{ fontSize: 10.5 }}>libra-sbx-04</span>
          <span style={{ color: 'var(--rule-2)' }}>·</span>
          <span className="mono" style={{ fontSize: 10.5 }}>rust:1.81 · net=off</span>
          <button onClick={onClose} style={termStyles.iconBtn} title="Hide terminal">
            <IconX size={12}/>
          </button>
        </div>
      </header>

      <div ref={scrollRef} style={termStyles.body}>
        {visibleLines.map((l, i) => <TermLine key={i} line={l}/>)}
      </div>

      <form onSubmit={submit} style={termStyles.prompt}>
        <span className="mono" style={termStyles.promptMark}>agent@sbx-04 ~ $</span>
        <input
          value={cmd}
          onChange={e => setCmd(e.target.value)}
          placeholder="run a command in the sandbox…"
          style={termStyles.promptInput}
          className="mono"
          spellCheck={false}
        />
      </form>
    </div>
  );
}

function TermTab({ active, onClick, children }) {
  return (
    <button onClick={onClick} style={{
      display: 'inline-flex', alignItems: 'center', gap: 5,
      padding: '4px 10px', fontSize: 11.5, fontWeight: 500,
      color: active ? 'var(--ink)' : 'var(--ink-3)',
      borderBottom: `1.5px solid ${active ? 'var(--ink)' : 'transparent'}`,
      marginBottom: -1,
    }}>{children}</button>
  );
}

function TermLine({ line }) {
  const tone = lineTone(line.kind);
  const showMark = line.kind === 'prompt';
  return (
    <div style={{ display: 'flex', gap: 8, alignItems: 'baseline', padding: '1.5px 0' }}>
      <span className="mono" style={{ color: tone.mark, width: 14, flexShrink: 0, fontSize: 10.5 }}>
        {lineMark(line.kind)}
      </span>
      <span className="mono" style={{
        fontSize: 11.5, color: tone.text, lineHeight: 1.55,
        whiteSpace: 'pre-wrap', wordBreak: 'break-word', flex: 1,
        fontWeight: showMark ? 500 : 400,
      }}>{line.text || '\u00A0'}</span>
    </div>
  );
}

function lineMark(kind) {
  switch (kind) {
    case 'prompt': return '$';
    case 'pass':   return '✓';
    case 'fail':   return '✗';
    case 'run':    return '•';
    case 'warn':   return '!';
    case 'info':   return 'ℹ';
    case 'meta':   return '·';
    default:       return ' ';
  }
}

function lineTone(kind) {
  switch (kind) {
    case 'prompt': return { mark: 'var(--accent)', text: 'var(--ink)' };
    case 'pass':   return { mark: 'var(--good)',   text: 'var(--ink-2)' };
    case 'fail':   return { mark: 'var(--bad)',    text: 'var(--bad)' };
    case 'run':    return { mark: 'var(--accent)', text: 'var(--ink-2)' };
    case 'warn':   return { mark: 'var(--warn)',   text: 'var(--ink-2)' };
    case 'info':   return { mark: 'var(--accent)', text: 'var(--ink-2)' };
    case 'meta':   return { mark: 'var(--ink-3)',  text: 'var(--ink-3)' };
    default:       return { mark: 'var(--ink-3)',  text: 'var(--ink-2)' };
  }
}

function tabFilter(line, tab) {
  if (tab === 'sandbox') return true;
  if (tab === 'tools')   return line.kind === 'prompt' || line.kind === 'stdout' || line.kind === 'meta';
  if (tab === 'agent')   return line.kind === 'info' || line.kind === 'warn' || line.kind === 'meta';
  return true;
}

function stubShellReply(cmd) {
  const c = cmd.trim().toLowerCase();
  if (c === 'ls')       return 'Cargo.toml  src/  tests/  target/';
  if (c.startsWith('cat '))  return `# ${c.slice(4)}\n(sandboxed — preview truncated)`;
  if (c.startsWith('cargo')) return 'error: sandbox locked to agent execution. Use the agent to re-run tests.';
  return `command not found: ${cmd.split(/\s+/)[0]}`;
}

const termStyles = {
  wrap: {
    flexShrink: 0, display: 'flex', flexDirection: 'column',
    background: 'var(--paper-2)', borderTop: '1px solid var(--rule-2)',
    minHeight: 120, overflow: 'hidden',
  },
  header: {
    height: 34, flexShrink: 0, display: 'flex', alignItems: 'center',
    justifyContent: 'space-between', padding: '0 12px 0 16px',
    borderBottom: '1px solid var(--rule)', background: 'var(--paper)',
  },
  tabs: { display: 'flex', gap: 2 },
  headerMeta: {
    display: 'flex', alignItems: 'center', gap: 6,
    color: 'var(--ink-3)', fontSize: 11,
  },
  statusDot: {
    width: 7, height: 7, borderRadius: '50%',
    background: 'var(--good)',
    boxShadow: '0 0 0 2px color-mix(in oklch, var(--good) 22%, transparent)',
  },
  iconBtn: {
    width: 22, height: 22, display: 'grid', placeItems: 'center',
    borderRadius: 4, color: 'var(--ink-3)', marginLeft: 4,
  },
  body: {
    flex: 1, overflowY: 'auto', padding: '8px 16px 8px',
    background: 'var(--paper-2)',
  },
  prompt: {
    flexShrink: 0, display: 'flex', alignItems: 'center', gap: 8,
    padding: '8px 16px', borderTop: '1px solid var(--rule)',
    background: 'var(--paper)',
  },
  promptMark: {
    fontSize: 11, color: 'var(--accent)', fontWeight: 500, flexShrink: 0,
  },
  promptInput: {
    flex: 1, border: 'none', outline: 'none', background: 'transparent',
    fontSize: 11.5, color: 'var(--ink)', padding: 0,
  },
};

window.Terminal = Terminal;
