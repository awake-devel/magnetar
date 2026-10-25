# syntax=docker/dockerfile:1
# magnetar-server image (ARCHITECTURE.md D37, docs/DOCKER.md).
#
#   docker build -t ghcr.io/awake-devel/magnetar:dev .
#
# Builds natively for the build platform (amd64 or arm64; no cross-compilation, no QEMU).
# Base images are pinned by tag and multi-arch index digest; bump both together.
#
# Builder: the official Rust image on Debian 13 (trixie). It ships gcc and the glibc headers
# that ring and aws-lc-rs (librqbit, D28) compile their C/asm with; no cmake or clang needed.
# Runtime: debian:trixie-slim, the same glibc as the builder (no musl allocator penalty, no
# libc mismatch), plus bash/coreutils for the healthcheck and `docker compose exec` debugging.
# distroless/cc would save ~20 MB but has no shell for the healthcheck.
ARG RUST_IMAGE=rust:1.99.0-slim-trixie@sha256:2752b332db73fdbb7dc576f06c82ed1f312005784ef913d7e04a28f5f55dc581
ARG RUNTIME_IMAGE=debian:trixie-slim@sha256:62bd423aeee912639ca8ee681c2b762cb3e25137e0a92b20be455898a9f034ad

FROM ${RUST_IMAGE} AS chef
# cargo-chef turns the workspace manifests into a recipe so dependencies build in their own
# layer: editing our sources reuses it, only Cargo.toml/Cargo.lock changes rebuild it.
RUN cargo install cargo-chef --version 0.1.78 --locked \
    && rm -rf "$CARGO_HOME/registry" "$CARGO_HOME/git"
WORKDIR /src

FROM chef AS planner
COPY Cargo.toml Cargo.lock ./
COPY crates crates
RUN cargo chef prepare --recipe-path recipe.json

FROM chef AS builder
ENV CARGO_PROFILE_RELEASE_STRIP=symbols
COPY --from=planner /src/recipe.json recipe.json
RUN cargo chef cook --release --locked -p magnetar-server --recipe-path recipe.json
COPY Cargo.toml Cargo.lock ./
COPY crates crates
RUN cargo build --release --locked -p magnetar-server --bin magnetar-server \
    && install -D -m 0755 target/release/magnetar-server /out/magnetar-server

FROM ${RUNTIME_IMAGE}
# Same uid/gid on every host; override with `user:` in compose for bind mounts (DOCKER.md).
ARG UID=10001
ARG GID=10001
RUN groupadd --system --gid "$GID" magnetar \
    && useradd --system --uid "$UID" --gid magnetar --home-dir /data --no-create-home \
       --shell /usr/sbin/nologin magnetar \
    && mkdir -p /data/downloads /data/state \
    && chown -R magnetar:magnetar /data
# System CA bundle for librqbit's HTTPS trackers (rustls platform verifier) and future TMDB
# calls; our own clients use webpki-roots (D21). Copied, so the runtime stage needs no apt.
COPY --from=builder /etc/ssl/certs/ca-certificates.crt /etc/ssl/certs/ca-certificates.crt
COPY --from=builder /out/magnetar-server /usr/local/bin/magnetar-server
COPY docker/healthcheck.sh /usr/local/bin/magnetar-healthcheck

# Inside a container the server must listen on all interfaces; what the host exposes is
# decided by the published port (compose: 127.0.0.1:8787 by default).
# State (librqbit session) lands in $XDG_STATE_HOME/magnetar = /data/state/magnetar.
# NO_COLOR: plain `docker logs` (tracing-subscriber writes ANSI colours otherwise).
ENV MAGNETAR_BIND=0.0.0.0:8787 \
    MAGNETAR_DOWNLOAD_DIR=/data/downloads \
    XDG_STATE_HOME=/data/state \
    HOME=/data \
    RUST_LOG=info \
    NO_COLOR=1 \
    SSL_CERT_FILE=/etc/ssl/certs/ca-certificates.crt

USER magnetar
WORKDIR /data
VOLUME ["/data"]
EXPOSE 8787
# librqbit listens for peers/DHT on random ports that are not published: downloads still
# work, with outbound connections only (like any client behind NAT).
HEALTHCHECK --interval=30s --timeout=10s --start-period=60s --retries=3 \
    CMD ["/usr/local/bin/magnetar-healthcheck"]
# `serve` applies migrations before listening. Other subcommands: `docker compose run --rm
# magnetar crawl-once --seconds 30`, `... migrate`, `... selftest`.
ENTRYPOINT ["/usr/local/bin/magnetar-server"]
CMD ["serve"]

LABEL org.opencontainers.image.source="https://github.com/awake-devel/magnetar" \
      org.opencontainers.image.description="Magnetar NIP-35 torrent index server" \
      org.opencontainers.image.licenses="MIT"
