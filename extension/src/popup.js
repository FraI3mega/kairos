import { HOST_PERMISSION, fetchStatus } from './api.js';
import { dashboardUrl, focusTime, pausedNote } from './format.js';

const ext = globalThis.browser ?? globalThis.chrome;
const $ = (id) => document.getElementById(id);

function show(id, text) {
  const el = $(id);
  el.hidden = text === null;
  if (text !== null && text !== undefined) el.textContent = text;
}

async function hasAccess() {
  try {
    return await ext.permissions.contains({ origins: [HOST_PERMISSION] });
  } catch {
    return true;
  }
}

function render(state, status) {
  const ok = state === 'connected';
  $('state').dataset.ok = String(ok);
  $('state').textContent = {
    connected: 'Connected to Kairos',
    offline: 'Kairos app not running',
    blocked: 'Access to the Kairos app is turned off',
  }[state];
  show('today', null);
  if (ok) {
    const today = $('today');
    today.hidden = false;
    today.textContent = focusTime(status.today_secs);
    const caption = document.createElement('small');
    caption.textContent = 'focus time today';
    today.append(caption);
  }
  show('note', ok ? pausedNote(status.paused_until) : null);
  if (state === 'offline') show('note', 'Start the Kairos app and this tab tracker reconnects by itself.');
  if (state === 'blocked') {
    show('note', 'The extension needs permission to reach the Kairos app on this computer. Nothing else is allowed.');
  }
  $('grant').hidden = state !== 'blocked';
  $('dashboard').href = dashboardUrl(status?.server);
}

async function refresh() {
  if (!(await hasAccess())) return render('blocked');
  try {
    render('connected', await fetchStatus());
  } catch {
    render('offline');
  }
}

$('grant').addEventListener('click', async () => {
  try {
    await ext.permissions.request({ origins: [HOST_PERMISSION] });
  } catch {
  }
  refresh();
});

refresh();
