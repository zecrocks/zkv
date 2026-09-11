//! Fetching and parsing a single full transaction from lightwalletd.
//!
//! Shallow's [`GrpcSource`](super::source::GrpcSource) needs exactly one
//! chain interaction the compact-block stream cannot serve: pull a whole
//! transaction so its memos can be decrypted (compact outputs carry no memo).
//! These two helpers are that interaction, kept here because shallow is their
//! long-term owner: the wallet sync pipeline is being replaced by an embedded
//! zecd node, while shallow keeps its own lightwalletd transport.

use tonic::{transport::Channel, Code};
use zcash_client_backend::proto::service::{
    self, compact_tx_streamer_client::CompactTxStreamerClient, RawTransaction,
};
use zcash_primitives::transaction::{Transaction, TxId};
use zcash_protocol::consensus::{BlockHeight, BranchId, Parameters};

/// Decode a lightwalletd `RawTransaction` into a [`Transaction`] plus the
/// height it was mined at (`None` while it is still in the mempool).
///
/// The consensus branch id is picked from the mining height when known, else
/// from `chain_tip`, since an unmined transaction is validated against the
/// current branch.
pub(crate) fn parse_raw_transaction<P: Parameters>(
    params: &P,
    chain_tip: BlockHeight,
    tx: RawTransaction,
) -> anyhow::Result<(Transaction, Option<BlockHeight>)> {
    let mined_height = (tx.height > 0 && tx.height <= u64::from(u32::MAX))
        .then(|| BlockHeight::from_u32(u32::try_from(tx.height).unwrap()));
    let tx = Transaction::read(
        &tx.data[..],
        BranchId::for_height(params, mined_height.unwrap_or(chain_tip)),
    )?;
    Ok((tx, mined_height))
}

/// Fetch one transaction by txid (`GetTransaction`).
///
/// A `NotFound` status is not an error: it means the transaction is gone (a
/// reorg dropped it, or it was evicted from the mempool between the sighting
/// and the fetch), so the caller gets `None` and skips it.
pub(crate) async fn fetch_transaction<P: Parameters>(
    client: &mut CompactTxStreamerClient<Channel>,
    params: &P,
    chain_tip: BlockHeight,
    txid: TxId,
) -> anyhow::Result<Option<(Transaction, Option<BlockHeight>)>> {
    let request = service::TxFilter {
        hash: txid.as_ref().to_vec(),
        ..Default::default()
    };
    let raw_tx = match client.get_transaction(request).await {
        Ok(response) => Ok(Some(response.into_inner())),
        Err(status) => {
            if status.code() == Code::NotFound {
                Ok(None)
            } else {
                Err(status)
            }
        }
    }?;
    raw_tx
        .map(|raw_tx| parse_raw_transaction(params, chain_tip, raw_tx))
        .transpose()
}
