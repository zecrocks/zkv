//! `zkv` — a Redis-style key-value store backed by Zcash Orchard memos.
//!
//! A "zkv address" identifies a database: `zkv1:<ufvk_bech32m>:<birthday_height>`.
//! Anyone with this string can read the database (after importing the UFVK and syncing).
//! Writes are signed with a secp256k1 key derived from the same wallet's transparent
//! account at a fixed BIP-44 path; readers verify by deriving the corresponding
//! pubkey from the UFVK's transparent component at the same path.

use std::collections::BTreeMap;
use std::str::FromStr;

use anyhow::{anyhow, bail};
use clap::Subcommand;
use rand::rngs::OsRng;
use secrecy::ExposeSecret;
use sha2::{Digest, Sha256};
use transparent::keys::{NonHardenedChildIndex, TransparentKeyScope};
use zcash_address::{
    unified::{self, Encoding},
    ZcashAddress,
};
use zcash_client_backend::data_api::Account;
use zcash_client_sqlite::{util::SystemClock, WalletDb};
use zcash_keys::keys::{UnifiedAddressRequest, UnifiedFullViewingKey, UnifiedSpendingKey};
use zcash_protocol::{
    consensus::{self, NetworkType, Parameters},
    memo::{Memo, MemoBytes},
    value::Zatoshis,
};
use zip321::{Payment, TransactionRequest};

use crate::{
    commands::{select_account, wallet::send::pay, wallet::send::PaymentContext},
    config::WalletConfig,
    data::get_db_paths,
};

pub(crate) mod address;
pub(crate) mod del;
pub(crate) mod get;
pub(crate) mod set;

/// Fixed BIP-44 scope for the zkv signing key (external).
pub(crate) const ZKV_TRANSPARENT_SCOPE: TransparentKeyScope = TransparentKeyScope::EXTERNAL;

/// Fixed BIP-44 address index for the zkv signing key.
pub(crate) const ZKV_TRANSPARENT_INDEX: u32 = 0;

/// Prefix for zkv address strings.
pub(crate) const ZKV_ADDR_PREFIX: &str = "zkv1:";

/// Magic prefix included in canonical signed bytes (separate from wire form).
const SIGNED_MAGIC: &[u8] = b"ZKV1";

/// Magic prefix on the wire (first token of a memo's first line).
const WIRE_MAGIC: &str = "ZKV1";

#[derive(Debug, Subcommand)]
pub(crate) enum Command {
    /// Print the zkv address for a local wallet account.
    Address(address::Command),

    /// Set a key to a value in a zkv database.
    Set(set::Command),

    /// Delete a key from a zkv database.
    Del(del::Command),

    /// Read the current state of a zkv database (one key, or all keys).
    Get(get::Command),
}

#[derive(Clone, Copy, Debug)]
pub(crate) enum Op {
    Set,
    Del,
}

impl Op {
    fn as_str(self) -> &'static str {
        match self {
            Op::Set => "SET",
            Op::Del => "DEL",
        }
    }

    fn parse(s: &str) -> Option<Self> {
        match s {
            "SET" => Some(Op::Set),
            "DEL" => Some(Op::Del),
            _ => None,
        }
    }
}

pub(crate) struct ParsedZkvAddr {
    pub(crate) raw: String,
    pub(crate) network: NetworkType,
    pub(crate) ufvk: UnifiedFullViewingKey,
    #[allow(dead_code)]
    pub(crate) birthday: u32,
}

/// `zkv1:<ufvk_bech32m>:<birthday>`
pub(crate) fn encode_zkv_addr(ufvk_str: &str, birthday: u32) -> String {
    format!("{ZKV_ADDR_PREFIX}{ufvk_str}:{birthday}")
}

pub(crate) fn parse_zkv_addr(s: &str) -> anyhow::Result<ParsedZkvAddr> {
    let body = s
        .strip_prefix(ZKV_ADDR_PREFIX)
        .ok_or_else(|| anyhow!("zkv address must start with `{ZKV_ADDR_PREFIX}`"))?;

    let (ufvk_str, birthday_str) = body
        .rsplit_once(':')
        .ok_or_else(|| anyhow!("zkv address must contain `:<birthday>` suffix"))?;

    let birthday: u32 = birthday_str
        .parse()
        .map_err(|e| anyhow!("invalid birthday in zkv address: {e}"))?;

    let (network, ufvk_parsed) = unified::Ufvk::decode(ufvk_str)
        .map_err(|e| anyhow!("invalid UFVK in zkv address: {e}"))?;
    let ufvk = UnifiedFullViewingKey::parse(&ufvk_parsed)
        .map_err(|e| anyhow!("could not parse UFVK: {e}"))?;

    if ufvk.transparent().is_none() {
        bail!("zkv requires a UFVK with a transparent component (signing key derives from t-account)");
    }
    if ufvk.orchard().is_none() {
        bail!("zkv requires a UFVK with an Orchard component (memos are delivered as Orchard notes)");
    }

    Ok(ParsedZkvAddr {
        raw: s.to_owned(),
        network,
        ufvk,
        birthday,
    })
}

/// Convert a parsed zkv address's `NetworkType` to a `consensus::Network`.
pub(crate) fn network_from_type(network: NetworkType) -> anyhow::Result<consensus::Network> {
    match network {
        NetworkType::Main => Ok(consensus::Network::MainNetwork),
        NetworkType::Test => Ok(consensus::Network::TestNetwork),
        NetworkType::Regtest => Err(anyhow!("regtest is not supported")),
    }
}

/// Derive the zkv signing pubkey from a UFVK at the fixed scope+index.
pub(crate) fn zkv_verifying_pubkey(
    ufvk: &UnifiedFullViewingKey,
) -> anyhow::Result<secp256k1::PublicKey> {
    let acct = ufvk
        .transparent()
        .ok_or_else(|| anyhow!("UFVK has no transparent component"))?;
    let index = NonHardenedChildIndex::from_index(ZKV_TRANSPARENT_INDEX)
        .ok_or_else(|| anyhow!("invalid zkv address index"))?;
    acct.derive_address_pubkey(ZKV_TRANSPARENT_SCOPE, index)
        .map_err(|e| anyhow!("failed to derive zkv signing pubkey: {e}"))
}

/// Build the canonical signed payload binding a command to a specific zkv address.
///
/// Format: `b"ZKV1\x00" || zkv_addr || b"\x00" || op || b"\x00" || key || b"\x00" || value`.
/// Null-separated so values containing spaces or other bytes are unambiguous.
pub(crate) fn signed_payload(zkv_addr: &str, op: Op, key: &str, value: Option<&str>) -> Vec<u8> {
    let mut buf = Vec::with_capacity(64 + zkv_addr.len() + key.len() + value.map_or(0, str::len));
    buf.extend_from_slice(SIGNED_MAGIC);
    buf.push(0);
    buf.extend_from_slice(zkv_addr.as_bytes());
    buf.push(0);
    buf.extend_from_slice(op.as_str().as_bytes());
    buf.push(0);
    buf.extend_from_slice(key.as_bytes());
    buf.push(0);
    if let Some(v) = value {
        buf.extend_from_slice(v.as_bytes());
    }
    buf
}

fn digest(payload: &[u8]) -> [u8; 32] {
    Sha256::digest(payload).into()
}

pub(crate) fn sign_command(sk: &secp256k1::SecretKey, payload: &[u8]) -> [u8; 64] {
    let secp = secp256k1::Secp256k1::signing_only();
    let msg = secp256k1::Message::from_digest(digest(payload));
    secp.sign_ecdsa(&msg, sk).serialize_compact()
}

pub(crate) fn verify_command(
    pk: &secp256k1::PublicKey,
    payload: &[u8],
    sig_hex: &str,
) -> bool {
    let Ok(sig_bytes) = hex::decode(sig_hex) else {
        return false;
    };
    let Ok(sig) = secp256k1::ecdsa::Signature::from_compact(&sig_bytes) else {
        return false;
    };
    let msg = secp256k1::Message::from_digest(digest(payload));
    let secp = secp256k1::Secp256k1::verification_only();
    secp.verify_ecdsa(&msg, &sig, pk).is_ok()
}

/// Build a two-line text memo for a zkv command.
///
/// Wire format (line 1 omits the zkv address to save bytes):
/// ```text
/// ZKV1 SET <key> <value>
/// <hex sig>
/// ```
pub(crate) fn build_memo(
    op: Op,
    key: &str,
    value: Option<&str>,
    sig: &[u8; 64],
) -> anyhow::Result<MemoBytes> {
    if key.contains(char::is_whitespace) {
        bail!("zkv keys must not contain whitespace");
    }
    if let Some(v) = value {
        if v.contains('\n') {
            bail!("zkv values must not contain newlines");
        }
    }

    let line1 = match (op, value) {
        (Op::Set, Some(v)) => format!("{WIRE_MAGIC} SET {key} {v}"),
        (Op::Del, None) => format!("{WIRE_MAGIC} DEL {key}"),
        (Op::Set, None) => bail!("SET requires a value"),
        (Op::Del, Some(_)) => bail!("DEL takes no value"),
    };

    let text = format!("{line1}\n{}", hex::encode(sig));
    let memo = Memo::from_str(&text)
        .map_err(|e| anyhow!("zkv memo too large for a Zcash text memo: {e}"))?;
    Ok(MemoBytes::from(memo))
}

#[derive(Debug)]
pub(crate) struct ZkvCommand {
    pub(crate) op: Op,
    pub(crate) key: String,
    pub(crate) value: Option<String>,
    pub(crate) sig_hex: String,
}

/// Parse a `Memo::Text` payload into a zkv command, if it is one.
///
/// Standard wire form: `"ZKV1 OP KEY VALUE\n<128-char hex sig>"`. Some
/// broadcaster wallets normalize newlines into whitespace, so as a fallback
/// we recover by taking the trailing 128 hex characters as the signature.
pub(crate) fn parse_text_memo(text: &str) -> Option<ZkvCommand> {
    let (line1, sig_hex) = if let Some((head, rest)) = text.split_once('\n') {
        (head.to_owned(), rest.trim().to_owned())
    } else {
        let trimmed = text.trim_end();
        if trimmed.len() < 128 {
            return None;
        }
        let (head, tail) = trimmed.split_at(trimmed.len() - 128);
        if !tail.chars().all(|c| c.is_ascii_hexdigit()) {
            return None;
        }
        (head.trim_end().to_owned(), tail.to_owned())
    };

    let mut tokens = line1.splitn(4, ' ');
    if tokens.next()? != WIRE_MAGIC {
        return None;
    }
    let op = Op::parse(tokens.next()?)?;
    let key = tokens.next()?.to_owned();
    if key.is_empty() {
        return None;
    }
    let value = match op {
        Op::Set => Some(tokens.next()?.to_owned()),
        Op::Del => {
            if tokens.next().is_some() {
                return None;
            }
            None
        }
    };

    Some(ZkvCommand {
        op,
        key,
        value,
        sig_hex,
    })
}

/// Replay a sequence of memo entries (in chain order) into final key-value state.
///
/// `entries` must be ordered by mined_height ASC (mempool last), txid ASC, output_index ASC.
/// Malformed memos and bad signatures are dropped silently unless `strict` is set.
pub(crate) fn replay<I>(
    entries: I,
    zkv_addr: &str,
    pk: &secp256k1::PublicKey,
    strict: bool,
) -> anyhow::Result<BTreeMap<String, String>>
where
    I: IntoIterator<Item = String>,
{
    let mut state: BTreeMap<String, String> = BTreeMap::new();
    for text in entries {
        let Some(cmd) = parse_text_memo(&text) else {
            if strict {
                bail!("malformed zkv memo: {text:?}");
            }
            continue;
        };
        let payload = signed_payload(zkv_addr, cmd.op, &cmd.key, cmd.value.as_deref());
        if !verify_command(pk, &payload, &cmd.sig_hex) {
            if strict {
                bail!("invalid zkv signature for key {:?}", cmd.key);
            }
            continue;
        }
        match cmd.op {
            Op::Set => {
                if let Some(v) = cmd.value {
                    state.insert(cmd.key, v);
                }
            }
            Op::Del => {
                state.remove(&cmd.key);
            }
        }
    }
    Ok(state)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn keypair() -> (secp256k1::SecretKey, secp256k1::PublicKey) {
        let secp = secp256k1::Secp256k1::new();
        let sk_bytes = [0x42u8; 32];
        let sk = secp256k1::SecretKey::from_slice(&sk_bytes).unwrap();
        let pk = sk.public_key(&secp);
        (sk, pk)
    }

    #[test]
    fn sign_verify_set_round_trip() {
        let (sk, pk) = keypair();
        let addr = "zkv1:uview1example:3000000";
        let payload = signed_payload(addr, Op::Set, "zec_usd_price", Some("123.45"));
        let sig = sign_command(&sk, &payload);
        let sig_hex = hex::encode(sig);
        assert!(verify_command(&pk, &payload, &sig_hex));
    }

    #[test]
    fn sign_verify_del_round_trip() {
        let (sk, pk) = keypair();
        let addr = "zkv1:uview1example:3000000";
        let payload = signed_payload(addr, Op::Del, "zec_usd_price", None);
        let sig = sign_command(&sk, &payload);
        let sig_hex = hex::encode(sig);
        assert!(verify_command(&pk, &payload, &sig_hex));
    }

    #[test]
    fn signature_does_not_verify_for_different_address() {
        let (sk, pk) = keypair();
        let addr_a = "zkv1:uview1A:3000000";
        let addr_b = "zkv1:uview1B:3000000";
        let payload_a = signed_payload(addr_a, Op::Set, "k", Some("v"));
        let sig = sign_command(&sk, &payload_a);
        let sig_hex = hex::encode(sig);

        // Replaying the memo against a *different* zkv address must not verify, because
        // the canonical payload binds the address.
        let payload_b = signed_payload(addr_b, Op::Set, "k", Some("v"));
        assert!(!verify_command(&pk, &payload_b, &sig_hex));
    }

    #[test]
    fn memo_round_trip() {
        let (sk, pk) = keypair();
        let addr = "zkv1:uview1example:3000000";
        let payload = signed_payload(addr, Op::Set, "greeting", Some("hello world"));
        let sig = sign_command(&sk, &payload);
        let memo = build_memo(Op::Set, "greeting", Some("hello world"), &sig).unwrap();

        // Decode the memo back into text.
        let m = Memo::try_from(memo).unwrap();
        let text = match m {
            Memo::Text(t) => t.to_string(),
            _ => panic!("expected text memo"),
        };

        let cmd = parse_text_memo(&text).expect("parses");
        assert!(matches!(cmd.op, Op::Set));
        assert_eq!(cmd.key, "greeting");
        assert_eq!(cmd.value.as_deref(), Some("hello world"));

        let recomputed = signed_payload(addr, cmd.op, &cmd.key, cmd.value.as_deref());
        assert!(verify_command(&pk, &recomputed, &cmd.sig_hex));
    }

    #[test]
    fn replay_overwrites_and_deletes() {
        let (sk, pk) = keypair();
        let addr = "zkv1:test:1";

        let make = |op: Op, k: &str, v: Option<&str>| -> String {
            let payload = signed_payload(addr, op, k, v);
            let sig = sign_command(&sk, &payload);
            let memo = build_memo(op, k, v, &sig).unwrap();
            match Memo::try_from(memo).unwrap() {
                Memo::Text(t) => t.to_string(),
                _ => unreachable!(),
            }
        };

        let entries = vec![
            make(Op::Set, "a", Some("1")),
            make(Op::Set, "b", Some("2")),
            make(Op::Set, "a", Some("3")), // overwrite
            make(Op::Del, "b", None),      // delete
        ];

        let state = replay(entries, addr, &pk, true).unwrap();
        assert_eq!(state.get("a").map(String::as_str), Some("3"));
        assert!(state.get("b").is_none());
    }

    #[test]
    fn parse_recovers_when_newline_collapsed_to_whitespace() {
        // Some broadcaster wallets replace the newline between command and
        // signature with whitespace; we should still parse the trailing 128
        // hex chars as the signature.
        let (sk, pk) = keypair();
        let addr = "zkv1:test:1";
        let payload = signed_payload(addr, Op::Set, "zec_usd_price", Some("1008.33"));
        let sig = sign_command(&sk, &payload);
        let sig_hex = hex::encode(sig);

        let mangled = format!("ZKV1 SET zec_usd_price 1008.33                       {sig_hex}");
        let cmd = parse_text_memo(&mangled).expect("parses despite missing newline");
        assert!(matches!(cmd.op, Op::Set));
        assert_eq!(cmd.key, "zec_usd_price");
        assert_eq!(cmd.value.as_deref(), Some("1008.33"));

        let recomputed = signed_payload(addr, cmd.op, &cmd.key, cmd.value.as_deref());
        assert!(verify_command(&pk, &recomputed, &cmd.sig_hex));
    }

    #[test]
    fn replay_silently_drops_invalid_sigs() {
        let (sk, pk) = keypair();
        let addr = "zkv1:test:1";

        let payload = signed_payload(addr, Op::Set, "a", Some("1"));
        let sig = sign_command(&sk, &payload);
        let good = build_memo(Op::Set, "a", Some("1"), &sig).unwrap();
        let good_text = match Memo::try_from(good).unwrap() {
            Memo::Text(t) => t.to_string(),
            _ => unreachable!(),
        };

        // A memo claiming to set b=2 but with a signature that is just zero bytes.
        let bad_sig = [0u8; 64];
        let bad = build_memo(Op::Set, "b", Some("2"), &bad_sig).unwrap();
        let bad_text = match Memo::try_from(bad).unwrap() {
            Memo::Text(t) => t.to_string(),
            _ => unreachable!(),
        };

        let state = replay(vec![good_text, bad_text], addr, &pk, false).unwrap();
        assert_eq!(state.get("a").map(String::as_str), Some("1"));
        assert!(state.get("b").is_none());
    }
}

/// Shared SET/DEL pipeline: validate, sign, build memo, and broadcast a tx
/// containing a single Orchard output to the receiver UA.
///
/// If `print_only` is true, the signed memo and recipient UA are printed and the
/// transaction is NOT broadcast — useful for handing the memo to another wallet.
#[allow(clippy::too_many_arguments)]
pub(crate) async fn do_write<C: PaymentContext>(
    wallet_dir: Option<String>,
    zkv_addr: Option<String>,
    op: Op,
    key: String,
    value: Option<String>,
    identity_path: String,
    print_only: bool,
    payment_ctx: C,
) -> anyhow::Result<()> {
    let mut config = WalletConfig::read(wallet_dir.as_ref())?;
    let params = config.network();

    // Open wallet DB to inspect accounts.
    let (_, db_data_path) = get_db_paths(wallet_dir.as_ref());
    let db_data = WalletDb::for_path(&db_data_path, params, SystemClock, OsRng)?;
    let account = select_account(&db_data, payment_ctx.spending_account())?;
    let local_ufvk = account
        .ufvk()
        .ok_or_else(|| anyhow!("selected account has no UFVK"))?;

    // Determine the target zkv address: if not provided, derive from this account.
    let parsed = match zkv_addr {
        Some(s) => {
            let parsed = parse_zkv_addr(&s)?;
            if local_ufvk.encode(&params) != parsed.ufvk.encode(&params) {
                bail!(
                    "selected local account does not own the zkv address's UFVK; \
                     the admin must hold a USK matching the zkv address to sign writes"
                );
            }
            // Network match check.
            let target_net = network_from_type(parsed.network)?;
            if target_net != params {
                bail!("zkv address network does not match local wallet network");
            }
            parsed
        }
        None => {
            let birthday: u32 = config.birthday().into();
            let raw = encode_zkv_addr(&local_ufvk.encode(&params), birthday);
            ParsedZkvAddr {
                raw,
                network: params.network_type(),
                ufvk: local_ufvk.clone(),
                birthday,
            }
        }
    };

    // Decrypt the seed and derive the zkv signing key.
    let identities =
        age::IdentityFile::from_file(identity_path.clone())?.into_identities()?;
    let seed = config
        .decrypt_seed(identities.iter().map(|i| i.as_ref() as _))?
        .ok_or_else(|| anyhow!("Seed must be present to enable signing"))?;
    let derivation = account
        .source()
        .key_derivation()
        .ok_or_else(|| anyhow!("Cannot sign with view-only accounts"))?;
    let usk =
        UnifiedSpendingKey::from_seed(&params, seed.expose_secret(), derivation.account_index())?;
    let index = NonHardenedChildIndex::from_index(ZKV_TRANSPARENT_INDEX)
        .ok_or_else(|| anyhow!("invalid zkv address index"))?;
    let sk = usk
        .transparent()
        .derive_secret_key(ZKV_TRANSPARENT_SCOPE, index)
        .map_err(|e| anyhow!("failed to derive zkv signing key: {e}"))?;

    // Build the canonical payload, sign, and assemble the memo.
    let payload = signed_payload(&parsed.raw, op, &key, value.as_deref());
    let sig = sign_command(&sk, &payload);
    let memo = build_memo(op, &key, value.as_deref(), &sig)?;

    // Resolve an Orchard-only recipient UA from the zkv address's UFVK so the memo is
    // guaranteed to land in an Orchard note (which `zkv get` filters on).
    let (ua, _) = parsed
        .ufvk
        .default_address(UnifiedAddressRequest::ORCHARD)
        .map_err(|e| anyhow!("could not derive Orchard receiver from UFVK: {e}"))?;
    let ua_str = ua.encode(&params);
    let recipient = ZcashAddress::from_str(&ua_str)
        .map_err(|e| anyhow!("invalid recipient UA: {e}"))?;

    println!("zkv {} {} → {}", op.as_str(), key, parsed.raw);
    println!("  recipient (Orchard-only UA): {ua_str}");

    if print_only {
        // Render the wire memo and emit it for the user to broadcast elsewhere.
        let memo_obj = Memo::try_from(memo)
            .map_err(|e| anyhow!("could not decode just-built memo: {e}"))?;
        let text = match memo_obj {
            Memo::Text(t) => t.to_string(),
            _ => bail!("unexpected non-text memo"),
        };
        println!("\n--- begin zkv memo ---");
        println!("{text}");
        println!("--- end zkv memo ---");
        println!(
            "\nSend a 1-zatoshi (or higher) Orchard payment to the recipient UA above with this exact memo."
        );
        return Ok(());
    }

    // Send a small dust-safe amount to the receiver UA with the zkv memo.
    let payment = Payment::new(
        recipient,
        Some(Zatoshis::from_u64(1)?),
        Some(memo),
        None,
        None,
        vec![],
    )
    .ok_or_else(|| anyhow!("failed to build payment for zkv memo"))?;
    let request = TransactionRequest::new(vec![payment])
        .map_err(|e| anyhow!("invalid transaction request: {e}"))?;

    // Drop the read-only handle before pay() opens its own.
    drop(account);
    drop(db_data);

    pay(wallet_dir, payment_ctx, request).await
}

