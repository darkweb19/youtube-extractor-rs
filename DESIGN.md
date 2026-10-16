# Frame — UI Design Context

## Product and audience
Frame is the single-operator browser interface of the standalone Rust YouTube extractor service. Its job is to help an authorized operator confirm a video before spending extraction resources, then produce a locally downloadable MP4. This is a *product tool*, not a public streaming or media-discovery site.

## North star
A lightweight video edit-bay console: precise labels, a two-step approval handoff, and a clear finished-file surface. The signature is a **video confirmation gate** between input and extraction—preview is deliberately separate from generation.

Avoid dashboard clutter, false download progress, dark neon themes, gradients-for-decoration, and media thumbnails without a real source.

## Design tokens
The canonical token values live in `web/app.css` under `:root`; this document mirrors their semantic roles and rationale. Changes to durable tokens should update both.

| CSS token | Current value | Role |
| --- | --- | --- |
| `--paper` | `#f4f7fd` | Canvas / cool editing desk |
| `--surface` | `#fff` | Foreground workspace |
| `--ink` | `#162347` | Headings, labels, primary text |
| `--ink-quiet` | `#67738f` | Supporting information |
| `--blue` | `#3459de` | Primary actions, focus hierarchy |
| `--blue-strong` | `#264bc8` | Primary hover state |
| `--blue-wash` | `#edf2ff` | Activity and index markers |
| `--border` | `#dfe5f1` | Surface separation |
| `--coral` | `#ff846e` | Small editorial accent only |
| `--success` | `#157a5b` | Completed file feedback |
| `--danger` | `#c13e50` | Failures and interruptions |

Use the display stack `Trebuchet MS / Avenir Next / Segoe UI` for compact, emphatic headings; `Segoe UI / Helvetica Neue / Arial` for UI text; system monospace for metadata and stage labels. No externally loaded fonts or runtime image assets.

## Layout
- Above 860px, show source entry and output as a two-column workspace on a soft canvas.
- At 860px and below, stack entry and output with natural document scrolling.
- At narrow widths, let the confirmation dialog fill most of the viewport while preserving a visible close/back action.
- Preserve the output panel's footprint across idle, working, completed, and error states. The modal's 16:9 embed owns its aspect ratio.

## Interaction behavior
- Source form accepts recognized YouTube link formats and sanitizes to a canonical watch URL.
- The first **Generate MP4** click opens a native modal YouTube embed. Only **Continue to generate** creates a backend job.
- The API token is entered explicitly by the trusted operator. Never bundle it in static files or save it to persistent web storage.
- Job execution is pessimistic and server-confirmed: queued/running are not success. Percentage appears only when the backend supplies real numeric progress.
- Errors appear in a transient toast *and* a persistent output panel. Do not dismiss an actionable failure without a way to retry.
- On success, reuse one authenticated fetched Blob for in-page video playback and explicit MP4 download; revoke old object URLs.
- All interactive controls support keyboard focus, hover, visible disabled states, and mobile touch. Confirmation uses native `<dialog>` semantics.
- Motion is subtle and honors `prefers-reduced-motion`. Scrollbars remain visible and use the shared tokenized baseline.

## Verification and limitations
- Backend route/HTML/CSS/JS availability, security headers, and existing API authorization are covered in `tests/api.rs`.
- GitHub CI validates `web/app.js` syntax with Node and runs the Rust test suite/container smoke tests.
- Real browser interaction and actual YouTube media extraction on Railway require deployment verification; CI fixtures are not evidence of upstream accessibility.
- Blob playback is a simple first version for one authorized operator, not a memory-efficient media-streaming solution for very large files.
