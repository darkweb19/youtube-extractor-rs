# ADR 0001: One native Rust extraction service with bounded asynchronous jobs

Status: accepted implementation direction from the user brief; operational
capacity and reliability targets remain provisional.

## Context

The owning application needs an independently deployable HTTP service for
metadata and authorized media downloads. The user requires ownership of the
YouTube extraction engine and explicitly excludes existing downloader engines.
Previous deployment experience includes automated-access denial from Railway.
The repository initially had no application code or workload measurements.

## Decision

Use stable Rust, Axum, Tokio, Reqwest, Serde, Tracing, Tower, and a native
player-response parser in one process. Keep extraction behind a common trait.
Expose synchronous metadata and asynchronous download operations through a
versioned authenticated API. Use an in-process bounded worker pool with explicit
admission failures. Stream network and file bodies. Use FFmpeg exclusively for
local media muxing, remuxing, and transcoding.

Start with a local storage abstraction and persistent-volume deployment.
Separate source and output paths by server-generated job identifier. Persist job
state on that volume and turn interrupted jobs into honest terminal outcomes
after restart. Treat S3-compatible object storage as an optional adapter, never
as a substitute for durable queue/job coordination. One local volume and queue
support one active replica per job namespace.

Authenticate with a deployment bearer token. Permit absent authentication only
with an explicit development flag. Enforce allowlisted outbound HTTPS,
validated DNS answers, controlled redirects, request limits, byte limits,
timeouts and cancellation. Do not execute untrusted JavaScript or use a
download-engine fallback to expand compatibility.

## Alternatives

An existing extraction engine would reduce protocol maintenance but violates a
hard product requirement. Multiple services and an external broker would help
horizontal coordination but add infrastructure before workload evidence exists;
the user explicitly asks for one service initially. Synchronous downloads would
hold client connections throughout network transfers and media conversion, so
bounded jobs provide a better lifecycle for cancellation and polling.

## Consequences

We own player-protocol maintenance and may support only a subset of stream
types. Unsupported encryption, challenge, signature, or manifest cases must
fail clearly. Railway egress compatibility requires actual deployment tests and
cannot be inferred from language choice. Horizontal scaling of a shared
namespace requires shared persistent metadata and queue coordination. Bearer
authentication provides service-level trust rather than tenant isolation.

Initial budgets are two workers and sixteen queued jobs; no QPS is promised.
Provisional local API targets are p50 20 ms / p95 80 ms / p99 200 ms and 99.9%
monthly availability, with external extraction work excluded. RPO depends on
volume backup policy; RTO is a provisional 30 minutes. These are planning
assumptions, not test results or a contractual SLO. A named owner, real workload,
resource measurement, backup schedule, and restore test remain release inputs.

## Validation

Require parser fixtures, controlled streaming/retry tests, byte/concurrency
limits, cancellation and restart tests, output probing with FFmpeg tools, API
authentication/error tests, container startup, and separate authorized YouTube
end-to-end tests on local and Railway networks. Verification results must
explicitly identify skipped external checks and external restrictions.
