# Deployment and operations

Deploy this repository as its own Railway service. It does not need, or share code with, another application backend. `railway.toml` selects the Dockerfile, `/health`, bounded restart attempts and one replica. Deployment requires an explicitly chosen Railway project, environment and service with suitable credentials; repository configuration alone does not create or deploy those resources.

## Railway setup

1. Connect this repository to a new service in the intended Railway project/environment. Select its Dockerfile builder and the intended reviewed commit.
2. Add a persistent volume mounted at `/data` and set `DATA_DIR=/data`. Arrange volume ownership so UID/GID `10001:10001` can create and delete files. The image prepares this path for ordinary Docker named volumes; an externally mounted Railway volume can have different ownership. Use the platform's supported volume initialization procedure, then keep the application non-root.
3. Set a randomly generated `API_TOKEN` of at least 32 characters with no whitespace in Railway variables. Keep `ALLOW_UNAUTHENTICATED=false`. Do not paste credentials into source or deployment logs.
4. Keep one replica for local storage and job ownership. Railway injects `PORT`; the service binds `0.0.0.0:$PORT`. Set resource limits and variables below before enabling public traffic. Expose an HTTPS Railway domain or private service network as appropriate.
5. Deploy and verify `/health`, authentication and resource usage. Then run the explicitly opted external smoke check from the same Railway deployment and inspect the structured result.

The public health route probes local readiness, not YouTube reachability. A healthy deployment may still be denied by YouTube's data-center IP policy. Do not route external verification through a different network and report it as Railway success.

```bash
# Export actual BASE_URL, API_TOKEN, YOUTUBE_TEST_URL securely first.
export ALLOW_EXTERNAL_YOUTUBE=1
bash scripts/smoke-railway.sh
```

This script requires local `curl`, Python 3 and `ffprobe`; it exercises the deployed HTTP origin and validates bytes returned from that deployment. It performs no deployment and needs no Railway CLI token. If upstream access is denied, retain the error evidence and mark compatibility blocked. Do not add CAPTCHA bypassing, credential cookies or proxy rotation.

## Container use

```bash
docker build -t youtube-extractor:local .
docker volume create youtube-extractor-data
# API_TOKEN must be exported securely before this command.
docker run --rm --name youtube-extractor \
  --publish 127.0.0.1:8080:8080 \
  --env API_TOKEN --env DATA_DIR=/data \
  --mount type=volume,src=youtube-extractor-data,dst=/data \
  --memory 2g --cpus 2 --pids-limit 128 --cap-drop ALL \
  --security-opt no-new-privileges:true \
  youtube-extractor:local
```

The pinned Rust builder compiles with the lockfile. The Debian runtime installs FFmpeg, `curl`, a pinned yt-dlp Python package with official EJS scripts, and Deno 2.7.4, then runs as UID/GID `10001:10001`. `YTDLP_PATH` points to the venv executable and the Deno cache is stored under `/data/deno-cache`. No source, Rust compiler or session credentials are copied into the runtime. Base version tags are explicit; release operators should additionally lock reviewed base image digests and refresh them for security updates.

In managed Codex environments, supply session CA trust through optional BuildKit secret mounts; never copy it into the image or disable TLS verification:

```bash
mkdir -p /workspace/.tools/buildx
BUILDX_CONFIG=/workspace/.tools/buildx docker build \
  --secret id=proxy_ca,src="$CODEX_PROXY_CERT" \
  --secret id=system_ca,src=/etc/ssl/certs/ca-certificates.crt \
  -t youtube-extractor:local .
```

Keep the environment's supplied Docker proxy defaults and registry credentials. Runtime HTTPS through that proxy also needs a read-only combined CA bundle mount and `SSL_CERT_FILE` pointing to it. This is environment-specific trust configuration, separate from ordinary Railway deployment.

Configured `HTTPS_PROXY`/`ALL_PROXY` (and their lowercase equivalents) are trusted infrastructure: the proxy owns remote DNS and destination connection enforcement. Direct connections resolve only public IP addresses and pin them for the request. Local address pinning cannot enforce the proxy's remote DNS resolution; configure any outbound proxy to deny private/reserved destinations and enforce the same destination policy. Application URL and redirect allowlists remain enforced in either mode. Do not accept proxy settings from API clients.

## Configuration

`.env.example` is the authoritative editable example. The service reads process environment variables; it does not load dotenv files automatically.

| Variable | Default | Purpose |
| --- | --- | --- |
| `API_TOKEN` | required | Shared bearer token, at least 32 characters, no whitespace |
| `ALLOW_UNAUTHENTICATED` | `false` | Explicit local development only, with no token configured |
| `PORT` | `8080` | Listen port on all IPv4 interfaces |
| `DATA_DIR` | `data` (`/data` in image) | Job records, temporary media and completed outputs |
| `YTDLP_PATH` | `yt-dlp` | Managed extractor executable (image: `/opt/ytdlp/bin/yt-dlp`) |
| `DENO_PATH` | `deno` | JS runtime availability check (image: `/usr/local/bin/deno`) |
| `WORKERS` | `2` | Concurrent background media jobs, range 1–64 |
| `QUEUE_CAPACITY` | `16` | Bounded waiting queue, range 1–4096 |
| `MAX_JOBS` | `256` | Retained/admitted job record limit; at least workers + queue |
| `MAX_DOWNLOAD_BYTES` | `1073741824` | Per-job combined workspace budget for downloaded inputs plus output |
| `MAX_STORAGE_BYTES` | `8589934592` | Logical aggregate reservations/output budget; at least MAX_DOWNLOAD_BYTES |
| `JOB_TTL_SECS` | `3600` | Job/output retention, at most seven days |
| `CONNECT_TIMEOUT_SECS` | `10` | Outbound connection timeout |
| `REQUEST_TIMEOUT_SECS` | `30` | Metadata/upstream request timeout |
| `DOWNLOAD_TIMEOUT_SECS` | `1800` | Job media processing/download timeout |
| `FFMPEG_PATH` | `ffmpeg` | FFmpeg executable path |
| `REQUESTS_PER_MINUTE` | `120` | In-process API rate limit |
| `MAX_CONCURRENT_REQUESTS` | `32` | Concurrent API request bound |
| `RUST_LOG` | `info` | Tracing filter; logs are structured JSON |

## Storage, resources and recovery

The initial storage implementation is local. A persistent `/data` volume is required to preserve records and completed output across container replacement. An ephemeral directory is suitable only for disposable development. This release does not provide an S3-compatible backend, distributed queue or distributed rate limiter. More than one replica can conflict over local job ownership and does not provide horizontal scaling; add those facilities before increasing replicas.

Start with two CPU cores and 2 GiB memory for two workers, then measure actual codecs and bitrates. These are planning estimates, not benchmarked guarantees. MP3 transcoding consumes CPU; remuxing is cheaper. Keep `WORKERS` and `MAX_DOWNLOAD_BYTES` low enough for the workload. `MAX_DOWNLOAD_BYTES` covers combined downloaded inputs and the processed output, so a remux generally needs roughly twice the source size. Admission reserves that budget for each nonterminal job, including cancellation still in progress, and counts retained completed output toward `MAX_STORAGE_BYTES`. This logical storage ceiling may bind before the queue's nominal capacity. It does not reserve blocks from the filesystem or account for unrelated files; monitor actual free space and leave disk headroom.

Enforce hard memory, CPU and process limits through Railway/container settings. FFmpeg's `-max_alloc` limits each allocation, not total process RAM, and a bounded Rust streaming buffer does not bound every codec allocation. Disk budget accounting excludes filesystem overhead and unrelated files, and FFmpeg's byte ceiling can overshoot slightly by an encoded packet. Keep free-space headroom and treat application limits as admission controls rather than operating-system quotas.

Graceful termination stops accepting work, cancels in-flight work and cleans partial files. On restart, queued/running persisted records become failed with `DOWNLOAD_INTERRUPTED`; they are not automatically retried or resumed. Completed records and files survive when the same volume is retained. Expiration removes records and associated files after `JOB_TTL_SECS`. Persisted jobs do not imply resumable byte-range downloads. Test restart scenarios before relying on recovery for production operations. Retention is not backup: volume deletion or corruption can lose records and outputs. Back up important data using platform-supported mechanisms.

Use JSON tracing, HTTP errors, job states, queue depth, completion latency, FFmpeg exit failures and disk utilization for operations. Do not log bearer tokens, signed source URLs or upstream page bodies. Keep application and platform logs together when distinguishing DNS, network policy, HTTP rate limiting, player changes and access restrictions.

For rollback, retain the last verified image/commit and the associated configuration. Roll back through Railway's deployment history and repeat health/auth and chosen compatibility checks. Preserve the data volume and assess state-schema compatibility before rollback. Do not claim deployed service availability or extraction success until those checks have run against the selected service.

## Railway-specific extraction limitations

A healthy container and successful local yt-dlp test do not prove a Railway outbound IP is accepted by YouTube. Avoid repeated immediate retry loops if Railway gets an upstream sign-in/verification response. The subprocess is given only a canonical, validated YouTube video URL, and neither Google cookies nor CAPTCHA bypass is configured. yt-dlp requires outbound HTTPS to YouTube/googlevideo hosts and optionally npm for EJS components. Keep platform network limits in place; yt-dlp manages redirects internally, so the legacy native HTTP destination allowlist is **not** an enforcement boundary for these subprocess requests.
