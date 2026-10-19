# Adversarial review

> **Experimental native adapter (2026-10-19).** The active `rusty_ytdl`
> adapter removes the Python yt-dlp/Deno subprocess dependency. The existing
> Rust `SafeHttpClient` validates and pins media-transfer destinations, but the
> crate's internal metadata fetches use their own HTTP client and need separate
> egress/security assessment. Parsed signed media URLs are not exposed to
> API clients or normal logs. A working Rust build does not establish live
> YouTube/Railway access or establish an overall security certification.
>
**Scope:** The initial native extraction service: URL/player parsing, outbound
HTTP, stream transfer, FFmpeg, local storage, job lifecycle, API authentication,
container configuration, and deployment instructions. This is a review of the
implementation available during development, not a certification of subsequent
changes or a claim of production or YouTube compatibility.

**Verdict: CONCERNS.** The blocking implementation findings below were corrected
in the reviewed source. Production release still requires the final integrated
checks and the deployment conditions below.

## Findings corrected during review

| Perspective | Finding | Correction inspected |
| --- | --- | --- |
| Saboteur / Security Auditor | A second cancellation, or retention cleanup after cancellation, could remove an active worker's directory and release its storage reservation. | An independent `in_flight` flag controls cleanup and quota release. Repeated DELETE and expired-TTL regression cases hold the executor open until explicitly released. |
| Saboteur | A worker timeout was reported as cancellation because its cancellation token was also set. | The timeout outcome is tracked separately and retains the `TIMEOUT` error code. |
| Security Auditor | Request concurrency permits ended when response headers were returned, allowing file streams to outlive the configured limit. | A bounded producer owns the body, permit and file lease. Its independent deadline covers both reads and sends, including receiver backpressure. |
| Saboteur / Security Auditor | Deleting a file being served released the disk budget while its open inode still occupied storage. | File leases defer cleanup and retain quota until readers exit. |
| Saboteur | Cleanup failures could release quota despite orphaned files remaining on disk. | Cleanup-pending state reserves the full job budget; retention retries cleanup, and restart recovery removes unreferenced UUID job directories or fails closed. |
| Security Auditor | An unauthenticated health request launched a fresh FFmpeg process on every request. | A single nonblocking readiness guard and short cache bound readiness subprocess creation. |
| New Hire | Audio `worst` quality was accepted but selected the highest bitrate. | Audio selection honors the requested ordering and has explicit selection coverage. |
| New Hire / Security Auditor | Tokens accepted by configuration could be unusable as HTTP headers; debug formatting could reveal signed media credentials. | Token validation requires visible ASCII; media debug formatting redacts ephemeral URLs and cipher values. Smoke helpers use a header file rather than interpolating tokens into curl configuration syntax. |

## Remaining deployment warnings

1. **FFmpeg limits require operating-system enforcement.** `-max_alloc` caps an
   individual allocation, not the entire subprocess. Stream/probe/thread limits
   and bounded output reduce exposure but do not substitute for hard container
   memory, CPU and PID limits. Apply the documented runtime limits and measure
   representative codecs before production traffic.
2. **External acceptance remains separate from deterministic tests.** Fixture
   parsing, synthetic FFmpeg output and a healthy HTTP API do not demonstrate
   that YouTube permits metadata or media access from a Railway outbound IP.
   The release owner must retain actual local and Railway metadata, audio and
   video evidence; structured access denials must remain failures.

## Security assumptions and limits

- The shared bearer token represents one trusted service tenant. It does not
  isolate independent customers or provide per-user ownership. All implemented
  v1 operations, including completed file retrieval, pass through authentication.
- Without a proxy, outbound destinations are exact permitted HTTPS hostnames,
  redirects are checked on each hop, and connections are pinned to a validated
  public DNS result. A configured HTTPS/ALL proxy is a trusted DNS and connection
  authority and must enforce equivalent destination policy. TLS verification
  stays enabled; session CA material enters Docker builds through secret mounts.
- Direct media only is the initial protocol surface. Cipher deciphering,
  throttling transforms, DRM, sign-in/challenge responses, active live delivery
  and manifest-only media are rejected explicitly. No bypass is implemented.
- The application stores state on one local persistent volume and supports one
  replica. S3, distributed job ownership, per-tenant authorization and signed
  callbacks remain separate work. Backup/restore and state-schema compatibility
  are operator responsibilities.
- Storage admission is a logical byte budget, not filesystem block reservation.
  Leave headroom for filesystem metadata, FFmpeg packet/mux overhead and unrelated
  files. Dedicated storage and free-space monitoring remain required.
- Local-volume contents, process environment and executable paths are trusted
  administrative inputs. File containment checks do not turn a shared,
  attacker-writable volume into a secure multi-user storage system.
- Ingress still needs connection, request-header and transport limits. Bounded
  application producers prevent indefinite file/worker resource ownership but
  do not by themselves cap all idle HTTP sockets or internet traffic.

## Verification observed by the independent reviewer

Executed `cargo test --locked --lib` with the workspace Rust installation:
**21 passed, 0 failed**. That run covered the then-current cancellation/TTL,
timeout, file lease/quota, restart and containment tests, URL/player fixtures,
IP/host validation, unsafe redirects and player response body bounds.

Additional API, cleanup-failure and streamed-download cases were being added
after that run. The lead agent must run the final integrated checks against the
finished tree. This report does not claim a Docker build, live YouTube download,
Railway deployment, memory benchmark or dependency vulnerability scan was
performed by the independent reviewer.
