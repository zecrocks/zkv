// flows.jsx: WriteFlow (modal), CreateFlow (wizard), ImportFlow (watch/restore)

// The signature line is always the last line of a wire memo (`ZKV0 …\n[seq]sig`).
const sigLineOf = (memo: string | null | undefined) => {
  if (!memo) return '';
  const lines = memo.split('\n');
  return lines[lines.length - 1] || '';
};

// A framed `--- begin/end zkv memo ---` block. `head` is the opcode/key/value
// line(s), rendered for highlighting; `preview` is the live-sign state
// (`{status, memo, sig}` from the debounced background signer). The signature
// line shows the *real* signature once signed, else a "(signature will go
// here)" placeholder while empty / mid-type. The copy button yanks the exact,
// verbatim memo so a power user can broadcast it from any wallet themselves.
const SignedMemo = ({ head, preview, card, style }: any) => {
  const [copied, setCopied] = React.useState(false);
  const ready = preview.status === 'ready' && !!preview.sig;
  const copy = () => {
    if (!ready || !preview.memo) return;
    try {
      navigator.clipboard.writeText(preview.memo);
      setCopied(true);
      setTimeout(() => setCopied(false), 1200);
    } catch (_) { /* clipboard blocked; nothing to do */ }
  };
  // Collapse the long (130+ hex char) signature to a middle ellipsis so the
  // block stays compact; the copy button still carries the full memo, and the
  // title shows it in full on hover.
  const sig = preview.sig || '';
  const sigShown = sig.length > 46 ? sig.slice(0, 30) + '…' + sig.slice(-12) : sig;
  return (
    <div className={'bcast-memo' + (card ? ' card' : '')} style={style}>
      <div className="bcast-memo-head">
        <span className="lbl">--- begin zkv memo ---</span>
        <button
          type="button"
          className="bcast-copy"
          disabled={!ready}
          title={ready ? 'Copy the exact signed memo' : 'Signing…'}
          onClick={copy}
        >
          <Icon name={copied ? 'check' : 'copy'} size={11} /> {copied ? 'Copied' : 'Copy'}
        </button>
      </div>
      {head}
      <div className="sig" style={ready ? { color: 'var(--fg-2)' } : undefined} title={ready ? sig : undefined}>
        {ready ? sigShown : '(signature will go here)'}
      </div>
      <div><span className="lbl">--- end zkv memo ---</span></div>
    </div>
  );
};

// ============================================================
// WRITE FLOW: modal for set/del operations.
// Steps: form → review (signed memo preview) → broadcasting → confirmed
// ============================================================
const WriteFlow = ({ db, prefillKey, prefillValue, mode = 'set', synced, syncing, paused, onBroadcast, onInit, onSync, onClose, onDone, onDeposit }: {
  db: ActiveDb;
  prefillKey?: string | null;
  prefillValue?: string | null;
  mode?: string;
  synced?: boolean | null;
  syncing?: boolean;
  paused?: boolean;
  onBroadcast: (mode: string, key: string, value: string) => Promise<string>;
  onInit: () => Promise<string>;
  onSync?: () => void;
  onClose: () => void;
  onDone?: (info: { init?: boolean; key?: string; value?: string; isDel?: boolean }) => void;
  onDeposit?: () => void;
}) => {
  const [step, setStep] = React.useState('form');   // form | review | broadcasting | confirmed
  const [keyName, setKeyName] = React.useState(prefillKey || '');
  const [val, setVal] = React.useState(prefillValue || '');
  const [progress, setProgress] = React.useState(0);    // 0..1
  const [confirmIdx, setConfirmIdx] = React.useState(0); // step index for broadcast animation
  const [txHash, setTxHash] = React.useState('');
  const [error, setError] = React.useState<any>(null);
  // INIT sub-flow state, used only when the database isn't initialized yet:
  // the modal offers to broadcast INIT instead of a key write.
  const [initPhase, setInitPhase] = React.useState('idle'); // idle | sending | done
  // Live signed-memo preview, recomputed in the background as the user types
  // (see the debounced effect below). status: idle | pending | ready | error.
  const [memoPreview, setMemoPreview] = React.useState({ status: 'idle', memo: '', sig: '' });

  // A write can't land until the database has a confirmed INIT. When it
  // doesn't, intercept the whole form/review/broadcast flow and offer to
  // broadcast INIT (gated on a full sync to tip) instead of a key write.
  const needsInit = !!(db && db.init && db.init !== 'initialized');
  const isInitializing = db && db.init === 'initializing';

  const doInit = async () => {
    setError(null);
    setInitPhase('sending');
    try {
      const txid = await onInit();
      setTxHash(txid || '');
      setInitPhase('done');
    } catch (e) {
      setInitPhase('idle');
      setError(e);
    }
  };

  // Testnet-only alternative to "Broadcast INIT": the backend signs the INIT
  // memo and hands it to the faucet, which broadcasts it (and pays the fee), so
  // an unfunded testnet database can still initialize. On success we drop into
  // the existing 'done' view (waiting for the INIT to confirm). The faucet call
  // returns { outcome }: "ok" → done; "outdated" (server said "update", or the
  // faucet is unreachable) → "Your app is outdated"; anything else → "Try again
  // later". The button disables itself until the modal is reopened.
  const [faucetInitState, setFaucetInitState] = React.useState('idle'); // idle | requesting | retry | outdated
  const doFaucetInit = async () => {
    setFaucetInitState('requesting');
    setError(null);
    try {
      const r = await window.zkvApi.faucetInit(db.name);
      if (r.outcome === 'outdated') { setFaucetInitState('outdated'); return; }
      if (r.outcome !== 'ok') { setFaucetInitState('retry'); return; }
      // The faucet (not our wallet) created the tx, but it returns the txid so
      // the receipt can show it just like the self-funded path.
      setTxHash(r.txid || '');
      setInitPhase('done');
    } catch (_) {
      setFaucetInitState('retry');
    }
  };
  const faucetInitLabel =
    faucetInitState === 'requesting' ? 'Initializing…'
      : faucetInitState === 'retry' ? 'Try again later'
        : faucetInitState === 'outdated' ? 'Your app is outdated'
          : 'Use our faucet';

  // The exact INIT wire memo this database will broadcast: `ZKV0 INIT
  // <zkv_addr>` plus the 130-char recoverable-ECDSA signature line. The
  // embedded address is an advisory echo (the signature binds only the
  // database's receiver; see protocol::signed_init_payload), but we render
  // the real address so the user can eyeball what's going out. The signature
  // itself is filled in by the background signer (the effect below).
  const initMemoPreview = (
    <SignedMemo
      card
      style={{marginTop:0}}
      preview={memoPreview}
      head={<div style={{wordBreak:'break-all'}}>ZKV0 INIT <span className="key">{db.address || '<zkv address>'}</span></div>}
    />
  );

  const isDel = mode === 'del';
  // The ONLY character rule for keys is "no whitespace": the wire memo
  // `ZKV0 SET <key> <value>` is space-delimited, so a space (or tab/newline)
  // would split the key. Everything else (`/`, uppercase, unicode, …) is
  // valid. Size is bounded by the 511-byte text-memo limit, shared with the
  // value, not an arbitrary per-key cap.
  const keyBytes = new Blob([keyName]).size;
  const valBytes = new Blob([val]).size;
  const keyHasWhitespace = /\s/.test(keyName);
  // Values may be any UTF-8 text. The compact `SET` wire form can't carry a
  // newline (it collides with the signature line) or an empty value (a
  // whitespace-stripping transport drops the trailing space), so those route
  // through the length-framed `SETL` opcode instead. This mirrors
  // `Op::set_for_value` on the backend; the user never has to pick.
  const valHasNewline = /\n/.test(val);
  const usesSetl = !isDel && (valBytes === 0 || valHasNewline);
  const opLabel = isDel ? 'DEL' : (usesSetl ? 'SETL' : 'SET');
  const keyOk = keyName.length > 0 && !keyHasWhitespace;
  // Text-memo byte estimate, matching `build_memo`:
  //   SET:  "ZKV0 SET " (9) + key + " " + value + "\n" + 130-hex sig
  //   SETL: "ZKV0 SETL " (10) + key + " " + len + "\n" + value + "\n" + 130-hex sig
  //   DEL:  "ZKV0 DEL " (9) + key + "\n" + 130-hex sig (no value)
  const lenDigits = String(valBytes).length;
  const memoBytes = isDel
    ? 140 + keyBytes
    : usesSetl
      ? 143 + keyBytes + lenDigits + valBytes
      : 141 + keyBytes + valBytes;
  const memoOk = memoBytes <= 511;
  const canSubmit = keyOk && memoOk;
  // Pre-flight funds hint: a write costs the ~0.0001 ZEC ZIP-317 fee. If the
  // (confirmed) balance is below that, the broadcast will fail; flag it.
  const FEE_FLOOR = 10000;
  const lowFunds = db.balance != null && db.balance < FEE_FLOOR;
  // Cost dot: grey until synced, green once we hold enough to cover the fee,
  // amber while funds are still unknown.
  const haveFunds = db.balance != null && !lowFunds;
  const costDotColor = !synced
    ? 'var(--fg-3)'
    : haveFunds
      ? 'var(--green-500)'
      : 'var(--amber-300)';

  // Drive the real broadcast. The step animation runs as a progress
  // indicator while the request is in flight; the actual txid and the
  // confirmed/error transition come from onBroadcast (a live API call).
  React.useEffect(() => {
    if (step !== 'broadcasting') return;
    setConfirmIdx(0);
    setProgress(0);
    setError(null);
    setTxHash('');
    let cancelled = false;

    const ticks = [
      { at: 300,  step: 1, prog: 0.18 },
      { at: 1100, step: 2, prog: 0.42 },
      { at: 2600, step: 3, prog: 0.72 },
    ];
    const timers = ticks.map(t => setTimeout(() => {
      if (cancelled) return;
      setConfirmIdx(t.step);
      setProgress(t.prog);
    }, t.at));

    (async () => {
      try {
        const txid = await onBroadcast(mode, keyName, val);
        if (cancelled) return;
        setTxHash(txid || '');
        setConfirmIdx(5);
        setProgress(1);
        setStep('confirmed');
      } catch (e) {
        if (cancelled) return;
        setError(e);
      }
    })();

    return () => { cancelled = true; timers.forEach(clearTimeout); };
  }, [step]);

  // The opcode/key/value head of the wire memo, shown in the live preview
  // (form) and the review block. The signature line is appended by SignedMemo
  // from the background-signed `memoPreview`.
  const writeHead = usesSetl ? (
    <>
      <div>ZKV0 SETL <span className="key">{keyName}</span> {valBytes}</div>
      <div>{val.length > 60 ? val.slice(0,60) + '…' : val}</div>
    </>
  ) : (
    <div>ZKV0 {opLabel} <span className="key">{keyName}</span>{!isDel && ` ${val.length > 60 ? val.slice(0,60) + '…' : val}`}</div>
  );

  // Background signer: keep `memoPreview` holding the *real* signature for the
  // current write, recomputed a beat after typing settles so keystrokes never
  // lag. On every input change we drop back to the placeholder first, so a
  // stale signature never lingers next to fresh text, then sign after the lull.
  // Signing is an async call onto the engine's blocking pool (the ECDSA work is
  // never on the UI thread); copying the result lets a power user broadcast the
  // memo by hand.
  React.useEffect(() => {
    // INIT has no typed inputs: sign its fixed memo once when the sub-flow is
    // active and idle (skip while it's broadcasting or already confirming; the
    // last-signed memo stays on screen).
    if (needsInit) {
      if (initPhase !== 'idle' || isInitializing) return;
      let cancelled = false;
      setMemoPreview({ status: 'pending', memo: '', sig: '' });
      (async () => {
        try {
          const r = await window.zkvApi.signPreview(db.name, 'init', '', '');
          if (!cancelled) setMemoPreview({ status: 'ready', memo: r.memo, sig: sigLineOf(r.memo) });
        } catch (_) {
          if (!cancelled) setMemoPreview({ status: 'error', memo: '', sig: '' });
        }
      })();
      return () => { cancelled = true; };
    }

    // SET/DEL: nothing to sign until the form is valid.
    if (!canSubmit) { setMemoPreview({ status: 'idle', memo: '', sig: '' }); return; }
    // Placeholder while the debounce is in flight ("empty state while typing"),
    // then sign once the user pauses.
    setMemoPreview({ status: 'pending', memo: '', sig: '' });
    let cancelled = false;
    const op = isDel ? 'del' : 'set';
    const timer = setTimeout(async () => {
      try {
        const r = await window.zkvApi.signPreview(db.name, op, keyName, isDel ? '' : val);
        if (!cancelled) setMemoPreview({ status: 'ready', memo: r.memo, sig: sigLineOf(r.memo) });
      } catch (_) {
        if (!cancelled) setMemoPreview({ status: 'error', memo: '', sig: '' });
      }
    }, 500);
    return () => { cancelled = true; clearTimeout(timer); };
  }, [db.name, needsInit, initPhase, isInitializing, canSubmit, isDel, keyName, val]);

  const heading = isDel ? `Delete key` : (prefillKey ? `Update value` : `Set new key`);
  const sub     = isDel ? `Broadcast a signed DEL memo. The key is removed once readers confirm it.`
                        : `Sign and broadcast a SET memo. Readers see it after the confirmation depth.`;

  // ---- FORM step ----
  const FormStep = () => (
    <>
      <div className="modal-body">
        <div className="write-fields">
          <div className="field-block">
            <label>Key</label>
            <input
              className="input mono lg"
              value={keyName}
              onChange={e => setKeyName(e.target.value)}
              // Locked when deleting, and when updating an existing key: the
              // key identifies the write, so only its value can change.
              disabled={isDel || !!prefillKey}
              title={prefillKey ? 'Key is fixed when updating its value' : undefined}
              autoFocus={!isDel && !prefillKey}
            />
            <div className="byte-counter">
              <span className={keyOk ? 'ok' : 'err'}>
                {keyName.length === 0 ? 'required' :
                 keyHasWhitespace ? 'keys can’t contain whitespace' :
                 ''}
              </span>
              <span style={{color:'var(--fg-3)'}}>{keyBytes} bytes</span>
            </div>
          </div>

          {!isDel && (
            <div className="field-block">
              <label>Value</label>
              <textarea
                className="input mono"
                value={val}
                onChange={e => setVal(e.target.value)}
                autoFocus={!!prefillKey}
              />
              <div className="byte-counter">
                <span className={!memoOk ? 'err' : memoBytes > 470 ? 'warn' : 'ok'}>
                  {!memoOk ? 'memo too large, shorten the key or value' :
                   memoBytes > 470 ? 'close to the 511-byte memo limit' :
                   valBytes === 0 ? 'empty value, sent as SETL' :
                   valHasNewline ? 'multi-line value, sent as SETL' :
                   ''}
                </span>
                <span style={{color:'var(--fg-3)'}}>{valBytes} bytes · memo {memoBytes} / 511</span>
              </div>
            </div>
          )}

          {isDel && (
            <div className="callout-flow warn">
              <Icon name="alert-triangle" size={16} color="var(--amber-400)" />
              <div>
                <strong style={{color:'inherit'}}>This broadcasts a permanent DEL memo.</strong>{' '}
                Future reads will not return <code>{keyName}</code>. Old values remain in chain history.
              </div>
            </div>
          )}

          <div style={{padding:'10px 14px', background:'var(--bg-sunken)', borderRadius:'var(--radius-md)', border:'1px solid var(--border-1)'}}>
            <div style={{display:'flex', justifyContent:'space-between', fontFamily:'var(--font-mono)', fontSize:11, color:'var(--fg-3)', textTransform:'uppercase', letterSpacing:'0.08em', marginBottom:6}}>
              <span>Tx preview</span>
              <span>{db.network}</span>
            </div>
            <div style={{fontFamily:'var(--font-mono)', fontSize:12, color:'var(--fg-2)', lineHeight:1.6}}>
              <div>fee · <span style={{color:'var(--fg-1)'}}>~{window.formatZats(FEE_FLOOR, db.network)}</span></div>
              <div>balance · <span style={{color:'var(--fg-1)'}}>{db.balance != null ? window.formatZats(db.balance, db.network) : '—'}</span></div>
              <div style={{display:'flex', gap:6, alignItems:'flex-start', minWidth:0}}>
                <span style={{flexShrink:0}}>database address ·</span>
                {db.address
                  ? <CollapsibleString value={db.address} onCopy={(t: string) => { try { navigator.clipboard.writeText(t); } catch {} }} />
                  : <span style={{color:'var(--fg-1)'}}>—</span>}
              </div>
            </div>
          </div>

          {/* Live signed-memo preview: the exact memo this write will broadcast,
              its signature filled in by the background signer a beat after you
              stop typing. Copy it to broadcast from any wallet yourself. */}
          <SignedMemo card preview={memoPreview} head={writeHead} />
        </div>
      </div>

      <div className="modal-foot">
        <div className="cost">
          {lowFunds ? (
            <>
              <Icon name="alert-triangle" size={11} color="var(--red-500)" />
              <span style={{ color: 'var(--red-500)' }}>~{window.formatZats(FEE_FLOOR, db.network)} · insufficient funds</span>
              {onDeposit && (
                <button className="btn ghost sm" style={{ marginLeft: 4 }} onClick={onDeposit}>
                  <Icon name="download" size={12} /> Deposit
                </button>
              )}
            </>
          ) : (
            <>
              <span className="amber-dot" style={{background: costDotColor}}></span>
              <span>Cost: ~{window.formatZats(FEE_FLOOR, db.network)} (network fee)</span>
            </>
          )}
        </div>
        <div style={{display:'flex', gap:8}}>
          <button className="btn secondary" onClick={onClose}>Cancel</button>
          <button className="btn primary" disabled={!canSubmit || lowFunds} onClick={() => setStep('review')}>
            {lowFunds ? 'Insufficient funds' : 'Review →'}
          </button>
        </div>
      </div>
    </>
  );

  // ---- REVIEW step ----
  const ReviewStep = () => (
    <>
      <div className="modal-body">
        <div style={{fontSize:13, color:'var(--fg-2)', marginBottom:14, lineHeight:1.5}}>
          On <strong style={{color:'var(--fg-1)'}}>Broadcast</strong>, the write is signed locally with the seed for <strong style={{color:'var(--fg-1)'}}>{db.name}</strong> and sent as a memo. You only pay the network fee.
        </div>

        <div className="broadcast-steps">
          <div className="bcast-step done">
            <div className="bcast-ic"><Icon name="check" size={12} /></div>
            <div><strong>Key & value validated</strong></div>
            <div className="bcast-meta">{memoBytes}b</div>
          </div>
          <div className={'bcast-step ' + (memoPreview.status === 'ready' ? 'done' : 'pending')}>
            <div className="bcast-ic">
              {memoPreview.status === 'ready'
                ? <Icon name="check" size={12} />
                : <span style={{fontFamily:'var(--font-mono)', fontSize:10}}>2</span>}
            </div>
            <div><strong>{memoPreview.status === 'ready' ? 'Signed' : 'Signed at broadcast'}</strong></div>
          </div>
          <SignedMemo preview={memoPreview} head={writeHead} />
        </div>
      </div>

      <div className="modal-foot">
        <div className="cost">
          <Icon name="zap" size={11} />
          <span>Recipient: this database's Zcash {db.pool === 'sapling' ? 'Sapling' : 'Orchard'} address</span>
        </div>
        <div style={{display:'flex', gap:8}}>
          <button className="btn secondary" onClick={() => setStep('form')}>← Back</button>
          <button className="btn primary" onClick={() => setStep('broadcasting')}>
            <Icon name="send" className="icon" /> Broadcast
          </button>
        </div>
      </div>
    </>
  );

  // ---- BROADCASTING step ----
  const BroadcastStep = () => {
    const steps = [
      { label: 'Memo signed',               sub: '' },
      { label: 'Transaction assembled',     sub: 'zero-value Orchard self-send carrying the memo' },
      { label: `Submitted to Zcash ${db.network}`, sub: 'broadcast via lightwalletd' },
      { label: 'Accepted to the mempool',   sub: 'awaiting a block' },
      { label: 'Confirming',                sub: 'readers see it after the confirmation threshold' },
    ];

    if (error) {
      const code = error.code;
      return (
        <>
          <div className="modal-body">
            <div className="callout-flow warn">
              <Icon name="alert-triangle" size={16} color="var(--amber-400)" />
              <div>
                <strong style={{color:'inherit'}}>
                  {code === 'insufficient_funds' ? 'Not enough funds to broadcast.'
                    : code === 'not_initialized' ? 'This database isn’t initialized yet.'
                    : code === 'watch_only' ? 'This is a watch-only database.'
                    : 'Broadcast failed.'}
                </strong>
                <ErrorMessage message={error.message} />
                {code === 'insufficient_funds' && error.data && (
                  <div style={{marginTop:6, fontFamily:'var(--font-mono)', fontSize:11.5, color:'var(--fg-3)'}}>
                    {window.formatZats(error.data.available, db.network)} available ·{' '}
                    {window.formatZats(error.data.required, db.network)} needed
                  </div>
                )}
                {code === 'insufficient_funds' && (db.confirming || 0) > 0 && (
                  <div style={{marginTop:4, fontSize:12.5, color:'var(--fg-2)'}}>
                    <Icon name="clock" size={12} /> {window.formatZats(db.confirming!, db.network)} confirming, this
                    becomes spendable in a few minutes. No need to deposit more.
                  </div>
                )}
              </div>
            </div>
          </div>
          <div className="modal-foot">
            <div className="cost"><Icon name="x" size={11} /> <span>not broadcast</span></div>
            <div style={{display:'flex', gap:8}}>
              {code === 'insufficient_funds' && onDeposit && (
                <button className="btn secondary" onClick={onDeposit}>
                  <Icon name="download" className="icon" /> Deposit QR
                </button>
              )}
              <button className="btn secondary" onClick={onClose}>Close</button>
              <button className="btn primary" onClick={() => setStep('form')}>← Back to form</button>
            </div>
          </div>
        </>
      );
    }

    return (
      <>
        <div className="modal-body">
          <div style={{fontSize:13, color:'var(--fg-2)', marginBottom:14, lineHeight:1.5}}>
            Signing and broadcasting to <strong style={{color:'var(--fg-1)'}}>{db.network}</strong> via the local
            wallet. This can take a few seconds.
          </div>

          <div className="broadcast-steps">
            {steps.map((s, i) => (
              <div key={i} className={'bcast-step ' + (i < confirmIdx ? 'done' : i === confirmIdx ? 'active' : 'pending')}>
                <div className="bcast-ic">
                  {i < confirmIdx ? <Icon name="check" size={12} /> :
                   i === confirmIdx ? <div className="spinner" style={{width:10, height:10, borderWidth:1.5}}></div> :
                   <span style={{fontFamily:'var(--font-mono)', fontSize:10, color:'inherit'}}>{i+1}</span>}
                </div>
                <div>
                  <strong>{s.label}</strong>
                  {s.sub && <div style={{fontSize:12, color:'var(--fg-3)'}}>{s.sub}</div>}
                </div>
              </div>
            ))}
          </div>

          <div className="prog-bar" style={{marginTop:18}}>
            <div className="prog-fill" style={{width: (progress * 100) + '%'}}></div>
          </div>
        </div>

        <div className="modal-foot">
          <div className="cost"><div className="spinner"></div><span>Broadcasting…</span></div>
          <div style={{display:'flex', gap:8}}>
            <button className="btn secondary" onClick={onClose}>Close window</button>
          </div>
        </div>
      </>
    );
  };

  // ---- INIT step (shown instead of the form when the db isn't initialized) ----
  const InitStep = () => {
    // Success: INIT broadcast.
    if (initPhase === 'done') {
      return (
        <>
          <div className="modal-body">
            <div className="confirmed-banner">
              <div style={{width:32, height:32, borderRadius:999, background:'var(--green-300)', display:'grid', placeItems:'center', color:'#fff', flexShrink:0}}>
                <Icon name="check" size={18} />
              </div>
              <div>
                <div style={{fontWeight:600, marginBottom:2, color:'inherit'}}>INIT broadcast</div>
                <div style={{fontSize:12, color:'var(--fg-2)'}}>
                  Once it confirms, this database is initialized and you can set keys.
                </div>
              </div>
            </div>
            <div style={{marginTop:16, padding:'14px 16px', background:'var(--bg-sunken)', border:'1px solid var(--border-1)', borderRadius:'var(--radius-md)'}}>
              <div style={{fontFamily:'var(--font-mono)', fontSize:11, letterSpacing:'0.08em', color:'var(--fg-3)', textTransform:'uppercase', marginBottom:8}}>Receipt</div>
              <div className="kv-row"><span className="label">Op</span><span className="value">INIT</span></div>
              <div className="kv-row"><span className="label">TXID</span><span className="value">{txHash ? <CollapsibleString value={txHash} onCopy={(t: string) => { try { navigator.clipboard.writeText(t); } catch {} }} /> : '—'}</span></div>
            </div>
          </div>
          <div className="modal-foot">
            <div className="cost"><Icon name="shield-check" size={11} color="var(--green-300)" /> <span>broadcast</span></div>
            <div style={{display:'flex', gap:8}}>
              <button className="btn primary" onClick={() => { onDone && onDone({init:true}); onClose(); }}>Done</button>
            </div>
          </div>
        </>
      );
    }

    // Error: surface the same way the write path does.
    if (error) {
      const code = error.code;
      return (
        <>
          <div className="modal-body">
            <div className="callout-flow warn">
              <Icon name="alert-triangle" size={16} color="var(--amber-400)" />
              <div>
                <strong style={{color:'inherit'}}>
                  {code === 'not_synced' ? 'Wallet isn’t fully synced yet.'
                    : code === 'stale_tip' ? 'Can’t confirm a current chain tip.'
                    : code === 'insufficient_funds' ? 'Not enough funds to broadcast INIT.'
                    : code === 'watch_only' ? 'This is a watch-only database.'
                    : 'INIT broadcast failed.'}
                </strong>
                <ErrorMessage message={error.message} />
              </div>
            </div>
          </div>
          <div className="modal-foot">
            <div className="cost"><Icon name="x" size={11} /> <span>not broadcast</span></div>
            <div style={{display:'flex', gap:8}}>
              <button className="btn secondary" onClick={onClose}>Close</button>
              <button className="btn primary" onClick={() => setError(null)}>← Back</button>
            </div>
          </div>
        </>
      );
    }

    // Sending. One spinner, in the footer (mirrors the write BroadcastStep),
    // with the signed memo still visible above so the user sees exactly what's
    // going out.
    if (initPhase === 'sending') {
      return (
        <>
          <div className="modal-body">
            <div style={{fontSize:13, color:'var(--fg-2)', marginBottom:14, lineHeight:1.5}}>
              Signing and broadcasting to <strong style={{color:'var(--fg-1)'}}>{db.network}</strong> via the local
              wallet. This can take a few minutes.
            </div>
            {initMemoPreview}
          </div>
          <div className="modal-foot">
            <div className="cost"><div className="spinner"></div><span>Broadcasting INIT…</span></div>
            <div></div>
          </div>
        </>
      );
    }

    // Still confirming a previously-broadcast INIT: nothing to do but wait.
    if (isInitializing) {
      const done = db.init_done || 0;
      const required = db.init_required || 0;
      return (
        <>
          <div className="modal-body">
            <div className="callout-flow warn">
              <Icon name="clock" size={16} color="var(--amber-400)" />
              <div>
                <strong style={{color:'inherit'}}>INIT is awaiting confirmation{required ? ` (${done}/${required})` : ''}.</strong>
                <div style={{marginTop:4, fontSize:12.5, color:'var(--fg-2)'}}>
                  It’s already been broadcast, no need to send it again. Should be ready within ~5 minutes, then you can set keys.
                </div>
              </div>
            </div>
          </div>
          <div className="modal-foot">
            <div className="cost"><Icon name="clock" size={11} /> <span>initializing</span></div>
            <div style={{display:'flex', gap:8}}>
              <button className="btn secondary" onClick={onClose}>Close</button>
              {paused && onSync && (
                <button className="btn primary" disabled={syncing} onClick={onSync}>
                  <Icon name="refresh-cw" className="icon" /> {syncing ? 'Syncing…' : 'Sync now'}
                </button>
              )}
            </div>
          </div>
        </>
      );
    }

    // Uninitialized but underfunded: INIT costs the network fee, so with a
    // balance below it the only useful action is to deposit. Show just that,
    // plus the signed memo for power users who want to broadcast it elsewhere.
    if (lowFunds) {
      return (
        <>
          <div className="modal-body">
            <div className="callout-flow warn">
              <Icon name="alert-triangle" size={16} color="var(--amber-400)" />
              <div>
                <strong style={{color:'inherit'}}>Insufficient funds.</strong>
                <div style={{marginTop:4, fontSize:12.5, color:'var(--fg-2)'}}>
                  Deposit to this database's funding address in order to initialize the database.
                </div>
              </div>
            </div>
            <div style={{marginTop:12}}>{initMemoPreview}</div>
          </div>
          <div className="modal-foot">
            <div className="cost">
              <span className="amber-dot"></span>
              <span>Needs ~{window.formatZats(FEE_FLOOR, db.network)} (network fee)</span>
            </div>
            <div style={{display:'flex', gap:8}}>
              <button className="btn secondary" onClick={onClose}>Cancel</button>
              {db.network === 'testnet' && (
                <button className="btn secondary" onClick={doFaucetInit} disabled={faucetInitState !== 'idle'}>
                  <Icon name="rocket" className="icon" /> {faucetInitLabel}
                </button>
              )}
              {onDeposit && (
                <button className="btn primary" onClick={() => { onClose(); onDeposit(); }}>
                  <Icon name="download" className="icon" /> Deposit
                </button>
              )}
            </div>
          </div>
        </>
      );
    }

    // Uninitialized: offer to broadcast INIT, gated on a full sync to tip.
    return (
      <>
        <div className="modal-body">
          <div className="callout-flow warn">
            <Icon name="alert-triangle" size={16} color="var(--amber-400)" />
            <div>
              <strong style={{color:'inherit'}}>This database isn’t initialized yet.</strong>
              <div style={{marginTop:4, fontSize:12.5, color:'var(--fg-2)'}}>
                Broadcast an INIT to open it for writes.
              </div>
            </div>
          </div>

          {!synced && (
            <div className="callout-flow" style={{marginTop:12}}>
              <Icon name="info" size={16} color="var(--fg-3)" />
              <div style={{fontSize:12.5, color:'var(--fg-2)'}}>
                Finish syncing before broadcasting INIT.{' '}
                {paused ? 'Sync first.' : 'Syncing now, ready once it catches up.'}
              </div>
            </div>
          )}

          <div style={{marginTop:12}}>{initMemoPreview}</div>

          <div style={{marginTop:12, padding:'10px 14px', background:'var(--bg-sunken)', borderRadius:'var(--radius-md)', border:'1px solid var(--border-1)'}}>
            <div style={{display:'flex', justifyContent:'space-between', fontFamily:'var(--font-mono)', fontSize:11, color:'var(--fg-3)', textTransform:'uppercase', letterSpacing:'0.08em', marginBottom:6}}>
              <span>INIT tx preview</span>
              <span>{db.network}</span>
            </div>
            <div style={{fontFamily:'var(--font-mono)', fontSize:12, color:'var(--fg-2)', lineHeight:1.6}}>
              <div>fee · <span style={{color:'var(--fg-1)'}}>~{window.formatZats(FEE_FLOOR, db.network)}</span> <span style={{color:'var(--fg-3)'}}>(network fee, the only cost)</span></div>
              <div>balance · <span style={{color:'var(--fg-1)'}}>{db.balance != null ? window.formatZats(db.balance, db.network) : '—'}</span></div>
            </div>
          </div>
        </div>

        <div className="modal-foot">
          <div className="cost">
            <span className="amber-dot"></span>
            <span>{synced ? `Cost: ~${window.formatZats(FEE_FLOOR, db.network)} (network fee)` : 'Sync to the chain tip first'}</span>
          </div>
          <div style={{display:'flex', gap:8}}>
            <button className="btn secondary" onClick={onClose}>Cancel</button>
            {db.network === 'testnet' && (
              <button className="btn secondary" onClick={doFaucetInit} disabled={faucetInitState !== 'idle'}>
                <Icon name="rocket" className="icon" /> {faucetInitLabel}
              </button>
            )}
            {synced ? (
              <button className="btn primary" onClick={doInit}>
                <Icon name="send" className="icon" /> Broadcast INIT
              </button>
            ) : (
              paused && onSync && (
                <button className="btn primary" disabled={syncing} onClick={onSync}>
                  <Icon name="refresh-cw" className="icon" /> {syncing ? 'Syncing…' : 'Sync now'}
                </button>
              )
            )}
          </div>
        </div>
      </>
    );
  };

  // ---- CONFIRMED step ----
  const ConfirmedStep = () => (
    <>
      <div className="modal-body">
        <div className="confirmed-banner">
          <div style={{width:32, height:32, borderRadius:999, background:'var(--green-300)', display:'grid', placeItems:'center', color:'#fff', flexShrink:0}}>
            <Icon name="check" size={18} />
          </div>
          <div>
            <div style={{fontWeight:600, color:'inherit'}}>
              {isDel ? `Deleted` : prefillKey ? `Updated` : `Set`} <code style={{fontFamily:'var(--font-mono)'}}>{keyName}</code>
            </div>
          </div>
        </div>

        <div style={{marginTop:16, padding:'14px 16px', background:'var(--bg-sunken)', border:'1px solid var(--border-1)', borderRadius:'var(--radius-md)'}}>
          <div style={{fontFamily:'var(--font-mono)', fontSize:11, letterSpacing:'0.08em', color:'var(--fg-3)', textTransform:'uppercase', marginBottom:8}}>Receipt</div>
          <div className="kv-row"><span className="label">Op</span><span className="value">{opLabel}</span></div>
          <div className="kv-row"><span className="label">Key</span><span className="value" style={{color:'var(--amber-400)'}}>{keyName}</span></div>
          {!isDel && <div className="kv-row"><span className="label">Value</span><span className="value" style={{wordBreak:'break-all'}}>{val.length > 80 ? val.slice(0,80) + '…' : val}</span></div>}
          <div className="kv-row"><span className="label">TXID</span><span className="value">{txHash ? <CollapsibleString value={txHash} onCopy={(t: string) => { try { navigator.clipboard.writeText(t); } catch {} }} /> : '—'}</span></div>
        </div>
      </div>

      <div className="modal-foot">
        <div className="cost"><Icon name="shield-check" size={11} color="var(--green-300)" /> <span>broadcast</span></div>
        <div style={{display:'flex', gap:8}}>
          <button className="btn secondary" onClick={() => { onDone && onDone({key:keyName, value:val, isDel}); setStep('form'); setKeyName(''); setVal(''); }}>
            <Icon name="plus" className="icon" /> Set another
          </button>
          <button className="btn primary" onClick={() => { onDone && onDone({key:keyName, value:val, isDel}); onClose(); }}>
            Done
          </button>
        </div>
      </div>
    </>
  );

  // While the INIT sub-flow is mid-broadcast, lock the modal like the write
  // path does during 'broadcasting'.
  const locked = step === 'broadcasting' || (needsInit && initPhase === 'sending');
  const headTitle = needsInit ? 'Initialize database' : heading;
  const headSub = needsInit
    ? 'Broadcast an INIT to open this database for writes.'
    : sub;

  return (
    <div className="modal-overlay" onClick={(e) => { if ((e.target as HTMLElement).classList.contains('modal-overlay') && !locked) onClose(); }}>
      <div className="modal" role="dialog" aria-labelledby="write-title">
        <div className="modal-head">
          <div>
            <div className="eyebrow">DB: {db.name}</div>
            <h2 id="write-title">{headTitle}</h2>
            <div style={{fontSize:12.5, color:'var(--fg-3)', marginTop:4}}>{headSub}</div>
          </div>
          {!locked && (
            <button className="close" onClick={onClose} title="Close">
              <Icon name="x" size={16} />
            </button>
          )}
        </div>
        {/* Call as functions, not <FormStep/>, so each keystroke reconciles
            the same inputs in place instead of remounting them (which would
            steal focus back to the autoFocus'd Key field). */}
        {needsInit ? InitStep() : (
          <>
            {step === 'form' && FormStep()}
            {step === 'review' && ReviewStep()}
            {step === 'broadcasting' && BroadcastStep()}
            {step === 'confirmed' && ConfirmedStep()}
          </>
        )}
      </div>
    </div>
  );
};

window.WriteFlow = WriteFlow;
