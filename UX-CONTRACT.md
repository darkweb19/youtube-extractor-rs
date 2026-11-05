# Frame interaction contract

This single workflow uses one DOM, one stylesheet, and one JavaScript state owner. Business rules remain in [API v1](docs/api.md), [src/model.rs](src/model.rs), [src/api.rs](src/api.rs), and [AGENTS.md](AGENTS.md); this document records their UI consequences.

## Canonical UI Map

| Capability | Canonical owner | Source of truth | Allowed variants | Verification |
| --- | --- | --- | --- | --- |
| Select/Listbox | Labeled native selects in web/index.html and app.js configuration handling | DESIGN.md and src/model.rs | Native platform popup | Browser keyboard and selection tests |
| Form | source-form, requireToken, and configuration validation in web/app.js | API request validation and this contract | URL, token, output configuration | Node and browser failure-path tests |
| Scrollbar | Global web/app.css baseline | DESIGN.md | Document and dialog geometry | Computed styles and narrow browser layout |
| Toast | toast in web/app.js and toast-region | This contract | Success and error | Diagnostic privacy and live-region tests |
| CRUD | generate, poll, cancel, loadVideo, reset in web/app.js | docs/api.md lifecycle | Submit, inspect, cancel, collect | Node diagnostics and browser workflow |

## Workflow and recovery

1. Submitting a recognized source opens the native confirmation dialog. No extraction job is created. The canonical watch URL and privacy-enhanced YouTube embed identify the source. Closing or pressing Escape returns to source entry and clears the embed.
2. Final confirmation validates the in-memory token and configured output before posting the existing DownloadRequest. Duplicate submissions are disabled. Format and quality values follow server validation; a resolution is a ceiling, not a guarantee. Optional audio bitrate applies only to audio.
3. Queue admission is pending work. Poll the accepted UUID until completed, failed, or cancelled. Show real percent only when available; keep an honest indeterminate state otherwise. A stale run cannot overwrite a newer run.
4. Completed output is fetched once with bearer authentication from the exact accepted job file route. Playback and download share its Blob URL. Replaced/reset/unloaded previews revoke that URL. Unsupported browser codecs leave explicit download available.
5. Cancellation aborts local polling and sends the idempotent DELETE. Success returns to source entry. An uncertain failure retains the known UUID and offers an explicit recovery action. Poll/request failure must not silently create replacement work.
6. Persistent inline/output errors explain the recovery path. Toasts are supplementary and dismissible. Entered source and configuration survive recoverable errors. A rejected token remains editable; no automatic retry loop runs against access restrictions.

## Authentication and privacy

The shared token is entered by the operator and held only in tab memory. It is masked by default and may be shown explicitly. Never put it in URLs, storage, diagnostic logs, HTML, or requests to the embed. Only the non-sensitive theme preference may persist. Existing CSP and response security headers remain unchanged. Safe diagnostics select known fields and redact private values.

## Accessibility and layout

Use semantic controls, associated labels and help/errors, visible focus, real disabled states, and native dialog focus behavior. Invalid fields expose aria-invalid and receive first-error focus. Status changes are perceivable without color. Light/dark, reduced motion, mobile, long file names, and keyboard use retain every action. Only the native select popup is platform-owned. This is an English single-operator product with no tables, date pickers, multi-user permissions, or destructive bulk workflows.

## Evidence boundary

Existing Node diagnostics tests and new workflow/browser regressions protect UI contracts. Rust API/media/job tests protect the backend. Local synthetic media and API fixtures establish frontend behavior and playable local output, not access to YouTube or production Railway availability. Live acceptance requires the selected deployment, credentials, and authorized media.
