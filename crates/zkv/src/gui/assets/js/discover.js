const DISCOVER_CATS = [
  { id: "all", label: "All", ct: 142 },
  { id: "oracle", label: "Oracles", ct: 28 },
  { id: "config", label: "App config", ct: 31 },
  { id: "archive", label: "Archives", ct: 14 },
  { id: "index", label: "Indexes", ct: 22 },
  { id: "demo", label: "Demos", ct: 47 }
];
const FEATURED = [
  {
    cat: "ORACLE",
    name: "zec-usd-feed",
    by: "electriccoin.eco",
    desc: "ZEC/USD spot price, refreshed every block. Median across Coingecko, Kraken, Binance.",
    keys: 4,
    watchers: "8.2k",
    addr: "zkv1ecq9p3k\u20267sm2kvfeed"
  },
  {
    cat: "CONFIG",
    name: "mobile-flags",
    by: "shielded-labs",
    desc: "Feature flag registry for the Shielded Labs mobile wallet. Updated on release.",
    keys: 31,
    watchers: "1.4k",
    addr: "zkv1sfm4d7q\u20262kv9hflags"
  },
  {
    cat: "ARCHIVE",
    name: "zip-archive",
    by: "zcash-foundation",
    desc: "Append-only index of canonical ZIP titles and statuses. Read-only mirror.",
    keys: 132,
    watchers: "643",
    addr: "zkv1zfx7a2c\u20269hqm5zips"
  }
];
const PUBLIC = [
  { cat: "oracle", name: "gas-prices-eth", by: "merklemanufactory", desc: "Ethereum gas price gauge, 30-block moving average.", keys: 6, watchers: "4.1k", addr: "zkv1mmk3p9w\u2026q8wv2gas" },
  { cat: "index", name: "zip-progress", by: "zcash-foundation", desc: "Live state of all open Zcash Improvement Proposals.", keys: 47, watchers: "912", addr: "zkv1zfa2c7x\u2026m5kq9zips" },
  { cat: "archive", name: "rpc-changelog", by: "electriccoin.eco", desc: "Per-release JSON-RPC changelog for zcashd.", keys: 184, watchers: "503", addr: "zkv1ec7r9d2\u2026k4xv8rpc" },
  { cat: "oracle", name: "btc-ema-200", by: "foundryusa", desc: "BTC 200-day EMA. Updated daily at 00:00 UTC.", keys: 2, watchers: "2.0k", addr: "zkv1fup4kx7\u2026q9wm2ema" },
  { cat: "config", name: "wallet-fees", by: "shielded-labs", desc: "Recommended fee tiers per wallet version. Read by 3 wallets.", keys: 14, watchers: "728", addr: "zkv1sl9m2k4\u2026x7vq8fee" },
  { cat: "demo", name: "guestbook", by: "anonymous", desc: "Public guestbook from the zkv beta announcement. New entries every few hours.", keys: 1284, watchers: "4.8k", addr: "zkv1axh6q3p\u2026m9kv2book" },
  { cat: "demo", name: "haiku-of-the-day", by: "jr@local", desc: "One haiku, signed daily.", keys: 1, watchers: "47", addr: "zkv1jrd8s5m\u20269kq2vhku" }
];
const Discover = ({ onWatch }) => {
  const [activeCat, setActiveCat] = React.useState("all");
  const [q, setQ] = React.useState("");
  const filtered = PUBLIC.filter(
    (p) => (activeCat === "all" || p.cat === activeCat) && (q === "" || p.name.toLowerCase().includes(q.toLowerCase()) || p.desc.toLowerCase().includes(q.toLowerCase()))
  );
  return /* @__PURE__ */ React.createElement("div", { className: "discover" }, /* @__PURE__ */ React.createElement("div", { className: "discover-header" }, /* @__PURE__ */ React.createElement("div", { style: { fontFamily: "IBM Plex Mono", fontSize: 11, letterSpacing: "0.12em", textTransform: "uppercase", color: "var(--fg-3)" } }, "WORKSPACE"), /* @__PURE__ */ React.createElement("h2", { style: { fontFamily: "IBM Plex Sans", fontWeight: 600, fontSize: 26, letterSpacing: "-0.01em", margin: "4px 0 8px", color: "var(--fg-1)" } }, "Discover"), /* @__PURE__ */ React.createElement("div", { style: { fontSize: 14, color: "var(--fg-2)", maxWidth: 680, lineHeight: 1.55 } }, "Browse public z:kv databases, oracles, app config, archives, demos. Watch any of them with one click; you'll see the same keys the publisher sees, signed and time-stamped on chain."), /* @__PURE__ */ React.createElement("div", { className: "callout-flow note", style: { marginTop: 16, maxWidth: 680 } }, /* @__PURE__ */ React.createElement(Icon, { name: "info", size: 16, color: "var(--amber-400)" }), /* @__PURE__ */ React.createElement("div", null, /* @__PURE__ */ React.createElement("strong", { style: { color: "inherit" } }, "Sample directory."), " zkv has no on-chain registry yet, so the list below is illustrative. To watch a real database, use ", /* @__PURE__ */ React.createElement("strong", null, "Import \u2192 Watch"), " and paste its", /* @__PURE__ */ React.createElement("code", null, " zkv1\u2026"), " address."))), /* @__PURE__ */ React.createElement("div", { style: { margin: "24px 0 18px" } }, /* @__PURE__ */ React.createElement("div", { style: { fontFamily: "var(--font-mono)", fontSize: 10, letterSpacing: "0.12em", textTransform: "uppercase", color: "var(--fg-3)", marginBottom: 10 } }, "FEATURED \xB7 ORACLES"), /* @__PURE__ */ React.createElement("div", { className: "discover-featured" }, FEATURED.map((f) => /* @__PURE__ */ React.createElement("div", { key: f.name, className: "featured-card" }, /* @__PURE__ */ React.createElement("div", { className: "featured-cat" }, f.cat), /* @__PURE__ */ React.createElement("div", { className: "featured-name" }, f.name), /* @__PURE__ */ React.createElement("div", { className: "featured-desc" }, f.desc), /* @__PURE__ */ React.createElement("div", { className: "featured-foot" }, /* @__PURE__ */ React.createElement("span", null, /* @__PURE__ */ React.createElement("span", { className: "mono", style: { color: "var(--fg-1)" } }, f.keys), " keys"), /* @__PURE__ */ React.createElement("button", { className: "btn primary sm", onClick: () => onWatch(f) }, /* @__PURE__ */ React.createElement(Icon, { name: "eye", className: "icon" }), " Watch")))))), /* @__PURE__ */ React.createElement("div", { className: "discover-toolbar" }, /* @__PURE__ */ React.createElement("div", { className: "discover-cats" }, DISCOVER_CATS.map((c) => /* @__PURE__ */ React.createElement(
    "button",
    {
      key: c.id,
      className: activeCat === c.id ? "on" : "",
      onClick: () => setActiveCat(c.id)
    },
    c.label,
    " ",
    /* @__PURE__ */ React.createElement("span", { className: "ct" }, c.ct)
  ))), /* @__PURE__ */ React.createElement("div", { style: { marginLeft: "auto", position: "relative" } }, /* @__PURE__ */ React.createElement(Icon, { name: "search", size: 12, style: { position: "absolute", left: 10, top: 9, color: "var(--fg-3)" } }), /* @__PURE__ */ React.createElement(
    "input",
    {
      className: "input",
      style: { width: 240, paddingLeft: 30 },
      placeholder: "search public databases\u2026",
      value: q,
      onChange: (e) => setQ(e.target.value)
    }
  ))), /* @__PURE__ */ React.createElement("div", { className: "discover-list" }, filtered.map((p) => /* @__PURE__ */ React.createElement("div", { key: p.name, className: "public-row" }, /* @__PURE__ */ React.createElement("div", { className: "public-name-col" }, /* @__PURE__ */ React.createElement("div", { className: "public-name" }, /* @__PURE__ */ React.createElement(Icon, { name: "database", size: 12, color: "var(--amber-400)" }), p.name), /* @__PURE__ */ React.createElement("div", { className: "public-addr" }, /* @__PURE__ */ React.createElement("span", null, p.addr), /* @__PURE__ */ React.createElement(
    "button",
    {
      className: "addr-copy",
      title: "Copy address",
      onClick: (e) => {
        e.stopPropagation();
        try {
          navigator.clipboard.writeText(p.addr);
        } catch {
        }
      }
    },
    /* @__PURE__ */ React.createElement(Icon, { name: "copy", size: 12 })
  ))), /* @__PURE__ */ React.createElement("div", { className: "public-desc" }, p.desc), /* @__PURE__ */ React.createElement("div", { className: "public-meta" }, /* @__PURE__ */ React.createElement("div", null, /* @__PURE__ */ React.createElement("span", { className: "lbl" }, "KEYS"), /* @__PURE__ */ React.createElement("strong", null, p.keys))), /* @__PURE__ */ React.createElement("div", null, /* @__PURE__ */ React.createElement("button", { className: "btn secondary sm", onClick: () => onWatch(p) }, /* @__PURE__ */ React.createElement(Icon, { name: "eye", className: "icon" }), " Watch")))), filtered.length === 0 && /* @__PURE__ */ React.createElement("div", { className: "empty", style: { padding: "48px 24px" } }, /* @__PURE__ */ React.createElement("div", { className: "glyph" }, /* @__PURE__ */ React.createElement(Icon, { name: "search-x", size: 28 })), /* @__PURE__ */ React.createElement("div", null, "No databases matching ", /* @__PURE__ */ React.createElement("code", null, '"', q, '"'), activeCat !== "all" && ` in ${activeCat}`, "."))), /* @__PURE__ */ React.createElement("div", { className: "discover-foot" }, "Listing is opt-in. Publishers register at ", /* @__PURE__ */ React.createElement("code", null, "https://discover.zkv.cash"), ", signed by the database's UFVK."));
};
const TIME_ZONES = (() => {
  try {
    return Intl.supportedValuesOf("timeZone");
  } catch (_) {
    return [
      "America/Los_Angeles",
      "America/Denver",
      "America/Chicago",
      "America/New_York",
      "America/Sao_Paulo",
      "Europe/London",
      "Europe/Berlin",
      "Europe/Moscow",
      "Asia/Kolkata",
      "Asia/Shanghai",
      "Asia/Tokyo",
      "Australia/Sydney"
    ];
  }
})();
const ForgetModal = ({ name, onClose, onConfirm }) => {
  const [typed, setTyped] = React.useState("");
  const [busy, setBusy] = React.useState(false);
  const [error, setError] = React.useState(null);
  const ready = typed.trim() === "FORGET";
  const [detail, setDetail] = React.useState(null);
  React.useEffect(() => {
    let alive = true;
    window.zkvApi.detail(name).then((d) => {
      if (alive) setDetail(d);
    }).catch(() => {
    });
    return () => {
      alive = false;
    };
  }, [name]);
  const address = detail && detail.address;
  const isAdmin = !!(detail && detail.role === "admin");
  const copyText = (s) => {
    try {
      navigator.clipboard.writeText(s);
    } catch {
    }
  };
  const doForget = async () => {
    if (!ready || busy) return;
    setBusy(true);
    setError(null);
    try {
      await onConfirm(name);
      onClose();
    } catch (e) {
      setError(e);
      setBusy(false);
    }
  };
  return /* @__PURE__ */ React.createElement("div", { className: "modal-overlay", onClick: (e) => {
    if (e.target.classList.contains("modal-overlay") && !busy) onClose();
  } }, /* @__PURE__ */ React.createElement("div", { className: "modal", role: "dialog", "aria-labelledby": "forget-title", style: { maxWidth: 460 } }, /* @__PURE__ */ React.createElement("div", { className: "modal-head" }, /* @__PURE__ */ React.createElement("div", null, /* @__PURE__ */ React.createElement("div", { className: "eyebrow" }, "DB: ", name), /* @__PURE__ */ React.createElement("h2", { id: "forget-title" }, "Forget database")), !busy && /* @__PURE__ */ React.createElement("button", { className: "close", onClick: onClose, title: "Close" }, /* @__PURE__ */ React.createElement(Icon, { name: "x", size: 16 }))), /* @__PURE__ */ React.createElement("div", { className: "modal-body" }, /* @__PURE__ */ React.createElement("div", { style: { fontSize: 13, color: "var(--fg-2)", lineHeight: 1.55, marginBottom: 14 } }, "This deletes your local cache of data stored on the Zcash blockchain. The data will remain visible to anyone who has the database's", " ", /* @__PURE__ */ React.createElement("code", { style: { fontFamily: "var(--font-mono)", fontSize: 12 } }, "zkv1"), " address. Confirm by typing ", /* @__PURE__ */ React.createElement("strong", { style: { color: "var(--fg-1)" } }, "FORGET"), " below:"), address && /* @__PURE__ */ React.createElement("div", { style: { marginBottom: 14, padding: "10px 12px", background: "var(--bg-sunken)", borderRadius: "var(--radius-md)", border: "1px solid var(--border-1)" } }, /* @__PURE__ */ React.createElement("div", { style: { fontFamily: "var(--font-mono)", fontSize: 10, letterSpacing: "0.08em", textTransform: "uppercase", color: "var(--fg-3)", marginBottom: 6 } }, "Back up this address first"), /* @__PURE__ */ React.createElement(CollapsibleString, { value: address, onCopy: copyText })), isAdmin && /* @__PURE__ */ React.createElement("div", { className: "callout-flow warn", style: { marginBottom: 14 } }, /* @__PURE__ */ React.createElement(Icon, { name: "alert-triangle", size: 16, color: "var(--amber-400)" }), /* @__PURE__ */ React.createElement("div", null, "If you didn't back up this database's ", /* @__PURE__ */ React.createElement("strong", { style: { color: "inherit" } }, "seed phrase"), " when you created it, forgetting it here means you may never be able to write to it again. Save the", " ", /* @__PURE__ */ React.createElement("code", { style: { fontFamily: "var(--font-mono)", fontSize: 12 } }, "zkv1"), " address above so you can at least keep reading it.")), /* @__PURE__ */ React.createElement(
    "input",
    {
      className: "input mono lg",
      value: typed,
      onChange: (e) => setTyped(e.target.value),
      placeholder: "FORGET",
      autoFocus: true,
      disabled: busy,
      spellCheck: false,
      autoCapitalize: "off",
      autoCorrect: "off",
      onKeyDown: (e) => {
        if (e.key === "Enter") doForget();
      }
    }
  ), error && /* @__PURE__ */ React.createElement("div", { style: { marginTop: 12 } }, /* @__PURE__ */ React.createElement(ErrorMessage, { message: error && error.message || "could not forget database" }))), /* @__PURE__ */ React.createElement("div", { className: "modal-foot" }, /* @__PURE__ */ React.createElement("div", { className: "cost" }, /* @__PURE__ */ React.createElement(Icon, { name: "alert-triangle", size: 11, color: "var(--red-500)" }), /* @__PURE__ */ React.createElement("span", { style: { color: "var(--red-500)" } }, "On-chain data is unaffected")), /* @__PURE__ */ React.createElement("div", { style: { display: "flex", gap: 8 } }, /* @__PURE__ */ React.createElement("button", { className: "btn secondary", onClick: onClose, disabled: busy }, "Cancel"), /* @__PURE__ */ React.createElement("button", { className: "btn danger", disabled: !ready || busy, onClick: doForget }, busy ? "Forgetting\u2026" : "Forget")))));
};
const RevealPhraseModal = ({ name, onClose }) => {
  const [phrase, setPhrase] = React.useState(null);
  const [error, setError] = React.useState(null);
  const [loading, setLoading] = React.useState(true);
  const [copied, setCopied] = React.useState(false);
  React.useEffect(() => {
    let live = true;
    window.zkvApi.revealPhrase(name).then((r) => {
      if (live) setPhrase(r.phrase);
    }).catch((e) => {
      if (live) setError(e);
    }).finally(() => {
      if (live) setLoading(false);
    });
    return () => {
      live = false;
    };
  }, [name]);
  const words = phrase ? phrase.trim().split(/\s+/) : [];
  return /* @__PURE__ */ React.createElement("div", { className: "modal-overlay", onClick: (e) => {
    if (e.target.classList.contains("modal-overlay")) onClose();
  } }, /* @__PURE__ */ React.createElement("div", { className: "modal", role: "dialog", "aria-labelledby": "reveal-title", style: { maxWidth: 560 } }, /* @__PURE__ */ React.createElement("div", { className: "modal-head" }, /* @__PURE__ */ React.createElement("div", null, /* @__PURE__ */ React.createElement("div", { className: "eyebrow" }, "DB: ", name), /* @__PURE__ */ React.createElement("h2", { id: "reveal-title" }, "Secret recovery phrase")), /* @__PURE__ */ React.createElement("button", { className: "close", onClick: onClose, title: "Close" }, /* @__PURE__ */ React.createElement(Icon, { name: "x", size: 16 }))), /* @__PURE__ */ React.createElement("div", { className: "modal-body" }, /* @__PURE__ */ React.createElement("div", { className: "form-stack" }, /* @__PURE__ */ React.createElement("div", { className: "callout-flow warn" }, /* @__PURE__ */ React.createElement(Icon, { name: "alert-triangle", size: 16, color: "var(--amber-400)" }), /* @__PURE__ */ React.createElement("div", null, /* @__PURE__ */ React.createElement("strong", { style: { color: "inherit" } }, "Anyone with your seed phrase can edit your database, impersonate you, and spend your funds."), " ", "Password-protected encrypted seed phrases are coming in a future release.")), loading && /* @__PURE__ */ React.createElement("div", { style: { display: "flex", alignItems: "center", gap: 8, color: "var(--fg-2)", fontSize: 13 } }, /* @__PURE__ */ React.createElement("div", { className: "spinner" }), " Decrypting\u2026"), error && /* @__PURE__ */ React.createElement(ErrorMessage, { message: error && error.message || "could not reveal seed phrase" }), phrase && /* @__PURE__ */ React.createElement(React.Fragment, null, /* @__PURE__ */ React.createElement("div", { className: "mnemonic-grid" }, words.map((w, i) => /* @__PURE__ */ React.createElement("div", { key: i, className: "mnemonic-cell" }, /* @__PURE__ */ React.createElement("span", { className: "idx" }, i + 1), /* @__PURE__ */ React.createElement("span", { className: "word" }, w)))), /* @__PURE__ */ React.createElement("div", { style: { display: "flex", justifyContent: "flex-end" } }, /* @__PURE__ */ React.createElement(
    "button",
    {
      className: "btn ghost sm",
      title: "Copy the 24 words (space-separated) to the clipboard",
      onClick: () => {
        try {
          navigator.clipboard.writeText(words.join(" "));
        } catch {
        }
        setCopied(true);
        setTimeout(() => setCopied(false), 1500);
      }
    },
    /* @__PURE__ */ React.createElement(Icon, { name: "copy", className: "icon" }),
    " ",
    copied ? "Copied" : "Copy phrase"
  ))))), /* @__PURE__ */ React.createElement("div", { className: "modal-foot" }, /* @__PURE__ */ React.createElement("div", { className: "cost" }, /* @__PURE__ */ React.createElement(Icon, { name: "alert-triangle", size: 11, color: "var(--red-500)" }), /* @__PURE__ */ React.createElement("span", { style: { color: "var(--red-500)" } }, "Never share these words with anyone")), /* @__PURE__ */ React.createElement("div", { style: { display: "flex", gap: 8 } }, /* @__PURE__ */ React.createElement("button", { className: "btn secondary", onClick: onClose }, "Done")))));
};
const Settings = ({
  theme,
  onTheme,
  timeZone,
  onTimeZone,
  onResetOnboarding,
  status,
  databases,
  onForget,
  onSyncWorkers,
  onViewLicenses,
  onViewShortcuts,
  onReimportDemo
}) => {
  const workers = status && status.sync_workers || 5;
  const version = status && status.version || "";
  const platform = status && status.platform;
  const dbNames = (databases || []).map((d) => d.name);
  const [forgetPick, setForgetPick] = React.useState("");
  const [forgetTarget, setForgetTarget] = React.useState(null);
  const forgetSel = forgetPick && dbNames.includes(forgetPick) ? forgetPick : dbNames[0] || "";
  const adminDbNames = (databases || []).filter((d) => d.role === "admin").map((d) => d.name);
  const [revealPick, setRevealPick] = React.useState("");
  const [revealTarget, setRevealTarget] = React.useState(null);
  const revealSel = revealPick && adminDbNames.includes(revealPick) ? revealPick : adminDbNames[0] || "";
  const [servers, setServers] = React.useState(null);
  const [probing, setProbing] = React.useState(false);
  const loadServers = React.useCallback(() => {
    setProbing(true);
    window.zkvApi.servers().then((s) => setServers(s)).catch(() => {
    }).finally(() => setProbing(false));
  }, []);
  React.useEffect(() => {
    loadServers();
  }, [loadServers]);
  const serverRow = (label, row) => /* @__PURE__ */ React.createElement("div", { className: "settings-row" }, /* @__PURE__ */ React.createElement("div", { className: "srl" }, label), /* @__PURE__ */ React.createElement("div", { className: "srv", style: { gridColumn: "2 / 4" } }, !row ? probing ? "probing\u2026" : "\u2014" : !row.online ? /* @__PURE__ */ React.createElement("span", { style: { color: "var(--fg-3)" } }, row.server, " \xB7 offline") : /* @__PURE__ */ React.createElement(React.Fragment, null, /* @__PURE__ */ React.createElement("span", null, row.server), /* @__PURE__ */ React.createElement("span", { style: { opacity: 0.5 } }, " \xB7 "), /* @__PURE__ */ React.createElement("span", null, "blk ", row.block_height != null ? row.block_height.toLocaleString() : "\u2014"), /* @__PURE__ */ React.createElement("span", { style: { opacity: 0.5 } }, " \xB7 "), /* @__PURE__ */ React.createElement("span", null, row.backend || "\u2014"))));
  return /* @__PURE__ */ React.createElement("div", { className: "settings" }, /* @__PURE__ */ React.createElement("div", { style: { marginBottom: 32 } }, /* @__PURE__ */ React.createElement("div", { style: { fontFamily: "IBM Plex Mono", fontSize: 11, letterSpacing: "0.12em", textTransform: "uppercase", color: "var(--fg-3)" } }, "WORKSPACE"), /* @__PURE__ */ React.createElement("h2", { style: { fontFamily: "IBM Plex Sans", fontWeight: 600, fontSize: 26, letterSpacing: "-0.01em", margin: "4px 0 0", color: "var(--fg-1)" } }, "Settings")), /* @__PURE__ */ React.createElement("div", { className: "settings-section" }, /* @__PURE__ */ React.createElement("h3", null, "Appearance"), /* @__PURE__ */ React.createElement("p", { className: "lede" }, "Theme and how timestamps are displayed."), /* @__PURE__ */ React.createElement("div", { className: "settings-card" }, /* @__PURE__ */ React.createElement("div", { className: "settings-row" }, /* @__PURE__ */ React.createElement("div", { className: "srl" }, "Time zone"), /* @__PURE__ */ React.createElement("div", { className: "srv" }), /* @__PURE__ */ React.createElement("div", { className: "ctl" }, /* @__PURE__ */ React.createElement(
    "select",
    {
      className: "input",
      value: timeZone,
      onChange: (e) => onTimeZone(e.target.value),
      style: { minWidth: 220, fontFamily: "var(--font-mono)", fontSize: 12 }
    },
    /* @__PURE__ */ React.createElement("option", { value: "UTC" }, "UTC"),
    /* @__PURE__ */ React.createElement("option", { value: "local" }, "Local (system)"),
    /* @__PURE__ */ React.createElement("optgroup", { label: "Regions" }, TIME_ZONES.filter((z) => z !== "UTC").map((z) => /* @__PURE__ */ React.createElement("option", { key: z, value: z }, z)))
  ))), /* @__PURE__ */ React.createElement("div", { className: "settings-row" }, /* @__PURE__ */ React.createElement("div", { className: "srl" }, "Theme"), /* @__PURE__ */ React.createElement("div", { className: "srv" }), /* @__PURE__ */ React.createElement("div", { className: "ctl" }, /* @__PURE__ */ React.createElement("div", { className: "seg" }, /* @__PURE__ */ React.createElement("button", { className: theme === "light" ? "on" : "", onClick: () => onTheme("light") }, "Light"), /* @__PURE__ */ React.createElement("button", { className: theme === "dark" ? "on" : "", onClick: () => onTheme("dark") }, "Dark")))))), /* @__PURE__ */ React.createElement("div", { className: "settings-section" }, /* @__PURE__ */ React.createElement("h3", null, "Network"), /* @__PURE__ */ React.createElement("p", { className: "lede" }, "The lightwalletd connection is chosen at launch, e.g.", " ", /* @__PURE__ */ React.createElement("code", { style: { fontFamily: "var(--font-mono)", fontSize: 11 } }, "zkv gui --mainnet-server 1.2.3"), " ", "(and ", /* @__PURE__ */ React.createElement("code", { style: { fontFamily: "var(--font-mono)", fontSize: 11 } }, "--testnet-server"), ")."), /* @__PURE__ */ React.createElement("div", { className: "settings-card" }, serverRow("Mainnet server", servers && servers.mainnet), serverRow("Testnet server", servers && servers.testnet), /* @__PURE__ */ React.createElement("div", { className: "settings-row" }, /* @__PURE__ */ React.createElement("div", { className: "srl" }, "Data directory"), /* @__PURE__ */ React.createElement("div", { className: "srv" }, servers && servers.data_dir || "\u2026"), /* @__PURE__ */ React.createElement("div", { className: "ctl" }, /* @__PURE__ */ React.createElement(
    "button",
    {
      className: "btn secondary sm",
      onClick: () => {
        window.zkvApi.openDataDir().catch(() => {
        });
      },
      title: "Open the data directory in your file manager"
    },
    /* @__PURE__ */ React.createElement(Icon, { name: "folder-open", className: "icon" }),
    " Open"
  ))), /* @__PURE__ */ React.createElement("div", { className: "settings-row" }, /* @__PURE__ */ React.createElement("div", { className: "srl" }, "Sync workers", /* @__PURE__ */ React.createElement("span", { className: "sub" }, "How many databases sync in parallel in the background.")), /* @__PURE__ */ React.createElement("div", { className: "srv" }, workers), /* @__PURE__ */ React.createElement("div", { className: "ctl" }, /* @__PURE__ */ React.createElement(
    "select",
    {
      className: "input",
      value: workers,
      onChange: (e) => onSyncWorkers && onSyncWorkers(parseInt(e.target.value, 10)),
      style: { minWidth: 80, fontFamily: "var(--font-mono)", fontSize: 12 }
    },
    [1, 2, 3, 4, 5, 6, 8, 12, 16].map((n) => /* @__PURE__ */ React.createElement("option", { key: n, value: n }, n))
  ))))), /* @__PURE__ */ React.createElement("div", { className: "settings-section" }, /* @__PURE__ */ React.createElement("h3", null, "About"), /* @__PURE__ */ React.createElement("div", { className: "settings-card" }, /* @__PURE__ */ React.createElement("div", { className: "settings-row" }, /* @__PURE__ */ React.createElement("div", { className: "srl" }, "Version"), /* @__PURE__ */ React.createElement("div", { className: "srv", style: { gridColumn: "2 / 4" } }, "zkv v", version, platform ? ` (${platform})` : "")), /* @__PURE__ */ React.createElement("div", { className: "settings-row" }, /* @__PURE__ */ React.createElement("div", { className: "srl" }, "Source"), /* @__PURE__ */ React.createElement("div", { className: "srv", style: { gridColumn: "2 / 4" } }, "github.com/zecrocks/zkv"))), /* @__PURE__ */ React.createElement("div", { style: { marginTop: 16, display: "flex", gap: 10, flexWrap: "wrap" } }, /* @__PURE__ */ React.createElement("button", { className: "btn secondary", onClick: onViewLicenses }, /* @__PURE__ */ React.createElement(Icon, { name: "scale", className: "icon" }), " View Licenses"), /* @__PURE__ */ React.createElement("button", { className: "btn secondary", onClick: onViewShortcuts }, /* @__PURE__ */ React.createElement(Icon, { name: "keyboard", className: "icon" }), " View Keyboard Shortcuts"), /* @__PURE__ */ React.createElement("button", { className: "btn secondary", onClick: onResetOnboarding }, /* @__PURE__ */ React.createElement(Icon, { name: "rotate-ccw", className: "icon" }), " Replay onboarding"), status && status.demo_reimport_available && /* @__PURE__ */ React.createElement("button", { className: "btn secondary", onClick: onReimportDemo }, /* @__PURE__ */ React.createElement(Icon, { name: "download-cloud", className: "icon" }), " Re-import Oracle Demo"))), /* @__PURE__ */ React.createElement("div", { className: "settings-section" }, /* @__PURE__ */ React.createElement("h3", null, "Danger Zone"), /* @__PURE__ */ React.createElement("p", { className: "lede" }, "Irreversible, local-only actions."), /* @__PURE__ */ React.createElement("div", { className: "settings-card danger-zone" }, /* @__PURE__ */ React.createElement("div", { className: "settings-row" }, /* @__PURE__ */ React.createElement("div", { className: "srl" }, "Show seed phrase", /* @__PURE__ */ React.createElement("span", { className: "sub" }, "Reveal an admin database's 24-word recovery phrase. Anyone with it can write to the database and spend its funds.")), /* @__PURE__ */ React.createElement("div", { className: "srv" }), /* @__PURE__ */ React.createElement("div", { className: "ctl" }, /* @__PURE__ */ React.createElement(
    "select",
    {
      className: "input",
      value: revealSel,
      onChange: (e) => setRevealPick(e.target.value),
      disabled: adminDbNames.length === 0,
      style: { minWidth: 180, fontFamily: "var(--font-mono)", fontSize: 12 }
    },
    adminDbNames.length === 0 ? /* @__PURE__ */ React.createElement("option", { value: "" }, "No admin databases") : adminDbNames.map((n) => /* @__PURE__ */ React.createElement("option", { key: n, value: n }, n))
  ), /* @__PURE__ */ React.createElement("button", { className: "btn danger", disabled: !revealSel, onClick: () => setRevealTarget(revealSel) }, /* @__PURE__ */ React.createElement(Icon, { name: "eye", className: "icon" }), " Reveal"))), /* @__PURE__ */ React.createElement("div", { className: "settings-row" }, /* @__PURE__ */ React.createElement("div", { className: "srl" }, "Forget database", /* @__PURE__ */ React.createElement("span", { className: "sub" }, "Delete a database's local cache. The on-chain data stays readable by anyone holding its zkv1 address.")), /* @__PURE__ */ React.createElement("div", { className: "srv" }), /* @__PURE__ */ React.createElement("div", { className: "ctl" }, /* @__PURE__ */ React.createElement(
    "select",
    {
      className: "input",
      value: forgetSel,
      onChange: (e) => setForgetPick(e.target.value),
      disabled: dbNames.length === 0,
      style: { minWidth: 180, fontFamily: "var(--font-mono)", fontSize: 12 }
    },
    dbNames.length === 0 ? /* @__PURE__ */ React.createElement("option", { value: "" }, "No databases") : dbNames.map((n) => /* @__PURE__ */ React.createElement("option", { key: n, value: n }, n))
  ), /* @__PURE__ */ React.createElement("button", { className: "btn danger", disabled: !forgetSel, onClick: () => setForgetTarget(forgetSel) }, /* @__PURE__ */ React.createElement(Icon, { name: "trash-2", className: "icon" }), " Forget"))))), forgetTarget && /* @__PURE__ */ React.createElement(
    ForgetModal,
    {
      name: forgetTarget,
      onClose: () => setForgetTarget(null),
      onConfirm: onForget
    }
  ), revealTarget && /* @__PURE__ */ React.createElement(
    RevealPhraseModal,
    {
      name: revealTarget,
      onClose: () => setRevealTarget(null)
    }
  ));
};
const LICENSES_CLI_CMD = "zkv --licenses";
const Licenses = ({ onBack }) => {
  const [status, setStatus] = React.useState(null);
  const [error, setError] = React.useState(null);
  const [saving, setSaving] = React.useState(false);
  const [copied, setCopied] = React.useState(false);
  const onSave = async () => {
    setError(null);
    setStatus(null);
    setSaving(true);
    try {
      const r = await window.zkvApi.saveLicenses();
      if (r && r.saved) setStatus(r.path ? `Saved to ${r.path}` : "Saved.");
    } catch (e) {
      setError(e.message || "failed to save licenses");
    } finally {
      setSaving(false);
    }
  };
  const onCopyCmd = () => {
    try {
      navigator.clipboard.writeText(LICENSES_CLI_CMD);
    } catch {
    }
    setCopied(true);
    setTimeout(() => setCopied(false), 1500);
  };
  return /* @__PURE__ */ React.createElement("div", { className: "settings" }, /* @__PURE__ */ React.createElement("div", { style: { marginBottom: 24, display: "flex", alignItems: "flex-end", gap: 16 } }, /* @__PURE__ */ React.createElement("div", { style: { flex: "1 1 auto" } }, /* @__PURE__ */ React.createElement("div", { style: { fontFamily: "IBM Plex Mono", fontSize: 11, letterSpacing: "0.12em", textTransform: "uppercase", color: "var(--fg-3)" } }, "WORKSPACE"), /* @__PURE__ */ React.createElement("h2", { style: { fontFamily: "IBM Plex Sans", fontWeight: 600, fontSize: 26, letterSpacing: "-0.01em", margin: "4px 0 0", color: "var(--fg-1)" } }, "Licenses")), /* @__PURE__ */ React.createElement("button", { className: "btn secondary sm", onClick: onBack }, /* @__PURE__ */ React.createElement(Icon, { name: "arrow-left", className: "icon" }), " Back to Settings")), /* @__PURE__ */ React.createElement("p", { className: "lede" }, "Third-party software bundled with or linked into zkv and the zkv Browser, with the license texts their authors ship. Generated from the build's resolved dependency graph. Save the full bundle to a file, or dump it from the command line with ", /* @__PURE__ */ React.createElement("code", { style: { fontFamily: "var(--font-mono)", fontSize: 12 } }, LICENSES_CLI_CMD), "."), /* @__PURE__ */ React.createElement("div", { className: "settings-card", style: { padding: 18, display: "flex", gap: 12, alignItems: "center", flexWrap: "wrap" } }, /* @__PURE__ */ React.createElement("button", { className: "btn", onClick: onSave, disabled: saving }, /* @__PURE__ */ React.createElement(Icon, { name: "download", className: "icon" }), " ", saving ? "Saving\u2026" : "Save to file"), /* @__PURE__ */ React.createElement("div", { style: { display: "flex", alignItems: "center", gap: 8, marginLeft: "auto" } }, /* @__PURE__ */ React.createElement("code", { style: { fontFamily: "var(--font-mono)", fontSize: 13, color: "var(--fg-2)" } }, LICENSES_CLI_CMD), /* @__PURE__ */ React.createElement("button", { className: "btn secondary sm", onClick: onCopyCmd, title: "Copy the CLI command" }, /* @__PURE__ */ React.createElement(Icon, { name: "copy", className: "icon" }), " ", copied ? "Copied" : "Copy"))), status && /* @__PURE__ */ React.createElement("div", { style: { marginTop: 12, color: "var(--fg-3)", fontSize: 13 } }, status), error && /* @__PURE__ */ React.createElement("div", { style: { marginTop: 12, color: "var(--fg-3)", fontSize: 13 } }, "Couldn't save: ", error));
};
const Kbd = ({ children }) => /* @__PURE__ */ React.createElement("kbd", { className: "kbd-inline" }, children);
const KeyCombo = ({ keys, alt }) => /* @__PURE__ */ React.createElement("span", { className: "shortcuts-keys" }, keys.map((k, i) => /* @__PURE__ */ React.createElement(React.Fragment, { key: i }, i > 0 && alt && /* @__PURE__ */ React.createElement("span", { className: "kbd-sep" }, "/"), /* @__PURE__ */ React.createElement(Kbd, null, k))));
const KeyboardShortcuts = ({ onBack }) => {
  const groups = [
    {
      title: "Global",
      items: [
        { keys: [MOD_KEY, "K"], desc: "Open the Find palette" },
        { keys: ["j"], desc: "Select the next database in the sidebar" },
        { keys: ["k"], desc: "Select the previous database in the sidebar" }
      ]
    },
    {
      title: "Find palette",
      items: [
        { keys: ["\u2191", "\u2193"], alt: true, desc: "Move through the results" },
        { keys: ["Enter"], desc: "Run the highlighted command" },
        { keys: ["Esc"], desc: "Close the palette" }
      ]
    },
    {
      title: "Open database",
      items: [
        { keys: ["\u2191", "\u2193"], alt: true, desc: "Navigate rows in the table" },
        { keys: ["\u2190", "\u2192"], alt: true, desc: "Switch tab (Browse, History, Roles\u2026)" }
      ]
    }
  ];
  return /* @__PURE__ */ React.createElement("div", { className: "settings" }, /* @__PURE__ */ React.createElement("div", { style: { marginBottom: 24, display: "flex", alignItems: "flex-end", gap: 16 } }, /* @__PURE__ */ React.createElement("div", { style: { flex: "1 1 auto" } }, /* @__PURE__ */ React.createElement("div", { style: { fontFamily: "IBM Plex Mono", fontSize: 11, letterSpacing: "0.12em", textTransform: "uppercase", color: "var(--fg-3)" } }, "WORKSPACE"), /* @__PURE__ */ React.createElement("h2", { style: { fontFamily: "IBM Plex Sans", fontWeight: 600, fontSize: 26, letterSpacing: "-0.01em", margin: "4px 0 0", color: "var(--fg-1)" } }, "Keyboard Shortcuts")), /* @__PURE__ */ React.createElement("button", { className: "btn secondary sm", onClick: onBack }, /* @__PURE__ */ React.createElement(Icon, { name: "arrow-left", className: "icon" }), " Back to Settings")), groups.map((g) => /* @__PURE__ */ React.createElement("div", { className: "settings-section", key: g.title }, /* @__PURE__ */ React.createElement("h3", null, g.title), /* @__PURE__ */ React.createElement("div", { className: "settings-card" }, g.items.map((it, i) => /* @__PURE__ */ React.createElement("div", { className: "shortcuts-row", key: i }, /* @__PURE__ */ React.createElement("span", { className: "desc" }, it.desc), /* @__PURE__ */ React.createElement(KeyCombo, { keys: it.keys, alt: it.alt })))))));
};
const DEMO_SEED_WORDS = [
  "wisdom",
  "fabric",
  "essence",
  "bottom",
  "luxury",
  "tower",
  "arrest",
  "quick",
  "ozone",
  "raccoon",
  "defy",
  "spy",
  "orbit",
  "canyon",
  "velvet",
  "harvest",
  "meadow",
  "silent",
  "copper",
  "ginger",
  "puzzle",
  "nimble",
  "cargo",
  "pioneer",
  "gravity",
  "lantern",
  "marble",
  "pelican",
  "quartz",
  "ribbon",
  "sunset",
  "timber",
  "umbrella",
  "voyage",
  "walnut",
  "anchor",
  "breeze",
  "cactus",
  "dolphin",
  "ember",
  "falcon",
  "glacier",
  "hollow",
  "island",
  "jungle",
  "kettle",
  "ladder",
  "mango",
  "nectar",
  "pebble",
  "rocket",
  "saddle",
  "thunder",
  "velour",
  "willow",
  "zebra",
  "almond",
  "bishop",
  "cobalt",
  "dynamo",
  "flicker",
  "gadget",
  "hazel",
  "ivory"
];
const Onboarding = ({ onChoose, onSkip, version }) => {
  const [terminalLine, setTerminalLine] = React.useState(0);
  const seed = React.useMemo(() => {
    const pick = [];
    for (let i = 0; i < 12; i++) {
      pick.push(DEMO_SEED_WORDS[Math.floor(Math.random() * DEMO_SEED_WORDS.length)]);
    }
    return pick;
  }, []);
  const lines = [
    { type: "prompt", text: "$ ", cmd: "zkv init" },
    { type: "out", text: 'Creating mainnet zkv database "default"' },
    { type: "out", text: "" },
    { type: "dim", text: "Recovery phrase, write these 24 words down NOW." },
    { type: "out", text: "" },
    { type: "out", text: "  " + seed.slice(0, 6).join(" ") },
    { type: "out", text: "  " + seed.slice(6, 12).join(" ") + " \u2026" },
    { type: "out", text: "" },
    { type: "ok", text: "\u2713 Confirmed." },
    { type: "ok", text: '\u2713 Created database "default" (mainnet, birthday 3338983)' },
    { type: "out", text: "" },
    { type: "prompt", text: "$ ", cmd: "zkv set zec_usd_price 1008.33" },
    { type: "out", text: "zkv SET zec_usd_price \u2192 zkv1qz9p\u2026k2voracle" },
    { type: "ok", text: "\u2713 broadcast tx 96feedf9\u2026584166214" },
    { type: "out", text: "" },
    { type: "prompt", text: "$ ", cmd: "zkv get zec_usd_price" },
    { type: "out", text: "zec_usd_price = 1008.33" }
  ];
  React.useEffect(() => {
    if (terminalLine >= lines.length) return;
    const cur = lines[terminalLine];
    const delay = cur.cmd ? 700 : cur.text === "" ? 120 : 220;
    const t = setTimeout(() => setTerminalLine((l) => l + 1), delay);
    return () => clearTimeout(t);
  }, [terminalLine]);
  return /* @__PURE__ */ React.createElement("div", { className: "onboard-overlay" }, /* @__PURE__ */ React.createElement("div", { className: "onboard-bar" }, /* @__PURE__ */ React.createElement("div", { className: "topbar-logo" }, /* @__PURE__ */ React.createElement("img", { src: "design-system/assets/logo-mark-dark.svg", width: "22", height: "22", alt: "zkv" }), /* @__PURE__ */ React.createElement("span", { className: "wordmark" }, /* @__PURE__ */ React.createElement("span", null, "z"), /* @__PURE__ */ React.createElement("span", { className: "colon" }, ":"), /* @__PURE__ */ React.createElement("span", null, "kv"))), /* @__PURE__ */ React.createElement("span", { className: "topbar-divider" }), /* @__PURE__ */ React.createElement("span", { style: { fontFamily: "var(--font-mono)", fontSize: 12, color: "var(--fg-3)" } }, "welcome"), /* @__PURE__ */ React.createElement("div", { style: { marginLeft: "auto" } }, /* @__PURE__ */ React.createElement("button", { className: "btn ghost sm", onClick: onSkip }, "Skip, I'll figure it out ", /* @__PURE__ */ React.createElement(Icon, { name: "x", className: "icon" })))), /* @__PURE__ */ React.createElement("div", { className: "onboard-body" }, /* @__PURE__ */ React.createElement("div", { className: "onboard-left" }, /* @__PURE__ */ React.createElement("h1", { className: "onboard-title" }, "A key value database on Zcash."), /* @__PURE__ */ React.createElement("p", { className: "onboard-lede" }, "z:kv stores key-value pairs as signed memos on Zcash. Anyone with your ", /* @__PURE__ */ React.createElement("code", null, "zkv1"), " address can read, only the authorized seed-holders can write. Use it for feature flags, price oracles, or any data needing decentralization."), /* @__PURE__ */ React.createElement("div", { className: "onboard-paths" }, /* @__PURE__ */ React.createElement("button", { className: "onboard-path", onClick: () => onChoose("create") }, /* @__PURE__ */ React.createElement("div", { className: "pic" }, /* @__PURE__ */ React.createElement(Icon, { name: "plus", size: 18 })), /* @__PURE__ */ React.createElement("div", null, /* @__PURE__ */ React.createElement("div", { className: "pname" }, "Create your first database")), /* @__PURE__ */ React.createElement("div", { className: "parrow" }, /* @__PURE__ */ React.createElement(Icon, { name: "arrow-right", size: 16 }))), /* @__PURE__ */ React.createElement("button", { className: "onboard-path", onClick: () => onChoose("demo") }, /* @__PURE__ */ React.createElement("div", { className: "pic" }, /* @__PURE__ */ React.createElement(Icon, { name: "eye", size: 18 })), /* @__PURE__ */ React.createElement("div", null, /* @__PURE__ */ React.createElement("div", { className: "pname" }, "Watch a Zcash price oracle")), /* @__PURE__ */ React.createElement("div", { className: "parrow" }, /* @__PURE__ */ React.createElement(Icon, { name: "arrow-right", size: 16 }))), /* @__PURE__ */ React.createElement("button", { className: "onboard-path", onClick: () => onChoose("reference") }, /* @__PURE__ */ React.createElement("div", { className: "pic" }, /* @__PURE__ */ React.createElement(Icon, { name: "book-open", size: 18 })), /* @__PURE__ */ React.createElement("div", null, /* @__PURE__ */ React.createElement("div", { className: "pname" }, "Explore the z:kv learning reference")), /* @__PURE__ */ React.createElement("div", { className: "parrow" }, /* @__PURE__ */ React.createElement(Icon, { name: "arrow-right", size: 16 }))))), /* @__PURE__ */ React.createElement("div", { className: "onboard-right" }, /* @__PURE__ */ React.createElement("div", { className: "terminal" }, /* @__PURE__ */ React.createElement("div", { className: "terminal-bar" }, /* @__PURE__ */ React.createElement("span", { className: "tl-dot" }), /* @__PURE__ */ React.createElement("span", { className: "tl-dot" }), /* @__PURE__ */ React.createElement("span", { className: "tl-dot" }), /* @__PURE__ */ React.createElement("span", { className: "tl-title" }, "emersonian@local \xB7 ~/projects/zcash-oracle")), /* @__PURE__ */ React.createElement("div", { className: "terminal-body" }, lines.slice(0, terminalLine).map((l, i) => {
    if (l.cmd) {
      return /* @__PURE__ */ React.createElement("div", { key: i }, /* @__PURE__ */ React.createElement("span", { className: "prompt" }, l.text), /* @__PURE__ */ React.createElement("span", { className: "cmd" }, l.cmd));
    }
    if (l.type === "dim") return /* @__PURE__ */ React.createElement("div", { key: i, className: "dim" }, l.text);
    if (l.type === "ok") return /* @__PURE__ */ React.createElement("div", { key: i, className: "ok" }, l.text);
    return /* @__PURE__ */ React.createElement("div", { key: i, className: l.type === "out" ? "out" : "" }, l.text || "\xA0");
  }), terminalLine < lines.length && /* @__PURE__ */ React.createElement("span", { className: "cursor" }))))), /* @__PURE__ */ React.createElement("div", { className: "onboard-foot" }, /* @__PURE__ */ React.createElement("span", null, version ? `zkv v${version}` : "zkv", " \xB7 alpha \xB7 for testing, not ready for production use"), /* @__PURE__ */ React.createElement("span", null, "github.com/zecrocks/zkv")));
};
const CommandPalette = ({ open, onClose, onGo, databases }) => {
  const [q, setQ] = React.useState("");
  const [idx, setIdx] = React.useState(0);
  React.useEffect(() => {
    if (open) {
      setQ("");
      setIdx(0);
    }
  }, [open]);
  const dbCommands = (databases || []).map((d) => ({
    group: "Databases",
    icon: d.role === "admin" ? "database" : "eye",
    name: `${d.name} \xB7 ${d.role}`,
    shortcut: "",
    action: () => onGo("keys:" + d.name)
  }));
  const opcodeCommands = (window.ZKV_OPCODES || []).map((o) => ({
    group: "Reference",
    icon: "book-open",
    name: `${o.name} opcode`,
    shortcut: "",
    action: () => onGo("ref:" + o.id)
  }));
  const commands = [
    // Databases first so the user's own DBs sit above generic navigation.
    ...dbCommands,
    { group: "Navigate", icon: "layout-dashboard", name: "Go to Dashboard", shortcut: "\u2318D", action: () => onGo("dashboard") },
    // Discover temporarily disabled.
    { group: "Navigate", icon: "settings", name: "Open Settings", shortcut: "\u2318,", action: () => onGo("settings") },
    { group: "Navigate", icon: "keyboard", name: "View keyboard shortcuts", shortcut: "", action: () => onGo("shortcuts") },
    { group: "Actions", icon: "plus", name: "Create database", shortcut: "", action: () => onGo("create") },
    { group: "Actions", icon: "download", name: "Import / watch database", shortcut: "", action: () => onGo("import") },
    { group: "Actions", icon: "send", name: "Set key in current database\u2026", shortcut: "", action: () => onGo("write") },
    { group: "Actions", icon: "refresh-cw", name: "Force sync now", shortcut: "\u2318R", action: () => onGo("sync") },
    { group: "Actions", icon: "sun", name: "Toggle theme", shortcut: "\u2318T", action: () => onGo("theme") },
    // Opcode reference jumps last, in their own group.
    ...opcodeCommands
  ];
  const filtered = q ? commands.filter((c) => c.name.toLowerCase().includes(q.toLowerCase())) : commands;
  React.useEffect(() => {
    setIdx(0);
  }, [q]);
  const groups = filtered.reduce((acc, c) => {
    (acc[c.group] = acc[c.group] || []).push(c);
    return acc;
  }, {});
  if (!open) return null;
  const onKey = (e) => {
    if (e.key === "Enter") {
      e.preventDefault();
      if (filtered[idx]) {
        filtered[idx].action();
        onClose();
      }
    }
    if (e.key === "ArrowDown") {
      e.preventDefault();
      setIdx((i) => Math.min(i + 1, filtered.length - 1));
    }
    if (e.key === "ArrowUp") {
      e.preventDefault();
      setIdx((i) => Math.max(i - 1, 0));
    }
  };
  return /* @__PURE__ */ React.createElement("div", { className: "cmd-overlay", onClick: onClose }, /* @__PURE__ */ React.createElement("div", { className: "cmd-panel", onClick: (e) => e.stopPropagation() }, /* @__PURE__ */ React.createElement(
    "input",
    {
      className: "cmd-input",
      autoFocus: true,
      placeholder: "Search commands, databases, keys\u2026",
      value: q,
      onChange: (e) => setQ(e.target.value),
      onKeyDown: onKey
    }
  ), /* @__PURE__ */ React.createElement("div", { className: "cmd-list" }, Object.entries(groups).map(([g, items]) => /* @__PURE__ */ React.createElement(React.Fragment, { key: g }, /* @__PURE__ */ React.createElement("div", { className: "cmd-group-h" }, g), items.map((c, i) => {
    const absIdx = filtered.indexOf(c);
    return /* @__PURE__ */ React.createElement(
      "div",
      {
        key: c.name,
        className: "cmd-item" + (absIdx === idx ? " active" : ""),
        onMouseEnter: () => setIdx(absIdx),
        onClick: () => {
          c.action();
          onClose();
        }
      },
      /* @__PURE__ */ React.createElement(Icon, { name: c.icon, size: 14, color: "var(--fg-3)" }),
      /* @__PURE__ */ React.createElement("span", null, c.name)
    );
  }))), filtered.length === 0 && /* @__PURE__ */ React.createElement("div", { className: "cmd-item", style: { color: "var(--fg-3)" } }, /* @__PURE__ */ React.createElement(Icon, { name: "search-x", size: 14 }), ' No results for "', q, '"'))));
};
window.Discover = Discover;
window.Settings = Settings;
window.Licenses = Licenses;
window.KeyboardShortcuts = KeyboardShortcuts;
window.Onboarding = Onboarding;
window.CommandPalette = CommandPalette;
