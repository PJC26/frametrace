// SEC-03 regression check: the pop-out player window is written with
// document.write; a hostile case_id must not break out of the inline script.
//
//   node scripts/verify-player-popup-escape.mjs   (needs playwright via NODE_PATH)
import { readFileSync, readdirSync } from 'node:fs';
import { join, dirname } from 'node:path';
import { fileURLToPath } from 'node:url';
import { createServer } from 'node:http';
import { createRequire } from 'node:module';

const root = join(dirname(fileURLToPath(import.meta.url)), '..');
const require = createRequire(import.meta.url);
const { chromium } = require('playwright');

const HOSTILE = '</script><img src=x onerror="window.__pwned=1;alert(1)">\u2028\u2029';

function buildViewer() {
  const dir = join(root, 'assets/viewer');
  let js = '';
  for (const part of readdirSync(dir).filter((f) => f.endsWith('.js')).sort()) {
    js += readFileSync(join(dir, part), 'utf8');
    if (!js.endsWith('\n')) js += '\n';
  }
  const videos = ['a', 'b'].map((n) => ({
    id: `vid_${n}`, relative_path: `DCIM/${n}.mp4`, source_path: `/case/DCIM/${n}.mp4`,
    ffprobe_ok: true, size_bytes: 1000, extension: 'mp4',
  }));
  const payload = JSON.stringify({
    manifest: { case_id: HOSTILE, title: 'escape' }, caseDir: '/case', scan: { videos },
    carveLog: [], filesystemLog: [], validationLog: [], anomalyLog: [], flsEntries: [],
    thumbs: {}, annotations: {}, deepfake: {}, telemetry: {}, proxies: [],
  }).replace(/</g, '\\u003c');
  const data = `window.__FRAMETRACE_DATA__ = ${payload};`;
  return readFileSync(join(root, 'assets/evidence_viewer.html'), 'utf8')
    .replace('__CSS__', () => readFileSync(join(root, 'assets/evidence_viewer.css'), 'utf8'))
    .replace('__DATA__', () => data)
    .replace('__JS__', () => js);
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
  const ctx = await browser.newContext();
  const page = await ctx.newPage();
  const errors = [];
  const dialogs = [];
  ctx.on('page', (p) => { p.on('dialog', (d) => { dialogs.push(d.message()); d.dismiss(); }); p.on('pageerror', (e) => errors.push('popup: ' + e)); });
  page.on('pageerror', (e) => errors.push(String(e)));
  page.on('dialog', (d) => { dialogs.push(d.message()); d.dismiss(); });
  await page.goto(base);
  await page.waitForSelector('.card');
  await page.locator('.card').first().click();
  await page.evaluate(() => document.activeElement?.blur());
  const [popup] = await Promise.all([ctx.waitForEvent('page'), page.locator('#btnPopPlayer').click()]);
  await popup.waitForSelector('#list .li', { timeout: 5000 }).catch(() => {});
  check('popup player script ran (list rendered)', await popup.locator('#list .li').count() === 2);
  check('no injected element in the popup', await popup.evaluate(() => !document.querySelector('img[src="x"]')));
  check('injected handler never ran', await popup.evaluate(() => !window.__pwned) && !(await page.evaluate(() => !!window.__pwned)));
  check('no alert dialogs fired', dialogs.length === 0, dialogs.join('|'));
  check('hostile case_id survives verbatim as the channel name', await popup.evaluate(() => window.chan && window.chan.name) === 'ft-player:' + HOSTILE);
  check('no page errors', !errors.length, errors.join(' | '));
} finally {
  await browser.close();
  server.close();
}
const failed = results.filter((r) => !r).length;
console.log(failed ? `\n${failed} check(s) failed` : '\nall checks passed');
process.exit(failed ? 1 : 0);
