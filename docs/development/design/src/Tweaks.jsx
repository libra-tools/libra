function Tweaks({ visible, onClose }) {
  const [accent, setAccent] = React.useState(window.__TWEAKS__.accent);

  React.useEffect(() => {
    document.documentElement.style.setProperty('--accent', accent);
  }, [accent]);

  function set(val) {
    setAccent(val);
    window.parent.postMessage({ type: '__edit_mode_set_keys', edits: { accent: val } }, '*');
  }

  if (!visible) return null;
  const swatches = [
    { name: 'Blue',   val: 'oklch(0.62 0.14 250)' },
    { name: 'Ink',    val: 'oklch(0.38 0.02 260)' },
    { name: 'Violet', val: 'oklch(0.58 0.16 300)' },
    { name: 'Teal',   val: 'oklch(0.62 0.10 190)' },
    { name: 'Olive',  val: 'oklch(0.58 0.08 130)' },
    { name: 'Clay',   val: 'oklch(0.60 0.12 45)'  },
    { name: 'Rose',   val: 'oklch(0.60 0.14 20)'  },
  ];

  return (
    <div style={tStyles.panel}>
      <div style={tStyles.head}>
        <div style={{ display: 'flex', alignItems: 'center', gap: 6 }}>
          <IconPaint size={13}/>
          <span style={{ fontWeight: 600, fontSize: 12 }}>Tweaks</span>
        </div>
        <button onClick={onClose} style={{ color: 'var(--ink-3)' }}><IconX size={13}/></button>
      </div>
      <div style={{ padding: '10px 12px 12px' }}>
        <div style={tStyles.label}>Accent color</div>
        <div style={tStyles.swatches}>
          {swatches.map(s => {
            const on = s.val === accent;
            return (
              <button key={s.name} onClick={() => set(s.val)} title={s.name}
                style={{
                  ...tStyles.swatch,
                  background: s.val,
                  outline: on ? '2px solid var(--ink)' : 'none',
                  outlineOffset: 2,
                }}/>
            );
          })}
        </div>
      </div>
    </div>
  );
}

const tStyles = {
  panel: {
    position: 'fixed', right: 16, bottom: 16, zIndex: 50,
    width: 240, background: 'var(--paper)',
    border: '1px solid var(--rule-2)', borderRadius: 10,
    boxShadow: '0 10px 30px -10px rgba(0,0,0,0.15), 0 2px 6px rgba(0,0,0,0.04)',
  },
  head: {
    display: 'flex', alignItems: 'center', justifyContent: 'space-between',
    padding: '8px 12px', borderBottom: '1px solid var(--rule)',
  },
  label: {
    fontSize: 10, letterSpacing: '0.08em', textTransform: 'uppercase',
    color: 'var(--ink-3)', fontWeight: 500, marginBottom: 8,
  },
  swatches: { display: 'flex', gap: 8, flexWrap: 'wrap' },
  swatch: {
    width: 24, height: 24, borderRadius: 6, border: '1px solid rgba(0,0,0,0.08)',
    cursor: 'pointer',
  },
};

window.Tweaks = Tweaks;
