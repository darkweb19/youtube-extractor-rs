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
  const state = { token: "", url: "", jobId: null, controller: null, objectUrl: null, run: 0, busy: false };
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
  function failure(message) { mode("error"); ui.errorText.textContent = message; toast(message); }
  function requireToken() {
    const value = ui.token.value;
    if (value.length < 32 || value.length > 4096 || !/^[!-~]+$/.test(value)) {
      ui.dialog.close(); ui.access.open = true; ui.token.focus();
      toast("Enter your private API token (at least 32 characters) to generate a video.");
      return false;
    }
    state.token = value; return true;
  }
  async function request(path, options = {}) {
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
      if (!result.ok) {
        let payload = null;
        try { payload = await result.json(); } catch { /* non-JSON proxy response */ }
        if (result.status === 401) throw Error("The API token was rejected. Check Private API access and retry.");
        throw Error(payload?.error?.message || "The service returned HTTP " + result.status + ".");
      }
      if (options.file) return await result.blob();
      return result.status === 204 ? null : await result.json();
    } catch (error) {
      if (error.name === "AbortError" && !options.signal?.aborted)
        throw Error("The service did not respond in time. Check Railway and retry.");
      if (error instanceof TypeError)
        throw Error("The extractor is unreachable. Check the network and Railway deployment.");
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
      throw Error("The backend returned an unexpected download URL.");
    working("Preparing your preview", "Loading the MP4 for playback and download.");
    // The file is bearer-protected; <video src> cannot attach an Authorization header.
    // Fetch once and share the transient blob URL with the player and download link.
    const blob = await request(url, { signal, file: true });
    if (run !== state.run) return;
    if (!blob.size) throw Error("The backend returned an empty video file.");
    discardPreview(); state.objectUrl = URL.createObjectURL(blob);
    ui.video.src = state.objectUrl;
    const safeName = (job.output?.filename || "video.mp4").replace(/[^A-Za-z0-9._-]/g, "_").slice(0, 100);
    ui.filename.textContent = safeName;
    ui.filesize.textContent = fileSize(blob.size) + " · MP4 video";
    ui.download.href = state.objectUrl;
    ui.download.download = safeName.endsWith(".mp4") ? safeName : "video.mp4";
    mode("success"); toast("Your MP4 was generated successfully and is ready to download.", true);
  }
  async function poll(signal, run) {
    const deadline = Date.now() + 35 * 60 * 1000;
    while (Date.now() < deadline) {
      if (run !== state.run) return;
      await sleep(4000, signal);
      const job = await request("/api/v1/downloads/" + state.jobId, { signal });
      if (run !== state.run) return;
      if (job.status === "completed") { await loadVideo(job, signal, run); return; }
      if (job.status === "failed" || job.status === "cancelled")
        throw Error(job.error?.message || "The job " + job.status + ".");
      if (!["queued", "running"].includes(job.status)) throw Error("Unexpected job status from the backend.");
      working(job.status === "queued" ? "Waiting in the queue" : "Generating your MP4",
        job.status === "queued" ? "The extractor will begin when a worker is available." : "Your video is being downloaded and prepared. Keep this tab open.",
        true, Number.isFinite(job.progress?.percent) ? job.progress.percent : null);
    }
    throw Error("The job is taking longer than expected. Check Railway before creating another.");
  }
  async function generate() {
    if (state.busy || !requireToken()) return;
    ui.dialog.close(); discardPreview();
    state.run += 1; const run = state.run; state.jobId = null;
    const controller = new AbortController(); state.controller = controller;
    working("Starting your download", "Sending your confirmed video to the extractor.");
    try {
      const job = await request("/api/v1/downloads", {
        method: "POST", body: { url: state.url, type: "video", format: "mp4", quality: "best" },
        signal: controller.signal
      });
      if (run !== state.run) return;
      if (typeof job?.job_id !== "string") throw Error("The backend did not return a job ID.");
      state.jobId = job.job_id;
      working("Waiting in the queue", "Your video is queued for processing.", true);
      await poll(controller.signal, run);
    } catch (error) {
      if (run === state.run && !controller.signal.aborted) failure(error.message || "Generation failed.");
    } finally { if (run === state.run) state.controller = null; }
  }
  async function cancel() {
    if (!state.busy || !state.jobId) return;
    const id = state.jobId; state.run += 1;
    state.controller?.abort(); state.controller = null; state.jobId = null;
    working("Cancelling", "Stopping your generation job.");
    try { await request("/api/v1/downloads/" + id, { method: "DELETE" }); mode("idle"); toast("Generation cancelled.", true); }
    catch (error) { failure("Could not confirm cancellation. " + (error.message || "Check the job status.")); }
  }
  function reset() {
    if (state.busy) return;
    state.run += 1; state.jobId = null; discardPreview(); mode("idle"); ui.url.focus();
  }
  ui.form.addEventListener("submit", (event) => {
    event.preventDefault(); if (state.busy) return;
    let value;
    try { value = validateLink(ui.url.value); }
    catch (error) { ui.url.focus(); toast(error.message); return; }
    state.url = value.canonical; ui.iframe.src = value.embed; ui.external.href = value.canonical;
    ui.dialog.showModal(); ui.confirm.focus();
  });
  $("close-dialog").addEventListener("click", () => ui.dialog.close());
  $("back-button").addEventListener("click", () => ui.dialog.close());
  ui.dialog.addEventListener("close", () => { ui.iframe.removeAttribute("src"); ui.button.focus(); });
  ui.confirm.addEventListener("click", generate);
  ui.cancel.addEventListener("click", cancel);
  $("new-video-button").addEventListener("click", reset);
  $("retry-button").addEventListener("click", reset);
  ui.video.addEventListener("error", () => {
    if (state.objectUrl) toast("The MP4 is ready, but this browser may not support the codec. Use Download MP4.");
  });
  window.addEventListener("pagehide", () => { if (state.objectUrl) URL.revokeObjectURL(state.objectUrl); });
  mode("idle");
})();
