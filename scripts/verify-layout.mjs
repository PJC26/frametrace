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
//   … 9–11 (keys, hosted reload, 401 banner: see the sections below)
//  12. R4 A12: Ctrl+K does not open the viewer palette over a shell dialog
//  13. R4 A15: hBeforeClose waits for the save in flight; a failing save →
//      ok:false with "저장 안 된 편집 N건"
//  14. R4 B2: ?launch= (and ?token=) leave the address bar — shell + /viewer/
//  R8 U3: a real save conflict at 1366x768 — the conflict list opens as the
//      notices row's sibling, Tab reaches its buttons (on screen, not
//      covered, one scroll container), also with five long notices
//
// Checks 1-5 run at 1366x768 and 1920x1080 in light and dark; 6-8 once.
// The viewer fragment must be hosted in the page (T5): a legacy frame in
// its place FAILS (F4).
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
  // F4: the viewer fragment is hosted in the shell page — a legacy frame is a failure.
  if (!check(`${label}: viewer fragment hosted in the page (no legacy frame)`, g.hosted)) return;
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
      // R7 U9: five long notices stacked in the viewer's notices row (save
      // conflicts, unreadable queue files, truncated lists, …) must not
      // squeeze the work area: the row stops at min(28vh, 216px) and
      // scrolls, the grid keeps its height, the selection bar stays on
      // screen, nothing scrolls sideways, the last notice is reachable.
      if (!wide) {
        const n9 = await page.evaluate(() => {
          const root = document.getElementById('ftViewer');
          if (!root) return { missing: true };
          let host = root.querySelector(':scope > .ft-notices');
          const made = !host;
          if (made) { host = document.createElement('div'); host.className = 'ft-notices'; root.insertBefore(host, root.firstChild); }
          const long = '레이아웃 확인용 알림 — 저장 충돌, 읽지 못한 대기 파일, 일부만 읽은 목록처럼 알림이 여러 개 겹쳐도 작업 영역을 밀어내지 않는지 확인합니다. '.repeat(4);
          const added = [];
          for (let i = 1; i <= 5; i++) {
            const bar = document.createElement('div');
            bar.className = 'ft-banner';
            bar.setAttribute('role', 'status');
            bar.dataset.layoutProbe = String(i);
            const msg = document.createElement('span');
            msg.className = 'ft-banner-msg';
            msg.textContent = `${i}. ${long}`;
            bar.appendChild(msg);
            host.appendChild(bar);
            added.push(bar);
          }
          const r = host.getBoundingClientRect();
          const grid = document.getElementById('recordGrid').getBoundingClientRect();
          const sel = document.getElementById('selectionBar').getBoundingClientRect();
          host.scrollTop = host.scrollHeight;
          const last = added[4].getBoundingClientRect();
          const hostAfter = host.getBoundingClientRect();
          const out = {
            h: Math.round(r.height), cap: Math.min(innerHeight * 0.28, 216), scrolls: host.scrollHeight > host.clientHeight + 1,
            gridH: Math.round(grid.height), selBottom: Math.round(sel.bottom), vh: innerHeight,
            lastVisible: last.bottom <= hostAfter.bottom + 1 && last.top >= hostAfter.top - 1,
            hscroll: document.documentElement.scrollWidth > innerWidth + 1,
          };
          for (const bar of added) bar.remove();
          if (made) host.remove();
          return out;
        });
        check(`${label}: R7 U9 five notices — the row stops at min(28vh, 216px) and scrolls`, !n9.missing && n9.h <= n9.cap + 0.5 && n9.scrolls && n9.lastVisible, JSON.stringify(n9));
        check(`${label}: R7 U9 five notices — the grid keeps >= 200px, the selection bar stays on screen, no sideways scroll`, !n9.missing && n9.gridH >= 200 && n9.selBottom <= n9.vh && !n9.hscroll, JSON.stringify(n9));
      }

      // R8 U3: the per-record conflict list is NOT inside the capped,
      // scrolling notices row (a scroller in a scroller): a real conflict
      // (another writer saved the record first) → open the list → Tab to
      // its buttons. Each focused button is fully on screen and not
      // covered, inside ONE scroll container (the list itself), the list
      // is the notices row's sibling, the selection bar stays on screen,
      // nothing scrolls sideways. Also with five long notices stacked.
      if (!wide) {
        const rid = await page.evaluate(() => document.querySelector('#recordGrid .card')?.dataset.id || null);
        const meta = await getJson('/api/records-meta');
        const caseId = meta.manifest && meta.manifest.case_id;
        const rev = Number(((meta.annotations || {}).revisions || {})[rid]) || 0;
        const other = await postJson('/api/import-marks', { marks_json: JSON.stringify({
          schema_version: 2, protocol: 'patch-v2', case_id: caseId, examiner: 'other', client_id: 'w-layout-other', exported_unix: 1,
          base_revisions: { [rid]: rev }, marks: [{ id: rid, status: 'important', marked_unix: 1, note: `layout ${label}`, examiner: 'other', tags: [], range: { in: null, out: null } }], tags: [], deleted_ids: [],
        }) });
        await page.click(`#recordGrid .card[data-id="${rid}"]`);
        await page.evaluate(() => document.activeElement?.blur());
        await page.keyboard.press('1');
        await page.evaluate(() => document.getElementById('saveState')?.click());
        const banner = await page.waitForSelector('#markConflictBanner', { timeout: 20000 }).then(() => true).catch(() => false);
        if (!check(`${label}: R8 U3 a real save conflict shows the conflict banner`, banner && other && other.ok !== false, JSON.stringify({ rid, rev, other: other && (other.applied || other.error) }))) {
          // nothing to measure
        } else {
          await page.click('#markConflictList');
          await page.waitForSelector('#markConflictPanel:not([hidden]) .conflict-item');
          const measure = (expectFocusIn) => page.evaluate((want) => {
            const el = document.activeElement;
            const panel = document.getElementById('markConflictPanel');
            const notices = document.querySelector('#ftViewer > .ft-notices');
            const r = el.getBoundingClientRect();
            const pr = panel.getBoundingClientRect();
            const hit = document.elementFromPoint(r.left + r.width / 2, r.top + r.height / 2);
            const scrollers = [];
            for (let n = el.parentElement; n && n !== document.body; n = n.parentElement) {
              const cs = getComputedStyle(n);
              if (/(auto|scroll)/.test(cs.overflowY) && n.scrollHeight > n.clientHeight + 1) scrollers.push(n.id || n.className);
            }
            const sel = document.getElementById('selectionBar').getBoundingClientRect();
            const grid = document.getElementById('recordGrid').getBoundingClientRect();
            return {
              focus: el.tagName + (el.dataset.side ? ':' + el.dataset.side : '') + (el.id ? '#' + el.id : ''),
              inPanel: panel.contains(el), want,
              onScreen: r.top >= -0.5 && r.bottom <= innerHeight + 0.5 && r.left >= -0.5 && r.right <= innerWidth + 0.5,
              inPanelBox: r.top >= pr.top - 0.5 && r.bottom <= pr.bottom + 0.5,
              covered: !(hit && (hit === el || el.contains(hit))), h: Math.round(r.height),
              scrollers, sibling: panel.parentElement && panel.parentElement.id === 'ftViewer' && !notices.contains(panel) && notices.nextElementSibling === panel,
              selBottom: Math.round(sel.bottom), vh: innerHeight, gridH: Math.round(grid.height), panelH: Math.round(pr.height), noticesH: Math.round(notices.getBoundingClientRect().height),
              hscroll: document.documentElement.scrollWidth > innerWidth + 1,
            };
          }, expectFocusIn);
          const tabs = [];
          for (let i = 0; i < 2; i++) { await page.keyboard.press('Tab'); tabs.push(await measure(true)); }
          const okTab = (m) => m.inPanel && /^BUTTON:(server|mine)/.test(m.focus) && m.onScreen && m.inPanelBox && !m.covered && m.h >= 24 && m.scrollers.length <= 1 && !m.scrollers.some((c) => /ft-notices/.test(c));
          check(`${label}: R8 U3 conflict list opened — Tab reaches both per-record buttons, each on screen, not covered, >= 24px, inside one scroll container`, tabs.every(okTab), JSON.stringify(tabs));
          check(`${label}: R8 U3 the conflict list is the notices row's sibling (not inside the capped row), the selection bar stays on screen, no sideways scroll`, tabs.every((m) => m.sibling && m.selBottom <= m.vh && !m.hscroll), JSON.stringify(tabs[0]));
          // R9 U10: in Tab order the open list comes right after its toggle
          // (the toggle is the last button of the last banner of the row).
          await page.focus('#markConflictList');
          await page.keyboard.press('Tab');
          const afterToggle = await measure(true);
          await page.keyboard.press('Shift+Tab');
          const backTo = await page.evaluate(() => document.activeElement?.id || document.activeElement?.tagName || '');
          check(`${label}: R9 U10 Tab from the list toggle lands on the list's first button (on screen, >= 24px); Shift+Tab goes back to the toggle`, okTab(afterToggle) && /^BUTTON:server/.test(afterToggle.focus) && backTo === 'markConflictList', JSON.stringify({ focus: afterToggle.focus, backTo }));
          // Five long notices stacked while the list is open.
          await page.evaluate(() => {
            const host = document.querySelector('#ftViewer > .ft-notices');
            for (let i = 1; i <= 5; i++) {
              const bar = document.createElement('div');
              bar.className = 'ft-banner'; bar.dataset.layoutProbe = String(i);
              bar.textContent = `${i}. ` + '레이아웃 확인용 긴 알림 — 충돌 목록이 열려 있어도 알림 행과 목록이 서로 가리지 않는지 확인합니다. '.repeat(4);
              host.appendChild(bar);
            }
          });
          await page.focus('#markConflictPanel .conflict-item button[data-side="server"]');
          await page.keyboard.press('Tab');
          const crowded = await measure(true);
          await page.evaluate(() => document.querySelectorAll('[data-layout-probe]').forEach((b) => b.remove()));
          check(`${label}: R8 U3 with five long notices too — the focused list button is on screen and not covered, the selection bar on screen, the grid keeps >= 120px`, okTab(crowded) && crowded.selBottom <= crowded.vh && crowded.gridH >= 120 && !crowded.hscroll, JSON.stringify(crowded));
          if (shots) await page.screenshot({ path: join(shots, `conflicts-${w}-${theme}.png`) });
          await page.click('#markConflictUseServer');
          await page.waitForSelector('#markConflictBanner', { state: 'detached', timeout: 20000 }).catch(() => {});
          await page.waitForFunction(() => FTViewer.saveState() === 'saved', null, { timeout: 20000 }).catch(() => {});
        }
      }

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

        // 9. U12: a viewer modal blocks the shell's Alt+P/J/R; Esc is the
        //    modal's (one layer) and leaves the shell alone.
        await page.locator('#recordGrid .card').first().click();
        await page.evaluate(() => document.activeElement?.blur());
        await page.keyboard.press('Shift+Slash');
        const modalUp = await page.evaluate(() => !document.getElementById('shortcutsModal').hidden);
        await page.keyboard.press('Alt+KeyJ');
        await page.keyboard.press('Alt+KeyP');
        const underModal = await panels();
        check('keys: Alt+J / Alt+P do nothing while a viewer modal is open', modalUp && !underModal.jobs && !underModal.project, JSON.stringify({ modalUp, underModal }));
        await page.evaluate(() => document.activeElement?.blur());
        await page.keyboard.press('Alt+KeyR');
        check('keys: Alt+R does nothing while a viewer modal is open (focus on body)', !(await panels()).finalize);
        await page.keyboard.press('Escape');
        const afterEsc = await page.evaluate(() => !document.getElementById('shortcutsModal').hidden);
        check('keys: Esc closes the viewer modal only', !afterEsc && !Object.values(await panels()).some(Boolean));

        // 10. U4 hosted: FTViewer.reload() (destroy + init in place) keeps
        //     the search, the active card and the selection.
        await page.locator('#query').fill('front');
        await sleep(400);
        await page.locator('#recordGrid .card').nth(1).click();
        await page.evaluate(() => document.activeElement?.blur());
        await page.keyboard.press('x');
        const vBefore = await page.evaluate(() => ({ active: document.querySelector('#recordGrid .card.active')?.dataset.id, count: document.getElementById('resultCount').textContent, sel: [...document.querySelectorAll('#recordGrid .card input[type="checkbox"]:checked')].map((c) => c.closest('.card').dataset.id) }));
        const reloaded = await page.evaluate(() => FTViewer.reload({ reason: 'test' }));
        await until(() => cardCount(page), 20000);
        await sleep(400);
        const vAfter = await page.evaluate(() => ({ active: document.querySelector('#recordGrid .card.active')?.dataset.id, count: document.getElementById('resultCount').textContent, query: document.getElementById('query').value, sel: [...document.querySelectorAll('#recordGrid .card input[type="checkbox"]:checked')].map((c) => c.closest('.card').dataset.id) }));
        check('hosted reload keeps search, active card and selection', reloaded === 'reloaded' && vAfter.query === 'front' && vAfter.count === vBefore.count && vAfter.active === vBefore.active && JSON.stringify(vAfter.sel) === JSON.stringify(vBefore.sel) && vBefore.sel.length === 1, JSON.stringify({ reloaded, vBefore, vAfter }));
        await page.keyboard.press('x').catch(() => {});
        await page.locator('#query').fill('');

        // 10b. F5: Esc in a hosted viewer text field blurs the field only —
        //      the shell's Esc handler leaves its toasts / popovers alone.
        await page.evaluate(() => {
          const el = document.createElement('div');
          el.className = 'h-toast';
          el.textContent = 'f5-shell-toast';
          document.getElementById('hToastBox').appendChild(el);
          // eslint-disable-next-line no-undef
          toasts.push({ el, sticky: true, timer: 0 });
        });
        await page.locator('#query').focus();
        await page.keyboard.press('Escape');
        const f5 = await page.evaluate(() => ({ shellToasts: toasts.length, toastInDom: !!document.querySelector('#hToastBox .h-toast'), focusInQuery: document.activeElement?.id === 'query' }));
        check('Esc in a viewer text field blurs it and does not dismiss a shell toast', f5.shellToasts === 1 && f5.toastInDom && !f5.focusInQuery, JSON.stringify(f5));
        await page.evaluate(() => dismissNewestToast());

        // 11. U8, modelled on the real server: the viewer's SAVE (POST
        //     /api/import-marks) is refused with 401 while GET /api/status
        //     keeps answering 200 (no-token mode: a GET needs no proof; the
        //     usual cause is another instance's cookie). The banner must stay
        //     through good status polls and refused retries, never say
        //     "server down" / "expired", and go only when a save gets through.
        const banner = () => page.evaluate(() => { const b = document.getElementById('serverBanner'); return { shown: !b.hidden, text: b.textContent, title: b.querySelector('b').textContent, btnHidden: document.getElementById('btnServerRetry').hidden }; });
        let statusOk = 0, saves401 = 0;
        const onResp = (r) => { if (/\/api\/status/.test(r.url()) && r.status() === 200) statusOk += 1; };
        page.on('response', onResp);
        await page.route('**/api/import-marks*', (route) => { saves401 += 1; return route.fulfill({ status: 401, contentType: 'application/json', body: '{"ok":false,"error":"unauthorized"}' }); });
        await page.locator('#recordGrid .card').first().click();
        await page.evaluate(() => document.activeElement?.blur());
        await page.keyboard.press('2');
        const saveAuthUp = await until(async () => { const b = await banner(); return b.shown && /저장이 인증 오류로 거부/.test(b.title) ? b : null; }, 20000);
        check('401 save: banner says the save was refused (auth), with a reload action', !!saveAuthUp && /다른 FrameTrace 창/.test(saveAuthUp.text) && /새로 고침/.test(saveAuthUp.text) && !/연결 끊김|만료/.test(saveAuthUp.text), JSON.stringify(saveAuthUp));
        const okAtUp = statusOk, savesAtUp = saves401;
        await until(() => statusOk >= okAtUp + 2 && saves401 >= savesAtUp + 1, 30000);
        const stillUp = await banner();
        check('401 save: banner stays through good /api/status polls and refused retries', stillUp.shown && /저장이 인증 오류/.test(stillUp.title) && statusOk >= okAtUp + 2 && saves401 >= savesAtUp + 1, JSON.stringify({ stillUp: stillUp.title, statusOk: statusOk - okAtUp, retries: saves401 - savesAtUp }));
        await page.unroute('**/api/import-marks*');
        const saveAuthGone = await until(async () => !(await banner()).shown, 70000);
        const restored = (await banner()).title;
        check('401 save: a save that gets through clears the banner and restores its normal face', !!saveAuthGone && /연결 끊김/.test(restored), restored);
        page.off('response', onResp);

        // Token mode, cookie gone: GET /api/status AND GET / answer 401 —
        // [이 창 새로 고침] must not reload into a dead page: guidance instead.
        const deny = (route) => route.fulfill({ status: 401, contentType: 'application/json', body: '{"ok":false,"error":"unauthorized"}' });
        await page.route('**/api/status*', deny);
        const readUp = await until(async () => { const b = await banner(); return b.shown && /인증 오류로 거부/.test(b.title) ? b : null; }, 15000);
        check('401 status: banner shows the auth face (not "server down")', !!readUp && !/연결 끊김/.test(readUp.title), JSON.stringify(readUp));
        const rootUrl = new URL('/', page.url()).href;
        await page.route(rootUrl, deny);
        let authLoads = 0;
        const onLoad2 = () => { authLoads += 1; };
        page.on('load', onLoad2);
        await page.click('#btnServerRetry');
        const gone = await until(async () => { const b = await banner(); return /FrameTrace 앱 창/.test(b.text) && b.btnHidden ? b : null; }, 8000);
        check('401 + no cookie: reload button gives "reopen from the app window" guidance, no dead reload', !!gone && authLoads === 0, JSON.stringify({ gone, authLoads }));
        await page.unroute(rootUrl);
        page.off('load', onLoad2);
        // Cookie still there (the usual token-mode case): the same button
        // loads "/" (the stripped ?token= is not needed — the cookie is).
        const [nav] = await Promise.all([
          page.waitForEvent('load', { timeout: 20000 }).catch(() => null),
          (async () => {
            // A fresh auth face (the button is shown again when the face
            // re-renders after a good poll); wait for the button, then click.
            await page.unroute('**/api/status*');
            await until(async () => !(await banner()).shown, 15000);
            await page.route('**/api/status*', deny);
            await until(async () => { const b = await banner(); return b.shown && !b.btnHidden ? b : null; }, 15000);
            await page.click('#btnServerRetry');
          })(),
        ]);
        check('401 + cookie present: the reload button loads "/" again', !!nav && new URL(page.url()).pathname === '/', page.url());
        await page.unroute('**/api/status*');
        await until(() => cardCount(page), 30000);
        await page.waitForFunction(() => window.FTViewer && FTViewer.saveState() === 'saved', null, { timeout: 60000 }).catch(() => {});

        // 12. R4 A12: Ctrl+K never opens the viewer's command palette over a
        //     shell modal (here the shell's in-page dialog).
        await page.evaluate(() => { window.__dlg = askDialog({ title: 'A12', html: '<p>a12</p>', okLabel: '확인', cancelLabel: '취소', focus: 'dlgCancel' }); });
        await page.waitForSelector('#dlgBackdrop:not([hidden])');
        await page.keyboard.press('Control+k');
        const a12 = await page.evaluate(() => { const p = document.getElementById('cmdPalette'); return !p || p.hidden; });
        check('A12: Ctrl+K over a shell dialog does not open the command palette', a12);
        await page.click('#dlgCancel');
        await page.waitForSelector('#dlgBackdrop', { state: 'hidden' });

        // 13. R4 A15: before the window closes the shell waits for the
        //     viewer's save (hBeforeClose — the WebView2 close path, C15).
        check('A15: nothing unsaved → hBeforeClose ok', (await page.evaluate(() => hBeforeClose(5000))).ok === true);
        let release;
        const held = new Promise((r) => { release = r; });
        let replied = 0;
        await page.route('**/api/import-marks*', async (route) => { await held; await route.continue(); replied = Date.now(); });
        await page.locator('#recordGrid .card').nth(2).click();
        await page.evaluate(() => document.activeElement?.blur());
        await page.keyboard.press('3');
        // R5 U14: the check used to compare Date.now() - t0 with the 1.5 s it
        // had just slept itself (always true). What matters: hBeforeClose
        // resolved only AFTER the held save's reply was let through.
        let closedAt = 0;
        const closing = page.evaluate(() => hBeforeClose(20000)).then((r) => { closedAt = Date.now(); return r; });
        await sleep(1500);
        const earlyClose = closedAt;
        release();
        const r13 = await closing;
        check('A15: hBeforeClose waits for the save in flight, then ok (nothing pending)', r13.ok === true && r13.pending === 0 && replied > 0 && earlyClose === 0 && closedAt >= replied, JSON.stringify({ r13, earlyClose, replied, closedAt }));
        await page.unroute('**/api/import-marks*');
        await page.route('**/api/import-marks*', (route) => route.fulfill({ status: 503, contentType: 'application/json', body: '{"ok":false,"error":"busy"}' }));
        await page.locator('#recordGrid .card').nth(2).click();
        await page.evaluate(() => document.activeElement?.blur());
        await page.keyboard.press('1');
        const r13b = await page.evaluate(() => hBeforeClose(4000));
        check('A15: a failing save → ok:false, "저장 안 된 편집 N건" for the confirmation', r13b.ok === false && r13b.pending >= 1 && /저장 안 된 편집 \d+건/.test(r13b.message), JSON.stringify(r13b));
        await page.unroute('**/api/import-marks*');
        await page.evaluate(() => FTViewer.save({ patient: true })); // R9 U9: an explicit save, not the 4 s close budget
        await page.waitForFunction(() => FTViewer.saveState() === 'saved', null, { timeout: 60000 }).catch(() => {});

        // 14. R4 B2 contract: the one-time launch code (?launch=) leaves the
        //     address bar like ?token= — the shell and the served viewer.
        // R5 U14: the old check waited for "no launch" and then asserted "no
        // launch" (always true) while the 303 silently dropped every other
        // query parameter. R6 S3 contract (S ↔ U): a launch answers 200
        // text/html — a same-origin step page that sets the cookie and goes
        // on with location.replace(target + location.hash) (a 303 lost the
        // SameSite=Strict cookie on Linux/macOS); NO 303 / Location. The
        // target drops ONLY launch. Asked with this browser's cookies
        // (page.request), not followed; then opened for real.
        const step = await page.request.get(`${base}/?launch=a4test&keep=1`, { maxRedirects: 0 }).catch(() => null);
        const stepBody = step ? await step.text().catch(() => '') : '';
        const stepInfo = step ? { status: step.status(), type: step.headers()['content-type'] || '', location: step.headers()['location'] || null } : { status: 0 };
        check('B2/S3: launch answers 200 text/html, a same-origin step page (location.replace(target + location.hash)), no 303 / Location',
          stepInfo.status === 200 && /text\/html/.test(stepInfo.type) && !stepInfo.location && /location\.replace\([^)]*\+ location\.hash\)/.test(stepBody), JSON.stringify(stepInfo));
        check('B2/U14: the step page\'s target keeps the other query (?keep=1) and drops only launch',
          /location\.replace\([^)]*keep=1/.test(stepBody) && !/location\.replace\([^)]*launch/.test(stepBody), (stepBody.match(/location\.replace\([^)]*\)/) || [''])[0]);
        await page.goto(`${base}/?launch=a4test&keep=1#frag`);
        await until(() => page.evaluate(() => !location.search.includes('launch') && !!document.getElementById('main')), 15000);
        const shellUrl = await page.evaluate(() => ({ search: location.search, hash: location.hash }));
        check('B2/U14: after the step page the shell has ?keep=1 and the hash, no ?launch=', shellUrl.search === '?keep=1' && shellUrl.hash === '#frag', JSON.stringify(shellUrl));
        await page.goto(`${base}/viewer/?token=${token}&launch=a4test&keep=2`);
        await until(() => page.evaluate(() => !/launch|token/.test(location.search)), 15000);
        const viewerUrl = await page.evaluate(() => location.search);
        check('B2: the served standalone viewer drops ?token= and ?launch=', viewerUrl === '?keep=2', viewerUrl);
      }
      await context.close();
    }
  }
  // R17 U1/U2: a withheld anomaly head (marker-unusable) and one
  // authenticated + one unauthenticated anomaly row, injected into
  // records-meta at 1366x768: the pill reads "이상 1 · 확인 불가" and stays
  // on screen / uncovered with every top-bar control, the banner says
  // "변조 가능성", the unauthenticated record has its own badge.
  {
    const context = await browser.newContext({ viewport: { width: 1366, height: 768 } });
    const page = await context.newPage();
    page.on('pageerror', (e) => pageErrors.push(`R17: ${e.message}`));
    await page.route('**/api/records-meta*', async (route) => {
      const res = await route.fetch();
      const body = await res.json();
      // records-meta carries no videos (they come paged from /api/records).
      const first = await getJson('/api/records?offset=0&limit=3');
      const ids = ((first && first.videos) || []).map((v) => v.id).filter(Boolean);
      body.logChains = Object.assign({}, body.logChains, { anomaly: { head: { status: 'unknown', reason_code: 'marker-unusable', withhold_reason: '표식 HMAC 없음', detail: 'marker' } } });
      // R18 U1/U2 (P30): the unauthenticated and the other-PC-key rows count too.
      body.anomalyLog = [{ event: 'anomaly-scan', kind: 'gop-gap', selector: ids[0], label: 'candidate-finding', detail: 'gap', trust: 'auth' },
        { event: 'anomaly-scan', kind: 'ts-gap', selector: ids[1], label: 'candidate-finding', detail: 'gap', trust: 'unauth' },
        { event: 'anomaly-scan', kind: 'ts-gap', selector: ids[2], label: 'candidate-finding', detail: 'gap', trust: 'pc:K2' }];
      return route.fulfill({ response: res, body: JSON.stringify(body), headers: Object.assign({}, res.headers(), { 'content-type': 'application/json' }) });
    });
    await page.goto(`${base}/?token=${token}`);
    await openProject(page);
    const r17 = await page.evaluate(() => {
      const pill = document.getElementById('pillAnomaly');
      const bar = document.querySelector('.ft-viewer .topbar');
      const ctl = bar ? [...bar.querySelectorAll('button')].filter((b) => b.getClientRects().length && getComputedStyle(b).visibility !== 'hidden') : [];
      const bad = [];
      for (const b of ctl) {
        const r = b.getBoundingClientRect();
        const hit = document.elementFromPoint(r.left + r.width / 2, r.top + r.height / 2);
        if (r.left < 0 || r.right > innerWidth + 0.5 || !hit || !(hit === b || b.contains(hit)) || b.scrollWidth > b.clientWidth + 1) bad.push(b.id || b.textContent.trim().slice(0, 20));
      }
      return { pill: pill ? pill.textContent.replace(/\s+/g, ' ').trim() : null, pillTip: pill ? pill.title : '', bad, n: ctl.length,
        hscroll: document.documentElement.scrollWidth > innerWidth + 1,
        banner: (document.getElementById('anomalyHeadBanner') || {}).textContent || '',
        unauth: document.querySelectorAll('#recordGrid .badge.anomaly.anomaly-unauth').length, pc: document.querySelectorAll('#recordGrid .badge.anomaly.anomaly-pc').length,
        auth: document.querySelectorAll('#recordGrid .badge.anomaly.anomaly-unverified').length,
        unauthChip: (() => { const c = document.querySelector('#presetChips [data-chip="anomaly:unauth"]'); return c && c.getClientRects().length ? c.textContent.trim() : ''; })(),
        badgeBad: [...document.querySelectorAll('#recordGrid .badge.anomaly')].filter((b) => b.getClientRects().length && b.scrollWidth > b.clientWidth + 1).length };
    });
    check('R18 U1 (P30): the anomaly pill reads "이상 3 (인증 안 됨 1 · 다른 PC 키 1) · 확인 불가"', r17.pill === '이상 3 (인증 안 됨 1 · 다른 PC 키 1) · 확인 불가' && /인증 안 됨 1건/.test(r17.pillTip), JSON.stringify(r17));
    check('R17/R18 U2: 1366x768 — every viewer top-bar control on screen, uncovered, unclipped; no sideways scroll', r17.n > 0 && !r17.bad.length && !r17.hscroll, JSON.stringify(r17.bad));
    check('R17 U1: marker-unusable banner "변조 가능성", no rescan hint', /변조 가능성/.test(r17.banner) && !/다시 스캔/.test(r17.banner), r17.banner);
    check('R18 U1/U2: badges — 이상 후보 · 확인 불가 3 (인증 안 됨 1, 다른 PC 키 1), none clipped', r17.auth === 3 && r17.unauth === 1 && r17.pc === 1 && !r17.badgeBad, JSON.stringify(r17));
    check('R18 U1: the "인증 안 된 후보 1" chip is on the rail without "더보기"', r17.unauthChip === '인증 안 된 후보 1', JSON.stringify(r17.unauthChip));
    await context.close();
  }
  // R18 U1 (track I2): records-meta `logChains.anomaly.withheld: {reason}`
  // with an intact head — the banner gives the reason, the pill "확인 불가".
  {
    const context = await browser.newContext({ viewport: { width: 1366, height: 768 } });
    const page = await context.newPage();
    page.on('pageerror', (e) => pageErrors.push(`R18: ${e.message}`));
    await page.route('**/api/records-meta*', async (route) => {
      const res = await route.fetch();
      const body = await res.json();
      const first = await getJson('/api/records?offset=0&limit=1');
      const ids = ((first && first.videos) || []).map((v) => v.id).filter(Boolean);
      body.logChains = Object.assign({}, body.logChains, { anomaly: { head: { status: 'intact', reason_code: null, withhold_reason: null }, withheld: { reason_code: 'unusable-after', codes: ['unusable-after'], reason: '가장 최근 완료 실행의 완료 표식 뒤에 이 PC에서 인증할 수 없는 항목이 있어 그 결과를 알 수 없습니다' } } });
      body.anomalyLog = [{ event: 'anomaly-scan', kind: 'gop-gap', selector: ids[0], label: 'candidate-finding', detail: 'gap', trust: 'unauth' }];
      return route.fulfill({ response: res, body: JSON.stringify(body), headers: Object.assign({}, res.headers(), { 'content-type': 'application/json' }) });
    });
    await page.goto(`${base}/?token=${token}`);
    await openProject(page);
    const r18 = await page.evaluate(() => ({
      pill: (document.getElementById('pillAnomaly') || {}).textContent?.replace(/\s+/g, ' ').trim() || null,
      banner: (document.getElementById('anomalyHeadBanner') || {}).textContent || '',
      bannerTip: ((document.getElementById('anomalyHeadBanner') || { querySelector: () => null }).querySelector('[title]') || {}).title || '',
      role: (document.getElementById('anomalyHeadBanner') || { getAttribute: () => '' }).getAttribute('role'),
      hscroll: document.documentElement.scrollWidth > innerWidth + 1 }));
    check('R18 U1: withheld → pill "이상 1 (인증 안 됨 1) · 확인 불가", banner shows the reason', r18.pill === '이상 1 (인증 안 됨 1) · 확인 불가' && r18.banner.trim() === '이상 징후 확인 불가 — 완료 표식 뒤 인증 안 된 줄' && r18.bannerTip.includes('완료 표식 뒤에 이 PC에서 인증할 수 없는 항목') && r18.role === 'status' && !r18.hscroll, JSON.stringify(r18));
    await context.close();
  }
  check('no page errors', pageErrors.length === 0, pageErrors.slice(0, 5).join(' | '));
} catch (err) {
  console.error(err);
  check('harness ran to completion', false, String(err && err.message || err));
}
const failed = results.filter((r) => !r.ok);
console.log(`\nverify-layout: ${results.length - failed.length}/${results.length} passed`);
process.exit(failed.length ? 1 : 0);
