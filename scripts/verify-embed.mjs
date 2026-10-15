// T5 harness: the viewer hosted in the shell document (no iframe), the
// shared fragment, the window.FTViewer API and the FRAMETRACE_UI=legacy
// rollback (docs/ui-rebuild-contract.md §3, §8.2 verify-embed).
//
//   cargo build --locked
//   NODE_PATH=<repo>/node_modules node scripts/verify-embed.mjs
//
// Hosted page: GET / once the shell carries <html data-ft-host> and the
// fragment include; until then (shell still embeds by iframe) a minimal
// host page assembled from the same build pieces (scripts/lib/app-bundle.mjs)
// is served at /__t5-host.html through Playwright routing, same origin as
// the isolated server, so the real /api/* answers it.
//
//  1 every fragment id exactly once on the hosted page, #ftViewer[data-ft-viewer="2"]
//  2 /review/evidence-viewer.html, /viewer/ and the hosted page carry the
//    fragment byte-identical (static, before any data is loaded)
//  3 shell bundle: no viewer id lookups, no postToViewer/contentWindow
//    (enforced once the shell hosts the viewer; reported before that)
//  4 keys: [data-keys="off"] inputs/buttons never feed j/k/1/Ctrl+Z; a
//    plain host button does
//  5 the two bundles' top-level bindings do not intersect; every viewer
//    document/window listener carries the mount's AbortSignal
//  6 computed-style spot checks identical standalone vs hosted
//  7 FTViewer API: ready/records/save-state/dirty/active-record/selection
//    events, summary/saveState/save/setExaminer/setLocale/setTheme/focus,
//    unsaved marks survive destroy()+init(), destroy() silences keys,
//    init() twice throws, request-popout + popout()/closePopout() events
//  8 FRAMETRACE_UI=legacy: GET / is the stage-rail shell whose iframe loads
//    the standalone viewer with ?embed=1 (.ft-viewer.embed, cards render)
import { mkdtempSync, rmSync, readFileSync } from 'node:fs';
import { join, dirname } from 'node:path';
import { tmpdir } from 'node:os';
import { fileURLToPath } from 'node:url';
import { spawnSync } from 'node:child_process';
import { createRequire } from 'node:module';
import { startIsolatedServer } from './lib/isolated-server.mjs';
import {
  appMarkup, appBundleJs, viewerBundleJs, viewerFragment, includeViewerBody, inlineIcons,
  concatParts, tokensPath, kitPath, viewerOwnCssPaths, topLevelBindings, scanCode, callArgs, VIEWER_INCLUDE,
} from './lib/app-bundle.mjs';

const root = join(dirname(fileURLToPath(import.meta.url)), '..');
let axeSource = '';
const require = createRequire(import.meta.url);
let chromium;
try { ({ chromium } = require('playwright')); } catch {
  console.error('playwright not resolvable — set NODE_PATH to a node_modules with playwright');
  process.exit(2);
}

try { axeSource = readFileSync(createRequire(import.meta.url).resolve('axe-core/axe.min.js'), 'utf8'); } catch { /* axe optional */ }
let failures = 0;
const check = (name, ok, detail = '') => {
  if (!ok) failures++;
  console.log(`${ok ? 'PASS' : 'FAIL'}  ${name}${detail ? '  — ' + detail : ''}`);
};
const note = (msg) => console.log(`NOTE  ${msg}`);

const fragment = viewerFragment(root);
const fragmentIds = [...fragment.matchAll(/\sid="([^"]+)"/g)].map((m) => m[1]);
const shellHosts = appMarkup(root).includes(VIEWER_INCLUDE) && /<html[^>]*data-ft-host/.test(appMarkup(root));

// ---------------------------------------------------------------- static
{
  // 5a top-level bindings
  const viewerTop = topLevelBindings(viewerBundleJs(root));
  const appTop = topLevelBindings(appBundleJs(root));
  const clash = [...appTop].filter((n) => viewerTop.has(n));
  check('5 bundles share no top-level binding', clash.length === 0, clash.join(', ') || `viewer ${viewerTop.size} / shell ${appTop.size}`);
  check('5 viewer exposes FTViewer + ftViewerBoot at top level', viewerTop.has('FTViewer') && viewerTop.has('ftViewerBoot'));
  // 5b every document/window listener is tied to the mount's signal
  const src = viewerBundleJs(root);
  const missing = [];
  const starts = [];
  scanCode(src, (i) => {
    if (src.startsWith('document.addEventListener(', i) || src.startsWith('window.addEventListener(', i) || src.startsWith('playerChannel.addEventListener(', i)) starts.push(i);
  });
  for (const at of starts) {
    const args = callArgs(src, src.indexOf('(', at));
    if (args === null || !/\bsignal\b/.test(args)) missing.push(src.slice(at, src.indexOf('\n', at)).trim().slice(0, 80));
  }
  check(`5 all ${starts.length} document/window/channel listeners use { signal }`, missing.length === 0, missing.join(' | '));

  // 3 shell bundle must not reach into the viewer DOM
  const appJs = appBundleJs(root);
  const own = new Set([...appMarkup(root).matchAll(/\sid="([^"]+)"/g)].map((m) => m[1]));
  const hits = [];
  for (const id of fragmentIds) {
    if (!shellHosts && own.has(id)) continue; // iframe era: the shell's own same-named element
    const re = new RegExp(`(?:\\$\\(\\s*|getElementById\\(\\s*)["'\`]${id}["'\`]|["'\`]#${id}\\b`);
    if (re.test(appJs)) hits.push(id);
  }
  const bridge = /postToViewer|contentWindow/.test(appJs);
  // Enforced: the integrated shell hosts the viewer in its own document.
  check('3 shell template hosts the fragment (<html data-ft-host> + include marker)', shellHosts);
  check('3 shell bundle has no viewer id lookups', hits.length === 0, hits.join(', '));
  check('3 shell bundle has no postToViewer/contentWindow', !bridge);
}

// ---------------------------------------------------------------- server
const work = mkdtempSync(join(tmpdir(), 'ft-embed-'));
let server = null;
let legacy = null;
let browser = null;
const cleanup = async () => {
  try { browser && await browser.close(); } catch {}
  try { server && server.stop(); } catch {}
  try { legacy && legacy.stop(); } catch {}
  try { rmSync(work, { recursive: true, force: true }); } catch {}
};
const die = async (msg) => { console.error('FAIL:', msg); await cleanup(); process.exit(1); };

const src = join(work, 'src');
const demo = spawnSync(process.execPath, [join(root, 'scripts/make-demo-case.mjs'), src], { encoding: 'utf8' });
if (demo.status !== 0) await die('demo source: ' + (demo.stderr || demo.stdout));
// R4 B5: generated pages are trusted by an HMAC with a per-user key; both
// servers (main + legacy, separate config dirs) must share one key file or
// the legacy one sandboxes the review page the main one generated.
const pagesKey = { FRAMETRACE_PAGES_KEY_FILE: join(work, 'generated-pages.key') };
try { server = await startIsolatedServer({ root, work: join(work, 'srv'), env: pagesKey }); } catch (e) { await die(e.message); }
const caseDir = join(work, 'case');
const started = await server.postJson('/api/start', { input_kind: 'folder', source_path: src, case_dir: caseDir, with_hash: true, with_ffprobe: true });
if (!started.ok) await die('start: ' + started.error);
for (const deadline = Date.now() + 240000; ; await new Promise((r) => setTimeout(r, 400))) {
  const st = await server.getJson('/api/status');
  if (st.phase === 'review-ready') break;
  if (st.phase === 'error') await die('analysis error: ' + st.error);
  if (Date.now() > deadline) await die('analysis timed out at ' + st.phase);
}
const { base, token } = server;
const getText = async (path) => (await fetch(base + path, { headers: server.headers, cache: 'no-store' })).text();

// 2 static fragment parity across every page that shows the viewer
const hostPath = shellHosts ? '/' : '/__t5-host.html';
function testHostPage() {
  // The minimal stand-in for T2's shell: same build pieces (tokens + kit +
  // the viewer's own sheets once, fragment include, viewer bundle), a
  // top-strip button and a [data-keys="off"] region with the shell's inputs.
  const css = concatParts([tokensPath(root), kitPath(root), ...viewerOwnCssPaths(root)], 'assets/app/bundle.css', root);
  const markup = `<!doctype html>
<html lang="ko" data-ft-host="shell">
<head><meta charset="utf-8"><title>T5 host</title>
<script>try{document.documentElement.dataset.theme=localStorage.getItem("ft-theme")||"light"}catch(e){}</script>
<style>
${css}
html,body{margin:0;height:100%}body{display:grid;grid-template-rows:44px auto minmax(0,1fr);background:var(--bg)}
#tTop{display:flex;gap:8px;align-items:center;padding:0 12px}
#tSheet{display:flex;gap:8px;padding:4px 12px}
#tMain{min-height:0}#hViewerHost{min-height:0;height:100%}
</style></head>
<body>
<!-- @bundle:icons.svg -->
<header id="tTop"><button id="tStripBtn" type="button">strip</button></header>
<section id="tSheet" data-keys="off"><input id="sourcePath" type="text" aria-label="source"><input id="finalExaminer" type="text" aria-label="examiner"><button id="tSheetBtn" type="button">sheet</button></section>
<main id="tMain" tabindex="-1"><div id="hViewerHost">${VIEWER_INCLUDE}</div></main>
<script>
${viewerBundleJs(root)}</script>
</body></html>`;
  return inlineIcons(includeViewerBody(markup, { required: true, root }).page, root);
}
const hostHtml = shellHosts ? await getText('/') : testHostPage();
for (const [name, html] of [['/review/evidence-viewer.html', await getText('/review/evidence-viewer.html')], ['/viewer/', await getText('/viewer/')], [hostPath, hostHtml]]) {
  check(`2 ${name} carries the fragment byte-identical`, html.includes(fragment));
}
if (!shellHosts) note('1-7 hosted checks use the T5 test host page (GET / still embeds by iframe)');

try { browser = await chromium.launch(); } catch { browser = await chromium.launch({ channel: 'chrome' }); }
const ctx = await browser.newContext({ viewport: { width: 1366, height: 768 } });
const errors = [];
async function openPage(path) {
  const page = await ctx.newPage();
  page.on('pageerror', (e) => errors.push(`${path}: ${e.message} @ ${(e.stack || '').split('\n').slice(1, 3).join(' ').trim()}`));
  if (!shellHosts) {
    await page.route(`${base}/__t5-host.html*`, (route) => route.fulfill({ contentType: 'text/html; charset=utf-8', body: hostHtml }));
  }
  // ?token= plants the auth cookie for the page's own /api fetches.
  await page.goto(`${base}/?token=${token}`);
  await page.goto(`${base}${path}`);
  return page;
}
const activeId = (page) => page.evaluate(() => document.querySelector('#recordGrid .card.active')?.dataset.id || null);
const markOf = (page, id) => page.evaluate((i) => document.querySelector(`#recordGrid .card[data-id="${CSS.escape(i)}"] .mark-chip`)?.className || '', id);

// ---------------------------------------------------------------- hosted
const host = await openPage(hostPath);
await host.evaluate(() => {
  window.__ev = [];
  for (const type of ['ready', 'records', 'save-state', 'dirty', 'selection', 'active-record', 'locale', 'theme', 'request-popout', 'popout', 'toast', 'error', 'destroyed', 'live-state']) {
    FTViewer.on(type, (e) => window.__ev.push({ type, detail: e.detail }));
  }
});
const mountOpts = shellHosts ? null : { embed: true };
if (!shellHosts) {
  await host.evaluate(() => FTViewer.init({ embed: true, host: document.getElementById('hViewerHost'), theme: 'light', locale: 'ko' }));
}
await host.waitForSelector('#recordGrid .card', { timeout: 30000 });
const counts = await host.evaluate((ids) => ids.map((id) => [id, document.querySelectorAll('#' + CSS.escape(id)).length]), fragmentIds);
const bad = counts.filter(([, n]) => n !== 1);
check(`1 all ${fragmentIds.length} fragment ids exactly once on the hosted page`, bad.length === 0, bad.map(([id, n]) => `${id}×${n}`).join(' '));
check('1 #ftViewer[data-ft-viewer="2"].hosted.embed', await host.evaluate(() => {
  const r = document.querySelector('#ftViewer[data-ft-viewer="2"]');
  return !!r && r.classList.contains('hosted') && r.classList.contains('embed');
}));
// 1 landmarks: the shell's <main> is the page's only main landmark; the
// fragment's #mainArea (role="main" standalone) is a labelled region when hosted.
check('9 hosted page has exactly one main landmark (the shell\'s) and #mainArea is a labelled region', await host.evaluate(() => {
  const a = document.getElementById('mainArea');
  return document.querySelectorAll('main, [role="main"]').length === 1 && a.getAttribute('role') === 'region' && !!a.getAttribute('aria-label');
}), await host.evaluate(() => String(document.querySelectorAll('main, [role="main"]').length)));
check('1 no iframe on the hosted page', await host.evaluate(() => document.querySelectorAll('iframe').length === 0));
check('1 standalone title row hidden when hosted', await host.evaluate(() => getComputedStyle(document.querySelector('#ftViewer > header')).display === 'none'));
check('7 ready + records events fired', await host.evaluate(() => ['ready', 'records'].every((t) => window.__ev.some((e) => e.type === t && e.detail.total > 0))));
check('7 init() while mounted throws', await host.evaluate(async () => { try { await FTViewer.init({}); return false; } catch (e) { return /already mounted/.test(e.message); } }));
const summary = await host.evaluate(() => FTViewer.summary());
check('7 summary() has total/byKind/byStatus/pending', summary && summary.total > 0 && summary.byKind && summary.byStatus && summary.pending === 0, JSON.stringify(summary));
check('7 saveState() saved', await host.evaluate(() => FTViewer.saveState()) === 'saved');

// 4 keys: host regions with [data-keys="off"] never feed viewer shortcuts
await host.evaluate(() => FTViewer.focus());
const first = await activeId(host);
// (#sourcePath lives in the empty state, hidden while a project is open.)
if (!shellHosts) {
  await host.focus('#sourcePath');
  await host.keyboard.press('j');
  await host.keyboard.press('1');
  await host.keyboard.press('Control+z');
}
// The real shell: the deliverables sheet field and a button inside the jobs
// sheet (both [data-keys="off"]); the T5 test host page has stand-ins.
const finSel = '#finalExaminer';
const sheetBtnSel = shellHosts ? '#btnCopyLog' : '#tSheetBtn';
const stripBtnSel = shellHosts ? '#hBtnTheme' : '#tStripBtn';
if (shellHosts) {
  await host.evaluate(() => hOpenSheet('finalize', document.getElementById('hBtnFinalize')));
  await host.waitForSelector('#finalExaminer', { state: 'visible', timeout: 10000 });
}
await host.focus(finSel);
await host.keyboard.press('k');
if (shellHosts) {
  await host.evaluate(() => hCloseSheet('finalize', false));
  await host.evaluate(() => hOpenSheet('jobs', document.getElementById('hBtnJobs')));
  await host.waitForSelector(sheetBtnSel, { state: 'visible', timeout: 10000 });
}
await host.focus(sheetBtnSel);
await host.keyboard.press('j');
await host.keyboard.press('1');
await host.keyboard.press('Control+b');
if (shellHosts) await host.evaluate(() => hCloseSheet('jobs', false));
check('4 keys-off inputs/buttons: active card + marks unchanged', await activeId(host) === first && !(await markOf(host, first)) && await host.evaluate(() => !document.getElementById('ftViewer').classList.contains('rail-collapsed') || window.innerWidth < 1000), `active ${await activeId(host)}`);
await host.focus(stripBtnSel);
await host.keyboard.press('j');
const second = await activeId(host);
check('4 plain host button: j moves the active card', second && second !== first, `${first} → ${second}`);
check('7 active-record event on j', await host.evaluate((id) => window.__ev.some((e) => e.type === 'active-record' && e.detail.id === id), second));

// 7 marks, events, setExaminer, unsaved marks across destroy()+init()
await host.evaluate(() => FTViewer.setExaminer('T5 Examiner'));
check('7 setExaminer() fills #examinerName', await host.evaluate(() => document.getElementById('examinerName').value === 'T5 Examiner' && FTViewer.getExaminer() === 'T5 Examiner'));
await host.keyboard.press('2'); // important on `second`, advances
check('7 key 2 marks the card (hosted)', /important/.test(await markOf(host, second)));
check('7 dirty + save-state(pending) events', await host.evaluate(() => window.__ev.some((e) => e.type === 'dirty' && e.detail.count > 0) && window.__ev.some((e) => e.type === 'save-state' && e.detail.state === 'pending')));
check('7 saveState() pending before autosave', await host.evaluate(() => FTViewer.saveState()) === 'pending');
const reloadAnswer = await host.evaluate(() => FTViewer.reload());
check('7 reload() refused while marks are unsaved (and a save started)', reloadAnswer === 'refused');
await host.waitForFunction(() => FTViewer.saveState() === 'saved', null, { timeout: 15000 });
check('7 save-state event saved after the refusal-triggered save', await host.evaluate(() => window.__ev.some((e) => e.type === 'save-state' && e.detail.state === 'saved' && e.detail.pending === 0)));
check('7 live-state events: canReload false while unsaved, true once saved; liveState() agrees', await host.evaluate(() => {
  const ls = window.__ev.filter((e) => e.type === 'live-state');
  const now = FTViewer.liveState();
  return ls.some((e) => e.detail.canReload === false) && ls.length && ls[ls.length - 1].detail.canReload === true &&
    now && now.canReload === true && typeof now.total === 'number' && 'generation' in now && 'live' in now;
}));
if (shellHosts) check('7 hosted: no viewer live chip (the shell chip owns live-index state)', await host.evaluate(() => !document.getElementById('ft-live-chip')));
// a NEW unsaved mark, then destroy immediately — before the 2.5 s autosave
const third = await activeId(host);
await host.keyboard.press('3');
check('7 key 3 marks the next card (hosted)', /needs_verification/.test(await markOf(host, third)) && await host.evaluate(() => FTViewer.saveState()) === 'pending');
await host.evaluate(() => FTViewer.destroy());
check('7 destroy(): markup kept, rendered state gone, destroyed event', await host.evaluate(() => !!document.getElementById('recordGrid') && document.querySelectorAll('#recordGrid .card').length === 0 && window.__ev.some((e) => e.type === 'destroyed') && FTViewer.saveState() === null));
await host.focus(stripBtnSel);
await host.keyboard.press('j');
await host.keyboard.press('1');
check('7 destroyed viewer ignores keys (no cards, no errors)', await host.evaluate(() => document.querySelectorAll('#recordGrid .card').length === 0));
await host.waitForTimeout(3000); // past the old mount's autosave delay
await host.evaluate(() => FTViewer.init({ embed: true, host: document.getElementById('hViewerHost') }));
await host.waitForSelector('#recordGrid .card', { timeout: 30000 });
check('7 re-init: unsaved mark restored from drafts', /needs_verification/.test(await markOf(host, third)) && await host.evaluate(() => FTViewer.summary().pending > 0), await host.evaluate(() => JSON.stringify(FTViewer.summary())));
check('7 save() resolves true and the indicator turns saved', await host.evaluate(() => FTViewer.save({ patient: true })) === true && await host.evaluate(() => FTViewer.saveState()) === 'saved');
const meta = await server.getJson('/api/records-meta');
const dbMarks = (meta.annotations && meta.annotations.marks) || [];
const byId = (id) => dbMarks.find((m) => (m.record_id || m.id) === id);
check('7 both marks reached the case DB with the examiner', byId(second)?.status === 'important' && byId(third)?.status === 'needs_verification' && byId(third)?.examiner === 'T5 Examiner', JSON.stringify(dbMarks));
check('7 reload() after save: reloaded in place (no page load)', await host.evaluate(async () => { window.__stay = 1; return (await FTViewer.reload()) === 'reloaded' && window.__stay === 1; }));
check('2 reload() keeps the host-controlled .popped class (and the region role)', await host.evaluate(async () => {
  const r = document.getElementById('ftViewer');
  r.classList.add('popped');
  const ok = (await FTViewer.reload()) === 'reloaded' && r.classList.contains('popped') && r.classList.contains('hosted');
  r.classList.remove('popped');
  return ok && document.getElementById('mainArea').getAttribute('role') === 'region';
}));
await host.waitForSelector('#recordGrid .card', { timeout: 30000 });
// a destroyed mount's listeners are gone: one j press moves exactly one card
await host.evaluate(() => FTViewer.focus());
const a0 = await activeId(host);
const ids = await host.evaluate(() => [...document.querySelectorAll('#recordGrid .card')].map((c) => c.dataset.id));
await host.keyboard.press('j');
check('7 after destroy/init cycles one j moves exactly one card', await activeId(host) === ids[ids.indexOf(a0) + 1], `${a0} → ${await activeId(host)}`);
await host.keyboard.press('x');
check('7 selection event on x', await host.evaluate(() => window.__ev.some((e) => e.type === 'selection' && e.detail.count === 1)));

// 3 single writer: only the window that opened the pop-out (token) answers on ft-player:<case>
const chanProbe = await host.evaluate(async () => {
  const key = Object.keys(localStorage).find((k) => /^ft\.viewer\..*\.examiner$/.test(k));
  const caseId = key.slice('ft.viewer.'.length, -'.examiner'.length);
  const ch = new BroadcastChannel(FTViewer.channels.player(caseId));
  const got = [];
  ch.onmessage = (e) => got.push(e.data);
  const ask = async (seq, token) => { ch.postMessage(token === undefined ? { type: 'list', seq } : { type: 'list', seq, token }); await new Promise((r) => setTimeout(r, 300)); return got.filter((m) => m.seq === seq).length; };
  const out = { none: await ask(1), wrong: await ask(2, 'other-tab-token'), mine: await ask(3, FTViewer.writerToken) };
  ch.close();
  return out;
});
check('3 ft-player channel: tokenless / foreign-token requests get no answer; the owner token does', chanProbe.none === 0 && chanProbe.wrong === 0 && chanProbe.mine === 1, JSON.stringify(chanProbe));

// 5 examiner attribution: marks saved without an examiner get one later
// Use a card that has never been marked: marks saved earlier in this run
// already carry an examiner (and, since hydration reads `record_id`, keep it
// after reload), so they are not "unassigned".
const lateId = await host.evaluate(() => {
  FTViewer.setExaminer('');
  const card = [...document.querySelectorAll('#recordGrid .card')].find((c) => !c.querySelector('.mark-chip'));
  return card ? card.dataset.id : null;
});
check('5 a never-marked card is available for the attribution test', !!lateId, String(lateId));
await host.click(`#recordGrid .card[data-id="${lateId}"]`);
await host.evaluate(() => FTViewer.focus());
await host.keyboard.press('1');
const lateEx = await host.evaluate(() => FTViewer.attributeUnassignedMarks('Late Examiner'));
check('5 attributeUnassignedMarks(): re-pushes the unassigned mark, sets the examiner', lateEx >= 1 && await host.evaluate(() => FTViewer.getExaminer()) === 'Late Examiner', String(lateEx));
await host.waitForFunction(() => FTViewer.saveState() === 'saved', null, { timeout: 15000 });
const meta2 = await server.getJson('/api/records-meta');
const lateRow = ((meta2.annotations && meta2.annotations.marks) || []).find((m) => (m.record_id || m.id) === lateId);
check('5 the case DB holds the late examiner on that mark', lateRow && lateRow.examiner === 'Late Examiner', JSON.stringify(lateRow));
check('5 attributeUnassignedMarks() is a no-op once everything is attributed', await host.evaluate(() => FTViewer.attributeUnassignedMarks('Late Examiner')) === 0);

// locale / theme / toast
await host.evaluate(() => FTViewer.setLocale('en'));
check('7 setLocale(en): getLocale + locale event + chrome translated', await host.evaluate(() => FTViewer.getLocale() === 'en' && window.__ev.some((e) => e.type === 'locale' && e.detail.locale === 'en') && document.getElementById('btnApplyMarks').textContent.trim() !== '케이스에 반영'));
await host.evaluate(() => FTViewer.setLocale('ko'));
await host.evaluate(() => FTViewer.setTheme('dark'));
check('7 setTheme(dark): <html data-theme> + theme event', await host.evaluate(() => document.documentElement.dataset.theme === 'dark' && FTViewer.getTheme() === 'dark' && window.__ev.some((e) => e.type === 'theme' && e.detail.theme === 'dark')));
await host.evaluate(() => FTViewer.setTheme('light'));
await host.evaluate(() => FTViewer.toast('T5 toast'));
check('7 toast(): one #toastHost inside the root + toast event', await host.evaluate(() => document.querySelectorAll('#toastHost').length === 1 && !!document.querySelector('#ftViewer #toastHost .toast') && window.__ev.some((e) => e.type === 'toast' && e.detail.message === 'T5 toast')));

// request-popout (hosted, a listener present) and popout()/closePopout()
await host.evaluate(() => { window.__opened = 0; const o = window.open; window.open = (...a) => { window.__opened++; return o.apply(window, a); }; FTViewer.on('request-popout', () => {}); });
const clickPop = () => host.click('#btnPopPlayer').catch(async () => { await host.evaluate(() => document.getElementById('btnPopPlayer').click()); });
let popup;
if (shellHosts) {
  // The integrated shell answers request-popout itself (23-popout.js calls
  // FTViewer.popout() once and folds the media column): exactly one window.
  await clickPop();
  await host.waitForFunction(() => window.__opened >= 1 && FTStore.state.popout.open === true, null, { timeout: 10000 }).catch(() => {});
  check('7 hosted btnPopPlayer emits request-popout; the shell opens one pop-out', await host.evaluate(() => window.__opened === 1 && window.__ev.some((e) => e.type === 'request-popout') && FTStore.state.popout.open === true));
} else {
  await clickPop();
  check('7 hosted btnPopPlayer emits request-popout instead of opening', await host.evaluate(() => window.__opened === 0 && window.__ev.some((e) => e.type === 'request-popout')));
  [popup] = await Promise.all([host.waitForEvent('popup', { timeout: 10000 }), host.evaluate(() => { FTViewer.popout(); })]);
}
if (popup) await popup.waitForLoadState().catch(() => {});
check('7 popout(): popup open, isPoppedOut + popout{open:true}', await host.evaluate(() => FTViewer.isPoppedOut() && window.__ev.some((e) => e.type === 'popout' && e.detail.open === true)));
await host.evaluate(() => FTViewer.closePopout());
await host.waitForTimeout(300);
check('7 closePopout(): popout{open:false}', await host.evaluate(() => !FTViewer.isPoppedOut() && window.__ev.some((e) => e.type === 'popout' && e.detail.open === false)));

// 6 computed style: hosted vs standalone
const SPOTS = ['.ft-viewer .card', '.ft-viewer .topbar', '#selectionBar button.primary', '#exportMenuList', '.mark-chip.important', '#toastHost .toast'];
const styleOf = (page) => page.evaluate((sels) => sels.map((s) => {
  const el = document.querySelector(s);
  if (!el) return [s, null];
  const cs = getComputedStyle(el);
  return [s, [cs.color, cs.backgroundColor, cs.height, cs.borderTopLeftRadius, cs.fontSize, cs.fontFamily].join(' | ')];
}), SPOTS);
const standalone = await openPage('/review/evidence-viewer.html');
await standalone.waitForSelector('#recordGrid .card', { timeout: 30000 });
await standalone.evaluate(() => FTViewer.toast('T5 toast'));
await host.evaluate(() => FTViewer.toast('T5 toast'));
// the same card marked important on both pages (already in the DB)
await standalone.evaluate(() => FTViewer.focus());
const hs = await styleOf(host);
const ss = await styleOf(standalone);
for (let i = 0; i < SPOTS.length; i++) {
  const [sel, h] = hs[i];
  const s = ss[i][1];
  if (h === null && s === null) { note(`6 ${sel} not rendered on either page`); continue; }
  check(`6 ${sel} computed style hosted == standalone`, h === s, h === s ? '' : `hosted ${h} / standalone ${s}`);
}
check('9 standalone page has exactly one main landmark (#mainArea role=main, not nested)', await standalone.evaluate(() => {
  const m = document.querySelectorAll('main, [role="main"]');
  return m.length === 1 && m[0].id === 'mainArea';
}));
if (axeSource) {
  await standalone.evaluate(axeSource);
  const axeRes = async (page) => page.evaluate(async () => (await window.axe.run(document, { runOnly: { type: 'rule', values: ['landmark-no-duplicate-main', 'landmark-main-is-top-level', 'landmark-one-main', 'region'] } })).violations.map((v) => v.id + ':' + v.nodes.length));
  const sv = (await axeRes(standalone)).filter((v) => !v.startsWith('region'));
  check('9 axe landmark rules clean on standalone', sv.length === 0, sv.join(','));
  await host.evaluate(axeSource);
  const hv = (await axeRes(host)).filter((v) => !v.startsWith('region'));
  check('9 axe landmark rules clean on hosted', hv.length === 0, hv.join(','));
} else note('9 axe-core not resolvable: DOM landmark counts only');
check('2 standalone page is not hosted, embed off at top level', await standalone.evaluate(() => !document.getElementById('ftViewer').classList.contains('hosted') && !document.getElementById('ftViewer').classList.contains('embed')));
await standalone.close();
await host.close();
void mountOpts;
check('no page errors (hosted + standalone)', errors.length === 0, errors.join(' | '));

// ---------------------------------------------------------------- legacy
server.stop();
server = null;
try { legacy = await startIsolatedServer({ root, work: join(work, 'legacy'), env: { FRAMETRACE_UI: 'legacy', ...pagesKey } }); } catch (e) { await die('legacy server: ' + e.message); }
const legacyHtml = await (await fetch(legacy.base + '/', { headers: legacy.headers })).text();
check('8 FRAMETRACE_UI=legacy: GET / has #stageNav and the review iframe', legacyHtml.includes('id="stageNav"') && legacyHtml.includes('<iframe id="reviewFrame"'));
check('8 legacy page carries no fragment of its own', !legacyHtml.includes('id="ftViewer"'));
const opened = await legacy.postJson('/api/open-case', { case_dir: caseDir });
check('8 legacy server opens the case', !!opened.ok, opened.error || '');
const lctx = await browser.newContext({ viewport: { width: 1366, height: 768 } });
const lpage = await lctx.newPage();
const lerr = [];
lpage.on('pageerror', (e) => lerr.push(e.message));
await lpage.goto(`${legacy.base}/?token=${legacy.token}`);
let frameOk = false;
let frameUrl = '';
for (const deadline = Date.now() + 30000; Date.now() < deadline && !frameOk; await lpage.waitForTimeout(300)) {
  // the stage-3 button opens the review stage once the case is loaded
  await lpage.evaluate(() => { const b = document.querySelector('.navstep[data-view="3"]:not([disabled])'); if (b && !document.getElementById('resultCard').classList.contains('on')) b.click(); }).catch(() => {});
  for (const f of lpage.frames()) {
    if (!/evidence-viewer\.html|\/viewer\//.test(f.url())) continue;
    frameUrl = f.url();
    frameOk = await f.evaluate(() => {
      const r = document.getElementById('ftViewer');
      return !!r && r.classList.contains('embed') && !r.classList.contains('hosted') && document.querySelectorAll('#recordGrid .card').length > 0;
    }).catch(() => false);
  }
}
check('8 legacy iframe loads the standalone viewer with ?embed=1 → .ft-viewer.embed + cards', frameOk && /embed=1/.test(frameUrl), frameUrl);
check('8 no page errors (legacy)', lerr.length === 0, lerr.join(' | '));
await lctx.close();

await cleanup();
console.log(failures ? `\n${failures} check(s) FAILED` : '\nverify-embed: all checks passed');
process.exit(failures ? 1 : 0);
