const DEFAULT_SERVER = 'https://kairos.obayy.dev';

export function focusTime(secs) {
  const minutes = Math.round(Math.max(0, Number(secs) || 0) / 60);
  const h = Math.floor(minutes / 60);
  const m = minutes % 60;
  return h === 0 ? `${m}m` : `${h}h ${String(m).padStart(2, '0')}m`;
}

export function dashboardUrl(server) {
  let base = DEFAULT_SERVER;
  try {
    const url = new URL(server);
    if (url.protocol === 'https:' || url.protocol === 'http:') base = url.origin;
  } catch {
  }
  return `${base}/me`;
}

export function pausedNote(pausedUntil, now = Date.now()) {
  if (!pausedUntil || pausedUntil * 1000 <= now) return null;
  const at = new Date(pausedUntil * 1000);
  const time = at.toLocaleTimeString([], { hour: '2-digit', minute: '2-digit' });
  return `Paused until ${time}`;
}
