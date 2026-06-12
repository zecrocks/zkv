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
//!   `0` is dropped *unless* it carries a deliberate self-send leg (an output
//!   back to one of our own accounts at an *external-scope* address — see
//!   [`OutputsAgg::self_value`] for why key scope, not the view's `is_change`
//!   flag, is the discriminator), in which case it surfaces as a
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

/// `v_tx_outputs.recipient_key_scope` encoding for the ZIP-32 *external* scope
/// (1 = internal/change, 2 = ephemeral ZIP-320 legs). An output received by
/// our own account at an external-scope address is a deliberate self-send.
const KEY_SCOPE_EXTERNAL: i64 = 0;

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
    /// Total value (zatoshi) of outputs addressed back to one of our own
    /// accounts at an **external-scope** address: a deliberate self-send leg.
    /// Lets a fee-only-netting tx be surfaced as a self-transfer instead of
    /// dropped like a zkv write.
    ///
    /// Keyed on `recipient_key_scope` (0 = external), **not** on the view's
    /// `is_change` flag: librustzcash's scanner marks *every* note an account
    /// receives in a transaction that account also spent in as change —
    /// explicitly including "notes sent from one account to itself"
    /// (`zcash_client_backend::scanning`) — so once a self-send is scanned its
    /// returning output reads `is_change = 1` and an `is_change`-based test
    /// drops the whole transaction from the ledger. Real change lands at
    /// internal-scope (1) addresses and ephemeral ZIP-320 legs at scope 2, so
    /// external scope alone cleanly identifies the deliberate leg.
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
    let entries = funding_entries(&conn, &account_uuid_bytes, tip)?;

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

/// The query + classification core of [`load_funding`], over an already-open
/// wallet-db connection. Split out so the classification can be exercised
/// against a stub `v_transactions`/`v_tx_outputs` schema in tests. Returns
/// every matching transaction, newest-first with mempool pinned on top.
fn funding_entries(
    conn: &Connection,
    account_uuid_bytes: &[u8],
    tip: u32,
) -> anyhow::Result<Vec<FundingTx>> {
    // ---- Per-tx outputs: zkv detection + display memo + send recipients ----
    let mut agg: HashMap<Vec<u8>, OutputsAgg> = HashMap::new();
    {
        let mut stmt = conn.prepare(
            "SELECT v.txid, v.memo, v.to_address, v.to_account_uuid, v.is_change, v.value,
                    v.recipient_key_scope
             FROM v_tx_outputs v
             WHERE v.to_account_uuid = :account_uuid
                OR v.from_account_uuid = :account_uuid",
        )?;
        let rows = stmt.query_map(
            named_params! { ":account_uuid": account_uuid_bytes },
            |row| {
                let txid: Option<Vec<u8>> = row.get(0)?;
                let memo: Option<Vec<u8>> = row.get(1)?;
                let to_address: Option<String> = row.get(2)?;
                let to_account_uuid: Option<Vec<u8>> = row.get(3)?;
                let is_change: Option<i64> = row.get(4)?;
                let value: Option<i64> = row.get(5)?;
                let key_scope: Option<i64> = row.get(6)?;
                Ok((
                    txid,
                    memo,
                    to_address,
                    to_account_uuid,
                    is_change,
                    value,
                    key_scope,
                ))
            },
        )?;
        for r in rows {
            let (txid, memo, to_address, to_account_uuid, is_change, value, key_scope) = r?;
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
            } else if to_account_uuid.as_deref() == Some(account_uuid_bytes)
                && key_scope == Some(KEY_SCOPE_EXTERNAL)
            {
                // A self-send leg: an output back to one of our own accounts at
                // an external-scope address (the deposit UA / a taddr), i.e. a
                // destination someone deliberately addressed. NOT gated on the
                // view's `is_change`: the scanner marks every note we receive
                // in a tx we also spent in as change — self-sends included —
                // so an `is_change` test would drop exactly the rows this
                // exists to surface (see `OutputsAgg::self_value`). Change
                // proper lands at internal scope (1) and never matches.
                // External receives land here too, but those net positive and
                // never reach the self-transfer branch below.
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
                ":account_uuid": account_uuid_bytes,
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

    Ok(entries)
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

#[cfg(test)]
mod tests {
    use super::*;

    const ACCOUNT: [u8; 16] = [7u8; 16];
    const OTHER_ACCOUNT: [u8; 16] = [9u8; 16];
    const TIP: u32 = 1000;
    const FEE: i64 = 10_000;

    /// Minimal stand-ins for the wallet's `v_transactions` / `v_tx_outputs`
    /// views: only the columns `funding_entries` reads (the pattern of
    /// `internal::sync`'s stub). Lets us exercise the funding classification
    /// without the full zcash_client_sqlite schema.
    fn stub_conn() -> Connection {
        let conn = Connection::open_in_memory().unwrap();
        conn.execute_batch(
            "CREATE TABLE v_transactions (
                 txid BLOB, account_uuid BLOB, mined_height INTEGER,
                 block_time INTEGER, account_balance_delta INTEGER,
                 fee_paid INTEGER, expiry_height INTEGER
             );
             CREATE TABLE v_tx_outputs (
                 txid BLOB, memo BLOB, to_address TEXT, to_account_uuid BLOB,
                 from_account_uuid BLOB, is_change INTEGER, value INTEGER,
                 recipient_key_scope INTEGER
             );",
        )
        .unwrap();
        conn
    }

    fn insert_tx(conn: &Connection, txid: u8, delta: i64, fee: Option<i64>) {
        conn.execute(
            "INSERT INTO v_transactions
                 (txid, account_uuid, mined_height, block_time,
                  account_balance_delta, fee_paid, expiry_height)
             VALUES (?1, ?2, 900, 1700000000, ?3, ?4, NULL)",
            rusqlite::params![vec![txid; 32], ACCOUNT.as_slice(), delta, fee],
        )
        .unwrap();
    }

    #[allow(clippy::too_many_arguments)]
    fn insert_output(
        conn: &Connection,
        txid: u8,
        to_address: Option<&str>,
        to_account: Option<&[u8]>,
        is_change: bool,
        value: i64,
        key_scope: Option<i64>,
        memo: Option<&[u8]>,
    ) {
        conn.execute(
            "INSERT INTO v_tx_outputs
                 (txid, memo, to_address, to_account_uuid, from_account_uuid,
                  is_change, value, recipient_key_scope)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)",
            rusqlite::params![
                vec![txid; 32],
                memo,
                to_address,
                to_account,
                ACCOUNT.as_slice(),
                is_change,
                value,
                key_scope,
            ],
        )
        .unwrap();
    }

    // The user-reported regression: a 10 ZEC send from the wallet back to its
    // own deposit UA. Once the block is scanned, librustzcash marks the
    // returning note as change (`is_change = 1`; the scanner flags every note
    // an account receives in a tx it also spent in, self-sends explicitly
    // included), so an `is_change`-based self-send test made the whole
    // transaction vanish from the Funding ledger — fee and all. The returning
    // leg sits at an external-scope address, which change never does, so the
    // scope-keyed classification surfaces it as a SelfTransfer.
    #[test]
    fn scanned_self_send_marked_change_still_surfaces_as_self_transfer() {
        let conn = stub_conn();
        let gross = 1_000_000_000i64; // 10 ZEC back to our own deposit UA
        insert_tx(&conn, 1, -FEE, Some(FEE)); // delta nets to the bare fee
        insert_output(
            &conn,
            1,
            Some("u1selfdeposit"),
            Some(&ACCOUNT),
            true, // the scanner's post-scan marking that used to hide it
            gross,
            Some(KEY_SCOPE_EXTERNAL),
            None,
        );
        // The change split, internal scope as real change always is.
        insert_output(&conn, 1, None, Some(&ACCOUNT), true, 172_455_000, Some(1), None);

        let entries = funding_entries(&conn, &ACCOUNT, TIP).unwrap();
        assert_eq!(entries.len(), 1, "the self-send must not be dropped");
        let tx = &entries[0];
        assert_eq!(tx.direction, FundingDirection::SelfTransfer);
        assert_eq!(tx.amount, FEE as u64, "net effect is the fee");
        assert_eq!(tx.self_sent, Some(gross as u64), "gross value kept aside");
        assert_eq!(tx.fee, Some(FEE as u64));
        assert!(tx.recipients.is_empty(), "own address is not a recipient");
    }

    // Real change must keep not surfacing: a fee-only tx whose returning
    // outputs are all internal-scope (an internal shuffle / note split) is
    // wallet plumbing, not funding activity.
    #[test]
    fn internal_shuffle_is_still_dropped() {
        let conn = stub_conn();
        insert_tx(&conn, 2, -FEE, Some(FEE));
        insert_output(&conn, 2, None, Some(&ACCOUNT), true, 50_000_000, Some(1), None);

        let entries = funding_entries(&conn, &ACCOUNT, TIP).unwrap();
        assert!(entries.is_empty(), "internal shuffles stay out of the ledger");
    }

    // A bare-fee zkv write still shows as a ZkvOperation, not a SelfTransfer:
    // its memo output goes to the database's own external receiver, but the
    // zkv classification takes its own branch only when no deliberate
    // self-send leg exists... and the write's memo marks the whole tx as zkv
    // traffic. Guards the INIT row the user *did* see.
    #[test]
    fn bare_fee_zkv_write_is_a_zkv_operation() {
        let conn = stub_conn();
        insert_tx(&conn, 3, -FEE, Some(FEE));
        // The ZKV0 memo rides an output to our own external receiver, but a
        // zkv write addresses the *protocol* receiver with a dust/zero note,
        // recorded as change of the write; only internal change comes back.
        let memo = b"ZKV0 SET k v".as_slice();
        insert_output(&conn, 3, None, Some(&ACCOUNT), true, 0, Some(1), Some(memo));

        let entries = funding_entries(&conn, &ACCOUNT, TIP).unwrap();
        assert_eq!(entries.len(), 1);
        assert_eq!(entries[0].direction, FundingDirection::ZkvOperation);
        assert_eq!(entries[0].amount, FEE as u64);
        assert!(entries[0].is_zkv);
    }

    // Plain deposits and payments are untouched by the scope-keyed change.
    #[test]
    fn receives_and_sends_classify_as_before() {
        let conn = stub_conn();
        // Inbound 1 ZEC deposit (no fee of ours).
        insert_tx(&conn, 4, 100_000_000, None);
        insert_output(
            &conn,
            4,
            Some("u1ourreceiver"),
            Some(&ACCOUNT),
            false,
            100_000_000,
            Some(KEY_SCOPE_EXTERNAL),
            None,
        );
        // Outbound 0.5 ZEC payment to an external recipient, plus change.
        insert_tx(&conn, 5, -(50_000_000 + FEE), Some(FEE));
        insert_output(&conn, 5, Some("u1recipient"), None, false, 50_000_000, None, None);
        insert_output(&conn, 5, None, Some(&ACCOUNT), true, 25_000_000, Some(1), None);

        let entries = funding_entries(&conn, &ACCOUNT, TIP).unwrap();
        assert_eq!(entries.len(), 2);
        let recv = entries.iter().find(|t| t.txid.starts_with("04")).unwrap();
        assert_eq!(recv.direction, FundingDirection::Received);
        assert_eq!(recv.amount, 100_000_000);
        let sent = entries.iter().find(|t| t.txid.starts_with("05")).unwrap();
        assert_eq!(sent.direction, FundingDirection::Sent);
        assert_eq!(sent.amount, 50_000_000, "fee excluded from the amount");
        assert_eq!(sent.recipients, vec!["u1recipient".to_owned()]);
    }

    // Outputs of other accounts' (or unknown) ownership never count toward a
    // self-send leg, even at external scope.
    #[test]
    fn external_scope_output_to_another_account_is_not_a_self_send() {
        let conn = stub_conn();
        insert_tx(&conn, 6, -FEE, Some(FEE));
        insert_output(
            &conn,
            6,
            Some("u1other"),
            Some(&OTHER_ACCOUNT),
            false,
            10_000_000,
            Some(KEY_SCOPE_EXTERNAL),
            None,
        );

        let entries = funding_entries(&conn, &ACCOUNT, TIP).unwrap();
        // The other account's leg still produces a SelfTransfer ONLY if the
        // uuid matches ours; here it doesn't, and nothing else qualifies.
        assert!(entries.is_empty());
    }
}
