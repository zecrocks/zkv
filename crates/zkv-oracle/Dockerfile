# Multi-stage build for the zkv-oracle service.
#
# Build context is the workspace root (see docker-compose.yml `build.context`),
# so the whole cargo workspace is available to compile the binary.

# ---- builder ---------------------------------------------------------------
FROM rust:1.81-bookworm AS builder
WORKDIR /build

# SQLite and ring build from source under the default toolchain, and reqwest
# uses rustls, so no system OpenSSL/SQLite dev packages are needed.
COPY . .
RUN cargo build --release -p zkv-oracle

# ---- runtime ---------------------------------------------------------------
FROM debian:bookworm-slim AS runtime

# ca-certificates: TLS to CoinGecko + lightwalletd. curl: container healthcheck.
RUN apt-get update \
    && apt-get install -y --no-install-recommends ca-certificates curl \
    && rm -rf /var/lib/apt/lists/*

COPY --from=builder /build/target/release/zkv-oracle /usr/local/bin/zkv-oracle

# Wallet data (data.sqlite, snapshot, keys.toml) persists here across restarts.
ENV ZKV_DATA=/data
VOLUME ["/data"]

# Health gate.
EXPOSE 8099

ENTRYPOINT ["zkv-oracle"]
