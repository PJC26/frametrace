// UX-14 / UX-09 / PERF-05 regression check for the viewer:
//  - bulk mark above the threshold asks via an in-page dialog (no
//    window.confirm); Esc / Cancel leaves marks untouched, OK applies them
//  - clip export uses the dialog (IN/OUT editable), follows a background
//    {ok, job} response through /api/status and shows the clip SHA-256;
//    the old synchronous response still works; the button shows a busy
//    state and recovers after an error
//  - a thumbnail that fails to load is retried once, then a placeholder
//  - the zoom slider leaves "fit" mode instead of doing nothing
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
import { viewerBundleCss } from './lib/app-bundle.mjs';

const root = join(dirname(fileURLToPath(import.meta.url)), '..');
const require = createRequire(import.meta.url);
const { chromium } = require('playwright');

const N = 8;
function buildViewer() {
  const dir = join(root, 'assets/viewer');
  let js = '';
  for (const part of readdirSync(dir).filter((f) => f.endsWith('.js')).sort()) {
    js += readFileSync(join(dir, part), 'utf8');
    if (!js.endsWith('\n')) js += '\n';
  }
  const videos = Array.from({ length: N }, (_, i) => ({
    id: `vid_${i}`, relative_path: `DCIM/c${i}.mp4`, source_path: `/case/DCIM/c${i}.mp4`,
    ffprobe_ok: true, size_bytes: 1000, extension: 'mp4', duration_seconds: 60,
  }));
  const data = `window.__FRAMETRACE_DATA__ = ${JSON.stringify({
    manifest: { case_id: 'FT-DLG', title: 'dlg' }, caseDir: '/case', scan: { videos },
    carveLog: [], filesystemLog: [], validationLog: [], anomalyLog: [], flsEntries: [],
    thumbs: { vid_0: '/thumbs/broken.jpg' }, annotations: {}, deepfake: {}, telemetry: {}, proxies: [],
  })};`;
  return readFileSync(join(root, 'assets/evidence_viewer.html'), 'utf8')
    .replace('__CSS__', () => viewerBundleCss(root))
    .replace('__DATA__', () => data)
    .replace('__JS__', () => js);
}

const results = [];
const check = (name, ok, detail = '') => {
  results.push(ok);
  console.log(`${ok ? 'PASS' : 'FAIL'}  ${name}${detail ? '  — ' + detail : ''}`);
};

const html = buildViewer();
let thumbHits = 0;
const server = createServer((req, res) => {
  if (req.url.startsWith('/thumbs/')) { thumbHits += 1; res.statusCode = 404; res.end('no'); return; }
  res.setHeader('Content-Type', 'text/html; charset=utf-8');
  res.end(html);
});
await new Promise((r) => server.listen(0, '127.0.0.1', r));
const base = `http://127.0.0.1:${server.address().port}/`;
const browser = await chromium.launch();
try {
  const ctx = await browser.newContext();
  await ctx.addInitScript(() => {
    localStorage.setItem('ft.viewer.FT-DLG.ranges', JSON.stringify({ vid_1: { in: 5, out: 15 } }));
  });
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
  check('no page errors', !errors.length, errors.join(' | '));
} finally {
  await browser.close();
  server.close();
}
const failed = results.filter((r) => !r).length;
console.log(failed ? `\n${failed} check(s) failed` : '\nall checks passed');
process.exit(failed ? 1 : 0);
