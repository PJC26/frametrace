// Save protocol v2 end to end (docs/FIX_PLAN_R1_2026-10-13.md §1.6/§1.7):
// two windows on one case, per-record revisions, the conflict banner,
// draft ownership across windows, chunked bulk saves and the legacy body.
// Drives the real workstation (startIsolatedServer + make-demo-case) in
// headless Chromium; every step awaits a save-state / DOM condition or a
// server reply — no fixed sleeps.
//
//   cargo build --locked
//   NODE_PATH=<repo>/node_modules node scripts/verify-marks-sync.mjs
//
//  ① A and B open the same case; B saves vid_1 first, A saves vid_1
//    differently → A shows #markConflictBanner, the server keeps B's value
//  ② [서버 값 사용] → A shows B's value, nothing pending, revision unchanged
//  ③ [내 값으로 덮어쓰기] → server revision +1 and A's value
//  ④ B's unsaved vid_2 draft survives A's save (per-id, per-writer drafts)
//  ⑤ A reloads while B is alive: B's live draft is not absorbed by A
//  ⑥ 30 000-record bulk "판독완료" → ceil(n/2000) POSTs, no 413, saved,
//    at most one storage-quota toast
//  ⑦ a legacy patch-v1 body sent with fetch still lands (200 + applied)
import { mkdtempSync, rmSync } from 'node:fs';
import { join, dirname } from 'node:path';
import { tmpdir } from 'node:os';
import { fileURLToPath } from 'node:url';
import { spawnSync } from 'node:child_process';
import { createRequire } from 'node:module';
import { startIsolatedServer } from './lib/isolated-server.mjs';
import { viewerPageHtml, viewerBundleJs } from './lib/app-bundle.mjs';

const root = join(dirname(fileURLToPath(import.meta.url)), '..');
const require = createRequire(join(root, 'package.json'));
let chromium;
try { ({ chromium } = require('playwright')); } catch {
  console.error('playwright not resolvable — set NODE_PATH to a node_modules with playwright');
  process.exit(2);
}

let failed = 0;
const check = (name, ok, detail = '') => {
  console.log(`${ok ? 'PASS' : 'FAIL'}  ${name}${detail ? '  — ' + String(detail).slice(0, 220) : ''}`);
  if (!ok) failed += 1;
};
const BULK_N = Number(process.env.FT_SYNC_BULK || 30000);

const work = mkdtempSync(join(tmpdir(), 'ft-marks-sync-'));
const src = join(work, 'src');
const demo = spawnSync(process.execPath, [join(root, 'scripts/make-demo-case.mjs'), src], { encoding: 'utf8' });
if (demo.status !== 0) { console.error('demo source: ' + (demo.stderr || demo.stdout)); process.exit(1); }

const srv = await startIsolatedServer({ root, work: join(work, 'srv') });
let browser;
const errors = [];
try {
  const started = await srv.postJson('/api/start', { input_kind: 'folder', source_path: src, case_dir: join(work, 'case'), with_hash: false, with_ffprobe: false });
  check('analysis starts', started.ok === true, started.error || '');
  for (const deadline = Date.now() + 240000; ; await new Promise((r) => setTimeout(r, 400))) {
    const st = await srv.getJson('/api/status');
    if (st.phase === 'review-ready' && !st.busy) break;
    if (st.phase === 'error') throw new Error('analysis error: ' + st.error);
    if (Date.now() > deadline) throw new Error('analysis timed out at ' + st.phase);
  }
  const recs = await srv.getJson('/api/records?offset=0&limit=10');
  const ids = (recs.videos || []).map((v) => v.id);
  const [vid1, vid2] = ids;
  check('two records exist', !!vid1 && !!vid2, ids.join(','));
  const meta0 = await srv.getJson('/api/records-meta');
  const caseId = meta0.manifest && meta0.manifest.case_id;
  check('records-meta carries annotations.revisions', meta0.annotations && typeof meta0.annotations.revisions === 'object', JSON.stringify(meta0.annotations && meta0.annotations.revisions));
  const serverRow = async (id) => {
    const meta = await srv.getJson('/api/records-meta');
    const row = ((meta.annotations && meta.annotations.marks) || []).find((m) => (m.record_id || m.id) === id) || null;
    const tags = ((meta.annotations && meta.annotations.tags) || []).find((t) => t.id === id);
    return { row, tags: tags ? tags.tags : [], revision: Number((meta.annotations.revisions || {})[id]) || 0 };
  };

  browser = await chromium.launch();
  // ONE context: both windows share localStorage (drafts + heartbeats).
  const ctx = await browser.newContext({ viewport: { width: 1366, height: 768 } });
  const open = async (name) => {
    const page = await ctx.newPage();
    page.on('pageerror', (e) => errors.push(`${name}: ${e.message}`));
    await page.goto(`${srv.base}/?token=${srv.token}`);
    await page.waitForSelector('#recordGrid .card', { timeout: 60000 });
    await page.evaluate(() => { window.__ss = []; FTViewer.on('save-state', (e) => window.__ss.push(e.detail)); });
    return page;
  };
  const saveState = (page) => page.evaluate(() => FTViewer.saveState());
  const waitState = (page, want, ms = 30000) => page.waitForFunction((w) => FTViewer.saveState() === w, want, { timeout: ms });
  const mark = async (page, id, key) => {
    await page.click(`#recordGrid .card[data-id="${id}"]`);
    await page.evaluate(() => document.activeElement?.blur());
    await page.keyboard.press(key);
    await page.waitForFunction((i) => !!document.querySelector(`#recordGrid .card[data-id="${CSS.escape(i)}"] .mark-chip`), id);
  };
  const chipOf = (page, id) => page.evaluate((i) => document.querySelector(`#recordGrid .card[data-id="${CSS.escape(i)}"] .mark-chip`)?.className || '', id);
  const saveNow = async (page) => {
    await page.evaluate(() => document.getElementById('saveState').click());
  };
  const bannerCount = (page) => page.locator('#markConflictBanner').count();

  const A = await open('A');
  const B = await open('B');
  check('second window toasts that the case is open elsewhere', await B.locator('#toastHost').innerText().then((t) => /다른 창|another window/.test(t)));

  // ① B saves first, A's stale save conflicts.
  await mark(B, vid1, '2'); // important
  await saveNow(B);
  await waitState(B, 'saved');
  const afterB = await serverRow(vid1);
  check('① B saved vid_1 = important', afterB.row?.status === 'important' && afterB.revision >= 1, JSON.stringify(afterB));
  // A has an examiner name, B none: [서버 값 사용] must still send the
  // server's (null) examiner back, or the "no-op" re-save would bump.
  await A.evaluate(() => FTViewer.setExaminer('검사관A'));
  await mark(A, vid1, '1'); // reviewed, base revision stale (A loaded before B saved)
  await saveNow(A);
  await A.waitForSelector('#markConflictBanner', { timeout: 30000 });
  const bannerText = await A.locator('#markConflictBanner').innerText();
  check('① A shows the conflict banner with both choices', /1건|1 record/.test(bannerText) && /서버 값 사용/.test(bannerText) && /내 값으로 덮어쓰기/.test(bannerText), bannerText.replace(/\n/g, ' '));
  check('① banner is role=alert with ≥24px buttons', await A.evaluate(() => {
    const bar = document.getElementById('markConflictBanner');
    const btns = [...bar.querySelectorAll('button')];
    return bar.getAttribute('role') === 'alert' && btns.length === 2 && btns.every((b) => b.getBoundingClientRect().height >= 24);
  }));
  const stillB = await serverRow(vid1);
  check('① server kept B\'s value (A did not overwrite)', stillB.row?.status === 'important' && stillB.revision === afterB.revision, JSON.stringify(stillB));
  check('① A is pending (conflict counts), save-state event carries conflicts', await saveState(A) === 'pending' && await A.evaluate(() => window.__ss.some((d) => d.conflicts === 1 && d.pending >= 1)));
  check('① reload refused while a conflict is open', await A.evaluate(() => FTViewer.reload()) === 'refused');

  // ② Use server → A adopts B's value, no more pending, revision unchanged.
  await A.click('#markConflictUseServer');
  await A.waitForFunction(() => !document.getElementById('markConflictBanner'));
  await waitState(A, 'saved');
  check('② [서버 값 사용]: A shows B\'s mark, saved', /important/.test(await chipOf(A, vid1)) && await A.evaluate(() => FTViewer.summary().pending) === 0);
  const afterUseServer = await serverRow(vid1);
  check('② server value and revision unchanged by the no-op re-save', afterUseServer.row?.status === 'important' && !afterUseServer.row?.examiner && afterUseServer.revision === afterB.revision, JSON.stringify(afterUseServer));

  // ③ Use mine → server revision +1 with A's value.
  await mark(B, vid1, '3'); // needs_verification
  await saveNow(B);
  await waitState(B, 'saved');
  const beforeMine = await serverRow(vid1);
  await mark(A, vid1, '1');
  await saveNow(A);
  await A.waitForSelector('#markConflictBanner', { timeout: 30000 });
  await A.click('#markConflictUseMine');
  await A.waitForFunction(() => !document.getElementById('markConflictBanner'));
  await waitState(A, 'saved');
  const afterMine = await serverRow(vid1);
  check('③ [내 값으로 덮어쓰기]: server = A\'s value, revision +1', afterMine.row?.status === 'reviewed' && afterMine.revision === beforeMine.revision + 1, `${beforeMine.revision} → ${afterMine.revision} ${afterMine.row?.status}`);
  check('③ A saved with no conflict left', await saveState(A) === 'saved' && await A.evaluate(() => FTViewer.summary().pending) === 0);

  // ④ B's unsaved draft survives A's save. B's saves are blocked so the
  // draft stays a draft whatever the timing.
  await B.route('**/api/import-marks', (route) => route.abort());
  await mark(B, vid2, '2');
  const draftKey = await B.evaluate(() => Object.keys(localStorage).find((k) => k.endsWith('.annotations.v1')));
  await B.waitForFunction(([k, id]) => !!(JSON.parse(localStorage.getItem(k) || '{}') || {})[id], [draftKey, vid2]);
  const bToken = await B.evaluate(() => FTViewer.writerToken);
  await mark(A, vid1, '2');
  await saveNow(A);
  await waitState(A, 'saved');
  // The confirmed draft leaves localStorage at the next debounced flush;
  // wait for that flush (FTViewer state is already 'saved').
  await A.waitForFunction(([k, id]) => !(JSON.parse(localStorage.getItem(k) || '{}') || {})[id], [draftKey, vid1], { timeout: 10000 }).catch(() => {});
  const drafts = await A.evaluate((k) => JSON.parse(localStorage.getItem(k) || '{}'), draftKey);
  check('④ B\'s unsaved vid_2 draft survives A\'s save (writer = B, A\'s own draft gone)', drafts[vid2]?.writer === bToken && drafts[vid2]?.mark?.status === 'important' && !drafts[vid1], JSON.stringify(drafts).slice(0, 400));
  check('④ B still reports its edit as pending', await B.evaluate(() => FTViewer.summary().pending) >= 1 && ['pending', 'failed', 'saving'].includes(await saveState(B)));

  // ⑤ A reloads while B is alive: the live draft is B's, not A's.
  await A.reload();
  await A.waitForSelector('#recordGrid .card', { timeout: 60000 });
  check('⑤ reloaded A does not absorb B\'s live draft', await A.evaluate(() => FTViewer.summary().pending) === 0 && !(await chipOf(A, vid2)), `pending=${await A.evaluate(() => FTViewer.summary().pending)} chip=${await chipOf(A, vid2)}`);
  await B.unroute('**/api/import-marks');
  await saveNow(B);
  await waitState(B, 'saved', 60000);
  const v2row = await serverRow(vid2);
  check('⑤ B\'s draft reaches the server once B saves', v2row.row?.status === 'important', JSON.stringify(v2row));
  check('⑤ A/B no page errors', errors.length === 0, errors.join(' | '));

  // ⑥ Bulk: a standalone viewer page on the SAME origin (so its /api
  // fetches hit the real server) with 30 000 synthetic records.
  const bulkIds = Array.from({ length: BULK_N }, (_, i) => `vid_bulk_${String(i).padStart(6, '0')}`);
  const bulkData = `window.__FRAMETRACE_DATA__ = ${JSON.stringify({
    manifest: { case_id: caseId, title: 'bulk' }, caseDir: join(work, 'case'),
    scan: { videos: bulkIds.map((id, i) => ({ id, relative_path: `DCIM/${i}.mp4`, source_path: `/case/DCIM/${i}.mp4`, ffprobe_ok: true, size_bytes: 1000, extension: 'mp4' })) },
    carveLog: [], filesystemLog: [], validationLog: [], anomalyLog: [], flsEntries: [],
    thumbs: {}, annotations: { marks: [], tags: [], revisions: {} }, deepfake: {}, telemetry: {}, proxies: [],
  })};`;
  const bulkHtml = viewerPageHtml({ data: bulkData, js: viewerBundleJs(root), root });
  const bulkPage = await ctx.newPage();
  const bulkErrors = [];
  bulkPage.on('pageerror', (e) => bulkErrors.push(e.message));
  const posts = [];
  await bulkPage.route(`${srv.base}/__sync-bulk.html`, (route) => route.fulfill({ contentType: 'text/html; charset=utf-8', body: bulkHtml }));
  await bulkPage.route('**/api/import-marks', async (route) => {
    const body = JSON.parse(JSON.parse(route.request().postData()).marks_json);
    const res = await route.fetch();
    posts.push({ ids: body.marks.length + body.deleted_ids.length, status: res.status(), batch: body.batch });
    await route.fulfill({ response: res });
  });
  await bulkPage.goto(`${srv.base}/__sync-bulk.html`);
  await bulkPage.waitForSelector('#recordGrid .card', { timeout: 120000 });
  const t0 = Date.now();
  await bulkPage.evaluate(() => document.getElementById('btnSelectFiltered').click());
  await bulkPage.waitForFunction((n) => FTViewer.summary() && document.getElementById('selectionCount') && /\d/.test(document.getElementById('selectionCount').textContent), BULK_N);
  await bulkPage.evaluate(() => document.getElementById('btnMarkReviewed').click());
  // Bulk actions above the threshold ask first (in-page dialog).
  const dlg = bulkPage.locator('.modal.dlg button.primary');
  if (await dlg.count()) await dlg.click();
  await bulkPage.waitForFunction((n) => FTViewer.summary().marked === n, BULK_N, { timeout: 120000 });
  const markMs = Date.now() - t0;
  await bulkPage.evaluate(() => document.getElementById('saveState').click());
  await waitState(bulkPage, 'saved', 600000);
  const expectPosts = Math.ceil(BULK_N / 2000);
  check(`⑥ ${BULK_N} bulk marks → ${expectPosts} POSTs (2000 ids each), none 413`, posts.length === expectPosts && posts.every((p) => p.status === 200 && p.ids <= 2000), JSON.stringify(posts.map((p) => [p.ids, p.status, p.batch && p.batch.index])).slice(0, 200));
  check('⑥ chunks share one batch id and the last one is index total-1', posts.every((p) => p.batch && p.batch.id === posts[0].batch.id && p.batch.total === expectPosts) && posts.at(-1).batch.index === expectPosts - 1);
  const bulkMeta = await srv.getJson('/api/records-meta');
  const bulkRevs = Object.keys(bulkMeta.annotations.revisions || {}).filter((id) => id.startsWith('vid_bulk_')).length;
  check(`⑥ server holds ${BULK_N} revisions`, bulkRevs === BULK_N, String(bulkRevs));
  const quotaToasts = await bulkPage.evaluate(() => [...document.querySelectorAll('#toastHost .toast')].filter((t) => /저장소|storage/.test(t.textContent)).length);
  check('⑥ at most one storage-quota toast', quotaToasts <= 1, `${quotaToasts} (marking took ${markMs} ms)`);
  check('⑥ bulk page: no page errors, nothing pending', bulkErrors.length === 0 && await bulkPage.evaluate(() => FTViewer.summary().pending) === 0, bulkErrors.join(' | '));

  // ⑦ Legacy patch-v1 body straight to the API still applies.
  const legacy = { schema_version: 2, protocol: 'patch-v1', case_id: caseId, examiner: 'legacy', exported_unix: 1, marks: [{ id: vid1, status: 'important', marked_unix: 7, note: 'legacy note', examiner: 'legacy' }], tags: [{ id: vid1, tags: ['레거시'] }], deleted_ids: [] };
  const legacyRes = await srv.postJson('/api/import-marks', { marks_json: JSON.stringify(legacy) });
  const legacyRow = await serverRow(vid1);
  check('⑦ legacy patch-v1 body: 200, applied map, value stored', legacyRes.ok === true && legacyRes.applied && typeof legacyRes.applied[vid1] === 'number' && legacyRow.row?.status === 'important' && legacyRow.row?.note === 'legacy note' && legacyRow.tags.includes('레거시'), JSON.stringify(legacyRes).slice(0, 200));
} catch (err) {
  check('harness ran to the end', false, err && err.stack || String(err));
} finally {
  if (browser) await browser.close();
  await srv.stop();
  rmSync(work, { recursive: true, force: true, maxRetries: 10, retryDelay: 200 });
}
console.log(failed ? `verify-marks-sync: ${failed} failed` : 'verify-marks-sync: all checks passed');
process.exit(failed ? 1 : 0);
