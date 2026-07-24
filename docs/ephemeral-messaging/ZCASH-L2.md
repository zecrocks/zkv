# Zcash Layer-2 Approaches

**Status:** design survey + recommendation (v0.1, 2026-07)
**Companion to:** [`ARCHITECTURE.md`](./ARCHITECTURE.md) (ephemeral messaging).
This document answers two questions: *what are the credible ways to build an
L2 on Zcash at all*, and *which (if any) should the messaging system use*.

---

## 1. The constraint that shapes everything

Zcash L1 is **not programmable** in the Ethereum sense. What it actually
offers a would-be L2 (verified against the current `librustzcash` tree):

**Available today**

- Bitcoin-derived transparent script: P2PKH, P2SH (arbitrary redeem scripts,
  revealed on spend), multisig incl. ZIP-48 descriptor multisig, and
  **OP_RETURN null-data outputs of ≤80 bytes** (`TransparentBuilder::
  add_null_data_output`). No covenants, no introspection, no fraud-proof
  windows enforceable by script.
- Shielded pools (Sapling / Orchard / Ironwood since NU6.3) with a **512-byte
  encrypted memo** per output (511 usable via `Memo::Arbitrary`). Memos are
  private to viewing-key holders — a *selective-audience* data channel, which
  OP_RETURN is not.
- **FROST** (Zcash-ecosystem rerandomized threshold Schnorr/RedDSA): a
  validator set can jointly control a *shielded* spending key, not just a
  transparent multisig. This is the single most underrated L2 building block
  Zcash has — threshold custody that doesn't dox the vault.
- ZIP-317 deterministic fees; 75-second blocks; PoW (Equihash) without
  protocol finality today.
- zkv (this repo): a proven pattern for **signed, replay-protected,
  sequence-versioned data channels inside memos**, readable by anyone with a
  viewing key, with reader-side authorization replay.

**Explicitly absent** (so an L2 cannot lean on them yet): consensus-level
verification of foreign proofs, ZSA/issued assets, ZIP-231 memo bundles,
covenants, and finality (Crosslink-style PoS finality is a roadmap item, not
a shipped one; NU7 is scaffolding).

Consequence: **Zcash cannot *enforce* an L2's state validity or custody
today.** Any near-term "L2" is really a sovereign system that *borrows*
specific properties from L1 — ordering/timestamping, data commitment,
equivocation-proofing, and (via FROST) distributed custody — rather than
inheriting full security the way an Ethereum rollup does. Honesty about this
is the start of good design: pick which properties you actually need, borrow
those, and don't pretend to rollup-grade trust minimization you don't have.

## 2. What L1 anchoring actually buys

Writing a commitment `H(state)` into a Zcash transaction (memo or OP_RETURN)
gives an off-chain system four things:

1. **Timestamping** — the state existed by block *h*.
2. **Unique ordering** — if commitments are written from a single
   sequence-protected channel (a zkv database is exactly this), there is one
   canonical chain of commitments; an operator cannot maintain two histories
   ("split view") without producing visible conflicting writes.
3. **Fork-choice pinning** — L2 nodes reject any L2 history that doesn't
   match the anchored commitment chain: long-range attacks, key-theft
   rewrites, and quiet rollbacks become detectable by every client.
4. **Censorship escape hatch** — anyone can pay L1 fees to force a small
   piece of data into the anchor channel (e.g. a forced-inclusion request or
   fraud accusation), which honest L2 nodes must then act on.

Anchoring does **not** buy: validity of the anchored state, data
availability of what's behind the hash, or custody of bridged funds. Those
must come from the L2's own design (proofs, DA layer, threshold custody).

**Anchor channel design (concrete):** use a zkv database owned by the L2
operator set — `anchor/<epoch> = H(state_root) || da_hint`, written under a
FROST-threshold root signer, with zkv's tombstone-surviving sequence numbers
preventing replay/rewind, and `shallow` reads giving any client a
seconds-fast, wallet-free anchor check. Prefer shielded memos over OP_RETURN:
same chain, 6× the payload, fee-equivalent, and the anchor stream's *content*
is visible only to holders of the (published) viewing key rather than to
every chain indexer — publishing the UFVK makes it public *on purpose*, which
also means you could keep pre-launch epochs private and reveal later.

---

## 3. The design space, ranked

### A. Checkpointed BFT sidechain with FROST vault — *buildable now*

- **Consensus:** its own proof-of-authority/proof-of-stake BFT set
  (CometBFT/Malachite-class, sub-second finality), validators bonded in ZEC.
- **Anchoring:** epoch state roots into the zkv anchor channel (§2).
- **Bridge:** deposits = send ZEC to a FROST-threshold *shielded* vault
  controlled by the validator set; withdrawals = threshold-signed payouts.
  Trust: honest threshold (e.g. 67-of-100) for custody; anchoring makes
  stealing-by-history-rewrite impossible, but a colluding threshold can still
  run with the vault. Slashing = seized bonds + social/legal layer.
- **Verdict:** the pragmatic default. Equivalent in trust class to Liquid or
  early sidechains, but with two Zcash-specific upgrades: the vault is
  *shielded* (observers can't even enumerate it) and the anchor channel is
  *sequence-protected* (operator equivocation is cryptographically evident,
  not merely reputationally discouraged).

### B. Validity-proved sovereign chain ("Zcash validium") — *buildable soon, more work*

Take (A) and make state transitions carry succinct proofs (halo2 — reuse the
Zcash proving stack; the ecosystem's zk competence is the point). Clients and
validators verify every epoch proof; the anchor commits to `(state_root,
proof_hash)`. Data availability comes from the validator set (DAC
signatures in the anchor) or an external DA network if one is acceptable.

- Removes: trust in operators for *state correctness* (no invalid balance
  updates, no forged identity-log entries — even a malicious quorum can only
  censor or halt, not lie).
- Keeps: honest-threshold trust for custody (L1 still can't verify the proof,
  so withdrawals are still FROST-gated) and for DA.
- **Verdict:** the right *end-state for a self-sovereign Zcash L2* under
  current L1 rules; do it as v2 of (A) — same anchor channel, same vault,
  proofs added — rather than as a separate system.

### C. Enshrined L2 via consensus change — *the lobbying track*

The only path to Ethereum-grade L2 trust: a ZIP adding one narrow consensus
feature. Candidates, smallest first:

1. **Proof-verification opcode / precompile**: consensus verifies a halo2
   (or PCD/recursive) proof against a registered verifying key. Zcash full
   nodes already run halo2 verification for Orchard — the machinery exists;
   the ZIP is "expose it to a new context".
2. **Enshrined bridge accumulator**: an L2 deposit/withdraw queue whose
   transitions consensus checks against the proof above — this is what turns
   a validium into a real rollup (custody enforced by L1, operators can
   *only* censor).
3. **ZSA-based representation** (post-NU7 world): bridged claims as shielded
   issued assets, making L2-credit transfers themselves shielded L1 objects.

Aligns with where the core teams are already heading: Tachyon (proof-carrying
data / oblivious sync for L1 scale) builds exactly the recursive-proof
muscles an enshrined verifier needs, and Crosslink-style finality makes
anchors settle fast. **Verdict:** not a build plan, a multi-year ZIP
campaign — but design (A)/(B) so their artifacts (anchor format, proof
system, vault) migrate into it, and start the conversation early.

### D. Merge-mined sidechain

Equihash merge-mining a la Namecoin: borrow miner hashpower for the L2's own
PoW. **Verdict: rejected.** Weak subset-of-hashpower security, no finality,
still needs a custodial bridge, and messaging wants fast finality — BFT
strictly dominates here.

### E. Borrowed-consensus L2 (build on someone else's chain)

Run the logic on an existing programmable chain (a Cosmos appchain, an ETH
L2, NEAR, etc.) and bridge ZEC via threshold custody. **Verdict: rejected
for this project.** It imports a second token, a second trust root, and a
second community's failure modes, and forfeits the only reason to be
Zcash-native: one seed, one chain of trust, shielded end-to-end. (It remains
the fastest path for someone who primarily wants DeFi-style programmability
against ZEC.)

### F. "L2 without a chain": anchored replicated log — *underrated, often sufficient*

Not every L2 needs consensus. If the application only needs *one specific
service* to be verifiable — for messaging: the key-transparency directory —
then a **single-writer (or k-of-n FROST-writer) append-only log, Merkle-ized,
with epoch roots anchored via zkv** delivers: canonical ordering (zkv
sequences), split-view impossibility (anchors), auditability (log replay),
and censorship evidence (missed anchor epochs are publicly visible). No
validator set, no token mechanics, no bridge — because no funds live in it.
**Verdict: this is the correct "L2" for the messaging directory**, and it is
~a service + a zkv database, not a chain launch.

---

## 4. Recommendation

**For Zcash L2s in general:** A → B → C as one evolutionary track.
Launch as a checkpointed BFT chain with a FROST shielded vault and a
zkv-style sequence-protected anchor channel (A); add halo2 validity proofs to
every epoch (B); spend the intervening years driving the one-opcode
enshrinement ZIP (C) that upgrades custody from honest-threshold to
L1-enforced. Avoid merge-mining (D); avoid foreign-chain builds (E) unless
Zcash-nativeness isn't actually a requirement. F is the right answer whenever
the "L2" holds *data* rather than *money*.

**For the messaging system specifically:** phase it, and resist launching a
chain before the product proves it needs one.

- **Phase 1 (MVP): no L2 at all.** Identity roots are user-owned zkv
  databases on L1; relays are paid with blind credits bought via ordinary
  shielded ZEC; messages are never on any chain. L1 write volume is a few
  identity writes per user per year — no scaling problem exists yet.
- **Phase 2: option F.** Stand up the key-transparency directory as an
  anchored replicated log (per-operator zkv anchor channels). This absorbs
  device churn, usernames, and prekey audit off L1 while inheriting L1
  equivocation-proofing. Federated: every directory operator anchors
  independently; clients cross-check.
- **Phase 3 (only on demonstrated need): option A→B.** If credit settlement
  between relays, introduction-bond escrow, username markets, or third-party
  app state genuinely need shared programmable money, launch the BFT chain
  with the FROST vault — the anchor channel and operator set already exist
  from Phase 2, so the launch is an upgrade, not a new trust negotiation.
  Messages still never touch it; its state is identities, credits, and
  receipts.

Two design invariants across all phases: **the chain (L1 or L2) never holds
message-shaped data**, and **every anchor channel is a zkv database** — one
audited primitive for every "commit small data, prevent equivocation, read
cheaply" job in the system.

---

## 5. Sketch: Phase-3 chain, if it comes to that

- **Validators:** 20–100 operators (relay/directory operators are natural
  candidates), ZEC-bonded via the vault, CometBFT-class BFT, ~1 s finality.
- **State:** account-less where possible — credits as blinded balances
  (Chaumian mint run by the validator set, so even the L2 can't link credit
  flows), identity-log commitments, bond escrows, relay settlement nets.
- **Execution:** deliberately minimal state machine (a dozen transaction
  types), *not* a general VM — every feature you don't have is attack
  surface you don't audit. Revisit general programmability only under C.
- **Anchoring:** every epoch (~every few minutes) → zkv anchor channel,
  FROST-signed; clients treat an unanchored epoch as unfinalized.
- **Bridge:** ZEC in via shielded deposit to the FROST vault (memo carries
  the L2 credit destination, blinded); ZEC out via threshold-signed shielded
  payout. Withdrawal queue commitments live in the anchored state so
  censorship of exits is publicly provable.
- **Fees:** paid in ZEC-denominated credits; no new token. A new token is a
  product decision with regulatory and incentive blast radius — nothing in
  the architecture requires one.
- **Proof track (B):** epoch transition proofs in halo2 over the minimal
  state machine; verifying key published; anchor commits to proof hashes;
  clients verify lazily (sampling) or fully (servers/auditors).

---

## 6. Risks & open questions

1. **FROST vault liveness**: threshold loss (operators vanish) strands funds
   — needs resharing ceremonies, standby signers, and time-locked recovery
   paths (transparent P2SH timelock fallback is expressible today; shielded
   equivalents are not).
2. **Anchor-channel fee censorship**: a miner cartel could theoretically
   filter the anchor transactions; shielded anchors are hard to target
   (indistinguishable from ordinary traffic) — quantify this advantage.
3. **DA for the validium step**: DAC honesty vs external DA network — an
   external DA reintroduces the foreign-trust-root objection from (E).
4. **Legal shape of the operator set** (bonded validators running custody):
   the threshold-custody bridge is the regulatory pressure point; keep Phase
   2's fund-less design as long as possible.
5. **Crosslink/Tachyon timelines**: both materially improve this design
   (finality → faster trustworthy anchors; PCD → cheaper proofs and maybe
   the enshrined verifier). Track them; don't block on them.
6. **Ironwood/Orchard split**: anchor channels should live in the
   currently-default pool (Ironwood post-NU6.3 on testnet) — zkv already
   handles the receiver-sharing subtlety; keep the anchor spec pool-agnostic
   the way zkv's receiver domains are.
