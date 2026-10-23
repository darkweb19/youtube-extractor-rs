# YouTube extraction service (Rust / Axum + yt-dlp)

The `fix/rust-yt-bot` branch uses **yt-dlp 2026.10.21** for both YouTube metadata discovery and media downloading. Rust/Axum continues to handle authentication, the unchanged HTTP API and browser UI, job lifecycle, cancellation, storage and file delivery. FFmpeg handles media post-processing.

The active adapter is `src/ytdlp.rs`. The old native player parser is retained for regression tests but `rusty_ytdl` is removed. The production Docker image installs pinned yt-dlp with its packaged EJS solver and Node.js 24; no Cloudflare WARP or IP rotation is configured.

**Network restriction remains:** changing to yt-dlp does not fix a YouTube block on Railway's outbound IP. Test real metadata and authorized media retrieval from the deployed service; `/health` and fixture tests do not prove YouTube access.

## Browser interface

Open the service root at `https://YOUR-RAILWAY-DOMAIN/`. The bundled, responsive browser UI provides a two-step workflow:

1. Paste a normal YouTube/watch/Shorts link and select **Generate MP4**. The first step opens a privacy-enhanced YouTube player for confirmation only; no extraction job is created.
2. Choose **Continue to generate**. The UI submits an authenticated MP4 job, polls its state, displays bounded progress when available, and finally loads the authenticated file as a temporary browser blob for playback and a **Download MP4** button. Backend and network failures appear as toasts and a persistent error panel; users can cancel queued/running jobs.

The API remains bearer-protected, including completed files. Enter your existing `API_TOKEN` in **Private API access** on the page. The UI retains it only in the current tab's memory; it is not baked into HTML/JavaScript, saved to local storage, or transmitted to YouTube. Use this interface only as a trusted single-operator tool on HTTPS. Anyone who knows the shared token can operate all jobs: **do not post the token publicly or use this UI as an anonymous public downloader**. Consider additional edge access controls for an exposed Railway domain.

Authenticated video preview downloads the MP4 into a browser Blob because a native `<video>` element cannot attach the required bearer header. For large files, this uses significant browser memory and bandwidth; keep the tab open while the job runs. The MP4 and its local job record expire according to `JOB_TTL_SECS`. The YouTube confirmation player is a separate third-party embed and may itself be unavailable for restricted videos. Neither preview nor a healthy API confirms that Railway's outbound IP is allowed to extract a YouTube video.

## Usage

### Run locally

Install the pinned Rust toolchain and FFmpeg (`ffmpeg` and `ffprobe`). On macOS: `brew install ffmpeg jq`. The Rust build uses the committed `Cargo.lock`. Local runs additionally require yt-dlp 2026.10.21, its `yt-dlp-ejs` scripts, Node.js 22+ and FFmpeg; the Docker image supplies these.

Generate a strong API token, for example with `openssl rand -hex 32`, and store it in your local `.env` or hosting provider's secret settings. Use the same token in your API clients.

```bash
cp -n .env.example .env
# Edit .env: set API_TOKEN to your random token (at least 32 characters).
set -a
source .env
set +a
: "${API_TOKEN:?Set API_TOKEN in .env before starting}"
cargo run --locked
```

The server binds `0.0.0.0:$PORT` (default `8080`). `/health` is public; all `/api/v1` endpoints require `Authorization: Bearer <API_TOKEN>`. `.env` is ignored and is loaded explicitly by your shell, not automatically by the service. `ALLOW_UNAUTHENTICATED=true` is available only for deliberate local development with no token configured.

### Call the API

In another terminal, load the same `.env` or export the same `API_TOKEN` securely. Keep this shared credential on your backend: every holder can access all jobs and files. File URLs require authentication and are not public share links.

```bash
export BASE_URL='http://127.0.0.1:8080'
# For a hosted service, use its HTTPS origin without a trailing slash.
export VIDEO_URL='https://www.youtube.com/watch?v=VIDEO_ID'
# Replace VIDEO_ID with a video you are authorized to download.
: "${API_TOKEN:?Export the service API_TOKEN first}"
```

| Method | Endpoint | Purpose |
| --- | --- | --- |
| `GET` | `/health` | Public local readiness check |
| `POST` | `/api/v1/videos/info` | Metadata and supported available formats |
| `POST` | `/api/v1/downloads` | Create an asynchronous job (`202 Accepted`) |
| `GET` | `/api/v1/downloads/{job_id}` | Status, progress, output and errors |
| `GET` | `/api/v1/downloads/{job_id}/file` | Authenticated completed-file download |
| `DELETE` | `/api/v1/downloads/{job_id}` | Cancel work or delete retained resources |

Check readiness and retrieve metadata:

```bash
curl --fail --silent --show-error "$BASE_URL/health" | jq .

jq -n --arg url "$VIDEO_URL" '{url: $url}' |
  curl --fail-with-body --silent --show-error \
    --header "Authorization: Bearer $API_TOKEN" \
    --header 'Content-Type: application/json' \
    --data-binary @- "$BASE_URL/api/v1/videos/info" | jq .
```

Create a video job and capture its identifier:

```bash
set -euo pipefail
JOB_JSON=$(jq -n --arg url "$VIDEO_URL" \
  '{url: $url, type: "video", format: "mp4", quality: "720p"}' |
  curl --fail-with-body --silent --show-error \
    --header "Authorization: Bearer $API_TOKEN" \
    --header 'Content-Type: application/json' \
    --data-binary @- "$BASE_URL/api/v1/downloads")
JOB_ID=$(printf '%s' "$JOB_JSON" | jq -er '.job_id')
printf '%s\n' "$JOB_JSON" | jq .
```

Poll this route about every five seconds until `status` is `completed` or `failed`:

```bash
curl --fail-with-body --silent --show-error \
  --header "Authorization: Bearer $API_TOKEN" \
  "$BASE_URL/api/v1/downloads/$JOB_ID" | jq .
```

`202 Accepted` means the job was queued. It does not mean extraction succeeded. A failed job includes an `error.code` and safe message; inspect that response before retrying. After `completed`, download the bytes:

```bash
curl --fail --silent --show-error \
  --header "Authorization: Bearer $API_TOKEN" \
  --output video.mp4 "$BASE_URL/api/v1/downloads/$JOB_ID/file"
```

For MP3 audio, create a separate job with this request and use its returned `job_id` for the same status/file routes:

```bash
jq -n --arg url "$VIDEO_URL" \
  '{url: $url, type: "audio", format: "mp3", quality: "best", audio_bitrate_kbps: 128}' |
  curl --fail-with-body --silent --show-error \
    --header "Authorization: Bearer $API_TOKEN" \
    --header 'Content-Type: application/json' \
    --data-binary @- "$BASE_URL/api/v1/downloads" | jq .
```

Audio output supports `original`, `m4a`, and `mp3`; conversion bitrate is 32–320 kbps. Video output supports `original`, `mp4`, and `webm`. A video quality such as `720p` or `1080p` is a ceiling: the best compatible available source at or below that height is selected. `best` and `worst` are also accepted. See the [API contract](docs/api.md) for fields and error codes.

Cancel or delete a job when finished; otherwise retention cleanup removes it after the configured TTL:

```bash
curl --fail --silent --show-error --request DELETE \
  --header "Authorization: Bearer $API_TOKEN" \
  "$BASE_URL/api/v1/downloads/$JOB_ID"
```

## Deployment guidelines

### Railway

1. Create a **separate service** from this GitHub repository and select the reviewed branch/commit. Keep the repository root as the build root. The supplied `railway.toml` selects the Dockerfile, `/health`, restart policy, and one replica. Let the Docker image's entry point start the service.
2. Set `API_TOKEN` to a strong random secret, `ALLOW_UNAUTHENTICATED=false`, and `DATA_DIR=/data` in service variables. Railway supplies `PORT`; the application listens on `0.0.0.0:$PORT`.
3. Attach a persistent volume at `/data`, sized above the configured storage budget with room for filesystem overhead (for example, 10 GiB for the default 8 GiB logical budget). Ensure UID/GID `10001:10001` can write it using the platform's supported volume initialization process. The application runs as that non-root user.
4. Keep **one replica**. Jobs and files use local volume state; replicas do not share a queue or job namespace. Start with roughly 2 CPU cores and 2 GiB RAM for two workers, then measure your workload and apply hard resource limits. These are starting estimates, not measured production guarantees.
5. Expose an HTTPS domain or use private service networking. Keep the bearer token in the consuming backend's secret settings. Set the health-check path to `/health`, and verify that a `POST /api/v1/videos/info` request without an authorization header returns `401`.
6. Test actual metadata, audio, and video retrieval through that deployed service. A healthy process does not prove YouTube allows the Railway outbound IP.

Use the included external smoke script after deploying. It requires `curl`, Python 3, and `ffprobe` on the machine running the script:

```bash
export BASE_URL='https://YOUR-SERVICE-DOMAIN'
export YOUTUBE_TEST_URL='https://www.youtube.com/watch?v=VIDEO_ID'
export ALLOW_EXTERNAL_YOUTUBE=1
# Export the same API_TOKEN securely, and select authorized test media.
bash scripts/smoke-railway.sh
```

The script calls the deployed service, retrieves actual MP3/MP4 bytes, and validates them with `ffprobe`. Railway deployment and live YouTube compatibility remain unverified in this repository's recorded results; these steps are deployment guidance.

### Docker on a VPS or your own server

Build the image and create a persistent named volume. Export `API_TOKEN` securely before starting the container:

```bash
docker build -t youtube-extractor:local .
docker volume create youtube-extractor-data
: "${API_TOKEN:?Export a strong API_TOKEN first}"
docker run --detach --name youtube-extractor --restart unless-stopped \
  --publish 127.0.0.1:8080:8080 \
  --env API_TOKEN --env PORT=8080 --env DATA_DIR=/data \
  --env ALLOW_UNAUTHENTICATED=false \
  --mount type=volume,src=youtube-extractor-data,dst=/data \
  --memory 2g --cpus 2 --pids-limit 128 --cap-drop ALL \
  --security-opt no-new-privileges:true --stop-timeout 40 \
  youtube-extractor:local
curl --fail --silent --show-error --retry 15 --retry-connrefused \
  --retry-delay 1 --max-time 5 http://127.0.0.1:8080/health
```

For public access, put a host reverse proxy such as Caddy or Nginx in front of `127.0.0.1:8080` and terminate HTTPS there. Point your domain's DNS at the server and configure proxy timeouts for expected file transfers. The API clients then use that HTTPS origin as `BASE_URL`. The named volume persists when the container is replaced; back it up separately.

For managed build environments that inject a proxy CA, use the [documented BuildKit secret mounts](docs/deployment.md#container-use). Keep TLS verification enabled.

### Render, Fly.io, and other container platforms

Use a long-running Docker service with persistent storage and one instance. The runtime image already contains FFmpeg and certificates; no Python/Node extraction backend or custom start command is needed.

| Platform | Build and routing | Storage and runtime settings |
| --- | --- | --- |
| Render | Create a Web Service from the repository using its Dockerfile. Use `/health` for health checks and the service's HTTPS domain for clients. | Choose a plan supporting a persistent disk, mount it at `/data`, set `DATA_DIR=/data`, configure `API_TOKEN`, and keep one instance. Respect the provider's `PORT`. |
| Fly.io | Deploy the Dockerfile to a Fly Machine; route the HTTP service to internal port `8080` and enable HTTPS. | Mount a Fly Volume at `/data` in the Machine's region, set `PORT=8080` and `DATA_DIR=/data`, store `API_TOKEN` as a Fly secret, and keep one Machine. Disable automatic stopping while background jobs must continue. |
| Other Docker hosts | Build this Dockerfile or push the built image to your registry. Route the platform's HTTP service to the configured `PORT`. | Supply the token through secret settings, persistent writable storage at `/data`, `/health` checks, explicit resource limits, and one application instance. |

Check volume permissions for UID/GID `10001:10001` on every platform. The Dockerfile builds the Rust API and installs yt-dlp, Node.js, EJS scripts and FFmpeg. Keep yt-dlp and its JavaScript challenge solver updated when YouTube changes. Diskless or scale-to-zero deployments can lose local state or interrupt background jobs; the current architecture requires persistent storage and a running worker process. Horizontal scaling needs a shared durable queue/database and object storage, which are not implemented.

### Configuration and operations

| Variable | Default | Guidance |
| --- | --- | --- |
| `API_TOKEN` | required | Same secret for every authenticated route, including files; at least 32 visible ASCII characters |
| `ALLOW_UNAUTHENTICATED` | `false` | Keep false in production |
| `PORT` | `8080` | Use the provider's assigned port, or route to 8080 |
| `DATA_DIR` | `data`; image: `/data` | Mount persistent writable storage here |
| `WORKERS` / `QUEUE_CAPACITY` | `2` / `16` | Concurrent jobs / maximum waiting jobs |
| `MAX_DOWNLOAD_BYTES` | `1073741824` (1 GiB) | Combined downloaded inputs plus processing output per job |
| `MAX_STORAGE_BYTES` | `8589934592` (8 GiB) | Logical reservations and retained-output limit; leave physical disk headroom |
| `JOB_TTL_SECS` | `3600` | Retention period for completed jobs and files |
| `REQUESTS_PER_MINUTE` | `120` | Shared API rate limit; polling consumes this budget |
| `DOWNLOAD_TIMEOUT_SECS` | `1800` | Rust job and FFmpeg timeout |

The storage budget can reject admission before the queue fills. A remux usually needs roughly twice the source size while inputs and output coexist. On restart, interrupted queued/running jobs become failed with `DOWNLOAD_INTERRUPTED`; completed files survive only if the same persistent volume is retained. See [deployment and operations](docs/deployment.md) for all variables, backups, rollback, trust configuration, and recovery behavior.

| Symptom | Check |
| --- | --- |
| Startup refuses to run / `401` | Set a valid `API_TOKEN` and send the matching bearer header |
| `/health` returns `503` | Writable `DATA_DIR`, volume ownership, and FFmpeg/FFprobe availability |
| `NETWORK` / `DNS` | Host DNS, outbound HTTPS policy, proxy trust, and allowed YouTube/media destinations |
| `ACCESS_RESTRICTED` | Upstream content or automated-access restriction; do not bypass it |
| `UNSUPPORTED_FORMAT` | `yt-dlp` could not supply compatible output streams or a matching compatible output |
| `QUEUE_FULL` / `RATE_LIMITED` | Worker, queue, storage, retention, and request-rate limits; back off before retrying |

## Development and verification

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
