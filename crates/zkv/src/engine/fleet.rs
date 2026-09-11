//! The shared per-network node behind the fleet, and its lifetime.
//!
//! A member of the shared scan does not get an engine of its own: every member
//! of a network is served by **one** node, so they share the upstream
//! connection, the shard databases and the single pass over the blocks that
//! made the whole arrangement worth building. This module owns that node.
//!
//! # Why one per process, not one per handle
//!
//! Not just for the sharing. zecd locks its datadir, `flock` is not reentrant
//! across handles, and the fleet's datadir is one directory for every member.
//! Two `Engine`s over it inside one process would therefore refuse each other,
//! and the refusal would look exactly like another process holding the
//! database. That is the shape of the GUI deadlock the regtest GUI test exists
//! to catch, so the node is a process-wide singleton and every member's handle
//! is a clone of the same `Arc`.
//!
//! # Lifetime
//!
//! Started lazily on the first operation that needs the chain and kept up: a
//! continuous scan is the point, and restarting it every auto-sync cycle would
//! give back the cost the fleet exists to remove. [`shutdown_all`] stops them,
//! which the GUI calls when it exits and when every database is paused.

use std::collections::HashMap;
use std::sync::{Arc, Mutex, OnceLock};

use crate::network::Network;
use crate::remote::ConnectionArgs;

use super::{Engine, EngineError, EngineKind};

/// The running fleet nodes, one per network, shared by every member.
fn registry() -> &'static Mutex<HashMap<Network, Arc<Engine>>> {
    static REGISTRY: OnceLock<Mutex<HashMap<Network, Arc<Engine>>>> = OnceLock::new();
    REGISTRY.get_or_init(Default::default)
}

/// The engine for a network's shared scan, built on first use.
///
/// Cheap and offline after the first call: this resolves a configuration and
/// hands back a handle. Nothing dials until an operation needs the chain, the
/// same as an own-node engine.
///
/// The connection arguments of the *first* caller win. That is deliberate: the
/// node is one process-wide scan, so a second caller asking for a different
/// upstream cannot be honoured without tearing down everyone else's sync, and
/// silently reconnecting would be worse than ignoring it. Every zkv binary
/// passes the same connection to every database anyway.
pub fn shared(network: Network, conn: &ConnectionArgs) -> Result<Arc<Engine>, EngineError> {
    let mut map = registry().lock().expect("fleet registry poisoned");
    if let Some(engine) = map.get(&network) {
        return Ok(Arc::clone(engine));
    }
    let engine = Arc::new(Engine::fleet(network, conn)?);
    map.insert(network, Arc::clone(&engine));
    Ok(engine)
}

/// Whether a network's fleet node has been built in this process.
///
/// "Built", not "scanning": the node itself starts lazily. What the callers
/// actually want to know is whether there is a handle to reach, so a member
/// can be enrolled through it rather than by writing a manifest for a later
/// pass to notice.
pub fn is_running(network: Network) -> bool {
    registry()
        .lock()
        .expect("fleet registry poisoned")
        .contains_key(&network)
}

/// Stop every fleet node this process started, releasing their datadir locks.
///
/// Waits for each, rather than dropping the handles: a lock left held by a
/// process that has finished with it is a lock the next one waits out for
/// nothing.
pub async fn shutdown_all() {
    let engines: Vec<Arc<Engine>> = {
        let mut map = registry().lock().expect("fleet registry poisoned");
        map.drain().map(|(_, e)| e).collect()
    };
    for engine in engines {
        engine.shutdown().await;
    }
}

/// Write a shared-scan manifest through the node's own writer.
///
/// Provisioning a member is something zkv does with **no node running**: it
/// happens on `Database::open`, offline, before anything dials. Upstream made
/// its writer public for exactly that case, which is worth using rather than
/// re-implementing: the write is temp + fsync + rename, and the difference
/// between doing that and not is a torn file holding a wallet's only copy of
/// its viewing key. Going through upstream's also means the manifest *format*
/// cannot drift from what the node reads, on a surface upstream still calls
/// experimental.
///
/// This lives here rather than in `crate::fleet` only because of the seam rule:
/// this module may name a `zecd::` path and that one may not.
/// Where a shard keeps its librustzcash files.
///
/// **Not the shard directory itself**, and not the `<db>/zec/lrz` a per-database
/// node uses either: upstream's `shard_engine_dir` joins only the coin's engine
/// segment, so a shard's wallet database is `<shard>/lrz/data.sqlite`. Deriving
/// that by hand is exactly the mistake that made every fleet read report a
/// member as never imported, so it goes through upstream's own function, which
/// is also what the shard actor is configured with.
///
/// Here rather than in `crate::fleet` for the seam rule: this module may name a
/// `zecd::` path and that one may not.
pub fn shard_engine_dir(shard_dir: &std::path::Path) -> std::path::PathBuf {
    zecd::config::shard_engine_dir(shard_dir, zecd::coin::Coin::Zcash)
}

pub fn write_manifest(
    manifest_dir: &std::path::Path,
    name: &str,
    ufvk: &str,
    birthday: u32,
) -> anyhow::Result<()> {
    zecd::fleet::write_manifest(
        manifest_dir,
        &zecd::wallet::shard::ShardMember {
            name: name.to_owned(),
            ufvk: ufvk.to_owned(),
            birthday: zcash_protocol::consensus::BlockHeight::from_u32(birthday),
        },
    )
}

impl Engine {
    /// The engine for a network's shared scan. Use [`shared`] instead: two of
    /// these in one process would refuse each other's datadir lock.
    fn fleet(network: Network, conn: &ConnectionArgs) -> Result<Engine, EngineError> {
        let dir = crate::fleet::fleet_dir(network).map_err(EngineError::Other)?;
        std::fs::create_dir_all(&dir).map_err(|e| {
            EngineError::Other(anyhow::anyhow!(
                "creating the shared-scan directory {}: {e}",
                dir.display()
            ))
        })?;
        let app = super::config::fleet_app_config(&dir, network, conn)?;
        Ok(Engine {
            // Named for what it is, since this name only ever appears in a log
            // line or an error: there is no zkv database called this.
            db_name: format!("shared scan ({})", network.name()),
            db_dir: dir,
            // A fleet node holds no single database's config. The one field
            // its own code reads off `cfg` is the network, which
            // `EngineKind::Fleet` carries, so this is a placeholder that the
            // fleet paths never consult.
            cfg: crate::config::WalletConfig::placeholder(network),
            kind: EngineKind::Fleet { network },
            app,
            node: tokio::sync::Mutex::new(None),
            busy_wait: Some(super::DEFAULT_BUSY_WAIT),
            progress: Default::default(),
        })
    }
}

/// A handle to whichever engine serves a database.
///
/// An own-node database owns its engine; a member of the shared scan borrows
/// the process-wide one, which is a clone of the same `Arc` every other member
/// holds. Callers do not care which, so this dereferences to [`Engine`] and
/// the difference shows up only where it has to: shutting down (a member must
/// not stop a node other databases are using) and addressing a wallet by name.
pub enum EngineRef {
    /// This database's own node. Behind an `Arc` like the shared one purely so
    /// the two variants are the same size; nothing else holds this handle.
    Own(Arc<Engine>),
    Shared(Arc<Engine>),
}

impl std::ops::Deref for EngineRef {
    type Target = Engine;

    fn deref(&self) -> &Engine {
        match self {
            EngineRef::Own(e) => e,
            EngineRef::Shared(e) => e,
        }
    }
}

impl EngineRef {
    /// The engine that serves `db_name`, read from its `keys.toml`.
    ///
    /// The counterpart of [`Engine::open`] for a caller that does not already
    /// know whether the database has a wallet engine of its own. Every path
    /// that opens one for a *named* database should come through here:
    /// `Engine::open` always builds a per-database node, which for a member of
    /// the shared scan means a node whose datadir holds no wallet files.
    pub fn open(db_name: &str, conn: &ConnectionArgs) -> Result<EngineRef, EngineError> {
        let cfg = crate::config::WalletConfig::read(db_name).map_err(EngineError::Other)?;
        engine_for(db_name, cfg, conn, Some(super::DEFAULT_BUSY_WAIT))
    }

    /// Stop this engine's node, if stopping it is this caller's to do.
    ///
    /// A no-op on the shared node: other databases are using it, and it is
    /// stopped through [`shutdown_all`] when the process is done with all of
    /// them.
    pub async fn shutdown_if_owned(&self) {
        if let EngineRef::Own(e) = self {
            e.shutdown().await;
        }
    }
}

/// The engine that serves `db_name`, which is the shared one when that database
/// is a member of its network's fleet.
pub fn engine_for(
    db_name: &str,
    cfg: crate::config::WalletConfig,
    conn: &ConnectionArgs,
    busy_wait: Option<std::time::Duration>,
) -> Result<EngineRef, EngineError> {
    if cfg.engine.is_fleet_member() {
        // The busy wait is not applied to the shared node: its lifetime is the
        // process's, so the first caller's setting would silently become
        // everybody's. A caller that wants to skip a busy fleet checks with
        // `EngineError::Busy` when the node actually starts.
        return Ok(EngineRef::Shared(shared(cfg.network, conn)?));
    }
    Ok(EngineRef::Own(Arc::new(
        Engine::from_config(db_name, cfg, conn)?.with_busy_wait(busy_wait),
    )))
}
