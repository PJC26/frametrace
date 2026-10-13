// Lossless join UI (docs/PLAN_PHASE2_JOIN_AUDIO_2026-10-13.md §1.6-§1.8, §3):
//  1. one selected video: the menu item is disabled and says why (title)
//  2. three consecutive front videos: the dialog summary reads
//     "전방 · 3개 · 0분 0N초 · 끊김 0곳 / 겹침 1곳…", one enabled primary
//  3. running: progress row with [중단], completion dialog with a 64-hex
//     SHA-256; after the list is refreshed a kind=joined card is there
//  4. front + rear: primary disabled, its title = the first blocking reason,
//     the reasons listed in the dialog
//  5. cancel: the toast says the partial file was deleted (live: no .part)
//  6. j / k / 1 / Space inside the dialog never reach the grid or marks
//  7. 1366x768 and 1920x1080 (segment list open): the dialog buttons are on
//     screen and not covered (elementFromPoint)
//  8. axe-core WCAG 2.x A/AA on the dialog in light and dark
//
//   node scripts/verify-join.mjs          mock mode: the standalone viewer
//                                          served over http, the join API
//                                          answered by page.route with the
//                                          §1.6 JSON (no server needed)
//   node scripts/verify-join.mjs --live   the real workstation (isolated
//                                          server + ffmpeg fixture); needs
//                                          the T2 join endpoints and a build
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
  console.log(`${ok ? 'PASS' : 'FAIL'}  ${name}${detail ? '  — ' + detail : ''}`);
};
const skip = (name, why) => console.log(`SKIP  ${name}  — ${why}`);

// ---------------------------------------------------------------- mock backend
// Front channel: 0 s, +1 s (overlap 2 s with 3 s segments), +4 s (continuous).
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
  const page = () => {
    const d = joined ? { ...data, joinLog: [{
      schema_version: 1, event: 'join-video', joined_unix: 1760000100,
      output_path: 'artifacts/joined/join_F_20260910-120000_n3.mp4', output_sha256: 'e'.repeat(64), output_size_bytes: 3000,
      manifest_path: 'artifacts/joined/join_F_20260910-120000_n3.manifest.json', manifest_sha256: 'f'.repeat(64),
      plan_id: 'plan-front-3', segment_count: 3, source_ids: ['vid_f1', 'vid_f2', 'vid_f3'], source_sha256: [], hash_mismatches: 0,
      order_basis: 'filename-time', channel: 'F', tolerance_seconds: 1, gaps: 0, overlaps: 1, mode: 'stream-copy', duration_check_ok: true,
    }] } : data;
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
      totals: { segments: ids.length, duration_seconds: 3 * ids.length, gaps: 0, gap_seconds: 0, overlaps: 1, overlap_seconds: 2, overlap_max_seconds: 2 },
      blocking: mixed ? [
        { code: 'channel-mixed', message: '채널이 섞여 있습니다 (전방 3 · 후방 1)', ids },
        { code: 'same-start', message: '시작 시각이 같은 영상 2개 — 다른 채널이나 복사본일 수 있습니다', ids: ['vid_f1', 'vid_r1'] },
        { code: 'path-unsupported', ids: ['vid_r1'] },
      ] : [],
      warnings: mixed ? [] : [{ code: 'avg-frame-rate-differs', message: '평균 프레임레이트가 세그먼트마다 다릅니다 (VFR 가능)' }, { code: 'hash-unavailable', message: '색인 해시가 없는 원본 1개 — 이어붙일 때 새로 계산합니다' }],
      summary_text: mixed ? `채널 미확인 · ${ids.length}개 · 0분 12초 · 끊김 0곳 / 겹침 1곳(각 2초)` : '전방 · 3개 · 0분 09초 · 끊김 0곳 / 겹침 1곳(각 2초)',
    };
  };
  // One job at a time: "done" walks hash → concat → verify → done; "cancel"
  // stays running until /api/export-clip-cancel arrives.
  let job = null;
  const requests = { check: [], join: [], cancel: 0 };
  const json = (route, body) => route.fulfill({ contentType: 'application/json', body: JSON.stringify(body) });
  return {
    url: `http://127.0.0.1:${server.address().port}/`,
    front: ['vid_f1', 'vid_f2', 'vid_f3'], rear: 'vid_r1', requests,
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
        job = { id: 'join-' + requests.join.length, kind: 'join', state: 'running', mode: this.mode, polls: 0, started_unix: 1760000000 };
        json(route, { ok: true, deduped: false, job: { id: job.id, kind: 'join', state: 'running', stage: 'hash', stage_done: 0, stage_total: 3, elapsed_secs: 0, bytes_written: 0, expected_bytes: 3000, path: 'artifacts/joined/join_F_20260910-120000_n3.mp4' } });
      });
      await p.route('**/api/export-clip-status*', (route) => {
        if (!job) return json(route, { ok: false, error: 'no job' });
        job.polls += 1;
        const base = { id: job.id, kind: 'join', elapsed_secs: job.polls, path: 'artifacts/joined/join_F_20260910-120000_n3.mp4', expected_bytes: 3000 };
        if (job.mode === 'cancel') {
          return json(route, { ok: true, job: job.state === 'cancelled' ? { ...base, state: 'cancelled', error: '사용자가 중단했습니다' } : { ...base, state: 'running', stage: 'concat', bytes_written: 900 } });
        }
        const steps = [
          { state: 'running', stage: 'hash', stage_done: 2, stage_total: 3, bytes_written: 0 },
          { state: 'running', stage: 'concat', bytes_written: 1500 },
          { state: 'running', stage: 'verify', bytes_written: 3000 },
        ];
        const step = steps[job.polls - 1];
        if (step) return json(route, { ok: true, job: { ...base, ...step } });
        joined = true;
        json(route, { ok: true, job: { ...base, state: 'done', size_bytes: 3000, sha256: 'e'.repeat(64), manifest_path: 'artifacts/joined/join_F_20260910-120000_n3.manifest.json', duration_check: { expected: 9, observed: 9.02, ok: true }, totals: { gaps: 0, overlaps: 1 } } });
      });
      await p.route('**/api/export-clip-cancel', (route) => {
        requests.cancel += 1;
        if (job) job.state = 'cancelled';
        json(route, { ok: true });
      });
    },
    async afterDone(p) { await p.reload(); await p.waitForSelector('.card'); },
    leftovers() { return []; },
    cancelObservable: true,
    async stop() { server.close(); },
  };
}

// ---------------------------------------------------------------- live backend
// Real server + real ffmpeg fixture. Needs the join endpoints (T2) and the
// join CLI (T1); until then the join-check request fails and this mode fails.
async function liveBackend() {
  const ffmpeg = process.env.FFMPEG || 'ffmpeg';
  if (spawnSync(ffmpeg, ['-version']).status !== 0) throw new Error('ffmpeg not found (set FFMPEG=)');
  const work = mkdtempSync(join(tmpdir(), 'ft-join-'));
  const src = join(work, "증거 '따옴표' 폴더");
  const caseDir = join(work, 'case');
  const clips = [['FRONT/20260910_120000_F.mp4', 3], ['FRONT/20260910_120001_F.mp4', 3], ['FRONT/20260910_120004_F.mp4', 3], ['REAR/20260910_120000_R.mp4', 3]];
  for (const [rel, secs] of clips) {
    const file = join(src, rel);
    mkdirSync(dirname(file), { recursive: true });
    const r = spawnSync(ffmpeg, ['-y', '-v', 'error', '-f', 'lavfi', '-i', 'testsrc2=size=640x360:rate=15', '-f', 'lavfi', '-i', 'sine=frequency=440',
      '-t', String(secs), '-c:v', 'libx264', '-pix_fmt', 'yuv420p', '-preset', 'ultrafast', '-g', '15', '-c:a', 'aac', '-shortest', file], { encoding: 'utf8' });
    if (r.status !== 0) throw new Error('ffmpeg fixture failed: ' + r.stderr);
  }
  const srv = await startIsolatedServer({ root, work });
  const start = await srv.postJson('/api/start', { input_kind: 'folder', source_path: src, case_dir: caseDir, with_hash: true, with_ffprobe: true, with_deepfake: false });
  if (!start || !start.ok) throw new Error('start failed: ' + JSON.stringify(start));
  let st = null;
  for (let i = 0; i < 480; i++) {
    st = await srv.getJson('/api/status').catch(() => null);
    if (st && ['review-ready', 'done'].includes(st.phase) && !st.busy) break;
    await sleep(500);
  }
  if (!st || !['review-ready', 'done'].includes(st.phase)) throw new Error('analysis did not finish: ' + JSON.stringify(st).slice(0, 300));
  const recs = await srv.getJson('/api/records?offset=0&limit=100');
  const idOf = (name) => (recs.videos || []).find((v) => String(v.relative_path || '').replace(/\\/g, '/').endsWith(name))?.id;
  const front = ['20260910_120000_F.mp4', '20260910_120001_F.mp4', '20260910_120004_F.mp4'].map(idOf);
  const rear = idOf('20260910_120000_R.mp4');
  if (front.some((x) => !x) || !rear) throw new Error('fixture records not indexed: ' + JSON.stringify(recs.videos?.map((v) => v.relative_path)));
  const joinedDir = join(caseDir, 'artifacts', 'joined');
  return {
    url: `${srv.base}/viewer/?token=${srv.token}`, front, rear, requests: null, mode: 'done',
    async install() { /* the real API answers */ },
    async afterDone(p) { await p.reload(); await p.waitForSelector('.card'); },
    leftovers() { return existsSync(joinedDir) ? readdirSync(joinedDir).filter((f) => f.startsWith('.join-')) : []; },
    cancelObservable: false, // tiny fixture: the job may finish before the click
    async stop() { srv.stop(); await sleep(300); try { rmSync(work, { recursive: true, force: true }); } catch { /* busy */ } },
  };
}

// ---------------------------------------------------------------- the run
const backend = LIVE ? await liveBackend() : await mockBackend();
console.log(`mode: ${LIVE ? 'live (real server)' : 'mock (page.route, §1.6 JSON)'}`);
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

  const select = async (ids) => {
    await page.evaluate(() => document.getElementById('btnClearSelection')?.click());
    for (const id of ids) await page.locator(`.card[data-id="${id}"] input[data-check]`).check();
  };
  const menuItem = async () => {
    await page.click('#btnExportMenu');
    const info = await page.evaluate(() => {
      const b = document.getElementById('btnJoinSelected');
      return { hidden: b.hidden, disabled: b.disabled, title: b.title, text: b.textContent };
    });
    return info;
  };
  const openJoin = async () => {
    const info = await menuItem();
    if (!info.disabled) await page.click('#btnJoinSelected');
    else await page.keyboard.press('Escape');
    return page.waitForSelector('.modal.dlg.dlg-join', { timeout: 15000 }).catch(() => null);
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
  const progressSeen = new Set();
  const watch = setInterval(async () => {
    try {
      const t = await page.evaluate(() => {
        const row = document.querySelector('#toastHost .clip-progress.job-join');
        return row ? { text: row.querySelector('.toast-text')?.textContent, cancel: row.querySelector('button')?.textContent } : null;
      });
      if (t) progressSeen.add(JSON.stringify(t));
    } catch { /* navigating */ }
  }, 150);
  await page.click('.modal.dlg.dlg-join .dlg-actions button.primary');
  const done = await page.waitForFunction(() => {
    const d = [...document.querySelectorAll('.modal.dlg')].find((x) => x.querySelector('.panel-title')?.textContent === '이어붙이기 완료');
    return d ? d.innerText : null;
  }, null, { timeout: LIVE ? 120000 : 20000 }).then((h) => h.jsonValue()).catch(() => null);
  clearInterval(watch);
  const seen = [...progressSeen].map((x) => JSON.parse(x));
  if (backend.requests) {
    const body = backend.requests.join.at(-1) || {};
    check('join request carries ids + plan_id + tolerance', body.plan_id === 'plan-front-3' && body.ids?.length === 3 && body.tolerance === 1, JSON.stringify(body));
    check('progress row shows the stages with [중단]', seen.some((s) => /^이어붙이는 중 · 해시 \d+\/3$/.test(s.text)) && seen.some((s) => /^이어붙이는 중 · \d+% · \d+초$/.test(s.text)) && seen.every((s) => s.cancel === '중단'), JSON.stringify(seen));
  } else {
    check('progress row shown with [중단] (or the job finished first)', seen.length === 0 || seen.every((s) => s.cancel === '중단'), JSON.stringify(seen));
  }
  check('completion dialog shows a 64-hex SHA-256', !!done && /SHA-256: [0-9a-f]{64}\b/.test(done), String(done).slice(0, 400));
  check('completion dialog: manifest, length vs sources, gaps/overlaps, refresh hint', !!done && /매니페스트: /.test(done) && /길이: \d+분 \d\d초 \(원본 합계 \d+분 \d\d초\)/.test(done) && /끊김 0곳 · 겹침 1곳/.test(done) && /목록을 새로 고치면 표시됩니다/.test(done), String(done).slice(0, 400));
  check('completion dialog has a single button', await page.evaluate(() => document.querySelectorAll('.modal.dlg .dlg-actions button').length === 1));
  await page.click('.modal.dlg .dlg-actions button.primary');
  await backend.afterDone(page);
  const joinedCard = await page.evaluate(() => {
    const c = [...document.querySelectorAll('.card[data-id^="join:"]')][0];
    return c ? { kind: c.querySelector('.kind-badge')?.textContent, status: c.querySelector('.badge')?.textContent } : null;
  });
  check('after refresh a kind=joined card is listed (이어붙임 · 파생물)', !!joinedCard && joinedCard.kind === '이어붙임' && joinedCard.status === '파생물', JSON.stringify(joinedCard));
  check('no temporary .join-* files left (live)', backend.leftovers().length === 0, backend.leftovers().join(', '));

  // 4. front + rear → blocked
  await select([...backend.front, backend.rear]);
  dlg = await openJoin();
  d = await dialog();
  check('mixed channels: primary disabled', !!d && d.okDisabled === true, JSON.stringify(d));
  check('mixed channels: primary title = first blocking reason', !!d && !!d.okTitle && d.okTitle === d.block[0], `${d && d.okTitle} | ${d && d.block[0]}`);
  check('mixed channels: reasons listed under 이어붙일 수 없습니다 in --danger', !!d && d.block.length >= 1 && /채널/.test(d.block[0]) && !!d.blockColor, JSON.stringify(d && d.block));
  if (backend.requests) check('a blocking code without a message gets the local wording (path-unsupported)', d.block.includes('원본 경로에 제어 문자(줄바꿈 등)가 있어 이어붙일 수 없습니다'), JSON.stringify(d.block));
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

  // 5. cancel
  backend.mode = 'cancel';
  await select(backend.front);
  await openJoin();
  await page.click('.modal.dlg.dlg-join .dlg-actions button.primary');
  const row = await page.waitForSelector('#toastHost .clip-progress.job-join button', { timeout: 10000 }).catch(() => null);
  if (row) {
    await row.click();
    const toastText = await page.waitForFunction(() => [...document.querySelectorAll('#toastHost .toast')].map((t) => t.textContent).find((t) => /이어붙이기를 중단했습니다/.test(t)) || null, null, { timeout: 10000 }).then((h) => h.jsonValue()).catch(() => null);
    check('cancel: toast says the partial file was deleted', !!toastText && /이어붙이기를 중단했습니다 — 만들던 파일은 지웠습니다\./.test(toastText), String(toastText));
    if (backend.requests) check('cancel: POST /api/export-clip-cancel sent once', backend.requests.cancel === 1, String(backend.requests.cancel));
    check('cancel: progress row removed', (await page.locator('#toastHost .clip-progress').count()) === 0);
    check('cancel: no .part / temp files left (live)', backend.leftovers().length === 0, backend.leftovers().join(', '));
  } else if (!backend.cancelObservable) {
    skip('cancel path', 'the live fixture job finished before the progress row could be clicked');
    await page.click('.modal.dlg .dlg-actions button.primary').catch(() => {});
  } else {
    check('cancel: progress row with [중단] appears', false);
  }
  const menuAfter = await menuItem();
  await page.keyboard.press('Escape');
  check('menu item re-enabled after the job (selection still 3)', !menuAfter.disabled, JSON.stringify(menuAfter));

  check('no page errors', !errors.length, errors.join(' | '));
} finally {
  await browser.close();
  await backend.stop();
}
const failed = results.filter((r) => !r).length;
console.log(failed ? `\n${failed} check(s) failed` : '\nall checks passed');
process.exit(failed ? 1 : 0);
