(() => {
  "use strict";
  const $ = (id) => document.getElementById(id);
  const ui = {
    form: $("source-form"), url: $("youtube-url"), token: $("api-token"),
    access: $("access-settings"), button: $("preview-button"),
    dialog: $("confirm-dialog"), iframe: $("youtube-preview"),
    external: $("open-youtube"), confirm: $("continue-button"),
    pill: $("stage-pill"), empty: $("empty-state"), working: $("working-state"),
    success: $("success-state"), error: $("error-state"),
    workTitle: $("work-heading"), workText: $("work-description"),
    progressArea: $("progress-area"), progress: $("job-progress"),
    progressNumber: $("progress-number"), cancel: $("cancel-button"),
    video: $("result-video"), filename: $("file-name"), filesize: $("file-size"),
    download: $("download-link"), errorText: $("error-description"),
    toasts: $("toast-region")
  };
  const state = { token: "", url: "", jobId: null, controller: null, objectUrl: null, run: 0, busy: false, phase: "idle", view: null };
  const jobStatuses = new Set(["queued", "running", "completed", "failed", "cancelled"]);
  const progressStages = new Set(["extracting", "downloading", "processing", "completed"]);
  const errorCodes = new Set(["INVALID_REQUEST", "UNAUTHORIZED", "NOT_FOUND", "QUEUE_FULL", "RATE_LIMITED", "CANCELLED", "NETWORK", "DNS", "ACCESS_RESTRICTED", "UPSTREAM_RATE_LIMITED", "PLAYER_CHANGED", "UNSUPPORTED_FORMAT", "EXPIRED_URL", "DOWNLOAD_INTERRUPTED", "DOWNLOAD_TOO_LARGE", "FFMPEG_FAILED", "STORAGE", "TIMEOUT", "INTERNAL"]);
  const uuid = (value) => typeof value === "string" && ![state.token, ui.token.value].includes(value) && /^[0-9a-f]{8}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{12}$/i.test(value) ? value : null;
  let requestNumber = 0;
  function context() { return { run: state.run, phase: state.phase, jobId: uuid(state.jobId) }; }
  function preparationContext() { return { run: state.run, phase: state.phase, jobId: null }; }
  function routeName(path) {
    if (path === "/api/v1/downloads") return path;
    if (/^\/api\/v1\/downloads\/[0-9a-f-]{36}\/file$/i.test(path)) return "/api/v1/downloads/:job_id/file";
    if (/^\/api\/v1\/downloads\/[0-9a-f-]{36}$/i.test(path)) return "/api/v1/downloads/:job_id";
    return "unknown";
  }
  function diagnostic(level, event, details = {}, captured = context()) {
    // Deliberately select fields: never pass requests, responses, errors or state to console.
    const entry = { timestamp: new Date().toISOString(), event, run: captured.run, step: captured.phase, job_id: uuid(captured.jobId) };
    for (const key of ["request_id", "elapsed_ms", "http_status", "controller_http_status", "bytes", "playback_code"])
      if (Number.isSafeInteger(details[key]) && details[key] >= 0) entry[key] = details[key];
    if (["GET", "POST", "DELETE"].includes(details.method)) entry.method = details.method;
    if (["/api/v1/downloads", "/api/v1/downloads/:job_id", "/api/v1/downloads/:job_id/file"].includes(details.route)) entry.route = details.route;
    if (["GET /status", "POST /start", "POST /stop"].includes(details.controller_operation)) entry.controller_operation = details.controller_operation;
    if (errorCodes.has(details.error_code)) entry.error_code = details.error_code;
    if (["http", "timeout", "abort", "network", "protocol", "job_failed", "job_cancelled", "validation", "playback", "unknown"].includes(details.error_kind)) entry.error_kind = details.error_kind;
    if (jobStatuses.has(details.job_status)) entry.job_status = details.job_status;
    if (progressStages.has(details.progress_stage)) entry.progress_stage = details.progress_stage;
    if (["idle", "working", "success", "error"].includes(details.view)) entry.view = details.view;
    console[level]("[Frame]", entry);
  }
  function step(next, captured) {
    if (state.phase === next) return;
    state.phase = next; diagnostic("info", "workflow.step", {}, captured ? { ...captured, phase: next } : context());
  }
  function safeMessage(value, fallback) {
    if (typeof value !== "string" || !value.trim()) return fallback;
    let message = value;
    for (const secret of [state.token, ui.token.value])
      if (secret) message = message.split(secret).join("[redacted]");
    return message.replace(/(?:https?:\/\/|blob:)\S+/gi, "[redacted URL]")
      .replace(/Bearer\s+\S+/gi, "Bearer [redacted]").slice(0, 800);
  }
  function appError(message, details = {}) {
    const error = Error(message); error.diagnostic = details; return error;
  }
  function backendError(payload, fallback, kind, httpStatus) {
    const message = safeMessage(payload?.message, fallback);
    const details = { error_kind: kind, error_code: payload?.code, http_status: httpStatus };
    // Only fixed controller operations and numeric statuses cross into console diagnostics.
    const match = /^VM controller (GET \/status|POST \/start|POST \/stop) returned HTTP ([1-5][0-9]{2})\./.exec(message);
    if (match) { details.controller_operation = match[1]; details.controller_http_status = Number(match[2]); }
    return appError(message, details);
  }
  function toast(message, success = false) {
    const item = document.createElement("div");
    item.className = "toast";
    item.dataset.tone = success ? "success" : "error";
    item.setAttribute("role", success ? "status" : "alert");
    const label = document.createElement("span");
    label.className = "toast-message";
    label.textContent = message;
    const close = document.createElement("button");
    close.type = "button"; close.className = "toast-close";
    close.setAttribute("aria-label", "Dismiss notification"); close.textContent = "×";
    close.addEventListener("click", () => item.remove());
    item.append(label, close); ui.toasts.append(item);
    setTimeout(() => item.remove(), success ? 5500 : 9000);
  }
  function validateLink(value) {
    let url;
    try { url = new URL(value.trim()); } catch { throw Error("Enter a valid YouTube video link."); }
    if (!["https:", "http:"].includes(url.protocol) || url.username || url.password || url.port)
      throw Error("Enter a standard YouTube video link.");
    const host = url.hostname.toLowerCase();
    let id = "";
    if (["youtu.be", "www.youtu.be"].includes(host)) id = url.pathname.split("/")[1] || "";
    else if (["youtube.com", "www.youtube.com", "m.youtube.com", "music.youtube.com", "youtube-nocookie.com", "www.youtube-nocookie.com"].includes(host)) {
      const parts = url.pathname.split("/").filter(Boolean);
      if (url.pathname === "/watch") id = url.searchParams.get("v") || "";
      else if (["shorts", "live", "embed", "v"].includes(parts[0]) && parts.length === 2) id = parts[1];
    } else throw Error("Only YouTube URLs are supported.");
    if (!/^[A-Za-z0-9_-]{11}$/.test(id)) throw Error("The link does not contain a valid 11-character video ID.");
    return { canonical: "https://www.youtube.com/watch?v=" + id, embed: "https://www.youtube-nocookie.com/embed/" + id + "?rel=0" };
  }
  function discardPreview() {
    ui.video.pause(); ui.video.removeAttribute("src"); ui.video.load();
    ui.download.removeAttribute("href");
    if (state.objectUrl) URL.revokeObjectURL(state.objectUrl);
    state.objectUrl = null;
  }
  function mode(next) {
    if (state.view !== next) { state.view = next; diagnostic("info", "ui.state", { view: next }); }
    ui.empty.hidden = next !== "idle"; ui.working.hidden = next !== "working";
    ui.success.hidden = next !== "success"; ui.error.hidden = next !== "error";
    ui.pill.dataset.stage = next;
    ui.pill.textContent = { idle: "WAITING", working: "PROCESSING", success: "READY", error: "NEEDS ATTENTION" }[next];
    state.busy = next === "working"; ui.button.disabled = state.busy; ui.url.disabled = state.busy;
    ui.cancel.hidden = true; ui.progressArea.hidden = true;
  }
  function working(title, description, canCancel = false, percentage = null) {
    mode("working"); ui.workTitle.textContent = title; ui.workText.textContent = description;
    ui.cancel.hidden = !canCancel;
    if (Number.isFinite(percentage) && percentage >= 0 && percentage <= 100) {
      ui.progressArea.hidden = false; ui.progress.value = percentage;
      ui.progressNumber.textContent = Math.round(percentage) + "%";
    }
  }
  function failure(error, prefix = "", captured = context()) {
    const message = prefix + safeMessage(error?.message, "Generation failed.");
    const details = error?.diagnostic || { error_kind: "unknown" };
    diagnostic(details.error_kind === "job_cancelled" ? "info" : "error", details.error_kind === "job_cancelled" ? "workflow.cancelled" : "workflow.failed", details, captured);
    const extra = ["Step: " + captured.phase, uuid(captured.jobId) ? "Job: " + captured.jobId : "", errorCodes.has(details.error_code) ? "Code: " + details.error_code : ""].filter(Boolean).join(" · ");
    step(details.error_kind === "job_cancelled" ? "cancelled" : "failed");
    mode("error"); ui.errorText.textContent = message + " " + extra; toast(message);
  }
  function requireToken() {
    const value = ui.token.value;
    if (value.length < 32 || value.length > 4096 || !/^[!-~]+$/.test(value)) {
      ui.dialog.close(); ui.access.open = true; ui.token.focus();
      diagnostic("warn", "access.validation_failed", { error_kind: "validation" }, preparationContext());
      toast("Enter your private API token (at least 32 characters) to generate a video.");
      return false;
    }
    state.token = value; diagnostic("info", "access.ready", {}, preparationContext()); return true;
  }
  async function request(path, options = {}) {
    const captured = options.context || context();
    const started = performance.now();
    const details = { request_id: ++requestNumber, method: options.method || "GET", route: routeName(path) };
    const level = details.method === "GET" && !options.file ? "debug" : "info";
    const elapsed = () => Math.round(performance.now() - started);
    diagnostic(level, "request.started", details, captured);
    const controller = new AbortController();
    const timer = setTimeout(() => controller.abort(), options.file ? 120000 : 30000);
    const forward = () => controller.abort();
    if (options.signal) {
      if (options.signal.aborted) controller.abort();
      else options.signal.addEventListener("abort", forward, { once: true });
    }
    try {
      const result = await fetch(path, {
        method: options.method || "GET",
        headers: { Authorization: "Bearer " + state.token, ...(options.body === undefined ? {} : { "Content-Type": "application/json" }) },
        body: options.body === undefined ? undefined : JSON.stringify(options.body),
        signal: controller.signal, cache: "no-store", credentials: "omit", redirect: "error"
      });
      details.http_status = result.status;
      diagnostic(level, "request.response", { ...details, elapsed_ms: elapsed() }, captured);
      if (!result.ok) {
        let payload = null;
        try { payload = await result.json(); } catch { /* non-JSON proxy response */ }
        if (result.status === 401) throw appError("The API token was rejected. Check Private API access and retry.", { error_kind: "http", error_code: "UNAUTHORIZED" });
        throw backendError(payload?.error, "The service returned HTTP " + result.status + ".", "http", result.status);
      }
      const value = options.file ? await result.blob() : result.status === 204 ? null : await result.json();
      diagnostic(level, "request.completed", { ...details, elapsed_ms: elapsed() }, captured);
      return value;
    } catch (caught) {
      let error = caught;
      if (caught?.name === "AbortError")
        error = options.signal?.aborted ? appError("Request cancelled.", { error_kind: "abort", error_code: "CANCELLED" })
          : appError("The service did not respond in time. Check Railway and retry.", { error_kind: "timeout", error_code: "TIMEOUT" });
      else if (caught instanceof TypeError)
        error = appError("The extractor is unreachable. Check the network and Railway deployment.", { error_kind: "network", error_code: "NETWORK" });
      else if (caught instanceof SyntaxError)
        error = appError("The service returned an invalid response. Check Railway logs and retry.", { error_kind: "protocol" });
      else if (!caught?.diagnostic)
        error = appError("The service returned an unexpected response. Check Railway logs and retry.", { error_kind: "protocol" });
      diagnostic(error.diagnostic.error_kind === "abort" ? "info" : "error", error.diagnostic.error_kind === "abort" ? "request.aborted" : "request.failed", { ...details, ...error.diagnostic, elapsed_ms: elapsed() }, captured);
      throw error;
    } finally {
      clearTimeout(timer); options.signal?.removeEventListener("abort", forward);
    }
  }
  function sleep(ms, signal) {
    return new Promise((resolve, reject) => {
      if (signal.aborted) { reject(new DOMException("Stopped", "AbortError")); return; }
      const timer = setTimeout(() => { signal.removeEventListener("abort", aborted); resolve(); }, ms);
      function aborted() { clearTimeout(timer); reject(new DOMException("Stopped", "AbortError")); }
      signal.addEventListener("abort", aborted, { once: true });
    });
  }
  function fileSize(n) { return n < 1048576 ? Math.ceil(n / 1024) + " KB" : (n / 1048576).toFixed(1) + " MB"; }
  async function loadVideo(job, signal, run) {
    const url = job.download_url;
    if (typeof url !== "string" ||
      url !== "/api/v1/downloads/" + state.jobId + "/file")
      throw appError("The backend returned an unexpected download URL.", { error_kind: "protocol" });
    step("file_transfer"); diagnostic("info", "file.transfer_started");
    working("Preparing your preview", "Loading the MP4 for playback and download.");
    // The file is bearer-protected; <video src> cannot attach an Authorization header.
    // Fetch once and share the transient blob URL with the player and download link.
    const blob = await request(url, { signal, file: true });
    if (run !== state.run) return;
    if (!blob.size) throw appError("The backend returned an empty video file.", { error_kind: "protocol" });
    diagnostic("info", "file.transfer_completed", { bytes: blob.size });
    discardPreview(); state.objectUrl = URL.createObjectURL(blob);
    ui.video.src = state.objectUrl;
    const safeName = (job.output?.filename || "video.mp4").replace(/[^A-Za-z0-9._-]/g, "_").slice(0, 100);
    ui.filename.textContent = safeName;
    ui.filesize.textContent = fileSize(blob.size) + " · MP4 video";
    ui.download.href = state.objectUrl;
    ui.download.download = safeName.endsWith(".mp4") ? safeName : "video.mp4";
    step("ready"); mode("success"); diagnostic("info", "preview.ready", { bytes: blob.size });
    toast("Your MP4 was generated successfully and is ready to download.", true);
  }
  async function poll(signal, run) {
    const deadline = Date.now() + 35 * 60 * 1000;
    let lastStatus = "", lastProgress = "";
    diagnostic("info", "poll.started");
    while (Date.now() < deadline) {
      if (run !== state.run) return;
      await sleep(4000, signal);
      const job = await request("/api/v1/downloads/" + state.jobId, { signal });
      if (run !== state.run) return;
      if (!job || !jobStatuses.has(job.status)) throw appError("Unexpected job status from the backend.", { error_kind: "protocol" });
      if (job.status !== lastStatus) {
        lastStatus = job.status;
        diagnostic(job.status === "failed" ? "error" : "info", "job.state", { job_status: job.status });
      }
      if (job.status === "completed") { step("completed"); await loadVideo(job, signal, run); return; }
      if (job.status === "failed" || job.status === "cancelled") {
        const error = backendError(job.error, "The job " + job.status + ".", "job_" + job.status);
        diagnostic(job.status === "cancelled" ? "info" : "error", "job." + job.status, { ...error.diagnostic, job_status: job.status, http_status: 200 });
        throw error;
      }
      step(job.status);
      const progress = job.progress?.stage;
      if (progressStages.has(progress) && progress !== lastProgress) {
        lastProgress = progress; diagnostic("info", "job.progress_stage", { progress_stage: progress });
      }
      working(job.status === "queued" ? "Waiting in the queue" : "Generating your MP4",
        job.status === "queued" ? "The extractor will begin when a worker is available." : "Your video is being downloaded and prepared. Keep this tab open.",
        true, Number.isFinite(job.progress?.percent) ? job.progress.percent : null);
    }
    throw appError("The job is taking longer than expected. Check Railway before creating another.", { error_kind: "timeout", error_code: "TIMEOUT" });
  }
  async function generate() {
    if (state.busy || !requireToken()) return;
    diagnostic("info", "confirmation.accepted", {}, preparationContext());
    ui.dialog.close(); discardPreview();
    state.run += 1; const run = state.run; state.jobId = null;
    const controller = new AbortController(); state.controller = controller;
    step("submitting"); diagnostic("info", "job.submission_started");
    working("Starting your download", "Sending your confirmed video to the extractor.");
    try {
      const job = await request("/api/v1/downloads", {
        method: "POST", body: { url: state.url, type: "video", format: "mp4", quality: "best" },
        signal: controller.signal
      });
      if (run !== state.run) return;
      if (!uuid(job?.job_id)) throw appError("The backend did not return a valid job ID.", { error_kind: "protocol" });
      state.jobId = job.job_id; diagnostic("info", "job.submitted"); step("queued");
      working("Waiting in the queue", "Your video is queued for processing.", true);
      await poll(controller.signal, run);
    } catch (error) {
      if (run === state.run && !controller.signal.aborted) failure(error);
    } finally { if (run === state.run) state.controller = null; }
  }
  async function cancel() {
    if (!state.busy || !state.jobId) return;
    const id = state.jobId; state.run += 1; step("cancelling");
    const captured = context(); diagnostic("info", "cancel.requested", {}, captured);
    state.controller?.abort(); state.controller = null; state.jobId = null;
    working("Cancelling", "Stopping your generation job.");
    try {
      await request("/api/v1/downloads/" + id, { method: "DELETE", context: captured });
      diagnostic("info", "cancel.confirmed", {}, captured); step("idle"); mode("idle"); toast("Generation cancelled.", true);
    } catch (error) {
      diagnostic("error", "cancel.failed", error.diagnostic, captured);
      failure(error, "Could not confirm cancellation. ", captured);
    }
  }
  function reset() {
    if (state.busy) return;
    diagnostic("info", "workflow.reset");
    state.run += 1; state.jobId = null; discardPreview(); step("idle"); mode("idle"); ui.url.focus();
  }
  ui.form.addEventListener("submit", (event) => {
    event.preventDefault(); if (state.busy) return;
    diagnostic("info", "source.validation_started", {}, preparationContext());
    let value;
    try { value = validateLink(ui.url.value); }
    catch (error) { diagnostic("warn", "source.validation_failed", { error_kind: "validation" }, preparationContext()); ui.url.focus(); toast(error.message); return; }
    diagnostic("info", "source.validated", {}, preparationContext()); step("confirmation", preparationContext());
    state.url = value.canonical; ui.iframe.src = value.embed; ui.external.href = value.canonical;
    ui.dialog.showModal(); ui.confirm.focus(); diagnostic("info", "confirmation.opened", {}, preparationContext());
  });
  $("close-dialog").addEventListener("click", () => ui.dialog.close());
  $("back-button").addEventListener("click", () => ui.dialog.close());
  ui.dialog.addEventListener("close", () => {
    ui.iframe.removeAttribute("src"); ui.button.focus(); diagnostic("debug", "confirmation.closed", {}, preparationContext());
    if (!state.busy && state.phase === "confirmation") step("idle", preparationContext());
  });
  ui.confirm.addEventListener("click", generate);
  ui.cancel.addEventListener("click", cancel);
  $("new-video-button").addEventListener("click", reset);
  $("retry-button").addEventListener("click", reset);
  ui.video.addEventListener("error", () => {
    if (state.objectUrl) {
      diagnostic("warn", "preview.playback_failed", { error_kind: "playback", playback_code: ui.video.error?.code });
      toast("The MP4 is ready, but this browser may not support the codec. Use Download MP4.");
    }
  });
  ui.video.addEventListener("loadedmetadata", () => { if (state.objectUrl) diagnostic("info", "preview.metadata_loaded"); });
  ui.video.addEventListener("playing", () => { if (state.objectUrl) diagnostic("info", "preview.playing"); });
  ui.download.addEventListener("click", () => { if (state.objectUrl) diagnostic("info", "file.download_clicked"); });
  window.addEventListener("pagehide", () => { if (state.objectUrl) URL.revokeObjectURL(state.objectUrl); });
  mode("idle"); diagnostic("info", "app.ready");
})();
