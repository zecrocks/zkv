use super::*;

#[derive(Debug)]
pub struct ParsedZkvAddr {
    #[allow(dead_code)]
    pub raw: String,
    pub network: NetworkType,
    pub ufvk: UnifiedFullViewingKey,
    pub birthday: u32,
    /// The shielded pool this database delivers memos in, inferred from which
    /// shielded component the published UFVK carries (Orchard if present, else
    /// Sapling).
    pub pool: ShieldedProtocol,
}

/// Private-use unified typecode carrying zkv metadata: currently just the
/// database's 4-byte big-endian birthday height. It sits in ZIP-316's
/// unknown-item range (`0x04..=0x02000000`), so conforming wallets ignore it;
/// the bytes spell ASCII `"zkm"`. A unified viewing key is only a valid **zkv
/// address** if it carries this item: that is what distinguishes a zkv database
/// key from a plain viewing key, and it lets the birthday travel *inside* the
/// key rather than as a separate suffix.
pub const TC_ZKV_META: u32 = 0x007A_6B6D;

/// The bech32 HRP family for zkv addresses, mirroring `uview`/`uviewtest`/
/// `uviewregtest` so the network is recoverable from the address alone.
pub(crate) fn zkv_hrp(net: NetworkType) -> &'static str {
    match net {
        NetworkType::Main => "zkv",
        NetworkType::Test => "zkvtest",
        NetworkType::Regtest => "zkvregtest",
    }
}

/// The standard unified-FVK HRP for `net` (`uview`/`uviewtest`/`uviewregtest`).
fn uview_hrp(net: NetworkType) -> &'static str {
    match net {
        NetworkType::Main => "uview",
        NetworkType::Test => "uviewtest",
        NetworkType::Regtest => "uviewregtest",
    }
}

/// Inverse of [`zkv_hrp`]: the network a zkv HRP denotes, or `None` if `hrp`
/// isn't one of ours (so a plain `uview…` is not accepted as a zkv address).
fn net_from_zkv_hrp(hrp: &str) -> Option<NetworkType> {
    match hrp {
        "zkv" => Some(NetworkType::Main),
        "zkvtest" => Some(NetworkType::Test),
        "zkvregtest" => Some(NetworkType::Regtest),
        _ => None,
    }
}

/// Whether `hrp` is the standard unified-FVK HRP family, i.e. the user pasted a
/// plain Zcash viewing key. Used only to give a friendlier "this isn't enabled
/// for zkv yet" rejection.
fn is_uview_hrp(hrp: &str) -> bool {
    matches!(hrp, "uview" | "uviewtest" | "uviewregtest")
}

// TODO(zkv-enable): add a utility that "enables" an existing standard `uview…`
// viewing key for zkv: `protocol::enable_uview(uview, birthday) -> zkv_addr`
// plus a `zkv enable <uview…> --birthday <h>` command (and a GUI affordance):
// inject the [`TC_ZKV_META`] item (the birthday is the zkv marker) and relabel
// the HRP `uview…` → `zkv…`, optionally broadcasting INIT in the same step (which
// needs the admin/spending key, so it's only for the key's owner). Today the
// only way to mint a zkv address is `zkv init`/`restore` from a seed, or
// `zkv address` to re-export one you own; a bare viewing key someone hands you
// cannot yet be turned into a zkv database without that helper.

/// Re-label a bech32m string's HRP, preserving the data part verbatim (only the
/// HRP and the trailing checksum change). A zkv address *is* the database's
/// unified viewing key under a `zkv` HRP instead of `uview` (same bytes,
/// different label), so this single primitive is the whole zkv⇄uview conversion.
/// (ZIP-316 unified encodings are a whole number of bytes, so the bech32 byte
/// round-trip is exact; `Bech32m::CODE_LENGTH` is 1023, far above a UFVK.)
pub(crate) fn relabel_hrp(src: &str, new_hrp: &str) -> anyhow::Result<String> {
    let (_, data) = bech32::decode(src).map_err(|e| anyhow!("bech32 decode: {e}"))?;
    bech32::encode::<Bech32m>(Hrp::parse_unchecked(new_hrp), &data)
        .map_err(|e| anyhow!("bech32 encode: {e}"))
}

/// Build the pool-stripped unified container with the zkv-meta birthday item
/// appended, returning `(network, container)`. Encodes as a standard `uview…`
/// (carrying the extra item) or, HRP-relabeled, as a `zkv…` address.
fn zkv_container<P: consensus::Parameters>(
    ufvk: &UnifiedFullViewingKey,
    params: &P,
    pool: ShieldedProtocol,
    birthday: u32,
) -> anyhow::Result<(NetworkType, unified::Ufvk)> {
    let stripped = encode_ufvk_for_pool(ufvk, params, pool);
    let (net, container) =
        unified::Ufvk::decode(&stripped).expect("internal: just-encoded UFVK must round-trip");
    let mut items = container.items();
    items.push(unified::Fvk::Unknown {
        typecode: TC_ZKV_META,
        data: birthday.to_be_bytes().to_vec(),
    });
    let with_meta =
        unified::Ufvk::try_from_items(items).map_err(|e| anyhow!("add zkv meta item: {e}"))?;
    Ok((net, with_meta))
}

/// The canonical zkv address: the database's unified viewing key (transparent +
/// its one shielded pool) plus the zkv-meta birthday item, encoded under the
/// `zkv` HRP family. A single self-describing token (no colons, no birthday
/// suffix) that converts to a standard `uview…` viewing key any wallet reads
/// (see [`zkv_addr_to_uview`]).
pub fn encode_zkv_addr<P: consensus::Parameters>(
    ufvk: &UnifiedFullViewingKey,
    params: &P,
    pool: ShieldedProtocol,
    birthday: u32,
) -> anyhow::Result<String> {
    let (net, with_meta) = zkv_container(ufvk, params, pool, birthday)?;
    relabel_hrp(&with_meta.encode(&net), zkv_hrp(net))
}

/// The standard `uview…` encoding of a zkv address: the *same* viewing-key
/// bytes (birthday meta included) under the `uview` HRP. This is what you paste
/// into a stock Zcash wallet (Zashi / Ywallet / zcashd) to view the raw memos;
/// conforming wallets ignore the unknown zkv-meta item.
pub fn zkv_addr_to_uview(zkv_addr: &str) -> anyhow::Result<String> {
    let (hrp, _) = bech32::decode(zkv_addr)
        .map_err(|_| anyhow!("this doesn't look like a zkv address (expected a `zkv1…` token)"))?;
    let net = net_from_zkv_hrp(hrp.as_str()).ok_or_else(|| {
        anyhow!(
            "this isn't a zkv address (its prefix is `{}`, not `zkv…`)",
            hrp.as_str()
        )
    })?;
    relabel_hrp(zkv_addr, uview_hrp(net))
}

/// Encode a UFVK keeping transparent + the chosen shielded pool, stripping the
/// *other* shielded component; a zkv database publishes exactly one pool. For
/// Orchard this strips Sapling (the original behavior); for Sapling it strips
/// Orchard.
pub fn encode_ufvk_for_pool<P: consensus::Parameters>(
    ufvk: &UnifiedFullViewingKey,
    params: &P,
    pool: ShieldedProtocol,
) -> String {
    let full = ufvk.encode(params);
    let (network_type, container) =
        unified::Ufvk::decode(&full).expect("internal: just-encoded UFVK must round-trip");
    let items: Vec<unified::Fvk> = container
        .items()
        .into_iter()
        .filter(|item| match pool {
            // Keep the chosen pool; drop the other shielded item.
            ShieldedProtocol::Orchard => !matches!(item, unified::Fvk::Sapling(_)),
            ShieldedProtocol::Sapling => !matches!(item, unified::Fvk::Orchard(_)),
        })
        .collect();
    unified::Ufvk::try_from_items(items)
        .expect("internal: stripping one shielded pool preserves a valid UFVK")
        .encode(&network_type)
}

/// The [`UnifiedAddressRequest`] for a database's single shielded pool: a
/// receiver in that pool only (no transparent receiver; memo writes are
/// shielded). zkv databases publish a one-pool UA, so the recipient address a
/// writer/broadcaster pays is unambiguous.
pub fn ua_request_for_pool(pool: ShieldedProtocol) -> UnifiedAddressRequest {
    match pool {
        ShieldedProtocol::Orchard => UnifiedAddressRequest::ORCHARD,
        ShieldedProtocol::Sapling => UnifiedAddressRequest::unsafe_custom(
            ReceiverRequirement::Omit,
            ReceiverRequirement::Require,
            ReceiverRequirement::Omit,
        ),
    }
}

pub fn parse_zkv_addr(s: &str) -> anyhow::Result<ParsedZkvAddr> {
    // A zkv address is a unified viewing key under a `zkv` HRP. Recover the
    // network from the HRP, then re-label to the standard `uview` HRP (same
    // bytes) and parse with the canonical machinery.
    let (hrp, _) = bech32::decode(s).map_err(|_| {
        anyhow!(
            "this doesn't look like a zkv address, a zkv address is a single \
             `zkv1…` token (no spaces or colons). Run `zkv address` to print yours."
        )
    })?;
    let network = net_from_zkv_hrp(hrp.as_str()).ok_or_else(|| {
        if is_uview_hrp(hrp.as_str()) {
            anyhow!(
                "that's a standard Zcash unified viewing key (`{}1…`), not a zkv \
                 address, it hasn't been enabled for zkv yet. If you own this \
                 database, run `zkv address` to get its `zkv1…` form.",
                hrp.as_str()
            )
        } else {
            anyhow!(
                "this doesn't look like a zkv address, expected a `zkv1…` / \
                 `zkvtest1…` token, but its prefix is `{}`.",
                hrp.as_str()
            )
        }
    })?;
    let uview = relabel_hrp(s, uview_hrp(network))?;
    let (net2, container) = unified::Ufvk::decode(&uview).map_err(|e| {
        anyhow!("this zkv address doesn't contain a valid unified viewing key: {e}")
    })?;
    debug_assert_eq!(network, net2);

    // The zkv-meta birthday item is mandatory: its presence is what makes this
    // a zkv database key rather than a plain viewing key.
    let birthday = container
        .items()
        .into_iter()
        .find_map(|f| match f {
            unified::Fvk::Unknown { typecode, data }
                if typecode == TC_ZKV_META && data.len() == 4 =>
            {
                Some(u32::from_be_bytes(
                    data.try_into().expect("length 4 checked"),
                ))
            }
            _ => None,
        })
        .ok_or_else(|| {
            anyhow!(
                "this is a zkv-format key but it's missing its zkv metadata (the \
                 birthday item), so it isn't a complete zkv address, it looks like a \
                 plain viewing key that was relabeled without being enabled for zkv."
            )
        })?;

    // Strip the zkv-meta item before handing the container to
    // `UnifiedFullViewingKey::parse`, so it sees a pristine standard UFVK.
    let clean_items: Vec<unified::Fvk> = container
        .items()
        .into_iter()
        .filter(
            |f| !matches!(f, unified::Fvk::Unknown { typecode, .. } if *typecode == TC_ZKV_META),
        )
        .collect();
    let clean = unified::Ufvk::try_from_items(clean_items)
        .map_err(|e| anyhow!("could not rebuild UFVK without zkv meta: {e}"))?;
    let ufvk =
        UnifiedFullViewingKey::parse(&clean).map_err(|e| anyhow!("could not parse UFVK: {e}"))?;

    if ufvk.transparent().is_none() {
        bail!(
            "this zkv address is missing a transparent component, which zkv needs to \
             derive the database's signing key"
        );
    }
    // A zkv database lives in exactly one shielded pool. Infer it from the
    // published UFVK: prefer Orchard when present, otherwise Sapling.
    let pool = if ufvk.orchard().is_some() {
        ShieldedProtocol::Orchard
    } else if ufvk.sapling().is_some() {
        ShieldedProtocol::Sapling
    } else {
        bail!(
            "this zkv address has no shielded pool (Sapling or Orchard), so it can't \
             carry memos"
        );
    };

    Ok(ParsedZkvAddr {
        raw: s.to_owned(),
        network,
        ufvk,
        birthday,
        pool,
    })
}

/// Convert a parsed zkv address's `NetworkType` to a [`crate::network::Network`].
pub fn network_from_type(network: NetworkType) -> anyhow::Result<crate::network::Network> {
    use crate::network::Network;
    Ok(match network {
        NetworkType::Main => Network::Main,
        NetworkType::Test => Network::Test,
        NetworkType::Regtest => Network::Regtest,
    })
}

/// Derive the zkv signing pubkey from a UFVK at the fixed scope+index.
pub fn zkv_verifying_pubkey(ufvk: &UnifiedFullViewingKey) -> anyhow::Result<secp256k1::PublicKey> {
    let acct = ufvk
        .transparent()
        .ok_or_else(|| anyhow!("UFVK has no transparent component"))?;
    let index = NonHardenedChildIndex::from_index(ZKV_TRANSPARENT_INDEX)
        .ok_or_else(|| anyhow!("invalid zkv address index"))?;
    acct.derive_address_pubkey(ZKV_TRANSPARENT_SCOPE, index)
        .map_err(|e| anyhow!("failed to derive zkv signing pubkey: {e}"))
}
