//! Compact-block trial decryption + full-tx memo extraction for the shallow
//! read path.
//!
//! Deliberately **not** `zcash_client_backend::scanning::scan_block`: that is
//! wallet-ingestion machinery (scanning keys, nullifier tracking,
//! note-commitment-tree accounting, block-continuity requirements). Shallow
//! only needs to know *which transactions pay this database's receiver*, so it
//! trial-decrypts each compact output directly with the UFVK's external
//! incoming viewing key. Compact outputs carry only the 52-byte truncated
//! ciphertext (no memo), so a hit yields a txid to enhance via
//! `GetTransaction`, nothing more.

use anyhow::anyhow;
use zcash_client_backend::{
    decrypt_transaction, proto::compact_formats::CompactBlock, DecryptedOutput,
};
use zcash_keys::keys::UnifiedFullViewingKey;
use zcash_note_encryption::try_compact_note_decryption;
use zcash_primitives::transaction::{components::sapling::zip212_enforcement, Transaction, TxId};
use zcash_protocol::{
    consensus::{BlockHeight, Parameters},
    memo::{Memo, MemoBytes},
    ShieldedProtocol,
};

/// The database's external incoming viewing key in its single pool, with the
/// per-pool trial-decryption precomputation done once.
pub(crate) enum PreparedIvk {
    Sapling(sapling::note_encryption::PreparedIncomingViewingKey),
    Orchard(orchard::keys::PreparedIncomingViewingKey),
}

/// Prepare the trial-decryption key for `pool` from the UFVK. Errors if the
/// UFVK does not carry that pool's component (a parsed zkv address always
/// does; its pool is inferred from which component is present).
pub(crate) fn prepare_ivk(
    ufvk: &UnifiedFullViewingKey,
    pool: ShieldedProtocol,
) -> anyhow::Result<PreparedIvk> {
    match pool {
        ShieldedProtocol::Sapling => {
            let dfvk = ufvk
                .sapling()
                .ok_or_else(|| anyhow!("UFVK has no Sapling component"))?;
            let ivk = dfvk.to_ivk(zip32::Scope::External);
            Ok(PreparedIvk::Sapling(
                sapling::note_encryption::PreparedIncomingViewingKey::new(&ivk),
            ))
        }
        ShieldedProtocol::Orchard => {
            let fvk = ufvk
                .orchard()
                .ok_or_else(|| anyhow!("UFVK has no Orchard component"))?;
            let ivk = fvk.to_ivk(zip32::Scope::External);
            Ok(PreparedIvk::Orchard(
                orchard::keys::PreparedIncomingViewingKey::new(&ivk),
            ))
        }
    }
}

/// Trial-decrypt every compact output in the database's pool within one
/// compact block. Returns the txids whose outputs decrypted (candidates for
/// the `GetTransaction` enhance step). Outputs in the *other* pool are
/// ignored, matching the read path's single-pool filter.
pub(crate) fn scan_compact_block<P: Parameters>(
    params: &P,
    block: &CompactBlock,
    ivk: &PreparedIvk,
) -> Vec<TxId> {
    let height = BlockHeight::from_u32(block.height as u32);
    let mut hits = Vec::new();
    for vtx in &block.vtx {
        let Ok(txid_bytes) = <[u8; 32]>::try_from(&vtx.txid[..]) else {
            continue;
        };
        let matched = match ivk {
            PreparedIvk::Sapling(pivk) => {
                let domain = sapling::note_encryption::SaplingDomain::new(zip212_enforcement(
                    params, height,
                ));
                vtx.outputs.iter().any(|out| {
                    sapling::note_encryption::CompactOutputDescription::try_from(out)
                        .ok()
                        .and_then(|desc| try_compact_note_decryption(&domain, pivk, &desc))
                        .is_some()
                })
            }
            PreparedIvk::Orchard(pivk) => vtx.actions.iter().any(|act| {
                orchard::note_encryption::CompactAction::try_from(act)
                    .ok()
                    .and_then(|action| {
                        let domain =
                            orchard::note_encryption::OrchardDomain::for_compact_action(&action);
                        try_compact_note_decryption(&domain, pivk, &action)
                    })
                    .is_some()
            }),
        };
        if matched {
            hits.push(TxId::from_bytes(txid_bytes));
        }
    }
    hits
}

/// Decrypt a full transaction with the UFVK and return every text memo in the
/// database's pool as `(output_index, memo_text)`.
///
/// No `TransferType` filter: self-sent writes and external writes must both
/// surface (trial decryption with the external IVK already scoped the hit to
/// this database's receiver). A batch ("sendmany") write carries several zkv
/// memos in one transaction; iterating all decrypted outputs handles that.
pub(crate) fn extract_memos<P: Parameters>(
    params: &P,
    pool: ShieldedProtocol,
    ufvk: &UnifiedFullViewingKey,
    tx: &Transaction,
    mined_height: Option<BlockHeight>,
    chain_tip: BlockHeight,
) -> Vec<(u32, String)> {
    let ufvks = std::collections::HashMap::from([(0u32, ufvk.clone())]);
    let decrypted =
        decrypt_transaction::<_, u32>(params, mined_height, Some(chain_tip), tx, &ufvks);
    let outputs: Vec<(usize, &MemoBytes)> = match pool {
        ShieldedProtocol::Sapling => decrypted
            .sapling_outputs()
            .iter()
            .map(|o: &DecryptedOutput<sapling::Note, u32>| (o.index(), o.memo()))
            .collect(),
        ShieldedProtocol::Orchard => decrypted
            .orchard_outputs()
            .iter()
            .map(|o: &DecryptedOutput<orchard::Note, u32>| (o.index(), o.memo()))
            .collect(),
    };
    outputs
        .into_iter()
        .filter_map(|(idx, memo_bytes)| match Memo::try_from(memo_bytes) {
            Ok(Memo::Text(text)) => Some((idx as u32, text.to_string())),
            _ => None,
        })
        .collect()
}
