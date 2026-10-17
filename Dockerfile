# syntax=docker/dockerfile:1
FROM rust:1.99.1-bookworm@sha256:e12566d5abc17a0da741628782cdb504b68b8961f5bfcf677936bf3e8b32de29 AS builder
ARG CARGO_BUILD_JOBS=3
WORKDIR /app
COPY Cargo.toml Cargo.lock rust-toolchain.toml ./
COPY src ./src
COPY web ./web
# Optional session trust mounts are never retained in an image layer.
RUN --mount=type=cache,target=/usr/local/cargo/registry \
    --mount=type=cache,target=/usr/local/cargo/git \
    --mount=type=cache,target=/app/target \
    --mount=type=secret,id=system_ca \
    --mount=type=secret,id=proxy_ca \
    set -eu; \
    cat /etc/ssl/certs/ca-certificates.crt > /tmp/build-ca.pem; \
    for cert in /run/secrets/system_ca /run/secrets/proxy_ca; do \
        if [ -f "$cert" ]; then cat "$cert" >> /tmp/build-ca.pem; fi; \
    done; \
    SSL_CERT_FILE=/tmp/build-ca.pem CURL_CA_BUNDLE=/tmp/build-ca.pem \
    CARGO_HTTP_CAINFO=/tmp/build-ca.pem CARGO_BUILD_JOBS="$CARGO_BUILD_JOBS" \
    RUSTUP_USE_CURL=1 cargo build --locked --release; \
    install -D target/release/youtube-extractor /out/youtube-extractor; \
    rm /tmp/build-ca.pem

FROM debian:bookworm-slim AS runtime
RUN --mount=type=secret,id=system_ca \
    --mount=type=secret,id=proxy_ca \
    set -eu; \
    touch /tmp/build-ca.pem; \
    if [ -f /etc/ssl/certs/ca-certificates.crt ]; then cat /etc/ssl/certs/ca-certificates.crt >> /tmp/build-ca.pem; fi; \
    for cert in /run/secrets/system_ca /run/secrets/proxy_ca; do \
        if [ -f "$cert" ]; then cat "$cert" >> /tmp/build-ca.pem; fi; \
    done; \
    apt-get -o Acquire::https::CaInfo=/tmp/build-ca.pem update; \
    apt-get -o Acquire::https::CaInfo=/tmp/build-ca.pem install -y --no-install-recommends ca-certificates curl ffmpeg; \
    rm -rf /var/lib/apt/lists/* /tmp/build-ca.pem; \
    groupadd --gid 10001 extractor; \
    useradd --uid 10001 --gid extractor --no-create-home --shell /usr/sbin/nologin extractor; \
    install -d -o extractor -g extractor -m 0700 /data
COPY --from=builder /out/youtube-extractor /usr/local/bin/youtube-extractor
ENV PORT=8080 DATA_DIR=/data RUST_LOG=info
USER 10001:10001
EXPOSE 8080
HEALTHCHECK --interval=30s --timeout=5s --start-period=10s --retries=3 \
    CMD curl --fail --silent --show-error --max-time 4 "http://127.0.0.1:${PORT:-8080}/health" || exit 1
ENTRYPOINT ["/usr/local/bin/youtube-extractor"]
