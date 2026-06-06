use super::*;

/// Build the canonical signed payload binding a command to a database's signing
/// **domain**.
///
/// Format: `b"ZKV0\x00" || domain || b"\x00" || op || b"\x00" || key || b"\x00" || value`.
///
/// `domain` is **not** the address string. It is the receiver-bound,
/// version-stamped domain produced by [`signing_domain`]: the hex of the
/// database's shielded receiver ([`receiver_domain`]) for INIT/VERSION, and that
/// same receiver hex plus `":"` plus the entity's current version for data and
/// management ops. Binding the receiver (rather than the `zkv1…` address string)
/// makes the birthday and UFVK encoding non-load-bearing: re-exporting an
/// address with a corrected birthday keeps every historical signature valid.
/// Folding the per-key/per-target version in defeats verbatim replay
/// (an old SET/DEL recovers the wrong signer once the version has advanced).
pub fn signed_payload(domain: &str, op: Op, key: &str, value: Option<&str>) -> Vec<u8> {
    let mut buf = Vec::with_capacity(64 + domain.len() + key.len() + value.map_or(0, str::len));
    buf.extend_from_slice(SIGNED_MAGIC);
    buf.push(0);
    buf.extend_from_slice(domain.as_bytes());
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

/// The database's **receiver domain**: lowercase hex of the raw bytes of the
/// database's default shielded receiver in its pool (Orchard or Sapling),
/// derived from the UFVK. This is the stable, birthday-independent identity a
/// `ZKV0` signature binds to; two readers who hold the same UFVK compute the
/// same string regardless of the birthday or which UFVK string encoding they
/// were handed.
///
/// The receiver is the *spend target* the database already publishes (the
/// single-pool UA a writer pays), so it cannot disagree with the read key.
pub fn receiver_domain(
    ufvk: &UnifiedFullViewingKey,
    pool: ShieldedProtocol,
    net: NetworkType,
) -> anyhow::Result<String> {
    let (ua, _) = ufvk
        .default_address(ua_request_for_pool(pool))
        .map_err(|e| anyhow!("derive {pool:?} receiver: {e}"))?;
    let raw: Vec<u8> = match pool {
        ShieldedProtocol::Orchard => ua
            .orchard()
            .ok_or_else(|| anyhow!("single-pool UA missing its Orchard receiver"))?
            .to_raw_address_bytes()
            .to_vec(),
        ShieldedProtocol::Sapling => ua
            .sapling()
            .ok_or_else(|| anyhow!("single-pool UA missing its Sapling receiver"))?
            .to_bytes()
            .to_vec(),
    };
    // Prefix an explicit network discriminant. The receiver bytes and the
    // transparent signing key are *already* network-specific via the ZIP-32
    // coin type (mainnet 133' vs testnet/regtest 1'), so cross-network replay is
    // implicitly prevented, but binding the network into the domain makes that
    // guarantee load-bearing rather than emergent (regtest and testnet even share
    // a coin type, so the receiver bytes alone can't separate them). Defense in
    // depth: a memo signed for one network can never verify on another.
    Ok(format!("{}:{}", network_tag(net), hex::encode(raw)))
}

/// Stable per-network discriminant folded into [`receiver_domain`]. Distinct
/// from the address HRP; this is a signing-domain token, so it must never
/// change for a given network.
fn network_tag(net: NetworkType) -> &'static str {
    match net {
        NetworkType::Main => "main",
        NetworkType::Test => "test",
        NetworkType::Regtest => "regtest",
    }
}

/// How far **ahead** of an entity's current version a write's sequence may be
/// and still be honored: the reader accepts `current ..= current + VERSION_WINDOW`
/// (a bounded *forward* window) rather than requiring an exact match.
///
/// Why a window, not exact match:
/// - **Robustness (the every-block-oracle case).** A writer signs its sequence
///   from `confirmed + own in-flight count`. If one in-flight write never
///   confirms (mempool eviction, a fee too low, a reorg) it lingers in
///   `pending.toml` and inflates that count, so the *next* write's sequence runs
///   ahead of what the reader has actually honored. With exact match that next
///   write and every one after it until `pending.toml` ages out are dropped,
///   silently burning fees. A forward window absorbs the gap: the write still
///   lands within `current + W`, so a single missed write doesn't strand the
///   stream.
/// - **No freeze.** The window is *bounded*: a write can advance the counter by
///   at most `W`, so an authorized writer can't jump it to a huge value to
///   permanently wedge a key (every step still costs a fee, and a later write
///   only needs to clear the new high-water). Pure "strictly increasing" would
///   reopen that freeze; exact match has no headroom at all. This is the middle
///   ground.
///
/// `W = 256` comfortably covers a writer's realistic in-flight depth (an
/// every-block oracle accumulates ~48 unconfirmed writes per `pending.toml` TTL)
/// with a wide safety margin, while keeping the freeze headroom small.
pub const VERSION_WINDOW: u64 = 256;

/// Compute the signing **domain** for one command, given the database's
/// [`receiver_domain`] (`receiver`) and the per-entity replay-protection
/// sequence `seq` the writer referenced.
///
/// - **Data ops** (`SET`/`SETL`/`DEL`) and **management ops**
///   (`OWNERSET`/`OWNERDEL`/`WRITERSET`/`WRITERDEL`): `"<receiver>:<seq>"`, where
///   `seq` is the count of honored writes to the entity (the data key, or the
///   target pubkey) so far. This is the replay-protection counter; a tombstone
///   *retains* its sequence, so a deleted key / revoked target cannot be
///   recreated by replaying its original creation. The sequence travels on the
///   wire as a compact prefix on the signature line (see [`encode_sig_line`]),
///   so the reader knows which version the writer signed without guessing.
/// - **INIT / VERSION**: just `"<receiver>"` (neither is version-CAS'd: INIT is
///   first-valid-wins; VERSION uses its own single-step transition rule).
pub fn signing_domain(receiver: &str, op: Op, seq: u64) -> String {
    match op {
        Op::Set
        | Op::SetL
        | Op::Del
        | Op::OwnerSet
        | Op::OwnerDel
        | Op::WriterSet
        | Op::WriterDel => format!("{receiver}:{seq}"),
        // INIT/VERSION/FINALIZE are not version-CAS'd (INIT is first-valid-wins;
        // VERSION uses its own transition rule; FINALIZE is a one-way latch).
        Op::Init | Op::Version | Op::Finalize => receiver.to_owned(),
    }
}

/// Fold an optional first-line comment into a signing `domain` string so the
/// comment is covered by the signature.
///
/// A present comment extends the domain with its SHA-256 (hex-encoded) behind a
/// `:c=` tag. An **absent** comment returns the domain byte-for-byte unchanged,
/// so every historical no-comment signature still verifies. Comments are a
/// pure superset of the existing wire format (no [`ZKV_VERSION`] bump).
///
/// Why the comment is bound through the *domain* (a NUL-delimited field that
/// precedes the trailing, unbounded `value` in [`signed_payload`]) rather than
/// appended after the value: appending after `value` would be malleable; a
/// `SETL` value can carry arbitrary bytes, so a crafted value could absorb or
/// impersonate a trailing comment region and let an attacker add/strip a comment
/// while keeping the same recovered signer. Binding the comment's *hash* inside
/// the delimited domain keeps `value` the sole trailing field, and preimage
/// resistance stops a value from masquerading as a `:c=` suffix. The `:c=` tag
/// can never collide with a base domain, which is only `tag:hex[:seq]`.
pub fn bind_comment(domain: &str, comment: Option<&str>) -> String {
    match comment {
        None => domain.to_owned(),
        Some(c) => format!("{domain}:c={}", hex::encode(digest(c.as_bytes()))),
    }
}

/// Reconstruct the exact payload a writer signed for a parsed [`ZkvCommand`]
/// against `receiver` (the database's [`receiver_domain`]), including any
/// first-line comment.
///
/// This is the one place the read path, the history view, and the snapshot
/// promote path agree on how a memo's bytes map back to a signed payload: INIT
/// binds only the receiver (its wire address is an advisory, unsigned echo);
/// every other op binds the receiver plus the replay-protection sequence the
/// writer put on the wire. A first-line comment, when present, is folded into
/// the domain by [`bind_comment`] in either branch.
pub fn payload_for(receiver: &str, cmd: &ZkvCommand) -> Vec<u8> {
    let comment = cmd.comment.as_deref();
    if matches!(cmd.op, Op::Init) {
        signed_payload(&bind_comment(receiver, comment), Op::Init, "", None)
    } else {
        let domain = bind_comment(&signing_domain(receiver, cmd.op, cmd.seq), comment);
        signed_payload(&domain, cmd.op, &cmd.key, cmd.value.as_deref())
    }
}

/// Hex-encoded signature length on the wire, as a count of leading hex chars to
/// reserve for the fixed 65-byte signature. The replay-protection sequence is
/// folded in *front* of these as a compact big-endian prefix (see
/// [`encode_sig_line`]).
pub(crate) const MAX_SEQ_BYTES: usize = 8;

/// Encode the wire signature line: the per-entity replay-protection sequence
/// `seq` folded in front of the 65-byte signature as a canonical (no leading
/// zero byte) big-endian prefix, the whole blob hex-encoded. `seq == 0` (INIT,
/// VERSION, or a first write to an entity) yields a bare `SIG_HEX_LEN`-char
/// signature with **no** prefix, so the common case costs zero extra
/// characters. Because the signature is a fixed [`SIG_LEN`] bytes, the boundary
/// is implicit: `parse_sig_line` peels the last 65 bytes as the signature
/// and reads whatever precedes it as the sequence. This keeps the line all-hex
/// and self-delimiting (no separator), and is strictly shorter than a decimal
/// `"<seq>:"` prefix.
pub fn encode_sig_line(seq: u64, sig_hex: &str) -> String {
    if seq == 0 {
        return sig_hex.to_owned();
    }
    let be = seq.to_be_bytes();
    let first = be.iter().position(|&b| b != 0).unwrap_or(MAX_SEQ_BYTES);
    format!("{}{sig_hex}", hex::encode(&be[first..]))
}

/// Parse a wire signature line into `(seq, sig_hex)`. Inverse of
/// [`encode_sig_line`]: the trailing `SIG_HEX_LEN` hex chars are the 65-byte
/// signature; any leading hex is the canonical big-endian sequence (empty ⇒ 0).
/// Rejects a non-hex / too-short / odd-length token, a non-canonical
/// (leading-zero-byte) sequence, or a sequence wider than a `u64`.
pub(crate) fn parse_sig_line(tok: &str) -> Result<(u64, String), MemoReject> {
    let tok = tok.trim();
    if tok.len() < SIG_HEX_LEN
        || !tok.len().is_multiple_of(2)
        || !tok.chars().all(|c| c.is_ascii_hexdigit())
    {
        return Err(MemoReject::Malformed(MemoFormat::BadSignatureFraming));
    }
    let (seq_hex, sig_hex) = tok.split_at(tok.len() - SIG_HEX_LEN);
    if seq_hex.is_empty() {
        return Ok((0, sig_hex.to_owned()));
    }
    let bytes =
        hex::decode(seq_hex).map_err(|_| MemoReject::Malformed(MemoFormat::BadSignatureFraming))?;
    // Canonical minimal big-endian: no leading zero byte, fits in a u64.
    if bytes.first() == Some(&0) || bytes.len() > MAX_SEQ_BYTES {
        return Err(MemoReject::Malformed(MemoFormat::BadSignatureFraming));
    }
    let seq = bytes.iter().fold(0u64, |acc, &b| (acc << 8) | u64::from(b));
    Ok((seq, sig_hex.to_owned()))
}

fn digest(payload: &[u8]) -> [u8; 32] {
    Sha256::digest(payload).into()
}

/// Sign `payload` with `sk`, producing a 65-byte recoverable signature
/// (64-byte compact ECDSA + 1-byte recovery id appended). The recovery id
/// lets a reader recover the signer's pubkey from the signature, so the memo
/// identifies *who* signed it without carrying the pubkey on the wire.
pub fn sign_command(sk: &secp256k1::SecretKey, payload: &[u8]) -> [u8; SIG_LEN] {
    let secp = secp256k1::Secp256k1::signing_only();
    let msg = secp256k1::Message::from_digest(digest(payload));
    let (recid, compact) = secp.sign_ecdsa_recoverable(&msg, sk).serialize_compact();
    let mut out = [0u8; SIG_LEN];
    out[..64].copy_from_slice(&compact);
    out[64] = recid.to_i32() as u8;
    out
}

/// Recover the signer's public key from a hex-encoded recoverable signature
/// over `payload`. Returns `None` if the hex is malformed, the wrong length,
/// the recovery id is invalid, or recovery fails. This is the v2 replacement
/// for "verify against a known pubkey": the signer is *derived* from the
/// signature, then checked against the authorization registry by the caller.
pub fn recover_signer(payload: &[u8], sig_hex: &str) -> Option<secp256k1::PublicKey> {
    let sig_bytes = hex::decode(sig_hex).ok()?;
    if sig_bytes.len() != SIG_LEN {
        return None;
    }
    let recid = RecoveryId::from_i32(sig_bytes[64] as i32).ok()?;
    let sig = RecoverableSignature::from_compact(&sig_bytes[..64], recid).ok()?;
    let msg = secp256k1::Message::from_digest(digest(payload));
    let secp = secp256k1::Secp256k1::verification_only();
    secp.recover_ecdsa(&msg, &sig).ok()
}

/// Verify that `sig_hex` is a valid signature over `payload` by `pk`.
///
/// With recoverable signatures, "verify against a known pubkey" is "recover
/// the signer and compare". Kept as a convenience for callers that already
/// know the expected signer (e.g. the faucet checking an INIT memo against
/// the address's UFVK-derived key).
pub fn verify_command(pk: &secp256k1::PublicKey, payload: &[u8], sig_hex: &str) -> bool {
    recover_signer(payload, sig_hex).is_some_and(|recovered| &recovered == pk)
}

/// The human-readable prefix for a zkv signer pubkey in its `zkvid1…` Bech32m
/// encoding: the **canonical** string form of an owner/writer identity, used
/// on the wire, in the signed payload, in the authorization registry, and in
/// all display.
///
/// This is a **zkv convention, not a Zcash standard.** Owners and writers are
/// identified by a raw compressed secp256k1 public key (the key a recoverable
/// signature recovers to), and Zcash has no blessed encoding for a bare signing
/// key the way it does for unified addresses / viewing keys (ZIP 316, `u` /
/// `uview` / `uivk`) or HD extended keys (ZIP 32/48). Wrapping the 33 compressed
/// bytes in Bech32m gives the identity a checksum, so a single mistyped
/// character is rejected outright rather than silently denoting a different key
/// (on the wire as much as on input), makes it self-describing, and keeps it from
/// being confused with a UA/UFVK or with a bare hex value / KV key. We
/// deliberately do *not* reuse `xpub` / `tpub` / `zpub`: those are SLIP-0132
/// Base58 *extended*-key prefixes (public key + chain code), which this is not.
///
/// The project-namespaced HRP is chosen for collision-safety; it can't clash
/// with another coin's registered Bech32 HRP (SLIP-0173) the way a bare `zid`
/// could.
pub const PUBKEY_HRP: &str = "zkvid";

/// The canonical string form of a pubkey: its `zkvid1…` Bech32m encoding (see
/// [`PUBKEY_HRP`]). This is what the registry keys on, what management memos
/// carry on the wire, and what every command prints. Inverse of [`parse_pubkey`].
pub fn pubkey_bech32(pk: &secp256k1::PublicKey) -> String {
    bech32::encode::<Bech32m>(Hrp::parse_unchecked(PUBKEY_HRP), &pk.serialize())
        .expect("33 bytes under a fixed 5-char HRP is always within Bech32m's length limit")
}

/// Parse an owner/writer pubkey, accepting **either** the canonical `zkvid1…`
/// Bech32m form **or** raw secp256k1 hex (compressed or uncompressed). Returns
/// `None` if it is neither a well-formed key. Used both for CLI input and for
/// re-parsing the target out of a management memo; callers re-encode via
/// [`pubkey_bech32`] so the stored/compared form is always canonical.
pub fn parse_pubkey(s: &str) -> Option<secp256k1::PublicKey> {
    // Only commit to the Bech32m branch when the HRP actually matches: a hex
    // string that happens to be structurally valid Bech32m under some *other*
    // HRP must still fall through to the hex parser. A `zkvid1…` string with a
    // corrupted checksum fails `CheckedHrpstring` outright (Bech32m detects any
    // single-char substitution), so the typo is rejected rather than decoded to
    // a different key, which is the whole point of the encoding.
    if let Ok(parsed) = CheckedHrpstring::new::<Bech32m>(s) {
        if parsed.hrp().as_str() == PUBKEY_HRP {
            let data = parsed.byte_iter().collect::<Vec<u8>>();
            return secp256k1::PublicKey::from_slice(&data).ok();
        }
    }
    secp256k1::PublicKey::from_str(s).ok()
}

/// Derive the public key for a secret key. The signing/verifying pubkey a
/// writer is known by in the registry is exactly this, so a caller can check
/// "am I authorized?" by deriving its own pubkey and consulting the registry.
pub fn pubkey_of(sk: &secp256k1::SecretKey) -> secp256k1::PublicKey {
    sk.public_key(&secp256k1::Secp256k1::signing_only())
}
