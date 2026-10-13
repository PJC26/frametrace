// UX-22 regression check: the calendar and the facet tree count each
// dimension with its OWN filter excluded, so picking a day does not collapse
// the calendar to that day; the year selector jumps across years; and the
// facet counts are computed once per render.
//
//   node scripts/verify-facets.mjs   (needs playwright via NODE_PATH)
import { readFileSync, readdirSync } from 'node:fs';
import { join, dirname } from 'node:path';
import { fileURLToPath } from 'node:url';
import { createServer } from 'node:http';
import { createRequire } from 'node:module';
import { viewerPageHtml } from './lib/app-bundle.mjs';

const root = join(dirname(fileURLToPath(import.meta.url)), '..');
const require = createRequire(import.meta.url);
const { chromium } = require('playwright');

const days = { '20231230': 1, '20240315': 3, '20240316': 2, '20240402': 1 };
function buildViewer() {
  const dir = join(root, 'assets/viewer');
  let js = '';
  for (const part of readdirSync(dir).filter((f) => f.endsWith('.js')).sort()) {
    js += readFileSync(join(dir, part), 'utf8');
    if (!js.endsWith('\n')) js += '\n';
  }
  const videos = [];
  for (const [d, n] of Object.entries(days)) {
    for (let i = 0; i < n; i++) {
      const name = `${d}_1${i}0000_F.mp4`;
      videos.push({ id: `vid_${d}_${i}`, relative_path: `DCIM/${name}`, source_path: `/case/DCIM/${name}`, ffprobe_ok: true, size_bytes: 1000, extension: 'mp4' });
    }
  }
  const data = `window.__FRAMETRACE_DATA__ = ${JSON.stringify({
    manifest: { case_id: 'FT-FACET', title: 'facets' }, caseDir: '/case', scan: { videos },
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
const server = createServer((req, res) => { res.setHeader('Content-Type', 'text/html; charset=utf-8'); res.end(html); });
await new Promise((r) => server.listen(0, '127.0.0.1', r));
const base = `http://127.0.0.1:${server.address().port}/`;
const browser = await chromium.launch();
try {
  // The facet rail (calendar + tree) auto-collapses below 1000px wide;
  // use a wide viewport so the calendar is on screen.
  const page = await browser.newPage({ viewport: { width: 1600, height: 900 } });
  const errors = [];
  page.on('pageerror', (e) => errors.push(String(e)));
  await page.goto(base);
  await page.waitForSelector('.card');
  const calCounts = () => page.evaluate(() => Object.fromEntries([...document.querySelectorAll('#dayHistogram button[data-day]')].map((b) => [b.dataset.day, b.querySelector('.cal-n').textContent])));
  const treeDays = () => page.evaluate(() => {
    const items = [...document.querySelectorAll('#facetTree > *')];
    const out = {};
    for (const el of items) {
      const m = /^(\d{4}-\d\d-\d\d)(\d+)$/.exec(el.textContent.replace(/\s+/g, ''));
      if (m) out[m[1]] = Number(m[2]);
    }
    return out;
  });

  const sections = await page.evaluate(() => [...document.querySelectorAll('#facetTree .tree-sec')].map((n) => n.textContent));
  check('facet tree sections: 녹화 유형 · 출처 · 날짜 · 소리', JSON.stringify(sections) === JSON.stringify(['녹화 유형', '출처', '날짜', '소리']), JSON.stringify(sections));
  check('latest month shown first with counts', JSON.stringify(await calCounts()) === JSON.stringify({ '2024-04-02': '1' }), JSON.stringify(await calCounts()));
  check('year selector present (2 years)', (await page.locator('#calYear option').count()) === 2);
  await page.selectOption('#calYear', '2024');
  await page.evaluate(() => document.getElementById('calPrev').click());
  const march = await calCounts();
  check('prev month shows March counts', march['2024-03-15'] === '3' && march['2024-03-16'] === '2', JSON.stringify(march));

  await page.locator('#dayHistogram button[data-day="2024-03-15"]').click();
  const cards = await page.locator('.card').count();
  check('picking a day filters the grid', cards === 3, String(cards));
  const after = await calCounts();
  check('calendar keeps the other days after a day pick', after['2024-03-16'] === '2' && after['2024-03-15'] === '3', JSON.stringify(after));
  check('calendar still navigable (prev/next/year)', (await page.locator('#calPrev').isEnabled()) && (await page.locator('#calNext').isEnabled()) && (await page.locator('#calYear option').count()) === 2);
  const tree = await treeDays();
  check('facet tree date counts ignore the date filter', tree['2024-03-16'] === 2 && tree['2023-12-30'] === 1 && tree['2024-03-15'] === 3, JSON.stringify(tree));

  await page.selectOption('#calYear', '2023');
  const y23 = await calCounts();
  check('year jump lands on the year\'s latest month', y23['2023-12-30'] === '1', JSON.stringify(y23));

  // Non-date filters still narrow the calendar.
  const runs = await page.evaluate(() => {
    const q = document.getElementById('query');
    q.value = 'zzz-no-match'; q.dispatchEvent(new Event('input', { bubbles: true }));
    return new Promise((r) => setTimeout(() => r(document.querySelectorAll('.card').length), 300));
  });
  check('search narrows grid to nothing', runs === 0, String(runs));
  const calEmpty = await page.evaluate(() => document.getElementById('dayHistogram').textContent.trim());
  check('calendar reflects non-date filters (empty search -> no days)', calEmpty.length > 0 && (await calCounts()) && Object.keys(await calCounts()).length === 0, calEmpty);
  check('no page errors', !errors.length, errors.join(' | '));
} finally {
  await browser.close();
  server.close();
}
const failed = results.filter((r) => !r).length;
console.log(failed ? `\n${failed} check(s) failed` : '\nall checks passed');
process.exit(failed ? 1 : 0);
