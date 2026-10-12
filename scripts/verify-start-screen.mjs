// Empty state of the integrated screen end-to-end against the REAL
// server, in a real browser, with an isolated config dir (never the
// examiner's ~/.config/frametrace) — docs/ui-rebuild-contract.md §6.4/§8.1:
//
//   a. empty state: exactly two blocks (증거 넣기 · 이전 프로젝트),
//      "아직 연 프로젝트가 없습니다", no step rail / breadcrumb / onboarding /
//      뷰어 보기 block in the page at all, 옵션 collapsed, 사용 안내 reachable
//      from the top strip (#hBtnHelp);
//   f. 옵션 disclosure exposes case folder + analysis modes with today's
//      defaults (case folder auto, 전체 (증거용), hash/ffprobe on);
//   e. a nonsense path and a non-evidence file show the inline error and
//      start nothing;
//   d. a small fake .E01 (EVF signature) and a renamed image with the EVF
//      header are detected as "E01 이미지";
//   b. a folder path → chip "폴더", one-line preflight, Enter starts the
//      analysis and the host shows the viewer grid (#hViewerHost);
//   c. back on the empty state (#hProjectBtn → 새 증거 넣기) the case is
//      listed in 이전 프로젝트 with the block's one primary, [이어서 검토],
//      which reopens it into the viewer without a page reload;
//   g. layout: both primary buttons sit inside the viewport, >= 36px
//      tall, labels unclipped, no horizontal scroll, blocks do not overlap —
//      at 1366x768 and 1920x1080, light and dark; keyboard Tab order;
//   d2. starting the fake E01 sends input_kind=e01 and the failure (libewf
//      missing, or the image rejected by libewf) is shown clearly;
//   h. a remembered case whose folder vanished is greyed and can be
//      removed with 목록에서 지우기.
//
//   node scripts/verify-start-screen.mjs [--keep] [--shots DIR]
//
// Needs a built target/debug/frametrace-app (or FRAMETRACE_BIN), ffmpeg
// (demo clips) and Playwright (NODE_PATH).
import { mkdtempSync, mkdirSync, writeFileSync, rmSync, existsSync } from 'node:fs';
import { join, dirname } from 'node:path';
import { tmpdir } from 'node:os';
import { fileURLToPath } from 'node:url';
import { spawnSync } from 'node:child_process';
import { createRequire } from 'node:module';
import { startIsolatedServer } from './lib/isolated-server.mjs';

const root = join(dirname(fileURLToPath(import.meta.url)), '..');
const argv = process.argv.slice(2);
const flag = (name) => argv.includes('--' + name);
const shotsIdx = argv.indexOf('--shots');
const shots = shotsIdx >= 0 ? argv[shotsIdx + 1] : '';

let chromium;
try { ({ chromium } = createRequire(import.meta.url)('playwright')); } catch {
  console.error('playwright not resolvable — set NODE_PATH to a node_modules with playwright');
  process.exit(2);
}

const results = [];
const check = (name, ok, detail = '') => {
  results.push({ name, ok: !!ok });
  console.log(`${ok ? 'PASS' : 'FAIL'}  ${name}${detail ? '  — ' + detail : ''}`);
  return !!ok;
};
const sleep = (ms) => new Promise((r) => setTimeout(r, ms));
async function until(fn, ms, step = 200) {
  const end = Date.now() + ms;
  for (;;) {
    let v;
    try { v = await fn(); } catch { v = null; }
    if (v) return v;
    if (Date.now() > end) return null;
    await sleep(step);
  }
}

// ---- synthetic inputs -------------------------------------------------------
const work = mkdtempSync(join(tmpdir(), 'ft-start-'));
const srcDir = join(work, 'BLACKBOX');
const caseA = join(work, 'cases', 'case-a');
const demo = spawnSync(process.execPath, [join(root, 'scripts/make-demo-case.mjs'), srcDir], { stdio: 'inherit' });
if (demo.status !== 0) { console.error('demo source generation failed'); process.exit(1); }
// A tiny "E01": the EWF v1 signature then zeros — enough for detection; a
// real libewf rejects it (no sections), which is the failure path checked.
const imgDir = join(work, 'images');
mkdirSync(imgDir, { recursive: true });
const evf = Buffer.concat([Buffer.from([0x45, 0x56, 0x46, 0x09, 0x0d, 0x0a, 0xff, 0x00]), Buffer.alloc(64 * 1024)]);
const fakeE01 = join(imgDir, 'disk.E01');
writeFileSync(fakeE01, evf);
const renamedImage = join(imgDir, 'export.img');
writeFileSync(renamedImage, evf);
const notEvidence = join(imgDir, 'notes.txt');
writeFileSync(notEvidence, 'just text\n');

const server = await startIsolatedServer({ root, work });
const { base, token, getJson, postJson } = server;
console.log('server:', base, '(isolated config in', work + ')');
let browser;
const pageErrors = [];
function cleanup() {
  try { browser && browser.close(); } catch { /* closed */ }
  server.stop();
  if (!flag('keep')) { try { rmSync(work, { recursive: true, force: true }); } catch { /* busy */ } }
}
process.on('exit', cleanup);
process.on('SIGINT', () => process.exit(130));

const shot = async (page, name) => {
  if (!shots) return;
  mkdirSync(shots, { recursive: true });
  await page.screenshot({ path: join(shots, name + '.png') });
};

// Geometry of the start screen's key controls, measured in the page.
const layoutProbe = () => {
  const vis = (el) => !!el && el.getClientRects().length > 0 && getComputedStyle(el).visibility !== 'hidden';
  const box = (el) => { const r = el.getBoundingClientRect(); return { top: r.top, bottom: r.bottom, left: r.left, right: r.right, h: r.height, w: r.width }; };
  const recentPrimary = document.querySelector('#blkRecent #recentCaseRow button.primary');
  const primaries = [document.getElementById('btnStart'), recentPrimary];
  const out = { vw: innerWidth, vh: innerHeight, hscroll: document.documentElement.scrollWidth > innerWidth + 1, primaries: [], blocks: [], small: [] };
  for (const el of primaries) {
    if (!el) { out.primaries.push(null); continue; }
    out.primaries.push({ id: el.id || el.textContent.trim(), visible: vis(el), ...box(el), clipped: el.scrollWidth > el.clientWidth + 1 });
  }
  for (const id of ['blkEvidence', 'blkRecent']) {
    const el = document.getElementById(id);
    out.blocks.push({ id, visible: vis(el), ...box(el) });
  }
  // Every visible control in the start view: >= 24px hit area.
  for (const el of document.querySelectorAll('#inputCard button, #inputCard summary, #inputCard input[type=text]')) {
    if (!vis(el)) continue;
    const r = el.getBoundingClientRect();
    if (r.height < 24 || r.width < 24) out.small.push(`${el.id || el.textContent.trim()} ${Math.round(r.width)}x${Math.round(r.height)}`);
  }
  // Exactly one primary per block.
  out.primaryPerBlock = ['blkEvidence', 'blkRecent'].map((id) =>
    [...document.querySelectorAll(`#${id} button.primary`)].filter(vis).length);
  return out;
};
function checkLayout(label, g, expectRecentPrimary) {
  const inside = (b) => b && b.visible && b.top >= 0 && b.left >= 0 && b.bottom <= g.vh + 0.5 && b.right <= g.vw + 0.5;
  const [start, recent] = g.primaries;
  check(`${label}: 분석 시작 inside the viewport, >=36px, label unclipped`, inside(start) && start.h >= 36 && !start.clipped, JSON.stringify(start));
  if (expectRecentPrimary) check(`${label}: [이어서 검토] inside the viewport, >=36px, label unclipped`, inside(recent) && recent.h >= 36 && !recent.clipped && recent.id === '이어서 검토', JSON.stringify(recent));
  check(`${label}: no horizontal scroll`, !g.hscroll);
  const [b1, b2] = g.blocks;
  check(`${label}: two blocks stacked without overlap`, b1.visible && b2.visible && b1.bottom <= b2.top, JSON.stringify(g.blocks.map((b) => [Math.round(b.top), Math.round(b.bottom)])));
  check(`${label}: centered column <= 760px`, Math.abs((b1.left + b1.right) / 2 - g.vw / 2) < 2 && b1.w <= 761, `${Math.round(b1.left)}..${Math.round(b1.right)} of ${g.vw}`);
  check(`${label}: every visible control >= 24px`, g.small.length === 0, g.small.join(', '));
  check(`${label}: exactly one primary per block`, g.primaryPerBlock.every((n) => n === 1), JSON.stringify(g.primaryPerBlock));
}

// Cards of the review grid in the host — the hosted viewer fragment, or
// (until it lands) the legacy viewer frame inside the same mount point.
const viewerCards = (page) => page.evaluate(() => {
  const host = document.getElementById('hViewerHost');
  if (!host || host.hidden) return 0;
  let n = host.querySelectorAll('#recordGrid .card').length;
  const fr = host.querySelector('iframe');
  try { if (!n && fr && !fr.hidden && fr.contentDocument) n = fr.contentDocument.querySelectorAll('#recordGrid .card').length; } catch { /* cross-origin */ }
  return n;
});
// Back to the empty state the way an examiner does: project switcher →
// 새 증거 넣기 (no page reload).
async function goHome(page) {
  if (await page.locator('#inputCard').isVisible()) return;
  await page.click('#hProjectBtn');
  await page.click('#hProjectNew');
  await page.waitForSelector('#inputCard:not([hidden])', { timeout: 10000 });
}

try {
  try { browser = await chromium.launch({ channel: 'chrome' }); } catch { browser = await chromium.launch(); }
  const context = await browser.newContext({ viewport: { width: 1366, height: 768 } });
  const page = await context.newPage();
  page.on('pageerror', (e) => pageErrors.push('shell: ' + e));
  page.on('console', (m) => { if (m.type() === 'error' && !/favicon|Failed to load resource/.test(m.text())) pageErrors.push('console: ' + m.text()); });
  const env = await getJson('/api/env');
  const haveEwf = !!(env.workflows && env.workflows.e01);
  const haveFfprobe = !!(env.tools && env.tools.ffprobe);

  // ---- a. empty state ---------------------------------------------------------
  await page.goto(`${base}/?token=${token}`);
  await page.waitForSelector('#blkEvidence');
  await until(() => page.locator('#recentEmpty').isVisible(), 5000);
  const titles = await page.locator('#inputCard .sblock h3').allTextContents();
  check('a. exactly two blocks: 증거 넣기 · 이전 프로젝트', JSON.stringify(titles) === JSON.stringify(['증거 넣기', '이전 프로젝트']), JSON.stringify(titles));
  check('a. each block has a one-line explanation', (await page.locator('#inputCard .sblock .sbdesc').count()) === 2);
  check('a. empty state says "아직 연 프로젝트가 없습니다"', (await page.locator('#recentEmpty').textContent()).includes('아직 연 프로젝트가 없습니다') && await page.locator('#recentCaseRow').isHidden());
  const removed = await page.evaluate(() => ['stageNav', 'crumbs', 'onboard', 'blkViewer', 'btnViewerOpen', 's1', 'resultCard', 'reviewFrame'].filter((id) => document.getElementById(id)));
  check('a. no step rail, breadcrumb, onboarding or 뷰어 보기 block in the page', removed.length === 0, removed.join(','));
  check('a. empty state: review host hidden, project switcher disabled', await page.locator('#hViewerHost').isHidden() && await page.locator('#hProjectBtn').isDisabled() && (await page.locator('#hProjectName').textContent()).trim() === '프로젝트 없음');
  check('a. 옵션 collapsed by default (case folder hidden)', !(await page.locator('#startOptions').evaluate((d) => d.open)) && await page.locator('#caseDir').isHidden());
  check('a. 사용 안내 still reachable', await page.locator('#hBtnHelp').isVisible());
  await page.click('#hBtnHelp');
  check('a. 사용 안내 opens as a popover from the top strip', await page.locator('#hHelpPop').isVisible() && (await page.getAttribute('#hBtnHelp', 'aria-expanded')) === 'true');
  await page.keyboard.press('Escape');
  check('a. Esc closes it and returns focus to the opener', await page.locator('#hHelpPop').isHidden() && await page.evaluate(() => document.activeElement && document.activeElement.id === 'hBtnHelp'));
  await shot(page, 'start-empty-1366');
  checkLayout('g. empty 1366x768', await page.evaluate(layoutProbe), false);
  // Keyboard: the evidence field has focus; Tab walks block 1 in order.
  await page.focus('#sourcePath');
  const order = [];
  for (let i = 0; i < 4; i++) { await page.keyboard.press('Tab'); order.push(await page.evaluate(() => document.activeElement.id || document.activeElement.tagName)); }
  check('g. Tab order: path → 찾아보기 → 분석 시작 → 옵션', order.slice(0, 3).join(',') === 'btnBrowseSource,btnStart,SUMMARY', order.join(','));

  // ---- f. 옵션 disclosure ---------------------------------------------------
  await page.click('#startOptions > summary');
  const opts = await page.evaluate(() => ({
    caseVisible: document.getElementById('caseDir').getClientRects().length > 0,
    caseValue: document.getElementById('caseDir').value,
    modes: [...document.querySelectorAll("input[name='scanMode']")].map((r) => r.value),
    mode: document.querySelector("input[name='scanMode']:checked")?.value,
    hash: document.getElementById('withHash').checked,
    ffprobe: document.getElementById('withFfprobe').checked,
    deepfake: document.getElementById('withDeepfake').checked,
    summary: document.getElementById('optSummary').textContent,
  }));
  check('f. 옵션 exposes the case folder (empty = auto)', opts.caseVisible && opts.caseValue === '', JSON.stringify(opts));
  check('f. 옵션 exposes all four analysis modes, default 전체 (증거용)', opts.modes.join(',') === 'quick,standard,full,custom' && opts.mode === 'full');
  check('f. defaults preserved: SHA-256 on, ffprobe on (when installed), deepfake off', opts.hash && opts.ffprobe === haveFfprobe && !opts.deepfake);
  check('f. collapsed summary names the defaults', /자동/.test(opts.summary) && /전체/.test(opts.summary), opts.summary);
  await page.click("label:has(input[name='scanMode'][value='quick'])");
  check('f. switching mode updates the summary', /빠른 선별/.test(await page.locator('#optSummary').textContent()));
  await page.click("label:has(input[name='scanMode'][value='full'])");
  await shot(page, 'start-options-open');
  await page.click('#startOptions > summary');

  // ---- e. inline errors -----------------------------------------------------
  await page.fill('#sourcePath', join(work, 'definitely-not-here', 'xyz'));
  const missingMsg = await until(async () => {
    const t = await page.locator('#sourceStatus').textContent();
    return /경로를 찾을 수 없습니다/.test(t) && t;
  }, 5000);
  check('e. nonsense path → inline "경로를 찾을 수 없습니다"', !!missingMsg && await page.locator('#kindChip').isHidden(), missingMsg || '');
  await page.press('#sourcePath', 'Enter');
  const errShown = await until(async () => (await page.locator('#startErr.show').isVisible()) && (await page.locator('#startErr').textContent()), 5000);
  check('e. Enter on a nonsense path shows the error, starts nothing', /경로를 찾을 수 없습니다/.test(errShown || '') && !(await getJson('/api/status')).has_job, errShown || '');
  check('e. preflight line stays hidden for an invalid path', await page.locator('#preflight').isHidden());
  await page.fill('#sourcePath', notEvidence);
  const otherMsg = await until(async () => { const t = await page.locator('#sourceStatus').textContent(); return /지원하지 않는 입력/.test(t) && t; }, 5000);
  check('e. a non-evidence file → says what is accepted (E01 or folder)', !!otherMsg && /E01/.test(otherMsg) && /폴더/.test(otherMsg), otherMsg || '');

  // ---- d. E01 detection -------------------------------------------------------
  await page.fill('#sourcePath', fakeE01);
  const chipE01 = await until(async () => { const t = (await page.locator('#kindChip').textContent()).trim(); return (await page.locator('#kindChip').isVisible()) && t === 'E01 이미지' && t; }, 5000);
  check('d. .E01 path → chip "E01 이미지"', chipE01 === 'E01 이미지', chipE01 || '');
  check('d. E01 flow chosen (input kind e01, E01 options shown)', await page.evaluate(() => document.querySelector("input[name='inputKind']:checked").value === 'e01' && document.getElementById('verifyRow').style.display !== 'none'));
  check('d. missing libewf is explained next to the chip (only when missing)', (await page.locator('#e01Need').isVisible()) === !haveEwf);
  await page.fill('#sourcePath', renamedImage);
  const chipMagic = await until(async () => (await page.locator('#kindChip').isVisible()) && (await page.locator('#kindChip').textContent()).trim() === 'E01 이미지', 5000);
  check('d. a renamed image with the EVF header → "E01 이미지" (server magic check)', !!chipMagic);
  await shot(page, 'start-e01-detected');

  // ---- b. folder → analysis → review -----------------------------------------
  await page.fill('#sourcePath', srcDir);
  const chipFolder = await until(async () => (await page.locator('#kindChip').isVisible()) && (await page.locator('#kindChip').textContent()).trim() === '폴더', 5000);
  check('b. folder path → chip "폴더"', !!chipFolder);
  check('b. folder flow chosen (input kind folder)', await page.evaluate(() => document.querySelector("input[name='inputKind']:checked").value === 'folder'));
  await page.click('#startOptions > summary');
  await page.fill('#caseDir', caseA);
  await page.click('#startOptions > summary');
  const pf = await until(async () => {
    const t = (await page.locator('#preflight').textContent()) || '';
    return (await page.locator('#preflight').isVisible()) && /사전 점검/.test(t) && /크기 [0-9]/.test(t) && t;
  }, 8000);
  const pfHeight = await page.locator('#preflight').evaluate((el) => el.getBoundingClientRect().height);
  check('b. preflight summary appears in one compact line', !!pf && pfHeight <= 40, `${Math.round(pfHeight)}px: ${pf || ''}`);
  await shot(page, 'start-folder-filled');
  const startReq = page.waitForRequest((r) => r.url().endsWith('/api/start') && r.method() === 'POST', { timeout: 15000 });
  await page.press('#sourcePath', 'Enter');
  const reqBody = JSON.parse((await startReq).postData() || '{}');
  check('b. Enter in the path field starts the folder analysis', reqBody.input_kind === 'folder' && reqBody.source_path === srcDir && reqBody.case_dir === caseA, JSON.stringify(reqBody).slice(0, 200));
  const reviewing = await until(async () => {
    if (await page.locator('#dlgBackdrop:not([hidden]) #dlgOk').isVisible().catch(() => false)) await page.click('#dlgOk');
    const st = await getJson('/api/status');
    return st.phase === 'review-ready' && !st.busy ? st : null;
  }, 240000, 500);
  check('b. analysis reaches review-ready', !!reviewing);
  const onReview = await until(() => viewerCards(page), 30000);
  check('b. the shell shows the viewer grid in the host (same screen)', !!onReview && await page.locator('#inputCard').isHidden(), `${onReview || 0} cards`);

  // ---- c. 이전 프로젝트 + [이어서 검토] -------------------------------------
  await goHome(page);
  const listed = await until(async () => {
    if (!(await page.locator('#recentCaseRow li').count())) return null;
    const t = await page.locator('#recentCaseRow li').first().textContent();
    return /검토|결과물/.test(t) && t;
  }, 8000);
  check('c. the analysed case appears in 이전 프로젝트', !!listed && listed.includes('case-a') && /검토|결과물/.test(listed), listed || '');
  check('c. empty-state line is gone', await page.locator('#recentEmpty').isHidden());
  // The browser's own remembered paths (typed spelling) and the server
  // list (canonical spelling) must not show the same case twice.
  const caseRows = await page.locator('#recentCaseRow li', { hasText: 'case-a' }).count();
  check('c. the case is listed once (no duplicate from the browser history)', caseRows === 1, `${caseRows} rows`);
  check('c. empty state again: host hidden, chip hidden', await page.locator('#hViewerHost').isHidden() && await page.locator('#jobPanel').isHidden());
  // While background work still reports busy the row is deliberately a
  // quiet "열기" (no primary); slow runners stay there for a while after
  // returning home, so measure the layout once it has settled.
  const settled = await until(async () => {
    const st = await getJson('/api/status');
    return !st.busy && st.phase !== 'running' && await page.locator('#recentCaseRow button.primary').count() > 0;
  }, 30000, 250);
  check('g. with recent: [이어서 검토] appears once background work settles', !!settled);
  await shot(page, 'start-with-recent-1366');
  checkLayout('g. with recent 1366x768 light', await page.evaluate(layoutProbe), true);
  for (const [w, h] of [[1920, 1080], [1366, 768]]) {
    for (const theme of ['dark', 'light']) {
      await page.setViewportSize({ width: w, height: h });
      await page.evaluate((t) => { document.documentElement.dataset.theme = t; }, theme);
      await sleep(150);
      if (w === 1920) await shot(page, `start-with-recent-${w}-${theme}`);
      checkLayout(`g. with recent ${w}x${h} ${theme}`, await page.evaluate(layoutProbe), true);
    }
  }
  const rowBtn = page.locator('#recentCaseRow li').first().locator('button');
  check('c. top row action is [이어서 검토] (primary lg)', (await rowBtn.textContent()).trim() === '이어서 검토' && /\bprimary\b/.test(await rowBtn.getAttribute('class')) && !/\bprimary\b/.test(await page.getAttribute('#btnOpenOther', 'class')));
  let loads = 0;
  page.on('load', () => { loads += 1; });
  const openReq = page.waitForRequest((r) => r.url().endsWith('/api/open-case'), { timeout: 10000 });
  await rowBtn.click();
  const openBody = JSON.parse((await openReq).postData() || '{}');
  const reopened = await until(() => viewerCards(page), 30000);
  check('c. [이어서 검토] reopens the case into the viewer', !!reopened && String(openBody.case_dir || '').endsWith('case-a'), JSON.stringify(openBody));
  check('c. reopening did not reload the page', loads === 0, `${loads} load events`);
  check('c. the top strip names the open project', (await page.locator('#hProjectName').textContent()).includes('case-a') && await page.locator('#hProjectBtn').isEnabled());
  const recentApi = await getJson('/api/recent-cases');
  check('c. GET /api/recent-cases lists the case (isolated config)', Array.isArray(recentApi) && recentApi.some((r) => r.case_dir.endsWith('case-a') && r.exists === true && r.source_path === srcDir) &&
    existsSync(join(work, 'cfg', 'frametrace', 'recent-cases.json')), JSON.stringify(recentApi).slice(0, 240));

  // ---- d2. E01 start: flow chosen, failure shown clearly --------------------
  await goHome(page);
  await page.fill('#sourcePath', fakeE01);
  await until(async () => (await page.locator('#kindChip').textContent()).trim() === 'E01 이미지', 5000);
  await page.click('#startOptions > summary');
  await page.fill('#caseDir', join(work, 'cases', 'case-e01'));
  await page.click('#startOptions > summary');
  const e01Req = page.waitForRequest((r) => r.url().endsWith('/api/start') && r.method() === 'POST', { timeout: 15000 });
  const e01Resp = page.waitForResponse((r) => r.url().endsWith('/api/start'), { timeout: 15000 });
  await page.click('#btnStart');
  const e01Body = JSON.parse((await e01Req).postData() || '{}');
  const e01Res = await (await e01Resp).json();
  check('d2. starting a .E01 sends input_kind=e01', e01Body.input_kind === 'e01' && e01Body.source_path === fakeE01, JSON.stringify(e01Body).slice(0, 160));
  if (!e01Res.ok) {
    const txt = await until(async () => (await page.locator('#startErr.show').isVisible()) && page.locator('#startErr').textContent(), 5000);
    check('d2. libewf missing → clear error on the start screen', /libewf/.test(txt || ''), txt || '');
  } else {
    const failed = await until(async () => { const st = await getJson('/api/status'); return (st.phase === 'error' || (st.error && !st.busy)) ? st : null; }, 120000, 500);
    check('d2. the E01 pipeline ran (E01 step names)', !!failed && (failed.step_names || []).some((n) => /E01/.test(n)), JSON.stringify(failed && failed.step_names));
    const shown = await until(async () => {
      const failedStep = await page.locator('#track li.failed').count();
      const err = (await page.locator('#startErr').textContent()) || '';
      return failedStep > 0 && err.includes('오류') && err;
    }, 15000);
    check('d2. the failure is shown clearly (failed step + error with a hint)', !!shown, (shown || '').slice(0, 160));
    await shot(page, 'start-e01-failed');
  }

  // ---- h. vanished case folder → greyed, forget -----------------------------
  const gone = join(work, 'cases', 'gone-case');
  mkdirSync(gone, { recursive: true });
  writeFileSync(join(gone, 'case.json'), '{}');
  const opened = await postJson('/api/open-case', { case_dir: gone });
  rmSync(gone, { recursive: true, force: true });
  await page.goto(`${base}/?token=${token}`);
  await page.waitForSelector('#blkRecent');
  await until(async () => (await page.locator('#inputCard').isVisible()) || (await page.locator('#hViewerHost').isVisible()), 8000);
  await goHome(page);
  const missingRow = page.locator('#recentCaseRow li.missing');
  await until(async () => (await missingRow.count()) > 0, 8000);
  check('h. a vanished case is listed greyed with 목록에서 지우기', opened.ok && (await missingRow.count()) === 1 && (await missingRow.locator('button').textContent()).trim() === '목록에서 지우기');
  await missingRow.locator('button').click();
  const goneAfter = await until(async () => (await page.locator('#recentCaseRow li.missing').count()) === 0, 8000);
  const recentAfter = await getJson('/api/recent-cases');
  check('h. 목록에서 지우기 removes it from the list (server + UI)', !!goneAfter && !recentAfter.some((r) => r.case_dir.includes('gone-case')));

  check('no page errors', pageErrors.length === 0, pageErrors.slice(0, 5).join(' | '));
} catch (err) {
  console.error(err);
  check('harness ran to completion', false, String(err && err.message || err));
}

const failed = results.filter((r) => !r.ok);
console.log(`\nverify-start-screen: ${results.length - failed.length}/${results.length} passed`);
process.exit(failed.length ? 1 : 0);
