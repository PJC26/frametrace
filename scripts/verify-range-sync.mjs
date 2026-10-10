// FOR-01 regression check: IN/OUT ranges persist server-side and a
// server/browser divergence is surfaced with an explicit choice.
//
//   node scripts/verify-range-sync.mjs
//
// Builds a standalone viewer page from assets/ (same bundling as build.rs),
// serves it from a mock case server that stores what /api/import-marks
// receives and returns it as annotations on the next page load.
import { readFileSync, readdirSync } from 'node:fs';
import { join, dirname } from 'node:path';
import { fileURLToPath } from 'node:url';
import { createServer } from 'node:http';
import { createRequire } from 'node:module';

const root = join(dirname(fileURLToPath(import.meta.url)), '..');
const require = createRequire(import.meta.url);
const { chromium } = require('playwright');

const CASE = 'FT-RANGE';
const RANGES_KEY = `ft.viewer.${CASE}.ranges`;

function viewerJs() {
  const dir = join(root, 'assets/viewer');
  let js = '';
  for (const part of readdirSync(dir).filter((f) => f.endsWith('.js')).sort()) {
    js += readFileSync(join(dir, part), 'utf8');
    if (!js.endsWith('\n')) js += '\n';
  }
  return js;
}

// Server-side state: id -> { status, range }
const serverMarks = new Map();
const imports = [];

function buildHtml() {
  const videos = ['a', 'b'].map((n) => ({
    id: `vid_${n}`, relative_path: `DCIM/${n}.mp4`, source_path: `/case/DCIM/${n}.mp4`,
    ffprobe_ok: true, size_bytes: 1000, extension: 'mp4',
  }));
  const marks = [...serverMarks.entries()].map(([id, m]) => ({
    id, status: m.status, marked_unix: 1, note: '', examiner: null,
    ...(m.range ? { range: m.range } : {}),
  }));
  const data = `window.__FRAMETRACE_DATA__ = ${JSON.stringify({
    manifest: { case_id: CASE, title: 'range' }, caseDir: '/case', scan: { videos },
    carveLog: [], filesystemLog: [], validationLog: [], anomalyLog: [], flsEntries: [],
    thumbs: {}, annotations: { marks, tags: [] }, deepfake: {}, telemetry: {}, proxies: [],
  })};`;
  return readFileSync(join(root, 'assets/evidence_viewer.html'), 'utf8')
    .replace('__CSS__', () => readFileSync(join(root, 'assets/evidence_viewer.css'), 'utf8'))
    .replace('__DATA__', () => data)
    .replace('__JS__', () => viewerJs());
}

const results = [];
const check = (name, ok, detail = '') => {
  results.push(ok);
  console.log(`${ok ? 'PASS' : 'FAIL'}  ${name}${detail ? '  — ' + detail : ''}`);
};

const server = createServer((req, res) => {
  if (req.url === '/api/import-marks') {
    let body = '';
    req.on('data', (c) => { body += c; });
    req.on('end', () => {
      const payload = JSON.parse(JSON.parse(body).marks_json);
      imports.push(payload);
      for (const id of payload.deleted_ids || []) serverMarks.delete(id);
      for (const m of payload.marks) {
        const prev = serverMarks.get(m.id) || {};
        // Mirrors patch_review_annotations: absent range keeps, {null,null} clears.
        let range = prev.range;
        if (m.range !== undefined && m.range !== null) {
          range = m.range.in == null && m.range.out == null ? undefined : m.range;
        }
        serverMarks.set(m.id, { status: m.status, range });
      }
      res.setHeader('Content-Type', 'application/json');
      res.end('{"ok":true}');
    });
    return;
  }
  res.setHeader('Content-Type', 'text/html; charset=utf-8');
  res.end(buildHtml());
});
await new Promise((r) => server.listen(0, '127.0.0.1', r));
const base = `http://127.0.0.1:${server.address().port}/`;
const browser = await chromium.launch();

async function open(page, lsRanges) {
  await page.goto(base);
  await page.evaluate(({ key, value }) => {
    try {
      localStorage.clear();
      if (value) localStorage.setItem(key, JSON.stringify(value));
    } catch {}
  }, { key: RANGES_KEY, value: lsRanges });
  await page.reload();
  await page.waitForSelector('.card');
}
const waitSaved = (page) => page.waitForFunction(
  () => document.getElementById('saveState').dataset.state === 'saved', null, { timeout: 8000 });

try {
  const page = await browser.newPage();
  const errors = [];
  page.on('pageerror', (e) => errors.push(String(e)));

  // (1) Set IN/OUT: the range rides in the payload and lands on the server.
  await open(page, null);
  await page.locator('.card').first().click();
  const id = await page.locator('.card').first().getAttribute('data-id');
  await page.evaluate(() => { document.activeElement?.blur(); });
  await page.waitForSelector('#mediaStage video', { state: 'attached' });
  await page.evaluate(() => {
    const video = document.querySelector('#mediaStage video');
    Object.defineProperty(video, 'currentTime', { get: () => window.__t, configurable: true });
    window.__t = 12.5;
  });
  await page.evaluate(() => document.getElementById('btnSetIn').click());
  await page.evaluate(() => { window.__t = 20; });
  await page.evaluate(() => document.getElementById('btnSetOut').click());
  await page.locator('#saveState').click();
  await waitSaved(page);
  const last = imports.at(-1);
  const sent = last?.marks.find((m) => m.id === id);
  check('payload carries range {in,out}', sent?.range?.in === 12.5 && sent?.range?.out === 20, JSON.stringify(sent));
  check('server stored the range', serverMarks.get(id)?.range?.out === 20);

  // (2) Same ranges in the browser -> no banner.
  await open(page, { [id]: { in: 12.5, out: 20 } });
  check('no banner when server and browser agree', (await page.locator('#rangeDivergenceBanner').count()) === 0);

  // (3) Different browser range -> banner; routine edit must not overwrite.
  await open(page, { [id]: { in: 3, out: 4 } });
  const banner = page.locator('#rangeDivergenceBanner');
  check('divergence banner shown', (await banner.count()) === 1, await banner.innerText().catch(() => ''));
  check('banner wording', /서버와 다른 1건/.test(await banner.innerText()) && /서버 기준/.test(await banner.innerText()) && /브라우저 기준/.test(await banner.innerText()));
  imports.length = 0;
  await page.locator('.card').first().click();
  await page.evaluate(() => document.activeElement?.blur());
  await page.keyboard.press('1');
  await page.locator('#saveState').click();
  await waitSaved(page);
  const during = imports.at(-1)?.marks.find((m) => m.id === id);
  check('unresolved conflict never sends a range', during && !('range' in during), JSON.stringify(during));
  check('server range untouched while unresolved', serverMarks.get(id)?.range?.out === 20);

  // (4) Choose server: browser adopts server range, nothing is pushed.
  await page.locator('#rangeUseServer').click();
  const afterServer = await page.evaluate((key) => JSON.parse(localStorage.getItem(key)), RANGES_KEY);
  check('"서버 기준" adopts server range', afterServer?.[id]?.in === 12.5 && afterServer?.[id]?.out === 20, JSON.stringify(afterServer));
  check('banner dismissed', (await banner.count()) === 0);

  // (5) Choose browser: browser range is pushed to the server.
  await open(page, { [id]: { in: 3, out: 4 } });
  imports.length = 0;
  await page.locator('#rangeUseBrowser').click();
  await page.locator('#saveState').click();
  await waitSaved(page);
  const pushed = imports.at(-1)?.marks.find((m) => m.id === id);
  check('"브라우저 기준" pushes browser range', pushed?.range?.in === 3 && pushed?.range?.out === 4, JSON.stringify(pushed));
  check('server now has browser range', serverMarks.get(id)?.range?.in === 3);

  // (6) Browser has no range, server does, choose browser -> server cleared.
  await open(page, null);
  check('banner when browser lacks a server range', (await page.locator('#rangeDivergenceBanner').count()) === 1);
  imports.length = 0;
  await page.locator('#rangeUseBrowser').click();
  await page.locator('#saveState').click();
  await waitSaved(page);
  const cleared = imports.at(-1)?.marks.find((m) => m.id === id);
  check('browser choice clears the server range', !serverMarks.get(id)?.range, JSON.stringify({ cleared, deleted: imports.at(-1)?.deleted_ids }));

  check('no page errors', !errors.length, errors.join(' | '));
} finally {
  await browser.close();
  server.close();
}
if (results.some((ok) => !ok) || !results.length) process.exit(1);
