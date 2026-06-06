use super::*;

/// Fixed BIP-44 scope for the zkv signing key (external).
pub const ZKV_TRANSPARENT_SCOPE: TransparentKeyScope = TransparentKeyScope::EXTERNAL;

/// Fixed BIP-44 address index for the zkv signing key.
pub const ZKV_TRANSPARENT_INDEX: u32 = 0;

/// Magic prefix included in canonical signed bytes (separate from wire form).
///
/// `ZKV0` is the receiver-bound, replay-protected signing domain: signatures
/// commit to the database's **shielded receiver** (not the address string) and
/// to a per-key/per-target **version** (see [`signed_payload`] /
/// [`receiver_domain`] / [`signing_domain`]).
pub(crate) const SIGNED_MAGIC: &[u8] = b"ZKV0";

/// Magic prefix on the wire (first token of a memo's first line).
pub(crate) const WIRE_MAGIC: &str = "ZKV0";

/// The zkv protocol version this build speaks. The wire/signing magic is
/// `ZKV<version>` (so this build is `WIRE_MAGIC` `"ZKV0"`). A memo whose magic
/// carries a *higher* version comes from a newer protocol we can't parse;
/// replay surfaces it as [`DropReason::UnsupportedVersion`] (with a "download
/// the latest zkv" message) rather than silently ignoring it. Any other first
/// token is foreign traffic (`NotZkv`).
pub const ZKV_VERSION: u32 = 0;

/// The version-independent magic family prefix. The full wire magic is this
/// prefix followed by the decimal protocol version (e.g. `ZKV0`).
pub(crate) const MAGIC_PREFIX: &str = "ZKV";

/// The database-version epoch a freshly-initialized database sits at, before
/// any `VERSION` memo has been confirmed.
pub const GENESIS_DB_VERSION: u32 = 0;

/// The maximum database-version epoch this build fully supports. Distinct from
/// [`ZKV_VERSION`] (the `ZKV0` wire-magic axis), though both are `0` today: a
/// `VERSION <n>` memo announces the epoch a database now requires, and a client
/// with `MAX_DB_VERSION < n` is out of date: it warns and (per the memo's
/// block flags) may refuse to sync/read/write. A future client that adds support
/// for a new epoch bumps this constant. This build never *broadcasts* a
/// `VERSION` memo; it only parses and honors them.
pub const MAX_DB_VERSION: u32 = 0;

/// Length in bytes of a wire signature: a 64-byte compact ECDSA signature
/// plus a 1-byte recovery id. The recovery id lets readers recover the
/// signer's public key from the signature alone, so a memo identifies *who*
/// signed it without carrying the pubkey explicitly.
pub const SIG_LEN: usize = 65;

/// Hex-encoded signature length on the wire (`SIG_LEN * 2`).
pub(crate) const SIG_HEX_LEN: usize = SIG_LEN * 2;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Op {
    Set,
    /// Length-framed SET: same semantic as `Set`, but the wire form encodes
    /// the value with a byte-length prefix so it can carry newlines, empty
    /// values, and any other content that the trailing-token `SET` form
    /// can't round-trip safely. The signed payload commits to the `SETL`
    /// op string, so a `SET` signature is not valid for a `SETL` memo
    /// (and vice versa): the writer authorizes a specific wire encoding.
    SetL,
    Del,
    /// Signed claim that the admin has committed to this zkv_addr on-chain.
    /// Required before any SET/DEL is honored.
    Init,
    /// Grant (or re-affirm) owner authority to a public key. Owner-only.
    /// Wire `key` is the target's canonical `zkvid1…` pubkey; no value.
    OwnerSet,
    /// Revoke owner authority from a public key. Owner-only. The last
    /// remaining owner cannot be removed (the database must stay manageable).
    OwnerDel,
    /// Grant (or overwrite) a scoped writer. Owner-only. Wire `key` is the
    /// target pubkey; `value` is the comma-separated capability scope
    /// (`CREATE`, `UPDATE`, `DESTROY`). A later `WRITERSET` for the same key
    /// replaces the previous scope wholesale.
    WriterSet,
    /// Revoke a scoped writer entirely. Owner-only. Any scope argument is
    /// ignored; removal is all-or-nothing.
    WriterDel,
    /// Permanently seal the database. Owner-only. Carries no key and no value.
    /// Once a FINALIZE confirms, every subsequent write (`SET`/`SETL`/`DEL`,
    /// `OWNER*`/`WRITER*`, and any further `FINALIZE`) is dropped during
    /// replay, by anyone. The latch is one-way: reads still work, but the
    /// database can never be written to again.
    Finalize,
    /// Owner-only announcement that the database now requires client/protocol
    /// epoch `<n>` (the wire `key`, a decimal `u32`). The wire `value` is the
    /// block set ([`BlockSet`]): which capabilities an under-versioned client
    /// must give up (`warn` = none, `blockall` = all). Read-only in this build:
    /// we parse and honor `VERSION` but never broadcast it. Folded into
    /// [`VersionState`] during replay; the transition is gated one-step-up /
    /// free-down by [`VersionState::apply_version`].
    Version,
}

impl Op {
    pub fn as_str(self) -> &'static str {
        match self {
            Op::Set => "SET",
            Op::SetL => "SETL",
            Op::Del => "DEL",
            Op::Init => "INIT",
            Op::OwnerSet => "OWNERSET",
            Op::OwnerDel => "OWNERDEL",
            Op::WriterSet => "WRITERSET",
            Op::WriterDel => "WRITERDEL",
            Op::Finalize => "FINALIZE",
            Op::Version => "VERSION",
        }
    }

    pub(crate) fn parse(s: &str) -> Option<Self> {
        match s {
            "SET" => Some(Op::Set),
            "SETL" => Some(Op::SetL),
            "DEL" => Some(Op::Del),
            "INIT" => Some(Op::Init),
            "OWNERSET" => Some(Op::OwnerSet),
            "OWNERDEL" => Some(Op::OwnerDel),
            "WRITERSET" => Some(Op::WriterSet),
            "WRITERDEL" => Some(Op::WriterDel),
            "FINALIZE" => Some(Op::Finalize),
            "VERSION" => Some(Op::Version),
            _ => None,
        }
    }

    /// True for the owner-only ops that are not per-key data writes:
    /// the registry-management ops (`OWNER*`/`WRITER*`) plus `FINALIZE`.
    /// These are honored only when signed by a current owner.
    pub fn is_management(self) -> bool {
        matches!(
            self,
            Op::OwnerSet | Op::OwnerDel | Op::WriterSet | Op::WriterDel | Op::Finalize
        )
    }

    /// True for the data ops that mutate per-key state (`SET`/`SETL`/`DEL`).
    pub fn is_data(self) -> bool {
        matches!(self, Op::Set | Op::SetL | Op::Del)
    }

    /// True for ops whose signing domain folds in a replay-protection version
    /// (per-key for data ops, per-target for the `OWNER*`/`WRITER*` management
    /// ops). `INIT`/`VERSION`/`FINALIZE` bind only the receiver, so they are not
    /// version-CAS'd (FINALIZE is a one-way latch, so it needs no per-target
    /// sequence even though it is an owner-only management op).
    pub fn is_versioned(self) -> bool {
        matches!(
            self,
            Op::Set
                | Op::SetL
                | Op::Del
                | Op::OwnerSet
                | Op::OwnerDel
                | Op::WriterSet
                | Op::WriterDel
        )
    }

    /// Pick the right SET wire form for a given value. Returns `Op::Set`
    /// for the common case (non-empty, no embedded newlines) so we keep
    /// the compact one-line wire; falls back to `Op::SetL` only when the
    /// value would otherwise be unrepresentable in `Op::Set`.
    pub fn set_for_value(value: &str) -> Op {
        if value.is_empty() || value.contains('\n') {
            Op::SetL
        } else {
            Op::Set
        }
    }
}

/// A single capability a scoped writer can hold. "CRUD minus R": reads are
/// public to anyone holding the UFVK, so there is no read capability.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub enum Capability {
    /// `SET`/`SETL` of a key that does **not** currently have a confirmed
    /// value (creating a new key).
    Create,
    /// `SET`/`SETL` of a key that **does** currently have a confirmed value
    /// (overwriting an existing key).
    Update,
    /// `DEL` of a key.
    Destroy,
}

impl Capability {
    pub fn as_str(self) -> &'static str {
        match self {
            Capability::Create => "CREATE",
            Capability::Update => "UPDATE",
            Capability::Destroy => "DESTROY",
        }
    }

    fn parse(s: &str) -> Option<Self> {
        match s {
            "CREATE" => Some(Capability::Create),
            "UPDATE" => Some(Capability::Update),
            "DESTROY" => Some(Capability::Destroy),
            _ => None,
        }
    }
}

/// A writer's capability set, encoded on the wire as a comma-separated list
/// (e.g. `CREATE,UPDATE`). Order-insensitive and de-duplicated; the canonical
/// string form sorts capabilities `CREATE,UPDATE,DESTROY`.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Scope(BTreeSet<Capability>);

impl Scope {
    /// Build a scope from an iterator of capabilities.
    pub fn from_caps<I: IntoIterator<Item = Capability>>(caps: I) -> Self {
        Scope(caps.into_iter().collect())
    }

    /// Parse a comma-separated scope string (`"CREATE,DESTROY"`). Whitespace
    /// around tokens is tolerated. Returns `None` on any unrecognized token
    /// or if the resulting scope is empty (an empty scope is meaningless;
    /// use `WRITERDEL` to remove a writer).
    pub fn parse(s: &str) -> Option<Self> {
        let mut caps = BTreeSet::new();
        for tok in s.split(',') {
            let tok = tok.trim();
            if tok.is_empty() {
                continue;
            }
            caps.insert(Capability::parse(tok)?);
        }
        if caps.is_empty() {
            return None;
        }
        Some(Scope(caps))
    }

    /// Canonical wire form: capabilities sorted and comma-joined, no spaces.
    pub fn to_wire(&self) -> String {
        self.0
            .iter()
            .map(|c| c.as_str())
            .collect::<Vec<_>>()
            .join(",")
    }

    pub fn contains(&self, cap: Capability) -> bool {
        self.0.contains(&cap)
    }

    pub fn capabilities(&self) -> impl Iterator<Item = Capability> + '_ {
        self.0.iter().copied()
    }
}

/// One capability an under-versioned client must give up when it reads a
/// database that has moved to a newer epoch via a `VERSION` memo, specifying
/// what to block. The three are independent (a set, not a ladder).
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub enum BlockCap {
    /// Stop scanning the chain for new blocks (view cached state only).
    Sync,
    /// Stop interpreting/displaying state (`get`/`show`/`history`/…).
    Read,
    /// Stop broadcasting writes (`set`/`del`/owner/writer).
    Write,
}

impl BlockCap {
    pub fn as_str(self) -> &'static str {
        match self {
            BlockCap::Sync => "blocksync",
            BlockCap::Read => "blockread",
            BlockCap::Write => "blockwrite",
        }
    }

    fn parse(s: &str) -> Option<Self> {
        match s {
            "blocksync" => Some(BlockCap::Sync),
            "blockread" => Some(BlockCap::Read),
            "blockwrite" => Some(BlockCap::Write),
            _ => None,
        }
    }
}

/// The set of capabilities a `VERSION` memo tells under-versioned clients to
/// give up, encoded on the wire as a single token: `warn` (block nothing),
/// `blockall` (block all three), or a comma-separated subset of
/// `blocksync,blockread,blockwrite` (canonical order). Modeled on [`Scope`]. An
/// empty set is the `warn` case, meaning a database can move to a new epoch while
/// staying fully back-compatible.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct BlockSet(BTreeSet<BlockCap>);

impl BlockSet {
    /// All three capabilities (the `blockall` case).
    pub fn all() -> Self {
        BlockSet(
            [BlockCap::Sync, BlockCap::Read, BlockCap::Write]
                .into_iter()
                .collect(),
        )
    }

    /// Build a block set from an iterator of capabilities.
    pub fn from_caps<I: IntoIterator<Item = BlockCap>>(caps: I) -> Self {
        BlockSet(caps.into_iter().collect())
    }

    /// Parse the wire flag token. `warn` → empty; `blockall` → all three; a
    /// comma-separated list of `block*` members → that set. Returns `None` on
    /// any unrecognized token (including mixing `warn`/`blockall` into a list)
    /// or an empty string; callers surface that as [`DropReason::VersionBadFlag`].
    pub fn parse(s: &str) -> Option<Self> {
        match s.trim() {
            "" => None,
            "warn" => Some(BlockSet::default()),
            "blockall" => Some(BlockSet::all()),
            list => {
                let mut caps = BTreeSet::new();
                for tok in list.split(',') {
                    caps.insert(BlockCap::parse(tok.trim())?);
                }
                if caps.is_empty() {
                    return None;
                }
                Some(BlockSet(caps))
            }
        }
    }

    /// Canonical wire form: `warn` when empty, `blockall` when full, else the
    /// members comma-joined in canonical order.
    pub fn to_wire(&self) -> String {
        if self.0.is_empty() {
            return "warn".to_owned();
        }
        if self.0.len() == 3 {
            return "blockall".to_owned();
        }
        self.0
            .iter()
            .map(|c| c.as_str())
            .collect::<Vec<_>>()
            .join(",")
    }

    pub fn contains(&self, cap: BlockCap) -> bool {
        self.0.contains(&cap)
    }

    pub fn is_empty(&self) -> bool {
        self.0.is_empty()
    }
}

/// The authority a given pubkey holds over a database, as projected by replay.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Authority {
    /// Full authority: write any key, manage owners, manage writers.
    Owner,
    /// Scoped write authority only (no registry management).
    Writer(Scope),
}

/// Structured cause for a memo whose *shape* is wrong: it carries the `ZKV0`
/// magic but the rest doesn't conform to the wire grammar. Carried inside
/// [`DropReason::MalformedMemo`] so the reason is standardized and matchable
/// rather than a free-form string. A memo with no `ZKV0` prefix is *not* a
/// `MemoFormat` error; it's [`MemoReject::NotZkv`], foreign traffic that the
/// history view filters out entirely.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum MemoFormat {
    /// The token after `ZKV0` is not a recognized opcode.
    UnknownOpcode,
    /// The key field is missing or empty.
    EmptyKey,
    /// The key field contains a control character (including NUL). Keys must be
    /// free of these so the NUL-delimited [`signed_payload`] stays injective
    /// across the `key`/`value` boundary; otherwise a captured signature could
    /// be re-split into a different `(key, value)` with identical signed bytes.
    ControlCharInKey,
    /// Wrong number of parameters for the opcode (extra or missing tokens
    /// where the grammar is fixed, e.g. a `DEL` with a trailing token).
    WrongArity { op: Op },
    /// A `SET`/`SETL` carried no value (or an empty one in the `SET` form).
    MissingValue,
    /// A `WRITERSET` carried no scope token.
    MissingScope,
    /// A `VERSION` carried no block-flags token.
    MissingVersionFlag,
    /// No signature line / the memo is too short to hold a signature.
    MissingSignature,
    /// The signature region is present but not 130 hex characters.
    BadSignatureFraming,
    /// The `SETL` length token is not a number.
    SetlNonNumericLength,
    /// The `SETL` declared length runs past the value into the signature.
    SetlLengthOverrun,
    /// The `SETL` value is not followed by the `\n` separator.
    SetlMissingSeparator,
    /// The `SETL` value bytes are not valid UTF-8.
    SetlValueNotUtf8,
    /// A `SETL` arrived in newline-collapsed form, which length-prefix framing
    /// cannot survive.
    SetlCollapsedUnsupported,
}

impl fmt::Display for MemoFormat {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            MemoFormat::UnknownOpcode => write!(f, "unknown opcode"),
            MemoFormat::EmptyKey => write!(f, "empty key"),
            MemoFormat::ControlCharInKey => write!(f, "key contains a control character"),
            MemoFormat::WrongArity { op } => {
                write!(f, "wrong number of parameters for {}", op.as_str())
            }
            MemoFormat::MissingValue => write!(f, "SET requires a non-empty value"),
            MemoFormat::MissingScope => write!(f, "WRITERSET requires a scope"),
            MemoFormat::MissingVersionFlag => write!(f, "VERSION requires block flags"),
            MemoFormat::MissingSignature => write!(f, "missing signature"),
            MemoFormat::BadSignatureFraming => write!(f, "signature not 130 hex chars"),
            MemoFormat::SetlNonNumericLength => write!(f, "SETL length is not a number"),
            MemoFormat::SetlLengthOverrun => {
                write!(f, "SETL length runs past the value into the signature")
            }
            MemoFormat::SetlMissingSeparator => write!(f, "SETL value not followed by a newline"),
            MemoFormat::SetlValueNotUtf8 => write!(f, "SETL value is not valid UTF-8"),
            MemoFormat::SetlCollapsedUnsupported => {
                write!(f, "SETL cannot be recovered from a newline-collapsed memo")
            }
        }
    }
}

/// The standardized reason a memo did not take effect during replay. Every
/// silent `continue` in the replay/snapshot paths corresponds to one of these.
/// `GLOBAL` reasons (`MalformedMemo`, `BadSignature`) can apply to any opcode;
/// the rest are lifecycle/authorization policy keyed to specific opcodes.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum DropReason {
    /// The memo's structure is wrong. See [`MemoFormat`] for the precise cause.
    MalformedMemo(MemoFormat),
    /// The signature is well-framed (130 hex chars) but does not recover to a
    /// public key over this payload (bad bytes, bad recovery id, or recovery
    /// failure). A reader cannot (and should not) distinguish a forged
    /// signature from a corrupt one, so all such cases collapse here.
    BadSignature,
    /// A zkv memo from a newer protocol version than this build understands
    /// (magic `ZKV<version>` with `version` > [`ZKV_VERSION`]).
    UnsupportedVersion { version: u32 },
    /// INIT not signed by the root key (the embedded address *was* this
    /// database's, but the signer was someone else: a genuine forgery).
    ForgedInit,
    /// INIT embeds a string that does not parse as a valid zkv address (wrong
    /// address type, missing transparent component, no shielded component, or
    /// garbage). A well-behaved client never emits this; raw broadcasters can.
    InitAddressInvalid,
    /// INIT embeds a valid zkv address but for a *different network* than the
    /// one this database is being read on (e.g. a testnet address observed on
    /// mainnet). The network label is part of the address, so this is a hard
    /// mismatch, not a signature question.
    InitNetworkMismatch,
    /// INIT embeds a valid, same-network zkv address that simply isn't this
    /// database's: a different database's INIT delivered into our memo
    /// stream, or the right UFVK with a wrong birthday.
    InitAddressMismatch,
    /// A second INIT after the database was already initialized.
    DuplicateInit,
    /// A data or management op before INIT was honored.
    NotInitialized,
    /// A data op whose signer holds neither owner nor writer authority.
    NoWriteAuthority,
    /// A **version-CAS miss**: the sequence the writer referenced (the `[seq]`
    /// prefix on the signature line) is outside the entity's accepted window
    /// `current ..= current + `[`VERSION_WINDOW`]. A `seq < current` is a stale
    /// replay (a verbatim re-broadcast of an already-honored write, or a lost
    /// compare-and-swap); a `seq` far ahead is a desync beyond tolerance. The
    /// bounded *forward* window (rather than exact match) means a single
    /// in-flight write that never confirms doesn't strand the writer's later
    /// writes: they still land within the window, while the upper bound keeps
    /// the counter from being jumped to a huge value (no freeze). Not a
    /// signature *failure*: the signature is real, just bound to a sequence the
    /// reader won't honor.
    StaleVersion,
    /// A data op outside the writer's scope (lacks the needed capability).
    OutOfScope { capability: Capability },
    /// A management op (`OWNER*`/`WRITER*`) not signed by a current owner.
    NotOwner,
    /// An `OWNERDEL` that would remove the last remaining owner.
    LastOwnerProtected,
    /// A `WRITERSET` targeting a pubkey that is already an owner.
    WriterTargetIsOwner,
    /// A management op whose target is not a valid secp256k1 public key.
    InvalidTargetPubkey,
    /// A `WRITERSET` whose value did not parse as a non-empty capability scope.
    InvalidScope,
    /// Any write after a confirmed `FINALIZE` sealed the database, including a
    /// second `FINALIZE`. The latch is one-way; nothing more is ever applied.
    Finalized,
    /// A `VERSION` whose key (the version number) is not a decimal `u32`.
    VersionNotNumeric,
    /// A `VERSION` whose value did not parse as a block set (`warn` / `blockall`
    /// / a comma-subset of `blocksync,blockread,blockwrite`).
    VersionBadFlag,
    /// A `VERSION` requesting a version below [`GENESIS_DB_VERSION`].
    VersionBelowGenesis,
    /// A `VERSION` requesting the version the database is already at (a no-op).
    VersionNoOp,
    /// A `VERSION` jumping more than one epoch in a single memo. Versions may
    /// only increase one step at a time (downgrades may jump freely).
    VersionJumpTooLarge { current: u32, requested: u32 },
}

impl DropReason {
    /// Whether this drop is a *signature-level* failure: the memo never
    /// yielded a recoverable signer, so there is nobody to authorize. The
    /// complement (every other variant) is reached only after a signature
    /// recovered successfully, i.e. it is an authorization/lifecycle decision
    /// on a cryptographically valid signature. Lets a rejections view split a
    /// "Valid Signature ✓ / Authorized ✗" display.
    pub fn is_signature_failure(&self) -> bool {
        matches!(
            self,
            DropReason::MalformedMemo(_)
                | DropReason::BadSignature
                | DropReason::UnsupportedVersion { .. }
        )
    }
}

impl fmt::Display for DropReason {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            DropReason::MalformedMemo(fmt) => write!(f, "malformed memo: {fmt}"),
            DropReason::BadSignature => write!(f, "unrecoverable signature"),
            DropReason::UnsupportedVersion { version } => write!(
                f,
                "ZKV protocol version {version} not recognized, download the latest version of zkv"
            ),
            DropReason::ForgedInit => write!(f, "INIT not signed by the root key"),
            DropReason::InitAddressInvalid => write!(f, "INIT embeds an invalid zkv address"),
            DropReason::InitNetworkMismatch => {
                write!(f, "INIT address is for a different network")
            }
            DropReason::InitAddressMismatch => {
                write!(f, "INIT address is for a different database")
            }
            DropReason::DuplicateInit => write!(f, "database already initialized"),
            DropReason::NotInitialized => write!(f, "database not initialized"),
            DropReason::NoWriteAuthority => write!(f, "signer has no write authority"),
            DropReason::StaleVersion => {
                write!(f, "stale-version replay of an already-honored write")
            }
            DropReason::OutOfScope { capability } => {
                write!(f, "writer scope lacks {}", capability.as_str())
            }
            DropReason::NotOwner => write!(f, "management op not signed by an owner"),
            DropReason::LastOwnerProtected => write!(f, "attempt to remove the last owner"),
            DropReason::WriterTargetIsOwner => write!(f, "writer target is already an owner"),
            DropReason::InvalidTargetPubkey => write!(f, "invalid target pubkey"),
            DropReason::InvalidScope => write!(f, "invalid writer scope"),
            DropReason::Finalized => write!(f, "database is finalized"),
            DropReason::VersionNotNumeric => write!(f, "VERSION number is not a u32"),
            DropReason::VersionBadFlag => write!(f, "invalid VERSION block flags"),
            DropReason::VersionBelowGenesis => write!(f, "VERSION below the genesis version"),
            DropReason::VersionNoOp => write!(f, "VERSION already at this version"),
            DropReason::VersionJumpTooLarge { current, requested } => write!(
                f,
                "VERSION jumps from {current} to {requested} (may only increase one step at a time)"
            ),
        }
    }
}
