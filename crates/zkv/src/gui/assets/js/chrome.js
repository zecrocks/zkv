const middleTruncate = (s, max) => {
  if (s.length <= max) return s;
  if (max <= 1) return max === 1 ? "\u2026" : "";
  const keep = max - 1;
  const head = Math.ceil(keep * 2 / 3);
  const tail = keep - head;
  return s.slice(0, head) + "\u2026" + (tail > 0 ? s.slice(-tail) : "");
};
let _charCanvas = null;
const monoCharWidth = (el) => {
  const cs = getComputedStyle(el);
  _charCanvas = _charCanvas || document.createElement("canvas");
  const ctx = _charCanvas.getContext("2d");
  ctx.font = `${cs.fontWeight} ${cs.fontSize} ${cs.fontFamily}`;
  return ctx.measureText("0").width || 7;
};
const DbAddr = ({ address }) => {
  const ref = React.useRef(null);
  const [text, setText] = React.useState(address);
  React.useLayoutEffect(() => {
    const el = ref.current;
    if (!el) return;
    const fit = () => {
      const max = Math.floor(el.clientWidth / monoCharWidth(el));
      setText(max > 0 ? middleTruncate(address, max) : "");
    };
    fit();
    const ro = new ResizeObserver(fit);
    ro.observe(el);
    return () => ro.disconnect();
  }, [address]);
  return /* @__PURE__ */ React.createElement("span", { className: "db-addr", ref, title: address }, text);
};
const currencyFor = (network) => network === "testnet" ? "TAZ" : "ZEC";
window.currencyFor = currencyFor;
const formatZats = (zats, network) => {
  if (zats == null) return "\u2014";
  const zec = (Number(zats) / 1e8).toFixed(8).replace(/0+$/, "").replace(/\.$/, ".0");
  return `${zec} ${currencyFor(network)}`;
};
window.formatZats = formatZats;
const fmtAgo = (ts) => {
  if (ts == null) return null;
  const diff = Math.floor(Date.now() / 1e3) - ts;
  if (diff < 10) return "just now";
  if (diff < 60) return diff + " seconds ago";
  const m = Math.floor(diff / 60);
  if (m < 60) return m === 1 ? "1 minute ago" : m + " minutes ago";
  const h = Math.floor(diff / 3600);
  if (h < 24) return h === 1 ? "1 hour ago" : h + " hours ago";
  const d = Math.floor(diff / 86400);
  if (d < 30) return d === 1 ? "1 day ago" : d + " days ago";
  const mo = Math.floor(diff / 2592e3);
  if (mo < 12) return mo === 1 ? "1 month ago" : mo + " months ago";
  const y = Math.floor(diff / 31536e3);
  return y === 1 ? "1 year ago" : y + " years ago";
};
const IS_MAC = (() => {
  try {
    const nav = navigator;
    const p = nav.userAgentData && nav.userAgentData.platform || nav.platform || "";
    return /mac/i.test(p);
  } catch {
    return false;
  }
})();
window.IS_MAC = IS_MAC;
const MOD_KEY = IS_MAC ? "\u2318" : "Ctrl";
window.MOD_KEY = MOD_KEY;
const Topbar = ({ db, view, onCmd, onCopy, onDeposit, onSend, syncing, syncedBlock, networkLatency }) => {
  const showDb = view === "keys" && db;
  return /* @__PURE__ */ React.createElement("div", { className: "topbar" + (showDb ? " has-db" : "") }, /* @__PURE__ */ React.createElement("div", { className: "topbar-logo" }, /* @__PURE__ */ React.createElement("img", { src: "design-system/assets/logo-mark-dark.svg", width: "22", height: "22", alt: "zkv" }), /* @__PURE__ */ React.createElement("span", { className: "wordmark" }, /* @__PURE__ */ React.createElement("span", null, "z"), /* @__PURE__ */ React.createElement("span", { className: "colon" }, ":"), /* @__PURE__ */ React.createElement("span", null, "kv"))), showDb && /* @__PURE__ */ React.createElement(React.Fragment, null, /* @__PURE__ */ React.createElement("span", { className: "topbar-divider" }), /* @__PURE__ */ React.createElement("div", { className: "db-identity", title: db.address }, /* @__PURE__ */ React.createElement(
    Icon,
    {
      name: db.role === "admin" ? "database" : "eye",
      size: 14,
      className: "db-icon",
      color: db.role === "admin" ? "currentColor" : "var(--fg-3)"
    }
  ), /* @__PURE__ */ React.createElement("span", { className: "db-name" }, db.name), /* @__PURE__ */ React.createElement("span", { className: "db-role", "data-role": db.role }, db.role), db.pool && /* @__PURE__ */ React.createElement("span", { className: "db-pool" }, db.pool), /* @__PURE__ */ React.createElement(DbAddr, { address: db.address }), /* @__PURE__ */ React.createElement(
    "button",
    {
      className: "btn ghost sm",
      title: "Copy address",
      onClick: () => onCopy && onCopy(db.address)
    },
    /* @__PURE__ */ React.createElement(Icon, { name: "copy", className: "icon" })
  ), db.role === "admin" && onDeposit && /* @__PURE__ */ React.createElement("button", { className: "btn secondary sm", title: "Deposit funds", onClick: onDeposit }, /* @__PURE__ */ React.createElement(Icon, { name: "qr-code", className: "icon" }), " Deposit"), db.role === "admin" && onSend && /* @__PURE__ */ React.createElement("button", { className: "btn secondary sm", title: "Send ZEC to any address", onClick: onSend }, /* @__PURE__ */ React.createElement(Icon, { name: "send", className: "icon" }), " Send"))), !showDb && view !== "create" && view !== "import" && /* @__PURE__ */ React.createElement(React.Fragment, null, /* @__PURE__ */ React.createElement("span", { className: "topbar-divider" }), /* @__PURE__ */ React.createElement("span", { className: "view-label" }, view === "dashboard" && /* @__PURE__ */ React.createElement(React.Fragment, null, /* @__PURE__ */ React.createElement(Icon, { name: "layout-dashboard", size: 13 }), " Dashboard"), view === "settings" && /* @__PURE__ */ React.createElement(React.Fragment, null, /* @__PURE__ */ React.createElement(Icon, { name: "settings", size: 13 }), " Settings"))), /* @__PURE__ */ React.createElement("div", { className: "topbar-spacer" }), /* @__PURE__ */ React.createElement("div", { className: "cmd-trigger", onClick: onCmd }, /* @__PURE__ */ React.createElement(Icon, { name: "search", size: 13 }), /* @__PURE__ */ React.createElement("span", null, "Find\u2026"), /* @__PURE__ */ React.createElement("kbd", null, IS_MAC ? "\u2318K" : "Ctrl+K")));
};
const Sidebar = ({ view, onSelectView, databases, sidebarDbs, truncated, totalCount, onViewAll, activeName, onSelect, onCreate, onImport }) => {
  const dbItem = (d) => /* @__PURE__ */ React.createElement(
    "div",
    {
      key: d.name,
      className: "sidebar-item" + (view === "keys" && d.name === activeName ? " active" : ""),
      onClick: () => onSelect(d.name)
    },
    /* @__PURE__ */ React.createElement(
      Icon,
      {
        name: d.role === "admin" ? "database" : "eye",
        size: 14,
        color: d.role === "admin" ? void 0 : "var(--fg-3)"
      }
    ),
    /* @__PURE__ */ React.createElement("span", null, d.name),
    d.detailed && d.unsynced > 0 && /* @__PURE__ */ React.createElement("span", { className: "unread-dot", title: `${d.unsynced} pending` }),
    d.network === "testnet" && /* @__PURE__ */ React.createElement("span", { className: "net-tag", title: "Testnet database" }, "T"),
    d.paused && /* @__PURE__ */ React.createElement(PauseGlyph, { size: 11, style: { color: "var(--fg-3)" }, title: "Auto-sync paused" }),
    /* @__PURE__ */ React.createElement("span", { className: "meta" }, d.detailed ? d.keys : /* @__PURE__ */ React.createElement("span", { style: { color: "var(--fg-3)" } }, "\xB7"))
  );
  return /* @__PURE__ */ React.createElement("aside", { className: "sidebar" }, /* @__PURE__ */ React.createElement("div", { className: "sidebar-actions" }, /* @__PURE__ */ React.createElement("button", { className: "btn secondary sm", onClick: onCreate }, /* @__PURE__ */ React.createElement(Icon, { name: "plus", className: "icon" }), " Create"), /* @__PURE__ */ React.createElement("button", { className: "btn secondary sm", onClick: onImport }, /* @__PURE__ */ React.createElement(Icon, { name: "download", className: "icon" }), " Import")), /* @__PURE__ */ React.createElement("div", { className: "sidebar-section" }, /* @__PURE__ */ React.createElement("div", { className: "sidebar-heading" }, /* @__PURE__ */ React.createElement("span", null, "Workspace")), /* @__PURE__ */ React.createElement(
    "div",
    {
      className: "sidebar-item" + (view === "dashboard" ? " active" : ""),
      onClick: () => onSelectView("dashboard")
    },
    /* @__PURE__ */ React.createElement(Icon, { name: "layout-dashboard", size: 14 }),
    /* @__PURE__ */ React.createElement("span", null, "Dashboard")
  )), truncated ? (
    // Edge case: too many databases to list. Show the most recently
    // updated and route the rest through the search palette.
    /* @__PURE__ */ React.createElement("div", { className: "sidebar-section" }, /* @__PURE__ */ React.createElement("div", { className: "sidebar-heading" }, /* @__PURE__ */ React.createElement("span", null, "Databases \xB7 recently updated")), (sidebarDbs || []).map(dbItem), /* @__PURE__ */ React.createElement("div", { className: "sidebar-item", onClick: onViewAll, style: { color: "var(--fg-3)" }, title: "Search all databases" }, /* @__PURE__ */ React.createElement(Icon, { name: "search", size: 14, color: "var(--fg-3)" }), /* @__PURE__ */ React.createElement("span", null, "View all databases (", totalCount, ")")))
  ) : /* @__PURE__ */ React.createElement(React.Fragment, null, databases.some((d) => d.role === "admin") && /* @__PURE__ */ React.createElement("div", { className: "sidebar-section" }, /* @__PURE__ */ React.createElement("div", { className: "sidebar-heading" }, /* @__PURE__ */ React.createElement("span", null, "Writable")), databases.filter((d) => d.role === "admin").map(dbItem)), databases.some((d) => d.role === "watch") && /* @__PURE__ */ React.createElement("div", { className: "sidebar-section" }, /* @__PURE__ */ React.createElement("div", { className: "sidebar-heading" }, /* @__PURE__ */ React.createElement("span", null, "Viewing")), databases.filter((d) => d.role === "watch").map(dbItem))), /* @__PURE__ */ React.createElement("div", { className: "sidebar-section" }, /* @__PURE__ */ React.createElement("div", { className: "sidebar-heading" }, /* @__PURE__ */ React.createElement("span", null, "Local")), /* @__PURE__ */ React.createElement(
    "div",
    {
      className: "sidebar-item" + (view === "reference" ? " active" : ""),
      onClick: () => onSelectView("reference")
    },
    /* @__PURE__ */ React.createElement(Icon, { name: "book-open", size: 14, color: "var(--fg-3)" }),
    /* @__PURE__ */ React.createElement("span", null, "Reference")
  ), /* @__PURE__ */ React.createElement(
    "div",
    {
      className: "sidebar-item" + (view === "settings" ? " active" : ""),
      onClick: () => onSelectView("settings")
    },
    /* @__PURE__ */ React.createElement(Icon, { name: "settings", size: 14, color: "var(--fg-3)" }),
    /* @__PURE__ */ React.createElement("span", null, "Settings")
  )));
};
const StatusBar = ({ db, synced, isSynced, latency, syncing, networkBlock, network, server, version, gitSha, onDeposit, pausedAll, onTogglePauseAll }) => {
  const [showSha, setShowSha] = React.useState(false);
  return /* @__PURE__ */ React.createElement("div", { className: "statusbar" }, /* @__PURE__ */ React.createElement("div", { className: "group" }, /* @__PURE__ */ React.createElement("span", { className: "dot" + (isSynced ? "" : " amber") }), /* @__PURE__ */ React.createElement("span", null, network || "mainnet"), /* @__PURE__ */ React.createElement("span", { style: { opacity: 0.5 } }, "\xB7"), /* @__PURE__ */ React.createElement("span", null, isSynced ? "synced" : "syncing")), db && db.role === "admin" && db.balance != null && /* @__PURE__ */ React.createElement(React.Fragment, null, /* @__PURE__ */ React.createElement("span", { className: "sep" }), /* @__PURE__ */ React.createElement(
    "div",
    {
      className: "group",
      onClick: onDeposit,
      style: { cursor: onDeposit ? "pointer" : "default" },
      title: "Add funds, show deposit QR"
    },
    /* @__PURE__ */ React.createElement(Icon, { name: "coins", size: 11 }),
    /* @__PURE__ */ React.createElement("span", null, formatZats(db.balance, db.network)),
    db.confirming > 0 && /* @__PURE__ */ React.createElement(
      "span",
      {
        style: { color: "var(--amber-300)", opacity: 0.85 },
        title: "Funds still confirming, included in the balance but not yet spendable"
      },
      "(confirming: ",
      formatZats(db.confirming, db.network),
      ")"
    ),
    onDeposit && /* @__PURE__ */ React.createElement(Icon, { name: "download", size: 10 })
  )), /* @__PURE__ */ React.createElement("span", { className: "sep" }), /* @__PURE__ */ React.createElement("div", { className: "group" }, /* @__PURE__ */ React.createElement(Icon, { name: "git-branch", size: 11 }), /* @__PURE__ */ React.createElement("span", null, "blk ", synced ? synced.toLocaleString() : "\u2014"), !isSynced && synced > 0 && /* @__PURE__ */ React.createElement("span", { style: { color: "var(--amber-300)" } }, "/ ", networkBlock.toLocaleString(), " \u2191")), /* @__PURE__ */ React.createElement("span", { className: "sep" }), /* @__PURE__ */ React.createElement("div", { className: "group" }, /* @__PURE__ */ React.createElement(Icon, { name: "zap", size: 11 }), /* @__PURE__ */ React.createElement("span", null, latency != null ? latency + "ms \xB7 " : "", server || "lightwalletd")), /* @__PURE__ */ React.createElement("div", { className: "right" }, /* @__PURE__ */ React.createElement(
    "button",
    {
      className: "btn ghost sm",
      onClick: onTogglePauseAll,
      style: { color: "inherit" },
      title: pausedAll ? "Resume auto-sync" : "Pause auto-sync"
    },
    pausedAll ? /* @__PURE__ */ React.createElement(Icon, { name: "play", size: 11 }) : /* @__PURE__ */ React.createElement(PauseGlyph, { size: 11 }),
    " ",
    pausedAll ? "Resume all syncing" : "Pause all syncing"
  ), /* @__PURE__ */ React.createElement(
    "span",
    {
      onClick: () => gitSha && setShowSha((s) => !s),
      style: { cursor: gitSha ? "pointer" : "default" },
      title: gitSha ? showSha ? "Show version" : "Show build commit" : void 0
    },
    showSha && gitSha ? `git: ${gitSha.slice(0, 7)}${gitSha.endsWith("-dirty") ? "-dirty" : ""}` : version ? `zkv v${version}` : "zkv"
  )));
};
window.Topbar = Topbar;
window.Sidebar = Sidebar;
window.StatusBar = StatusBar;
