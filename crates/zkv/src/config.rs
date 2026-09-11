//! Per-database config: `~/.zkv/<name>/keys.toml` + `~/.zkv/<name>/security-theater-key`.
//!
//! `keys.toml` holds the database metadata and, for admin databases, the seed
//! mnemonic. The `security-theater-key` file is an auto-generated age identity
//! used to wrap that mnemonic on disk; the user never sees it, and their real
//! backup is the 24-word phrase shown during `zkv init`. Databases created
//! before the rename named this file `.id`; it is read and migrated forward to
//! the current name on first access (see `identity_path`), so the change is
//! backwards compatible.
//!
//! IMPORTANT: the on-disk wrapping is NOT a meaningful at-rest security
//! boundary, and the file name now says so. The key sits in the same directory
//! as the wrapped seed, so anything that can read the database directory can
//! recover the seed. On-disk protection therefore reduces to filesystem
//! permissions: the secret files are created `0600` and the database directory
//! `0700` on Unix (see `create_private_file` and `data.rs`).
//!
//! A passphrase-derived key that would make the stored seed independently
//! secret is future work. When that lands (passworded wallets), the wrapping
//! becomes a real at-rest boundary and this file will be renamed again to a
//! name that no longer calls itself security theater.

use std::fs::{self, File};
use std::io::{BufReader, Read, Write};
use std::path::{Path, PathBuf};

use age::secrecy::ExposeSecret as _;
use anyhow::anyhow;
use bip0039::{English, Mnemonic};
use secrecy::{ExposeSecret, SecretVec, Zeroize};
use serde::{Deserialize, Serialize};

use zcash_protocol::consensus::{BlockHeight, NetworkUpgrade, Parameters};
use zcash_protocol::ShieldedPool;

use crate::{
    data::{db_dir, ensure_db_dir, Network},
    error,
};

const KEYS_FILE: &str = "keys.toml";
/// Current name of the age identity file that wraps the seed. Named for what
/// it is: with the key sitting next to the ciphertext, the wrapping is not an
/// at-rest security boundary (see the module docs). When passworded wallets
/// land and the wrapping becomes a real boundary, this is renamed again.
const IDENTITY_FILE: &str = "security-theater-key";
/// Legacy name for the identity file. Databases created before the rename
/// wrote `.id`; `identity_path` reads it and renames it forward to
/// [`IDENTITY_FILE`] on first access, so old databases keep working.
const LEGACY_IDENTITY_FILE: &str = ".id";

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Role {
    /// Owns a seed; can sign and broadcast SET/DEL.
    Admin,
    /// View-only (UFVK imported via `zkv watch`).
    Watch,
}

pub struct WalletConfig {
    pub network: Network,
    pub role: Role,
    pub birthday: BlockHeight,
    /// For Watch databases, the original zkv address used at `zkv watch`
    /// time. Persisted so the wallet can be re-bootstrapped from
    /// `keys.toml` after a sidecar wipe (e.g. recovery from an
    /// unrecoverable reorg). The UFVK it contains is non-secret;
    /// the address is the public identifier of the database.
    pub zkv_address: Option<String>,
    /// The single shielded pool this database lives in: every memo is read
    /// from, and written to, this pool. Chosen at creation and fixed
    /// thereafter. Absent in `keys.toml` (legacy databases) means Orchard.
    pub pool: ShieldedPool,
    /// This database's Unified Full Viewing Key, in its encoded `uview…`
    /// form, pinned so the wallet engine can check that `data.sqlite` still
    /// holds the account this `keys.toml` describes.
    ///
    /// Absent means the database has not been adopted by the engine yet; see
    /// [`crate::engine::migrate`], which derives it from the seed (or, for a
    /// watch-only database, from the stored address) and writes it here.
    ///
    /// Nothing secret: a viewing key is the public identifier of the database,
    /// and the `zkv1…` address is the same key under a different label.
    pub ufvk: Option<String>,
    /// Which wallet engine serves this database; see [`WalletEngine`].
    pub engine: WalletEngine,
    /// The cached shard directory name for a fleet member (`shard-0000`).
    /// A hint only, refreshed by `crate::fleet::locate_member`.
    shard: Option<String>,
    seed_ciphertext: Option<String>,
    db_dir: PathBuf,
}

/// Parse a `--pool` value into a [`ShieldedPool`]: `"ironwood"`, `"orchard"`,
/// or `"sapling"`. Ironwood and Orchard share the Orchard receiver and are
/// chain-identical; which one a new database uses is a per-network policy
/// (see [`default_pool_for_network`] / [`ironwood_available`]): Ironwood is
/// the default on every network now that NU6.3 is active on mainnet. The
/// `String` error feeds clap; network validation happens at the creation call
/// site, not here.
pub fn parse_pool(s: &str) -> Result<ShieldedPool, String> {
    match s.trim().to_ascii_lowercase().as_str() {
        "sapling" => Ok(ShieldedPool::Sapling),
        "orchard" => Ok(ShieldedPool::Orchard),
        "ironwood" => Ok(ShieldedPool::Ironwood),
        other => Err(format!(
            "unknown pool {other:?} (expected \"ironwood\", \"orchard\", or \"sapling\")"
        )),
    }
}

/// Whether the Ironwood (NU6.3) pool can back a database on `network`. NU6.3
/// activated on mainnet at height 3_428_143 (2026-07-28), so Ironwood is now
/// live on every network (mainnet, testnet, and regtest). Existing Orchard
/// databases keep working unchanged: Ironwood and Orchard share the Orchard
/// receiver, and the tx builder picks the V6 Ironwood bundle by chain height.
pub fn ironwood_available(_network: Network) -> bool {
    true
}

/// The default shielded pool for a new database on `network`: Ironwood (the
/// NU6.3 Orchard pool) on every network.
pub fn default_pool_for_network(network: Network) -> ShieldedPool {
    if ironwood_available(network) {
        ShieldedPool::Ironwood
    } else {
        ShieldedPool::Orchard
    }
}

/// Resolve the pool for a database being **imported** (`zkv restore`, watch
/// databases): fall back to the network default when unspecified, and reject
/// Ironwood on a network where it isn't available (none today; the guard stays
/// so the policy lives in one place). Orchard is accepted here so existing
/// Orchard wallets load unchanged; brand-new databases go through
/// [`resolve_pool_for_new_database`], which additionally rejects it.
pub fn resolve_pool_for_network(
    pool: Option<ShieldedPool>,
    network: Network,
) -> anyhow::Result<ShieldedPool> {
    let pool = pool.unwrap_or_else(|| default_pool_for_network(network));
    if pool == ShieldedPool::Ironwood && !ironwood_available(network) {
        anyhow::bail!(
            "the Ironwood pool is not available on this network; create the database \
             with `--pool orchard` instead"
        );
    }
    Ok(pool)
}

/// Resolve the pool for a **brand-new** database (`zkv init`, the GUI create
/// flow, the facade's `init_admin`): like [`resolve_pool_for_network`], but
/// additionally rejects Orchard. Orchard is the legacy label for the same
/// chain pool as Ironwood (identical receiver), so new databases take
/// Ironwood; Orchard stays accepted on the import paths only (`zkv restore`,
/// watch databases), where it must match the pool the wallet was originally
/// created with.
pub fn resolve_pool_for_new_database(
    pool: Option<ShieldedPool>,
    network: Network,
) -> anyhow::Result<ShieldedPool> {
    let pool = resolve_pool_for_network(pool, network)?;
    if pool == ShieldedPool::Orchard {
        anyhow::bail!(
            "new databases cannot use the legacy Orchard pool; use `--pool ironwood` \
             (the same pool under NU6.3, identical receiver). Orchard remains available \
             when importing an existing wallet with `zkv restore`"
        );
    }
    Ok(pool)
}

/// Lowercase label for a pool, as written to `keys.toml` and surfaced to the
/// GUI. Ironwood and Orchard are distinct labels (Ironwood is new; legacy
/// databases stay labelled `"orchard"`) even though they share the Orchard
/// receiver and are handled identically on the chain.
pub fn pool_label(pool: ShieldedPool) -> &'static str {
    match pool {
        ShieldedPool::Sapling => "sapling",
        ShieldedPool::Orchard => "orchard",
        ShieldedPool::Ironwood => "ironwood",
    }
}

/// Parse a `keys.toml` pool label. `"ironwood"` is the pool for databases
/// created since NU6.3; `"orchard"` and an absent label are the legacy Orchard
/// form (kept as-is so existing databases open unchanged, and handled
/// identically to Ironwood since they share the Orchard receiver).
fn pool_from_label(label: Option<&str>) -> ShieldedPool {
    match label {
        Some("sapling") => ShieldedPool::Sapling,
        Some("ironwood") => ShieldedPool::Ironwood,
        // `Some("orchard")` and `None` (pre-pool-field databases) are legacy
        // Orchard, behaviourally identical to Ironwood.
        _ => ShieldedPool::Orchard,
    }
}

impl WalletConfig {
    /// Create an admin database: generate the `security-theater-key` age
    /// identity, store the seed mnemonic wrapped under it, and save the config.
    /// See the module docs: the wrapping is not an at-rest security boundary;
    /// the file permissions are.
    pub fn init_admin(
        db_name: &str,
        mnemonic: &Mnemonic,
        birthday: BlockHeight,
        network: Network,
        pool: ShieldedPool,
    ) -> anyhow::Result<()> {
        let dir = ensure_db_dir(db_name)?;
        Self::init_admin_at(&dir, mnemonic, birthday, network, pool)
    }

    /// [`WalletConfig::init_admin`] against an explicit directory, rather than
    /// resolving one from the process-wide data directory. The name-taking
    /// form is the normal entry point; this one exists so the config layer can
    /// be exercised without reaching for global state.
    pub(crate) fn init_admin_at(
        dir: &Path,
        mnemonic: &Mnemonic,
        birthday: BlockHeight,
        network: Network,
        pool: ShieldedPool,
    ) -> anyhow::Result<()> {
        std::fs::create_dir_all(dir)?;

        // Generate the fresh age identity (the `security-theater-key` file).
        let identity = age::x25519::Identity::generate();
        write_identity(dir, &identity)?;

        // Wrap the mnemonic under the identity (obfuscation only, not a
        // security boundary; the protection is the file permissions, see the
        // module docs).
        let recipient = identity.to_public();
        let recipients: Vec<Box<dyn age::Recipient>> = vec![Box::new(recipient)];
        let ciphertext = encrypt_mnemonic(recipients.iter().map(|r| r.as_ref() as _), mnemonic)?;

        write_config(
            dir,
            ConfigEncoding {
                mnemonic: Some(ciphertext),
                network: Some(network.name().to_string()),
                birthday: Some(u32::from(birthday)),
                role: Some("admin".to_owned()),
                zkv_address: None,
                pool: pool_encoding(pool),
                // Filled in when the wallet engine first opens the database,
                // so that pinning has exactly one implementation rather than
                // one here and another for databases that predate the field.
                ufvk: None,
                // An admin database always has a node of its own: it holds a
                // spending key, and the fleet is watch-only.
                engine: None,
                shard: None,
            },
        )
    }

    /// Create a watch-only database. The zkv address is persisted so
    /// recovery flows can re-import the UFVK without user interaction.
    pub fn init_watch(
        db_name: &str,
        birthday: BlockHeight,
        network: Network,
        zkv_address: &str,
        pool: ShieldedPool,
        engine: WalletEngine,
    ) -> anyhow::Result<()> {
        let dir = ensure_db_dir(db_name)?;
        Self::init_watch_at(&dir, birthday, network, zkv_address, pool, engine)
    }

    /// [`WalletConfig::init_watch`] against an explicit directory; see
    /// [`WalletConfig::init_admin_at`].
    pub(crate) fn init_watch_at(
        dir: &Path,
        birthday: BlockHeight,
        network: Network,
        zkv_address: &str,
        pool: ShieldedPool,
        engine: WalletEngine,
    ) -> anyhow::Result<()> {
        std::fs::create_dir_all(dir)?;
        // A fleet member has no `data.sqlite` of its own to derive the viewing
        // key from later, and the resolver needs it to find the member's
        // account in a shard, so it is pinned at creation from the address.
        // For an own-node database this stays absent and adoption fills it in.
        let ufvk = if engine.is_standalone() {
            None
        } else {
            Some(
                crate::protocol::parse_zkv_addr(zkv_address)?
                    .ufvk
                    .encode(&network),
            )
        };
        write_config(
            dir,
            ConfigEncoding {
                mnemonic: None,
                network: Some(network.name().to_string()),
                birthday: Some(u32::from(birthday)),
                role: Some("watch".to_owned()),
                zkv_address: Some(zkv_address.to_owned()),
                pool: pool_encoding(pool),
                // See `init_admin`: the engine pins this on first open.
                ufvk,
                engine: engine.label().map(str::to_owned),
                shard: None,
            },
        )
    }

    /// Record which engine serves this database, preserving every other field.
    /// The transition it drives is described in `crate::fleet`.
    pub fn set_engine(&mut self, engine: WalletEngine) -> anyhow::Result<()> {
        self.engine = engine;
        if engine.is_standalone() {
            // Placement is meaningless once the database is off the fleet, and
            // a stale hint would send a later rejoin at the wrong shard first.
            self.shard = None;
        }
        rewrite_config(&self.db_dir, self.to_encoding())
    }

    /// The cached shard directory name, if this member has been located before.
    pub fn shard(&self) -> Option<&str> {
        self.shard.as_deref()
    }

    /// Drop the remembered shard, after something moved every member's account
    /// (a rebuild). The next read re-derives it.
    pub fn forget_shard(&mut self) -> anyhow::Result<()> {
        if self.shard.is_none() {
            return Ok(());
        }
        self.shard = None;
        rewrite_config(&self.db_dir, self.to_encoding())
    }

    /// Remember where this member's account was found. A hint, so a failure to
    /// persist it is not worth failing the read that produced it.
    pub fn cache_shard(&mut self, shard: &str) -> anyhow::Result<()> {
        if self.shard.as_deref() == Some(shard) {
            return Ok(());
        }
        self.shard = Some(shard.to_owned());
        rewrite_config(&self.db_dir, self.to_encoding())
    }

    /// Read the config for an existing database by name.
    pub fn read(db_name: &str) -> anyhow::Result<Self> {
        let dir = db_dir(db_name)?;
        if !dir.join(KEYS_FILE).exists() {
            anyhow::bail!(
                "no database named {db_name:?} (no keys.toml found in {})",
                dir.display()
            );
        }
        Self::read_at(&dir)
    }

    /// [`WalletConfig::read`] against an explicit directory; see
    /// [`WalletConfig::init_admin_at`].
    pub(crate) fn read_at(dir: &Path) -> anyhow::Result<Self> {
        let dir = dir.to_path_buf();
        let path = dir.join(KEYS_FILE);
        let mut buf = String::new();
        BufReader::new(File::open(&path)?).read_to_string(&mut buf)?;
        let cfg: ConfigEncoding = toml::from_str(&buf)?;

        let network = cfg.network.map_or_else(
            || Ok(Network::Main),
            |n| Network::parse(n.trim()).map_err(|_| error::Error::InvalidKeysFile),
        )?;

        let birthday = cfg.birthday.map(BlockHeight::from).unwrap_or_else(|| {
            network
                .activation_height(NetworkUpgrade::Sapling)
                .expect("Sapling activation height known")
        });

        let role = match cfg.role.as_deref() {
            Some("watch") => Role::Watch,
            // Legacy or unset: infer from the presence of a stored mnemonic.
            None if cfg.mnemonic.is_none() => Role::Watch,
            _ => Role::Admin,
        };

        // Legacy or unset pool means Orchard, matching pre-pool-field databases.
        let pool = pool_from_label(cfg.pool.as_deref());

        let engine = WalletEngine::from_label(cfg.engine.as_deref());
        // A fleet member is watch-only with a known viewing key, both by
        // construction upstream (the fleet serves UFVKs, never seeds) and
        // because the resolver finds its account by matching that key. Refuse
        // rather than resolve: a hand-edited file that claims otherwise would
        // otherwise send reads to whichever account came first in a shard.
        if !engine.is_standalone() {
            if role == Role::Admin {
                anyhow::bail!(
                    "{}: an admin database cannot be a fleet member (the fleet is \
                     watch-only); remove the `engine` line to serve it from its own node",
                    path.display(),
                );
            }
            if cfg.ufvk.is_none() && cfg.zkv_address.is_none() {
                anyhow::bail!(
                    "{}: a fleet member needs its viewing key (`ufvk`) or its `zkv_address` \
                     to find its account in a shard",
                    path.display(),
                );
            }
        }

        Ok(Self {
            network,
            role,
            birthday,
            zkv_address: cfg.zkv_address,
            pool,
            ufvk: cfg.ufvk,
            engine,
            shard: cfg.shard,
            seed_ciphertext: cfg.mnemonic,
            db_dir: dir,
        })
    }

    /// A config that describes no database, for the shared scan's node.
    ///
    /// A fleet node serves many databases and holds none: it needs a network
    /// (which it carries separately) and nothing else off this struct. Every
    /// other field is deliberately the emptiest value that cannot be mistaken
    /// for real: watch-only, no address, no key, no seed, no directory.
    pub(crate) fn placeholder(network: Network) -> Self {
        Self {
            network,
            role: Role::Watch,
            birthday: network
                .activation_height(NetworkUpgrade::Sapling)
                .expect("Sapling activation height known"),
            zkv_address: None,
            pool: ShieldedPool::Orchard,
            ufvk: None,
            engine: WalletEngine::Unset,
            shard: None,
            seed_ciphertext: None,
            db_dir: PathBuf::new(),
        }
    }

    /// The directory this config was read from, which is also where the
    /// wallet database, the snapshot and the age identity live.
    pub fn db_dir(&self) -> &Path {
        &self.db_dir
    }

    /// This config as it is written to `keys.toml`.
    ///
    /// Every field round-trips, so rewriting a config that was read back
    /// cannot silently drop one. That matters because the file has two
    /// readers now: zkv, and the wallet engine's zecd node, whose own writer
    /// keeps only the fields it knows about. zkv writing the file itself is
    /// what keeps `role`, `pool` and `zkv_address` alive.
    fn to_encoding(&self) -> ConfigEncoding {
        ConfigEncoding {
            mnemonic: self.seed_ciphertext.clone(),
            network: Some(self.network.name().to_string()),
            birthday: Some(u32::from(self.birthday)),
            role: Some(
                match self.role {
                    Role::Admin => "admin",
                    Role::Watch => "watch",
                }
                .to_owned(),
            ),
            zkv_address: self.zkv_address.clone(),
            pool: pool_encoding(self.pool),
            ufvk: self.ufvk.clone(),
            engine: self.engine.label().map(str::to_owned),
            shard: self.shard.clone(),
        }
    }

    /// Record this database's viewing key in `keys.toml`, preserving every
    /// other field.
    ///
    /// Writing it from zkv, rather than letting the engine's node fill it in
    /// on first open, is deliberate: zecd pins the key it finds if the field
    /// is empty, and its writer rewrites the file through a struct that has no
    /// `role`, `pool` or `zkv_address`, so those would be dropped. Getting
    /// there first means the field is already populated and that path never
    /// runs.
    pub fn pin_ufvk(&mut self, ufvk: &str) -> anyhow::Result<()> {
        self.ufvk = Some(ufvk.to_owned());
        rewrite_config(&self.db_dir, self.to_encoding())
    }

    /// Read the age identity and unwrap the stored seed.
    pub fn decrypt_seed(&self) -> anyhow::Result<SecretVec<u8>> {
        let ciphertext = self
            .seed_ciphertext
            .as_deref()
            .ok_or_else(|| anyhow!("this is a watch-only database; no seed to decrypt"))?;
        let identity = read_identity(&self.db_dir)?;
        let identities: Vec<Box<dyn age::Identity>> = vec![Box::new(identity)];
        decrypt_seed_from_ciphertext(identities.iter().map(|i| i.as_ref() as _), ciphertext)
    }

    /// Read the age identity and unwrap the stored seed back into its
    /// human-readable BIP-39 mnemonic: the same 24 words shown at `zkv init`.
    /// Errors for a watch-only database (no seed to decrypt). The caller owns
    /// the returned secret and must handle it with care.
    pub fn decrypt_mnemonic_phrase(&self) -> anyhow::Result<String> {
        let ciphertext = self
            .seed_ciphertext
            .as_deref()
            .ok_or_else(|| anyhow!("this is a watch-only database; no seed to decrypt"))?;
        let identity = read_identity(&self.db_dir)?;
        let identities: Vec<Box<dyn age::Identity>> = vec![Box::new(identity)];
        let bytes = decrypt_mnemonic(identities.iter().map(|i| i.as_ref() as _), ciphertext)?;
        Ok(std::str::from_utf8(bytes.expose_secret())?.to_owned())
    }
}

#[derive(Deserialize, Serialize)]
struct ConfigEncoding {
    mnemonic: Option<String>,
    network: Option<String>,
    birthday: Option<u32>,
    role: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    zkv_address: Option<String>,
    /// `"sapling"` or `"orchard"`; absent means Orchard (legacy databases).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pool: Option<String>,
    /// The pinned `uview…` viewing key; absent until the wallet engine adopts
    /// the database. Shares its name and encoding with the field zecd reads,
    /// so one file satisfies both readers.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    ufvk: Option<String>,
    /// Which wallet engine serves this database: absent (or anything
    /// unrecognised) means its own per-database node, `"fleet"` means it is a
    /// member of the shared per-network fleet, and `"fleet-pending"` means it
    /// is converting (see [`WalletEngine`]).
    ///
    /// Deliberately *not* under `deny_unknown_fields`, and deliberately
    /// meaningless to a pre-fleet build: an older zkv reading a member's
    /// `keys.toml` ignores this, looks for the wallet files that are no longer
    /// there, and reports the database as having no key imported. That is the
    /// documented downgrade boundary, and `zkv fleet leave` from a current
    /// build undoes it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    engine: Option<String>,
    /// Which shard directory currently holds this member's account, cached so
    /// a read does not have to open every shard to find it. Purely a hint: it
    /// is re-derived whenever it does not hold, and it is never authoritative
    /// (the shard databases are, exactly as upstream keeps placement out of
    /// its manifests).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    shard: Option<String>,
}

/// Which wallet engine serves a database.
///
/// Only watch-only databases can be anything but [`WalletEngine::Own`]: the
/// fleet is watch-only by construction upstream, and an admin database needs
/// its own node to spend from anyway.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum WalletEngine {
    /// No choice recorded, which is every database created before the shared
    /// scan existed. Served from its own node exactly like [`WalletEngine::Own`],
    /// and the one state a watch-only database is enrolled *out of*
    /// automatically: an absent field means nobody has decided, where `own`
    /// means somebody decided against.
    Unset,
    /// A node of this database's own, with the database directory as its
    /// datadir. Every admin database, and any watch-only database that opted
    /// out with `--standalone`.
    Own,
    /// Converting from [`WalletEngine::Own`] to [`WalletEngine::Fleet`]: the
    /// manifest is written and the shard is catching up, while reads and syncs
    /// still use this database's own files, so it never goes dark. See
    /// `crate::fleet` for the transition.
    FleetPending,
    /// A member of the shared per-network fleet: no wallet files of its own,
    /// its account living in a shard database that many members share.
    Fleet,
}

impl WalletEngine {
    fn label(self) -> Option<&'static str> {
        match self {
            WalletEngine::Unset => None,
            WalletEngine::Own => Some("own"),
            WalletEngine::FleetPending => Some("fleet-pending"),
            WalletEngine::Fleet => Some("fleet"),
        }
    }

    fn from_label(label: Option<&str>) -> WalletEngine {
        match label {
            None => WalletEngine::Unset,
            Some("fleet") => WalletEngine::Fleet,
            Some("fleet-pending") => WalletEngine::FleetPending,
            // `own`, or a spelling from a future build. Both mean "serve it
            // from its own node", which is the mode whose files are where zkv
            // has always kept them, and both mean a choice was recorded, so
            // neither is enrolled automatically.
            Some(_) => WalletEngine::Own,
        }
    }

    /// Whether the database's wallet files live in a shard rather than in its
    /// own directory. False while pending, which is the point of that state.
    pub fn is_fleet_member(self) -> bool {
        matches!(self, WalletEngine::Fleet)
    }

    /// Whether this database has anything to do with the shared scan at all.
    pub fn is_standalone(self) -> bool {
        matches!(self, WalletEngine::Unset | WalletEngine::Own)
    }

    /// Whether nobody has yet chosen how this database is served, which is the
    /// only state automatic enrolment acts on.
    pub fn is_unset(self) -> bool {
        matches!(self, WalletEngine::Unset)
    }
}

/// `keys.toml` encoding for a pool. Orchard is the implied default and is
/// omitted, so newly-created Orchard databases keep byte-identical config to
/// pre-pool-field databases.
fn pool_encoding(pool: ShieldedPool) -> Option<String> {
    match pool {
        // Orchard is the implied default: omit it so legacy databases keep
        // byte-identical config.
        ShieldedPool::Orchard => None,
        ShieldedPool::Sapling => Some(pool_label(pool).to_owned()),
        // Ironwood (the default for databases created since NU6.3) is written
        // explicitly; absence still means legacy Orchard.
        ShieldedPool::Ironwood => Some(pool_label(pool).to_owned()),
    }
}

/// Create a new file for writing with owner-only permissions where the platform
/// supports it.
///
/// On Unix the mode is applied atomically at creation (`0600`) via
/// `OpenOptionsExt`, so the secret is never even briefly group/world-readable
/// (no create-then-chmod TOCTOU window). On Windows the file inherits the parent
/// directory's ACL (the data dir lives under the per-user `%APPDATA%`);
/// tightening that further is future work and acceptable for the v0.0.1 alpha.
///
/// `create_new(true)` keeps the "never clobber an existing key file" guarantee.
fn create_private_file(path: &Path) -> std::io::Result<File> {
    let mut opts = fs::OpenOptions::new();
    opts.create_new(true).write(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt as _;
        opts.mode(0o600);
    }
    opts.open(path)
}

fn write_config(dir: &Path, cfg: ConfigEncoding) -> anyhow::Result<()> {
    let path = dir.join(KEYS_FILE);
    let mut f = create_private_file(&path)
        .map_err(|e| anyhow!("could not create {}: {e}", path.display()))?;
    let s = toml::to_string(&cfg)
        .map_err::<anyhow::Error, _>(|_| anyhow!("could not serialize config"))?;
    write!(f, "{s}")?;
    Ok(())
}

/// Replace an existing `keys.toml` with new contents.
///
/// [`write_config`] deliberately refuses to overwrite, so that creating a
/// database can never clobber one that is already there. Rewriting an existing
/// config is a different operation and needs to be safe in a different way:
/// the file holds the wrapped seed, so a half-written one is a lost wallet.
/// This writes a sibling temp file with the same `0600` mode and renames it
/// over the original, which is atomic within a directory, so a reader sees
/// either the old file or the new one.
fn rewrite_config(dir: &Path, cfg: ConfigEncoding) -> anyhow::Result<()> {
    let path = dir.join(KEYS_FILE);
    let tmp = dir.join(format!("{KEYS_FILE}.tmp{}", std::process::id()));
    let s = toml::to_string(&cfg)
        .map_err::<anyhow::Error, _>(|_| anyhow!("could not serialize config"))?;

    // A leftover temp file from a crashed rewrite would otherwise block this
    // one forever, and its contents are of no value: the real file is intact.
    let _ = fs::remove_file(&tmp);
    let mut f = create_private_file(&tmp)
        .map_err(|e| anyhow!("could not create {}: {e}", tmp.display()))?;
    write!(f, "{s}")?;
    f.sync_all()?;
    drop(f);

    fs::rename(&tmp, &path).map_err(|e| {
        let _ = fs::remove_file(&tmp);
        anyhow!("could not replace {}: {e}", path.display())
    })?;
    Ok(())
}

fn write_identity(dir: &Path, identity: &age::x25519::Identity) -> anyhow::Result<()> {
    let path = dir.join(IDENTITY_FILE);
    let mut f = create_private_file(&path)?;
    writeln!(f, "{}", identity.to_string().expose_secret())?;
    Ok(())
}

/// Resolve the path to the age identity file, migrating a legacy `.id` to the
/// current [`IDENTITY_FILE`] name when found.
///
/// Backwards compatibility for databases created before the rename: if only the
/// old `.id` exists, it is renamed forward to the current name (the rename
/// preserves the file's `0600` mode, since it is the same inode). The rename is
/// best-effort: if it fails (e.g. a read-only filesystem) the legacy path is
/// returned so the seed still decrypts. When neither file exists the current
/// path is returned so the read error names the file we now expect.
pub(crate) fn identity_path(dir: &Path) -> PathBuf {
    let current = dir.join(IDENTITY_FILE);
    if current.exists() {
        return current;
    }
    let legacy = dir.join(LEGACY_IDENTITY_FILE);
    if legacy.exists() {
        return match fs::rename(&legacy, &current) {
            Ok(()) => current,
            Err(_) => legacy,
        };
    }
    current
}

fn read_identity(dir: &Path) -> anyhow::Result<age::x25519::Identity> {
    let path = identity_path(dir);
    let s = fs::read_to_string(&path)
        .map_err(|e| anyhow!("could not read {} : {e}", path.display()))?;
    let line = s
        .lines()
        .find(|l| !l.starts_with('#') && !l.trim().is_empty());
    let key = line.ok_or_else(|| anyhow!("identity file is empty"))?;
    use std::str::FromStr;
    age::x25519::Identity::from_str(key.trim()).map_err(|e| anyhow!("invalid identity: {e}"))
}

fn encrypt_mnemonic<'a>(
    recipients: impl Iterator<Item = &'a dyn age::Recipient>,
    mnemonic: &Mnemonic,
) -> anyhow::Result<String> {
    let encryptor = age::Encryptor::with_recipients(recipients)?;
    let mut ciphertext = vec![];
    let mut writer = encryptor.wrap_output(age::armor::ArmoredWriter::wrap_output(
        &mut ciphertext,
        age::armor::Format::AsciiArmor,
    )?)?;
    writer.write_all(mnemonic.phrase().as_bytes())?;
    writer.finish().and_then(|armor| armor.finish())?;
    Ok(String::from_utf8(ciphertext).expect("armor is valid UTF-8"))
}

fn decrypt_mnemonic<'a>(
    identities: impl Iterator<Item = &'a dyn age::Identity>,
    ciphertext: &str,
) -> anyhow::Result<SecretVec<u8>> {
    let decryptor = age::Decryptor::new(age::armor::ArmoredReader::new(ciphertext.as_bytes()))?;
    let mut buf = vec![];
    let ret = decryptor.decrypt(identities)?.read_to_end(&mut buf);
    let res = SecretVec::new(buf);
    ret?;
    Ok(res)
}

fn decrypt_seed_from_ciphertext<'a>(
    identities: impl Iterator<Item = &'a dyn age::Identity>,
    ciphertext: &str,
) -> anyhow::Result<SecretVec<u8>> {
    let mnemonic_bytes = decrypt_mnemonic(identities, ciphertext)?;
    let mnemonic = std::str::from_utf8(mnemonic_bytes.expose_secret())?;
    let mut seed_bytes = <Mnemonic<English>>::from_phrase(mnemonic)?.to_seed("");
    let seed = SecretVec::new(seed_bytes.to_vec());
    seed_bytes.zeroize();
    Ok(seed)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_pool_accepts_known_values_case_insensitively() {
        // parse_pool is literal; the Ironwood/Orchard network policy is applied
        // separately (see resolve_pool_for_network).
        assert_eq!(parse_pool("orchard"), Ok(ShieldedPool::Orchard));
        assert_eq!(parse_pool("ironwood"), Ok(ShieldedPool::Ironwood));
        assert_eq!(parse_pool("sapling"), Ok(ShieldedPool::Sapling));
        assert_eq!(parse_pool("  Sapling "), Ok(ShieldedPool::Sapling));
        assert_eq!(parse_pool("ORCHARD"), Ok(ShieldedPool::Orchard));
        assert_eq!(parse_pool("Ironwood"), Ok(ShieldedPool::Ironwood));
        assert!(parse_pool("transparent").is_err());
        assert!(parse_pool("").is_err());
    }

    #[test]
    fn pool_network_policy() {
        use Network::{Main as MainNetwork, Regtest, Test as TestNetwork};
        // Ironwood is available on every network since NU6.3 activated on
        // mainnet (height 3_428_143, 2026-07-28).
        assert!(ironwood_available(MainNetwork));
        assert!(ironwood_available(TestNetwork));
        assert!(ironwood_available(Regtest));
        // Ironwood is the default everywhere.
        assert_eq!(
            default_pool_for_network(MainNetwork),
            ShieldedPool::Ironwood
        );
        assert_eq!(
            default_pool_for_network(TestNetwork),
            ShieldedPool::Ironwood
        );
        assert_eq!(default_pool_for_network(Regtest), ShieldedPool::Ironwood);
        // resolve_pool_for_network: unspecified falls back to the network
        // default (Ironwood everywhere); explicit Ironwood/Orchard/Sapling are
        // all accepted on every network.
        assert_eq!(
            resolve_pool_for_network(None, MainNetwork).unwrap(),
            ShieldedPool::Ironwood
        );
        assert_eq!(
            resolve_pool_for_network(None, TestNetwork).unwrap(),
            ShieldedPool::Ironwood
        );
        assert_eq!(
            resolve_pool_for_network(Some(ShieldedPool::Ironwood), MainNetwork).unwrap(),
            ShieldedPool::Ironwood
        );
        assert_eq!(
            resolve_pool_for_network(Some(ShieldedPool::Orchard), MainNetwork).unwrap(),
            ShieldedPool::Orchard
        );
        assert_eq!(
            resolve_pool_for_network(Some(ShieldedPool::Ironwood), TestNetwork).unwrap(),
            ShieldedPool::Ironwood
        );
        assert_eq!(
            resolve_pool_for_network(Some(ShieldedPool::Sapling), MainNetwork).unwrap(),
            ShieldedPool::Sapling
        );
        // resolve_pool_for_new_database: same fallback (Ironwood), but the
        // legacy Orchard label is import-only and rejected for creation;
        // Ironwood/Sapling stay creatable.
        assert_eq!(
            resolve_pool_for_new_database(None, MainNetwork).unwrap(),
            ShieldedPool::Ironwood
        );
        assert!(resolve_pool_for_new_database(Some(ShieldedPool::Orchard), MainNetwork).is_err());
        assert!(resolve_pool_for_new_database(Some(ShieldedPool::Orchard), TestNetwork).is_err());
        assert_eq!(
            resolve_pool_for_new_database(Some(ShieldedPool::Ironwood), MainNetwork).unwrap(),
            ShieldedPool::Ironwood
        );
        assert_eq!(
            resolve_pool_for_new_database(Some(ShieldedPool::Sapling), TestNetwork).unwrap(),
            ShieldedPool::Sapling
        );
    }

    #[test]
    fn absent_or_unknown_pool_label_defaults_to_orchard() {
        // Legacy keys.toml has no `pool` field at all: still legacy Orchard.
        assert_eq!(pool_from_label(None), ShieldedPool::Orchard);
        // Unknown labels fall back to Orchard rather than erroring on read.
        assert_eq!(pool_from_label(Some("bogus")), ShieldedPool::Orchard);
        assert_eq!(pool_from_label(Some("sapling")), ShieldedPool::Sapling);
        // An explicit legacy "orchard" label stays Orchard; new databases write
        // "ironwood".
        assert_eq!(pool_from_label(Some("orchard")), ShieldedPool::Orchard);
        assert_eq!(pool_from_label(Some("ironwood")), ShieldedPool::Ironwood);
    }

    #[test]
    fn pool_encoding_omits_orchard_and_round_trips() {
        // Orchard is omitted so legacy Orchard databases keep byte-identical
        // config to pre-pool-field ones.
        assert_eq!(pool_encoding(ShieldedPool::Orchard), None);
        assert_eq!(
            pool_encoding(ShieldedPool::Sapling),
            Some("sapling".to_owned())
        );
        // Ironwood is written explicitly.
        assert_eq!(
            pool_encoding(ShieldedPool::Ironwood),
            Some("ironwood".to_owned())
        );
        // Encoding then reading back is the identity on the pool.
        for pool in [
            ShieldedPool::Orchard,
            ShieldedPool::Sapling,
            ShieldedPool::Ironwood,
        ] {
            let label = pool_encoding(pool);
            assert_eq!(pool_from_label(label.as_deref()), pool);
        }
    }

    fn unique_temp_dir(tag: &str) -> PathBuf {
        use std::time::{SystemTime, UNIX_EPOCH};
        let nanos = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let dir = std::env::temp_dir().join(format!("zkv-{tag}-{}-{nanos}", std::process::id()));
        fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[test]
    fn read_identity_migrates_legacy_id_file() {
        let dir = unique_temp_dir("id-migrate");

        // Simulate a database created before the rename: the identity lives
        // under the legacy `.id` name only.
        let identity = age::x25519::Identity::generate();
        write_identity(&dir, &identity).unwrap();
        fs::rename(dir.join(IDENTITY_FILE), dir.join(LEGACY_IDENTITY_FILE)).unwrap();

        // Reading migrates `.id` forward to the current name and still yields
        // the same identity.
        let recovered = read_identity(&dir).unwrap();
        assert_eq!(
            recovered.to_public().to_string(),
            identity.to_public().to_string()
        );
        assert!(dir.join(IDENTITY_FILE).exists());
        assert!(!dir.join(LEGACY_IDENTITY_FILE).exists());

        fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn read_identity_prefers_current_name_over_legacy() {
        let dir = unique_temp_dir("id-current");

        // The current file holds the real identity; a stale legacy `.id` must
        // be ignored and left untouched (no clobber, no migration).
        let identity = age::x25519::Identity::generate();
        write_identity(&dir, &identity).unwrap();
        fs::write(dir.join(LEGACY_IDENTITY_FILE), "AGE-SECRET-KEY-stale\n").unwrap();

        let recovered = read_identity(&dir).unwrap();
        assert_eq!(
            recovered.to_public().to_string(),
            identity.to_public().to_string()
        );
        assert!(dir.join(LEGACY_IDENTITY_FILE).exists());

        fs::remove_dir_all(&dir).ok();
    }
}

#[cfg(test)]
mod engine_field_tests {
    use super::*;
    use zcash_protocol::consensus::BlockHeight;

    /// A real `zkvtest1…` address. Enrolling in the shared scan pins the
    /// viewing key out of the address, so this cannot be a placeholder.
    fn address() -> String {
        let ufvk = zcash_keys::keys::UnifiedSpendingKey::from_seed(
            &Network::Test,
            &[7u8; 64],
            zip32::AccountId::ZERO,
        )
        .unwrap()
        .to_unified_full_viewing_key();
        crate::protocol::encode_zkv_addr(&ufvk, &Network::Test, ShieldedPool::Ironwood, 3_000_000)
            .unwrap()
    }

    fn watch_dir(engine: WalletEngine) -> (tempfile::TempDir, WalletConfig, String) {
        let dir = tempfile::tempdir().unwrap();
        let addr = address();
        WalletConfig::init_watch_at(
            dir.path(),
            BlockHeight::from_u32(3_000_000),
            Network::Test,
            &addr,
            ShieldedPool::Ironwood,
            engine,
        )
        .unwrap();
        let cfg = WalletConfig::read_at(dir.path()).unwrap();
        (dir, cfg, addr)
    }

    /// The default is unchanged and unwritten: a database with a wallet engine
    /// of its own produces the same `keys.toml` it always did, so nothing about
    /// existing databases or older builds moves.
    #[test]
    fn an_unset_engine_writes_no_new_lines() {
        let (dir, cfg, _addr) = watch_dir(WalletEngine::Unset);
        assert_eq!(cfg.engine, WalletEngine::Unset);
        assert_eq!(cfg.shard(), None);
        let text = std::fs::read_to_string(dir.path().join(KEYS_FILE)).unwrap();
        assert!(!text.contains("engine"), "{text}");
        assert!(!text.contains("shard"), "{text}");
    }

    /// A file written before the field existed reads as its own engine, which
    /// is what every database on disk today is.
    #[test]
    fn a_pre_fleet_keys_file_reads_as_unset() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(
            dir.path().join(KEYS_FILE),
            "network = \"test\"\nbirthday = 100\nrole = \"watch\"\nzkv_address = \"z\"\n",
        )
        .unwrap();
        let cfg = WalletConfig::read_at(dir.path()).unwrap();
        assert_eq!(cfg.engine, WalletEngine::Unset);
        assert!(
            cfg.engine.is_unset(),
            "nobody has chosen for this database yet"
        );
    }

    /// An `engine` spelling from a future build must not be guessed at: serving
    /// the database from its own files is the reading whose data is where this
    /// build expects it.
    #[test]
    fn an_unknown_engine_spelling_falls_back_to_own() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(
            dir.path().join(KEYS_FILE),
            "network = \"test\"\nbirthday = 100\nrole = \"watch\"\n\
             zkv_address = \"z\"\nengine = \"warp-drive\"\n",
        )
        .unwrap();
        let cfg = WalletConfig::read_at(dir.path()).unwrap();
        assert_eq!(cfg.engine, WalletEngine::Own);
        // And it counts as a recorded choice, so a database is never enrolled
        // behind the user's back on the strength of not being understood.
        assert!(!cfg.engine.is_unset());
    }

    /// `--standalone` records a choice, and that is what stops the automatic
    /// enrolment treating the database as one nobody has decided about.
    #[test]
    fn an_explicit_own_engine_is_written_and_read_back() {
        let (dir, cfg, _addr) = watch_dir(WalletEngine::Own);
        assert_eq!(cfg.engine, WalletEngine::Own);
        assert!(!cfg.engine.is_unset());
        let text = std::fs::read_to_string(dir.path().join(KEYS_FILE)).unwrap();
        assert!(text.contains("engine = \"own\""), "{text}");
    }

    #[test]
    fn the_engine_and_shard_fields_round_trip() {
        let (dir, mut cfg, addr) = watch_dir(WalletEngine::Fleet);
        assert_eq!(cfg.engine, WalletEngine::Fleet);
        // A member pins its viewing key at creation: it has no wallet database
        // of its own to derive one from later, and the resolver needs it.
        assert!(cfg.ufvk.is_some());

        cfg.cache_shard("shard-0007").unwrap();
        let reread = WalletConfig::read_at(dir.path()).unwrap();
        assert_eq!(reread.engine, WalletEngine::Fleet);
        assert_eq!(reread.shard(), Some("shard-0007"));
        // Every other field survives the rewrite, which is the property that
        // matters: the file has two readers and the other one drops what it
        // does not know.
        assert_eq!(reread.role, Role::Watch);
        assert_eq!(reread.pool, ShieldedPool::Ironwood);
        assert_eq!(reread.zkv_address.as_deref(), Some(addr.as_str()));
        assert_eq!(reread.birthday, BlockHeight::from_u32(3_000_000));

        // Pending is a distinct state, and it is not a member: that is what
        // keeps a converting database reading from its own files.
        let mut cfg = reread;
        cfg.set_engine(WalletEngine::FleetPending).unwrap();
        let reread = WalletConfig::read_at(dir.path()).unwrap();
        assert_eq!(reread.engine, WalletEngine::FleetPending);
        assert!(!reread.engine.is_fleet_member());

        // Leaving drops the placement hint, so a later rejoin does not start by
        // looking in a shard this database is no longer in.
        let mut cfg = reread;
        cfg.set_engine(WalletEngine::Own).unwrap();
        let reread = WalletConfig::read_at(dir.path()).unwrap();
        assert_eq!(reread.engine, WalletEngine::Own);
        assert_eq!(reread.shard(), None);
    }

    /// The fleet is watch-only upstream, and the resolver finds a member's
    /// account by its viewing key. A file claiming otherwise is refused rather
    /// than resolved, because resolving it would hand reads whichever account
    /// happened to come first in a shard.
    #[test]
    fn an_incoherent_member_is_refused_rather_than_resolved() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(
            dir.path().join(KEYS_FILE),
            "network = \"test\"\nbirthday = 1\nrole = \"admin\"\nmnemonic = \"x\"\n\
             engine = \"fleet\"\n",
        )
        .unwrap();
        // `WalletConfig` has no `Debug` on purpose (it carries the wrapped
        // seed), so the error is taken by matching rather than by `unwrap_err`.
        let err = match WalletConfig::read_at(dir.path()) {
            Err(e) => e.to_string(),
            Ok(_) => panic!("an admin fleet member must be refused"),
        };
        assert!(err.contains("watch-only"), "{err}");

        let dir = tempfile::tempdir().unwrap();
        std::fs::write(
            dir.path().join(KEYS_FILE),
            "network = \"test\"\nbirthday = 1\nrole = \"watch\"\nengine = \"fleet\"\n",
        )
        .unwrap();
        let err = match WalletConfig::read_at(dir.path()) {
            Err(e) => e.to_string(),
            Ok(_) => panic!("a member with no viewing key must be refused"),
        };
        assert!(err.contains("viewing key"), "{err}");
    }
}
