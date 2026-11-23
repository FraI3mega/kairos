(() => {
  function dashboard(box, snapEl) {
    let snapshot;
    try {
      snapshot = JSON.parse(snapEl.textContent);
    } catch {
      return;
    }

    const COLORS = {
      typing: '#ffd166',
      reading: '#6ee7c8',
      editing: '#ff8fab',
      testing: '#8fd3ff',
      terminal: '#c3c8f5',
    };
    const LABELS = {
      typing: 'typing a prompt',
      reading: 'reading a reply',
      editing: 'in your editor',
      testing: 'testing',
      terminal: 'in the terminal',
    };
    const SVG_NS = 'http://www.w3.org/2000/svg';
    const svg = document.getElementById('dial');
    const timeEl = document.getElementById('dial-b');
    const captionEl = document.getElementById('dial-s');
    const defaultCaption = captionEl.textContent;
    let minuteKinds;
    let clockOffset;
    let hovering = false;

    function addSvg(name, attrs) {
      const node = document.createElementNS(SVG_NS, name);
      for (const [key, value] of Object.entries(attrs)) node.setAttribute(key, value);
      svg.appendChild(node);
      return node;
    }

    // Hours into the user's day, by the server's clock rather than this browser's.
    function nowHours() {
      return ((Date.now() + clockOffset) / 1000 - snapshot.day_start) / 3600;
    }

    function dominantKind(hour, radius) {
      const counts = {};
      let best = null;
      let bestCount = 0;
      const last = Math.round((hour + radius) * 60);
      for (let i = Math.round((hour - radius) * 60); i <= last; i++) {
        const kind = minuteKinds[(i + 1440) % 1440];
        if (kind) {
          counts[kind] = (counts[kind] || 0) + 1;
          if (counts[kind] > bestCount) {
            bestCount = counts[kind];
            best = kind;
          }
        }
      }
      return best;
    }

    function load(next) {
      snapshot = next;
      clockOffset = snapshot.now * 1000 - Date.now();
      minuteKinds = new Array(1440).fill(null);
      snapshot.spans.forEach(([start, end, kind]) => {
        for (let i = Math.max(0, Math.floor(start * 60)); i < Math.min(1440, Math.ceil(end * 60)); i++) {
          minuteKinds[i] = kind;
        }
      });
    }

    function draw() {
      const now = nowHours();
      svg.textContent = '';

      for (let y = 0; y < 25; y++) {
        let first = -1;
        let last = -1;
        for (let x = 0; x < 25; x++) {
          if (Math.hypot(x - 12, y - 12) <= 12.45) {
            if (first < 0) first = x;
            last = x;
          }
        }
        if (first >= 0) addSvg('rect', { x: first, y, width: last - first + 1, height: 1, fill: '#1d1633' });
      }

      [['00', 12, 4.3], ['06', 19.8, 12.3], ['12', 12, 20.3], ['18', 4.2, 12.3]].forEach(([text, x, y]) => {
        addSvg('text', {
          x: x + 0.5,
          y: y + 0.5,
          'text-anchor': 'middle',
          'dominant-baseline': 'middle',
          'font-size': '.95',
          fill: '#6f66a0',
        }).textContent = text;
      });

      for (let y = 0; y < 25; y++) {
        for (let x = 0; x < 25; x++) {
          const distance = Math.hypot(x - 12, y - 12);
          if (distance < 8.3 || distance > 11.5) continue;
          let angle = Math.atan2(x - 12, -(y - 12));
          if (angle < 0) angle += Math.PI * 2;
          const hour = (angle / (Math.PI * 2)) * 24;
          const kind = dominantKind(hour, 0.17);
          if (kind) {
            addSvg('rect', { x: x + 0.1, y: y + 0.1, width: 0.8, height: 0.8, fill: COLORS[kind] || '#e4e1f0' });
          } else {
            addSvg('rect', {
              x: x + 0.39,
              y: y + 0.39,
              width: 0.22,
              height: 0.22,
              fill: hour > now ? '#2e2758' : '#4a4180',
            });
          }
        }
      }

      const nowAngle = (Math.min(now, 23.99) / 24) * Math.PI * 2;
      addSvg('rect', {
        x: Math.round(12 + Math.sin(nowAngle) * 12) + 0.15,
        y: Math.round(12 - Math.cos(nowAngle) * 12) + 0.15,
        width: 0.7,
        height: 0.7,
        fill: '#fff',
      });
    }

    function showHour(hour) {
      const kind = dominantKind(hour, 0.05);
      const hours = Math.floor(hour);
      const minutes = Math.floor((hour - hours) * 60);
      timeEl.textContent = `${String(hours).padStart(2, '0')}:${String(minutes).padStart(2, '0')}`;
      captionEl.textContent = hour > nowHours()
        ? 'later today'
        : kind ? `focus time, ${LABELS[kind]}` : 'no focus time';
    }

    function renderWeek(bars) {
      const days = document.querySelectorAll('#wk .wk-day');
      bars.forEach((bar, i) => {
        const day = days[i];
        if (!day) return;
        day.title = bar.title;
        day.className = `wk-day${bar.today ? ' now' : ''}`;
        day.querySelector('.l').textContent = bar.label;
        day.querySelector('.f').style.height = `${bar.focus}%`;
        const agent = day.querySelector('.a');
        if (agent) agent.style.height = `${bar.agent}%`;
      });
    }

    function apply(next) {
      load(next);
      document.querySelectorAll('[data-live]').forEach((el) => {
        const value = snapshot[el.dataset.live];
        if (typeof value === 'string') el.textContent = value;
      });
      if (!hovering) timeEl.textContent = snapshot.today;
      renderWeek(snapshot.bars);
      draw();
    }

    load(snapshot);
    draw();

    box.addEventListener('pointermove', (e) => {
      const rect = box.getBoundingClientRect();
      let angle = Math.atan2(e.clientX - rect.left - rect.width / 2, -(e.clientY - rect.top - rect.height / 2));
      if (angle < 0) angle += Math.PI * 2;
      hovering = true;
      showHour((angle / (Math.PI * 2)) * 24);
    });
    box.addEventListener('pointerleave', () => {
      hovering = false;
      timeEl.textContent = snapshot.today;
      captionEl.textContent = defaultCaption;
    });

    setInterval(() => {
      if (nowHours() >= 24) location.reload();
      else draw();
    }, 30000);

    if (window.EventSource) {
      const source = new EventSource('/me/live');
      source.addEventListener('snapshot', (e) => {
        try {
          apply(JSON.parse(e.data));
        } catch {
          // A bad event leaves the page as it was.
        }
      });
      source.onerror = () => {
        if (source.readyState === EventSource.CLOSED) source.close();
      };
      addEventListener('pagehide', () => source.close());
    }
  }

  const box = document.getElementById('dialbox');
  const snapEl = document.getElementById('snap');
  if (box && snapEl) dashboard(box, snapEl);

  document.querySelectorAll('.heat').forEach((heat) => {
    heat.scrollLeft = heat.scrollWidth;
  });

  document.querySelectorAll('.cmd .copy').forEach((button) => {
    const code = button.parentNode.querySelector('code');
    button.hidden = false;
    button.addEventListener('click', () => {
      const done = () => {
        button.textContent = 'Copied';
        setTimeout(() => {
          button.textContent = 'Copy';
        }, 1400);
      };
      const selectCode = () => {
        const range = document.createRange();
        range.selectNodeContents(code);
        const selection = getSelection();
        selection.removeAllRanges();
        selection.addRange(range);
      };
      try {
        navigator.clipboard.writeText(code.textContent).then(done, selectCode);
      } catch {
        selectCode();
      }
    });
  });

  const hint = document.getElementById('tzhint');
  if (hint) {
    let zone;
    try {
      zone = Intl.DateTimeFormat().resolvedOptions().timeZone;
    } catch {
      zone = null;
    }
    if (zone && hint.dataset.auto) {
      fetch('/me/timezone', {
        method: 'POST',
        body: new URLSearchParams({ csrf: hint.dataset.csrf, timezone: zone }),
      })
        .then((res) => (res.ok ? res.json() : null))
        .then((json) => {
          if (json && json.changed) location.reload();
        })
        .catch(() => {});
    } else if (zone && zone !== hint.dataset.tz) {
      document.getElementById('tzname').textContent = zone;
      hint.hidden = false;
    }
  }
})();
