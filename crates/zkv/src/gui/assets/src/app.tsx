// app.jsx: main App component. Orchestrates data loading from the zkv
// gui JSON API (window.zkvApi) and wires every screen/action to real
// backend calls. No fixtures: databases, keys, balances, sync state and
// every write are live against the local zkv databases.

const api = window.zkvApi;
const MIN_INIT_ZATS = 10000; // funding floor shown in the create flow
// The bundled read-only demo database (see crate `demo` module). Excluded from
// the "no databases yet" onboarding gate so a first-time user still sees
// onboarding, and opened by the onboarding "see a demo" path.
const DEMO_DB_NAME = "demo-oracles";
const HISTORY_PAGE = 100; // rows per History page; bar appears past this
// Database tabs are derived per-database (role-aware) inside App as `keysTabs`;
// the Funding tab is admin-only. HISTORY_PAGE doubles as the Funding page size.

function App() {
  // ===== Theme =====
  const [theme, setTheme] = React.useState(() => {
    try {
      return localStorage.getItem("zkv-theme") || "light";
    } catch {
      return "light";
    }
  });

  // ===== Display time zone for timestamps; defaults to the local system =====
  const [timeZone, setTimeZone] = React.useState(() => {
    try {
      return localStorage.getItem("zkv-tz") || "local";
    } catch {
      return "local";
    }
  });

  // ===== Navigation / selection =====
  const [view, setView] = React.useState("dashboard");
  // Which Reference section to show (e.g. "op:set"); set by the command palette
  // to deep-link to an opcode page. Null falls back to the default landing.
  const [refSection, setRefSection] = React.useState<string | null>(null);
  const [activeName, setActiveName] = React.useState<string | null>(null);
  const [filter, setFilter] = React.useState("");
  const [selected, setSelected] = React.useState(0);
  const [keysTab, setKeysTab] = React.useState("browse");
  const [cmdOpen, setCmdOpen] = React.useState(false);
  // A signer pubkey to highlight on the Roles tab, set when the user clicks a
  // signer chip in History/Browse. Cleared on db switch and manual tab change.
  const [focusPubkey, setFocusPubkey] = React.useState<string | null>(null);

  // ===== Live data =====
  const [databases, setDatabases] = React.useState<DbSummary[]>([]);
  const [detail, setDetail] = React.useState<DbDetail | null>(null);
  // True from the moment you click a *different* db until its detail lands. We
  // keep the previous db's panel rendered during this gap (no blank flash);
  // `switchSlow` only flips true if the load drags past a short threshold, at
  // which point we show a spinner instead of stale content.
  const [switching, setSwitching] = React.useState(false);
  const [switchSlow, setSwitchSlow] = React.useState(false);
  const [status, setStatus] = React.useState<StatusResp | null>(null);
  // Both networks' lightwalletd identities (GetLightdInfo), probed once at
  // launch, and shown on the Settings screen's per-network server rows.
  const [servers, setServers] = React.useState<ServersResp | null>(null);
  const [syncing, setSyncing] = React.useState(false);
  const [manualSyncing, setManualSyncing] = React.useState(false);
  const [notice, setNotice] = React.useState<{ kind: string; text: string } | null>(null);
  const [booted, setBooted] = React.useState(false);

  // ===== Write history (History tab), paginated + server-side filtered =====
  const [history, setHistory] = React.useState<HistoryResp | null>(null);
  const [historyLoading, setHistoryLoading] = React.useState(false);
  const [selectedHistory, setSelectedHistory] = React.useState(0);
  const [historyOffset, setHistoryOffset] = React.useState(0);
  const [focusTxid, setFocusTxid] = React.useState<string | null>(null); // select + scroll to this tx after load
  const [locateTxid, setLocateTxid] = React.useState<string | null>(null); // jump to the page holding this tx

  // ===== Rejections (Rejections tab): writes replay dropped, with reasons =====
  const [rejections, setRejections] = React.useState<RejectionsResp | null>(null);
  const [rejectionsLoading, setRejectionsLoading] = React.useState(false);
  const [selectedRejection, setSelectedRejection] = React.useState(0);
  // ===== Roles (Roles tab): the on-chain owner/writer authorization registry =====
  const [roles, setRoles] = React.useState<RolesResp | null>(null);
  const [rolesLoading, setRolesLoading] = React.useState(false);
  // Selected row in the Roles tab (index into the combined active+revoked list),
  // driving the RoleDetail pane. Mirrors the other tabs' selection state.
  const [selectedRole, setSelectedRole] = React.useState(0);
  // ===== Funding tab (admin-only): non-zkv ZEC transfers, paginated =====
  const [funding, setFunding] = React.useState<FundingResp | null>(null);
  const [fundingLoading, setFundingLoading] = React.useState(false);
  const [selectedFunding, setSelectedFunding] = React.useState(0);
  const [fundingOffset, setFundingOffset] = React.useState(0);

  // ===== Write modal =====
  const [writeOpen, setWriteOpen] = React.useState(false);
  const [writeMode, setWriteMode] = React.useState("set");
  const [writePrefill, setWritePrefill] = React.useState<any>(null);

  // ===== Deposit (funding QR) modal =====
  const [depositOpen, setDepositOpen] = React.useState(false);

  // ===== Send (ZEC value transfer) modal =====
  const [sendOpen, setSendOpen] = React.useState(false);

  // ===== Create / Import modals =====
  const [createOpen, setCreateOpen] = React.useState(false);
  const [importOpen, setImportOpen] = React.useState(false);

  // ===== Onboarding =====
  const [showOnboarding, setShowOnboarding] = React.useState(false);

  const flash: any = React.useCallback((text: string, kind = "info") => {
    setNotice({ text, kind });
    window.clearTimeout(flash._t);
    flash._t = window.setTimeout(() => setNotice(null), 4200);
  }, []);

  // ---- data loaders ----
  const refreshDatabases = React.useCallback(async () => {
    try {
      const dbs = await api.listDatabases();
      setDatabases(dbs);
      return dbs;
    } catch (e) {
      flash("Couldn't list databases: " + (e as Error).message, "error");
      return [];
    }
  }, [flash]);

  const refreshStatus = React.useCallback(async () => {
    try {
      const s = await api.status();
      setStatus(s);
      return s;
    } catch (_) {
      /* transient; keep last known */
      return null;
    }
  }, []);

  // Monotonic token so only the most recent detail load can write state.
  // Clicking between databases quickly fires overlapping api.detail() calls
  // that resolve out of order; without this, a slow earlier response could
  // clobber the freshly-selected db's detail (and, in the desktop webview,
  // drive an extra burst of re-renders into AppKit's display cycle).
  const loadSeqRef = React.useRef(0);
  const loadDetail = React.useCallback(
    async (name: string) => {
      const seq = ++loadSeqRef.current;
      try {
        const d = await api.detail(name);
        if (seq !== loadSeqRef.current) return d; // superseded; drop the result
        setDetail(d);
        return d;
      } catch (e) {
        if (seq !== loadSeqRef.current) return null; // superseded; stay quiet
        flash("Couldn't open " + name + ": " + (e as Error).message, "error");
        setDetail(null);
        return null;
      }
    },
    [flash]
  );

  const loadHistory = React.useCallback(
    async (name: string, filter: string, offset?: number, locate?: string | null) => {
      setHistoryLoading(true);
      try {
        // `locate` jumps to the page holding a specific txid in full context;
        // the server resolves the page and ignores filter/offset.
        const h = await api.history(name, {
          filter: locate ? undefined : filter || undefined,
          limit: HISTORY_PAGE,
          offset: locate ? undefined : offset || 0,
          locate: locate || undefined,
        });
        setHistory(h);
        return h;
      } catch (e) {
        flash("Couldn't load history: " + (e as Error).message, "error");
        setHistory({ creator: "", entries: [], total: 0, offset: 0, limit: HISTORY_PAGE });
        return null;
      } finally {
        setHistoryLoading(false);
      }
    },
    [flash]
  );

  const loadRejections = React.useCallback(
    async (name: string) => {
      setRejectionsLoading(true);
      try {
        const r = await api.rejections(name);
        setRejections(r);
        return r;
      } catch (e) {
        flash("Couldn't load rejections: " + (e as Error).message, "error");
        setRejections({ entries: [], total: 0 });
        return null;
      } finally {
        setRejectionsLoading(false);
      }
    },
    [flash]
  );

  const loadRoles = React.useCallback(
    async (name: string) => {
      setRolesLoading(true);
      try {
        const r = await api.roles(name);
        setRoles(r);
        return r;
      } catch (e) {
        flash("Couldn't load roles: " + (e as Error).message, "error");
        setRoles({ creator: null, rows: [], revoked: [] });
        return null;
      } finally {
        setRolesLoading(false);
      }
    },
    [flash]
  );

  const loadFunding = React.useCallback(
    async (name: string, offset: number) => {
      setFundingLoading(true);
      try {
        const f = await api.funding(name, { limit: HISTORY_PAGE, offset: offset || 0 });
        setFunding(f);
        return f;
      } catch (e) {
        flash("Couldn't load funding: " + (e as Error).message, "error");
        setFunding({ entries: [], total: 0, offset: 0, limit: HISTORY_PAGE });
        return null;
      } finally {
        setFundingLoading(false);
      }
    },
    [flash]
  );

  // ---- boot ----
  React.useEffect(() => {
    (async () => {
      const dbs = await refreshDatabases();
      const st = await refreshStatus();
      // Onboarding state lives in the data dir (`.zkv`), not the browser. An
      // earlier version persisted a "dismissed" flag in localStorage, but that
      // is per-origin (every zkv install shares http://127.0.0.1:<port>), so a
      // once-dismissed flag suppressed onboarding across unrelated installs and
      // data-dir resets, leaving a genuinely-new user stuck without it. The
      // server reports `onboarded` off a marker file in `.zkv`; show the
      // welcome overlay only when it is false and there is no real database
      // (the bundled demo doesn't count). Dismissing/completing persists the
      // marker via `markOnboarded`, so a fresh `.zkv` shows onboarding again.
      const userDbs = dbs.filter((d) => d.name !== DEMO_DB_NAME);
      if (userDbs.length === 0 && !(st && st.onboarded)) setShowOnboarding(true);
      setBooted(true);
    })();
    // Probe both networks' lightwalletd early (GetLightdInfo) for the Settings
    // screen's per-network server rows. Fire-and-forget: the probe is slow and
    // must not gate boot.
    api.servers().then(setServers).catch(() => {});
    // eslint-disable-next-line
  }, []);

  // Spinner safety net for slow switches. We keep the previous db's panel up
  // while the new detail loads (no blank flash), but if that load drags past
  // this threshold we'd be showing clearly-stale content for too long, so flip
  // `switchSlow` to swap in a spinner. Cleared the moment the switch resolves.
  const SWITCH_SPINNER_MS = 400;
  React.useEffect(() => {
    if (!switching) {
      setSwitchSlow(false);
      return;
    }
    const id = setTimeout(() => setSwitchSlow(true), SWITCH_SPINNER_MS);
    return () => clearTimeout(id);
  }, [switching]);

  // Persist + apply theme.
  React.useEffect(() => {
    try {
      localStorage.setItem("zkv-theme", theme);
    } catch {}
    document.documentElement.setAttribute("data-theme", theme);
  }, [theme]);

  // Persist the chosen display time zone.
  React.useEffect(() => {
    try {
      localStorage.setItem("zkv-tz", timeZone);
    } catch {}
  }, [timeZone]);

  // Reflect the current screen in the document title: "z:kv Browser" at the
  // root, "<database> | z:kv Browser" with a db open, "<Section> | z:kv Browser"
  // elsewhere. (The native desktop window title is set in desktop.rs.)
  React.useEffect(() => {
    const labels: Record<string, string> = {
      dashboard: "Dashboard",
      discover: "Discover",
      settings: "Settings",
      create: "New database",
      import: "Import",
    };
    let title = "z:kv Browser";
    if (view === "keys" && activeName) title = `${activeName} | z:kv Browser`;
    else if (labels[view]) title = `${labels[view]} | z:kv Browser`;
    document.title = title;
  }, [view, activeName]);

  // Right after launch the server provisions the bundled demo database; its
  // config lands within a moment. Refresh the sidebar list a few times early
  // so the demo shows up promptly instead of waiting for the 10s poll below.
  React.useEffect(() => {
    const ids = [800, 2000, 4000, 7000].map((ms) =>
      setTimeout(() => refreshDatabases(), ms)
    );
    return () => ids.forEach(clearTimeout);
  }, [refreshDatabases]);

  // Poll ambient status periodically. The server auto-syncs every database in
  // the background, so we also reload the open db's detail + the sidebar list
  // here to surface that fresh state without any manual action.
  React.useEffect(() => {
    const tick = () => {
      refreshStatus();
      if (activeName) loadDetail(activeName!);
      refreshDatabases();
    };
    const id = setInterval(tick, 10000);
    return () => clearInterval(id);
  }, [refreshStatus, loadDetail, refreshDatabases, activeName]);

  // Only keyboard shortcut: ⌘K / Ctrl-K toggles the search palette (Esc closes
  // it). Arrow-key list navigation lives in its own effect below. No others.
  React.useEffect(() => {
    const onKey = (e: KeyboardEvent) => {
      const mod = e.metaKey || e.ctrlKey;
      if (mod && e.key.toLowerCase() === "k") {
        e.preventDefault();
        setCmdOpen((v) => !v);
      }
      if (e.key === "Escape") setCmdOpen(false);
    };
    window.addEventListener("keydown", onKey);
    return () => window.removeEventListener("keydown", onKey);
  }, []);

  // Load the open History page from the server (filtered + paginated).
  // Debounced so typing in the filter box doesn't hammer the backend; re-runs
  // on tab open, db change, sync/broadcast (detail), filter, and page change.
  // When a txid is pending location, fetch its page (unfiltered, full context)
  // and snap the offset to the page the server resolved (no debounce).
  React.useEffect(() => {
    if (view === "keys" && keysTab === "history" && activeName) {
      if (locateTxid) {
        loadHistory(activeName, "", 0, locateTxid).then((h) => {
          if (h) setHistoryOffset(h.offset || 0);
          setLocateTxid(null);
        });
        return;
      }
      const id = setTimeout(() => loadHistory(activeName, filter, historyOffset), 200);
      return () => clearTimeout(id);
    }
    // eslint-disable-next-line
  }, [view, keysTab, activeName, detail, filter, historyOffset, locateTxid]);

  // Load rejections when that tab is open; re-runs on db change and after a
  // sync/broadcast (detail). It's a full re-scan, so only fetch on demand.
  React.useEffect(() => {
    if (view === "keys" && keysTab === "rejections" && activeName) {
      loadRejections(activeName);
    }
    // eslint-disable-next-line
  }, [view, keysTab, activeName, detail]);

  // Load the owner/writer registry whenever a database is open (any tab), not
  // just the Roles tab: History and Browse use it to resolve each signer's role
  // for the clickable signer chips. Small + unpaginated, so it just reloads on
  // db change and fresh detail (a management broadcast refreshes detail, which
  // re-pulls the roles).
  React.useEffect(() => {
    if (view === "keys" && activeName) {
      loadRoles(activeName);
    }
    // eslint-disable-next-line
  }, [view, activeName, detail]);

  // Load the Funding page when that tab is active. Re-runs on tab open, db
  // change, and background sync (detail), and page change. Debounced like
  // History so background refreshes don't hammer the backend.
  React.useEffect(() => {
    if (view === "keys" && keysTab === "funding" && activeName) {
      const id = setTimeout(() => loadFunding(activeName, fundingOffset), 200);
      return () => clearTimeout(id);
    }
    // eslint-disable-next-line
  }, [view, keysTab, activeName, detail, fundingOffset]);

  // ---- actions ----
  // Tracks the db we're currently showing/opening. Guards against re-entrant
  // opens (mashing the sidebar, or re-clicking the open row), which would
  // otherwise tear down and rebuild the whole keys view (10+ setState calls)
  // and re-fire loadDetail for no reason. Clicking between databases quickly is
  // what crashes the desktop webview (an AppKit display-cycle exception), so we
  // keep that burst as small as possible. Synchronous (a ref, not activeName
  // state) so a double-click within one tick is deduped before React flushes.
  const openNameRef = React.useRef<string | null>(null);
  const openDb = React.useCallback(
    async (name: string) => {
      // Already on this db's keys view: a no-op re-click. Skip the teardown +
      // reload entirely (the background poll keeps its detail fresh).
      const reopen = openNameRef.current === name;
      openNameRef.current = name;
      setView("keys"); // always; they may be returning from another section
      if (reopen) return;
      setActiveName(name);
      setSelected(0);
      setFilter("");
      setKeysTab("browse");
      // Keep the previous db's `detail` rendered during the load; clearing it
      // is what made the content area blank-flash on every switch. Instead mark
      // a switch in progress; the keys view keeps showing the old rows until the
      // new detail lands (or, if the load is slow, a spinner takes over). The
      // tab-scoped lists DO reset now, since they're cheap and reload anyway.
      setHistory(null);
      setSelectedHistory(0);
      setHistoryOffset(0);
      setRejections(null);
      setSelectedRejection(0);
      setFocusTxid(null);
      setRoles(null);
      setFocusPubkey(null);
      setSelectedRole(0);
      setFunding(null);
      setSelectedFunding(0);
      setFundingOffset(0);
      setSwitching(true);
      setSwitchSlow(false);
      // Note: selecting a db here deliberately does NOT write the CLI's
      // `current` marker. The GUI tracks its own `activeName`; the `current`
      // marker is a CLI concern, so GUI browsing never perturbs which db the
      // CLI considers current.
      await loadDetail(name);
      setSwitching(false);
    },
    [loadDetail]
  );

  // Filter-box changes also reset the History page to the first one.
  const onFilterChange = React.useCallback((v: string) => {
    setFilter(v);
    setHistoryOffset(0);
  }, []);

  // Jump from a key's detail to that key's history: pre-fill the filter with
  // the key and switch to the History tab (the server does the filtering).
  const viewHistory = React.useCallback((key: string) => {
    setFilter(key || "");
    setSelectedHistory(0);
    setHistoryOffset(0);
    setFocusTxid(null);
    setKeysTab("history");
  }, []);
  // Jump to a specific write in *full context*: clear the filter, locate the
  // page that holds this txid in the whole history (the load effect resolves
  // the offset), then highlight + scroll to it once that page loads.
  const openTxid = React.useCallback((key: string, txid: string | null) => {
    setFilter("");
    setSelectedHistory(0);
    setHistoryOffset(0);
    setFocusTxid(txid || null);
    setLocateTxid(txid || null);
    setKeysTab("history");
  }, []);
  // Jump to the Roles tab with a signer pubkey highlighted. Sets the tab
  // directly (not via changeTab, which clears the focus) so the highlight
  // survives the switch. Used by the signer chips in History/Browse.
  const openRole = React.useCallback((pubkey: string) => {
    setKeysTab("roles");
    setFocusPubkey(pubkey || null);
  }, []);

  const doSync = React.useCallback(
    async (name?: string, mempool?: boolean) => {
      const target = name || activeName;
      if (!target) {
        await refreshStatus();
        return;
      }
      setSyncing(true);
      try {
        await api.sync(target, mempool);
        await loadDetail(target);
        await refreshStatus();
      } catch (e) {
        flash("Sync failed: " + (e as Error).message, "error");
      } finally {
        setSyncing(false);
      }
    },
    [activeName, loadDetail, refreshStatus, flash]
  );

  // One-shot sync of the active db, used by the "Manual Sync" button that
  // appears while this db's continuous syncing is paused. A ref guards against
  // overlapping runs: rapid clicks would otherwise queue several /sync calls
  // that the server runs serially (per-db lock), leaving the spinner stuck.
  const manualSyncingRef = React.useRef(false);
  const doManualSync = React.useCallback(async () => {
    if (!activeName || manualSyncingRef.current) return;
    manualSyncingRef.current = true;
    setManualSyncing(true);
    try {
      await api.sync(activeName, true);
      await loadDetail(activeName!);
      await refreshStatus();
    } catch (e) {
      flash("Sync failed: " + (e as Error).message, "error");
    } finally {
      manualSyncingRef.current = false;
      setManualSyncing(false);
    }
  }, [activeName, loadDetail, refreshStatus, flash]);

  // Pause/resume continuous auto-sync for the active db (per-database).
  const doTogglePause = React.useCallback(async () => {
    if (!activeName) return;
    const next = !(detail && detail.paused);
    try {
      await api.setPause(activeName, next);
      await loadDetail(activeName!);
      refreshDatabases();
    } catch (e) {
      flash("Couldn't change sync: " + (e as Error).message, "error");
    }
  }, [activeName, detail, loadDetail, refreshDatabases, flash]);

  // Global "pause all syncing" toggle, surfaced in the bottom status bar.
  const doTogglePauseAll = React.useCallback(async () => {
    const next = !(status && status.paused_all);
    try {
      await api.pauseAll(next);
      await refreshStatus();
    } catch (e) {
      flash("Couldn't change sync: " + (e as Error).message, "error");
    }
  }, [status, refreshStatus, flash]);

  // Set the background concurrent-sync worker count (Settings).
  const onSyncWorkers = React.useCallback(
    async (n: number) => {
      try {
        await api.setSettings(n);
        await refreshStatus();
      } catch (e) {
        flash("Couldn't update workers: " + (e as Error).message, "error");
      }
    },
    [refreshStatus, flash]
  );

  // Forget (delete the local cache of) a database, from the Settings Danger
  // Zone. Wipes only local state; the on-chain writes stay readable by anyone
  // holding the zkv1 address. If the forgotten db is the one currently open,
  // tear down the keys view so the background poll doesn't chase a deleted db.
  // Throws on failure so the modal can surface it (and stay open).
  const onForget = React.useCallback(
    async (name: string) => {
      await api.forget(name);
      if (openNameRef.current === name) {
        openNameRef.current = null;
        setActiveName(null);
        setDetail(null);
      }
      await refreshDatabases();
      await refreshStatus();
      flash("Forgot " + name);
    },
    [refreshDatabases, refreshStatus, flash]
  );

  // Re-import the bundled demo database after it was deleted, from the
  // Settings About section. Refreshes the sidebar + status so the button
  // disappears and the new db shows up. Throws on failure so the caller can
  // surface it.
  const onReimportDemo = React.useCallback(async () => {
    const r = await api.reimportDemo();
    await refreshDatabases();
    await refreshStatus();
    flash("Re-imported " + r.name);
  }, [refreshDatabases, refreshStatus, flash]);

  const openWrite = (row: KeyRow | null) => {
    setWritePrefill(row);
    setWriteMode("set");
    setWriteOpen(true);
  };
  const openDelete = (row: KeyRow) => {
    setWritePrefill(row);
    setWriteMode("del");
    setWriteOpen(true);
  };

  // Real broadcast used by the write modal. Returns the txid.
  const doBroadcast = React.useCallback(
    async (mode: string, key: string, value: string) => {
      const txid =
        mode === "del"
          ? (await api.del(activeName!, key)).txid
          : (await api.set(activeName!, key, value)).txid;
      // Refresh state so the new pending op shows immediately.
      loadDetail(activeName!);
      refreshDatabases();
      return txid;
    },
    [activeName, loadDetail, refreshDatabases]
  );

  // Re-broadcast INIT from the write modal when the active db is
  // uninitialized. `requireSync` makes the server refuse unless the wallet is
  // fully synced to tip (so we don't double-INIT a db whose valid INIT is in
  // not-yet-scanned blocks). Returns the txid.
  const doInitWrite = React.useCallback(async () => {
    const txid = (await api.init(activeName!, { requireSync: true })).txid;
    loadDetail(activeName!);
    refreshDatabases();
    return txid;
  }, [activeName, loadDetail, refreshDatabases]);

  const handleCopy = (text: string) => {
    try {
      navigator.clipboard.writeText(text);
      flash("Copied to clipboard");
    } catch {}
  };

  // Onboarding
  const finishOnboard = (path: string) => {
    // Persist the dismissal in the data dir (not the browser) so onboarding
    // does not reappear on the next launch. Best-effort: a failure just means
    // it may show again. See the boot effect for why this is server-side.
    api.markOnboarded().catch(() => {});
    setShowOnboarding(false);
    if (path === "reference") {
      setView("reference");
      setRefSection("quickstart");
      return;
    }
    if (path === "demo") {
      // Open the bundled demo database so the user lands right on live data.
      openDb(DEMO_DB_NAME);
      return;
    }
    setView("dashboard");
    if (path === "create") setCreateOpen(true);
    else if (path === "watch") setImportOpen(true);
  };
  const resetOnboarding = () => setShowOnboarding(true);

  // Create flow callbacks (real)
  const onCreate = async (name: string, network: string, pool: string, phrase: string) =>
    api.create(name, network, pool, phrase);
  const onGeneratePhrase = async () => (await api.generatePhrase()).phrase;
  const onInit = async (name: string) => api.init(name);
  // Sync first so the funding step actually observes incoming ZEC.
  const pollDb = async (name: string) => {
    try {
      await api.sync(name);
    } catch (_) {}
    return api.detail(name);
  };

  // Import flow callbacks (real)
  const onWatch = async (addr: string, nickname: string) => {
    const r = await api.watch(addr, nickname || null);
    await refreshDatabases();
    flash("Watching " + r.name);
    openDb(r.name);
    return r;
  };
  const onRestore = async (name: string, phrase: string, network: string, birthday?: number) => {
    const r = await api.restore(name, phrase, network, birthday);
    await refreshDatabases();
    flash("Restored " + r.name);
    openDb(r.name);
    return r;
  };

  const handleCmdGo = (target: string) => {
    if (target === "dashboard" || target === "discover" || target === "settings" || target === "shortcuts") {
      setView(target);
    } else if (target === "create") setCreateOpen(true);
    else if (target === "import") setImportOpen(true);
    else if (target === "sync") doSync();
    else if (target === "theme") setTheme((t) => (t === "light" ? "dark" : "light"));
    else if (target === "write") {
      if (activeName) {
        setWritePrefill(null);
        setWriteMode("set");
        setWriteOpen(true);
      } else {
        flash("Open an admin database first to write a key", "error");
      }
    } else if (target.startsWith("keys:")) {
      openDb(target.slice(5));
    } else if (target.startsWith("ref:")) {
      // Jump to a specific opcode's Reference page.
      setRefSection("op:" + target.slice(4));
      setView("reference");
    }
  };

  // ---- derived ----
  // Does the loaded `detail` actually describe the selected db? During a switch
  // we deliberately keep the *previous* db's detail on screen (keep-previous, no
  // blank flash) while `activeName` has already advanced, so `detail` is stale
  // for a beat. Content (rows/key detail) may show through that beat, but any
  // per-db *truth* (the status-bar height) must not trust a mismatched detail.
  const detailMatches = !!detail && detail.name === activeName;
  const activeDb = detail
    ? {
        name: detail.name,
        role: detail.role,
        network: detail.network,
        pool: detail.pool,
        // Per-db synced height + server, so the status bar reads them straight
        // from the open db's detail (flips instantly on switch) instead of
        // waiting on the ambient /api/status poll, which lags a db change and
        // is network-gated; that lag is what made a synced db read "syncing".
        synced: detail.synced,
        server: detail.server,
        address: detail.address,
        funding_address: detail.funding_address,
        // The db's root signer travels with the detail so the Browse "Updated
        // by" chip lands in the same commit as the key rows (never a beat after,
        // the way the separate roles fetch could).
        signer: detail.signer,
        balance: detail.balance,
        confirming: detail.confirming,
        keys: detail.keys.length,
        init: detail.init,
        init_done: detail.init_done,
        init_required: detail.init_required,
      }
    : null;

  // Database tabs in display order; Left/Right cycles. Funding is admin-only,
  // so it only appears (and is reachable) for admin databases.
  const keysTabs = React.useMemo(
    () =>
      activeDb && activeDb.role === "admin"
        ? ["browse", "history", "roles", "funding", "rejections"]
        : ["browse", "history", "roles", "rejections"],
    [activeDb && activeDb.role]
  );

  // Keep showing the previous db's keys during a fast switch (no blank flash),
  // but once a switch is officially "slow" drop them so the Loading state takes
  // over rather than leaving clearly-stale rows on screen.
  const rows = switchSlow
    ? []
    : (detail?.keys || []).filter(
        (r) => !filter || r.key.toLowerCase().includes(filter.toLowerCase())
      );

  // The History page is whatever the server returned (already filtered,
  // ordered newest-first with in-flight pinned). No client-side filtering.
  const historyEntries = history?.entries || [];
  const fundingEntries = funding?.entries || [];
  const rejectionEntries = rejections?.entries || [];

  // The roles registry as returned by the server (owners first, then writers).
  const rolesRows = roles?.rows || [];
  // Revoked tombstones (owners/writers removed by an owner), newest-first.
  const rolesRevoked = roles?.revoked || [];
  // Combined list in table render order (active rows, then revoked), tagged so
  // the RoleDetail pane can tell them apart. `selectedRole` indexes into this.
  const rolesAll = React.useMemo(
    () => [
      ...rolesRows.map((r) => ({ ...r, revoked: false })),
      ...rolesRevoked.map((r) => ({ ...r, revoked: true })),
    ],
    [rolesRows, rolesRevoked]
  );

  // Keep each list's selection in range as the list shrinks (e.g. filtering down
  // to a single row, or a background refresh dropping rows): a stale index would
  // otherwise leave the row unhighlighted with nothing in the detail pane. Clamp
  // to the last valid row so a one-row table always shows its row selected.
  React.useEffect(() => {
    const clamp = (val: number, len: number, set: (n: number) => void) => {
      if (val > 0 && val >= len) set(Math.max(0, len - 1));
    };
    clamp(selected, rows.length, setSelected);
    clamp(selectedHistory, historyEntries.length, setSelectedHistory);
    clamp(selectedFunding, fundingEntries.length, setSelectedFunding);
    clamp(selectedRejection, rejectionEntries.length, setSelectedRejection);
    clamp(selectedRole, rolesAll.length, setSelectedRole);
  }, [
    rows.length,
    historyEntries.length,
    fundingEntries.length,
    rejectionEntries.length,
    rolesAll.length,
    selected,
    selectedHistory,
    selectedFunding,
    selectedRejection,
    selectedRole,
  ]);

  // After opening a TXID from a key's detail, select the matching entry once
  // the page loads; the "scroll selected into view" effect brings it on-screen.
  React.useEffect(() => {
    if (!focusTxid) return;
    const idx = historyEntries.findIndex((e) => e.txid === focusTxid);
    if (idx >= 0) {
      setSelectedHistory(idx);
      setFocusTxid(null);
    }
    // eslint-disable-next-line
  }, [history, focusTxid]);

  // Arriving on the Roles tab from a signer chip (openRole sets focusPubkey):
  // select that key's row so the RoleDetail pane opens on it. Re-runs when the
  // registry loads after the jump, so it lands even if roles weren't ready yet.
  React.useEffect(() => {
    if (!focusPubkey) return;
    const idx = rolesAll.findIndex((r) => r.pubkey === focusPubkey);
    if (idx >= 0) {
      setSelectedRole(idx);
      // Consume it once matched (like focusTxid) so a later background roles
      // reload doesn't yank the selection back from a manual click.
      setFocusPubkey(null);
    }
    // eslint-disable-next-line
  }, [focusPubkey, roles]);

  // Timestamp of the last arrow-key row move, for throttling held keys only.
  const lastRowMove = React.useRef(0);

  // Up/Down arrow navigation for the Browse and History row lists. Active
  // only on the keys view, and ignored while focus sits in a form field so
  // it never fights the filter input. Selection is clamped to the list.
  React.useEffect(() => {
    if (view !== "keys") return;
    const onArrow = (e: KeyboardEvent) => {
      if (e.key !== "ArrowDown" && e.key !== "ArrowUp") return;
      if (e.metaKey || e.ctrlKey || e.altKey) return;
      const t = e.target as HTMLElement;
      const tag = t && t.tagName;
      // Up/Down also drive the list straight from the filter box (type to
      // narrow, press Down to step into the results) but stay inert in any
      // other field (e.g. the write modal) so they never hijack a real caret.
      const inFilter = t && t.closest && t.closest(".kv-filter");
      if (!inFilter && (tag === "INPUT" || tag === "TEXTAREA" || tag === "SELECT" || (t && t.isContentEditable))) return;
      const len =
        keysTab === "history"
          ? historyEntries.length
          : keysTab === "funding"
          ? fundingEntries.length
          : keysTab === "rejections"
          ? rejectionEntries.length
          : keysTab === "roles"
          ? rolesAll.length
          : rows.length;
      if (len === 0) return;
      e.preventDefault();
      // Honor every fresh press immediately; throttle only auto-repeat from a
      // held key (e.repeat) so a held arrow scrolls smoothly instead of
      // machine-gunning through the list.
      const now = Date.now();
      if (e.repeat && now - lastRowMove.current < 70) return;
      lastRowMove.current = now;
      const step = e.key === "ArrowDown" ? 1 : -1;
      const move = (i: number) => Math.max(0, Math.min(len - 1, i + step));
      if (keysTab === "history") setSelectedHistory(move);
      else if (keysTab === "funding") setSelectedFunding(move);
      else if (keysTab === "rejections") setSelectedRejection(move);
      else if (keysTab === "roles") setSelectedRole(move);
      else setSelected(move);
    };
    window.addEventListener("keydown", onArrow);
    return () => window.removeEventListener("keydown", onArrow);
  }, [view, keysTab, rows.length, historyEntries.length, fundingEntries.length, rejectionEntries.length, rolesAll.length]);

  // Switch database tabs, resetting the History page when it becomes active.
  // Shared by the tab buttons and the Left/Right keyboard shortcut below.
  const changeTab = React.useCallback((t: string) => {
    // A deliberate tab switch drops any Roles highlight carried over from a
    // signer chip click (openRole sets the tab directly, bypassing this).
    setFocusPubkey(null);
    if (t === "history") setHistoryOffset(0);
    if (t === "rejections") setSelectedRejection(0);
    if (t === "roles") setSelectedRole(0);
    if (t === "funding") {
      setFundingOffset(0);
      setSelectedFunding(0);
    }
    setKeysTab(t);
  }, []);

  // Timestamp of the last arrow-key tab switch, for throttling held keys.
  const lastTabSwitch = React.useRef(0);

  // Left/Right arrow keys cycle between the database tabs (Browse <-> History).
  // Same guards as the row navigation above: keys view only, no modifiers, and
  // inert while a form field holds focus so it never fights the filter input.
  React.useEffect(() => {
    if (view !== "keys") return;
    const onArrow = (e: KeyboardEvent) => {
      if (e.key !== "ArrowLeft" && e.key !== "ArrowRight") return;
      if (e.metaKey || e.ctrlKey || e.altKey) return;
      const t = e.target as HTMLElement;
      const tag = t && t.tagName;
      if (tag === "INPUT" || tag === "TEXTAREA" || tag === "SELECT" || (t && t.isContentEditable)) return;
      e.preventDefault();
      // Honor every fresh press; throttle only auto-repeat (a held key) so
      // holding the key doesn't blow through the tabs, while distinct taps
      // (even rapid ones) always register.
      const now = Date.now();
      if (e.repeat && now - lastTabSwitch.current < 300) return;
      lastTabSwitch.current = now;
      const step = e.key === "ArrowRight" ? 1 : -1;
      const i = keysTabs.indexOf(keysTab);
      changeTab(keysTabs[(i + step + keysTabs.length) % keysTabs.length]);
    };
    window.addEventListener("keydown", onArrow);
    return () => window.removeEventListener("keydown", onArrow);
  }, [view, keysTab, changeTab, keysTabs]);

  // Sidebar database list. Nav order is admin-first then watch (matching the
  // grouped render). Past 10 databases the sidebar instead shows the 10 most
  // recently updated + a "View all" entry that opens the search palette.
  const DB_SIDEBAR_LIMIT = 10;
  const orderedDbs = React.useMemo(
    () => [
      ...databases.filter((d) => d.role === "admin"),
      ...databases.filter((d) => d.role === "watch"),
    ],
    [databases]
  );
  const dbTruncated = databases.length > DB_SIDEBAR_LIMIT;
  const sidebarDbs = React.useMemo(() => {
    if (!dbTruncated) return orderedDbs;
    return [...databases]
      .sort((a, b) => (b.updated_at || 0) - (a.updated_at || 0))
      .slice(0, DB_SIDEBAR_LIMIT);
  }, [databases, dbTruncated, orderedDbs]);

  // j / k move down / up the sidebar database list (vim-style), switching the
  // active database. Inert while typing or when any overlay is open, and it
  // never leaves the database list.
  React.useEffect(() => {
    const onJK = (e: KeyboardEvent) => {
      if (e.metaKey || e.ctrlKey || e.altKey) return;
      const k = e.key.toLowerCase();
      if (k !== "j" && k !== "k") return;
      const t = e.target as HTMLElement;
      const tag = t && t.tagName;
      if (tag === "INPUT" || tag === "TEXTAREA" || tag === "SELECT" || (t && t.isContentEditable)) return;
      if (cmdOpen || writeOpen || createOpen || importOpen || depositOpen || sendOpen || showOnboarding) return;
      if (sidebarDbs.length === 0) return;
      e.preventDefault();
      const cur = view === "keys" ? sidebarDbs.findIndex((d) => d.name === activeName) : -1;
      const step = k === "j" ? 1 : -1;
      const next = cur === -1 ? 0 : Math.min(Math.max(cur + step, 0), sidebarDbs.length - 1);
      // Already at the top (k) or bottom (j): do nothing, so we don't reopen
      // the same database and cause a reload flicker.
      if (next === cur) return;
      openDb(sidebarDbs[next].name);
    };
    window.addEventListener("keydown", onJK);
    return () => window.removeEventListener("keydown", onJK);
  }, [sidebarDbs, activeName, view, cmdOpen, writeOpen, createOpen, importOpen, depositOpen, sendOpen, showOnboarding, openDb]);

  // While the user navigates by keyboard, suppress mouse :hover so a cursor
  // resting on a row (sidebar db or table row) doesn't double-highlight against
  // the keyboard selection. The class goes on for any nav keypress (arrows /
  // j / k, outside form fields) and comes off on the next real mouse move, so
  // the moment the user reaches for the mouse, hovering works again.
  React.useEffect(() => {
    const navKeys = new Set(["arrowup", "arrowdown", "arrowleft", "arrowright", "j", "k"]);
    const onKey = (e: KeyboardEvent) => {
      if (e.metaKey || e.ctrlKey || e.altKey) return;
      if (!navKeys.has(e.key.toLowerCase())) return;
      const t = e.target as HTMLElement;
      const tag = t && t.tagName;
      if (tag === "INPUT" || tag === "TEXTAREA" || tag === "SELECT" || (t && t.isContentEditable)) return;
      document.body.classList.add("kbd-nav");
    };
    // Only a *real* cursor move re-enables hover. Programmatic scroll (from
    // scrolling the keyboard selection into view) can fire mousemove with the
    // cursor's viewport coords unchanged; ignore those so the selection
    // scrolling under a resting cursor doesn't flip hover back on mid-navigation.
    let lastX: number | null = null;
    let lastY: number | null = null;
    const onMove = (e: MouseEvent) => {
      if (e.clientX === lastX && e.clientY === lastY) return;
      lastX = e.clientX;
      lastY = e.clientY;
      if (document.body.classList.contains("kbd-nav")) document.body.classList.remove("kbd-nav");
    };
    window.addEventListener("keydown", onKey);
    window.addEventListener("mousemove", onMove, true);
    return () => {
      window.removeEventListener("keydown", onKey);
      window.removeEventListener("mousemove", onMove, true);
    };
  }, []);

  // Keep the keyboard-selected row (or, on the Roles tab, a signer chip's
  // focused row, which also carries `.selected`) scrolled into view.
  React.useEffect(() => {
    if (view !== "keys") return;
    const el = document.querySelector(".dt tr.selected");
    if (el && el.scrollIntoView) el.scrollIntoView({ block: "nearest" });
  }, [view, keysTab, selected, selectedHistory, selectedFunding, selectedRejection, selectedRole, focusPubkey, rolesRows.length]);

  const isGlobal =
    view === "dashboard" ||
    view === "discover" ||
    view === "settings" ||
    view === "licenses" ||
    view === "shortcuts";

  const workspaceClasses = [
    "workspace",
    isGlobal ? "workspace-global" : "",
    view === "reference" ? "workspace-reference" : "",
  ]
    .filter(Boolean)
    .join(" ");

  // The open database's own detail is the source of truth for network /
  // block / server: it's reloaded on every switch and is per-db, so these
  // flip instantly when you change databases. The ambient status poll only
  // contributes the chain tip + last-measured latency, and only when its
  // network agrees with the active db; otherwise it's a stale cross-network
  // reading (e.g. mainnet's tip while we're now looking at a testnet db).
  // Crucially, switching databases reads these locally and does NOT trigger a
  // fresh ping; the periodic background poll refreshes tip/latency on its own.
  // The clicked db's sidebar summary. Already loaded and NOT cleared on switch
  // (unlike `detail`, which we null while the new db loads), so it carries the
  // selected db's own network + scanned height the instant you click, before
  // its detail lands. This is what keeps the status bar truthful for the db
  // you're looking at even with more databases than sync workers.
  const activeSummary =
    activeName && databases ? databases.find((d) => d.name === activeName) : null;
  // A *definite* network for the open db. Prefer the loaded detail, but only
  // when it actually describes the clicked db; during a switch `detail` is the
  // *previous* db's (keep-previous), so trusting its network is what briefly
  // flashed e.g. "testnet syncing" right after clicking a mainnet db: the stale
  // network poisoned `statusMatches` below, so the testnet tip got compared
  // against the mainnet db's height and read as "syncing". The sidebar summary
  // is always the clicked db's, so it's the reliable source mid-switch.
  const definiteNet =
    (detailMatches && activeDb && activeDb.network) ||
    (activeSummary && activeSummary.network) ||
    null;
  // Sticky last-known network: if `definiteNet` is momentarily null (sidebar
  // mid-refresh during a switch) keep showing the previous one rather than
  // flashing the ambient status / "mainnet" default. A real switch updates it
  // the same frame, since the new db's summary already carries its network.
  const lastNetRef = React.useRef<string | null>(null);
  if (definiteNet) lastNetRef.current = definiteNet;
  const activeNet = definiteNet || lastNetRef.current;
  const netName = activeNet || (status && status.network) || "mainnet";
  const statusMatches = !!status && (!activeNet || status.network === activeNet);
  // Keep the last known server label so it stays put while syncing (a slow
  // status poll briefly has no fresh value), instead of flashing the generic
  // "lightwalletd" placeholder.
  const serverRef = React.useRef("lightwalletd");
  // The sidebar summary now carries the clicked db's server, so it's right the
  // instant you switch, unlike `detail`, which is the previous db's mid-switch
  // (its server would otherwise lag a network behind the label).
  if (activeSummary && activeSummary.server) serverRef.current = activeSummary.server;
  else if (detailMatches && activeDb && activeDb.server) serverRef.current = activeDb.server;
  else if (statusMatches && status.server) serverRef.current = status.server;
  const serverLabel = serverRef.current;
  // The open db's own scanned height: the loaded detail when it actually
  // matches the selected db (freshest), else this db's sidebar summary, so it's
  // correct the instant you select a db. Both come from the *selected* db, so
  // with 50 dbs and 5 workers the bar shows this db's true height, never a
  // neighbor's, and never the previous db's height held over during a switch.
  const rawSyncedBlock =
    (detailMatches && activeDb && activeDb.synced) ||
    (activeSummary && activeSummary.synced) ||
    (statusMatches && status.synced) ||
    0;
  // Truthful but steady: never visibly step the height *backward* for the same
  // db. The summary and detail are read at different moments, so the height can
  // briefly tick down by a block between sources; that backward flicker is
  // what made the number "jump" on a click. We pin the max seen for the active
  // db and reset it when the db changes, so a genuine new db shows its own
  // (possibly lower) height immediately while same-db reads only ever climb.
  // NOTE: the one legitimate backward move is a chain reorg; we don't detect
  // that here (it needs the block *hash*, tracked as follow-up work), so a
  // post-reorg height will read one block stale until the next forward tick.
  const syncedMaxRef = React.useRef<{ name: string | null; height: number }>({ name: null, height: 0 });
  if (syncedMaxRef.current.name !== activeName) {
    syncedMaxRef.current = { name: activeName, height: rawSyncedBlock };
  } else if (rawSyncedBlock > syncedMaxRef.current.height) {
    syncedMaxRef.current.height = rawSyncedBlock;
  }
  const syncedBlock = syncedMaxRef.current.height || rawSyncedBlock;
  const networkBlock = (statusMatches && status.chain_tip) || syncedBlock;
  // Treat "within 1 block of the tip" as fully synced. The background loop
  // advances block-by-block, so a strict `== tip` check makes the UI flap
  // between synced/not-synced constantly; a 1-block tolerance is steady.
  const isSynced = networkBlock > 0 && syncedBlock >= networkBlock - 1;
  // First-import gate. Until the open db has been scanned from its birthday all
  // the way to a known chain tip, we cannot be certain whether a valid INIT
  // exists: a partial scan reads "uninitialized" only because the INIT block
  // isn't reached yet, then flips once it lands. That race made the panel
  // flicker "not initialized" before settling. We are sure once either the read
  // already found an INIT (init != "uninitialized") OR the db's own scanned
  // height (detail.synced, the height that produced this init verdict, not the
  // racy status height) has reached the tip. Until then, hold every tab in a
  // single syncing view. Gating on detail.synced (vs the live status height)
  // is what keeps the verdict and the height from coming from different reads.
  const tipKnown = statusMatches && status.chain_tip != null && status.chain_tip > 0;
  const detailSynced = detailMatches && detail && detail.synced != null ? detail.synced : null;
  const initKnown = !!detail && detail.init !== "uninitialized";
  const fullyScanned = !!(tipKnown && detailSynced != null && detailSynced >= (status!.chain_tip as number) - 1);
  const firstSyncPending = !!detail && detailMatches && !initKnown && !fullyScanned;
  const latency = statusMatches && status.latency_ms != null ? status.latency_ms : null;
  // The status-bar funds come from `detail` (there's no per-db balance in the
  // sidebar summary), so during a *cross-network* switch the stale balance and
  // its currency would contradict the now-correct network label. Show it only
  // when it's for the network we're labeling: the matched detail, or a
  // same-network switch where the keep-previous balance still reads in the right
  // currency. Otherwise hide it for the beat until the new detail lands.
  const statusBarDb =
    view === "keys" && activeDb && (detailMatches || activeDb.network === netName)
      ? activeDb
      : null;

  const outOfDate = !!(status && status.build_out_of_date);

  return (
    <div className={outOfDate ? "app has-banner" : "app"}>
      {outOfDate && (
        <div className="update-banner" role="alert">
          This build is out of date. Please download the latest build from{" "}
          <span className="update-banner-url">github.com/zecrocks/zkv</span> to
          continue using zkv.
        </div>
      )}
      <Topbar
        db={view === "keys" ? activeDb : null}
        view={view}
        onCmd={() => setCmdOpen(true)}
        onCopy={handleCopy}
        onDeposit={() => setDepositOpen(true)}
        onSend={() => setSendOpen(true)}
        syncing={syncing}
        syncedBlock={syncedBlock}
        networkLatency={latency}
      />

      <div className={workspaceClasses} data-screen-label={`ZKV, ${view}`}>
        <Sidebar
          view={view}
          onSelectView={(v) => {
            // Opening Reference from the sidebar lands on the default section,
            // not wherever a prior command-palette jump left it.
            if (v === "reference") setRefSection("quickstart");
            setView(v);
          }}
          databases={databases}
          sidebarDbs={sidebarDbs}
          truncated={dbTruncated}
          totalCount={databases.length}
          onViewAll={() => setCmdOpen(true)}
          activeName={view === "keys" ? activeName : null}
          onSelect={openDb}
          onCreate={() => setCreateOpen(true)}
          onImport={() => setImportOpen(true)}
        />

        {view === "dashboard" && (
          <main className="global-main" data-screen-label="Dashboard">
            <Dashboard
              databases={databases}
              detail={detail}
              onOpenDb={openDb}
              onCreate={() => setCreateOpen(true)}
              booted={booted}
            />
          </main>
        )}

        {view === "discover" && (
          <main className="global-main" data-screen-label="Discover">
            <Discover onWatch={() => setImportOpen(true)} />
          </main>
        )}

        {view === "settings" && (
          <main className="global-main" data-screen-label="Settings">
            <Settings
              theme={theme}
              onTheme={setTheme}
              timeZone={timeZone}
              onTimeZone={setTimeZone}
              onResetOnboarding={resetOnboarding}
              status={status}
              databases={databases}
              onForget={onForget}
              onSyncWorkers={onSyncWorkers}
              onViewLicenses={() => setView("licenses")}
              onViewShortcuts={() => setView("shortcuts")}
              onReimportDemo={onReimportDemo}
            />
          </main>
        )}

        {view === "reference" && (
          <Reference
            databases={databases}
            activeName={activeName}
            onCopy={handleCopy}
            target={refSection}
          />
        )}

        {view === "licenses" && (
          <main className="global-main" data-screen-label="Licenses">
            <Licenses onBack={() => setView("settings")} />
          </main>
        )}

        {view === "shortcuts" && (
          <main className="global-main" data-screen-label="Keyboard Shortcuts">
            <KeyboardShortcuts onBack={() => setView("settings")} />
          </main>
        )}

        {view === "keys" && (
          <>
            <KeyList
              db={activeDb}
              detail={detail}
              rows={rows}
              allRows={detail?.keys || []}
              selectedIdx={selected}
              onSelect={setSelected}
              filter={filter}
              onFilter={onFilterChange}
              tab={keysTab}
              onTab={changeTab}
              onWriteKey={openWrite}
              onDelete={openDelete}
              paused={!!(detail && detail.paused)}
              onTogglePause={doTogglePause}
              onManualSync={doManualSync}
              manualSyncing={manualSyncing}
              // Show the loading state only with nothing to display yet (cold
              // open) or when a switch is dragging (switchSlow). During a normal
              // fast switch we keep the previous db's rows on screen instead of
              // flashing empty, then swap them when the new detail lands.
              loading={detail === null || switchSlow}
              // First-import sync gate: while a freshly-imported db (the demo,
              // any zkv1 watch) is still scanning birthday->tip we can't yet
              // conclude whether it has a valid INIT, so hold every tab in a
              // single syncing view instead of flashing "not initialized".
              // Pass only a known tip so the panel shows real progress.
              chainTip={tipKnown ? (status!.chain_tip as number) : 0}
              firstSyncPending={firstSyncPending}
              history={historyEntries}
              historyLoading={historyLoading}
              selectedHistoryIdx={selectedHistory}
              onSelectHistory={setSelectedHistory}
              historyTotal={history ? history.total : 0}
              historyOffset={historyOffset}
              historyPageSize={HISTORY_PAGE}
              onHistoryPage={(o: number) => {
                setHistoryOffset(Math.max(0, o));
                setSelectedHistory(0);
              }}
              rejections={rejections ? rejections.entries : null}
              rejectionsLoading={rejectionsLoading}
              selectedRejectionIdx={selectedRejection}
              onSelectRejection={setSelectedRejection}
              roles={rolesRows}
              rolesRevoked={rolesRevoked}
              rolesCreator={roles ? roles.creator : null}
              rolesLoading={rolesLoading}
              onCopy={handleCopy}
              funding={fundingEntries}
              fundingLoading={fundingLoading}
              selectedFundingIdx={selectedFunding}
              onSelectFunding={setSelectedFunding}
              fundingTotal={funding ? funding.total : 0}
              fundingOffset={fundingOffset}
              fundingPageSize={HISTORY_PAGE}
              onFundingPage={(o: number) => {
                setFundingOffset(Math.max(0, o));
                setSelectedFunding(0);
              }}
              timeZone={timeZone}
              selectedRoleIdx={selectedRole}
              onSelectRole={setSelectedRole}
            />
            {firstSyncPending ? null : keysTab === "funding" ? (
              <FundingDetail
                tx={fundingEntries[selectedFunding]}
                onCopy={handleCopy}
                timeZone={timeZone}
                network={activeDb && activeDb.network}
                onOpenTxid={openTxid}
              />
            ) : keysTab === "history" ? (
              <HistoryDetail
                entry={historyEntries[selectedHistory]}
                creator={history && history.creator}
                onCopy={handleCopy}
                timeZone={timeZone}
                network={activeDb && activeDb.network}
                roles={rolesRows}
                onOpenRole={openRole}
              />
            ) : keysTab === "rejections" ? (
              <RejectionDetail
                entry={rejections && rejections.entries[selectedRejection]}
                onCopy={handleCopy}
                timeZone={timeZone}
              />
            ) : keysTab === "roles" ? (
              <RoleDetail
                entry={rolesAll[selectedRole]}
                db={activeDb}
                creator={roles ? roles.creator : null}
                timeZone={timeZone}
                onCopy={handleCopy}
                loading={rolesLoading}
              />
            ) : (
              <KeyDetail
                row={rows[selected]}
                db={activeDb}
                timeZone={timeZone}
                onWriteKey={openWrite}
                onDelete={openDelete}
                onCopy={handleCopy}
                onViewHistory={viewHistory}
                onOpenTxid={openTxid}
                signer={activeDb && activeDb.signer}
                roles={rolesRows}
                onOpenRole={openRole}
                loading={detail === null || switchSlow}
              />
            )}
          </>
        )}

      </div>

      <StatusBar
        db={statusBarDb}
        synced={syncedBlock}
        isSynced={isSynced}
        networkBlock={networkBlock}
        latency={latency}
        syncing={syncing}
        network={netName}
        server={serverLabel}
        version={status && status.version}
        gitSha={status && status.git_sha}
        onDeposit={() => setDepositOpen(true)}
        pausedAll={!!(status && status.paused_all)}
        onTogglePauseAll={doTogglePauseAll}
      />

      <CommandPalette
        open={cmdOpen}
        onClose={() => setCmdOpen(false)}
        onGo={handleCmdGo}
        databases={databases}
      />

      {writeOpen && activeDb && (
        <WriteFlow
          db={activeDb}
          mode={writeMode}
          prefillKey={writePrefill ? writePrefill.key : ""}
          prefillValue={writePrefill ? writePrefill.value || "" : ""}
          synced={isSynced}
          paused={!!(detail && detail.paused) || !!(status && status.paused_all)}
          onBroadcast={doBroadcast}
          onInit={doInitWrite}
          onSync={doManualSync}
          syncing={syncing || manualSyncing}
          onClose={() => setWriteOpen(false)}
          onDone={() => loadDetail(activeName!)}
          onDeposit={() => setDepositOpen(true)}
        />
      )}

      {depositOpen && activeDb && (
        <DepositModal
          db={activeDb}
          onClose={() => setDepositOpen(false)}
          onCopy={handleCopy}
          onInited={async () => {
            // Faucet broadcast our INIT memo. Close deposit, sync so the
            // mempool memo flips the db to "initializing", then open the
            // write modal, which lands on its waiting-for-INIT view.
            setDepositOpen(false);
            try { await api.sync(activeName!, true); } catch (_) {}
            await loadDetail(activeName!);
            refreshDatabases();
            setWriteOpen(true);
          }}
        />
      )}

      {sendOpen && activeDb && (
        <SendModal
          db={activeDb}
          onClose={() => setSendOpen(false)}
          onCopy={handleCopy}
          onDeposit={() => { setSendOpen(false); setDepositOpen(true); }}
          onDone={() => loadDetail(activeName!)}
        />
      )}

      {createOpen && (
        <CreateFlow
          onCancel={() => setCreateOpen(false)}
          onCreate={onCreate}
          onGeneratePhrase={onGeneratePhrase}
          onInit={onInit}
          pollDb={pollDb}
          servers={servers}
          existingNames={databases.map((d) => d.name)}
          minInitZats={MIN_INIT_ZATS}
          onComplete={(name) => {
            setCreateOpen(false);
            refreshDatabases();
            openDb(name);
          }}
        />
      )}

      {importOpen && (
        <ImportFlow
          onCancel={() => setImportOpen(false)}
          onWatch={onWatch}
          onRestore={onRestore}
          onComplete={() => setImportOpen(false)}
        />
      )}

      {showOnboarding && (
        <Onboarding onChoose={finishOnboard} onSkip={() => finishOnboard("dismiss")} version={status && status.version} />
      )}

      {notice && (
        <div className={"zkv-toast " + notice.kind} role="status">
          {notice.text}
        </div>
      )}
    </div>
  );
}

(ReactDOM as any).createRoot(document.getElementById("root")).render(<App />);
