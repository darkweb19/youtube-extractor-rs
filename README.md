# YouTube Extractor (Apify backend)

Rust/Axum service for asynchronous YouTube video downloads. **Media acquisition now runs through Apify Actors**, not a local yt-dlp package or the previous native YouTube player/stream extractor. Existing HTTP endpoints, job IDs, authenticated file delivery and the browser generator UI remain unchanged.

The service validates YouTube links, creates an Apify Actor run, waits for the remote result, streams the stored file into a bounded local workspace, converts/validates it with FFmpeg and publishes the final file through the same local API/browser routes. A separate metadata Actor supplies the video information endpoint. External Actor operation depends on provider availability and can incur charges.

Use only videos that you own or have rights to download.

## Required configuration

Copy `.env.example` and set:

```bash
API_TOKEN=YOUR_32_PLUS_CHARACTER_PRIVATE_API_BEARER_TOKEN
APIFY_API_TOKEN=YOUR_PRIVATE_APIFY_TOKEN
APIFY_DOWNLOAD_ACTOR=marielise.dev~youtube-video-downloader
APIFY_METADATA_ACTOR=streamers~youtube-scraper
DATA_DIR=./data
```

- Obtain `APIFY_API_TOKEN` from [Apify API & Integrations](https://console.apify.com/settings/integrations). **Set it only as a server-side secret** (Railway Variables, Docker secrets, etc.). It is not the service's `API_TOKEN`.
- Default downloader: [marielise.dev/youtube-video-downloader](https://apify.com/marielise.dev/youtube-video-downloader). It accepts `urls: [{url}]`, `format: "default" | "mp3"` and `quality: "360" | "480" | "720" | "1080"`, and returns a dataset row with `videoId`, `status`, `downloadUrl` and `fileSizeBytes`.
- Default metadata Actor: [streamers/youtube-scraper](https://apify.com/streamers/youtube-scraper). Its `startUrls` response is normalized to the unchanged `VideoInfo` JSON shape.
- Set `APIFY_MEDIA_HOSTS` to comma-separated **exact** hostnames only if the downloader emits external Cloudflare R2 URLs instead of Apify Key-Value Store links. The default permitted host is `api.apify.com`. Never copy signed URLs into source or logs.
- For public HTTPS hosting, set `UI_SECURE_COOKIES=true`; set `UI_PUBLIC_ORIGIN` to your exact external HTTPS origin only when necessary.
- Bind `DATA_DIR` to persistent storage (e.g. `/data` on Railway) and keep one replica.

## Run

```bash
cp .env.example .env
# Fill in the two required tokens privately.
set -a; source .env; set +a
cargo run --locked
```

The frontend is served at `http://localhost:8080/`. The public browser experience is unchanged: paste a YouTube URL, confirm generation, follow its status, preview the completed MP4, then download it. The frontend never receives either API credential.

The REST API still supports:

| Method | Route | Description |
|---|---|---|
| GET | `/health` | Local readiness |
| POST | `/api/v1/videos/info` | Apify-backed metadata |
| POST | `/api/v1/downloads` | Queue an asynchronous job |
| GET | `/api/v1/downloads/{id}` | Job state and progress |
| GET | `/api/v1/downloads/{id}/file` | Authenticated output file |
| DELETE | `/api/v1/downloads/{id}` | Cancel/delete a job |

All `/api/v1` endpoints require `Authorization: Bearer <API_TOKEN>`. No endpoint or JSON request field was renamed.

Example:

```bash
curl -fS -X POST http://localhost:8080/api/v1/downloads \
  -H "Authorization: Bearer $API_TOKEN" \
  -H 'Content-Type: application/json' \
  -d '{"url":"https://youtu.be/dQw4w9WgXcQ","type":"video","format":"mp4","quality":"720p"}'
```

The response is `202 Accepted` with a `job_id`. Poll the job route; when `completed`, fetch its authenticated file route.

## Formats and provider limitations

The default Actor delivers MP4 (with audio) or MP3, not source adaptive streams. Video supports 360p, 480p, 720p and 1080p caps. The REST API continues to accept `best`, `worst` and height strings, but values outside the Actor's range fail with `UNSUPPORTED_FORMAT`. `best` means an Actor cap of 1080p. `original` maps to the Actor's MP4 for video and MP3 for audio. WebM uses local VP9/Opus transcoding; M4A uses local AAC conversion. Audio bitrate is applied locally by FFmpeg.

The metadata endpoint preserves the `formats` array but returns it empty: the configured metadata Actor does not provide verified YouTube itags. The exact playable format is determined during the download job. Long videos may require raising `DOWNLOAD_TIMEOUT_SECS` and resource budgets. Apify charges separately for Actor runs and downloaded content. Signed remote file URLs expire, so only successfully copied local files are reported complete.

## Deployment and verification

Railway uses the root `Dockerfile` and `railway.toml`. Set both tokens in Railway service variables, set `DATA_DIR=/data`, mount a writable persistent volume, and use a single instance. See [deployment and operations](docs/deployment.md).

```bash
cargo fmt --all -- --check
cargo clippy --locked --all-targets -- -D warnings
cargo test --locked --all-targets
docker build -t youtube-extractor:local .
IMAGE=youtube-extractor:local bash scripts/smoke-container.sh
```

For live verification using media you are authorized to download, set `BASE_URL`, `API_TOKEN`, `YOUTUBE_TEST_URL` and `ALLOW_EXTERNAL_YOUTUBE=1`, then run `bash scripts/smoke-youtube.sh` (or `scripts/smoke-railway.sh`). This makes paid Apify calls and verifies metadata, MP3, MP4, duration and video/audio tracks. It is **not** exercised by deterministic CI.

The `/health` endpoint checks local disk and FFmpeg only. It does not prove Apify credentials are valid or that YouTube downloads work. Check [API details](docs/api.md) and [architecture](docs/architecture.md) for the exact behavior.
