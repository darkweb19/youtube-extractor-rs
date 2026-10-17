import test from 'node:test';
import assert from 'node:assert/strict';
import { normalizeYouTubeUrl } from '../../web/url.mjs';

test('normalizes supported YouTube video URLs without carrying extra parameters', () => {
  for (const url of [
    ' https://www.youtube.com/watch?v=dQw4w9WgXcQ&t=42 ',
    'https://youtu.be/dQw4w9WgXcQ?si=tracking',
    'https://m.youtube.com/watch?v=dQw4w9WgXcQ',
    'https://youtube.com/shorts/dQw4w9WgXcQ',
    'https://youtube.com/live/dQw4w9WgXcQ',
  ]) {
    assert.deepEqual(normalizeYouTubeUrl(url), {
      id: 'dQw4w9WgXcQ', url: 'https://www.youtube.com/watch?v=dQw4w9WgXcQ',
    });
  }
});

test('rejects malformed, misleading and unsafe links before preview or backend requests', () => {
  for (const value of [
    '', 'dQw4w9WgXcQ', 'javascript:alert(1)', 'file:///watch?v=dQw4w9WgXcQ',
    'https://youtube.com.attacker.invalid/watch?v=dQw4w9WgXcQ',
    'https://youtube.com@attacker.invalid/watch?v=dQw4w9WgXcQ',
    'https://attacker@youtube.com/watch?v=dQw4w9WgXcQ',
    'https://youtube.com:444/watch?v=dQw4w9WgXcQ',
    'https://youtu.be/dQw4w9WgXcQ/extra', 'https://youtu.be/short',
    'https://youtube.com/playlist?list=PLexample', 'https://example.com/?v=dQw4w9WgXcQ',
    'https://youtube.com/watch?v=../../oops',
    'https://youtube.com/embed/dQw4w9WgXcQ',
    'https://youtu.be/dQw4w9WgXcQ/',
    'https://youtube.com/watch?v=dQw4w9WgXcQ&v=dQw4w9WgXcQ',
    'https://youtube.com/shorts/dQw4w9WgXcQ?v=AAAAAAAAAAA',
    'https://youtube.com/a/../watch?v=dQw4w9WgXcQ',
    'https://youtube.com/%77atch?v=dQw4w9WgXcQ',
  ]) assert.throws(() => normalizeYouTubeUrl(value), Error, value);
});
