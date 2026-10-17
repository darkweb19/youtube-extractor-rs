import test, { before, after } from 'node:test';
import assert from 'node:assert/strict';
import { createServer } from 'node:http';
import { readFile, mkdtemp, mkdir } from 'node:fs/promises';
import { execFileSync } from 'node:child_process';
import { tmpdir } from 'node:os';
import { join, resolve } from 'node:path';
import { chromium } from 'playwright';

// Controlled HTTP fixtures exercise the real frontend without YouTube/network access.
// Rust bridge authentication, ownership, validation and media leases have separate tests.
const root = resolve(import.meta.dirname, '../..');
const artifacts = process.env.FRONTEND_ARTIFACTS_DIR || '/tmp/frontend-artifacts';
const source = 'https://www.youtube.com/watch?v=dQw4w9WgXcQ';
const jobId = '11111111-1111-4111-8111-111111111111';
let browser, server, origin, mp4, state;

function reset(overrides = {}) {
  state = { creates: 0, polls: 0, requests: [], complete: false, createError: null,
    pollError: null, fileExpired: false, jobError: null, sessionError: null, ...overrides };
}
function json(response, status, body) {
  response.writeHead(status, { 'content-type': 'application/json' });
  response.end(JSON.stringify(body));
}
function job(status) {
  return { job_id: jobId, status, created_at: '2026-01-01T00:00:00Z', updated_at: '2026-01-01T00:00:01Z',
    progress: { downloaded_bytes: status === 'queued' ? 0 : status === 'running' ? Math.floor(mp4.length * .45) : mp4.length, total_bytes: mp4.length, percent: status === 'queued' ? 0 : status === 'running' ? 45 : 100 },
    output: status === 'completed' ? { filename: 'fixture-video.mp4', mime_type: 'video/mp4', size_bytes: mp4.length } : null,
    error: status === 'failed' ? state.jobError : null,
    download_url: status === 'completed' ? `/ui/downloads/${jobId}/file` : null,
    media_url: status === 'completed' ? `/ui/downloads/${jobId}/media` : null };
}
async function handler(request, response) {
  const pathname = new URL(request.url, origin).pathname;
  state.requests.push({ method: request.method, path: pathname, headers: request.headers });
  if (pathname === '/ui/session') {
    if (state.sessionError) return json(response, state.sessionError.status, { error: state.sessionError.error });
    response.setHeader('set-cookie', 'extractor_session=controlled-fixture; HttpOnly; SameSite=Strict; Path=/');
    return json(response, 200, { csrf_token: 'controlled-csrf-token' });
  }
  if (pathname === '/ui/downloads' && request.method === 'POST') {
    state.creates++;
    let body = ''; for await (const chunk of request) body += chunk;
    state.lastBody = JSON.parse(body);
    if (state.createError) return json(response, state.createError.status, { error: state.createError.error });
    if (state.abortCreate) {
      // Send response headers first so Chromium cannot transparently replay a
      // reused-connection POST after an early socket reset. Then interrupt its body.
      response.writeHead(202, { 'content-type': 'application/json', 'content-length': 100 });
      response.flushHeaders();
      response.write('{"job_id":');
      setTimeout(() => response.destroy(), 10);
      return;
    }
    return json(response, 202, job('queued'));
  }
  if (pathname === `/ui/downloads/${jobId}`) {
    if (request.method === 'DELETE') return json(response, 200, job('cancelled'));
    state.polls++;
    if (state.pollError) return json(response, state.pollError.status, { error: state.pollError.error });
    return json(response, 200, job(state.jobError ? 'failed' : state.complete ? 'completed' : 'running'));
  }
  if ([`/ui/downloads/${jobId}/file`, `/ui/downloads/${jobId}/media`].includes(pathname)) {
    if (state.fileExpired) return json(response, 404, { error: { code: 'NOT_FOUND', message: 'This generated file has expired. Generate it again.' } });
    if (state.badMedia && pathname.endsWith('/media')) {
      response.writeHead(200, { 'content-type': 'video/mp4' });
      return response.end('controlled invalid video');
    }
    let start = 0, end = mp4.length - 1;
    const range = request.headers.range?.match(/^bytes=(\d+)-(\d*)$/);
    if (range) { start = Number(range[1]); if (range[2]) end = Math.min(Number(range[2]), end); }
    const headers = { 'content-type': 'video/mp4', 'accept-ranges': 'bytes', 'content-length': end - start + 1 };
    if (range) headers['content-range'] = `bytes ${start}-${end}/${mp4.length}`;
    if (pathname.endsWith('/file')) headers['content-disposition'] = 'attachment; filename="fixture-video.mp4"';
    response.writeHead(range ? 206 : 200, headers);
    return response.end(request.method === 'HEAD' ? undefined : mp4.subarray(start, end + 1));
  }
  const files = { '/': ['web/index.html', 'text/html'], '/assets/app.js': ['web/app.js', 'text/javascript'],
    '/assets/url.mjs': ['web/url.mjs', 'text/javascript'], '/assets/styles.css': ['web/styles.css', 'text/css'] };
  if (pathname === '/favicon.ico') { response.writeHead(204); return response.end(); }
  const entry = files[pathname];
  if (!entry) { response.writeHead(404); return response.end(); }
  response.writeHead(200, { 'content-type': entry[1] });
  response.end(await readFile(join(root, entry[0])));
}

before(async () => {
  await mkdir(artifacts, { recursive: true });
  const temp = await mkdtemp(join(tmpdir(), 'extractor-browser-'));
  const fixture = join(temp, 'fixture.mp4');
  execFileSync('ffmpeg', ['-hide_banner', '-loglevel', 'error', '-f', 'lavfi', '-i', 'color=c=0x28584c:s=320x180:r=24',
    '-f', 'lavfi', '-i', 'sine=frequency=440:sample_rate=44100', '-t', '1', '-c:v', 'libx264', '-preset', 'ultrafast',
    '-pix_fmt', 'yuv420p', '-c:a', 'aac', '-movflags', '+faststart', fixture]);
  mp4 = await readFile(fixture);
  reset();
  server = createServer((request, response) => handler(request, response).catch(error => { response.destroy(error); }));
  await new Promise(resolve => server.listen(0, '127.0.0.1', resolve));
  origin = `http://127.0.0.1:${server.address().port}`;
  browser = await chromium.launch({ headless: true, executablePath: process.env.PLAYWRIGHT_CHROMIUM_EXECUTABLE_PATH || undefined,
    args: ['--no-sandbox'] });
});
after(async () => { await browser?.close(); await new Promise(resolve => server?.close(resolve)); });

async function setup(t, options = {}) {
  reset(options.fixture);
  const context = await browser.newContext({ viewport: { width: 1440, height: 1100 }, acceptDownloads: true, ...options.context });
  t.after(() => context.close());
  const page = await context.newPage();
  const errors = [];
  page.on('pageerror', error => errors.push(error.message));
  page.on('console', message => {
    if (message.type() === 'error' && !message.text().startsWith('Failed to load resource:')) errors.push(message.text());
  });
  t.after(() => assert.deepEqual(errors, [], 'No uncaught browser exceptions'));
  // A deterministic embedded-player transport: it never contacts YouTube.
  await context.route(/https:\/\/(?:www\.)?(?:youtube(?:-nocookie)?\.com|youtube\.com)\/.*/, route => {
    if (route.request().resourceType() === 'document') return route.fulfill({ contentType: 'text/html', body: '<!doctype html><title>Controlled YouTube preview</title><p>Preview fixture</p><script>addEventListener("message", event => { if (JSON.parse(event.data).event === "listening") parent.postMessage(JSON.stringify({event:"onReady"}), event.origin); });</script>' });
    return route.fulfill({ contentType: 'text/javascript', body: '' });
  });
  await page.goto(origin);
  return { page, context };
}
async function until(predicate, message, timeout = 10000) {
  const deadline = Date.now() + timeout;
  while (Date.now() < deadline) { if (await predicate()) return; await new Promise(resolve => setTimeout(resolve, 50)); }
  assert.fail(message);
}
async function preview(page) {
  await page.locator('#youtube-url').fill(source);
  await page.locator('#generate-button').click();
  await page.locator('#confirm-dialog[open]').waitFor();
}
async function generate(page) {
  await preview(page);
  await page.locator('#continue-button').click();
  await until(() => state.creates === 1, 'Continue should submit exactly one job');
}
async function complete(page) {
  state.complete = true;
  await page.locator('#result-panel:not([hidden])').waitFor({ timeout: 15000 });
}
async function toast(page, match) {
  await until(async () => match.test(await page.locator('#toast-region').innerText()), `Toast should match ${match}`);
}

test('validation rejects unsafe URLs with inline error and toast, before any job', async t => {
  const { page } = await setup(t);
  await page.locator('#youtube-url').fill('https://youtube.com.attacker.invalid/watch?v=dQw4w9WgXcQ');
  await page.locator('#generate-button').click();
  await toast(page, /YouTube video link/i);
  assert.match(await page.locator('#url-error').innerText(), /YouTube/i);
  assert.equal(await page.locator('#youtube-url').getAttribute('aria-invalid'), 'true');
  assert.equal(state.creates, 0);
  assert.equal(await page.locator('#confirm-dialog').getAttribute('open'), null);
});

test('without JavaScript the form remains disabled and explains the prerequisite', async t => {
  const { page } = await setup(t, { context: { javaScriptEnabled: false } });
  assert.equal(await page.locator('#generate-button').isDisabled(), true);
  assert.equal(await page.locator('noscript').isVisible(), true);
  assert.match(await page.locator('noscript').innerText(), /Enable JavaScript/i);
  assert.equal(state.creates, 0);
  assert.equal(state.requests.some(request => request.path.startsWith('/ui/')), false);
});

test('confirmation previews correct ID without generating; Escape restores focus and dialog traps keyboard', async t => {
  const { page } = await setup(t);
  await page.screenshot({ path: join(artifacts, 'desktop-initial.png'), fullPage: true });
  await preview(page);
  assert.equal(state.creates, 0);
  assert.equal(state.requests.some(request => request.path.startsWith('/ui/downloads')), false, 'First click does not request generation or job status');
  const iframe = page.locator('#preview-container iframe');
  await iframe.waitFor();
  assert.match(await iframe.getAttribute('src'), /\/embed\/dQw4w9WgXcQ(?:\?|$)/);
  await page.screenshot({ path: join(artifacts, 'desktop-preview.png'), fullPage: true });
  for (let i = 0; i < 8; i++) {
    await page.keyboard.press('Tab');
    assert.ok(await page.evaluate(() => document.querySelector('#confirm-dialog').contains(document.activeElement)), 'Focus stays in dialog');
  }
  await page.keyboard.press('Escape');
  await until(() => page.locator('#confirm-dialog').evaluate(el => !el.open), 'Escape closes preview');
  assert.equal(state.creates, 0);
  assert.ok(await page.locator('#generate-button').evaluate(el => el === document.activeElement));
});

test('Cancel closes the confirmation without creating a job and preview failures reach a toast', async t => {
  const { page } = await setup(t);
  await preview(page);
  const iframe = await page.locator('#preview-player').elementHandle();
  const frame = await iframe.contentFrame();
  await frame.evaluate(parentOrigin => parent.postMessage({ event: 'onError', info: 100 }, parentOrigin), origin);
  await toast(page, /video is unavailable/i);
  assert.equal(await page.locator('#preview-error').isVisible(), true);
  assert.ok(await page.locator('#toast-region').evaluate(el => document.querySelector('#confirm-dialog').contains(el)), 'Live toast is within the active modal');
  await page.locator('#cancel-preview').click();
  assert.equal(await page.locator('#confirm-dialog').getAttribute('open'), null);
  assert.equal(state.creates, 0);
  assert.equal(state.requests.some(request => request.path.startsWith('/ui/downloads')), false);
});

test('Continue generates once; progress, playable MP4, explicit download and no browser API token', async t => {
  const { page, context } = await setup(t);
  await preview(page);
  await page.locator('#continue-button').evaluate(button => { button.click(); button.click(); });
  await until(() => state.creates === 1, 'Continue creates a job');
  await page.locator('#job-panel:not([hidden])').waitFor();
  await until(() => page.locator('#job-progress').evaluate(el => el.value === 45), 'Running progress is rendered');
  await page.screenshot({ path: join(artifacts, 'desktop-generating.png'), fullPage: true });
  await complete(page);
  assert.equal(state.creates, 1);
  assert.equal(state.lastBody.url, source);
  assert.ok(state.requests.filter(r => r.method === 'POST').every(r => r.headers['x-csrf-token'] === 'controlled-csrf-token'));
  const video = page.locator('#result-video');
  await until(() => video.evaluate(el => el.readyState >= 2 && el.videoWidth === 320), 'Synthetic MP4 loads in the HTML5 player');
  await video.evaluate(async el => { el.muted = true; await el.play(); });
  await until(() => video.evaluate(el => el.currentTime > 0), 'HTML5 video actually plays');
  await video.evaluate(el => el.pause());
  await page.screenshot({ path: join(artifacts, 'desktop-success.png'), fullPage: true });
  const downloadPromise = page.waitForEvent('download');
  await page.locator('#download-button').click();
  const download = await downloadPromise;
  const saved = join(artifacts, 'downloaded-fixture.mp4');
  await download.saveAs(saved);
  assert.deepEqual(await readFile(saved), mp4, 'Download preserves actual MP4 bytes');
  const probe = JSON.parse(execFileSync('ffprobe', ['-v', 'error', '-show_entries', 'stream=codec_type', '-of', 'json', saved], { encoding: 'utf8' }));
  assert.ok(probe.streams.some(stream => stream.codec_type === 'video'));
  assert.ok(probe.streams.some(stream => stream.codec_type === 'audio'));
  assert.equal(state.requests.some(r => r.headers.authorization || r.path.startsWith('/api/v1')), false);
  assert.deepEqual(await page.evaluate(() => Object.keys(localStorage)), []);
  assert.ok((await context.cookies()).find(cookie => cookie.name === 'extractor_session')?.httpOnly);
  assert.doesNotMatch(await page.content(), /Bearer |API_TOKEN/);
});

for (const [status, code, message] of [
  [429, 'RATE_LIMITED', 'Too many requests. Try again shortly.'],
  [403, 'ACCESS_RESTRICTED', 'Public access to this video is restricted.'],
  [504, 'TIMEOUT', 'Generation timed out. Try again.'],
]) test(`backend ${status} reaches shared toast and persistent inline error`, async t => {
  const { page } = await setup(t, { fixture: { createError: { status, error: { code, message } } } });
  await generate(page);
  await toast(page, new RegExp(message.replace(/[.*+?^${}()|[\]\\]/g, '\\$&')));
  assert.ok((await page.locator('#inline-error').innerText()).includes(message));
  assert.equal(await page.locator('#generate-button').isEnabled(), true);
});

test('uncertain creation network failure is visible and never automatically resubmitted', async t => {
  const { page } = await setup(t, { fixture: { abortCreate: true } });
  await generate(page);
  await toast(page, /unreadable|connect|network|reach|failed|lost/i);
  assert.equal(await page.locator('#inline-error').isVisible(), true);
  assert.match(await page.locator('#inline-error').innerText(), /could not confirm whether generation started/i);
  await page.clock.install();
  await page.clock.fastForward(60000);
  assert.equal(state.creates, 1, 'An uncertain creation response is never automatically resubmitted');
});

test('polling error can resume the same job and expired output is shown as a toast', async t => {
  const { page } = await setup(t, { fixture: { pollError: { status: 503, error: { code: 'NETWORK', message: 'Status temporarily unavailable.' } } } });
  await generate(page);
  await toast(page, /Status temporarily unavailable/);
  state.pollError = null;
  state.complete = true;
  await page.locator('#retry-status').click({ timeout: 40000 });
  await page.locator('#result-panel:not([hidden])').waitFor({ timeout: 15000 });
  assert.equal(state.creates, 1, 'Retry resumes polling rather than submitting another job');
  state.fileExpired = true;
  await page.locator('#download-button').click();
  await toast(page, /expired|Generate.*again/i);
});

test('session expiry blocks new work and can be recovered without submitting another job', async t => {
  const { page } = await setup(t, { fixture: { pollError: { status: 401, error: { code: 'SESSION_EXPIRED', message: 'Your browser session expired.' } } } });
  await generate(page);
  await toast(page, /session expired/i);
  assert.equal(await page.locator('#generate-button').isEnabled(), false);
  assert.equal(await page.locator('#refresh-session').isVisible(), true);
  assert.equal(await page.locator('#retry-status').isVisible(), false);
  state.pollError = null;
  await page.locator('#refresh-session').click();
  await until(() => page.locator('#generate-button').isEnabled(), 'A refreshed session permits generation');
  assert.equal(state.creates, 1);
  assert.equal(await page.locator('#result-panel').isVisible(), false);
});

test('session initialization failures show a toast and explicit recovery', async t => {
  const { page } = await setup(t, { fixture: { sessionError: { status: 503, error: { code: 'UNAVAILABLE', message: 'Browser access is temporarily unavailable.' } } } });
  await toast(page, /Browser access is temporarily unavailable/);
  assert.equal(state.creates, 0);
  assert.equal(await page.locator('#generate-button').isEnabled(), false);
  state.sessionError = null;
  await page.locator('#refresh-session').click();
  await until(() => page.locator('#generate-button').isEnabled(), 'Session initialization retry succeeds');
});

test('generation cancellation sends one CSRF-protected DELETE and returns to the form', async t => {
  const { page } = await setup(t);
  await generate(page);
  await until(() => page.locator('#cancel-job').isEnabled(), 'Running job can be cancelled');
  await page.locator('#cancel-job').click();
  await toast(page, /Generation cancelled/);
  assert.equal(state.requests.filter(request => request.method === 'DELETE').length, 1);
  assert.equal(state.requests.find(request => request.method === 'DELETE').headers['x-csrf-token'], 'controlled-csrf-token');
  assert.equal(await page.locator('#generate-button').isEnabled(), true);
  assert.equal(await page.locator('#job-panel').isVisible(), false);
});

test('reload resumes the browser-owned job without a duplicate generation', async t => {
  const { page } = await setup(t);
  await generate(page);
  await until(() => state.polls > 0, 'Initial job status requested');
  state.complete = true;
  await page.reload();
  await page.locator('#result-panel:not([hidden])').waitFor({ timeout: 15000 });
  assert.equal(state.creates, 1);
  assert.ok(state.polls >= 2);
});

test('unplayable generated media reports a toast while retaining the explicit download option', async t => {
  const { page } = await setup(t, { fixture: { badMedia: true, complete: true } });
  await generate(page);
  await page.locator('#result-panel:not([hidden])').waitFor();
  await toast(page, /could not play/i);
  assert.equal(await page.locator('#download-button').isVisible(), true);
});

test('terminal extractor errors appear in toast and inline without a result', async t => {
  const { page } = await setup(t, { fixture: { jobError: { code: 'UNSUPPORTED_FORMAT', message: 'This video has no supported MP4 stream.' } } });
  await generate(page);
  await toast(page, /no supported MP4 stream/);
  assert.match(await page.locator('#inline-error').innerText(), /no supported MP4 stream/);
  assert.equal(await page.locator('#result-panel').isVisible(), false);
});

test('mobile, zoom and reduced motion remain usable without horizontal overflow', async t => {
  const { page } = await setup(t, { context: { viewport: { width: 375, height: 812 }, reducedMotion: 'reduce' } });
  assert.ok(await page.evaluate(() => document.documentElement.scrollWidth <= innerWidth + 1));
  await page.screenshot({ path: join(artifacts, 'mobile-initial.png'), fullPage: true });
  await preview(page);
  assert.ok(await page.locator('#confirm-dialog').evaluate(el => el.scrollWidth <= el.clientWidth + 1));
  await page.screenshot({ path: join(artifacts, 'mobile-preview.png'), fullPage: true });
  await page.locator('#continue-button').click();
  await complete(page);
  assert.ok(await page.evaluate(() => document.documentElement.scrollWidth <= innerWidth + 1));
  assert.ok(await page.locator('#result-heading').evaluate(el => {
    const bounds = el.getBoundingClientRect();
    return bounds.top >= 0 && bounds.bottom < innerHeight - 80;
  }), 'Completion brings the success heading into the mobile viewport above the toast');
  await page.screenshot({ path: join(artifacts, 'mobile-success.png'), fullPage: true });
  await page.setViewportSize({ width: 720, height: 900 });
  await page.evaluate(() => { document.documentElement.style.zoom = '2'; });
  assert.ok(await page.evaluate(() => document.documentElement.scrollWidth <= innerWidth + 1), '200% zoom reflows');
  assert.equal(await page.locator('#download-button').isVisible(), true);
  assert.ok(await page.evaluate(() => matchMedia('(prefers-reduced-motion: reduce)').matches));
  assert.equal(await page.locator('#generate-button').evaluate(el => getComputedStyle(el).transitionDuration), '0s', 'Reduced motion disables optional button transitions');
});
