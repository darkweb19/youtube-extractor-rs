import { normalizeYouTubeUrl } from './url.mjs';

const $ = (id) => document.getElementById(id);
const form = $('url-form');
const input = $('youtube-url');
const generate = $('generate-button');
const dialog = $('confirm-dialog');
const continueButton = $('continue-button');
const JOB_KEY = 'frame.current-job';
const REQUEST_TIMEOUT = 20000;
const POLL_WINDOW = 15 * 60 * 1000;
const ID_PATTERN = /^[0-9a-f]{8}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{12}$/i;
let csrfToken = null;
let selection = null;
let jobId = null;
let busy = false;
let flowController = null;
let previewTimer;
let previewListening;
let toastTimer;
let lastToast = '';
let dialogOpener = null;
let downloadBusy = false;
let downloadApproved = false;
let mediaTimer;
let composing = false;

class ApiError extends Error {
  constructor(message, status = 0, code = 'NETWORK') { super(message); this.status = status; this.code = code; }
}

function toast(message, tone = 'error') {
  const key = `${tone}:${message}`;
  if (key === lastToast && $('toast-region').hasChildNodes()) return;
  clearTimeout(toastTimer);
  lastToast = key;
  const notice = document.createElement('div');
  notice.className = 'toast';
  notice.dataset.tone = tone;
  const copy = document.createElement('p');
  copy.textContent = message;
  const dismiss = document.createElement('button');
  dismiss.type = 'button';
  dismiss.textContent = '×';
  dismiss.setAttribute('aria-label', 'Dismiss notification');
  dismiss.addEventListener('click', () => { region.replaceChildren(); lastToast = ''; });
  notice.append(copy, dismiss);
  // The native modal makes document siblings inert, including live regions.
  const region = $('toast-region');
  if (dialog.open) dialog.append(region);
  else document.body.append(region);
  region.replaceChildren(notice);
  if (tone !== 'error') {
    const expire = () => { toastTimer = setTimeout(() => { region.replaceChildren(); lastToast = ''; }, 6000); };
    notice.addEventListener('mouseenter', () => clearTimeout(toastTimer));
    notice.addEventListener('mouseleave', expire);
    notice.addEventListener('focusin', () => clearTimeout(toastTimer));
    notice.addEventListener('focusout', expire);
    expire();
  }
}

function clearError() {
  $('inline-error').hidden = true;
  $('error-message').textContent = '';
  $('retry-status').hidden = true;
  $('refresh-session').hidden = true;
}

function showError(message, { retry = false, session = false } = {}) {
  $('error-message').textContent = message;
  $('inline-error').hidden = false;
  $('retry-status').hidden = !retry;
  $('retry-status').disabled = !retry;
  $('refresh-session').hidden = !session;
  toast(message);
}

function setBusy(value) {
  busy = value;
  generate.disabled = value || !csrfToken;
  input.disabled = value;
  form.setAttribute('aria-busy', String(value));
  generate.setAttribute('aria-busy', String(value));
}

async function api(path, { method = 'GET', body, signal } = {}) {
  const controller = new AbortController();
  let timedOut = false;
  const abort = () => controller.abort();
  if (signal?.aborted) controller.abort();
  signal?.addEventListener('abort', abort, { once: true });
  const timer = setTimeout(() => { timedOut = true; controller.abort(); }, REQUEST_TIMEOUT);
  const headers = { Accept: 'application/json' };
  if (body !== undefined) headers['Content-Type'] = 'application/json';
  if (!['GET', 'HEAD'].includes(method) && csrfToken) headers['X-CSRF-Token'] = csrfToken;
  try {
    const response = await fetch(path, { method, headers, credentials: 'same-origin', cache: 'no-store', body: body === undefined ? undefined : JSON.stringify(body), signal: controller.signal });
    if (!response.ok) {
      let payload;
      if (method !== 'HEAD') { try { payload = await response.json(); } catch { /* A gateway may return a non-JSON error. */ } }
      const fallback = response.status === 401 ? 'Your browser session expired. Start a new session to generate again.'
        : response.status === 404 ? 'This video file is no longer available. Generate the video again.'
          : response.status === 429 ? 'Too many requests. Wait a moment, then try again.'
            : `The request could not be completed (HTTP ${response.status}). Try again shortly.`;
      throw new ApiError(typeof payload?.error?.message === 'string' ? payload.error.message : fallback, response.status, payload?.error?.code);
    }
    if (method === 'HEAD' || response.status === 204) return null;
    try { return await response.json(); } catch { throw new ApiError('The server returned an unreadable response. Try again shortly.', 502, 'INVALID_RESPONSE'); }
  } catch (error) {
    if (signal?.aborted) throw error;
    if (timedOut) throw new ApiError('The server took too long to respond. Check your connection and try again.', 504, 'TIMEOUT');
    if (error instanceof ApiError) throw error;
    throw new ApiError('Could not reach the server. Check your connection and try again.');
  } finally {
    clearTimeout(timer);
    signal?.removeEventListener('abort', abort);
  }
}

function storeJob(value) {
  try { if (value) sessionStorage.setItem(JOB_KEY, value); else sessionStorage.removeItem(JOB_KEY); } catch { /* Storage is optional. */ }
}
function savedJob() { try { return sessionStorage.getItem(JOB_KEY); } catch { return null; } }
function stopFlow() { flowController?.abort(); flowController = null; }
function beginFlow() { stopFlow(); flowController = new AbortController(); return flowController.signal; }
function delay(ms, signal) {
  return new Promise((resolve, reject) => {
    if (signal.aborted) { reject(new DOMException('Aborted', 'AbortError')); return; }
    const abort = () => { clearTimeout(timer); reject(new DOMException('Aborted', 'AbortError')); };
    const timer = setTimeout(() => { signal.removeEventListener('abort', abort); resolve(); }, ms);
    signal.addEventListener('abort', abort, { once: true });
  });
}

function removePreview() {
  clearTimeout(previewTimer);
  clearInterval(previewListening);
  $('preview-container').replaceChildren();
}
function previewError(message) {
  if (!dialog.open) return;
  clearTimeout(previewTimer);
  $('preview-error-message').textContent = message;
  $('preview-error').hidden = false;
  toast(message);
}
function closePreview() {
  if (dialog.open) dialog.close();
}
function openPreview(video) {
  removePreview();
  for (const id of ['close-preview', 'cancel-preview', 'continue-button']) $(id).disabled = false;
  $('preview-error').hidden = true;
  $('preview-id').textContent = `VIDEO / ${video.id}`;
  $('watch-link').href = video.url;
  const frame = document.createElement('iframe');
  frame.id = 'preview-player';
  frame.title = 'YouTube video preview';
  frame.allow = 'accelerometer; autoplay; clipboard-write; encrypted-media; gyroscope; picture-in-picture; web-share';
  frame.allowFullscreen = true;
  frame.referrerPolicy = 'strict-origin-when-cross-origin';
  const source = new URL(`https://www.youtube-nocookie.com/embed/${video.id}`);
  source.searchParams.set('enablejsapi', '1');
  source.searchParams.set('origin', window.location.origin);
  source.searchParams.set('rel', '0');
  frame.src = source.href;
  frame.addEventListener('error', () => previewError('The YouTube preview could not load. Open it on YouTube to check the video before continuing.'));
  frame.addEventListener('load', () => {
    const listen = () => {
      frame.contentWindow?.postMessage(JSON.stringify({ event: 'listening', id: 'preview-player' }), 'https://www.youtube-nocookie.com');
      for (const event of ['onReady', 'onError']) frame.contentWindow?.postMessage(JSON.stringify({ event: 'command', func: 'addEventListener', args: [event] }), 'https://www.youtube-nocookie.com');
    };
    listen();
    previewListening = setInterval(listen, 1000);
  });
  $('preview-container').append(frame);
  dialogOpener = document.activeElement;
  dialog.showModal();
  $('cancel-preview').focus();
  previewTimer = setTimeout(() => previewError('The YouTube preview is taking too long to load. Open it on YouTube to check the video before continuing.'), 20000);
}

window.addEventListener('message', (event) => {
  const frame = $('preview-player');
  if (!dialog.open || !frame || event.source !== frame.contentWindow || event.origin !== 'https://www.youtube-nocookie.com') return;
  let data = event.data;
  if (typeof data === 'string') { try { data = JSON.parse(data); } catch { return; } }
  if (!data || typeof data !== 'object') return;
  if (['onReady', 'initialDelivery', 'infoDelivery'].includes(data.event)) {
    clearTimeout(previewTimer);
    clearInterval(previewListening);
    frame.contentWindow?.postMessage(JSON.stringify({ event: 'command', func: 'addEventListener', args: ['onError'] }), event.origin);
  }
  if (data.event === 'onError') {
    const messages = { 2: 'YouTube could not find this video.', 5: 'YouTube could not play this video.', 100: 'This YouTube video is unavailable.', 101: 'The owner does not allow this video to play here.', 150: 'The owner does not allow this video to play here.', 153: 'YouTube could not verify this preview.' };
    previewError(`${messages[data.info] || 'The YouTube preview could not play.'} Open it on YouTube to check the video before continuing.`);
  }
});

dialog.addEventListener('close', () => {
  removePreview();
  document.body.append($('toast-region'));
  if (dialogOpener && !dialogOpener.disabled) dialogOpener.focus();
  else if (busy) { $('job-heading').tabIndex = -1; $('job-heading').focus(); }
});
dialog.addEventListener('keydown', event => {
  if (event.key !== 'Tab') return;
  const targets = [...dialog.querySelectorAll('button:not(:disabled), a[href], iframe, [tabindex="0"]')].filter(el => el.getClientRects().length);
  const first = targets[0], last = targets.at(-1);
  if (event.shiftKey && document.activeElement === first) { event.preventDefault(); last.focus(); }
  else if (!event.shiftKey && document.activeElement === last) { event.preventDefault(); first.focus(); }
});
$('cancel-preview').addEventListener('click', closePreview);
$('close-preview').addEventListener('click', closePreview);

form.addEventListener('submit', (event) => {
  event.preventDefault();
  if (composing || event.isComposing || busy || dialog.open || !csrfToken) return;
  clearError();
  $('url-error').textContent = '';
  input.removeAttribute('aria-invalid');
  try {
    selection = normalizeYouTubeUrl(input.value);
    openPreview(selection);
  } catch (error) {
    input.setAttribute('aria-invalid', 'true');
    $('url-error').textContent = error.message;
    input.focus();
    toast(error.message);
  }
});
input.addEventListener('input', () => { input.removeAttribute('aria-invalid'); $('url-error').textContent = ''; });
input.addEventListener('compositionstart', () => { composing = true; });
input.addEventListener('compositionend', () => { composing = false; });
input.addEventListener('keydown', event => { if (event.key === 'Enter' && (composing || event.isComposing)) event.preventDefault(); });

function validJob(value, expectedId) {
  if (!value || !ID_PATTERN.test(value.job_id) || (expectedId && value.job_id !== expectedId) || !['queued', 'running', 'completed', 'failed', 'cancelled'].includes(value.status)) {
    throw new ApiError('The server returned an invalid generation status. Check status again shortly.', 502, 'INVALID_RESPONSE');
  }
  return value;
}
function clearMedia() {
  clearTimeout(mediaTimer);
  const video = $('result-video');
  video.pause();
  video.removeAttribute('src');
  video.load();
  $('result-panel').hidden = true;
  $('download-button').removeAttribute('href');
}
function showJob(job) {
  $('cancel-job').disabled = false;
  $('empty-state').hidden = true;
  $('job-panel').hidden = false;
  $('job-phase').textContent = job.status === 'queued' ? 'Queued' : 'Working';
  $('job-heading').textContent = job.status === 'queued' ? 'Waiting to generate' : 'Preparing your MP4';
  const percent = job.progress?.percent;
  if (typeof percent === 'number' && Number.isFinite(percent)) $('job-progress').value = Math.max(0, Math.min(100, percent));
  else $('job-progress').removeAttribute('value');
  $('job-status').textContent = job.status === 'queued' ? 'Your video is in the queue. This page will update automatically.'
    : typeof percent === 'number' && Number.isFinite(percent) ? `${Math.round(percent)}% downloaded · Preparing the video file…`
      : 'Generating your video. This can take a few minutes.';
  document.title = 'Generating MP4 — Frame';
}
function completeJob(job) {
  $('job-panel').hidden = true;
  $('result-panel').hidden = false;
  $('empty-state').hidden = true;
  const video = $('result-video');
  video.src = `/ui/downloads/${job.job_id}/media`;
  video.load();
  $('download-button').href = `/ui/downloads/${job.job_id}/file`;
  const size = job.output?.size_bytes;
  $('file-details').textContent = typeof size === 'number' ? `MP4 video · ${new Intl.NumberFormat('en', { maximumFractionDigits: 1 }).format(size / 1048576)} MB` : 'MP4 video';
  document.title = 'Your MP4 is ready — Frame';
  setBusy(false);
  toast('Your MP4 is ready. Preview it below or download your file.', 'success');
  $('result-panel').scrollIntoView({ block: 'start', behavior: 'auto' });
  $('result-heading').tabIndex = -1;
  $('result-heading').focus({ preventScroll: true });
}
function terminalError(job) {
  $('job-panel').hidden = true;
  $('empty-state').hidden = false;
  storeJob(null);
  setBusy(false);
  const message = job.status === 'cancelled' ? 'Generation was cancelled. You can choose a video and try again.' : `${job.error?.message || 'The video could not be generated.'} Check the link or try another video.`;
  showError(message);
  $('error-message').tabIndex = -1;
  $('error-message').focus({ preventScroll: true });
  document.title = 'Generation stopped — Frame';
}
function handleFlowError(error, { knownJob = false } = {}) {
  const expired = error.status === 401;
  if (expired) { csrfToken = null; jobId = null; storeJob(null); clearMedia(); $('session-status').textContent = 'Start a new session to reconnect.'; }
  setBusy(false);
  if ($('job-panel').hidden === false) $('job-status').textContent = 'Status updates paused.';
  $('cancel-job').disabled = true;
  showError(error.message, { retry: knownJob && ![401, 403, 404].includes(error.status), session: expired });
  $('error-message').tabIndex = -1;
  $('error-message').focus({ preventScroll: true });
  document.title = 'Check generation — Frame';
}

async function poll(id, signal) {
  const deadline = Date.now() + POLL_WINDOW;
  let failures = 0;
  while (!signal.aborted && id === jobId) {
    if (Date.now() > deadline) {
      setBusy(false);
      $('job-status').textContent = 'Automatic updates paused. Generation may still be running.';
      showError('Generation is taking longer than expected. Check its status again; a new generation has not been started.', { retry: true });
      return;
    }
    try {
      const job = validJob(await api(`/ui/downloads/${id}`, { signal }), id);
      if (signal.aborted || id !== jobId) return;
      failures = 0;
      if (job.status === 'completed') { completeJob(job); return; }
      if (['failed', 'cancelled'].includes(job.status)) { terminalError(job); return; }
      showJob(job);
      $('cancel-job').disabled = false;
      await delay(job.status === 'queued' ? 8000 : 5000, signal);
    } catch (error) {
      if (signal.aborted) return;
      const transient = !error.status || error.status === 429 || error.status >= 500;
      if (transient && failures < 3) {
        failures += 1;
        const message = `${error.message} Retrying status check (${failures}/3).`;
        $('job-status').textContent = 'Connection interrupted. Retrying status updates…';
        showError(message, { retry: true });
        try { await delay(5000 * failures, signal); } catch { return; }
        if (!signal.aborted) clearError();
      } else { handleFlowError(error, { knownJob: true }); return; }
    }
  }
}

continueButton.addEventListener('click', async () => {
  if (!selection || busy || !dialog.open) return;
  const video = selection;
  setBusy(true);
  continueButton.disabled = true;
  closePreview();
  clearError();
  clearMedia();
  storeJob(null);
  jobId = null;
  const signal = beginFlow();
  $('empty-state').hidden = true;
  $('job-panel').hidden = false;
  $('job-heading').textContent = 'Starting generation';
  $('job-phase').textContent = 'Starting';
  $('job-status').textContent = 'Sending your confirmed video…';
  $('job-progress').removeAttribute('value');
  $('cancel-job').disabled = true;
  try {
    const job = validJob(await api('/ui/downloads', { method: 'POST', body: { url: video.url, type: 'video', format: 'mp4', quality: '720p' }, signal }));
    if (signal.aborted) return;
    jobId = job.job_id;
    storeJob(jobId);
    if (job.status === 'completed') completeJob(job);
    else if (['failed', 'cancelled'].includes(job.status)) terminalError(job);
    else { showJob(job); await poll(jobId, signal); }
  } catch (error) {
    if (signal.aborted) return;
    $('job-panel').hidden = true;
    $('empty-state').hidden = false;
    if (!error.status || error.status >= 500) error.message += ' We could not confirm whether generation started. It may still be running; wait before submitting again.';
    handleFlowError(error);
  } finally { continueButton.disabled = false; }
});

$('retry-status').addEventListener('click', () => {
  if (!jobId || $('retry-status').hidden) return;
  clearError();
  setBusy(true);
  $('job-panel').hidden = false;
  $('cancel-job').disabled = false;
  void poll(jobId, beginFlow());
});

$('cancel-job').addEventListener('click', async () => {
  if (!jobId || $('cancel-job').disabled) return;
  stopFlow();
  $('cancel-job').disabled = true;
  try {
    await api(`/ui/downloads/${jobId}`, { method: 'DELETE' });
    storeJob(null);
    jobId = null;
    setBusy(false);
    $('job-panel').hidden = true;
    $('empty-state').hidden = false;
    clearError();
    toast('Generation cancelled.', 'info');
    input.focus({ preventScroll: true });
    document.title = 'Frame — YouTube to MP4';
  } catch (error) { handleFlowError(error, { knownJob: true }); }
});

$('download-button').addEventListener('click', async (event) => {
  if (downloadApproved) { downloadApproved = false; return; }
  event.preventDefault();
  if (downloadBusy || !jobId) return;
  const id = jobId;
  downloadBusy = true;
  const link = $('download-button');
  link.setAttribute('aria-disabled', 'true');
  try {
    await api(`/ui/downloads/${id}/file`, { method: 'HEAD' });
    if (jobId !== id) return;
    clearError();
    downloadApproved = true;
    link.click();
    toast('Download requested. Your browser will save the MP4 file.', 'info');
  } catch (error) { handleFlowError(error); }
  finally { downloadBusy = false; link.removeAttribute('aria-disabled'); }
});

const video = $('result-video');
video.addEventListener('error', () => {
  if (!video.getAttribute('src')) return;
  clearTimeout(mediaTimer);
  showError('The generated video could not play. Try Download MP4; if the file has expired, generate it again.');
});
video.addEventListener('stalled', () => {
  clearTimeout(mediaTimer);
  mediaTimer = setTimeout(() => { if (video.getAttribute('src')) showError('Video playback stopped loading. Check your connection or use Download MP4.'); }, 15000);
});
video.addEventListener('waiting', () => {
  clearTimeout(mediaTimer);
  mediaTimer = setTimeout(() => { if (video.getAttribute('src')) showError('Video playback is taking too long. Check your connection or use Download MP4.'); }, 15000);
});
for (const event of ['loadeddata', 'playing', 'canplay', 'pause', 'emptied']) video.addEventListener(event, () => clearTimeout(mediaTimer));

async function initializeSession({ restore = true } = {}) {
  generate.disabled = true;
  $('session-status').textContent = 'Connecting to the converter…';
  $('refresh-session').disabled = true;
  try {
    const session = await api('/ui/session');
    if (typeof session.csrf_token !== 'string' || !session.csrf_token) throw new ApiError('Could not initialize a browser session. Try again.');
    csrfToken = session.csrf_token;
    $('session-status').textContent = '';
    clearError();
    setBusy(false);
    const saved = restore ? savedJob() : null;
    if (saved && ID_PATTERN.test(saved)) {
      jobId = saved;
      setBusy(true);
      $('job-panel').hidden = false;
      $('empty-state').hidden = true;
      $('job-status').textContent = 'Checking your previous generation…';
      void poll(jobId, beginFlow());
    }
  } catch (error) {
    csrfToken = null;
    $('session-status').textContent = 'The converter could not connect.';
    setBusy(false);
    showError(`${error.message} Start a new session to try again.`, { session: true });
  } finally { $('refresh-session').disabled = false; }
}
$('refresh-session').addEventListener('click', () => {
  stopFlow();
  jobId = null;
  storeJob(null);
  clearMedia();
  $('job-panel').hidden = true;
  $('empty-state').hidden = false;
  void initializeSession({ restore: false });
});
window.addEventListener('offline', () => showError('You appear to be offline. Check your connection. Any accepted generation may still be running.'));
window.addEventListener('pagehide', () => { stopFlow(); removePreview(); clearTimeout(mediaTimer); });
window.addEventListener('pageshow', event => { if (event.persisted) { closePreview(); void initializeSession(); } });
void initializeSession();
