//! End-to-end regtest harness for `zkv`.
//!
//! Orchestrates a `zebrad` (Regtest, PoW disabled) node with a `lightwalletd`
//! in front of it, and drives the real `zkv` CLI binary as a subprocess
//! against that stack, so the whole pipeline (lightwalletd gRPC sync, memo
//! build/sign, ZIP-317 spend, broadcast, snapshot + tail replay) is exercised
//! exactly as a user runs it. Blocks are mined with zebrad's own Regtest-only
//! `generate` RPC (zebra >= 2.0.0), which runs the template -> assemble ->
//! submit flow server-side against the node's own network parameters; the
//! harness has no zebra crate dependency and works against any zebrad release.
//!
//! Funding: regtest can't mine a coinbase straight into an Orchard note, so
//! the funded test mines a transparent coinbase to a `zcash-devtool` funding
//! wallet, lets it mature, shields it into Orchard, and sends TAZ to the zkv
//! wallet's funding address.
//!
//! Binaries are supplied by the caller via `$ZEBRAD_BIN` / `$LIGHTWALLETD_BIN`
//! / `$DEVTOOL_BIN` (see [`resolve_bin`]); in CI they are extracted from the
//! official `zfnd/zebra` and `electriccoinco/lightwalletd` images. `zkv`
//! itself is the built release binary (`$ZKV_BIN`, defaulting to the parent
//! workspace's `target/release/zkv`).
//!
//! The process-orchestration layer (zebrad/lightwalletd/funder drivers) is
//! adapted from zecd's regtest harness, which pioneered this
//! Zcash-Foundation-standard stack (zebra `generate` RPC, no zingo-infra).

use std::io::Write;
use std::net::TcpListener;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Output, Stdio};
use std::time::{Duration, Instant};

use anyhow::{anyhow, bail, Context, Result};
use serde_json::{json, Value};

/// Pick an unused loopback TCP port (bind `:0`, read the port, release it).
/// Racy by nature, but fine for a single-threaded test run.
pub fn pick_port() -> Result<u16> {
    let listener = TcpListener::bind("127.0.0.1:0").context("bind ephemeral port")?;
    Ok(listener.local_addr()?.port())
}

/// Resolve a required external binary from `$<env_var>`, returning `None` if
/// unset or missing so callers can skip the live test cleanly.
pub fn resolve_bin(env_var: &str) -> Option<PathBuf> {
    std::env::var(env_var)
        .ok()
        .map(PathBuf::from)
        .filter(|p| p.is_file())
}

// =============================== zebrad (Regtest validator) ===============================

/// Height at which NU6.1 and NU6.2 activate on our regtest chain. NU5/NU6 are
/// active from height 1; NU6.1's activation block requires ZIP-271 lockbox
/// disbursements out of the deferred pool, which only accrues once NU6 is
/// live, so NU6.1/NU6.2 activate a few blocks in, after a pool exists.
/// Must match `zkv`'s `network::Network::Regtest` activation heights
/// (`crates/zkv/src/network.rs`).
const NU6_2_ACTIVATION_HEIGHT: u32 = 4;
/// Height at which NU6.3 (Ironwood) activates on our regtest chain, emitted into
/// zebrad's config only for the Ironwood tier (see [`nu6_3_height`]). **Must**
/// match `zkv`'s `network::REGTEST_NU6_3_HEIGHT` and the devtool funder's
/// `--activation-heights`, or NU6.3 consensus diverges. A few blocks after
/// NU6.2, mirroring how NU6.2 trails NU6.
const NU6_3_ACTIVATION_HEIGHT: u32 = 8;

/// The NU6.3 (Ironwood) regtest activation height, or `None` for a pre-Ironwood
/// (stock zebra) run. Enabled by setting `ZKV_REGTEST_NU6_3=1` (the CI Ironwood
/// tier does this). Opt-in because stock zebra rejects the unknown `"NU6.3"`
/// activation-height key at startup, so it can only be emitted against an
/// Ironwood-capable zebra (e.g. `zfnd/zebra:6.0.0-rc.0`). An Ironwood zkv build
/// always activates NU6.3 at this height on regtest, so it agrees with the chain
/// only when this is enabled.
fn nu6_3_height() -> Option<u32> {
    match std::env::var("ZKV_REGTEST_NU6_3") {
        Ok(v) if !v.is_empty() && v != "0" => Some(NU6_3_ACTIVATION_HEIGHT),
        _ => None,
    }
}
/// ZIP-271 one-time lockbox disbursement paid in the NU6.1 activation block's
/// coinbase. A P2SH regtest address and a token amount (<= the pool accrued by
/// [`NU6_2_ACTIVATION_HEIGHT`]).
const LOCKBOX_DISBURSEMENT_ADDR: &str = "t27eWDgjFYJGVXmzrXeVjnb5J3uXDM9xH9v";
const LOCKBOX_DISBURSEMENT_ZATS: u64 = 1;

/// A throwaway transparent address used as zebra's coinbase recipient when the
/// caller doesn't need to control the coinbase (the unfunded e2e). Funded
/// flows pass the funding wallet's own address.
const DEFAULT_MINER_ADDRESS: &str = "t27eWDgjFYJGVXmzrXeVjnb5J3uXDM9xH9v";

/// A running `zebrad` Regtest node.
pub struct Zebrad {
    child: Child,
    /// JSON-RPC port (cookie auth disabled so lightwalletd can connect).
    pub rpc_port: u16,
    net_port: u16,
    bin: PathBuf,
    config_path: PathBuf,
    _dir: tempfile::TempDir,
}

/// Spawn `zebrad --config <config_path> start`. Set ZEBRAD_STDERR to a file
/// path to capture its logs (zebra logs to stdout, so route both there);
/// otherwise discard them to keep test output clean.
fn spawn_zebrad(bin: &Path, config_path: &Path) -> Result<Child> {
    let (out, err) = match std::env::var_os("ZEBRAD_STDERR") {
        Some(p) => {
            let f = std::fs::File::create(&p).context("create ZEBRAD_STDERR file")?;
            let f2 = f.try_clone().context("clone ZEBRAD_STDERR file")?;
            (Stdio::from(f), Stdio::from(f2))
        }
        None => (Stdio::null(), Stdio::null()),
    };
    let mut cmd = Command::new(bin);
    // zebrad reads `ZEBRA_*` environment variables as config overrides
    // (config-rs), and an unrelated variable like `ZEBRA_TAG` in a CI job
    // makes it exit at startup with "Configuration error: unknown field".
    // Scrub the prefix so the harness only ever configures zebrad through the
    // config file it writes.
    for (key, _) in std::env::vars_os() {
        if key.to_string_lossy().starts_with("ZEBRA_") {
            cmd.env_remove(key);
        }
    }
    cmd.args(["--config", config_path.to_str().unwrap(), "start"])
        .stdout(out)
        .stderr(err)
        .spawn()
        .with_context(|| format!("spawn zebrad ({})", bin.display()))
}

impl Zebrad {
    /// Launch `zebrad` in Regtest mode (mining to a throwaway address) and
    /// wait until its JSON-RPC answers.
    pub async fn start(bin: &Path) -> Result<Zebrad> {
        Self::start_with_miner(bin, DEFAULT_MINER_ADDRESS).await
    }

    /// Launch `zebrad` mining its coinbase to `miner_address`, so a wallet
    /// that controls that address can spend the matured coinbase (used to
    /// fund the wallet under test).
    pub async fn start_with_miner(bin: &Path, miner_address: &str) -> Result<Zebrad> {
        let dir = tempfile::tempdir().context("create zebrad dir")?;
        let rpc_port = pick_port()?;
        let net_port = pick_port()?;
        let config_path = dir.path().join("zebrad.toml");
        let cache_dir = dir.path().join("state");
        std::fs::write(
            &config_path,
            zebrad_toml(
                net_port,
                rpc_port,
                miner_address,
                &cache_dir.to_string_lossy(),
            ),
        )
        .context("write zebrad.toml")?;
        let child = spawn_zebrad(bin, &config_path)?;
        let mut zebrad = Zebrad {
            child,
            rpc_port,
            net_port,
            bin: bin.to_path_buf(),
            config_path,
            _dir: dir,
        };
        zebrad.wait_until_rpc_up().await?;
        Ok(zebrad)
    }

    /// Restart `zebrad` mining to a different address, preserving the chain
    /// (persistent state). Used by the funded e2e to stop minting coinbases to
    /// the funder so its existing coinbases can age past maturity while a
    /// throwaway address mines the tail.
    pub async fn restart_with_miner(&mut self, miner_address: &str) -> Result<()> {
        // Clean shutdown via the regtest `stop` RPC (raises SIGINT) so zebra
        // backs up its non-finalized state. A SIGKILL would drop the recent,
        // not-yet-finalized blocks and reset the chain to genesis, losing the
        // funder's coinbases.
        let _ = self.rpc("stop", json!([])).await;
        let deadline = Instant::now() + Duration::from_secs(60);
        loop {
            match self.child.try_wait() {
                Ok(Some(_)) => break,
                Ok(None) if Instant::now() < deadline => {
                    tokio::time::sleep(Duration::from_millis(200)).await;
                }
                _ => {
                    let _ = self.child.kill();
                    let _ = self.child.wait();
                    break;
                }
            }
        }
        let cache_dir = self._dir.path().join("state");
        std::fs::write(
            &self.config_path,
            zebrad_toml(
                self.net_port,
                self.rpc_port,
                miner_address,
                &cache_dir.to_string_lossy(),
            ),
        )
        .context("rewrite zebrad.toml for restart")?;
        self.child = spawn_zebrad(&self.bin, &self.config_path)?;
        self.wait_until_rpc_up().await?;
        Ok(())
    }

    fn rpc_url(&self) -> String {
        format!("http://127.0.0.1:{}/", self.rpc_port)
    }

    async fn wait_until_rpc_up(&mut self) -> Result<()> {
        let deadline = Instant::now() + Duration::from_secs(120);
        let mut last_err = anyhow!("no getblocktemplate attempt completed");
        loop {
            // A dead zebrad can never become mineable; fail immediately with
            // the exit status instead of burning the whole timeout on
            // connection-refused.
            if let Ok(Some(status)) = self.child.try_wait() {
                bail!(
                    "zebrad exited during startup ({status}); \
                     set ZEBRAD_STDERR=<file> to capture its logs"
                );
            }
            // `getblocktemplate` succeeds only once zebra's RPC is up *and* it
            // considers itself synced to the chain tip (mempool active), which
            // is exactly the precondition for `generate_blocks`. On a fresh
            // node this readiness lags RPC availability by a moment, so poll
            // the template endpoint itself rather than `getblockchaininfo`.
            match zebra_rpc_call(&self.rpc_url(), "getblocktemplate", json!([])).await {
                Ok(_) => return Ok(()),
                Err(e) => last_err = e,
            }
            if Instant::now() >= deadline {
                bail!("zebrad did not become mineable within 120s; last error: {last_err:#}");
            }
            tokio::time::sleep(Duration::from_millis(500)).await;
        }
    }

    /// Issue a raw JSON-RPC call to this zebrad (test/diagnostic helper).
    pub async fn rpc(&self, method: &str, params: Value) -> Result<Value> {
        zebra_rpc_call(&self.rpc_url(), method, params).await
    }

    /// Mine `n` blocks via zebrad's Regtest-only `generate` RPC (zebra >=
    /// 2.0.0). Server-side it runs the same `getblocktemplate` -> assemble ->
    /// `submitblock` flow zebra's own regtest tests use, against the node's
    /// own network parameters, so the harness needs no zebra crates and can't
    /// drift from the running node's consensus rules. Regtest disables PoW, so
    /// there is no solving step.
    pub async fn generate_blocks(&self, n: u32) -> Result<()> {
        let hashes = zebra_rpc_call(&self.rpc_url(), "generate", json!([n]))
            .await
            .context("generate")?;
        // `generate` returns the array of mined block hashes; a short array
        // means some block was rejected. Fail loudly so the chain can't
        // silently stop advancing.
        let mined = hashes.as_array().map(|a| a.len()).unwrap_or(0);
        if mined != n as usize {
            bail!("generate mined {mined} of {n} requested blocks: {hashes}");
        }
        Ok(())
    }

    /// The current best-chain tip height as zebra reports it.
    pub async fn tip_height(&self) -> Result<u64> {
        let info = self.rpc("getblockchaininfo", json!([])).await?;
        info.get("blocks")
            .and_then(|b| b.as_u64())
            .ok_or_else(|| anyhow!("getblockchaininfo.blocks missing: {info}"))
    }
}

impl Drop for Zebrad {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

/// zebrad Regtest config. `disable_pow = true` so submitted blocks need no
/// PoW, and `enable_cookie_auth = false` so lightwalletd can use the
/// rpcuser/rpcpassword from its `zcash.conf`. The activation heights must
/// match zkv's `network::Network::Regtest`.
fn zebrad_toml(net_port: u16, rpc_port: u16, miner_address: &str, cache_dir: &str) -> String {
    let nu6_2 = NU6_2_ACTIVATION_HEIGHT;
    let lockbox_addr = LOCKBOX_DISBURSEMENT_ADDR;
    let lockbox_amount = LOCKBOX_DISBURSEMENT_ZATS;
    // Ironwood tier only: stock zebra rejects the unknown "NU6.3" key at startup,
    // so emit it solely when running against an Ironwood-capable zebra.
    let nu6_3_line = match nu6_3_height() {
        Some(h) => format!("\n\"NU6.3\" = {h}"),
        None => String::new(),
    };
    format!(
        r#"[network]
network = "Regtest"
listen_addr = "127.0.0.1:{net_port}"

[network.testnet_parameters]
disable_pow = true

# NU5/NU6 from genesis, then NU6.1+NU6.2 at NU6_2_ACTIVATION_HEIGHT. NU6.1
# can't activate at height 1: its activation block must carry ZIP-271 one-time
# lockbox disbursements, and the deferred (lockbox) pool only accrues once NU6
# is active, so we let NU6 run for a few blocks to build a pool, then disburse
# a token amount at the NU6.1/NU6.2 activation block. zebra's getblocktemplate
# emits the disbursement output automatically from the config below.
# zkv's regtest network (crates/zkv/src/network.rs) must match these heights.
[network.testnet_parameters.activation_heights]
NU5 = 1
NU6 = 1
"NU6.1" = {nu6_2}
"NU6.2" = {nu6_2}{nu6_3_line}

# A deferred (lockbox) funding stream so the pool has something to disburse at NU6.1.
[[network.testnet_parameters.funding_streams]]
[network.testnet_parameters.funding_streams.height_range]
start = 1
end = 1_000_000
[[network.testnet_parameters.funding_streams.recipients]]
receiver = "Deferred"
numerator = 12
addresses = []

# The ZIP-271 one-time disbursement paid at the NU6.1 activation block. The
# amount need only be <= the pool accrued by then; the residual stays in the
# lockbox.
[[network.testnet_parameters.lockbox_disbursements]]
address = "{lockbox_addr}"
amount = {lockbox_amount}

[mining]
miner_address = "{miner_address}"

[state]
# Persistent (not ephemeral) so the chain survives a restart with a different
# miner address: the funded e2e mines the funder's coinbases, then restarts
# mining to a throwaway address to age them past coinbase maturity (see
# Zebrad::restart_with_miner).
ephemeral = false
cache_dir = "{cache_dir}"

[rpc]
listen_addr = "127.0.0.1:{rpc_port}"
enable_cookie_auth = false
"#
    )
}

// =============================== lightwalletd (indexer) ===============================

/// A running `lightwalletd` pointed at a regtest zebrad. zkv speaks
/// lightwalletd's gRPC (`CompactTxStreamer`), so every zkv command in the
/// harness goes through this.
pub struct Lightwalletd {
    child: Child,
    /// gRPC port serving the lightwalletd `CompactTxStreamer` protocol.
    pub grpc_port: u16,
    _dir: tempfile::TempDir,
}

impl Lightwalletd {
    /// Launch `lightwalletd` against the given zebrad RPC port and wait until
    /// its gRPC server is up.
    pub async fn start(bin: &Path, zebrad_rpc_port: u16) -> Result<Lightwalletd> {
        let grpc_port = pick_port()?;
        let dir = tempfile::tempdir().context("create lightwalletd dir")?;
        let http_port = pick_port()?;
        let data_dir = dir.path().join("data");
        std::fs::create_dir_all(&data_dir)?;

        // lightwalletd reads the node's RPC connection details from a
        // zcash.conf-style file.
        let zcash_conf = dir.path().join("zcash.conf");
        std::fs::write(
            &zcash_conf,
            format!(
                "rpcuser=zkvtest\nrpcpassword=zkvtest\nrpcbind=127.0.0.1\nrpcport={zebrad_rpc_port}\n"
            ),
        )
        .context("write zcash.conf")?;

        let log_file = dir.path().join("lightwalletd.log");
        let child = Command::new(bin)
            .args([
                "--no-tls-very-insecure",
                "--grpc-bind-addr",
                &format!("127.0.0.1:{grpc_port}"),
                "--http-bind-addr",
                &format!("127.0.0.1:{http_port}"),
                "--data-dir",
                data_dir.to_str().unwrap(),
                "--log-file",
                log_file.to_str().unwrap(),
                "--zcash-conf-path",
                zcash_conf.to_str().unwrap(),
            ])
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .with_context(|| format!("spawn lightwalletd ({})", bin.display()))?;

        let lwd = Lightwalletd {
            child,
            grpc_port,
            _dir: dir,
        };
        lwd.wait_until_ready(&log_file).await?;
        Ok(lwd)
    }

    async fn wait_until_ready(&self, log_file: &Path) -> Result<()> {
        let deadline = Instant::now() + Duration::from_secs(90);
        loop {
            if let Ok(log) = std::fs::read_to_string(log_file) {
                if log.contains("Starting insecure no-TLS (plaintext) server") {
                    return Ok(());
                }
            }
            if Instant::now() >= deadline {
                let log = std::fs::read_to_string(log_file).unwrap_or_default();
                bail!(
                    "lightwalletd did not become ready within 90s; log tail:\n{}",
                    tail(&log, 20)
                );
            }
            tokio::time::sleep(Duration::from_millis(500)).await;
        }
    }
}

impl Drop for Lightwalletd {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

// =============================== funder (zcash-devtool) ===============================

/// A valid 24-word BIP-39 test mnemonic (the canonical all-zero-entropy
/// vector). Regtest only: it controls throwaway coinbase funds, never
/// anything of value.
const FUNDER_MNEMONIC: &str = "abandon abandon abandon abandon abandon abandon abandon abandon \
abandon abandon abandon abandon abandon abandon abandon abandon abandon abandon abandon abandon \
abandon abandon abandon art";

/// Drives the `zcash-devtool` binary as a funding wallet. It controls zebra's
/// coinbase (which is mined to its address), shields the matured transparent
/// coinbase into Orchard, and sends Orchard TAZ to the zkv wallet's funding
/// address. Resolve the binary via `$DEVTOOL_BIN`.
///
/// Regtest can't mine a coinbase straight into an Orchard note, so this is
/// how funds reach zkv: mine transparent coinbase -> mature (100 blocks) ->
/// shield to Orchard -> send to zkv's funding UA.
pub struct Funder {
    bin: PathBuf,
    dir: tempfile::TempDir,
}

impl Funder {
    /// Derive the funder's default transparent address offline (no chain, no
    /// wallet) from its fixed mnemonic, so zebra can be told to mine its
    /// coinbase here *before* any chain exists. Mining straight to the funder
    /// keeps everything on one chain, so the wallet's birthday anchor stays
    /// valid (a throwaway chain would hand the wallet a wrong note-commitment
    /// anchor).
    pub fn derive_transparent_address(bin: &Path) -> Result<String> {
        let output = Command::new(bin)
            .args([
                "wallet",
                "derive-address",
                "--network",
                "regtest",
                "--mnemonic",
                FUNDER_MNEMONIC,
            ])
            .output()
            .context("spawn devtool derive-address")?;
        if !output.status.success() {
            bail!(
                "devtool derive-address failed ({}): {}",
                output.status,
                String::from_utf8_lossy(&output.stderr)
            );
        }
        let out = String::from_utf8_lossy(&output.stdout);
        out.lines()
            .find_map(|line| line.split("Transparent Address:").nth(1))
            .map(|addr| addr.trim().to_string())
            .ok_or_else(|| anyhow!("no Transparent Address in derive-address output:\n{out}"))
    }

    /// Initialise the funding wallet against a lightwalletd. `--birthday 2` is
    /// the lowest height with a tree state (init fetches `GetTreeState(birthday-1)`,
    /// which needs a real block; birthday 0/1 requests genesis and is rejected).
    /// The funder's transparent coinbase is detected regardless of birthday.
    pub fn init(bin: &Path, lwd_port: u16) -> Result<Funder> {
        let dir = tempfile::tempdir().context("create funder dir")?;
        let funder = Funder {
            bin: bin.to_path_buf(),
            dir,
        };
        let identity = funder.identity();
        let mut args: Vec<String> = vec![
            "--name".into(),
            "funder".into(),
            "--network".into(),
            "regtest".into(),
            "--identity".into(),
            identity,
            "--birthday".into(),
            "2".into(),
        ];
        // Two devtool generations differ in how `wallet init` takes the seed and
        // the regtest activation heights, so gate both on the Ironwood-tier
        // marker (see `nu6_3_height`):
        //   - Ironwood devtool: `wallet init` no longer accepts `--mnemonic`; it
        //     reads the phrase from stdin when stdin is not a terminal. It also
        //     *requires* `--activation-heights` for `-n regtest` (rejecting it
        //     otherwise); the heights must match the zebrad config and zkv's
        //     `network::Network::Regtest` exactly, or NU6.3 consensus diverges.
        //   - Stock (pre-Ironwood) devtool: takes `--mnemonic` and has no
        //     `--activation-heights` flag.
        let stdin = if nu6_3_height().is_some() {
            let path = funder.write_activation_heights()?;
            args.push("--activation-heights".into());
            args.push(path);
            Some(format!("{FUNDER_MNEMONIC}\n"))
        } else {
            args.push("--mnemonic".into());
            args.push(FUNDER_MNEMONIC.into());
            None
        };
        let arg_refs: Vec<&str> = args.iter().map(String::as_str).collect();
        funder.run_with_stdin("init", &arg_refs, Some(lwd_port), stdin.as_deref())?;
        Ok(funder)
    }

    /// Write the regtest activation-heights TOML the Ironwood devtool's
    /// `wallet init --activation-heights <file>` consumes (the
    /// `ActivationHeights` schema: one optional height per upgrade). Must match
    /// the zebrad config ([`zebrad_toml`]) and zkv's `network::Network::Regtest`
    /// heights, or the funder's transactions carry the wrong consensus branch id.
    fn write_activation_heights(&self) -> Result<String> {
        let path = self.dir.path().join("activation-heights.toml");
        let nu6_2 = NU6_2_ACTIVATION_HEIGHT;
        let nu6_3 = NU6_3_ACTIVATION_HEIGHT;
        let toml = format!(
            "overwinter = 1\nsapling = 1\nblossom = 1\nheartwood = 1\ncanopy = 1\n\
             nu5 = 1\nnu6 = 1\nnu6_1 = {nu6_2}\nnu6_2 = {nu6_2}\nnu6_3 = {nu6_3}\n"
        );
        std::fs::write(&path, toml).context("write activation-heights.toml")?;
        Ok(path.to_string_lossy().into_owned())
    }

    fn identity(&self) -> String {
        self.dir
            .path()
            .join("identity.txt")
            .to_string_lossy()
            .into_owned()
    }

    fn wallet_dir(&self) -> String {
        self.dir.path().to_string_lossy().into_owned()
    }

    /// Scan the chain via lightwalletd to pick up new transactions / UTXOs.
    pub fn sync(&self, lwd_port: u16) -> Result<()> {
        self.run("sync", &[], Some(lwd_port)).map(|_| ())
    }

    /// Shield all spendable transparent funds (the matured coinbase) into Orchard.
    pub fn shield(&self, lwd_port: u16) -> Result<()> {
        let identity = self.identity();
        self.run("shield", &["--identity", &identity], Some(lwd_port))
            .map(|_| ())
    }

    /// Send `zatoshis` to `to_address` (a shielded/unified address).
    pub fn send(&self, lwd_port: u16, to_address: &str, zatoshis: u64) -> Result<()> {
        let identity = self.identity();
        let value = zatoshis.to_string();
        self.run(
            "send",
            &[
                "--identity",
                &identity,
                "--address",
                to_address,
                "--value",
                &value,
            ],
            Some(lwd_port),
        )
        .map(|_| ())
    }

    /// Run `zcash-devtool wallet -w <dir> <subcommand> <extra...> [--server .. --connection direct]`.
    fn run(&self, subcommand: &str, extra: &[&str], lwd_port: Option<u16>) -> Result<String> {
        self.run_with_stdin(subcommand, extra, lwd_port, None)
    }

    /// Like [`Funder::run`], but optionally pipes `stdin_input` to the child's
    /// stdin. The Ironwood devtool's `wallet init` reads its mnemonic from
    /// stdin (it no longer takes `--mnemonic`).
    fn run_with_stdin(
        &self,
        subcommand: &str,
        extra: &[&str],
        lwd_port: Option<u16>,
        stdin_input: Option<&str>,
    ) -> Result<String> {
        let mut args: Vec<String> = vec![
            "wallet".into(),
            "-w".into(),
            self.wallet_dir(),
            subcommand.into(),
        ];
        args.extend(extra.iter().map(|s| s.to_string()));
        if let Some(port) = lwd_port {
            args.extend([
                "--server".into(),
                format!("127.0.0.1:{port}"),
                "--connection".into(),
                "direct".into(),
            ]);
        }
        let mut cmd = Command::new(&self.bin);
        cmd.args(&args);
        let output = if let Some(input) = stdin_input {
            cmd.stdin(Stdio::piped());
            cmd.stdout(Stdio::piped());
            cmd.stderr(Stdio::piped());
            let mut child = cmd
                .spawn()
                .with_context(|| format!("spawn devtool {subcommand}"))?;
            child
                .stdin
                .take()
                .expect("piped stdin")
                .write_all(input.as_bytes())
                .with_context(|| format!("write stdin to devtool {subcommand}"))?;
            child
                .wait_with_output()
                .with_context(|| format!("wait for devtool {subcommand}"))?
        } else {
            cmd.output()
                .with_context(|| format!("spawn devtool {subcommand}"))?
        };
        if !output.status.success() {
            bail!(
                "devtool {subcommand} failed ({}):\nstdout: {}\nstderr: {}",
                output.status,
                String::from_utf8_lossy(&output.stdout),
                tail(&String::from_utf8_lossy(&output.stderr), 30),
            );
        }
        Ok(String::from_utf8_lossy(&output.stdout).into_owned())
    }
}

// =============================== zkv (the system under test) ===============================

/// Locate the built `zkv` binary: `$ZKV_BIN` if set, else the parent
/// workspace's release build.
pub fn zkv_bin() -> PathBuf {
    if let Ok(p) = std::env::var("ZKV_BIN") {
        return PathBuf::from(p);
    }
    let manifest = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    manifest
        .parent()
        .map(|p| p.join("target/release/zkv"))
        .unwrap_or_else(|| PathBuf::from("zkv"))
}

/// The captured result of one `zkv` invocation.
pub struct ZkvOutput {
    pub status_ok: bool,
    pub stdout: String,
    pub stderr: String,
}

/// Drives the real `zkv` CLI binary against an isolated data directory and a
/// local regtest lightwalletd. Every method is a fresh subprocess, exactly as
/// a user runs the tool; nothing shortcuts through the library.
pub struct Zkv {
    bin: PathBuf,
    data_dir: tempfile::TempDir,
    /// `host:port` of the regtest lightwalletd, passed as `--server` to every
    /// chain-touching command.
    server: String,
}

impl Zkv {
    /// Set up an isolated zkv home pointed at the given lightwalletd gRPC
    /// port. Pre-creates the `.demo-oracles-provisioned` marker so the CLI's
    /// one-time demo auto-provision (which would dial the public testnet on
    /// every command until it succeeds) never fires in the sandboxed run.
    pub fn new(lwd_grpc_port: u16) -> Result<Zkv> {
        let bin = zkv_bin();
        if !bin.is_file() {
            bail!(
                "zkv binary not found at {} - build it first (cargo build --release -p zkv \
                 --bin zkv) or set $ZKV_BIN",
                bin.display()
            );
        }
        let data_dir = tempfile::tempdir().context("create zkv data dir")?;
        std::fs::write(data_dir.path().join(".demo-oracles-provisioned"), b"")
            .context("write demo marker")?;
        Ok(Zkv {
            bin,
            data_dir,
            server: format!("127.0.0.1:{lwd_grpc_port}"),
        })
    }

    /// The data directory (owned by this handle; deleted when it drops).
    pub fn data_dir(&self) -> &Path {
        self.data_dir.path()
    }

    fn command(&self, db: Option<&str>, args: &[&str]) -> Command {
        let mut cmd = Command::new(&self.bin);
        cmd.arg("--data-dir").arg(self.data_dir.path());
        if let Some(db) = db {
            cmd.args(["--db", db]);
        }
        cmd.args(args);
        cmd
    }

    /// Run an offline `zkv` command (no `--server`), capturing output.
    pub fn run_local(&self, db: Option<&str>, args: &[&str]) -> Result<ZkvOutput> {
        let out = self
            .command(db, args)
            .output()
            .with_context(|| format!("spawn zkv {args:?}"))?;
        Ok(capture(out))
    }

    /// Run a chain-touching `zkv` command with `--server` appended, capturing
    /// output.
    pub fn run_online(&self, db: Option<&str>, args: &[&str]) -> Result<ZkvOutput> {
        let mut full: Vec<&str> = args.to_vec();
        full.extend_from_slice(&["--server", &self.server]);
        self.run_local(db, &full)
    }

    /// Like [`Zkv::run_online`] but errors (with both streams attached)
    /// unless the command exited 0. Returns stdout.
    pub fn ok_online(&self, db: Option<&str>, args: &[&str]) -> Result<String> {
        let out = self.run_online(db, args)?;
        ensure_ok(&out, args)?;
        Ok(out.stdout)
    }

    /// Like [`Zkv::run_local`] but errors unless the command exited 0.
    /// Returns stdout.
    pub fn ok_local(&self, db: Option<&str>, args: &[&str]) -> Result<String> {
        let out = self.run_local(db, args)?;
        ensure_ok(&out, args)?;
        Ok(out.stdout)
    }

    /// Create a fresh regtest database non-interactively (no INIT broadcast
    /// yet; the wallet is unfunded). Returns `(zkv_address, funding_ua)`.
    pub fn create_db(&self, name: &str) -> Result<(String, String)> {
        self.ok_online(
            None,
            &["init", name, "--network", "regtest", "--non-interactive"],
        )
        .context("zkv init --non-interactive")?;
        let addr = self.address(name)?;
        let funding = self.funding_address(name)?;
        Ok((addr, funding))
    }

    /// The database's `zkvregtest1...` address (offline read).
    pub fn address(&self, name: &str) -> Result<String> {
        Ok(self.ok_local(Some(name), &["address"])?.trim().to_owned())
    }

    /// The database's shielded funding UA (offline read).
    pub fn funding_address(&self, name: &str) -> Result<String> {
        Ok(self
            .ok_local(Some(name), &["address", "--funding"])?
            .trim()
            .to_owned())
    }

    /// Resume `zkv init` on a funded database to broadcast INIT and wait for
    /// its confirmation, mining a block every couple of seconds so the poll
    /// loop can make progress. `zkv init`'s poll interval is 15s, so this
    /// completes in a few cycles.
    pub async fn init_until_confirmed(
        &self,
        name: &str,
        zebrad: &Zebrad,
        timeout: Duration,
    ) -> Result<()> {
        // Inherit stderr so the poll loop's status lines land in the test log
        // (visible with --nocapture); stdout carries only funding info we
        // already have.
        let mut child = self
            .command(None, &["init", name, "--init-timeout", "600"])
            .args(["--server", &self.server])
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::inherit())
            .spawn()
            .context("spawn zkv init (resume)")?;
        let deadline = Instant::now() + timeout;
        loop {
            if let Some(status) = child.try_wait().context("wait on zkv init")? {
                if status.success() {
                    return Ok(());
                }
                bail!("zkv init (resume) exited with {status}");
            }
            if Instant::now() >= deadline {
                let _ = child.kill();
                let _ = child.wait();
                bail!("zkv init did not confirm INIT within {timeout:?}");
            }
            zebrad
                .generate_blocks(1)
                .await
                .context("mine while waiting for INIT")?;
            tokio::time::sleep(Duration::from_secs(2)).await;
        }
    }

    /// `zkv set <key> <value>`; returns the broadcast txid.
    pub fn set(&self, name: &str, key: &str, value: &str) -> Result<String> {
        Ok(self
            .ok_online(Some(name), &["set", key, value])?
            .trim()
            .to_owned())
    }

    /// `zkv del <key>`; returns the broadcast txid.
    pub fn del(&self, name: &str, key: &str) -> Result<String> {
        Ok(self.ok_online(Some(name), &["del", key])?.trim().to_owned())
    }

    /// `zkv get <key> --output json -c <confirmations>`: `Ok(Some(value))`
    /// for a confirmed value, `Ok(None)` when the key has no confirmed value,
    /// `Err` for anything else (not initialized, sync failure, ...).
    pub fn get(&self, name: &str, key: &str, confirmations: u32) -> Result<Option<String>> {
        let confs = confirmations.to_string();
        let out = self.run_online(
            Some(name),
            &["get", key, "--output", "json", "--confirmations", &confs],
        )?;
        // The command exits 1 for a key without a confirmed value but still
        // prints the JSON record; only a run with no JSON on stdout is a real
        // failure.
        let parsed: Value = match serde_json::from_str(out.stdout.trim()) {
            Ok(v) => v,
            Err(_) => bail!(
                "zkv get {key} produced no JSON (exit ok: {}):\nstdout: {}\nstderr: {}",
                out.status_ok,
                out.stdout,
                tail(&out.stderr, 15),
            ),
        };
        Ok(parsed
            .get("value")
            .and_then(|v| v.as_str())
            .map(|s| s.to_owned()))
    }

    /// `zkv keys <pattern> --output json`: the matching key names.
    pub fn keys(&self, name: &str, pattern: &str, confirmations: u32) -> Result<Vec<String>> {
        let confs = confirmations.to_string();
        let stdout = self.ok_online(
            Some(name),
            &[
                "keys",
                pattern,
                "--output",
                "json",
                "--confirmations",
                &confs,
            ],
        )?;
        let parsed: Value = serde_json::from_str(stdout.trim()).context("parse keys JSON")?;
        Ok(parsed
            .get("keys")
            .and_then(|k| k.as_array())
            .map(|a| {
                a.iter()
                    .filter_map(|v| v.as_str().map(|s| s.to_owned()))
                    .collect()
            })
            .unwrap_or_default())
    }

    /// `zkv history --all --output json -c <confirmations>`, parsed.
    pub fn history_json(&self, name: &str, confirmations: u32) -> Result<Value> {
        let confs = confirmations.to_string();
        let stdout = self.ok_online(
            Some(name),
            &[
                "history",
                "--all",
                "--output",
                "json",
                "--confirmations",
                &confs,
            ],
        )?;
        serde_json::from_str(stdout.trim()).context("parse history JSON")
    }

    /// `zkv roles -c <confirmations>` with stdout piped, so the stable
    /// one-record-per-line machine format is returned.
    pub fn roles_raw(&self, name: &str, confirmations: u32) -> Result<String> {
        let confs = confirmations.to_string();
        self.ok_online(Some(name), &["roles", "--confirmations", &confs])
    }

    /// `zkv balance`: the total balance in TAZ, as printed on stdout
    /// (`<decimal> TAZ`; only the number is returned).
    pub fn balance(&self, name: &str) -> Result<f64> {
        let stdout = self.ok_online(Some(name), &["balance"])?;
        stdout
            .split_whitespace()
            .next()
            .unwrap_or("")
            .parse::<f64>()
            .with_context(|| format!("parse balance {stdout:?}"))
    }

    /// `zkv watch <address> <name>`: import a watch-only replica.
    pub fn watch(&self, address: &str, name: &str) -> Result<()> {
        self.ok_online(None, &["watch", address, name]).map(|_| ())
    }

    /// `zkv inspect <address> --output json` (offline), parsed.
    pub fn inspect_json(&self, address: &str) -> Result<Value> {
        let stdout = self.ok_local(None, &["inspect", address, "--output", "json"])?;
        serde_json::from_str(stdout.trim()).context("parse inspect JSON")
    }

    /// `zkv shallow get <key> --address <addr>`: the db-less shallow read.
    /// Returns the bare value printed for a single exact key.
    pub fn shallow_get(&self, address: &str, key: &str, max_depth: u32) -> Result<String> {
        let depth = max_depth.to_string();
        let stdout = self.ok_online(
            None,
            &[
                "shallow",
                "get",
                key,
                "--address",
                address,
                "--max-depth",
                &depth,
                "--confirmations",
                "1",
            ],
        )?;
        Ok(stdout.trim().to_owned())
    }
}

fn capture(out: Output) -> ZkvOutput {
    ZkvOutput {
        status_ok: out.status.success(),
        stdout: String::from_utf8_lossy(&out.stdout).into_owned(),
        stderr: String::from_utf8_lossy(&out.stderr).into_owned(),
    }
}

fn ensure_ok(out: &ZkvOutput, args: &[&str]) -> Result<()> {
    if out.status_ok {
        return Ok(());
    }
    bail!(
        "zkv {args:?} failed:\nstdout: {}\nstderr: {}",
        out.stdout,
        tail(&out.stderr, 25),
    )
}

// =============================== helpers ===============================

/// JSON-RPC 2.0 call to zebrad; returns the `result` or an error carrying the
/// message.
async fn zebra_rpc_call(url: &str, method: &str, params: Value) -> Result<Value> {
    let body = json!({ "jsonrpc": "2.0", "id": 1, "method": method, "params": params });
    let resp = reqwest::Client::new()
        .post(url)
        .json(&body)
        .send()
        .await
        .context("zebra rpc request")?;
    let envelope: Value = resp.json().await.context("decode zebra rpc response")?;
    if let Some(err) = envelope.get("error").filter(|e| !e.is_null()) {
        bail!("zebra rpc error from {method}: {err}");
    }
    Ok(envelope.get("result").cloned().unwrap_or(Value::Null))
}

fn tail(s: &str, lines: usize) -> String {
    let all: Vec<&str> = s.lines().collect();
    all[all.len().saturating_sub(lines)..].join("\n")
}
