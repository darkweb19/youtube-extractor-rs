# syntax=docker/dockerfile:1
FROM rust:1.99.1-bookworm@sha256:e12566d5abc17a0da741628782cdb504b68b8961f5bfcf677936bf3e8b32de29 AS builder
ARG CARGO_BUILD_JOBS=3
WORKDIR /app
COPY Cargo.toml Cargo.lock rust-toolchain.toml ./
COPY src ./src
COPY web ./web
# Use standard Docker build steps for compatibility with Railway's Dockerfile parser.
RUN set -eu; \
    CARGO_BUILD_JOBS="$CARGO_BUILD_JOBS" cargo build --locked --release; \
    install -D target/release/youtube-extractor /out/youtube-extractor

FROM debian:bookworm-slim AS runtime
RUN set -eu; \
    apt-get update; \
    apt-get install -y --no-install-recommends ca-certificates curl ffmpeg python3 python3-venv unzip; \
    rm -rf /var/lib/apt/lists/*; \
    python3 -m venv /opt/ytdlp; \
    /opt/ytdlp/bin/pip install --no-cache-dir 'yt-dlp[default]==2026.08.19'; \
    arch="$(dpkg --print-architecture)"; \
    case "$arch" in \
      amd64) deno_arch=x86_64-unknown-linux-gnu ;; \
      arm64) deno_arch=aarch64-unknown-linux-gnu ;; \
      *) echo "Unsupported Deno architecture: $arch" >&2; exit 1 ;; \
    esac; \
    curl --fail --location --silent --show-error \
      "https://github.com/denoland/deno/releases/download/v2.7.4/deno-${deno_arch}.zip" -o /tmp/deno.zip; \
    unzip -q /tmp/deno.zip -d /usr/local/bin; \
    chmod 755 /usr/local/bin/deno; \
    rm /tmp/deno.zip; \
    /opt/ytdlp/bin/yt-dlp --version; \
    deno --version; \
    groupadd --gid 10001 extractor; \
    useradd --uid 10001 --gid extractor --no-create-home --shell /usr/sbin/nologin extractor; \
    install -d -o extractor -g extractor -m 0700 /data
COPY --from=builder /out/youtube-extractor /usr/local/bin/youtube-extractor
ENV PORT=8080 DATA_DIR=/data RUST_LOG=info \
    YTDLP_PATH=/opt/ytdlp/bin/yt-dlp DENO_PATH=/usr/local/bin/deno \
    HOME=/data DENO_DIR=/data/deno-cache XDG_CACHE_HOME=/data/cache
USER 10001:10001
EXPOSE 8080
HEALTHCHECK --interval=30s --timeout=5s --start-period=10s --retries=3 \
    CMD curl --fail --silent --show-error --max-time 4 "http://127.0.0.1:${PORT:-8080}/health" || exit 1
ENTRYPOINT ["/usr/local/bin/youtube-extractor"]
