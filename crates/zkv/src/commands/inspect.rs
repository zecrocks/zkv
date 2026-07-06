use clap::{Args, ValueEnum};
use serde::Serialize;
use zcash_protocol::{consensus::NetworkType, ShieldedProtocol};

use crate::internal::protocol::{
    network_from_type, parse_zkv_addr, pubkey_bech32, receiver_domain, ua_request_for_pool,
    zkv_addr_to_uview, zkv_verifying_pubkey, ParsedZkvAddr,
};

#[derive(Clone, Copy, Debug, Default, ValueEnum)]
pub(crate) enum OutputFormat {
    /// Human-readable, aligned report on stdout.
    #[default]
    Friendly,
    /// Machine-readable JSON on stdout.
    Json,
}

/// `zkv inspect <zkv1…>`: decode a zkv address and print everything it carries.
///
/// Pure-offline: it parses the address and derives every field from the embedded
/// viewing key. No database, no chain access, so it works on any `zkv1…` token
/// someone hands you, not just databases you've imported. Handy before
/// `zkv watch` to see what an address actually is (which network and pool, the
/// birthday, the funding address, the database creator's signing key).
#[derive(Debug, Args)]
pub(crate) struct Command {
    /// The zkv address to inspect (a `zkv1…` / `zkvtest1…` token).
    zkv_addr: String,

    /// Output format: `friendly` (default, aligned) or `json`.
    #[arg(long, value_enum, default_value_t = OutputFormat::Friendly)]
    output: OutputFormat,
}

impl Command {
    pub(crate) fn run(self, _db: Option<String>) -> anyhow::Result<()> {
        let info = describe(&self.zkv_addr)?;
        match self.output {
            OutputFormat::Json => println!("{}", serde_json::to_string(&info)?),
            OutputFormat::Friendly => info.print_friendly(),
        }
        Ok(())
    }
}

/// Decode a zkv address into the fields `inspect` reports. Pure: it only parses
/// the token and derives values from the embedded viewing key, no I/O.
fn describe(zkv_addr: &str) -> anyhow::Result<Inspect> {
    let parsed = parse_zkv_addr(zkv_addr)?;

    // The root signing key (the UFVK-derived pubkey that signs INIT, hence the
    // database's permanent creator), canonical `zkvid1…`.
    let signing_key = pubkey_bech32(&zkv_verifying_pubkey(&parsed.ufvk)?);
    // The signing-domain receiver every `ZKV0` signature binds to.
    let receiver = receiver_domain(&parsed.ufvk, parsed.pool, parsed.network)?;
    // Same bytes under the standard `uview` HRP: paste into any wallet to view
    // the raw memos.
    let view_key = zkv_addr_to_uview(zkv_addr)?;

    Ok(Inspect {
        zkv_address: zkv_addr.to_owned(),
        network: network_label(parsed.network),
        pool: pool_label(parsed.pool),
        birthday: parsed.birthday,
        signing_key,
        funding_address: funding_address(&parsed),
        receiver,
        view_key,
    })
}

/// The funding address: a shielded-only unified address (the database's single
/// pool, no transparent receiver) you send ZEC to so the wallet can pay write
/// fees. Derived straight from the viewing key, so no synced wallet is needed.
fn funding_address(parsed: &ParsedZkvAddr) -> Option<String> {
    let net = network_from_type(parsed.network).ok()?;
    parsed
        .ufvk
        .default_address(ua_request_for_pool(parsed.pool))
        .ok()
        .map(|(ua, _)| ua.encode(&net))
}

fn network_label(net: NetworkType) -> &'static str {
    match net {
        NetworkType::Main => "mainnet",
        NetworkType::Test => "testnet",
        NetworkType::Regtest => "regtest",
    }
}

fn pool_label(pool: ShieldedProtocol) -> &'static str {
    match pool {
        ShieldedProtocol::Orchard => "orchard",
        ShieldedProtocol::Sapling => "sapling",
    }
}

/// The decoded address, ready to render as an aligned report or stable JSON.
#[derive(Serialize)]
struct Inspect {
    zkv_address: String,
    network: &'static str,
    pool: &'static str,
    birthday: u32,
    /// The root signing key (the database creator), canonical `zkvid1…`.
    signing_key: String,
    /// Where to send ZEC to fund write fees. Absent on regtest.
    #[serde(skip_serializing_if = "Option::is_none")]
    funding_address: Option<String>,
    /// The signing-domain receiver (`<network>:<hex>`) every signature binds to.
    receiver: String,
    /// The standard `uview…` viewing key (paste into any wallet to view memos).
    view_key: String,
}

impl Inspect {
    fn print_friendly(&self) {
        let mut rows: Vec<(&str, String)> = vec![
            ("zkv address", self.zkv_address.clone()),
            ("Network", self.network.to_owned()),
            ("Pool", self.pool.to_owned()),
            ("Birthday", self.birthday.to_string()),
            ("Signing key", self.signing_key.clone()),
        ];
        if let Some(addr) = &self.funding_address {
            rows.push(("Funding address", addr.clone()));
        }
        rows.push(("Receiver", self.receiver.clone()));
        rows.push(("View key", self.view_key.clone()));

        let width = rows.iter().map(|(l, _)| l.len() + 1).max().unwrap_or(0);
        for (label, value) in &rows {
            println!("{:<width$} {value}", format!("{label}:"));
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use zcash_keys::keys::UnifiedSpendingKey;
    use zcash_protocol::consensus;

    use crate::internal::protocol::encode_zkv_addr;

    fn sample_addr<P: consensus::Parameters>(
        net: P,
        pool: ShieldedProtocol,
        birthday: u32,
    ) -> String {
        let ufvk = UnifiedSpendingKey::from_seed(&net, &[0x42; 32], zip32::AccountId::ZERO)
            .expect("derive USK")
            .to_unified_full_viewing_key();
        encode_zkv_addr(&ufvk, &net, pool, birthday).expect("encode zkv address")
    }

    #[test]
    fn describes_a_testnet_orchard_address() {
        let addr = sample_addr(
            consensus::Network::TestNetwork,
            ShieldedProtocol::Orchard,
            1_234_567,
        );
        let info = describe(&addr).expect("describe");

        assert_eq!(info.zkv_address, addr);
        assert_eq!(info.network, "testnet");
        assert_eq!(info.pool, "orchard");
        assert_eq!(info.birthday, 1_234_567);
        // The fields derived from the viewing key are populated and well-formed.
        assert!(
            info.signing_key.starts_with("zkvid1"),
            "{}",
            info.signing_key
        );
        assert!(info.view_key.starts_with("uviewtest1"), "{}", info.view_key);
        assert!(info.receiver.starts_with("test:"), "{}", info.receiver);
        // Testnet is wallet-supported, so a funding address is derivable.
        let funding = info.funding_address.expect("funding address");
        assert!(funding.starts_with("utest1"), "{funding}");
    }

    #[test]
    fn pool_is_inferred_from_the_published_key() {
        let addr = sample_addr(
            consensus::Network::MainNetwork,
            ShieldedProtocol::Sapling,
            900_000,
        );
        let info = describe(&addr).expect("describe");
        assert_eq!(info.network, "mainnet");
        assert_eq!(info.pool, "sapling");
        assert!(info.view_key.starts_with("uview1"), "{}", info.view_key);
        assert!(info.receiver.starts_with("main:"), "{}", info.receiver);
    }

    #[test]
    fn describes_a_regtest_address() {
        // The regtest HRP family (`zkvregtest1...`) round-trips offline just
        // like the public networks; the regtest e2e harness relies on this.
        let addr = sample_addr(crate::data::Network::Regtest, ShieldedProtocol::Orchard, 42);
        assert!(addr.starts_with("zkvregtest1"), "{addr}");
        let info = describe(&addr).expect("describe");
        assert_eq!(info.network, "regtest");
        assert_eq!(info.pool, "orchard");
        assert_eq!(info.birthday, 42);
        assert!(
            info.signing_key.starts_with("zkvid1"),
            "{}",
            info.signing_key
        );
        assert!(
            info.view_key.starts_with("uviewregtest1"),
            "{}",
            info.view_key
        );
        assert!(info.receiver.starts_with("regtest:"), "{}", info.receiver);
        let funding = info.funding_address.expect("funding address");
        assert!(funding.starts_with("uregtest1"), "{funding}");
    }

    #[test]
    fn rejects_a_non_zkv_token() {
        assert!(describe("not-an-address").is_err());
    }
}
