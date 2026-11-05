(() => {
  "use strict";
  const $ = (id) => document.getElementById(id);
  // Only the display preference is stored in JavaScript-accessible storage.
  const themeButton = $("theme-toggle");
  let theme = "light";
  try { theme = window.localStorage?.getItem("frame-theme") || ""; } catch { theme = ""; }
  if (!["light", "dark"].includes(theme))
    theme = window.matchMedia?.("(prefers-color-scheme: dark)").matches ? "dark" : "light";
  function applyTheme(value) {
    theme = value;
    document.documentElement?.setAttribute("data-theme", value);
    themeButton.setAttribute("aria-label", value === "dark" ? "Switch to light theme" : "Switch to dark theme");
  }
  applyTheme(theme);
  themeButton.addEventListener("click", () => {
    applyTheme(theme === "dark" ? "light" : "dark");
    try { window.localStorage?.setItem("frame-theme", theme); } catch { /* preference is optional */ }
  });
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
    video: $("result-video"), audio: $("result-audio"), filename: $("file-name"), filesize: $("file-size"),
    extension: $("file-extension"), successTitle: $("success-title"), errorTitle: $("error-title"),
    download: $("download-link"), errorText: $("error-description"),
    toasts: $("toast-region"), media: $("media-type"), format: $("output-format"),
    quality: $("output-quality"), bitrate: $("audio-bitrate"), bitrateField: $("bitrate-field"),
    urlError: $("url-error"), tokenError: $("token-error"), bitrateError: $("bitrate-error"),
    selection: $("confirm-selection"), jobId: $("job-id"), retry: $("retry-button"),
    checkJob: $("check-job-button"), cancelRecovery: $("cancel-recovery-button"),
    sessionStatus: $("session-status"), sessionMessage: $("session-message"),
    sessionRetry: $("session-retry-button"), saveAccess: $("save-access-button"),
    forgetAccess: $("forget-access-button")
  };
  const state = { access: "checking", sessionAction: null, sessionError: "", actionPending: false, confirmationVersion: 0, url: "", configuration: null, jobId: null, jobOutstanding: false, tokenFocusPending: false, controller: null, objectUrl: null, run: 0, busy: false, phase: "idle", view: null };
  let sessionTask = null;
  const jobStatuses = new Set(["queued", "running", "completed", "failed", "cancelled"]);
  const progressStages = new Set(["extracting", "downloading", "processing", "completed"]);
  const errorCodes = new Set(["INVALID_REQUEST", "UNAUTHORIZED", "NOT_FOUND", "QUEUE_FULL", "RATE_LIMITED", "CANCELLED", "NETWORK", "DNS", "ACCESS_RESTRICTED", "UPSTREAM_RATE_LIMITED", "PLAYER_CHANGED", "UNSUPPORTED_FORMAT", "EXPIRED_URL", "DOWNLOAD_INTERRUPTED", "DOWNLOAD_TOO_LARGE", "FFMPEG_FAILED", "STORAGE", "TIMEOUT", "INTERNAL"]);
  const uuid = (value) => typeof value === "string" && value !== ui.token.value && /^[0-9a-f]{8}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{12}$/i.test(value) ? value : null;
  let requestNumber = 0;
  function context() { return { run: state.run, phase: state.phase, jobId: uuid(state.jobId) }; }
  function preparationContext() { return { run: state.run, phase: state.phase, jobId: null }; }
  function routeName(path) {
    if (path === "/api/v1/downloads" || path === "/api/v1/session") return path;
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
    if (["/api/v1/session", "/api/v1/downloads", "/api/v1/downloads/:job_id", "/api/v1/downloads/:job_id/file"].includes(details.route)) entry.route = details.route;
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
    state.phase = next;
    const current = next === "confirmation" ? "configure" : ["submitting", "queued", "running", "checking", "cancelling", "completed", "file_transfer", "ready", "failed", "cancelled"].includes(next) ? "export" : "source";
    for (const name of ["source", "configure", "export"]) {
      const item = $("step-" + name); item.dataset.current = String(name === current);
      if (name === current) item.setAttribute("aria-current", "step"); else item.removeAttribute("aria-current");
    }
    diagnostic("info", "workflow.step", {}, captured ? { ...captured, phase: next } : context());
  }
  function safeMessage(value, fallback) {
    if (typeof value !== "string" || !value.trim()) return fallback;
    let message = value;
    for (const secret of [ui.token.value])
      if (secret) message = message.split(secret).join("[redacted]");
    return message.replace(/(?:https?:\/\/|blob:)\S+/gi, "[redacted URL]")
      .replace(/Bearer\s+\S+/gi, "Bearer [redacted]")
      // A token is 32–4096 contiguous visible ASCII characters; do not retain
      // its value after saving just to redact a later backend error.
      .replace(/[!-~]{32,}/g, "[redacted]").slice(0, 800);
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
    const input = value.trim();
    try { url = new URL(input); } catch { throw Error("Enter a valid YouTube video link."); }
    if (!["https:", "http:"].includes(url.protocol) || url.username || url.password || url.port || /[\\\x00-\x1f\x7f]/.test(input))
      throw Error("Enter a standard YouTube video link without credentials or a custom port.");
    const rawPath = input.split("://")[1]?.replace(/^[^/?#]*/, "").split(/[?#]/)[0] || "";
    if (rawPath.includes("%") || rawPath.split("/").some((part) => part === "." || part === ".."))
      throw Error("Enter a standard YouTube video link.");
    const host = url.hostname.toLowerCase();
    const ids = url.searchParams.getAll("v");
    let id = "";
    if (host === "youtu.be" && !ids.length) id = url.pathname.slice(1);
    else if (["youtube.com", "www.youtube.com", "m.youtube.com"].includes(host)) {
      if (url.pathname === "/watch" && ids.length === 1) id = ids[0];
      else if (!ids.length && /^\/(shorts|live)\//.test(url.pathname)) id = url.pathname.split("/")[2] || "";
      if (url.pathname !== "/watch" && url.pathname.split("/").length !== 3) id = "";
    }
    if (!/^[A-Za-z0-9_-]{11}$/.test(id)) throw Error("Enter a YouTube watch, Shorts, live, or youtu.be link with a valid video ID.");
    return { canonical: "https://www.youtube.com/watch?v=" + id, embed: "https://www.youtube-nocookie.com/embed/" + id + "?rel=0" };
  }
  function fieldError(field, target, message = "") {
    target.textContent = message; target.hidden = !message;
    if (message) field.setAttribute("aria-invalid", "true"); else field.removeAttribute("aria-invalid");
  }
  const formats = {
    video: [["mp4", "MP4"], ["original", "Default (MP4)"], ["webm", "WebM"]],
    audio: [["original", "Default (M4A)"], ["m4a", "M4A"], ["mp3", "MP3"]]
  };
  const qualities = [["best", "Best available"], ["worst", "Lowest available"], ...[144, 240, 360, 480, 720, 1080, 1440, 2160, 4320, 8640].map((height) => [height + "p", "Up to " + height + "p"])];
  function setOptions(select, options, value) {
    const nodes = options.map(([key, label]) => {
      const option = document.createElement("option");
      option.value = key; option.textContent = label; return option;
    });
    select.replaceChildren(...nodes);
    select.value = options.some(([key]) => key === value) ? value : options[0][0];
  }
  function updateMedia() {
    const kind = ui.media.value === "audio" ? "audio" : "video";
    setOptions(ui.format, formats[kind], kind === "video" ? "mp4" : "original");
    // The current audio adapter always selects the best source, regardless of quality.
    setOptions(ui.quality, kind === "video" ? qualities : qualities.slice(0, 1), "best");
    ui.bitrateField.hidden = kind !== "audio";
    fieldError(ui.bitrate, ui.bitrateError);
  }
  function configuration() {
    const kind = ui.media.value === "audio" ? "audio" : "video";
    const format = formats[kind].some(([key]) => key === ui.format.value) ? ui.format.value : formats[kind][0][0];
    const allowedQualities = kind === "video" ? qualities : qualities.slice(0, 1);
    const quality = allowedQualities.some(([key]) => key === ui.quality.value) ? ui.quality.value : "best";
    const bitrate = ui.bitrate.value.trim();
    if (kind === "audio" && bitrate && (!/^\d+$/.test(bitrate) || Number(bitrate) < 32 || Number(bitrate) > 320)) {
      fieldError(ui.bitrate, ui.bitrateError, "Enter a whole number from 32 to 320 kbps, or leave this blank.");
      ui.bitrate.focus(); return null;
    }
    fieldError(ui.bitrate, ui.bitrateError);
    return { type: kind, format, quality, ...(kind === "audio" && bitrate ? { audio_bitrate_kbps: Number(bitrate) } : {}) };
  }
  function describeConfiguration(config) {
    const quality = qualities.find(([key]) => key === config.quality)?.[1] || "Best available";
    const format = formats[config.type].find(([key]) => key === config.format)?.[1] || config.format.toUpperCase();
    return [config.type === "audio" ? "Audio" : "Video", format, quality,
      config.audio_bitrate_kbps ? config.audio_bitrate_kbps + " kbps" : ""].filter(Boolean).join(" · ");
  }
  function discardPreview() {
    for (const player of [ui.video, ui.audio]) { player.pause(); player.removeAttribute("src"); player.load(); }
    ui.download.removeAttribute("href");
    if (state.objectUrl) URL.revokeObjectURL(state.objectUrl);
    state.objectUrl = null;
  }
  function revealResult(target) {
    if (window.matchMedia?.("(max-width: 760px)").matches && typeof target.scrollIntoView === "function") {
      const reduce = window.matchMedia("(prefers-reduced-motion: reduce)").matches;
      target.scrollIntoView({ behavior: reduce ? "auto" : "smooth", block: "center" });
    }
    target.focus({ preventScroll: true });
  }
  function mode(next) {
    if (state.view !== next) { state.view = next; diagnostic("info", "ui.state", { view: next }); }
    ui.empty.hidden = next !== "idle"; ui.working.hidden = next !== "working";
    ui.success.hidden = next !== "success"; ui.error.hidden = next !== "error";
    ui.pill.dataset.stage = next;
    ui.pill.textContent = { idle: "WAITING", working: "PROCESSING", success: "READY", error: "NEEDS ATTENTION" }[next];
    state.busy = next === "working"; syncControls();
    ui.jobId.hidden = !uuid(state.jobId);
    ui.jobId.textContent = uuid(state.jobId) ? "Job ID · " + state.jobId : "";
    ui.cancel.hidden = true; ui.progressArea.hidden = true;
  }
  function working(title, description, canCancel = false, percentage = null) {
    mode("working"); ui.workTitle.textContent = title; ui.workText.textContent = description;
    ui.cancel.hidden = !canCancel;
    ui.progressArea.hidden = false;
    if (Number.isFinite(percentage) && percentage >= 0 && percentage <= 100) {
      ui.progress.value = percentage; ui.progressNumber.textContent = Math.round(percentage) + "%";
    } else {
      ui.progress.removeAttribute("value"); ui.progressNumber.textContent = "In progress";
    }
  }
  function failure(error, prefix = "", captured = context()) {
    const message = prefix + safeMessage(error?.message, "Generation failed.");
    const details = error?.diagnostic || { error_kind: "unknown" };
    diagnostic(details.error_kind === "job_cancelled" ? "info" : "error", details.error_kind === "job_cancelled" ? "workflow.cancelled" : "workflow.failed", details, captured);
    const extra = ["Step: " + captured.phase, uuid(captured.jobId) ? "Job: " + captured.jobId : "", errorCodes.has(details.error_code) ? "Code: " + details.error_code : ""].filter(Boolean).join(" · ");
    step(details.error_kind === "job_cancelled" ? "cancelled" : "failed");
    mode("error"); ui.errorText.textContent = message + " " + extra;
    const recoverable = Boolean(uuid(state.jobId) && state.jobOutstanding);
    ui.retry.hidden = recoverable; ui.checkJob.hidden = !recoverable; ui.cancelRecovery.hidden = !recoverable;
    toast(message); revealResult(ui.errorTitle);
  }
  function syncControls() {
    const locked = state.busy || state.actionPending;
    ui.button.disabled = locked || state.jobOutstanding;
    ui.url.disabled = locked;
    for (const field of [ui.media, ui.format, ui.quality, ui.bitrate]) field.disabled = locked || state.jobOutstanding;
    const tokenLocked = locked || state.sessionAction === "save" || state.sessionAction === "forget";
    ui.token.disabled = tokenLocked; $("token-toggle").disabled = tokenLocked;
    ui.saveAccess.disabled = locked || Boolean(state.sessionAction);
    ui.forgetAccess.disabled = locked || state.jobOutstanding || Boolean(state.sessionAction);
    ui.sessionRetry.disabled = locked || Boolean(state.sessionAction);
    ui.confirm.disabled = state.actionPending || state.sessionAction === "save" || state.sessionAction === "forget";
    ui.saveAccess.textContent = state.sessionAction === "save" ? "Saving…" : "Save access";
  }
  function renderAccess() {
    const ready = state.access === "saved" || state.access === "open";
    ui.access.hidden = ready;
    ui.sessionStatus.hidden = !ready && !state.sessionAction && !state.sessionError;
    ui.sessionStatus.dataset.state = state.sessionError ? "error" : state.access;
    ui.sessionMessage.textContent = state.sessionAction === "check" ? "Checking access…" :
      state.sessionAction === "save" ? "Saving access…" :
      state.sessionAction === "forget" ? "Removing access…" :
      state.sessionError || (state.access === "saved" ? "Access saved" : "Access ready");
    ui.forgetAccess.hidden = state.access !== "saved";
    ui.sessionRetry.hidden = !state.sessionError;
    syncControls();
  }
  function focusAccess() {
    if (ui.dialog.open) { state.tokenFocusPending = true; ui.dialog.close(); }
    ui.access.open = true; ui.token.focus();
  }
  function accessResult(result) {
    if (!result || result.authenticated !== true || typeof result.saved !== "boolean")
      throw appError("The service returned an invalid access status.", { error_kind: "protocol" });
    state.access = result.saved ? "saved" : "open";
    state.sessionError = ""; ui.token.value = "";
    ui.token.type = "password"; $("token-toggle").textContent = "Show";
    $("token-toggle").setAttribute("aria-label", "Show access token");
    $("token-toggle").setAttribute("aria-pressed", "false");
    fieldError(ui.token, ui.tokenError);
    diagnostic("info", "access.ready", {}, preparationContext());
  }
  function runSession(action, operation) {
    if (sessionTask) return sessionTask;
    state.sessionAction = action; renderAccess();
    sessionTask = (async () => {
      try { return await operation(); }
      finally { state.sessionAction = null; sessionTask = null; renderAccess(); }
    })();
    return sessionTask;
  }
  async function refreshAccess() {
    if (sessionTask) return sessionTask;
    return runSession("check", async () => {
      try {
        accessResult(await request("/api/v1/session", { quietUnauthorized: true, context: preparationContext() }));
        return true;
      } catch (error) {
        state.access = "required";
        state.sessionError = error?.diagnostic?.http_status === 401 ? "" : "Couldn't check saved access. Enter a token or check again.";
        return false;
      }
    });
  }
  async function saveAccess() {
    if (sessionTask) await sessionTask;
    if (state.access === "saved" || state.access === "open") return true;
    const value = ui.token.value;
    if (value.length < 32 || value.length > 4096 || !/^[!-~]+$/.test(value)) {
      diagnostic("warn", "access.validation_failed", { error_kind: "validation" }, preparationContext());
      fieldError(ui.token, ui.tokenError, "Enter a token of 32–4096 visible characters, with no spaces.");
      return false;
    }
    fieldError(ui.token, ui.tokenError); state.sessionError = "";
    return runSession("save", async () => {
      let accepted = false;
      try {
        await request("/api/v1/session", { method: "POST", bearer: value, context: preparationContext() });
        accepted = true;
        accessResult(await request("/api/v1/session", { quietUnauthorized: true, context: preparationContext() }));
        return true;
      } catch (error) {
        state.access = "required";
        const unauthorized = error?.diagnostic?.http_status === 401;
        fieldError(ui.token, ui.tokenError, unauthorized && accepted ? "Access couldn't be saved. Allow cookies for this site and try again." :
          unauthorized ? "The access token was rejected. Check it and try again." :
            "Couldn't save access. Check the connection and try again.");
        return false;
      }
    });
  }
  async function ensureAccess() {
    if (sessionTask) await sessionTask;
    return state.access === "saved" || state.access === "open" || await saveAccess();
  }
  async function prepareAccess() {
    state.actionPending = true; syncControls();
    try { return await ensureAccess(); }
    catch {
      state.access = "required";
      state.sessionError = "Couldn't check access. Enter a token or check again.";
      renderAccess(); return false;
    } finally { state.actionPending = false; syncControls(); }
  }
  async function forgetAccess() {
    if (state.busy || state.actionPending || state.jobOutstanding || sessionTask || state.access !== "saved") return;
    await runSession("forget", async () => {
      try {
        await request("/api/v1/session", { method: "DELETE", context: preparationContext() });
        ui.token.value = ""; fieldError(ui.token, ui.tokenError);
        state.access = "required";
        try {
          const result = await request("/api/v1/session", { quietUnauthorized: true, context: preparationContext() });
          accessResult(result);
          if (result.saved) {
            state.sessionError = "Couldn't remove saved access. Check browser settings and try again.";
            return;
          }
        } catch (error) {
          if (error?.diagnostic?.http_status !== 401) throw error;
        }
        state.sessionError = ""; toast("Access removed from this browser.", true);
      } catch {
        state.sessionError = "Couldn't confirm access was removed. Check again.";
      }
    });
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
        headers: { "X-Frame-Client": "1", ...(options.bearer ? { Authorization: "Bearer " + options.bearer } : {}), ...(options.body === undefined ? {} : { "Content-Type": "application/json" }), ...(uuid(options.idempotencyKey) ? { "X-Idempotency-Key": options.idempotencyKey } : {}) },
        body: options.body === undefined ? undefined : JSON.stringify(options.body),
        signal: controller.signal, cache: "no-store", credentials: "same-origin", redirect: "error"
      });
      details.http_status = result.status;
      diagnostic(level, "request.response", { ...details, elapsed_ms: elapsed() }, captured);
      if (!result.ok) {
        let payload = null;
        try { payload = await result.json(); } catch { /* non-JSON proxy response */ }
        if (result.status === 401) {
          if (path !== "/api/v1/session") {
            state.access = "required"; state.sessionError = ""; ui.access.open = true;
            fieldError(ui.token, ui.tokenError, "Saved access expired. Enter your access token and save it again.");
            renderAccess();
          }
          throw appError(path === "/api/v1/session" ? "Access was not authorized." : "Access expired. Save your API access token again, then retry.", { error_kind: "http", error_code: "UNAUTHORIZED", http_status: result.status });
        }
        throw backendError(payload?.error, "The service returned HTTP " + result.status + ".", "http", result.status);
      }
      const value = options.file ? await result.blob() : result.status === 204 ? null : await result.json();
      diagnostic(level, "request.completed", { ...details, elapsed_ms: elapsed() }, captured);
      return value;
    } catch (caught) {
      let error = caught;
      if (caught?.name === "AbortError")
        error = options.signal?.aborted ? appError("Request cancelled.", { error_kind: "abort", error_code: "CANCELLED" })
          : appError("The service did not respond in time. Check the service and retry.", { error_kind: "timeout", error_code: "TIMEOUT" });
      else if (caught instanceof TypeError)
        error = appError("The extractor is unreachable. Check the network and service, then retry.", { error_kind: "network", error_code: "NETWORK" });
      else if (caught instanceof SyntaxError)
        error = appError("The service returned an invalid response. Check the service and retry.", { error_kind: "protocol" });
      else if (!caught?.diagnostic)
        error = appError("The service returned an unexpected response. Check the service and retry.", { error_kind: "protocol" });
      const quiet = options.quietUnauthorized && error.diagnostic.http_status === 401;
      diagnostic(quiet ? "debug" : error.diagnostic.error_kind === "abort" ? "info" : "error", quiet ? "request.unauthenticated" : error.diagnostic.error_kind === "abort" ? "request.aborted" : "request.failed", { ...details, ...error.diagnostic, elapsed_ms: elapsed() }, captured);
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
    working("Preparing your preview", "Loading your file for playback and download.");
    // Fetch the authenticated file once and share its transient blob URL with
    // the player and download link.
    const blob = await request(url, { signal, file: true });
    if (run !== state.run) return;
    if (!blob.size) throw appError("The backend returned an empty file.", { error_kind: "protocol" });
    diagnostic("info", "file.transfer_completed", { bytes: blob.size });
    discardPreview(); state.objectUrl = URL.createObjectURL(blob);
    const mime = typeof job.output?.mime_type === "string" ? job.output.mime_type : "";
    const isAudio = mime.startsWith("audio/") || state.configuration?.type === "audio";
    ui.video.hidden = isAudio; ui.audio.hidden = !isAudio;
    (isAudio ? ui.audio : ui.video).src = state.objectUrl;
    const fallback = "frame-export" + (state.configuration?.format && state.configuration.format !== "original" ? "." + state.configuration.format : "");
    const filename = typeof job.output?.filename === "string" ? job.output.filename : fallback;
    const safeName = filename.replace(/[^A-Za-z0-9._-]/g, "_").slice(0, 100) || fallback;
    const extension = safeName.includes(".") ? safeName.split(".").pop() : "file";
    ui.filename.textContent = safeName;
    ui.filesize.textContent = fileSize(blob.size) + " · " + (isAudio ? "Audio" : "Video") + " file";
    ui.extension.textContent = /^[A-Za-z0-9]{1,8}$/.test(extension) ? extension.toUpperCase() : "FILE";
    ui.download.href = state.objectUrl; ui.download.download = safeName;
    state.jobOutstanding = false; step("ready"); mode("success"); diagnostic("info", "preview.ready", { bytes: blob.size });
    toast("Your file is ready to download.", true); revealResult(ui.successTitle);
  }
  async function poll(signal, run, immediate = false) {
    const deadline = Date.now() + 35 * 60 * 1000;
    let lastStatus = "", lastProgress = "", first = true;
    diagnostic("info", "poll.started");
    while (Date.now() < deadline) {
      if (run !== state.run) return;
      if (!first || !immediate) await sleep(4000, signal);
      first = false;
      const job = await request("/api/v1/downloads/" + state.jobId, { signal });
      if (run !== state.run) return;
      if (!job || !jobStatuses.has(job.status)) throw appError("Unexpected job status from the backend.", { error_kind: "protocol" });
      if (job.job_id !== undefined && job.job_id !== state.jobId)
        throw appError("The backend returned a different job ID. Check the job before continuing.", { error_kind: "protocol" });
      if (job.status !== lastStatus) {
        lastStatus = job.status;
        diagnostic(job.status === "failed" ? "error" : "info", "job.state", { job_status: job.status });
      }
      if (job.status === "completed") { step("completed"); await loadVideo(job, signal, run); return; }
      if (job.status === "failed" || job.status === "cancelled") {
        state.jobOutstanding = false;
        const error = backendError(job.error, "The job " + job.status + ".", "job_" + job.status);
        diagnostic(job.status === "cancelled" ? "info" : "error", "job." + job.status, { ...error.diagnostic, job_status: job.status, http_status: 200 });
        throw error;
      }
      step(job.status);
      const progress = job.progress?.stage;
      if (progressStages.has(progress) && progress !== lastProgress) {
        lastProgress = progress; diagnostic("info", "job.progress_stage", { progress_stage: progress });
      }
      working(job.status === "queued" ? "Waiting in the queue" : "Preparing your file",
        job.status === "queued" ? "The extractor will begin when a worker is available." : "Your file is being downloaded and prepared. Keep this tab open.",
        true, Number.isFinite(job.progress?.percent) ? job.progress.percent : null);
    }
    throw appError("The job is taking longer than expected. Check this job before creating another.", { error_kind: "timeout", error_code: "TIMEOUT" });
  }
  async function generate() {
    if (state.busy || state.jobOutstanding || state.actionPending) return;
    const confirmation = state.confirmationVersion;
    const authorized = await prepareAccess();
    if (!ui.dialog.open || state.confirmationVersion !== confirmation) return;
    if (!authorized) { focusAccess(); return; }
    diagnostic("info", "confirmation.accepted", {}, preparationContext());
    ui.dialog.close(); discardPreview();
    state.run += 1; const run = state.run; state.jobId = null; state.jobOutstanding = false;
    // A supplied UUID is both an idempotency key and the server's job ID. Retain it
    // if admission becomes uncertain, so the operator can check or cancel safely.
    const submissionId = typeof window.crypto?.randomUUID === "function" ? uuid(window.crypto.randomUUID()) : null;
    const controller = new AbortController(); state.controller = controller;
    step("submitting"); diagnostic("info", "job.submission_started");
    working("Starting your export", "Sending your confirmed video to the extractor."); revealResult(ui.workTitle);
    try {
      const job = await request("/api/v1/downloads", {
        method: "POST", body: { url: state.url, ...state.configuration },
        signal: controller.signal, idempotencyKey: submissionId
      });
      if (run !== state.run) return;
      if (!uuid(job?.job_id) || (submissionId && job.job_id !== submissionId)) throw appError("The backend did not return a valid job ID.", { error_kind: "protocol" });
      state.jobId = job.job_id; state.jobOutstanding = true; diagnostic("info", "job.submitted"); step("queued");
      working("Waiting in the queue", "Your video is queued for processing.", true);
      await poll(controller.signal, run);
    } catch (error) {
      if (run === state.run && !controller.signal.aborted) {
        const detail = error?.diagnostic || {};
        if (!state.jobId && submissionId && (["network", "timeout", "protocol"].includes(detail.error_kind) || (detail.error_kind === "http" && detail.http_status >= 500))) {
          state.jobId = submissionId; state.jobOutstanding = true;
        }
        failure(error);
      }
    } finally { if (run === state.run) state.controller = null; }
  }
  async function checkJob() {
    if (state.busy || state.actionPending || !state.jobOutstanding || !uuid(state.jobId)) return;
    const authorized = await prepareAccess();
    if (!authorized) { focusAccess(); return; }
    state.run += 1; const run = state.run;
    const controller = new AbortController(); state.controller = controller;
    step("checking"); working("Checking your export", "Looking up the latest job status.", true); revealResult(ui.workTitle);
    try { await poll(controller.signal, run, true); }
    catch (error) { if (run === state.run && !controller.signal.aborted) failure(error); }
    finally { if (run === state.run) state.controller = null; }
  }
  async function cancel() {
    if (!state.jobOutstanding || !uuid(state.jobId) || state.actionPending || (state.busy && state.phase === "cancelling")) return;
    if (!state.busy) {
      const authorized = await prepareAccess();
      if (!authorized) { focusAccess(); return; }
    }
    const id = state.jobId; state.run += 1; step("cancelling");
    const captured = context(); diagnostic("info", "cancel.requested", {}, captured);
    state.controller?.abort(); state.controller = null;
    working("Cancelling", "Stopping your export.");
    try {
      await request("/api/v1/downloads/" + id, { method: "DELETE", context: captured });
      diagnostic("info", "cancel.confirmed", {}, captured); state.jobId = null; state.jobOutstanding = false;
      step("idle"); mode("idle"); toast("Export cancelled.", true);
    } catch (error) {
      diagnostic("error", "cancel.failed", error.diagnostic, captured);
      failure(error, "Could not confirm cancellation. ", captured);
    }
  }
  function reset() {
    if (state.busy || state.actionPending || state.jobOutstanding) return;
    diagnostic("info", "workflow.reset");
    state.run += 1; state.jobId = null; discardPreview(); step("idle"); mode("idle"); ui.url.focus();
  }
  ui.saveAccess.addEventListener("click", async () => {
    if (state.busy || state.actionPending || sessionTask) return;
    if (!await saveAccess()) focusAccess();
  });
  ui.forgetAccess.addEventListener("click", forgetAccess);
  ui.sessionRetry.addEventListener("click", () => { if (!state.busy && !state.actionPending) return refreshAccess(); });
  ui.media.addEventListener("change", updateMedia);
  ui.url.addEventListener("input", () => fieldError(ui.url, ui.urlError));
  ui.token.addEventListener("input", () => fieldError(ui.token, ui.tokenError));
  ui.bitrate.addEventListener("input", () => fieldError(ui.bitrate, ui.bitrateError));
  if (!ui.media.value) ui.media.value = "video";
  updateMedia();
  $("token-toggle").addEventListener("click", () => {
    const visible = ui.token.type === "password";
    ui.token.type = visible ? "text" : "password";
    $("token-toggle").textContent = visible ? "Hide" : "Show";
    $("token-toggle").setAttribute("aria-label", visible ? "Hide access token" : "Show access token");
    $("token-toggle").setAttribute("aria-pressed", String(visible));
  });
  ui.form.addEventListener("submit", (event) => {
    event.preventDefault(); if (state.busy || state.actionPending || state.jobOutstanding) return;
    diagnostic("info", "source.validation_started", {}, preparationContext());
    let value;
    try { value = validateLink(ui.url.value); }
    catch (error) { diagnostic("warn", "source.validation_failed", { error_kind: "validation" }, preparationContext()); fieldError(ui.url, ui.urlError, error.message); ui.url.focus(); return; }
    fieldError(ui.url, ui.urlError);
    const config = configuration(); if (!config) return;
    diagnostic("info", "source.validated", {}, preparationContext()); step("confirmation", preparationContext());
    state.configuration = config; ui.selection.textContent = describeConfiguration(config);
    state.url = value.canonical; ui.iframe.src = value.embed; ui.external.href = value.canonical;
    state.confirmationVersion += 1; ui.dialog.showModal(); $("back-button").focus(); diagnostic("info", "confirmation.opened", {}, preparationContext());
  });
  $("close-dialog").addEventListener("click", () => ui.dialog.close());
  $("back-button").addEventListener("click", () => ui.dialog.close());
  ui.dialog.addEventListener("close", () => {
    state.confirmationVersion += 1; ui.iframe.removeAttribute("src");
    if (state.tokenFocusPending) { state.tokenFocusPending = false; ui.token.focus(); }
    else if (!state.busy) ui.button.focus();
    diagnostic("debug", "confirmation.closed", {}, preparationContext());
    if (!state.busy && state.phase === "confirmation") step("idle", preparationContext());
  });
  ui.confirm.addEventListener("click", generate);
  ui.cancel.addEventListener("click", cancel);
  ui.checkJob.addEventListener("click", checkJob);
  ui.cancelRecovery.addEventListener("click", cancel);
  $("new-video-button").addEventListener("click", reset);
  $("retry-button").addEventListener("click", reset);
  for (const player of [ui.video, ui.audio]) {
    player.addEventListener("error", () => {
      if (state.objectUrl && !player.hidden) {
        diagnostic("warn", "preview.playback_failed", { error_kind: "playback", playback_code: player.error?.code });
        toast("The file is ready, but this browser may not support the format. Use Download file.");
      }
    });
    player.addEventListener("loadedmetadata", () => { if (state.objectUrl && !player.hidden) diagnostic("info", "preview.metadata_loaded"); });
    player.addEventListener("playing", () => { if (state.objectUrl && !player.hidden) diagnostic("info", "preview.playing"); });
  }
  ui.download.addEventListener("click", () => { if (state.objectUrl) diagnostic("info", "file.download_clicked"); });
  window.addEventListener("pagehide", () => { if (state.objectUrl) URL.revokeObjectURL(state.objectUrl); });
  mode("idle"); renderAccess(); void refreshAccess(); diagnostic("info", "app.ready");
})();
