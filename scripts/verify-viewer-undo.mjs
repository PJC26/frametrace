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

function buildViewer(annotations = {}) {
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
    thumbs: {}, annotations, deepfake: {}, telemetry: {}, proxies: [],
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
let deny401 = false;
const savedMarks = new Map();
const server = createServer((req, res) => {
  if (req.url === '/api/import-marks' && deny401) {
    res.statusCode = 401;
    res.setHeader('Content-Type', 'application/json');
    res.end('{"ok":false,"error":"unauthorized"}');
    return;
  }
  if (req.url === '/api/import-marks') {
    imports += 1;
    // Keep what was saved so the next page load serves it back like the
    // case DB would (the U4 reload check relies on it).
    let body = '';
    req.on('data', (c) => { body += c; });
    req.on('end', () => {
      try {
        const payload = JSON.parse(JSON.parse(body).marks_json);
        for (const m of payload.marks || []) savedMarks.set(m.id, m);
        for (const id of payload.deleted_ids || []) savedMarks.delete(id);
      } catch { /* not a marks body */ }
    });
    const reply = () => { res.setHeader('Content-Type', 'application/json'); res.end('{"ok":true}'); };
    if (imports === 1) releaseFirst = reply; else reply();
    return;
  }
  res.setHeader('Content-Type', 'text/html; charset=utf-8');
  res.end(savedMarks.size ? buildViewer({ marks: [...savedMarks.values()].map((m) => ({ ...m, record_id: m.id })) }) : html);
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
  // Persisted state lives in the per-id drafts (ft.draft.<case>.<writer>.<id>
  // = {mark, note, tags, ..., seq, base}, §10.3): hydration replays them on
  // reload. They are written by a debounced flush, so poll until it landed.
  const draftOf = async (rid, want) => {
    await page.waitForFunction(([r, w]) => {
      const key = `ft.draft.FT-UNDO.${FTViewer.writerToken}.${r}`;
      const d = JSON.parse(localStorage.getItem(key) || 'null');
      return !!d && (w === 'nomark' ? !d.mark : d.mark?.status === w);
    }, [rid, want], { timeout: 5000 }).catch(() => {});
    return page.evaluate((r) => {
      const d = JSON.parse(localStorage.getItem(`ft.draft.FT-UNDO.${FTViewer.writerToken}.${r}`) || '{}') || {};
      return { note: d.note, tags: d.tags, mark: d.mark, base: d.base, noteBox: document.getElementById('evidenceNote')?.value };
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
  check('draft carries its base revision', afterRedo.base === 0, JSON.stringify(afterRedo));
  // §10.3: no whole-map MARKS / NOTES / TAGS store is written any more —
  // the per-id drafts are the only browser copy.
  const wholeMaps = await page.evaluate(() => Object.keys(localStorage).filter((k) => /^ft\.viewer\.FT-UNDO\.(marks|notes|tags|annotations\.v1)$/.test(k)));
  check('no whole-map marks/notes/tags writes', wholeMaps.length === 0, wholeMaps.join(','));

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
  await page2.close();
  await runReloadCheck(browser);
  await runAuthAndOriginCheck(browser);
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

// U4: a reload (standalone: FTViewer.reload() → page reload with the view
// stashed) keeps sort, selection, the active card and the undo journal.
async function runReloadCheck(browser) {
  savedMarks.clear();
  const page = await browser.newPage();
  const errors = [];
  page.on('pageerror', (e) => errors.push(String(e)));
  await page.goto(base);
  await page.waitForSelector('.card');
  await page.evaluate(() => { const el = document.getElementById('sortBy'); el.value = 'name'; el.dispatchEvent(new Event('change', { bubbles: true })); });
  await page.locator('.card[data-id="vid_a"]').click();
  await page.evaluate(() => document.activeElement?.blur());
  await page.keyboard.press('x'); // select vid_a
  await page.keyboard.press('1'); // mark vid_a reviewed, advance to vid_b
  await page.waitForFunction(() => document.getElementById('saveState').dataset.state === 'saved', null, { timeout: 5000 }).catch(() => {});
  const before = await page.evaluate(() => ({
    active: document.querySelector('.card.active')?.dataset.id,
    save: document.getElementById('saveState').dataset.state,
  }));
  const [, result] = await Promise.all([
    page.waitForEvent('load', { timeout: 15000 }),
    page.evaluate(() => FTViewer.reload({ reason: 'test' })),
  ]);
  void result;
  await page.waitForSelector('.card');
  const after = await page.evaluate(() => ({
    active: document.querySelector('.card.active')?.dataset.id,
    sort: document.getElementById('sortBy').value,
    selected: !!document.querySelector('.card[data-id="vid_a"] input[type="checkbox"]')?.checked,
    marked: !!document.querySelector('.card[data-id="vid_a"] .mark-chip'),
  }));
  check('reload ran with nothing unsaved', before.save === 'saved', before.save);
  check('reload keeps the active card', after.active === before.active && !!after.active, `${before.active} -> ${after.active}`);
  check('reload keeps the sort order and the selection', after.sort === 'name' && after.selected, JSON.stringify(after));
  await page.evaluate(() => document.activeElement?.blur());
  await page.keyboard.press('Control+z');
  await page.waitForTimeout(200);
  const undone = await page.evaluate(() => ({
    marked: !!document.querySelector('.card[data-id="vid_a"] .mark-chip'),
    toast: document.getElementById('toastHost').textContent,
  }));
  check('undo journal survives the reload (Ctrl+Z removes the mark)', after.marked && !undone.marked && /실행 취소|Undone/.test(undone.toast), JSON.stringify(undone));
  check('no page errors (reload)', !errors.length, errors.join(' | '));
  await page.close();
}

// U8: a save refused with 401 says "authorization expired — reopen", not
// "server unreachable". U9: a draft marker of this case under another port
// (cookie ft_draft_ports, shared by every port of the host) is announced in
// the notices row; this window's own drafts set its marker.
async function runAuthAndOriginCheck(browser) {
  const fnv = (text) => { let h = 0x811c9dc5; for (let i = 0; i < text.length; i++) { h ^= text.charCodeAt(i); h = Math.imul(h, 0x01000193) >>> 0; } return h.toString(16).padStart(8, '0'); };
  const hash = fnv('FT-UNDO|/case');
  const myPort = String(server.address().port);
  const otherPort = myPort === '8478' ? '8479' : '8478';
  const ctx = await browser.newContext();
  await ctx.addCookies([{ name: 'ft_draft_ports', value: `${otherPort}.${hash}-9999.00000000`, url: base }]);
  const page = await ctx.newPage();
  const errors = [];
  page.on('pageerror', (e) => errors.push(String(e)));
  await page.goto(base);
  await page.waitForSelector('.card');
  const notice = await page.evaluate(() => document.getElementById('originDraftsNotice')?.textContent || '');
  check('drafts under another port are announced (notices row)', notice.includes(`127.0.0.1:${otherPort}`) && /남아 있을 수|may remain/.test(notice), notice);
  check('a marker of another case is not announced', !notice.includes(':9999'), notice);

  deny401 = true;
  await page.locator('.card').first().click();
  await page.evaluate(() => document.activeElement?.blur());
  await page.keyboard.press('2');
  await page.locator('#saveState').click();
  await page.waitForFunction(() => /인증이 만료|authorization expired/.test(document.getElementById('toastHost').textContent), null, { timeout: 8000 }).catch(() => {});
  const auth = await page.evaluate(() => ({
    toast: document.getElementById('toastHost').textContent,
    title: document.getElementById('saveState').title,
    state: document.getElementById('saveState').dataset.state,
  }));
  check('401 save: "browser authorization expired — reopen" (not server down)', /브라우저 인증이 만료되었습니다 — 워크스테이션 창에서 다시 여십시오/.test(auth.toast) && !/연결할 수 없/.test(auth.toast), JSON.stringify(auth));
  check('401 save: save state says why and stays unsaved', /인증이 만료/.test(auth.title) && auth.state !== 'saved', JSON.stringify(auth));
  await page.waitForFunction((p) => document.cookie.includes(`${p}.`), myPort, { timeout: 5000 }).catch(() => {});
  const cookie = await page.evaluate(() => document.cookie);
  check('own unsaved drafts set this port\'s marker', cookie.includes(`${myPort}.${hash}`), cookie);
  await page.locator('#originDraftsDismiss').click();
  const afterDismiss = await page.evaluate(() => ({ cookie: document.cookie, notice: !!document.getElementById('originDraftsNotice') }));
  check('dismiss removes the notice and the other port\'s marker', !afterDismiss.notice && !afterDismiss.cookie.includes(`${otherPort}.${hash}`), JSON.stringify(afterDismiss));
  deny401 = false;
  await page.locator('#saveState').click();
  await page.waitForFunction(() => document.getElementById('saveState').dataset.state === 'saved', null, { timeout: 10000 }).catch(() => {});
  await page.waitForFunction((p) => !document.cookie.includes(`${p}.`), myPort, { timeout: 5000 }).catch(() => {});
  check('marker cleared once every draft of the case is saved', !(await page.evaluate(() => document.cookie)).includes(`${myPort}.${hash}`), await page.evaluate(() => document.cookie));
  check('no page errors (auth/origin)', !errors.length, errors.join(' | '));
  await ctx.close();
}
