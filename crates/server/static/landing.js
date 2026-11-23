(() => {
  const MARK = [
    '...o##...',
    '.ooo####.',
    '.oo.####.',
    'oo..##.oo',
    'oo.....oo',
    'oo.....oo',
    '.oo...oo.',
    '.ooooooo.',
    '...ooo...',
  ];

  document.querySelectorAll('svg[data-mark]').forEach((mark) => {
    let rects = '';
    MARK.forEach((row, y) => {
      for (let x = 0; x < 9; x++) {
        const cell = row[x];
        if (cell !== '.') {
          rects += `<rect x="${x}" y="${y}" width="1" height="1"${cell === 'o' ? ' opacity=".42"' : ''}/>`;
        }
      }
    });
    mark.setAttribute('viewBox', '0 0 9 9');
    mark.setAttribute('fill', 'currentColor');
    mark.setAttribute('shape-rendering', 'crispEdges');
    mark.innerHTML = rects;
  });

  const BASE = document.documentElement.dataset.base || location.origin;
  const INSTALL = {
    win: ['>', `irm ${BASE}/install.ps1 | iex`],
    mac: ['$', `curl -fsSL ${BASE}/install.sh | sh`],
    linux: ['$', `curl -fsSL ${BASE}/install.sh | sh`],
  };
  const guessedOs = /Mac/.test(navigator.platform)
    ? 'mac'
    : /Linux/.test(navigator.platform) && !/Android/.test(navigator.userAgent)
      ? 'linux'
      : 'win';

  document.querySelectorAll('.install').forEach((box) => {
    const tabs = box.querySelectorAll('[data-os]');
    const code = box.querySelector('code');
    const copyButton = box.querySelector('.copy');

    const pick = (os) => {
      tabs.forEach((tab) => tab.setAttribute('aria-selected', tab.dataset.os === os));
      box.querySelector('.ps').textContent = INSTALL[os][0];
      code.textContent = INSTALL[os][1];
    };
    tabs.forEach((tab) => tab.addEventListener('click', () => pick(tab.dataset.os)));
    pick(guessedOs);

    copyButton.addEventListener('click', () => {
      const done = () => {
        copyButton.textContent = 'copied';
        setTimeout(() => {
          copyButton.textContent = 'copy';
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

  const COLORS = { typing: '#ffd166', reading: '#6ee7c8', editing: '#ff8fab', testing: '#8fd3ff' };
  // [start hour, end hour, kind]. Kinds without a color are the agent working, the gaps are breaks.
  const DAY = [
    [8.2, 8.45, 'reading'], [8.45, 8.6, 'typing'], [8.6, 9.4, 'agent'],
    [9.4, 9.55, 'reading'], [9.55, 9.7, 'typing'], [9.7, 10.6, 'agent'],
    [10.6, 11.3, 'editing'], [11.3, 11.5, 'testing'], [11.5, 11.65, 'typing'], [11.65, 12.4, 'agent'],
    [13.3, 13.5, 'reading'], [13.5, 13.7, 'typing'], [13.7, 15.1, 'agent'],
    [15.1, 15.5, 'testing'], [15.5, 15.7, 'reading'], [15.7, 15.85, 'typing'], [15.85, 16.9, 'agent'],
    [16.9, 17.6, 'editing'], [17.6, 18, 'testing'],
    [19.5, 19.7, 'typing'], [19.7, 20.8, 'agent'],
    [20.8, 21.1, 'reading'], [21.1, 21.3, 'typing'], [21.3, 22.1, 'agent'],
    [22.1, 22.6, 'testing'],
  ];
  const LABELS = {
    typing: 'typing a prompt',
    reading: 'reading the reply',
    editing: 'editing code',
    testing: 'testing the app',
    agent: 'the agent is working',
    away: 'taking a break',
  };

  const kindAt = (hour) => {
    for (const [start, end, kind] of DAY) {
      if (hour >= start && hour < end) return kind;
    }
    return 'away';
  };
  const formatHours = (hours) => {
    const minutes = Math.round(hours * 60);
    const h = Math.floor(minutes / 60);
    return minutes % 60 === 0 ? `${h}h` : `${h}h ${String(minutes % 60).padStart(2, '0')}m`;
  };

  let focusHours = 0;
  let agentHours = 0;
  DAY.forEach(([start, end, kind]) => {
    if (COLORS[kind]) focusHours += end - start;
    else agentHours += end - start;
  });
  document.getElementById('d-note').innerHTML =
    `A sample day: <b>${formatHours(focusHours)}</b> of focus time<i style="box-shadow:inset 0 0 0 2px var(--agent)"></i>${formatHours(agentHours)} agent time`;

  const svg = document.getElementById('dial-svg');
  const SVG_NS = 'http://www.w3.org/2000/svg';
  const FACE_RADIUS = 12.45;
  const cells = [];

  const addSvg = (name, attrs) => {
    const node = document.createElementNS(SVG_NS, name);
    for (const [key, value] of Object.entries(attrs)) node.setAttribute(key, value);
    svg.appendChild(node);
    return node;
  };

  for (let y = 0; y < 25; y++) {
    let first = -1;
    let last = -1;
    for (let x = 0; x < 25; x++) {
      if (Math.hypot(x - 12, y - 12) <= FACE_RADIUS) {
        if (first < 0) first = x;
        last = x;
      }
    }
    if (first >= 0) addSvg('rect', { x: first, y, width: last - first + 1, height: 1, class: 'face' });
  }

  [['00', 12, 4.3], ['06', 19.8, 12.3], ['12', 12, 20.3], ['18', 4.2, 12.3]].forEach(([text, x, y]) => {
    const label = addSvg('text', {
      x: x + 0.5,
      y: y + 0.5,
      'text-anchor': 'middle',
      'dominant-baseline': 'middle',
      'font-size': '.95',
      'font-family': 'IBM Plex Mono, monospace',
      fill: '#6f66a0',
    });
    label.textContent = text;
  });

  for (let y = 0; y < 25; y++) {
    for (let x = 0; x < 25; x++) {
      const distance = Math.hypot(x - 12, y - 12);
      if (distance < 8.3 || distance > 11.5) continue;
      let angle = Math.atan2(x - 12, -(y - 12));
      if (angle < 0) angle += Math.PI * 2;
      const hour = (angle / (Math.PI * 2)) * 24;
      const kind = kindAt(hour);
      let rect;
      if (COLORS[kind]) {
        rect = addSvg('rect', { x: x + 0.1, y: y + 0.1, width: 0.8, height: 0.8, fill: COLORS[kind] });
      } else if (kind === 'agent') {
        rect = addSvg('rect', {
          x: x + 0.22,
          y: y + 0.22,
          width: 0.56,
          height: 0.56,
          fill: 'none',
          stroke: '#a297f5',
          'stroke-width': '.16',
        });
      } else {
        rect = addSvg('rect', { x: x + 0.39, y: y + 0.39, width: 0.22, height: 0.22, fill: '#4a4180' });
      }
      cells.push({ rect, hour, kind });
    }
  }

  const timeEl = document.getElementById('d-time');
  const labelEl = document.getElementById('d-label');
  const stateEl = document.getElementById('d-state');
  const reduceMotion = matchMedia('(prefers-reduced-motion: reduce)').matches;
  let now = 10.9;
  let heldHour = null;
  let lastFrame = 0;
  let shownKey = '';

  function paint() {
    cells.forEach(({ rect, hour, kind }) => {
      const passed = hour <= now;
      const current = Math.abs(hour - now) < 0.34;
      rect.setAttribute('opacity', passed || heldHour !== null ? 1 : 0.5);
      if (current) {
        rect.setAttribute(kind === 'agent' ? 'stroke' : 'fill', '#ffffff');
      } else if (COLORS[kind]) {
        rect.setAttribute('fill', COLORS[kind]);
      } else if (kind === 'agent') {
        rect.setAttribute('stroke', '#a297f5');
      } else {
        rect.setAttribute('fill', '#4a4180');
      }
    });

    const hours = Math.floor(now) % 24;
    const minutes = Math.floor(((now % 1) * 60) / 5) * 5;
    const kind = kindAt(now);
    const key = `${hours}:${minutes}`;
    if (key === shownKey) return;
    shownKey = key;
    timeEl.textContent = `${String(hours).padStart(2, '0')}:${String(minutes).padStart(2, '0')}`;
    labelEl.textContent = LABELS[kind];
    if (COLORS[kind]) {
      stateEl.className = '';
      stateEl.style.setProperty('--c', COLORS[kind]);
      stateEl.textContent = 'focus time';
    } else {
      stateEl.className = 'no';
      stateEl.textContent = kind === 'agent' ? 'agent time' : 'break';
    }
  }

  let visible = true;
  let ticking = false;

  function tick(time) {
    if (!visible) {
      ticking = false;
      return;
    }
    if (heldHour === null) {
      const elapsed = Math.min(0.05, (time - lastFrame) / 1000);
      now += elapsed * 0.8;
      if (now >= 24.6) now = 6;
    }
    lastFrame = time;
    paint();
    requestAnimationFrame(tick);
  }

  function start() {
    if (!ticking && !reduceMotion) {
      ticking = true;
      lastFrame = performance.now();
      requestAnimationFrame(tick);
    }
  }

  const dial = document.getElementById('dial');

  function holdAtPoint(clientX, clientY) {
    const box = dial.getBoundingClientRect();
    const dx = clientX - (box.left + box.width / 2);
    const dy = clientY - (box.top + box.height / 2);
    let angle = Math.atan2(dx, -dy);
    if (angle < 0) angle += Math.PI * 2;
    heldHour = now = (angle / (Math.PI * 2)) * 24;
    if (reduceMotion) paint();
  }

  dial.addEventListener('pointermove', (e) => holdAtPoint(e.clientX, e.clientY));
  dial.addEventListener('pointerleave', () => {
    heldHour = null;
  });
  dial.addEventListener('keydown', (e) => {
    if (e.key === 'ArrowRight' || e.key === 'ArrowLeft') {
      now = ((heldHour === null ? now : heldHour) + (e.key === 'ArrowRight' ? 0.25 : -0.25) + 24) % 24;
      heldHour = now;
      paint();
      e.preventDefault();
    }
  });
  dial.addEventListener('blur', () => {
    heldHour = null;
  });

  paint();
  start();
  new IntersectionObserver((entries) => {
    visible = entries[0].isIntersecting;
    if (visible) start();
  }).observe(dial);
})();

(() => {
  const CELL = 6;
  const PAPER = [251, 248, 242];
  const KIND_RGB = {
    typing: [255, 209, 102],
    reading: [110, 231, 200],
    editing: [255, 143, 171],
    testing: [143, 211, 255],
  };
  const WARM = {
    core: [255, 248, 226],
    edge: [255, 214, 150],
    peach: [255, 166, 124],
    rose: [222, 102, 152],
    gold: [255, 214, 140],
  };

  function oklchToRgb(lightness, chroma, hue) {
    const a = chroma * Math.cos((hue * Math.PI) / 180);
    const b = chroma * Math.sin((hue * Math.PI) / 180);
    const l = Math.pow(lightness + 0.3963377774 * a + 0.2158037573 * b, 3);
    const m = Math.pow(lightness - 0.1055613458 * a - 0.0638541728 * b, 3);
    const s = Math.pow(lightness - 0.0894841775 * a - 1.291485548 * b, 3);
    const red = 4.0767416621 * l - 3.3077115913 * m + 0.2309699292 * s;
    const green = -1.2684380046 * l + 2.6097574011 * m - 0.3413193965 * s;
    const blue = -0.0041960863 * l - 0.7034186147 * m + 1.707614701 * s;
    const encode = (linear) => {
      const x = Math.max(0, Math.min(1, linear));
      return Math.round(255 * (x <= 0.0031308 ? 12.92 * x : 1.055 * Math.pow(x, 1 / 2.4) - 0.055));
    };
    return [encode(red), encode(green), encode(blue)];
  }

  const toHex = (rgb) => `#${rgb.map((v) => v.toString(16).padStart(2, '0')).join('')}`;

  const ramp = Array.from({ length: 16 }, (_, i) => {
    const t = i / 15;
    return oklchToRgb(
      0.975 - t * 0.82,
      0.19 * Math.pow(Math.sin(Math.PI * Math.min(1, 0.15 + t * 0.95)), 0.9),
      300 + (262 - 300) * t,
    );
  });
  document.documentElement.style.setProperty('--r10', toHex(ramp[10]));

  const ICONS = {
    eye: ['........', '..####..', '.#....#.', '#..##..#', '#..##..#', '.#....#.', '..####..', '........'],
    pencil: ['......##', '.....###', '....###.', '...###..', '..###...', '.###....', '##......', '#.......'],
    chat: ['........', '.######.', '#......#', '#.#.#..#', '#......#', '.######.', '.#......', '#.......'],
    lock: ['..####..', '.#....#.', '.#....#.', '########', '###..###', '###..###', '########', '........'],
    hash: ['.#..#...', '######..', '.#..#...', '.#..#...', '######..', '.#..#...', '........', '........'],
    chart: ['........', '......#.', '......#.', '...#..#.', '...#..#.', '#..#..#.', '#..#..#.', '########'],
  };

  function iconSvg(name, color) {
    const bitmap = ICONS[name];
    let rects = '';
    for (let y = 0; y < 8; y++) {
      for (let x = 0; x < 8; x++) {
        if (bitmap[y][x] === '#') rects += `<rect x="${x}" y="${y}" width="1" height="1"/>`;
      }
    }
    return `<svg class="ico" viewBox="0 0 8 8" fill="${color}" aria-hidden="true">${rects}</svg>`;
  }

  const ICON_COLORS = { typing: '#e0a800', reading: '#18b48e', editing: '#e8577c', testing: '#3a9fe0' };
  document.querySelectorAll('[data-ico]').forEach((el) => {
    const name = el.dataset.c;
    const color = ICON_COLORS[name]
      || (name === 'ink' ? '#17132e' : getComputedStyle(document.documentElement).getPropertyValue(`--${name}`).trim());
    el.insertAdjacentHTML('afterbegin', iconSvg(el.dataset.ico, color));
  });

  const BAYER = [0, 8, 2, 10, 12, 4, 14, 6, 3, 11, 1, 9, 15, 7, 13, 5].map((v) => (v + 0.5) / 16);
  const bayer = (x, y) => BAYER[(y & 3) * 4 + (x & 3)];
  const random = (n) => {
    const s = Math.sin(n * 127.1 + 311.7) * 43758.5453;
    return s - Math.floor(s);
  };
  const noise = (x) => {
    const i = Math.floor(x);
    const f = x - i;
    const u = f * f * (3 - 2 * f);
    return random(i) * (1 - u) + random(i + 1) * u;
  };

  const formatHours = (hours) => {
    const minutes = Math.round(hours * 60);
    const h = Math.floor(minutes / 60);
    return minutes % 60 === 0 ? `${h}h` : `${h}h ${String(minutes % 60).padStart(2, '0')}m`;
  };

  class PixelSurface {
    constructor(canvas, host) {
      this.canvas = canvas;
      this.host = host;
      this.ctx = canvas.getContext('2d');
      this.offscreen = document.createElement('canvas');
      this.offscreenCtx = this.offscreen.getContext('2d');
      this.width = 0;
      this.height = 0;
      this.image = null;
    }

    resize() {
      const rect = this.host.getBoundingClientRect();
      const ratio = window.devicePixelRatio || 1;
      this.width = Math.ceil(rect.width / CELL);
      this.height = Math.ceil(rect.height / CELL);
      this.offscreen.width = this.width;
      this.offscreen.height = this.height;
      this.image = this.offscreenCtx.createImageData(this.width, this.height);
      this.canvas.width = Math.round(this.width * CELL * ratio);
      this.canvas.height = Math.round(this.height * CELL * ratio);
      this.canvas.style.width = `${this.width * CELL}px`;
      this.canvas.style.height = `${this.height * CELL}px`;
    }

    setPixel(x, y, rgb) {
      if (x < 0 || y < 0 || x >= this.width || y >= this.height) return;
      const data = this.image.data;
      const p = (y * this.width + x) * 4;
      data[p] = rgb[0];
      data[p + 1] = rgb[1];
      data[p + 2] = rgb[2];
      data[p + 3] = 255;
    }

    // `level` is a position along the 16 step ramp, clamped, fractions floor.
    put(x, y, level) {
      this.setPixel(x, y, ramp[Math.max(0, Math.min(15, Math.floor(level)))]);
    }

    flush() {
      this.offscreenCtx.putImageData(this.image, 0, 0);
      this.ctx.imageSmoothingEnabled = false;
      this.ctx.drawImage(this.offscreen, 0, 0, this.canvas.width, this.canvas.height);
    }
  }

  const dithered = (level, x, y) => level + bayer(x, y) - 0.5;

  function drawHills(surface, horizon, darkness, farLevel, nearLevel) {
    const width = surface.width;
    const ridgeHeight = (x) => Math.round(4 + 6 * noise(x * 0.04 + 3) + 2.5 * noise(x * 0.13 + 7));

    for (let x = 0; x < width; x++) {
      const top = horizon - ridgeHeight(x);
      for (let y = top; y <= horizon; y++) {
        surface.put(x, y, dithered(farLevel + darkness * 0.4 + (y === top ? -0.6 : 0), x, y));
      }
    }

    const towerX = Math.round(width * 0.8);
    const towerY = horizon - ridgeHeight(towerX) - 1;
    for (let col = 0; col < 7; col++) {
      for (let row = 0; row < 5; row++) surface.put(towerX + col * 2, towerY - row, farLevel + 1.6 + darkness * 0.2);
    }
    for (let col = -1; col <= 13; col++) {
      surface.put(towerX + col, towerY - 5, farLevel + 2);
      surface.put(towerX + col, towerY - 6, farLevel + 2);
    }
    for (let row = 0; row < 4; row++) {
      for (let col = row * 2; col <= 12 - row * 2; col++) surface.put(towerX + col, towerY - 7 - row, farLevel + 2);
    }

    for (let x = 0; x < width; x++) {
      const top = horizon + 2 - Math.round(1 + 3.4 * noise(x * 0.07 + 20));
      for (let y = top; y <= horizon + 2; y++) surface.put(x, y, dithered(nearLevel + darkness * 0.4, x, y));
    }

    const drawTree = (baseX, baseY, height, halfWidth) => {
      for (let k = 0; k <= height; k++) {
        const along = k / height;
        const half = Math.round(halfWidth * Math.pow(Math.sin(Math.PI * Math.min(1, 0.12 + along * 0.95)), 0.75));
        for (let q = -half; q <= half; q++) surface.put(baseX + q, baseY - k, 14 + (q === half ? -0.6 : 0));
      }
    };
    const plant = (trees) => {
      trees.forEach(([x, height, halfWidth], i) => drawTree(x, horizon + 3 + (i % 2), height, halfWidth));
    };
    plant([[3, 15, 2], [7, 11, 1], [11, 18, 2], [15, 9, 1], [19, 13, 1]]);
    plant([[width - 4, 17, 2], [width - 9, 12, 1], [width - 13, 20, 2], [width - 18, 10, 1], [width - 22, 14, 1], [width - 27, 8, 1]]);
  }

  function vignette(pic) {
    const surface = new PixelSurface(pic.querySelector('canvas'), pic);
    const hour = parseFloat(pic.dataset.hour);

    const paint = () => {
      surface.resize();
      const { width: W, height: H } = surface;
      const horizon = Math.round(H * 0.64);
      const progress = Math.max(0, Math.min(1, (hour - 6) / 14));
      const elevation = Math.sin(Math.PI * progress);
      const sunX = W * (parseFloat(pic.dataset.sx) || (0.12 + 0.76 * progress));
      const sunY = horizon - 2 - elevation * horizon * 0.72;
      const night = Math.max(0, Math.min(1, (hour - 19.2) / 1.2));
      const darkness = night * 3;
      const low = Math.max(0, 1 - elevation * 1.6) * (1 - night * 0.7);
      const day = Math.max(0, elevation - 0.2) / 0.8;

      for (let y = 0; y < H; y++) {
        for (let x = 0; x < W; x++) {
          let level;
          if (y <= horizon) {
            level = 10.6 - day * 5.2 - (y / horizon) * (2.9 - day * 0.6) + darkness;
            level -= Math.max(0, 1 - Math.hypot(x - sunX, (y - sunY) * 1.15) / 16) * 3.2 * (1 - night);
          } else {
            level = 12.6 - day * 3.4 - ((y - horizon) / (H - horizon)) * 2.4 + darkness * 0.5
              + (random(Math.floor(x / 3) * 13 + y * 977) - 0.5) * 0.9;
          }
          surface.put(x, y, dithered(level, x, y));
        }
      }

      if (low > 0) {
        for (let y = horizon - 20; y <= horizon; y++) {
          for (let x = 0; x < W; x++) {
            const height = Math.max(0, (horizon - 4 - y) / 16);
            const strength = low * (1 - height) * (0.3 + 0.7 * Math.max(0, 1 - Math.abs(x - sunX) / (W * 0.45)));
            if (bayer(x, y) < strength * 0.95) surface.setPixel(x, y, height < 0.34 ? WARM.peach : WARM.rose);
          }
        }
      }

      for (let dy = -7; dy <= 7; dy++) {
        for (let dx = -7; dx <= 7; dx++) {
          const distance = Math.hypot(dx, dy);
          const x = Math.round(sunX) + dx;
          const y = Math.round(sunY) + dy;
          if (y > horizon) continue;
          if (distance <= 2.6) surface.setPixel(x, y, WARM.core);
          else if (distance <= 3.5) surface.setPixel(x, y, WARM.edge);
          else if (distance <= 7 && bayer(x, y) < (1 - (distance - 3.5) / 3.5) * 0.5) {
            surface.setPixel(x, y, low > 0.3 ? WARM.peach : WARM.gold);
          }
        }
      }

      drawHills(surface, horizon, darkness, 10.4 - day * 4, 12.3 - day * 3.4);
      surface.flush();
    };

    new ResizeObserver(paint).observe(pic);
  }
  document.querySelectorAll('.pic[data-hour]').forEach(vignette);

  const steps = [...document.querySelectorAll('.step')];
  const panels = [...document.querySelectorAll('.panel')];

  function show(index) {
    steps.forEach((step, i) => {
      step.classList.toggle('on', index === i);
      step.setAttribute('aria-selected', index === i);
      step.tabIndex = index === i ? 0 : -1;
    });
    panels.forEach((panel, i) => panel.classList.toggle('on', index === i));
  }

  steps.forEach((step, i) => {
    step.addEventListener('click', () => show(i));
    step.querySelector('.bar i').addEventListener('animationend', () => show((i + 1) % steps.length));
    step.addEventListener('keydown', (e) => {
      const direction = e.key === 'ArrowDown' || e.key === 'ArrowRight'
        ? 1
        : e.key === 'ArrowUp' || e.key === 'ArrowLeft' ? -1 : 0;
      if (direction) {
        const next = (i + direction + steps.length) % steps.length;
        show(next);
        steps[next].focus();
        e.preventDefault();
      }
    });
  });
  show(0);

  const howGrid = document.getElementById('how-grid');
  new IntersectionObserver((entries) => {
    howGrid.classList.toggle('idle', !entries[0].isIntersecting);
  }).observe(howGrid);

  const STRIP_KINDS = { t: 'typing', r: 'reading', x: 'testing' };
  const strip = document.getElementById('vstrip');
  [...'trrraaaaaaaaaaaaaaxxxxt'].forEach((code) => {
    const cell = document.createElement('i');
    if (code === 'a') cell.className = 'a';
    else cell.style.setProperty('--c', toHex(KIND_RGB[STRIP_KINDS[code]]));
    strip.appendChild(cell);
  });

  const MIX = [['typing', 0.22], ['reading', 0.21], ['editing', 0.27], ['testing', 0.30]];
  const PROJECTS = [['platformer', 6.083], ['portfolio', 3.333], ['discord-bot', 2.25]];
  document.getElementById('vdash').innerHTML = PROJECTS.map(([name, hours]) => {
    const bar = MIX
      .map(([kind, share]) => `<i style="--c:${toHex(KIND_RGB[kind])};width:${(hours / 6.083) * share * 100}%"></i>`)
      .join('');
    return `<div class="pr"><span>${name}</span><span class="sb">${bar}</span><span>${formatHours(hours)}</span></div>`;
  }).join('');

  function hexToRgb(hex) {
    let digits = hex.trim().replace('#', '');
    if (digits.length === 3) digits = digits.replace(/./g, '$&$&');
    return [parseInt(digits.slice(0, 2), 16), parseInt(digits.slice(2, 4), 16), parseInt(digits.slice(4, 6), 16)];
  }

  function drawDrips() {
    const styles = getComputedStyle(document.documentElement);
    const page = hexToRgb(styles.getPropertyValue('--paper'));
    const field = hexToRgb(styles.getPropertyValue('--field'));
    const bands = [ramp[8], ramp[6], ramp[4], ramp[2], ramp[1]];

    document.querySelectorAll('canvas.drip').forEach((canvas, index) => {
      const up = canvas.classList.contains('up');
      const width = Math.max(1, Math.ceil(canvas.clientWidth / CELL));
      const height = 20;
      const reach = height / 1.45;
      const seed = index * 17 + 3;
      const ctx = canvas.getContext('2d');
      canvas.width = width;
      canvas.height = height;
      const image = ctx.createImageData(width, height);
      const data = image.data;

      for (let x = 0; x < width; x++) {
        const edge = noise(x * 0.06 + seed) * reach * 0.5
          + noise(x * 0.31 + seed + 9) * reach * 0.18
          + random(x * 7.3 + seed) * reach * 0.08;
        for (let k = 0; k < height; k++) {
          const y = up ? height - 1 - k : k;
          const t = (k - edge) / (reach * 0.62);
          let rgb = field;
          if (t > 0) {
            const band = Math.floor(t * 6 + bayer(x, y) - 0.5);
            if (band >= 1) rgb = band >= 6 ? page : bands[band - 1];
          }
          const q = (y * width + x) * 4;
          data[q] = rgb[0];
          data[q + 1] = rgb[1];
          data[q + 2] = rgb[2];
          data[q + 3] = 255;
        }
      }
      ctx.putImageData(image, 0, 0);
    });
  }

  drawDrips();
  let lastWidth = innerWidth;
  addEventListener('resize', () => {
    if (innerWidth !== lastWidth) {
      lastWidth = innerWidth;
      requestAnimationFrame(drawDrips);
    }
  });
})();
