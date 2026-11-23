// Nothing here ever carries a URL, title or page content.

export const BASE = 'http://127.0.0.1:24816';
export const HOST_PERMISSION = `${BASE}/*`;
const TIMEOUT_MS = 3000;

async function request(path, init) {
  const res = await fetch(`${BASE}${path}`, { ...init, signal: AbortSignal.timeout(TIMEOUT_MS) });
  if (!res.ok) throw new Error(`kairos api ${res.status}`);
  return res.json();
}

export function sendTab({ browser, kind, port }) {
  const body = { browser, kind };
  if (kind === 'local') body.port = port;
  return request('/v1/browser/tab', {
    method: 'POST',
    headers: { 'Content-Type': 'application/json' },
    body: JSON.stringify(body),
  });
}

export function fetchStatus() {
  return request('/v1/status');
}
