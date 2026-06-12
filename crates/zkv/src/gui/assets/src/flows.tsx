// flows.jsx: CreateFlow (init wizard) + ImportFlow (watch / restore)
// Both render as modal dialogs (matching WriteFlow), wired to the live API:
// real mnemonic + funding address from the server, real balance polling,
// real INIT broadcast, real watch/restore.

// Real, scannable QR for `text`, rendered server-side by the gui's
// /api/qr endpoint (the `qrcode` crate) and inlined here. Fetched with the
// session token, so it can't be an <img src>. Shows a spinner while loading
// and a terse note if the endpoint is unavailable (the address is always
// shown as copyable text beside it, so nothing is lost).
const Qr = ({ text }: { text: string }) => {
  const [svg, setSvg] = React.useState<string | null>(null);
  const [failed, setFailed] = React.useState(false);
  React.useEffect(() => {
    let alive = true;
    setSvg(null);
    setFailed(false);
    if (!text) return;
    window.zkvApi
      .qr(text)
      .then((r) => alive && setSvg(r.svg))
      .catch(() => alive && setFailed(true));
    return () => {
      alive = false;
    };
  }, [text]);

  if (failed)
    return (
      <div style={{ display: "grid", placeItems: "center", width: "100%", height: "100%", padding: 8, textAlign: "center", color: "var(--fg-3)", fontSize: 11 }}>
        QR unavailable, use the address below
      </div>
    );
  if (!svg)
    return (
      <div style={{ display: "grid", placeItems: "center", width: "100%", height: "100%" }}>
        <div className="spinner" />
      </div>
    );
  return <div style={{ width: "100%", height: "100%" }} dangerouslySetInnerHTML={{ __html: svg }} />;
};

// A reassuring "Is this a valid Zcash address?" link shown under any funding
// QR. zkv addresses are shielded (Orchard) UAs, which some wallets/tools
// wrongly flag as invalid. Opens a small info dialog.
const ValidAddressInfo = () => {
  const [open, setOpen] = React.useState(false);
  return (
    <>
      <button
        type="button"
        onClick={() => setOpen(true)}
        style={{ background: "none", border: "none", padding: 0, marginTop: 4, color: "var(--fg-link)", cursor: "pointer", fontSize: 11, fontFamily: "inherit" }}
      >
        Is this a valid Zcash address?
      </button>
      {open && (
        <div className="modal-overlay" style={{ zIndex: 1000 }}
             onClick={(e) => {
               // This info dialog stacks on top of the funding modal. Swallow the
               // click so clicking its dimmed backdrop dismisses only this dialog,
               // not the modal underneath (whose overlay would otherwise catch the
               // same bubbling click and close too).
               e.stopPropagation();
               if ((e.target as HTMLElement).classList.contains("modal-overlay")) setOpen(false);
             }}>
          <div className="modal" role="dialog" style={{ maxWidth: 470 }}>
            <div className="modal-head">
              <h2 id="valid-addr-title" style={{ fontSize: 18 }}>Is this a valid Zcash address?</h2>
              <button className="close" onClick={() => setOpen(false)} title="Close"><Icon name="x" size={16} /></button>
            </div>
            <div className="modal-body" style={{ fontSize: 13.5, color: "var(--fg-2)", lineHeight: 1.6 }}>
              <p style={{ marginTop: 0 }}>If someone tells you that this Zcash address is invalid, that's okay! That means that they don't support shielded Zcash. Unfortunate.</p>
              <p>To receive transparent Zcash from them, install a Zcash wallet on your mobile phone, then use it to send the Zcash into the database's shielded address, shown in the QR code within this app, once it confirms.</p>
              <p style={{ marginBottom: 0 }}>This application only supports shielded Zcash.</p>
            </div>
            <div className="modal-foot">
              <div></div>
              <button className="btn primary" onClick={() => setOpen(false)}>Got it</button>
            </div>
          </div>
        </div>
      )}
    </>
  );
};

// Reusable "add funds" modal: shows a database's Orchard funding address as a
// scannable QR + copyable text. Opened from the status-bar balance and from
// any insufficient-funds error. `db` is the active database summary
// ({ name, network, funding_address }).
const DepositModal = ({ db, onClose, onCopy, onInited }: {
  db: ActiveDb;
  onClose: () => void;
  onCopy: (s: string) => void;
  // Called after a successful faucet-driven INIT so the host can hand off to
  // the "waiting for INIT" view. Testnet-only path.
  onInited?: () => void;
}) => {
  const addr = db && db.funding_address;
  const cur = window.currencyFor(db && db.network);
  const feeAmt = window.formatZats(10000, db && db.network);
  // idle | requesting | done | retry | outdated. The faucet call is proxied
  // through the Rust backend (no browser CORS; errors logged under RUST_LOG)
  // and returns { outcome }. Any failure latches the button to retry/outdated
  // and leaves it disabled; reopening the modal mounts a fresh component, which
  // resets this back to "idle".
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
  const faucetLabel =
    faucetState === "requesting" ? "Requesting…"
      : faucetState === "done" ? "Requested"
        : faucetState === "retry" ? "Try again later"
          : faucetState === "outdated" ? "Your app is outdated"
            : "Request from faucet";

  // Testnet-only "Use our faucet": the backend signs this database's INIT memo
  // and hands it to the faucet to broadcast (it pays the fee), so an unfunded
  // testnet database can initialize. idle | requesting | retry | outdated. On
  // success we don't relabel: onInited tears the modal down and moves to the
  // waiting-for-INIT view.
  const showInitFaucet = !!(addr && db.network === "testnet" && db.init === "uninitialized");
  const [initState, setInitState] = React.useState("idle");
  const initViaFaucet = async () => {
    setInitState("requesting");
    try {
      const r = await window.zkvApi.faucetInit(db.name);
      if (r.outcome === "outdated") { setInitState("outdated"); return; }
      if (r.outcome !== "ok") { setInitState("retry"); return; }
      onInited && onInited();
    } catch (_) {
      setInitState("retry");
    }
  };
  const initLabel =
    initState === "requesting" ? "Initializing…"
      : initState === "retry" ? "Try again later"
        : initState === "outdated" ? "Your app is outdated"
          : "Use our faucet";
  return (
    <div
      className="modal-overlay"
      onClick={(e) => {
        if ((e.target as HTMLElement).classList.contains("modal-overlay")) onClose();
      }}
    >
      <div className="modal" role="dialog" style={{ maxWidth: 420 }}>
        <div className="modal-head">
          <div>
            <div className="eyebrow">DB: {db.name}</div>
            <h2>Add {db.network} funds</h2>
            <div style={{ fontSize: 12.5, color: "var(--fg-3)", marginTop: 4 }}>
              Send {cur} to this database's funding address. Each write costs about {feeAmt},
              just the network fee.
            </div>
          </div>
          <button className="close" onClick={onClose} title="Close">
            <Icon name="x" size={16} />
          </button>
        </div>
        <div className="modal-body">
          {addr ? (
            <>
              <div className="qr-card" style={{ margin: "0 auto", maxWidth: 300, border: "none", background: "transparent", padding: 0 }}>
                <div className="qr">
                  <Qr text={addr} />
                </div>
              </div>
              <div className="kv-row" style={{ marginTop: 16 }}>
                <span className="label" style={{ minWidth: 110 }}>Funding address</span>
                <span className="value"><CollapsibleString value={addr} onCopy={onCopy} /></span>
              </div>
              {db.network === "mainnet" && (
                <div style={{ marginTop: 4 }}><ValidAddressInfo /></div>
              )}
              <div className="kv-row" style={{ marginTop: 10 }}>
                <span className="label" style={{ minWidth: 110 }}>Spendable</span>
                <span className="value mono">
                  {/* `balance` IS the spendable figure (confirming is reported
                      disjointly by the backend), so no subtraction here. */}
                  {db.balance != null
                    ? window.formatZats(db.balance, db.network)
                    : "—"}
                  {db.confirming ? (
                    <span style={{ color: "var(--fg-3)" }}> (+ {window.formatZats(db.confirming, db.network)} confirming)</span>
                  ) : null}
                </span>
              </div>
            </>
          ) : (
            <div className="callout-flow warn">
              <Icon name="alert-triangle" size={16} color="var(--amber-400)" />
              <div>Watch-only databases hold no spending key, so there's nothing to fund.</div>
            </div>
          )}
        </div>
        <div className="modal-foot">
          <div className="cost"></div>
          <div style={{ display: "flex", gap: 8 }}>
            {showInitFaucet && (
              <button
                className="btn secondary"
                onClick={initViaFaucet}
                disabled={initState !== "idle"}
              >
                <Icon name="rocket" className="icon" /> {initLabel}
              </button>
            )}
            {addr && db.network !== "mainnet" && (
              <button
                className="btn secondary"
                onClick={requestFaucet}
                disabled={faucetState !== "idle"}
              >
                <Icon name="droplets" className="icon" /> {faucetLabel}
              </button>
            )}
            <button className="btn primary" onClick={onClose}>Done</button>
          </div>
        </div>
      </div>
    </div>
  );
};

// Send modal: a plain ZEC value transfer to any Zcash address librustzcash
// supports (transparent, Sapling, unified, or TEX). Distinct from a zkv write,
// which is a zero-value memo to the database's own address. This spends real
// funds. The recipient is validated live against the database's network
// (zkvApi.checkAddress) before Send enables; the amount stays a decimal ZEC
// string and is parsed to zatoshi server-side (no float rounding). Steps:
// form → review → sending → done.
const SendModal = ({ db, onClose, onCopy, onDeposit, onDone }: {
  db: ActiveDb;
  onClose: () => void;
  onCopy: (s: string) => void;
  onDeposit?: () => void;
  onDone?: () => void;
}) => {
  const [step, setStep] = React.useState("form"); // form | review | sending | done
  const [recipient, setRecipient] = React.useState("");
  const [amount, setAmount] = React.useState("");
  const [check, setCheck] = React.useState<AddrCheckResp | null>(null);
  const [checking, setChecking] = React.useState(false);
  const [error, setError] = React.useState<any>(null);
  const [txHash, setTxHash] = React.useState("");
  const [memo, setMemo] = React.useState("");

  const cur = window.currencyFor(db && db.network);
  const FEE_FLOOR = 10000;

  // Live recipient validation, debounced. An empty field clears the verdict;
  // otherwise checkAddress reports { valid, kind } or a friendly reason.
  React.useEffect(() => {
    const addr = recipient.trim();
    if (!addr) { setCheck(null); setChecking(false); return; }
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
    return () => { cancelled = true; clearTimeout(t); };
  }, [recipient, db.name]);

  // Amount validation mirrors the backend's parse_zec: a decimal with at most
  // 8 fractional digits, greater than zero. Kept in ZEC for display; the
  // backend does the authoritative zatoshi conversion.
  const amtTrim = amount.trim();
  const amtMatch = /^\d*\.?\d*$/.test(amtTrim) && amtTrim !== "" && amtTrim !== ".";
  const amtZats = amtMatch ? Math.round(parseFloat(amtTrim) * 1e8) : 0;
  const tooManyDecimals = (amtTrim.split(".")[1] || "").length > 8;
  const amtOk = amtMatch && !tooManyDecimals && amtZats > 0;
  const haveBal = db.balance != null;
  const overBalance = !!(haveBal && amtOk && amtZats + FEE_FLOOR > db.balance!);

  // Optional ZIP-302 text memo, capped at 512 bytes (UTF-8). Memos only ride on
  // shielded outputs, so a transparent or TEX recipient (on any network) can't
  // carry one: disable the field and never send a memo to those.
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
      setError(e); // stay on the 'sending' step to show the error view
    }
  };

  // ---- FORM step ----
  const FormStep = () => (
    <>
      <div className="modal-body">
        <div className="write-fields">
          <div className="field-block">
            <label>Recipient address</label>
            <input
              className="input mono lg"
              value={recipient}
              onChange={(e) => setRecipient(e.target.value)}
              placeholder={`(${db.network})`}
              autoFocus
            />
            <div className="byte-counter">
              <span className={!recipient.trim() || checking ? "" : check && check.valid ? "ok" : "err"}>
                {!recipient.trim() ? "required"
                  : checking ? "checking…"
                  : check && check.valid
                    ? `valid ${check.kind} address (${[check.network, check.pool].filter(Boolean).join(", ")})`
                  : check && check.error ? check.error
                  : ""}
              </span>
            </div>
          </div>

          <div className="field-block">
            <label>Amount ({cur})</label>
            <input
              className="input mono lg"
              value={amount}
              onChange={(e) => setAmount(e.target.value)}
              placeholder="0.00"
              inputMode="decimal"
            />
            <div className="byte-counter">
              <span className={amtTrim === "" ? "" : amtOk && !overBalance ? "ok" : "err"}>
                {amtTrim === "" ? "required"
                  : tooManyDecimals ? "at most 8 decimal places"
                  : !amtMatch || amtZats <= 0 ? "enter an amount greater than zero"
                  : overBalance ? "more than the available balance"
                  : ""}
              </span>
              <span style={{ color: "var(--green-500)", fontFamily: "var(--font-mono)" }}>
                {/* `balance` is the spendable figure; label it as such so a
                    wallet with funds still confirming isn't confusing here. */}
                spendable {haveBal ? window.formatZats(db.balance, db.network) : "—"}
              </span>
            </div>
          </div>

          <div className="field-block">
            <label>
              Memo <span style={{ color: "var(--fg-3)", fontWeight: 400 }}>· optional</span>
            </label>
            <textarea
              className="input mono"
              value={memo}
              onChange={(e) => setMemo(e.target.value)}
              placeholder={recipientIsTransparent ? "Transparent recipients can't carry a memo" : "Optional message sent with the payment"}
              rows={3}
              disabled={recipientIsTransparent}
              style={{ resize: "vertical", opacity: recipientIsTransparent ? 0.55 : 1 }}
            />
            <div className="byte-counter">
              <span className={memoOk ? "" : "err"}>
                {recipientIsTransparent ? "memo not supported for this recipient" : !memoOk ? "memo is over the 512-byte limit" : ""}
              </span>
              <span style={{ color: memoOk ? "var(--fg-3)" : "var(--red-500)" }}>{memoBytes}/512</span>
            </div>
          </div>
        </div>
      </div>

      <div className="modal-foot">
        <div className="cost">
          {overBalance ? (
            <>
              <Icon name="alert-triangle" size={11} color="var(--red-500)" />
              <span style={{ color: "var(--red-500)" }}>insufficient funds</span>
              {onDeposit && (
                <button className="btn ghost sm" style={{ marginLeft: 4 }} onClick={onDeposit}>
                  <Icon name="download" size={12} /> Deposit
                </button>
              )}
            </>
          ) : (
            <>
              <Icon name="zap" size={11} />
              <span>Spends {cur}</span>
            </>
          )}
        </div>
        <div style={{ display: "flex", gap: 8 }}>
          <button className="btn secondary" onClick={onClose}>Cancel</button>
          <button className="btn primary" disabled={!canSend} onClick={() => setStep("review")}>
            Review →
          </button>
        </div>
      </div>
    </>
  );

  // ---- REVIEW step ----
  const ReviewStep = () => (
    <>
      <div className="modal-body">
        <div style={{ fontSize: 13, color: "var(--fg-2)", marginBottom: 14, lineHeight: 1.5 }}>
          Sending {cur} from <strong style={{ color: "var(--fg-1)" }}>{db.name}</strong>.
        </div>
        <div style={{ padding: "14px 16px", background: "var(--bg-sunken)", border: "1px solid var(--border-1)", borderRadius: "var(--radius-md)" }}>
          <div className="kv-row"><span className="label">To</span><span className="value"><CollapsibleString value={recipient.trim()} onCopy={onCopy} /></span></div>
          <div className="kv-row"><span className="label">Amount</span><span className="value">{window.formatZats(amtZats, db.network)}</span></div>
          <div className="kv-row"><span className="label">Fee</span><span className="value">~{window.formatZats(FEE_FLOOR, db.network)}</span></div>
          <div className="kv-row"><span className="label">Network</span><span className="value">{db.network}</span></div>
        </div>
      </div>
      <div className="modal-foot">
        <div></div>
        <div style={{ display: "flex", gap: 8 }}>
          <button className="btn secondary" onClick={() => setStep("form")}>← Back</button>
          <button className="btn primary" onClick={doSend}><Icon name="send" className="icon" /> Confirm send</button>
        </div>
      </div>
    </>
  );

  // ---- SENDING step (spinner, or the error view on failure) ----
  const SendingStep = () => {
    if (error) {
      const code = error.code;
      return (
        <>
          <div className="modal-body">
            <div className="callout-flow warn">
              <Icon name="alert-triangle" size={16} color="var(--amber-400)" />
              <div>
                <strong style={{ color: "inherit" }}>
                  {code === "insufficient_funds" ? "Not enough funds to send."
                    : code === "watch_only" ? "This is a watch-only database."
                    : "Send failed."}
                </strong>
                <div style={{ marginTop: 4, fontSize: 12.5, color: "var(--fg-2)" }}>{error.message}</div>
                {code === "insufficient_funds" && error.data && (
                  <div style={{ marginTop: 6, fontFamily: "var(--font-mono)", fontSize: 11.5, color: "var(--fg-3)" }}>
                    {window.formatZats(error.data.available, db.network)} available ·{" "}
                    {window.formatZats(error.data.required, db.network)} needed
                  </div>
                )}
              </div>
            </div>
          </div>
          <div className="modal-foot">
            <div className="cost"><Icon name="x" size={11} /> <span>not sent</span></div>
            <div style={{ display: "flex", gap: 8 }}>
              {code === "insufficient_funds" && onDeposit && (
                <button className="btn secondary" onClick={onDeposit}><Icon name="download" className="icon" /> Deposit</button>
              )}
              <button className="btn primary" onClick={() => { setError(null); setStep("form"); }}>← Back to form</button>
            </div>
          </div>
        </>
      );
    }
    return (
      <div className="modal-body">
        <div style={{ display: "flex", gap: 10, alignItems: "center", fontSize: 13, color: "var(--fg-2)", padding: "8px 0" }}>
          <div className="spinner" />
          <span>Signing and broadcasting to {db.network}…</span>
        </div>
      </div>
    );
  };

  // ---- DONE step ----
  const DoneStep = () => (
    <>
      <div className="modal-body">
        <div className="confirmed-banner">
          <div style={{ width: 32, height: 32, borderRadius: 999, background: "var(--green-300)", display: "grid", placeItems: "center", color: "#fff", flexShrink: 0 }}>
            <Icon name="check" size={18} />
          </div>
          <div>
            <div style={{ fontWeight: 600, color: "inherit" }}>Sent {window.formatZats(amtZats, db.network)}</div>
            <div style={{ fontSize: 12, color: "var(--fg-2)" }}>Broadcast to {db.network}. It’ll confirm in a few minutes.</div>
          </div>
        </div>
        <div style={{ marginTop: 16, padding: "14px 16px", background: "var(--bg-sunken)", border: "1px solid var(--border-1)", borderRadius: "var(--radius-md)" }}>
          <div style={{ fontFamily: "var(--font-mono)", fontSize: 11, letterSpacing: "0.08em", color: "var(--fg-3)", textTransform: "uppercase", marginBottom: 8 }}>Receipt</div>
          <div className="kv-row"><span className="label">To</span><span className="value"><CollapsibleString value={recipient.trim()} onCopy={onCopy} /></span></div>
          <div className="kv-row"><span className="label">Amount</span><span className="value">{window.formatZats(amtZats, db.network)}</span></div>
          <div className="kv-row"><span className="label">TXID</span><span className="value">{txHash ? <CollapsibleString value={txHash} onCopy={onCopy} /> : "—"}</span></div>
        </div>
      </div>
      <div className="modal-foot">
        <div className="cost"><Icon name="shield-check" size={11} color="var(--green-300)" /> <span>broadcast</span></div>
        <div style={{ display: "flex", gap: 8 }}>
          <button className="btn primary" onClick={onClose}>Done</button>
        </div>
      </div>
    </>
  );

  // Lock the modal (no backdrop/close) only while a send is actually in flight.
  const locked = step === "sending" && !error;
  return (
    <div className="modal-overlay" onClick={(e) => { if ((e.target as HTMLElement).classList.contains("modal-overlay") && !locked) onClose(); }}>
      <div className="modal" role="dialog" aria-labelledby="send-title" style={{ maxWidth: 460 }}>
        <div className="modal-head">
          <div>
            <div className="eyebrow">DB: {db.name}</div>
            <h2 id="send-title">Send {cur}</h2>
          </div>
          {!locked && (
            <button className="close" onClick={onClose} title="Close"><Icon name="x" size={16} /></button>
          )}
        </div>
        {/* Call as functions, not <FormStep/>, so typing reconciles the same
            inputs in place instead of remounting them (which steals focus). */}
        {step === "form" && FormStep()}
        {step === "review" && ReviewStep()}
        {step === "sending" && SendingStep()}
        {step === "done" && DoneStep()}
      </div>
    </div>
  );
};

// ============================================================
// CREATE FLOW: init a new admin database (real), as a modal
// ============================================================
const CreateFlow = ({ onCancel, onCreate, onGeneratePhrase, onInit, pollDb, minInitZats = 10000, onComplete, servers, existingNames }: {
  onCancel: () => void;
  onCreate: (name: string, network: string, pool: string, phrase: string) => Promise<CreateResp>;
  onGeneratePhrase: () => Promise<string>;
  onInit: (name: string) => Promise<unknown>;
  pollDb: (name: string) => Promise<DbDetail>;
  minInitZats?: number;
  onComplete?: (name: string) => void;
  servers?: ServersResp | null;
  existingNames?: string[];
}) => {
  const [step, setStep] = React.useState(0);
  const [name, setName] = React.useState("");
  const [network, setNetwork] = React.useState("mainnet");
  const [pool, setPool] = React.useState("orchard");
  // Until the user picks a pool explicitly, it tracks the network's default
  // (see `defaultPoolFor`).
  const [poolTouched, setPoolTouched] = React.useState(false);
  // The generated recovery phrase. Held in memory only until the user confirms
  // it (step 2); the database directory is not persisted before then.
  const [phrase, setPhrase] = React.useState<string | null>(null);
  const [busy, setBusy] = React.useState(false);
  const [error, setError] = React.useState<any>(null);

  // Filled by the server once the database is created.
  const [created, setCreated] = React.useState<CreateResp | null>(null);
  const [acked, setAcked] = React.useState(false);
  const [copiedPhrase, setCopiedPhrase] = React.useState(false);
  const [confirmWords, setConfirmWords] = React.useState<Record<number, string>>({ 3: "", 11: "", 19: "" });

  // Funding / INIT progress.
  const [balance, setBalance] = React.useState<any>(null);
  const [confirming, setConfirming] = React.useState(0);
  const [initState, setInitState] = React.useState("uninitialized");
  // Step 4 progress: whether the signed INIT memo has been broadcast, and the
  // confirmation count toward the read threshold (e.g. 0/3 → 3/3).
  const [broadcast, setBroadcast] = React.useState(false);
  const [initDone, setInitDone] = React.useState(0);
  const [initRequired, setInitRequired] = React.useState(0);

  const words = phrase ? phrase.trim().split(/\s+/) : [];
  const confirmOk =
    words.length === 24 &&
    confirmWords[3] === words[3] &&
    confirmWords[11] === words[11] &&
    confirmWords[19] === words[19];

  // Orchard is live on both mainnet and testnet (NU6.2 has activated on
  // testnet), so it is the default pool everywhere.
  const defaultPoolFor = (_net: string) => "orchard";
  const selectNetwork = (net: string) => {
    setNetwork(net);
    if (!poolTouched) setPool(defaultPoolFor(net));
  };
  // Ironwood is an upcoming Zcash shielded pool. It is surfaced as a preview
  // option only: it exists on testnet alone and cannot yet be used to create a
  // database, so selecting it pins the network to testnet (shown greyed) and
  // blocks the rest of the flow.
  const ironwoodSelected = pool === "ironwood";
  const selectPool = (p: string) => {
    setPool(p);
    setPoolTouched(true);
    if (p === "ironwood") setNetwork("testnet");
  };

  // Local db-name validation, mirrors the backend's data::validate_db_name:
  // ASCII letters, digits, '-' and '_', 1..24 chars, and not already in use.
  const nameOk = /^[A-Za-z0-9_-]{1,24}$/.test(name) && !(existingNames || []).includes(name);

  const stepDots = ["Configure", "Seed phrase", "Confirm", "Fund", "INIT", "Done"];
  const stepSubs = [
    "Pick a local name and network.",
    "",
    "Confirm a few words to prove you saved the phrase.",
    "Fund the database's address. Each write costs only the network fee.",
    "Broadcasting the INIT memo that opens this database for writes.",
    "Done, your database is initialized and ready for writes.",
  ];

  // Step 0 → generate the recovery phrase only (NO persistence: the database
  // directory and the sidebar entry appear only once the seed is confirmed in
  // step 2). Reuse an already-generated phrase if the user stepped back, so the
  // confirm words they'll be asked for don't change underfoot.
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

  // Step 2 → persist the database now that the seed is confirmed. Guarded on
  // `created` so stepping back to Fund and forward again doesn't re-create.
  const doPersist = async () => {
    if (created) {
      setStep(3);
      return;
    }
    setBusy(true);
    setError(null);
    try {
      const r = await onCreate(name.trim(), network, pool, phrase!);
      setCreated(r);
      setStep(3);
    } catch (e) {
      setError(e);
    } finally {
      setBusy(false);
    }
  };

  // Poll the wallet balance while on the Fund step.
  React.useEffect(() => {
    if (step !== 3 || !created) return;
    let live = true;
    const tick = async () => {
      try {
        const d = await pollDb(created!.name);
        if (!live) return;
        setBalance(d.balance);
        setConfirming(d.confirming || 0);
        setInitState(d.init);
      } catch (_) {}
    };
    tick();
    const id = setInterval(tick, 7000);
    return () => {
      live = false;
      clearInterval(id);
    };
  }, [step, created, pollDb]);

  // Only *spendable* funds can pay for the INIT. `balance` is already the
  // spendable figure (the backend reports confirming disjointly).
  const spendable = balance != null ? balance : 0;
  const funded = balance != null && spendable >= minInitZats;
  // Funding-step state, drives the status callout + button.
  const fundingState =
    balance == null ? "checking"
    : funded ? "ready"
    : (confirming || 0) > 0 ? "confirming"
    : spendable > 0 ? "short"
    : "waiting";

  // Step 4 → broadcast INIT, then poll until initialized.
  const doInit = async () => {
    setBusy(true);
    setError(null);
    setBroadcast(false);
    setInitDone(0);
    setStep(4);
    try {
      await onInit(created!.name);
    } catch (e) {
      setError(e);
      setBusy(false);
      return;
    }
    setBroadcast(true);
    // Poll until the INIT confirms at the read threshold, updating the
    // confirmation counter (done/required) as it climbs.
    const id = setInterval(async () => {
      try {
        const d = await pollDb(created!.name);
        setInitState(d.init);
        setInitDone(d.init_done);
        if (d.init_required) setInitRequired(d.init_required);
        if (d.init === "initialized") {
          clearInterval(id);
          setBusy(false);
          // INIT confirmed at the read threshold: dismiss the modal and open
          // the database. Fall back to the done screen if no onComplete.
          if (onComplete) onComplete(created!.name);
          else setStep(5);
        }
      } catch (_) {}
    }, 7000);
  };

  // Testnet-only alternative to "Broadcast INIT": sign the INIT memo locally
  // and hand it to the faucet, which broadcasts it (and pays the fee), so the
  // database can initialize without being funded first. On success we advance
  // to the same INIT-waiting step and poll until confirmed. idle | requesting |
  // retry | outdated.
  const [faucetInitState, setFaucetInitState] = React.useState("idle");
  const doFaucetInit = async () => {
    setFaucetInitState("requesting");
    setError(null);
    try {
      const r = await window.zkvApi.faucetInit(created!.name);
      if (r.outcome === "outdated") { setFaucetInitState("outdated"); return; }
      if (r.outcome !== "ok") { setFaucetInitState("retry"); return; }
      setBroadcast(true);
      setInitDone(0);
      setStep(4);
      const id = setInterval(async () => {
        try {
          const d = await pollDb(created!.name);
          setInitState(d.init);
          setInitDone(d.init_done);
          if (d.init_required) setInitRequired(d.init_required);
          if (d.init === "initialized") {
            clearInterval(id);
            if (onComplete) onComplete(created!.name);
            else setStep(5);
          }
        } catch (_) {}
      }, 7000);
    } catch (_) {
      setFaucetInitState("retry");
    }
  };
  const faucetInitLabel =
    faucetInitState === "requesting" ? "Initializing…"
      : faucetInitState === "retry" ? "Try again later"
        : faucetInitState === "outdated" ? "Your app is outdated"
          : "Use our faucet";

  // Shared error banner, rendered at the top of each step's body.
  const errBanner = error ? (
    <div className="callout-flow warn" style={{ marginBottom: 16 }}>
      <Icon name="alert-triangle" size={16} color="var(--amber-400)" />
      <div>
        <strong style={{ color: "inherit" }}>Something went wrong.</strong>
        <ErrorMessage message={error.message} />
      </div>
    </div>
  ) : null;

  const counter = (
    <div className="cost" style={{ fontFamily: "var(--font-mono)", fontSize: 11, color: "var(--fg-3)" }}>
      step {step + 1} / {stepDots.length}
    </div>
  );

  return (
    <div
      className="modal-overlay"
      onClick={(e) => {
        if ((e.target as HTMLElement).classList.contains("modal-overlay") && !busy) onCancel();
      }}
    >
      <div className="modal create-modal" role="dialog" aria-labelledby="create-title">
        <div className="modal-head">
          <div>
            <div className="eyebrow">CREATE DATABASE · {stepDots[step].toUpperCase()}</div>
            <h2 id="create-title">{step === 1 ? "Database Seed Phrase" : stepDots[step]}</h2>
            {stepSubs[step] && <div style={{ fontSize: 12.5, color: "var(--fg-3)", marginTop: 4 }}>{stepSubs[step]}</div>}
          </div>
          {!busy && (
            <button className="close" onClick={onCancel} title="Close">
              <Icon name="x" size={16} />
            </button>
          )}
        </div>

        <div className="modal-stepper">
          {stepDots.map((s, i) => (
            <div key={i} className={"step-dot " + (i < step ? "done" : i === step ? "active" : "")}>
              <span>{i < step ? "✓" : i + 1}</span>
              <span className="dot-label">{s}</span>
            </div>
          ))}
        </div>

        {/* STEP 0: Configure */}
        {step === 0 && (
          <>
            <div className="modal-body">
              {errBanner}
              <div className="write-fields">
                <div className="field-block">
                  <label>
                    Database name <span style={{ color: "var(--fg-3)", fontWeight: 400 }}>· local nickname only</span>
                  </label>
                  <input className="input lg mono" value={name} onChange={(e) => setName(e.target.value)} maxLength={24} autoFocus />
                  {name && !/^[A-Za-z0-9_-]*$/.test(name) ? (
                    <div className="hint" style={{ color: "var(--red-500)" }}>Use only letters, digits, - and _.</div>
                  ) : name.length > 24 ? (
                    <div className="hint" style={{ color: "var(--red-500)" }}>At most 24 characters.</div>
                  ) : name && (existingNames || []).includes(name) ? (
                    <div className="hint" style={{ color: "var(--red-500)" }}>A database named "{name}" already exists.</div>
                  ) : null}
                </div>
                <div className="field-block">
                  <label>Network</label>
                  <div className="seg">
                    <button className={network === "mainnet" ? "on" : ""} disabled={ironwoodSelected} onClick={() => selectNetwork("mainnet")}>
                      <Icon name="globe" size={12} /> mainnet
                    </button>
                    <button className={network === "testnet" ? "on" : ""} disabled={ironwoodSelected} onClick={() => selectNetwork("testnet")}>
                      <Icon name="flask-conical" size={12} /> testnet
                    </button>
                  </div>
                  {network === "testnet" && (
                    <div className="hint" style={{ fontSize: 12.5, color: "var(--fg-3)", marginTop: 4 }}>
                      Receive TAZ from our faucet to get started within limits.
                    </div>
                  )}
                </div>
                <div className="field-block">
                  <label>Shielded pool</label>
                  <div className="seg">
                    <button className={pool === "ironwood" ? "on" : ""} onClick={() => selectPool("ironwood")}>
                      <Icon name="trees" size={12} /> ironwood
                    </button>
                    <button className={pool === "orchard" ? "on" : ""} onClick={() => selectPool("orchard")}>
                      <Icon name="shield" size={12} /> orchard
                    </button>
                    <button className={pool === "sapling" ? "on" : ""} onClick={() => selectPool("sapling")}>
                      <Icon name="leaf" size={12} /> sapling
                    </button>
                  </div>
                </div>
                {ironwoodSelected && (
                  <div className="callout-flow warn">
                    <Icon name="alert-triangle" size={16} color="var(--amber-400)" />
                    <div>Ironwood is an upcoming Zcash shielded pool.</div>
                  </div>
                )}
              </div>
            </div>
            <div className="modal-foot">
              {counter}
              <div style={{ display: "flex", gap: 8 }}>
                <button className="btn primary" disabled={!nameOk || busy || ironwoodSelected} onClick={doGenerate}>
                  {busy ? <><div className="spinner" /> Generating…</> : "Generate phrase →"}
                </button>
              </div>
            </div>
          </>
        )}

        {/* STEP 1: Seed phrase */}
        {step === 1 && phrase && (
          <>
            <div className="modal-body">
              {errBanner}
              <div className="form-stack">
                <div className="callout-flow warn">
                  <Icon name="alert-triangle" size={16} color="var(--amber-400)" />
                  <div>
                    <strong style={{ color: "inherit" }}>Write these 24 words down now.</strong> They are the only backup
                    of the spending key. Anyone with this phrase can write to this database, and spend its {currencyFor(network)}.
                  </div>
                </div>
                <div className="mnemonic-grid">
                  {words.map((w, i) => (
                    <div key={i} className="mnemonic-cell">
                      <span className="idx">{i + 1}</span>
                      <span className="word">{w}</span>
                    </div>
                  ))}
                </div>
                <div style={{ display: "flex", justifyContent: "space-between", alignItems: "center", gap: 12 }}>
                  <label className="check-row" style={{ margin: 0 }}>
                    <input type="checkbox" checked={acked} onChange={(e) => setAcked(e.target.checked)} />I have written down all 24 words and stored them offline.
                  </label>
                  <button
                    className="btn ghost sm"
                    style={{ flexShrink: 0 }}
                    title="Copy the 24 words (space-separated) to the clipboard"
                    onClick={() => {
                      try { navigator.clipboard.writeText(words.join(" ")); } catch {}
                      setCopiedPhrase(true);
                      setTimeout(() => setCopiedPhrase(false), 1500);
                    }}
                  >
                    <Icon name="copy" className="icon" /> {copiedPhrase ? "Copied" : "Copy phrase"}
                  </button>
                </div>
              </div>
            </div>
            <div className="modal-foot">
              {counter}
              <div style={{ display: "flex", gap: 8 }}>
                <button className="btn secondary" onClick={() => setStep(0)}>← Back</button>
                <button className="btn primary" disabled={!acked} onClick={() => setStep(2)}>I've written it down →</button>
              </div>
            </div>
          </>
        )}

        {/* STEP 2: Confirm */}
        {step === 2 && (
          <>
            <div className="modal-body">
              {errBanner}
              <div style={{ fontSize: 14, color: "var(--fg-2)", lineHeight: 1.55, marginBottom: 14 }}>
                Type the following words from your phrase to confirm you wrote them down:
              </div>
              <div className="write-fields">
                {[3, 11, 19].map((i, idx) => {
                  const val = confirmWords[i];
                  const matched = val === words[i];
                  return (
                    <div key={i} className="field-block">
                      <label>Word #{i + 1}</label>
                      <div style={{ position: "relative" }}>
                        <input
                          className="input lg mono"
                          style={{ paddingRight: 92 }}
                          value={val}
                          placeholder={`enter word ${i + 1}`}
                          autoFocus={idx === 0}
                          onChange={(e) => setConfirmWords({ ...confirmWords, [i]: e.target.value.trim().toLowerCase() })}
                          onKeyDown={(e) => {
                            if (idx === 2 && e.key === "Enter" && confirmOk && !busy) doPersist();
                          }}
                        />
                        {val.length > 0 && (
                          <span style={{ position: "absolute", right: 10, top: "50%", transform: "translateY(-50%)", display: "inline-flex", alignItems: "center", gap: 4, fontSize: 11, pointerEvents: "none", color: matched ? "var(--green-500)" : "var(--red-500)" }}>
                            <Icon name={matched ? "check" : "x"} size={11} /> {matched ? "match" : "no match"}
                          </span>
                        )}
                      </div>
                    </div>
                  );
                })}
              </div>
            </div>
            <div className="modal-foot">
              {counter}
              <div style={{ display: "flex", gap: 8 }}>
                <button className="btn secondary" onClick={() => setStep(1)}>← Back</button>
                <button className="btn primary" disabled={!confirmOk || busy} onClick={doPersist}>
                  {busy ? <><div className="spinner" /> Creating…</> : "Confirm →"}
                </button>
              </div>
            </div>
          </>
        )}

        {/* STEP 3: Fund */}
        {step === 3 && created && (
          <>
            <div className="modal-body">
              {errBanner}
              <div className="funding-layout">
                <div className="qr-card">
                  <div className="qr"><Qr text={created.funding_address} /></div>
                  <div className="qr-cap">scan with any Zcash wallet</div>
                </div>
                <div>
                  <div className="callout-flow note" style={{ marginBottom: 16 }}>
                    <Icon name="zap" size={16} color="var(--amber-400)" />
                    <div>
                      Send <strong style={{ color: "inherit" }}>at least {window.formatZats(minInitZats, network)}</strong> to
                      the funding address below from any Zcash wallet. This window polls for the funds. Every database
                      action, each write or delete, spends at least {window.formatZats(minInitZats, network)} in network fees.
                    </div>
                  </div>
                  <div className="kv-row">
                    <span className="label" style={{ minWidth: 120 }}>Funding address</span>
                    <span className="value mono trunc" title={created.funding_address}>{created.funding_address}</span>
                  </div>
                  {network === "mainnet" && (
                    <div style={{ marginBottom: 10 }}><ValidAddressInfo /></div>
                  )}
                  <div className="kv-row">
                    <span className="label" style={{ minWidth: 120 }}>Min amount</span>
                    <span className="value mono">{window.formatZats(minInitZats, network)}</span>
                  </div>
                  <div className="kv-row">
                    <span className="label" style={{ minWidth: 120 }}>Network</span>
                    <span className="value mono">{network}</span>
                  </div>

                  {fundingState === "confirming" && (
                    <div className="callout-flow" style={{ marginTop: 12 }}>
                      <div className="spinner" />
                      <div style={{ fontSize: 12.5, color: "var(--fg-2)" }}>
                        <strong style={{ color: "inherit" }}>Payment detected, confirming…</strong>{" "}
                        {window.formatZats(confirming, network)} is confirming on-chain. This can take a few minutes on
                        mainnet, <strong>Broadcast INIT</strong> enables once the funds are spendable.
                      </div>
                    </div>
                  )}
                  {fundingState === "short" && (
                    <div className="callout-flow warn" style={{ marginTop: 12 }}>
                      <Icon name="alert-triangle" size={16} color="var(--amber-400)" />
                      <div style={{ fontSize: 12.5, color: "var(--fg-2)" }}>
                        <strong style={{ color: "inherit" }}>Insufficient balance to initialize.</strong>{" "}
                        Initializing broadcasts a signed INIT memo that costs ~{window.formatZats(minInitZats, network)} (the
                        network fee). You have {window.formatZats(spendable, network)} spendable, send at least{" "}
                        {window.formatZats(minInitZats - spendable, network)} more to the address above.
                      </div>
                    </div>
                  )}

                  <div style={{ display: "flex", gap: 8, marginTop: 12 }}>
                    <button className="btn secondary sm" onClick={() => navigator.clipboard.writeText(created.funding_address)}>
                      <Icon name="copy" className="icon" /> Copy address
                    </button>
                  </div>
                </div>
              </div>
            </div>
            <div className="modal-foot">
              {counter}
              <div style={{ display: "flex", gap: 8 }}>
                {/* No "Back" here: the seed is already confirmed and the database
                    persisted, so stepping back to Confirm/Seed would re-show a
                    phrase the user already committed to. Forward only. */}
                {network === "testnet" && (
                  <button className="btn secondary" onClick={doFaucetInit} disabled={faucetInitState !== "idle"}>
                    <Icon name="rocket" className="icon" /> {faucetInitLabel}
                  </button>
                )}
                <button className="btn primary" disabled={!funded} onClick={doInit}>
                  {funded ? "Broadcast INIT →"
                    : fundingState === "confirming" ? "Confirming payment…"
                    : fundingState === "short" ? "Send more funds"
                    : "Waiting for funds…"}
                </button>
              </div>
            </div>
          </>
        )}

        {/* STEP 4: INIT broadcast */}
        {step === 4 && created && (
          <>
            <div className="modal-body">
              {errBanner}
              <div className="form-stack">
                <div style={{ fontSize: 14, color: "var(--fg-2)", marginBottom: 6 }}>
                  Funds received. Broadcasting the INIT memo that opens this database for writes.
                </div>
                <div className="progress-card">
                  <div className={"prog-row " + (broadcast ? "done" : "in-flight")}>
                    {broadcast ? <Icon name="check" size={14} color="var(--green-500)" /> : <div className="spinner" />}
                    <span>{broadcast ? "Signed and broadcast INIT memo" : "Signing & broadcasting INIT memo…"}</span>
                  </div>
                  <div className={"prog-row " + (initState === "initialized" ? "done" : "in-flight")}>
                    {initState === "initialized" ? <Icon name="check" size={14} color="var(--green-500)" /> : <div className="spinner" />}
                    <span>
                      {initState === "initialized"
                        ? `Confirmed (${initRequired || 3}/${initRequired || 3})`
                        : `Awaiting confirmations (${Math.min(initDone, initRequired || 3)}/${initRequired || 3})…`}
                    </span>
                  </div>
                </div>
              </div>
            </div>
            <div className="modal-foot">
              {counter}
              <div style={{ display: "flex", gap: 8 }}>
                <button className="btn primary" disabled>{!broadcast ? "Broadcasting…" : "Awaiting confirmations…"}</button>
              </div>
            </div>
          </>
        )}

        {/* STEP 5: Done */}
        {step === 5 && created && (
          <>
            <div className="modal-body">
              <div className="form-stack">
                <div className="confirmed-banner">
                  <div style={{ width: 36, height: 36, borderRadius: 999, background: "var(--green-300)", display: "grid", placeItems: "center", color: "#fff", flexShrink: 0 }}>
                    <Icon name="check" size={20} />
                  </div>
                  <div>
                    <div style={{ fontWeight: 600, marginBottom: 2, color: "inherit", fontSize: 14 }}>
                      Database <code style={{ fontFamily: "var(--font-mono)" }}>{created.name}</code> created
                    </div>
                    <div style={{ fontSize: 12.5, color: "inherit", opacity: 0.8 }}>
                      INIT confirmed. Writes are now enabled, open it and set your first key.
                    </div>
                  </div>
                </div>
                <div style={{ padding: "16px 18px", background: "var(--bg-sunken)", border: "1px solid var(--border-1)", borderRadius: "var(--radius-md)" }}>
                  <div style={{ fontFamily: "var(--font-mono)", fontSize: 11, letterSpacing: "0.08em", color: "var(--fg-3)", textTransform: "uppercase", marginBottom: 10 }}>
                    Your zkv address, share to give read access
                  </div>
                  <div style={{ fontFamily: "var(--font-mono)", fontSize: 12.5, color: "var(--fg-1)", wordBreak: "break-all", lineHeight: 1.55 }}>
                    {created.address}
                  </div>
                  <div style={{ display: "flex", gap: 8, marginTop: 12 }}>
                    <button className="btn secondary sm" onClick={() => navigator.clipboard.writeText(created.address)}>
                      <Icon name="copy" className="icon" /> Copy
                    </button>
                  </div>
                </div>
              </div>
            </div>
            <div className="modal-foot">
              {counter}
              <div style={{ display: "flex", gap: 8 }}>
                <button className="btn primary" onClick={() => onComplete && onComplete(created.name)}>Open {created.name} →</button>
              </div>
            </div>
          </>
        )}
      </div>
    </div>
  );
};

// ============================================================
// IMPORT FLOW: watch by address OR restore from phrase (real), as a modal
// ============================================================
const ImportFlow = ({ onCancel, onWatch, onRestore, onComplete }: {
  onCancel: () => void;
  onWatch: (addr: string, nickname: string) => Promise<unknown>;
  onRestore: (nickname: string, phrase: string, network: string, pool: string, birthday?: number) => Promise<unknown>;
  onComplete?: () => void;
}) => {
  const [method, setMethod] = React.useState<"watch" | "restore" | null>(null);
  const [addr, setAddr] = React.useState("");
  const [nickname, setNickname] = React.useState("");
  const [phrase, setPhrase] = React.useState("");
  const [busy, setBusy] = React.useState(false);
  const [error, setError] = React.useState<any>(null);
  // Restore: the required second field accepts EITHER a zkv address (which pins
  // the network/pool/birthday) OR a bare birthday height (used with the network
  // toggle and the Orchard default). One of the two is required: without a
  // birthday we'd have to scan for the INIT to recover the right database, which
  // is future work. `network` is the toggle, used only in the height case.
  const [addrOrHeight, setAddrOrHeight] = React.useState("");
  const [network, setNetwork] = React.useState<"mainnet" | "testnet">("mainnet");
  // What a pasted zkv address resolves to (network/pool/birthday), once the
  // backend parses it. Drives the badge and supplies the restore parameters.
  const [addrInfo, setAddrInfo] = React.useState<ZkvAddrInfoResp | null>(null);
  const [inspecting, setInspecting] = React.useState(false);
  // Whether the typed phrase actually controls the pasted address's database,
  // checked by the backend. Only meaningful once an address is resolved;
  // `null` means not-checked / not-applicable.
  const [phraseMatch, setPhraseMatch] =
    React.useState<"checking" | "match" | "mismatch" | "error" | null>(null);

  // Shape-check a zkv address: a single bech32m token (`zkv1…` on mainnet,
  // `zkvtest1…` / `zkvregtest1…` elsewhere). Returns a plain boolean (never a
  // fresh object) so derived values can sit in effect dependency arrays without
  // changing identity every render, which would re-run the effect forever and
  // leave the network/pool probe spinning. `parse_zkv_addr` on the backend is
  // the authority; this only gates the UI.
  const isZkvAddr = (a: string) => /^zkv(test|regtest)?1[a-z0-9]{20,}$/.test(a.trim().toLowerCase());
  const parsed = isZkvAddr(addr);
  const addrState = addr.length === 0 ? "empty" : parsed ? "parsed" : "invalid";

  const phraseWords = phrase.trim().split(/\s+/).filter(Boolean);
  const phraseOk = phraseWords.length === 24;

  // Classify the restore form's optional field: a zkv address, a bare block
  // height, empty, or unusable. An address is authoritative for
  // network/pool/birthday; a height just sets the birthday; empty falls back to
  // the launch-window default the backend fills in.
  const aoh = addrOrHeight.trim();
  const aohIsHeight = /^\d{1,9}$/.test(aoh);
  const aohState = aoh === "" ? "empty" : isZkvAddr(aoh) ? "addr" : aohIsHeight ? "height" : "invalid";
  const fromAddr = aohState === "addr" && !!addrInfo;

  // Effective restore parameters. The pasted address wins when resolved;
  // otherwise the toggle drives the network and the pool defaults to Orchard
  // (live on both networks post-NU6.2; paste a zkv address for a Sapling
  // database). The birthday is the address's or the typed height: one is
  // required, so there is no implicit default here.
  const effNetwork = fromAddr ? addrInfo!.network : network;
  const effPool = fromAddr ? addrInfo!.pool : "orchard";
  const effBirthday = fromAddr ? addrInfo!.birthday : aohState === "height" ? Number(aoh) : undefined;

  // When the optional field holds a shape-valid zkv address, resolve its
  // network/pool/birthday. Deps are primitives, so no object-identity churn.
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
    window.zkvApi
      .inspectAddress(v)
      .then((info) => alive && setAddrInfo(info))
      .catch(() => alive && setAddrInfo(null))
      .finally(() => alive && setInspecting(false));
    return () => {
      alive = false;
    };
  }, [method, addrOrHeight]);

  // When both a resolved address and a full 24-word phrase are present, ask the
  // backend whether the phrase actually controls that database, so a wrong
  // phrase (or wrong address) is caught before submit. Deps are primitives.
  React.useEffect(() => {
    if (method !== "restore" || !fromAddr || !phraseOk) {
      setPhraseMatch(null);
      return;
    }
    let alive = true;
    setPhraseMatch("checking");
    window.zkvApi
      .verifyPhrase(phrase.trim(), addrOrHeight.trim())
      .then((ok) => alive && setPhraseMatch(ok ? "match" : "mismatch"))
      .catch(() => alive && setPhraseMatch("error"));
    return () => {
      alive = false;
    };
  }, [method, fromAddr, phrase, phraseOk, addrOrHeight]);

  // Restore is submittable with a full phrase, a nickname, a usable second
  // field (empty / height / resolved address), and, when an address is pasted,
  // a phrase that verifies against it.
  const secondOk =
    aohState === "height" || (aohState === "addr" && !!addrInfo && !inspecting);
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

  const ErrBanner = () =>
    error ? (
      <div className="callout-flow warn" style={{ marginBottom: 16 }}>
        <Icon name="alert-triangle" size={16} color="var(--amber-400)" />
        <div>
          <strong style={{ color: "inherit" }}>Couldn't add the database.</strong>
          <ErrorMessage message={error.message} />
        </div>
      </div>
    ) : null;

  const dismiss = (e: React.MouseEvent) => {
    if ((e.target as HTMLElement).classList.contains("modal-overlay") && !busy) onCancel();
  };

  // Method picker
  if (!method) {
    return (
      <div className="modal-overlay" onClick={dismiss}>
        <div className="modal" role="dialog" aria-labelledby="import-title">
          <div className="modal-head">
            <div>
              <div className="eyebrow">IMPORT DATABASE</div>
              <h2 id="import-title">Add a database</h2>
            </div>
            <button className="close" onClick={onCancel} title="Close">
              <Icon name="x" size={16} />
            </button>
          </div>
          <div className="modal-body">
            <div className="onboard-paths" style={{ maxWidth: "none" }}>
              <button className="onboard-path" onClick={() => setMethod("watch")}>
                <div className="pic"><Icon name="eye" size={18} /></div>
                <div>
                  <div className="pname">Watch (read-only)</div>
                  <div className="pdesc">Paste a <code>zkv1…</code> address</div>
                </div>
                <div className="parrow"><Icon name="arrow-right" size={16} /></div>
              </button>
              <button className="onboard-path" onClick={() => setMethod("restore")}>
                <div className="pic"><Icon name="key-round" size={18} /></div>
                <div>
                  <div className="pname">Restore admin (write)</div>
                  <div className="pdesc">Enter a 24-word phrase</div>
                </div>
                <div className="parrow"><Icon name="arrow-right" size={16} /></div>
              </button>
            </div>
          </div>
          <div className="modal-foot">
            <div className="cost" style={{ fontFamily: "var(--font-mono)", fontSize: 11, color: "var(--fg-3)" }}>
              <code>zkv watch &lt;addr&gt;</code> · <code>zkv restore</code>
            </div>
          </div>
        </div>
      </div>
    );
  }

  // Watch form
  if (method === "watch") {
    return (
      <div className="modal-overlay" onClick={dismiss}>
        <div className="modal" role="dialog" aria-labelledby="import-title">
          <div className="modal-head">
            <div>
              <div className="eyebrow">IMPORT · WATCH</div>
              <h2 id="import-title">Add a read-only database</h2>
              <div style={{ fontSize: 12.5, color: "var(--fg-3)", marginTop: 4 }}>
                Paste a zkv address; sync starts at its birthday block.
              </div>
            </div>
            {!busy && (
              <button className="close" onClick={onCancel} title="Close">
                <Icon name="x" size={16} />
              </button>
            )}
          </div>
          <div className="modal-body">
            <div className="write-fields">
              <ErrBanner />
              <div className="field-block">
                <label>zkv address</label>
                <textarea
                  className="input addr-input"
                  style={{ minHeight: 78, fontFamily: "var(--font-mono)" }}
                  value={addr}
                  onChange={(e) => setAddr(e.target.value)}
                  autoFocus
                />
                <div className="addr-preview" data-state={addrState} style={{ marginTop: 8 }}>
                  {addrState === "empty" && (
                    <span className="prev-empty"><Icon name="link-2" size={12} /> paste an address starting with <code>zkv1…</code></span>
                  )}
                  {addrState === "invalid" && (
                      <span className="prev-invalid"><Icon name="alert-circle" size={12} /> not a valid zkv address, expected a <code>zkv1…</code> token</span>
                  )}
                  {addrState === "parsed" && (
                    <span className="prev-ok"><Icon name="check" size={12} /> looks like a zkv address · birthday resolved on add</span>
                  )}
                </div>
              </div>
              <div className="field-block">
                <label>Nickname <span style={{ color: "var(--fg-3)", fontWeight: 400 }}>· local only, optional</span></label>
                <input className="input lg" value={nickname} onChange={(e) => setNickname(e.target.value)} maxLength={24} />
              </div>
              <div className="callout-flow note">
                <Icon name="info" size={16} color="var(--amber-400)" />
                <div>Sync starts at the address's birthday block. Older history is skipped automatically.</div>
              </div>
            </div>
          </div>
          <div className="modal-foot">
            <div className="cost" style={{ fontFamily: "var(--font-mono)", fontSize: 11, color: "var(--fg-3)" }}>
              equivalent to <code>zkv watch &lt;addr&gt;</code>
            </div>
            <div style={{ display: "flex", gap: 8 }}>
              <button className="btn secondary" onClick={() => setMethod(null)} disabled={busy}>← Back</button>
              <button className="btn primary" disabled={!parsed || busy} onClick={submitWatch}>
                {busy ? <><div className="spinner" /> Adding…</> : <><Icon name="download" className="icon" /> Add database</>}
              </button>
            </div>
          </div>
        </div>
      </div>
    );
  }

  // Restore form
  return (
    <div className="modal-overlay" onClick={dismiss}>
      <div className="modal" role="dialog" aria-labelledby="import-title">
        <div className="modal-head">
          <div>
            <div className="eyebrow">IMPORT · RESTORE</div>
            <h2 id="import-title">Restore admin access</h2>
            <div style={{ fontSize: 12.5, color: "var(--fg-3)", marginTop: 4 }}>
              Enter your 24-word recovery phrase, plus the database's zkv address or its birthday height.
            </div>
          </div>
          {!busy && (
            <button className="close" onClick={onCancel} title="Close">
              <Icon name="x" size={16} />
            </button>
          )}
        </div>
        <div className="modal-body">
          <div className="write-fields">
            <ErrBanner />
            <div className="callout-flow warn">
              <Icon name="alert-triangle" size={16} color="var(--amber-400)" />
              <div>
                <strong style={{ color: "inherit" }}>Do not use any seed phrase you have ever used in a Zcash wallet.</strong>{" "}
                z:kv databases are not private, anyone with your <code>zkv1</code> address can view the wallet and
                database's entire transaction history by design.
              </div>
            </div>
            {/* Primary: the recovery phrase. */}
            <div className="field-block">
              <label>24-word recovery phrase</label>
              <textarea
                className="input phrase-input"
                style={{ minHeight: 80, fontFamily: "var(--font-mono)" }}
                value={phrase}
                onChange={(e) => setPhrase(e.target.value)}
                disabled={busy}
                autoFocus
              />
              <div className="hint">
                {phraseWords.length > 0 && phraseWords.length < 24 && (
                  <span style={{ color: "var(--amber-400)" }}>{phraseWords.length} / 24 words</span>
                )}
                {phraseWords.length > 24 && (
                  <span style={{ color: "var(--red-500)" }}><Icon name="x" size={11} /> {phraseWords.length} words, phrase is too long</span>
                )}
                {/* No address pasted: a plain 24-word confirmation. */}
                {phraseOk && !fromAddr && (
                  <span style={{ color: "var(--green-500)" }}><Icon name="check" size={11} /> 24 words detected</span>
                )}
                {/* Address pasted: report whether this phrase controls THAT database. */}
                {phraseOk && fromAddr && (phraseMatch === "checking" || phraseMatch === null) && (
                  <span style={{ color: "var(--fg-3)", display: "inline-flex", alignItems: "center", gap: 6 }}><div className="spinner" /> checking against address…</span>
                )}
                {phraseOk && fromAddr && phraseMatch === "match" && (
                  <span style={{ color: "var(--green-500)" }}><Icon name="check" size={11} /> matches this address</span>
                )}
                {phraseOk && fromAddr && phraseMatch === "mismatch" && (
                  <span style={{ color: "var(--red-500)" }}><Icon name="x" size={11} /> does not match this address</span>
                )}
                {phraseOk && fromAddr && phraseMatch === "error" && (
                  <span style={{ color: "var(--amber-400)" }}><Icon name="alert-circle" size={11} /> couldn't verify against the address</span>
                )}
              </div>
            </div>
            {/* Required: a zkv address (pins network/pool/birthday) or a bare
                birthday height (used with the network toggle + Orchard default). */}
            <div className="field-block">
              <label>zkv address or birthday height <span style={{ color: "var(--fg-3)", fontWeight: 400 }}>· required</span></label>
              <textarea
                className="input addr-input"
                style={{ minHeight: 60, fontFamily: "var(--font-mono)" }}
                value={addrOrHeight}
                onChange={(e) => setAddrOrHeight(e.target.value)}
                disabled={busy}
              />
              <div className="addr-preview" data-state={fromAddr || aohState === "height" ? "parsed" : aohState === "invalid" ? "invalid" : "empty"} style={{ marginTop: 8 }}>
                {aohState === "empty" && (
                  <span className="prev-empty"><Icon name="link-2" size={12} /> required: paste the database's <code>zkv1…</code> address, or enter its birthday height</span>
                )}
                {aohState === "invalid" && (
                  <span className="prev-invalid"><Icon name="alert-circle" size={12} /> enter a <code>zkv1…</code> address or a block height</span>
                )}
                {aohState === "height" && (
                  <span className="prev-ok" style={{ display: "inline-flex", alignItems: "center", gap: 6 }}>
                    <Icon name="check" size={12} /> birthday height {Number(aoh)}
                  </span>
                )}
                {aohState === "addr" && inspecting && (
                  <span className="prev-empty"><div className="spinner" /> reading network and pool…</span>
                )}
                {aohState === "addr" && !inspecting && !addrInfo && (
                  <span className="prev-invalid"><Icon name="alert-circle" size={12} /> couldn't read this address; check it and try again</span>
                )}
                {aohState === "addr" && !inspecting && addrInfo && (
                  <span className="prev-ok" style={{ display: "inline-flex", alignItems: "center", gap: 6 }}>
                    <Icon name="check" size={12} />
                    <span className="net-badge" data-net={addrInfo.network}>{addrInfo.network}</span>
                    <span className="db-pool">{addrInfo.pool}</span>
                    <span>· birthday {addrInfo.birthday}</span>
                  </span>
                )}
              </div>
            </div>
            {/* Network: only when no address is pasted. A pasted address pins
                the network and pool (shown in the badge above); in the height
                case the toggle drives the network and the pool defaults to
                Orchard. */}
            {!fromAddr && (
              <div className="field-block">
                <label>Network</label>
                <div className="seg">
                  <button className={network === "mainnet" ? "on" : ""} disabled={busy} onClick={() => setNetwork("mainnet")}>
                    <Icon name="globe" size={12} /> mainnet
                  </button>
                  <button className={network === "testnet" ? "on" : ""} disabled={busy} onClick={() => setNetwork("testnet")}>
                    <Icon name="flask-conical" size={12} /> testnet
                  </button>
                </div>
              </div>
            )}
            <div className="field-block">
              <label>Local database nickname</label>
              <input className="input lg" value={nickname} onChange={(e) => setNickname(e.target.value)} maxLength={24} disabled={busy} />
            </div>
          </div>
        </div>
        <div className="modal-foot">
          <div className="cost" style={{ fontFamily: "var(--font-mono)", fontSize: 11, color: "var(--fg-3)" }}>
            equivalent to <code>zkv restore {nickname || "<name>"}</code>
          </div>
          <div style={{ display: "flex", gap: 8 }}>
            <button className="btn secondary" onClick={() => setMethod(null)} disabled={busy}>← Back</button>
            <button className="btn primary" disabled={!canRestore} onClick={submitRestore}>
              {busy ? <><div className="spinner" /> Restoring…</> : <><Icon name="key-round" className="icon" /> Restore</>}
            </button>
          </div>
        </div>
      </div>
    </div>
  );
};

window.CreateFlow = CreateFlow;
window.ImportFlow = ImportFlow;
window.Qr = Qr;
window.DepositModal = DepositModal;
window.SendModal = SendModal;
