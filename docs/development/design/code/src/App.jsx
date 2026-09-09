// Min/max bounds for the two resizable panels.
const SIDEBAR_MIN = 180, SIDEBAR_MAX = 420;
const WORKFLOW_MIN = 420, WORKFLOW_MAX = 980;
const CHAT_MIN = 360;

function App() {
  const [tweaks, setTweaks] = React.useState(false);
  const [sidebarW, setSidebarW]   = React.useState(() => readNum('libra.sidebarW', 248));
  const [workflowW, setWorkflowW] = React.useState(() => readNum('libra.workflowW', 660));

  React.useEffect(() => { writeNum('libra.sidebarW', sidebarW); }, [sidebarW]);
  React.useEffect(() => { writeNum('libra.workflowW', workflowW); }, [workflowW]);

  React.useEffect(() => {
    // Apply persisted accent
    if (window.__TWEAKS__?.accent) {
      document.documentElement.style.setProperty('--accent', window.__TWEAKS__.accent);
    }
    function onMsg(e) {
      if (!e.data || typeof e.data !== 'object') return;
      if (e.data.type === '__activate_edit_mode')   setTweaks(true);
      if (e.data.type === '__deactivate_edit_mode') setTweaks(false);
    }
    window.addEventListener('message', onMsg);
    window.parent.postMessage({ type: '__edit_mode_available' }, '*');
    return () => window.removeEventListener('message', onMsg);
  }, []);

  // Drag handlers clamp so chat never gets narrower than CHAT_MIN.
  function onDragSidebar(dx, startW) {
    const total = window.innerWidth;
    const max = Math.min(SIDEBAR_MAX, total - workflowW - CHAT_MIN);
    setSidebarW(clamp(startW + dx, SIDEBAR_MIN, max));
  }
  function onDragWorkflow(dx, startW) {
    const total = window.innerWidth;
    const max = Math.min(WORKFLOW_MAX, total - sidebarW - CHAT_MIN);
    setWorkflowW(clamp(startW - dx, WORKFLOW_MIN, max));
  }

  return (
    <div style={{ display: 'flex', height: '100vh', width: '100%' }}>
      <Sidebar width={sidebarW}/>
      <Splitter onDrag={onDragSidebar} value={sidebarW}/>
      <Chat/>
      <Splitter onDrag={onDragWorkflow} value={workflowW}/>
      <Workflow width={workflowW}/>
      <Tweaks visible={tweaks} onClose={() => setTweaks(false)}/>
    </div>
  );
}

function Splitter({ onDrag, value }) {
  const [hover, setHover] = React.useState(false);
  const [drag, setDrag] = React.useState(false);

  function onMouseDown(e) {
    e.preventDefault();
    const startX = e.clientX;
    const startW = value;
    setDrag(true);
    document.body.style.cursor = 'col-resize';
    document.body.style.userSelect = 'none';

    function onMove(ev) { onDrag(ev.clientX - startX, startW); }
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
        width: 1, flexShrink: 0,
        background: 'var(--rule)',
        cursor: 'col-resize',
        zIndex: 5,
      }}
    >
      {/* Wider invisible hit area */}
      <div style={{
        position: 'absolute', top: 0, bottom: 0,
        left: -3, right: -3,
      }}/>
      {/* Accent hairline on hover/drag */}
      <div style={{
        position: 'absolute', top: 0, bottom: 0,
        left: 0, right: 0,
        background: active ? 'var(--accent)' : 'transparent',
        transition: 'background 120ms',
      }}/>
    </div>
  );
}

function clamp(v, lo, hi) { return Math.max(lo, Math.min(hi, v)); }
function readNum(k, fallback) {
  try { const v = parseInt(localStorage.getItem(k), 10); return Number.isFinite(v) ? v : fallback; }
  catch { return fallback; }
}
function writeNum(k, v) { try { localStorage.setItem(k, String(v)); } catch {} }

ReactDOM.createRoot(document.getElementById('root')).render(<App/>);
