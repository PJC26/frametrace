// Regression check for the evidence viewer's undo journal, grid Space key
// and queued "Apply to case". Builds a standalone viewer page from
// assets/ (same bundling as build.rs) with synthetic records and drives it
// in headless Chromium via Playwright.
//
//   node scripts/verify-viewer-undo.mjs
//
// Needs the `playwright` package resolvable (global install is fine via
// NODE_PATH) and a Chromium build (PLAYWRIGHT_BROWSERS_PATH).
import { readFileSync, readdirSync, writeFileSync, mkdtempSync, rmSync } from 'node:fs';
import { join, dirname } from 'node:path';
import { tmpdir } from 'node:os';
import { fileURLToPath } from 'node:url';
import { createServer } from 'node:http';
import { createRequire } from 'node:module';
import { viewerPageHtml } from './lib/app-bundle.mjs';

const root = join(dirname(fileURLToPath(import.meta.url)), '..');
const require = createRequire(import.meta.url);
const { chromium } = require('playwright');

function buildViewer() {
  const dir = join(root, 'assets/viewer');
  let js = '';
  for (const part of readdirSync(dir).filter((f) => f.endsWith('.js')).sort()) {
    js += readFileSync(join(dir, part), 'utf8');
    if (!js.endsWith('\n')) js += '\n';
  }
  const videos = ['a', 'b', 'c'].map((n) => ({
    id: `vid_${n}`, relative_path: `DCIM/${n}.mp4`, source_path: `/case/DCIM/${n}.mp4`,
    ffprobe_ok: true, size_bytes: 1000, extension: 'mp4',
  }));
  const data = `window.__FRAMETRACE_DATA__ = ${JSON.stringify({
    manifest: { case_id: 'FT-UNDO', title: 'undo' }, caseDir: '/case', scan: { videos },
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
let imports = 0;
let releaseFirst = null;
const server = createServer((req, res) => {
  if (req.url === '/api/import-marks') {
    imports += 1;
    const reply = () => { res.setHeader('Content-Type', 'application/json'); res.end('{"ok":true}'); };
    if (imports === 1) releaseFirst = reply; else reply();
    return;
  }
  res.setHeader('Content-Type', 'text/html; charset=utf-8');
  res.end(html);
});
await new Promise((r) => server.listen(0, '127.0.0.1', r));
const base = `http://127.0.0.1:${server.address().port}/`;
const tmp = mkdtempSync(join(tmpdir(), 'ft-undo-'));
const browser = await chromium.launch();
try {
  const page = await browser.newPage();
  const errors = [];
  page.on('pageerror', (e) => errors.push(String(e)));
  await page.goto(base);
  await page.waitForSelector('.card');
  await page.evaluate(() => { try { localStorage.clear(); } catch {} });
  await page.reload();
  await page.waitForSelector('.card');

  // (1) undo restores only the fields the journaled action changed.
  await page.locator('.card').first().click();
  const id = await page.locator('.card').first().getAttribute('data-id');
  await page.locator('body').focus();
  await page.evaluate(() => document.activeElement.blur());
  await page.keyboard.press('1');
  await page.keyboard.press('k');
  await page.locator('#evidenceNote').fill('keep this note');
  await page.locator('#evidenceNote').evaluate((el) => el.blur());
  const tagName = await page.locator('.tag-btn').first().getAttribute('data-preset-tag');
  await page.locator('.tag-btn').first().click();
  await page.evaluate(() => document.activeElement?.blur());
  // Persisted state lives in the per-case drafts (ft.viewer.<case>.annotations.v1,
  // {mark, note, tags, ...} per record): hydration replays them on reload.
  // They are written by a debounced flush, so poll until the flush landed.
  const draftOf = async (rid, want) => {
    await page.waitForFunction(([r, w]) => {
      const key = Object.keys(localStorage).find((k) => k.endsWith('.annotations.v1'));
      const d = key ? (JSON.parse(localStorage.getItem(key) || '{}') || {})[r] : null;
      return !!d && (w === 'nomark' ? !d.mark : d.mark?.status === w);
    }, [rid, want], { timeout: 5000 }).catch(() => {});
    return page.evaluate((r) => {
      const key = Object.keys(localStorage).find((k) => k.endsWith('.annotations.v1'));
      const d = (key ? JSON.parse(localStorage.getItem(key) || '{}') : {})[r] || {};
      return { note: d.note, tags: d.tags, mark: d.mark, noteBox: document.getElementById('evidenceNote')?.value };
    }, rid);
  };
  await page.keyboard.press('Control+z');
  const afterUndo = await draftOf(id, 'nomark');
  check('undo removes the mark', !afterUndo.mark, JSON.stringify(afterUndo));
  check('undo keeps the later note', afterUndo.note === 'keep this note' && afterUndo.noteBox === 'keep this note');
  check('undo keeps the later tag', Array.isArray(afterUndo.tags) && afterUndo.tags.includes(tagName));
  await page.keyboard.press('Control+y');
  const afterRedo = await draftOf(id, 'reviewed');
  check('redo restores the mark and keeps note/tag',
    afterRedo.mark?.status === 'reviewed' && afterRedo.note === 'keep this note' && afterRedo.tags?.includes(tagName),
    JSON.stringify(afterRedo));

  // (2) Space on a focused grid card does not toggle selection.
  const card = page.locator('.card').nth(1);
  await card.focus();
  await page.keyboard.press(' ');
  const selectedAfterSpace = await card.locator("input[type='checkbox']").isChecked().catch(() => false);
  check('Space on a grid card does not toggle selection', !selectedAfterSpace);
  check('no page errors', !errors.length, errors.join(' | '));
  await page.close();

  // (3) "Apply to case" while a save is in flight runs a follow-up push.
  const page2 = await browser.newPage();
  await page2.goto(base);
  await page2.waitForSelector('.card');
  imports = 0;
  await runApplyCheck(page2);
} finally {
  await browser.close();
  server.close();
  rmSync(tmp, { recursive: true, force: true });
}
if (results.some((ok) => !ok) || !results.length) process.exit(1);

async function runApplyCheck(page) {
  await page.locator('.card').first().click();
  await page.evaluate(() => document.activeElement?.blur());
  await page.keyboard.press('2');
  // Click the save indicator to start a save right away (held by the server).
  await page.locator('#saveState').click();
  await page.waitForFunction(() => document.getElementById('saveState').dataset.state === 'saving');
  // An edit made while the save is in flight is not in that request; the
  // queued "Apply to case" must push it (save protocol v2 sends dirty ids only).
  await page.keyboard.press('j');
  await page.keyboard.press('3');
  await page.evaluate(() => document.getElementById('btnApplyMarks').click());
  const sawQueued = await page.locator('#toastHost').innerText();
  releaseFirst?.();
  await page.waitForFunction(() => document.getElementById('toastHost').innerText.length > 0 && document.getElementById('saveState').dataset.state === 'saved', null, { timeout: 5000 });
  await page.waitForTimeout(300);
  const toasts = await page.locator('#toastHost').innerText();
  check('apply during in-flight save is queued', /progress|저장 중/.test(sawQueued), sawQueued);
  check('queued apply pushes again', imports >= 2, `imports=${imports}`);
  check('queued apply reports applyDone', /applied to the case|케이스에 반영/.test(toasts), toasts);
}
