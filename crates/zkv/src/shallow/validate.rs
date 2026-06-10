//! The pure shallow validator: no I/O, no async.
//!
//! Takes the raw memo texts the network pipeline recovered from a block
//! window and classifies each one with the stateless protocol primitives
//! ([`payload_for`] + [`recover_signer`]), producing the per-key
//! last-write-wins view plus the trust-model warnings.
//!
//! This is deliberately **not** [`crate::protocol::replay_with_seed`]: with no
//! prior history there is no confirmed INIT (every data op would drop as
//! `NotInitialized`) and no per-entity high-water (any wire sequence outside
//! `[0, VERSION_WINDOW]` would drop as `StaleVersion`). Shallow validation
//! verifies each signature against the address-derived identity instead and
//! surfaces what it *cannot* check as [`ShallowWarning`]s.

use std::collections::{BTreeMap, BTreeSet};

use serde::{Deserialize, Serialize};
use zcash_primitives::transaction::TxId;

use crate::protocol::{
    parse_text_memo_detailed, payload_for, pubkey_bech32, recover_signer, MemoReject, Op,
};

/// One raw memo recovered from the chain window: position plus decrypted text.
/// The pipeline's hand-off into [`validate`].
#[derive(Clone, Debug)]
pub(crate) struct RawHit {
    pub height: u32,
    pub txid: [u8; 32],
    pub output_index: u32,
    pub text: String,
}

impl RawHit {
    fn txid_string(&self) -> String {
        TxId::from_bytes(self.txid).to_string()
    }
}

/// One validated data-op observation (`SET`/`SETL`/`DEL`) from the window.
#[derive(Clone, Debug, Serialize)]
pub struct ShallowUpdate {
    pub key: String,
    /// The written value; `None` for a `DEL`.
    pub value: Option<String>,
    /// The wire opcode, as its canonical string (`SET`/`SETL`/`DEL`).
    #[serde(serialize_with = "ser_op")]
    pub op: Op,
    pub height: u32,
    /// Display-order (big-endian) transaction id hex.
    pub txid: String,
    pub output_index: u32,
    /// The replay-protection sequence the writer signed over (the wire `[seq]`
    /// prefix). Exposed so callers can apply their own monotonic-seq policy;
    /// shallow itself cannot know the entity's true high-water.
    pub seq: u64,
    /// The recovered signer (canonical `zkvid1…`), or `None` when the
    /// signature did not recover.
    pub signer: Option<String>,
    /// Whether the signature is valid AND the signer is accepted (the
    /// address-derived root key, or one of the caller's extra signers). Only
    /// verified entries compete for the per-key winner.
    pub verified: bool,
    /// Depth from the tip the validator was given (`tip - height + 1`).
    pub confirmations: u32,
    /// The signed first-line comment, when the memo carried one.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub comment: Option<String>,
}

/// A trust-model caveat the shallow validator surfaced. None of these are
/// errors: the window is still reported, but the caller should know what
/// shallow could not check (a full sync resolves all of them).
#[derive(Clone, Debug, Serialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum ShallowWarning {
    /// A data op with a valid signature from a signer shallow does not accept:
    /// possibly a delegated writer (run a full sync to verify). The entry is
    /// still in `updates` with `verified: false`; it never wins a key.
    UnverifiedSigner {
        key: String,
        signer: String,
        height: u32,
        txid: String,
    },
    /// Two verified entries for one key whose chain order and sequence order
    /// disagree: the chain-order winner carries a *lower* sequence than another
    /// entry in the window. A rebroadcast-replay tell; with no high-water
    /// counter, shallow cannot prove which is stale. The chain-order winner is
    /// still reported.
    SeqOrderMismatch {
        key: String,
        chain_winner_seq: u64,
        max_seq: u64,
    },
    /// A management / FINALIZE / VERSION memo was seen in the window. Shallow
    /// cannot safely apply registry or lifecycle changes (it has no confirmed
    /// base state to apply them to), so authority may have changed: full sync
    /// to be sure.
    ManagementSeen {
        #[serde(serialize_with = "ser_op")]
        op: Op,
        height: u32,
    },
    /// A `ZKV0`-prefixed memo that did not parse (malformed framing, or a
    /// newer protocol version than this build understands).
    Malformed {
        height: u32,
        txid: String,
        detail: String,
    },
}

/// An INIT memo observed while scanning, with the only verdict shallow can
/// reach on it: whether its signature recovers to the address-derived root key
/// (the first-valid-wins rule's only input; the wire address echo is advisory).
#[derive(Clone, Debug)]
pub(crate) struct InitObservation {
    pub height: u32,
    pub txid: [u8; 32],
    pub root_signed: bool,
    /// The raw on-chain memo text (the `ZKV0 INIT …` header plus its
    /// signature line, verbatim), so a verified anchor can be re-checked or
    /// displayed without re-fetching it from the chain.
    pub memo: String,
}

/// The validator's output: every classified data op in chain order, the
/// per-key winners among verified entries, the warnings, and any INITs seen.
#[derive(Debug, Default)]
pub(crate) struct Validated {
    pub updates: Vec<ShallowUpdate>,
    pub latest: BTreeMap<String, ShallowUpdate>,
    pub warnings: Vec<ShallowWarning>,
    pub inits: Vec<InitObservation>,
}

/// Classify a window of raw hits against a database identity.
///
/// `receiver` is the database's [`crate::protocol::receiver_domain`];
/// `root_hex` the canonical `zkvid1…` of its UFVK-derived root key;
/// `extra_signers` additional accepted `zkvid1…` signers (e.g. seeded from a
/// full snapshot's auth registry). `tip`/`min_confirmations` apply the same
/// depth filter the full read path uses.
pub(crate) fn validate(
    mut hits: Vec<RawHit>,
    receiver: &str,
    root_hex: &str,
    extra_signers: &BTreeSet<String>,
    tip: u32,
    min_confirmations: u32,
) -> Validated {
    // The same total order the full read path replays in.
    hits.sort_by(|a, b| {
        (a.height, a.txid, a.output_index).cmp(&(b.height, b.txid, b.output_index))
    });

    let mut out = Validated::default();
    for hit in &hits {
        let confirmations = tip.saturating_sub(hit.height) + 1;
        if confirmations < min_confirmations {
            continue;
        }
        let cmd = match parse_text_memo_detailed(&hit.text) {
            Ok(cmd) => cmd,
            // Foreign traffic: not zkv, not worth a warning.
            Err(MemoReject::NotZkv) => continue,
            Err(MemoReject::Malformed(fmt)) => {
                out.warnings.push(ShallowWarning::Malformed {
                    height: hit.height,
                    txid: hit.txid_string(),
                    detail: fmt.to_string(),
                });
                continue;
            }
            Err(MemoReject::UnsupportedVersion(v)) => {
                out.warnings.push(ShallowWarning::Malformed {
                    height: hit.height,
                    txid: hit.txid_string(),
                    detail: format!("unsupported zkv protocol version {v}"),
                });
                continue;
            }
        };
        // Recover the signer over the receiver-bound payload (the wire seq is
        // folded in by `payload_for`, exactly as the full replay does).
        let signer =
            recover_signer(&payload_for(receiver, &cmd), &cmd.sig_hex).map(|pk| pubkey_bech32(&pk));

        if cmd.op.is_data() {
            let verified = signer
                .as_deref()
                .is_some_and(|s| s == root_hex || extra_signers.contains(s));
            if let (false, Some(s)) = (verified, signer.as_deref()) {
                out.warnings.push(ShallowWarning::UnverifiedSigner {
                    key: cmd.key.clone(),
                    signer: s.to_owned(),
                    height: hit.height,
                    txid: hit.txid_string(),
                });
            }
            out.updates.push(ShallowUpdate {
                key: cmd.key,
                value: cmd.value,
                op: cmd.op,
                height: hit.height,
                txid: hit.txid_string(),
                output_index: hit.output_index,
                seq: cmd.seq,
                signer,
                verified,
                confirmations,
                comment: cmd.comment,
            });
        } else if matches!(cmd.op, Op::Init) {
            out.inits.push(InitObservation {
                height: hit.height,
                txid: hit.txid,
                root_signed: signer.as_deref() == Some(root_hex),
                memo: hit.text.clone(),
            });
        } else {
            out.warnings.push(ShallowWarning::ManagementSeen {
                op: cmd.op,
                height: hit.height,
            });
        }
    }

    // Per-key winner: last write wins, in chain order, among verified entries
    // only (a verified DEL wins with `value: None`, "confirmed deleted").
    for u in &out.updates {
        if u.verified {
            out.latest.insert(u.key.clone(), u.clone());
        }
    }

    // Rebroadcast tell: a chain-order winner carrying a lower sequence than
    // another verified entry for the same key in this window.
    let mut max_seq: BTreeMap<&str, u64> = BTreeMap::new();
    for u in out.updates.iter().filter(|u| u.verified) {
        let e = max_seq.entry(u.key.as_str()).or_insert(u.seq);
        *e = (*e).max(u.seq);
    }
    for (key, winner) in &out.latest {
        let max = max_seq.get(key.as_str()).copied().unwrap_or(winner.seq);
        if winner.seq < max {
            out.warnings.push(ShallowWarning::SeqOrderMismatch {
                key: key.clone(),
                chain_winner_seq: winner.seq,
                max_seq: max,
            });
        }
    }

    out
}

/// A resumable position in the chain for the follow/poll loop.
///
/// `height` is the highest block already processed; `recent` carries the ids
/// (`(height, txid, output_index)`) of updates within the reorg re-fetch
/// margin, so a poll that re-reads those blocks doesn't report them twice.
/// `Serialize`/`Deserialize` so library consumers can persist it across
/// process restarts.
#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct ShallowCursor {
    pub height: u32,
    pub recent: Vec<(u32, String, u32)>,
}

impl ShallowCursor {
    /// Build a cursor at `tip` from a window's updates: remember every update
    /// within `margin` blocks of the tip for poll-time dedup.
    pub(crate) fn at_tip(tip: u32, updates: &[ShallowUpdate], margin: u32) -> Self {
        ShallowCursor {
            height: tip,
            recent: updates
                .iter()
                .filter(|u| u.height > tip.saturating_sub(margin))
                .map(|u| (u.height, u.txid.clone(), u.output_index))
                .collect(),
        }
    }
}

/// The block range a poll from `cursor_height` to `tip` must fetch: the new
/// blocks plus `margin` already-seen blocks behind the cursor (reorg
/// re-check). `None` when the tip hasn't advanced.
pub(crate) fn poll_range(cursor_height: u32, tip: u32, margin: u32) -> Option<(u32, u32)> {
    (tip > cursor_height).then(|| (cursor_height.saturating_sub(margin).saturating_add(1), tip))
}

/// Drop updates already reported by a previous poll (their id is in the
/// cursor's `recent` list).
pub(crate) fn dedup_updates(
    updates: Vec<ShallowUpdate>,
    seen: &[(u32, String, u32)],
) -> Vec<ShallowUpdate> {
    updates
        .into_iter()
        .filter(|u| {
            !seen
                .iter()
                .any(|(h, t, i)| *h == u.height && *t == u.txid && *i == u.output_index)
        })
        .collect()
}

/// Split `[lo, hi]` (inclusive) into chunks walked **newest first**, starting
/// small and doubling up to `max` (the order `find` walks). A search that
/// resolves near the tip then only streams a few dozen compact blocks instead
/// of a full fixed-size chunk; a deep search still converges to `max`-sized
/// requests. Empty when `lo > hi`.
pub(crate) fn chunks_desc_progressive(lo: u32, hi: u32, first: u32, max: u32) -> Vec<(u32, u32)> {
    let mut out = Vec::new();
    let mut top = hi;
    let mut size = first.max(1);
    while top >= lo {
        let bottom = top.saturating_sub(size - 1).max(lo);
        out.push((bottom, top));
        if bottom == lo || bottom == 0 {
            break;
        }
        top = bottom - 1;
        size = size.saturating_mul(2).min(max.max(1));
    }
    out
}

/// Split `[lo, hi]` (inclusive) into chunks walked **oldest first**, starting
/// small and doubling up to `max` (the order the INIT walk uses). The genesis
/// INIT sits at/just past the birthday, so a small first chunk usually finds
/// it without streaming a full `max`-block window. Empty when `lo > hi`.
pub(crate) fn chunks_asc_progressive(lo: u32, hi: u32, first: u32, max: u32) -> Vec<(u32, u32)> {
    let mut out = Vec::new();
    let mut bottom = lo;
    let mut size = first.max(1);
    while bottom <= hi {
        let top = bottom.saturating_add(size - 1).min(hi);
        out.push((bottom, top));
        if top == hi || top == u32::MAX {
            break;
        }
        bottom = top + 1;
        size = size.saturating_mul(2).min(max.max(1));
    }
    out
}

/// Split `[lo, hi]` (inclusive) into chunks of at most `chunk` blocks,
/// oldest first (the order `scan` uses). Empty when `lo > hi`.
pub(crate) fn chunks_asc(lo: u32, hi: u32, chunk: u32) -> Vec<(u32, u32)> {
    let chunk = chunk.max(1);
    let mut out = Vec::new();
    let mut start = lo;
    while start <= hi {
        let end = start.saturating_add(chunk - 1).min(hi);
        out.push((start, end));
        if end == u32::MAX {
            break;
        }
        start = end + 1;
    }
    out
}

/// Pure early-stop bookkeeping for the backward `find` walk.
///
/// The walk processes blocks newest-first, one height at a time, and asks the
/// caller's predicate after each height whether the search is satisfied. The
/// first satisfied height anchors a **grace floor** `height - grace`: the walk
/// keeps processing down to that floor (so keys updated slightly before the
/// match, e.g. siblings of a glob written a few blocks apart, are still
/// caught), then stops. `grace = 0` stops immediately, which is right for
/// exact-key searches (nothing older can beat last-write-wins or add a
/// requested key).
#[derive(Clone, Copy, Debug)]
pub(crate) struct FindStepper {
    grace: u32,
    stop_below: Option<u32>,
}

impl FindStepper {
    pub(crate) fn new(grace: u32) -> Self {
        FindStepper {
            grace,
            stop_below: None,
        }
    }

    /// Should `height` still be processed, or is the walk complete?
    pub(crate) fn wants(&self, height: u32) -> bool {
        self.stop_below.is_none_or(|sb| height >= sb)
    }

    /// Whether a whole chunk topping out at `hi` is already below the grace
    /// floor (skip fetching it entirely).
    pub(crate) fn skip_chunk(&self, hi: u32) -> bool {
        self.stop_below.is_some_and(|sb| sb > hi)
    }

    /// Record that, after processing `height`, the caller's predicate reports
    /// `satisfied`. Only the first satisfied height anchors the grace floor.
    pub(crate) fn observe(&mut self, height: u32, satisfied: bool) {
        if self.stop_below.is_none() && satisfied {
            self.stop_below = Some(height.saturating_sub(self.grace));
        }
    }

    /// The lowest height the walk guarantees it covered: the grace floor when
    /// the search was satisfied, else the full-search `floor`.
    pub(crate) fn covered_from(&self, floor: u32) -> u32 {
        self.stop_below.map_or(floor, |sb| sb.max(floor))
    }
}

fn ser_op<S: serde::Serializer>(op: &Op, s: S) -> Result<S::Ok, S::Error> {
    s.serialize_str(op.as_str())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::protocol::{bind_comment, sign_command, signed_payload, signing_domain};
    use crate::shallow::testutil::{fixture, signed_memo};

    fn hit(height: u32, txid_byte: u8, output_index: u32, text: String) -> RawHit {
        RawHit {
            height,
            txid: [txid_byte; 32],
            output_index,
            text,
        }
    }

    const TIP: u32 = 1_000;
    const NO_EXTRA: &BTreeSet<String> = &BTreeSet::new();

    #[test]
    fn last_write_wins_in_chain_order() {
        let (receiver, root, sk) = fixture();
        let hits = vec![
            hit(
                900,
                1,
                0,
                signed_memo(&receiver, &sk, Op::Set, "k", Some("old"), 5),
            ),
            hit(
                950,
                2,
                0,
                signed_memo(&receiver, &sk, Op::Set, "k", Some("new"), 6),
            ),
        ];
        let v = validate(hits, &receiver, &root, NO_EXTRA, TIP, 1);
        assert_eq!(v.updates.len(), 2);
        assert!(v.updates.iter().all(|u| u.verified));
        let w = v.latest.get("k").expect("winner");
        assert_eq!(w.value.as_deref(), Some("new"));
        assert_eq!(w.height, 950);
        assert!(v.warnings.is_empty(), "{:?}", v.warnings);
    }

    #[test]
    fn same_block_tiebreak_by_txid_then_output_index() {
        let (receiver, root, sk) = fixture();
        // Same height; the higher txid (then higher output index) wins. Feed
        // them out of order to prove the validator sorts.
        let hits = vec![
            hit(
                900,
                9,
                0,
                signed_memo(&receiver, &sk, Op::Set, "k", Some("by-txid"), 2),
            ),
            hit(
                900,
                1,
                1,
                signed_memo(&receiver, &sk, Op::Set, "k", Some("low"), 1),
            ),
            hit(
                900,
                1,
                0,
                signed_memo(&receiver, &sk, Op::Set, "k", Some("lower"), 0),
            ),
        ];
        let v = validate(hits, &receiver, &root, NO_EXTRA, TIP, 1);
        assert_eq!(
            v.latest.get("k").and_then(|u| u.value.as_deref()),
            Some("by-txid")
        );
        // Chain order ascending in `updates`.
        let order: Vec<_> = v.updates.iter().map(|u| u.value.as_deref()).collect();
        assert_eq!(order, vec![Some("lower"), Some("low"), Some("by-txid")]);
    }

    #[test]
    fn verified_del_wins_as_tombstone() {
        let (receiver, root, sk) = fixture();
        let hits = vec![
            hit(
                900,
                1,
                0,
                signed_memo(&receiver, &sk, Op::Set, "k", Some("v"), 0),
            ),
            hit(
                950,
                2,
                0,
                signed_memo(&receiver, &sk, Op::Del, "k", None, 1),
            ),
        ];
        let v = validate(hits, &receiver, &root, NO_EXTRA, TIP, 1);
        let w = v.latest.get("k").expect("winner");
        assert_eq!(w.op, Op::Del);
        assert_eq!(w.value, None);
    }

    #[test]
    fn non_root_signer_is_unverified_and_never_wins() {
        let (receiver, root, root_sk) = fixture();
        let other_sk = secp256k1::SecretKey::from_slice(&[7u8; 32]).expect("sk");
        let hits = vec![
            hit(
                900,
                1,
                0,
                signed_memo(&receiver, &root_sk, Op::Set, "k", Some("root"), 0),
            ),
            hit(
                950,
                2,
                0,
                signed_memo(&receiver, &other_sk, Op::Set, "k", Some("other"), 1),
            ),
        ];
        let v = validate(hits, &receiver, &root, NO_EXTRA, TIP, 1);
        // The non-root write is surfaced but unverified, with a warning.
        let other = v.updates.iter().find(|u| u.height == 950).expect("entry");
        assert!(!other.verified);
        assert!(other.signer.is_some(), "valid signature still recovers");
        assert!(matches!(
            &v.warnings[..],
            [ShallowWarning::UnverifiedSigner { key, .. }] if key == "k"
        ));
        // The root-signed (older) write still wins.
        assert_eq!(
            v.latest.get("k").and_then(|u| u.value.as_deref()),
            Some("root")
        );
    }

    #[test]
    fn extra_signers_are_accepted() {
        let (receiver, root, _) = fixture();
        let other_sk = secp256k1::SecretKey::from_slice(&[7u8; 32]).expect("sk");
        let other_id = pubkey_bech32(&crate::protocol::pubkey_of(&other_sk));
        let extra: BTreeSet<String> = [other_id].into_iter().collect();
        let hits = vec![hit(
            900,
            1,
            0,
            signed_memo(&receiver, &other_sk, Op::Set, "k", Some("delegated"), 0),
        )];
        let v = validate(hits, &receiver, &root, &extra, TIP, 1);
        assert!(v.updates[0].verified);
        assert_eq!(
            v.latest.get("k").and_then(|u| u.value.as_deref()),
            Some("delegated")
        );
        assert!(v.warnings.is_empty());
    }

    #[test]
    fn seq_order_mismatch_flags_rebroadcast_tell() {
        let (receiver, root, sk) = fixture();
        // The chain-order winner (newer block) carries a LOWER seq than an
        // earlier entry: a rebroadcast of an old memo would look like this.
        let hits = vec![
            hit(
                900,
                1,
                0,
                signed_memo(&receiver, &sk, Op::Set, "k", Some("fresh"), 9),
            ),
            hit(
                950,
                2,
                0,
                signed_memo(&receiver, &sk, Op::Set, "k", Some("replayed"), 3),
            ),
        ];
        let v = validate(hits, &receiver, &root, NO_EXTRA, TIP, 1);
        // Chain order still wins (shallow cannot prove staleness)...
        assert_eq!(
            v.latest.get("k").and_then(|u| u.value.as_deref()),
            Some("replayed")
        );
        // ...but the disagreement is flagged.
        assert!(v.warnings.iter().any(|w| matches!(
            w,
            ShallowWarning::SeqOrderMismatch { key, chain_winner_seq: 3, max_seq: 9 } if key == "k"
        )));
    }

    #[test]
    fn confirmation_depth_filter() {
        let (receiver, root, sk) = fixture();
        let hits = vec![
            // tip - 998 + 1 = 3 confirmations: included at min_confs 3.
            hit(
                998,
                1,
                0,
                signed_memo(&receiver, &sk, Op::Set, "deep", Some("v"), 0),
            ),
            // tip - 999 + 1 = 2 confirmations: excluded at min_confs 3.
            hit(
                999,
                2,
                0,
                signed_memo(&receiver, &sk, Op::Set, "shallow", Some("v"), 0),
            ),
        ];
        let v = validate(hits, &receiver, &root, NO_EXTRA, TIP, 3);
        assert!(v.latest.contains_key("deep"));
        assert!(!v.latest.contains_key("shallow"));
        assert_eq!(v.updates.len(), 1);
        assert_eq!(v.updates[0].confirmations, 3);
    }

    #[test]
    fn foreign_memos_skip_silently_and_garbage_warns() {
        let (receiver, root, _) = fixture();
        let hits = vec![
            hit(900, 1, 0, "thanks for the coffee".to_owned()),
            hit(901, 2, 0, "ZKV0 BOGUS nonsense".to_owned()),
        ];
        let v = validate(hits, &receiver, &root, NO_EXTRA, TIP, 1);
        assert!(v.updates.is_empty());
        assert!(matches!(
            &v.warnings[..],
            [ShallowWarning::Malformed { height: 901, .. }]
        ));
    }

    #[test]
    fn init_predicate_accepts_root_and_rejects_others() {
        let (receiver, root, root_sk) = fixture();
        let other_sk = secp256k1::SecretKey::from_slice(&[7u8; 32]).expect("sk");
        let hits = vec![
            hit(
                900,
                1,
                0,
                signed_memo(&receiver, &root_sk, Op::Init, "zkvtest1echo", None, 0),
            ),
            hit(
                901,
                2,
                0,
                signed_memo(&receiver, &other_sk, Op::Init, "zkvtest1echo", None, 0),
            ),
            // A SET is never an INIT observation.
            hit(
                902,
                3,
                0,
                signed_memo(&receiver, &root_sk, Op::Set, "k", Some("v"), 0),
            ),
        ];
        let v = validate(hits, &receiver, &root, NO_EXTRA, TIP, 1);
        assert_eq!(v.inits.len(), 2);
        assert!(v.inits[0].root_signed);
        assert!(!v.inits[1].root_signed, "forged INIT must not verify");
        assert_eq!(v.updates.len(), 1);
    }

    #[test]
    fn management_ops_surface_as_warnings_not_updates() {
        let (receiver, root, sk) = fixture();
        let target = "zkvid1qfakefakefake";
        let hits = vec![hit(
            900,
            1,
            0,
            signed_memo(&receiver, &sk, Op::OwnerAdd, target, None, 0),
        )];
        let v = validate(hits, &receiver, &root, NO_EXTRA, TIP, 1);
        assert!(v.updates.is_empty());
        assert!(matches!(
            &v.warnings[..],
            [ShallowWarning::ManagementSeen {
                op: Op::OwnerAdd,
                height: 900
            }]
        ));
    }

    #[test]
    fn signed_comment_round_trips_and_tampered_comment_unverifies() {
        let (receiver, root, sk) = fixture();
        // Sign WITH the comment folded into the domain, exactly as a writer would.
        let domain = bind_comment(&signing_domain(&receiver, Op::Set, 0), Some("hello"));
        let payload = signed_payload(&domain, Op::Set, "k", Some("v"));
        let sig = sign_command(&sk, &payload);
        let good = crate::protocol::render_memo_with_comment(
            Op::Set,
            "k",
            Some("v"),
            0,
            &hex::encode(sig),
            Some("hello"),
        );
        let tampered = crate::protocol::render_memo_with_comment(
            Op::Set,
            "k",
            Some("v"),
            0,
            &hex::encode(sig),
            Some("evil"),
        );
        let hits = vec![hit(900, 1, 0, good), hit(901, 2, 0, tampered)];
        let v = validate(hits, &receiver, &root, NO_EXTRA, TIP, 1);
        let good_u = v.updates.iter().find(|u| u.height == 900).expect("good");
        assert!(good_u.verified);
        assert_eq!(good_u.comment.as_deref(), Some("hello"));
        let bad_u = v.updates.iter().find(|u| u.height == 901).expect("bad");
        assert!(
            !bad_u.verified,
            "a tampered comment recovers a different key"
        );
    }

    #[test]
    fn chunk_math() {
        assert_eq!(chunks_asc(10, 10, 5), vec![(10, 10)]);
        assert_eq!(chunks_asc(1, 10, 4), vec![(1, 4), (5, 8), (9, 10)]);
        assert_eq!(chunks_asc(10, 9, 4), Vec::<(u32, u32)>::new());
        // Degenerate chunk size is clamped, not an infinite loop.
        assert_eq!(chunks_asc(1, 3, 0), vec![(1, 1), (2, 2), (3, 3)]);
    }

    #[test]
    fn progressive_chunks_double_newest_first() {
        // 4-block first chunk, doubling, capped at 16; covers [1, 100] with
        // contiguous, non-overlapping, descending ranges.
        let chunks = chunks_desc_progressive(1, 100, 4, 16);
        assert_eq!(chunks[0], (97, 100), "first chunk is small and newest");
        assert_eq!(chunks[1], (89, 96), "second doubles");
        assert_eq!(chunks[2], (73, 88), "third doubles to the cap");
        assert_eq!(chunks[3], (57, 72), "later chunks stay at the cap");
        // Full, gap-free coverage down to the floor.
        assert_eq!(chunks.last().unwrap().0, 1);
        for w in chunks.windows(2) {
            assert_eq!(w[0].0 - 1, w[1].1, "contiguous descending");
        }

        // Window smaller than the first chunk: one clamped chunk.
        assert_eq!(chunks_desc_progressive(90, 100, 48, 1000), vec![(90, 100)]);
        // Empty range.
        assert!(chunks_desc_progressive(10, 9, 4, 16).is_empty());
        // Degenerate sizes are clamped, not an infinite loop.
        assert_eq!(
            chunks_desc_progressive(1, 3, 0, 0),
            vec![(3, 3), (2, 2), (1, 1)]
        );
    }

    #[test]
    fn progressive_chunks_oldest_first_double() {
        // 4-block first chunk, doubling, capped at 16; covers [1, 100] from
        // the bottom up with contiguous, non-overlapping ranges.
        let chunks = chunks_asc_progressive(1, 100, 4, 16);
        assert_eq!(chunks[0], (1, 4), "first chunk is small and oldest");
        assert_eq!(chunks[1], (5, 12), "second doubles");
        assert_eq!(chunks[2], (13, 28), "third doubles to the cap");
        assert_eq!(chunks[3], (29, 44), "later chunks stay at the cap");
        assert_eq!(chunks.last().unwrap().1, 100, "covers up to the top");
        for w in chunks.windows(2) {
            assert_eq!(w[0].1 + 1, w[1].0, "contiguous ascending");
        }

        // Window smaller than the first chunk: one clamped chunk.
        assert_eq!(chunks_asc_progressive(90, 100, 48, 1000), vec![(90, 100)]);
        // Empty range.
        assert!(chunks_asc_progressive(10, 9, 4, 16).is_empty());
        // Degenerate sizes are clamped, not an infinite loop.
        assert_eq!(
            chunks_asc_progressive(1, 3, 0, 0),
            vec![(1, 1), (2, 2), (3, 3)]
        );
    }

    #[test]
    fn find_stepper_exact_stops_immediately() {
        // grace 0: the height that satisfies the search is the last processed.
        let mut s = FindStepper::new(0);
        assert!(s.wants(1000));
        s.observe(1000, false);
        assert!(s.wants(999));
        s.observe(999, true);
        assert!(s.wants(999), "the satisfying height itself stays covered");
        assert!(!s.wants(998));
        assert_eq!(s.covered_from(1), 999);
    }

    #[test]
    fn find_stepper_grace_keeps_walking() {
        // grace 48: keep processing 48 blocks below the first match.
        let mut s = FindStepper::new(48);
        s.observe(1000, true);
        assert!(s.wants(952), "within the grace window");
        assert!(!s.wants(951), "below the grace floor");
        // A later satisfied observation must not move the floor.
        s.observe(960, true);
        assert!(!s.wants(951));
        assert_eq!(s.covered_from(1), 952);
    }

    #[test]
    fn find_stepper_chunk_skip_and_floor_clamp() {
        let mut s = FindStepper::new(10);
        assert!(!s.skip_chunk(500), "no match yet: every chunk is wanted");
        s.observe(600, true);
        // Grace floor is 590: a chunk topping out below it is skippable.
        assert!(s.skip_chunk(589));
        assert!(!s.skip_chunk(590));
        // Never satisfied → covered down to the search floor.
        assert_eq!(FindStepper::new(10).covered_from(123), 123);
        // Floor clamps the grace floor (search bounded by birthday/max_depth).
        assert_eq!(s.covered_from(595), 595);
    }

    #[test]
    fn poll_range_and_dedup() {
        // Tip hasn't moved: nothing to fetch.
        assert_eq!(poll_range(100, 100, 10), None);
        assert_eq!(poll_range(100, 90, 10), None);
        // Tip advanced: re-fetch the margin behind the cursor too.
        assert_eq!(poll_range(100, 105, 10), Some((91, 105)));

        let (receiver, root, sk) = fixture();
        let hits = vec![
            hit(
                95,
                1,
                0,
                signed_memo(&receiver, &sk, Op::Set, "a", Some("1"), 0),
            ),
            hit(
                105,
                2,
                0,
                signed_memo(&receiver, &sk, Op::Set, "b", Some("2"), 0),
            ),
        ];
        let v = validate(hits, &receiver, &root, NO_EXTRA, 105, 1);
        // The previous cursor saw the height-95 update.
        let seen = vec![(95u32, v.updates[0].txid.clone(), 0u32)];
        let fresh = dedup_updates(v.updates.clone(), &seen);
        assert_eq!(fresh.len(), 1);
        assert_eq!(fresh[0].key, "b");

        // Cursor construction: only updates within the margin are remembered.
        let cursor = ShallowCursor::at_tip(105, &v.updates, 10);
        assert_eq!(cursor.height, 105);
        assert_eq!(cursor.recent.len(), 1, "95 is outside the 10-block margin");
        assert_eq!(cursor.recent[0].0, 105);
    }

    #[test]
    fn updates_serialize_with_op_string() {
        let (receiver, root, sk) = fixture();
        let hits = vec![hit(
            900,
            1,
            0,
            signed_memo(&receiver, &sk, Op::Set, "k", Some("v"), 0),
        )];
        let v = validate(hits, &receiver, &root, NO_EXTRA, TIP, 1);
        let json = serde_json::to_value(&v.updates[0]).expect("serialize");
        assert_eq!(json["op"], "SET");
        assert_eq!(json["verified"], true);
    }
}
