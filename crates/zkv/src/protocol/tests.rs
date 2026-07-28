use super::*;

fn keypair() -> (secp256k1::SecretKey, secp256k1::PublicKey) {
    let secp = secp256k1::Secp256k1::new();
    let sk_bytes = [0x42u8; 32];
    let sk = secp256k1::SecretKey::from_slice(&sk_bytes).unwrap();
    let pk = sk.public_key(&secp);
    (sk, pk)
}

fn keypair_from(seed: u8) -> (secp256k1::SecretKey, secp256k1::PublicKey) {
    let secp = secp256k1::Secp256k1::new();
    let sk = secp256k1::SecretKey::from_slice(&[seed; 32]).unwrap();
    let pk = sk.public_key(&secp);
    (sk, pk)
}

/// Build a confirmed signed INIT memo for `addr`, signed by `sk`. Most
/// replay tests need an INIT prelude so SET/DEL ops are honored.
fn init_memo(sk: &secp256k1::SecretKey, addr: &str) -> String {
    let payload = signed_init_payload(addr);
    let sig = sign_command(sk, &payload);
    let memo = build_init_memo(addr, &sig).unwrap();
    match Memo::try_from(memo).unwrap() {
        Memo::Text(t) => t.to_string(),
        _ => unreachable!(),
    }
}

/// Build a signed SET/DEL/management memo for `addr` at the entity's
/// **version 0** (the common case: a single write, or the first write to a
/// key/target). Same-key/target follow-ups must use [`op_memo_v`] with the
/// advancing version, since the reader folds the replay-protection counter.
fn op_memo(sk: &secp256k1::SecretKey, addr: &str, op: Op, k: &str, v: Option<&str>) -> String {
    op_memo_v(sk, addr, op, k, v, 0)
}

/// Build a signed memo over the `ZKV0` versioned signing domain: `addr` is
/// the receiver domain, `version` is the entity's (key or target) current
/// replay-protection version. INIT/VERSION ignore `version` (not CAS'd).
fn op_memo_v(
    sk: &secp256k1::SecretKey,
    addr: &str,
    op: Op,
    k: &str,
    v: Option<&str>,
    version: u64,
) -> String {
    let payload = if matches!(op, Op::Init) {
        signed_init_payload(addr)
    } else {
        let domain = signing_domain(addr, op, version);
        signed_payload(&domain, op, k, v)
    };
    let sig = sign_command(sk, &payload);
    // The version rides on the wire as the compact signature-line prefix.
    let memo = build_memo(op, k, v, version, &sig).unwrap();
    match Memo::try_from(memo).unwrap() {
        Memo::Text(t) => t.to_string(),
        _ => unreachable!(),
    }
}

fn sample_ufvk(net: consensus::Network, seed: u8) -> UnifiedFullViewingKey {
    use zcash_keys::keys::UnifiedSpendingKey;
    UnifiedSpendingKey::from_seed(&net, &[seed; 32], zip32::AccountId::ZERO)
        .expect("derive USK")
        .to_unified_full_viewing_key()
}

#[test]
fn parse_rejects_control_char_in_key() {
    // A hand-crafted memo whose key carries a control byte (NUL here) is
    // malformed: keys must be control-char-free so `signed_payload` stays
    // injective across the key/value boundary (the NUL-malleability fix).
    let (sk, _pk) = keypair_from(9);
    let addr = "zkv1test1";

    // Two-section SETL form.
    let setl_payload = signed_payload(
        &signing_domain(addr, Op::SetL, 0),
        Op::SetL,
        "a\u{0}b",
        Some("v"),
    );
    let setl_sig = hex::encode(sign_command(&sk, &setl_payload));
    assert_eq!(
        parse_text_memo_detailed(&format!("ZKV0 SETL a\u{0}b 1\nv\n{setl_sig}")),
        Err(MemoReject::Malformed(MemoFormat::ControlCharInKey)),
    );

    // Two-section SET form and the newline-collapsed fallback.
    let set_payload = signed_payload(
        &signing_domain(addr, Op::Set, 0),
        Op::Set,
        "a\u{0}b",
        Some("v"),
    );
    let set_sig = hex::encode(sign_command(&sk, &set_payload));
    assert_eq!(
        parse_text_memo_detailed(&format!("ZKV0 SET a\u{0}b v\n{set_sig}")),
        Err(MemoReject::Malformed(MemoFormat::ControlCharInKey)),
    );
    assert_eq!(
        parse_text_memo_detailed(&format!("ZKV0 SET a\u{0}b v {set_sig}")),
        Err(MemoReject::Malformed(MemoFormat::ControlCharInKey)),
    );
}

#[test]
fn nul_in_value_is_still_allowed() {
    // The fix restricts only the KEY. A NUL inside the value (binary data
    // via SETL) remains fully supported and round-trips byte-for-byte.
    let (sk, _pk) = keypair_from(9);
    let addr = "zkv1test1";
    let memo = op_memo_v(&sk, addr, Op::SetL, "msg", Some("A\u{0}B"), 0);
    let cmd = parse_text_memo(&memo).expect("NUL-in-value memo parses");
    assert_eq!(cmd.key, "msg");
    assert_eq!(cmd.value.as_deref(), Some("A\u{0}B"));
}

#[test]
fn build_memo_rejects_control_char_in_key() {
    let sig = [0u8; SIG_LEN];
    assert!(build_memo(Op::Set, "a\u{0}b", Some("v"), 0, &sig).is_err());
    assert!(build_memo(Op::Del, "a\u{1}b", None, 0, &sig).is_err());
    // A normal key still builds.
    assert!(build_memo(Op::Set, "ab", Some("v"), 0, &sig).is_ok());
}

#[test]
fn replay_drops_nul_key_signature_forgery() {
    // Regression for the NUL-byte signature-malleability write forgery: an
    // owner's SETL with a NUL in its *value* is honored, but a captured
    // signature re-split into a NUL-bearing *key* (byte-identical signed
    // payload) is now dropped instead of creating a phantom,
    // owner-attributed key.
    let (sk, pk) = keypair_from(9);
    let addr = "zkv1test1";
    let legit = op_memo_v(&sk, addr, Op::SetL, "msg", Some("A\u{0}B"), 0);
    // The forged memo reuses the legit signature (the two payloads are
    // byte-identical) under the re-split key="msg\0A", value="B".
    let sig = sign_command(
        &sk,
        &signed_payload(
            &signing_domain(addr, Op::SetL, 0),
            Op::SetL,
            "msg",
            Some("A\u{0}B"),
        ),
    );
    let forged = format!("ZKV0 SETL msg\u{0}A 1\nB\n{}", hex::encode(sig));
    let res = replay(
        vec![
            (init_memo(&sk, addr), WriteStatus::Confirmed),
            (legit, WriteStatus::Confirmed),
            (forged, WriteStatus::Confirmed),
        ],
        addr,
        &pk,
        false,
    )
    .unwrap();
    assert_eq!(
        res.state.get("msg").unwrap().confirmed.as_deref(),
        Some("A\u{0}B"),
        "legit NUL-in-value write must still apply"
    );
    assert!(
        !res.state.contains_key("msg\u{0}A"),
        "forged NUL-in-key write must be dropped"
    );
}

#[test]
fn zkv_addr_encodes_under_zkv_hrp_and_round_trips() {
    for (net, nt, hrp, uhrp) in [
        (
            consensus::Network::MainNetwork,
            NetworkType::Main,
            "zkv1",
            "uview1",
        ),
        (
            consensus::Network::TestNetwork,
            NetworkType::Test,
            "zkvtest1",
            "uviewtest1",
        ),
    ] {
        let ufvk = sample_ufvk(net, 0x7a);
        let addr = encode_zkv_addr(&ufvk, &net, ShieldedPool::Orchard, 1_234_567).unwrap();
        // The address is a single bech32m token under the zkv HRP (no colon,
        // no birthday suffix).
        assert!(addr.starts_with(hrp), "got {addr}");
        assert!(!addr.contains(':'), "address must not contain a colon");

        let parsed = parse_zkv_addr(&addr).expect("parses");
        assert_eq!(parsed.network, nt);
        // An Orchard-receiver address resolves to Ironwood on every network
        // now that NU6.3 is active on mainnet; they share the receiver, so
        // importing an old Orchard wallet is lossless.
        assert_eq!(parsed.pool, ShieldedPool::Ironwood);
        assert_eq!(
            parsed.birthday, 1_234_567,
            "birthday rides inside the meta item"
        );

        // Re-encoding the parsed key reproduces the exact address (the meta
        // item + HRP are deterministic).
        let addr2 =
            encode_zkv_addr(&parsed.ufvk, &net, ShieldedPool::Orchard, parsed.birthday).unwrap();
        assert_eq!(addr, addr2);

        // `--view-key`: the same bytes under the standard uview HRP, which
        // decodes in standard tooling (wallets ignore the zkv-meta item).
        let uview = zkv_addr_to_uview(&addr).unwrap();
        assert!(uview.starts_with(uhrp), "got {uview}");
        let (decoded_net, _) = unified::Ufvk::decode(&uview).expect("standard Ufvk decode");
        assert_eq!(decoded_net, nt);
    }
}

#[test]
fn parse_zkv_addr_rejects_plain_uview_and_missing_meta() {
    let net = consensus::Network::TestNetwork;
    let ufvk = sample_ufvk(net, 0x55);
    // A plain uview (no zkv HRP, no meta) is not a zkv address; the
    // rejection must guide the user toward enabling it for zkv.
    let plain_uview = encode_ufvk_for_pool(&ufvk, &net, ShieldedPool::Orchard);
    let uview_err = parse_zkv_addr(&plain_uview).unwrap_err().to_string();
    assert!(
        uview_err.contains("viewing key") && uview_err.contains("enabled for zkv"),
        "uview rejection should be friendly + actionable: {uview_err}"
    );
    // Same key/birthday relabeled to the zkv HRP but WITHOUT the meta item:
    // valid bech32 under a zkv HRP, but missing the mandatory metadata.
    let zkv_no_meta = relabel_hrp(&plain_uview, zkv_hrp(NetworkType::Test)).unwrap();
    let err = parse_zkv_addr(&zkv_no_meta).unwrap_err().to_string();
    assert!(err.contains("zkv metadata"), "unexpected error: {err}");
}

#[test]
fn sig_line_encode_parse_round_trips() {
    // 130-char hex stub standing in for a real signature (content is opaque
    // to the framing).
    let sig = "ab".repeat(SIG_LEN);
    assert_eq!(sig.len(), SIG_HEX_LEN);
    for seq in [
        0u64,
        1,
        2,
        127,
        255,
        256,
        4_200_000,
        u32::MAX as u64,
        u64::MAX,
    ] {
        let line = encode_sig_line(seq, &sig);
        assert!(
            line.ends_with(&sig),
            "sig must be the trailing run (seq {seq})"
        );
        if seq == 0 {
            // The common case costs zero extra characters.
            assert_eq!(line, sig, "seq 0 is a bare signature");
        } else {
            assert!(line.len() > sig.len(), "seq {seq} adds a prefix");
            // All-hex, even length (no separator).
            assert!(line.chars().all(|c| c.is_ascii_hexdigit()));
        }
        let (got_seq, got_sig) = parse_sig_line(&line).expect("round-trips");
        assert_eq!(got_seq, seq);
        assert_eq!(got_sig, sig);
    }
    // A four-byte counter (~4.2M, an every-block oracle after a decade)
    // costs 6 hex chars, shorter than the decimal "4200000:" alternative.
    assert_eq!(encode_sig_line(4_200_000, &sig).len(), sig.len() + 6);
}

#[test]
fn sig_line_rejects_noncanonical_and_malformed() {
    let sig = "ab".repeat(SIG_LEN);
    let bad = |t: String| {
        assert!(
            parse_sig_line(&t).is_err(),
            "should reject: {}",
            &t[..t.len().min(12)]
        )
    };
    bad(format!("0001{sig}")); // leading zero byte: non-canonical seq
    bad(format!("1{sig}")); // odd-length prefix
    bad(format!("{}{sig}", "01".repeat(MAX_SEQ_BYTES + 1))); // wider than u64
    bad(format!("zz{sig}")); // non-hex prefix
    bad("abcd".to_string()); // shorter than a signature
                             // A bare signature decodes as sequence 0.
    assert_eq!(parse_sig_line(&sig).unwrap(), (0, sig));
}

#[test]
fn wire_carries_compact_sequence() {
    let (sk, _pk) = keypair();
    let addr = "zkv1test1";
    // A SET at sequence 7 (a same-key follow-up write).
    let memo = op_memo_v(&sk, addr, Op::Set, "k", Some("v"), 7);
    let cmd = parse_text_memo(&memo).expect("parses");
    assert_eq!(cmd.op, Op::Set);
    assert_eq!(cmd.key, "k");
    assert_eq!(cmd.value.as_deref(), Some("v"));
    assert_eq!(cmd.seq, 7, "sequence decodes from the wire");
    assert_eq!(cmd.sig_hex.len(), SIG_HEX_LEN);
    // Sequence 7 is a single byte → 2 hex chars in front of the signature.
    let sig_line = memo.lines().last().unwrap();
    assert_eq!(sig_line.len(), SIG_HEX_LEN + 2);
}

#[test]
fn init_wire_has_no_sequence_prefix() {
    let (sk, _pk) = keypair();
    let addr = "zkv1test1";
    let memo = init_memo(&sk, addr);
    let cmd = parse_text_memo(&memo).expect("parses");
    assert_eq!(cmd.op, Op::Init);
    assert_eq!(cmd.seq, 0, "INIT is not version-CAS'd");
    let sig_line = memo.lines().last().unwrap();
    assert_eq!(
        sig_line.len(),
        SIG_HEX_LEN,
        "INIT signature line is a bare signature (no prefix)"
    );
}

#[test]
fn collapsed_memo_preserves_sequence() {
    let (sk, _pk) = keypair();
    let addr = "zkv1test1";
    let memo = op_memo_v(&sk, addr, Op::Set, "k", Some("v"), 5);
    // Simulate a broadcaster wallet that flattened the newline to a space.
    let collapsed = memo.replace('\n', " ");
    let cmd = parse_text_memo(&collapsed).expect("collapsed memo parses");
    assert_eq!(cmd.op, Op::Set);
    assert_eq!(cmd.key, "k");
    assert_eq!(cmd.value.as_deref(), Some("v"));
    assert_eq!(cmd.seq, 5, "sequence survives newline collapse");
}

#[test]
fn replay_drops_stale_sequence_set() {
    // End-to-end through `replay` (not just the audit): a verbatim
    // re-broadcast of the original create signs a stale sequence, so it is
    // dropped and cannot revert the value.
    let (sk, pk) = keypair();
    let addr = "zkv1test1";
    let create = op_memo_v(&sk, addr, Op::Set, "k", Some("v1"), 0);
    let update = op_memo_v(&sk, addr, Op::Set, "k", Some("v2"), 1);
    let res = replay(
        vec![
            (init_memo(&sk, addr), WriteStatus::Confirmed),
            (create.clone(), WriteStatus::Confirmed),
            (update, WriteStatus::Confirmed),
            // Replay the original create (sequence 0) after "k" advanced.
            (create, WriteStatus::Confirmed),
        ],
        addr,
        &pk,
        false,
    )
    .unwrap();
    assert_eq!(
        res.state.get("k").unwrap().confirmed.as_deref(),
        Some("v2"),
        "stale-sequence replay must not revert the value"
    );
    assert_eq!(res.kv_versions.get("k").copied(), Some(2));
}

#[test]
fn replay_accepts_forward_gap_within_window() {
    // F2: a single in-flight write that never confirms leaves a gap; the
    // next write signs one sequence ahead of what was actually honored. The
    // bounded-forward window accepts it instead of stranding the stream.
    let (sk, pk) = keypair();
    let addr = "zkv1test1";
    let res = replay(
        vec![
            (init_memo(&sk, addr), WriteStatus::Confirmed),
            // Honored create at seq 0 (the "seq 1" write evaporated, never
            // mined, so the chain high-water stays at 1).
            (
                op_memo_v(&sk, addr, Op::Set, "k", Some("v0"), 0),
                WriteStatus::Confirmed,
            ),
            // Next confirmed write signs seq 2 (the writer counted its lost
            // in-flight op). expected = 1, a forward gap of 1; still honored.
            (
                op_memo_v(&sk, addr, Op::Set, "k", Some("v2"), 2),
                WriteStatus::Confirmed,
            ),
        ],
        addr,
        &pk,
        false,
    )
    .unwrap();
    assert_eq!(
        res.state.get("k").unwrap().confirmed.as_deref(),
        Some("v2"),
        "a single missed write must not strand later writes"
    );
    // High-water jumped past the gap to seq + 1 = 3.
    assert_eq!(res.kv_versions.get("k").copied(), Some(3));
}

#[test]
fn replay_drops_sequence_beyond_window() {
    // A sequence past the window (a desync larger than tolerated, or a
    // freeze attempt jumping the counter way ahead) is rejected.
    let (sk, pk) = keypair();
    let addr = "zkv1test1";
    let res = replay(
        vec![
            (init_memo(&sk, addr), WriteStatus::Confirmed),
            (
                op_memo_v(&sk, addr, Op::Set, "k", Some("v"), VERSION_WINDOW + 1),
                WriteStatus::Confirmed,
            ),
        ],
        addr,
        &pk,
        false,
    )
    .unwrap();
    assert!(
        res.state.get("k").is_none_or(|ks| ks.confirmed.is_none()),
        "a beyond-window sequence must be dropped"
    );
    assert_eq!(res.kv_versions.get("k").copied(), None);
}

#[test]
fn pending_management_advances_target_window() {
    // F1: two back-to-back *pending* management ops to the same target. The
    // first advances the target's high-water even while pending, so the
    // second (signed one sequence ahead) verifies in the live tail rather
    // than colliding on a stale sequence; symmetric with pending data ops.
    let (root_sk, root_pk) = keypair();
    let (_w_sk, w_pk) = keypair_from(0x33);
    let addr = "zkv1test1";
    let w = pubkey_bech32(&w_pk);
    let confirming = WriteStatus::Confirming {
        done: 0,
        required: 3,
    };
    let res = replay(
        vec![
            (init_memo(&root_sk, addr), WriteStatus::Confirmed),
            // Grant CREATE (pending, target seq 0).
            (
                op_memo_v(&root_sk, addr, Op::WriterAdd, &w, Some("CREATE"), 0),
                confirming.clone(),
            ),
            // Immediately overwrite the scope (pending, target seq 1).
            (
                op_memo_v(&root_sk, addr, Op::WriterAdd, &w, Some("UPDATE"), 1),
                confirming.clone(),
            ),
        ],
        addr,
        &root_pk,
        false,
    )
    .unwrap();
    // Both pending management ops advanced the target high-water (to seq + 1
    // = 2), proving the pending bump fired (without it the second would have
    // expected seq 0).
    assert_eq!(res.target_versions.get(&w).copied(), Some(2));
    // ...but a pending grant still confers no authority yet.
    assert!(res.auth.authority_of(&w).is_none());
}

#[test]
fn receiver_domain_binds_network() {
    // F6: the network is folded into the signing domain, so a memo signed for
    // one network can never verify on another; this is load-bearing, not merely an
    // emergent property of ZIP-32 coin types (testnet and regtest even share
    // a coin type, so the receiver bytes alone can't separate them).
    use zcash_keys::keys::UnifiedSpendingKey;
    let seed = [0x5cu8; 32];
    let main = consensus::Network::MainNetwork;
    let ufvk = UnifiedSpendingKey::from_seed(&main, &seed, zip32::AccountId::ZERO)
        .unwrap()
        .to_unified_full_viewing_key();
    let dom_main = receiver_domain(&ufvk, ShieldedPool::Orchard, NetworkType::Main).unwrap();
    let dom_test = receiver_domain(&ufvk, ShieldedPool::Orchard, NetworkType::Test).unwrap();
    let dom_reg = receiver_domain(&ufvk, ShieldedPool::Orchard, NetworkType::Regtest).unwrap();
    assert!(dom_main.starts_with("main:"));
    assert!(dom_test.starts_with("test:"));
    assert!(dom_reg.starts_with("regtest:"));
    // Same UFVK / same receiver bytes, but the network tag separates them.
    assert_ne!(dom_main, dom_test);
    assert_ne!(dom_test, dom_reg);
}

#[test]
fn pubkey_bech32_round_trips_through_parse() {
    let (_, pk) = keypair();
    let encoded = pubkey_bech32(&pk);
    assert!(encoded.starts_with("zkvid1"), "got {encoded}");
    assert_eq!(parse_pubkey(&encoded), Some(pk));
}

#[test]
fn parse_pubkey_accepts_hex_and_bech32_interchangeably() {
    let (_, pk) = keypair_from(9);
    let canonical = pubkey_bech32(&pk); // zkvid1…
    let compressed_hex = hex::encode(pk.serialize()); // 02/03…, 66 chars
    let uncompressed_hex = hex::encode(pk.serialize_uncompressed()); // 04…, 130 chars

    // All three input forms parse to the identical key, and re-encode to the
    // one canonical zkvid1… form the wire + registry key on.
    for form in [
        canonical.as_str(),
        compressed_hex.as_str(),
        uncompressed_hex.as_str(),
    ] {
        assert_eq!(parse_pubkey(form), Some(pk));
        assert_eq!(pubkey_bech32(&parse_pubkey(form).unwrap()), canonical);
    }
}

#[test]
fn parse_pubkey_rejects_corrupted_checksum() {
    let (_, pk) = keypair_from(13);
    let good = pubkey_bech32(&pk);
    // Flip the final (checksum) character to another valid Bech32 char.
    // Bech32m detects any single substitution, so the typo must be rejected
    // outright, not silently decoded to some other key.
    let mut chars: Vec<char> = good.chars().collect();
    let last = chars.len() - 1;
    chars[last] = if chars[last] == 'q' { 'p' } else { 'q' };
    let corrupted: String = chars.into_iter().collect();
    assert_ne!(corrupted, good);
    assert_eq!(parse_pubkey(&corrupted), None);
}

#[test]
fn parse_pubkey_rejects_wrong_hrp_and_junk() {
    let (_, pk) = keypair_from(17);
    // Right bytes, wrong HRP (e.g. the `zid` we explicitly did *not* pick, or
    // a UA-shaped `u`): not our key, and not hex, so it parses to nothing.
    let wrong_hrp =
        bech32::encode::<Bech32m>(Hrp::parse_unchecked("zid"), &pk.serialize()).unwrap();
    let ua_like = bech32::encode::<Bech32m>(Hrp::parse_unchecked("u"), &pk.serialize()).unwrap();
    assert_eq!(parse_pubkey(&wrong_hrp), None);
    assert_eq!(parse_pubkey(&ua_like), None);
    assert_eq!(parse_pubkey("not a key"), None);
    assert_eq!(parse_pubkey(""), None);
    assert_eq!(parse_pubkey("02abcd"), None); // valid hex, wrong length
}

#[test]
fn looks_like_zkv_matches_valid_and_invalid_markers() {
    // Valid ops.
    assert!(looks_like_zkv("ZKV0 SET k v\n<sig>"));
    assert!(looks_like_zkv("ZKV0 INIT zkv1abc\n<sig>"));
    // Malformed / unknown-opcode ZKV0 (parse_text_memo would reject these,
    // but they must still be excluded from the funding view).
    assert!(looks_like_zkv("ZKV0 BOGUS x"));
    assert!(looks_like_zkv("ZKV0"));
    // Leading whitespace tolerated.
    assert!(looks_like_zkv("   ZKV0 SET k v"));
    // Not a ZKV0 marker.
    assert!(!looks_like_zkv("ZKV0234 not us"));
    assert!(!looks_like_zkv("hello world"));
    assert!(!looks_like_zkv(""));
    assert!(!looks_like_zkv("thanks for the ZKV0 mention"));
    // A first-line comment above the header is still recognized as zkv.
    assert!(looks_like_zkv("# just testing\nZKV0 SET abc 123\n<sig>"));
    // A bare `#` line with no command is not zkv traffic.
    assert!(!looks_like_zkv("# orphan comment, no newline"));
    assert!(!looks_like_zkv("# foo\nhello world"));
}

/// Build a signed memo for `addr` carrying a first-line `comment`, at the
/// entity's `version`. Mirrors `op_memo_v` but exercises the comment path:
/// the payload binds the comment (via [`payload_for`]) and the wire carries
/// the `#…` line.
fn op_memo_commented(
    sk: &secp256k1::SecretKey,
    addr: &str,
    op: Op,
    k: &str,
    v: Option<&str>,
    version: u64,
    comment: &str,
) -> String {
    let cmd = ZkvCommand {
        op,
        key: k.to_owned(),
        value: v.map(str::to_owned),
        seq: version,
        sig_hex: String::new(),
        comment: Some(comment.to_owned()),
    };
    let sig = sign_command(sk, &payload_for(addr, &cmd));
    let memo = build_memo_with_comment(op, k, v, version, &sig, Some(comment)).unwrap();
    match Memo::try_from(memo).unwrap() {
        Memo::Text(t) => t.to_string(),
        _ => unreachable!(),
    }
}

#[test]
fn comment_round_trips_through_build_and_parse() {
    let (sk, pk) = keypair();
    let addr = "main:deadbeef";
    let text = op_memo_commented(&sk, addr, Op::Set, "k", Some("v"), 0, " just testing");
    // Wire layout: comment line, then header, then signature.
    let mut lines = text.splitn(3, '\n');
    assert_eq!(lines.next(), Some("# just testing"));
    assert_eq!(lines.next(), Some("ZKV0 SET k v"));

    let cmd = parse_text_memo(&text).expect("parses");
    assert_eq!(cmd.op, Op::Set);
    assert_eq!(cmd.key, "k");
    assert_eq!(cmd.value.as_deref(), Some("v"));
    assert_eq!(cmd.comment.as_deref(), Some(" just testing"));
    // The comment is signed: recovering over the reconstructed payload
    // (which folds the comment in) yields the real signer.
    let signer = recover_signer(&payload_for(addr, &cmd), &cmd.sig_hex).unwrap();
    assert_eq!(signer, pk);
}

#[test]
fn empty_comment_round_trips_and_is_distinct_from_none() {
    let (sk, pk) = keypair();
    let addr = "main:deadbeef";
    let text = op_memo_commented(&sk, addr, Op::Set, "k", Some("v"), 0, "");
    let cmd = parse_text_memo(&text).expect("parses");
    assert_eq!(cmd.comment.as_deref(), Some(""));
    assert_eq!(
        recover_signer(&payload_for(addr, &cmd), &cmd.sig_hex),
        Some(pk)
    );
    // An empty comment binds a different domain than no comment at all.
    assert_ne!(bind_comment(addr, Some("")), bind_comment(addr, None));
    assert_eq!(bind_comment(addr, None), addr);
}

#[test]
fn no_comment_payload_is_byte_identical_to_legacy() {
    // A None comment must leave both the signed payload and the wire memo
    // byte-for-byte identical to the pre-comment format (no ZKV_VERSION bump).
    let (sk, _pk) = keypair();
    let addr = "main:deadbeef";
    let domain = signing_domain(addr, Op::Set, 0);
    assert_eq!(bind_comment(&domain, None), domain);
    let sig = sign_command(&sk, &signed_payload(&domain, Op::Set, "k", Some("v")));
    let legacy = build_memo(Op::Set, "k", Some("v"), 0, &sig).unwrap();
    let with_none = build_memo_with_comment(Op::Set, "k", Some("v"), 0, &sig, None).unwrap();
    assert_eq!(
        Memo::try_from(legacy).unwrap(),
        Memo::try_from(with_none).unwrap()
    );
}

#[test]
fn altering_comment_breaks_the_signature() {
    let (sk, pk) = keypair();
    let addr = "main:deadbeef";
    let text = op_memo_commented(&sk, addr, Op::Set, "k", Some("v"), 0, "original");
    // The wire is faithfully parsed, but a verifier who flips the comment
    // text recovers a *different* key; the comment is under the signature.
    let mut tampered = parse_text_memo(&text).unwrap();
    assert_eq!(
        recover_signer(&payload_for(addr, &tampered), &tampered.sig_hex),
        Some(pk)
    );
    tampered.comment = Some("forged".to_owned());
    assert_ne!(
        recover_signer(&payload_for(addr, &tampered), &tampered.sig_hex),
        Some(pk)
    );
}

#[test]
fn stripping_or_adding_a_comment_breaks_the_signature() {
    let (sk, pk) = keypair();
    let addr = "main:deadbeef";
    // Signed WITH a comment, presented WITHOUT one → wrong signer recovered.
    let with = op_memo_commented(&sk, addr, Op::Set, "k", Some("v"), 0, "note");
    let mut stripped = parse_text_memo(&with).unwrap();
    stripped.comment = None;
    assert_ne!(
        recover_signer(&payload_for(addr, &stripped), &stripped.sig_hex),
        Some(pk)
    );

    // Signed WITHOUT a comment, presented WITH one → wrong signer recovered.
    let sig = sign_command(
        &sk,
        &signed_payload(&signing_domain(addr, Op::Set, 0), Op::Set, "k", Some("v")),
    );
    let bare = build_memo(Op::Set, "k", Some("v"), 0, &sig).unwrap();
    let bare_text = match Memo::try_from(bare).unwrap() {
        Memo::Text(t) => t.to_string(),
        _ => unreachable!(),
    };
    let mut added = parse_text_memo(&bare_text).unwrap();
    assert_eq!(
        recover_signer(&payload_for(addr, &added), &added.sig_hex),
        Some(pk)
    );
    added.comment = Some("sneaky".to_owned());
    assert_ne!(
        recover_signer(&payload_for(addr, &added), &added.sig_hex),
        Some(pk)
    );
}

#[test]
fn replay_applies_commented_write_and_drops_tampered_comment() {
    let (sk, pk) = keypair();
    let addr = "zkv1test1";
    // A correctly-signed commented SET applies just like an uncommented one.
    let good = replay(
        vec![
            (init_memo(&sk, addr), WriteStatus::Confirmed),
            (
                op_memo_commented(&sk, addr, Op::Set, "a", Some("v"), 0, "hello"),
                WriteStatus::Confirmed,
            ),
        ],
        addr,
        &pk,
        true,
    )
    .unwrap();
    assert_eq!(good.state.get("a").unwrap().confirmed.as_deref(), Some("v"));

    // Take that same commented memo and rewrite only the comment text on the
    // wire. The signature no longer covers it, so replay drops the write.
    let original = op_memo_commented(&sk, addr, Op::Set, "a", Some("v"), 0, "hello");
    assert!(original.starts_with("#hello\n"));
    let forged = original.replacen("#hello", "#forged", 1);
    assert_ne!(forged, original);
    let bad = replay(
        vec![
            (init_memo(&sk, addr), WriteStatus::Confirmed),
            (forged, WriteStatus::Confirmed),
        ],
        addr,
        &pk,
        false,
    )
    .unwrap();
    assert!(
        !bad.state.contains_key("a"),
        "forged-comment write must be dropped"
    );
}

#[test]
fn build_memo_rejects_multiline_comment() {
    let (sk, _pk) = keypair();
    let sig = sign_command(&sk, &signed_payload("main:dead", Op::Set, "k", Some("v")));
    build_memo_with_comment(Op::Set, "k", Some("v"), 0, &sig, Some("two\nlines"))
        .expect_err("a comment must be a single line");
}

#[test]
fn orphan_comment_line_is_foreign() {
    // A `#…` line with no terminating newline (hence no command) is foreign.
    assert_eq!(
        parse_text_memo_detailed("# nothing follows"),
        Err(MemoReject::NotZkv)
    );
    // A comment followed by non-zkv text is foreign too.
    assert_eq!(
        parse_text_memo_detailed("# c\nnot a zkv memo"),
        Err(MemoReject::NotZkv)
    );
}

#[test]
fn encode_ufvk_for_orchard_strips_sapling() {
    use zcash_keys::keys::UnifiedSpendingKey;

    let params = consensus::Network::TestNetwork;
    let seed = [0x7au8; 32];
    let usk =
        UnifiedSpendingKey::from_seed(&params, &seed, zip32::AccountId::ZERO).expect("derive USK");
    let ufvk = usk.to_unified_full_viewing_key();

    let full_encoded = ufvk.encode(&params);
    let (_, full) = unified::Ufvk::decode(&full_encoded).expect("decode full UFVK");
    assert!(
        full.items()
            .iter()
            .any(|i| matches!(i, unified::Fvk::Sapling(_))),
        "control: USK-derived UFVK should contain sapling",
    );

    let stripped = encode_ufvk_for_pool(&ufvk, &params, ShieldedPool::Orchard);
    let (_, stripped_container) = unified::Ufvk::decode(&stripped).expect("decode stripped");
    let items = stripped_container.items();
    assert!(
        !items.iter().any(|i| matches!(i, unified::Fvk::Sapling(_))),
        "Orchard-pool UFVK must not contain sapling",
    );
    assert!(
        items.iter().any(|i| matches!(i, unified::Fvk::P2pkh(_))),
        "Orchard-pool UFVK must retain transparent",
    );
    assert!(
        items.iter().any(|i| matches!(i, unified::Fvk::Orchard(_))),
        "Orchard-pool UFVK must retain orchard",
    );

    let zkv_addr = encode_zkv_addr(&ufvk, &params, ShieldedPool::Orchard, 3_000_000).unwrap();
    let parsed = parse_zkv_addr(&zkv_addr).expect("round-trip parses");
    assert_eq!(parsed.birthday, 3_000_000);
    // An Orchard-receiver address resolves to Ironwood (shared receiver).
    assert_eq!(parsed.pool, ShieldedPool::Ironwood);
}

/// Ironwood shares the Orchard receiver, so a wallet's Orchard and Ironwood
/// framings are indistinguishable at the address / receiver / signing layer.
/// This is what makes "import an old Orchard wallet as Ironwood" and
/// "auto-upgrade to Ironwood on the first send" lossless: the zkv address, the
/// funding UA, and the signing domain (which binds the receiver bytes, not the
/// pool label) are all byte-identical, so every historical signature still
/// verifies after the relabel and the reader sees the same memos.
#[test]
fn orchard_and_ironwood_are_receiver_identical() {
    for (net, nt) in [
        (consensus::Network::MainNetwork, NetworkType::Main),
        (consensus::Network::TestNetwork, NetworkType::Test),
    ] {
        let ufvk = sample_ufvk(net, 0x5a);

        // Same zkv address bytes under either framing.
        let orchard_addr = encode_zkv_addr(&ufvk, &net, ShieldedPool::Orchard, 999).unwrap();
        let ironwood_addr = encode_zkv_addr(&ufvk, &net, ShieldedPool::Ironwood, 999).unwrap();
        assert_eq!(
            orchard_addr, ironwood_addr,
            "Orchard and Ironwood must encode to the identical zkv address"
        );

        // Same signing domain (the raw receiver bytes): a signature produced
        // under the Orchard framing verifies under Ironwood and vice versa.
        let dom_orchard = receiver_domain(&ufvk, ShieldedPool::Orchard, nt).unwrap();
        let dom_ironwood = receiver_domain(&ufvk, ShieldedPool::Ironwood, nt).unwrap();
        assert_eq!(
            dom_orchard, dom_ironwood,
            "receiver domain must not depend on the Orchard/Ironwood label"
        );

        // Same funding UA (an Orchard receiver in both).
        let (ua_o, _) = ufvk
            .default_address(ua_request_for_pool(ShieldedPool::Orchard))
            .unwrap();
        let (ua_i, _) = ufvk
            .default_address(ua_request_for_pool(ShieldedPool::Ironwood))
            .unwrap();
        assert_eq!(ua_o.encode(&net), ua_i.encode(&net));

        // Importing an Orchard-receiver address yields Ironwood on every
        // network now that NU6.3 is active on mainnet; it reads the identical
        // memos, since the receiver is the same.
        assert_eq!(
            parse_zkv_addr(&orchard_addr).unwrap().pool,
            ShieldedPool::Ironwood
        );
    }
}

#[test]
fn encode_ufvk_for_sapling_strips_orchard() {
    use zcash_keys::keys::UnifiedSpendingKey;

    let params = consensus::Network::TestNetwork;
    let seed = [0x7au8; 32];
    let usk =
        UnifiedSpendingKey::from_seed(&params, &seed, zip32::AccountId::ZERO).expect("derive USK");
    let ufvk = usk.to_unified_full_viewing_key();

    let stripped = encode_ufvk_for_pool(&ufvk, &params, ShieldedPool::Sapling);
    let (_, stripped_container) = unified::Ufvk::decode(&stripped).expect("decode stripped");
    let items = stripped_container.items();
    assert!(
        !items.iter().any(|i| matches!(i, unified::Fvk::Orchard(_))),
        "Sapling-pool UFVK must not contain orchard",
    );
    assert!(
        items.iter().any(|i| matches!(i, unified::Fvk::P2pkh(_))),
        "Sapling-pool UFVK must retain transparent",
    );
    assert!(
        items.iter().any(|i| matches!(i, unified::Fvk::Sapling(_))),
        "Sapling-pool UFVK must retain sapling",
    );

    // A Sapling-only zkv address parses, infers the Sapling pool, and
    // still derives the (transparent-based) signing pubkey.
    let zkv_addr = encode_zkv_addr(&ufvk, &params, ShieldedPool::Sapling, 2_500_000).unwrap();
    let parsed = parse_zkv_addr(&zkv_addr).expect("sapling address parses");
    assert_eq!(parsed.birthday, 2_500_000);
    assert_eq!(parsed.pool, ShieldedPool::Sapling);
    zkv_verifying_pubkey(&parsed.ufvk).expect("sapling address still has a signing pubkey");
}

/// The funding/recipient UA a database publishes must carry ONLY its single
/// shielded receiver and never a transparent (p2pkh) one: zkv funds and
/// writes are shielded-only, even though the UFVK itself retains its
/// transparent component for signing-key derivation. Guards every
/// `default_address(ua_request_for_pool(..))` call site (show/inspect/the
/// facade funding address/the faucet) against re-introducing a t-address.
#[test]
fn funding_ua_never_includes_a_transparent_receiver() {
    let params = consensus::Network::TestNetwork;
    let ufvk = sample_ufvk(params, 0x42);

    for pool in [
        ShieldedPool::Orchard,
        ShieldedPool::Sapling,
        ShieldedPool::Ironwood,
    ] {
        let (ua, _) = ufvk
            .default_address(ua_request_for_pool(pool))
            .unwrap_or_else(|e| panic!("derive {pool:?} funding UA: {e}"));
        assert!(
            !ua.has_transparent(),
            "{pool:?} funding UA must not carry a transparent receiver",
        );
        assert!(
            ua.transparent().is_none(),
            "{pool:?} funding UA transparent() must be None",
        );
        // The matching shielded receiver IS present (sanity: we didn't
        // produce an empty/wrong-pool UA).
        match pool {
            // Ironwood shares the Orchard receiver.
            ShieldedPool::Orchard | ShieldedPool::Ironwood => assert!(
                ua.orchard().is_some() && ua.sapling().is_none(),
                "Orchard/Ironwood funding UA must carry only an orchard receiver",
            ),
            ShieldedPool::Sapling => assert!(
                ua.sapling().is_some() && ua.orchard().is_none(),
                "Sapling funding UA must carry only a sapling receiver",
            ),
        }
    }
}

#[test]
fn sign_verify_set_round_trip() {
    let (sk, pk) = keypair();
    let addr = "zkv1uview1example3000000";
    let payload = signed_payload(addr, Op::Set, "zec_usd_price", Some("123.45"));
    let sig = sign_command(&sk, &payload);
    let sig_hex = hex::encode(sig);
    assert!(verify_command(&pk, &payload, &sig_hex));
}

#[test]
fn sign_verify_del_round_trip() {
    let (sk, pk) = keypair();
    let addr = "zkv1uview1example3000000";
    let payload = signed_payload(addr, Op::Del, "zec_usd_price", None);
    let sig = sign_command(&sk, &payload);
    let sig_hex = hex::encode(sig);
    assert!(verify_command(&pk, &payload, &sig_hex));
}

#[test]
fn signature_does_not_verify_for_different_address() {
    let (sk, pk) = keypair();
    let addr_a = "zkv1uview1a3000000";
    let addr_b = "zkv1uview1b3000000";
    let payload_a = signed_payload(addr_a, Op::Set, "k", Some("v"));
    let sig = sign_command(&sk, &payload_a);
    let sig_hex = hex::encode(sig);
    let payload_b = signed_payload(addr_b, Op::Set, "k", Some("v"));
    assert!(!verify_command(&pk, &payload_b, &sig_hex));
}

#[test]
fn memo_round_trip() {
    let (sk, pk) = keypair();
    let addr = "zkv1uview1example3000000";
    let payload = signed_payload(addr, Op::Set, "greeting", Some("hello world"));
    let sig = sign_command(&sk, &payload);
    let memo = build_memo(Op::Set, "greeting", Some("hello world"), 0, &sig).unwrap();

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
    let addr = "zkv1test1";

    let entries = vec![
        (init_memo(&sk, addr), WriteStatus::Confirmed),
        (
            op_memo(&sk, addr, Op::Set, "a", Some("1")),
            WriteStatus::Confirmed,
        ),
        (
            op_memo(&sk, addr, Op::Set, "b", Some("2")),
            WriteStatus::Confirmed,
        ),
        (
            // Second write to "a" (version 1).
            op_memo_v(&sk, addr, Op::Set, "a", Some("3"), 1),
            WriteStatus::Confirmed,
        ),
        (
            // Second write to "b" (after its create), version 1.
            op_memo_v(&sk, addr, Op::Del, "b", None, 1),
            WriteStatus::Confirmed,
        ),
    ];

    let result = replay(entries, addr, &pk, true).unwrap();
    assert_eq!(result.init, InitState::Initialized);
    let state = &result.state;
    assert_eq!(
        state.get("a").and_then(|ks| ks.confirmed.as_deref()),
        Some("3")
    );
    assert!(state.get("a").unwrap().pending.is_empty());
    assert!(state.get("b").is_none(), "DEL'd key is pruned");
}

#[test]
fn replay_separates_confirmed_from_pending() {
    let (sk, pk) = keypair();
    let addr = "zkv1test1";

    // Confirmed value plus a self-sent pending overwrite on `a`, and a
    // separately-stable `b` with no pending.
    let entries = vec![
        (init_memo(&sk, addr), WriteStatus::Confirmed),
        (
            op_memo(&sk, addr, Op::Set, "a", Some("old")),
            WriteStatus::Confirmed,
        ),
        (
            op_memo(&sk, addr, Op::Set, "b", Some("stable")),
            WriteStatus::Confirmed,
        ),
        (
            // Pending overwrite of "a" at version 1 (a pending op advances
            // the per-key version too, so it verifies in the live tail).
            op_memo_v(&sk, addr, Op::Set, "a", Some("new"), 1),
            WriteStatus::Confirming {
                done: 0,
                required: 3,
            },
        ),
    ];
    let result = replay(entries, addr, &pk, true).unwrap();
    assert_eq!(result.init, InitState::Initialized);
    let state = &result.state;
    let a = state.get("a").unwrap();
    assert_eq!(a.confirmed.as_deref(), Some("old"));
    assert_eq!(
        a.pending,
        vec![PendingOp::Set {
            value: "new".into(),
            done: 0,
            required: 3,
            txid: String::new(),
        }]
    );
    let b = state.get("b").unwrap();
    assert_eq!(b.confirmed.as_deref(), Some("stable"));
    assert!(b.pending.is_empty());
}

#[test]
fn replay_keeps_new_key_pending_set_without_confirmed() {
    let (sk, pk) = keypair();
    let addr = "zkv1test1";

    let result = replay(
        vec![
            (init_memo(&sk, addr), WriteStatus::Confirmed),
            (
                op_memo(&sk, addr, Op::Set, "a", Some("v")),
                WriteStatus::Confirming {
                    done: 0,
                    required: 3,
                },
            ),
        ],
        addr,
        &pk,
        true,
    )
    .unwrap();
    assert_eq!(result.init, InitState::Initialized);
    let a = result.state.get("a").unwrap();
    assert!(a.confirmed.is_none());
    assert_eq!(
        a.pending,
        vec![PendingOp::Set {
            value: "v".into(),
            done: 0,
            required: 3,
            txid: String::new(),
        }]
    );
}

#[test]
fn replay_prunes_pending_del_on_nonexistent_key() {
    let (sk, pk) = keypair();
    let addr = "zkv1test1";

    let result = replay(
        vec![
            (init_memo(&sk, addr), WriteStatus::Confirmed),
            (
                op_memo(&sk, addr, Op::Del, "a", None),
                WriteStatus::Confirming {
                    done: 0,
                    required: 3,
                },
            ),
        ],
        addr,
        &pk,
        true,
    )
    .unwrap();
    assert_eq!(result.init, InitState::Initialized);
    assert!(result.state.is_empty());
}

#[test]
fn replay_accumulates_multiple_pending_ops() {
    let (sk, pk) = keypair();
    let addr = "zkv1test1";

    let entries = vec![
        (init_memo(&sk, addr), WriteStatus::Confirmed),
        (
            op_memo(&sk, addr, Op::Set, "a", Some("v")),
            WriteStatus::Confirmed,
        ),
        (
            // Pending DEL of "a" at version 1 (after the confirmed SET).
            op_memo_v(&sk, addr, Op::Del, "a", None, 1),
            WriteStatus::Confirming {
                done: 0,
                required: 3,
            },
        ),
        (
            // Pending SET of "a" at version 2 (a writer's own back-to-back
            // pending ops each advance the version so all stay visible).
            op_memo_v(&sk, addr, Op::Set, "a", Some("w"), 2),
            WriteStatus::Confirming {
                done: 0,
                required: 3,
            },
        ),
    ];
    let result = replay(entries, addr, &pk, true).unwrap();
    assert_eq!(result.init, InitState::Initialized);
    let a = result.state.get("a").unwrap();
    assert_eq!(a.confirmed.as_deref(), Some("v"));
    assert_eq!(a.pending.len(), 2);
    assert!(matches!(a.pending[0], PendingOp::Del { .. }));
    assert!(matches!(a.pending[1], PendingOp::Set { .. }));
}

#[test]
fn parse_recovers_when_newline_collapsed_to_whitespace() {
    let (sk, pk) = keypair();
    let addr = "zkv1test1";
    let payload = signed_payload(addr, Op::Set, "zec_usd_price", Some("1008.33"));
    let sig = sign_command(&sk, &payload);
    let sig_hex = hex::encode(sig);

    let mangled = format!("ZKV0 SET zec_usd_price 1008.33                       {sig_hex}");
    let cmd = parse_text_memo(&mangled).expect("parses despite missing newline");
    assert!(matches!(cmd.op, Op::Set));
    assert_eq!(cmd.key, "zec_usd_price");
    assert_eq!(cmd.value.as_deref(), Some("1008.33"));

    let recomputed = signed_payload(addr, cmd.op, &cmd.key, cmd.value.as_deref());
    assert!(verify_command(&pk, &recomputed, &cmd.sig_hex));
}

#[test]
fn replay_drops_memo_with_no_signature() {
    // A memo body in zkv command shape but lacking any signature. The
    // parser's no-newline fallback requires a trailing 130-hex tail; this
    // memo is far shorter than that, so `parse_text_memo` returns None and
    // replay silently drops it. Even with a valid INIT prelude (so the DB
    // is initialized), the unsigned SET must not appear.
    let (sk, pk) = keypair();
    let addr = "zkv1test1";
    let unsigned = "ZKV0 SET a 1".to_owned();

    let result = replay(
        vec![
            (init_memo(&sk, addr), WriteStatus::Confirmed),
            (unsigned, WriteStatus::Confirmed),
        ],
        addr,
        &pk,
        false,
    )
    .unwrap();
    assert_eq!(result.init, InitState::Initialized);
    assert!(result.state.is_empty());
}

#[test]
fn replay_drops_memo_signed_by_different_keypair() {
    // Cross-wallet attack: an attacker writes a memo to the victim's
    // funding UA, signed with the attacker's own secp256k1 key. The memo
    // decrypts (it's just text), but its signature must not verify under
    // the victim's pubkey and so replay must drop it. Pair it with a
    // valid INIT signed by the victim so the rejection is on signature
    // grounds (not pre-INIT noise).
    let (sk_attacker, _) = keypair_from(0x11);
    let (sk_victim, pk_victim) = keypair_from(0x42);
    let addr = "zkv1test1";

    let attacker_set = op_memo(&sk_attacker, addr, Op::Set, "a", Some("attacker_value"));

    let result = replay(
        vec![
            (init_memo(&sk_victim, addr), WriteStatus::Confirmed),
            (attacker_set, WriteStatus::Confirmed),
        ],
        addr,
        &pk_victim,
        false,
    )
    .unwrap();
    assert_eq!(result.init, InitState::Initialized);
    assert!(
        result.state.is_empty(),
        "memo signed by different key must not enter state",
    );
}

#[test]
fn replay_strict_bails_on_bad_sig() {
    let (_, pk) = keypair();
    let addr = "zkv1test1";

    let bad_sig = [0u8; 65];
    let memo = build_memo(Op::Set, "a", Some("1"), 0, &bad_sig).unwrap();
    let text = match Memo::try_from(memo).unwrap() {
        Memo::Text(t) => t.to_string(),
        _ => unreachable!(),
    };

    let err = replay(vec![(text, WriteStatus::Confirmed)], addr, &pk, true)
        .expect_err("strict mode must bail on bad signature");
    assert!(
        err.to_string().contains("invalid zkv signature"),
        "unexpected error: {err}",
    );
}

#[test]
fn replay_strict_bails_on_malformed_memo() {
    let (_, pk) = keypair();
    let addr = "zkv1test1";

    let err = replay(
        vec![(
            "ZKV0 BOGUS not a real op".to_owned(),
            WriteStatus::Confirmed,
        )],
        addr,
        &pk,
        true,
    )
    .expect_err("strict mode must bail on malformed memo");
    assert!(
        err.to_string().contains("malformed zkv memo"),
        "unexpected error: {err}",
    );
}

#[test]
fn replay_drops_non_zkv_text_memo() {
    // A plain user-written text memo (e.g., a personal note). Not a zkv
    // command at all; `parse_text_memo` returns None and replay drops it.
    let (sk, pk) = keypair();
    let addr = "zkv1test1";

    let result = replay(
        vec![
            (init_memo(&sk, addr), WriteStatus::Confirmed),
            ("hello world".to_owned(), WriteStatus::Confirmed),
        ],
        addr,
        &pk,
        false,
    )
    .unwrap();
    assert_eq!(result.init, InitState::Initialized);
    assert!(result.state.is_empty());
}

#[test]
fn replay_silently_drops_invalid_sigs() {
    let (sk, pk) = keypair();
    let addr = "zkv1test1";

    let good_text = op_memo(&sk, addr, Op::Set, "a", Some("1"));

    let bad_sig = [0u8; 65];
    let bad = build_memo(Op::Set, "b", Some("2"), 0, &bad_sig).unwrap();
    let bad_text = match Memo::try_from(bad).unwrap() {
        Memo::Text(t) => t.to_string(),
        _ => unreachable!(),
    };

    let result = replay(
        vec![
            (init_memo(&sk, addr), WriteStatus::Confirmed),
            (good_text, WriteStatus::Confirmed),
            (bad_text, WriteStatus::Confirmed),
        ],
        addr,
        &pk,
        false,
    )
    .unwrap();
    assert_eq!(result.init, InitState::Initialized);
    assert_eq!(
        result.state.get("a").and_then(|ks| ks.confirmed.as_deref()),
        Some("1"),
    );
    assert!(!result.state.contains_key("b"));
}

#[test]
fn build_memo_rejects_empty_set_value() {
    // Empty values can't be safely framed in the current wire format
    // (they encode as a trailing space before the newline, which any
    // whitespace-stripping transport silently drops). Reject at the
    // writer so callers see a loud error instead of broadcasting a memo
    // that some readers will parse and others will drop.
    let sig = [0u8; 65];
    let err = build_memo(Op::Set, "k", Some(""), 0, &sig)
        .expect_err("empty value must be rejected at build time");
    assert!(
        err.to_string().contains("must not be empty"),
        "unexpected error: {err}",
    );
}

#[test]
fn parse_drops_set_with_empty_value() {
    // Defense-in-depth: even if some encoder produced the trailing-space
    // form, the parser must drop it rather than yield Some("") and
    // pretend the write was intentional. Construct the wire form by
    // hand because build_memo now refuses to.
    let (sk, _) = keypair();
    let addr = "zkv1test1";
    let payload = signed_payload(addr, Op::Set, "k", Some(""));
    let sig = sign_command(&sk, &payload);
    let text = format!("ZKV0 SET k \n{}", hex::encode(sig));
    assert!(
        parse_text_memo(&text).is_none(),
        "SET memo with empty value must not parse",
    );
}

// ----- SETL (length-framed SET) tests -----

#[test]
fn setl_round_trip_normal_value() {
    let (sk, pk) = keypair();
    let addr = "zkv1test1";
    let value = "hello world";
    let payload = signed_payload(addr, Op::SetL, "greeting", Some(value));
    let sig = sign_command(&sk, &payload);
    let memo = build_memo(Op::SetL, "greeting", Some(value), 0, &sig).unwrap();
    let text = match Memo::try_from(memo).unwrap() {
        Memo::Text(t) => t.to_string(),
        _ => panic!("expected text memo"),
    };

    let cmd = parse_text_memo(&text).expect("SETL must parse");
    assert_eq!(cmd.op, Op::SetL);
    assert_eq!(cmd.key, "greeting");
    assert_eq!(cmd.value.as_deref(), Some(value));
    let recomputed = signed_payload(addr, cmd.op, &cmd.key, cmd.value.as_deref());
    assert!(verify_command(&pk, &recomputed, &cmd.sig_hex));
}

#[test]
fn setl_round_trip_empty_value() {
    // The whole point of SETL: empty values survive end-to-end.
    let (sk, pk) = keypair();
    let addr = "zkv1test1";
    let payload = signed_payload(addr, Op::SetL, "blank", Some(""));
    let sig = sign_command(&sk, &payload);
    let memo = build_memo(Op::SetL, "blank", Some(""), 0, &sig).unwrap();
    let text = match Memo::try_from(memo).unwrap() {
        Memo::Text(t) => t.to_string(),
        _ => panic!("expected text memo"),
    };
    let cmd = parse_text_memo(&text).expect("SETL with empty value must parse");
    assert_eq!(cmd.op, Op::SetL);
    assert_eq!(cmd.value.as_deref(), Some(""));
    let recomputed = signed_payload(addr, cmd.op, &cmd.key, cmd.value.as_deref());
    assert!(verify_command(&pk, &recomputed, &cmd.sig_hex));
}

#[test]
fn setl_round_trip_value_with_newlines() {
    // SETL's other reason to exist: newline-containing values survive.
    let (sk, pk) = keypair();
    let addr = "zkv1test1";
    let value = "line1\nline2\nline3";
    let payload = signed_payload(addr, Op::SetL, "multi", Some(value));
    let sig = sign_command(&sk, &payload);
    let memo = build_memo(Op::SetL, "multi", Some(value), 0, &sig).unwrap();
    let text = match Memo::try_from(memo).unwrap() {
        Memo::Text(t) => t.to_string(),
        _ => panic!("expected text memo"),
    };
    let cmd = parse_text_memo(&text).expect("SETL with newlines must parse");
    assert_eq!(cmd.value.as_deref(), Some(value));
    let recomputed = signed_payload(addr, cmd.op, &cmd.key, cmd.value.as_deref());
    assert!(verify_command(&pk, &recomputed, &cmd.sig_hex));
}

#[test]
fn setl_round_trip_multibyte_utf8() {
    // Length is in bytes, not characters; multibyte sequences must work
    // and not get split mid-codepoint.
    let (sk, pk) = keypair();
    let addr = "zkv1test1";
    let value = "héllo 🦀 wörld"; // 18 bytes, 13 chars
    let payload = signed_payload(addr, Op::SetL, "k", Some(value));
    let sig = sign_command(&sk, &payload);
    let memo = build_memo(Op::SetL, "k", Some(value), 0, &sig).unwrap();
    let text = match Memo::try_from(memo).unwrap() {
        Memo::Text(t) => t.to_string(),
        _ => panic!("expected text memo"),
    };
    let cmd = parse_text_memo(&text).expect("SETL multibyte UTF-8 must parse");
    assert_eq!(cmd.value.as_deref(), Some(value));
    let recomputed = signed_payload(addr, cmd.op, &cmd.key, cmd.value.as_deref());
    assert!(verify_command(&pk, &recomputed, &cmd.sig_hex));
}

#[test]
fn setl_signed_payload_distinct_from_set() {
    // SET and SETL produce different signed payloads ("SET" vs "SETL"
    // as the op token), so a SET signature cannot be repackaged as a
    // SETL memo with the same key/value (or vice versa). The writer
    // authorizes a specific wire encoding.
    let (sk, pk) = keypair();
    let addr = "zkv1test1";
    let set_payload = signed_payload(addr, Op::Set, "k", Some("v"));
    let setl_payload = signed_payload(addr, Op::SetL, "k", Some("v"));
    assert_ne!(set_payload, setl_payload);
    let set_sig = sign_command(&sk, &set_payload);
    assert!(!verify_command(&pk, &setl_payload, &hex::encode(set_sig),));
}

#[test]
fn setl_parse_rejects_truncated_value() {
    // Declared byte_len overruns the actual rest. Must drop.
    let (sk, _) = keypair();
    let addr = "zkv1test1";
    let payload = signed_payload(addr, Op::SetL, "k", Some("short"));
    let sig = sign_command(&sk, &payload);
    // Claim 999 bytes when only 5 are present.
    let text = format!("ZKV0 SETL k 999\nshort\n{}", hex::encode(sig));
    assert!(parse_text_memo(&text).is_none());
}

#[test]
fn setl_parse_rejects_bad_length_token() {
    let (sk, _) = keypair();
    let addr = "zkv1test1";
    let sig = sign_command(&sk, &signed_payload(addr, Op::SetL, "k", Some("v")));
    // Non-numeric length token.
    let text = format!("ZKV0 SETL k abc\nv\n{}", hex::encode(sig));
    assert!(parse_text_memo(&text).is_none());
    // Negative length (also non-parseable as usize).
    let text = format!("ZKV0 SETL k -1\nv\n{}", hex::encode(sig));
    assert!(parse_text_memo(&text).is_none());
}

#[test]
fn setl_parse_rejects_missing_separator() {
    // No newline between value and sig. Must drop.
    let (sk, _) = keypair();
    let addr = "zkv1test1";
    let sig = sign_command(&sk, &signed_payload(addr, Op::SetL, "k", Some("v")));
    // Slap the sig directly after the value with no `\n`.
    let text = format!("ZKV0 SETL k 1\nv{}", hex::encode(sig));
    assert!(parse_text_memo(&text).is_none());
}

#[test]
fn setl_parse_rejects_extra_tokens_on_header() {
    // Anything past the length token on line 1 is a protocol violation.
    let (sk, _) = keypair();
    let addr = "zkv1test1";
    let sig = sign_command(&sk, &signed_payload(addr, Op::SetL, "k", Some("v")));
    let text = format!("ZKV0 SETL k 1 garbage\nv\n{}", hex::encode(sig));
    assert!(parse_text_memo(&text).is_none());
}

#[test]
fn setl_collapsed_memo_fallback_rejected() {
    // A SETL memo with no \n at all can't be length-decoded; the
    // collapsed-newline fallback is unsafe for length-framed wire,
    // so the parser drops it entirely.
    let (sk, _) = keypair();
    let addr = "zkv1test1";
    let sig = sign_command(&sk, &signed_payload(addr, Op::SetL, "k", Some("v")));
    let text = format!("ZKV0 SETL k 1 v {}", hex::encode(sig));
    assert!(parse_text_memo(&text).is_none());
}

#[test]
fn build_memo_setl_requires_value() {
    let sig = [0u8; 65];
    let err = build_memo(Op::SetL, "k", None, 0, &sig).expect_err("SETL with no value must bail");
    assert!(
        err.to_string().contains("SETL requires a value"),
        "unexpected error: {err}",
    );
}

#[test]
fn build_memo_setl_accepts_empty_and_newline_values() {
    // Both cases that `Op::Set` rejects must succeed under `Op::SetL`.
    let sig = [0u8; 65];
    build_memo(Op::SetL, "k", Some(""), 0, &sig).expect("SETL must accept empty");
    build_memo(Op::SetL, "k", Some("a\nb"), 0, &sig).expect("SETL must accept newlines");
}

#[test]
fn set_for_value_picks_compact_when_possible() {
    assert_eq!(Op::set_for_value("hello"), Op::Set);
    assert_eq!(Op::set_for_value("1234567890"), Op::Set);
    assert_eq!(Op::set_for_value("with spaces ok"), Op::Set);
}

#[test]
fn set_for_value_picks_setl_for_unsafe_values() {
    assert_eq!(Op::set_for_value(""), Op::SetL);
    assert_eq!(Op::set_for_value("a\nb"), Op::SetL);
    assert_eq!(Op::set_for_value("\n"), Op::SetL);
}

#[test]
fn replay_handles_mixed_set_and_setl() {
    // SET and SETL interleave: each overwrite (whichever wire form)
    // wins in chain order. The semantic is identical.
    let (sk, pk) = keypair();
    let addr = "zkv1test1";

    let result = replay(
        vec![
            (init_memo(&sk, addr), WriteStatus::Confirmed),
            (
                op_memo(&sk, addr, Op::Set, "k", Some("first")),
                WriteStatus::Confirmed,
            ),
            (
                // SET and SETL share the per-key version counter (v1).
                op_memo_v(&sk, addr, Op::SetL, "k", Some("second\nwith newline"), 1),
                WriteStatus::Confirmed,
            ),
            (
                op_memo_v(&sk, addr, Op::Set, "k", Some("third"), 2),
                WriteStatus::Confirmed,
            ),
        ],
        addr,
        &pk,
        false,
    )
    .unwrap();
    assert_eq!(result.init, InitState::Initialized);
    assert_eq!(
        result.state.get("k").and_then(|ks| ks.confirmed.as_deref()),
        Some("third"),
    );
}

#[test]
fn replay_setl_empty_value_round_trips() {
    let (sk, pk) = keypair();
    let addr = "zkv1test1";
    let result = replay(
        vec![
            (init_memo(&sk, addr), WriteStatus::Confirmed),
            (
                op_memo(&sk, addr, Op::SetL, "blank", Some("")),
                WriteStatus::Confirmed,
            ),
        ],
        addr,
        &pk,
        false,
    )
    .unwrap();
    assert_eq!(
        result
            .state
            .get("blank")
            .and_then(|ks| ks.confirmed.as_deref()),
        Some(""),
        "empty SETL value must land as Some(\"\"), not None",
    );
}

#[test]
fn replay_setl_pending_carries_newline_value() {
    // A confirming-but-not-yet-confirmed SETL must round-trip through
    // PendingOp::Set with the full multi-line value intact.
    let (sk, pk) = keypair();
    let addr = "zkv1test1";
    let result = replay(
        vec![
            (init_memo(&sk, addr), WriteStatus::Confirmed),
            (
                op_memo(&sk, addr, Op::SetL, "k", Some("a\nb\nc")),
                WriteStatus::Confirming {
                    done: 0,
                    required: 1,
                },
            ),
        ],
        addr,
        &pk,
        false,
    )
    .unwrap();
    let ks = result.state.get("k").expect("key must exist");
    assert!(ks.confirmed.is_none());
    match ks.pending.as_slice() {
        [PendingOp::Set { value, .. }] => {
            assert_eq!(value, "a\nb\nc");
        }
        _ => panic!("expected exactly one pending SET"),
    }
}

// ----- INIT-specific tests -----

#[test]
fn init_memo_round_trip() {
    let (sk, pk) = keypair();
    let addr = "zkv1test1";
    let payload = signed_init_payload(addr);
    let sig = sign_command(&sk, &payload);
    let memo = build_init_memo(addr, &sig).unwrap();
    let text = match Memo::try_from(memo).unwrap() {
        Memo::Text(t) => t.to_string(),
        _ => panic!("expected text memo"),
    };

    let cmd = parse_text_memo(&text).expect("parses");
    assert_eq!(cmd.op, Op::Init);
    assert_eq!(
        cmd.key, addr,
        "INIT memo still echoes the zkv_addr in its key field (advisory)"
    );
    assert!(
        cmd.value.is_none(),
        "v1 INIT carries no reserved tokens; value should be None",
    );

    // The signature binds only the receiver domain (`addr` here), not the
    // embedded echo; recompute via `signed_init_payload`, not the generic
    // `signed_payload` over the parsed key.
    let recomputed = signed_init_payload(addr);
    assert!(verify_command(&pk, &recomputed, &cmd.sig_hex));
}

#[test]
fn init_memo_size_under_text_memo_limit() {
    // Worst-case-ish: a realistic UFVK-derived zkv address. Compose by
    // taking a typical mainnet UFVK length (transparent + orchard, no
    // sapling). We don't have a real one available cheaply here, so use
    // a placeholder of the maximum length we expect. If this assertion
    // fails for a real wallet, the INIT wire format itself needs to
    // shrink (e.g., drop the embedded address).
    let long_ufvk: String = "a".repeat(320);
    let addr = format!("zkv1{long_ufvk}3500000");
    let (sk, _) = keypair();
    let payload = signed_init_payload(&addr);
    let sig = sign_command(&sk, &payload);
    let memo = build_init_memo(&addr, &sig).expect("must fit in a text memo");
    let m = Memo::try_from(memo).unwrap();
    let text = match m {
        Memo::Text(t) => t.to_string(),
        _ => panic!("expected text memo"),
    };
    // The memo bytes themselves are 512 bytes (1 type byte + 511 content);
    // the text content must be <= 511 bytes.
    assert!(
        text.len() <= 511,
        "INIT memo text is {} bytes; must be <= 511",
        text.len(),
    );
}

#[test]
fn replay_uninitialized_without_init() {
    // No INIT memo present; even a valid signed SET must not appear in
    // state. The database is not operable until INIT confirms.
    let (sk, pk) = keypair();
    let addr = "zkv1test1";

    let result = replay(
        vec![
            (
                op_memo(&sk, addr, Op::Set, "a", Some("1")),
                WriteStatus::Confirmed,
            ),
            (
                op_memo(&sk, addr, Op::Set, "b", Some("2")),
                WriteStatus::Confirmed,
            ),
        ],
        addr,
        &pk,
        false,
    )
    .unwrap();
    assert_eq!(result.init, InitState::Uninitialized);
    assert!(
        result.state.is_empty(),
        "valid signed SETs without an INIT must not enter state",
    );
}

#[test]
fn replay_initializing_with_unconfirmed_init() {
    // INIT is in mempool / below confirmation depth. State remains empty
    // because SET/DEL only apply after Initialized.
    let (sk, pk) = keypair();
    let addr = "zkv1test1";

    let result = replay(
        vec![(
            init_memo(&sk, addr),
            WriteStatus::Confirming {
                done: 1,
                required: 3,
            },
        )],
        addr,
        &pk,
        true,
    )
    .unwrap();
    assert_eq!(
        result.init,
        InitState::Initializing {
            done: 1,
            required: 3
        },
    );
    assert!(result.state.is_empty());
}

#[test]
fn replay_drops_set_before_init() {
    // Security: a fully-signed-by-the-admin SET that precedes the INIT in
    // chain order must NOT appear in state. The rule fires on chain
    // order, not on signature validity; the SET signature is valid here.
    let (sk, pk) = keypair();
    let addr = "zkv1test1";

    let result = replay(
        vec![
            (
                op_memo(&sk, addr, Op::Set, "a", Some("pre_init")),
                WriteStatus::Confirmed,
            ),
            (init_memo(&sk, addr), WriteStatus::Confirmed),
            (
                op_memo(&sk, addr, Op::Set, "b", Some("post_init")),
                WriteStatus::Confirmed,
            ),
        ],
        addr,
        &pk,
        true,
    )
    .unwrap();
    assert_eq!(result.init, InitState::Initialized);
    assert!(
        !result.state.contains_key("a"),
        "pre-INIT SET must not appear in state even though its signature is valid",
    );
    assert_eq!(
        result.state.get("b").and_then(|ks| ks.confirmed.as_deref()),
        Some("post_init"),
        "post-INIT SET must apply normally",
    );
}

#[test]
fn replay_drops_del_before_init() {
    // Same as above but for DEL. A pre-INIT DEL must have no effect.
    let (sk, pk) = keypair();
    let addr = "zkv1test1";

    let result = replay(
        vec![
            (
                op_memo(&sk, addr, Op::Del, "phantom", None),
                WriteStatus::Confirmed,
            ),
            (init_memo(&sk, addr), WriteStatus::Confirmed),
            (
                op_memo(&sk, addr, Op::Set, "real", Some("v")),
                WriteStatus::Confirmed,
            ),
        ],
        addr,
        &pk,
        true,
    )
    .unwrap();
    assert_eq!(result.init, InitState::Initialized);
    assert!(!result.state.contains_key("phantom"));
    assert_eq!(
        result
            .state
            .get("real")
            .and_then(|ks| ks.confirmed.as_deref()),
        Some("v"),
    );
}

#[test]
fn replay_ignores_subsequent_init() {
    // First valid INIT wins. A second INIT at a later block must not
    // re-initialize or disturb state.
    let (sk, pk) = keypair();
    let addr = "zkv1test1";

    let result = replay(
        vec![
            (init_memo(&sk, addr), WriteStatus::Confirmed),
            (
                op_memo(&sk, addr, Op::Set, "a", Some("1")),
                WriteStatus::Confirmed,
            ),
            // Second INIT; must be silently ignored.
            (init_memo(&sk, addr), WriteStatus::Confirmed),
            (
                // Second confirmed write to "a" (version 1).
                op_memo_v(&sk, addr, Op::Set, "a", Some("2"), 1),
                WriteStatus::Confirmed,
            ),
        ],
        addr,
        &pk,
        true,
    )
    .unwrap();
    assert_eq!(result.init, InitState::Initialized);
    assert_eq!(
        result.state.get("a").and_then(|ks| ks.confirmed.as_deref()),
        Some("2"),
        "writes after both INITs apply; second INIT is a no-op",
    );
}

#[test]
fn replay_rejects_cross_database_init() {
    // Cross-database INIT replay: an INIT signed for database A appears
    // in database B's memo stream. The embedded zkv_addr (A) does not
    // match B's address, AND the signature was computed against A's
    // signed payload; both checks must reject it.
    let (sk, pk) = keypair();
    let addr_a = "zkv1test1";
    let addr_b = "zkv1test2";

    let init_for_a = init_memo(&sk, addr_a);

    // Replay this memo as if it were in database B's stream.
    let result = replay(
        vec![(init_for_a, WriteStatus::Confirmed)],
        addr_b,
        &pk,
        false,
    )
    .unwrap();
    assert_eq!(
        result.init,
        InitState::Uninitialized,
        "INIT for a different DB must not initialize this DB",
    );
}

#[test]
fn replay_rejects_forged_init_signature() {
    // An attacker constructs an INIT memo with the victim's zkv_addr in
    // the body, but signs it with their own (attacker's) key. The
    // signature must not verify under the victim's pubkey; the INIT must
    // be rejected and the DB stays Uninitialized.
    let (sk_attacker, _) = keypair_from(0x11);
    let (_, pk_victim) = keypair_from(0x42);
    let addr = "zkv1test1";

    let forged_init = init_memo(&sk_attacker, addr);

    let result = replay(
        vec![(forged_init, WriteStatus::Confirmed)],
        addr,
        &pk_victim,
        false,
    )
    .unwrap();
    assert_eq!(result.init, InitState::Uninitialized);
}

#[test]
fn replay_init_advisory_tokens_do_not_affect_signature() {
    // Build a valid INIT for `addr`, then append an extra token before the
    // signature line. Under receiver-binding the INIT signature commits to
    // the receiver alone; the embedded address and any trailing tokens are
    // advisory and unsigned, so this does NOT invalidate the INIT. (It
    // could only let an attacker scribble cosmetic tokens onto an INIT they
    // can't otherwise forge: it stays root-signed and receiver-bound.)
    let (sk, pk) = keypair();
    let addr = "zkv1test1";

    let valid = init_memo(&sk, addr);
    // valid is "ZKV0 INIT zkv1test1\n<sig>". Splice "extra" before the
    // newline so the parser sees value=Some("extra").
    let (line1, sig_part) = valid.split_once('\n').unwrap();
    let appended = format!("{line1} extra\n{sig_part}");

    let result = replay(vec![(appended, WriteStatus::Confirmed)], addr, &pk, false).unwrap();
    assert_eq!(
        result.init,
        InitState::Initialized,
        "advisory trailing tokens are unsigned, so the INIT still initializes",
    );
}

#[test]
fn replay_init_embedded_address_is_advisory() {
    // Under receiver-binding the embedded wire address is an advisory echo,
    // not authorization input: an INIT correctly signed over this database's
    // *receiver* (`addr` here) but echoing a different address in plaintext
    // is still honored. This is the deliberate relaxation that makes the
    // birthday/UFVK-encoding non-load-bearing: a re-export with a corrected
    // birthday changes the echo but not the receiver, so the INIT survives.
    // Identity is still fully pinned: the signature must recover to the root
    // key over *our* receiver, which no other party can produce.
    let (sk, pk) = keypair();
    let addr = "zkv1test1";
    let other = "zkv1testother";

    // Sign the receiver-only INIT payload for `addr`, but echo `other`.
    let payload = signed_init_payload(addr);
    let sig = sign_command(&sk, &payload);
    let memo = build_memo(Op::Init, other, None, 0, &sig).unwrap();
    let text = match Memo::try_from(memo).unwrap() {
        Memo::Text(t) => t.to_string(),
        _ => unreachable!(),
    };

    let result = replay(vec![(text, WriteStatus::Confirmed)], addr, &pk, false).unwrap();
    assert_eq!(
        result.init,
        InitState::Initialized,
        "a root-signed, receiver-bound INIT initializes regardless of its advisory echo",
    );
}

#[test]
fn replay_with_seed_matches_full_replay_at_every_split() {
    // The seed-aware replay is supposed to be exactly equivalent to a
    // single-pass replay over the same chain-ordered entries: the
    // snapshot module relies on this to update an on-disk projection
    // incrementally. Verify by splitting the same sequence at every
    // boundary and asserting both confirmed state and init status match
    // the full-replay result.
    //
    // Cases covered by `entries`:
    //   * INIT in the middle (split that crosses the INIT gate must
    //     produce the same Uninitialized-then-Initialized transition).
    //   * Pre-INIT SET that must stay dropped regardless of split.
    //   * SET overwrite across the split.
    //   * SET then DEL across the split (key vanishes from final state).
    //   * Confirmed SET of a key that is later DEL'd in the tail.
    //   * Mempool / Confirming ops on top of a seeded confirmed state.
    //   * DEL of a key that doesn't exist in the seed (no-op).
    //   * Confirmed SETL with an empty value (seeded as `Some("")`,
    //     not `None`; the distinction must survive the boundary).
    //   * Confirmed SETL with newlines in the value.
    //   * SETL overwriting a SET-set key: the wire form changes, the
    //     kv result should not.
    //   * Confirmed SETL on a key, then DEL of the same key.
    //   * Confirming (pending) SET on top of a seeded confirmed SETL
    //     and pending SETL on top of a seeded confirmed SETL; both
    //     must end up in the pending queue regardless of split.
    let (sk, pk) = keypair();
    let addr = "zkv1test1";
    let entries: Vec<(String, WriteStatus)> = vec![
        (
            op_memo(&sk, addr, Op::Set, "pre", Some("ignored")),
            WriteStatus::Confirmed,
        ),
        (init_memo(&sk, addr), WriteStatus::Confirmed),
        (
            op_memo(&sk, addr, Op::Set, "a", Some("1")),
            WriteStatus::Confirmed,
        ),
        (
            op_memo(&sk, addr, Op::Set, "b", Some("stable")),
            WriteStatus::Confirmed,
        ),
        (
            op_memo(&sk, addr, Op::Set, "a", Some("2")),
            WriteStatus::Confirmed,
        ),
        (
            op_memo(&sk, addr, Op::Set, "c", Some("doomed")),
            WriteStatus::Confirmed,
        ),
        (
            op_memo(&sk, addr, Op::Del, "c", None),
            WriteStatus::Confirmed,
        ),
        (
            op_memo(&sk, addr, Op::Del, "never_existed", None),
            WriteStatus::Confirmed,
        ),
        // ---- SETL coverage in the seed/tail boundary ----
        // Empty value via SETL: seed must carry `Some("")`, not `None`.
        (
            op_memo(&sk, addr, Op::SetL, "blank", Some("")),
            WriteStatus::Confirmed,
        ),
        // Newline-carrying value via SETL.
        (
            op_memo(&sk, addr, Op::SetL, "multi", Some("line1\nline2")),
            WriteStatus::Confirmed,
        ),
        // SETL overwrites a SET-set key ("a" was 2): the wire form
        // changes but the kv projection records the latest value.
        (
            op_memo(&sk, addr, Op::SetL, "a", Some("setl-wins\n")),
            WriteStatus::Confirmed,
        ),
        // SETL-set key then DEL'd: the key must vanish from final state
        // regardless of which side of the split owns each row.
        (
            op_memo(&sk, addr, Op::SetL, "d", Some("ephemeral")),
            WriteStatus::Confirmed,
        ),
        (
            op_memo(&sk, addr, Op::Del, "d", None),
            WriteStatus::Confirmed,
        ),
        // Confirming SET on top of a seeded confirmed SETL ("a"): the
        // pending op is a SET but the confirmed value came in via SETL.
        // Last-write-wins doesn't see the wire form, only the kv result.
        (
            op_memo(&sk, addr, Op::Set, "a", Some("3")),
            WriteStatus::Confirming {
                done: 0,
                required: 3,
            },
        ),
        // Confirming SETL on top of a seeded confirmed SETL ("blank"):
        // pending queue must materialize identically across every split.
        (
            op_memo(&sk, addr, Op::SetL, "blank", Some("filled\nin")),
            WriteStatus::Confirming {
                done: 0,
                required: 3,
            },
        ),
    ];

    let full = replay(entries.clone(), addr, &pk, false).unwrap();

    // A snapshot only contains rows the read path has classified as
    // Confirmed; Confirming rows live in the tail. So splits are only
    // meaningful up to the first Confirming entry. Past that, "the head
    // was persisted to a snapshot" is not a state the system ever
    // reaches and the equivalence does not need to hold.
    let max_split = entries
        .iter()
        .position(|(_, s)| matches!(s, WriteStatus::Confirming { .. }))
        .unwrap_or(entries.len());
    assert!(max_split > 0, "test must exercise at least one split");

    for split in 0..=max_split {
        let (head, tail) = entries.split_at(split);
        let head_result = replay(head.to_vec(), addr, &pk, false).unwrap();
        let combined =
            replay_with_seed(tail.to_vec(), Some(head_result), addr, &pk, false).unwrap();
        assert_eq!(combined.init, full.init, "init mismatch at split={split}",);
        assert_eq!(
            combined.state, full.state,
            "state mismatch at split={split}",
        );
    }
}

#[test]
fn replay_with_seed_drops_pending_from_seed() {
    // The snapshot only persists confirmed projection; a seed that
    // somehow arrived with stale pending entries (e.g., from an old
    // query's ReplayResult) must have them stripped before folding,
    // because the live tail will recompute pending from the current
    // unconfirmed set.
    let (sk, pk) = keypair();
    let addr = "zkv1test1";

    // Build a seed via a full replay that intentionally carries pending.
    let seed = replay(
        vec![
            (init_memo(&sk, addr), WriteStatus::Confirmed),
            (
                op_memo(&sk, addr, Op::Set, "a", Some("v")),
                WriteStatus::Confirmed,
            ),
            (
                op_memo_v(&sk, addr, Op::Set, "a", Some("new"), 1),
                WriteStatus::Confirming {
                    done: 0,
                    required: 3,
                },
            ),
        ],
        addr,
        &pk,
        true,
    )
    .unwrap();
    assert!(
        !seed.state.get("a").unwrap().pending.is_empty(),
        "control: seed must arrive with pending so the test is meaningful",
    );

    // Folding an empty tail must produce a state with no pending.
    let result = replay_with_seed(
        Vec::<(String, WriteStatus)>::new(),
        Some(seed),
        addr,
        &pk,
        true,
    )
    .unwrap();
    assert_eq!(result.init, InitState::Initialized);
    assert_eq!(
        result.state.get("a").and_then(|k| k.confirmed.as_deref()),
        Some("v")
    );
    assert!(
        result.state.get("a").unwrap().pending.is_empty(),
        "stale pending from seed must be discarded before folding",
    );
}

#[test]
fn replay_init_forward_compat_extra_tokens() {
    // Forward-compat: a future INIT memo that carries extra reserved tokens
    // in its wire form must still verify and initialize the DB cleanly.
    // Under receiver-binding the INIT signature commits to the receiver
    // alone; the embedded address and any reserved tokens are advisory and
    // unsigned, so trailing tokens are ignored rather than breaking the sig.
    let (sk, pk) = keypair();
    let addr = "zkv1test1";

    // Sign the receiver-only INIT payload, then attach reserved tokens to the
    // wire memo (the signature does not cover them).
    let reserved = "future-config-v2 schema=string";
    let payload = signed_init_payload(addr);
    let sig = sign_command(&sk, &payload);
    let memo = build_memo(Op::Init, addr, Some(reserved), 0, &sig).unwrap();
    let text = match Memo::try_from(memo).unwrap() {
        Memo::Text(t) => t.to_string(),
        _ => unreachable!(),
    };

    let result = replay(
        vec![
            (text, WriteStatus::Confirmed),
            (
                op_memo(&sk, addr, Op::Set, "k", Some("v")),
                WriteStatus::Confirmed,
            ),
        ],
        addr,
        &pk,
        true,
    )
    .unwrap();
    assert_eq!(result.init, InitState::Initialized);
    assert_eq!(
        result.state.get("k").and_then(|ks| ks.confirmed.as_deref()),
        Some("v"),
    );
}

// ----- history helpers -----

#[test]
fn render_memo_text_round_trips_through_parser() {
    let (sk, _) = keypair();
    let addr = "zkv1test1";
    // SET with a value.
    let payload = signed_payload(addr, Op::Set, "a", Some("hello world"));
    let sig_hex = hex::encode(sign_command(&sk, &payload));
    let text = render_memo_text(Op::Set, "a", Some("hello world"), 0, &sig_hex);
    let cmd = parse_text_memo(&text).expect("renders parseable memo");
    assert_eq!(cmd.op, Op::Set);
    assert_eq!(cmd.key, "a");
    assert_eq!(cmd.value.as_deref(), Some("hello world"));
    assert_eq!(cmd.sig_hex, sig_hex);

    // DEL has no value.
    let del_payload = signed_payload(addr, Op::Del, "a", None);
    let del_sig = hex::encode(sign_command(&sk, &del_payload));
    let del_text = render_memo_text(Op::Del, "a", None, 0, &del_sig);
    let del_cmd = parse_text_memo(&del_text).expect("renders parseable DEL memo");
    assert_eq!(del_cmd.op, Op::Del);
    assert_eq!(del_cmd.value, None);
}

#[test]
fn render_memo_text_matches_build_memo() {
    // render_memo_text must produce byte-identical text to build_memo
    // for the same inputs (they share `memo_line1`).
    let (sk, _) = keypair();
    let addr = "zkv1test1";
    let payload = signed_payload(addr, Op::Set, "k", Some("v"));
    let sig = sign_command(&sk, &payload);
    let built = match Memo::try_from(build_memo(Op::Set, "k", Some("v"), 0, &sig).unwrap()).unwrap()
    {
        Memo::Text(t) => t.to_string(),
        _ => unreachable!(),
    };
    let rendered = render_memo_text(Op::Set, "k", Some("v"), 0, &hex::encode(sig));
    assert_eq!(built, rendered);
}

#[test]
fn history_entry_from_memo_builds_verified_set() {
    let (sk, pk) = keypair();
    let addr = "zkv1test1";
    let text = op_memo(&sk, addr, Op::Set, "a", Some("1"));
    let entry = history_entry_from_memo(
        addr,
        &pk,
        &text,
        Some(500),
        Some(1_700_000_000),
        "txhex".to_owned(),
        2,
        HistoryStatus::Confirmed { confirmations: 7 },
    )
    .expect("SET produces an entry");
    assert_eq!(entry.op, Op::Set);
    assert_eq!(entry.key, "a");
    assert_eq!(entry.value.as_deref(), Some("1"));
    assert_eq!(entry.height, Some(500));
    assert_eq!(entry.timestamp, Some(1_700_000_000));
    assert_eq!(entry.txid, "txhex");
    assert_eq!(entry.output_index, 2);
    assert_eq!(entry.verified, Some(true));
    assert_eq!(entry.status, HistoryStatus::Confirmed { confirmations: 7 });
    assert!(entry.signature.is_some());
    assert_eq!(entry.memo.as_deref(), Some(text.as_str()));
}

#[test]
fn history_entry_from_memo_builds_verified_del() {
    let (sk, pk) = keypair();
    let addr = "zkv1test1";
    let text = op_memo(&sk, addr, Op::Del, "gone", None);
    let entry = history_entry_from_memo(
        addr,
        &pk,
        &text,
        None,
        None,
        String::new(),
        0,
        HistoryStatus::Pending,
    )
    .expect("DEL produces an entry");
    assert_eq!(entry.op, Op::Del);
    assert_eq!(entry.value, None);
    assert_eq!(entry.timestamp, None);
    assert_eq!(entry.verified, Some(true));
    assert_eq!(entry.status, HistoryStatus::Pending);
}

#[test]
fn history_entry_from_memo_flags_bad_signature() {
    // A SET signed by the wrong key still parses, but `verified` must be
    // Some(false) so the UI can flag tampering; the entry is not dropped
    // (the live tail surfaces it; only the reducer drops it from state).
    let (sk_attacker, _) = keypair_from(0x11);
    let (_, pk_victim) = keypair_from(0x42);
    let addr = "zkv1test1";
    let text = op_memo(&sk_attacker, addr, Op::Set, "a", Some("forged"));
    let entry = history_entry_from_memo(
        addr,
        &pk_victim,
        &text,
        Some(10),
        Some(1_700_000_000),
        "tx".to_owned(),
        0,
        HistoryStatus::Confirmed { confirmations: 1 },
    )
    .expect("entry is still produced");
    assert_eq!(entry.verified, Some(false));
}

#[test]
fn history_entry_from_memo_includes_init_excludes_non_zkv() {
    let (sk, pk) = keypair();
    let addr = "zkv1test1";

    // INIT now appears in history, keyed by the zkv address.
    let init = init_memo(&sk, addr);
    let entry = history_entry_from_memo(
        addr,
        &pk,
        &init,
        Some(1),
        None,
        "t".into(),
        0,
        HistoryStatus::Confirmed { confirmations: 5 },
    )
    .expect("INIT produces an entry");
    assert_eq!(entry.op, Op::Init);
    assert_eq!(entry.key, addr);
    assert_eq!(entry.verified, Some(true));

    // A plain (non-zkv) text memo is still excluded.
    assert!(
        history_entry_from_memo(
            addr,
            &pk,
            "just a note",
            Some(1),
            None,
            "t".into(),
            0,
            HistoryStatus::Pending,
        )
        .is_none(),
        "non-zkv text memo must not appear in history",
    );
}

// ===== Owner / Writer authorization =====
//
// In these tests the *root* keypair is whatever signs INIT; it becomes
// owner #1. Other keypairs (`keypair_from(seed)`) stand in for delegated
// owners, scoped writers, and outright attackers. `op_memo` signs a memo
// with whichever secret key it's handed, so "who signed this" is the
// whole point of each scenario.

/// Hex of a pubkey, for building OWNER*/WRITER* memos whose `key` field is
/// the target pubkey.
fn pk_hex(pk: &secp256k1::PublicKey) -> String {
    pubkey_bech32(pk)
}

#[test]
fn root_is_sole_owner_after_init() {
    let (sk, pk) = keypair();
    let addr = "zkv1test1";
    let result = replay(
        vec![(init_memo(&sk, addr), WriteStatus::Confirmed)],
        addr,
        &pk,
        true,
    )
    .unwrap();
    assert!(result.auth.is_owner(&pk_hex(&pk)), "root must be owner #1");
    assert_eq!(result.auth.owners().count(), 1);
    assert_eq!(result.auth.writers().count(), 0);
}

#[test]
fn auth_registry_empty_until_init_confirms() {
    // An INIT in mempool (Confirming) does not yet seat the root owner;
    // a pending grant of authority confers nothing.
    let (sk, pk) = keypair();
    let addr = "zkv1test1";
    let result = replay(
        vec![(
            init_memo(&sk, addr),
            WriteStatus::Confirming {
                done: 0,
                required: 1,
            },
        )],
        addr,
        &pk,
        true,
    )
    .unwrap();
    assert!(result.auth.is_empty(), "no owners until INIT confirms");
}

// ===== FINALIZE (one-way seal) =====

#[test]
fn finalize_memo_round_trips_two_section_and_collapsed() {
    let (sk, _pk) = keypair();
    let addr = "zkv1test1";
    let text = op_memo(&sk, addr, Op::Finalize, "", None);

    // Two-section form: "ZKV0 FINALIZE\n<sig>".
    let cmd = parse_text_memo(&text).expect("FINALIZE parses");
    assert_eq!(cmd.op, Op::Finalize);
    assert_eq!(cmd.key, "");
    assert_eq!(cmd.value, None);
    assert_eq!(cmd.sig_hex.len(), SIG_HEX_LEN);

    // Newline-collapsed form (some broadcaster wallets do this).
    let collapsed = text.replace('\n', " ");
    let cmd2 = parse_text_memo(&collapsed).expect("collapsed FINALIZE parses");
    assert_eq!(cmd2.op, Op::Finalize);
    assert_eq!(cmd2.key, "");
    assert_eq!(cmd2, cmd);
}

#[test]
fn finalize_rejects_a_trailing_token() {
    // FINALIZE is header-only; a stray token is a wire violation, not a key.
    let sig = "00".repeat(SIG_LEN);
    let text = format!("ZKV0 FINALIZE extra\n{sig}");
    assert_eq!(
        parse_text_memo_detailed(&text),
        Err(MemoReject::Malformed(MemoFormat::WrongArity {
            op: Op::Finalize
        })),
    );
}

#[test]
fn confirmed_finalize_seals_database_against_all_writes() {
    let (root_sk, root_pk) = keypair();
    let (owner2_sk, owner2_pk) = keypair_from(0x55);
    let addr = "zkv1test1";

    let result = replay(
        vec![
            (init_memo(&root_sk, addr), WriteStatus::Confirmed),
            (
                op_memo(&root_sk, addr, Op::Set, "k", Some("v")),
                WriteStatus::Confirmed,
            ),
            // The owner seals the database.
            (
                op_memo(&root_sk, addr, Op::Finalize, "", None),
                WriteStatus::Confirmed,
            ),
            // Everything after a confirmed FINALIZE is dropped, by anyone.
            (
                op_memo(&root_sk, addr, Op::Set, "k", Some("after")),
                WriteStatus::Confirmed,
            ),
            (
                op_memo(&root_sk, addr, Op::Del, "k", None),
                WriteStatus::Confirmed,
            ),
            (
                op_memo(&root_sk, addr, Op::OwnerAdd, &pk_hex(&owner2_pk), None),
                WriteStatus::Confirmed,
            ),
            // A second FINALIZE is also dropped; the latch is one-way.
            (
                op_memo(&owner2_sk, addr, Op::Finalize, "", None),
                WriteStatus::Confirmed,
            ),
        ],
        addr,
        &root_pk,
        false,
    )
    .unwrap();

    assert!(result.finalized, "database must be sealed");
    // The pre-FINALIZE value survives; the post-FINALIZE SET/DEL never applied.
    assert_eq!(
        result.state.get("k").unwrap().confirmed.as_deref(),
        Some("v")
    );
    // The post-FINALIZE OWNERADD never took effect.
    assert!(!result.auth.is_owner(&pk_hex(&owner2_pk)));
    assert_eq!(result.auth.owners().count(), 1);
}

#[test]
fn pending_finalize_does_not_seal_yet() {
    let (root_sk, root_pk) = keypair();
    let addr = "zkv1test1";

    let result = replay(
        vec![
            (init_memo(&root_sk, addr), WriteStatus::Confirmed),
            // FINALIZE still in flight; confers no seal yet.
            (
                op_memo(&root_sk, addr, Op::Finalize, "", None),
                WriteStatus::Confirming {
                    done: 0,
                    required: 1,
                },
            ),
            // A confirmed write after a *pending* FINALIZE still applies.
            (
                op_memo(&root_sk, addr, Op::Set, "k", Some("v")),
                WriteStatus::Confirmed,
            ),
        ],
        addr,
        &root_pk,
        false,
    )
    .unwrap();

    assert!(!result.finalized, "a pending FINALIZE must not seal");
    assert_eq!(
        result.state.get("k").unwrap().confirmed.as_deref(),
        Some("v")
    );
}

#[test]
fn non_owner_finalize_is_dropped() {
    let (root_sk, root_pk) = keypair();
    let (attacker_sk, _attacker_pk) = keypair_from(0x99);
    let addr = "zkv1test1";

    let result = replay(
        vec![
            (init_memo(&root_sk, addr), WriteStatus::Confirmed),
            // A non-owner cannot seal the database.
            (
                op_memo(&attacker_sk, addr, Op::Finalize, "", None),
                WriteStatus::Confirmed,
            ),
            // So a subsequent owner write still applies.
            (
                op_memo(&root_sk, addr, Op::Set, "k", Some("v")),
                WriteStatus::Confirmed,
            ),
        ],
        addr,
        &root_pk,
        false,
    )
    .unwrap();

    assert!(!result.finalized, "a non-owner FINALIZE must be dropped");
    assert_eq!(
        result.state.get("k").unwrap().confirmed.as_deref(),
        Some("v")
    );
}

#[test]
fn finalize_before_init_is_dropped() {
    let (root_sk, root_pk) = keypair();
    let addr = "zkv1test1";

    // FINALIZE before any INIT is pre-init noise; dropped.
    let result = replay(
        vec![(
            op_memo(&root_sk, addr, Op::Finalize, "", None),
            WriteStatus::Confirmed,
        )],
        addr,
        &root_pk,
        false,
    )
    .unwrap();
    assert!(!result.finalized);
    assert!(matches!(result.init, InitState::Uninitialized));
}

#[test]
fn owner_can_add_second_owner_who_can_then_write() {
    let (root_sk, root_pk) = keypair();
    let (owner2_sk, owner2_pk) = keypair_from(0x55);
    let addr = "zkv1test1";

    let result = replay(
        vec![
            (init_memo(&root_sk, addr), WriteStatus::Confirmed),
            // Root grants owner #2.
            (
                op_memo(&root_sk, addr, Op::OwnerAdd, &pk_hex(&owner2_pk), None),
                WriteStatus::Confirmed,
            ),
            // Owner #2 writes a key; allowed because owners write anything.
            (
                op_memo(&owner2_sk, addr, Op::Set, "k", Some("by-owner2")),
                WriteStatus::Confirmed,
            ),
        ],
        addr,
        &root_pk,
        true,
    )
    .unwrap();
    assert!(result.auth.is_owner(&pk_hex(&owner2_pk)));
    assert_eq!(
        result.state.get("k").and_then(|ks| ks.confirmed.as_deref()),
        Some("by-owner2"),
    );
}

#[test]
fn writer_create_update_destroy_within_scope() {
    let (root_sk, root_pk) = keypair();
    let (w_sk, w_pk) = keypair_from(0x33);
    let addr = "zkv1test1";

    let result = replay(
        vec![
            (init_memo(&root_sk, addr), WriteStatus::Confirmed),
            // Grant a full-scope writer.
            (
                op_memo(
                    &root_sk,
                    addr,
                    Op::WriterAdd,
                    &pk_hex(&w_pk),
                    Some("CREATE,UPDATE,DESTROY"),
                ),
                WriteStatus::Confirmed,
            ),
            // CREATE a new key (version 0).
            (
                op_memo(&w_sk, addr, Op::Set, "k", Some("v1")),
                WriteStatus::Confirmed,
            ),
            // UPDATE the existing key (version 1).
            (
                op_memo_v(&w_sk, addr, Op::Set, "k", Some("v2"), 1),
                WriteStatus::Confirmed,
            ),
            // CREATE a second key, then DESTROY it (versions 0, 1).
            (
                op_memo(&w_sk, addr, Op::Set, "doomed", Some("x")),
                WriteStatus::Confirmed,
            ),
            (
                op_memo_v(&w_sk, addr, Op::Del, "doomed", None, 1),
                WriteStatus::Confirmed,
            ),
        ],
        addr,
        &root_pk,
        true,
    )
    .unwrap();
    assert_eq!(
        result.state.get("k").and_then(|ks| ks.confirmed.as_deref()),
        Some("v2"),
    );
    assert!(!result.state.contains_key("doomed"), "DESTROY removed it");
    match result.auth.authority_of(&pk_hex(&w_pk)) {
        Some(Authority::Writer(scope)) => {
            assert!(scope.contains(Capability::Create));
            assert!(scope.contains(Capability::Update));
            assert!(scope.contains(Capability::Destroy));
        }
        other => panic!("expected scoped writer, got {other:?}"),
    }
}

#[test]
fn writer_create_only_cannot_update_existing_key() {
    // CREATE lets a writer seat a *new* key but not overwrite one that
    // already has a confirmed value. The owner seeds the key; the
    // create-only writer's overwrite must be dropped.
    let (root_sk, root_pk) = keypair();
    let (w_sk, w_pk) = keypair_from(0x33);
    let addr = "zkv1test1";

    let result = replay(
        vec![
            (init_memo(&root_sk, addr), WriteStatus::Confirmed),
            (
                op_memo(
                    &root_sk,
                    addr,
                    Op::WriterAdd,
                    &pk_hex(&w_pk),
                    Some("CREATE"),
                ),
                WriteStatus::Confirmed,
            ),
            // Owner seeds "k" (version 0).
            (
                op_memo(&root_sk, addr, Op::Set, "k", Some("owner-value")),
                WriteStatus::Confirmed,
            ),
            // Create-only writer tries to overwrite; must be dropped on
            // scope (not signature), so it signs the correct version 1.
            (
                op_memo_v(&w_sk, addr, Op::Set, "k", Some("hijacked"), 1),
                WriteStatus::Confirmed,
            ),
            // But it CAN create a brand-new key.
            (
                op_memo(&w_sk, addr, Op::Set, "fresh", Some("ok")),
                WriteStatus::Confirmed,
            ),
        ],
        addr,
        &root_pk,
        false,
    )
    .unwrap();
    assert_eq!(
        result.state.get("k").and_then(|ks| ks.confirmed.as_deref()),
        Some("owner-value"),
        "create-only writer must not overwrite an existing key",
    );
    assert_eq!(
        result
            .state
            .get("fresh")
            .and_then(|ks| ks.confirmed.as_deref()),
        Some("ok"),
    );
}

#[test]
fn writer_update_only_cannot_create_new_key() {
    // The inverse: UPDATE-only may overwrite an existing key but cannot
    // bring a new one into being.
    let (root_sk, root_pk) = keypair();
    let (w_sk, w_pk) = keypair_from(0x34);
    let addr = "zkv1test1";

    let result = replay(
        vec![
            (init_memo(&root_sk, addr), WriteStatus::Confirmed),
            (
                op_memo(
                    &root_sk,
                    addr,
                    Op::WriterAdd,
                    &pk_hex(&w_pk),
                    Some("UPDATE"),
                ),
                WriteStatus::Confirmed,
            ),
            (
                op_memo(&root_sk, addr, Op::Set, "k", Some("owner-value")),
                WriteStatus::Confirmed,
            ),
            // UPDATE an existing key: allowed (version 1, after the seed).
            (
                op_memo_v(&w_sk, addr, Op::Set, "k", Some("updated"), 1),
                WriteStatus::Confirmed,
            ),
            // CREATE a new key: must be dropped (version 0, a fresh key).
            (
                op_memo(&w_sk, addr, Op::Set, "new", Some("nope")),
                WriteStatus::Confirmed,
            ),
        ],
        addr,
        &root_pk,
        false,
    )
    .unwrap();
    assert_eq!(
        result.state.get("k").and_then(|ks| ks.confirmed.as_deref()),
        Some("updated"),
    );
    assert!(
        !result.state.contains_key("new"),
        "update-only writer must not create a new key",
    );
}

#[test]
fn writer_without_destroy_cannot_delete() {
    let (root_sk, root_pk) = keypair();
    let (w_sk, w_pk) = keypair_from(0x35);
    let addr = "zkv1test1";

    let result = replay(
        vec![
            (init_memo(&root_sk, addr), WriteStatus::Confirmed),
            (
                op_memo(
                    &root_sk,
                    addr,
                    Op::WriterAdd,
                    &pk_hex(&w_pk),
                    Some("CREATE,UPDATE"),
                ),
                WriteStatus::Confirmed,
            ),
            (
                op_memo(&w_sk, addr, Op::Set, "k", Some("v")),
                WriteStatus::Confirmed,
            ),
            // No DESTROY in scope; the DEL is dropped, key survives.
            (
                op_memo(&w_sk, addr, Op::Del, "k", None),
                WriteStatus::Confirmed,
            ),
        ],
        addr,
        &root_pk,
        false,
    )
    .unwrap();
    assert_eq!(
        result.state.get("k").and_then(|ks| ks.confirmed.as_deref()),
        Some("v"),
        "writer without DESTROY must not delete",
    );
}

// ----- Malicious / unauthorized attempts -----

#[test]
fn attacker_with_no_role_cannot_write() {
    // A stranger who never appears in the registry signs a SET. The
    // signature is perfectly valid ECDSA, but the recovered signer holds
    // no authority, so the write is dropped.
    let (root_sk, root_pk) = keypair();
    let (attacker_sk, _) = keypair_from(0x99);
    let addr = "zkv1test1";

    let result = replay(
        vec![
            (init_memo(&root_sk, addr), WriteStatus::Confirmed),
            (
                op_memo(&attacker_sk, addr, Op::Set, "k", Some("evil")),
                WriteStatus::Confirmed,
            ),
        ],
        addr,
        &root_pk,
        false,
    )
    .unwrap();
    assert!(
        result.state.is_empty(),
        "a signer with no registry entry must not be able to write",
    );
}

#[test]
fn writer_cannot_grant_owners_or_writers() {
    // Privilege escalation attempt: a fully-scoped writer tries to add
    // itself as an owner, and to grant a confederate writer access. Both
    // management memos are signed by a non-owner, so both are ignored.
    let (root_sk, root_pk) = keypair();
    let (w_sk, w_pk) = keypair_from(0x33);
    let (confederate_sk, confederate_pk) = keypair_from(0x44);
    let addr = "zkv1test1";

    let result = replay(
        vec![
            (init_memo(&root_sk, addr), WriteStatus::Confirmed),
            (
                op_memo(
                    &root_sk,
                    addr,
                    Op::WriterAdd,
                    &pk_hex(&w_pk),
                    Some("CREATE,UPDATE,DESTROY"),
                ),
                WriteStatus::Confirmed,
            ),
            // Writer tries to promote itself to owner.
            (
                op_memo(&w_sk, addr, Op::OwnerAdd, &pk_hex(&w_pk), None),
                WriteStatus::Confirmed,
            ),
            // Writer tries to grant a confederate.
            (
                op_memo(
                    &w_sk,
                    addr,
                    Op::WriterAdd,
                    &pk_hex(&confederate_pk),
                    Some("CREATE,UPDATE,DESTROY"),
                ),
                WriteStatus::Confirmed,
            ),
            // The confederate then tries to write; should fail, never granted.
            (
                op_memo(&confederate_sk, addr, Op::Set, "k", Some("evil")),
                WriteStatus::Confirmed,
            ),
        ],
        addr,
        &root_pk,
        false,
    )
    .unwrap();
    assert!(
        !result.auth.is_owner(&pk_hex(&w_pk)),
        "writer must not be able to promote itself to owner",
    );
    assert!(
        result.auth.authority_of(&pk_hex(&confederate_pk)).is_none(),
        "writer must not be able to grant another writer",
    );
    assert!(
        result.state.is_empty(),
        "the unauthorized confederate write must not land",
    );
}

#[test]
fn revoked_writer_loses_access() {
    // A writer is granted, writes successfully, is revoked, then tries to
    // write again. The post-revocation write must be dropped.
    let (root_sk, root_pk) = keypair();
    let (w_sk, w_pk) = keypair_from(0x33);
    let addr = "zkv1test1";

    let result = replay(
        vec![
            (init_memo(&root_sk, addr), WriteStatus::Confirmed),
            (
                op_memo(
                    &root_sk,
                    addr,
                    Op::WriterAdd,
                    &pk_hex(&w_pk),
                    Some("CREATE,UPDATE"),
                ),
                WriteStatus::Confirmed,
            ),
            (
                op_memo(&w_sk, addr, Op::Set, "k", Some("before-revoke")),
                WriteStatus::Confirmed,
            ),
            // Owner revokes the writer (target version 1, after the grant).
            (
                op_memo_v(&root_sk, addr, Op::WriterDel, &pk_hex(&w_pk), None, 1),
                WriteStatus::Confirmed,
            ),
            // Writer attempts another write post-revocation; dropped on
            // authority (signs the correct key version 1, not a stale sig).
            (
                op_memo_v(&w_sk, addr, Op::Set, "k", Some("after-revoke"), 1),
                WriteStatus::Confirmed,
            ),
        ],
        addr,
        &root_pk,
        false,
    )
    .unwrap();
    assert_eq!(
        result.state.get("k").and_then(|ks| ks.confirmed.as_deref()),
        Some("before-revoke"),
        "post-revocation write must be dropped",
    );
    assert!(result.auth.authority_of(&pk_hex(&w_pk)).is_none());
}

#[test]
fn revoked_owner_cannot_write_or_manage() {
    // Owner #2 is added, then removed by owner #1. After revocation its
    // writes and management ops are both dropped.
    let (root_sk, root_pk) = keypair();
    let (owner2_sk, owner2_pk) = keypair_from(0x55);
    let (writer_pk_seed_sk, writer_pk) = keypair_from(0x66);
    let _ = writer_pk_seed_sk;
    let addr = "zkv1test1";

    let result = replay(
        vec![
            (init_memo(&root_sk, addr), WriteStatus::Confirmed),
            (
                op_memo(&root_sk, addr, Op::OwnerAdd, &pk_hex(&owner2_pk), None),
                WriteStatus::Confirmed,
            ),
            // Root removes owner #2 (root remains, so this is allowed). The
            // target's version is 1 after the OWNERADD above.
            (
                op_memo_v(&root_sk, addr, Op::OwnerDel, &pk_hex(&owner2_pk), None, 1),
                WriteStatus::Confirmed,
            ),
            // Ex-owner tries to write.
            (
                op_memo(&owner2_sk, addr, Op::Set, "k", Some("ghost")),
                WriteStatus::Confirmed,
            ),
            // Ex-owner tries to grant a writer.
            (
                op_memo(
                    &owner2_sk,
                    addr,
                    Op::WriterAdd,
                    &pk_hex(&writer_pk),
                    Some("CREATE"),
                ),
                WriteStatus::Confirmed,
            ),
        ],
        addr,
        &root_pk,
        false,
    )
    .unwrap();
    assert!(!result.auth.is_owner(&pk_hex(&owner2_pk)));
    assert!(result.state.is_empty(), "ex-owner write must be dropped",);
    assert!(
        result.auth.authority_of(&pk_hex(&writer_pk)).is_none(),
        "ex-owner must not be able to grant writers",
    );
}

#[test]
fn last_owner_cannot_be_removed() {
    // Safety invariant: an OWNERDEL that would leave the registry with no
    // owners is dropped. Root is the only owner and tries (perhaps by
    // fat-finger) to remove itself.
    let (root_sk, root_pk) = keypair();
    let addr = "zkv1test1";

    let result = replay(
        vec![
            (init_memo(&root_sk, addr), WriteStatus::Confirmed),
            (
                op_memo(&root_sk, addr, Op::OwnerDel, &pk_hex(&root_pk), None),
                WriteStatus::Confirmed,
            ),
            // Root can still write afterwards; it never lost ownership.
            (
                op_memo(&root_sk, addr, Op::Set, "k", Some("still-owner")),
                WriteStatus::Confirmed,
            ),
        ],
        addr,
        &root_pk,
        true,
    )
    .unwrap();
    assert!(
        result.auth.is_owner(&pk_hex(&root_pk)),
        "the last owner must not be removable",
    );
    assert_eq!(
        result.state.get("k").and_then(|ks| ks.confirmed.as_deref()),
        Some("still-owner"),
    );
}

#[test]
fn replayed_self_ownerdel_cannot_remove_owner_after_second_owner_seated() {
    // Replay-protection regression (the OWNERDEL-replay gap): a sole owner's
    // self-OWNERDEL is dropped as LastOwnerProtected, but it MUST still
    // advance that target's replay-protection high-water. Otherwise the
    // unbumped, on-chain seq-0 memo stays inside the version window, and once
    // a second owner is seated (which flips the last-owner condition) anyone
    // can re-broadcast it to remove the original owner.
    let (root_sk, root_pk) = keypair();
    let (_owner2_sk, owner2_pk) = keypair_from(0x55);
    let addr = "zkv1test1";
    let root_hex = pk_hex(&root_pk);
    let owner2_hex = pk_hex(&owner2_pk);

    // The exact memo a sole owner broadcasts via `zkv roles owner remove
    // <self>` while still the only owner: OWNERDEL of root at version 0.
    let ownerdel_root_v0 = op_memo(&root_sk, addr, Op::OwnerDel, &root_hex, None);

    let result = replay(
        vec![
            (init_memo(&root_sk, addr), WriteStatus::Confirmed),
            // (1) sole owner self-removal: dropped LastOwnerProtected, but it
            //     must bump target_versions[root] to 1.
            (ownerdel_root_v0.clone(), WriteStatus::Confirmed),
            // (2) root later seats a second owner (bumps owner2's counter).
            (
                op_memo(&root_sk, addr, Op::OwnerAdd, &owner2_hex, None),
                WriteStatus::Confirmed,
            ),
            // (3) attacker replays the original seq-0 OWNERDEL of root. With
            //     the bump in place this is now StaleVersion and dropped.
            (ownerdel_root_v0, WriteStatus::Confirmed),
        ],
        addr,
        &root_pk,
        // strict = false: the replayed memo is dropped as a stale replay
        // (policy), which strict mode tolerates; the point is the *state*.
        false,
    )
    .unwrap();

    assert!(
        result.auth.is_owner(&root_hex),
        "a replayed seq-0 self-OWNERDEL must NOT remove root after a 2nd owner is seated",
    );
    assert!(
        result.auth.is_owner(&owner2_hex),
        "the legitimately-seated second owner remains",
    );
}

#[test]
fn root_owner_is_removable_once_a_second_owner_exists() {
    // Key rotation: once a second owner is seated, the root key CAN be
    // removed. The removed root then loses write authority.
    let (root_sk, root_pk) = keypair();
    let (owner2_sk, owner2_pk) = keypair_from(0x55);
    let addr = "zkv1test1";

    let result = replay(
        vec![
            (init_memo(&root_sk, addr), WriteStatus::Confirmed),
            (
                op_memo(&root_sk, addr, Op::OwnerAdd, &pk_hex(&owner2_pk), None),
                WriteStatus::Confirmed,
            ),
            // Owner #2 removes the root key.
            (
                op_memo(&owner2_sk, addr, Op::OwnerDel, &pk_hex(&root_pk), None),
                WriteStatus::Confirmed,
            ),
            // The deposed root can no longer write.
            (
                op_memo(&root_sk, addr, Op::Set, "k", Some("deposed")),
                WriteStatus::Confirmed,
            ),
            // Owner #2 still can.
            (
                op_memo(&owner2_sk, addr, Op::Set, "k", Some("by-owner2")),
                WriteStatus::Confirmed,
            ),
        ],
        addr,
        &root_pk,
        false,
    )
    .unwrap();
    assert!(
        !result.auth.is_owner(&pk_hex(&root_pk)),
        "root was rotated out"
    );
    assert!(result.auth.is_owner(&pk_hex(&owner2_pk)));
    assert_eq!(
        result.state.get("k").and_then(|ks| ks.confirmed.as_deref()),
        Some("by-owner2"),
        "deposed root's write dropped; owner2's write wins",
    );
}

#[test]
fn pending_management_op_confers_no_authority_yet() {
    // A WRITERADD that is only Confirming (in mempool) must not yet let
    // the named writer act. The data write in the same batch is dropped
    // because the grant hasn't confirmed.
    let (root_sk, root_pk) = keypair();
    let (w_sk, w_pk) = keypair_from(0x33);
    let addr = "zkv1test1";

    let result = replay(
        vec![
            (init_memo(&root_sk, addr), WriteStatus::Confirmed),
            // Grant is only in mempool.
            (
                op_memo(
                    &root_sk,
                    addr,
                    Op::WriterAdd,
                    &pk_hex(&w_pk),
                    Some("CREATE"),
                ),
                WriteStatus::Confirming {
                    done: 0,
                    required: 1,
                },
            ),
            // Writer acts immediately (confirmed write, unconfirmed grant).
            (
                op_memo(&w_sk, addr, Op::Set, "k", Some("too-soon")),
                WriteStatus::Confirmed,
            ),
        ],
        addr,
        &root_pk,
        false,
    )
    .unwrap();
    assert!(
        result.auth.authority_of(&pk_hex(&w_pk)).is_none(),
        "an unconfirmed WRITERADD must not seat the writer",
    );
    assert!(
        result.state.is_empty(),
        "write authorized only by an unconfirmed grant must be dropped",
    );
}

#[test]
fn writeradd_overwrites_scope_wholesale() {
    // A second WRITERADD for the same key replaces the scope entirely;
    // it is not additive. Narrowing CREATE,UPDATE,DESTROY down to CREATE
    // must strip UPDATE and DESTROY.
    let (root_sk, root_pk) = keypair();
    let (w_sk, w_pk) = keypair_from(0x33);
    let addr = "zkv1test1";

    let result = replay(
        vec![
            (init_memo(&root_sk, addr), WriteStatus::Confirmed),
            (
                op_memo(
                    &root_sk,
                    addr,
                    Op::WriterAdd,
                    &pk_hex(&w_pk),
                    Some("CREATE,UPDATE,DESTROY"),
                ),
                WriteStatus::Confirmed,
            ),
            // Owner seeds a key the writer will try to destroy later (k v0).
            (
                op_memo(&root_sk, addr, Op::Set, "k", Some("v")),
                WriteStatus::Confirmed,
            ),
            // Narrow the scope to CREATE only (target version 1).
            (
                op_memo_v(
                    &root_sk,
                    addr,
                    Op::WriterAdd,
                    &pk_hex(&w_pk),
                    Some("CREATE"),
                    1,
                ),
                WriteStatus::Confirmed,
            ),
            // DESTROY is no longer in scope; DEL dropped (k version 1).
            (
                op_memo_v(&w_sk, addr, Op::Del, "k", None, 1),
                WriteStatus::Confirmed,
            ),
        ],
        addr,
        &root_pk,
        false,
    )
    .unwrap();
    match result.auth.authority_of(&pk_hex(&w_pk)) {
        Some(Authority::Writer(scope)) => {
            assert!(scope.contains(Capability::Create));
            assert!(!scope.contains(Capability::Update), "UPDATE must be gone");
            assert!(!scope.contains(Capability::Destroy), "DESTROY must be gone");
        }
        other => panic!("expected narrowed writer scope, got {other:?}"),
    }
    assert_eq!(
        result.state.get("k").and_then(|ks| ks.confirmed.as_deref()),
        Some("v"),
        "narrowed writer must no longer be able to DESTROY",
    );
}

#[test]
fn owneradd_on_existing_writer_promotes_and_clears_scope() {
    // Promoting a writer to owner removes its writer entry (an owner has
    // full authority; the scoped row would just be shadowed). The pubkey
    // ends up an owner, not both.
    let (root_sk, root_pk) = keypair();
    let (p_sk, p_pk) = keypair_from(0x33);
    let _ = p_sk;
    let addr = "zkv1test1";

    let result = replay(
        vec![
            (init_memo(&root_sk, addr), WriteStatus::Confirmed),
            (
                op_memo(
                    &root_sk,
                    addr,
                    Op::WriterAdd,
                    &pk_hex(&p_pk),
                    Some("CREATE"),
                ),
                WriteStatus::Confirmed,
            ),
            // Promote the same target to owner (target version 1).
            (
                op_memo_v(&root_sk, addr, Op::OwnerAdd, &pk_hex(&p_pk), None, 1),
                WriteStatus::Confirmed,
            ),
        ],
        addr,
        &root_pk,
        true,
    )
    .unwrap();
    assert!(matches!(
        result.auth.authority_of(&pk_hex(&p_pk)),
        Some(Authority::Owner)
    ));
    assert_eq!(
        result.auth.writers().count(),
        0,
        "promoted writer must no longer appear as a writer",
    );
}

#[test]
fn management_ops_dropped_before_init() {
    // Pre-INIT management memos are noise, just like pre-INIT writes.
    // Even a root-signed OWNERADD before INIT must not seat anyone.
    let (root_sk, root_pk) = keypair();
    let (owner2_sk, owner2_pk) = keypair_from(0x55);
    let _ = owner2_sk;
    let addr = "zkv1test1";

    let result = replay(
        vec![
            // OWNERADD before INIT: dropped.
            (
                op_memo(&root_sk, addr, Op::OwnerAdd, &pk_hex(&owner2_pk), None),
                WriteStatus::Confirmed,
            ),
            (init_memo(&root_sk, addr), WriteStatus::Confirmed),
        ],
        addr,
        &root_pk,
        false,
    )
    .unwrap();
    assert!(
        !result.auth.is_owner(&pk_hex(&owner2_pk)),
        "pre-INIT OWNERADD must not take effect",
    );
    assert!(result.auth.is_owner(&pk_hex(&root_pk)));
}

#[test]
fn malformed_target_pubkey_in_management_is_dropped() {
    // An OWNERADD whose target isn't a valid pubkey is a no-op; it can
    // never correspond to a real signer, so nothing is seated. Build the
    // memo by hand because the key just needs to be non-whitespace.
    let (root_sk, root_pk) = keypair();
    let addr = "zkv1test1";
    let bogus = "not-a-pubkey";
    let payload = signed_payload(addr, Op::OwnerAdd, bogus, None);
    let sig = sign_command(&root_sk, &payload);
    let memo = build_memo(Op::OwnerAdd, bogus, None, 0, &sig).unwrap();
    let text = match Memo::try_from(memo).unwrap() {
        Memo::Text(t) => t.to_string(),
        _ => unreachable!(),
    };

    let result = replay(
        vec![
            (init_memo(&root_sk, addr), WriteStatus::Confirmed),
            (text, WriteStatus::Confirmed),
        ],
        addr,
        &root_pk,
        false,
    )
    .unwrap();
    // Only the root owner remains; the bogus target was never seated.
    assert_eq!(result.auth.owners().count(), 1);
    assert!(result.auth.is_owner(&pk_hex(&root_pk)));
}

#[test]
fn scope_parse_and_wire_round_trip() {
    // Order-insensitive, de-duplicated, canonical wire form.
    let s = Scope::parse("DESTROY,CREATE,CREATE").unwrap();
    assert!(s.contains(Capability::Create));
    assert!(s.contains(Capability::Destroy));
    assert!(!s.contains(Capability::Update));
    assert_eq!(s.to_wire(), "CREATE,DESTROY");
    // Whitespace tolerated.
    assert_eq!(
        Scope::parse(" UPDATE , CREATE ").unwrap().to_wire(),
        "CREATE,UPDATE"
    );
    // Empty and all-garbage scopes are rejected.
    assert!(Scope::parse("").is_none());
    assert!(Scope::parse("READ").is_none());
    assert!(Scope::parse(",,").is_none());
}

#[test]
fn writeradd_with_invalid_scope_is_dropped() {
    // A WRITERADD whose scope value is unparseable seats no writer.
    let (root_sk, root_pk) = keypair();
    let (w_sk, w_pk) = keypair_from(0x33);
    let _ = w_sk;
    let addr = "zkv1test1";

    let result = replay(
        vec![
            (init_memo(&root_sk, addr), WriteStatus::Confirmed),
            (
                op_memo(&root_sk, addr, Op::WriterAdd, &pk_hex(&w_pk), Some("READ")),
                WriteStatus::Confirmed,
            ),
        ],
        addr,
        &root_pk,
        false,
    )
    .unwrap();
    assert!(
        result.auth.authority_of(&pk_hex(&w_pk)).is_none(),
        "WRITERADD with an unrecognized scope must seat no writer",
    );
}

#[test]
fn owner_data_and_management_ops_round_trip_through_seed_split() {
    // The seed/tail equivalence must hold for the registry too: splitting
    // a stream of INIT + grants + writes at any confirmed boundary and
    // replaying the tail on top of the head's ReplayResult must reproduce
    // both the kv state AND the auth registry of a single-pass replay.
    let (root_sk, root_pk) = keypair();
    let (owner2_sk, owner2_pk) = keypair_from(0x55);
    let (w_sk, w_pk) = keypair_from(0x33);
    let addr = "zkv1test1";

    let entries: Vec<(String, WriteStatus)> = vec![
        (init_memo(&root_sk, addr), WriteStatus::Confirmed),
        (
            op_memo(&root_sk, addr, Op::OwnerAdd, &pk_hex(&owner2_pk), None),
            WriteStatus::Confirmed,
        ),
        (
            op_memo(
                &owner2_sk,
                addr,
                Op::WriterAdd,
                &pk_hex(&w_pk),
                Some("CREATE,UPDATE"),
            ),
            WriteStatus::Confirmed,
        ),
        (
            op_memo(&w_sk, addr, Op::Set, "k", Some("v1")),
            WriteStatus::Confirmed,
        ),
        (
            op_memo(&owner2_sk, addr, Op::Set, "k", Some("v2")),
            WriteStatus::Confirmed,
        ),
        (
            op_memo(&root_sk, addr, Op::WriterDel, &pk_hex(&w_pk), None),
            WriteStatus::Confirmed,
        ),
    ];

    let full = replay(entries.clone(), addr, &root_pk, false).unwrap();
    for split in 0..=entries.len() {
        let (head, tail) = entries.split_at(split);
        let head_result = replay(head.to_vec(), addr, &root_pk, false).unwrap();
        let combined =
            replay_with_seed(tail.to_vec(), Some(head_result), addr, &root_pk, false).unwrap();
        assert_eq!(combined.init, full.init, "init mismatch at split={split}");
        assert_eq!(
            combined.state, full.state,
            "state mismatch at split={split}"
        );
        assert_eq!(combined.auth, full.auth, "auth mismatch at split={split}");
    }
}

// ----- replay_audit / DropReason taxonomy -----

fn confirmed_entry(text: String) -> AuditEntry {
    AuditEntry {
        mined_height: Some(100),
        timestamp: None,
        txid: String::new(),
        text,
        status: WriteStatus::Confirmed,
    }
}

fn confirming_entry(text: String, done: u32, required: u32) -> AuditEntry {
    AuditEntry {
        mined_height: None,
        timestamp: None,
        txid: String::new(),
        text,
        status: WriteStatus::Confirming { done, required },
    }
}

#[test]
fn history_folding_attributes_delegated_writer_and_authorization() {
    // In a multi-signer database, a delegated writer's confirmed write is
    // attributed to the *writer* (not root) and `verified` reflects real
    // authorization: in-scope writes verify, out-of-scope ones don't.
    use std::collections::BTreeMap;
    let (root_sk, root_pk) = keypair();
    let (w_sk, w_pk) = keypair_from(0x33);
    let addr = "zkv1test1";
    let root_hex = pubkey_bech32(&root_pk);
    let w_hex = pubkey_bech32(&w_pk);

    let mut init = InitState::Uninitialized;
    let mut auth = AuthRegistry::default();
    let mut finalized = false;
    let mut state: BTreeMap<String, KeyState> = BTreeMap::new();
    let mut kv_versions: BTreeMap<String, u64> = BTreeMap::new();
    let mut target_versions: BTreeMap<String, u64> = BTreeMap::new();
    let mut fold = |text: String| {
        history_entry_folding(
            addr,
            &root_hex,
            &text,
            Some(100),
            None,
            String::new(),
            0,
            HistoryStatus::Confirmed { confirmations: 10 },
            &WriteStatus::Confirmed,
            &mut init,
            &mut auth,
            &mut finalized,
            &mut state,
            &mut kv_versions,
            &mut target_versions,
        )
    };

    // INIT emits the genesis entry, signed by (and attributed to) root.
    let e = fold(init_memo(&root_sk, addr)).expect("INIT emits an entry");
    assert_eq!(e.op, Op::Init);
    assert_eq!(e.signer.as_deref(), Some(root_hex.as_str()));
    assert_eq!(e.verified, Some(true));

    // WRITERADD folds into the registry but is not a write-log entry.
    assert!(
        fold(op_memo(
            &root_sk,
            addr,
            Op::WriterAdd,
            &w_hex,
            Some("CREATE,UPDATE")
        ))
        .is_none(),
        "a management op emits no history entry",
    );

    // Writer SET on a new key: authorized via CREATE, attributed to writer.
    let e = fold(op_memo(&w_sk, addr, Op::Set, "k", Some("v1"))).expect("SET emits");
    assert_eq!(e.signer.as_deref(), Some(w_hex.as_str()));
    assert_eq!(e.verified, Some(true));

    // Writer DEL needs DESTROY, which this writer lacks → unauthorized, but
    // still shown in history (attributed to the writer, verified=false). The
    // key already had a confirmed SET (version 1), so the DEL signs over
    // version 1; otherwise it would fail on the signature, not on scope.
    let e = fold(op_memo_v(&w_sk, addr, Op::Del, "k", None, 1)).expect("DEL still emits");
    assert_eq!(e.op, Op::Del);
    assert_eq!(e.signer.as_deref(), Some(w_hex.as_str()));
    assert_eq!(e.verified, Some(false));
}

#[test]
fn audit_row_signer_splits_unauthorized_from_bad_signature() {
    // The rejections "Valid Signature ✓ / Authorized ✗" split rests on
    // AuditRow.signer being present iff the signature recovered.
    let (root_sk, root_pk) = keypair();
    let (w_sk, w_pk) = keypair_from(0x33);
    let addr = "zkv1test1";

    let entries = vec![
        confirmed_entry(init_memo(&root_sk, addr)),
        confirmed_entry(op_memo(
            &root_sk,
            addr,
            Op::WriterAdd,
            &pk_hex(&w_pk),
            Some("CREATE"),
        )),
        // CREATE a new key: authorized (version 0).
        confirmed_entry(op_memo(&w_sk, addr, Op::Set, "k", Some("v1"))),
        // UPDATE the now-existing key (version 1): out of scope (no UPDATE
        // capability), so it must drop on authorization, not signature.
        confirmed_entry(op_memo_v(&w_sk, addr, Op::Set, "k", Some("v2"), 1)),
    ];
    let audit = replay_audit(entries, addr, &root_pk);

    let oos = audit
        .rows
        .iter()
        .find(|r| {
            matches!(
                r.outcome,
                RowOutcome::Dropped(DropReason::OutOfScope { .. })
            )
        })
        .expect("an out-of-scope rejection");
    // Valid signature: signer recovered, and the drop is not a sig failure.
    assert_eq!(oos.signer.as_deref(), Some(pk_hex(&w_pk).as_str()));
    if let RowOutcome::Dropped(reason) = oos.outcome {
        assert!(!reason.is_signature_failure());
    }
}

#[test]
fn revoked_roles_reports_owner_and_writer_tombstones() {
    // A granted-then-revoked owner and writer each become a tombstone with
    // the revoking owner recorded; current roles stay only on `auth`.
    let (root_sk, root_pk) = keypair();
    let (_owner2_sk, owner2_pk) = keypair_from(0x55);
    let (_w_sk, w_pk) = keypair_from(0x33);
    let addr = "zkv1test1";
    let root_hex = pk_hex(&root_pk);
    let o2 = pk_hex(&owner2_pk);
    let w = pk_hex(&w_pk);

    let entries = vec![
        confirmed_entry(init_memo(&root_sk, addr)),
        confirmed_entry(op_memo(&root_sk, addr, Op::OwnerAdd, &o2, None)),
        confirmed_entry(op_memo(
            &root_sk,
            addr,
            Op::WriterAdd,
            &w,
            Some("CREATE,UPDATE"),
        )),
        // Each revoke targets a pubkey already granted once → version 1.
        confirmed_entry(op_memo_v(&root_sk, addr, Op::WriterDel, &w, None, 1)),
        confirmed_entry(op_memo_v(&root_sk, addr, Op::OwnerDel, &o2, None, 1)),
    ];
    let audit = replay_audit(entries, addr, &root_pk);

    // Current registry: only root remains an owner.
    assert!(audit.auth.is_owner(&root_hex));
    assert_eq!(audit.auth.owners().count(), 1);
    assert_eq!(audit.auth.writers().count(), 0);

    let revoked = revoked_roles(&audit);
    assert_eq!(revoked.len(), 2);

    let wt = revoked
        .iter()
        .find(|r| r.pubkey == w)
        .expect("writer tombstone");
    assert!(!wt.was_owner);
    assert_eq!(
        wt.capabilities,
        vec!["CREATE".to_owned(), "UPDATE".to_owned()]
    );
    assert_eq!(wt.revoked_by.as_deref(), Some(root_hex.as_str()));

    let ot = revoked
        .iter()
        .find(|r| r.pubkey == o2)
        .expect("owner tombstone");
    assert!(ot.was_owner);
    assert!(ot.capabilities.is_empty());
    assert_eq!(ot.revoked_by.as_deref(), Some(root_hex.as_str()));
}

#[test]
fn revoked_roles_clears_tombstone_on_regrant() {
    // A writer revoked and then re-granted is current, not a tombstone.
    let (root_sk, root_pk) = keypair();
    let (_w_sk, w_pk) = keypair_from(0x33);
    let addr = "zkv1test1";
    let w = pk_hex(&w_pk);

    // Grant (v0), revoke (v1), re-grant (v2): each op targets the same
    // pubkey, so the target version advances every time.
    let entries = vec![
        confirmed_entry(init_memo(&root_sk, addr)),
        confirmed_entry(op_memo(&root_sk, addr, Op::WriterAdd, &w, Some("CREATE"))),
        confirmed_entry(op_memo_v(&root_sk, addr, Op::WriterDel, &w, None, 1)),
        confirmed_entry(op_memo_v(
            &root_sk,
            addr,
            Op::WriterAdd,
            &w,
            Some("UPDATE"),
            2,
        )),
    ];
    let audit = replay_audit(entries, addr, &root_pk);
    assert!(audit.auth.authority_of(&w).is_some(), "writer is current");
    assert!(
        revoked_roles(&audit).is_empty(),
        "re-granted writer leaves no tombstone",
    );
}

#[test]
fn granted_roles_reports_creator_and_active_grants_with_provenance() {
    // INIT makes root the creator (`via_init`); later OWNERADD/WRITERADD
    // record the granting owner. A revoked key drops out entirely, and the
    // survivors are exactly the complement of the tombstones.
    let (root_sk, root_pk) = keypair();
    let (_owner2_sk, owner2_pk) = keypair_from(0x55);
    let (_w_sk, w_pk) = keypair_from(0x33);
    let (_gone_sk, gone_pk) = keypair_from(0x44);
    let addr = "zkv1test1";
    let root = pk_hex(&root_pk);
    let o2 = pk_hex(&owner2_pk);
    let w = pk_hex(&w_pk);
    let gone = pk_hex(&gone_pk);

    let entries = vec![
        confirmed_entry(init_memo(&root_sk, addr)),
        confirmed_entry(op_memo(&root_sk, addr, Op::OwnerAdd, &o2, None)),
        confirmed_entry(op_memo(
            &root_sk,
            addr,
            Op::WriterAdd,
            &w,
            Some("CREATE,UPDATE"),
        )),
        confirmed_entry(op_memo(
            &root_sk,
            addr,
            Op::WriterAdd,
            &gone,
            Some("DESTROY"),
        )),
        // Revoke targets `gone`, already granted once → version 1.
        confirmed_entry(op_memo_v(&root_sk, addr, Op::WriterDel, &gone, None, 1)),
    ];
    let audit = replay_audit(entries, addr, &root_pk);
    let granted = granted_roles(&audit);

    // Three survivors (root, owner2, writer); the revoked writer is gone.
    assert_eq!(granted.len(), 3);
    assert!(granted.iter().all(|g| g.pubkey != gone));
    // Owners first, then writers (the registry's iteration order).
    assert!(granted[0].is_owner && granted[1].is_owner && !granted[2].is_owner);

    let creator = granted.iter().find(|g| g.pubkey == root).expect("creator");
    assert!(creator.via_init, "root was granted by INIT");
    assert!(creator.is_owner);
    assert_eq!(creator.height, Some(100));
    assert_eq!(creator.granted_by.as_deref(), Some(root.as_str()));

    let owner2 = granted.iter().find(|g| g.pubkey == o2).expect("owner2");
    assert!(!owner2.via_init && owner2.is_owner);
    assert_eq!(owner2.granted_by.as_deref(), Some(root.as_str()));

    let writer = granted.iter().find(|g| g.pubkey == w).expect("writer");
    assert_eq!(
        writer.capabilities,
        vec!["CREATE".to_owned(), "UPDATE".to_owned()]
    );
    assert_eq!(writer.granted_by.as_deref(), Some(root.as_str()));

    // Survivors are the complement of the tombstones: no overlap, full cover.
    let revoked = revoked_roles(&audit);
    assert_eq!(revoked.len(), 1);
    assert_eq!(revoked[0].pubkey, gone);
}

/// The outcome of the last row in a history result (the row under test).
fn last_outcome(res: &AuditResult) -> RowOutcome {
    res.rows.last().expect("at least one row").outcome
}

#[test]
fn history_reports_forged_init() {
    let (_, root_pk) = keypair();
    let (attacker_sk, _) = keypair_from(9);
    let addr = "zkv1test1";
    // INIT signed by someone other than the root key.
    let res = replay_audit(
        vec![confirmed_entry(init_memo(&attacker_sk, addr))],
        addr,
        &root_pk,
    );
    assert_eq!(
        last_outcome(&res),
        RowOutcome::Dropped(DropReason::ForgedInit)
    );
    assert_eq!(res.init, InitState::Uninitialized);
}

#[test]
fn history_reports_duplicate_init() {
    let (root_sk, root_pk) = keypair();
    let addr = "zkv1test1";
    let res = replay_audit(
        vec![
            confirmed_entry(init_memo(&root_sk, addr)),
            confirmed_entry(init_memo(&root_sk, addr)),
        ],
        addr,
        &root_pk,
    );
    assert_eq!(res.rows[0].outcome, RowOutcome::Applied);
    assert_eq!(
        res.rows[1].outcome,
        RowOutcome::Dropped(DropReason::DuplicateInit)
    );
}

#[test]
fn history_reports_pre_init_set() {
    let (root_sk, root_pk) = keypair();
    let addr = "zkv1test1";
    let res = replay_audit(
        vec![confirmed_entry(op_memo(
            &root_sk,
            addr,
            Op::Set,
            "k",
            Some("v"),
        ))],
        addr,
        &root_pk,
    );
    assert_eq!(
        last_outcome(&res),
        RowOutcome::Dropped(DropReason::NotInitialized)
    );
}

#[test]
fn history_reports_no_write_authority() {
    let (root_sk, root_pk) = keypair();
    let (stranger_sk, _) = keypair_from(5);
    let addr = "zkv1test1";
    let res = replay_audit(
        vec![
            confirmed_entry(init_memo(&root_sk, addr)),
            confirmed_entry(op_memo(&stranger_sk, addr, Op::Set, "k", Some("v"))),
        ],
        addr,
        &root_pk,
    );
    assert_eq!(
        last_outcome(&res),
        RowOutcome::Dropped(DropReason::NoWriteAuthority)
    );
}

#[test]
fn history_reports_non_owner_management() {
    let (root_sk, root_pk) = keypair();
    let (stranger_sk, stranger_pk) = keypair_from(5);
    let addr = "zkv1test1";
    let target = pubkey_bech32(&stranger_pk);
    let res = replay_audit(
        vec![
            confirmed_entry(init_memo(&root_sk, addr)),
            confirmed_entry(op_memo(&stranger_sk, addr, Op::OwnerAdd, &target, None)),
        ],
        addr,
        &root_pk,
    );
    assert_eq!(
        last_outcome(&res),
        RowOutcome::Dropped(DropReason::NotOwner)
    );
}

#[test]
fn history_reports_last_owner_protected() {
    let (root_sk, root_pk) = keypair();
    let addr = "zkv1test1";
    let root_hex = pubkey_bech32(&root_pk);
    // The sole owner tries to remove itself.
    let res = replay_audit(
        vec![
            confirmed_entry(init_memo(&root_sk, addr)),
            confirmed_entry(op_memo(&root_sk, addr, Op::OwnerDel, &root_hex, None)),
        ],
        addr,
        &root_pk,
    );
    assert_eq!(
        last_outcome(&res),
        RowOutcome::Dropped(DropReason::LastOwnerProtected)
    );
    // Registry must still hold the owner.
    assert!(res.auth.is_owner(&root_hex));
}

#[test]
fn history_reports_writer_target_is_owner() {
    let (root_sk, root_pk) = keypair();
    let (_, second_pk) = keypair_from(6);
    let addr = "zkv1test1";
    let second = pubkey_bech32(&second_pk);
    let res = replay_audit(
        vec![
            confirmed_entry(init_memo(&root_sk, addr)),
            // Promote a second owner (target version 0)...
            confirmed_entry(op_memo(&root_sk, addr, Op::OwnerAdd, &second, None)),
            // ...then try to seat that owner as a scoped writer (version 1).
            confirmed_entry(op_memo_v(
                &root_sk,
                addr,
                Op::WriterAdd,
                &second,
                Some("CREATE"),
                1,
            )),
        ],
        addr,
        &root_pk,
    );
    assert_eq!(
        last_outcome(&res),
        RowOutcome::Dropped(DropReason::WriterTargetIsOwner)
    );
}

#[test]
fn history_reports_invalid_scope() {
    let (root_sk, root_pk) = keypair();
    let (_, w_pk) = keypair_from(6);
    let addr = "zkv1test1";
    let w = pubkey_bech32(&w_pk);
    let res = replay_audit(
        vec![
            confirmed_entry(init_memo(&root_sk, addr)),
            confirmed_entry(op_memo(&root_sk, addr, Op::WriterAdd, &w, Some("NONSENSE"))),
        ],
        addr,
        &root_pk,
    );
    assert_eq!(
        last_outcome(&res),
        RowOutcome::Dropped(DropReason::InvalidScope)
    );
}

#[test]
fn history_reports_invalid_target_pubkey() {
    let (root_sk, root_pk) = keypair();
    let addr = "zkv1test1";
    let res = replay_audit(
        vec![
            confirmed_entry(init_memo(&root_sk, addr)),
            confirmed_entry(op_memo(&root_sk, addr, Op::OwnerAdd, "not-a-pubkey", None)),
        ],
        addr,
        &root_pk,
    );
    assert_eq!(
        last_outcome(&res),
        RowOutcome::Dropped(DropReason::InvalidTargetPubkey)
    );
}

#[test]
fn history_reports_out_of_scope_and_key_exists_flip() {
    let (root_sk, root_pk) = keypair();
    let (w_sk, w_pk) = keypair_from(6);
    let addr = "zkv1test1";
    let w = pubkey_bech32(&w_pk);
    // Writer with CREATE only.
    let res = replay_audit(
        vec![
            confirmed_entry(init_memo(&root_sk, addr)),
            confirmed_entry(op_memo(&root_sk, addr, Op::WriterAdd, &w, Some("CREATE"))),
            // First SET creates the key: allowed (version 0 → key v1).
            confirmed_entry(op_memo(&w_sk, addr, Op::Set, "k", Some("v1"))),
            // Second SET would be an update: out of scope. It is dropped
            // (no version bump), so it and the DEL below both sign version 1.
            confirmed_entry(op_memo_v(&w_sk, addr, Op::Set, "k", Some("v2"), 1)),
            // DEL is also out of scope (no DESTROY); still version 1.
            confirmed_entry(op_memo_v(&w_sk, addr, Op::Del, "k", None, 1)),
        ],
        addr,
        &root_pk,
    );
    assert_eq!(res.rows[2].outcome, RowOutcome::Applied);
    assert_eq!(
        res.rows[3].outcome,
        RowOutcome::Dropped(DropReason::OutOfScope {
            capability: Capability::Update
        })
    );
    assert_eq!(
        res.rows[4].outcome,
        RowOutcome::Dropped(DropReason::OutOfScope {
            capability: Capability::Destroy
        })
    );
    // The confirmed value is the first (only applied) write.
    assert_eq!(res.state.get("k").unwrap().confirmed.as_deref(), Some("v1"));
}

#[test]
fn history_reports_stale_version_replay() {
    // A verbatim re-broadcast of an already-honored write is defeated by
    // version-CAS and the audit labels it `StaleVersion` (not the generic
    // `NoWriteAuthority` its wrong-version recovery would otherwise yield),
    // distinguishing a defeated replay from a never-authorized writer.
    let (root_sk, root_pk) = keypair();
    let (w_sk, w_pk) = keypair_from(0x33);
    let addr = "zkv1test1";
    let w = pubkey_bech32(&w_pk);

    // INIT, grant a full-scope writer, writer SET "k" (v0), writer UPDATE
    // "k" (v1); then the attacker re-broadcasts the v0 SET verbatim.
    let set_v0 = op_memo(&w_sk, addr, Op::Set, "k", Some("v1"));
    let res = replay_audit(
        vec![
            confirmed_entry(init_memo(&root_sk, addr)),
            confirmed_entry(op_memo(
                &root_sk,
                addr,
                Op::WriterAdd,
                &w,
                Some("CREATE,UPDATE"),
            )),
            confirmed_entry(set_v0.clone()),
            confirmed_entry(op_memo_v(&w_sk, addr, Op::Set, "k", Some("v2"), 1)),
            // Verbatim replay of the original v0 create; must be flagged.
            confirmed_entry(set_v0),
        ],
        addr,
        &root_pk,
    );
    assert_eq!(
        last_outcome(&res),
        RowOutcome::Dropped(DropReason::StaleVersion),
    );
    // It is not a signature failure (the signature is real), and the live
    // state still reflects the honored v1 update.
    assert!(!DropReason::StaleVersion.is_signature_failure());
    assert_eq!(res.state.get("k").unwrap().confirmed.as_deref(), Some("v2"));

    // A management replay is caught too: OWNERADD a second owner (target
    // v0), then re-broadcast that exact OWNERADD after its version moved on.
    let (_o2_sk, o2_pk) = keypair_from(0x55);
    let o2 = pubkey_bech32(&o2_pk);
    let owneradd_v0 = op_memo(&root_sk, addr, Op::OwnerAdd, &o2, None);
    let res2 = replay_audit(
        vec![
            confirmed_entry(init_memo(&root_sk, addr)),
            confirmed_entry(owneradd_v0.clone()),
            // Bump the target's version with an OWNERDEL (v1)...
            confirmed_entry(op_memo_v(&root_sk, addr, Op::OwnerDel, &o2, None, 1)),
            // ...then replay the original OWNERADD verbatim.
            confirmed_entry(owneradd_v0),
        ],
        addr,
        &root_pk,
    );
    assert_eq!(
        last_outcome(&res2),
        RowOutcome::Dropped(DropReason::StaleVersion),
    );
}

#[test]
fn history_reports_bad_signature() {
    let (_, root_pk) = keypair();
    let addr = "zkv1test1";
    // A well-framed memo whose signature is all zeroes; doesn't recover.
    let memo = build_memo(Op::Set, "k", Some("v"), 0, &[0u8; SIG_LEN]).unwrap();
    let text = match Memo::try_from(memo).unwrap() {
        Memo::Text(t) => t.to_string(),
        _ => unreachable!(),
    };
    let res = replay_audit(vec![confirmed_entry(text)], addr, &root_pk);
    assert_eq!(
        last_outcome(&res),
        RowOutcome::Dropped(DropReason::BadSignature)
    );
}

#[test]
fn history_reports_malformed_unknown_opcode() {
    let (_, root_pk) = keypair();
    let addr = "zkv1test1";
    let text = format!("ZKV0 BOGUS somekey\n{}", "0".repeat(SIG_HEX_LEN));
    let res = replay_audit(vec![confirmed_entry(text)], addr, &root_pk);
    assert_eq!(
        last_outcome(&res),
        RowOutcome::Dropped(DropReason::MalformedMemo(MemoFormat::UnknownOpcode))
    );
}

#[test]
fn history_filters_non_zkv_memos() {
    let (root_sk, root_pk) = keypair();
    let addr = "zkv1test1";
    let res = replay_audit(
        vec![
            confirmed_entry("just a personal note, not a command".to_owned()),
            confirmed_entry(init_memo(&root_sk, addr)),
        ],
        addr,
        &root_pk,
    );
    // Only the INIT becomes a row; the plain note is filtered out.
    assert_eq!(res.rows.len(), 1);
    assert_eq!(res.rows[0].op, Some(Op::Init));
}

#[test]
fn history_confirming_data_is_pending_not_dropped() {
    let (root_sk, root_pk) = keypair();
    let addr = "zkv1test1";
    let res = replay_audit(
        vec![
            confirmed_entry(init_memo(&root_sk, addr)),
            confirming_entry(op_memo(&root_sk, addr, Op::Set, "k", Some("v")), 1, 3),
        ],
        addr,
        &root_pk,
    );
    assert_eq!(last_outcome(&res), RowOutcome::Pending);
}

#[test]
fn history_confirming_management_is_pending_then_applied_when_confirmed() {
    let (root_sk, root_pk) = keypair();
    let (_, w_pk) = keypair_from(6);
    let addr = "zkv1test1";
    let w = pubkey_bech32(&w_pk);
    // A confirming WRITERADD confers no authority yet → Pending, no change.
    let res = replay_audit(
        vec![
            confirmed_entry(init_memo(&root_sk, addr)),
            confirming_entry(
                op_memo(&root_sk, addr, Op::WriterAdd, &w, Some("CREATE")),
                1,
                3,
            ),
        ],
        addr,
        &root_pk,
    );
    assert_eq!(last_outcome(&res), RowOutcome::Pending);
    assert!(res.auth.authority_of(&w).is_none());

    // The same op, confirmed, applies.
    let res = replay_audit(
        vec![
            confirmed_entry(init_memo(&root_sk, addr)),
            confirmed_entry(op_memo(&root_sk, addr, Op::WriterAdd, &w, Some("CREATE"))),
        ],
        addr,
        &root_pk,
    );
    assert_eq!(last_outcome(&res), RowOutcome::Applied);
    assert!(matches!(
        res.auth.authority_of(&w),
        Some(Authority::Writer(_))
    ));
}

#[test]
fn history_matches_replay_on_applied_subset() {
    // The shared classifier means replay_audit's final (init, auth, and
    // pruned state) must agree with the canonical replay reducer.
    let (root_sk, root_pk) = keypair();
    let (w_sk, w_pk) = keypair_from(6);
    let addr = "zkv1test1";
    let w = pubkey_bech32(&w_pk);
    let texts = vec![
        init_memo(&root_sk, addr),
        op_memo(&root_sk, addr, Op::WriterAdd, &w, Some("CREATE,UPDATE")),
        op_memo(&w_sk, addr, Op::Set, "k", Some("v1")),
        op_memo(&w_sk, addr, Op::Set, "k", Some("v2")),
        op_memo(&root_sk, addr, Op::Del, "k", None),
        op_memo(&root_sk, addr, Op::Set, "other", Some("x")),
    ];
    let replay_res = replay(
        texts
            .iter()
            .cloned()
            .map(|t| (t, WriteStatus::Confirmed))
            .collect::<Vec<_>>(),
        addr,
        &root_pk,
        false,
    )
    .unwrap();
    let hist = replay_audit(
        texts.into_iter().map(confirmed_entry).collect::<Vec<_>>(),
        addr,
        &root_pk,
    );
    assert_eq!(replay_res.init, hist.init);
    assert_eq!(replay_res.auth, hist.auth);
    // replay prunes empty keys; history keeps them. Compare after pruning.
    let mut pruned = hist.state.clone();
    pruned.retain(|_, ks| {
        ks.confirmed.is_some()
            || ks
                .pending
                .iter()
                .any(|op| matches!(op, PendingOp::Set { .. }))
    });
    assert_eq!(replay_res.state, pruned);
}

#[test]
fn audit_init_embedded_address_is_advisory_not_gated() {
    // Receiver-binding makes the embedded INIT address advisory: a
    // root-signed, receiver-bound INIT is Applied even when its plaintext
    // echo is garbage or another database's address; INIT is **not** gated
    // on the embedded address at all. Identity rests entirely on the
    // receiver-bound root signature. A non-root signer is still ForgedInit.
    let (root_sk, root_pk) = keypair();
    let addr = "zkv1test1";

    for echo in ["zkv1notaufvk1", "zkv1testother", "totally-different"] {
        // Correctly sign the receiver-only INIT for `addr`, echo something else.
        let sig = sign_command(&root_sk, &signed_init_payload(addr));
        let memo = build_memo(Op::Init, echo, None, 0, &sig).unwrap();
        let text = match Memo::try_from(memo).unwrap() {
            Memo::Text(t) => t.to_string(),
            _ => unreachable!(),
        };
        let res = replay_audit(vec![confirmed_entry(text)], addr, &root_pk);
        assert_eq!(
            last_outcome(&res),
            RowOutcome::Applied,
            "advisory echo {echo:?} must not gate a receiver-bound INIT",
        );
    }

    // A non-root signer over our receiver is still a forgery.
    let (attacker_sk, _) = keypair_from(0x11);
    let sig = sign_command(&attacker_sk, &signed_init_payload(addr));
    let memo = build_memo(Op::Init, addr, None, 0, &sig).unwrap();
    let text = match Memo::try_from(memo).unwrap() {
        Memo::Text(t) => t.to_string(),
        _ => unreachable!(),
    };
    let res = replay_audit(vec![confirmed_entry(text)], addr, &root_pk);
    assert_eq!(
        last_outcome(&res),
        RowOutcome::Dropped(DropReason::ForgedInit),
    );
}

#[test]
fn history_reports_unsupported_version() {
    let (_, root_pk) = keypair();
    let addr = "zkv1test1";
    // A memo from a future protocol version (ZKV1; this build is ZKV0). We
    // can't build it via build_memo (which emits our version), so hand-craft
    // the wire form.
    let text = format!("ZKV1 SET k v\n{}", "0".repeat(SIG_HEX_LEN));
    let res = replay_audit(vec![confirmed_entry(text)], addr, &root_pk);
    assert_eq!(
        last_outcome(&res),
        RowOutcome::Dropped(DropReason::UnsupportedVersion { version: 1 })
    );
    // It IS surfaced as a row (so `--include-invalid` shows it), unlike a
    // non-zkv memo which is filtered out entirely.
    assert_eq!(res.rows.len(), 1);
}

#[test]
fn wire_magic_matches_version_constant() {
    assert_eq!(WIRE_MAGIC, format!("{MAGIC_PREFIX}{ZKV_VERSION}"));
}

#[test]
fn signed_magic_matches_version_constant() {
    // The signing-domain tag must move with the protocol version, or
    // signatures would be made over a stale domain when ZKV_VERSION bumps
    // (e.g. launching as ZKV0). Guards all three magics together.
    assert_eq!(
        SIGNED_MAGIC,
        format!("{MAGIC_PREFIX}{ZKV_VERSION}").as_bytes()
    );
}

#[test]
fn parse_detailed_unsupported_version() {
    let sig = "0".repeat(SIG_HEX_LEN);
    // Newer versions → UnsupportedVersion carrying the parsed number.
    for v in [2u32, 9, 42] {
        let two_section = format!("ZKV{v} SET k value\n{sig}");
        assert_eq!(
            parse_text_memo_detailed(&two_section),
            Err(MemoReject::UnsupportedVersion(v)),
        );
        // Collapsed (no newline) form must gate on version too.
        let collapsed = format!("ZKV{v} SET k value {sig}");
        assert_eq!(
            parse_text_memo_detailed(&collapsed),
            Err(MemoReject::UnsupportedVersion(v)),
        );
    }
    // Our own version still parses; a non-numeric/foreign magic is NotZkv.
    assert!(parse_text_memo_detailed(&format!("ZKV0 DEL k\n{sig}")).is_ok());
    assert_eq!(
        parse_text_memo_detailed(&format!("ZKVX DEL k\n{sig}")),
        Err(MemoReject::NotZkv),
    );
}

// ----- parse_text_memo_detailed: MemoFormat sub-causes -----

fn sig_hex_stub() -> String {
    "0".repeat(SIG_HEX_LEN)
}

#[test]
fn parse_detailed_distinguishes_not_zkv_from_malformed() {
    assert_eq!(
        parse_text_memo_detailed("hello world"),
        Err(MemoReject::NotZkv)
    );
    let text = format!("ZKV0 BOGUS k\n{}", sig_hex_stub());
    assert_eq!(
        parse_text_memo_detailed(&text),
        Err(MemoReject::Malformed(MemoFormat::UnknownOpcode))
    );
}

#[test]
fn parse_detailed_empty_key() {
    let text = format!("ZKV0 SET \n{}", sig_hex_stub());
    assert_eq!(
        parse_text_memo_detailed(&text),
        Err(MemoReject::Malformed(MemoFormat::EmptyKey))
    );
}

#[test]
fn parse_detailed_wrong_arity_del() {
    let text = format!("ZKV0 DEL k extra\n{}", sig_hex_stub());
    assert_eq!(
        parse_text_memo_detailed(&text),
        Err(MemoReject::Malformed(MemoFormat::WrongArity {
            op: Op::Del
        }))
    );
}

#[test]
fn parse_detailed_missing_value_and_scope() {
    let set = format!("ZKV0 SET k\n{}", sig_hex_stub());
    assert_eq!(
        parse_text_memo_detailed(&set),
        Err(MemoReject::Malformed(MemoFormat::MissingValue))
    );
    let wset = format!("ZKV0 WRITERADD k\n{}", sig_hex_stub());
    assert_eq!(
        parse_text_memo_detailed(&wset),
        Err(MemoReject::Malformed(MemoFormat::MissingScope))
    );
}

#[test]
fn parse_detailed_missing_and_bad_signature() {
    let missing = "ZKV0 DEL k\n".to_string();
    assert_eq!(
        parse_text_memo_detailed(&missing),
        Err(MemoReject::Malformed(MemoFormat::MissingSignature))
    );
    let bad = "ZKV0 DEL k\nnot-130-hex-chars".to_string();
    assert_eq!(
        parse_text_memo_detailed(&bad),
        Err(MemoReject::Malformed(MemoFormat::BadSignatureFraming))
    );
}

#[test]
fn parse_detailed_setl_length_faults() {
    // Non-numeric length.
    let nan = format!("ZKV0 SETL k notnum\nvalue\n{}", sig_hex_stub());
    assert_eq!(
        parse_text_memo_detailed(&nan),
        Err(MemoReject::Malformed(MemoFormat::SetlNonNumericLength))
    );
    // Length longer than the available body.
    let overrun = format!("ZKV0 SETL k 9999\nv\n{}", sig_hex_stub());
    assert_eq!(
        parse_text_memo_detailed(&overrun),
        Err(MemoReject::Malformed(MemoFormat::SetlLengthOverrun))
    );
}

#[test]
fn parse_detailed_setl_zero_length_is_valid() {
    // The canonical empty-value encoding; must NOT be rejected.
    let text = format!("ZKV0 SETL k 0\n\n{}", sig_hex_stub());
    let cmd = parse_text_memo_detailed(&text).expect("SETL 0 is valid");
    assert_eq!(cmd.op, Op::SetL);
    assert_eq!(cmd.value.as_deref(), Some(""));
}

#[test]
fn parse_detailed_setl_collapsed_unsupported() {
    // A SETL with no newline at all (collapsed transport).
    let text = format!("ZKV0 SETL k 1 v {}", sig_hex_stub());
    assert_eq!(
        parse_text_memo_detailed(&text),
        Err(MemoReject::Malformed(MemoFormat::SetlCollapsedUnsupported))
    );
}

// ---------- VERSION opcode (read-only detection) ----------

/// A signed VERSION memo for `addr` announcing epoch `n` with block `flags`.
fn version_memo(sk: &secp256k1::SecretKey, addr: &str, n: u32, flags: &str) -> String {
    op_memo(sk, addr, Op::Version, &n.to_string(), Some(flags))
}

/// INIT (by root) followed by `memos`, all confirmed; returns the replayed
/// state (with `.version`).
fn replay_confirmed(
    addr: &str,
    root_pk: &secp256k1::PublicKey,
    memos: Vec<String>,
) -> ReplayResult {
    let entries: Vec<(String, WriteStatus)> = memos
        .into_iter()
        .map(|t| (t, WriteStatus::Confirmed))
        .collect();
    replay(entries, addr, root_pk, false).unwrap()
}

#[test]
fn blockset_parse_and_wire_round_trip() {
    assert!(BlockSet::parse("warn").unwrap().is_empty());
    assert_eq!(BlockSet::parse("warn").unwrap().to_wire(), "warn");
    assert_eq!(BlockSet::parse("blockall").unwrap(), BlockSet::all());
    assert_eq!(BlockSet::all().to_wire(), "blockall");

    let w = BlockSet::parse("blockwrite").unwrap();
    assert!(w.contains(BlockCap::Write) && !w.contains(BlockCap::Read));
    assert_eq!(w.to_wire(), "blockwrite");

    // Canonical order regardless of input order, and dedup.
    assert_eq!(
        BlockSet::parse("blockwrite,blocksync").unwrap().to_wire(),
        "blocksync,blockwrite",
    );
    assert_eq!(
        BlockSet::parse("blockread,blockread").unwrap().to_wire(),
        "blockread",
    );

    // Invalid tokens / empty / mixing alias with members.
    assert!(BlockSet::parse("").is_none());
    assert!(BlockSet::parse("blockfoo").is_none());
    assert!(BlockSet::parse("warn,blockwrite").is_none());
}

#[test]
fn version_state_predicates_honor_max_db_version() {
    // At or below MAX: never blocks, regardless of flags.
    let cur = VersionState {
        version: MAX_DB_VERSION,
        blocks: BlockSet::all(),
    };
    assert!(!cur.is_outdated());
    assert!(!cur.blocks_sync() && !cur.blocks_read() && !cur.blocks_write());
    assert!(cur.upgrade_warning().is_none());

    // Above MAX: blocks exactly the flagged capabilities.
    let newer = VersionState {
        version: MAX_DB_VERSION + 1,
        blocks: BlockSet::parse("blockwrite").unwrap(),
    };
    assert!(newer.is_outdated());
    assert!(newer.blocks_write());
    assert!(!newer.blocks_read() && !newer.blocks_sync());
    assert!(newer.upgrade_warning().is_some());
}

#[test]
fn version_transition_allowed_matrix() {
    use DropReason::*;
    assert!(VersionState::transition_allowed(1, 2).is_ok()); // single step up
    assert!(VersionState::transition_allowed(5, 1).is_ok()); // free downgrade
    assert!(VersionState::transition_allowed(5, 4).is_ok()); // downgrade one
    assert_eq!(VersionState::transition_allowed(1, 1), Err(VersionNoOp));
    assert_eq!(
        VersionState::transition_allowed(1, 3),
        Err(VersionJumpTooLarge {
            current: 1,
            requested: 3
        }),
    );
    // With genesis at 0, no `u32` request can fall below it, so the
    // below-genesis branch is unreachable; a downgrade to 0 is a normal
    // (allowed) downgrade.
    assert!(VersionState::transition_allowed(2, 0).is_ok());
}

#[test]
fn version_one_step_upgrade_applies_with_flags() {
    let (root_sk, root_pk) = keypair();
    let addr = "zkv1test1";
    let res = replay_confirmed(
        addr,
        &root_pk,
        vec![
            init_memo(&root_sk, addr),
            version_memo(&root_sk, addr, 1, "blockwrite"),
        ],
    );
    assert_eq!(res.version.version, 1);
    assert!(res.version.blocks.contains(BlockCap::Write));
    assert!(!res.version.blocks.contains(BlockCap::Read));
    assert_eq!(res.version.blocks.to_wire(), "blockwrite");
}

#[test]
fn version_blockall_and_warn_round_trip_through_replay() {
    let (root_sk, root_pk) = keypair();
    let addr = "zkv1test1";

    let all = replay_confirmed(
        addr,
        &root_pk,
        vec![
            init_memo(&root_sk, addr),
            version_memo(&root_sk, addr, 1, "blockall"),
        ],
    );
    assert_eq!(all.version.version, 1);
    assert_eq!(all.version.blocks, BlockSet::all());

    let warn = replay_confirmed(
        addr,
        &root_pk,
        vec![
            init_memo(&root_sk, addr),
            version_memo(&root_sk, addr, 1, "warn"),
        ],
    );
    assert_eq!(warn.version.version, 1);
    assert!(warn.version.blocks.is_empty());
}

#[test]
fn version_two_single_steps_reach_two() {
    let (root_sk, root_pk) = keypair();
    let addr = "zkv1test1";
    let res = replay_confirmed(
        addr,
        &root_pk,
        vec![
            init_memo(&root_sk, addr),
            version_memo(&root_sk, addr, 1, "warn"),
            version_memo(&root_sk, addr, 2, "blockread"),
        ],
    );
    assert_eq!(res.version.version, 2);
    assert!(res.version.blocks.contains(BlockCap::Read));
}

#[test]
fn version_multi_step_jump_dropped() {
    let (root_sk, root_pk) = keypair();
    let addr = "zkv1test1";
    let res = replay_audit(
        vec![
            confirmed_entry(init_memo(&root_sk, addr)),
            confirmed_entry(version_memo(&root_sk, addr, 2, "warn")),
        ],
        addr,
        &root_pk,
    );
    assert_eq!(
        last_outcome(&res),
        RowOutcome::Dropped(DropReason::VersionJumpTooLarge {
            current: 0,
            requested: 2
        }),
    );
    assert_eq!(res.version.version, 0);
}

#[test]
fn version_downgrade_jumps_freely() {
    let (root_sk, root_pk) = keypair();
    let addr = "zkv1test1";
    // Climb 0 -> 1 -> 2 one step at a time, then downgrade 2 -> 0 in one memo.
    let res = replay_confirmed(
        addr,
        &root_pk,
        vec![
            init_memo(&root_sk, addr),
            version_memo(&root_sk, addr, 1, "blockall"),
            version_memo(&root_sk, addr, 2, "blockall"),
            version_memo(&root_sk, addr, 0, "warn"),
        ],
    );
    assert_eq!(res.version.version, 0);
    assert!(res.version.blocks.is_empty());
}

#[test]
fn version_noop_dropped() {
    let (root_sk, root_pk) = keypair();
    let addr = "zkv1test1";
    // Genesis is 0; a VERSION 0 is a no-op.
    let res = replay_audit(
        vec![
            confirmed_entry(init_memo(&root_sk, addr)),
            confirmed_entry(version_memo(&root_sk, addr, 0, "warn")),
        ],
        addr,
        &root_pk,
    );
    assert_eq!(
        last_outcome(&res),
        RowOutcome::Dropped(DropReason::VersionNoOp)
    );
}

#[test]
fn version_non_owner_dropped() {
    let (root_sk, root_pk) = keypair();
    let (stranger_sk, _) = keypair_from(7);
    let addr = "zkv1test1";
    let res = replay_audit(
        vec![
            confirmed_entry(init_memo(&root_sk, addr)),
            confirmed_entry(version_memo(&stranger_sk, addr, 2, "blockall")),
        ],
        addr,
        &root_pk,
    );
    assert_eq!(
        last_outcome(&res),
        RowOutcome::Dropped(DropReason::NotOwner)
    );
    assert_eq!(res.version.version, 0);
}

#[test]
fn version_before_init_dropped() {
    let (root_sk, root_pk) = keypair();
    let addr = "zkv1test1";
    let res = replay_audit(
        vec![
            confirmed_entry(version_memo(&root_sk, addr, 2, "warn")),
            confirmed_entry(init_memo(&root_sk, addr)),
        ],
        addr,
        &root_pk,
    );
    // The VERSION (row 0) precedes INIT and is dropped as pre-INIT noise.
    assert_eq!(
        res.rows[0].outcome,
        RowOutcome::Dropped(DropReason::NotInitialized),
    );
    assert_eq!(res.version.version, 0);
}

#[test]
fn version_bad_flag_dropped() {
    let (root_sk, root_pk) = keypair();
    let addr = "zkv1test1";
    let res = replay_audit(
        vec![
            confirmed_entry(init_memo(&root_sk, addr)),
            confirmed_entry(version_memo(&root_sk, addr, 2, "blockfoo")),
        ],
        addr,
        &root_pk,
    );
    assert_eq!(
        last_outcome(&res),
        RowOutcome::Dropped(DropReason::VersionBadFlag)
    );
    assert_eq!(res.version.version, 0);
}

#[test]
fn version_non_numeric_dropped() {
    let (root_sk, root_pk) = keypair();
    let addr = "zkv1test1";
    let res = replay_audit(
        vec![
            confirmed_entry(init_memo(&root_sk, addr)),
            // Hand the version number slot a non-numeric token.
            confirmed_entry(op_memo(&root_sk, addr, Op::Version, "abc", Some("warn"))),
        ],
        addr,
        &root_pk,
    );
    assert_eq!(
        last_outcome(&res),
        RowOutcome::Dropped(DropReason::VersionNotNumeric),
    );
}

#[test]
fn version_pending_confers_nothing() {
    let (root_sk, root_pk) = keypair();
    let addr = "zkv1test1";
    let res = replay(
        vec![
            (init_memo(&root_sk, addr), WriteStatus::Confirmed),
            (
                version_memo(&root_sk, addr, 2, "blockall"),
                WriteStatus::Confirming {
                    done: 1,
                    required: 3,
                },
            ),
        ],
        addr,
        &root_pk,
        false,
    )
    .unwrap();
    // A confirming VERSION is recognized but confers no change yet.
    assert_eq!(res.version, VersionState::default());
}

#[test]
fn version_collapsed_form_parses_and_applies() {
    let (root_sk, root_pk) = keypair();
    let addr = "zkv1test1";
    // Build a normal VERSION memo, then collapse the newline to a space as a
    // whitespace-normalizing broadcaster would. It must still parse (the
    // header-only trailing-hex fallback) and apply through replay.
    let collapsed = version_memo(&root_sk, addr, 1, "blockwrite").replace('\n', " ");
    let cmd = parse_text_memo_detailed(&collapsed).expect("collapsed VERSION parses");
    assert_eq!(cmd.op, Op::Version);
    assert_eq!(cmd.key, "1");
    assert_eq!(cmd.value.as_deref(), Some("blockwrite"));

    let res = replay_confirmed(addr, &root_pk, vec![init_memo(&root_sk, addr), collapsed]);
    assert_eq!(res.version.version, 1);
    assert!(res.version.blocks.contains(BlockCap::Write));
}

#[test]
fn version_signature_binds_opcode() {
    let (root_sk, root_pk) = keypair();
    let addr = "zkv1test1";
    // Sign the payload for a *different* opcode, then frame it as VERSION.
    // The signed payload commits to the opcode string, so recovery yields a
    // different (non-owner) pubkey and the memo is dropped; the version is
    // left untouched.
    let payload = signed_payload(addr, Op::OwnerAdd, "2", Some("blockwrite"));
    let sig = sign_command(&root_sk, &payload);
    let forged = render_memo_text(Op::Version, "2", Some("blockwrite"), 0, &hex::encode(sig));
    let res = replay_audit(
        vec![
            confirmed_entry(init_memo(&root_sk, addr)),
            confirmed_entry(forged),
        ],
        addr,
        &root_pk,
    );
    assert!(matches!(
        last_outcome(&res),
        RowOutcome::Dropped(DropReason::NotOwner | DropReason::BadSignature),
    ));
    assert_eq!(res.version.version, 0);
}

// --- `zkv verify`: standalone signature + full verification -------------

/// An initialized database state with one confirmed key `k` (version 1),
/// owned by the root key; the fixture the full-verify tests check against.
fn initialized_state() -> (
    secp256k1::SecretKey,
    secp256k1::PublicKey,
    &'static str,
    ReplayResult,
) {
    let (sk, pk) = keypair();
    let addr = "zkv1test1";
    let state = replay(
        vec![
            (init_memo(&sk, addr), WriteStatus::Confirmed),
            (
                op_memo_v(&sk, addr, Op::Set, "k", Some("v1"), 0),
                WriteStatus::Confirmed,
            ),
        ],
        addr,
        &pk,
        false,
    )
    .unwrap();
    (sk, pk, addr, state)
}

#[test]
fn verify_signature_recovers_signer_without_state() {
    let (sk, pk) = keypair();
    let addr = "zkv1test1";
    let root = pubkey_bech32(&pk);
    let memo = op_memo(&sk, addr, Op::Set, "k", Some("v"));
    let v = verify_signature(&memo, addr, &root).expect("parses");
    assert!(v.signature_valid, "the message verifies");
    assert_eq!(v.signer.as_deref(), Some(root.as_str()));
    assert_eq!(v.is_root, Some(true));
    assert!(
        v.outcome.is_none(),
        "signature-only mode checks neither authorization nor ordering"
    );
    assert_eq!(v.op, Op::Set);
    assert_eq!(v.key, "k");
    assert_eq!(v.value.as_deref(), Some("v"));
}

#[test]
fn verify_signature_flags_non_root_signer() {
    // The signature is internally valid, but the signer is not the root key
    // the address derives, which is exactly the "verifies but unauthorized" case the
    // command warns about.
    let (sk, pk) = keypair_from(7);
    let (_root_sk, root_pk) = keypair_from(1);
    let addr = "zkv1test1";
    let root = pubkey_bech32(&root_pk);
    let signer = pubkey_bech32(&pk);
    let memo = op_memo(&sk, addr, Op::Set, "k", Some("v"));
    let v = verify_signature(&memo, addr, &root).expect("parses");
    assert!(v.signature_valid);
    assert_eq!(v.signer.as_deref(), Some(signer.as_str()));
    assert_eq!(v.is_root, Some(false));
}

#[test]
fn verify_signature_rejects_foreign_memo() {
    let err = verify_signature("hello, not a zkv memo", "zkv1test1", "zkvid1whatever").unwrap_err();
    assert_eq!(err, MemoReject::NotZkv);
}

#[test]
fn verify_memo_accepts_authorized_in_order_write() {
    let (sk, pk, addr, state) = initialized_state();
    let root = pubkey_bech32(&pk);
    // A fresh key written by the root owner at version 0: authorized and in
    // order, so it would be applied.
    let memo = op_memo(&sk, addr, Op::Set, "fresh", Some("x"));
    let v = verify_memo(&memo, addr, &root, &state).expect("parses");
    assert!(v.signature_valid);
    assert_eq!(v.is_root, Some(true));
    assert_eq!(v.outcome, Some(RowOutcome::Applied));
}

#[test]
fn verify_memo_rejects_unauthorized_signer() {
    let (_sk, pk, addr, state) = initialized_state();
    let root = pubkey_bech32(&pk);
    let (stranger_sk, _stranger_pk) = keypair_from(9);
    let memo = op_memo(&stranger_sk, addr, Op::Set, "fresh", Some("x"));
    let v = verify_memo(&memo, addr, &root, &state).expect("parses");
    assert!(v.signature_valid, "the signature itself is valid");
    assert_eq!(v.is_root, Some(false));
    assert_eq!(
        v.outcome,
        Some(RowOutcome::Dropped(DropReason::NoWriteAuthority))
    );
}

#[test]
fn verify_memo_rejects_stale_sequence() {
    let (sk, pk, addr, state) = initialized_state();
    let root = pubkey_bech32(&pk);
    // Re-broadcast the original create of `k` at version 0; `k` has
    // already advanced to version 1, so the ordering check rejects it.
    let stale = op_memo_v(&sk, addr, Op::Set, "k", Some("v1"), 0);
    let v = verify_memo(&stale, addr, &root, &state).expect("parses");
    assert!(v.signature_valid);
    assert_eq!(
        v.outcome,
        Some(RowOutcome::Dropped(DropReason::StaleVersion))
    );
}

#[test]
fn verify_handles_first_line_comment() {
    // A comment is folded into the signed domain, so the verifiers must
    // route through `payload_for` to recover the real signer (and not flag a
    // valid commented memo as a bad signature).
    let (sk, pk) = keypair();
    let addr = "zkv1test1";
    let root = pubkey_bech32(&pk);
    let memo = op_memo_commented(&sk, addr, Op::Set, "fresh", Some("x"), 0, " a note");

    let v = verify_signature(&memo, addr, &root).expect("parses");
    assert!(v.signature_valid, "comment-bearing signature verifies");
    assert_eq!(v.signer.as_deref(), Some(root.as_str()));
    assert_eq!(v.is_root, Some(true));

    // And the full check applies it as an authorized, in-order write. The
    // same root key signs the state fixture (both use `keypair()`), so
    // authorization holds.
    let (_sk2, _pk2, addr2, state) = initialized_state();
    let memo2 = op_memo_commented(&sk, addr2, Op::Set, "fresh", Some("x"), 0, " a note");
    let v2 = verify_memo(&memo2, addr2, &root, &state).expect("parses");
    assert!(v2.signature_valid);
    assert_eq!(v2.outcome, Some(RowOutcome::Applied));
}

#[test]
fn seq_in_window_is_the_single_replay_protection_rule() {
    // The shared version-CAS predicate used verbatim by the in-memory replay
    // (`classify_parsed`) and the snapshot promote (`apply_row`). Locking it
    // here guards the invariant that the two paths can't drift.
    let mut kv: BTreeMap<String, u64> = BTreeMap::new();
    let tgt_empty: BTreeMap<String, u64> = BTreeMap::new();

    // Absent entry reads as current = 0: the window is `0 ..= VERSION_WINDOW`.
    assert!(seq_in_window(Op::Set, "k", 0, &kv, &tgt_empty));
    assert!(seq_in_window(Op::Set, "k", VERSION_WINDOW, &kv, &tgt_empty));
    assert!(!seq_in_window(
        Op::Set,
        "k",
        VERSION_WINDOW + 1,
        &kv,
        &tgt_empty
    )); // desync

    // High-water at 5: below it is a stale replay; the window slides up with it.
    kv.insert("k".into(), 5);
    assert!(!seq_in_window(Op::Set, "k", 4, &kv, &tgt_empty)); // stale / lost CAS
    assert!(seq_in_window(Op::Set, "k", 5, &kv, &tgt_empty));
    assert!(seq_in_window(
        Op::Set,
        "k",
        5 + VERSION_WINDOW,
        &kv,
        &tgt_empty
    ));
    assert!(!seq_in_window(
        Op::Set,
        "k",
        6 + VERSION_WINDOW,
        &kv,
        &tgt_empty
    ));

    // Data ops key on `kv_versions`; management ops key on `target_versions`.
    let mut tgt: BTreeMap<String, u64> = BTreeMap::new();
    tgt.insert("k".into(), 100);
    assert!(seq_in_window(Op::Set, "k", 5, &kv, &tgt)); // data → kv (=5)
    assert!(!seq_in_window(Op::OwnerAdd, "k", 5, &kv, &tgt)); // mgmt → target (=100)
    assert!(seq_in_window(Op::OwnerAdd, "k", 100, &kv, &tgt));

    // Non-versioned ops (INIT / VERSION / FINALIZE) are always in window.
    assert!(seq_in_window(Op::Init, "k", 0, &kv, &tgt));
    assert!(seq_in_window(Op::Version, "k", u64::MAX, &kv, &tgt));
    assert!(seq_in_window(Op::Finalize, "", u64::MAX, &kv, &tgt));
}
