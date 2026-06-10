//! The chain transport behind a [`ShallowClient`]: the three lightwalletd
//! interactions shallow needs, behind a trait so the drivers (`scan`, `find`,
//! `poll`, `verify_init`) can be tested deterministically against an in-memory
//! chain (see the mock in `mod.rs`'s tests).
//!
//! The seam sits *above* the wire format: a source reports candidate hits
//! (which transactions pay this database's receiver) and decrypted memo
//! texts, so the production impl ([`GrpcSource`]) owns trial decryption and
//! full-output decryption, and everything above it deals in plain text memos.
//!
//! [`ShallowClient`]: super::ShallowClient

use tonic::transport::Channel;
use zcash_client_backend::proto::service::{
    self, compact_tx_streamer_client::CompactTxStreamerClient,
};
use zcash_keys::keys::UnifiedFullViewingKey;
use zcash_primitives::transaction::TxId;
use zcash_protocol::{
    consensus::{self, BlockHeight},
    ShieldedProtocol,
};

use super::{decrypt, validate, ShallowError, CHUNK};
use crate::internal::sync;

/// A shallow client's view of the chain. Implemented by [`GrpcSource`]
/// (lightwalletd) in production and by an in-memory mock in tests.
///
/// Experimental, like the rest of [`crate::shallow`].
#[allow(async_fn_in_trait)] // static dispatch only; the client owns its source
pub trait ChainSource {
    /// The current chain tip height (`GetLatestBlock`).
    async fn tip(&mut self) -> Result<u32, ShallowError>;

    /// Compact-scan blocks `[lo, hi]` (inclusive): which transactions have an
    /// output that trial-decrypts to this database's viewing key? Returns
    /// `(height, txid)` candidates; compact outputs carry no memo, so a
    /// candidate still needs [`ChainSource::transaction_memos`].
    async fn candidates(&mut self, lo: u32, hi: u32) -> Result<Vec<(u32, TxId)>, ShallowError>;

    /// Fetch one transaction and decrypt its memos in the database's pool.
    /// Returns `None` when the transaction is gone (reorged away between the
    /// compact scan and this call); otherwise the mined height (falling back
    /// to `fallback_height`, where the compact scan saw it) and every text
    /// memo as `(output_index, text)`.
    async fn transaction_memos(
        &mut self,
        txid: TxId,
        fallback_height: u32,
        tip: u32,
    ) -> Result<Option<(u32, Vec<(u32, String)>)>, ShallowError>;
}

/// The production [`ChainSource`]: lightwalletd over gRPC, with per-output
/// trial decryption (compact) and full-output memo decryption (enhance) done
/// with the database's viewing key.
pub struct GrpcSource {
    pub(crate) client: CompactTxStreamerClient<Channel>,
    pub(crate) network: consensus::Network,
    pub(crate) pool: ShieldedProtocol,
    pub(crate) ufvk: UnifiedFullViewingKey,
    pub(crate) ivk: decrypt::PreparedIvk,
}

impl ChainSource for GrpcSource {
    async fn tip(&mut self) -> Result<u32, ShallowError> {
        let height = self
            .client
            .get_latest_block(service::ChainSpec::default())
            .await
            .map_err(|e| ShallowError::Connect(anyhow::anyhow!("GetLatestBlock: {e}")))?
            .get_ref()
            .height;
        u32::try_from(height)
            .map_err(|_| ShallowError::Other(anyhow::anyhow!("chain tip {height} out of range")))
    }

    async fn candidates(&mut self, lo: u32, hi: u32) -> Result<Vec<(u32, TxId)>, ShallowError> {
        let mut hits: Vec<(u32, TxId)> = Vec::new();
        for (start, end) in validate::chunks_asc(lo, hi, CHUNK) {
            let range = service::BlockRange {
                start: Some(service::BlockId {
                    height: u64::from(start),
                    ..Default::default()
                }),
                end: Some(service::BlockId {
                    height: u64::from(end),
                    ..Default::default()
                }),
                pool_types: Default::default(),
            };
            let mut stream = self
                .client
                .get_block_range(range)
                .await
                .map_err(|e| ShallowError::Connect(anyhow::anyhow!("GetBlockRange: {e}")))?
                .into_inner();
            while let Some(block) = stream
                .message()
                .await
                .map_err(|e| ShallowError::Connect(anyhow::anyhow!("block stream: {e}")))?
            {
                let height = block.height as u32;
                for txid in decrypt::scan_compact_block(&self.network, &block, &self.ivk) {
                    hits.push((height, txid));
                }
            }
        }
        Ok(hits)
    }

    async fn transaction_memos(
        &mut self,
        txid: TxId,
        fallback_height: u32,
        tip: u32,
    ) -> Result<Option<(u32, Vec<(u32, String)>)>, ShallowError> {
        let tip_bh = BlockHeight::from_u32(tip);
        let Some((tx, mined)) =
            sync::fetch_transaction(&mut self.client, &self.network, tip_bh, txid).await?
        else {
            return Ok(None);
        };
        let height = mined.map(u32::from).unwrap_or(fallback_height);
        let memos = decrypt::extract_memos(
            &self.network,
            self.pool,
            &self.ufvk,
            &tx,
            mined.or(Some(BlockHeight::from_u32(height))),
            tip_bh,
        );
        Ok(Some((height, memos)))
    }
}
