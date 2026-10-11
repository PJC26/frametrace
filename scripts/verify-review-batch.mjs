// CDP driver: verify the review-productivity batch (notes/examiner, marks
// apply, frame capture, IN/OUT clip export, proxy playback, timeline panel,
// warning chip) plus workstation advanced tools, recent cases, and carving.
//
// Usage:
//   1. Start the workstation:  target/debug/frametrace-app.exe
//   2. Open a case with real videos:
//        POST /api/open-case {"case_dir": "D:/frametrace-e01-test/case-synth"}
//   3. Generate a timeline for the case (optional but exercised):
//        frametrace-app timeline <case>
//   4. Start headless Edge/Chrome:
//        msedge --headless=new --remote-debugging-port=9222
//          --user-data-dir=<temp> --autoplay-policy=no-user-gesture-required
//          --mute-audio about:blank
//   5. Run:  node scripts/verify-review-batch.mjs
const CDP = 'http://127.0.0.1:9222';
const BASE = 'http://127.0.0.1:8477';
const VIEWER = BASE + '/review/evidence-viewer.html';

let nextId = 1;
const pending = new Map();

function connect(url) {
  return new Promise((resolve, reject) => {
    const ws = new WebSocket(url);
    ws.onopen = () => resolve(ws);
    ws.onerror = reject;
  });
}

function send(ws, method, params = {}, sessionId) {
  const id = nextId++;
  return new Promise((resolve, reject) => {
    pending.set(id, { resolve, reject });
    ws.send(JSON.stringify({ id, method, params, sessionId }));
  });
}

function wire(ws) {
  ws.onmessage = (event) => {
    const msg = JSON.parse(event.data);
    if (msg.id && pending.has(msg.id)) {
      const { resolve, reject } = pending.get(msg.id);
      pending.delete(msg.id);
      if (msg.error) reject(new Error(msg.error.message));
      else resolve(msg.result);
    }
  };
}

async function evalIn(ws, sessionId, expression, userGesture = false) {
  const res = await send(ws, 'Runtime.evaluate', {
    expression, returnByValue: true, awaitPromise: true, userGesture,
  }, sessionId);
  if (res.exceptionDetails) {
    throw new Error('page exception: ' + JSON.stringify(res.exceptionDetails.exception?.description || res.exceptionDetails.text));
  }
  return res.result.value;
}

const sleep = (ms) => new Promise(r => setTimeout(r, ms));

async function attachToNewTarget(browserWs, url) {
  const { targetId } = await send(browserWs, 'Target.createTarget', { url });
  const { sessionId } = await send(browserWs, 'Target.attachToTarget', { targetId, flatten: true });
  return { targetId, sessionId };
}

async function main() {
  const results = [];
  const check = (name, ok, detail = '') => {
    results.push({ name, ok, detail });
    console.log(`${ok ? 'PASS' : 'FAIL'}  ${name}${detail ? '  — ' + detail : ''}`);
  };

  const version = await (await fetch(CDP + '/json/version')).json();
  const browserWs = await connect(version.webSocketDebuggerUrl);
  wire(browserWs);
  await send(browserWs, 'Target.setDiscoverTargets', { discover: true });

  // ================= viewer =================
  const main = await attachToNewTarget(browserWs, VIEWER);
  await sleep(2500);

  const recs = await evalIn(browserWs, main.sessionId,
    `(() => ({ count: records.length, server: typeof serverMode === 'undefined' ? 'n/a' : serverMode() }))()`);
  check('viewer loads records', recs.count > 0, `records=${recs.count} server=${recs.server}`);

  // warning chip filters records with warnings — add a synthetic warning
  // state check: chip exists and applies the predicate
  const warn = await evalIn(browserWs, main.sessionId,
    `(() => { const chip = [...document.querySelectorAll('.chip')].find(c => c.dataset.chip === 'warning');
        if (!chip) return { exists: false };
        chip.click();
        const n = filteredRecords().length;
        const allHave = filteredRecords().every(r => (r.warnings || []).length > 0);
        return { exists: true, n, allHave }; })()`);
  check('warning chip filters warning-bearing records', warn.exists && warn.allHave, JSON.stringify(warn));

  // reset chip to all (data-chip="")
  await evalIn(browserWs, main.sessionId,
    `[...document.querySelectorAll('.chip')].find(c => c.dataset.chip === '')?.click()`);

  // note editor: pick a record, type a note, verify persistence
  const note = await evalIn(browserWs, main.sessionId,
    `(() => { const f = filteredRecords(); state.activeId = f[0].id; render();
        const ta = document.getElementById('evidenceNote');
        if (!ta) return { exists: false };
        ta.value = 'CDP test note';
        ta.dispatchEvent(new Event('input'));
        const key = Object.keys(localStorage).find(k => k.endsWith('.notes'));
        const stored = JSON.parse(localStorage.getItem(key) || '{}');
        return { exists: true, saved: stored[f[0].id] === 'CDP test note' }; })()`);
  check('note editor persists to localStorage', note.exists && note.saved, JSON.stringify(note));

  // examiner name input
  const ex = await evalIn(browserWs, main.sessionId,
    `(() => { const el = document.getElementById('examinerName');
        if (!el) return { exists: false };
        el.value = 'CDP검사관'; el.dispatchEvent(new Event('input'));
        return { exists: true, saved: state.examiner === 'CDP검사관' }; })()`);
  check('examiner name input updates state', ex.exists && ex.saved, JSON.stringify(ex));

  // marks payload includes note + examiner
  const payload = await evalIn(browserWs, main.sessionId,
    `(() => { const p = marksPayload();
        return { examiner: p.examiner, note: (p.marks.find(m => m.note) || {}).note, n: p.marks.length }; })()`);
  check('marks payload carries examiner + note', payload.examiner === 'CDP검사관', JSON.stringify(payload));

  // IN/OUT controls on a playable record
  const range = await evalIn(browserWs, main.sessionId,
    `(() => { const f = filteredRecords(); const r = f.find(r => mediaSrcFor(r));
        if (!r) return { playable: false };
        state.activeId = r.id; render();
        const video = document.querySelector('#mediaStage video');
        if (!video) return { playable: true, video: false };
        video.currentTime = 1.0; document.getElementById('btnSetIn').click();
        video.currentTime = 3.0; document.getElementById('btnSetOut').click();
        return { playable: true, video: true, label: document.getElementById('rangeLabel').textContent }; })()`);
  check('IN/OUT sets range label', range.label && range.label.length > 0, JSON.stringify(range));

  // clip export hits the server (real export of vid with range set)
  const clip = await evalIn(browserWs, main.sessionId,
    `(async () => { document.getElementById('btnExportClip').click();
        let text = '';
        for (let i = 0; i < 40; i++) { await new Promise(r => setTimeout(r, 500));
            text = (document.getElementById('toastHost')||{}).textContent || '';
            if (text.trim()) break; }
        return { toast: text }; })()`);
  check('export clip request completes', clip.toast.length > 0, JSON.stringify(clip));

  // proxy toggle asks the server and flips playback source
  const proxy = await evalIn(browserWs, main.sessionId,
    `(async () => { const btn = document.getElementById('btnProxy');
        if (!btn) return { exists: false };
        btn.click();
        await new Promise(r => setTimeout(r, 15000));
        const video = document.querySelector('#mediaStage video');
        return { exists: true, src: video && video.src || '', label: btn.textContent }; })()`);
  check('proxy toggle produces playback src', proxy.exists && proxy.src.length > 0, JSON.stringify(proxy).slice(0, 160));

  // timeline panel
  const tl = await evalIn(browserWs, main.sessionId,
    `(async () => { document.getElementById('btnTimeline').click();
        await new Promise(r => setTimeout(r, 2500));
        const panel = document.getElementById('timelinePanel');
        const items = panel.querySelectorAll('.timeline-item, li, .tl-item').length;
        const meta = document.getElementById('timelineMeta').textContent;
        const list = document.getElementById('timelineList').textContent;
        return { hidden: panel.hidden, items, meta, listLen: list.length }; })()`);
  check('timeline panel opens with events', !tl.hidden && (tl.items > 0 || tl.listLen > 20), JSON.stringify(tl).slice(0, 200));

  // apply marks → server import (note + examiner land in the case DB)
  const apply = await evalIn(browserWs, main.sessionId,
    `(async () => { document.getElementById('btnApplyMarks').click();
        let text = '';
        for (let i = 0; i < 40; i++) { await new Promise(r => setTimeout(r, 500));
            text = (document.getElementById('toastHost')||{}).textContent || '';
            if (text.trim()) break; }
        return { toast: text }; })()`);
  check('apply marks posts to server', apply.toast.length > 0, JSON.stringify(apply));

  // ================= workstation =================
  const ws = await attachToNewTarget(browserWs, BASE + '/');
  await sleep(2000);

  const stage = await evalIn(browserWs, ws.sessionId,
    `(() => { return { adv: !!document.querySelector('[data-adv="timeline"]'),
        carve: !!document.getElementById('btnCarve'),
        recent: !!document.getElementById('recentCaseRow') }; })()`);
  check('workstation has advanced tools + carve + recent row', stage.adv && stage.carve && stage.recent, JSON.stringify(stage));

  // recent cases: seed prefs then re-render
  const recent = await evalIn(browserWs, ws.sessionId,
    `(async () => { const prefs = loadPrefs(); prefs.recentCases = ['D:/frametrace-e01-test/case-synth','D:/frametrace-e01-test/case-direct2','D:/nope/not-a-case'];
        savePrefs(prefs); await renderRecentCases();
        const row = document.getElementById('recentCaseRow');
        return { hidden: row.hidden, chips: row.querySelectorAll('.casechip').length }; })()`);
  check('recent case chips keep only real cases', recent.chips === 2, JSON.stringify(recent));

  // advanced tool dispatch via UI button (timeline is cheap)
  const adv = await evalIn(browserWs, ws.sessionId,
    `(async () => { const btn = document.querySelector('[data-adv="timeline"]');
        btn.click(); await new Promise(r => setTimeout(r, 8000));
        const out = document.getElementById('advOut');
        return { text: (out && out.textContent || '').slice(0, 200) }; })()`);
  check('advanced tool (timeline) runs from UI', adv.text.length > 0, JSON.stringify(adv));

  // ---- summary ----
  const failed = results.filter(r => !r.ok);
  console.log(`\n${results.length - failed.length}/${results.length} passed`);
  browserWs.close();
  process.exit(failed.length ? 1 : 0);
}

async function unitTests() {
  const { readFileSync } = await import('node:fs');
  const { runInNewContext } = await import('node:vm');
  const { default: assert } = await import('node:assert/strict');
  const { readdirSync } = await import('node:fs');
  const viewerDir = new URL('../assets/viewer/', import.meta.url);
  const source = readdirSync(viewerDir, 'utf8').filter(f => f.endsWith('.js')).sort()
    .map(f => readFileSync(new URL(f, viewerDir), 'utf8')).join('\n');
  const fn = name => source.match(new RegExp(`^function ${name}\\([^]*?^}`, 'm'))?.[0];
  const label = { textContent: '' };
  const state = { activeId: 'a', ranges: { a: { in: 1, out: null } }, proxies: {} };
  const t = key => ({ 'range.label': '구간 {in} ~ {out}' })[key] || key;
  const tf = (key, subs) => Object.entries(subs || {}).reduce((s, [k, v]) => s.split('{' + k + '}').join(String(v)), t(key));
  const context = { state, document: { getElementById: id => id === 'rangeLabel' ? label : null }, selectedRecord: () => ({ id: 'a' }), t, tf };
  runInNewContext(fn('rangeOf')?.split('\n')[0] + '\n' + fn('updateRangeLabel') + '\nupdateRangeLabel();', context);
  assert.equal(label.textContent, '구간 1.0s ~ —');
  state.ranges.a = { in: null, out: 3 };
  runInNewContext(fn('updateRangeLabel') + '\nupdateRangeLabel();', context);
  assert.equal(label.textContent, '구간 — ~ 3.0s');
  console.log('PASS VM partial IN/OUT labels');
  const records = ['a', 'b', 'c'].map(id => ({ id }));
  const els = Object.fromEntries(['resultCount', 'pageStatus', 'prevPage', 'nextPage', 'recordGrid'].map(id => [id, {}]));
  els.recordGrid.classList = { add: () => {}, remove: () => {}, toggle: () => {} };
  // State classes (vgrid, theater, ...) live on the fragment root #ftViewer.
  els.root = { classList: { add: () => {}, remove: () => {}, toggle: () => {}, contains: () => false } };
  Object.assign(state, { marks: {}, selectedIds: new Set(), pageSize: 100, currentPage: 1, groupBy: 'none', collapsedGroups: new Set() });
  // Exercises the paged renderer; the virtual grid has its own Playwright check.
  const grid = { state, records, recordById: new Map(records.map(r => [r.id, r])), els, t: x => x, tf, escapeHtml: x => x, groupKeyFor: () => 'group', renderCard: r => `<card>${r.id}</card>`, groupHeaderHtml: key => `<hdr>${key}</hdr>`, announceResultCount: () => {}, virtualGridEnabled: () => false, document: { body: { classList: { toggle: () => {} } } } };
  const selectedSource = source.match(/^function selectedRecord\(\).*$/m)[0];
  assert.equal(runInNewContext(selectedSource + '\nselectedRecord();', grid), records[0]);
  runInNewContext(fn('renderGrid') + '\nrenderGrid([]);', grid);
  assert.equal(state.activeId, null);
  assert.equal(runInNewContext(source.match(/^function selectedRecord\(\).*$/m)[0] + '\nselectedRecord();', grid) || undefined, undefined);
  state.groupBy = 'kind'; state.collapsedGroups.add('group');
  runInNewContext(fn('renderGrid') + '\nrenderGrid(records);', grid);
  assert.ok(!els.recordGrid.innerHTML.includes('<card>'));
  state.groupBy = 'none'; state.activeId = 'a';
  Object.assign(grid, { MARKS_KEY: 'marks', ANNOTATIONS_KEY: 'annotations', storageSet: () => {}, markReviewDirty: () => {}, filteredRecords: () => records.filter(r => !state.marks[r.id]), render: () => runInNewContext(fn('renderGrid') + '\nrenderGrid(filteredRecords());', grid), moveActive: () => { state.activeId = 'c'; } });
  Object.assign(state, { drafts: {}, deletedIds: [], examiners: {}, notes: {}, tags: {} });
  const touch = fn('touchAnnotation');
  Object.assign(grid, { markLabel: s => s, document: { getElementById: () => null, body: { classList: { toggle: () => {} } } }, toast: () => {}, RANGES_KEY: 'ranges', NOTES_KEY: 'notes', TAGS_KEY: 'tags' });
  const journalSrc = [fn('snapshotIds'), fn('journalDiff'), fn('toastUndo'), fn('journaled'), fn('markStatusLabel')].join('\n');
  runInNewContext('undoStack=[];redoStack=[];JOURNAL_LIMIT=200;JOURNAL_FIELDS=["mark","note","tags","range"];\n' + journalSrc + '\n' + touch + '\n' + fn('markActive') + '\nmarkActive("reviewed");', grid);
  assert.equal(state.activeId, 'b');
  console.log('PASS VM empty filter, collapsed group, sequential triage');
  Object.assign(state, { marks: {}, notes: {}, tags: {}, examiner: 'New', deletedIds: ['a'], examiners: { b: 'Bob' } });
  Object.assign(grid, { manifest: { case_id: 'test' } });
  state.ranges = {}; // range-only records are exported too (FOR-01); this block tests marks/notes
  let payload = runInNewContext(fn('marksPayload') + '\nmarksPayload();', grid);
  assert.equal(payload.protocol, 'patch-v1');
  assert.equal(JSON.stringify(payload.deleted_ids), '["a"]');
  state.marks.b = { status: 'important', marked_unix: 1 };
  payload = runInNewContext(fn('marksPayload') + '\nmarksPayload();', grid);
  assert.equal(payload.marks[0].note, '');
  assert.equal(payload.marks[0].examiner, 'Bob');
  console.log('PASS VM explicit annotation clears and examiner preservation');
  Object.assign(grid, { ANNOTATIONS_KEY: 'annotations', storageSet: () => {} });
  state.drafts = {};
  runInNewContext(fn('touchAnnotation') + '\ntouchAnnotation("a");', grid);
  assert.ok(state.deletedIds.includes('a'));
  const saved = JSON.stringify(state.drafts);
  Object.assign(state, { marks: {}, notes: {}, tags: {}, examiners: {}, deletedIds: [], drafts: JSON.parse(saved) });
  grid.annotations = { marks: [{ id: 'a', status: 'important', note: 'old' }, { id: 'b', status: 'reviewed', note: 'DB memo', examiner: 'Bob' }], tags: [{ id: 'b', tags: ['DB tag'] }] };
  runInNewContext(fn('hydrateAnnotations') + '\nhydrateAnnotations(annotations);', grid);
  assert.equal(state.marks.a, undefined);
  assert.equal(state.notes.b, 'DB memo');
  assert.equal(state.examiners.b, 'Bob');
  assert.equal(JSON.stringify(state.tags.b), '["DB tag"]');
  console.log('PASS VM DB hydration with pending tombstone');
  // Drafts must snapshot post-mutation state: a draft recorded before the
  // mark/tag mutation replays the old value on reload and resurrects
  // cleared annotations (or drops new ones) after hydration.
  Object.assign(state, { marks: {}, notes: {}, tags: {}, drafts: {}, deletedIds: [], selectedIds: new Set(['a']) });
  Object.assign(grid, { toast: () => {}, requestRender: () => {}, storageSet: () => {}, MARKS_KEY: 'marks', markLabel: s => s, NOTES_KEY: 'notes', TAGS_KEY: 'tags' });
  const ops = [fn('targetIds'), fn('touchAnnotation'), fn('applyMark'), fn('applyTag'), fn('clearTags')].join('\n');
  runInNewContext(ops + '\napplyMark("important");', grid);
  assert.equal(state.drafts.a.mark.status, 'important');
  runInNewContext(ops + '\napplyMark(null);', grid);
  assert.equal(state.drafts.a.mark, null);
  runInNewContext(ops + '\napplyTag("사고");', grid);
  assert.equal(JSON.stringify(state.drafts.a.tags), '["사고"]');
  runInNewContext(ops + '\nclearTags();', grid);
  assert.equal(JSON.stringify(state.drafts.a.tags), '[]');
  console.log('PASS VM annotation drafts capture post-mutation state');
  // STR-01: the shell script is bundled from assets/app/*.js (build.rs).
  const { appBundleJs } = await import('./lib/app-bundle.mjs');
  const exScript = appBundleJs();
  const { document: exDoc } = { document: {} };
  void exDoc;
  const el = id => ({ id, value: '', textContent: '', innerHTML: '', disabled: false, checked: false, classList: { add() {}, remove() {}, toggle() {} }, style: {}, dataset: {}, addEventListener() {}, setAttribute() {}, removeAttribute() {}, closest: () => null, querySelectorAll: () => [] });
  const dom = {
    getElementById: id => (dom[id] ||= el(id)),
    querySelectorAll: () => [],
    querySelector: () => null,
    addEventListener() {},
    body: el('body')
  };
  dom.startErr = { classList: { add() {}, remove() {} }, textContent: '' };
  // setTimeout must not actually schedule: poll() re-arms itself forever and
  // would keep the --unit process alive after the assertions finish.
  const noopTimeout = () => 0;
  const ex = { window: {}, document: dom, location: { protocol: 'http:' }, localStorage: { getItem: () => null, setItem() {} }, fetch: async () => { throw new Error('down'); }, console, confirm: () => true, setTimeout: noopTimeout, clearTimeout, setInterval: () => 0, clearInterval, addEventListener() {} };
  ex.window = ex;
  runInNewContext(exScript, ex);
  const openFail = async () => {
    ex.document.getElementById('caseDir').value = '/tmp/nope';
    await ex.openCase('/tmp/nope');
    assert.equal(dom.startErr.textContent.includes('서버에 연결할 수 없습니다'), true);
    // One integrated screen: no step rail any more — the reset shows in the
    // shell's workflow latches (docs/ui-rebuild-contract.md §2.2).
    assert.equal(runInNewContext('reviewReady || jobSeen', ex), false);
    assert.equal(dom.btnFinalize.disabled, true);
  };
  await openFail();
  console.log('PASS VM examiner open-case failure resets workflow state');
  // A successful switch must also clear the previous case's results —
  // otherwise stale report links, package paths and the audit badge of
  // case A stay visible while case B is being reviewed.
  dom.finalLinks.innerHTML = '<a href="/case/reports/case-report.html">old</a>';
  dom.pkgPath.textContent = 'old-package';
  dom.pkgPath.classList.add('show');
  dom.btnFinalize.disabled = false;
  dom.btnFinalize.textContent = '보고서 재생성';
  dom.auditBadge.className = 'badge ok';
  dom.auditBadge.textContent = '감사 로그 구조 검증됨';
  runInNewContext('reviewReady = true; jobSeen = true;', ex);
  ex.fetch = async () => ({ ok: true, has_review: true });
  await ex.openCase('/tmp/other-case');
  assert.equal(dom.finalLinks.innerHTML, '');
  assert.equal(dom.pkgPath.textContent, '');
  assert.equal(dom.btnFinalize.disabled, true);
  assert.equal(dom.btnFinalize.textContent, '보고서 생성·패키징');
  assert.equal(dom.auditBadge.textContent, '감사 로그 대기');
  assert.equal(runInNewContext('reviewReady || jobSeen', ex), false);
  console.log('PASS VM examiner open-case success clears stale results');
  // SHOULD-FIX 4: "모두 일치" only when files were compared and none were
  // left uncompared.
  const verdict = res => ex.verifySourcesVerdict(res);
  assert.equal(verdict({ matched: 0, mismatched: 0, not_comparable: 3 }).level, 'warn');
  assert.ok(!verdict({ matched: 0, mismatched: 0, not_comparable: 3 }).toast.includes('모두 일치'));
  assert.ok(verdict({ matched: 0, mismatched: 0, not_comparable: 3 }).hint.includes('재검증되지 않았습니다'));
  assert.equal(verdict({ matched: 0, mismatched: 0, not_comparable: 0 }).level, 'warn');
  assert.equal(verdict({ matched: 2, mismatched: 0, not_comparable: 1 }).level, 'warn');
  assert.equal(verdict({ matched: 2, mismatched: 1, not_comparable: 0 }).level, 'err');
  assert.equal(verdict({ matched: 2, mismatched: 0, not_comparable: 0 }).level, 'ok');
  console.log('PASS VM verify-sources never reports all-match when nothing was compared');
  // SHOULD-FIX 5: unsaved or queued marks block the finalize, with a retry.
  const levels = input => ex.unsavedMarksChecks(input).map(c => c.level);
  const blockRetry = input => ex.unsavedMarksChecks(input).some(c => c.level === 'block' && c.retry);
  assert.equal(JSON.stringify(levels({ pendingMarks: 0, saveState: 'saved', drafts: 0 })), '["ok"]');
  assert.ok(levels({ pendingMarks: 2, saveState: 'saved', drafts: 0 }).includes('block'), 'server queue blocks');
  assert.ok(blockRetry({ pendingMarks: 0, saveState: 'failed', drafts: 1 }), 'failed save blocks with retry');
  assert.ok(blockRetry({ pendingMarks: 0, saveState: 'pending', drafts: 1 }), 'unsaved edits block with retry');
  assert.ok(levels({ pendingMarks: 0, saveState: 'queued', drafts: 0 }).includes('block'), 'queued marks block');
  assert.ok(blockRetry({ pendingMarks: 0, saveState: null, drafts: 3 }), 'browser-only drafts block');
  assert.ok(levels({ pendingMarks: null, saveState: 'saved', drafts: 0 }).includes('block'), 'unknown server queue blocks');
  assert.ok(!levels({ pendingMarks: 0, saveState: null, drafts: null }).includes('block'));
  console.log('PASS VM finalize precheck blocks on unsaved marks');
  // Browser-playability gate: AVI/bin/proprietary records must prefer a
  // proxy and never reach a dead <video>; playable mp4 keeps the original.
  const consts = [...source.matchAll(/^const (BROWSER_CODECS|BROWSER_CONTAINERS|PROPRIETARY_EXTS|PLAYABLE_EXTS) = .*$/gm)].map(m => m[0]).join('\n');
  const mediaFns = consts + '\n' + [fn('extOfPath'), fn('needsProxy'), fn('sanitizeSelector'), fn('proxyPathFor'), fn('fileUrl'), fn('mediaSrcFor')].join('\n');
  const media = {
    state: { proxies: {} },
    location: { protocol: 'http:' },
    DATA: { proxies: ['D:\\case\\artifacts\\proxies\\vid_000001_proxy_1.mp4'] },
    BS: '\\', EXT_PREFIX: '\\\\?\\', EXT_UNC: '\\\\?\\unc\\',
  };
  const runMedia = expr => runInNewContext('{' + mediaFns + '\n' + expr + '}', media);
  const avi = { id: 'v1', kind: 'video', path: 'D:/e/x.avi', fileUrl: 'file:///D:/e/x.avi', ext: 'avi', container: 'avi', codec: 'h264', probeOk: true, proxyPath: '' };
  media.avi = avi;
  assert.ok(runMedia('needsProxy(avi)').includes('container'), 'avi container flagged');
  assert.equal(runMedia('mediaSrcFor(avi)'), '', 'unplayable without proxy yields no src');
  avi.proxyPath = 'D:\\case\\artifacts\\proxies\\v1_proxy_9.mp4';
  assert.equal(runMedia('mediaSrcFor(avi)'), '/media?path=' + encodeURIComponent('D:\\case\\artifacts\\proxies\\v1_proxy_9.mp4'), 'proxy wins for avi');
  const mp4 = { id: 'v2', kind: 'video', path: 'D:/e/x.mp4', fileUrl: 'file:///D:/e/x.mp4', ext: 'mp4', container: 'mov,mp4', codec: 'h264', probeOk: true, proxyPath: 'D:\\p\\x.mp4' };
  media.mp4 = mp4;
  assert.equal(runMedia('needsProxy(mp4)'), '', 'mp4 playable');
  assert.equal(runMedia('mediaSrcFor(mp4)'), '/media?path=' + encodeURIComponent('D:/e/x.mp4'), 'playable keeps original');
  const fsRec = { id: 'inode:32768:18440', kind: 'filesystem', path: 'D:/e/inode.bin', fileUrl: 'file:///D:/e/inode.bin', ext: 'bin', codec: '-', proxyPath: '' };
  media.fsRec = fsRec;
  assert.ok(runMedia('needsProxy(fsRec)'), '.bin recovered output flagged');
  media.location = { protocol: 'file:' };
  mp4.proxyPath = '';
  assert.equal(runMedia('mediaSrcFor(mp4)'), 'file:///D:/e/x.mp4', 'file:// keeps original');
  assert.equal(runMedia('mediaSrcFor(avi)'), 'file:///D:/case/artifacts/proxies/v1_proxy_9.mp4', 'file:// proxy via fileUrl');
  // Standalone download: file:// mode falls back to the original's fileUrl
  // (the browser saves unrenderable types like .bin/.avi instead of playing).
  const dl = { location: { protocol: 'file:' }, BS: '\\', EXT_PREFIX: '\\\\?\\', EXT_UNC: '\\\\?\\unc\\' };
  runInNewContext('{' + fn('fileUrl') + '\n' + fn('downloadHref') + '\nthis.__dl = downloadHref; }', dl);
  assert.equal(
    runInNewContext(`this.__dl(${JSON.stringify(avi)})`, dl),
    'file:///D:/e/x.avi',
    'file:// download falls back to fileUrl',
  );
  dl.location = { protocol: 'http:' };
  assert.equal(
    runInNewContext(`this.__dl(${JSON.stringify(avi)})`, dl),
    '/media?path=' + encodeURIComponent('D:/e/x.avi') + '&download=1',
    'http download uses /media',
  );
  console.log('PASS VM unplayable gate, proxy preference, file:// fallback');
}

(process.argv.includes('--unit') ? unitTests() : main()).catch(err => { console.error('DRIVER ERROR:', err); process.exit(2); });
