# Deployment and operations — Apify backend

The Rust/Axum service retains the existing HTTP API and public browser UI. Apify now fetches metadata and downloads media; FFmpeg performs local validation and requested output conversion. The service uses its original bounded in-process jobs and persistent disk. It does not execute yt-dlp or fetch YouTube media directly.

## Railway setup

1. Connect `darkweb19/youtube-extractor-rs` to a Railway Docker service. Use the root `Dockerfile`, which does not need BuildKit secret mounts.
2. Mount a persistent volume at `/data` writable by UID/GID 10001, set `DATA_DIR=/data`, and run one replica.
3. Set `API_TOKEN` to a random 32+ character value and set `ALLOW_UNAUTHENTICATED=false`.
4. Set `APIFY_API_TOKEN` from your Apify console in Railway service variables. Never put the token in source control or frontend JavaScript.
5. Default Actors: `APIFY_METADATA_ACTOR=streamers~youtube-scraper` and `APIFY_DOWNLOAD_ACTOR=marielise.dev~youtube-video-downloader`. Override only when the replacement implements the same JSON input/output schema.
6. If the downloader's file is in external R2 rather than the Apify key-value store, add the precise hostname from that trusted Actor to `APIFY_MEDIA_HOSTS` (comma-separated). The service rejects unexpected media hosts and redirect targets. Do not set a wildcard. Credentials are only transmitted to `api.apify.com`.
7. Set `UI_SECURE_COOKIES=true` on HTTPS, optionally `UI_PUBLIC_ORIGIN=https://your-domain` if a reverse proxy changes the origin.
8. Verify `/health` and API authentication. Then run `bash scripts/smoke-railway.sh` from a machine that has `ffprobe`, `curl`, and Python, with `BASE_URL`, `API_TOKEN`, `YOUTUBE_TEST_URL` and `ALLOW_EXTERNAL_YOUTUBE=1` set. This starts paid Actor runs. Use only videos you are authorized to download.

## Operational behavior

- The metadata endpoint invokes the metadata Actor and can block up to `DOWNLOAD_TIMEOUT_SECS` (default 1800). Download creation returns 202 immediately; progress can be polled through the existing job route.
- Download jobs use Apify's asynchronous Actor run API, poll for a terminal result and fetch one dataset item. Cancellation attempts to abort an active Actor, although already incurred provider charges may remain.
- A successful actor must supply a matching `videoId`, `status: success`, `downloadUrl`, and optional `fileSizeBytes`; invalid or missing output fails closed.
- Media is downloaded with streaming byte caps, cancellation and an exact HTTPS media host allowlist. There is no automatic resume for interrupted Apify object downloads; failed jobs clean partial local files.
- The actor supports MP4 at 360/480/720/1080p and MP3. FFmpeg converts to requested M4A or WebM. `original` means the actor's native output, not an arbitrary YouTube source codec.
- Apify may store large video files on Cloudflare R2; its signed links expire. The service must copy them locally before completion. Do not expose the signed link or bearer token to clients.
- `/health` checks local disk and FFmpeg, not remote actor availability or token validity. Live acceptance requires the external smoke test.
- Actor providers can change pricing, input schema, outputs and reliability. Monitor spending and impose Apify actor run charge caps where appropriate.

## Configuration

| Variable | Default | Meaning |
|---|---|---|
| `APIFY_API_TOKEN` | required | Private server-side Apify credential |
| `APIFY_DOWNLOAD_ACTOR` | `marielise.dev~youtube-video-downloader` | Downloader Actor identity |
| `APIFY_METADATA_ACTOR` | `streamers~youtube-scraper` | Metadata Actor identity |
| `APIFY_MEDIA_HOSTS` | empty | Additional exact trusted HTTPS media hostnames |
| `API_TOKEN` | required | Client API bearer token (separate from Apify) |
| `DATA_DIR` | `data` | Persistent file and job storage |
| `WORKERS` | 2 | Concurrent jobs |
| `MAX_DOWNLOAD_BYTES` | 1 GiB | Combined input/output job workspace budget |
| `MAX_STORAGE_BYTES` | 8 GiB | Logical retained storage limit |
| `DOWNLOAD_TIMEOUT_SECS` | 1800 | Actor and processing timeout |
| `REQUEST_TIMEOUT_SECS` | 30 | Normal HTTP request deadline |

## Verification and rollback

CI uses synthetic files and does not incur actor charges. The optional external smoke verifies the deployed API contract, playable bytes and MP3/MP4 behavior. If Apify rejects a run, inspect the Actor run in your private Apify console; public job errors intentionally suppress sensitive response contents. Roll back to a previously reviewed commit or restore the relevant deployment image only after considering the differing extractor configuration and ensuring the persistent volume is retained.
