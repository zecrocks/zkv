# zkv-oracle

A production, always-on price oracle built on the [`zkv`](../zkv) library. Every
15 minutes (configurable) it fetches the latest **ZEC/USD** and **BTC/USD** spot
prices from the CoinGecko **Pro** API and publishes them as authenticated memos
to a testnet zkv database, under:

```
rates/zec_usd/coingecko
rates/btc_usd/coingecko
```

The provider is the **final** key segment, so additional providers can be added
later (`rates/zec_usd/<other>`) without disturbing the CoinGecko feed. Values are
written as bare decimal strings (e.g. `23.4`).

Anyone holding the database's zkv address (UFVK) can read the latest values by
syncing and reading those keys; see the library's `read_value` example.

## How it works

- Opens the database once; on a fresh data dir it restores the admin wallet from
  the seed, taking the network, birthday, and shielded pool from `ZKV_ADDRESS`
  (`Database::restore_admin_with_pool`). It does **not** broadcast INIT; the
  database is assumed already created and INITed on-chain for this seed.
- Each tick: fetch both prices → one `sync()` → `set_no_sync()` each key. A
  single sync covers both writes.
- A transient fetch/sync/write failure is retried inside the same tick (up to
  `ZKV_TICK_ATTEMPTS`, `ZKV_RETRY_BACKOFF_SECS` apart) instead of going dark
  until the next tick. Every retry **re-fetches** the price, so a retried write
  never publishes a stale quote, and only keys that have not yet landed are
  re-written (a key that already succeeded is not paid for twice). A tick never
  crashes the process; on exhausted retries it just waits for the next tick.
- Exposes a **health gate** (`GET /health`, also `/healthz` and `/`) that returns
  HTTP 200 while healthy and 503 once the oracle drifts too far out of sync or
  stops landing writes, so a container orchestrator can restart it.

Health is **healthy** when both:
- the wallet scan is within `ZKV_MAX_LAG_BLOCKS` of the chain tip, and
- a write landed within `ZKV_MAX_WRITE_AGE_SECS`.

During the initial cold-start sync (before the first tick completes) it reports
`200 "starting"`, which Docker's `start_period` covers. Write-freshness only
gates health **after** the first successful write: before any write has landed
(cold start, or a funding/confirmation lull right after a restart) it stays
healthy as long as sync lag is OK, since a restart can't make the chain confirm
or conjure funds. Sync lag is always enforced, so a genuinely stuck wallet is
still caught.

## Configuration (environment variables)

| Var | Required | Default | Meaning |
|---|---|---|---|
| `ZKV_ORACLE_SEED` | ✅ | — | 24-word recovery phrase (admin wallet) |
| `COINGECKO_API_KEY` | ✅ | — | CoinGecko **Pro** API key |
| `ZKV_ADDRESS` | ✅¹ | — | The database's zkv address (`zkvtest1…`). The restore reads the network, birthday, and shielded pool from it |
| `ZKV_DB` | | `oracle-testnet` | Database name |
| `ZKV_DATA` | | `/data` | Data directory (mounted volume) |
| `ZKV_BIRTHDAY` | | from `ZKV_ADDRESS` | Override the restore birthday; normally unset (it rides inside the address) |
| `ZKV_SERVER` | | `zecrocks` | lightwalletd: `zecrocks`/`ecc`/`host:port,…` |
| `ZKV_INTERVAL_SECS` | | `900` | Publish cadence |
| `ZKV_TICK_ATTEMPTS` | | `4` | Max attempts within one tick (re-fetch + sync + write) before waiting for the next tick; `1` disables in-tick retry |
| `ZKV_RETRY_BACKOFF_SECS` | | `30` | Pause between in-tick attempts (capped so retries never overrun the next tick) |
| `ZKV_HEALTH_ADDR` | | `0.0.0.0:8099` | Health gate bind address |
| `ZKV_MAX_LAG_BLOCKS` | | `50` | Unhealthy if scan is this far behind tip |
| `ZKV_MAX_WRITE_AGE_SECS` | | `2 × interval` | Unhealthy if no write in this long |

¹ `ZKV_ADDRESS` is required only the first time, when restoring onto a fresh
`/data` volume: the network, birthday, and pool (Sapling vs Orchard) are decoded
from it so they never have to be repeated by hand, and the restored wallet's
re-derived address is checked against it to catch a seed that belongs to a
different database. Once the wallet exists in `/data` it is opened directly and
`ZKV_ADDRESS` is not consulted.

Logging honours `RUST_LOG` (default `info`).

## Run with docker compose

```sh
cp .env.example .env          # fill in ZKV_ORACLE_SEED + COINGECKO_API_KEY
docker compose up -d --build
docker compose logs -f oracle
curl -fsS http://localhost:8099/health
```

The wallet data persists in the `oracle-data` named volume, so restarts do not
re-scan the chain from the birthday.

### Restart-on-unhealthy

Vanilla `docker compose` (non-Swarm) marks a container `unhealthy` but does not
itself restart it on a failed healthcheck; `restart: unless-stopped` only covers
crashes/exits. For a true restart-on-unhealthy on a single host, add an autoheal
sidecar:

```yaml
  autoheal:
    image: willfarrell/autoheal
    restart: unless-stopped
    environment:
      AUTOHEAL_CONTAINER_LABEL: all
    volumes:
      - /var/run/docker.sock:/var/run/docker.sock
```

Under Docker Swarm, an unhealthy task is rescheduled automatically.

## Run locally (without Docker)

```sh
ZKV_ORACLE_SEED="word1 word2 … word24" \
COINGECKO_API_KEY="CG-…" \
ZKV_DATA=./tmp-data \
ZKV_INTERVAL_SECS=60 \
cargo run -p zkv-oracle

# read it back with the CLI:
zkv --data-dir ./tmp-data --db oracle-testnet get rates/zec_usd/coingecko
```

## Prerequisites

- The seed's wallet holds testnet **TAZ**; each tick spends two ZIP-317 fees
  (one transaction per key).
- The database is already **INITed** on-chain for this seed. Use the `zkv` CLI
  (`zkv init` / faucet) once before running the oracle.
