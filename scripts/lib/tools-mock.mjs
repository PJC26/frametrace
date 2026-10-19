// A mocked tools API in the real track S shapes (docs/PLAN_TOOLS_2026-10-20.md
// "§S as implemented", docs/ui-rebuild-contract.md §28) for browser
// harnesses (verify-layout, check-a11y):
//   GET  /api/tools                 {ok, platform, platform_supported, tools:[T], install: J|null}
//   POST /api/tools/install {name}  {ok:true, install: J} | {ok:false, code, error[, manual_hint, homepage]}
//   GET  /api/tools/install-status  {ok:true, install: J|null}
//   POST /api/tools/install-cancel  {ok:true, install: J} | {ok:false, code:"no_install"|"not_cancellable"}
// Nothing is downloaded: the install walks downloading → verifying →
// extracting → done over the next status polls.
const FF_ZIP = 'ffmpeg-n8.1.3-9-g29e619e767-win64-lgpl-8.1.zip';
export const TOOLS_FIXTURE = () => ([
  {
    name: 'ffmpeg', title: 'FFmpeg (LGPL build)', label: 'FFmpeg (LGPL build)', kind: 'archive', required: true, on_demand: false,
    version: 'n8.1.3-9-g29e619e767', pinned_version: 'n8.1.3-9-g29e619e767', license: 'LGPL-3.0-or-later', homepage: 'https://ffmpeg.org/',
    source_url: 'https://github.com/FFmpeg/FFmpeg/tree/29e619e767', provides: ['ffmpeg', 'ffprobe'], status: 'missing', installed: false,
    paths: { ffmpeg: null, ffprobe: null }, installable: true, unavailable_reason: null, not_installable_reason: null,
    manual_hint: 'https://ffmpeg.org/download.html 에서 LGPL 빌드를 받아 ffmpeg.exe·ffprobe.exe를 tools/bin에 복사하십시오.',
    source_host: 'github.com', size_bytes: 4194304, encoders: null,
    download: { file_name: FF_ZIP, url: 'https://github.com/BtbN/FFmpeg-Builds/releases/download/autobuild-2026-09-30-13-08/' + FF_ZIP, host: 'github.com', size: 4194304, sha256: '4a7642b2264c03e8a0ce8a3825b933ee5580656f45695a086fe7e294045ffc0a', checksum_source: 'https://github.com/BtbN/FFmpeg-Builds/releases/download/autobuild-2026-09-30-13-08/checksums.sha256' },
  },
  {
    name: 'libewf', title: 'libewf (E01)', label: 'libewf (E01)', kind: 'archive', required: false, on_demand: false,
    version: '20260924', pinned_version: '20260924', license: 'LGPL-3.0-or-later', homepage: 'https://github.com/libyal/libewf',
    source_url: 'https://github.com/libyal/libewf/releases/tag/20260924', provides: ['ewfinfo', 'ewfverify', 'ewfexport'], status: 'missing', installed: false,
    paths: { ewfinfo: null, ewfverify: null, ewfexport: null }, installable: false,
    unavailable_reason: 'The official release (https://github.com/libyal/libewf/releases/tag/20260924) publishes only a source tarball and its PGP signature, no Windows binaries.',
    not_installable_reason: 'The official release (https://github.com/libyal/libewf/releases/tag/20260924) publishes only a source tarball and its PGP signature, no Windows binaries.',
    manual_hint: 'libewf는 공식 Windows 실행 파일이 없습니다 — scripts/build-libewf-tools.ps1로 소스에서 빌드하거나 직접 설치한 뒤 ewfinfo·ewfverify·ewfexport(와 DLL)를 tools/bin에 복사하십시오: https://github.com/libyal/libewf',
    source_host: null, size_bytes: null, encoders: null, download: null,
  },
  {
    name: 'sleuthkit', title: 'The Sleuth Kit (삭제 파일 복구)', label: 'The Sleuth Kit (삭제 파일 복구)', kind: 'archive', required: false, on_demand: false,
    version: '4.15.0', pinned_version: '4.15.0', license: 'IPL-1.0 / CPL-1.0', homepage: 'https://www.sleuthkit.org/sleuthkit/',
    source_url: 'https://github.com/sleuthkit/sleuthkit/releases/tag/sleuthkit-4.15.0', provides: ['mmls', 'fls', 'icat'], status: 'missing', installed: false,
    paths: { mmls: null, fls: null, icat: null }, installable: false,
    unavailable_reason: 'The maintainers publish a PGP signature (.asc), not a checksum file, and the archive\'s file list could not be verified without downloading it.',
    not_installable_reason: 'The maintainers publish a PGP signature (.asc), not a checksum file, and the archive\'s file list could not be verified without downloading it.',
    manual_hint: 'sleuthkit-4.15.0-win32.zip을 공식 배포처에서 받아 PGP 서명(.asc)을 확인한 뒤 mmls·fls·icat과 함께 들어 있는 DLL을 tools/bin에 복사하십시오: https://www.sleuthkit.org/sleuthkit/download.php',
    source_host: null, size_bytes: null, encoders: null, download: null,
  },
  {
    name: 'deepfake-lens', title: 'deepfake-lens (합성 영상 스크리닝)', label: 'deepfake-lens (합성 영상 스크리닝)', kind: 'pip', required: false, on_demand: true,
    version: null, pinned_version: null, license: null, homepage: null, source_url: null, provides: ['deepfake-lens'], status: 'missing', installed: false,
    paths: { 'deepfake-lens': null }, installable: false,
    unavailable_reason: 'deepfake-lens is not published on PyPI, so no hash-pinned requirements file can be built.',
    not_installable_reason: 'deepfake-lens is not published on PyPI, so no hash-pinned requirements file can be built.',
    manual_hint: 'deepfake-lens는 PyPI에 공개되지 않아 자동 설치할 수 없습니다 — 받은 패키지를 직접 설치(pip install <경로>)하고 FRAMETRACE_DEEPFAKE_LENS에 deepfake-lens.exe 경로를 지정하십시오.',
    source_host: 'pypi.org', size_bytes: null, encoders: null, download: null,
  },
]);

const STAGE = { downloading: 'download', verifying: 'verify', extracting: 'install', done: 'done', failed: 'failed' };
const json = (route, body) => route.fulfill({ status: 200, contentType: 'application/json', body: JSON.stringify(body) });

// opts.steps: status polls per install stage (default 2); opts.fail: an
// error string → the job ends `failed`; opts.tools: a fixture override;
// opts.install: a job already running when the page loads (a refresh).
// Returns the live mock state { posts: [{name}], cancels, tools, install }.
export async function routeTools(page, opts = {}) {
  const steps = opts.steps || 2;
  const m = { posts: [], cancels: 0, tools: opts.tools || TOOLS_FIXTURE(), install: opts.install || null, polls: 0, seq: 0 };
  const advance = () => {
    const i = m.install;
    if (!i || i.state === 'done' || i.state === 'failed') return;
    m.polls += 1;
    const t = m.tools.find((x) => x.name === i.tool);
    if (i.state === 'downloading') {
      i.bytes = Math.min(i.total || 0, Math.round((i.total || 0) * m.polls / (steps + 1)));
      if (m.polls > steps) { i.state = 'verifying'; m.polls = 0; }
    } else if (i.state === 'verifying') {
      if (m.polls > steps) { i.state = 'extracting'; m.polls = 0; }
    } else if (i.state === 'extracting') {
      if (m.polls > steps) {
        i.finished_unix = 1760000100;
        if (opts.fail) { i.state = 'failed'; i.error = opts.fail; } else {
          i.state = 'done'; t.installed = true; t.status = 'installed';
          for (const k of Object.keys(t.paths)) t.paths[k] = 'C:\\FrameTrace\\tools\\bin\\' + k + '.exe';
          i.installed_files = Object.values(t.paths);
        }
      }
    }
    i.stage = STAGE[i.state];
  };
  await page.route('**/api/tools/install', async (route) => {
    const req = route.request();
    if (req.method() !== 'POST') return route.fulfill({ status: 405, body: '{"ok":false}', contentType: 'application/json' });
    let body = {};
    try { body = JSON.parse(req.postData() || '{}'); } catch { /* bad body */ }
    m.posts.push(body);
    const t = m.tools.find((x) => x.name === body.name);
    if (!t) return json(route, { ok: false, code: 'unknown_tool', error: '알 수 없는 도구입니다: ' + body.name });
    if (!t.installable) return json(route, { ok: false, code: 'not_installable', error: `${t.name} 자동 설치 불가: ${t.unavailable_reason}`, manual_hint: t.manual_hint, homepage: t.homepage });
    if (m.install && m.install.state !== 'done' && m.install.state !== 'failed') return json(route, { ok: false, code: 'install_running', error: '다른 도구 설치가 진행 중입니다.' });
    m.seq += 1;
    m.install = { id: m.seq, tool: t.name, state: 'downloading', stage: 'queued', bytes: 0, total: t.size_bytes, error: null, started_unix: 1760000000, finished_unix: null, installed_files: [], audit_warning: null };
    m.polls = 0;
    return json(route, { ok: true, install: m.install });
  });
  await page.route('**/api/tools/install-status', async (route) => { advance(); return json(route, { ok: true, install: m.install }); });
  await page.route('**/api/tools/install-cancel', async (route) => {
    m.cancels += 1;
    const i = m.install;
    if (!i || i.state === 'done' || i.state === 'failed') return json(route, { ok: false, code: 'no_install', error: '진행 중인 도구 설치가 없습니다.' });
    if (i.state === 'extracting') return json(route, { ok: false, code: 'not_cancellable', error: '이 단계는 중단할 수 없습니다.' });
    i.state = 'failed'; i.stage = 'failed'; i.error = '사용자가 설치를 중단했습니다.'; i.finished_unix = 1760000050;
    return json(route, { ok: true, install: i });
  });
  await page.route('**/api/tools', async (route) => json(route, { ok: true, platform: 'windows-x86_64', platform_supported: true, tools: m.tools, install: m.install }));
  return m;
}
