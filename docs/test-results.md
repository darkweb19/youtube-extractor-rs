# Verification results

## Railway Dockerfile syntax correction — 2026-10-17

Railway rejected unnamed cache mounts and unsupported secret mounts in the
deployment Dockerfile. The default `Dockerfile` now uses ordinary build steps
without any build mounts; CI checks that restriction before building the image.
`Dockerfile.cloud` retains temporary proxy CA mounts and explicitly named caches
for this managed development environment. `railway.toml` continues to select
the default `Dockerfile`.

The no-mount compatibility check and documentation shell checks passed. The
cloud-specific image built successfully with TLS verification and passed
`scripts/smoke-container.sh` (health, non-root user, FFmpeg, authentication,
destination validation and missing-token startup refusal). It produced the same
runtime image digest recorded below. The default Dockerfile parsed locally but
its build encountered this environment's proxy CA requirement (`UnknownIssuer`);
the hosted CI container job builds and tests that default recipe. A successful
Docker build or CI run does not establish a successful Railway deployment.

## Public frontend verification — 2026-10-17

CI follow-up: the initial browser run failed its Escape assertion when delayed
iframe loading left focus inside the cross-origin player after eight Tab presses.
The original assertion was reproduced locally with a delayed, focusable player
fixture. The corrected regression checks forward/reverse player navigation and
modal focus wrapping, then Escape from a host control. All 19 browser tests pass
with Node.js 22.23.3 and system Chromium, as well as Node.js 24.19.0. The separate
initial Rust CI test failure has not been reproduced locally; repeated complete
suites passed. These local results do not establish a successful hosted CI rerun.

The public browser generator was checked with Rust 1.99.1, Node.js 24.19.0,
Chromium, FFmpeg and the committed Cargo/npm lockfiles in the managed Linux
environment. Production remains the Rust binary with embedded HTML/CSS/JavaScript;
Node.js and Playwright are development tools only.

| Check | Result | Evidence and limits |
| --- | --- | --- |
| Formatting, Clippy and native build | Passed | `cargo fmt --all -- --check`, `cargo clippy --locked --all-targets -- -D warnings`, `cargo build --locked` |
| `cargo test --locked --all-targets` | Passed | 49 tests: 31 library, 4 API, 3 selection, 4 media/storage, 7 web. One external metadata probe remains ignored by default. |
| Frontend syntax and browser tests | Passed | `npm run check:frontend`; 19 tests passed with `PLAYWRIGHT_CHROMIUM_EXECUTABLE_PATH=/usr/bin/chromium npm run test:frontend` |
| Browser interaction and accessibility | Passed | Preview before submission, Continue exactly once, Cancel/Escape, modal focus trap and restoration, no-JS prerequisite, progress, reload recovery, errors/retries, 375px layout, 200% zoom and reduced motion |
| Generated-file UI | Passed with controlled media | An actual synthetic MP4 plays in Chromium; downloaded bytes match the fixture and FFprobe confirms audio/video. HTTP job responses and YouTube player transport are controlled fixtures. |
| Rust web integration | Passed | Session/CSRF/origin checks, ownership isolation, URL validation, public/private access separation, range/HEAD media delivery and bounded admission. The corrected HEAD capacity-release regression also passed five repeated focused runs. |
| Packaged service in Chromium | Passed | Actual Docker-hosted Rust page/assets load under its CSP; anonymous HttpOnly session, preview without POST, real job acceptance, structured `NETWORK` error toast, other-session 404 and private API 401. No bearer headers, CSP violations or uncaught exceptions observed. Only the external player transport was controlled. |
| Multi-stage Docker build and container smoke | Passed | Embedded assets packaged successfully; health, non-root runtime, FFmpeg, authentication, destination validation and missing-token startup refusal |
| Premium UI strict audit | Passed | Zero findings; design metadata lint has zero errors and 14 nonblocking orphan-token warnings |
| Live YouTube generation / Railway deployment | Unverified | The environment's YouTube network restriction remains unresolved; fixture playback and honest network-error handling do not prove successful extraction or deployment. |

Frontend image: `sha256:1fd258fcb5e7df9c29984ae91675964a2c9eece69012f8013ca141cc4b872da0`
(547,591,043 bytes uncompressed). This local image was not published or deployed.
Browser screenshots and the fixture MP4 are local verification artifacts under
`/tmp/frontend-artifacts/`, rather than committed production assets.

The API bearer token stays server-side. Public visitors can generate and access
their own browser jobs without signing in. Production HTTPS requires
`UI_SECURE_COOKIES=true`; browser ownership is process-local and expires after
one hour of inactivity. Native attachment failures after the availability check
are handled by the browser's download manager. See the
[frontend review](frontend-review.md), [browser API contract](api.md), and
[deployment guidance](deployment.md) for these operational limits.

## Initial service verification — 2026-10-16

Verified on 2026-10-16 in the managed Linux x86_64 development environment, with
Rust 1.99.1 and the committed dependency lockfile. These results describe this
implementation and the local container, not an approved production release.

| Check | Result | Evidence and limits |
| --- | --- | --- |
| `cargo fmt --all -- --check` | Passed | Final source formatting |
| `cargo clippy --locked --all-targets -- -D warnings` | Passed | No Clippy warnings |
| `cargo test --locked --all-targets` | Passed | 40 tests passed: 29 library, 4 API, 3 format selection, 4 media/storage. One external metadata test is ignored by default. |
| `cargo build --locked` | Passed | Native development binary |
| Saved environment installation script | Passed | Re-executed tool activation, locked fetch, build and the deterministic suite successfully |
| Multi-stage Docker build | Passed | Release binary built with TLS verification; temporary mounted CA bundles cover Rustup/Cargo and package installation |
| `scripts/smoke-container.sh` | Passed | Health, UID/GID 10001, FFmpeg, required auth, invalid destination rejection, missing-token startup refusal |
| Constrained container replacement | Passed | 2 CPU, 2 GiB memory, PID limit 128, dropped capabilities, no-new-privileges; retained failed-job state recovered from a named volume after replacement |
| Graceful stop | Passed | Native process and both constrained containers exited with code 0 after SIGTERM, within five seconds for the exercised idle/error workloads |
| Runtime image contents | Passed | No Cargo/Rust compiler or temporary build CA file in runtime |
| Explicit live metadata probe | Failed: external prerequisite | `NETWORK`: the environment proxy rejects YouTube CONNECT with HTTP 403 before contacting YouTube |
| Actual YouTube audio/video download | Not run | Metadata access is blocked; controlled HTTP/media pipeline tests are not a substitute |
| Railway deployment and live smoke | Not run | No selected project/service or usable Railway deployment binding was supplied |
| Large-file RSS benchmark / sustained load / backup restore drill | Not run | Bounded streaming/quota/concurrency fixtures passed; no production capacity or total-memory guarantee is claimed |

Built local image:
`sha256:b3b7b1be8dd255fb6ecfbc11157fd551329e780260bb2827b5bd3cd01bdba6bc`
(approximately 547 MB uncompressed). Local image creation does not publish a
registry image or deploy Railway.

## What the deterministic suite exercises

- URL ambiguity, host boundaries, private/reserved IP rejection, unsafe redirects,
  player body limits, typed restrictions and malformed player data.
- Native metadata fixtures and direct progressive/adaptive format selection.
- Streamed local HTTP downloads, exact-offset interruption recovery, invalid or
  ignored Content-Range, changed ETag, bounded retries and byte limits.
- Controlled local HTTP through the complete download/FFmpeg/publication pipeline
  to MP3 and adaptive MP4 with actual audio/video stream verification.
- AAC remux, MP3 conversion, explicit M4A bitrate conversion, output-size handling,
  atomic publication and partial-file cleanup.
- Queue/storage backpressure, repeated cancellation and TTL races, timeout
  classification, restart recovery, path containment, cleanup failures, file
  leases, API authentication/rate limits and stalled receiver deadlines.

An additional real native HTTP smoke verified readiness/authentication,
`NETWORK` metadata errors, accepted asynchronous jobs transitioning to a
structured network failure, deletion, and clean termination. A corresponding
container check preserved that failed job across replacement using a named
volume. These checks verify honest failure behavior, not successful extraction.

## Remaining release gates

The engine currently resolves direct public media URLs. JavaScript signature and
throttling transforms (`s`/`n`), manifest delivery, S3 storage and distributed job
coordination remain unimplemented. See [protocol support](protocol-support.md)
and [security review](security-review.md). Local persistent storage supports one
replica; it does not establish horizontal job-state sharing.

Review and save the draft network additions in environment settings before
retrying live checks: `www.youtube.com`, `youtube.com`, `youtu.be`, and
`*.googlevideo.com`. `api.github.com` is also required for the repository's pull
request workflow; its current policy denial prevents PR creation. Existing
Git HTTPS authentication was reused; no new GitHub token was requested.

The environment draft also contains tested installation and startup instructions.
Saving that draft does not apply its runtime policy or publish an environment.
Review/save it in environment settings, then publish the environment when ready.
Fresh-task restoration has not been independently verified.

Select an independent Railway project/service and configure its token, persistent
volume and hard resource limits before deployment. Then run metadata plus actual
audio/video smoke checks from that service's outbound network. If YouTube denies
that host, retain the structured error and report the restriction; using Rust
does not resolve an outbound-IP policy.
