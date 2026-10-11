// QA-01: syntax-check every JavaScript input the shipped UI is built from.
//  - assets/viewer/*.js  -> concatenated exactly like build.rs (sorted by
//    filename, newline-terminated) and checked as ONE classic script, because
//    the viewer is a single IIFE split across files.
//    (parts are not checked individually: the IIFE spans files).
//  - assets/app/*.js -> the workstation shell, bundled the same way
//    (build.rs app_bundle) and checked as ONE classic script; plus an id
//    check: every literal $("id") / document.getElementById("id") must
//    exist in assets/examiner_app.html or be created by the app's own
//    markup strings (id="..."), so a renamed element cannot silently
//    turn a handler into a TypeError at load.
//  - inline <script> blocks of assets/examiner_app.html and
//    assets/evidence_viewer.html.
//  - every scripts/*.mjs (and scripts/lib/*.mjs) must parse.
// Exits non-zero if an input is missing or has a syntax error.
import { readFileSync, readdirSync, existsSync, writeFileSync, mkdtempSync, rmSync } from 'node:fs';
import { join, dirname } from 'node:path';
import { tmpdir } from 'node:os';
import { fileURLToPath } from 'node:url';
import { spawnSync } from 'node:child_process';
import { appParts, concatParts, appBundleCss, appMarkup } from './lib/app-bundle.mjs';

const root = join(dirname(fileURLToPath(import.meta.url)), '..');
const tmp = mkdtempSync(join(tmpdir(), 'ft-checkjs-'));
let failed = false;
const fail = (msg) => { console.error('FAIL ' + msg); failed = true; };

function nodeCheck(label, source, ext = 'js') {
  const file = join(tmp, label.replace(/[^A-Za-z0-9._-]/g, '_') + '.' + ext);
  writeFileSync(file, source);
  const r = spawnSync(process.execPath, ['--check', file], { encoding: 'utf8' });
  if (r.status !== 0) fail(`${label}\n${r.stderr.replace(new RegExp(file.replace(/[\\^$.*+?()[\]{}|]/g, '\\$&'), 'g'), label)}`);
  else console.log('ok   ' + label);
}

// 1. viewer bundle (same order/format as build.rs)
const viewerDir = join(root, 'assets/viewer');
if (!existsSync(viewerDir)) fail('assets/viewer missing');
else {
  const parts = readdirSync(viewerDir).filter((f) => f.endsWith('.js')).sort();
  if (parts.length === 0) fail('assets/viewer has no .js parts');
  let bundle = '';
  for (const p of parts) {
    bundle += readFileSync(join(viewerDir, p), 'utf8');
    if (!bundle.endsWith('\n')) bundle += '\n';
  }
  nodeCheck(`assets/viewer/*.js (${parts.length} parts bundled)`, bundle);
}

// 1b. workstation shell bundle (assets/app, same order/format as build.rs)
const appDir = join(root, 'assets/app');
if (!existsSync(appDir)) fail('assets/app missing');
else {
  const jsParts = appParts('js', root);
  if (jsParts.length === 0) fail('assets/app has no .js parts');
  const appJs = concatParts(jsParts);
  nodeCheck(`assets/app/*.js (${jsParts.length} parts bundled)`, appJs);
  if (/<\/script/i.test(appJs)) fail('assets/app/*.js contains a literal </script');
  const css = appBundleCss(root);
  const open = (css.match(/{/g) || []).length, close = (css.match(/}/g) || []).length;
  if (open !== close) fail(`assets/app/*.css unbalanced braces (${open} open / ${close} close)`);
  else if (!css.trimStart().startsWith(':root')) fail('assets/app/*.css must start with the :root token block');
  else console.log('ok   assets/app/*.css');
  const markup = appMarkup(root);
  for (const marker of ['<!-- @bundle:app.css -->', '<!-- @bundle:app.js -->']) {
    if (markup.split(marker).length !== 2) fail(`assets/examiner_app.html must contain exactly one ${marker}`);
  }
  // id check
  const known = new Set();
  for (const m of markup.matchAll(/\bid\s*=\s*"([^"]+)"/g)) known.add(m[1]);
  for (const m of appJs.matchAll(/\bid\s*=\s*\\?"([A-Za-z][\w-]*)\\?"/g)) known.add(m[1]);
  for (const m of appJs.matchAll(/\.id\s*=\s*"([^"]+)"/g)) known.add(m[1]);
  const missing = new Map();
  for (const f of jsParts) {
    const src = readFileSync(f, 'utf8');
    for (const m of src.matchAll(/(?<![\w.])\$\(\s*"([^"]+)"\s*\)|\bdocument\.getElementById\(\s*"([^"]+)"\s*\)/g)) {
      const id = m[1] || m[2];
      if (!known.has(id)) missing.set(id, (missing.get(id) || []).concat(f.slice(root.length + 1)));
    }
  }
  if (missing.size) fail('assets/app ids referenced but not in the markup:\n' + [...missing].map(([id, fs]) => `  ${id}  (${[...new Set(fs)].join(', ')})`).join('\n'));
  else console.log(`ok   assets/app element ids (${known.size} known)`);
}

// 2. inline classic scripts of the HTML shells
for (const rel of ['assets/examiner_app.html', 'assets/evidence_viewer.html']) {
  const file = join(root, rel);
  if (!existsSync(file)) { fail(`${rel} missing`); continue; }
  const html = readFileSync(file, 'utf8');
  const re = /<script(?![^>]*\bsrc=)([^>]*)>([\s\S]*?)<\/script>/gi;
  let m, n = 0;
  while ((m = re.exec(html))) {
    const attrs = m[1];
    const body = m[2];
    if (!body.trim()) continue;
    if (/type\s*=\s*["']?(?!module|text\/javascript|application\/javascript)[^"'\s>]+/i.test(attrs) && !/module/i.test(attrs)) continue; // json/template blobs
    n += 1;
    nodeCheck(`${rel} inline#${n}`, body, /module/i.test(attrs) ? 'mjs' : 'js');
  }
  // examiner_app.html keeps only the pre-paint theme snippet inline; the
  // app script itself lives in assets/app (checked above).
  if (n === 0 && rel.endsWith('examiner_app.html')) fail(`${rel}: theme pre-paint inline script missing`);
}

// 3. every scripts/*.mjs (and scripts/lib/*.mjs) must parse
for (const sub of ['scripts', 'scripts/lib']) {
  if (!existsSync(join(root, sub))) continue;
  for (const f of readdirSync(join(root, sub)).filter((x) => x.endsWith('.mjs')).sort()) {
    const r = spawnSync(process.execPath, ['--check', join(root, sub, f)], { encoding: 'utf8' });
    if (r.status !== 0) fail(`${sub}/${f}\n${r.stderr}`); else console.log(`ok   ${sub}/${f}`);
  }
}

rmSync(tmp, { recursive: true, force: true });
process.exit(failed ? 1 : 0);
