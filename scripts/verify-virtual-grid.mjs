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
