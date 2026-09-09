function Sidebar({ width = 248 }) {
  const [query, setQuery] = React.useState('');
  const [menuOpen, setMenuOpen] = React.useState(false);
  const avatarRef = React.useRef(null);
  const filtered = THREADS.filter(t => t.title.toLowerCase().includes(query.toLowerCase()));

  React.useEffect(() => {
    if (!menuOpen) return;
    function onDown(e) {
      if (avatarRef.current && !avatarRef.current.contains(e.target)) setMenuOpen(false);
    }
    function onKey(e) { if (e.key === 'Escape') setMenuOpen(false); }
    document.addEventListener('mousedown', onDown);
    document.addEventListener('keydown', onKey);
    return () => {
      document.removeEventListener('mousedown', onDown);
      document.removeEventListener('keydown', onKey);
    };
  }, [menuOpen]);

  return (
    <aside style={{ ...ssStyles.aside, width }}>
      <div style={ssStyles.brand}>
        <div style={ssStyles.brandMark}>
          <svg width="18" height="18" viewBox="0 0 24 24" fill="none">
            <path d="M5 4h3v13h8v3H5z" stroke="currentColor" strokeWidth="1.8" strokeLinejoin="round"/>
            <circle cx="17" cy="6" r="2" fill="currentColor"/>
          </svg>
        </div>
        <div>
          <div style={{ fontWeight: 600, letterSpacing: '-0.01em' }}>Libra</div>
          <div style={{ fontSize: 11, color: 'var(--ink-3)' }}>agent workspace</div>
        </div>
      </div>

      <button style={ssStyles.newBtn}>
        <IconPlus size={14} sw={2}/> New thread
        <span style={ssStyles.kbd}>⌘N</span>
      </button>

      <div style={ssStyles.searchWrap}>
        <IconSearch size={14}/>
        <input
          placeholder="Search threads"
          value={query}
          onChange={e => setQuery(e.target.value)}
          style={ssStyles.search}
        />
      </div>

      <div style={ssStyles.sectionLabel}>Threads</div>
      <div style={{ overflowY: 'auto', flex: 1, margin: '0 -4px', padding: '0 4px' }}>
        {filtered.map(t => <ThreadItem key={t.id} t={t}/>)}
      </div>

      <div style={ssStyles.footer}>
        <div style={{ ...ssStyles.workspace, position: 'relative' }} ref={avatarRef}>
          <button
            onClick={() => setMenuOpen(o => !o)}
            style={{
              ...ssStyles.avatar,
              outline: menuOpen ? '2px solid var(--accent-line)' : 'none',
              outlineOffset: 1,
            }}
            title="Account"
          >
            EC
          </button>
          <div style={{ flex: 1, minWidth: 0 }}>
            <div style={{ fontSize: 12, fontWeight: 500, whiteSpace: 'nowrap', overflow: 'hidden', textOverflow: 'ellipsis' }}>web3infra / libra</div>
            <div style={{ fontSize: 11, color: 'var(--ink-3)' }}>main · clean</div>
          </div>
          <IconSettings size={14}/>
          {menuOpen && <SettingsMenu onClose={() => setMenuOpen(false)}/>}
        </div>
      </div>
    </aside>
  );
}

function SettingsMenu({ onClose }) {
  return (
    <div style={ssStyles.menu} onClick={e => e.stopPropagation()}>
      <div style={ssStyles.menuHead}>
        <div style={{ ...ssStyles.avatar, width: 32, height: 32, fontSize: 11 }}>EC</div>
        <div style={{ flex: 1, minWidth: 0 }}>
          <div style={{ fontSize: 12.5, fontWeight: 600 }}>Erin Chen</div>
          <div style={{ fontSize: 10.5, color: 'var(--ink-3)' }}>erin@web3infra.io</div>
        </div>
      </div>
      <div style={ssStyles.menuGroup}>
        <MenuItem label="Personal account" meta="erin@web3infra.io" active/>
        <MenuItem label="web3infra / libra" meta="team"/>
      </div>
      <div style={ssStyles.menuSep}/>
      <MenuItem label="Settings" shortcut="⌘,"/>
      <MenuItem label="Integrations"/>
      <MenuItem label="Rate limits remaining" meta="84%" mono/>
      <div style={ssStyles.menuSep}/>
      <MenuItem label="Keyboard shortcuts" shortcut="⌘/"/>
      <MenuItem label="Documentation"/>
    </div>
  );
}

function MenuItem({ label, meta, shortcut, active, danger, mono }) {
  return (
    <button style={{
      ...ssStyles.menuItem,
      color: danger ? 'var(--bad)' : 'var(--ink)',
      background: active ? 'var(--paper-2)' : 'transparent',
    }}>
      <span style={{ flex: 1, fontSize: 12.5 }}>{label}</span>
      {meta && <span className={mono ? 'mono' : undefined} style={{ fontSize: 10.5, color: 'var(--ink-3)' }}>{meta}</span>}
      {shortcut && <span className="mono" style={ssStyles.shortcut}>{shortcut}</span>}
    </button>
  );
}

function ThreadItem({ t }) {
  const phase = PHASES[t.phase];
  const isActive = t.active;
  return (
    <button style={{...ssStyles.thread, ...(isActive ? ssStyles.threadActive : {})}}>
      <div style={ssStyles.threadRail}>
        {isActive && <div style={ssStyles.threadRailFill}/>}
      </div>
      <div style={{ flex: 1, minWidth: 0 }}>
        <div style={{ fontSize: 12.5, fontWeight: isActive ? 500 : 400, whiteSpace: 'nowrap', overflow: 'hidden', textOverflow: 'ellipsis', color: 'var(--ink)' }}>
          {t.title}
        </div>
        <div style={ssStyles.threadMeta}>
          <span style={{
            fontFamily: "'JetBrains Mono', monospace",
            fontSize: 10,
            letterSpacing: '0.03em',
            color: isActive ? 'var(--accent)' : 'var(--ink-3)'
          }}>
            P{t.phase} · {phase.label}
          </span>
          <span style={{ color: 'var(--ink-3)', fontSize: 11 }}>{t.ago}</span>
        </div>
      </div>
    </button>
  );
}

const ssStyles = {
  aside: {
    flexShrink: 0, background: 'var(--paper-2)',
    borderRight: '1px solid var(--rule)',
    display: 'flex', flexDirection: 'column', padding: '14px 12px',
  },
  brand: { display: 'flex', alignItems: 'center', gap: 10, padding: '2px 4px 14px' },
  brandMark: {
    width: 28, height: 28, borderRadius: 6, background: 'var(--ink)',
    color: 'var(--paper)', display: 'grid', placeItems: 'center',
  },
  newBtn: {
    display: 'flex', alignItems: 'center', gap: 8, width: '100%',
    padding: '8px 10px', border: '1px solid var(--rule-2)', borderRadius: 6,
    background: 'var(--paper)', fontSize: 12.5, fontWeight: 500,
    color: 'var(--ink)', marginBottom: 10,
  },
  kbd: {
    marginLeft: 'auto', fontFamily: "'JetBrains Mono', monospace",
    fontSize: 10, color: 'var(--ink-3)',
    padding: '2px 5px', background: 'var(--paper-2)', borderRadius: 3,
    border: '1px solid var(--rule)',
  },
  searchWrap: {
    display: 'flex', alignItems: 'center', gap: 6,
    padding: '6px 10px', background: 'var(--paper)',
    border: '1px solid var(--rule)', borderRadius: 6,
    color: 'var(--ink-3)', marginBottom: 14,
  },
  search: { flex: 1, border: 'none', outline: 'none', background: 'transparent', fontSize: 12.5, color: 'var(--ink)' },
  sectionLabel: {
    fontSize: 10, letterSpacing: '0.08em', textTransform: 'uppercase',
    color: 'var(--ink-3)', padding: '0 4px 8px', fontWeight: 500,
  },
  thread: {
    width: '100%', display: 'flex', gap: 8, alignItems: 'flex-start',
    padding: '8px 8px 8px 6px', borderRadius: 6, textAlign: 'left',
    marginBottom: 2,
  },
  threadActive: { background: 'var(--paper)' },
  threadRail: {
    width: 2, alignSelf: 'stretch', background: 'transparent',
    position: 'relative', marginTop: 3, marginBottom: 3, borderRadius: 2,
  },
  threadRailFill: { position: 'absolute', inset: 0, background: 'var(--accent)', borderRadius: 2 },
  threadMeta: { display: 'flex', gap: 8, alignItems: 'center', marginTop: 3 },
  footer: { borderTop: '1px solid var(--rule)', paddingTop: 10, marginTop: 8 },
  workspace: {
    display: 'flex', alignItems: 'center', gap: 10,
    padding: '4px 2px', color: 'var(--ink-2)',
  },
  avatar: {
    width: 26, height: 26, borderRadius: '50%',
    background: 'var(--ink)', color: 'var(--paper)',
    display: 'grid', placeItems: 'center',
    fontSize: 10, fontWeight: 600, letterSpacing: '0.02em',
    cursor: 'pointer', flexShrink: 0,
  },
  menu: {
    position: 'absolute', bottom: 'calc(100% + 8px)', left: 0,
    width: 240, background: 'var(--paper)',
    border: '1px solid var(--rule-2)', borderRadius: 8,
    boxShadow: '0 12px 32px -12px rgba(0,0,0,0.18), 0 2px 6px rgba(0,0,0,0.05)',
    padding: 6, zIndex: 40,
  },
  menuHead: {
    display: 'flex', alignItems: 'center', gap: 10,
    padding: '6px 8px 10px', borderBottom: '1px solid var(--rule)',
    marginBottom: 4,
  },
  menuGroup: { display: 'flex', flexDirection: 'column', gap: 1 },
  menuSep: { height: 1, background: 'var(--rule)', margin: '5px -6px' },
  menuItem: {
    display: 'flex', alignItems: 'center', gap: 8,
    padding: '6px 8px', borderRadius: 4,
    textAlign: 'left', width: '100%',
  },
  shortcut: {
    fontSize: 10, color: 'var(--ink-3)',
    padding: '1px 5px', borderRadius: 3,
    background: 'var(--paper-2)', border: '1px solid var(--rule)',
  },
};

window.Sidebar = Sidebar;
