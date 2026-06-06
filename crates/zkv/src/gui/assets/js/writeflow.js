const sigLineOf = (memo) => {
  if (!memo) return "";
  const lines = memo.split("\n");
  return lines[lines.length - 1] || "";
};
const SignedMemo = ({ head, preview, card, style }) => {
  const [copied, setCopied] = React.useState(false);
  const ready = preview.status === "ready" && !!preview.sig;
  const copy = () => {
    if (!ready || !preview.memo) return;
    try {
      navigator.clipboard.writeText(preview.memo);
      setCopied(true);
      setTimeout(() => setCopied(false), 1200);
    } catch (_) {
    }
  };
  const sig = preview.sig || "";
  const sigShown = sig.length > 46 ? sig.slice(0, 30) + "\u2026" + sig.slice(-12) : sig;
  return /* @__PURE__ */ React.createElement("div", { className: "bcast-memo" + (card ? " card" : ""), style }, /* @__PURE__ */ React.createElement("div", { className: "bcast-memo-head" }, /* @__PURE__ */ React.createElement("span", { className: "lbl" }, "--- begin zkv memo ---"), /* @__PURE__ */ React.createElement(
    "button",
    {
      type: "button",
      className: "bcast-copy",
      disabled: !ready,
      title: ready ? "Copy the exact signed memo" : "Signing\u2026",
      onClick: copy
    },
    /* @__PURE__ */ React.createElement(Icon, { name: copied ? "check" : "copy", size: 11 }),
    " ",
    copied ? "Copied" : "Copy"
  )), head, /* @__PURE__ */ React.createElement("div", { className: "sig", style: ready ? { color: "var(--fg-2)" } : void 0, title: ready ? sig : void 0 }, ready ? sigShown : "(signature will go here)"), /* @__PURE__ */ React.createElement("div", null, /* @__PURE__ */ React.createElement("span", { className: "lbl" }, "--- end zkv memo ---")));
};
const WriteFlow = ({ db, prefillKey, prefillValue, mode = "set", synced, syncing, paused, onBroadcast, onInit, onSync, onClose, onDone, onDeposit }) => {
  const [step, setStep] = React.useState("form");
  const [keyName, setKeyName] = React.useState(prefillKey || "");
  const [val, setVal] = React.useState(prefillValue || "");
  const [progress, setProgress] = React.useState(0);
  const [confirmIdx, setConfirmIdx] = React.useState(0);
  const [txHash, setTxHash] = React.useState("");
  const [error, setError] = React.useState(null);
  const [initPhase, setInitPhase] = React.useState("idle");
  const [memoPreview, setMemoPreview] = React.useState({ status: "idle", memo: "", sig: "" });
  const needsInit = !!(db && db.init && db.init !== "initialized");
  const isInitializing = db && db.init === "initializing";
  const doInit = async () => {
    setError(null);
    setInitPhase("sending");
    try {
      const txid = await onInit();
      setTxHash(txid || "");
      setInitPhase("done");
    } catch (e) {
      setInitPhase("idle");
      setError(e);
    }
  };
  const [faucetInitState, setFaucetInitState] = React.useState("idle");
  const doFaucetInit = async () => {
    setFaucetInitState("requesting");
    setError(null);
    try {
      const r = await window.zkvApi.faucetInit(db.name);
      if (r.outcome === "outdated") {
        setFaucetInitState("outdated");
        return;
      }
      if (r.outcome !== "ok") {
        setFaucetInitState("retry");
        return;
      }
      setTxHash("");
      setInitPhase("done");
    } catch (_) {
      setFaucetInitState("retry");
    }
  };
  const faucetInitLabel = faucetInitState === "requesting" ? "Initializing\u2026" : faucetInitState === "retry" ? "Try again later" : faucetInitState === "outdated" ? "Your app is outdated" : "Use our faucet";
  const initMemoPreview = /* @__PURE__ */ React.createElement(
    SignedMemo,
    {
      card: true,
      style: { marginTop: 0 },
      preview: memoPreview,
      head: /* @__PURE__ */ React.createElement("div", { style: { wordBreak: "break-all" } }, "ZKV0 INIT ", /* @__PURE__ */ React.createElement("span", { className: "key" }, db.address || "<zkv address>"))
    }
  );
  const isDel = mode === "del";
  const keyBytes = new Blob([keyName]).size;
  const valBytes = new Blob([val]).size;
  const keyHasWhitespace = /\s/.test(keyName);
  const valHasNewline = /\n/.test(val);
  const usesSetl = !isDel && (valBytes === 0 || valHasNewline);
  const opLabel = isDel ? "DEL" : usesSetl ? "SETL" : "SET";
  const keyOk = keyName.length > 0 && !keyHasWhitespace;
  const lenDigits = String(valBytes).length;
  const memoBytes = isDel ? 140 + keyBytes : usesSetl ? 143 + keyBytes + lenDigits + valBytes : 141 + keyBytes + valBytes;
  const memoOk = memoBytes <= 511;
  const canSubmit = keyOk && memoOk;
  const FEE_FLOOR = 1e4;
  const lowFunds = db.balance != null && db.balance < FEE_FLOOR;
  const haveFunds = db.balance != null && !lowFunds;
  const costDotColor = !synced ? "var(--fg-3)" : haveFunds ? "var(--green-500)" : "var(--amber-300)";
  React.useEffect(() => {
    if (step !== "broadcasting") return;
    setConfirmIdx(0);
    setProgress(0);
    setError(null);
    setTxHash("");
    let cancelled = false;
    const ticks = [
      { at: 300, step: 1, prog: 0.18 },
      { at: 1100, step: 2, prog: 0.42 },
      { at: 2600, step: 3, prog: 0.72 }
    ];
    const timers = ticks.map((t) => setTimeout(() => {
      if (cancelled) return;
      setConfirmIdx(t.step);
      setProgress(t.prog);
    }, t.at));
    (async () => {
      try {
        const txid = await onBroadcast(mode, keyName, val);
        if (cancelled) return;
        setTxHash(txid || "");
        setConfirmIdx(5);
        setProgress(1);
        setStep("confirmed");
      } catch (e) {
        if (cancelled) return;
        setError(e);
      }
    })();
    return () => {
      cancelled = true;
      timers.forEach(clearTimeout);
    };
  }, [step]);
  const writeHead = usesSetl ? /* @__PURE__ */ React.createElement(React.Fragment, null, /* @__PURE__ */ React.createElement("div", null, "ZKV0 SETL ", /* @__PURE__ */ React.createElement("span", { className: "key" }, keyName), " ", valBytes), /* @__PURE__ */ React.createElement("div", null, val.length > 60 ? val.slice(0, 60) + "\u2026" : val)) : /* @__PURE__ */ React.createElement("div", null, "ZKV0 ", opLabel, " ", /* @__PURE__ */ React.createElement("span", { className: "key" }, keyName), !isDel && ` ${val.length > 60 ? val.slice(0, 60) + "\u2026" : val}`);
  React.useEffect(() => {
    if (needsInit) {
      if (initPhase !== "idle" || isInitializing) return;
      let cancelled2 = false;
      setMemoPreview({ status: "pending", memo: "", sig: "" });
      (async () => {
        try {
          const r = await window.zkvApi.signPreview(db.name, "init", "", "");
          if (!cancelled2) setMemoPreview({ status: "ready", memo: r.memo, sig: sigLineOf(r.memo) });
        } catch (_) {
          if (!cancelled2) setMemoPreview({ status: "error", memo: "", sig: "" });
        }
      })();
      return () => {
        cancelled2 = true;
      };
    }
    if (!canSubmit) {
      setMemoPreview({ status: "idle", memo: "", sig: "" });
      return;
    }
    setMemoPreview({ status: "pending", memo: "", sig: "" });
    let cancelled = false;
    const op = isDel ? "del" : "set";
    const timer = setTimeout(async () => {
      try {
        const r = await window.zkvApi.signPreview(db.name, op, keyName, isDel ? "" : val);
        if (!cancelled) setMemoPreview({ status: "ready", memo: r.memo, sig: sigLineOf(r.memo) });
      } catch (_) {
        if (!cancelled) setMemoPreview({ status: "error", memo: "", sig: "" });
      }
    }, 500);
    return () => {
      cancelled = true;
      clearTimeout(timer);
    };
  }, [db.name, needsInit, initPhase, isInitializing, canSubmit, isDel, keyName, val]);
  const heading = isDel ? `Delete key` : prefillKey ? `Update value` : `Set new key`;
  const sub = isDel ? `Broadcast a signed DEL memo. The key is removed once readers confirm it.` : `Sign and broadcast a SET memo. Readers see it after the confirmation depth.`;
  const FormStep = () => /* @__PURE__ */ React.createElement(React.Fragment, null, /* @__PURE__ */ React.createElement("div", { className: "modal-body" }, /* @__PURE__ */ React.createElement("div", { className: "write-fields" }, /* @__PURE__ */ React.createElement("div", { className: "field-block" }, /* @__PURE__ */ React.createElement("label", null, "Key"), /* @__PURE__ */ React.createElement(
    "input",
    {
      className: "input mono lg",
      value: keyName,
      onChange: (e) => setKeyName(e.target.value),
      disabled: isDel || !!prefillKey,
      title: prefillKey ? "Key is fixed when updating its value" : void 0,
      autoFocus: !isDel && !prefillKey
    }
  ), /* @__PURE__ */ React.createElement("div", { className: "byte-counter" }, /* @__PURE__ */ React.createElement("span", { className: keyOk ? "ok" : "err" }, keyName.length === 0 ? "required" : keyHasWhitespace ? "keys can\u2019t contain whitespace" : ""), /* @__PURE__ */ React.createElement("span", { style: { color: "var(--fg-3)" } }, keyBytes, " bytes"))), !isDel && /* @__PURE__ */ React.createElement("div", { className: "field-block" }, /* @__PURE__ */ React.createElement("label", null, "Value"), /* @__PURE__ */ React.createElement(
    "textarea",
    {
      className: "input mono",
      value: val,
      onChange: (e) => setVal(e.target.value),
      autoFocus: !!prefillKey
    }
  ), /* @__PURE__ */ React.createElement("div", { className: "byte-counter" }, /* @__PURE__ */ React.createElement("span", { className: !memoOk ? "err" : memoBytes > 470 ? "warn" : "ok" }, !memoOk ? "memo too large, shorten the key or value" : memoBytes > 470 ? "close to the 511-byte memo limit" : valBytes === 0 ? "empty value, sent as SETL" : valHasNewline ? "multi-line value, sent as SETL" : ""), /* @__PURE__ */ React.createElement("span", { style: { color: "var(--fg-3)" } }, valBytes, " bytes \xB7 memo ", memoBytes, " / 511"))), isDel && /* @__PURE__ */ React.createElement("div", { className: "callout-flow warn" }, /* @__PURE__ */ React.createElement(Icon, { name: "alert-triangle", size: 16, color: "var(--amber-400)" }), /* @__PURE__ */ React.createElement("div", null, /* @__PURE__ */ React.createElement("strong", { style: { color: "inherit" } }, "This broadcasts a permanent DEL memo."), " ", "Future reads will not return ", /* @__PURE__ */ React.createElement("code", null, keyName), ". Old values remain in chain history.")), /* @__PURE__ */ React.createElement("div", { style: { padding: "10px 14px", background: "var(--bg-sunken)", borderRadius: "var(--radius-md)", border: "1px solid var(--border-1)" } }, /* @__PURE__ */ React.createElement("div", { style: { display: "flex", justifyContent: "space-between", fontFamily: "var(--font-mono)", fontSize: 11, color: "var(--fg-3)", textTransform: "uppercase", letterSpacing: "0.08em", marginBottom: 6 } }, /* @__PURE__ */ React.createElement("span", null, "Tx preview"), /* @__PURE__ */ React.createElement("span", null, db.network)), /* @__PURE__ */ React.createElement("div", { style: { fontFamily: "var(--font-mono)", fontSize: 12, color: "var(--fg-2)", lineHeight: 1.6 } }, /* @__PURE__ */ React.createElement("div", null, "fee \xB7 ", /* @__PURE__ */ React.createElement("span", { style: { color: "var(--fg-1)" } }, "~", window.formatZats(FEE_FLOOR, db.network))), /* @__PURE__ */ React.createElement("div", null, "balance \xB7 ", /* @__PURE__ */ React.createElement("span", { style: { color: "var(--fg-1)" } }, db.balance != null ? window.formatZats(db.balance, db.network) : "\u2014")), /* @__PURE__ */ React.createElement("div", { style: { display: "flex", gap: 6, alignItems: "flex-start", minWidth: 0 } }, /* @__PURE__ */ React.createElement("span", { style: { flexShrink: 0 } }, "database address \xB7"), db.address ? /* @__PURE__ */ React.createElement(CollapsibleString, { value: db.address, onCopy: (t) => {
    try {
      navigator.clipboard.writeText(t);
    } catch {
    }
  } }) : /* @__PURE__ */ React.createElement("span", { style: { color: "var(--fg-1)" } }, "\u2014")))), /* @__PURE__ */ React.createElement(SignedMemo, { card: true, preview: memoPreview, head: writeHead }))), /* @__PURE__ */ React.createElement("div", { className: "modal-foot" }, /* @__PURE__ */ React.createElement("div", { className: "cost" }, lowFunds ? /* @__PURE__ */ React.createElement(React.Fragment, null, /* @__PURE__ */ React.createElement(Icon, { name: "alert-triangle", size: 11, color: "var(--red-500)" }), /* @__PURE__ */ React.createElement("span", { style: { color: "var(--red-500)" } }, "~", window.formatZats(FEE_FLOOR, db.network), " \xB7 insufficient funds"), onDeposit && /* @__PURE__ */ React.createElement("button", { className: "btn ghost sm", style: { marginLeft: 4 }, onClick: onDeposit }, /* @__PURE__ */ React.createElement(Icon, { name: "download", size: 12 }), " Deposit")) : /* @__PURE__ */ React.createElement(React.Fragment, null, /* @__PURE__ */ React.createElement("span", { className: "amber-dot", style: { background: costDotColor } }), /* @__PURE__ */ React.createElement("span", null, "Cost: ~", window.formatZats(FEE_FLOOR, db.network), " (network fee)"))), /* @__PURE__ */ React.createElement("div", { style: { display: "flex", gap: 8 } }, /* @__PURE__ */ React.createElement("button", { className: "btn secondary", onClick: onClose }, "Cancel"), /* @__PURE__ */ React.createElement("button", { className: "btn primary", disabled: !canSubmit || lowFunds, onClick: () => setStep("review") }, lowFunds ? "Insufficient funds" : "Review \u2192"))));
  const ReviewStep = () => /* @__PURE__ */ React.createElement(React.Fragment, null, /* @__PURE__ */ React.createElement("div", { className: "modal-body" }, /* @__PURE__ */ React.createElement("div", { style: { fontSize: 13, color: "var(--fg-2)", marginBottom: 14, lineHeight: 1.5 } }, "On ", /* @__PURE__ */ React.createElement("strong", { style: { color: "var(--fg-1)" } }, "Broadcast"), ", the write is signed locally with the seed for ", /* @__PURE__ */ React.createElement("strong", { style: { color: "var(--fg-1)" } }, db.name), " and sent as a memo. You only pay the network fee."), /* @__PURE__ */ React.createElement("div", { className: "broadcast-steps" }, /* @__PURE__ */ React.createElement("div", { className: "bcast-step done" }, /* @__PURE__ */ React.createElement("div", { className: "bcast-ic" }, /* @__PURE__ */ React.createElement(Icon, { name: "check", size: 12 })), /* @__PURE__ */ React.createElement("div", null, /* @__PURE__ */ React.createElement("strong", null, "Key & value validated")), /* @__PURE__ */ React.createElement("div", { className: "bcast-meta" }, memoBytes, "b")), /* @__PURE__ */ React.createElement("div", { className: "bcast-step " + (memoPreview.status === "ready" ? "done" : "pending") }, /* @__PURE__ */ React.createElement("div", { className: "bcast-ic" }, memoPreview.status === "ready" ? /* @__PURE__ */ React.createElement(Icon, { name: "check", size: 12 }) : /* @__PURE__ */ React.createElement("span", { style: { fontFamily: "var(--font-mono)", fontSize: 10 } }, "2")), /* @__PURE__ */ React.createElement("div", null, /* @__PURE__ */ React.createElement("strong", null, memoPreview.status === "ready" ? "Signed" : "Signed at broadcast"))), /* @__PURE__ */ React.createElement(SignedMemo, { preview: memoPreview, head: writeHead }))), /* @__PURE__ */ React.createElement("div", { className: "modal-foot" }, /* @__PURE__ */ React.createElement("div", { className: "cost" }, /* @__PURE__ */ React.createElement(Icon, { name: "zap", size: 11 }), /* @__PURE__ */ React.createElement("span", null, "Recipient: this database's Zcash Orchard address")), /* @__PURE__ */ React.createElement("div", { style: { display: "flex", gap: 8 } }, /* @__PURE__ */ React.createElement("button", { className: "btn secondary", onClick: () => setStep("form") }, "\u2190 Back"), /* @__PURE__ */ React.createElement("button", { className: "btn primary", onClick: () => setStep("broadcasting") }, /* @__PURE__ */ React.createElement(Icon, { name: "send", className: "icon" }), " Broadcast"))));
  const BroadcastStep = () => {
    const steps = [
      { label: "Memo signed", sub: "" },
      { label: "Transaction assembled", sub: "zero-value Orchard self-send carrying the memo" },
      { label: `Submitted to Zcash ${db.network}`, sub: "broadcast via lightwalletd" },
      { label: "Accepted to the mempool", sub: "awaiting a block" },
      { label: "Confirming", sub: "readers see it after the confirmation threshold" }
    ];
    if (error) {
      const code = error.code;
      return /* @__PURE__ */ React.createElement(React.Fragment, null, /* @__PURE__ */ React.createElement("div", { className: "modal-body" }, /* @__PURE__ */ React.createElement("div", { className: "callout-flow warn" }, /* @__PURE__ */ React.createElement(Icon, { name: "alert-triangle", size: 16, color: "var(--amber-400)" }), /* @__PURE__ */ React.createElement("div", null, /* @__PURE__ */ React.createElement("strong", { style: { color: "inherit" } }, code === "insufficient_funds" ? "Not enough funds to broadcast." : code === "not_initialized" ? "This database isn\u2019t initialized yet." : code === "watch_only" ? "This is a watch-only database." : "Broadcast failed."), /* @__PURE__ */ React.createElement(ErrorMessage, { message: error.message }), code === "insufficient_funds" && error.data && /* @__PURE__ */ React.createElement("div", { style: { marginTop: 6, fontFamily: "var(--font-mono)", fontSize: 11.5, color: "var(--fg-3)" } }, window.formatZats(error.data.available, db.network), " available \xB7", " ", window.formatZats(error.data.required, db.network), " needed"), code === "insufficient_funds" && (db.confirming || 0) > 0 && /* @__PURE__ */ React.createElement("div", { style: { marginTop: 4, fontSize: 12.5, color: "var(--fg-2)" } }, /* @__PURE__ */ React.createElement(Icon, { name: "clock", size: 12 }), " ", window.formatZats(db.confirming, db.network), " confirming, this becomes spendable in a few minutes. No need to deposit more.")))), /* @__PURE__ */ React.createElement("div", { className: "modal-foot" }, /* @__PURE__ */ React.createElement("div", { className: "cost" }, /* @__PURE__ */ React.createElement(Icon, { name: "x", size: 11 }), " ", /* @__PURE__ */ React.createElement("span", null, "not broadcast")), /* @__PURE__ */ React.createElement("div", { style: { display: "flex", gap: 8 } }, code === "insufficient_funds" && onDeposit && /* @__PURE__ */ React.createElement("button", { className: "btn secondary", onClick: onDeposit }, /* @__PURE__ */ React.createElement(Icon, { name: "download", className: "icon" }), " Deposit QR"), /* @__PURE__ */ React.createElement("button", { className: "btn secondary", onClick: onClose }, "Close"), /* @__PURE__ */ React.createElement("button", { className: "btn primary", onClick: () => setStep("form") }, "\u2190 Back to form"))));
    }
    return /* @__PURE__ */ React.createElement(React.Fragment, null, /* @__PURE__ */ React.createElement("div", { className: "modal-body" }, /* @__PURE__ */ React.createElement("div", { style: { fontSize: 13, color: "var(--fg-2)", marginBottom: 14, lineHeight: 1.5 } }, "Signing and broadcasting to ", /* @__PURE__ */ React.createElement("strong", { style: { color: "var(--fg-1)" } }, db.network), " via the local wallet. This can take a few seconds."), /* @__PURE__ */ React.createElement("div", { className: "broadcast-steps" }, steps.map((s, i) => /* @__PURE__ */ React.createElement("div", { key: i, className: "bcast-step " + (i < confirmIdx ? "done" : i === confirmIdx ? "active" : "pending") }, /* @__PURE__ */ React.createElement("div", { className: "bcast-ic" }, i < confirmIdx ? /* @__PURE__ */ React.createElement(Icon, { name: "check", size: 12 }) : i === confirmIdx ? /* @__PURE__ */ React.createElement("div", { className: "spinner", style: { width: 10, height: 10, borderWidth: 1.5 } }) : /* @__PURE__ */ React.createElement("span", { style: { fontFamily: "var(--font-mono)", fontSize: 10, color: "inherit" } }, i + 1)), /* @__PURE__ */ React.createElement("div", null, /* @__PURE__ */ React.createElement("strong", null, s.label), s.sub && /* @__PURE__ */ React.createElement("div", { style: { fontSize: 12, color: "var(--fg-3)" } }, s.sub))))), /* @__PURE__ */ React.createElement("div", { className: "prog-bar", style: { marginTop: 18 } }, /* @__PURE__ */ React.createElement("div", { className: "prog-fill", style: { width: progress * 100 + "%" } }))), /* @__PURE__ */ React.createElement("div", { className: "modal-foot" }, /* @__PURE__ */ React.createElement("div", { className: "cost" }, /* @__PURE__ */ React.createElement("div", { className: "spinner" }), /* @__PURE__ */ React.createElement("span", null, "Broadcasting\u2026")), /* @__PURE__ */ React.createElement("div", { style: { display: "flex", gap: 8 } }, /* @__PURE__ */ React.createElement("button", { className: "btn secondary", onClick: onClose }, "Close window"))));
  };
  const InitStep = () => {
    if (initPhase === "done") {
      return /* @__PURE__ */ React.createElement(React.Fragment, null, /* @__PURE__ */ React.createElement("div", { className: "modal-body" }, /* @__PURE__ */ React.createElement("div", { className: "confirmed-banner" }, /* @__PURE__ */ React.createElement("div", { style: { width: 32, height: 32, borderRadius: 999, background: "var(--green-300)", display: "grid", placeItems: "center", color: "#fff", flexShrink: 0 } }, /* @__PURE__ */ React.createElement(Icon, { name: "check", size: 18 })), /* @__PURE__ */ React.createElement("div", null, /* @__PURE__ */ React.createElement("div", { style: { fontWeight: 600, marginBottom: 2, color: "inherit" } }, "INIT broadcast"), /* @__PURE__ */ React.createElement("div", { style: { fontSize: 12, color: "var(--fg-2)" } }, "Once it confirms, this database is initialized and you can set keys."))), /* @__PURE__ */ React.createElement("div", { style: { marginTop: 16, padding: "14px 16px", background: "var(--bg-sunken)", border: "1px solid var(--border-1)", borderRadius: "var(--radius-md)" } }, /* @__PURE__ */ React.createElement("div", { style: { fontFamily: "var(--font-mono)", fontSize: 11, letterSpacing: "0.08em", color: "var(--fg-3)", textTransform: "uppercase", marginBottom: 8 } }, "Receipt"), /* @__PURE__ */ React.createElement("div", { className: "kv-row" }, /* @__PURE__ */ React.createElement("span", { className: "label" }, "Op"), /* @__PURE__ */ React.createElement("span", { className: "value" }, "INIT")), /* @__PURE__ */ React.createElement("div", { className: "kv-row" }, /* @__PURE__ */ React.createElement("span", { className: "label" }, "TXID"), /* @__PURE__ */ React.createElement("span", { className: "value" }, txHash ? /* @__PURE__ */ React.createElement(CollapsibleString, { value: txHash, onCopy: (t) => {
        try {
          navigator.clipboard.writeText(t);
        } catch {
        }
      } }) : "\u2014")))), /* @__PURE__ */ React.createElement("div", { className: "modal-foot" }, /* @__PURE__ */ React.createElement("div", { className: "cost" }, /* @__PURE__ */ React.createElement(Icon, { name: "shield-check", size: 11, color: "var(--green-300)" }), " ", /* @__PURE__ */ React.createElement("span", null, "broadcast")), /* @__PURE__ */ React.createElement("div", { style: { display: "flex", gap: 8 } }, /* @__PURE__ */ React.createElement("button", { className: "btn primary", onClick: () => {
        onDone && onDone({ init: true });
        onClose();
      } }, "Done"))));
    }
    if (error) {
      const code = error.code;
      return /* @__PURE__ */ React.createElement(React.Fragment, null, /* @__PURE__ */ React.createElement("div", { className: "modal-body" }, /* @__PURE__ */ React.createElement("div", { className: "callout-flow warn" }, /* @__PURE__ */ React.createElement(Icon, { name: "alert-triangle", size: 16, color: "var(--amber-400)" }), /* @__PURE__ */ React.createElement("div", null, /* @__PURE__ */ React.createElement("strong", { style: { color: "inherit" } }, code === "not_synced" ? "Wallet isn\u2019t fully synced yet." : code === "stale_tip" ? "Can\u2019t confirm a current chain tip." : code === "insufficient_funds" ? "Not enough funds to broadcast INIT." : code === "watch_only" ? "This is a watch-only database." : "INIT broadcast failed."), /* @__PURE__ */ React.createElement(ErrorMessage, { message: error.message })))), /* @__PURE__ */ React.createElement("div", { className: "modal-foot" }, /* @__PURE__ */ React.createElement("div", { className: "cost" }, /* @__PURE__ */ React.createElement(Icon, { name: "x", size: 11 }), " ", /* @__PURE__ */ React.createElement("span", null, "not broadcast")), /* @__PURE__ */ React.createElement("div", { style: { display: "flex", gap: 8 } }, /* @__PURE__ */ React.createElement("button", { className: "btn secondary", onClick: onClose }, "Close"), /* @__PURE__ */ React.createElement("button", { className: "btn primary", onClick: () => setError(null) }, "\u2190 Back"))));
    }
    if (initPhase === "sending") {
      return /* @__PURE__ */ React.createElement(React.Fragment, null, /* @__PURE__ */ React.createElement("div", { className: "modal-body" }, /* @__PURE__ */ React.createElement("div", { style: { fontSize: 13, color: "var(--fg-2)", marginBottom: 14, lineHeight: 1.5 } }, "Signing and broadcasting to ", /* @__PURE__ */ React.createElement("strong", { style: { color: "var(--fg-1)" } }, db.network), " via the local wallet. This can take a few minutes."), initMemoPreview), /* @__PURE__ */ React.createElement("div", { className: "modal-foot" }, /* @__PURE__ */ React.createElement("div", { className: "cost" }, /* @__PURE__ */ React.createElement("div", { className: "spinner" }), /* @__PURE__ */ React.createElement("span", null, "Broadcasting INIT\u2026")), /* @__PURE__ */ React.createElement("div", null)));
    }
    if (isInitializing) {
      const done = db.init_done || 0;
      const required = db.init_required || 0;
      return /* @__PURE__ */ React.createElement(React.Fragment, null, /* @__PURE__ */ React.createElement("div", { className: "modal-body" }, /* @__PURE__ */ React.createElement("div", { className: "callout-flow warn" }, /* @__PURE__ */ React.createElement(Icon, { name: "clock", size: 16, color: "var(--amber-400)" }), /* @__PURE__ */ React.createElement("div", null, /* @__PURE__ */ React.createElement("strong", { style: { color: "inherit" } }, "INIT is awaiting confirmation", required ? ` (${done}/${required})` : "", "."), /* @__PURE__ */ React.createElement("div", { style: { marginTop: 4, fontSize: 12.5, color: "var(--fg-2)" } }, "It\u2019s already been broadcast, no need to send it again. Should be ready within ~5 minutes, then you can set keys.")))), /* @__PURE__ */ React.createElement("div", { className: "modal-foot" }, /* @__PURE__ */ React.createElement("div", { className: "cost" }, /* @__PURE__ */ React.createElement(Icon, { name: "clock", size: 11 }), " ", /* @__PURE__ */ React.createElement("span", null, "initializing")), /* @__PURE__ */ React.createElement("div", { style: { display: "flex", gap: 8 } }, /* @__PURE__ */ React.createElement("button", { className: "btn secondary", onClick: onClose }, "Close"), paused && onSync && /* @__PURE__ */ React.createElement("button", { className: "btn primary", disabled: syncing, onClick: onSync }, /* @__PURE__ */ React.createElement(Icon, { name: "refresh-cw", className: "icon" }), " ", syncing ? "Syncing\u2026" : "Sync now"))));
    }
    if (lowFunds) {
      return /* @__PURE__ */ React.createElement(React.Fragment, null, /* @__PURE__ */ React.createElement("div", { className: "modal-body" }, /* @__PURE__ */ React.createElement("div", { className: "callout-flow warn" }, /* @__PURE__ */ React.createElement(Icon, { name: "alert-triangle", size: 16, color: "var(--amber-400)" }), /* @__PURE__ */ React.createElement("div", null, /* @__PURE__ */ React.createElement("strong", { style: { color: "inherit" } }, "Insufficient funds."), /* @__PURE__ */ React.createElement("div", { style: { marginTop: 4, fontSize: 12.5, color: "var(--fg-2)" } }, "Deposit to this database's funding address in order to initialize the database."))), /* @__PURE__ */ React.createElement("div", { style: { marginTop: 12 } }, initMemoPreview)), /* @__PURE__ */ React.createElement("div", { className: "modal-foot" }, /* @__PURE__ */ React.createElement("div", { className: "cost" }, /* @__PURE__ */ React.createElement("span", { className: "amber-dot" }), /* @__PURE__ */ React.createElement("span", null, "Needs ~", window.formatZats(FEE_FLOOR, db.network), " (network fee)")), /* @__PURE__ */ React.createElement("div", { style: { display: "flex", gap: 8 } }, /* @__PURE__ */ React.createElement("button", { className: "btn secondary", onClick: onClose }, "Cancel"), db.network === "testnet" && /* @__PURE__ */ React.createElement("button", { className: "btn secondary", onClick: doFaucetInit, disabled: faucetInitState !== "idle" }, /* @__PURE__ */ React.createElement(Icon, { name: "rocket", className: "icon" }), " ", faucetInitLabel), onDeposit && /* @__PURE__ */ React.createElement("button", { className: "btn primary", onClick: () => {
        onClose();
        onDeposit();
      } }, /* @__PURE__ */ React.createElement(Icon, { name: "download", className: "icon" }), " Deposit"))));
    }
    return /* @__PURE__ */ React.createElement(React.Fragment, null, /* @__PURE__ */ React.createElement("div", { className: "modal-body" }, /* @__PURE__ */ React.createElement("div", { className: "callout-flow warn" }, /* @__PURE__ */ React.createElement(Icon, { name: "alert-triangle", size: 16, color: "var(--amber-400)" }), /* @__PURE__ */ React.createElement("div", null, /* @__PURE__ */ React.createElement("strong", { style: { color: "inherit" } }, "This database isn\u2019t initialized yet."), /* @__PURE__ */ React.createElement("div", { style: { marginTop: 4, fontSize: 12.5, color: "var(--fg-2)" } }, "Broadcast an INIT to open it for writes."))), !synced && /* @__PURE__ */ React.createElement("div", { className: "callout-flow", style: { marginTop: 12 } }, /* @__PURE__ */ React.createElement(Icon, { name: "info", size: 16, color: "var(--fg-3)" }), /* @__PURE__ */ React.createElement("div", { style: { fontSize: 12.5, color: "var(--fg-2)" } }, "Finish syncing before broadcasting INIT.", " ", paused ? "Sync first." : "Syncing now, ready once it catches up.")), /* @__PURE__ */ React.createElement("div", { style: { marginTop: 12 } }, initMemoPreview), /* @__PURE__ */ React.createElement("div", { style: { marginTop: 12, padding: "10px 14px", background: "var(--bg-sunken)", borderRadius: "var(--radius-md)", border: "1px solid var(--border-1)" } }, /* @__PURE__ */ React.createElement("div", { style: { display: "flex", justifyContent: "space-between", fontFamily: "var(--font-mono)", fontSize: 11, color: "var(--fg-3)", textTransform: "uppercase", letterSpacing: "0.08em", marginBottom: 6 } }, /* @__PURE__ */ React.createElement("span", null, "INIT tx preview"), /* @__PURE__ */ React.createElement("span", null, db.network)), /* @__PURE__ */ React.createElement("div", { style: { fontFamily: "var(--font-mono)", fontSize: 12, color: "var(--fg-2)", lineHeight: 1.6 } }, /* @__PURE__ */ React.createElement("div", null, "fee \xB7 ", /* @__PURE__ */ React.createElement("span", { style: { color: "var(--fg-1)" } }, "~", window.formatZats(FEE_FLOOR, db.network)), " ", /* @__PURE__ */ React.createElement("span", { style: { color: "var(--fg-3)" } }, "(network fee, the only cost)")), /* @__PURE__ */ React.createElement("div", null, "balance \xB7 ", /* @__PURE__ */ React.createElement("span", { style: { color: "var(--fg-1)" } }, db.balance != null ? window.formatZats(db.balance, db.network) : "\u2014"))))), /* @__PURE__ */ React.createElement("div", { className: "modal-foot" }, /* @__PURE__ */ React.createElement("div", { className: "cost" }, /* @__PURE__ */ React.createElement("span", { className: "amber-dot" }), /* @__PURE__ */ React.createElement("span", null, synced ? `Cost: ~${window.formatZats(FEE_FLOOR, db.network)} (network fee)` : "Sync to the chain tip first")), /* @__PURE__ */ React.createElement("div", { style: { display: "flex", gap: 8 } }, /* @__PURE__ */ React.createElement("button", { className: "btn secondary", onClick: onClose }, "Cancel"), db.network === "testnet" && /* @__PURE__ */ React.createElement("button", { className: "btn secondary", onClick: doFaucetInit, disabled: faucetInitState !== "idle" }, /* @__PURE__ */ React.createElement(Icon, { name: "rocket", className: "icon" }), " ", faucetInitLabel), synced ? /* @__PURE__ */ React.createElement("button", { className: "btn primary", onClick: doInit }, /* @__PURE__ */ React.createElement(Icon, { name: "send", className: "icon" }), " Broadcast INIT") : paused && onSync && /* @__PURE__ */ React.createElement("button", { className: "btn primary", disabled: syncing, onClick: onSync }, /* @__PURE__ */ React.createElement(Icon, { name: "refresh-cw", className: "icon" }), " ", syncing ? "Syncing\u2026" : "Sync now"))));
  };
  const ConfirmedStep = () => /* @__PURE__ */ React.createElement(React.Fragment, null, /* @__PURE__ */ React.createElement("div", { className: "modal-body" }, /* @__PURE__ */ React.createElement("div", { className: "confirmed-banner" }, /* @__PURE__ */ React.createElement("div", { style: { width: 32, height: 32, borderRadius: 999, background: "var(--green-300)", display: "grid", placeItems: "center", color: "#fff", flexShrink: 0 } }, /* @__PURE__ */ React.createElement(Icon, { name: "check", size: 18 })), /* @__PURE__ */ React.createElement("div", null, /* @__PURE__ */ React.createElement("div", { style: { fontWeight: 600, color: "inherit" } }, isDel ? `Deleted` : prefillKey ? `Updated` : `Set`, " ", /* @__PURE__ */ React.createElement("code", { style: { fontFamily: "var(--font-mono)" } }, keyName)))), /* @__PURE__ */ React.createElement("div", { style: { marginTop: 16, padding: "14px 16px", background: "var(--bg-sunken)", border: "1px solid var(--border-1)", borderRadius: "var(--radius-md)" } }, /* @__PURE__ */ React.createElement("div", { style: { fontFamily: "var(--font-mono)", fontSize: 11, letterSpacing: "0.08em", color: "var(--fg-3)", textTransform: "uppercase", marginBottom: 8 } }, "Receipt"), /* @__PURE__ */ React.createElement("div", { className: "kv-row" }, /* @__PURE__ */ React.createElement("span", { className: "label" }, "Op"), /* @__PURE__ */ React.createElement("span", { className: "value" }, opLabel)), /* @__PURE__ */ React.createElement("div", { className: "kv-row" }, /* @__PURE__ */ React.createElement("span", { className: "label" }, "Key"), /* @__PURE__ */ React.createElement("span", { className: "value", style: { color: "var(--amber-400)" } }, keyName)), !isDel && /* @__PURE__ */ React.createElement("div", { className: "kv-row" }, /* @__PURE__ */ React.createElement("span", { className: "label" }, "Value"), /* @__PURE__ */ React.createElement("span", { className: "value", style: { wordBreak: "break-all" } }, val.length > 80 ? val.slice(0, 80) + "\u2026" : val)), /* @__PURE__ */ React.createElement("div", { className: "kv-row" }, /* @__PURE__ */ React.createElement("span", { className: "label" }, "TXID"), /* @__PURE__ */ React.createElement("span", { className: "value" }, txHash ? /* @__PURE__ */ React.createElement(CollapsibleString, { value: txHash, onCopy: (t) => {
    try {
      navigator.clipboard.writeText(t);
    } catch {
    }
  } }) : "\u2014")))), /* @__PURE__ */ React.createElement("div", { className: "modal-foot" }, /* @__PURE__ */ React.createElement("div", { className: "cost" }, /* @__PURE__ */ React.createElement(Icon, { name: "shield-check", size: 11, color: "var(--green-300)" }), " ", /* @__PURE__ */ React.createElement("span", null, "broadcast")), /* @__PURE__ */ React.createElement("div", { style: { display: "flex", gap: 8 } }, /* @__PURE__ */ React.createElement("button", { className: "btn secondary", onClick: () => {
    onDone && onDone({ key: keyName, value: val, isDel });
    setStep("form");
    setKeyName("");
    setVal("");
  } }, /* @__PURE__ */ React.createElement(Icon, { name: "plus", className: "icon" }), " Set another"), /* @__PURE__ */ React.createElement("button", { className: "btn primary", onClick: () => {
    onDone && onDone({ key: keyName, value: val, isDel });
    onClose();
  } }, "Done"))));
  const locked = step === "broadcasting" || needsInit && initPhase === "sending";
  const headTitle = needsInit ? "Initialize database" : heading;
  const headSub = needsInit ? "Broadcast an INIT to open this database for writes." : sub;
  return /* @__PURE__ */ React.createElement("div", { className: "modal-overlay", onClick: (e) => {
    if (e.target.classList.contains("modal-overlay") && !locked) onClose();
  } }, /* @__PURE__ */ React.createElement("div", { className: "modal", role: "dialog", "aria-labelledby": "write-title" }, /* @__PURE__ */ React.createElement("div", { className: "modal-head" }, /* @__PURE__ */ React.createElement("div", null, /* @__PURE__ */ React.createElement("div", { className: "eyebrow" }, "DB: ", db.name), /* @__PURE__ */ React.createElement("h2", { id: "write-title" }, headTitle), /* @__PURE__ */ React.createElement("div", { style: { fontSize: 12.5, color: "var(--fg-3)", marginTop: 4 } }, headSub)), !locked && /* @__PURE__ */ React.createElement("button", { className: "close", onClick: onClose, title: "Close" }, /* @__PURE__ */ React.createElement(Icon, { name: "x", size: 16 }))), needsInit ? InitStep() : /* @__PURE__ */ React.createElement(React.Fragment, null, step === "form" && FormStep(), step === "review" && ReviewStep(), step === "broadcasting" && BroadcastStep(), step === "confirmed" && ConfirmedStep())));
};
window.WriteFlow = WriteFlow;
