# zkv — a key-value store on Zcash

`zkv` is a Redis-style key-value store backed by Zcash Orchard memos. Each
"database" is identified by a **zkv address** — a Unified Full Viewing Key plus
a wallet birthday height — and is read by anyone with that address. Writes are
authenticated with a secp256k1 signature derived from the same UFVK's
transparent component, so only the admin who owns the wallet can issue valid
SETs and DELs.

```
SET zec_usd_price 1008.33
DEL zec_usd_price
```

This repo is a fork of [`zcash/zcash-devtool`](https://github.com/zcash/zcash-devtool)
with the `zkv` subcommand added.

> **Hackathon-grade. Do not use this in production.** No replay nonces,
> no rate-limit, the same-block ordering tiebreak is lexicographic txid not
> consensus order, and a full demo round-trip costs ~10–15k zatoshi in fees per
> SET/DEL. Read the [security notes](#security-notes) before assuming
> anything.

## Why?

A small public-but-authenticated config feed turns out to be useful:

- **Price oracles** — broadcast `zec_usd_price` so any wallet can pull a fresh
  rate without a centralized API.
- **Wallet feature flags** — `near_intents_enabled = false`, deployed faster
  than any release cycle. All your wallets watch one zkv address; you sign and
  broadcast a memo to flip the flag.
- **Anything where**: many readers, one trusted writer, on-chain authenticity,
  and you'd rather not run a server.

The reader's threat model is the same as Zcash itself plus "the writer hasn't
leaked their seed". The writer's threat model is the same as any Zcash sender.

## How it works

A **zkv address** is a string:

```
zkv1:<ufvk_bech32m>:<birthday_height>
```

The UFVK identifies a wallet account; the birthday is the chain height past
which readers need to scan. The zkv address embeds no separate signing pubkey
— it's *derived* from the UFVK's transparent component at a fixed BIP-44 path
(scope=external, index=0). Anyone who has the zkv address can compute the
signing pubkey; only the admin (who has the seed) can produce valid signatures.

A **write** is a Zcash transaction with a 1-zatoshi (or larger) Orchard output
to the UA derived from the zkv address's UFVK. The output's memo is a two-line
text payload:

```
ZKV1 SET <key> <value>
<128-char hex ECDSA signature, 64-byte compact form>
```

Or for deletes:

```
ZKV1 DEL <key>
<sig>
```

The signature is over `sha256(b"ZKV1\x00<zkv_addr>\x00<op>\x00<key>\x00<value>")`,
so a memo cannot be replayed against a different zkv database.

A **read** scans the wallet's Orchard memos (after `wallet sync`), drops any
that aren't well-formed `ZKV1` payloads or fail signature verification, sorts
the remainder by `(mined_height, txid, output_index)`, and replays SET/DEL into
a final state. Last write wins; deletes remove the key.

## Prerequisites

- Rust toolchain (install via [rustup](https://rustup.rs))
- A funded mainnet Zcash wallet (you'll spend small amounts of ZEC for fees)

## Build

```
cargo build --release
```

All `zkv` commands have a `--release` and a debug build; both work. The
demo commands below use `cargo run --quiet --` for brevity; substitute
`cargo run --release --quiet --` if you care about speed.

## Quick demo on mainnet

The full round-trip: init a wallet, fund it, write a key, read it back.

### 1. Init a mainnet wallet

```bash
mkdir -p ~/zkv-demo
echo "" | cargo run --quiet -- wallet \
  --wallet-dir ~/zkv-demo \
  init --name zkv-admin \
       --identity ~/zkv-demo/id.txt \
       --network main \
       --server zecrocks \
       --connection direct
```

The empty stdin tells the prompt to generate a fresh 24-word mnemonic. The
`id.txt` file is an [age](https://age-encryption.org/) identity that decrypts
the seed — **back it up; if you lose it, the wallet is gone.**

### 2. Get your zkv address

```bash
cargo run --quiet -- zkv --wallet-dir ~/zkv-demo address
```

Output (one long line):

```
zkv1:uview1...:3335223
```

This is your database identifier. Anyone you share it with can read the
database; only you (with the seed) can write authenticated entries.

### 3. Fund the wallet

Each SET or DEL is a real Zcash transaction. ZIP-317 fees are ~10–15k zatoshi
per write. To fund:

```bash
cargo run --quiet -- wallet --wallet-dir ~/zkv-demo list-addresses
```

Send a small amount (50,000 zatoshi covers a handful of writes; 0.01 ZEC =
1,000,000 zatoshi is more than enough) from any wallet to the listed UA, then:

```bash
cargo run --quiet -- wallet --wallet-dir ~/zkv-demo sync -s zecrocks
```

Wait for the balance to appear:

```bash
cargo run --quiet -- wallet --wallet-dir ~/zkv-demo balance
```

If the funds arrived as transparent or Sapling, the next SET will auto-shield
into Orchard as a side effect of the transfer.

### 4. Write a key

```bash
cargo run --quiet -- zkv --wallet-dir ~/zkv-demo \
  set zec_usd_price 1008.33 \
  -i ~/zkv-demo/id.txt
```

This signs and broadcasts a transaction. Expected output:

```
zkv SET zec_usd_price → zkv1:uview1...:3335223
  recipient (Orchard-only UA): u1q...
Creating transaction...
Proposed transfer: ...
Sending transaction...
<txid>
```

### 5. Read it back

After the tx is mined (≈75s on mainnet) and synced:

```bash
cargo run --quiet -- wallet --wallet-dir ~/zkv-demo sync -s zecrocks
cargo run --quiet -- zkv --wallet-dir ~/zkv-demo get
# zec_usd_price = 1008.33
```

A specific key:

```bash
cargo run --quiet -- zkv --wallet-dir ~/zkv-demo get zec_usd_price
# 1008.33
```

Delete:

```bash
cargo run --quiet -- zkv --wallet-dir ~/zkv-demo \
  del zec_usd_price -i ~/zkv-demo/id.txt
# ... wait for mining + sync ...
cargo run --quiet -- zkv --wallet-dir ~/zkv-demo get
# (empty)
```

## Reading from a third-party wallet

Any wallet that imports the UFVK and syncs can read a zkv database. With this
devtool, the consumer flow is:

```bash
mkdir -p ~/zkv-reader

# Extract the UFVK and birthday from the zkv address: zkv1:<ufvk>:<birthday>
ZKV='zkv1:uview1...:3335223'
UFVK=$(echo "$ZKV" | cut -d: -f2)
BDAY=$(echo "$ZKV" | cut -d: -f3)

cargo run --quiet -- wallet --wallet-dir ~/zkv-reader \
  init-fvk --name zkv --fvk "$UFVK" --birthday "$BDAY" -s zecrocks
cargo run --quiet -- wallet --wallet-dir ~/zkv-reader sync -s zecrocks
cargo run --quiet -- zkv --wallet-dir ~/zkv-reader get --zkv-addr "$ZKV"
```

The reader holds no spending key — they can decrypt the memos but cannot
forge writes. Choose a recent birthday: an old birthday makes mainnet sync
slow.

## Sign without broadcasting

To produce a signed memo that someone else broadcasts (e.g. from a different
wallet on the demo stage), pass `--print-memo`:

```bash
cargo run --quiet -- zkv --wallet-dir ~/zkv-demo \
  set zec_usd_price 1008.33 -i ~/zkv-demo/id.txt --print-memo
```

Output:

```
zkv SET zec_usd_price → zkv1:uview1...:3335223
  recipient (Orchard-only UA): u1q...

--- begin zkv memo ---
ZKV1 SET zec_usd_price 1008.33
0285d48bd429fd9382bb...
--- end zkv memo ---

Send a 1-zatoshi (or higher) Orchard payment to the recipient UA above
with this exact memo.
```

Now any wallet — Zashi, YWallet, Zingo, etc. — can broadcast the transaction
by sending the listed amount to the listed UA with the listed memo. The memo
signature does not depend on the broadcasting wallet, only on the admin's
seed and the zkv address.

## Subcommand reference

```
zkv address [ACCOUNT_ID]
zkv set <KEY> <VALUE> -i <identity> [--zkv-addr <a>] [--account-id <u>]
                                    [--print-memo]
zkv del <KEY>         -i <identity> [--zkv-addr <a>] [--account-id <u>]
                                    [--print-memo]
zkv get [KEY]                       [--zkv-addr <a>] [--account-id <u>]
                                    [--strict]
```

- **`address`** — derive the zkv address from a local wallet account's UFVK
  and the wallet's stored birthday height.
- **`set` / `del`** — sign with the admin's t-account secp256k1 key, build an
  Orchard-only UA from the zkv address's UFVK, and broadcast a 1-zatoshi
  payment with the signed memo. With `--print-memo`, prints the memo and the
  recipient UA without broadcasting.
- **`get`** — reads memos addressed to the local account that matches the zkv
  address's UFVK, replays SET/DEL in chain order, and prints either one key
  (when `KEY` is given) or all keys. `--strict` errors on the first malformed
  or invalid-signature memo instead of skipping it.

When `--zkv-addr` is omitted, `zkv` derives it from the selected local
account. `set`/`del` require the local account to *own* the zkv address's
UFVK (you can't sign for a database you don't hold the seed for). `get`
accepts any zkv address as long as the UFVK has been imported (e.g. via
`wallet init-fvk`) and synced.

## zkv address format

```
zkv1:<ufvk_bech32m>:<birthday_height_u32>
```

- `ufvk_bech32m` — a Unified Full Viewing Key in canonical bech32m encoding.
  Must contain both a transparent component (used to derive the signing
  pubkey) and an Orchard component (used to deliver memos). Sapling-only
  legacy UFVKs are rejected.
- `birthday_height` — the wallet birthday height as a base-10 u32. Used by
  consumers to bound their sync.

The signing pubkey is **not** in the zkv address. Both writers and readers
derive it from the UFVK at the fixed BIP-44 path
`m/44'/<coin_type>'/<account>'/0/0` (scope=external, index=0). For the writer
this resolves to a `secp256k1::SecretKey` via `usk.transparent().derive_secret_key`;
for the reader this resolves to the matching `secp256k1::PublicKey` via
`ufvk.transparent().derive_address_pubkey`.

## Memo wire format

Two lines of UTF-8, well within the 512-byte Zcash memo limit:

```
ZKV1 SET <key> <value>
<hex(64-byte compact ECDSA sig)>
```

```
ZKV1 DEL <key>
<hex sig>
```

Keys cannot contain whitespace; values cannot contain newlines. The signed
canonical payload (separate from the wire form) is null-separated to remove
ambiguity:

```
b"ZKV1\x00" || zkv_addr_utf8 || b"\x00" || op || b"\x00" || key || b"\x00" || value
```

The signature is over `sha256(canonical_payload)`. Including the full zkv
address in the signed bytes prevents a memo from being valid against any
other database.

## Replay semantics

Reading a zkv database = scanning Orchard memos addressed to the UFVK,
filtering well-formed `ZKV1` payloads, verifying signatures, sorting by
`(mined_height ASC NULLS LAST, txid ASC, output_index ASC)`, and applying SET
/ DEL into a `BTreeMap<String, String>` in that order. Same-block ordering
falls back to lexicographic txid, which is deterministic but **not**
consensus-canonical — don't rely on it for security-critical ordering.
Mempool entries (`mined_height IS NULL`) are applied last. Malformed memos
and bad signatures are dropped silently unless `--strict` is set.

## Security notes

- **No replay protection across blocks.** A SET memo, once mined, can be
  re-mined verbatim by anyone (e.g. by another full-node bot relaying
  copies). Last-write-wins masks this within a single database — but if you
  delete a key, an attacker who saved the original SET memo can't forge a
  re-SET (signatures don't help them broadcast a *new* tx because they don't
  hold the seed; but they could re-broadcast the *old* tx if it isn't yet in
  a block, which doesn't actually help them either since duplicates are
  rejected). In practice, **your enemy is mempool reorgs and your own
  fat-fingered overwrites**, not replay.
- **Address-binding** is real: a memo signed for database A will not verify
  for database B, because the canonical payload includes the zkv address.
- **Same-block ordering** is lexicographic txid. Two SETs to the same key in
  the same block produce a deterministic but unpredictable winner; don't do
  this if you care.
- **Memo size limit ~370 bytes for `key + value`.** ~140 bytes are
  overhead (`ZKV1 SET ` + newline + hex sig). The build will reject memos
  that overflow.
- **Fees are real.** Each SET / DEL is a real Zcash transaction. Plan ~15k
  zatoshi/write and budget accordingly.
- **The seed is everything.** `id.txt` is the age identity that decrypts the
  seed in `keys.toml`. Lose `id.txt` and the database is read-only forever.
- **Orchard-only delivery is enforced.** Writes go to an Orchard-only UA
  derived from the receiver UFVK; reads filter on `output_pool = 3`. Sapling
  memos are ignored even if some other tool sends them.
- **Mainnet-only design choices**: the `zkv address` command refuses
  Sapling-only legacy UFVKs, and the network type embedded in the UFVK must
  match the local wallet network.

## Tests

```
cargo test --bin zcash-devtool commands::zkv
```

Covers signature round-trip, address-binding (cross-DB replay rejection),
SET/DEL replay overwrites, and silent-drop of invalid signatures.

## License

Dual licensed under either of:

- Apache License, Version 2.0 ([LICENSE-APACHE](LICENSE-APACHE) or http://www.apache.org/licenses/LICENSE-2.0)
- MIT license ([LICENSE-MIT](LICENSE-MIT) or http://opensource.org/licenses/MIT)

at your option. Contributions are dual-licensed under the same terms unless
you state otherwise.
