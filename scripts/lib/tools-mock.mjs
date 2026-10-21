// A mocked tools API in the real track S shapes (docs/PLAN_TOOLS_2026-10-20.md
// "§S as implemented", docs/ui-rebuild-contract.md §28) for browser
// harnesses (verify-layout, check-a11y):
//   GET  /api/tools                 {ok, platform, platform_supported, tools:[T], install: J|null, now_unix}
//   POST /api/tools/install {name}  {ok:true, install: J} | {ok:false, code, error[, manual_hint, homepage]}
//   GET  /api/tools/install-status  {ok:true, install: J|null, now_unix}
//   POST /api/tools/install-cancel {id?}  {ok:true, install: J} | {ok:false, code:"no_install"|"not_cancellable"|"stale_job"}
//   POST /api/cancel (opts.routeCancel; apiCancelReply)  {ok:true, cancelled:"tool-install", requested, install: J} | {ok:false, code:"not_cancellable"}
// T carries notice / notice_en (option C distribution notice; null for
// tools that cannot be installed), shadowed_by {<binary>: path|null},
// rollback_incomplete / rollback_note / rollback_backups (Fix round 13 L2) and,
// for ffmpeg once installed, encoders {…, trial_failed: []}; J carries
// staging_left and age_s (seconds since it finished, by the server's clock).
// Nothing is downloaded: the install walks downloading → verifying →
// extracting (stage extract, then install on its last poll) → done over the
// next status polls. Like the real server, a cancel is refused
// (`not_cancellable`) only in stage install|venv|pip; an accepted cancel
// replies with the job unchanged and the NEXT poll shows it `failed` with
// error "install cancelled" and `cancelled: true`.
const FF_ZIP = 'ffmpeg-n8.1.3-9-g29e619e767-win64-lgpl-8.1.zip';
export const TOOLS_FIXTURE = () => ([
  {
    name: 'ffmpeg', title: 'FFmpeg', label: 'FFmpeg', kind: 'archive', required: true, on_demand: false,
    version: 'n8.1.3-9-g29e619e767', pinned_version: 'n8.1.3-9-g29e619e767', license: 'LGPL-3.0-or-later (FFmpeg); the BtbN win64-lgpl binary also contains GPL-2.0-only (zvbi), GPL-2.0-or-later (FFTW3), Apache-2.0 and BSD/MIT components', homepage: 'https://ffmpeg.org/',
    notice: '이 파일은 BtbN FFmpeg-Builds(github.com)에서 사용자가 직접 내려받습니다. FrameTrace는 이 바이너리를 배포하지 않으며 소스 제공·특허 사용권도 제공하지 않습니다. 빌드 이름은 lgpl이지만 GPL-2.0-only(zvbi)·LGPL-3.0·Apache-2.0 코드가 섞여 있어, FrameTrace는 이 파일을 다른 사람에게 넘겨도 되는지 보증하지 않습니다. 다른 PC에는 복사하지 말고 각자 BtbN에서 받으십시오. 이 라이선스들은 내 PC에서의 사용을 제한하지 않습니다(특허는 별개).',
    notice_en: 'You download this file yourself from BtbN FFmpeg-Builds (github.com). FrameTrace does not distribute it, offers no source for it and grants no patent licence. Although the build is named lgpl, it mixes GPL-2.0-only (zvbi), LGPL-3.0 and Apache-2.0 code, so FrameTrace makes no assurance that the file may be passed on to others. Do not copy it to other PCs; let each user download it from BtbN. These licenses do not restrict use on your own PC (patents are a separate matter).',
    source_url: 'https://github.com/FFmpeg/FFmpeg/tree/29e619e767', provides: ['ffmpeg', 'ffprobe'], status: 'missing', installed: false,
    paths: { ffmpeg: null, ffprobe: null }, shadowed_by: { ffmpeg: null, ffprobe: null }, rollback_incomplete: false, rollback_note: null, rollback_backups: [], installable: true, unavailable_reason: null, not_installable_reason: null,
    manual_hint: 'https://github.com/BtbN/FFmpeg-Builds/releases 또는 https://ffmpeg.org/download.html 에서 Windows 빌드를 받아 ffmpeg.exe·ffprobe.exe를 tools/bin에 복사하십시오.',
    source_host: 'github.com', size_bytes: 4194304, encoders: null,
    download: { file_name: FF_ZIP, url: 'https://github.com/BtbN/FFmpeg-Builds/releases/download/autobuild-2026-09-30-13-08/' + FF_ZIP, host: 'github.com', size: 4194304, sha256: '4a7642b2264c03e8a0ce8a3825b933ee5580656f45695a086fe7e294045ffc0a', checksum_source: 'https://github.com/BtbN/FFmpeg-Builds/releases/download/autobuild-2026-09-30-13-08/checksums.sha256' },
  },
  {
    name: 'libewf', title: 'libewf (E01)', label: 'libewf (E01)', kind: 'archive', required: false, on_demand: false,
    version: '20260924', pinned_version: '20260924', license: 'LGPL-3.0-or-later', notice: null, notice_en: null, homepage: 'https://github.com/libyal/libewf',
    source_url: 'https://github.com/libyal/libewf/releases/tag/20260924', provides: ['ewfinfo', 'ewfverify', 'ewfexport'], status: 'missing', installed: false,
    paths: { ewfinfo: null, ewfverify: null, ewfexport: null }, shadowed_by: { ewfinfo: null, ewfverify: null, ewfexport: null }, rollback_incomplete: false, rollback_note: null, rollback_backups: [], installable: false,
    unavailable_reason: 'The official release (https://github.com/libyal/libewf/releases/tag/20260924) publishes only a source tarball and its PGP signature, no Windows binaries.',
    not_installable_reason: 'The official release (https://github.com/libyal/libewf/releases/tag/20260924) publishes only a source tarball and its PGP signature, no Windows binaries.',
    manual_hint: 'libewf는 공식 Windows 실행 파일이 없습니다 — scripts/build-libewf-tools.ps1로 소스에서 빌드하거나 직접 설치한 뒤 ewfinfo·ewfverify·ewfexport(와 DLL)를 tools/bin에 복사하십시오: https://github.com/libyal/libewf',
    source_host: null, size_bytes: null, encoders: null, download: null,
  },
  {
    name: 'sleuthkit', title: 'The Sleuth Kit (삭제 파일 복구)', label: 'The Sleuth Kit (삭제 파일 복구)', kind: 'archive', required: false, on_demand: false,
    version: '4.15.0', pinned_version: '4.15.0', license: 'IPL-1.0 / CPL-1.0', notice: null, notice_en: null, homepage: 'https://www.sleuthkit.org/sleuthkit/',
    source_url: 'https://github.com/sleuthkit/sleuthkit/releases/tag/sleuthkit-4.15.0', provides: ['mmls', 'fls', 'icat'], status: 'missing', installed: false,
    paths: { mmls: null, fls: null, icat: null }, shadowed_by: { mmls: null, fls: null, icat: null }, rollback_incomplete: false, rollback_note: null, rollback_backups: [], installable: false,
    unavailable_reason: 'The maintainers publish a PGP signature (.asc), not a checksum file, and the archive\'s file list could not be verified without downloading it.',
    not_installable_reason: 'The maintainers publish a PGP signature (.asc), not a checksum file, and the archive\'s file list could not be verified without downloading it.',
    manual_hint: 'sleuthkit-4.15.0-win32.zip을 공식 배포처에서 받아 PGP 서명(.asc)을 확인한 뒤 mmls·fls·icat과 함께 들어 있는 DLL을 tools/bin에 복사하십시오: https://www.sleuthkit.org/sleuthkit/download.php',
    source_host: null, size_bytes: null, encoders: null, download: null,
  },
  {
    name: 'deepfake-lens', title: 'deepfake-lens (합성 영상 스크리닝)', label: 'deepfake-lens (합성 영상 스크리닝)', kind: 'pip', required: false, on_demand: true,
    version: null, pinned_version: null, license: null, notice: null, notice_en: null, homepage: null, source_url: null, provides: ['deepfake-lens'], status: 'missing', installed: false,
    paths: { 'deepfake-lens': null }, shadowed_by: { 'deepfake-lens': null }, rollback_incomplete: false, rollback_note: null, rollback_backups: [], installable: false,
    unavailable_reason: 'deepfake-lens is not published on PyPI, so no hash-pinned requirements file can be built.',
    not_installable_reason: 'deepfake-lens is not published on PyPI, so no hash-pinned requirements file can be built.',
    manual_hint: 'deepfake-lens는 PyPI에 공개되지 않아 자동 설치할 수 없습니다 — 받은 패키지를 직접 설치(pip install <경로>)하고 FRAMETRACE_DEEPFAKE_LENS에 deepfake-lens.exe 경로를 지정하십시오.',
    source_host: 'pypi.org', size_bytes: null, encoders: null, download: null,
  },
]);

const STAGE = { downloading: 'download', verifying: 'verify', extracting: 'extract', done: 'done', failed: 'failed' };
// src/tool_install/mod.rs INSTALL_CANCELLED / the non-cancellable stages.
export const INSTALL_CANCELLED = 'install cancelled';
const LOCKED_STAGES = ['install', 'venv', 'pip'];
const nowUnix = () => Math.floor(Date.now() / 1000);
const json = (route, body) => route.fulfill({ status: 200, contentType: 'application/json', body: JSON.stringify(body) });
// J as the server sends it: age_s by the server's clock (null while running).
const withAge = (i) => (i ? { ...i, age_s: i.finished_unix ? Math.max(0, nowUnix() - i.finished_unix) : null } : null);
// GET /api/tools encoders of an installed ffmpeg without libx264 (the BtbN
// build the installer fetches; src/ffmpeg_encoders.rs to_json).
const LGPL_ENCODERS = { probed: true, libx264: false, h264: 'h264_mf', aac: 'aac', avi_audio: 'mp2', note: 'libx264 없음 — H.264는 h264_mf 사용', trial_failed: [], trial_timed_out: [] };

// POST /api/cancel (the global cancel of [종료]) while the mock state `m`
// has an install running — src/serve/system.rs api_cancel, Fix round 11 M1:
// it holds `busy`, so the cancel goes to the install's own latch
// ({ok:true, cancelled:"tool-install"}, the job ends `cancelled` on the next
// poll) or is refused in the move / venv / pip stage
// ({ok:false, code:"not_cancellable"}; the install then runs to its end).
// `null` when no install runs (the caller answers for other jobs).
export function apiCancelReply(m) {
  const i = m.install;
  if (!i || i.state === 'done' || i.state === 'failed') return null;
  if (LOCKED_STAGES.includes(i.stage)) return { ok: false, code: 'not_cancellable', error: '지금 단계(파일 이동·Python 환경 구성)는 취소할 수 없습니다 — 끝날 때까지 기다리십시오.', job_kind: 'tool-install', install: withAge(i) };
  // Fix round 15: the worker already ended (stage done / failed) but still
  // holds `busy` while it publishes — acknowledged, nothing asked.
  if (i.stage === 'done' || i.stage === 'failed') return { ok: true, cancelled: 'tool-install', job_kind: 'tool-install', requested: false, install: withAge(i), clips_cancelled: 0, clips_running: 0 };
  m.cancelRequested = true;
  return { ok: true, cancelled: 'tool-install', job_kind: 'tool-install', requested: true, install: withAge(i), clips_cancelled: 0, clips_running: 0 };
}

// opts.steps: status polls per install stage (default 2); opts.fail: an
// error string → the job ends `failed`; opts.tools: a fixture override;
// opts.install: a job already running when the page loads (a refresh).
// Returns the live mock state { posts: [{name}], cancels, tools, install,
// cancelRequested } — set `cancelRequested = true` to stop the job from
// "another tab / the CLI".
export async function routeTools(page, opts = {}) {
  const steps = opts.steps || 2;
  const m = { posts: [], cancels: 0, tools: opts.tools || TOOLS_FIXTURE(), install: opts.install || null, polls: 0, seq: 0, cancelRequested: false };
  if (m.install && !('cancelled' in m.install)) m.install.cancelled = false;
  if (m.install && !('staging_left' in m.install)) m.install.staging_left = null;
  const advance = () => {
    const i = m.install;
    if (!i || i.state === 'done' || i.state === 'failed') return;
    if (m.cancelRequested) {
      m.cancelRequested = false;
      Object.assign(i, { state: 'failed', stage: 'failed', error: INSTALL_CANCELLED, cancelled: true, finished_unix: nowUnix() });
      return;
    }
    m.polls += 1;
    const t = m.tools.find((x) => x.name === i.tool);
    if (i.state === 'downloading') {
      i.bytes = Math.min(i.total || 0, Math.round((i.total || 0) * m.polls / (steps + 1)));
      if (m.polls > steps) { i.state = 'verifying'; m.polls = 0; }
    } else if (i.state === 'verifying') {
      if (m.polls > steps) { i.state = 'extracting'; m.polls = 0; }
    } else if (i.state === 'extracting') {
      if (m.polls > steps) {
        i.finished_unix = nowUnix();
        if (opts.fail) { i.state = 'failed'; i.error = opts.fail; } else {
          i.state = 'done'; t.installed = true; t.status = 'installed';
          for (const k of Object.keys(t.paths)) t.paths[k] = 'C:\\FrameTrace\\tools\\bin\\' + k + '.exe';
          if (t.name === 'ffmpeg') t.encoders = { ...LGPL_ENCODERS };
          i.installed_files = Object.values(t.paths);
        }
      }
    }
    // Files are moved into place (non-cancellable) on extracting's last poll.
    i.stage = i.state === 'extracting' && m.polls >= steps ? 'install' : STAGE[i.state];
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
    m.install = { id: m.seq, tool: t.name, state: 'downloading', stage: 'queued', bytes: 0, total: t.size_bytes, error: null, cancelled: false, started_unix: nowUnix(), finished_unix: null, installed_files: [], audit_warning: null, staging_left: null };
    m.polls = 0;
    m.cancelRequested = false;
    return json(route, { ok: true, install: withAge(m.install) });
  });
  await page.route('**/api/tools/install-status', async (route) => { advance(); return json(route, { ok: true, install: withAge(m.install), now_unix: nowUnix() }); });
  await page.route('**/api/tools/install-cancel', async (route) => {
    m.cancels += 1;
    const i = m.install;
    let body = {};
    try { body = JSON.parse(route.request().postData() || '{}'); } catch { /* bad body */ }
    // A cancel naming another job (a late click in a tab showing an older
    // install) never stops the current one.
    if (i && body.id !== undefined && body.id !== null && String(body.id) !== String(i.id)) return json(route, { ok: false, code: 'stale_job', error: '그 설치는 이미 끝났습니다 — 지금 진행 중인 설치는 중단하지 않았습니다.', install: withAge(i) });
    if (!i || i.state === 'done' || i.state === 'failed') return json(route, { ok: false, code: 'no_install', error: '진행 중인 도구 설치가 없습니다.' });
    if (LOCKED_STAGES.includes(i.stage)) return json(route, { ok: false, code: 'not_cancellable', error: '지금 단계(파일 이동·Python 환경 구성)는 취소할 수 없습니다 — 끝날 때까지 기다리십시오.', install: withAge(i) });
    // Honoured by the worker at its next check: the job ends on the next poll.
    m.cancelRequested = true;
    return json(route, { ok: true, install: withAge(i) });
  });
  // opts.routeCancel: also answer the global POST /api/cancel ([종료]).
  if (opts.routeCancel) await page.route('**/api/cancel', async (route) => json(route, apiCancelReply(m) || { ok: false, error: '진행 중인 작업이 없습니다.' }));
  await page.route('**/api/tools', async (route) => json(route, { ok: true, platform: 'windows-x86_64', platform_supported: true, tools: m.tools, install: withAge(m.install), now_unix: nowUnix() }));
  return m;
}
