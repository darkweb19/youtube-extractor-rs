# Frame redesign verification

This report records the initial redesign before saved browser access was added.
The later session-cookie contract and its validation are documented in
[browser access](browser-access.md).

Verified locally on 2026-11-05 with Rust 1.99.1, Node.js 24, system Chromium,
Playwright as test tooling, and FFmpeg/ffprobe. The application remains embedded
vanilla HTML/CSS/JavaScript served by the existing Rust/Axum backend. No runtime
package or framework was added. API routes, schemas, authentication and CSP are
unchanged.

## Results

| Check | Result |
| --- | --- |
| `cargo fmt --all -- --check` | Passed |
| `cargo clippy --locked --all-targets -- -D warnings` | Passed |
| `cargo test --locked --all-targets` | 98 passed; one opt-in external metadata test remains ignored |
| `node --check web/app.js` | Passed |
| `node --test tests/frontend/*.test.mjs` | Passed |
| `node tests/frontend/diagnostics.test.mjs` | 22 passed, including original privacy diagnostics and new recovery/configuration cases |
| `node tests/frontend/browser.mjs` | Passed in real Chromium under the production CSP |
| `python3 -m unittest discover -s evals -p 'test_*.py'` | 11 passed |
| `python3 evals/run.py --out work/evals/frame-redesign` | 28/28 deterministic scenarios passed |
| `bash -n gcp-worker/*.sh scripts/smoke-*.sh` | Passed |
| Official `designmd lint DESIGN.md` | Zero errors and warnings |
| `git diff --check` | Passed |

Cargo commands in this environment use `RUSTUP_HOME=/workspace/.rustup`,
`CARGO_HOME=/workspace/.cargo`, and the pinned Cargo on PATH. Loopback-based
fixtures and Chromium require the executor's network permission; the initial
restricted-socket run returned EPERM, then the permitted run passed.

## Browser and media evidence

The committed browser runner exercises source review before any job creation,
keyboard Escape/back/confirmation, configuration payloads, theme persistence and
fallback, authenticated Blob output, cancellation, rejected-token recovery,
mobile layout, and source-select interaction. VM tests also protect safe
structured diagnostics, malicious output URLs, stale poll responses, idempotent
uncertain submission lookup, response UUID mismatch, corrected cancellation
credentials, and Blob revocation.

A separate local acceptance run used the **real Axum binary**, a synthetic
network-free yt-dlp fixture executable, and FFmpeg-generated one-second media.
All six output choices passed browser confirmation, authenticated POST/poll/file
requests, actual media metadata decoding, and file download: video MP4, WebM and
original/default MP4; audio MP3, M4A and original/default M4A. Sizes were 8–13 KB.
The fixtures never contacted YouTube. This verifies the complete local browser,
API, job, publication and file path, not upstream availability.

Additional manual Chromium checks covered light/dark idle and output views,
320×640 and 390×844 layouts, narrow audio configuration, native dialog bounds,
Escape, and reduced motion. Screenshots and temporary fixtures are in ignored
`work/`. The source and output states share one visual system and retain natural
page scrolling.

## Static audit limitation

The premium skill's unmodified `audit_project.py --mode strict` was run and its
JSON retained in `work/premium-audit.json`. It exits 1 with ten
`affordance.actionless-button` findings because its HTML-only detector recognizes
inline handlers but not this app's external `addEventListener` bindings. Every
flagged button has an explicit binding in `web/app.js`: theme, token visibility,
export cancellation, reset/retry, job check, cancellation retry, dialog close,
back, and final confirmation. Inline handlers would violate the preserved CSP.
Browser and VM tests exercise those owners; the audit is not presented as green.
The real placeholder-link and standards-scrollbar findings were fixed.

## Product decisions and limits

The current adapter maps `original` video/audio to MP4/M4A and always selects the
best audio source. Frame labels those defaults explicitly and offers best audio
quality rather than promising an ignored lowest-quality setting. The existing
API continues to accept its established request contract.

No deployment or production mutation was performed. The supplied Railway URL
and requested UX skill URL returned HTTP 403 through this environment's outbound
policy. The exact external UX skill could not be read; repository guidance and
the available frontend design skills were used. Live YouTube extraction and
production token/worker behavior remain unverified. The external smoke scripts
were not run without production credentials and selected authorized media.
