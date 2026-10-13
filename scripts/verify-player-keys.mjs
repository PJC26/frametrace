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
import { viewerPageHtml } from './lib/app-bundle.mjs';

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
  const videos = ['a', 'b', 'c', 'd', 'e'].map((n) => ({
    id: `vid_${n}`, relative_path: `DCIM/${n}.mp4`, source_path: `/case/DCIM/${n}.mp4`,
    ffprobe_ok: true, size_bytes: 1000, extension: 'mp4',
    ...(n === 'e' ? { ffprobe: { streams: [{ codec_type: 'video', avg_frame_rate: '0/0', r_frame_rate: '30000/1001' }] } } : {}),
    ...(n === 'd' ? { ffprobe: { streams: [{ codec_type: 'audio' }, { codec_type: 'video', avg_frame_rate: '25/1', r_frame_rate: '25/1' }] } } : {}),
  }));
  const data = `window.__FRAMETRACE_DATA__ = ${JSON.stringify({
    manifest: { case_id: 'FT-UNDO', title: 'undo' }, caseDir: '/case', scan: { videos },
    carveLog: [], filesystemLog: [], validationLog: [], anomalyLog: [], flsEntries: [],
    thumbs: {}, annotations: {}, deepfake: {}, telemetry: {}, proxies: [],
  })};`;
  // Standalone page exactly as build.rs assembles it (shared fragment included).
  return viewerPageHtml({ data, js: js, root });
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
  check('key without playable media shows the no-video toast', /재생 중인 영상이 없습니다|No video is playing/.test(noVideoToast) && !/셔틀|Shuttle/.test(noVideoToast), noVideoToast);

  // CapsLock alone (key "J"/"K", no Shift) must not shuttle; acts as j/k.
  {
    const c0 = await page.evaluate(() => document.querySelector('.card.active')?.dataset.id);
    await page.evaluate(() => { document.getElementById('toastHost').textContent = ''; });
    await page.evaluate(() => document.dispatchEvent(new KeyboardEvent('keydown', { key: 'L', bubbles: true })));
    const capsToast = await page.evaluate(() => document.getElementById('toastHost').textContent);
    check('CapsLock-only "L" does not shuttle', !/셔틀|Shuttle|재생 중인|No video/.test(capsToast), capsToast);
    await page.evaluate(() => document.dispatchEvent(new KeyboardEvent('keydown', { key: 'J', bubbles: true })));
    const c1 = await page.evaluate(() => document.querySelector('.card.active')?.dataset.id);
    check('CapsLock-only "J" navigates like j', c1 !== c0, `${c0} -> ${c1}`);
    await page.evaluate(() => document.dispatchEvent(new KeyboardEvent('keydown', { key: 'K', bubbles: true })));
  }

  // U6: a Korean IME reports the composed jamo ("ㅓ" for J) or "Process";
  // the physical key (event.code) decides. Same ignore rules as Latin keys.
  {
    const active = () => page.evaluate(() => document.querySelector('.card.active')?.dataset.id);
    const key = (init, target) => page.evaluate(([i, t]) => {
      const el = t ? document.querySelector(t) : document;
      if (t) el.focus();
      el.dispatchEvent(new KeyboardEvent('keydown', { bubbles: true, cancelable: true, ...i }));
      if (t) el.blur();
    }, [init, target || null]);
    const toastText = () => page.evaluate(() => document.getElementById('toastHost').textContent);
    const clearToasts = () => page.evaluate(() => { document.getElementById('toastHost').textContent = ''; });
    const i0 = await active();
    await key({ key: 'ㅓ', code: 'KeyJ' });
    const i1 = await active();
    check('IME "ㅓ" (KeyJ) navigates like j', i1 !== i0, `${i0} -> ${i1}`);
    await key({ key: 'Process', code: 'KeyK' });
    check('IME "Process" (KeyK) navigates like k', (await active()) === i0, `${i1} -> ${await active()}`);
    await key({ key: 'ㅓ', code: 'KeyJ', altKey: true });
    check('IME Alt+KeyJ is ignored (shell/browser key)', (await active()) === i0);
    await key({ key: 'ㅓ', code: 'KeyJ' }, '#query');
    check('IME key in a focused text input is ignored', (await active()) === i0 && (await page.locator('#query').inputValue()) === '');
    await clearToasts();
    await key({ key: 'ㅓ', code: 'KeyJ', shiftKey: true });
    check('IME Shift+KeyJ is the J shuttle (not navigation)', /셔틀|Shuttle|재생 중인 영상이 없습니다|No video is playing/.test(await toastText()) && (await active()) === i0, await toastText());
    await key({ key: 'Process', code: 'KeyK', shiftKey: true }); // stop the shuttle
    await clearToasts();
    const m0 = await active();
    await key({ key: 'Process', code: 'Digit1' });
    const markToast = await toastText();
    check('IME "Process" (Digit1) marks reviewed', /적용했습니다|Applied/.test(markToast) && (await active()) !== m0, markToast);
    await clearToasts();
    await key({ key: 'ㅋ', code: 'KeyZ', ctrlKey: true });
    check('IME Ctrl+KeyZ undoes', /실행 취소|Undone/.test(await toastText()), await toastText());
    await clearToasts();
    await page.evaluate(() => document.activeElement?.blur());
  }

  // j/k stay record navigation.
  const before =await page.evaluate(() => document.querySelector('.card.active')?.dataset.id);
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
    window.__tv = v; document.getElementById('mediaStage').replaceChildren(v);
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
  await page.keyboard.press('Space'); await settle();
  const rateAfterSpace = await page.evaluate(() => [document.querySelector('#mediaStage video').playbackRate, document.querySelector('#mediaStage video').paused, document.getElementById('playRate').value]);
  check('Space after forward shuttle pauses and restores chip rate', rateAfterSpace[1] && rateAfterSpace[0] === (Number(rateAfterSpace[2]) || 1), rateAfterSpace.join(','));
  await page.keyboard.press('Shift+L'); await page.keyboard.press('Shift+L');
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
  // 25 fps record (fps parsed from the raw ffprobe stream): step is 0.04 s.
  {
    await page.locator('#query').fill('');
    await page.locator('.card[data-id="vid_d"]').click();
    await page.evaluate(() => { document.activeElement?.blur(); document.getElementById('mediaStage').replaceChildren(window.__tv); window.__tv.pause(); window.__tv.currentTime = 0.5; });
    await settle();
    const f0 = await time();
    await page.keyboard.press('.'); await settle();
    const f1 = await time();
    check('25 fps record steps 0.04 s', Math.abs(f1 - f0 - 0.04) < 0.005, `${f0} -> ${f1}`);
    // 30000/1001 via r_frame_rate (avg is 0/0) and a record with no probe data.
    for (const [id, want, label] of [['vid_e', 1001 / 30000, 'NTSC 30000/1001 (avg 0/0)'], ['vid_a', 1 / 30, 'no ffprobe data -> 1/30 fallback']]) {
      await page.locator(`.card[data-id="${id}"]`).click();
      await page.evaluate(() => { document.activeElement?.blur(); document.getElementById('mediaStage').replaceChildren(window.__tv); window.__tv.pause(); window.__tv.currentTime = 0.5; });
      await settle();
      const g0 = await time();
      await page.keyboard.press('.'); await settle();
      const g1 = await time();
      check(`${label} steps ${want.toFixed(4)} s`, Math.abs(g1 - g0 - want) < 0.005, `${g0} -> ${g1}`);
    }
  }
  check('no page errors', !errors.length, errors.join(' | '));
} finally {
  await browser.close();
  server.close();
}
const failed = results.filter((r) => !r).length;
console.log(failed ? `\n${failed} check(s) failed` : '\nall checks passed');
process.exit(failed ? 1 : 0);
