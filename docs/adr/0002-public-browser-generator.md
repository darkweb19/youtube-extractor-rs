# ADR 0002: Public browser generator with session-owned jobs

Status: accepted from the user's explicit choice of a public frontend.

## Context

The user requested a simple YouTube-link form with a two-step workflow: preview
the video in a confirmation modal, then start generation only after choosing
Continue to generate. Successful generation must show a playable MP4 and an
explicit download action; failures must appear in a toast.

The existing versioned API uses a deployment bearer token. Publishing that token
in JavaScript, a video URL, or browser storage would expose privileged access to
every job. The user explicitly selected public generation rather than a private
sign-in page.

## Decision

Serve a small HTML/CSS/JavaScript interface from the Rust service at `/`, with
assets embedded in the binary. No Node.js or Python production server or frontend
build step is required. The original `/api/v1` API remains bearer-protected.

Expose a separate `/ui` browser workflow. An opaque HttpOnly, SameSite=Strict
cookie identifies an anonymous browser session; a separate request token protects
mutating requests. The server checks request origin and only allows a browser
session to access jobs it created through these routes. Knowledge of a private
API job ID does not grant browser access. Sessions and ownership maps are bounded
and expire. They are in memory, so restarting the service ends anonymous browser
access to earlier jobs even when the durable API job records and files survive.

The preview uses an official YouTube embed constructed from a validated video ID.
Opening or dismissing the confirmation modal does not create a generation job.
Continue submits one MP4 video job, initially using a 720p source-quality ceiling.
The browser polls status and streams the completed file through session-owned
media/file routes. Media playback supports byte ranges; downloads do not require
loading the complete file into browser JavaScript memory.

The public browser and authenticated API share the existing request limits,
worker queue, job retention, and storage budget. Public visitors may consume
those bounded resources. This is an explicitly public service, not an identity
or tenant system. `FRONTEND_ENABLED=false` disables the browser surface for
deployments that need the original private API only.

## Consequences

Production HTTPS deployments use `UI_SECURE_COOKIES=true`. A deployment
whose reverse proxy rewrites Host can configure its exact `UI_PUBLIC_ORIGIN`.
When unset, Railway's server-provided `RAILWAY_PUBLIC_DOMAIN` supplies the HTTPS
origin. Secure cookies default to true for a resolved HTTPS origin, with explicit
settings taking precedence. Client-supplied forwarded headers remain untrusted.
Keep one service replica: anonymous sessions and workers are local to that
process. Restart/session expiry requires generating a new browser job; the
operator can still access retained jobs through the authenticated API.

A completed YouTube preview is not proof that extraction is supported. The native
engine's existing stream limitations and hosting-network restrictions still
apply. Browser fixture success must remain distinct from real YouTube and
Railway end-to-end success.

## Verification

Require Rust integration tests for private API authentication, browser CSRF and
origin checks, session/job isolation, bounded admission, streaming ranges and
HEAD. Require real browser checks for confirmation-before-generation, duplicate
prevention, progress, actual controlled MP4 playback/download, failure feedback,
keyboard operation, responsive layout, and reduced motion.
