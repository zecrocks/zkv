//! Load this database's *funding* ledger: the ZEC transfers in and out of the
//! wallet (deposits and withdrawals), plus the database's own zkv writes shown
//! as bare-fee `ZkvOperation` rows. Backs the GUI's Funding tab via
//! [`crate::db::Database::funding`].
//!
//! Unlike the read/history path there is no snapshot here; funding is a direct
//! read over the wallet DB's `v_transactions` / `v_tx_outputs` views:
//! - **Signed value** comes from `v_transactions.account_balance_delta`
//!   (`+` received, `−` sent, fee included on sends). On the *outgoing* side we
//!   re-add the fee so the displayed amount is the *value transferred*
//!   (external amount), fee excluded; on the *incoming* side the delta is
//!   already the value received (we paid no fee), so the fee is never re-added
//!   there even when the row happens to report one. A transaction that nets to
//!   a bare fee (a zkv write or an
//!   internal shield) therefore comes out as `0`. A non-zkv shuffle netting to
//!   `0` is dropped *unless* it carries a deliberate self-send leg (a non-change
//!   output back to one of our own accounts), in which case it surfaces as a
//!   [`FundingDirection::SelfTransfer`] whose `amount` is the net effect (the
//!   fee, as librustzcash reports it) with the gross self-sent value kept on
//!   the side in [`FundingTx::self_sent`]. An *outbound zkv write* netting to a
//!   bare fee instead surfaces as a [`FundingDirection::ZkvOperation`] whose
//!   `amount` is that fee, so the ledger accounts for what a write costs.
//! - **Recipients** for sends come from `v_tx_outputs.to_address` on the
//!   external output legs (`to_account_uuid IS NULL`, not change).
//! - **Memos** are decoded exactly as the read path decodes them. A transaction
//!   carrying a zkv-looking memo (valid *or* invalid, per
//!   [`crate::protocol::looks_like_zkv`]) is flagged [`FundingTx::is_zkv`]: if
//!   it moved external value (an inbound tip/deposit or an outbound payment) it
//!   shows as a normal Received/Sent row, otherwise as a `ZkvOperation`.
//!
//! Performance: the output scan covers every output addressed to/from the
//! account, including the (excluded) zkv writes, so this is `O(writes)` per
//! call. The Funding tab loads on demand (like History), so this is acceptable;
//! a future optimization could maintain a funding-specific snapshot.

use std::collections::HashMap;

use rusqlite::{named_params, Connection};
use zcash_client_backend::data_api::WalletRead;
use zcash_protocol::memo::{Memo, MemoBytes};

use crate::{
    config::WalletConfig,
    data::{get_db_paths, open_wallet_db},
    internal::{account::account_keys, state::txid_hex},
    protocol::looks_like_zkv,
};

/// Confirmations before one of *our own* (trusted) outputs counts as
/// confirmed, matching the `trusted` leg of the wallet's
/// `ConfirmationsPolicy::default()` (ZIP-315). Applies to sends and
/// self-transfers.
const TRUSTED_CONFIRMATIONS: u32 = 3;

/// Confirmations before an *externally received* (untrusted) output counts as
/// confirmed, matching the `untrusted` leg of the wallet's
/// `ConfirmationsPolicy::default()` (ZIP-315). This is why a freshly received
/// deposit reads as "confirming" in the wallet balance until it is 10 blocks
/// deep; the Funding tab now agrees.
const UNTRUSTED_CONFIRMATIONS: u32 = 10;

/// Direction of a funding transaction relative to this wallet.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum FundingDirection {
    /// Net value entered the wallet (a deposit).
    Received,
    /// Net value left the wallet (a withdrawal / payment).
    Sent,
    /// Value sent to one of the wallet's own addresses; the funds never left
    /// (the tx nets to a bare fee), but it was a deliberate transfer, not a
    /// zkv write or internal shuffle. The `amount` is the net effect (the fee);
    /// the gross value routed back to us is in [`FundingTx::self_sent`].
    SelfTransfer,
    /// An outbound zkv operation (a SET/DEL/INIT/management write) that moved no
    /// external value: the tx nets to just the ZIP-317 fee. Surfaced so the
    /// funding ledger accounts for the fee a zkv write costs; the `amount` is
    /// the fee, and the detail pane links to the write in History.
    ZkvOperation,
}

/// One non-zkv ZEC transfer in or out of the database's wallet.
#[derive(Clone, Debug)]
pub struct FundingTx {
    /// Transaction id in conventional display order (big-endian hex).
    pub txid: String,
    /// Mined height, or `None` while still in the mempool.
    pub height: Option<u32>,
    /// Block timestamp (unix seconds), or `None` until mined.
    pub timestamp: Option<u32>,
    /// Whether value entered or left the wallet.
    pub direction: FundingDirection,
    /// Absolute value transferred in zatoshi, **fee excluded** (a send shows
    /// the external amount, not amount + fee). For a [`FundingDirection::
    /// SelfTransfer`] or [`FundingDirection::ZkvOperation`] this is the *net*
    /// effect (the fee), matching librustzcash's `account_balance_delta`.
    pub amount: u64,
    /// For a [`FundingDirection::SelfTransfer`], the gross value (zatoshi)
    /// routed back to one of our own addresses; `None` otherwise.
    pub self_sent: Option<u64>,
    /// Fee paid in zatoshi, if this wallet built the tx (`None` for
    /// received-only transactions).
    pub fee: Option<u64>,
    /// First non-empty, non-zkv text memo on the transaction's outputs.
    pub memo: Option<String>,
    /// External recipient address(es) for a send (`to_account_uuid IS NULL`).
    /// Empty for receives.
    pub recipients: Vec<String>,
    /// Whether this transaction carries a zkv-format memo (a zkv operation,
    /// valid or not). Lets the Funding detail link to the write in History.
    /// Always set for a [`FundingDirection::ZkvOperation`]; also true for an
    /// inbound deposit or outbound payment that happens to ride a zkv memo.
    pub is_zkv: bool,
    /// Whether the transaction is still in the mempool (unmined).
    pub pending: bool,
    /// On-chain confirmations (`tip − height + 1`); `0` while in the mempool.
    pub confirmations: u32,
    /// Confirmations required before this transaction counts as confirmed: the
    /// ZIP-315 depth for its direction (10 for an external receive, 3 for our
    /// own send/self-transfer), matching the wallet's spendability policy.
    pub required: u32,
    /// Whether the transaction has reached [`FundingTx::required`] confirmations
    /// (mined and `confirmations >= required`). Until then it is still
    /// confirming, exactly as the wallet balance reports it.
    pub confirmed: bool,
}

/// A page of the funding ledger, newest-first with mempool txs pinned on top.
#[derive(Clone, Debug)]
pub struct FundingResult {
    pub entries: Vec<FundingTx>,
    /// Total matching transactions across all pages (drives pagination).
    pub total: u64,
    pub offset: u32,
    pub limit: Option<u32>,
}

/// Per-transaction aggregate built from this tx's `v_tx_outputs` rows.
#[derive(Default)]
struct OutputsAgg {
    /// Any output carries a zkv-looking memo ⇒ the whole tx is zkv traffic.
    is_zkv: bool,
    /// First non-empty, non-zkv text memo seen on the tx.
    memo: Option<String>,
    /// External recipient addresses (sends only).
    recipients: Vec<String>,
    /// Total value (zatoshi) of non-change outputs addressed back to one of our
    /// own accounts: a deliberate self-send leg. Lets a fee-only-netting tx be
    /// surfaced as a self-transfer instead of dropped like a zkv write.
    self_value: u64,
}

/// Load one page of the database's funding ledger. `limit = None` returns every
/// matching transaction; `offset` skips the newest N. Newest-first, mempool
/// pinned on top.
pub fn load_funding(
    db_name: &str,
    limit: Option<u32>,
    offset: u32,
) -> anyhow::Result<FundingResult> {
    let cfg = WalletConfig::read(db_name)?;
    let keys = account_keys(&cfg, db_name)?;
    let account_uuid_bytes = keys.account_uuid_bytes;

    let (_, db_data_path) = get_db_paths(db_name)?;
    let tip: u32 = {
        let db_data = open_wallet_db(&db_data_path, cfg.network)?;
        db_data.chain_height()?.map(u32::from).unwrap_or(0)
    };

    let conn = Connection::open(&db_data_path)?;

    // ---- Per-tx outputs: zkv detection + display memo + send recipients ----
    let mut agg: HashMap<Vec<u8>, OutputsAgg> = HashMap::new();
    {
        let mut stmt = conn.prepare(
            "SELECT v.txid, v.memo, v.to_address, v.to_account_uuid, v.is_change, v.value
             FROM v_tx_outputs v
             WHERE v.to_account_uuid = :account_uuid
                OR v.from_account_uuid = :account_uuid",
        )?;
        let rows = stmt.query_map(
            named_params! { ":account_uuid": account_uuid_bytes.as_slice() },
            |row| {
                let txid: Option<Vec<u8>> = row.get(0)?;
                let memo: Option<Vec<u8>> = row.get(1)?;
                let to_address: Option<String> = row.get(2)?;
                let to_account_uuid: Option<Vec<u8>> = row.get(3)?;
                let is_change: Option<i64> = row.get(4)?;
                let value: Option<i64> = row.get(5)?;
                Ok((txid, memo, to_address, to_account_uuid, is_change, value))
            },
        )?;
        for r in rows {
            let (txid, memo, to_address, to_account_uuid, is_change, value) = r?;
            let Some(txid) = txid.filter(|t| !t.is_empty()) else {
                continue;
            };
            let entry = agg.entry(txid).or_default();
            // Memo decode mirrors the read path: only `Memo::Text` is a memo.
            if let Some(text) = memo.as_deref().and_then(decode_text_memo) {
                if looks_like_zkv(&text) {
                    entry.is_zkv = true;
                } else if entry.memo.is_none() && !text.is_empty() {
                    entry.memo = Some(text);
                }
            }
            let is_change = is_change.unwrap_or(0) != 0;
            if to_account_uuid.is_none() && !is_change {
                // A send leg: an output addressed outside this wallet's accounts
                // (`to_account_uuid` NULL) that isn't change.
                if let Some(addr) = to_address.filter(|a| !a.is_empty()) {
                    if !entry.recipients.contains(&addr) {
                        entry.recipients.push(addr);
                    }
                }
            } else if to_account_uuid.is_some() && !is_change {
                // A self-send leg: a deliberate, non-change output back to one
                // of our own accounts. Receives land here too, but those net
                // positive and never reach the self-transfer branch below.
                entry.self_value = entry
                    .self_value
                    .saturating_add(value.unwrap_or(0).max(0) as u64);
            }
        }
    }

    // ---- Per-tx value / fee / time from v_transactions (one row per tx) ----
    let mut entries: Vec<FundingTx> = Vec::new();
    {
        let mut stmt = conn.prepare(
            "SELECT t.txid, t.mined_height, t.block_time, t.account_balance_delta, t.fee_paid
             FROM v_transactions t
             WHERE t.account_uuid = :account_uuid
               AND (t.mined_height IS NOT NULL
                    OR t.expiry_height IS NULL
                    OR t.expiry_height = 0
                    OR t.expiry_height >= :tip)",
        )?;
        let rows = stmt.query_map(
            named_params! {
                ":account_uuid": account_uuid_bytes.as_slice(),
                ":tip": tip,
            },
            |row| {
                let txid: Option<Vec<u8>> = row.get(0)?;
                let mined_height: Option<i64> = row.get(1)?;
                let block_time: Option<i64> = row.get(2)?;
                let delta: Option<i64> = row.get(3)?;
                let fee: Option<i64> = row.get(4)?;
                Ok((txid, mined_height, block_time, delta, fee))
            },
        )?;
        for r in rows {
            let (txid, mined_height, block_time, delta, fee) = r?;
            let Some(txid_bytes) = txid.filter(|t| !t.is_empty()) else {
                continue;
            };
            let outputs = agg.get(&txid_bytes);
            let is_zkv = outputs.is_some_and(|o| o.is_zkv);

            let delta = delta.unwrap_or(0);
            let raw_fee = fee.filter(|f| *f >= 0).map(|f| f as u64);
            // `account_balance_delta` is the net change to our balance: `+value`
            // on a receive (we paid no fee) and `−(value + fee)` on a send or
            // self-send (we paid the fee). The fee is re-added only on the
            // outgoing side, so the displayed amount is the value transferred,
            // fee excluded: send delta `−(value + fee)` + fee ⇒ `−value`. A
            // receive's delta is already the value received and must not absorb
            // a fee, even though some wallet rows report `fee_paid` on the
            // incoming transaction too (that fee was paid by the sender, not us).
            let outgoing = delta < 0;
            let signed = if outgoing {
                delta + raw_fee.map_or(0, |f| f as i64)
            } else {
                delta
            };
            // The fee belongs to us only when we built the tx (outgoing); a
            // received deposit never carries one, regardless of the row.
            let fee = if outgoing { raw_fee } else { None };
            let self_value = outputs.map_or(0, |o| o.self_value);
            // Classification, in precedence order:
            // - net inbound  ⇒ a deposit (Received), even if it rides a zkv memo
            //   (a tip broadcast with a write): "a nonzero inbound output shows".
            // - net outbound ⇒ a payment (Sent), even if it rides a zkv memo
            //   (an outbound tx that isn't a bare-fee write always shows).
            // - a deliberate self-send leg with no net value ⇒ SelfTransfer.
            // - a bare-fee outbound zkv write ⇒ ZkvOperation (the fee it cost).
            // - anything else (a non-zkv internal shuffle netting to 0) ⇒ dropped.
            // A self-send's value returns to us, so librustzcash's net balance
            // delta is just the fee; we report that net as the `amount` and stash
            // the gross value routed back to us in `self_sent`.
            let (direction, amount, self_sent) = if signed > 0 {
                (FundingDirection::Received, signed.unsigned_abs(), None)
            } else if signed < 0 {
                (FundingDirection::Sent, signed.unsigned_abs(), None)
            } else if self_value > 0 {
                (
                    FundingDirection::SelfTransfer,
                    fee.unwrap_or(0),
                    Some(self_value),
                )
            } else if is_zkv && outgoing {
                (FundingDirection::ZkvOperation, fee.unwrap_or(0), None)
            } else {
                continue;
            };

            let is_mempool = mined_height.is_none_or(|h| h <= 0);
            let height = mined_height
                .filter(|h| *h > 0)
                .map(|h| u32::try_from(h).unwrap_or(u32::MAX));
            let timestamp = block_time
                .filter(|t| *t > 0)
                .map(|t| u32::try_from(t).unwrap_or(u32::MAX));
            // Confirmations count the mined block itself (tip == height ⇒ 1).
            // "Confirmed" tracks the wallet's ZIP-315 spendability depth for
            // this output's trust class, so the Funding tab agrees with the
            // balance: an external receive is "confirming" until 10 deep, our
            // own send/self-transfer until 3.
            let confirmations = if is_mempool {
                0
            } else {
                tip.saturating_sub(height.unwrap_or(0)).saturating_add(1)
            };
            let required = match direction {
                FundingDirection::Received => UNTRUSTED_CONFIRMATIONS,
                FundingDirection::Sent
                | FundingDirection::SelfTransfer
                | FundingDirection::ZkvOperation => TRUSTED_CONFIRMATIONS,
            };
            let confirmed = !is_mempool && confirmations >= required;
            let (memo, recipients) = outputs
                .map(|o| (o.memo.clone(), o.recipients.clone()))
                .unwrap_or_default();

            entries.push(FundingTx {
                txid: txid_hex(&txid_bytes),
                height,
                timestamp,
                direction,
                amount,
                self_sent,
                fee,
                memo,
                recipients,
                is_zkv,
                pending: is_mempool,
                confirmations,
                required,
                confirmed,
            });
        }
    }

    // Newest-first, mempool (pending) pinned on top.
    entries.sort_by(|a, b| {
        a.pending
            .cmp(&b.pending)
            .reverse()
            .then(
                b.height
                    .unwrap_or(u32::MAX)
                    .cmp(&a.height.unwrap_or(u32::MAX)),
            )
            .then(b.txid.cmp(&a.txid))
    });

    let total = entries.len() as u64;
    let off = offset as usize;
    let paged: Vec<FundingTx> = match limit {
        None => entries.into_iter().skip(off).collect(),
        Some(lim) => entries.into_iter().skip(off).take(lim as usize).collect(),
    };

    Ok(FundingResult {
        entries: paged,
        total,
        offset,
        limit,
    })
}

/// Decode raw memo bytes to text, returning `Some` only for `Memo::Text`
/// (mirrors the decode in [`crate::internal::state`]).
fn decode_text_memo(bytes: &[u8]) -> Option<String> {
    let mb = MemoBytes::from_bytes(bytes).ok()?;
    match Memo::try_from(mb).ok()? {
        Memo::Text(t) => Some(t.to_string()),
        _ => None,
    }
}
