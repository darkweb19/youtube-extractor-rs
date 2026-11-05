// Offline Chromium workflow regression. API replies are fixtures; this does not
// contact YouTube or establish upstream media availability.
import assert from "node:assert/strict";
import { createServer } from "node:http";
import { mkdir, readFile } from "node:fs/promises";
import { createRequire } from "node:module";
import { fileURLToPath } from "node:url";

const require = createRequire(import.meta.url);
const playwrightCandidates = [process.env.PLAYWRIGHT_MODULE, "playwright", "/opt/codex/cua_node/lib/node_modules/playwright"].filter(Boolean);
let chromium;
let playwrightLoadError;
for (const candidate of playwrightCandidates) {
  try {
    chromium = require(candidate).chromium;
    if (chromium) break;
  } catch (error) {
    playwrightLoadError = error;
  }
}
if (!chromium) throw new Error("Install Playwright or set PLAYWRIGHT_MODULE to its module path.", { cause: playwrightLoadError });
const repo = new URL("../../", import.meta.url);
const assets = new Map([
  ["/", ["index.html", "text/html; charset=utf-8"]],
  ["/app.css", ["app.css", "text/css; charset=utf-8"]],
  ["/app.js", ["app.js", "text/javascript; charset=utf-8"]]
]);
const fixtureToken = "frame-browser-fixture-token-0123456789";
const refreshedToken = "frame-refreshed-fixture-token-012345678901234";
const fixtureUrl = "https://youtu.be/abcdefghijk?si=private-fragment";
const canonicalUrl = "https://www.youtube.com/watch?v=abcdefghijk";
const jobId = "494d32e9-9c34-479b-8c12-b12401c7fca3";
const cookieName = "__Secure-frame_session";
const screenshotDir = fileURLToPath(new URL("work/frame-ux/", repo));
const audioFixture = Buffer.from("SUQzBAAAAAAAIlRTU0UAAAAOAAADTGF2ZjYxLjcuMTAzAAAAAAAAAAAAAAD/85DEAAWgBt5hQQABkhCd6zRh4efhmHjz8EAA7/Dw8P/gAIU2gWAhZQBwCADgYDgNKWNpKXJVVAMKM2GHUYSYQLA6csMTPpw5pJrIBoAkAr+EoAsAW/BdAogKiC/xFCKIR6Pfg0FQVZ+WCoKiIKoJMGjQAA+ciOhwFwCS07LVADDwXwwO6gAw8E8C4SVqADDwXwKBFapMQU1FADDwTwLhJWpMQU1FqqoAMOBfAoO7TEFNRVVVACDQTwLx7ExBTUVVADDwXwcBFZVMQU1FVQAw8E8HYSVVTEFNRVUAIPBfBAFr3ExBTUWqqgAw8E8HY9lMQU1FVQAg8F8AgSjFTEFNRVVVADD/80DE4RKo1q8ZmlAA4E8HY7FMQU1FVVUAQPBfAAd1TEFNRVUAMPBPAeDQtUxBTUVVADDwXwGAyMVMQU1FVQAw8E8B4RKq/AAd3eOABTxuN2nAYhS3qd2IZNyCVQ5Yw/86fPuqlJY2AP/zEMTxA+g+5wHHAAEA0fjIA6OPzf/+H5YA//MQxO4BqDrugAHSJHAGD0cCgUCAQA3yE5P/8xDE9AHQNuqAAdIlWlVP/tll7suj/qCv5P/zEMT5AdA26oAB0iXlr/ASIAXA2TJNXAuR//MQxPoB0DbqgAHSJQwW/9MsLS9/mJxRuaf/8xDE+QG4OuqAAdIkwWFxG/9ChZJMQU1FM//zEMT5AcA64oACkiQuMTAwqkxBTUUzLjEw//MQxPoB0DbqgAHSJTCqqqqqqqqqqqqqqqr/8xDE+gHQNuqAAdIlqqqqqqqqqqqqqqqqqv/zEMT6Afg+4oACmCSqqqqqqqqqqqqqqqqq//MQxPkBuDrqgAHSJKqqqqqqqqqqqqqqqqr/8xDE+gHgNuKAAdYlqqqqqqqqqqqqqqqqqv/zEMT5AbA66oABzCSqqqqqqqqqqqqqqqqq//MQxPkBsDrugAFSJKqqqqqqqqqqqqqqqqr/8xDE+gHgNuaAAdIlqqqqqqqqqqqqqqqqqv/zEMT6AeA25oAB0iWqqqqqqqqqqqqqqqqq//MQxPoByDrmgAHQJKqqqqqqqqqqqqqqqqr/8yDE/ws4eqZ5WRABqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqq//MwxPkPeNau+ZpoAKqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqq//MQxPIAAANIAcAAAKqqqqqqqqqqqqqqqqo=", "base64");

async function startStaticServer() {
  const server = createServer(async (request, response) => {
    const route = new URL(request.url || "/", "http://127.0.0.1").pathname;
    const asset = assets.get(route);
    if (!asset) {
      response.writeHead(404).end();
      return;
    }
    try {
      const body = await readFile(new URL(`web/${asset[0]}`, repo));
      const headers = { "Content-Type": asset[1], "Cache-Control": "no-store", "X-Content-Type-Options": "nosniff" };
      if (route === "/") {
        headers["Referrer-Policy"] = "strict-origin-when-cross-origin";
        headers["Content-Security-Policy"] = "default-src 'none'; script-src 'self'; style-src 'self'; connect-src 'self'; img-src 'self' data:; media-src blob:; frame-src https://www.youtube-nocookie.com; base-uri 'none'; form-action 'self'; frame-ancestors https://sujanshrestha.ca; object-src 'none'";
      }
      response.writeHead(200, headers).end(body);
    } catch {
      response.writeHead(500).end();
    }
  });
  await new Promise((resolve) => server.listen(0, "127.0.0.1", resolve));
  const address = server.address();
  return { server, origin: `http://127.0.0.1:${address.port}` };
}

async function attachFixtures(page, { creationStatus = 202, jobStatus = "completed", sessionMode = "missing", rejectSaveCookie = false, unauthorizedPolls = 0, sessionGates = null } = {}) {
  const requests = [];
  let activeJobId = jobId;
  let activeMediaType = "video";
  let activeCookie = sessionMode === "saved" || sessionMode === "expire-on-poll" ? "initial-session-cookie" : null;
  let activeBearer = fixtureToken;
  let remainingUnauthorizedPolls = unauthorizedPolls;
  const state = { sessionMode, get activeCookie() { return activeCookie; } };
  await page.route("**/api/v1/**", async (route) => {
    const request = route.request();
    const url = new URL(request.url());
    const method = request.method();
    const body = method === "POST" && request.postData() ? request.postDataJSON() : null;
    requests.push({ method, path: url.pathname, headers: request.headers(), body });

    if (url.pathname === "/api/v1/session" && method === "GET") {
      const cookies = request.headers().cookie || "";
      if (sessionMode === "development") {
        await route.fulfill({ status: 200, contentType: "application/json", body: JSON.stringify({ authenticated: true, saved: false }) });
      } else if (activeCookie && cookies.split("; ").includes(`${cookieName}=${activeCookie}`)) {
        const sessionGetCount = requests.filter((entry) => entry.path === "/api/v1/session" && entry.method === "GET").length;
        if (sessionGetCount > 1 && sessionGates?.verify) await sessionGates.verify.promise;
        await route.fulfill({ status: 200, contentType: "application/json", body: JSON.stringify({ authenticated: true, saved: true }) });
      } else {
        await route.fulfill({ status: 401, contentType: "application/json", body: JSON.stringify({ error: { code: "UNAUTHORIZED", message: "Unauthorized" } }) });
      }
      return;
    }
    if (url.pathname === "/api/v1/session" && method === "POST") {
      const auth = request.headers().authorization || "";
      if (!auth.startsWith("Bearer ") || request.headers()["x-frame-client"] !== "1") {
        await route.fulfill({ status: 401, contentType: "application/json", body: JSON.stringify({ error: { code: "UNAUTHORIZED", message: "Unauthorized" } }) });
      } else if (rejectSaveCookie) {
        activeBearer = auth.slice("Bearer ".length);
        await route.fulfill({ status: 204, body: "" });
      } else {
        if (sessionGates?.save) await sessionGates.save.promise;
        activeBearer = auth.slice("Bearer ".length);
        activeCookie = `saved-session-cookie-${requests.filter((entry) => entry.path === "/api/v1/session" && entry.method === "POST").length}`;
        await route.fulfill({ status: 204, headers: { "Set-Cookie": `${cookieName}=${activeCookie}; Max-Age=31536000; Path=/api; Secure; HttpOnly; SameSite=Strict` }, body: "" });
      }
      return;
    }
    if (url.pathname === "/api/v1/session" && method === "DELETE") {
      activeCookie = null;
      await route.fulfill({ status: 204, headers: { "Set-Cookie": `${cookieName}=; Max-Age=0; Path=/api; Secure; HttpOnly; SameSite=Strict` }, body: "" });
      return;
    }

    if (method === "POST" && url.pathname === "/api/v1/downloads") {
      activeJobId = request.headers()["x-idempotency-key"] || jobId;
      activeMediaType = body?.type || "video";
      const requestCookie = request.headers().cookie || "";
      const authenticated = activeCookie && requestCookie.split("; ").includes(`${cookieName}=${activeCookie}`)
        || request.headers().authorization === `Bearer ${activeBearer}`
        || sessionMode === "development";
      if (!authenticated) {
        await route.fulfill({ status: 401, contentType: "application/json", body: JSON.stringify({ error: { code: "UNAUTHORIZED", message: "Unauthorized" } }) });
        return;
      }
      if (creationStatus !== 202) {
        await route.fulfill({
          status: creationStatus,
          contentType: "application/json",
          body: JSON.stringify({ error: { code: "UNAUTHORIZED", message: "The API token was rejected." } })
        });
      } else {
        await route.fulfill({ status: 202, contentType: "application/json", body: JSON.stringify({ job_id: activeJobId }) });
      }
      return;
    }
    if (method === "GET" && url.pathname === `/api/v1/downloads/${activeJobId}`) {
      const requestCookie = request.headers().cookie || "";
      const authenticated = activeCookie && requestCookie.split("; ").includes(`${cookieName}=${activeCookie}`)
        || request.headers().authorization === `Bearer ${activeBearer}`
        || sessionMode === "development";
      if (!authenticated || remainingUnauthorizedPolls > 0) {
        if (remainingUnauthorizedPolls > 0) remainingUnauthorizedPolls -= 1;
        await route.fulfill({ status: 401, contentType: "application/json", body: JSON.stringify({ error: { code: "UNAUTHORIZED", message: "Unauthorized" } }) });
        return;
      }
      await route.fulfill({
        status: 200,
        contentType: "application/json",
        body: JSON.stringify({
          job_id: activeJobId,
          status: jobStatus,
          progress: { downloaded_bytes: 32768, total_bytes: 32768, percent: 100 },
          output: {
            filename: activeMediaType === "audio" ? "fixture-output.mp3" : "fixture-output.mp4",
            size_bytes: 32768,
            mime_type: activeMediaType === "audio" ? "audio/mpeg" : "video/mp4"
          },
          error: null,
          download_url: `/api/v1/downloads/${activeJobId}/file`
        })
      });
      return;
    }
    if (method === "GET" && url.pathname === `/api/v1/downloads/${activeJobId}/file`) {
      const requestCookie = request.headers().cookie || "";
      const authenticated = activeCookie && requestCookie.split("; ").includes(`${cookieName}=${activeCookie}`)
        || request.headers().authorization === `Bearer ${activeBearer}`
        || sessionMode === "development";
      if (!authenticated) {
        await route.fulfill({ status: 401, contentType: "application/json", body: JSON.stringify({ error: { code: "UNAUTHORIZED", message: "Unauthorized" } }) });
        return;
      }
      await route.fulfill({ status: 200, contentType: activeMediaType === "audio" ? "audio/mpeg" : "video/mp4", body: activeMediaType === "audio" ? audioFixture : Buffer.from("offline-video-fixture") });
      return;
    }
    if (method === "DELETE" && url.pathname === `/api/v1/downloads/${activeJobId}`) {
      const requestCookie = request.headers().cookie || "";
      const authenticated = activeCookie && requestCookie.split("; ").includes(`${cookieName}=${activeCookie}`)
        || request.headers().authorization === `Bearer ${activeBearer}`
        || sessionMode === "development";
      if (!authenticated) {
        await route.fulfill({ status: 401, contentType: "application/json", body: JSON.stringify({ error: { code: "UNAUTHORIZED", message: "Unauthorized" } }) });
        return;
      }
      await route.fulfill({ status: 204, body: "" });
      return;
    }
    await route.fulfill({ status: 404, contentType: "application/json", body: "{}" });
  });
  // Keep the confirmation step local. The fixture does not verify YouTube playback.
  await page.route("https://www.youtube-nocookie.com/**", (route) => route.fulfill({
    status: 200,
    contentType: "text/html",
    body: "<!doctype html><title>Offline confirmation fixture</title>"
  }));
  Object.defineProperty(requests, "fixtureState", { value: state });
  return requests;
}

async function newPage(browser, origin, viewport, { invalidSavedTheme = false, reducedMotion, captureScroll = false, fixtureOptions = {} } = {}) {
  const context = await browser.newContext({ viewport });
  if (["saved", "expire-on-poll", "invalid"].includes(fixtureOptions.sessionMode)) {
    await context.addCookies([{
      name: cookieName,
      value: fixtureOptions.sessionMode === "invalid" ? "expired-session-cookie" : "initial-session-cookie",
      domain: new URL(origin).hostname,
      path: "/api",
      httpOnly: true,
      secure: true,
      sameSite: "Strict",
      expires: Math.floor(Date.now() / 1000) + 31536000
    }]);
  }
  if (invalidSavedTheme) {
    await context.addInitScript(() => {
      if (!sessionStorage.getItem("frame-theme-seeded")) {
        localStorage.setItem("frame-theme", "invalid-theme-value");
        sessionStorage.setItem("frame-theme-seeded", "yes");
      }
    });
  }
  if (captureScroll) {
    await context.addInitScript(() => {
      window.__frameScrollCalls = [];
      const nativeScrollIntoView = Element.prototype.scrollIntoView;
      Element.prototype.scrollIntoView = function (options) {
        window.__frameScrollCalls.push({ target: this.id, options });
        return nativeScrollIntoView.call(this, options);
      };
    });
  }
  const page = await context.newPage();
  const consoleMessages = [];
  page.on("console", (message) => consoleMessages.push(message.text()));
  if (reducedMotion) await page.emulateMedia({ reducedMotion });
  const requests = await attachFixtures(page, fixtureOptions);
  const response = await page.goto(origin, { waitUntil: "networkidle" });
  assert.equal(response?.status(), 200);
  assert.equal(response?.headers()["x-content-type-options"], "nosniff");
  assert.equal(response?.headers()["content-security-policy"], "default-src 'none'; script-src 'self'; style-src 'self'; connect-src 'self'; img-src 'self' data:; media-src blob:; frame-src https://www.youtube-nocookie.com; base-uri 'none'; form-action 'self'; frame-ancestors https://sujanshrestha.ca; object-src 'none'");
  return { context, page, requests, consoleMessages };
}

function downloadRequests(requests) { return requests.filter((entry) => entry.path.startsWith("/api/v1/downloads")); }
function sessionRequests(requests) { return requests.filter((entry) => entry.path === "/api/v1/session"); }
function deferred() {
  let resolve;
  const promise = new Promise((done) => { resolve = done; });
  return { promise, resolve };
}

async function enterSource(page, { keyboard = false } = {}) {
  if (!(await page.locator("#access-settings").evaluate((node) => node.open))) {
    await page.locator("#access-settings summary").click();
  }
  await page.locator("#api-token").fill(fixtureToken);
  await page.locator("#youtube-url").fill(fixtureUrl);
  if (keyboard) await page.locator("#youtube-url").press("Enter");
  else await page.locator("#preview-button").click();
  await page.locator("#confirm-dialog").waitFor({ state: "visible" });
}

async function chooseAudioMp3(page) {
  await page.locator("#media-type").selectOption("audio");
  await page.locator("#output-format").selectOption("mp3");
  await page.locator("#output-quality").selectOption("best");
  await page.locator("#audio-bitrate").fill("192");
}

async function chooseVideoWebm(page) {
  await page.locator("#media-type").selectOption("video");
  await page.locator("#output-format").selectOption("webm");
  await page.locator("#output-quality").selectOption("720p");
}

async function testSuccessfulKeyboardAudioWorkflow(browser, origin) {
  const { context, page, requests } = await newPage(browser, origin, { width: 1280, height: 900 }, { invalidSavedTheme: true });
  try {
    const initialTheme = await page.locator("html").getAttribute("data-theme");
    assert.ok(["light", "dark"].includes(initialTheme), "invalid stored theme must fall back to a supported theme");
    const toggle = page.locator("#theme-toggle");
    await toggle.click();
    const changedTheme = await page.locator("html").getAttribute("data-theme");
    assert.notEqual(changedTheme, initialTheme, "theme toggle must switch themes");
    assert.equal(await page.evaluate(() => localStorage.getItem("frame-theme")), changedTheme);
    await page.reload({ waitUntil: "networkidle" });
    assert.equal(await page.locator("html").getAttribute("data-theme"), changedTheme, "theme choice should survive reload");

    await chooseAudioMp3(page);
    await enterSource(page, { keyboard: true });
    assert.equal(downloadRequests(requests).length, 0, "source confirmation must not create a job");
    assert.equal(await page.locator("#confirm-dialog").evaluate((node) => node instanceof HTMLDialogElement), true);
    assert.equal(await page.locator("#confirm-dialog").evaluate((dialog) => dialog.contains(document.activeElement)), true, "native confirmation should place focus within the dialog");
    await page.screenshot({ path: `${screenshotDir}/confirmation.png`, fullPage: true });

    await page.keyboard.press("Escape");
    await page.locator("#confirm-dialog").waitFor({ state: "hidden" });
    assert.equal(downloadRequests(requests).length, 0, "closing confirmation must not create a job");
    await enterSource(page, { keyboard: true });
    await page.locator("#continue-button").press("Enter");

    await page.locator("#success-state").waitFor({ state: "visible", timeout: 12000 });
    const create = downloadRequests(requests).find((item) => item.method === "POST");
    assert.ok(create, "confirmed flow must create a job");
    assert.deepEqual(create.body, {
      url: canonicalUrl,
      type: "audio",
      format: "mp3",
      quality: "best",
      audio_bitrate_kbps: 192
    });
    assert.equal(create.headers.authorization, undefined, "saved requests use the HttpOnly cookie without a bearer header");
    assert.equal(create.headers["x-frame-client"], "1");
    assert.ok(create.headers.cookie.includes(`${cookieName}=`));
    assert.equal(downloadRequests(requests).filter((item) => item.path === "/api/v1/videos/info").length, 0, "fixture flow should use the existing confirmation source");
    const fileRequest = downloadRequests(requests).find((item) => item.path.endsWith("/file"));
    assert.equal(fileRequest?.headers.authorization, undefined);
    assert.ok(fileRequest?.headers.cookie.includes(`${cookieName}=`));
    assert.ok(requests.every(({ headers }) => headers["x-frame-client"] === "1"));
    assert.equal(await page.locator("#result-audio").getAttribute("src"), await page.locator("#download-link").getAttribute("href"));
    assert.match(await page.locator("#result-audio").getAttribute("src"), /^blob:/);
    assert.equal(await page.locator("#result-audio").isVisible(), true);
    assert.equal(await page.evaluate(() => Object.values(localStorage).some((value) => value === "frame-browser-fixture-token-0123456789")), false, "token must not be stored in local storage");
    await page.screenshot({ path: `${screenshotDir}/success-dark.png`, fullPage: true });

    const source = await page.locator(".source-panel").boundingBox();
    const output = await page.locator(".result-panel").boundingBox();
    assert.ok(source && output && source.x < output.x, "wide layout should place source before output in columns");
  } finally {
    await context.close();
  }
}

async function testCancellation(browser, origin) {
  const { context, page, requests } = await newPage(browser, origin, { width: 1280, height: 900 });
  try {
    await page.locator("#youtube-url").fill(fixtureUrl);
    await chooseVideoWebm(page);
    await enterSource(page);
    assert.equal(downloadRequests(requests).length, 0);
    await page.locator("#continue-button").click();
    await page.locator("#cancel-button").waitFor({ state: "visible" });
    await page.locator("#cancel-button").click();
    await page.locator("#stage-pill").filter({ hasText: "WAITING" }).waitFor({ state: "visible" });
    assert.deepEqual(downloadRequests(requests).map((item) => item.method), ["POST", "DELETE"], "cancellation before the first poll should stop polling");
    assert.deepEqual(downloadRequests(requests)[0].body, { url: canonicalUrl, type: "video", format: "webm", quality: "720p" });
    assert.equal(downloadRequests(requests)[0].headers["x-idempotency-key"], downloadRequests(requests)[1].path.split("/")[4]);
    assert.equal(downloadRequests(requests)[1].headers.authorization, undefined);
    assert.ok(downloadRequests(requests)[1].headers.cookie.includes(`${cookieName}=`));
  } finally {
    await context.close();
  }
}

async function testRejectedTokenAndNarrowLayout(browser, origin) {
  const { context, page, requests } = await newPage(browser, origin, { width: 390, height: 844 }, { fixtureOptions: { creationStatus: 401 } });
  try {
    await chooseVideoWebm(page);
    await enterSource(page);
    assert.equal(downloadRequests(requests).length, 0);
    await page.locator("#continue-button").click();
    await page.locator("#error-state").waitFor({ state: "visible" });
    assert.equal(downloadRequests(requests).length, 1);
    assert.deepEqual(downloadRequests(requests)[0].body, { url: canonicalUrl, type: "video", format: "webm", quality: "720p" });
    assert.equal(downloadRequests(requests)[0].headers.authorization, undefined);
    assert.ok(downloadRequests(requests)[0].headers.cookie.includes(`${cookieName}=`));
    assert.equal(await page.locator("#error-description").evaluate((node, secret) => node.textContent.includes(secret), fixtureToken), false);
    assert.match(await page.locator("#error-description").textContent(), /access|token/i);
    await page.screenshot({ path: `${screenshotDir}/mobile-rejected-token.png`, fullPage: true });

    const geometry = await page.evaluate(() => ({ width: innerWidth, scrollWidth: document.documentElement.scrollWidth }));
    assert.ok(geometry.scrollWidth <= geometry.width, "narrow layout should not overflow horizontally");
    const source = await page.locator(".source-panel").boundingBox();
    const output = await page.locator(".result-panel").boundingBox();
    assert.ok(source && output && output.y > source.y, "narrow layout should stack source before output");
  } finally {
    await context.close();
  }
}

async function test320PixelReducedMotion(browser, origin) {
  const { context, page, requests } = await newPage(browser, origin, { width: 320, height: 800 }, { reducedMotion: "reduce", captureScroll: true, fixtureOptions: { creationStatus: 401 } });
  try {
    await chooseVideoWebm(page);
    await enterSource(page);
    await page.locator("#continue-button").click();
    await page.locator("#error-state").waitFor({ state: "visible" });
    const layout = await page.evaluate(() => ({ width: innerWidth, scrollWidth: document.documentElement.scrollWidth }));
    assert.ok(layout.scrollWidth <= layout.width, "320px layout should fit without horizontal scrolling");
    const source = await page.locator(".source-panel").boundingBox();
    const output = await page.locator(".result-panel").boundingBox();
    assert.ok(source && output && output.y > source.y, "320px layout should keep panels in a single column");
    const scrollCalls = await page.evaluate(() => window.__frameScrollCalls);
    assert.ok(scrollCalls.some((call) => call.target === "error-title" && call.options.behavior === "auto"), "reduced motion should use immediate result focus");
    const transition = await page.locator(".panel").first().evaluate((node) => getComputedStyle(node).transitionDuration);
    assert.match(transition, /0\.0*1ms|0\.00001s|1e-05s/);
    await page.screenshot({ path: `${screenshotDir}/mobile-320-reduced-motion.png`, fullPage: true });
    assert.equal(downloadRequests(requests).length, 1);
  } finally {
    await context.close();
  }
}

async function testSavedCookieReloadExportAndForget(browser, origin) {
  const { context, page, requests, consoleMessages } = await newPage(browser, origin, { width: 1280, height: 900 });
  try {
    const startup = sessionRequests(requests)[0];
    assert.ok(startup, "startup checks whether this browser has a saved session");
    assert.equal(startup.headers.authorization, undefined);
    assert.equal(startup.headers["x-frame-client"], "1");
    await page.locator("#session-message").waitFor({ state: "hidden" });
    assert.equal(await page.locator("#access-settings").evaluate((node) => node.hidden), false);
    assert.equal(await page.locator(".toast").count(), 0, "missing saved access is a quiet startup state");

    await page.locator("#access-settings summary").click();
    await page.locator("#api-token").fill(fixtureToken);
    await page.locator("#save-access-button").click();
    await page.waitForFunction(() => document.querySelector("#session-message").textContent === "Access saved");
    const saveRequest = sessionRequests(requests).find((entry) => entry.method === "POST");
    assert.ok(saveRequest);
    assert.equal(saveRequest.headers.authorization, `Bearer ${fixtureToken}`);
    assert.equal(saveRequest.body, null);
    assert.equal(saveRequest.headers["x-frame-client"], "1");
    const verifyRequest = sessionRequests(requests).filter((entry) => entry.method === "GET").at(-1);
    assert.equal(verifyRequest.headers.authorization, undefined, "save verification must prove cookie auth without a bearer");
    assert.ok(verifyRequest.headers.cookie.includes(`${cookieName}=`));
    assert.equal(await page.locator("#api-token").inputValue(), "");
    assert.equal(await page.locator("#access-settings").evaluate((node) => node.hidden), true);

    const savedCookie = (await context.cookies()).find((cookie) => cookie.name === cookieName);
    assert.ok(savedCookie, "browser accepted the session cookie");
    assert.equal(savedCookie.httpOnly, true);
    assert.equal(savedCookie.secure, true);
    assert.equal(savedCookie.sameSite, "Strict");
    assert.equal(savedCookie.path, "/api");
    assert.notEqual(savedCookie.value, fixtureToken);
    assert.ok(savedCookie.expires > Math.floor(Date.now() / 1000) + 300 * 86400);
    const browserStorage = await page.evaluate(() => ({
      cookie: document.cookie,
      values: Array.from({ length: localStorage.length }, (_, index) => localStorage.getItem(localStorage.key(index)))
    }));
    assert.equal(browserStorage.cookie, "", "HttpOnly cookie is not visible to page scripts");
    assert.ok(!browserStorage.values.includes(fixtureToken));

    await page.reload({ waitUntil: "networkidle" });
    await page.waitForFunction(() => document.querySelector("#session-message").textContent === "Access saved");
    assert.equal(await page.locator("#access-settings").evaluate((node) => node.hidden), true);
    assert.equal(await page.locator("#api-token").inputValue(), "");
    await chooseAudioMp3(page);
    await page.locator("#youtube-url").fill(fixtureUrl);
    await page.locator("#preview-button").click();
    await page.locator("#confirm-dialog").waitFor({ state: "visible" });
    assert.equal(downloadRequests(requests).length, 0, "saved session still respects source confirmation");
    await page.locator("#continue-button").click();
    await page.locator("#success-state").waitFor({ state: "visible", timeout: 12000 });
    const downloadCreate = downloadRequests(requests).find((entry) => entry.method === "POST");
    assert.deepEqual(downloadCreate.body, { url: canonicalUrl, type: "audio", format: "mp3", quality: "best", audio_bitrate_kbps: 192 });
    for (const entry of downloadRequests(requests)) {
      assert.equal(entry.headers.authorization, undefined, "saved access must not send a bearer header");
      assert.equal(entry.headers["x-frame-client"], "1");
      assert.ok(entry.headers.cookie.includes(`${cookieName}=`));
    }
    assert.ok(sessionRequests(requests).every((entry) => entry.headers["x-frame-client"] === "1"));
    assert.ok(consoleMessages.every((line) => !line.includes(fixtureToken)), "console output must not expose the API token");

    await page.locator("#forget-access-button").click();
    await page.locator("#access-settings").waitFor({ state: "visible" });
    const deleteRequest = sessionRequests(requests).find((entry) => entry.method === "DELETE");
    assert.ok(deleteRequest);
    assert.equal(deleteRequest.headers.authorization, undefined);
    assert.equal(deleteRequest.headers["x-frame-client"], "1");
    assert.ok(deleteRequest.headers.cookie.includes(`${cookieName}=`));
    assert.ok(!(await context.cookies()).some((cookie) => cookie.name === cookieName), "forget removes the cookie from the browser jar");
    assert.equal(await page.locator("#forget-access-button").isVisible(), false);
    assert.notEqual(await page.locator("#session-message").textContent(), "Access saved");
  } finally {
    await context.close();
  }
}

async function testInvalidAndDevelopmentSession(browser, origin) {
  const invalid = await newPage(browser, origin, { width: 1000, height: 800 }, { fixtureOptions: { sessionMode: "invalid" } });
  try {
    assert.notEqual(await invalid.page.locator("#session-message").textContent(), "Access saved");
    assert.equal(await invalid.page.locator("#forget-access-button").isVisible(), false);
    assert.equal(await invalid.page.locator(".toast").count(), 0, "invalid saved cookie is a quiet startup state");
  } finally {
    await invalid.context.close();
  }

  const development = await newPage(browser, origin, { width: 1000, height: 800 }, { fixtureOptions: { sessionMode: "development" } });
  try {
    assert.equal(await development.page.locator("#session-message").textContent(), "Access ready");
    assert.notEqual(await development.page.locator("#session-message").textContent(), "Access saved");
    assert.equal(await development.page.locator("#forget-access-button").isVisible(), false);
  } finally {
    await development.context.close();
  }
}

async function testBlockedCookieSave(browser, origin) {
  const { context, page, requests } = await newPage(browser, origin, { width: 1000, height: 800 }, { fixtureOptions: { rejectSaveCookie: true } });
  try {
    await page.locator("#access-settings summary").click();
    await page.locator("#api-token").fill(fixtureToken);
    await page.locator("#save-access-button").click();
    await page.locator("#token-error").waitFor({ state: "visible" });
    assert.match(await page.locator("#token-error").textContent(), /allow cookies/i);
    assert.equal(await page.locator("#api-token").inputValue(), fixtureToken);
    assert.notEqual(await page.locator("#session-message").textContent(), "Access saved");
    assert.equal(await page.locator("#forget-access-button").isVisible(), false);
    assert.equal((await context.cookies()).some((cookie) => cookie.name === cookieName), false);
    assert.equal(downloadRequests(requests).length, 0);
  } finally {
    await context.close();
  }
}

async function testExpiredCookieKeepsJobForReentry(browser, origin) {
  const { context, page, requests } = await newPage(browser, origin, { width: 1000, height: 800 }, {
    fixtureOptions: { sessionMode: "expire-on-poll", unauthorizedPolls: 1 }
  });
  try {
    await chooseVideoWebm(page);
    await page.locator("#youtube-url").fill(fixtureUrl);
    await page.locator("#preview-button").click();
    await page.locator("#confirm-dialog").waitFor({ state: "visible" });
    await page.locator("#continue-button").click();
    await page.locator("#error-state").waitFor({ state: "visible" });
    const expectedJobId = downloadRequests(requests).find((entry) => entry.method === "POST").headers["x-idempotency-key"];
    assert.ok(expectedJobId);
    assert.ok((await page.locator("#job-id").textContent()).includes(expectedJobId));
    assert.equal(await page.locator("#check-job-button").isVisible(), true);
    assert.match(await page.locator("#token-error").textContent(), /expired/i);

    await page.locator("#api-token").fill(refreshedToken);
    await page.locator("#save-access-button").click();
    await page.waitForFunction(() => document.querySelector("#session-message").textContent === "Access saved");
    const replacementSave = sessionRequests(requests).filter((entry) => entry.method === "POST").at(-1);
    assert.equal(replacementSave.headers.authorization, `Bearer ${refreshedToken}`);
    assert.equal(await page.locator("#api-token").inputValue(), "");
    await page.locator("#check-job-button").click();
    await page.locator("#success-state").waitFor({ state: "visible", timeout: 12000 });
    assert.equal(downloadRequests(requests).filter((entry) => entry.method === "POST").length, 1, "recovery must not submit a duplicate job");
    assert.ok(downloadRequests(requests).some((entry) => entry.path === `/api/v1/downloads/${expectedJobId}`));
    assert.ok(downloadRequests(requests).filter((entry) => entry.method !== "POST").every((entry) => entry.headers.authorization === undefined));
    assert.ok(downloadRequests(requests).every((entry) => entry.headers["x-frame-client"] === "1"));
    assert.ok(downloadRequests(requests).filter((entry) => entry.method !== "POST").every((entry) => entry.headers.cookie.includes(`${cookieName}=`)));
  } finally {
    await context.close();
  }
}

async function testConfirmationDismissalDuringPendingCookieSave(browser, origin, dismissal) {
  const sessionGates = { save: deferred(), verify: deferred() };
  const { context, page, requests } = await newPage(browser, origin, { width: 1000, height: 800 }, { fixtureOptions: { sessionGates } });
  try {
    await enterSource(page);
    const saveStarted = page.waitForRequest((request) => new URL(request.url()).pathname === "/api/v1/session" && request.method() === "POST");
    await page.locator("#continue-button").click();
    await saveStarted;
    if (dismissal === "back") await page.locator("#back-button").click();
    else await page.keyboard.press("Escape");
    await page.locator("#confirm-dialog").waitFor({ state: "hidden" });
    const verificationStarted = page.waitForRequest((request) => new URL(request.url()).pathname === "/api/v1/session" && request.method() === "GET" && request.headers().cookie?.includes(`${cookieName}=`));
    sessionGates.save.resolve();
    await verificationStarted;
    sessionGates.verify.resolve();
    await page.waitForFunction(() => document.querySelector("#session-message").textContent === "Access saved");
    assert.equal(downloadRequests(requests).some((entry) => entry.method === "POST"), false, "closed confirmation must not submit after auth verification resolves");
    assert.deepEqual(sessionRequests(requests).map((entry) => entry.method), ["GET", "POST", "GET"]);
  } finally {
    sessionGates.save.resolve();
    sessionGates.verify.resolve();
    await context.close();
  }
}

const { server, origin } = await startStaticServer();
let browser;
try {
  await mkdir(screenshotDir, { recursive: true });
  browser = await chromium.launch({ executablePath: process.env.CHROMIUM_PATH || "/usr/bin/chromium", args: ["--no-sandbox"] });
  await testSuccessfulKeyboardAudioWorkflow(browser, origin);
  await testCancellation(browser, origin);
  await testRejectedTokenAndNarrowLayout(browser, origin);
  await test320PixelReducedMotion(browser, origin);
  await testSavedCookieReloadExportAndForget(browser, origin);
  await testInvalidAndDevelopmentSession(browser, origin);
  await testBlockedCookieSave(browser, origin);
  await testExpiredCookieKeepsJobForReentry(browser, origin);
  await testConfirmationDismissalDuringPendingCookieSave(browser, origin, "back");
  await testConfirmationDismissalDuringPendingCookieSave(browser, origin, "escape");
  console.log("PASS Chromium Frame workflows: export, cancellation, cookie save/reload/forget, invalid and blocked cookies, 401 recovery, confirmation dismissal, keyboard, themes and responsive motion");
} finally {
  if (browser) await browser.close();
  await new Promise((resolve, reject) => server.close((error) => error ? reject(error) : resolve()));
}
