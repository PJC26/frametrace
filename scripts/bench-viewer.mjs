// Repeatable evidence-viewer render benchmark (R4 F5).
//
// Loads the REAL viewer bundle (assets/viewer/*.js + CSS + the shared
// fragment, assembled exactly like build.rs / GET /viewer/ via
// lib/app-bundle.mjs viewerPageHtml) in headless Chromium with a synthetic
// 10k-record case, then drives the real search box and times the viewer's own
// render() — filter + sort + facets + histogram + tree + virtual grid paint —
// by capturing its debounce-timer callback and running it under
// performance.now(). Nothing in this file re-implements viewer logic, so a
// regression in assets/viewer/** moves the number (the old version timed a
// hand-written copy of filteredRecords()).
//
// Threshold: median render <= RENDER_BUDGET_MS (150 ms, see below).
//
// Run: node scripts/bench-viewer.mjs   (needs Playwright; FAILS without it
// when CI / FRAMETRACE_REQUIRE_BROWSER=1)
import { createServer } from 'node:http';
import { createRequire } from 'node:module';
import { viewerPageHtml, repoRoot } from './lib/app-bundle.mjs';

const ROWS = 10_000;
const RENDERS = 40;
// The real render builds DOM (virtual grid, tree, histogram), so it costs more
// than the filter+sort-only copy the old bench timed (~75 ms median measured on
// a dev laptop vs. <10 ms for that copy). 150 ms keeps ~2x headroom for CI
// runners; override with FRAMETRACE_BENCH_BUDGET_MS.
const RENDER_BUDGET_MS = Number(process.env.FRAMETRACE_BENCH_BUDGET_MS) || 150;

const require = createRequire(import.meta.url);
let chromium;
try {
  ({ chromium } = require('playwright'));
} catch {
  if (process.env.CI || process.env.FRAMETRACE_REQUIRE_BROWSER === '1') {
    console.error('FAIL: playwright not resolvable but the bench is required (CI / FRAMETRACE_REQUIRE_BROWSER=1)');
    process.exit(1);
  }
  console.log('playwright not resolvable — bench skipped (set FRAMETRACE_REQUIRE_BROWSER=1 to forbid)');
  process.exit(0);
}

const videos = Array.from({ length: ROWS }, (_, i) => {
  const n = String(i).padStart(6, '0');
  const day = String((i % 28) + 1).padStart(2, '0');
  return {
    id: `vid_${n}`,
    relative_path: `DCIM/CAM${i % 32}/CAM${i % 32}_2024-01-${day}_${n}.mp4`,
    source_path: `/case/DCIM/CAM${i % 32}/CAM${i % 32}_2024-01-${day}_${n}.mp4`,
    ffprobe_ok: i % 4 !== 3,
    size_bytes: 10_000_000 + i,
    extension: 'mp4',
  };
});
const data = `window.__FRAMETRACE_DATA__ = ${JSON.stringify({
  manifest: { case_id: 'FT-BENCH', title: 'bench' }, caseDir: '/case', scan: { videos },
  carveLog: [], filesystemLog: [], validationLog: [], anomalyLog: [], flsEntries: [],
  thumbs: {}, annotations: {}, deepfake: {}, telemetry: {}, proxies: [],
})};`;
const html = viewerPageHtml({ data, root: repoRoot });

const server = createServer((req, res) => {
  res.setHeader('Content-Type', 'text/html; charset=utf-8');
  res.end(html);
});
await new Promise((r) => server.listen(0, '127.0.0.1', r));
const browser = await chromium.launch();
let perRender;
try {
  const page = await browser.newPage({ viewport: { width: 1500, height: 900 } });
  const errors = [];
  page.on('pageerror', (e) => errors.push(String(e)));
  await page.goto(`http://127.0.0.1:${server.address().port}/`);
  await page.waitForSelector('#recordGrid .card', { timeout: 60_000 });
  const times = await page.evaluate(async (renders) => {
    // The search box debounces with setTimeout and then calls the viewer's
    // own render() synchronously: capture that timer callback and run it
    // under performance.now() (restoring setTimeout straight after).
    const realSetTimeout = window.setTimeout;
    const input = document.getElementById('query');
    const out = [];
    const queries = ['cam1', 'cam2', 'cam3', 'cam17'];
    for (let i = 0; i < renders + 1; i++) {
      let captured = null;
      window.setTimeout = (cb) => { captured = cb; return 0; };
      input.value = queries[i % queries.length]; // alternate: the filter cache must miss
      input.dispatchEvent(new Event('input', { bubbles: true }));
      window.setTimeout = realSetTimeout;
      if (!captured) throw new Error('search did not schedule a render');
      const t0 = performance.now();
      captured();
      out.push(performance.now() - t0);
      await new Promise((r) => realSetTimeout(r, 20));
    }
    out.shift(); // warm-up
    return out;
  }, RENDERS);
  if (errors.length) {
    console.error('FAIL: viewer page errors:\n' + errors.join('\n'));
    process.exit(1);
  }
  times.sort((a, b) => a - b);
  perRender = times[Math.floor(times.length / 2)];
  const worst = times[times.length - 1];
  console.log(`real viewer bundle: ${ROWS} rows, median ${perRender.toFixed(2)} ms/render (worst ${worst.toFixed(2)} ms) over ${RENDERS} search renders (budget ${RENDER_BUDGET_MS} ms)`);
} finally {
  await browser.close();
  server.close();
}
if (perRender > RENDER_BUDGET_MS) {
  console.error('FAIL: render exceeds budget — memoize filteredRecords or trim the per-render passes');
  process.exit(1);
}
console.log('OK');
