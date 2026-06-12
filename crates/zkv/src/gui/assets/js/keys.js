const fmtBytes = (n) => {
  if (n == null) return "\u2014";
  if (n < 1024) return n + " B";
  return (n / 1024).toFixed(1) + " KB";
};
const CollapsibleString = ({ value, onCopy }) => {
  const [open, setOpen] = React.useState(false);
  const long = value && value.length > 30;
  const shown = open || !long ? value : value.slice(0, 16) + "\u2026" + value.slice(-10);
  return /* @__PURE__ */ React.createElement(
    "span",
    {
      className: "collapse-str",
      style: { display: "inline-flex", alignItems: "flex-start", gap: 6, minWidth: 0, maxWidth: "100%" }
    },
    /* @__PURE__ */ React.createElement(
      "span",
      {
        onClick: () => long && setOpen((o) => !o),
        title: long ? open ? "Click to collapse" : "Click to expand" : void 0,
        style: {
          fontFamily: "var(--font-mono)",
          color: "var(--fg-1)",
          cursor: long ? "pointer" : "default",
          // Collapsed: the value is already middle-truncated to fit, so keep
          // it on a single line; otherwise it breaks at the "…" the moment a
          // scrollbar (or a shorter window) shaves a few pixels off the pane.
          wordBreak: open ? "break-all" : "normal",
          whiteSpace: open ? "normal" : "nowrap",
          lineHeight: 1.5
        }
      },
      shown,
      long && open && /* @__PURE__ */ React.createElement(Icon, { name: "chevron-up", size: 12, style: { marginLeft: 4, color: "var(--fg-3)", verticalAlign: "-2px" } })
    ),
    /* @__PURE__ */ React.createElement(
      "button",
      {
        className: "btn ghost sm",
        title: "Copy",
        onClick: (e) => {
          e.stopPropagation();
          onCopy(value);
        },
        style: { flexShrink: 0, width: 22, height: 22, padding: 0 }
      },
      /* @__PURE__ */ React.createElement(Icon, { name: "copy", className: "icon" })
    )
  );
};
const CopyableBlock = ({ text, onCopy }) => {
  const [copied, setCopied] = React.useState(false);
  const doCopy = () => {
    onCopy(text);
    setCopied(true);
    setTimeout(() => setCopied(false), 1200);
  };
  return /* @__PURE__ */ React.createElement("div", { className: "copyable-block" }, /* @__PURE__ */ React.createElement("pre", { className: "value-block" }, text), /* @__PURE__ */ React.createElement("button", { className: "copy-fab", title: "Copy", onClick: doCopy }, /* @__PURE__ */ React.createElement(Icon, { name: copied ? "check" : "copy", className: "icon" })));
};
const ErrorMessage = ({ message }) => {
  const [copied, setCopied] = React.useState(false);
  const doCopy = (e) => {
    e.stopPropagation();
    try {
      navigator.clipboard.writeText(message);
    } catch {
    }
    setCopied(true);
    setTimeout(() => setCopied(false), 1200);
  };
  return /* @__PURE__ */ React.createElement("div", { style: { marginTop: 4, display: "flex", alignItems: "flex-start", gap: 8 } }, /* @__PURE__ */ React.createElement(
    "div",
    {
      style: {
        fontSize: 12.5,
        color: "var(--fg-2)",
        flex: "1 1 auto",
        minWidth: 0,
        wordBreak: "break-word"
      }
    },
    message
  ), /* @__PURE__ */ React.createElement(
    "button",
    {
      className: "btn ghost sm",
      title: copied ? "Copied" : "Copy error",
      onClick: doCopy,
      style: { flexShrink: 0, width: 22, height: 22, padding: 0 }
    },
    /* @__PURE__ */ React.createElement(Icon, { name: copied ? "check" : "copy", className: "icon" })
  ));
};
const fmtWhen = (ts, tz, withZone = true) => {
  const opts = {
    year: "numeric",
    month: "short",
    day: "numeric",
    hour: "numeric",
    minute: "2-digit"
  };
  const zone = tz || "UTC";
  if (zone !== "local") opts.timeZone = zone;
  if (withZone) opts.timeZoneName = "short";
  return new Date(ts * 1e3).toLocaleString(void 0, opts);
};
const truncPub = (pk) => pk && pk.length > 18 ? pk.slice(0, 10) + "\u2026" + pk.slice(-6) : pk || "";
const midTrunc = (s) => s && s.length > 26 ? s.slice(0, 16) + "\u2026" + s.slice(-8) : s || "";
const whenAt = (ts, height, tz) => ts != null ? fmtWhen(ts, tz) : height != null ? "#" + height : "\u2014";
const roleOf = (rows, pk) => {
  if (!pk || !rows) return null;
  const r = rows.find((x) => x.pubkey === pk);
  return r ? r.role : null;
};
const SignerLink = ({ pubkey, role, onOpenRole }) => {
  if (!pubkey) return /* @__PURE__ */ React.createElement("span", { style: { color: "var(--fg-3)" } }, "\u2014");
  return /* @__PURE__ */ React.createElement(
    "button",
    {
      type: "button",
      className: "signer-link",
      title: (role ? role + " \xB7 " : "") + pubkey + "\nView in Roles",
      onClick: () => onOpenRole && onOpenRole(pubkey)
    },
    role && /* @__PURE__ */ React.createElement("span", { className: "role " + role }, role),
    /* @__PURE__ */ React.createElement("span", { className: "signer-pub" }, truncPub(pubkey))
  );
};
const yamlScalar = (v) => {
  if (v == null) return "null";
  if (typeof v === "number") return String(v);
  if (typeof v === "boolean") return v ? "true" : "false";
  const s = String(v);
  if (s === "") return '""';
  if (s.indexOf("\n") >= 0) {
    return "|-\n" + s.split("\n").map((l) => "    " + l).join("\n");
  }
  if (/^[A-Za-z0-9_][\w./@+-]*$/.test(s) && !/^(true|false|null|yes|no|on|off|~)$/i.test(s) && !/^[+-]?\d[\d_]*(\.\d*)?([eE][+-]?\d+)?$/.test(s)) {
    return s;
  }
  return '"' + s.replace(/\\/g, "\\\\").replace(/"/g, '\\"') + '"';
};
const toYaml = (pairs) => pairs.filter((p) => p[1] !== void 0).map((p) => p[0] + ": " + yamlScalar(p[1])).join("\n");
const historyYaml = (e, signer, network, tz) => {
  const s = e.status || {};
  const statusLabel = s.kind === "confirmed" ? "confirmed" : s.kind === "confirming" ? "confirming (" + s.done + "/" + s.required + ")" : "pending";
  const hasValue = e.value != null && e.op !== "DEL" && e.op !== "INIT";
  return toYaml([
    ["operation", e.op],
    ["key", e.op === "INIT" ? void 0 : e.key],
    ["address", e.op === "INIT" ? e.key : void 0],
    ["value", hasValue ? e.value : void 0],
    ["timestamp", e.timestamp ? fmtWhen(e.timestamp, tz) : null],
    ["height", e.height != null ? e.height : "mempool"],
    ["status", statusLabel],
    ["txid", e.txid || null],
    ["output_index", e.output_index],
    [
      "output_value",
      e.output_value != null ? window.formatZats(e.output_value, network) : "0 " + window.currencyFor(network)
    ],
    ["fee", e.fee != null ? window.formatZats(e.fee, network) : null],
    ["sequence", e.seq != null ? e.seq : null],
    ["signer", signer || null],
    ["signature", e.signature || null]
  ]) + "\n";
};
const keyYaml = (row, db, signer, tz) => {
  const s = row.status || {};
  const statusLabel = s.kind === "confirming" ? "confirming (" + s.done + "/" + s.required + ")" : s.kind;
  return toYaml([
    ["key", row.key],
    ["value", row.value != null ? row.value : null],
    ["status", statusLabel],
    ["deleted", row.deleted ? true : void 0],
    ["size_in_bytes", row.size != null ? row.size : void 0],
    ["database", db && db.address],
    ["last_updated", row.updated_at ? fmtWhen(row.updated_at, tz) : null],
    ["txid", row.txid || null],
    ["updated_by", signer || null]
  ]) + "\n";
};
const histWhen = (e, tz) => {
  const mono = { fontFamily: "var(--font-mono)", fontSize: 11.5, color: "var(--fg-3)", whiteSpace: "nowrap" };
  if (e.timestamp) return /* @__PURE__ */ React.createElement("span", { style: mono }, fmtWhen(e.timestamp, tz));
  if (e.height != null) return /* @__PURE__ */ React.createElement("span", { style: mono }, "#", e.height);
  return /* @__PURE__ */ React.createElement("span", { className: "tag-amber" }, /* @__PURE__ */ React.createElement("span", { className: "dot", style: { background: "var(--amber-400)" } }), " pending");
};
const histSig = (v) => v === true ? /* @__PURE__ */ React.createElement(Icon, { name: "shield-check", size: 14, color: "var(--green-500)" }) : v === false ? /* @__PURE__ */ React.createElement(Icon, { name: "shield-alert", size: 14, color: "var(--red-500)" }) : /* @__PURE__ */ React.createElement(Icon, { name: "clock", size: 14, color: "var(--amber-400)" });
const statusChip = (s) => {
  const k = s && s.kind || "confirmed";
  if (k === "confirmed") {
    return /* @__PURE__ */ React.createElement("span", { style: { display: "inline-flex", alignItems: "center", gap: 5, color: "var(--green-500)" } }, /* @__PURE__ */ React.createElement(Icon, { name: "shield-check", size: 12 }), " confirmed");
  }
  const label = k === "confirming" ? `confirming \xB7 ${s.done}/${s.required}` : k === "deleting" ? "deleting" : "pending";
  return /* @__PURE__ */ React.createElement("span", { style: { display: "inline-flex", alignItems: "center", gap: 5, color: "var(--amber-500)" } }, /* @__PURE__ */ React.createElement("span", { className: "dot", style: { background: "var(--amber-400)" } }), " ", label);
};
const lastUpdateCell = (r, tz) => {
  const s = r.status || {};
  const chip = (label) => /* @__PURE__ */ React.createElement("span", { className: "tag-amber" }, /* @__PURE__ */ React.createElement("span", { className: "dot", style: { background: "var(--amber-400)" } }), " ", label);
  if (s.kind === "confirming") return chip(`now (confirming ${s.done}/${s.required})`);
  if (s.kind === "pending") return chip("now (pending)");
  if (s.kind === "deleting") return chip("now (deleting)");
  return r.updated_at ? /* @__PURE__ */ React.createElement(
    "span",
    {
      title: fmtWhen(r.updated_at, tz),
      style: { fontSize: 12, color: "var(--fg-3)", whiteSpace: "nowrap" }
    },
    fmtAgo(r.updated_at)
  ) : /* @__PURE__ */ React.createElement("span", { style: { color: "var(--fg-3)" } }, "\u2014");
};
const roleScopeCell = (r) => {
  if (r.role === "owner") {
    return /* @__PURE__ */ React.createElement("span", { style: { color: "var(--fg-3)", fontStyle: "italic" } }, "full authority");
  }
  const caps = r.capabilities || [];
  if (caps.length === 0) return /* @__PURE__ */ React.createElement("span", { style: { color: "var(--fg-3)" } }, "\u2014");
  return /* @__PURE__ */ React.createElement("span", { style: { display: "inline-flex", gap: 4, flexWrap: "wrap" } }, caps.map((c) => /* @__PURE__ */ React.createElement("span", { key: c, className: "cap-chip" }, c)));
};
const revokedScopeCell = (r, tz) => {
  const when = r.timestamp != null ? fmtWhen(r.timestamp, tz) : r.height != null ? "#" + r.height : "unknown time";
  const caps = r.role === "writer" ? r.capabilities || [] : [];
  return /* @__PURE__ */ React.createElement("span", { style: { display: "inline-flex", gap: 6, flexWrap: "wrap", alignItems: "center" } }, caps.map((c) => /* @__PURE__ */ React.createElement("span", { key: c, className: "cap-chip", style: { opacity: 0.5, textDecoration: "line-through" } }, c)), /* @__PURE__ */ React.createElement("span", { style: { fontFamily: "var(--font-mono)", fontSize: 11, color: "var(--fg-3)", whiteSpace: "nowrap" } }, "revoked ", when));
};
const fundingAmount = (t, network) => {
  const received = t.direction === "received";
  return /* @__PURE__ */ React.createElement(
    "span",
    {
      style: {
        fontFamily: "var(--font-mono)",
        fontSize: 12,
        fontWeight: 500,
        color: received ? "var(--green-500)" : "var(--red-500)",
        whiteSpace: "nowrap"
      }
    },
    received ? "+" : "\u2212",
    window.formatZats(t.amount, network)
  );
};
const FirstSyncPanel = ({ detail, chainTip }) => {
  const bday = detail && detail.birthday || 0;
  const tip = chainTip || 0;
  const synced = detail && detail.synced || 0;
  const cur = tip > 0 ? Math.min(tip, Math.max(synced, bday)) : synced;
  const span = tip - bday;
  const pct = span > 0 ? Math.max(0, Math.min(100, Math.round((cur - bday) / span * 100))) : 0;
  return /* @__PURE__ */ React.createElement("div", { className: "dt-wrap" }, /* @__PURE__ */ React.createElement("div", { className: "empty", style: { padding: "64px 24px" } }, /* @__PURE__ */ React.createElement("div", null, /* @__PURE__ */ React.createElement("div", { className: "glyph", style: { marginBottom: 4 } }, /* @__PURE__ */ React.createElement(Icon, { name: "loader", size: 28 })), /* @__PURE__ */ React.createElement("strong", { style: { color: "var(--fg-1)" } }, tip > 0 ? `Syncing ${cur.toLocaleString()} / ${tip.toLocaleString()} (${pct}%)\u2026` : "Starting sync\u2026"), /* @__PURE__ */ React.createElement("div", { style: { marginTop: 6, color: "var(--fg-3)", fontSize: 13, maxWidth: 360, marginLeft: "auto", marginRight: "auto" } }, "Scanning the chain from this database's birthday to the tip to determine its state."), tip > 0 && span > 0 && /* @__PURE__ */ React.createElement(
    "div",
    {
      style: {
        marginTop: 16,
        width: 280,
        maxWidth: "100%",
        height: 6,
        background: "var(--bg-sunken)",
        border: "1px solid var(--border-1)",
        borderRadius: 4,
        overflow: "hidden",
        marginLeft: "auto",
        marginRight: "auto"
      }
    },
    /* @__PURE__ */ React.createElement(
      "div",
      {
        style: {
          width: pct + "%",
          height: "100%",
          background: "var(--amber-400)",
          transition: "width 0.4s ease"
        }
      }
    )
  ))));
};
const KeyList = ({
  db,
  detail,
  rows,
  selectedIdx,
  onSelect,
  filter,
  onFilter,
  onWriteKey,
  onDelete,
  paused,
  onTogglePause,
  onManualSync,
  manualSyncing,
  chainTip,
  firstSyncPending,
  tab,
  onTab,
  loading,
  history,
  historyLoading,
  selectedHistoryIdx,
  onSelectHistory,
  historyTotal,
  historyOffset,
  historyPageSize,
  onHistoryPage,
  rejections,
  rejectionsLoading,
  selectedRejectionIdx,
  onSelectRejection,
  roles,
  rolesRevoked,
  rolesCreator,
  rolesLoading,
  onCopy,
  funding,
  fundingLoading,
  selectedFundingIdx,
  onSelectFunding,
  fundingTotal,
  fundingOffset,
  fundingPageSize,
  onFundingPage,
  timeZone,
  selectedRoleIdx,
  onSelectRole
}) => {
  const isAdmin = db && db.role === "admin";
  const [menuIdx, setMenuIdx] = React.useState(-1);
  React.useEffect(() => {
    if (menuIdx < 0) return;
    const close = () => setMenuIdx(-1);
    window.addEventListener("click", close);
    return () => window.removeEventListener("click", close);
  }, [menuIdx]);
  return /* @__PURE__ */ React.createElement("div", { className: "main" }, /* @__PURE__ */ React.createElement("div", { className: "main-toolbar" }, tab !== "roles" && /* @__PURE__ */ React.createElement("div", { className: "kv-filter", style: { position: "relative" } }, /* @__PURE__ */ React.createElement(Icon, { name: "search", size: 12, style: { position: "absolute", left: 9, top: 8, color: "var(--fg-3)" } }), /* @__PURE__ */ React.createElement(
    "input",
    {
      className: "input",
      style: { width: 240, maxWidth: "100%", paddingLeft: 28 },
      placeholder: "filter by key\u2026",
      value: filter,
      onChange: (e) => onFilter(e.target.value)
    }
  ), filter && /* @__PURE__ */ React.createElement(
    "button",
    {
      className: "btn ghost sm",
      style: { position: "absolute", right: 2, top: 2, height: 24, width: 24, padding: 0 },
      onClick: () => onFilter("")
    },
    /* @__PURE__ */ React.createElement(Icon, { name: "x", size: 12 })
  )), /* @__PURE__ */ React.createElement("span", { style: { marginLeft: "auto" } }), /* @__PURE__ */ React.createElement(
    "button",
    {
      className: "btn secondary sm",
      onClick: onTogglePause,
      title: paused ? "Resume continuous syncing for this database" : "Pause continuous syncing for this database"
    },
    paused ? /* @__PURE__ */ React.createElement(Icon, { name: "play", className: "icon" }) : /* @__PURE__ */ React.createElement(PauseGlyph, { className: "icon" }),
    " ",
    paused ? "Resume" : /* @__PURE__ */ React.createElement("span", { className: "btn-label" }, /* @__PURE__ */ React.createElement("span", { className: "lbl-full" }, "Pause Syncing"), /* @__PURE__ */ React.createElement("span", { className: "lbl-short" }, "Pause"))
  ), paused && /* @__PURE__ */ React.createElement(
    "button",
    {
      className: "btn secondary sm",
      onClick: onManualSync,
      disabled: manualSyncing,
      title: "Sync this database once now"
    },
    manualSyncing ? /* @__PURE__ */ React.createElement("div", { className: "spinner" }) : /* @__PURE__ */ React.createElement(Icon, { name: "refresh-cw", className: "icon" }),
    " ",
    "Sync"
  ), isAdmin && !firstSyncPending && (detail && detail.init === "uninitialized" ? /* @__PURE__ */ React.createElement("button", { className: "btn primary sm", onClick: () => onWriteKey(null), title: "Broadcast INIT to open this database for writes" }, /* @__PURE__ */ React.createElement(Icon, { name: "zap", className: "icon" }), " Initialize") : detail && detail.init === "initializing" ? /* @__PURE__ */ React.createElement("button", { className: "btn primary sm", disabled: true, title: "INIT is confirming" }, /* @__PURE__ */ React.createElement("div", { className: "spinner" }), " Initializing\u2026") : /* @__PURE__ */ React.createElement("button", { className: "btn primary sm", onClick: () => onWriteKey(null) }, /* @__PURE__ */ React.createElement(Icon, { name: "plus", className: "icon" }), " Set key"))), /* @__PURE__ */ React.createElement("div", { className: "kv-tabs" }, /* @__PURE__ */ React.createElement("button", { className: tab === "browse" ? "on" : "", onClick: () => onTab("browse") }, "Browse"), /* @__PURE__ */ React.createElement("button", { className: tab === "history" ? "on" : "", onClick: () => onTab("history") }, "History"), /* @__PURE__ */ React.createElement("button", { className: tab === "roles" ? "on" : "", onClick: () => onTab("roles") }, "Roles"), isAdmin && /* @__PURE__ */ React.createElement("button", { className: tab === "funding" ? "on" : "", onClick: () => onTab("funding") }, "Funding"), /* @__PURE__ */ React.createElement("button", { className: tab === "rejections" ? "on" : "", onClick: () => onTab("rejections") }, "Rejections")), firstSyncPending && /* @__PURE__ */ React.createElement(FirstSyncPanel, { detail, chainTip }), !firstSyncPending && tab === "browse" && /* @__PURE__ */ React.createElement("div", { className: "dt-wrap" }, /* @__PURE__ */ React.createElement("table", { className: "dt" }, /* @__PURE__ */ React.createElement("thead", null, /* @__PURE__ */ React.createElement("tr", null, /* @__PURE__ */ React.createElement("th", { style: { width: "30%" } }, "Key"), /* @__PURE__ */ React.createElement("th", null, "Value"), /* @__PURE__ */ React.createElement("th", { style: { width: 150 } }, "Updated"), /* @__PURE__ */ React.createElement("th", { style: { width: 34 } }))), /* @__PURE__ */ React.createElement("tbody", null, rows.map((r, i) => /* @__PURE__ */ React.createElement("tr", { key: r.key, className: i === selectedIdx ? "selected" : "", onClick: () => {
    onSelect(i);
    setMenuIdx(-1);
  } }, /* @__PURE__ */ React.createElement("td", null, /* @__PURE__ */ React.createElement("span", { className: "key" }, r.key)), /* @__PURE__ */ React.createElement(
    "td",
    {
      style: {
        color: r.deleted ? "var(--fg-3)" : "var(--fg-1)",
        whiteSpace: "nowrap",
        overflow: "hidden",
        textOverflow: "ellipsis",
        // width:100% + max-width:0 makes this the column that
        // absorbs the table's leftover width and truncates;
        // without the width the slack inflates the fixed columns.
        width: "100%",
        maxWidth: 0,
        fontStyle: r.deleted ? "italic" : "normal"
      }
    },
    r.deleted ? "(deleting)" : r.value
  ), /* @__PURE__ */ React.createElement("td", null, lastUpdateCell(r, timeZone)), /* @__PURE__ */ React.createElement("td", { style: { textAlign: "right", position: "relative" } }, isAdmin ? /* @__PURE__ */ React.createElement(React.Fragment, null, /* @__PURE__ */ React.createElement(
    "button",
    {
      className: "btn ghost sm",
      title: "Actions",
      style: { width: 24, height: 24, padding: 0 },
      onClick: (e) => {
        e.stopPropagation();
        setMenuIdx((m) => m === i ? -1 : i);
      }
    },
    /* @__PURE__ */ React.createElement(Icon, { name: "more-horizontal", size: 14 })
  ), menuIdx === i && /* @__PURE__ */ React.createElement("div", { className: "row-menu", onClick: (e) => e.stopPropagation() }, /* @__PURE__ */ React.createElement("button", { onClick: () => {
    setMenuIdx(-1);
    onWriteKey(r);
  } }, /* @__PURE__ */ React.createElement(Icon, { name: "edit-3", size: 14 }), " Set new value"), /* @__PURE__ */ React.createElement("button", { className: "danger", onClick: () => {
    setMenuIdx(-1);
    onDelete(r);
  } }, /* @__PURE__ */ React.createElement(Icon, { name: "trash-2", size: 14 }), " Delete key"))) : /* @__PURE__ */ React.createElement(Icon, { name: "more-horizontal", size: 13, color: "var(--fg-3)" })))), rows.length === 0 && (() => {
    const init = detail && detail.init;
    const remaining = Math.max(0, (detail?.init_required || 0) - (detail?.init_done || 0));
    const glyph = loading ? "loader" : init === "uninitialized" ? "zap" : init === "initializing" ? "loader" : "inbox";
    return /* @__PURE__ */ React.createElement("tr", { className: "empty-row" }, /* @__PURE__ */ React.createElement("td", { colSpan: 4 }, /* @__PURE__ */ React.createElement("div", { className: "empty", style: { padding: "48px 24px" } }, /* @__PURE__ */ React.createElement("div", { className: "glyph" }, /* @__PURE__ */ React.createElement(Icon, { name: glyph, size: 28 })), loading ? /* @__PURE__ */ React.createElement("div", null, "Loading keys\u2026") : filter ? /* @__PURE__ */ React.createElement("div", null, "No keys matching", " ", /* @__PURE__ */ React.createElement("code", { style: { fontFamily: "var(--font-mono)", color: "var(--amber-400)" } }, '"', filter, '"'), " in this database.") : init === "uninitialized" ? /* @__PURE__ */ React.createElement("div", null, /* @__PURE__ */ React.createElement("strong", { style: { color: "var(--fg-1)" } }, "Database not initialized."), isAdmin && /* @__PURE__ */ React.createElement("div", { style: { marginTop: 14 } }, /* @__PURE__ */ React.createElement("button", { className: "btn primary sm", onClick: () => onWriteKey(null) }, /* @__PURE__ */ React.createElement(Icon, { name: "zap", className: "icon" }), " Initialize database"))) : init === "initializing" ? /* @__PURE__ */ React.createElement("div", null, /* @__PURE__ */ React.createElement("strong", { style: { color: "var(--fg-1)" } }, "Database initializing\u2026"), /* @__PURE__ */ React.createElement("div", { style: { marginTop: 6, color: "var(--fg-3)", fontSize: 13, maxWidth: 320 } }, remaining, " confirmation", remaining === 1 ? "" : "s", " remaining. You can write keys once the INIT confirms.")) : /* @__PURE__ */ React.createElement("div", null, "No keys yet.", isAdmin && /* @__PURE__ */ React.createElement("div", { style: { marginTop: 14 } }, /* @__PURE__ */ React.createElement("button", { className: "btn primary sm", onClick: () => onWriteKey(null) }, /* @__PURE__ */ React.createElement(Icon, { name: "plus", className: "icon" }), " Write your first key"))))));
  })()))), !firstSyncPending && tab === "history" && /* @__PURE__ */ React.createElement(React.Fragment, null, /* @__PURE__ */ React.createElement("div", { className: "dt-wrap" }, /* @__PURE__ */ React.createElement("table", { className: "dt" }, /* @__PURE__ */ React.createElement("thead", null, /* @__PURE__ */ React.createElement("tr", null, /* @__PURE__ */ React.createElement("th", { style: { width: 64 } }, "OP"), /* @__PURE__ */ React.createElement("th", { style: { width: "26%" } }, "Key"), /* @__PURE__ */ React.createElement("th", null, "Value"), /* @__PURE__ */ React.createElement("th", { style: { width: 200, whiteSpace: "nowrap" } }, "When"), /* @__PURE__ */ React.createElement("th", { style: { width: 48, textAlign: "center" } }, "Sig"))), /* @__PURE__ */ React.createElement("tbody", null, history.map((e, i) => /* @__PURE__ */ React.createElement(
    "tr",
    {
      key: e.txid + ":" + e.output_index + ":" + i,
      className: i === selectedHistoryIdx ? "selected" : "",
      onClick: () => onSelectHistory(i)
    },
    /* @__PURE__ */ React.createElement("td", null, /* @__PURE__ */ React.createElement("span", { className: "op " + e.op.toLowerCase() }, e.op)),
    /* @__PURE__ */ React.createElement("td", null, e.op === "INIT" ? /* @__PURE__ */ React.createElement("span", { style: { color: "var(--fg-3)", fontStyle: "italic" } }, "database created") : /* @__PURE__ */ React.createElement("span", { className: "key" }, e.key)),
    /* @__PURE__ */ React.createElement(
      "td",
      {
        style: {
          color: e.op === "DEL" ? "var(--fg-3)" : "var(--fg-1)",
          whiteSpace: "nowrap",
          overflow: "hidden",
          textOverflow: "ellipsis",
          // Absorbs leftover width + truncates (see Browse note).
          width: "100%",
          maxWidth: 0,
          fontStyle: e.op === "DEL" ? "italic" : "normal"
        }
      },
      e.op === "DEL" ? "(deleted)" : e.op === "INIT" ? "" : e.value
    ),
    /* @__PURE__ */ React.createElement("td", null, histWhen(e, timeZone)),
    /* @__PURE__ */ React.createElement("td", { style: { textAlign: "center" } }, histSig(e.verified))
  )), history.length === 0 && /* @__PURE__ */ React.createElement("tr", { className: "empty-row" }, /* @__PURE__ */ React.createElement("td", { colSpan: 5 }, /* @__PURE__ */ React.createElement("div", { className: "empty", style: { padding: "48px 24px" } }, /* @__PURE__ */ React.createElement("div", { className: "glyph" }, /* @__PURE__ */ React.createElement(Icon, { name: historyLoading ? "loader" : "history", size: 28 })), historyLoading ? /* @__PURE__ */ React.createElement("div", null, "Loading write history\u2026") : filter ? /* @__PURE__ */ React.createElement("div", null, "No writes to keys matching", " ", /* @__PURE__ */ React.createElement("code", { style: { fontFamily: "var(--font-mono)", color: "var(--amber-400)" } }, '"', filter, '"'), ".") : detail && detail.init === "uninitialized" ? /* @__PURE__ */ React.createElement("div", null, /* @__PURE__ */ React.createElement("strong", { style: { color: "var(--fg-1)" } }, "Database not initialized.")) : detail && detail.init === "initializing" ? /* @__PURE__ */ React.createElement("div", null, /* @__PURE__ */ React.createElement("strong", { style: { color: "var(--fg-1)" } }, "Database initializing\u2026"), /* @__PURE__ */ React.createElement("div", { style: { marginTop: 6, color: "var(--fg-3)", fontSize: 13, maxWidth: 340 } }, "The INIT has been broadcast and is confirming. It will appear here once the wallet sees it.")) : /* @__PURE__ */ React.createElement("div", null, "No writes yet. Every change shows up here the moment it's broadcast."))))))), /* @__PURE__ */ React.createElement(
    PaginationBar,
    {
      total: historyTotal,
      offset: historyOffset,
      pageSize: historyPageSize,
      onPage: onHistoryPage,
      loading: historyLoading
    }
  )), !firstSyncPending && tab === "rejections" && /* @__PURE__ */ React.createElement("div", { className: "dt-wrap" }, /* @__PURE__ */ React.createElement("table", { className: "dt" }, /* @__PURE__ */ React.createElement("thead", null, /* @__PURE__ */ React.createElement("tr", null, /* @__PURE__ */ React.createElement("th", { style: { width: 200, whiteSpace: "nowrap" } }, "When"), /* @__PURE__ */ React.createElement("th", { style: { width: 220 } }, "TXID"), /* @__PURE__ */ React.createElement("th", null, "Rejection Reason"))), /* @__PURE__ */ React.createElement("tbody", null, (rejections || []).map((e, i) => /* @__PURE__ */ React.createElement(
    "tr",
    {
      key: (e.txid || "") + ":" + i,
      className: i === selectedRejectionIdx ? "selected" : "",
      onClick: () => onSelectRejection(i)
    },
    /* @__PURE__ */ React.createElement("td", null, histWhen(e, timeZone)),
    /* @__PURE__ */ React.createElement("td", { style: { fontFamily: "var(--font-mono)", fontSize: 12, color: "var(--fg-2)" } }, e.txid ? e.txid.length > 26 ? `${e.txid.slice(0, 16)}\u2026${e.txid.slice(-8)}` : e.txid : /* @__PURE__ */ React.createElement("span", { style: { color: "var(--fg-3)" } }, "\u2014")),
    /* @__PURE__ */ React.createElement(
      "td",
      {
        style: {
          color: "var(--amber-400)",
          fontSize: 12.5,
          whiteSpace: "nowrap",
          overflow: "hidden",
          textOverflow: "ellipsis",
          // Absorbs leftover width + truncates (see Browse note).
          width: "100%",
          maxWidth: 0
        }
      },
      /* @__PURE__ */ React.createElement(
        Icon,
        {
          name: "shield-alert",
          size: 13,
          style: { verticalAlign: "-2px", marginRight: 5 }
        }
      ),
      e.reason
    )
  )), (!rejections || rejections.length === 0) && /* @__PURE__ */ React.createElement("tr", { className: "empty-row" }, /* @__PURE__ */ React.createElement("td", { colSpan: 3 }, /* @__PURE__ */ React.createElement("div", { className: "empty", style: { padding: "48px 24px" } }, /* @__PURE__ */ React.createElement("div", { className: "glyph" }, /* @__PURE__ */ React.createElement(Icon, { name: rejectionsLoading && !rejections ? "loader" : "shield-check", size: 28 })), rejectionsLoading && !rejections ? /* @__PURE__ */ React.createElement("div", null, "Scanning the chain for rejected writes\u2026") : /* @__PURE__ */ React.createElement("div", null, "Every memo in this database parsed and verified correctly."))))))), !firstSyncPending && tab === "roles" && /* @__PURE__ */ React.createElement("div", { className: "dt-wrap" }, /* @__PURE__ */ React.createElement("table", { className: "dt" }, /* @__PURE__ */ React.createElement("thead", null, /* @__PURE__ */ React.createElement("tr", null, /* @__PURE__ */ React.createElement("th", { style: { width: 120 } }, "Role"), /* @__PURE__ */ React.createElement("th", null, "Public Key"), /* @__PURE__ */ React.createElement("th", { style: { width: "36%" } }, "Scope"))), /* @__PURE__ */ React.createElement("tbody", null, roles.map((r, i) => {
    const isCreator = rolesCreator && r.pubkey === rolesCreator;
    return /* @__PURE__ */ React.createElement(
      "tr",
      {
        key: r.role + ":" + r.pubkey,
        className: i === selectedRoleIdx ? "selected" : "",
        onClick: () => onSelectRole(i)
      },
      /* @__PURE__ */ React.createElement("td", null, /* @__PURE__ */ React.createElement("span", { className: "role " + r.role }, /* @__PURE__ */ React.createElement(Icon, { name: r.role === "owner" ? "shield-check" : "key-round", size: 11 }), r.role)),
      /* @__PURE__ */ React.createElement("td", null, /* @__PURE__ */ React.createElement("div", { style: { display: "flex", alignItems: "center", gap: 8, minWidth: 0 } }, /* @__PURE__ */ React.createElement("span", { className: "hash-cell" }, midTrunc(r.pubkey)), isCreator && /* @__PURE__ */ React.createElement("span", { className: "tag-self" }, "creator"), isCreator && isAdmin && /* @__PURE__ */ React.createElement("span", { className: "tag-self" }, "you"))),
      /* @__PURE__ */ React.createElement("td", null, roleScopeCell(r))
    );
  }), (rolesRevoked || []).map((r, j) => {
    const isCreator = rolesCreator && r.pubkey === rolesCreator;
    const idx = roles.length + j;
    return /* @__PURE__ */ React.createElement(
      "tr",
      {
        key: "revoked:" + r.role + ":" + r.pubkey,
        className: "revoked-row" + (idx === selectedRoleIdx ? " selected" : ""),
        onClick: () => onSelectRole(idx)
      },
      /* @__PURE__ */ React.createElement("td", null, /* @__PURE__ */ React.createElement("span", { className: "role revoked " + r.role }, /* @__PURE__ */ React.createElement(Icon, { name: "shield-off", size: 11 }), "revoked ", r.role)),
      /* @__PURE__ */ React.createElement("td", null, /* @__PURE__ */ React.createElement("div", { style: { display: "flex", alignItems: "center", gap: 8, minWidth: 0 } }, /* @__PURE__ */ React.createElement("span", { className: "hash-cell" }, midTrunc(r.pubkey)), isCreator && /* @__PURE__ */ React.createElement("span", { className: "tag-self" }, "creator"))),
      /* @__PURE__ */ React.createElement("td", null, revokedScopeCell(r, timeZone))
    );
  }), roles.length === 0 && (rolesRevoked || []).length === 0 && /* @__PURE__ */ React.createElement("tr", { className: "empty-row" }, /* @__PURE__ */ React.createElement("td", { colSpan: 3 }, /* @__PURE__ */ React.createElement("div", { className: "empty", style: { padding: "48px 24px" } }, /* @__PURE__ */ React.createElement("div", { className: "glyph" }, /* @__PURE__ */ React.createElement(Icon, { name: rolesLoading ? "loader" : "users-round", size: 28 })), rolesLoading ? /* @__PURE__ */ React.createElement("div", null, "Loading roles\u2026") : /* @__PURE__ */ React.createElement("div", null, "No roles yet. Owners and scoped writers appear here once the database is initialized and its", " ", /* @__PURE__ */ React.createElement("code", { style: { fontFamily: "var(--font-mono)", color: "var(--amber-400)" } }, "INIT"), " ", "confirms."))))))), !firstSyncPending && tab === "funding" && /* @__PURE__ */ React.createElement(React.Fragment, null, /* @__PURE__ */ React.createElement("div", { className: "dt-wrap" }, /* @__PURE__ */ React.createElement("table", { className: "dt" }, /* @__PURE__ */ React.createElement("thead", null, /* @__PURE__ */ React.createElement("tr", null, /* @__PURE__ */ React.createElement("th", { style: { width: 200, whiteSpace: "nowrap" } }, "When"), /* @__PURE__ */ React.createElement("th", { style: { width: 160 } }, "Amount"), /* @__PURE__ */ React.createElement("th", null, "Memo"))), /* @__PURE__ */ React.createElement("tbody", null, funding.map((t, i) => /* @__PURE__ */ React.createElement(
    "tr",
    {
      key: t.txid + ":" + i,
      className: i === selectedFundingIdx ? "selected" : "",
      onClick: () => onSelectFunding(i)
    },
    /* @__PURE__ */ React.createElement("td", null, histWhen(t, timeZone)),
    /* @__PURE__ */ React.createElement("td", null, fundingAmount(t, db && db.network)),
    /* @__PURE__ */ React.createElement(
      "td",
      {
        style: {
          whiteSpace: "nowrap",
          overflow: "hidden",
          textOverflow: "ellipsis",
          // Absorbs leftover width + truncates (see Browse note).
          width: "100%",
          maxWidth: 0,
          color: t.memo ? "var(--fg-1)" : "var(--fg-3)"
        }
      },
      t.memo || "\u2014"
    )
  )), funding.length === 0 && /* @__PURE__ */ React.createElement("tr", { className: "empty-row" }, /* @__PURE__ */ React.createElement("td", { colSpan: 3 }, /* @__PURE__ */ React.createElement("div", { className: "empty", style: { padding: "48px 24px" } }, /* @__PURE__ */ React.createElement("div", { className: "glyph" }, /* @__PURE__ */ React.createElement(Icon, { name: fundingLoading ? "loader" : "wallet", size: 28 })), fundingLoading ? /* @__PURE__ */ React.createElement("div", null, "Loading funding\u2026") : /* @__PURE__ */ React.createElement("div", null, "No funding transactions yet. ZEC sent to or from this database's wallet shows up here."))))))), /* @__PURE__ */ React.createElement(
    PaginationBar,
    {
      total: fundingTotal,
      offset: fundingOffset,
      pageSize: fundingPageSize,
      onPage: onFundingPage,
      loading: fundingLoading
    }
  )));
};
const PaginationBar = ({ total, offset, pageSize, onPage, loading }) => {
  if (!total || total <= pageSize) return null;
  const start = offset + 1;
  const end = Math.min(offset + pageSize, total);
  const atStart = offset <= 0;
  const atEnd = offset + pageSize >= total;
  const lastOffset = Math.max(0, Math.floor((total - 1) / pageSize) * pageSize);
  return /* @__PURE__ */ React.createElement("div", { className: "pagination-bar" }, /* @__PURE__ */ React.createElement("span", { className: "pg-info" }, start.toLocaleString(), "\u2013", end.toLocaleString(), " of ", total.toLocaleString()), /* @__PURE__ */ React.createElement("span", { style: { marginLeft: "auto" } }), /* @__PURE__ */ React.createElement("button", { className: "btn ghost sm", disabled: atStart || loading, onClick: () => onPage(0), title: "First page" }, "\xAB"), /* @__PURE__ */ React.createElement("button", { className: "btn ghost sm", disabled: atStart || loading, onClick: () => onPage(offset - pageSize), title: "Previous page" }, "\u2039 Prev"), /* @__PURE__ */ React.createElement("button", { className: "btn ghost sm", disabled: atEnd || loading, onClick: () => onPage(offset + pageSize), title: "Next page" }, "Next \u203A"), /* @__PURE__ */ React.createElement("button", { className: "btn ghost sm", disabled: atEnd || loading, onClick: () => onPage(lastOffset), title: "Last page" }, "\xBB"));
};
const KeyDetail = ({ row, db, timeZone, onWriteKey, onDelete, onCopy, onViewHistory, onOpenTxid, signer, roles, onOpenRole, loading }) => {
  if (!row) {
    return /* @__PURE__ */ React.createElement("aside", { className: "detail" }, /* @__PURE__ */ React.createElement("div", { className: "empty" }, /* @__PURE__ */ React.createElement("div", { className: "glyph" }, /* @__PURE__ */ React.createElement(Icon, { name: loading ? "loader" : "mouse-pointer-click", size: 28 })), /* @__PURE__ */ React.createElement("div", null, loading ? "Loading\u2026" : "Select a key to inspect its current value and on-chain write.")));
  }
  const isAdmin = db && db.role === "admin";
  const s = row.status || { kind: "confirmed", done: 0, required: 0 };
  return /* @__PURE__ */ React.createElement("aside", { className: "detail" }, /* @__PURE__ */ React.createElement("div", { className: "detail-header" }, /* @__PURE__ */ React.createElement("div", { style: { display: "flex", alignItems: "center", justifyContent: "space-between" } }, /* @__PURE__ */ React.createElement("span", { style: { fontFamily: "var(--font-mono)", fontSize: 10, letterSpacing: "0.12em", textTransform: "uppercase", color: "var(--fg-3)" } }, "KEY"), /* @__PURE__ */ React.createElement("div", { style: { display: "flex", gap: 4 } }, /* @__PURE__ */ React.createElement(
    "button",
    {
      className: "btn ghost sm",
      title: "Copy metadata (YAML)",
      onClick: () => onCopy(keyYaml(row, db, signer, timeZone))
    },
    /* @__PURE__ */ React.createElement(Icon, { name: "copy", className: "icon" })
  ), isAdmin && /* @__PURE__ */ React.createElement("button", { className: "btn ghost sm", title: "Delete (broadcast DEL)", onClick: () => onDelete(row) }, /* @__PURE__ */ React.createElement(Icon, { name: "trash-2", className: "icon" })))), /* @__PURE__ */ React.createElement("div", { className: "title" }, row.key), /* @__PURE__ */ React.createElement("div", { style: { display: "flex", alignItems: "center", gap: 8, marginTop: 10, fontFamily: "var(--font-mono)", fontSize: 11.5, color: "var(--fg-3)" } }, statusChip(s), /* @__PURE__ */ React.createElement("span", { style: { opacity: 0.5 } }, "\xB7"), /* @__PURE__ */ React.createElement("span", null, fmtBytes(row.size)))), /* @__PURE__ */ React.createElement("div", { className: "detail-section" }, /* @__PURE__ */ React.createElement("h4", null, "Current value"), row.value != null ? /* @__PURE__ */ React.createElement(CopyableBlock, { text: row.value, onCopy }) : /* @__PURE__ */ React.createElement("div", { style: { fontFamily: "var(--font-mono)", fontSize: 12.5, color: "var(--fg-3)", fontStyle: "italic" } }, "(no confirmed value", row.deleted ? ", deletion in flight" : "", ")"), isAdmin && !row.deleted && /* @__PURE__ */ React.createElement("div", { style: { display: "flex", gap: 8, marginTop: 10 } }, /* @__PURE__ */ React.createElement("button", { className: "btn primary sm", onClick: () => onWriteKey(row) }, /* @__PURE__ */ React.createElement(Icon, { name: "edit-3", className: "icon" }), " Set new value"))), /* @__PURE__ */ React.createElement("div", { className: "detail-section" }, /* @__PURE__ */ React.createElement("h4", null, "On-chain"), /* @__PURE__ */ React.createElement("div", { className: "kv-row" }, /* @__PURE__ */ React.createElement("span", { className: "label" }, "Database"), /* @__PURE__ */ React.createElement("span", { className: "value" }, /* @__PURE__ */ React.createElement(CollapsibleString, { value: db.address, onCopy }))), /* @__PURE__ */ React.createElement("div", { className: "kv-row" }, /* @__PURE__ */ React.createElement("span", { className: "label" }, "Status"), /* @__PURE__ */ React.createElement("span", { className: "value" }, statusChip(s))), /* @__PURE__ */ React.createElement("div", { className: "kv-row" }, /* @__PURE__ */ React.createElement("span", { className: "label" }, "Last updated"), /* @__PURE__ */ React.createElement("span", { className: "value", title: "On-chain block time of the latest write to this key" }, row.updated_at ? fmtWhen(row.updated_at, timeZone) : /* @__PURE__ */ React.createElement("span", { style: { color: "var(--fg-3)" } }, "\u2014"))), /* @__PURE__ */ React.createElement("div", { className: "kv-row" }, /* @__PURE__ */ React.createElement("span", { className: "label" }, "Updated by"), /* @__PURE__ */ React.createElement("span", { className: "value" }, /* @__PURE__ */ React.createElement(SignerLink, { pubkey: signer, role: roleOf(roles, signer), onOpenRole }))), /* @__PURE__ */ React.createElement("div", { className: "kv-row" }, /* @__PURE__ */ React.createElement("span", { className: "label" }, "TXID"), /* @__PURE__ */ React.createElement("span", { className: "value" }, row.txid ? /* @__PURE__ */ React.createElement(
    "button",
    {
      className: "hist-link",
      style: { marginTop: 0, fontFamily: "var(--font-mono)", fontSize: 12 },
      title: "View this transaction in history",
      onClick: () => onOpenTxid && onOpenTxid(row.key, row.txid)
    },
    row.txid.length > 26 ? `${row.txid.slice(0, 16)}\u2026${row.txid.slice(-8)}` : row.txid
  ) : /* @__PURE__ */ React.createElement("span", { style: { color: "var(--fg-3)" } }, "\u2014"))), onViewHistory && /* @__PURE__ */ React.createElement("button", { className: "hist-link", onClick: () => onViewHistory(row.key) }, /* @__PURE__ */ React.createElement(Icon, { name: "history", size: 13 }), " View full history for this key")));
};
const HistoryDetail = ({ entry, creator, onCopy, timeZone, network, roles, onOpenRole }) => {
  if (!entry) {
    return /* @__PURE__ */ React.createElement("aside", { className: "detail" }, /* @__PURE__ */ React.createElement("div", { className: "empty" }, /* @__PURE__ */ React.createElement("div", { className: "glyph" }, /* @__PURE__ */ React.createElement(Icon, { name: "history", size: 28 })), /* @__PURE__ */ React.createElement("div", null, "Select a write to inspect its signature and raw on-chain memo.")));
  }
  const v = entry.verified;
  const signer = entry.signer || creator;
  const signerRole = entry.signer_role || roleOf(roles, signer);
  const banner = v === true ? {
    cls: "ok",
    icon: "shield-check",
    iconColor: "var(--green-500)",
    title: "Valid and authorized signature",
    // No prose: just who signed it, as a clickable role-tagged chip.
    sub: /* @__PURE__ */ React.createElement(SignerLink, { pubkey: signer, role: signerRole, onOpenRole })
  } : v === false ? {
    cls: "bad",
    icon: "shield-alert",
    iconColor: "var(--amber-500)",
    title: "Invalid or unauthorized signature",
    sub: "Readers drop this write, it never enters state."
  } : {
    cls: "bad",
    icon: "clock",
    iconColor: "var(--amber-400)",
    title: "Awaiting confirmation",
    sub: "Verified once it's confirmed on-chain."
  };
  const s = entry.status || {};
  return /* @__PURE__ */ React.createElement("aside", { className: "detail" }, /* @__PURE__ */ React.createElement("div", { className: "detail-header" }, /* @__PURE__ */ React.createElement("div", { style: { display: "flex", alignItems: "center", justifyContent: "space-between" } }, /* @__PURE__ */ React.createElement("span", { style: { fontFamily: "var(--font-mono)", fontSize: 10, letterSpacing: "0.12em", textTransform: "uppercase", color: "var(--fg-3)" } }, entry.op === "DEL" ? "DELETE" : "WRITE"), /* @__PURE__ */ React.createElement("div", { style: { display: "flex", gap: 4 } }, /* @__PURE__ */ React.createElement(
    "button",
    {
      className: "btn ghost sm",
      title: "Copy metadata (YAML)",
      onClick: () => onCopy(historyYaml(entry, signer, network, timeZone))
    },
    /* @__PURE__ */ React.createElement(Icon, { name: "copy", className: "icon" })
  ))), /* @__PURE__ */ React.createElement("div", { className: "title" }, entry.op === "INIT" ? "Database created" : entry.key), /* @__PURE__ */ React.createElement("div", { style: { display: "flex", alignItems: "center", gap: 8, marginTop: 10, fontFamily: "var(--font-mono)", fontSize: 11.5, color: "var(--fg-3)" } }, /* @__PURE__ */ React.createElement("span", { className: "op " + entry.op.toLowerCase() }, entry.op), statusChip(s))), /* @__PURE__ */ React.createElement("div", { className: "detail-section" }, /* @__PURE__ */ React.createElement("h4", null, entry.op === "INIT" ? "Database claim" : entry.op === "DEL" ? "Deleted value" : "Value written"), entry.op === "INIT" ? /* @__PURE__ */ React.createElement("div", { style: { fontSize: 12.5, color: "var(--fg-2)", lineHeight: 1.5 } }, "The signed INIT that claims this database on-chain. Required before any writes.") : entry.op === "DEL" ? /* @__PURE__ */ React.createElement("div", { style: { fontFamily: "var(--font-mono)", fontSize: 12.5, color: "var(--fg-3)", fontStyle: "italic" } }, "(this write removed the key)") : entry.value != null ? /* @__PURE__ */ React.createElement(CopyableBlock, { text: entry.value, onCopy }) : /* @__PURE__ */ React.createElement("div", { style: { fontFamily: "var(--font-mono)", fontSize: 12.5, color: "var(--fg-3)", fontStyle: "italic" } }, "(no value)")), /* @__PURE__ */ React.createElement("div", { className: "detail-section" }, /* @__PURE__ */ React.createElement("h4", null, "On-chain"), /* @__PURE__ */ React.createElement("div", { className: "kv-row" }, /* @__PURE__ */ React.createElement("span", { className: "label" }, "Timestamp"), /* @__PURE__ */ React.createElement("span", { className: "value" }, entry.timestamp ? fmtWhen(entry.timestamp, timeZone) : "\u2014")), /* @__PURE__ */ React.createElement("div", { className: "kv-row" }, /* @__PURE__ */ React.createElement("span", { className: "label" }, "Height"), /* @__PURE__ */ React.createElement("span", { className: "value" }, entry.height != null ? "#" + entry.height : "mempool")), /* @__PURE__ */ React.createElement("div", { className: "kv-row" }, /* @__PURE__ */ React.createElement("span", { className: "label" }, "Status"), /* @__PURE__ */ React.createElement("span", { className: "value" }, statusChip(s))), /* @__PURE__ */ React.createElement("div", { className: "kv-row" }, /* @__PURE__ */ React.createElement("span", { className: "label" }, "TXID"), /* @__PURE__ */ React.createElement("span", { className: "value" }, entry.txid ? /* @__PURE__ */ React.createElement(CollapsibleString, { value: entry.txid, onCopy }) : /* @__PURE__ */ React.createElement("span", { style: { color: "var(--fg-3)" } }, "\u2014"))), entry.output_value != null && entry.output_value > 0 && /* @__PURE__ */ React.createElement("div", { className: "kv-row" }, /* @__PURE__ */ React.createElement("span", { className: "label" }, "Output"), /* @__PURE__ */ React.createElement("span", { className: "value", title: "ZEC value carried by this write's output (broadcast alongside the memo)" }, window.formatZats(entry.output_value, network))), /* @__PURE__ */ React.createElement("div", { className: "kv-row" }, /* @__PURE__ */ React.createElement("span", { className: "label" }, "Fee"), /* @__PURE__ */ React.createElement("span", { className: "value", title: "Actual fee paid for this transaction" }, entry.fee != null ? window.formatZats(entry.fee, network) : /* @__PURE__ */ React.createElement("span", { style: { color: "var(--fg-3)" } }, "\u2014"))), /* @__PURE__ */ React.createElement("div", { className: "kv-row" }, /* @__PURE__ */ React.createElement("span", { className: "label" }, "Signer"), /* @__PURE__ */ React.createElement("span", { className: "value" }, /* @__PURE__ */ React.createElement(SignerLink, { pubkey: signer, role: signerRole, onOpenRole }))), /* @__PURE__ */ React.createElement("div", { className: "kv-row" }, /* @__PURE__ */ React.createElement("span", { className: "label", title: "Replay-protection sequence this write referenced on the wire" }, "Sequence"), /* @__PURE__ */ React.createElement("span", { className: "value" }, entry.seq != null ? entry.seq : /* @__PURE__ */ React.createElement("span", { style: { color: "var(--fg-3)" } }, "\u2014"))), entry.signature && /* @__PURE__ */ React.createElement("div", { className: "kv-row" }, /* @__PURE__ */ React.createElement("span", { className: "label" }, "Signature"), /* @__PURE__ */ React.createElement("span", { className: "value" }, /* @__PURE__ */ React.createElement(CollapsibleString, { value: entry.signature, onCopy })))), /* @__PURE__ */ React.createElement("div", { className: "detail-section" }, /* @__PURE__ */ React.createElement("h4", null, "Raw memo"), entry.memo ? /* @__PURE__ */ React.createElement(CopyableBlock, { text: entry.memo, onCopy }) : /* @__PURE__ */ React.createElement("div", { style: { fontFamily: "var(--font-mono)", fontSize: 12.5, color: "var(--fg-3)", fontStyle: "italic" } }, "(appears once it's on-chain)")), /* @__PURE__ */ React.createElement("div", { className: "detail-section" }, /* @__PURE__ */ React.createElement("div", { className: "verify-banner " + banner.cls }, /* @__PURE__ */ React.createElement(Icon, { name: banner.icon, size: 18, color: banner.iconColor }), /* @__PURE__ */ React.createElement("div", null, /* @__PURE__ */ React.createElement("div", { className: "vb-title" }, banner.title), /* @__PURE__ */ React.createElement("div", { className: "vb-sub" }, banner.sub)))));
};
const RejectionDetail = ({ entry, onCopy, timeZone }) => {
  if (!entry) {
    return /* @__PURE__ */ React.createElement("aside", { className: "detail" }, /* @__PURE__ */ React.createElement("div", { className: "empty" }, /* @__PURE__ */ React.createElement("div", { className: "glyph" }, /* @__PURE__ */ React.createElement(Icon, { name: "shield-alert", size: 28 })), /* @__PURE__ */ React.createElement("div", null, "Select a rejected write to inspect the raw broadcast and why it was dropped.")));
  }
  const title = entry.op || "Unparseable memo";
  return /* @__PURE__ */ React.createElement("aside", { className: "detail" }, /* @__PURE__ */ React.createElement("div", { className: "detail-header" }, /* @__PURE__ */ React.createElement("span", { style: { fontFamily: "var(--font-mono)", fontSize: 10, letterSpacing: "0.12em", textTransform: "uppercase", color: "var(--fg-3)" } }, "REJECTED"), /* @__PURE__ */ React.createElement("div", { className: "title" }, title), /* @__PURE__ */ React.createElement("div", { style: { display: "flex", alignItems: "center", gap: 8, marginTop: 10, fontFamily: "var(--font-mono)", fontSize: 11.5, color: "var(--fg-3)" } }, /* @__PURE__ */ React.createElement("span", { className: "op " + (entry.op ? entry.op.toLowerCase() : "unknown") }, entry.op || "?"), /* @__PURE__ */ React.createElement("span", null, "dropped during replay"))), /* @__PURE__ */ React.createElement("div", { className: "detail-section" }, /* @__PURE__ */ React.createElement("h4", null, "Raw broadcast"), entry.raw ? /* @__PURE__ */ React.createElement(CopyableBlock, { text: entry.raw, onCopy }) : /* @__PURE__ */ React.createElement("div", { style: { fontFamily: "var(--font-mono)", fontSize: 12.5, color: "var(--fg-3)", fontStyle: "italic" } }, "(memo text unavailable)")), /* @__PURE__ */ React.createElement("div", { className: "detail-section" }, /* @__PURE__ */ React.createElement("h4", null, "Rejection Reason"), /* @__PURE__ */ React.createElement("div", { className: "verify-banner bad", style: { marginBottom: 12 } }, /* @__PURE__ */ React.createElement(Icon, { name: "shield-alert", size: 18, color: "var(--amber-500)" }), /* @__PURE__ */ React.createElement("div", { style: { flex: 1, minWidth: 0 } }, /* @__PURE__ */ React.createElement("div", { className: "vb-title" }, entry.reason)), /* @__PURE__ */ React.createElement(
    "button",
    {
      className: "btn ghost sm",
      title: "Copy error",
      onClick: () => onCopy(entry.reason),
      style: { flexShrink: 0, width: 24, height: 24, padding: 0 }
    },
    /* @__PURE__ */ React.createElement(Icon, { name: "copy", className: "icon" })
  )), /* @__PURE__ */ React.createElement("div", { className: "kv-row" }, /* @__PURE__ */ React.createElement("span", { className: "label" }, "Valid Signature"), /* @__PURE__ */ React.createElement("span", { className: "value", style: { display: "inline-flex", alignItems: "center", gap: 6 } }, entry.signature_valid ? /* @__PURE__ */ React.createElement(React.Fragment, null, /* @__PURE__ */ React.createElement(Icon, { name: "shield-check", size: 14, color: "var(--green-500)" }), /* @__PURE__ */ React.createElement("span", { style: { color: "var(--green-500)" } }, "yes")) : /* @__PURE__ */ React.createElement(React.Fragment, null, /* @__PURE__ */ React.createElement(Icon, { name: "shield-alert", size: 14, color: "var(--red-500)" }), /* @__PURE__ */ React.createElement("span", { style: { color: "var(--red-500)" } }, "no")))), /* @__PURE__ */ React.createElement("div", { className: "kv-row" }, /* @__PURE__ */ React.createElement("span", { className: "label" }, "Authorized"), /* @__PURE__ */ React.createElement("span", { className: "value", style: { display: "inline-flex", alignItems: "center", gap: 6 } }, entry.signature_valid ? /* @__PURE__ */ React.createElement(React.Fragment, null, /* @__PURE__ */ React.createElement(Icon, { name: "x-circle", size: 14, color: "var(--red-500)" }), /* @__PURE__ */ React.createElement("span", { style: { color: "var(--red-500)" } }, "no")) : /* @__PURE__ */ React.createElement("span", { style: { color: "var(--fg-3)" } }, "\u2014 (no signer to authorize)"))), entry.signature_valid && entry.signer && /* @__PURE__ */ React.createElement("div", { className: "kv-row" }, /* @__PURE__ */ React.createElement("span", { className: "label" }, "Signer"), /* @__PURE__ */ React.createElement("span", { className: "value" }, /* @__PURE__ */ React.createElement(CollapsibleString, { value: entry.signer, onCopy })))), /* @__PURE__ */ React.createElement("div", { className: "detail-section" }, /* @__PURE__ */ React.createElement("h4", null, "On-chain"), /* @__PURE__ */ React.createElement("div", { className: "kv-row" }, /* @__PURE__ */ React.createElement("span", { className: "label" }, "Timestamp"), /* @__PURE__ */ React.createElement("span", { className: "value" }, entry.timestamp ? fmtWhen(entry.timestamp, timeZone) : "\u2014")), /* @__PURE__ */ React.createElement("div", { className: "kv-row" }, /* @__PURE__ */ React.createElement("span", { className: "label" }, "Height"), /* @__PURE__ */ React.createElement("span", { className: "value" }, entry.height != null ? "#" + entry.height : "mempool")), /* @__PURE__ */ React.createElement("div", { className: "kv-row" }, /* @__PURE__ */ React.createElement("span", { className: "label" }, "TXID"), /* @__PURE__ */ React.createElement("span", { className: "value" }, entry.txid ? /* @__PURE__ */ React.createElement(CollapsibleString, { value: entry.txid, onCopy }) : /* @__PURE__ */ React.createElement("span", { style: { color: "var(--fg-3)" } }, "\u2014"))), entry.op && /* @__PURE__ */ React.createElement("div", { className: "kv-row" }, /* @__PURE__ */ React.createElement("span", { className: "label" }, "Op"), /* @__PURE__ */ React.createElement("span", { className: "value" }, entry.op)), entry.key != null && /* @__PURE__ */ React.createElement("div", { className: "kv-row" }, /* @__PURE__ */ React.createElement("span", { className: "label" }, "Key"), /* @__PURE__ */ React.createElement("span", { className: "value" }, /* @__PURE__ */ React.createElement("span", { className: "key" }, entry.key))), entry.value != null && entry.value !== "" && /* @__PURE__ */ React.createElement("div", { className: "kv-row" }, /* @__PURE__ */ React.createElement("span", { className: "label" }, "Value"), /* @__PURE__ */ React.createElement("span", { className: "value" }, /* @__PURE__ */ React.createElement(CollapsibleString, { value: entry.value, onCopy })))));
};
const RoleDetail = ({ entry, db, creator, timeZone, onCopy, loading }) => {
  const eyebrow = {
    fontFamily: "var(--font-mono)",
    fontSize: 10,
    letterSpacing: "0.12em",
    textTransform: "uppercase",
    color: "var(--fg-3)"
  };
  if (!entry) {
    return /* @__PURE__ */ React.createElement("aside", { className: "detail" }, /* @__PURE__ */ React.createElement("div", { className: "empty" }, /* @__PURE__ */ React.createElement("div", { className: "glyph" }, /* @__PURE__ */ React.createElement(Icon, { name: loading ? "loader" : "users-round", size: 28 })), /* @__PURE__ */ React.createElement("div", null, loading ? "Loading roles\u2026" : "Select a role to see its authority, public key, and when it was granted.")));
  }
  const isAdmin = db && db.role === "admin";
  const isCreator = creator && entry.pubkey === creator;
  const revoked = !!entry.revoked;
  const isOwner = entry.role === "owner";
  const caps = entry.capabilities || [];
  const subtitle = revoked ? "no longer authorized" : isOwner ? "full authority" : "scoped writer";
  return /* @__PURE__ */ React.createElement("aside", { className: "detail" }, /* @__PURE__ */ React.createElement("div", { className: "detail-header" }, /* @__PURE__ */ React.createElement("div", { style: { display: "flex", alignItems: "center", justifyContent: "space-between" } }, /* @__PURE__ */ React.createElement("span", { style: eyebrow }, "ROLE"), /* @__PURE__ */ React.createElement("button", { className: "btn ghost sm", title: "Copy public key", onClick: () => onCopy(entry.pubkey) }, /* @__PURE__ */ React.createElement(Icon, { name: "copy", className: "icon" }))), /* @__PURE__ */ React.createElement("div", { className: "title", style: { display: "flex", alignItems: "center", gap: 8, flexWrap: "wrap" } }, /* @__PURE__ */ React.createElement("span", { className: "role " + (revoked ? "revoked " : "") + entry.role }, /* @__PURE__ */ React.createElement(Icon, { name: revoked ? "shield-off" : isOwner ? "shield-check" : "key-round", size: 12 }), (revoked ? "revoked " : "") + entry.role), isCreator && /* @__PURE__ */ React.createElement("span", { className: "tag-self" }, "creator"), isCreator && isAdmin && /* @__PURE__ */ React.createElement("span", { className: "tag-self" }, "you")), /* @__PURE__ */ React.createElement("div", { style: { marginTop: 10, fontFamily: "var(--font-mono)", fontSize: 11.5, color: "var(--fg-3)" } }, subtitle)), /* @__PURE__ */ React.createElement("div", { className: "detail-section" }, /* @__PURE__ */ React.createElement("h4", null, "Public Key"), /* @__PURE__ */ React.createElement(CopyableBlock, { text: entry.pubkey, onCopy })), !isOwner && /* @__PURE__ */ React.createElement("div", { className: "detail-section" }, /* @__PURE__ */ React.createElement("h4", null, "Scope"), caps.length ? /* @__PURE__ */ React.createElement("span", { style: { display: "inline-flex", gap: 6, flexWrap: "wrap" } }, caps.map((c) => /* @__PURE__ */ React.createElement(
    "span",
    {
      key: c,
      className: "cap-chip",
      style: revoked ? { opacity: 0.5, textDecoration: "line-through" } : void 0
    },
    c
  ))) : /* @__PURE__ */ React.createElement("span", { style: { color: "var(--fg-3)" } }, "\u2014"), /* @__PURE__ */ React.createElement("div", { style: { fontSize: 12, color: "var(--fg-3)", lineHeight: 1.5, marginTop: 8 } }, "A writer may write only within its scope. Reads are public to anyone holding the UFVK.")), /* @__PURE__ */ React.createElement("div", { className: "detail-section" }, /* @__PURE__ */ React.createElement("h4", null, revoked ? "Revoked" : "Granted"), /* @__PURE__ */ React.createElement("div", { className: "kv-row" }, /* @__PURE__ */ React.createElement("span", { className: "label" }, "When"), /* @__PURE__ */ React.createElement(
    "span",
    {
      className: "value",
      title: revoked ? "On-chain block time of the revoking op" : "On-chain block time of the grant"
    },
    whenAt(entry.timestamp, entry.height, timeZone)
  )), /* @__PURE__ */ React.createElement("div", { className: "kv-row" }, /* @__PURE__ */ React.createElement("span", { className: "label" }, "By"), /* @__PURE__ */ React.createElement("span", { className: "value" }, revoked ? entry.revoked_by ? /* @__PURE__ */ React.createElement(CollapsibleString, { value: entry.revoked_by, onCopy }) : /* @__PURE__ */ React.createElement("span", { style: { color: "var(--fg-3)" } }, "\u2014") : isCreator ? /* @__PURE__ */ React.createElement("span", { style: { color: "var(--fg-3)" } }, "self \xB7 INIT") : entry.granted_by ? /* @__PURE__ */ React.createElement(CollapsibleString, { value: entry.granted_by, onCopy }) : /* @__PURE__ */ React.createElement("span", { style: { color: "var(--fg-3)" } }, "\u2014"))), isCreator && !revoked && /* @__PURE__ */ React.createElement("div", { style: { fontSize: 11.5, color: "var(--fg-3)", lineHeight: 1.5, marginTop: 8 } }, "The UFVK-derived key that signed INIT, owner #1 the moment INIT confirmed, so this is the database's birth. The creator is a permanent trait, kept even if its owner authority is later revoked.")));
};
const FundingDetail = ({ tx, onCopy, timeZone, network, onOpenTxid }) => {
  if (!tx) {
    return /* @__PURE__ */ React.createElement("aside", { className: "detail" }, /* @__PURE__ */ React.createElement("div", { className: "empty" }, /* @__PURE__ */ React.createElement("div", { className: "glyph" }, /* @__PURE__ */ React.createElement(Icon, { name: "wallet", size: 28 })), /* @__PURE__ */ React.createElement("div", null, "Select a transaction to see its amount, memo, and, for sends, the recipient.")));
  }
  const self = tx.direction === "self";
  const received = tx.direction === "received";
  const isZkvOp = tx.direction === "zkv";
  const color = received ? "var(--green-500)" : "var(--red-500)";
  const amountStr = (received ? "+" : "\u2212") + window.formatZats(tx.amount, network);
  const fundStatus = tx.pending ? { kind: "pending" } : tx.confirmed ? { kind: "confirmed" } : { kind: "confirming", done: tx.confirmations, required: tx.required };
  const hasRecipients = !received && !self && tx.recipients && tx.recipients.length > 0;
  return /* @__PURE__ */ React.createElement("aside", { className: "detail" }, /* @__PURE__ */ React.createElement("div", { className: "detail-header" }, /* @__PURE__ */ React.createElement("div", { style: { display: "flex", alignItems: "center", justifyContent: "space-between" } }, /* @__PURE__ */ React.createElement("span", { style: { fontFamily: "var(--font-mono)", fontSize: 10, letterSpacing: "0.12em", textTransform: "uppercase", color: "var(--fg-3)" } }, isZkvOp ? "ZKV OPERATION" : self ? "SENT TO SELF" : received ? "RECEIVED" : "SENT"), /* @__PURE__ */ React.createElement("button", { className: "btn ghost sm", title: "Copy amount", onClick: () => onCopy(amountStr) }, /* @__PURE__ */ React.createElement(Icon, { name: "copy", className: "icon" }))), /* @__PURE__ */ React.createElement("div", { className: "title", style: { color, fontFamily: "var(--font-mono)" } }, amountStr), /* @__PURE__ */ React.createElement("div", { style: { display: "flex", alignItems: "center", gap: 8, marginTop: 10, fontFamily: "var(--font-mono)", fontSize: 11.5, color: "var(--fg-3)" } }, statusChip(fundStatus))), hasRecipients && /* @__PURE__ */ React.createElement("div", { className: "detail-section" }, /* @__PURE__ */ React.createElement("h4", null, tx.recipients.length > 1 ? "Recipients" : "Recipient"), tx.recipients.map((r, i) => /* @__PURE__ */ React.createElement("div", { className: "kv-row", key: i }, /* @__PURE__ */ React.createElement("span", { className: "value" }, /* @__PURE__ */ React.createElement(CollapsibleString, { value: r, onCopy }))))), /* @__PURE__ */ React.createElement("div", { className: "detail-section" }, /* @__PURE__ */ React.createElement("h4", null, "On-chain"), /* @__PURE__ */ React.createElement("div", { className: "kv-row" }, /* @__PURE__ */ React.createElement("span", { className: "label" }, "Amount"), /* @__PURE__ */ React.createElement("span", { className: "value", style: { color } }, amountStr)), self && tx.self_sent != null && /* @__PURE__ */ React.createElement("div", { className: "kv-row" }, /* @__PURE__ */ React.createElement("span", { className: "label" }, "Output"), /* @__PURE__ */ React.createElement("span", { className: "value", title: "Value sent to one of your own addresses (returned to this wallet)" }, window.formatZats(tx.self_sent, network))), /* @__PURE__ */ React.createElement("div", { className: "kv-row" }, /* @__PURE__ */ React.createElement("span", { className: "label" }, "Timestamp"), /* @__PURE__ */ React.createElement("span", { className: "value" }, tx.timestamp ? fmtWhen(tx.timestamp, timeZone) : "\u2014")), /* @__PURE__ */ React.createElement("div", { className: "kv-row" }, /* @__PURE__ */ React.createElement("span", { className: "label" }, "Height"), /* @__PURE__ */ React.createElement("span", { className: "value" }, tx.height != null ? "#" + tx.height : "mempool")), /* @__PURE__ */ React.createElement("div", { className: "kv-row" }, /* @__PURE__ */ React.createElement("span", { className: "label" }, "Status"), /* @__PURE__ */ React.createElement("span", { className: "value" }, statusChip(fundStatus))), /* @__PURE__ */ React.createElement("div", { className: "kv-row" }, /* @__PURE__ */ React.createElement("span", { className: "label" }, "Fee"), /* @__PURE__ */ React.createElement("span", { className: "value", title: "Network fee paid by this wallet (sends only)" }, tx.fee != null ? window.formatZats(tx.fee, network) : /* @__PURE__ */ React.createElement("span", { style: { color: "var(--fg-3)" } }, "\u2014"))), /* @__PURE__ */ React.createElement("div", { className: "kv-row" }, /* @__PURE__ */ React.createElement("span", { className: "label" }, "TXID"), /* @__PURE__ */ React.createElement("span", { className: "value" }, tx.txid ? /* @__PURE__ */ React.createElement(CollapsibleString, { value: tx.txid, onCopy }) : /* @__PURE__ */ React.createElement("span", { style: { color: "var(--fg-3)" } }, "\u2014")))), tx.is_zkv && tx.txid && onOpenTxid && /* @__PURE__ */ React.createElement("div", { className: "detail-section" }, /* @__PURE__ */ React.createElement("h4", null, "zkv operation"), /* @__PURE__ */ React.createElement("div", { style: { fontSize: 12.5, color: "var(--fg-2)", lineHeight: 1.5, marginBottom: 8 } }, isZkvOp ? "This transaction is a zkv write; its only cost was the network fee." : "This transaction also carries a zkv write."), /* @__PURE__ */ React.createElement(
    "button",
    {
      className: "hist-link",
      style: { marginTop: 0, fontFamily: "var(--font-mono)", fontSize: 12 },
      title: "View this write in History",
      onClick: () => onOpenTxid("", tx.txid)
    },
    "View operation in History \u2192"
  )), tx.memo && /* @__PURE__ */ React.createElement("div", { className: "detail-section" }, /* @__PURE__ */ React.createElement("h4", null, "Memo"), /* @__PURE__ */ React.createElement(CopyableBlock, { text: tx.memo, onCopy })));
};
window.KeyList = KeyList;
window.KeyDetail = KeyDetail;
window.HistoryDetail = HistoryDetail;
window.RejectionDetail = RejectionDetail;
window.RoleDetail = RoleDetail;
window.FundingDetail = FundingDetail;
window.CollapsibleString = CollapsibleString;
window.CopyableBlock = CopyableBlock;
window.ErrorMessage = ErrorMessage;
