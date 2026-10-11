// Screenshot gallery: builds the Rust workstation, starts it on a free port
// against a synthetic dashcam folder and saves named PNGs of every stage.
//
//   node scripts/capture-screens.mjs [--repo <dir>] [--out <dir>] [--target-dir <dir>]
//                                     [--sizes 1366x768,1920x1080] [--themes light,dark]
//                                     [--theme light|dark] [--no-build] [--keep]
//
//   Output layout: <out>/<WxH>-<theme>/<name>.png, plus <out>/INDEX.md.
//   --theme X is shorthand for --themes X. Each size x theme combination gets
//   its own isolated server + fresh case (a full pipeline run).
//
//   --repo        source tree to build+serve (default: this checkout). Point it
//                 at a `git worktree` of an older commit to capture "before".
//   --out         PNG output dir (default <repo-of-this-script>/docs/screens/current)
//   --target-dir  CARGO_TARGET_DIR (default /tmp/ft-target-F, or ft-target-F-<hash> for --repo)
//
// Needs: node 18+, ffmpeg, Playwright (npm i -D playwright, or a global
// install found via NODE_PATH / /opt/*-tools) and a Chromium. Set
// CHROMIUM_PATH to use a pre-installed browser instead of Playwright's own.
import { spawnSync } from 'node:child_process';
import { createRequire } from 'node:module';
import { existsSync, mkdirSync, mkdtempSync, readdirSync, rmSync, statSync, writeFileSync } from 'node:fs';
import { tmpdir } from 'node:os';
import { dirname, join, resolve } from 'node:path';
import { fileURLToPath } from 'node:url';
import { startIsolatedServer } from './lib/isolated-server.mjs';

const here = dirname(fileURLToPath(import.meta.url));
const selfRepo = resolve(here, '..');
const argv = process.argv.slice(2);
const opt = (name, def) => { const i = argv.indexOf('--' + name); return i >= 0 && argv[i + 1] && !argv[i + 1].startsWith('--') ? argv[i + 1] : def; };
const flag = (name) => argv.includes('--' + name);
const repo = resolve(opt('repo', selfRepo));
const outDir = resolve(opt('out', join(selfRepo, 'docs/screens/current')));
const sizes = opt('sizes', '1366x768,1920x1080').split(',').map((z) => {
  const m = /^(\d+)x(\d+)$/.exec(z.trim());
  if (!m) { console.error('bad --sizes entry: ' + z); process.exit(2); }
  return { w: +m[1], h: +m[2] };
});
const themes = opt('themes', opt('theme', 'light,dark')).split(',').map((x) => x.trim());
if (themes.some((x) => x !== 'light' && x !== 'dark')) { console.error('--themes must be light and/or dark'); process.exit(2); }
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
// STR-01: the shell page is bundled at build time from assets/examiner_app.html
// + assets/app/* (and the viewer from assets/viewer/*). With --no-build a
// binary older than those sources would screenshot stale UI.
if (flag('no-build')) {
  const exeTime = statSync(exe).mtimeMs;
  const sources = [join(repo, 'assets/examiner_app.html')];
  for (const dir of ['assets/app', 'assets/viewer']) {
    const abs = join(repo, dir);
    if (existsSync(abs)) for (const f of readdirSync(abs)) sources.push(join(abs, f));
  }
  const stale = sources.filter((f) => existsSync(f) && statSync(f).mtimeMs > exeTime);
  if (stale.length) console.warn(`[warn] --no-build: ${stale.length} UI source file(s) are newer than ${exe} — screenshots may show stale UI (e.g. ${stale[0]})`);
}

// ---- determinism helpers ----------------------------------------------------
const STABLE_CSS = '*,*::before,*::after{animation:none!important;transition:none!important;caret-color:transparent!important;scroll-behavior:auto!important}';
const STABLE_INIT = (css) => {
  const add = () => { const st = document.createElement('style'); st.textContent = css; (document.head || document.documentElement).appendChild(st); };
  if (document.readyState === 'loading') document.addEventListener('DOMContentLoaded', add); else add();
};

// Fonts + images decoded (or failed) + short network quiet; best effort.
async function settle(target, ms = 4000) {
  await target.evaluate(async (limit) => {
    const t0 = Date.now();
    try { await Promise.race([document.fonts.ready, new Promise((r) => setTimeout(r, limit))]); } catch { /* ignore */ }
    while (Date.now() - t0 < limit && [...document.images].some((i) => !i.complete)) await new Promise((r) => setTimeout(r, 100));
  }, ms).catch(() => {});
  if (typeof target.waitForLoadState === 'function') await target.waitForLoadState('networkidle', { timeout: 1500 }).catch(() => {});
  await sleep(150);
}

async function runCombo({ w, h, theme }, sub) {
  const outSub = join(outDir, sub);
  mkdirSync(outSub, { recursive: true });
  for (const f of readdirSync(outSub)) if (/^(\d\d|zz).*\.png$/.test(f)) rmSync(join(outSub, f));
  // Fresh demo source + isolated state per combination.
  const work = mkdtempSync(join(tmpdir(), 'ft-shots-'));
  const srcDir = join(work, 'dashcam-source');
  const caseDir = join(work, 'case-demo');
  const demo = spawnSync(process.execPath, [join(here, 'make-demo-case.mjs'), srcDir], { stdio: 'inherit' });
  if (demo.status !== 0) process.exit(1);
  // Explicit port + throwaway HOME/XDG/APPDATA/instance file + pinned token:
  // never touches ~/.config/frametrace or ~/.frametrace, never attaches to another server.
  const prevTarget = process.env.CARGO_TARGET_DIR;
  process.env.CARGO_TARGET_DIR = targetDir;
  let srv;
  try { srv = await startIsolatedServer({ root: repo, work, token: 'capture-' + Math.random().toString(36).slice(2, 12) }); }
  finally { if (prevTarget === undefined) delete process.env.CARGO_TARGET_DIR; else process.env.CARGO_TARGET_DIR = prevTarget; }
  const base = srv.base, token = srv.token;

  const browser = await chromium.launch({ headless: true, ...(chromiumPath ? { executablePath: chromiumPath } : {}), args: ['--no-sandbox', '--autoplay-policy=no-user-gesture-required', '--mute-audio', '--font-render-hinting=none'] });
  const ctx = await browser.newContext({ viewport: { width: w, height: h }, locale: 'ko-KR', timezoneId: 'Asia/Seoul', colorScheme: theme, reducedMotion: 'reduce', deviceScaleFactor: 1 });
  await ctx.addInitScript((t) => { try { localStorage.setItem('ft-theme', t); } catch { /* ignore */ } }, theme);
  await ctx.addInitScript(STABLE_INIT, STABLE_CSS);
  const page = await ctx.newPage();
  const consoleErrors = [];
  page.on('pageerror', (e) => consoleErrors.push('pageerror: ' + e.message));
  page.on('console', (m) => { if (m.type() === 'error') consoleErrors.push('console: ' + m.text()); });

  const shots = [];
  const notes = {};
  // Run-specific text (temp dir suffix, case id) would shift layouts between
  // runs; replace it with same-length constants right before the screenshot.
  const MASK = () => {
    const re = [[/ft-shots-[A-Za-z0-9]{6}/g, 'ft-shots-AAAAAA'], [/FT-\d{10}-[0-9a-f]{32}/g, 'FT-1700000000-00000000000000000000000000000000'], [/case-\d{10,}/g, 'case-1700000000']];
    const walk = (root) => {
      const w = document.createTreeWalker(root, NodeFilter.SHOW_TEXT);
      for (let n = w.nextNode(); n; n = w.nextNode()) {
        let v = n.nodeValue, o = v;
        for (const [r, s] of re) v = v.replace(r, s);
        if (v !== o) n.nodeValue = v;
      }
    };
    walk(document.body);
  };
  async function shot(name, desc, target = page) {
    await settle(target);
    await target.evaluate(MASK).catch(() => {});
    for (const f of typeof target.frames === 'function' ? target.frames() : []) if (f !== target.mainFrame()) await f.evaluate(MASK).catch(() => {});
    const file = join(outSub, name + '.png');
    await target.screenshot({ path: file, fullPage: false });
    shots.push({ name, desc, bytes: statSync(file).size });
    console.log(`[shot ${sub}] ${name}`);
  }
  const status = async () => srv.getJson('/api/status');
  async function waitPhase(want, ms) {
    const t0 = Date.now();
    while (Date.now() - t0 < ms) { const s = await status(); if (want.includes(s.phase)) return s; if (s.phase === 'error' || s.phase === 'failed') throw new Error('pipeline ' + s.phase); await sleep(250); }
    throw new Error('timeout waiting for ' + want);
  }
  // Grid rows fully visible (distinct card tops inside the scroll container).
  const visibleRows = (t) => t.evaluate(() => {
    const cards = [...document.querySelectorAll('.card[data-id]')];
    const tops = new Set();
    for (const c of cards) {
      const r = c.getBoundingClientRect();
      let clipBottom = innerHeight;
      for (let p = c.parentElement; p && p !== document.body; p = p.parentElement) {
        const o = getComputedStyle(p).overflowY;
        if (o === 'auto' || o === 'scroll') clipBottom = Math.min(clipBottom, p.getBoundingClientRect().bottom);
      }
      if (r.top >= 0 && r.bottom <= clipBottom + 1) tops.add(Math.round(r.top));
    }
    return { visible: tops.size, cards: cards.length };
  }).catch(() => null);

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
    // UX-11 preflight panel: let it settle so the shot shows the result.
    await page.waitForFunction(() => { const p = document.getElementById('preflight'); return !p || p.hidden || !/사전 점검 중/.test(p.textContent || ''); }, null, { timeout: 6000 }).catch(() => {});
    await shot('01b-start-filled', '입력 단계 — 데모 소스와 케이스 폴더를 채운 상태 (Input stage, filled)');
    await page.click('#btnStart');
    await page.waitForSelector('#progCard.on', { timeout: 15000 }).catch(() => {});
    await page.waitForFunction(() => document.querySelectorAll('#track li').length > 0 && (document.getElementById('log')?.textContent || '').trim().length > 0, null, { timeout: 8000 }).catch(() => {});
    await shot('02-scan-running', '분석 진행 단계 — 단계 표시와 실시간 로그 (Progress stage with live log)');
    await waitPhase(['review-ready', 'done'], 180000);
    await page.waitForSelector('#s3:not([disabled])', { timeout: 20000 });
    await page.click('#s3');
    await page.waitForSelector('#reviewFrame', { state: 'attached' });
    const frame = page.frameLocator('#reviewFrame');
    await frame.locator('.card[data-id]').first().waitFor({ timeout: 30000 });
    await sleep(1200);
    const rf = page.frames().find((f) => f !== page.mainFrame());
    notes.rows = rf ? await visibleRows(rf) : null;
    const rowsTxt = notes.rows ? notes.rows.visible : '?';
    await shot('03-review-embedded', `검토 단계 — 셸 안에 임베드된 증거 뷰어; 완전히 보이는 그리드 행 ${rowsTxt}개 (Review stage, viewer embedded in the shell; fully visible grid rows: ${rowsTxt})`);
    // UX-06: Recover runs as a background job with a visible panel.
    if (await page.locator('#btnRecoverDeleted').isVisible().catch(() => false)) {
      await page.click('#btnRecoverDeleted');
      await page.waitForFunction(() => /\b(ok|failed|cancelled|deferred)\b/.test(document.getElementById('jobPanel')?.className || ''), null, { timeout: 20000 }).catch(() => {});
      await shot('03b-review-job', '검토 단계 — 삭제 영상 복구 작업 표시줄 (데모 케이스: 조사된 이미지 없음 → 실패 표시) (Background recover job panel)');
      if (await page.locator('#btnJobClose').isVisible().catch(() => false)) await page.click('#btnJobClose');
    }

    // Standalone viewer (what examiners open full-screen / in a new window).
    const vpage = await ctx.newPage();
    await vpage.goto(base + '/review/evidence-viewer.html', { waitUntil: 'load' });
    await vpage.locator('.card[data-id]').first().waitFor({ timeout: 30000 });
    await sleep(1200);
    await shot('04-viewer-grid', '증거 뷰어 — 레코드 그리드와 필터 (Viewer grid + filters)', vpage);
    await vpage.locator('.card[data-id]').first().click();
    await sleep(900);
    await shot('05-viewer-detail', '증거 뷰어 — 선택한 영상의 상세/플레이어 패널 (Viewer detail + player)', vpage);
    // Details panel scrolled so the forensic inspector heading is at the top.
    await vpage.evaluate(() => {
      const t = [...document.querySelectorAll('.mini-info .panel-title')].find((e) => /포렌식 인스펙터/.test(e.textContent));
      if (t) {
        t.scrollIntoView({ block: 'start', inline: 'nearest' });
        // scrollIntoView also scrolls overflowing ancestors sideways; undo that.
        for (let p = t; p; p = p.parentElement) if (p.scrollLeft) p.scrollLeft = 0;
        document.scrollingElement.scrollLeft = 0;
      }
    }).catch(() => {});
    await sleep(200);
    await shot('05b-viewer-inspector', '증거 뷰어 — 상세 패널을 포렌식 인스펙터까지 스크롤 (Details panel scrolled to the forensic inspector)', vpage);
    await vpage.evaluate(() => { const b = document.querySelector('.mini-info'); if (b) b.scrollTop = 0; window.scrollTo(0, 0); document.querySelectorAll('*').forEach((e) => { if (e.scrollLeft) e.scrollLeft = 0; }); });
    // Shortcuts modal.
    await vpage.click('#btnShortcuts').catch(() => {});
    await vpage.waitForSelector('#shortcutsModal:not([hidden])', { timeout: 3000 }).catch(() => {});
    await shot('05c-shortcuts', '증거 뷰어 — 단축키 대화상자 (Shortcuts modal)', vpage);
    await vpage.click('#btnShortcutsClose').catch(() => {});
    // Clip export dialog: needs an IN/OUT range. The range is a browser-only
    // unsaved change that would block finalize, so use a throwaway context.
    // Headless Chromium cannot decode the demo H.264 (currentTime stays 0), so
    // shim currentTime on the player.
    const ctx2 = await browser.newContext({ storageState: await ctx.storageState(), viewport: { width: w, height: h }, locale: 'ko-KR', timezoneId: 'Asia/Seoul', colorScheme: theme, reducedMotion: 'reduce', deviceScaleFactor: 1 });
    await ctx2.addInitScript((t) => { try { localStorage.setItem('ft-theme', t); } catch { /* ignore */ } }, theme);
    await ctx2.addInitScript(STABLE_INIT, STABLE_CSS);
    const cpage = await ctx2.newPage();
    await cpage.goto(base + '/review/evidence-viewer.html?token=' + token, { waitUntil: 'load' });
    await cpage.locator('.card[data-id]').first().click({ timeout: 30000 });
    await sleep(900);
    await cpage.evaluate(() => {
      const v = document.querySelector('video');
      if (v) { let ct = 1; Object.defineProperty(v, 'currentTime', { configurable: true, get: () => ct, set: (x) => { ct = x; } }); window.__ftCt = (x) => { ct = x; }; }
    });
    await cpage.click('#btnSetIn').catch(() => {});
    await cpage.evaluate(() => window.__ftCt && window.__ftCt(4));
    await cpage.click('#btnSetOut').catch(() => {});
    // Narrow media column: the clip export lives in the 더보기 popover.
    await cpage.evaluate(() => { const b = document.getElementById('btnMoreTools'); const l = document.getElementById('moreToolsList'); if (b && b.offsetParent && l && l.hidden) b.click(); });
    await cpage.click('#btnExportClip').catch(() => {});
    const clipOpen = await cpage.waitForSelector('.modal.dlg', { timeout: 2500 }).catch(() => null);
    // Focusing the dialog input scrolls the overflowing page sideways by a
    // run-dependent amount; pin it back to 0 for a stable shot.
    await cpage.evaluate(() => document.querySelectorAll('*').forEach((e) => { if (e.scrollLeft) e.scrollLeft = 0; })).catch(() => {});
    await shot('05d-clip-dialog', clipOpen ? '증거 뷰어 — 구간 내보내기 대화상자 (Clip export dialog)' : '증거 뷰어 — 구간 내보내기 시도 (영상 디코드 불가로 범위 없음 → 안내 토스트) (Clip export attempt; no usable range → toast)', cpage);
    await ctx2.close();
    const second = vpage.locator('.card[data-id]').nth(1);
    if (await second.count()) {
      await second.click({ modifiers: ['Control'] }).catch(() => {});
      await sleep(500);
      await shot('06-viewer-multiselect', '증거 뷰어 — 다중 선택 상태 (Viewer multi-select)', vpage);
    }
    // Toast over the selection/action bar (frozen so it cannot expire mid-shot).
    await vpage.evaluate(() => document.getElementById('toastHost')?.replaceChildren());
    // A stateless toast: marking would leave unsaved changes that block finalize.
    await vpage.evaluate(() => {
      const msg = '캡처 확인용 토스트 — 이 알림은 곧 사라집니다';
      try { toast(msg); } catch { const d = document.createElement('div'); d.className = 'toast'; d.textContent = msg; document.getElementById('toastHost').appendChild(d); }
    });
    await sleep(150);
    await vpage.evaluate(() => { const host = document.getElementById('toastHost'); host.replaceWith(host.cloneNode(true)); });
    await shot('06b-toast-actionbar', '증거 뷰어 — 하단 작업 막대 위에 표시된 토스트 (Toast visible over the action bar)', vpage);
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
    console.error(`capture failed (${sub}): ` + e.message);
    try { await shot('zz-failure', '캡처 실패 시점의 화면 (State at failure)'); } catch { /* ignore */ }
    process.exitCode = 1;
  }
  await browser.close();
  srv.stop();
  if (!flag('keep')) rmSync(work, { recursive: true, force: true });
  return { shots, notes, consoleErrors };
}

mkdirSync(outDir, { recursive: true });
const results = [];
for (const { w, h } of sizes) {
  for (const theme of themes) {
    const sub = `${w}x${h}-${theme}`;
    results.push({ sub, ...(await runCombo({ w, h, theme }, sub)) });
  }
}

// ---- contact page -----------------------------------------------------------
const index = ['# UI screenshot index', '', 'Generated by `node scripts/capture-screens.mjs` (ko-KR, Asia/Seoul, synthetic dashcam demo case, reduced motion). Layout: `<WxH>-<theme>/<name>.png`. See [../SCREENSHOTS.md](../SCREENSHOTS.md).', ''];
for (const r of results) {
  index.push(`## ${r.sub}`, '');
  if (r.notes.rows) index.push(`Stage-3 grid: ${r.notes.rows.visible} row(s) fully visible, ${r.notes.rows.cards} cards total.`, '');
  index.push('| Preview | File | What it shows |', '|---|---|---|');
  for (const s of r.shots) index.push(`| ![${s.name}](${r.sub}/${s.name}.png) | \`${s.name}.png\` | ${s.desc} |`);
  index.push('');
}
writeFileSync(join(outDir, 'INDEX.md'), index.join('\n'));
const allShots = results.flatMap((r) => r.shots.map((s) => ({ ...s, name: r.sub + '/' + s.name })));
const blank = allShots.filter((s) => s.bytes < 8000);
console.log(`${allShots.length} screenshots in ${outDir}` + (blank.length ? `; SUSPECT BLANK: ${blank.map((s) => s.name).join(', ')}` : ''));
const errs = [...new Set(results.flatMap((r) => r.consoleErrors))];
if (errs.length) console.log('page errors:\n  ' + errs.slice(0, 10).join('\n  '));
if (blank.length) process.exitCode = 1;
process.exit(process.exitCode || 0);
