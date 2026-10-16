# Native YouTube extraction service

A standalone Rust/Axum HTTP service with its own player-response parser and stream resolver. General-purpose Rust crates handle HTTP, serialization and asynchronous I/O; FFmpeg handles local media processing. No existing YouTube extraction or downloader engine is used.

Use only publicly accessible media you are authorized to download. The service does not bypass DRM, sign-in requirements, CAPTCHA, or upstream access restrictions. A native implementation cannot fix a hosting provider's outbound-IP restriction.

This is an initial implementation with a deliberately limited protocol surface. Direct media formats are supported; unsupported player signatures, throttling transforms, live streams and manifest-only delivery must fail explicitly. See [architecture](docs/architecture.md), [API](docs/api.md), and [deployment](docs/deployment.md). Production acceptance requires actual metadata, audio and video verification from the intended deployment environment. A successful health check or fixture test does not establish YouTube compatibility.

See [protocol support](docs/protocol-support.md) for the exact supported parser and stream surface, JavaScript evaluation decision and manifest roadmap.

## Develop

Install the toolchain pinned in `rust-toolchain.toml` and FFmpeg (`ffmpeg` and `ffprobe` on `PATH`).

```bash
cp .env.example .env
# Set API_TOKEN in .env to a random value of at least 32 characters.
set -a
source .env
set +a
cargo run --locked
```

The server binds `0.0.0.0:$PORT` (default `8080`). `/health` is public; all `/api/v1` endpoints require `Authorization: Bearer <API_TOKEN>`. `.env` is ignored and is loaded explicitly by your shell, not automatically by the service. `ALLOW_UNAUTHENTICATED=true` is available only for deliberate local development with no token configured.

```bash
cargo fmt --all -- --check
cargo clippy --locked --all-targets -- -D warnings
cargo test --locked --all-targets
```

Deterministic tests use controlled fixtures and test media rather than external YouTube access. To check the container:

```bash
docker build -t youtube-extractor:local .
IMAGE=youtube-extractor:local bash scripts/smoke-container.sh
```

CI runs formatting, Clippy, tests, the Docker build and deterministic container health/authentication checks. External checks are explicitly opted in:

```bash
export BASE_URL=http://127.0.0.1:8080
export YOUTUBE_TEST_URL='https://www.youtube.com/watch?v=AUTHORIZED_VIDEO_ID'
export ALLOW_EXTERNAL_YOUTUBE=1
# API_TOKEN must already be set securely in your environment.
bash scripts/smoke-youtube.sh
```

The external script verifies metadata, creates and polls MP3 and MP4 jobs, downloads actual bytes, checks audio/video streams and duration with `ffprobe`, and deletes the completed jobs. It saves structured error responses when compatibility fails. Run the same check through `scripts/smoke-railway.sh` against an explicitly selected Railway service. See [test results](docs/test-results.md) for the evidence available for this revision.

The optional metadata-only probe runs with `AUTHORIZED_TEST_YOUTUBE_URL` set and `cargo test --locked --test external_metadata -- --ignored`. It does not verify media downloading. Player fixtures represent the native `ytInitialPlayerResponse` JSON parser; when updating fixtures, preserve a minimal reproducible player shape and strip signed media URLs, tokens, personal data and upstream page bodies. Synthetic fixtures demonstrate parser behavior rather than live compatibility.

## Scope and operation

The initial service uses a bounded in-process worker pool and a bounded queue, streamed downloads with per-job limits, local job/output storage, cancellation and expiration. It is independently deployable from other applications. Local state requires one replica and a persistent volume on Railway; it does not implement distributed job ownership or an S3 backend. Callbacks and signed webhook delivery are future API extensions.

Do not count the writable container layer as durable storage. Mount `/data`, retain an appropriate disk budget, and follow the restart/recovery behavior in the deployment guide. Live YouTube compatibility and deployment status must be assessed separately from local tests.
