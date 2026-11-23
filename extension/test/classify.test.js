import assert from 'node:assert/strict';
import { test } from 'node:test';

import { classify } from '../src/classify.js';

test('local addresses report their port', () => {
  assert.deepEqual(classify('http://localhost:3000/app'), { kind: 'local', port: 3000 });
  assert.deepEqual(classify('http://127.0.0.1:8080/'), { kind: 'local', port: 8080 });
  assert.deepEqual(classify('http://127.1.2.3:5173/x'), { kind: 'local', port: 5173 });
  assert.deepEqual(classify('http://[::1]:4000/'), { kind: 'local', port: 4000 });
  assert.deepEqual(classify('http://app.localhost:3000/'), { kind: 'local', port: 3000 });
  assert.deepEqual(classify('http://0.0.0.0:8000/'), { kind: 'local', port: 8000 });
  assert.deepEqual(classify('http://localhost/'), { kind: 'local', port: 80 });
  assert.deepEqual(classify('https://localhost/'), { kind: 'local', port: 443 });
  assert.deepEqual(classify('https://LOCALHOST:8443/'), { kind: 'local', port: 8443 });
});

test('file pages and browser pages have no port to look up and count as other', () => {
  assert.deepEqual(classify('file:///home/me/site/index.html'), { kind: 'other' });
  for (const url of ['chrome://extensions', 'about:blank', 'moz-extension://x/popup.html', 'data:text/html,hi', 'view-source:http://localhost:3000', 'ftp://localhost/']) {
    assert.deepEqual(classify(url), { kind: 'other' }, url);
  }
});

test('lookalikes of local hosts are not local', () => {
  for (const url of ['http://localhost.evil.com:3000/', 'http://evil.com/localhost', 'http://127.0.0.1.evil.com/', 'http://notlocalhost:3000/', 'http://128.0.0.1:3000/']) {
    assert.equal(classify(url).kind, 'other', url);
  }
});

test('every other website is other, whatever it is', () => {
  for (const url of [
    'https://docs.rs/serde', 'https://developer.mozilla.org/en-US/docs/Web/API', 'https://claude.ai/code/abc',
    'https://chatgpt.com/codex', 'https://mine.example.com/dashboard', 'https://octo.github.io/cool-app/',
  ]) {
    assert.deepEqual(classify(url), { kind: 'other' }, url);
  }
});

test('results never contain the url', () => {
  for (const url of ['https://example.com/secret?token=abc', 'http://localhost:3000/secret?token=abc']) {
    assert.ok(!JSON.stringify(classify(url)).includes('secret'), url);
  }
});

test('a broken url still classifies', () => {
  assert.deepEqual(classify(''), { kind: 'other' });
  assert.deepEqual(classify('not a url'), { kind: 'other' });
});
