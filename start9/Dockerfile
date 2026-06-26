# StartOS image for broadcast-pool.
# Built by `start-cli s9pk pack` with the manifest's dockerBuild workdir set to the
# REPOSITORY ROOT (start9/startos/manifest/index.ts -> source.dockerBuild.workdir = '..'),
# so all COPY paths below are root-relative — same convention as the Umbrel Dockerfile.
FROM rust:1.86-bookworm AS builder

WORKDIR /build
COPY Cargo.toml Cargo.lock ./
COPY src/ src/

RUN apt-get update && apt-get install -y pkg-config libssl-dev && rm -rf /var/lib/apt/lists/*
RUN cargo build --release && strip target/release/broadcast-pool

FROM debian:bookworm-slim

RUN apt-get update && apt-get install -y ca-certificates && rm -rf /var/lib/apt/lists/*

COPY --from=builder /build/target/release/broadcast-pool /usr/local/bin/broadcast-pool
COPY src/api/dashboard.html /usr/share/broadcast-pool/dashboard.html
COPY start9/entrypoint.sh /entrypoint.sh
RUN chmod +x /entrypoint.sh

# Static, non-network-specific defaults. The network is auto-detected from Bitcoin
# Core at runtime (see entrypoint.sh / discovery::apply_network_from_rpc), so we do
# NOT set BROADCAST_POOL_NETWORK here.
ENV BROADCAST_POOL_DASHBOARD=/usr/share/broadcast-pool/dashboard.html
ENV BROADCAST_POOL_DATA_DIR=/data
ENV BROADCAST_POOL_WEB_HOST=0.0.0.0
ENV BROADCAST_POOL_WEB_PORT=8080
ENV BROADCAST_POOL_ELECTRUM_HOST=0.0.0.0
ENV BROADCAST_POOL_ELECTRUM_PORT=50050

EXPOSE 8080 50050
ENTRYPOINT ["/entrypoint.sh"]
