> **Runtime note (2026-10-23):** The active extractor is now yt-dlp 2026.10.21, backed by EJS scripts and Node.js. The native player parser, signature and manifest discussion below describes legacy code retained for unit tests, not the active YouTube download engine.

# YouTube protocol support and legacy parser

## Experimental active path: rusty_ytdl

This branch uses `rusty_ytdl = 0.7.4` for native Rust metadata extraction and
format discovery. The Rust service reconstructs a canonical watch URL from a
strictly validated 11-character video ID. The crate's parsed formats are
normalized to the existing model; live/HLS/DASH-only/unsupported formats and
unsafe media destinations are omitted. Signed media URLs are retained privately
for download execution and excluded from public JSON.

The existing Rust `DownloadPipeline` selects compatible separate video/audio
formats and streams direct HTTPS media into a UUID job workspace, within the
worker quota and cancellation deadlines. FFmpeg processes completed local
inputs and the output is published through the existing authenticated storage.
This is **not** a new YouTube anti-bot workaround: Railway previously returned
HTTP 429 and a sign-in verification challenge, which the library cannot be
assumed to solve. DRM, sign-in cookies, automated verification and protected
streams remain unsupported.

Because `rusty_ytdl` is older than the current yt-dlp ecosystem, live
compatibility must be tested independently for authorized media, including
the exact Railway outbound network.

## Legacy native parser (inactive in production)

The remainder of this document describes the experimental native Rust parser,
`src/extractor.rs` and `src/player.rs`, which remain available for test
fixtures but are **not used by the default production server or job executor**.
These historical limitations are not representative of yt-dlp's supported
protocols.

## Inputs and source of player data

`extractor::normalize_url` accepts full HTTP/HTTPS URLs identifying exactly one 11-character video ID using ASCII letters, digits, `_`, or `-`:

- `youtube.com`, `www.youtube.com`, or `m.youtube.com` with `/watch?v=...`.
- `youtu.be/<id>`.
- The same three long hosts with `/shorts/<id>` or `/live/<id>`.

Credentials, custom ports, unsupported hosts/paths, encoded or dot-normalized paths, duplicate `v` parameters, and conflicting path/query identifiers are rejected. Other query parameters are not forwarded. The fetcher reconstructs `https://www.youtube.com/watch?v=<id>`; it never fetches a user-supplied URL directly. An ordinary URL fragment is not used for requests.

The parser locates an assignment to `ytInitialPlayerResponse`, including the `window["ytInitialPlayerResponse"] = ...` form. It extracts one balanced JSON object while respecting quoted strings and escaped characters, then deserializes typed player fields. This is an assignment-marker scan, not a general HTML/JavaScript parser. It may identify a marker inside unrelated upstream text, and a page using another representation can fail. The parser does not evaluate JavaScript, repair malformed JSON, or guess an undocumented API request as a fallback. The response video ID must match the requested ID.

The HTTP body is bounded before parsing; main currently supplies a 4 MiB limit. Serde's normal recursion limit remains enabled. Redirects are bounded and each destination is validated. A direct connection resolves and pins only public IP addresses; an explicitly inherited outbound proxy owns destination DNS and must itself enforce safe egress. No automatic redirect, cookie jar, custom user authentication, or alternate outbound identity is used. Injected proxy and TLS trust settings are preserved.

## Metadata and direct streams

The supported typed fields are:

| Player field | Service output or behavior |
| --- | --- |
| `playabilityStatus.status` and `reason` | Playability classification; upstream reason text is not echoed to clients |
| `videoDetails.videoId`, `title`, `shortDescription` | Identifier, title, description |
| `lengthSeconds`, `channelId`, `author` | Optional duration and channel fields |
| `thumbnail.thumbnails` | Thumbnail URL, width, height; these URLs are not fetched |
| `streamingData.formats`, `adaptiveFormats` | Candidate progressive and adaptive streams |
| Format `itag`, `mimeType`, `bitrate`, `width`, `height`, `fps`, `contentLength` | Typed format metadata; optional numeric string fields must parse to be populated |
| MIME `codecs`, `audioQuality`, `audioSampleRate` | Codec list and audio/video presence classification |
| `isLive` | Active-live rejection |
| `isLiveContent` | Does not itself reject a completed replay |

A format needs an allowed HTTPS URL on a subdomain of `googlevideo.com`. MP4 video, MP4 audio exposed as M4A, and WebM audio/video MIME types are supported. Signed URLs remain internal and are not serialized in the public metadata or job API. The original signed query is preserved; the service does not mint signatures.

A `signatureCipher`/`cipher` containing a URL and an already supplied `sig` or `signature` can be decoded as URL-encoded fields. Only the `sig` or `signature` destination parameter is accepted. A cipher containing `s` is only resolved if a strictly bounded native parser recognizes a unique, classical swap/reverse/splice signature function in the same-origin public player script. This is not a general JavaScript decoder. URLs with an `n` query parameter are retained unchanged and may be throttled or rejected by YouTube; the `n` transformation is not implemented. Unknown MIME formats are skipped. Invalid media destinations fail explicitly rather than being fetched.

A successful metadata response contains at least one supported direct format. If all candidates require unimplemented resolution, the result is `UNSUPPORTED_FORMAT`, rather than a misleading successful empty list. When supported direct formats coexist with unsupported candidates, only supported formats are returned. This release does not expose a separate per-format omission reason.

## Explicit failures

| Condition | Result |
| --- | --- |
| HTTP DNS/connection/TLS/proxy failure | `DNS` or `NETWORK`, with timeout separated where identifiable |
| HTTP 429 or a recognized playback rate-limit reason | `UPSTREAM_RATE_LIMITED` |
| HTTP 401/403 | `ACCESS_RESTRICTED`; an identifiable proxy denial stays `NETWORK` |
| Rejected HTTP response with an already expired `expire` query | `EXPIRED_URL` |
| Sign-in, age/content checks, unavailable playback, consent or recognized verification page | `ACCESS_RESTRICTED` |
| Missing/malformed player JSON, changed schema, mismatched video ID, unknown playability status | `PLAYER_CHANGED` |
| Active live or offline live with no playable replay | `UNSUPPORTED_FORMAT` |
| Declared DRM, no supported clear direct stream, or manifest-only delivery | `UNSUPPORTED_FORMAT` |

Classification is necessarily bounded by what the upstream exposes. An unfamiliar denial page without a recognizable marker may appear as `PLAYER_CHANGED`; a generic proxy's HTTP denial without identifying headers may resemble an upstream denial. Errors never include a signed media URL, raw cipher, or arbitrary upstream error text. There is no retry using cookies, CAPTCHA execution, a different identity, or a private proxy pool.

## JavaScript URL-resolution assessment

The service now includes a limited native recognizer of classical signature functions but not an embedded JavaScript engine, general signature support, or an `n` transform. No current player JavaScript samples have been validated against authorized live media from this environment; the environment's current egress policy blocks YouTube. Changing implementation language does not remove that restriction.

| Option | What must be implemented | Benefits | Limitations and required controls |
| --- | --- | --- | --- |
| Constrained native transform parser | Locate the relevant function and dependencies, recognize an explicitly supported syntax/operation subset, execute it over a bounded byte/string input | Small runtime, no JavaScript host APIs, auditable operations, predictable memory | Minified JavaScript and dependency shapes change; `n` cannot be assumed to use the same operations as `s`; unknown syntax must fail closed rather than be approximated |
| Sandboxed general-purpose JavaScript runtime | Dependency extraction and a minimal execution environment, with public-player fixtures and correct URL argument handling | Can represent legitimate transforms that exceed a narrow native operation set | A general interpreter creates a larger attack surface, and evaluating the entire remote player may require browser APIs or unrelated code; engine integration alone does not discover the correct function or prove output validity |

A constrained native path is the preferred first experiment for a *measured, current* public-player function whose complete operation set can be proven small. Do not assume a historical reverse/slice/swap recipe is sufficient. Identify the function from captured current inputs, preserve the exact input/output test vectors, reject unsupported syntax, cap source length and operation counts, and version the parser. Player source hashes help reproduce a fixture and identify upstream changes; a hash alone does not prove a transform is safe or correct. The `s` and `n` paths need independent evidence and tests.

If that experiment cannot cover an observed legitimate function without becoming a partial JavaScript interpreter, evaluate a maintained general-purpose runtime such as Boa or QuickJS bindings. These are candidates for a separate compatibility and security evaluation, not dependencies selected by this implementation. Measure supported Rust/toolchain/platform versions, compilation footprint, API-level memory/stack limits, interruption support, and deterministic test behavior before choosing one.

A JavaScript prototype must expose no filesystem, network, environment, process, module-loading, timers, or other host I/O. It must enforce input/output byte limits, heap and stack limits, and an execution budget through the runtime's interruption mechanism. An async Rust timeout alone is insufficient for a non-yielding interpreter. Prefer a killable worker process with OS memory/CPU limits if hard interruption cannot be demonstrated. Ensure concurrency and process teardown remain bounded on cancellation. Railway's available process-isolation controls must be verified rather than assumed.

Only consider URL transforms after the public player reports playable, clear content. DRM, sign-in, CAPTCHA, consent, and other access denials remain terminal. Do not execute arbitrary page scripts or expand host capabilities to make a denied or unsupported case work. Do not advertise general decipher support without controlled transform tests, malicious/looping-program tests, and an authorized end-to-end media download using current player data on the intended host.

## Manifest roadmap

HLS and DASH manifest parsing is not implemented. `hlsManifestUrl` and `dashManifestUrl` currently establish that a manifest-only response is unsupported. They are never sent to FFmpeg; FFmpeg receives local files only. Direct formats can still work when a manifest URL is also present.

A future native manifest module must reuse the safe HTTP client and maintain a bounded, typed intermediate representation separate from download execution. Required behavior includes:

1. Fetch only validated HTTPS media origins, with byte/depth/redirect limits. Resolve each inherited or relative URL and validate it before fetching; never delegate remote playlist retrieval to FFmpeg.
2. Reject HLS encryption/key retrieval and DASH `ContentProtection`/DRM in this scope. Disable XML external entities and DTD processing. Do not accept alternate protocols, file URLs, or manifest-specified credentials.
3. For DASH, parse `MPD`/`Period`/`AdaptationSet`/`Representation`, inherited MIME/codecs, bandwidth, resolution, frame rate, and hierarchical `BaseURL`. Handle `SegmentBase` initialization/index ranges, `SegmentList`, and `SegmentTemplate`/`SegmentTimeline` as distinct mechanisms. Bound expanded segment counts, range arithmetic, and total bytes. Dynamic MPDs require a separately designed live lifecycle and remain unsupported initially.
4. For HLS, distinguish master and media playlists, choose matching audio/video renditions, support bounded VOD playlists with `ENDLIST`, initialization maps and byte ranges, and validate every segment URL. Handle discontinuities and timestamp alignment explicitly. Sliding/live windows, encryption, and unrecognized tags that affect interpretation remain unsupported initially.
5. Extend the download model to describe initialization objects, ordered segments and byte ranges. Current `MediaFormat.url` represents one complete object and is insufficient for a general segmented presentation. Apply the existing job byte quota, progress, retries, cancellation and cleanup to the aggregate, including audio/video pairs.
6. Define expiration and refresh behavior. A new manifest must not silently change representation, sequence, or initialization bytes during a resumed job. Verify representation continuity before reusing partial output.
7. Test with controlled manifests for relative/absolute URL resolution, inherited fields, malicious redirects, XML entity attempts, overflow/expansion, missing segments, interruptions, invalid ranges, encryption, mismatched audio/video, cancellation, and FFmpeg output validation. Then verify authorized current YouTube samples locally and on Railway before claiming compatibility.

A narrow static DASH subset could fit current types: a static MPD with a single, numeric-ID `Representation` per candidate, no protection or segmentation constructs, and a single validated `BaseURL` demonstrably pointing to a complete self-contained media object. It still needs bounded XML parsing, inheritance handling, complete-object verification and controlled fixtures. A `BaseURL` alone does not prove that it names a complete playable file; many DASH presentations require initialization and segment/index ranges. This subset is a proposed follow-up, not implemented support, and must not be described as general DASH or YouTube manifest compatibility.

## Updating compatibility fixtures

`tests/fixtures/player-direct.json`, `player-watch.html`, and `player-restricted.json` are synthetic. The tests cover escaped braces/quotes, progressive and adaptive metadata, replay semantics, mismatched identifiers, access denials, unsupported transforms, DRM and manifests. Network unit fixtures cover unsafe redirects, bounded chunked bodies and status classification.

When the upstream changes, record the failure category first. Capture only public, authorized player/manifest data through approved egress, remove signed URLs, signatures, cookies, user identifiers and other credentials, and minimize it to the failing schema. Add a versioned fixture plus an explicit parser test before changing a recognized field or syntax boundary. Keep assertions about known failures when adding support. Do not copy an existing extraction engine's implementation or claim synthetic fixtures are live samples.

The ignored `tests/external_metadata.rs` probe requires `AUTHORIZED_TEST_YOUTUBE_URL` and approved egress. It checks metadata and supported format discovery only. Separate authorized audio/video downloads, output inspection, Docker runtime checks and Railway deployment checks are required for the full acceptance criteria. Record the source host, deployment environment, test date, observed outcome and limitation without storing signed URLs or tokens.
