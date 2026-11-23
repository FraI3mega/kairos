import assert from 'node:assert/strict';
import { test } from 'node:test';

import { detectBrowser } from '../src/browser.js';
import { dashboardUrl, focusTime, pausedNote } from '../src/format.js';

test('browser families come from brands, then the user agent', () => {
  const brands = (...names) => names.map((brand) => ({ brand }));
  assert.equal(detectBrowser({ hasBrowserInfo: true, userAgent: 'Mozilla/5.0 Firefox/140.0' }), 'firefox');
  assert.equal(detectBrowser({ brands: brands('Not A;Brand', 'Chromium', 'Google Chrome') }), 'chrome');
  assert.equal(detectBrowser({ brands: brands('Chromium', 'Microsoft Edge') }), 'edge');
  assert.equal(detectBrowser({ brands: brands('Chromium', 'Opera') }), 'opera');
  assert.equal(detectBrowser({ brands: brands('Chromium', 'Brave') }), 'brave');
  assert.equal(detectBrowser({ brands: brands('Chromium', 'Vivaldi') }), 'vivaldi');
  assert.equal(detectBrowser({ userAgent: 'Mozilla/5.0 Chrome/130 Safari/537.36 Edg/130.0' }), 'edge');
  assert.equal(detectBrowser({ userAgent: 'Mozilla/5.0 Chrome/130 Safari/537.36 OPR/115.0' }), 'opera');
  assert.equal(detectBrowser({ userAgent: 'Mozilla/5.0 Chrome/130 Safari/537.36' }), 'chrome');
  assert.equal(detectBrowser({}), 'chrome');
  assert.equal(detectBrowser(), 'chrome');
});

test('focus time reads like the CLI', () => {
  assert.equal(focusTime(0), '0m');
  assert.equal(focusTime(29), '0m');
  assert.equal(focusTime(45 * 60), '45m');
  assert.equal(focusTime(3600), '1h 00m');
  assert.equal(focusTime(5000), '1h 23m');
  assert.equal(focusTime(-5), '0m');
  assert.equal(focusTime(undefined), '0m');
  assert.equal(focusTime('abc'), '0m');
});

test('the dashboard link uses the account server only when it is a web address', () => {
  assert.equal(dashboardUrl('https://kairos.example.org/'), 'https://kairos.example.org/me');
  assert.equal(dashboardUrl('http://localhost:8080'), 'http://localhost:8080/me');
  assert.equal(dashboardUrl('javascript:alert(1)'), 'https://kairos.hackclub.com/me');
  assert.equal(dashboardUrl(null), 'https://kairos.hackclub.com/me');
  assert.equal(dashboardUrl(undefined), 'https://kairos.hackclub.com/me');
});

test('the pause note only shows while a pause is running', () => {
  const now = Date.UTC(2026, 0, 1, 12, 0, 0);
  assert.equal(pausedNote(null, now), null);
  assert.equal(pausedNote(undefined, now), null);
  assert.equal(pausedNote(now / 1000 - 5, now), null);
  assert.match(pausedNote(now / 1000 + 600, now), /^Paused until /);
});
