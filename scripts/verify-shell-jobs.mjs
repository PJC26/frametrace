// VM harness for the workstation shell's job UI (assets/app, bundled like
// build.rs): background Recover/Carve jobs (async contract + old synchronous
// responses) rendered into the chip (#jobPanel) and the work sheet
// (#hJobsSheet), hosted-viewer refresh via FTViewer.reload() (never a page
// reload), reload refusal -> 'deferred', cancel, failure visibility, plus the
// preflight panel, checkpoint resume info and shell dialog Escape.
//
//   node scripts/verify-shell-jobs.mjs
//
// No server or browser: a fake DOM, a scripted fetch and virtual timers
// (scripts/lib/shell-vm.mjs). The viewer is an FTViewer mock; there is no
// review iframe and no stage rail any more (docs/ui-rebuild-contract.md §8.1).
import { makeEnv, test, passedCount, assert, markupHas } from './lib/shell-vm.mjs';

// Brings an env to "case open, viewer mounted".
async function openReview(env) {
  await env.advance(2000); // first status poll
  env.store().set({ viewer: { ...env.state().viewer, mounted: true } });
  await env.advance(10);
}
const reloads = (env) => env.viewer.reloadCalls.length;
const toastTexts = (env) => env.$('hToastBox').children.map((c) => (c.children && c.children[0] ? c.children[0].textContent : '')).concat(env.viewer.toasts.map(([m]) => m));

await test('Recover/Carve hidden until a case is open; chip is the entry to the sheet', async () => {
  const env = makeEnv();
  assert.equal(env.$('btnRecoverDeleted').hidden, true);
  assert.equal(env.$('btnCarve').hidden, true);
  assert.equal(env.$('reassembleWrap').hidden, true);
  await env.advance(2000);
  assert.equal(env.$('btnRecoverDeleted').hidden, false);
  assert.equal(env.$('btnCarve').hidden, false);
  assert.equal(env.$('jobPanel').hidden, false, 'idle chip stays reachable while a case is open');
  assert.match(env.$('jobPanel').className, /\bidle\b/);
  assert.match(env.$('jobPanel').innerHTML, /id="hBtnJobs"[^>]*aria-controls="hJobsSheet"/);
  assert.equal(env.state().project.name, 'c1');
  env.status = { ok: true, has_job: false, phase: 'idle', busy: false };
  await env.advance(5100);
  assert.equal(env.$('btnRecoverDeleted').hidden, true, 'no case -> tools hidden again');
  assert.equal(env.$('jobPanel').hidden, true, 'no project -> no chip');
});

await test('async job: progress, elapsed, completion via FTViewer.reload (never a page reload)', async () => {
  const env = makeEnv({ routes: { 'POST /api/recover-deleted': () => ({ json: { ok: true, job: { kind: 'recover' } } }) } });
  await openReview(env);
  env.status = { ...env.status, phase: 'finalizing', busy: true, logs: ['삭제 영상 후보 복구 중… (img @ 오프셋 2048)'] };
  env.$('btnRecoverDeleted').click();
  await env.advance(1500);
  assert.equal(env.$('jobPanel').hidden, false);
  assert.match(env.$('jobPanel').className, /\brunning\b/);
  assert.match(env.$('jobTitle').textContent, /삭제 영상 후보 복구 진행 중/);
  assert.match(env.$('jobDetail').textContent, /오프셋 2048/);
  assert.equal(env.$('btnJobCancel').hidden, false);
  assert.equal(env.$('btnRecoverDeleted').disabled, true);
  assert.equal(env.$('btnCarve').disabled, true);
  await env.advance(64000);
  assert.match(env.$('jobElapsed').textContent, /^경과 1:0\d$/);
  assert.equal(reloads(env), 0, 'no refresh while running');
  env.status = { ...env.status, phase: 'review-ready', busy: false, logs: ['복구 완료'] };
  await env.advance(1300 + 5100);
  assert.equal(reloads(env), 1);
  assert.equal(env.viewer.reloadCalls[0].keepUnsaved, true);
  assert.match(env.$('jobTitle').textContent, /완료/);
  assert.match(env.$('jobDetail').textContent, /뷰어에 결과를 반영/);
  assert.equal(env.ctx.location.reloads, 0, 'the page itself is never reloaded');
  assert.equal(env.$('btnRecoverDeleted').disabled, false);
  assert.equal(env.$('btnJobCancel').hidden, true);
});

await test('viewer refuses the reload (unsaved marks) -> deferred, retry via btnJobReload', async () => {
  const env = makeEnv({ routes: { 'POST /api/recover-deleted': () => ({ json: { ok: true, job: 'j1' } }) } });
  await openReview(env);
  env.viewer.reloadResult = 'refused';
  env.status = { ...env.status, busy: true, phase: 'finalizing' };
  env.$('btnRecoverDeleted').click();
  await env.advance(1300);
  env.status = { ...env.status, busy: false, phase: 'review-ready' };
  await env.advance(1300 + 5100);
  assert.equal(reloads(env), 1);
  assert.match(env.$('jobPanel').className, /deferred/);
  assert.equal(env.$('btnJobReload').hidden, false);
  env.$('btnJobReload').click(); // still refused
  await env.flush();
  assert.equal(reloads(env), 2);
  assert.equal(env.$('btnJobReload').hidden, false, 'stays until the viewer takes the data');
  env.viewer.reloadResult = 'reloaded';
  env.$('btnJobReload').click();
  await env.advance(50);
  assert.equal(reloads(env), 3);
  assert.equal(env.$('btnJobReload').hidden, true);
  assert.match(env.$('jobPanel').className, /\bok\b/);
  assert.equal(env.ctx.location.reloads, 0);
});

await test('cancel: confirm -> /api/cancel -> cancelled outcome, buttons re-enabled', async () => {
  const env = makeEnv({ routes: { 'POST /api/carve': () => ({ json: { ok: true, job: { kind: 'carve' } } }), 'POST /api/cancel': () => ({ json: { ok: true } }) } });
  await openReview(env);
  env.status = { ...env.status, busy: true, phase: 'finalizing', logs: ['시그니처 카빙 실행 중…'] };
  env.$('btnCarve').click();
  await env.answerDialog(true); // carve confirm
  await env.advance(1300);
  env.$('btnJobCancel').click();
  await env.answerDialog(true);
  assert.ok(env.calls.some((c) => c.method === 'POST' && c.url === '/api/cancel'));
  assert.match(env.$('jobTitle').textContent, /중단 요청 중/);
  assert.equal(env.$('btnJobCancel').disabled, true);
  env.status = { ...env.status, busy: false, phase: 'review-ready', error: '사용자 요청으로 중단되었습니다' };
  await env.advance(1300 + 5100);
  assert.match(env.$('jobTitle').textContent, /이미지 카빙 중단됨/);
  assert.match(env.$('jobPanel').className, /cancelled/);
  assert.equal(env.$('btnCarve').disabled, false);
  assert.equal(reloads(env), 0, 'no refresh after cancel');
});

// ---- real server contract (src/serve/jobs.rs): /api/status `job` ----
const sjob = (over = {}) => ({ kind: 'carve', state: 'running', started_unix: 1_700_000_100, finished_unix: null,
  elapsed_secs: 75, stage: 1, stages: 2, stage_label: '시그니처 카빙', error: null, ...over });
const carveRoutes = (extra = {}) => ({
  'POST /api/carve': () => ({ json: { ok: true, job: { kind: 'carve', started_unix: 1_700_000_100 } } }),
  'POST /api/cancel': () => ({ json: { ok: true } }),
  ...extra,
});

await test('server job: stage + elapsed from st.job, done -> viewer refresh', async () => {
  const env = makeEnv({ routes: carveRoutes() });
  await openReview(env);
  // An earlier job of another kind in st.job must not be taken for ours.
  env.status = { ...env.status, phase: 'finalizing', busy: true, job_kind: 'carve', logs: ['시그니처 카빙 실행 중… (raw)'],
    job: sjob({ kind: 'recover-deleted', state: 'done', started_unix: 1_600_000_000 }) };
  env.$('btnCarve').click();
  await env.answerDialog(true);
  await env.advance(1300);
  assert.match(env.$('jobTitle').textContent, /이미지 카빙 진행 중/, 'stale job of another kind ignored');
  env.status = { ...env.status, job: sjob() };
  await env.advance(1300);
  assert.match(env.$('jobDetail').textContent, /단계 1\/2: 시그니처 카빙 · 시그니처 카빙 실행 중/);
  assert.match(env.$('jobElapsed').textContent, /^경과 1:1\d$/);
  env.status = { ...env.status, job: sjob({ stage: 2, stage_label: '리뷰 생성', elapsed_secs: 80 }) };
  await env.advance(1300);
  assert.match(env.$('jobDetail').textContent, /단계 2\/2: 리뷰 생성/);
  // busy already dropped but st.job still running: not finished yet.
  env.status = { ...env.status, busy: false, phase: 'review-ready', job_kind: null };
  await env.advance(1300 + 5100);
  assert.match(env.$('jobTitle').textContent, /진행 중/);
  env.status = { ...env.status, job: sjob({ state: 'done', stage: 2, finished_unix: 1_700_000_190, elapsed_secs: 90 }) };
  await env.advance(1300 + 5100);
  assert.equal(reloads(env), 1);
  assert.match(env.$('jobTitle').textContent, /이미지 카빙 완료/);
  assert.match(env.$('jobElapsed').textContent, /^경과 1:3\d$/);
});

await test('server job: cancelled read from state (no keyword needed), not shown as failed', async () => {
  const env = makeEnv({ routes: carveRoutes() });
  await openReview(env);
  env.status = { ...env.status, phase: 'finalizing', busy: true, job: sjob() };
  env.$('btnCarve').click();
  await env.answerDialog(true);
  await env.advance(1300);
  env.$('btnJobCancel').click();
  await env.answerDialog(true);
  assert.ok(env.calls.some((c) => c.method === 'POST' && c.url === '/api/cancel'));
  env.status = { ...env.status, busy: false, phase: 'review-ready', error: null,
    job: sjob({ state: 'cancelled', finished_unix: 1_700_000_180, error: '시그니처 카빙 작업을 멈췄습니다 — 결과가 리뷰에 반영되지 않았을 수 있습니다.' }) };
  await env.advance(1300 + 5100);
  assert.match(env.$('jobTitle').textContent, /이미지 카빙 중단됨/);
  assert.match(env.$('jobPanel').className, /cancelled/);
  assert.match(env.$('jobDetail').textContent, /결과가 리뷰에 반영되지 않았을 수 있습니다/);
  assert.equal(env.$('btnCarve').disabled, false);
  assert.equal(reloads(env), 0);
});

await test('server job: failure whose text says "cancel" is still a failure, shown in chip, sheet and toast', async () => {
  const env = makeEnv({ routes: carveRoutes() });
  await openReview(env);
  env.status = { ...env.status, phase: 'finalizing', busy: true, job: sjob() };
  env.$('btnCarve').click();
  await env.answerDialog(true);
  await env.advance(1300);
  env.status = { ...env.status, busy: false, phase: 'review-ready', error: 'carve-file: cancelled read (I/O error)',
    job: sjob({ state: 'failed', error: 'carve-file: cancelled read (I/O error)' }) };
  await env.advance(1300 + 5100);
  assert.match(env.$('jobTitle').textContent, /이미지 카빙 실패/);
  assert.match(env.$('jobDetail').textContent, /I\/O error/);
  assert.match(env.$('jobPanel').className, /failed/);
  assert.equal(env.$('jobsErr').hidden, false, 'sheet shows the failure');
  assert.match(env.$('jobsErr').innerHTML, /I\/O error/);
  assert.ok(toastTexts(env).some((t) => /이미지 카빙 실패/.test(t)), 'sticky error toast: ' + toastTexts(env));
});

await test('shell reloaded mid-job adopts the running server job (progress + cancel)', async () => {
  const env = makeEnv({ routes: carveRoutes(), status: { phase: 'finalizing', busy: true, job_kind: 'recover-deleted',
    job: sjob({ kind: 'recover-deleted', stage_label: '삭제 영상 후보 복구', elapsed_secs: 130 }) } });
  await env.advance(2000);
  assert.equal(env.$('jobPanel').hidden, false);
  assert.match(env.$('jobTitle').textContent, /삭제 영상 후보 복구 진행 중/);
  assert.match(env.$('jobElapsed').textContent, /^경과 2:1\d$/);
  assert.equal(env.$('btnJobCancel').hidden, false);
  assert.equal(env.$('btnRecoverDeleted').disabled, true);
  env.status = { ...env.status, busy: false, phase: 'review-ready', job: sjob({ kind: 'recover-deleted', state: 'done' }) };
  await env.advance(1600 + 5100);
  assert.match(env.$('jobTitle').textContent, /완료/);
});

await test('carve sends ?reassemble=1 when the checkbox is on', async () => {
  const env = makeEnv({ routes: { 'POST /api/carve': () => ({ json: { ok: true, job: 1 } }) } });
  await openReview(env);
  env.$('chkReassemble').checked = true;
  env.$('btnCarve').click();
  await env.answerDialog(true);
  await env.advance(10);
  assert.ok(env.calls.some((c) => c.url === '/api/carve?reassemble=1'));
});

await test('old synchronous server: elapsed + cancel while outstanding, refresh on {ok:true}', async () => {
  let resolveSync;
  const env = makeEnv({ routes: { 'POST /api/recover-deleted': () => new Promise((r) => { resolveSync = r; }) } });
  await openReview(env);
  env.status = { ...env.status, busy: true, phase: 'finalizing', logs: ['삭제 영상 후보 복구 중…'] };
  env.$('btnRecoverDeleted').click();
  await env.advance(12000);
  assert.equal(env.$('jobPanel').hidden, false);
  assert.match(env.$('jobElapsed').textContent, /^경과 0:1\d$/);
  assert.equal(env.$('btnJobCancel').hidden, false);
  assert.match(env.$('jobTitle').textContent, /진행 중/);
  env.status = { ...env.status, busy: false, phase: 'review-ready' };
  // Status says idle but the sync request is still out: not finished yet.
  await env.advance(5000);
  assert.match(env.$('jobTitle').textContent, /진행 중/);
  resolveSync({ json: { ok: true } });
  await env.advance(10);
  assert.equal(reloads(env), 1);
  assert.match(env.$('jobTitle').textContent, /완료/);
});

await test('old synchronous server failure -> visible error, retry possible, dismissible', async () => {
  const env = makeEnv({ routes: { 'POST /api/recover-deleted': () => ({ json: { ok: false, error: '파일시스템 조사 결과가 없습니다 — 먼저 E01/이미지 분석을 실행하십시오.' } }) } });
  await openReview(env);
  env.$('btnRecoverDeleted').click();
  await env.advance(10);
  assert.match(env.$('jobTitle').textContent, /실패/);
  assert.match(env.$('jobDetail').textContent, /파일시스템 조사 결과가 없습니다/);
  assert.match(env.$('jobsNote').textContent, /다시 시도/);
  assert.equal(env.$('btnRecoverDeleted').disabled, false);
  assert.equal(env.$('btnJobClose').hidden, false);
  env.$('btnJobClose').click();
  assert.match(env.$('jobPanel').className, /\bidle\b/, 'dismissed -> back to the idle entry');
});

await test('btnNew hands over to the project popover action (one guarded flow)', async () => {
  const env = makeEnv();
  await openReview(env);
  assert.equal(env.$('btnNew').hidden, false);
  env.$('btnNew').click();
  await env.flush();
  assert.equal(env.$('hProjectNew').clicks, 1);
  assert.equal(env.ctx.location.reloads, 0);
});

await test('preflight: render (estimate, inside-source, escaping) and 404 degrade', async () => {
  const env = makeEnv();
  const html = env.ev('renderPreflight')({ sampled_bytes: 500 * 1024 ** 3, est_files: 12345, free_bytes: 1024 ** 3, case_inside_source: true, warnings: ['<b>x</b>'] }, { hash: true });
  assert.match(html, /케이스 폴더가 증거 소스 안에/);
  assert.match(html, /크기 500\.0 GB/);
  assert.match(html, /12,345/);
  assert.match(html, /해시 약 \d+분 – 약 1시간 \d+분/);
  assert.match(html, /부족할 수 있습니다/);
  assert.match(html, /&lt;b&gt;x&lt;\/b&gt;/);
  assert.doesNotMatch(html, /<b>x<\/b>/);
  assert.match(env.ev('renderPreflight')({ sampled_bytes: 1024 ** 3 }, { hash: false }), /해시 생략/);
  env.$('sourcePath').value = '/evidence/src';
  const res = await env.ev('runPreflight()');
  assert.equal(res, null);
  assert.equal(env.ev('preflightUnsupported'), true);
  assert.equal(env.$('preflight').hidden, true);
});

await test('preflight: shown before Start; server-reported case_inside_source asks first', async () => {
  const env = makeEnv({ routes: {
    'POST /api/preflight': () => ({ json: { ok: true, sampled_bytes: 2 * 1024 ** 3, est_files: 40, free_bytes: 900 * 1024 ** 3, case_inside_source: true, warnings: [] } }),
    // Start auto-detects the evidence kind first: D:\BLACKBOX is a folder.
    'GET /api/browse': () => ({ json: { ok: true, exists: true, is_dir: true, kind: 'folder', entries: [] } }),
  } });
  env.status = { ok: true, has_job: false, phase: 'idle', busy: false };
  await env.advance(1500);
  env.$('sourcePath').value = 'D:\\BLACKBOX';
  env.$('caseDir').value = 'E:\\link-into-source';
  env.ev('schedulePreflight()');
  await env.advance(700);
  assert.equal(env.$('preflight').hidden, false);
  assert.match(env.$('preflight').innerHTML, /크기 2\.0 GB/);
  env.$('btnStart').click();
  await env.flush();
  assert.equal(env.$('dlgBackdrop').hidden, false);
  assert.match(env.$('dlgTitle').textContent, /증거 소스 안에/);
  await env.answerDialog(false);
  assert.equal(env.calls.some((c) => c.url === '/api/start'), false, 'start must wait for the decision');
});

await test('case-info: N/M resume text; 404 falls back to the checkpoint probe', async () => {
  const env = makeEnv({ routes: { 'GET /api/case-info': () => ({ json: { ok: true, checkpoint: { done: 40, total: 100 }, has_review: false } }) } });
  const info = await env.ev('caseResumeInfo("/cases/c1")');
  assert.equal(info.checkpoint, true);
  assert.equal(info.done, 40);
  assert.equal(info.total, 100);
  assert.match(env.ev('resumeText')(info), /이전 분석 40\/100 진행됨 \(40%\)/);
  assert.ok(env.calls.some((c) => c.url === '/api/case-info?case_dir=%2Fcases%2Fc1'));

  // No index yet: the server reports checkpoint.total = null → "N건 진행됨"
  // without inventing a total (Number(null) would read as 0).
  const noTotal = makeEnv({ routes: { 'GET /api/case-info': () => ({ json: { ok: true, checkpoint: { done: 37, total: null }, has_review: false } }) } });
  const info3 = await noTotal.ev('caseResumeInfo("/cases/c1")');
  assert.equal(info3.checkpoint, true);
  assert.equal(info3.done, 37);
  assert.ok(Number.isNaN(info3.total));
  const text3 = noTotal.ev('resumeText')(info3);
  assert.match(text3, /이전 분석 37건 진행됨/);
  assert.doesNotMatch(text3, /\/|%/);

  const old = makeEnv({ routes: { 'GET /api/browse': (_b, url) => ({ json: { ok: true, is_file: url.includes('scan-progress'), is_case: url.endsWith('c1') } }) } });
  const info2 = await old.ev('caseResumeInfo("/cases/c1")');
  assert.equal(info2.checkpoint, true);
  assert.equal(info2.done, undefined); // no N/M from the probe
  assert.match(old.ev('resumeText')(info2), /체크포인트가 있습니다/);
  assert.equal(old.ev('caseInfoUnsupported'), true);
  // validatePaths wires it into the resume box.
  old.$('caseDir').value = '/cases/c1';
  await old.ev('validatePaths()');
  await old.flush();
  assert.equal(old.$('resumeBox').hidden, false);
  assert.match(old.$('resumeText').textContent, /체크포인트/);
});


await test('shell dialog: Escape works at document level even after focus left the dialog', async () => {
  const env = makeEnv();
  const p = env.ev('askDialog')({ title: 't', html: '<p>x</p>' });
  await env.flush();
  assert.equal(env.$('dlgBackdrop').hidden, false);
  env.ctx.document.activeElement = null; // backdrop click moved focus to <body>
  const ev = env.docKey('Escape');
  assert.equal(ev.prevented, true);
  assert.equal(await p, false);
  assert.equal(env.$('dlgBackdrop').hidden, true);
  assert.equal(env.docKey('Escape').prevented, false, 'ignored while no dialog is open');
});


// ---- T2-dependent (shell chrome rebuilt by T2): enabled once its markup lands ----
if (markupHas('hProjectBtn')) {
  await test('project button shows the open case name', async () => {
    const env = makeEnv();
    await env.advance(2000);
    assert.equal(env.$('hProjectName').textContent, 'c1');
    assert.equal(env.$('hProjectBtn').disabled, false);
  });
} else console.log('SKIP project button text (needs T2 markup: hProjectBtn)');
if (markupHas('hBtnTheme') && markupHas('hBtnLang')) {
  await test('theme + language buttons drive FTViewer', async () => {
    const env = makeEnv();
    const calls = [];
    env.ctx.FTViewer.setTheme = (t) => calls.push(['theme', t]);
    env.ctx.FTViewer.setLocale = (l) => calls.push(['locale', l]);
    env.ctx.FTViewer.getTheme = () => 'light';
    env.ctx.FTViewer.getLocale = () => 'ko';
    await env.advance(2000);
    env.$('hBtnTheme').click();
    env.$('hBtnLang').click();
    assert.ok(calls.some((c) => c[0] === 'theme' && c[1] === 'dark'), JSON.stringify(calls));
    assert.ok(calls.some((c) => c[0] === 'locale' && c[1] === 'en'), JSON.stringify(calls));
  });
} else console.log('SKIP theme/language buttons (needs T2 markup: hBtnTheme/hBtnLang)');

await test('no uncaught errors from timers', async () => {
  const env = makeEnv();
  await env.advance(30000);
  assert.deepEqual(env.errors.map(String), []);
  assert.deepEqual(env.consoleErrors, []);
});

console.log(process.exitCode ? 'verify-shell-jobs: FAILED' : `verify-shell-jobs: ${passedCount()} passed`);
