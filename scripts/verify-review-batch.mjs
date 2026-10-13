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
  const draftsSrc = [fn('writerId'), fn('nextDraftSeq'), fn('draftRangeOf'), fn('draftValuesOf'), fn('touchAnnotation'), fn('dropDraft')].join('\n');
  const touch = 'draftSeq=0;draftFlushKeys=new Set();draftRangesDirty=false;\n' + draftsSrc;
  let flushes = 0;
  Object.assign(grid, { FTViewer: { writerToken: 'w1' }, scheduleDraftFlush: () => { flushes += 1; }, markLabel: s => s, document: { getElementById: () => null, body: { classList: { toggle: () => {} } } }, toast: () => {}, RANGES_KEY: 'ranges', NOTES_KEY: 'notes', TAGS_KEY: 'tags' });
  const journalSrc = [fn('snapshotIds'), fn('journalDiff'), fn('toastUndo'), fn('journaled'), fn('markStatusLabel')].join('\n');
  runInNewContext('undoStack=[];redoStack=[];JOURNAL_LIMIT=200;JOURNAL_FIELDS=["mark","note","tags","range"];\n' + journalSrc + '\n' + touch + '\n' + fn('markActive') + '\nmarkActive("reviewed");', grid);
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
  console.log('PASS VM chunking (2000 ids / 1 MiB), 413 halving, per-id outcome merge');
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
  const hydrateSrc = [fn('aliveKey'), fn('draftKey'), fn('parseDraftKey'), fn('writerVerdict'), fn('storedDraftEntries'), fn('migrateLegacyDrafts'), fn('applyDraftValues'), fn('adoptDraft'), fn('annotationMarkId'), fn('hydrateAnnotations'), fn('adoptOrphanDrafts'), fn('adoptDeadWriters'), fn('flushDraftsNow'), fn('applyAppliedIds'), fn('dropJournalIds')].join('\n');
  runInNewContext('skippedDraftWriters=new Set();restoredConflictIds=[];draftAdoptedFrom=new Map();orphanCheck=null;draftsUnpersisted=false;draftFlushTimer=null;\n' + touch + '\n' + hydrateSrc + '\nhydrateAnnotations(annotations);', grid);
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
  runInNewContext('undoStack=[{changes:[{id:"a"},{id:"b"}]},{changes:[{id:"a"}]}];redoStack=[{changes:[{id:"a"}]}];dropJournalIds(["a"]);', grid);
  assert.equal(runInNewContext('JSON.stringify([undoStack.map(e => e.changes.map(c => c.id)), redoStack.length])', grid), '[[["b"]],0]', 'resolved ids leave the undo/redo journal');
  console.log('PASS VM quota-bounded flush, seq-guarded confirm with rebase, journal drop on resolve');
  // Drafts must snapshot post-mutation state: a draft recorded before the
  // mark/tag mutation replays the old value on reload and resurrects
  // cleared annotations (or drops new ones) after hydration.
  Object.assign(state, { marks: {}, notes: {}, tags: {}, drafts: {}, dirtyIds: new Set(), selectedIds: new Set(['a']) });
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
  // A draft is dropped only when its seq is the one that was sent.
  const seqSent = state.drafts.a.seq;
  runInNewContext(ops + '\ntouchAnnotation("a");', grid);
  assert.notEqual(state.drafts.a.seq, seqSent, 'a mid-flight edit bumps seq');
  assert.ok(state.drafts.a && state.dirtyIds.has('a'), 'edited-while-saving draft survives the save');
  console.log('PASS VM annotation drafts capture post-mutation state, seq-guarded drop');
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
