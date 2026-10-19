// Offline test of scripts/make-portable.ps1 (docs/PLAN_TOOLS_C_2026-10-20.md
// P5): a fake repo root with fake binaries, no build, no network. FFmpeg files
// are planted on purpose in tools/bin; the package must leave them out (with
// a "never packaged" warning) while still carrying the other tools, and the
// zip must use forward-slash entry names with a one-line LF SHA256SUMS.
// Windows only (needs Windows PowerShell).
//
//   node scripts/test-make-portable.mjs
import { spawn } from 'node:child_process';
import { createHash } from 'node:crypto';
import {
  mkdtempSync, mkdirSync, writeFileSync, readFileSync, copyFileSync, existsSync, rmSync,
} from 'node:fs';
import { tmpdir } from 'node:os';
import { join, dirname } from 'node:path';
import { fileURLToPath } from 'node:url';

if (process.platform !== 'win32') {
  console.log('test-make-portable: SKIPPED (Windows only)');
  process.exit(0);
}

const repo = join(dirname(fileURLToPath(import.meta.url)), '..');
const sha256 = (bytes) => createHash('sha256').update(bytes).digest('hex');

// Entry names from a zip's central directory (any zip, deflated or not).
function zipNames(buf) {
  let eocd = buf.length - 22;
  while (eocd >= 0 && buf.readUInt32LE(eocd) !== 0x06054b50) eocd--;
  if (eocd < 0) throw new Error('no end of central directory');
  const count = buf.readUInt16LE(eocd + 10);
  let at = buf.readUInt32LE(eocd + 16);
  const names = [];
  for (let i = 0; i < count; i++) {
    if (buf.readUInt32LE(at) !== 0x02014b50) throw new Error('bad central directory');
    const n = buf.readUInt16LE(at + 28); const x = buf.readUInt16LE(at + 30); const c = buf.readUInt16LE(at + 32);
    names.push(buf.subarray(at + 46, at + 46 + n).toString('utf8'));
    at += 46 + n + x + c;
  }
  return names;
}

function fakeRepo({ toolsDir = 'tools/bin' } = {}) {
  const root = mkdtempSync(join(tmpdir(), 'ft-portable-'));
  for (const d of ['scripts', 'target/release', 'docs', `${toolsDir}/licenses`]) mkdirSync(join(root, d), { recursive: true });
  copyFileSync(join(repo, 'scripts/make-portable.ps1'), join(root, 'scripts/make-portable.ps1'));
  for (const f of ['frametrace.exe', 'frametrace-app.exe']) writeFileSync(join(root, 'target/release', f), 'MZ fake');
  for (const f of ['README.md', 'LICENSE', 'THIRD_PARTY_NOTICES.md']) writeFileSync(join(root, f), `fake ${f}\n`);
  writeFileSync(join(root, 'docs/WINDOWS_USAGE.md'), 'fake\n');
  writeFileSync(join(root, 'scripts/install.ps1'), '﻿# fake\n');
  // Planted on purpose: none of these may reach the zip.
  for (const f of ['ffmpeg.exe', 'ffprobe.exe', 'ffplay.exe', 'avcodec-61.dll']) writeFileSync(join(root, toolsDir, f), 'MZ planted ffmpeg');
  writeFileSync(join(root, toolsDir, 'licenses/ffmpeg-LICENSE.txt'), 'planted license\n');
  // A vendored tool that must still be packaged.
  writeFileSync(join(root, toolsDir, 'mmls.exe'), 'MZ fake mmls');
  writeFileSync(join(root, toolsDir, 'libewf.dll'), 'MZ fake dll');
  return root;
}

function runPortable(root, version = '0.0.0-test') {
  const env = { ...process.env };
  for (const k of ['HTTP_PROXY', 'HTTPS_PROXY', 'ALL_PROXY', 'http_proxy', 'https_proxy', 'all_proxy']) delete env[k];
  // A pwsh-7 parent (GitHub's `shell: pwsh`) leaves a PSModulePath that Windows
  // PowerShell 5.1 cannot auto-load modules from; unset it so 5.1 rebuilds its own.
  for (const k of Object.keys(env)) if (k.toLowerCase() === 'psmodulepath') delete env[k];
  return new Promise((ok) => {
    const child = spawn('powershell.exe', [
      '-NoProfile', '-ExecutionPolicy', 'Bypass', '-File', join(root, 'scripts/make-portable.ps1'),
      '-SkipBuild', '-Version', version,
    ], { cwd: root, env });
    let out = '';
    child.stdout.on('data', (d) => { out += d; });
    child.stderr.on('data', (d) => { out += d; });
    child.on('close', (code) => ok({ code, out }));
  });
}

let failures = 0;
const check = (cond, what, detail = '') => {
  if (!cond) { failures++; console.error(`FAIL ${what}${detail ? `\n${detail}` : ''}`); } else console.log(`ok   ${what}`);
};

const PREFIX = 'FrameTrace-0.0.0-test-win64/';
const roots = [];
try {
  for (const toolsDir of ['tools/bin', 'target/release/tools/bin']) {
    const root = fakeRepo({ toolsDir }); roots.push(root);
    const { code, out } = await runPortable(root);
    check(code === 0, `[${toolsDir}] make-portable.ps1 exits 0`, out);
    const zipPath = join(root, 'dist/FrameTrace-0.0.0-test-win64.zip');
    check(existsSync(zipPath), `[${toolsDir}] zip written`, out);
    check(/never packaged/.test(out), `[${toolsDir}] "never packaged" warning printed`, out);
    if (!existsSync(zipPath)) continue;
    const bytes = readFileSync(zipPath);
    const names = zipNames(bytes);
    check(names.length > 0 && names.every((n) => !n.includes('\\')), `[${toolsDir}] every entry name uses forward slashes`, names.join('\n'));
    for (const banned of ['ffmpeg.exe', 'ffprobe.exe', 'ffplay.exe', 'avcodec-61.dll', 'licenses/ffmpeg-LICENSE.txt']) {
      check(!names.some((n) => n.endsWith(banned)), `[${toolsDir}] zip has no ${banned}`, names.join('\n'));
    }
    for (const want of [
      'frametrace.exe', 'frametrace-app.exe', 'README.md', 'LICENSE', 'THIRD_PARTY_NOTICES.md',
      'docs/WINDOWS_USAGE.md', 'install.ps1',
      'tools/bin/mmls.exe', 'tools/bin/libewf.dll', 'tools/bin/README-tools.txt',
    ]) check(names.includes(PREFIX + want), `[${toolsDir}] zip has ${PREFIX}${want}`, names.join('\n'));
    const readme = readFileSync(join(root, 'dist/FrameTrace-0.0.0-test-win64/tools/bin/README-tools.txt'), 'utf8');
    check(readme.includes('ffmpeg.exe / ffprobe.exe — 패키지에 포함되지 않음'), `[${toolsDir}] README-tools says ffmpeg is not included`, readme);
    const sums = readFileSync(join(root, 'dist/SHA256SUMS'));
    check(sums[0] !== 0xef, `[${toolsDir}] SHA256SUMS has no BOM`);
    check(sums.toString('utf8') === `${sha256(bytes)}  FrameTrace-0.0.0-test-win64.zip\n`, `[${toolsDir}] SHA256SUMS is the zip hash, one LF line`, sums.toString('utf8'));
  }
  // A malformed -Version is refused before anything is staged.
  {
    const root = fakeRepo(); roots.push(root);
    const { code, out } = await runPortable(root, '../x');
    check(code !== 0 && /bad -Version/.test(out), 'path-like -Version refused', out);
  }
} finally {
  for (const root of roots) rmSync(root, { recursive: true, force: true });
}
if (failures) { console.error(`test-make-portable: ${failures} failure(s)`); process.exit(1); }
console.log('test-make-portable: all checks passed');
