// VM harness for the inline analysis progress (docs/ui-rebuild-contract.md
// §8.2, track T3): the top-strip chip (#jobPanel) and the right-hand work
// sheet (#hJobsSheet), driven only by scripted /api/status replies.
//
//   node scripts/verify-progress.mjs
//
//   1. a status sequence running -> review-partial (busy) -> review-ready
//      drives the chip's class + text ("색인 n/m · 단계 2/5 · 경과 m:ss")
//   2. the sheet: 5-step track, append-only log, opens once automatically
//      on the first analysis start (never again after the examiner closes it)
//   3. 분석 중단 -> confirm -> POST /api/cancel exactly once
//   4. recover / carve / clip-export jobs join the SAME chip
//      (priority pipeline > case job > clip export)
//   5. finished work: FTViewer.reload() once; refused -> 'deferred' +
//      btnJobReload; enrichment finishing never reloads behind the examiner
//   6. polling cadence: 1 s while busy, 5 s while idle
//   7. failures are never silent (chip + sheet + sticky toast)
//   8. progress never blocks review: the chip is a plain button and nothing
//      here disables the viewer or hides the grid
//
// No server or browser (scripts/lib/shell-vm.mjs): fake DOM, scripted fetch,
// virtual timers, FTViewer/FTStore mocks (T2/T5 are not required).
import { makeEnv, test, passedCount, assert } from './lib/shell-vm.mjs';

const NAMES = ['케이스 준비', '소스 등록', '스캔 · 색인', '재생성 검증', '리뷰 생성'];
const running = (over = {}) => ({
  ok: true, has_job: true, phase: 'running', busy: true, case_dir: '/cases/c2', step_names: NAMES,
  steps: ['done', 'running', 'pending', 'pending', 'pending'], index_generation: 2,
  logs: ['소스 등록 중…'], error: null, progress: { done: 10, total: 40, eta_secs: 30, elapsed_secs: 3, job_type: 'scan' }, ...over,
});
const partial = (over = {}) => running({ phase: 'review-partial', steps: ['done', 'done', 'done', 'running', 'pending'],
  enrichment: { step: 3, label: '해시 계산', stage: 1, stages: 2, done: 120, total: 400, unit: 'units' }, progress: null,
  logs: ['소스 등록 중…', '1단계 색인 완료'], ...over });
const ready = (over = {}) => running({ phase: 'review-ready', busy: false, steps: ['done', 'done', 'done', 'done', 'done'], progress: null, enrichment: null,
  logs: ['소스 등록 중…', '1단계 색인 완료', '리뷰 생성 완료'], ...over });
const idle = { ok: true, has_job: false, phase: 'idle', busy: false };
const toastTexts = (env) => env.$('hToastBox').children.map((c) => (c.children && c.children[0] ? c.children[0].textContent : '')).concat(env.viewer.toasts.map(([m]) => m));
const mount = (env, on = true) => env.store().set({ viewer: { ...env.state().viewer, mounted: on } });
const liCount = (env) => (env.$('track').innerHTML.match(/<li[ >]/g) || []).length;

await test('1. chip follows running -> review-partial (busy) -> review-ready', async () => {
  const env = makeEnv({ status: idle });
  await env.advance(1000);
  assert.equal(env.$('jobPanel').hidden, true, 'no project, no chip');
  env.recordsTotal = 4000;
  env.status = running();
  await env.advance(5100); // idle cadence 5 s picks the new run up
  env.ctx.FTViewer && env.viewer.emit('records', { total: 1240, generation: 2 });
  await env.advance(1100);
  assert.match(env.$('jobPanel').className, /\brunning\b/);
  assert.equal(env.$('jobTitle').textContent, '분석 중');
  assert.match(env.$('jobDetail').textContent, /^색인 1,240\/4,000 · 단계 2\/5 소스 등록$/);
  assert.match(env.$('jobElapsed').textContent, /^경과 0:0\d$/);
  await env.advance(65000);
  assert.match(env.$('jobElapsed').textContent, /^경과 1:\d\d$/, 'elapsed keeps counting');
  assert.equal(env.state().phase, 'running');
  assert.equal(env.state().counts.total, 4000);
  // 1st stage done: enrichment (review-partial, busy) — still the same chip
  env.status = partial();
  await env.advance(1100);
  assert.match(env.$('jobPanel').className, /\brunning\b/);
  assert.equal(env.$('jobTitle').textContent, '보강 중');
  assert.match(env.$('jobDetail').textContent, /^해시 계산 1\/2 120\/400 · 색인 1,240\/4,000$/);
  assert.equal(env.state().phase, 'review-partial');
  // done: the integrated shell mounted the viewer at review-partial, so the
  // fresh hashes wait for the examiner ('새 데이터 반영'), never auto-reload.
  env.status = ready();
  await env.advance(1100);
  assert.equal(env.state().phase, 'review-ready');
  assert.equal(env.state().viewer.mounted, true, 'viewer mounted once the index existed');
  assert.match(env.$('jobPanel').className, /\bdeferred\b/);
  assert.equal(env.viewer.reloadCalls.length, 0);
});

await test('2. sheet: five-step track, append-only log, opens once on the first start only', async () => {
  const env = makeEnv({ status: idle });
  await env.advance(1000);
  assert.equal(env.state().panels.jobs, false);
  env.status = running();
  await env.advance(5100);
  assert.equal(env.state().panels.jobs, true, 'first analysis start opens the sheet');
  assert.equal(liCount(env), 5);
  assert.match(env.$('track').innerHTML, /class="running"[^>]*>.*소스 등록/);
  assert.match(env.$('jobsCounts').textContent, /^$/);
  assert.equal(env.$('log').textContent, '소스 등록 중…');
  env.status = running({ logs: ['소스 등록 중…', '파일 40개 발견'] });
  await env.advance(1100);
  assert.equal(env.$('log').textContent, '소스 등록 중…\n파일 40개 발견');
  const kept = env.$('log').children.length;
  env.status = running({ logs: ['소스 등록 중…', '파일 40개 발견'] });
  await env.advance(1100);
  assert.equal(env.$('log').children.length, kept, 'identical poll appends nothing (selection/scroll survive)');
  env.status = running({ logs: ['파일 40개 발견', '메타데이터 추출 중…'] });
  await env.advance(1100);
  assert.equal(env.$('log').textContent, '소스 등록 중…\n파일 40개 발견\n메타데이터 추출 중…', 'overlap-aware append');
  // the examiner closes it: never auto-opens again this session
  env.$('hJobsClose').click();
  assert.equal(env.state().panels.jobs, false);
  env.status = ready();
  await env.advance(1100);
  env.status = idle;
  await env.advance(5100);
  env.status = running({ case_dir: '/cases/c3' });
  await env.advance(5100);
  assert.equal(env.state().panels.jobs, false, 'not re-opened after the examiner closed it');
  // chip click toggles via the shell wiring (T2); here the same wiring as the real one
  env.$('hBtnJobs').click();
  assert.equal(env.state().panels.jobs, true);
});

await test('2b. a page reloaded into a running job does not pop the sheet open', async () => {
  const env = makeEnv({ status: running() });
  await env.advance(3000);
  assert.match(env.$('jobPanel').className, /\brunning\b/);
  assert.equal(env.state().panels.jobs, false);
});

await test('3. 분석 중단 -> confirm -> /api/cancel exactly once', async () => {
  const env = makeEnv({ status: running(), routes: { 'POST /api/cancel': () => ({ json: { ok: true } }) } });
  await env.advance(1200);
  assert.equal(env.$('btnCancel').hidden, false, 'cancel is reachable while the analysis runs');
  assert.equal(env.$('btnCancel').textContent, '분석 중단');
  env.$('btnCancel').click();
  await env.answerDialog(false); // "계속 분석"
  assert.equal(env.calls.filter((c) => c.url === '/api/cancel').length, 0);
  env.$('btnCancel').click();
  await env.answerDialog(true);
  assert.equal(env.calls.filter((c) => c.url === '/api/cancel').length, 1);
  assert.equal(env.$('btnCancel').disabled, true);
  assert.equal(env.$('btnCancel').textContent, '중단 요청 중…');
  env.$('btnCancel').click(); // disabled: no second request
  await env.flush();
  assert.equal(env.calls.filter((c) => c.url === '/api/cancel').length, 1);
  env.status = { ...running(), phase: 'error', busy: false, error: '사용자 요청으로 중단되었습니다' };
  await env.advance(1200);
  assert.match(env.$('jobPanel').className, /\bcancelled\b/);
  assert.equal(env.$('jobTitle').textContent, '분석 중단됨');
  assert.equal(env.$('btnCancel').hidden, true);
  assert.equal(env.$('btnNew').hidden, false);
});

await test('3b. a failed cancel request is reported and the button comes back', async () => {
  const env = makeEnv({ status: running(), routes: { 'POST /api/cancel': () => ({ json: { ok: false, error: '진행 중인 작업이 없습니다.' } }) } });
  await env.advance(1200);
  env.$('btnCancel').click();
  await env.answerDialog(true);
  assert.equal(env.$('btnCancel').disabled, false);
  assert.ok(toastTexts(env).some((t) => /중단 요청 실패.*진행 중인 작업이 없습니다/.test(t)), toastTexts(env).join('|'));
});

await test('4. recover/carve/clip jobs join the same chip; priority pipeline > case job > clip', async () => {
  const clip = (over = {}) => ({ id: 7, kind: 'export-clip', record_id: 'v1', state: 'running', started_unix: 1_700_000_050, elapsed_secs: 12,
    path: 'exports/clip_v1.mp4', bytes_written: 2 * 1024 * 1024, error: null, ...over });
  const sj = (over = {}) => ({ kind: 'carve', state: 'running', started_unix: 1_700_000_100, elapsed_secs: 75, stage: 1, stages: 2, stage_label: '시그니처 카빙', error: null, ...over });
  const env = makeEnv({ status: ready(), routes: { 'POST /api/carve': () => ({ json: { ok: true, job: { kind: 'carve', started_unix: 1_700_000_100 } } }) } });
  await env.advance(1200);
  // clip export alone
  env.status = ready({ clip_jobs: [clip()], clips_running: 1 });
  await env.advance(5100);
  assert.equal(env.$('jobTitle').textContent, '클립 내보내기');
  assert.match(env.$('jobDetail').textContent, /1건 진행 중 · 2\.0 MB/);
  assert.match(env.$('jobPanel').className, /\brunning\b/);
  assert.equal(env.$('jobsClips').hidden, false);
  assert.match(env.$('jobsClipList').innerHTML, /clip_v1\.mp4/);
  assert.equal(env.$('btnCancel').hidden, false, 'clip exports can be cancelled from the sheet');
  assert.equal(env.$('btnCancel').textContent, '내보내기 중단');
  // carve starts: the case job takes the chip over the clip export
  env.status = ready({ busy: true, phase: 'finalizing', job_kind: 'carve', job: sj(), clip_jobs: [clip()], clips_running: 1 });
  env.$('btnCarve').click();
  await env.answerDialog(true);
  await env.advance(1300);
  assert.match(env.$('jobTitle').textContent, /이미지 카빙 진행 중/);
  assert.equal(env.$('btnJobCancel').hidden, false);
  assert.equal(env.$('btnCancel').hidden, true, 'one cancel per kind of work');
  // a pipeline run (analysis of another source) outranks both
  env.status = running({ job: sj(), clip_jobs: [clip()], clips_running: 1 });
  await env.advance(1300);
  assert.equal(env.$('jobTitle').textContent, '분석 중');
  assert.equal(env.$('btnCancel').textContent, '분석 중단');
  // the sheet note / counts stay a single line each
  assert.match(env.$('jobsNote').textContent, /분석 중/);
});

await test('4b. a failed clip export is reported (sheet row + sticky toast), once', async () => {
  const env = makeEnv({ status: ready({ clip_jobs: [], clips_running: 0 }) });
  await env.advance(1200);
  env.status = ready({ clip_jobs: [{ id: 3, kind: 'export-clip', state: 'failed', started_unix: 1, elapsed_secs: 2, path: 'exports/a.mp4', error: 'ffmpeg 종료 코드 1\n자세히' }], clips_running: 0 });
  await env.advance(5100);
  assert.equal(toastTexts(env).filter((t) => /클립 내보내기 실패: a\.mp4 — ffmpeg 종료 코드 1/.test(t)).length, 1);
  await env.advance(5100);
  assert.equal(toastTexts(env).filter((t) => /클립 내보내기 실패/.test(t)).length, 1, 'not repeated every poll');
  assert.match(env.$('jobsClipList').innerHTML, /class="failed"/);
  assert.match(env.$('jobsClipList').innerHTML, /실패/);
});

await test('5. enrichment finishing waits for the examiner (no reload behind their back); reload via the sheet', async () => {
  const env = makeEnv({ status: partial() });
  await env.advance(1200);
  mount(env);
  env.status = ready();
  await env.advance(1200);
  assert.equal(env.viewer.reloadCalls.length, 0, 'no automatic reload while reviewing');
  assert.match(env.$('jobPanel').className, /\bdeferred\b/);
  assert.equal(env.$('jobTitle').textContent, '보강 완료');
  assert.equal(env.$('btnJobReload').hidden, false);
  assert.equal(env.$('btnJobReload').textContent, '새 데이터 반영');
  env.viewer.reloadResult = 'refused';
  env.$('btnJobReload').click();
  await env.advance(50);
  assert.equal(env.viewer.reloadCalls.length, 1);
  assert.equal(env.$('btnJobReload').hidden, false, 'refused: still offered');
  assert.ok(toastTexts(env).some((t) => /저장되지 않은/.test(t)));
  env.viewer.reloadResult = 'reloaded';
  env.$('btnJobReload').click();
  await env.advance(50);
  assert.equal(env.viewer.reloadCalls.length, 2);
  assert.equal(env.$('btnJobReload').hidden, true);
  assert.match(env.$('jobPanel').className, /\bidle\b/);
});

await test('5b. a finished recover job: FTViewer.reload() exactly once; refused -> deferred + btnJobReload', async () => {
  const sj = (over = {}) => ({ kind: 'recover-deleted', state: 'running', started_unix: 1_700_000_100, elapsed_secs: 5, stage: 1, stages: 2, stage_label: '복구', error: null, ...over });
  for (const refuse of [false, true]) {
    const env = makeEnv({ status: ready(), routes: { 'POST /api/recover-deleted': () => ({ json: { ok: true, job: { kind: 'recover-deleted', started_unix: 1_700_000_100 } } }) } });
    await env.advance(1200);
    mount(env);
    env.viewer.reloadResult = refuse ? 'refused' : 'reloaded';
    env.status = ready({ busy: true, phase: 'finalizing', job: sj() });
    env.$('btnRecoverDeleted').click();
    await env.advance(1300);
    env.status = ready({ job: sj({ state: 'done' }) });
    await env.advance(1300 + 5100);
    assert.equal(env.viewer.reloadCalls.length, 1);
    if (refuse) {
      assert.match(env.$('jobPanel').className, /\bdeferred\b/);
      assert.equal(env.$('btnJobReload').hidden, false);
    } else {
      assert.match(env.$('jobPanel').className, /\bok\b/);
      assert.equal(env.$('btnJobReload').hidden, true);
    }
  }
});

await test('6. polling cadence: 1 s while busy, 5 s while idle', async () => {
  const env = makeEnv({ status: idle });
  await env.advance(31000);
  const idleCalls = env.calls.filter((c) => c.url === '/api/status').map((c) => c.at);
  const gaps = idleCalls.slice(1).map((t, i) => t - idleCalls[i]);
  assert.ok(gaps.length >= 5 && gaps.every((g) => g === 5000), 'idle gaps: ' + gaps);
  env.status = running();
  await env.advance(5100);
  const mark = env.calls.length;
  await env.advance(10000);
  const busyCalls = env.calls.slice(mark).filter((c) => c.url === '/api/status').map((c) => c.at);
  const busyGaps = busyCalls.slice(1).map((t, i) => t - busyCalls[i]);
  assert.ok(busyGaps.length >= 8 && busyGaps.every((g) => g === 1000), 'busy gaps: ' + busyGaps);
  env.status = ready();
  await env.advance(2000);
  const mark2 = env.calls.length;
  await env.advance(12000);
  const back = env.calls.slice(mark2).filter((c) => c.url === '/api/status').map((c) => c.at);
  assert.ok(back.length <= 3, 'back to the slow cadence: ' + back.length);
});

await test('6b. server unreachable: store flags it, backs off, recovers', async () => {
  const env = makeEnv({ status: ready() });
  await env.advance(1200);
  env.routes['GET /api/status'] = () => { throw new Error('down'); };
  await env.advance(60000);
  assert.equal(env.state().server.lost, true);
  assert.equal(env.state().phase, 'server-lost');
  assert.equal(env.$('serverBanner').hidden, false);
  delete env.routes['GET /api/status'];
  await env.advance(15000);
  assert.equal(env.state().server.lost, false);
  assert.equal(env.state().phase, 'review-ready');
  assert.equal(env.$('serverBanner').hidden, true);
});

await test('7. failures are never silent: chip + sheet error box + sticky toast (once)', async () => {
  const env = makeEnv({ status: running() });
  await env.advance(1200);
  const raw = 'ewfinfo: 이미지를 열 수 없습니다\n상세 로그 줄';
  env.status = { ...running(), phase: 'error', busy: false, steps: ['done', 'failed', 'pending', 'pending', 'pending'], error: raw, logs: ['오류: ' + raw] };
  await env.advance(1200);
  assert.match(env.$('jobPanel').className, /\bfailed\b/);
  assert.equal(env.$('jobTitle').textContent, '분석 실패');
  assert.match(env.$('jobDetail').textContent, /ewfinfo: 이미지를 열 수 없습니다/);
  assert.equal(env.$('jobsErr').hidden, false);
  assert.match(env.$('jobsErr').innerHTML, /libewf 도구가 필요/);
  assert.match(env.$('jobsErr').innerHTML, /자세한 오류 내용/);
  assert.match(env.$('track').innerHTML, /class="failed"/);
  await env.advance(3000);
  assert.equal(toastTexts(env).filter((t) => /분석 실패/.test(t)).length, 1);
  assert.equal(env.$('btnNew').hidden, false, 'a way out of the failure');
  assert.equal(env.$('btnCancel').hidden, true);
  // dismissible, and it stays dismissed
  assert.equal(env.$('btnJobClose').hidden, false);
  env.$('btnJobClose').click();
  await env.advance(1200);
  assert.doesNotMatch(env.$('jobPanel').className, /failed/);
});

await test('7b. stopped enrichment is shown (not a failure) and keeps review open', async () => {
  const env = makeEnv({ status: partial({ busy: false, enrichment: null, error: null }) });
  await env.advance(1200);
  assert.match(env.$('jobPanel').className, /\bcancelled\b/);
  assert.equal(env.$('jobTitle').textContent, '보강 중단됨');
  assert.equal(env.$('btnCancel').hidden, true);
  assert.equal(env.$('btnRecoverDeleted').hidden, false, 'review-partial keeps the case tools');
});

await test('8. progress never blocks review: nothing locks the viewer or the chip', async () => {
  const env = makeEnv({ status: partial() });
  await env.advance(1200);
  mount(env);
  assert.equal(env.state().viewer.mounted, true);
  assert.equal(env.viewer.reloadCalls.length, 0);
  assert.equal(env.$('hBtnJobs').disabled, false);
  // selection / save-state traffic from the viewer does not churn the sheet
  const html = env.$('track').innerHTML;
  env.store().set({ viewer: { ...env.state().viewer, selection: 3 } });
  assert.equal(env.$('track').innerHTML, html);
  // a locked start form while running is the only thing the analysis disables
  assert.equal(env.$('btnStart').disabled, true);
});

await test('9. no uncaught errors', async () => {
  const env = makeEnv({ status: idle });
  env.status = running();
  await env.advance(20000);
  env.status = partial();
  await env.advance(5000);
  env.status = ready();
  await env.advance(8000);
  assert.deepEqual(env.errors.map(String), []);
  assert.deepEqual(env.consoleErrors, []);
});

console.log(process.exitCode ? 'verify-progress: FAILED' : `verify-progress: ${passedCount()} passed`);
