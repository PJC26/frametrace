// Screenshot gallery: builds the Rust workstation, starts it on a free port
// against a synthetic dashcam folder and saves named PNGs of every stage.
//
//   node scripts/capture-screens.mjs [--repo <dir>] [--out <dir>] [--target-dir <dir>]
//                                     [--theme light|dark] [--no-build] [--keep]
//
//   --repo        source tree to build+serve (default: this checkout). Point it
//                 at a `git worktree` of an older commit to capture "before".
//   --out         PNG output dir (default <repo-of-this-script>/docs/screens/current)
//   --target-dir  CARGO_TARGET_DIR (default /tmp/ft-target-F, or ft-target-F-<hash> for --repo)
//
// Needs: node 18+, ffmpeg, Playwright (npm i -D playwright, or a global
// install found via NODE_PATH / /opt/*-tools) and a Chromium. Set
// CHROMIUM_PATH to use a pre-installed browser instead of Playwright's own.
import { spawn, spawnSync } from 'node:child_process';
import { createRequire } from 'node:module';
import { existsSync, mkdirSync, mkdtempSync, readdirSync, rmSync, statSync, writeFileSync } from 'node:fs';
import { createServer } from 'node:net';
import { tmpdir } from 'node:os';
import { dirname, join, resolve } from 'node:path';
import { fileURLToPath } from 'node:url';

const here = dirname(fileURLToPath(import.meta.url));
const selfRepo = resolve(here, '..');
const argv = process.argv.slice(2);
const opt = (name, def) => { const i = argv.indexOf('--' + name); return i >= 0 && argv[i + 1] && !argv[i + 1].startsWith('--') ? argv[i + 1] : def; };
const flag = (name) => argv.includes('--' + name);
const repo = resolve(opt('repo', selfRepo));
const outDir = resolve(opt('out', join(selfRepo, 'docs/screens/current')));
const theme = opt('theme', 'light');
const targetDir = resolve(opt('target-dir', repo === selfRepo ? '/tmp/ft-target-F' : '/tmp/ft-target-F-' + Buffer.from(repo).toString('hex').slice(-8)));
const sleep = (ms) => new Promise((r) => setTimeout(r, ms));

// ---- Playwright + Chromium -------------------------------------------------
function loadPlaywright() {
  const roots = [process.cwd(), selfRepo, '/opt/node-tools', '/opt/npm-tools', ...(process.env.NODE_PATH || '').split(':').filter(Boolean)];
  for (const r of roots) {
    try { return createRequire(join(r, 'x.js'))('playwright'); } catch { /* next */ }
    try { return createRequire(join(r, 'node_modules', 'x.js'))('playwright'); } catch { /* next */ }
  }
  throw new Error('playwright not found: run `npm i --no-save playwright` (do not need `playwright install` when CHROMIUM_PATH is set)');
}
const { chromium } = loadPlaywright();
const chromiumPath = process.env.CHROMIUM_PATH || ['/opt/pw-browsers/chromium'].find((p) => existsSync(p));

// ---- build ------------------------------------------------------------------
const env = { ...process.env, CARGO_TARGET_DIR: targetDir };
if (!env.RUSTUP_TOOLCHAIN && process.platform === 'linux' && existsSync(join(process.env.HOME || '', '.rustup/toolchains/stable-x86_64-unknown-linux-gnu'))) env.RUSTUP_TOOLCHAIN = 'stable-x86_64-unknown-linux-gnu';
if (!flag('no-build')) {
  console.log(`[build] ${repo} -> ${targetDir}`);
  const b = spawnSync('cargo', ['build', '--bin', 'frametrace-app'], { cwd: repo, env, stdio: 'inherit' });
  if (b.status !== 0) { console.error('cargo build failed'); process.exit(1); }
}
const exe = join(targetDir, 'debug', process.platform === 'win32' ? 'frametrace-app.exe' : 'frametrace-app');
if (!existsSync(exe)) { console.error('missing binary ' + exe); process.exit(1); }

// ---- demo source + isolated state ------------------------------------------
const work = mkdtempSync(join(tmpdir(), 'ft-shots-'));
const srcDir = join(work, 'dashcam-source');
const caseDir = join(work, 'case-demo');
const demo = spawnSync(process.execPath, [join(here, 'make-demo-case.mjs'), srcDir], { stdio: 'inherit' });
if (demo.status !== 0) process.exit(1);

const port = await new Promise((res, rej) => { const s = createServer(); s.listen(0, '127.0.0.1', () => { const p = s.address().port; s.close(() => res(p)); }); s.on('error', rej); });
const base = `http://127.0.0.1:${port}`;
// SEC-04: the server requires a token by default; pin a known one for the harness.
const token = 'capture-' + Math.random().toString(36).slice(2, 12);
const authHeaders = { 'X-FrameTrace-Token': token };
const server = spawn(exe, [String(port)], {
  env: { ...process.env, FRAMETRACE_NO_BROWSER: '1', FRAMETRACE_TOKEN: token, XDG_CONFIG_HOME: join(work, 'cfg'), APPDATA: join(work, 'cfg'), HOME: process.env.HOME },
  stdio: ['ignore', 'pipe', 'pipe'],
});
let serverLog = '';
server.stdout.on('data', (d) => { serverLog += d; });
server.stderr.on('data', (d) => { serverLog += d; });
const cleanup = () => { try { server.kill(); } catch { /* gone */ } if (!flag('keep')) rmSync(work, { recursive: true, force: true }); };
process.on('exit', cleanup);
process.on('SIGINT', () => process.exit(130));
for (let i = 0; ; i++) {
  try { if ((await fetch(base + '/api/status', { headers: authHeaders })).ok) break; } catch { /* not yet */ }
  if (i > 100) { console.error('server did not start\n' + serverLog); process.exit(1); }
  await sleep(100);
}

// ---- drive the UI -----------------------------------------------------------
mkdirSync(outDir, { recursive: true });
for (const f of readdirSync(outDir)) if (/^(\d\d|zz).*\.png$/.test(f)) rmSync(join(outDir, f));
const browser = await chromium.launch({ headless: true, ...(chromiumPath ? { executablePath: chromiumPath } : {}), args: ['--no-sandbox', '--autoplay-policy=no-user-gesture-required', '--mute-audio'] });
const ctx = await browser.newContext({ viewport: { width: 1440, height: 900 }, locale: 'ko-KR', timezoneId: 'Asia/Seoul', colorScheme: theme, deviceScaleFactor: 1 });
await ctx.addInitScript((t) => { try { localStorage.setItem('ft-theme', t); } catch { /* ignore */ } }, theme);
const page = await ctx.newPage();
const consoleErrors = [];
page.on('pageerror', (e) => consoleErrors.push('pageerror: ' + e.message));
page.on('console', (m) => { if (m.type() === 'error') consoleErrors.push('console: ' + m.text()); });

const shots = [];
async function shot(name, desc, target = page) {
  const file = join(outDir, name + '.png');
  await target.screenshot({ path: file, fullPage: false });
  shots.push({ name, desc, bytes: statSync(file).size });
  console.log('[shot] ' + name);
}
async function status() { return (await fetch(base + '/api/status', { headers: authHeaders })).json(); }
async function waitPhase(want, ms) {
  const t0 = Date.now();
  while (Date.now() - t0 < ms) { const s = await status(); if (want.includes(s.phase)) return s; if (s.phase === 'error' || s.phase === 'failed') throw new Error('pipeline ' + s.phase); await sleep(250); }
  throw new Error('timeout waiting for ' + want);
}

try {
  await page.goto(base + '/?token=' + token, { waitUntil: 'load' });
  await page.waitForSelector('#sourcePath');
  await sleep(600);
  await shot('00-onboarding', '첫 실행 안내 카드가 보이는 입력 화면 (First-run onboarding on the input stage)');
  if (await page.locator('#btnOnboardOk').isVisible()) await page.click('#btnOnboardOk');
  await sleep(300);
  await shot('01-start', '입력 단계 — 소스/케이스 경로가 비어 있는 초기 화면 (Input stage, empty)');
  await page.fill('#sourcePath', srcDir);
  await page.fill('#caseDir', caseDir);
  await page.dispatchEvent('#sourcePath', 'input');
  await page.dispatchEvent('#caseDir', 'input');
  await sleep(400);
  await shot('01b-start-filled', '입력 단계 — 데모 소스와 케이스 폴더를 채운 상태 (Input stage, filled)');
  await page.click('#btnStart');
  await page.waitForSelector('#progCard.on', { timeout: 15000 }).catch(() => {});
  // The first status poll fills the step track and log; wait for it (best effort).
  await page.waitForFunction(() => document.querySelectorAll('#track li').length > 0 && (document.getElementById('log')?.textContent || '').trim().length > 0, null, { timeout: 8000 }).catch(() => {});
  await shot('02-scan-running', '분석 진행 단계 — 단계 표시와 실시간 로그 (Progress stage with live log)');
  await waitPhase(['review-ready', 'done'], 180000);
  await page.waitForSelector('#s3:not([disabled])', { timeout: 20000 });
  await page.click('#s3');
  await page.waitForSelector('#reviewFrame', { state: 'attached' });
  const frame = page.frameLocator('#reviewFrame');
  await frame.locator('.card[data-id]').first().waitFor({ timeout: 30000 });
  await sleep(1200);
  await shot('03-review-embedded', '검토 단계 — 셸 안에 임베드된 증거 뷰어 (Review stage, viewer embedded in the shell)');

  // Standalone viewer (what examiners open full-screen / in a new window).
  const vpage = await ctx.newPage();
  await vpage.goto(base + '/review/evidence-viewer.html', { waitUntil: 'load' });
  await vpage.locator('.card[data-id]').first().waitFor({ timeout: 30000 });
  await sleep(1200);
  await shot('04-viewer-grid', '증거 뷰어 — 레코드 그리드와 필터 (Viewer grid + filters)', vpage);
  await vpage.locator('.card[data-id]').first().click();
  await sleep(900);
  await shot('05-viewer-detail', '증거 뷰어 — 선택한 영상의 상세/플레이어 패널 (Viewer detail + player)', vpage);
  const second = vpage.locator('.card[data-id]').nth(1);
  if (await second.count()) { await second.click({ modifiers: ['Control'] }).catch(() => {}); await sleep(500); await shot('06-viewer-multiselect', '증거 뷰어 — 다중 선택 상태 (Viewer multi-select)', vpage); }
  await vpage.close();

  await page.bringToFront();
  await page.click('#s4');
  await sleep(600);
  await shot('07-finalize', '결과물 단계 — 보고서 생성 및 패키징 전 (Finalize stage before generating)');
  await page.click('#btnFinalize');
  // Older UIs finalize immediately; newer ones show a pre-finalize dialog.
  const dlg = await page.waitForSelector('#dlgExaminer', { timeout: 4000 }).catch(() => null);
  if (dlg) {
    await sleep(500);
    await shot('07b-finalize-precheck', '최종화 전 점검 대화상자 — 검토자 이름과 경고 목록 (Pre-finalize checklist dialog)');
    await page.fill('#dlgExaminer', '데모 검토자');
    await page.getByRole('button', { name: '보고서 생성 진행' }).click();
  }
  await waitPhase(['done'], 180000);
  await sleep(2500);
  await shot('08-report', '결과물 단계 — 생성된 보고서와 패키지 (Finalize stage after report + package)');

  await page.click('#btnTheme').catch(() => {});
  await sleep(500);
  await shot('09-alt-theme', '테마 전환 후 결과물 화면 (After toggling the theme)');
} catch (e) {
  console.error('capture failed: ' + e.message);
  try { await shot('zz-failure', '캡처 실패 시점의 화면 (State at failure)'); } catch { /* ignore */ }
  process.exitCode = 1;
}

await browser.close();

// ---- contact page -----------------------------------------------------------
const rel = outDir.startsWith(join(selfRepo, 'docs/screens') + '/') ? outDir.slice(join(selfRepo, 'docs/screens').length + 1) : null;
if (rel === 'current') {
  const rows = shots.map((s) => `| ![${s.name}](current/${s.name}.png) | \`${s.name}.png\` | ${s.desc} |`).join('\n');
  writeFileSync(join(selfRepo, 'docs/screens/INDEX.md'),
    `# UI screenshot index\n\nGenerated by \`node scripts/capture-screens.mjs\` (viewport 1440x900, ko-KR, ${theme} theme, synthetic dashcam demo case).\nBaseline captures of the pre-change UI live in \`before/\`; see [../SCREENSHOTS.md](../SCREENSHOTS.md).\n\n| Preview | File | What it shows |\n|---|---|---|\n${rows}\n`);
}
const blank = shots.filter((s) => s.bytes < 8000);
console.log(`${shots.length} screenshots in ${outDir}` + (blank.length ? `; SUSPECT BLANK: ${blank.map((s) => s.name).join(', ')}` : ''));
if (consoleErrors.length) console.log('page errors:\n  ' + [...new Set(consoleErrors)].slice(0, 10).join('\n  '));
if (blank.length) process.exitCode = 1;
process.exit(process.exitCode || 0);
