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
// F7: the live mode drives a REAL workstation; lib/real-server-guard.mjs
// (track F) refuses the user's own 8477 unless --i-know. Imported lazily in
// main() so the --unit mode (CI) never depends on it.
//   const BASE = guardBase('verify-review-batch', process.env.FT_BASE || 'http://127.0.0.1:8477')
let BASE = '';
let VIEWER = '';

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
  const { guardBase } = await import('./lib/real-server-guard.mjs');
  BASE = guardBase('verify-review-batch', process.env.FT_BASE || 'http://127.0.0.1:8477');
  VIEWER = BASE + '/review/evidence-viewer.html';
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
    `(() => { const chip = [...document.querySelectorAll('[data-chip]')].find(c => c.dataset.chip === 'warning');
        if (!chip) return { exists: false };
        chip.click();
        const n = filteredRecords().length;
        const allHave = filteredRecords().every(r => (r.warnings || []).length > 0);
        return { exists: true, n, allHave }; })()`);
  check('warning chip filters warning-bearing records', warn.exists && warn.allHave, JSON.stringify(warn));

  // reset chip to all (data-chip="")
  await evalIn(browserWs, main.sessionId,
    `[...document.querySelectorAll('[data-chip]')].find(c => c.dataset.chip === '')?.click()`);

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
  const fn = name => source.match(new RegExp(`^(?:async )?function ${name}\\([^]*?^}`, 'm'))?.[0];
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
  // renderGrid sets / clears the grid's ARIA (U14: role, aria-rowcount).
  els.recordGrid.attrs = {};
  els.recordGrid.setAttribute = (k, v) => { els.recordGrid.attrs[k] = String(v); };
  els.recordGrid.removeAttribute = (k) => { delete els.recordGrid.attrs[k]; };
  els.recordGrid.getAttribute = (k) => (k in els.recordGrid.attrs ? els.recordGrid.attrs[k] : null);
  // State classes (vgrid, theater, ...) live on the fragment root #ftViewer.
  els.root = { classList: { add: () => {}, remove: () => {}, toggle: () => {}, contains: () => false } };
  Object.assign(state, { marks: {}, selectedIds: new Set(), pageSize: 100, currentPage: 1, groupBy: 'none', collapsedGroups: new Set() });
  // Exercises the paged renderer; the virtual grid has its own Playwright check.
  const grid = { state, records, recordById: new Map(records.map(r => [r.id, r])), els, t: x => x, tf, escapeHtml: x => x, groupKeyFor: () => 'group', renderCard: r => `<card>${r.id}</card>`, groupHeaderHtml: key => `<hdr>${key}</hdr>`, announceResultCount: () => {}, virtualGridEnabled: () => false, document: { body: { classList: { toggle: () => {} } } } };
  runInNewContext(fn('indexInList'), grid); // R6 U8: renderGrid looks the active id up by its index map
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
  Object.assign(state, { drafts: {}, dirtyIds: new Set(), revisions: {}, examiners: {}, notes: {}, tags: {}, ranges: {} });
  // Save protocol v2 (docs/FIX_PLAN_R1_2026-10-13.md §1.6): touchAnnotation
  // only records the draft in memory + dirtyIds; storage is a debounced flush.
  const draftsSrc = [fn('annotationsLocked'), fn('refuseLockedEdit'), fn('writerId'), fn('nextDraftSeq'), fn('draftNormRange'), fn('draftRangeOf'), fn('draftValuesOf'), fn('touchAnnotation'), fn('dropDraft')].join('\n');
  const touch = 'draftSeq=0;draftFlushKeys=new Set();draftRangesDirty=false;\n' + draftsSrc;
  let flushes = 0;
  Object.assign(grid, { FTViewer: { writerToken: 'w1' }, scheduleDraftFlush: () => { flushes += 1; }, markLabel: s => s, document: { getElementById: () => null, body: { classList: { toggle: () => {} } } }, toast: () => {}, RANGES_KEY: 'ranges', NOTES_KEY: 'notes', TAGS_KEY: 'tags' });
  const journalSrc = [fn('snapshotIds'), fn('journalDiff'), fn('journalPack'), fn('journalEntry'), fn('journalIdCount'), fn('trimJournal'), fn('toastUndo'), fn('journaled'), fn('markStatusLabel')].join('\n');
  runInNewContext('undoStack=[];redoStack=[];JOURNAL_LIMIT=200;JOURNAL_ID_CAP=200000;JOURNAL_FIELDS=["mark","note","tags","range"];\n' + journalSrc + '\n' + touch + '\n' + fn('indexInList') + '\n' + fn('markActive') + '\nmarkActive("reviewed");', grid);
  assert.equal(state.activeId, 'b');
  assert.ok(state.dirtyIds.has('a') && state.drafts.a.seq === 1 && state.drafts.a.base === 0 && !('writer' in state.drafts.a), 'draft carries seq + base (the writer is in its storage key)');
  assert.equal(flushes, 1, 'one debounced flush scheduled, no synchronous storage write');
  console.log('PASS VM empty filter, collapsed group, sequential triage');
  Object.assign(state, { marks: {}, notes: {}, tags: {}, drafts: {}, examiner: 'New', dirtyIds: new Set(['a']), revisions: { a: 4 }, examiners: { b: 'Bob' } });
  Object.assign(grid, { manifest: { case_id: 'test' } });
  state.ranges = {}; // range-only records are exported too (FOR-01); this block tests marks/notes
  const payloadSrc = [fn('marksPayload'), fn('payloadIds'), fn('payloadEmpty')].join('\n');
  let payload = runInNewContext(payloadSrc + '\nmarksPayload();', grid);
  assert.equal(payload.protocol, 'patch-v2');
  assert.equal(payload.client_id, 'w1');
  assert.equal(JSON.stringify(payload.deleted_ids), '["a"]');
  assert.equal(JSON.stringify(payload.tags), '[{"id":"a","tags":[]}]', 'a deleted id still sends its (empty) tags explicitly');
  assert.equal(JSON.stringify(payload.base_revisions), '{"a":4}');
  assert.equal(payload.marks.length, 0, 'only dirty ids travel — nothing else');
  state.marks.b = { status: 'important', marked_unix: 1 };
  payload = runInNewContext(payloadSrc + '\nmarksPayload();', grid);
  assert.equal(payload.marks.length, 0, 'a clean (not dirty) mark is not re-sent');
  state.dirtyIds.add('b');
  state.tags.b = ['사고'];
  payload = runInNewContext(payloadSrc + '\nmarksPayload();', grid);
  assert.equal(payload.marks[0].note, '');
  assert.equal(payload.marks[0].examiner, 'Bob');
  assert.equal(JSON.stringify(payload.marks[0].tags), '["사고"]', 'tags ride inside the mark entry');
  assert.equal(payload.tags.length, 1, 'no second tags entry for a marked id (server rejects duplicate ids)');
  assert.equal(payload.base_revisions.b, 0, 'unknown revision is sent as 0');
  // §10.3: a draft's stored base wins over the last revision seen — an
  // adopted (dead-window) draft goes out on the revision it was made on.
  state.drafts.b = { mark: null, note: '', tags: ['사고'], examiner: 'Bob', range: null, seq: 9, base: 2 };
  state.revisions.b = 5;
  payload = runInNewContext(payloadSrc + '\nmarksPayload();', grid);
  assert.equal(payload.base_revisions.b, 2, 'draft base sent, not the newer known revision');
  delete state.drafts.b; delete state.revisions.b;
  const everything = runInNewContext(payloadSrc + '\nmarksPayload({ all: true });', grid);
  state.dirtyIds.delete('b');
  const allButDirty = runInNewContext(payloadSrc + '\nmarksPayload({ all: true });', grid);
  assert.ok(everything.marks.some(m => m.id === 'b') && allButDirty.marks.some(m => m.id === 'b'), 'the marks download ({all}) includes clean marks');
  // "[서버 값 사용]" on a row with no examiner: that mark must not pick up
  // the body's top-level examiner, so it travels in its own body.
  state.dirtyIds = new Set(['a', 'b', 'c']);
  state.marks.c = { status: 'reviewed', marked_unix: 2 };
  state.examiners.c = null;
  state.revisions.c = 7;
  payload = runInNewContext(payloadSrc + '\nmarksPayload();', grid);
  assert.equal(payload.marks.find(m => m.id === 'c').examiner, null, 'explicit null examiner is kept, not the current name');
  const splitSrc = payloadSrc + '\n' + fn('splitByExaminer') + '\n';
  const parts = JSON.parse(runInNewContext(splitSrc + 'JSON.stringify(splitByExaminer(marksPayload()));', grid));
  assert.equal(parts.length, 2);
  assert.equal(JSON.stringify(parts.map(p => [p.examiner, p.marks.map(m => m.id), p.deleted_ids, Object.keys(p.base_revisions)])), '[["New",["b"],["a"],["b","a"]],[null,["c"],[],["c"]]]');
  delete state.marks.c; delete state.examiners.c; delete state.revisions.c;
  state.dirtyIds = new Set(['a']);
  assert.equal(runInNewContext(splitSrc + 'splitByExaminer(marksPayload()).length;', grid), 1, 'no split when every mark is named');
  console.log('PASS VM explicit annotation clears, dirty-only payload, examiner preservation');
  // Chunking: 2000 ids / 1 MiB per POST; every chunk is a complete body
  // with batch {id, index, total}; ids never repeat across chunks.
  const chunkSrc = payloadSrc + '\n' + [fn('splitPayloadChunks'), fn('legacyPayloadBody'), fn('sendPayloadChunks')].join('\n') + '\nPAYLOAD_CHUNK_IDS=2000;PAYLOAD_CHUNK_BYTES=1048576;PAYLOAD_CHUNK_MIN_IDS=50;';
  const bigIds = Array.from({ length: 4500 }, (_, i) => `vid_${String(i).padStart(6, '0')}`);
  const big = { schema_version: 2, protocol: 'patch-v2', case_id: 'c', examiner: 'x', client_id: 'w1', exported_unix: 1, base_revisions: Object.fromEntries(bigIds.map(id => [id, 1])), marks: bigIds.slice(0, 4000).map(id => ({ id, status: 'reviewed', marked_unix: 1, note: '', examiner: 'x', tags: [], range: { in: null, out: null } })), tags: bigIds.slice(4000).map(id => ({ id, tags: [] })), deleted_ids: bigIds.slice(4000) };
  grid.big = big;
  const chunks = runInNewContext(chunkSrc + '\nsplitPayloadChunks(big);', grid);
  assert.equal(chunks.length, 3, `ceil(4500/2000) chunks, got ${chunks.length}`);
  assert.equal(JSON.stringify(chunks.map(c => c.batch.index)), '[0,1,2]');
  assert.ok(chunks.every(c => c.batch.total === 3 && c.batch.id === chunks[0].batch.id && c.protocol === 'patch-v2'));
  const seen = chunks.flatMap(c => [...c.marks.map(m => m.id), ...c.deleted_ids]);
  assert.equal(new Set(seen).size, 4500);
  assert.equal(seen.length, 4500, 'no id in two chunks');
  assert.ok(chunks.every(c => Object.keys(c.base_revisions).length === c.marks.length + c.deleted_ids.length), 'base_revisions only for the chunk ids');
  assert.ok(chunks[2].deleted_ids.length === 500 && chunks[2].tags.length === 500, 'deleted ids keep their explicit tags entry');
  grid.fat = { ...big, deleted_ids: [], tags: [], marks: bigIds.slice(0, 3000).map(id => ({ id, status: 'reviewed', marked_unix: 1, note: 'x'.repeat(1024), examiner: 'x', tags: [] })) };
  const fatChunks = runInNewContext(chunkSrc + '\nsplitPayloadChunks(fat);', grid);
  assert.ok(fatChunks.length > 3 && fatChunks.every(c => JSON.stringify(c).length <= 1048576), `1 MiB cap splits further (${fatChunks.length} chunks)`);
  // 413 halving: 2000 → 1000 → 500 ids, a fresh batch from the failed chunk on.
  const posted = [];
  grid.post413 = async body => { posted.push(body); if (body.marks.length + body.deleted_ids.length > 600) return { ok: false, status: 413, error: 'HTTP 413' }; return { ok: true, protocol: 'patch-v2', applied: Object.fromEntries([...body.marks.map(m => m.id), ...body.deleted_ids].map(id => [id, 2])), conflicts: [] }; };
  const halved = await runInNewContext(chunkSrc + '\nsendPayloadChunks(big, post413);', grid);
  assert.ok(halved.ok && halved.maxIds === 500, `413 halves the chunk size to 500 (got ${halved.maxIds})`);
  assert.equal(Object.keys(halved.applied).length, 4500, 'every id applied after halving');
  assert.ok(posted.filter(b => b.marks.length + b.deleted_ids.length > 600).length === 2, 'two 413s (2000, 1000) before 500 fits');
  const lastBatch = posted.at(-1).batch;
  assert.ok(lastBatch.index === lastBatch.total - 1 && posted.filter(b => b.batch.id === lastBatch.id).length === lastBatch.total, 'the final batch is complete (post-save work runs once on its last chunk)');
  // Below the 50-id floor a 413 is a failure (no endless halving).
  grid.postAlways413 = async () => ({ ok: false, status: 413, error: 'HTTP 413' });
  const floor = await runInNewContext(chunkSrc + '\nsendPayloadChunks(big, postAlways413, { maxIds: 50 });', grid);
  assert.ok(!floor.ok && floor.status === 413, 'at the floor the 413 surfaces as a failure');
  // Per-id outcome merge: applied map + conflicts, partial progress on a later failure.
  grid.postMixed = async body => body.batch.index === 0
    ? { ok: true, protocol: 'patch-v2', applied: Object.fromEntries(body.marks.slice(1).map(m => [m.id, 7])), conflicts: [{ id: body.marks[0].id, server_revision: 9, server: null }] }
    : { ok: false, status: 503, error: 'busy' };
  const mixed = await runInNewContext(chunkSrc + '\nsendPayloadChunks(big, postMixed);', grid);
  assert.ok(!mixed.ok && mixed.error === 'busy' && Object.keys(mixed.applied).length === 1999 && mixed.conflicts.length === 1 && mixed.conflicts[0].server_revision === 9, 'first chunk outcome kept, second chunk failure reported');
  // Legacy / deferred replies confirm the chunk's ids at their sent revision.
  grid.postLegacy = async () => ({ ok: true });
  const legacy = await runInNewContext(chunkSrc + '\nsendPayloadChunks(big, postLegacy);', grid);
  assert.ok(legacy.ok && Object.keys(legacy.applied).length === 4500 && legacy.applied.vid_000001 === 1 && legacy.posts === 3);
  grid.postDeferred = async () => ({ ok: true, deferred: true, pending: 2 });
  const deferred = await runInNewContext(chunkSrc + '\nsendPayloadChunks(big, postDeferred);', grid);
  assert.ok(deferred.ok && deferred.deferred && deferred.pending === 2 && deferred.queuedIds.length === 4500 && Object.keys(deferred.applied).length === 0,
    'a deferred reply queues the ids; it confirms no revision (the drain decides)');
  // §10.3: only the exact pre-v2 refusal downgrades to patch-v1.
  grid.postProtoErr = async () => ({ ok: false, error: 'protocol error: bad batch' });
  const protoErr = await runInNewContext(chunkSrc + '\nsendPayloadChunks(big, postProtoErr);', grid);
  assert.ok(!protoErr.ok && !protoErr.legacy && protoErr.posts === 1, 'an error merely mentioning "protocol" is a failure, not a legacy switch');
  // A pre-v2 workstation refuses the protocol: the same chunk is resent as
  // patch-v1 (tags in the v1 array, no revisions) and the run continues.
  const v1Bodies = [];
  grid.postOld = async body => { if (body.protocol === 'patch-v2') return { ok: false, error: 'unsupported marks protocol' }; v1Bodies.push(body); return { ok: true, applied: true, stored: 1 }; };
  const old = await runInNewContext(chunkSrc + '\nsendPayloadChunks(big, postOld);', grid);
  assert.ok(old.ok && old.legacy && old.posts === 4 && Object.keys(old.applied).length === 4500, `legacy fallback (${old.posts} posts)`);
  assert.ok(v1Bodies.length === 3 && v1Bodies.every(b => b.protocol === 'patch-v1' && !('base_revisions' in b) && !('batch' in b) && b.marks.every(m => !('tags' in m))), 'v1 bodies carry no v2 fields');
  assert.ok(v1Bodies[2].tags.length === 500 && v1Bodies[0].tags.length === 2000, 'v1 bodies list every id\'s tags in the tags array');
  // R4 B4/B7 (server as implemented): "briefly busy, retry" is transient
  // (busy + retryAfterMs, nothing confirmed); report_error rides beside ok.
  grid.postBusy = async () => ({ ok: false, busy: true, retry: true, retry_after_ms: 1500, error: 'busy' });
  const busyOut = await runInNewContext(chunkSrc + '\nsendPayloadChunks(big, postBusy);', grid);
  assert.ok(!busyOut.ok && busyOut.busy === true && busyOut.retryAfterMs === 1500 && Object.keys(busyOut.applied).length === 0 && busyOut.posts === 1, 'B4: busy/retry reply is transient, nothing confirmed');
  grid.postReportErr = async (body) => ({ ok: true, protocol: 'patch-v2', applied: Object.fromEntries(body.marks.map(m => [m.id, 2])), conflicts: [], report_error: 'make-report failed' });
  const repOut = await runInNewContext(chunkSrc + '\nsendPayloadChunks(big, postReportErr);', grid);
  assert.ok(repOut.ok && repOut.reportError === 'make-report failed', 'B7: report_error is surfaced beside a successful save');
  // B3/B6: paging follows next_offset until null.
  const pages = [];
  grid.getPaged = async (url) => { const o = Number(new URL(url, 'http://x').searchParams.get('offset')); pages.push(o); return { ok: true, count: 5, next_offset: o + 2 < 5 ? o + 2 : null, conflicts: [o, o + 1].filter(n => n < 5).map(n => ({ id: 'c' + n })) }; };
  const paged = await runInNewContext(fn('readPagedList') + '\nCONFLICT_PAGE=500;CONFLICT_PAGES_MAX=2000;\nreadPagedList(getPaged, "/api/mark-conflicts", d => d.conflicts);', grid);
  assert.equal(JSON.stringify([paged.map(c => c.id), pages]), '[["c0","c1","c2","c3","c4"],[0,2,4]]', 'B3: every page read via next_offset');
  console.log('PASS VM chunking (2000 ids / 1 MiB), 413 halving, per-id outcome merge, busy/retry, report_error, next_offset paging');
  // Drafts (§10.3): one key per id, ft.draft.<case>.<writer>.<id>. Hydration
  // replays this window's drafts and those of a CLOSED writer — never a live
  // one, and a merely stale one only after an unanswered ping
  // (adoptOrphanDrafts). Adopted drafts keep their base and are re-keyed;
  // the pre-§10.3 single key is migrated once (base 0); a stored conflict
  // is restored as a conflict, not as a dirty edit.
  const store = new Map();
  let storeFull = false;
  const quota = () => Object.assign(new Error('quota'), { name: 'QuotaExceededError' });
  const ls = {
    get length() { return store.size; },
    key: (i) => [...store.keys()][i] ?? null,
    getItem: (k) => (store.has(k) ? store.get(k) : null),
    setItem: (k, v) => { if (storeFull) throw quota(); store.set(k, String(v)); },
    removeItem: (k) => { store.delete(k); },
  };
  const P = 'ft.draft.test.';
  const put = (k, v) => store.set(k, JSON.stringify(v));
  put(P + 'w1.a', { mark: null, note: '', tags: [], examiner: null, range: null, seq: 3, base: 3 });
  put(P + 'dead.c', { mark: { status: 'reviewed', marked_unix: 5 }, note: 'orphan', tags: ['x'], examiner: 'Dead', range: { in: 1, out: 2 }, seq: 40, base: 1 });
  put(P + 'live.d', { mark: { status: 'important', marked_unix: 6 }, note: 'theirs', tags: [], examiner: 'Live', seq: 3, base: 0 });
  put(P + 'stale.f', { mark: { status: 'important', marked_unix: 6 }, note: 'stale', tags: [], examiner: null, seq: 2, base: 1 });
  put(P + 'w1.g', { mark: { status: 'reviewed', marked_unix: 7 }, note: 'mine, in conflict', tags: [], examiner: null, range: null, seq: 5, base: 1, conflict: { server: { status: 'important', note: 'srv', tags: [] }, server_revision: 2, queued: false } });
  store.set('ft.alive:test:live', String(Date.now() - 60000));       // throttled, < 5 min: alive
  store.set('ft.alive:test:stale', String(Date.now() - 6 * 60000));  // >= 5 min: ping first
  store.set('ft.alive:test:dead', 'closed');
  store.set('ft.alive:test:gone1', 'closed');
  put('annotations', { e: { mark: null, note: 'pre-10.3 draft', tags: [], writer: 'gone1', seq: 4 }, h: { mark: { status: 'reviewed', marked_unix: 1 }, note: '', tags: [] } });
  Object.assign(state, { marks: {}, notes: {}, tags: {}, examiners: {}, ranges: {}, dirtyIds: new Set(), drafts: {} });
  let storageReports = 0;
  Object.assign(grid, {
    localStorage: ls, ANNOTATIONS_KEY: 'annotations', DRAFT_PREFIX: P, ALIVE_PREFIX: 'ft.alive:test:', ALIVE_CLOSED: 'closed',
    ALIVE_STALE_MS: 300000, ALIVE_RECENT_MS: 30000, Date, JSON, clearTimeout, setTimeout,
    reportStorageError: () => { storageReports += 1; }, ftSignal: { aborted: false },
  });
  grid.annotations = { marks: [{ id: 'a', status: 'important', note: 'old' }, { id: 'b', status: 'reviewed', note: 'DB memo', examiner: 'Bob' }], tags: [{ id: 'b', tags: ['DB tag'] }], revisions: { a: 3, b: 1, c: 2 } };
  const hydrateSrc = [fn('aliveKey'), fn('draftKey'), fn('parseDraftKey'), fn('writerVerdict'), fn('storedDraftEntries'), fn('migrateLegacyDrafts'), fn('applyDraftValues'), fn('adoptDraft'), fn('annotationMarkId'), fn('hydrateAnnotations'), fn('adoptOrphanDrafts'), fn('adoptDeadWriters'), fn('flushDraftsNow'), fn('applyAppliedIds'), fn('dropJournalIds'), fn('draftNormRange'), fn('draftSameValues'), fn('serverDraftValues'), fn('noteOrphanDraft'), fn('migrateLegacyRanges')].join('\n');
  runInNewContext('orphanDrafts=new Map();legacyRangesLeft=null;annotationsError="";markRowMaybe=new Set();skippedDraftWriters=new Set();restoredConflictIds=[];draftAdoptedFrom=new Map();orphanCheck=null;draftsUnpersisted=false;draftFlushTimer=null;\n' + touch + '\n' + hydrateSrc + '\nhydrateAnnotations(annotations);', grid);
  assert.equal(state.marks.a, undefined, 'own draft (cleared mark) replays over the DB row');
  assert.equal(state.notes.b, 'DB memo');
  assert.equal(state.examiners.b, 'Bob');
  assert.equal(JSON.stringify(state.tags.b), '["DB tag"]');
  assert.equal(JSON.stringify(state.revisions), '{"a":3,"b":1,"c":2}', 'revisions seeded from annotations.revisions');
  assert.ok(state.marks.c?.status === 'reviewed' && state.ranges.c?.out === 2 && state.dirtyIds.has('c'), 'closed writer draft adopted');
  assert.ok(state.drafts.c.base === 1, 'adopted draft keeps ITS base (1), not the newer known revision (2) — the server answers with a conflict');
  assert.ok(store.has(P + 'w1.c') && !store.has(P + 'dead.c'), 'adopted draft re-keyed under this writer, the dead key removed after');
  assert.ok(state.drafts.c.seq > 40 && runInNewContext('draftSeq', grid) >= 41, 'seq counter moves past adopted seqs');
  assert.ok(!state.marks.d && !state.dirtyIds.has('d') && !state.drafts.d && store.has(P + 'live.d'), 'live other window draft left alone');
  assert.ok(!state.drafts.f && store.has(P + 'stale.f'), 'stale-heartbeat draft not adopted at boot (ping first)');
  assert.ok(runInNewContext('skippedDraftWriters.has("live") && skippedDraftWriters.has("stale")', grid), 'skipped writers remembered for the heartbeat tick');
  assert.ok(!store.has('annotations') && state.dirtyIds.has('e') && state.drafts.e.base === 0 && state.notes.e === 'pre-10.3 draft', 'pre-§10.3 single-key draft migrated (base 0) and adopted');
  assert.ok(state.marks.h?.status === 'reviewed' && state.drafts.h.base === 0, 'writer-less legacy draft migrated and adopted');
  assert.ok(!state.dirtyIds.has('g') && runInNewContext('restoredConflictIds.includes("g")', grid) && state.drafts.g.conflict.server_revision === 2, 'stored conflict restored as a conflict, not a dirty edit');
  // Heartbeat tick: a stale writer that answers the ping keeps its drafts;
  // one that does not is adopted (with its base).
  grid.pingWriters = async () => new Set(['stale']);
  assert.equal(await runInNewContext('adoptOrphanDrafts()', grid), 0, 'stale but answering the ping: not adopted');
  assert.ok(!state.drafts.f && store.has(P + 'stale.f'));
  grid.pingWriters = async () => new Set();
  assert.equal(await runInNewContext('adoptOrphanDrafts()', grid), 1, 'stale and silent: adopted');
  assert.ok(state.drafts.f && state.drafts.f.base === 1 && state.dirtyIds.has('f') && !store.has(P + 'stale.f'));
  assert.ok(!state.drafts.d, 'the live writer is never pinged into adoption');
  console.log('PASS VM per-id drafts: migration, closed/stale/live writers, ping-gated adoption, stored base + conflict');
  // Flush: removals first; the first quota failure stops the pass (ONE
  // report, not one per key) and keeps the rest for the next flush.
  state.drafts = { x1: { seq: 1, base: 0 }, x2: { seq: 2, base: 0 } };
  store.set(P + 'w1.gone', '{}');
  storeFull = true;
  storageReports = 0;
  runInNewContext('draftFlushKeys=new Set(["x1","x2","gone"]);\nflushDraftsNow(true);', grid);
  assert.ok(!store.has(P + 'w1.gone'), 'removal done even with storage full');
  assert.ok(storageReports === 1 && runInNewContext('draftsUnpersisted && draftFlushKeys.size === 2', grid), `one quota report per flush, rest kept (${storageReports})`);
  storeFull = false;
  runInNewContext('flushDraftsNow(true);', grid);
  assert.ok(store.has(P + 'w1.x1') && store.has(P + 'w1.x2') && runInNewContext('!draftsUnpersisted && draftFlushKeys.size === 0', grid), 'next flush stores the rest');
  // A confirmed id drops its draft only when the sent seq is current; an
  // edit made mid-flight stays dirty, now based on the confirmed revision.
  state.drafts = { p: { seq: 7, base: 1 }, q: { seq: 3, base: 1 } };
  state.dirtyIds = new Set(['p', 'q']);
  runInNewContext('applyAppliedIds({ p: 2, q: 2 }, new Map([["p", 7], ["q", 2]]));', grid);
  assert.ok(!state.drafts.p && !state.dirtyIds.has('p') && state.drafts.q.base === 2 && state.dirtyIds.has('q') && state.revisions.q === 2, 'mid-flight edit rebased on the confirmed revision');
  // Resolving a conflict drops that id's undo/redo changes (entries left empty go).
  runInNewContext('undoStack=[{n:2,groups:[{ids:["a","b"],fields:["mark"],before:{mark:null},after:{mark:null}}]},{n:1,groups:[{ids:["a"],fields:["mark"],before:{},after:{}}]}];redoStack=[{n:1,groups:[{ids:["a"],fields:["mark"],before:{},after:{}}]}];dropJournalIds(["a"]);', grid);
  assert.equal(runInNewContext('JSON.stringify([undoStack.map(e => e.groups.map(g => g.ids)), undoStack.map(e => e.n), redoStack.length])', grid), '[[[["b"]]],[1],0]', 'resolved ids leave the undo/redo journal');
  console.log('PASS VM quota-bounded flush, seq-guarded confirm with rebase, journal drop on resolve');
  // Drafts must snapshot post-mutation state: a draft recorded before the
  // mark/tag mutation replays the old value on reload and resurrects
  // cleared annotations (or drops new ones) after hydration.
  Object.assign(state, { marks: {}, notes: {}, tags: {}, drafts: {}, dirtyIds: new Set(), selectedIds: new Set(['a']) });
  Object.assign(grid, { toast: () => {}, requestRender: () => {}, storageSet: () => {}, MARKS_KEY: 'marks', markLabel: s => s, NOTES_KEY: 'notes', TAGS_KEY: 'tags' });
  const ops = [fn('annotationsLocked'), fn('refuseLockedEdit'), fn('targetIds'), fn('touchAnnotation'), fn('applyMark'), fn('applyTag'), fn('clearTags')].join('\n');
  runInNewContext(ops + '\napplyMark("important");', grid);
  assert.equal(state.drafts.a.mark.status, 'important');
  runInNewContext(ops + '\napplyMark(null);', grid);
  assert.equal(state.drafts.a.mark, null);
  runInNewContext(ops + '\napplyTag("사고");', grid);
  assert.equal(JSON.stringify(state.drafts.a.tags), '["사고"]');
  runInNewContext(ops + '\nclearTags();', grid);
  assert.equal(JSON.stringify(state.drafts.a.tags), '[]');
  // A draft is dropped only when its seq is the one that was sent.
  const seqSent = state.drafts.a.seq;
  runInNewContext(ops + '\ntouchAnnotation("a");', grid);
  assert.notEqual(state.drafts.a.seq, seqSent, 'a mid-flight edit bumps seq');
  assert.ok(state.drafts.a && state.dirtyIds.has('a'), 'edited-while-saving draft survives the save');
  console.log('PASS VM annotation drafts capture post-mutation state, seq-guarded drop');

  // ---- R4 track A (docs/PLAN_R4_2026-10-14.md) -----------------------------
  // A1: every marks entry carries an explicit range; deleted_ids only for a
  // record with no mark, note AND range — and only when the server may hold
  // its row (a tags-only record sends its tags alone).
  Object.assign(state, { marks: {}, notes: {}, tags: { r: ['x'], q: ['y'], t: ['z'] }, ranges: { r: { in: 1, out: 2 } }, drafts: {}, dirtyIds: new Set(['r', 'q', 't']), revisions: {}, examiners: {} });
  grid.markRowMaybe = new Set(['q']);
  const pr = runInNewContext(payloadSrc + '\nmarksPayload();', grid);
  const rEntry = pr.marks.find(m => m.id === 'r');
  assert.ok(rEntry && rEntry.range.in === 1 && rEntry.range.out === 2 && !pr.deleted_ids.includes('r'), 'A1: a record that keeps its range travels as an entry with that range, never in deleted_ids');
  assert.ok(pr.marks.every(m => m.range && 'in' in m.range && 'out' in m.range), 'A1: every marks entry carries an explicit range');
  assert.ok(pr.deleted_ids.includes('q') && pr.tags.some(e => e.id === 'q' && e.tags[0] === 'y'), 'A1: a cleared mark row is deleted, its remaining tags travel explicitly');
  assert.ok(!pr.deleted_ids.includes('t') && pr.tags.some(e => e.id === 't'), 'A1: a record whose server row never existed sends its tags only');
  assert.equal(runInNewContext(payloadSrc + '\nJSON.stringify(payloadIds(marksPayload()).sort());', grid), '["q","r","t"]', 'a tags-only id is part of the body (chunking keeps it)');
  delete grid.markRowMaybe;
  console.log('PASS VM A1 explicit ranges, deleted_ids only for mark-row deletions');

  // A2: ranges hydrate from the case data; the retired RANGES_KEY is migrated
  // (served: a different value → a draft carrying a conflict; equal → dropped).
  store.clear();
  const RK = 'ft.viewer.test.ranges';
  store.set(RK, JSON.stringify({ a: { in: 3, out: 4 }, b: { in: 7, out: 8 }, zz: { in: 1, out: 2 } }));
  Object.assign(grid, { RANGES_KEY: RK, RANGES_HELD_KEY: RK + '.held', recordById: new Map(['a', 'b', 'c'].map(id => [id, { id }])), location: { protocol: 'http:' } });
  grid.annotations = { marks: [{ record_id: 'a', status: 'important', note: '', range: { in: 1, out: 2 } }, { record_id: 'b', status: 'noted', note: '', range: { in: 7, out: 8 } }], tags: [{ id: 'a', tags: ['현장'] }], revisions: { a: 5, b: 2 } };
  const a2Src = [fn('draftNormRange'), fn('draftSameValues'), fn('serverDraftValues'), fn('noteOrphanDraft'), fn('migrateLegacyRanges')].join('\n');
  runInNewContext('orphanDrafts=new Map();legacyRangesLeft=null;annotationsError="";markRowMaybe=new Set();\n' + a2Src + '\n' + hydrateSrc + '\nhydrateAnnotations(annotations);', grid);
  assert.ok(state.ranges.b && state.ranges.b.in === 7, 'A2: the server range hydrates like a mark');
  assert.ok(state.ranges.a.in === 3 && state.drafts.a.conflict && state.drafts.a.conflict.legacy && state.drafts.a.conflict.server.range.in === 1 && state.drafts.a.base === 5 && runInNewContext('restoredConflictIds.includes("a")', grid), 'A2: a different stored range becomes a conflict (server 1~2 vs mine 3~4), base = the server revision');
  assert.ok(!state.drafts.b && !state.dirtyIds.has('b'), 'A2: an equal stored range is no edit');
  assert.ok(!store.has(RK) && store.has(P + 'w1.a'), 'A2: the retired key is gone once every entry is stored');
  assert.ok(!state.drafts.zz, 'A2: an id this case no longer lists is not invented');
  assert.ok(JSON.parse(store.get(RK + '.held') || '{}').zz?.in === 1, 'R5 U3: ...but its range is held (RANGES_HELD_KEY), not dropped with the retired key');
  // A14: hydrated tag lists are copies — a tag edit never rewrites the case data.
  state.tags.a.push('local');
  assert.deepEqual(grid.annotations.tags[0].tags, ['현장'], 'A14: the server tag list stays the server\'s');
  // file:// (standalone): the bundle wins, leftovers are only reported.
  store.clear();
  store.set(RK, JSON.stringify({ b: { in: 9, out: 10 } }));
  grid.location = { protocol: 'file:' };
  runInNewContext('hydrateAnnotations(annotations);', grid);
  assert.ok(state.ranges.b.in === 7 && !state.drafts.b && runInNewContext('legacyRangesLeft && legacyRangesLeft.count === 1', grid) && store.has(RK), 'A2: file:// keeps the bundle range, reports (and keeps) the leftover');
  grid.location = { protocol: 'http:' };
  console.log('PASS VM A2 range hydration + one-time RANGES_KEY migration, A14 copied tag lists');

  // A7: a closed window's draft for an id this window holds a draft of:
  // identical → removed now; different → held, and once ours is confirmed it
  // becomes a conflict (server = what was saved, mine = the closed window's).
  store.clear();
  store.set('ft.alive:test:gone', 'closed');
  put(P + 'w1.a', { mark: { status: 'reviewed', marked_unix: 1 }, note: '', tags: [], examiner: null, range: null, seq: 2, base: 5 });
  put(P + 'gone.a', { mark: { status: 'important', marked_unix: 1 }, note: 'old', tags: [], examiner: null, range: null, seq: 9, base: 4 });
  put(P + 'w1.b', { mark: { status: 'reviewed', marked_unix: 1 }, note: '', tags: [], examiner: null, range: { in: 7, out: 8 }, seq: 3, base: 2 });
  put(P + 'gone.b', { mark: { status: 'reviewed', marked_unix: 9 }, note: '', tags: [], examiner: null, range: { in: 7, out: 8 }, seq: 1, base: 2 });
  runInNewContext('hydrateAnnotations(annotations);', grid);
  assert.ok(!store.has(P + 'gone.b'), 'A7: an identical closed-window draft is removed at once');
  assert.ok(store.has(P + 'gone.a') && runInNewContext('orphanDrafts.has("a") && !orphanDrafts.has("b")', grid) && state.marks.a.status === 'reviewed', 'A7: a different one is held (ours shown), not dropped');
  const registered = [];
  Object.assign(grid, { registerConflicts: list => { registered.push(...list); return list.length; }, normRange: r => (r && (Number.isFinite(r.in) || Number.isFinite(r.out)) ? { in: r.in ?? null, out: r.out ?? null } : null),
    localAnnotationOf: id => ({ status: state.marks[id] ? state.marks[id].status : null, note: state.notes[id] || '', range: state.ranges[id] || null, tags: [...(state.tags[id] || [])] }) });
  runInNewContext(fn('settleOrphanDrafts') + '\n' + fn('applyAppliedIds') + '\napplyAppliedIds({ a: 6 }, new Map([["a", state.drafts.a.seq]]));', grid);
  assert.ok(registered.length === 1 && registered[0].id === 'a' && registered[0].orphan && registered[0].server.status === 'reviewed' && registered[0].mine.status === 'important' && registered[0].mine.note === 'old' && registered[0].server_revision === 6,
    'A7: after our save the closed window\'s different draft is a conflict at once (never re-adopted later)');
  assert.ok(runInNewContext('draftAdoptedFrom.get("a")', grid) === P + 'gone.a', 'A7: its key goes once ours (with the conflict) is stored');
  console.log('PASS VM A7 closed-window drafts: dropped when identical, conflict after our save otherwise');

  // A8: the journal packs a bulk action into one group and is capped by ids.
  const jstate = { marks: {}, notes: {}, tags: {}, ranges: {}, activeId: null };
  const jctx = { state: jstate, document: { getElementById: () => null }, toastUndo: () => {}, JSON, Set, Map };
  const journalA8 = [fn('snapshotIds'), fn('journalDiff'), fn('journalPack'), fn('journalEntry'), fn('journalUnpack'), fn('journalIdCount'), fn('trimJournal'), fn('journalStats'), fn('journaled')].join('\n');
  runInNewContext('var undoStack=[],redoStack=[];var JOURNAL_LIMIT=200,JOURNAL_ID_CAP=200000;var JOURNAL_FIELDS=["mark","note","tags","range"];\n' + journalA8, jctx);
  const big100k = Array.from({ length: 100000 }, (_, i) => 'v' + i);
  jctx.big100k = big100k;
  runInNewContext('journaled("bulk", big100k, () => { for (const id of big100k) state.marks[id] = { status: "reviewed", marked_unix: 7 }; });', jctx);
  const st1 = runInNewContext('journalStats()', jctx);
  assert.ok(st1.ids === 100000 && runInNewContext('undoStack[0].groups.length', jctx) === 1, `A8: a 100 000-record bulk mark is ONE group (${runInNewContext('undoStack[0].groups.length', jctx)})`);
  assert.ok(st1.bytes < 100000 * 12, `A8: ~ids only, no per-record snapshots (${st1.bytes} bytes)`);
  for (const status of ['important', 'needs_verification']) {
    jctx.status = status;
    runInNewContext('journaled("bulk2", big100k, () => { for (const id of big100k) state.marks[id] = { status, marked_unix: 8 }; });', jctx);
  }
  const st2 = runInNewContext('journalStats()', jctx);
  assert.ok(st2.ids <= 200000 && runInNewContext('undoStack.length', jctx) === 2 && runInNewContext('undoStack[1].label', jctx) === 'bulk2', `A8: past ${200000} ids the oldest entries go first (${st2.ids} ids in ${runInNewContext('undoStack.length', jctx)} entries)`);
  const back = runInNewContext('journalUnpack(undoStack[1])', jctx);
  assert.ok(back.length === 100000 && back[0].fields.join() === 'mark' && back[0].before.mark.status === 'important' && back[0].after.mark.status === 'needs_verification' && !('note' in back[0].before), 'A8: unpacking restores per-record changes of the changed fields only');
  console.log(`PASS VM A8 packed journal (${st1.bytes} B for 100 000 ids) + id cap`);

  // A14: search highlighting matches the raw text, then escapes each piece.
  const hl = runInNewContext([fn('escapeHtml'), fn('highlightEscape')].join('\n') + '\n[highlightEscape("R&D amp <b>", "amp"), highlightEscape("a&b", "&"), highlightEscape("x<y", "<y"), highlightEscape("AmP", "amp")];', {});
  assert.deepEqual(JSON.parse(JSON.stringify(hl)), ['R&amp;D <mark>amp</mark> &lt;b&gt;', 'a<mark>&amp;</mark>b', 'x<mark>&lt;y</mark>', '<mark>AmP</mark>'], 'A14: no entity is split, the match is escaped');
  console.log('PASS VM A14 highlight escapes per piece');

  // R4 D1/D2: "unverified" / "replayed" measurements never read as sound/silent.
  const audioCtx = { audioLevels: { u: { r: 'unverified', max: -3 }, p: { r: 'replayed', max: -80 }, s: { r: 'sound', max: -3 } }, audioLevelsError: '' };
  runInNewContext([fn('audioInfoFor'), fn('audioFacetOf')].join('\n') + '\nthis.info = (r) => { const i = audioInfoFor(r); return [i.state, i.reason, audioFacetOf(i.state)].join("|"); };', audioCtx);
  assert.equal(audioCtx.info({ id: 'u', audioCodec: 'aac', probeOk: true }), 'track|unverified|unknown', 'unverified → 미확인');
  assert.equal(audioCtx.info({ id: 'p', audioCodec: 'aac', probeOk: true }), 'track|replayed|unknown', 'replayed → 미확인');
  assert.equal(audioCtx.info({ id: 'p', audioCodec: '', probeOk: true }), 'unknown|replayed|unknown', 'replayed without a probed track → 미확인, never "none"');
  assert.equal(audioCtx.info({ id: 's', audioCodec: 'aac', probeOk: true }), 'sound||sound');
  console.log('PASS VM unverified / replayed sound measurements read 미확인');

  // R4 D5: path-free proxy names (proxy_<id>_<hash8>(_NNN).mp4) are matched.
  const proxyRe = /^proxy_(.+)_[0-9a-f]{8}(?:_\d{3})?\.mp4$/;
  assert.ok(/proxy_\(\.\+\)_\[0-9a-f\]\{8\}/.test(source) && proxyRe.exec('proxy_vid_000001_0a1b2c3d_002.mp4')[1] === 'vid_000001', 'D5 proxy name pattern in 30-records');

  // A10: the jobs sheet's failure box is rewritten only when its markup changes
  // (a re-serializing element: the read-back never equals the written string).
  const appSrc = (await import('./lib/app-bundle.mjs')).appBundleJs();
  const jobsErrFn = appSrc.match(/^let jobsErrWritten = null;\nfunction jobsErrWrite\([^]*?^}/m)?.[0];
  assert.ok(jobsErrFn, 'jobsErrWrite exists');
  let writes = 0;
  const box = { _h: '', get innerHTML() { return this._h.replaceAll('&quot;', '"'); }, set innerHTML(v) { writes += 1; this._h = v; } };
  const jctx2 = { box };
  runInNewContext(jobsErrFn + '\nthis.w = jobsErrWrite;', jctx2);
  const errHtml = '<div>오류: &quot;x&quot;</div><details><summary>자세한 오류 내용</summary><pre>&quot;x&quot;</pre></details>';
  for (let i = 0; i < 5; i++) jctx2.w(box, errHtml);
  assert.equal(writes, 1, `A10: five polls with the same error write the box once (${writes})`);
  jctx2.w(box, '<div>other</div>');
  assert.equal(writes, 2, 'A10: a new error is written');
  console.log('PASS VM A10 failure box written only on change');
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
  };
  await openFail();
  console.log('PASS VM examiner open-case failure is reported');
  // A successful switch clears what the shell owns from the previous case
  // (audit badge, audit latch). The deliverables sheet resets itself on the
  // project change (41-finalize.js finResetSheet; verify-finalize-sheet 6).
  dom.auditBadge.className = 'badge ok';
  dom.auditBadge.textContent = '감사 로그 구조 검증됨';
  runInNewContext('auditVerified = true;', ex);
  ex.fetch = async () => ({ ok: true, has_review: true });
  await ex.openCase('/tmp/other-case');
  assert.equal(dom.auditBadge.textContent, '감사 로그 대기');
  assert.equal(runInNewContext('auditVerified', ex), false);
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
  // §10.3: unresolved server queue conflicts (/api/status.mark_conflicts.count) block.
  assert.ok(levels({ pendingMarks: 0, saveState: 'saved', drafts: 0, markConflicts: 2 }).includes('block'), 'queue conflicts block the finalize');
  assert.ok(ex.unsavedMarksChecks({ pendingMarks: 0, saveState: 'saved', drafts: 0, markConflicts: 2 }).some(c => /저장 충돌 2건/.test(c.text)));
  assert.equal(JSON.stringify(levels({ pendingMarks: 0, saveState: 'saved', drafts: 0, markConflicts: 0 })), '["ok"]');
  // Browser-only drafts are counted from the per-id keys (+ an unmigrated legacy key).
  const lsKeys = { 'ft.draft.c1.w1.vid_1': '{}', 'ft.draft.c1.w2.vid_2': '{}', 'ft.draft.c10.w1.vid_3': '{}', 'ft.viewer.c1.annotations.v1': '{"vid_4":{}}', other: '1' };
  ex.localStorage = { get length() { return Object.keys(lsKeys).length; }, key: (i) => Object.keys(lsKeys)[i] ?? null, getItem: (k) => lsKeys[k] ?? null, setItem() {} };
  assert.equal(runInNewContext('unsavedDraftCount("c1")', ex), 3, 'per-id drafts of this case (not c10) + legacy entries');
  assert.ok(!levels({ pendingMarks: 0, saveState: null, drafts: null }).includes('block'));
  // R4 A3/A5: readonly and unknown save states block the finalize too.
  assert.ok(levels({ pendingMarks: 0, saveState: 'readonly', drafts: 0 }).includes('block'), 'readonly (annotations unreadable) blocks');
  assert.ok(levels({ pendingMarks: 0, saveState: 'unknown', drafts: 0 }).includes('block'), 'unknown (conflict lookup failed) blocks');
  console.log('PASS VM finalize precheck blocks on unsaved marks');
  // R4 B16: a "partial" pipeline step is drawn as a warning, not done/failed.
  ex.renderTrack(['done', 'done', 'partial', 'pending', 'pending'], null, null, []);
  const trackHtml = dom.getElementById('track').innerHTML;
  assert.ok(/data-step="partial"/.test(trackHtml) && /부분 결과/.test(trackHtml) && /class="done warned"/.test(trackHtml) && !/class="failed"/.test(trackHtml), 'partial step = warning state');
  console.log('PASS VM partial pipeline step shows as a warning');
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

  await r5TrackU({ fn, source, grid, state, runInNewContext, assert, appSrc: exScript, ex, store, RK: 'ft.viewer.test.ranges', P: 'ft.draft.test.', hydrateSrc, touch });
  await r6TrackU({ fn, source, runInNewContext, assert, ex });
}

// ---- R6 track U (docs/PLAN_R6_2026-10-15.md) ------------------------------
async function r6TrackU({ fn, source, runInNewContext, assert, ex }) {
  const { readFileSync } = await import('node:fs');
  // U1 (S2): the "<seq>:<idx>" string cursor is followed verbatim (URL-
  // encoded) across a page boundary inside ONE file (800 ids); U2: the
  // server's `unreadable` files are collected over the pages (once each).
  const seen = [];
  const items = Array.from({ length: 800 }, (_, i) => ({ id: 'q' + i, seq: 4, cursor: '4:' + i }));
  const pg = { get: async (url) => {
    const q = new URL(url, 'http://x').searchParams;
    seen.push(q.has('after') ? 'a' + q.get('after') : 'o' + q.get('offset'));
    const start = q.has('after') ? items.findIndex((e) => e.cursor === q.get('after')) + 1 : 0;
    const page = items.slice(start, start + 500);
    const more = start + page.length < items.length;
    return { ok: true, count: 800, next_after: more ? page.at(-1).cursor : null, entries: page, unreadable: [{ file: '0005.json', error: 'EOF' }, { file: '0006.json', error: 'utf-8' }] };
  } };
  const got = await runInNewContext(fn('readPagedList') + '\nCONFLICT_PAGE=500;CONFLICT_PAGES_MAX=2000;\nreadPagedList(get, "/api/marks-pending", d => d.entries);', pg);
  assert.ok(got.length === 800 && new Set(got.map((e) => e.id)).size === 800 && !got.truncated && JSON.stringify(seen) === '["o0","a4:499"]', 'U1: 800 ids of one file across the page boundary by string cursor: ' + JSON.stringify(seen));
  assert.ok(Array.isArray(got.unreadable) && got.unreadable.length === 2, 'U2: unreadable queue files carried (once per file over 2 pages): ' + JSON.stringify(got.unreadable));
  assert.ok(/noteQueueUnreadable\(items && items\.unreadable \? items\.unreadable\.length : 0\)/.test(source), 'U2: the queue load shows the unreadable files');
  console.log('PASS VM R6 U1/U2 string cursor across a file boundary, unreadable queue files kept');

  // U3: [원본 다시 가져오기] only while the server names a waiting raw
  // (hRawReimport); a re-render keeps its in-progress state; a busy
  // pipeline keeps it disabled (and the handler refuses).
  const html = (expr) => runInNewContext(expr, ex);
  runInNewContext('hRawReimport = null; hStatusBusy = false; reimportRawBusy = false;', ex);
  assert.ok(!/data-reimport-raw/.test(html('errorHtml("raw 불일치 — 재임포트 필요: case/raw/evidence.raw")')), 'U3: an error text alone (no raw_reimport from the server) shows no reimport button');
  runInNewContext('hRawReimport = { raw_path: "C:/case/raw/e.raw", reason: "marker mismatch", endpoint: "/api/reimport-raw" };', ex);
  const shown = html('errorHtml("raw 불일치 — 재임포트 필요")');
  assert.ok(/<button type="button" data-reimport-raw>원본 다시 가져오기<\/button>/.test(shown), 'U3: shown, enabled, while the server waits for the decision: ' + shown);
  runInNewContext('reimportRawBusy = true;', ex);
  assert.ok(/data-reimport-raw disabled aria-busy="true">다시 가져오는 중…/.test(html('errorHtml("raw 불일치")')), 'U3: a re-render while the request runs keeps "다시 가져오는 중…" (disabled, aria-busy)');
  runInNewContext('reimportRawBusy = false; hStatusBusy = true;', ex);
  assert.ok(/data-reimport-raw disabled title="작업이 진행 중/.test(html('errorHtml("raw 불일치")')), 'U3: disabled while a job is busy');
  ex.__calls = [];
  runInNewContext('askDialog = async () => true; jpost = async (u, b) => { __calls.push([u, b]); return { ok: true }; }; poll = () => {};', ex);
  await runInNewContext('hReimportRaw(null)', ex);
  assert.equal(ex.__calls.length, 0, 'U3: the handler refuses while busy');
  runInNewContext('hStatusBusy = false; hRawReimport = null;', ex);
  await runInNewContext('hReimportRaw(null)', ex);
  assert.equal(ex.__calls.length, 0, 'U3: the handler refuses without a waiting raw');
  console.log('PASS VM R6 U3 reimport button: only with raw_reimport, state kept across re-renders, disabled while busy');

  // U4 (S ↔ U): the shell's close flush prefers hBeforeClose() — it must
  // answer inside the shell's 5 s wait: default 4 s, the viewer's busy
  // budget passed down (FTViewer.save({timeoutMs})), the read-only warning
  // shown here too.
  const asked = [];
  const api = { save: async (opts) => { asked.push(opts); return true; }, summary: () => ({ pending: 0 }), saveState: () => 'readonly' };
  ex.__api = api;
  ex.__toasts = [];
  runInNewContext('hViewerApi = () => __api; FTStore.state.viewer.mounted = true; toast = (m, k) => { __toasts.push([m, k]); };', ex);
  const r4 = await runInNewContext('hBeforeClose()', ex);
  assert.ok(asked.length === 1 && asked[0] && asked[0].timeoutMs > 0 && asked[0].timeoutMs < 4500, 'U4: hBeforeClose passes a busy budget below the shell\'s 5 s wait to FTViewer.save: ' + JSON.stringify(asked));
  assert.ok(r4.ok === true && r4.pending === 0 && /판독 기록을 읽지 못한/.test(r4.warning) && ex.__toasts.some(([m, k]) => /판독 기록을 읽지 못한/.test(m) && k === 'warn'), 'U4: read-only close → ok + warning, and the warning is shown: ' + JSON.stringify([r4, ex.__toasts]));
  runInNewContext('FTStore.state.viewer.mounted = false;', ex);
  // Standalone pages answer too (00-host.js): same shape, prefers the viewer's own hook.
  assert.ok(/window\.hBeforeClose = async \(timeoutMs = 4000\)/.test(source) && /!document\.documentElement\.hasAttribute\("data-ft-host"\)/.test(source), 'U4: a standalone viewer page defines window.hBeforeClose (not on the shell page)');
  console.log('PASS VM R6 U4 hBeforeClose: prompt (4 s, budget passed down), read-only warning returned and shown');

  // U5: FTViewer.save(opts) — the caller's busy budget (default 5 s), not a fixed 20 s for every caller.
  const saveFn = source.match(/^ {2}async save\(opts\) \{[^]*?^ {2}\},/m)?.[0] || '';
  assert.ok(/SAVE_CALLER_WAIT_MS/.test(saveFn) && /Math\.min\(SAVE_BUSY_WAIT_MS/.test(saveFn) && /const SAVE_CALLER_WAIT_MS = 5000;/.test(source), 'U5: save(opts) takes the caller\'s budget (default 5 s, capped at 20 s)');
  assert.ok(/async save\(opts\) \{ return impl \? impl\.save\(opts\) : true; \}/.test(source), 'U5: FTViewer.save passes the options through');
  console.log('PASS VM R6 U5 per-caller save budget');

  // U9: the base -1 group's failure no longer stops the loop before the ordinary group.
  const syncFn = fn('syncMarksNow') || '';
  const unsafeBlock = syncFn.match(/if \(group\.unsafe\) \{[^]*?continue;\s*\}/)?.[0] || '';
  assert.ok(unsafeBlock && !/break/.test(unsafeBlock) && !/if \(result && !result\.ok\) break;/.test(syncFn), 'U9: the unsafe body is sent independently of the ordinary edits');
  console.log('PASS VM R6 U9 unsafe group sent independently');

  // U11: the harness job's limit has a measured basis.
  const yml = readFileSync(new URL('../.github/workflows/harnesses.yml', import.meta.url), 'utf8').replace(/\r\n/g, '\n');
  const minutes = Number((yml.match(/^    timeout-minutes: (\d+)$/m) || [])[1]);
  assert.ok(minutes === 60, 'U11: harnesses job timeout-minutes 60, got ' + minutes);
  assert.ok(/Measured/.test(yml) && /verify-virtual-grid/.test(yml) && /total/i.test(yml), 'U11: the measured harness durations are recorded next to the limit');
  const sh = readFileSync(new URL('./ci-harnesses.sh', import.meta.url), 'utf8');
  assert.ok(/durations/.test(sh), 'U11: ci-harnesses.sh prints the per-harness durations summary');
  console.log('PASS R6 U11 harness CI limit 60 min with measured durations');

  // S4 ↔ U: progress job_type "raw-rehash" (bytes) reads as raw re-verification.
  const rh = { job_type: 'raw-rehash', done: 3 * 1024 ** 3, total: 12 * 1024 ** 3, elapsed_secs: 0, eta_secs: 0 };
  ex.__rh = rh;
  const rhText = runInNewContext('rawRehashText(__rh)', ex);
  assert.ok(/^원본 재검증 3(\.0)? GB \/ 12(\.0)? GB \(25%\)$/.test(rhText), 'S4: raw-rehash progress text: ' + rhText);
  ex.renderTrack(['done', 'running', 'pending', 'pending', 'pending'], null, rh, []);
  const trk = ex.document.getElementById('track').innerHTML;
  assert.ok(/원본 재검증/.test(trk) && /aria-valuenow="25"/.test(trk) && !/raw-rehash/.test(trk), 'S4: the running step shows the re-verification bar, not the raw job_type: ' + trk.slice(0, 300));
  console.log('PASS VM R6 S4 raw-rehash progress shown as 원본 재검증');
}

// ---- R5 track U (docs/PLAN_R5_2026-10-14.md) ------------------------------
async function r5TrackU({ fn, source, grid, state, runInNewContext, assert, appSrc, ex, store, RK, P, hydrateSrc, touch }) {
  const { readFileSync } = await import('node:fs');
  // U1: a draft made while the annotations were unreadable is marked
  // unsafe_base and travels with base_revisions -1 (always a conflict).
  Object.assign(state, { marks: {}, notes: {}, tags: {}, ranges: {}, drafts: {}, dirtyIds: new Set(), revisions: { u1: 6 }, examiners: {} });
  delete grid.markRowMaybe;
  runInNewContext('var annotationsReadOnly = true;\n' + touch + '\nstate.marks.u1 = { status: "important", marked_unix: 1 }; touchAnnotation("u1");', grid);
  assert.ok(state.drafts.u1.unsafe_base === true && state.dirtyIds.has('u1'), 'U1: an edit while read-only marks the draft unsafe_base');
  runInNewContext('annotationsReadOnly = false;\ntouchAnnotation("u1");', grid);
  assert.ok(state.drafts.u1.unsafe_base === true, 'U1: a later edit keeps the mark (the draft still rests on unseen values)');
  const payloadSrc = [fn('marksPayload'), fn('payloadIds'), fn('payloadEmpty')].join('\n');
  const pu = runInNewContext(payloadSrc + '\nmarksPayload();', grid);
  assert.equal(pu.base_revisions.u1, -1, 'U1: an unsafe_base draft is sent with base_revisions -1, not its stored base');
  runInNewContext('touchAnnotation("u1", { base: 6 });', grid);
  assert.ok(!state.drafts.u1.unsafe_base && runInNewContext(payloadSrc + '\nmarksPayload().base_revisions.u1', grid) === 6, 'U1: a conflict resolution (explicit base) clears it');
  // U1: every edit entry point refuses while read-only — nothing changes.
  let toasts = 0;
  Object.assign(state, { marks: {}, tags: {}, drafts: {}, dirtyIds: new Set(), selectedIds: new Set(['u2']), activeId: 'u2' });
  Object.assign(grid, { toast: () => { toasts += 1; }, t: k => k });
  const ops = [fn('annotationsLocked'), fn('refuseLockedEdit'), fn('targetIds'), fn('touchAnnotation'), fn('applyMark'), fn('applyTag'), fn('clearTags'), fn('addTag'), fn('removeTag')].join('\n');
  runInNewContext('annotationsReadOnly = true;\n' + ops + '\napplyMark("important"); applyTag("x"); clearTags(); addTag("u2", "y"); removeTag("u2", "y");', grid);
  assert.ok(!state.marks.u2 && !state.tags.u2 && !state.drafts.u2 && !state.dirtyIds.size && toasts === 5, `U1: read-only refuses mark/tag edits with the reason (${toasts} toasts)`);
  runInNewContext('annotationsReadOnly = false;', grid);
  for (const name of ['applyMarkJournaled', 'applyTagJournaled', 'clearTagsJournaled', 'markActive', 'setRangePoint', 'undoJournal', 'redoJournal']) {
    assert.ok(/^\s+if \(refuseLockedEdit\(\)\) return/m.test(fn(name).split('\n')[1]), `U1: ${name} refuses first while read-only`);
  }
  assert.ok(/refuseLockedEdit\(\)/.test(source.match(/getElementById\("evidenceNote"\)\.addEventListener\("input"[^]*?\}\);/)[0]), 'U1: the note field refuses while read-only');
  console.log('PASS VM R5 U1 read-only edits refused, unsafe_base drafts sent with base -1');

  // U1 transport: the unsafe group goes alone; a server that predates -1
  // (parse refusal, or patch-v1) never applies it — each id becomes a
  // conflict against the case's current value instead.
  const sendSrc = [fn('splitUnsafeBase'), fn('unsafeBaseRefused'), fn('sendUnsafeGroup'), fn('holdUnsafeLocally'), fn('mergeSaveResult'), fn('splitByExaminer'),
    fn('splitPayloadChunks'), fn('legacyPayloadBody'), fn('sendPayloadChunks'), fn('serverAnnotationMap'), fn('annotationMarkId'), fn('normRange'), fn('rangeValid'), payloadSrc].join('\n') +
    '\nPAYLOAD_CHUNK_IDS=2000;PAYLOAD_CHUNK_BYTES=1048576;PAYLOAD_CHUNK_MIN_IDS=50;saveChunkIds=2000;saveLegacyProtocol=false;';
  const body = { schema_version: 2, protocol: 'patch-v2', case_id: 'c', examiner: 'x', client_id: 'w1', exported_unix: 1,
    base_revisions: { s1: -1, n1: 3 }, marks: [{ id: 's1', status: 'reviewed', marked_unix: 1, note: '', examiner: 'x', tags: [], range: { in: null, out: null } }, { id: 'n1', status: 'important', marked_unix: 1, note: '', examiner: 'x', tags: [], range: { in: null, out: null } }], tags: [], deleted_ids: [] };
  const posts = [];
  const u = { posts, body, t: k => k,
    fetchJsonOrNull: async () => ({ ok: true, annotations: { marks: [{ record_id: 's1', status: 'important', note: 'srv' }], tags: [], revisions: { s1: 4 } } }) };
  u.pushMarksToCase = async b => { posts.push(b); if (Object.values(b.base_revisions).includes(-1)) return { ok: false, error: 'marks file x (patch-v2): base_revisions["s1"] must be a non-negative integer' }; return { ok: true, protocol: 'patch-v2', applied: Object.fromEntries(b.marks.map(m => [m.id, 4])), conflicts: [] }; };
  runInNewContext(sendSrc, u);
  const groups = runInNewContext('splitUnsafeBase(body)', u);
  assert.ok(groups.length === 2 && groups[0].unsafe && groups[0].body.marks.map(m => m.id).join() === 's1' && groups[1].body.marks.map(m => m.id).join() === 'n1' && !('s1' in groups[1].body.base_revisions), 'U1: the base -1 ids go in a body of their own');
  const held = await runInNewContext('sendUnsafeGroup(splitUnsafeBase(body)[0].body)', u);
  assert.ok(held.ok && !Object.keys(held.applied).length && held.conflicts.length === 1 && held.conflicts[0].id === 's1' && held.conflicts[0].server_revision === 4 && held.conflicts[0].server.note === 'srv',
    'U1: a server without base -1 → the id is a conflict against the case value (nothing written)');
  u.pushMarksToCase = async b => { posts.push(b); return b.protocol === 'patch-v2' ? { ok: false, error: 'unsupported marks protocol' } : { ok: true }; };
  posts.length = 0;
  const v1 = await runInNewContext('sendUnsafeGroup(splitUnsafeBase(body)[0].body)', u);
  assert.ok(v1.ok && v1.conflicts.length === 1 && posts.every(b => b.protocol === 'patch-v2'), 'U1: an unsafe body is never downgraded to patch-v1 (last write wins)');
  u.pushMarksToCase = async b => ({ ok: true, protocol: 'patch-v2', applied: {}, conflicts: Object.keys(b.base_revisions).map(id => ({ id, server_revision: 4, server: { status: 'important' } })) });
  const tp = await runInNewContext('sendUnsafeGroup(splitUnsafeBase(body)[0].body)', u);
  assert.ok(tp.ok && tp.conflicts.length === 1 && tp.conflicts[0].id === 's1', 'U1: a server that knows -1 (track P) answers the conflict itself');
  console.log('PASS VM R5 U1 base -1 transport: own body, never downgraded, local conflict on an older server');

  // S11 ↔ U: cursor paging (next_after → ?after=), truncation flag (U4).
  const seenQ = [];
  const pg = { get: async url => { const q = new URL(url, 'http://x').searchParams; seenQ.push(q.has('after') ? 'a' + q.get('after') : 'o' + q.get('offset'));
    const after = q.has('after') ? Number(q.get('after')) : 0; const ids = [after + 1, after + 2].filter(n => n <= 5);
    return { ok: true, count: 5, next_after: ids.at(-1) < 5 ? ids.at(-1) : null, entries: ids.map(n => ({ id: 'q' + n })) }; } };
  const cur = await runInNewContext(fn('readPagedList') + '\nCONFLICT_PAGE=500;CONFLICT_PAGES_MAX=2000;\nreadPagedList(get, "/api/marks-pending", d => d.entries);', pg);
  assert.equal(JSON.stringify([cur.map(e => e.id), seenQ, !!cur.truncated]), '[["q1","q2","q3","q4","q5"],["o0","a2","a4"],false]', 'S11: next_after cursors are followed with ?after=');
  pg.get = async () => ({ ok: true, count: 5, next_offset: null, entries: [{ id: 'q1' }, { id: 'q2' }] });
  const short = await runInNewContext('readPagedList(get, "/api/marks-pending", d => d.entries);', pg);
  assert.ok(short.truncated === true, 'U4: fewer entries than the count → flagged truncated');
  console.log('PASS VM R5 cursor paging (after/next_after) + truncation flag');

  // U2/U3: RANGES_KEY is left alone while read-only; an id the case does not
  // list is held (RANGES_HELD_KEY), never dropped, and migrated once listed.
  store.clear();
  store.set(RK, JSON.stringify({ a: { in: 3, out: 4 }, zz: { in: 1, out: 2 } }));
  Object.assign(grid, { RANGES_KEY: RK, RANGES_HELD_KEY: RK + '.held', recordById: new Map(['a', 'b', 'c'].map(id => [id, { id }])), location: { protocol: 'http:' } });
  grid.annotations = { marks: [], tags: [], revisions: {}, error: 'database is locked' };
  runInNewContext('orphanDrafts=new Map();legacyRangesLeft=null;markRowMaybe=new Set();\n' + hydrateSrc + '\nhydrateAnnotations(annotations);', grid);
  assert.ok(store.has(RK) && !state.drafts.a && !store.has(RK + '.held'), 'U2: read-only boot migrates nothing and keeps RANGES_KEY');
  grid.annotations = { marks: [{ record_id: 'a', status: 'important', note: '', range: { in: 1, out: 2 } }], tags: [], revisions: { a: 5 } };
  runInNewContext('hydrateAnnotations(annotations);', grid);
  assert.ok(!store.has(RK) && JSON.parse(store.get(RK + '.held') || '{}').zz?.out === 2 && state.drafts.a?.conflict?.legacy, 'U3: the unknown id is held, the known one migrated, the retired key gone');
  grid.recordById.set('zz', { id: 'zz' });
  runInNewContext('hydrateAnnotations(annotations);', grid);
  assert.ok(!store.has(RK + '.held') && state.drafts.zz?.range?.in === 1 && state.drafts.zz.conflict, 'U3: once the case lists the id, the held range is migrated (conflict) and the hold key goes');
  console.log('PASS VM R5 U2/U3 range migration: none while read-only, unknown ids held');

  // U16 (S15): the server's per-record proxy is the only one used.
  const px = { sanitizeSelector: v => String(v || '').replace(/[^A-Za-z0-9_-]/g, '_') || 'clip', absolutizeArtifactPath: p => 'C:\\case\\' + p };
  runInNewContext(fn('resolveProxyPaths'), px);
  const recs = () => [{ id: 'carve:1', path: 'X' }, { id: 'carve_1', path: 'Y' }];
  const files = ['C:\\case\\artifacts\\proxies\\carve_1_proxy_1.mp4'];
  px.list = recs();
  px.data = { proxies: files };
  runInNewContext('resolveProxyPaths(list, data)', px);
  assert.ok(px.list[0].proxyPath === files[0] && px.list[1].proxyPath === files[0], 'U16 baseline: a name match cannot tell carve:1 from carve_1');
  px.list = recs();
  px.data = { proxies: files, proxyPaths: { carve_1: files[0] } };
  assert.equal(runInNewContext('resolveProxyPaths(list, data)', px), 'server');
  assert.ok(px.list[0].proxyPath === '' && px.list[1].proxyPath === files[0], 'U16: records-meta proxy_paths decide — carve:1 gets none');
  px.list = recs().map((r, i) => ({ ...r, serverProxy: i ? 'artifacts/proxies/p.mp4' : '' }));
  px.data = { proxies: files };
  runInNewContext('resolveProxyPaths(list, data)', px);
  assert.ok(px.list[0].proxyPath === '' && px.list[1].proxyPath === 'C:\\case\\artifacts/proxies/p.mp4', 'U16: a per-row proxy_path is used as given (relative → case path)');
  assert.ok(/proxyPaths: meta\.proxy_paths \|\| meta\.proxyPaths/.test(source), 'U16: the loader carries records-meta proxy_paths');
  console.log('PASS VM R5 U16 per-record proxy_path wins over the name match');

  // U7: the shell's direct attribution patch carries a per-tab random client id.
  assert.ok(!/client_id: "shell-attribute"/.test(appSrc) && /client_id: FIN_CLIENT_ID,/.test(appSrc), 'U7: no fixed shell client_id');
  const idSrc = appSrc.match(/^const FIN_CLIENT_ID = [^]*?^\}\)\(\);/m)[0];
  const ids2 = [0, 1].map(() => runInNewContext(idSrc + '\nFIN_CLIENT_ID;', { crypto: globalThis.crypto, Uint32Array, Math, Date, String, Array }));
  assert.ok(ids2.every(id => /^shell-[0-9a-z]{6,}$/.test(id)) && ids2[0] !== ids2[1], `U7: per-tab random ids (${ids2.join(', ')})`);
  console.log('PASS VM R5 U7 per-tab shell client id');

  // U15: read-only with nothing pending closes without a confirmation (warning only).
  const api = { save: async () => true, summary: () => ({ pending: 0 }), saveState: () => 'readonly' };
  ex.__api = api;
  runInNewContext('hViewerApi = () => __api; FTStore.state.viewer.mounted = true;', ex);
  const c15 = await runInNewContext('hBeforeClose(1000)', ex);
  assert.ok(c15.ok === true && c15.pending === 0 && /저장 안 된 편집이 없습니다/.test(c15.warning), 'U15: read-only + nothing pending → ok, warning only: ' + JSON.stringify(c15));
  api.save = async () => false;
  api.summary = () => ({ pending: 2 });
  const c15b = await runInNewContext('hBeforeClose(1000)', ex);
  assert.ok(c15b.ok === false && /저장 안 된 편집 2건/.test(c15b.message), 'U15: read-only WITH drafts still asks');
  assert.ok(/if \(annotationsReadOnly\) return !Object\.keys\(state\.drafts\)\.length && !state\.dirtyIds\.size && !conflictIds\.size;/.test(source), 'U15: FTViewer.save() is true read-only only when nothing is held here');
  runInNewContext('FTStore.state.viewer.mounted = false;', ex);
  console.log('PASS VM R5 U15 read-only close without edits: no confirmation');

  // S6 ↔ U: a raw-mismatch error carries the [원본 다시 가져오기] action.
  // (R6 U3: the button needs the server's raw_reimport — set here.)
  runInNewContext('hRawReimport = { raw_path: "C:/case/raw/e.raw", reason: "marker mismatch", endpoint: "/api/reimport-raw" };', ex);
  const eh = runInNewContext('errorHtml("raw 불일치 — 재임포트 필요: case/raw/evidence.raw")', ex);
  assert.ok(/data-reimport-raw/.test(eh) && !/data-reimport-raw/.test(runInNewContext('errorHtml("ffmpeg not found")', ex)), 'S6: the reimport button only on a raw mismatch');
  // The button deletes the kept raw: it asks first, and sends the raw_path the
  // server named (/api/status.raw_reimport).
  ex.__calls = [];
  ex.__answer = false;
  runInNewContext('hRawReimport = { raw_path: "C:/case/raw/e.raw", reason: "marker mismatch", endpoint: "/api/reimport-raw" }; askDialog = async () => __answer; jpost = async (u, b) => { __calls.push([u, b]); return { ok: true }; }; poll = () => {};', ex);
  await runInNewContext('hReimportRaw(null)', ex);
  assert.equal(ex.__calls.length, 0, 'S6: cancelled confirmation → nothing posted');
  ex.__answer = true;
  await runInNewContext('hReimportRaw(null)', ex);
  assert.ok(ex.__calls.length === 1 && ex.__calls[0][0] === '/api/reimport-raw' && ex.__calls[0][1].raw_path === 'C:/case/raw/e.raw', 'S6: confirmed → POST /api/reimport-raw {raw_path}: ' + JSON.stringify(ex.__calls));
  runInNewContext('hRawReimport = null;', ex);
  // S13: the new join blocking code has its own text.
  assert.ok(/"audio-only": "/.test(source), 'S13: audio-only join block code is labelled');
  console.log('PASS VM R5 raw-mismatch error offers /api/reimport-raw');

  // U12: the live-server guard covers the whole workstation range 8477–8486.
  const { isWorkstationPort } = await import('./lib/real-server-guard.mjs');
  assert.ok(['8477', '8480', '8486'].every(p => isWorkstationPort(`http://127.0.0.1:${p}`)) && !isWorkstationPort('http://127.0.0.1:8487') && !isWorkstationPort('http://127.0.0.1:51234'), 'U12: 8477..8486 refused, others allowed');
  console.log('PASS R5 U12 real-server guard range');

  // U13: CI shell smoke waits longer than the shell's own deadline, prints
  // the shell's logs on failure; every workflow job has a time limit.
  const root = new URL('../', import.meta.url);
  const shellRs = readFileSync(new URL('src/shell.rs', root), 'utf8');
  const deadline = Number((shellRs.match(/const SMOKE_DEADLINE: Duration = Duration::from_secs\((\d+)\)/) || [])[1]);
  const win = readFileSync(new URL('.github/workflows/windows-ci.yml', root), 'utf8');
  const waitMs = Number((win.match(/WaitForExit\((\d+)\)/) || [])[1]);
  assert.ok(deadline > 0 && waitMs > deadline * 1000, `U13: CI waits ${waitMs} ms > shell deadline ${deadline} s`);
  assert.ok(/-RedirectStandardError/.test(win) && /Get-Content \$log/.test(win), 'U13: the shell smoke prints its stderr/stdout on failure');
  const { readdirSync } = await import('node:fs');
  for (const f of readdirSync(new URL('.github/workflows/', root)).filter(n => n.endsWith('.yml'))) {
    const text = readFileSync(new URL('.github/workflows/' + f, root), 'utf8').replace(/\r\n/g, '\n');
    const jobs = text.split('\njobs:\n')[1] || '';
    const names = [...jobs.matchAll(/^  ([A-Za-z0-9_-]+):$/gm)].map(m => m[1]);
    for (const name of names) {
      const block = jobs.split(new RegExp(`^  ${name}:$`, 'm'))[1].split(/^  [A-Za-z0-9_-]+:$/m)[0];
      assert.ok(/^    timeout-minutes: \d+/m.test(block), `U13: ${f} job ${name} has timeout-minutes`);
    }
  }
  console.log('PASS R5 U13 CI smoke wait > shell deadline, logs on failure, bounded jobs');
}

(process.argv.includes('--unit') ? unitTests() : main()).catch(err => { console.error('DRIVER ERROR:', err); process.exit(2); });
