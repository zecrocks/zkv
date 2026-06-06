// chrome.jsx: Topbar, Sidebar, StatusBar
// Shared chrome elements that live in the app shell.

// Middle-truncate a string to at most `max` characters, keeping a 2:1
// head:tail split (mirroring the old fixed 16/8) so both ends of an address
// stay recognizable. The ellipsis costs one of the `max` slots.
const middleTruncate = (s: string, max: number) => {
  if (s.length <= max) return s;
  if (max <= 1) return max === 1 ? '…' : '';
  const keep = max - 1;
  const head = Math.ceil((keep * 2) / 3);
  const tail = keep - head;
  return s.slice(0, head) + '…' + (tail > 0 ? s.slice(-tail) : '');
};

// Width of one monospace glyph in the element's computed font, measured off a
// reusable canvas so re-fitting on resize never touches layout.
let _charCanvas: HTMLCanvasElement | null = null;
const monoCharWidth = (el: HTMLElement) => {
  const cs = getComputedStyle(el);
  _charCanvas = _charCanvas || document.createElement('canvas');
  const ctx = _charCanvas.getContext('2d')!;
  ctx.font = `${cs.fontWeight} ${cs.fontSize} ${cs.fontFamily}`;
  return ctx.measureText('0').width || 7;
};

// The database-address pill. It middle-truncates to the *measured* width of
// its field (which flexes with the topbar/window) rather than a fixed length,
// re-fitting on every resize so the visible portion always fills the field.
const DbAddr = ({ address }: { address: string }) => {
  const ref = React.useRef<HTMLSpanElement>(null);
  const [text, setText] = React.useState(address);
  React.useLayoutEffect(() => {
    const el = ref.current;
    if (!el) return;
    const fit = () => {
      const max = Math.floor(el.clientWidth / monoCharWidth(el));
      setText(max > 0 ? middleTruncate(address, max) : '');
    };
    fit();
    const ro = new ResizeObserver(fit);
    ro.observe(el);
    return () => ro.disconnect();
  }, [address]);
  return <span className="db-addr" ref={ref} title={address}>{text}</span>;
};

// Testnet funds are TAZ, mainnet funds are ZEC. Use everywhere a balance/fee is shown.
const currencyFor = (network?: string | null) => network === 'testnet' ? 'TAZ' : 'ZEC';
window.currencyFor = currencyFor;

// Exact base-unit amount → ZEC/TAZ string (8 dp, trailing zeros trimmed to >= 5 dp).
const formatZats = (zats: number | null | undefined, network?: string | null) => {
  if (zats == null) return '—';
  const zec = (Number(zats) / 1e8).toFixed(8).replace(/0+$/, '').replace(/\.$/, '.0');
  return `${zec} ${currencyFor(network)}`;
};
window.formatZats = formatZats;

// Relative "time ago" label for a unix-seconds timestamp, e.g. "just now",
// "5 minutes ago", "3 days ago". Shared by the dashboard cards and the Browse
// table's Updated column; chrome loads before both, so both reference it bare.
const fmtAgo = (ts: number | null | undefined): string | null => {
  if (ts == null) return null;
  const diff = Math.floor(Date.now() / 1000) - ts;
  if (diff < 10) return 'just now';
  if (diff < 60) return diff + ' seconds ago';
  const m = Math.floor(diff / 60);
  if (m < 60) return m === 1 ? '1 minute ago' : m + ' minutes ago';
  const h = Math.floor(diff / 3600);
  if (h < 24) return h === 1 ? '1 hour ago' : h + ' hours ago';
  const d = Math.floor(diff / 86400);
  if (d < 30) return d === 1 ? '1 day ago' : d + ' days ago';
  const mo = Math.floor(diff / 2592000);
  if (mo < 12) return mo === 1 ? '1 month ago' : mo + ' months ago';
  const y = Math.floor(diff / 31536000);
  return y === 1 ? '1 year ago' : y + ' years ago';
};

// macOS spells the command modifier ⌘; every other platform uses Ctrl. The web
// UI only ever serves localhost and the desktop build is a local webview, so
// the browser's own platform IS the user's platform. Prefer the modern
// userAgentData (Chromium: WebView2, Chrome/Edge), fall back to the legacy
// navigator.platform (WebKit: macOS/Linux desktop webview, Safari). Used by the
// Find trigger hint and the keyboard-shortcuts reference so the displayed key
// matches the one that actually works on this machine.
const IS_MAC: boolean = (() => {
  try {
    const nav = navigator as any;
    const p = (nav.userAgentData && nav.userAgentData.platform) || nav.platform || '';
    return /mac/i.test(p);
  } catch {
    return false;
  }
})();
window.IS_MAC = IS_MAC;
// The command-modifier label for this platform: "⌘" on macOS, "Ctrl" elsewhere.
const MOD_KEY: string = IS_MAC ? '⌘' : 'Ctrl';
window.MOD_KEY = MOD_KEY;

// ============ TOPBAR ============
const Topbar = ({ db, view, onCmd, onCopy, onDeposit, onSend, syncing, syncedBlock, networkLatency }: {
  db: ActiveDb | null;
  view: string;
  onCmd: () => void;
  onCopy: (s: string) => void;
  onDeposit?: () => void;
  onSend?: () => void;
  syncing?: boolean;
  syncedBlock?: number | null;
  networkLatency?: number | null;
}) => {
  const showDb = view === 'keys' && db;
  return (
    <div className={'topbar' + (showDb ? ' has-db' : '')}>
      <div className="topbar-logo">
        <img src="design-system/assets/logo-mark-dark.svg" width="22" height="22" alt="zkv"/>
        <span className="wordmark"><span>z</span><span className="colon">:</span><span>kv</span></span>
      </div>

      {showDb && (
        <>
          <span className="topbar-divider"></span>
          <div className="db-identity" title={db.address}>
            <Icon name={db.role === 'admin' ? 'database' : 'eye'} size={14}
                  className="db-icon"
                  color={db.role === 'admin' ? 'currentColor' : 'var(--fg-3)'} />
            <span className="db-name">{db.name}</span>
            <span className="db-role" data-role={db.role}>{db.role}</span>
            {db.pool && <span className="db-pool">{db.pool}</span>}
            <DbAddr address={db.address} />
            <button className="btn ghost sm" title="Copy address"
                    onClick={() => onCopy && onCopy(db.address)}>
              <Icon name="copy" className="icon" />
            </button>
            {db.role === 'admin' && onDeposit && (
              <button className="btn secondary sm" title="Deposit funds" onClick={onDeposit}>
                <Icon name="qr-code" className="icon" /> Deposit
              </button>
            )}
            {db.role === 'admin' && onSend && (
              <button className="btn secondary sm" title="Send ZEC to any address" onClick={onSend}>
                <Icon name="send" className="icon" /> Send
              </button>
            )}
          </div>
        </>
      )}

      {!showDb && view !== 'create' && view !== 'import' && (
        <>
          <span className="topbar-divider"></span>
          <span className="view-label">
            {view === 'dashboard' && (<><Icon name="layout-dashboard" size={13} /> Dashboard</>)}
            {view === 'settings'  && (<><Icon name="settings" size={13} /> Settings</>)}
          </span>
        </>
      )}

      <div className="topbar-spacer"></div>

      <div className="cmd-trigger" onClick={onCmd}>
        <Icon name="search" size={13} />
        <span>Find…</span>
        <kbd>{IS_MAC ? '⌘K' : 'Ctrl+K'}</kbd>
      </div>
    </div>
  );
};

// ============ SIDEBAR ============
const Sidebar = ({ view, onSelectView, databases, sidebarDbs, truncated, totalCount, onViewAll, activeName, onSelect, onCreate, onImport }: {
  view: string;
  onSelectView: (v: string) => void;
  databases: DbSummary[];
  sidebarDbs: DbSummary[];
  truncated: boolean;
  totalCount: number;
  onViewAll: () => void;
  activeName: string | null;
  onSelect: (name: string) => void;
  onCreate: () => void;
  onImport: () => void;
}) => {
  // One database row, used by both the grouped (<=10) and recent (>10) views.
  const dbItem = (d: DbSummary) => (
    <div key={d.name}
      className={'sidebar-item' + (view === 'keys' && d.name === activeName ? ' active' : '')}
      onClick={() => onSelect(d.name)}>
      <Icon name={d.role === 'admin' ? 'database' : 'eye'} size={14}
            color={d.role === 'admin' ? undefined : 'var(--fg-3)'} />
      <span>{d.name}</span>
      {d.unsynced > 0 && <span className="unread-dot" title={`${d.unsynced} pending`}></span>}
      {d.network === 'testnet' && <span className="net-tag" title="Testnet database">T</span>}
      {d.paused && <PauseGlyph size={11} style={{color:'var(--fg-3)'}} title="Auto-sync paused" />}
      <span className="meta">{d.keys}</span>
    </div>
  );
  return (
    <aside className="sidebar">
      <div className="sidebar-actions">
        <button className="btn secondary sm" onClick={onCreate}><Icon name="plus" className="icon" /> Create</button>
        <button className="btn secondary sm" onClick={onImport}><Icon name="download" className="icon" /> Import</button>
      </div>

      <div className="sidebar-section">
        <div className="sidebar-heading"><span>Workspace</span></div>
        <div className={'sidebar-item' + (view === 'dashboard' ? ' active' : '')}
             onClick={() => onSelectView('dashboard')}>
          <Icon name="layout-dashboard" size={14} />
          <span>Dashboard</span>
        </div>
        {/* Discover temporarily disabled. */}
      </div>

      {truncated ? (
        // Edge case: too many databases to list. Show the most recently
        // updated and route the rest through the search palette.
        <div className="sidebar-section">
          <div className="sidebar-heading"><span>Databases · recently updated</span></div>
          {(sidebarDbs || []).map(dbItem)}
          <div className="sidebar-item" onClick={onViewAll} style={{color:'var(--fg-3)'}} title="Search all databases">
            <Icon name="search" size={14} color="var(--fg-3)" />
            <span>View all databases ({totalCount})</span>
          </div>
        </div>
      ) : (
        <>
          {databases.some(d => d.role === 'admin') && (
            <div className="sidebar-section">
              <div className="sidebar-heading"><span>Writable</span></div>
              {databases.filter(d => d.role === 'admin').map(dbItem)}
            </div>
          )}

          {databases.some(d => d.role === 'watch') && (
            <div className="sidebar-section">
              <div className="sidebar-heading"><span>Viewing</span></div>
              {databases.filter(d => d.role === 'watch').map(dbItem)}
            </div>
          )}
        </>
      )}

      <div className="sidebar-section">
        <div className="sidebar-heading"><span>Local</span></div>
        <div className={'sidebar-item' + (view === 'reference' ? ' active' : '')}
             onClick={() => onSelectView('reference')}>
          <Icon name="book-open" size={14} color="var(--fg-3)" />
          <span>Reference</span>
        </div>
        <div className={'sidebar-item' + (view === 'settings' ? ' active' : '')}
             onClick={() => onSelectView('settings')}>
          <Icon name="settings" size={14} color="var(--fg-3)" />
          <span>Settings</span>
        </div>
      </div>
    </aside>
  );
};

// ============ STATUSBAR ============
// Single home for ambient network state. All values are live from the
// /api/status poll (network, chain tip, synced height, latency, server)
// plus the active admin DB's balance.
const StatusBar = ({ db, synced, isSynced, latency, syncing, networkBlock, network, server, version, gitSha, onDeposit, pausedAll, onTogglePauseAll }: {
  db: ActiveDb | null;
  synced: number | null;
  isSynced: boolean;
  latency: number | null;
  syncing?: boolean;
  networkBlock: number | null;
  network: string | null;
  server: string | null;
  version: string | number | null;
  gitSha: string | null;
  onDeposit?: () => void;
  pausedAll: boolean;
  onTogglePauseAll: () => void;
}) => {
  // Click the version chip to reveal the build's git SHA (truncated, GitHub
  // style); click again to toggle back to the version.
  const [showSha, setShowSha] = React.useState(false);
  return (
  <div className="statusbar">
    {/* Order: network·state | balance | block heights | ping·server. The pool
        (orchard/sapling) moved to the topbar's db identity. The widest, most
        variable group (latency + server) sits last so its changes don't shove
        the others around. */}
    <div className="group">
      {/* Within 1 block of the tip counts as synced, so the bar stays steady
          instead of flapping as the background loop advances. */}
      <span className={'dot' + (isSynced ? '' : ' amber')}></span>
      <span>{network || 'mainnet'}</span>
      <span style={{opacity:0.5}}>·</span>
      <span>{isSynced ? 'synced' : 'syncing'}</span>
    </div>
    {db && db.role === 'admin' && db.balance != null && (
      <>
        <span className="sep"></span>
        <div
          className="group"
          onClick={onDeposit}
          style={{ cursor: onDeposit ? 'pointer' : 'default' }}
          title="Add funds, show deposit QR"
        >
          <Icon name="coins" size={11} />
          <span>{formatZats(db.balance, db.network)}</span>
          {db.confirming! > 0 && (
            <span style={{ color: 'var(--amber-300)', opacity: 0.85 }}
                  title="Funds still confirming, included in the balance but not yet spendable">
              (confirming: {formatZats(db.confirming, db.network)})
            </span>
          )}
          {onDeposit && <Icon name="download" size={10} />}
        </div>
      </>
    )}
    <span className="sep"></span>
    <div className="group">
      <Icon name="git-branch" size={11} />
      <span>blk {synced ? synced.toLocaleString() : '—'}</span>
      {!isSynced && synced! > 0 && (
        <span style={{color:'var(--amber-300)'}}>/ {networkBlock!.toLocaleString()} ↑</span>
      )}
    </div>
    <span className="sep"></span>
    <div className="group">
      <Icon name="zap" size={11} />
      <span>{latency != null ? latency + 'ms · ' : ''}{server || 'lightwalletd'}</span>
    </div>
    <div className="right">
      <button
        className="btn ghost sm"
        onClick={onTogglePauseAll}
        style={{ color: 'inherit' }}
        title={pausedAll ? 'Resume auto-sync' : 'Pause auto-sync'}
      >
        {pausedAll ? <Icon name="play" size={11} /> : <PauseGlyph size={11} />} {pausedAll ? 'Resume all syncing' : 'Pause all syncing'}
      </button>
      <span
        onClick={() => gitSha && setShowSha(s => !s)}
        style={{ cursor: gitSha ? 'pointer' : 'default' }}
        title={gitSha ? (showSha ? 'Show version' : 'Show build commit') : undefined}
      >
        {showSha && gitSha
          ? `git: ${gitSha.slice(0, 7)}${gitSha.endsWith('-dirty') ? '-dirty' : ''}`
          : version ? `zkv v${version}` : 'zkv'}
      </span>
    </div>
  </div>
  );
};

window.Topbar = Topbar;
window.Sidebar = Sidebar;
window.StatusBar = StatusBar;
