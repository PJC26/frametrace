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
  // R7 U4: the triage line reads the incremental mark tally.
  runInNewContext('var markTally = null, markTallyTouched = new Set(), markTallyAll = true, markTallyFullCount = 0;\n' + ['markTallyNote', 'markTallyBucket', 'markTallyMove', 'markTallyNow'].map(fn).join('\n'), grid);
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
  const draftsSrc = [fn('annotationsLocked'), fn('lockKey'), fn('refuseLockedEdit'), fn('writerId'), fn('nextDraftSeq'), fn('draftNormRange'), fn('draftRangeOf'), fn('draftValuesOf'), fn('touchAnnotation'), fn('dropDraft')].join('\n');
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
  const ops = [fn('annotationsLocked'), fn('lockKey'), fn('refuseLockedEdit'), fn('targetIds'), fn('touchAnnotation'), fn('applyMark'), fn('applyTag'), fn('clearTags')].join('\n');
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
  await r8TrackU({ fn, source, runInNewContext, assert });
  await r9TrackU({ fn, source, runInNewContext, assert, ex });
  await r10TrackU({ fn, source, runInNewContext, assert, ex });
  await r11TrackU({ fn, runInNewContext, assert, ex });
  await r12TrackU({ fn, runInNewContext, assert, ex });
  await r13TrackU({ fn, runInNewContext, assert, ex });
  await r14TrackU({ fn, runInNewContext, assert, ex });
  await r15TrackU({ fn, runInNewContext, assert, ex });
  await r15TrackIFollowUp({ fn, runInNewContext, assert, ex });
  await r15TrackSFollowUp({ runInNewContext, assert, ex });
}

// ---- R15 track S (8b523ba) follow-up ---------------------------------------
async function r15TrackSFollowUp({ runInNewContext, assert, ex }) {
  const { readFileSync } = await import('node:fs');
  const plain = (v) => JSON.parse(JSON.stringify(v));
  const legacySrc = readFileSync(new URL('../assets/legacy/app/70-finalize.js', import.meta.url), 'utf8');
  const lfn = (name) => legacySrc.match(new RegExp(`^(?:async )?function ${name}\\([^]*?^}`, 'm'))?.[0];
  const lconst = (name) => legacySrc.match(new RegExp(`^(?:const|let) ${name} = .*;$`, 'm'))?.[0] || legacySrc.match(new RegExp(`^(?:const|let) ${name} = \\{[^]*?^\\};`, 'm'))?.[0];
  const lvm = {};
  runInNewContext(['FIN_JOURNAL_HEAD', 'FIN_JOURNAL_INFO', 'FIN_UNPAIRED_SHORT'].map(lconst).concat(['finJournalState', 'finJournalBlocks', 'finJournalHead', 'finJournalDismissCmd', 'finJournalDismissQuote', 'finJournalUnpairedCode', 'finJournalDismissLines', 'finJournalTipExtra', 'finShortDetail', 'finJournalChecks'].map(lfn)).filter(Boolean).join('\n'), lvm);
  const jn = (state, extra) => Object.assign({ path: 'review/finalize.inprogress.json', state, examiner: 'Hong', started_unix: 1760000000, signoff_sha256: 'ab12', error: null }, extra || {});
  const js = (st) => runInNewContext(`FIN.status = st; FIN.incomplete = null; FIN.reportStale = null; FIN.result = null; finResultHtml()`, Object.assign(ex, { st }));
  const lck = (st) => plain(runInNewContext('finJournalChecks(st)', Object.assign(lvm, { st })));
  // dismiss_command present-but-null → no command; absent (older server) → the single-quoted form.
  const nul = { ok: true, phase: 'review-ready', case_dir: 'C:/cases/c9', finalize_journal: jn('unpaired', { unpaired_code: 'lane', reason: '서명 레인 확인 불가', blocks_finalize: true, dismiss_command: null, dismiss_command_cmd: null }) };
  assert.ok(!/치우기/.test(js(nul)) && !lck(nul).some((c) => /치우기/.test(c.sub)), 'R15 S: dismiss_command null → no command (shell + legacy): ' + js(nul));
  const abs = { ok: true, phase: 'review-ready', case_dir: 'C:/cases/c9', finalize_journal: jn('unpaired', { blocks_finalize: true }) };
  assert.ok(/치우기: <code class="mono">frametrace finalize-journal &#39;C:\/cases\/c9&#39; --dismiss<\/code>/.test(js(abs)) && lck(abs).some((c) => c.sub.includes("치우기: frametrace finalize-journal 'C:/cases/c9' --dismiss")), 'R15 S: field absent → built-in single-quoted command (shell + legacy)');
  // in_progress_code other-account: blocks, no dismiss hint.
  const oa = { ok: true, phase: 'review-ready', finalize_journal: jn('in-progress', { in_progress_code: 'other-account', blocks_finalize: true, blocked_until_unix: 1760086400, reason: '이 PC의 다른 계정 FrameTrace 프로세스(pid 812)일 수 있음', dismiss_command: null, dismiss_command_cmd: null }) };
  const oh = js(oa);
  assert.ok(/data-journal="otheraccount"/.test(oh) && /class="h-fin-incomplete">다른 계정의 최종화일 수 있음 — 차단 중</.test(oh) && /pid 812/.test(oh) && /까지 차단/.test(oh) && !/치우기/.test(oh) && /진행 중 코드 other-account/.test(oh), 'R15 S other-account row: ' + oh);
  assert.equal(runInNewContext('finJournalHead(finJournalState(st))', Object.assign(ex, { st: oa })), '다른 계정의 최종화일 수 있음 — 차단 중', 'R15 S other-account checklist head (shell)');
  const lo = lck(oa);
  assert.ok(lo.length === 1 && lo[0].level === 'block' && lo[0].text === '최종화 차단: 다른 계정의 최종화일 수 있음 — 차단 중' && !/치우기/.test(lo[0].sub) && /까지 차단/.test(lo[0].sub), 'R15 S other-account legacy block row: ' + JSON.stringify(lo));
  assert.ok(runInNewContext('finJournalBlocks(finJournalState(st))', Object.assign(ex, { st: { finalize_journal: jn('in-progress', { in_progress_code: 'other-account' }) } })), 'R15 S: other-account blocks even without blocks_finalize');
  assert.ok(/data-journal="inprogress"/.test(js({ ok: true, phase: 'review-ready', finalize_journal: jn('in-progress', { in_progress_code: 'live-writer', blocks_finalize: true }) })), 'R15 S: live-writer stays in-progress');
  // unpaired_code package-changed / local-state; local_record_state in the tooltip.
  for (const [i, vm] of [ex, lvm].entries()) {
    for (const [code, w] of [['package-changed', '패키지 변경됨'], ['local-state', '로컬 기록 상태 불일치']]) {
      assert.equal(runInNewContext('finJournalHead(finJournalState(s))', Object.assign(vm, { s: { finalize_journal: jn('unpaired', { unpaired_code: code, reason: 'x' }) } })), `이전 최종화 짝 없음(${w}) — 확인 필요`, `R15 S unpaired_code ${code} (${i ? 'legacy' : 'shell'})`);
    }
  }
  assert.ok(/로컬 기록 상태 packaged/.test(js({ ok: true, phase: 'review-ready', finalize_journal: jn('unpaired', { unpaired_code: 'package-changed', local_record: true, local_record_state: 'packaged', reason: 'r', blocks_finalize: true, dismiss_command: null }) })), 'R15 S: local_record_state in the tooltip');
  runInNewContext('FIN.status = null;', ex);
  console.log('PASS VM R15 track S follow-up: dismiss null → no command (absent → built-in), other-account blocks without a dismiss hint, package-changed / local-state heads, local_record_state tooltip (shell + legacy)');
}

// ---- R15 track I (db2bcb3) follow-up ---------------------------------------
async function r15TrackIFollowUp({ fn, runInNewContext, assert, ex }) {
  const { readFileSync } = await import('node:fs');
  const plain = (v) => JSON.parse(JSON.stringify(v));
  const legacySrc = readFileSync(new URL('../assets/legacy/app/70-finalize.js', import.meta.url), 'utf8');
  const lfn = (name) => legacySrc.match(new RegExp(`^(?:async )?function ${name}\\([^]*?^}`, 'm'))?.[0];
  const lvm = {};
  runInNewContext([lfn('verifySourcesVerdict'), lfn('verifyChecks')].join('\n'), lvm);
  const base = { ok: true, matched: 3, mismatched: 0, not_comparable: 0, unauthenticated: 0, verified: true, is_result: true, run_id: '9-z', lane_status: 'keyed', head_status: 'intact', head_reason: null, clock_note: null };
  const NOTE = '시계 이상: 실행 시각이 이 PC의 현재 시각보다 600초 넘게 뒤인 실행 1개(9-z)';
  for (const [i, vm] of [ex, lvm].entries()) {
    const tag = i ? 'legacy' : 'shell';
    const v = (o) => plain(runInNewContext('verifySourcesVerdict(o)', Object.assign(vm, { o: Object.assign({}, base, o) })));
    // (1) head_reason: short wording in the tooltip; other-pc → the other-PC warning.
    const words = { 'unusable-line': '쓸 수 없는 줄', 'chain-failed': '체인 실패', 'repaired': '레인 수리', 'malformed': '형식 오류', 'own-head-missing': '결과 실행 머리 없음', 'unreadable': '읽지 못함' };
    for (const [code, w] of Object.entries(words)) {
      const r = v({ is_result: false, verified: false, head_status: 'unknown', head_reason: code, withheld_reason: '재검증 머리 확인 불가 — 결과 없음: x' });
      assert.ok(r.level === 'warn' && r.kind === 'headunknown' && r.tip.includes(`재검증 머리 unknown (${w})`), `R15 I head_reason ${code} → tooltip "${w}" (${tag}): ` + JSON.stringify(r));
    }
    const op = v({ is_result: false, verified: false, head_status: 'unknown', head_reason: 'other-pc', withheld_reason: '재검증 머리 확인 불가 — 결과 없음: 키 kid-2 줄 3개' });
    assert.ok(op.level === 'warn' && op.kind === 'headotherpc' && op.tip.includes('재검증 머리 unknown (다른 PC 머리일 수 있음)'), `R15 I head_reason other-pc → other-PC warning (${tag}): ` + JSON.stringify(op));
    // (2) clock_note: its own warning, the result stands (green stays green).
    const ck = v({ clock_note: NOTE });
    assert.ok(ck.level === 'ok' && ck.clock === NOTE, `R15 I clock_note does not withhold (${tag}): ` + JSON.stringify(ck));
    assert.ok(!('clock' in v({})), `R15 I: no clock_note → no clock (${tag})`);
    // (3) "완료 실행 구간 겹침" → warn.
    const ov = v({ is_result: false, verified: false, withheld_reason: '완료 실행 구간 겹침 — 마지막 완료 실행(2-b)의 구간이 (1-a)와 겹침' });
    assert.ok(ov.level === 'warn' && ov.kind === 'overlap' && ov.label === '원본 재검증: 완료 실행 겹침 — 현재 결과 없음' && /2-b/.test(ov.hint), `R15 I3 overlap → warn (${tag}): ` + JSON.stringify(ov));
    // (4) no usable head + an unusable line: head unknown → warn.
    const nu = v({ is_result: false, verified: false, head_status: 'unknown', head_reason: 'unusable-line', withheld_reason: '재검증 머리 확인 불가 — 결과 없음: 서명 레인에 쓸 수 없는 줄 1개' });
    assert.equal(nu.level, 'warn', `R15 I1 unknown head → warn (${tag})`);
  }
  // clock rows: legacy checklist and the app shell's checklist.
  const lrows = plain(runInNewContext('verifyChecks(verifySourcesVerdict(o))', Object.assign(lvm, { o: Object.assign({}, base, { clock_note: NOTE }) })));
  assert.ok(lrows.length === 1 && lrows[0].level === 'warn' && lrows[0].text === '원본 재검증: 시계 이상' && lrows[0].sub === NOTE, 'R15 I legacy clock row: ' + JSON.stringify(lrows));
  ex.fr15 = Object.assign({}, base, { clock_note: NOTE });
  runInNewContext(`jget = async (u) => (u === '/api/status' ? { ok: true, phase: 'review-ready', pending_marks: 0 } : { ok: true, manifest: { case_id: 'c1' }, annotations: { marks: [] } });
    FIN.audit = { level: 'ok', text: '감사 로그 인증됨' };
    { const v = verifySourcesVerdict(fr15); FIN.verify = { level: v.level, hint: v.hint, kind: v.kind || '', tip: v.tip || '', label: v.label || '', items: v.items || [], more: v.more || 0, clock: v.clock || '' }; }`, ex);
  const sc = plain((await runInNewContext('gatherFinalizeChecks()', ex)).checks);
  assert.ok(sc.some((c) => c.level === 'ok' && /모두 일치/.test(c.text)) && sc.some((c) => c.level === 'warn' && c.text === '원본 재검증: 시계 이상' && c.tip === NOTE), 'R15 I shell: green row + separate clock warning: ' + JSON.stringify(sc));
  const shellSrc = readFileSync(new URL('../assets/app/41-finalize.js', import.meta.url), 'utf8');
  assert.ok(/clock: verdict\.clock \|\| ""/.test(shellSrc), 'R15 I shell: the verify button keeps the clock note');
  runInNewContext('FIN.verify = null; FIN.audit = null; FIN.status = null;', ex);
  console.log('PASS VM R15 track I follow-up: head_reason wording (other-pc → warn), clock_note separate warn row, 완료 실행 겹침 → warn');

  // (4) timeline: index / ffprobe events are all unaudited — one badge per
  // run of same-source, same-kind rows, with the count.
  const tl = { t: (k) => ({ 'timeline.badge.timeUnaudited': '시각 감사 안 됨', 'timeline.tip.timeUnaudited': '감사 기록에 없는 시각' })[k] || k,
    tf: (k, s) => (({ 'timeline.badge.timeUnauditedN': '시각 감사 안 됨 ({n}건 연속)' })[k] || k).replace('{n}', s.n),
    escapeHtml: (v) => String(v).replace(/&/g, '&amp;').replace(/</g, '&lt;').replace(/"/g, '&quot;') };
  runInNewContext(['timelineKindHtml', 'timelineTimeUnaudited', 'timelineUnauditedGroups'].map((n) => fn(n) || '').join('\n'), tl);
  const evs = [
    { source: 'index', kind: 'file-modified', time_unaudited: true }, { source: 'index', kind: 'file-modified', time_unaudited: true }, { source: 'index', kind: 'file-modified', time_unaudited: true },
    { source: 'ffprobe', kind: 'media-creation-time', time_unaudited: true },
    { source: 'carve', kind: 'carved-candidate', time_unaudited: false },
    { source: 'index', kind: 'file-modified', time_unaudited: true },
  ];
  const html = plain(runInNewContext('(() => { const g = timelineUnauditedGroups(evs); return evs.map((e, i) => timelineKindHtml(e.kind, e, g[i])); })()', Object.assign(tl, { evs })));
  const badges = html.filter((h) => /tl-time-unaudited/.test(h));
  assert.equal(badges.length, 3, 'R15 I8: one badge per run, not per row: ' + JSON.stringify(html));
  assert.ok(/시각 감사 안 됨 \(3건 연속\)/.test(html[0]) && html[1] === 'file-modified' && html[2] === 'file-modified' && /">시각 감사 안 됨</.test(html[3]) && html[4] === 'carved-candidate' && /">시각 감사 안 됨</.test(html[5]), 'R15 I8 run heads: ' + JSON.stringify(html));
  const sync = readFileSync(new URL('../assets/viewer/90-sync.js', import.meta.url), 'utf8');
  assert.ok(/timelineKindHtml\(ev\.kind, ev, groups\[i\]\)/.test(sync) && /groups\[i\] === 0 \? "\\n" \+ t\("timeline\.tip\.timeUnaudited"\)/.test(sync), 'R15 I8: the panel groups the badges and keeps the tooltip on later rows');
  const i18n = readFileSync(new URL('../assets/viewer/20-i18n.js', import.meta.url), 'utf8');
  assert.equal((i18n.match(/"timeline\.badge\.timeUnauditedN": /g) || []).length, 2, 'R15 I8: timeUnauditedN in ko + en');
  console.log('PASS VM R15 I8 timeline: unaudited index rows get one badge per run (count), later rows keep the tooltip');
}

// ---- R15 track U (docs/PLAN_R15_2026-10-16.md) -----------------------------
async function r15TrackU({ fn, runInNewContext, assert, ex }) {
  const { readFileSync } = await import('node:fs');
  const plain = (v) => JSON.parse(JSON.stringify(v));
  const legacySrc = readFileSync(new URL('../assets/legacy/app/70-finalize.js', import.meta.url), 'utf8');
  const lfn = (name) => legacySrc.match(new RegExp(`^(?:async )?function ${name}\\([^]*?^}`, 'm'))?.[0];
  const lconst = (name) => legacySrc.match(new RegExp(`^(?:const|let) ${name} = .*;$`, 'm'))?.[0] || legacySrc.match(new RegExp(`^(?:const|let) ${name} = \\{[^]*?^\\};`, 'm'))?.[0];

  // U1 (R14 reviewer PoC): a validation logged after the repair re-confirms
  // only when it is a confirmed state of the same bytes; otherwise its own
  // status shows (a failure) or the record stays "최신 여부 알 수 없음".
  const rctx = { t: (k) => k, tf: (k) => k, viewerTrustFromBundle: () => false, normalizePath: (p) => String(p) };
  runInNewContext(['logTrustOf', 'logTrustUsable', 'logTrustPcKey', 'untrustedStatusOf', 'logTrustSuperseded', 'logRepairs', 'logPosOf', 'logSlots', 'currentLogEntries', 'logTrustNote', 'recordStatusOf', 'splitValidationSlots'].map((n) => {
    const f = fn(n); assert.ok(f, `R15 U1: ${n} exists`); return f; }).concat([fn('logPosOf') || '']).join('\n'), rctx);
  const val = (status, sha, u) => ({ target_path: 'X', validation_status: status, target_sha256: sha, validated_unix: u, trust: 'auth' });
  const cst = (cl, vl, id, recheck) => plain(runInNewContext(`(() => { const own = currentLogEntries(cl, (i) => i.id || '', null).find((e) => e.id === id);
    const sp = splitValidationSlots(logSlots(vl, (i) => i.target_path || '', null));
    return recheck ? recordStatusOf(own, null, null, 'derived', sp.usable.get(own.output_path) || null)
      : recordStatusOf(own, sp.usable.get(own.output_path) || null, sp.untrusted.get(own.output_path) || null, 'candidate-unvalidated'); })()`, Object.assign(rctx, { cl, vl, id, recheck })));
  const rep = { event: 'lane-repair', logged_unix: 2000, trust: 'auth' };
  const join = [{ id: 'j1', output_path: 'X', output_sha256: 'aa11', trust: 'auth' }, rep];
  assert.equal(cst(join, [val('validation-failed', 'aa11', 2500)], 'j1', true).status, 'validation-failed', 'R15 U1 PoC: a join whose output FAILED validation after the repair is not "derived"');
  assert.equal(cst(join, [val('ffprobe-confirmed', 'bb22', 2500)], 'j1', true).status, 'unknown-after-repair', 'R15 U1 PoC: a validation of other bytes (sha differs) never re-confirms a join');
  assert.equal(cst(join, [val('ffprobe-confirmed', '', 2500)], 'j1', true).status, 'unknown-after-repair', 'R15 U1: no target_sha256 → no re-confirmation');
  assert.equal(cst(join, [val('candidate-unvalidated', 'aa11', 2500)], 'j1', true).status, 'candidate-unvalidated', 'R15 U1: a non-confirmed state shows as itself');
  assert.equal(cst(join, [val('ffprobe-video-stream-confirmed', 'AA11', 2500)], 'j1', true).status, 'derived', 'R15 U1: confirmed + same sha (any case) → derived');
  assert.equal(cst(join, [val('validation-failed', 'aa11', 1500)], 'j1', true).status, 'unknown-after-repair', 'R15 U1: a failure logged BEFORE the repair stays unknown');
  const carve = [{ id: 'c1', output_path: 'X', sha256: 'aa11', trust: 'auth' }, rep];
  assert.equal(cst(carve, [val('ffprobe-confirmed', 'bb22', 2500)], 'c1').status, 'unknown-after-repair', 'R15 U1: carve — validation of other bytes never re-confirms');
  assert.equal(cst(carve, [val('ffprobe-confirmed', 'aa11', 2500)], 'c1').status, 'ffprobe-confirmed', 'R15 U1: carve — confirmed + same sha → confirmed');
  assert.equal(cst(carve, [val('validation-failed', 'bb22', 2500)], 'c1').status, 'validation-failed', 'R15 U1: carve — a failure after the repair shows as failed');
  assert.equal(cst([{ id: 'c1', output_path: 'X', trust: 'auth' }, rep], [val('ffprobe-confirmed', 'aa11', 2500)], 'c1').status, 'unknown-after-repair', 'R15 U1: own sha unknown → no re-confirmation');
  console.log('PASS VM R15 U1 re-confirmation after a repair needs a confirmed state of the same SHA-256; otherwise the validation\'s own status');

  // U2: positions first — a validation after the repair in the same lane is
  // after it whatever its clock says; one before it never is.
  const lane = (u) => [{ event: 'recover-inode', id: 'r1', output_path: 'X', sha256: 'aa11', trust: 'auth' }, { event: 'lane-repair', logged_unix: 5000, trust: 'auth' }, Object.assign(val('ffprobe-confirmed', 'aa11', u), { event: 'validate' })];
  const pst = (all) => plain(runInNewContext(`(() => { const own = currentLogEntries(all.filter((i) => i.event === 'recover-inode'), (i) => i.id, null, all)[0];
    const v = currentLogEntries(all.filter((i) => i.event === 'validate'), (i) => i.target_path, null, all)[0];
    return recordStatusOf(own, v, null, 'candidate-unvalidated'); })()`, Object.assign(rctx, { all })));
  assert.equal(pst(lane(1000)).status, 'ffprobe-confirmed', 'R15 U2: after the repair by line, clock behind → still re-confirmed (line wins)');
  const before = lane(9000); before.splice(1, 0, before.pop());
  assert.equal(pst(before).status, 'unknown-after-repair', 'R15 U2: before the repair by line, clock ahead → still unknown');
  console.log('PASS VM R15 U2 lane position wins over validated_unix when both are known');

  // U3: the built-in dismiss command (no server command) is single-quoted, both shells.
  const lvm = {};
  runInNewContext([lfn('finJournalDismissCmd'), lfn('finJournalDismissQuote') || ''].join('\n'), lvm);
  for (const [i, vm] of [ex, lvm].entries()) {
    const tag = i ? 'legacy' : 'shell';
    const c = (dir) => runInNewContext('finJournalDismissCmd(s)', Object.assign(vm, { s: { case_dir: dir, finalize_journal: { dismiss_command: null } } }));
    assert.equal(c("C:\\cases\\it's"), "frametrace finalize-journal 'C:\\cases\\it''s' --dismiss", `R15 U3 Windows path → PowerShell quoting (${tag})`);
    assert.equal(c("/home/a/it's"), "frametrace finalize-journal '/home/a/it'\\''s' --dismiss", `R15 U3 POSIX path → '\\'' (${tag})`);
    assert.equal(c('D:/c2'), "frametrace finalize-journal 'D:/c2' --dismiss", `R15 U3 plain (${tag})`);
    assert.equal(c(''), 'frametrace finalize-journal --dismiss', `R15 U3 no case dir (${tag})`);
  }
  console.log('PASS VM R15 U3 built-in dismiss command single-quoted (shell + legacy)');

  // U4: completed-unrecorded that blocks (S6), "시계 이상" (I4), "다른 PC 머리일 수 있음" (I5).
  const jn = (state, extra) => Object.assign({ path: 'review/finalize.inprogress.json', state, examiner: 'Hong', started_unix: 1760000000, signoff_sha256: 'ab12', error: null }, extra || {});
  const ub = { ok: true, phase: 'review-ready', finalize_journal: jn('completed-unrecorded', { outcome: 'completed', blocks_finalize: true, local_record: true, reason: '완료 기록을 다시 쓰지 못함: 액세스 거부', dismiss_command: null }) };
  const h = runInNewContext(`FIN.status = st; FIN.incomplete = null; FIN.reportStale = null; FIN.result = null; finResultHtml()`, Object.assign(ex, { st: ub }));
  assert.ok(/data-journal="unrecordedblocked"/.test(h) && /class="h-fin-incomplete">이전 최종화 완료 — 완료 기록 못 씀</.test(h) && /액세스 거부/.test(h) && !/치우기/.test(h), 'R15 U4 completed-unrecorded + blocks → warning row: ' + h);
  const ok = runInNewContext(`FIN.status = st; finResultHtml()`, Object.assign(ex, { st: { ok: true, phase: 'review-ready', finalize_journal: jn('completed-unrecorded', { reason: 'r' }) } }));
  assert.ok(/data-journal="unrecorded"/.test(ok) && /class="h-fin-info">이전 최종화 완료 — 완료 기록만 빠짐</.test(ok), 'R15 U4: blocks_finalize absent → info as before: ' + ok);
  const lparts = ['FIN_JOURNAL_HEAD', 'FIN_JOURNAL_INFO', 'FIN_UNPAIRED_SHORT'].map(lconst).concat(['finJournalState', 'finJournalBlocks', 'finJournalHead', 'finJournalDismissCmd', 'finJournalDismissQuote', 'finJournalUnpairedCode', 'finJournalDismissLines', 'finShortDetail', 'finJournalChecks'].map(lfn)).filter(Boolean);
  const ljv = {};
  runInNewContext(lparts.join('\n'), ljv);
  const lrows = plain(runInNewContext('finJournalChecks(st)', Object.assign(ljv, { st: ub })));
  assert.ok(lrows.length === 1 && lrows[0].level === 'block' && lrows[0].text === '최종화 차단: 이전 최종화 완료 — 완료 기록 못 씀' && /액세스 거부/.test(lrows[0].sub), 'R15 U4 legacy block row: ' + JSON.stringify(lrows));
  assert.equal(runInNewContext('finJournalHead(finJournalState(st))', Object.assign(ex, { st: ub })), '이전 최종화 완료 — 완료 기록 못 씀', 'R15 U4 shell checklist head');
  runInNewContext('FIN.status = null;', ex);
  const legacyVerdict = {};
  runInNewContext(lfn('verifySourcesVerdict'), legacyVerdict);
  const base = { ok: true, matched: 3, mismatched: 0, not_comparable: 0, unauthenticated: 0, verified: true, is_result: true, run_id: '9-z', lane_status: 'keyed', head_status: 'intact' };
  for (const [i, vm] of [ex, legacyVerdict].entries()) {
    const tag = i ? 'legacy' : 'shell';
    const v = (o) => plain(runInNewContext('verifySourcesVerdict(o)', Object.assign(vm, { o: Object.assign({}, base, o) })));
    // (track I db2bcb3: "시계 이상" comes as `clock_note` — R15 I follow-up below.)
    const c2 = v({ is_result: false, verified: false, withheld_reason: '표식 없는 더 새 실행 있음; 시계 이상: 결과를 보류하는 실행의 시각이 뒤임' });
    assert.ok(c2.level === 'warn' && /시계 이상/.test(c2.hint), `R15 U4 "시계 이상" in a withheld reason stays in the hint (${tag}): ` + JSON.stringify(c2));
    for (const hs of ['unknown', 'rewritten', 'not-authenticated']) {
      const p = v({ is_result: false, verified: false, head_status: hs, withheld_reason: '재검증 머리 확인 불가 — 결과 없음: 다른 PC 머리일 수 있음 (키 k2)' });
      assert.ok(p.level === 'warn' && p.kind === 'headotherpc' && p.label === '원본 재검증: 다른 PC 머리일 수 있음 — 현재 결과 없음' && /키 k2/.test(p.hint), `R15 U4 "다른 PC 머리일 수 있음" (${hs}) → warn, never err (${tag}): ` + JSON.stringify(p));
    }
    const cut = v({ is_result: false, verified: false, head_status: 'truncated', withheld_reason: '꼬리 잘림 — 결과 없음; 다른 PC 머리일 수 있음' });
    assert.equal(cut.level, 'err', `R15 U4: a cut tail stays err (${tag})`);
  }
  console.log('PASS VM R15 U4 completed-unrecorded blocking → warning / block row; "시계 이상" and "다른 PC 머리일 수 있음" → warn (shell + legacy)');
}

// ---- R14 track U (docs/PLAN_R14_2026-10-16.md) -----------------------------
async function r14TrackU({ fn, runInNewContext, assert, ex }) {
  const { readFileSync } = await import('node:fs');
  const plain = (v) => JSON.parse(JSON.stringify(v));
  const jn = (state, extra) => Object.assign({ path: 'review/finalize.inprogress.json', state, examiner: 'Hong', started_unix: 1760000000, signoff_sha256: 'ab12', error: null }, extra || {});
  const cmdS = 'frametrace finalize-journal "C:/cases/c1" --dismiss';
  const legacySrc = readFileSync(new URL('../assets/legacy/app/70-finalize.js', import.meta.url), 'utf8');
  const lfn = (name) => legacySrc.match(new RegExp(`^(?:async )?function ${name}\\([^]*?^}`, 'm'))?.[0];
  // A one-line declaration (ending in ";") first, else a multi-line object up to "};".
  const lconst = (name) => legacySrc.match(new RegExp(`^(?:const|let) ${name} = .*;$`, 'm'))?.[0] || legacySrc.match(new RegExp(`^(?:const|let) ${name} = \\{[^]*?^\\};`, 'm'))?.[0];

  // U1: the legacy shell blocks on the journal like the app shell, with the
  // dismiss command. Its whole gatherFinalizeChecks runs against a stub server.
  const lnames = ['finJournalState', 'finJournalBlocks', 'finJournalHead', 'finJournalDismissCmd', 'finJournalDismissQuote', 'finJournalUnpairedCode', 'finJournalDismissLines', 'finShortDetail', 'finJournalChecks', 'verifyChecks', 'unsavedMarksChecks', 'readLs', 'viewerWindow', 'viewerSaveState', 'gatherFinalizeChecks'];
  // (Helpers are optional here so a shell without them fails on behaviour.)
  assert.ok(lfn('gatherFinalizeChecks'), 'R14 U1: legacy gatherFinalizeChecks exists');
  const lparts = ['FIN_JOURNAL_HEAD', 'FIN_JOURNAL_INFO', 'FIN_UNPAIRED_SHORT', 'lastVerifyVerdict'].map(lconst).concat(lnames.map(lfn)).filter(Boolean);
  const lvm = { reviewFrameLoaded: false, reviewFrame: null, localStorage: { getItem: () => null }, $: () => null, ST: null,
    jget: async (u) => (u === '/api/status' ? lvm.ST : { manifest: { case_id: 'c1' }, annotations: { marks: [] } }), console };
  runInNewContext(lparts.join('\n') + '\nthis.setVerdict = (v) => { lastVerifyVerdict = v; };', lvm);
  const lchecks = async (st) => { lvm.ST = st; return plain((await runInNewContext('gatherFinalizeChecks()', lvm)).checks); };
  const lu = await lchecks({ ok: true, phase: 'review-ready', case_dir: 'C:/cases/c1', finalize_journal: jn('unpaired', { reason: '서명 레인에 짝 맞는 begin 없음', blocks_finalize: true, dismiss_command: cmdS }) });
  const lb = lu.find((c) => c.level === 'block' && /^최종화 차단: /.test(c.text));
  assert.ok(lb && /이전 최종화 기록 짝 없음 — 확인 필요/.test(lb.text) && lb.sub.includes('치우기: ' + cmdS) && /서명 레인에 짝 맞는 begin 없음/.test(lb.sub), 'R14 U1 legacy unpaired → block + dismiss: ' + JSON.stringify(lu));
  const lr = await lchecks({ ok: true, phase: 'review-ready', finalize_journal: jn('unreadable', { error: 'JSON 파싱 실패', blocks_finalize: true }), case_dir: 'D:/c2' });
  assert.ok(lr.some((c) => c.level === 'block' && /기록을 읽지 못함/.test(c.text) && c.sub.includes("치우기: frametrace finalize-journal 'D:/c2' --dismiss")), 'R14 U1 legacy unreadable → fallback command: ' + JSON.stringify(lr));
  const lrec = await lchecks({ ok: true, phase: 'review-ready', busy: true, finalize_journal: jn('recovering', { blocks_finalize: true }) });
  assert.ok(lrec.some((c) => c.level === 'block' && c.text === '이전 최종화 복구 중') && !lrec.some((c) => /다른 작업이 진행 중/.test(c.text)), 'R14 U1 legacy recovering: ' + JSON.stringify(lrec));
  const loh = await lchecks({ ok: true, phase: 'review-ready', finalize_journal: jn('other-host', { host: '0a1b2c3d4e5f', blocks_finalize: true }) });
  assert.ok(loh.some((c) => c.level === 'block' && /다른 PC의 최종화 — 차단 중/.test(c.text) && /PC 0a1b2c3d4e5f/.test(c.sub)), 'R14 U1 legacy other-host: ' + JSON.stringify(loh));
  const ldone = await lchecks({ ok: true, phase: 'review-ready', finalize_journal: jn('completed', { blocks_finalize: false, dismiss_command: cmdS }) });
  assert.ok(!ldone.some((c) => /최종화/.test(c.text)), 'R14 U1 legacy completed → no row: ' + JSON.stringify(ldone));
  assert.ok(!(await lchecks({ ok: true, phase: 'review-ready' })).some((c) => /최종화/.test(c.text)), 'R14 U1 legacy: no journal → no row');
  assert.ok(/journalRows\.forEach\(c => checks\.push\(c\)\)/.test(legacySrc) && /lastVerifyVerdict = null;/.test(readFileSync(new URL('../assets/legacy/app/50-start.js', import.meta.url), 'utf8')), 'R14 U1/U3: legacy wiring + reset on case switch');
  console.log('PASS VM R14 U1 legacy shell: journal block rows + dismiss command (unpaired / unreadable / recovering / other-host)');

  // U2 (track S9 outcome, P18 "로컬 기록 없음").
  const js = (st) => runInNewContext(`FIN.status = st; FIN.incomplete = null; FIN.reportStale = null; FIN.result = null; finResultHtml()`, Object.assign(ex, { st }));
  const closed = (outcome) => js({ ok: true, phase: 'review-ready', finalize_journal: jn('completed', { outcome, blocks_finalize: false, dismiss_command: cmdS }) });
  const cf = closed('failed');
  assert.ok(/data-journal="closedfailed"/.test(cf) && /class="h-fin-incomplete">이전 최종화 실패 — 기록만 남음</.test(cf) && !/완료/.test(cf) && !/치우기/.test(cf), 'R14 U2 outcome failed → warning, not 완료: ' + cf);
  const cd = closed('dismissed');
  assert.ok(/data-journal="closeddismissed"/.test(cd) && /class="h-fin-incomplete">이전 최종화 치워짐 — 기록만 남음</.test(cd) && !/완료/.test(cd), 'R14 U2 outcome dismissed → warning: ' + cd);
  assert.ok(/class="h-fin-info">이전 최종화 복구됨 — 기록만 남음</.test(closed('recovered')), 'R14 U2 outcome recovered → info');
  assert.ok(/data-journal="completed"/.test(closed('completed')) && /이전 최종화 완료 — 기록만 남음/.test(closed(undefined)), 'R14 U2 outcome completed / absent → 완료');
  assert.ok(/class="h-fin-incomplete">이전 최종화 끝남 — 확인 필요</.test(closed('weird')), 'R14 U2 unknown outcome → 확인 필요');
  const nl = js({ ok: true, phase: 'review-ready', finalize_journal: jn('unpaired', { reason: '짝 없음(로컬 기록 없음) — 이 PC의 열린 최종화 기록이 없습니다', blocks_finalize: true, dismiss_command: cmdS }) });
  assert.ok(/data-journal="unpaired"/.test(nl) && /이전 최종화 짝 없음\(로컬 기록 없음\) — 확인 필요/.test(nl) && /이 PC의 열린 최종화 기록이 없습니다/.test(nl) && /치우기:/.test(nl), 'R14 U2 unpaired 로컬 기록 없음: ' + nl);
  for (const [i, vm] of [ex, lvm].entries()) {
    const head = runInNewContext('finJournalHead(finJournalState(s))', Object.assign(vm, { s: { finalize_journal: jn('unpaired', { reason: '로컬 기록 없음' }) } }));
    assert.equal(head, '이전 최종화 짝 없음(로컬 기록 없음) — 확인 필요', `R14 U2 checklist head (${i ? 'legacy' : 'shell'})`);
    const st = runInNewContext('finJournalState(s).kind', Object.assign(vm, { s: { finalize_journal: jn('completed', { outcome: 'failed' }) } }));
    assert.equal(st, 'closedfailed', `R14 U2 kind (${i ? 'legacy' : 'shell'})`);
  }
  const lfail = await lchecks({ ok: true, phase: 'review-ready', finalize_journal: jn('completed', { outcome: 'failed', blocks_finalize: false }) });
  assert.ok(lfail.some((c) => c.level === 'warn' && c.text === '이전 최종화 실패 — 기록만 남음'), 'R14 U2 legacy failed outcome → warn row: ' + JSON.stringify(lfail));
  runInNewContext('FIN.status = null;', ex);
  console.log('PASS VM R14 U2 closed journal outcome (failed / dismissed warn, recovered / completed info) + 짝 없음(로컬 기록 없음)');

  // U3: a newer run's MISMATCH is a block row (finalize disabled) in both shells.
  const fr = { ok: true, matched: 0, mismatched: 0, not_comparable: 0, unauthenticated: 0, verified: false, is_result: false, run_id: '100-aaaa',
    withheld_reason: '표식 없는 더 새 실행 있음 (실행 1개 · 기록 1개 · 불일치 1건)', newer_runs: 1, newer_records: 1, newer_mismatched: 1,
    newer_mismatches: [{ run_id: '300-c', run_unix: 1760000300, id: 'vid_000007', source_path: 'D:/src/a.mp4', recorded_sha256: 'aa', observed_sha256: 'bb' }], lane_status: 'keyed' };
  runInNewContext(`jget = async (u) => (u === '/api/status' ? { ok: true, phase: 'review-ready', pending_marks: 0 } : { ok: true, manifest: { case_id: 'c1' }, annotations: { marks: [] } });
    FIN.audit = { level: 'ok', text: '감사 로그 인증됨' };
    { const v = verifySourcesVerdict(fr); FIN.verify = { level: v.level, hint: v.hint, kind: v.kind || '', tip: v.tip || '', label: v.label || '', items: v.items || [], more: v.more || 0 }; }`, Object.assign(ex, { fr }));
  const sc = plain((await runInNewContext('gatherFinalizeChecks()', ex)).checks);
  const sb = sc.find((c) => /현재 결과 없음 — 불일치 1건/.test(c.text));
  assert.ok(sb && sb.level === 'block' && sb.items.length === 1, 'R14 U3 shell: newer-run mismatch → block row: ' + JSON.stringify(sc));
  runInNewContext(`FIN.verify = { level: 'warn', hint: 'h', kind: 'noresult', tip: '', label: '', items: [], more: 0 };`, ex);
  const sw = plain((await runInNewContext('gatherFinalizeChecks()', ex)).checks);
  assert.ok(sw.some((c) => c.level === 'warn' && c.text === '원본 재검증: 현재 결과 없음'), 'R14 U3 shell: a plain withheld result stays warn');
  lvm.fr = fr;
  runInNewContext(legacySrc.match(/^function verifySourcesVerdict\([^]*?^}/m)[0] + '\nsetVerdict(verifySourcesVerdict(fr));', lvm);
  const lm = await lchecks({ ok: true, phase: 'review-ready', pending_marks: 0 });
  assert.ok(lm.some((c) => c.level === 'block' && /현재 결과 없음 — 불일치 1건/.test(c.text)), 'R14 U3 legacy: newer-run mismatch → block row: ' + JSON.stringify(lm));
  // Track I3: a lane shorter than its anchored head ("꼬리 잘림") is an error.
  const tr = `verifySourcesVerdict({ ok: true, matched: 3, mismatched: 0, not_comparable: 0, verified: false, is_result: false, run_id: '1-a', withheld_reason: '재검증 레인 꼬리 잘림 — 앵커 항목 수 9, 레인 7', lane_status: 'keyed' })`;
  for (const [i, vm] of [ex, lvm].entries()) {
    const r = plain(runInNewContext(tr, vm));
    assert.ok(r.level === 'err' && r.kind === 'truncated' && /꼬리 잘림/.test(r.toast) && /앵커 항목 수 9/.test(r.hint) && /꼬리 잘림/.test(r.label), `R14 꼬리 잘림 → err (${i ? 'legacy' : 'shell'}): ` + JSON.stringify(r));
  }
  runInNewContext('FIN.verify = null; FIN.audit = null; FIN.status = null;', ex);
  console.log('PASS VM R14 U3 newer-run MISMATCH → block row (shell + legacy); "꼬리 잘림" → err');

  // U4: an own entry before its lane's repair is confirmed again by a usable
  // validation logged after the repair; an entry after the repair is current.
  const rctx = { t: (k) => k, tf: (k) => k, viewerTrustFromBundle: () => false, normalizePath: (p) => String(p) };
  runInNewContext(['logTrustOf', 'logTrustUsable', 'logTrustPcKey', 'untrustedStatusOf', 'logTrustSuperseded', 'logPosOf', 'logSlots', 'currentLogEntries', 'logTrustNote', 'recordStatusOf', 'splitValidationSlots'].map((n) => {
    const f = fn(n); assert.ok(f, `R14 U4: ${n} exists`); return f; }).concat([fn('logRepairs') || fn('logRepairLines') || '']).join('\n'), rctx);
  const carve = [{ id: 'c1', output_path: 'X', sha256: 'aa', trust: 'auth' }, { id: 'c2', output_path: 'Y', trust: 'auth' }, { event: 'lane-repair', logged_unix: 2000, trust: 'auth' }, { id: 'c3', output_path: 'Z', trust: 'auth' }];
  const vlog = (u) => [{ target_path: 'X', validation_status: 'ffprobe-confirmed', target_sha256: 'aa', validated_unix: u, trust: 'auth' }];
  const cst = (cl, chain, vl, id, recheck) => plain(runInNewContext(`(() => { const own = currentLogEntries(cl, (i) => i.id || '', chain).find((e) => e.id === id);
    const sp = splitValidationSlots(logSlots(vl, (i) => i.target_path || '', null));
    return recheck ? recordStatusOf(own, null, null, 'derived', sp.usable.get(own.output_path) || null)
      : recordStatusOf(own, sp.usable.get(own.output_path) || null, sp.untrusted.get(own.output_path) || null, 'candidate-unvalidated'); })()`, Object.assign(rctx, { cl, chain, vl, id, recheck })));
  assert.equal(cst(carve, null, vlog(2500), 'c1').status, 'ffprobe-confirmed', 'R14 U4: re-validated after the repair (marker time) → confirmed');
  assert.equal(cst(carve.filter((x) => !x.event), { status: 'repaired', repairs: [{ line: 3, logged_unix: 2000 }] }, vlog(2500), 'c1').status, 'ffprobe-confirmed', 'R14 U4: repairs[].logged_unix');
  assert.equal(cst(carve, null, vlog(1500), 'c1').status, 'unknown-after-repair', 'R14 U4: validated BEFORE the repair → still unknown');
  assert.equal(cst(carve, null, vlog(2000), 'c1').status, 'unknown-after-repair', 'R14 U4: same second → still unknown (closed side)');
  assert.equal(cst(carve.map((x) => (x.event ? { event: 'lane-repair', trust: 'auth' } : x)), null, vlog(2500), 'c1').status, 'unknown-after-repair', 'R14 U4: repair time unknown → still unknown');
  assert.equal(cst(carve, null, [{ target_path: 'X', validation_status: 'ffprobe-confirmed', target_sha256: 'aa', validated_unix: 2500, trust: 'unauth' }], 'c1').status, 'unknown-after-repair', 'R14 U4: an unusable validation never re-confirms');
  assert.equal(cst(carve, null, [], 'c3').status, 'candidate-unvalidated', 'R14 U4: an own entry after the repair is current');
  assert.equal(cst(carve, null, [], 'c2').status, 'unknown-after-repair', 'R14 U4: before the repair, no validation → unknown');
  const join = [{ id: 'j1', output_path: 'X', output_sha256: 'aa', trust: 'auth' }, { event: 'lane-repair', logged_unix: 2000, trust: 'auth' }];
  assert.equal(cst(join, null, vlog(2500), 'j1', true).status, 'derived', 'R14 U4: a join re-validated after the repair → derived');
  assert.equal(cst(join, null, vlog(1000), 'j1', true).status, 'unknown-after-repair', 'R14 U4: a join validated before the repair stays unknown');
  const recSrc = readFileSync(new URL('../assets/viewer/30-records.js', import.meta.url), 'utf8');
  assert.ok(/recordStatusOf\(item, null, null, "derived", validationsByPath\.get\(normalizePath\(item\.output_path\)\)\)/.test(recSrc), 'R14 U4: join records pass their validation');
  console.log('PASS VM R14 U4 a record re-validated after its lane\'s repair is confirmed again; entries after the repair are current');

  // U5 (track I8): time_unaudited → "시각 감사 안 됨" badge on the timeline.
  const tl = { t: (k) => ({ 'timeline.badge.timeUnaudited': '시각 감사 안 됨', 'timeline.tip.timeUnaudited': '감사 기록에 없는 시각' })[k] || k, escapeHtml: (v) => String(v).replace(/&/g, '&amp;').replace(/</g, '&lt;').replace(/"/g, '&quot;') };
  runInNewContext(fn('timelineKindHtml') + '\n' + (fn('timelineTimeUnaudited') || ''), tl);
  const b1 = runInNewContext('timelineKindHtml("carved-candidate", { time_unaudited: true })', tl);
  assert.ok(/^carved-candidate <span class="badge warn tl-time-unaudited" title="감사 기록에 없는 시각">시각 감사 안 됨<\/span>$/.test(b1), 'R14 U5 time_unaudited badge: ' + b1);
  const b2 = runInNewContext('timelineKindHtml("carved-candidate-time-unaudited", {})', tl);
  assert.ok(/^carved-candidate <span class="badge warn tl-time-unaudited"/.test(b2), 'R14 U5 kind …-time-unaudited: ' + b2);
  assert.equal(runInNewContext('timelineKindHtml("carved-candidate", { time_unaudited: false })', tl), 'carved-candidate', 'R14 U5: audited time → no badge');
  const i18n = readFileSync(new URL('../assets/viewer/20-i18n.js', import.meta.url), 'utf8');
  for (const k of ['timeline.badge.timeUnaudited', 'timeline.tip.timeUnaudited']) assert.equal((i18n.match(new RegExp(`"${k.replace(/\./g, '\\.')}": `, 'g')) || []).length, 2, `R14 U5: ${k} in ko + en`);
  console.log('PASS VM R14 U5 timeline time_unaudited → "시각 감사 안 됨" badge');

  // Track I (3e002d8) follow-up.
  // (1) head_status: truncated / rewritten → err, unknown → warn, always in the tooltip.
  const legacyVerdict = {};
  runInNewContext(legacySrc.match(/^function verifySourcesVerdict\([^]*?^}/m)[0], legacyVerdict);
  const base = { ok: true, matched: 3, mismatched: 0, not_comparable: 0, unauthenticated: 0, verified: true, is_result: true, run_id: '9-z', lane_status: 'keyed' };
  for (const [i, vm] of [ex, legacyVerdict].entries()) {
    const v = (o) => plain(runInNewContext('verifySourcesVerdict(o)', Object.assign(vm, { o: Object.assign({}, base, o) })));
    const tag = i ? 'legacy' : 'shell';
    const rw = v({ is_result: false, verified: false, head_status: 'rewritten', withheld_reason: null });
    assert.ok(rw.level === 'err' && rw.kind === 'truncated' && /재검증 머리 rewritten/.test(rw.tip), `R14 I3 head rewritten → err + tooltip (${tag}): ` + JSON.stringify(rw));
    const tc = v({ is_result: false, verified: false, head_status: 'truncated', withheld_reason: '꼬리 잘림 — 결과 없음: 앵커 4 · 레인 2' });
    assert.ok(tc.level === 'err' && /앵커 4/.test(tc.hint) && /재검증 머리 truncated/.test(tc.tip), `R14 I3 head truncated (${tag}): ` + JSON.stringify(tc));
    const un = v({ is_result: false, verified: false, head_status: 'unknown', withheld_reason: '재검증 머리 확인 불가 — 결과 없음: 머리 뒤 쓸 수 없는 줄' });
    assert.ok(un.level === 'warn' && un.kind === 'headunknown' && /머리 확인 불가 — 현재 결과 없음/.test(un.label) && /머리 뒤 쓸 수 없는 줄/.test(un.hint) && /재검증 머리 unknown/.test(un.tip), `R14 I3 head unknown → warn (${tag}): ` + JSON.stringify(un));
    const ok = v({ head_status: 'intact' });
    assert.ok(ok.level === 'ok' && ok.tip === '재검증 머리 intact', `R14 I3 head intact in the tooltip (${tag}): ` + JSON.stringify(ok));
    assert.ok(!('tip' in v({})) || !v({}).tip, `R14 I3: no head_status (older server) → no head tooltip (${tag})`);
  }
  // (2) the time_unaudited field decides (kind unchanged); the suffix only without it.
  assert.equal(runInNewContext('timelineKindHtml("carved-candidate-time-unaudited", { time_unaudited: false })', tl), 'carved-candidate', 'R14 I8: time_unaudited:false → no badge');
  assert.ok(/tl-time-unaudited/.test(runInNewContext('timelineKindHtml("carve-file", { time_unaudited: true })', tl)), 'R14 I8: time_unaudited:true → badge');
  // (3) an unverified sound measurement shows the server's reason.
  const NOTE = '측정 레인이 이 측정 뒤에서 수리됨(lane-repair) — 최신 여부 알 수 없음 (다시 측정하십시오)';
  const au = { t: (k) => ({ 'audio.tip.trackUnverified': '오디오 트랙 있음 · 측정 기록 검증 실패 — 미확인', 'audio.detail.trackUnverified': '오디오 트랙 · 측정 기록 검증 실패{codec}', 'audio.unknown': '미확인' })[k] || k,
    tf: (k, s) => (au.t(k)).replace('{codec}', (s && s.codec) || ''), audioLevelsError: '',
    audioLevels: { a1: { r: 'unverified', note: NOTE, trust: 'unauth' }, a2: { r: 'unverified', error: 'chain: ' + NOTE, trust: 'unauth' }, a3: { r: 'unverified', trust: 'unauth' } } };
  runInNewContext(['audioInfoFor', 'fmtDb', 'audioTrustNote', 'audioReasonNote', 'audioTipFor', 'audioTipBase', 'audioDetailFor', 'audioDetailBase'].map((n) => fn(n) || '').join('\n') +
    '\nthis.view = (id) => { const r = { id, audioCodec: "aac", probeOk: true }; const a = audioInfoFor(r); Object.assign(r, { audioState: a.state, audioLevel: a.level, audioReason: a.reason, audioTrust: a.trust || "", audioPcKey: a.pcKey || "", audioNote: a.note || "" }); return { tip: audioTipFor(r), detail: audioDetailFor(r) }; };', au);
  const a1 = plain(runInNewContext('view("a1")', au));
  assert.ok(a1.tip === '오디오 트랙 있음 · 측정 기록 검증 실패 — 미확인\n' + NOTE && a1.detail.endsWith(' · ' + NOTE), 'R14 I5 sound tooltip / detail carry the note: ' + JSON.stringify(a1));
  assert.ok(plain(runInNewContext('view("a2")', au)).tip.includes(NOTE), 'R14 I5: `error` as the reason when there is no `note`');
  assert.equal(plain(runInNewContext('view("a3")', au)).tip, '오디오 트랙 있음 · 측정 기록 검증 실패 — 미확인', 'R14 I5: no reason → tooltip unchanged');
  const recAll = readFileSync(new URL('../assets/viewer/30-records.js', import.meta.url), 'utf8');
  assert.ok(/record\.audioNote = audio\.note \|\| "";/.test(recAll), 'R14 I5: records carry audioNote');
  console.log('PASS VM R14 track I follow-up: head_status (err / warn, tooltip), time_unaudited field decides, sound reason in the tooltip');

  // Track S (0608d98) follow-up: completed-unrecorded, refused / superseded,
  // unpaired_code, local_record, single-quoted dismiss_command + dismiss_command_cmd.
  const psCmd = "frametrace finalize-journal 'C:\\cases\\c1' --dismiss";
  const cmdCmd = 'frametrace finalize-journal "C:\\cases\\c1" --dismiss';
  const ur = js({ ok: true, phase: 'review-ready', finalize_journal: jn('completed-unrecorded', { outcome: 'completed', blocks_finalize: false, local_record: true, reason: '로컬 기록이 완료를 확인 — 다음 열기 때 완료 기록', dismiss_command: null, dismiss_command_cmd: null }) });
  assert.ok(/data-journal="unrecorded"/.test(ur) && /class="h-fin-info">이전 최종화 완료 — 완료 기록만 빠짐</.test(ur) && /로컬 기록이 완료를 확인/.test(ur) && /이 PC의 로컬 기록 있음/.test(ur) && !/치우기/.test(ur), 'R14 S completed-unrecorded → info + reason: ' + ur);
  for (const [o, w] of [['refused', '거부됨'], ['superseded', '대체됨']]) {
    const h = closed(o);
    assert.ok(new RegExp(`class="h-fin-incomplete">이전 최종화 ${w} — 기록만 남음<`).test(h) && !/완료/.test(h), `R14 S outcome ${o} → warning "${w}": ` + h);
  }
  const shorts = { 'no-local-record': '로컬 기록 없음', 'local-mismatch': '로컬 기록 불일치', 'local-unreadable': '로컬 기록 읽지 못함', 'lane': '서명 레인 확인 불가', 'journal-mismatch': '저널 불일치', 'legacy-journal': '이전 형식 기록', 'host-unknown': '이 PC 식별 불가' };
  for (const [i, vm] of [ex, lvm].entries()) {
    for (const [code, w] of Object.entries(shorts)) {
      const head = runInNewContext('finJournalHead(finJournalState(s))', Object.assign(vm, { s: { finalize_journal: jn('unpaired', { unpaired_code: code, reason: 'x' }) } }));
      assert.equal(head, `이전 최종화 짝 없음(${w}) — 확인 필요`, `R14 S unpaired_code ${code} (${i ? 'legacy' : 'shell'})`);
    }
    assert.equal(runInNewContext('finJournalHead(finJournalState(s))', Object.assign(vm, { s: { finalize_journal: jn('unpaired', { reason: 'x' }) } })), '이전 최종화 기록 짝 없음 — 확인 필요', `R14 S: no code → plain head (${i ? 'legacy' : 'shell'})`);
  }
  const two = js({ ok: true, phase: 'review-ready', finalize_journal: jn('unpaired', { unpaired_code: 'host-unknown', reason: '이 PC의 호스트 ID를 알 수 없음', blocks_finalize: true, local_record: null, dismiss_command: psCmd, dismiss_command_cmd: cmdCmd }) });
  assert.ok(two.includes(`치우기(PowerShell): <code class="mono">frametrace finalize-journal &#39;C:\\cases\\c1&#39; --dismiss</code>`) || two.includes(`치우기(PowerShell): <code class="mono">${psCmd.replace(/'/g, '&#39;')}</code>`), 'R14 S PowerShell line: ' + two);
  assert.ok(two.includes(`치우기(명령 프롬프트): <code class="mono">${cmdCmd.replace(/"/g, '&quot;')}</code>`) && /이 PC 식별 불가/.test(two) && /짝 없음 코드 host-unknown/.test(two), 'R14 S cmd.exe line + host-unknown: ' + two);
  const ohCmd = js({ ok: true, phase: 'review-ready', finalize_journal: jn('other-host', { host: 'abc', blocks_finalize: true, dismiss_command: psCmd, dismiss_command_cmd: null }) });
  assert.ok(/data-journal="otherhost"/.test(ohCmd) && /치우기: <code class="mono">frametrace finalize-journal/.test(ohCmd), 'R14 S other-host shows the server command: ' + ohCmd);
  const nullCmd = js({ ok: true, phase: 'review-ready', case_dir: 'C:/cases/c9', finalize_journal: jn('unpaired', { blocks_finalize: true }) });
  assert.ok(/치우기: <code class="mono">frametrace finalize-journal &#39;C:\/cases\/c9&#39; --dismiss<\/code>/.test(nullCmd), 'R14 S (R15 S: fields absent, older server) → the built-in fallback: ' + nullCmd);
  assert.ok(!/치우기/.test(js({ ok: true, phase: 'review-ready', finalize_journal: jn('other-host', { host: 'abc', blocks_finalize: true, dismiss_command: null }) })), 'R14 S: other-host without a server command → no fallback');
  const lTwo = await lchecks({ ok: true, phase: 'review-ready', finalize_journal: jn('unpaired', { unpaired_code: 'local-mismatch', reason: 'r', blocks_finalize: true, dismiss_command: psCmd, dismiss_command_cmd: cmdCmd }) });
  assert.ok(lTwo.some((c) => c.level === 'block' && /짝 없음\(로컬 기록 불일치\)/.test(c.text) && c.sub.includes('치우기(PowerShell): ' + psCmd) && c.sub.includes('치우기(명령 프롬프트): ' + cmdCmd)), 'R14 S legacy: both command lines: ' + JSON.stringify(lTwo));
  const lUr = await lchecks({ ok: true, phase: 'review-ready', finalize_journal: jn('completed-unrecorded', { outcome: 'completed', blocks_finalize: false }) });
  assert.ok(!lUr.some((c) => /최종화/.test(c.text)), 'R14 S legacy: completed-unrecorded is info (no row)');
  const lRef = await lchecks({ ok: true, phase: 'review-ready', finalize_journal: jn('completed', { outcome: 'refused', blocks_finalize: false }) });
  assert.ok(lRef.some((c) => c.level === 'warn' && c.text === '이전 최종화 거부됨 — 기록만 남음'), 'R14 S legacy refused → warn');
  runInNewContext('FIN.status = null;', ex);
  console.log('PASS VM R14 track S follow-up: completed-unrecorded (info), refused / superseded (warn), unpaired_code heads, local_record tooltip, PowerShell + cmd.exe dismiss lines (shell + legacy)');
}

// ---- R13 track U (docs/PLAN_R13_2026-10-16.md) -----------------------------
async function r13TrackU({ fn, runInNewContext, assert, ex }) {
  const { readFileSync } = await import('node:fs');
  const plain = (v) => JSON.parse(JSON.stringify(v));
  const js = (st) => runInNewContext(`FIN.status = st; FIN.incomplete = null; FIN.reportStale = null; FIN.result = null; finResultHtml()`, Object.assign(ex, { st }));
  const jn = (state, extra) => Object.assign({ path: 'review/finalize.inprogress.json', state, examiner: 'Hong', started_unix: 1760000000, signoff_sha256: 'ab12', error: null }, extra || {});
  // U1 (track S, P15/P17): unpaired / other-host / recovering.
  const unp = js({ ok: true, phase: 'review-ready', finalize_journal: jn('unpaired') });
  assert.ok(/data-journal="unpaired"/.test(unp) && /이전 최종화 기록 짝 없음 — 확인 필요/.test(unp) && /<small class="h-fin-detail[^"]*">아무것도 바뀌지 않았습니다<\/small>/.test(unp), 'R13 U1 unpaired: ' + unp);
  const oh = js({ ok: true, phase: 'review-ready', finalize_journal: jn('other-host', { host: 'PC-LAB2' }) });
  assert.ok(/data-journal="otherhost"/.test(oh) && /다른 PC의 최종화 — 차단 중/.test(oh) && />PC PC-LAB2<\/small>/.test(oh) && /title="호스트 PC-LAB2/.test(oh), 'R13 U1 other-host (host shown): ' + oh);
  const recv = js({ ok: true, phase: 'review-ready', busy: true, finalize_journal: jn('recovering') });
  assert.ok(/data-journal="recovering"/.test(recv) && /class="h-fin-info">이전 최종화 복구 중</.test(recv) && !/확인 필요/.test(recv), 'R13 U1 recovering is info: ' + recv);
  // The checklist blocks while recovering / another PC's finalize is open.
  const src = readFileSync(new URL('../assets/app/41-finalize.js', import.meta.url), 'utf8');
  assert.ok(/journal\.kind === "recovering" && finJournalBlocks\(journal\)\) add\("block", "이전 최종화 복구 중"/.test(src) && /finJournalBlocks\(journal\)\) \{[^]{0,900}add\("block", "최종화 차단: " \+ finJournalHead\(journal\)/.test(src), 'R13 U1: checklist block rows');
  const css = readFileSync(new URL('../assets/app/40-finalize.css', import.meta.url), 'utf8');
  assert.ok(/\.h-fin-info \{[^}]*var\(--info-bg\)[^}]*var\(--info-fg\)/.test(css), 'R13 U1: .h-fin-info uses the info tokens');
  console.log('PASS VM R13 U1 journal unpaired / other-host (host) / recovering (info) + checklist blocks');

  // U3 (track S6): unreadable row shows the dismiss command.
  const unr = js({ ok: true, phase: 'review-ready', case_dir: 'C:/cases/c1', finalize_journal: jn('unreadable', { error: 'JSON 파싱 실패' }) });
  assert.ok(/data-journal="unreadable"/.test(unr) && /치우기: <code class="mono">frametrace finalize-journal &#39;C:\/cases\/c1&#39; --dismiss<\/code>/.test(unr), 'R13 U3 dismiss command: ' + unr);
  const own = js({ ok: true, phase: 'review-ready', finalize_journal: jn('unreadable', { error: 'x', dismiss_command: 'frametrace finalize-journal --dismiss D:/c' }) });
  assert.ok(/<code class="mono">frametrace finalize-journal --dismiss D:\/c<\/code>/.test(own), 'R13 U3: the server\'s own command wins: ' + own);
  assert.ok(!/finalize-journal --dismiss/.test(js({ ok: true, phase: 'review-ready', finalize_journal: jn('interrupted') })), 'R13 U3: only the unreadable row');
  console.log('PASS VM R13 U3 unreadable journal row names finalize-journal --dismiss');

  // R13 follow-up (track S c76a4df): completed / in-progress, reason,
  // blocks_finalize, blocked_until_unix, dismiss_command on unpaired.
  const cmdS = 'frametrace finalize-journal "C:/cases/c1" --dismiss';
  const done = js({ ok: true, phase: 'review-ready', finalize_recovered: false, finalize_journal: jn('completed', { blocks_finalize: false, dismiss_command: cmdS }) });
  assert.ok(/data-journal="completed"/.test(done) && /class="h-fin-info">이전 최종화 완료 — 기록만 남음</.test(done) && !/확인 필요|치우기/.test(done), 'R13 S completed → info, no command: ' + done);
  const unpR = js({ ok: true, phase: 'review-ready', finalize_journal: jn('unpaired', { reason: '서명 레인에 이 최종화의 시작 기록이 없음(R12 저널)', blocks_finalize: true, dismiss_command: cmdS }) });
  assert.ok(/<small class="h-fin-detail muted">서명 레인에 이 최종화의 시작 기록이 없음\(R12 저널\)<\/small>/.test(unpR) && unpR.includes('<code class="mono">frametrace finalize-journal &quot;C:/cases/c1&quot; --dismiss</code>') && !/아무것도 바뀌지 않았습니다/.test(unpR), 'R13 S unpaired: reason in the body + dismiss_command: ' + unpR);
  const live = js({ ok: true, phase: 'review-ready', finalize_journal: jn('in-progress', { reason: '다른 FrameTrace 프로세스(pid 4120)가 최종화 중', blocks_finalize: true, dismiss_command: cmdS }) });
  assert.ok(/data-journal="inprogress"/.test(live) && /pid 4120/.test(live) && !/치우기/.test(live), 'R13 S in-progress reason: ' + live);
  const until = 1760086400;
  const ohU = js({ ok: true, phase: 'review-ready', finalize_journal: jn('other-host', { host: '0a1b2c3d4e5f', host_id: '0a1b2c3d4e5f'.padEnd(32, '0'), this_host: false, blocks_finalize: true, blocked_until_unix: until, dismiss_command: cmdS }) });
  const untilText = runInNewContext(`new Date(${until} * 1000).toLocaleString()`, ex);
  assert.ok(ohU.includes(`>PC 0a1b2c3d4e5f · ${untilText}까지 차단</small>`), 'R13 S other-host blocked until: ' + ohU);
  const blocks = (k, extra) => runInNewContext(`finJournalBlocks(finJournalState(s))`, Object.assign(ex, { s: { ok: true, finalize_journal: jn(k, extra) } }));
  assert.deepEqual([blocks('other-host', { blocks_finalize: false }), blocks('interrupted', { blocks_finalize: true }), blocks('completed', { blocks_finalize: false }), blocks('unpaired', { blocks_finalize: true }), blocks('other-host'), blocks('interrupted')],
    [false, true, false, true, true, false], 'R13 S: blocks_finalize decides (fallback: recovering / other-host only)');
  runInNewContext('FIN.status = null;', ex);
  console.log('PASS VM R13 S journal completed (info) / in-progress, reason in the body, blocks_finalize, blocked_until, dismiss_command');

  // U2 (track I1, P16): newer usable run without a marker — mismatches shown.
  const legacySrc = readFileSync(new URL('../assets/legacy/app/70-finalize.js', import.meta.url), 'utf8');
  const legacyVm = {};
  runInNewContext(legacySrc.match(/^function verifySourcesVerdict\([^]*?^}/m)[0], legacyVm);
  const nm = `verifySourcesVerdict({ ok: true, matched: 0, mismatched: 0, not_comparable: 0, unauthenticated: 0, verified: false, is_result: false, run_id: 'r1', withheld_reason: '표식 없는 더 새 실행 있음', newer_mismatched: 2, lane_status: 'keyed' })`;
  for (const [i, r] of [runInNewContext(nm, ex), runInNewContext(nm, legacyVm)].entries()) {
    assert.ok(r.level === 'err' && /불일치 2건/.test(r.toast) && /표식 없는 더 새 실행 있음 — 불일치 2건/.test(r.hint) && /현재 결과 없음 — 불일치 2건/.test(r.label), `R13 U2 newer_mismatched shown (${i ? 'legacy' : 'shell'}): ` + JSON.stringify(r));
  }
  const wr = runInNewContext(`verifySourcesVerdict({ matched: 0, mismatched: 0, not_comparable: 0, is_result: false, withheld_reason: '표식 없는 더 새 실행 있음', newer_mismatched: 0, not_verified_reason: null })`, ex);
  assert.ok(wr.level === 'warn' && /현재 결과 없음\(결과 아님\) — 표식 없는 더 새 실행 있음/.test(wr.hint), 'R13 U2 withheld_reason in the hint: ' + JSON.stringify(wr));
  assert.ok(/label: verdict\.label/.test(src) && /FIN\.verify\.label \?/.test(src), 'R13 U2: the checklist uses the verdict label');
  console.log('PASS VM R13 U2 withheld_reason / newer_mismatched (shell + legacy): mismatches never hidden');

  // U4 (track I5): removed_archive on repaired lanes.
  const i18n = readFileSync(new URL('../assets/viewer/20-i18n.js', import.meta.url), 'utf8');
  for (const k of ['match', 'missing', 'differs', 'unknown']) assert.equal((i18n.match(new RegExp(`"chain\\.archive\\.${k}": `, 'g')) || []).length, 2, `R13 U4: chain.archive.${k} in ko + en`);
  const L = { 'chain.failed': '체인 검증 실패', 'chain.structural': '자기 일관성만(인증 아님)', 'chain.notVerified': '인증 불가(변조 판정 아님)', 'chain.repaired': '수리됨(손상된 끝부분 잘라냄)', 'chain.repairedShort': '수리됨',
    'chain.archive.match': '보관본 일치', 'chain.archive.missing': '보관본 없음', 'chain.archive.differs': '보관본 다름', 'chain.archive.unknown': '보관본 확인 안 됨' };
  const tctx = { t: (k) => L[k] || k, tf: (k, s) => (k === 'chain.unauthN' ? `인증 안 된 줄 ${s.n}개` : k) };
  runInNewContext(['logChainArchiveText', 'logChainSummary'].map((n) => fn(n)).join('\n'), tctx);
  const sum = (c) => runInNewContext(`logChainSummary(${JSON.stringify(c)})`, tctx);
  assert.equal(sum({ status: 'repaired', base_status: 'keyed', repaired: true, removed_archive: 'match' }), '수리됨(손상된 끝부분 잘라냄) · 보관본 일치', 'R13 U4 match');
  assert.equal(sum({ status: 'repaired', base_status: 'structural', repaired: true, removed_archive: 'missing' }), '수리됨(손상된 끝부분 잘라냄) · 보관본 없음 · 자기 일관성만(인증 아님)', 'R13 U4 missing');
  assert.equal(sum({ status: 'failed', repaired: true, removed_archive: 'differs' }), '체인 검증 실패 · 수리됨 · 보관본 다름', 'R13 U4 differs on a worse state');
  assert.equal(sum({ status: 'repaired', repaired: true, repairs: [{ removed_archive: 'match' }, { removed_archive: 'differs' }] }), '수리됨(손상된 끝부분 잘라냄) · 보관본 다름', 'R13 U4 worst of repairs[]');
  assert.equal(sum({ status: 'repaired', repaired: true }), '수리됨(손상된 끝부분 잘라냄)', 'R13 U4 absent (older server) → nothing added');
  assert.equal(sum({ status: 'repaired', repaired: true, removed_archive: 'weird' }), '수리됨(손상된 끝부분 잘라냄) · 보관본 확인 안 됨', 'R13 U4 unknown value');
  console.log('PASS VM R13 U4 logChains removed_archive → 보관본 일치/없음/다름');

  // R13 follow-up (track I 32e88a2): newer_mismatches listed under the row;
  // withheld_reason may join several reasons with "; ".
  const mm = [{ run_id: '300-cccc', run_unix: 1760000300, id: 'vid_000007', source_path: 'D:/src/a.mp4', recorded_sha256: 'aa11', observed_sha256: 'bb22' },
    { run_id: '300-cccc', run_unix: 1760000300, id: 'vid_000009', source_path: 'D:/src/b.mp4', recorded_sha256: 'cc33', observed_sha256: 'dd44' }];
  const fr = { ok: true, matched: 0, mismatched: 0, not_comparable: 0, unauthenticated: 0, verified: false, is_result: false, run_id: '100-aaaa',
    withheld_reason: '완료 표식 뒤 사용 불가 줄; 표식 없는 더 새 실행 있음 (실행 1개 · 기록 2개 · 불일치 2건)', not_verified_reason: '표식 없는 더 새 실행 있음 (실행 1개 · 기록 2개 · 불일치 2건)',
    newer_runs: 1, newer_records: 2, newer_mismatched: 2, newer_mismatches: mm, lane_status: 'keyed' };
  for (const [i, vm] of [ex, legacyVm].entries()) {
    const r = plain(runInNewContext('verifySourcesVerdict(fr)', Object.assign(vm, { fr })));
    assert.ok(r.level === 'err' && r.items.length === 2 && r.items[0].id === 'vid_000007' && r.items[0].path === 'D:/src/a.mp4' && /aa11/.test(r.items[0].tip) && /bb22/.test(r.items[0].tip) && /300-cccc/.test(r.items[0].tip) && r.more === 0,
      `R13 I1 newer_mismatches items (${i ? 'legacy' : 'shell'}): ` + JSON.stringify(r));
    assert.ok(/완료 표식 뒤 사용 불가 줄/.test(r.hint) && (r.hint.match(/표식 없는 더 새 실행 있음/g) || []).length === 1, `R13 I1 "; "-joined reasons, each once (${i ? 'legacy' : 'shell'}): ` + r.hint);
  }
  const rowHtml = runInNewContext(`finRowHtml({ level: 'warn', text: 'x', sub: 'y', tip: '', items: verifySourcesVerdict(fr).items, more: 3 })`, ex);
  assert.ok(/<ul class="h-check-items"><li title="기록 SHA-256 aa11\n관측 SHA-256 bb22\n실행 300-cccc[^"]*"><code class="mono">vid_000007<\/code> <span>D:\/src\/a\.mp4<\/span><\/li>/.test(rowHtml) && /외 3건/.test(rowHtml), 'R13 I1 rows rendered under the count: ' + rowHtml);
  const many = runInNewContext('verifySourcesVerdict(fm)', Object.assign(ex, { fm: Object.assign({}, fr, { newer_mismatched: 14, newer_mismatches: Array.from({ length: 14 }, (_, k) => Object.assign({}, mm[0], { id: 'v' + k })) }) }));
  assert.ok(many.items.length === 10 && many.more === 4 && /불일치 14건/.test(many.label), 'R13 I1: 10 rows + 외 4건');
  // withheld_reason with no newer run: the P16 sentence matched by contains.
  const contains = runInNewContext(`verifySourcesVerdict({ matched: 0, mismatched: 0, not_comparable: 0, is_result: false, withheld_reason: 'lane-repair 표식; 표식 없는 더 새 실행 있음 (실행 1개 · 기록 1개 · 불일치 0건)', newer_mismatched: 0 })`, ex);
  assert.ok(contains.level === 'warn' && /lane-repair 표식 · 표식 없는 더 새 실행 있음/.test(contains.hint), 'R13 I1 several reasons: ' + contains.hint);
  assert.ok(/rows\.map\(m => `\\n· \$\{m\.id/.test(legacySrc) && /white-space: pre-line/.test(readFileSync(new URL('../assets/legacy/app/app.css', import.meta.url), 'utf8')), 'R13 I1: the legacy hint lists the rows on their own lines');
  console.log('PASS VM R13 I1 newer_mismatches listed (id + path, hashes in tooltip) in both shells; "; "-joined reasons');

  // R13 follow-up (track I2): repaired-after in the viewer's latest-value logic.
  const rctx = { t: (k) => ({ 'status.repairedAfter': '최신 여부 알 수 없음', 'status.tip.repairedAfter': '최신 여부 알 수 없음 — 수리됨', 'status.unauth': '인증 안 됨', 'status.otherPc': '이 PC에서 인증 불가' })[k] || k, tf: (k) => k, viewerTrustFromBundle: () => false, normalizePath: (p) => String(p) };
  runInNewContext(['logTrustOf', 'logTrustUsable', 'logTrustPcKey', 'untrustedStatusOf', 'logTrustSuperseded', 'logRepairs', 'logPosOf', 'logSlots', 'currentLogEntries', 'logTrustNote', 'recordStatusOf', 'splitValidationSlots', 'statusLabel'].map((n) => {
    const f = fn(n); assert.ok(f, `R13 I2: ${n} exists`); return f; }).join('\n'), rctx);
  const vlog = [
    { target_path: 'A', validation_status: 'ffprobe-confirmed', trust: 'auth' },
    { target_path: 'B', validation_status: 'ffprobe-confirmed', trust: 'auth' },
    { event: 'lane-repair', kept_entries: 2, trust: 'auth' },
    { target_path: 'B', validation_status: 'ffprobe-video-stream-confirmed', trust: 'auth' },
  ];
  const st = (list, chain, path) => plain(runInNewContext(`(() => { const sp = splitValidationSlots(logSlots(list, (i) => i.target_path || '', chain));
    return recordStatusOf(null, sp.usable.get(path) || null, sp.untrusted.get(path) || null, 'candidate-unvalidated'); })()`, Object.assign(rctx, { list, chain, path })));
  const a = st(vlog, { status: 'repaired', repairs: [{ line: 3 }] }, 'A');
  assert.ok(a.status === 'unknown-after-repair' && a.repairedAfter === true, 'R13 I2: newest entry before the repair → 최신 여부 알 수 없음: ' + JSON.stringify(a));
  assert.equal(runInNewContext('statusLabel("unknown-after-repair")', rctx), '최신 여부 알 수 없음', 'R13 I2 label');
  assert.equal(st(vlog, { status: 'repaired', repairs: [{ line: 3 }] }, 'B').status, 'ffprobe-video-stream-confirmed', 'R13 I2: an entry after the repair stays current');
  // From repairs[].line alone (no marker in the list), and from the marker alone.
  const noMarker = vlog.filter((x) => x.event !== 'lane-repair');
  assert.equal(st(noMarker, { status: 'repaired', repairs: [{ line: 2 }] }, 'A').status, 'unknown-after-repair', 'R13 I2: repairs[].line alone');
  assert.equal(st(vlog, null, 'A').status, 'unknown-after-repair', 'R13 I2: the usable marker entry alone');
  assert.equal(st(noMarker, null, 'A').status, 'ffprobe-confirmed', 'R13 I2: no repair → unchanged');
  assert.equal(st([vlog[0], { event: 'lane-repair', trust: 'unauth' }], null, 'A').status, 'ffprobe-confirmed', 'R13 I2: an unusable marker is not a repair');
  // Own entries (carve / join) and filtered lists use the whole lane's positions.
  const ownSt = plain(runInNewContext(`(() => { const all = [{ event: 'recover-inode', output_path: 'X', trust: 'auth' }, { event: 'inspect' }, { event: 'lane-repair', trust: 'auth' }];
    const cur = currentLogEntries(all.filter((i) => i.event === 'recover-inode'), (i) => i.output_path, null, all);
    return recordStatusOf(cur[0], null, null, 'derived'); })()`, rctx));
  assert.equal(ownSt.status, 'unknown-after-repair', 'R13 I2: a filtered list uses the source positions: ' + JSON.stringify(ownSt));
  const recSrc = readFileSync(new URL('../assets/viewer/30-records.js', import.meta.url), 'utf8');
  const bootSrc = readFileSync(new URL('../assets/viewer/10-boot.js', import.meta.url), 'utf8');
  assert.ok(/logSlots\(validationLog, [^\n]*logChains && logChains\.validationLog\)/.test(recSrc) && /logChains && logChains\.filesystemLog, filesystemLog\)/.test(recSrc) && /logChains && logChains\.joinLog\)/.test(recSrc) && /DATA\.logChains\.carveLog/.test(bootSrc), 'R13 I2: every lane passes its chain');
  for (const k of ['status.repairedAfter', 'status.tip.repairedAfter']) assert.equal((i18n.match(new RegExp(`"${k.replace(/\./g, '\\.')}": `, 'g')) || []).length, 2, `R13 I2: ${k} in ko + en`);
  console.log('PASS VM R13 I2 a lane-repair after a record\'s newest entry → 최신 여부 알 수 없음 (validation / own / filtered lanes)');
}

// ---- R12 track U (docs/PLAN_R12_2026-10-16.md) -----------------------------
async function r12TrackU({ fn, runInNewContext, assert, ex }) {
  const { readFileSync } = await import('node:fs');
  const plain = (v) => JSON.parse(JSON.stringify(v));
  // U1: lane_status "structural" (no audit key) is never green, in both shells.
  const legacySrc = readFileSync(new URL('../assets/legacy/app/70-finalize.js', import.meta.url), 'utf8');
  const legacyFn = legacySrc.match(/^function verifySourcesVerdict\([^]*?^}/m)?.[0];
  assert.ok(legacyFn, 'R12 U1: legacy verifySourcesVerdict exists');
  const legacyVm = {};
  runInNewContext(legacyFn, legacyVm);
  const expr = `verifySourcesVerdict({ ok: true, matched: 4, mismatched: 0, not_comparable: 0, unauthenticated: 0, verified: true, is_result: true, run_id: 'r1', lane_status: 'structural' })`;
  for (const [i, r] of [runInNewContext(expr, ex), runInNewContext(expr, legacyVm)].entries()) {
    assert.ok(r.level === 'info' && /자기 일관성만 — 인증 아님/.test(r.toast) && /자기 일관성만 — 인증 아님/.test(r.hint) && !/모두 일치/.test(r.toast + r.hint),
      `R12 U1 structural → info, never 모두 일치 (${i ? 'legacy' : 'shell'}): ` + JSON.stringify(r));
  }
  // A mismatch / an uncompared file still outranks the structural hint.
  assert.equal(runInNewContext(`verifySourcesVerdict({ matched: 4, mismatched: 1, not_comparable: 0, verified: true, is_result: true, lane_status: 'structural' }).level`, ex), 'err', 'R12 U1: mismatch first');
  assert.equal(runInNewContext(`verifySourcesVerdict({ matched: 4, mismatched: 0, not_comparable: 2, verified: true, is_result: true, lane_status: 'structural' }).level`, ex), 'warn', 'R12 U1: not comparable stays warn');
  assert.equal(runInNewContext(`verifySourcesVerdict({ matched: 4, mismatched: 0, not_comparable: 0, verified: true, is_result: true, lane_status: 'keyed' }).level`, ex), 'ok', 'R12 U1: keyed stays green');
  console.log('PASS VM R12 U1 lane_status "structural" → info "자기 일관성만 — 인증 아님" (shell + legacy), never green');

  // U2: finalize_incomplete_detail in the row body (shortened), tooltip = full.
  const longDetail = '검토자 기록 확정 실패: review/signoff.json 사용 중 — 최종화는 끝나지 않았고 패키지는 만들지 않았습니다. 이전 서명을 복원하지 못했습니다: 액세스가 거부되었습니다 (os error 5) — 수동으로 확인하십시오';
  const html = runInNewContext(`FIN.result = null; FIN.incomplete = null; FIN.reportStale = null;
    FIN.status = { ok: true, phase: 'review-ready', finalize_incomplete: true, finalize_incomplete_detail: d };
    finResultHtml()`, Object.assign(ex, { d: longDetail }));
  const body = /<small class="h-fin-detail[^"]*">([^<]*)<\/small>/.exec(html);
  assert.ok(body && /^검토자 기록 확정 실패/.test(body[1]) && body[1].length <= 90 && /…$/.test(body[1]), 'R12 U2: short detail in the row body: ' + html);
  assert.ok(html.includes(`title="${longDetail}"`), 'R12 U2: full detail stays the tooltip');
  const short = runInNewContext(`FIN.status = { ok: true, phase: 'review-ready', finalize_incomplete: true, finalize_incomplete_detail: '보고서 재생성 실패' }; finResultHtml()`, ex);
  assert.ok(/<small class="h-fin-detail[^"]*">보고서 재생성 실패<\/small>/.test(short), 'R12 U2: a short detail is shown whole: ' + short);
  assert.ok(!/h-fin-detail/.test(runInNewContext(`FIN.status = { ok: true, phase: 'review-ready', finalize_incomplete: true, finalize_incomplete_detail: null }; finResultHtml()`, ex)), 'R12 U2: no detail → no line');
  console.log('PASS VM R12 U2 finalize_incomplete_detail shown (short) in the row body');

  // U3 (track S2): finalize_journal / finalize_recovered; absent = nothing.
  const js = (st) => runInNewContext(`FIN.status = st; FIN.incomplete = null; FIN.reportStale = null; FIN.result = null; finResultHtml()`, Object.assign(ex, { st }));
  const chk = js({ ok: true, phase: 'review-ready', finalize_journal: { path: 'review/finalize.inprogress.json', state: 'interrupted', examiner: 'Hong', signoff_sha256: 'ab12', started_unix: 1760000000, error: null }, finalize_recovered: false });
  assert.ok(/data-journal="check"/.test(chk) && /이전 최종화 중단됨 — 확인 필요/.test(chk) && /title="[^"]*ab12[^"]*상태 interrupted/.test(chk), 'R12 U3 journal on disk → 확인 필요: ' + chk);
  const rec = js({ ok: true, phase: 'done', finalize_recovered: true, finalize_journal: null });
  assert.ok(/data-journal="recovered"/.test(rec) && /이전 최종화 중단됨 — 복구됨/.test(rec) && !/확인 필요/.test(rec), 'R12 U3 recovered: ' + rec);
  assert.ok(!/h-fin-journal-row/.test(js({ ok: true, phase: 'review-ready' })) && !/h-fin-journal-row/.test(js({ ok: true, phase: 'review-ready', finalize_recovered: false, finalize_journal: null })), 'R12 U3: absent fields (older server) → no row');
  runInNewContext('FIN.status = null;', ex);
  console.log('PASS VM R12 U3 finalize journal: 이전 최종화 중단됨 — 확인 필요 / 복구됨');

  // U4 (track I3): logChains status "repaired" reads 수리됨, detail = tooltip.
  const i18n = readFileSync(new URL('../assets/viewer/20-i18n.js', import.meta.url), 'utf8');
  assert.equal((i18n.match(/"chain\.repaired": /g) || []).length, 2, 'R12 U4: chain.repaired in ko + en');
  const tctx = {
    t: (k) => ({ 'chain.failed': '체인 검증 실패', 'chain.unreadable': '읽지 못함', 'chain.structural': '자기 일관성만(인증 아님)', 'chain.notVerified': '인증 불가(변조 판정 아님)', 'chain.repaired': '수리됨(손상된 끝부분 잘라냄)', 'chain.lane.validationLog': '검증 로그' })[k] || k,
    tf: (k, s) => (k === 'chain.unauthN' ? `인증 안 된 줄 ${s.n}개` : k),
    escapeHtml: (v) => String(v).replace(/&/g, '&amp;').replace(/</g, '&lt;').replace(/"/g, '&quot;'),
  };
  runInNewContext('function viewerTrustFromBundle() { return false; }\n' + ['logChainArchiveText', 'logChainSummary', 'logChainItems'].map((n) => fn(n)).join('\n'), tctx);
  assert.equal(runInNewContext('logChainSummary({ status: "repaired", entries: 5, detail: "x" })', tctx), '수리됨(손상된 끝부분 잘라냄)', 'R12 U4: repaired is not "인증 불가"');
  assert.equal(runInNewContext('logChainSummary({ status: "repaired", unauthenticated: 2 })', tctx), '수리됨(손상된 끝부분 잘라냄) · 인증 안 된 줄 2개', 'R12 U4: repaired + unauthenticated lines');
  const rows = plain(runInNewContext('logChainItems({ validationLog: { status: "repaired", detail: "잘라낸 37바이트 SHA-256 9f…" } }, ["validationLog"])', tctx));
  assert.ok(rows.length === 1 && /검증 로그<\/strong> 수리됨/.test(rows[0]) && /title="잘라낸 37바이트 SHA-256 9f…"/.test(rows[0]), 'R12 U4 row: ' + JSON.stringify(rows));
  console.log('PASS VM R12 U4 logChains "repaired" → 수리됨 (detail tooltip)');

  // R12 follow-up (track I cc4688c): base_status / repaired on logChains.
  tctx.t = ((prev) => (k) => (k === 'chain.repairedShort' ? '수리됨' : prev(k)))(tctx.t);
  assert.equal((i18n.match(/"chain\.repairedShort": /g) || []).length, 2, 'R12 I3: chain.repairedShort in ko + en');
  assert.equal(runInNewContext('logChainSummary({ status: "repaired", base_status: "structural", repaired: true })', tctx), '수리됨(손상된 끝부분 잘라냄) · 자기 일관성만(인증 아님)', 'R12 I3: repaired shows its base status');
  assert.equal(runInNewContext('logChainSummary({ status: "repaired", base_status: "keyed", repaired: true })', tctx), '수리됨(손상된 끝부분 잘라냄)', 'R12 I3: repaired keyed = repair only');
  assert.equal(runInNewContext('logChainSummary({ status: "failed", base_status: "failed", repaired: true })', tctx), '체인 검증 실패 · 수리됨', 'R12 I3: a worse state wins, repair appended');
  assert.equal(runInNewContext('logChainSummary({ status: "not-verified", unauthenticated: 2, repaired: true })', tctx), '인증 안 된 줄 2개 · 수리됨', 'R12 I3: unauth lines + repair');
  assert.equal(runInNewContext('logChainSummary({ status: "failed", repaired: false })', tctx), '체인 검증 실패', 'R12 I3: no repair, no suffix');
  console.log('PASS VM R12 I3 logChains base_status shown with 수리됨; a worse state gets "· 수리됨"');

  // R12 follow-up (track I2): timeline kind carved-candidate-unaudited.
  const tl = { t: (k) => ({ 'timeline.kind.carvedUnaudited': '후보(감사 안 됨)', 'timeline.tip.carvedUnaudited': 'db/carve_results.json에만' })[k] || k, escapeHtml: tctx.escapeHtml };
  const tlFn = fn('timelineKindHtml');
  assert.ok(tlFn, 'R12 I2: timelineKindHtml exists (90-sync.js)');
  runInNewContext(tlFn + '\n' + (fn('timelineTimeUnaudited') || ''), tl);
  const un = runInNewContext('timelineKindHtml("carved-candidate-unaudited")', tl);
  assert.ok(/class="badge warn tl-unaudited"/.test(un) && />후보\(감사 안 됨\)</.test(un) && !/>carved-candidate/.test(un), 'R12 I2 unaudited badge: ' + un);
  assert.equal(runInNewContext('timelineKindHtml("carved-candidate")', tl), 'carved-candidate', 'R12 I2: plain candidate unchanged');
  const sync = readFileSync(new URL('../assets/viewer/90-sync.js', import.meta.url), 'utf8');
  assert.ok(/\$\{timelineKindHtml\(ev\.kind, ev(, groups\[i\])?\)\}/.test(sync), 'R12 I2: the timeline panel uses timelineKindHtml');
  for (const k of ['timeline.kind.carvedUnaudited', 'timeline.tip.carvedUnaudited']) assert.equal((i18n.match(new RegExp(`"${k.replace(/\./g, '\\.')}": `, 'g')) || []).length, 2, `R12 I2: ${k} in ko + en`);
  console.log('PASS VM R12 I2 timeline carved-candidate-unaudited → "후보(감사 안 됨)" warn badge');

  // R12 follow-up (track I4): expected_run_id in the tooltip when is_result:false.
  const nr = `verifySourcesVerdict({ ok: true, matched: 0, mismatched: 0, not_comparable: 0, unauthenticated: 0, verified: false, is_result: false, run_id: null, expected_run_id: '200-bbbb', not_verified_reason: '이번 실행이 결과 실행이 아님', lane_status: 'keyed' })`;
  for (const [i, r] of [runInNewContext(nr, ex), runInNewContext(nr, legacyVm)].entries()) {
    assert.ok(r.kind === 'noresult' || /현재 결과 없음/.test(r.toast), `R12 I4 (${i ? 'legacy' : 'shell'}) no result`);
    assert.ok(/200-bbbb/.test(r.tip || '') && /결과 실행 없음/.test(r.tip || ''), `R12 I4 expected_run_id in the tip (${i ? 'legacy' : 'shell'}): ` + JSON.stringify(r));
  }
  assert.ok(!runInNewContext(`verifySourcesVerdict({ matched: 0, mismatched: 0, not_comparable: 0, is_result: false }).tip`, ex), 'R12 I4: no expected_run_id → no tip');
  assert.ok(/title="\$\{esc\(c\.tip\)\}"/.test(readFileSync(new URL('../assets/app/41-finalize.js', import.meta.url), 'utf8')) && /\$\("finalHint"\)\.title = \[?verdict\.tip/.test(legacySrc), 'R12 I4: both shells put the tip on screen');
  console.log('PASS VM R12 I4 expected_run_id shown in the tooltip when is_result:false');

  // R12 follow-up (track S 608884d): journal states, recovered detail,
  // incomplete_package_dirs, "최종화 차단:".
  const unr = js({ ok: true, phase: 'review-ready', finalize_journal: { path: 'review/finalize.inprogress.json', state: 'unreadable', examiner: null, started_unix: null, signoff_sha256: null, error: 'JSON 파싱 실패: line 1' } });
  assert.ok(/data-journal="unreadable"/.test(unr) && /이전 최종화 기록을 읽지 못함 — 확인 필요/.test(unr) && /<small class="h-fin-detail[^"]*">JSON 파싱 실패: line 1<\/small>/.test(unr), 'R12 S2 unreadable journal: ' + unr);
  const inprog = js({ ok: true, phase: 'review-ready', finalize_journal: { path: 'p', state: 'in-progress', examiner: 'Kim', started_unix: 1760000000, signoff_sha256: 'ff', error: null } });
  // R13 (track S c76a4df): in-progress = another live FrameTrace process on this PC.
  assert.ok(/data-journal="inprogress"/.test(inprog) && /다른 FrameTrace에서 최종화 중/.test(inprog) && /title="검토자 Kim/.test(inprog), 'R12/R13 S in-progress (another process): ' + inprog);
  const recDetail = '이전 최종화(2026-10-16 10:00)를 되돌렸습니다: 이전 서명 복원, 보고서 재생성 — 확인 필요: reports/package_003_INCOMPLETE';
  const recd = js({ ok: true, phase: 'review-ready', finalize_journal: null, finalize_recovered: true, finalize_recovered_detail: recDetail });
  const recBody = /<small class="h-fin-detail[^"]*">([^<]*)<\/small>/.exec(recd);
  assert.ok(/data-journal="recovered"/.test(recd) && recBody && /^확인 필요: reports\/package_003_INCOMPLETE/.test(recBody[1]) && recd.includes('package_003_INCOMPLETE') && /title="[^"]*되돌렸습니다/.test(recd), 'R12 S2 recovered detail in the body (확인 필요 part first): ' + recd);
  const recOk = js({ ok: true, phase: 'review-ready', finalize_recovered: true, finalize_recovered_detail: '이전 서명 복원' });
  assert.ok(/<small class="h-fin-detail[^"]*">이전 서명 복원<\/small>/.test(recOk), 'R12 S2 recovered detail without 확인 필요: ' + recOk);
  assert.equal(runInNewContext(`FIN.status = { ok: true, phase: 'review-ready', finalize_journal: null }; finJournalRow(FIN.status)`, ex), '', 'R12 S2: null journal (own finalize running) → no row');
  const dirsRes = { ok: false, error: '패키징 실패', report_stale: false, finalize_incomplete: false, incomplete_package_dirs: ['C:/c/reports/package_004_INCOMPLETE', 'C:/c/reports/package 005'] };
  const fid = plain(runInNewContext('finIncompleteOf(r)', Object.assign(ex, { r: dirsRes })));
  assert.deepEqual(fid, { package_dir: 'C:/c/reports/package_004_INCOMPLETE', dirs: dirsRes.incomplete_package_dirs }, 'R12 S1 incomplete_package_dirs: ' + JSON.stringify(fid));
  const dirsHtml = runInNewContext(`FIN.status = { ok: true, phase: 'review-ready' }; FIN.result = null; FIN.reportStale = null; FIN.incomplete = finIncompleteOf(r); const h = finResultHtml(); FIN.incomplete = null; FIN.status = null; h`, ex);
  assert.ok(dirsHtml.includes('<code class="mono">C:/c/reports/package_004_INCOMPLETE</code>') && dirsHtml.includes('<code class="mono">C:/c/reports/package 005</code>') && (dirsHtml.match(/data-pkgdir=/g) || []).length === 2, 'R12 S1 every dir listed: ' + dirsHtml);
  assert.equal(runInNewContext(`[finBlockedError('최종화 차단: 저장 충돌 2건을 먼저 해결하십시오'), finBlockedError('패키징 실패: 최종화 차단: x'), finBlockedError('')].join()`, ex), 'true,false,false', 'R12 S: 최종화 차단 = blocked');
  assert.ok(/최종화 차단:/.test(legacySrc) && /toast\(err, "warn"\)/.test(legacySrc), 'R12 S: the legacy shell shows a blocked finalize as a warning');
  console.log('PASS VM R12 S journal unreadable/interrupted/recovered detail, incomplete_package_dirs, 최종화 차단 = warning');
}

// ---- R11 track U (docs/PLAN_R11_2026-10-16.md) -----------------------------
async function r11TrackU({ fn, runInNewContext, assert, ex }) {
  const { readFileSync } = await import('node:fs');
  const tctx = {
    t: (k) => ({ 'status.unauth': '인증 안 됨', 'status.otherPc': '이 PC에서 인증 불가', 'status.tip.unauth': '인증 안 됨 — …', 'status.tip.newer': '더 최근 인증 안 된 항목 있음', 'trust.sc': '자기 일관성만(인증 아님)', 'trust.unauth': '인증 안 됨' })[k] || k,
    tf: (k, s) => (k === 'trust.otherPc' ? `이 PC에서 인증 불가(변조 판정 아님) — 키 ${s.key} 보유 PC에서 확인` : k),
    escapeHtml: (v) => String(v).replace(/&/g, '&amp;').replace(/</g, '&lt;').replace(/"/g, '&quot;'),
    bundle: false,
  };
  const names = ['logTrustOf', 'logTrustUsable', 'logTrustPcKey', 'logTrustSuperseded', 'untrustedStatusOf', 'logRepairs', 'logPosOf', 'logSlots', 'currentLogEntries', 'logTrustNote', 'recordStatusOf', 'splitValidationSlots', 'statusTitle', 'ftIcon', 'logTrustTagHtml'];
  runInNewContext('function viewerTrustFromBundle() { return bundle; }\n' + names.map((n) => {
    const src = fn(n);
    assert.ok(src, `R11 U1/U2: ${n} exists (viewer)`);
    return src;
  }).join('\n'), tctx);
  const run = (expr, vars) => runInNewContext(expr, Object.assign(tctx, vars || {}));
  const plain = (v) => JSON.parse(JSON.stringify(v));

  // U1: what the server really sends under P7 — the OLDER entry already
  // reads `unauth` with `trust_superseded_by` (1-based line of the newer one).
  const SUP = '더 최근 인증 안 된 항목 있음 — 같은 기록(c1)의 더 새 항목(2번째 줄)이 이 PC에서 인증되지 않아 …';
  const list = [
    { id: 'c1', v: 1, trust: 'unauth', trust_note: SUP, trust_superseded_by: 2 },
    { id: 'c1', v: 2, trust: 'unauth', trust_note: '이 PC에서 인증되지 않음 — HMAC 없음' },
    { id: 'c2', v: 1, trust: 'unauth', trust_note: SUP, trust_superseded_by: 9 },
    { id: 'c3', v: 1, trust: 'unauth', trust_note: 'HMAC 없음' },
  ];
  const cur = run('currentLogEntries(list, (i) => i.id)', { list });
  assert.deepEqual(plain(cur.map((i) => `${i.id}${i.v}`)), ['c12', 'c21', 'c31'], 'R11 U1: latest per key');
  assert.deepEqual(plain(cur.map((i) => !!i.ftNewerUnauth)), [true, true, false], 'R11 U1: trust_superseded_by sets "newer" (on the entry after it, and on a marked entry that is itself current); a plain unauth entry is not newer');
  const st = run('recordStatusOf(cur[0], null, null, "candidate-unvalidated")', { cur });
  assert.ok(st.status === 'unauthenticated' && st.newer === true && /HMAC 없음/.test(st.note), 'R11 U1: ' + JSON.stringify(st));
  const vsplit = run('splitValidationSlots(logSlots(vl, (i) => i.target_path))', { vl: [
    { target_path: '/p', validation_status: 'ffprobe-video-stream-confirmed', trust: 'unauth', trust_superseded_by: 2, trust_note: SUP },
    { target_path: '/p', validation_status: 'validation-failed', trust: 'unauth' },
  ] });
  const vst = run('recordStatusOf(null, vsplit.usable.get("/p") || null, vsplit.untrusted.get("/p") || null, "ffprobe-confirmed")', { vsplit });
  assert.ok(vst.status === 'unauthenticated' && vst.newer === true, 'R11 U1: validation superseded on the server → newer: ' + JSON.stringify(vst));
  assert.ok(/^더 최근 인증 안 된 항목 있음 · 인증 안 됨/.test(run('statusTitle({ status: "unauthenticated", statusTrust: "unauth", statusNewer: true })')));
  console.log('PASS VM R11 U1 trust_superseded_by → "더 최근 인증 안 된 항목 있음"');

  // U2: a legacy authenticated entry (`auth` + "레거시" note) keeps its note in tooltips.
  const LEG = '레인·케이스 결합 없음(레거시) — 이 PC의 감사 키로 인증됐으나 …';
  assert.equal(run('logTrustNote({ trust: "auth", trust_note: "x" })'), '', 'R11 U2: no detail line (default) for a usable entry');
  assert.equal(run('logTrustNote({ trust: "auth", trust_note: n }, true)', { n: LEG }), LEG, 'R11 U2: tooltip note of a usable entry');
  const legOwn = run('recordStatusOf({ id: "c9", trust: "auth", trust_note: n }, { validation_status: "ffprobe-confirmed", trust: "auth", trust_note: n }, null, "candidate-unvalidated")', { n: LEG });
  assert.ok(legOwn.status === 'ffprobe-confirmed' && /레거시/.test(legOwn.note || ''), 'R11 U2: record status keeps the legacy note: ' + JSON.stringify(legOwn));
  const legFb = run('recordStatusOf({ id: "c9", trust: "auth", trust_note: n }, null, null, "candidate-unvalidated")', { n: LEG });
  assert.ok(/레거시/.test(legFb.note || ''), 'R11 U2: fallback status keeps it too');
  const legTip = run('statusTitle({ status: "ffprobe-confirmed", statusTrust: "auth", statusNote: n })', { n: LEG });
  assert.ok(/^ffprobe-confirmed\n레인·케이스 결합 없음\(레거시\)/.test(legTip), 'R11 U2: badge tooltip: ' + legTip);
  const tag = run('logTrustTagHtml({ trust: "auth", trust_note: n })', { n: LEG });
  assert.ok(/title="레인·케이스 결합 없음\(레거시\)/.test(tag) && /i-info/.test(tag), 'R11 U2: detail list tooltip: ' + tag);
  assert.equal(run('logTrustTagHtml({ trust: "auth" })'), '', 'R11 U2: a plain authenticated entry shows nothing');
  console.log('PASS VM R11 U2 legacy note of a usable entry stays in tooltips');

  // U3 (P11): _INCOMPLETE_n, copy tails, trailing dots/spaces; dead home branch gone.
  const pi = runInNewContext(`[
    packageIncomplete('C:\\\\c\\\\reports\\\\package_001_INCOMPLETE_2'),
    packageIncomplete('C:/c/reports/package_001_INCOMPLETE - 복사본'),
    packageIncomplete('C:/c/reports/package_001_INCOMPLETE (3)/'),
    packageIncomplete('C:/c/reports/package_001_INCOMPLETE_2 (2)'),
    packageIncomplete('C:/c/reports/package_001_INCOMPLETE. .'),
    packageIncomplete('/c/reports/package_001_INCOMPLETEX'),
    packageIncomplete('/c/reports/package_001 - 복사본'),
    packageIncomplete('/c/reports/package_001_INCOMPLETE_x'),
  ]`, ex);
  assert.deepEqual(plain(pi), [true, true, true, true, true, false, false, false], 'R11 U3 packageIncomplete: ' + JSON.stringify(pi));
  // Track S (2a51c94) name rule: " - Copy" (any case), several tails, a dot
  // after a tail; and the server's boolean verdict wins over the name.
  const pi2 = runInNewContext(`[
    packageIncomplete('C:/c/reports/package_001_INCOMPLETE - Copy'),
    packageIncomplete('C:/c/reports/package_001_incomplete - COPY (2)'),
    packageIncomplete('C:/c/reports/package_001_INCOMPLETE_3 - 복사본 - 복사본 (4).'),
    packageIncomplete('C:/c/reports/package_001_INCOMPLETE (2) - Copy. '),
    packageIncomplete('C:/c/reports/package_001 - Copy'),
    packageIncomplete('C:/c/reports/package_001_INCOMPLETE', false),
    packageIncomplete('C:/c/reports/package_001', true),
  ]`, ex);
  assert.deepEqual(plain(pi2), [true, true, true, true, false, false, true], 'R11 U3 (S rule) packageIncomplete: ' + JSON.stringify(pi2));
  assert.equal(runInNewContext(`stPackageIncomplete({ package_dir: '/c/reports/p_INCOMPLETE', package_incomplete: false })`, ex), false, 'R11 U3: the status verdict wins over the name');
  assert.equal(runInNewContext(`stPackageIncomplete({ package_dir: '/c/reports/p_INCOMPLETE - Copy' })`, ex), true, 'R11 U3: older server → the name');
  // Track S finalize answers: finalize_incomplete is the verdict, no package path.
  const fi = runInNewContext(`[
    finIncompleteOf({ ok: false, error: 'x 최종화 미완료', finalize_incomplete: false, report_stale: false }),
    finIncompleteOf({ ok: false, error: 'x', finalize_incomplete: true, report_stale: true }),
    finIncompleteOf({ ok: true, package_dir: 'C:/c/reports/package_001_INCOMPLETE', finalize_incomplete: false, report_stale: false }),
  ]`, ex);
  assert.deepEqual(plain(fi), [null, { package_dir: '' }, null], 'R11 (S) finIncompleteOf prefers finalize_incomplete: ' + JSON.stringify(fi));
  const home = readFileSync(new URL('../assets/app/55-home.js', import.meta.url), 'utf8');
  assert.ok(!/case "finalize-incomplete"|it\.finalize_incomplete|it\.package_dir/.test(home), 'R11 U3: no dead recent-cases package branch (recent.rs sends no hint)');
  assert.equal(runInNewContext(`recentState({ phase_hint: 'finalized' }, null).state`, ex), '결과물 생성됨');
  console.log('PASS VM R11 U3 _INCOMPLETE_n / copy tails read 최종화 미완료; dead home branch removed');

  // U4: hash-verify `unauthenticated` (I2) never "모두 일치"; S6 status flag.
  // The legacy shell's verdict, evaluated on its own.
  const legacySrc = readFileSync(new URL('../assets/legacy/app/70-finalize.js', import.meta.url), 'utf8');
  const legacyFn = legacySrc.match(/^function verifySourcesVerdict\([^]*?^}/m)?.[0];
  assert.ok(legacyFn, 'R11 U4: legacy verifySourcesVerdict exists');
  const legacyVm = {};
  runInNewContext(legacyFn, legacyVm);
  const v = runInNewContext(`verifySourcesVerdict({ matched: 3, mismatched: 0, not_comparable: 0, unauthenticated: 2 })`, ex);
  assert.ok(v.level === 'warn' && !/모두 일치/.test(v.toast + v.hint.replace(/"모두 일치"로 쓸 수 없습니다/, '')) && /인증 안 된 줄 2개/.test(v.toast) && /인증 안 됨 2/.test(v.hint), 'R11 U4: ' + JSON.stringify(v));
  // Track I (dfa2f8a) verdict fields: verified / is_result / not_verified_reason.
  const both = (expr) => [runInNewContext(expr, ex), runInNewContext(expr, legacyVm)];
  for (const [i, r] of both(`verifySourcesVerdict({ matched: 0, mismatched: 0, not_comparable: 0, unauthenticated: 1, verified: false, is_result: false, not_verified_reason: '완료 표식 인증 안 됨', run_id: null, lane_status: 'not-verified' })`).entries()) {
    assert.ok(r.level === 'warn' && /현재 결과 없음\(결과 아님\)/.test(r.toast) && /완료 표식 인증 안 됨/.test(r.hint) && !/모두 일치/.test(r.toast), `R11 U4 is_result:false (${i ? 'legacy' : 'shell'}): ` + JSON.stringify(r));
  }
  for (const [i, r] of both(`verifySourcesVerdict({ matched: 4, mismatched: 0, not_comparable: 0, unauthenticated: 0, verified: false, is_result: true, not_verified_reason: '레인 체인 파손', run_id: 'r1', lane_status: 'failed' })`).entries()) {
    assert.ok(r.level === 'warn' && /확인 불가/.test(r.toast) && /레인 체인 파손/.test(r.hint) && !/모두 일치/.test(r.toast), `R11 U4 verified:false (${i ? 'legacy' : 'shell'}): ` + JSON.stringify(r));
  }
  for (const [i, r] of both(`verifySourcesVerdict({ matched: 4, mismatched: 1, not_comparable: 0, verified: false, is_result: true })`).entries()) {
    assert.equal(r.level, 'err', `R11 U4: a mismatch still ranks first (${i ? 'legacy' : 'shell'})`);
  }
  for (const [i, r] of both(`verifySourcesVerdict({ matched: 4, mismatched: 0, not_comparable: 0, unauthenticated: 0, verified: true, is_result: true, run_id: 'r2', lane_status: 'keyed' })`).entries()) {
    assert.ok(r.level === 'ok' && /모두 일치/.test(r.toast), `R11 U4: verified current result → 모두 일치 (${i ? 'legacy' : 'shell'})`);
  }
  assert.equal(runInNewContext(`verifySourcesVerdict({ matched: 0, mismatched: 0, not_comparable: 0, verified: true, is_result: true }).level`, ex), 'warn', 'R11 U4: nothing compared is never green');
  assert.equal(runInNewContext(`verifySourcesVerdict({ matched: 3, mismatched: 1, not_comparable: 0, unauthenticated: 2 }).level`, ex), 'err', 'R11 U4: a mismatch still outranks');
  assert.equal(runInNewContext(`verifySourcesVerdict({ matched: 3, mismatched: 0, not_comparable: 0 }).level`, ex), 'ok', 'R11 U4: absent field (older server) unchanged');
  assert.equal(runInNewContext(`storePhase({ has_job: true, phase: 'done', package_dir: '/c/reports/package_002', package_incomplete: true })`, ex), 'done', 'R11 U4 (S6): flag without a renamed folder → not finalized');
  assert.equal(runInNewContext(`typeof stPackageIncomplete === 'function' && stPackageIncomplete({ package_incomplete: true }) && !stPackageIncomplete({ package_dir: '/c/p_001' })`, ex), true, 'R11 U4 (S6): the status flag counts without a package_dir');
  const legacyFin = readFileSync(new URL('../assets/legacy/app/70-finalize.js', import.meta.url), 'utf8');
  assert.ok(/res\.unauthenticated/.test(legacyFin), 'R11 U4: the legacy shell reads unauthenticated too');
  console.log('PASS VM R11 U4 unauthenticated hash-verify lines never "모두 일치"; package_incomplete flag honoured');
}

// ---- R10 track U (docs/PLAN_R10_2026-10-15.md) -----------------------------
async function r10TrackU({ fn, source, runInNewContext, assert, ex }) {
  const { readFileSync } = await import('node:fs');
  const tctx = {
    t: (k) => ({ 'status.unauth': '인증 안 됨', 'status.otherPc': '이 PC에서 인증 불가', 'status.tip.unauth': '인증 안 됨 — …', 'status.tip.newer': '더 최근 인증 안 된 항목 있음', 'trust.sc': '자기 일관성만(인증 아님)', 'chain.failed': '체인 검증 실패', 'chain.unreadable': '읽지 못함', 'chain.structural': '자기 일관성만(인증 아님)', 'chain.notVerified': '인증 불가(변조 판정 아님)', 'chain.lane.validationLog': '검증 로그', 'chain.lane.joinLog': '이어붙임 로그' })[k] || k,
    tf: (k, s) => (k === 'trust.otherPc' ? `이 PC에서 인증 불가(변조 판정 아님) — 키 ${s.key} 보유 PC에서 확인` : k === 'chain.unauthN' ? `인증 안 된 줄 ${s.n}개` : k),
    escapeHtml: (v) => String(v).replace(/&/g, '&amp;').replace(/</g, '&lt;').replace(/"/g, '&quot;'),
    bundle: false,
  };
  const names = ['logTrustOf', 'logTrustUsable', 'logTrustPcKey', 'logTrustSuperseded', 'untrustedStatusOf', 'logRepairs', 'logPosOf', 'logSlots', 'currentLogEntries', 'logTrustNote', 'recordStatusOf', 'splitValidationSlots', 'statusTitle', 'logChainArchiveText', 'logChainSummary', 'logChainLanesOf', 'logChainItems'];
  runInNewContext('function viewerTrustFromBundle() { return bundle; }\n' + names.map((n) => {
    const src = fn(n);
    assert.ok(src, `R10 U1/U3: ${n} exists (30-records.js)`);
    return src;
  }).join('\n'), tctx);
  const run = (expr, vars) => runInNewContext(expr, Object.assign(tctx, vars || {}));
  const plain = (v) => JSON.parse(JSON.stringify(v));

  // U1 (P7): the newest entry of a key decides; an older usable line is never rolled back to.
  const cur = run('currentLogEntries(list, (i) => i.id)', { list: [
    { id: 'a', v: 1, trust: 'auth' }, { id: 'a', v: 2, trust: 'unauth', trust_note: '이 PC에서 인증되지 않음 — HMAC 없음' },
    { id: 'b', v: 1, trust: 'auth' }, { id: 'b', v: 2, trust: 'pc:k7' }, { id: 'b', v: 3, trust: 'sc' },
    { id: 'c', v: 1, trust: 'unauth' },
    { id: 'd', v: 1 }, { id: 'd', v: 2, trust: 'pc:k8' },
  ] });
  assert.deepEqual(plain(cur.map((i) => `${i.id}${i.v}`)), ['a2', 'b3', 'c1', 'd2'], 'R10 U1 (P7): per key the LATEST entry — a newer unauthenticated line is never skipped for an older authenticated one');
  assert.deepEqual(plain(cur.map((i) => !!i.ftNewerUnauth)), [true, false, false, true], 'R10 U1: flagged only when it superseded a usable entry');
  assert.ok(!('ftNewerUnauth' in JSON.parse(JSON.stringify(cur[0]))), 'R10 U1: the flag never reaches an export');
  const own = run('recordStatusOf(cur[0], null, null, "candidate-unvalidated")', { cur });
  assert.deepEqual(plain(own), { status: 'unauthenticated', trust: 'unauth', newer: true, note: '이 PC에서 인증되지 않음 — HMAC 없음' }, 'R10 U1/U3: newer + the server reason');
  assert.equal(run('recordStatusOf(cur[3], null, null, "derived")', { cur }).status, 'auth-other-pc');
  // Validations of one path: confirmed only when the latest one is usable.
  const split = run('splitValidationSlots(logSlots(list, (i) => i.target_path))', { list: [
    { target_path: '/p1', validation_status: 'validation-failed', trust: 'auth' }, { target_path: '/p1', validation_status: 'ffprobe-video-stream-confirmed', trust: 'unauth' },
    { target_path: '/p2', validation_status: 'validation-failed', trust: 'unauth' }, { target_path: '/p2', validation_status: 'ffprobe-video-stream-confirmed', trust: 'auth' },
  ] });
  assert.ok(!split.usable.has('/p1') && split.untrusted.get('/p1').validation_status === 'ffprobe-video-stream-confirmed', 'R10 U1 (P7): /p1 newest validation is unauthenticated → no usable validation');
  assert.ok(split.usable.get('/p2').validation_status === 'ffprobe-video-stream-confirmed' && !split.untrusted.has('/p2'), 'R10 U1: /p2 newest is authenticated → it confirms');
  const v1 = run('recordStatusOf(null, split.usable.get("/p1") || null, split.untrusted.get("/p1") || null, "ffprobe-confirmed")', { split });
  assert.ok(v1.status === 'unauthenticated' && v1.newer === true, 'R10 U1: the record of /p1 is 인증 안 됨 (newer), not 검증 실패: ' + JSON.stringify(v1));
  const tip = run('statusTitle({ status: "unauthenticated", statusTrust: "unauth", statusNewer: true, statusNote: "HMAC 없음" })');
  assert.ok(/^더 최근 인증 안 된 항목 있음/.test(tip) && /HMAC 없음/.test(tip), 'R10 U1/U3 badge tooltip: ' + tip);
  assert.ok(/더 최근 인증 안 된 항목 있음 · 이 PC에서 인증 불가\(변조 판정 아님\) — 키 k8/.test(run('statusTitle({ status: "auth-other-pc", statusTrust: "pc:k8", statusNewer: true })')), 'R10 U1: newer + other PC');
  console.log('PASS VM R10 U1 recency (P7): a newer unauthenticated entry makes the record unconfirmed, "더 최근 인증 안 된 항목 있음"');

  // U3: trust_note only for unusable entries; logChains rows head the log list.
  assert.equal(run('logTrustNote({ trust: "auth", trust_note: "x" })'), '', 'R10 U3: no note on a usable entry');
  assert.equal(run('logTrustNote({ trust: "unauth", trust_note: " 이 PC에서 인증되지 않음 " })'), '이 PC에서 인증되지 않음');
  const sums = ['absent', 'keyed', 'failed', 'structural', 'not-verified', 'unreadable', 'weird'].map((s) => run('logChainSummary(c)', { c: { status: s, entries: 3 } }));
  assert.deepEqual(sums, ['', '', '체인 검증 실패', '자기 일관성만(인증 아님)', '인증 불가(변조 판정 아님)', '읽지 못함', '인증 불가(변조 판정 아님)'], 'R10 U3 chain summaries: ' + JSON.stringify(sums));
  assert.equal(run('logChainSummary({ status: "keyed", unauthenticated: 2 })'), '인증 안 된 줄 2개');
  assert.equal(run('logChainSummary({ status: "not-verified", unauthenticated: 1 })'), '인증 안 된 줄 1개');
  assert.deepEqual(plain(run('logChainLanesOf({ kind: "joined", anomalies: [1] })')), ['validationLog', 'joinLog', 'anomalyLog']);
  const rows = run('logChainItems(chains, ["validationLog", "joinLog", "carveLog"])', { chains: { validationLog: { status: 'not-verified', unauthenticated: 2, detail: '이 PC에서 인증되지 않은 줄 2개' }, joinLog: { status: 'keyed', unauthenticated: 0 } } });
  assert.ok(rows.length === 1 && /data-lane="validationLog"/.test(rows[0]) && /검증 로그<\/strong> 인증 안 된 줄 2개/.test(rows[0]) && /title="이 PC에서 인증되지 않은 줄 2개"/.test(rows[0]), 'R10 U3 log list head: ' + JSON.stringify(plain(rows)));
  assert.deepEqual(plain(run('logChainItems(null, ["validationLog"])')), [], 'R10 U3: no logChains (older server / bundle) → nothing');
  console.log('PASS VM R10 U3 trust_note in tooltips/detail, logChains state at the head of the log list');

  // U5: a bundle says "생성 PC 기준", never "이 PC".
  tctx.bundle = true;
  assert.equal(run('logTrustNote({ trust: "unauth", trust_note: "이 PC에서 인증되지 않음" })'), '생성 PC에서 인증되지 않음', 'R10 U5: bundle trust_note');
  assert.ok(/title="생성 PC에서/.test(run('logChainItems({ validationLog: { status: "not-verified", detail: "이 PC에서 인증 불가" } }, ["validationLog"])')[0]), 'R10 U5: bundle chain detail');
  tctx.bundle = false;
  const i18nSrc = readFileSync(new URL('../assets/viewer/20-i18n.js', import.meta.url), 'utf8');
  const tableSrc = i18nSrc.slice(i18nSrc.indexOf('const I18N = {'), i18nSrc.indexOf('\n};', i18nSrc.indexOf('const I18N = {')) + 3);
  const ictx = { storageGet: () => 'ko', LOCALE_KEY: 'x', state: { locale: 'ko' } };
  runInNewContext(tableSrc + '\n' + ['viewerTrustFromBundle', 'trustI18nKey', 't', 'tf'].map((n) => {
    const src = fn(n);
    assert.ok(src, `R10 U5: ${n} exists (20-i18n.js)`);
    return src;
  }).join('\n') + '\nthis.__I18N = I18N;', ictx);
  const live = runInNewContext('[tf("trust.otherPc", { key: "k1" }), t("status.otherPc"), t("status.tip.unauth"), t("trust.sc")]', ictx);
  assert.ok(live.every((s) => !/생성 PC/.test(s)) && /^이 PC에서 인증 불가/.test(live[0]), 'R10 U5: live data keeps "이 PC": ' + JSON.stringify(live));
  runInNewContext('var trustFromBundle = true;', ictx);
  const bun = runInNewContext('[tf("trust.otherPc", { key: "k1" }), t("status.otherPc"), t("status.tip.unauth"), t("trust.sc"), t("audio.selfConsistent")]', ictx);
  assert.ok(bun.every((s) => /생성 PC 기준/.test(s) && !/이 PC/.test(s)) && /키 k1 보유 PC에서 확인/.test(bun[0]), 'R10 U5: bundle → "생성 PC 기준": ' + JSON.stringify(bun));
  const en = runInNewContext('Object.keys(__I18N.ko).filter((k) => /@bundle$/.test(k) && !(k in __I18N.en))', ictx);
  assert.deepEqual(plain(en), [], 'R10 U5: every @bundle key in both locales');
  // Only data fetched from this server's API is live (99-loader LIVE_TRUST_DATA).
  assert.ok(/LIVE_TRUST_DATA\.add\(data\)/.test(source) && /const trustFromBundle = !\(typeof LIVE_TRUST_DATA === "object" && LIVE_TRUST_DATA\.has\(DATA\)\)/.test(source), 'R10 U5: live = the loader\'s API data object only');
  console.log('PASS VM R10 U5 bundle trust texts say "생성 PC 기준"');

  // U2: an unreadable key configuration first, in its own words (never "검증 실패").
  const kv = runInNewContext(`auditVerdict({ ok: true, integrity: 'integrity-not-verified', verified: 2, entries: 9, logs: [
    { name: 'a.jsonl', integrity: 'not-verified', entries: 5, unauthenticated_lines: 3, other_pc_lines: 0, trust: 'HMAC 없는 줄 3개' },
    { name: 'b.jsonl', integrity: 'not-verified', entries: 4, key_config_error: true, trust: '키 설정을 읽지 못함: FRAMETRACE_AUDIT_KEY_FILE 없음' }] })`, ex);
  assert.ok(kv.level === 'warn' && /감사 키 설정을 읽지 못함/.test(kv.text) && /변조 판정 아님/.test(kv.text) && !/인증 안 된 줄/.test(kv.text) && /b\.jsonl: 키 설정을 읽지 못함: FRAMETRACE_AUDIT_KEY_FILE/.test(kv.title), 'R10 U2: ' + JSON.stringify(kv));
  const kv2 = runInNewContext(`auditVerdict({ ok: false, error: 'x', logs: [{ name: 'c.jsonl', key_config_error: true, trust: '키 설정을 읽지 못함' }] })`, ex);
  assert.ok(kv2.level === 'warn' && /감사 키 설정을 읽지 못함/.test(kv2.text), 'R10 U2: never a failure verdict (P8): ' + JSON.stringify(kv2));
  assert.equal(runInNewContext(`auditVerdict({ ok: true, integrity: 'integrity-keyed', verified: 1, entries: 1, logs: [{ name: 'a', key_config_error: false }] }).level`, ex), 'ok');
  console.log('PASS VM R10 U2 key_config_error outranks every other audit state');

  // U4: "<package>_INCOMPLETE" (S2) reads 최종화 미완료 — status phase, project list, home list.
  const pi = runInNewContext(`[packageIncomplete('C:\\\\case\\\\reports\\\\package_001_INCOMPLETE'), packageIncomplete('/case/reports/package_001_INCOMPLETE/'), packageIncomplete('/case/reports/package_001'), packageIncomplete('', true), packageIncomplete(null)]`, ex);
  assert.deepEqual(plain(pi), [true, true, false, true, false], 'R10 U4 packageIncomplete');
  assert.equal(runInNewContext(`storePhase({ has_job: true, phase: 'done', package_dir: '/c/reports/package_002_INCOMPLETE' })`, ex), 'done', 'R10 U4: an incomplete package is not "finalized"');
  assert.equal(runInNewContext(`storePhase({ has_job: true, phase: 'done', package_dir: '/c/reports/package_002' })`, ex), 'finalized');
  // R11 U3: the recent-cases package branches were dead (no server hint) and are gone.
  const inc = runInNewContext(`[finIncompleteOf({ ok: false, error: '검토자 기록 확정 실패: x — 패키지는 이미 C:\\\\c\\\\reports\\\\package_001_INCOMPLETE에 만들어졌지만 최종화는 끝나지 않았습니다(최종화 미완료).' }), finIncompleteOf({ ok: false, error: 'x', incomplete_package_dir: 'D:/c/p_INCOMPLETE' }), finIncompleteOf({ ok: true, package_dir: '/c/p_001' }), finIncompleteOf({ ok: false, error: 'disk full' })]`, ex);
  assert.deepEqual(plain(inc), [{ package_dir: '' }, { package_dir: 'D:/c/p_INCOMPLETE' }, null, null], 'R10 U4 finalize answer: ' + JSON.stringify(inc));
  runInNewContext(`FTStore.set({ phase: 'done', project: Object.assign({}, FTStore.state.project, { caseDir: '/c/case', name: 'case', packageDir: '/c/reports/p_INCOMPLETE', packageIncomplete: true }) }); hProjectPopRender();`, ex);
  const cur1 = ex.document.getElementById('hProjectCur').innerHTML;
  assert.ok(/h-badge warn">최종화 미완료</.test(cur1), 'R10 U4 project switcher: ' + cur1);
  runInNewContext(`FTStore.set({ phase: 'finalized', project: Object.assign({}, FTStore.state.project, { packageDir: '/c/reports/p_001', packageIncomplete: false }) }); hProjectPopRender();`, ex);
  assert.ok(/h-badge ok">결과물 생성됨</.test(ex.document.getElementById('hProjectCur').innerHTML), 'R10 U4: a complete package still reads 결과물 생성됨');
  console.log('PASS VM R10 U4 "_INCOMPLETE" packages read 최종화 미완료 (status, project switcher, recent list, finalize answer)');
}

// ---- R9 track U (docs/PLAN_R9_2026-10-15.md) ------------------------------
async function r9TrackU({ fn, source, runInNewContext, assert, ex }) {
  const { readFileSync, readdirSync } = await import('node:fs');
  // U1 (track I P3 ↔ U): the status a record shows from its log entries.
  const tctx = {
    t: (k) => ({ 'status.unauth': '인증 안 됨', 'status.otherPc': '이 PC에서 인증 불가', 'status.tip.unauth': '인증 안 됨 — …', 'trust.sc': '자기 일관성만(인증 아님)' })[k] || k,
    tf: (k, s) => (k === 'trust.otherPc' ? `이 PC에서 인증 불가(변조 판정 아님) — 키 ${s.key} 보유 PC에서 확인` : k),
  };
  runInNewContext('function viewerTrustFromBundle() { return false; }\n' + ['logTrustOf', 'logTrustUsable', 'logTrustPcKey', 'logTrustSuperseded', 'untrustedStatusOf', 'logRepairs', 'logPosOf', 'logSlots', 'currentLogEntries', 'logTrustNote', 'recordStatusOf', 'statusTitle', 'statusLabel'].map((n) => {
    const src = fn(n);
    assert.ok(src, `R9 U1: ${n} exists (30-records.js)`);
    return src;
  }).join('\n'), tctx);
  const st = (own, valid, unvalid, fb) => JSON.parse(JSON.stringify(runInNewContext('recordStatusOf(own, valid, unvalid, fb)', Object.assign(tctx, { own, valid, unvalid, fb }))));
  const ok = { validation_status: 'ffprobe-video-stream-confirmed' };
  assert.deepEqual(st(null, { ...ok, trust: 'auth' }, null, 'x'), { status: 'ffprobe-video-stream-confirmed', trust: 'auth' });
  assert.deepEqual(st(null, { ...ok, trust: 'sc' }, null, 'x'), { status: 'ffprobe-video-stream-confirmed', trust: 'sc' });
  assert.deepEqual(st(null, ok, null, 'x'), { status: 'ffprobe-video-stream-confirmed', trust: '' }, 'legacy (no trust) as before');
  assert.equal(st(null, null, { ...ok, trust: 'unauth' }, 'ffprobe-confirmed').status, 'unauthenticated', 'an unauthenticated validation never confirms (not even the index fallback)');
  assert.deepEqual(st(null, null, { ...ok, trust: 'pc:k1' }, 'x'), { status: 'auth-other-pc', trust: 'pc:k1' });
  assert.equal(st({ trust: 'unauth' }, null, null, 'derived').status, 'unauthenticated', 'an unauthenticated join line is never "derived"');
  assert.equal(st({ trust: 'pc:k2' }, { ...ok, trust: 'auth' }, null, 'x').status, 'auth-other-pc', 'a record whose own line is not authenticated stays so, whatever validates its path');
  assert.equal(st({ trust: 'weird' }, null, null, 'derived').status, 'unauthenticated', 'an unknown trust word fails closed');
  const cur = runInNewContext('currentLogEntries(list, (i) => i.id)', Object.assign(tctx, { list: [{ id: 'a', v: 1, trust: 'auth' }, { id: 'b', v: 1, trust: 'unauth' }, { id: 'a', v: 2, trust: 'unauth' }, { id: 'a', v: 3 }, { id: 'b', v: 2, trust: 'pc:x' }] }));
  assert.deepEqual(JSON.parse(JSON.stringify(cur.map((i) => `${i.id}${i.v}`))), ['a3', 'b2'], 'R9 U1: per key the latest usable entry; an unauthenticated one only when nothing usable exists');
  assert.ok(/키 k9 보유 PC/.test(runInNewContext('statusTitle({ status: "auth-other-pc", statusTrust: "pc:k9" })', tctx)) && /자기 일관성만/.test(runInNewContext('statusTitle({ status: "ffprobe-confirmed", statusTrust: "sc" })', tctx)), 'R9 U1: badge tooltips');
  assert.equal(runInNewContext('statusLabel("unauthenticated")', tctx), '인증 안 됨');
  console.log('PASS VM R9 U1 log trust: unauth / other-PC lines never confirm, verify or derive; sc keeps the value; legacy as before');

  // U8: the pop-out channel without a case key is the page's own key, never "ft-player:case".
  const hostSrc = readFileSync(new URL('../assets/viewer/00-host.js', import.meta.url), 'utf8');
  const hctx = { EventTarget, AbortController, CustomEvent: class extends Event { constructor(t, o) { super(t); this.detail = o && o.detail; } }, crypto: globalThis.crypto, Uint32Array, location: { origin: 'http://127.0.0.1:1', pathname: '/viewer/' }, document: { documentElement: { hasAttribute: () => true, dataset: {} } }, localStorage: { setItem() {} }, setInterval, clearInterval };
  hctx.window = hctx;
  runInNewContext(hostSrc, hctx);
  const chan = runInNewContext('FTViewer.channels.player("")', hctx);
  assert.ok(chan !== 'ft-player:case' && /^ft-player:nocase-[0-9a-f]{16}$/.test(chan) && runInNewContext('FTViewer.channels.player("C1")', hctx) === 'ft-player:C1', 'R9 U8: channels.player() without a key: ' + chan);
  console.log('PASS VM R9 U8 channels.player uses caseStorageKey, never "case"');

  // U2: the shell's close flush names the read-only cause (case.json vs the marks).
  ex.__api = { save: async () => true, summary: () => ({ pending: 0, lockCause: 'case' }), saveState: () => 'readonly' };
  ex.__toasts = [];
  runInNewContext('hViewerApi = () => __api; FTStore.state.viewer.mounted = true; toast = (m, k) => { __toasts.push([m, k]); };', ex);
  const c1 = await runInNewContext('hBeforeClose(1000)', ex);
  assert.ok(c1.ok && /case\.json/.test(c1.warning) && !/판독 기록/.test(c1.warning), 'R9 U2: read-only by case.json → its own warning: ' + JSON.stringify(c1));
  ex.__api = { save: async () => false, summary: () => ({ pending: 0, lockCause: 'case' }), saveState: () => 'readonly' };
  const c2 = await runInNewContext('hBeforeClose(1000)', ex);
  assert.ok(!c2.ok && /case\.json/.test(c2.message), 'R9 U2: blocked close names case.json: ' + JSON.stringify(c2));
  ex.__api = { save: async () => true, summary: () => ({ pending: 0, lockCause: 'annotations' }), saveState: () => 'readonly' };
  const c3 = await runInNewContext('hBeforeClose(1000)', ex);
  assert.ok(c3.ok && /판독 기록/.test(c3.warning), 'R9 U2: the marks cause keeps its text');
  console.log('PASS VM R9 U2 close flush: read-only cause in its own words');

  // U6: a patient save that ran out of time while its request is still on
  // the wire is "아직 저장 중", not "저장하지 못했습니다" (finalize retry,
  // examiner attribution, project switch).
  const stillApi = { save: async (o) => { o.timedOut = true; return false; }, saveState: () => 'saving', summary: () => ({ pending: 1 }), init() {} };
  const failApi = { save: async () => false, saveState: () => 'failed', summary: () => ({ pending: 1 }), init() {} };
  runInNewContext('finRefresh = () => {};', ex);
  for (const [api, want, not] of [[stillApi, /아직 저장 중/, /저장하지 못했습니다/], [failApi, /저장하지 못했습니다/, /아직 저장 중/]]) {
    ex.window.FTViewer = api;
    ex.document.getElementById('finalHint').textContent = '';
    await runInNewContext('finRetrySave()', ex);
    const hint = ex.document.getElementById('finalHint').textContent;
    assert.ok(want.test(hint) && !not.test(hint), 'R9 U6 finalize retry: ' + hint);
  }
  ex.window.FTViewer = stillApi;
  ex.document.getElementById('finalExaminer').value = 'Kim';
  runInNewContext('FIN.attributing = false; FIN.busy = false; finViewerSaveState = () => "pending"; finPaint = () => {};', ex);
  await runInNewContext('finAttribute()', ex);
  assert.ok(/아직 저장 중/.test(ex.document.getElementById('finalHint').textContent) || ex.__toasts.some(([m]) => /아직 저장 중/.test(m)), 'R9 U6 attribution says still saving: ' + JSON.stringify(ex.__toasts.slice(-2)));
  ex.document.documentElement = { hasAttribute: (a) => a === 'data-ft-host' };
  ex.FTViewer = stillApi;
  runInNewContext('hostViewer.mounted = true;', ex);
  const sw = await runInNewContext('hostViewerSwitch(async () => true)', ex);
  assert.ok(sw === false && runInNewContext('hostViewer.stillSaving', ex) === true, 'R9 U6 project switch: refused, and known to be still saving');
  ex.window.FTViewer = failApi;
  ex.FTViewer = failApi;
  const sw2 = await runInNewContext('hostViewerSwitch(async () => true)', ex);
  assert.ok(sw2 === false && runInNewContext('hostViewer.stillSaving', ex) === false, 'R9 U6 project switch: a failed save is not "still saving"');
  runInNewContext('hostViewer.mounted = false; FTStore.state.viewer.mounted = false;', ex);
  delete ex.document.documentElement;
  delete ex.FTViewer;
  delete ex.window.FTViewer;
  console.log('PASS VM R9 U6 patient save timed out while still on the wire reads "아직 저장 중"');

  // U9: harness saves that wait for a real save are explicit ({ patient: true });
  // a no-budget FTViewer.save() (the shell fallback's 4 s) only where that
  // budget is what is tested, marked /* no-budget: … */.
  const scriptsDir = new URL('./', import.meta.url);
  const bare = [];
  for (const f of readdirSync(scriptsDir).filter((n) => n.endsWith('.mjs'))) {
    readFileSync(new URL(f, scriptsDir), 'utf8').split(/\r?\n/).forEach((line, i) => {
      if (/FTViewer\.save\(\)/.test(line.replace(/\/\/.*$/, '')) && !/^\s*\/\//.test(line)) bare.push(`${f}:${i + 1}`);
    });
  }
  assert.deepEqual(bare, [], 'R9 U9: a harness save with no budget (the 4 s hard close budget) — use { patient: true } or mark /* no-budget */: ' + bare.join(', '));
  console.log('PASS R9 U9 harness saves are explicit (patient) unless the no-budget path is under test');
}

// ---- R8 track U (docs/PLAN_R8_2026-10-15.md) ------------------------------
async function r8TrackU({ fn, source, runInNewContext, assert }) {
  // U5 + U8: the incremental mark tally (30-records.js markTallyNow) against
  // a brute-force recount after every few steps of seeded random runs:
  // marks with a status, an EMPTY / missing status (U5: R7 bucketed those
  // with the "id not in this case" marker, so they fell out of `n`), ids
  // this case does not list, clears, undo / redo of earlier steps, a
  // conflict resolved to the server's value, hydration (a new mark map),
  // bulk runs past the 4096-id touch cap, records appearing (live index).
  const tallySrc = [
    'var markTally = null, markTallyTouched = new Set(), markTallyAll = true, markTallyFullCount = 0;',
    (source.match(/^var MARK_TALLY_OFFCASE = .*$/m) || [''])[0],
    ...['markTallyNote', 'markTallyBucket', 'markTallyMove', 'markTallyNow'].map(fn),
    'function bump(id) { markTallyNote(id); }',
  ].join('\n');
  const STATUSES = ['reviewed', 'important', 'needs_verification', '', undefined];
  for (const seed of [1, 7, 42, 1234, 99991]) {
    let x = seed >>> 0;
    const rnd = (n) => { x = (Math.imul(x ^ (x >>> 15), 2246822507) + 0x9e3779b9) >>> 0; x ^= x >>> 13; return x % n; };
    const recordById = new Map();
    for (let i = 0; i < 300; i++) recordById.set('r' + i, { id: 'r' + i });
    const vm = { state: { marks: {} }, recordById };
    runInNewContext(tallySrc, vm);
    const idOf = () => (rnd(10) === 0 ? 'x' + rnd(40) : 'r' + rnd(360)); // x… and r300+ are not (yet) records
    const server = {};
    const undo = [], redo = [];
    const setMark = (id, value, journal) => {
      const before = vm.state.marks[id] ? { ...vm.state.marks[id] } : null;
      if (value) vm.state.marks[id] = value; else delete vm.state.marks[id];
      if (journal) journal.push({ id, before });
      runInNewContext(`bump(${JSON.stringify(id)})`, vm);
    };
    const brute = () => {
      const out = { all: 0, n: 0, byStatus: {} };
      for (const [id, m] of Object.entries(vm.state.marks)) {
        if (!m) continue;
        out.all += 1;
        if (!recordById.has(id)) continue;
        out.n += 1;
        const s = String(m.status || '');
        out.byStatus[s] = (out.byStatus[s] || 0) + 1;
      }
      return out;
    };
    const ops = { mark: 0, clear: 0, undo: 0, redo: 0, conflict: 0, hydrate: 0, bulk: 0, grow: 0, empty: 0 };
    for (let step = 0; step < 1500; step++) {
      const r = rnd(100);
      if (r < 40) {
        const status = STATUSES[rnd(STATUSES.length)];
        if (!status) ops.empty += 1;
        setMark(idOf(), status === undefined ? { marked_unix: 1 } : { status, marked_unix: 1 }, undo);
        redo.length = 0;
        ops.mark += 1;
      } else if (r < 52) {
        setMark(idOf(), null, undo); redo.length = 0; ops.clear += 1;
      } else if (r < 64 && undo.length) {
        const e = undo.pop();
        setMark(e.id, e.before, redo); ops.undo += 1;
      } else if (r < 72 && redo.length) {
        const e = redo.pop();
        setMark(e.id, e.before, undo); ops.redo += 1;
      } else if (r < 82) {
        // A conflict resolved to the server's value (or a server value applied).
        const id = idOf();
        server[id] = rnd(3) ? { status: STATUSES[rnd(STATUSES.length)] || '', marked_unix: 2 } : null;
        setMark(id, server[id], null); ops.conflict += 1;
      } else if (r < 86) {
        // Hydration: a new map, bump without an id.
        const next = {};
        for (const [id, m] of Object.entries(server)) if (m) next[id] = { ...m };
        for (let i = 0; i < 20; i++) next[idOf()] = { status: STATUSES[rnd(STATUSES.length)], marked_unix: 3 };
        vm.state.marks = next;
        runInNewContext('bump()', vm);
        undo.length = 0; redo.length = 0; ops.hydrate += 1;
      } else if (r < 88) {
        // Bulk: more touched ids than the touch set keeps (4096) before a read.
        for (let i = 0; i < 4200; i++) {
          const id = 'r' + rnd(360);
          const st = STATUSES[rnd(STATUSES.length)];
          if (rnd(5) === 0) delete vm.state.marks[id]; else vm.state.marks[id] = { status: st, marked_unix: 4 };
          runInNewContext(`bump(${JSON.stringify(id)})`, vm);
        }
        undo.length = 0; redo.length = 0; ops.bulk += 1;
      } else if (r < 90) {
        // A growing live index lists ids that were marked before.
        const n = recordById.size;
        for (let i = n; i < n + 5; i++) recordById.set('r' + i, { id: 'r' + i });
        ops.grow += 1;
      }
      if (rnd(3) === 0 || step === 1499) {
        const got = runInNewContext('(() => { const t = markTallyNow(); return { all: t.all, n: t.n, byStatus: { ...t.byStatus } }; })()', vm);
        const want = brute();
        const clean = (o) => Object.fromEntries(Object.entries(o).filter(([, v]) => v));
        assert.deepEqual({ all: got.all, n: got.n, byStatus: clean(got.byStatus) }, { all: want.all, n: want.n, byStatus: clean(want.byStatus) },
          `R8 U8: incremental tally = recount (seed ${seed}, step ${step}, ops ${JSON.stringify(ops)})`);
      }
    }
    assert.ok(ops.empty > 0 && ops.undo > 0 && ops.redo > 0 && ops.hydrate > 0 && ops.bulk > 0 && ops.grow > 0, 'R8 U8: the run exercised every kind of step: ' + JSON.stringify(ops));
  }
  console.log('PASS VM R8 U5/U8 mark tally = recount over 5 seeded random runs (empty status, off-case ids, undo/redo, conflicts, hydrate, bulk, growth)');

  // U2: an explicit save ({ patient: true }) keeps waiting while its request
  // progresses (chunks answered) and times out only on a genuine stall; a
  // hard budget ({ timeoutMs }) is not extended; save() with no budget is
  // the shell fallback's hard <= 4 s.
  const saveSrc = fn('viewerSave');
  assert.ok(saveSrc, 'R8 U2: viewerSave (90-sync.js) holds the save budgets');
  const defaultMs = Number(((source.match(/^const SAVE_DEFAULT_MS = (\d+);/m) || [])[1]));
  assert.ok(defaultMs > 0 && defaultMs <= 4000, 'R8 U2: the no-budget save() is a hard <= 4 s (the shell fallback path): ' + defaultMs);
  const mkSave = () => {
    const vm = {
      setTimeout, clearTimeout,
      viewerIsServed: true, annotationsReadOnly: false, ftSignal: { aborted: false },
      state: { drafts: {}, dirtyIds: new Set(['a']) }, conflictIds: new Map(), pendingReviewSync: false,
      conflictLookupFailed: false, checkServerConflicts: async () => true,
      saveInFlight: false, saveProgressAt: 0, waiters: [],
      SAVE_BUSY_RETRIES: 10, SAVE_CALLER_WAIT_MS: 20000, SAVE_STALL_MS: 30000, SAVE_DEFAULT_MS: defaultMs,
    };
    runInNewContext(`
      function localReviewIdle() { return !state.dirtyIds.size && !saveInFlight; }
      function whenSaveSettled() { return saveInFlight ? new Promise(r => waiters.push(r)) : Promise.resolve(true); }
      async function whenConflictsChecked() { return true; }
      function settle() { saveInFlight = false; const w = waiters; waiters = []; w.forEach(r => r(true)); }
      // n chunk replies, ms apart (each one a sign of life); n = 0: never answers.
      function syncWith(n, ms) {
        syncMarksNow = () => new Promise(resolve => {
          saveInFlight = true; saveProgressAt = Date.now();
          if (!n) return;
          let left = n;
          const next = () => setTimeout(() => {
            saveProgressAt = Date.now();
            if (--left > 0) return next();
            state.dirtyIds.clear(); settle(); resolve({ ok: true });
          }, ms);
          next();
        });
      }
    ` + saveSrc, vm);
    return vm;
  };
  const run = async (vm, expr) => { const t0 = Date.now(); const out = await runInNewContext(expr, vm); return { out, ms: Date.now() - t0 }; };
  const call = (opts) => `(async () => { const o = ${opts}; const r = await viewerSave(o); return { r, timedOut: !!o.timedOut }; })()`;
  // Progressing: 4 chunks x 350 ms (1.4 s) against a 300 ms budget, 700 ms stall window.
  let vm = mkSave();
  runInNewContext('syncWith(4, 350)', vm);
  const prog = await run(vm, call('{ patient: true, timeoutMs: 300, stallMs: 700 }'));
  assert.ok(prog.out.r === true && !prog.out.timedOut && prog.ms >= 1300, 'R8 U2: a patient save waits while its chunks keep answering (1.4 s past a 300 ms budget) and lands: ' + JSON.stringify(prog));
  // The same progress under a HARD budget: not extended.
  vm = mkSave();
  runInNewContext('syncWith(4, 350)', vm);
  const hard = await run(vm, call('{ timeoutMs: 300 }'));
  assert.ok(hard.out.r === false && hard.out.timedOut && hard.ms < 650, 'R8 U2: save({ timeoutMs }) stays a hard total (close flush): ' + JSON.stringify(hard));
  // A request already on the wire (another caller's) that keeps answering chunks.
  vm = mkSave();
  runInNewContext('syncWith(5, 300); syncMarksNow();', vm);
  const wire = await run(vm, call('{ patient: true, timeoutMs: 200, stallMs: 600 }'));
  assert.ok(wire.out.r === true && wire.ms >= 1300, 'R8 U2: a patient save waits out a progressing save already in flight (not abandoned at its budget): ' + JSON.stringify(wire));
  // A genuine stall: sent, never answered -> false once stallMs passed without a sign of life.
  vm = mkSave();
  runInNewContext('syncWith(0, 0)', vm);
  const stall = await run(vm, call('{ patient: true, timeoutMs: 200, stallMs: 600 }'));
  assert.ok(stall.out.r === false && stall.out.timedOut && stall.ms >= 550 && stall.ms < 1100, 'R8 U2: a stalled request (no reply for stallMs) times out: ' + JSON.stringify(stall));
  // save() with no budget: the shell fallback path — hard, <= 4 s, even while a request hangs.
  vm = mkSave();
  runInNewContext('syncWith(0, 0)', vm);
  const dflt = await run(vm, 'viewerSave()');
  assert.ok(dflt.out === false && dflt.ms >= defaultMs - 100 && dflt.ms <= 4100, 'R8 U2: save() with no budget answers within 4 s while a request hangs: ' + JSON.stringify(dflt));
  console.log('PASS VM R8 U2 patient save follows progress, stalls time out, hard budgets stay hard, save() <= 4 s');

  // Track I (R8 P1/P2) <-> U: a level measured on a keyless PC (`sc: true`)
  // is shown but says "자기 일관성만(인증 아님)"; an unverified line signed by
  // another PC's key (`pc`) stays 미확인 and names that key.
  // (R9 U3: another PC's key reads "이 PC에서 인증 불가(변조 판정 아님) — 키 <id> …", never "검증 실패".)
  const i18nKo = { 'audio.selfConsistent': '자기 일관성만(인증 아님)', 'audio.otherPc': '키 {key} 보유 PC에서 확인', 'trust.otherPc': '이 PC에서 인증 불가(변조 판정 아님) — 키 {key} 보유 PC에서 확인', 'audio.tip.trackOtherPc': '오디오 트랙 있음 · 소리 미확인', 'audio.detail.trackOtherPc': '오디오 트랙 있음{codec} — 소리 미확인', 'audio.tip.trackUnverified': '오디오 트랙 있음 · 측정 기록 검증 실패 — 미확인' };
  const au = {
    audioLevels: {
      s1: { r: 'sound', max: -12, mean: -30, sc: true }, s2: { r: 'sound', max: -12, mean: -30 },
      u1: { r: 'unverified', pc: 'pc-b' }, u2: { r: 'unverified' },
    },
    audioLevelsError: '',
    t: (k) => i18nKo[k] || k,
    tf: (k, subs) => Object.entries(subs || {}).reduce((x, [n, v]) => x.split('{' + n + '}').join(String(v)), i18nKo[k] || k),
  };
  runInNewContext(['audioInfoFor', 'fmtDb', 'audioTrustNote', ...(fn('audioReasonNote') ? ['audioReasonNote'] : []), 'audioTipFor', 'audioTipBase', 'audioDetailFor', 'audioDetailBase'].map(fn).join('\n') +
    '\nthis.view = (id) => { const r = { id, audioCodec: "aac", probeOk: true }; const a = audioInfoFor(r); Object.assign(r, { audioState: a.state, audioLevel: a.level, audioReason: a.reason, audioTrust: a.trust || "", audioPcKey: a.pcKey || "" }); return { state: a.state, tip: audioTipFor(r), detail: audioDetailFor(r) }; };', au);
  const v = (id) => runInNewContext(`view(${JSON.stringify(id)})`, au);
  const s1 = v('s1'), s2 = v('s2'), u1 = v('u1'), u2 = v('u2');
  assert.ok(s1.state === 'sound' && /자기 일관성만\(인증 아님\)/.test(s1.tip) && /자기 일관성만\(인증 아님\)/.test(s1.detail), 'R8: sc level shown with the self-consistency note: ' + JSON.stringify(s1));
  assert.ok(s2.state === 'sound' && !/자기 일관성/.test(s2.tip + s2.detail), 'R8: an authenticated level has no note: ' + JSON.stringify(s2));
  assert.ok(u1.state === 'track' && /키 pc-b 보유 PC에서 확인/.test(u1.tip) && /키 pc-b 보유 PC에서 확인/.test(u1.detail), 'R8: another PC\'s line stays 미확인 and names its key: ' + JSON.stringify(u1));
  assert.ok(u2.state === 'track' && !/보유 PC/.test(u2.tip), 'R8: a plain unverified line names no key: ' + JSON.stringify(u2));
  assert.ok(!/검증 실패/.test(u1.tip + u1.detail) && /이 PC에서 인증 불가\(변조 판정 아님\)/.test(u1.tip) && /이 PC에서 인증 불가\(변조 판정 아님\)/.test(u1.detail), 'R9 U3: another PC\'s line is "이 PC에서 인증 불가(변조 판정 아님)", never "검증 실패": ' + JSON.stringify(u1));
  au.audioLevels.n1 = { r: 'unverified', pc: 'pc-n' };
  const n1 = runInNewContext('(() => { const r = { id: "n1", audioCodec: "", probeOk: false }; const a = audioInfoFor(r); Object.assign(r, { audioState: a.state, audioLevel: a.level, audioReason: a.reason, audioTrust: a.trust || "", audioPcKey: a.pcKey || "" }); return { state: a.state, tip: audioTipFor(r) }; })()', au);
  assert.ok(n1.state === 'unknown' && /키 pc-n 보유 PC에서 확인/.test(n1.tip), 'R9 U3: shown for the unknown state too: ' + JSON.stringify(n1));
  console.log('PASS VM R8 (I <-> U) audio levels: sc = self-consistency only, pc = check on the PC holding that key');
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
  // (R7 U3: the source-regex check that the queue load shows them is gone —
  // verify-range-sync ⑱ shows the notice in a real page and its drain.)
  console.log('PASS VM R6 U1/U2 string cursor across a file boundary, unreadable queue files kept');
  // R7 U9: entries without a file name are not ONE file: two nameless
  // entries (repeated on both pages) count two, a named one counts once.
  const nl = { get: async (url) => {
    const q = new URL(url, 'http://x').searchParams;
    const second = q.has('after');
    return { ok: true, count: 2, next_after: second ? null : 'c1', entries: [{ id: second ? 'b' : 'a', cursor: second ? 'c2' : 'c1' }], unreadable: [{ error: 'EOF' }, { file: '', error: 'utf-8' }, { file: '0009.json', error: 'x' }] };
  } };
  const gotNl = await runInNewContext(fn('readPagedList') + '\nCONFLICT_PAGE=500;CONFLICT_PAGES_MAX=2000;\nreadPagedList(get, "/api/marks-pending", d => d.entries);', nl);
  assert.ok(Array.isArray(gotNl.unreadable) && gotNl.unreadable.length === 3, 'R7 U9: two nameless unreadable entries are two files (+1 named), not merged into one: ' + JSON.stringify(gotNl.unreadable));
  console.log('PASS VM R7 U9 nameless unreadable queue entries are counted apart');

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

  // R7 U7: the examiner confirms, but meanwhile a job started or the
  // server's raw decision changed — re-read /api/status, cancel (said so),
  // never POST the delete on a stale decision. Unchanged → it goes.
  const waiting = { raw_path: 'C:/case/raw/e.raw', reason: 'marker mismatch', endpoint: '/api/reimport-raw' };
  ex.__toasts7 = [];
  ex.__status = null;
  runInNewContext('toast = (m, k) => { __toasts7.push([m, k]); }; jget = async (u) => { if (u !== "/api/status") throw new Error(u); return __status; };', ex);
  const reimport7 = async (status) => {
    ex.__calls = [];
    ex.__toasts7.length = 0;
    ex.__status = status;
    ex.__waiting = waiting;
    runInNewContext('hStatusBusy = false; reimportRawBusy = false; hRawReimport = __waiting;', ex);
    await runInNewContext('hReimportRaw(null)', ex);
    return { posts: ex.__calls.length, toasts: ex.__toasts7.map(([m, k]) => k + ':' + m) };
  };
  const busy7 = await reimport7({ busy: true, raw_reimport: waiting });
  assert.ok(busy7.posts === 0 && busy7.toasts.some((t) => /^warn:.*작업이 시작되어/.test(t)), 'R7 U7: a job started while the dialog was open → no POST, said so: ' + JSON.stringify(busy7));
  const moved7 = await reimport7({ busy: false, raw_reimport: { ...waiting, raw_path: 'C:/case/raw/other.raw' } });
  assert.ok(moved7.posts === 0 && moved7.toasts.some((t) => /^warn:.*상태가 바뀌어/.test(t)), 'R7 U7: the waiting raw changed → no POST: ' + JSON.stringify(moved7));
  const gone7 = await reimport7({ busy: false, raw_reimport: null });
  assert.ok(gone7.posts === 0 && gone7.toasts.some((t) => /^warn:.*상태가 바뀌어/.test(t)), 'R7 U7: no raw waits any more → no POST: ' + JSON.stringify(gone7));
  const ok7 = await reimport7({ busy: false, raw_reimport: { ...waiting } });
  assert.ok(ok7.posts === 1 && ex.__calls[0][0] === '/api/reimport-raw' && ex.__calls[0][1].raw_path === waiting.raw_path, 'R7 U7: unchanged → exactly one POST of the confirmed raw: ' + JSON.stringify(ok7));
  runInNewContext('hRawReimport = null; hStatusBusy = false;', ex);
  console.log('PASS VM R7 U7 reimport re-checks busy / raw_reimport after the confirmation');

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
  // (R7 U3: the source-regex checks that stood for behaviour are gone — the
  // standalone hBeforeClose (verify-range-sync ⑨/㉒), the per-caller save
  // budget (⑳) and the independent base -1 body (㉑) run in a real page.)
  console.log('PASS VM R6 U4 hBeforeClose: prompt (4 s, budget passed down), read-only warning returned and shown');

  // R7 U1 (S7 ↔ U): hBeforeClose(timeoutMs) is a HARD TOTAL from the call —
  // a viewer save that never answers (a request left on the wire) gets
  // the budget minus a margin, and the backstop answers BEFORE the budget
  // is up (the shell's 4 s call leaves its own 5 s wait room); a save that
  // reports its own deadline (opts.timedOut) is said as a timeout.
  const realTimers = { setTimeout: ex.setTimeout, clearTimeout: ex.clearTimeout };
  ex.setTimeout = setTimeout;
  ex.clearTimeout = clearTimeout;
  try {
    const askedU1 = [];
    ex.__api = { save: (opts) => { askedU1.push({ ...opts }); return new Promise(() => {}); }, summary: () => ({ pending: 2 }), saveState: () => 'saving' };
    runInNewContext('FTStore.state.viewer.mounted = true;', ex);
    let t0 = Date.now();
    const hung = await runInNewContext('hBeforeClose(1000)', ex);
    const hungMs = Date.now() - t0;
    assert.ok(hungMs < 1000 && hung.ok === false && /^저장 안 된 편집 2건 \(저장 응답 대기 시간 초과\)$/.test(hung.message), `R7 U1: a hanging save → answered within the 1000 ms total (${hungMs} ms), Korean timeout message: ` + JSON.stringify(hung));
    assert.ok(askedU1.length === 1 && askedU1[0].timeoutMs > 0 && askedU1[0].timeoutMs < 1000, 'R7 U1: the viewer save gets less than the total: ' + JSON.stringify(askedU1));
    ex.__api = { save: async (opts) => { opts.timedOut = true; return false; }, summary: () => ({ pending: 0 }), saveState: () => 'saving' };
    t0 = Date.now();
    const own = await runInNewContext('hBeforeClose(1000)', ex);
    assert.ok(Date.now() - t0 < 500 && own.ok === false && /시간 초과/.test(own.message), 'R7 U1: the viewer\'s own deadline (opts.timedOut) reads as a timeout: ' + JSON.stringify(own));
  } finally {
    ex.setTimeout = realTimers.setTimeout;
    ex.clearTimeout = realTimers.clearTimeout;
    runInNewContext('FTStore.state.viewer.mounted = false;', ex);
  }
  console.log('PASS VM R7 U1 hBeforeClose hard total budget (hanging save, viewer deadline)');

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
  const ops = [fn('annotationsLocked'), fn('lockKey'), fn('refuseLockedEdit'), fn('targetIds'), fn('touchAnnotation'), fn('applyMark'), fn('applyTag'), fn('clearTags'), fn('addTag'), fn('removeTag')].join('\n');
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
  // (R7 U3: the source-regex stand-in for FTViewer.save()'s read-only rule
  // is gone — verify-range-sync ⑨ checks it in a real page.)
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
  // (R7 U7: after the confirmation /api/status is read again — unchanged here.)
  runInNewContext('hRawReimport = { raw_path: "C:/case/raw/e.raw", reason: "marker mismatch", endpoint: "/api/reimport-raw" }; askDialog = async () => __answer; jpost = async (u, b) => { __calls.push([u, b]); return { ok: true }; }; poll = () => {}; jget = async () => ({ busy: false, raw_reimport: { raw_path: "C:/case/raw/e.raw", reason: "marker mismatch", endpoint: "/api/reimport-raw" } });', ex);
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
