// Layout gate for the integrated workstation screen
// (docs/ui-rebuild-contract.md §6, §8.2 verify-layout) against the REAL
// server in a real browser, isolated config, two synthetic cases:
//
//   1. region rects: #hTopStrip 44px; hosted viewer: #leftRail 240/280,
//      section.media 480/640, #mediaStage 270/360, #selectionBar <= 52,
//      grid column >= 600/960 (1366x768 / 1920x1080)
//   2. no horizontal scroll; every top-strip control inside the viewport,
//      >= 24px and not covered (elementFromPoint at its centre)
//   3. empty state: two blocks, [이어서 검토] primary >= 36px, column <= 760
//      and centred
//   4. sheets: the grid does not reflow when a sheet opens, sheet width
//      480/560, focus moves into the sheet, Esc closes it and returns focus
//      to the opener, the deliverables sheet makes #main inert
//   5. three toasts never intersect the top strip, the viewer top bar, the
//      selection bar or an open sheet
//   6. pop-out (window.open stubbed): note shown in the media column's
//      place, single writer, a second shell tab hears "popout-open" on the
//      ft-review channel and disables its own pop-out button
//   7. project switch between the two cases: no page load, the grid's card
//      count changes
//   8. Alt+P / Alt+J / Alt+R toggle; Alt+1 does nothing
//
// Checks 1-5 run at 1366x768 and 1920x1080 in light and dark; 6-8 once.
// Until the viewer fragment is hosted in the page (T5), the review page is
// a frame in the same mount: viewer-internal rects are then reported as
// SKIP and the frame box stands in for the grid.
//
//   node scripts/verify-layout.mjs [--keep] [--shots DIR]
import { mkdtempSync, mkdirSync, rmSync, copyFileSync, readdirSync } from 'node:fs';
import { join, dirname } from 'node:path';
import { tmpdir } from 'node:os';
import { fileURLToPath } from 'node:url';
import { spawnSync } from 'node:child_process';
import { createRequire } from 'node:module';
import { startIsolatedServer } from './lib/isolated-server.mjs';

const root = join(dirname(fileURLToPath(import.meta.url)), '..');
const argv = process.argv.slice(2);
const shotsIdx = argv.indexOf('--shots');
const shots = shotsIdx >= 0 ? argv[shotsIdx + 1] : '';
let chromium;
try { ({ chromium } = createRequire(import.meta.url)('playwright')); } catch {
  console.error('playwright not resolvable — set NODE_PATH to a node_modules with playwright');
  process.exit(2);
}

const results = [];
const check = (name, ok, detail = '') => {
  results.push({ name, ok: !!ok });
  console.log(`${ok ? 'PASS' : 'FAIL'}  ${name}${detail ? '  — ' + detail : ''}`);
  return !!ok;
};
const skip = (name, why) => console.log(`SKIP  ${name}  — ${why}`);
const sleep = (ms) => new Promise((r) => setTimeout(r, ms));
async function until(fn, ms, step = 200) {
  const end = Date.now() + ms;
  for (;;) {
    let v;
    try { v = await fn(); } catch { v = null; }
    if (v) return v;
    if (Date.now() > end) return null;
    await sleep(step);
  }
}

// ---- two cases: the demo source (12 clips) and a 3-clip subset ----------
const work = mkdtempSync(join(tmpdir(), 'ft-layout-'));
const srcA = join(work, 'src-a');
const srcB = join(work, 'src-b');
const demo = spawnSync(process.execPath, [join(root, 'scripts/make-demo-case.mjs'), srcA], { stdio: 'inherit' });
if (demo.status !== 0) { console.error('demo source generation failed'); process.exit(1); }
mkdirSync(join(srcB, 'DCIM'), { recursive: true });
const front = join(srcA, 'DCIM', 'FRONT');
for (const f of readdirSync(front).slice(0, 3)) copyFileSync(join(front, f), join(srcB, 'DCIM', f));

const server = await startIsolatedServer({ root, work });
const { base, token, getJson, postJson } = server;
let browser;
const pageErrors = [];
process.on('exit', () => {
  try { browser && browser.close(); } catch { /* closed */ }
  server.stop();
  if (!argv.includes('--keep')) { try { rmSync(work, { recursive: true, force: true }); } catch { /* busy */ } }
});
process.on('SIGINT', () => process.exit(130));

async function analyse(src, caseDir) {
  const res = await postJson('/api/start', { input_kind: 'folder', source_path: src, case_dir: caseDir, with_hash: false, with_ffprobe: true });
  if (!res.ok) throw new Error('start: ' + res.error);
  const st = await until(async () => { const s = await getJson('/api/status'); return s.phase === 'review-ready' && !s.busy ? s : (s.phase === 'error' ? Promise.reject(new Error(s.error)) : null); }, 240000, 400);
  if (!st) throw new Error('analysis timed out: ' + caseDir);
}
const caseA = join(work, 'cases', 'case-a');
const caseB = join(work, 'cases', 'case-b');

// The window.open stub for the pop-out: a fake window that only records.
const POPOUT_STUB = () => {
  window.__opened = [];
  window.open = (url, name, feat) => {
    const w = { url, name, feat, closed: false, document: document.implementation.createHTMLDocument(''), focus() {}, close() { this.closed = true; }, postMessage() {}, addEventListener() {}, removeEventListener() {} };
    window.__opened.push(w);
    return w;
  };
};

// ---- in-page probes -------------------------------------------------------
const rectOf = (el) => { if (!el) return null; const r = el.getBoundingClientRect(); return { top: r.top, bottom: r.bottom, left: r.left, right: r.right, w: r.width, h: r.height }; };
const probe = () => {
  const box = (el) => { if (!el || !el.getClientRects().length) return null; const r = el.getBoundingClientRect(); return { top: r.top, bottom: r.bottom, left: r.left, right: r.right, w: r.width, h: r.height }; };
  const host = document.getElementById('hViewerHost');
  const hosted = !!document.querySelector('[data-ft-viewer="2"]');
  const frame = host ? host.querySelector('iframe') : null;
  const q = (sel) => (hosted ? document.querySelector(sel) : null);
  const strip = document.getElementById('hTopStrip');
  const stripCtl = [...strip.querySelectorAll('button')].filter((b) => b.getClientRects().length && getComputedStyle(b).visibility !== 'hidden');
  const covered = [], small = [], outside = [];
  for (const b of stripCtl) {
    const r = b.getBoundingClientRect();
    if (r.height < 24 || r.width < 24) small.push(`${b.id} ${Math.round(r.width)}x${Math.round(r.height)}`);
    if (r.left < 0 || r.right > innerWidth + 0.5 || r.top < 0 || r.bottom > innerHeight + 0.5) outside.push(b.id);
    const hit = document.elementFromPoint(r.left + r.width / 2, r.top + r.height / 2);
    if (!hit || !(hit === b || b.contains(hit))) covered.push(`${b.id}→${hit ? (hit.id || hit.className || hit.tagName) : 'null'}`);
  }
  return {
    vw: innerWidth, vh: innerHeight, hosted, hasFrame: !!frame,
    hscroll: document.documentElement.scrollWidth > innerWidth + 1,
    strip: box(strip), covered, small, outside,
    host: box(host),
    rail: box(q('#leftRail')), media: box(q('section.media')), stage: box(q('#mediaStage')),
    selection: box(q('#selectionBar')), browse: box(q('section.browse')), grid: box(q('#recordGrid')), topbar: box(q('.ft-viewer .topbar')),
    frame: box(frame),
  };
};

function checkRegions(label, g, wide) {
  check(`${label}: #hTopStrip is 44px`, g.strip && Math.round(g.strip.h) === 44, JSON.stringify(g.strip && g.strip.h));
  check(`${label}: no horizontal scroll`, !g.hscroll);
  check(`${label}: top-strip controls inside the viewport, >= 24px, not covered`, !g.covered.length && !g.small.length && !g.outside.length, [...g.covered, ...g.small, ...g.outside].join(', '));
  check(`${label}: host fills the screen under the strip`, g.host && Math.abs(g.host.top - 44) < 1 && Math.abs(g.host.bottom - g.vh) < 1 && Math.abs(g.host.w - g.vw) < 1, JSON.stringify(g.host));
  if (!g.hosted) { skip(`${label}: viewer regions (rail/media/stage/selection/grid)`, 'legacy viewer frame (viewer fragment not hosted yet)'); return; }
  const [rail, media, stage, grid] = wide ? [280, 640, 360, 960] : [240, 480, 270, 600];
  check(`${label}: #leftRail ${rail}px`, g.rail && Math.round(g.rail.w) === rail, JSON.stringify(g.rail && g.rail.w));
  check(`${label}: section.media ${media}px`, g.media && Math.round(g.media.w) === media, JSON.stringify(g.media && g.media.w));
  check(`${label}: #mediaStage ${stage}px tall`, g.stage && Math.abs(g.stage.h - stage) <= 1, JSON.stringify(g.stage && g.stage.h));
  check(`${label}: #selectionBar <= 52px`, g.selection && g.selection.h <= 52, JSON.stringify(g.selection && g.selection.h));
  check(`${label}: grid column >= ${grid}px`, g.browse && g.browse.w >= grid, JSON.stringify(g.browse && g.browse.w));
}
const intersects = (a, b) => !!a && !!b && a.left < b.right - 0.5 && b.left < a.right - 0.5 && a.top < b.bottom - 0.5 && b.top < a.bottom - 0.5;

async function openProject(page) {
  await page.waitForSelector('#hViewerHost:not([hidden]), #inputCard:not([hidden])', { timeout: 20000 });
  if (await page.locator('#inputCard').isVisible()) {
    await page.locator('#recentCaseRow li').first().locator('button.primary').click();
  }
  await page.waitForSelector('#hViewerHost:not([hidden])', { timeout: 20000 });
  await until(() => cardCount(page), 30000);
  await sleep(600);
}
const cardCount = (page) => page.evaluate(() => {
  const host = document.getElementById('hViewerHost');
  if (!host || host.hidden) return 0;
  let n = host.querySelectorAll('#recordGrid .card').length;
  const fr = host.querySelector('iframe');
  try { if (!n && fr && !fr.hidden && fr.contentDocument) n = fr.contentDocument.querySelectorAll('#recordGrid .card').length; } catch { /* no access */ }
  return n;
});
const gridBox = async (page) => page.evaluate(() => {
  const el = document.querySelector('[data-ft-viewer="2"] #recordGrid') || document.querySelector('#hViewerHost iframe');
  if (!el) return null;
  const r = el.getBoundingClientRect();
  return { top: r.top, left: r.left, w: r.width, h: r.height };
});

try {
  await analyse(srcA, caseA);
  await analyse(srcB, caseB);
  try { browser = await chromium.launch({ channel: 'chrome' }); } catch { browser = await chromium.launch(); }
  let once = true;
  for (const [w, h] of [[1366, 768], [1920, 1080]]) {
    for (const theme of ['light', 'dark']) {
      const label = `${w}x${h} ${theme}`;
      const wide = w >= 1600;
      const context = await browser.newContext({ viewport: { width: w, height: h }, colorScheme: theme });
      await context.addInitScript((t) => { try { localStorage.setItem('ft-theme', t); } catch { /* blocked */ } }, theme);
      await context.addInitScript(POPOUT_STUB);
      const page = await context.newPage();
      page.on('pageerror', (e) => pageErrors.push(`${label}: ${e.message}`));
      page.on('console', (m) => { if (m.type() === 'error' && !/favicon|Failed to load resource/.test(m.text())) pageErrors.push(`${label}: console ${m.text()}`); });
      await page.goto(`${base}/?token=${token}`);
      await openProject(page);

      // 1/2. regions
      checkRegions(label, await page.evaluate(probe), wide);
      if (shots) { mkdirSync(shots, { recursive: true }); await page.screenshot({ path: join(shots, `project-${w}-${theme}.png`) }); }

      // 4. sheets
      for (const [which, opener, sheetId] of [['jobs', '#hBtnJobs', '#hJobsSheet'], ['finalize', '#hBtnFinalize', '#hFinalizeSheet']]) {
        const before = await gridBox(page);
        await page.click(opener);
        await page.waitForSelector(`${sheetId}:not([hidden])`);
        await sleep(150);
        const st = await page.evaluate((id) => {
          const s = document.querySelector(id);
          const r = s.getBoundingClientRect();
          return { w: r.width, top: r.top, right: innerWidth - r.right, focusIn: s.contains(document.activeElement), inert: !!document.getElementById('main').inert, stripInert: !!document.getElementById('hTopStrip').inert };
        }, sheetId);
        const after = await gridBox(page);
        check(`${label}: ${which} sheet — grid does not reflow`, before && after && Math.abs(before.w - after.w) < 1 && Math.abs(before.left - after.left) < 1 && Math.abs(before.h - after.h) < 1, `${JSON.stringify(before)} → ${JSON.stringify(after)}`);
        check(`${label}: ${which} sheet ${wide ? 560 : 480}px at the right edge under the strip`, Math.round(st.w) === (wide ? 560 : 480) && Math.abs(st.right) < 1 && Math.abs(st.top - 44) < 1, JSON.stringify(st));
        check(`${label}: ${which} sheet takes focus`, st.focusIn);
        if (which === 'finalize') check(`${label}: deliverables sheet is modal (#main and strip inert)`, st.inert && st.stripInert);
        else check(`${label}: jobs sheet is non-modal (#main not inert)`, !st.inert);
        if (shots && which === 'finalize' && once) await page.screenshot({ path: join(shots, `finalize-${w}-${theme}.png`) });
        // 5. toasts with the sheet open
        if (which === 'jobs') {
          await page.evaluate(() => { for (let i = 1; i <= 3; i++) toast(`레이아웃 확인용 알림 ${i} — 버튼을 가리지 않는지 확인합니다`, i === 3 ? 'err' : 'info'); });
          await sleep(200);
          const t = await page.evaluate(() => {
            const box = (el) => { if (!el || !el.getClientRects().length) return null; const r = el.getBoundingClientRect(); return { top: r.top, bottom: r.bottom, left: r.left, right: r.right }; };
            const hosted = !!document.querySelector('[data-ft-viewer="2"]');
            const host = document.getElementById('hViewerHost').getBoundingClientRect();
            const toasts = [...document.querySelectorAll('.h-toast, #toastHost > *')].map(box).filter(Boolean);
            return {
              toasts,
              strip: box(document.getElementById('hTopStrip')),
              sheet: box(document.getElementById('hJobsSheet')),
              topbar: hosted ? box(document.querySelector('.ft-viewer .topbar')) : { top: host.top, bottom: host.top + 40, left: host.left, right: host.right },
              selection: hosted ? box(document.getElementById('selectionBar')) : { top: host.bottom - 44, bottom: host.bottom, left: host.left, right: host.right },
            };
          });
          const hits = [];
          for (const r of t.toasts) for (const k of ['strip', 'sheet', 'topbar', 'selection']) if (intersects(r, t[k])) hits.push(k);
          check(`${label}: 3 toasts clear of strip / viewer top bar / selection bar / sheet`, t.toasts.length === 3 && hits.length === 0, `${t.toasts.length} toasts; hits: ${hits.join(',') || 'none'}`);
          if (shots && once) await page.screenshot({ path: join(shots, `toasts-${w}-${theme}.png`) });
          await page.evaluate(() => { while (dismissNewestToast()) { /* clear */ } });
        }
        const focusBefore = await page.evaluate((id) => document.querySelector(id).contains(document.activeElement), sheetId);
        if (!focusBefore) await page.focus(`${sheetId} button`);
        await page.keyboard.press('Escape');
        const closed = await page.evaluate(([id, op]) => ({ hidden: document.querySelector(id).hidden, focus: document.activeElement && ('#' + document.activeElement.id) === op }), [sheetId, opener]);
        check(`${label}: Esc closes the ${which} sheet and returns focus to ${opener}`, closed.hidden && closed.focus, JSON.stringify(closed));
      }

      // 3. empty state with a previous project
      await page.click('#hProjectBtn');
      await page.click('#hProjectNew');
      await page.waitForSelector('#inputCard:not([hidden])');
      await until(() => page.locator('#recentCaseRow li').count(), 8000);
      const e = await page.evaluate(() => {
        const vis = (el) => !!el && el.getClientRects().length > 0;
        const blocks = [...document.querySelectorAll('#inputCard .sblock')].filter(vis).map((b) => { const r = b.getBoundingClientRect(); return { left: r.left, right: r.right, w: r.width, top: r.top, bottom: r.bottom }; });
        const top = document.querySelector('#recentCaseRow li button.primary');
        const r = top ? top.getBoundingClientRect() : null;
        return { vw: innerWidth, vh: innerHeight, blocks, top: top ? { text: top.textContent.trim(), h: r.height, bottom: r.bottom } : null, hscroll: document.documentElement.scrollWidth > innerWidth + 1 };
      });
      check(`${label}: empty state has exactly two blocks`, e.blocks.length === 2, JSON.stringify(e.blocks.length));
      check(`${label}: column <= 760px and centred`, e.blocks.every((b) => b.w <= 760.5 && Math.abs((b.left + b.right) / 2 - e.vw / 2) < 2), JSON.stringify(e.blocks.map((b) => [Math.round(b.left), Math.round(b.right)])));
      check(`${label}: [이어서 검토] is the block's primary, >= 36px, inside the viewport`, e.top && e.top.text === '이어서 검토' && e.top.h >= 36 && e.top.bottom <= e.vh, JSON.stringify(e.top));
      check(`${label}: empty state has no horizontal scroll`, !e.hscroll);
      if (shots) await page.screenshot({ path: join(shots, `empty-${w}-${theme}.png`) });
      await openProject(page);

      if (once) {
        once = false;
        // 6. pop-out + second tab on the ft-review channel
        const peer = await context.newPage();
        peer.on('pageerror', (er) => pageErrors.push(`peer: ${er.message}`));
        await peer.goto(`${base}/?token=${token}`);
        await openProject(peer);
        await peer.evaluate(() => { window.__heard = []; const c = new BroadcastChannel('ft-review'); c.onmessage = (m) => window.__heard.push(m.data); });
        await page.bringToFront();
        const before = await gridBox(page);
        const mediaBefore = await page.evaluate(() => { const m = document.querySelector('[data-ft-viewer="2"] section.media'); if (!m) return null; const r = m.getBoundingClientRect(); return { left: r.left, w: r.width }; });
        await page.click('#hBtnPopout');
        await sleep(300);
        const p = await page.evaluate(() => {
          const note = document.getElementById('hPopoutNote');
          const r = note.getBoundingClientRect();
          const root = document.querySelector('[data-ft-viewer="2"]');
          const fr = document.querySelector('#hViewerHost iframe');
          return { opened: (window.__opened || []).length, noteVisible: !note.hidden && r.width > 0, note: { left: r.left, w: r.width }, popped: root ? root.classList.contains('popped') : null, frameHidden: fr ? fr.hidden : null };
        });
        check('popout: window opened once (stub) and the note is shown', p.opened === 1 && p.noteVisible, JSON.stringify(p));
        if (p.popped === null) {
          check('popout (legacy frame): the parked frame is hidden behind the note — one writer', p.frameHidden === true);
        } else {
          check('popout: .ft-viewer.popped on the hosted viewer', p.popped === true);
          check('popout: note sits in the media column (same place, same width)', mediaBefore && Math.abs(p.note.left - mediaBefore.left) < 2 && Math.abs(p.note.w - mediaBefore.w) < 2, `${JSON.stringify(mediaBefore)} vs ${JSON.stringify(p.note)}`);
          const after = await gridBox(page);
          check('popout: grid keeps (or gains) its width', before && after && after.w >= before.w - 1, `${before && before.w} → ${after && after.w}`);
        }
        const heard = await until(() => peer.evaluate(() => (window.__heard || []).find((m) => m.type === 'popout-open')), 3000);
        check('popout: second tab hears popout-open on ft-review', !!heard && typeof heard.from === 'string', JSON.stringify(heard));
        const peerBtn = await peer.evaluate(() => { const b = document.getElementById('hBtnPopout'); return { disabled: b.disabled, title: b.title }; });
        check('popout: second tab disables its own pop-out button', peerBtn.disabled && /다른 창에서 열림/.test(peerBtn.title), JSON.stringify(peerBtn));
        if (shots) await page.screenshot({ path: join(shots, `popout-${w}-${theme}.png`) });
        await page.click('#hBtnPopoutReturn');
        await sleep(300);
        check('popout: 여기로 되돌리기 restores the viewer here', await page.locator('#hPopoutNote').isHidden());
        const back = await until(() => peer.evaluate(() => (window.__heard || []).some((m) => m.type === 'popout-closed') && !document.getElementById('hBtnPopout').disabled), 3000);
        check('popout: second tab hears popout-closed and re-enables', !!back);
        await peer.close();

        // 7. project switch without a page load
        let loads = 0;
        const onLoad = () => { loads += 1; };
        page.on('load', onLoad);
        const n0 = await cardCount(page);
        await page.click('#hProjectBtn');
        const row = page.locator('#hProjectRecent button').first();
        await row.waitFor({ timeout: 8000 });
        await row.click();
        const n1 = await until(async () => { const n = await cardCount(page); return n && n !== n0 ? n : null; }, 30000);
        check('switch: the other case opens in place (card count changes)', !!n1, `${n0} → ${n1}`);
        check('switch: no page load', loads === 0, `${loads} load events`);
        const name = (await page.locator('#hProjectName').textContent()).trim();
        check('switch: the top strip names the new project', /case-[ab]/.test(name) && n1 && name !== '', name);
        page.off('load', onLoad);

        // 8. shell shortcuts
        await page.locator('#hBtnTheme').focus();
        const panels = () => page.evaluate(() => ({ project: !document.getElementById('hProjectPop').hidden, jobs: !document.getElementById('hJobsSheet').hidden, finalize: !document.getElementById('hFinalizeSheet').hidden }));
        await page.keyboard.press('Alt+KeyP');
        const ap = await panels();
        await page.keyboard.press('Alt+KeyP');
        const ap2 = await panels();
        check('keys: Alt+P toggles the project popover', ap.project && !ap2.project, JSON.stringify([ap, ap2]));
        await page.keyboard.press('Alt+KeyJ');
        const aj = await panels();
        await page.keyboard.press('Alt+KeyJ');
        const aj2 = await panels();
        check('keys: Alt+J toggles the jobs sheet', aj.jobs && !aj2.jobs, JSON.stringify([aj, aj2]));
        await page.keyboard.press('Alt+KeyR');
        const ar = await panels();
        await page.keyboard.press('Escape');
        const ar2 = await panels();
        check('keys: Alt+R opens the deliverables sheet (Esc closes)', ar.finalize && !ar2.finalize, JSON.stringify([ar, ar2]));
        const url0 = page.url();
        await page.locator('#hBtnTheme').focus();
        await page.keyboard.press('Alt+Digit1');
        await sleep(200);
        const a1 = await panels();
        check('keys: Alt+1 does nothing (no stages)', !a1.project && !a1.jobs && !a1.finalize && page.url() === url0 && await page.locator('#hViewerHost').isVisible());
        await page.locator('#sourcePath').evaluate(() => {}).catch(() => {});
      }
      await context.close();
    }
  }
  check('no page errors', pageErrors.length === 0, pageErrors.slice(0, 5).join(' | '));
} catch (err) {
  console.error(err);
  check('harness ran to completion', false, String(err && err.message || err));
}
const failed = results.filter((r) => !r.ok);
console.log(`\nverify-layout: ${results.length - failed.length}/${results.length} passed`);
process.exit(failed.length ? 1 : 0);
