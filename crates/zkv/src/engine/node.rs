//! Node lifecycle and the chain operations that need one.
//!
//! zecd's node is a long-lived thing: it owns the datadir lock, runs a wallet
//! actor per wallet, and syncs on its own cadence. zkv's commands are mostly
//! one-shot, so the node is started on demand and stopped when the caller is
//! done with it, and the interesting work is in the two places where those
//! models meet: acquiring the lock (zecd refuses where zkv blocks) and knowing
//! when a sync has actually finished (zecd has no sync-now call).

use std::sync::atomic::Ordering;
use std::sync::Arc;
use std::time::{Duration, Instant};

use tracing::{debug, info, warn};
use zecd::node::{Node, NodeBuilder};

use crate::internal::sync::CancelFlag;

use super::{config::WALLET, Engine, EngineError, EngineKind};

/// How long each `waitforsync` call blocks before returning so the cancel flag
/// can be checked.
///
/// The barrier itself would happily wait indefinitely; slicing it is only what
/// keeps a cancel responsive, since the wait cannot be interrupted from here.
/// It does not pace the sync, and a slice expiring is not an error.
const SYNC_WAIT_SLICE: Duration = Duration::from_millis(500);

/// How long to wait between attempts to start a node whose datadir another
/// process holds.
const BUSY_RETRY_INTERVAL: Duration = Duration::from_millis(500);

/// What a sync run found.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct SyncOutcome {
    /// The height the wallet has now fully scanned, and at which its memos are
    /// complete.
    pub scanned_height: u32,
    /// The chain tip the node reported, which at a successful sync equals
    /// [`SyncOutcome::scanned_height`]: a sync only reports success once the
    /// scan has reached the tip *and* the enhancement backlog has drained, so
    /// at that moment the two are the same number.
    ///
    /// Falls back to the scanned height when the node reported no tip, so it
    /// is never a number the wallet has not reached. Use [`Engine::chain_tip`]
    /// for the tip without waiting for the wallet to catch up to it.
    pub chain_tip: u32,
}

impl Engine {
    /// The running node, started if it is not running yet.
    ///
    /// Starting is the expensive step: it takes the datadir lock, opens the
    /// wallet database and runs any pending librustzcash migrations, spawns
    /// the wallet actor, and kicks off the background build of the Orchard
    /// proving key. The node then stays up until [`Engine::shutdown`], so a
    /// caller that performs several operations pays this once.
    pub async fn node(&self) -> Result<Arc<Node>, EngineError> {
        debug_assert!(
            tokio::runtime::Handle::try_current()
                .map(|h| h.runtime_flavor() != tokio::runtime::RuntimeFlavor::CurrentThread)
                .unwrap_or(true),
            "the wallet engine needs a multi-thread tokio runtime: zecd's scan and proving \
             paths call block_in_place, which panics on a current-thread runtime",
        );

        let mut slot = self.node.lock().await;
        if let Some(node) = slot.as_ref() {
            return Ok(Arc::clone(node));
        }
        let node = Arc::new(self.start_node().await?);
        *slot = Some(Arc::clone(&node));
        Ok(node)
    }

    /// Start a node, waiting out another process's lock if we are allowed to.
    ///
    /// zecd refuses a datadir whose lock is held, while every zkv command so
    /// far has *blocked* until its turn. Retrying preserves that: a `zkv set`
    /// run while the GUI is mid-sync waits instead of failing. A caller that
    /// would rather skip (the GUI's own auto-sync loop, which will come back
    /// next pass) sets no wait and gets [`EngineError::Busy`] immediately.
    async fn start_node(&self) -> Result<Node, EngineError> {
        let deadline = self.busy_wait.map(|w| Instant::now() + w);
        let mut warned = false;
        loop {
            match NodeBuilder::new(self.app.clone()).start().await {
                Ok(node) => return Ok(node),
                Err(e) if is_locked(&e) => {
                    let Some(deadline) = deadline else {
                        return Err(EngineError::Busy(self.db_name.clone()));
                    };
                    if Instant::now() >= deadline {
                        return Err(EngineError::Busy(self.db_name.clone()));
                    }
                    if !warned {
                        warned = true;
                        warn!(
                            "another zkv process is using database '{}'; waiting for it to \
                             finish",
                            self.db_name,
                        );
                    }
                    tokio::time::sleep(BUSY_RETRY_INTERVAL).await;
                }
                // Anything else is fatal, and the one-time layout migration
                // runs in here (moving librustzcash's files into the per-coin
                // engine directory), so its refusals surface as a failed
                // start. zecd's own messages already name the paths and the
                // remedy; what they cannot name is which zkv database, since
                // from its side every one of ours is the wallet called
                // `default`. So add that and pass the rest through intact.
                Err(e) => {
                    return Err(EngineError::Other(e.context(format!(
                        "starting the wallet engine for database '{}'",
                        self.db_name,
                    ))))
                }
            }
        }
    }

    /// Stop the node, if one is running. Idempotent.
    ///
    /// Waits for the wallet actor so the wallet database is closed cleanly and
    /// the datadir lock is released, rather than leaving the next process to
    /// wait out a lock nobody is using.
    pub async fn shutdown(&self) {
        let taken = self.node.lock().await.take();
        let Some(node) = taken else { return };
        match Arc::try_unwrap(node) {
            // The usual case: we hold the only handle, so we can shut down and
            // wait for the actors.
            Ok(node) => node.shutdown().await,
            // Someone else is still holding a handle (a concurrent operation
            // on this engine). Ask for shutdown and let their handle's drop
            // finish it, rather than blocking here or cutting them off.
            Err(node) => node.trigger_shutdown(),
        }
    }

    /// Sync to the chain tip and return when the wallet can serve complete
    /// memos for everything up to it.
    ///
    /// "Caught up" has to mean more than "the block scan reached the tip".
    /// Compact blocks carry no memos, so a scanned block's memos arrive only
    /// once the full transactions behind it are fetched, and that runs as a
    /// separate pass afterwards. Reading in between would see rows whose memo
    /// is still NULL, which is exactly what
    /// [`crate::internal::state`]'s promote clamp refuses to fold into the
    /// snapshot. zecd reports both conditions as one flag, so this waits for
    /// that flag to clear.
    ///
    /// The node syncs on its own cadence; there is no sync-now call to make,
    /// so this observes rather than drives.
    pub async fn sync_to_tip(
        &self,
        cancel: Option<CancelFlag>,
    ) -> Result<SyncOutcome, EngineError> {
        self.sync_wallet(WALLET, cancel).await
    }

    /// [`Engine::sync_to_tip`] for one named wallet.
    ///
    /// On a fleet node a "sync" is per member: the shard scans once for all of
    /// them, but each member's readiness (has its account been imported, has
    /// the scan reached its birthday) is its own. The wait itself is
    /// shard-wide, which under-reports rather than over-reports: it can say
    /// "not yet" while this member is already complete, never the reverse.
    pub async fn sync_wallet(
        &self,
        wallet: &str,
        cancel: Option<CancelFlag>,
    ) -> Result<SyncOutcome, EngineError> {
        match self.sync_wallet_once(wallet, cancel.clone()).await {
            // A shared-scan node that has been up since before this member's
            // manifest was written does not know about it: the manifest
            // directory is read at start. Ask it to pick the member up and try
            // again, so a `zkv watch` in a terminal starts being scanned by a
            // GUI that is already running, rather than at its next restart.
            Err(EngineError::Rpc { code, .. })
                if code == super::codes::UNKNOWN_WALLET
                    && matches!(self.kind, EngineKind::Fleet { .. }) =>
            {
                self.load_member(wallet).await?;
                self.sync_wallet_once(wallet, cancel).await
            }
            other => other,
        }
    }

    async fn sync_wallet_once(
        &self,
        wallet: &str,
        cancel: Option<CancelFlag>,
    ) -> Result<SyncOutcome, EngineError> {
        let node = self.node().await?;
        let progress = self.progress_for(wallet);
        let mut last_logged = 0;
        loop {
            if cancelled(&cancel) {
                return Err(EngineError::Cancelled);
            }
            // The timeout is in milliseconds, and a slice expiring comes back
            // as `synced: false` rather than as an error, so the loop branches
            // on the flag and never on `Err`. The first call also nudges a
            // sync pass, so this does not wait out the node's own interval.
            let state = node
                .wallet(Some(wallet))
                .wait_for_sync(Some(SYNC_WAIT_SLICE.as_millis() as u64))
                .await?;

            // A member whose import can never succeed says why. The node
            // answers immediately rather than waiting out the slice, so this
            // is not a timeout to retry: without it, such a member is a
            // healthy-looking empty database forever.
            if let Some(reason) = state.import_error.clone() {
                return Err(EngineError::ImportFailed {
                    wallet: wallet.to_owned(),
                    reason,
                });
            }
            // Placed but not imported: this member has scanned nothing of its
            // own however far its shard has scanned, and `synced` is false for
            // exactly that reason, so looping here would spin until the
            // caller's own timeout. `None` is a node predating the field, which
            // is not the same answer as `false`.
            if state.imported == Some(false) {
                return Err(EngineError::NotImported(wallet.to_owned()));
            }

            // Publish every observation, so a caller rendering progress sees
            // the scan advance rather than a bare "working" line.
            progress.store(state.height, state.chain_tip);
            if state.synced {
                // A node is in hand and the member is imported, which is the
                // one moment zkv can check its offline placement against the
                // supported answer for free.
                self.cross_check_member(wallet).await;
                debug!(
                    scanned = state.height,
                    tip = state.chain_tip,
                    enhanced_through = state.enhanced_through,
                    "wallet is caught up and its memos are complete",
                );
                return Ok(SyncOutcome {
                    scanned_height: state.height,
                    // A synced wallet is at the tip by definition, so this
                    // only differs from `height` if the node saw a new block
                    // between the scan finishing and the reply.
                    chain_tip: state.chain_tip.unwrap_or(state.height),
                });
            }
            if state.height != last_logged {
                info!(scanned = state.height, tip = state.chain_tip, "syncing",);
                last_logged = state.height;
            }
            debug!(
                scanned = state.height,
                tip = state.chain_tip,
                pending_enhancements = state.pending_enhancements,
                "waiting for the wallet to catch up",
            );
        }
    }

    /// The chain tip the node last saw, without waiting for the wallet to
    /// catch up to it.
    pub async fn chain_tip(&self) -> Result<u32, EngineError> {
        self.chain_tip_for(WALLET).await
    }

    /// [`Engine::chain_tip`] as seen through one named wallet. The tip is the
    /// node's, so every wallet it serves answers the same; the name only picks
    /// a route that exists.
    pub async fn chain_tip_for(&self, wallet: &str) -> Result<u32, EngineError> {
        let node = self.node().await?;
        Ok(node
            .wallet(Some(wallet))
            .get_blockchain_info()
            .await?
            .headers)
    }

    /// Enrol a member in this fleet node, so it starts being scanned without
    /// waiting for a restart.
    ///
    /// The node writes the manifest itself, atomically, and begins importing
    /// the account on a later sync pass. Calling this for a member whose
    /// manifest zkv already wrote is the ordinary racing case and comes back as
    /// an "already exists" error, which the caller treats as success.
    pub async fn onboard(&self, name: &str, ufvk: &str, birthday: u32) -> Result<(), EngineError> {
        let node = self.node().await?;
        node.wallet(None)
            .create_wallet(name, ufvk, birthday)
            .await?;
        Ok(())
    }

    /// Start serving a member whose manifest is already on disk (one written
    /// by another process, or by zkv before this node started).
    pub async fn load_member(&self, name: &str) -> Result<(), EngineError> {
        let node = self.node().await?;
        node.wallet(None).load_wallet(name).await?;
        Ok(())
    }

    /// Stop serving a member. The shard keeps its account and keeps scanning
    /// it: upstream has no way to remove one, which is why `zkv fleet rebuild`
    /// exists.
    pub async fn unload_member(&self, name: &str) -> Result<(), EngineError> {
        let node = self.node().await?;
        node.wallet(None).unload_wallet(name).await?;
        Ok(())
    }

    /// Where the node says a member's files and account are.
    ///
    /// This is the supported answer to the question zkv otherwise answers for
    /// itself, offline, by matching the pinned viewing key across the shard
    /// databases (`crate::fleet::locate_member`). zkv cannot simply use it:
    /// resolving a path here needs a running node, and zkv's reads are
    /// deliberately node-free, so a read that started one would give back the
    /// whole reason the fleet is cheap.
    ///
    /// What it is good for is checking the offline answer whenever a node
    /// happens to be in hand, which is what `Engine::cross_check_member`
    /// does. That turns "zkv reads shard files and hopes it agrees with the
    /// node" into a claim something actually verifies.
    pub async fn member_location(
        &self,
        name: &str,
    ) -> Result<Option<std::path::PathBuf>, EngineError> {
        let node = self.node().await?;
        match node.wallet_location(Some(name)) {
            Ok(loc) => Ok(Some(loc.engine_dir)),
            // The node does not serve this member (yet). Not a failure: the
            // caller is cross-checking, not depending on the answer.
            Err(_) => Ok(None),
        }
    }

    /// Warn if zkv's own idea of where a member lives disagrees with the
    /// node's.
    ///
    /// Best effort and never fatal: a disagreement means zkv's reads are
    /// pointed at the wrong shard, which is worth a loud line in the log, but
    /// failing the sync that discovered it would take a working database
    /// offline over a diagnostic. The reads themselves stay correct regardless,
    /// because they are account-scoped by viewing key: the worst a stale
    /// placement can do is find nothing.
    async fn cross_check_member(&self, name: &str) {
        let EngineKind::Fleet { network } = self.kind else {
            return;
        };
        let Ok(Some(theirs)) = self.member_location(name).await else {
            return;
        };
        let Ok(cfg) = crate::config::WalletConfig::read(name) else {
            return;
        };
        let ours = match crate::fleet::locate_member(&cfg, name) {
            Ok(Some((dir, _))) => dir,
            // Not placed yet on our side either; nothing to compare.
            _ => return,
        };
        if !Self::placements_agree(&ours, &theirs) {
            warn!(
                "the shared scan says {name:?} lives in {} but zkv resolved {}; \
                 zkv's placement hint is stale (network {})",
                theirs.display(),
                super::fleet::shard_engine_dir(&ours).display(),
                network.name(),
            );
        }
    }

    /// Whether zkv's placement for a member and the node's are the same place.
    ///
    /// The two answer at different levels and both are right:
    /// `wallet_location` reports the engine directory (`<shard>/lrz`), while a
    /// placement is named by its shard directory, which is what
    /// `locate_member` returns. Comparing them raw is a guaranteed mismatch,
    /// which is how the cross-check first shipped: it warned on every member
    /// sync, and a check that always fires can never report a real
    /// disagreement. Normalising to the deeper of the two is what makes it a
    /// check rather than a constant.
    fn placements_agree(ours_shard: &std::path::Path, theirs_engine: &std::path::Path) -> bool {
        super::fleet::shard_engine_dir(ours_shard) == theirs_engine
    }

    /// The members this node is serving right now, and any manifest it could
    /// not read.
    ///
    /// A skipped manifest is a member somebody meant to be scanned and that
    /// silently is not, which is indistinguishable from one still catching up
    /// unless the reason is surfaced. The node reports them; `zkv fleet status`
    /// is where they surface.
    pub async fn served_members(&self) -> Result<(Vec<String>, Vec<String>), EngineError> {
        let node = self.node().await?;
        let dir = node.wallet(None).list_wallet_dir().await?;
        Ok((
            dir.wallets.into_iter().map(|w| w.name).collect(),
            dir.warnings,
        ))
    }
}

/// Whether a node-start failure is "someone else has this database".
///
/// zecd types this now (`lock::DatadirLocked`, downcastable from the
/// `lock_datadir` error), so the answer comes from upstream rather than from
/// matching its prose. This used to read the message, which made zecd's
/// wording load-bearing for zkv's retry loop: a reworded upstream error would
/// have turned a `zkv set` run beside the GUI from "wait your turn" into an
/// outright failure, with nothing to catch it but the test that pinned the
/// string.
fn is_locked(e: &anyhow::Error) -> bool {
    zecd::lock::is_datadir_locked(e)
}

/// Whether a cooperative cancellation has been requested.
fn cancelled(cancel: &Option<CancelFlag>) -> bool {
    cancel.as_ref().is_some_and(|c| c.load(Ordering::Relaxed))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The placement cross-check compares two answers that name different
    /// levels, and normalising them is the whole of it.
    ///
    /// Written after the check shipped comparing them raw: it warned on every
    /// single member sync, so it was pure noise and could never have reported
    /// the disagreement it exists for. Only a live run showed that, because
    /// nothing offline had both answers in hand at once. This is that missing
    /// half, as a pure one.
    #[test]
    fn a_placement_agrees_with_the_nodes_when_it_names_the_same_shard() {
        let shard = std::path::Path::new("/tmp/zkv-fleet/shards/shard-0000");
        let theirs = super::super::fleet::shard_engine_dir(shard);

        assert!(
            Engine::placements_agree(shard, &theirs),
            "the same shard, named at each side's own level, must agree",
        );
        assert!(
            !Engine::placements_agree(shard, shard),
            "the node never answers with the shard directory itself; treating \
             that as agreement is what made the check vacuous",
        );
        let other = std::path::Path::new("/tmp/zkv-fleet/shards/shard-0001");
        assert!(
            !Engine::placements_agree(other, &theirs),
            "a genuinely different shard must still disagree",
        );
    }

    /// The retry loop's predicate, against a real held lock rather than a
    /// rehearsal of upstream's wording.
    ///
    /// This is the case that matters: `start_node` waits its turn only for a
    /// datadir another process holds, and fails fast for everything else. It
    /// takes the lock twice for real, so it stays honest through any upstream
    /// rewording, and would only break if the error stopped carrying the type.
    #[test]
    fn a_held_datadir_is_recognised_and_other_failures_are_not() {
        let dir = tempfile::tempdir().expect("tempdir");
        let _held = zecd::lock::lock_datadir(dir.path()).expect("first lock");

        let refused = zecd::lock::lock_datadir(dir.path())
            .expect_err("a second lock on the same datadir must be refused");
        assert!(
            is_locked(&refused),
            "a genuinely held datadir must read as locked, got: {refused:#}"
        );

        // Real failures must surface, not spin until the deadline.
        assert!(!is_locked(&anyhow::anyhow!("no usable wallets")));
        assert!(!is_locked(&anyhow::anyhow!(
            "failed to connect to the upstream server"
        )));
        assert!(!is_locked(&anyhow::anyhow!(
            "account/keys binding mismatch"
        )));
    }

    #[test]
    fn cancellation_is_observed_through_the_flag() {
        let flag: CancelFlag = Arc::new(std::sync::atomic::AtomicBool::new(false));
        assert!(!cancelled(&Some(Arc::clone(&flag))));
        flag.store(true, Ordering::Relaxed);
        assert!(cancelled(&Some(flag)));
        assert!(!cancelled(&None));
    }
}
