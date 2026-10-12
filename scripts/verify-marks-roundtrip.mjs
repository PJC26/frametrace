// Marks saved to the case DB must reappear in a FRESH browser (no
// localStorage): records-meta keys marks by `record_id`, and the viewer once
// read only `mark.id`, so server-saved marks, notes, examiners and IN/OUT
// ranges silently never hydrated on another PC, another port or after the
// browser storage was cleared. This drives the real server end to end:
// analyse a tiny folder, save annotations through /api/import-marks exactly
// as the viewer does, then open the integrated page in a brand-new context.
//
//   node scripts/verify-marks-roundtrip.mjs
import { mkdtempSync, mkdirSync, writeFileSync, rmSync } from 'node:fs';
import { join, dirname } from 'node:path';
import { tmpdir } from 'node:os';
import { fileURLToPath } from 'node:url';
import { randomBytes } from 'node:crypto';
import { createRequire } from 'node:module';
import { startIsolatedServer } from './lib/isolated-server.mjs';

const root = join(dirname(fileURLToPath(import.meta.url)), '..');
const require = createRequire(join(root, 'package.json'));
const { chromium } = require('playwright');

let failed = 0;
const check = (name, ok, detail = '') => {
  console.log(`${ok ? 'PASS' : 'FAIL'}  ${name}${detail ? '  — ' + detail : ''}`);
  if (!ok) failed += 1;
};
const until = async (fn, ms = 20000, step = 250) => {
  const end = Date.now() + ms;
  while (Date.now() < end) {
    const v = await fn().catch(() => null);
    if (v) return v;
    await new Promise((r) => setTimeout(r, step));
  }
  return null;
};

const work = mkdtempSync(join(tmpdir(), 'ft-marks-rt-'));
const src = join(work, 'source');
mkdirSync(src);
for (const name of ['20260909_083000_NF.mp4', '20260909_083100_NF.mp4']) writeFileSync(join(src, name), randomBytes(4096));

const srv = await startIsolatedServer({ root, work });
let browser;
try {
  const start = await srv.postJson('/api/start', { input_kind: 'folder', source_path: src, case_dir: join(work, 'case'), with_hash: false, with_ffprobe: false });
  check('analysis starts', start.ok === true, start.error || '');
  const ready = await until(async () => {
    const st = await srv.getJson('/api/status');
    return st.phase === 'review-ready' && !st.busy ? st : null;
  }, 120000, 500);
  check('analysis reaches review-ready', !!ready);

  const recs = await srv.getJson('/api/records?offset=0&limit=10');
  const id = recs.videos && recs.videos[0] && recs.videos[0].id;
  check('a record exists', !!id, String(id));

  // Same body the viewer's pushMarksToCase() sends.
  const payload = { marks: [{ id, status: 'important', note: '왕복 확인 메모', examiner: '김검토', range: { in: 1.5, out: 3 } }], tags: [{ id, tags: ['핵심증거'] }], deleted_ids: [] };
  const imp = await srv.postJson('/api/import-marks', { marks_json: JSON.stringify(payload) });
  check('marks saved to the case DB', imp.ok === true, imp.error || '');
  const meta = await srv.getJson('/api/records-meta');
  const row = (meta.annotations && meta.annotations.marks || [])[0] || {};
  check('records-meta returns the saved row', (row.record_id || row.id) === id && row.status === 'important', JSON.stringify(row).slice(0, 160));

  browser = await chromium.launch();
  const ctx = await browser.newContext({ viewport: { width: 1366, height: 768 } }); // fresh: empty localStorage
  const page = await ctx.newPage();
  const errors = [];
  page.on('pageerror', (e) => errors.push(String(e)));
  await page.goto(`${srv.base}/?token=${srv.token}`);
  const card = `[data-id="${id}"]`;
  const chip = await until(async () => (await page.locator(`${card} .mark-chip.important`).count()) > 0, 30000);
  check('fresh browser: grid card shows the saved 중요 mark', !!chip);

  await page.click(card);
  const note = await until(async () => {
    const v = await page.locator('#evidenceNote').inputValue().catch(() => '');
    return v === '왕복 확인 메모' ? v : null;
  }, 10000);
  check('fresh browser: saved note appears in the detail panel', !!note);
  const range = await until(async () => {
    const txt = (await page.locator('#rangeLabel').textContent()) || '';
    return /1\.5/.test(txt) && /3\.0/.test(txt) ? txt : null;
  }, 10000);
  check('fresh browser: saved IN/OUT range appears', !!range, range || '');
  const tag = await until(async () => (await page.locator(`${card}`).textContent() || '').includes('핵심증거') || (await page.locator('.tag-btn.on, .tag-chip').allTextContents()).join(' ').includes('핵심증거'), 5000);
  check('fresh browser: saved tag appears', !!tag);
  check('no page errors', errors.length === 0, errors.join(' | ').slice(0, 200));
} finally {
  if (browser) await browser.close();
  await srv.stop();
  rmSync(work, { recursive: true, force: true, maxRetries: 10, retryDelay: 200 });
}
console.log(failed ? `verify-marks-roundtrip: ${failed} failed` : 'verify-marks-roundtrip: all checks passed');
process.exit(failed ? 1 : 0);
