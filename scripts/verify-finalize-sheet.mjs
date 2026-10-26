// Deliverables (결과물) right sheet — assets/app/41-finalize.js + 40-finalize.css
// (docs/ui-rebuild-contract.md 8.2, track T4). Fixture harness: the real shell
// scripts (10-core, 20-net, 41-finalize) run in headless Chromium against a
// minimal page holding the empty mount node <div id="hFinalizeSheet"> that the
// host layout (T2) provides, with every /api/* answered by a scripted route and
// FTViewer replaced by a stub. No server, no build. When the real shell page
// already carries the mount (T2 landed) the inert/Esc contract is checked on
// that page too.
//
//   node scripts/verify-finalize-sheet.mjs
//
// Needs Playwright (NODE_PATH) with Chromium.
import assert from 'node:assert/strict';
import { readFileSync } from 'node:fs';
import { join } from 'node:path';
import { createRequire } from 'node:module';
import { repoRoot, appPageHtml, viewerBundleCss } from './lib/app-bundle.mjs';

let chromium;
try { ({ chromium } = createRequire(import.meta.url)('playwright')); } catch {
  console.error('playwright is not resolvable (set NODE_PATH to a node_modules containing it)');
  process.exit(1);
}
const read = (rel) => readFileSync(join(repoRoot, rel), 'utf8');

// ---- fixture page --------------------------------------------------------
const tokens = read('assets/tokens.css');
const kit = read('assets/kit.css');
const finCss = read('assets/app/40-finalize.css');
const T1_LANDED = /--mark-reviewed-ink\s*:/.test(tokens);
const KIT_LANDED = /\.primary[^{]*\{/.test(kit.replace(/\/\*[^]*?\*\//g, ''));
// Contract 5.1 values for every token the sheet uses — only injected while
// T1's tokens.css has not landed yet, so the AA checks run on contract colours.
const TOKEN_SHIM = T1_LANDED ? '' : `
:root{--bg:#ecebe7;--surface:#fbfaf7;--surface-2:#f3f2ee;--text:#1a1c1e;--muted:#55595e;--border:#d6d3cc;--border-strong:#858177;--accent:#234e8c;--accent-ink:#1c3f73;--ok:#2a6b3a;--warn:#7a5200;--info:#234e8c;--verified-fg:#2a6b3a;--verified-bg:#e4f1e7;--warn-fg:#7a5200;--warn-bg:#fbf1dc;--focus-ring:2px solid #234e8c;--fs-xs:13px;--fs-sm:14px;--fs-md:16px;--radius:3px;--sp-1:4px;--sp-2:8px;--sp-3:12px;--sp-4:16px;--lh-body:1.5;--lh-ctl:1.35;--font-mono:ui-monospace,Menlo,monospace}
html[data-theme="dark"]{--bg:#121416;--surface:#1b1e21;--surface-2:#24282c;--text:#e6e4df;--muted:#a7acb2;--border:#33383d;--border-strong:#767d84;--accent:#8db4ea;--accent-ink:#a9c7f2;--ok:#7fcb95;--warn:#e8b45a;--info:#8db4ea;--verified-fg:#7fcb95;--verified-bg:#17352a;--warn-fg:#e8b45a;--warn-bg:#3a2e1a;--focus-ring:2px solid #8db4ea}
.sr-only{position:absolute;width:1px;height:1px;overflow:hidden;clip:rect(0 0 0 0);white-space:nowrap}`;
const SCRIPT = ['assets/app/10-core.js', 'assets/app/20-net.js', 'assets/app/41-finalize.js'].map(read).join('\n');
const PREFS_STUB = `
function loadPrefs() { try { return JSON.parse(localStorage.getItem("ft.examiner.prefs") || "{}") || {}; } catch { return {}; } }
function savePrefs(p) { const v = Object.assign(loadPrefs(), p); try { localStorage.setItem("ft.examiner.prefs", JSON.stringify(v)); } catch {} }
`;
const fixtureHtml = `<!doctype html><html lang="ko" data-theme="light"><head><meta charset="utf-8"><title>fixture</title>
<style>${tokens}\n${TOKEN_SHIM}\n${kit}\n${finCss}
body{margin:0;background:var(--bg);color:var(--text);font-family:system-ui,sans-serif}
.h-sheet{position:fixed;top:44px;right:0;bottom:0;width:480px;background:var(--surface);box-shadow:-8px 0 24px rgba(0,0,0,.12)}
.h-sheet[hidden]{display:none}
@media (min-width:1600px){.h-sheet{width:560px}}
.h-modal-backdrop[hidden]{display:none}
</style></head><body>
<main id="main"><p>그리드(가짜)</p></main>
<div id="toastBox"></div><div id="srStatus"></div><div id="srAlert"></div>
<div id="dlgBackdrop" class="h-modal-backdrop" hidden><div><h3 id="dlgTitle"></h3><div id="dlgBody"></div><p id="dlgNote"></p><button id="dlgOk"></button><button id="dlgCancel"></button></div></div>
<div id="hFinalizeSheet" class="h-sheet" role="dialog" aria-modal="true" aria-labelledby="hFinalizeTitle" hidden></div>
<script>${PREFS_STUB}\n${SCRIPT}</script></body></html>`;

// ---- scripted server -----------------------------------------------------
const freshState = () => ({
  status: { ok: true, phase: 'review-ready', busy: false, pending_marks: 0, case_dir: '/case/demo' },
  meta: { manifest: { case_id: 'CASE-1' }, annotations: { marks: [
    { id: 'a', status: 'reviewed', examiner: 'kim' }, { id: 'b', status: 'needs_verification', examiner: 'kim' },
    { id: 'c', status: 'needs_verification', examiner: 'kim' }, { id: 'd', status: 'important', note: 'x', examiner: 'kim' },
  ] } },
  env: { ok: true, deepfake: false },
  audit: { ok: true, integrity: 'integrity-keyed', verified: 2, entries: 14, logs: [] },
  verifySources: { ok: true, matched: 3, mismatched: 0, not_comparable: 1 },
  finalize: { ok: true, examiner: 'Hong', manifest_sha256: 'a'.repeat(64), package_dir: '/case/package-1', package_url: 'package-1/' },
  posts: [],
});
let S = freshState();
async function newPage(browser, { width = 1366, height = 768, theme = 'light' } = {}) {
  S = freshState();
  // Reduced motion: contrast is sampled on settled colours, not mid-transition.
  const ctx = await browser.newContext({ viewport: { width, height }, reducedMotion: 'reduce' });
  const page = await ctx.newPage();
  const errors = [];
  page.on('pageerror', (e) => errors.push(String(e)));
  await page.route('http://ft.test/**', async (route) => {
    const url = new URL(route.request().url());
    const json = (o) => route.fulfill({ status: 200, contentType: 'application/json', body: JSON.stringify(o) });
    if (url.pathname === '/') return route.fulfill({ status: 200, contentType: 'text/html', body: fixtureHtml });
    if (route.request().method() === 'POST') S.posts.push({ path: url.pathname, body: route.request().postDataJSON() });
    switch (url.pathname) {
      case '/api/status': return json(S.status);
      case '/api/records-meta': return json(S.meta);
      case '/api/env': return json(S.env);
      case '/api/verify-audit': return json(S.audit);
      case '/api/verify-sources': return json(S.verifySources);
      case '/api/finalize': return json(S.finalize);
      case '/api/advanced': return json({ ok: true, output: 'done', url: 'reports/x.json' });
      case '/api/open-folder': return json({ ok: true });
      default: return route.fulfill({ status: 404, body: '{}' });
    }
  });
  await page.addInitScript(() => {
    window.__v = { save: 'saved', saves: 0, total: 10, marked: 4, listeners: {} };
    window.FTViewer = {
      saveState: () => window.__v.save,
      summary: () => ({ total: window.__v.total, marked: window.__v.marked, byKind: {}, byStatus: {} }),
      save: async () => { window.__v.saves++; window.__v.save = 'saved'; return true; },
      on: (ev, fn) => { (window.__v.listeners[ev] ||= []).push(fn); }, off() {},
    };
    // Draft/pref storage is per-context; nothing to seed.
  });
  await page.goto('http://ft.test/');
  await page.evaluate((th) => { document.documentElement.dataset.theme = th; }, theme);
  page.__errors = errors;
  return page;
}
const openSheet = async (page) => {
  await page.evaluate(() => { document.getElementById('hFinalizeSheet').hidden = false; });
  await page.waitForFunction(() => !document.getElementById('finalVerdict')?.className.includes('loading') && document.querySelectorAll('#finalChecklist li').length > 0);
};
const rows = (page) => page.$$eval('#finalChecklist li', (ls) => ls.map((l) => ({ level: l.dataset.level, text: l.textContent.replace(/\s+/g, ' ').trim() })));
const btnState = (page) => page.$eval('#btnFinalize', (b) => ({ disabled: b.disabled, title: b.title }));

// ---- contrast ------------------------------------------------------------
const lum = ([r, g, b]) => { const f = (c) => { c /= 255; return c <= 0.03928 ? c / 12.92 : ((c + 0.055) / 1.055) ** 2.4; }; return 0.2126 * f(r) + 0.7152 * f(g) + 0.0722 * f(b); };
const ratio = (a, b) => { const [x, y] = [lum(a), lum(b)].sort((p, q) => q - p); return (x + 0.05) / (y + 0.05); };
async function contrastReport(page) {
  return page.evaluate(() => {
    const parse = (s) => { const m = s.match(/rgba?\(([^)]+)\)/); const p = m[1].split(',').map(Number); return { rgb: p.slice(0, 3), a: p.length > 3 ? p[3] : 1 }; };
    const bgOf = (el) => {
      for (let e = el; e; e = e.parentElement) { const c = parse(getComputedStyle(e).backgroundColor); if (c.a > 0.5) return c.rgb; }
      return [255, 255, 255];
    };
    const out = [];
    for (const el of document.querySelectorAll('#hFinalizeSheet *')) {
      if (!el.childNodes.length || ![...el.childNodes].some((n) => n.nodeType === 3 && n.textContent.trim())) continue;
      const cs = getComputedStyle(el);
      if (cs.visibility === 'hidden' || el.offsetParent === null) continue;
      if (el.closest('button:disabled')) continue; // disabled controls are exempt (>= 3 is verify-buttons' job)
      out.push({ text: el.textContent.trim().slice(0, 30), fg: parse(cs.color).rgb, bg: bgOf(el), cls: el.className || el.tagName });
    }
    return out;
  });
}

// ---- run -----------------------------------------------------------------
let browser;
try { browser = await chromium.launch({ channel: 'chrome' }); } catch { browser = await chromium.launch(); }
let failed = 0;
const check = async (name, fn) => {
  try { await fn(); console.log('PASS', name); } catch (e) { failed++; console.error('FAIL', name, '\n  ', e.message.split('\n').join('\n   ')); }
};
const skip = (name, why) => console.log('SKIP', name, '—', why);

// ① pure checklist rows + ② block rows disable with a reason
await check('1 pure rows: unsavedMarksChecks / verifySourcesVerdict / finSummarize', async () => {
  const page = await newPage(browser);
  const r = await page.evaluate(() => ({
    saved: unsavedMarksChecks({ pendingMarks: 0, saveState: 'saved', drafts: 0 }).map((c) => c.level),
    failed: unsavedMarksChecks({ pendingMarks: 0, saveState: 'failed', drafts: 1 }).map((c) => [c.level, c.retry]),
    queue: unsavedMarksChecks({ pendingMarks: 2, saveState: 'saved', drafts: 0 }).map((c) => c.level),
    verdictNone: verifySourcesVerdict({ matched: 0, mismatched: 0, not_comparable: 3 }).level,
    verdictAll: verifySourcesVerdict({ matched: 2, mismatched: 0, not_comparable: 0 }).level,
    sumBlocked: finSummarize([{ level: 'block', text: 'T', sub: 'S' }, { level: 'warn', text: 'w' }], { name: 'x' }),
    sumNoName: finSummarize([{ level: 'ok', text: 'ok' }], { name: '  ' }),
    sumReady: finSummarize([{ level: 'ok', text: 'ok' }, { level: 'warn', text: 'w' }], { name: 'x' }),
    // R8 (track I ↔ U): /api/verify-audit is green ONLY when authenticated here.
    auKeyed: auditVerdict({ ok: true, integrity: 'integrity-keyed', verified: 1, entries: 3, logs: [{ name: 'a.jsonl', integrity: 'integrity-keyed', entries: 3, trust: '' }] }),
    auSelf: auditVerdict({ ok: true, integrity: 'integrity-structural-only', verified: 1, entries: 3, logs: [{ name: 'a.jsonl', integrity: 'integrity-structural-only', entries: 3, trust: '자기 일관성만 확인(인증 아님)', keys_held: false, other_pc_key_ids: [] }] }),
    auOther: auditVerdict({ ok: true, integrity: 'integrity-not-verified', verified: 1, entries: 3, logs: [{ name: 'a.jsonl', integrity: 'not-verified', entries: 3, trust: '키 pc-b 보유 PC에서 확인', keys_held: true, other_pc_key_ids: ['pc-b'] }] }),
    auFail: auditVerdict({ ok: false, error: 'chain broken' }),
    // R9 U3/U4: the server's own reason texts (src/audit.rs lane_trust).
    auOnlyOther: auditVerdict({ ok: true, integrity: 'integrity-not-verified', verified: 1, entries: 3, logs: [{ name: 'a.jsonl', integrity: 'not-verified', entries: 3, trust: '이 PC에서 인증되지 않은 줄 2개 (전체 3개 중) — NOT VERIFIED; 다른 PC 키로 서명된 줄 2개 — 이 PC에서 인증 불가, 키 "pc-b" 보유 PC에서 확인 (변조 판정 아님)', keys_held: true, other_pc_key_ids: ['pc-b'] }] }),
    auMixed: auditVerdict({ ok: true, integrity: 'integrity-not-verified', verified: 2, entries: 9, logs: [
      { name: 'a.jsonl', integrity: 'not-verified', entries: 5, trust: '이 PC에서 인증되지 않은 줄 3개 (전체 5개 중) — NOT VERIFIED; HMAC 없는 줄 1개 (HMAC이 제거됐거나 키 없이 추가됐을 수 있음); 다른 PC 키로 서명된 줄 2개 — 이 PC에서 인증 불가, 키 "pc-b" 보유 PC에서 확인 (변조 판정 아님)', keys_held: true, other_pc_key_ids: ['pc-b'] },
      { name: 'b.jsonl', integrity: 'not-verified', entries: 4, trust: '…; 은퇴한 키로만 서명되고 이후 현재 키 항목이 없는 줄 2개', keys_held: true, other_pc_key_ids: [] }] }),
    auNumeric: auditVerdict({ ok: true, integrity: 'integrity-not-verified', verified: 1, entries: 3, logs: [{ name: 'a.jsonl', integrity: 'not-verified', entries: 3, trust: '', unauthenticated_lines: 3, other_pc_lines: 0, keys_held: true, other_pc_key_ids: ['pc-c'] }] }),
    auRewrite: auditVerdict({ ok: true, integrity: 'integrity-not-verified', verified: 1, entries: 3, logs: [{ name: 'a.jsonl', integrity: 'not-verified', entries: 3, trust: '다른 PC 키로 서명된 줄 1개', rewrite_not_excluded: 'rewrite not excluded', keys_held: true, other_pc_key_ids: ['pc-b'] }] }),
    // R10 U2: an unreadable key configuration outranks every other state, in its own words.
    auKeyCfg: auditVerdict({ ok: true, integrity: 'integrity-not-verified', verified: 2, entries: 6, logs: [
      { name: 'a.jsonl', integrity: 'not-verified', entries: 3, unauthenticated_lines: 3, other_pc_lines: 0, trust: '' },
      { name: 'b.jsonl', integrity: 'not-verified', entries: 3, key_config_error: true, trust: '키 설정을 읽지 못함: 폴더입니다' }] }),
    auUnknownState: auditVerdict({ ok: true, integrity: 'integrity-something-new', verified: 1, entries: 3, logs: [{ name: 'a.jsonl', integrity: 'x', entries: 3, other_pc_key_ids: ['pc-d'] }] }),
    // R9 U2: the read-only row names its cause.
    roCase: unsavedMarksChecks({ pendingMarks: 0, saveState: 'readonly', drafts: 0, lockCause: 'case' }).map((c) => c.text).join(' | '),
    roAnn: unsavedMarksChecks({ pendingMarks: 0, saveState: 'readonly', drafts: 0, lockCause: 'annotations' }).map((c) => c.text).join(' | '),
    lockFromMeta: typeof finLockCause === 'function' ? [finLockCause(null, { ok: true, manifest: null }), finLockCause({ lockCause: 'annotations' }, { ok: true, manifest: null }), finLockCause(null, { ok: true, manifest: { case_id: 'c' } })] : null,
  }));
  assert.equal(r.auKeyed.level, 'ok');
  assert.ok(r.auSelf.level === 'warn' && /자기 일관성만 확인\(인증 아님\)/.test(r.auSelf.text), JSON.stringify(r.auSelf));
  // R9 U3: another PC's key is never a failure: "이 PC에서 인증 불가(변조 판정 아님) — 키 <id> 보유 PC에서 확인".
  assert.ok(r.auOnlyOther.level === 'warn' && /이 PC에서 인증 불가\(변조 판정 아님\) — 키 pc-b 보유 PC에서 확인/.test(r.auOnlyOther.text) && /pc-b/.test(r.auOnlyOther.title), 'R9 U3: ' + JSON.stringify(r.auOnlyOther));
  // R9 U4: unsigned / retired-key lines come first; the other-PC hint only when it is the ONLY cause.
  assert.ok(r.auMixed.level === 'warn' && /인증 안 된 줄 3개/.test(r.auMixed.text) && !/보유 PC/.test(r.auMixed.text), 'R9 U4 mixed: ' + JSON.stringify(r.auMixed));
  assert.ok(/인증 안 된 줄 3개/.test(r.auNumeric.text) && !/pc-c/.test(r.auNumeric.text), 'R9 U4 numeric fields: ' + JSON.stringify(r.auNumeric));
  assert.ok(!/보유 PC/.test(r.auRewrite.text) && !/인증 안 된 줄/.test(r.auRewrite.text) && /인증되지 않음/.test(r.auRewrite.text), 'R9 U4 another cause too: ' + JSON.stringify(r.auRewrite));
  assert.ok(r.auKeyCfg.level === 'warn' && /^감사 키 설정을 읽지 못함 — 인증 불가\(변조 판정 아님\)$/.test(r.auKeyCfg.text) && /b\.jsonl: 키 설정을 읽지 못함: 폴더입니다/.test(r.auKeyCfg.title), 'R10 U2: ' + JSON.stringify(r.auKeyCfg));
  assert.ok(r.auUnknownState.level === 'warn' && /키 pc-d 보유 PC에서 확인/.test(r.auUnknownState.text), 'R9 U3 unknown state names the key too: ' + JSON.stringify(r.auUnknownState));
  assert.ok(/case\.json/.test(r.roCase) && !/판독 기록을 읽지 못했습니다/.test(r.roCase) && /판독 기록을 읽지 못했습니다/.test(r.roAnn), 'R9 U2 finalize read-only row by cause: ' + r.roCase + ' || ' + r.roAnn);
  assert.deepEqual(r.lockFromMeta, ['case', 'annotations', ''], 'R9 U2 lock cause: the viewer\'s first, else records-meta');
  assert.equal(r.auFail.level, 'bad');
  assert.deepEqual(r.saved, ['ok']);
  assert.deepEqual(r.failed, [['block', true]]);
  assert.deepEqual(r.queue, ['block']);
  assert.equal(r.verdictNone, 'warn');
  assert.equal(r.verdictAll, 'ok');
  assert.equal(r.sumBlocked.disabled, true);
  assert.match(r.sumBlocked.reason, /^T — S$/);
  assert.equal(r.sumNoName.reason, '검토자 이름을 입력하십시오');
  assert.equal(r.sumReady.disabled, false);
  assert.equal(r.sumReady.state, 'ready');
  await page.context().close();
});

await check('2 checklist: 미검토 · 검증 대기 · 검토자 · 원본 재검증 · 감사 로그, exactly one primary', async () => {
  const page = await newPage(browser);
  await page.evaluate(() => savePrefs({ examinerName: 'Hong' }));
  await openSheet(page);
  const rs = await rows(page);
  const has = (re, level) => rs.some((r) => re.test(r.text) && (!level || r.level === level));
  assert.ok(has(/검증 대기 2건/, 'warn'), JSON.stringify(rs));
  assert.ok(has(/미검토 6건 \(전체 10건\)/, 'warn'), JSON.stringify(rs));
  assert.ok(has(/원본 재검증: 아직 실행하지 않음/, 'info'), 'verification is not claimed');
  assert.ok(has(/감사 로그 인증됨/, 'ok'));
  assert.ok(has(/마크·메모가 모두 케이스에 반영되었습니다/, 'ok'));
  assert.equal(await page.inputValue('#finalExaminer'), 'Hong', 'examiner prefilled from prefs');
  assert.ok(/보고서를 낼 수 있습니다 — 확인할 항목 \d+건/.test(await page.textContent('#finalVerdict')));
  const primaries = await page.$$eval('#hFinalizeSheet button.primary', (bs) => bs.filter((b) => b.offsetParent).map((b) => b.id));
  assert.deepEqual(primaries, ['btnFinalize']);
  assert.equal((await page.textContent('#btnFinalize')).trim(), '보고서 생성·패키징');
  assert.deepEqual(page.__errors, []);
  await page.context().close();
});

await check('2b block rows disable btnFinalize with a reason tooltip (보강 중 / 저장 대기 / 미저장)', async () => {
  const page = await newPage(browser);
  await page.evaluate(() => savePrefs({ examinerName: 'Hong' }));
  S.status = { ok: true, phase: 'review-partial', busy: true, enrichment: { label: '해시', stage: 2, stages: 5 }, pending_marks: 0 };
  await openSheet(page);
  let rs = await rows(page);
  assert.equal(rs[0].level, 'block');
  assert.match(rs[0].text, /분석 보강 중/);
  let b = await btnState(page);
  assert.equal(b.disabled, true);
  assert.match(b.title, /분석 보강 중/);
  assert.match(await page.textContent('#finalVerdict'), /보고서를 낼 수 없습니다 — 해결할 항목 1건/);
  // enrichment done, but the server still queues marks
  S.status = { ok: true, phase: 'review-ready', busy: false, pending_marks: 3 };
  await page.click('#btnFinalRecheck');
  await page.waitForFunction(() => /저장 대기 마크 3건/.test(document.getElementById('finalChecklist').textContent));
  b = await btnState(page);
  assert.equal(b.disabled, true);
  assert.match(b.title, /저장 대기 마크 3건/);
  // viewer has unsaved changes -> block with retry; retry saves and unblocks
  S.status.pending_marks = 0;
  await page.evaluate(() => { window.__v.save = 'pending'; });
  await page.click('#btnFinalRecheck');
  await page.waitForFunction(() => !document.getElementById('btnFinalRetrySave').hidden);
  assert.equal((await btnState(page)).disabled, true);
  await page.click('#btnFinalRetrySave');
  await page.waitForFunction(() => document.getElementById('btnFinalRetrySave').hidden && !document.getElementById('btnFinalize').disabled);
  assert.equal(await page.evaluate(() => window.__v.saves), 1);
  await page.context().close();
});

await check('3 empty examiner disables, typing enables', async () => {
  const page = await newPage(browser);
  await openSheet(page);
  assert.equal(await page.inputValue('#finalExaminer'), '');
  let b = await btnState(page);
  assert.equal(b.disabled, true);
  assert.match(b.title, /검토자 이름이 필요합니다/);
  await page.fill('#finalExaminer', 'Hong');
  b = await btnState(page);
  assert.equal(b.disabled, false);
  await page.fill('#finalExaminer', '   ');
  assert.equal((await btnState(page)).disabled, true);
  await page.context().close();
});

await check('4 finalize POSTs once with {examiner}; result links open in a new tab; no report iframe', async () => {
  const page = await newPage(browser);
  await openSheet(page);
  await page.fill('#finalExaminer', 'Hong');
  await page.dblclick('#btnFinalize'); // a double click must not double-seal
  await page.waitForSelector('#finalLinks a[href$="reports/case-report.html"]');
  const posts = S.posts.filter((p) => p.path === '/api/finalize');
  assert.equal(posts.length, 1, 'exactly one /api/finalize');
  assert.deepEqual(posts[0].body, { examiner: 'Hong' });
  const link = await page.$eval('#finalLinks a[href$="reports/case-report.html"]', (a) => ({ t: a.target, rel: a.rel }));
  assert.equal(link.t, '_blank');
  assert.match(link.rel, /noopener/);
  const text = await page.textContent('#finalLinks');
  assert.ok(text.includes('/case/package-1'), 'package path shown');
  assert.ok(text.includes('a'.repeat(64)), 'manifest SHA-256 shown');
  assert.equal(await page.$('#reportFrame'), null);
  assert.equal(await page.$('iframe'), null);
  assert.match(await page.textContent('#finalVerdict'), /보고서·패키지를 만들었습니다/);
  // never claims source verification that did not run
  assert.ok(!/원본 재검증: 모두 일치/.test(await page.textContent('#finalChecklist')));
  assert.ok(await page.evaluate(() => JSON.parse(localStorage.getItem('ft.viewer.CASE-1.examiner')) === 'Hong'));
  await page.context().close();
});

// R10 U4 (track S2): a package published by a finalize that did not complete
// ("<package>_INCOMPLETE", marker beside it) reads 최종화 미완료.
await check('4c R10 U4: a failed finalize that left a package says 최종화 미완료; an _INCOMPLETE package is marked', async () => {
  const page = await newPage(browser);
  await openSheet(page);
  await page.fill('#finalExaminer', 'Hong');
  S.finalize = { ok: false, error: '검토자 기록 확정 실패: busy — 패키지는 이미 C:\\c\\reports\\package_001_INCOMPLETE에 만들어졌지만 최종화는 끝나지 않았습니다(최종화 미완료).' };
  await page.click('#btnFinalize');
  await page.waitForFunction(() => /실패/.test(document.getElementById('finalHint').textContent));
  const links = await page.textContent('#finalLinks');
  assert.ok(/최종화 미완료/.test(links) && /최종 결과로 쓰지 마십시오/.test(links), 'incomplete row: ' + links);
  // Explicit field + success answer with an _INCOMPLETE path (a server that names it).
  S.finalize = { ok: true, examiner: 'Hong', manifest_sha256: 'b'.repeat(64), package_dir: 'C:/c/reports/package_002_INCOMPLETE', package_url: 'reports/package_002_INCOMPLETE/' };
  await page.click('#btnFinalize');
  await page.waitForFunction(() => /package_002_INCOMPLETE/.test(document.getElementById('finalLinks').textContent));
  const r = await page.evaluate(() => ({ badge: document.querySelector('#finalLinks .h-fin-incomplete')?.textContent, verdict: document.getElementById('finalVerdict').textContent }));
  assert.ok(r.badge === '최종화 미완료' && /최종화 미완료/.test(r.verdict) && !/만들었습니다$/.test(r.verdict.trim()), JSON.stringify(r));
  for (const th of ['light', 'dark']) {
    await page.evaluate((t) => { document.documentElement.dataset.theme = t; }, th);
    const label = (await contrastReport(page)).find((e) => /h-fin-incomplete/.test(e.cls));
    assert.ok(label && ratio(label.fg, label.bg) >= 4.5, `${th}: 최종화 미완료 label contrast ${label && ratio(label.fg, label.bg).toFixed(2)}`);
  }
  await page.evaluate(() => { document.documentElement.dataset.theme = 'light'; });
  // A complete package afterwards clears it.
  S.finalize = { ok: true, examiner: 'Hong', manifest_sha256: 'c'.repeat(64), package_dir: 'C:/c/reports/package_003', package_url: 'reports/package_003/' };
  await page.click('#btnFinalize');
  await page.waitForFunction(() => /package_003/.test(document.getElementById('finalLinks').textContent));
  assert.ok(!/최종화 미완료/.test(await page.textContent('#finalLinks')) && /보고서·패키지를 만들었습니다/.test(await page.textContent('#finalVerdict')));
  assert.deepEqual(page.__errors, []);
  await page.context().close();
});

// R11 U4 (track S5): a finalize whose report re-render failed says the
// report is not current; R11 U4 (I2): unauthenticated re-verify lines are
// never "모두 일치".
await check('4d R11 U4: report_stale → 보고서 최신 아님; unauthenticated verify-sources → not 모두 일치', async () => {
  const page = await newPage(browser);
  await openSheet(page);
  await page.fill('#finalExaminer', 'Hong');
  S.finalize = { ok: false, report_stale: true, error: '패키징 실패: disk full — 경고: 보고서를 다시 만들지 못했습니다 (reports/case-report.html이 최신이 아님)' };
  await page.click('#btnFinalize');
  await page.waitForFunction(() => /실패/.test(document.getElementById('finalHint').textContent));
  const row = await page.evaluate(() => {
    const li = document.querySelector('#finalLinks .h-fin-stale-row');
    return li ? { text: li.textContent, title: li.querySelector('[title]')?.getAttribute('title') || '' } : null;
  });
  assert.ok(row && /보고서 최신 아님/.test(row.text) && /다시 최종화/.test(row.text) && /보고서를 다시 만들지 못했습니다/.test(row.title), 'stale row: ' + JSON.stringify(row));
  for (const th of ['light', 'dark']) {
    await page.evaluate((t) => { document.documentElement.dataset.theme = t; }, th);
    const label = (await contrastReport(page)).find((e) => /h-fin-incomplete/.test(e.cls));
    assert.ok(label && ratio(label.fg, label.bg) >= 4.5, `${th}: 보고서 최신 아님 label contrast ${label && ratio(label.fg, label.bg).toFixed(2)}`);
  }
  await page.evaluate(() => { document.documentElement.dataset.theme = 'light'; });
  // A success that still could not refresh the report.
  S.finalize = { ok: true, report_stale: true, examiner: 'Hong', manifest_sha256: 'd'.repeat(64), package_dir: 'C:/c/reports/package_004', package_url: 'reports/package_004/' };
  await page.click('#btnFinalize');
  await page.waitForFunction(() => /package_004/.test(document.getElementById('finalLinks').textContent));
  assert.match(await page.textContent('#finalVerdict'), /보고서 최신 아님/);
  // A clean finalize clears it.
  S.finalize = { ok: true, examiner: 'Hong', manifest_sha256: 'e'.repeat(64), package_dir: 'C:/c/reports/package_005', package_url: 'reports/package_005/' };
  await page.click('#btnFinalize');
  await page.waitForFunction(() => /package_005/.test(document.getElementById('finalLinks').textContent));
  assert.ok(!/보고서 최신 아님/.test(await page.textContent('#finalLinks')) && /보고서·패키지를 만들었습니다/.test(await page.textContent('#finalVerdict')));
  // verify-sources with unauthenticated lines (all compared files match).
  const saved = S.verifySources;
  S.verifySources = { ok: true, matched: 3, mismatched: 0, not_comparable: 0, unauthenticated: 1 };
  await page.click('#finalAdvanced > summary');
  await page.click('#btnVerifySources');
  await page.waitForFunction(() => /원본 재검증: 확인 불가/.test(document.getElementById('finalChecklist').textContent));
  assert.ok(!/원본 재검증: 모두 일치/.test(await page.textContent('#finalChecklist')));
  // Track I verdict: no current result (completion marker not authenticated).
  S.verifySources = { ok: true, matched: 0, mismatched: 0, not_comparable: 0, unauthenticated: 1, verified: false, is_result: false, not_verified_reason: '완료 표식 인증 안 됨', run_id: null, lane_status: 'not-verified' };
  await page.click('#btnVerifySources');
  await page.waitForFunction(() => /원본 재검증: 현재 결과 없음/.test(document.getElementById('finalChecklist').textContent));
  assert.ok(!/원본 재검증: 모두 일치/.test(await page.textContent('#finalChecklist')));
  S.verifySources = saved;
  assert.deepEqual(page.__errors, []);
  await page.context().close();
});

// Track S (2a51c94): after a reload, /api/status carries what the last
// finalize left (memory only) — the sheet shows it without an answer of its
// own; a failure answer's finalize_incomplete:false is believed over its words.
await check('4e R11 (S): status finalize_incomplete / report_stale after a reload; the answer\'s verdict wins', async () => {
  const page = await newPage(browser);
  const before = S.status;
  S.status = Object.assign({}, before, { phase: 'review-ready', busy: false, pending_marks: 0, finalize_incomplete: true, finalize_incomplete_detail: '검토자 기록 확정 실패: busy', report_stale: true });
  await openSheet(page);
  await page.waitForFunction(() => /최종화 미완료/.test(document.getElementById('finalLinks').textContent), null, { timeout: 5000 })
    .catch(async (e) => { throw new Error(e.message + ' ' + await page.evaluate(() => JSON.stringify({ st: FIN.status, html: document.getElementById('finalLinks').innerHTML }))); });
  const rows = await page.evaluate(() => ({
    inc: document.querySelector('#finalLinks .h-fin-incomplete-row')?.getAttribute('title') || '',
    stale: !!document.querySelector('#finalLinks .h-fin-stale-row'),
  }));
  assert.ok(rows.inc === '검토자 기록 확정 실패: busy' && rows.stale, 'status rows: ' + JSON.stringify(rows));
  S.status = Object.assign({}, before, { phase: 'review-ready', busy: false, pending_marks: 0, finalize_incomplete: false, report_stale: false });
  await page.fill('#finalExaminer', 'Hong');
  S.finalize = { ok: false, error: '패키징 실패 — (이전 실행의 _INCOMPLETE 폴더 언급) 최종화 미완료 아님', finalize_incomplete: false, report_stale: false };
  await page.click('#btnFinalize');
  await page.waitForFunction(() => /실패/.test(document.getElementById('finalHint').textContent));
  const links = await page.textContent('#finalLinks');
  assert.ok(!/최종화 미완료/.test(links) && !/보고서 최신 아님/.test(links), 'server verdict false: ' + links);
  assert.deepEqual(page.__errors, []);
  await page.context().close();
});

// R12 track U: U1 lane_status "structural" → info, never 모두 일치; U2 the
// status detail shows in the row; U3 finalize journal (track S2) rows.
await check('4f R12 U1–U3: structural verify-sources is info; detail in the row; 이전 최종화 중단됨', async () => {
  const page = await newPage(browser);
  const before = S.status;
  const savedVs = S.verifySources;
  const detail = '검토자 기록 확정 실패: review/signoff.json 사용 중 — 최종화는 끝나지 않았고 패키지는 만들지 않았습니다. 이전 서명을 복원하지 못했습니다';
  S.status = Object.assign({}, before, { phase: 'review-ready', busy: false, pending_marks: 0, finalize_incomplete: true, finalize_incomplete_detail: detail,
    finalize_recovered: false, finalize_journal: { path: 'review/finalize.inprogress.json', state: 'interrupted', examiner: 'Hong', signoff_sha256: 'c0ffee', started_unix: 1760000000, error: null } });
  await openSheet(page);
  await page.waitForFunction(() => !!document.querySelector('#finalLinks .h-fin-journal-row'), null, { timeout: 5000 })
    .catch(async (e) => { throw new Error(e.message + ' ' + await page.evaluate(() => document.getElementById('finalLinks').innerHTML)); });
  const r = await page.evaluate(() => {
    const inc = document.querySelector('#finalLinks .h-fin-incomplete-row');
    const d = inc && inc.querySelector('.h-fin-detail');
    const j = document.querySelector('#finalLinks .h-fin-journal-row');
    const box = d && d.getBoundingClientRect();
    const sheet = document.querySelector('#finalLinks').getBoundingClientRect();
    return { detail: d ? d.textContent : '', visible: !!(box && box.width > 0 && box.height > 0 && box.right <= sheet.right + 1),
      journal: j ? j.textContent : '', kind: j ? j.dataset.journal : '', tip: j ? j.getAttribute('title') : '' };
  });
  assert.ok(/^검토자 기록 확정 실패/.test(r.detail) && r.visible, 'U2 detail in the row body: ' + JSON.stringify(r));
  assert.ok(r.kind === 'check' && /이전 최종화 중단됨 — 확인 필요/.test(r.journal) && /c0ffee/.test(r.tip), 'U3 journal row: ' + JSON.stringify(r));
  for (const th of ['light', 'dark']) {
    await page.evaluate((t) => { document.documentElement.dataset.theme = t; }, th);
    for (const e of (await contrastReport(page)).filter((x) => /h-fin-incomplete|h-fin-detail/.test(x.cls))) {
      assert.ok(ratio(e.fg, e.bg) >= 4.5, `${th}: ${e.cls} contrast ${ratio(e.fg, e.bg).toFixed(2)}`);
    }
  }
  await page.evaluate(() => { document.documentElement.dataset.theme = 'light'; });
  // U1: a match on a PC without an audit key.
  S.verifySources = { ok: true, matched: 3, mismatched: 0, not_comparable: 0, unauthenticated: 0, verified: true, is_result: true, run_id: 'r1', not_verified_reason: null, lane_status: 'structural' };
  await page.click('#finalAdvanced > summary');
  await page.click('#btnVerifySources');
  await page.waitForFunction(() => /원본 재검증: 자기 일관성만 — 인증 아님/.test(document.getElementById('finalChecklist').textContent), null, { timeout: 5000 })
    .catch(async (e) => { throw new Error(e.message + ' ' + await page.textContent('#finalChecklist')); });
  const row = await page.evaluate(() => [...document.querySelectorAll('#finalChecklist .h-check')].find((li) => /원본 재검증/.test(li.textContent))?.dataset.level);
  assert.equal(row, 'info', 'U1: structural is info');
  assert.ok(!/모두 일치/.test(await page.textContent('#finalChecklist')), 'U1: never 모두 일치');
  // Track I4: is_result:false names the run this request started (tooltip).
  S.verifySources = { ok: true, matched: 0, mismatched: 0, not_comparable: 0, unauthenticated: 0, verified: false, is_result: false, run_id: null, expected_run_id: '200-bbbb', not_verified_reason: '이번 실행이 결과 실행이 아님', lane_status: 'keyed' };
  await page.click('#btnVerifySources');
  await page.waitForFunction(() => /원본 재검증: 현재 결과 없음/.test(document.getElementById('finalChecklist').textContent));
  const tip = await page.evaluate(() => [...document.querySelectorAll('#finalChecklist .h-check')].find((li) => /원본 재검증/.test(li.textContent))?.getAttribute('title') || '');
  assert.ok(/200-bbbb/.test(tip), 'I4 expected_run_id tooltip: ' + tip);
  S.verifySources = savedVs;
  // Track S: a blocked finalize is a warning toast; a failure lists every
  // incomplete package it left.
  S.status = Object.assign({}, before, { phase: 'review-ready', busy: false, pending_marks: 0 });
  await page.click('#finalAdvanced > summary');
  await page.fill('#finalExaminer', 'Hong');
  S.finalize = { ok: false, error: '최종화 차단: 저장 충돌 2건을 먼저 해결하십시오', report_stale: false, finalize_incomplete: false, incomplete_package_dirs: [] };
  // This fixture has no toast box: record what toast() is asked to show.
  await page.evaluate(() => { const real = window.toast; window.__toasts = []; window.toast = (m, k) => { window.__toasts.push(k + '|' + m); return real(m, k); }; });
  await page.click('#btnFinalize');
  await page.waitForFunction(() => /최종화 차단/.test(document.getElementById('finalHint').textContent));
  const blocked = await page.evaluate(() => ({ hint: document.getElementById('finalHint').textContent, toasts: window.__toasts }));
  assert.ok(!/^실패/.test(blocked.hint) && blocked.toasts.some((t) => /\bwarn\b/.test(t) && /최종화 차단/.test(t)), 'blocked finalize = warning: ' + JSON.stringify(blocked));
  S.finalize = { ok: false, error: '패키징 실패', report_stale: false, finalize_incomplete: true, incomplete_package_dirs: ['C:/c/reports/package_004_INCOMPLETE', 'C:/c/reports/package_005_INCOMPLETE'] };
  await page.click('#btnFinalize');
  await page.waitForFunction(() => /package_005_INCOMPLETE/.test(document.getElementById('finalLinks').textContent));
  assert.equal(await page.evaluate(() => document.querySelectorAll('#finalLinks .h-fin-incomplete-row [data-pkgdir]').length), 2, 'both incomplete packages listed');
  S.status = before;
  assert.deepEqual(page.__errors, []);
  await page.context().close();
});

// R13 track U: U1 journal unpaired / other-host / recovering (track S P15/P17),
// U3 the dismiss command on an unreadable journal, U2 a newer run's
// mismatches (track I P16) on the checklist.
await check('4g R13 U1–U3: 짝 없음 / 다른 PC 차단 / 복구 중; --dismiss command; newer run mismatches shown', async () => {
  const page = await newPage(browser);
  await page.evaluate(() => savePrefs({ examinerName: 'Hong' }));
  const before = S.status;
  const savedVs = S.verifySources;
  const jn = (state, extra) => Object.assign({ path: 'review/finalize.inprogress.json', state, examiner: 'Hong', signoff_sha256: 'c0ffee', started_unix: 1760000000, error: null }, extra || {});
  const base = { phase: 'review-ready', busy: false, pending_marks: 0, finalize_recovered: false };
  const journalRow = () => page.evaluate(() => {
    const j = document.querySelector('#finalLinks .h-fin-journal-row');
    const sheet = document.querySelector('#finalLinks').getBoundingClientRect();
    const out = j ? [...j.querySelectorAll('span, small, code')].every((e) => { const b = e.getBoundingClientRect(); return b.width > 0 && b.right <= sheet.right + 1; }) : false;
    return j ? { kind: j.dataset.journal, text: j.textContent, inside: out, label: j.querySelector('span')?.className || '' } : null;
  });
  // other-host: a block row, the button is disabled, the host is named.
  S.status = Object.assign({}, before, base, { finalize_journal: jn('other-host', { host: 'PC-LAB2' }) });
  await openSheet(page);
  await page.waitForFunction(() => !!document.querySelector('#finalLinks .h-fin-journal-row'), null, { timeout: 5000 });
  let j = await journalRow();
  assert.ok(j && j.kind === 'otherhost' && /다른 PC의 최종화 — 차단 중/.test(j.text) && /PC-LAB2/.test(j.text) && j.inside, 'other-host row: ' + JSON.stringify(j));
  let rs = await rows(page);
  assert.ok(rs.some((r) => r.level === 'block' && /다른 PC의 최종화 — 차단 중/.test(r.text) && /PC-LAB2/.test(r.text)), 'other-host block row: ' + JSON.stringify(rs));
  assert.equal((await btnState(page)).disabled, true, 'other-host disables 최종화');
  // recovering: info label, block row (not "다른 작업이 진행 중").
  S.status = Object.assign({}, before, base, { busy: true, finalize_journal: jn('recovering') });
  await page.click('#btnFinalRecheck');
  await page.waitForFunction(() => document.querySelector('#finalLinks .h-fin-journal-row')?.dataset.journal === 'recovering', null, { timeout: 5000 });
  j = await journalRow();
  assert.ok(/이전 최종화 복구 중/.test(j.text) && j.label === 'h-fin-info' && !/확인 필요/.test(j.text), 'recovering row: ' + JSON.stringify(j));
  rs = await rows(page);
  assert.ok(rs[0].level === 'block' && /이전 최종화 복구 중/.test(rs[0].text) && !rs.some((r) => /다른 작업이 진행 중/.test(r.text)), 'recovering block row: ' + JSON.stringify(rs));
  for (const th of ['light', 'dark']) {
    await page.evaluate((t) => { document.documentElement.dataset.theme = t; }, th);
    const lab = (await contrastReport(page)).find((e) => e.cls === 'h-fin-info');
    assert.ok(lab && ratio(lab.fg, lab.bg) >= 4.5, `${th}: 복구 중 label contrast ${lab && ratio(lab.fg, lab.bg).toFixed(2)}`);
  }
  await page.evaluate(() => { document.documentElement.dataset.theme = 'light'; });
  // unpaired: 확인 필요, nothing changed; does not block on its own.
  S.status = Object.assign({}, before, base, { finalize_journal: jn('unpaired') });
  await page.click('#btnFinalRecheck');
  await page.waitForFunction(() => document.querySelector('#finalLinks .h-fin-journal-row')?.dataset.journal === 'unpaired', null, { timeout: 5000 });
  j = await journalRow();
  assert.ok(/이전 최종화 기록 짝 없음 — 확인 필요/.test(j.text) && /아무것도 바뀌지 않았습니다/.test(j.text) && j.inside, 'unpaired row: ' + JSON.stringify(j));
  // Track S (c76a4df): blocks_finalize decides the block row, not the state.
  const blockRows = async () => (await rows(page)).filter((r) => r.level === 'block' && /최종화 차단: |이전 최종화 복구 중/.test(r.text));
  S.status = Object.assign({}, before, base, { finalize_journal: jn('completed', { blocks_finalize: false, dismiss_command: 'frametrace finalize-journal "C:/cases/c1" --dismiss' }) });
  await page.click('#btnFinalRecheck');
  await page.waitForFunction(() => document.querySelector('#finalLinks .h-fin-journal-row')?.dataset.journal === 'completed', null, { timeout: 5000 });
  j = await journalRow();
  assert.ok(/이전 최종화 완료 — 기록만 남음/.test(j.text) && j.label === 'h-fin-info' && !(await blockRows()).length && !(await btnState(page)).disabled, 'completed = info, no block: ' + JSON.stringify(j));
  for (const th of ['light', 'dark']) {
    await page.evaluate((t) => { document.documentElement.dataset.theme = t; }, th);
    const lab = (await contrastReport(page)).find((e) => e.cls === 'h-fin-info');
    assert.ok(lab && ratio(lab.fg, lab.bg) >= 4.5, `${th}: 기록만 남음 label contrast`);
  }
  await page.evaluate(() => { document.documentElement.dataset.theme = 'light'; });
  S.status = Object.assign({}, before, base, { finalize_journal: jn('other-host', { host: '0a1b2c3d4e5f', blocks_finalize: false, blocked_until_unix: 1760086400 }) });
  await page.click('#btnFinalRecheck');
  await page.waitForFunction(() => /0a1b2c3d4e5f/.test(document.querySelector('#finalLinks .h-fin-journal-row')?.textContent || ''), null, { timeout: 5000 });
  assert.ok(!(await blockRows()).length && /까지 차단/.test((await journalRow()).text), 'other-host past its block: no block row');
  S.status = Object.assign({}, before, base, { finalize_journal: jn('interrupted', { blocks_finalize: true, reason: null }) });
  await page.click('#btnFinalRecheck');
  await page.waitForFunction(() => document.querySelector('#finalLinks .h-fin-journal-row')?.dataset.journal === 'check', null, { timeout: 5000 });
  const ib = await blockRows();
  assert.ok(ib.length === 1 && /최종화 차단: 이전 최종화 중단됨/.test(ib[0].text) && (await btnState(page)).disabled, 'interrupted + blocks_finalize → block: ' + JSON.stringify(ib));
  // unreadable: the dismiss command, inside the sheet, AA in both themes.
  // R18 U3/U5: no server command → none built, the server's sentence instead.
  S.status = Object.assign({}, before, base, { case_dir: 'C:/cases/a\u2019b', finalize_journal: jn('unreadable', { error: 'JSON 파싱 실패: line 1', dismiss_command: null, dismiss_command_cmd: null, command_unavailable_reason: '경로에 특수 문자 — 명령을 직접 입력하십시오' }) });
  await page.click('#btnFinalRecheck');
  await page.waitForFunction(() => document.querySelector('#finalLinks .h-fin-journal-row')?.dataset.journal === 'unreadable', null, { timeout: 5000 });
  j = await journalRow();
  assert.ok(/경로에 특수 문자 — 명령을 직접 입력하십시오/.test(j.text) && !/frametrace/.test(j.text) && j.inside, 'R18 U3: no command, the reason: ' + JSON.stringify(j));
  S.status = Object.assign({}, before, base, { case_dir: 'C:/cases/c1', finalize_journal: jn('unreadable', { error: 'JSON 파싱 실패: line 1', dismiss_command: "frametrace finalize-journal 'C:/cases/c1' --dismiss" }) });
  await page.click('#btnFinalRecheck');
  await page.waitForFunction(() => !!document.querySelector('#finalLinks .h-fin-journal-row .h-fin-cmd code'), null, { timeout: 5000 });
  j = await journalRow();
  const cmd = await page.textContent('#finalLinks .h-fin-journal-row .h-fin-cmd code');
  assert.ok(cmd === "frametrace finalize-journal 'C:/cases/c1' --dismiss" && j.inside, 'dismiss command: ' + cmd + ' ' + JSON.stringify(j));
  for (const th of ['light', 'dark']) {
    await page.evaluate((t) => { document.documentElement.dataset.theme = t; }, th);
    const cmdEls = (await contrastReport(page)).filter((x) => /h-fin-cmd|mono/.test(x.cls) && /finalize-journal|치우기/.test(x.text));
    assert.ok(cmdEls.length >= 2, `${th}: command text measured (${cmdEls.length})`);
    for (const e of cmdEls) {
      assert.ok(ratio(e.fg, e.bg) >= 4.5, `${th}: ${e.cls} contrast ${ratio(e.fg, e.bg).toFixed(2)}`);
    }
  }
  await page.evaluate(() => { document.documentElement.dataset.theme = 'light'; });
  // U2: a newer usable run without a marker — its mismatches on the checklist.
  S.status = Object.assign({}, before, base, { finalize_journal: null });
  S.verifySources = { ok: true, matched: 0, mismatched: 0, not_comparable: 0, unauthenticated: 0, verified: false, is_result: false, run_id: 'r1', withheld_reason: '완료 표식 뒤 사용 불가 줄; 표식 없는 더 새 실행 있음 (실행 1개 · 기록 2개 · 불일치 2건)', newer_runs: 1, newer_records: 2, newer_mismatched: 2, lane_status: 'keyed',
    newer_mismatches: [{ run_id: '300-cccc', run_unix: 1760000300, id: 'vid_000007', source_path: 'D:/증거/원본/a_very_long_folder_name_for_wrapping/clip_0007.mp4', recorded_sha256: 'a'.repeat(64), observed_sha256: 'b'.repeat(64) },
      { run_id: '300-cccc', run_unix: 1760000300, id: 'vid_000009', source_path: 'D:/src/b.mp4', recorded_sha256: 'c'.repeat(64), observed_sha256: 'd'.repeat(64) }] };
  await page.click('#finalAdvanced > summary');
  await page.click('#btnVerifySources');
  await page.waitForFunction(() => /원본 재검증: 현재 결과 없음 — 불일치 2건/.test(document.getElementById('finalChecklist').textContent), null, { timeout: 5000 })
    .catch(async (e) => { throw new Error(e.message + ' ' + await page.textContent('#finalChecklist')); });
  const vs = (await rows(page)).find((r) => /원본 재검증/.test(r.text));
  // R14 U3: a newer run's mismatch blocks — the finalize button is disabled.
  assert.ok(vs && vs.level === 'block' && /표식 없는 더 새 실행 있음 — 불일치 2건/.test(vs.text) && !/모두 일치/.test(vs.text), 'U2/R14 U3 row: ' + JSON.stringify(vs));
  assert.ok(await page.evaluate(() => document.getElementById('btnFinalize').disabled), 'R14 U3: newer-run mismatch disables 보고서 생성·패키징');
  // Track I (32e88a2): each mismatch listed under the row — id + source path,
  // hashes in its tooltip, inside the sheet, AA in both themes.
  const items = await page.evaluate(() => {
    const sheet = document.querySelector('#finalChecklist').getBoundingClientRect();
    return [...document.querySelectorAll('#finalChecklist .h-check-items li')].map((li) => {
      const b = li.getBoundingClientRect();
      return { text: li.textContent, tip: li.getAttribute('title') || '', inside: b.width > 0 && b.right <= sheet.right + 1 };
    });
  });
  assert.ok(items.length === 2 && /vid_000007/.test(items[0].text) && /clip_0007\.mp4/.test(items[0].text) && items[0].tip.includes('a'.repeat(64)) && items[0].tip.includes('b'.repeat(64)) && items.every((i) => i.inside), 'newer_mismatches rows: ' + JSON.stringify(items));
  for (const th of ['light', 'dark']) {
    await page.evaluate((t) => { document.documentElement.dataset.theme = t; }, th);
    const els = await page.evaluate(() => [...document.querySelectorAll('#finalChecklist .h-check-items li, #finalChecklist .h-check-items li *')].map((el) => {
      const parse = (s) => s.match(/rgba?\(([^)]+)\)/)[1].split(',').map(Number);
      let bg = [255, 255, 255];
      for (let e = el; e; e = e.parentElement) { const c = parse(getComputedStyle(e).backgroundColor); if (c.length < 4 || c[3] > 0.5) { bg = c.slice(0, 3); break; } }
      return { fg: parse(getComputedStyle(el).color).slice(0, 3), bg };
    }));
    assert.ok(els.length >= 4, 'measured rows');
    for (const e of els) assert.ok(ratio(e.fg, e.bg) >= 4.5, `${th}: mismatch row contrast ${ratio(e.fg, e.bg).toFixed(2)}`);
  }
  await page.evaluate(() => { document.documentElement.dataset.theme = 'light'; });
  S.verifySources = savedVs;
  S.status = before;
  assert.deepEqual(page.__errors, []);
  await page.context().close();
});

await check('4b a stale "ready" never seals: fresh block at click time -> no POST', async () => {
  const page = await newPage(browser);
  await openSheet(page);
  await page.fill('#finalExaminer', 'Hong');
  S.status = { ok: true, phase: 'review-ready', busy: false, pending_marks: 5 };
  await page.click('#btnFinalize');
  await page.waitForFunction(() => /해결할 항목/.test(document.getElementById('finalHint').textContent));
  assert.equal(S.posts.filter((p) => p.path === '/api/finalize').length, 0);
  await page.context().close();
});

await check('5 advanced: collapsed by default, holds data-adv x8, marks import, verify-sources/audit', async () => {
  const page = await newPage(browser);
  await openSheet(page);
  assert.equal(await page.$eval('#finalAdvanced', (d) => d.open), false);
  const inside = await page.$eval('#finalAdvanced', (d) => ({
    adv: [...d.querySelectorAll('[data-adv]')].map((b) => b.dataset.adv).sort(),
    ids: ['btnImportMarks', 'btnVerifySources', 'btnVerifyAudit', 'marksFile', 'advPath', 'advOut', 'advLink'].filter((id) => d.querySelector('#' + id)),
  }));
  assert.deepEqual(inside.adv, ['compare', 'deepfake', 'dfxml', 'known-hash', 'merge', 'qa-consistency', 'qa-defense', 'timeline']);
  assert.equal(inside.ids.length, 7);
  assert.equal(await page.$eval('[data-adv="deepfake"]', (b) => b.disabled), true, 'deepfake greyed when the tool is missing');
  // outside the <details> there is no tool button
  assert.equal(await page.$$eval('#hFinalizeSheet button', (bs) => bs.filter((b) => !b.closest('#finalAdvanced')).map((b) => b.id).sort().join(',')),
    'btnFinalRecheck,btnFinalRetrySave,btnFinalize,hFinalizeClose');
  // verify-sources: one POST, the honest partial verdict lands in the checklist
  await page.click('#finalAdvanced > summary');
  await page.click('#btnVerifySources');
  await page.waitForFunction(() => /원본 재검증: 일부만 확인됨/.test(document.getElementById('finalChecklist').textContent));
  assert.equal(S.posts.filter((p) => p.path === '/api/verify-sources').length, 1);
  // a path tool without a path asks for one and posts nothing
  await page.click('[data-adv="compare"]');
  assert.match(await page.textContent('#advOut'), /경로를 입력하십시오/);
  await page.click('[data-adv="dfxml"]');
  await page.waitForFunction(() => document.getElementById('advOut').textContent === 'done');
  assert.deepEqual(S.posts.filter((p) => p.path === '/api/advanced').map((p) => p.body), [{ tool: 'dfxml', extra: '' }]);
  await page.context().close();
});

// ⑥ project switch clears everything
await check('6 finResetSheet clears result, examiner and verify state', async () => {
  const page = await newPage(browser);
  await openSheet(page);
  await page.fill('#finalExaminer', 'Hong');
  await page.click('#btnFinalize');
  await page.waitForSelector('#finalLinks a');
  await page.evaluate(() => finResetSheet());
  await page.waitForFunction(() => !document.querySelector('#finalLinks a'));
  assert.equal(await page.inputValue('#finalExaminer'), '');
  await page.context().close();
});

// ⑦ geometry / AA / glyphs, both themes, both sizes
for (const [w, h] of [[1366, 768], [1920, 1080]]) {
  for (const theme of ['light', 'dark']) {
    await check(`7 buttons + AA + never covered @${w}x${h} ${theme}`, async () => {
      const page = await newPage(browser, { width: w, height: h, theme });
      await page.evaluate(() => savePrefs({ examinerName: 'Hong' }));
      await openSheet(page);
      await page.click('#finalAdvanced > summary');
      const btns = await page.$$eval('#hFinalizeSheet button', (bs) => bs.filter((b) => b.offsetParent).map((b) => {
        b.scrollIntoView({ block: 'nearest' }); // body scrolls; the foot stays put
        const r = b.getBoundingClientRect();
        const hit = document.elementFromPoint(r.x + r.width / 2, r.y + r.height / 2);
        return { id: b.id || b.dataset.adv || b.textContent.trim(), h: r.height, w: r.width, primary: b.classList.contains('primary'), icon: b.classList.contains('icon'),
          label: b.getAttribute('aria-label'), text: b.textContent.trim(), covered: !(hit === b || b.contains(hit)),
          inView: r.bottom <= innerHeight + 0.5 && r.right <= innerWidth + 0.5 && r.top >= 0 && r.left >= 0 };
      }));
      for (const b of btns) {
        assert.equal(b.covered, false, `${b.id} is covered`);
        assert.ok(!/[◐☀⏻⤢⧉▾↗✓✕☰⚠⤓▸×]/.test(b.text), `${b.id} text carries a glyph`);
        if (b.icon) assert.ok(b.label, `${b.id} icon button needs aria-label`);
      }
      const fin = btns.find((b) => b.id === 'btnFinalize');
      assert.ok(fin && fin.inView && !fin.covered, 'btnFinalize inside the viewport, not covered');
      for (const b of btns) { assert.ok(b.h >= 24, `${b.id} height ${b.h} < 24`); if (b.primary) assert.ok(b.h >= 36, `${b.id} primary height ${b.h} < 36`); }
      assert.equal(await page.evaluate(() => document.documentElement.scrollWidth <= innerWidth), true, 'no horizontal scroll');
      // AA 4.5 for every text node in the sheet (incl. verdict, block/warn rows).
      const report = await contrastReport(page);
      const bad = report.map((r) => ({ ...r, ratio: ratio(r.fg, r.bg) })).filter((r) => r.ratio < 4.5 && !/^(BUTTON|SUMMARY)/i.test(String(r.cls)) || r.ratio < 3);
      assert.deepEqual(bad.map((r) => `${r.text} ${r.ratio.toFixed(2)}`), [], 'text contrast < 4.5');
      await page.context().close();
    });
  }
}
// R4 F4: a missing kit/mount/token is a FAIL, never a silent SKIP.
await check('7b kit.css defines .primary (button-height checks above are real)', async () => {
  assert.ok(KIT_LANDED, 'assets/kit.css has no .primary rule');
});

// ⑧ opening the sheet under the REAL shell page (once T2's mount exists)
const shellHasMount = /id="hFinalizeSheet"/.test(read('assets/examiner_app.html'));
await check('8 real shell has the #hFinalizeSheet mount', async () => {
  assert.ok(shellHasMount, 'assets/examiner_app.html has no #hFinalizeSheet mount');
});
if (shellHasMount) {
  await check('8 real shell: mount renders, #main is inert while open, Esc closes when idle', async () => {
    const ctx = await browser.newContext({ viewport: { width: 1366, height: 768 } });
    const page = await ctx.newPage();
    await page.route('http://ft.test/**', async (route) => {
      const url = new URL(route.request().url());
      if (url.pathname === '/') return route.fulfill({ status: 200, contentType: 'text/html', body: appPageHtml() });
      return route.fulfill({ status: 200, contentType: 'application/json', body: JSON.stringify({ ok: true, has_job: true, case_dir: '/case/c', phase: 'review-ready', pending_marks: 0, manifest: { case_id: 'c' } }) });
    });
    await page.goto('http://ft.test/');
    await page.click('#hBtnFinalize');
    await page.waitForSelector('#hFinalizeSheet #btnFinalize');
    assert.equal(await page.$eval('#main', (m) => m.inert), true, '#main inert while the sheet is open');
    await page.waitForFunction(() => document.activeElement && document.activeElement.id === 'finalExaminer', null, { timeout: 3000 }).catch(() => {});
    assert.equal(await page.evaluate(() => document.activeElement && document.activeElement.id), 'finalExaminer');
    await page.keyboard.press('Escape');
    await page.waitForFunction(() => document.getElementById('hFinalizeSheet').hidden);
    assert.equal(await page.evaluate(() => document.activeElement && document.activeElement.id), 'hBtnFinalize');
    await ctx.close();
  });
}

// ⑨ package viewer parity: the exported viewer carries the contract mark colours
await check('9 viewer bundle carries the contract --mark-important-bg', async () => {
  assert.ok(T1_LANDED, 'tokens.css has no --mark-* tokens');
  assert.match(viewerBundleCss(), /--mark-important-bg:\s*#f6dfb3/i);
});

await browser.close();
if (failed) { console.error(`\n${failed} check(s) failed`); process.exit(1); }
console.log('\nverify-finalize-sheet: all checks passed');
