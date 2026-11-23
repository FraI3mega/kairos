// Brave and Arc often look like Chrome from in here, so the app falls back to a "chrome" report.

const BRAND_FAMILY = [
  ['Microsoft Edge', 'edge'],
  ['Opera', 'opera'],
  ['Brave', 'brave'],
  ['Vivaldi', 'vivaldi'],
  ['Arc', 'arc'],
];

const UA_FAMILY = [
  [/\bEdg(e|A|iOS)?\//, 'edge'],
  [/\bOPR\//, 'opera'],
  [/\bVivaldi\//, 'vivaldi'],
  [/\bFirefox\//, 'firefox'],
];

// hasBrowserInfo is whether runtime.getBrowserInfo exists (Firefox only).
export function detectBrowser({ brands = [], userAgent = '', hasBrowserInfo = false } = {}) {
  if (hasBrowserInfo) return 'firefox';
  for (const [brand, family] of BRAND_FAMILY) {
    if (brands.some((b) => b?.brand === brand)) return family;
  }
  for (const [pattern, family] of UA_FAMILY) {
    if (pattern.test(userAgent)) return family;
  }
  return 'chrome';
}
