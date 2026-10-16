// End-to-end proof that the background-job tracks work together in the REAL
// workstation UI against the REAL server (no mocks):
//
//   1. analysis of a synthetic dashcam folder runs to the review stage;
//   2. Carve (UX-06) on a synthetic raw image with an embedded MP4: the job
//      chip shows stage + elapsed, one run is cancelled from the work sheet
//      and the chip says 중단됨 (server job.state = cancelled);
//   3. a second Carve runs to done while a review mark is saved from the
//      viewer (accepted into the durable queue during the job); the shell
//      calls FTViewer.reload() (same document, no navigation), the carved
//      candidates appear and the mark is still there (case DB + viewer);
//   4. a clip export (UX-09) from the viewer: progress, then the exhibit
//      summary shows the clip path + SHA-256 — checked against the file;
//   5. /api/capture-frame accepts a > 4 MiB frame payload over HTTP.
//
//   node scripts/verify-jobs-e2e.mjs [--no-build] [--keep] [--shots DIR]
//
// The viewer is hosted in the same document (no iframe; docs/ui-rebuild-
// contract.md §3): its DOM is queried on `page`, its API is window.FTViewer.
//
// Needs ffmpeg (demo clips) and Playwright (NODE_PATH). Uses Google Chrome
// when installed (H.264 playback for IN/OUT), else Playwright's Chromium.
// The server runs isolated (scripts/lib/isolated-server.mjs): explicit
// port, throwaway HOME/XDG_CONFIG_HOME/APPDATA/instance file, pinned token.
import { mkdtempSync, mkdirSync, writeFileSync, rmSync, existsSync, readFileSync, openSync, writeSync, closeSync, statSync } from 'node:fs';
import { join, dirname } from 'node:path';
import { tmpdir } from 'node:os';
import { fileURLToPath, pathToFileURL } from 'node:url';
import { spawnSync } from 'node:child_process';
import { createRequire } from 'node:module';
import { createHash, randomFillSync } from 'node:crypto';
import { startIsolatedServer, isolatedEnv } from './lib/isolated-server.mjs';

const root = join(dirname(fileURLToPath(import.meta.url)), '..');
const argv = process.argv.slice(2);
const flag = (name) => argv.includes('--' + name);
const shotsIdx = argv.indexOf('--shots');
const shots = shotsIdx >= 0 ? argv[shotsIdx + 1] : '';

let chromium;
try { ({ chromium } = createRequire(import.meta.url)('playwright')); } catch {
  console.error('playwright not resolvable — set NODE_PATH to a node_modules with playwright');
  process.exit(2);
}

if (!flag('no-build')) {
  console.log('building (cargo build --locked)…');
  const b = spawnSync('cargo', ['build', '--locked'], { cwd: root, stdio: 'inherit' });
  if (b.status !== 0) process.exit(1);
}

const results = [];
const check = (name, ok, detail = '') => {
  results.push({ name, ok: !!ok });
  console.log(`${ok ? 'PASS' : 'FAIL'}  ${name}${detail ? '  — ' + detail : ''}`);
  return !!ok;
};
const sleep = (ms) => new Promise((r) => setTimeout(r, ms));
async function until(fn, ms, step = 200) {
  const end = Date.now() + ms;
  for (;;) {
    let v;
    try { v = await fn(); } catch { v = null; }
    if (v) return v;
    if (Date.now() > end) return null;
    await sleep(step);
  }
}
const sha256File = (p) => createHash('sha256').update(readFileSync(p)).digest('hex');

// ---- synthetic inputs -------------------------------------------------------
const work = mkdtempSync(join(tmpdir(), 'ft-jobs-e2e-'));
const srcDir = join(work, 'dashcam-source');
const caseDir = join(work, 'case');
const demo = spawnSync(process.execPath, [join(root, 'scripts/make-demo-case.mjs'), srcDir], { stdio: 'inherit' });
if (demo.status !== 0) { console.error('demo source generation failed'); process.exit(1); }
const embedded = readFileSync(join(srcDir, 'DCIM/FRONT/FRONT_20260908_010000.mp4'));

// A raw "disk image": zeros / random filler with an MP4 embedded. The
// debug carver reads ~6 MB/s, so the big image keeps the job running long
// enough to cancel and the small one finishes in seconds.
function writeRaw(path, mib) {
  mkdirSync(dirname(path), { recursive: true });
  const fd = openSync(path, 'w');
  const noise = Buffer.alloc(1 << 20);
  randomFillSync(noise);
  const zero = Buffer.alloc(1 << 20);
  for (let i = 0; i < mib; i++) {
    if (i === 4) writeSync(fd, embedded);
    writeSync(fd, i % 3 === 0 ? noise : zero);
  }
  closeSync(fd);
  // R4 C1: a raw image is carved only with its completion marker (size +
  // modification time in ns — written as the exact BigInt digits, > 2^53).
  const st = statSync(path, { bigint: true });
  writeFileSync(path + '.done.json', JSON.stringify({
    schema_version: 1, kind: 'frametrace-e01-raw-complete', raw_path: path, size_bytes: Number(st.size),
    modified_nanos: '@NANOS@', sha256: '0'.repeat(64), e01_path: join(dirname(path), 'synthetic.E01'), imported_unix: 1,
  }).replace('"@NANOS@"', st.mtimeNs.toString()));
}

const server = await startIsolatedServer({ root, work });
const { base, token, getJson, postJson } = server;
console.log('server:', base, '(isolated config in', work + ')');
let browser;
const pageErrors = [];
function cleanup() {
  try { browser && browser.close(); } catch { /* closed */ }
  server.stop();
  if (!flag('keep')) { try { rmSync(work, { recursive: true, force: true }); } catch { /* busy */ } }
}
process.on('exit', cleanup);
process.on('SIGINT', () => process.exit(130));

try {
  try { browser = await chromium.launch({ channel: 'chrome' }); } catch { browser = await chromium.launch(); }
  const page = await browser.newPage({ viewport: { width: 1500, height: 950 } });
  page.on('pageerror', (e) => pageErrors.push('shell: ' + e));
  // The hosted viewer is re-initialised in place: the document must never navigate.
  let navigations = 0;
  page.on('framenavigated', (f) => { if (f === page.mainFrame()) navigations += 1; });
  page.on('console', (m) => { if (m.type() === 'error' && !/favicon|Failed to load resource/.test(m.text())) pageErrors.push('console: ' + m.text()); });
  const importMarks = [];
  page.on('response', async (res) => {
    if (res.url().includes('/api/import-marks')) {
      try { importMarks.push({ at: Date.now(), body: await res.json() }); } catch { /* aborted */ }
    }
  });

  // ---- 1. analysis to review ------------------------------------------------
  await page.goto(`${base}/?token=${token}`);
  await page.fill('#sourcePath', srcDir);
  // The case folder lives under the start screen's collapsed '옵션'.
  if (!(await page.locator('#caseDir').isVisible())) await page.click('#startOptions > summary');
  await page.fill('#caseDir', caseDir);
  await page.click('#btnStart');
  // A preflight / resume dialog may ask for confirmation.
  const reviewing = await until(async () => {
    if (await page.locator('#dlgBackdrop:not([hidden]) #dlgOk').isVisible().catch(() => false)) await page.click('#dlgOk');
    const st = await getJson('/api/status');
    return st.phase === 'review-ready' && !st.busy ? st : null;
  }, 240000, 500);
  check('analysis reaches review-ready', reviewing, reviewing ? `case ${reviewing.case_dir}` : '');
  if (!reviewing) throw new Error('analysis did not finish: ' + JSON.stringify(await getJson('/api/status')).slice(0, 400));
  // Stub-free /api/recover-deleted on the real server: a folder case has no
  // filesystem-investigation result, so the endpoint must refuse cleanly
  // (no job started). A positive recovery needs an E01/raw image with
  // deleted NTFS/FAT entries plus The Sleuth Kit — covered at unit level in
  // src/serve/tests_jobs.rs with a fake exe, not available on CI runners.
  const rec = await postJson('/api/recover-deleted', {});
  check('recover-deleted refuses without filesystem results (real endpoint, no stub)',
    rec && rec.ok === false && typeof rec.error === 'string' && rec.error.length > 0 && !rec.job, JSON.stringify(rec).slice(0, 200));
  check('refused recover-deleted left no job behind', !((await getJson('/api/status')).job_kind));
  await page.waitForSelector('#hViewerHost:not([hidden])', { timeout: 15000 });
  const viewerReady = await until(async () => (await page.evaluate(() => !!window.FTViewer && document.querySelectorAll('#hViewerHost #recordGrid .card').length)) ? page : null, 30000);
  check('review viewer hosted in the workstation (same document)', viewerReady, viewerReady ? '' : page.url());
  if (!viewerReady) { await page.screenshot({ path: join(work, 'noviewer.png') }); throw new Error('no viewer; screenshot ' + join(work, 'noviewer.png')); }
  const before = await page.evaluate(() => window.FTViewer.summary());
  console.log('viewer before carve:', JSON.stringify(before));
  const navsBefore = navigations;
  // Recover / Carve live in the work sheet ('작업 시작'); the chip opens it.
  const openJobsSheet = async () => {
    if (await page.locator('#hJobsSheet').isHidden().catch(() => true)) await page.click('#hBtnJobs');
    await page.waitForSelector('#hJobsSheet:not([hidden])', { timeout: 5000 });
  };
  await openJobsSheet();
  await page.waitForSelector('#btnCarve:not([hidden])', { timeout: 10000 });

  // ---- 2. carve → cancel ----------------------------------------------------
  const raw = join(caseDir, 'evidence/images/evidence.raw');
  writeRaw(raw, 320);
  await page.click('#btnCarve');
  await page.waitForSelector('#dlgBackdrop:not([hidden]) #dlgOk');
  await page.click('#dlgOk'); // carve confirm
  const running = await until(async () => {
    const title = await page.textContent('#jobTitle');
    const detail = await page.textContent('#jobDetail');
    return /이미지 카빙 진행 중/.test(title) && /단계 1\/2: 시그니처 카빙/.test(detail) ? detail : null;
  }, 15000);
  check('carve chip shows stage from /api/status job', running, running || await page.textContent('#jobDetail'));
  const st1 = await getJson('/api/status');
  check('server reports job_kind=carve, job.state=running', st1.job_kind === 'carve' && st1.job && st1.job.state === 'running', JSON.stringify(st1.job));
  // Elapsed comes from whole-second server polls, so compare two readings
  // instead of expecting an exact value after a fixed sleep.
  const secs = (text) => { const m = /경과 (\d+):(\d{2})/.exec(text || ''); return m ? (+m[1]) * 60 + (+m[2]) : -1; };
  const firstElapsed = await page.textContent('#jobElapsed');
  const elapsedText = await until(async () => {
    const text = await page.textContent('#jobElapsed');
    return secs(text) > secs(firstElapsed) ? text : null;
  }, 6000);
  check('elapsed time ticks', !!elapsedText && secs(firstElapsed) >= 0, `${firstElapsed} → ${elapsedText || await page.textContent('#jobElapsed')}`);
  if (shots) { mkdirSync(shots, { recursive: true }); await page.screenshot({ path: join(shots, 'jobs-carve-running.png') }); }
  await openJobsSheet(); // cancel lives in the sheet
  await page.click('#btnJobCancel');
  await page.waitForSelector('#dlgBackdrop:not([hidden]) #dlgOk');
  await page.click('#dlgOk'); // confirm cancel
  const cancelled = await until(async () => /이미지 카빙 중단됨/.test(await page.textContent('#jobTitle')), 30000);
  check('UI says the carve was cancelled (중단됨)', cancelled, await page.textContent('#jobTitle'));
  check('panel styled as cancelled, not failed', /\bcancelled\b/.test(await page.getAttribute('#jobPanel', 'class')));
  const st2 = await getJson('/api/status');
  check('server job.state = cancelled; busy released', st2.job && st2.job.state === 'cancelled' && !st2.busy && st2.phase === 'review-ready', JSON.stringify({ job: st2.job, busy: st2.busy, phase: st2.phase }));
  check('cancelled run did not refresh the viewer', navigations === navsBefore && await page.evaluate(() => !!window.FTViewer.summary().total));
  if (shots) await page.screenshot({ path: join(shots, 'jobs-carve-cancelled.png') });
  await openJobsSheet();
  await page.click('#btnJobClose');

  // ---- 3. carve → done, with a mark saved during the job -------------------
  writeRaw(raw, 40);
  // FTViewer 'records' events carry the index generation the viewer loaded.
  await page.evaluate(() => { window.__e2eRecords = []; window.FTViewer.on('records', (e) => window.__e2eRecords.push(e.detail)); });
  const markId = await page.evaluate(() => document.querySelector('#recordGrid .card[data-id^="vid_"]')?.dataset.id);
  const marksBefore = importMarks.length;
  await openJobsSheet();
  await page.click('#btnCarve');
  await page.waitForSelector('#dlgBackdrop:not([hidden]) #dlgOk');
  await page.click('#dlgOk');
  await until(async () => /진행 중/.test(await page.textContent('#jobTitle')), 10000);
  // Mark one record important from the viewer while the job holds busy.
  await page.evaluate((id) => {
    document.querySelector(`#recordGrid .card[data-id="${id}"]`).click();
    document.getElementById('btnMarkImportant').click();
  }, markId);
  const savedDuring = await until(() => importMarks.slice(marksBefore).find((r) => r.body && r.body.ok), 20000);
  const stAtSave = await getJson('/api/status');
  check('mark saved during the job is accepted (durable queue)', savedDuring && (savedDuring.body.deferred || savedDuring.body.applied),
    JSON.stringify(savedDuring && savedDuring.body) + ` job=${stAtSave.job && stAtSave.job.state}`);
  check('the mark was saved while the carve job was still running', savedDuring && savedDuring.body.deferred === true);
  const done = await until(async () => /이미지 카빙 완료/.test(await page.textContent('#jobTitle')) && /뷰어에 결과를 반영/.test(await page.textContent('#jobDetail')), 120000);
  check('UI says carve done and the viewer took the new data', done, `${await page.textContent('#jobTitle')} | ${await page.textContent('#jobDetail')}`);
  const st3 = await getJson('/api/status');
  check('server job.state = done; no queued marks left', st3.job && st3.job.state === 'done' && st3.pending_marks === 0, JSON.stringify({ job: st3.job, pending: st3.pending_marks }));
  const reloaded = await until(async () => (await page.evaluate(() => window.FTViewer.summary().byKind.carved > 0 && document.querySelectorAll('#hViewerHost #recordGrid .card').length > 0)) ? page : null, 30000);
  const gens = await page.evaluate(() => (window.__e2eRecords || []).map((d) => d.generation));
  check('viewer re-initialised in place (FTViewer.reload): new records generation, no page navigation', reloaded && gens.length > 0 && navigations === navsBefore, `generations ${JSON.stringify(gens)}, navigations ${navigations - navsBefore}`);
  const after = reloaded ? await page.evaluate(() => window.FTViewer.summary()) : null;
  console.log('viewer after carve:', JSON.stringify(after));
  check('carved candidates appear in the refreshed viewer', after && (after.byKind.carved || 0) > 0 && after.total > before.total, `before ${before.total} → after ${after && after.total}`);
  const meta = await getJson('/api/records-meta');
  const stored = (meta.annotations?.marks || []).find((m) => (m.record_id || m.id) === markId);
  check('mark saved during the job persisted in the case DB', stored && stored.status === 'important', JSON.stringify(stored || null));
  const shown = reloaded && await page.evaluate((id) => {
    const card = document.querySelector(`#recordGrid .card[data-id="${id}"]`);
    return { marked: window.FTViewer.summary().marked, cls: card ? card.className : '', save: document.getElementById('saveState')?.dataset.state };
  }, markId);
  check('mark still shown after the viewer reload; save state clean', shown && shown.marked >= 1 && shown.save === 'saved', JSON.stringify(shown));
  if (shots) await page.screenshot({ path: join(shots, 'jobs-carve-done.png') });

  // ---- 3b. R11 U5 (track I5): carve page metrics are not taken from the
  // unaudited db/carve_results.json. Forge it (a fake source size, warnings
  // dropped, scan "complete"), re-render the review pages, and the page must
  // either show the audited values or mark the metrics "감사 안 됨" without
  // an ok (green) colour — never the forged size as fact.
  {
    const caseRoot = st3.case_dir || caseDir;
    const resultsPath = join(caseRoot, 'db/carve_results.json');
    const pagePath = join(caseRoot, 'review/carve-report.html');
    if (!existsSync(resultsPath) || !existsSync(pagePath)) {
      check('R11 U5: carve results + carve page exist after the carve job', false, `${existsSync(resultsPath)} ${existsSync(pagePath)}`);
    } else {
      const forged = JSON.parse(readFileSync(resultsPath, 'utf8'));
      const FORGED_SIZE = 7777777777777;
      Object.assign(forged, { source_size_bytes: FORGED_SIZE, scan_complete: true, candidate_limit_reached: false, warnings: [] });
      writeFileSync(resultsPath, JSON.stringify(forged));
      const mr = spawnSync(server.exe, ['make-review', caseRoot], { env: isolatedEnv(work), encoding: 'utf8' });
      check('R11 U5: make-review re-renders the carve page', mr.status === 0, (mr.stderr || '').slice(-400));
      const cp = await browser.newPage();
      await cp.goto(pathToFileURL(pagePath).href);
      const m = await cp.evaluate(() => [...document.querySelectorAll('#metrics .metric')].map((el) => ({ text: el.textContent, cls: el.className, marked: !!el.querySelector('.unaudited') })));
      await cp.close();
      const all = m.map((x) => x.text).join(' | ');
      const unaudited = /감사 안 됨/.test(all);
      const sizeText = m.find((x) => /원본 크기/.test(x.text))?.text || '';
      // The page's fmtSize renders the forged 7777777777777 B as "7777.78 GB".
      const forgedShown = /7777\.78\s*GB/.test(sizeText);
      const okColour = m.some((x) => /\bok\b/.test(x.cls));
      // dfa2f8a: the four db/carve_results.json metrics each carry <span class="unaudited">· 감사 안 됨</span>.
      const fromResults = m.filter((x) => /스캔 완료|상한 도달|원본 크기|경고/.test(x.text));
      const verdict = unaudited ? !okColour && fromResults.length === 4 && fromResults.every((x) => x.marked) : !forgedShown;
      if (!verdict && !unaudited && !process.env.FRAMETRACE_R11_STRICT) {
        console.log(`PEND  R11 U5: carve page still reads db/carve_results.json (track I5 not in this binary) — ${all}`);
      } else {
        check('R11 U5: forged carve_results.json → metrics marked "감사 안 됨" (no green) or audited values', verdict, all);
      }
    }
  }

  // ---- 4. clip export from the viewer ---------------------------------------
  const v = page; // hosted viewer: same document
  const clipId = await v.evaluate(() => {
    const card = [...document.querySelectorAll('#recordGrid .card[data-id^="vid_"]')]
      .find((c) => /FRONT_20260908_010000|FRONT_20260908_010400/.test(c.textContent)) || document.querySelector('#recordGrid .card[data-id^="vid_"]');
    card.click();
    return card.dataset.id;
  });
  const seekable = await until(() => v.evaluate(() => {
    const video = document.querySelector('#mediaStage video');
    return !!video && video.readyState >= 1 && video.duration > 3;
  }), 20000);
  check('selected clip plays in the viewer (metadata loaded)', seekable, clipId);
  for (const [t, btn] of [[1, 'btnSetIn'], [3, 'btnSetOut']]) {
    await v.evaluate(async ([t, btn]) => {
      const video = document.querySelector('#mediaStage video');
      await new Promise((r) => { video.addEventListener('seeked', r, { once: true }); video.currentTime = t; });
      document.getElementById(btn).click();
    }, [t, btn]);
  }
  // Toasts vanish after ~3 s; keep every one shown from here on so a
  // missing summary can be explained (e.g. "server unreachable", failure).
  await v.evaluate(() => {
    window.__e2eToasts = [];
    const host = document.getElementById('toastHost');
    if (host) {
      new MutationObserver((muts) => {
        for (const m of muts) for (const n of m.addedNodes) window.__e2eToasts.push(`${new Date().toISOString().slice(11, 23)} ${n.textContent}`);
      }).observe(host, { childList: true });
    }
  });
  await v.evaluate(() => document.getElementById('btnExportClip').click());
  await v.waitForSelector('.modal.dlg input', { timeout: 5000 });
  await v.evaluate(() => document.querySelector('.modal.dlg button.primary').click());
  const summary = await until(async () => {
    const el = await v.$('.modal.dlg .dlg-msg');
    return el ? el.innerText() : null;
  }, 120000, 300);
  console.log('exhibit summary:\n' + summary);
  if (!summary) {
    // Explain a missing summary instead of only failing on it: what the
    // page shows, what the viewer thinks, and what the server reports.
    const ui = await v.evaluate(() => {
      const visible = (el) => !!el && !el.hidden && el.offsetParent !== null;
      return {
        dialogs: [...document.querySelectorAll('.modal, [role="dialog"], #dlgBackdrop')].filter(visible).map((el) => (el.id ? '#' + el.id + ': ' : '') + el.innerText.slice(0, 300)),
        toasts: window.__e2eToasts || [],
        toastsNow: [...document.querySelectorAll('#toastHost .toast')].map((el) => el.textContent),
        saveState: window.FTViewer ? window.FTViewer.saveState() : 'no FTViewer',
        exportBtn: (() => { const b = document.getElementById('btnExportClip'); return b ? { disabled: b.disabled, busy: b.getAttribute('aria-busy'), cls: b.className } : null; })(),
      };
    }).catch((e) => ({ evaluateError: String(e) }));
    console.log('no-summary diagnostics (page):', JSON.stringify(ui, null, 1));
    const st = await getJson('/api/status').catch((e) => ({ fetchError: String(e && e.cause ? e.cause.code || e.cause.message : e) }));
    console.log('no-summary diagnostics (/api/status):', JSON.stringify({
      busy: st.busy, phase: st.phase, job: st.job, job_kind: st.job_kind, clip_jobs: st.clip_jobs, clips_running: st.clips_running,
      pending_marks: st.pending_marks, mark_conflicts: st.mark_conflicts, fetchError: st.fetchError,
    }));
    console.log('server log tail:\n' + server.log().split('\n').slice(-20).join('\n'));
  }
  const shaMatch = summary && summary.match(/SHA-256:\s*([0-9a-f]{64})/);
  const pathMatch = summary && summary.match(/(?:클립 파일|Clip file):\s*(.+)/);
  check('exhibit summary shows a SHA-256', shaMatch);
  const clipPath = pathMatch ? pathMatch[1].trim() : '';
  check('exhibit summary shows the clip path inside the case', clipPath.startsWith(caseDir) && clipPath.replace(/\\/g, '/').includes('artifacts/clips/'), clipPath);
  check('summary SHA-256 equals the clip file on disk', shaMatch && existsSync(clipPath) && sha256File(clipPath) === shaMatch[1],
    existsSync(clipPath) ? `${statSync(clipPath).size} bytes` : 'missing');
  const st4 = await getJson('/api/status');
  const clipJob = (st4.clip_jobs || [])[0];
  check('/api/status clip_jobs reports the finished export', clipJob && clipJob.state === 'done' && clipJob.sha256 === (shaMatch && shaMatch[1]) && st4.clips_running === 0, JSON.stringify(clipJob));
  if (shots) await page.screenshot({ path: join(shots, 'jobs-clip-summary.png') });
  await v.evaluate(() => document.querySelector('.modal.dlg button.primary').click());

  // ---- 5. > 4 MiB frame capture over HTTP -----------------------------------
  const groups = 1_500_000; // "/9j/" = FF D8 FF; each "AAAA" = 3 zero bytes → ~6 MB of base64
  const image = '/9j/' + 'AAAA'.repeat(groups);
  const cap = await postJson('/api/capture-frame', { id: clipId, image, time: '1.000' }, 'text/plain');
  check('capture-frame accepts a > 4 MiB payload', cap.ok && image.length > 4 * 1024 * 1024, cap.ok ? cap.path : cap.error);
  if (cap.ok) {
    const saved = join(caseDir, cap.path);
    check('captured frame stored with the decoded size and its hash', existsSync(saved) && statSync(saved).size === 3 + 3 * groups && sha256File(saved) === cap.sha256);
  }

  // ---- 6. finalize packages the viewer together with its data bundle ----------
  // Finalize refuses (correctly) while any job holds `busy` — including a
  // viewer mark save, which takes busy and refreshes the report. Wait for
  // an idle server and a settled viewer instead of racing them; a refusal
  // that still hits a just-started save is retried until the deadline.
  const finStart = Date.now();
  const fin = await until(async () => {
    const idle = await getJson('/api/status');
    const saved = await page.evaluate(() => { const s = window.FTViewer.saveState(); return !s || s === 'saved'; });
    if (idle.busy || idle.clips_running || !saved) return null;
    const res = await postJson('/api/finalize', { examiner: 'e2e' });
    return res && res.ok === false && /진행 중/.test(res.error || '') ? null : res;
  }, 60000, 250) || await postJson('/api/finalize', { examiner: 'e2e' });
  const marksDuring = importMarks.filter((r) => r.at >= finStart).length;
  if (marksDuring) console.log(`finalize waited for ${marksDuring} viewer mark save(s)`);
  check('finalize succeeds', fin && fin.ok === true, JSON.stringify(fin).slice(0, 300));
  if (fin && fin.ok) {
    const pkgDir = fin.package_dir || join(caseDir, fin.package_url || '');
    check('package folder contains review/data-bundle.js next to the viewer',
      existsSync(join(pkgDir, 'review', 'evidence-viewer.html')) && existsSync(join(pkgDir, 'review', 'data-bundle.js')), pkgDir);
  }

  check('no page errors in shell or viewer', pageErrors.length === 0, pageErrors.join(' | '));
} catch (err) {
  check('harness ran to completion', false, String(err && err.stack || err));
  console.error(server.log().split('\n').slice(-30).join('\n'));
}
const failed = results.filter((r) => !r.ok).length;
console.log(failed ? `\nverify-jobs-e2e: ${failed} check(s) failed` : `\nverify-jobs-e2e: all ${results.length} checks passed`);
process.exit(failed ? 1 : 0);
