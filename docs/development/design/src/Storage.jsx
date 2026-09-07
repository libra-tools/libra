// Objects view — the `libra graph` TUI rendered for the web.
// Shows the SQLite-backed object store as a git-graph of commits,
// each expandable to reveal its tree → blobs.

function ObjectsView({ onOpen, activeDetail }) {
  const [filter, setFilter] = React.useState('all'); // all | commit | tree | blob | tag
  const [expanded, setExpanded] = React.useState({}); // commit oid -> bool

  function toggle(oid) {
    setExpanded(e => ({ ...e, [oid]: !e[oid] }));
  }

  const store = OBJECTS.store;
  const showCommits = filter === 'all' || filter === 'commit';

  return (
    <div style={obStyles.wrap}>
      {/* Store header */}
      <div style={obStyles.storeBar}>
        <div style={{ display: 'flex', alignItems: 'center', gap: 7 }}>
          <IconDatabase size={13}/>
          <span style={{ fontWeight: 600, fontSize: 12 }}>Object store</span>
          <span className="mono" style={obStyles.storePath}>{store.path}</span>
        </div>
        <div style={obStyles.storeStats}>
          <Stat n={store.count} l="objects"/>
          <Stat n={store.loose} l="loose"/>
          <Stat n={store.packed} l="packed"/>
          <Stat n={store.size} l="on disk"/>
        </div>
      </div>

      {/* Filter segmented control */}
      <div style={obStyles.filterRow}>
        {[
          { k: 'all', label: 'All' },
          { k: 'commit', label: 'Commits' },
          { k: 'tree', label: 'Trees' },
          { k: 'blob', label: 'Blobs' },
          { k: 'tag', label: 'Tags' },
        ].map(f => (
          <button key={f.k} onClick={() => setFilter(f.k)}
            style={{ ...obStyles.filterBtn, ...(filter === f.k ? obStyles.filterOn : {}) }}>
            {f.label}
          </button>
        ))}
        <div style={{ flex: 1 }}/>
        <span className="mono" style={{ fontSize: 10, color: 'var(--ink-3)' }}>libra graph</span>
      </div>

      {/* TUI graph */}
      <div style={obStyles.tui}>
        {filter === 'tag'
          ? <TagList onOpen={onOpen} activeDetail={activeDetail}/>
          : showCommits
            ? OBJECTS.commits.map(c => (
                <CommitNode
                  key={c.oid}
                  c={c}
                  open={!!expanded[c.oid]}
                  onToggle={() => toggle(c.oid)}
                  onOpen={onOpen}
                  activeDetail={activeDetail}
                  filter={filter}
                />
              ))
            : <FlatObjectList filter={filter} onOpen={onOpen} activeDetail={activeDetail}/>}
      </div>
    </div>
  );
}

function Stat({ n, l }) {
  return (
    <span style={{ display: 'inline-flex', alignItems: 'baseline', gap: 4 }}>
      <span className="mono" style={{ fontSize: 11, color: 'var(--ink)', fontWeight: 600 }}>{n}</span>
      <span style={{ fontSize: 10, color: 'var(--ink-3)' }}>{l}</span>
    </span>
  );
}

function CommitNode({ c, open, onToggle, onOpen, activeDetail, filter }) {
  const selected = activeDetail?.kind === 'object' && activeDetail.data?.oid === c.oid;
  return (
    <div>
      <div style={{ ...obStyles.row, background: selected ? 'var(--accent-soft)' : 'transparent' }}>
        <span style={obStyles.rail}>{c.rail}</span>
        <button onClick={onToggle} style={obStyles.caret} title={open ? 'Collapse' : 'Expand tree'}>
          <span style={{ display: 'inline-block', transform: open ? 'rotate(90deg)' : 'none', transition: 'transform 120ms', color: 'var(--ink-3)' }}>
            ▸
          </span>
        </button>
        <TypeBadge type="commit"/>
        <button onClick={() => onOpen({ kind: 'object', data: { ...c, type: 'commit' } })} style={obStyles.oidBtn}>
          {c.oid}
        </button>
        <span style={obStyles.msg}>{c.msg}</span>
        {c.refs && c.refs.map(r => <RefChip key={r} name={r}/>)}
        <span style={obStyles.meta}>{c.stat}</span>
        <span style={obStyles.when}>{c.when}</span>
      </div>
      {open && (
        <div style={obStyles.treeWrap}>
          <TreeEntries treeOid={c.tree} depth={0} onOpen={onOpen} activeDetail={activeDetail} filter={filter}/>
        </div>
      )}
    </div>
  );
}

function TreeEntries({ treeOid, depth, onOpen, activeDetail, filter }) {
  const [openSub, setOpenSub] = React.useState({});
  const entries = OBJECTS.trees[treeOid];
  if (!entries) {
    return (
      <div style={obStyles.treeRow}>
        <span style={obStyles.treeRail}>{railFor(depth)}└─</span>
        <span style={{ fontSize: 11, color: 'var(--ink-3)', fontStyle: 'italic' }}>
          tree {treeOid} not loaded
        </span>
      </div>
    );
  }
  return (
    <>
      {entries.map((e, i) => {
        const last = i === entries.length - 1;
        const isOpen = !!openSub[e.oid];
        const selected = activeDetail?.kind === 'object' && activeDetail.data?.oid === e.oid;
        const dim = filter === 'tree' && e.type === 'blob' ? 0.4
                  : filter === 'blob' && e.type === 'tree' ? 0.4 : 1;
        return (
          <div key={e.oid + i} style={{ opacity: dim }}>
            <div style={{ ...obStyles.treeRow, background: selected ? 'var(--accent-soft)' : 'transparent' }}>
              <span style={obStyles.treeRail}>{railFor(depth)}{last ? '└─' : '├─'}</span>
              {e.type === 'tree' ? (
                <button onClick={() => setOpenSub(s => ({ ...s, [e.oid]: !s[e.oid] }))} style={obStyles.caret}>
                  <span style={{ display: 'inline-block', transform: isOpen ? 'rotate(90deg)' : 'none', transition: 'transform 120ms', color: 'var(--ink-3)' }}>▸</span>
                </button>
              ) : <span style={{ width: 14, flexShrink: 0 }}/>}
              <TypeBadge type={e.type}/>
              <button onClick={() => onOpen({ kind: 'object', data: e })} style={obStyles.oidBtn}>{e.oid}</button>
              <span style={{ ...obStyles.msg, color: e.type === 'tree' ? 'var(--ink)' : 'var(--ink-2)' }}>
                {e.name}
                {e.changed && <span style={obStyles.changedDot} title="changed in working set"/>}
              </span>
              <span style={obStyles.meta}>
                {e.type === 'tree' ? `${e.entries} entries` : e.size}
              </span>
            </div>
            {e.type === 'tree' && isOpen && (
              <TreeEntries treeOid={e.oid} depth={depth + 1} onOpen={onOpen} activeDetail={activeDetail} filter={filter}/>
            )}
          </div>
        );
      })}
    </>
  );
}

// Flatten all trees/blobs across the store for the Trees/Blobs filters.
function FlatObjectList({ filter, onOpen, activeDetail }) {
  const seen = new Set();
  const rows = [];
  Object.entries(OBJECTS.trees).forEach(([toid, entries]) => {
    if (!seen.has(toid)) { seen.add(toid); rows.push({ type: 'tree', oid: toid, name: '(tree)', entries: entries.length }); }
    entries.forEach(e => {
      if (!seen.has(e.oid)) { seen.add(e.oid); rows.push(e); }
    });
  });
  const filtered = rows.filter(r => r.type === filter);
  return (
    <>
      {filtered.map((e, i) => {
        const selected = activeDetail?.kind === 'object' && activeDetail.data?.oid === e.oid;
        return (
          <div key={e.oid + i} style={{ ...obStyles.row, background: selected ? 'var(--accent-soft)' : 'transparent' }}>
            <span style={{ width: 14, flexShrink: 0 }}/>
            <TypeBadge type={e.type}/>
            <button onClick={() => onOpen({ kind: 'object', data: e })} style={obStyles.oidBtn}>{e.oid}</button>
            <span style={obStyles.msg}>{e.name}</span>
            <span style={obStyles.meta}>{e.type === 'tree' ? `${e.entries} entries` : e.size}</span>
          </div>
        );
      })}
    </>
  );
}

function TagList({ onOpen, activeDetail }) {
  const tags = OBJECTS.refs.filter(r => r.kind === 'tag');
  return (
    <>
      {tags.map(t => (
        <div key={t.name} style={obStyles.row}>
          <span style={{ width: 14, flexShrink: 0 }}/>
          <TypeBadge type="tag"/>
          <span style={obStyles.oidBtn}>{t.oid}</span>
          <span style={obStyles.msg}>{t.name}</span>
          <RefChip name={t.name} tag/>
        </div>
      ))}
    </>
  );
}

function TypeBadge({ type }) {
  const map = {
    commit: { c: 'var(--accent)', t: 'commit' },
    tree:   { c: 'var(--warn)',   t: 'tree' },
    blob:   { c: 'var(--ink-3)',  t: 'blob' },
    tag:    { c: 'var(--good)',   t: 'tag' },
  };
  const m = map[type] || map.blob;
  return (
    <span style={{
      fontFamily: "'JetBrains Mono', monospace", fontSize: 9,
      padding: '1px 5px', borderRadius: 3, letterSpacing: '0.04em',
      color: m.c, border: `1px solid color-mix(in oklch, ${m.c} 40%, var(--paper))`,
      background: `color-mix(in oklch, ${m.c} 9%, var(--paper))`,
      flexShrink: 0, textTransform: 'uppercase', fontWeight: 600,
      width: 46, textAlign: 'center',
    }}>{m.t}</span>
  );
}

function RefChip({ name, tag }) {
  const isHead = name === 'HEAD';
  const c = tag ? 'var(--good)' : isHead ? 'var(--accent)' : 'var(--ink-2)';
  return (
    <span style={{
      fontFamily: "'JetBrains Mono', monospace", fontSize: 9.5,
      padding: '1px 6px', borderRadius: 999, flexShrink: 0,
      color: c, border: `1px solid color-mix(in oklch, ${c} 45%, var(--paper))`,
      background: `color-mix(in oklch, ${c} 10%, var(--paper))`,
      display: 'inline-flex', alignItems: 'center', gap: 3,
    }}>
      {tag ? '⌑' : isHead ? '◆' : '⎇'} {name}
    </span>
  );
}

function railFor(depth) {
  // Indentation for nested tree levels, drawn with TUI pipes.
  let s = '';
  for (let i = 0; i < depth; i++) s += '│  ';
  return s;
}

const obStyles = {
  wrap: { display: 'flex', flexDirection: 'column' },
  storeBar: {
    display: 'flex', alignItems: 'center', justifyContent: 'space-between',
    padding: '10px 12px', border: '1px solid var(--rule)', borderRadius: 8,
    background: 'var(--paper-2)', marginBottom: 10, gap: 12, flexWrap: 'wrap',
  },
  storePath: {
    fontSize: 10.5, color: 'var(--ink-3)', padding: '1px 6px',
    background: 'var(--paper)', border: '1px solid var(--rule)', borderRadius: 3,
  },
  storeStats: { display: 'flex', alignItems: 'center', gap: 14 },
  filterRow: { display: 'flex', alignItems: 'center', gap: 4, marginBottom: 8 },
  filterBtn: {
    padding: '4px 10px', borderRadius: 5, fontSize: 11.5, fontWeight: 500,
    color: 'var(--ink-3)', border: '1px solid transparent',
  },
  filterOn: {
    color: 'var(--ink)', background: 'var(--paper-2)',
    border: '1px solid var(--rule-2)',
  },
  tui: {
    border: '1px solid var(--rule)', borderRadius: 8,
    background: 'var(--paper)', padding: '8px 4px', overflow: 'hidden',
  },
  row: {
    display: 'flex', alignItems: 'center', gap: 8,
    padding: '4px 10px', borderRadius: 4, minHeight: 28,
  },
  rail: {
    fontFamily: "'JetBrains Mono', monospace", fontSize: 13,
    color: 'var(--accent)', width: 32, flexShrink: 0, lineHeight: 1,
    whiteSpace: 'pre',
  },
  caret: { width: 14, flexShrink: 0, fontSize: 10, lineHeight: 1, textAlign: 'center' },
  oidBtn: {
    fontFamily: "'JetBrains Mono', monospace", fontSize: 11.5,
    color: 'var(--ink)', fontWeight: 600, flexShrink: 0,
    padding: '1px 4px', borderRadius: 3,
  },
  msg: {
    fontSize: 12, color: 'var(--ink-2)', flex: 1, minWidth: 0,
    whiteSpace: 'nowrap', overflow: 'hidden', textOverflow: 'ellipsis',
    display: 'flex', alignItems: 'center', gap: 6,
  },
  meta: {
    fontFamily: "'JetBrains Mono', monospace", fontSize: 10,
    color: 'var(--ink-3)', flexShrink: 0,
  },
  when: { fontSize: 10.5, color: 'var(--ink-3)', flexShrink: 0, width: 36, textAlign: 'right' },
  treeWrap: { paddingLeft: 0 },
  treeRow: {
    display: 'flex', alignItems: 'center', gap: 8,
    padding: '3px 10px', borderRadius: 4, minHeight: 26,
  },
  treeRail: {
    fontFamily: "'JetBrains Mono', monospace", fontSize: 12,
    color: 'var(--rule-2)', flexShrink: 0, whiteSpace: 'pre',
    paddingLeft: 32,
  },
  changedDot: {
    width: 5, height: 5, borderRadius: '50%', background: 'var(--accent)',
    display: 'inline-block', flexShrink: 0,
  },
};

window.ObjectsView = ObjectsView;
