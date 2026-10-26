// UX-14 / UX-09 / PERF-05 regression check for the viewer:
//  - bulk mark above the threshold asks via an in-page dialog (no
//    window.confirm); Esc / Cancel leaves marks untouched, OK applies them
//  - clip export uses the dialog (IN/OUT editable), follows a background
//    {ok, job} response through /api/status and shows the clip SHA-256;
//    the old synchronous response still works; the button shows a busy
//    state and recovers after an error
//  - a thumbnail that fails to load is retried once, then a placeholder
//  - the zoom slider leaves "fit" mode instead of doing nothing
//  - A6: an Enter that only commits an IME composition (isComposing, or
//    keyCode 229) never adds a tag, runs a palette command or confirms a
//    dialog; the next real Enter does
//
//   node scripts/verify-viewer-dialogs.mjs   (needs playwright via NODE_PATH)
import { readFileSync, readdirSync } from 'node:fs';
import { join, dirname } from 'node:path';
import { fileURLToPath } from 'node:url';
import { createServer } from 'node:http';
import { createRequire } from 'node:module';

// The secondary player tools (clip export, size ...) live in the "더보기"
// popover when the media column is narrow: open it first.
const openTools = (p) => p.evaluate(() => {
  const b = document.getElementById('btnMoreTools');
  const l = document.getElementById('moreToolsList');
  if (b && b.offsetParent && l && l.hidden) b.click();
});
import { viewerPageHtml } from './lib/app-bundle.mjs';

// FT_ASSETS_ROOT: build the page from another checkout's assets (regression runs).
const root = process.env.FT_ASSETS_ROOT || join(dirname(fileURLToPath(import.meta.url)), '..');
const require = createRequire(import.meta.url);
const { chromium } = require('playwright');

const N = 8;
const EVIL_CASE = 'FT"<img src=x onerror=window.__pwn=1>&';
function buildViewer(caseId = 'FT-DLG', names = null) {
  const evil = caseId !== 'FT-DLG' && !names;
  const dir = join(root, 'assets/viewer');
  let js = '';
  for (const part of readdirSync(dir).filter((f) => f.endsWith('.js')).sort()) {
    js += readFileSync(join(dir, part), 'utf8');
    if (!js.endsWith('\n')) js += '\n';
  }
  const videos = Array.from({ length: names ? names.length : N }, (_, i) => ({
    id: `vid_${i}`, relative_path: names ? `DCIM/${names[i]}` : evil ? `DCIM/20260910_12000${i}_F.mp4` : `DCIM/c${i}.mp4`, source_path: `/case/DCIM/c${i}.mp4`,
    ffprobe_ok: true, size_bytes: 1000, extension: 'mp4', duration_seconds: 60,
  }));
  const data = `window.__FRAMETRACE_DATA__ = ${JSON.stringify({
    manifest: { case_id: caseId, title: 'dlg' }, caseDir: '/case', scan: { videos },
    carveLog: [], filesystemLog: [], validationLog: [], anomalyLog: [], flsEntries: [],
    thumbs: evil ? {} : { vid_0: '/thumbs/broken.jpg' },
    // vid_1's IN/OUT is a case annotation (R4 A2: ranges hydrate from the
    // case data like marks; the old browser-only range store is retired).
    annotations: caseId === 'FT-DLG' ? { marks: [{ record_id: 'vid_1', status: 'noted', marked_unix: 1, note: '', range: { in: 5, out: 15 } }], tags: [], revisions: { vid_1: 1 } } : {},
    deepfake: {}, telemetry: {}, proxies: [],
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
const evilHtml = buildViewer(EVIL_CASE);
// A device-clock time that does not exist in Europe/Berlin (DST spring
// forward 2026-03-29 02:00 → 03:00): it must read as written.
const dstHtml = buildViewer('FT-DST', ['20260329_023000_F.mp4']);
let thumbHits = 0;
const server = createServer((req, res) => {
  if (req.url.startsWith('/thumbs/')) { thumbHits += 1; res.statusCode = 404; res.end('no'); return; }
  res.setHeader('Content-Type', 'text/html; charset=utf-8');
  res.end(req.url === '/evil' ? evilHtml : req.url === '/dst' ? dstHtml : html);
});
await new Promise((r) => server.listen(0, '127.0.0.1', r));
const base = `http://127.0.0.1:${server.address().port}/`;
const browser = await chromium.launch();
try {
  const ctx = await browser.newContext();
  const page = await ctx.newPage();
  const errors = [];
  const nativeDialogs = [];
  page.on('pageerror', (e) => errors.push(String(e)));
  page.on('dialog', (d) => { nativeDialogs.push(d.type()); d.dismiss(); });
  await page.goto(base);
  await page.waitForSelector('.card');

  // --- thumbnail retry then placeholder -------------------------------
  await page.waitForFunction(() => !document.querySelector('.card .thumb img') && document.querySelector('.card .thumb .ph'), null, { timeout: 5000 }).catch(() => {});
  check('failed thumbnail becomes a placeholder (no broken img)', await page.evaluate(() => !document.querySelector('.card .thumb img')));
  check('thumbnail load is retried a bounded number of times (no loop)', thumbHits >= 2 && thumbHits <= 3, String(thumbHits));

  // --- bulk mark dialog ------------------------------------------------
  await page.locator('.card').first().click();
  await page.evaluate(() => document.activeElement?.blur());
  await page.keyboard.press('Control+a');
  await page.locator('#btnMarkReviewed').click();
  check('bulk mark opens the in-page dialog', await page.locator('.modal.dlg').count() === 1);
  check('dialog is aria-modal with focus inside', await page.evaluate(() => document.querySelector('.modal.dlg').getAttribute('aria-modal') === 'true' && document.querySelector('.modal.dlg').contains(document.activeElement)));
  await page.keyboard.press('Escape');
  check('Esc cancels the dialog', await page.locator('.modal.dlg').count() === 0);
  const marksAfterEsc = await page.evaluate(() => document.querySelectorAll('.card .mark-chip').length);
  check('Esc leaves marks untouched', marksAfterEsc === 0, String(marksAfterEsc));
  await page.locator('#btnMarkReviewed').click();
  await page.locator('.modal.dlg button.primary').click();
  await page.waitForTimeout(150);
  const marked = await page.evaluate(() => document.querySelectorAll('.card .mark-chip').length);
  check('OK applies the mark to the whole selection', marked === N, String(marked));
  check('no native confirm/prompt/alert used', nativeDialogs.length === 0, nativeDialogs.join(','));

  // --- clip export: dialog + job + sha ---------------------------------
  await page.locator('.card[data-id="vid_1"]').click();
  await page.evaluate(() => document.activeElement?.blur());
  let exportBody = null;
  let statusCalls = 0;
  // Real contract (src/serve/media.rs): the POST answers with a running
  // job; progress via /api/export-clip-status?job=ID; the finished job
  // carries the case-relative path and the clip's SHA-256.
  await page.route('**/api/export-clip', (route) => {
    exportBody = JSON.parse(route.request().postData());
    route.fulfill({ contentType: 'application/json', body: JSON.stringify({ ok: true, deduped: false, job: { id: 'clip-1', kind: 'export-clip', state: 'running', elapsed_secs: 0, bytes_written: 0, path: 'artifacts/clips/clip.mp4', sha256: null } }) });
  });
  await page.route('**/api/export-clip-status*', (route) => {
    statusCalls += 1;
    const url = new URL(route.request().url());
    const job = statusCalls < 3
      ? { id: 'clip-1', state: 'running', elapsed_secs: statusCalls, bytes_written: 524288 * statusCalls, path: 'artifacts/clips/clip.mp4', sha256: null }
      : { id: 'clip-1', state: 'done', elapsed_secs: 3, bytes_written: 1500000, size_bytes: 1500000, path: 'artifacts/clips/clip.mp4', sha256: 'ab'.repeat(32) };
    const body = url.searchParams.get('job') === 'clip-1' ? { ok: true, job } : { ok: false, error: 'unknown job' };
    route.fulfill({ contentType: 'application/json', body: JSON.stringify(body) });
  });
  await openTools(page);
  await page.locator('#btnExportClip').click();
  check('export asks IN/OUT in a dialog', await page.locator('.modal.dlg input').count() === 2);
  check('IN/OUT prefilled from the marked range', (await page.locator('.modal.dlg input').nth(0).inputValue()) === '5.000' && (await page.locator('.modal.dlg input').nth(1).inputValue()) === '15.000');
  await page.locator('.modal.dlg input').nth(0).fill('0:06.5');
  await page.keyboard.press('Enter');
  await page.waitForFunction(() => document.querySelector('#btnExportClip')?.classList.contains('busy'), null, { timeout: 3000 }).catch(() => {});
  check('button shows busy state while the job runs', await page.evaluate(() => document.querySelector('#btnExportClip').disabled && document.querySelector('#btnExportClip').getAttribute('aria-busy') === 'true'));
  await page.waitForFunction(() => /KB|MB/.test(document.querySelector('.clip-progress')?.textContent || ''), null, { timeout: 4000 }).catch(() => {});
  check('progress row shows elapsed + bytes written, with a cancel button', await page.evaluate(() => {
    const row = document.querySelector('.clip-progress');
    return !!row && /KB|MB/.test(row.textContent) && !!row.querySelector('button');
  }));
  await page.waitForSelector('.modal.dlg .dlg-msg', { timeout: 8000 });
  const summary = await page.locator('.modal.dlg .dlg-msg').innerText();
  check('request used the edited IN and the right duration', exportBody && exportBody.start === '6.500' && exportBody.duration === '8.500', JSON.stringify(exportBody));
  check('exhibit summary shows the clip path and SHA-256', summary.includes('ab'.repeat(32)) && summary.includes('artifacts/clips/clip.mp4'), summary);
  check('progress row removed when the job ends', await page.locator('.clip-progress').count() === 0);
  await page.locator('.modal.dlg button.primary').click();
  check('button re-enabled after the job', await page.evaluate(() => !document.querySelector('#btnExportClip').disabled && !document.querySelector('#btnExportClip').classList.contains('busy')));

  // cancel: POST /api/export-clip-cancel {job}, the job ends "cancelled"
  let cancelBody = null;
  await page.unroute('**/api/export-clip-status*');
  await page.route('**/api/export-clip-cancel', (route) => {
    cancelBody = JSON.parse(route.request().postData());
    route.fulfill({ contentType: 'application/json', body: JSON.stringify({ ok: true }) });
  });
  await page.route('**/api/export-clip-status*', (route) => {
    const job = cancelBody
      ? { id: 'clip-1', state: 'cancelled', elapsed_secs: 2, error: '사용자가 클립 내보내기를 중단했습니다.' }
      : { id: 'clip-1', state: 'running', elapsed_secs: 1, bytes_written: 1000 };
    route.fulfill({ contentType: 'application/json', body: JSON.stringify({ ok: true, job }) });
  });
  await openTools(page);
  await page.locator('#btnExportClip').click();
  await page.locator('.modal.dlg button.primary').click();
  await page.waitForSelector('.clip-progress button', { timeout: 4000 });
  await page.locator('.clip-progress button').click();
  await page.waitForFunction(() => !document.querySelector('#btnExportClip').disabled, null, { timeout: 6000 }).catch(() => {});
  check('cancel posts the job id', cancelBody && cancelBody.job === 'clip-1', JSON.stringify(cancelBody));
  const cancelState = await page.evaluate(() => ({
    toasts: document.getElementById('toastHost').textContent,
    summary: !!document.querySelector('.modal.dlg .dlg-msg'),
  }));
  check('cancelled export says so (no summary, no failure)',
    /클립 내보내기를 중단/.test(cancelState.toasts) && !/내보내기 실패/.test(cancelState.toasts) && !cancelState.summary, JSON.stringify(cancelState));
  await page.unroute('**/api/export-clip-status*');
  await page.unroute('**/api/export-clip-cancel');

  // legacy synchronous response still works
  await page.unroute('**/api/export-clip');
  await page.route('**/api/export-clip', (route) => route.fulfill({ contentType: 'application/json', body: JSON.stringify({ ok: true, path: '/case/exports/old.mp4' }) }));
  await openTools(page);
  await page.locator('#btnExportClip').click();
  await page.locator('.modal.dlg button.primary').click();
  await page.waitForSelector('.modal.dlg .dlg-msg');
  check('old sync response tolerated (no sha -> honest note)', (await page.locator('.modal.dlg .dlg-msg').innerText()).includes('/case/exports/old.mp4'));
  await page.locator('.modal.dlg button.primary').click();

  // server error: toast + button recovers
  await page.unroute('**/api/export-clip');
  await page.route('**/api/export-clip', (route) => route.fulfill({ contentType: 'application/json', body: JSON.stringify({ ok: false, error: 'ffmpeg missing' }) }));
  await openTools(page);
  await page.locator('#btnExportClip').click();
  await page.locator('.modal.dlg button.primary').click();
  await page.waitForFunction(() => /ffmpeg missing/.test(document.getElementById('toastHost').textContent), null, { timeout: 3000 }).catch(() => {});
  check('failed export shows the server error', /ffmpeg missing/.test(await page.evaluate(() => document.getElementById('toastHost').textContent)));
  check('button usable again after an error', await page.evaluate(() => !document.querySelector('#btnExportClip').disabled));

  // bad range is rejected without a request
  await openTools(page);
  await page.locator('#btnExportClip').click();
  await page.locator('.modal.dlg input').nth(1).fill('1');
  await page.locator('.modal.dlg button.primary').click();
  check('OUT before IN is rejected in the dialog flow', await page.evaluate(() => /IN|OUT/.test(document.getElementById('toastHost').textContent)));

  // --- zoom slider ------------------------------------------------------
  await page.evaluate(() => { const z = document.getElementById('videoZoom'); z.value = '150'; z.dispatchEvent(new Event('input', { bubbles: true })); });
  const zoomState = await page.evaluate(() => [document.getElementById('videoMode').value, document.getElementById('mediaStage').classList.contains('mode-zoom')]);
  check('zoom slider leaves fit mode', zoomState[0] === 'custom' && zoomState[1] === true, zoomState.join(','));
  await openTools(page);
  await page.selectOption('#videoMode', '50');
  check('preset size drives the slider', (await page.locator('#videoZoom').inputValue()) === '50');

  // --- ft-reload with unsaved marks: refused + acked, page not reloaded ----
  // (this fixture's /api/import-marks never succeeds, so marks stay pending)
  const reloadAck = await page.evaluate(() => new Promise((resolve) => {
    window.__noReload = 1;
    const onMsg = (e) => { if (e.data && e.data.type === 'ft-reload-ack') { window.removeEventListener('message', onMsg); resolve(e.data); } };
    window.addEventListener('message', onMsg);
    window.postMessage({ type: 'ft-reload', reason: 'carve' }, '*');
    setTimeout(() => resolve(null), 4000);
  }));
  await page.waitForTimeout(500);
  const stillHere = await page.evaluate(() => window.__noReload === 1 && document.getElementById('saveState')?.dataset.state);
  check('ft-reload with unsaved marks is refused and acked (reloaded:false)', reloadAck && reloadAck.reloaded === false && reloadAck.reason === 'unsaved', JSON.stringify(reloadAck));
  check('page kept (no reload), save state still not clean', stillHere && stillHere !== 'saved', String(stillHere));
  check('refusal is reported to the examiner', /저장되지 않은 판독 내용/.test(await page.evaluate(() => document.getElementById('toastHost').textContent)));

  // --- shell → viewer theme/locale handshake (findings 11) ---------------
  const acks = await page.evaluate(async () => {
    const got = [];
    window.addEventListener('message', (e) => { if (e.data && /-ack$/.test(e.data.type)) got.push(e.data); });
    const before = document.documentElement.lang;
    window.postMessage({ type: 'ft-locale', locale: before === 'en' ? 'ko' : 'en' }, '*');
    window.postMessage({ type: 'ft-locale', locale: 'xx' }, '*');
    window.postMessage({ type: 'ft-theme', theme: 'dark' }, '*');
    window.postMessage({ type: 'ft-theme', theme: 'purple' }, '*');
    await new Promise((r) => setTimeout(r, 300));
    return { got, before, after: document.documentElement.lang, theme: document.documentElement.dataset.theme };
  });
  check('ft-locale switches the viewer language and is acked', acks.after !== acks.before && acks.got.some((m) => m.type === 'ft-locale-ack' && m.locale === acks.after), JSON.stringify(acks));
  check('ft-theme applies and is acked; invalid values are ignored and not acked',
    acks.theme === 'dark' && acks.got.some((m) => m.type === 'ft-theme-ack' && m.theme === 'dark') && acks.got.length === 2, JSON.stringify(acks));

  // --- U13: Esc closes ONE layer per press, topmost first ----------------
  // (shortcut sheet > newest toast > theater). U12 (viewer side): keys do
  // not act behind an open viewer modal.
  {
    await page.evaluate(() => document.activeElement?.blur());
    const toastCount = () => page.evaluate(() => document.querySelectorAll('#toastHost .toast:not(.clip-progress)').length);
    const theater = () => page.evaluate(() => document.getElementById('ftViewer').classList.contains('theater'));
    const modalOpen = () => page.evaluate(() => !document.getElementById('shortcutsModal').hidden);
    // Isolate the toast layer (PR #4 Linux CI flake): this fixture's
    // /api/import-marks never succeeds, so the viewer's autosave keeps
    // retrying and adds a "케이스 DB 저장 실패" toast on its own schedule,
    // and an info toast expires after 3.2 s — on a slower runner either
    // lands between two Esc presses, so a count or "the newest toast" is
    // not this test's. The viewer is right; the measurement was not. Here:
    // start from an empty host, let only this test's toasts in while the
    // Esc checks run (restored below), and use error toasts (no expiry).
    await page.evaluate(() => {
      const host = document.getElementById('toastHost');
      host.querySelectorAll('.toast').forEach((el) => { clearTimeout(el._ftTimer); el.remove(); });
      const own = /esc-layer|focus-older|focus-newer/;
      const keep = (n) => !(n && n.classList && n.classList.contains('toast')) || own.test(n.textContent || '');
      const ap = host.appendChild;
      const ib = host.insertBefore;
      host.appendChild = function (n) { return keep(n) ? ap.call(this, n) : n; };
      host.insertBefore = function (n, r) { return keep(n) ? ib.call(this, n, r) : n; };
      window.__ftToastIsolationEnd = () => { delete host.appendChild; delete host.insertBefore; };
    });
    await page.keyboard.press('t');
    await page.evaluate(() => { FTViewer.toast('esc-layer-1', 'error'); FTViewer.toast('esc-layer-2', 'error'); });
    await page.keyboard.press('Shift+Slash');
    check('"?" opens the shortcut sheet over theater + a toast', (await modalOpen()) && (await theater()) && (await toastCount()) > 0);
    const a0 = await page.evaluate(() => document.querySelector('.card.active')?.dataset.id);
    await page.evaluate(() => document.dispatchEvent(new KeyboardEvent('keydown', { key: 'j', bubbles: true, cancelable: true })));
    check('j does nothing behind the open shortcut sheet', (await page.evaluate(() => document.querySelector('.card.active')?.dataset.id)) === a0);
    const t0 = await toastCount();
    await page.keyboard.press('Escape');
    check('1st Esc closes only the shortcut sheet', !(await modalOpen()) && (await theater()) && (await toastCount()) === t0, `toasts ${t0} -> ${await toastCount()}`);
    await page.evaluate(() => document.activeElement?.blur());
    await page.keyboard.press('Escape');
    const texts = () => page.evaluate(() => [...document.querySelectorAll('#toastHost .toast:not(.clip-progress)')].map((el) => el.textContent));
    check('2nd Esc dismisses one toast (the newest), theater stays', (await toastCount()) === t0 - 1 && (await theater()) && !(await texts()).some((s) => s.includes('esc-layer-2')) && (await texts()).some((s) => s.includes('esc-layer-1')), `toasts ${t0} -> ${await toastCount()} ${JSON.stringify(await texts())}`);
    for (let i = 0; i < 10 && (await toastCount()) > 0; i++) await page.keyboard.press('Escape');
    check('theater survives until the toasts are gone', await theater());
    await page.keyboard.press('Escape');
    check('last Esc leaves theater', !(await theater()));
    // F7: focus inside a toast — Esc dismisses THAT toast (not the newest)
    // and focus moves to the remaining toast, then back to the grid.
    await page.evaluate(() => { FTViewer.toast('focus-older', 'error'); FTViewer.toast('focus-newer', 'error'); });
    await page.locator('#toastHost .toast', { hasText: 'focus-older' }).locator('button').first().focus();
    await page.keyboard.press('Escape');
    const f7 = await page.evaluate(() => ({
      texts: [...document.querySelectorAll('#toastHost .toast')].map((t) => t.textContent),
      focusInToast: document.activeElement?.closest('.toast')?.textContent || '',
    }));
    check('Esc in a focused toast dismisses that toast and focuses the remaining one', !f7.texts.some((t) => t.includes('focus-older')) && f7.texts.some((t) => t.includes('focus-newer')) && f7.focusInToast.includes('focus-newer'), JSON.stringify(f7));
    await page.keyboard.press('Escape');
    // PR #4 Windows CI ({"n":0,"onCard":false}): the toast was gone and focus
    // had gone to the active card, but a grid rebuild that landed right after
    // (this fixture's autosave retries / the theater-exit resize render)
    // replaced the card node and focus fell to <body>. The viewer now keeps
    // grid focus across rebuilds (40-grid renderGridKeepFocus); the check
    // below forces such a rebuild deterministically instead of racing one.
    const f7b = await page.evaluate(() => ({ n: document.querySelectorAll('#toastHost .toast').length, onCard: !!document.activeElement?.closest('#recordGrid') }));
    check('Esc on the last focused toast returns focus to the grid', f7b.n === 0 && f7b.onCard, JSON.stringify(f7b));
    const f7c = await page.evaluate(() => {
      const card = document.querySelector('#recordGrid .card.active') || document.querySelector('#recordGrid .card');
      card.focus();
      const id = card.dataset.id;
      const sel = document.getElementById('sortBy');
      sel.dispatchEvent(new Event('change', { bubbles: true })); // render() → grid rebuilt
      const el = document.activeElement;
      return { id, rebuilt: !card.isConnected, focusedId: el?.closest?.('.card')?.dataset.id || el?.id || el?.tagName };
    });
    check('R11: a grid rebuild keeps keyboard focus on the same card (not <body>)', f7c.rebuilt && f7c.focusedId === f7c.id, JSON.stringify(f7c));
    await page.evaluate(() => window.__ftToastIsolationEnd && window.__ftToastIsolationEnd());
  }
  // --- A6: IME composition Enter is not a command -------------------------
  {
    // keydown Enter while composing: isComposing (standard) or keyCode 229
    // (what Chromium reports for keys the IME consumes).
    const imeEnter = (selector, kind) => page.evaluate(([sel, k]) => {
      const el = document.querySelector(sel);
      const ev = new KeyboardEvent('keydown', { key: 'Enter', code: 'Enter', bubbles: true, cancelable: true, isComposing: k === 'composing' });
      if (k === '229') Object.defineProperty(ev, 'keyCode', { get: () => 229 });
      el.dispatchEvent(ev);
    }, [selector, kind]);
    await page.locator('.card[data-id="vid_3"]').click();
    await page.waitForSelector('#customTagInput');
    await page.locator('#customTagInput').fill('합성태그');
    await imeEnter('#customTagInput', 'composing');
    await imeEnter('#customTagInput', '229');
    const kept = await page.evaluate(() => ({ value: document.getElementById('customTagInput')?.value, tagged: (document.querySelector('.card[data-id="vid_3"]')?.textContent || '').includes('합성태그') }));
    check('A6 composing Enter (isComposing / keyCode 229) in the tag input adds nothing', kept.value === '합성태그' && !kept.tagged, JSON.stringify(kept));
    await page.locator('#customTagInput').press('Enter');
    await page.waitForFunction(() => (document.querySelector('.card[data-id="vid_3"]')?.textContent || '').includes('합성태그'), null, { timeout: 3000 }).catch(() => {});
    check('A6 the next real Enter adds the tag once', await page.evaluate(() => (document.querySelector('.card[data-id="vid_3"]')?.textContent.match(/합성태그/g) || []).length) === 1);
    // Command palette: a composing Enter does not run the highlighted command.
    await page.evaluate(() => document.activeElement?.blur());
    await page.keyboard.press('Control+k');
    await page.waitForSelector('#cmdPalette:not([hidden])');
    await page.locator('#cmdPalette input').fill('btnShortcuts'); // matches the shortcuts-sheet command by id
    await imeEnter('#cmdPalette input', 'composing');
    await imeEnter('#cmdPalette input', '229');
    check('A6 composing Enter in the command palette runs nothing (palette stays open)', await page.evaluate(() => !document.getElementById('cmdPalette').hidden && document.getElementById('shortcutsModal').hidden));
    await page.keyboard.press('Escape');
    // In-page dialog: a composing Enter in its input does not confirm.
    await page.locator('.card[data-id="vid_1"]').click();
    await page.evaluate(() => document.activeElement?.blur());
    await openTools(page);
    await page.unroute('**/api/export-clip').catch(() => {});
    let clipPosts = 0;
    await page.route('**/api/export-clip', (route) => { clipPosts += 1; route.fulfill({ contentType: 'application/json', body: '{"ok":false,"error":"test"}' }); });
    await page.locator('#btnExportClip').click();
    await page.waitForSelector('.modal.dlg input');
    await imeEnter('.modal.dlg input', 'composing');
    await imeEnter('.modal.dlg input', '229');
    check('A6 composing Enter in a dialog input does not confirm it', await page.locator('.modal.dlg').count() === 1 && clipPosts === 0);
    await page.keyboard.press('Escape');
  }
  check('no page errors', !errors.length, errors.join(' | '));
  await ctx.close();

  // --- U10: a hostile case id and stored playback rate -------------------
  {
    const evilCtx = await browser.newContext({ acceptDownloads: true });
    await evilCtx.addInitScript((key) => {
      localStorage.setItem(key, JSON.stringify({ rate: '1;window.__pwn=2' }));
    }, `ft.viewer.${EVIL_CASE}.layout.v2`);
    const evil = await evilCtx.newPage();
    const evilErrors = [];
    evil.on('pageerror', (e) => evilErrors.push(String(e)));
    await evil.goto(base + 'evil');
    await evil.waitForSelector('.card');
    check('stored non-numeric playback rate is normalised to 1', (await evil.locator('#playRate').inputValue()) === '1');
    await evil.locator('.card').first().click();
    await evil.evaluate(() => document.activeElement?.blur());
    const [popup] = await Promise.all([evil.waitForEvent('popup'), evil.evaluate(() => FTViewer.popout())]);
    await popup.waitForLoadState('domcontentloaded');
    const pop = await popup.evaluate(() => ({ pwn: window.__pwn, rate: document.getElementById('vv').playbackRate }));
    check('pop-out player gets a numeric rate literal (no injected code)', pop.pwn === undefined && pop.rate === 1, JSON.stringify(pop));
    await popup.close();
    await evil.keyboard.press('Control+a');
    await openTools(evil);
    const [download] = await Promise.all([
      evil.waitForEvent('download'),
      evil.evaluate(() => document.getElementById('btnSummary').click()),
    ]);
    const summaryPath = await download.path();
    const summary = readFileSync(summaryPath, 'utf8');
    check('summary report escapes the case id (title and meta line)', !summary.includes('<img src=x') && (summary.match(/&lt;img src=x/g) || []).length >= 2, summary.slice(0, 200));
    check('summary download name has no path/markup characters', !/[<>:"\/\\|?*]/.test(download.suggestedFilename()), download.suggestedFilename());
    check('summary report times carry a basis label', /시간대 미상|time zone unknown|UTC[+-]\d\d:\d\d/.test(summary));
    check('no page errors (hostile case)', !evilErrors.length, evilErrors.join(' | '));
    await evilCtx.close();
  }

  // --- F3: naive device times are formatted from their parsed fields ------
  {
    const dstCtx = await browser.newContext({ timezoneId: 'Europe/Berlin', locale: 'ko-KR' });
    const dst = await dstCtx.newPage();
    const dstErrors = [];
    dst.on('pageerror', (e) => dstErrors.push(String(e)));
    await dst.goto(base + 'dst');
    await dst.waitForSelector('.card');
    // Precondition: the emulated zone really skips 02:30 (a local Date shifts it).
    const shifted = await dst.evaluate(() => new Date(2026, 2, 29, 2, 30, 0).getHours());
    check('precondition: Europe/Berlin skips 2026-03-29 02:30 (local Date shifts it)', shifted === 3, `hours=${shifted}`);
    await dst.locator('.card').first().click();
    const shown = await dst.evaluate(() => ({
      card: document.querySelector('#recordGrid .card .time-text')?.textContent || '',
      detail: document.getElementById('summaryList')?.textContent || '',
    }));
    check('card shows the device time as written (02:30, no DST shift)', shown.card.includes('2026-03-29 02:30'), shown.card);
    check('details show the device time as written (02:30:00, zone unknown)', shown.detail.includes('2026-03-29 02:30:00') && !shown.detail.includes('03:30'), shown.detail.slice(0, 160));
    check('no page errors (DST zone)', !dstErrors.length, dstErrors.join(' | '));
    await dstCtx.close();
  }
} finally {
  await browser.close();
  server.close();
}
const failed = results.filter((r) => !r).length;
console.log(failed ? `\n${failed} check(s) failed` : '\nall checks passed');
process.exit(failed ? 1 : 0);
