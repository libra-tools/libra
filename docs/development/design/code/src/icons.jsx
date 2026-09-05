// Minimal line icons, 1.5px stroke, 16px default
const Icon = ({ d, size = 16, fill = 'none', stroke = 'currentColor', sw = 1.5, ...rest }) => (
  <svg width={size} height={size} viewBox="0 0 24 24" fill={fill} stroke={stroke}
    strokeWidth={sw} strokeLinecap="round" strokeLinejoin="round" {...rest}>
    {typeof d === 'string' ? <path d={d} /> : d}
  </svg>
);

const IconPlus    = (p) => <Icon {...p} d="M12 5v14M5 12h14" />;
const IconSearch  = (p) => <Icon {...p} d={<><circle cx="11" cy="11" r="7"/><path d="m20 20-3.5-3.5"/></>} />;
const IconSettings= (p) => <Icon {...p} d={<><circle cx="12" cy="12" r="3"/><path d="M12 2v2M12 20v2M4.93 4.93l1.41 1.41M17.66 17.66l1.41 1.41M2 12h2M20 12h2M4.93 19.07l1.41-1.41M17.66 6.34l1.41-1.41"/></>} />;
const IconThread  = (p) => <Icon {...p} d={<><path d="M21 15a2 2 0 0 1-2 2H7l-4 4V5a2 2 0 0 1 2-2h14a2 2 0 0 1 2 2z"/></>} />;
const IconBranch  = (p) => <Icon {...p} d={<><circle cx="6" cy="5" r="2"/><circle cx="6" cy="19" r="2"/><circle cx="18" cy="12" r="2"/><path d="M6 7v10M6 12h8a2 2 0 0 0 2-2V7"/></>} />;
const IconCheck   = (p) => <Icon {...p} d="M5 12l5 5 9-11" />;
const IconDot     = (p) => <Icon {...p} d={<circle cx="12" cy="12" r="3" fill="currentColor" stroke="none"/>} />;
const IconPlay    = (p) => <Icon {...p} d={<polygon points="7 4 20 12 7 20 7 4" fill="currentColor" stroke="none"/>} />;
const IconClock   = (p) => <Icon {...p} d={<><circle cx="12" cy="12" r="9"/><path d="M12 7v5l3 2"/></>} />;
const IconSpark   = (p) => <Icon {...p} d="M12 3v4M12 17v4M3 12h4M17 12h4M6 6l2.5 2.5M15.5 15.5L18 18M6 18l2.5-2.5M15.5 8.5L18 6" />;
const IconArrow   = (p) => <Icon {...p} d="M5 12h14M13 6l6 6-6 6" />;
const IconAt      = (p) => <Icon {...p} d={<><circle cx="12" cy="12" r="4"/><path d="M16 12v2a3 3 0 0 0 6 0v-2a10 10 0 1 0-4 8"/></>} />;
const IconFile    = (p) => <Icon {...p} d="M14 3H6a2 2 0 0 0-2 2v14a2 2 0 0 0 2 2h12a2 2 0 0 0 2-2V9zM14 3v6h6" />;
const IconChev    = (p) => <Icon {...p} d="M9 6l6 6-6 6" />;
const IconMore    = (p) => <Icon {...p} d={<><circle cx="5" cy="12" r="1.2" fill="currentColor"/><circle cx="12" cy="12" r="1.2" fill="currentColor"/><circle cx="19" cy="12" r="1.2" fill="currentColor"/></>} />;
const IconCopy    = (p) => <Icon {...p} d={<><rect x="9" y="9" width="12" height="12" rx="2"/><path d="M5 15V5a2 2 0 0 1 2-2h10"/></>} />;
const IconX       = (p) => <Icon {...p} d="M6 6l12 12M18 6L6 18" />;
const IconGit     = (p) => <Icon {...p} d={<><circle cx="6" cy="6" r="2"/><circle cx="6" cy="18" r="2"/><circle cx="18" cy="12" r="2"/><path d="M6 8v8M8 6h5a3 3 0 0 1 3 3v1"/></>} />;
const IconShield  = (p) => <Icon {...p} d="M12 3l8 3v6c0 5-3.5 8.5-8 9-4.5-.5-8-4-8-9V6z" />;
const IconFlask   = (p) => <Icon {...p} d="M9 3h6M10 3v5L4 20a1 1 0 0 0 1 1h14a1 1 0 0 0 1-1l-6-12V3" />;
const IconSend    = (p) => <Icon {...p} d="M4 12l16-8-6 18-3-8z" />;
const IconBook    = (p) => <Icon {...p} d="M4 4h9a3 3 0 0 1 3 3v13H7a3 3 0 0 1-3-3zM20 4h-4a3 3 0 0 0-3 3v13h4a3 3 0 0 0 3-3z" />;
const IconPaint   = (p) => <Icon {...p} d={<><circle cx="12" cy="12" r="9"/><circle cx="7.5" cy="10" r="1" fill="currentColor"/><circle cx="11" cy="7" r="1" fill="currentColor"/><circle cx="15.5" cy="8.5" r="1" fill="currentColor"/><path d="M12 21c-1 0-2-1-2-2s1-1 1-2-1-1-1-2 2-2 3-2h5"/></>} />;
const IconDiff    = (p) => <Icon {...p} d={<><path d="M8 3v12M4 7l4-4 4 4"/><path d="M16 21V9M20 17l-4 4-4-4"/></>} />;

const IconTerm    = (p) => <Icon {...p} d={<><rect x="3" y="5" width="18" height="14" rx="2"/><path d="M7 10l3 2-3 2M13 14h4"/></>} />;
const IconTool    = (p) => <Icon {...p} d="M14.7 6.3a4 4 0 0 0-5.4 5.4L3 18v3h3l6.3-6.3a4 4 0 0 0 5.4-5.4l-2.5 2.5-2.5-2.5 2.5-2.5z" />;
const IconDatabase= (p) => <Icon {...p} d={<><ellipse cx="12" cy="5" rx="8" ry="3"/><path d="M4 5v6c0 1.7 3.6 3 8 3s8-1.3 8-3V5"/><path d="M4 11v6c0 1.7 3.6 3 8 3s8-1.3 8-3v-6"/></>} />;

Object.assign(window, {
  IconPlus, IconSearch, IconSettings, IconThread, IconBranch, IconCheck,
  IconDot, IconPlay, IconClock, IconSpark, IconArrow, IconAt, IconFile,
  IconChev, IconMore, IconCopy, IconX, IconGit, IconShield, IconFlask,
  IconSend, IconBook, IconPaint, IconDiff,
  IconTerm, IconTool, IconDatabase,
});
