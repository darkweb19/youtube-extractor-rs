# HTTP API v1

All versioned endpoints use `Authorization: Bearer <API_TOKEN>`. The token is a shared service credential; this release does not provide per-user ownership or multi-tenant authorization. Keep it server-side and terminate public connections with TLS. Signed media source URLs, Apify credentials and internal paths are not part of the client API.

`GET /health` is public and reports readiness. A successful response establishes that the local service prerequisites work; it does not contact YouTube or establish outbound compatibility. An unavailable local storage directory or FFmpeg returns `503`.

## Metadata

`POST /api/v1/videos/info` accepts JSON:

```json
{"url":"https://www.youtube.com/watch?v=VIDEO_ID"}
```

The successful response preserves `id`, `title`, `description`, `duration_seconds`, `channel_id`, `channel_name`, `thumbnails` and `formats`. Metadata is retrieved by the configured Apify metadata Actor. The `formats` array is empty because the Actor does not expose verified YouTube itags or direct media stream descriptors. Format availability is decided by the Apify downloader when a job runs. Only canonical, allowlisted YouTube URL forms are accepted.

## Download lifecycle

`POST /api/v1/downloads` creates a bounded asynchronous job and returns `202`:

```json
{"url":"https://www.youtube.com/watch?v=VIDEO_ID","type":"video","format":"mp4","quality":"720p"}
```

The downloader invokes the Apify Actor `marielise.dev~youtube-video-downloader` by default. It then fetches the resulting MP4 or MP3 into bounded local storage, converts with FFmpeg as requested, and publishes only a same-origin authenticated download route. Apify Actor processing may take minutes and incurs Apify charges.

Audio requests use `"type":"audio"` and `"format":"original"`, `"m4a"` or `"mp3"`. Audio requests may include `"audio_bitrate_kbps":128` for FFmpeg conversion (32–320 kbps). The Actor returns MP3 for audio; `original` therefore maps to MP3 rather than the original YouTube audio codec. M4A uses an MP3-to-AAC transcode. Audio `quality` values are accepted for backward compatibility but do not choose an upstream audio stream. Video quality supports `best` (1080p cap), `worst` (360p) and heights 360p, 480p, 720p or 1080p; intermediate requested heights are rounded down to the supported cap. Higher and lower resolutions are unsupported by the default Actor. Video `original` maps to the Actor's MP4. WebM requests use a local VP9/Opus transcode. Actual delivered height depends on upstream availability. The existing request and response fields remain unchanged.

`GET /api/v1/downloads/{job_id}` returns the job state. States are `queued`, `running`, `completed`, `failed` and `cancelled`. An example completed response has this shape:

```json
{
  "job_id":"a1b2c3d4-0000-4000-8000-000000000001",
  "status":"completed",
  "created_at":"2026-10-16T12:00:00Z",
  "updated_at":"2026-10-16T12:00:01Z",
  "progress":{"downloaded_bytes":1024,"total_bytes":1024,"percent":100.0},
  "output":{"filename":"audio.mp3","size_bytes":1024,"mime_type":"audio/mpeg"},
  "error":null,
  "download_url":"/api/v1/downloads/a1b2c3d4-0000-4000-8000-000000000001/file"
}
```

`output`, `error`, total size, percentage and download URL depend on state and availability. The examples illustrate schema, not verified media sizes or identifiers. Clients should tolerate additional response fields.

`GET /api/v1/downloads/{job_id}/file` streams a completed local file and requires the same bearer token. `download_url` is a relative authenticated route, not a public signed URL. Wait for `completed` before requesting bytes. Files are retained only for the configured TTL.

`DELETE /api/v1/downloads/{job_id}` requests cancellation and removes local job resources. It returns `204`; deleting an absent job is idempotent. Cancellation can race with an already completing job. Clients must stop polling after deletion.

## Error handling

Errors use JSON with a stable code and a safe message:

```json
{"error":{"code":"ACCESS_RESTRICTED","message":"Public playback access was denied"}}
```

| HTTP | Codes and meaning |
| --- | --- |
| `400` | `INVALID_REQUEST`: malformed or unsupported input |
| `401` | `UNAUTHORIZED`: missing or invalid bearer token |
| `403` | `ACCESS_RESTRICTED`: upstream sign-in/challenge/access restriction |
| `404` | `NOT_FOUND`: job or output is absent |
| `409` | `CANCELLED`: operation cannot proceed in this state |
| `413` | `DOWNLOAD_TOO_LARGE`: configured media limit exceeded |
| `422` | `UNSUPPORTED_FORMAT`: stream/protocol/output is not implemented |
| `429` | `RATE_LIMITED` or `UPSTREAM_RATE_LIMITED` |
| `502` | `NETWORK`, `DNS`, `PLAYER_CHANGED`, `EXPIRED_URL`, `DOWNLOAD_INTERRUPTED` |
| `503` | `QUEUE_FULL`: bounded admission capacity reached |
| `504` | `TIMEOUT` |
| `500` | `FFMPEG_FAILED`, `STORAGE`, `INTERNAL` |

A job can be accepted and fail later; inspect its `error` rather than treating `202` as successful extraction. Do not retry access restrictions in a loop. Queue pressure and transient network errors may be retried with bounded client backoff. No callback or webhook fields are currently accepted or delivered.

Apify Actor failures are mapped to stable API error codes without leaking actor responses, tokens or signed media URLs. Downloaded files remain local to the service, not on a public response endpoint. The downloader requires `APIFY_API_TOKEN` and the provider may charge for attempts or successful deliveries. Remote object links expire; the service copies them locally before marking a job completed.

## Public browser workflow

The page at `/` is public when `FRONTEND_ENABLED=true` (the default). Its static
assets are embedded in the Rust binary. The public page never receives the
private API bearer token and does not call `/api/v1` from browser JavaScript.
The versioned API authentication rules above still apply.

The browser obtains an anonymous HttpOnly, SameSite=Strict cookie and a separate
CSRF value through `GET /ui/session`. Mutations send the CSRF value in
`X-CSRF-Token`; cross-origin browser requests are rejected. These routes use the
same bounded request/worker/storage capacity as the authenticated API:

| Method | Route | Behavior |
| --- | --- | --- |
| `GET` | `/ui/session` | Create or refresh an anonymous browser session; return `csrf_token` |
| `POST` | `/ui/downloads` | Create a video/MP4 job using the request schema above; CSRF required |
| `GET` | `/ui/downloads/{job_id}` | Read a job owned by this browser session |
| `GET`, `HEAD` | `/ui/downloads/{job_id}/media` | Inline MP4 playback, with single byte-range support |
| `GET`, `HEAD` | `/ui/downloads/{job_id}/file` | MP4 attachment; HEAD checks availability without downloading bytes |
| `DELETE` | `/ui/downloads/{job_id}` | Cancel/delete an owned job; CSRF required |

Browser job responses have the normal job fields plus `media_url`, and their
`download_url` points to the `/ui` attachment route. Both URLs require the
browser's cookie. Other sessions and privately created API jobs are inaccessible
through this interface, even if their IDs are known. Browser responses and assets
use `Cache-Control: no-store`.

Sessions expire after one hour of inactivity. Anonymous ownership is process-local
and ends on restart; retained jobs/files can still be accessed by an authorized
operator through `/api/v1`. A failed or expired browser session must be refreshed
explicitly, without automatically resubmitting a generation request.

Set `UI_SECURE_COOKIES=true` for HTTPS deployments, and configure an exact
`UI_PUBLIC_ORIGIN` if a reverse proxy rewrites Host or a custom domain is used.
When unset, `UI_PUBLIC_ORIGIN` defaults to `https://$RAILWAY_PUBLIC_DOMAIN` if
Railway supplies that variable. Secure cookies default to true for the resolved
HTTPS origin; explicit cookie settings take precedence. Expected origins are
never derived from visitor-supplied forwarded headers. Set `FRONTEND_ENABLED=false`
to remove the browser page, assets and `/ui` endpoints while retaining the API.
See [ADR 0002](adr/0002-public-browser-generator.md) and the
[frontend review](frontend-review.md) for the security boundary and limits.
