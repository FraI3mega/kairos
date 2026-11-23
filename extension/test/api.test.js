import assert from 'node:assert/strict';
import { afterEach, test } from 'node:test';

import { sendTab } from '../src/api.js';

const realFetch = globalThis.fetch;
afterEach(() => {
  globalThis.fetch = realFetch;
});

function stub(reply, status = 200) {
  const calls = [];
  globalThis.fetch = async (url, init) => {
    calls.push({ url, init });
    return { ok: status < 400, status, json: async () => reply };
  };
  return calls;
}

test('tab reports carry only the kind and what that kind needs', async () => {
  const calls = stub({});
  await sendTab({ browser: 'firefox', kind: 'other', port: 3000, host: 'https://secret.example/path' });
  await sendTab({ browser: 'chrome', kind: 'local', port: 3000, host: 'x.com' });
  const bodies = calls.map((c) => JSON.parse(c.init.body));
  assert.deepEqual(bodies, [
    { browser: 'firefox', kind: 'other' },
    { browser: 'chrome', kind: 'local', port: 3000 },
  ]);
  for (const c of calls) {
    assert.equal(c.url, 'http://127.0.0.1:24816/v1/browser/tab');
    assert.equal(c.init.method, 'POST');
    assert.equal(c.init.headers['Content-Type'], 'application/json');
  }
});

test('error replies reject', async () => {
  stub({ error: 'nope' }, 403);
  await assert.rejects(sendTab({ browser: 'chrome', kind: 'other' }));
});
