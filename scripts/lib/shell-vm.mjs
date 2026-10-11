// Shared VM environment for the workstation-shell harnesses
// (verify-shell-jobs, verify-progress). No server, no browser: the app
// bundle (assets/app/*.js, exactly as build.rs concatenates it) runs in a
// vm context with a fake DOM, a scripted fetch and virtual timers.
//
// The shell no longer hosts the viewer in an iframe (docs/ui-rebuild-
// contract.md §3): the viewer is `FTViewer`, a mock injected here
// (init/destroy/reload/saveState/save/on/off/summary/toast). The shell
// store (FTStore, T2's 01-store.js) is used from the bundle when it exists
// and replaced by a contract-shaped mock otherwise; likewise the sheet /
// chip open-close wiring (T2's 97-shell.js) is emulated when the markup
// does not have the mount nodes yet. Nothing here is committed to the app.
import vm from 'node:vm';
import assert from 'node:assert/strict';
import { existsSync } from 'node:fs';
import { join } from 'node:path';
import { appBundleJs, appMarkup, repoRoot } from './app-bundle.mjs';

export const MARKUP = appMarkup();
const HAS_STORE = existsSync(join(repoRoot, 'assets/app/01-store.js'));
const HAS_T2_MOUNTS = /id="hJobsSheet"/.test(MARKUP);

// Initial `hidden` state of every id'd element, straight from the markup.
const MARKUP_HIDDEN = new Map();
for (const m of MARKUP.matchAll(/<(\w+)([^>]*?)\bid="([^"]+)"([^>]*)>/g)) {
  const attrs = m[2] + ' ' + m[4];
  MARKUP_HIDDEN.set(m[3], /(^|\s)hidden(\s|=|$)/.test(attrs));
}
export const markupHas = (id) => MARKUP_HIDDEN.has(id);

// Contract §4 store, shaped exactly like T2's 01-store.js (mock).
const STORE_MOCK = `
const FTStore = (() => {
  const state = {
    project: { caseDir: "", name: "", packageDir: "", restored: false, sourcePath: "" },
    phase: "empty",
    jobs: { pipeline: { steps: [], stepNames: [], current: "", progress: null, enrichment: null, warnings: [], logs: [], busy: false, error: "" },
      caseJob: null, clipJobs: [], clipsRunning: 0, pendingMarks: 0, jobKind: null },
    counts: { indexed: 0, total: 0, generation: 0 },
    panels: { jobs: false, finalize: false, project: false, tools: false, help: false },
    popout: { open: false, writer: "main" },
    viewer: { mounted: false, saveState: null, dirty: 0, selection: 0, activeId: null, locale: "ko", theme: "light" },
    server: { instance: "", lost: false, failures: 0 },
    env: null,
  };
  const fns = new Set();
  const api = {
    state,
    get(path) { return String(path).split(".").reduce((o, k) => (o == null ? o : o[k]), state); },
    set(patch) { const keys = new Set(Object.keys(patch)); Object.assign(state, patch); for (const f of [...fns]) f({ keys }); },
    on(ev, fn) { if (ev === "change") fns.add(fn); },
    off(ev, fn) { fns.delete(fn); },
    isBusy() { const p = state.jobs.pipeline; return !!(p.busy || (state.jobs.caseJob && !state.jobs.caseJob.outcome) || state.jobs.clipsRunning > 0); },
    hasProject() { return !!state.project.caseDir; },
    canFinalize() { return ["review-ready", "done", "finalized"].includes(state.phase) && !api.isBusy(); },
  };
  return api;
})();
`;

// Symbols the stage UI used to define in 40-status.js and that T3 removed
// (docs §1.2: showView/setStep/crumbs deleted). Other tracks' files (T2:
// 50-start/55-home/95-quit, T4: 70-finalize) still reference them until
// their rewrites land; the VM declares a no-op for any that the bundle uses
// but no longer defines, so this harness runs on a partially merged tree.
// After all tracks merged nothing is referenced and this adds nothing.
function removedStageShims(bundle) {
  const fns = ['showView', 'setStep', 'renderCrumbs', 'syncViewerBlock', 'openBoundViewer', 'enableReviewStage', 'ensureReportFrame', 'ensureReviewFrame'];
  const vars = { reviewReady: 'false', jobSeen: 'false', autoReviewed: 'false', autoProgress: 'false', homeRestored: '""', restoreHandled: 'false', lastPhase: '""', stepActive: '1', stepDone: '0', viewShown: '"1"', reviewPartialSeen: 'false' };
  const defined = (n) => new RegExp(`\\b(?:let|const|var|function)\\s+${n}\\b`).test(bundle);
  const used = (n) => new RegExp(`\\b${n}\\b`).test(bundle);
  let out = '';
  for (const n of fns) if (used(n) && !defined(n)) out += `function ${n}() {}\n`;
  for (const [n, v] of Object.entries(vars)) if (used(n) && !defined(n)) out += `let ${n} = ${v};\n`;
  return out;
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
    click() { if (el.disabled) return; el.clicks += 1; el.dispatch('click'); },
    setAttribute(k, v) { el.attrs[k] = String(v); },
    getAttribute(k) { return el.attrs[k] ?? null; },
    removeAttribute(k) { delete el.attrs[k]; },
    appendChild(c) { el.children.push(c); if (c && typeof c.text === 'string') el.textContent += c.text; return c; },
    append(...c) { el.children.push(...c); },
    remove() {}, focus() {}, select() {}, scrollIntoView() {},
    closest: () => null, querySelectorAll: () => [], querySelector: () => null, contains: () => false,
  };
  return el;
}

export function makeEnv(opts = {}) {
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
    status: { ok: true, has_job: true, phase: 'review-ready', busy: false, case_dir: '/cases/c1', steps: ['done', 'done', 'done', 'done', 'done'], logs: [], error: null, ...(opts.status || {}) },
    routes: opts.routes || {},
    calls: [],
    recordsTotal: 0,
    statusCalls: () => env.calls.filter((c) => c.url === '/api/status').length,
  };
  const respond = (status, json) => ({ ok: status < 400, status, json: async () => { if (json === undefined) throw new Error('not json'); return json; } });
  async function fetchImpl(url, init = {}) {
    const method = (init.method || 'GET').toUpperCase();
    const body = init.body ? JSON.parse(init.body) : null;
    env.calls.push({ method, url, body, at: clock });
    const path = url.split('?')[0];
    const route = env.routes[`${method} ${url}`] || env.routes[`${method} ${path}`];
    if (route) {
      const r = await route(body, url);
      return respond(r.status ?? 200, r.json);
    }
    if (path === '/api/status') return respond(200, JSON.parse(JSON.stringify(env.status)));
    if (path === '/api/records') return respond(200, { ok: true, total: env.recordsTotal, videos: [] });
    if (path === '/api/env') return respond(200, { ok: true, tools: { ffmpeg: true, ffprobe: true }, workflows: {}, hints: {} });
    if (path === '/api/browse') return respond(200, { ok: true, exists: false });
    return respond(404, undefined);
  }

  const els = {};
  const winListeners = {};
  const docListeners = {};
  const store = () => { const m = new Map(); return { getItem: (k) => (m.has(k) ? m.get(k) : null), setItem: (k, v) => m.set(k, String(v)), removeItem: (k) => m.delete(k) }; };
  const document = {
    getElementById: (id) => (els[id] ||= makeEl(id)),
    querySelectorAll: () => [],
    querySelector: () => null,
    createElement: (tag) => makeEl('', tag),
    createTextNode: (t) => ({ text: t }),
    addEventListener(t, fn) { (docListeners[t] ||= []).push(fn); },
    documentElement: { dataset: { theme: 'light' } },
    activeElement: null,
    body: makeEl('body'),
  };

  // FTViewer mock (docs §3): only what the shell may call.
  const viewerEvents = {};
  const viewer = {
    reloadResult: 'reloaded', // 'reloaded' | 'refused'
    reloadCalls: [],
    saved: 'saved',
    saveCalls: 0,
    toasts: [],
  };
  const FTViewer = {
    version: 2,
    init: async () => {}, destroy() {},
    reload: async (o = {}) => { viewer.reloadCalls.push(o); return viewer.reloadResult; },
    saveState: () => viewer.saved,
    save: async () => { viewer.saveCalls += 1; return true; },
    summary: () => ({ total: 0, byKind: {}, byStatus: {}, warned: 0, marked: 0, noted: 0, pending: 0 }),
    toast: (m, k) => viewer.toasts.push([m, k]),
    on(ev, fn) { (viewerEvents[ev] ||= []).push(fn); },
    off(ev, fn) { viewerEvents[ev] = (viewerEvents[ev] || []).filter((f) => f !== fn); },
  };
  viewer.emit = (ev, detail) => { for (const fn of [...(viewerEvents[ev] || [])]) fn({ type: ev, detail }); };

  const ctx = {
    document, console: { log() {}, error: (...a) => env.consoleErrors.push(a.join(' ')), warn() {} },
    fetch: fetchImpl, setTimeout: sT, clearTimeout: cT, setInterval: sI, clearInterval: cT,
    Date: FakeDate, localStorage: store(), sessionStorage: store(), navigator: {},
    location: { protocol: 'http:', origin: 'http://127.0.0.1', reloads: 0, reload() { this.reloads += 1; } },
    Event: class Event { constructor(type) { this.type = type; } },
    addEventListener(t, fn) { (winListeners[t] ||= []).push(fn); },
    removeEventListener(t, fn) { winListeners[t] = (winListeners[t] || []).filter((f) => f !== fn); },
    open: () => ({ focus() {} }),
    Blob: class Blob { constructor(p) { this.parts = p; } }, URL: { createObjectURL: () => 'blob:x', revokeObjectURL() {} },
    URLSearchParams, encodeURIComponent, JSON, Math, Number, String, Object, Array, Promise, Set, Map, RegExp, Error,
    FTViewer,
  };
  ctx.window = ctx;
  env.consoleErrors = [];
  vm.createContext(ctx);
  const app = appBundleJs();
  const bundle = (HAS_STORE ? '' : STORE_MOCK) + removedStageShims(app) + app;
  vm.runInContext(bundle, ctx);

  const ev = (expr) => vm.runInContext(expr, ctx);
  Object.assign(env, {
    ctx, els, viewer, advance, flush, errors,
    $: (id) => document.getElementById(id),
    ev,
    store: () => ev('FTStore'),
    state: () => ev('FTStore').state,
    docKey: (key) => { const e = { type: 'keydown', key, prevented: false, preventDefault() { this.prevented = true; } }; for (const fn of [...(docListeners.keydown || [])]) fn(e); return e; },
    async answerDialog(ok) {
      await flush();
      assert.equal(env.$('dlgBackdrop').hidden, false, 'dialog expected to be open');
      env.$(ok ? 'dlgOk' : 'dlgCancel').click();
      await flush();
    },
  });
  // T2 owns the open/close wiring of the sheet (97-shell.js, panels.jobs);
  // until its mount nodes exist the harness stands in for it.
  if (!HAS_T2_MOUNTS) {
    const toggle = (on) => { const S = env.state(); env.store().set({ panels: { ...S.panels, jobs: on === undefined ? !S.panels.jobs : on } }); };
    env.$('hBtnJobs').addEventListener('click', () => toggle());
    env.$('hJobsClose').addEventListener('click', () => toggle(false));
  } else {
    // The fake DOM does not bubble: the sheets close by delegation (a click
    // on the sheet whose target is inside its close button), so forward it.
    for (const [sheet, close] of [['hJobsSheet', 'hJobsClose'], ['hFinalizeSheet', 'hFinalizeClose']]) {
      env.$(close).addEventListener('click', () => env.$(sheet).dispatch('click', { target: { closest: () => env.$(close) } }));
    }
  }
  return env;
}

let passed = 0;
export async function test(name, fn) {
  try {
    await fn();
    passed += 1;
    console.log('PASS ' + name);
  } catch (err) {
    console.error('FAIL ' + name + '\n' + (err && err.stack || err));
    process.exitCode = 1;
  }
}
export const passedCount = () => passed;
export { assert };
