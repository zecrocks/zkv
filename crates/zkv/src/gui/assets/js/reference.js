const DROP = {
  NoWriteAuthority: {
    t: "NoWriteAuthority",
    d: "The recovered signer holds neither owner nor writer authority for this database, so the write is silently dropped on replay."
  },
  OutOfScope: {
    t: "OutOfScope",
    d: "The signer is a scoped writer that lacks the capability this write needs, namely CREATE for a new key, UPDATE for an existing key, or DESTROY for a delete."
  },
  StaleVersion: {
    t: "StaleVersion",
    d: "The sequence on the signature line is outside the entity's bounded-forward window (current \u2026 current+256): below it is a stale replay or lost compare-and-swap, far above it is a desync. Versions are tombstone-surviving, so a deleted key / revoked target can't be revived by replaying an old memo."
  },
  ForgedInit: {
    t: "ForgedInit",
    d: "INIT was not signed by the root (UFVK-derived) key. Only the root key can initialize a database; any other signer is rejected."
  },
  InitAddressInvalid: {
    t: "InitAddressInvalid",
    d: "The zkv address echoed in the INIT memo does not parse as a valid zkv address. (The echo is advisory, only the receiver is signed, but a garbage echo is still rejected.)"
  },
  InitNetworkMismatch: {
    t: "InitNetworkMismatch",
    d: "The echoed INIT address is for a different network than the database being read."
  },
  InitAddressMismatch: {
    t: "InitAddressMismatch",
    d: "The echoed INIT address is valid and same-network but resolves to a different database (different UFVK or birthday)."
  },
  DuplicateInit: {
    t: "DuplicateInit",
    d: "The database was already initialized. First valid INIT wins; later INITs are ignored."
  },
  NotInitialized: {
    t: "NotInitialized",
    d: "No confirmed INIT has been honored yet. Every data and management op is dropped until the database is initialized."
  },
  Finalized: {
    t: "Finalized",
    d: "A FINALIZE has confirmed and sealed the database. Every subsequent write is dropped. That includes SET/SETL/DEL, OWNER*/WRITER*, and any further FINALIZE. The latch is one-way; nothing more is ever applied."
  },
  NotOwner: {
    t: "NotOwner",
    d: "The recovered signer is not a current owner. Only owners may issue management (OWNER*/WRITER*) and VERSION memos."
  },
  LastOwnerProtected: {
    t: "LastOwnerProtected",
    d: "This OWNERDEL would remove the last remaining owner. The database must always stay manageable, so it is dropped. (The root key is removable once a second owner exists, key rotation.)"
  },
  WriterTargetIsOwner: {
    t: "WriterTargetIsOwner",
    d: "WRITERSET targets a pubkey that is already an owner; owner already subsumes any writer scope."
  },
  InvalidTargetPubkey: {
    t: "InvalidTargetPubkey",
    d: "The management op's target is not a valid secp256k1 public key."
  },
  InvalidScope: {
    t: "InvalidScope",
    d: "The WRITERSET scope did not parse as a non-empty subset of CREATE,UPDATE,DESTROY."
  },
  VersionNotNumeric: {
    t: "VersionNotNumeric",
    d: "The VERSION epoch was not a decimal u32."
  },
  VersionBadFlag: {
    t: "VersionBadFlag",
    d: "The VERSION flags did not parse as a block set (warn, blockall, or a subset of blocksync,blockread,blockwrite)."
  },
  VersionBelowGenesis: {
    t: "VersionBelowGenesis",
    d: "The requested epoch is below the genesis version (0)."
  },
  VersionNoOp: {
    t: "VersionNoOp",
    d: "The requested epoch equals the current one, a no-op."
  },
  VersionJumpTooLarge: {
    t: "VersionJumpTooLarge",
    d: "The VERSION jumps more than one epoch up. Upgrades may only step +1 (each costs a fee, defeating a jump-to-huge DoS); downgrades may jump freely."
  },
  MalformedMemo: {
    t: "MalformedMemo",
    d: "The memo structure is wrong: unknown opcode, empty key, wrong arity, a missing value/scope/flag/signature, bad signature framing, or a malformed SETL length/value."
  },
  BadSignature: {
    t: "BadSignature",
    d: "The 130-hex signature is well-framed but does not recover to a public key (corrupt bytes or recovery id). The reader can't distinguish forged from corrupt, so both land here."
  },
  UnsupportedVersion: {
    t: "UnsupportedVersion",
    d: "The wire magic is from a newer zkv protocol (ZKV<n>, n > 1) than this build understands. Update zkv."
  }
};
const globalErrorsFor = (op) => {
  const base = ["MalformedMemo", "BadSignature", "UnsupportedVersion"];
  return op.id === "init" ? base : ["NotInitialized", "Finalized", ...base];
};
const SIG_LINE = "[seq]<130 hex chars, 65-byte recoverable ECDSA signature>";
const byteLen = (s) => new TextEncoder().encode(s || "").length;
const SAMPLE_MEMO = "ZKV0 SET zec_usd_price 1008.33\n0285d48bd429fd9382bb54145ce66107c8faee6fb38c4e55fccb8d44f9c0bd542d4c14710dbb3f82a22d40810d57759d3e49a3825e66f74d54bd22e1c8c7527501";
const OPCODES = [
  {
    id: "set",
    name: "SET",
    kind: "Data",
    summary: "Set a key to a value using the compact single-line form.",
    authz: "An owner, or a writer scoped CREATE (new key) / UPDATE (existing key).",
    wire: "ZKV0 SET <key> <value>\n" + SIG_LINE,
    desc: "The value is a trailing token on the header line, so it cannot contain a newline and cannot be empty; those cases auto-promote to SETL. SET and SETL are semantically identical, but the signature commits to the opcode string, so a SET signature is not valid for a SETL memo or vice versa.",
    inputs: [
      { name: "key", label: "Key", ph: "zec_usd_price" },
      { name: "value", label: "Value", ph: "1008.33" }
    ],
    unsigned: (v) => `ZKV0 SET ${v.key || "<key>"} ${v.value ? v.value : "<value>"}`,
    api: (v) => ({ op: "SET", key: v.key || "", value: v.value || "" }),
    signable: true,
    errors: ["NoWriteAuthority", "OutOfScope", "StaleVersion"]
  },
  {
    id: "setl",
    name: "SETL",
    kind: "Data",
    summary: "Set a key using the length-framed form (empty / multi-line / binary values).",
    authz: "An owner, or a writer scoped CREATE (new key) / UPDATE (existing key).",
    wire: "ZKV0 SETL <key> <byte_len>\n<value, exactly byte_len bytes>\n" + SIG_LINE,
    desc: "Length-framed alternative to SET: the value follows on its own line(s), framed by an explicit byte length, so it may be empty, contain newlines, or carry arbitrary bytes. Requires byte-exact transport; the collapsed-newline fallback that rescues the header-only ops is not available for SETL.",
    inputs: [
      { name: "key", label: "Key", ph: "changelog" },
      { name: "value", label: "Value (any bytes / newlines / empty)", ph: "line one\nline two", multiline: true }
    ],
    unsigned: (v) => `ZKV0 SETL ${v.key || "<key>"} ${byteLen(v.value)}
${v.value || ""}`,
    api: (v) => ({ op: "SETL", key: v.key || "", value: v.value || "" }),
    signable: true,
    errors: ["NoWriteAuthority", "OutOfScope", "StaleVersion"]
  },
  {
    id: "del",
    name: "DEL",
    kind: "Data",
    summary: "Delete (tombstone) a key.",
    authz: "An owner, or a writer scoped DESTROY.",
    wire: "ZKV0 DEL <key>\n" + SIG_LINE,
    desc: "Tombstones the key. The per-key version counter survives the delete, so the key cannot be recreated by replaying its original SET.",
    inputs: [{ name: "key", label: "Key", ph: "zec_usd_price" }],
    unsigned: (v) => `ZKV0 DEL ${v.key || "<key>"}`,
    api: (v) => ({ op: "DEL", key: v.key || "" }),
    signable: true,
    errors: ["NoWriteAuthority", "OutOfScope", "StaleVersion"]
  },
  {
    id: "init",
    name: "INIT",
    kind: "Lifecycle",
    summary: "Bootstrap the database. Root-signed, first-valid-wins.",
    authz: "The root (UFVK-derived) key only. It becomes owner #1 and the permanent creator.",
    wire: "ZKV0 INIT <zkv_addr> [<reserved>\u2026]\n<130 hex chars, 65-byte recoverable ECDSA signature>",
    desc: "A database is not considered initialized until a signed INIT confirms at the read threshold. The echoed address is advisory (self-description); INIT binds only the receiver, so a corrected birthday or re-encoded UFVK keeps it valid. seq is always 0 (no version prefix). Reserved tokens are an unsigned forward-compat slot.",
    inputs: [],
    unsigned: () => "ZKV0 INIT <this database's zkv1\u2026 address>",
    api: () => ({ op: "INIT" }),
    signable: true,
    errors: ["ForgedInit", "InitAddressInvalid", "InitNetworkMismatch", "InitAddressMismatch", "DuplicateInit"]
  },
  {
    id: "ownerset",
    name: "OWNERSET",
    kind: "Management",
    summary: "Grant (or re-affirm) owner authority.",
    authz: "Owner-only.",
    wire: "ZKV0 OWNERSET <pubkey>\n" + SIG_LINE,
    desc: "Adds a pubkey to the owner set. Owners may write any key and add/remove owners and writers. Promoting a writer to owner clears its writer entry (owner subsumes it). The target is the canonical zkvid1\u2026 pubkey; raw hex is normalized before signing.",
    inputs: [{ name: "key", label: "Target pubkey", ph: "zkvid1\u2026 (or raw hex)" }],
    unsigned: (v) => `ZKV0 OWNERSET ${v.key || "<zkvid1\u2026>"}`,
    api: (v) => ({ op: "OWNERSET", key: v.key || "" }),
    signable: true,
    errors: ["NotOwner", "InvalidTargetPubkey", "StaleVersion"]
  },
  {
    id: "ownerdel",
    name: "OWNERDEL",
    kind: "Management",
    summary: "Revoke owner authority.",
    authz: "Owner-only.",
    wire: "ZKV0 OWNERDEL <pubkey>\n" + SIG_LINE,
    desc: "Removes a pubkey from the owner set. The last remaining owner cannot be removed; an OWNERDEL that would empty the set is dropped. The root key itself is removable once a second owner exists (key rotation).",
    inputs: [{ name: "key", label: "Target pubkey", ph: "zkvid1\u2026 (or raw hex)" }],
    unsigned: (v) => `ZKV0 OWNERDEL ${v.key || "<zkvid1\u2026>"}`,
    api: (v) => ({ op: "OWNERDEL", key: v.key || "" }),
    signable: true,
    errors: ["NotOwner", "InvalidTargetPubkey", "LastOwnerProtected", "StaleVersion"]
  },
  {
    id: "writerset",
    name: "WRITERSET",
    kind: "Management",
    summary: "Grant (or overwrite) a scoped writer.",
    authz: "Owner-only.",
    wire: "ZKV0 WRITERSET <pubkey> <scope>\n" + SIG_LINE,
    desc: 'Grants a writer a capability scope, a subset of CREATE, UPDATE, DESTROY (canonical order). WRITERSET overwrites the scope wholesale; it is not additive. Reads are public to anyone holding the address, so there is no read capability ("CRUD minus R").',
    inputs: [
      { name: "key", label: "Target pubkey", ph: "zkvid1\u2026 (or raw hex)" },
      { name: "scope", label: "Scope", type: "scope" }
    ],
    unsigned: (v) => `ZKV0 WRITERSET ${v.key || "<zkvid1\u2026>"} ${v.scope || "<scope>"}`,
    api: (v) => ({ op: "WRITERSET", key: v.key || "", scope: v.scope || "" }),
    signable: true,
    errors: ["NotOwner", "InvalidTargetPubkey", "InvalidScope", "WriterTargetIsOwner", "StaleVersion"]
  },
  {
    id: "writerdel",
    name: "WRITERDEL",
    kind: "Management",
    summary: "Revoke a writer entirely.",
    authz: "Owner-only.",
    wire: "ZKV0 WRITERDEL <pubkey>\n" + SIG_LINE,
    desc: "Removes a writer's entry entirely (all-or-nothing). Any scope token on the wire is ignored.",
    inputs: [{ name: "key", label: "Target pubkey", ph: "zkvid1\u2026 (or raw hex)" }],
    unsigned: (v) => `ZKV0 WRITERDEL ${v.key || "<zkvid1\u2026>"}`,
    api: (v) => ({ op: "WRITERDEL", key: v.key || "" }),
    signable: true,
    errors: ["NotOwner", "InvalidTargetPubkey", "StaleVersion"]
  },
  {
    id: "version",
    name: "VERSION",
    kind: "Version",
    summary: "Announce the required client/protocol epoch (forward-compat gate).",
    authz: "Owner-only.",
    wire: "ZKV0 VERSION <n> <flags>\n" + SIG_LINE,
    desc: "Owner-only forward-compat fence: once a future v1 ships, an owner can require epoch n and gate under-versioned clients. n is a decimal u32; flags is a block set, one of warn (notice only), blockall, or a subset of blocksync,blockread,blockwrite. Honored only on a single +1 step up or any downgrade (floor 0). This build is epoch 0 and never broadcasts VERSION; it only parses and honors it, so do not broadcast.",
    inputs: [
      { name: "n", label: "Epoch (n)", ph: "2", type: "number" },
      { name: "flags", label: "Flags", ph: "warn | blockall | blocksync,blockread,blockwrite" }
    ],
    unsigned: (v) => `ZKV0 VERSION ${v.n || "<n>"} ${v.flags || "<flags>"}`,
    api: (v) => ({ op: "VERSION", key: v.n || "", value: v.flags || "" }),
    signable: false,
    errors: ["NotOwner", "VersionNotNumeric", "VersionBadFlag", "VersionBelowGenesis", "VersionNoOp", "VersionJumpTooLarge"]
  },
  {
    id: "finalize",
    name: "FINALIZE",
    kind: "Lifecycle",
    summary: "Permanently seal the database, a one-way latch.",
    authz: "Owner-only.",
    wire: "ZKV0 FINALIZE\n<130 hex chars, 65-byte recoverable ECDSA signature>",
    desc: "Once a FINALIZE confirms, the database is sealed forever: every subsequent write is dropped as Finalized, including SET/SETL/DEL, OWNER*/WRITER*, and any further FINALIZE. Header-only (no key, no value) and not version-CAS'd (seq is always 0), since the latch only ever flips once. Use it to freeze a finished dataset into a permanent, read-only public record.",
    inputs: [],
    unsigned: () => "ZKV0 FINALIZE",
    api: () => ({ op: "FINALIZE" }),
    signable: true,
    errors: ["NotOwner"]
  }
];
const DOCS = [
  { id: "quickstart", label: "Quickstart" },
  { id: "design", label: "Design" },
  { id: "identifiers", label: "Identifiers" },
  { id: "faq", label: "FAQ" }
];
const RefWire = ({ children }) => /* @__PURE__ */ React.createElement("pre", { className: "value-block ref-wire" }, children);
const ErrorTable = ({ ids }) => /* @__PURE__ */ React.createElement("table", { className: "ref-errtable" }, /* @__PURE__ */ React.createElement("tbody", null, ids.map((id) => {
  const e = DROP[id];
  return /* @__PURE__ */ React.createElement("tr", { key: id }, /* @__PURE__ */ React.createElement("td", { className: "ref-errname" }, e ? e.t : id), /* @__PURE__ */ React.createElement("td", { className: "ref-errdesc" }, e ? e.d : ""));
})));
const ScopeInput = ({ value, onChange }) => {
  const parts = (value || "").split(",").filter(Boolean);
  const toggle = (cap) => {
    const set = new Set(parts);
    if (set.has(cap)) set.delete(cap);
    else set.add(cap);
    onChange(["CREATE", "UPDATE", "DESTROY"].filter((c) => set.has(c)).join(","));
  };
  return /* @__PURE__ */ React.createElement("div", { className: "ref-scope" }, ["CREATE", "UPDATE", "DESTROY"].map((c) => /* @__PURE__ */ React.createElement("label", { key: c, className: "ref-scope-opt" }, /* @__PURE__ */ React.createElement("input", { type: "checkbox", checked: parts.includes(c), onChange: () => toggle(c) }), " ", c)));
};
const OpcodePage = ({ op, databases, activeName, onCopy }) => {
  const adminDbs = (databases || []).filter((d) => d.role === "admin");
  const defaultDb = adminDbs.some((d) => d.name === activeName) ? activeName : adminDbs[0] && adminDbs[0].name || "";
  const [vals, setVals] = React.useState({});
  const [dbName, setDbName] = React.useState(defaultDb);
  const [signed, setSigned] = React.useState(null);
  const [err, setErr] = React.useState(null);
  const [busy, setBusy] = React.useState(false);
  const [signer, setSigner] = React.useState(null);
  React.useEffect(() => {
    let live = true;
    setSigner(null);
    if (op.signable && dbName) {
      window.zkvApi.detail(dbName).then((d) => {
        if (live) setSigner(d && d.signer || null);
      }).catch(() => {
      });
    }
    return () => {
      live = false;
    };
  }, [dbName]);
  const set = (name, value) => {
    setVals((v) => ({ ...v, [name]: value }));
    setSigned(null);
    setErr(null);
  };
  const unsigned = op.unsigned(vals);
  const noAdmin = adminDbs.length === 0;
  const doSign = async () => {
    setBusy(true);
    setErr(null);
    setSigned(null);
    try {
      const res = await window.zkvApi.signMemo(dbName, op.api(vals));
      setSigned(res);
    } catch (e) {
      setErr(e && e.message || "sign failed");
    }
    setBusy(false);
  };
  const copyKey = () => {
    if (signer) onCopy(signer);
    else setErr("no signer public key available for this database");
  };
  return /* @__PURE__ */ React.createElement("div", { className: "ref-op" }, /* @__PURE__ */ React.createElement("div", { className: "ref-op-title" }, /* @__PURE__ */ React.createElement("h2", { className: "ref-h" }, op.name), /* @__PURE__ */ React.createElement("span", { className: "ref-kind kind-" + op.kind.toLowerCase() }, op.kind)), /* @__PURE__ */ React.createElement("p", { className: "ref-p ref-lede" }, op.summary), /* @__PURE__ */ React.createElement("p", { className: "ref-p" }, op.desc), /* @__PURE__ */ React.createElement("div", { className: "ref-authz" }, /* @__PURE__ */ React.createElement(Icon, { name: "shield", size: 13, color: "var(--fg-3)" }), /* @__PURE__ */ React.createElement("span", null, /* @__PURE__ */ React.createElement("strong", null, "Authorized:"), " ", op.authz)), /* @__PURE__ */ React.createElement("h3", { className: "ref-sub" }, "Wire format"), /* @__PURE__ */ React.createElement(RefWire, null, op.wire), /* @__PURE__ */ React.createElement("h3", { className: "ref-sub" }, "Builder"), op.inputs.length === 0 && /* @__PURE__ */ React.createElement("p", { className: "ref-p ref-muted" }, "No fields. The memo is fully determined by the database."), op.inputs.map((f) => /* @__PURE__ */ React.createElement("div", { className: "ref-field", key: f.name }, /* @__PURE__ */ React.createElement("label", { className: "ref-field-label" }, f.label), f.type === "scope" ? /* @__PURE__ */ React.createElement(ScopeInput, { value: vals[f.name] || "", onChange: (val) => set(f.name, val) }) : f.multiline ? /* @__PURE__ */ React.createElement(
    "textarea",
    {
      className: "input ref-textarea",
      rows: 3,
      placeholder: f.ph || "",
      value: vals[f.name] || "",
      onChange: (e) => set(f.name, e.target.value)
    }
  ) : /* @__PURE__ */ React.createElement(
    "input",
    {
      className: "input",
      type: f.type === "number" ? "number" : "text",
      placeholder: f.ph || "",
      value: vals[f.name] || "",
      onChange: (e) => set(f.name, e.target.value)
    }
  ))), /* @__PURE__ */ React.createElement("div", { className: "ref-memo-head" }, "Unsigned memo"), /* @__PURE__ */ React.createElement(CopyableBlock, { text: unsigned, onCopy }), op.signable ? /* @__PURE__ */ React.createElement("div", { className: "ref-sign" }, /* @__PURE__ */ React.createElement("label", { className: "ref-field-label" }, "Sign as"), /* @__PURE__ */ React.createElement(
    "select",
    {
      className: "input",
      value: dbName,
      disabled: noAdmin,
      onChange: (e) => setDbName(e.target.value),
      style: { minWidth: 180, fontFamily: "var(--font-mono)", fontSize: 12 }
    },
    noAdmin ? /* @__PURE__ */ React.createElement("option", { value: "" }, "no admin database") : adminDbs.map((d) => /* @__PURE__ */ React.createElement("option", { key: d.name, value: d.name }, d.name))
  ), /* @__PURE__ */ React.createElement(
    "button",
    {
      className: "btn primary sm",
      disabled: noAdmin || busy || !dbName,
      onClick: doSign
    },
    busy ? "Signing\u2026" : "Sign"
  ), /* @__PURE__ */ React.createElement(
    "button",
    {
      className: "btn secondary sm",
      disabled: noAdmin || !signer,
      onClick: copyKey,
      title: "Copy this database's signer public key (zkvid1\u2026)"
    },
    /* @__PURE__ */ React.createElement(Icon, { name: "copy", className: "icon" }),
    " Copy key"
  ), noAdmin && /* @__PURE__ */ React.createElement("span", { className: "ref-hint" }, "Create or import an admin database to sign.")) : /* @__PURE__ */ React.createElement("p", { className: "ref-p ref-muted" }, "This build is protocol epoch 0 and never broadcasts VERSION. It is shown for reference only; there is no Sign action."), err && /* @__PURE__ */ React.createElement("div", { className: "ref-err" }, /* @__PURE__ */ React.createElement(Icon, { name: "alert-triangle", size: 13 }), " ", err), signed && /* @__PURE__ */ React.createElement("div", { className: "ref-signed" }, /* @__PURE__ */ React.createElement("div", { className: "ref-memo-head" }, "Signed memo"), /* @__PURE__ */ React.createElement(CopyableBlock, { text: signed.signed, onCopy }), /* @__PURE__ */ React.createElement("p", { className: "ref-p ref-muted" }, "Send a zero-value (memo-only) shielded payment to ", /* @__PURE__ */ React.createElement("code", null, signed.recipient_ua), " carrying this exact memo.")), /* @__PURE__ */ React.createElement("h3", { className: "ref-sub" }, "Errors"), /* @__PURE__ */ React.createElement("p", { className: "ref-p ref-muted" }, "Unauthorized and stale writes are ", /* @__PURE__ */ React.createElement("em", null, "silently dropped"), " on replay; the fee is spent but state doesn't change. The builder pre-checks authorization, so an unauthorized Sign fails fast instead."), /* @__PURE__ */ React.createElement(ErrorTable, { ids: op.errors }), /* @__PURE__ */ React.createElement("h4", { className: "ref-errhead" }, "Global checks (every opcode)"), /* @__PURE__ */ React.createElement(ErrorTable, { ids: globalErrorsFor(op) }));
};
const Quickstart = () => /* @__PURE__ */ React.createElement("div", { className: "ref-doc" }, /* @__PURE__ */ React.createElement("h2", { className: "ref-h" }, "Quickstart"), /* @__PURE__ */ React.createElement("p", { className: "ref-p ref-lede" }, "A zkv database is a key-value store whose entries are signed Zcash shielded memos. The chain is the source of truth; everything on disk is a rebuildable cache."), /* @__PURE__ */ React.createElement("ol", { className: "ref-steps" }, /* @__PURE__ */ React.createElement("li", null, /* @__PURE__ */ React.createElement("strong", null, "Create or import."), " Use ", /* @__PURE__ */ React.createElement("em", null, "Create"), " for a new admin database (back up the 24-word phrase) or ", /* @__PURE__ */ React.createElement("em", null, "Import"), " to restore from a phrase or watch a shared ", /* @__PURE__ */ React.createElement("code", null, "zkv1\u2026"), " address read-only."), /* @__PURE__ */ React.createElement("li", null, /* @__PURE__ */ React.createElement("strong", null, "Fund it."), " A new admin database needs a little ZEC to pay transaction fees. Use ", /* @__PURE__ */ React.createElement("em", null, "Deposit"), " to send funds to its funding address."), /* @__PURE__ */ React.createElement("li", null, /* @__PURE__ */ React.createElement("strong", null, "Initialize."), " Once funded, broadcast the ", /* @__PURE__ */ React.createElement("code", null, "INIT"), " memo. The database isn't usable until INIT confirms."), /* @__PURE__ */ React.createElement("li", null, /* @__PURE__ */ React.createElement("strong", null, "Write."), " ", /* @__PURE__ */ React.createElement("code", null, "SET"), " a key to a value, or ", /* @__PURE__ */ React.createElement("code", null, "DEL"), " a key. Each write is one signed memo (one Zcash transaction, one ZIP-317 fee)."), /* @__PURE__ */ React.createElement("li", null, /* @__PURE__ */ React.createElement("strong", null, "Read."), " Anyone holding the address can read every value. Reads are pure-local after a sync; no key required."), /* @__PURE__ */ React.createElement("li", null, /* @__PURE__ */ React.createElement("strong", null, "Delegate (optional)."), " Add owners or scoped writers under ", /* @__PURE__ */ React.createElement("em", null, "Roles"), " so authorized signers can write.")), /* @__PURE__ */ React.createElement("p", { className: "ref-p" }, "Each opcode page in this Reference lets you build the exact memo and sign it. State on disk lives in the data directory shown under ", /* @__PURE__ */ React.createElement("em", null, "Settings \u2192 Network"), "."));
const Design = ({ onCopy }) => /* @__PURE__ */ React.createElement("div", { className: "ref-doc" }, /* @__PURE__ */ React.createElement("h2", { className: "ref-h" }, "Design"), /* @__PURE__ */ React.createElement("p", { className: "ref-p ref-lede" }, "zkv stores a key-value log entirely on the Zcash blockchain. There is no separate database server; readers reconstruct state by scanning the chain with a viewing key and replaying the signed memos."), /* @__PURE__ */ React.createElement("h3", { className: "ref-sub" }, "Chain as the source of truth"), /* @__PURE__ */ React.createElement("p", { className: "ref-p" }, `A "database" is a Unified Full Viewing Key plus a birthday height, living in a single shielded pool (Orchard by default, or Sapling). A write is a zero-value shielded output to the database's own address carrying a `, /* @__PURE__ */ React.createElement("code", null, "Memo::Text"), " that begins with the wire magic ", /* @__PURE__ */ React.createElement("code", null, "ZKV0"), ". Same-block ordering is ", /* @__PURE__ */ React.createElement("code", null, "(mined_height, txid, output_index)"), "; last write wins."), /* @__PURE__ */ React.createElement("p", { className: "ref-p" }, "A complete write looks like this on the wire: a one-line header with the opcode, key, and value, then a recoverable signature line. Copy it to see the exact bytes."), /* @__PURE__ */ React.createElement(CopyableBlock, { text: SAMPLE_MEMO, onCopy }), /* @__PURE__ */ React.createElement("h3", { className: "ref-sub" }, "Snapshot + tail replay"), /* @__PURE__ */ React.createElement("p", { className: "ref-p" }, "Scanning every memo on every read would be O(total writes). Instead a local ", /* @__PURE__ */ React.createElement("code", null, "zkv_state.sqlite"), " snapshot holds the projection of memos buried past a reorg-safe depth (100 blocks); only the recent live tail is re-verified in memory per read. The snapshot is a cache, safe to delete; it rebuilds on the next read."), /* @__PURE__ */ React.createElement("h3", { className: "ref-sub" }, "Recoverable signatures"), /* @__PURE__ */ React.createElement("p", { className: "ref-p" }, "Each write carries a 65-byte recoverable ECDSA signature, so the reader recovers the signer's pubkey from the signature itself; the memo carries no pubkey field. That's what lets a database have many authorized writers without growing the wire format."), /* @__PURE__ */ React.createElement("h3", { className: "ref-sub" }, "Receiver-binding (not the address string)"), /* @__PURE__ */ React.createElement("p", { className: "ref-p" }, "Signatures commit to the database's ", /* @__PURE__ */ React.createElement("em", null, "receiver"), " (derived from the viewing key) and a network tag, not the address text. A memo signed for one database, or one network, can never verify against another."), /* @__PURE__ */ React.createElement("h3", { className: "ref-sub" }, "Replay protection (version-CAS)"), /* @__PURE__ */ React.createElement("p", { className: "ref-p" }, "Each data key and each management target carries a tombstone-surviving high-water counter folded into the signing domain. The writer's referenced sequence rides on the signature line as a compact ", /* @__PURE__ */ React.createElement("code", null, "[seq]"), " prefix. A sequence in the bounded-forward window ", /* @__PURE__ */ React.createElement("code", null, "current \u2026 current+256"), " is honored; a verbatim re-broadcast (below the window) is dropped as ", /* @__PURE__ */ React.createElement("code", null, "StaleVersion"), ", so it can't revert a value or resurrect a deleted key."), /* @__PURE__ */ React.createElement("h3", { className: "ref-sub" }, "Owners and writers"), /* @__PURE__ */ React.createElement("p", { className: "ref-p" }, "The root (UFVK-derived) key becomes owner #1 when INIT confirms, and is the permanent ", /* @__PURE__ */ React.createElement("em", null, "creator"), ". Owners may write anything and manage the registry; writers are limited to a scope of CREATE / UPDATE / DESTROY. Authorization is enforced identically by every reader (it's just replay), so it holds with no central server. An owner can also ", /* @__PURE__ */ React.createElement("code", null, "FINALIZE"), " the database, a one-way latch that seals it permanently, after which every further write is dropped."), /* @__PURE__ */ React.createElement("h3", { className: "ref-sub" }, "Privacy caveat"), /* @__PURE__ */ React.createElement("p", { className: "ref-p" }, "The address ", /* @__PURE__ */ React.createElement("em", null, "is"), " a viewing key: everyone you share it with can read all values, forever, with no forward secrecy. Don't put secrets in it; it's a public, auditable KV log."));
const Identifiers = () => /* @__PURE__ */ React.createElement("div", { className: "ref-doc" }, /* @__PURE__ */ React.createElement("h2", { className: "ref-h" }, "Identifiers"), /* @__PURE__ */ React.createElement("p", { className: "ref-p ref-lede" }, "Every identifier is a self-describing, checksummed Bech32m token; a mistyped character fails the checksum rather than denoting a different thing."), /* @__PURE__ */ React.createElement("h3", { className: "ref-sub" }, "zkv address, the database identity"), /* @__PURE__ */ React.createElement("p", { className: "ref-p" }, /* @__PURE__ */ React.createElement("code", null, "zkv1\u2026"), " (mainnet), ", /* @__PURE__ */ React.createElement("code", null, "zkvtest1\u2026"), " / ", /* @__PURE__ */ React.createElement("code", null, "zkvregtest1\u2026"), " (other networks). A single token carrying the database's Unified Full Viewing Key (transparent + one shielded pool) plus a private-use ", /* @__PURE__ */ React.createElement("em", null, "zkv-meta"), " item (typecode ", /* @__PURE__ */ React.createElement("code", null, "0x7A6B6D"), ', ascii "zkm") holding the 4-byte big-endian birthday. A unified key is only a valid zkv address if it carries that meta item. Network is recoverable from the HRP alone.'), /* @__PURE__ */ React.createElement("h3", { className: "ref-sub" }, "uview\u2026, the same key relabeled"), /* @__PURE__ */ React.createElement("p", { className: "ref-p" }, "Because a zkv address is just a viewing key under a different HRP, it relabels to a standard ", /* @__PURE__ */ React.createElement("code", null, "uview\u2026"), " that pastes into any wallet (Zashi / Ywallet / zcashd) to view the raw memos. Casual users can see a database's data by replacing the ", /* @__PURE__ */ React.createElement("code", null, "zkv"), " prefix with ", /* @__PURE__ */ React.createElement("code", null, "uview"), " in the address and importing it into a view-only wallet."), /* @__PURE__ */ React.createElement("h3", { className: "ref-sub" }, "zkvid1\u2026, a signer pubkey"), /* @__PURE__ */ React.createElement("p", { className: "ref-p" }, "The canonical Bech32m encoding of a secp256k1 public key (33-byte compressed) under the ", /* @__PURE__ */ React.createElement("code", null, "zkvid"), " HRP. This is what the owner/writer registry keys on, what management memos carry on the wire, and what signatures commit to. Commands that take a pubkey also accept raw hex, but normalize to ", /* @__PURE__ */ React.createElement("code", null, "zkvid1\u2026"), " before signing."), /* @__PURE__ */ React.createElement("h3", { className: "ref-sub" }, "The root verifying key"), /* @__PURE__ */ React.createElement("p", { className: "ref-p" }, "Derived deterministically from the UFVK's transparent component at BIP-44 path ", /* @__PURE__ */ React.createElement("code", null, "m/44'/<coin>'/<account>'/0/0"), " (external scope, index 0). It needs no seed (it's in the UFVK), is the key that broadcasts INIT, and becomes owner #1 / the permanent creator. Both writer and reader derive it the same way, so it is implicit on the wire."), /* @__PURE__ */ React.createElement("h3", { className: "ref-sub" }, "The receiver / signing domain"), /* @__PURE__ */ React.createElement("p", { className: "ref-p" }, "The signed payload is ", /* @__PURE__ */ React.createElement("code", null, 'sha256("ZKV0\\0<domain>\\0<op>\\0<key>\\0<value>")'), ". The domain is the network-tagged receiver hex (e.g. ", /* @__PURE__ */ React.createElement("code", null, "main:<receiver_hex>"), "), and for versioned ops also the entity's sequence (", /* @__PURE__ */ React.createElement("code", null, "<receiver_hex>:<seq>"), "). The receiver is the raw bytes of the database's default shielded address, derived from the viewing key, so it can't disagree with the read key, and the birthday/UFVK encoding are advisory."));
const Faq = () => /* @__PURE__ */ React.createElement("div", { className: "ref-doc" }, /* @__PURE__ */ React.createElement("h2", { className: "ref-h" }, "FAQ"), [
  ["Can anyone read my data?", "Yes, if they have your database's zkv1 address. The zkv address is a viewing key, so anyone you share it with can read every value, now and forever. There's no forward secrecy. Don't store secrets."],
  ["Who can write?", "Any authorized signer, broadcasting to the database's UA (the address derived from its viewing key). Owners may write any key; writers act within their CREATE / UPDATE / DESTROY scope. The creator, the root key that signs INIT, is the first authorized signer and becomes owner #1."],
  ["Does each write cost money?", "Yes. Every SET / DEL / INIT / management memo is one Zcash transaction with a ZIP-317 fee, real ZEC on mainnet. Fund the database's funding address first."],
  ["Is there replay protection?", "Yes. Every write to a key carries a version number, and readers only honor the next expected version. So rebroadcasting an old signed memo can't roll a value back or bring a deleted key back; it is recognized as stale and dropped. A side effect is light throttling on a single key: if two writers race, the first to confirm wins and the other has to re-read and retry. Your own back-to-back writes to the same key are fine, since the client counts its in-flight writes, and a generous forward window keeps one stuck write from blocking the ones after it."],
  ["Why was my write ignored?", "Readers silently drop writes that fail authorization (NoWriteAuthority / NotOwner / OutOfScope), replay protection (StaleVersion), or memo validity (MalformedMemo / BadSignature). See each opcode's Errors section. The builder pre-checks authorization so an unauthorized Sign fails fast."],
  ["What's the difference between SET and SETL?", "SET is the compact one-line form; SETL frames the value by byte length so it can be empty, multi-line, or binary. They're semantically identical, but a signature is bound to one opcode string; they're not interchangeable. The client auto-promotes SET \u2192 SETL when needed."],
  ["Can I sign a memo without broadcasting it?", "Yes. That's exactly what each opcode builder's Sign button does. It produces the real signed memo (the GUI counterpart of `zkv sign`); copy it and relay it through any funded wallet or the faucet."],
  ["Can a deleted key be recreated by replaying the old SET?", "No. The per-key version counter is tombstone-surviving, so an old SET falls below the window and is dropped as StaleVersion."],
  ["Why can't I remove the last owner?", "An OWNERDEL that would leave zero owners is dropped (LastOwnerProtected); the database must always stay manageable. Add a second owner first if you're rotating keys."],
  ["Can I make a database read-only forever?", "Yes. Broadcast FINALIZE. Once it confirms, the database is permanently sealed: every later write (including another FINALIZE) is dropped as Finalized. The latch is one-way and can't be undone, so freeze only a finished dataset."]
].map(([q, a], i) => /* @__PURE__ */ React.createElement("div", { className: "ref-faq", key: i }, /* @__PURE__ */ React.createElement("div", { className: "ref-faq-q" }, q), /* @__PURE__ */ React.createElement("div", { className: "ref-faq-a" }, a))));
const Reference = ({ databases, activeName, onCopy, target }) => {
  const [section, setSection] = React.useState(target || "quickstart");
  React.useEffect(() => {
    if (target) setSection(target);
  }, [target]);
  const isOp = section.indexOf("op:") === 0;
  const op = isOp ? OPCODES.find((o) => "op:" + o.id === section) : null;
  return /* @__PURE__ */ React.createElement(React.Fragment, null, /* @__PURE__ */ React.createElement("aside", { className: "ref-nav" }, /* @__PURE__ */ React.createElement("div", { className: "ref-nav-group" }, DOCS.map((d) => /* @__PURE__ */ React.createElement(
    "button",
    {
      key: d.id,
      className: "ref-nav-item" + (section === d.id ? " active" : ""),
      onClick: () => setSection(d.id)
    },
    d.label
  ))), /* @__PURE__ */ React.createElement("div", { className: "ref-nav-heading" }, "Opcodes"), /* @__PURE__ */ React.createElement("div", { className: "ref-nav-group" }, OPCODES.map((o) => /* @__PURE__ */ React.createElement(
    "button",
    {
      key: o.id,
      className: "ref-nav-item ref-nav-op" + (section === "op:" + o.id ? " active" : ""),
      onClick: () => setSection("op:" + o.id)
    },
    /* @__PURE__ */ React.createElement("span", { className: "ref-nav-op-name" }, o.name),
    /* @__PURE__ */ React.createElement("span", { className: "ref-kind kind-" + o.kind.toLowerCase() }, o.kind)
  )))), /* @__PURE__ */ React.createElement("main", { className: "ref-content", "data-screen-label": "Reference" }, section === "quickstart" && /* @__PURE__ */ React.createElement(Quickstart, null), section === "design" && /* @__PURE__ */ React.createElement(Design, { onCopy }), section === "identifiers" && /* @__PURE__ */ React.createElement(Identifiers, null), section === "faq" && /* @__PURE__ */ React.createElement(Faq, null), op && /* @__PURE__ */ React.createElement(
    OpcodePage,
    {
      key: op.id,
      op,
      databases,
      activeName,
      onCopy
    }
  )));
};
window.Reference = Reference;
window.ZKV_OPCODES = OPCODES.map((o) => ({ id: o.id, name: o.name, kind: o.kind }));
