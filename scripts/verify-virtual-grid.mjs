// PERF-02 regression check: the evidence viewer grid is virtualized, so the
// DOM stays small with 50k records, j/k reaches the last record, the active
// card is visible, grouping/collapse still work, and ?grid=paged falls back
// to the paged renderer. Builds the standalone viewer like build.rs and
// drives it in headless Chromium via Playwright.
//
//   node scripts/verify-virtual-grid.mjs
import { readFileSync, readdirSync } from 'node:fs';
import { join, dirname } from 'node:path';
import { fileURLToPath } from 'node:url';
import { createServer } from 'node:http';
import { createRequire } from 'node:module';
import { viewerPageHtml } from './lib/app-bundle.mjs';

const root = join(dirname(fileURLToPath(import.meta.url)), '..');
const require = createRequire(import.meta.url);
const { chromium } = require('playwright');

const ROWS = 50_000;
const MAX_CARDS = 400;

function buildViewer() {
  const dir = join(root, 'assets/viewer');
  let js = '';
  for (const part of readdirSync(dir).filter((f) => f.endsWith('.js')).sort()) {
    js += readFileSync(join(dir, part), 'utf8');
    if (!js.endsWith('\n')) js += '\n';
  }
  const videos = Array.from({ length: ROWS }, (_, i) => {
    const n = String(i).padStart(6, '0');
    return {
      id: `vid_${n}`, relative_path: `DCIM/CAM${i % 7}/clip_${n}.mp4`, source_path: `/case/DCIM/CAM${i % 7}/clip_${n}.mp4`,
      ffprobe_ok: true, size_bytes: 1000 + i, extension: 'mp4',
    };
  });
  const data = `window.__FRAMETRACE_DATA__ = ${JSON.stringify({
    manifest: { case_id: 'FT-VGRID', title: 'vgrid' }, caseDir: '/case', scan: { videos },
    carveLog: [], filesystemLog: [], validationLog: [], anomalyLog: [], flsEntries: [],
    thumbs: {}, annotations: {}, deepfake: {}, telemetry: {}, proxies: [],
  })};`;
  // Standalone page exactly as build.rs assembles it (shared fragment included).
  return viewerPageHtml({ data, js: js, root });
}

const results = [];
const check = (name, ok, detail = '') => {
  results.push(ok);
  console.log(`${ok ? 'PASS' : 'FAIL'}  ${name}${detail ? '  — ' + detail : ''}`);
};

const html = buildViewer();
const server = createServer((req, res) => {
  res.setHeader('Content-Type', 'text/html; charset=utf-8');
  res.end(html);
});
await new Promise((r) => server.listen(0, '127.0.0.1', r));
const base = `http://127.0.0.1:${server.address().port}/`;
const browser = await chromium.launch();
const setGroup = (page, value) => page.evaluate((v) => { const el = document.getElementById('groupBy'); el.value = v; el.dispatchEvent(new Event('change', { bubbles: true })); }, value);
const cardCount = (page) => page.evaluate(() => document.querySelectorAll('#recordGrid .card').length);
const activeVisible = (page) => page.evaluate(() => {
  const g = document.getElementById('recordGrid').getBoundingClientRect();
  const c = document.querySelector('#recordGrid .card.active');
  if (!c) return { found: false };
  const r = c.getBoundingClientRect();
  return { found: true, visible: r.top >= g.top - 1 && r.bottom <= g.bottom + 1, id: c.dataset.id };
});
try {
  const page = await browser.newPage({ viewport: { width: 1500, height: 900 } });
  const errors = [];
  page.on('pageerror', (e) => errors.push(String(e)));
  await page.goto(base);
  await page.waitForSelector('#recordGrid .card', { timeout: 60_000 });

  const total = await page.evaluate(() => document.getElementById('resultCount').textContent);
  check('all records are in the result set', total === String(ROWS), total);
  const n0 = await cardCount(page);
  check(`initial DOM card count < ${MAX_CARDS}`, n0 > 0 && n0 < MAX_CARDS, String(n0));
  const spacerH = await page.evaluate(() => document.querySelector('#recordGrid .vspacer')?.getBoundingClientRect().height || 0);
  check('spacer height covers the whole list', spacerH > 1_000_000, String(spacerH));

  // Scroll to the middle: DOM stays bounded and shows later records.
  await page.evaluate(() => { const g = document.getElementById('recordGrid'); g.scrollTop = g.scrollHeight / 2; });
  await page.waitForTimeout(150);
  const mid = await page.evaluate(() => ({ n: document.querySelectorAll('#recordGrid .card').length, first: document.querySelector('#recordGrid .card')?.dataset.id }));
  check('mid-list scroll keeps DOM bounded and swaps rows', mid.n < MAX_CARDS && mid.first > 'vid_010000', JSON.stringify(mid));
  await page.evaluate(() => { document.getElementById('recordGrid').scrollTop = 0; });
  await page.waitForTimeout(150);

  // Keyboard: j to step; End-of-list via repeated j is slow, so jump with
  // many presses in-page through the real key handler.
  await page.locator('#recordGrid .card').first().click();
  await page.evaluate(() => document.activeElement?.blur());
  for (let i = 0; i < 40; i++) await page.keyboard.press('j');
  let vis = await activeVisible(page);
  check('active card stays visible after 40 x j', vis.found && vis.visible, JSON.stringify(vis));
  for (let i = 0; i < 40; i++) await page.keyboard.press('k');
  vis = await activeVisible(page);
  check('k returns to the first record, visible', vis.found && vis.visible && vis.id === 'vid_000000', JSON.stringify(vis));

  // Reach the last record: dispatch j keydowns straight at the document so
  // 50k presses do not go through Playwright's per-key round trip.
  const last = await page.evaluate(async () => {
    for (let i = 0; i < 50_000; i++) {
      document.dispatchEvent(new KeyboardEvent('keydown', { key: 'j', bubbles: true }));
    }
    await new Promise((r) => setTimeout(r, 200));
    const active = document.querySelector('#recordGrid .card.active');
    return { id: active?.dataset.id, n: document.querySelectorAll('#recordGrid .card').length };
  });
  check('j reaches the last record', last.id === `vid_${String(ROWS - 1).padStart(6, '0')}`, JSON.stringify(last));
  check('DOM stays bounded at the end', last.n < MAX_CARDS, String(last.n));
  vis = await activeVisible(page);
  check('last active card is visible', vis.found && vis.visible, JSON.stringify(vis));

  // Arrow-key path through the grid's own handler keeps focus on the card.
  await page.evaluate(() => document.querySelector('#recordGrid .card.active').focus());
  await page.keyboard.press('ArrowLeft');
  const focused = await page.evaluate(() => ({ focusId: document.activeElement?.dataset?.id, activeId: document.querySelector('#recordGrid .card.active')?.dataset.id }));
  check('ArrowLeft moves focus to the previous painted card', focused.focusId && focused.focusId === focused.activeId && focused.focusId === `vid_${String(ROWS - 2).padStart(6, '0')}`, JSON.stringify(focused));

  // Click on a card selects it (delegation works on virtual rows).
  const clickedTarget = await page.locator('#recordGrid .card').nth(2).getAttribute('data-id');
  await page.locator(`#recordGrid .card[data-id="${clickedTarget}"]`).click();
  const clicked = await page.evaluate(() => document.querySelector('#recordGrid .card.active')?.dataset.id);
  check('click activates a virtual card', clicked === clickedTarget, `${clicked} vs ${clickedTarget}`);

  // R5 U17: per-interaction latency budget on 50k records — a card click
  // and a mark (key 1), each timed from the event to the next painted frame
  // (handler + render + layout). Median < 200 ms; any O(n)-per-click work
  // (a findIndex over every record, a full recount, a storage scan) shows.
  const lat = await page.evaluate(async () => {
    const frame = () => new Promise((r) => requestAnimationFrame(() => setTimeout(r, 0)));
    const out = { click: [], mark: [] };
    for (let i = 0; i < 15; i++) {
      const cards = [...document.querySelectorAll('#recordGrid .card')];
      const card = cards[(i * 3 + 1) % cards.length];
      let t0 = performance.now();
      card.dispatchEvent(new MouseEvent('click', { bubbles: true }));
      await frame();
      out.click.push(performance.now() - t0);
      document.activeElement?.blur();
      t0 = performance.now();
      document.dispatchEvent(new KeyboardEvent('keydown', { key: '1', bubbles: true }));
      await frame();
      out.mark.push(performance.now() - t0);
    }
    const med = (a) => [...a].sort((x, y) => x - y)[Math.floor(a.length / 2)];
    return { click: med(out.click), mark: med(out.mark), maxClick: Math.max(...out.click), maxMark: Math.max(...out.mark) };
  });
  check('U17: card click median < 200 ms on 50k records', lat.click < 200, JSON.stringify(lat));
  check('U17: mark (key 1) median < 200 ms on 50k records', lat.mark < 200, JSON.stringify(lat));
  // R6 U8: a time budget passes on a fast machine even with O(n) work per
  // click, so the interactions are COUNTED as well: every whole-list pass
  // through an Array method (some/find/findIndex/filter/reduce/forEach/map/
  // slice/indexOf/includes/every/sort/…) over an array of ≥ 10 000 entries
  // is a list scan. A click and a mark must cause none (the memoised list,
  // the id → index map, the sparse mark map), never re-run the filter, and
  // must actually DO what they are for: the clicked card is the active one,
  // the marked record carries its mark chip.
  const counted = await page.evaluate(async () => {
    const frame = () => new Promise((r) => requestAnimationFrame(() => setTimeout(r, 0)));
    const BIG = 10_000;
    const names = ['some', 'find', 'findIndex', 'findLast', 'findLastIndex', 'filter', 'reduce', 'reduceRight', 'forEach', 'map', 'flatMap', 'slice', 'indexOf', 'lastIndexOf', 'includes', 'every', 'sort', 'concat', 'join'];
    const orig = {};
    const scans = { n: 0, where: {} };
    for (const name of names) {
      orig[name] = Array.prototype[name];
      Array.prototype[name] = function (...args) {
        if (this && this.length >= BIG && scans.on) {
          scans.n += 1;
          const at = (new Error().stack || '').split('\n')[2] || '?';
          const key = name + ' @ ' + at.trim().replace(/\(?https?:\/\/[^)]*\/:?/, '').slice(0, 80);
          scans.where[key] = (scans.where[key] || 0) + 1;
        }
        return orig[name].apply(this, args);
      };
    }
    const out = { click: 0, mark: 0, clickWhere: {}, markWhere: {}, clickMiss: [], markMiss: [], filter: { click: 0, mark: 0 } };
    try {
      for (let i = 0; i < 12; i++) {
        const cards = [...document.querySelectorAll('#recordGrid .card')];
        const card = cards[(i * 5 + 2) % cards.length];
        const id = card.dataset.id;
        let f0 = FTViewer._debugStats().filterComputes;
        scans.n = 0; scans.where = {}; scans.on = true;
        card.dispatchEvent(new MouseEvent('click', { bubbles: true }));
        await frame();
        scans.on = false;
        out.click += scans.n; Object.assign(out.clickWhere, scans.where);
        out.filter.click += FTViewer._debugStats().filterComputes - f0;
        if (FTViewer._debugStats().activeId !== id || document.querySelector('#recordGrid .card.active')?.dataset.id !== id) out.clickMiss.push(id);
        document.activeElement?.blur();
        f0 = FTViewer._debugStats().filterComputes;
        const marked0 = FTViewer.summary().marked;
        scans.n = 0; scans.where = {}; scans.on = true;
        document.dispatchEvent(new KeyboardEvent('keydown', { key: '2', bubbles: true }));
        await frame();
        scans.on = false;
        out.mark += scans.n; Object.assign(out.markWhere, scans.where);
        out.filter.mark += FTViewer._debugStats().filterComputes - f0;
        // Key 2 = 중요 (important): the chip of THAT status, whatever the card held before.
        const chip = document.querySelector(`#recordGrid .card[data-id="${CSS.escape(id)}"] .mark-chip.important`);
        if (!chip || FTViewer.summary().marked < marked0) out.markMiss.push(id);
      }
    } finally {
      for (const name of names) Array.prototype[name] = orig[name];
    }
    return out;
  });
  check('U8: a card click is applied (that card is the active one, 12/12)', counted.clickMiss.length === 0, JSON.stringify(counted.clickMiss));
  check('U8: a mark is applied (the marked card shows its mark chip, 12/12)', counted.markMiss.length === 0, JSON.stringify(counted.markMiss));
  check('U8: 12 clicks on 50k records: 0 whole-list scans, 0 filter runs', counted.click === 0 && counted.filter.click === 0, JSON.stringify({ scans: counted.click, filter: counted.filter.click, where: counted.clickWhere }));
  check('U8: 12 marks on 50k records: 0 whole-list scans, 0 filter runs', counted.mark === 0 && counted.filter.mark === 0, JSON.stringify({ scans: counted.mark, filter: counted.filter.mark, where: counted.markWhere }));
  // Grouping: headers render, collapse works, DOM bounded.
  await setGroup(page, 'prefix');
  await page.waitForTimeout(300);
  await page.evaluate(() => { document.getElementById('recordGrid').scrollTop = 0; });
  await page.waitForTimeout(150);
  const grouped = await page.evaluate(() => ({ headers: document.querySelectorAll('#recordGrid .group-header').length, n: document.querySelectorAll('#recordGrid .card').length }));
  check('grouped mode paints headers with bounded DOM', grouped.headers >= 1 && grouped.n < MAX_CARDS, JSON.stringify(grouped));
  const before = await page.evaluate(() => document.getElementById('recordGrid').scrollHeight);
  await page.evaluate(() => { document.getElementById('recordGrid').scrollTop = 0; });
  await page.waitForTimeout(100);
  await page.locator('#recordGrid .group-header').first().click();
  await page.waitForTimeout(200);
  const after = await page.evaluate(() => document.getElementById('recordGrid').scrollHeight);
  check('collapsing a group shrinks the virtual height', after < before, `${before} -> ${after}`);
  await setGroup(page, 'none');
  await page.waitForTimeout(200);

  // Resize re-layouts the column count.
  const colsBefore = await page.evaluate(() => document.querySelector('#recordGrid .vrow')?.children.length);
  await page.setViewportSize({ width: 800, height: 900 });
  await page.waitForTimeout(400);
  const colsAfter = await page.evaluate(() => document.querySelector('#recordGrid .vrow')?.children.length);
  check('resize recomputes columns', colsAfter > 0 && colsAfter < colsBefore, `${colsBefore} -> ${colsAfter}`);

  // U14: grid > rowgroup (the spacer) > row > gridcell, with aria-rowcount
  // on the grid and aria-rowindex on every painted row.
  const aria = await page.evaluate(() => {
    const g = document.getElementById('recordGrid');
    const spacer = g.querySelector(':scope > .vspacer');
    const rows = [...(spacer ? spacer.children : [])];
    return {
      gridChildrenOk: [...g.children].every((c) => ['row', 'rowgroup'].includes(c.getAttribute('role'))),
      spacerRole: spacer && spacer.getAttribute('role'),
      rowsOk: rows.length > 0 && rows.every((r) => r.getAttribute('role') === 'row' && Number(r.getAttribute('aria-rowindex')) >= 1),
      cellsOk: rows.every((r) => [...r.children].every((c) => c.getAttribute('role') === 'gridcell')),
      rowcount: Number(g.getAttribute('aria-rowcount')),
      maxIndex: Math.max(...rows.map((r) => Number(r.getAttribute('aria-rowindex')))),
    };
  });
  check('virtual grid ARIA chain grid > rowgroup > row > gridcell', aria.gridChildrenOk && aria.spacerRole === 'rowgroup' && aria.rowsOk && aria.cellsOk, JSON.stringify(aria));
  check('aria-rowcount covers every painted aria-rowindex', aria.rowcount > 0 && aria.maxIndex <= aria.rowcount, JSON.stringify(aria));
  {
    let axeSource = '';
    try { axeSource = readFileSync(require.resolve('axe-core/axe.min.js'), 'utf8'); } catch { /* optional */ }
    if (axeSource) {
      await page.addScriptTag({ content: axeSource });
      const v = await page.evaluate(async () => {
        const r = await window.axe.run(document.getElementById('recordGrid'), { runOnly: { type: 'rule', values: ['aria-required-children', 'aria-required-parent'] } });
        return r.violations.map((x) => `${x.id}: ${x.nodes.length}`);
      });
      check('axe aria-required-children / -parent on the virtual grid', v.length === 0, v.join(', '));
    } else {
      console.log('SKIP  axe-core not resolvable (aria-required-children)');
    }
  }

  // U7: under a mark filter j/k reuse the filtered list (memo keyed on the
  // annotation generation) instead of filtering + sorting every record per
  // key. Counted, not timed: FTViewer._debugStats().filterComputes is the
  // number of full filter + sort runs (memo misses) — j/k must add none.
  const stats = () => page.evaluate(() => FTViewer._debugStats());
  const pressJK = (n) => page.evaluate((count) => {
    document.activeElement?.blur();
    for (let i = 0; i < count; i++) document.dispatchEvent(new KeyboardEvent('keydown', { key: i % 2 ? 'k' : 'j', bubbles: true }));
  }, n);
  await page.evaluate(() => document.querySelector('[data-chip="mark:none"]')?.click());
  await page.waitForTimeout(300);
  const chipOn = await page.evaluate(() => document.querySelector('[data-chip="mark:none"]')?.getAttribute('aria-pressed'));
  check('mark filter active for the counting run', chipOn === 'true', String(chipOn));
  const s0 = await stats();
  await pressJK(300);
  const s1 = await stats();
  check('stats hook present and j/k moved the active card', !!s0 && Number.isFinite(s0.filterComputes) && s1.activeId !== null, JSON.stringify({ s0, s1 }));
  check('300 j/k under a mark filter run the full filter 0 times (memoised)', s1.filterComputes === s0.filterComputes, `${s0.filterComputes} -> ${s1.filterComputes}`);
  // A mark still updates the filtered list (the memo key moves with it) —
  // and that is exactly one recompute, which proves the counter counts.
  const beforeMark = await page.evaluate(() => document.getElementById('resultCount').textContent);
  await page.evaluate(() => { document.activeElement?.blur(); document.dispatchEvent(new KeyboardEvent('keydown', { key: '1', bubbles: true })); });
  await page.waitForTimeout(200);
  const afterMark = await page.evaluate(() => document.getElementById('resultCount').textContent);
  const s2 = await stats();
  check('marking under mark:none drops the record from the filtered list', Number(afterMark) === Number(beforeMark) - 1, `${beforeMark} -> ${afterMark}`);
  check('a mark recomputes the filter (counter is live)', s2.filterComputes > s1.filterComputes, `${s1.filterComputes} -> ${s2.filterComputes}`);
  await page.evaluate(() => document.querySelector('[data-chip=""]')?.click());
  await page.waitForTimeout(200);

  // U4: a reload keeps the view — scroll, active card, filter — (standalone
  // FTViewer.reload() = page reload with the view stashed for the next boot).
  // Search first, then pick a card inside the result (an active card the
  // search hides would be vacuous), then scroll away from the top.
  await page.locator('#query').fill('cam3');
  await page.waitForTimeout(400);
  await page.evaluate(() => { const g = document.getElementById('recordGrid'); g.scrollTop = Math.floor(g.scrollHeight / 3); });
  await page.waitForTimeout(300);
  await page.locator('#recordGrid .card').nth(3).click();
  await page.evaluate(() => document.activeElement?.blur());
  await page.waitForTimeout(200);
  const viewBefore = await page.evaluate(() => ({
    active: FTViewer._debugStats().activeId,
    activeCard: document.querySelector('#recordGrid .card.active')?.dataset.id,
    count: document.getElementById('resultCount').textContent,
  }));
  const scrollBefore = await page.evaluate(() => document.getElementById('recordGrid').scrollTop);
  check('U4 precondition: grid scrolled (> 0) and a searched card is active', scrollBefore > 0 && !!viewBefore.active && viewBefore.active === viewBefore.activeCard && Number(String(viewBefore.active).slice(4)) % 7 === 3, JSON.stringify({ scrollBefore, viewBefore }));
  await Promise.all([
    page.waitForEvent('load', { timeout: 60_000 }),
    page.evaluate(() => FTViewer.reload({ keepUnsaved: false, reason: 'test' })),
  ]);
  await page.waitForSelector('#recordGrid .card', { timeout: 60_000 });
  await page.waitForTimeout(400);
  const viewAfter = await page.evaluate(() => ({
    scroll: document.getElementById('recordGrid').scrollTop,
    active: FTViewer._debugStats().activeId,
    activeCard: document.querySelector('#recordGrid .card.active')?.dataset.id,
    count: document.getElementById('resultCount').textContent,
    query: document.getElementById('query').value,
  }));
  check('reload keeps the search text and filtered result', viewAfter.query === 'cam3' && viewAfter.count === viewBefore.count, JSON.stringify({ viewBefore, viewAfter }));
  check('reload restores the active card (state and painted card)', viewAfter.active === viewBefore.active && viewAfter.activeCard === viewBefore.active, JSON.stringify({ viewBefore, viewAfter }));
  check('reload stash is consumed on boot', await page.evaluate(() => { try { return !Object.keys(sessionStorage).some((k) => k.endsWith('.reloadView')); } catch { return false; } }));
  check('reload keeps the grid scroll position', Math.abs(viewAfter.scroll - scrollBefore) < 2, `${scrollBefore} -> ${viewAfter.scroll}`);
  await page.locator('#query').fill('');
  await page.waitForTimeout(300);
  check('no page errors', !errors.length, errors.join(' | '));
  await page.close();

  // Fallback: ?grid=paged keeps the old paged renderer.
  const paged = await browser.newPage({ viewport: { width: 1500, height: 900 } });
  await paged.goto(base + '?grid=paged');
  await paged.waitForSelector('#recordGrid .card', { timeout: 60_000 });
  const pagedInfo = await paged.evaluate(() => ({ n: document.querySelectorAll('#recordGrid .card').length, virtual: !!document.querySelector('#recordGrid .vspacer'), size: Number(document.getElementById('pageSize').value) }));
  check('?grid=paged renders one page without a spacer', !pagedInfo.virtual && pagedInfo.n === pagedInfo.size, JSON.stringify(pagedInfo));
  await paged.close();
} finally {
  await browser.close();
  server.close();
}
if (results.some((ok) => !ok) || !results.length) process.exit(1);
console.log('OK');
