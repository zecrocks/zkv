//! Mapping a zkv database onto a zecd node configuration.
//!
//! zkv and zecd describe a wallet differently, and the whole translation lives
//! here:
//!
//! * **One node per database.** zecd's datadir *is* the zkv database directory,
//!   and so is the wallet directory, so `data.sqlite`, `blockmeta.sqlite` and
//!   `blocks/` are the files already there. Both projects build them with the
//!   same `zcash_client_sqlite` version through the same `FsBlockDb` layout,
//!   so there is nothing to convert.
//! * **Pool.** A zkv database lives in exactly one shielded pool, fixed at
//!   creation. zecd has no such concept, so the pool becomes the wallet's
//!   enabled receiver set, which is what decides the pool its addresses
//!   receive into.
//! * **Seed.** zkv keeps ownership of `keys.toml` and its age identity, so
//!   zecd is pointed at the identity file zkv already wrote rather than being
//!   allowed to manage its own.
//! * **Upstream.** zkv resolves a lightwalletd `host:port`; zecd takes a token
//!   in a small grammar where a bare `host:port` means lightwalletd with TLS
//!   decided by the same locality heuristic zkv applies. So the resolved
//!   endpoint passes through unchanged.
//!
//! Nothing here starts a node or touches the network.

use std::path::Path;

use zecd::config::{AppConfig, ConfigOverrides};
use zecd::pools::{Receiver, ReceiverSet};

use crate::config::WalletConfig;
use crate::network::{Network, REGTEST_NU6_3_HEIGHT};
use crate::remote::{ConnectionArgs, ConnectionMode};

use super::EngineError;

/// The wallet name zkv uses inside a node. Each node serves exactly one
/// database, so the name is a constant; it is zecd's default wallet, which is
/// what `Node::call(None, ..)` targets.
pub(crate) const WALLET: &str = "default";

/// zecd's environment variable for the regtest NU6.3 (Ironwood) activation
/// height. zkv compiles its regtest heights in ([`REGTEST_NU6_3_HEIGHT`]);
/// zecd reads this instead, and unset means "no NU6.3 on regtest". They have to
/// agree or the two sides commit transactions to different consensus branches.
const ZECD_REGTEST_NU63_HEIGHT: &str = "ZECD_REGTEST_NU63_HEIGHT";

/// Build the zecd configuration for one zkv database.
pub(crate) fn app_config(
    db_dir: &Path,
    cfg: &WalletConfig,
    conn: &ConnectionArgs,
) -> Result<AppConfig, EngineError> {
    align_regtest_activation(cfg.network)?;

    let server = upstream_token(cfg.network, conn)?;
    let overrides = ConfigOverrides {
        datadir: Some(db_dir.to_path_buf()),
        network: Some(network_token(cfg.network).to_string()),
        server: Some(server),
        // The proxy travels with the server choice. zecd routes every
        // connection the node makes through `[backend] proxy`, with the
        // destination resolved by the proxy rather than locally, which is the
        // same promise zkv's own transport keeps for the probes and the
        // shallow reader.
        proxy: proxy_token(conn),
        // zkv owns keys.toml and the age identity that wraps its seed, so zecd
        // is pointed at the file that is already there instead of managing one
        // of its own under the datadir.
        age_identity: Some(crate::config::identity_path(db_dir)),
        ..Default::default()
    };

    let mut app = AppConfig::resolve_overrides(&overrides).map_err(EngineError::Other)?;

    // The wallet directory is the database directory itself, not the
    // `<datadir>/default/` subdirectory zecd would use by default: the wallet
    // files are already where zkv has always kept them.
    let entry = app
        .wallets
        .get_mut(WALLET)
        .ok_or_else(|| EngineError::Other(anyhow::anyhow!("zecd has no default wallet entry")))?;
    entry.dir = db_dir.to_path_buf();
    entry.pools = receiver_set(cfg.pool);
    entry.default_receivers = receiver_set(cfg.pool);
    // zkv memo writes are shielded self-sends; the database never hands out or
    // spends transparent receivers, and its funding address is derived by zkv
    // itself. Leaving transparent off keeps a light backend from doing
    // per-address spend-detection round trips it would gain nothing from.
    entry.transparent_enabled = false;
    entry.transparent_default = false;

    // A batch write puts one memo per output in a single transaction, and zkv
    // bounds the batch itself. zecd's default guardrail (50 Orchard actions)
    // would reject a large batch that is otherwise perfectly valid, and it
    // counts change notes as well as payments, so the effective limit would be
    // both lower than it looks and dependent on note selection.
    app.spend.orchard_action_limit = 0;

    // Restore zkv's own change-splitting policy over the node's defaults; see
    // [`min_split_output_value`].
    app.spend.target_note_count = TARGET_NOTE_COUNT;
    app.spend.min_split_output_value = min_split_output_value(cfg.network);
    app.spend
        .validate_change_splitting()
        .map_err(EngineError::Other)?;

    // Memos are the entire product. `[sync] fetch_memos = false` is an
    // exchange's setting: it skips fetching the full transaction for anything
    // the wallet only received in, which is exactly where a zkv memo lives, and
    // then withholds memos uniformly. Every key in the database would read as
    // absent, with a perfectly healthy sync. It is the node's default, but it
    // is stated rather than inherited: zecd resolves `<datadir>/zecd.toml` when
    // one exists, and the datadir here is the user's database directory.
    app.sync.fetch_memos = true;

    // A per-database node is never a fleet host. Upstream's default is off and
    // the fleet is experimental, but the same stray-file argument applies: a
    // `wallets.d/` under a database directory must not enrol the node that
    // serves it. The fleet gets its own node, with its own datadir.
    app.fleet.enabled = false;

    Ok(app)
}

/// Build the zecd configuration for a network's **shared scan**.
///
/// The differences from a per-database node are all consequences of serving
/// many watch-only viewing keys instead of one database:
///
/// * **The fleet is switched on**, and pointed at zkv's own layout under the
///   fleet datadir. Upstream ships it experimental and off, so this is the one
///   place zkv opts in; every per-database node states the opposite.
/// * **Both shielded receivers are scanned.** A per-database node scans the one
///   pool its database lives in, but a shard holds whatever its members are, so
///   scanning only Orchard (the node's default) would leave a Sapling member
///   permanently empty. The per-member read filter is still that member's own
///   pool.
/// * **No proving keys and no shutdown drain.** A member cannot spend: there is
///   no seed anywhere in the fleet. Building Orchard and Ironwood proving keys
///   at every start, and waiting at shutdown for sends that cannot exist, would
///   both be pure cost.
/// * **No age identity.** Same reason: nothing here decrypts a seed.
///
/// `cohort_depth` is the one number zkv sets against upstream's default rather
/// than with it, and it is a policy choice, not a fix. Placement groups members
/// by arrival: a newcomer whose birthday is within `cohort_depth` *below* a
/// shard's floor joins that shard and **rewinds it** to that birthday, while an
/// older one opens a new shard. Upstream's 10_000 suits a server enrolling
/// accounts near the tip; a desktop user adds watch databases with arbitrary
/// birthdays in whatever order they hear about them, so the smaller value
/// bounds how far a newcomer can drag everybody else back, at the price of more
/// shards. Upstream has this on its own list to fix properly.
pub(crate) fn fleet_app_config(
    fleet_dir: &Path,
    network: Network,
    conn: &ConnectionArgs,
) -> Result<AppConfig, EngineError> {
    align_regtest_activation(network)?;

    let overrides = ConfigOverrides {
        datadir: Some(fleet_dir.to_path_buf()),
        network: Some(network_token(network).to_string()),
        server: Some(upstream_token(network, conn)?),
        proxy: proxy_token(conn),
        // Nothing in the shared scan holds a seed, so there is no identity to
        // point at and nothing that could ask for one.
        age_identity: None,
        ..Default::default()
    };
    let mut app = AppConfig::resolve_overrides(&overrides).map_err(EngineError::Other)?;

    app.fleet.enabled = true;
    app.fleet.manifest_dir = fleet_dir.join(crate::fleet::MANIFEST_DIR);
    app.fleet.dir = fleet_dir.join(crate::fleet::SHARDS_DIR);
    app.fleet.cohort_depth = FLEET_COHORT_DEPTH;

    // A shard is scanned for every member in it, whichever pool each lives in.
    let receivers =
        ReceiverSet::new([Receiver::Sapling, Receiver::Orchard]).map_err(EngineError::Other)?;
    app.pools.enabled = receivers.clone();
    app.pools.default_receivers = receivers;

    // Memos are the product here as much as anywhere; see `app_config`.
    app.sync.fetch_memos = true;
    // Members never spend.
    app.spend.cache_proving_key = false;
    app.spend.shutdown_drain_secs = 0;

    Ok(app)
}

/// How far below a shard's oldest birthday a newcomer may be and still join it,
/// rewinding that shard to its birthday rather than opening a new one.
///
/// See [`fleet_app_config`] for why this is a tenth of upstream's default.
const FLEET_COHORT_DEPTH: u32 = 1_000;

// The whole point of setting it is that upstream's default lets a newcomer drag
// a running shard much further back than a desktop user would tolerate, so this
// has to stay below it. A compile-time check rather than a test: if upstream
// ever lowers its own default past this, the override has stopped meaning
// anything and the build should say so.
const _: () = assert!(FLEET_COHORT_DEPTH < zecd::config::DEFAULT_FLEET_COHORT_DEPTH);

/// Keep ~this many spendable change notes, so a high-frequency writer is not
/// stalled waiting on a single unconfirmed one.
///
/// Same value as zecd's default; set explicitly because it is half of a policy
/// whose other half zkv does override, and the two only make sense read
/// together.
const TARGET_NOTE_COUNT: usize = 4;

/// Floor (in zatoshis) on each split change note, per network.
///
/// Mainnet uses 0.005 ZEC (matching `zcash_client_backend`'s own
/// `SplitPolicy::MIN_NOTE_VALUE`); test networks use a much smaller 0.0005 TAZ
/// so even a ~0.0025 TAZ faucet drip still splits into several notes on a new
/// user's first write. The value doubles as the threshold for counting
/// existing notes toward the target, so it is the single knob controlling
/// splitting.
///
/// This is zkv's pre-engine policy, restored. The node's own floor is 0.1 ZEC
/// on every network, and it applies to a wallet's *balance* rather than to the
/// network: a testnet database funded by the faucet sits entirely below it, so
/// it got one change note where it wanted four, and consecutive writes then
/// serialized on `[spend] trusted_confirmations` (3 blocks, ~7.5 minutes)
/// instead of spending several notes in turn. That was the one behavioural
/// regression the port accepted, filed upstream as ask B2 and answered in
/// zecd #219 by making both values configurable.
fn min_split_output_value(net: Network) -> u64 {
    match net {
        Network::Main => 500_000,
        Network::Test | Network::Regtest => 50_000,
    }
}

/// The node's proxy setting for a database's connection mode.
///
/// zkv addresses a proxy as a bare socket address, zecd as a `socks5://host:port`
/// token: the same value in two spellings. Before zecd #221 the node had no
/// proxy support at all, so this seam refused a SOCKS-configured database
/// outright rather than let a node dial around the proxy the operator asked
/// for. Now the node dials through it, and the refusal is gone.
fn proxy_token(conn: &ConnectionArgs) -> Option<String> {
    match conn.connection {
        ConnectionMode::Direct => None,
        ConnectionMode::SocksProxy(addr) => Some(format!("socks5://{addr}")),
    }
}

/// The receiver set for a database's shielded pool.
///
/// Ironwood is not a receiver: it is received at Orchard addresses, as an
/// Orchard V3 note, so an Ironwood database is an Orchard-receiver wallet. This
/// mirrors [`crate::internal::state::pool_output_codes`], which reads pool
/// codes 3 and 4 for the same reason.
fn receiver_set(pool: zcash_protocol::ShieldedPool) -> ReceiverSet {
    match pool {
        zcash_protocol::ShieldedPool::Sapling => ReceiverSet::single(Receiver::Sapling),
        _ => ReceiverSet::single(Receiver::Orchard),
    }
}

/// zecd's name for a network.
fn network_token(network: Network) -> &'static str {
    match network {
        Network::Main => "main",
        Network::Test => "test",
        Network::Regtest => "regtest",
    }
}

/// The upstream token for zecd's `[backend] server`.
///
/// zkv picks a concrete lightwalletd `host:port` for the network; zecd reads a
/// bare `host:port` as a lightwalletd endpoint whose TLS follows the same
/// locality heuristic zkv uses (plaintext to loopback, TLS otherwise), so the
/// endpoint carries over unchanged rather than through the `zecrocks` preset.
fn upstream_token(network: Network, conn: &ConnectionArgs) -> Result<String, EngineError> {
    let servers = match network {
        Network::Main => conn.mainnet_server.as_ref().unwrap_or(&conn.server),
        Network::Test => conn.testnet_server.as_ref().unwrap_or(&conn.server),
        Network::Regtest => &conn.server,
    };
    let server = servers.pick(network).map_err(EngineError::Other)?;
    Ok(server.to_string())
}

/// Make zecd's regtest chain agree with zkv's.
///
/// zkv compiles its regtest activation heights in, while zecd takes NU6.3's
/// from the environment. Disagreement is not a loud failure: transactions get
/// built for the wrong consensus branch, or Ironwood outputs silently do not
/// appear. So this sets the variable when it is unset, and refuses to run
/// against a conflicting value rather than quietly overriding what an operator
/// asked for.
///
/// zecd's other regtest heights (NU5/NU6 at 1, NU6.1/NU6.2 at 4) are compiled
/// into zecd and already match [`crate::network`]'s.
fn align_regtest_activation(network: Network) -> Result<(), EngineError> {
    if network != Network::Regtest {
        return Ok(());
    }
    let want = REGTEST_NU6_3_HEIGHT.to_string();
    match std::env::var(ZECD_REGTEST_NU63_HEIGHT) {
        Ok(found) if found.trim() == want => Ok(()),
        Ok(found) => Err(EngineError::Unsupported(format!(
            "{ZECD_REGTEST_NU63_HEIGHT} is set to '{found}', but this build's regtest chain \
             activates NU6.3 at {want}. Unset it, or set it to {want}, so the wallet engine \
             and zkv agree on the chain"
        ))),
        Err(_) => {
            // Safety: this runs during engine setup, before the node (and its
            // threads) exist, and every zkv binary reaches it from a single
            // startup path. It is also idempotent: a second call finds the
            // value already set and takes the match arm above.
            unsafe { std::env::set_var(ZECD_REGTEST_NU63_HEIGHT, &want) };
            Ok(())
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use zcash_protocol::ShieldedPool;

    #[test]
    fn pool_maps_to_its_receiver() {
        assert!(receiver_set(ShieldedPool::Sapling).contains(Receiver::Sapling));
        assert!(!receiver_set(ShieldedPool::Sapling).contains(Receiver::Orchard));
        // Ironwood rides the Orchard receiver, so both map to Orchard.
        assert!(receiver_set(ShieldedPool::Orchard).contains(Receiver::Orchard));
        assert!(receiver_set(ShieldedPool::Ironwood).contains(Receiver::Orchard));
        assert!(!receiver_set(ShieldedPool::Ironwood).contains(Receiver::Sapling));
    }

    #[test]
    fn network_tokens_are_the_ones_zecd_parses() {
        // zecd's ZNetwork::parse accepts these three spellings; a typo here
        // would surface only as a confusing config error at node start.
        for (network, token) in [
            (Network::Main, "main"),
            (Network::Test, "test"),
            (Network::Regtest, "regtest"),
        ] {
            assert_eq!(network_token(network), token);
            assert!(
                zecd::network::ZNetwork::parse(token).is_ok(),
                "zecd should parse '{token}'",
            );
        }
    }

    #[test]
    fn upstream_token_round_trips_through_zecd_backend_resolution() {
        // A custom endpoint reaches zecd as a bare host:port, which it reads
        // as lightwalletd. This is the shape the regtest harness passes.
        let conn = ConnectionArgs {
            server: crate::remote::Servers::parse("127.0.0.1:9067").unwrap(),
            mainnet_server: None,
            testnet_server: None,
            connection: ConnectionMode::Direct,
        };
        let token = upstream_token(Network::Regtest, &conn).unwrap();
        assert_eq!(token, "127.0.0.1:9067");
        assert!(zecd::backend::resolve(&token, zecd::network::regtest()).is_ok());
    }

    #[test]
    fn hosted_operators_resolve_to_endpoints_zecd_accepts() {
        // The default `zecrocks` operator resolves to a concrete host:port,
        // which must still parse on zecd's side.
        let conn = ConnectionArgs {
            server: crate::remote::Servers::parse("zecrocks").unwrap(),
            mainnet_server: None,
            testnet_server: None,
            connection: ConnectionMode::Direct,
        };
        for (network, znetwork) in [
            (Network::Main, zecd::network::ZNetwork::Main),
            (Network::Test, zecd::network::ZNetwork::Test),
        ] {
            let token = upstream_token(network, &conn).unwrap();
            assert!(
                zecd::backend::resolve(&token, znetwork).is_ok(),
                "zecd should accept '{token}'",
            );
        }
    }

    /// A SOCKS5 database used to be refused at this seam because the node had
    /// no way to honour the proxy. Now the proxy is handed to the node in the
    /// spelling it parses, and the resolved configuration carries it, so the
    /// node's own dial goes through the proxy the operator asked for. Both
    /// address families, since zecd re-brackets an IPv6 literal on its side.
    #[test]
    fn a_socks_proxy_reaches_the_node_in_the_spelling_it_parses() {
        for (addr, token) in [
            ("127.0.0.1:9050", "socks5://127.0.0.1:9050"),
            ("[::1]:9050", "socks5://[::1]:9050"),
        ] {
            let conn = ConnectionArgs {
                server: crate::remote::Servers::parse("zecrocks").unwrap(),
                mainnet_server: None,
                testnet_server: None,
                connection: ConnectionMode::SocksProxy(addr.parse().unwrap()),
            };
            assert_eq!(proxy_token(&conn).as_deref(), Some(token));

            let dir = tempfile::tempdir().unwrap();
            let app = app_config(dir.path(), &watch_config(dir.path()), &conn)
                .expect("a proxied connection is served, not refused");
            let proxy = app
                .backend
                .proxy
                .as_ref()
                .expect("the resolved node config must carry the proxy");
            assert_eq!(
                proxy.to_string(),
                token,
                "zecd's canonical rendering should be the token zkv produced",
            );
        }

        // The direct case is the one every other test relies on.
        let direct = ConnectionArgs {
            server: crate::remote::Servers::parse("zecrocks").unwrap(),
            mainnet_server: None,
            testnet_server: None,
            connection: ConnectionMode::Direct,
        };
        assert!(proxy_token(&direct).is_none());
        let dir = tempfile::tempdir().unwrap();
        let app = app_config(dir.path(), &watch_config(dir.path()), &direct).unwrap();
        assert!(app.backend.proxy.is_none());
    }

    /// A watch-only testnet `keys.toml` in `dir`, read back the way the
    /// engine reads one. The address is never dereferenced by `app_config`.
    fn watch_config(dir: &std::path::Path) -> WalletConfig {
        WalletConfig::init_watch_at(
            dir,
            zcash_protocol::consensus::BlockHeight::from_u32(3_000_000),
            Network::Test,
            "zkvtest1placeholder",
            ShieldedPool::Ironwood,
            crate::config::WalletEngine::Unset,
        )
        .unwrap();
        WalletConfig::read_at(dir).unwrap()
    }

    #[test]
    fn regtest_activation_is_set_when_unset_and_accepted_when_matching() {
        // Idempotent: the first call sets the variable, the second sees its
        // own value and agrees. (These run in one process, so this test owns
        // the variable for the regtest network.)
        align_regtest_activation(Network::Regtest).unwrap();
        assert_eq!(
            std::env::var(ZECD_REGTEST_NU63_HEIGHT).unwrap(),
            REGTEST_NU6_3_HEIGHT.to_string(),
        );
        align_regtest_activation(Network::Regtest).unwrap();

        // A non-regtest database never consults it.
        align_regtest_activation(Network::Main).unwrap();
    }

    /// zkv resolves `<db>/zec/lrz` itself, from constants in `crate::data`,
    /// because that module sits below this seam and may not name a `zecd::`
    /// path. This is what keeps the copy honest: the names are frozen upstream
    /// and pinned by a test there, so if one ever moves, it fails here, in the
    /// one module allowed to know about zecd, rather than silently sending
    /// reads to a directory that no longer holds the wallet.
    /// The floor zkv overrides the node's default with, per network.
    ///
    /// The testnet value is the one that matters: the node's 0.1 ZEC default
    /// is above a faucet-funded database's whole balance, which costs such a
    /// wallet its change notes and serializes its writes on
    /// `trusted_confirmations`. `target_note_count` is pinned too, since the
    /// floor only means anything alongside it.
    #[test]
    fn the_change_split_floor_is_zkvs_own_per_network_policy() {
        assert_eq!(min_split_output_value(Network::Main), 500_000);
        assert_eq!(min_split_output_value(Network::Test), 50_000);
        assert_eq!(min_split_output_value(Network::Regtest), 50_000);
        assert!(
            min_split_output_value(Network::Test) < zecd::config::DEFAULT_MIN_SPLIT_OUTPUT_VALUE,
            "the point of overriding is that the node's default is too high for a \
             faucet-funded testnet database",
        );

        // Both values must survive upstream's validation, which is what turns
        // a bad setting into a refusal to start rather than a panic on the
        // first send.
        for net in [Network::Main, Network::Test, Network::Regtest] {
            let spend = zecd::config::SpendConfig {
                target_note_count: TARGET_NOTE_COUNT,
                min_split_output_value: min_split_output_value(net),
                ..Default::default()
            };
            spend
                .validate_change_splitting()
                .expect("zkv's own splitting policy must be one zecd accepts");
        }
    }

    /// Two upstream defaults zkv cannot afford to inherit silently, both
    /// stated at the seam and asserted here on the resolved configuration.
    ///
    /// `fetch_memos` is the dangerous one: with it off the node stops fetching
    /// the full transaction for anything the wallet only received in, and then
    /// reports no memos at all. A zkv database would read as empty while every
    /// health signal stayed green, because zkv's memos are precisely the data
    /// that cut discards. The fleet gate is the milder one: it decides whether
    /// a node reads a manifest directory under its datadir, and a per-database
    /// node's datadir is a user's database directory.
    #[test]
    fn the_resolved_config_always_fetches_memos_and_never_hosts_a_fleet() {
        let dir = tempfile::tempdir().unwrap();
        let conn = ConnectionArgs {
            server: crate::remote::Servers::parse("zecrocks").unwrap(),
            mainnet_server: None,
            testnet_server: None,
            connection: ConnectionMode::Direct,
        };
        let app = app_config(dir.path(), &watch_config(dir.path()), &conn).unwrap();
        assert!(
            app.sync.fetch_memos,
            "a zkv node that does not fetch memos reads every key as absent",
        );
        assert!(
            !app.fleet.enabled,
            "a per-database node must not enrol itself into a fleet",
        );

        // And the upstream default is still the safe one for the fleet gate:
        // this fails if a zecd bump flips it, which is worth hearing about even
        // though the line above already protects zkv.
        assert!(!zecd::config::FleetConfig::default().enabled);
    }

    /// The shared scan's configuration, and the four places zkv's own idea of
    /// the fleet layout has to agree with the node's.
    ///
    /// zkv reads shard files directly, because there is no supported way to ask
    /// a node where a member lives, and it writes manifests itself so a
    /// database can be enrolled without a node running. Both mean zkv is
    /// reimplementing an upstream layout, and upstream ships the fleet
    /// **experimental**: its manifest format and keys may change in a patch
    /// release. The dependency is pinned to an exact commit so that cannot
    /// happen unnoticed, and this is what says where to look when the pin
    /// moves.
    #[test]
    fn the_shared_scan_config_is_the_one_the_node_reads() {
        let dir = tempfile::tempdir().unwrap();
        let conn = ConnectionArgs {
            server: crate::remote::Servers::parse("zecrocks").unwrap(),
            mainnet_server: None,
            testnet_server: None,
            connection: ConnectionMode::Direct,
        };
        let app = fleet_app_config(dir.path(), Network::Test, &conn).unwrap();

        // The fleet is off by default upstream; the shared scan is the one
        // node zkv turns it on for.
        assert!(app.fleet.enabled);
        assert_eq!(app.fleet.manifest_dir, dir.path().join("wallets.d"));
        assert_eq!(app.fleet.dir, dir.path().join("shards"));
        assert_eq!(app.fleet.cohort_depth, FLEET_COHORT_DEPTH);

        // A shard holds whichever pools its members live in, so scanning only
        // the node's default (Orchard) would leave a Sapling member empty.
        assert!(app.pools.enabled.contains(Receiver::Sapling));
        assert!(app.pools.enabled.contains(Receiver::Orchard));
        // Memos are the product here too; nothing in the fleet can spend.
        assert!(app.sync.fetch_memos);
        assert!(!app.spend.cache_proving_key);
        assert_eq!(app.spend.shutdown_drain_secs, 0);
    }

    /// A manifest zkv wrote has to be one the node serves: same directory, same
    /// two fields, same name-is-the-file-stem rule. If upstream changes the
    /// format, this fails here rather than as a member that silently never gets
    /// scanned.
    #[test]
    fn a_manifest_zkv_writes_is_one_the_node_reads() {
        let dir = tempfile::tempdir().unwrap();
        let manifest = crate::fleet::Manifest {
            ufvk: "uviewtest1example".to_owned(),
            birthday: 3_000_000,
        };
        crate::fleet::write_manifest_in(dir.path(), "watch-a", &manifest).unwrap();
        // A leftover temporary must be ignored rather than read as a member.
        std::fs::write(dir.path().join("watch-b.toml.tmp"), "garbage").unwrap();

        let (members, skipped) = zecd::fleet::load_manifests(dir.path()).unwrap();
        assert!(skipped.is_empty(), "{skipped:?}");
        assert_eq!(members.len(), 1);
        assert_eq!(members[0].name, "watch-a");
        assert_eq!(members[0].ufvk, manifest.ufvk);
        assert_eq!(u32::from(members[0].birthday), manifest.birthday);
    }

    /// zkv finds a member's account by opening `<shard>/data.sqlite` itself, so
    /// both halves of that path are upstream's to change.
    #[test]
    fn the_shard_layout_is_the_one_the_node_lays_down() {
        assert_eq!(
            crate::fleet::shard_dir_name(3),
            zecd::wallet::shard::shard_dir_name(3),
        );
        // Where a shard's wallet database actually is. The previous version of
        // this compared `data_db_path(dir)` with `dir.join(DATA_DB)`, which is
        // that function's definition: it could not fail, and it did not, while
        // zkv read `<shard>/data.sqlite` and the node wrote `<shard>/lrz/`.
        // Every fleet read reported its member as never imported. So compose
        // the path the way each side really does, from the shard directory.
        let shard = std::path::Path::new("/tmp/zkv-shard-path-check/shard-0000");
        assert_eq!(
            crate::fleet::shard_engine_dir(shard).join(crate::data::DATA_DB),
            zecd::wallet::open::data_db_path(&zecd::config::shard_engine_dir(
                shard,
                zecd::coin::Coin::Zcash,
            )),
        );
        assert_ne!(
            crate::fleet::shard_engine_dir(shard),
            shard.to_path_buf(),
            "a shard's wallet files are a level below the shard directory",
        );
    }

    #[test]
    fn the_layout_constants_match_the_ones_zecd_uses() {
        use zecd::coin::Coin;

        assert_eq!(crate::data::ENGINE_COIN_DIR, Coin::Zcash.data_dir());
        assert_eq!(crate::data::ENGINE_STORAGE_DIR, Coin::Zcash.engine_dir());
    }

    /// The engine's node locks the same file zkv's own `DbLock` uses, and that
    /// collision is load-bearing in both directions: it is what makes the two
    /// exclude each other across processes, and it is why nothing may hold
    /// zkv's lock across a call that starts a node. `flock` is not reentrant
    /// across separate handles and zkv's reentrancy registry does not know
    /// about zecd's, so the node would fail to take what its own caller holds.
    /// A GUI regression of exactly that shape once reached CI green, because
    /// nothing in CI drives the GUI.
    ///
    /// zecd names that path now (`lock::datadir_lock_path`), so this asserts
    /// the real thing rather than restating zkv's own constant: if the two
    /// ever diverge they stop excluding each other, and two processes scanning
    /// and spending one wallet is silent corruption rather than a visible
    /// failure.
    #[test]
    fn zkv_locks_the_file_the_node_also_locks() {
        let dir = std::path::Path::new("/tmp/zkv-lock-path-check");
        assert_eq!(
            zecd::lock::datadir_lock_path(dir),
            dir.join(crate::data::LOCK_FILE),
            "the node locks `<datadir>/.lock`; zkv must name the same file",
        );
    }
}
