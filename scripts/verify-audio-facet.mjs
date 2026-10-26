// Measured sound in the viewer (docs/PLAN_PHASE2_JOIN_AUDIO_2026-10-13.md
// §2.4-§2.6, §3): a standalone viewer page (exactly as build.rs assembles it)
// with synthetic DATA covering the five sound states (+ the "measurement
// failed" and "changed since" reasons) and one derived joined record.
//  1. card icon (use href) + aria-label per state; unprobed shows nothing
//  2. detail "소리" text per state; no "음성" label left anywhere
//  3. the "소리" facet counts every dimension but its own (other facets move
//     it, picking it leaves it), fixed value order
//  4. picking a value filters the grid
//  5. date + kind facets keep working (and honour the sound filter)
//  6. j / k / x / 1 / 0 still drive the grid
//  7. CSV carries audio_state / audio_max_db / audio_mean_db
//  8. memo regression: a sort change does not recompute facet counts
//  9. a joinLog entry becomes a derived "이어붙인 영상" record with its sources
//
//   node scripts/verify-audio-facet.mjs   (needs playwright via NODE_PATH)
import { readFileSync, writeFileSync, mkdtempSync, rmSync } from 'node:fs';
import { tmpdir } from 'node:os';
import { pathToFileURL } from 'node:url';
import { join, dirname } from 'node:path';
import { fileURLToPath } from 'node:url';
import { createServer } from 'node:http';
import { createRequire } from 'node:module';
import { viewerPageHtml, viewerBundleJs } from './lib/app-bundle.mjs';

const root = join(dirname(fileURLToPath(import.meta.url)), '..');
const require = createRequire(import.meta.url);
const { chromium } = require('playwright');

const vid = (id, day, extra) => ({
  id, relative_path: `DCIM/${day}_120000_F.mp4`, source_path: `/case/DCIM/${id}.mp4`,
  size_bytes: 1000, extension: 'mp4', duration_seconds: 60, ...extra,
});
const videos = [
  vid('vid_sound', '20260910', { ffprobe_ok: true, audio_codec: 'aac', relative_path: 'DCIM/20260910_120000_F.mp4', sha256: 'a'.repeat(64) }),
  vid('vid_silent', '20260910', { ffprobe_ok: true, audio_codec: 'aac', relative_path: 'DCIM/20260910_120100_F.mp4', sha256: 'b'.repeat(64) }),
  vid('vid_none', '20260911', { ffprobe_ok: true, relative_path: 'DCIM/20260911_120000_F.mp4' }),
  vid('vid_track', '20260911', { ffprobe_ok: true, audio_codec: 'aac', relative_path: 'EVENT/20260911_130000_R.mp4' }),
  vid('vid_err', '20260912', { ffprobe_ok: true, audio_codec: 'pcm_s16le', relative_path: 'DCIM/20260912_120000_F.mp4' }),
  vid('vid_stale', '20260912', { ffprobe_ok: true, audio_codec: 'aac', relative_path: 'DCIM/20260912_130000_F.mp4' }),
  vid('vid_unprobed', '20260912', { relative_path: 'DCIM/20260912_140000_F.mp4' }),
];
const DATA = {
  manifest: { case_id: 'FT-AUDIO', title: 'audio' }, caseDir: '/case', scan: { videos },
  carveLog: [{ id: 'carve_1', output_path: 'artifacts/carved/carve_1.mp4', size_bytes: 10 }],
  filesystemLog: [], validationLog: [], anomalyLog: [], flsEntries: [],
  thumbs: {}, annotations: {}, deepfake: {}, telemetry: {}, proxies: [],
  audioLevels: {
    vid_sound: { r: 'sound', max: -12.3, mean: -38.04, sec: 60, t: 1760000000 },
    vid_silent: { r: 'silent', max: -91.02, mean: -96, sec: 60, t: 1760000000 },
    vid_err: { r: 'error', sec: 0, t: 1760000000 },
    vid_stale: { r: 'stale', max: -20, mean: -40, sec: 60, t: 1760000000 },
  },
  joinLog: [{
    schema_version: 1, event: 'join-video', joined_unix: 1760000100,
    output_path: 'artifacts/joined/join_F_20260910-120000_n2.mp4', output_sha256: 'c'.repeat(64), output_size_bytes: 2000,
    manifest_path: 'artifacts/joined/join_F_20260910-120000_n2.manifest.json', manifest_sha256: 'd'.repeat(64),
    plan_id: 'p1', segment_count: 2, source_ids: ['vid_sound', 'vid_silent'], source_sha256: ['a'.repeat(64), 'b'.repeat(64)],
    hash_mismatches: 0, order_basis: 'filename-time', channel: 'F', tolerance_seconds: 1, gaps: 0, overlaps: 1,
    mode: 'stream-copy', duration_check_ok: true,
  }],
};

const html = viewerPageHtml({ data: `window.__FRAMETRACE_DATA__ = ${JSON.stringify(DATA)};`, js: viewerBundleJs(root), root });

const results = [];
const check = (name, ok, detail = '') => {
  results.push(ok);
  console.log(`${ok ? 'PASS' : 'FAIL'}  ${name}${detail ? '  — ' + detail : ''}`);
};

// records-meta could not read the measurement log: an empty map beside
// audioLevelsError (served at /err).
const errHtml = viewerPageHtml({ data: `window.__FRAMETRACE_DATA__ = ${JSON.stringify({ ...DATA, audioLevels: {}, audioLevelsError: '소리 측정 로그 체인이 손상되었습니다 (line 3)' })};`, js: viewerBundleJs(root), root });
// R9 U1/U3/U5 (track I P3 ↔ U): every log entry carries `trust` ("auth" |
// "sc" | "unauth" | "pc:<key id>"; absent = an older server). Served at /trust.
const tv = (id, extra) => vid(id, '20260913', { ffprobe_ok: true, relative_path: `DCIM/20260913_${id.slice(-2)}0000_F.mp4`, ...extra });
const vl = (id, status, trust) => ({ target_path: `/case/DCIM/${id}.mp4`, validation_status: status, target_sha256: 'e'.repeat(64), ...(trust === undefined ? {} : { trust }) });
const TRUST = {
  ...DATA,
  manifest: { case_id: 'FT-TRUST', title: 'trust' },
  scan: { videos: [tv('t_auth'), tv('t_sc'), tv('t_legacy'), tv('t_unauth'), tv('t_otherpc'), tv('t_newer'), tv('t_audiopc', { ffprobe_ok: false, audio_codec: '' })] },
  validationLog: [
    vl('t_auth', 'ffprobe-video-stream-confirmed', 'auth'),
    vl('t_sc', 'ffprobe-video-stream-confirmed', 'sc'),
    vl('t_legacy', 'ffprobe-video-stream-confirmed'),
    { ...vl('t_unauth', 'ffprobe-video-stream-confirmed', 'unauth'), trust_note: '이 PC에서 인증되지 않음 — HMAC 없음' },
    vl('t_otherpc', 'ffprobe-video-stream-confirmed', 'pc:pc-b'),
    vl('t_newer', 'validation-failed', 'auth'),
    vl('t_newer', 'ffprobe-video-stream-confirmed', 'unauth'),
  ],
  carveLog: [
    { id: 'carve_t', output_path: 'artifacts/carved/carve_t.mp4', size_bytes: 10, validation_status: 'candidate-unvalidated', trust: 'auth' },
    { id: 'carve_t', output_path: 'artifacts/carved/carve_t_forged.mp4', size_bytes: 99, validation_status: 'ffprobe-video-stream-confirmed', trust: 'unauth' },
  ],
  joinLog: [{ ...DATA.joinLog[0], trust: 'unauth' }],
  audioLevels: { t_audiopc: { r: 'unverified', pc: 'pc-z' } },
  // R10 U3: records-meta's per-lane chain state (a bundle may carry it too).
  logChains: { validationLog: { path: 'evidence/logs/validation.jsonl', status: 'not-verified', entries: 7, unauthenticated: 3, detail: '이 PC에서 인증되지 않은 줄 3개 (전체 7개 중)' } },
};
const trustHtml = viewerPageHtml({ data: `window.__FRAMETRACE_DATA__ = ${JSON.stringify(TRUST)};`, js: viewerBundleJs(root), root });
// R10 U5: the same records served by an API (/live/api/records, /live/api/
// records-meta) — trust codes computed by this server, so "이 PC" wording.
const liveHtml = viewerPageHtml({ js: viewerBundleJs(root), root });
const liveApi = (url) => {
  const u = new URL(url, 'http://x');
  if (u.pathname === '/api/records') return { ok: true, total: TRUST.scan.videos.length, videos: Number(u.searchParams.get('offset')) ? [] : TRUST.scan.videos };
  if (u.pathname === '/api/records-meta') return { ok: true, ...TRUST, scan: { ...TRUST.scan, videos: undefined } };
  return null;
};
const server = createServer((req, res) => {
  if (req.url.startsWith('/api/')) {
    const body = liveApi(req.url);
    res.statusCode = body ? 200 : 404;
    res.setHeader('Content-Type', 'application/json');
    return res.end(JSON.stringify(body || { ok: false, error: 'not here' }));
  }
  res.setHeader('Content-Type', 'text/html; charset=utf-8');
  res.end(req.url.startsWith('/err') ? errHtml : req.url.startsWith('/trust') ? trustHtml : req.url.startsWith('/live') ? liveHtml : html);
});
await new Promise((r) => server.listen(0, '127.0.0.1', r));
const base = `http://127.0.0.1:${server.address().port}/`;
const browser = await chromium.launch();
try {
  const ctx = await browser.newContext({ viewport: { width: 1600, height: 900 }, acceptDownloads: true });
  const page = await ctx.newPage();
  const errors = [];
  page.on('pageerror', (e) => errors.push(String(e)));
  await page.goto(base);
  await page.waitForSelector('.card');

  // 1. icons + labels
  const flags = await page.evaluate(() => Object.fromEntries([...document.querySelectorAll('.card[data-id]')].map((c) => {
    const f = c.querySelector('.audio-flag');
    return [c.dataset.id, f ? { href: f.querySelector('use')?.getAttribute('href'), label: f.getAttribute('aria-label'), role: f.getAttribute('role') } : null];
  })));
  const want = {
    vid_sound: ['#i-audio', '소리 있음 · 측정 최대 −12 dB'],
    vid_silent: ['#i-audio-silent', '무음 트랙 · 측정 최대 −91 dB'],
    vid_none: ['#i-mute', '오디오 트랙 없음'],
    vid_track: ['#i-audio-track', '오디오 트랙 있음 · 소리 미측정'],
    vid_err: ['#i-audio-track', '오디오 트랙 있음 · 소리 측정 실패 — 미확인'],
    vid_stale: ['#i-audio-track', '오디오 트랙 있음 · 측정 후 파일 변경 — 미확인'],
  };
  for (const [id, [href, label]] of Object.entries(want)) {
    const f = flags[id];
    check(`card icon ${id}`, !!f && f.href === href && f.label === label && f.role === 'img', JSON.stringify(f));
  }
  check('unprobed / carved / joined records show no sound flag', !flags.vid_unprobed && !flags.carve_1 && !flags['join:join_F_20260910-120000_n2.mp4'], JSON.stringify([flags.vid_unprobed, flags.carve_1]));
  const sprite = await page.evaluate(() => ['i-audio-silent', 'i-audio-track'].map((id) => {
    const s = document.getElementById(id);
    return !!s && s.getAttribute('viewBox') === '0 0 16 16' && /currentColor/.test(s.outerHTML) && !/#[0-9a-f]{3,6}/i.test(s.outerHTML);
  }));
  check('new sprite icons exist and use currentColor only', sprite.every(Boolean), JSON.stringify(sprite));

  // 2. detail text
  const detailOf = async (id) => {
    await page.locator(`.card[data-id="${id}"]`).click();
    return page.evaluate(() => {
      const dts = [...document.querySelectorAll('#summaryList dt')];
      const dt = dts.find((d) => d.textContent === '소리');
      return dt ? dt.nextElementSibling.textContent : null;
    });
  };
  const details = {
    vid_sound: '소리 있음 — 측정: 최대 −12.3 dB · 평균 −38.0 dB (aac)',
    vid_silent: '무음 트랙 — 측정: 최대 −91.0 dB (기준 −70 dB 이하)',
    vid_none: '오디오 트랙 없음 (ffprobe)',
    vid_track: '오디오 트랙 있음 (aac) — 소리 미측정',
    vid_err: '오디오 트랙 있음 (pcm_s16le) — 소리 측정 실패, 미확인',
    vid_unprobed: '미확인',
  };
  for (const [id, text] of Object.entries(details)) {
    const got = await detailOf(id);
    check(`detail 소리 ${id}`, got === text, String(got));
  }
  const leftover = await page.evaluate(() => document.getElementById('ftViewer')?.innerText.includes('음성') || document.body.innerText.includes('음성'));
  check('no "음성" label left in the page', !leftover);

  // 3-5. facet
  const audioFacet = () => page.evaluate(() => [...document.querySelectorAll('#facetTree .tree-item[data-facet="audio"]')]
    .map((n) => [n.dataset.key, n.querySelector('.muted')?.textContent ?? null, n.getAttribute('aria-pressed')]));
  const sections = await page.evaluate(() => [...document.querySelectorAll('#facetTree .tree-sec')].map((n) => n.textContent));
  check('facet tree has the 소리 section after 날짜', JSON.stringify(sections) === JSON.stringify(['녹화 유형', '출처', '날짜', '소리']), JSON.stringify(sections));
  const f0 = await audioFacet();
  check('소리 values in fixed order with counts', JSON.stringify(f0.map((x) => x.slice(0, 2))) === JSON.stringify([['', null], ['sound', '1'], ['silent', '1'], ['none', '1'], ['unknown', '6']]), JSON.stringify(f0));
  await page.selectOption('#kind', 'carved');
  await page.waitForTimeout(150);
  const f1 = await audioFacet();
  check('another facet (출처=카빙) changes the 소리 counts', JSON.stringify(f1.map((x) => x[1])) === JSON.stringify([null, '0', '0', '0', '1']), JSON.stringify(f1));
  await page.selectOption('#kind', '');
  await page.waitForTimeout(150);
  await page.locator('#facetTree .tree-item[data-facet="audio"][data-key="sound"]').click();
  await page.waitForTimeout(150);
  const f2 = await audioFacet();
  check('picking a 소리 value leaves its own counts', JSON.stringify(f2.map((x) => x[1])) === JSON.stringify(f0.map((x) => x[1])), JSON.stringify(f2));
  check('picked value is pressed', f2.find((x) => x[0] === 'sound')[2] === 'true');
  const cards = await page.evaluate(() => [...document.querySelectorAll('.card[data-id]')].map((c) => c.dataset.id));
  check('picking 소리 있음 filters the grid', JSON.stringify(cards) === JSON.stringify(['vid_sound']), JSON.stringify(cards));
  const kindTree = await page.evaluate(() => {
    const out = {};
    let sec = '';
    for (const n of document.querySelectorAll('#facetTree > *')) {
      if (n.classList.contains('tree-sec')) { sec = n.textContent; continue; }
      if (sec === '출처' || sec === '날짜') out[sec + ':' + n.querySelector('span').textContent] = n.querySelector('.muted')?.textContent ?? null;
    }
    return out;
  });
  check('출처/날짜 facets honour the sound filter', kindTree['출처:원본 (논리 파일)'] === '1' && kindTree['날짜:2026-09-10'] === '1' && !('출처:카빙 후보' in kindTree), JSON.stringify(kindTree));
  // date facet still filters and is not collapsed by the date pick
  await page.locator('#facetTree .tree-item[data-facet="audio"][data-key=""]').click();
  await page.waitForTimeout(150);
  await page.locator('#facetTree .tree-item[data-pick="2026-09-12"]').click();
  await page.waitForTimeout(150);
  const f3 = await audioFacet();
  const dayCards = await page.locator('.card[data-id]').count();
  check('date pick filters the grid and narrows 소리 counts', dayCards === 3 && JSON.stringify(f3.map((x) => x[1])) === JSON.stringify([null, '0', '0', '0', '3']), `${dayCards} ${JSON.stringify(f3)}`);
  await page.evaluate(() => { const b = document.querySelector('#activeFilters [data-af="all"]') || document.querySelector('#activeFilters [data-af="date"]'); b?.click(); });
  await page.evaluate(() => { for (const id of ['dateFrom', 'dateTo']) { const el = document.getElementById(id); el.value = ''; el.dispatchEvent(new Event('change', { bubbles: true })); } });
  await page.waitForTimeout(200);

  // 8. memo: sort only -> no recompute; a filter -> recompute
  const setSelect = (id, v) => page.evaluate(([i, val]) => { const el = document.getElementById(i); el.value = val; el.dispatchEvent(new Event('change', { bubbles: true })); }, [id, v]);
  const gen = () => page.evaluate(() => Number(document.getElementById('facetTree').dataset.facetGen));
  const g0 = await gen();
  await setSelect('sortBy', 'name');
  await page.waitForTimeout(150);
  const g1 = await gen();
  await setSelect('sortBy', 'id');
  await page.waitForTimeout(150);
  check('sort change keeps the facet memo', g1 === g0 && (await gen()) === g0, `${g0} -> ${g1}`);
  await page.locator('#facetTree .tree-item[data-facet="audio"][data-key="none"]').click();
  await page.waitForTimeout(150);
  const g2 = await gen();
  check('a 소리 filter change recomputes the facets', g2 > g0, `${g0} -> ${g2}`);
  await page.locator('#facetTree .tree-item[data-facet="audio"][data-key=""]').click();
  await page.waitForTimeout(150);

  // facet keyboard focus follows the ITEM (section + value), not its
  // position: picking a date shrinks the 녹화 유형 / 출처 sections above it.
  const focusInfo = () => page.evaluate(() => ({ sec: document.activeElement?.dataset?.sec, pick: document.activeElement?.dataset?.pick }));
  const before = await page.evaluate(() => [...document.querySelectorAll('#facetTree .tree-item')].findIndex((n) => n.dataset.pick === '2026-09-12'));
  await page.locator('#facetTree .tree-item[data-pick="2026-09-12"]').focus();
  await page.keyboard.press('Enter');
  await page.waitForTimeout(150);
  const fDate = await focusInfo();
  const afterIdx = await page.evaluate(() => [...document.querySelectorAll('#facetTree .tree-item')].findIndex((n) => n.dataset.pick === '2026-09-12'));
  check('Enter on a date keeps focus on that date although items above it changed', fDate.sec === 'date' && fDate.pick === '2026-09-12' && afterIdx !== before, `${JSON.stringify(fDate)} idx ${before}→${afterIdx}`);
  await page.locator('#facetTree .tree-item[data-facet="audio"][data-key="unknown"]').focus();
  await page.keyboard.press('Enter');
  await page.waitForTimeout(150);
  const fAudio = await page.evaluate(() => ({ sec: document.activeElement?.dataset?.sec, key: document.activeElement?.dataset?.key }));
  check('Enter on a 소리 value keeps focus on that value', fAudio.sec === 'audio' && fAudio.key === 'unknown', JSON.stringify(fAudio));
  await page.locator('#facetTree .tree-item[data-facet="audio"][data-key=""]').click();
  await page.evaluate(() => { for (const id of ['dateFrom', 'dateTo']) { const el = document.getElementById(id); el.value = ''; el.dispatchEvent(new Event('change', { bubbles: true })); } });
  await page.waitForTimeout(200);

  // 6. keys
  const total = await page.locator('.card[data-id]').count();
  check('all records back after clearing filters', total === 9, String(total));
  await page.locator('.card[data-id="vid_none"]').click();
  await page.evaluate(() => document.activeElement?.blur());
  const active = () => page.evaluate(() => document.querySelector('.card.active')?.dataset.id);
  const a0 = await active();
  await page.keyboard.press('j');
  const a1 = await active();
  await page.keyboard.press('k');
  const a2 = await active();
  check('j / k move the active card', a1 && a1 !== a0 && a2 === a0, `${a0} ${a1} ${a2}`);
  await page.keyboard.press('x');
  const sel = await page.evaluate((id) => document.querySelector(`.card[data-id="${id}"]`).getAttribute('aria-selected'), a0);
  check('x toggles the selection', sel === 'true', String(sel));
  await page.keyboard.press('1');
  await page.waitForTimeout(100);
  const marked = await page.evaluate((id) => !!document.querySelector(`.card[data-id="${id}"] .mark-chip`), a0);
  await page.keyboard.press('k');
  await page.keyboard.press('0');
  await page.waitForTimeout(100);
  const cleared = await page.evaluate((id) => !document.querySelector(`.card[data-id="${id}"] .mark-chip`), a0);
  check('1 marks and 0 clears', marked && cleared, `${marked} ${cleared}`);

  // 7. CSV
  await page.locator('.card[data-id="vid_sound"] input[data-check]').check();
  await page.locator('.card[data-id="vid_none"] input[data-check]').check();
  // Listen before clicking: a fast download can fire before the wait starts.
  const dlWait = page.waitForEvent('download', { timeout: 15000 }).catch(() => null);
  await page.evaluate(() => document.getElementById('btnDownloadCsv').click());
  const dl = await dlWait;
  let csv = '';
  if (dl) csv = readFileSync(await dl.path(), 'utf8');
  const lines = csv.replace(/^﻿/, '').split(/\r?\n/);
  const header = (lines[0] || '').split(',');
  const row = lines.find((l) => l.startsWith('vid_sound,')) || '';
  const cells = row.split(',');
  const at = (name) => cells[header.indexOf(name)];
  check('CSV has the sound columns', ['audio_state', 'audio_max_db', 'audio_mean_db'].every((c) => header.includes(c)), lines[0]);
  check('CSV sound values are the measured ones', at('audio_state') === 'sound' && at('audio_max_db') === '-12.3' && at('audio_mean_db') === '-38.04', row);
  // The row must exist: a missing row is a failure, never a pass.
  const noneLine = lines.find((l) => l.startsWith('vid_none,'));
  const noneRow = noneLine ? noneLine.split(',') : null;
  check('CSV unmeasured row is present with state none and empty dB cells', !!noneRow && noneRow[header.indexOf('audio_state')] === 'none' && noneRow[header.indexOf('audio_max_db')] === '' && noneRow[header.indexOf('audio_mean_db')] === '', noneLine === undefined ? 'vid_none row missing' : noneLine);

  // 9. derived joined record
  const jid = 'join:join_F_20260910-120000_n2.mp4';
  const jcard = await page.evaluate((id) => {
    const c = document.querySelector(`.card[data-id="${CSS.escape(id)}"]`);
    if (!c) return null;
    return { kind: c.querySelector('.kind-badge')?.textContent, status: c.querySelector('.badge')?.textContent, statusIcon: !!c.querySelector('.badge use'), channel: c.querySelector('.channel-badge')?.textContent, time: c.querySelector('.time-text')?.textContent };
  }, jid);
  check('joined record card: kind 이어붙임, status 파생물 (icon + text), channel 전방', !!jcard && jcard.kind === '이어붙임' && jcard.status === '파생물' && jcard.statusIcon && /전방/.test(jcard.channel || ''), JSON.stringify(jcard));
  await page.locator(`.card[data-id="${jid}"]`).click();
  const jval = await page.evaluate(() => {
    const it = document.querySelector('#validationList .join-sources');
    return it ? { head: it.querySelector('strong').textContent, items: [...it.querySelectorAll('li code:first-of-type')].map((c) => c.textContent) } : null;
  });
  check('joined record lists its sources in the 검증 list', !!jval && jval.head === '이어붙인 원본 2개' && JSON.stringify(jval.items) === JSON.stringify(['vid_sound', 'vid_silent']), JSON.stringify(jval));
  const jsha = await page.evaluate(() => [...document.querySelectorAll('#metaList dt')].find((d) => d.textContent === 'SHA-256')?.nextElementSibling.textContent);
  check('joined record carries the output SHA-256', jsha === 'c'.repeat(64), String(jsha));
  await page.selectOption('#kind', 'joined');
  await page.waitForTimeout(150);
  check('출처 filter "이어붙인 영상" shows only the derived record', (await page.locator('.card[data-id]').count()) === 1);
  check('join menu item is shown on a served page', await page.evaluate(() => !document.getElementById('btnJoinSelected').hidden));

  // file:// viewer (no server): the join entry is not offered at all.
  const dir = mkdtempSync(join(tmpdir(), 'ft-audio-'));
  try {
    const file = join(dir, 'evidence-viewer.html');
    writeFileSync(file, html);
    const fpage = await ctx.newPage();
    await fpage.goto(pathToFileURL(file).href);
    await fpage.waitForSelector('.card');
    check('join menu item hidden in the file:// viewer', await fpage.evaluate(() => document.getElementById('btnJoinSelected').hidden === true));
    await fpage.close();
  } finally { rmSync(dir, { recursive: true, force: true }); }

  // audioLevelsError: a track reads "측정 결과를 읽지 못함" (never "미측정"),
  // with ONE notice per case/error — not again after a reload.
  const epage = await ctx.newPage();
  epage.on('pageerror', (e) => errors.push('err page: ' + String(e)));
  await epage.goto(base + 'err');
  await epage.waitForSelector('.card');
  const errToasts = () => epage.evaluate(() => [...document.querySelectorAll('#toastHost .toast')].map((t) => t.textContent).filter((t) => /소리 측정 기록을 읽지 못했습니다/.test(t)));
  const t1 = await errToasts();
  check('audioLevelsError: one notice naming the read failure', t1.length === 1 && /체인이 손상/.test(t1[0]), JSON.stringify(t1));
  const eflag = await epage.evaluate(() => {
    const f = document.querySelector('.card[data-id="vid_track"] .audio-flag');
    return f ? { href: f.querySelector('use')?.getAttribute('href'), label: f.getAttribute('aria-label') } : null;
  });
  check('audioLevelsError: track icon says 측정 결과를 읽지 못함', !!eflag && eflag.href === '#i-audio-track' && eflag.label === '오디오 트랙 있음 · 측정 결과를 읽지 못함 — 미확인', JSON.stringify(eflag));
  await epage.locator('.card[data-id="vid_sound"]').click();
  const edetail = await epage.evaluate(() => {
    const dt = [...document.querySelectorAll('#summaryList dt')].find((d) => d.textContent === '소리');
    return dt ? dt.nextElementSibling.textContent : null;
  });
  check('audioLevelsError: detail says 측정 결과를 읽지 못함, not 미측정', edetail === '오디오 트랙 있음 (aac) — 측정 결과를 읽지 못함, 미확인', String(edetail));
  const noneFlag = await epage.evaluate(() => document.querySelector('.card[data-id="vid_none"] .audio-flag')?.getAttribute('aria-label'));
  check('audioLevelsError: a record without a track still reads 오디오 트랙 없음', noneFlag === '오디오 트랙 없음', String(noneFlag));
  await epage.reload();
  await epage.waitForSelector('.card');
  await epage.waitForTimeout(300);
  check('audioLevelsError: the notice is not repeated after a reload', (await errToasts()).length === 0, JSON.stringify(await errToasts()));
  await epage.close();

  // R9 U1: an entry this PC could not authenticate never makes a record
  // verified / derived, nor counts in 검증됨; "sc" keeps the value with
  // "자기 일관성만"; a later unauthenticated line never replaces an
  // authenticated one. U3: another PC's key is "이 PC에서 인증 불가(변조
  // 판정 아님)", also for a record without a probed track. U5: exports
  // carry audio_trust / status_trust. U7: "case"-key drafts of an older
  // build are announced once, never adopted.
  const tpage = await ctx.newPage();
  tpage.on('pageerror', (e) => errors.push('trust page: ' + String(e)));
  await tpage.addInitScript(() => {
    if (sessionStorage.getItem('seeded')) return;
    sessionStorage.setItem('seeded', '1');
    localStorage.setItem('ft.draft.case.w-old.vid_x', JSON.stringify({ v: 1, mark: { status: 'important' } }));
    localStorage.setItem('ft.draft.case.w-old.vid_y', JSON.stringify({ v: 1, note: 'old' }));
  });
  await tpage.goto(base + 'trust');
  await tpage.waitForSelector('.card');
  const badges = await tpage.evaluate(() => Object.fromEntries([...document.querySelectorAll('.card[data-id]')].map((c) => {
    const b = c.querySelector('.badge-row .badge');
    return [c.dataset.id, b ? { text: b.textContent, title: b.getAttribute('title') || '', icon: b.querySelector('use')?.getAttribute('href') } : null];
  })));
  check('R9 U1: authenticated validation → 검증됨', badges.t_auth?.text === '검증됨' && !/자기 일관성/.test(badges.t_auth.title), JSON.stringify(badges.t_auth));
  check('R9 U1: sc validation → 검증됨, tooltip 자기 일관성만(인증 아님)', badges.t_sc?.text === '검증됨' && /자기 일관성만\(인증 아님\)/.test(badges.t_sc.title), JSON.stringify(badges.t_sc));
  check('R9 U1: legacy (no trust field) → as before, 검증됨, nothing called authenticated', badges.t_legacy?.text === '검증됨' && !/인증/.test(badges.t_legacy.title), JSON.stringify(badges.t_legacy));
  check('R9 U1: unauth validation → 인증 안 됨 (lock), never 검증됨', badges.t_unauth?.text === '인증 안 됨' && badges.t_unauth.icon === '#i-lock', JSON.stringify(badges.t_unauth));
  // R10 U5: this page is a bundle — its trust codes were computed on the PC that generated it.
  check('R9 U1/U3 + R10 U5: another PC\'s key → 생성 PC 기준 인증 불가 (bundle), tooltip names the key (not a tampering verdict)', badges.t_otherpc?.text === '생성 PC 기준 인증 불가' && /변조 판정 아님/.test(badges.t_otherpc.title) && /키 pc-b 보유 PC에서 확인/.test(badges.t_otherpc.title) && !/이 PC/.test(badges.t_otherpc.title), JSON.stringify(badges.t_otherpc));
  // R10 U1 (P7): the newest entry decides — an older authenticated result is never rolled back to.
  check('R10 U1: a later unauthenticated validation → 인증 안 됨, "더 최근 인증 안 된 항목 있음" (not the older 검증 실패)', badges.t_newer?.text === '인증 안 됨' && /^더 최근 인증 안 된 항목 있음/.test(badges.t_newer.title), JSON.stringify(badges.t_newer));
  check('R10 U1: carve id — the newer unauthenticated entry makes the record 인증 안 됨 (never the older authenticated one as confirmed)', badges.carve_t?.text === '인증 안 됨' && /^더 최근 인증 안 된 항목 있음/.test(badges.carve_t.title), JSON.stringify(badges.carve_t));
  check('R10 U3/U5: trust_note in the badge tooltip, "이 PC" → "생성 PC" in a bundle', /생성 PC에서 인증되지 않음 — HMAC 없음/.test(badges.t_unauth?.title || '') && !/이 PC/.test(badges.t_unauth?.title || ''), JSON.stringify(badges.t_unauth));
  check('R9 U1: an unauthenticated join line → 인증 안 됨, never 파생물', badges[jid]?.text === '인증 안 됨', JSON.stringify(badges[jid]));
  const metric = await tpage.evaluate(() => document.getElementById('metricVerified')?.textContent);
  check('R9 U1: 검증됨 counts auth + sc + legacy only (3), not unauth / other-PC lines', metric === '3', String(metric));
  await tpage.locator('.card[data-id="t_newer"]').click();
  const vitems = await tpage.evaluate(() => [...document.querySelectorAll('#validationList .validation-item')].map((i) => i.textContent.replace(/\s+/g, ' ').trim()));
  check('R9 U1: the detail list shows the unauthenticated line marked 인증 안 됨', vitems.some((t) => /ffprobe-video-stream-confirmed/.test(t) && /인증 안 됨/.test(t)) && vitems.some((t) => /validation-failed/.test(t) && !/인증 안 됨/.test(t)), JSON.stringify(vitems));
  check('R10 U3: the log list starts with the lane state (logChains), then "더 최근 인증 안 된 항목 있음"', /^검증 로그 인증 안 된 줄 3개$/.test(vitems[0] || '') && /^더 최근 인증 안 된 항목 있음/.test(vitems[1] || ''), JSON.stringify(vitems.slice(0, 2)));
  const chainTip = await tpage.evaluate(() => document.querySelector('#validationList .log-chain')?.getAttribute('title'));
  check('R10 U3/U5: the lane state tooltip is the server detail ("생성 PC" in a bundle)', chainTip === '생성 PC에서 인증되지 않은 줄 3개 (전체 7개 중)', String(chainTip));
  await tpage.locator('.card[data-id="t_unauth"]').click();
  const unNote = await tpage.evaluate(() => document.querySelector('#validationList .trust-note')?.textContent);
  check('R10 U3: the detail shows the trust_note under the entry', unNote === '생성 PC에서 인증되지 않음 — HMAC 없음', String(unNote));
  await tpage.locator('.card[data-id="t_auth"]').click();
  const authItems = await tpage.evaluate(() => [...document.querySelectorAll('#validationList .validation-item')].map((i) => i.className));
  check('R10 U1: no "더 최근" row on a record whose newest entry is authenticated', !authItems.some((c) => /status-newer/.test(c)) && authItems.some((c) => /log-chain/.test(c)), JSON.stringify(authItems));
  const apc = await tpage.evaluate(() => {
    const f = document.querySelector('.card[data-id="t_audiopc"] .audio-flag');
    return f ? { href: f.querySelector('use')?.getAttribute('href'), label: f.getAttribute('aria-label') } : null;
  });
  check('R9 U3 + R10 U5: an unknown sound state signed by another PC\'s key says so on the card (생성 PC 기준 in a bundle)', !!apc && apc.href === '#i-lock' && /생성 PC 기준 인증 불가\(변조 판정 아님\)/.test(apc.label) && /키 pc-z 보유 PC에서 확인/.test(apc.label) && !/검증 실패|이 PC/.test(apc.label), JSON.stringify(apc));
  await tpage.locator('.card[data-id="t_audiopc"]').click();
  const apcDetail = await tpage.evaluate(() => {
    const dt = [...document.querySelectorAll('#summaryList dt')].find((d) => d.textContent === '소리');
    return dt ? dt.nextElementSibling.textContent : null;
  });
  check('R9 U3 + R10 U5: the detail says 생성 PC 기준 인증 불가(변조 판정 아님) — 키 pc-z', /생성 PC 기준 인증 불가\(변조 판정 아님\) — 키 pc-z 보유 PC에서 확인/.test(apcDetail || '') && !/이 PC/.test(apcDetail || ''), String(apcDetail));
  for (const id of ['t_sc', 't_unauth', 't_audiopc']) await tpage.locator(`.card[data-id="${id}"] input[data-check]`).check();
  const tdlWait = tpage.waitForEvent('download', { timeout: 15000 }).catch(() => null);
  await tpage.evaluate(() => document.getElementById('btnDownloadCsv').click());
  const tdl = await tdlWait;
  const tcsv = tdl ? readFileSync(await tdl.path(), 'utf8').replace(/^﻿/, '').split(/\r?\n/) : [];
  const th = (tcsv[0] || '').split(',');
  const tcell = (id, col) => ((tcsv.find((l) => l.startsWith(id + ',')) || '').split(','))[th.indexOf(col)];
  check('R9 U5: CSV has audio_trust and status_trust columns', th.includes('audio_trust') && th.includes('status_trust'), tcsv[0] || '');
  check('R9 U5: CSV trust values (sc / unauth / pc:<id>)', tcell('t_sc', 'status_trust') === 'sc' && tcell('t_unauth', 'status_trust') === 'unauth' && tcell('t_unauth', 'status') === 'unauthenticated' && tcell('t_audiopc', 'audio_trust') === 'pc:pc-z', JSON.stringify(tcsv.slice(0, 4)));
  const legacyNotice = await tpage.evaluate(() => document.getElementById('legacyCaseDraftsNotice')?.textContent || '');
  check('R9 U7: drafts kept under the old "case" key are announced (count, not applied)', /임시 편집 2건/.test(legacyNotice) && /반영하지 않았습니다/.test(legacyNotice), legacyNotice);
  const adopted = await tpage.evaluate(() => !!document.querySelector('.card[data-id="vid_x"], .card .mark-chip'));
  check('R9 U7: none of them adopted', !adopted);
  await tpage.reload();
  await tpage.waitForSelector('.card');
  check('R9 U7: said once — not again after a reload', await tpage.evaluate(() => !document.getElementById('legacyCaseDraftsNotice')));
  await tpage.close();

  // R10 U5: the same data from this server's API keeps "이 PC"; logChains travel through the loader.
  const lpage = await ctx.newPage();
  lpage.on('pageerror', (e) => errors.push('live page: ' + String(e)));
  await lpage.goto(base + 'live/');
  await lpage.waitForSelector('.card[data-id="t_otherpc"]');
  const lb = await lpage.evaluate(() => {
    const b = document.querySelector('.card[data-id="t_otherpc"] .badge-row .badge');
    const u = document.querySelector('.card[data-id="t_unauth"] .badge-row .badge');
    return { text: b?.textContent, title: b?.getAttribute('title'), unTitle: u?.getAttribute('title') };
  });
  check('R10 U5: API data says 이 PC에서 인증 불가 (not 생성 PC)', lb.text === '이 PC에서 인증 불가' && /^이 PC에서 인증 불가\(변조 판정 아님\) — 키 pc-b/.test(lb.title || '') && /이 PC에서 인증되지 않음 — HMAC 없음/.test(lb.unTitle || '') && !/생성 PC/.test((lb.title || '') + (lb.unTitle || '')), JSON.stringify(lb));
  await lpage.locator('.card[data-id="t_newer"]').click();
  const lchain = await lpage.evaluate(() => { const c = document.querySelector('#validationList .log-chain'); return c ? [c.textContent.replace(/\s+/g, ' ').trim(), c.getAttribute('title')] : null; });
  check('R10 U3: records-meta logChains reach the viewer through the loader', !!lchain && lchain[0] === '검증 로그 인증 안 된 줄 3개' && lchain[1] === '이 PC에서 인증되지 않은 줄 3개 (전체 7개 중)', JSON.stringify(lchain));
  await lpage.close();

  check('no page errors', !errors.length, errors.join(' | '));
} finally {
  await browser.close();
  server.close();
}
const failed = results.filter((r) => !r).length;
console.log(failed ? `\n${failed} check(s) failed` : '\nall checks passed');
process.exit(failed ? 1 : 0);
