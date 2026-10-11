// UX-02 end-to-end check: the folder pipeline opens review (phase
// "review-partial") after the hash-less scan while the hash pass still
// runs, /api/records?live=1 serves that index, /viewer/ is reachable before
// make-review, and the run ends in "review-ready". With Playwright
// available it also loads /viewer/ during enrichment and checks the live
// chip (screenshot to the given directory).
//
//   cargo build && node scripts/verify-live-index.mjs [shots-dir]
//
// Uses a throwaway XDG_CONFIG_HOME so the real workstation session file is
// never touched, and synthesizes 30 × 20 MiB files so hashing is slow
// enough to observe phase 2.
import { mkdtempSync, mkdirSync, writeFileSync, rmSync, existsSync } from 'node:fs';
import { join, dirname } from 'node:path';
import { tmpdir } from 'node:os';
import { fileURLToPath } from 'node:url';
import { spawn } from 'node:child_process';
import { createRequire } from 'node:module';
import { randomFillSync } from 'node:crypto';

const root = join(dirname(fileURLToPath(import.meta.url)), '..');
const shots = process.argv[2] || '';
const exe = join(root, 'target/debug/frametrace' + (process.platform === 'win32' ? '.exe' : ''));
if (!existsSync(exe)) { console.error('build first: cargo build'); process.exit(2); }

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
const server = spawn(exe, [], {
  env: { ...process.env, XDG_CONFIG_HOME: join(work, 'xdg'), APPDATA: join(work, 'xdg'), FRAMETRACE_NO_BROWSER: '1' },
  stdio: ['ignore', 'pipe', 'pipe'],
});
const fail = (msg) => { console.error('FAIL:', msg); cleanup(); process.exit(1); };
function cleanup() { try { server.kill(); } catch {} try { rmSync(work, { recursive: true, force: true }); } catch {} }

const base = await new Promise((resolve, reject) => {
  let out = '';
  server.stdout.on('data', (d) => { out += d; const m = out.match(/http:\/\/127\.0\.0\.1:\d+/); if (m) resolve(m[0]); });
  server.on('exit', (code) => reject(new Error(`server exited ${code}`)));
  setTimeout(() => reject(new Error('server did not start')), 15000);
}).catch((e) => fail(e.message));

const home = await fetch(base + '/');
const nonce = (home.headers.get('set-cookie') || '').match(/ft_nonce=([^;]+)/)?.[1];
if (!nonce) fail('no nonce cookie');
const getJson = async (path) => (await fetch(base + path, { cache: 'no-store' })).json();
const start = await (await fetch(base + '/api/start', {
  method: 'POST',
  headers: { 'Content-Type': 'application/json', 'X-FrameTrace-Nonce': nonce, Cookie: `ft_nonce=${nonce}` },
  body: JSON.stringify({ input_kind: 'folder', source_path: src, case_dir: caseDir, with_hash: true, with_ffprobe: false }),
})).json();
if (!start.ok) fail('start: ' + start.error);

const phases = [];
let partialChecked = false;
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
    const viewer = await fetch(base + '/viewer/?embed=1');
    console.log('viewer status:', viewer.status, 'review/index.html exists:', existsSync(join(caseDir, 'review/index.html')));
    if (viewer.status !== 200) fail('/viewer/ not served');
    if (chromium) {
      const browser = await chromium.launch();
      const page = await browser.newPage({ viewport: { width: 1400, height: 860 } });
      const errors = [];
      page.on('pageerror', (e) => errors.push(String(e)));
      await page.goto(base + '/viewer/');
      await page.waitForSelector('#ft-live-chip', { timeout: 20000 });
      await page.waitForTimeout(1500);
      const chip = await page.textContent('#ft-live-chip');
      const cards = await page.locator('#recordGrid .card').count();
      console.log('chip:', chip.trim(), '| cards:', cards, '| page errors:', errors.length);
      if (shots) { mkdirSync(shots, { recursive: true }); await page.screenshot({ path: join(shots, 'live-index-chip.png') }); }
      await browser.close();
      if (!/색인/.test(chip) || errors.length) fail('live chip / page errors: ' + errors.join('; '));
    } else {
      console.log('playwright not resolvable — browser check skipped');
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
console.log('OK');
cleanup();
process.exit(0);
