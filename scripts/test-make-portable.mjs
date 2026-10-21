// Offline test of scripts/make-portable.ps1 (docs/PLAN_TOOLS_C_2026-10-20.md
// P5): a fake repo root with fake binaries, no build, no network. FFmpeg files
// are planted on purpose in tools/bin; the package must leave them out (with
// a "never packaged" warning) while still carrying the other tools, and the
// zip must use forward-slash entry names with a one-line LF SHA256SUMS.
// Fix round 5: renamed/odd-cased FFmpeg names (L3), a renamed binary found by
// its PE version information (L3), a stale FFmpeg zip in dist/ and
// SHA256SUMS listing only this run's zip (L4), and the "Owner to confirm"
// release guard (M2).
// Fix round 6: renamed FFmpeg files found by signature strings in their bytes
// under any extension (.exe, .bin, none; one match straddles the 4 MiB chunk
// boundary) while our own docs (the real README.md / WINDOWS_USAGE.md, prose
// that says "ffmpeg") still pass (L3b); an -AllowUnconfirmedNotices package is
// named *-UNCONFIRMED.zip with SHA256SUMS.unconfirmed and no SHA256SUMS, and
// the marker is found across a line break (L-CI).
// Fix round 11 M2: tools/manifest.json's notice_confirmed guard (same switch,
// same UNCONFIRMED naming).
// Windows only (needs Windows PowerShell).
//
//   node scripts/test-make-portable.mjs
import { spawn, spawnSync } from 'node:child_process';
import { createHash } from 'node:crypto';
import {
  mkdtempSync, mkdirSync, writeFileSync, readFileSync, copyFileSync, existsSync, rmSync, unlinkSync,
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

// A minimal stored (uncompressed) zip of `{ name: content }`.
const CRC_TABLE = Array.from({ length: 256 }, (_, n) => {
  let c = n;
  for (let k = 0; k < 8; k++) c = c & 1 ? 0xedb88320 ^ (c >>> 1) : c >>> 1;
  return c >>> 0;
});
const crc32 = (buf) => {
  let c = 0xffffffff;
  for (const b of buf) c = CRC_TABLE[(c ^ b) & 0xff] ^ (c >>> 8);
  return (c ^ 0xffffffff) >>> 0;
};
function makeZip(files) {
  const locals = []; const centrals = []; let offset = 0;
  for (const [name, content] of Object.entries(files)) {
    const n = Buffer.from(name, 'utf8'); const data = Buffer.from(content);
    const crc = crc32(data);
    const local = Buffer.alloc(30);
    local.writeUInt32LE(0x04034b50, 0); local.writeUInt16LE(20, 4); local.writeUInt16LE(0x0800, 6);
    local.writeUInt32LE(crc, 14); local.writeUInt32LE(data.length, 18); local.writeUInt32LE(data.length, 22);
    local.writeUInt16LE(n.length, 26);
    const central = Buffer.alloc(46);
    central.writeUInt32LE(0x02014b50, 0); central.writeUInt16LE(20, 4); central.writeUInt16LE(20, 6); central.writeUInt16LE(0x0800, 8);
    central.writeUInt32LE(crc, 16); central.writeUInt32LE(data.length, 20); central.writeUInt32LE(data.length, 24);
    central.writeUInt16LE(n.length, 28); central.writeUInt32LE(offset, 42);
    locals.push(local, n, data); centrals.push(central, n);
    offset += 30 + n.length + data.length;
  }
  const cd = Buffer.concat(centrals);
  const eocd = Buffer.alloc(22);
  eocd.writeUInt32LE(0x06054b50, 0); eocd.writeUInt16LE(Object.keys(files).length, 8); eocd.writeUInt16LE(Object.keys(files).length, 10);
  eocd.writeUInt32LE(cd.length, 12); eocd.writeUInt32LE(offset, 16);
  return Buffer.concat([...locals, cd, eocd]);
}

// A .NET library whose PE version information names FFmpeg (C# compiles the
// assembly attributes into the Win32 VERSIONINFO) — a renamed FFmpeg binary.
function ffmpegVersionInfoDll(out) {
  const src = [
    '[assembly: System.Reflection.AssemblyProduct("FFmpeg")]',
    '[assembly: System.Reflection.AssemblyCompany("FFmpeg Project")]',
    '[assembly: System.Reflection.AssemblyTitle("FFmpeg codec library")]',
    'public class FtFakeRenamedTool { }',
  ].join('\n');
  const ps = `$src = @'\n${src}\n'@\nAdd-Type -TypeDefinition $src -OutputAssembly '${out}' -OutputType Library`;
  const env = { ...process.env };
  for (const k of Object.keys(env)) if (k.toLowerCase() === 'psmodulepath') delete env[k];
  const r = spawnSync('powershell.exe', ['-NoProfile', '-NonInteractive', '-Command', ps], { env, encoding: 'utf8' });
  if (r.status !== 0 || !existsSync(out)) throw new Error(`cannot build the version-info fixture: ${r.stdout}${r.stderr}`);
}

// Odd FFmpeg names that the name filter must catch (L3). `ffmpeg.EXE ` has a
// real trailing space (written through the \\?\ namespace, which keeps it).
const ODD_NAMES = ['ffmpeg.exe.bak', 'FFProbe-Old.EXE', 'avcodec.dll', 'libavcodec-61.dll', 'swscale-8.dll', 'my-ffmpeg-copy.exe'];
const TRAILING = 'ffmpeg.EXE ';
const longPath = (p) => '\\\\?\\' + p.replace(/\//g, '\\');

// tools/manifest.json of a fake repo (Fix round 11 M2): one installable tool
// (sha256 pinned) whose notice is confirmed unless `noticeConfirmed` says
// otherwise (`'omit'` leaves the key out), and one manual tool (sha256
// null) that never needs a confirmation.
const fakeManifest = (noticeConfirmed = true) => JSON.stringify({
  schema: 1,
  tools: [
    { name: 'ffmpeg', sha256: 'a'.repeat(64), notice: 'n', ...(noticeConfirmed === 'omit' ? {} : { notice_confirmed: noticeConfirmed }) },
    { name: 'libewf', sha256: null, notice: null },
  ],
}, null, 2);

function fakeRepo({ toolsDir = 'tools/bin', notices = null, manifest = fakeManifest() } = {}) {
  const root = mkdtempSync(join(tmpdir(), 'ft-portable-'));
  for (const d of ['scripts', 'target/release', 'docs', 'tools', `${toolsDir}/licenses`]) mkdirSync(join(root, d), { recursive: true });
  if (manifest !== null) writeFileSync(join(root, 'tools/manifest.json'), manifest);
  copyFileSync(join(repo, 'scripts/make-portable.ps1'), join(root, 'scripts/make-portable.ps1'));
  for (const f of ['frametrace.exe', 'frametrace-app.exe']) writeFileSync(join(root, 'target/release', f), 'MZ fake');
  for (const f of ['LICENSE', 'THIRD_PARTY_NOTICES.md']) writeFileSync(join(root, f), `fake ${f}\n`);
  if (notices !== null) writeFileSync(join(root, 'THIRD_PARTY_NOTICES.md'), notices);
  // Our real docs: they mention ffmpeg in prose ("FFmpeg version text") and
  // must pass the signature scan.
  copyFileSync(join(repo, 'README.md'), join(root, 'README.md'));
  copyFileSync(join(repo, 'docs/WINDOWS_USAGE.md'), join(root, 'docs/WINDOWS_USAGE.md'));
  writeFileSync(join(root, 'scripts/install.ps1'), '﻿# fake\n');
  // Planted on purpose: none of these may reach the zip.
  for (const f of ['ffmpeg.exe', 'ffprobe.exe', 'ffplay.exe', 'avcodec-61.dll']) writeFileSync(join(root, toolsDir, f), 'MZ planted ffmpeg');
  writeFileSync(join(root, toolsDir, 'licenses/ffmpeg-LICENSE.txt'), 'planted license\n');
  for (const f of ODD_NAMES) writeFileSync(join(root, toolsDir, f), 'MZ planted ffmpeg');
  writeFileSync(longPath(join(root, toolsDir, TRAILING)), 'MZ planted ffmpeg');
  // A vendored tool that must still be packaged.
  writeFileSync(join(root, toolsDir, 'mmls.exe'), 'MZ fake mmls');
  writeFileSync(join(root, toolsDir, 'libewf.dll'), 'MZ fake dll');
  // Prose that names ffmpeg (no signature string, harmless name): packaged.
  writeFileSync(join(root, toolsDir, 'NOTES.txt'),
    'ffmpeg and ffprobe are not included; run frametrace tools install ffmpeg.\nThe log keeps the FFmpeg version text. Trial encodes use -f lavfi.\n');
  return root;
}

// Removes a fake repo (the trailing-space file only goes through \\?\).
function removeRoot(root) {
  for (const toolsDir of ['tools/bin', 'target/release/tools/bin']) {
    const odd = join(root, toolsDir, TRAILING);
    try { unlinkSync(longPath(odd)); } catch { /* not there */ }
  }
  rmSync(root, { recursive: true, force: true });
}

function runPortable(root, version = '0.0.0-test', extra = []) {
  const env = { ...process.env };
  for (const k of ['HTTP_PROXY', 'HTTPS_PROXY', 'ALL_PROXY', 'http_proxy', 'https_proxy', 'all_proxy']) delete env[k];
  // A pwsh-7 parent (GitHub's `shell: pwsh`) leaves a PSModulePath that Windows
  // PowerShell 5.1 cannot auto-load modules from; unset it so 5.1 rebuilds its own.
  for (const k of Object.keys(env)) if (k.toLowerCase() === 'psmodulepath') delete env[k];
  return new Promise((ok) => {
    const child = spawn('powershell.exe', [
      '-NoProfile', '-ExecutionPolicy', 'Bypass', '-File', join(root, 'scripts/make-portable.ps1'),
      '-SkipBuild', '-Version', version, ...extra,
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
    // L4: a clean zip left in dist/ by something else is not vouched for.
    mkdirSync(join(root, 'dist'), { recursive: true });
    writeFileSync(join(root, 'dist/old-notes.zip'), makeZip({ 'notes/readme.txt': 'old\n' }));
    const { code, out } = await runPortable(root);
    check(code === 0, `[${toolsDir}] make-portable.ps1 exits 0`, out);
    const zipPath = join(root, 'dist/FrameTrace-0.0.0-test-win64.zip');
    check(existsSync(zipPath), `[${toolsDir}] zip written`, out);
    check(/never packaged/.test(out), `[${toolsDir}] "never packaged" warning printed`, out);
    if (!existsSync(zipPath)) continue;
    const bytes = readFileSync(zipPath);
    const names = zipNames(bytes);
    check(names.length > 0 && names.every((n) => !n.includes('\\')), `[${toolsDir}] every entry name uses forward slashes`, names.join('\n'));
    for (const banned of ['ffmpeg.exe', 'ffprobe.exe', 'ffplay.exe', 'avcodec-61.dll', 'licenses/ffmpeg-LICENSE.txt', ...ODD_NAMES, TRAILING]) {
      check(!names.some((n) => n.toLowerCase().endsWith(banned.toLowerCase())), `[${toolsDir}] zip has no ${JSON.stringify(banned)}`, names.join('\n'));
    }
    check(!names.some((n) => /ffmpeg|ffprobe|ffplay|avcodec|swscale/i.test(n)), `[${toolsDir}] no entry name mentions FFmpeg at all`, names.join('\n'));
    for (const f of [...ODD_NAMES, TRAILING]) {
      check(out.includes(`skipped: `) && out.includes(f.trim()), `[${toolsDir}] warned about ${JSON.stringify(f)}`, out);
    }
    for (const want of [
      'frametrace.exe', 'frametrace-app.exe', 'README.md', 'LICENSE', 'THIRD_PARTY_NOTICES.md',
      'docs/WINDOWS_USAGE.md', 'install.ps1',
      'tools/bin/mmls.exe', 'tools/bin/libewf.dll', 'tools/bin/README-tools.txt', 'tools/bin/NOTES.txt',
    ]) check(names.includes(PREFIX + want), `[${toolsDir}] zip has ${PREFIX}${want}`, names.join('\n'));
    const readme = readFileSync(join(root, 'dist/FrameTrace-0.0.0-test-win64/tools/bin/README-tools.txt'), 'utf8');
    check(readme.includes('ffmpeg.exe / ffprobe.exe — 패키지에 포함되지 않음'), `[${toolsDir}] README-tools says ffmpeg is not included`, readme);
    const sums = readFileSync(join(root, 'dist/SHA256SUMS'));
    check(sums[0] !== 0xef, `[${toolsDir}] SHA256SUMS has no BOM`);
    check(sums.toString('utf8') === `${sha256(bytes)}  FrameTrace-0.0.0-test-win64.zip\n`, `[${toolsDir}] SHA256SUMS is this run's zip hash only (not old-notes.zip), one LF line`, sums.toString('utf8'));
    check(!existsSync(join(root, 'dist/SHA256SUMS.unconfirmed')) && !existsSync(join(root, 'dist/FrameTrace-0.0.0-test-win64-UNCONFIRMED.zip')),
      `[${toolsDir}] a confirmed package has no *.unconfirmed / *-UNCONFIRMED.zip`);
  }
  // L3b: a renamed FFmpeg file is found by signature strings in its bytes,
  // whatever its extension (real static ffmpeg.exe has empty version info).
  {
    const CHUNK = 4 * 1024 * 1024;
    const straddle = Buffer.alloc(CHUNK + 1024, 0x20);
    Buffer.from('Lavf61.7.100').copy(straddle, CHUNK - 5); // across the 4 MiB chunk boundary
    for (const [name, content, sig] of [
      ['my-tool.exe', Buffer.from('MZ\0\0Copyright (c) 2000-2024 the FFmpeg developers\0'), 'the FFmpeg developers'],
      ['payload.bin', straddle, 'Lavf61.7'],
      ['blob', Buffer.from('\0\0\0FFMPEG VERSION 7.1-full_build-www.gyan.dev\0'), 'FFMPEG VERSION 7'],
      ['helper.dat', Buffer.from('\0ident\0libavcodec\0Lavc61.19.1\0'), 'libavcodec'],
      // Fix round 7: libavutil / libswscale (absent from our exes and docs).
      ['util.so', Buffer.from('\0\0LIBAVUTIL_59\0'), 'LIBAVUTIL'],
      ['scale.part', Buffer.from('\0libswscale license: LGPL\0'), 'libswscale'],
    ]) {
      const root = fakeRepo(); roots.push(root);
      writeFileSync(join(root, 'tools/bin', name), content);
      const started = Date.now();
      const { code, out } = await runPortable(root);
      const escaped = name.replace(/\./g, '\\.');
      check(code !== 0 && new RegExp(`${escaped} contains the FFmpeg signature`).test(out) && out.includes(sig),
        `a renamed FFmpeg file "${name}" (signature ${JSON.stringify(sig)}) fails the run`, out);
      check(!existsSync(join(root, 'dist/FrameTrace-0.0.0-test-win64.zip')), `no zip written for "${name}"`);
      check(Date.now() - started < 60000, `"${name}" scan is quick (${Date.now() - started} ms)`);
    }
  }
  // L4: a stale zip in dist/ that contains FFmpeg (the old *-bundled.zip) fails the run.
  {
    const root = fakeRepo(); roots.push(root);
    mkdirSync(join(root, 'dist'), { recursive: true });
    writeFileSync(join(root, 'dist/FrameTrace-0.0.0-test-win64-bundled.zip'),
      makeZip({ 'FrameTrace-0.0.0-test-win64/tools/bin/ffmpeg.exe': 'MZ old bundle', 'FrameTrace-0.0.0-test-win64/README.md': 'x' }));
    const { code, out } = await runPortable(root);
    check(code !== 0 && /bundled\.zip contains FFmpeg/.test(out), 'a stale FFmpeg zip in dist/ fails the run', out);
    check(!existsSync(join(root, 'dist/SHA256SUMS')), 'no SHA256SUMS written next to a stale FFmpeg zip');
  }
  // L3: a renamed FFmpeg binary is found by its PE version information.
  {
    const root = fakeRepo(); roots.push(root);
    ffmpegVersionInfoDll(join(root, 'tools/bin/renamed-tool.dll'));
    const { code, out } = await runPortable(root);
    check(code !== 0 && /renamed-tool\.dll is an FFmpeg binary by its version information/.test(out), 'a renamed FFmpeg binary (version info) fails the run', out);
    check(!existsSync(join(root, 'dist/FrameTrace-0.0.0-test-win64.zip')), 'no zip written for it');
  }
  // M2: "Owner to confirm" in THIRD_PARTY_NOTICES.md blocks a release package …
  {
    const notices = 'Notices\n\nSome paragraph. (Owner to confirm this paragraph.)\n';
    const root = fakeRepo({ notices }); roots.push(root);
    const blocked = await runPortable(root);
    check(blocked.code !== 0 && /Owner to confirm/.test(blocked.out) && /AllowUnconfirmedNotices/.test(blocked.out), 'unconfirmed notices block the package', blocked.out);
    check(!existsSync(join(root, 'dist/FrameTrace-0.0.0-test-win64.zip')), 'no zip while the notices are unconfirmed');
    // … unless -AllowUnconfirmedNotices (CI test package), with a warning —
    // and marked (L-CI): *-UNCONFIRMED.zip, SHA256SUMS.unconfirmed, no
    // SHA256SUMS (a stale one from an earlier run is removed).
    mkdirSync(join(root, 'dist'), { recursive: true });
    writeFileSync(join(root, 'dist/SHA256SUMS'), 'stale  FrameTrace-0.0.0-test-win64.zip\n');
    const allowed = await runPortable(root, '0.0.0-test', ['-AllowUnconfirmedNotices']);
    check(allowed.code === 0 && /test package only/.test(allowed.out), '-AllowUnconfirmedNotices packages with a warning', allowed.out);
    check(/UNCONFIRMED TEST PACKAGE/.test(allowed.out) && /NOT FOR RELEASE/.test(allowed.out), '-AllowUnconfirmedNotices prints the loud warning', allowed.out);
    const unconfirmedZip = join(root, 'dist/FrameTrace-0.0.0-test-win64-UNCONFIRMED.zip');
    check(existsSync(unconfirmedZip), 'zip written as *-UNCONFIRMED.zip with -AllowUnconfirmedNotices', allowed.out);
    check(!existsSync(join(root, 'dist/FrameTrace-0.0.0-test-win64.zip')), 'no plain (release-looking) zip with -AllowUnconfirmedNotices');
    check(!existsSync(join(root, 'dist/SHA256SUMS')), 'no SHA256SUMS with -AllowUnconfirmedNotices (the stale one removed)');
    const usums = join(root, 'dist/SHA256SUMS.unconfirmed');
    check(existsSync(usums) && existsSync(unconfirmedZip)
      && readFileSync(usums, 'utf8') === `${sha256(readFileSync(unconfirmedZip))}  FrameTrace-0.0.0-test-win64-UNCONFIRMED.zip\n`,
    'SHA256SUMS.unconfirmed lists the UNCONFIRMED zip', existsSync(usums) ? readFileSync(usums, 'utf8') : 'missing');
  }
  // L-CI: the marker reflowed across a line break (CRLF + indent) still blocks.
  {
    const notices = 'Notices\r\n\r\nSome paragraph. (Owner to\r\n   confirm this paragraph.)\r\n';
    const root = fakeRepo({ notices }); roots.push(root);
    const blocked = await runPortable(root);
    check(blocked.code !== 0 && /AllowUnconfirmedNotices/.test(blocked.out), 'a line-broken "Owner to / confirm" marker blocks the package', blocked.out);
    check(!existsSync(join(root, 'dist/FrameTrace-0.0.0-test-win64.zip')), 'no zip for the line-broken marker');
  }
  // The real THIRD_PARTY_NOTICES.md: blocked while it carries the marker, and
  // its text (prose about FFmpeg) passes the signature scan as a test package.
  {
    const real = readFileSync(join(repo, 'THIRD_PARTY_NOTICES.md'), 'utf8');
    const root = fakeRepo({ notices: real }); roots.push(root);
    const marked = /Owner\s+to\s+confirm/i.test(real);
    const run = await runPortable(root, '0.0.0-test', marked ? ['-AllowUnconfirmedNotices'] : []);
    const zip = join(root, `dist/FrameTrace-0.0.0-test-win64${marked ? '-UNCONFIRMED' : ''}.zip`);
    check(run.code === 0 && existsSync(zip), `the real THIRD_PARTY_NOTICES.md passes the signature scan (${marked ? 'unconfirmed' : 'confirmed'})`, run.out);
  }
  // Fix round 11 M2: an installable tool whose distribution notice is not
  // confirmed (false, missing, or a non-boolean "true") blocks a release
  // package; a manual tool (sha256 null) never does.
  for (const [label, confirmed] of [['false', false], ['missing', 'omit'], ['string "true"', 'true']]) {
    const root = fakeRepo({ manifest: fakeManifest(confirmed) }); roots.push(root);
    const blocked = await runPortable(root);
    check(blocked.code !== 0 && /notice_confirmed/.test(blocked.out) && /ffmpeg/.test(blocked.out) && /AllowUnconfirmedNotices/.test(blocked.out),
      `notice_confirmed ${label} blocks the package`, blocked.out);
    check(!existsSync(join(root, 'dist/FrameTrace-0.0.0-test-win64.zip')), `no zip for notice_confirmed ${label}`);
  }
  {
    const root = fakeRepo({ manifest: fakeManifest(false) }); roots.push(root);
    mkdirSync(join(root, 'dist'), { recursive: true });
    writeFileSync(join(root, 'dist/SHA256SUMS'), 'stale  FrameTrace-0.0.0-test-win64.zip\n');
    const allowed = await runPortable(root, '0.0.0-test', ['-AllowUnconfirmedNotices']);
    check(allowed.code === 0 && /notice of ffmpeg is not confirmed/.test(allowed.out) && /UNCONFIRMED TEST PACKAGE/.test(allowed.out) && /notice_confirmed is not true for: ffmpeg/.test(allowed.out),
      'an unconfirmed tool notice with -AllowUnconfirmedNotices: a test package with the loud warning', allowed.out);
    check(existsSync(join(root, 'dist/FrameTrace-0.0.0-test-win64-UNCONFIRMED.zip')) && !existsSync(join(root, 'dist/FrameTrace-0.0.0-test-win64.zip')),
      'an unconfirmed tool notice: the zip is *-UNCONFIRMED.zip', allowed.out);
    check(existsSync(join(root, 'dist/SHA256SUMS.unconfirmed')) && !existsSync(join(root, 'dist/SHA256SUMS')), 'an unconfirmed tool notice: SHA256SUMS.unconfirmed only');
  }
  {
    const root = fakeRepo({ manifest: null }); roots.push(root);
    const missing = await runPortable(root);
    check(missing.code !== 0 && /tools\/manifest\.json not found/.test(missing.out), 'a missing tools/manifest.json fails the run', missing.out);
  }
  // The real tools/manifest.json: blocked while a notice is unconfirmed.
  {
    const real = readFileSync(join(repo, 'tools/manifest.json'), 'utf8');
    const pending = JSON.parse(real).tools.filter((t) => t.sha256 && t.notice_confirmed !== true).map((t) => t.name);
    const root = fakeRepo({ manifest: real }); roots.push(root);
    const run = await runPortable(root);
    check(pending.length ? run.code !== 0 && /notice_confirmed/.test(run.out) : run.code === 0,
      `the real tools/manifest.json ${pending.length ? `blocks a release package (unconfirmed: ${pending.join(', ')})` : 'packages (every notice confirmed)'}`, run.out);
  }
  // A malformed -Version is refused before anything is staged.
  {
    const root = fakeRepo(); roots.push(root);
    const { code, out } = await runPortable(root, '../x');
    check(code !== 0 && /bad -Version/.test(out), 'path-like -Version refused', out);
  }
} finally {
  for (const root of roots) removeRoot(root);
}
if (failures) { console.error(`test-make-portable: ${failures} failure(s)`); process.exit(1); }
console.log('test-make-portable: all checks passed');
