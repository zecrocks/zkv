# zkv regtest end-to-end harness

A standalone crate that runs the real `zkv` CLI binary against a **real**
Zcash backend, `zebra` in **Regtest** mode behind `lightwalletd`, and drives
it through the full database lifecycle. It is the "live" integration tier;
the offline tier lives in `cargo test --workspace` at the repo root (the
protocol/replay/snapshot/shallow unit tests).

The stack and the process-orchestration code follow zecd's regtest harness
(the Zcash-Foundation-standard approach): there is **no
`zingo-infra`/`zcash_local_net` dependency** and **no compile-time zebra
dependency**. Blocks are mined with zebrad's own **Regtest-only `generate`
RPC** (shipped since zebra 2.0.0), which runs the `getblocktemplate` ->
assemble -> `submitblock` flow *server-side* against the node's own network
parameters (PoW is disabled on Regtest, so there is no solving step). The
harness is a pure black-box driver: it works unmodified against any zebrad
release and drives `zkv` strictly as subprocesses of the built binary, so the
real sync/sign/broadcast pipeline is exercised, not library shortcuts.

## Why a separate crate

The harness lives in its **own workspace** (note the empty `[workspace]` in
`Cargo.toml`), so its e2e-only dependency tree (reqwest etc.) never touches
zkv's hard-pinned librustzcash lockfile: `cargo build` at the repo root never
sees it. It commits its **own `Cargo.lock`**; build with `--locked`.

## What runs

- `tests/regtest_unfunded.rs`: the unfunded surface. Create a regtest
  database against a live chain (`zkv init --network regtest
  --non-interactive`), the self-describing `zkvregtest1...` address (offline
  `inspect` recovers network/pool/birthday/keys), the not-initialized read
  refusal, `list`, and the non-interactive resume path. Needs only
  zebrad + lightwalletd.
- `tests/regtest_kv.rs`: the funded lifecycle, the load-bearing protocol e2e:
  1. Fund a `zcash-devtool` wallet by mining its transparent coinbase, mature
     it (100 blocks, via a miner-swap restart), shield to Orchard.
  2. `zkv init` (create), fund the wallet's UA from the devtool wallet, then
     the `zkv init` resume path broadcasts INIT and waits for confirmation
     while the harness mines.
  3. Data ops on chain: SET (create), SET (overwrite; the second write must
     carry a nonzero replay-protection `[seq]` on the wire), a second key,
     DEL (tombstone), `keys` globbing.
  4. `history`: the genesis INIT entry, both greeting writes, the DEL, every
     entry signature-verified, creator attribution.
  5. Roles: WRITERADD/WRITERDEL management memos targeting a second
     database's `zkvid1...` key; the registry and the revocation tombstone.
  6. A watch-only replica imported from nothing but the `zkvregtest1...`
     address converges on the same state; a duplicate import is refused.
  7. A shallow (db-less) `zkv shallow get` against the bare address agrees
     with the full replay.

Together these cover the paths with no offline tests at all (`sync.rs`,
`write.rs`, `send.rs`, the command modules) plus the on-chain halves of the
protocol invariants the unit tests can only simulate.

## Funding Orchard on regtest

Regtest can't mine a coinbase straight into an Orchard note that a shielded
wallet would scan, so the funded test funds zkv the way the protocol allows,
using [`zcash-devtool`](https://github.com/zecrocks/zcash-devtool)
(regtest-enabled) as a funding wallet (`$DEVTOOL_BIN`):

1. Mine a **transparent** coinbase to the funder's address (zebra's
   `[mining] miner_address`).
2. Mine past **coinbase maturity** (100 blocks).
3. `devtool wallet shield`: shield the matured coinbase into **Orchard**.
4. `devtool wallet send`: send TAZ to the zkv wallet's `uregtest1...` funding
   UA; mine to spendability (external receives confirm at the untrusted
   ZIP-315 depth, 10 blocks).

## Running

Provide the node binaries via `$ZEBRAD_BIN` / `$LIGHTWALLETD_BIN` (any zebrad
>= 2.2.0 works; in CI they're extracted from the `zfnd/zebra` and
`electriccoinco/lightwalletd` images) and the funder via `$DEVTOOL_BIN`.
Without them the tests **skip**, so they still validate that the harness
compiles and links.

```sh
# From the repo root: the harness drives the release binary (debug Orchard
# proving is >20s per write).
cargo build --release -p zkv --bin zkv --no-default-features --features cli,transparent-inputs

# Compile + link; skips the live run unless the binaries are provided:
cargo test --locked --manifest-path regtest-harness/Cargo.toml -- --nocapture --test-threads=1

# Full live run:
ZKV_BIN=$PWD/target/release/zkv \
ZEBRAD_BIN=/path/to/zebrad LIGHTWALLETD_BIN=/path/to/lightwalletd \
DEVTOOL_BIN=/path/to/zcash-devtool \
  cargo test --locked --manifest-path regtest-harness/Cargo.toml -- --nocapture --test-threads=1
```

Debug hooks: `ZEBRAD_STDERR=<file>` captures zebrad's logs; the `zkv init`
poll loop's status lines stream to the test output (use `--nocapture`).

The regtest chain's activation heights (NU5/NU6 at height 1, NU6.1/NU6.2 at
height 4) are written into `zebrad.toml` by the harness and **must match**
`zkv`'s fixed regtest parameters in `crates/zkv/src/network.rs`; change them
together or signatures/branch ids diverge.

Bumping zebra: change the `zfnd/zebra` image tag in
`.github/workflows/regtest.yml`; that's it. The weekly CI cron tests both the
pinned image and `zfnd/zebra:latest` (the upstream canary).
