use super::*;

/// Status of a single memo at query time.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum WriteStatus {
    /// Mined deep enough to meet the caller's `--confirmations` threshold.
    Confirmed,
    /// Below the threshold. `done` is the current confirmation count (0 if
    /// still in mempool / locally-broadcast); `required` is the threshold.
    Confirming { done: u32, required: u32 },
}

/// Whether the database has a valid signed INIT memo on chain.
///
/// A database is `Initialized` only after the admin's signed INIT memo has
/// been confirmed at the caller's threshold. Until then, SET/DEL memos are
/// dropped during replay (chain-order noise from before the DB was claimed).
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum InitState {
    /// No valid INIT memo has been observed at all.
    Uninitialized,
    /// A valid INIT memo is in flight but below the confirmation threshold.
    /// `done` is the current confirmation count (0 if still in mempool);
    /// `required` is the threshold.
    Initializing { done: u32, required: u32 },
    /// A valid INIT memo has been confirmed at the threshold; SET/DEL apply.
    Initialized,
}

impl InitState {
    pub fn is_initialized(&self) -> bool {
        matches!(self, InitState::Initialized)
    }
}

/// The database's required protocol epoch plus the capabilities an
/// under-versioned client must give up, projected from `VERSION` memos. A fresh
/// database sits at [`GENESIS_DB_VERSION`] with an empty block set until an
/// owner broadcasts a `VERSION` memo.
///
/// Transition policy (enforced identically by the in-memory replay and the
/// snapshot promote path, both via [`apply_version`](VersionState::apply_version)):
/// a `VERSION n` is honored only if `n == current + 1` (a single-step upgrade)
/// or `n < current` (any downgrade, floor [`GENESIS_DB_VERSION`]); a multi-step
/// jump (`n > current + 1`) and a no-op (`n == current`) are dropped. The
/// one-step-up rule makes a "jump to a huge number" denial-of-service infeasible
/// (each step costs a separate on-chain fee); free downgrades let a later owner
/// undo a mistaken or malicious bump in a single memo.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct VersionState {
    pub version: u32,
    pub blocks: BlockSet,
}

impl Default for VersionState {
    fn default() -> Self {
        VersionState {
            version: GENESIS_DB_VERSION,
            blocks: BlockSet::default(),
        }
    }
}

impl VersionState {
    /// Is the database's required epoch newer than this build supports?
    pub fn is_outdated(&self) -> bool {
        self.version > MAX_DB_VERSION
    }

    /// Should an out-of-date client stop scanning the chain?
    pub fn blocks_sync(&self) -> bool {
        self.is_outdated() && self.blocks.contains(BlockCap::Sync)
    }

    /// Should an out-of-date client refuse to interpret/display state?
    pub fn blocks_read(&self) -> bool {
        self.is_outdated() && self.blocks.contains(BlockCap::Read)
    }

    /// Should an out-of-date client refuse to broadcast writes?
    pub fn blocks_write(&self) -> bool {
        self.is_outdated() && self.blocks.contains(BlockCap::Write)
    }

    /// A human-readable upgrade notice when the database is newer than this
    /// build, else `None`.
    pub fn upgrade_warning(&self) -> Option<String> {
        self.is_outdated().then(|| {
            format!(
                "this database has been upgraded to version {} (this build supports up to {}); \
                 update zkv to the latest version",
                self.version, MAX_DB_VERSION,
            )
        })
    }

    /// Is moving from `current` to `requested` a legal transition? Pure, so both
    /// the replay/promote apply path and a future pre-broadcast check share one
    /// rule. Accepts a single-step upgrade or any downgrade (floor
    /// [`GENESIS_DB_VERSION`]); rejects no-ops and multi-step jumps.
    pub fn transition_allowed(current: u32, requested: u32) -> Result<(), DropReason> {
        // Floor at the genesis epoch. With `GENESIS_DB_VERSION == 0` this is
        // unreachable (no `u32` is below zero), but the check is retained so the
        // floor still holds if a future build raises the genesis epoch.
        #[allow(clippy::absurd_extreme_comparisons)]
        if requested < GENESIS_DB_VERSION {
            return Err(DropReason::VersionBelowGenesis);
        }
        if requested == current {
            return Err(DropReason::VersionNoOp);
        }
        if requested > current + 1 {
            return Err(DropReason::VersionJumpTooLarge { current, requested });
        }
        Ok(())
    }

    /// Apply a confirmed, owner-authorized `VERSION` memo. `key` is the decimal
    /// new version; `value` is the [`BlockSet`] wire token. On a legal
    /// transition mutates `self` and returns `Ok`; otherwise returns the
    /// [`DropReason`] and leaves `self` unchanged (the memo is dropped).
    pub fn apply_version(&mut self, key: &str, value: Option<&str>) -> Result<(), DropReason> {
        let requested: u32 = key.parse().map_err(|_| DropReason::VersionNotNumeric)?;
        let blocks = value
            .and_then(BlockSet::parse)
            .ok_or(DropReason::VersionBadFlag)?;
        Self::transition_allowed(self.version, requested)?;
        self.version = requested;
        self.blocks = blocks;
        Ok(())
    }
}
