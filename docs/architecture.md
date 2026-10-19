# Architecture: Apify-backed video ingestion

The public Axum routes, session/CSRF browser UI and downloadable local file endpoints remain unchanged. The upstream layer now invokes Apify remotely, rather than parsing YouTube player JavaScript, resolving signatures or running yt-dlp locally.

```text
Browser or backend API client
    -> Axum authentication/session middleware
    -> POST /ui/downloads or /api/v1/downloads
    -> bounded JobManager queue
    -> DownloadPipeline
    -> Apify Actor run + polling
    -> Apify dataset result and signed media URL
    -> allowlisted, byte-limited transfer to local job storage
    -> FFmpeg transcode/remux/probe
    -> local published output + existing download route
```

The metadata endpoint calls the configured Apify metadata Actor and normalizes the result to the existing VideoInfo schema. `formats` is empty because Apify does not supply verified source itags.

`src/apify.rs` owns the outbound Apify client, run lifecycle, actor IDs and media-host authorization. `src/extractor.rs` retains strict input YouTube URL normalization. `src/download.rs` owns conversion plans, job progress and input/output storage limits. `src/jobs.rs` preserves concurrency, TTL, cancellation and recovery. `src/media.rs` runs FFmpeg only on local files, and `src/storage.rs` publishes results under UUID-scoped paths.

Security boundaries: Apify credentials remain server-side; the client never receives signed media links; media redirects must remain within exact trusted HTTPS hostnames; byte limits are enforced in streaming code; API and browser job ownership remain unchanged. Apify Actor outputs are untrusted and normalized before use. Actor completion is not proof that media transfer or transcoding succeeded.

This is still one application instance with local persistence. Scaling horizontally requires a shared job queue, distributed ownership and external object storage; those capabilities are not part of this change.
