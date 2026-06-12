// keys.jsx: KeyList + KeyDetail, wired to live DbDetail data.
//
// A key row from /api/databases/:n is:
//   { key, value: string|null, status: { kind, done, required },
//     txid: string|null, deleted: bool, size: number|null }
// where status.kind is one of confirmed | confirming | pending | deleting.

const fmtBytes = (n: number | null | undefined) => {
  if (n == null) return "—";
  if (n < 1024) return n + " B";
  return (n / 1024).toFixed(1) + " KB";
};

// ----- shared primitives -----

// Collapsible monospace string: truncated by default, click to expand,
// with a copy button. Used for the database address and the TXID.
const CollapsibleString = ({ value, onCopy }: { value: string | null; onCopy: (s: string) => void }) => {
  const [open, setOpen] = React.useState(false);
  const long = value && value.length > 30;
  const shown = open || !long ? value : value.slice(0, 16) + "…" + value.slice(-10);
  return (
    <span
      className="collapse-str"
      style={{ display: "inline-flex", alignItems: "flex-start", gap: 6, minWidth: 0, maxWidth: "100%" }}
    >
      <span
        onClick={() => long && setOpen((o) => !o)}
        title={long ? (open ? "Click to collapse" : "Click to expand") : undefined}
        style={{
          fontFamily: "var(--font-mono)",
          color: "var(--fg-1)",
          cursor: long ? "pointer" : "default",
          // Collapsed: the value is already middle-truncated to fit, so keep
          // it on a single line; otherwise it breaks at the "…" the moment a
          // scrollbar (or a shorter window) shaves a few pixels off the pane.
          wordBreak: open ? "break-all" : "normal",
          whiteSpace: open ? "normal" : "nowrap",
          lineHeight: 1.5,
        }}
      >
        {shown}
        {long && open && (
          <Icon name="chevron-up" size={12} style={{ marginLeft: 4, color: "var(--fg-3)", verticalAlign: "-2px" }} />
        )}
      </span>
      <button
        className="btn ghost sm"
        title="Copy"
        onClick={(e) => {
          e.stopPropagation();
          onCopy(value!);
        }}
        style={{ flexShrink: 0, width: 22, height: 22, padding: 0 }}
      >
        <Icon name="copy" className="icon" />
      </button>
    </span>
  );
};

// Value block with a hover-reveal copy button.
const CopyableBlock = ({ text, onCopy }: { text: string; onCopy: (s: string) => void }) => {
  const [copied, setCopied] = React.useState(false);
  const doCopy = () => {
    onCopy(text);
    setCopied(true);
    setTimeout(() => setCopied(false), 1200);
  };
  return (
    <div className="copyable-block">
      <pre className="value-block">{text}</pre>
      <button className="copy-fab" title="Copy" onClick={doCopy}>
        <Icon name={copied ? "check" : "copy"} className="icon" />
      </button>
    </div>
  );
};

// Error detail line with a copy button. Surfaces the raw error message (e.g. a
// node's consensus-validation reason) next to a one-click copy control so users
// can paste it into a bug report instead of retyping it from the screen.
const ErrorMessage = ({ message }: { message: string }) => {
  const [copied, setCopied] = React.useState(false);
  const doCopy = (e: React.MouseEvent) => {
    e.stopPropagation();
    try {
      navigator.clipboard.writeText(message);
    } catch {}
    setCopied(true);
    setTimeout(() => setCopied(false), 1200);
  };
  return (
    <div style={{ marginTop: 4, display: "flex", alignItems: "flex-start", gap: 8 }}>
      <div
        style={{
          fontSize: 12.5,
          color: "var(--fg-2)",
          flex: "1 1 auto",
          minWidth: 0,
          wordBreak: "break-word",
        }}
      >
        {message}
      </div>
      <button
        className="btn ghost sm"
        title={copied ? "Copied" : "Copy error"}
        onClick={doCopy}
        style={{ flexShrink: 0, width: 22, height: 22, padding: 0 }}
      >
        <Icon name={copied ? "check" : "copy"} className="icon" />
      </button>
    </div>
  );
};

// Format a unix-seconds block timestamp in the chosen display zone, with the
// zone abbreviation appended, e.g. "May 11, 2026, 11:18 PM UTC". `tz` is an
// IANA name, "UTC", or "local" (the browser's zone); defaults to UTC.
const fmtWhen = (ts: number, tz?: string | null, withZone = true) => {
  const opts: Intl.DateTimeFormatOptions = {
    year: "numeric",
    month: "short",
    day: "numeric",
    hour: "numeric",
    minute: "2-digit",
  };
  const zone = tz || "UTC";
  if (zone !== "local") opts.timeZone = zone;
  if (withZone) opts.timeZoneName = "short";
  return new Date(ts * 1000).toLocaleString(undefined, opts);
};

// `fmtAgo` (relative "time ago" label) is defined in chrome.tsx and shared
// across the program; chrome.js loads before keys.js so it's available here.

// ----- signer ↔ roles linking -----

// Truncate a 66-char compressed pubkey for inline display.
const truncPub = (pk: string | null | undefined) => (pk && pk.length > 18 ? pk.slice(0, 10) + "…" + pk.slice(-6) : pk || "");

// Middle-truncate a long hash/id for a non-expanding inline cell (same
// look as the Rejections table's TXID). Used in the Roles table, where the full
// key lives in the detail pane rather than expanding in place.
const midTrunc = (s: string | null | undefined) => (s && s.length > 26 ? s.slice(0, 16) + "…" + s.slice(-8) : s || "");

// "Granted/revoked at" label: the block time if known, else the height, else the "—" placeholder.
const whenAt = (ts: number | null, height: number | null, tz?: string | null) =>
  ts != null ? fmtWhen(ts, tz) : height != null ? "#" + height : "—";

// Resolve a signer pubkey to its role in the loaded registry: "owner" or
// "writer", or null when the key isn't in the registry yet (or roles haven't
// loaded). Both sides use the same compressed-hex, so a direct match works.
const roleOf = (rows: RoleRow[] | null | undefined, pk: string | null | undefined) => {
  if (!pk || !rows) return null;
  const r = rows.find((x) => x.pubkey === pk);
  return r ? r.role : null;
};

// A signer pubkey shown as a role-tagged, truncated, clickable chip that jumps
// to the Roles tab with this key highlighted. `role` is "owner"/"writer"/null.
const SignerLink = ({ pubkey, role, onOpenRole }: {
  pubkey: string | null | undefined;
  role: string | null;
  onOpenRole?: (pk: string) => void;
}) => {
  if (!pubkey) return <span style={{ color: "var(--fg-3)" }}>—</span>;
  return (
    <button
      type="button"
      className="signer-link"
      title={(role ? role + " · " : "") + pubkey + "\nView in Roles"}
      onClick={() => onOpenRole && onOpenRole(pubkey)}
    >
      {role && <span className={"role " + role}>{role}</span>}
      <span className="signer-pub">{truncPub(pubkey)}</span>
    </button>
  );
};

// ----- YAML metadata (clipboard copy for the History / Browse detail panes) -----

// Minimal YAML scalar emitter: bare for safe identifier-ish tokens,
// double-quoted (escaped) for freeform strings, block scalar for multiline.
// Tuned for readable copy-to-clipboard, not full YAML spec coverage.
const yamlScalar = (v: unknown) => {
  if (v == null) return "null";
  if (typeof v === "number") return String(v);
  if (typeof v === "boolean") return v ? "true" : "false";
  const s = String(v);
  if (s === "") return '""';
  if (s.indexOf("\n") >= 0) {
    return "|-\n" + s.split("\n").map((l) => "    " + l).join("\n");
  }
  // Bare only for identifier-ish tokens that aren't a YAML keyword or a
  // number-looking string (so a string value of "42000" round-trips as text,
  // not an int). Everything else is double-quoted.
  if (
    /^[A-Za-z0-9_][\w./@+-]*$/.test(s) &&
    !/^(true|false|null|yes|no|on|off|~)$/i.test(s) &&
    !/^[+-]?\d[\d_]*(\.\d*)?([eE][+-]?\d+)?$/.test(s)
  ) {
    return s;
  }
  return '"' + s.replace(/\\/g, "\\\\").replace(/"/g, '\\"') + '"';
};

// Build a YAML document from ordered [key, value] pairs. A pair whose value is
// `undefined` is dropped entirely; `null` is emitted explicitly as `null`.
const toYaml = (pairs: Array<[string, unknown]>) =>
  pairs
    .filter((p) => p[1] !== undefined)
    .map((p) => p[0] + ": " + yamlScalar(p[1]))
    .join("\n");

// Verbose YAML for one History write: the op/key/value plus every field the
// detail's "On-chain" panel shows. Mirrors HistoryDetail.
const historyYaml = (e: HistoryEntryResp, signer: string | null, network: string | null, tz?: string | null) => {
  const s = e.status || ({} as HistoryStatusResp);
  const statusLabel =
    s.kind === "confirmed"
      ? "confirmed"
      : s.kind === "confirming"
      ? "confirming (" + s.done + "/" + s.required + ")"
      : "pending";
  const hasValue = e.value != null && e.op !== "DEL" && e.op !== "INIT";
  return (
    toYaml([
      ["operation", e.op],
      ["key", e.op === "INIT" ? undefined : e.key],
      ["address", e.op === "INIT" ? e.key : undefined],
      ["value", hasValue ? e.value : undefined],
      ["timestamp", e.timestamp ? fmtWhen(e.timestamp, tz) : null],
      ["height", e.height != null ? e.height : "mempool"],
      ["status", statusLabel],
      ["txid", e.txid || null],
      ["output_index", e.output_index],
      [
        "output_value",
        e.output_value != null
          ? window.formatZats(e.output_value, network)
          : "0 " + window.currencyFor(network),
      ],
      ["fee", e.fee != null ? window.formatZats(e.fee, network) : null],
      ["sequence", e.seq != null ? e.seq : null],
      ["signer", signer || null],
      ["signature", e.signature || null],
    ]) + "\n"
  );
};

// Verbose YAML for one Browse key: its current value plus the detail's
// "On-chain" panel fields. Mirrors KeyDetail.
const keyYaml = (row: KeyRow, db: ActiveDb | null, signer: string | null, tz?: string | null) => {
  const s = row.status || ({} as KeyStatus);
  const statusLabel =
    s.kind === "confirming" ? "confirming (" + s.done + "/" + s.required + ")" : s.kind;
  return (
    toYaml([
      ["key", row.key],
      ["value", row.value != null ? row.value : null],
      ["status", statusLabel],
      ["deleted", row.deleted ? true : undefined],
      ["size_in_bytes", row.size != null ? row.size : undefined],
      ["database", db && db.address],
      ["last_updated", row.updated_at ? fmtWhen(row.updated_at, tz) : null],
      ["txid", row.txid || null],
      ["updated_by", signer || null],
    ]) + "\n"
  );
};

// "When" cell for a history row: the block's date/time once mined (falling
// back to the height if the timestamp couldn't be resolved), or an amber
// "pending" chip while still in the mempool. `nowrap` keeps it on one line.
const histWhen = (e: { timestamp: number | null; height: number | null }, tz?: string | null) => {
  const mono = { fontFamily: "var(--font-mono)", fontSize: 11.5, color: "var(--fg-3)", whiteSpace: "nowrap" };
  if (e.timestamp) return <span style={mono}>{fmtWhen(e.timestamp, tz)}</span>;
  if (e.height != null) return <span style={mono}>#{e.height}</span>;
  return (
    <span className="tag-amber">
      <span className="dot" style={{ background: "var(--amber-400)" }}></span> pending
    </span>
  );
};

// Per-row signature glyph: verified ✓ (green), invalid ✗ (red), or
// awaiting-confirmation (amber clock) for not-yet-on-chain writes.
const histSig = (v: boolean | null | undefined) =>
  v === true ? (
    <Icon name="shield-check" size={14} color="var(--green-500)" />
  ) : v === false ? (
    <Icon name="shield-alert" size={14} color="var(--red-500)" />
  ) : (
    <Icon name="clock" size={14} color="var(--amber-400)" />
  );

// The one true confirmation-status indicator, shared by every detail pane
// (Browse + History headers and their On-chain "Status" rows) so "confirmed"
// reads identically everywhere: a green shield for confirmed, an amber dot for
// the in-flight states. `s` is { kind, done, required }; inherits the caller's
// font (mono in headers, sans in kv-rows).
const statusChip = (s: KeyStatus | null | undefined) => {
  const k = (s && s.kind) || "confirmed";
  if (k === "confirmed") {
    return (
      <span style={{ display: "inline-flex", alignItems: "center", gap: 5, color: "var(--green-500)" }}>
        <Icon name="shield-check" size={12} /> confirmed
      </span>
    );
  }
  const label =
    k === "confirming" ? `confirming · ${s!.done}/${s!.required}` : k === "deleting" ? "deleting" : "pending";
  return (
    <span style={{ display: "inline-flex", alignItems: "center", gap: 5, color: "var(--amber-500)" }}>
      <span className="dot" style={{ background: "var(--amber-400)" }}></span> {label}
    </span>
  );
};

// Browse "Last update" cell: the block time of the latest confirmed write,
// or an amber "now (…)" chip while a write to the key is still in flight.
const lastUpdateCell = (r: KeyRow, tz?: string | null) => {
  const s = r.status || ({} as KeyStatus);
  const chip = (label: string) => (
    <span className="tag-amber">
      <span className="dot" style={{ background: "var(--amber-400)" }}></span> {label}
    </span>
  );
  if (s.kind === "confirming") return chip(`now (confirming ${s.done}/${s.required})`);
  if (s.kind === "pending") return chip("now (pending)");
  if (s.kind === "deleting") return chip("now (deleting)");
  return r.updated_at ? (
    <span
      title={fmtWhen(r.updated_at, tz)}
      style={{ fontSize: 12, color: "var(--fg-3)", whiteSpace: "nowrap" }}
    >
      {fmtAgo(r.updated_at)}
    </span>
  ) : (
    <span style={{ color: "var(--fg-3)" }}>—</span>
  );
};

// Roles "Scope" cell: owners hold full authority (no scope); writers show
// their capability tokens as chips. Reads are public to anyone with the
// UFVK, so there is no read capability to show.
const roleScopeCell = (r: RoleRow) => {
  if (r.role === "owner") {
    return <span style={{ color: "var(--fg-3)", fontStyle: "italic" }}>full authority</span>;
  }
  const caps = r.capabilities || [];
  if (caps.length === 0) return <span style={{ color: "var(--fg-3)" }}>—</span>;
  return (
    <span style={{ display: "inline-flex", gap: 4, flexWrap: "wrap" }}>
      {caps.map((c) => (
        <span key={c} className="cap-chip">{c}</span>
      ))}
    </span>
  );
};

// Revoked-role "Scope" cell: the date the authority was revoked. Owners show
// no scope; writers show the (now-defunct) capability chips they last held.
const revokedScopeCell = (r: RevokedRoleRow, tz?: string | null) => {
  const when =
    r.timestamp != null
      ? fmtWhen(r.timestamp, tz)
      : r.height != null
        ? "#" + r.height
        : "unknown time";
  const caps = r.role === "writer" ? r.capabilities || [] : [];
  return (
    <span style={{ display: "inline-flex", gap: 6, flexWrap: "wrap", alignItems: "center" }}>
      {caps.map((c) => (
        <span key={c} className="cap-chip" style={{ opacity: 0.5, textDecoration: "line-through" }}>
          {c}
        </span>
      ))}
      <span style={{ fontFamily: "var(--font-mono)", fontSize: 11, color: "var(--fg-3)", whiteSpace: "nowrap" }}>
        revoked {when}
      </span>
    </span>
  );
};

// Funding "Amount" cell: the signed value transferred (fee excluded), green
// with a leading + for received, red with a − for sent, in the db's currency.
// A self-send (`direction === "self"`) reads like a send of its net cost (the
// fee (librustzcash's balance delta), with the gross amount in the detail.
const fundingAmount = (t: FundingTxResp, network: string | null) => {
  const received = t.direction === "received";
  return (
    <span
      style={{
        fontFamily: "var(--font-mono)",
        fontSize: 12,
        fontWeight: 500,
        color: received ? "var(--green-500)" : "var(--red-500)",
        whiteSpace: "nowrap",
      }}
    >
      {received ? "+" : "−"}
      {window.formatZats(t.amount, network)}
    </span>
  );
};

// First-import sync gate. Shown across every tab while the open database is
// still being scanned from its birthday to the chain tip, before we can be
// certain whether a valid INIT exists. A partial scan can read "uninitialized"
// only because the INIT block isn't reached yet, so revealing the panels early
// flickers "not initialized" and then the real state; this holds a single
// syncing view until the verdict is final. Progress is derived from the db's
// birthday (start), its locally scanned height (current), and the live tip.
const FirstSyncPanel = ({ detail, chainTip }: any) => {
  const bday = (detail && detail.birthday) || 0;
  const tip = chainTip || 0;
  const synced = (detail && detail.synced) || 0;
  const span = tip - bday;
  // Prefer the wallet's note-count scan ratio over the contiguous `synced`
  // frontier: the scanner works out of priority order (tip region first, then
  // the historic sweep), so `synced` can sit at the birthday for most of a
  // first import while real progress is being committed. The ratio grows with
  // every committed batch; map it onto the block span for the height readout.
  const sp = detail && detail.scan_progress;
  const ratio = sp && sp[1] > 0 ? Math.min(1, sp[0] / sp[1]) : null;
  let cur = tip > 0 ? Math.min(tip, Math.max(synced, bday)) : synced;
  if (ratio != null && tip > 0 && span > 0) {
    cur = Math.max(cur, Math.min(tip, bday + Math.round(ratio * span)));
  }
  const pct = span > 0 ? Math.max(0, Math.min(100, Math.round(((cur - bday) / span) * 100))) : 0;
  return (
    <div className="dt-wrap">
      <div className="empty" style={{ padding: "64px 24px" }}>
        {/* Single child so `.empty`'s grid centers the whole block as one unit
            instead of stretching the glyph and text into two tall rows. */}
        <div>
          <div className="glyph" style={{ marginBottom: 4 }}>
            <Icon name="loader" size={28} />
          </div>
          <strong style={{ color: "var(--fg-1)" }}>
            {tip > 0
              ? `Syncing ${cur.toLocaleString()} / ${tip.toLocaleString()} (${pct}%)…`
              : "Starting sync…"}
          </strong>
          <div style={{ marginTop: 6, color: "var(--fg-3)", fontSize: 13, maxWidth: 360, marginLeft: "auto", marginRight: "auto" }}>
            Scanning the chain from this database's birthday to the tip to
            determine its state.
          </div>
          {tip > 0 && span > 0 && (
            <div
              style={{
                marginTop: 16,
                width: 280,
                maxWidth: "100%",
                height: 6,
                background: "var(--bg-sunken)",
                border: "1px solid var(--border-1)",
                borderRadius: 4,
                overflow: "hidden",
                marginLeft: "auto",
                marginRight: "auto",
              }}
            >
              <div
                style={{
                  width: pct + "%",
                  height: "100%",
                  background: "var(--amber-400)",
                  transition: "width 0.4s ease",
                }}
              />
            </div>
          )}
        </div>
      </div>
    </div>
  );
};

// ============ KEY LIST ============
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
  onSelectRole,
}: any) => {
  const isAdmin = db && db.role === "admin";
  const [menuIdx, setMenuIdx] = React.useState(-1);

  React.useEffect(() => {
    if (menuIdx < 0) return;
    const close = () => setMenuIdx(-1);
    window.addEventListener("click", close);
    return () => window.removeEventListener("click", close);
  }, [menuIdx]);

  return (
    <div className="main">
      <div className="main-toolbar">
        {tab !== "roles" && (
          <div className="kv-filter" style={{ position: "relative" }}>
            <Icon name="search" size={12} style={{ position: "absolute", left: 9, top: 8, color: "var(--fg-3)" }} />
            <input
              className="input"
              style={{ width: 240, maxWidth: "100%", paddingLeft: 28 }}
              placeholder="filter by key…"
              value={filter}
              onChange={(e) => onFilter(e.target.value)}
            />
            {filter && (
              <button
                className="btn ghost sm"
                style={{ position: "absolute", right: 2, top: 2, height: 24, width: 24, padding: 0 }}
                onClick={() => onFilter("")}
              >
                <Icon name="x" size={12} />
              </button>
            )}
          </div>
        )}
        <span style={{ marginLeft: "auto" }}></span>
        <button
          className="btn secondary sm"
          onClick={onTogglePause}
          title={paused ? "Resume continuous syncing for this database" : "Pause continuous syncing for this database"}
        >
          {paused ? <Icon name="play" className="icon" /> : <PauseGlyph className="icon" />}{" "}
          {/* Paused: the compact "Resume" + "Sync" pair (the sync button only
              shows while paused), saving space. Live: the responsive
              "Pause Syncing"/"Pause" label. */}
          {paused ? (
            "Resume"
          ) : (
            <span className="btn-label">
              <span className="lbl-full">Pause Syncing</span>
              <span className="lbl-short">Pause</span>
            </span>
          )}
        </button>
        {paused && (
          <button
            className="btn secondary sm"
            onClick={onManualSync}
            disabled={manualSyncing}
            title="Sync this database once now"
          >
            {manualSyncing ? <div className="spinner" /> : <Icon name="refresh-cw" className="icon" />}{" "}
            Sync
          </button>
        )}
        {/* While the first birthday->tip scan is still running the init
            verdict is provisional (an INIT may sit in not-yet-scanned
            blocks), so offering Initialize would invite a double-INIT;
            hide the primary action until the gate settles. */}
        {isAdmin && !firstSyncPending && (
          (detail && detail.init === "uninitialized") ? (
            <button className="btn primary sm" onClick={() => onWriteKey(null)} title="Broadcast INIT to open this database for writes">
              <Icon name="zap" className="icon" /> Initialize
            </button>
          ) : (detail && detail.init === "initializing") ? (
            <button className="btn primary sm" disabled title="INIT is confirming">
              <div className="spinner" /> Initializing…
            </button>
          ) : (
            <button className="btn primary sm" onClick={() => onWriteKey(null)}>
              <Icon name="plus" className="icon" /> Set key
            </button>
          )
        )}
      </div>

      <div className="kv-tabs">
        <button className={tab === "browse" ? "on" : ""} onClick={() => onTab("browse")}>
          Browse
        </button>
        <button className={tab === "history" ? "on" : ""} onClick={() => onTab("history")}>
          History
        </button>
        <button className={tab === "roles" ? "on" : ""} onClick={() => onTab("roles")}>
          Roles
        </button>
        {isAdmin && (
          <button className={tab === "funding" ? "on" : ""} onClick={() => onTab("funding")}>
            Funding
          </button>
        )}
        <button className={tab === "rejections" ? "on" : ""} onClick={() => onTab("rejections")}>
          Rejections
        </button>
      </div>

      {firstSyncPending && <FirstSyncPanel detail={detail} chainTip={chainTip} />}

      {!firstSyncPending && tab === "browse" && (
        <div className="dt-wrap">
          <table className="dt">
            <thead>
              <tr>
                <th style={{ width: "30%" }}>Key</th>
                <th>Value</th>
                <th style={{ width: 150 }}>Updated</th>
                <th style={{ width: 34 }}></th>
              </tr>
            </thead>
            <tbody>
              {rows.map((r: KeyRow, i: number) => (
                <tr key={r.key} className={i === selectedIdx ? "selected" : ""} onClick={() => { onSelect(i); setMenuIdx(-1); }}>
                  <td>
                    <span className="key">{r.key}</span>
                  </td>
                  <td
                    style={{
                      color: r.deleted ? "var(--fg-3)" : "var(--fg-1)",
                      whiteSpace: "nowrap",
                      overflow: "hidden",
                      textOverflow: "ellipsis",
                      // width:100% + max-width:0 makes this the column that
                      // absorbs the table's leftover width and truncates;
                      // without the width the slack inflates the fixed columns.
                      width: "100%",
                      maxWidth: 0,
                      fontStyle: r.deleted ? "italic" : "normal",
                    }}
                  >
                    {r.deleted ? "(deleting)" : r.value}
                  </td>
                  <td>{lastUpdateCell(r, timeZone)}</td>
                  <td style={{ textAlign: "right", position: "relative" }}>
                    {isAdmin ? (
                      <>
                        <button
                          className="btn ghost sm"
                          title="Actions"
                          style={{ width: 24, height: 24, padding: 0 }}
                          onClick={(e) => {
                            e.stopPropagation();
                            setMenuIdx((m) => (m === i ? -1 : i));
                          }}
                        >
                          <Icon name="more-horizontal" size={14} />
                        </button>
                        {menuIdx === i && (
                          <div className="row-menu" onClick={(e) => e.stopPropagation()}>
                            <button onClick={() => { setMenuIdx(-1); onWriteKey(r); }}>
                              <Icon name="edit-3" size={14} /> Set new value
                            </button>
                            <button className="danger" onClick={() => { setMenuIdx(-1); onDelete(r); }}>
                              <Icon name="trash-2" size={14} /> Delete key
                            </button>
                          </div>
                        )}
                      </>
                    ) : (
                      <Icon name="more-horizontal" size={13} color="var(--fg-3)" />
                    )}
                  </td>
                </tr>
              ))}
              {rows.length === 0 && (() => {
                const init = detail && detail.init;
                const remaining = Math.max(0, (detail?.init_required || 0) - (detail?.init_done || 0));
                const glyph = loading ? "loader"
                  : init === "uninitialized" ? "zap"
                  : init === "initializing" ? "loader"
                  : "inbox";
                return (
                <tr className="empty-row">
                  <td colSpan={4}>
                    <div className="empty" style={{ padding: "48px 24px" }}>
                      <div className="glyph">
                        <Icon name={glyph} size={28} />
                      </div>
                      {loading ? (
                        <div>Loading keys…</div>
                      ) : filter ? (
                        <div>
                          No keys matching{" "}
                          <code style={{ fontFamily: "var(--font-mono)", color: "var(--amber-400)" }}>"{filter}"</code> in
                          this database.
                        </div>
                      ) : init === "uninitialized" ? (
                        <div>
                          <strong style={{ color: "var(--fg-1)" }}>Database not initialized.</strong>
                          {isAdmin && (
                            <div style={{ marginTop: 14 }}>
                              <button className="btn primary sm" onClick={() => onWriteKey(null)}>
                                <Icon name="zap" className="icon" /> Initialize database
                              </button>
                            </div>
                          )}
                        </div>
                      ) : init === "initializing" ? (
                        <div>
                          <strong style={{ color: "var(--fg-1)" }}>Database initializing…</strong>
                          <div style={{ marginTop: 6, color: "var(--fg-3)", fontSize: 13, maxWidth: 320 }}>
                            {remaining} confirmation{remaining === 1 ? "" : "s"} remaining. You can write keys once the INIT confirms.
                          </div>
                        </div>
                      ) : (
                        <div>
                          No keys yet.
                          {isAdmin && (
                            <div style={{ marginTop: 14 }}>
                              <button className="btn primary sm" onClick={() => onWriteKey(null)}>
                                <Icon name="plus" className="icon" /> Write your first key
                              </button>
                            </div>
                          )}
                        </div>
                      )}
                    </div>
                  </td>
                </tr>
                );
              })()}
            </tbody>
          </table>
        </div>
      )}

      {!firstSyncPending && tab === "history" && (
        <>
          <div className="dt-wrap">
          <table className="dt">
            <thead>
              <tr>
                <th style={{ width: 64 }}>OP</th>
                <th style={{ width: "26%" }}>Key</th>
                <th>Value</th>
                <th style={{ width: 200, whiteSpace: "nowrap" }}>When</th>
                <th style={{ width: 48, textAlign: "center" }}>Sig</th>
              </tr>
            </thead>
            <tbody>
              {history.map((e: HistoryEntryResp, i: number) => (
                <tr
                  key={e.txid + ":" + e.output_index + ":" + i}
                  className={i === selectedHistoryIdx ? "selected" : ""}
                  onClick={() => onSelectHistory(i)}
                >
                  <td>
                    <span className={"op " + e.op.toLowerCase()}>{e.op}</span>
                  </td>
                  <td>
                    {e.op === "INIT" ? (
                      <span style={{ color: "var(--fg-3)", fontStyle: "italic" }}>database created</span>
                    ) : (
                      <span className="key">{e.key}</span>
                    )}
                  </td>
                  <td
                    style={{
                      color: e.op === "DEL" ? "var(--fg-3)" : "var(--fg-1)",
                      whiteSpace: "nowrap",
                      overflow: "hidden",
                      textOverflow: "ellipsis",
                      // Absorbs leftover width + truncates (see Browse note).
                      width: "100%",
                      maxWidth: 0,
                      fontStyle: e.op === "DEL" ? "italic" : "normal",
                    }}
                  >
                    {e.op === "DEL" ? "(deleted)" : e.op === "INIT" ? "" : e.value}
                  </td>
                  <td>{histWhen(e, timeZone)}</td>
                  <td style={{ textAlign: "center" }}>{histSig(e.verified)}</td>
                </tr>
              ))}
              {history.length === 0 && (
                <tr className="empty-row">
                  <td colSpan={5}>
                    <div className="empty" style={{ padding: "48px 24px" }}>
                      <div className="glyph">
                        <Icon name={historyLoading ? "loader" : "history"} size={28} />
                      </div>
                      {historyLoading ? (
                        <div>Loading write history…</div>
                      ) : filter ? (
                        <div>
                          No writes to keys matching{" "}
                          <code style={{ fontFamily: "var(--font-mono)", color: "var(--amber-400)" }}>"{filter}"</code>.
                        </div>
                      ) : detail && detail.init === "uninitialized" ? (
                        <div>
                          <strong style={{ color: "var(--fg-1)" }}>Database not initialized.</strong>
                        </div>
                      ) : detail && detail.init === "initializing" ? (
                        <div>
                          <strong style={{ color: "var(--fg-1)" }}>Database initializing…</strong>
                          <div style={{ marginTop: 6, color: "var(--fg-3)", fontSize: 13, maxWidth: 340 }}>
                            The INIT has been broadcast and is confirming. It will appear here once the wallet sees it.
                          </div>
                        </div>
                      ) : (
                        <div>
                          No writes yet. Every change shows up here the moment it's broadcast.
                        </div>
                      )}
                    </div>
                  </td>
                </tr>
              )}
            </tbody>
          </table>
          </div>
          <PaginationBar
            total={historyTotal}
            offset={historyOffset}
            pageSize={historyPageSize}
            onPage={onHistoryPage}
            loading={historyLoading}
          />
        </>
      )}

      {!firstSyncPending && tab === "rejections" && (
        <div className="dt-wrap">
          <table className="dt">
            <thead>
              <tr>
                <th style={{ width: 200, whiteSpace: "nowrap" }}>When</th>
                <th style={{ width: 220 }}>TXID</th>
                <th>Rejection Reason</th>
              </tr>
            </thead>
            <tbody>
              {(rejections || []).map((e: RejectionResp, i: number) => (
                <tr
                  key={(e.txid || "") + ":" + i}
                  className={i === selectedRejectionIdx ? "selected" : ""}
                  onClick={() => onSelectRejection(i)}
                >
                  <td>{histWhen(e, timeZone)}</td>
                  <td style={{ fontFamily: "var(--font-mono)", fontSize: 12, color: "var(--fg-2)" }}>
                    {e.txid ? (
                      e.txid.length > 26 ? `${e.txid.slice(0, 16)}…${e.txid.slice(-8)}` : e.txid
                    ) : (
                      <span style={{ color: "var(--fg-3)" }}>—</span>
                    )}
                  </td>
                  <td
                    style={{
                      color: "var(--amber-400)",
                      fontSize: 12.5,
                      whiteSpace: "nowrap",
                      overflow: "hidden",
                      textOverflow: "ellipsis",
                      // Absorbs leftover width + truncates (see Browse note).
                      width: "100%",
                      maxWidth: 0,
                    }}
                  >
                    <Icon
                      name="shield-alert"
                      size={13}
                      style={{ verticalAlign: "-2px", marginRight: 5 }}
                    />
                    {e.reason}
                  </td>
                </tr>
              ))}
              {(!rejections || rejections.length === 0) && (
                <tr className="empty-row">
                  <td colSpan={3}>
                    <div className="empty" style={{ padding: "48px 24px" }}>
                      <div className="glyph">
                        {/* Loader only on the cold first load; once we have a
                            result, background re-scans keep the steady state so
                            the panel doesn't flicker every poll. */}
                        <Icon name={rejectionsLoading && !rejections ? "loader" : "shield-check"} size={28} />
                      </div>
                      {rejectionsLoading && !rejections ? (
                        <div>Scanning the chain for rejected writes…</div>
                      ) : (
                        <div>Every memo in this database parsed and verified correctly.</div>
                      )}
                    </div>
                  </td>
                </tr>
              )}
            </tbody>
          </table>
        </div>
      )}

      {!firstSyncPending && tab === "roles" && (
        <div className="dt-wrap">
          <table className="dt">
            <thead>
              <tr>
                <th style={{ width: 120 }}>Role</th>
                <th>Public Key</th>
                <th style={{ width: "36%" }}>Scope</th>
              </tr>
            </thead>
            <tbody>
              {roles.map((r: RoleRow, i: number) => {
                const isCreator = rolesCreator && r.pubkey === rolesCreator;
                return (
                  <tr
                    key={r.role + ":" + r.pubkey}
                    className={i === selectedRoleIdx ? "selected" : ""}
                    onClick={() => onSelectRole(i)}
                  >
                    <td>
                      <span className={"role " + r.role}>
                        <Icon name={r.role === "owner" ? "shield-check" : "key-round"} size={11} />
                        {r.role}
                      </span>
                    </td>
                    <td>
                      <div style={{ display: "flex", alignItems: "center", gap: 8, minWidth: 0 }}>
                        <span className="hash-cell">{midTrunc(r.pubkey)}</span>
                        {isCreator && <span className="tag-self">creator</span>}
                        {isCreator && isAdmin && <span className="tag-self">you</span>}
                      </div>
                    </td>
                    <td>{roleScopeCell(r)}</td>
                  </tr>
                );
              })}
              {(rolesRevoked || []).map((r: RevokedRoleRow, j: number) => {
                const isCreator = rolesCreator && r.pubkey === rolesCreator;
                const idx = roles.length + j;
                return (
                  <tr
                    key={"revoked:" + r.role + ":" + r.pubkey}
                    className={"revoked-row" + (idx === selectedRoleIdx ? " selected" : "")}
                    onClick={() => onSelectRole(idx)}
                  >
                    <td>
                      <span className={"role revoked " + r.role}>
                        <Icon name="shield-off" size={11} />
                        revoked {r.role}
                      </span>
                    </td>
                    <td>
                      <div style={{ display: "flex", alignItems: "center", gap: 8, minWidth: 0 }}>
                        <span className="hash-cell">{midTrunc(r.pubkey)}</span>
                        {isCreator && <span className="tag-self">creator</span>}
                      </div>
                    </td>
                    <td>{revokedScopeCell(r, timeZone)}</td>
                  </tr>
                );
              })}
              {roles.length === 0 && (rolesRevoked || []).length === 0 && (
                <tr className="empty-row">
                  <td colSpan={3}>
                    <div className="empty" style={{ padding: "48px 24px" }}>
                      <div className="glyph">
                        <Icon name={rolesLoading ? "loader" : "users-round"} size={28} />
                      </div>
                      {rolesLoading ? (
                        <div>Loading roles…</div>
                      ) : (
                        <div>
                          No roles yet. Owners and scoped writers appear here once the database is
                          initialized and its{" "}
                          <code style={{ fontFamily: "var(--font-mono)", color: "var(--amber-400)" }}>INIT</code>{" "}
                          confirms.
                        </div>
                      )}
                    </div>
                  </td>
                </tr>
              )}
            </tbody>
          </table>
        </div>
      )}

      {!firstSyncPending && tab === "funding" && (
        <>
          <div className="dt-wrap">
            <table className="dt">
              <thead>
                <tr>
                  <th style={{ width: 200, whiteSpace: "nowrap" }}>When</th>
                  <th style={{ width: 160 }}>Amount</th>
                  <th>Memo</th>
                </tr>
              </thead>
              <tbody>
                {funding.map((t: FundingTxResp, i: number) => (
                  <tr
                    key={t.txid + ":" + i}
                    className={i === selectedFundingIdx ? "selected" : ""}
                    onClick={() => onSelectFunding(i)}
                  >
                    <td>{histWhen(t, timeZone)}</td>
                    <td>{fundingAmount(t, db && db.network)}</td>
                    <td
                      style={{
                        whiteSpace: "nowrap",
                        overflow: "hidden",
                        textOverflow: "ellipsis",
                        // Absorbs leftover width + truncates (see Browse note).
                        width: "100%",
                        maxWidth: 0,
                        color: t.memo ? "var(--fg-1)" : "var(--fg-3)",
                      }}
                    >
                      {t.memo || "—"}
                    </td>
                  </tr>
                ))}
                {funding.length === 0 && (
                  <tr className="empty-row">
                    <td colSpan={3}>
                      <div className="empty" style={{ padding: "48px 24px" }}>
                        <div className="glyph">
                          <Icon name={fundingLoading ? "loader" : "wallet"} size={28} />
                        </div>
                        {fundingLoading ? (
                          <div>Loading funding…</div>
                        ) : (
                          <div>
                            No funding transactions yet. ZEC sent to or from this
                            database's wallet shows up here.
                          </div>
                        )}
                      </div>
                    </td>
                  </tr>
                )}
              </tbody>
            </table>
          </div>
          <PaginationBar
            total={fundingTotal}
            offset={fundingOffset}
            pageSize={fundingPageSize}
            onPage={onFundingPage}
            loading={fundingLoading}
          />
        </>
      )}
    </div>
  );
};

// Floating bottom pagination bar, shown only when there are more rows than
// one page. Mirrors a database browser's footer.
const PaginationBar = ({ total, offset, pageSize, onPage, loading }: {
  total: number;
  offset: number;
  pageSize: number;
  onPage: (offset: number) => void;
  loading?: boolean;
}) => {
  if (!total || total <= pageSize) return null;
  const start = offset + 1;
  const end = Math.min(offset + pageSize, total);
  const atStart = offset <= 0;
  const atEnd = offset + pageSize >= total;
  const lastOffset = Math.max(0, Math.floor((total - 1) / pageSize) * pageSize);
  return (
    <div className="pagination-bar">
      <span className="pg-info">
        {start.toLocaleString()}–{end.toLocaleString()} of {total.toLocaleString()}
      </span>
      <span style={{ marginLeft: "auto" }}></span>
      <button className="btn ghost sm" disabled={atStart || loading} onClick={() => onPage(0)} title="First page">
        «
      </button>
      <button className="btn ghost sm" disabled={atStart || loading} onClick={() => onPage(offset - pageSize)} title="Previous page">
        ‹ Prev
      </button>
      <button className="btn ghost sm" disabled={atEnd || loading} onClick={() => onPage(offset + pageSize)} title="Next page">
        Next ›
      </button>
      <button className="btn ghost sm" disabled={atEnd || loading} onClick={() => onPage(lastOffset)} title="Last page">
        »
      </button>
    </div>
  );
};

// ============ KEY DETAIL ============
const KeyDetail = ({ row, db, timeZone, onWriteKey, onDelete, onCopy, onViewHistory, onOpenTxid, signer, roles, onOpenRole, loading }: {
  row: KeyRow | null;
  db: ActiveDb | null;
  timeZone?: string | null;
  onWriteKey: (row: KeyRow | null) => void;
  onDelete: (row: KeyRow) => void;
  onCopy: (s: string) => void;
  onViewHistory: (key: string) => void;
  onOpenTxid?: (key: string, txid: string | null) => void;
  signer: string | null;
  roles: RoleRow[] | null;
  onOpenRole?: (pk: string) => void;
  loading?: boolean;
}) => {
  if (!row) {
    // Mirror the table's loading affordance so the two panes never disagree:
    // one showing a spinner while the other invites a selection.
    return (
      <aside className="detail">
        <div className="empty">
          <div className="glyph">
            <Icon name={loading ? "loader" : "mouse-pointer-click"} size={28} />
          </div>
          <div>
            {loading
              ? "Loading…"
              : "Select a key to inspect its current value and on-chain write."}
          </div>
        </div>
      </aside>
    );
  }
  const isAdmin = db && db.role === "admin";
  const s = row.status || { kind: "confirmed", done: 0, required: 0 };

  return (
    <aside className="detail">
      <div className="detail-header">
        <div style={{ display: "flex", alignItems: "center", justifyContent: "space-between" }}>
          <span style={{ fontFamily: "var(--font-mono)", fontSize: 10, letterSpacing: "0.12em", textTransform: "uppercase", color: "var(--fg-3)" }}>
            KEY
          </span>
          <div style={{ display: "flex", gap: 4 }}>
            <button
              className="btn ghost sm"
              title="Copy metadata (YAML)"
              onClick={() => onCopy(keyYaml(row, db, signer, timeZone))}
            >
              <Icon name="copy" className="icon" />
            </button>
            {isAdmin && (
              <button className="btn ghost sm" title="Delete (broadcast DEL)" onClick={() => onDelete(row)}>
                <Icon name="trash-2" className="icon" />
              </button>
            )}
          </div>
        </div>
        <div className="title">{row.key}</div>
        <div style={{ display: "flex", alignItems: "center", gap: 8, marginTop: 10, fontFamily: "var(--font-mono)", fontSize: 11.5, color: "var(--fg-3)" }}>
          {statusChip(s)}
          <span style={{ opacity: 0.5 }}>·</span>
          <span>{fmtBytes(row.size)}</span>
        </div>
      </div>

      <div className="detail-section">
        <h4>Current value</h4>
        {row.value != null ? (
          <CopyableBlock text={row.value} onCopy={onCopy} />
        ) : (
          <div style={{ fontFamily: "var(--font-mono)", fontSize: 12.5, color: "var(--fg-3)", fontStyle: "italic" }}>
            (no confirmed value{row.deleted ? ", deletion in flight" : ""})
          </div>
        )}
        {isAdmin && !row.deleted && (
          <div style={{ display: "flex", gap: 8, marginTop: 10 }}>
            <button className="btn primary sm" onClick={() => onWriteKey(row)}>
              <Icon name="edit-3" className="icon" /> Set new value
            </button>
          </div>
        )}
      </div>

      <div className="detail-section">
        <h4>On-chain</h4>
        <div className="kv-row">
          <span className="label">Database</span>
          <span className="value">
            <CollapsibleString value={db!.address} onCopy={onCopy} />
          </span>
        </div>
        <div className="kv-row">
          <span className="label">Status</span>
          <span className="value">{statusChip(s)}</span>
        </div>
        <div className="kv-row">
          <span className="label">Last updated</span>
          <span className="value" title="On-chain block time of the latest write to this key">
            {row.updated_at ? fmtWhen(row.updated_at, timeZone) : <span style={{ color: "var(--fg-3)" }}>—</span>}
          </span>
        </div>
        <div className="kv-row">
          <span className="label">Updated by</span>
          <span className="value">
            <SignerLink pubkey={signer} role={roleOf(roles, signer)} onOpenRole={onOpenRole} />
          </span>
        </div>
        <div className="kv-row">
          <span className="label">TXID</span>
          <span className="value">
            {row.txid ? (
              <button
                className="hist-link"
                style={{ marginTop: 0, fontFamily: "var(--font-mono)", fontSize: 12 }}
                title="View this transaction in history"
                onClick={() => onOpenTxid && onOpenTxid(row.key, row.txid)}
              >
                {row.txid.length > 26 ? `${row.txid.slice(0, 16)}…${row.txid.slice(-8)}` : row.txid}
              </button>
            ) : (
              <span style={{ color: "var(--fg-3)" }}>—</span>
            )}
          </span>
        </div>
        {onViewHistory && (
          <button className="hist-link" onClick={() => onViewHistory(row.key)}>
            <Icon name="history" size={13} /> View full history for this key
          </button>
        )}
      </div>
    </aside>
  );
};

// ============ HISTORY DETAIL ============
// Per-write inspector: signature-verification banner, the value, the
// on-chain coordinates, and the raw signed memo as broadcast.
const HistoryDetail = ({ entry, creator, onCopy, timeZone, network, roles, onOpenRole }: {
  entry: HistoryEntryResp | null;
  creator: string | null;
  onCopy: (s: string) => void;
  timeZone?: string | null;
  network: string | null;
  roles: RoleRow[] | null;
  onOpenRole?: (pk: string) => void;
}) => {
  if (!entry) {
    return (
      <aside className="detail">
        <div className="empty">
          <div className="glyph">
            <Icon name="history" size={28} />
          </div>
          <div>Select a write to inspect its signature and raw on-chain memo.</div>
        </div>
      </aside>
    );
  }

  const v = entry.verified;
  // The key that authored *this* write (a delegated owner/writer in a
  // multi-signer database), falling back to the creator for older/pending
  // entries that don't carry a per-entry signer yet.
  const signer = entry.signer || creator;
  // Its role in the on-chain registry, so a verified write reads "Signed by
  // owner/writer" with a click-through to the Roles tab. Prefer the
  // backend-resolved per-entry role; fall back to a lookup in the loaded rows.
  const signerRole = entry.signer_role || roleOf(roles, signer);
  const banner =
    v === true
      ? {
          cls: "ok",
          icon: "shield-check",
          iconColor: "var(--green-500)",
          title: "Valid and authorized signature",
          // No prose: just who signed it, as a clickable role-tagged chip.
          sub: <SignerLink pubkey={signer} role={signerRole} onOpenRole={onOpenRole} />,
        }
      : v === false
      ? {
          cls: "bad",
          icon: "shield-alert",
          iconColor: "var(--amber-500)",
          title: "Invalid or unauthorized signature",
          sub: "Readers drop this write, it never enters state.",
        }
      : {
          cls: "bad",
          icon: "clock",
          iconColor: "var(--amber-400)",
          title: "Awaiting confirmation",
          sub: "Verified once it's confirmed on-chain.",
        };

  const s = entry.status || {};

  return (
    <aside className="detail">
      <div className="detail-header">
        <div style={{ display: "flex", alignItems: "center", justifyContent: "space-between" }}>
          <span style={{ fontFamily: "var(--font-mono)", fontSize: 10, letterSpacing: "0.12em", textTransform: "uppercase", color: "var(--fg-3)" }}>
            {entry.op === "DEL" ? "DELETE" : "WRITE"}
          </span>
          <div style={{ display: "flex", gap: 4 }}>
            <button
              className="btn ghost sm"
              title="Copy metadata (YAML)"
              onClick={() => onCopy(historyYaml(entry, signer, network, timeZone))}
            >
              <Icon name="copy" className="icon" />
            </button>
          </div>
        </div>
        <div className="title">{entry.op === "INIT" ? "Database created" : entry.key}</div>
        <div style={{ display: "flex", alignItems: "center", gap: 8, marginTop: 10, fontFamily: "var(--font-mono)", fontSize: 11.5, color: "var(--fg-3)" }}>
          <span className={"op " + entry.op.toLowerCase()}>{entry.op}</span>
          {statusChip(s)}
        </div>
      </div>

      <div className="detail-section">
        <h4>{entry.op === "INIT" ? "Database claim" : entry.op === "DEL" ? "Deleted value" : "Value written"}</h4>
        {entry.op === "INIT" ? (
          <div style={{ fontSize: 12.5, color: "var(--fg-2)", lineHeight: 1.5 }}>
            The signed INIT that claims this database on-chain. Required before any writes.
          </div>
        ) : entry.op === "DEL" ? (
          <div style={{ fontFamily: "var(--font-mono)", fontSize: 12.5, color: "var(--fg-3)", fontStyle: "italic" }}>
            (this write removed the key)
          </div>
        ) : entry.value != null ? (
          <CopyableBlock text={entry.value} onCopy={onCopy} />
        ) : (
          <div style={{ fontFamily: "var(--font-mono)", fontSize: 12.5, color: "var(--fg-3)", fontStyle: "italic" }}>
            (no value)
          </div>
        )}
      </div>

      <div className="detail-section">
        <h4>On-chain</h4>
        <div className="kv-row">
          <span className="label">Timestamp</span>
          <span className="value">
            {entry.timestamp ? fmtWhen(entry.timestamp, timeZone) : "—"}
          </span>
        </div>
        <div className="kv-row">
          <span className="label">Height</span>
          <span className="value">{entry.height != null ? "#" + entry.height : "mempool"}</span>
        </div>
        <div className="kv-row">
          <span className="label">Status</span>
          <span className="value">{statusChip(s)}</span>
        </div>
        <div className="kv-row">
          <span className="label">TXID</span>
          <span className="value">
            {entry.txid ? (
              <CollapsibleString value={entry.txid} onCopy={onCopy} />
            ) : (
              <span style={{ color: "var(--fg-3)" }}>—</span>
            )}
          </span>
        </div>
        {/* Output value is 0 for a plain zkv memo write (hidden as noise), but
            a write can also carry ZEC (a tip/deposit broadcast with the memo);
            show that nonzero value on its own row, distinct from the fee. */}
        {entry.output_value != null && entry.output_value > 0 && (
          <div className="kv-row">
            <span className="label">Output</span>
            <span className="value" title="ZEC value carried by this write's output (broadcast alongside the memo)">
              {window.formatZats(entry.output_value, network)}
            </span>
          </div>
        )}
        <div className="kv-row">
          <span className="label">Fee</span>
          <span className="value" title="Actual fee paid for this transaction">
            {entry.fee != null ? window.formatZats(entry.fee, network) : <span style={{ color: "var(--fg-3)" }}>—</span>}
          </span>
        </div>
        <div className="kv-row">
          <span className="label">Signer</span>
          <span className="value">
            <SignerLink pubkey={signer} role={signerRole} onOpenRole={onOpenRole} />

          </span>
        </div>
        <div className="kv-row">
          <span className="label" title="Replay-protection sequence this write referenced on the wire">Sequence</span>
          <span className="value">
            {entry.seq != null ? entry.seq : <span style={{ color: "var(--fg-3)" }}>—</span>}
          </span>
        </div>
        {entry.signature && (
          <div className="kv-row">
            <span className="label">Signature</span>
            <span className="value">
              <CollapsibleString value={entry.signature} onCopy={onCopy} />
            </span>
          </div>
        )}
      </div>

      <div className="detail-section">
        <h4>Raw memo</h4>
        {entry.memo ? (
          <CopyableBlock text={entry.memo} onCopy={onCopy} />
        ) : (
          <div style={{ fontFamily: "var(--font-mono)", fontSize: 12.5, color: "var(--fg-3)", fontStyle: "italic" }}>
            (appears once it's on-chain)
          </div>
        )}
      </div>

      <div className="detail-section">
        <div className={"verify-banner " + banner.cls}>
          <Icon name={banner.icon} size={18} color={banner.iconColor} />
          <div>
            <div className="vb-title">{banner.title}</div>
            <div className="vb-sub">{banner.sub}</div>
          </div>
        </div>
      </div>
    </aside>
  );
};

// ============ REJECTION DETAIL ============
// Right-pane inspector for one rejected write: the raw memo exactly as it
// was broadcast, the full reason replay dropped it, and the on-chain
// coordinates. Mirrors HistoryDetail so the two panes feel the same.
const RejectionDetail = ({ entry, onCopy, timeZone }: {
  entry: RejectionResp | null;
  onCopy: (s: string) => void;
  timeZone?: string | null;
}) => {
  if (!entry) {
    return (
      <aside className="detail">
        <div className="empty">
          <div className="glyph">
            <Icon name="shield-alert" size={28} />
          </div>
          <div>Select a rejected write to inspect the raw broadcast and why it was dropped.</div>
        </div>
      </aside>
    );
  }

  const title = entry.op || "Unparseable memo";
  return (
    <aside className="detail">
      <div className="detail-header">
        <span style={{ fontFamily: "var(--font-mono)", fontSize: 10, letterSpacing: "0.12em", textTransform: "uppercase", color: "var(--fg-3)" }}>
          REJECTED
        </span>
        <div className="title">{title}</div>
        <div style={{ display: "flex", alignItems: "center", gap: 8, marginTop: 10, fontFamily: "var(--font-mono)", fontSize: 11.5, color: "var(--fg-3)" }}>
          <span className={"op " + (entry.op ? entry.op.toLowerCase() : "unknown")}>{entry.op || "?"}</span>
          <span>dropped during replay</span>
        </div>
      </div>

      <div className="detail-section">
        <h4>Raw broadcast</h4>
        {entry.raw ? (
          <CopyableBlock text={entry.raw} onCopy={onCopy} />
        ) : (
          <div style={{ fontFamily: "var(--font-mono)", fontSize: 12.5, color: "var(--fg-3)", fontStyle: "italic" }}>
            (memo text unavailable)
          </div>
        )}
      </div>

      <div className="detail-section">
        <h4>Rejection Reason</h4>
        {/* The reason leads the section and is copyable; the signature/authz
            breakdown follows underneath. */}
        <div className="verify-banner bad" style={{ marginBottom: 12 }}>
          <Icon name="shield-alert" size={18} color="var(--amber-500)" />
          <div style={{ flex: 1, minWidth: 0 }}>
            <div className="vb-title">{entry.reason}</div>
          </div>
          <button
            className="btn ghost sm"
            title="Copy error"
            onClick={() => onCopy(entry.reason)}
            style={{ flexShrink: 0, width: 24, height: 24, padding: 0 }}
          >
            <Icon name="copy" className="icon" />
          </button>
        </div>
        {/* Two axes: is the signature cryptographically valid, and was the
            signer authorized? A valid signature from an unauthorized signer
            shows Valid Signature ✓ / Authorized ✗. */}
        <div className="kv-row">
          <span className="label">Valid Signature</span>
          <span className="value" style={{ display: "inline-flex", alignItems: "center", gap: 6 }}>
            {entry.signature_valid ? (
              <>
                <Icon name="shield-check" size={14} color="var(--green-500)" />
                <span style={{ color: "var(--green-500)" }}>yes</span>
              </>
            ) : (
              <>
                <Icon name="shield-alert" size={14} color="var(--red-500)" />
                <span style={{ color: "var(--red-500)" }}>no</span>
              </>
            )}
          </span>
        </div>
        <div className="kv-row">
          <span className="label">Authorized</span>
          <span className="value" style={{ display: "inline-flex", alignItems: "center", gap: 6 }}>
            {entry.signature_valid ? (
              <>
                <Icon name="x-circle" size={14} color="var(--red-500)" />
                <span style={{ color: "var(--red-500)" }}>no</span>
              </>
            ) : (
              <span style={{ color: "var(--fg-3)" }}>— (no signer to authorize)</span>
            )}
          </span>
        </div>
        {entry.signature_valid && entry.signer && (
          <div className="kv-row">
            <span className="label">Signer</span>
            <span className="value">
              <CollapsibleString value={entry.signer} onCopy={onCopy} />
            </span>
          </div>
        )}
      </div>

      <div className="detail-section">
        <h4>On-chain</h4>
        <div className="kv-row">
          <span className="label">Timestamp</span>
          <span className="value">
            {entry.timestamp ? fmtWhen(entry.timestamp, timeZone) : "—"}
          </span>
        </div>
        <div className="kv-row">
          <span className="label">Height</span>
          <span className="value">{entry.height != null ? "#" + entry.height : "mempool"}</span>
        </div>
        <div className="kv-row">
          <span className="label">TXID</span>
          <span className="value">
            {entry.txid ? (
              <CollapsibleString value={entry.txid} onCopy={onCopy} />
            ) : (
              <span style={{ color: "var(--fg-3)" }}>—</span>
            )}
          </span>
        </div>
        {entry.op && (
          <div className="kv-row">
            <span className="label">Op</span>
            <span className="value">{entry.op}</span>
          </div>
        )}
        {entry.key != null && (
          <div className="kv-row">
            <span className="label">Key</span>
            <span className="value"><span className="key">{entry.key}</span></span>
          </div>
        )}
        {entry.value != null && entry.value !== "" && (
          <div className="kv-row">
            <span className="label">Value</span>
            <span className="value"><CollapsibleString value={entry.value} onCopy={onCopy} /></span>
          </div>
        )}
      </div>
    </aside>
  );
};

// ============ ROLE DETAIL ============
// Right-pane inspector for one role row (active or revoked): the public key,
// the authority it holds, when it was granted and by whom. For a revoked
// row, shows when/by-whom it was revoked, plus a creator badge for the INIT signer.
// Mirrors the other detail panes (selected row drives it, empty state otherwise).
const RoleDetail = ({ entry, db, creator, timeZone, onCopy, loading }: {
  entry: {
    pubkey: string;
    role: string;
    capabilities?: string[];
    revoked?: boolean;
    timestamp: number | null;
    height: number | null;
    granted_by?: string | null;
    revoked_by?: string | null;
  } | null;
  db: ActiveDb | null;
  creator: string | null;
  timeZone?: string | null;
  onCopy: (s: string) => void;
  loading?: boolean;
}) => {
  const eyebrow = {
    fontFamily: "var(--font-mono)",
    fontSize: 10,
    letterSpacing: "0.12em",
    textTransform: "uppercase",
    color: "var(--fg-3)",
  };
  if (!entry) {
    return (
      <aside className="detail">
        <div className="empty">
          <div className="glyph">
            <Icon name={loading ? "loader" : "users-round"} size={28} />
          </div>
          <div>
            {loading
              ? "Loading roles…"
              : "Select a role to see its authority, public key, and when it was granted."}
          </div>
        </div>
      </aside>
    );
  }

  const isAdmin = db && db.role === "admin";
  const isCreator = creator && entry.pubkey === creator;
  const revoked = !!entry.revoked;
  const isOwner = entry.role === "owner";
  const caps = entry.capabilities || [];
  const subtitle = revoked ? "no longer authorized" : isOwner ? "full authority" : "scoped writer";

  return (
    <aside className="detail">
      <div className="detail-header">
        <div style={{ display: "flex", alignItems: "center", justifyContent: "space-between" }}>
          <span style={eyebrow}>ROLE</span>
          <button className="btn ghost sm" title="Copy public key" onClick={() => onCopy(entry.pubkey)}>
            <Icon name="copy" className="icon" />
          </button>
        </div>
        <div className="title" style={{ display: "flex", alignItems: "center", gap: 8, flexWrap: "wrap" }}>
          <span className={"role " + (revoked ? "revoked " : "") + entry.role}>
            <Icon name={revoked ? "shield-off" : isOwner ? "shield-check" : "key-round"} size={12} />
            {(revoked ? "revoked " : "") + entry.role}
          </span>
          {isCreator && <span className="tag-self">creator</span>}
          {isCreator && isAdmin && <span className="tag-self">you</span>}
        </div>
        <div style={{ marginTop: 10, fontFamily: "var(--font-mono)", fontSize: 11.5, color: "var(--fg-3)" }}>
          {subtitle}
        </div>
      </div>

      <div className="detail-section">
        <h4>Public Key</h4>
        <CopyableBlock text={entry.pubkey} onCopy={onCopy} />
      </div>

      {!isOwner && (
        <div className="detail-section">
          <h4>Scope</h4>
          {caps.length ? (
            <span style={{ display: "inline-flex", gap: 6, flexWrap: "wrap" }}>
              {caps.map((c) => (
                <span
                  key={c}
                  className="cap-chip"
                  style={revoked ? { opacity: 0.5, textDecoration: "line-through" } : undefined}
                >
                  {c}
                </span>
              ))}
            </span>
          ) : (
            <span style={{ color: "var(--fg-3)" }}>—</span>
          )}
          <div style={{ fontSize: 12, color: "var(--fg-3)", lineHeight: 1.5, marginTop: 8 }}>
            A writer may write only within its scope. Reads are public to anyone holding the UFVK.
          </div>
        </div>
      )}

      <div className="detail-section">
        <h4>{revoked ? "Revoked" : "Granted"}</h4>
        <div className="kv-row">
          <span className="label">When</span>
          <span
            className="value"
            title={revoked ? "On-chain block time of the revoking op" : "On-chain block time of the grant"}
          >
            {whenAt(entry.timestamp, entry.height, timeZone)}
          </span>
        </div>
        <div className="kv-row">
          <span className="label">By</span>
          <span className="value">
            {revoked ? (
              entry.revoked_by ? (
                <CollapsibleString value={entry.revoked_by} onCopy={onCopy} />
              ) : (
                <span style={{ color: "var(--fg-3)" }}>—</span>
              )
            ) : isCreator ? (
              <span style={{ color: "var(--fg-3)" }}>self · INIT</span>
            ) : entry.granted_by ? (
              <CollapsibleString value={entry.granted_by} onCopy={onCopy} />
            ) : (
              <span style={{ color: "var(--fg-3)" }}>—</span>
            )}
          </span>
        </div>
        {isCreator && !revoked && (
          <div style={{ fontSize: 11.5, color: "var(--fg-3)", lineHeight: 1.5, marginTop: 8 }}>
            The UFVK-derived key that signed INIT, owner #1 the moment INIT confirmed,
            so this is the database's birth. The creator is a permanent trait, kept even
            if its owner authority is later revoked.
          </div>
        )}
      </div>
    </aside>
  );
};

// ============ FUNDING DETAIL ============
// Per-transaction inspector for the Funding tab: the signed amount, the
// recipient address(es) for a send, the fee, on-chain coordinates, and memo.
const FundingDetail = ({ tx, onCopy, timeZone, network, onOpenTxid }: {
  tx: FundingTxResp | null;
  onCopy: (s: string) => void;
  timeZone?: string | null;
  network: string | null;
  onOpenTxid?: (key: string, txid: string | null) => void;
}) => {
  if (!tx) {
    return (
      <aside className="detail">
        <div className="empty">
          <div className="glyph">
            <Icon name="wallet" size={28} />
          </div>
          <div>Select a transaction to see its amount, memo, and, for sends, the recipient.</div>
        </div>
      </aside>
    );
  }
  const self = tx.direction === "self";
  const received = tx.direction === "received";
  const isZkvOp = tx.direction === "zkv";
  const color = received ? "var(--green-500)" : "var(--red-500)";
  const amountStr = (received ? "+" : "−") + window.formatZats(tx.amount, network);
  // Mirror the wallet's ZIP-315 spendability: mined but under `required`
  // confirmations is still "confirming", not "confirmed" (a received deposit
  // needs 10, our own send/self 3). `statusChip` renders it like every other
  // detail pane.
  const fundStatus = (
    tx.pending
      ? { kind: "pending" }
      : tx.confirmed
      ? { kind: "confirmed" }
      : { kind: "confirming", done: tx.confirmations, required: tx.required }
  ) as KeyStatus;
  const hasRecipients = !received && !self && tx.recipients && tx.recipients.length > 0;
  return (
    <aside className="detail">
      <div className="detail-header">
        <div style={{ display: "flex", alignItems: "center", justifyContent: "space-between" }}>
          <span style={{ fontFamily: "var(--font-mono)", fontSize: 10, letterSpacing: "0.12em", textTransform: "uppercase", color: "var(--fg-3)" }}>
            {isZkvOp ? "ZKV OPERATION" : self ? "SENT TO SELF" : received ? "RECEIVED" : "SENT"}
          </span>
          <button className="btn ghost sm" title="Copy amount" onClick={() => onCopy(amountStr)}>
            <Icon name="copy" className="icon" />
          </button>
        </div>
        <div className="title" style={{ color, fontFamily: "var(--font-mono)" }}>{amountStr}</div>
        <div style={{ display: "flex", alignItems: "center", gap: 8, marginTop: 10, fontFamily: "var(--font-mono)", fontSize: 11.5, color: "var(--fg-3)" }}>
          {statusChip(fundStatus)}
        </div>
      </div>

      {hasRecipients && (
        <div className="detail-section">
          <h4>{tx.recipients.length > 1 ? "Recipients" : "Recipient"}</h4>
          {tx.recipients.map((r, i) => (
            <div className="kv-row" key={i}>
              <span className="value">
                <CollapsibleString value={r} onCopy={onCopy} />
              </span>
            </div>
          ))}
        </div>
      )}

      <div className="detail-section">
        <h4>On-chain</h4>
        <div className="kv-row">
          <span className="label">Amount</span>
          <span className="value" style={{ color }}>{amountStr}</span>
        </div>
        {self && tx.self_sent != null && (
          <div className="kv-row">
            <span className="label">Output</span>
            <span className="value" title="Value sent to one of your own addresses (returned to this wallet)">
              {window.formatZats(tx.self_sent, network)}
            </span>
          </div>
        )}
        <div className="kv-row">
          <span className="label">Timestamp</span>
          <span className="value">{tx.timestamp ? fmtWhen(tx.timestamp, timeZone) : "—"}</span>
        </div>
        <div className="kv-row">
          <span className="label">Height</span>
          <span className="value">{tx.height != null ? "#" + tx.height : "mempool"}</span>
        </div>
        <div className="kv-row">
          <span className="label">Status</span>
          <span className="value">{statusChip(fundStatus)}</span>
        </div>
        <div className="kv-row">
          <span className="label">Fee</span>
          <span className="value" title="Network fee paid by this wallet (sends only)">
            {tx.fee != null ? window.formatZats(tx.fee, network) : <span style={{ color: "var(--fg-3)" }}>—</span>}
          </span>
        </div>
        <div className="kv-row">
          <span className="label">TXID</span>
          <span className="value">
            {tx.txid ? (
              <CollapsibleString value={tx.txid} onCopy={onCopy} />
            ) : (
              <span style={{ color: "var(--fg-3)" }}>—</span>
            )}
          </span>
        </div>
      </div>

      {tx.is_zkv && tx.txid && onOpenTxid && (
        <div className="detail-section">
          <h4>zkv operation</h4>
          <div style={{ fontSize: 12.5, color: "var(--fg-2)", lineHeight: 1.5, marginBottom: 8 }}>
            {isZkvOp
              ? "This transaction is a zkv write; its only cost was the network fee."
              : "This transaction also carries a zkv write."}
          </div>
          <button
            className="hist-link"
            style={{ marginTop: 0, fontFamily: "var(--font-mono)", fontSize: 12 }}
            title="View this write in History"
            onClick={() => onOpenTxid("", tx.txid)}
          >
            View operation in History →
          </button>
        </div>
      )}

      {tx.memo && (
        <div className="detail-section">
          <h4>Memo</h4>
          <CopyableBlock text={tx.memo} onCopy={onCopy} />
        </div>
      )}
    </aside>
  );
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
