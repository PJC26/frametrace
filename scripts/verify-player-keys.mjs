// Regression check for player frame-step (, .) and Shift+J/K/L shuttle keys.
// Builds a standalone viewer page from
// assets/ (same bundling as build.rs) with synthetic records and drives it
// in headless Chromium via Playwright.
//
//   node scripts/verify-player-keys.mjs
//
// Needs the `playwright` package resolvable (global install is fine via
// NODE_PATH) and a Chromium build (PLAYWRIGHT_BROWSERS_PATH).
import { readFileSync, readdirSync, writeFileSync, mkdtempSync, rmSync } from 'node:fs';
import { join, dirname } from 'node:path';
import { tmpdir } from 'node:os';
import { fileURLToPath } from 'node:url';
import { createServer } from 'node:http';
import { createRequire } from 'node:module';

const root = join(dirname(fileURLToPath(import.meta.url)), '..');
const require = createRequire(import.meta.url);
const { chromium } = require('playwright');

function buildViewer() {
  const dir = join(root, 'assets/viewer');
  let js = '';
  for (const part of readdirSync(dir).filter((f) => f.endsWith('.js')).sort()) {
    js += readFileSync(join(dir, part), 'utf8');
    if (!js.endsWith('\n')) js += '\n';
  }
  const videos = ['a', 'b', 'c'].map((n) => ({
    id: `vid_${n}`, relative_path: `DCIM/${n}.mp4`, source_path: `/case/DCIM/${n}.mp4`,
    ffprobe_ok: true, size_bytes: 1000, extension: 'mp4',
  }));
  const data = `window.__FRAMETRACE_DATA__ = ${JSON.stringify({
    manifest: { case_id: 'FT-UNDO', title: 'undo' }, caseDir: '/case', scan: { videos },
    carveLog: [], filesystemLog: [], validationLog: [], anomalyLog: [], flsEntries: [],
    thumbs: {}, annotations: {}, deepfake: {}, telemetry: {}, proxies: [],
  })};`;
  return readFileSync(join(root, 'assets/evidence_viewer.html'), 'utf8')
    .replace('__CSS__', () => readFileSync(join(root, 'assets/evidence_viewer.css'), 'utf8'))
    .replace('__DATA__', () => data)
    .replace('__JS__', () => js);
}

const results = [];
const check = (name, ok, detail = '') => {
  results.push(ok);
  console.log(`${ok ? 'PASS' : 'FAIL'}  ${name}${detail ? '  — ' + detail : ''}`);
};

const html = buildViewer();
const server = createServer((req, res) => {
  res.setHeader('Content-Type', 'text/html; charset=utf-8');
  res.end(html);
});
await new Promise((r) => server.listen(0, '127.0.0.1', r));
const base = `http://127.0.0.1:${server.address().port}/`;
const browser = await chromium.launch();
try {
  const page = await browser.newPage();
  const errors = [];
  page.on('pageerror', (e) => errors.push(String(e)));
  await page.goto(base);
  await page.waitForSelector('.card');
  await page.locator('.card').first().click();
  await page.evaluate(() => document.activeElement?.blur());

  // No video loaded: toast, no crash.
  await page.keyboard.press('.');
  await page.keyboard.press('Shift+L');
  const noVideoToast = await page.evaluate(() => document.getElementById('toastHost').textContent);
  check('key without playable media shows a toast', noVideoToast.length > 0, noVideoToast);

  // j/k stay record navigation.
  const before = await page.evaluate(() => document.querySelector('.card.active')?.dataset.id);
  await page.keyboard.press('j');
  const afterJ = await page.evaluate(() => document.querySelector('.card.active')?.dataset.id);
  await page.keyboard.press('k');
  const afterK = await page.evaluate(() => document.querySelector('.card.active')?.dataset.id);
  check('j/k still navigate records', afterJ !== before && afterK === before, `${before} ${afterJ} ${afterK}`);

  // Inject a real (webm) video generated in-page.
  const ok = await page.evaluate(async () => {
    const canvas = document.createElement('canvas'); canvas.width = 64; canvas.height = 48;
    const ctx = canvas.getContext('2d');
    const rec = new MediaRecorder(canvas.captureStream(30), { mimeType: 'video/webm' });
    const chunks = []; rec.ondataavailable = (e) => chunks.push(e.data);
    const done = new Promise((r) => (rec.onstop = r));
    rec.start();
    for (let i = 0; i < 40; i++) { ctx.fillStyle = `hsl(${i * 9},80%,50%)`; ctx.fillRect(0, 0, 64, 48); await new Promise((r) => setTimeout(r, 50)); }
    rec.stop(); await done;
    const v = document.createElement('video');
    v.muted = true; v.loop = true;
    v.src = URL.createObjectURL(new Blob(chunks, { type: 'video/webm' }));
    document.getElementById('mediaStage').replaceChildren(v);
    await new Promise((r) => (v.onloadeddata = r));
    v.currentTime = 0.5;
    await new Promise((r) => setTimeout(r, 200));
    return Number.isFinite(v.currentTime);
  });
  check('test video ready', ok);
  const time = () => page.evaluate(() => document.querySelector('#mediaStage video').currentTime);
  const settle = () => page.waitForTimeout(250);

  const t0 = await time();
  await page.keyboard.press('.'); await settle();
  const t1 = await time();
  check('"." steps forward ~1/fps', Math.abs(t1 - t0 - 1 / 30) < 0.02, `${t0} -> ${t1}`);
  await page.keyboard.press(','); await settle();
  const t2 = await time();
  check('"," steps back ~1/fps', Math.abs(t1 - t2 - 1 / 30) < 0.02, `${t1} -> ${t2}`);

  await page.keyboard.press('Shift+L');
  await page.keyboard.press('Shift+L');
  await page.waitForTimeout(100);
  const rate = await page.evaluate(() => document.querySelector('#mediaStage video').playbackRate);
  check('Shift+L twice -> playbackRate 2', rate === 2, String(rate));
  await page.keyboard.press('Shift+K'); await settle();
  check('Shift+K pauses', await page.evaluate(() => document.querySelector('#mediaStage video').paused));

  await page.keyboard.press('Shift+J'); await page.waitForTimeout(600);
  await page.keyboard.press('Shift+K');
  const tr = await time();
  check('Shift+J moves backwards', tr < t2 - 0.1, `${t2} -> ${tr}`);

  {
    const g0 = await time();
    await page.keyboard.press('Control+.'); await page.keyboard.press('Meta+.'); await page.keyboard.press('Alt+.'); await settle();
    check('Ctrl/Meta/Alt+. do nothing', Math.abs((await time()) - g0) < 0.001);
    await page.locator('#query').focus();
    await page.keyboard.type(',.LJK');
    check('keys in a text input do nothing', Math.abs((await time()) - g0) < 0.001
      && (await page.evaluate(() => document.querySelector('#mediaStage video').paused)) && (await page.locator('#query').inputValue()) === ',.LJK');
  }
  check('no page errors', !errors.length, errors.join(' | '));
} finally {
  await browser.close();
  server.close();
}
const failed = results.filter((r) => !r).length;
console.log(failed ? `\n${failed} check(s) failed` : '\nall checks passed');
process.exit(failed ? 1 : 0);
