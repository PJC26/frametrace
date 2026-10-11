// Regression checks for the adversarial review of the rebuilt shell
// (assets/app/*). VM harness (scripts/lib/shell-vm.mjs): the real app
// bundle with a fake DOM, scripted fetch, virtual timers and an FTViewer
// mock — no server, no browser.
//
//   node scripts/verify-rebuild-regressions.mjs
//
// B1  a transient server loss keeps the review screen, the open sheet and
//     the mounted viewer; the banner says so once; polls recover it.
// B2  status polls that change nothing do not re-check the deliverables
//     sheet; a meaningful change re-checks it without a "점검 중…" flash.
// S1  unattributed marks (counted from records-meta) block the report with
//     a one-click "현재 이름으로 귀속"; the strip sets the examiner before
//     any mark without the sheet.
// S2  FTViewer.reload() while popped out folds the media column again.
// S4  a peer tab frees its pop-out button on pagehide / a stale heartbeat.
// S5  toasts while the modal sheet is open go to the body-level box.
// N   finalize re-checks the save state right before POST; "local" with
//     pending edits is unsaved everywhere; opening a sheet closes viewer
//     menus (window "resize").
import { makeEnv, test, assert } from './lib/shell-vm.mjs';

const READY = { has_job: true, phase: 'review-ready', busy: false, case_dir: '/cases/c1', pending_marks: 0, instance: 'i1' };
const META = (marks = []) => ({ json: { ok: true, manifest: { case_id: 'CASE-1' }, annotations: { marks, tags: [] } } });
const AUDIT = () => ({ json: { ok: true, integrity: 'integrity-structural', verified: 1, entries: 3, logs: [] } });

// A project is open, the viewer mounted and "ready".
async function projectEnv(opts = {}) {
  const env = makeEnv({ status: { ...READY, ...(opts.status || {}) }, globals: opts.globals });
  env.routes['GET /api/records-meta'] = () => META(opts.marks || []);
  env.routes['POST /api/verify-audit'] = AUDIT;
  const V = env.ctx.FTViewer;
  env.examiner = '';
  env.setExaminerCalls = [];
  V.getExaminer = () => env.examiner;
  V.setExaminer = (n) => { env.setExaminerCalls.push(n); env.examiner = String(n); };
  V.isPoppedOut = () => false;
  V.popout = () => ({ focus() {} });
  V.closePopout = () => {};
  await env.advance(1200);
  env.viewer.emit('ready', { total: 0, live: false, generation: 1 });
  await env.advance(50);
  assert.equal(env.ctx.document.documentElement.dataset.mode, 'project', 'project mode expected');
  assert.equal(env.state().viewer.mounted, true, 'viewer mounted');
  return env;
}
// Counts every write of a property on a fake element (and remembers them).
function spy(el, prop) {
  let v = el[prop];
  const writes = [];
  Object.defineProperty(el, prop, { configurable: true, get: () => v, set: (x) => { writes.push(x); v = x; } });
  return writes;
}
const metaCalls = (env) => env.calls.filter((c) => c.url === '/api/records-meta').length;
// Fake-DOM click on a button the finalize sheet rendered through innerHTML.
function clickInSheet(env, id) {
  const btn = { id, dataset: {}, disabled: false };
  env.$('hFinalizeSheet').dispatch('click', { target: { closest: (sel) => (sel.includes('#hFinalizeClose') ? null : btn) } });
}

await test('B1. server loss keeps the review screen, the open sheet and the viewer; recovers', async () => {
  const env = await projectEnv();
  env.$('hBtnFinalize').click();
  await env.advance(1000);
  assert.equal(env.state().panels.finalize, true);
  const msgWrites = spy(env.$('serverBannerMsg'), 'textContent');
  env.routes['GET /api/status'] = () => { throw new Error('down'); };
  await env.advance(60000);
  assert.equal(env.state().server.lost, true, 'server.lost');
  assert.equal(env.$('serverBanner').hidden, false, 'banner shown');
  assert.equal(env.state().phase, 'review-ready', 'last good phase kept');
  assert.equal(env.ctx.document.documentElement.dataset.mode, 'project', 'mode stays project');
  assert.equal(env.$('hViewerHost').hidden, false, 'viewer host stays visible');
  assert.equal(env.$('inputCard').hidden, true, 'empty state stays hidden');
  assert.equal(env.state().panels.finalize, true, 'deliverables sheet stays open');
  assert.equal(env.$('hFinalizeSheet').hidden, false);
  assert.equal(env.state().viewer.mounted, true);
  assert.ok(msgWrites.length <= 1, 'banner text written once, not per failed poll: ' + msgWrites.length);
  delete env.routes['GET /api/status'];
  await env.advance(15000);
  assert.equal(env.state().server.lost, false);
  assert.equal(env.$('serverBanner').hidden, true);
  assert.equal(env.state().panels.finalize, true);
  assert.equal(env.$('hViewerHost').hidden, false);
});

await test('B2. idle polls never re-check the sheet; a real change re-checks without "점검 중…"', async () => {
  const env = await projectEnv();
  env.$('hBtnFinalize').click();
  await env.advance(2000);
  const verdictWrites = spy(env.$('finalVerdict'), 'innerHTML');
  const listWrites = spy(env.$('finalChecklist'), 'innerHTML');
  const before = metaCalls(env);
  await env.advance(30000);
  assert.equal(metaCalls(env) - before, 0, 'records-meta re-fetched by idle polls');
  assert.equal(verdictWrites.length, 0, 'verdict (role=status) rewritten by idle polls');
  assert.equal(listWrites.length, 0, 'checklist rewritten by idle polls');
  env.status = { ...env.status, pending_marks: 2 };
  await env.advance(8000);
  assert.ok(metaCalls(env) > before, 'a pending-marks change re-checks');
  assert.ok(!verdictWrites.some((w) => /점검 중/.test(w)), 'background re-check flashed "점검 중…"');
  assert.match(env.$('finalChecklist').innerHTML, /저장 대기 마크 2건/);
  // Selection / active card changes are not checklist inputs either.
  const mid = metaCalls(env);
  env.viewer.emit('selection', { ids: ['a'], count: 1 });
  env.viewer.emit('active-record', { id: 'a' });
  await env.advance(2000);
  assert.equal(metaCalls(env), mid, 'selection change re-checked the sheet');
});

await test('S1a. unattributed marks block the report; one click records the name on exactly those', async () => {
  const marks = [
    { record_id: 'v1', status: 'important', marked_unix: 100, examiner: null, note: 'memo' },
    { record_id: 'v2', status: 'reviewed', marked_unix: 200, examiner: 'Kim', note: '' },
  ];
  const env = await projectEnv({ marks });
  env.$('hBtnFinalize').click();
  await env.advance(2000);
  const html = env.$('finalChecklist').innerHTML;
  assert.match(html, /h-check-block[^]*검토자 이름 없이 기록된 마크 1건/, 'block row with the count from records-meta');
  assert.match(html, /id="btnFinalAttribute"[^>]*>현재 이름으로 귀속</);
  let posted = null;
  env.routes['POST /api/import-marks'] = (body) => { posted = JSON.parse(body.marks_json); return { json: { ok: true } }; };
  env.$('finalExaminer').value = 'Hong';
  clickInSheet(env, 'btnFinalAttribute');
  env.routes['GET /api/records-meta'] = () => META(marks.map((m) => ({ ...m, examiner: m.examiner || 'Hong' })));
  await env.advance(2000);
  assert.ok(posted, 'POST /api/import-marks');
  assert.equal(posted.case_id, 'CASE-1');
  assert.equal(posted.examiner, 'Hong');
  assert.deepEqual(posted.marks, [{ id: 'v1', status: 'important', examiner: 'Hong', marked_unix: 100 }], 'only the unattributed mark, note/range left to the server');
  assert.deepEqual(env.setExaminerCalls.slice(-1), ['Hong'], 'FTViewer.setExaminer');
  assert.doesNotMatch(env.$('finalChecklist').innerHTML, /검토자 이름 없이/, 'block row gone after re-check');
});

await test('S1a-1. with FTViewer.attributeUnassignedMarks the viewer does it (no direct POST)', async () => {
  const marks = [{ record_id: 'v1', status: 'important', marked_unix: 100, examiner: null }];
  const env = await projectEnv({ marks });
  const calls = [];
  env.ctx.FTViewer.attributeUnassignedMarks = async (n) => { calls.push(n); env.routes['GET /api/records-meta'] = () => META([{ ...marks[0], examiner: n }]); return 1; };
  env.$('hBtnFinalize').click();
  await env.advance(2000);
  env.$('finalExaminer').value = 'Hong';
  clickInSheet(env, 'btnFinalAttribute');
  await env.advance(2000);
  assert.deepEqual(calls, ['Hong']);
  assert.equal(env.calls.filter((c) => c.url === '/api/import-marks').length, 0);
  assert.deepEqual(env.setExaminerCalls.slice(-1), ['Hong']);
  assert.doesNotMatch(env.$('finalChecklist').innerHTML, /검토자 이름 없이/);
});

await test('S1a-2. a failed attribution is never silent', async () => {
  const env = await projectEnv({ marks: [{ record_id: 'v1', status: 'reviewed', marked_unix: 1, examiner: null }] });
  env.$('hBtnFinalize').click();
  await env.advance(2000);
  env.routes['POST /api/import-marks'] = () => ({ json: { ok: false, error: 'busy' } });
  env.$('finalExaminer').value = 'Hong';
  clickInSheet(env, 'btnFinalAttribute');
  await env.advance(2000);
  assert.match(env.$('finalHint').textContent, /기록하지 못했습니다: busy/);
  assert.match(env.$('finalChecklist').innerHTML, /검토자 이름 없이 기록된 마크 1건/);
});

await test('S1b. the strip sets the examiner before any mark, without the sheet', async () => {
  const env = await projectEnv();
  assert.equal(env.$('hExaminerWrap').hidden, false, 'shown in project mode');
  assert.equal(env.$('hExaminerName').textContent, '미설정');
  env.$('hBtnExaminer').click();
  assert.equal(env.state().panels.examiner, true, 'popover open');
  assert.equal(env.$('hExaminerPop').hidden, false);
  env.$('hExaminerInput').value = '';
  env.$('hBtnExaminerSave').click();
  assert.equal(env.$('hExaminerPop').hidden, false, 'empty name keeps the popover open');
  assert.deepEqual(env.setExaminerCalls, []);
  env.$('hExaminerInput').value = '  Lee (과학수사)  ';
  env.$('hBtnExaminerSave').click();
  assert.deepEqual(env.setExaminerCalls, ['Lee (과학수사)'], 'FTViewer.setExaminer');
  assert.equal(JSON.parse(env.ctx.localStorage.getItem('ft.examiner.prefs')).examinerName, 'Lee (과학수사)', 'pref persisted');
  assert.equal(env.$('hExaminerName').textContent, 'Lee (과학수사)');
  assert.equal(env.$('hExaminerPop').hidden, true, 'popover closed');
  assert.equal(env.state().panels.examiner, false);
});

await test('S2. reload() while popped out folds the media column again', async () => {
  const env = await projectEnv();
  const root = env.$('ftRootFake');
  env.$('hViewerHost').querySelector = () => root;
  env.ev('hSetPopped(true)');
  assert.equal(root.classList.contains('popped'), true);
  root.classList.remove('popped'); // destroy()+init() restores pristine classes
  env.viewer.emit('ready', { total: 0 });
  await env.advance(10);
  assert.equal(root.classList.contains('popped'), true, 'popped re-applied on ready');
  assert.equal(env.$('hPopoutNote').hidden, false);
});

await test('S4. pagehide frees the peers; a stale peer pop-out times out', async () => {
  const sent = [];
  class FakeBC { constructor() { this.onmessage = null; } postMessage(m) { sent.push(m); } close() {} }
  const env = await projectEnv({ globals: { BroadcastChannel: FakeBC } });
  assert.ok(sent.some((m) => m.type === 'popout-query'), 'a new tab asks who owns the pop-out');
  // Peer side: another tab announced its pop-out, then went silent.
  env.ev('hChan').onmessage({ data: { type: 'popout-open', from: 'peerA', caseDir: '/cases/c1' } });
  assert.equal(env.$('hBtnPopout').disabled, true, 'disabled while a peer owns it');
  await env.advance(16000);
  assert.equal(env.$('hBtnPopout').disabled, false, 'stale peer state timed out');
  // Owner side: closing the tab tells the peers.
  env.ev('hSetPopped(true)');
  sent.length = 0;
  await env.advance(5100);
  assert.ok(sent.some((m) => m.type === 'popout-open'), 'owner heartbeat');
  env.winEvent('pagehide');
  assert.ok(sent.some((m) => m.type === 'popout-closed'), 'popout-closed broadcast on pagehide');
});

await test('S5. toasts while the modal sheet is open go to the body-level box', async () => {
  const env = await projectEnv();
  env.ev('toast("before")');
  assert.equal(env.viewer.toasts.length, 1, 'viewer host while no sheet');
  env.$('hBtnFinalize').click();
  await env.advance(500);
  const box = env.$('hToastBox');
  const n = box.children.length;
  env.ev('toast("during", "warn")');
  assert.equal(env.viewer.toasts.length, 1, 'not into the inert viewer host');
  assert.equal(box.children.length, n + 1, 'into #hToastBox');
  let prevented = false;
  env.viewer.emit('toast', { message: '저장 실패', kind: 'error' }, { preventDefault() { prevented = true; } });
  assert.equal(prevented, true, "the viewer's own toast is taken over");
  assert.equal(box.children.length, n + 2);
});

await test('N1. finalize re-checks the save state right before POST /api/finalize', async () => {
  const env = await projectEnv();
  env.examiner = 'Hong';
  env.$('hBtnFinalize').click();
  await env.advance(2000);
  env.$('finalExaminer').value = 'Hong';
  // A mark lands while the checklist is being gathered.
  env.ctx.FTViewer.setExaminer = (n) => { env.examiner = n; env.viewer.saved = 'pending'; };
  clickInSheet(env, 'btnFinalize');
  await env.advance(3000);
  assert.equal(env.calls.filter((c) => c.url === '/api/finalize').length, 0, 'sealed with an unsaved mark');
  assert.match(env.$('finalHint').textContent, /보고서를 만들지 않았습니다/);
});

await test('N2. "local" with pending edits is unsaved for every guard; clean "local" is not', async () => {
  const env = await projectEnv();
  env.viewer.saved = 'local';
  env.ctx.FTViewer.summary = () => ({ total: 0, byKind: {}, byStatus: {}, pending: 0 });
  const ok = await Promise.race([env.ev('hConfirmLeave("t")'), env.flush().then(() => 'hung')]);
  assert.equal(ok, true, 'clean local: no dialog');
  assert.equal(env.$('dlgBackdrop').hidden, true);
  env.ctx.FTViewer.summary = () => ({ total: 0, byKind: {}, byStatus: {}, pending: 2 });
  assert.equal(env.ev('hViewer.hasUnsaved()'), true, 'local + pending = unsaved');
  const before = env.viewer.saveCalls;
  await env.ev('hostViewerSwitch(async () => false)');
  assert.equal(env.viewer.saveCalls, before + 1, 'switch saves local edits first');
});

await test('N3. opening a sheet closes viewer menus first (Esc reaches the sheet)', async () => {
  const env = await projectEnv();
  let resized = 0;
  env.ctx.addEventListener('resize', () => { resized += 1; });
  env.$('hBtnFinalize').click();
  assert.ok(resized >= 1, 'window resize dispatched when the sheet opens');
  const e = env.docKey('Escape');
  assert.equal(e.prevented, true);
  assert.equal(env.state().panels.finalize, false, 'first Esc closes the sheet');
});
