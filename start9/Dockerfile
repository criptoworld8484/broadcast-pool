# StartOS image for broadcast-pool.
#
# Idiomatic StartOS packaging builds the image from a prebuilt artifact rather than
# from repo source (the s9pk build context is this package dir, start9/, which does
# not contain the Rust crate). We reuse the already-published binary image as the base
# layer and only overlay the StartOS entrypoint + env. The base image carries the
# broadcast-pool binary (/usr/local/bin/broadcast-pool) and the dashboard.
#
# NOTE: the base image is currently linux/amd64 only, so this package is x86_64 only.
# Multi-arch (aarch64) is a follow-up: publish a multi-arch base image or per-arch
# release binaries and download the matching one here.
FROM ghcr.io/criptoworld8484/broadcast-pool-umbrel:0.3.14

USER root
COPY --chmod=0755 entrypoint.sh /entrypoint.sh

# StartOS data volume + auto-detected network (do NOT pin BROADCAST_POOL_NETWORK).
ENV BROADCAST_POOL_DATA_DIR=/data
ENV BROADCAST_POOL_ELECTRUM_PORT=50050
ENV BROADCAST_POOL_LIANA_ELECTRUM_PORT=50051
ENV BROADCAST_POOL_WEB_PORT=8080

EXPOSE 50051

# StartOS runs service subcontainers as root and mounts /data owned by root, so run
# as root (the base image's trailing `USER app` would hit EPERM writing to /data).
USER root

ENTRYPOINT ["/entrypoint.sh"]
