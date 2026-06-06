// discover.jsx: Public z:kv database directory + Settings + Onboarding + Palette

// ============================================================
// DISCOVER
// ============================================================
const DISCOVER_CATS = [
  { id: 'all',     label: 'All',         ct: 142 },
  { id: 'oracle',  label: 'Oracles',     ct: 28 },
  { id: 'config',  label: 'App config',  ct: 31 },
  { id: 'archive', label: 'Archives',    ct: 14 },
  { id: 'index',   label: 'Indexes',     ct: 22 },
  { id: 'demo',    label: 'Demos',       ct: 47 },
];

const FEATURED = [
  {
    cat: 'ORACLE', name: 'zec-usd-feed', by: 'electriccoin.eco',
    desc: 'ZEC/USD spot price, refreshed every block. Median across Coingecko, Kraken, Binance.',
    keys: 4, watchers: '8.2k', addr: 'zkv1ecq9p3k…7sm2kvfeed',
  },
  {
    cat: 'CONFIG', name: 'mobile-flags', by: 'shielded-labs',
    desc: 'Feature flag registry for the Shielded Labs mobile wallet. Updated on release.',
    keys: 31, watchers: '1.4k', addr: 'zkv1sfm4d7q…2kv9hflags',
  },
  {
    cat: 'ARCHIVE', name: 'zip-archive', by: 'zcash-foundation',
    desc: 'Append-only index of canonical ZIP titles and statuses. Read-only mirror.',
    keys: 132, watchers: '643', addr: 'zkv1zfx7a2c…9hqm5zips',
  },
];

const PUBLIC = [
  { cat: 'oracle',  name: 'gas-prices-eth', by: 'merklemanufactory', desc: 'Ethereum gas price gauge, 30-block moving average.', keys: 6, watchers: '4.1k', addr: 'zkv1mmk3p9w…q8wv2gas' },
  { cat: 'index',   name: 'zip-progress',    by: 'zcash-foundation', desc: 'Live state of all open Zcash Improvement Proposals.', keys: 47, watchers: '912',   addr: 'zkv1zfa2c7x…m5kq9zips' },
  { cat: 'archive', name: 'rpc-changelog',   by: 'electriccoin.eco', desc: 'Per-release JSON-RPC changelog for zcashd.', keys: 184, watchers: '503',   addr: 'zkv1ec7r9d2…k4xv8rpc' },
  { cat: 'oracle',  name: 'btc-ema-200',     by: 'foundryusa',        desc: 'BTC 200-day EMA. Updated daily at 00:00 UTC.', keys: 2, watchers: '2.0k', addr: 'zkv1fup4kx7…q9wm2ema' },
  { cat: 'config',  name: 'wallet-fees',     by: 'shielded-labs',     desc: 'Recommended fee tiers per wallet version. Read by 3 wallets.', keys: 14, watchers: '728', addr: 'zkv1sl9m2k4…x7vq8fee' },
  { cat: 'demo',    name: 'guestbook',       by: 'anonymous',         desc: 'Public guestbook from the zkv beta announcement. New entries every few hours.', keys: 1284, watchers: '4.8k', addr: 'zkv1axh6q3p…m9kv2book' },
  { cat: 'demo',    name: 'haiku-of-the-day', by: 'jr@local',         desc: 'One haiku, signed daily.', keys: 1, watchers: '47', addr: 'zkv1jrd8s5m…9kq2vhku' },
];

const Discover = ({ onWatch }: { onWatch: (entry?: { addr: string; name: string }) => void }) => {
  const [activeCat, setActiveCat] = React.useState('all');
  const [q, setQ] = React.useState('');

  const filtered = PUBLIC.filter(p =>
    (activeCat === 'all' || p.cat === activeCat) &&
    (q === '' || p.name.toLowerCase().includes(q.toLowerCase()) || p.desc.toLowerCase().includes(q.toLowerCase()))
  );

  return (
    <div className="discover">
      <div className="discover-header">
        <div style={{fontFamily:'IBM Plex Mono', fontSize:11, letterSpacing:'0.12em', textTransform:'uppercase', color:'var(--fg-3)'}}>WORKSPACE</div>
        <h2 style={{fontFamily:'IBM Plex Sans', fontWeight:600, fontSize:26, letterSpacing:'-0.01em', margin:'4px 0 8px', color:'var(--fg-1)'}}>Discover</h2>
        <div style={{fontSize:14, color:'var(--fg-2)', maxWidth:680, lineHeight:1.55}}>
          Browse public z:kv databases, oracles, app config, archives, demos. Watch any of them with one click; you'll see the same keys the publisher sees, signed and time-stamped on chain.
        </div>
        <div className="callout-flow note" style={{marginTop:16, maxWidth:680}}>
          <Icon name="info" size={16} color="var(--amber-400)" />
          <div>
            <strong style={{color:'inherit'}}>Sample directory.</strong> zkv has no on-chain registry yet, so the
            list below is illustrative. To watch a real database, use <strong>Import → Watch</strong> and paste its
            <code> zkv1…</code> address.
          </div>
        </div>
      </div>

      <div style={{margin:'24px 0 18px'}}>
        <div style={{fontFamily:'var(--font-mono)', fontSize:10, letterSpacing:'0.12em', textTransform:'uppercase', color:'var(--fg-3)', marginBottom:10}}>FEATURED · ORACLES</div>
        <div className="discover-featured">
          {FEATURED.map(f => (
            <div key={f.name} className="featured-card">
              <div className="featured-cat">{f.cat}</div>
              <div className="featured-name">{f.name}</div>
              <div className="featured-desc">{f.desc}</div>
              <div className="featured-foot">
                <span><span className="mono" style={{color:'var(--fg-1)'}}>{f.keys}</span> keys</span>
                <button className="btn primary sm" onClick={() => onWatch(f)}>
                  <Icon name="eye" className="icon"/> Watch
                </button>
              </div>
            </div>
          ))}
        </div>
      </div>

      <div className="discover-toolbar">
        <div className="discover-cats">
          {DISCOVER_CATS.map(c => (
            <button key={c.id}
              className={activeCat === c.id ? 'on' : ''}
              onClick={() => setActiveCat(c.id)}>
              {c.label} <span className="ct">{c.ct}</span>
            </button>
          ))}
        </div>
        <div style={{marginLeft:'auto', position:'relative'}}>
          <Icon name="search" size={12} style={{position:'absolute', left:10, top:9, color:'var(--fg-3)'}}/>
          <input className="input" style={{width:240, paddingLeft:30}}
                 placeholder="search public databases…"
                 value={q} onChange={e => setQ(e.target.value)} />
        </div>
      </div>

      <div className="discover-list">
        {filtered.map(p => (
          <div key={p.name} className="public-row">
            <div className="public-name-col">
              <div className="public-name">
                <Icon name="database" size={12} color="var(--amber-400)" />
                {p.name}
              </div>
              <div className="public-addr">
                <span>{p.addr}</span>
                <button className="addr-copy" title="Copy address"
                        onClick={(e) => { e.stopPropagation(); try { navigator.clipboard.writeText(p.addr); } catch {} }}>
                  <Icon name="copy" size={12} />
                </button>
              </div>
            </div>
            <div className="public-desc">{p.desc}</div>
            <div className="public-meta">
              <div><span className="lbl">KEYS</span><strong>{p.keys}</strong></div>
            </div>
            <div>
              <button className="btn secondary sm" onClick={() => onWatch(p)}>
                <Icon name="eye" className="icon"/> Watch
              </button>
            </div>
          </div>
        ))}
        {filtered.length === 0 && (
          <div className="empty" style={{padding:'48px 24px'}}>
            <div className="glyph"><Icon name="search-x" size={28}/></div>
            <div>No databases matching <code>"{q}"</code>{activeCat !== 'all' && ` in ${activeCat}`}.</div>
          </div>
        )}
      </div>

      <div className="discover-foot">
        Listing is opt-in. Publishers register at <code>https://discover.zkv.cash</code>, signed by the database's UFVK.
      </div>
    </div>
  );
};

// ============================================================
// SETTINGS
// ============================================================
// Full IANA zone list when the browser supports it; a small curated fallback
// otherwise. "UTC" and "local" are offered as explicit choices on top.
const TIME_ZONES = (() => {
  try {
    return (Intl as any).supportedValuesOf("timeZone");
  } catch (_) {
    return [
      "America/Los_Angeles", "America/Denver", "America/Chicago", "America/New_York",
      "America/Sao_Paulo", "Europe/London", "Europe/Berlin", "Europe/Moscow",
      "Asia/Kolkata", "Asia/Shanghai", "Asia/Tokyo", "Australia/Sydney",
    ];
  }
})();

// The Danger Zone's "Forget database" confirmation gate. Deleting a database
// only wipes the local cache: the confirmed writes live on the Zcash chain and
// stay readable by anyone holding the zkv1 address. Because it's destructive,
// the Forget button stays disabled until the user types FORGET verbatim.
const ForgetModal = ({ name, onClose, onConfirm }: {
  name: string;
  onClose: () => void;
  onConfirm: (name: string) => Promise<void>;
}) => {
  const [typed, setTyped] = React.useState("");
  const [busy, setBusy] = React.useState(false);
  const [error, setError] = React.useState<any>(null);
  const ready = typed.trim() === "FORGET";

  // Pull the db's detail so we can offer its zkv address for backup before the
  // local cache (and, for admin dbs, the only on-machine copy of the seed) is
  // wiped. Best-effort: a failure just hides the backup block.
  const [detail, setDetail] = React.useState<DbDetail | null>(null);
  React.useEffect(() => {
    let alive = true;
    window.zkvApi.detail(name).then((d) => { if (alive) setDetail(d); }).catch(() => {});
    return () => { alive = false; };
  }, [name]);
  const address = detail && detail.address;
  const isAdmin = !!(detail && detail.role === "admin");
  const copyText = (s: string) => { try { navigator.clipboard.writeText(s); } catch {} };

  const doForget = async () => {
    if (!ready || busy) return;
    setBusy(true);
    setError(null);
    try {
      await onConfirm(name);
      onClose(); // App refreshes the sidebar + clears the active db.
    } catch (e) {
      setError(e);
      setBusy(false);
    }
  };

  return (
    <div className="modal-overlay" onClick={(e) => { if ((e.target as HTMLElement).classList.contains("modal-overlay") && !busy) onClose(); }}>
      <div className="modal" role="dialog" aria-labelledby="forget-title" style={{ maxWidth: 460 }}>
        <div className="modal-head">
          <div>
            <div className="eyebrow">DB: {name}</div>
            <h2 id="forget-title">Forget database</h2>
          </div>
          {!busy && (
            <button className="close" onClick={onClose} title="Close"><Icon name="x" size={16} /></button>
          )}
        </div>
        <div className="modal-body">
          <div style={{ fontSize: 13, color: "var(--fg-2)", lineHeight: 1.55, marginBottom: 14 }}>
            This deletes your local cache of data stored on the Zcash blockchain.
            The data will remain visible to anyone who has the database's{" "}
            <code style={{ fontFamily: "var(--font-mono)", fontSize: 12 }}>zkv1</code> address.
            Confirm by typing <strong style={{ color: "var(--fg-1)" }}>FORGET</strong> below:
          </div>
          {address && (
            <div style={{ marginBottom: 14, padding: "10px 12px", background: "var(--bg-sunken)", borderRadius: "var(--radius-md)", border: "1px solid var(--border-1)" }}>
              <div style={{ fontFamily: "var(--font-mono)", fontSize: 10, letterSpacing: "0.08em", textTransform: "uppercase", color: "var(--fg-3)", marginBottom: 6 }}>
                Back up this address first
              </div>
              <CollapsibleString value={address} onCopy={copyText} />
            </div>
          )}
          {isAdmin && (
            <div className="callout-flow warn" style={{ marginBottom: 14 }}>
              <Icon name="alert-triangle" size={16} color="var(--amber-400)" />
              <div>
                If you didn't back up this database's <strong style={{ color: "inherit" }}>seed phrase</strong> when
                you created it, forgetting it here means you may never be able to write to it again. Save the{" "}
                <code style={{ fontFamily: "var(--font-mono)", fontSize: 12 }}>zkv1</code> address above so you can at
                least keep reading it.
              </div>
            </div>
          )}
          <input
            className="input mono lg"
            value={typed}
            onChange={(e) => setTyped(e.target.value)}
            placeholder="FORGET"
            autoFocus
            disabled={busy}
            spellCheck={false}
            autoCapitalize="off"
            autoCorrect="off"
            onKeyDown={(e) => { if (e.key === "Enter") doForget(); }}
          />
          {error && (
            <div style={{ marginTop: 12 }}>
              <ErrorMessage message={(error && error.message) || "could not forget database"} />
            </div>
          )}
        </div>
        <div className="modal-foot">
          <div className="cost">
            <Icon name="alert-triangle" size={11} color="var(--red-500)" />
            <span style={{ color: "var(--red-500)" }}>On-chain data is unaffected</span>
          </div>
          <div style={{ display: "flex", gap: 8 }}>
            <button className="btn secondary" onClick={onClose} disabled={busy}>Cancel</button>
            <button className="btn danger" disabled={!ready || busy} onClick={doForget}>
              {busy ? "Forgetting…" : "Forget"}
            </button>
          </div>
        </div>
      </div>
    </div>
  );
};

// Danger Zone "Show seed phrase": fetch and reveal an admin database's 24-word
// recovery phrase, rendered the same way the create flow shows it at init. The
// phrase is the local backup of the spending key, so the modal leads with a
// blunt warning and a "Never share" footer; it fetches on mount via the same
// `window.zkvApi` surface every other screen uses.
const RevealPhraseModal = ({ name, onClose }: {
  name: string;
  onClose: () => void;
}) => {
  const [phrase, setPhrase] = React.useState<string | null>(null);
  const [error, setError] = React.useState<any>(null);
  const [loading, setLoading] = React.useState(true);
  const [copied, setCopied] = React.useState(false);

  React.useEffect(() => {
    let live = true;
    window.zkvApi.revealPhrase(name)
      .then((r) => { if (live) setPhrase(r.phrase); })
      .catch((e) => { if (live) setError(e); })
      .finally(() => { if (live) setLoading(false); });
    return () => { live = false; };
  }, [name]);

  const words = phrase ? phrase.trim().split(/\s+/) : [];

  return (
    <div className="modal-overlay" onClick={(e) => { if ((e.target as HTMLElement).classList.contains("modal-overlay")) onClose(); }}>
      <div className="modal" role="dialog" aria-labelledby="reveal-title" style={{ maxWidth: 560 }}>
        <div className="modal-head">
          <div>
            <div className="eyebrow">DB: {name}</div>
            <h2 id="reveal-title">Secret recovery phrase</h2>
          </div>
          <button className="close" onClick={onClose} title="Close"><Icon name="x" size={16} /></button>
        </div>
        <div className="modal-body">
          <div className="form-stack">
            <div className="callout-flow warn">
              <Icon name="alert-triangle" size={16} color="var(--amber-400)" />
              <div>
                <strong style={{ color: "inherit" }}>Anyone with your seed phrase can edit your database, impersonate you, and spend your funds.</strong>{" "}
                Password-protected encrypted seed phrases are coming in a future release.
              </div>
            </div>
            {loading && (
              <div style={{ display: "flex", alignItems: "center", gap: 8, color: "var(--fg-2)", fontSize: 13 }}>
                <div className="spinner" /> Decrypting…
              </div>
            )}
            {error && (
              <ErrorMessage message={(error && error.message) || "could not reveal seed phrase"} />
            )}
            {phrase && (
              <>
                <div className="mnemonic-grid">
                  {words.map((w, i) => (
                    <div key={i} className="mnemonic-cell">
                      <span className="idx">{i + 1}</span>
                      <span className="word">{w}</span>
                    </div>
                  ))}
                </div>
                <div style={{ display: "flex", justifyContent: "flex-end" }}>
                  <button
                    className="btn ghost sm"
                    title="Copy the 24 words (space-separated) to the clipboard"
                    onClick={() => {
                      try { navigator.clipboard.writeText(words.join(" ")); } catch {}
                      setCopied(true);
                      setTimeout(() => setCopied(false), 1500);
                    }}
                  >
                    <Icon name="copy" className="icon" /> {copied ? "Copied" : "Copy phrase"}
                  </button>
                </div>
              </>
            )}
          </div>
        </div>
        <div className="modal-foot">
          <div className="cost">
            <Icon name="alert-triangle" size={11} color="var(--red-500)" />
            <span style={{ color: "var(--red-500)" }}>Never share these words with anyone</span>
          </div>
          <div style={{ display: "flex", gap: 8 }}>
            <button className="btn secondary" onClick={onClose}>Done</button>
          </div>
        </div>
      </div>
    </div>
  );
};

const Settings = ({
  theme, onTheme,
  timeZone, onTimeZone,
  onResetOnboarding,
  status,
  databases,
  onForget,
  onSyncWorkers,
  onViewLicenses,
  onViewShortcuts,
  onReimportDemo,
}: {
  theme: string;
  onTheme: (theme: string) => void;
  timeZone: string;
  onTimeZone: (tz: string) => void;
  onResetOnboarding: () => void;
  status: StatusResp | null;
  databases: DbSummary[];
  onForget: (name: string) => Promise<void>;
  onSyncWorkers: (n: number) => void;
  onViewLicenses: () => void;
  onViewShortcuts: () => void;
  onReimportDemo: () => void;
}) => {
  const workers = (status && status.sync_workers) || 5;
  const version = (status && status.version) || '';
  const platform = status && status.platform;

  // Danger Zone: the "Forget database" picker. `forgetPick` is the user's
  // choice; `forgetSel` falls back to the first db so the picker is never stuck
  // on a since-forgotten name. `forgetTarget` is non-null while the modal is up.
  const dbNames = (databases || []).map((d) => d.name);
  const [forgetPick, setForgetPick] = React.useState("");
  const [forgetTarget, setForgetTarget] = React.useState<string | null>(null);
  const forgetSel = forgetPick && dbNames.includes(forgetPick) ? forgetPick : (dbNames[0] || "");

  // Danger Zone: the "Show seed phrase" picker. Only admin databases hold a
  // seed, so watch-only ones are filtered out. Same fallback-to-first pattern
  // as the forget picker; `revealTarget` is non-null while the modal is up.
  const adminDbNames = (databases || []).filter((d) => d.role === "admin").map((d) => d.name);
  const [revealPick, setRevealPick] = React.useState("");
  const [revealTarget, setRevealTarget] = React.useState<string | null>(null);
  const revealSel = revealPick && adminDbNames.includes(revealPick) ? revealPick : (adminDbNames[0] || "");

  // Probe both networks' lightwalletd servers (endpoint, tip height, backend).
  // Independent of the per-database status poll: a server's chain tip is a
  // property of the network, not of any one database's sync progress, so this
  // is fetched once when the screen mounts (with a manual refresh).
  const [servers, setServers] = React.useState<ServersResp | null>(null);
  const [probing, setProbing] = React.useState(false);
  const loadServers = React.useCallback(() => {
    setProbing(true);
    window.zkvApi.servers()
      .then((s) => setServers(s))
      .catch(() => {})
      .finally(() => setProbing(false));
  }, []);
  React.useEffect(() => { loadServers(); }, [loadServers]);

  // A server row: endpoint · chain tip · backend+version. No per-database sync
  // state; the tip is shared across every database on that network.
  const serverRow = (label: string, row: ServerRow | null | undefined) => (
    <div className="settings-row">
      <div className="srl">{label}</div>
      <div className="srv" style={{gridColumn:'2 / 4'}}>
        {!row
          ? (probing ? 'probing…' : '—')
          : !row.online
            ? <span style={{color:'var(--fg-3)'}}>{row.server} · offline</span>
            : (
              <>
                <span>{row.server}</span>
                <span style={{opacity:0.5}}> · </span>
                <span>blk {row.block_height != null ? row.block_height.toLocaleString() : '—'}</span>
                <span style={{opacity:0.5}}> · </span>
                <span>{row.backend || '—'}</span>
              </>
            )}
      </div>
    </div>
  );

  return (
    <div className="settings">
      <div style={{marginBottom:32}}>
        <div style={{fontFamily:'IBM Plex Mono', fontSize:11, letterSpacing:'0.12em', textTransform:'uppercase', color:'var(--fg-3)'}}>WORKSPACE</div>
        <h2 style={{fontFamily:'IBM Plex Sans', fontWeight:600, fontSize:26, letterSpacing:'-0.01em', margin:'4px 0 0', color:'var(--fg-1)'}}>Settings</h2>
      </div>

      <div className="settings-section">
        <h3>Appearance</h3>
        <p className="lede">Theme and how timestamps are displayed.</p>
        <div className="settings-card">
          <div className="settings-row">
            <div className="srl">Time zone</div>
            <div className="srv"></div>
            <div className="ctl">
              <select
                className="input"
                value={timeZone}
                onChange={(e) => onTimeZone(e.target.value)}
                style={{ minWidth: 220, fontFamily: 'var(--font-mono)', fontSize: 12 }}
              >
                <option value="UTC">UTC</option>
                <option value="local">Local (system)</option>
                <optgroup label="Regions">
                  {TIME_ZONES.filter((z: string) => z !== 'UTC').map((z: string) => (
                    <option key={z} value={z}>{z}</option>
                  ))}
                </optgroup>
              </select>
            </div>
          </div>
          <div className="settings-row">
            <div className="srl">Theme</div>
            <div className="srv"></div>
            <div className="ctl">
              <div className="seg">
                <button className={theme === 'light' ? 'on' : ''} onClick={() => onTheme('light')}>Light</button>
                <button className={theme === 'dark' ? 'on' : ''} onClick={() => onTheme('dark')}>Dark</button>
              </div>
            </div>
          </div>
        </div>
      </div>

      <div className="settings-section">
        <h3>Network</h3>
        <p className="lede">
          The lightwalletd connection is chosen at launch, e.g.{' '}
          <code style={{fontFamily:'var(--font-mono)', fontSize:11}}>zkv gui --mainnet-server 1.2.3</code>{' '}
          (and <code style={{fontFamily:'var(--font-mono)', fontSize:11}}>--testnet-server</code>).
        </p>
        <div className="settings-card">
          {serverRow('Mainnet server', servers && servers.mainnet)}
          {serverRow('Testnet server', servers && servers.testnet)}
          <div className="settings-row">
            <div className="srl">Data directory</div>
            <div className="srv">{(servers && servers.data_dir) || '…'}</div>
            <div className="ctl">
              <button
                className="btn secondary sm"
                onClick={() => { window.zkvApi.openDataDir().catch(() => {}); }}
                title="Open the data directory in your file manager"
              >
                <Icon name="folder-open" className="icon"/> Open
              </button>
            </div>
          </div>
          <div className="settings-row">
            <div className="srl">Sync workers<span className="sub">How many databases sync in parallel in the background.</span></div>
            <div className="srv">{workers}</div>
            <div className="ctl">
              <select
                className="input"
                value={workers}
                onChange={(e) => onSyncWorkers && onSyncWorkers(parseInt(e.target.value, 10))}
                style={{ minWidth: 80, fontFamily: 'var(--font-mono)', fontSize: 12 }}
              >
                {[1, 2, 3, 4, 5, 6, 8, 12, 16].map((n) => (
                  <option key={n} value={n}>{n}</option>
                ))}
              </select>
            </div>
          </div>
        </div>
      </div>

      <div className="settings-section">
        <h3>About</h3>
        <div className="settings-card">
          <div className="settings-row">
            <div className="srl">Version</div>
            <div className="srv" style={{gridColumn:'2 / 4'}}>zkv v{version}{platform ? ` (${platform})` : ''}</div>
          </div>
          <div className="settings-row">
            <div className="srl">Source</div>
            <div className="srv" style={{gridColumn:'2 / 4'}}>github.com/zecrocks/zkv</div>
          </div>
        </div>
        <div style={{marginTop:16, display:'flex', gap:10, flexWrap:'wrap'}}>
          <button className="btn secondary" onClick={onViewLicenses}>
            <Icon name="scale" className="icon"/> View Licenses
          </button>
          <button className="btn secondary" onClick={onViewShortcuts}>
            <Icon name="keyboard" className="icon"/> View Keyboard Shortcuts
          </button>
          <button className="btn secondary" onClick={onResetOnboarding}>
            <Icon name="rotate-ccw" className="icon"/> Replay onboarding
          </button>
          {status && status.demo_reimport_available && (
            <button className="btn secondary" onClick={onReimportDemo}>
              <Icon name="download-cloud" className="icon"/> Re-import Oracle Demo
            </button>
          )}
        </div>
      </div>

      <div className="settings-section">
        <h3>Danger Zone</h3>
        <p className="lede">Irreversible, local-only actions.</p>
        <div className="settings-card danger-zone">
          <div className="settings-row">
            <div className="srl">
              Show seed phrase
              <span className="sub">Reveal an admin database's 24-word recovery phrase. Anyone with it can write to the database and spend its funds.</span>
            </div>
            <div className="srv"></div>
            <div className="ctl">
              <select
                className="input"
                value={revealSel}
                onChange={(e) => setRevealPick(e.target.value)}
                disabled={adminDbNames.length === 0}
                style={{ minWidth: 180, fontFamily: 'var(--font-mono)', fontSize: 12 }}
              >
                {adminDbNames.length === 0
                  ? <option value="">No admin databases</option>
                  : adminDbNames.map((n) => <option key={n} value={n}>{n}</option>)}
              </select>
              <button className="btn danger" disabled={!revealSel} onClick={() => setRevealTarget(revealSel)}>
                <Icon name="eye" className="icon"/> Reveal
              </button>
            </div>
          </div>
          <div className="settings-row">
            <div className="srl">
              Forget database
              <span className="sub">Delete a database's local cache. The on-chain data stays readable by anyone holding its zkv1 address.</span>
            </div>
            <div className="srv"></div>
            <div className="ctl">
              <select
                className="input"
                value={forgetSel}
                onChange={(e) => setForgetPick(e.target.value)}
                disabled={dbNames.length === 0}
                style={{ minWidth: 180, fontFamily: 'var(--font-mono)', fontSize: 12 }}
              >
                {dbNames.length === 0
                  ? <option value="">No databases</option>
                  : dbNames.map((n) => <option key={n} value={n}>{n}</option>)}
              </select>
              <button className="btn danger" disabled={!forgetSel} onClick={() => setForgetTarget(forgetSel)}>
                <Icon name="trash-2" className="icon"/> Forget
              </button>
            </div>
          </div>
        </div>
      </div>

      {forgetTarget && (
        <ForgetModal
          name={forgetTarget}
          onClose={() => setForgetTarget(null)}
          onConfirm={onForget}
        />
      )}

      {revealTarget && (
        <RevealPhraseModal
          name={revealTarget}
          onClose={() => setRevealTarget(null)}
        />
      )}
    </div>
  );
};

// ============================================================
// LICENSES: third-party notices
// ============================================================
// The generated third-party license bundle (the verbatim license texts of
// every bundled dependency) is large, so we don't render it inline. Instead
// the user saves the whole bundle to a file, or dumps it from the CLI with
// `zkv --licenses`. On desktop (Tauri) "Save" pops the native save dialog and
// writes the file in Rust; in the browser it triggers a normal download. Both
// paths go through the same `window.zkvApi` surface (see api.ts).
const LICENSES_CLI_CMD = 'zkv --licenses';

const Licenses = ({ onBack }: { onBack: () => void }) => {
  const [status, setStatus] = React.useState<string | null>(null);
  const [error, setError] = React.useState<any>(null);
  const [saving, setSaving] = React.useState(false);
  const [copied, setCopied] = React.useState(false);

  const onSave = async () => {
    setError(null); setStatus(null); setSaving(true);
    try {
      const r = await window.zkvApi.saveLicenses();
      if (r && r.saved) setStatus(r.path ? `Saved to ${r.path}` : 'Saved.');
      // r.saved === false means the native picker was cancelled; stay quiet.
    } catch (e: any) {
      setError(e.message || 'failed to save licenses');
    } finally {
      setSaving(false);
    }
  };

  const onCopyCmd = () => {
    try { navigator.clipboard.writeText(LICENSES_CLI_CMD); } catch {}
    setCopied(true);
    setTimeout(() => setCopied(false), 1500);
  };

  return (
    <div className="settings">
      <div style={{marginBottom:24, display:'flex', alignItems:'flex-end', gap:16}}>
        <div style={{flex:'1 1 auto'}}>
          <div style={{fontFamily:'IBM Plex Mono', fontSize:11, letterSpacing:'0.12em', textTransform:'uppercase', color:'var(--fg-3)'}}>WORKSPACE</div>
          <h2 style={{fontFamily:'IBM Plex Sans', fontWeight:600, fontSize:26, letterSpacing:'-0.01em', margin:'4px 0 0', color:'var(--fg-1)'}}>Licenses</h2>
        </div>
        <button className="btn secondary sm" onClick={onBack}>
          <Icon name="arrow-left" className="icon"/> Back to Settings
        </button>
      </div>
      <p className="lede">
        Third-party software bundled with or linked into zkv and the zkv Browser,
        with the license texts their authors ship. Generated from the build's
        resolved dependency graph. Save the full bundle to a file, or dump it
        from the command line with <code style={{fontFamily:'var(--font-mono)', fontSize:12}}>{LICENSES_CLI_CMD}</code>.
      </p>
      <div className="settings-card" style={{padding:18, display:'flex', gap:12, alignItems:'center', flexWrap:'wrap'}}>
        <button className="btn" onClick={onSave} disabled={saving}>
          <Icon name="download" className="icon"/> {saving ? 'Saving…' : 'Save to file'}
        </button>
        <div style={{display:'flex', alignItems:'center', gap:8, marginLeft:'auto'}}>
          <code style={{fontFamily:'var(--font-mono)', fontSize:13, color:'var(--fg-2)'}}>{LICENSES_CLI_CMD}</code>
          <button className="btn secondary sm" onClick={onCopyCmd} title="Copy the CLI command">
            <Icon name="copy" className="icon"/> {copied ? 'Copied' : 'Copy'}
          </button>
        </div>
      </div>
      {status && (
        <div style={{marginTop:12, color:'var(--fg-3)', fontSize:13}}>{status}</div>
      )}
      {error && (
        <div style={{marginTop:12, color:'var(--fg-3)', fontSize:13}}>Couldn't save: {error}</div>
      )}
    </div>
  );
};

// ============================================================
// KEYBOARD SHORTCUTS: reference card
// ============================================================
// A read-only catalog of every keyboard shortcut the app actually implements
// (see the keydown handlers in app.tsx + the Find palette in this file). It is
// deliberately NOT in the sidebar: the only ways in are the Find palette and
// the Settings button, mirroring the Licenses screen. The command modifier is
// rendered per-platform via MOD_KEY (⌘ on macOS, Ctrl elsewhere), so the keys
// shown are the ones that actually fire on this machine.

// One key chip. `.kbd-inline` is the shared key-cap style (styles.css).
const Kbd = ({ children }: { children: React.ReactNode }) => (
  <kbd className="kbd-inline">{children}</kbd>
);

// A shortcut's keys: a chord (pressed together, rendered adjacent) or, with
// `alt`, a set of interchangeable keys (rendered separated by "/").
const KeyCombo = ({ keys, alt }: { keys: string[]; alt?: boolean }) => (
  <span className="shortcuts-keys">
    {keys.map((k, i) => (
      <React.Fragment key={i}>
        {i > 0 && alt && <span className="kbd-sep">/</span>}
        <Kbd>{k}</Kbd>
      </React.Fragment>
    ))}
  </span>
);

const KeyboardShortcuts = ({ onBack }: { onBack: () => void }) => {
  // Built at render time so MOD_KEY (set by chrome.js, loaded first) is resolved.
  const groups: {
    title: string;
    items: { keys: string[]; alt?: boolean; desc: string }[];
  }[] = [
    {
      title: 'Global',
      items: [
        { keys: [MOD_KEY, 'K'], desc: 'Open the Find palette' },
        { keys: ['j'], desc: 'Select the next database in the sidebar' },
        { keys: ['k'], desc: 'Select the previous database in the sidebar' },
      ],
    },
    {
      title: 'Find palette',
      items: [
        { keys: ['↑', '↓'], alt: true, desc: 'Move through the results' },
        { keys: ['Enter'], desc: 'Run the highlighted command' },
        { keys: ['Esc'], desc: 'Close the palette' },
      ],
    },
    {
      title: 'Open database',
      items: [
        { keys: ['↑', '↓'], alt: true, desc: 'Navigate rows in the table' },
        { keys: ['←', '→'], alt: true, desc: 'Switch tab (Browse, History, Roles…)' },
      ],
    },
  ];

  return (
    <div className="settings">
      <div style={{marginBottom:24, display:'flex', alignItems:'flex-end', gap:16}}>
        <div style={{flex:'1 1 auto'}}>
          <div style={{fontFamily:'IBM Plex Mono', fontSize:11, letterSpacing:'0.12em', textTransform:'uppercase', color:'var(--fg-3)'}}>WORKSPACE</div>
          <h2 style={{fontFamily:'IBM Plex Sans', fontWeight:600, fontSize:26, letterSpacing:'-0.01em', margin:'4px 0 0', color:'var(--fg-1)'}}>Keyboard Shortcuts</h2>
        </div>
        <button className="btn secondary sm" onClick={onBack}>
          <Icon name="arrow-left" className="icon"/> Back to Settings
        </button>
      </div>
      {groups.map(g => (
        <div className="settings-section" key={g.title}>
          <h3>{g.title}</h3>
          <div className="settings-card">
            {g.items.map((it, i) => (
              <div className="shortcuts-row" key={i}>
                <span className="desc">{it.desc}</span>
                <KeyCombo keys={it.keys} alt={it.alt} />
              </div>
            ))}
          </div>
        </div>
      ))}
    </div>
  );
};

// ============================================================
// ONBOARDING: first-run welcome
// ============================================================
// A pool of real BIP-39 words, used only to fabricate a fresh, throwaway
// example recovery phrase for the onboarding terminal demo (see below).
const DEMO_SEED_WORDS = [
  'wisdom', 'fabric', 'essence', 'bottom', 'luxury', 'tower', 'arrest', 'quick',
  'ozone', 'raccoon', 'defy', 'spy', 'orbit', 'canyon', 'velvet', 'harvest',
  'meadow', 'silent', 'copper', 'ginger', 'puzzle', 'nimble', 'cargo', 'pioneer',
  'gravity', 'lantern', 'marble', 'pelican', 'quartz', 'ribbon', 'sunset', 'timber',
  'umbrella', 'voyage', 'walnut', 'anchor', 'breeze', 'cactus', 'dolphin', 'ember',
  'falcon', 'glacier', 'hollow', 'island', 'jungle', 'kettle', 'ladder', 'mango',
  'nectar', 'pebble', 'rocket', 'saddle', 'thunder', 'velour', 'willow', 'zebra',
  'almond', 'bishop', 'cobalt', 'dynamo', 'flicker', 'gadget', 'hazel', 'ivory',
];

const Onboarding = ({ onChoose, onSkip, version }: {
  onChoose: (choice: string) => void;
  onSkip: () => void;
  version?: number | string | null;
}) => {
  const [terminalLine, setTerminalLine] = React.useState(0);
  // Fabricate a fresh random example phrase every time onboarding is shown, so
  // the seed printed in the demo is never a real key anyone might actually
  // write down. Only the first 12 of the 24 words are shown (then "…").
  const seed = React.useMemo(() => {
    const pick: string[] = [];
    for (let i = 0; i < 12; i++) {
      pick.push(DEMO_SEED_WORDS[Math.floor(Math.random() * DEMO_SEED_WORDS.length)]);
    }
    return pick;
  }, []);
  const lines = [
    { type: 'prompt', text: '$ ', cmd: 'zkv init' },
    { type: 'out', text: 'Creating mainnet zkv database "default"' },
    { type: 'out', text: '' },
    { type: 'dim', text: 'Recovery phrase, write these 24 words down NOW.' },
    { type: 'out', text: '' },
    { type: 'out', text: '  ' + seed.slice(0, 6).join(' ') },
    { type: 'out', text: '  ' + seed.slice(6, 12).join(' ') + ' …' },
    { type: 'out', text: '' },
    { type: 'ok',  text: '✓ Confirmed.' },
    { type: 'ok',  text: '✓ Created database "default" (mainnet, birthday 3338983)' },
    { type: 'out', text: '' },
    { type: 'prompt', text: '$ ', cmd: 'zkv set zec_usd_price 1008.33' },
    { type: 'out', text: 'zkv SET zec_usd_price → zkv1qz9p…k2voracle' },
    { type: 'ok',  text: '✓ broadcast tx 96feedf9…584166214' },
    { type: 'out', text: '' },
    { type: 'prompt', text: '$ ', cmd: 'zkv get zec_usd_price' },
    { type: 'out', text: 'zec_usd_price = 1008.33' },
  ];

  React.useEffect(() => {
    if (terminalLine >= lines.length) return;
    const cur = lines[terminalLine];
    const delay = cur.cmd ? 700 : (cur.text === '' ? 120 : 220);
    const t = setTimeout(() => setTerminalLine(l => l + 1), delay);
    return () => clearTimeout(t);
  }, [terminalLine]);

  return (
    <div className="onboard-overlay">
      <div className="onboard-bar">
        <div className="topbar-logo">
          <img src="design-system/assets/logo-mark-dark.svg" width="22" height="22" alt="zkv"/>
          <span className="wordmark"><span>z</span><span className="colon">:</span><span>kv</span></span>
        </div>
        <span className="topbar-divider"></span>
        <span style={{fontFamily:'var(--font-mono)', fontSize:12, color:'var(--fg-3)'}}>welcome</span>
        <div style={{marginLeft:'auto'}}>
          <button className="btn ghost sm" onClick={onSkip}>Skip, I'll figure it out <Icon name="x" className="icon"/></button>
        </div>
      </div>

      <div className="onboard-body">
        <div className="onboard-left">
          <h1 className="onboard-title">
            A key value database on Zcash.
          </h1>
          <p className="onboard-lede">
            z:kv stores key-value pairs as signed memos on Zcash. Anyone with your <code>zkv1</code> address can read, only the authorized seed-holders can write. Use it for feature flags, price oracles, or any data needing decentralization.
          </p>

          <div className="onboard-paths">
            <button className="onboard-path" onClick={() => onChoose('create')}>
              <div className="pic"><Icon name="plus" size={18}/></div>
              <div>
                <div className="pname">Create your first database</div>
              </div>
              <div className="parrow"><Icon name="arrow-right" size={16}/></div>
            </button>
            <button className="onboard-path" onClick={() => onChoose('demo')}>
              <div className="pic"><Icon name="eye" size={18}/></div>
              <div>
                <div className="pname">Watch a Zcash price oracle</div>
              </div>
              <div className="parrow"><Icon name="arrow-right" size={16}/></div>
            </button>
            <button className="onboard-path" onClick={() => onChoose('reference')}>
              <div className="pic"><Icon name="book-open" size={18}/></div>
              <div>
                <div className="pname">Explore the z:kv learning reference</div>
              </div>
              <div className="parrow"><Icon name="arrow-right" size={16}/></div>
            </button>
          </div>
        </div>

        <div className="onboard-right">
          <div className="terminal">
            <div className="terminal-bar">
              <span className="tl-dot"></span>
              <span className="tl-dot"></span>
              <span className="tl-dot"></span>
              <span className="tl-title">emersonian@local · ~/projects/zcash-oracle</span>
            </div>
            <div className="terminal-body">
              {lines.slice(0, terminalLine).map((l, i) => {
                if (l.cmd) {
                  return (
                    <div key={i}>
                      <span className="prompt">{l.text}</span>
                      <span className="cmd">{l.cmd}</span>
                    </div>
                  );
                }
                if (l.type === 'dim')    return <div key={i} className="dim">{l.text}</div>;
                if (l.type === 'ok')     return <div key={i} className="ok">{l.text}</div>;
                return <div key={i} className={l.type === 'out' ? 'out' : ''}>{l.text || '\u00a0'}</div>;
              })}
              {terminalLine < lines.length && <span className="cursor"></span>}
            </div>
          </div>
        </div>
      </div>

      <div className="onboard-foot">
        <span>{version ? `zkv v${version}` : 'zkv'} · alpha · for testing, not ready for production use</span>
        <span>github.com/zecrocks/zkv</span>
      </div>
    </div>
  );
};

// ============================================================
// COMMAND PALETTE
// ============================================================
const CommandPalette = ({ open, onClose, onGo, databases }: {
  open: boolean;
  onClose: () => void;
  onGo: (target: string) => void;
  databases: DbSummary[];
}) => {
  const [q, setQ] = React.useState('');
  const [idx, setIdx] = React.useState(0);

  // Always start fresh: clear any previously-typed text each time it opens.
  React.useEffect(() => { if (open) { setQ(''); setIdx(0); } }, [open]);

  const dbCommands = (databases || []).map(d => ({
    group: 'Databases',
    icon: d.role === 'admin' ? 'database' : 'eye',
    name: `${d.name} · ${d.role}`,
    shortcut: '',
    action: () => onGo('keys:' + d.name),
  }));

  // One entry per zkv opcode, jumping to its Reference page. Sourced from the
  // reference bundle's global so this stays in step with the opcode catalog.
  const opcodeCommands = (window.ZKV_OPCODES || []).map(o => ({
    group: 'Reference',
    icon: 'book-open',
    name: `${o.name} opcode`,
    shortcut: '',
    action: () => onGo('ref:' + o.id),
  }));

  const commands = [
    // Databases first so the user's own DBs sit above generic navigation.
    ...dbCommands,
    { group: 'Navigate', icon: 'layout-dashboard', name: 'Go to Dashboard', shortcut: '⌘D', action: () => onGo('dashboard') },
    // Discover temporarily disabled.
    { group: 'Navigate', icon: 'settings', name: 'Open Settings', shortcut: '⌘,', action: () => onGo('settings') },
    { group: 'Navigate', icon: 'keyboard', name: 'View keyboard shortcuts', shortcut: '', action: () => onGo('shortcuts') },
    { group: 'Actions', icon: 'plus', name: 'Create database', shortcut: '', action: () => onGo('create') },
    { group: 'Actions', icon: 'download', name: 'Import / watch database', shortcut: '', action: () => onGo('import') },
    { group: 'Actions', icon: 'send', name: 'Set key in current database…', shortcut: '', action: () => onGo('write') },
    { group: 'Actions', icon: 'refresh-cw', name: 'Force sync now', shortcut: '⌘R', action: () => onGo('sync') },
    { group: 'Actions', icon: 'sun', name: 'Toggle theme', shortcut: '⌘T', action: () => onGo('theme') },
    // Opcode reference jumps last, in their own group.
    ...opcodeCommands,
  ];

  const filtered = q
    ? commands.filter(c => c.name.toLowerCase().includes(q.toLowerCase()))
    : commands;

  React.useEffect(() => { setIdx(0); }, [q]);

  const groups = filtered.reduce<Record<string, typeof filtered>>((acc, c) => {
    (acc[c.group] = acc[c.group] || []).push(c);
    return acc;
  }, {});

  if (!open) return null;

  const onKey = (e: React.KeyboardEvent) => {
    if (e.key === 'Enter') { e.preventDefault(); if (filtered[idx]) { filtered[idx].action(); onClose(); }}
    if (e.key === 'ArrowDown') { e.preventDefault(); setIdx(i => Math.min(i+1, filtered.length-1)); }
    if (e.key === 'ArrowUp')   { e.preventDefault(); setIdx(i => Math.max(i-1, 0)); }
  };

  return (
    <div className="cmd-overlay" onClick={onClose}>
      <div className="cmd-panel" onClick={e => e.stopPropagation()}>
        <input className="cmd-input" autoFocus
               placeholder="Search commands, databases, keys…"
               value={q}
               onChange={e => setQ(e.target.value)}
               onKeyDown={onKey} />
        <div className="cmd-list">
          {Object.entries(groups).map(([g, items]) => (
            <React.Fragment key={g}>
              <div className="cmd-group-h">{g}</div>
              {(items as any[]).map((c, i) => {
                const absIdx = filtered.indexOf(c);
                return (
                  <div key={c.name}
                       className={'cmd-item' + (absIdx === idx ? ' active' : '')}
                       onMouseEnter={() => setIdx(absIdx)}
                       onClick={() => { c.action(); onClose(); }}>
                    <Icon name={c.icon} size={14} color="var(--fg-3)"/>
                    <span>{c.name}</span>
                    {/* Keyboard shortcuts removed; palette is the single entry point. */}
                  </div>
                );
              })}
            </React.Fragment>
          ))}
          {filtered.length === 0 && (
            <div className="cmd-item" style={{color:'var(--fg-3)'}}>
              <Icon name="search-x" size={14}/> No results for "{q}"
            </div>
          )}
        </div>
      </div>
    </div>
  );
};

window.Discover = Discover;
window.Settings = Settings;
window.Licenses = Licenses;
window.KeyboardShortcuts = KeyboardShortcuts;
window.Onboarding = Onboarding;
window.CommandPalette = CommandPalette;
