import { sendTab } from './api.js';
import { detectBrowser } from './browser.js';
import { classify } from './classify.js';

const ext = globalThis.browser ?? globalThis.chrome;
const REPORT_ALARM = 'report';

const family = detectBrowser({
  brands: globalThis.navigator?.userAgentData?.brands,
  userAgent: globalThis.navigator?.userAgent,
  hasBrowserInfo: typeof ext.runtime.getBrowserInfo === 'function',
});

async function reportActiveTab() {
  const win = await ext.windows.getLastFocused();
  if (!win?.focused) return;
  const [tab] = await ext.tabs.query({ active: true, windowId: win.id });
  if (!tab?.url) return;
  const result = tab.incognito ? { kind: 'other' } : classify(tab.url);
  await sendTab({ browser: family, ...result });
}

let running = false;
let queued = false;

async function report() {
  if (running) {
    queued = true;
    return;
  }
  running = true;
  try {
    do {
      queued = false;
      try {
        await reportActiveTab();
      } catch {
        // The app is not running or the tab went away. The next event retries.
      }
    } while (queued);
  } finally {
    running = false;
  }
}

async function ensureAlarm(name, periodInMinutes) {
  if (!(await ext.alarms.get(name))) ext.alarms.create(name, { periodInMinutes });
}

ext.tabs.onActivated.addListener(report);
ext.tabs.onUpdated.addListener((_id, change, tab) => {
  if (tab.active && (change.url || change.status === 'complete')) report();
});
ext.windows.onFocusChanged.addListener((windowId) => {
  if (windowId !== ext.windows.WINDOW_ID_NONE) report();
});
ext.alarms.onAlarm.addListener((alarm) => {
  if (alarm.name === REPORT_ALARM) report();
});

ensureAlarm(REPORT_ALARM, 1);
report();
