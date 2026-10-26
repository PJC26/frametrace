// UX-02 end-to-end check: the folder pipeline opens review (phase
// "review-partial") after the hash-less scan while the hash pass still
// runs, /api/records?live=1 serves that index, /viewer/ is reachable before
// make-review, and the run ends in "review-ready". With Playwright
// available it also loads /viewer/ during enrichment and checks the live
// chip (screenshot to the given directory).
//
//   cargo build && node scripts/verify-live-index.mjs [shots-dir]
//
// Runs its own isolated server (scripts/lib/isolated-server.mjs) so it can
// never attach to another workstation, and synthesizes 30 × 20 MiB files so hashing is slow
// enough to observe phase 2.
import { mkdtempSync, mkdirSync, writeFileSync, rmSync, existsSync, readFileSync } from 'node:fs';
import { join, dirname } from 'node:path';
import { tmpdir } from 'node:os';
import { fileURLToPath } from 'node:url';
import { startIsolatedServer } from './lib/isolated-server.mjs';
import { createRequire } from 'node:module';
import { randomFillSync } from 'node:crypto';

const root = join(dirname(fileURLToPath(import.meta.url)), '..');
const shots = process.argv[2] || '';

let chromium = null;
try { ({ chromium } = createRequire(import.meta.url)('playwright')); } catch { /* HTTP-only */ }

const work = mkdtempSync(join(tmpdir(), 'ft-live-'));
const src = join(work, 'src');
mkdirSync(src);
const chunk = Buffer.alloc(20 * 1024 * 1024);
for (let i = 10; i < 40; i++) {
  randomFillSync(chunk);
  writeFileSync(join(src, `20260916-13h41m${i}s_F.mp4`), chunk);
}
const caseDir = join(work, 'case');
// Isolated dedicated server (explicit port skips the reuse-a-running-
// workstation discovery; throwaway HOME/XDG/APPDATA/instance file; pinned
// token) — see scripts/lib/isolated-server.mjs.
const fail = (msg) => { console.error('FAIL:', msg); cleanup(); process.exit(1); };
let server = null;
function cleanup() { try { server && server.stop(); } catch {} try { rmSync(work, { recursive: true, force: true }); } catch {} }
try {
  server = await startIsolatedServer({ root, work });
} catch (e) {
  fail(e.message);
}
const { base, token } = server;
const authHeaders = server.headers;
// With a token configured the server authenticates by token and does not
// require the per-instance nonce.
const nonceHeaders = {};
const getJson = server.getJson;
const start = await (await fetch(base + '/api/start', {
  method: 'POST',
  headers: { ...authHeaders, ...nonceHeaders, 'Content-Type': 'application/json' },
  body: JSON.stringify({ input_kind: 'folder', source_path: src, case_dir: caseDir, with_hash: true, with_ffprobe: false }),
})).json();
if (!start.ok) fail('start: ' + start.error);

const phases = [];
let partialChecked = false;
let markSavedDuringPartial = false;
const deadline = Date.now() + 300000;
let st;
while (Date.now() < deadline) {
  st = await getJson('/api/status');
  if (phases[phases.length - 1] !== st.phase) { phases.push(st.phase); console.log('phase:', st.phase, st.enrichment ? JSON.stringify(st.enrichment) : ''); }
  if (st.phase === 'review-partial' && st.busy && !partialChecked) {
    partialChecked = true;
    const rec = await getJson('/api/records?offset=0&limit=500&live=1');
    console.log(`live records: ok=${rec.ok} live=${rec.live} total=${rec.total} generation=${rec.generation} hashed=${rec.videos.filter((v) => v.sha256).length}`);
    if (!rec.ok || !rec.live || rec.total !== 30) fail('live records during review-partial');
    const viewer = await fetch(base + '/viewer/?embed=1', { headers: authHeaders });
    console.log('viewer status:', viewer.status, 'review/index.html exists:', existsSync(join(caseDir, 'review/index.html')));
    if (viewer.status !== 200) fail('/viewer/ not served');
    // Marks saved during review-partial must be accepted (applied or queued
    // behind the hash/make-review step) and persisted by review-ready.
    const caseId = JSON.parse(readFileSync(join(caseDir, 'case.json'), 'utf8')).case_id;
    const markBody = { schema_version: 1, case_id: caseId, marks: [{ id: 'vid_000001', status: 'important', note: 'saved during review-partial' }] };
    const imp = await (await fetch(base + '/api/import-marks', {
      method: 'POST',
      headers: { ...authHeaders, ...nonceHeaders, 'Content-Type': 'text/plain' },
      body: JSON.stringify({ marks_json: JSON.stringify(markBody) }),
    })).json();
    console.log('import-marks during review-partial:', JSON.stringify(imp));
    if (!imp.ok || !(imp.applied || imp.deferred)) fail('import-marks refused during review-partial: ' + JSON.stringify(imp));
    markSavedDuringPartial = true;
    if (chromium) {
      const browser = await chromium.launch();
      const page = await browser.newPage({ viewport: { width: 1400, height: 860 } });
      const errors = [];
      page.on('pageerror', (e) => errors.push(String(e)));
      await page.goto(base + '/viewer/?token=' + token);
      await page.waitForSelector('#ft-live-chip', { timeout: 20000 });
      await page.waitForTimeout(1500);
      const chip = await page.textContent('#ft-live-chip');
      const cards = await page.locator('#recordGrid .card').count();
      console.log('chip:', chip.trim(), '| cards:', cards, '| page errors:', errors.length);
      if (shots) { mkdirSync(shots, { recursive: true }); await page.screenshot({ path: join(shots, 'live-index-chip.png') }); }
      await browser.close();
      if (!/색인/.test(chip) || errors.length) fail('live chip / page errors: ' + errors.join('; '));
    } else {
      // T7: a skipped browser check must never pass in CI (or when the
      // caller demands it with FRAMETRACE_REQUIRE_BROWSER=1).
      if (process.env.CI || process.env.FRAMETRACE_REQUIRE_BROWSER === '1') {
        fail('playwright not resolvable but a browser check is required (CI / FRAMETRACE_REQUIRE_BROWSER=1)');
      }
      console.log('playwright not resolvable — browser check skipped (HTTP-only run; set FRAMETRACE_REQUIRE_BROWSER=1 to forbid)');
    }
  }
  if (['review-ready', 'error'].includes(st.phase) || (st.phase === 'review-partial' && !st.busy)) break;
  await new Promise((r) => setTimeout(r, 250));
}
console.log('phases:', phases.join(' → '));
const finalRec = await getJson('/api/records?offset=0&limit=500');
console.log(`final: phase=${st.phase} hashed=${finalRec.videos.filter((v) => v.sha256).length}/${finalRec.total} warnings=${st.warnings.length}`);
if (st.phase !== 'review-ready') fail('did not reach review-ready: ' + (st.error || st.phase));
if (!phases.includes('review-partial')) fail('review-partial never observed');
if (finalRec.videos.some((v) => !v.sha256)) fail('phase 2 did not hash every record');
if (!markSavedDuringPartial) fail('never saved a mark during review-partial');
const meta = await getJson('/api/records-meta');
const persisted = (meta.annotations?.marks || []).find((m) => m.record_id === 'vid_000001' || m.id === 'vid_000001');
console.log('mark after review-ready:', JSON.stringify(persisted || null), 'pending_marks=' + st.pending_marks);
if (!persisted || persisted.status !== 'important') fail('mark saved during review-partial missing after review-ready');
if (st.pending_marks) fail('marks still queued after review-ready');
console.log('OK');
cleanup();
process.exit(0);
