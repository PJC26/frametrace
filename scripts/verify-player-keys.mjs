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

function buildViewer(annotations = {}, nullCase = false, opts = {}) {
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
    manifest: nullCase || opts.nullManifest ? null : { case_id: 'FT-UNDO', title: 'undo' }, caseDir: opts.caseDir || '/case', scan: nullCase ? null : { videos },
    carveLog: [], filesystemLog: [], validationLog: [], anomalyLog: [], flsEntries: [],
    thumbs: {}, annotations, deepfake: {}, telemetry: {}, proxies: [],
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
// R6 U6: the same page with unreadable review records (read-only).
const htmlRo = buildViewer({ marks: [], tags: [], revisions: {}, error: 'database is locked' });
// R7 (S ↔ U): make-review embeds null for an unreadable case.json / index.
const htmlNull = buildViewer({}, true);
// R8 U1: case.json alone unreadable (the index is fine: records exist),
// two different case folders.
const htmlNoCaseX = buildViewer({}, false, { nullManifest: true, caseDir: 'C:/cases/case-x' });
const htmlNoCaseY = buildViewer({}, false, { nullManifest: true, caseDir: 'C:/cases/case-y' });
let captures = 0;
// '401' (R5 U8) | 'slow' (R6 U7: answers 200 after 1.5 s) | 'hang' (R7 U6).
let capMode = '401';
const hungCaptures = [];
const server = createServer((req, res) => {
  // R5 U8: the frame-capture POST is refused (401) like a stale cookie.
  if (req.url.startsWith('/api/capture-frame')) {
    captures += 1;
    res.setHeader('Content-Type', 'application/json');
    if (capMode === 'hang') {
      // R7 U6: the capture POST never answers (until released).
      hungCaptures.push(res);
      return;
    }
    if (capMode === 'slow') {
      setTimeout(() => res.end(JSON.stringify({ ok: true, path: 'artifacts/captures/frame_' + captures + '.jpg' })), 1500);
      return;
    }
    res.statusCode = 401;
    res.end('{"ok":false,"error":"unauthorized"}');
    return;
  }
  res.setHeader('Content-Type', 'text/html; charset=utf-8');
  res.end(req.url.startsWith('/ro') ? htmlRo : req.url.startsWith('/nullcase') ? htmlNull
    : req.url.startsWith('/nocase-x') ? htmlNoCaseX : req.url.startsWith('/nocase-y') ? htmlNoCaseY : html);
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
    // Cyrillic is a non-Latin script too: Russian "о" on KeyJ navigates.
    const r0 = await active();
    await key({ key: 'о', code: 'KeyJ' });
    check('Russian "о" (KeyJ) navigates like j', (await active()) !== r0);
    await key({ key: 'л', code: 'KeyK' });
    // Latin letters (accented ones included) keep key-based behaviour: a
    // layout's own character on a digit / letter key is not a shortcut.
    const markChips = () => page.evaluate(() => document.querySelectorAll('#recordGrid .card .mark-chip').length);
    const n0 = await markChips(), a0 = await active();
    for (const [k, c] of [['é', 'Digit2'], ['&', 'Digit1'], ['à', 'Digit0'], ['ü', 'BracketLeft'], ['ě', 'Digit2'], ['š', 'Digit3'], ['ı', 'KeyI'], ['å', 'BracketLeft']]) {
      await key({ key: k, code: c });
    }
    await page.waitForTimeout(100);
    check('Latin/accented keys on digit keys mark nothing (é Digit2, AZERTY & Digit1, ě, š, à)', (await markChips()) === n0 && (await active()) === a0 && !/적용했습니다|Applied/.test(await toastText()), `${n0} -> ${await markChips()} ${await toastText()}`);
    await clearToasts();
    // Ctrl+K / Ctrl+B through the same mapping: Russian Ctrl+"л" opens the
    // palette, Ctrl+"и" toggles the rail; Ctrl+B does nothing under a modal.
    const paletteOpen = () => page.evaluate(() => { const b = document.getElementById('cmdPalette'); return !!b && !b.hidden; });
    await page.evaluate(() => window.dispatchEvent(new KeyboardEvent('keydown', { key: 'л', code: 'KeyK', ctrlKey: true, bubbles: true, cancelable: true })));
    check('Russian Ctrl+"л" (KeyK) opens the command palette', await paletteOpen());
    await page.keyboard.press('Escape');
    check('Esc closes the palette', !(await paletteOpen()));
    await page.evaluate(() => document.activeElement?.blur());
    const railState = () => page.evaluate(() => document.getElementById('ftViewer').className);
    const rail0 = await railState();
    await key({ key: 'и', code: 'KeyB', ctrlKey: true });
    const rail1 = await railState();
    check('Russian Ctrl+"и" (KeyB) toggles the rail', rail1 !== rail0, `${rail0} -> ${rail1}`);
    await key({ key: 'и', code: 'KeyB', ctrlKey: true }); // back
    await page.keyboard.press('Shift+Slash');
    const rail2 = await railState();
    await key({ key: 'b', code: 'KeyB', ctrlKey: true });
    check('Ctrl+B does nothing while a viewer modal is open', (await railState()) === rail2 && await page.evaluate(() => !document.getElementById('shortcutsModal').hidden));
    await page.keyboard.press('Escape');
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
  // R5 U8: the pop-out's capture goes through the viewer's common POST door:
  // a 401 reads as the authorization refusal (and tells the host), never
  // as "server unreachable".
  {
    await page.evaluate(() => { window.__ftErr = []; FTViewer.on('error', (e) => window.__ftErr.push(e.detail)); });
    const [popup] = await Promise.all([page.waitForEvent('popup'), page.evaluate(() => FTViewer.popout())]);
    await popup.waitForFunction(() => typeof call === 'function');
    const res = await popup.evaluate(() => new Promise((r) => call('capture', { id: 'vid_a', image: 'AAAA', time: '0.00' }, r, 10000)));
    const heard = await page.evaluate(() => window.__ftErr);
    check('U8: pop-out capture 401 → authorization refusal via the bridge (not "offline")', res && res.ok === false && res.auth === true && /인증 오류/.test(res.error) && captures === 1, JSON.stringify(res));
    check('U8: the host hears the 401 (auth error event)', heard.some((d) => d.auth === true && d.stage === 'request'), JSON.stringify(heard));
    // R7 U6: through the OPENER (a direct __ftBridge call) a capture whose
    // POST never answers used to leave the pop-out waiting forever (the
    // opener path had no time limit). Now it times out like the channel
    // path, re-asks with the same request id (one POST), and — the main
    // viewer still answering the ping — says the reply is late, never
    // "the main viewer closed".
    capMode = 'hang';
    const beforeHang = captures;
    const hang = await popup.evaluate(async () => {
      const viaOpener = !!(window.opener && window.opener.__ftBridge);
      CAP_MS = 300;
      document.getElementById('note').textContent = '';
      const t0 = performance.now();
      capture({ id: 'vid_a', image: 'AAAA', time: '0.00', rid: 'r-hang-1' });
      for (let i = 0; i < 160 && !document.getElementById('note').textContent; i++) await new Promise((r) => setTimeout(r, 50));
      return { viaOpener, ms: performance.now() - t0, note: document.getElementById('note').textContent };
    }).catch((e) => ({ error: String(e) }));
    check('R7 U6: opener path + a capture that never answers → the pop-out stops waiting (time limit), one POST', hang.viaOpener && !!hang.note && hang.ms < 7000 && captures - beforeHang === 1, JSON.stringify({ ...hang, posts: captures - beforeHang }));
    check('R7 U6: the main viewer still answers → "응답이 늦습니다", not "메인 뷰어가 닫혔습니다"', /응답이 늦습니다/.test(hang.note || '') && !/닫혔습니다/.test(hang.note || ''), hang.note);
    capMode = '401';
    for (const res of hungCaptures.splice(0)) res.end('{"ok":false,"error":"released"}');
    await popup.evaluate(() => { CAP_MS = 30000; });
    // R6 U7: the main viewer no longer answers (its channel token does not
    // match, no opener): the capture falls back to the direct POST after
    // the short ping — not after a 30 s wait.
    const beforeDead = captures;
    const dead = await popup.evaluate(async () => {
      window.opener = null;
      window.__tok = TOKEN;
      TOKEN = 'nobody-answers';
      document.getElementById('note').textContent = '';
      const t0 = performance.now();
      capture({ id: 'vid_a', image: 'AAAA', time: '0.00', rid: 'r-dead-1' });
      for (let i = 0; i < 200 && !document.getElementById('note').textContent; i++) await new Promise((r) => setTimeout(r, 50));
      TOKEN = window.__tok;
      return { ms: performance.now() - t0, note: document.getElementById('note').textContent };
    }).catch((e) => ({ error: String(e) }));
    check('U7: a dead main viewer → direct POST right after the ping (< 5 s, not 30 s)', dead.ms < 5000 && /인증 오류/.test(dead.note || '') && captures - beforeDead === 1, JSON.stringify({ ...dead, posts: captures - beforeDead }));
    // R6 U7: a capture whose reply is late (slow server) is asked for again
    // with the same request id — the main viewer answers the first request:
    // one POST, one frame file, the path shown. (Through the BroadcastChannel:
    // the opener reference is already severed above.)
    capMode = 'slow';
    const before = captures;
    const late = await popup.evaluate(async () => {
      CAP_MS = 400;
      document.getElementById('note').textContent = '';
      capture({ id: 'vid_a', image: 'AAAA', time: '0.00', rid: 'r-late-1' });
      for (let i = 0; i < 160 && !/frame_/.test(document.getElementById('note').textContent); i++) await new Promise((r) => setTimeout(r, 50));
      await new Promise((r) => setTimeout(r, 1800)); // a duplicate would land meanwhile
      return { note: document.getElementById('note').textContent };
    }).catch((e) => ({ error: String(e) }));
    check('U7: a late capture reply is re-asked with the same request id — exactly one POST, the saved path shown', captures - before === 1 && /frame_/.test(late.note || ''), JSON.stringify({ ...late, posts: captures - before }));
    capMode = '401';
    await popup.close();
  }
  // R6 U6: in a read-only viewer the pop-out never claims "실행 취소됨" or
  // a mark that did not happen: the bridge's refusal reaches it.
  {
    const ro = await browser.newPage();
    ro.on('pageerror', (e) => errors.push(String(e)));
    await ro.goto(base + 'ro');
    await ro.waitForSelector('.card');
    await ro.locator('.card').first().click();
    const [popup] = await Promise.all([ro.waitForEvent('popup'), ro.evaluate(() => FTViewer.popout())]);
    await popup.waitForFunction(() => typeof call === 'function' && !!cur);
    const note = () => popup.evaluate(() => document.getElementById('note').textContent);
    await popup.evaluate(() => { document.getElementById('note').textContent = ''; doHist('undo'); });
    await popup.waitForFunction(() => !!document.getElementById('note').textContent, null, { timeout: 5000 }).catch(() => {});
    const undoNote = await note();
    check('U6: read-only → pop-out Ctrl+Z says the refusal, not "실행 취소됨"', /읽기 전용/.test(undoNote) && !/실행 취소됨/.test(undoNote), undoNote);
    await popup.evaluate(() => { document.getElementById('note').textContent = ''; doMark('important'); });
    await popup.waitForFunction(() => !!document.getElementById('note').textContent, null, { timeout: 5000 }).catch(() => {});
    const markNote = await note();
    const marked = await ro.evaluate(() => FTViewer.summary().marked);
    check('U6: read-only → pop-out mark is refused and says so (nothing marked)', /읽기 전용/.test(markNote) && marked === 0, `${markNote} marked=${marked}`);
    await popup.close();
    await ro.close();
  }
  // R7 (S ↔ U): case.json and the index were not valid JSON — the bundle
  // carries null for both. The viewer boots (no crash, no "data-bundle.js
  // not found") and says the case files were not read, instead of an
  // unexplained empty list.
  {
    const np = await browser.newPage();
    np.on('pageerror', (e) => errors.push('nullcase: ' + String(e)));
    await np.goto(base + 'nullcase');
    const banner = await np.waitForSelector('#caseInputErrorBanner', { timeout: 15000 }).then(() => np.evaluate(() => {
      const b = document.getElementById('caseInputErrorBanner');
      return { role: b.getAttribute('role'), text: b.innerText, problems: b.dataset.problems, cards: document.querySelectorAll('#recordGrid .card').length, overlay: !!document.querySelector('.ft-loader-error, .loader-error') };
    })).catch((e) => ({ error: String(e).split('\n')[0] }));
    check('R7 null case.json / index → role=alert "케이스 파일을 읽지 못했습니다" naming both, not a silent empty list',
      banner.role === 'alert' && /케이스 파일을 읽지 못했습니다/.test(banner.text || '') && /case\.json/.test(banner.text || '') && /증거 색인/.test(banner.text || '') && banner.problems === 'manifest scan' && banner.cards === 0, JSON.stringify(banner));
    await np.close();
  }
  // R8 U1: case.json unreadable but the index read (records listed). The
  // viewer is READ-ONLY (the R5 read-only path: no edit, no save), says so
  // in the one case-input alert, and its storage keys never collapse to
  // "case": each case folder gets its own key.
  {
    const keysOf = async (path) => {
      const pg = await browser.newPage();
      pg.on('pageerror', (e) => errors.push(path + ': ' + String(e)));
      await pg.goto(base + path);
      await pg.waitForSelector('#recordGrid .card', { timeout: 15000 });
      await pg.locator('#recordGrid .card').first().click();
      await pg.evaluate(() => { document.activeElement?.blur(); document.getElementById('toastHost').textContent = ''; });
      await pg.keyboard.press('1');
      await pg.waitForTimeout(300);
      const out = await pg.evaluate(() => ({
        marked: FTViewer.summary().marked,
        state: FTViewer.saveState(),
        toast: document.getElementById('toastHost').textContent,
        markBtnDisabled: !!document.getElementById('btnMarkReviewed')?.disabled,
        banner: document.getElementById('caseInputErrorBanner')?.innerText || '',
        problems: document.getElementById('caseInputErrorBanner')?.dataset.problems || '',
        annBanner: !!document.getElementById('annotationsErrorBanner'),
      }));
      // U6 too: the language switch re-renders the case-input alert.
      await pg.evaluate(() => document.getElementById('btnLang').click());
      await pg.waitForTimeout(200);
      out.bannerEn = await pg.evaluate(() => document.getElementById('caseInputErrorBanner')?.innerText || '');
      out.keys = await pg.evaluate(() => Object.keys(localStorage).filter((k) => k.startsWith('ft.')));
      await pg.close();
      return out;
    };
    const cx = await keysOf('nocase-x');
    const cy = await keysOf('nocase-y');
    check('R8 U1: case.json unreadable → read-only: key 1 marks nothing, the edit is refused with the case.json reason, save state "readonly"',
      cx.marked === 0 && cx.state === 'readonly' && /case\.json/.test(cx.toast) && cx.markBtnDisabled, JSON.stringify(cx));
    check('R8 U1: one alert (case input) says editing is blocked — no second read-only banner',
      cx.problems === 'manifest' && /편집·저장이 막혀/.test(cx.banner) && !cx.annBanner, JSON.stringify(cx));
    const caseOf = (keys) => [...new Set(keys.map((k) => (/^ft\.(?:viewer|draft)\.([^.]+)\./.exec(k) || /^ft\.alive:([^:]+):/.exec(k) || [])[1]).filter(Boolean))];
    const kx = caseOf(cx.keys), ky = caseOf(cy.keys);
    check('R8 U1: storage keys never use the literal "case" — a case-folder key, different per case folder',
      kx.length === 1 && ky.length === 1 && /^nocase-[0-9a-f]{16}$/.test(kx[0]) && /^nocase-[0-9a-f]{16}$/.test(ky[0]) && kx[0] !== ky[0] && ![...cx.keys, ...cy.keys].some((k) => /^ft\.(viewer|draft)\.case\.|^ft\.alive:case:/.test(k)),
      JSON.stringify({ kx, ky, keys: cx.keys }));
    check('R8 U6: the case-input alert follows the language switch (English after EN)', /could not be read/.test(cx.bannerEn) && !/읽지 못했습니다/.test(cx.bannerEn), cx.bannerEn);
  }
  // R8 U6 / U7: the read-only page's notices row — one short notice is not
  // a Tab stop (U7: only an overflowing row is), five long ones make it
  // one; the read-only notice follows the language switch (U6).
  {
    const pg = await browser.newPage({ viewport: { width: 1366, height: 768 } });
    pg.on('pageerror', (e) => errors.push('ro-notices: ' + String(e)));
    await pg.goto(base + 'ro');
    await pg.waitForSelector('#annotationsErrorBanner', { timeout: 15000 });
    const frame = () => pg.evaluate(() => new Promise((r) => requestAnimationFrame(() => requestAnimationFrame(r))));
    await frame();
    const one = await pg.evaluate(() => { const h = document.querySelector('#ftViewer > .ft-notices'); return { tab: h.getAttribute('tabindex'), overflow: h.scrollHeight > h.clientHeight + 1, role: h.getAttribute('role'), label: h.getAttribute('aria-label') }; });
    check('R8 U7: one short notice — the notices row is no Tab stop (still a labelled region)', one.tab === null && !one.overflow && one.role === 'region' && !!one.label, JSON.stringify(one));
    await pg.evaluate(() => {
      const h = document.querySelector('#ftViewer > .ft-notices');
      for (let i = 0; i < 5; i++) {
        const b = document.createElement('div');
        b.className = 'ft-banner'; b.dataset.probe = '1';
        b.textContent = '긴 알림 '.repeat(80);
        h.appendChild(b);
      }
    });
    await frame();
    const many = await pg.evaluate(() => { const h = document.querySelector('#ftViewer > .ft-notices'); return { tab: h.getAttribute('tabindex'), overflow: h.scrollHeight > h.clientHeight + 1 }; });
    check('R8 U7: an overflowing notices row becomes a Tab stop (tabindex 0) so the keyboard can scroll it', many.tab === '0' && many.overflow, JSON.stringify(many));
    await pg.evaluate(() => document.querySelectorAll('[data-probe]').forEach((b) => b.remove()));
    await frame();
    const back = await pg.evaluate(() => document.querySelector('#ftViewer > .ft-notices').getAttribute('tabindex'));
    check('R8 U7: back to one notice — no Tab stop again', back === null, String(back));
    const ko = await pg.evaluate(() => document.getElementById('annotationsErrorBanner').innerText);
    await pg.evaluate(() => document.getElementById('btnLang').click());
    await pg.waitForTimeout(200);
    const en = await pg.evaluate(() => document.getElementById('annotationsErrorBanner').innerText);
    check('R8 U6: the read-only notice follows the language switch', /판독 기록/.test(ko) && /could not be read/.test(en) && !/판독 기록/.test(en), `${ko} → ${en}`);
    await pg.close();
  }
  check('no page errors', !errors.length, errors.join(' | '));
} finally {
  await browser.close();
  server.close();
}
const failed = results.filter((r) => !r).length;
console.log(failed ? `\n${failed} check(s) failed` : '\nall checks passed');
process.exit(failed ? 1 : 0);
