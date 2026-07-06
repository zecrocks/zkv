//! Shared test fixtures for the shallow module: a deterministic database
//! identity, a wire-memo builder, and an in-memory [`ChainSource`] mock so
//! the drivers (`scan`/`find_where`/`poll`/`verify_init`) run against a
//! scripted chain with call counting.

use std::cell::RefCell;
use std::collections::BTreeMap;
use std::rc::Rc;

use transparent::keys::NonHardenedChildIndex;
use zcash_keys::keys::UnifiedSpendingKey;
use zcash_primitives::transaction::TxId;
use zcash_protocol::consensus::NetworkType;
use zcash_protocol::ShieldedProtocol;

use super::{ChainSource, ShallowError};
use crate::protocol::{
    bind_comment, pubkey_bech32, receiver_domain, render_memo_text, sign_command, signed_payload,
    signing_domain, zkv_verifying_pubkey, Op, ZKV_TRANSPARENT_INDEX, ZKV_TRANSPARENT_SCOPE,
};

/// A test database identity: the receiver domain, the root `zkvid1…`, and
/// the root signing key (the same derivation `internal::account` uses).
pub(crate) fn fixture() -> (String, String, secp256k1::SecretKey) {
    let net = crate::network::Network::Test;
    let usk = UnifiedSpendingKey::from_seed(&net, &[0x42; 32], zip32::AccountId::ZERO)
        .expect("derive USK");
    let ufvk = usk.to_unified_full_viewing_key();
    let receiver = receiver_domain(&ufvk, ShieldedProtocol::Orchard, NetworkType::Test)
        .expect("receiver domain");
    let root_hex = pubkey_bech32(&zkv_verifying_pubkey(&ufvk).expect("root pubkey"));
    let idx = NonHardenedChildIndex::from_index(ZKV_TRANSPARENT_INDEX).expect("index");
    let sk = usk
        .transparent()
        .derive_secret_key(ZKV_TRANSPARENT_SCOPE, idx)
        .expect("derive signing key");
    (receiver, root_hex, sk)
}

/// Render a correctly-signed wire memo for any op.
pub(crate) fn signed_memo(
    receiver: &str,
    sk: &secp256k1::SecretKey,
    op: Op,
    key: &str,
    value: Option<&str>,
    seq: u64,
) -> String {
    let payload = if matches!(op, Op::Init) {
        signed_payload(&bind_comment(receiver, None), op, "", None)
    } else {
        let domain = bind_comment(&signing_domain(receiver, op, seq), None);
        signed_payload(&domain, op, key, value)
    };
    let sig = sign_command(sk, &payload);
    render_memo_text(op, key, value, seq, &hex::encode(sig))
}

/// What the mock observed: which compact ranges were requested and which
/// transactions were fetched. Shared via `Rc` so a test keeps a handle after
/// the client takes ownership of the source.
#[derive(Default)]
pub(crate) struct MockStats {
    pub candidate_ranges: Vec<(u32, u32)>,
    pub tx_fetches: Vec<TxId>,
}

/// An in-memory chain: per height, the transactions (txid + memo texts) that
/// would trial-decrypt to the database's key.
pub(crate) struct MockSource {
    pub tip: u32,
    pub blocks: BTreeMap<u32, Vec<(TxId, Vec<String>)>>,
    pub stats: Rc<RefCell<MockStats>>,
}

impl MockSource {
    pub(crate) fn new(tip: u32) -> (Self, Rc<RefCell<MockStats>>) {
        let stats = Rc::new(RefCell::new(MockStats::default()));
        (
            MockSource {
                tip,
                blocks: BTreeMap::new(),
                stats: stats.clone(),
            },
            stats,
        )
    }

    /// Script one transaction at `height` with the given memo texts. The
    /// txid is `[txid_byte; 32]`, so tests can reference it back.
    pub(crate) fn push(&mut self, height: u32, txid_byte: u8, memos: Vec<String>) {
        self.blocks
            .entry(height)
            .or_default()
            .push((TxId::from_bytes([txid_byte; 32]), memos));
    }
}

impl ChainSource for MockSource {
    async fn tip(&mut self) -> Result<u32, ShallowError> {
        Ok(self.tip)
    }

    async fn candidates(&mut self, lo: u32, hi: u32) -> Result<Vec<(u32, TxId)>, ShallowError> {
        self.stats.borrow_mut().candidate_ranges.push((lo, hi));
        Ok(self
            .blocks
            .range(lo..=hi)
            .flat_map(|(&h, txs)| txs.iter().map(move |(txid, _)| (h, *txid)))
            .collect())
    }

    async fn transaction_memos(
        &mut self,
        txid: TxId,
        _fallback_height: u32,
        _tip: u32,
    ) -> Result<Option<(u32, Vec<(u32, String)>)>, ShallowError> {
        self.stats.borrow_mut().tx_fetches.push(txid);
        for (&height, txs) in &self.blocks {
            if let Some((_, memos)) = txs.iter().find(|(t, _)| *t == txid) {
                let indexed = memos
                    .iter()
                    .enumerate()
                    .map(|(i, m)| (i as u32, m.clone()))
                    .collect();
                return Ok(Some((height, indexed)));
            }
        }
        Ok(None)
    }
}
