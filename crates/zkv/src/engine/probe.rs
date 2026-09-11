//! Chain queries that must happen *before* the wallet they are for exists.
//!
//! This is the one thing a node cannot serve, and not for want of an API: a
//! birthday has to be chosen before `create_account` runs, and a node needs
//! that account to start. zecd answers it with `chain_probe`, which takes a
//! caller-supplied [`ChainSource`] rather than a running node, so these run
//! over zkv's own dialed channel, which honours the operator's server choice
//! and SOCKS5 proxy exactly as the node's own dial does.
//!
//! Using upstream's helper rather than zkv's own tree-state fetch is what
//! makes the birthday a zkv database is created with the same one zecd's
//! `init` would have recorded: `zecd::init` builds its birthday through this
//! same function, so the two cannot drift.

use tonic::transport::Channel;
use zcash_client_backend::data_api::AccountBirthday;
use zcash_protocol::consensus::BlockHeight;
use zecd::chain::lwd::LwdSource;

use crate::network::Network;
use crate::remote::ConnectionArgs;

/// A chain tip, with the time its block was mined.
///
/// zkv's own type so callers outside the seam stay free of `zecd::` paths.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct TipStatus {
    pub height: u32,
    /// Unix epoch seconds when the tip block was mined.
    pub time: u32,
}

/// Dial `network` and wrap the channel in the chain source the probes take.
///
/// `assume_capable` is `false`: it asserts that a server which does not
/// advertise the versioned lightwallet protocol nevertheless puts transparent
/// data in its compact blocks. Nothing here reads compact blocks at all, so
/// there is no capability to assume.
async fn source(conn: &ConnectionArgs, network: Network) -> anyhow::Result<LwdSource> {
    let channel: Channel = conn.dial(network).await?;
    LwdSource::connect(channel, false).await
}

/// The chain tip and the time it was mined.
///
/// Two round trips, because the tip reply carries no timestamp: the tree state
/// is the one call on this API that does. That is upstream's reasoning for
/// keeping the time off the tip type, and it is right, since the sync path
/// refreshes a tip constantly and this runs once.
pub async fn tip_status(conn: &ConnectionArgs, network: Network) -> anyhow::Result<TipStatus> {
    let mut source = source(conn, network).await?;
    let status = zecd::chain_probe::tip_status(&mut source).await?;
    Ok(TipStatus {
        height: u32::from(status.height),
        time: status.time,
    })
}

/// Build the [`AccountBirthday`] that account creation and viewing-key import
/// take, for a wallet whose first transaction is no earlier than
/// `birthday_height`.
///
/// The anchor is the commitment tree state of the block *below*
/// `birthday_height`; upstream clamps that to height 1, which is what a short
/// regtest chain needs. `recover_until` bounds the recovery window: zkv passes
/// the tip it just validated, for every case, since a zkv database's key may
/// already have history (an imported address, a restore) and the buffer a
/// freshly generated one gets is applied to the birthday rather than here.
pub async fn account_birthday(
    conn: &ConnectionArgs,
    network: Network,
    birthday_height: u32,
    recover_until: Option<u32>,
) -> anyhow::Result<AccountBirthday> {
    let mut source = source(conn, network).await?;
    zecd::chain_probe::account_birthday(
        &mut source,
        BlockHeight::from_u32(birthday_height),
        recover_until.map(BlockHeight::from_u32),
    )
    .await
}
