//! Plain value transfers, and the address handling they share with the GUI.
//!
//! The transaction building itself belongs to the wallet engine now: this
//! module composes a [`TransactionRequest`] and hands it to `Engine::ship`,
//! exactly as the memo write path does. What is left here is zkv's own:
//! parsing a user-typed ZEC amount, and classifying a user-typed recipient
//! address against the database's network before anything is broadcast.

use std::str::FromStr;

use anyhow::anyhow;

use zcash_address::{ConversionError, ZcashAddress};
use zcash_keys::address::Address;
use zcash_protocol::{
    consensus::{NetworkType, Parameters},
    memo::{Memo, MemoBytes},
    value::Zatoshis,
};
use zip321::{Payment, TransactionRequest};

use crate::config::WalletConfig;

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
/// address it is the preferred shielded pool present (`"ironwood/orchard"` over
/// Sapling; Ironwood shares the Orchard receiver, so one receiver serves
/// both), or transparent if the UA carries only a transparent receiver.
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
                "ironwood/orchard"
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
    engine: &crate::engine::Engine,
    recipient: &str,
    amount: Zatoshis,
    memo: Option<&str>,
    no_sync: bool,
) -> anyhow::Result<String> {
    let recipient = recipient.trim();
    let cfg = WalletConfig::read(db_name)?;
    validate_recipient(recipient, cfg.network).map_err(|m| anyhow!(m))?;

    // Through the engine, like every other spend the facade drives: taking
    // zkv's own DbLock here and then starting a node would have each waiting
    // on the other's hold of the same `.lock` file.
    if !no_sync {
        engine.sync_to_tip(None).await?;
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

    engine
        .ship(request)
        .await
        .map_err(|e| crate::internal::write::insufficient_from_engine(e, db_name))
}

#[cfg(test)]
mod tests {
    use super::*;
    use zcash_keys::keys::UnifiedSpendingKey;
    use zcash_protocol::ShieldedPool;

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
        // A UA with an Orchard receiver is labeled ironwood/orchard
        // (Ironwood shares the Orchard receiver, so one receiver serves both).
        let ua = {
            use crate::internal::protocol::ua_request_for_pool;
            let net = crate::network::Network::Main;
            UnifiedSpendingKey::from_seed(&net, &[0x42; 32], zip32::AccountId::ZERO)
                .expect("derive USK")
                .to_unified_full_viewing_key()
                .default_address(ua_request_for_pool(ShieldedPool::Ironwood))
                .expect("orchard UA")
                .0
                .encode(&net)
        };
        let info = describe_recipient(&ua, crate::network::Network::Main).expect("valid unified");
        assert_eq!(info.kind, "unified");
        assert_eq!(info.network, "mainnet");
        assert_eq!(info.pool.as_deref(), Some("ironwood/orchard"));
        // Wrong network is a clear error, not a misclassification.
        assert!(describe_recipient(zs, crate::network::Network::Test).is_err());
        // Empty and garbage are rejected before any network check.
        assert!(describe_recipient("", crate::network::Network::Main).is_err());
        assert!(describe_recipient("not-an-address", crate::network::Network::Main).is_err());
    }
}
