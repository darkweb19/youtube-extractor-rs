# syntax=docker/dockerfile:1
FROM rust:1.99.1-bookworm@sha256:e12566d5abc17a0da741628782cdb504b68b8961f5bfcf677936bf3e8b32de29 AS builder
ARG CARGO_BUILD_JOBS=3
WORKDIR /app
COPY Cargo.toml Cargo.lock rust-toolchain.toml ./
COPY src ./src
COPY web ./web
RUN set -eu; \
    CARGO_BUILD_JOBS="$CARGO_BUILD_JOBS" cargo build --locked --release; \
    install -D target/release/youtube-extractor /out/youtube-extractor

# yt-dlp uses Node for YouTube JavaScript challenge handling.
FROM node:24-bookworm-slim AS jsruntime

FROM debian:bookworm-slim AS runtime
RUN set -eu; \
    apt-get update; \
    apt-get install -y --no-install-recommends ca-certificates curl ffmpeg python3 python3-venv; \
    rm -rf /var/lib/apt/lists/*; \
    python3 -m venv /opt/yt-dlp; \
    /opt/yt-dlp/bin/pip install --no-cache-dir 'yt-dlp[default]==2026.10.21'; \
    ln -s /opt/yt-dlp/bin/yt-dlp /usr/local/bin/yt-dlp; \
    groupadd --gid 10001 extractor; \
    useradd --uid 10001 --gid extractor --no-create-home --shell /usr/sbin/nologin extractor; \
    install -d -o extractor -g extractor -m 0700 /data
COPY --from=jsruntime /usr/local/bin/node /usr/local/bin/node
COPY --from=builder /out/youtube-extractor /usr/local/bin/youtube-extractor
RUN node --version && yt-dlp --version && ffmpeg -version >/dev/null
ENV PORT=8080 DATA_DIR=/data RUST_LOG=info YTDLP_PATH=/usr/local/bin/yt-dlp
USER 10001:10001
EXPOSE 8080
HEALTHCHECK --interval=30s --timeout=5s --start-period=10s --retries=3 \
    CMD curl --fail --silent --show-error --max-time 4 "http://127.0.0.1:${PORT:-8080}/health" || exit 1
ENTRYPOINT ["/usr/local/bin/youtube-extractor"]
