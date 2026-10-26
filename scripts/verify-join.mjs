// Lossless join UI (docs/PLAN_PHASE2_JOIN_AUDIO_2026-10-13.md §1.6-§1.8, §3):
//  1. one selected video: the menu item is disabled and says why (title)
//  2. three consecutive front videos: the dialog summary reads
//     "전방 · 3개 · 0분 0N초 · 끊김 0곳 / 겹침 1곳…", one enabled primary
//  3. running: progress row with [중단] (observed — never vacuous),
//     completion dialog with a 64-hex SHA-256; after the list is refreshed a
//     kind=joined card is there, under the server's record_id (old logs:
//     "join:<file>"), and a filename-order join says "끊김·겹침 확인 불가"
//  4. front + rear: primary disabled, its title = the first blocking reason,
//     the reasons listed in the dialog (unknown codes still read as text)
//  5. cancel: the toast says the partial file was deleted (live: large
//     ffmpeg segments so the cancel always lands; no .part left)
//  6. j / k / 1 / Space inside the dialog never reach the grid or marks
//  7. 1366x768 and 1920x1080 (segment list open): the dialog buttons are on
//     screen and not covered (elementFromPoint)
//  8. axe-core WCAG 2.x A/AA on the dialog in light and dark
//  9. (mock) a cancel that loses the race to completion: no failure toast,
//     the success dialog only
// 10. (mock) duration_check.observed null: "길이 확인 불가", never 0분 00초
// 11. (live) the HOSTED shell: a join started in the shell-hosted viewer
//     makes the shell offer '새 데이터 반영' even when its status poll never
//     saw the job running (FTViewer "derived-job"), the shell polls fast
//     after the start, the reload shows the joined record, and a mark on it
//     saves through save protocol v2 and survives a page reload
//
//   node scripts/verify-join.mjs          mock mode: the standalone viewer
//                                          served over http, the join API
//                                          answered by page.route with the
//                                          real server's JSON shapes
//   node scripts/verify-join.mjs --live   the real workstation (isolated
//                                          server + ffmpeg fixtures)
//   (needs playwright via NODE_PATH; axe-core optional but expected)
import { readFileSync, readdirSync, existsSync, mkdirSync, mkdtempSync, rmSync } from 'node:fs';
import { join, dirname } from 'node:path';
import { tmpdir } from 'node:os';
import { fileURLToPath } from 'node:url';
import { createServer } from 'node:http';
import { createRequire } from 'node:module';
import { spawnSync } from 'node:child_process';
import { viewerPageHtml, viewerBundleJs } from './lib/app-bundle.mjs';
import { startIsolatedServer } from './lib/isolated-server.mjs';

const root = join(dirname(fileURLToPath(import.meta.url)), '..');
const require = createRequire(import.meta.url);
const { chromium } = require('playwright');
let axeSource = '';
try { axeSource = readFileSync(require.resolve('axe-core/axe.min.js'), 'utf8'); } catch { /* reported below */ }
const LIVE = process.argv.includes('--live');
const sleep = (ms) => new Promise((r) => setTimeout(r, ms));

const results = [];
const check = (name, ok, detail = '') => {
  results.push(!!ok);
  console.log(`${ok ? 'PASS' : 'FAIL'}  ${name}${detail ? '  — ' + String(detail).slice(0, 400) : ''}`);
};
const skip = (name, why) => console.log(`SKIP  ${name}  — ${why}`);

// ---------------------------------------------------------------- mock backend
// Front channel: 0 s, +1 s (overlap 2 s with 3 s segments), +4 s (continuous).
const MOCK_JOINED_ID = 'join:0123456789abcdef';
const MOCK_OUT = 'artifacts/joined/join_F_20260910-120000_n3.mp4';
const MOCK_MANIFEST = 'artifacts/joined/join_F_20260910-120000_n3.manifest.json';
async function mockBackend() {
  const names = {
    vid_f1: 'DCIM/FRONT/20260910_120000_F.mp4',
    vid_f2: 'DCIM/FRONT/20260910_120001_F.mp4',
    vid_f3: 'DCIM/FRONT/20260910_120004_F.mp4',
    vid_r1: 'DCIM/REAR/20260910_120000_R.mp4',
    vid_x1: 'DCIM/FRONT/20260911_090000_F.mp4',
  };
  const videos = Object.entries(names).map(([id, rel]) => ({
    id, relative_path: rel, source_path: `/case/src/${rel}`, ffprobe_ok: true, audio_codec: 'aac',
    size_bytes: 1000, extension: 'mp4', duration_seconds: 3, sha256: id.padEnd(64, '0').slice(0, 64),
  }));
  const data = {
    manifest: { case_id: 'FT-JOIN', title: 'join' }, caseDir: '/case', scan: { videos },
    carveLog: [], filesystemLog: [], validationLog: [], anomalyLog: [], flsEntries: [],
    thumbs: {}, annotations: {}, deepfake: {}, telemetry: {}, proxies: [],
  };
  const js = viewerBundleJs(root);
  let joined = false;
  // join-log.jsonl entries as records-meta passes them: the new one names
  // its record (record_id); an older filename-order one has none.
  const joinLog = [{
    schema_version: 1, event: 'join-video', joined_unix: 1760000050,
    output_path: 'artifacts/joined/join_F_20260911-090000_n2.mp4', output_sha256: '9'.repeat(64), output_size_bytes: 2000,
    manifest_path: 'artifacts/joined/join_F_20260911-090000_n2.manifest.json', manifest_sha256: '8'.repeat(64),
    plan_id: 'plan-old', segment_count: 2, source_ids: ['vid_x1', 'vid_f1'], source_sha256: [], hash_mismatches: 0,
    order_basis: 'filename-order', channel: 'F', tolerance_seconds: 1, gaps: 0, overlaps: 0, mode: 'stream-copy', duration_check_ok: true,
  }, {
    schema_version: 1, event: 'join-video', joined_unix: 1760000100, record_id: MOCK_JOINED_ID,
    output_path: MOCK_OUT, output_sha256: 'e'.repeat(64), output_size_bytes: 3000,
    manifest_path: MOCK_MANIFEST, manifest_sha256: 'f'.repeat(64),
    plan_id: 'plan-front-3', segment_count: 3, source_ids: ['vid_f1', 'vid_f2', 'vid_f3'], source_sha256: [], hash_mismatches: 0,
    order_basis: 'filename-time', channel: 'F', tolerance_seconds: 1, gaps: 0, overlaps: 1, unknown_junctions: 0, mode: 'stream-copy', duration_check_ok: true,
  }];
  const page = () => {
    const d = joined ? { ...data, joinLog } : data;
    return viewerPageHtml({ data: `window.__FRAMETRACE_DATA__ = ${JSON.stringify(d)};`, js, root });
  };
  const server = createServer((req, res) => { res.setHeader('Content-Type', 'text/html; charset=utf-8'); res.end(page()); });
  await new Promise((r) => server.listen(0, '127.0.0.1', r));
  const seg = (id, i) => ({
    id, name: names[id].split('/').pop(), recorded_start_text: `2026-09-10 12:00:0${[0, 1, 4][i] ?? 0}`, time_source: 'filename',
    duration_seconds: 3, size_bytes: 1000,
    video: { codec: 'h264', profile: 'High', width: 640, height: 360, pix_fmt: 'yuv420p', r_frame_rate: '15/1', time_base: '1/15360', extradata_hash: 'x' },
    audio: { codec: 'aac', sample_rate: 44100, channels: 1 },
  });
  const planFor = (ids) => {
    const mixed = ids.includes('vid_r1');
    return {
      plan_id: mixed ? 'plan-mixed' : 'plan-front-3', order_basis: 'filename-time', tolerance_seconds: 1,
      channel: mixed ? { code: null, label: '', basis: 'filename-or-path-token' } : { code: 'F', label: '전방', basis: 'filename-or-path-token' },
      container: 'mp4',
      segments: ids.map(seg),
      junctions: ids.slice(1).map((_, i) => ({ after_index: i + 1, delta_seconds: i === 0 ? -2 : 0, kind: i === 0 ? 'overlap' : 'continuous' })),
      totals: { segments: ids.length, duration_seconds: 3 * ids.length, gaps: 0, gap_seconds: 0, overlaps: 1, overlap_seconds: 2, overlap_max_seconds: 2, unknown_junctions: 0 },
      blocking: mixed ? [
        { code: 'channel-mixed', message: '채널이 섞여 있습니다 (전방 3 · 후방 1)', ids },
        { code: 'same-start', message: '시작 시각이 같은 영상 2개 — 다른 채널이나 복사본일 수 있습니다', ids: ['vid_f1', 'vid_r1'] },
        { code: 'path-unsupported', ids: ['vid_r1'] },
        { code: 'some-future-code', ids: ['vid_r1'] },
      ] : [],
      warnings: mixed ? [] : [{ code: 'avg-frame-rate-differs', message: '평균 프레임레이트가 세그먼트마다 다릅니다 (VFR 가능)' }, { code: 'hash-unavailable', message: '색인 해시가 없는 원본 1개 — 이어붙일 때 새로 계산합니다' }],
      summary_text: mixed ? `채널 미확인 · ${ids.length}개 · 0분 12초 · 끊김 0곳 / 겹침 1곳(각 2초)` : '전방 · 3개 · 0분 09초 · 끊김 0곳 / 겹침 1곳(각 2초)',
    };
  };
  // One job at a time. Modes: "done" walks hash → concat → verify → done;
  // "done-null" finishes with duration_check.observed null; "cancel" stays
  // running until /api/export-clip-cancel arrives; "late-cancel" finishes
  // the moment the cancel arrives (the server then answers "already ended").
  let job = null;
  const requests = { check: [], join: [], cancel: 0 };
  const json = (route, body) => route.fulfill({ contentType: 'application/json', body: JSON.stringify(body) });
  // Mirrors clip_job_json (src/serve/media.rs) for kind "join": no totals,
  // path null until the join is logged, record_id = the first source.
  const jobJson = (over) => ({
    id: job.id, kind: 'join', record_id: 'vid_f1', state: 'running', started_unix: job.started, finished_unix: null,
    elapsed_secs: job.polls, path: null, stage: null, stage_done: null, stage_total: null, bytes_written: 0, expected_bytes: 3000,
    sha256: null, size_bytes: null, error: null, cancel_requested: !!job.cancelRequested,
    plan_id: job.plan, segments: 3, manifest_path: null, duration_check_ok: null, duration_check: null, ...over,
  });
  const doneJson = (check) => jobJson({
    state: 'done', finished_unix: job.started + job.polls, path: MOCK_OUT, bytes_written: 3000, sha256: 'e'.repeat(64), size_bytes: 3000,
    manifest_path: MOCK_MANIFEST, duration_check_ok: check.ok, duration_check: check,
  });
  return {
    url: `http://127.0.0.1:${server.address().port}/`,
    front: ['vid_f1', 'vid_f2', 'vid_f3'], cancelIds: ['vid_f1', 'vid_f2', 'vid_f3'], rear: 'vid_r1', requests,
    mode: 'done',
    async install(p) {
      await p.route('**/api/join-check', (route) => {
        const body = JSON.parse(route.request().postData() || '{}');
        requests.check.push(body);
        json(route, { ok: true, plan: planFor(body.ids || []) });
      });
      await p.route('**/api/join', (route) => {
        const body = JSON.parse(route.request().postData() || '{}');
        requests.join.push(body);
        job = { id: 'join-' + requests.join.length, state: 'running', mode: this.mode, polls: 0, started: 1760000000, plan: body.plan_id, cancelRequested: false };
        json(route, { ok: true, deduped: false, job: jobJson({ stage: 'hash', stage_done: 0, stage_total: 3 }) });
      });
      await p.route('**/api/export-clip-status*', (route) => {
        if (!job) return json(route, { ok: false, error: '해당 클립 내보내기 작업을 찾을 수 없습니다.' });
        job.polls += 1;
        if (job.mode === 'cancel' || job.mode === 'late-cancel') {
          if (job.state === 'cancelled') return json(route, { ok: true, job: jobJson({ state: 'cancelled', finished_unix: job.started + job.polls, error: '사용자가 중단했습니다', cancel_requested: true }) });
          if (job.state === 'done') { joined = true; return json(route, { ok: true, job: doneJson({ expected: 9, observed: 9.02, ok: true }) }); }
          return json(route, { ok: true, job: jobJson({ stage: 'concat', stage_done: 900, stage_total: 3000, bytes_written: 900 }) });
        }
        const steps = [
          { stage: 'hash', stage_done: 2, stage_total: 3 },
          { stage: 'concat', stage_done: 1500, stage_total: 3000, bytes_written: 1500 },
          { stage: 'verify', stage_done: 0, stage_total: 1, bytes_written: 3000 },
        ];
        const step = steps[job.polls - 1];
        if (step) return json(route, { ok: true, job: jobJson(step) });
        joined = true;
        job.state = 'done';
        // The server's fallback when the manifest has no duration_check.
        const dc = job.mode === 'done-null' ? { expected: 9, observed: null, ok: false } : { expected: 9, observed: 9.02, ok: true };
        json(route, { ok: true, job: doneJson(dc) });
      });
      await p.route('**/api/export-clip-cancel', (route) => {
        requests.cancel += 1;
        if (job && job.mode === 'late-cancel' && job.state === 'running') job.state = 'done';
        if (!job || job.state !== 'running') return json(route, { ok: false, error: '이미 끝난 클립 내보내기입니다.' });
        job.cancelRequested = true;
        job.state = 'cancelled';
        json(route, { ok: true });
      });
    },
    async afterDone(p) { await p.reload(); await p.waitForSelector('.card'); },
    leftovers() { return []; },
    async joinedIdFor() { return MOCK_JOINED_ID; },
    async stop() { server.close(); },
  };
}

// ---------------------------------------------------------------- live backend
// Real server + real ffmpeg fixtures: three short front clips + one rear
// (the dialog checks), two clips without recording times (filename-order),
// and three 30 s 40 Mbit/s clips (~450 MB — the cancel always lands while
// the debug build hashes/copies, as in tests/workstation_e2e).
async function liveBackend() {
  const ffmpeg = process.env.FFMPEG || 'ffmpeg';
  if (spawnSync(ffmpeg, ['-version']).status !== 0) throw new Error('ffmpeg not found (set FFMPEG=)');
  const work = mkdtempSync(join(tmpdir(), 'ft-join-'));
  const src = join(work, "증거 '따옴표' 폴더");
  const caseDir = join(work, 'case');
  const small = ['-c:v', 'libx264', '-pix_fmt', 'yuv420p', '-preset', 'ultrafast', '-g', '15'];
  const clips = [
    ['FRONT/20260910_120000_F.mp4', 3, 'testsrc2=size=640x360:rate=15', true, small],
    ['FRONT/20260910_120001_F.mp4', 3, 'testsrc2=size=640x360:rate=15', true, small],
    ['FRONT/20260910_120004_F.mp4', 3, 'testsrc2=size=640x360:rate=15', true, small],
    ['REAR/20260910_120000_R.mp4', 3, 'testsrc2=size=640x360:rate=15', true, small],
    ['NOTIME/clip_a_F.mp4', 2, 'testsrc2=size=640x360:rate=15', true, small],
    ['NOTIME/clip_b_F.mp4', 2, 'testsrc2=size=640x360:rate=15', true, small],
  ];
  const big = ['20260911_120000_F.mp4', '20260911_120030_F.mp4', '20260911_120100_F.mp4'];
  for (const name of big) clips.push([`BIG/${name}`, 30, 'testsrc2=size=1280x720:rate=30', false,
    ['-c:v', 'libx264', '-pix_fmt', 'yuv420p', '-preset', 'ultrafast', '-b:v', '40M', '-maxrate', '40M', '-bufsize', '40M']]);
  // Every segment must be byte-distinct (the server blocks a selection with
  // the same hash twice — duplicate-file): a per-file hue shift, tone and
  // container title, as tests/join_audio_it.rs varies its fixtures.
  for (const [i, [rel, secs, video, audio, enc]] of clips.entries()) {
    const file = join(src, rel);
    mkdirSync(dirname(file), { recursive: true });
    const args = ['-y', '-v', 'error', '-f', 'lavfi', '-i', `${video},hue=h=${i * 37}`];
    if (audio) args.push('-f', 'lavfi', '-i', `sine=frequency=${300 + i * 110}`);
    args.push('-t', String(secs), ...enc, '-metadata', `title=ft-join-fixture-${i}-${rel}`);
    if (audio) args.push('-c:a', 'aac', '-shortest');
    args.push(file);
    const r = spawnSync(ffmpeg, args, { encoding: 'utf8' });
    if (r.status !== 0) throw new Error('ffmpeg fixture failed: ' + r.stderr);
  }
  const srv = await startIsolatedServer({ root, work });
  const start = await srv.postJson('/api/start', { input_kind: 'folder', source_path: src, case_dir: caseDir, with_hash: true, with_ffprobe: true, with_deepfake: false });
  if (!start || !start.ok) throw new Error('start failed: ' + JSON.stringify(start));
  let st = null;
  for (let i = 0; i < 960; i++) {
    st = await srv.getJson('/api/status').catch(() => null);
    if (st && ['review-ready', 'done'].includes(st.phase) && !st.busy) break;
    await sleep(500);
  }
  if (!st || !['review-ready', 'done'].includes(st.phase)) throw new Error('analysis did not finish: ' + JSON.stringify(st).slice(0, 300));
  const recs = await srv.getJson('/api/records?offset=0&limit=100');
  const idOf = (name) => (recs.videos || []).find((v) => String(v.relative_path || '').replace(/\\/g, '/').endsWith(name))?.id;
  const front = ['20260910_120000_F.mp4', '20260910_120001_F.mp4', '20260910_120004_F.mp4'].map(idOf);
  const rear = idOf('20260910_120000_R.mp4');
  const notime = ['clip_a_F.mp4', 'clip_b_F.mp4'].map(idOf);
  const cancelIds = big.map(idOf);
  if ([...front, rear, ...notime, ...cancelIds].some((x) => !x)) throw new Error('fixture records not indexed: ' + JSON.stringify(recs.videos?.map((v) => v.relative_path)));
  const joinedDir = join(caseDir, 'artifacts', 'joined');
  return {
    url: `${srv.base}/viewer/?token=${srv.token}`, srv, front, rear, notime, cancelIds, requests: null, mode: 'done',
    async install() { /* the real API answers */ },
    async afterDone(p) { await p.reload(); await p.waitForSelector('.card'); },
    leftovers() { return existsSync(joinedDir) ? readdirSync(joinedDir).filter((f) => f.startsWith('.join-')) : []; },
    // The joined record id the viewer must use: the log entry's record_id
    // (server contract), else "join:<output file name>" (older logs).
    async joinedEntryFor(ids) {
      const meta = await srv.getJson('/api/records-meta');
      const want = JSON.stringify([...ids].sort());
      return (meta.joinLog || []).filter((e) => JSON.stringify([...(e.source_ids || [])].sort()) === want).at(-1) || null;
    },
    async joinedIdFor(ids) {
      const e = await this.joinedEntryFor(ids);
      if (!e) return null;
      return (typeof e.record_id === 'string' && e.record_id.trim()) || 'join:' + String(e.output_path || '').split(/[\\/]/).pop();
    },
    async stop() { srv.stop(); await sleep(300); try { rmSync(work, { recursive: true, force: true }); } catch { /* busy */ } },
  };
}

// ---------------------------------------------------------------- page helpers
function helpers(page) {
  const select = async (ids) => {
    await page.evaluate(() => document.getElementById('btnClearSelection')?.click());
    for (const id of ids) await page.locator(`.card[data-id="${id}"] input[data-check]`).check();
  };
  const menuItem = async () => {
    await page.click('#btnExportMenu');
    return page.evaluate(() => {
      const b = document.getElementById('btnJoinSelected');
      return { hidden: b.hidden, disabled: b.disabled, title: b.title, text: b.textContent };
    });
  };
  const openJoin = async () => {
    const info = await menuItem();
    if (!info.disabled) await page.click('#btnJoinSelected');
    else await page.keyboard.press('Escape');
    return page.waitForSelector('.modal.dlg.dlg-join', { timeout: 30000 }).catch(() => null);
  };
  const dialog = () => page.evaluate(() => {
    const d = document.querySelector('.modal.dlg.dlg-join');
    if (!d) return null;
    const ok = d.querySelector('.dlg-actions button.primary');
    return {
      title: d.querySelector('.panel-title')?.textContent,
      summary: d.querySelector('.join-sum')?.textContent,
      basis: d.querySelector('.join-basis')?.textContent,
      notes: [...d.querySelectorAll('.join-note')].map((n) => n.textContent),
      warns: [...d.querySelectorAll('.join-warn li')].map((n) => n.textContent),
      block: [...d.querySelectorAll('.join-block li')].map((n) => n.textContent),
      blockColor: d.querySelector('.join-block') ? getComputedStyle(d.querySelector('.join-block')).color : null,
      segsOpen: d.querySelector('details.join-segs')?.open,
      rows: d.querySelectorAll('.join-segs tbody tr').length,
      primaries: [...d.querySelectorAll('button.primary')].filter((b) => b.offsetParent).length,
      okDisabled: ok?.disabled, okTitle: ok?.title, okText: ok?.textContent,
      buttons: [...d.querySelectorAll('.dlg-actions button')].map((b) => b.textContent),
    };
  });
  // Every state the progress row ever showed (MutationObserver: a short
  // job cannot slip between two samples).
  const watchRows = () => page.evaluate(() => {
    window.__joinRows = [];
    const host = document.getElementById('toastHost');
    const note = () => {
      const row = host && host.querySelector('.clip-progress.job-join');
      if (!row) return;
      const s = JSON.stringify({ text: row.querySelector('.toast-text')?.textContent || '', cancel: row.querySelector('button')?.textContent || '' });
      if (window.__joinRows.at(-1) !== s) window.__joinRows.push(s);
    };
    if (window.__joinRowObs) window.__joinRowObs.disconnect();
    window.__joinRowObs = new MutationObserver(note);
    if (host) window.__joinRowObs.observe(host, { childList: true, subtree: true, characterData: true });
  });
  const rowsSeen = () => page.evaluate(() => (window.__joinRows || []).map((s) => JSON.parse(s)));
  const doneDialog = (timeout) => page.waitForFunction(() => {
    const d = [...document.querySelectorAll('.modal.dlg')].find((x) => x.querySelector('.panel-title')?.textContent === '이어붙이기 완료');
    return d ? d.innerText : null;
  }, null, { timeout }).then((h) => h.jsonValue()).catch(() => null);
  const toasts = () => page.evaluate(() => [...document.querySelectorAll('#toastHost .toast:not(.clip-progress)')].map((t) => t.textContent));
  // Clicks [이어붙이기]; a disabled (blocked) or missing primary is a clear
  // FAIL naming the blocking reasons, not a 30 s click timeout. Closes the
  // dialog and returns false then.
  const startJoin = async (what) => {
    const info = await dialog();
    if (!info || info.okDisabled) {
      check(`${what}: [이어붙이기] is enabled`, false, info ? 'blocked: ' + JSON.stringify(info.block) + ' warnings: ' + JSON.stringify(info.warns) : 'no join dialog');
      if (info) await page.keyboard.press('Escape');
      return false;
    }
    await page.click('.modal.dlg.dlg-join .dlg-actions button.primary');
    return true;
  };
  return { select, menuItem, openJoin, dialog, watchRows, rowsSeen, doneDialog, toasts, startJoin };
}

// ---------------------------------------------------------------- hosted shell (live)
async function hostedShellFlow(browser, backend) {
  const { srv } = backend;
  const ctx = await browser.newContext({ viewport: { width: 1366, height: 768 }, colorScheme: 'light' });
  const page = await ctx.newPage();
  const errors = [];
  page.on('pageerror', (e) => errors.push(String(e)));
  // The shell's status poll must NEVER see the join running: only the
  // viewer's "derived-job" event can then set the '새 데이터 반영' wait.
  let hideRunning = true;
  let hiddenSeen = 0;
  await page.route('**/api/status*', async (route) => {
    const res = await route.fetch();
    let body = await res.text();
    if (hideRunning) {
      try {
        const st = JSON.parse(body);
        if (Array.isArray(st.clip_jobs)) {
          const running = st.clip_jobs.filter((c) => c && c.kind === 'join' && c.state === 'running').length;
          hiddenSeen += running;
          st.clip_jobs = st.clip_jobs.filter((c) => !(c && c.kind === 'join' && c.state === 'running'));
          st.clips_running = Math.max(0, (Number(st.clips_running) || 0) - running);
          body = JSON.stringify(st);
        }
      } catch { /* not JSON: pass through */ }
    }
    await route.fulfill({ response: res, body });
  });
  const statusTimes = [];
  let joinAnsweredAt = 0;
  const saves = [];
  page.on('request', (r) => {
    if (/\/api\/status(\?|$)/.test(r.url())) statusTimes.push(Date.now());
    if (/\/api\/import-marks/.test(r.url())) {
      try { saves.push(JSON.parse(JSON.parse(r.postData() || '{}').marks_json || '{}')); } catch { saves.push(null); }
    }
  });
  page.on('response', (r) => { if (/\/api\/join$/.test(new URL(r.url()).pathname)) joinAnsweredAt = Date.now(); });
  const h = helpers(page);
  try {
    await page.goto(`${srv.base}/?token=${srv.token}`);
    await page.waitForSelector('#recordGrid .card', { timeout: 60000 });
    check('hosted: the shell mounts the viewer (FTViewer, data-ft-host)', await page.evaluate(() => document.documentElement.hasAttribute('data-ft-host') && typeof FTViewer === 'object'));
    const joinedIds = () => page.evaluate(() => [...document.querySelectorAll('#recordGrid .card[data-id^="join:"]')].map((c) => c.dataset.id));
    const before = await joinedIds();
    const ids = backend.front.slice(0, 2);
    await page.evaluate(() => { window.__derived = []; FTViewer.on('derived-job', (e) => window.__derived.push(e.detail)); });
    await h.select(ids);
    const dlg = await h.openJoin();
    check('hosted: join dialog opens in the shell-hosted viewer', !!dlg);
    if (!dlg) return;
    await h.watchRows();
    if (!(await h.startJoin('hosted join'))) return;
    const done = await h.doneDialog(180000);
    check('hosted: completion dialog points at the jobs sheet', !!done && /작업 시트의 '새 데이터 반영'/.test(done), String(done).slice(0, 300));
    const derived = await page.evaluate(() => window.__derived);
    check('hosted: viewer emitted derived-job started → done', derived.length >= 2 && derived[0].state === 'started' && derived.at(-1).state === 'done' && derived.every((d) => d.kind === 'join'), JSON.stringify(derived));
    const firstAfter = statusTimes.find((t) => t >= joinAnsweredAt);
    check('hosted: the shell polls fast after the join starts (< 1 s, idle cadence is 5 s)', !!joinAnsweredAt && !!firstAfter && firstAfter - joinAnsweredAt < 1000, `${firstAfter && joinAnsweredAt ? firstAfter - joinAnsweredAt : '-'} ms`);
    await page.click('.modal.dlg .dlg-actions button.primary');
    // Within 3 s (< one idle poll) and with the running job hidden from
    // every poll: only the viewer event can have set it.
    const offered = await page.waitForFunction(() => { const b = document.getElementById('btnJobReload'); return !!b && !b.hidden && /새 데이터 반영/.test(b.textContent); }, null, { timeout: 3000 }).then(() => true).catch(() => false);
    check('hosted: shell offers 새 데이터 반영 although its poll never saw the join running', offered, `hidden running sightings: ${hiddenSeen}`);
    const chip = await page.evaluate(() => document.getElementById('jobPanel')?.textContent || '');
    check('hosted: the chip says 이어붙이기 완료 · 새 데이터 반영 대기', /이어붙이기 완료/.test(chip), chip);
    hideRunning = false;
    // Open the work sheet the way the examiner does (chip), then reload.
    await page.click('#jobPanel button').catch(() => {});
    const sheetBtn = page.locator('#btnJobReload');
    if (await sheetBtn.isVisible().catch(() => false)) await sheetBtn.click();
    else await page.evaluate(() => document.getElementById('btnJobReload').click());
    const expectId = await backend.joinedIdFor(ids);
    const appeared = await page.waitForFunction((id) => !!document.querySelector(`#recordGrid .card[data-id="${CSS.escape(id)}"]`), expectId, { timeout: 30000 }).then(() => true).catch(() => false);
    const after = await joinedIds();
    check('hosted: clicking 새 데이터 반영 shows the joined record', appeared && after.length === before.length + 1, `${expectId} | ${JSON.stringify(after)}`);
    check('hosted: the reload offer is cleared after applying', await page.evaluate(() => document.getElementById('btnJobReload').hidden));
    const entry = await backend.joinedEntryFor(ids);
    // F4: the backend writes record_id on every join-log entry — a missing
    // one is a failure, not a skip.
    check('hosted: joined record id is the log entry record_id',
      !!entry && typeof entry.record_id === 'string' && after.includes(entry.record_id) && /^join:[0-9a-f]{16}$/.test(entry.record_id),
      entry ? String(entry.record_id) : 'no join-log entry; fallback ' + expectId);

    // Mark the joined record and save through save protocol v2.
    await page.click(`#recordGrid .card[data-id="${expectId}"]`);
    await page.evaluate(() => document.activeElement?.blur());
    await page.keyboard.press('2');
    await page.waitForFunction((id) => !!document.querySelector(`#recordGrid .card[data-id="${CSS.escape(id)}"] .mark-chip`), expectId, { timeout: 5000 }).catch(() => {});
    const saved = await page.evaluate(() => FTViewer.save({ patient: true })); // R9 U9
    const body = saves.filter(Boolean).find((b) => (b.marks || []).some((m) => m.id === expectId)) || null;
    check('hosted: the joined-record mark saves (FTViewer.save → true)', saved === true && (await page.evaluate(() => FTViewer.saveState())) === 'saved', String(saved));
    check('hosted: the save is a patch-v2 body carrying the joined id', !!body && body.protocol === 'patch-v2' && body.schema_version === 2, JSON.stringify(body).slice(0, 300));
    const meta = await srv.getJson('/api/records-meta');
    const row = ((meta.annotations && meta.annotations.marks) || []).find((m) => (m.record_id || m.id) === expectId);
    const rev = Number(((meta.annotations && meta.annotations.revisions) || {})[expectId]) || 0;
    check('hosted: the server stored the joined-record mark (important, revision ≥ 1)', !!row && row.status === 'important' && rev >= 1, JSON.stringify(row) + ' rev ' + rev);
    await page.reload();
    await page.waitForSelector('#recordGrid .card', { timeout: 60000 });
    const chipAfter = await page.evaluate((id) => document.querySelector(`#recordGrid .card[data-id="${CSS.escape(id)}"] .mark-chip`)?.className || '', expectId);
    check('hosted: the mark is still on the joined record after a page reload', /important/.test(chipAfter), chipAfter);
    check('hosted: no page errors', !errors.length, errors.join(' | '));
  } finally {
    await ctx.close();
  }
}

// ---------------------------------------------------------------- the run
const backend = LIVE ? await liveBackend() : await mockBackend();
console.log(`mode: ${LIVE ? 'live (real server)' : 'mock (page.route, real server JSON shapes)'}`);
const browser = await chromium.launch();
try {
  const ctx = await browser.newContext({ viewport: { width: 1366, height: 768 }, colorScheme: 'light' });
  await ctx.addInitScript(() => { try { localStorage.setItem('ft-theme', 'light'); } catch { /* blocked */ } });
  const page = await ctx.newPage();
  const errors = [];
  page.on('pageerror', (e) => errors.push(String(e)));
  await backend.install(page);
  await page.goto(backend.url);
  await page.waitForSelector('.card');
  const { select, menuItem, openJoin, dialog, watchRows, rowsSeen, doneDialog, toasts, startJoin } = helpers(page);

  // 1. one selected video
  await select([backend.front[0]]);
  const one = await menuItem();
  await page.keyboard.press('Escape');
  check('menu item present in 보내기·복사 with the plan wording', one.text === '선택 영상 이어붙이기…' && !one.hidden, JSON.stringify(one));
  check('one selected: disabled with the reason in title', one.disabled && one.title === '영상을 2개 이상 선택하십시오', JSON.stringify(one));

  // 2. three front videos → dialog
  await select(backend.front);
  const three = await menuItem();
  await page.keyboard.press('Escape');
  check('three selected: enabled', !three.disabled, JSON.stringify(three));
  let dlg = await openJoin();
  let d = await dialog();
  check('dialog opens titled 영상 이어붙이기', !!dlg && d.title === '영상 이어붙이기', JSON.stringify(d && d.title));
  if (backend.requests) {
    const body = backend.requests.check.at(-1) || {};
    check('join-check request: selected ids + tolerance 1.0', JSON.stringify([...(body.ids || [])].sort()) === JSON.stringify([...backend.front].sort()) && body.tolerance === 1, JSON.stringify(body));
  }
  check('summary line', /^전방 · 3개 · 0분 0\d초 · 끊김 0곳 \/ 겹침 1곳/.test(d.summary || ''), d.summary);
  check('basis line: order basis, tolerance, no re-encoding', d.basis === '순서: 파일명 시각 기준 · 허용 오차 1초 · 원본 프레임 그대로(재인코딩 없음)', d.basis);
  check('overlap note shown (and no gap note)', d.notes.length === 1 && /겹친 구간은 잘라내지 않습니다/.test(d.notes[0]), JSON.stringify(d.notes));
  if (backend.requests) check('warnings show the server message (avg-frame-rate-differs, hash-unavailable)', JSON.stringify(d.warns) === JSON.stringify(['평균 프레임레이트가 세그먼트마다 다릅니다 (VFR 가능)', '색인 해시가 없는 원본 1개 — 이어붙일 때 새로 계산합니다']), JSON.stringify(d.warns));
  check('segment list collapsed, 3 rows', d.segsOpen === false && d.rows === 3, `${d.segsOpen} ${d.rows}`);
  check('buttons: [취소] [이어붙이기], exactly one primary, enabled', JSON.stringify(d.buttons) === JSON.stringify(['취소', '이어붙이기']) && d.primaries === 1 && !d.okDisabled, JSON.stringify(d));

  // 6. keys inside the dialog
  const gridState = () => page.evaluate(() => ({ active: document.querySelector('.card.active')?.dataset.id, marks: document.querySelectorAll('.card .mark-chip').length }));
  const g0 = await gridState();
  await page.focus('.modal.dlg.dlg-join details.join-segs > summary');
  for (const key of ['j', 'k', '1', '2', 'x']) await page.keyboard.press(key);
  await page.keyboard.press('Space');
  const g1 = await gridState();
  const afterKeys = await dialog();
  check('j/k/1/2/x/Space inside the dialog leave the grid and marks alone', JSON.stringify(g0) === JSON.stringify(g1) && !!afterKeys, `${JSON.stringify(g0)} -> ${JSON.stringify(g1)}`);
  check('Space on the summary opens the segment list', afterKeys && afterKeys.segsOpen === true);

  // 7. on screen at both sizes with the list open
  for (const [w, h] of [[1366, 768], [1920, 1080]]) {
    await page.setViewportSize({ width: w, height: h });
    await sleep(150);
    const vis = await page.evaluate(() => {
      const out = [];
      for (const b of document.querySelectorAll('.modal.dlg.dlg-join .dlg-actions button')) {
        const r = b.getBoundingClientRect();
        const inside = r.left >= 0 && r.top >= 0 && r.right <= innerWidth && r.bottom <= innerHeight && r.width > 0;
        const hit = document.elementFromPoint(r.left + r.width / 2, r.top + r.height / 2);
        out.push({ text: b.textContent, inside, top: hit === b || b.contains(hit), h: Math.round(r.height) });
      }
      const box = document.querySelector('.modal.dlg.dlg-join').getBoundingClientRect();
      return { out, boxInside: box.top >= 0 && box.bottom <= innerHeight && box.left >= 0 && box.right <= innerWidth };
    });
    check(`${w}x${h}: dialog and its buttons on screen, not covered (list open)`, vis.boxInside && vis.out.length === 2 && vis.out.every((b) => b.inside && b.top), JSON.stringify(vis));
  }
  await page.setViewportSize({ width: 1366, height: 768 });

  // 8. axe on the dialog, both themes
  const axeRun = async (theme) => {
    await page.evaluate((t) => { document.documentElement.dataset.theme = t; }, theme);
    await sleep(100);
    if (!(await page.evaluate(() => !!window.axe))) await page.evaluate(axeSource);
    return page.evaluate(async () => {
      const r = await window.axe.run(document.querySelector('.modal.dlg.dlg-join'), { runOnly: { type: 'tag', values: ['wcag2a', 'wcag2aa', 'wcag21a', 'wcag21aa', 'wcag22aa'] }, resultTypes: ['violations'] });
      return r.violations.map((v) => `${v.id}: ${v.nodes.map((n) => n.target.join(' ')).join(', ')}`);
    });
  };
  if (axeSource) {
    for (const theme of ['light', 'dark']) {
      const v = await axeRun(theme);
      check(`axe AA on the dialog (${theme})`, !v.length, v.join(' | '));
    }
    await page.evaluate(() => { document.documentElement.dataset.theme = 'light'; });
  } else {
    check('axe-core resolvable (npm i --no-save axe-core)', false);
  }

  // 3. run → progress → completion
  backend.mode = 'done';
  await watchRows();
  const started = await startJoin('three front videos');
  const done = started ? await doneDialog(LIVE ? 180000 : 20000) : null;
  const seen = await rowsSeen();
  if (backend.requests) {
    const body = backend.requests.join.at(-1) || {};
    check('join request carries ids + plan_id + tolerance', body.plan_id === 'plan-front-3' && body.ids?.length === 3 && body.tolerance === 1, JSON.stringify(body));
    check('progress row shows the stages with [중단]', seen.some((s) => /^이어붙이는 중 · 해시 \d+\/3$/.test(s.text)) && seen.some((s) => /^이어붙이는 중 · \d+% · \d+초$/.test(s.text)) && seen.every((s) => s.cancel === '중단'), JSON.stringify(seen));
  } else {
    // Never vacuous: the row is drawn as soon as /api/join answers.
    check('progress row shown with [중단] and an 이어붙이는 중 line', seen.length > 0 && seen.every((s) => s.cancel === '중단' || s.cancel === '중단 요청 중…') && seen.some((s) => /^이어붙이는 중/.test(s.text)), JSON.stringify(seen));
  }
  check('completion dialog shows a 64-hex SHA-256', !!done && /SHA-256: [0-9a-f]{64}\b/.test(done), String(done).slice(0, 400));
  check('completion dialog: manifest, length vs sources, gaps/overlaps, refresh hint', !!done && /매니페스트: /.test(done) && /길이: \d+분 \d\d초 \(원본 합계 \d+분 \d\d초\)/.test(done) && /끊김 0곳 · 겹침 1곳/.test(done) && /목록을 새로 고치면 표시됩니다/.test(done), String(done).slice(0, 400));
  check('completion dialog has a single button', await page.evaluate(() => document.querySelectorAll('.modal.dlg .dlg-actions button').length === 1));
  if (done) await page.click('.modal.dlg .dlg-actions button.primary');
  await backend.afterDone(page);
  const jid = await backend.joinedIdFor(backend.front);
  const joinedCard = await page.evaluate((id) => {
    const c = id && document.querySelector(`.card[data-id="${CSS.escape(id)}"]`);
    return c ? { kind: c.querySelector('.kind-badge')?.textContent, status: c.querySelector('.badge')?.textContent } : null;
  }, jid);
  check('after refresh a kind=joined card is listed under the expected id (이어붙임 · 파생물)', !!joinedCard && joinedCard.kind === '이어붙임' && joinedCard.status === '파생물', `${jid} ${JSON.stringify(joinedCard)}`);
  if (backend.requests) {
    check('joined record uses the log entry record_id (not join:<file>)', jid === MOCK_JOINED_ID && (await page.locator(`.card[data-id="join:join_F_20260910-120000_n3.mp4"]`).count()) === 0);
    const old = 'join:join_F_20260911-090000_n2.mp4';
    check('an old log entry without record_id falls back to join:<file>', (await page.locator(`.card[data-id="${old}"]`).count()) === 1);
    await page.locator(`.card[data-id="${old}"]`).click();
    const oldNote = await page.evaluate(() => document.querySelector('#validationList .join-sources > .muted')?.textContent || '');
    check('a filename-order join reads "끊김·겹침 확인 불가" (never 끊김 0 / 겹침 0)', /끊김·겹침 확인 불가/.test(oldNote) && !/끊김 0/.test(oldNote), oldNote);
  }
  check('no temporary .join-* files left (live)', backend.leftovers().length === 0, backend.leftovers().join(', '));

  // filename-order (live): no recording times → notes say gaps/overlaps
  // cannot be checked; the server's unknown_junctions count when present.
  if (LIVE) {
    await select(backend.notime);
    await openJoin();
    const dn = await dialog();
    check('filename-order plan: the dialog says gaps/overlaps cannot be checked', !!dn && dn.notes.some((n) => /끊김·겹침은 확인할 수 없습니다|끊김·겹침 확인 불가/.test(n)), JSON.stringify(dn && dn.notes));
    await page.keyboard.press('Escape');
    // F4: a failed join-check request or a plan without unknown_junctions fails.
    let plan = null;
    let planErr = '';
    try { plan = await backend.srv.postJson('/api/join-check', { ids: backend.notime, tolerance: 1 }); } catch (err) { planErr = String(err && err.message || err); }
    const totals = plan && plan.plan && plan.plan.totals;
    check('server plan counts unknown_junctions for a filename-order join', !!totals && 'unknown_junctions' in totals && Number(totals.unknown_junctions) > 0, planErr || JSON.stringify(totals || plan));
  }

  // 4. front + rear → blocked
  await select([...backend.front, backend.rear]);
  dlg = await openJoin();
  d = await dialog();
  check('mixed channels: primary disabled', !!d && d.okDisabled === true, JSON.stringify(d));
  check('mixed channels: primary title = first blocking reason', !!d && !!d.okTitle && d.okTitle === d.block[0], `${d && d.okTitle} | ${d && d.block[0]}`);
  check('mixed channels: reasons listed under 이어붙일 수 없습니다 in --danger', !!d && d.block.length >= 1 && /채널/.test(d.block[0]) && !!d.blockColor, JSON.stringify(d && d.block));
  if (backend.requests) {
    check('a blocking code without a message gets the local wording (path-unsupported)', d.block.includes('원본 경로에 제어 문자(줄바꿈 등)가 있어 이어붙일 수 없습니다'), JSON.stringify(d.block));
    check('an unknown blocking code without a message still reads as a sentence', d.block.includes('이어붙일 수 없는 조건이 있습니다 (some-future-code)'), JSON.stringify(d.block));
  }
  check('blocked dialog focus starts on 취소', await page.evaluate(() => document.activeElement?.textContent === '취소'));
  if (axeSource) {
    for (const theme of ['light', 'dark']) {
      const v = await axeRun(theme);
      check(`axe AA on the blocked dialog (${theme})`, !v.length, v.join(' | '));
    }
    await page.evaluate(() => { document.documentElement.dataset.theme = 'light'; });
  }
  await page.keyboard.press('Escape');
  check('Esc closes the dialog without a join', (await page.locator('.modal.dlg').count()) === 0 && (!backend.requests || backend.requests.join.length === 1));

  // 5. cancel (live: 3 × 30 s at 40 Mbit/s — the job outlives the click)
  backend.mode = 'cancel';
  await select(backend.cancelIds);
  await openJoin();
  const cancelStarted = await startJoin('cancel run');
  const row = cancelStarted ? await page.waitForSelector('#toastHost .clip-progress.job-join button', { timeout: 30000 }).catch(() => null) : null;
  check('cancel: progress row with [중단] appears', !!row);
  if (row) {
    await row.click();
    const toastText = await page.waitForFunction(() => [...document.querySelectorAll('#toastHost .toast')].map((t) => t.textContent).find((t) => /이어붙이기를 중단했습니다/.test(t)) || null, null, { timeout: 60000 }).then((h) => h.jsonValue()).catch(() => null);
    check('cancel: toast says the partial file was deleted', !!toastText && /이어붙이기를 중단했습니다 — 만들던 파일은 지웠습니다\./.test(toastText), String(toastText) + ' | ' + JSON.stringify(await toasts()));
    if (backend.requests) check('cancel: POST /api/export-clip-cancel sent once', backend.requests.cancel === 1, String(backend.requests.cancel));
    check('cancel: progress row removed', (await page.locator('#toastHost .clip-progress').count()) === 0);
    check('cancel: no failure toast beside the cancel toast', !(await toasts()).some((t) => /실패/.test(t)), JSON.stringify(await toasts()));
    check('cancel: no .part / temp files left (live)', backend.leftovers().length === 0, backend.leftovers().join(', '));
    if (LIVE) {
      const entry = await backend.joinedEntryFor(backend.cancelIds);
      check('cancel: no join-log entry for the cancelled join (live)', !entry, JSON.stringify(entry));
    }
  }
  const menuAfter = await menuItem();
  await page.keyboard.press('Escape');
  check('menu item re-enabled after the job (selection still 3)', !menuAfter.disabled, JSON.stringify(menuAfter));

  if (backend.requests) {
    // 9. a cancel that loses the race: the server says "already ended", the
    // poll then sees "done" → the success dialog only, no failure toast.
    backend.mode = 'late-cancel';
    await page.evaluate(() => { for (const t of document.querySelectorAll('#toastHost .toast')) t.remove(); });
    await select(backend.front);
    await openJoin();
    const lateStarted = await startJoin('late cancel');
    const lateRow = lateStarted ? await page.waitForSelector('#toastHost .clip-progress.job-join button', { timeout: 10000 }).catch(() => null) : null;
    check('late cancel: [중단] clicked while the row was up', !!lateRow);
    if (lateRow) await lateRow.click();
    const lateDone = lateStarted ? await doneDialog(20000) : null;
    const lateToasts = await toasts();
    check('late cancel: the success dialog is shown', !!lateDone && /SHA-256: [0-9a-f]{64}/.test(lateDone), String(lateDone).slice(0, 200));
    check('late cancel: no failure / cancelled toast', !lateToasts.some((t) => /실패|이미 끝난|중단했습니다/.test(t)), JSON.stringify(lateToasts));
    if (lateDone) await page.click('.modal.dlg .dlg-actions button.primary');

    // 10. observed null → "길이 확인 불가", and the length warning says so.
    backend.mode = 'done-null';
    await select(backend.front);
    await openJoin();
    const nullDone = (await startJoin('observed-null run')) ? await doneDialog(20000) : null;
    check('observed null: length reads 길이 확인 불가 (not 0분 00초)', !!nullDone && /길이: 길이 확인 불가 \(원본 합계 0분 09초\)/.test(nullDone) && !/길이: 0분 00초/.test(nullDone), String(nullDone).slice(0, 400));
    check('observed null: the warning says the length could not be determined', !!nullDone && /출력 길이를 확인하지 못했습니다/.test(nullDone) && !/\?초 다릅니다/.test(nullDone), String(nullDone).slice(0, 400));
    check('completion counts come from the checked plan (the job reply has no totals)', !!nullDone && /끊김 0곳 · 겹침 1곳/.test(nullDone));
    if (nullDone) await page.click('.modal.dlg .dlg-actions button.primary');
  } else {
    skip('late cancel / observed null', 'mock only (timing / server fallback shapes)');
  }

  check('no page errors', !errors.length, errors.join(' | '));
  await ctx.close();

  // 11. hosted shell path (real server only).
  if (LIVE) await hostedShellFlow(browser, backend);
  else skip('hosted shell: 새 데이터 반영 after a viewer-started join, joined-record marks', 'run with --live (real server)');
} finally {
  await browser.close();
  await backend.stop();
}
const failed = results.filter((r) => !r).length;
console.log(failed ? `\n${failed} check(s) failed` : '\nall checks passed');
process.exit(failed ? 1 : 0);
