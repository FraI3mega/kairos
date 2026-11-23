(() => {
  const CELL = 6;
  const LIGHT_BANDS = [[94, 92, 205], [134, 119, 251], [176, 152, 255], [218, 190, 255], [238, 211, 255]];
  const FIELD = [58, 51, 163];

  function readTheme() {
    const channels = (getComputedStyle(document.body).backgroundColor || '').match(/\d+/g);
    const page = channels ? [+channels[0], +channels[1], +channels[2]] : [245, 245, 248];
    const dark = page[0] + page[1] + page[2] < 300;
    const bands = dark
      ? [1, 2, 3, 4, 5].map((i) => FIELD.map((f, j) => Math.round(f + ((page[j] - f) * i) / 6)))
      : LIGHT_BANDS;
    return { page, bands };
  }

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

  function draw() {
    const { page, bands } = readTheme();
    document.querySelectorAll('canvas.drip').forEach((canvas, index) => {
      const width = Math.max(1, Math.ceil(canvas.clientWidth / CELL));
      const height = 16;
      const reach = height / 1.45;
      const ctx = canvas.getContext('2d');
      const seed = index * 17 + 3;
      canvas.width = width;
      canvas.height = height;
      const image = ctx.createImageData(width, height);
      const data = image.data;

      for (let x = 0; x < width; x++) {
        const edge = noise(x * 0.06 + seed) * reach * 0.5
          + noise(x * 0.31 + seed + 9) * reach * 0.18
          + random(x * 7.3 + seed) * reach * 0.08;
        for (let y = 0; y < height; y++) {
          const t = (y - edge) / (reach * 0.62);
          let rgb = FIELD;
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

  draw();
  try {
    matchMedia('(prefers-color-scheme: dark)').addEventListener('change', draw);
  } catch {
    // Browsers without media query change events keep the first drawing.
  }
  let lastWidth = innerWidth;
  addEventListener('resize', () => {
    if (innerWidth !== lastWidth) {
      lastWidth = innerWidth;
      draw();
    }
  });
})();
