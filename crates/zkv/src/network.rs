//! The consensus network a zkv database lives on: mainnet, testnet, or a local
//! regtest chain.
//!
//! librustzcash's own [`zcash_protocol::consensus::Network`] only models
//! main/test, but the whole wallet stack (`WalletDb`, key derivation, address
//! encoding, the sync pipeline) is generic over
//! [`zcash_protocol::consensus::Parameters`]. [`Network`] is the single
//! `Parameters` value zkv threads through that stack so regtest, backed by
//! fixed [`zcash_protocol::local_consensus::LocalNetwork`] activation
//! heights, fits without bifurcating every signature. Re-exported as
//! `data::Network` (its historical home) so existing paths keep working.
//!
//! The regtest activation heights are **fixed** (no config surface): they must
//! match the chain the regtest harness's zebrad runs (see
//! `regtest-harness/src/lib.rs`), and there is exactly one such chain shape.

use zcash_protocol::consensus::{
    BlockHeight, NetworkType, NetworkUpgrade, Parameters, MAIN_NETWORK, TEST_NETWORK,
};
use zcash_protocol::local_consensus::LocalNetwork;

/// The network a zkv database is bound to. `Copy` so it threads by value
/// through the wallet APIs exactly as the upstream `consensus::Network` did.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Network {
    /// Production Zcash.
    #[default]
    Main,
    /// The public testnet.
    Test,
    /// A local regtest chain, for the end-to-end harness. Activation heights
    /// are fixed (see `regtest_activation` in this module) and must match the
    /// harness's zebrad config.
    Regtest,
}

/// Height at which NU5..NU6 activate on the regtest chain (Orchard live from
/// the first block).
const REGTEST_GENESIS_UPGRADES: u32 = 1;
/// Height at which NU6.1/NU6.2 activate. Not height 1: NU6.1's activation
/// block must carry ZIP-271 lockbox disbursements, and the deferred pool only
/// accrues once NU6 is live, so the harness lets NU6 run for a few blocks
/// first. Must match the harness zebrad config's activation heights.
const REGTEST_NU6_2_HEIGHT: u32 = 4;

/// Height at which NU6.3 (Ironwood) activates on the regtest chain. Kept a few
/// blocks after NU6.2 (like NU6.2 trails NU6) and **must** agree across all
/// three sides of the harness: this `LocalNetwork`, the zebrad config's
/// `[network.testnet_parameters.activation_heights]` `"NU6.3"` key (emitted only
/// for the Ironwood tier, since stock zebra rejects the unknown key), and the
/// devtool funder's `--activation-heights`. Regtest is a testnet-flavored local
/// chain, so Ironwood is available there (see `config::ironwood_available`).
pub const REGTEST_NU6_3_HEIGHT: u32 = 8;

/// The fixed regtest chain parameters, as a [`LocalNetwork`].
// `zcash_unstable` is a librustzcash RUSTFLAGS cfg (nu7/zfuture). We don't set
// it, but the gated fields keep this literal valid if someone builds with
// those NUs enabled.
#[allow(unexpected_cfgs)]
fn regtest_activation() -> LocalNetwork {
    let h = Some(BlockHeight::from_u32(REGTEST_GENESIS_UPGRADES));
    let nu62 = Some(BlockHeight::from_u32(REGTEST_NU6_2_HEIGHT));
    let nu63 = Some(BlockHeight::from_u32(REGTEST_NU6_3_HEIGHT));
    LocalNetwork {
        overwinter: h,
        sapling: h,
        blossom: h,
        heartwood: h,
        canopy: h,
        nu5: h,
        nu6: h,
        nu6_1: nu62,
        nu6_2: nu62,
        nu6_3: nu63,
        #[cfg(zcash_unstable = "nu7")]
        nu7: nu63,
        #[cfg(zcash_unstable = "zfuture")]
        z_future: nu63,
    }
}

impl Network {
    /// Parse a CLI/keys.toml network name. Canonical names are `"mainnet"`,
    /// `"testnet"`, and `"regtest"`; short forms accepted for legacy/CLI
    /// brevity. (The `String` error feeds clap.)
    pub fn parse(name: &str) -> Result<Network, String> {
        match name {
            "mainnet" | "main" => Ok(Network::Main),
            "testnet" | "test" => Ok(Network::Test),
            "regtest" => Ok(Network::Regtest),
            other => Err(format!(
                "Unsupported network: {other:?} (use \"mainnet\", \"testnet\" or \"regtest\")",
            )),
        }
    }

    /// The canonical name, as written to `keys.toml` and shown in the CLI/GUI.
    pub fn name(&self) -> &'static str {
        match self {
            Network::Main => "mainnet",
            Network::Test => "testnet",
            Network::Regtest => "regtest",
        }
    }

    pub fn ticker(&self) -> &'static str {
        match self {
            Network::Main => "ZEC",
            // Regtest coins are testnet-flavored throwaway money.
            Network::Test | Network::Regtest => "TAZ",
        }
    }
}

impl Parameters for Network {
    fn network_type(&self) -> NetworkType {
        match self {
            Network::Main => NetworkType::Main,
            Network::Test => NetworkType::Test,
            Network::Regtest => NetworkType::Regtest,
        }
    }

    fn activation_height(&self, nu: NetworkUpgrade) -> Option<BlockHeight> {
        match self {
            Network::Main => MAIN_NETWORK.activation_height(nu),
            Network::Test => TEST_NETWORK.activation_height(nu),
            Network::Regtest => regtest_activation().activation_height(nu),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_and_name_round_trip() {
        for net in [Network::Main, Network::Test, Network::Regtest] {
            assert_eq!(Network::parse(net.name()), Ok(net));
        }
        assert_eq!(Network::parse("main"), Ok(Network::Main));
        assert_eq!(Network::parse("test"), Ok(Network::Test));
        assert!(Network::parse("bogus").is_err());
    }

    #[test]
    fn network_types_match_upstream() {
        assert_eq!(Network::Main.network_type(), NetworkType::Main);
        assert_eq!(Network::Test.network_type(), NetworkType::Test);
        assert_eq!(Network::Regtest.network_type(), NetworkType::Regtest);
    }

    #[test]
    fn main_and_test_delegate_to_upstream_heights() {
        for nu in [NetworkUpgrade::Sapling, NetworkUpgrade::Nu5] {
            assert_eq!(
                Network::Main.activation_height(nu),
                MAIN_NETWORK.activation_height(nu)
            );
            assert_eq!(
                Network::Test.activation_height(nu),
                TEST_NETWORK.activation_height(nu)
            );
        }
    }

    #[test]
    fn regtest_has_orchard_active_from_the_first_block() {
        // network_type drives address HRPs and consensus branch ids; NU5
        // (Orchard) must be live at height 1 so the harness chain carries
        // Orchard memos from the start.
        let net = Network::Regtest;
        assert!(net.is_nu_active(NetworkUpgrade::Nu5, BlockHeight::from_u32(1)));
        assert!(net.is_nu_active(NetworkUpgrade::Sapling, BlockHeight::from_u32(1)));
        assert_eq!(
            net.activation_height(NetworkUpgrade::Nu6_1),
            Some(BlockHeight::from_u32(REGTEST_NU6_2_HEIGHT))
        );
    }
}
