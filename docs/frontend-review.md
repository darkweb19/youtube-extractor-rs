# Public frontend review

Reviewed the public browser flow, its Rust bridge, the shared API admission
middleware, and the browser and HTTP test sources. This is a source review; test
execution and external compatibility results belong in `docs/test-results.md`.

## Resolved findings

- **Saboteur — ownership lifetime:** the original bridge expired ownership at
  submission time plus one execution timeout and retention interval. A queued
  job can wait longer than that. Ownership now lasts with the browser session;
  stale job IDs are reclaimed only after the job manager confirms removal.
- **Saboteur / security auditor — bounded ownership:** the original capacity
  check ran before an awaited submission and could race with concurrent
  requests. A reservation now counts pending submissions before the await and
  releases its slot on failure or cancellation. Pending sessions remain alive
  until that reservation is committed or dropped.
- **New hire / accessibility — confirmation focus:** confirming generation
  disables the Generate button before closing its modal. Restoring focus to
  that disabled opener lost the keyboard position. The close handler now moves
  focus to the generation heading; completion and failure focus their headings.

## Security boundaries checked

The public `/ui` surface is intentional. It creates opaque browser sessions and
accepts MP4 video requests without the private API token. The cookie is HttpOnly
and SameSite=Strict; production HTTPS also requires `UI_SECURE_COOKIES=true`.
State-changing browser requests require the session's CSRF header. Origin and
Fetch Metadata checks reject cross-origin browser requests. A configured
`UI_PUBLIC_ORIGIN` must be an exact origin and must use HTTPS with secure cookies.

Only jobs created through that browser session have public ownership entries.
Other sessions and jobs created through `/api/v1` do not acquire public access.
The private API retains bearer authentication. Static files, session responses,
job views, media URLs, and frontend storage do not need the bearer token.

The frontend constructs its embed URL from a validated YouTube ID, renders
messages as text, and accepts player messages only from the current iframe and
the expected YouTube origin. Its confirmation step submits no generation request;
the Continue action gates submission. Status retries resume the same job. Failed
or interrupted creation is not automatically repeated, since the server may have
accepted the request before the response was lost.

Media delivery supports HEAD and a single byte range. It streams bounded chunks
while retaining the file lease; the common admission middleware limits request
rate, concurrency, and stream duration. Neither playback nor download requires
loading a complete MP4 into JavaScript memory.

## Operational assumptions and limits

- Public visitors share queue, storage, request-rate, and concurrency capacity
  with private API clients. The bounds limit resource use; they do not provide
  per-person fairness or prevent deliberate exhaustion. Operators exposing the
  service should apply platform-level traffic controls appropriate to their
  deployment.
- Browser sessions expire after one hour of inactivity, and ownership is held
  in memory. Restarting the service clears browser access even if a persistent
  volume still contains the underlying job and MP4. Use one replica.
- Reverse proxies must preserve the public Host or supply an exact configured
  `UI_PUBLIC_ORIGIN`. Railway's server-provided `RAILWAY_PUBLIC_DOMAIN` supplies
  the default HTTPS origin when no explicit origin is set. Secure cookies default
  to true for a resolved HTTPS origin; an explicit setting overrides inference.
  Custom domains require a matching explicit origin. Forwarded headers supplied
  by visitors are not trusted to reconstruct the origin.
- Download uses a HEAD availability check followed by the browser's native
  attachment request. Preflight and playback failures can appear as frontend
  notifications. Transfer failures after a successful preflight are reported by
  the browser's download manager; JavaScript cannot reliably observe that
  attachment transfer without changing the download mechanism.
- Controlled browser fixtures can demonstrate confirmation, progress, errors,
  synthetic MP4 playback, and download behavior. They do not establish live
  YouTube preview, extraction, or Railway network compatibility.

The blocking source findings above were corrected during review. The remaining
items are deployment assumptions and explicitly described limitations, rather
than evidence of a completed live deployment.
