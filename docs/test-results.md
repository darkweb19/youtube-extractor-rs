# Verification results

> **yt-dlp migration (2026-10-23):** The runtime changed to yt-dlp 2026.10.21 and Node.js 24. The historical results below predate this migration. Passing unit tests and container checks does not establish successful live YouTube extraction or Railway access.

> **Experimental branch update (2026-10-19):** `fix/rust-yt-bot` uses
> `rusty_ytdl` with the Rust media pipeline. The test results below are
> historical and do not represent deployment acceptance for this experimental
> branch. GitHub Actions `Rusty extractor branch validation` is the
> authoritative build/lint/test status for the newest commit. No live
> YouTube or Railway download with `rusty_ytdl` has been verified here.
> The earlier successful Mac MP4 test was performed with Python yt-dlp.
>
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
