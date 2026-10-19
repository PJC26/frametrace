// Offline test of scripts/package-windows.ps1 (tools track S5, Fix round 1):
// a fake FFmpeg zip served from 127.0.0.1, a fake repo root, no internet, no
// real binaries. Checks: redirects are followed hop by hop and a hop to a
// host outside the allowlist is refused before it is contacted; a redirect
// loop is bounded; a hash mismatch packages nothing; the package zip is
// written by ZipFile.CreateFromDirectory with forward-slash entry names and
// carries the manifest's files plus the pinned GPLv3 / third-party license
// texts. Windows only (needs Windows PowerShell and System32 curl.exe).
//
//   node scripts/test-package-windows.mjs
import { createServer } from 'node:http';
import { spawn } from 'node:child_process';
import { createHash } from 'node:crypto';
import {
  mkdtempSync, mkdirSync, writeFileSync, readFileSync, copyFileSync, existsSync, rmSync,
} from 'node:fs';
import { tmpdir } from 'node:os';
import { join, dirname } from 'node:path';
import { fileURLToPath } from 'node:url';

if (process.platform !== 'win32') {
  console.log('test-package-windows: SKIPPED (Windows only)');
  process.exit(0);
}

const repo = join(dirname(fileURLToPath(import.meta.url)), '..');
const sha256 = (bytes) => createHash('sha256').update(bytes).digest('hex');

// ---- a minimal stored zip ------------------------------------------------------
const CRC_TABLE = Array.from({ length: 256 }, (_, n) => {
  let c = n;
  for (let k = 0; k < 8; k++) c = c & 1 ? 0xedb88320 ^ (c >>> 1) : c >>> 1;
  return c >>> 0;
});
function crc32(buf) {
  let c = 0xffffffff;
  for (const b of buf) c = CRC_TABLE[(c ^ b) & 0xff] ^ (c >>> 8);
  return (c ^ 0xffffffff) >>> 0;
}
function makeZip(entries) {
  const locals = [];
  const central = [];
  let offset = 0;
  for (const [name, text] of entries) {
    const data = Buffer.from(text);
    const nameBuf = Buffer.from(name);
    const crc = crc32(data);
    const local = Buffer.alloc(30);
    local.writeUInt32LE(0x04034b50, 0); local.writeUInt16LE(20, 4);
    local.writeUInt32LE(crc, 14); local.writeUInt32LE(data.length, 18); local.writeUInt32LE(data.length, 22);
    local.writeUInt16LE(nameBuf.length, 26);
    locals.push(local, nameBuf, data);
    const cen = Buffer.alloc(46);
    cen.writeUInt32LE(0x02014b50, 0); cen.writeUInt16LE(20, 4); cen.writeUInt16LE(20, 6);
    cen.writeUInt32LE(crc, 16); cen.writeUInt32LE(data.length, 20); cen.writeUInt32LE(data.length, 24);
    cen.writeUInt16LE(nameBuf.length, 28); cen.writeUInt32LE(offset, 42);
    central.push(cen, nameBuf);
    offset += local.length + nameBuf.length + data.length;
  }
  const cd = Buffer.concat(central);
  const end = Buffer.alloc(22);
  end.writeUInt32LE(0x06054b50, 0); end.writeUInt16LE(entries.length, 8); end.writeUInt16LE(entries.length, 10);
  end.writeUInt32LE(cd.length, 12); end.writeUInt32LE(offset, 16);
  return Buffer.concat([...locals, cd, end]);
}
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

// ---- fake sources ----------------------------------------------------------------
function serve(routes) {
  const server = createServer((req, res) => {
    server.hits = (server.hits || 0) + 1;
    const route = routes[req.url];
    if (!route) { res.writeHead(404); res.end(); return; }
    if (route.redirect) { res.writeHead(302, { Location: route.redirect(server) }); res.end(); return; }
    res.writeHead(200, { 'Content-Length': route.body.length }); res.end(route.body);
  });
  server.hits = 0;
  return new Promise((ok) => server.listen(0, '127.0.0.1', () => ok(server)));
}

const ZIPROOT = 'ffmpeg-fake-win64-lgpl';
const fakeZip = makeZip([
  [`${ZIPROOT}/`, ''],
  [`${ZIPROOT}/bin/ffmpeg.exe`, 'MZ fake ffmpeg'],
  [`${ZIPROOT}/bin/ffprobe.exe`, 'MZ fake ffprobe'],
  [`${ZIPROOT}/bin/ffplay.exe`, 'not listed'],
  [`${ZIPROOT}/LICENSE.txt`, 'fake LGPLv3'],
]);

function fakeRepo(manifest) {
  const root = mkdtempSync(join(tmpdir(), 'ft-pkgtest-'));
  for (const d of ['scripts', 'target/release', 'docs', 'tools/licenses']) mkdirSync(join(root, d), { recursive: true });
  copyFileSync(join(repo, 'scripts/package-windows.ps1'), join(root, 'scripts/package-windows.ps1'));
  for (const f of ['frametrace.exe', 'frametrace-app.exe']) writeFileSync(join(root, 'target/release', f), 'MZ fake');
  for (const f of ['README.md', 'LICENSE', 'THIRD_PARTY_NOTICES.md']) writeFileSync(join(root, f), `fake ${f}\n`);
  writeFileSync(join(root, 'docs/WINDOWS_USAGE.md'), 'fake\n');
  writeFileSync(join(root, 'scripts/install.ps1'), '﻿# fake\n');
  for (const l of [...manifest.tools[0].bundled_licenses, ...manifest.tools[0].package_licenses]) {
    copyFileSync(join(repo, l.file), join(root, l.file));
  }
  writeFileSync(join(root, 'fake-manifest.json'), JSON.stringify(manifest, null, 2));
  return root;
}

function runPackage(root) {
  const env = { ...process.env, NO_PROXY: '127.0.0.1,localhost', no_proxy: '127.0.0.1,localhost' };
  for (const k of ['HTTP_PROXY', 'HTTPS_PROXY', 'ALL_PROXY', 'http_proxy', 'https_proxy', 'all_proxy']) delete env[k];
  // A pwsh-7 parent (GitHub's `shell: pwsh`) leaves a PSModulePath that Windows
  // PowerShell 5.1 cannot auto-load modules from; unset it so 5.1 rebuilds its own.
  for (const k of Object.keys(env)) if (k.toLowerCase() === 'psmodulepath') delete env[k];
  return new Promise((ok) => {
    const child = spawn('powershell.exe', [
      '-NoProfile', '-ExecutionPolicy', 'Bypass', '-File', join(root, 'scripts/package-windows.ps1'),
      '-SkipBuild', '-Version', '0.0.0-test', '-ManifestPath', 'fake-manifest.json', '-TestLoopbackHttp',
    ], { cwd: root, env });
    let out = '';
    child.stdout.on('data', (d) => { out += d; });
    child.stderr.on('data', (d) => { out += d; });
    child.on('close', (code) => ok({ code, out }));
  });
}

const real = JSON.parse(readFileSync(join(repo, 'tools/manifest.json'), 'utf8')).tools.find((t) => t.name === 'ffmpeg');
function manifestFor(url, zip = fakeZip) {
  return {
    schema: 1,
    allowed_hosts: ['127.0.0.1'],
    tools: [{
      name: 'ffmpeg', version: 'fake', license: 'LGPL-3.0-or-later',
      url, file_name: 'ffmpeg.zip', size: zip.length, sha256: sha256(zip),
      checksum_source: 'test', source_url: 'test', build_url: 'test',
      files: [
        { from: `${ZIPROOT}/bin/ffmpeg.exe`, to: 'ffmpeg.exe' },
        { from: `${ZIPROOT}/bin/ffprobe.exe`, to: 'ffprobe.exe' },
      ],
      license_files: [{ from: `${ZIPROOT}/LICENSE.txt`, to: 'ffmpeg-LICENSE.txt' }],
      bundled_licenses: real.bundled_licenses,
      package_licenses: real.package_licenses || [],
    }],
  };
}

let failures = 0;
const check = (cond, what, detail = '') => {
  if (!cond) { failures++; console.error(`FAIL ${what}${detail ? `\n${detail}` : ''}`); } else console.log(`ok   ${what}`);
};

const outside = await serve({ '/x.zip': { body: fakeZip } });
const source = await serve({
  '/start': { redirect: (s) => `http://127.0.0.1:${s.address().port}/real/ffmpeg.zip` },
  '/real/ffmpeg.zip': { body: fakeZip },
  '/evil': { redirect: () => `http://localhost:${outside.address().port}/x.zip` },
  '/evil-https': { redirect: () => 'https://example.com/x.zip' },
  '/loop': { redirect: (s) => `http://127.0.0.1:${s.address().port}/loop` },
});
const base = `http://127.0.0.1:${source.address().port}`;
const roots = [];
try {
  // 1. Success through one allowed redirect hop.
  {
    const root = fakeRepo(manifestFor(`${base}/start`)); roots.push(root);
    const hits = source.hits;
    const { code, out } = await runPackage(root);
    check(code === 0, 'packages through an allowlisted redirect', out);
    check(source.hits - hits === 2, 'one hop + the download', `hits ${source.hits - hits}`);
    const zipPath = join(root, 'dist/FrameTrace-0.0.0-test-win64-bundled.zip');
    check(existsSync(zipPath), 'package zip written');
    if (existsSync(zipPath)) {
      const names = zipNames(readFileSync(zipPath));
      check(names.length > 0 && names.every((n) => !n.includes('\\')), 'every entry name uses forward slashes', names.join('\n'));
      for (const want of [
        'FrameTrace/frametrace.exe', 'FrameTrace/THIRD_PARTY_NOTICES.md',
        'FrameTrace/tools/bin/ffmpeg.exe', 'FrameTrace/tools/bin/ffprobe.exe',
        'FrameTrace/tools/bin/licenses/ffmpeg-LICENSE.txt',
        'FrameTrace/tools/bin/licenses/ffmpeg-SOURCE.txt',
        ...[...real.bundled_licenses, ...(real.package_licenses || [])].map((l) => `FrameTrace/tools/bin/licenses/${l.to}`),
      ]) check(names.includes(want), `zip has ${want}`, names.join('\n'));
      check(!names.some((n) => n.endsWith('ffplay.exe')), 'unlisted archive file not packaged');
      const gpl = readFileSync(join(root, 'dist/FrameTrace/tools/bin/licenses', real.bundled_licenses[0].to));
      check(sha256(gpl) === real.bundled_licenses[0].sha256, 'GPLv3 text is the pinned bytes');
      const sums = readFileSync(join(root, 'dist/SHA256SUMS'), 'utf8');
      check(sums === `${sha256(readFileSync(zipPath))}  FrameTrace-0.0.0-test-win64-bundled.zip\n`, 'SHA256SUMS matches the zip', sums);
    }
  }
  // 2. A redirect to a host outside the allowlist is refused before contact.
  {
    const root = fakeRepo(manifestFor(`${base}/evil`)); roots.push(root);
    const before = outside.hits;
    const { code, out } = await runPackage(root);
    check(code !== 0 && /refused URL/.test(out), 'off-allowlist redirect refused', out);
    check(outside.hits === before, 'the off-allowlist host was never contacted');
    check(!existsSync(join(root, 'dist/FrameTrace')), 'no half-built package left');
  }
  {
    const root = fakeRepo(manifestFor(`${base}/evil-https`)); roots.push(root);
    const { code, out } = await runPackage(root);
    check(code !== 0 && /host example\.com is not on the tool source allowlist/.test(out), 'https redirect to another host refused by the allowlist', out);
  }
  // 3. A redirect loop is bounded.
  {
    const root = fakeRepo(manifestFor(`${base}/loop`)); roots.push(root);
    const { code, out } = await runPackage(root);
    check(code !== 0 && /more than 5 redirects/.test(out), 'redirect loop bounded', out);
  }
  // 4. A hash mismatch packages nothing.
  {
    const manifest = manifestFor(`${base}/real/ffmpeg.zip`);
    manifest.tools[0].sha256 = '0'.repeat(64);
    const root = fakeRepo(manifest); roots.push(root);
    const { code, out } = await runPackage(root);
    check(code !== 0 && /SHA-256 mismatch/.test(out), 'hash mismatch refused', out);
    check(!existsSync(join(root, 'dist/FrameTrace')), 'nothing staged after a mismatch');
  }
  // 5. An https URL on a non-allowlisted host is refused up front.
  {
    const root = fakeRepo(manifestFor('https://example.com/ffmpeg.zip')); roots.push(root);
    const { code, out } = await runPackage(root);
    check(code !== 0 && /not on the tool source allowlist/.test(out), 'non-allowlisted manifest URL refused', out);
  }
} finally {
  source.close(); outside.close();
  for (const root of roots) rmSync(root, { recursive: true, force: true });
}
if (failures) { console.error(`test-package-windows: ${failures} failure(s)`); process.exit(1); }
console.log('test-package-windows: all checks passed');
