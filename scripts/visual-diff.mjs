// Compare two screenshot directories (same layout) and write an HTML report.
//
//   node scripts/visual-diff.mjs <before-dir> <after-dir> [--out docs/screens/diff]
//                                [--threshold 0.1] [--fail-over <pct>]
//
// Directories may be flat (<dir>/<name>.png) or nested one level
// (<dir>/<WxH>-<theme>/<name>.png) as written by capture-screens.mjs.
// Uses pixelmatch + pngjs when resolvable (npm i --no-save pixelmatch pngjs);
// otherwise a built-in dependency-free PNG decoder (node:zlib) and a
// per-pixel colour-distance comparison. Output: <out>/index.html plus
// before/after/diff PNGs next to it, and a one-line-per-screen summary.
import { copyFileSync, existsSync, mkdirSync, readdirSync, readFileSync, statSync, writeFileSync } from 'node:fs';
import { createRequire } from 'node:module';
import { dirname, join, resolve } from 'node:path';
import { fileURLToPath } from 'node:url';
import { deflateSync, inflateSync } from 'node:zlib';

const here = dirname(fileURLToPath(import.meta.url));
const selfRepo = resolve(here, '..');
const argv = process.argv.slice(2);
const pos = [];
const flags = {};
for (let i = 0; i < argv.length; i++) {
  if (argv[i].startsWith('--')) { flags[argv[i].slice(2)] = argv[i + 1]; i++; } else pos.push(argv[i]);
}
if (pos.length !== 2) { console.error('usage: node scripts/visual-diff.mjs <before-dir> <after-dir> [--out dir] [--threshold 0.1] [--fail-over pct]'); process.exit(2); }
const [beforeDir, afterDir] = pos.map((p) => resolve(p));
const outDir = resolve(flags.out || join(selfRepo, 'docs/screens/diff'));
const threshold = Number(flags.threshold ?? 0.1); // YIQ-ish colour tolerance, 0..1
const failOver = flags['fail-over'] === undefined ? null : Number(flags['fail-over']);

// ---- optional pixelmatch/pngjs ---------------------------------------------
let PNGjs = null, pixelmatch = null;
for (const r of [process.cwd(), selfRepo, ...(process.env.NODE_PATH || '').split(':').filter(Boolean)]) {
  try {
    const req = createRequire(join(r, 'x.js'));
    const reqNm = createRequire(join(r, 'node_modules', 'x.js'));
    const get = (n) => { try { return req(n); } catch { return reqNm(n); } };
    PNGjs = get('pngjs').PNG; const pm = get('pixelmatch'); pixelmatch = pm.default || pm; break;
  } catch { PNGjs = pixelmatch = null; }
}

// ---- minimal PNG codec (8-bit, non-interlaced; gray/RGB/RGBA/palette) -------
function decodePng(buf) {
  if (PNGjs) { const p = PNGjs.sync.read(buf); return { width: p.width, height: p.height, data: p.data }; }
  if (buf.readUInt32BE(0) !== 0x89504e47) throw new Error('not a PNG');
  let off = 8, width = 0, height = 0, depth = 0, ctype = 0, interlace = 0, plte = null, trns = null;
  const idat = [];
  while (off < buf.length) {
    const len = buf.readUInt32BE(off), type = buf.toString('latin1', off + 4, off + 8), d = buf.subarray(off + 8, off + 8 + len);
    if (type === 'IHDR') { width = d.readUInt32BE(0); height = d.readUInt32BE(4); depth = d[8]; ctype = d[9]; interlace = d[12]; }
    else if (type === 'PLTE') plte = d; else if (type === 'tRNS') trns = d; else if (type === 'IDAT') idat.push(d);
    else if (type === 'IEND') break;
    off += 12 + len;
  }
  if (depth !== 8 || interlace) throw new Error(`unsupported PNG (depth ${depth}, interlace ${interlace}); install pngjs`);
  const ch = { 0: 1, 2: 3, 3: 1, 4: 2, 6: 4 }[ctype];
  const bpp = ch, stride = width * ch;
  const raw = inflateSync(Buffer.concat(idat));
  const out = Buffer.alloc(height * stride);
  for (let y = 0; y < height; y++) {
    const f = raw[y * (stride + 1)];
    const src = raw.subarray(y * (stride + 1) + 1, (y + 1) * (stride + 1));
    for (let x = 0; x < stride; x++) {
      const a = x >= bpp ? out[y * stride + x - bpp] : 0;
      const b = y > 0 ? out[(y - 1) * stride + x] : 0;
      const c = x >= bpp && y > 0 ? out[(y - 1) * stride + x - bpp] : 0;
      let v = src[x];
      if (f === 1) v += a; else if (f === 2) v += b; else if (f === 3) v += (a + b) >> 1;
      else if (f === 4) { const p = a + b - c, pa = Math.abs(p - a), pb = Math.abs(p - b), pc = Math.abs(p - c); v += pa <= pb && pa <= pc ? a : pb <= pc ? b : c; }
      out[y * stride + x] = v & 255;
    }
  }
  const data = Buffer.alloc(width * height * 4);
  for (let i = 0; i < width * height; i++) {
    let r, g, bl, al = 255;
    if (ctype === 6) { r = out[i * 4]; g = out[i * 4 + 1]; bl = out[i * 4 + 2]; al = out[i * 4 + 3]; }
    else if (ctype === 2) { r = out[i * 3]; g = out[i * 3 + 1]; bl = out[i * 3 + 2]; }
    else if (ctype === 0) { r = g = bl = out[i]; }
    else if (ctype === 4) { r = g = bl = out[i * 2]; al = out[i * 2 + 1]; }
    else { const k = out[i]; r = plte[k * 3]; g = plte[k * 3 + 1]; bl = plte[k * 3 + 2]; al = trns && k < trns.length ? trns[k] : 255; }
    data[i * 4] = r; data[i * 4 + 1] = g; data[i * 4 + 2] = bl; data[i * 4 + 3] = al;
  }
  return { width, height, data };
}
const crcTable = (() => { const t = new Uint32Array(256); for (let n = 0; n < 256; n++) { let c = n; for (let k = 0; k < 8; k++) c = c & 1 ? 0xedb88320 ^ (c >>> 1) : c >>> 1; t[n] = c >>> 0; } return t; })();
const crc32 = (b) => { let c = 0xffffffff; for (const x of b) c = crcTable[(c ^ x) & 255] ^ (c >>> 8); return (c ^ 0xffffffff) >>> 0; };
function encodePng({ width, height, data }) {
  if (PNGjs) { const p = new PNGjs({ width, height }); data.copy(p.data); return PNGjs.sync.write(p); }
  const chunk = (type, d) => { const b = Buffer.alloc(12 + d.length); b.writeUInt32BE(d.length, 0); b.write(type, 4, 'latin1'); d.copy(b, 8); b.writeUInt32BE(crc32(b.subarray(4, 8 + d.length)), 8 + d.length); return b; };
  const ihdr = Buffer.alloc(13); ihdr.writeUInt32BE(width, 0); ihdr.writeUInt32BE(height, 4); ihdr[8] = 8; ihdr[9] = 6;
  const raw = Buffer.alloc(height * (width * 4 + 1));
  for (let y = 0; y < height; y++) data.copy(raw, y * (width * 4 + 1) + 1, y * width * 4, (y + 1) * width * 4);
  return Buffer.concat([Buffer.from([0x89, 0x50, 0x4e, 0x47, 0x0d, 0x0a, 0x1a, 0x0a]), chunk('IHDR', ihdr), chunk('IDAT', deflateSync(raw)), chunk('IEND', Buffer.alloc(0))]);
}

// ---- comparison --------------------------------------------------------------
const maxDelta = 35215 * threshold * threshold; // same scale as pixelmatch
function yiq(r1, g1, b1, r2, g2, b2) {
  const y = (r, g, b) => r * 0.29889531 + g * 0.58662247 + b * 0.11448223;
  const i = (r, g, b) => r * 0.59597799 - g * 0.2741761 - b * 0.32180189;
  const q = (r, g, b) => r * 0.21147017 - g * 0.52261711 + b * 0.31114694;
  const dy = y(r1, g1, b1) - y(r2, g2, b2), di = i(r1, g1, b1) - i(r2, g2, b2), dq = q(r1, g1, b1) - q(r2, g2, b2);
  return 0.5053 * dy * dy + 0.299 * di * di + 0.1957 * dq * dq;
}
function compare(a, b) {
  if (pixelmatch && a.width === b.width && a.height === b.height) {
    const diff = Buffer.alloc(a.width * a.height * 4);
    const n = pixelmatch(a.data, b.data, diff, a.width, a.height, { threshold, includeAA: false });
    return { width: a.width, height: a.height, diff, count: n, pct: (n / (a.width * a.height)) * 100, sizeMismatch: false };
  }
  const width = Math.max(a.width, b.width), height = Math.max(a.height, b.height);
  const diff = Buffer.alloc(width * height * 4);
  let n = 0;
  const px = (img, x, y) => (x < img.width && y < img.height ? (y * img.width + x) * 4 : -1);
  for (let y = 0; y < height; y++) for (let x = 0; x < width; x++) {
    const o = (y * width + x) * 4, ia = px(a, x, y), ib = px(b, x, y);
    let bad;
    if (ia < 0 || ib < 0) bad = true;
    else bad = yiq(a.data[ia], a.data[ia + 1], a.data[ia + 2], b.data[ib], b.data[ib + 1], b.data[ib + 2]) > maxDelta;
    if (bad) { n++; diff[o] = 255; diff[o + 1] = 0; diff[o + 2] = 80; diff[o + 3] = 255; }
    else if (ib >= 0) { const g = Math.round((b.data[ib] + b.data[ib + 1] + b.data[ib + 2]) / 3 * 0.25 + 191); diff[o] = diff[o + 1] = diff[o + 2] = g; diff[o + 3] = 255; }
    else { diff[o] = diff[o + 1] = diff[o + 2] = 235; diff[o + 3] = 255; }
  }
  return { width, height, diff, count: n, pct: (n / (width * height)) * 100, sizeMismatch: a.width !== b.width || a.height !== b.height };
}

// ---- collect pairs -----------------------------------------------------------
function listPngs(dir) {
  const out = new Map();
  if (!existsSync(dir)) return out;
  for (const e of readdirSync(dir)) {
    const p = join(dir, e);
    if (statSync(p).isDirectory()) { for (const f of readdirSync(p)) if (f.endsWith('.png')) out.set(`${e}/${f}`, join(p, f)); }
    else if (e.endsWith('.png')) out.set(e, p);
  }
  return out;
}
const A = listPngs(beforeDir), B = listPngs(afterDir);
if (!A.size || !B.size) { console.error(`no PNGs found (before: ${A.size}, after: ${B.size})`); process.exit(2); }
mkdirSync(outDir, { recursive: true });
// Generated report (copies of every PNG): keep it out of git.
if (!existsSync(join(outDir, '.gitignore'))) writeFileSync(join(outDir, '.gitignore'), '*\n');
const keys = [...new Set([...A.keys(), ...B.keys()])].sort();
const rows = [];
for (const k of keys) {
  const slug = k.replace(/\//g, '__').replace(/\.png$/, '');
  const row = { key: k, slug };
  try {
    if (A.has(k)) { copyFileSync(A.get(k), join(outDir, slug + '.before.png')); row.before = slug + '.before.png'; }
    if (B.has(k)) { copyFileSync(B.get(k), join(outDir, slug + '.after.png')); row.after = slug + '.after.png'; }
    if (A.has(k) && B.has(k)) {
      const r = compare(decodePng(readFileSync(A.get(k))), decodePng(readFileSync(B.get(k))));
      writeFileSync(join(outDir, slug + '.diff.png'), encodePng({ width: r.width, height: r.height, data: r.diff }));
      row.diff = slug + '.diff.png'; row.pct = r.pct; row.sizeMismatch = r.sizeMismatch;
    } else row.status = A.has(k) ? 'removed' : 'added';
  } catch (e) { row.error = e.message; }
  rows.push(row);
}

const esc = (s) => String(s).replace(/[&<>"]/g, (c) => ({ '&': '&amp;', '<': '&lt;', '>': '&gt;', '"': '&quot;' }[c]));
const fmt = (p) => (p === undefined ? '' : p === 0 ? '0.00%' : p < 0.01 ? '<0.01%' : p.toFixed(2) + '%');
const changed = rows.filter((r) => r.pct > 0 || r.status || r.error).length;
const html = `<!doctype html><html lang="en"><meta charset="utf-8"><meta name="viewport" content="width=device-width,initial-scale=1">
<title>FrameTrace visual diff</title>
<style>
:root{color-scheme:light dark;--bg:#fff;--fg:#1b1f24;--mut:#667;--bd:#d0d7de;--bad:#cf222e;--ok:#1a7f37}
@media(prefers-color-scheme:dark){:root{--bg:#0d1117;--fg:#e6edf3;--mut:#9aa4af;--bd:#30363d;--bad:#ff7b72;--ok:#56d364}}
body{background:var(--bg);color:var(--fg);font:14px/1.5 system-ui,sans-serif;margin:0 16px 48px}
h1{font-size:20px}table{border-collapse:collapse;margin:12px 0}td,th{border:1px solid var(--bd);padding:4px 10px;text-align:left}
.bad{color:var(--bad);font-weight:600}.ok{color:var(--ok)}.mut{color:var(--mut)}
section{border-top:1px solid var(--bd);margin-top:20px;padding-top:8px}
.trio{display:grid;grid-template-columns:repeat(3,minmax(0,1fr));gap:8px}
.trio figure{margin:0}.trio img{width:100%;height:auto;border:1px solid var(--bd);display:block}
figcaption{font-size:12px;color:var(--mut)}
@media(max-width:900px){.trio{grid-template-columns:1fr}}
</style>
<h1>Visual diff</h1>
<p class="mut">before: ${esc(beforeDir)}<br>after: ${esc(afterDir)}<br>threshold ${threshold} (${pixelmatch ? 'pixelmatch + pngjs' : 'built-in PNG decoder + YIQ compare'}) &middot; ${rows.length} screens, ${changed} differ</p>
<table><tr><th>Screen</th><th>Diff</th></tr>
${rows.map((r) => `<tr><td><a href="#${esc(r.slug)}">${esc(r.key)}</a></td><td class="${r.pct > 0 || r.status || r.error ? 'bad' : 'ok'}">${r.error ? 'error: ' + esc(r.error) : r.status ? r.status : fmt(r.pct) + (r.sizeMismatch ? ' (size differs)' : '')}</td></tr>`).join('\n')}
</table>
${rows.map((r) => `<section id="${esc(r.slug)}"><h2 style="font-size:15px">${esc(r.key)} <span class="${r.pct > 0 || r.status ? 'bad' : 'ok'}">${r.status || fmt(r.pct)}</span></h2>
<div class="trio">
<figure>${r.before ? `<img loading="lazy" src="${esc(r.before)}" alt="before ${esc(r.key)}">` : '<p class="mut">missing</p>'}<figcaption>before</figcaption></figure>
<figure>${r.after ? `<img loading="lazy" src="${esc(r.after)}" alt="after ${esc(r.key)}">` : '<p class="mut">missing</p>'}<figcaption>after</figcaption></figure>
<figure>${r.diff ? `<img loading="lazy" src="${esc(r.diff)}" alt="diff ${esc(r.key)}">` : '<p class="mut">n/a</p>'}<figcaption>diff (red = changed pixels)</figcaption></figure>
</div></section>`).join('\n')}
</html>`;
writeFileSync(join(outDir, 'index.html'), html);
for (const r of rows) console.log(`${(r.error ? 'ERROR ' + r.error : r.status || fmt(r.pct)).padEnd(10)} ${r.key}`);
console.log(`${rows.length} screens, ${changed} differ -> ${join(outDir, 'index.html')}`);
const worst = Math.max(0, ...rows.map((r) => r.pct || 0));
if (failOver !== null && (worst > failOver || rows.some((r) => r.error))) process.exitCode = 1;
