# YouTube Extractor

A Rust/Axum service for authenticated YouTube metadata and asynchronous media
jobs, with a browser interface. This branch uses **yt-dlp** for active extraction
and FFmpeg for processing. It runs locally or delegates extraction to an
on-demand GCP worker while Railway keeps the public API and completed files.

Inspired by [T3 Code's contributor guide](https://github.com/pingdotgg/t3code/blob/main/AGENTS.md):
understand the real constraint, protect behavior people depend on, and prefer the
smallest change that makes correctness clear. T3 Code's tooling does not apply here.

## What we preserve

- **Stable contracts.** Keep bearer authentication, v1 response shapes, UUID job
  identity, polling, cancellation and authenticated files working in both modes.
  The browser holds the shared token only in tab memory: this is a trusted
  operator tool, not an anonymous downloader.
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
integration and review. It must delegate production code implementation to coding
agents unless the developer explicitly overrides that workflow. Give agents
clear file ownership and inspect their integrated changes. The main thread may
edit instructions, documentation, evaluation tooling and CI directly.

Use repository skills when appropriate: `.agents/SKILLS.md` lists `cto-advisor`,
`senior-backend`, `docker-development`, `senior-devops` and `adversarial-reviewer`.
Read a skill before applying it. Local copies omit optional Python generators;
do not reinstall missing helpers. Project constraints and the developer's task
take precedence over generic skill defaults.

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

Read [architecture](docs/architecture.md), [deployment](docs/deployment.md), and
[GCP/Cloudflare operations](docs/gcp-cloudflare-operations.md) for cross-system
constraints. `.env.example` and `gcp-worker/worker.env.example` describe the two
configurations. Keep protocol complexity at adapter boundaries; extend existing
abstractions before introducing frameworks or services.

## Separate remote control, ingress and egress

Railway calls a keyless Cloud Run controller using `X-Controller-Token` to inspect,
start and stop one configured Compute Engine VM. Railway carries no Google
private key and sends no Google IAM identity token. The controller's attached
identity needs narrowly scoped lifecycle permissions for the intended VM.

Worker ingress uses an HTTPS Cloudflare Tunnel hostname, Access service-token
policy, and the worker's separate bearer token. `cloudflared` forwards to
`127.0.0.1:8080`. Separately, yt-dlp uses the local WARP SOCKS proxy at
`127.0.0.1:40000` for approved egress. Tunnel handles ingress; WARP handles
extraction egress. Neither replaces application authentication or proves that
YouTube permits a request.

Keep worker/SOCKS ports private. Keep independent public API, worker API,
controller and Access secrets. Never log tokens, complete headers, signed URLs,
raw upstream bodies or private video URLs. Never forward credentials on redirects.

One process owns the lifecycle lock. Multiple Railway replicas need a distributed
lease and durable dispatch design first. Idle shutdown is best effort; preserve
the independent GCP maximum-runtime STOP policy as a cost guard that can interrupt
work. Worker failures currently become `DOWNLOAD_INTERRUPTED` on Railway and can
trigger bounded retry regardless of the original worker error category.

## Hit every applicable path

For a behavior change, consider local and remote execution; metadata, creation,
polling, cancellation, transfer and cleanup; browser and backend clients; restart
and retention; both deployment units and configuration examples. Eligible queued
records can replay after restart; running records become interrupted. Account for
a worker still busy after Railway restarts. Do not add replicas or assume jobs
resume transparently.

## Verification and evals

Use `rust-toolchain.toml`, committed `Cargo.lock`, FFmpeg and `ffprobe`.
Deterministic tests use synthetic subprocesses, files and loopback HTTP. Actual
extraction additionally requires yt-dlp, its EJS scripts and Node.js.

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
`scripts/smoke-railway.sh` require an explicitly selected deployment, credentials,
authorized media and `ALLOW_EXTERNAL_YOUTUBE=1`. Never invoke them implicitly
from offline evals or CI. The ignored `external_metadata` test exercises the
retained native extractor, not the active yt-dlp deployment.

Use only media the operator is permitted to retrieve. Do not add DRM, CAPTCHA,
authentication or platform-access-control circumvention. Distinguish outbound
policy denials, upstream restrictions and parsing defects; never promise that
a proxy or extractor change repairs a blocked outbound IP.

## Git and documentation

Follow the developer's requested repository, branch and commit structure. For a
new task without an explicit branch, start from current `main`; on an existing
branch, preserve its history. Never force-push or discard unrelated work. Open a
pull request or merge only when asked. Use conventional commit titles; review
staged paths for secrets, generated files and unrelated changes before pushing.

Keep this guide focused on contributor decisions and invariants. Put cross-system
reasoning in architecture docs and setup/debugging in operations docs. Rewrite
obsolete guidance instead of appending a conflicting account. Link to maintained
configuration/source and report verification that actually ran.
