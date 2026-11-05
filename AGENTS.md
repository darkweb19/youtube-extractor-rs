# YouTube Extractor

A Rust/Axum service for authenticated YouTube metadata and asynchronous media
jobs, with a browser interface. This branch uses **yt-dlp** for active
extraction and FFmpeg for processing. It runs locally or delegates extraction to
an on-demand GCP worker while Railway keeps the public API and completed files.
The current GCP worker uses direct egress and Deno by default; WARP is optional,
not a prerequisite.

Inspired by [T3 Code's contributor
guide](https://github.com/pingdotgg/t3code/blob/main/AGENTS.md): understand the
real constraint, protect behavior people depend on, and prefer the smallest
change that makes correctness clear. T3 Code's tooling does not apply here.

## What we preserve

- **Stable contracts.** Keep bearer authentication, v1 response shapes, UUID job
  identity, polling, cancellation and authenticated files working in both modes.
  The browser exchanges a user-entered token for a persistent HttpOnly, Secure
  session cookie. Raw tokens stay transient; this remains a trusted operator
  tool, not an anonymous downloader.
- **Bounded work.** Queue admission, workers, storage reservations, byte limits,
  deadlines and cleanup are correctness requirements. Complete a job only after
  its output is safely published on the serving instance.
- **Honest evidence.** Fixtures and `/health` establish local behavior. They do
  not establish YouTube access, WARP connectivity, Cloudflare authorization or
  deployed reliability. State which layer was actually verified.
- **Independent deployment.** Do not introduce a dependency on the separate
  video-automation repository or choose technology merely to match another stack.

## Working together

The main thread leads analysis, architecture, decomposition, delegation,
integration and review. It must delegate production code implementation to
coding agents unless the developer explicitly overrides that workflow. Give
agents clear file ownership and inspect their integrated changes. The main
thread may edit instructions, documentation, evaluation tooling and CI directly.

Use repository skills when appropriate: `.agents/SKILLS.md` lists `cto-advisor`,
`senior-backend`, `docker-development`, `senior-devops` and
`adversarial-reviewer`. Read a skill before applying it. Local copies omit
optional Python generators; do not reinstall missing helpers. Project
constraints and the developer's task take precedence over generic skill
defaults.

## Where boundaries belong

- `src/api.rs`, `src/model.rs`, `web/`: HTTP contracts and operator interface.
  Keep handlers thin; decode worker replies using the shared public models.
- `src/jobs.rs`, `src/storage.rs`: ownership, persistent records, budgets,
  cancellation, retention and atomic publication. Local state requires a single
  Railway replica and persistent `DATA_DIR`.
- `src/ytdlp.rs`, `src/media.rs`: active extraction and local processing. Never
  accept arbitrary subprocess arguments from clients.
- `src/remote.rs`, `src/remote/tests/`: controller lifecycle, worker transport,
  idempotent delivery and bounded copying. Validate external replies before use.
- `src/extractor.rs`, `src/player.rs`, `src/network.rs`, `src/download.rs`: URL
  normalization and retained native-parser/transfer implementation. Native
  fixtures do not test the active yt-dlp runtime.
- `gcp-worker/`: packaging and systemd setup for the same binary. The separately
  provisioned Cloud Run controller implementation is not shipped here.

Read [README](README.md), [architecture](docs/architecture.md),
[deployment](docs/deployment.md), and [GCP/Cloudflare
operations](docs/gcp-cloudflare-operations.md) for cross-system constraints.
`.env.example` and `gcp-worker/worker.env.example` describe the two
configurations. Some older architecture/operations sections assume mandatory
WARP; confirm defaults against `src/main.rs`, `src/ytdlp.rs`, `src/remote.rs`
and the worker setup scripts before applying them. Keep protocol complexity at
adapter boundaries; extend existing abstractions before introducing frameworks
or services.

## Runtime and configuration sources

- Rust is pinned to `1.99.1` in `rust-toolchain.toml`; use the committed lockfile.
  Axum 0.8 and Tokio serve HTTP and asynchronous work. There is no separate
  frontend build: `src/api.rs` embeds `web/index.html`, `app.css` and `app.js`.
- Root `Dockerfile`: Rust release binary, yt-dlp `2026.10.21` with its default
  extras/EJS scripts, Python, Node.js 24 and FFmpeg. It runs as UID/GID
  `10001:10001`. `railway.toml` selects that image build and one replica.
- `gcp-worker/setup-ubuntu.sh`: Ubuntu 24.04, system-wide Deno, yt-dlp and FFmpeg.
  Keep Deno accessible to the `extractor` system user; systemd uses
  `ProtectHome=true`. `gcp-worker/package.sh` builds the Linux/amd64 bundle.
  `install-service.sh` handles installation/upgrades; review its atomic binary
  replacement and restart behavior before changing deployment steps.
- The application defaults `YTDLP_JS_RUNTIME` to `deno`, while the root Docker
  image supplies Node. Set `YTDLP_JS_RUNTIME=node` for that image or Node-based
  local runs. Do not document Node as the application default. `YTDLP_PATH`,
  `FFMPEG_PATH` and `FFPROBE_PATH` select the executables.
- `.env` is not loaded by the application. Export variables through the shell,
  container, hosting service or systemd environment file. Never commit runtime
  environment files or credentials.

`src/config.rs` defines common defaults and validation; adapter-specific
settings are read in `src/main.rs`, `src/ytdlp.rs` and `src/remote.rs`. Keep
these sources, examples and documentation consistent when changing a setting.

| Budget | Current default / constraint |
| --- | --- |
| Listener | `BIND_HOST=0.0.0.0`, `PORT=8080`; worker example binds loopback |
| Authentication | `API_TOKEN`: 32–4096 visible ASCII bytes; absent only with explicit `ALLOW_UNAUTHENTICATED=true` for development |
| Workers / queue / retained records | `WORKERS=2`, `QUEUE_CAPACITY=16`, `MAX_JOBS=256`; records must cover workers plus queue |
| Per-job / total storage | `MAX_DOWNLOAD_BYTES=1073741824`, `MAX_STORAGE_BYTES=8589934592`; total must cover a job reservation |
| Retention | `JOB_TTL_SECS=3600`; terminal transitions refresh expiry |
| API limits | `REQUESTS_PER_MINUTE=120`, `MAX_CONCURRENT_REQUESTS=32`; shared across authenticated clients/routes |
| General timeouts | `CONNECT_TIMEOUT_SECS=10`, `REQUEST_TIMEOUT_SECS=30`, `DOWNLOAD_TIMEOUT_SECS=1800`; adapters also impose their own limits |
| yt-dlp stall detection | `STALL_TIMEOUT_SECS=180`; invalid/out-of-range values fall back to 180; accepted range 30–1800 |
| Remote lifecycle | `GCP_IDLE_TIMEOUT_SECS=300` (60–3600), `GCP_START_TIMEOUT_SECS=300` (60–900) |
| Remote extraction attempts | `GCP_MAX_JOB_ATTEMPTS=2`, including the first; accepted range 1–3 |

Do not equate logical reservations with measured RSS or physical disk usage.
Sources and processed output coexist; leave filesystem headroom. Do not expose
subprocess flags, filesystem paths, proxy targets or arbitrary URLs as client
configuration. yt-dlp runs with `--ignore-config` and canonical YouTube URLs.

## HTTP and browser contracts

The route table is in `src/api.rs`; shared serialized types and validation are
in `src/model.rs`, and error codes/status mapping are in `src/error.rs`.

| Route | Contract |
| --- | --- |
| `GET /`, `/app.css`, `/app.js` | Public embedded operator UI and assets; no embedded credentials |
| `GET /health` | Public local readiness, `200`/`503`; optional Linux boot identity |
| `POST /api/v1/videos/info` | `{url}` to normalized `VideoInfo`, including optional metadata and format descriptors |
| `POST /api/v1/downloads` | `DownloadRequest` to `202` + `JobView`; optional UUID `X-Idempotency-Key` |
| `GET /api/v1/downloads/{id}` | UUID job lookup and current `JobView` |
| `GET /api/v1/downloads/{id}/file` | Completed output streamed under the same bearer authentication |
| `DELETE /api/v1/downloads/{id}` | Idempotent cancellation/deletion, `204`; clients stop polling |
| `GET /api/v1/internal/activity` | Authenticated `{busy, boot_id}` for remote ownership/idle decisions |
| `GET/POST/DELETE /api/v1/session` | Check saved access, exchange an explicit bearer token for a browser cookie, or forget browser access |

Protected v1 routes, including internal activity, accept the shared bearer
token or a valid browser session and retain the rate limit and request gate.
Cookie requests require `X-Frame-Client: 1` and reject cross-site use; bearer
clients keep their existing contract. Session creation requires an explicit
bearer token. Cookie deletion requires the browser header but no valid session. There is no per-user ownership. JSON bodies are limited
to 16 KiB. Info and download requests reject unknown fields. Download creation
requires `url`, `type` and `format`; optional fields are `quality` and
`audio_bitrate_kbps`. Accept only the normalized URL forms in
`src/extractor.rs`: exact YouTube watch, short-link, Shorts and live paths with
a valid 11-character video ID. URL acceptance does not prove playable content.

Audio formats are `original`, `m4a`, `mp3`; video formats are `original`, `mp4`,
`webm`. Quality is `best`, `worst`, or a video height from 144p through 8640p;
audio accepts only best/worst. Height caps select compatible available sources,
not guaranteed output resolution. Optional audio bitrate is 32–320 kbps and
applies only to audio jobs.

`JobView` exposes `job_id`, `status`, timestamps, `progress`, `output`, `error`
and `download_url`. Preserve nullable progress totals/percentages. `202` only
confirms admission; a status GET may return `200` with a failed job and
structured error. Completion requires local publication. Relative file URLs need
bearer or browser-session auth; never serialize upstream signed URLs, signature ciphers or internal
paths.

The UI is called **Frame**. Read `DESIGN.md` before UI work. Preserve the
two-step confirmation gate, saved browser access, same-origin authenticated requests,
honest progress, cancellation, persistent error feedback and keyboard access.
Playback uses an authenticated fetched Blob; revoke old object URLs. Do not
promise low-memory streaming or anonymous public access. Preserve CSP and other
response security headers when changing embedded assets. The raw token must
not enter persistent JavaScript storage or JavaScript-readable cookies. Session
cookies are opaque, HMAC-signed, HttpOnly, Secure, SameSite=Strict, host-only,
and scoped to `/api`; they expire after one year and are invalidated by API
token rotation. Keep startup restoration, expired-session re-entry, and
**Forget this browser** working. See `docs/browser-access.md`.

## Separate remote control, ingress and egress

Railway calls a keyless Cloud Run controller using `X-Controller-Token` to
inspect, start and stop one configured Compute Engine VM. Railway carries no
Google private key and sends no Google IAM identity token. The controller's
attached identity needs narrowly scoped lifecycle permissions for the intended
VM.

Worker ingress uses an HTTPS Cloudflare Tunnel hostname, Access service-token
policy, and the worker's separate bearer token. `cloudflared` forwards to
`127.0.0.1:8080`. yt-dlp uses direct GCP egress by default. An operator may
optionally configure `YTDLP_PROXY=socks5://127.0.0.1:40000` after validating
that route; `YTDLP_PROXY_REQUIRED=true` makes local WARP readiness mandatory.
Tunnel handles ingress; an optional WARP proxy handles extraction egress.
Neither replaces application authentication or proves that YouTube permits a
request.

Keep worker/SOCKS ports private. Keep independent public API, worker API,
controller and Access secrets. Never log tokens, complete headers, signed URLs,
raw upstream bodies or private video URLs. Never forward credentials on
redirects.

One process owns the lifecycle lock. Multiple Railway replicas need a
distributed lease and durable dispatch design first. Idle shutdown is best
effort; preserve the independent GCP maximum-runtime STOP policy as a cost guard
that can interrupt work. Worker failures currently become `DOWNLOAD_INTERRUPTED`
on Railway and can trigger bounded retry regardless of the original worker error
category.

### Controller and worker transport

Remote startup requires `GCP_CONTROLLER_URL`, `GCP_CONTROLLER_TOKEN`,
`GCP_WORKER_URL`, `GCP_WORKER_TOKEN`, `CF_ACCESS_CLIENT_ID` and
`CF_ACCESS_CLIENT_SECRET`. Keep origins HTTPS-only, with no path, query,
fragment or credentials. `GCP_WORKER_TOKEN` matches the worker's `API_TOKEN`,
not the public API token. The obsolete `GCP_SERVICE_ACCOUNT_JSON` flow is not
used.

The separately deployed controller implements `GET /status` with `200` JSON
containing a Compute Engine `status`, and `POST /start` / `POST /stop` with
`200` (already satisfied), `202` (accepted), or `409` (transitioning).
Only a `202` start grants ownership; a `200` already-running reply must not claim
an externally started VM. Pin ownership to the first verified worker boot and
revoke both persisted and in-memory ownership on a different boot. Retain that
ownership after stop acceptance until `TERMINATED` is observed; repeat idle
checks and retry if the same owned boot remains running. Missing/unverifiable
boot identity must block automatic shutdown. Keep worker activity probes bounded
including response-body reads.

Controller HTTP uses a 10-second connection timeout, 45-second request timeout
and no redirects; status bodies are bounded. Worker HTTP also rejects redirects.
Access credentials and worker bearer authentication belong only on the
configured worker origin.

Metadata uses the remote worker synchronously. Its cold-start budget can exceed
`REQUEST_TIMEOUT_SECS=30`; document and configure API/client/proxy deadlines
accordingly. Download startup happens inside asynchronous job execution, under
the job deadline. Retry uncertain job delivery idempotently; a fresh extraction
attempt gets a new worker UUID. Preserve bounded attempts, cancellation and
cleanup when copying worker files back to the API host.

`/health` validates writable local storage and FFmpeg/ffprobe. With
`YTDLP_PROXY_REQUIRED=true`, it additionally checks the local SOCKS listener and
`warp-cli status`. It does not exercise YouTube, Access or controller IAM. Keep
remote diagnostics safe and useful: operation/method/path, HTTP status, elapsed
time and job ID. Never include raw upstream bodies in error messages.

## Hit every applicable path

For a behavior change, consider local and remote execution; metadata, creation,
polling, cancellation, transfer and cleanup; browser and backend clients;
restart and retention; both deployment units and configuration examples.
Eligible unexpired queued records with saved requests can replay after restart
if queue capacity allows; running or unrecoverable active records become
`DOWNLOAD_INTERRUPTED`. Completed files survive only on the retained volume and
only until expiry. File readers hold leases so cleanup must not remove an
actively served file. Preserve partial file cleanup, local state identity checks
and remote boot ownership matching. Account for a worker still busy after
Railway restarts. Do not add replicas or assume jobs resume transparently.

## Verification and evals

Use `rust-toolchain.toml`, committed `Cargo.lock`, FFmpeg and `ffprobe`.
Deterministic tests use synthetic subprocesses, files and loopback HTTP. Actual
extraction additionally requires yt-dlp, its EJS scripts and a configured
JavaScript runtime: Deno on the GCP worker, or Node.js with
`YTDLP_JS_RUNTIME=node`.

```bash
cargo fmt --all -- --check
cargo clippy --locked --all-targets -- -D warnings
cargo test --locked --all-targets
node --check web/app.js
node --test tests/frontend/*.test.mjs
bash -n gcp-worker/*.sh scripts/smoke-*.sh
python3 evals/run.py --out work/evals
```

Iterate with the relevant target/module; run affected checks before pushing.
Read [evals/README.md](evals/README.md) before changing extraction inputs,
metadata mapping, remote contracts or the corpus. Keep generated reports under
ignored `work/` or `evals/runs/`.

Add cases for consequential behavior: malformed payloads, wrong IDs, credential
forwarding, cancellation, limits, cleanup and recovery. Extend existing cases
before adding another layer. Assert outcomes rather than mock-call mirrors;
synchronize async fixtures with events instead of arbitrary sleeps. Empty
selections and missing prerequisites must never count as passing evals.

Live acceptance is separate: `scripts/smoke-youtube.sh` and
`scripts/smoke-railway.sh` require an explicitly selected deployment,
credentials, authorized media and `ALLOW_EXTERNAL_YOUTUBE=1`. Never invoke them
implicitly from offline evals or CI. The ignored `external_metadata` test
exercises the retained native extractor, not the active yt-dlp deployment.

Use only media the operator is permitted to retrieve. Do not add DRM, CAPTCHA,
authentication or platform-access-control circumvention. Distinguish outbound
policy denials, upstream restrictions and parsing defects; never promise that a
proxy or extractor change repairs a blocked outbound IP.

## Git and documentation

Follow the developer's requested repository, branch and commit structure. For a
new task without an explicit branch, start from current `main`; on an existing
branch, preserve its history. Never force-push or discard unrelated work. Open a
pull request or merge only when asked. Use conventional commit titles; review
staged paths for secrets, generated files and unrelated changes before pushing.

Keep this guide focused on contributor decisions and invariants. Put
cross-system reasoning in architecture docs and setup/debugging in operations
docs. Keep the README practical for operators: explain setup, API usage,
architecture and limitations without a chronological account of previous
branches. Rewrite obsolete guidance instead of appending a conflicting account.
Link to maintained configuration/source and report verification that actually
ran.