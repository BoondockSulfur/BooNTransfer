// Generates placeholder brand icons (rounded square + two transfer arrows) as
// valid PNGs, with zero dependencies. Replace later with a real logo via:
//   npm run tauri icon assets/logo.png
import { deflateSync } from "node:zlib";
import { writeFileSync, mkdirSync } from "node:fs";
import { fileURLToPath } from "node:url";
import { dirname, join } from "node:path";

const OUT = join(dirname(fileURLToPath(import.meta.url)), "..", "src-tauri", "icons");
mkdirSync(OUT, { recursive: true });

const CRC = (() => {
  const t = new Uint32Array(256);
  for (let n = 0; n < 256; n++) {
    let c = n;
    for (let k = 0; k < 8; k++) c = c & 1 ? 0xedb88320 ^ (c >>> 1) : c >>> 1;
    t[n] = c >>> 0;
  }
  return (buf) => {
    let c = 0xffffffff;
    for (const b of buf) c = t[(c ^ b) & 0xff] ^ (c >>> 8);
    return (c ^ 0xffffffff) >>> 0;
  };
})();

function chunk(type, data) {
  const len = Buffer.alloc(4);
  len.writeUInt32BE(data.length);
  const td = Buffer.concat([Buffer.from(type, "ascii"), data]);
  const crc = Buffer.alloc(4);
  crc.writeUInt32BE(CRC(td));
  return Buffer.concat([len, td, crc]);
}

function png(size, rgba) {
  const sig = Buffer.from([137, 80, 78, 71, 13, 10, 26, 10]);
  const ihdr = Buffer.alloc(13);
  ihdr.writeUInt32BE(size, 0);
  ihdr.writeUInt32BE(size, 4);
  ihdr[8] = 8; // bit depth
  ihdr[9] = 6; // RGBA
  const raw = Buffer.alloc(size * (size * 4 + 1));
  for (let y = 0; y < size; y++) {
    raw[y * (size * 4 + 1)] = 0; // filter: none
    rgba.copy(raw, y * (size * 4 + 1) + 1, y * size * 4, (y + 1) * size * 4);
  }
  return Buffer.concat([
    sig,
    chunk("IHDR", ihdr),
    chunk("IDAT", deflateSync(raw, { level: 9 })),
    chunk("IEND", Buffer.alloc(0)),
  ]);
}

// simple linear blend
const mix = (a, b, t) => Math.round(a + (b - a) * t);

function render(size) {
  const buf = Buffer.alloc(size * size * 4);
  const r = size * 0.22; // corner radius
  const inCorner = (x, y) => {
    const cx = Math.min(Math.max(x, r), size - r);
    const cy = Math.min(Math.max(y, r), size - r);
    return (x - cx) ** 2 + (y - cy) ** 2 <= r * r;
  };
  // arrow band helper
  const band = (x, y, y0, y1, x0, x1) =>
    y >= y0 * size && y <= y1 * size && x >= x0 * size && x <= x1 * size;
  const triRight = (x, y, cx, cy, w, h) =>
    x >= cx && x <= cx + w && Math.abs(y - cy) <= (h / 2) * (1 - (x - cx) / w);
  const triLeft = (x, y, cx, cy, w, h) =>
    x <= cx && x >= cx - w && Math.abs(y - cy) <= (h / 2) * (1 - (cx - x) / w);

  for (let y = 0; y < size; y++) {
    for (let x = 0; x < size; x++) {
      const i = (y * size + x) * 4;
      if (!inCorner(x + 0.5, y + 0.5)) {
        buf[i + 3] = 0; // transparent outside rounded square
        continue;
      }
      // background gradient (teal -> deep blue)
      const t = y / size;
      let R = mix(0x24, 0x14, t),
        G = mix(0x8b, 0x53, t),
        B = mix(0x86, 0x8f, t);
      // white arrows
      const s = size;
      const top =
        band(x, y, 0.31, 0.42, 0.24, 0.6) ||
        triRight(x, y, 0.58 * s, 0.365 * s, 0.16 * s, 0.24 * s);
      const bottom =
        band(x, y, 0.58, 0.69, 0.4, 0.76) ||
        triLeft(x, y, 0.42 * s, 0.635 * s, 0.16 * s, 0.24 * s);
      if (top || bottom) {
        R = 0xff;
        G = 0xff;
        B = 0xff;
      }
      buf[i] = R;
      buf[i + 1] = G;
      buf[i + 2] = B;
      buf[i + 3] = 255;
    }
  }
  return buf;
}

for (const size of [32, 128, 256, 512]) {
  const name = size === 512 ? "icon.png" : `${size}x${size}.png`;
  writeFileSync(join(OUT, name), png(size, render(size)));
  console.log("wrote", name);
}
