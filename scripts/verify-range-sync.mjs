// IN/OUT ranges as first-class annotations of save protocol v2 (FOR-01,
// docs/PLAN_R4_2026-10-14.md A1/A2).
//
//   node scripts/verify-range-sync.mjs
//   FT_ASSETS_ROOT=<other checkout> node scripts/verify-range-sync.mjs   (run against other assets)
//
// Builds a standalone viewer page from assets/ (same bundling as build.rs)
// and serves it from a mock case server that implements the patch-v2 rules
// of src/case_db/marks.rs: per-id revisions, a stale base is a conflict
// (unless the patch changes nothing), `deleted_ids` removes the mark row
// (status, note AND range), an absent range keeps the stored one and
// {in:null,out:null} clears it. Every step awaits a DOM / save-state
// condition — no fixed sleeps.
//
//  ① i/o → the range rides in the payload, lands on the server, "saved"
//  ② the old whole-map RANGES_KEY equal to the server: dropped, no conflict
//  ③ RANGES_KEY different from the server → an ordinary conflict (the
//    conflict list, tagged as this browser's earlier range), "pending";
//    i/o, a tag and a cleared mark meanwhile send NOTHING for that id and
//    never show "saved"; the server keeps its range, tags, mark
//  ④ [서버 값 사용] → the server's range shown, saved, RANGES_KEY gone
//  ⑤ [내 값으로 덮어쓰기] → the browser's range reaches the server
//  ⑥ a fresh browser: the server's ranges show (hydrated like marks); a tag
//    and a cleared mark keep the stored range (explicit range, no
//    deleted_ids for a record that still has one)
//  ⑦ a range another window deleted does not come back from an old
//    RANGES_KEY: it is a conflict, the server stays empty until chosen
//  ⑧ standalone file:// page: an old package's RANGES_KEY never wins over
//    the new bundle — the bundle's range shows, the leftovers are only
//    reported (#legacyRangesNotice) and kept until discarded
// Save-state recovery (same mock):
//  ⑨ A3 records-meta `annotations.error` → read-only banner, state
//    "readonly", an edit sends nothing, summary / CSV disabled
//  ⑩ A4 a reload while the server queue holds marks: the queued values
//    (GET /api/marks-pending, R4 B6) show, state "queued", leaving warns;
//    once the queue drains the indicator turns "saved". A server without
//    the route still shows "queued" (never "saved")
//  ⑪ A5 the queue conflicts cannot be read (both routes fail) → "unknown",
//    never "saved"; the lookup retries and, once readable, reads EVERY page
//    of GET /api/mark-conflicts?offset&limit (R4 B3)
//  ⑫ A9 attributeUnassignedMarks resolves only after its save landed, and
//    rejects (no success to report) when the save met a conflict
// R6 track U (docs/PLAN_R6_2026-10-15.md):
//  ⑱ U1/U2 marks-pending with the S2 string cursor "<seq>:<idx>": one queue
//    file of 800 ids is read across the page boundary (?after=7%3A499), and
//    the server's `unreadable` files are shown ("대기 파일 N개 읽기 실패");
//    an older numeric-cursor server that skips inside a file → "truncated"
//  ⑲ U10 a conflict list the server cut short (`truncated`) is said so
//  ⑳ U5 FTViewer.save() waits out "busy, retry" only for the caller's
//    budget and shows the retries (R7 U5: default 20 s for explicit saves)
//  ㉑ U9 a failing base -1 body does not keep the ordinary edits from going out
//  ⑨ U4 the standalone page answers window.hBeforeClose() at once with the
//    read-only warning (the shell's close flush prefers it)
// R7 track U (docs/PLAN_R7_2026-10-15.md):
//  ⑨ U8/U10 the standalone close warning is a warning toast; the close
//    message is the Korean i18n text
//  ⑱ U2 the unreadable-files notice goes when the queue drains on the same
//    page (no reload); U9 said once
//  ⑳ U1 save({timeoutMs}) is a hard total in a busy spell (timedOut set)
//  ㉒ U1/U10 a save request left on the wire: save({timeoutMs}) and
//    hBeforeClose(2000) answer within their budget, in Korean
// R8 track U (docs/PLAN_R8_2026-10-15.md):
//  ㉓ U2 an explicit save ({ patient: true }) waits for a slow but live
//    reply past its budget; a hard budget does not; a request without a
//    reply (stall) times out; save() with no budget (the desktop shell's
//    fallback) answers within 4 s
import { writeFileSync, mkdtempSync, rmSync } from 'node:fs';
import { join, dirname } from 'node:path';
import { tmpdir } from 'node:os';
import { fileURLToPath, pathToFileURL } from 'node:url';
import { createServer } from 'node:http';
import { createRequire } from 'node:module';
import { viewerPageHtml, viewerBundleJs } from './lib/app-bundle.mjs';

const here = join(dirname(fileURLToPath(import.meta.url)), '..');
const root = process.env.FT_ASSETS_ROOT || here;
const require = createRequire(join(here, 'package.json'));
let chromium;
try { ({ chromium } = require('playwright')); } catch {
  console.error('playwright not resolvable — set NODE_PATH to a node_modules with playwright');
  process.exit(2);
}

const CASE = 'FT-RANGE';
const RANGES_KEY = `ft.viewer.${CASE}.ranges`;
const IDS = ['vid_a', 'vid_b', 'vid_c'];

// ---- mock workstation (patch-v2 rules) ----------------------------------
// id -> { status, note, examiner, range, tags, rev }
const rows = new Map();
const imports = [];
// Server switches for ⑨–⑪.
const mock = { annError: null, pending: 0, pendingRoute: true, pendingItems: [], pendingQueries: [], conflicts: [], conflictsFail: false, metaFail: false, pageCap: 2, conflictPages: [] };
const row = (id) => rows.get(id) || { status: null, note: '', examiner: null, range: null, tags: [], rev: 0 };
const valueOf = (r) => JSON.stringify([r.status, r.note || '', r.examiner || null, r.range || null, [...(r.tags || [])]]);
function apply(body) {
  const v2 = body.protocol === 'patch-v2';
  const marks = new Map((body.marks || []).map((m) => [m.id, m]));
  const tags = new Map((body.tags || []).map((t) => [t.id, t.tags]));
  for (const m of body.marks || []) if (v2 && Array.isArray(m.tags)) tags.set(m.id, m.tags);
  const deleted = new Set(body.deleted_ids || []);
  const ids = new Set([...marks.keys(), ...tags.keys(), ...deleted]);
  const applied = {};
  const conflicts = [];
  for (const id of ids) {
    const cur = row(id);
    const next = { ...cur, tags: [...cur.tags] };
    if (deleted.has(id)) Object.assign(next, { status: null, note: '', range: null, examiner: null });
    if (tags.has(id)) next.tags = [...tags.get(id)];
    const m = marks.get(id);
    if (m) {
      next.status = m.status === 'noted' ? 'noted' : m.status;
      if (typeof m.note === 'string') next.note = m.note;
      if (m.examiner) next.examiner = m.examiner;
      if (m.range && typeof m.range === 'object') next.range = m.range.in == null && m.range.out == null ? null : { in: m.range.in ?? null, out: m.range.out ?? null };
    }
    const changed = valueOf(next) !== valueOf(cur);
    if (v2) {
      const base = Number((body.base_revisions || {})[id]) || 0;
      // Track P contract: base -1 is ALWAYS a conflict (mock.minusOne).
      const forced = (mock.forceConflict && mock.forceConflict.has(id)) || base === -1;
      if ((base !== cur.rev && changed) || forced) {
        conflicts.push({ id, server_revision: cur.rev, server: cur.status || cur.tags.length || cur.range ? { status: cur.status, note: cur.note, examiner: cur.examiner, range: cur.range, tags: cur.tags } : null });
        continue;
      }
    }
    if (changed) { next.rev = cur.rev + 1; rows.set(id, next); }
    applied[id] = changed ? next.rev : cur.rev;
  }
  return { ok: true, protocol: 'patch-v2', applied, conflicts };
}
function annotations() {
  if (mock.annError) return { marks: [], tags: [], revisions: {}, error: mock.annError };
  const marks = [];
  const tags = [];
  const revisions = {};
  for (const [id, r] of rows) {
    revisions[id] = r.rev;
    if (r.status || r.note || r.range) marks.push({ record_id: id, status: r.status || 'noted', marked_unix: 1, note: r.note, examiner: r.examiner, ...(r.range ? { range: r.range } : {}) });
    if (r.tags.length) tags.push({ id, tags: r.tags });
  }
  return { marks, tags, revisions };
}
function buildHtml() {
  const videos = IDS.map((id, i) => ({ id, relative_path: `DCIM/${i}.mp4`, source_path: `/case/DCIM/${i}.mp4`, ffprobe_ok: true, size_bytes: 1000, extension: 'mp4' }));
  const data = `window.__FRAMETRACE_DATA__ = ${JSON.stringify({
    manifest: { case_id: CASE, title: 'range' }, caseDir: '/case', scan: { videos },
    carveLog: [], filesystemLog: [], validationLog: [], anomalyLog: [], flsEntries: [],
    thumbs: {}, annotations: annotations(), deepfake: {}, telemetry: {}, proxies: [],
  })};`;
  return viewerPageHtml({ data, js: viewerBundleJs(root), root });
}
const server = createServer((req, res) => {
  if (req.url === '/api/import-marks') {
    let body = '';
    req.on('data', (c) => { body += c; });
    req.on('end', () => {
      const payload = JSON.parse(JSON.parse(body).marks_json);
      imports.push(payload);
      // The current server parses base_revisions as u64: a -1 refuses the
      // whole body (nothing applied). mock.minusOne = a track-P server.
      const minus = Object.entries(payload.base_revisions || {}).find(([, v]) => v === -1);
      if (minus && !mock.minusOne && !mock.failUnsafe) {
        res.setHeader('Content-Type', 'application/json');
        res.end(JSON.stringify({ ok: false, error: `marks file marks-imported.json (patch-v2): base_revisions[${JSON.stringify(minus[0])}] must be a non-negative integer` }));
        return;
      }
      if (minus && mock.failUnsafe) {
        // R6 U9: the base -1 body fails for a reason of its own (not the parse refusal).
        res.statusCode = 500;
        res.setHeader('Content-Type', 'application/json');
        res.end(JSON.stringify({ ok: false, error: 'disk full while journaling' }));
        return;
      }
      if (mock.hang) {
        // R7 U1: the request stays on the wire until the harness lets it go.
        mock.hung.push(() => { res.setHeader('Content-Type', 'application/json'); res.end(JSON.stringify(apply(payload))); });
        return;
      }
      if (mock.delayMs > 0) {
        // R8 U2: a slow but live server — the reply comes, late.
        setTimeout(() => { res.setHeader('Content-Type', 'application/json'); res.end(JSON.stringify(apply(payload))); }, mock.delayMs);
        return;
      }
      if (mock.busyReplies > 0) {
        // R4 B4: briefly busy outside enrichment — nothing queued, retry.
        mock.busyReplies -= 1;
        res.statusCode = 503;
        res.setHeader('Content-Type', 'application/json');
        res.end(JSON.stringify({ ok: false, busy: true, retry: true, retry_after_ms: mock.busyAfterMs || 300, error: '다른 작업이 진행 중입니다' }));
        return;
      }
      res.setHeader('Content-Type', 'application/json');
      res.end(JSON.stringify(apply(payload)));
    });
    return;
  }
  const json = (code, body) => { res.statusCode = code; res.setHeader('Content-Type', 'application/json'); res.end(JSON.stringify(body)); };
  const url = new URL(req.url, 'http://x');
  if (url.pathname === '/api/status') {
    // mock.oldConflicts: a pre-R4 server — a count, no ids, no route.
    const mc = mock.oldConflicts ? { count: mock.oldConflicts } : { count: mock.conflicts.length, ids: mock.conflicts.map((c) => c.id) };
    return json(200, { ok: true, pending_marks: mock.pending, mark_conflicts: mc });
  }
  if (url.pathname === '/api/records-meta') {
    if (mock.metaFail) return json(500, { ok: false, error: 'meta down' });
    return json(200, { ok: true, annotations: annotations() });
  }
  if (url.pathname === '/api/marks-pending') {
    if (!mock.pendingRoute) return json(404, { ok: false, error: 'not found' });
    mock.pendingQueries.push(url.search);
    if (mock.pendingCursor) {
      // R6 S2 contract: every entry carries cursor "<file seq>:<index>"; the
      // page after "<s>:<i>" starts right behind that entry; next_after is
      // the last entry's cursor (a string), null on the last page. With
      // pendingCursor === 'seq' the server is the R5 one: next_after is the
      // file seq alone and ?after=<seq> skips the rest of that file (S2 bug).
      const items = mock.pendingItems.map((e, i) => ({ ...e, seq: 7, cursor: `7:${i}` }));
      const after = url.searchParams.get('after');
      let start = 0;
      if (after != null) {
        const m = /^(\d+):(\d+)$/.exec(after);
        start = m ? items.findIndex((e) => e.cursor === after) + 1 : items.filter((e) => e.seq <= Number(after)).length;
      }
      const page = items.slice(start, start + 500);
      const more = start + page.length < items.length;
      const next = more ? (mock.pendingCursor === 'seq' ? 7 : page.at(-1).cursor) : null;
      return json(200, { ok: true, files: 3, count: items.length, offset: start, limit: 500, after, next_offset: more ? start + page.length : null, next_after: next, entries: page, unreadable: mock.pendingUnreadable || [], truncated: false });
    }
    const offset = Number(url.searchParams.get('offset')) || 0;
    // Shape of the real route (docs/PLAN_R4 §B4 as implemented): count, next_offset (null at the end), entries.
    const end = offset + mock.pageCap;
    return json(200, { ok: true, count: mock.pendingItems.length + (mock.pendingCountExtra || 0), offset, limit: 500, next_offset: end < mock.pendingItems.length ? end : null, entries: mock.pendingItems.slice(offset, end) });
  }
  if (url.pathname === '/api/mark-conflicts') {
    if (mock.oldConflicts) return json(404, { ok: false, error: 'not found' });
    if (mock.conflictsFail) return json(500, { ok: false, error: 'down' });
    const offset = Number(url.searchParams.get('offset')) || 0;
    const limit = Number(url.searchParams.get('limit')) || 500;
    mock.conflictPages.push([offset, limit]);
    // The server caps a page (mock: 2) below the asked limit: the client pages on.
    const cEnd = offset + Math.min(limit, mock.pageCap);
    // R6 U10: mock.conflictsCut — the server stops listing (its own cap) and says truncated.
    if (mock.conflictsCut) return json(200, { ok: true, count: mock.conflicts.length, offset, limit, next_offset: null, truncated: true, conflicts: mock.conflicts.slice(0, mock.conflictsCut) });
    return json(200, { ok: true, count: mock.conflicts.length, offset, limit, next_offset: cEnd < mock.conflicts.length ? cEnd : null, conflicts: mock.conflicts.slice(offset, cEnd) });
  }
  res.setHeader('Content-Type', 'text/html; charset=utf-8');
  if (url.pathname === '/blank') { res.end('<!doctype html><title>blank</title>'); return; }
  res.end(buildHtml());
});
await new Promise((r) => server.listen(0, '127.0.0.1', r));
const base = `http://127.0.0.1:${server.address().port}/`;

let failed = 0;
const check = (name, ok, detail = '') => {
  console.log(`${ok ? 'PASS' : 'FAIL'}  ${name}${detail ? '  — ' + String(detail).slice(0, 300) : ''}`);
  if (!ok) failed += 1;
};

const browser = await chromium.launch();
const work = mkdtempSync(join(tmpdir(), 'ft-range-sync-'));
try {
  const ctx = await browser.newContext({ viewport: { width: 1366, height: 768 } });
  const page = await ctx.newPage();
  const errors = [];
  page.on('pageerror', (e) => errors.push(String(e)));
  // Loads the page with exactly `ls` in localStorage (null = nothing).
  async function open(ls, raw) {
    // Storage is set on a blank page of the same origin: a viewer page would
    // flush its drafts back on pagehide, after the clear. `raw` values are
    // stored as given (not JSON-encoded).
    await page.goto(base + 'blank');
    await page.evaluate(([entries, rawEntries]) => {
      localStorage.clear();
      for (const [k, v] of Object.entries(entries || {})) localStorage.setItem(k, JSON.stringify(v));
      for (const [k, v] of Object.entries(rawEntries || {})) localStorage.setItem(k, v);
    }, [ls, raw]);
    await page.goto(base);
    await page.waitForSelector('#recordGrid .card');
  }
  const state = () => page.evaluate(() => FTViewer.saveState());
  const waitState = (want, ms = 10000) => page.waitForFunction((w) => FTViewer.saveState() === w, want, { timeout: ms })
    .catch(async (err) => { throw new Error(`waiting for "${want}", state is "${await state().catch(() => '?')}" (${await page.evaluate(() => document.getElementById('saveState').title).catch(() => '')}): ${err.message.split('\n')[0]}`); });
  const select = async (id) => {
    await page.click(`#recordGrid .card[data-id="${id}"]`);
    await page.evaluate(() => document.activeElement?.blur());
    await page.waitForSelector('#mediaStage video', { state: 'attached' });
  };
  const setPoint = async (which, t) => {
    await page.evaluate((v) => {
      const video = document.querySelector('#mediaStage video');
      Object.defineProperty(video, 'currentTime', { get: () => v, configurable: true });
    }, t);
    await page.evaluate((w) => document.getElementById(w === 'in' ? 'btnSetIn' : 'btnSetOut').click(), which);
  };
  const addTag = async (tag) => {
    await page.evaluate(() => document.getElementById('btnTagMenu').click());
    await page.click(`#tagMenuList [data-tag="${tag}"]`);
    await page.keyboard.press('Escape');
  };
  const saveNow = () => page.evaluate(() => document.getElementById('saveState').click());
  const sentFor = (id) => imports.flatMap((b) => [...(b.marks || []).filter((m) => m.id === id), ...(b.deleted_ids || []).filter((d) => d === id).map(() => ({ deleted: true }))]);
  const conflictItem = (id) => page.evaluate((i) => {
    if (!document.getElementById('markConflictPanel') || document.getElementById('markConflictPanel').hidden) document.getElementById('markConflictList')?.click();
    return document.querySelector(`#markConflictPanel [data-id="${CSS.escape(i)}"]`)?.innerText || '';
  }, id);
  const [A, B, C] = IDS;

  // ① i/o → payload → server → saved.
  await open(null);
  await select(A);
  await setPoint('in', 12.5);
  await setPoint('out', 20);
  await saveNow();
  await waitState('saved');
  const sent = imports.at(-1)?.marks.find((m) => m.id === A);
  check('① payload carries range {in,out}', sent?.range?.in === 12.5 && sent?.range?.out === 20, JSON.stringify(sent));
  check('① server stored the range', row(A).range?.out === 20, JSON.stringify(row(A)));

  // ② An old RANGES_KEY that matches the server: no conflict, key dropped.
  await open({ [RANGES_KEY]: { [A]: { in: 12.5, out: 20 } } });
  await waitState('saved');
  check('② equal old RANGES_KEY: no conflict, saved, key removed', (await page.locator('#markConflictBanner').count()) === 0 &&
    await page.evaluate((k) => localStorage.getItem(k) === null, RANGES_KEY));
  check('② no separate range banner any more', (await page.locator('#rangeDivergenceBanner').count()) === 0);

  // ③ An old RANGES_KEY that differs: an ordinary conflict, and edits of
  // that id meanwhile never reach the server nor read as "saved".
  await page.evaluate(() => { document.activeElement?.blur(); });
  rows.set(A, { ...row(A), status: 'important', tags: ['현장'] });
  rows.get(A).rev += 1;
  await open({ [RANGES_KEY]: { [A]: { in: 3, out: 4 } } });
  await page.waitForSelector('#markConflictBanner', { timeout: 10000 });
  const item = await conflictItem(A);
  check('③ differing old range → the conflict list (server 12.5~20 vs mine 3~4, tagged)', /12\.5s ~ 20\.0s/.test(item) && /3\.0s ~ 4\.0s/.test(item) && /이전 구간/.test(item), item.replace(/\s+/g, ' '));
  check('③ save state pending (never saved) while the conflict is open', await state() === 'pending');
  imports.length = 0;
  const revBefore = row(A).rev;
  await select(A);
  await setPoint('out', 9);
  await setPoint('in', 5);
  await addTag('사고');
  await page.evaluate(() => document.activeElement?.blur());
  await page.keyboard.press('0'); // clear the mark of the active record
  await saveNow();
  await page.waitForFunction(() => FTViewer.saveState() !== 'saving');
  check('③ i/o, a tag and a cleared mark on the conflicted id: nothing sent for it', sentFor(A).length === 0, JSON.stringify(imports).slice(0, 300));
  check('③ still pending, never "saved"', await state() === 'pending' && await page.evaluate(() => FTViewer.summary().pending) >= 1, await state());
  check('③ server kept its range, mark and tags', row(A).range?.in === 12.5 && row(A).status === 'important' && JSON.stringify(row(A).tags) === '["현장"]' && row(A).rev === revBefore, JSON.stringify(row(A)));
  const mine3 = await conflictItem(A);
  check('③ the conflict\'s "mine" follows the new edit (5~9)', /5\.0s ~ 9\.0s/.test(mine3), mine3.replace(/\s+/g, ' '));

  // ④ [서버 값 사용]: the server's range, saved, the old key gone.
  await page.click('#markConflictUseServer');
  await page.waitForFunction(() => !document.getElementById('markConflictBanner'));
  await waitState('saved');
  await select(A);
  check('④ [서버 값 사용]: server range shown, server unchanged, old key gone',
    /12\.5s/.test(await page.locator('#rangeLabel').innerText()) && row(A).range?.in === 12.5 && row(A).rev === revBefore &&
    await page.evaluate((k) => localStorage.getItem(k) === null, RANGES_KEY), await page.locator('#rangeLabel').innerText());

  // ⑤ [내 값으로 덮어쓰기]: the browser's range reaches the server.
  await open({ [RANGES_KEY]: { [A]: { in: 3, out: 4 } } });
  await page.waitForSelector('#markConflictBanner', { timeout: 10000 });
  imports.length = 0;
  await page.click('#markConflictUseMine');
  await page.waitForFunction(() => !document.getElementById('markConflictBanner'));
  await waitState('saved');
  const pushed = sentFor(A).at(-1);
  check('⑤ [내 값으로 덮어쓰기] pushes the browser range', pushed?.range?.in === 3 && pushed?.range?.out === 4 && row(A).range?.in === 3, JSON.stringify(pushed));

  // ⑥ Fresh browser: ranges hydrate from the server; a tag and a cleared
  // mark keep the stored range (no deleted_ids while it has one).
  rows.set(B, { status: 'reviewed', note: '', examiner: null, range: { in: 7, out: 8 }, tags: [], rev: 1 });
  await open(null);
  await select(B);
  check('⑥ fresh browser shows the server range (hydrated like marks)', /7\.0s ~ 8\.0s/.test(await page.locator('#rangeLabel').innerText()), await page.locator('#rangeLabel').innerText());
  imports.length = 0;
  await addTag('현장');
  await page.evaluate(() => document.activeElement?.blur());
  await page.keyboard.press('0');
  await saveNow();
  await waitState('saved');
  const bSent = sentFor(B);
  check('⑥ tag + cleared mark: explicit range sent, never deleted_ids', bSent.length > 0 && bSent.every((m) => !m.deleted && m.range?.in === 7), JSON.stringify(bSent));
  check('⑥ server kept the range (mark cleared, tag added)', row(B).range?.in === 7 && row(B).status === 'noted' && row(B).tags.includes('현장'), JSON.stringify(row(B)));

  // ⑦ Another window deleted C's range; an old RANGES_KEY still has it.
  rows.set(C, { status: 'reviewed', note: '', examiner: null, range: null, tags: [], rev: 4 });
  imports.length = 0;
  await open({ [RANGES_KEY]: { [C]: { in: 1, out: 2 } } });
  await page.waitForSelector('#markConflictBanner', { timeout: 10000 });
  await page.waitForFunction(() => FTViewer.saveState() === 'pending');
  await conflictItem(C); // opens the list
  const vals7 = await page.evaluate((i) => [...document.querySelectorAll(`#markConflictPanel [data-id="${CSS.escape(i)}"] dd`)].map((d) => d.textContent), C);
  check('⑦ a range deleted elsewhere is not resurrected: conflict (server: no range, mine: 1~2), server still empty, nothing sent',
    row(C).range === null && sentFor(C).length === 0 && vals7.length === 2 && !/1\.0s/.test(vals7[0]) && /1\.0s ~ 2\.0s/.test(vals7[1]), JSON.stringify(vals7));
  await page.click('#markConflictUseServer');
  await waitState('saved');
  check('⑦ [서버 값 사용] keeps it deleted', row(C).range === null && !(await page.evaluate(() => document.getElementById('rangeLabel').textContent)).includes('1.0s'));

  // ⑧ file:// standalone page: the bundle wins over an old package's leftovers.
  rows.set(A, { status: 'reviewed', note: '', examiner: null, range: { in: 30, out: 31 }, tags: [], rev: 9 });
  const file = join(work, 'evidence-viewer.html');
  writeFileSync(file, buildHtml());
  const fpage = await ctx.newPage();
  fpage.on('pageerror', (e) => errors.push('file: ' + String(e)));
  await fpage.goto(pathToFileURL(file).href);
  await fpage.evaluate((k) => { localStorage.clear(); localStorage.setItem(k, JSON.stringify({ vid_a: { in: 1, out: 2 } })); }, RANGES_KEY);
  await fpage.reload();
  await fpage.waitForSelector('#recordGrid .card');
  await fpage.click(`#recordGrid .card[data-id="${A}"]`);
  await fpage.waitForFunction(() => /\d/.test(document.getElementById('rangeLabel').textContent));
  const fLabel = await fpage.locator('#rangeLabel').innerText();
  check('⑧ file://: the new bundle\'s range shows, not the leftover', /30\.0s ~ 31\.0s/.test(fLabel), fLabel);
  const note = fpage.locator('#legacyRangesNotice');
  check('⑧ file://: the leftover is reported ("이 브라우저의 이전 편집"), kept until discarded',
    (await note.count()) === 1 && /이전 편집/.test(await note.innerText()) && await fpage.evaluate((k) => localStorage.getItem(k) !== null, RANGES_KEY), await note.innerText().catch(() => ''));
  await fpage.click('#legacyRangesDrop');
  check('⑧ [이전 편집 지우기] removes it', (await note.count()) === 0 && await fpage.evaluate((k) => localStorage.getItem(k) === null, RANGES_KEY));

  await fpage.close();
  const chipOf = (id) => page.evaluate((i) => document.querySelector(`#recordGrid .card[data-id="${CSS.escape(i)}"] .mark-chip`)?.className || '', id);

  // ⑨ A3: the annotations could not be read.
  mock.annError = 'database is locked';
  imports.length = 0;
  await open(null);
  await page.waitForSelector('#annotationsErrorBanner');
  check('⑨ read-only banner (role=alert, "판독 기록을 읽지 못했습니다"), state "readonly"',
    /판독 기록을 읽지 못했습니다/.test(await page.locator('#annotationsErrorBanner').innerText()) &&
    await page.locator('#annotationsErrorBanner').getAttribute('role') === 'alert' && await state() === 'readonly', await state());
  await select(B);
  const markBefore = await chipOf(B);
  await page.keyboard.press('2');
  await saveNow();
  await page.waitForFunction(() => FTViewer.saveState() === 'readonly');
  // R5 U1: editing is refused while read-only (it used to be accepted, kept
  // as a draft built on values never seen, and later overwrote / deleted marks).
  const draftCount = () => page.evaluate(() => Object.keys(localStorage).filter((k) => k.startsWith('ft.draft.')).length);
  const r9 = await page.evaluate(() => ({
    toast: document.getElementById('toastHost').innerText,
    buttons: ['btnMarkReviewed', 'btnMarkImportant', 'btnMarkVerify', 'btnMarkClear', 'btnTagMenu', 'btnSetIn', 'btnSetOut'].map((b) => { const el = document.getElementById(b); return el.disabled && /편집할 수 없습니다/.test(el.title); }),
    note: (() => { const n = document.getElementById('evidenceNote'); return !!n && n.readOnly && /편집할 수 없습니다/.test(n.title); })(),
    tagBtns: [...document.querySelectorAll('.tag-editor .tag-btn, #btnAddCustomTag, #customTagInput')].every((el) => el.disabled),
    pending: FTViewer.summary().pending,
  }));
  check('⑨ U1: key 2 while read-only changes nothing (no mark, no draft) and says why',
    await chipOf(B) === markBefore && await draftCount() === 0 && r9.pending === 0 && /편집할 수 없습니다/.test(r9.toast), JSON.stringify(r9));
  check('⑨ U1: mark / tag / IN-OUT buttons, tag editor and note field disabled with the reason', r9.buttons.every(Boolean) && r9.note && r9.tagBtns, JSON.stringify(r9));
  await page.evaluate(() => { const n = document.getElementById('evidenceNote'); n.value = 'typed'; n.dispatchEvent(new Event('input', { bubbles: true })); });
  await page.keyboard.press('Control+z');
  check('⑨ U1: input reaching the note field, and Ctrl+Z, are refused (no draft)', await draftCount() === 0 && await page.evaluate(() => FTViewer.summary().pending) === 0);
  check('⑨ an edit is never sent, the state stays "readonly"', imports.length === 0 && await state() === 'readonly');
  check('⑨ U15: read-only with nothing held here → save() true (closing needs no confirmation)', await page.evaluate(() => FTViewer.save(/* no-budget: read-only answers at once */)) === true && await state() === 'readonly');
  // R6 U4: the desktop shell's close flush prefers window.hBeforeClose():
  // a standalone page has one too, answers at once and carries (and shows)
  // the read-only warning.
  const close9 = await page.evaluate(async () => {
    if (typeof window.hBeforeClose !== 'function') return { missing: true };
    const t0 = performance.now();
    const r = await window.hBeforeClose();
    const shown = [...document.querySelectorAll('#toastHost .toast')].find((el) => /판독 기록을 읽지 못한 상태였습니다/.test(el.innerText));
    return { ...r, ms: performance.now() - t0, toast: document.getElementById('toastHost').innerText, toastClass: shown ? shown.className : '' };
  });
  check('⑨ U4: standalone hBeforeClose() → ok, the read-only warning (also shown), answered at once',
    close9.ok === true && close9.pending === 0 && /판독 기록을 읽지 못한/.test(close9.warning || '') && /판독 기록을 읽지 못한/.test(close9.toast || '') && close9.ms < 1000, JSON.stringify(close9));
  check('⑨ R7 U8: that warning is a warning toast (toast-warn), not the error look', /\btoast-warn\b/.test(close9.toastClass) && !/\btoast-error\b/.test(close9.toastClass), close9.toastClass);
  check('⑨ summary report / CSV disabled', await page.evaluate(() => ['btnSummary', 'btnDownloadCsv'].every((id) => document.getElementById(id).disabled)));
  // A draft held from before (a closed earlier window): kept, save() false.
  const seeded = { mark: { status: 'important', marked_unix: 1 }, note: '', tags: [], examiner: null, range: null, seq: 1, base: 0 };
  await open({ [`ft.draft.${CASE}.wdead.${C}`]: seeded }, { [`ft.alive:${CASE}:wdead`]: 'closed' });
  await page.waitForSelector('#annotationsErrorBanner');
  check('⑨ U15: read-only WITH a held draft → save() false (the close asks)', await page.evaluate(() => FTViewer.save(/* no-budget: read-only answers at once */)) === false && imports.length === 0);
  const close9b = await page.evaluate(() => window.hBeforeClose && window.hBeforeClose());
  check('⑨ U4: standalone hBeforeClose() with a held draft → ok false, no "nothing unsaved" warning', !!close9b && close9b.ok === false && !close9b.warning, JSON.stringify(close9b));
  check('⑨ R7 U10: its message is Korean (i18n "저장 안 된 편집 N건"), not "unsaved N"', !!close9b && /^저장 안 된 편집 \d+건$/.test(close9b.message || ''), JSON.stringify(close9b));
  mock.annError = null;

  // ⑩ A4: marks in the server's queue across a reload.
  rows.delete(C);
  mock.pending = 1;
  mock.pendingItems = [{ id: C, mark: { id: C, status: 'important', note: 'queued note', marked_unix: 5 }, tags: ['현장'], deleted: false, base_revision: 0, client_id: 'w-before-reload' }];
  await open(null);
  await page.waitForFunction(() => FTViewer.saveState() === 'queued', null, { timeout: 10000 });
  check('⑩ after a reload the queued value shows (important) and the state is "queued"', /important/.test(await chipOf(C)) && await state() === 'queued', await chipOf(C));
  const warns = await page.evaluate(() => { const e = new Event('beforeunload', { cancelable: true }); window.dispatchEvent(e); return e.defaultPrevented; });
  check('⑩ leaving the page warns while the server queue holds marks', warns === true);
  check('⑩ FTViewer.save({ patient: true }) is true (the queue is durable), the indicator is not "saved"', await page.evaluate(() => FTViewer.save({ patient: true })) === true && await state() === 'queued');
  // The queue drains: the value lands in the case DB.
  rows.set(C, { status: 'important', note: 'queued note', examiner: null, range: null, tags: ['현장'], rev: 1 });
  mock.pending = 0;
  await waitState('saved', 15000);
  check('⑩ queue drained → "saved", the value stays', /important/.test(await chipOf(C)));
  mock.pending = 1;
  mock.pendingRoute = false;
  await open(null);
  await page.waitForFunction(() => FTViewer.saveState() === 'queued', null, { timeout: 10000 });
  check('⑩ a server without /api/marks-pending: still "queued", never "saved"', await state() === 'queued');
  mock.pending = 0;
  mock.pendingRoute = true;
  await waitState('saved', 15000);

  // ⑪ A5: the server's queue conflicts cannot be read.
  mock.conflicts = [A, B, C].map((id) => ({ id, server_revision: row(id).rev, server: { status: 'reviewed', note: 'srv', tags: [] }, queued: { mark: { id, status: 'important', note: 'q' }, tags: null, deleted: false } }));
  mock.conflictsFail = true;
  mock.metaFail = true;
  await open(null);
  await page.waitForFunction(() => FTViewer.saveState() === 'unknown', null, { timeout: 10000 });
  check('⑪ conflicts unreadable (both routes fail) → "unknown" ("충돌 확인 실패"), never "saved"',
    /충돌 확인 실패/.test(await page.locator('#saveState').innerText()) && await page.evaluate(() => FTViewer.save(/* no-budget: the lookup failure answers at once */)) === false);
  mock.conflictsFail = false;
  mock.metaFail = false;
  mock.conflictPages = [];
  await page.waitForSelector('#markConflictBanner', { timeout: 15000 }); // the backoff retry (2 s)
  await page.waitForFunction(() => FTViewer.saveState() === 'pending');
  check('⑪ the retry reads every page (server caps at 2 of 3): all 3 conflicts listed', /3/.test(await page.locator('#markConflictBanner .conflict-msg').innerText()) &&
    mock.conflictPages.length >= 2 && mock.conflictPages[1][0] === 2, JSON.stringify(mock.conflictPages));
  mock.conflicts = [];

  // ⑫ A9: "현재 이름으로 귀속" reports success only after the save came
  // back — and never when it ran into a conflict.
  for (const id of IDS) rows.set(id, { status: 'important', note: '', examiner: null, range: null, tags: [], rev: 3 });
  await open(null);
  await waitState('saved');
  mock.forceConflict = new Set([C]);
  imports.length = 0;
  const a9fail = await page.evaluate(() => FTViewer.attributeUnassignedMarks('검사관').then((n) => ({ ok: true, n }), (e) => ({ ok: false, err: String(e && e.message) })));
  check('⑫ a conflicted attribution rejects (no success to report)', a9fail.ok === false && /충돌/.test(a9fail.err) && imports.length >= 1, JSON.stringify(a9fail));
  mock.forceConflict = null;
  await page.click('#markConflictUseMine');
  await waitState('saved');
  for (const id of IDS) rows.set(id, { ...row(id), examiner: null, rev: row(id).rev + 1 });
  await open(null);
  await waitState('saved');
  imports.length = 0;
  const a9ok = await page.evaluate(() => FTViewer.attributeUnassignedMarks('검사관2').then((n) => ({ ok: true, n, state: FTViewer.saveState() }), (e) => ({ ok: false, err: String(e && e.message) })));
  check('⑫ attribution resolves with the count only after the save landed', a9ok.ok === true && a9ok.n === 3 && imports.length >= 1 && IDS.every((id) => row(id).examiner === '검사관2'), JSON.stringify(a9ok));

  // ⑬ R4 B4: a "busy, retry" reply is transient — never "saved", never
  // "failed", retried after retry_after_ms until it lands.
  mock.busyReplies = 2;
  imports.length = 0;
  const seen13 = [];
  await page.evaluate(() => { window.__ss13 = []; FTViewer.on('save-state', (e) => window.__ss13.push(e.detail.state)); });
  await select(A);
  await page.keyboard.press('3');
  await saveNow();
  await waitState('saved', 15000);
  seen13.push(...await page.evaluate(() => window.__ss13));
  check('⑬ busy/retry replies: retried until saved, never shown as failed', imports.length >= 3 && !seen13.includes('failed') && row(A).status === 'needs_verification', `${imports.length} posts, states ${seen13.join(',')}`);

  // ---- R5 track U ---------------------------------------------------------
  // ⑭ U1: a stored draft made while read-only (unsafe_base) whose base even
  // MATCHES the server's revision is never applied: it goes out with base -1.
  rows.set(A, { status: 'important', note: 'srv', examiner: null, range: null, tags: [], rev: 7 });
  const unsafeDraft = { mark: { status: 'reviewed', marked_unix: 1 }, note: '', tags: [], examiner: null, range: null, seq: 1, base: 7, unsafe_base: true };
  for (const minusOne of [false, true]) {
    mock.minusOne = minusOne;
    imports.length = 0;
    await open({ [`ft.draft.${CASE}.wold.${A}`]: unsafeDraft }, { [`ft.alive:${CASE}:wold`]: 'closed' });
    await page.waitForSelector('#markConflictBanner', { timeout: 15000 });
    const sentMinus = imports.some((b) => (b.base_revisions || {})[A] === -1);
    const item14 = await conflictItem(A);
    check(`⑭ U1 (${minusOne ? 'server knows -1' : 'current server refuses -1'}): unsafe draft sent with base -1, shown as a conflict, server unchanged`,
      sentMinus && row(A).status === 'important' && row(A).rev === 7 && /srv/.test(item14) && await state() === 'pending', JSON.stringify({ sentMinus, row: row(A), item14: item14.replace(/\s+/g, ' ') }));
  }
  await page.click('#markConflictUseServer');
  await waitState('saved');
  check('⑭ U1: [서버 값 사용] resolves it; the server keeps its value', row(A).status === 'important' && row(A).rev === 7);
  mock.minusOne = false;

  // ⑮ U4: a queued value shown over the server's goes back to the case DB
  // value when the queue drains without applying it (here: unchanged row).
  rows.set(C, { status: 'reviewed', note: '', examiner: null, range: null, tags: [], rev: 2 });
  mock.pending = 1;
  mock.pageCap = 2;
  mock.pendingItems = [{ id: C, mark: { id: C, status: 'important', note: 'q', marked_unix: 5 }, tags: null, deleted: false, base_revision: 1, client_id: 'w-x' }];
  await open(null);
  await page.waitForFunction(() => FTViewer.saveState() === 'queued', null, { timeout: 10000 });
  const overlay = await chipOf(C);
  mock.pending = 0; // drained — the body met a newer revision and was not applied
  await waitState('saved', 15000);
  await page.waitForFunction((id) => !/important/.test(document.querySelector(`#recordGrid .card[data-id="${CSS.escape(id)}"] .mark-chip`)?.className || ''), C, { timeout: 5000 }).catch(() => {});
  check('⑮ U4: after the drain the queued overlay gives way to the case DB value', /important/.test(overlay) && /reviewed/.test(await chipOf(C)), `${overlay} → ${await chipOf(C)}`);
  // Unreadable / truncated queued values are named, never silently missing.
  mock.pending = 1;
  mock.pendingRoute = false;
  await open(null);
  await page.waitForFunction(() => FTViewer.saveState() === 'queued', null, { timeout: 10000 });
  const unread = await page.evaluate(() => ({ v: document.getElementById('saveState').dataset.queueValues, title: document.getElementById('saveState').title }));
  check('⑮ U4: queued values unreadable → said in the indicator', unread.v === 'unreadable' && /읽지 못해/.test(unread.title), JSON.stringify(unread));
  mock.pendingRoute = true;
  mock.pendingCountExtra = 3;
  mock.pendingItems = [C, A, B].map((id) => ({ id, mark: { id, status: 'important', note: '', marked_unix: 5 }, tags: null, deleted: false, base_revision: 0 }));
  await open(null);
  await page.waitForFunction(() => FTViewer.saveState() === 'queued', null, { timeout: 10000 });
  const trunc = await page.evaluate(() => ({ v: document.getElementById('saveState').dataset.queueValues, title: document.getElementById('saveState').title }));
  check('⑮ U4: fewer queued entries than the server counts → "truncated" said', trunc.v === 'truncated' && /일부만/.test(trunc.title), JSON.stringify(trunc));
  mock.pendingCountExtra = 0;
  mock.pendingItems = [];
  mock.pending = 0;
  await waitState('saved', 15000);

  // ⑯ U5: an old server reports conflicts with neither ids nor a list route.
  mock.oldConflicts = 2;
  await open(null);
  await page.waitForSelector('#conflictsUnlistedNotice', { timeout: 10000 });
  await waitState('saved', 10000);
  check('⑯ U5: old server (count, no ids, no route) → notice "구버전 서버", state not stuck at "unknown"',
    /구버전 서버/.test(await page.locator('#conflictsUnlistedNotice').innerText()) && await state() === 'saved');
  mock.oldConflicts = 0;

  // ⑰ U6: FTViewer.save() waits out "busy, retry" replies instead of false.
  mock.busyReplies = 2;
  imports.length = 0;
  await open(null);
  await waitState('saved');
  await select(B);
  await page.keyboard.press('3');
  const saved17 = await page.evaluate(() => FTViewer.save({ patient: true })); // R9 U9
  check('⑰ U6: save() through two busy replies resolves true, the mark landed', saved17 === true && row(B).status === 'needs_verification' && imports.length >= 3, `${saved17}, ${imports.length} posts, ${row(B).status}`);

  // ---- R6 track U ---------------------------------------------------------
  // ⑱ U1/U2: one queue file of 800 ids, string cursors across the page
  // boundary; the last id is C (so a missed second page would show).
  rows.set(C, { status: 'reviewed', note: '', examiner: null, range: null, tags: [], rev: 2 });
  const many = Array.from({ length: 799 }, (_, i) => ({ id: `vid_q${String(i).padStart(4, '0')}`, mark: { status: 'reviewed', marked_unix: 5 }, tags: null, deleted: false, base_revision: 0 }));
  many.push({ id: C, mark: { id: C, status: 'important', note: 'page 2', marked_unix: 5 }, tags: null, deleted: false, base_revision: 2 });
  Object.assign(mock, { pending: 3, pendingCursor: true, pendingItems: many, pendingQueries: [], pendingUnreadable: [{ file: '00000000000000000000000000000005.json', error: 'EOF while parsing' }, { file: '00000000000000000000000000000006.json', error: 'invalid utf-8' }] });
  await open(null);
  await page.waitForFunction(() => FTViewer.saveState() === 'queued', null, { timeout: 10000 });
  const q18 = await page.evaluate(() => ({ v: document.getElementById('saveState').dataset.queueValues || '', u: document.getElementById('saveState').dataset.queueUnreadable || '', title: document.getElementById('saveState').title,
    notice: document.getElementById('queueUnreadableNotice')?.innerText || '', role: document.getElementById('queueUnreadableNotice')?.getAttribute('role') }));
  check('⑱ U1: 800 ids in one queue file read across the page boundary with the string cursor (?after=7%3A499)',
    mock.pendingQueries.length === 2 && /after=7%3A499&/.test(mock.pendingQueries[1]) && /important/.test(await chipOf(C)) && q18.v === '', JSON.stringify({ q: mock.pendingQueries, chip: await chipOf(C), v: q18.v }));
  check('⑱ U2: the server\'s unreadable queue files are shown ("대기 파일 2개 읽기 실패", role=status, indicator title)',
    /대기 파일 2개 읽기 실패/.test(q18.notice) && q18.role === 'status' && q18.u === '2' && /대기 파일 2개 읽기 실패/.test(q18.title), JSON.stringify(q18));
  // R7 U9: said once — not "…읽지 못했습니다(대기 파일 2개 읽기 실패)".
  check('⑱ R7 U9: the unreadable-files notice says it once (no doubled wording)', (q18.notice.match(/읽기 실패|읽지 못했/g) || []).length === 1, q18.notice);
  // R7 U2: the queue drains while the page stays open — the notice and the
  // indicator's note go with it (R6 checked this only across a reload,
  // which re-reads everything anyway).
  await page.evaluate(() => { window.__sameLoad18 = 1; });
  Object.assign(mock, { pending: 0, pendingCursor: false, pendingItems: [], pendingUnreadable: [] });
  await waitState('saved', 15000);
  const drained18 = await page.evaluate(() => ({ same: window.__sameLoad18 === 1, notice: !!document.getElementById('queueUnreadableNotice'), u: document.getElementById('saveState').dataset.queueUnreadable || '', title: document.getElementById('saveState').title }));
  check('⑱ R7 U2: the queue drained (same page, no reload) → the unreadable notice and the indicator note are gone',
    drained18.same && !drained18.notice && drained18.u === '' && !/읽기 실패/.test(drained18.title), JSON.stringify(drained18));
  // The R5 numeric cursor (?after=<seq> skips the rest of that file): never a silent gap.
  Object.assign(mock, { pending: 3, pendingCursor: 'seq', pendingItems: many, pendingQueries: [], pendingUnreadable: [] });
  await open(null);
  await page.waitForFunction(() => FTViewer.saveState() === 'queued', null, { timeout: 10000 });
  const q18b = await page.evaluate(() => ({ v: document.getElementById('saveState').dataset.queueValues || '', notice: !!document.getElementById('queueUnreadableNotice') }));
  check('⑱ U1: an R5 numeric-cursor server that skips inside the file → "truncated" said, no unreadable notice', q18b.v === 'truncated' && !q18b.notice && /after=7&/.test(mock.pendingQueries[1] || ''), JSON.stringify({ q18b, q: mock.pendingQueries }));
  Object.assign(mock, { pending: 0, pendingCursor: false, pendingItems: [] });
  await waitState('saved', 15000);

  // ⑲ U10: the server lists 2 of its 3 conflicts and says truncated.
  mock.conflicts = [A, B, C].map((id) => ({ id, server_revision: row(id).rev, server: { status: 'reviewed', note: 'srv', tags: [] }, queued: { mark: { id, status: 'important', note: 'q' }, tags: null, deleted: false } }));
  mock.conflictsCut = 2;
  await open(null);
  await page.waitForSelector('#markConflictBanner', { timeout: 10000 });
  const c19 = await page.evaluate(() => ({ notice: document.getElementById('conflictsTruncatedNotice')?.innerText || '', role: document.getElementById('conflictsTruncatedNotice')?.getAttribute('role'), banner: document.querySelector('#markConflictBanner .conflict-msg')?.innerText || '' }));
  check('⑲ U10: a truncated conflict list is said (2/3, role=status), the 2 listed are shown',
    /2\/3/.test(c19.notice) && c19.role === 'status' && /2건/.test(c19.banner), JSON.stringify(c19));
  mock.conflictsCut = 0;
  mock.conflicts = [];
  await page.click('#markConflictUseServer');
  await waitState('saved', 15000);
  check('⑲ U10: once the listed ones are decided the list is read again — the notice goes', !(await page.locator('#conflictsTruncatedNotice').count()));

  // ⑳ R7 U5: an explicit save() (finalize retry, case switch) keeps the
  // full 20 s budget — 4 busy replies × 1.5 s (6 s) are waited out and the
  // mark lands (R6 cut every default caller to 5 s: false). The close
  // flush's short budget (save({ timeoutMs })) is a hard total: a long
  // busy spell answers false by then. The indicator shows the retries.
  await waitState('saved');
  Object.assign(mock, { busyReplies: 8, busyAfterMs: 1500 });
  await select(A);
  await page.keyboard.press('3');
  const t20 = Date.now();
  const p20 = page.evaluate(() => FTViewer.save({ patient: true }));
  await page.waitForFunction(() => Number(document.getElementById('saveState').dataset.busyRetry) > 0, null, { timeout: 8000 }).catch(() => {});
  const ind20 = await page.evaluate(() => ({ n: document.getElementById('saveState').dataset.busyRetry || '', label: document.getElementById('saveState').innerText }));
  const saved20 = await p20;
  const ms20 = Date.now() - t20;
  check('⑳ R7 U5 / R8 U2: save({ patient: true }) (explicit, 20 s budget) waits out a 6 s+ busy spell (8 × 1.5 s replies) and lands', saved20 === true && row(A).status === 'needs_verification' && ms20 >= 5500, `${saved20} ${row(A).status} after ${ms20} ms`);
  check('⑳ U5: the indicator shows the busy retries meanwhile ("서버 바쁨 · 재시도 n/10")', Number(ind20.n) > 0 && /서버 바쁨/.test(ind20.label), JSON.stringify(ind20));
  await waitState('saved', 15000);
  Object.assign(mock, { busyReplies: 1000, busyAfterMs: 1500 });
  await select(A);
  await page.keyboard.press('2');
  const r20b = await page.evaluate(async () => {
    const opts = { timeoutMs: 2500 };
    const t0 = performance.now();
    const saved = await FTViewer.save(opts);
    return { saved, timedOut: opts.timedOut === true, ms: performance.now() - t0 };
  });
  check('⑳ R7 U1: save({ timeoutMs: 2500 }) in a long busy spell answers false within the budget (timedOut set)', r20b.saved === false && r20b.timedOut && r20b.ms < 2900, JSON.stringify(r20b));
  Object.assign(mock, { busyReplies: 0, busyAfterMs: 0 });
  await waitState('saved', 15000);

  // ㉒ R7 U1/U10: a save request that stays on the wire. The close budget
  // is a hard TOTAL — the wait for the request in flight counts (R6 waited
  // for it without a limit inside save(), only a race outside answered) —
  // and the standalone hBeforeClose answers in Korean (i18n), not English.
  Object.assign(mock, { hang: true, hung: [] });
  await select(A);
  await page.keyboard.press('1');
  await saveNow();
  await page.waitForFunction(() => FTViewer.saveState() === 'saving', null, { timeout: 5000 }).catch(() => {});
  const r22 = await page.evaluate(async () => {
    const guard = (p) => Promise.race([p, new Promise((r) => setTimeout(() => r('hung'), 6000))]);
    const opts = { timeoutMs: 1500 };
    let t0 = performance.now();
    const saved = await guard(FTViewer.save(opts));
    const saveMs = performance.now() - t0;
    t0 = performance.now();
    const close = await guard(window.hBeforeClose(2000));
    return { saved, timedOut: opts.timedOut === true, saveMs, close, closeMs: performance.now() - t0 };
  });
  check('㉒ R7 U1: save({ timeoutMs: 1500 }) with a save on the wire answers false within the budget (timedOut)', r22.saved === false && r22.timedOut && r22.saveMs < 1900, JSON.stringify(r22));
  check('㉒ R7 U1: hBeforeClose(2000) answers within its 2 s total while the request hangs', r22.close !== 'hung' && r22.closeMs <= 2000 && r22.close.ok === false, JSON.stringify(r22));
  check('㉒ R7 U10: its message is the Korean i18n text ("저장 안 된 편집 1건 (저장 응답 대기 시간 초과)")', r22.close && /^저장 안 된 편집 1건 \(저장 응답 대기 시간 초과\)$/.test(r22.close.message || ''), JSON.stringify(r22.close));
  mock.hang = false;
  for (const release of mock.hung.splice(0)) release();
  await waitState('saved', 15000);
  check('㉒ the released request still settles the drafts ("saved", the mark stored)', row(A).status === 'reviewed', JSON.stringify(row(A)));

  // ㉓ R8 U2: a slow but LIVE save (the reply comes after 2.5 s) is not
  // abandoned by an explicit save at its budget; the close flush's hard
  // budget still answers on time; a genuinely stalled request (no reply)
  // times out after the stall window; save() with no budget is <= 4 s.
  const timed = (expr) => page.evaluate(async (e) => {
    const opts = e.opts;
    const t0 = performance.now();
    const saved = await (opts ? FTViewer.save(opts) : FTViewer.save(/* no-budget: the shell fallback under test */));
    return { saved, timedOut: !!(opts && opts.timedOut), ms: Math.round(performance.now() - t0) };
  }, expr);
  mock.delayMs = 2500;
  await select(A);
  await page.keyboard.press('2');
  const r23a = await timed({ opts: { patient: true, timeoutMs: 1000, stallMs: 5000 } });
  check('㉓ R8 U2: save({ patient: true, timeoutMs: 1000 }) waits for the slow reply (2.5 s) and lands', r23a.saved === true && r23a.ms >= 2300 && row(A).status === 'important', JSON.stringify({ r23a, A: row(A).status }));
  await waitState('saved', 15000);
  await select(A);
  await page.keyboard.press('3');
  const r23b = await timed({ opts: { timeoutMs: 1000 } });
  check('㉓ R8 U2: the hard save({ timeoutMs: 1000 }) still answers false at its budget (timedOut)', r23b.saved === false && r23b.timedOut && r23b.ms < 1400, JSON.stringify(r23b));
  await waitState('saved', 15000);
  mock.delayMs = 0;
  Object.assign(mock, { hang: true, hung: [] });
  await select(A);
  await page.keyboard.press('1');
  const r23c = await timed({ opts: { patient: true, timeoutMs: 500, stallMs: 1500 } });
  check('㉓ R8 U2: a stalled request (no reply) — the patient save times out after the stall window', r23c.saved === false && r23c.timedOut && r23c.ms >= 1300 && r23c.ms < 2600, JSON.stringify(r23c));
  const r23d = await timed({ opts: null });
  check("㉓ R8 U2: save() with no budget (the desktop shell's fallback) answers within 4 s while the request hangs", r23d.saved === false && r23d.ms <= 4300, JSON.stringify(r23d));
  mock.hang = false;
  for (const release of mock.hung.splice(0)) release();
  await waitState('saved', 15000);

  // ㉑ U9: the base -1 body fails (its own error); the ordinary edit still goes out.
  rows.set(A, { status: 'important', note: 'srv', examiner: null, range: null, tags: [], rev: 9 });
  rows.set(B, { status: null, note: '', examiner: null, range: null, tags: [], rev: 0 });
  mock.failUnsafe = true;
  await open({ [`ft.draft.${CASE}.wold9.${A}`]: { ...unsafeDraft, base: 9 } }, { [`ft.alive:${CASE}:wold9`]: 'closed' });
  await page.waitForFunction(() => FTViewer.saveState() === 'failed', null, { timeout: 15000 }).catch(() => {});
  imports.length = 0;
  await select(B);
  await page.keyboard.press('2');
  await saveNow();
  await page.waitForFunction(() => FTViewer.saveState() !== 'saving', null, { timeout: 10000 });
  await page.waitForFunction(() => FTViewer.saveState() !== 'saving');
  const sentB = imports.some((b) => (b.marks || []).some((m) => m.id === B) && (b.base_revisions || {})[B] !== -1);
  check('㉑ U9: the failing base -1 body did not block the ordinary edit — B sent and stored, A still held',
    sentB && row(B).status === 'important' && row(A).status === 'important' && row(A).rev === 9 && await page.evaluate((id) => !!Object.keys(localStorage).find((k) => k.endsWith('.' + id) && k.startsWith('ft.draft.')), A),
    JSON.stringify({ sentB, B: row(B), A: row(A), posts: imports.map((b) => Object.entries(b.base_revisions || {})) }));
  mock.failUnsafe = false;

  check('no page errors', !errors.length, errors.join(' | '));
} catch (err) {
  check('harness ran to the end', false, err && err.stack || String(err));
} finally {
  await browser.close();
  server.close();
  rmSync(work, { recursive: true, force: true });
}
console.log(failed ? `verify-range-sync: ${failed} failed` : 'verify-range-sync: all checks passed');
process.exit(failed ? 1 : 0);
