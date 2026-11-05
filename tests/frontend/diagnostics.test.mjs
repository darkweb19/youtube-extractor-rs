import assert from "node:assert/strict";
import { readFile } from "node:fs/promises";
import test from "node:test";
import vm from "node:vm";

const source = await readFile(new URL("../../web/app.js", import.meta.url), "utf8");
const jobId = "494d32e9-9c34-479b-8c12-b12401c7fca3";
const token = "private-api-token-never-in-console-0123456789";
const video = "https://www.youtube.com/watch?v=abcdefghijk&private=source-secret";
const upstreamSecret = "upstream-private-response-never-log";
const controllerMessage = "VM controller POST /start returned HTTP 403. Check controller token, Cloud Run invocation policy and runtime VM permissions.";

class Element {
  constructor(id) {
    this.id = id; this.value = ""; this.textContent = ""; this.hidden = false;
    this.disabled = false; this.dataset = {}; this.listeners = new Map(); this.children = [];
    this.attributes = new Map(); this.open = false; this.type = "text";
  }
  addEventListener(name, callback) {
    if (!this.listeners.has(name)) this.listeners.set(name, []);
    this.listeners.get(name).push(callback);
  }
  async emit(name) {
    const event = { preventDefault() {} };
    await Promise.all((this.listeners.get(name) || []).map((callback) => callback(event)));
  }
  setAttribute(name, value) { this.attributes.set(name, String(value)); }
  getAttribute(name) { return this.attributes.get(name) ?? null; }
  removeAttribute(name) {
    this.attributes.delete(name);
    if (name === "src" || name === "href") this[name] = "";
  }
  append(...children) { this.children.push(...children); }
  replaceChildren(...children) { this.children = [...children]; }
  focus(options) { this.focused = true; this.focusOptions = options; }
  scrollIntoView(options) { this.scrollOptions = options; }
  pause() {}
  load() {}
  remove() {}
  showModal() { this.open = true; }
  close() { if (this.open) { this.open = false; void this.emit("close"); } }
}
function response(status, body = null) {
  return { status, ok: status >= 200 && status < 300, async json() { return body; }, async blob() { return new Blob(["controlled-mp4"]); } };
}
function pending(signal) {
  return new Promise((_, reject) => {
    const abort = () => reject(new DOMException("Stopped", "AbortError"));
    if (signal.aborted) abort(); else signal.addEventListener("abort", abort, { once: true });
  });
}
async function until(predicate) {
  for (let i = 0; i < 50; i += 1) {
    if (predicate()) return;
    await new Promise((resolve) => setImmediate(resolve));
  }
  assert.fail("Expected browser event was not observed");
}
function app(fetcher, { initialStorage = [], storageUnavailable = false, prefersDark = false, narrowScreen = false, reduceMotion = false, generatedIds = [jobId], operatorToken = token, sessionFetcher = (_, options) => options.method === "POST" || options.method === "DELETE" ? response(204) : response(200, { authenticated: true, saved: true }) } = {}) {
  const elements = new Map(), logs = [], requests = [], timers = new Map();
  const storage = new Map(initialStorage);
  const objectUrls = [], revokedUrls = [];
  const ids = [...generatedIds];
  let timerId = 0;
  const element = (id) => {
    if (!elements.has(id)) elements.set(id, new Element(id));
    return elements.get(id);
  };
  class BrowserURL extends URL {
    static createObjectURL() {
      const url = objectUrls.length ? `blob:private-object-url-${objectUrls.length + 1}` : "blob:private-object-url";
      objectUrls.push(url); return url;
    }
    static revokeObjectURL(url) { revokedUrls.push(url); }
  }
  const windowObject = element("window");
  const localStorage = {
    getItem(key) { if (storageUnavailable) throw Error("Storage unavailable"); return storage.get(key) ?? null; },
    setItem(key, value) { if (storageUnavailable) throw Error("Storage unavailable"); storage.set(key, String(value)); }
  };
  Object.defineProperty(windowObject, "localStorage", { get() { if (storageUnavailable) throw Error("Storage unavailable"); return localStorage; } });
  windowObject.matchMedia = (query) => ({ matches:
    query.includes("prefers-color-scheme: dark") ? prefersDark :
      query.includes("prefers-reduced-motion: reduce") ? reduceMotion :
        query.includes("max-width: 760px") ? narrowScreen : false
  });
  windowObject.crypto = { randomUUID() { return ids.shift() || jobId; } };
  const sandbox = {
    document: { getElementById: element, createElement: (id) => new Element(id), documentElement: element("html") },
    window: windowObject, URL: BrowserURL, AbortController, DOMException, Blob,
    Error, TypeError, SyntaxError, Date, performance,
    console: Object.fromEntries(["info", "debug", "warn", "error"].map((level) => [level, (...args) => logs.push({ level, prefix: args[0], ...args[1] })])),
    setTimeout(callback, delay) {
      const id = ++timerId; timers.set(id, { callback, delay });
      if (delay === 4000) queueMicrotask(() => { if (timers.delete(id)) callback(); });
      return id;
    },
    clearTimeout(id) { timers.delete(id); },
    async fetch(path, options) {
      requests.push({ path, options });
      return path === "/api/v1/session" ? sessionFetcher(path, options) : fetcher(path, options);
    }
  };
  vm.runInNewContext(source, sandbox, { filename: "app.js" });
  element("api-token").type = "password";
  element("api-token").value = operatorToken; element("youtube-url").value = video;
  return {
    element, logs, requests, storage, objectUrls, revokedUrls,
    async confirm() { await element("source-form").emit("submit"); },
    generate() { return element("continue-button").emit("click"); },
    fireTimeout(delay) {
      for (const [id, timer] of timers) if (timer.delay === delay) { timers.delete(id); timer.callback(); }
    }
  };
}
function events(browser, event) { return browser.logs.filter((entry) => entry.event === event); }
function downloadRequests(browser) { return browser.requests.filter(({ path }) => path.startsWith("/api/v1/downloads")); }
function sessionRequests(browser) { return browser.requests.filter(({ path }) => path === "/api/v1/session"); }
function assertPrivate(browser, additionalSecrets = []) {
  const serialized = JSON.stringify(browser.logs);
  for (const secret of [token, video, "abcdefghijk", "source-secret", upstreamSecret, "blob:private-object-url", "private-filename.mp4", ...additionalSecrets])
    assert.ok(!serialized.includes(secret), "Console must not expose " + secret);
  for (const entry of browser.logs) {
    assert.equal(entry.prefix, "[Frame]");
    assert.ok(Number.isFinite(Date.parse(entry.timestamp)));
    assert.ok(!("message" in entry) && !("headers" in entry) && !("body" in entry));
  }
}

async function configure(browser, { type, format, quality, bitrate }) {
  browser.element("media-type").value = type;
  await browser.element("media-type").emit("change");
  browser.element("output-format").value = format;
  browser.element("output-quality").value = quality;
  if (bitrate !== undefined) browser.element("audio-bitrate").value = String(bitrate);
}

test("video and audio configuration is confirmed before dispatch and maps to valid request fields", async () => {
  const expected = [
    { type: "video", format: "webm", quality: "720p", body: { url: "https://www.youtube.com/watch?v=abcdefghijk", type: "video", format: "webm", quality: "720p" } },
    { type: "audio", format: "mp3", quality: "best", bitrate: 192, body: { url: "https://www.youtube.com/watch?v=abcdefghijk", type: "audio", format: "mp3", quality: "best", audio_bitrate_kbps: 192 } },
    { type: "audio", format: "original", quality: "best", body: { url: "https://www.youtube.com/watch?v=abcdefghijk", type: "audio", format: "original", quality: "best" } }
  ];
  const ids = [jobId, "aaaaaaaa-bbbb-cccc-dddd-eeeeeeeeeeee", "bbbbbbbb-cccc-dddd-eeee-ffffffffffff"];
  for (let index = 0; index < expected.length; index += 1) {
    const item = expected[index];
    const browser = app((path, options) => {
      if (options.method === "POST") return response(202, { job_id: options.headers["X-Idempotency-Key"] });
      if (path.endsWith("/file")) return response(200);
      const id = path.split("/")[4];
      return response(200, { job_id: id, status: "completed", output: { filename: "fixture-output.mp4", mime_type: item.type === "audio" ? "audio/mpeg" : "video/mp4" }, download_url: path + "/file" });
    }, { generatedIds: [ids[index]] });
    await configure(browser, item);
    if (item.type === "audio") {
      assert.deepEqual(browser.element("output-quality").children.map((option) => option.value), ["best"], "audio exposes only the adapter-supported quality");
    }
    await browser.confirm();
    assert.equal(downloadRequests(browser).length, 0, "reviewing a source must not create extraction work");
    await browser.generate();
    const submitted = downloadRequests(browser).find(({ options }) => options.method === "POST");
    assert.ok(submitted);
    assert.deepEqual(JSON.parse(submitted.options.body), item.body);
    assert.equal(submitted.options.headers["X-Idempotency-Key"], ids[index]);
    assertPrivate(browser);
  }
});

test("invalid audio bitrate is blocked before opening confirmation or creating a job", async () => {
  const browser = app(() => response(500));
  await configure(browser, { type: "audio", format: "mp3", quality: "best", bitrate: 321 });
  await browser.confirm();
  assert.equal(downloadRequests(browser).length, 0);
  assert.equal(browser.element("confirm-dialog").open, false);
  assert.match(browser.element("bitrate-error").textContent, /32 to 320/);
  assert.equal(browser.element("audio-bitrate").getAttribute("aria-invalid"), "true");
  assertPrivate(browser);
});

test("logs confirmation, job transitions, transfer and playback without logging private data", async () => {
  let polls = 0;
  const browser = app((path, options) => {
    if (options.method === "POST") return response(202, { job_id: jobId });
    if (path.endsWith("/file")) return response(200);
    const status = ["queued", "running", "running", "completed"][polls++];
    return response(200, { status, progress: { stage: status === "running" ? "downloading" : "" }, output: { filename: "private-filename.mp4" }, download_url: `/api/v1/downloads/${jobId}/file` });
  });
  await browser.confirm();
  assert.equal(downloadRequests(browser).length, 0, "Confirmation must not dispatch a download");
  await browser.generate();
  await browser.element("result-video").emit("loadedmetadata");
  await browser.element("result-video").emit("playing");
  await browser.element("download-link").emit("click");
  assert.deepEqual(events(browser, "job.state").map((event) => event.job_status), ["queued", "running", "completed"]);
  assert.equal(events(browser, "job.progress_stage").length, 1);
  for (const event of ["source.validated", "confirmation.opened", "access.ready", "confirmation.accepted", "job.submission_started", "job.submitted", "file.transfer_started", "file.transfer_completed", "preview.ready", "preview.metadata_loaded", "preview.playing", "file.download_clicked"])
    assert.equal(events(browser, event).length, 1, event);
  assert.equal(browser.element("stage-pill").textContent, "READY");
  assert.ok(downloadRequests(browser).every(({ options }) => !options.headers.Authorization && options.headers["X-Frame-Client"] === "1" && options.credentials === "same-origin"));
  assert.equal(browser.logs.filter((entry) => entry.level === "error").length, 0);
  const objectUrl = browser.objectUrls[0];
  assert.equal(browser.element("result-video").src, objectUrl);
  assert.equal(browser.element("download-link").href, objectUrl);
  await browser.element("new-video-button").emit("click");
  assert.deepEqual(browser.revokedUrls, [objectUrl], "starting another export revokes the previous Blob URL");
  assert.equal(browser.element("result-video").src, "");
  assert.equal(browser.element("download-link").href, "");
  assertPrivate(browser);
});

test("theme fallback tolerates invalid or unavailable storage and never stores the API token", async () => {
  const invalid = app(() => response(500), { initialStorage: [["frame-theme", "violet"]], prefersDark: true });
  assert.equal(invalid.element("html").getAttribute("data-theme"), "dark", "invalid stored preference follows system theme");
  await invalid.element("theme-toggle").emit("click");
  assert.equal(invalid.element("html").getAttribute("data-theme"), "light");
  assert.equal(invalid.storage.get("frame-theme"), "light");
  assert.ok(![...invalid.storage.values()].includes(token));

  const unavailable = app(() => response(500), { storageUnavailable: true, prefersDark: true });
  assert.equal(unavailable.element("html").getAttribute("data-theme"), "dark");
  await unavailable.element("theme-toggle").emit("click");
  assert.equal(unavailable.element("html").getAttribute("data-theme"), "light", "theme toggle still works when storage throws");
  assert.equal(unavailable.logs.filter((entry) => entry.level === "error").length, 0);
});

test("startup reports saved and development access without conflating them", async () => {
  const saved = app(() => response(500), { operatorToken: "" });
  await until(() => events(saved, "access.ready").length === 1);
  assert.equal(saved.element("session-message").textContent, "Access saved");
  assert.equal(saved.element("forget-access-button").hidden, false);
  assert.equal(saved.element("api-token").value, "");
  const initialCheck = sessionRequests(saved)[0];
  assert.equal(initialCheck.options.method, "GET");
  assert.equal(initialCheck.options.headers.Authorization, undefined);
  assert.equal(initialCheck.options.headers["X-Frame-Client"], "1");
  assert.equal(initialCheck.options.credentials, "same-origin");

  const development = app(() => response(500), {
    operatorToken: "",
    sessionFetcher: () => response(200, { authenticated: true, saved: false })
  });
  await until(() => events(development, "access.ready").length === 1);
  assert.equal(development.element("session-message").textContent, "Access ready");
  assert.equal(development.element("forget-access-button").hidden, true);
  assert.notEqual(development.element("session-message").textContent, "Access saved");
});

test("saving requires a bearer once, verifies the cookie without bearer, and clears the field", async () => {
  let sessionGets = 0;
  const browser = app(() => response(500), {
    sessionFetcher: (_, options) => {
      if (options.method === "POST") return response(204);
      if (options.method === "GET" && sessionGets++ === 0) return response(401);
      return response(200, { authenticated: true, saved: true });
    }
  });
  await until(() => events(browser, "request.unauthenticated").length === 1);
  await browser.element("save-access-button").emit("click");
  assert.deepEqual(sessionRequests(browser).map(({ options }) => options.method), ["GET", "POST", "GET"]);
  const [startup, save, verify] = sessionRequests(browser);
  assert.equal(startup.options.headers.Authorization, undefined);
  assert.equal(save.options.headers.Authorization, `Bearer ${token}`);
  assert.equal(save.options.body, undefined, "the session endpoint takes the credential only in Authorization");
  assert.equal(verify.options.headers.Authorization, undefined);
  for (const request of [startup, save, verify]) {
    assert.equal(request.options.headers["X-Frame-Client"], "1");
    assert.equal(request.options.credentials, "same-origin");
  }
  assert.equal(browser.element("session-message").textContent, "Access saved");
  assert.equal(browser.element("access-settings").hidden, true);
  assert.equal(browser.element("api-token").value, "");
  assert.ok(![...browser.storage.values()].includes(token));
  assertPrivate(browser);
});

test("blocked cookies cannot produce a false saved state and retain the token for retry", async () => {
  let sessionGets = 0;
  const browser = app(() => response(500), {
    sessionFetcher: (_, options) => {
      if (options.method === "POST") return response(204);
      if (options.method === "GET" && sessionGets++ === 0) return response(401);
      return response(401);
    }
  });
  await until(() => events(browser, "request.unauthenticated").length === 1);
  await browser.element("save-access-button").emit("click");
  assert.match(browser.element("token-error").textContent, /allow cookies/i);
  assert.equal(browser.element("api-token").value, token);
  assert.notEqual(browser.element("session-message").textContent, "Access saved");
  assert.equal(browser.element("forget-access-button").hidden, true);
  assert.equal(downloadRequests(browser).length, 0);
  assertPrivate(browser);
});

test("forget sends unauthenticated DELETE and verifies that the saved session is gone", async () => {
  let deleted = false;
  const browser = app(() => response(500), {
    operatorToken: "",
    sessionFetcher: (_, options) => {
      if (options.method === "DELETE") { deleted = true; return response(204); }
      return deleted ? response(401) : response(200, { authenticated: true, saved: true });
    }
  });
  await until(() => events(browser, "access.ready").length === 1);
  await browser.element("forget-access-button").emit("click");
  assert.deepEqual(sessionRequests(browser).map(({ options }) => options.method), ["GET", "DELETE", "GET"]);
  const deletion = sessionRequests(browser)[1];
  assert.equal(deletion.options.headers.Authorization, undefined);
  assert.equal(deletion.options.headers["X-Frame-Client"], "1");
  assert.equal(deletion.options.credentials, "same-origin");
  assert.equal(browser.element("forget-access-button").hidden, true);
  assert.equal(browser.element("access-settings").hidden, false);
  assert.match(browser.element("toast-region").children[0].children[0].textContent, /Access removed/);
  assertPrivate(browser);
});

for (const dismissal of ["back-button", "escape"]) test(`dismissing confirmation while access save and verification are pending never submits: ${dismissal}`, async () => {
  let sessionGets = 0;
  let releaseSave, releaseVerify;
  const browser = app(() => response(500), {
    sessionFetcher: (_, options) => {
      if (options.method === "POST") return new Promise((resolve) => { releaseSave = resolve; });
      if (options.method === "GET" && sessionGets++ === 0) return response(401);
      return new Promise((resolve) => { releaseVerify = resolve; });
    }
  });
  await until(() => events(browser, "request.unauthenticated").length === 1);
  await browser.confirm();
  const generation = browser.generate();
  await until(() => typeof releaseSave === "function");
  if (dismissal === "back-button") await browser.element("back-button").emit("click");
  else browser.element("confirm-dialog").close();
  releaseSave(response(204));
  await until(() => typeof releaseVerify === "function");
  releaseVerify(response(200, { authenticated: true, saved: true }));
  await generation;
  assert.equal(browser.element("confirm-dialog").open, false);
  assert.equal(downloadRequests(browser).some(({ options }) => options.method === "POST"), false);
  assert.deepEqual(sessionRequests(browser).map(({ options }) => options.method), ["GET", "POST", "GET"]);
  assertPrivate(browser);
});

test("terminal failed job is an error even when its poll returned HTTP 200", async () => {
  const browser = app((_, options) => options.method === "POST" ? response(202, { job_id: jobId }) : response(200, { status: "failed", error: { code: "NETWORK", message: controllerMessage + " " + token + " " + upstreamSecret } }));
  await browser.confirm(); await browser.generate();
  const failure = events(browser, "job.failed")[0];
  assert.equal(failure.level, "error");
  assert.equal(failure.http_status, 200);
  assert.equal(failure.controller_http_status, 403);
  assert.equal(failure.controller_operation, "POST /start");
  assert.equal(failure.error_code, "NETWORK");
  assert.equal(failure.job_id, jobId);
  assert.equal(events(browser, "workflow.failed")[0].error_kind, "job_failed");
  const text = browser.element("error-description").textContent;
  assert.ok(text.includes("HTTP 403") && text.includes(jobId) && text.includes("NETWORK"));
  assert.ok(!text.includes(token));
  assertPrivate(browser);
});

for (const status of [401, 500]) test(`HTTP ${status} request failures include status and a safe error category`, async () => {
  const browser = app(() => response(status, { error: { code: "NETWORK", message: `${token} ${upstreamSecret} https://private.invalid/secret` } }));
  await browser.confirm(); await browser.generate();
  const error = events(browser, "request.failed")[0];
  assert.equal(error.http_status, status); assert.equal(error.error_kind, "http");
  assert.equal(error.method, "POST"); assert.equal(error.route, "/api/v1/downloads");
  assert.ok(Number.isSafeInteger(error.elapsed_ms)); assert.equal(error.job_id, null);
  assertPrivate(browser);
});

test("network errors omit the raw fetch error and its private URL", async () => {
  const browser = app(() => { throw new TypeError(`${token} ${upstreamSecret} ${video}`); });
  await browser.confirm(); await browser.generate();
  assert.equal(events(browser, "request.failed")[0].error_kind, "network");
  assert.ok(browser.element("error-description").textContent.includes("unreachable"));
  assertPrivate(browser);
});

test("request timeout is distinct from user cancellation", async () => {
  const browser = app((_, options) => pending(options.signal));
  await browser.confirm(); const generation = browser.generate();
  await until(() => events(browser, "request.started").some((event) => event.method === "POST" && event.route === "/api/v1/downloads"));
  browser.fireTimeout(30000); await generation;
  const error = events(browser, "request.failed")[0];
  assert.equal(error.error_kind, "timeout"); assert.equal(error.error_code, "TIMEOUT");
  assert.ok(browser.element("error-description").textContent.includes("did not respond in time"));
  assertPrivate(browser);
});

test("invalid JSON is classified as a protocol error without logging the body", async () => {
  const browser = app(() => ({ status: 200, ok: true, async json() { throw new SyntaxError(upstreamSecret + token); } }));
  await browser.confirm(); await browser.generate();
  assert.equal(events(browser, "request.failed")[0].error_kind, "protocol");
  assert.ok(browser.element("error-description").textContent.includes("invalid response"));
  assertPrivate(browser);
});

test("cancel aborts the pending poll and logs its captured job without producing an error", async () => {
  const browser = app((_, options) => {
    if (options.method === "POST") return response(202, { job_id: jobId });
    if (options.method === "DELETE") return response(204);
    return pending(options.signal);
  });
  await browser.confirm(); const generation = browser.generate();
  await until(() => downloadRequests(browser).length === 2);
  await browser.element("cancel-button").emit("click"); await generation;
  assert.equal(events(browser, "request.aborted")[0].job_id, jobId);
  assert.equal(events(browser, "cancel.confirmed")[0].job_id, jobId);
  assert.equal(browser.element("stage-pill").textContent, "WAITING");
  assert.equal(browser.logs.filter((entry) => entry.level === "error").length, 0);
  assertPrivate(browser);
});

test("invalid input and unknown upstream fields cannot be copied into console", async () => {
  const browser = app(() => response(202, { job_id: token }));
  browser.element("youtube-url").value = "https://private.invalid/" + token;
  await browser.confirm();
  assert.equal(events(browser, "source.validation_failed").length, 1);
  assert.equal(downloadRequests(browser).length, 0);
  browser.element("youtube-url").value = video; await browser.confirm(); await browser.generate();
  assert.ok(browser.element("error-description").textContent.includes("valid job ID"));
  assertPrivate(browser);
});


test("a worker-cancelled job is recorded as cancellation rather than an error", async () => {
  const browser = app((_, options) => options.method === "POST" ? response(202, { job_id: jobId }) : response(200, { status: "cancelled", error: { code: "CANCELLED", message: "Job cancelled." } }));
  await browser.confirm(); await browser.generate();
  assert.equal(events(browser, "job.cancelled")[0].job_id, jobId);
  assert.equal(events(browser, "workflow.cancelled")[0].error_code, "CANCELLED");
  assert.equal(browser.logs.filter((entry) => entry.level === "error").length, 0);
  assertPrivate(browser);
});

test("a cancelled generation ignores a late completed poll response", async () => {
  let finishPoll;
  const browser = app((path, options) => {
    if (options.method === "POST") return response(202, { job_id: jobId });
    if (options.method === "DELETE") return response(204);
    return new Promise((resolve) => { finishPoll = () => resolve(response(200, {
      job_id: jobId, status: "completed", download_url: `/api/v1/downloads/${jobId}/file`
    })); });
  });
  await browser.confirm();
  const generation = browser.generate();
  await until(() => downloadRequests(browser).length === 2);
  await browser.element("cancel-button").emit("click");
  assert.equal(browser.element("stage-pill").textContent, "WAITING");
  finishPoll();
  await generation;
  assert.equal(browser.requests.some(({ path }) => path.endsWith("/file")), false, "late completion must not fetch a file after cancellation");
  assert.equal(browser.element("success-state").hidden, true);
  assert.equal(events(browser, "workflow.failed").length, 0);
  assertPrivate(browser);
});

for (const [caseName, maliciousUrl] of [
  ["off-origin URL", "https://attacker.invalid/collect"],
  ["different job route", `/api/v1/downloads/aaaaaaaa-bbbb-cccc-dddd-eeeeeeeeeeee/file`],
  ["query-tainted route", `/api/v1/downloads/${jobId}/file?next=https://attacker.invalid/collect`]
]) test(`malicious completed-file URL is rejected before fetch: ${caseName}`, async () => {
  const browser = app((path, options) => {
    if (options.method === "POST") return response(202, { job_id: jobId });
    if (path.endsWith(`/downloads/${jobId}`)) return response(200, {
      job_id: jobId, status: "completed", output: { filename: "private-filename.mp4" }, download_url: maliciousUrl
    });
    return response(200);
  });
  await browser.confirm(); await browser.generate();
  assert.equal(browser.requests.some(({ path }) => path.includes("/file")), false);
  assert.equal(events(browser, "workflow.failed")[0]?.error_kind, "protocol");
  assert.ok(browser.element("error-description").textContent.includes("unexpected download URL"));
  assertPrivate(browser);
});

test("an uncertain POST can be looked up by its idempotency UUID through saved access", async () => {
  const browser = app((path, options) => {
    if (options.method === "POST") return response(500, { error: { code: "INTERNAL", message: "The response was interrupted." } });
    if (path === `/api/v1/downloads/${jobId}`) return response(200, {
      job_id: jobId, status: "completed", output: { filename: "private-filename.mp4", mime_type: "video/mp4" },
      download_url: `/api/v1/downloads/${jobId}/file`
    });
    return response(200);
  });
  await browser.confirm(); await browser.generate();
  const post = downloadRequests(browser)[0];
  assert.equal(post.options.method, "POST");
  assert.equal(post.options.headers["X-Idempotency-Key"], jobId);
  assert.equal(post.options.headers.Authorization, undefined);
  assert.equal(browser.element("check-job-button").hidden, false);
  assert.equal(browser.element("job-id").textContent.includes(jobId), true);

  await browser.element("check-job-button").emit("click");
  assert.deepEqual(downloadRequests(browser).map(({ options }) => options.method), ["POST", "GET", "GET"]);
  assert.ok(downloadRequests(browser).every(({ options }) => options.headers.Authorization === undefined && options.credentials === "same-origin"));
  assert.equal(browser.element("stage-pill").textContent, "READY");
  assertPrivate(browser);
});

test("a 401 recovery saves the replacement token, preserves the job and uses cookie auth for cancellation", async () => {
  const refreshedToken = "second-private-api-token-012345678901234";
  let polls = 0;
  const browser = app((path, options) => {
    if (options.method === "POST") return response(202, { job_id: jobId });
    if (options.method === "DELETE") return response(204);
    if (path === `/api/v1/downloads/${jobId}` && polls++ === 0) return response(401, { error: { code: "UNAUTHORIZED", message: "Unauthorized" } });
    return response(200, { job_id: jobId, status: "queued" });
  });
  await browser.confirm(); await browser.generate();
  assert.equal(browser.element("cancel-recovery-button").hidden, false);
  assert.equal(browser.element("job-id").textContent.includes(jobId), true);
  browser.element("api-token").value = refreshedToken;
  await browser.element("save-access-button").emit("click");
  const save = sessionRequests(browser).find(({ options }) => options.method === "POST");
  assert.equal(save.options.headers.Authorization, `Bearer ${refreshedToken}`);
  assert.equal(save.options.body, undefined);
  const verified = sessionRequests(browser).filter(({ options }) => options.method === "GET").at(-1);
  assert.equal(verified.options.headers.Authorization, undefined);
  assert.equal(browser.element("api-token").value, "", "verified saved access clears the replacement token field");
  assert.equal(browser.element("job-id").textContent.includes(jobId), true);
  await browser.element("cancel-recovery-button").emit("click");
  assert.deepEqual(downloadRequests(browser).map(({ options }) => options.method), ["POST", "GET", "DELETE"]);
  assert.equal(downloadRequests(browser)[2].path, `/api/v1/downloads/${jobId}`);
  assert.equal(downloadRequests(browser)[2].options.headers.Authorization, undefined);
  assert.equal(downloadRequests(browser)[2].options.credentials, "same-origin");
  assert.equal(browser.element("stage-pill").textContent, "WAITING");
  assertPrivate(browser, [refreshedToken]);
});

test("a POST response with a different UUID is never trusted as the job identity", async () => {
  const wrongId = "cccccccc-dddd-eeee-ffff-000000000000";
  const browser = app(() => response(202, { job_id: wrongId }));
  await browser.confirm(); await browser.generate();
  assert.equal(downloadRequests(browser).length, 1);
  assert.equal(downloadRequests(browser)[0].options.headers["X-Idempotency-Key"], jobId);
  assert.ok(browser.element("error-description").textContent.includes("valid job ID"));
  assert.ok(browser.element("job-id").textContent.includes(jobId));
  assert.ok(!browser.element("job-id").textContent.includes(wrongId));
  assert.equal(browser.element("cancel-recovery-button").hidden, false);
  assertPrivate(browser);
});

test("a poll response with a different UUID is rejected and retains only the requested job", async () => {
  const wrongId = "dddddddd-eeee-ffff-0000-111111111111";
  const browser = app((path, options) => options.method === "POST"
    ? response(202, { job_id: jobId })
    : response(200, { job_id: wrongId, status: "queued" }));
  await browser.confirm(); await browser.generate();
  assert.equal(downloadRequests(browser).length, 2);
  assert.equal(events(browser, "workflow.failed")[0]?.error_kind, "protocol");
  assert.ok(browser.element("job-id").textContent.includes(jobId));
  assert.ok(!browser.element("job-id").textContent.includes(wrongId));
  assert.equal(browser.element("check-job-button").hidden, false);
  assert.equal(browser.requests.some(({ path }) => path.endsWith("/file")), false);
  assertPrivate(browser);
});


test("pre-submission diagnostics never reuse a completed job ID for the next video", async () => {
  let submissions = 0;
  const nextId = "aaaaaaaa-bbbb-cccc-dddd-eeeeeeeeeeee";
  const browser = app((path, options) => {
    if (options.method === "POST") return response(202, { job_id: ++submissions === 1 ? jobId : nextId });
    if (path.endsWith("/file")) return response(200);
    return response(200, { status: "completed", download_url: path + "/file" });
  }, { generatedIds: [jobId, nextId] });
  await browser.confirm(); await browser.generate();
  const firstRunCount = browser.logs.length;
  await browser.confirm(); await browser.generate();
  const preparation = browser.logs.slice(firstRunCount).filter((entry) => entry.event.startsWith("source.") || entry.event.startsWith("confirmation.") || entry.event.startsWith("access.") || (entry.event === "workflow.step" && ["confirmation", "idle"].includes(entry.step)));
  assert.ok(preparation.length >= 6);
  assert.ok(preparation.every((entry) => entry.job_id === null));
  assert.equal(events(browser, "job.submitted")[1].job_id, nextId);
  assertPrivate(browser);
});
