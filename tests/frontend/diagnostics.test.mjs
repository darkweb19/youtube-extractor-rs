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
    this.attributes = new Map(); this.open = false;
  }
  addEventListener(name, callback) {
    if (!this.listeners.has(name)) this.listeners.set(name, []);
    this.listeners.get(name).push(callback);
  }
  async emit(name) {
    const event = { preventDefault() {} };
    await Promise.all((this.listeners.get(name) || []).map((callback) => callback(event)));
  }
  setAttribute(name, value) { this.attributes.set(name, value); }
  removeAttribute(name) { this.attributes.delete(name); }
  append(...children) { this.children.push(...children); }
  focus() { this.focused = true; }
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
function app(fetcher) {
  const elements = new Map(), logs = [], requests = [], timers = new Map();
  let timerId = 0;
  const element = (id) => {
    if (!elements.has(id)) elements.set(id, new Element(id));
    return elements.get(id);
  };
  class BrowserURL extends URL {
    static createObjectURL() { return "blob:private-object-url"; }
    static revokeObjectURL() {}
  }
  const sandbox = {
    document: { getElementById: element, createElement: (id) => new Element(id) },
    window: new Element("window"), URL: BrowserURL, AbortController, DOMException, Blob,
    Error, TypeError, SyntaxError, Date, performance,
    console: Object.fromEntries(["info", "debug", "warn", "error"].map((level) => [level, (...args) => logs.push({ level, prefix: args[0], ...args[1] })])),
    setTimeout(callback, delay) {
      const id = ++timerId; timers.set(id, { callback, delay });
      if (delay === 4000) queueMicrotask(() => { if (timers.delete(id)) callback(); });
      return id;
    },
    clearTimeout(id) { timers.delete(id); },
    async fetch(path, options) { requests.push({ path, options }); return fetcher(path, options); }
  };
  vm.runInNewContext(source, sandbox, { filename: "app.js" });
  element("api-token").value = token; element("youtube-url").value = video;
  return {
    element, logs, requests,
    async confirm() { await element("source-form").emit("submit"); },
    generate() { return element("continue-button").emit("click"); },
    fireTimeout(delay) {
      for (const [id, timer] of timers) if (timer.delay === delay) { timers.delete(id); timer.callback(); }
    }
  };
}
function events(browser, event) { return browser.logs.filter((entry) => entry.event === event); }
function assertPrivate(browser) {
  const serialized = JSON.stringify(browser.logs);
  for (const secret of [token, video, "abcdefghijk", "source-secret", upstreamSecret, "blob:private-object-url", "private-filename.mp4"])
    assert.ok(!serialized.includes(secret), "Console must not expose " + secret);
  for (const entry of browser.logs) {
    assert.equal(entry.prefix, "[Frame]");
    assert.ok(Number.isFinite(Date.parse(entry.timestamp)));
    assert.ok(!("message" in entry) && !("headers" in entry) && !("body" in entry));
  }
}

test("logs confirmation, job transitions, transfer and playback without logging private data", async () => {
  let polls = 0;
  const browser = app((path, options) => {
    if (options.method === "POST") return response(202, { job_id: jobId });
    if (path.endsWith("/file")) return response(200);
    const status = ["queued", "running", "running", "completed"][polls++];
    return response(200, { status, progress: { stage: status === "running" ? "downloading" : "" }, output: { filename: "private-filename.mp4" }, download_url: `/api/v1/downloads/${jobId}/file` });
  });
  await browser.confirm();
  assert.equal(browser.requests.length, 0, "Confirmation must not dispatch a download");
  await browser.generate();
  await browser.element("result-video").emit("loadedmetadata");
  await browser.element("result-video").emit("playing");
  await browser.element("download-link").emit("click");
  assert.deepEqual(events(browser, "job.state").map((event) => event.job_status), ["queued", "running", "completed"]);
  assert.equal(events(browser, "job.progress_stage").length, 1);
  for (const event of ["source.validated", "confirmation.opened", "access.ready", "confirmation.accepted", "job.submission_started", "job.submitted", "file.transfer_started", "file.transfer_completed", "preview.ready", "preview.metadata_loaded", "preview.playing", "file.download_clicked"])
    assert.equal(events(browser, event).length, 1, event);
  assert.equal(browser.element("stage-pill").textContent, "READY");
  assert.ok(browser.requests.every(({ options }) => options.headers.Authorization === `Bearer ${token}`));
  assert.equal(browser.logs.filter((entry) => entry.level === "error").length, 0);
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
  await until(() => events(browser, "request.started").length === 1);
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
  await until(() => browser.requests.length === 2);
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
  assert.equal(browser.requests.length, 0);
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


test("pre-submission diagnostics never reuse a completed job ID for the next video", async () => {
  let submissions = 0;
  const nextId = "aaaaaaaa-bbbb-cccc-dddd-eeeeeeeeeeee";
  const browser = app((path, options) => {
    if (options.method === "POST") return response(202, { job_id: ++submissions === 1 ? jobId : nextId });
    if (path.endsWith("/file")) return response(200);
    return response(200, { status: "completed", download_url: path + "/file" });
  });
  await browser.confirm(); await browser.generate();
  const firstRunCount = browser.logs.length;
  await browser.confirm(); await browser.generate();
  const preparation = browser.logs.slice(firstRunCount).filter((entry) => entry.event.startsWith("source.") || entry.event.startsWith("confirmation.") || entry.event.startsWith("access.") || (entry.event === "workflow.step" && ["confirmation", "idle"].includes(entry.step)));
  assert.ok(preparation.length >= 6);
  assert.ok(preparation.every((entry) => entry.job_id === null));
  assert.equal(events(browser, "job.submitted")[1].job_id, nextId);
  assertPrivate(browser);
});
