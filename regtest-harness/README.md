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

## Rust version

This crate needs **Rust 1.89** or newer (std file locks, used by the datadir
lock probe). That is above the workspace's 1.88 MSRV, which is fine because
the root workspace excludes this crate: `cargo check --workspace` and the
`msrv (1.88)` CI job never build it.

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
  8. A batch write through the `batch_write` example: several ops in ONE
     transaction (one txid, one fee), with two writes to the same key taking
     consecutive replay versions. `write_many` has no CLI surface, and this
     harness has no `zkv` dependency, so a compiled example is how it is
     reached. Needs `$ZKV_BATCH_BIN`; skipped without it.
  9. `sync --rebuild`, admin and watch-only. A canary planted in the block
     cache proves the wipe happened (afterwards a rebuilt `data.sqlite` is
     indistinguishable from an untouched one), the root → `zec/lrz/`
     relocation is asserted on disk, and every read is compared through a
     projection that leaves out tip-relative confirmation counts.
- `tests/regtest_lock.rs`: two zkv processes on one database. zkv holds no
  lock of its own any more, so this is the node's datadir lock doing the
  serializing, and a node start *retries* for 60s rather than blocking
  forever. One process waits its turn and succeeds; one whose turn never
  comes gives up inside a two-sided time bound saying the database is in
  use; the database survives a SIGKILLed holder. Unfunded (the lock holder
  is `zkv init` on an unfunded database, whose poll loop holds one engine
  for its whole timeout), so it needs no `$DEVTOOL_BIN`.
- `tests/regtest_gui.rs`: the GUI browser transport against a live chain.
  Spawns the real `zkv gui-browser`, scrapes its session token out of
  `index.html` exactly as the frontend does, and drives `/api/*`. Covers the
  token and `Host` guards, that the demo database is not auto-provisioned
  (which would mean CI dialed a public server), the auto-sync loop advancing
  with no CLI involvement, a write through the GUI read back via the CLI,
  the `ZkvError`→HTTP contract, and a GUI sync waiting out a CLI that holds
  the lock. Needs a `zkv` built with `--features gui`; skips otherwise.
- `tests/regtest_migration.rs`: a database laid down by a pre-engine binary
  (`$ZKV_OLD_BIN`), adopted in place, with every read asserted identical.

`src/lib.rs` also carries the harness's own unit tests, which need no chain
and run under `--lib`: the datadir-lock probe against a real `flock`, and
the token scrape including its refusal of an unsubstituted placeholder.

Together these cover the paths with no offline tests at all (the command
modules) plus the on-chain halves of the protocol invariants the unit tests
can only simulate.

**Adding a test:** prefer a new phase on `regtest_kv.rs` when the work needs
a funded, INITed database - that stack is already up, so the marginal cost
is the mining your writes need rather than another three-minute funding
dance. `FundedStack::up` is there for when a genuinely separate binary is
warranted. Either way, a new binary **must** be added to the `tests` list in
`.github/workflows/regtest.yml`: it enumerates `--test` targets explicitly,
so one that is not listed silently never runs.

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
# `gui` is needed by regtest_gui.rs (it drives `zkv gui-browser`); drop it and
# that one test skips itself.
cargo build --release -p zcash_zkv --bin zkv --no-default-features --features cli,gui,transparent-inputs
# The batch-write phase runs this example as a subprocess.
cargo build --release -p zcash_zkv --example batch_write --no-default-features --features cli,gui,transparent-inputs

# Compile + link; skips the live run unless the binaries are provided:
cargo test --locked --manifest-path regtest-harness/Cargo.toml -- --nocapture --test-threads=1

# Full live run:
ZKV_BIN=$PWD/target/release/zkv \
ZKV_BATCH_BIN=$PWD/target/release/examples/batch_write \
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
