# HTTP API v1

API clients continue to use `Authorization: Bearer <API_TOKEN>`. Frame also supports an authenticated browser session cookie. The token is a shared service credential; this release does not provide per-user ownership or multi-tenant authorization. Keep it private and terminate public connections with TLS. The bundled single-operator UI exchanges a user-entered token for an opaque, signed HttpOnly cookie through the same-origin session endpoint; it never persists the raw token in browser storage. Cookie-authenticated requests use `X-Frame-Client: 1` and same-origin credentials. Do not use the shared-token UI for anonymous public access; use HTTPS and additional ingress access controls where appropriate. Signed media source URLs and internal paths are not part of the client API.

`GET /health` is public and reports readiness. A successful response establishes that the local service prerequisites work; it does not contact YouTube or establish outbound compatibility. An unavailable local storage directory or FFmpeg returns `503`.

## Saved browser access

- `GET /api/v1/session` returns `200` with `{"authenticated":true,"saved":true}` for a valid saved browser cookie. Explicit bearer authentication or unauthenticated development mode returns `saved:false`; missing/invalid credentials return `401` on an authenticated service.
- `POST /api/v1/session` requires an explicit valid bearer token and `X-Frame-Client: 1`. It returns `204` and sets `__Secure-frame_session`; the token is not returned or stored verbatim. No request body is needed.
- `DELETE /api/v1/session` requires `X-Frame-Client: 1` and returns `204` with an expired cookie. It can clear expired/invalid browser access without requiring the old token. It does not cancel jobs or revoke other browsers.

The cookie is host-only with `Path=/api`, `HttpOnly`, `Secure`, `SameSite=Strict`, and a one-year lifetime verified by its signature. API token rotation invalidates all existing browser sessions; ordinary process restarts retain them. Cookies are accepted only with the browser custom header and same-origin request protections; there is no credentialed cross-origin API. An explicitly supplied invalid Authorization header cannot fall back to a valid cookie. Session responses are non-cacheable. Hosted browser access requires HTTPS; Secure cookies are also supported by modern browsers on loopback during local development.

See [browser access](browser-access.md) for the security boundary and recovery behavior.

## Metadata

`POST /api/v1/videos/info` accepts JSON:

```json
{"url":"https://www.youtube.com/watch?v=VIDEO_ID"}
```

The successful response contains `id`, `title`, `description`, `duration_seconds`, `channel_id`, `channel_name`, `thumbnails` and `formats`. Values absent from the upstream response remain optional. A format descriptor reports `itag`, `mime_type`, `codecs`, `container`, `bitrate`, `width`, `height`, `fps`, `content_length`, `has_audio` and `has_video`; discovery is not a guarantee that every format can be resolved or downloaded. Only allowlisted YouTube URL forms are accepted; arbitrary network destinations and unsafe redirects are rejected.

## Download lifecycle

`POST /api/v1/downloads` creates a bounded asynchronous job and returns `202`:

```json
{"url":"https://www.youtube.com/watch?v=VIDEO_ID","type":"video","format":"mp4","quality":"720p"}
```

Audio requests use `"type":"audio"` and `"format":"original"`, `"m4a"` or `"mp3"`. Audio quality accepts `"best"` or `"worst"`, with optional `"audio_bitrate_kbps":128` for conversion (32–320 kbps). Video quality accepts `"best"`, `"worst"` or a height such as `"360p"`, `"720p"` or `"1080p"`; video output formats are `original`, `mp4` or `webm`. A requested height is a ceiling: selection chooses the best compatible available stream at or below it. Selection is constrained by the source and supported codecs. If no compatible stream exists, the job fails with a structured error.

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

`GET /api/v1/downloads/{job_id}/file` streams a completed local file and requires a valid bearer token or browser session. `download_url` is a relative authenticated route, not a public signed URL. Wait for `completed` before requesting bytes. Files are retained only for the configured TTL.

`DELETE /api/v1/downloads/{job_id}` requests cancellation and removes local job resources. It returns `204`; deleting an absent job is idempotent. Cancellation can race with an already completing job. Clients must stop polling after deletion.

## Error handling

Errors use JSON with a stable code and a safe message:

```json
{"error":{"code":"ACCESS_RESTRICTED","message":"Public playback access was denied"}}
```

| HTTP | Codes and meaning |
| --- | --- |
| `400` | `INVALID_REQUEST`: malformed or unsupported input |
| `401` | `UNAUTHORIZED`: missing or invalid bearer token/browser session |
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
