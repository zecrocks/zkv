//! Build and broadcast a tiny shielded self-send carrying a zkv memo, in the
//! database's configured pool (Sapling or Orchard).

use std::num::NonZeroUsize;
use std::str::FromStr;

use anyhow::anyhow;
use secrecy::ExposeSecret as _;

use zcash_address::{ConversionError, ZcashAddress};
use zcash_client_backend::{
    data_api::{
        wallet::{
            create_proposed_transactions,
            input_selection::{GreedyInputSelector, TransparentSpendPolicy},
            propose_transfer, ConfirmationsPolicy, SpendingKeys,
        },
        Account, WalletRead,
    },
    fees::{standard::MultiOutputChangeStrategy, DustOutputPolicy, SplitPolicy, StandardFeeRule},
    proto::service,
    wallet::OvkPolicy,
};
use zcash_keys::{address::Address, keys::UnifiedSpendingKey};
use zcash_proofs::prover::LocalTxProver;
use zcash_protocol::{
    consensus::{NetworkType, Parameters},
    memo::{Memo, MemoBytes},
    value::Zatoshis,
    ShieldedPool,
};
use zip321::{Payment, TransactionRequest};

use crate::{
    config::WalletConfig,
    data::{get_db_paths, open_wallet_db},
    error,
    internal::{sync::run_sync, write::augment_insufficient_funds},
    remote::ConnectionArgs,
};

/// Note-management defaults for the change splitter ([`MultiOutputChangeStrategy`]):
/// keep ~`TARGET_NOTE_COUNT` spendable change notes so a high-frequency writer
/// isn't stalled waiting on a single unconfirmed note. Internal constants (no
/// longer CLI flags) because zkv writes are uniform.
const TARGET_NOTE_COUNT: usize = 4;

/// Floor for each split change note, per network. Mainnet uses 0.005 ZEC
/// (matches zcash_client_backend's own `SplitPolicy::MIN_NOTE_VALUE`); test
/// networks use a much smaller 0.0005 TAZ so even a ~0.0025 TAZ faucet drip
/// splits into several notes on a new user's first write. This value doubles
/// as the `ExceedsMinValue` threshold for counting existing notes toward the
/// target, so it is the single knob that controls splitting.
const MIN_SPLIT_VALUE_MAIN: u64 = 500_000;
const MIN_SPLIT_VALUE_TEST: u64 = 50_000;

/// Per-network floor (in zatoshis) for each split change note.
fn min_split_output_value(net: NetworkType) -> u64 {
    match net {
        NetworkType::Main => MIN_SPLIT_VALUE_MAIN,
        NetworkType::Test | NetworkType::Regtest => MIN_SPLIT_VALUE_TEST,
    }
}

/// A user-facing network label for error text.
fn net_label(net: NetworkType) -> &'static str {
    match net {
        NetworkType::Main => "mainnet",
        NetworkType::Test => "testnet",
        NetworkType::Regtest => "regtest",
    }
}

/// Parse a decimal ZEC amount (`"1.5"`, `"0.0001"`, `".5"`) into [`Zatoshis`].
/// Rejects empty input, non-numeric characters, more than 8 fractional digits,
/// zero, and overflow. The `String` error is a short, user-facing reason.
pub fn parse_zec(input: &str) -> Result<Zatoshis, String> {
    let s = input.trim();
    if s.is_empty() {
        return Err("enter an amount".into());
    }
    let (whole, frac) = s.split_once('.').unwrap_or((s, ""));
    if frac.len() > 8 {
        return Err("at most 8 decimal places".into());
    }
    let digits_only = |p: &str| p.bytes().all(|b| b.is_ascii_digit());
    if !digits_only(whole) || !digits_only(frac) {
        return Err("not a valid amount".into());
    }
    let whole_zats: u64 = if whole.is_empty() {
        0
    } else {
        whole
            .parse::<u64>()
            .ok()
            .and_then(|w| w.checked_mul(100_000_000))
            .ok_or("amount is too large")?
    };
    // Right-pad the fractional part to 8 digits (zatoshi precision).
    let frac_zats: u64 = if frac.is_empty() {
        0
    } else {
        format!("{frac:0<8}")
            .parse()
            .map_err(|_| "not a valid amount".to_string())?
    };
    let zats = whole_zats
        .checked_add(frac_zats)
        .ok_or("amount is too large")?;
    if zats == 0 {
        return Err("amount must be greater than zero".into());
    }
    Zatoshis::from_u64(zats).map_err(|_| "amount is too large".into())
}

/// Validate `recipient` as a Zcash address on `network`, returning a short
/// label for its kind (`"unified"`, `"sapling"`, `"transparent"`, `"TEX"`).
/// Surfaces a friendly reason on failure (unparseable, wrong network, or an
/// unsupported kind). Pure: no I/O. Accepts every address type librustzcash
/// recognizes.
pub fn validate_recipient(
    recipient: &str,
    network: crate::network::Network,
) -> Result<String, String> {
    describe_recipient(recipient, network).map(|info| info.kind)
}

/// A validated recipient: its kind label plus the network and shielded pool it
/// pays into, for a richer "valid X address (network, pool)" UI hint. `pool` is
/// `None` for transparent / TEX recipients (no shielded pool); for a unified
/// address it is the preferred shielded pool present (Orchard over Sapling), or
/// transparent if the UA carries only a transparent receiver.
pub struct RecipientInfo {
    pub kind: String,
    pub network: String,
    pub pool: Option<String>,
}

/// Validate `recipient` as a Zcash address on `network`, returning its kind,
/// network label, and shielded pool. Surfaces the same friendly reasons on
/// failure as [`validate_recipient`]. Pure: no I/O.
pub fn describe_recipient(
    recipient: &str,
    network: crate::network::Network,
) -> Result<RecipientInfo, String> {
    let recipient = recipient.trim();
    if recipient.is_empty() {
        return Err("enter a recipient address".into());
    }
    let addr = ZcashAddress::from_str(recipient)
        .map_err(|_| "that doesn't look like a Zcash address".to_string())?;
    let net = net_label(network.network_type()).to_string();
    match addr.convert_if_network::<Address>(network.network_type()) {
        Ok(Address::Unified(ua)) => {
            let pool = if ua.has_orchard() {
                "orchard"
            } else if ua.has_sapling() {
                "sapling"
            } else {
                "transparent"
            };
            Ok(RecipientInfo {
                kind: "unified".into(),
                network: net,
                pool: Some(pool.into()),
            })
        }
        Ok(Address::Sapling(_)) => Ok(RecipientInfo {
            kind: "sapling".into(),
            network: net,
            pool: Some("sapling".into()),
        }),
        Ok(Address::Transparent(_)) => Ok(RecipientInfo {
            kind: "transparent".into(),
            network: net,
            pool: None,
        }),
        Ok(Address::Tex(_)) => Ok(RecipientInfo {
            kind: "TEX".into(),
            network: net,
            pool: None,
        }),
        Err(ConversionError::IncorrectNetwork { expected, actual }) => Err(format!(
            "that's a {} address, but this database is on {}",
            net_label(actual),
            net_label(expected),
        )),
        Err(_) => Err("that address type isn't supported".into()),
    }
}

/// Build and broadcast a plain value transfer of `amount` to an arbitrary
/// Zcash address (any type librustzcash supports). Validates the recipient
/// against the database's network first (so we never broadcast to a
/// wrong-network address), syncs unless `no_sync`, then signs and submits.
/// Returns the broadcast txid. No memo; this is a bare ZEC send, so transparent and
/// TEX recipients work too.
pub async fn send_funds(
    db_name: &str,
    connection: &ConnectionArgs,
    recipient: &str,
    amount: Zatoshis,
    memo: Option<&str>,
    no_sync: bool,
) -> anyhow::Result<String> {
    let recipient = recipient.trim();
    let cfg = WalletConfig::read(db_name)?;
    validate_recipient(recipient, cfg.network).map_err(|m| anyhow!(m))?;

    if !no_sync {
        run_sync(db_name, connection, false).await?;
    }

    let address = ZcashAddress::from_str(recipient).map_err(|e| anyhow!("bad address: {e}"))?;
    // An optional ZIP-302 text memo (<=512 bytes). Only shielded recipients can
    // carry one; `Payment::new` rejects a memo to a transparent/TEX address.
    let payment = match memo.map(str::trim).filter(|m| !m.is_empty()) {
        Some(text) => {
            let memo = Memo::from_str(text).map_err(|e| anyhow!("invalid memo: {e}"))?;
            Payment::new(
                address,
                Some(amount),
                Some(MemoBytes::from(&memo)),
                None,
                None,
                vec![],
            )
            .map_err(|e| anyhow!("this recipient can't receive a memo: {e}"))?
        }
        None => Payment::without_memo(address, amount),
    };
    let request =
        TransactionRequest::new(vec![payment]).map_err(|e| anyhow!("bad tx request: {e}"))?;

    pay(db_name, connection, request)
        .await
        .map_err(|e| augment_insufficient_funds(e, db_name))
}

/// Build and broadcast a single transaction containing the supplied payment(s).
/// Picks the only local account in the named database, unwraps the stored seed
/// via the age identity (the `security-theater-key` file), signs, and submits
/// to the configured lightwalletd.
///
/// Returns the broadcast txid as a hex string.
pub async fn pay(
    db_name: &str,
    connection: &ConnectionArgs,
    request: TransactionRequest,
) -> anyhow::Result<String> {
    let config = WalletConfig::read(db_name)?;
    let params = config.network;

    let (_, db_data_path) = get_db_paths(db_name)?;
    let mut db_data = open_wallet_db(db_data_path, params)?;

    // For zkv the wallet always has exactly one account (the admin's). Pick it.
    let account_ids = db_data.get_account_ids()?;
    let account_id = match account_ids.as_slice() {
        [id] => *id,
        [] => anyhow::bail!("database {db_name:?} has no accounts"),
        _ => anyhow::bail!("database {db_name:?} has multiple accounts; zkv assumes one"),
    };
    let account = db_data
        .get_account(account_id)?
        .ok_or_else(|| anyhow!("account vanished"))?;
    let derivation = account
        .source()
        .key_derivation()
        .ok_or_else(|| anyhow!("cannot spend from a watch-only database"))?;

    // Unwrap the stored seed using the age identity in the db dir.
    let seed = config.decrypt_seed()?;
    let usk =
        UnifiedSpendingKey::from_seed(&params, seed.expose_secret(), derivation.account_index())
            .map_err(error::Error::from)?;

    let mut client = connection.connect(params).await?;

    tracing::debug!(db = db_name, "creating transaction");
    let prover = LocalTxProver::bundled();
    // The change strategy's `fallback_change_pool` is where change lands when
    // the transaction has no shielded inputs. It must be a pool the fee/change
    // accounting models directly (`OutputManifest` has only Sapling and Orchard
    // slots): passing `Ironwood` trips a `total_shielded() == target_change_count`
    // assertion in `zcash_client_backend`. Ironwood shares the Orchard pool
    // on-chain, and the builder routes Orchard-pool change into the V6 Ironwood
    // bundle when NU6.3 is active, so fold Ironwood to Orchard here (matching
    // zcash-devtool, which always passes Orchard).
    let fallback_change_pool = match config.pool {
        ShieldedPool::Ironwood => ShieldedPool::Orchard,
        other => other,
    };
    let change_strategy = MultiOutputChangeStrategy::new(
        StandardFeeRule::Zip317,
        None,
        fallback_change_pool,
        DustOutputPolicy::default(),
        SplitPolicy::with_min_output_value(
            NonZeroUsize::new(TARGET_NOTE_COUNT).expect("nonzero const"),
            zcash_protocol::value::Zatoshis::from_u64(min_split_output_value(
                params.network_type(),
            ))?,
        ),
    );
    let input_selector = GreedyInputSelector::new();

    let proposal = propose_transfer(
        &mut db_data,
        &params,
        account.id(),
        &input_selector,
        &change_strategy,
        request,
        ConfirmationsPolicy::default(),
        // spend_policy (added in the Ironwood RC, transparent-inputs feature):
        // ShieldedOnly (the library default). zkv's funding UA is shielded-only
        // (ua_request_for_pool omits the transparent receiver), so writes are
        // funded by and spent from shielded notes; there are no transparent
        // inputs to select.
        &TransparentSpendPolicy::default(),
        // proposed_version (unstable feature): let the wallet pick the tx version
        // for the target height (Ironwood/V6 past NU6.3).
        None,
    )
    .map_err(error::Error::from)?;

    tracing::debug!(?proposal, "proposed transfer");

    let txids = create_proposed_transactions(
        &mut db_data,
        &params,
        &prover,
        &prover,
        &SpendingKeys::from_unified_spending_key(usk),
        OvkPolicy::Sender,
        &proposal,
        None,
    )
    .map_err(error::Error::from)?;

    if txids.len() > 1 {
        anyhow::bail!("Multi-transaction proposals are not supported.");
    }
    let txid = *txids.first();

    tracing::debug!(%txid, "broadcasting");
    let tx = db_data
        .get_transaction(txid)?
        .ok_or_else(|| anyhow!("Transaction not found for id {:?}", txid))?;
    let mut raw_tx = service::RawTransaction::default();
    tx.write(&mut raw_tx.data)
        .map_err(|e| anyhow!("serializing transaction {:?}: {e}", tx.txid()))?;
    let txid = tx.txid();
    let response = client.send_transaction(raw_tx).await?.into_inner();

    if response.error_code != 0 {
        return Err(error::Error::SendFailed {
            code: response.error_code,
            reason: response.error_message,
        }
        .into());
    }

    Ok(txid.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn zats(s: &str) -> u64 {
        u64::from(parse_zec(s).expect("valid amount"))
    }

    #[test]
    fn parse_zec_whole_and_fractional() {
        assert_eq!(zats("1"), 100_000_000);
        assert_eq!(zats("1.5"), 150_000_000);
        assert_eq!(zats("0.0001"), 10_000);
        assert_eq!(zats("0.00000001"), 1);
        // Leading-dot and surrounding whitespace are accepted.
        assert_eq!(zats(".5"), 50_000_000);
        assert_eq!(zats("  2.25  "), 225_000_000);
    }

    #[test]
    fn parse_zec_rejects_bad_input() {
        for bad in [
            "",
            " ",
            ".",
            "0",
            "0.0",
            "abc",
            "1.2.3",
            "-1",
            "1e3",
            "1.234567890",
        ] {
            assert!(parse_zec(bad).is_err(), "expected {bad:?} to be rejected");
        }
    }

    #[test]
    fn validate_recipient_rejects_empty_and_garbage() {
        let net = crate::network::Network::Main;
        assert!(validate_recipient("", net).is_err());
        assert!(validate_recipient("not-an-address", net).is_err());
    }

    #[test]
    fn validate_recipient_accepts_on_its_network_and_rejects_cross_network() {
        // A verified mainnet Sapling address (the `zcash_address` crate's own
        // doctest vector). It validates on mainnet as "sapling" and is refused
        // on testnet, exercising both the kind-label and wrong-network
        // branches without fabricating a checksum.
        let zs = "zs1z7rejlpsa98s2rrrfkwmaxu53e4ue0ulcrw0h4x5g8jl04tak0d3mm47vdtahatqrlkngh9slya";
        assert_eq!(
            validate_recipient(zs, crate::network::Network::Main).as_deref(),
            Ok("sapling"),
        );
        assert!(validate_recipient(zs, crate::network::Network::Test).is_err());
    }

    #[test]
    fn describe_recipient_classifies_kind_and_pool() {
        // The same verified mainnet Sapling vector as the test above.
        let zs = "zs1z7rejlpsa98s2rrrfkwmaxu53e4ue0ulcrw0h4x5g8jl04tak0d3mm47vdtahatqrlkngh9slya";
        let info = describe_recipient(zs, crate::network::Network::Main).expect("valid sapling");
        assert_eq!(info.kind, "sapling");
        assert_eq!(info.pool.as_deref(), Some("sapling"));
        // Wrong network is a clear error, not a misclassification.
        assert!(describe_recipient(zs, crate::network::Network::Test).is_err());
        // Empty and garbage are rejected before any network check.
        assert!(describe_recipient("", crate::network::Network::Main).is_err());
        assert!(describe_recipient("not-an-address", crate::network::Network::Main).is_err());
    }
}
