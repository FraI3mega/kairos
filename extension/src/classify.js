// Only the returned `{kind, port?}` is ever sent anywhere, never the URL.

const DEFAULT_PORT = { 'http:': 80, 'https:': 443 };

function isLocalHost(hostname) {
  return (
    hostname === 'localhost' ||
    hostname.endsWith('.localhost') ||
    hostname === '[::1]' ||
    hostname === '0.0.0.0' ||
    /^127(\.\d{1,3}){3}$/.test(hostname)
  );
}

export function classify(rawUrl) {
  let url;
  try {
    url = new URL(rawUrl);
  } catch {
    return { kind: 'other' };
  }
  if (url.protocol !== 'http:' && url.protocol !== 'https:') return { kind: 'other' };
  if (!isLocalHost(url.hostname.toLowerCase())) return { kind: 'other' };
  const port = url.port ? Number(url.port) : DEFAULT_PORT[url.protocol];
  return { kind: 'local', port };
}
