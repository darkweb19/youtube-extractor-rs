const VIDEO_ID = /^[A-Za-z0-9_-]{11}$/;
const YOUTUBE_HOSTS = new Set(['youtube.com', 'www.youtube.com', 'm.youtube.com']);

/** Parse only supported public YouTube URL shapes; never reuse user input as a frame URL. */
export function normalizeYouTubeUrl(value) {
  const input = String(value ?? '').trim();
  if (!input) throw new Error('Paste a YouTube link to get started.');
  if (input.length > 2048 || /[\u0000-\u001f\u007f\\]/.test(input)) {
    throw new Error('Use a standard YouTube video link without encoded or extra path segments.');
  }
  let url;
  try { url = new URL(input); } catch { throw new Error('Enter a full YouTube link, starting with https://.'); }
  if (!['https:', 'http:'].includes(url.protocol) || url.username || url.password || url.port) {
    throw new Error('Use a standard https:// YouTube video link.');
  }
  // URL parsers normalize dot segments. Inspect the original path before accepting it.
  const raw = input.match(/^https?:\/\/([^/?#]*)([^?#]*)/i);
  if (!raw || raw[1].includes('@') || raw[2].includes('%') || raw[2].split('/').some(part => part === '.' || part === '..')) {
    throw new Error('Use a standard YouTube video link without encoded or extra path segments.');
  }
  const identifiers = url.searchParams.getAll('v');
  let id;
  if (url.hostname === 'youtu.be') {
    if (!identifiers.length && /^\/[A-Za-z0-9_-]{11}$/.test(url.pathname)) id = url.pathname.slice(1);
  } else if (YOUTUBE_HOSTS.has(url.hostname)) {
    if (url.pathname === '/watch' && identifiers.length === 1) [id] = identifiers;
    else if (!identifiers.length) {
      const match = url.pathname.match(/^\/(?:shorts|live)\/([A-Za-z0-9_-]{11})$/);
      if (match) id = match[1];
    }
  }
  if (!id || !VIDEO_ID.test(id)) throw new Error('Use a YouTube video link, such as youtube.com/watch?v=… or youtu.be/….');
  return { id, url: `https://www.youtube.com/watch?v=${id}` };
}
