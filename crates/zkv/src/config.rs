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

        // Generate the fresh age identity (the `security-theater-key` file).
        let identity = age::x25519::Identity::generate();
        write_identity(&dir, &identity)?;

        // Wrap the mnemonic under the identity (obfuscation only, not a
        // security boundary; the protection is the file permissions, see the
        // module docs).
        let recipient = identity.to_public();
        let recipients: Vec<Box<dyn age::Recipient>> = vec![Box::new(recipient)];
        let ciphertext = encrypt_mnemonic(recipients.iter().map(|r| r.as_ref() as _), mnemonic)?;

        write_config(
            &dir,
            ConfigEncoding {
                mnemonic: Some(ciphertext),
                network: Some(network.name().to_string()),
                birthday: Some(u32::from(birthday)),
                role: Some("admin".to_owned()),
                zkv_address: None,
                pool: pool_encoding(pool),
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
    ) -> anyhow::Result<()> {
        let dir = ensure_db_dir(db_name)?;
        write_config(
            &dir,
            ConfigEncoding {
                mnemonic: None,
                network: Some(network.name().to_string()),
                birthday: Some(u32::from(birthday)),
                role: Some("watch".to_owned()),
                zkv_address: Some(zkv_address.to_owned()),
                pool: pool_encoding(pool),
            },
        )
    }

    /// Read the config for an existing database by name.
    pub fn read(db_name: &str) -> anyhow::Result<Self> {
        let dir = db_dir(db_name)?;
        let path = dir.join(KEYS_FILE);
        if !path.exists() {
            anyhow::bail!(
                "no database named {db_name:?} (no keys.toml found in {})",
                dir.display()
            );
        }
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

        Ok(Self {
            network,
            role,
            birthday,
            zkv_address: cfg.zkv_address,
            pool,
            seed_ciphertext: cfg.mnemonic,
            db_dir: dir,
        })
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
fn identity_path(dir: &Path) -> PathBuf {
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
