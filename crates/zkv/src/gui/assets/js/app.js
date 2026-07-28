const api = window.zkvApi;
const MIN_INIT_ZATS = 1e4;
const DEMO_DB_NAME = "demo-oracles";
const HISTORY_PAGE = 100;
function App() {
  const [theme, setTheme] = React.useState(() => {
    try {
      return localStorage.getItem("zkv-theme") || "light";
    } catch {
      return "light";
    }
  });
  const [timeZone, setTimeZone] = React.useState(() => {
    try {
      return localStorage.getItem("zkv-tz") || "local";
    } catch {
      return "local";
    }
  });
  const [view, setView] = React.useState("dashboard");
  const [refSection, setRefSection] = React.useState(null);
  const [activeName, setActiveName] = React.useState(null);
  const [filter, setFilter] = React.useState("");
  const [selected, setSelected] = React.useState(0);
  const [keysTab, setKeysTab] = React.useState("browse");
  const [cmdOpen, setCmdOpen] = React.useState(false);
  const [focusPubkey, setFocusPubkey] = React.useState(null);
  const [databases, setDatabases] = React.useState([]);
  const [detail, setDetail] = React.useState(null);
  const [detailError, setDetailError] = React.useState(null);
  const [switching, setSwitching] = React.useState(false);
  const [switchSlow, setSwitchSlow] = React.useState(false);
  const [status, setStatus] = React.useState(null);
  const [servers, setServers] = React.useState(null);
  const [syncing, setSyncing] = React.useState(false);
  const [manualSyncing, setManualSyncing] = React.useState(false);
  const [notice, setNotice] = React.useState(null);
  const [booted, setBooted] = React.useState(false);
  const [history, setHistory] = React.useState(null);
  const [historyLoading, setHistoryLoading] = React.useState(false);
  const [selectedHistory, setSelectedHistory] = React.useState(0);
  const [historyOffset, setHistoryOffset] = React.useState(0);
  const [focusTxid, setFocusTxid] = React.useState(null);
  const [locateTxid, setLocateTxid] = React.useState(null);
  const [rejections, setRejections] = React.useState(null);
  const [rejectionsLoading, setRejectionsLoading] = React.useState(false);
  const [selectedRejection, setSelectedRejection] = React.useState(0);
  const [roles, setRoles] = React.useState(null);
  const [rolesLoading, setRolesLoading] = React.useState(false);
  const [selectedRole, setSelectedRole] = React.useState(0);
  const [funding, setFunding] = React.useState(null);
  const [fundingLoading, setFundingLoading] = React.useState(false);
  const [selectedFunding, setSelectedFunding] = React.useState(0);
  const [fundingOffset, setFundingOffset] = React.useState(0);
  const [writeOpen, setWriteOpen] = React.useState(false);
  const [writeMode, setWriteMode] = React.useState("set");
  const [writePrefill, setWritePrefill] = React.useState(null);
  const [depositOpen, setDepositOpen] = React.useState(false);
  const [sendOpen, setSendOpen] = React.useState(false);
  const [createOpen, setCreateOpen] = React.useState(false);
  const [importOpen, setImportOpen] = React.useState(false);
  const [showOnboarding, setShowOnboarding] = React.useState(false);
  const flash = React.useCallback((text, kind = "info") => {
    setNotice({ text, kind });
    window.clearTimeout(flash._t);
    flash._t = window.setTimeout(() => setNotice(null), 4200);
  }, []);
  const refreshDatabases = React.useCallback(async () => {
    try {
      const dbs = await api.listDatabases();
      setDatabases(dbs);
      return dbs;
    } catch (e) {
      flash("Couldn't list databases: " + e.message, "error");
      return [];
    }
  }, [flash]);
  const refreshDatabasesBasic = React.useCallback(async () => {
    try {
      const basic = await api.listDatabasesBasic();
      setDatabases((prev) => {
        const byName = new Map(prev.map((d) => [d.name, d]));
        return basic.map((b) => {
          const old = byName.get(b.name);
          return old && old.detailed ? { ...old, paused: b.paused } : b;
        });
      });
      return basic;
    } catch (_) {
      return [];
    }
  }, []);
  const refreshStatus = React.useCallback(async () => {
    try {
      const s = await api.status();
      setStatus(s);
      return s;
    } catch (_) {
      return null;
    }
  }, []);
  const loadSeqRef = React.useRef(0);
  const loadDetail = React.useCallback(
    async (name) => {
      const seq = ++loadSeqRef.current;
      try {
        const d = await api.detail(name);
        if (seq !== loadSeqRef.current) return d;
        setDetail(d);
        setDetailError(null);
        return d;
      } catch (e) {
        if (seq !== loadSeqRef.current) return null;
        setDetail(null);
        setDetailError(e.message || "Couldn't open " + name + ".");
        return null;
      }
    },
    [flash]
  );
  const retryDetail = React.useCallback(() => {
    if (activeName) loadDetail(activeName);
  }, [activeName, loadDetail]);
  const loadHistory = React.useCallback(
    async (name, filter2, offset, locate) => {
      setHistoryLoading(true);
      try {
        const h = await api.history(name, {
          filter: locate ? void 0 : filter2 || void 0,
          limit: HISTORY_PAGE,
          offset: locate ? void 0 : offset || 0,
          locate: locate || void 0
        });
        setHistory(h);
        return h;
      } catch (e) {
        flash("Couldn't load history: " + e.message, "error");
        setHistory({ creator: "", entries: [], total: 0, offset: 0, limit: HISTORY_PAGE });
        return null;
      } finally {
        setHistoryLoading(false);
      }
    },
    [flash]
  );
  const loadRejections = React.useCallback(
    async (name) => {
      setRejectionsLoading(true);
      try {
        const r = await api.rejections(name);
        setRejections(r);
        return r;
      } catch (e) {
        flash("Couldn't load rejections: " + e.message, "error");
        setRejections({ entries: [], total: 0 });
        return null;
      } finally {
        setRejectionsLoading(false);
      }
    },
    [flash]
  );
  const loadRoles = React.useCallback(
    async (name) => {
      setRolesLoading(true);
      try {
        const r = await api.roles(name);
        setRoles(r);
        return r;
      } catch (e) {
        flash("Couldn't load roles: " + e.message, "error");
        setRoles({ creator: null, rows: [], revoked: [] });
        return null;
      } finally {
        setRolesLoading(false);
      }
    },
    [flash]
  );
  const loadFunding = React.useCallback(
    async (name, offset) => {
      setFundingLoading(true);
      try {
        const f = await api.funding(name, { limit: HISTORY_PAGE, offset: offset || 0 });
        setFunding(f);
        return f;
      } catch (e) {
        flash("Couldn't load funding: " + e.message, "error");
        setFunding({ entries: [], total: 0, offset: 0, limit: HISTORY_PAGE });
        return null;
      } finally {
        setFundingLoading(false);
      }
    },
    [flash]
  );
  React.useEffect(() => {
    (async () => {
      const dbs = await refreshDatabasesBasic();
      const st = await refreshStatus();
      refreshDatabases();
      const userDbs = dbs.filter((d) => d.name !== DEMO_DB_NAME);
      if (userDbs.length === 0 && !(st && st.onboarded)) setShowOnboarding(true);
      setBooted(true);
    })();
    api.servers().then(setServers).catch(() => {
    });
  }, []);
  const SWITCH_SPINNER_MS = 400;
  React.useEffect(() => {
    if (!switching) {
      setSwitchSlow(false);
      return;
    }
    const id = setTimeout(() => setSwitchSlow(true), SWITCH_SPINNER_MS);
    return () => clearTimeout(id);
  }, [switching]);
  React.useEffect(() => {
    try {
      localStorage.setItem("zkv-theme", theme);
    } catch {
    }
    document.documentElement.setAttribute("data-theme", theme);
  }, [theme]);
  React.useEffect(() => {
    try {
      localStorage.setItem("zkv-tz", timeZone);
    } catch {
    }
  }, [timeZone]);
  React.useEffect(() => {
    const labels = {
      dashboard: "Dashboard",
      discover: "Discover",
      settings: "Settings",
      create: "New database",
      import: "Import"
    };
    let title = "z:kv Browser";
    if (view === "keys" && activeName) title = `${activeName} | z:kv Browser`;
    else if (labels[view]) title = `${labels[view]} | z:kv Browser`;
    document.title = title;
  }, [view, activeName]);
  React.useEffect(() => {
    const ids = [800, 2e3, 4e3, 7e3].map(
      (ms) => setTimeout(() => refreshDatabases(), ms)
    );
    return () => ids.forEach(clearTimeout);
  }, [refreshDatabases]);
  React.useEffect(() => {
    const tick = () => {
      refreshStatus();
      if (activeName) loadDetail(activeName);
      refreshDatabases();
    };
    const id = setInterval(tick, 1e4);
    return () => clearInterval(id);
  }, [refreshStatus, loadDetail, refreshDatabases, activeName]);
  React.useEffect(() => {
    const onKey = (e) => {
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
  }, [view, keysTab, activeName, detail, filter, historyOffset, locateTxid]);
  React.useEffect(() => {
    if (view === "keys" && keysTab === "rejections" && activeName) {
      loadRejections(activeName);
    }
  }, [view, keysTab, activeName, detail]);
  React.useEffect(() => {
    if (view === "keys" && activeName) {
      loadRoles(activeName);
    }
  }, [view, activeName, detail]);
  React.useEffect(() => {
    if (view === "keys" && keysTab === "funding" && activeName) {
      const id = setTimeout(() => loadFunding(activeName, fundingOffset), 200);
      return () => clearTimeout(id);
    }
  }, [view, keysTab, activeName, detail, fundingOffset]);
  const openNameRef = React.useRef(null);
  const openDb = React.useCallback(
    async (name) => {
      const reopen = openNameRef.current === name;
      openNameRef.current = name;
      setView("keys");
      if (reopen) return;
      setActiveName(name);
      setSelected(0);
      setFilter("");
      setKeysTab("browse");
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
      setDetailError(null);
      await loadDetail(name);
      setSwitching(false);
    },
    [loadDetail]
  );
  const onFilterChange = React.useCallback((v) => {
    setFilter(v);
    setHistoryOffset(0);
  }, []);
  const viewHistory = React.useCallback((key) => {
    setFilter(key || "");
    setSelectedHistory(0);
    setHistoryOffset(0);
    setFocusTxid(null);
    setKeysTab("history");
  }, []);
  const openTxid = React.useCallback((key, txid) => {
    setFilter("");
    setSelectedHistory(0);
    setHistoryOffset(0);
    setFocusTxid(txid || null);
    setLocateTxid(txid || null);
    setKeysTab("history");
  }, []);
  const openRole = React.useCallback((pubkey) => {
    setKeysTab("roles");
    setFocusPubkey(pubkey || null);
  }, []);
  const doSync = React.useCallback(
    async (name, mempool) => {
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
        flash("Sync failed: " + e.message, "error");
      } finally {
        setSyncing(false);
      }
    },
    [activeName, loadDetail, refreshStatus, flash]
  );
  const manualSyncingRef = React.useRef(false);
  const doManualSync = React.useCallback(async () => {
    if (!activeName || manualSyncingRef.current) return;
    manualSyncingRef.current = true;
    setManualSyncing(true);
    try {
      await api.sync(activeName, true);
      await loadDetail(activeName);
      await refreshStatus();
    } catch (e) {
      flash("Sync failed: " + e.message, "error");
    } finally {
      manualSyncingRef.current = false;
      setManualSyncing(false);
    }
  }, [activeName, loadDetail, refreshStatus, flash]);
  const doTogglePause = React.useCallback(async () => {
    if (!activeName) return;
    const next = !(detail && detail.paused);
    const setPausedLocally = (name, value) => {
      setDetail((d) => d && d.name === name ? { ...d, paused: value } : d);
      setDatabases(
        (dbs) => dbs.map((db) => db.name === name ? { ...db, paused: value } : db)
      );
    };
    setPausedLocally(activeName, next);
    try {
      await api.setPause(activeName, next);
      loadDetail(activeName);
      refreshDatabases();
    } catch (e) {
      setPausedLocally(activeName, !next);
      flash("Couldn't change sync: " + e.message, "error");
    }
  }, [activeName, detail, loadDetail, refreshDatabases, flash]);
  const doTogglePauseAll = React.useCallback(async () => {
    const next = !(status && status.paused_all);
    setStatus((s) => s ? { ...s, paused_all: next } : s);
    try {
      await api.pauseAll(next);
      refreshStatus();
    } catch (e) {
      setStatus((s) => s ? { ...s, paused_all: !next } : s);
      flash("Couldn't change sync: " + e.message, "error");
    }
  }, [status, refreshStatus, flash]);
  const onSyncWorkers = React.useCallback(
    async (n) => {
      try {
        await api.setSettings(n);
        await refreshStatus();
      } catch (e) {
        flash("Couldn't update workers: " + e.message, "error");
      }
    },
    [refreshStatus, flash]
  );
  const onForget = React.useCallback(
    async (name) => {
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
  const onReimportDemo = React.useCallback(async () => {
    const r = await api.reimportDemo();
    await refreshDatabases();
    await refreshStatus();
    flash("Re-imported " + r.name);
  }, [refreshDatabases, refreshStatus, flash]);
  const openWrite = (row) => {
    setWritePrefill(row);
    setWriteMode("set");
    setWriteOpen(true);
  };
  const openDelete = (row) => {
    setWritePrefill(row);
    setWriteMode("del");
    setWriteOpen(true);
  };
  const doBroadcast = React.useCallback(
    async (mode, key, value) => {
      const txid = mode === "del" ? (await api.del(activeName, key)).txid : (await api.set(activeName, key, value)).txid;
      loadDetail(activeName);
      refreshDatabases();
      return txid;
    },
    [activeName, loadDetail, refreshDatabases]
  );
  const doInitWrite = React.useCallback(async () => {
    const txid = (await api.init(activeName, { requireSync: true })).txid;
    loadDetail(activeName);
    refreshDatabases();
    return txid;
  }, [activeName, loadDetail, refreshDatabases]);
  const handleCopy = (text) => {
    try {
      navigator.clipboard.writeText(text);
      flash("Copied to clipboard");
    } catch {
    }
  };
  const finishOnboard = (path) => {
    api.markOnboarded().catch(() => {
    });
    setShowOnboarding(false);
    if (path === "reference") {
      setView("reference");
      setRefSection("quickstart");
      return;
    }
    if (path === "demo") {
      openDb(DEMO_DB_NAME);
      return;
    }
    setView("dashboard");
    if (path === "create") setCreateOpen(true);
    else if (path === "watch") setImportOpen(true);
  };
  const resetOnboarding = () => setShowOnboarding(true);
  const onCreate = async (name, network, pool, phrase) => api.create(name, network, pool, phrase);
  const onGeneratePhrase = async () => (await api.generatePhrase()).phrase;
  const onInit = async (name) => api.init(name);
  const pollDb = async (name) => {
    try {
      await api.sync(name);
    } catch (_) {
    }
    return api.detail(name);
  };
  const onWatch = async (addr, nickname) => {
    const r = await api.watch(addr, nickname || null);
    await refreshDatabases();
    flash("Watching " + r.name);
    openDb(r.name);
    return r;
  };
  const onRestore = async (name, phrase, network, pool, birthday) => {
    const r = await api.restore(name, phrase, network, pool, birthday);
    await refreshDatabases();
    flash("Restored " + r.name);
    openDb(r.name);
    return r;
  };
  const handleCmdGo = (target) => {
    if (target === "dashboard" || target === "discover" || target === "settings" || target === "shortcuts") {
      setView(target);
    } else if (target === "create") setCreateOpen(true);
    else if (target === "import") setImportOpen(true);
    else if (target === "sync") doSync();
    else if (target === "theme") setTheme((t) => t === "light" ? "dark" : "light");
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
      setRefSection("op:" + target.slice(4));
      setView("reference");
    }
  };
  const detailMatches = !!detail && detail.name === activeName;
  const detailFailed = detail === null && detailError !== null;
  const activeDb = detail ? {
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
    init_required: detail.init_required
  } : null;
  const keysTabs = React.useMemo(
    () => activeDb && activeDb.role === "admin" ? ["browse", "history", "roles", "funding", "rejections"] : ["browse", "history", "roles", "rejections"],
    [activeDb && activeDb.role]
  );
  const rows = switchSlow ? [] : (detail?.keys || []).filter(
    (r) => !filter || r.key.toLowerCase().includes(filter.toLowerCase())
  );
  const historyEntries = history?.entries || [];
  const fundingEntries = funding?.entries || [];
  const rejectionEntries = rejections?.entries || [];
  const rolesRows = roles?.rows || [];
  const rolesRevoked = roles?.revoked || [];
  const rolesAll = React.useMemo(
    () => [
      ...rolesRows.map((r) => ({ ...r, revoked: false })),
      ...rolesRevoked.map((r) => ({ ...r, revoked: true }))
    ],
    [rolesRows, rolesRevoked]
  );
  React.useEffect(() => {
    const clamp = (val, len, set) => {
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
    selectedRole
  ]);
  React.useEffect(() => {
    if (!focusTxid) return;
    const idx = historyEntries.findIndex((e) => e.txid === focusTxid);
    if (idx >= 0) {
      setSelectedHistory(idx);
      setFocusTxid(null);
    }
  }, [history, focusTxid]);
  React.useEffect(() => {
    if (!focusPubkey) return;
    const idx = rolesAll.findIndex((r) => r.pubkey === focusPubkey);
    if (idx >= 0) {
      setSelectedRole(idx);
      setFocusPubkey(null);
    }
  }, [focusPubkey, roles]);
  const lastRowMove = React.useRef(0);
  React.useEffect(() => {
    if (view !== "keys") return;
    const onArrow = (e) => {
      if (e.key !== "ArrowDown" && e.key !== "ArrowUp") return;
      if (e.metaKey || e.ctrlKey || e.altKey) return;
      const t = e.target;
      const tag = t && t.tagName;
      const inFilter = t && t.closest && t.closest(".kv-filter");
      if (!inFilter && (tag === "INPUT" || tag === "TEXTAREA" || tag === "SELECT" || t && t.isContentEditable)) return;
      const len = keysTab === "history" ? historyEntries.length : keysTab === "funding" ? fundingEntries.length : keysTab === "rejections" ? rejectionEntries.length : keysTab === "roles" ? rolesAll.length : rows.length;
      if (len === 0) return;
      e.preventDefault();
      const now = Date.now();
      if (e.repeat && now - lastRowMove.current < 70) return;
      lastRowMove.current = now;
      const step = e.key === "ArrowDown" ? 1 : -1;
      const move = (i) => Math.max(0, Math.min(len - 1, i + step));
      if (keysTab === "history") setSelectedHistory(move);
      else if (keysTab === "funding") setSelectedFunding(move);
      else if (keysTab === "rejections") setSelectedRejection(move);
      else if (keysTab === "roles") setSelectedRole(move);
      else setSelected(move);
    };
    window.addEventListener("keydown", onArrow);
    return () => window.removeEventListener("keydown", onArrow);
  }, [view, keysTab, rows.length, historyEntries.length, fundingEntries.length, rejectionEntries.length, rolesAll.length]);
  const changeTab = React.useCallback((t) => {
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
  const lastTabSwitch = React.useRef(0);
  React.useEffect(() => {
    if (view !== "keys") return;
    const onArrow = (e) => {
      if (e.key !== "ArrowLeft" && e.key !== "ArrowRight") return;
      if (e.metaKey || e.ctrlKey || e.altKey) return;
      const t = e.target;
      const tag = t && t.tagName;
      if (tag === "INPUT" || tag === "TEXTAREA" || tag === "SELECT" || t && t.isContentEditable) return;
      e.preventDefault();
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
  const DB_SIDEBAR_LIMIT = 10;
  const orderedDbs = React.useMemo(
    () => [
      ...databases.filter((d) => d.role === "admin"),
      ...databases.filter((d) => d.role === "watch")
    ],
    [databases]
  );
  const dbTruncated = databases.length > DB_SIDEBAR_LIMIT;
  const sidebarDbs = React.useMemo(() => {
    if (!dbTruncated) return orderedDbs;
    return [...databases].sort((a, b) => (b.updated_at || 0) - (a.updated_at || 0)).slice(0, DB_SIDEBAR_LIMIT);
  }, [databases, dbTruncated, orderedDbs]);
  React.useEffect(() => {
    const onJK = (e) => {
      if (e.metaKey || e.ctrlKey || e.altKey) return;
      const k = e.key.toLowerCase();
      if (k !== "j" && k !== "k") return;
      const t = e.target;
      const tag = t && t.tagName;
      if (tag === "INPUT" || tag === "TEXTAREA" || tag === "SELECT" || t && t.isContentEditable) return;
      if (cmdOpen || writeOpen || createOpen || importOpen || depositOpen || sendOpen || showOnboarding) return;
      if (sidebarDbs.length === 0) return;
      e.preventDefault();
      const cur = view === "keys" ? sidebarDbs.findIndex((d) => d.name === activeName) : -1;
      const step = k === "j" ? 1 : -1;
      const next = cur === -1 ? 0 : Math.min(Math.max(cur + step, 0), sidebarDbs.length - 1);
      if (next === cur) return;
      openDb(sidebarDbs[next].name);
    };
    window.addEventListener("keydown", onJK);
    return () => window.removeEventListener("keydown", onJK);
  }, [sidebarDbs, activeName, view, cmdOpen, writeOpen, createOpen, importOpen, depositOpen, sendOpen, showOnboarding, openDb]);
  React.useEffect(() => {
    const navKeys = /* @__PURE__ */ new Set(["arrowup", "arrowdown", "arrowleft", "arrowright", "j", "k"]);
    const onKey = (e) => {
      if (e.metaKey || e.ctrlKey || e.altKey) return;
      if (!navKeys.has(e.key.toLowerCase())) return;
      const t = e.target;
      const tag = t && t.tagName;
      if (tag === "INPUT" || tag === "TEXTAREA" || tag === "SELECT" || t && t.isContentEditable) return;
      document.body.classList.add("kbd-nav");
    };
    let lastX = null;
    let lastY = null;
    const onMove = (e) => {
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
  React.useEffect(() => {
    if (view !== "keys") return;
    const el = document.querySelector(".dt tr.selected");
    if (el && el.scrollIntoView) el.scrollIntoView({ block: "nearest" });
  }, [view, keysTab, selected, selectedHistory, selectedFunding, selectedRejection, selectedRole, focusPubkey, rolesRows.length]);
  const isGlobal = view === "dashboard" || view === "discover" || view === "settings" || view === "licenses" || view === "shortcuts";
  const workspaceClasses = [
    "workspace",
    isGlobal ? "workspace-global" : "",
    view === "reference" ? "workspace-reference" : ""
  ].filter(Boolean).join(" ");
  const activeSummary = activeName && databases ? databases.find((d) => d.name === activeName) : null;
  const definiteNet = detailMatches && activeDb && activeDb.network || activeSummary && activeSummary.network || null;
  const lastNetRef = React.useRef(null);
  if (definiteNet) lastNetRef.current = definiteNet;
  const activeNet = definiteNet || lastNetRef.current;
  const netName = activeNet || status && status.network || "mainnet";
  const statusMatches = !!status && (!activeNet || status.network === activeNet);
  const serverRef = React.useRef("lightwalletd");
  if (activeSummary && activeSummary.server) serverRef.current = activeSummary.server;
  else if (detailMatches && activeDb && activeDb.server) serverRef.current = activeDb.server;
  else if (statusMatches && status.server) serverRef.current = status.server;
  const serverLabel = serverRef.current;
  const rawSyncedBlock = detailMatches && activeDb && activeDb.synced || activeSummary && activeSummary.synced || statusMatches && status.synced || 0;
  const syncedMaxRef = React.useRef({ name: null, height: 0 });
  if (syncedMaxRef.current.name !== activeName) {
    syncedMaxRef.current = { name: activeName, height: rawSyncedBlock };
  } else if (rawSyncedBlock > syncedMaxRef.current.height) {
    syncedMaxRef.current.height = rawSyncedBlock;
  }
  const syncedBlock = syncedMaxRef.current.height || rawSyncedBlock;
  const networkBlock = statusMatches && status.chain_tip || syncedBlock;
  const isSynced = networkBlock > 0 && syncedBlock >= networkBlock - 1;
  const tipKnown = statusMatches && status.chain_tip != null && status.chain_tip > 0;
  const detailSynced = detailMatches && detail && detail.synced != null ? detail.synced : null;
  const initKnown = !!detail && detail.init !== "uninitialized";
  const fullyScanned = !!(detail && detailMatches && (detail.synced_to_tip === true || tipKnown && detailSynced != null && detailSynced >= status.chain_tip - 1));
  const firstSyncSettledRef = React.useRef({ name: null, settled: false });
  if (firstSyncSettledRef.current.name !== activeName) {
    firstSyncSettledRef.current = { name: activeName, settled: false };
  } else if (detailMatches && (initKnown || fullyScanned)) {
    firstSyncSettledRef.current.settled = true;
  }
  const firstSyncPending = !!detail && detailMatches && !initKnown && !fullyScanned && !firstSyncSettledRef.current.settled;
  const latency = statusMatches && status.latency_ms != null ? status.latency_ms : null;
  const statusBarDb = view === "keys" && activeDb && (detailMatches || activeDb.network === netName) ? activeDb : null;
  const outOfDate = !!(status && status.build_out_of_date);
  return /* @__PURE__ */ React.createElement("div", { className: outOfDate ? "app has-banner" : "app" }, outOfDate && /* @__PURE__ */ React.createElement("div", { className: "update-banner", role: "alert" }, "This build is out of date. Please download the latest build from", " ", /* @__PURE__ */ React.createElement("span", { className: "update-banner-url" }, "github.com/zecrocks/zkv"), " to continue using zkv."), /* @__PURE__ */ React.createElement(
    Topbar,
    {
      db: view === "keys" ? activeDb : null,
      view,
      onCmd: () => setCmdOpen(true),
      onCopy: handleCopy,
      onDeposit: () => setDepositOpen(true),
      onSend: () => setSendOpen(true),
      syncing,
      syncedBlock,
      networkLatency: latency
    }
  ), /* @__PURE__ */ React.createElement("div", { className: workspaceClasses, "data-screen-label": `ZKV, ${view}` }, /* @__PURE__ */ React.createElement(
    Sidebar,
    {
      view,
      onSelectView: (v) => {
        if (v === "reference") setRefSection("quickstart");
        setView(v);
      },
      databases,
      sidebarDbs,
      truncated: dbTruncated,
      totalCount: databases.length,
      onViewAll: () => setCmdOpen(true),
      activeName: view === "keys" ? activeName : null,
      onSelect: openDb,
      onCreate: () => setCreateOpen(true),
      onImport: () => setImportOpen(true)
    }
  ), view === "dashboard" && /* @__PURE__ */ React.createElement("main", { className: "global-main", "data-screen-label": "Dashboard" }, /* @__PURE__ */ React.createElement(
    Dashboard,
    {
      databases,
      detail,
      onOpenDb: openDb,
      onCreate: () => setCreateOpen(true),
      booted
    }
  )), view === "discover" && /* @__PURE__ */ React.createElement("main", { className: "global-main", "data-screen-label": "Discover" }, /* @__PURE__ */ React.createElement(Discover, { onWatch: () => setImportOpen(true) })), view === "settings" && /* @__PURE__ */ React.createElement("main", { className: "global-main", "data-screen-label": "Settings" }, /* @__PURE__ */ React.createElement(
    Settings,
    {
      theme,
      onTheme: setTheme,
      timeZone,
      onTimeZone: setTimeZone,
      onResetOnboarding: resetOnboarding,
      status,
      databases,
      onForget,
      onSyncWorkers,
      onViewLicenses: () => setView("licenses"),
      onViewShortcuts: () => setView("shortcuts"),
      onReimportDemo
    }
  )), view === "reference" && /* @__PURE__ */ React.createElement(
    Reference,
    {
      databases,
      activeName,
      onCopy: handleCopy,
      target: refSection
    }
  ), view === "licenses" && /* @__PURE__ */ React.createElement("main", { className: "global-main", "data-screen-label": "Licenses" }, /* @__PURE__ */ React.createElement(Licenses, { onBack: () => setView("settings") })), view === "shortcuts" && /* @__PURE__ */ React.createElement("main", { className: "global-main", "data-screen-label": "Keyboard Shortcuts" }, /* @__PURE__ */ React.createElement(KeyboardShortcuts, { onBack: () => setView("settings") })), view === "keys" && /* @__PURE__ */ React.createElement(React.Fragment, null, /* @__PURE__ */ React.createElement(
    KeyList,
    {
      db: activeDb,
      detail,
      rows,
      allRows: detail?.keys || [],
      selectedIdx: selected,
      onSelect: setSelected,
      filter,
      onFilter: onFilterChange,
      tab: keysTab,
      onTab: changeTab,
      onWriteKey: openWrite,
      onDelete: openDelete,
      paused: !!(detail && detail.paused),
      onTogglePause: doTogglePause,
      onManualSync: doManualSync,
      manualSyncing,
      loading: detail === null && !detailFailed || switchSlow,
      error: detailFailed ? detailError : null,
      onRetry: retryDetail,
      syncError: detailMatches ? detail && detail.sync_error : null,
      chainTip: tipKnown ? status.chain_tip : 0,
      firstSyncPending,
      history: historyEntries,
      historyLoading,
      selectedHistoryIdx: selectedHistory,
      onSelectHistory: setSelectedHistory,
      historyTotal: history ? history.total : 0,
      historyOffset,
      historyPageSize: HISTORY_PAGE,
      onHistoryPage: (o) => {
        setHistoryOffset(Math.max(0, o));
        setSelectedHistory(0);
      },
      rejections: rejections ? rejections.entries : null,
      rejectionsLoading,
      selectedRejectionIdx: selectedRejection,
      onSelectRejection: setSelectedRejection,
      roles: rolesRows,
      rolesRevoked,
      rolesCreator: roles ? roles.creator : null,
      rolesLoading,
      onCopy: handleCopy,
      funding: fundingEntries,
      fundingLoading,
      selectedFundingIdx: selectedFunding,
      onSelectFunding: setSelectedFunding,
      fundingTotal: funding ? funding.total : 0,
      fundingOffset,
      fundingPageSize: HISTORY_PAGE,
      onFundingPage: (o) => {
        setFundingOffset(Math.max(0, o));
        setSelectedFunding(0);
      },
      timeZone,
      selectedRoleIdx: selectedRole,
      onSelectRole: setSelectedRole
    }
  ), firstSyncPending ? null : keysTab === "funding" ? /* @__PURE__ */ React.createElement(
    FundingDetail,
    {
      tx: fundingEntries[selectedFunding],
      onCopy: handleCopy,
      timeZone,
      network: activeDb && activeDb.network,
      onOpenTxid: openTxid
    }
  ) : keysTab === "history" ? /* @__PURE__ */ React.createElement(
    HistoryDetail,
    {
      entry: historyEntries[selectedHistory],
      creator: history && history.creator,
      onCopy: handleCopy,
      timeZone,
      network: activeDb && activeDb.network,
      roles: rolesRows,
      onOpenRole: openRole
    }
  ) : keysTab === "rejections" ? /* @__PURE__ */ React.createElement(
    RejectionDetail,
    {
      entry: rejections && rejections.entries[selectedRejection],
      onCopy: handleCopy,
      timeZone
    }
  ) : keysTab === "roles" ? /* @__PURE__ */ React.createElement(
    RoleDetail,
    {
      entry: rolesAll[selectedRole],
      db: activeDb,
      creator: roles ? roles.creator : null,
      timeZone,
      onCopy: handleCopy,
      loading: rolesLoading
    }
  ) : /* @__PURE__ */ React.createElement(
    KeyDetail,
    {
      row: rows[selected],
      db: activeDb,
      timeZone,
      onWriteKey: openWrite,
      onDelete: openDelete,
      onCopy: handleCopy,
      onViewHistory: viewHistory,
      onOpenTxid: openTxid,
      signer: activeDb && activeDb.signer,
      roles: rolesRows,
      onOpenRole: openRole,
      loading: detail === null && !detailFailed || switchSlow,
      error: detailFailed ? detailError : null,
      onRetry: retryDetail
    }
  ))), /* @__PURE__ */ React.createElement(
    StatusBar,
    {
      db: statusBarDb,
      synced: syncedBlock,
      isSynced,
      networkBlock,
      latency,
      syncing,
      network: netName,
      server: serverLabel,
      version: status && status.version,
      gitSha: status && status.git_sha,
      onDeposit: () => setDepositOpen(true),
      pausedAll: !!(status && status.paused_all),
      onTogglePauseAll: doTogglePauseAll
    }
  ), /* @__PURE__ */ React.createElement(
    CommandPalette,
    {
      open: cmdOpen,
      onClose: () => setCmdOpen(false),
      onGo: handleCmdGo,
      databases
    }
  ), writeOpen && activeDb && /* @__PURE__ */ React.createElement(
    WriteFlow,
    {
      db: activeDb,
      mode: writeMode,
      prefillKey: writePrefill ? writePrefill.key : "",
      prefillValue: writePrefill ? writePrefill.value || "" : "",
      synced: isSynced,
      paused: !!(detail && detail.paused) || !!(status && status.paused_all),
      onBroadcast: doBroadcast,
      onInit: doInitWrite,
      onSync: doManualSync,
      syncing: syncing || manualSyncing,
      onClose: () => setWriteOpen(false),
      onDone: () => loadDetail(activeName),
      onDeposit: () => setDepositOpen(true)
    }
  ), depositOpen && activeDb && /* @__PURE__ */ React.createElement(
    DepositModal,
    {
      db: activeDb,
      onClose: () => setDepositOpen(false),
      onCopy: handleCopy,
      onInited: async () => {
        setDepositOpen(false);
        try {
          await api.sync(activeName, true);
        } catch (_) {
        }
        await loadDetail(activeName);
        refreshDatabases();
        setWriteOpen(true);
      }
    }
  ), sendOpen && activeDb && /* @__PURE__ */ React.createElement(
    SendModal,
    {
      db: activeDb,
      onClose: () => setSendOpen(false),
      onCopy: handleCopy,
      onDeposit: () => {
        setSendOpen(false);
        setDepositOpen(true);
      },
      onDone: () => loadDetail(activeName)
    }
  ), createOpen && /* @__PURE__ */ React.createElement(
    CreateFlow,
    {
      onCancel: () => setCreateOpen(false),
      onCreate,
      onGeneratePhrase,
      onInit,
      pollDb,
      servers,
      existingNames: databases.map((d) => d.name),
      minInitZats: MIN_INIT_ZATS,
      onComplete: (name) => {
        setCreateOpen(false);
        refreshDatabases();
        openDb(name);
      }
    }
  ), importOpen && /* @__PURE__ */ React.createElement(
    ImportFlow,
    {
      onCancel: () => setImportOpen(false),
      onWatch,
      onRestore,
      onComplete: () => setImportOpen(false)
    }
  ), showOnboarding && /* @__PURE__ */ React.createElement(Onboarding, { onChoose: finishOnboard, onSkip: () => finishOnboard("dismiss"), version: status && status.version }), notice && /* @__PURE__ */ React.createElement("div", { className: "zkv-toast " + notice.kind, role: "status" }, notice.text));
}
ReactDOM.createRoot(document.getElementById("root")).render(/* @__PURE__ */ React.createElement(App, null));
