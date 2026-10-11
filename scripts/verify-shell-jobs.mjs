// VM harness for the workstation shell (assets/app, bundled like build.rs):
// background Recover/Carve jobs (async contract + old synchronous
// responses), viewer refresh without `src = src`, review-frame load
// timeout/retry, preflight panel, checkpoint resume info, guarded
// "새 케이스 시작" reload, theme/locale propagation and the breadcrumb.
//
//   node scripts/verify-shell-jobs.mjs
//
// No server or browser: a fake DOM, a scripted fetch and virtual timers.
import vm from 'node:vm';
import assert from 'node:assert/strict';
import { appBundleJs, appMarkup } from './lib/app-bundle.mjs';

const BUNDLE = appBundleJs();
const MARKUP = appMarkup();

// Initial `hidden` state of every id'd element, straight from the markup.
const MARKUP_HIDDEN = new Map();
for (const m of MARKUP.matchAll(/<(\w+)([^>]*?)\bid="([^"]+)"([^>]*)>/g)) {
  const attrs = m[2] + ' ' + m[4];
  MARKUP_HIDDEN.set(m[3], /(^|\s)hidden(\s|=|$)/.test(attrs));
}

function makeEl(id, tag = 'div') {
  const listeners = {};
  const set = new Set();
  const el = {
    id, tagName: tag.toUpperCase(), value: '', textContent: '', innerHTML: '', title: '', placeholder: '',
    hidden: MARKUP_HIDDEN.get(id) === true, disabled: false, checked: false, className: '',
    style: {}, dataset: {}, attrs: {}, children: [], clicks: 0, files: [],
    classList: {
      add: (...c) => c.forEach((x) => set.add(x)),
      remove: (...c) => c.forEach((x) => set.delete(x)),
      toggle: (c, on) => { const v = on === undefined ? !set.has(c) : !!on; if (v) set.add(c); else set.delete(c); return v; },
      contains: (c) => set.has(c),
    },
    addEventListener(t, fn) { (listeners[t] ||= []).push(fn); },
    removeEventListener(t, fn) { listeners[t] = (listeners[t] || []).filter((f) => f !== fn); },
    dispatch(t, ev = {}) { for (const fn of [...(listeners[t] || [])]) fn({ type: t, target: el, preventDefault() {}, ...ev }); },
    click() { el.clicks += 1; el.dispatch('click'); },
    setAttribute(k, v) { el.attrs[k] = String(v); },
    getAttribute(k) { return el.attrs[k] ?? null; },
    removeAttribute(k) { delete el.attrs[k]; },
    appendChild(c) { el.children.push(c); return c; },
    append(...c) { el.children.push(...c); },
    remove() {}, focus() {}, select() {}, scrollIntoView() {},
    closest: () => null, querySelectorAll: () => [], querySelector: () => null, contains: () => false,
  };
  return el;
}

function fakeViewerDoc({ scripts = true, saveState = 'saved', lang = 'ko', readyState = 'complete' } = {}) {
  const saveEl = makeEl('saveState');
  saveEl.dataset.state = saveState;
  const langBtn = makeEl('btnLang');
  const doc = {
    readyState,
    documentElement: { dataset: {}, lang },
    querySelector: (sel) => (sel === 'script' && scripts ? {} : null),
    getElementById: (id) => (id === 'saveState' ? saveEl : id === 'btnLang' ? langBtn : null),
  };
  langBtn.addEventListener('click', () => { doc.documentElement.lang = doc.documentElement.lang === 'ko' ? 'en' : 'ko'; });
  return { doc, saveEl, langBtn };
}

function makeEnv(opts = {}) {
  let clock = 1_700_000_000_000;
  const timers = new Map();
  let timerSeq = 1;
  const errors = [];
  const sT = (fn, ms = 0) => { const id = timerSeq++; timers.set(id, { fn, due: clock + Math.max(0, Number(ms) || 0), every: 0 }); return id; };
  const cT = (id) => { timers.delete(id); };
  const sI = (fn, ms) => { const id = timerSeq++; timers.set(id, { fn, due: clock + ms, every: Math.max(1, ms) }); return id; };
  const flush = async () => { for (let i = 0; i < 30; i++) await new Promise((r) => setImmediate(r)); };
  async function advance(ms) {
    const target = clock + ms;
    for (;;) {
      await flush();
      let next = null;
      for (const [id, t] of timers) if (t.due <= target && (!next || t.due < next[1].due)) next = [id, t];
      if (!next) break;
      const [id, t] = next;
      clock = Math.max(clock, t.due);
      if (t.every) t.due += t.every; else timers.delete(id);
      try { t.fn(); } catch (e) { errors.push(e); }
    }
    clock = target;
    await flush();
  }
  class FakeDate extends Date {
    constructor(...a) { if (a.length) super(...a); else super(clock); }
    static now() { return clock; }
  }

  const env = {
    status: { ok: true, has_job: true, phase: 'review-ready', busy: false, case_dir: '/cases/c1', steps: ['done', 'done', 'done', 'done', 'done'], logs: [], error: null },
    routes: opts.routes || {},
    calls: [],
  };
  const respond = (status, json) => ({ ok: status < 400, status, json: async () => { if (json === undefined) throw new Error('not json'); return json; } });
  async function fetchImpl(url, init = {}) {
    const method = (init.method || 'GET').toUpperCase();
    const body = init.body ? JSON.parse(init.body) : null;
    env.calls.push({ method, url, body });
    const path = url.split('?')[0];
    const route = env.routes[`${method} ${url}`] || env.routes[`${method} ${path}`];
    if (route) {
      const r = await route(body, url);
      return respond(r.status ?? 200, r.json);
    }
    if (path === '/api/status') return respond(200, JSON.parse(JSON.stringify(env.status)));
    if (path === '/api/env') return respond(200, { ok: true, tools: { ffmpeg: true, ffprobe: true }, workflows: {}, hints: {} });
    if (path === '/api/browse') return respond(200, { ok: true, exists: false });
    return respond(404, undefined);
  }

  const els = {};
  const winListeners = {};
  const store = () => { const m = new Map(); return { getItem: (k) => (m.has(k) ? m.get(k) : null), setItem: (k, v) => m.set(k, String(v)), removeItem: (k) => m.delete(k) }; };
  const document = {
    getElementById: (id) => (els[id] ||= makeEl(id, id === 'reviewFrame' ? 'iframe' : 'div')),
    querySelectorAll: () => [],
    querySelector: () => null,
    createElement: (tag) => makeEl('', tag),
    createTextNode: (t) => ({ text: t }),
    addEventListener() {},
    documentElement: { dataset: { theme: 'light' } },
    activeElement: null,
    body: makeEl('body'),
  };
  const frame = document.getElementById('reviewFrame');
  const frameWin = {
    posted: [],
    postMessage(m) { this.posted.push(m); },
    location: { reloads: 0, reload() { this.reloads += 1; } },
  };
  frame.contentWindow = frameWin;
  frame.contentDocument = null;
  // Count every src assignment: `src = src` keeps the string but navigates.
  let frameSrc = "";
  frame.srcSets = 0;
  Object.defineProperty(frame, "src", { get: () => frameSrc, set: (v) => { frame.srcSets += 1; frameSrc = String(v); } });

  const ctx = {
    document, console: { log() {}, error: (...a) => env.consoleErrors.push(a.join(' ')), warn() {} },
    fetch: fetchImpl, setTimeout: sT, clearTimeout: cT, setInterval: sI, clearInterval: cT,
    Date: FakeDate, localStorage: store(), sessionStorage: store(), navigator: {},
    location: { protocol: 'http:', origin: 'http://127.0.0.1', reloads: 0, reload() { this.reloads += 1; } },
    Event: class Event { constructor(type) { this.type = type; } },
    addEventListener(t, fn) { (winListeners[t] ||= []).push(fn); },
    removeEventListener(t, fn) { winListeners[t] = (winListeners[t] || []).filter((f) => f !== fn); },
    open: () => ({ focus() {} }),
    URLSearchParams, encodeURIComponent, JSON, Math, Number, String, Object, Array, Promise, Set, Map, RegExp, Error,
  };
  ctx.window = ctx;
  env.consoleErrors = [];
  vm.createContext(ctx);
  vm.runInContext(BUNDLE, ctx);
  Object.assign(env, {
    ctx, els, frame, frameWin, advance, flush, errors,
    $: (id) => document.getElementById(id),
    ev: (expr) => vm.runInContext(expr, ctx),
    winMessage: (data, source) => { for (const fn of [...(winListeners.message || [])]) fn({ data, source }); },
    async answerDialog(ok) {
      await flush();
      assert.equal(env.$('dlgBackdrop').hidden, false, 'dialog expected to be open');
      env.$(ok ? 'dlgOk' : 'dlgCancel').click();
      await flush();
    },
    // Puts a live viewer document into the frame and fires load.
    loadFrame(docOpts) {
      const v = fakeViewerDoc(docOpts);
      frame.contentDocument = v.doc;
      frame.dispatch('load');
      return v;
    },
  });
  return env;
}

let passed = 0;
async function test(name, fn) {
  try {
    await fn();
    passed += 1;
    console.log('PASS ' + name);
  } catch (err) {
    console.error('FAIL ' + name + '\n' + (err && err.stack || err));
    process.exitCode = 1;
  }
}

// Brings an env to "case open, review frame loaded".
async function openReview(env, docOpts) {
  await env.advance(2000); // first status poll → review stage + frame src
  assert.match(env.frame.src || '', /evidence-viewer\.html\?embed=1/);
  const v = env.loadFrame(docOpts);
  await env.advance(10);
  return v;
}

await test('Recover/Carve hidden until a case is open; breadcrumb shows case + step', async () => {
  const env = makeEnv();
  assert.equal(env.$('btnRecoverDeleted').hidden, true);
  assert.equal(env.$('btnCarve').hidden, true);
  assert.equal(env.$('reassembleWrap').hidden, true);
  await env.advance(2000);
  assert.equal(env.$('btnRecoverDeleted').hidden, false);
  assert.equal(env.$('btnCarve').hidden, false);
  assert.match(env.$('crumbs').innerHTML, /케이스: c1/);
  assert.match(env.$('crumbs').innerHTML, /단계 4\/4/);
  env.ev('resetCaseUi()');
  assert.equal(env.$('btnRecoverDeleted').hidden, true);
});

await test('async job: progress, elapsed, completion via postMessage reload (never src=src)', async () => {
  const env = makeEnv({ routes: { 'POST /api/recover-deleted': () => ({ json: { ok: true, job: { kind: 'recover' } } }) } });
  await openReview(env);
  const srcBefore = env.frame.src, setsBefore = env.frame.srcSets;
  env.status = { ...env.status, phase: 'finalizing', busy: true, logs: ['삭제 영상 후보 복구 중… (img @ 오프셋 2048)'] };
  env.$('btnRecoverDeleted').click();
  await env.advance(1500);
  assert.equal(env.$('jobPanel').hidden, false);
  assert.match(env.$('jobTitle').textContent, /삭제 영상 후보 복구 진행 중/);
  assert.match(env.$('jobDetail').textContent, /오프셋 2048/);
  assert.equal(env.$('btnJobCancel').hidden, false);
  assert.equal(env.$('btnRecoverDeleted').disabled, true);
  assert.equal(env.$('btnCarve').disabled, true);
  await env.advance(64000);
  assert.match(env.$('jobElapsed').textContent, /^경과 1:0\d$/);
  env.status = { ...env.status, phase: 'review-ready', busy: false, logs: ['복구 완료'] };
  await env.advance(1300);
  assert.deepEqual(env.frameWin.posted.filter((m) => m.type === 'ft-reload').length, 1);
  env.winMessage({ type: 'ft-reload-ack' }, env.frameWin);
  await env.advance(10);
  assert.match(env.$('jobTitle').textContent, /완료/);
  assert.match(env.$('jobDetail').textContent, /뷰어에 결과를 반영/);
  assert.equal(env.frame.src, srcBefore);
  assert.equal(env.frame.srcSets, setsBefore, 'frame src must not be re-assigned');
  assert.equal(env.frameWin.location.reloads, 0);
  assert.equal(env.$('btnRecoverDeleted').disabled, false);
  assert.equal(env.$('btnJobCancel').hidden, true);
});

await test('no viewer ack + clean save → guarded in-frame reload', async () => {
  const env = makeEnv({ routes: { 'POST /api/recover-deleted': () => ({ json: { ok: true, job: 'j1' } }) } });
  await openReview(env);
  const srcBefore = env.frame.src, setsBefore = env.frame.srcSets;
  env.status = { ...env.status, busy: true, phase: 'finalizing' };
  env.$('btnRecoverDeleted').click();
  await env.advance(1300);
  env.status = { ...env.status, busy: false, phase: 'review-ready' };
  await env.advance(1300 + 1600);
  assert.equal(env.frameWin.location.reloads, 1);
  assert.equal(env.frame.src, srcBefore);
  assert.equal(env.frame.srcSets, setsBefore);
});

await test('no viewer ack + unsaved marks → reload deferred, save retried, manual reload later', async () => {
  const env = makeEnv({ routes: { 'POST /api/recover-deleted': () => ({ json: { ok: true, job: 'j1' } }) } });
  const v = await openReview(env, { saveState: 'pending' });
  env.status = { ...env.status, busy: true, phase: 'finalizing' };
  env.$('btnRecoverDeleted').click();
  await env.advance(1300);
  env.status = { ...env.status, busy: false, phase: 'review-ready' };
  await env.advance(1300 + 1600);
  assert.equal(env.frameWin.location.reloads, 0, 'must not reload over unsaved marks');
  assert.ok(v.saveEl.clicks >= 1, 'viewer save retried');
  assert.equal(env.$('btnJobReload').hidden, false);
  assert.match(env.$('jobPanel').className, /deferred/);
  env.$('btnJobReload').click(); // still pending → refused
  await env.flush();
  assert.equal(env.frameWin.location.reloads, 0);
  v.saveEl.dataset.state = 'saved';
  env.$('btnJobReload').click();
  await env.flush();
  assert.equal(env.frameWin.location.reloads, 1);
  assert.equal(env.$('btnJobReload').hidden, true);
});

await test('cancel: confirm → /api/cancel → cancelled outcome, buttons re-enabled', async () => {
  const env = makeEnv({ routes: { 'POST /api/carve': () => ({ json: { ok: true, job: { kind: 'carve' } } }), 'POST /api/cancel': () => ({ json: { ok: true } }) } });
  await openReview(env);
  env.status = { ...env.status, busy: true, phase: 'finalizing', logs: ['시그니처 카빙 실행 중…'] };
  env.$('btnCarve').click();
  await env.answerDialog(true); // carve confirm
  await env.advance(1300);
  env.$('btnJobCancel').click();
  await env.answerDialog(true);
  assert.ok(env.calls.some((c) => c.method === 'POST' && c.url === '/api/cancel'));
  assert.match(env.$('jobTitle').textContent, /중단 요청 중/);
  env.status = { ...env.status, busy: false, phase: 'review-ready', error: '사용자 요청으로 중단되었습니다' };
  await env.advance(1300);
  assert.match(env.$('jobTitle').textContent, /이미지 카빙 중단됨/);
  assert.match(env.$('jobPanel').className, /cancelled/);
  assert.equal(env.$('btnCarve').disabled, false);
  assert.equal(env.frameWin.posted.filter((m) => m.type === 'ft-reload').length, 0, 'no refresh after cancel');
});

await test('carve sends ?reassemble=1 when the checkbox is on', async () => {
  const env = makeEnv({ routes: { 'POST /api/carve': () => ({ json: { ok: true, job: 1 } }) } });
  await openReview(env);
  env.$('chkReassemble').checked = true;
  env.$('btnCarve').click();
  await env.answerDialog(true);
  await env.advance(10);
  assert.ok(env.calls.some((c) => c.url === '/api/carve?reassemble=1'));
});

await test('old synchronous server: elapsed + cancel while outstanding, refresh on {ok:true}', async () => {
  let resolveSync;
  const env = makeEnv({ routes: { 'POST /api/recover-deleted': () => new Promise((r) => { resolveSync = r; }) } });
  await openReview(env);
  env.status = { ...env.status, busy: true, phase: 'finalizing', logs: ['삭제 영상 후보 복구 중…'] };
  env.$('btnRecoverDeleted').click();
  await env.advance(12000);
  assert.equal(env.$('jobPanel').hidden, false);
  assert.match(env.$('jobElapsed').textContent, /^경과 0:1\d$/);
  assert.equal(env.$('btnJobCancel').hidden, false);
  assert.match(env.$('jobTitle').textContent, /진행 중/);
  env.status = { ...env.status, busy: false, phase: 'review-ready' };
  // Status says idle but the sync request is still out: not finished yet.
  await env.advance(5000);
  assert.match(env.$('jobTitle').textContent, /진행 중/);
  resolveSync({ json: { ok: true } });
  await env.advance(10);
  assert.equal(env.frameWin.posted.filter((m) => m.type === 'ft-reload').length, 1);
  env.winMessage({ type: 'ft-reload-ack' }, env.frameWin);
  await env.advance(10);
  assert.match(env.$('jobTitle').textContent, /완료/);
});

await test('old synchronous server failure → visible error, retry possible', async () => {
  const env = makeEnv({ routes: { 'POST /api/recover-deleted': () => ({ json: { ok: false, error: '파일시스템 조사 결과가 없습니다 — 먼저 E01/이미지 분석을 실행하십시오.' } }) } });
  await openReview(env);
  env.$('btnRecoverDeleted').click();
  await env.advance(10);
  assert.match(env.$('jobTitle').textContent, /실패/);
  assert.match(env.$('jobDetail').textContent, /파일시스템 조사 결과가 없습니다/);
  assert.match(env.$('recoverHint').textContent, /다시 시도/);
  assert.equal(env.$('btnRecoverDeleted').disabled, false);
  assert.equal(env.$('btnJobClose').hidden, false);
  env.$('btnJobClose').click();
  assert.equal(env.$('jobPanel').hidden, true);
});

await test('review frame: load timeout shows retry; retry reloads with a fresh URL', async () => {
  const env = makeEnv();
  await env.advance(2000);
  assert.equal(env.$('frameFail').hidden, true);
  await env.advance(20000);
  assert.equal(env.$('frameFail').hidden, false);
  assert.match(env.$('frameFailMsg').textContent, /20초/);
  env.$('btnFrameRetry').click();
  assert.match(env.frame.src, /retry=2/);
  assert.equal(env.$('frameFail').hidden, true);
  env.loadFrame();
  await env.advance(25000);
  assert.equal(env.$('frameFail').hidden, true, 'loaded frame must not time out');
});

await test('review frame: error page (no script) is reported, not left blank', async () => {
  const env = makeEnv();
  await env.advance(2000);
  env.loadFrame({ scripts: false });
  assert.equal(env.$('frameFail').hidden, false);
  assert.match(env.$('frameFailMsg').textContent, /비어 있거나 오류/);
});

await test('preflight: render (estimate, inside-source, escaping) and 404 degrade', async () => {
  const env = makeEnv();
  const html = env.ev('renderPreflight')({ sampled_bytes: 500 * 1024 ** 3, est_files: 12345, free_bytes: 1024 ** 3, case_inside_source: true, warnings: ['<b>x</b>'] }, { hash: true });
  assert.match(html, /케이스 폴더가 증거 소스 안에/);
  assert.match(html, /확인된 소스 크기: 500\.0 GB/);
  assert.match(html, /12,345/);
  assert.match(html, /SHA-256 해시 예상 소요: 약 \d+분 – 약 1시간 \d+분/);
  assert.match(html, /부족할 수 있습니다/);
  assert.match(html, /&lt;b&gt;x&lt;\/b&gt;/);
  assert.doesNotMatch(html, /<b>x<\/b>/);
  assert.match(env.ev('renderPreflight')({ sampled_bytes: 1024 ** 3 }, { hash: false }), /해시 생략/);
  env.$('sourcePath').value = '/evidence/src';
  const res = await env.ev('runPreflight()');
  assert.equal(res, null);
  assert.equal(env.ev('preflightUnsupported'), true);
  assert.equal(env.$('preflight').hidden, true);
});

await test('preflight: shown before Start; server-reported case_inside_source asks first', async () => {
  const env = makeEnv({ routes: {
    'POST /api/preflight': () => ({ json: { ok: true, sampled_bytes: 2 * 1024 ** 3, est_files: 40, free_bytes: 900 * 1024 ** 3, case_inside_source: true, warnings: [] } }),
  } });
  env.status = { ok: true, has_job: false, phase: 'idle', busy: false };
  await env.advance(1500);
  env.$('sourcePath').value = 'D:\\BLACKBOX';
  env.$('caseDir').value = 'E:\\link-into-source';
  env.ev('schedulePreflight()');
  await env.advance(700);
  assert.equal(env.$('preflight').hidden, false);
  assert.match(env.$('preflight').innerHTML, /확인된 소스 크기: 2\.0 GB/);
  env.$('btnStart').click();
  await env.flush();
  assert.equal(env.$('dlgBackdrop').hidden, false);
  assert.match(env.$('dlgTitle').textContent, /증거 소스 안에/);
  await env.answerDialog(false);
  assert.equal(env.calls.some((c) => c.url === '/api/start'), false, 'start must wait for the decision');
});

await test('case-info: N/M resume text; 404 falls back to the checkpoint probe', async () => {
  const env = makeEnv({ routes: { 'GET /api/case-info': () => ({ json: { ok: true, checkpoint: { done: 40, total: 100 }, has_review: false } }) } });
  const info = await env.ev('caseResumeInfo("/cases/c1")');
  assert.equal(info.checkpoint, true);
  assert.equal(info.done, 40);
  assert.equal(info.total, 100);
  assert.match(env.ev('resumeText')(info), /이전 분석 40\/100 진행됨 \(40%\)/);
  assert.ok(env.calls.some((c) => c.url === '/api/case-info?case_dir=%2Fcases%2Fc1'));

  // No index yet: the server reports checkpoint.total = null → "N건 진행됨"
  // without inventing a total (Number(null) would read as 0).
  const noTotal = makeEnv({ routes: { 'GET /api/case-info': () => ({ json: { ok: true, checkpoint: { done: 37, total: null }, has_review: false } }) } });
  const info3 = await noTotal.ev('caseResumeInfo("/cases/c1")');
  assert.equal(info3.checkpoint, true);
  assert.equal(info3.done, 37);
  assert.ok(Number.isNaN(info3.total));
  const text3 = noTotal.ev('resumeText')(info3);
  assert.match(text3, /이전 분석 37건 진행됨/);
  assert.doesNotMatch(text3, /\/|%/);

  const old = makeEnv({ routes: { 'GET /api/browse': (_b, url) => ({ json: { ok: true, is_file: url.includes('scan-progress'), is_case: url.endsWith('c1') } }) } });
  const info2 = await old.ev('caseResumeInfo("/cases/c1")');
  assert.equal(info2.checkpoint, true);
  assert.equal(info2.done, undefined); // no N/M from the probe
  assert.match(old.ev('resumeText')(info2), /체크포인트가 있습니다/);
  assert.equal(old.ev('caseInfoUnsupported'), true);
  // validatePaths wires it into the resume box.
  old.$('caseDir').value = '/cases/c1';
  await old.ev('validatePaths()');
  await old.flush();
  assert.equal(old.$('resumeBox').hidden, false);
  assert.match(old.$('resumeText').textContent, /체크포인트/);
});

await test('새 케이스 시작 never reloads over unsaved marks', async () => {
  const env = makeEnv();
  const v = await openReview(env, { saveState: 'pending' });
  env.frameWin.__frametraceSummary = () => ({ total: 1, pending: 0, byKind: {}, byStatus: {} });
  env.$('btnNew').click();
  await env.flush();
  assert.equal(env.$('dlgBackdrop').hidden, false);
  await env.answerDialog(true); // "저장 다시 시도"
  assert.equal(env.ctx.location.reloads, 0);
  assert.ok(v.saveEl.clicks >= 1);
  v.saveEl.dataset.state = 'saved';
  env.$('btnNew').click();
  await env.flush();
  assert.equal(env.ctx.location.reloads, 1);
  assert.equal(env.ctx.sessionStorage.getItem('ft_want_new'), '1');
});

await test('theme + viewer language live in the shell and propagate to the frame', async () => {
  const env = makeEnv();
  const v = await openReview(env);
  env.$('btnTheme').click();
  assert.ok(env.frameWin.posted.some((m) => m.type === 'ft-theme' && m.theme === 'dark'));
  assert.equal(v.doc.documentElement.dataset.theme, 'dark');
  env.$('btnViewerLang').click();
  assert.ok(env.frameWin.posted.some((m) => m.type === 'ft-locale' && m.locale === 'en'));
  assert.equal(v.langBtn.clicks, 1);
  assert.equal(v.doc.documentElement.lang, 'en');
  assert.match(env.$('btnViewerLang').textContent, /English/);
  // A reloaded frame comes back in the shell's language/theme.
  const v2 = env.loadFrame({ lang: 'ko' });
  assert.equal(v2.doc.documentElement.lang, 'en');
  assert.equal(v2.doc.documentElement.dataset.theme, 'dark');
});

await test('no uncaught errors from timers', async () => {
  const env = makeEnv();
  await env.advance(30000);
  assert.deepEqual(env.errors.map(String), []);
});

console.log(process.exitCode ? 'verify-shell-jobs: FAILED' : `verify-shell-jobs: ${passed} passed`);
