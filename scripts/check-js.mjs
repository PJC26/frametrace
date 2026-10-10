// QA-01: syntax-check every JavaScript input the shipped UI is built from.
//  - assets/viewer/*.js  -> concatenated exactly like build.rs (sorted by
//    filename, newline-terminated) and checked as ONE classic script, because
//    the viewer is a single IIFE split across files.
//    (parts are not checked individually: the IIFE spans files).
//  - inline <script> blocks of assets/examiner_app.html and
//    assets/evidence_viewer.html.
//  - every scripts/*.mjs must parse.
// Exits non-zero if an input is missing or has a syntax error.
import { readFileSync, readdirSync, existsSync, writeFileSync, mkdtempSync, rmSync } from 'node:fs';
import { join, dirname } from 'node:path';
import { tmpdir } from 'node:os';
import { fileURLToPath } from 'node:url';
import { spawnSync } from 'node:child_process';

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
  if (n === 0 && rel.endsWith('examiner_app.html')) fail(`${rel}: no inline script found`);
}

// 3. every scripts/*.mjs must parse
for (const f of readdirSync(join(root, 'scripts')).filter((x) => x.endsWith('.mjs')).sort()) {
  const r = spawnSync(process.execPath, ['--check', join(root, 'scripts', f)], { encoding: 'utf8' });
  if (r.status !== 0) fail(`scripts/${f}\n${r.stderr}`); else console.log('ok   scripts/' + f);
}

rmSync(tmp, { recursive: true, force: true });
process.exit(failed ? 1 : 0);
