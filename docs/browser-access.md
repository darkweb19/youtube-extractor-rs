# Saved browser access

Frame remembers an authorized browser after the operator enters the service API
token once and selects **Save access**. A later visit checks the saved session
and hides token entry while it remains valid. **Forget this browser** removes the
cookie. Clearing cookies, expiry, or rotating the server API token requires
entry again. Source/configuration and an outstanding job remain available when
access needs to be restored.

## Credential boundary

The server exchanges an explicit valid bearer token for an opaque signed
credential. The raw token is not embedded in the cookie, returned in JSON, logged,
or written to local storage or JavaScript-readable cookies. The frontend clears
its input and transient credential after a cookie-only verification succeeds.
A blocked/rejected cookie must leave an actionable error rather than claiming
that access was saved.

The cookie is `__Secure-frame_session`, host-only, scoped to `/api`, `HttpOnly`,
`Secure`, and `SameSite=Strict`. Its one-year expiry is signed with HMAC-SHA256
using the existing API token as key, a purpose-specific message, and a random
nonce. Standard cryptography comes from the existing `ring` dependency graph;
there is no new authentication service or session database. A restart preserves
valid access; API token rotation invalidates all previous sessions.

Browser API calls use same-origin credentials and `X-Frame-Client: 1`. Cookie
requests reject cross-site use and require this non-simple header; no CORS
permission is added. Explicit invalid Authorization cannot fall back to a valid
cookie. Login requires explicit bearer authentication. Forgetting requires the
browser header but not a valid session, so an expired cookie can still be cleared.
All session responses are non-cacheable and carry no credential in their body.

The Secure attribute is unconditional, including behind Railway TLS termination;
forwarded headers do not weaken it. Use HTTPS for a hosted instance. Modern
Chromium permits Secure cookies on loopback for local testing. Cookie expiry and
deletion follow browser policy; this is saved access to a shared operator service,
not a new user/account system.

Forgetting clears this browser's credential; it does not revoke another browser
or a copied credential. Rotate API_TOKEN to revoke all saved browser credentials.
The existing bearer API, job lifecycle, rate limits, cancellation, authenticated
file path, and privacy-enhanced YouTube preview retain their contracts.

## Endpoints

See [HTTP API v1](api.md#saved-browser-access) for GET, POST, and DELETE
`/api/v1/session`. Development mode with no API token reports ready access but
never claims that a persistent cookie was saved.

## Verification

Local checks on 2026-11-05:

- `cargo test --locked --all-targets`: 103 passed; the opt-in external metadata test remains ignored.
- `cargo clippy --locked --all-targets -- -D warnings`, Rust formatting, JavaScript syntax, and `git diff --check`: passed.
- `python3 evals/run.py --out work/evals/browser-access`: 28/28 deterministic cases passed. Python evaluator tests: 11 passed.
- Official DESIGN.md lint: no errors or warnings.
- A real Chromium + Axum integration verified saving a HttpOnly/Secure/SameSite cookie, clearing the raw input, hiding token entry after reload, actual cookie-only media export and file download, validity across process restart, invalidation after API token rotation, replacement credentials, forgetting across reload, and unchanged bearer-client access. The media was generated locally and the yt-dlp fixture made no network requests.

The frontend Node VM suite passes 28 tests. `node --test tests/frontend/*.test.mjs` and `node tests/frontend/browser.mjs` pass. Chromium checks cover save/verify cookie flags, reload with hidden and empty token entry, cookie-only audio export, Forget and deletion, invalid/development/blocked cookies, 401 reauthorization retaining the same job, and Back/Escape while access setup is pending. Existing export, cancellation, keyboard, theme, narrow-layout and reduced-motion checks also pass with the new cookie transport.

The premium static auditor was also rerun. Its only findings are the thirteen known HTML-only `affordance.actionless-button` false positives: it cannot detect this app's external `addEventListener` bindings. Inline handlers would violate the preserved CSP. All controls have explicit event owners; the Node/browser tests exercise their owning workflows; the strict audit is not presented as passing.

Live YouTube extraction and production deployment are separate from this cookie-authentication change.
