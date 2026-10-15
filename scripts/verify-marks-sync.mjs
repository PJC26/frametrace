// Save protocol v2 end to end (docs/FIX_PLAN_R1_2026-10-13.md §1.6/§1.7,
// §10.3): two windows on one case, per-record revisions, the conflict
// banner + per-record conflict list, per-id drafts across windows,
// dead-window adoption, the server queue, chunked bulk saves and the
// legacy body. Drives the real workstation (startIsolatedServer +
// make-demo-case) in headless Chromium; every step awaits a save-state /
// DOM / storage condition or a server reply — no fixed sleeps.
//
//   cargo build --locked
//   NODE_PATH=<repo>/node_modules node scripts/verify-marks-sync.mjs [--pre-v10]
//
//  ① A and B open the same case; B saves vid_1 first, A saves vid_1
//    differently → A shows #markConflictBanner, the server keeps B's value
//  ② [서버 값 사용] → A shows B's value, nothing pending, revision unchanged
//  ③ [내 값으로 덮어쓰기] → server revision +1 and A's value
//  ④ B's unsaved vid_2 draft survives A's save (per-id, per-writer keys)
//  ⑤ A reloads while B is alive: B's live draft is not absorbed by A
//  ⑧ reload with an open conflict restores the banner (conflict in the draft)
//  ⑨ conflict list panel: keyboard-operable, both values per record,
//    per-item [서버 값] / [내 값], AA contrast in both themes, resolved ids
//    leave the undo journal
//  ⑩ a crashed window's draft (stale heartbeat, no ping answer) is adopted
//    with its base revision → conflict, never an overwrite
//  ⑪ a live window with a stale heartbeat answers the ping: not absorbed
//  ⑭ hosted unmount waits for the save in flight; its reply cleans the draft
//  ⑫ server queue (carve job running): a mark then a note on one record
//    from the same window → no conflict after the drain [needs §10.1 same-
//    client rule]; a queued save that met a newer revision comes back
//    from GET /api/mark-conflicts with the queued value as "mine" [needs
//    §10.2 route], blocks the shell finalize checklist, and resolves
//  ⑥ 30 000-record bulk "판독완료" with storage pre-filled to the quota →
//    ceil(n/2000) POSTs, no 413, saved, exactly ONE storage-quota toast
//  ⑦ a legacy patch-v1 body sent with fetch still lands (200 + applied)
//  ⑯ R6 U1/U2 (S2): GET /api/marks-pending of the real server, read by the
//    viewer's own readPagedList: one queue file of 800 ids is read in full
//    across the page boundary (string cursor "<seq>:<idx>"), an unreadable
//    queue file is reported. R7 U3: the string cursor is required (the R6
//    numeric-cursor fallback passed a regressed server).
//  ⑮ R4 A1/A2: an older browser range (retired RANGES_KEY) vs a range another
//    window stored → an ordinary conflict; i/o, a tag and a cleared mark
//    meanwhile send nothing and never read "saved"; the server keeps its
//    range, mark and tags
//
// --pre-v10: skip the checks that need the §10.1/§10.2 server rules
// (reported as SKIP), for a workstation built before them.
import { mkdtempSync, rmSync, openSync, writeSync, closeSync, mkdirSync, readFileSync, statSync, writeFileSync, existsSync } from 'node:fs';
import { runInNewContext } from 'node:vm';
import { randomFillSync } from 'node:crypto';
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

const PRE_V10 = process.argv.includes('--pre-v10');
let failed = 0;
const check = (name, ok, detail = '') => {
  console.log(`${ok ? 'PASS' : 'FAIL'}  ${name}${detail ? '  — ' + String(detail).slice(0, 260) : ''}`);
  if (!ok) failed += 1;
};
// Checks that hold only once the §10.1/§10.2 server rules are in.
const checkV10 = (name, ok, detail = '') => {
  if (PRE_V10) { console.log(`SKIP  ${name}  — needs the §10.1/§10.2 server rules (--pre-v10)`); return; }
  check(name, ok, detail);
};
const BULK_N = Number(process.env.FT_SYNC_BULK || 30000);

const work = mkdtempSync(join(tmpdir(), 'ft-marks-sync-'));
const src = join(work, 'src');
const caseDir = join(work, 'case');
const demo = spawnSync(process.execPath, [join(root, 'scripts/make-demo-case.mjs'), src], { encoding: 'utf8' });
if (demo.status !== 0) { console.error('demo source: ' + (demo.stderr || demo.stdout)); process.exit(1); }

// A raw "disk image" for the carve job that keeps the case busy (the debug
// carver reads a few MB/s): random filler with an MP4 embedded.
function writeRaw(path, mib) {
  mkdirSync(dirname(path), { recursive: true });
  const embedded = readFileSync(join(src, 'DCIM/FRONT/FRONT_20260908_010000.mp4'));
  const fd = openSync(path, 'w');
  const noise = Buffer.alloc(1 << 20);
  randomFillSync(noise);
  const zero = Buffer.alloc(1 << 20);
  for (let i = 0; i < mib; i++) {
    if (i === 4) writeSync(fd, embedded);
    writeSync(fd, i % 3 === 0 ? noise : zero);
  }
  closeSync(fd);
}

const srv = await startIsolatedServer({ root, work: join(work, 'srv') });
// Node's fetch pools keep-alive sockets; one the server closed while the
// browser pages were busy fails with "fetch failed" before any byte is
// sent. Such a request never reached the server, so it is retried.
for (const name of ['getJson', 'postJson']) {
  const call = srv[name];
  srv[name] = async (...args) => {
    for (let attempt = 0; ; attempt++) {
      try { return await call(...args); } catch (err) {
        if (attempt >= 20 || !/fetch failed|ECONNRESET|socket/i.test(String(err && (err.message + (err.cause ? ' ' + err.cause.code : ''))))) throw err;
        await new Promise((r) => setTimeout(r, 100 * (attempt + 1)));
      }
    }
  };
}
let browser;
const errors = [];
try {
  const started = await srv.postJson('/api/start', { input_kind: 'folder', source_path: src, case_dir: caseDir, with_hash: false, with_ffprobe: false });
  check('analysis starts', started.ok === true, started.error || '');
  for (const deadline = Date.now() + 240000; ; await new Promise((r) => setTimeout(r, 400))) {
    const st = await srv.getJson('/api/status');
    if (st.phase === 'review-ready' && !st.busy) break;
    if (st.phase === 'error') throw new Error('analysis error: ' + st.error);
    if (Date.now() > deadline) throw new Error('analysis timed out at ' + st.phase);
  }
  const recs = await srv.getJson('/api/records?offset=0&limit=20');
  const ids = (recs.videos || []).map((v) => v.id);
  const [vid1, vid2, vid3, vid4, vid5, vid6, vid7, vid8] = ids;
  check('eight records exist', ids.length >= 8, ids.join(','));
  const meta0 = await srv.getJson('/api/records-meta');
  const caseId = meta0.manifest && meta0.manifest.case_id;
  check('records-meta carries annotations.revisions', meta0.annotations && typeof meta0.annotations.revisions === 'object', JSON.stringify(meta0.annotations && meta0.annotations.revisions));
  const serverRow = async (id) => {
    const meta = await srv.getJson('/api/records-meta');
    const row = ((meta.annotations && meta.annotations.marks) || []).find((m) => (m.record_id || m.id) === id) || null;
    const tags = ((meta.annotations && meta.annotations.tags) || []).find((t) => t.id === id);
    return { row, tags: tags ? tags.tags : [], revision: Number((meta.annotations.revisions || {})[id]) || 0, writer: (meta.annotations.writers || {})[id] };
  };
  // Another window's save, straight to the API (patch-v2, its own client id).
  const otherSave = async (id, status, note, base) => srv.postJson('/api/import-marks', { marks_json: JSON.stringify({
    schema_version: 2, protocol: 'patch-v2', case_id: caseId, examiner: 'other', client_id: 'w-other-window', exported_unix: 1,
    base_revisions: { [id]: base }, marks: [{ id, status, marked_unix: 1, note, examiner: 'other', tags: [], range: { in: null, out: null } }], tags: [], deleted_ids: [],
  }) });
  const draftKeyOf = (writer, id) => `ft.draft.${caseId}.${writer}.${id}`;

  browser = await chromium.launch();
  // ONE context: every window shares localStorage (drafts + heartbeats)
  // and BroadcastChannel (the liveness ping).
  const ctx = await browser.newContext({ viewport: { width: 1366, height: 768 } });
  // save-state / toast recorders (again after every page reload).
  const instrument = (page) => page.evaluate(() => {
    window.__ss = []; window.__toasts = [];
    FTViewer.on('save-state', (e) => window.__ss.push(e.detail));
    FTViewer.on('toast', (e) => window.__toasts.push(e.detail.message));
  });
  const reloadPage = async (page) => {
    await page.reload();
    await page.waitForSelector('#recordGrid .card', { timeout: 60000 });
    await instrument(page);
  };
  const open = async (name) => {
    const page = await ctx.newPage();
    page.on('pageerror', (e) => errors.push(`${name}: ${e.message}`));
    await page.goto(`${srv.base}/?token=${srv.token}`);
    await page.waitForSelector('#recordGrid .card', { timeout: 60000 });
    await instrument(page);
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
  const conflictCount = (page) => page.evaluate(() => {
    const m = /(\d+)/.exec(document.querySelector('#markConflictBanner .conflict-msg')?.textContent || '');
    return m ? Number(m[1]) : 0;
  });
  // Text contrast of the banner + list, in the current theme (WCAG ratio
  // against the first non-transparent background up the tree).
  const contrastIssues = (page) => page.evaluate(() => {
    const parse = (c) => { const m = c.match(/[\d.]+/g) || []; return m.map(Number); };
    const lum = ([r, g, b]) => { const f = (v) => { v /= 255; return v <= 0.03928 ? v / 12.92 : Math.pow((v + 0.055) / 1.055, 2.4); }; return 0.2126 * f(r) + 0.7152 * f(g) + 0.0722 * f(b); };
    const bgOf = (el) => { for (let n = el; n; n = n.parentElement) { const c = parse(getComputedStyle(n).backgroundColor); if (c.length >= 3 && (c.length < 4 || c[3] > 0.5)) return c; } return [255, 255, 255]; };
    const sel = '#markConflictBanner .ft-banner-msg, #markConflictBanner button, #markConflictPanel .conflict-title, #markConflictPanel .conflict-name, #markConflictPanel .conflict-id, #markConflictPanel .conflict-tag, #markConflictPanel dt, #markConflictPanel dd, #markConflictPanel button';
    const out = [];
    for (const el of document.querySelectorAll(sel)) {
      const a = lum(parse(getComputedStyle(el).color)); const b = lum(bgOf(el));
      const ratio = (Math.max(a, b) + 0.05) / (Math.min(a, b) + 0.05);
      if (ratio < 4.5) out.push(`${el.className || el.tagName}:${ratio.toFixed(2)}`);
    }
    return { issues: out, n: document.querySelectorAll(sel).length };
  });

  const A = await open('A');
  const B = await open('B');
  // F12: the toast fires during boot (before instrument() listens), so the
  // assertion reads the rendered toast in whichever host shows it.
  const otherToast = await B.waitForFunction(() => [...document.querySelectorAll('#toastHost .toast, #hToastBox .h-toast')]
    .map((t) => t.textContent).find((t) => /다른 창|another window/.test(t)) || null, null, { timeout: 10000 }).then((h) => h.jsonValue()).catch(() => null);
  check('second window toasts that the case is open elsewhere', !!otherToast, String(otherToast));

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
  check('① banner is role=alert, ≥24px buttons incl. the list toggle', await A.evaluate(() => {
    const bar = document.getElementById('markConflictBanner');
    const btns = [...bar.querySelectorAll('button')];
    const toggle = document.getElementById('markConflictList');
    return bar.getAttribute('role') === 'alert' && btns.length === 3 && btns.every((b) => b.getBoundingClientRect().height >= 24) &&
      toggle.getAttribute('aria-controls') === 'markConflictPanel' && toggle.getAttribute('aria-expanded') === 'false';
  }));
  check('① banner sits in the notices row above the grid (not over it)', await A.evaluate(() => {
    const bar = document.getElementById('markConflictBanner').getBoundingClientRect();
    const grid = document.querySelector('#ftViewer .workarea').getBoundingClientRect();
    return bar.bottom <= grid.top + 1 && bar.height > 0;
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
  const bToken = await B.evaluate(() => FTViewer.writerToken);
  const aToken = await A.evaluate(() => FTViewer.writerToken);
  await B.waitForFunction((k) => !!localStorage.getItem(k), draftKeyOf(bToken, vid2));
  await mark(A, vid1, '2');
  await A.waitForFunction((k) => !!localStorage.getItem(k), draftKeyOf(aToken, vid1));
  await saveNow(A);
  await waitState(A, 'saved');
  // The confirmed draft leaves localStorage at the next debounced flush.
  await A.waitForFunction((k) => !localStorage.getItem(k), draftKeyOf(aToken, vid1), { timeout: 10000 }).catch(() => {});
  const bDraft = await A.evaluate((k) => JSON.parse(localStorage.getItem(k) || 'null'), draftKeyOf(bToken, vid2));
  const aDraftGone = await A.evaluate((k) => !localStorage.getItem(k), draftKeyOf(aToken, vid1));
  check('④ B\'s unsaved vid_2 draft survives A\'s save (B\'s own key, with its base), A\'s confirmed draft gone', bDraft?.mark?.status === 'important' && Number.isFinite(bDraft?.base) && aDraftGone, JSON.stringify(bDraft));
  check('④ no single-key draft store is written any more', await A.evaluate((c) => localStorage.getItem(`ft.viewer.${c}.annotations.v1`) === null && localStorage.getItem(`ft.viewer.${c}.marks`) === null, caseId));
  check('④ B still reports its edit as pending', await B.evaluate(() => FTViewer.summary().pending) >= 1 && ['pending', 'failed', 'saving'].includes(await saveState(B)));

  // ⑤ A reloads while B is alive: the live draft is B's, not A's.
  await reloadPage(A);
  await waitState(A, 'saved');
  check('⑤ reloaded A does not absorb B\'s live draft', await A.evaluate(() => FTViewer.summary().pending) === 0 && !(await chipOf(A, vid2)), `pending=${await A.evaluate(() => FTViewer.summary().pending)} chip=${await chipOf(A, vid2)}`);
  await B.unroute('**/api/import-marks');
  await saveNow(B);
  await waitState(B, 'saved', 60000);
  const v2row = await serverRow(vid2);
  check('⑤ B\'s draft reaches the server once B saves', v2row.row?.status === 'important', JSON.stringify(v2row));

  // ⑧ An open conflict survives a reload (it rides in the draft).
  await mark(B, vid3, '2');
  await saveNow(B);
  await waitState(B, 'saved');
  const v3B = await serverRow(vid3);
  await mark(A, vid3, '1');
  await saveNow(A);
  await A.waitForSelector('#markConflictBanner', { timeout: 30000 });
  const aTokenR = await A.evaluate(() => FTViewer.writerToken); // A reloaded in ⑤: a new writer token
  await A.waitForFunction((k) => !!(JSON.parse(localStorage.getItem(k) || 'null') || {}).conflict, draftKeyOf(aTokenR, vid3));
  await reloadPage(A);
  await A.waitForSelector('#markConflictBanner', { timeout: 30000 });
  check('⑧ reload restores the conflict banner, still pending, A\'s value shown', await conflictCount(A) === 1 && await saveState(A) === 'pending' && /reviewed/.test(await chipOf(A, vid3)));
  check('⑧ the server kept B\'s value', (await serverRow(vid3)).row?.status === 'important');

  // ⑨ Per-record conflict list. A second conflict, made in this page load
  // (so it has an undo journal entry).
  await mark(B, vid4, '3');
  await saveNow(B);
  await waitState(B, 'saved');
  const v4B = await serverRow(vid4);
  const aToken2 = await A.evaluate(() => FTViewer.writerToken);
  await mark(A, vid4, '1');
  await saveNow(A);
  await A.waitForFunction(() => /(\d+)/.exec(document.querySelector('#markConflictBanner .conflict-msg')?.textContent || '')?.[1] === '2', null, { timeout: 30000 });
  // Keyboard: focus the list toggle, Enter opens it and moves focus to the title.
  await A.focus('#markConflictList');
  await A.keyboard.press('Enter');
  await A.waitForSelector('#markConflictPanel:not([hidden]) .conflict-item');
  const panel = await A.evaluate(() => ({
    expanded: document.getElementById('markConflictList').getAttribute('aria-expanded'),
    focus: document.activeElement?.id,
    items: [...document.querySelectorAll('#markConflictPanel .conflict-item')].map((li) => ({ id: li.dataset.id, text: li.innerText.replace(/\s+/g, ' '), buttons: [...li.querySelectorAll('button')].map((b) => [b.getAttribute('aria-label'), Math.round(b.getBoundingClientRect().height)]) })),
  }));
  const item3 = panel.items.find((i) => i.id === vid3);
  const item4 = panel.items.find((i) => i.id === vid4);
  check('⑨ list opens by keyboard (aria-expanded, focus on its title), one row per conflict', panel.expanded === 'true' && panel.focus === 'markConflictPanelTitle' && panel.items.length === 2, JSON.stringify(panel).slice(0, 300));
  check('⑨ each row shows the server value AND mine', /서버.*중요/.test(item3?.text) && /내 값.*판독 완료/.test(item3?.text) && /서버.*검증/.test(item4?.text), `${item3?.text} | ${item4?.text}`);
  check('⑨ per-row buttons are labelled per record and ≥24px', panel.items.every((i) => i.buttons.length === 2 && i.buttons.every(([label, h]) => label && label.includes('—') && h >= 24)));
  for (const theme of ['light', 'dark']) {
    await A.evaluate((t) => FTViewer.setTheme(t), theme);
    await A.waitForFunction(() => document.getAnimations().every((a) => !(a instanceof CSSTransition) || a.playState === "finished")); // theme transitions settled
    const c = await contrastIssues(A);
    check(`⑨ banner + list text ≥4.5:1 in the ${theme} theme`, c.n > 10 && c.issues.length === 0, `${c.n} elements; ${c.issues.join(' ')}`);
  }
  await A.evaluate(() => FTViewer.setTheme('light'));
  // vid3: [서버 값] by keyboard (Tab to it, Enter) → focus lands on vid4's [서버 값].
  await A.focus(`#markConflictPanel [data-id="${vid3}"] button[data-side="server"]`);
  await A.keyboard.press('Enter');
  await A.waitForFunction(() => document.querySelectorAll('#markConflictPanel .conflict-item').length === 1);
  check('⑨ per-item [서버 값] resolves one record; focus moves to the next row', await A.evaluate((v4) => document.activeElement?.closest('[data-id]')?.dataset.id === v4 && document.activeElement?.dataset.side === 'server', vid4) && await conflictCount(A) === 1);
  await A.click(`#markConflictPanel [data-id="${vid4}"] button[data-side="mine"]`);
  await A.waitForFunction(() => !document.getElementById('markConflictBanner') && !document.getElementById('markConflictPanel'));
  await waitState(A, 'saved');
  const v3A = await serverRow(vid3);
  const v4A = await serverRow(vid4);
  check('⑨ vid_3 = server value, revision unchanged; vid_4 = mine, revision +1', /important/.test(await chipOf(A, vid3)) && v3A.row?.status === 'important' && v3A.revision === v3B.revision && v4A.row?.status === 'reviewed' && v4A.revision === v4B.revision + 1, `${JSON.stringify(v3A)} ${JSON.stringify(v4A)}`);
  // The undo journal entry of the vid_4 mark is gone: Ctrl+Z reverts nothing.
  await A.evaluate(() => document.activeElement?.blur());
  await A.keyboard.press('Control+z');
  await A.waitForFunction(() => window.__toasts.some((t) => /취소할 작업이 없|Nothing to undo/i.test(t)), null, { timeout: 5000 }).catch(() => {});
  check('⑨ resolved ids left the undo journal (Ctrl+Z: nothing to undo, vid_4 stays)', await A.evaluate(() => window.__toasts.some((t) => /취소할 작업이 없|Nothing to undo/i.test(t))) && /reviewed/.test(await chipOf(A, vid4)), (await A.evaluate(() => window.__toasts)).join(' | '));
  void aToken2;

  // ⑪ A live window whose heartbeat went stale (storage full, frozen
  // timers) answers the BroadcastChannel ping: its draft stays its own.
  const D = await open('D');
  await D.route('**/api/import-marks', (route) => route.abort());
  const dToken = await D.evaluate(() => FTViewer.writerToken);
  await mark(D, vid6, '2');
  await D.waitForFunction((k) => !!localStorage.getItem(k), draftKeyOf(dToken, vid6));
  await D.evaluate(([c, w]) => {
    // The heartbeat can no longer be written (as with a full storage) …
    const set = Storage.prototype.setItem;
    Storage.prototype.setItem = function (k, v) { if (String(k).startsWith('ft.alive:')) return; return set.call(this, k, v); };
    // … and is 6 minutes old. A spy channel records the pings D receives.
    set.call(localStorage, `ft.alive:${c}:${w}`, String(Date.now() - 6 * 60000));
    window.__pings = [];
    const spy = new BroadcastChannel(`ft-alive:${c}`);
    spy.onmessage = (e) => { if (e.data && e.data.type === 'ping') window.__pings.push(e.data); };
  }, [caseId, dToken]);
  await reloadPage(A);
  const aToken3 = await A.evaluate(() => FTViewer.writerToken);
  // Two pings from A for D: the first adoption check finished (one at a time).
  await D.waitForFunction(([a, d]) => window.__pings.filter((p) => p.from === a && p.writers.includes(d)).length >= 2, [aToken3, dToken], { timeout: 30000 });
  check('⑪ stale-heartbeat live window answered the ping: A did not adopt its draft', await A.evaluate(() => FTViewer.summary().pending) === 0 && !(await chipOf(A, vid6)) && await A.evaluate((k) => !!localStorage.getItem(k), draftKeyOf(dToken, vid6)));
  await D.unroute('**/api/import-marks');
  await saveNow(D);
  await waitState(D, 'saved', 60000);
  check('⑪ D saves its own draft afterwards', (await serverRow(vid6)).row?.status === 'important');
  await D.close();

  // ⑩ A crashed window: stale heartbeat, gone (no ping answer). Its draft
  // was made on revision r; another window saved r+1 since. Adoption sends
  // the stored base → the server answers with a conflict, never an overwrite.
  await B.close();
  const C = await open('C');
  await C.route('**/api/import-marks', (route) => route.abort());
  const cToken = await C.evaluate(() => FTViewer.writerToken);
  const v5Base = (await serverRow(vid5)).revision;
  await mark(C, vid5, '2'); // important, base v5Base
  await C.waitForFunction((k) => !!localStorage.getItem(k), draftKeyOf(cToken, vid5));
  const other = await otherSave(vid5, 'needs_verification', 'other window', v5Base);
  const v5Other = await serverRow(vid5);
  check('⑩ another window saved vid_5 after C\'s draft was made', other.ok === true && v5Other.revision === v5Base + 1 && v5Other.row?.status === 'needs_verification', JSON.stringify(other).slice(0, 160));
  // Crash: the heartbeat stops (stale 6 min) and pagehide cannot mark C closed.
  await C.evaluate(([c, w]) => {
    const set = Storage.prototype.setItem; const rm = Storage.prototype.removeItem;
    Storage.prototype.setItem = function (k, v) { if (String(k).startsWith('ft.alive:')) return; return set.call(this, k, v); };
    Storage.prototype.removeItem = function (k) { if (String(k).startsWith('ft.alive:')) return; return rm.call(this, k); };
    set.call(localStorage, `ft.alive:${c}:${w}`, String(Date.now() - 6 * 60000));
  }, [caseId, cToken]);
  await C.close();
  // A's heartbeat tick pings C, gets no answer, adopts the draft and saves it.
  await A.waitForSelector('#markConflictBanner', { timeout: 60000 });
  const v5After = await serverRow(vid5);
  check('⑩ dead window\'s draft adopted with its base → conflict in A, not an overwrite', await conflictCount(A) === 1 && v5After.row?.status === 'needs_verification' && v5After.revision === v5Other.revision, JSON.stringify(v5After));
  check('⑩ the adopted draft moved to A\'s key (dead key removed)', await A.evaluate(([dead, mine]) => !localStorage.getItem(dead) && !!localStorage.getItem(mine), [draftKeyOf(cToken, vid5), draftKeyOf(aToken3, vid5)]));
  await A.click('#markConflictList');
  const v5Item = await A.locator(`#markConflictPanel [data-id="${vid5}"]`).innerText();
  check('⑩ list shows server = the other window, mine = the dead window\'s draft', /검증/.test(v5Item) && /중요/.test(v5Item), v5Item.replace(/\s+/g, ' '));
  await A.click('#markConflictUseServer');
  await A.waitForFunction(() => !document.getElementById('markConflictBanner'));
  await waitState(A, 'saved');

  // ⑭ Hosted unmount waits for the save in flight; the reply settles the draft.
  let releaseHeld;
  const gate = new Promise((r) => { releaseHeld = r; });
  let arrived;
  const reached = new Promise((r) => { arrived = r; });
  await A.route('**/api/import-marks', async (route) => {
    arrived();
    await gate;
    await route.continue();
  });
  await mark(A, vid2, '3');
  await saveNow(A);
  await reached; // the save request is on the wire (held)
  const unmounting = await A.evaluate(() => {
    window.__unmounted = false;
    FTViewer.on('destroyed', () => { window.__unmounted = true; });
    hostViewerUnmount().then(() => { window.__unmountDone = true; });
    return !window.__unmounted;
  });
  check('⑭ unmount does not destroy the viewer while its save is in flight', unmounting && await A.evaluate(() => !window.__unmounted && FTViewer.saveState() === 'saving'));
  releaseHeld();
  await A.waitForFunction(() => window.__unmountDone === true && window.__unmounted === true, null, { timeout: 30000 });
  await A.unroute('**/api/import-marks');
  check('⑭ after the reply: destroyed, the confirmed draft removed from storage', await A.evaluate((k) => !localStorage.getItem(k), draftKeyOf(aToken3, vid2)) && (await serverRow(vid2)).row?.status === 'needs_verification');
  await A.evaluate(() => hostViewerMount());
  await A.waitForSelector('#recordGrid .card', { timeout: 60000 });
  await waitState(A, 'saved');
  check('⑭ remount: nothing replayed as unsaved', await A.evaluate(() => FTViewer.summary().pending) === 0);

  // ⑫ Server queue: a carve job holds the case busy, so saves are queued
  // (deferred) and applied when the job ends.
  const rawPath = join(caseDir, 'evidence/images/evidence.raw');
  writeRaw(rawPath, 200);
  // R4 C1: a raw image is carved only with its completion marker (size +
  // modification time in ns, src/e01.rs write_raw_done_marker).
  {
    const st = statSync(rawPath, { bigint: true });
    // modified_nanos exceeds 2^53: written as the exact BigInt digits.
    writeFileSync(rawPath + '.done.json', JSON.stringify({
      schema_version: 1, kind: 'frametrace-e01-raw-complete', raw_path: rawPath, size_bytes: Number(st.size),
      modified_nanos: '@NANOS@', sha256: '0'.repeat(64), e01_path: join(work, 'synthetic.E01'), imported_unix: 1,
    }).replace('"@NANOS@"', st.mtimeNs.toString()));
  }
  const imports = [];
  await A.route('**/api/import-marks', async (route) => {
    const res = await route.fetch();
    let body = null; try { body = await res.json(); } catch { body = null; }
    imports.push(body);
    await route.fulfill({ response: res, json: body || undefined });
  });
  const v8Base = (await serverRow(vid8)).revision;
  const carve = await srv.postJson('/api/carve', {});
  check('⑫ carve job started (case busy)', carve.ok === true, JSON.stringify(carve).slice(0, 160));
  for (const deadline = Date.now() + 30000; ; await new Promise((r) => setTimeout(r, 100))) {
    const st = await srv.getJson('/api/status');
    if (st.busy) break;
    if (Date.now() > deadline) throw new Error('carve never went busy');
  }
  // Another window's save of vid_8 is queued first …
  const otherQueued = await otherSave(vid8, 'needs_verification', 'other, queued first', v8Base);
  // … then A marks vid_8 (same base) and vid_7, and adds a note to vid_7.
  await mark(A, vid8, '2');
  await mark(A, vid7, '2');
  await saveNow(A);
  await A.waitForFunction(() => FTViewer.saveState() === 'queued', null, { timeout: 30000 })
    .catch(async (err) => { const s = await srv.getJson("/api/status").catch(() => ({})); throw new Error(`⑫ never "queued": status busy=${s.busy} job=${JSON.stringify(s.job)} other=${JSON.stringify(otherQueued).slice(0, 120)} state=${await saveState(A)} replies=${JSON.stringify(imports).slice(0, 400)} — ${err.message.split('\n')[0]}`); });
  await A.click(`#recordGrid .card[data-id="${vid7}"]`);
  await A.locator('#evidenceNote').fill('queued note');
  await A.waitForFunction(() => FTViewer.summary().pending > 0);
  await saveNow(A);
  await A.waitForFunction((n) => FTViewer.saveState() === 'queued' && FTViewer.summary().pending === 0, null, { timeout: 30000 });
  const deferredReplies = imports.filter((b) => b && b.deferred === true).length;
  check('⑫ both of A\'s saves (and the other window\'s) were queued while the job ran', otherQueued.deferred === true && deferredReplies >= 2, `other=${otherQueued.deferred} A=${deferredReplies}/${imports.length}`);
  await srv.postJson('/api/cancel', {});
  for (const deadline = Date.now() + 120000; ; await new Promise((r) => setTimeout(r, 200))) {
    const st = await srv.getJson('/api/status');
    if (!st.busy && Number(st.pending_marks) === 0) break;
    if (Date.now() > deadline) throw new Error('queue never drained');
  }
  // A's queue watch reads the conflict journal before it may say "saved".
  await A.waitForSelector('#markConflictBanner', { timeout: 60000 }).catch(() => {});
  await A.waitForFunction(() => FTViewer.saveState() === 'pending' || FTViewer.saveState() === 'saved', null, { timeout: 60000 });
  const st12 = await srv.getJson('/api/status');
  const v7Row = await serverRow(vid7);
  checkV10('⑫ same-window chain (mark, then note while queued): both applied, no conflict for vid_7', v7Row.row?.status === 'important' && v7Row.row?.note === 'queued note' && !(st12.mark_conflicts?.ids || []).includes(vid7), `${JSON.stringify(v7Row)} conflicts=${JSON.stringify(st12.mark_conflicts)}`);
  checkV10('⑫ records-meta reports the last writer per id (annotations.writers)', v7Row.writer === aToken3, String(v7Row.writer));
  check('⑫ vid_8: the queued save that met a newer revision is a conflict in A', (st12.mark_conflicts?.ids || []).includes(vid8) && await A.evaluate((v) => !!document.querySelector('#markConflictBanner') && FTViewer.saveState() === 'pending', vid8), JSON.stringify(st12.mark_conflicts));
  const mc = await srv.getJson('/api/mark-conflicts').catch(() => null);
  checkV10('⑫ GET /api/mark-conflicts lists vid_8 with the queued value', !!mc && mc.ok === true && (mc.conflicts || []).some((c) => c.id === vid8 && c.queued), JSON.stringify(mc).slice(0, 200));
  await A.click('#markConflictList');
  const v8Item = await A.locator(`#markConflictPanel [data-id="${vid8}"]`).innerText().catch(() => '');
  check('⑫ list row for vid_8: server = other window (검증 대기), mine = A\'s queued value (중요), marked as a queued save', /서버[\s\S]*검증/.test(v8Item) && /내 값[\s\S]*중요/.test(v8Item) && /대기열/.test(v8Item), v8Item.replace(/\s+/g, ' '));
  const finChecks = await A.evaluate(async () => (await gatherFinalizeChecks()).checks.filter((c) => c.level === 'block').map((c) => c.text));
  check('⑫ shell finalize checklist blocks on the open conflict', finChecks.some((t) => /저장 충돌|저장되지 않은/.test(t)), finChecks.join(' | '));
  await A.click(`#markConflictPanel [data-id="${vid8}"] button[data-side="mine"]`);
  await A.waitForFunction((v) => !document.querySelector(`#markConflictPanel [data-id="${CSS.escape(v)}"]`), vid8);
  // Without the same-client rule (--pre-v10 server) vid_7's second queued
  // body conflicts with its first: keep A's value so the run can go on.
  if (PRE_V10 && await A.locator('#markConflictBanner').count()) await A.click('#markConflictUseMine');
  await A.waitForFunction(() => !document.getElementById('markConflictBanner'));
  await waitState(A, 'saved', 60000);
  const v8Row = await serverRow(vid8);
  const st12b = await srv.getJson('/api/status');
  check('⑫ [내 값] on the queue conflict: server = A\'s queued value, journal resolved', v8Row.row?.status === 'important' && Number(st12b.mark_conflicts?.count) === 0, `${JSON.stringify(v8Row)} ${JSON.stringify(st12b.mark_conflicts)}`);
  await A.unroute('**/api/import-marks');

  // ⑮ R4 A1/A2: IN/OUT ranges are first-class. Another window stored a
  // range on vid_2; this browser still has an older one in the retired
  // whole-map store (RANGES_KEY). After a reload that is an ordinary
  // conflict: i/o, a tag and a cleared mark meanwhile send nothing for the
  // id and never read "saved"; the server keeps its range, tags and mark.
  // vid_2 (vid_6 of the demo is a deliberately broken file: no player).
  const vidR = vid2;
  const vR = await serverRow(vidR);
  const rangeSave = await srv.postJson('/api/import-marks', { marks_json: JSON.stringify({
    schema_version: 2, protocol: 'patch-v2', case_id: caseId, examiner: 'other', client_id: 'w-other-window', exported_unix: 1,
    base_revisions: { [vidR]: vR.revision }, marks: [{ id: vidR, status: 'important', marked_unix: 1, note: '', examiner: 'other', tags: ['현장'], range: { in: 10, out: 20 } }], tags: [], deleted_ids: [],
  }) });
  const vRr = await serverRow(vidR);
  check('⑮ another window stored vid_2 = important, range 10~20, tag 현장', rangeSave.ok === true && vRr.row?.range?.in === 10 && vRr.tags.includes('현장'), JSON.stringify(vRr));
  await A.evaluate(([c, id]) => localStorage.setItem(`ft.viewer.${c}.ranges`, JSON.stringify({ [id]: { in: 3, out: 4 } })), [caseId, vidR]);
  await reloadPage(A);
  await A.waitForSelector('#markConflictBanner', { timeout: 30000 });
  await A.waitForFunction(() => FTViewer.saveState() === 'pending', null, { timeout: 30000 });
  check('⑮ the old browser range is a conflict (not a silent overwrite, not "saved"), the retired key is gone',
    await conflictCount(A) === 1 && await A.evaluate(([c]) => localStorage.getItem(`ft.viewer.${c}.ranges`) === null, [caseId]));
  const rangeImports = [];
  await A.route('**/api/import-marks', async (route) => {
    try { rangeImports.push(JSON.parse(JSON.parse(route.request().postData()).marks_json)); } catch { /* not JSON */ }
    await route.continue();
  });
  await A.click(`#recordGrid .card[data-id="${vidR}"]`);
  await A.evaluate(() => document.activeElement?.blur());
  await A.waitForSelector('#mediaStage video', { state: 'attached', timeout: 30000 })
    .catch(async (err) => {
      const why = await A.evaluate(() => `${document.querySelector('#recordGrid .card.active')?.dataset.id || '-'} | ${(document.getElementById('mediaStage')?.innerHTML || '').slice(0, 300)} | ${document.getElementById('ftViewer').className}`);
      throw new Error(`⑮ no video element: ${why} — ${err.message.split('\n')[0]}`);
    });
  for (const [btn, t] of [['btnSetOut', 9], ['btnSetIn', 5]]) {
    await A.evaluate(([b, v]) => {
      const video = document.querySelector('#mediaStage video');
      Object.defineProperty(video, 'currentTime', { get: () => v, configurable: true });
      document.getElementById(b).click();
    }, [btn, t]);
  }
  await A.evaluate(() => document.getElementById('btnTagMenu').click());
  await A.click('#tagMenuList [data-tag="사고"]');
  await A.keyboard.press('Escape');
  await A.click(`#recordGrid .card[data-id="${vidR}"]`);
  await A.evaluate(() => document.activeElement?.blur());
  await A.keyboard.press('0');
  await saveNow(A);
  await A.waitForFunction(() => FTViewer.saveState() !== 'saving', null, { timeout: 30000 });
  const sentV6 = rangeImports.filter((b) => (b.marks || []).some((m) => m.id === vidR) || (b.deleted_ids || []).includes(vidR) || (b.tags || []).some((t) => t.id === vidR));
  const vRafter = await serverRow(vidR);
  check('⑮ i/o + tag + cleared mark on the conflicted id: nothing sent, still pending',
    sentV6.length === 0 && await saveState(A) === 'pending', `${JSON.stringify(sentV6).slice(0, 200)} state=${await saveState(A)}`);
  check('⑮ the server kept vid_2\'s range, mark and tags (revision unchanged)',
    vRafter.row?.range?.in === 10 && vRafter.row?.range?.out === 20 && vRafter.row?.status === 'important' && vRafter.tags.includes('현장') && !vRafter.tags.includes('사고') && vRafter.revision === vRr.revision, JSON.stringify(vRafter));
  await A.click('#markConflictUseServer');
  await A.waitForFunction(() => !document.getElementById('markConflictBanner'));
  await waitState(A, 'saved', 60000);
  const vRfinal = await serverRow(vidR);
  check('⑮ [서버 값 사용]: saved, the server value untouched', vRfinal.row?.range?.in === 10 && vRfinal.revision === vRr.revision, JSON.stringify(vRfinal));
  await A.unroute('**/api/import-marks');
  check('A/B/C/D no page errors', errors.length === 0, errors.join(' | '));

  // ⑥ Bulk: a standalone viewer page on the SAME origin (so its /api
  // fetches hit the real server) with 30 000 synthetic records, and the
  // origin's localStorage filled to the quota first, so the draft flush
  // really fails (the toast assertion is not vacuous).
  const bulkIds = Array.from({ length: BULK_N }, (_, i) => `vid_bulk_${String(i).padStart(6, '0')}`);
  const bulkData = `window.__FRAMETRACE_DATA__ = ${JSON.stringify({
    manifest: { case_id: caseId, title: 'bulk' }, caseDir,
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
  await bulkPage.evaluate(() => { window.__toasts = []; FTViewer.on('toast', (e) => window.__toasts.push(e.detail.message)); });
  const filled = await bulkPage.evaluate(() => {
    let n = 0;
    for (let size = 1 << 20; size >= 64; size >>= 1) {
      const chunk = 'x'.repeat(size);
      for (;;) { try { localStorage.setItem(`ft.fill.${n}`, chunk); n += 1; } catch { break; } }
    }
    return n;
  });
  check('⑥ storage filled to the quota for the bulk run', filled > 0, `${filled} filler keys`);
  const t0 = Date.now();
  await bulkPage.evaluate(() => document.getElementById('btnSelectFiltered').click());
  await bulkPage.waitForFunction(() => FTViewer.summary() && document.getElementById('selectionCount') && /\d/.test(document.getElementById('selectionCount').textContent));
  await bulkPage.evaluate(() => document.getElementById('btnMarkReviewed').click());
  // Bulk actions above the threshold ask first (in-page dialog).
  const dlg = bulkPage.locator('.modal.dlg button.primary');
  if (await dlg.count()) await dlg.click();
  await bulkPage.waitForFunction((n) => FTViewer.summary().marked === n, BULK_N, { timeout: 120000 });
  const markMs = Date.now() - t0;
  // The flush (250 ms debounce) hits the quota: one toast, the reason in the indicator.
  await bulkPage.waitForFunction(() => /가득|full/i.test(document.getElementById('saveState').title), null, { timeout: 30000 });
  await bulkPage.evaluate(() => document.getElementById('saveState').click());
  await waitState(bulkPage, 'saved', 600000);
  const expectPosts = Math.ceil(BULK_N / 2000);
  check(`⑥ ${BULK_N} bulk marks → ${expectPosts} POSTs (2000 ids each), none 413`, posts.length === expectPosts && posts.every((p) => p.status === 200 && p.ids <= 2000), JSON.stringify(posts.map((p) => [p.ids, p.status, p.batch && p.batch.index])).slice(0, 200));
  check('⑥ chunks share one batch id and the last one is index total-1', posts.every((p) => p.batch && p.batch.id === posts[0].batch.id && p.batch.total === expectPosts) && posts.at(-1).batch.index === expectPosts - 1);
  const bulkMeta = await srv.getJson('/api/records-meta');
  const bulkRevs = Object.keys(bulkMeta.annotations.revisions || {}).filter((id) => id.startsWith('vid_bulk_')).length;
  check(`⑥ server holds ${BULK_N} revisions`, bulkRevs === BULK_N, String(bulkRevs));
  const quotaToasts = await bulkPage.evaluate(() => window.__toasts.filter((t) => /저장소가 가득|storage is full/i.test(t)).length);
  check('⑥ the quota was hit and toasted exactly once', quotaToasts === 1, `${quotaToasts} (marking took ${markMs} ms)`);
  check('⑥ bulk page: no page errors, nothing pending', bulkErrors.length === 0 && await bulkPage.evaluate(() => FTViewer.summary().pending) === 0, bulkErrors.join(' | '));
  await bulkPage.evaluate(() => { for (const k of Object.keys(localStorage)) if (k.startsWith('ft.fill.')) localStorage.removeItem(k); });

  // ⑦ Legacy patch-v1 body straight to the API still applies.
  const legacy = { schema_version: 2, protocol: 'patch-v1', case_id: caseId, examiner: 'legacy', exported_unix: 1, marks: [{ id: vid1, status: 'important', marked_unix: 7, note: 'legacy note', examiner: 'legacy' }], tags: [{ id: vid1, tags: ['레거시'] }], deleted_ids: [] };
  const legacyRes = await srv.postJson('/api/import-marks', { marks_json: JSON.stringify(legacy) });
  const legacyRow = await serverRow(vid1);
  check('⑦ legacy patch-v1 body: 200, applied map, value stored', legacyRes.ok === true && legacyRes.applied && typeof legacyRes.applied[vid1] === 'number' && legacyRow.row?.status === 'important' && legacyRow.row?.note === 'legacy note' && legacyRow.tags.includes('레거시'), JSON.stringify(legacyRes).slice(0, 200));

  // ⑯ R6 U1/U2 ↔ S2: the real marks-pending route, read the viewer's way.
  {
    const qdir = join(caseDir, 'marks-pending');
    const preexisting = existsSync(qdir);
    mkdirSync(qdir, { recursive: true });
    const seqName = (n) => `${String(n).padStart(32, '0')}.json`;
    const big = join(qdir, seqName(900000001));
    const bad = join(qdir, seqName(900000002));
    const ids = Array.from({ length: 800 }, (_, i) => `vid_qpage_${String(i).padStart(4, '0')}`);
    writeFileSync(big, JSON.stringify({ schema_version: 2, protocol: 'patch-v2', case_id: caseId, examiner: 'q', client_id: 'w-queue-test', exported_unix: 1,
      base_revisions: Object.fromEntries(ids.map((id) => [id, 0])), marks: ids.map((id) => ({ id, status: 'reviewed', marked_unix: 1, note: '', examiner: 'q', tags: [], range: { in: null, out: null } })), tags: [], deleted_ids: [] }));
    writeFileSync(bad, '{"schema_version":2,"marks":[{"id":"vid_qbroken"'); // cut short: unreadable
    try {
      const fnSrc = viewerBundleJs(root).match(/^async function readPagedList\([^]*?^}/m)[0];
      const queries = [];
      const ctx = { get: async (path) => { queries.push(path); return srv.getJson(path); } };
      const list = await runInNewContext(fnSrc + '\nvar CONFLICT_PAGE = 500, CONFLICT_PAGES_MAX = 2000;\nreadPagedList(get, "/api/marks-pending", (d) => Array.isArray(d.entries) ? d.entries : null);', ctx);
      const got = new Set((list || []).map((e) => e && e.id).filter((id) => id && id.startsWith('vid_qpage_')));
      const unread = (list && list.unreadable) || [];
      const page1 = await srv.getJson('/api/marks-pending?offset=0&limit=500');
      const stringCursor = typeof page1.next_after === 'string';
      check('⑯ U2: the cut-short queue file is reported unreadable (carried by the viewer\'s reader)', unread.some((u) => String(u.file || '').endsWith(seqName(900000002))), JSON.stringify(unread).slice(0, 200));
      // R7 U3: S2 is merged — the string cursor is REQUIRED. (R6 accepted a
      // numeric cursor as long as the short read was flagged — a fallback
      // that let a regressed server pass; it is a failure now.)
      const cursors = (page1.entries || []).map((e) => e && e.cursor);
      check('⑯ R7 U3 (S2): the server pages marks-pending by the "<seq>:<idx>" string cursor', stringCursor && /^\d+:\d+$/.test(page1.next_after) && cursors.length > 0 && cursors.every((c) => typeof c === 'string' && /^\d+:\d+$/.test(c)), `next_after=${JSON.stringify(page1.next_after)} cursors ${JSON.stringify(cursors.slice(0, 3))}`);
      check('⑯ U1 (S2): 800 ids of ONE queue file read in full across the page boundary (string cursor, sent back URL-encoded)', got.size === 800 && !list.truncated && queries.length >= 2 && queries.slice(1).every((q) => /[?&]after=\d+%3A\d+&/.test(q)), `${got.size} ids, truncated=${list && list.truncated}, queries ${JSON.stringify(queries)}`);
    } finally {
      rmSync(big, { force: true });
      rmSync(bad, { force: true });
      if (!preexisting) rmSync(qdir, { recursive: true, force: true });
    }
  }
} catch (err) {
  check('harness ran to the end', false, err && err.stack || String(err));
  console.log('--- server log (tail) ---\n' + srv.log().split('\n').slice(-25).join('\n'));
} finally {
  if (browser) await browser.close();
  await srv.stop();
  rmSync(work, { recursive: true, force: true, maxRetries: 10, retryDelay: 200 });
}
console.log(failed ? `verify-marks-sync: ${failed} failed` : 'verify-marks-sync: all checks passed');
process.exit(failed ? 1 : 0);
