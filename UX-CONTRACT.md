# Frame interaction contract

## Product evidence and scope

The current user request specifies a public converter with a mandatory video confirmation before generation, in-page playback after success, an explicit download, and toast errors. [docs/api.md](docs/api.md), `src/web.rs`, `src/api.rs`, and `src/extractor.rs` supply the runtime/API invariants. Earlier private-API guidance remains applicable to `/api/v1`; the browser uses the separate anonymous session bridge, never a bearer token.

| Business concern | Authoritative source | UI consequence |
| --- | --- | --- |
| Public access and ownership | User decision: anyone can generate; `src/web.rs` | No login/token form; anonymous HttpOnly cookie, in-memory CSRF; only browser-owned jobs are accessible |
| Job lifecycle | `src/jobs.rs`, `src/model.rs`, `docs/api.md` | Queued → running → completed/failed/cancelled; show genuine progress and terminal outcome |
| Retention | `src/storage.rs`, `src/web.rs` | Files are temporary; an expired file shows a persistent recovery message |
| Cancellation | `src/web.rs`, `src/api.rs` | Explicit Cancel generation uses authenticated/CSRF DELETE; status retry never creates a job |
| Billing | No billing feature | No pricing, payment, or cost claims |
| Download permission | `AGENTS.md` | Existing permission reminder; no claim that a public link establishes download authorization |

## Canonical UI Map

| Capability | Canonical owner | Source of truth | Allowed variants | Verification |
| --- | --- | --- | --- | --- |
| Form | `web/index.html` URL form, `web/url.mjs` validator, `web/app.js` submit controller | This contract + extractor URL grammar | Single video URL only | `tests/frontend/url.test.mjs`, `tests/frontend/browser.test.mjs` |
| Scrollbar | Global `web/styles.css` baseline | `DESIGN.md` | Stable document gutter; modal internal scrolling | Browser computed styles, narrow viewport |
| Toast | `toast()` in `web/app.js`, `#toast-region` | This contract | success / info / error | Browser failure/success tests |
| CRUD | `api()`, `poll()`, Continue/cancel handlers in `web/app.js` and session bridge in `src/web.rs` | Server lifecycle and this contract | Create/read/cancel; no edit or job browser | Browser full-flow + `tests/web.rs` |
| Dialog | `#confirm-dialog`, `openPreview()`/`closePreview()` | This contract | Native modal video confirmation | Keyboard, Escape, no-create-before-Continue tests |

Table selection, select/listbox, dates, and search do not exist and have no canonical owner yet. A new recurring capability must resolve ownership before implementation.

## Workflow and state ledger

| Entry/action | Validation or precondition | Pending | Success destination/feedback | Failure recovery |
| --- | --- | --- | --- | --- |
| Initial page | GET `/ui/session` | Reserved connection text, Generate disabled | Same form, no sign-in | Inline error + toast + Start a new session |
| Generate MP4 | Exact supported YouTube URL; reject ambiguous IDs and normalized/encoded paths | No mutation | Official YouTube confirmation modal | Field text + `aria-invalid`, toast, focus URL |
| Cancel / Escape / close preview | Any open preview | Remove iframe and timers | Same form; restore Generate focus; no job | Not applicable |
| Continue to generate | Snapshot of confirmed URL; session ready | Disable duplicate submission, close modal, focus generation heading | POST exactly once, retain returned job ID, poll status | Backend/network error inline + toast; never automatically retry POST |
| Status poll | Known owned job ID | Queued or running progress | Completed player, success heading/focus + toast + explicit Download MP4 | Bounded transient retries; Check status again resumes the same job |
| Cancel generation | Existing known job | Disable cancel, stop stale polling | Clear current job; confirmation toast, retain entered URL | Inline error + toast; check the same job status again |
| Download MP4 | Completed owned job | HEAD preflight; prevent repeated clicks | Native streamed attachment; informational toast | Expired/session/network error inline + toast; no full-file blob buffering |
| Session expires | Server 401 | Stop allowing generation | Clear stale job/media; Start a new session | Fresh anonymous session; no automatic resubmission |

Source: user request for the two-stage confirmation and result; server contracts for lifecycle, ownership, cancellation, and retention. English is the only owned locale. There are no list/detail or multi-route flows; the converter and modal are the consistency comparison surfaces.

## Async and recovery invariants

- All fetches use same-origin credentials and a 20-second abort deadline. Mutations include the in-memory CSRF value. No API token is loaded by JavaScript.
- A create timeout may mean the job was accepted. State that uncertainty, do not automatically repeat the POST, and advise waiting before another manual generation.
- Status polling uses five-second running/eight-second queued intervals, at most three bounded retries, and a 15-minute automatic window. Explicit status retry aborts the previous safe GET flow and reuses the job ID.
- Superseded flows and page hide abort outstanding requests; stale job responses cannot replace the selected job. BFCache restoration initializes/rechecks the current session.
- Only the current opaque job ID is stored in sessionStorage, guarded for unavailable storage. Reload rechecks that job through the same ownership guard. No links, tokens, or file blobs are persisted.
- Backend copy is rendered as text. Media error/stall and preview load/error failures use the same toast system and persistent contextual copy. YouTube preview events must match both iframe source and exact trusted origin.

## Accessibility and layout

Use native form, label, button, link, dialog, progress, and video semantics; target WCAG 2.2 AA. The product owns validation (`novalidate`) and focuses the invalid URL with a correction hint. IME composition must not submit. Modal focus is contained, Cancel initially receives focus, Escape closes without generation, and dismissed media is destroyed. Confirming transfers focus to the generation heading; completion scrolls the result into view without animation and focuses its heading, and failure focuses the persistent error message.

Every error appears in a shared live toast plus field/contextual text; it never relies on color alone. Error toasts persist until dismissed/replaced. Success/info expire after six seconds with hover/focus pause. Owned scroll regions receive global scrollbar tokens automatically, with forced-colors support. Preserve native focus visibility, reduced motion, 16:9 media reservations, busy-button geometry, natural document scrolling, and reachable controls at narrow widths and 200% reflow.

## Verification

Run `npm run check:frontend` and `npm run test:frontend`; browser checks require Chromium and FFmpeg. Runtime evidence lives in `tests/frontend/browser.test.mjs`; URL invariants in `tests/frontend/url.test.mjs`; Rust browser-bridge ownership/security/media evidence in `tests/web.rs`. Controlled media verifies rendering and actual byte downloads, not live YouTube extraction or Railway egress. Design tokens map to the one CSS root block as documented in `DESIGN.md`.
