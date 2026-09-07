function Chat() {
  const [messages, setMessages] = React.useState(MESSAGES);
  const [draft, setDraft] = React.useState('');
  const [mode, setMode] = React.useState('Plan');
  const [termOpen, setTermOpen] = React.useState(() => readBool('libra.termOpen', true));
  const [termH, setTermH]       = React.useState(() => readInt('libra.termH', 240));
  const scrollRef = React.useRef(null);
  const chatBodyRef = React.useRef(null);

  React.useEffect(() => { writeBool('libra.termOpen', termOpen); }, [termOpen]);
  React.useEffect(() => { writeInt('libra.termH', termH); }, [termH]);

  React.useEffect(() => {
    if (scrollRef.current) scrollRef.current.scrollTop = scrollRef.current.scrollHeight;
  }, [messages]);

  // Simulate streaming the last assistant message on first render
  React.useEffect(() => {
    const last = messages[messages.length - 1];
    if (!last || !last.streaming) return;
    const full = last.body;
    let i = 0;
    setMessages(m => {
      const copy = [...m];
      copy[copy.length - 1] = { ...copy[copy.length - 1], body: '' };
      return copy;
    });
    const t = setInterval(() => {
      i += 3;
      setMessages(m => {
        const copy = [...m];
        const msg = copy[copy.length - 1];
        copy[copy.length - 1] = { ...msg, body: full.slice(0, i) };
        return copy;
      });
      if (i >= full.length) {
        clearInterval(t);
        setMessages(m => {
          const copy = [...m];
          copy[copy.length - 1] = { ...copy[copy.length - 1], streaming: false };
          return copy;
        });
      }
    }, 26);
    return () => clearInterval(t);
  }, []);

  function submit() {
    if (!draft.trim()) return;
    const userMsg = { role: 'user', time: nowTime(), body: draft };
    const assistantMsg = { role: 'assistant', time: nowTime(), body: '', streaming: true, _target: stubReply(draft) };
    setMessages(m => [...m, userMsg, assistantMsg]);
    setDraft('');
    // stream the reply
    setTimeout(() => streamLast(setMessages), 120);
  }

  function onKey(e) {
    if (e.key === 'Enter' && !e.shiftKey) { e.preventDefault(); submit(); }
  }

  return (
    <section style={chatStyles.wrap}>
      <header style={chatStyles.header}>
        <div style={{ display: 'flex', alignItems: 'center', gap: 10, minWidth: 0 }}>
          <IconThread size={15}/>
          <div style={{ fontWeight: 600, fontSize: 13.5, whiteSpace: 'nowrap', overflow: 'hidden', textOverflow: 'ellipsis' }}>
            Add optimistic updates to useMutation
          </div>
          <span style={chatStyles.chip}>
            <IconBranch size={11}/> agent/optimistic-mutate
          </span>
          <span style={{...chatStyles.chip, color: 'var(--accent)', borderColor: 'var(--accent-line)', background: 'var(--accent-soft)' }}>
            Phase 2 · Execution
          </span>
        </div>
        <div style={{ display: 'flex', alignItems: 'center', gap: 4, color: 'var(--ink-3)' }}>
          {!termOpen && (
            <button onClick={() => setTermOpen(true)} style={chatStyles.termToggle} title="Show terminal">
              <IconTerm size={13}/> Terminal
            </button>
          )}
          <button style={chatStyles.iconBtn} title="Share"><IconCopy size={14}/></button>
          <button style={chatStyles.iconBtn} title="More"><IconMore size={14}/></button>
        </div>
      </header>

      <div ref={chatBodyRef} style={chatStyles.chatBody}>
        <div ref={scrollRef} style={chatStyles.scroll}>
          <div style={chatStyles.transcriptHead}>
            <div style={chatStyles.rule}/>
            <div style={{ fontSize: 11, color: 'var(--ink-3)', fontFamily: "'JetBrains Mono', monospace" }}>
              thread opened 10:42 · intent confirmed · 2 plan revisions
            </div>
            <div style={chatStyles.rule}/>
          </div>
          {messages.map((m, i) => <Message key={i} m={m}/>)}
        </div>

        <div style={chatStyles.composerWrap}>
          <div style={chatStyles.composer}>
            <div style={chatStyles.composerToolbar}>
              <button style={chatStyles.chipBtn}><IconAt size={12}/> Add context</button>
              <button style={chatStyles.chipBtn}><IconFile size={12}/> src/lib/query.ts</button>
              <div style={{ flex: 1 }}/>
              <ModeToggle value={mode} onChange={setMode}/>
            </div>
            <textarea
              value={draft}
              onChange={e => setDraft(e.target.value)}
              onKeyDown={onKey}
              placeholder="Reply to the agent, or steer the next step…"
              rows={2}
              style={chatStyles.textarea}
            />
            <div style={chatStyles.composerFoot}>
              <div style={{ display: 'flex', gap: 10, alignItems: 'center', color: 'var(--ink-3)', fontSize: 11 }}>
                <span className="mono" style={{ fontSize: 10.5 }}>claude-sonnet-4.5</span>
                <span>·</span>
                <span>read-only tools in Phase 0/1, sandboxed in Phase 2</span>
              </div>
              <button
                onClick={submit}
                disabled={!draft.trim()}
                style={{...chatStyles.send, ...(draft.trim() ? chatStyles.sendOn : {})}}
              >
                <IconSend size={13}/> Send <span style={chatStyles.kbdInline}>↵</span>
              </button>
            </div>
          </div>
        </div>
      </div>

      {termOpen && (
        <>
          <HSplitter
            value={termH}
            onDrag={(dy, startH) => {
              const parentH = chatBodyRef.current?.parentElement?.clientHeight || 800;
              const max = parentH - 260; // leave room for messages+composer
              setTermH(Math.max(120, Math.min(max, startH - dy)));
            }}
          />
          <Terminal height={termH} onClose={() => setTermOpen(false)}/>
        </>
      )}
    </section>
  );
}

function HSplitter({ value, onDrag }) {
  const [hover, setHover] = React.useState(false);
  const [drag, setDrag]   = React.useState(false);

  function onMouseDown(e) {
    e.preventDefault();
    const startY = e.clientY;
    const startH = value;
    setDrag(true);
    document.body.style.cursor = 'row-resize';
    document.body.style.userSelect = 'none';
    function onMove(ev) { onDrag(ev.clientY - startY, startH); }
    function onUp() {
      setDrag(false);
      document.body.style.cursor = '';
      document.body.style.userSelect = '';
      window.removeEventListener('mousemove', onMove);
      window.removeEventListener('mouseup', onUp);
    }
    window.addEventListener('mousemove', onMove);
    window.addEventListener('mouseup', onUp);
  }

  const active = hover || drag;
  return (
    <div
      onMouseDown={onMouseDown}
      onMouseEnter={() => setHover(true)}
      onMouseLeave={() => setHover(false)}
      style={{
        position: 'relative',
        height: 1, flexShrink: 0,
        background: 'var(--rule-2)',
        cursor: 'row-resize',
      }}
    >
      <div style={{ position: 'absolute', left: 0, right: 0, top: -3, bottom: -3 }}/>
      <div style={{
        position: 'absolute', inset: 0,
        background: active ? 'var(--accent)' : 'transparent',
        transition: 'background 120ms',
      }}/>
    </div>
  );
}

function readBool(k, fallback) { try { const v = localStorage.getItem(k); return v === null ? fallback : v === 'true'; } catch { return fallback; } }
function writeBool(k, v) { try { localStorage.setItem(k, String(v)); } catch {} }
function readInt(k, fallback)  { try { const v = parseInt(localStorage.getItem(k), 10); return Number.isFinite(v) ? v : fallback; } catch { return fallback; } }
function writeInt(k, v)  { try { localStorage.setItem(k, String(v)); } catch {} }

function UserMessage({ m }) {
  const [hover, setHover] = React.useState(false);
  const [copied, setCopied] = React.useState(false);

  function copy() {
    const text = m.body;
    const done = () => {
      setCopied(true);
      setTimeout(() => setCopied(false), 1400);
    };
    if (navigator.clipboard?.writeText) {
      navigator.clipboard.writeText(text).then(done, () => fallback(text, done));
    } else {
      fallback(text, done);
    }
  }

  return (
    <div
      style={chatStyles.user}
      onMouseEnter={() => setHover(true)}
      onMouseLeave={() => setHover(false)}
    >
      <div style={chatStyles.userBubbleRow}>
        <button
          onClick={copy}
          title={copied ? 'Copied' : 'Copy message'}
          style={{
            ...chatStyles.copyBtn,
            opacity: hover || copied ? 1 : 0,
            pointerEvents: hover || copied ? 'auto' : 'none',
            color: copied ? 'var(--good)' : 'var(--ink-3)',
            borderColor: copied ? 'var(--good)' : 'var(--rule-2)',
          }}
        >
          {copied ? <IconCheck size={12} sw={2.5}/> : <IconCopy size={12}/>}
          <span style={{ fontSize: 10.5 }}>{copied ? 'Copied' : 'Copy'}</span>
        </button>
        <div style={chatStyles.userBubble}>{m.body}</div>
      </div>
      <div style={chatStyles.metaR}>
        <span className="mono">you</span>
        <span>·</span>
        <span>{m.time}</span>
      </div>
    </div>
  );
}

function fallback(text, done) {
  try {
    const ta = document.createElement('textarea');
    ta.value = text;
    ta.style.position = 'fixed'; ta.style.opacity = '0';
    document.body.appendChild(ta);
    ta.select();
    document.execCommand('copy');
    document.body.removeChild(ta);
    done();
  } catch {}
}

function Message({ m }) {
  if (m.role === 'user') {
    return <UserMessage m={m}/>;
  }
  return (
    <div style={chatStyles.asst}>
      <div style={chatStyles.asstHead}>
        <div style={chatStyles.asstMark}>
          <svg width="11" height="11" viewBox="0 0 24 24" fill="none">
            <path d="M5 4h3v13h8v3H5z" stroke="currentColor" strokeWidth="2.2" strokeLinejoin="round"/>
            <circle cx="17" cy="6" r="2.2" fill="currentColor"/>
          </svg>
        </div>
        <span className="mono" style={{ fontSize: 10.5, fontWeight: 500 }}>libra</span>
        <span style={{ color: 'var(--ink-3)', fontSize: 10.5 }}>·</span>
        <span style={{ color: 'var(--ink-3)', fontSize: 10.5 }}>{m.time}</span>
        {m.streaming && <span style={chatStyles.streaming}>
          <span style={chatStyles.streamDot}/> streaming
        </span>}
      </div>
      <div style={chatStyles.asstBody}>
        {m.body.split('\n').map((line, i) => <div key={i} style={{ minHeight: '1em' }}>{line}</div>)}
        {m.streaming && <span style={chatStyles.caret}/>}
      </div>
    </div>
  );
}

function ModeToggle({ value, onChange }) {
  const modes = ['Plan', 'Build'];
  return (
    <div style={chatStyles.modeToggle}>
      {modes.map(m => (
        <button key={m} onClick={() => onChange(m)}
          style={{...chatStyles.modeBtn, ...(value === m ? chatStyles.modeOn : {})}}>
          {m}
        </button>
      ))}
    </div>
  );
}

function nowTime() {
  const d = new Date();
  return `${String(d.getHours()).padStart(2,'0')}:${String(d.getMinutes()).padStart(2,'0')}`;
}

function stubReply(input) {
  const short = input.trim().split(/\s+/).slice(0, 6).join(' ');
  return `Got it — "${short}…". I'll re-read the relevant files and draft a revised execution plan; the test plan stays unless I need new coverage. One moment.`;
}

function streamLast(setMessages) {
  setMessages(prev => {
    const last = prev[prev.length - 1];
    if (!last || !last._target) return prev;
    const full = last._target;
    let i = 0;
    const t = setInterval(() => {
      i += 3;
      setMessages(curr => {
        const copy = [...curr];
        const msg = copy[copy.length - 1];
        if (!msg || msg._target !== full) { clearInterval(t); return curr; }
        copy[copy.length - 1] = { ...msg, body: full.slice(0, i) };
        return copy;
      });
      if (i >= full.length) {
        clearInterval(t);
        setMessages(curr => {
          const copy = [...curr];
          copy[copy.length - 1] = { ...copy[copy.length - 1], streaming: false, _target: undefined };
          return copy;
        });
      }
    }, 22);
    return prev;
  });
}

const chatStyles = {
  wrap: { flex: 1, display: 'flex', flexDirection: 'column', minWidth: 0, background: 'var(--paper)' },
  chatBody: { flex: 1, display: 'flex', flexDirection: 'column', minHeight: 0 },
  termToggle: {
    display: 'inline-flex', alignItems: 'center', gap: 5,
    fontSize: 11, color: 'var(--ink-2)',
    padding: '4px 8px', borderRadius: 4,
    border: '1px solid var(--rule-2)', background: 'var(--paper-2)',
    marginRight: 4,
  },
  header: {
    height: 48, flexShrink: 0, display: 'flex', alignItems: 'center',
    justifyContent: 'space-between', padding: '0 20px',
    borderBottom: '1px solid var(--rule)',
  },
  chip: {
    display: 'inline-flex', alignItems: 'center', gap: 5,
    fontFamily: "'JetBrains Mono', monospace", fontSize: 10.5,
    padding: '3px 7px', border: '1px solid var(--rule-2)',
    borderRadius: 4, color: 'var(--ink-2)', background: 'var(--paper-2)',
    whiteSpace: 'nowrap',
  },
  iconBtn: {
    width: 28, height: 28, display: 'grid', placeItems: 'center',
    borderRadius: 5, color: 'var(--ink-3)',
  },
  scroll: { flex: 1, overflowY: 'auto', padding: '24px 32px 20px' },
  transcriptHead: {
    display: 'flex', alignItems: 'center', gap: 10, marginBottom: 22,
  },
  rule: { height: 1, background: 'var(--rule)', flex: 1 },
  user: { display: 'flex', flexDirection: 'column', alignItems: 'flex-end', marginBottom: 22 },
  userBubbleRow: {
    display: 'flex', alignItems: 'flex-end', gap: 8,
    maxWidth: '82%',
  },
  copyBtn: {
    display: 'inline-flex', alignItems: 'center', gap: 4,
    padding: '3px 7px', borderRadius: 4,
    border: '1px solid var(--rule-2)',
    background: 'var(--paper)',
    color: 'var(--ink-3)',
    fontSize: 10.5, fontWeight: 500,
    transition: 'opacity 120ms, color 120ms, border-color 120ms',
    marginBottom: 2, flexShrink: 0,
  },
  userBubble: {
    maxWidth: '78%', padding: '10px 14px', borderRadius: '10px 10px 2px 10px',
    background: 'var(--ink)', color: 'var(--paper)',
    lineHeight: 1.55, fontSize: 13, whiteSpace: 'pre-wrap',
  },
  metaR: { display: 'flex', gap: 6, marginTop: 4, fontSize: 10.5, color: 'var(--ink-3)' },
  asst: { marginBottom: 26, maxWidth: 720 },
  asstHead: { display: 'flex', alignItems: 'center', gap: 6, marginBottom: 8, color: 'var(--ink-2)' },
  asstMark: {
    width: 18, height: 18, borderRadius: 4, background: 'var(--ink)',
    color: 'var(--paper)', display: 'grid', placeItems: 'center',
  },
  asstBody: {
    fontSize: 13.5, lineHeight: 1.62, color: 'var(--ink)',
    whiteSpace: 'pre-wrap',
    paddingLeft: 24, borderLeft: '1px solid var(--rule)',
  },
  streaming: {
    marginLeft: 8, display: 'inline-flex', alignItems: 'center', gap: 5,
    fontFamily: "'JetBrains Mono', monospace", fontSize: 10,
    color: 'var(--accent)', padding: '1px 6px',
    background: 'var(--accent-soft)', borderRadius: 3,
  },
  streamDot: {
    width: 5, height: 5, borderRadius: '50%', background: 'var(--accent)',
    animation: 'pulse 1.2s ease-in-out infinite',
  },
  caret: {
    display: 'inline-block', width: 7, height: 14, background: 'var(--ink)',
    marginLeft: 2, verticalAlign: '-2px',
    animation: 'caret 1s steps(2) infinite',
  },
  composerWrap: {
    padding: '12px 32px 20px',
    borderTop: '1px solid var(--rule)',
    background: 'var(--paper)',
  },
  composer: {
    border: '1px solid var(--rule-2)', borderRadius: 10,
    background: 'var(--paper)',
    boxShadow: '0 1px 0 rgba(0,0,0,0.02), 0 2px 8px -2px rgba(0,0,0,0.04)',
  },
  composerToolbar: {
    display: 'flex', alignItems: 'center', gap: 6, padding: '8px 10px',
    borderBottom: '1px solid var(--rule)',
  },
  chipBtn: {
    display: 'inline-flex', alignItems: 'center', gap: 5,
    fontSize: 11.5, color: 'var(--ink-2)',
    padding: '4px 8px', borderRadius: 4,
    background: 'var(--paper-2)', border: '1px solid var(--rule)',
  },
  textarea: {
    width: '100%', border: 'none', outline: 'none', resize: 'none',
    padding: '12px 14px', background: 'transparent',
    fontSize: 13.5, lineHeight: 1.55, color: 'var(--ink)',
    fontFamily: 'inherit', minHeight: 44,
  },
  composerFoot: {
    display: 'flex', alignItems: 'center', justifyContent: 'space-between',
    padding: '6px 10px 8px 14px',
  },
  send: {
    display: 'inline-flex', alignItems: 'center', gap: 6,
    padding: '6px 10px', borderRadius: 5,
    background: 'var(--paper-2)', color: 'var(--ink-3)',
    border: '1px solid var(--rule)',
    fontSize: 12, fontWeight: 500,
  },
  sendOn: {
    background: 'var(--accent)', color: 'white', borderColor: 'var(--accent)',
  },
  kbdInline: { fontFamily: "'JetBrains Mono', monospace", fontSize: 10, opacity: 0.8, marginLeft: 2 },
  modeToggle: {
    display: 'flex', padding: 2, background: 'var(--paper-2)',
    border: '1px solid var(--rule)', borderRadius: 5,
  },
  modeBtn: {
    padding: '3px 10px', borderRadius: 3, fontSize: 11.5,
    color: 'var(--ink-3)', fontWeight: 500,
  },
  modeOn: { background: 'var(--paper)', color: 'var(--ink)', boxShadow: '0 1px 0 rgba(0,0,0,0.04)' },
};

// Inject keyframes once
if (typeof document !== 'undefined' && !document.getElementById('libra-kf')) {
  const st = document.createElement('style');
  st.id = 'libra-kf';
  st.textContent = `
    @keyframes pulse { 0%,100% { opacity: 1 } 50% { opacity: 0.3 } }
    @keyframes caret { 50% { opacity: 0 } }
    @keyframes spin  { to { transform: rotate(360deg) } }
  `;
  document.head.appendChild(st);
}

window.Chat = Chat;
