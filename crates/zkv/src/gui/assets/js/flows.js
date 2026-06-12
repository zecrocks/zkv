const Qr = ({ text }) => {
  const [svg, setSvg] = React.useState(null);
  const [failed, setFailed] = React.useState(false);
  React.useEffect(() => {
    let alive = true;
    setSvg(null);
    setFailed(false);
    if (!text) return;
    window.zkvApi.qr(text).then((r) => alive && setSvg(r.svg)).catch(() => alive && setFailed(true));
    return () => {
      alive = false;
    };
  }, [text]);
  if (failed)
    return /* @__PURE__ */ React.createElement("div", { style: { display: "grid", placeItems: "center", width: "100%", height: "100%", padding: 8, textAlign: "center", color: "var(--fg-3)", fontSize: 11 } }, "QR unavailable, use the address below");
  if (!svg)
    return /* @__PURE__ */ React.createElement("div", { style: { display: "grid", placeItems: "center", width: "100%", height: "100%" } }, /* @__PURE__ */ React.createElement("div", { className: "spinner" }));
  return /* @__PURE__ */ React.createElement("div", { style: { width: "100%", height: "100%" }, dangerouslySetInnerHTML: { __html: svg } });
};
const ValidAddressInfo = () => {
  const [open, setOpen] = React.useState(false);
  return /* @__PURE__ */ React.createElement(React.Fragment, null, /* @__PURE__ */ React.createElement(
    "button",
    {
      type: "button",
      onClick: () => setOpen(true),
      style: { background: "none", border: "none", padding: 0, marginTop: 4, color: "var(--fg-link)", cursor: "pointer", fontSize: 11, fontFamily: "inherit" }
    },
    "Is this a valid Zcash address?"
  ), open && /* @__PURE__ */ React.createElement(
    "div",
    {
      className: "modal-overlay",
      style: { zIndex: 1e3 },
      onClick: (e) => {
        e.stopPropagation();
        if (e.target.classList.contains("modal-overlay")) setOpen(false);
      }
    },
    /* @__PURE__ */ React.createElement("div", { className: "modal", role: "dialog", style: { maxWidth: 470 } }, /* @__PURE__ */ React.createElement("div", { className: "modal-head" }, /* @__PURE__ */ React.createElement("h2", { id: "valid-addr-title", style: { fontSize: 18 } }, "Is this a valid Zcash address?"), /* @__PURE__ */ React.createElement("button", { className: "close", onClick: () => setOpen(false), title: "Close" }, /* @__PURE__ */ React.createElement(Icon, { name: "x", size: 16 }))), /* @__PURE__ */ React.createElement("div", { className: "modal-body", style: { fontSize: 13.5, color: "var(--fg-2)", lineHeight: 1.6 } }, /* @__PURE__ */ React.createElement("p", { style: { marginTop: 0 } }, "If someone tells you that this Zcash address is invalid, that's okay! That means that they don't support shielded Zcash. Unfortunate."), /* @__PURE__ */ React.createElement("p", null, "To receive transparent Zcash from them, install a Zcash wallet on your mobile phone, then use it to send the Zcash into the database's shielded address, shown in the QR code within this app, once it confirms."), /* @__PURE__ */ React.createElement("p", { style: { marginBottom: 0 } }, "This application only supports shielded Zcash.")), /* @__PURE__ */ React.createElement("div", { className: "modal-foot" }, /* @__PURE__ */ React.createElement("div", null), /* @__PURE__ */ React.createElement("button", { className: "btn primary", onClick: () => setOpen(false) }, "Got it")))
  ));
};
const DepositModal = ({ db, onClose, onCopy, onInited }) => {
  const addr = db && db.funding_address;
  const cur = window.currencyFor(db && db.network);
  const feeAmt = window.formatZats(1e4, db && db.network);
  const [faucetState, setFaucetState] = React.useState("idle");
  const requestFaucet = async () => {
    setFaucetState("requesting");
    try {
      const r = await window.zkvApi.faucetFunds(db.name);
      setFaucetState(r.outcome === "ok" ? "done" : r.outcome === "outdated" ? "outdated" : "retry");
    } catch (_) {
      setFaucetState("retry");
    }
  };
  const faucetLabel = faucetState === "requesting" ? "Requesting\u2026" : faucetState === "done" ? "Requested" : faucetState === "retry" ? "Try again later" : faucetState === "outdated" ? "Your app is outdated" : "Request from faucet";
  const showInitFaucet = !!(addr && db.network === "testnet" && db.init === "uninitialized");
  const [initState, setInitState] = React.useState("idle");
  const initViaFaucet = async () => {
    setInitState("requesting");
    try {
      const r = await window.zkvApi.faucetInit(db.name);
      if (r.outcome === "outdated") {
        setInitState("outdated");
        return;
      }
      if (r.outcome !== "ok") {
        setInitState("retry");
        return;
      }
      onInited && onInited();
    } catch (_) {
      setInitState("retry");
    }
  };
  const initLabel = initState === "requesting" ? "Initializing\u2026" : initState === "retry" ? "Try again later" : initState === "outdated" ? "Your app is outdated" : "Use our faucet";
  return /* @__PURE__ */ React.createElement(
    "div",
    {
      className: "modal-overlay",
      onClick: (e) => {
        if (e.target.classList.contains("modal-overlay")) onClose();
      }
    },
    /* @__PURE__ */ React.createElement("div", { className: "modal", role: "dialog", style: { maxWidth: 420 } }, /* @__PURE__ */ React.createElement("div", { className: "modal-head" }, /* @__PURE__ */ React.createElement("div", null, /* @__PURE__ */ React.createElement("div", { className: "eyebrow" }, "DB: ", db.name), /* @__PURE__ */ React.createElement("h2", null, "Add ", db.network, " funds"), /* @__PURE__ */ React.createElement("div", { style: { fontSize: 12.5, color: "var(--fg-3)", marginTop: 4 } }, "Send ", cur, " to this database's funding address. Each write costs about ", feeAmt, ", just the network fee.")), /* @__PURE__ */ React.createElement("button", { className: "close", onClick: onClose, title: "Close" }, /* @__PURE__ */ React.createElement(Icon, { name: "x", size: 16 }))), /* @__PURE__ */ React.createElement("div", { className: "modal-body" }, addr ? /* @__PURE__ */ React.createElement(React.Fragment, null, /* @__PURE__ */ React.createElement("div", { className: "qr-card", style: { margin: "0 auto", maxWidth: 300, border: "none", background: "transparent", padding: 0 } }, /* @__PURE__ */ React.createElement("div", { className: "qr" }, /* @__PURE__ */ React.createElement(Qr, { text: addr }))), /* @__PURE__ */ React.createElement("div", { className: "kv-row", style: { marginTop: 16 } }, /* @__PURE__ */ React.createElement("span", { className: "label", style: { minWidth: 110 } }, "Funding address"), /* @__PURE__ */ React.createElement("span", { className: "value" }, /* @__PURE__ */ React.createElement(CollapsibleString, { value: addr, onCopy }))), db.network === "mainnet" && /* @__PURE__ */ React.createElement("div", { style: { marginTop: 4 } }, /* @__PURE__ */ React.createElement(ValidAddressInfo, null)), /* @__PURE__ */ React.createElement("div", { className: "kv-row", style: { marginTop: 10 } }, /* @__PURE__ */ React.createElement("span", { className: "label", style: { minWidth: 110 } }, "Spendable"), /* @__PURE__ */ React.createElement("span", { className: "value mono" }, db.balance != null ? window.formatZats(db.balance, db.network) : "\u2014", db.confirming ? /* @__PURE__ */ React.createElement("span", { style: { color: "var(--fg-3)" } }, " (+ ", window.formatZats(db.confirming, db.network), " confirming)") : null))) : /* @__PURE__ */ React.createElement("div", { className: "callout-flow warn" }, /* @__PURE__ */ React.createElement(Icon, { name: "alert-triangle", size: 16, color: "var(--amber-400)" }), /* @__PURE__ */ React.createElement("div", null, "Watch-only databases hold no spending key, so there's nothing to fund."))), /* @__PURE__ */ React.createElement("div", { className: "modal-foot" }, /* @__PURE__ */ React.createElement("div", { className: "cost" }), /* @__PURE__ */ React.createElement("div", { style: { display: "flex", gap: 8 } }, showInitFaucet && /* @__PURE__ */ React.createElement(
      "button",
      {
        className: "btn secondary",
        onClick: initViaFaucet,
        disabled: initState !== "idle"
      },
      /* @__PURE__ */ React.createElement(Icon, { name: "rocket", className: "icon" }),
      " ",
      initLabel
    ), addr && db.network !== "mainnet" && /* @__PURE__ */ React.createElement(
      "button",
      {
        className: "btn secondary",
        onClick: requestFaucet,
        disabled: faucetState !== "idle"
      },
      /* @__PURE__ */ React.createElement(Icon, { name: "droplets", className: "icon" }),
      " ",
      faucetLabel
    ), /* @__PURE__ */ React.createElement("button", { className: "btn primary", onClick: onClose }, "Done"))))
  );
};
const SendModal = ({ db, onClose, onCopy, onDeposit, onDone }) => {
  const [step, setStep] = React.useState("form");
  const [recipient, setRecipient] = React.useState("");
  const [amount, setAmount] = React.useState("");
  const [check, setCheck] = React.useState(null);
  const [checking, setChecking] = React.useState(false);
  const [error, setError] = React.useState(null);
  const [txHash, setTxHash] = React.useState("");
  const [memo, setMemo] = React.useState("");
  const cur = window.currencyFor(db && db.network);
  const FEE_FLOOR = 1e4;
  React.useEffect(() => {
    const addr = recipient.trim();
    if (!addr) {
      setCheck(null);
      setChecking(false);
      return;
    }
    let cancelled = false;
    setChecking(true);
    const t = window.setTimeout(async () => {
      try {
        const r = await window.zkvApi.checkAddress(db.name, addr);
        if (!cancelled) setCheck(r);
      } catch (_) {
        if (!cancelled) setCheck({ valid: false, kind: null, network: null, pool: null, error: "could not validate address" });
      } finally {
        if (!cancelled) setChecking(false);
      }
    }, 350);
    return () => {
      cancelled = true;
      clearTimeout(t);
    };
  }, [recipient, db.name]);
  const amtTrim = amount.trim();
  const amtMatch = /^\d*\.?\d*$/.test(amtTrim) && amtTrim !== "" && amtTrim !== ".";
  const amtZats = amtMatch ? Math.round(parseFloat(amtTrim) * 1e8) : 0;
  const tooManyDecimals = (amtTrim.split(".")[1] || "").length > 8;
  const amtOk = amtMatch && !tooManyDecimals && amtZats > 0;
  const haveBal = db.balance != null;
  const overBalance = !!(haveBal && amtOk && amtZats + FEE_FLOOR > db.balance);
  const memoBytes = new TextEncoder().encode(memo).length;
  const memoOk = memoBytes <= 512;
  const recipientIsTransparent = !!(check && check.valid && (check.kind === "transparent" || check.kind === "TEX"));
  const canSend = !!(check && check.valid) && amtOk && !overBalance && memoOk;
  const doSend = async () => {
    setStep("sending");
    setError(null);
    setTxHash("");
    try {
      const sendMemo = recipientIsTransparent ? null : memo.trim() || null;
      const r = await window.zkvApi.send(db.name, recipient.trim(), amtTrim, sendMemo);
      setTxHash(r.txid || "");
      setStep("done");
      onDone && onDone();
    } catch (e) {
      setError(e);
    }
  };
  const FormStep = () => /* @__PURE__ */ React.createElement(React.Fragment, null, /* @__PURE__ */ React.createElement("div", { className: "modal-body" }, /* @__PURE__ */ React.createElement("div", { className: "write-fields" }, /* @__PURE__ */ React.createElement("div", { className: "field-block" }, /* @__PURE__ */ React.createElement("label", null, "Recipient address"), /* @__PURE__ */ React.createElement(
    "input",
    {
      className: "input mono lg",
      value: recipient,
      onChange: (e) => setRecipient(e.target.value),
      placeholder: `(${db.network})`,
      autoFocus: true
    }
  ), /* @__PURE__ */ React.createElement("div", { className: "byte-counter" }, /* @__PURE__ */ React.createElement("span", { className: !recipient.trim() || checking ? "" : check && check.valid ? "ok" : "err" }, !recipient.trim() ? "required" : checking ? "checking\u2026" : check && check.valid ? `valid ${check.kind} address (${[check.network, check.pool].filter(Boolean).join(", ")})` : check && check.error ? check.error : ""))), /* @__PURE__ */ React.createElement("div", { className: "field-block" }, /* @__PURE__ */ React.createElement("label", null, "Amount (", cur, ")"), /* @__PURE__ */ React.createElement(
    "input",
    {
      className: "input mono lg",
      value: amount,
      onChange: (e) => setAmount(e.target.value),
      placeholder: "0.00",
      inputMode: "decimal"
    }
  ), /* @__PURE__ */ React.createElement("div", { className: "byte-counter" }, /* @__PURE__ */ React.createElement("span", { className: amtTrim === "" ? "" : amtOk && !overBalance ? "ok" : "err" }, amtTrim === "" ? "required" : tooManyDecimals ? "at most 8 decimal places" : !amtMatch || amtZats <= 0 ? "enter an amount greater than zero" : overBalance ? "more than the available balance" : ""), /* @__PURE__ */ React.createElement("span", { style: { color: "var(--green-500)", fontFamily: "var(--font-mono)" } }, "spendable ", haveBal ? window.formatZats(db.balance, db.network) : "\u2014"))), /* @__PURE__ */ React.createElement("div", { className: "field-block" }, /* @__PURE__ */ React.createElement("label", null, "Memo ", /* @__PURE__ */ React.createElement("span", { style: { color: "var(--fg-3)", fontWeight: 400 } }, "\xB7 optional")), /* @__PURE__ */ React.createElement(
    "textarea",
    {
      className: "input mono",
      value: memo,
      onChange: (e) => setMemo(e.target.value),
      placeholder: recipientIsTransparent ? "Transparent recipients can't carry a memo" : "Optional message sent with the payment",
      rows: 3,
      disabled: recipientIsTransparent,
      style: { resize: "vertical", opacity: recipientIsTransparent ? 0.55 : 1 }
    }
  ), /* @__PURE__ */ React.createElement("div", { className: "byte-counter" }, /* @__PURE__ */ React.createElement("span", { className: memoOk ? "" : "err" }, recipientIsTransparent ? "memo not supported for this recipient" : !memoOk ? "memo is over the 512-byte limit" : ""), /* @__PURE__ */ React.createElement("span", { style: { color: memoOk ? "var(--fg-3)" : "var(--red-500)" } }, memoBytes, "/512"))))), /* @__PURE__ */ React.createElement("div", { className: "modal-foot" }, /* @__PURE__ */ React.createElement("div", { className: "cost" }, overBalance ? /* @__PURE__ */ React.createElement(React.Fragment, null, /* @__PURE__ */ React.createElement(Icon, { name: "alert-triangle", size: 11, color: "var(--red-500)" }), /* @__PURE__ */ React.createElement("span", { style: { color: "var(--red-500)" } }, "insufficient funds"), onDeposit && /* @__PURE__ */ React.createElement("button", { className: "btn ghost sm", style: { marginLeft: 4 }, onClick: onDeposit }, /* @__PURE__ */ React.createElement(Icon, { name: "download", size: 12 }), " Deposit")) : /* @__PURE__ */ React.createElement(React.Fragment, null, /* @__PURE__ */ React.createElement(Icon, { name: "zap", size: 11 }), /* @__PURE__ */ React.createElement("span", null, "Spends ", cur))), /* @__PURE__ */ React.createElement("div", { style: { display: "flex", gap: 8 } }, /* @__PURE__ */ React.createElement("button", { className: "btn secondary", onClick: onClose }, "Cancel"), /* @__PURE__ */ React.createElement("button", { className: "btn primary", disabled: !canSend, onClick: () => setStep("review") }, "Review \u2192"))));
  const ReviewStep = () => /* @__PURE__ */ React.createElement(React.Fragment, null, /* @__PURE__ */ React.createElement("div", { className: "modal-body" }, /* @__PURE__ */ React.createElement("div", { style: { fontSize: 13, color: "var(--fg-2)", marginBottom: 14, lineHeight: 1.5 } }, "Sending ", cur, " from ", /* @__PURE__ */ React.createElement("strong", { style: { color: "var(--fg-1)" } }, db.name), "."), /* @__PURE__ */ React.createElement("div", { style: { padding: "14px 16px", background: "var(--bg-sunken)", border: "1px solid var(--border-1)", borderRadius: "var(--radius-md)" } }, /* @__PURE__ */ React.createElement("div", { className: "kv-row" }, /* @__PURE__ */ React.createElement("span", { className: "label" }, "To"), /* @__PURE__ */ React.createElement("span", { className: "value" }, /* @__PURE__ */ React.createElement(CollapsibleString, { value: recipient.trim(), onCopy }))), /* @__PURE__ */ React.createElement("div", { className: "kv-row" }, /* @__PURE__ */ React.createElement("span", { className: "label" }, "Amount"), /* @__PURE__ */ React.createElement("span", { className: "value" }, window.formatZats(amtZats, db.network))), /* @__PURE__ */ React.createElement("div", { className: "kv-row" }, /* @__PURE__ */ React.createElement("span", { className: "label" }, "Fee"), /* @__PURE__ */ React.createElement("span", { className: "value" }, "~", window.formatZats(FEE_FLOOR, db.network))), /* @__PURE__ */ React.createElement("div", { className: "kv-row" }, /* @__PURE__ */ React.createElement("span", { className: "label" }, "Network"), /* @__PURE__ */ React.createElement("span", { className: "value" }, db.network)))), /* @__PURE__ */ React.createElement("div", { className: "modal-foot" }, /* @__PURE__ */ React.createElement("div", null), /* @__PURE__ */ React.createElement("div", { style: { display: "flex", gap: 8 } }, /* @__PURE__ */ React.createElement("button", { className: "btn secondary", onClick: () => setStep("form") }, "\u2190 Back"), /* @__PURE__ */ React.createElement("button", { className: "btn primary", onClick: doSend }, /* @__PURE__ */ React.createElement(Icon, { name: "send", className: "icon" }), " Confirm send"))));
  const SendingStep = () => {
    if (error) {
      const code = error.code;
      return /* @__PURE__ */ React.createElement(React.Fragment, null, /* @__PURE__ */ React.createElement("div", { className: "modal-body" }, /* @__PURE__ */ React.createElement("div", { className: "callout-flow warn" }, /* @__PURE__ */ React.createElement(Icon, { name: "alert-triangle", size: 16, color: "var(--amber-400)" }), /* @__PURE__ */ React.createElement("div", null, /* @__PURE__ */ React.createElement("strong", { style: { color: "inherit" } }, code === "insufficient_funds" ? "Not enough funds to send." : code === "watch_only" ? "This is a watch-only database." : "Send failed."), /* @__PURE__ */ React.createElement("div", { style: { marginTop: 4, fontSize: 12.5, color: "var(--fg-2)" } }, error.message), code === "insufficient_funds" && error.data && /* @__PURE__ */ React.createElement("div", { style: { marginTop: 6, fontFamily: "var(--font-mono)", fontSize: 11.5, color: "var(--fg-3)" } }, window.formatZats(error.data.available, db.network), " available \xB7", " ", window.formatZats(error.data.required, db.network), " needed")))), /* @__PURE__ */ React.createElement("div", { className: "modal-foot" }, /* @__PURE__ */ React.createElement("div", { className: "cost" }, /* @__PURE__ */ React.createElement(Icon, { name: "x", size: 11 }), " ", /* @__PURE__ */ React.createElement("span", null, "not sent")), /* @__PURE__ */ React.createElement("div", { style: { display: "flex", gap: 8 } }, code === "insufficient_funds" && onDeposit && /* @__PURE__ */ React.createElement("button", { className: "btn secondary", onClick: onDeposit }, /* @__PURE__ */ React.createElement(Icon, { name: "download", className: "icon" }), " Deposit"), /* @__PURE__ */ React.createElement("button", { className: "btn primary", onClick: () => {
        setError(null);
        setStep("form");
      } }, "\u2190 Back to form"))));
    }
    return /* @__PURE__ */ React.createElement("div", { className: "modal-body" }, /* @__PURE__ */ React.createElement("div", { style: { display: "flex", gap: 10, alignItems: "center", fontSize: 13, color: "var(--fg-2)", padding: "8px 0" } }, /* @__PURE__ */ React.createElement("div", { className: "spinner" }), /* @__PURE__ */ React.createElement("span", null, "Signing and broadcasting to ", db.network, "\u2026")));
  };
  const DoneStep = () => /* @__PURE__ */ React.createElement(React.Fragment, null, /* @__PURE__ */ React.createElement("div", { className: "modal-body" }, /* @__PURE__ */ React.createElement("div", { className: "confirmed-banner" }, /* @__PURE__ */ React.createElement("div", { style: { width: 32, height: 32, borderRadius: 999, background: "var(--green-300)", display: "grid", placeItems: "center", color: "#fff", flexShrink: 0 } }, /* @__PURE__ */ React.createElement(Icon, { name: "check", size: 18 })), /* @__PURE__ */ React.createElement("div", null, /* @__PURE__ */ React.createElement("div", { style: { fontWeight: 600, color: "inherit" } }, "Sent ", window.formatZats(amtZats, db.network)), /* @__PURE__ */ React.createElement("div", { style: { fontSize: 12, color: "var(--fg-2)" } }, "Broadcast to ", db.network, ". It\u2019ll confirm in a few minutes."))), /* @__PURE__ */ React.createElement("div", { style: { marginTop: 16, padding: "14px 16px", background: "var(--bg-sunken)", border: "1px solid var(--border-1)", borderRadius: "var(--radius-md)" } }, /* @__PURE__ */ React.createElement("div", { style: { fontFamily: "var(--font-mono)", fontSize: 11, letterSpacing: "0.08em", color: "var(--fg-3)", textTransform: "uppercase", marginBottom: 8 } }, "Receipt"), /* @__PURE__ */ React.createElement("div", { className: "kv-row" }, /* @__PURE__ */ React.createElement("span", { className: "label" }, "To"), /* @__PURE__ */ React.createElement("span", { className: "value" }, /* @__PURE__ */ React.createElement(CollapsibleString, { value: recipient.trim(), onCopy }))), /* @__PURE__ */ React.createElement("div", { className: "kv-row" }, /* @__PURE__ */ React.createElement("span", { className: "label" }, "Amount"), /* @__PURE__ */ React.createElement("span", { className: "value" }, window.formatZats(amtZats, db.network))), /* @__PURE__ */ React.createElement("div", { className: "kv-row" }, /* @__PURE__ */ React.createElement("span", { className: "label" }, "TXID"), /* @__PURE__ */ React.createElement("span", { className: "value" }, txHash ? /* @__PURE__ */ React.createElement(CollapsibleString, { value: txHash, onCopy }) : "\u2014")))), /* @__PURE__ */ React.createElement("div", { className: "modal-foot" }, /* @__PURE__ */ React.createElement("div", { className: "cost" }, /* @__PURE__ */ React.createElement(Icon, { name: "shield-check", size: 11, color: "var(--green-300)" }), " ", /* @__PURE__ */ React.createElement("span", null, "broadcast")), /* @__PURE__ */ React.createElement("div", { style: { display: "flex", gap: 8 } }, /* @__PURE__ */ React.createElement("button", { className: "btn primary", onClick: onClose }, "Done"))));
  const locked = step === "sending" && !error;
  return /* @__PURE__ */ React.createElement("div", { className: "modal-overlay", onClick: (e) => {
    if (e.target.classList.contains("modal-overlay") && !locked) onClose();
  } }, /* @__PURE__ */ React.createElement("div", { className: "modal", role: "dialog", "aria-labelledby": "send-title", style: { maxWidth: 460 } }, /* @__PURE__ */ React.createElement("div", { className: "modal-head" }, /* @__PURE__ */ React.createElement("div", null, /* @__PURE__ */ React.createElement("div", { className: "eyebrow" }, "DB: ", db.name), /* @__PURE__ */ React.createElement("h2", { id: "send-title" }, "Send ", cur)), !locked && /* @__PURE__ */ React.createElement("button", { className: "close", onClick: onClose, title: "Close" }, /* @__PURE__ */ React.createElement(Icon, { name: "x", size: 16 }))), step === "form" && FormStep(), step === "review" && ReviewStep(), step === "sending" && SendingStep(), step === "done" && DoneStep()));
};
const CreateFlow = ({ onCancel, onCreate, onGeneratePhrase, onInit, pollDb, minInitZats = 1e4, onComplete, servers, existingNames }) => {
  const [step, setStep] = React.useState(0);
  const [name, setName] = React.useState("");
  const [network, setNetwork] = React.useState("mainnet");
  const [pool, setPool] = React.useState("orchard");
  const [poolTouched, setPoolTouched] = React.useState(false);
  const [phrase, setPhrase] = React.useState(null);
  const [busy, setBusy] = React.useState(false);
  const [error, setError] = React.useState(null);
  const [created, setCreated] = React.useState(null);
  const [acked, setAcked] = React.useState(false);
  const [copiedPhrase, setCopiedPhrase] = React.useState(false);
  const [confirmWords, setConfirmWords] = React.useState({ 3: "", 11: "", 19: "" });
  const [balance, setBalance] = React.useState(null);
  const [confirming, setConfirming] = React.useState(0);
  const [initState, setInitState] = React.useState("uninitialized");
  const [broadcast, setBroadcast] = React.useState(false);
  const [initDone, setInitDone] = React.useState(0);
  const [initRequired, setInitRequired] = React.useState(0);
  const words = phrase ? phrase.trim().split(/\s+/) : [];
  const confirmOk = words.length === 24 && confirmWords[3] === words[3] && confirmWords[11] === words[11] && confirmWords[19] === words[19];
  const defaultPoolFor = (_net) => "orchard";
  const selectNetwork = (net) => {
    setNetwork(net);
    if (!poolTouched) setPool(defaultPoolFor(net));
  };
  const ironwoodSelected = pool === "ironwood";
  const selectPool = (p) => {
    setPool(p);
    setPoolTouched(true);
    if (p === "ironwood") setNetwork("testnet");
  };
  const nameOk = /^[A-Za-z0-9_-]{1,24}$/.test(name) && !(existingNames || []).includes(name);
  const stepDots = ["Configure", "Seed phrase", "Confirm", "Fund", "INIT", "Done"];
  const stepSubs = [
    "Pick a local name and network.",
    "",
    "Confirm a few words to prove you saved the phrase.",
    "Fund the database's address. Each write costs only the network fee.",
    "Broadcasting the INIT memo that opens this database for writes.",
    "Done, your database is initialized and ready for writes."
  ];
  const doGenerate = async () => {
    setBusy(true);
    setError(null);
    try {
      if (!phrase) setPhrase(await onGeneratePhrase());
      setStep(1);
    } catch (e) {
      setError(e);
    } finally {
      setBusy(false);
    }
  };
  const doPersist = async () => {
    if (created) {
      setStep(3);
      return;
    }
    setBusy(true);
    setError(null);
    try {
      const r = await onCreate(name.trim(), network, pool, phrase);
      setCreated(r);
      setStep(3);
    } catch (e) {
      setError(e);
    } finally {
      setBusy(false);
    }
  };
  React.useEffect(() => {
    if (step !== 3 || !created) return;
    let live = true;
    const tick = async () => {
      try {
        const d = await pollDb(created.name);
        if (!live) return;
        setBalance(d.balance);
        setConfirming(d.confirming || 0);
        setInitState(d.init);
      } catch (_) {
      }
    };
    tick();
    const id = setInterval(tick, 7e3);
    return () => {
      live = false;
      clearInterval(id);
    };
  }, [step, created, pollDb]);
  const spendable = balance != null ? balance : 0;
  const funded = balance != null && spendable >= minInitZats;
  const fundingState = balance == null ? "checking" : funded ? "ready" : (confirming || 0) > 0 ? "confirming" : spendable > 0 ? "short" : "waiting";
  const doInit = async () => {
    setBusy(true);
    setError(null);
    setBroadcast(false);
    setInitDone(0);
    setStep(4);
    try {
      await onInit(created.name);
    } catch (e) {
      setError(e);
      setBusy(false);
      return;
    }
    setBroadcast(true);
    const id = setInterval(async () => {
      try {
        const d = await pollDb(created.name);
        setInitState(d.init);
        setInitDone(d.init_done);
        if (d.init_required) setInitRequired(d.init_required);
        if (d.init === "initialized") {
          clearInterval(id);
          setBusy(false);
          if (onComplete) onComplete(created.name);
          else setStep(5);
        }
      } catch (_) {
      }
    }, 7e3);
  };
  const [faucetInitState, setFaucetInitState] = React.useState("idle");
  const doFaucetInit = async () => {
    setFaucetInitState("requesting");
    setError(null);
    try {
      const r = await window.zkvApi.faucetInit(created.name);
      if (r.outcome === "outdated") {
        setFaucetInitState("outdated");
        return;
      }
      if (r.outcome !== "ok") {
        setFaucetInitState("retry");
        return;
      }
      setBroadcast(true);
      setInitDone(0);
      setStep(4);
      const id = setInterval(async () => {
        try {
          const d = await pollDb(created.name);
          setInitState(d.init);
          setInitDone(d.init_done);
          if (d.init_required) setInitRequired(d.init_required);
          if (d.init === "initialized") {
            clearInterval(id);
            if (onComplete) onComplete(created.name);
            else setStep(5);
          }
        } catch (_) {
        }
      }, 7e3);
    } catch (_) {
      setFaucetInitState("retry");
    }
  };
  const faucetInitLabel = faucetInitState === "requesting" ? "Initializing\u2026" : faucetInitState === "retry" ? "Try again later" : faucetInitState === "outdated" ? "Your app is outdated" : "Use our faucet";
  const errBanner = error ? /* @__PURE__ */ React.createElement("div", { className: "callout-flow warn", style: { marginBottom: 16 } }, /* @__PURE__ */ React.createElement(Icon, { name: "alert-triangle", size: 16, color: "var(--amber-400)" }), /* @__PURE__ */ React.createElement("div", null, /* @__PURE__ */ React.createElement("strong", { style: { color: "inherit" } }, "Something went wrong."), /* @__PURE__ */ React.createElement(ErrorMessage, { message: error.message }))) : null;
  const counter = /* @__PURE__ */ React.createElement("div", { className: "cost", style: { fontFamily: "var(--font-mono)", fontSize: 11, color: "var(--fg-3)" } }, "step ", step + 1, " / ", stepDots.length);
  return /* @__PURE__ */ React.createElement(
    "div",
    {
      className: "modal-overlay",
      onClick: (e) => {
        if (e.target.classList.contains("modal-overlay") && !busy) onCancel();
      }
    },
    /* @__PURE__ */ React.createElement("div", { className: "modal create-modal", role: "dialog", "aria-labelledby": "create-title" }, /* @__PURE__ */ React.createElement("div", { className: "modal-head" }, /* @__PURE__ */ React.createElement("div", null, /* @__PURE__ */ React.createElement("div", { className: "eyebrow" }, "CREATE DATABASE \xB7 ", stepDots[step].toUpperCase()), /* @__PURE__ */ React.createElement("h2", { id: "create-title" }, step === 1 ? "Database Seed Phrase" : stepDots[step]), stepSubs[step] && /* @__PURE__ */ React.createElement("div", { style: { fontSize: 12.5, color: "var(--fg-3)", marginTop: 4 } }, stepSubs[step])), !busy && /* @__PURE__ */ React.createElement("button", { className: "close", onClick: onCancel, title: "Close" }, /* @__PURE__ */ React.createElement(Icon, { name: "x", size: 16 }))), /* @__PURE__ */ React.createElement("div", { className: "modal-stepper" }, stepDots.map((s, i) => /* @__PURE__ */ React.createElement("div", { key: i, className: "step-dot " + (i < step ? "done" : i === step ? "active" : "") }, /* @__PURE__ */ React.createElement("span", null, i < step ? "\u2713" : i + 1), /* @__PURE__ */ React.createElement("span", { className: "dot-label" }, s)))), step === 0 && /* @__PURE__ */ React.createElement(React.Fragment, null, /* @__PURE__ */ React.createElement("div", { className: "modal-body" }, errBanner, /* @__PURE__ */ React.createElement("div", { className: "write-fields" }, /* @__PURE__ */ React.createElement("div", { className: "field-block" }, /* @__PURE__ */ React.createElement("label", null, "Database name ", /* @__PURE__ */ React.createElement("span", { style: { color: "var(--fg-3)", fontWeight: 400 } }, "\xB7 local nickname only")), /* @__PURE__ */ React.createElement("input", { className: "input lg mono", value: name, onChange: (e) => setName(e.target.value), maxLength: 24, autoFocus: true }), name && !/^[A-Za-z0-9_-]*$/.test(name) ? /* @__PURE__ */ React.createElement("div", { className: "hint", style: { color: "var(--red-500)" } }, "Use only letters, digits, - and _.") : name.length > 24 ? /* @__PURE__ */ React.createElement("div", { className: "hint", style: { color: "var(--red-500)" } }, "At most 24 characters.") : name && (existingNames || []).includes(name) ? /* @__PURE__ */ React.createElement("div", { className: "hint", style: { color: "var(--red-500)" } }, 'A database named "', name, '" already exists.') : null), /* @__PURE__ */ React.createElement("div", { className: "field-block" }, /* @__PURE__ */ React.createElement("label", null, "Network"), /* @__PURE__ */ React.createElement("div", { className: "seg" }, /* @__PURE__ */ React.createElement("button", { className: network === "mainnet" ? "on" : "", disabled: ironwoodSelected, onClick: () => selectNetwork("mainnet") }, /* @__PURE__ */ React.createElement(Icon, { name: "globe", size: 12 }), " mainnet"), /* @__PURE__ */ React.createElement("button", { className: network === "testnet" ? "on" : "", disabled: ironwoodSelected, onClick: () => selectNetwork("testnet") }, /* @__PURE__ */ React.createElement(Icon, { name: "flask-conical", size: 12 }), " testnet")), network === "testnet" && /* @__PURE__ */ React.createElement("div", { className: "hint", style: { fontSize: 12.5, color: "var(--fg-3)", marginTop: 4 } }, "Receive TAZ from our faucet to get started within limits.")), /* @__PURE__ */ React.createElement("div", { className: "field-block" }, /* @__PURE__ */ React.createElement("label", null, "Shielded pool"), /* @__PURE__ */ React.createElement("div", { className: "seg" }, /* @__PURE__ */ React.createElement("button", { className: pool === "ironwood" ? "on" : "", onClick: () => selectPool("ironwood") }, /* @__PURE__ */ React.createElement(Icon, { name: "trees", size: 12 }), " ironwood"), /* @__PURE__ */ React.createElement("button", { className: pool === "orchard" ? "on" : "", onClick: () => selectPool("orchard") }, /* @__PURE__ */ React.createElement(Icon, { name: "shield", size: 12 }), " orchard"), /* @__PURE__ */ React.createElement("button", { className: pool === "sapling" ? "on" : "", onClick: () => selectPool("sapling") }, /* @__PURE__ */ React.createElement(Icon, { name: "leaf", size: 12 }), " sapling"))), ironwoodSelected && /* @__PURE__ */ React.createElement("div", { className: "callout-flow warn" }, /* @__PURE__ */ React.createElement(Icon, { name: "alert-triangle", size: 16, color: "var(--amber-400)" }), /* @__PURE__ */ React.createElement("div", null, "Ironwood is an upcoming Zcash shielded pool.")))), /* @__PURE__ */ React.createElement("div", { className: "modal-foot" }, counter, /* @__PURE__ */ React.createElement("div", { style: { display: "flex", gap: 8 } }, /* @__PURE__ */ React.createElement("button", { className: "btn primary", disabled: !nameOk || busy || ironwoodSelected, onClick: doGenerate }, busy ? /* @__PURE__ */ React.createElement(React.Fragment, null, /* @__PURE__ */ React.createElement("div", { className: "spinner" }), " Generating\u2026") : "Generate phrase \u2192")))), step === 1 && phrase && /* @__PURE__ */ React.createElement(React.Fragment, null, /* @__PURE__ */ React.createElement("div", { className: "modal-body" }, errBanner, /* @__PURE__ */ React.createElement("div", { className: "form-stack" }, /* @__PURE__ */ React.createElement("div", { className: "callout-flow warn" }, /* @__PURE__ */ React.createElement(Icon, { name: "alert-triangle", size: 16, color: "var(--amber-400)" }), /* @__PURE__ */ React.createElement("div", null, /* @__PURE__ */ React.createElement("strong", { style: { color: "inherit" } }, "Write these 24 words down now."), " They are the only backup of the spending key. Anyone with this phrase can write to this database, and spend its ", currencyFor(network), ".")), /* @__PURE__ */ React.createElement("div", { className: "mnemonic-grid" }, words.map((w, i) => /* @__PURE__ */ React.createElement("div", { key: i, className: "mnemonic-cell" }, /* @__PURE__ */ React.createElement("span", { className: "idx" }, i + 1), /* @__PURE__ */ React.createElement("span", { className: "word" }, w)))), /* @__PURE__ */ React.createElement("div", { style: { display: "flex", justifyContent: "space-between", alignItems: "center", gap: 12 } }, /* @__PURE__ */ React.createElement("label", { className: "check-row", style: { margin: 0 } }, /* @__PURE__ */ React.createElement("input", { type: "checkbox", checked: acked, onChange: (e) => setAcked(e.target.checked) }), "I have written down all 24 words and stored them offline."), /* @__PURE__ */ React.createElement(
      "button",
      {
        className: "btn ghost sm",
        style: { flexShrink: 0 },
        title: "Copy the 24 words (space-separated) to the clipboard",
        onClick: () => {
          try {
            navigator.clipboard.writeText(words.join(" "));
          } catch {
          }
          setCopiedPhrase(true);
          setTimeout(() => setCopiedPhrase(false), 1500);
        }
      },
      /* @__PURE__ */ React.createElement(Icon, { name: "copy", className: "icon" }),
      " ",
      copiedPhrase ? "Copied" : "Copy phrase"
    )))), /* @__PURE__ */ React.createElement("div", { className: "modal-foot" }, counter, /* @__PURE__ */ React.createElement("div", { style: { display: "flex", gap: 8 } }, /* @__PURE__ */ React.createElement("button", { className: "btn secondary", onClick: () => setStep(0) }, "\u2190 Back"), /* @__PURE__ */ React.createElement("button", { className: "btn primary", disabled: !acked, onClick: () => setStep(2) }, "I've written it down \u2192")))), step === 2 && /* @__PURE__ */ React.createElement(React.Fragment, null, /* @__PURE__ */ React.createElement("div", { className: "modal-body" }, errBanner, /* @__PURE__ */ React.createElement("div", { style: { fontSize: 14, color: "var(--fg-2)", lineHeight: 1.55, marginBottom: 14 } }, "Type the following words from your phrase to confirm you wrote them down:"), /* @__PURE__ */ React.createElement("div", { className: "write-fields" }, [3, 11, 19].map((i, idx) => {
      const val = confirmWords[i];
      const matched = val === words[i];
      return /* @__PURE__ */ React.createElement("div", { key: i, className: "field-block" }, /* @__PURE__ */ React.createElement("label", null, "Word #", i + 1), /* @__PURE__ */ React.createElement("div", { style: { position: "relative" } }, /* @__PURE__ */ React.createElement(
        "input",
        {
          className: "input lg mono",
          style: { paddingRight: 92 },
          value: val,
          placeholder: `enter word ${i + 1}`,
          autoFocus: idx === 0,
          onChange: (e) => setConfirmWords({ ...confirmWords, [i]: e.target.value.trim().toLowerCase() }),
          onKeyDown: (e) => {
            if (idx === 2 && e.key === "Enter" && confirmOk && !busy) doPersist();
          }
        }
      ), val.length > 0 && /* @__PURE__ */ React.createElement("span", { style: { position: "absolute", right: 10, top: "50%", transform: "translateY(-50%)", display: "inline-flex", alignItems: "center", gap: 4, fontSize: 11, pointerEvents: "none", color: matched ? "var(--green-500)" : "var(--red-500)" } }, /* @__PURE__ */ React.createElement(Icon, { name: matched ? "check" : "x", size: 11 }), " ", matched ? "match" : "no match")));
    }))), /* @__PURE__ */ React.createElement("div", { className: "modal-foot" }, counter, /* @__PURE__ */ React.createElement("div", { style: { display: "flex", gap: 8 } }, /* @__PURE__ */ React.createElement("button", { className: "btn secondary", onClick: () => setStep(1) }, "\u2190 Back"), /* @__PURE__ */ React.createElement("button", { className: "btn primary", disabled: !confirmOk || busy, onClick: doPersist }, busy ? /* @__PURE__ */ React.createElement(React.Fragment, null, /* @__PURE__ */ React.createElement("div", { className: "spinner" }), " Creating\u2026") : "Confirm \u2192")))), step === 3 && created && /* @__PURE__ */ React.createElement(React.Fragment, null, /* @__PURE__ */ React.createElement("div", { className: "modal-body" }, errBanner, /* @__PURE__ */ React.createElement("div", { className: "funding-layout" }, /* @__PURE__ */ React.createElement("div", { className: "qr-card" }, /* @__PURE__ */ React.createElement("div", { className: "qr" }, /* @__PURE__ */ React.createElement(Qr, { text: created.funding_address })), /* @__PURE__ */ React.createElement("div", { className: "qr-cap" }, "scan with any Zcash wallet")), /* @__PURE__ */ React.createElement("div", null, /* @__PURE__ */ React.createElement("div", { className: "callout-flow note", style: { marginBottom: 16 } }, /* @__PURE__ */ React.createElement(Icon, { name: "zap", size: 16, color: "var(--amber-400)" }), /* @__PURE__ */ React.createElement("div", null, "Send ", /* @__PURE__ */ React.createElement("strong", { style: { color: "inherit" } }, "at least ", window.formatZats(minInitZats, network)), " to the funding address below from any Zcash wallet. This window polls for the funds. Every database action, each write or delete, spends at least ", window.formatZats(minInitZats, network), " in network fees.")), /* @__PURE__ */ React.createElement("div", { className: "kv-row" }, /* @__PURE__ */ React.createElement("span", { className: "label", style: { minWidth: 120 } }, "Funding address"), /* @__PURE__ */ React.createElement("span", { className: "value mono trunc", title: created.funding_address }, created.funding_address)), network === "mainnet" && /* @__PURE__ */ React.createElement("div", { style: { marginBottom: 10 } }, /* @__PURE__ */ React.createElement(ValidAddressInfo, null)), /* @__PURE__ */ React.createElement("div", { className: "kv-row" }, /* @__PURE__ */ React.createElement("span", { className: "label", style: { minWidth: 120 } }, "Min amount"), /* @__PURE__ */ React.createElement("span", { className: "value mono" }, window.formatZats(minInitZats, network))), /* @__PURE__ */ React.createElement("div", { className: "kv-row" }, /* @__PURE__ */ React.createElement("span", { className: "label", style: { minWidth: 120 } }, "Network"), /* @__PURE__ */ React.createElement("span", { className: "value mono" }, network)), fundingState === "confirming" && /* @__PURE__ */ React.createElement("div", { className: "callout-flow", style: { marginTop: 12 } }, /* @__PURE__ */ React.createElement("div", { className: "spinner" }), /* @__PURE__ */ React.createElement("div", { style: { fontSize: 12.5, color: "var(--fg-2)" } }, /* @__PURE__ */ React.createElement("strong", { style: { color: "inherit" } }, "Payment detected, confirming\u2026"), " ", window.formatZats(confirming, network), " is confirming on-chain. This can take a few minutes on mainnet, ", /* @__PURE__ */ React.createElement("strong", null, "Broadcast INIT"), " enables once the funds are spendable.")), fundingState === "short" && /* @__PURE__ */ React.createElement("div", { className: "callout-flow warn", style: { marginTop: 12 } }, /* @__PURE__ */ React.createElement(Icon, { name: "alert-triangle", size: 16, color: "var(--amber-400)" }), /* @__PURE__ */ React.createElement("div", { style: { fontSize: 12.5, color: "var(--fg-2)" } }, /* @__PURE__ */ React.createElement("strong", { style: { color: "inherit" } }, "Insufficient balance to initialize."), " ", "Initializing broadcasts a signed INIT memo that costs ~", window.formatZats(minInitZats, network), " (the network fee). You have ", window.formatZats(spendable, network), " spendable, send at least", " ", window.formatZats(minInitZats - spendable, network), " more to the address above.")), /* @__PURE__ */ React.createElement("div", { style: { display: "flex", gap: 8, marginTop: 12 } }, /* @__PURE__ */ React.createElement("button", { className: "btn secondary sm", onClick: () => navigator.clipboard.writeText(created.funding_address) }, /* @__PURE__ */ React.createElement(Icon, { name: "copy", className: "icon" }), " Copy address"))))), /* @__PURE__ */ React.createElement("div", { className: "modal-foot" }, counter, /* @__PURE__ */ React.createElement("div", { style: { display: "flex", gap: 8 } }, network === "testnet" && /* @__PURE__ */ React.createElement("button", { className: "btn secondary", onClick: doFaucetInit, disabled: faucetInitState !== "idle" }, /* @__PURE__ */ React.createElement(Icon, { name: "rocket", className: "icon" }), " ", faucetInitLabel), /* @__PURE__ */ React.createElement("button", { className: "btn primary", disabled: !funded, onClick: doInit }, funded ? "Broadcast INIT \u2192" : fundingState === "confirming" ? "Confirming payment\u2026" : fundingState === "short" ? "Send more funds" : "Waiting for funds\u2026")))), step === 4 && created && /* @__PURE__ */ React.createElement(React.Fragment, null, /* @__PURE__ */ React.createElement("div", { className: "modal-body" }, errBanner, /* @__PURE__ */ React.createElement("div", { className: "form-stack" }, /* @__PURE__ */ React.createElement("div", { style: { fontSize: 14, color: "var(--fg-2)", marginBottom: 6 } }, "Funds received. Broadcasting the INIT memo that opens this database for writes."), /* @__PURE__ */ React.createElement("div", { className: "progress-card" }, /* @__PURE__ */ React.createElement("div", { className: "prog-row " + (broadcast ? "done" : "in-flight") }, broadcast ? /* @__PURE__ */ React.createElement(Icon, { name: "check", size: 14, color: "var(--green-500)" }) : /* @__PURE__ */ React.createElement("div", { className: "spinner" }), /* @__PURE__ */ React.createElement("span", null, broadcast ? "Signed and broadcast INIT memo" : "Signing & broadcasting INIT memo\u2026")), /* @__PURE__ */ React.createElement("div", { className: "prog-row " + (initState === "initialized" ? "done" : "in-flight") }, initState === "initialized" ? /* @__PURE__ */ React.createElement(Icon, { name: "check", size: 14, color: "var(--green-500)" }) : /* @__PURE__ */ React.createElement("div", { className: "spinner" }), /* @__PURE__ */ React.createElement("span", null, initState === "initialized" ? `Confirmed (${initRequired || 3}/${initRequired || 3})` : `Awaiting confirmations (${Math.min(initDone, initRequired || 3)}/${initRequired || 3})\u2026`))))), /* @__PURE__ */ React.createElement("div", { className: "modal-foot" }, counter, /* @__PURE__ */ React.createElement("div", { style: { display: "flex", gap: 8 } }, /* @__PURE__ */ React.createElement("button", { className: "btn primary", disabled: true }, !broadcast ? "Broadcasting\u2026" : "Awaiting confirmations\u2026")))), step === 5 && created && /* @__PURE__ */ React.createElement(React.Fragment, null, /* @__PURE__ */ React.createElement("div", { className: "modal-body" }, /* @__PURE__ */ React.createElement("div", { className: "form-stack" }, /* @__PURE__ */ React.createElement("div", { className: "confirmed-banner" }, /* @__PURE__ */ React.createElement("div", { style: { width: 36, height: 36, borderRadius: 999, background: "var(--green-300)", display: "grid", placeItems: "center", color: "#fff", flexShrink: 0 } }, /* @__PURE__ */ React.createElement(Icon, { name: "check", size: 20 })), /* @__PURE__ */ React.createElement("div", null, /* @__PURE__ */ React.createElement("div", { style: { fontWeight: 600, marginBottom: 2, color: "inherit", fontSize: 14 } }, "Database ", /* @__PURE__ */ React.createElement("code", { style: { fontFamily: "var(--font-mono)" } }, created.name), " created"), /* @__PURE__ */ React.createElement("div", { style: { fontSize: 12.5, color: "inherit", opacity: 0.8 } }, "INIT confirmed. Writes are now enabled, open it and set your first key."))), /* @__PURE__ */ React.createElement("div", { style: { padding: "16px 18px", background: "var(--bg-sunken)", border: "1px solid var(--border-1)", borderRadius: "var(--radius-md)" } }, /* @__PURE__ */ React.createElement("div", { style: { fontFamily: "var(--font-mono)", fontSize: 11, letterSpacing: "0.08em", color: "var(--fg-3)", textTransform: "uppercase", marginBottom: 10 } }, "Your zkv address, share to give read access"), /* @__PURE__ */ React.createElement("div", { style: { fontFamily: "var(--font-mono)", fontSize: 12.5, color: "var(--fg-1)", wordBreak: "break-all", lineHeight: 1.55 } }, created.address), /* @__PURE__ */ React.createElement("div", { style: { display: "flex", gap: 8, marginTop: 12 } }, /* @__PURE__ */ React.createElement("button", { className: "btn secondary sm", onClick: () => navigator.clipboard.writeText(created.address) }, /* @__PURE__ */ React.createElement(Icon, { name: "copy", className: "icon" }), " Copy"))))), /* @__PURE__ */ React.createElement("div", { className: "modal-foot" }, counter, /* @__PURE__ */ React.createElement("div", { style: { display: "flex", gap: 8 } }, /* @__PURE__ */ React.createElement("button", { className: "btn primary", onClick: () => onComplete && onComplete(created.name) }, "Open ", created.name, " \u2192")))))
  );
};
const ImportFlow = ({ onCancel, onWatch, onRestore, onComplete }) => {
  const [method, setMethod] = React.useState(null);
  const [addr, setAddr] = React.useState("");
  const [nickname, setNickname] = React.useState("");
  const [phrase, setPhrase] = React.useState("");
  const [busy, setBusy] = React.useState(false);
  const [error, setError] = React.useState(null);
  const [addrOrHeight, setAddrOrHeight] = React.useState("");
  const [network, setNetwork] = React.useState("mainnet");
  const [addrInfo, setAddrInfo] = React.useState(null);
  const [inspecting, setInspecting] = React.useState(false);
  const [phraseMatch, setPhraseMatch] = React.useState(null);
  const isZkvAddr = (a) => /^zkv(test|regtest)?1[a-z0-9]{20,}$/.test(a.trim().toLowerCase());
  const parsed = isZkvAddr(addr);
  const addrState = addr.length === 0 ? "empty" : parsed ? "parsed" : "invalid";
  const phraseWords = phrase.trim().split(/\s+/).filter(Boolean);
  const phraseOk = phraseWords.length === 24;
  const aoh = addrOrHeight.trim();
  const aohIsHeight = /^\d{1,9}$/.test(aoh);
  const aohState = aoh === "" ? "empty" : isZkvAddr(aoh) ? "addr" : aohIsHeight ? "height" : "invalid";
  const fromAddr = aohState === "addr" && !!addrInfo;
  const effNetwork = fromAddr ? addrInfo.network : network;
  const effPool = fromAddr ? addrInfo.pool : "orchard";
  const effBirthday = fromAddr ? addrInfo.birthday : aohState === "height" ? Number(aoh) : void 0;
  React.useEffect(() => {
    const v = addrOrHeight.trim();
    if (method !== "restore" || !isZkvAddr(v)) {
      setAddrInfo(null);
      setInspecting(false);
      return;
    }
    let alive = true;
    setInspecting(true);
    setAddrInfo(null);
    window.zkvApi.inspectAddress(v).then((info) => alive && setAddrInfo(info)).catch(() => alive && setAddrInfo(null)).finally(() => alive && setInspecting(false));
    return () => {
      alive = false;
    };
  }, [method, addrOrHeight]);
  React.useEffect(() => {
    if (method !== "restore" || !fromAddr || !phraseOk) {
      setPhraseMatch(null);
      return;
    }
    let alive = true;
    setPhraseMatch("checking");
    window.zkvApi.verifyPhrase(phrase.trim(), addrOrHeight.trim()).then((ok) => alive && setPhraseMatch(ok ? "match" : "mismatch")).catch(() => alive && setPhraseMatch("error"));
    return () => {
      alive = false;
    };
  }, [method, fromAddr, phrase, phraseOk, addrOrHeight]);
  const secondOk = aohState === "height" || aohState === "addr" && !!addrInfo && !inspecting;
  const matchGate = !fromAddr || phraseMatch === "match";
  const canRestore = phraseOk && !!nickname.trim() && secondOk && matchGate && !busy && !inspecting;
  const submitWatch = async () => {
    setBusy(true);
    setError(null);
    try {
      await onWatch(addr.trim(), nickname.trim());
      onComplete && onComplete();
    } catch (e) {
      setError(e);
      setBusy(false);
    }
  };
  const submitRestore = async () => {
    if (!canRestore) return;
    setBusy(true);
    setError(null);
    try {
      await onRestore(nickname.trim(), phrase.trim(), effNetwork, effPool, effBirthday);
      onComplete && onComplete();
    } catch (e) {
      setError(e);
      setBusy(false);
    }
  };
  const ErrBanner = () => error ? /* @__PURE__ */ React.createElement("div", { className: "callout-flow warn", style: { marginBottom: 16 } }, /* @__PURE__ */ React.createElement(Icon, { name: "alert-triangle", size: 16, color: "var(--amber-400)" }), /* @__PURE__ */ React.createElement("div", null, /* @__PURE__ */ React.createElement("strong", { style: { color: "inherit" } }, "Couldn't add the database."), /* @__PURE__ */ React.createElement(ErrorMessage, { message: error.message }))) : null;
  const dismiss = (e) => {
    if (e.target.classList.contains("modal-overlay") && !busy) onCancel();
  };
  if (!method) {
    return /* @__PURE__ */ React.createElement("div", { className: "modal-overlay", onClick: dismiss }, /* @__PURE__ */ React.createElement("div", { className: "modal", role: "dialog", "aria-labelledby": "import-title" }, /* @__PURE__ */ React.createElement("div", { className: "modal-head" }, /* @__PURE__ */ React.createElement("div", null, /* @__PURE__ */ React.createElement("div", { className: "eyebrow" }, "IMPORT DATABASE"), /* @__PURE__ */ React.createElement("h2", { id: "import-title" }, "Add a database")), /* @__PURE__ */ React.createElement("button", { className: "close", onClick: onCancel, title: "Close" }, /* @__PURE__ */ React.createElement(Icon, { name: "x", size: 16 }))), /* @__PURE__ */ React.createElement("div", { className: "modal-body" }, /* @__PURE__ */ React.createElement("div", { className: "onboard-paths", style: { maxWidth: "none" } }, /* @__PURE__ */ React.createElement("button", { className: "onboard-path", onClick: () => setMethod("watch") }, /* @__PURE__ */ React.createElement("div", { className: "pic" }, /* @__PURE__ */ React.createElement(Icon, { name: "eye", size: 18 })), /* @__PURE__ */ React.createElement("div", null, /* @__PURE__ */ React.createElement("div", { className: "pname" }, "Watch (read-only)"), /* @__PURE__ */ React.createElement("div", { className: "pdesc" }, "Paste a ", /* @__PURE__ */ React.createElement("code", null, "zkv1\u2026"), " address")), /* @__PURE__ */ React.createElement("div", { className: "parrow" }, /* @__PURE__ */ React.createElement(Icon, { name: "arrow-right", size: 16 }))), /* @__PURE__ */ React.createElement("button", { className: "onboard-path", onClick: () => setMethod("restore") }, /* @__PURE__ */ React.createElement("div", { className: "pic" }, /* @__PURE__ */ React.createElement(Icon, { name: "key-round", size: 18 })), /* @__PURE__ */ React.createElement("div", null, /* @__PURE__ */ React.createElement("div", { className: "pname" }, "Restore admin (write)"), /* @__PURE__ */ React.createElement("div", { className: "pdesc" }, "Enter a 24-word phrase")), /* @__PURE__ */ React.createElement("div", { className: "parrow" }, /* @__PURE__ */ React.createElement(Icon, { name: "arrow-right", size: 16 }))))), /* @__PURE__ */ React.createElement("div", { className: "modal-foot" }, /* @__PURE__ */ React.createElement("div", { className: "cost", style: { fontFamily: "var(--font-mono)", fontSize: 11, color: "var(--fg-3)" } }, /* @__PURE__ */ React.createElement("code", null, "zkv watch <addr>"), " \xB7 ", /* @__PURE__ */ React.createElement("code", null, "zkv restore")))));
  }
  if (method === "watch") {
    return /* @__PURE__ */ React.createElement("div", { className: "modal-overlay", onClick: dismiss }, /* @__PURE__ */ React.createElement("div", { className: "modal", role: "dialog", "aria-labelledby": "import-title" }, /* @__PURE__ */ React.createElement("div", { className: "modal-head" }, /* @__PURE__ */ React.createElement("div", null, /* @__PURE__ */ React.createElement("div", { className: "eyebrow" }, "IMPORT \xB7 WATCH"), /* @__PURE__ */ React.createElement("h2", { id: "import-title" }, "Add a read-only database"), /* @__PURE__ */ React.createElement("div", { style: { fontSize: 12.5, color: "var(--fg-3)", marginTop: 4 } }, "Paste a zkv address; sync starts at its birthday block.")), !busy && /* @__PURE__ */ React.createElement("button", { className: "close", onClick: onCancel, title: "Close" }, /* @__PURE__ */ React.createElement(Icon, { name: "x", size: 16 }))), /* @__PURE__ */ React.createElement("div", { className: "modal-body" }, /* @__PURE__ */ React.createElement("div", { className: "write-fields" }, /* @__PURE__ */ React.createElement(ErrBanner, null), /* @__PURE__ */ React.createElement("div", { className: "field-block" }, /* @__PURE__ */ React.createElement("label", null, "zkv address"), /* @__PURE__ */ React.createElement(
      "textarea",
      {
        className: "input addr-input",
        style: { minHeight: 78, fontFamily: "var(--font-mono)" },
        value: addr,
        onChange: (e) => setAddr(e.target.value),
        autoFocus: true
      }
    ), /* @__PURE__ */ React.createElement("div", { className: "addr-preview", "data-state": addrState, style: { marginTop: 8 } }, addrState === "empty" && /* @__PURE__ */ React.createElement("span", { className: "prev-empty" }, /* @__PURE__ */ React.createElement(Icon, { name: "link-2", size: 12 }), " paste an address starting with ", /* @__PURE__ */ React.createElement("code", null, "zkv1\u2026")), addrState === "invalid" && /* @__PURE__ */ React.createElement("span", { className: "prev-invalid" }, /* @__PURE__ */ React.createElement(Icon, { name: "alert-circle", size: 12 }), " not a valid zkv address, expected a ", /* @__PURE__ */ React.createElement("code", null, "zkv1\u2026"), " token"), addrState === "parsed" && /* @__PURE__ */ React.createElement("span", { className: "prev-ok" }, /* @__PURE__ */ React.createElement(Icon, { name: "check", size: 12 }), " looks like a zkv address \xB7 birthday resolved on add"))), /* @__PURE__ */ React.createElement("div", { className: "field-block" }, /* @__PURE__ */ React.createElement("label", null, "Nickname ", /* @__PURE__ */ React.createElement("span", { style: { color: "var(--fg-3)", fontWeight: 400 } }, "\xB7 local only, optional")), /* @__PURE__ */ React.createElement("input", { className: "input lg", value: nickname, onChange: (e) => setNickname(e.target.value), maxLength: 24 })), /* @__PURE__ */ React.createElement("div", { className: "callout-flow note" }, /* @__PURE__ */ React.createElement(Icon, { name: "info", size: 16, color: "var(--amber-400)" }), /* @__PURE__ */ React.createElement("div", null, "Sync starts at the address's birthday block. Older history is skipped automatically.")))), /* @__PURE__ */ React.createElement("div", { className: "modal-foot" }, /* @__PURE__ */ React.createElement("div", { className: "cost", style: { fontFamily: "var(--font-mono)", fontSize: 11, color: "var(--fg-3)" } }, "equivalent to ", /* @__PURE__ */ React.createElement("code", null, "zkv watch <addr>")), /* @__PURE__ */ React.createElement("div", { style: { display: "flex", gap: 8 } }, /* @__PURE__ */ React.createElement("button", { className: "btn secondary", onClick: () => setMethod(null), disabled: busy }, "\u2190 Back"), /* @__PURE__ */ React.createElement("button", { className: "btn primary", disabled: !parsed || busy, onClick: submitWatch }, busy ? /* @__PURE__ */ React.createElement(React.Fragment, null, /* @__PURE__ */ React.createElement("div", { className: "spinner" }), " Adding\u2026") : /* @__PURE__ */ React.createElement(React.Fragment, null, /* @__PURE__ */ React.createElement(Icon, { name: "download", className: "icon" }), " Add database"))))));
  }
  return /* @__PURE__ */ React.createElement("div", { className: "modal-overlay", onClick: dismiss }, /* @__PURE__ */ React.createElement("div", { className: "modal", role: "dialog", "aria-labelledby": "import-title" }, /* @__PURE__ */ React.createElement("div", { className: "modal-head" }, /* @__PURE__ */ React.createElement("div", null, /* @__PURE__ */ React.createElement("div", { className: "eyebrow" }, "IMPORT \xB7 RESTORE"), /* @__PURE__ */ React.createElement("h2", { id: "import-title" }, "Restore admin access"), /* @__PURE__ */ React.createElement("div", { style: { fontSize: 12.5, color: "var(--fg-3)", marginTop: 4 } }, "Enter your 24-word recovery phrase, plus the database's zkv address or its birthday height.")), !busy && /* @__PURE__ */ React.createElement("button", { className: "close", onClick: onCancel, title: "Close" }, /* @__PURE__ */ React.createElement(Icon, { name: "x", size: 16 }))), /* @__PURE__ */ React.createElement("div", { className: "modal-body" }, /* @__PURE__ */ React.createElement("div", { className: "write-fields" }, /* @__PURE__ */ React.createElement(ErrBanner, null), /* @__PURE__ */ React.createElement("div", { className: "callout-flow warn" }, /* @__PURE__ */ React.createElement(Icon, { name: "alert-triangle", size: 16, color: "var(--amber-400)" }), /* @__PURE__ */ React.createElement("div", null, /* @__PURE__ */ React.createElement("strong", { style: { color: "inherit" } }, "Do not use any seed phrase you have ever used in a Zcash wallet."), " ", "z:kv databases are not private, anyone with your ", /* @__PURE__ */ React.createElement("code", null, "zkv1"), " address can view the wallet and database's entire transaction history by design.")), /* @__PURE__ */ React.createElement("div", { className: "field-block" }, /* @__PURE__ */ React.createElement("label", null, "24-word recovery phrase"), /* @__PURE__ */ React.createElement(
    "textarea",
    {
      className: "input phrase-input",
      style: { minHeight: 80, fontFamily: "var(--font-mono)" },
      value: phrase,
      onChange: (e) => setPhrase(e.target.value),
      disabled: busy,
      autoFocus: true
    }
  ), /* @__PURE__ */ React.createElement("div", { className: "hint" }, phraseWords.length > 0 && phraseWords.length < 24 && /* @__PURE__ */ React.createElement("span", { style: { color: "var(--amber-400)" } }, phraseWords.length, " / 24 words"), phraseWords.length > 24 && /* @__PURE__ */ React.createElement("span", { style: { color: "var(--red-500)" } }, /* @__PURE__ */ React.createElement(Icon, { name: "x", size: 11 }), " ", phraseWords.length, " words, phrase is too long"), phraseOk && !fromAddr && /* @__PURE__ */ React.createElement("span", { style: { color: "var(--green-500)" } }, /* @__PURE__ */ React.createElement(Icon, { name: "check", size: 11 }), " 24 words detected"), phraseOk && fromAddr && (phraseMatch === "checking" || phraseMatch === null) && /* @__PURE__ */ React.createElement("span", { style: { color: "var(--fg-3)", display: "inline-flex", alignItems: "center", gap: 6 } }, /* @__PURE__ */ React.createElement("div", { className: "spinner" }), " checking against address\u2026"), phraseOk && fromAddr && phraseMatch === "match" && /* @__PURE__ */ React.createElement("span", { style: { color: "var(--green-500)" } }, /* @__PURE__ */ React.createElement(Icon, { name: "check", size: 11 }), " matches this address"), phraseOk && fromAddr && phraseMatch === "mismatch" && /* @__PURE__ */ React.createElement("span", { style: { color: "var(--red-500)" } }, /* @__PURE__ */ React.createElement(Icon, { name: "x", size: 11 }), " does not match this address"), phraseOk && fromAddr && phraseMatch === "error" && /* @__PURE__ */ React.createElement("span", { style: { color: "var(--amber-400)" } }, /* @__PURE__ */ React.createElement(Icon, { name: "alert-circle", size: 11 }), " couldn't verify against the address"))), /* @__PURE__ */ React.createElement("div", { className: "field-block" }, /* @__PURE__ */ React.createElement("label", null, "zkv address or birthday height ", /* @__PURE__ */ React.createElement("span", { style: { color: "var(--fg-3)", fontWeight: 400 } }, "\xB7 required")), /* @__PURE__ */ React.createElement(
    "textarea",
    {
      className: "input addr-input",
      style: { minHeight: 60, fontFamily: "var(--font-mono)" },
      value: addrOrHeight,
      onChange: (e) => setAddrOrHeight(e.target.value),
      disabled: busy
    }
  ), /* @__PURE__ */ React.createElement("div", { className: "addr-preview", "data-state": fromAddr || aohState === "height" ? "parsed" : aohState === "invalid" ? "invalid" : "empty", style: { marginTop: 8 } }, aohState === "empty" && /* @__PURE__ */ React.createElement("span", { className: "prev-empty" }, /* @__PURE__ */ React.createElement(Icon, { name: "link-2", size: 12 }), " required: paste the database's ", /* @__PURE__ */ React.createElement("code", null, "zkv1\u2026"), " address, or enter its birthday height"), aohState === "invalid" && /* @__PURE__ */ React.createElement("span", { className: "prev-invalid" }, /* @__PURE__ */ React.createElement(Icon, { name: "alert-circle", size: 12 }), " enter a ", /* @__PURE__ */ React.createElement("code", null, "zkv1\u2026"), " address or a block height"), aohState === "height" && /* @__PURE__ */ React.createElement("span", { className: "prev-ok", style: { display: "inline-flex", alignItems: "center", gap: 6 } }, /* @__PURE__ */ React.createElement(Icon, { name: "check", size: 12 }), " birthday height ", Number(aoh)), aohState === "addr" && inspecting && /* @__PURE__ */ React.createElement("span", { className: "prev-empty" }, /* @__PURE__ */ React.createElement("div", { className: "spinner" }), " reading network and pool\u2026"), aohState === "addr" && !inspecting && !addrInfo && /* @__PURE__ */ React.createElement("span", { className: "prev-invalid" }, /* @__PURE__ */ React.createElement(Icon, { name: "alert-circle", size: 12 }), " couldn't read this address; check it and try again"), aohState === "addr" && !inspecting && addrInfo && /* @__PURE__ */ React.createElement("span", { className: "prev-ok", style: { display: "inline-flex", alignItems: "center", gap: 6 } }, /* @__PURE__ */ React.createElement(Icon, { name: "check", size: 12 }), /* @__PURE__ */ React.createElement("span", { className: "net-badge", "data-net": addrInfo.network }, addrInfo.network), /* @__PURE__ */ React.createElement("span", { className: "db-pool" }, addrInfo.pool), /* @__PURE__ */ React.createElement("span", null, "\xB7 birthday ", addrInfo.birthday)))), !fromAddr && /* @__PURE__ */ React.createElement("div", { className: "field-block" }, /* @__PURE__ */ React.createElement("label", null, "Network"), /* @__PURE__ */ React.createElement("div", { className: "seg" }, /* @__PURE__ */ React.createElement("button", { className: network === "mainnet" ? "on" : "", disabled: busy, onClick: () => setNetwork("mainnet") }, /* @__PURE__ */ React.createElement(Icon, { name: "globe", size: 12 }), " mainnet"), /* @__PURE__ */ React.createElement("button", { className: network === "testnet" ? "on" : "", disabled: busy, onClick: () => setNetwork("testnet") }, /* @__PURE__ */ React.createElement(Icon, { name: "flask-conical", size: 12 }), " testnet"))), /* @__PURE__ */ React.createElement("div", { className: "field-block" }, /* @__PURE__ */ React.createElement("label", null, "Local database nickname"), /* @__PURE__ */ React.createElement("input", { className: "input lg", value: nickname, onChange: (e) => setNickname(e.target.value), maxLength: 24, disabled: busy })))), /* @__PURE__ */ React.createElement("div", { className: "modal-foot" }, /* @__PURE__ */ React.createElement("div", { className: "cost", style: { fontFamily: "var(--font-mono)", fontSize: 11, color: "var(--fg-3)" } }, "equivalent to ", /* @__PURE__ */ React.createElement("code", null, "zkv restore ", nickname || "<name>")), /* @__PURE__ */ React.createElement("div", { style: { display: "flex", gap: 8 } }, /* @__PURE__ */ React.createElement("button", { className: "btn secondary", onClick: () => setMethod(null), disabled: busy }, "\u2190 Back"), /* @__PURE__ */ React.createElement("button", { className: "btn primary", disabled: !canRestore, onClick: submitRestore }, busy ? /* @__PURE__ */ React.createElement(React.Fragment, null, /* @__PURE__ */ React.createElement("div", { className: "spinner" }), " Restoring\u2026") : /* @__PURE__ */ React.createElement(React.Fragment, null, /* @__PURE__ */ React.createElement(Icon, { name: "key-round", className: "icon" }), " Restore"))))));
};
window.CreateFlow = CreateFlow;
window.ImportFlow = ImportFlow;
window.Qr = Qr;
window.DepositModal = DepositModal;
window.SendModal = SendModal;
