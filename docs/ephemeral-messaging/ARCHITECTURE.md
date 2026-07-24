# Ephemeral Messaging on Zcash — Architecture

**Status:** design draft (v0.1, 2026-07)
**Working name:** `zmsg` (wire magic `ZMG0`, following zkv's `ZKV0` convention)
**Audience:** protocol/product engineering; assumes familiarity with zkv
(`README.md`), Zcash shielded transactions, and Signal-family cryptography.

This document architects a private, ephemeral messaging system that could power
a Signal-class consumer app, using Zcash as its trust anchor. Companion
document: [`ZCASH-L2.md`](./ZCASH-L2.md) covers the Layer-2 design space in
depth.

---

## 1. Design goals

1. **Ephemerality as a system property, not a UI toggle.** A message exists in
   exactly three places during its life: the sender's device, an encrypted
   in-transit queue with a TTL, and the recipient's device. Nothing durable
   accrues anywhere else — not on servers, and **never on the Zcash chain**.
   After delivery + expiry, no party (including us) can recover plaintext even
   with full key compromise, because ratchet keys are deleted (cryptographic
   erasure) and ciphertext is gone (physical erasure).
2. **Self-owned identity.** No phone numbers, no accounts on our servers.
   Identity is a keypair hierarchy rooted in a Zcash seed phrase; the same 24
   words restore your money *and* your identity (deliberately **not** your
   message history — see §7).
3. **Metadata minimization.** The strongest realistic adversary learns as
   little as possible about who talks to whom, when, and how much. This drives
   the transport design (§6) more than the message crypto does.
4. **Spam resistance without surveillance.** Zcash's shielded payments let us
   price abuse (unlinkable prepaid credits, refundable introduction bonds)
   instead of demanding identity.
5. **Payments are native.** Sending ZEC in a chat is a first-class message
   type, not an integration.
6. **No trusted operators.** Every server role (relay, directory) is untrusted
   for confidentiality and authenticity, and *detectably* faulty for
   availability/consistency. Chain anchoring (via zkv) is what makes directory
   equivocation detectable.

### Non-goals

- On-chain message transport or storage. The 512-byte memo is used for exactly
  one thing: censorship-resistant *first contact* (§6.5), and even that carries
  an introduction, not conversation content.
- Anonymity against a global passive adversary by default. We integrate
  Tor/mixnet hops (§6.4) but do not require constant-rate cover traffic in v1.
- Cross-protocol federation (Matrix/XMPP bridging). Bridges reintroduce
  plaintext-holding servers.

---

## 2. What Zcash gives us (L1 primitives inventory)

From `librustzcash` (state of tree: NU6.3/"Ironwood" on testnet, NU7 branch
defined):

| Primitive | Where | Use here |
|---|---|---|
| 512-byte encrypted memo per shielded output; `Memo::Arbitrary` = 511 usable bytes. **No ZIP-231 memo bundles in tree** — chunking across outputs is a client-level concern | `zcash_protocol::memo` | first-contact channel, zkv writes |
| Note encryption w/ ephemeral keys, ivk/ovk trial decryption; **no detection keys / OMR** — trial decryption is the only discovery model; compact blocks exclude memos (full-tx fetch to read one) | `zcash_note_encryption`, `zcash_client_backend::scanning` | the privacy + cost model for anything on-chain |
| Unified addresses / UFVK / UIVK; ZIP-316 unknown items (`Fvk::Unknown` etc.) round-trip | `zcash_keys`, `zcash_address` | zkv addresses; identity encoding |
| Diversified addresses + `UnifiedIncomingViewingKey::decrypt_diversifiers` (recover which index produced a UA) | `zcash_keys::keys` | per-context receiving addresses; a covert session-index channel |
| ZIP-32 hierarchical derivation; transparent scopes incl. ephemeral (ZIP-320/TEX fully plumbed) | `zip32`, `zcash_transparent::keys` | one seed → wallet + identity + messaging keys |
| Light-client scanning (compact blocks, lightwalletd, `BatchRunner`) | `zcash_client_backend` | shallow reads of identity databases |
| Built-in Tor (arti) with per-request circuit isolation | `zcash_client_backend::tor` | metadata-resistant chain reads & tx submission |
| Shielded pools: Sapling, Orchard, Ironwood (NU6.3 / Orchard rev. V3) | `zcash_protocol::ShieldedPool` | target Orchard/Ironwood for all zmsg on-chain traffic |
| ZIP-317 fees (~0.0001 ZEC/action) | `zcash_primitives` | cost model for identity writes & intros |
| ZIP-321 payment URIs (`other_params` extension hatch), PCZT (roles + `proprietary` fields + redactor) | `zip321`, `pczt` | in-chat payments, multi-device/threshold signing |
| Transparent OP_RETURN, 80 bytes, zero-value (`add_null_data_output`) | `zcash_transparent::builder` | (avoided here — shielded memos dominate; relevant to L2 anchoring, see companion doc) |
| FROST threshold signatures (ecosystem: ZF frost crates) | external | L2 bridge custody, org accounts |

Not in the tree (plan around): ZIP-231 memo bundles, ZIP-316 R1 metadata/expiry,
detection keys/OMR, ZSA, Tachyon. NU7 exists as scaffolding only (ZIP-233
ZEC-burn rides behind it — a future primitive for burn-priced anti-spam).

And from **zkv** specifically (this repo):

- A **database identity** = UFVK + birthday in one bech32m token (`zkv1…`),
  readable by anyone holding it, writable only by registered signers.
- **Recoverable secp256k1 signatures** over receiver-bound, sequence-versioned
  domains: multi-writer without pubkey fields, replay-proof across databases
  and networks, tombstone-surviving counters.
- **Owner/writer registry with scopes** (`CREATE`/`UPDATE`/`DESTROY`) — an
  on-chain ACL replayed identically by every reader; no server arbitrates.
- **Shallow sync** — stateless, wallet-less reads of recent writes with
  per-memo signature verification. This is what makes chain-anchored identity
  *cheap to consume* on mobile.
- **FINALIZE** — permanent sealing, useful for identity tombstones.

The key architectural judgment: **Zcash is our root of trust and our money,
not our message bus.** Block time (~75 s), fee-per-write, and permanent public
storage make the chain exactly wrong for messages and exactly right for the
small, slow, must-not-be-equivocated data: identity roots, device sets, key
rotations, revocations, and anchors for off-chain logs.

---

## 3. System overview

```
┌─────────────────────────────────────────────────────────────────────┐
│  Client (mobile/desktop)                                            │
│  • key vault (seed-derived)   • Double Ratchet / MLS sessions       │
│  • local ephemeral store      • wallet (spend + view)               │
└───────┬──────────────────┬──────────────────────┬───────────────────┘
        │ sealed sender    │ prekey fetch,        │ identity writes,
        │ over Tor/mixnet  │ KT proofs            │ intro memos, ZEC
┌───────▼────────┐  ┌──────▼────────────┐  ┌──────▼───────────────────┐
│ Relays         │  │ Directories       │  │ Zcash L1                 │
│ TTL'd queues,  │  │ prekey bundles,   │  │ • identity DBs (zkv)     │
│ no accounts,   │  │ KT Merkle log,    │  │ • KT anchors (zkv)       │
│ paid w/ blind  │  │ handle resolution │  │ • first-contact memos    │
│ credits        │  │ (all commitments  │  │ • payments               │
│                │  │  anchored → zkv)  │  │                          │
└────────────────┘  └───────────────────┘  └──────────────────────────┘
```

Four planes:

1. **Identity plane** (chain-anchored, slow, public): who you are, which
   devices speak for you, how to reach you. §4.
2. **Session plane** (end-to-end, off-chain): PQXDH key agreement, Double
   Ratchet / MLS, disappearing-message policy. §5.
3. **Transport plane** (untrusted infrastructure): unlinkable TTL'd queues on
   relays, sealed sender, Tor/mixnet, on-chain first contact. §6.
4. **Value plane**: shielded ZEC for anti-spam credits, introduction bonds,
   and in-chat payments. §8.

---

## 4. Identity

### 4.1 Requirements

- Survives total loss of our infrastructure (user re-derives from seed).
- Multi-device with per-device keys and remote revocation.
- Rotation and compromise recovery with a verifiable audit trail.
- Resolvable from a short shareable handle; verifiable out-of-band (QR /
  safety numbers).
- No global consensus needed for *reads* to be safe: a reader must detect a
  split view (directory showing Alice one key set and Bob another).

### 4.2 Options considered

**(a) Bare UA-as-identity.** Your Zcash unified address is your handle;
messaging keys derived via ZIP-32 from the same seed; first contact via memo.
*Pros:* zero infrastructure. *Cons:* no rotation (a UA can't be re-keyed, only
abandoned), no device management, no revocation story, ties identity
irrevocably to a payment address. Kept only as the degenerate fallback.

**(b) zkv1 identity database (chosen as the root).** Each user identity *is a
zkv database*: a UFVK + birthday under a dedicated ZIP-32 account, with the
signed KV log carrying the identity record. zkv already provides exactly the
properties an identity root needs:

- **Authenticated multi-writer**: each device is a `zkvid1…` signer added via
  `WRITERADD` with a scope; the root (seed-derived) key is the owner. Losing a
  phone → `WRITERDEL` from any owner device. Losing everything → restore seed.
- **Replay-protected, tombstone-surviving sequences**: a revoked device's old
  key announcements cannot be replayed back into validity — precisely the
  attack that breaks naive key servers.
- **Reader-side enforcement**: any contact holding the `zkv1…` address replays
  the ACL themselves; no server can forge device additions.
- **Shallow sync**: contacts verify a rotation in seconds without a wallet.
- **FINALIZE**: a permanent "this identity is dead, never trust new keys"
  tombstone — something no PKI does well.

**(c) Directory service with key transparency (chosen for the operational
layer).** One-time prekeys, presence, and handle→identity resolution cannot
live on-chain (too slow, prekeys are consumable). Directories are untrusted
servers whose entire state is a Merkle-ized append-only log (CONIKS/Signal-KT
style), with epoch roots **anchored into a zkv database** the operator owns.
zkv's sequenced, signed, publicly-replayable log kills KT's classic weakness —
split-view/equivocation — without asking users to gossip roots: two clients
comparing an epoch root both resolve it from the same Zcash chain state.

**(d) DID/VC stacks, ENS-style name chains, Session/Oxen-style network IDs.**
Surveyed; rejected for v1: they either import a foreign trust root (another
chain), an oracle, or a token we don't need. A minimal `did:zkv` method can be
specified later as a compatibility veneer over (b).

### 4.3 The identity record (zkv key schema)

One ZIP-32 seed derives, at distinct accounts/paths:

```
m/44'/133'/0'   wallet (user funds; never used by zmsg directly)
m/44'/133'/1'   identity database (zkv UFVK; its root signer = identity root)
identity root key  → secp256k1 (zkv signing)      "who owns this identity"
messaging identity → Ed25519/X25519 pair (IK)      "what PQXDH binds to"
per-device keys    → generated on-device, never leave it
```

Identity database contents (all values signed zkv records):

```
id/v            format version
id/ik           messaging identity public key (X25519 + ML-KEM pubkey hash)
id/profile      display name, avatar hash (optional, user choice)
id/dev/<fpr>    device record: device pubkey, capabilities, added-at
id/dir          list of directory endpoints serving this identity's prekeys
id/relay        current mailbox hints (relay endpoints; coarse, rotatable)
id/kt           directory operators' zkv anchor addresses this user accepts
id/revoke/<fpr> device revocation (tombstone; DEL also acceptable)
```

Writes are rare (device add/remove, rotation, moving relays): a handful of
ZIP-317-fee transactions per year per user. Reads are `shallow` scans by
contacts, plus full replay on first add.

**Cost/scale note:** an identity write is a zero-value shielded output —
globally, millions of users doing a few writes/year is well within L1
capacity; this is why the *identity* plane can stay on L1 even at Signal
scale, while messages never touch it. If identity-write volume ever matters,
the directory log itself can absorb device churn with only its *anchors* on
L1 (see `ZCASH-L2.md` §"Phase 2").

### 4.4 Handles, discovery, verification

- **Canonical identity** = the zkv address (`zkv1…`, long, QR-friendly).
- **Short handle** `zid1…` = bech32m of the 32-byte hash of the identity
  database's receiver domain (the same birthday-independent domain zkv
  signatures bind). Directories resolve `zid1…` → `zkv1…` → record; the hash
  makes the resolution self-verifying.
- **Human names** are petnames (local address book) or directory-scoped
  usernames (`alice@dir.example`) — directory-scoped names are convenience
  pointers inside the KT log, never the trust root.
- **Safety numbers**: fingerprint of both parties' identity roots + messaging
  IKs, exactly Signal's UX; additionally, clients continuously verify the
  contact's device set against the contact's own zkv database, so a directory
  cannot silently inject a device — the attack Signal's design must trust its
  server not to do.
- **Contact discovery** is deliberately *not* "upload your address book". No
  phone numbers exist in the system. Discovery = QR / link / handle exchange /
  on-chain intro (§6.5).

### 4.5 Multi-device & groups identity

Each device holds its own ratchet state and device key; devices appear in
`id/dev/*` (signed by root or any owner-scoped device) and in the sender's
sealed-sender certificates. Group *membership* lives only inside MLS group
state on members' devices — membership lists never go on-chain and never sit
plaintext on servers. Organizations can run an identity database with
multiple `OWNER` keys (or a FROST-threshold root) for shared/broadcast
identities.

---

## 5. Sessions and ephemerality

### 5.1 Pairwise: PQXDH + Double Ratchet

- **Initial key agreement: PQXDH** (Signal's X3DH successor): X25519 DH
  combinations + ML-KEM-768 encapsulation against harvest-now-decrypt-later.
  Bundle = identity key IK, signed prekey SPK (rotated ~weekly), one-time
  prekeys OPK (consumed per contact), last-resort KEM prekey. Bundles are
  served by directories (§4.2c), signed by a device key that chains to the
  zkv identity record.
- **Conversation: Double Ratchet** (X25519 DH ratchet + HKDF symmetric
  ratchet), per device pair. Post-quantum ratchet (e.g. KEM-ratchet hybrid) is
  an upgrade slot in the wire format, not a v1 blocker.
- **Deniability**: no signatures over message content; authenticity comes from
  the DH-derived MAC keys (offline deniability as in X3DH/PQXDH). Content
  signatures are reserved for identity-plane records, where non-repudiation is
  the point.

### 5.2 Groups: MLS

RFC 9420 MLS for group sessions (TreeKEM), with zmsg identities as the MLS
credential type (credential = device key + chain of custody to the zkv
identity root). Relays play the untrusted MLS Delivery Service role for
ordering commits; they see only opaque handshake/application ciphertext on
TTL'd queues. Sender-keys à la Signal is the fallback if MLS proves too heavy
on mobile for very large groups.

### 5.3 What "ephemeral" means, precisely

| Layer | Mechanism | Guarantee |
|---|---|---|
| Cryptographic | ratchet key deletion after decrypt (and skipped-key caps) | forward secrecy: past messages unrecoverable post-compromise |
| Transit | relay queues have hard TTL (default 14 d) + delete-on-ack | no durable ciphertext archive exists anywhere |
| Endpoint | disappearing timers (per-conversation, default **on**, e.g. 7 d) | cooperative plaintext erasure on devices |
| Chain | **nothing message-shaped is ever written on-chain** | no permanent record to regret |
| Backup | no message backup in v1; identity/wallet restore from seed only | ephemerality survives our own product roadmap |

Post-compromise security comes from the DH ratchet (pairwise) and MLS
epochs (groups). "Sealed + ephemeral" composition detail: delivery receipts
and typing indicators ride inside the ratchet like Signal, so the relay's TTL
enforcement can't be spoofed into an oracle by observing ack traffic.

---

## 6. Transport

### 6.1 Relays: unlinkable TTL'd queues

The SimpleX insight, adopted: **the transport layer has no user identifiers at
all.** A conversation direction = a *queue* on some relay:

- Recipient creates a queue on a relay of their choice; the queue ID is a
  random capability, unlinkable to identity or to their other queues.
- Sender gets (relay endpoint, queue ID, sender-auth key) inside the encrypted
  session (or the intro payload); rotates per conversation, rotatable at will.
- Relay stores opaque ciphertext until fetch+ack or TTL, then deletes. Relays
  keep no accounts; authorization to enqueue = possession of a per-queue
  sender token + a spend of one anti-spam credit (§8.1).
- Multi-device fan-out is the recipient's client's job (queues per device).

Relays are federated: anyone can run one (we operate defaults). A user's
current relay *hints* live in their identity record (`id/relay`) coarse enough
not to leak per-conversation structure; the per-conversation queue handles are
only ever exchanged E2E.

### 6.2 Sealed sender, always

Every envelope a relay accepts is sender-anonymous: the outer layer
authenticates only "holder of this queue's sender token + a valid credit".
Sender identity and device appear only inside the ratchet ciphertext
(certificate chained to the sender's zkv identity). Unlike Signal, sealed
sender is not an optimization over an account system — there are no accounts
to fall back to.

### 6.3 Push and mobile reality

Mobile delivery uses platform push (APNs/FCM) as a *wake-up bell only*:
content-free pings from the recipient's own relay, with the app then fetching
over Tor. Users who refuse platform push get long-poll/websocket with
batched wakeups at a battery cost. This is the pragmatic compromise every
private messenger makes; putting the push token at the relay (not a central
zmsg server) at least shards the metadata.

### 6.4 Network-layer privacy

- Built-in Tor (arti) for all relay and directory traffic — zkv already ships
  SOCKS support (`socks.rs`); lightwalletd connections included.
- Optional mixnet mode (Nym or a Sphinx-format mix layer over volunteer
  relays) for cover against timing correlation, accepting latency.
- Padding: fixed-size envelope buckets (e.g. 2 KiB / 16 KiB / 64 KiB) so
  ciphertext length classes, not exact sizes, are visible. Attachments go
  encrypted-chunked to content-addressed ephemeral blob stores (paid with the
  same credits, same TTL semantics).

### 6.5 First contact over the chain (the memo bootstrap)

The one chain-touching message flow, and the piece nothing but Zcash gives us:
**you can always reach an identity, even if every relay and directory is
blocked or the recipient's infra is unknown** — because an identity is a
shielded address.

```
ZMG0 INTRO <ver> <from-zid> <pqxdh-prekey-offer|queue-offer> <note…>
```

- Encoded as `Memo::Arbitrary` (511 usable bytes) on zero-value shielded
  outputs to the recipient's identity UA (derived from their zkv address).
  There are no ZIP-231 memo bundles yet, so an oversize INTRO (PQXDH offer
  with ML-KEM material runs past one memo) chunks across 2–3 outputs of the
  same transaction with a short continuation header; ZIP-317 fee per action
  prices spam.
- The recipient's client `shallow`-scans its identity database for `ZMG0`
  memos alongside `ZKV0` ones. Detection is trial decryption (there is no
  OMR/detection-key primitive in the protocol today), and compact blocks
  don't carry memos, so the flow is: trial-decrypt compact outputs → fetch
  the few matching full transactions → parse memos. zkv's shallow-sync path
  does exactly this already. An INTRO carries enough (ephemeral keys, a
  sender-created reply queue) for the recipient to complete key agreement and
  move *immediately and permanently* off-chain.
- Anti-spam: intros from unknown senders can be required to carry a **bond**
  — a small shielded ZEC amount in the same transaction, refundable via the
  reply flow on accept (§8.2). Burning a fee to annoy a stranger is possible;
  doing it at scale is expensive.
- Privacy note: an intro's *existence* is on-chain but shielded — an observer
  learns nothing (zero-value output to an unknown receiver among all shielded
  traffic). The recipient's UFVK holder set (their contacts, if they shared
  the full zkv address) can see intro *timing*; the `zid1…` handle flow hands
  out an incoming-viewing-only path instead. Payload content is E2E to the
  identity UA. This is contact metadata equal to what any on-chain payment
  already reveals — and it is the fallback path, not the common path.

---

## 7. Client storage & recovery

- Message store: encrypted SQLite (per-device key in OS keystore), rows carry
  their disappearing deadline; deletion is eager + vacuumed.
- The seed restores: funds, identity ownership, device-authorization ability.
  It deliberately does **not** restore messages, contacts' trust states, or
  ratchets — new device = new device key admitted via an existing device or
  root key, sessions re-established, history absent by design. (Optional
  encrypted contact-list backup is a product decision, off by default.)

---

## 8. The value plane

### 8.1 Anti-spam credits (unlinkable prepaid)

Relays and blob stores meter by **blind credits**: the client pays a relay a
shielded ZEC amount, receives a batch of blind-signed tokens (Privacy
Pass/blind-RSA or KVAC family), spends one per enqueue. The relay cannot link
payment→token→message; the client gets fee-priced spam resistance without an
account. Default relays can grant free starter quotas; the mechanism is the
same (zero-priced tokens), so the privacy property doesn't depend on payment.

### 8.2 Introduction bonds

First contact from a stranger (on-chain intro or relay intro-queue) may carry
a refundable bond (e.g. 0.005 ZEC): accepted → auto-refund inside the new
session's first messages (a ZIP-321 payment or a bare note transfer);
declined/ignored → sender's loss after timeout. Users set their own price,
including zero for open-DM.

### 8.3 In-chat payments

A payment is a message type: client constructs a shielded transaction
(ZIP-321 request → PCZT flow for multi-device/threshold signing), sends it via
the wallet plane, and drops the receipt (txid + amount + memo) into the
ratchet. Group splits, requests, and streaming-ish allowances are application
sugar on the same primitive. This is the product moat: Signal cannot do
native private money.

---

## 9. Threat model summary

| Adversary | Mitigation |
|---|---|
| Relay/directory operator (honest-but-curious or malicious) | E2E crypto; sealed sender; no accounts; KT log anchored via zkv → equivocation detectable; queues unlinkable |
| Network observer (local/ISP) | Tor by default; padding buckets; mixnet option |
| Global passive adversary | partial: mixnet mode + padding; explicit non-goal in v1 to fully defeat |
| Contact turned adversary | per-conversation queues rotatable; disappearing history; deniable authentication |
| Device compromise | FS (past safe), PCS via ratchet/MLS epochs (future recovers), remote device revocation via zkv `WRITERDEL` |
| Our own infrastructure seized | nothing to seize but ciphertext-in-transit and TTL'd queues; identity plane on-chain; users migrate relays via `id/relay` |
| Chain-level: reorgs | identity reads honor confirmation depth (zkv `Confirmations`); intros are latency-tolerant |
| Harvest-now-decrypt-later | PQXDH (ML-KEM) in v1; PQ ratchet slotted; note: Zcash L1 privacy itself is not PQ — intros' existence metadata inherits that risk |
| Legal compulsion of operators | can only stop service (visible), not disclose content/history that doesn't exist; federation + on-chain intro path as fallback |

---

## 10. Why not… (alternatives considered)

- **Messages in memos (on-chain chat):** permanent public ciphertext archive
  violates goal #1 categorically; 75 s latency; fee per message; chain bloat.
  Rejected regardless of L2 availability.
- **Session/Oxen model (network-stored swarms):** stores ciphertext on a
  DHT-ish network — durable by design, weak FS history. Conflicts with
  ephemerality.
- **Waku/XMTP gossip:** store-and-forward networks with retention windows;
  closest cousin, but identity is chain-token-centric and retention is a
  network property we'd rather make a *contractual TTL* per relay.
- **Pure P2P (no relays):** async delivery to phones requires *someone* to
  hold ciphertext; better to make that role explicit, paid, and TTL-bound.
- **A smart-contract identity registry on another chain:** imports a second
  trust root and token; zkv gives us the registry semantics we need on the
  chain we already trust for money.

---

## 11. Build phases

**Phase 1 — no new infrastructure classes (MVP):**
zkv-based identity databases + `zid` handles; PQXDH/Double Ratchet client;
2–3 first-party relays (TTL queues, blind credits); directory v0 = prekey
serving with signed bundles (KT log format defined, single operator);
on-chain INTRO; in-chat payments. *Zcash L1 only.*

**Phase 2 — federation + verifiability:**
open relay federation; KT log live with zkv anchoring and client verification;
MLS groups; mixnet option; introduction bonds.

**Phase 3 — scale-out (only if needed):**
directory/anchoring L2 per `ZCASH-L2.md` recommendation (federated BFT log
with Zcash anchoring and FROST-vault settlement), absorbing identity churn
and credit settlement off L1. Messages remain off-chain forever, L2 included.

---

## 12. Open questions

1. Prekey-bundle authenticity UX when a user's chosen directory is offline —
   how aggressively to fall back to last-resort KEM prekeys vs on-chain intro.
2. MLS credential binding format for zkv identities (needs a short spec).
3. Blind-credit scheme selection (Privacy Pass RSA vs KVAC/anonymous
   credentials with rate-limiting semantics).
4. Multi-memo INTRO framing (512-byte limit vs ML-KEM-768 encapsulation +
   offer payload — likely 3 outputs; alternatively an on-chain pointer to an
   encrypted blob on a relay).
5. Whether `id/relay` hints should be dropped entirely in favor of
   directory-only reachability (less on-chain metadata, more directory
   dependence).
6. Push-token privacy: per-relay tokens vs a rendezvous push proxy.
