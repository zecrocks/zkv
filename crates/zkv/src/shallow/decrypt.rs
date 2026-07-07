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
    ShieldedPool,
};

/// The database's external incoming viewing key in its single pool, with the
/// per-pool trial-decryption precomputation done once.
pub(crate) enum PreparedIvk {
    // Both the External and Internal scopes: a zkv write to the database's own
    // address is a same-account self-send, so the memo output lands under the
    // wallet's *internal* (change) IVK, not the external one (the wallet scan
    // tries both scopes; shallow must too, or it detects no candidates for the
    // database's own writes, including INIT).
    Sapling(Vec<sapling::note_encryption::PreparedIncomingViewingKey>),
    Orchard(Vec<orchard::keys::PreparedIncomingViewingKey>),
}

/// Prepare the trial-decryption keys for `pool` from the UFVK. Errors if the
/// UFVK does not carry that pool's component (a parsed zkv address always
/// does; its pool is inferred from which component is present).
pub(crate) fn prepare_ivk(
    ufvk: &UnifiedFullViewingKey,
    pool: ShieldedPool,
) -> anyhow::Result<PreparedIvk> {
    let scopes = [zip32::Scope::External, zip32::Scope::Internal];
    match pool {
        ShieldedPool::Sapling => {
            let dfvk = ufvk
                .sapling()
                .ok_or_else(|| anyhow!("UFVK has no Sapling component"))?;
            Ok(PreparedIvk::Sapling(
                scopes
                    .iter()
                    .map(|s| {
                        sapling::note_encryption::PreparedIncomingViewingKey::new(&dfvk.to_ivk(*s))
                    })
                    .collect(),
            ))
        }
        // Ironwood shares the Orchard receiver, so it trial-decrypts with the
        // Orchard IVK.
        ShieldedPool::Orchard | ShieldedPool::Ironwood => {
            let fvk = ufvk
                .orchard()
                .ok_or_else(|| anyhow!("UFVK has no Orchard component"))?;
            Ok(PreparedIvk::Orchard(
                scopes
                    .iter()
                    .map(|s| orchard::keys::PreparedIncomingViewingKey::new(&fvk.to_ivk(*s)))
                    .collect(),
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
            PreparedIvk::Sapling(pivks) => {
                let domain = sapling::note_encryption::SaplingDomain::new(zip212_enforcement(
                    params, height,
                ));
                vtx.outputs.iter().any(|out| {
                    let Ok(desc) =
                        sapling::note_encryption::CompactOutputDescription::try_from(out)
                    else {
                        return false;
                    };
                    pivks
                        .iter()
                        .any(|pivk| try_compact_note_decryption(&domain, pivk, &desc).is_some())
                })
            }
            PreparedIvk::Orchard(pivks) => {
                // Orchard (V5) and Ironwood (V6) actions live in *separate*
                // compact fields: `CompactTx.actions` carries V5 Orchard actions
                // (V2 note plaintexts, `OrchardDomain`), while `CompactTx
                // .ironwood_actions` carries V6 Ironwood actions (V3 note
                // plaintexts, `IronwoodDomain`). They share the Orchard receiver
                // and IVK and the `CompactOrchardAction` shape, only the field
                // and note-plaintext version differ. This build's own writes on
                // an Ironwood chain land in `ironwood_actions`, so scanning only
                // `actions` (the earlier bug) finds nothing.
                let orchard_hit = vtx.actions.iter().any(|act| {
                    let Ok(action) = orchard::note_encryption::CompactAction::try_from(act) else {
                        return false;
                    };
                    let domain =
                        orchard::note_encryption::OrchardDomain::for_compact_action(&action);
                    pivks
                        .iter()
                        .any(|pivk| try_compact_note_decryption(&domain, pivk, &action).is_some())
                });
                let ironwood_hit = vtx.ironwood_actions.iter().any(|act| {
                    let Ok(action) = orchard::note_encryption::CompactAction::try_from(act) else {
                        return false;
                    };
                    let domain =
                        orchard::note_encryption::IronwoodDomain::for_compact_action(&action);
                    pivks
                        .iter()
                        .any(|pivk| try_compact_note_decryption(&domain, pivk, &action).is_some())
                });
                orchard_hit || ironwood_hit
            }
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
    pool: ShieldedPool,
    ufvk: &UnifiedFullViewingKey,
    tx: &Transaction,
    mined_height: Option<BlockHeight>,
    chain_tip: BlockHeight,
) -> Vec<(u32, String)> {
    let ufvks = std::collections::HashMap::from([(0u32, ufvk.clone())]);
    let decrypted =
        decrypt_transaction::<_, u32>(params, mined_height, Some(chain_tip), tx, &ufvks);
    let outputs: Vec<(usize, &MemoBytes)> = match pool {
        ShieldedPool::Sapling => decrypted
            .sapling_outputs()
            .iter()
            .map(|o: &DecryptedOutput<sapling::Note, u32>| (o.index(), o.memo()))
            .collect(),
        // Ironwood shares the Orchard receiver but is a distinct value pool with
        // its own decrypted-output accessor (`decrypt_transaction` splits V2
        // Orchard from V3 Ironwood notes). A V6 write lands in `ironwood_outputs`,
        // so both must be scanned or this build's own memos on an Ironwood chain
        // vanish. The element type is identical, so they chain directly.
        ShieldedPool::Orchard | ShieldedPool::Ironwood => decrypted
            .orchard_outputs()
            .iter()
            .chain(decrypted.ironwood_outputs())
            .map(
                |o: &DecryptedOutput<(orchard::Note, orchard::ValuePool), u32>| {
                    (o.index(), o.memo())
                },
            )
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
