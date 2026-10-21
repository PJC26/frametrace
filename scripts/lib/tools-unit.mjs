// Tools track U (docs/PLAN_TOOLS_2026-10-20.md U1-U2 + "§S as implemented",
// contract §28): assets/app/56-tools.js in a VM with stubbed network /
// dialog / timers, fed the real track S shapes.
// Run from `node scripts/verify-review-batch.mjs --unit`.
import { readFileSync } from 'node:fs';
import { runInNewContext } from 'node:vm';
import assert from 'node:assert/strict';
import { appBundleJs } from './app-bundle.mjs';
import { TOOLS_FIXTURE, routeTools, apiCancelReply, INSTALL_CANCELLED as INSTALL_CANCELLED_U } from './tools-mock.mjs';

const read = (rel) => readFileSync(new URL('../../' + rel, import.meta.url), 'utf8');

export async function toolsTrackU() {
  const appJs = appBundleJs();
  const toolsSrc = read('assets/app/56-tools.js').split('(function htWire()')[0];
  const finSrc = read('assets/app/41-finalize.js');
  const formSrc = read('assets/app/30-form.js');
  const startSrc = read('assets/app/50-start.js');
  const html = read('assets/examiner_app.html');
  const legacyHtml = read('assets/legacy/examiner_app.html');
  const fmtBytesSrc = appJs.match(/^function fmtBytes\([^]*?^}/m)[0];
  const escSrc = appJs.match(/^const esc = .*$/m)[0];
  const setEnvOffSrc = appJs.match(/^function setEnvOff\([^]*?^}/m)[0];
  const finApplyEnvSrc = finSrc.match(/^function finApplyEnv\(\) \{[^]*?^}/m)[0];
  const failed = [];
  const row = async (name, body) => {
    try { await body(); console.log('PASS VM ' + name); } catch (err) { failed.push(name); console.log('FAIL VM ' + name + ': ' + (err && err.message || err)); }
  };
  const tick = async (n = 8) => { for (let i = 0; i < n; i++) await new Promise((r) => setImmediate(r)); };
  const plain = (v) => JSON.parse(JSON.stringify(v));
  // The real GET /api/tools rows (scripts/lib/tools-mock.mjs, copied from
  // tools/manifest.json + tool_json()).
  const [FF, EWF, TSK, DF] = TOOLS_FIXTURE();
  const OPT = { ...EWF, name: 'x-opt', label: 'X 도구', installable: true, unavailable_reason: null, not_installable_reason: null, manual_hint: null, source_host: 'github.com', size_bytes: 4 * 1048576, download: { file_name: 'x-opt-1.0-win64.zip', url: 'https://github.com/x/x-opt-1.0-win64.zip', host: 'github.com', size: 4 * 1048576, sha256: 'a'.repeat(64), checksum_source: 'https://github.com/x/SHA256SUMS' } };
  const DFI = { ...DF, installable: true, unavailable_reason: null, not_installable_reason: null, size_bytes: 2048, version: '0.4.2', pinned_version: '0.4.2', license: 'MIT' };
  const J = (state, tool, extra = {}) => ({ id: 7, tool, state, stage: { downloading: 'download', verifying: 'verify', extracting: 'extract', done: 'done', failed: 'failed' }[state], bytes: null, total: null, error: null, started_unix: 1, finished_unix: null, installed_files: [], audit_warning: null, ...extra });
  const el = (id, calls) => {
    const e = { id, hidden: true, innerHTML: '', textContent: '', dataset: {}, classList: { toggle() {} }, listeners: [], attrs: {}, addEventListener(t, f) { this.listeners.push(f); }, contains: () => false, querySelectorAll: () => [], setAttribute(k, v) { this.attrs[k] = v; }, closest: () => null };
    e.querySelector = (sel) => (sel === '[data-ht-focus]' && /data-ht-focus/.test(e.innerHTML) ? { focus: () => calls.focused.push(id) } : null);
    return e;
  };
  // A fresh VM: GET /api/tools and GET /api/tools/install-status answer from
  // scripts of replies (the last repeats); POSTs answer by URL.
  const make = ({ replies = [], status = [], confirm = true, post = null, cancel = { ok: false, code: 'no_install' }, extra = '' } = {}) => {
    const els = {};
    const calls = { dialogs: [], posts: [], gets: [], timers: [], loadEnv: 0, toasts: [], announced: [], focused: [] };
    const next = (q) => { const r = q.length > 1 ? q.shift() : q[0]; return r === undefined ? null : plain(r); };
    const ctx = {
      console, document: { activeElement: null }, setTimeout: (f) => { calls.timers.push(f); return calls.timers.length; }, clearTimeout() {},
      $: (id) => (els[id] ||= el(id, calls)),
      jgetSoft: async (url) => { calls.gets.push(url); return next(url === '/api/tools/install-status' ? status : replies); },
      endpointMissing: (r) => !!(r && !r.ok && (r.status === 404 || r.status === 405 || r.status === 501)),
      jpost: async (url, body) => { calls.posts.push({ url, body }); return plain(url === '/api/tools/install-cancel' ? cancel : (post || { ok: true, install: J('downloading', body.name, { stage: 'queued', bytes: 0 }) })); },
      askDialog: async (o) => { calls.dialogs.push(o); if (typeof ctx.__onDialog === 'function') await ctx.__onDialog(); return confirm; },
      announce: (m, a) => calls.announced.push([m, !!a]), toast: (m, k) => calls.toasts.push([m, k]), loadEnv: async () => { calls.loadEnv += 1; },
      hLocalePref: () => ctx.__locale || '',
    };
    runInNewContext(escSrc + '\n' + fmtBytesSrc + '\n' + setEnvOffSrc + '\n' + toolsSrc + '\n' + extra, ctx);
    const fire = async () => { const f = calls.timers.shift(); assert.ok(f, 'a poll is scheduled'); f(); await tick(); };
    return { ctx, els, calls, fire, run: (expr) => runInNewContext(expr, ctx) };
  };
  const reply = (tools, install = null) => ({ ok: true, platform: 'windows-x86_64', platform_supported: true, tools, install });
  const st = (install) => ({ ok: true, install });

  await row('Tools U1 an older server (404 / no tools) shows no panel, never an error', async () => {
    for (const r of [{ ok: false, status: 404, error: 'x' }, { ok: false, network: true, status: 0 }, { ok: true }, null]) {
      const v = make({ replies: [r] });
      await v.run('htLoad()');
      assert.equal(v.els.hToolsPanel.hidden, true, 'U1: hidden for ' + JSON.stringify(r));
      assert.equal(v.run('htNeedsTool("deepfake-lens")'), false, 'U1: an older server never asks to install');
    }
  });

  await row('Tools U1 panel (real shapes): FFmpeg warning + 설치; libewf / Sleuth Kit show the Korean manual_hint (linked), the English unavailable_reason only as a title; on-demand never listed', async () => {
    const v = make({ replies: [reply([FF, EWF, TSK, DF, { ...OPT, name: 'od', on_demand: true }, { ...OPT, name: 'x-ok', installed: true }])] });
    await v.run('htLoad()');
    const list = v.els.hToolsList.innerHTML;
    assert.equal(v.els.hToolsPanel.hidden, false, 'U1: shown when a tool is missing');
    assert.ok(/h-tool-warn" data-tool="ffmpeg"/.test(list) && list.includes('FFmpeg 없음 — 필수') && /data-tool-install="ffmpeg"[^>]*>설치</.test(list), 'U1: FFmpeg missing is a warning: ' + list);
    assert.ok(/h-tool-info" data-tool="libewf"/.test(list) && !/data-tool-install="libewf"/.test(list), 'U1: libewf info, no button');
    const ewfHint = list.match(/<li[^>]*data-tool="libewf"[^]*?<\/li>/)[0];
    assert.ok(ewfHint.includes('libewf는 공식 Windows 실행 파일이 없습니다') && ewfHint.includes('<a href="https://github.com/libyal/libewf"'), 'U1: manual_hint shown, its URL linked: ' + ewfHint);
    assert.ok(/title="The official release/.test(ewfHint) && !/>The official release/.test(ewfHint), 'U1: unavailable_reason only in a title, never as row text: ' + ewfHint);
    const tsk = list.match(/<li[^>]*data-tool="sleuthkit"[^]*?<\/li>/)[0];
    assert.ok(tsk.includes('PGP 서명(.asc)을 확인한 뒤') && tsk.includes('href="https://www.sleuthkit.org/sleuthkit/download.php"') && !/>The maintainers/.test(tsk), 'U1: TSK manual hint: ' + tsk);
    assert.ok(!/deepfake/.test(list) && !/data-tool="od"/.test(list), 'U1: on_demand / pip never in the panel');
    assert.ok(!/x-ok/.test(list), 'U1: installed tools are not listed');
    const bad = v.run('htRowHtml(htRowModel(' + JSON.stringify({ ...TSK, manual_hint: '', homepage: 'javascript:alert(1)' }) + ', null, {}))');
    assert.ok(bad.includes('여기서 설치할 수 없음') && !/javascript:/.test(bad), 'U1: no hint → fallback; non-https never a link: ' + bad);
    const home = v.run('htRowHtml(htRowModel(' + JSON.stringify({ ...TSK, manual_hint: '직접 설치하십시오' }) + ', null, {}))');
    assert.ok(home.includes('직접 설치하십시오 — 수동 설치: <a href="https://www.sleuthkit.org/sleuthkit/"'), 'U1: a hint without a URL gets the homepage: ' + home);
    const allOk = make({ replies: [reply([{ ...FF, installed: true }, DF])] });
    await allOk.run('htLoad()');
    assert.equal(allOk.els.hToolsPanel.hidden, true, 'U1: nothing missing → no panel');
  });

  await row('Tools U1 confirm dialog: 파일 = download.file_name, source host, size, license (escaped) before anything is sent; cancel sends nothing', async () => {
    const v = make({ replies: [reply([FF, { ...OPT, license: '<b>x</b>' }, TSK])], confirm: false });
    await v.run('htLoad()');
    assert.equal(await v.run('htInstall("ffmpeg")'), false);
    assert.equal(v.calls.posts.length, 0, 'U1: cancel → no POST');
    const d = v.calls.dialogs[0];
    assert.ok(d && d.title === 'FFmpeg 설치' && d.html.includes('<dd>ffmpeg-n8.1.3-9-g29e619e767-win64-lgpl-8.1.zip</dd>') && d.html.includes('github.com') && d.html.includes('4.0 MB') && d.html.includes('LGPL-3.0-or-later (FFmpeg)') && d.html.includes('배포하지 않'), 'U1: dialog body: ' + (d && d.html));
    const x = v.run('htConfirmHtml(' + JSON.stringify({ ...OPT, download: null, size_bytes: null, pinned_version: '1.2', license: '<b>x</b>' }) + ')');
    assert.ok(x.includes('X 도구 1.2') && x.includes('알 수 없음') && x.includes('&lt;b&gt;x&lt;/b&gt;'), 'U1: fallbacks: ' + x);
    assert.equal(await v.run('htInstall("sleuthkit")'), false, 'U1: not installable → nothing');
    assert.equal(v.calls.dialogs.length, 1, 'U1: no dialog for a tool without a button');
  });

  await row('Tools confirm dialog carries the distribution notice (ko/en)', async () => {
    const v = make({ replies: [reply([FF])] });
    const para = (html) => (html.match(/<p class="h-tool-notice">([^]*?)<\/p>/g) || []);
    const ko = v.run('htConfirmHtml(' + JSON.stringify(FF) + ', "ko")');
    assert.equal(para(ko).length, 1, 'ko: one notice paragraph: ' + ko);
    assert.ok(ko.includes(`<p class="h-tool-notice">${FF.notice}</p>`) && ko.indexOf('h-tool-notice') > ko.indexOf('</dl>'), 'ko: the notice after the list: ' + ko);
    assert.ok(ko.includes('확인을 누르면 내려받습니다'), 'ko: the confirm note stays');
    const en = v.run('htConfirmHtml(' + JSON.stringify(FF) + ', "en")');
    assert.ok(en.includes(`<p class="h-tool-notice">${FF.notice_en}</p>`) && !en.includes(FF.notice), 'en: notice_en: ' + en);
    // English falls back to the Korean notice when the tool has no notice_en.
    const enKo = v.run('htConfirmHtml(' + JSON.stringify({ ...FF, notice_en: null }) + ', "en")');
    assert.ok(enKo.includes(`<p class="h-tool-notice">${FF.notice}</p>`), 'en without notice_en: ' + enKo);
    // An installable tool without either notice: no paragraph at all.
    for (const loc of ['ko', 'en']) {
      const none = v.run('htConfirmHtml(' + JSON.stringify({ ...EWF, installable: true, notice: null, notice_en: null }) + ', "' + loc + '")');
      assert.equal(para(none).length, 0, loc + ': no notice → no paragraph: ' + none);
      assert.ok(!none.includes('h-tool-notice'), loc + ': no empty paragraph');
    }
    // Escaped like every other server string.
    const evil = v.run('htConfirmHtml(' + JSON.stringify({ ...FF, notice: '<img src=x onerror=alert(1)> & "q"' }) + ', "ko")');
    assert.ok(evil.includes('&lt;img src=x onerror=alert(1)&gt; &amp; ') && !evil.includes('<img'), 'escaped: ' + evil);
    // Without a locale argument it follows the viewer language.
    v.ctx.__locale = 'en';
    assert.ok(v.run('htConfirmHtml(' + JSON.stringify(FF) + ')').includes(FF.notice_en), 'viewer language en');
  });

  await row('Tools U1 install: one POST {name}, POST {ok,install} shown at once, progress polled from /api/tools/install-status, [중단] while running, result + env refreshed', async () => {
    const v = make({
      replies: [reply([FF, OPT]), reply([FF, { ...OPT, installed: true }], J('done', 'x-opt'))],
      status: [st(J('downloading', 'x-opt', { bytes: 1048576, total: 4 * 1048576 })), st(J('verifying', 'x-opt')), st(J('done', 'x-opt'))],
    });
    await v.run('htLoad()');
    const p = v.run('htInstall("x-opt")');
    const dup = v.run('htInstall("x-opt")');
    await p; await dup;
    assert.equal(v.calls.posts.length, 1, 'U1: one POST');
    assert.equal(v.calls.dialogs.length, 1, 'U1: the second click opens no dialog');
    assert.deepEqual(plain(v.calls.posts[0]), { url: '/api/tools/install', body: { name: 'x-opt' } });
    let list = v.els.hToolsList.innerHTML;
    assert.ok(list.includes('내려받는 중 0 B') || list.includes('내려받는 중'), 'U1: the POST reply state is shown before any poll: ' + list);
    await v.fire();
    list = v.els.hToolsList.innerHTML;
    assert.equal(v.calls.gets.filter((u) => u === '/api/tools/install-status').length, 1, 'U1: the light status endpoint is polled');
    assert.ok(list.includes('내려받는 중 1.0 MB / 4.0 MB (25%)') && /data-tool-install="ffmpeg"[^>]* disabled/.test(list), 'U1: progress + other install buttons disabled: ' + list);
    assert.ok(/data-tool-cancel="x-opt"[^>]*aria-label="X 도구 설치 중단"[^>]*>중단</.test(list) && !/data-tool-install="x-opt"/.test(list), 'U1: [중단] replaces 설치 while it runs: ' + list);
    await v.fire();
    assert.ok(v.els.hToolsList.innerHTML.includes('해시 확인 중'), 'U1: verifying shown');
    await v.fire(); await tick();
    list = v.els.hToolsList.innerHTML;
    assert.ok(list.includes('X 도구 설치됨') && /h-tool-ok/.test(list), 'U1: result row: ' + list);
    assert.ok(v.calls.loadEnv === 1 && v.calls.toasts.some(([m, k]) => m === 'X 도구 설치됨' && k === 'ok'), 'U1: env refreshed + toast');
    assert.equal(v.run('HT.busy'), '', 'U1: idle again');
  });

  await row('Tools U1 a failed status poll keeps the last progress (no flicker) and is not a failure; only the server\'s `failed` is', async () => {
    const v = make({
      replies: [reply([OPT]), { ok: false, network: true, status: 0 }],
      status: [st(J('downloading', 'x-opt', { bytes: 1048576, total: 4 * 1048576 })), { ok: false, network: true, status: 0 }, { ok: false, status: 500, error: 'x' }, st(J('failed', 'x-opt', { error: 'SHA-256 불일치' }))],
    });
    await v.run('htLoad()');
    await v.run('htInstall("x-opt")');
    await v.fire();
    const before = v.els.hToolsList.innerHTML;
    assert.ok(before.includes('(25%)'), before);
    await v.fire(); await v.fire();
    assert.equal(v.els.hToolsList.innerHTML, before, 'U1: unchanged across failed polls');
    assert.equal(v.els.hToolsPanel.hidden, false, 'U1: a failed GET /api/tools does not hide the panel');
    assert.ok(!v.calls.announced.some(([m]) => /실패/.test(m)) && v.calls.toasts.length === 0, 'U1: no failure reported on a transient error');
    await v.fire(); await tick();
    const list = v.els.hToolsList.innerHTML;
    assert.ok(list.includes('설치 실패: SHA-256 불일치') && v.calls.toasts.some(([m, k]) => /설치 실패/.test(m) && k === 'err'), 'U1: real failure: ' + list);
    assert.ok(/data-tool-install="x-opt"/.test(list) && !/data-tool-install="x-opt"[^>]*disabled/.test(list), 'U1: retry possible');
  });

  await row('Tools U1 refused POST: the reply `code` picks short wording; not_installable shows manual_hint + homepage; install_running follows the other install', async () => {
    const r = make({ replies: [reply([OPT])], post: { ok: false, code: 'not_installable', error: 'x-opt 자동 설치 불가: long English reason', manual_hint: '직접 받아 tools/bin에 넣으십시오', homepage: 'https://example.org/x' } });
    await r.run('htLoad()');
    await r.run('htInstall("x-opt")');
    let list = r.els.hToolsList.innerHTML;
    assert.ok(list.includes('여기서 자동 설치할 수 없습니다') && list.includes('직접 받아 tools/bin에 넣으십시오') && list.includes('href="https://example.org/x"') && !/>[^<]*long English reason/.test(list), 'U1: not_installable: ' + list);
    assert.ok(r.calls.announced.some(([m, a]) => a && m === '여기서 자동 설치할 수 없습니다'), 'U1: refusal announced assertively');
    for (const [code, want] of [['busy', '분석·내보내기가 끝난 뒤 설치하십시오'], ['python_required', 'Python이 필요합니다'], ['unsupported_platform', '이 운영체제에서는 자동 설치를 지원하지 않습니다'], ['shutdown', '서버가 종료 중입니다'], ['weird', '설치 요청 실패: boom']]) {
      const b = make({ replies: [reply([OPT])], post: { ok: false, code, error: 'boom' } });
      await b.run('htLoad()');
      await b.run('htInstall("x-opt")');
      assert.ok(b.els.hToolsList.innerHTML.includes(want), code + ': ' + b.els.hToolsList.innerHTML);
    }
    const run = make({ replies: [reply([FF, OPT]), reply([FF, OPT], J('downloading', 'ffmpeg', { bytes: 1, total: 4 }))], post: { ok: false, code: 'install_running', error: '다른 도구 설치가 진행 중입니다.' } });
    await run.run('htLoad()');
    await run.run('htInstall("x-opt")'); await tick();
    assert.equal(run.run('HT.busy'), 'ffmpeg', 'U1: install_running → the running install is followed');
    assert.ok(run.calls.timers.length >= 1, 'U1: polling started');
    list = run.els.hToolsList.innerHTML;
    assert.ok(list.includes('다른 설치가 진행 중입니다'), 'U1: install_running wording: ' + list);
  });

  await row('Tools U1 an install already running on the server (refresh / second tab) is polled without a click and finishes with result + loadEnv', async () => {
    const v = make({ replies: [reply([FF, OPT], J('verifying', 'ffmpeg')), reply([{ ...FF, installed: true }, OPT], J('done', 'ffmpeg'))], status: [st(J('extracting', 'ffmpeg')), st(J('done', 'ffmpeg'))] });
    await v.run('htLoad()');
    assert.equal(v.run('HT.busy'), 'ffmpeg', 'U1: adopted');
    assert.equal(v.calls.timers.length, 1, 'U1: a poll is scheduled without any click');
    assert.ok(/data-tool-cancel="ffmpeg"/.test(v.els.hToolsList.innerHTML) && v.els.hToolsList.innerHTML.includes('해시 확인 중'), 'U1: progress + [중단]: ' + v.els.hToolsList.innerHTML);
    await v.fire();
    assert.ok(v.els.hToolsList.innerHTML.includes('압축 푸는 중'), v.els.hToolsList.innerHTML);
    await v.fire(); await tick();
    assert.ok(v.els.hToolsList.innerHTML.includes('FFmpeg 설치됨') && v.calls.loadEnv === 1 && v.calls.toasts.some(([, k]) => k === 'ok'), 'U1: result + loadEnv: ' + v.els.hToolsList.innerHTML);
    const idle = make({ replies: [reply([FF], J('failed', 'ffmpeg', { error: 'x' }))] });
    await idle.run('htLoad()');
    assert.equal(idle.calls.timers.length, 0, 'U1: a finished job is not polled');
  });

  await row('Tools U1 [중단]: POST /api/tools/install-cancel; not_cancellable → "이 단계는 중단할 수 없습니다" and the install goes on; accepted → "설치를 중단했습니다"', async () => {
    const v = make({ replies: [reply([OPT], J('extracting', 'x-opt'))], status: [st(J('extracting', 'x-opt'))], cancel: { ok: false, code: 'not_cancellable', error: 'x' } });
    await v.run('htLoad()');
    assert.ok(/data-tool-cancel\][^]*htCancel\(b\.dataset\.toolCancel\)/.test(read('assets/app/56-tools.js').split('(function htWire()')[1]), 'U1: the panel wires [중단] to htCancel');
    await v.run('htCancel("x-opt")');
    assert.deepEqual(plain(v.calls.posts[0]), { url: '/api/tools/install-cancel', body: { id: 7 } }, 'the cancel names the followed job');
    let list = v.els.hToolsList.innerHTML;
    assert.ok(list.includes('이 단계는 중단할 수 없습니다') && /data-tool-cancel="x-opt"/.test(list) && !/data-tool-cancel="x-opt"[^>]*disabled/.test(list), 'U1: not cancellable: ' + list);
    assert.ok(v.calls.announced.some(([m]) => m === '이 단계는 중단할 수 없습니다'), 'announced');
    assert.equal(v.run('HT.busy'), 'x-opt', 'still following');
    const c = make({ replies: [reply([OPT], J('downloading', 'x-opt', { bytes: 1, total: 4 })), reply([OPT], J('failed', 'x-opt', { error: '사용자가 설치를 중단했습니다.' }))], status: [st(J('failed', 'x-opt', { error: '사용자가 설치를 중단했습니다.' }))], cancel: { ok: true, install: J('downloading', 'x-opt', { bytes: 1, total: 4 }) } });
    await c.run('htLoad()');
    await c.run('htCancel("x-opt")');
    list = c.els.hToolsList.innerHTML;
    assert.ok(/data-tool-cancel="x-opt"[^>]*disabled/.test(list) && list.includes('중단하는 중…'), 'U1: stopping: ' + list);
    await c.fire(); await tick();
    list = c.els.hToolsList.innerHTML;
    assert.ok(list.includes('X 도구 설치를 중단했습니다') && c.calls.toasts.some(([m, k]) => /중단했습니다/.test(m) && k === 'warn'), 'U1: stopped: ' + list);
  });

  await row('Tools U2 announcements go through the global announce() (no live region inside the hidden panel); the prompt takes focus when it appears', async () => {
    assert.ok(!/id="hToolsLive"/.test(html), 'U2: no live region inside #hToolsPanel');
    assert.ok(/id="srStatus" role="status" aria-live="polite"/.test(html), 'U2: the global region exists');
    const v = make({ replies: [reply([OPT])], status: [st(J('verifying', 'x-opt'))] });
    await v.run('htLoad()');
    v.els.hToolsPanel.hidden = true; // e.g. a project is open
    await v.run('htInstall("x-opt")');
    await v.fire();
    assert.ok(v.calls.announced.some(([m, a]) => !a && /해시 확인 중 — X 도구/.test(m)), 'U2: state changes announced politely: ' + JSON.stringify(v.calls.announced));
    const n = v.calls.announced.length;
    v.run('HT.timer = 0'); await v.fire();
    assert.equal(v.calls.announced.length, n, 'U2: the same state is not announced again');
    const p = make({ replies: [reply([FF, DFI])] });
    await p.run('htShowToolPrompt($("startToolPrompt"), "deepfake-lens", () => 0, "retryStart")');
    assert.deepEqual(p.calls.focused, ['startToolPrompt'], 'U2: focus moves to the prompt text ([data-ht-focus])');
  });

  await row('Tools deepfake option: re-enabled only when installable === true, whichever of /api/tools and /api/env answers first; otherwise disabled with the manual_hint tooltip (start + finalize)', async () => {
    const fin = 'var FIN = { built: true, env: { deepfake: false }, advBusy: false }; var __btn = { dataset: {}, disabled: true, title: "" }; function finApplyAudioEnv() {} function finMount() { return { querySelector: () => __btn }; }\n' + finApplyEnvSrc;
    for (const order of ['tools-first', 'env-first']) {
      for (const df of [DF, DFI]) {
        const v = make({ replies: [reply([FF, df])], extra: fin });
        const box = v.ctx.$('withDeepfake');
        const lab = { attrs: {}, setAttribute(k, s) { this.attrs[k] = s; } };
        box.closest = () => lab;
        const env = () => { v.run('setEnvOff($("withDeepfake"), true); __btn.dataset.envOff = "1"; __btn.disabled = true; if (typeof htSyncDeepfake === "function") htSyncDeepfake();'); };
        if (order === 'env-first') env();
        await v.run('htLoad()');
        if (order === 'tools-first') env();
        const can = df.installable === true;
        assert.equal(v.run('htNeedsTool("deepfake-lens")'), can, `${order}: htNeedsTool follows installable`);
        assert.equal(box.disabled, !can, `${order} installable=${can}: start option disabled=${!can}`);
        assert.equal(v.run('__btn.disabled'), !can, `${order} installable=${can}: finalize button`);
        if (!can) {
          assert.ok(/PyPI에 공개되지 않아/.test(lab.attrs.title || '') && /PyPI에 공개되지 않아/.test(v.run('__btn.title')), `${order}: manual_hint tooltip: ` + lab.attrs.title + ' / ' + v.run('__btn.title'));
        }
      }
    }
    assert.ok(/setEnvOff\(\$\("withDeepfake"\), !dfOk\)[^]*htSyncDeepfake\(\)/.test(formSrc), 'loadEnv re-applies the /api/tools verdict after setting the env state');
  });

  await row('Tools U1 finalize checklist: one warning line per missing required tool only', async () => {
    const v = make({ replies: [reply([FF, EWF, DF])] });
    const c = plain(await v.run('htFinalizeChecks()'));
    assert.deepEqual(c.map((x) => [x.level, x.text]), [['warn', 'FFmpeg 없음 — 영상 검증·프록시를 만들 수 없음']]);
    const none = make({ replies: [{ ok: false, status: 404 }] });
    assert.equal((await none.run('htFinalizeChecks()')).length, 0, 'U1: older server → no line');
    assert.ok(/htFinalizeChecks\(\)/.test(finSrc), 'U1: gatherFinalizeChecks uses it');
  });

  await row('Tools deepfake-lens prompt: tool_missing → "합성 영상 검사에 필요한 도구가 없습니다" → 설치 (pypi.org) → progress / [중단] → 다시 검사', async () => {
    const v = make({ replies: [reply([FF, DFI]), reply([FF, DFI]), reply([FF, { ...DFI, installed: true }], J('done', 'deepfake-lens'))], status: [st(J('downloading', 'deepfake-lens', { bytes: 1024, total: 2048 })), st(J('done', 'deepfake-lens'))] });
    await v.run('htLoad()');
    assert.equal(v.run('htNeedsTool("deepfake-lens")'), true);
    const box = v.ctx.$('advToolPrompt');
    let retried = 0;
    v.ctx.__retry = () => { retried += 1; };
    await v.run('htShowToolPrompt($("advToolPrompt"), "deepfake-lens", () => __retry())');
    assert.ok(!box.hidden && box.innerHTML.includes('합성 영상 검사에 필요한 도구가 없습니다') && /data-ht-prompt="install"/.test(box.innerHTML), 'prompt: ' + box.innerHTML);
    const click = (kind) => box.listeners[0]({ target: { closest: () => ({ disabled: false, dataset: { htPrompt: kind } }) } });
    click('install'); await tick();
    const d = v.calls.dialogs[0];
    assert.ok(d && d.html.includes('pypi.org') && d.html.includes('deepfake-lens (합성 영상 스크리닝) 0.4.2') && d.html.includes('2.0 KB') && d.html.includes('MIT'), 'dialog: ' + (d && d.html));
    assert.ok(!/pip install/.test(d.html + box.innerHTML), 'the pip command is never shown');
    assert.deepEqual(plain(v.calls.posts[0].body), { name: 'deepfake-lens' });
    await v.fire();
    assert.ok(box.innerHTML.includes('내려받는 중 1.0 KB / 2.0 KB (50%)') && /data-ht-prompt="stop"[^>]*>중단</.test(box.innerHTML), 'progress + [중단]: ' + box.innerHTML);
    await v.fire(); await tick();
    assert.ok(box.innerHTML.includes('deepfake-lens (합성 영상 스크리닝) 설치됨') && /data-ht-prompt="retry"[^>]*>다시 검사</.test(box.innerHTML), 'retry offered: ' + box.innerHTML);
    click('retry');
    assert.equal(retried, 1, 'retry runs the screening again');
    assert.ok(box.hidden, 'the prompt goes away on retry');
  });

  await row('Tools deepfake-lens prompt: not installable (real manifest) → the Korean manual_hint, reason as title, no button; older server → manual hint', async () => {
    const v = make({ replies: [reply([FF, DF])] });
    await v.run('htShowToolPrompt($("startToolPrompt"), "deepfake-lens", () => 0, "retryStart")');
    const b = v.ctx.$('startToolPrompt').innerHTML;
    assert.ok(b.includes('합성 영상 검사에 필요한 도구가 없습니다') && b.includes('PyPI에 공개되지 않아 자동 설치할 수 없습니다') && /title="deepfake-lens is not published/.test(b) && !/>deepfake-lens is not published/.test(b) && !/<button/.test(b), b);
    const o = make({ replies: [{ ok: false, status: 404 }] });
    await o.run('htShowToolPrompt($("startToolPrompt"), "deepfake-lens", () => 0)');
    assert.ok(!/<button/.test(o.ctx.$('startToolPrompt').innerHTML) && o.ctx.$('startToolPrompt').innerHTML.includes('tools/bin 또는 PATH'), 'older server: no button');
    assert.ok(/res\.tool_missing/.test(finSrc) && /res\.tool_missing/.test(startSrc), 'both run paths read tool_missing');
    assert.ok(/htNeedsTool\("deepfake-lens"\)/.test(finSrc) && /htNeedsTool\("deepfake-lens"\)/.test(startSrc), 'a known-missing tool prompts before running');
  });

  await row('Tools U2 ko/en strings match and follow the viewer language; labelled list; legacy shell: one hint line', async () => {
    const v = make({ replies: [reply([FF])] });
    assert.deepEqual(Object.keys(v.run('HT_STR.en')).sort(), Object.keys(v.run('HT_STR.ko')).sort(), 'U2: same keys');
    v.ctx.__locale = 'en';
    await v.run('htLoad()');
    assert.ok(v.els.hToolsList.innerHTML.includes('FFmpeg missing — required') && v.els.hToolsPanelTitle.textContent === 'Tools', 'U2: English');
    // The English dialog carries notice_en (option C), not the Korean notice.
    const ve = make({ replies: [reply([FF])], confirm: false });
    ve.ctx.__locale = 'en';
    await ve.run('htLoad()');
    assert.equal(await ve.run('htInstall("ffmpeg")'), false);
    const de = ve.calls.dialogs[0];
    assert.ok(de && de.title === 'Install FFmpeg' && de.html.includes('FrameTrace does not distribute it') && !de.html.includes('배포하지 않'), 'U2: English dialog notice: ' + (de && de.html));
    assert.ok(/id="hToolsPanel"[^>]*aria-labelledby="hToolsPanelTitle"[^>]*hidden/.test(html), 'U2: markup');
    assert.ok(/id="startToolPrompt"/.test(html) && /id="advToolPrompt"/.test(finSrc), 'U2: prompt mounts');
    assert.ok(!/class="sblock[^"]*" id="hToolsPanel"/.test(html), 'the empty state keeps exactly two blocks');
    assert.equal((legacyHtml.match(/frametrace tools list/g) || []).length, 1, 'U2: legacy hint');
  });

  // ---- round-2 review fixes ----
  const nowS = () => Math.floor(Date.now() / 1000);
  const LOST = '설치 상태를 알 수 없습니다 — 다시 시도하십시오';

  await row('Tools job id replaced mid-poll (another tab / restarted server): the new job is adopted and followed, never "상태를 알 수 없습니다" + a stuck panel', async () => {
    for (const [tool, label] of [['ffmpeg', 'FFmpeg'], ['x-opt', 'X 도구']]) {
      const v = make({
        replies: [reply([FF, OPT])],
        status: [st(J('downloading', 'x-opt', { bytes: 1, total: 4 })), st({ ...J('downloading', tool, { bytes: 2, total: 4 }), id: 8 }), st({ ...J('verifying', tool), id: 8 })],
      });
      await v.run('htLoad()');
      await v.run('htInstall("x-opt")');
      await v.fire();
      await v.fire(); await tick();
      assert.ok(!v.calls.toasts.some(([m]) => m === LOST), tool + ': no "lost" toast: ' + JSON.stringify(v.calls.toasts));
      assert.equal(v.run('HT.busy'), tool, tool + ': the new job is followed');
      assert.equal(v.run('HT.job'), 8, tool + ': its id');
      assert.equal(v.calls.timers.length, 1, tool + ': polling continues');
      assert.ok(new RegExp('data-tool-cancel="' + tool + '"').test(v.els.hToolsList.innerHTML), tool + ': [중단] for the new job: ' + v.els.hToolsList.innerHTML);
      await v.fire();
      assert.ok(v.els.hToolsList.innerHTML.includes('해시 확인 중'), tool + ': its progress: ' + v.els.hToolsList.innerHTML);
      assert.ok(v.calls.announced.some(([m]) => m.includes(label)), tool + ': announced');
    }
    // Our job ended while another tab already started the next one: the
    // result is shown AND the new job is adopted (busy is reset before
    // the reload, so htAdopt runs).
    const v = make({
      replies: [reply([FF, OPT]), reply([FF, { ...OPT, installed: true }], { ...J('downloading', 'ffmpeg', { bytes: 1, total: 4 }), id: 9 })],
      status: [st(J('done', 'x-opt'))],
    });
    await v.run('htLoad()');
    await v.run('htInstall("x-opt")');
    await v.fire(); await tick();
    assert.ok(v.calls.toasts.some(([m, k]) => m === 'X 도구 설치됨' && k === 'ok'), 'result toast');
    assert.equal(v.run('HT.busy'), 'ffmpeg', 'the other tab\'s install is adopted after ours ended');
    assert.ok(v.calls.timers.length >= 1 && /data-tool-cancel="ffmpeg"/.test(v.els.hToolsList.innerHTML), 'and polled: ' + v.els.hToolsList.innerHTML);
  });

  await row('Tools second tab cancels: a `failed` job that ended on a cancel (cancelled:true, or the INSTALL_CANCELLED error from an older server, or before our cancel reply) is "설치를 중단했습니다" (warn), never "설치 실패: install cancelled"', async () => {
    for (const extra of [{ error: 'install cancelled', cancelled: true }, { error: 'install cancelled' }]) {
      const fj = J('failed', 'x-opt', { ...extra, finished_unix: nowS() });
      const v = make({ replies: [reply([OPT], J('downloading', 'x-opt', { bytes: 1, total: 4 })), reply([OPT], fj)], status: [st(fj)] });
      await v.run('htLoad()');
      await v.fire(); await tick();
      const list = v.els.hToolsList.innerHTML;
      assert.ok(list.includes('X 도구 설치를 중단했습니다') && !/설치 실패/.test(list) && !/install cancelled/.test(list), JSON.stringify(extra) + ': ' + list);
      assert.ok(v.calls.toasts.some(([m, k]) => m === 'X 도구 설치를 중단했습니다' && k === 'warn') && !v.calls.toasts.some(([, k]) => k === 'err'), 'warn toast: ' + JSON.stringify(v.calls.toasts));
    }
    // A refresh right after: the recent cancelled job reads as stopped too.
    const r = make({ replies: [reply([OPT], J('failed', 'x-opt', { error: 'install cancelled', cancelled: true, finished_unix: nowS() }))] });
    await r.run('htLoad()');
    assert.ok(r.els.hToolsList.innerHTML.includes('X 도구 설치를 중단했습니다') && !/설치 실패/.test(r.els.hToolsList.innerHTML), 'refresh: ' + r.els.hToolsList.innerHTML);
    // The poll's failed arrives while our cancel is still in flight.
    const pj = J('failed', 'x-opt', { error: '설치가 취소되었습니다', finished_unix: nowS() });
    const p = make({ replies: [reply([OPT], J('downloading', 'x-opt')), reply([OPT], pj)], status: [st(pj)] });
    await p.run('htLoad()');
    p.run('HT.stopping = "x-opt"');
    await p.fire(); await tick();
    assert.ok(p.calls.toasts.some(([m, k]) => /중단했습니다/.test(m) && k === 'warn'), 'in-flight cancel: ' + JSON.stringify(p.calls.toasts));
    // A new server says cancelled:false: a real failure stays a failure.
    const fj2 = J('failed', 'x-opt', { error: 'SHA-256 불일치', cancelled: false, finished_unix: nowS() });
    const f = make({ replies: [reply([OPT], J('downloading', 'x-opt')), reply([OPT], fj2)], status: [st(fj2)] });
    await f.run('htLoad()');
    f.run('HT.stopAsked = true');
    await f.fire(); await tick();
    assert.ok(f.calls.toasts.some(([m, k]) => m === '설치 실패: SHA-256 불일치' && k === 'err'), 'cancelled:false: ' + JSON.stringify(f.calls.toasts));
  });

  await row('Tools stale failed job not re-shown: an old `failed` job (finished minutes ago, or no finished_unix) shows no "설치 실패" on refresh / in another tab; a recent one does', async () => {
    for (const fin of [nowS() - 3600, null]) {
      const v = make({ replies: [reply([OPT, DFI], J('failed', 'x-opt', { error: 'SHA-256 불일치', finished_unix: fin }))] });
      await v.run('htLoad()');
      assert.ok(!/설치 실패/.test(v.els.hToolsList.innerHTML) && /data-tool-install="x-opt"/.test(v.els.hToolsList.innerHTML), fin + ': panel: ' + v.els.hToolsList.innerHTML);
      const d = make({ replies: [reply([FF, DFI], J('failed', 'deepfake-lens', { error: 'pip 실패', finished_unix: fin }))] });
      await d.run('htShowToolPrompt($("advToolPrompt"), "deepfake-lens", () => 0)');
      assert.ok(!/설치 실패/.test(d.ctx.$('advToolPrompt').innerHTML), fin + ': prompt: ' + d.ctx.$('advToolPrompt').innerHTML);
    }
    const recent = make({ replies: [reply([OPT], J('failed', 'x-opt', { error: 'SHA-256 불일치', finished_unix: nowS() - 30 }))] });
    await recent.run('htLoad()');
    assert.ok(recent.els.hToolsList.innerHTML.includes('설치 실패: SHA-256 불일치'), 'recent: ' + recent.els.hToolsList.innerHTML);
  });

  await row('Tools focus stays on the same row: a re-render while focus is on a row\'s text ([data-ht-focus]) keeps it on that row, not the first one', async () => {
    const v = make({ replies: [reply([FF, OPT])] });
    await v.run('htLoad()');
    const list = v.els.hToolsList;
    const li = (tool) => ({ dataset: { tool }, querySelector: (s) => (s === '[data-ht-focus]' ? { focus: () => v.calls.focused.push('row:' + tool) } : null) });
    list.contains = () => true;
    list.querySelectorAll = (s) => (s === 'li' ? [li('ffmpeg'), li('x-opt')] : []);
    v.ctx.document.activeElement = { dataset: {}, closest: (s) => (s === 'li[data-tool]' ? { dataset: { tool: 'x-opt' } } : null) };
    v.run('HT.written.clear(); htRenderPanel()');
    assert.deepEqual(v.calls.focused, ['row:x-opt'], 'focus restored to the x-opt row');
  });

  await row('Tools [중단] disabled (title "이 단계는 중단할 수 없습니다") while stage is install / venv / pip; enabled in download / verify / extract', async () => {
    const v = make({ replies: [reply([OPT])] });
    for (const [stage, locked] of [['download', false], ['verify', false], ['extract', false], ['install', true], ['venv', true], ['pip', true]]) {
      const state = stage === 'download' ? 'downloading' : stage === 'verify' ? 'verifying' : 'extracting';
      const inst = JSON.stringify(J(state, 'x-opt', { stage }));
      const html = v.run(`htRowHtml(htRowModel(${JSON.stringify(OPT)}, htParseInstall(${inst}), { busy: "x-opt" }))`);
      const btn = html.match(/<button[^>]*data-tool-cancel[^>]*>/)[0];
      assert.equal(/ disabled/.test(btn) && /title="이 단계는 중단할 수 없습니다"/.test(btn), locked, stage + ': ' + btn);
      const pr = v.run(`htPromptHtml(htPromptModel("x-opt", { supported: true, tools: [${JSON.stringify(OPT)}], install: htParseInstall(${inst}) }, { busy: "x-opt" }))`);
      const pbtn = pr.match(/<button[^>]*data-ht-prompt="stop"[^>]*>/)[0];
      assert.equal(/ disabled/.test(pbtn) && /title="이 단계는 중단할 수 없습니다"/.test(pbtn), locked, stage + ' prompt: ' + pbtn);
    }
  });

  await row('Tools polling gives up: a 401, or 20 failed polls in a row, stops following ("설치 상태를 알 수 없습니다") instead of polling forever; a success resets the count', async () => {
    const auth = make({ replies: [reply([OPT], J('downloading', 'x-opt')), { ok: false, status: 401, auth: true, error: 'x' }], status: [{ ok: false, status: 401, auth: true, error: 'x' }] });
    await auth.run('htLoad()');
    await auth.fire(); await tick();
    assert.equal(auth.run('HT.busy'), '', '401: idle');
    assert.equal(auth.calls.timers.length, 0, '401: no more polls');
    assert.ok(auth.calls.toasts.some(([m]) => m === LOST), '401: lost toast');
    const down = { ok: false, network: true, status: 0 };
    const v = make({ replies: [reply([OPT], J('downloading', 'x-opt')), down], status: [...Array(19).fill(down), st(J('downloading', 'x-opt', { bytes: 1, total: 4 })), down] });
    await v.run('htLoad()');
    for (let i = 0; i < 20; i++) await v.fire();
    assert.equal(v.run('HT.busy'), 'x-opt', '19 failures + a success: still following');
    for (let i = 0; i < 19; i++) await v.fire();
    assert.equal(v.run('HT.busy'), 'x-opt', '19 more failures: still following');
    await v.fire(); await tick();
    assert.equal(v.run('HT.busy'), '', '20 in a row: stopped');
    assert.equal(v.calls.timers.length, 0, 'no more polls');
    assert.ok(v.calls.toasts.some(([m]) => m === LOST), 'lost toast');
  });

  await row('Tools announcements: "설치 요청 중…" is not announced (the POST reply\'s state follows at once); the reply state is', async () => {
    const v = make({ replies: [reply([OPT])] });
    await v.run('htLoad()');
    await v.run('htInstall("x-opt")');
    assert.ok(!v.calls.announced.some(([m]) => /설치 요청 중/.test(m)), JSON.stringify(v.calls.announced));
    assert.ok(v.calls.announced.some(([m]) => m === '내려받는 중 — X 도구'), JSON.stringify(v.calls.announced));
  });

  await row('Tools deepfake tooltips follow the viewer language (HT_STR keys, no Korean literals in 30-form.js / 41-finalize.js / htSyncDeepfake)', async () => {
    const tsrc = read('assets/app/56-tools.js');
    for (const [f, src] of [['30-form.js', formSrc], ['41-finalize.js', finSrc], ['56-tools.js htSyncDeepfake', tsrc.match(/function htSyncDeepfake\(\) \{[^]*?^}/m)[0]]]) {
      assert.ok(!/"deepfake-lens 미설치|"케이스의 모든 파일을/.test(src), f + ': a hard-coded Korean deepfake tooltip remains');
    }
    const fin = 'var FIN = { built: true, env: { deepfake: false }, advBusy: false }; var __btn = { dataset: {}, disabled: true, title: "" }; function finApplyAudioEnv() {} function finMount() { return { querySelector: () => __btn }; }\n' + finApplyEnvSrc;
    for (const [loc, df, want] of [['en', DFI, 'deepfake-lens is not installed — click to install it'], ['ko', DFI, 'deepfake-lens 미설치 — 누르면 설치를 안내합니다'], ['en', { ...DF, manual_hint: '' }, 'deepfake-lens is not installed — not on PyPI']]) {
      const v = make({ replies: [reply([FF, df])], extra: fin });
      v.ctx.__locale = loc;
      const box = v.ctx.$('withDeepfake');
      const lab = { attrs: {}, setAttribute(k, s) { this.attrs[k] = s; } };
      box.closest = () => lab;
      v.run('setEnvOff($("withDeepfake"), true); __btn.dataset.envOff = "1"; __btn.disabled = true;');
      await v.run('htLoad()');
      assert.ok(v.run('__btn.title').startsWith(want), loc + ': finalize title: ' + v.run('__btn.title'));
      if (df === DFI) assert.equal(lab.attrs.title, loc === 'en' ? 'deepfake-lens is not installed — starting offers to install it' : 'deepfake-lens 미설치 — 시작하면 설치를 안내합니다', loc + ': start option title');
    }
  });

  await row('Tools mock (tools-mock.mjs) cancels like the real server: refused only in stage install|venv|pip (not while extracting / extract); accepted → job unchanged, `failed` + cancelled:true on the NEXT poll', async () => {
    const fakePage = () => {
      const routes = [];
      return {
        routes,
        route: async (pat, fn) => { routes.push([pat, fn]); },
        call: async (path, method = 'GET', body = null) => {
          const hit = routes.find(([pat]) => pat === '**' + path);
          let out = null;
          await hit[1]({ request: () => ({ method: () => method, postData: () => (body ? JSON.stringify(body) : '') }), fulfill: (r) => { out = JSON.parse(r.body); } });
          return out;
        },
      };
    };
    const pg = fakePage();
    const m = await routeTools(pg, { steps: 2 });
    assert.equal((await pg.call('/api/tools/install', 'POST', { name: 'ffmpeg' })).ok, true);
    let s;
    do { s = (await pg.call('/api/tools/install-status')).install; } while (s.state !== 'extracting');
    assert.equal(s.stage, 'extract', 'extracting starts in stage extract');
    const c1 = await pg.call('/api/tools/install-cancel', 'POST', {});
    assert.ok(c1.ok === true && c1.install.state === 'extracting', 'extract is cancellable; reply = job unchanged: ' + JSON.stringify(c1));
    s = (await pg.call('/api/tools/install-status')).install;
    assert.ok(s.state === 'failed' && s.error === 'install cancelled' && s.cancelled === true, 'next poll: failed/cancelled: ' + JSON.stringify(s));
    assert.equal((await pg.call('/api/tools/install', 'POST', { name: 'ffmpeg' })).ok, true);
    do { s = (await pg.call('/api/tools/install-status')).install; } while (s.stage !== 'install' && s.state !== 'done');
    assert.equal(s.stage, 'install', 'files are moved in stage install');
    const c2 = await pg.call('/api/tools/install-cancel', 'POST', {});
    assert.equal(c2.code, 'not_cancellable', 'stage install refuses: ' + JSON.stringify(c2));
    assert.equal((await pg.call('/api/tools/install-status')).install.state, 'done', 'and the install finishes');
    assert.equal(m.cancels, 2);
  });

  // ---- round-3 review fixes ----
  await row('Tools confirm dialog open while another install is adopted: OK starts nothing and says "다른 설치가 진행 중입니다" (not a silent no-op)', async () => {
    const v = make({ replies: [reply([FF, OPT]), reply([FF, OPT], { ...J('downloading', 'ffmpeg', { bytes: 1, total: 4 }), id: 9 })] });
    await v.run('htLoad()');
    // While the dialog is open, a refresh of the list (another tab
    // started ffmpeg) adopts that install.
    v.ctx.__onDialog = async () => { await v.run('htLoad()'); };
    const started = await v.run('htInstall("x-opt")');
    assert.equal(started, false);
    assert.equal(v.calls.posts.length, 0, 'no POST for the second install');
    assert.equal(v.run('HT.busy'), 'ffmpeg', 'the other install is followed');
    const busy = v.calls.announced.filter(([m]) => m === '다른 설치가 진행 중입니다');
    assert.ok(busy.length >= 2, 'announced when adopted under the dialog and again on OK: ' + JSON.stringify(v.calls.announced));
  });

  await row('Tools "recent" is judged by the server clock (J.age_s / now_unix), never this browser\'s clock', async () => {
    const b = nowS();
    // The server's clock is an hour behind this browser: finished 30 s ago
    // by the server → recent (the browser clock would say an hour).
    const behind = make({ replies: [{ ...reply([OPT], J('failed', 'x-opt', { error: 'SHA-256 불일치', finished_unix: b - 3600 })), now_unix: b - 3570 }] });
    await behind.run('htLoad()');
    assert.ok(behind.els.hToolsList.innerHTML.includes('설치 실패: SHA-256 불일치'), 'server says 30 s ago: ' + behind.els.hToolsList.innerHTML);
    // The server's clock is an hour ahead: finished "now" by the browser,
    // an hour ago by the server → stale, not shown.
    const ahead = make({ replies: [{ ...reply([OPT], J('failed', 'x-opt', { error: 'SHA-256 불일치', finished_unix: b })), now_unix: b + 3600 }] });
    await ahead.run('htLoad()');
    assert.ok(!/설치 실패/.test(ahead.els.hToolsList.innerHTML), 'server says an hour ago: ' + ahead.els.hToolsList.innerHTML);
    // J.age_s wins (also from install-status).
    const aged = make({ replies: [reply([OPT], J('failed', 'x-opt', { error: 'x', finished_unix: b, age_s: 4000 }))] });
    await aged.run('htLoad()');
    assert.ok(!/설치 실패/.test(aged.els.hToolsList.innerHTML), 'age_s 4000: ' + aged.els.hToolsList.innerHTML);
    assert.equal(aged.run('htParseInstall({ state: "failed", finished_unix: 100 }, 160).age_s'), 60);
    assert.equal(aged.run('htParseInstall({ state: "failed", finished_unix: 100 }).age_s'), null, 'an older server: no age (browser clock fallback)');
  });

  await row('Tools a lost POST reply (network / status 0) asks the server and follows the install it started, instead of "설치 요청 실패"', async () => {
    const lost = { ok: false, network: true, status: 0, error: '서버에 연결할 수 없습니다: Failed to fetch' };
    const v = make({ replies: [reply([FF, OPT]), reply([FF, OPT], J('downloading', 'x-opt', { bytes: 1, total: 4 }))], post: lost });
    await v.run('htLoad()');
    assert.equal(await v.run('htInstall("x-opt")'), true, 'followed');
    assert.equal(v.run('HT.busy'), 'x-opt');
    assert.equal(v.run('HT.job'), 7);
    assert.ok(v.calls.timers.length >= 1, 'polling');
    const list = v.els.hToolsList.innerHTML;
    assert.ok(!/설치 요청 실패/.test(list) && !v.calls.announced.some(([m]) => /설치 요청 실패/.test(m)), 'no request-failed: ' + list);
    assert.ok(/data-tool-cancel="x-opt"/.test(list), '[중단] for the adopted job: ' + list);
    // Nothing running on the server: "상태를 알 수 없습니다", not a refusal.
    const n = make({ replies: [reply([OPT]), reply([OPT])], post: lost });
    await n.run('htLoad()');
    assert.equal(await n.run('htInstall("x-opt")'), false);
    assert.ok(n.els.hToolsList.innerHTML.includes(LOST) && !/설치 요청 실패/.test(n.els.hToolsList.innerHTML), 'lost: ' + n.els.hToolsList.innerHTML);
    // It already finished: its result is shown.
    const d = make({ replies: [reply([OPT]), { ...reply([{ ...OPT, installed: true }], J('done', 'x-opt', { finished_unix: nowS() })), now_unix: nowS() }], post: lost });
    await d.run('htLoad()');
    await d.run('htInstall("x-opt")'); await tick();
    assert.ok(d.calls.toasts.some(([m, k]) => m === 'X 도구 설치됨' && k === 'ok'), 'done: ' + JSON.stringify(d.calls.toasts));
    // A real HTTP error is still a refusal.
    const e = make({ replies: [reply([OPT])], post: { ok: false, status: 500, error: 'boom' } });
    await e.run('htLoad()');
    await e.run('htInstall("x-opt")');
    assert.ok(e.els.hToolsList.innerHTML.includes('설치 요청 실패: boom'), e.els.hToolsList.innerHTML);
  });

  await row('Tools [중단] names the job ({id}); a `stale_job` reply (the job already ended, another one runs) is not an error', async () => {
    const v = make({ replies: [reply([OPT], { ...J('downloading', 'x-opt', { bytes: 1, total: 4 }), id: 12 })], cancel: { ok: false, code: 'stale_job', error: 'x' } });
    await v.run('htLoad()');
    await v.run('htCancel("x-opt")');
    assert.deepEqual(plain(v.calls.posts[0]), { url: '/api/tools/install-cancel', body: { id: 12 } });
    const list = v.els.hToolsList.innerHTML;
    assert.ok(!/중단 요청 실패/.test(list) && !v.calls.announced.some(([m]) => /중단 요청 실패/.test(m)), 'no failure note: ' + list);
    assert.equal(v.run('HT.stopping'), '', 'not stuck in "중단하는 중…"');
  });

  await row('Tools shadow warning: a tool whose tools/bin copy shadows another on PATH shows "PATH의 다른 사본 무시됨" (paths in the title), installed or not', async () => {
    const shadowPath = 'C:\\Users\\me\\AppData\\Local\\Microsoft\\WindowsApps\\ffmpeg.exe';
    const v = make({ replies: [reply([{ ...FF, installed: true, status: 'installed', shadowed_by: { ffmpeg: shadowPath, ffprobe: null } }, { ...OPT, shadowed_by: { 'x-opt': null } }])] });
    await v.run('htLoad()');
    const list = v.els.hToolsList.innerHTML;
    const ff = (list.match(/<li[^>]*data-tool="ffmpeg"[^]*?<\/li>/) || [''])[0];
    assert.ok(ff.includes('PATH의 다른 사본 무시됨') && ff.includes('WindowsApps\\ffmpeg.exe') && /title="FrameTrace는 tools\/bin의 것을 씁니다/.test(ff), 'installed + shadow: ' + list);
    assert.ok(!/<button/.test(ff), 'no button on the shadow line');
    const opt = (list.match(/<li[^>]*data-tool="x-opt"[^]*?<\/li>/) || [''])[0];
    assert.ok(!/PATH의 다른 사본/.test(opt), 'no shadow, no warning: ' + opt);
    const missing = v.run('htRowHtml(htRowModel(' + JSON.stringify({ ...FF, paths: { ffmpeg: 'C:\\FT\\tools\\bin\\ffmpeg.exe', ffprobe: null }, shadowed_by: { ffmpeg: shadowPath, ffprobe: null } }) + ', null, {}))');
    assert.ok(missing.includes('PATH의 다른 사본 무시됨') && /data-tool-install="ffmpeg"/.test(missing), 'partial install + shadow: ' + missing);
    const plainOk = make({ replies: [reply([{ ...FF, installed: true, shadowed_by: { ffmpeg: null, ffprobe: null } }])] });
    await plainOk.run('htLoad()');
    assert.equal(plainOk.els.hToolsPanel.hidden, true, 'installed without a shadow: no panel');
    v.ctx.__locale = 'en';
    v.run('htRenderPanel()');
    assert.ok(v.els.hToolsList.innerHTML.includes('Other copy on PATH ignored'), 'en: ' + v.els.hToolsList.innerHTML);
  });

  await row('Tools mock + fixture carry the round-3 fields: shadowed_by per tool, now_unix, J.age_s / staging_left, encoders.trial_failed, stale_job', async () => {
    for (const t of TOOLS_FIXTURE()) assert.deepEqual(Object.keys(t.shadowed_by).sort(), Object.keys(t.paths).sort(), t.name + ': shadowed_by keys = paths keys');
    const routes = [];
    const pg = { route: async (pat, fn) => { routes.push([pat, fn]); } };
    const call = async (path, method = 'GET', body = null) => {
      const hit = routes.find(([pat]) => pat === '**' + path);
      let out = null;
      await hit[1]({ request: () => ({ method: () => method, postData: () => (body ? JSON.stringify(body) : '') }), fulfill: (r) => { out = JSON.parse(r.body); } });
      return out;
    };
    await routeTools(pg, { steps: 1 });
    assert.ok(Number.isFinite((await call('/api/tools')).now_unix), 'GET /api/tools now_unix');
    const started = await call('/api/tools/install', 'POST', { name: 'ffmpeg' });
    assert.ok(started.install.staging_left === null && started.install.age_s === null, 'J: ' + JSON.stringify(started.install));
    const st = await call('/api/tools/install-status');
    assert.ok(Number.isFinite(st.now_unix), 'install-status now_unix');
    const stale = await call('/api/tools/install-cancel', 'POST', { id: started.install.id + 1 });
    assert.equal(stale.code, 'stale_job', JSON.stringify(stale));
    let s2;
    do { s2 = (await call('/api/tools/install-status')).install; } while (s2.state !== 'done' && s2.state !== 'failed');
    assert.ok(s2.state === 'done' && Number.isFinite(s2.age_s), 'done + age_s: ' + JSON.stringify(s2));
    const ff = (await call('/api/tools')).tools.find((t) => t.name === 'ffmpeg');
    assert.deepEqual(ff.encoders.trial_failed, [], 'encoders.trial_failed: ' + JSON.stringify(ff.encoders));
  });

  await row('Tools round 4: a 5xx POST reply with a non-JSON body is a refusal ("설치 요청 실패"), never "상태를 알 수 없습니다"', async () => {
    for (const post of [{ ok: false, status: 502, malformed: true, error: '서버 오류 (HTTP 502)' }, { ok: false, status: 500, error: '서버 오류 (HTTP 500)' }, { ok: false, status: 503, network: true, error: '서버 오류 (HTTP 503)' }]) {
      const v = make({ replies: [reply([OPT])], post });
      await v.run('htLoad()');
      const gets = v.calls.gets.length;
      assert.equal(await v.run('htInstall("x-opt")'), false);
      const list = v.els.hToolsList.innerHTML;
      assert.ok(list.includes('설치 요청 실패: ' + post.error) && !list.includes(LOST), JSON.stringify(post) + ': ' + list);
      assert.equal(v.calls.gets.length, gets, JSON.stringify(post) + ': a refusal does not ask the server again');
    }
    assert.equal(make().run('htPostLost({ ok: false, network: true, status: 0 })'), true);
    assert.equal(make().run('htPostLost({ ok: false, malformed: true })'), true, 'an unreadable 2xx reply is lost');
    assert.equal(make().run('htPostLost(null)'), true);
  });

  await row('Tools round 4: a lost POST whose htLoad() adopts ANOTHER tool\'s install is not success for this tool — "상태를 알 수 없습니다" for it, the other install followed', async () => {
    const lost = { ok: false, network: true, status: 0, error: '서버에 연결할 수 없습니다' };
    const v = make({ replies: [reply([FF, OPT]), reply([FF, OPT], J('downloading', 'ffmpeg', { bytes: 1, total: 4 }))], post: lost });
    await v.run('htLoad()');
    assert.equal(await v.run('htInstall("x-opt")'), false, 'not started for x-opt');
    assert.equal(v.run('HT.busy'), 'ffmpeg', 'the other install is followed');
    const list = v.els.hToolsList.innerHTML;
    const opt = (list.match(/<li[^>]*data-tool="x-opt"[^]*?<\/li>/) || [''])[0];
    assert.ok(opt.includes(LOST), 'x-opt row says lost: ' + list);
    assert.ok(v.calls.announced.some(([m, a]) => m === LOST && a), JSON.stringify(v.calls.announced));
  });

  await row('Tools round 4: the shadow path is in the row text for keyboard / screen-reader users (sr-only), not only in a title', async () => {
    const shadowPath = 'C:\\Users\\me\\AppData\\Local\\Microsoft\\WindowsApps\\ffmpeg.exe';
    const v = make({ replies: [reply([{ ...FF, installed: true, status: 'installed', shadowed_by: { ffmpeg: shadowPath, ffprobe: null } }])] });
    await v.run('htLoad()');
    const ff = (v.els.hToolsList.innerHTML.match(/<li[^>]*data-tool="ffmpeg"[^]*?<\/li>/) || [''])[0];
    const small = (ff.match(/<small class="h-tool-shadow"[^]*?<\/small>/) || [''])[0];
    const text = small.replace(/<small[^>]*>/, '').replace(/<[^>]+>/g, '');
    assert.ok(text.includes('WindowsApps\\ffmpeg.exe') && text.includes('PATH의 다른 사본 무시됨'), 'path in the text: ' + small);
    assert.ok(/<span class="sr-only">[^<]*WindowsApps\\ffmpeg\.exe/.test(small), 'sr-only: ' + small);
  });

  // Fix round 11 M1: [종료] (assets/app/95-quit.js) while a guided install
  // holds busy. The global /api/cancel answers from the mock's apiCancelReply
  // (the server's shapes); /api/status is busy while the install runs.
  // Fix round 12 L5: opts.closeAfterPolls closes the dialog ([계속 작업])
  // after that many status polls during the wait; the dialog's html and
  // OK label are recorded.
  const quitRun = async (stage, finishAfterPolls, opts = {}) => {
    const quitSrc = read('assets/app/95-quit.js');
    const m = { install: { ...J('downloading', 'ffmpeg', { stage, bytes: 1, total: 4 }) }, cancelRequested: false };
    if (stage === 'install') m.install.state = 'extracting';
    const notes = []; const posts = []; const toasts = []; let polls = 0; let okResult = null; let dialog = null;
    const els = {};
    const elOf = (id) => (els[id] ||= {
      id, hidden: false, inert: false, innerHTML: '', disabled: false, listeners: [],
      set textContent(v) { if (id === 'dlgNote') notes.push(v); this._t = v; }, get textContent() { return this._t || ''; },
      addEventListener(t, f) { this.listeners.push(f); }, setAttribute() {}, removeAttribute() {},
    });
    const running = () => m.install.state !== 'done' && m.install.state !== 'failed';
    const ctx = {
      console, window: {}, $: elOf, esc: (s) => String(s),
      setTimeout: (f) => { setImmediate(f); return 1; }, clearTimeout() {},
      hViewer: { summary: () => ({ pending: 0 }) }, FTStore: { hasProject: () => false, state: {} },
      dlgResolve: null,
      jget: async (url) => {
        if (url === '/api/tools/install-status') return { ok: true, install: running() ? { ...m.install } : null };
        if (url !== '/api/status') return { ok: true };
        if (running()) {
          polls += 1;
          if (opts.closeAfterPolls && polls > opts.closeAfterPolls) ctx.dlgResolve = null;
          // Fix round 13 L4: opts.cancelAfterPolls — the cancelled install
          // takes that many polls to wind down.
          if (m.cancelRequested && polls > (opts.cancelAfterPolls || 0)) Object.assign(m.install, { state: 'failed', stage: 'failed', error: INSTALL_CANCELLED_U, cancelled: true });
          else if (finishAfterPolls && polls > finishAfterPolls) Object.assign(m.install, { state: 'done', stage: 'done' });
        }
        return { ok: true, busy: running(), phase: 'task' };
      },
      jpost: async (url) => {
        posts.push(url);
        // Fix round 15: opts.endedAtCancel — the worker finished just before
        // /api/cancel arrived (stage done, `busy` not yet released).
        if (url === '/api/cancel' && opts.endedAtCancel) m.install.stage = 'done';
        if (url === '/api/cancel') return apiCancelReply(m) || { ok: false, error: '진행 중인 작업이 없습니다.' };
        return { ok: true };
      },
      askDialog: async (o) => { dialog = o; ctx.dlgResolve = () => {}; okResult = await o.onOk(); return false; },
      alertDialog: async () => {},
      toast: (msg, kind) => { toasts.push([msg, kind]); },
    };
    // Fix round 14 L2: opts.ht — the tools panel's state (56-tools.js HT).
    if (opts.ht) ctx.HT = opts.ht;
    runInNewContext(quitSrc, ctx);
    await elOf('hBtnQuit').listeners[0]();
    return { okResult, notes, posts, m, dialog, polls, toasts };
  };

  await row('Fix round 11 M1: [종료] during a download stops the install through /api/cancel (cancelled "tool-install") and then shuts the server down', async () => {
    const r = await quitRun('download', 0);
    assert.equal(r.okResult, true, 'onOk proceeds: ' + JSON.stringify(r.notes));
    assert.deepEqual(r.posts, ['/api/cancel', '/api/shutdown'], JSON.stringify(r.posts));
    assert.equal(r.m.install.cancelled, true, 'the install ended cancelled');
    assert.ok(r.notes.some((n) => n.startsWith('도구 설치를 중단하는 중')), JSON.stringify(r.notes));
    assert.ok(!r.notes.some((n) => /중단 요청 실패|아직 멈추지 않았습니다/.test(n)), JSON.stringify(r.notes));
    // Fix round 12 L5: the dialog speaks of the install, not of an analysis.
    assert.ok(r.dialog.html.includes('도구 설치를 중단한 뒤 서버를 종료합니다') && r.dialog.html.includes('(ffmpeg)'), r.dialog.html);
    assert.ok(!/체크포인트/.test(r.dialog.html), 'no checkpoint promise for an install: ' + r.dialog.html);
    assert.equal(r.dialog.okLabel, '설치 중단 후 서버 종료');
  });

  await row('Fix round 11 M1: [종료] while files move into place (not_cancellable) says so, waits for the install to end, then shuts down', async () => {
    const r = await quitRun('install', 3);
    assert.equal(r.okResult, true, 'onOk proceeds: ' + JSON.stringify(r.notes));
    assert.deepEqual(r.posts, ['/api/cancel', '/api/shutdown'], JSON.stringify(r.posts));
    assert.equal(r.m.install.state, 'done', 'the install ran to its end');
    assert.ok(r.notes.includes('파일을 옮기는 중이라 중단할 수 없습니다 — 끝나면 종료합니다'), JSON.stringify(r.notes));
    assert.ok(!r.notes.some((n) => /중단 요청 실패/.test(n)), 'never "중단 요청 실패": ' + JSON.stringify(r.notes));
  });

  await row('Fix round 12 L5: [종료] during a venv/pip step longer than 60 s keeps waiting (up to 10 min), never says "아직 멈추지 않았습니다", then shuts down', async () => {
    const r = await quitRun('pip', 200);
    assert.equal(r.okResult, true, 'onOk proceeds: ' + JSON.stringify(r.notes.slice(-3)));
    assert.deepEqual(r.posts, ['/api/cancel', '/api/shutdown'], JSON.stringify(r.posts));
    assert.equal(r.m.install.state, 'done', 'the install ran to its end');
    assert.ok(r.polls > 120, 'waited past the 60 s bound: ' + r.polls);
    assert.ok(r.notes.some((n) => /^파일을 옮기는 중이라 중단할 수 없습니다 — 끝나면 종료합니다 \d+초 \/ 최대 10분$/.test(n)), JSON.stringify(r.notes.slice(0, 4)));
    assert.ok(!r.notes.some((n) => /아직 멈추지 않았습니다|최대 60초/.test(n)), JSON.stringify(r.notes.filter((n) => /아직|60초/.test(n))));
  });

  await row('Fix round 12 L5: [계속 작업] during the wait for an install stops waiting — the server is NOT shut down afterwards', async () => {
    const r = await quitRun('pip', 50, { closeAfterPolls: 5 });
    assert.equal(r.okResult, false, 'onOk gave up: ' + JSON.stringify(r.notes.slice(-3)));
    assert.deepEqual(r.posts, ['/api/cancel'], 'no /api/shutdown: ' + JSON.stringify(r.posts));
    assert.ok(r.polls <= 7, 'stopped polling soon after the close: ' + r.polls);
    // Fix round 13 L4: not_cancellable — the install really goes on.
    assert.deepEqual(r.toasts, [['도구 설치는 중단되지 않았고 계속 진행됩니다 (파일을 옮기는 중) — 서버는 종료하지 않았습니다.', 'warn']], JSON.stringify(r.toasts));
  });

  await row('Fix round 13 L4: [계속 작업] after the install was already cancelled (download stage) says "설치는 이미 중단됐습니다" — the server stays up', async () => {
    const r = await quitRun('download', 0, { closeAfterPolls: 3, cancelAfterPolls: 10 });
    assert.equal(r.okResult, false, 'onOk gave up: ' + JSON.stringify(r.notes.slice(-3)));
    assert.deepEqual(r.posts, ['/api/cancel'], 'no /api/shutdown: ' + JSON.stringify(r.posts));
    assert.deepEqual(r.toasts, [['설치는 이미 중단됐습니다 — 필요하면 다시 설치하십시오.', 'warn']], JSON.stringify(r.toasts));
    // Not closed: no note at all, the server is shut down as before.
    const shut = await quitRun('download', 0, { cancelAfterPolls: 2 });
    assert.deepEqual(shut.posts, ['/api/cancel', '/api/shutdown'], JSON.stringify(shut.posts));
    assert.deepEqual(shut.toasts, [], JSON.stringify(shut.toasts));
  });

  await row('Fix round 15: [계속 작업] after a cancel that arrived when the install had already ended (requested:false) says it had finished, never "이미 중단됐습니다"', async () => {
    const r = await quitRun('download', 2, { endedAtCancel: true, closeAfterPolls: 1 });
    assert.equal(r.okResult, false, 'onOk gave up: ' + JSON.stringify(r.notes.slice(-3)));
    assert.deepEqual(r.posts, ['/api/cancel'], 'no /api/shutdown: ' + JSON.stringify(r.posts));
    assert.equal(r.m.cancelRequested, false, 'nothing was asked');
    assert.deepEqual(r.toasts, [['도구 설치는 이미 끝나 있었습니다 — 도구 패널에서 결과를 확인하십시오.', 'warn']], JSON.stringify(r.toasts));
  });

  await row('Fix round 14 L2: [계속 작업] noticed only after a not_cancellable install ended says it finished (see the tools panel), never "계속 진행됩니다"; no quit toast while the tools panel follows that install', async () => {
    // Closed at the poll on which the install ends: the wait loop ends first.
    const r = await quitRun('install', 3, { closeAfterPolls: 3 });
    assert.equal(r.okResult, false, 'onOk gave up: ' + JSON.stringify(r.notes.slice(-3)));
    assert.deepEqual(r.posts, ['/api/cancel'], 'no /api/shutdown: ' + JSON.stringify(r.posts));
    assert.equal(r.m.install.state, 'done');
    assert.deepEqual(r.toasts, [['도구 설치가 끝났습니다 — 도구 패널에서 결과를 확인하십시오.', 'warn']], JSON.stringify(r.toasts));
    // Still running when closed: it does continue.
    const cont = await quitRun('pip', 50, { closeAfterPolls: 5 });
    assert.deepEqual(cont.toasts, [['도구 설치는 중단되지 않았고 계속 진행됩니다 (파일을 옮기는 중) — 서버는 종료하지 않았습니다.', 'warn']], JSON.stringify(cont.toasts));
    // A cancelled install keeps "이미 중단됐습니다" either way.
    const cancelled = await quitRun('download', 0, { closeAfterPolls: 3, cancelAfterPolls: 3 });
    assert.deepEqual(cancelled.posts, ['/api/cancel'], JSON.stringify(cancelled.posts));
    assert.deepEqual(cancelled.toasts, [['설치는 이미 중단됐습니다 — 필요하면 다시 설치하십시오.', 'warn']], JSON.stringify(cancelled.toasts));
    // The tools panel follows the same install: it announces the end.
    for (const [stage, finish, o] of [['install', 3, { closeAfterPolls: 3 }], ['pip', 50, { closeAfterPolls: 5 }], ['download', 0, { closeAfterPolls: 3, cancelAfterPolls: 10 }]]) {
      const followed = await quitRun(stage, finish, { ...o, ht: { busy: 'ffmpeg' } });
      assert.deepEqual(followed.posts, ['/api/cancel'], stage + ': ' + JSON.stringify(followed.posts));
      assert.deepEqual(followed.toasts, [], stage + ': ' + JSON.stringify(followed.toasts));
      // Following another tool's install: the quit toast stays.
      const other = await quitRun(stage, finish, { ...o, ht: { busy: 'x-opt' } });
      assert.equal(other.toasts.length, 1, stage + ': ' + JSON.stringify(other.toasts));
    }
  });

  await row('Fix round 13 L2: a tool with rollback_incomplete shows a warning line (note in the title / sr-only), installed or not; en follows the viewer language', async () => {
    const note = '이전 설치 되돌리기가 끝나지 않아 이전 파일이 C:/FT/tools/rollback-backup-00aa (3.0 MB)에 남아 있습니다';
    const v = make({ replies: [reply([{ ...FF, installed: true, status: 'installed', rollback_incomplete: true, rollback_note: note, rollback_backups: ['C:/FT/tools/rollback-backup-00aa'] }, { ...OPT }])] });
    await v.run('htLoad()');
    assert.equal(v.els.hToolsPanel.hidden, false, 'the panel shows the warning');
    const list = v.els.hToolsList.innerHTML;
    const ff = (list.match(/<li[^>]*data-tool="ffmpeg"[^]*?<\/li>/) || [''])[0];
    assert.ok(/class="h-tool h-tool-warn"/.test(ff), 'warn level: ' + ff);
    const small = (ff.match(/<small class="h-tool-rollback"[^]*?<\/small>/) || [''])[0];
    assert.ok(small.includes('이전 설치를 되돌리지 못했습니다') && small.includes('rollback-backup-00aa') && /title="/.test(small), 'warning line: ' + ff);
    assert.ok(!/<button/.test(ff), 'no button on an installed row');
    const opt = (list.match(/<li[^>]*data-tool="x-opt"[^]*?<\/li>/) || [''])[0];
    assert.ok(!/h-tool-rollback/.test(opt), 'no flag, no warning: ' + opt);
    const missing = v.run('htRowHtml(htRowModel(' + JSON.stringify({ ...FF, rollback_incomplete: true, rollback_note: note }) + ', null, {}))');
    assert.ok(/h-tool-rollback/.test(missing) && /data-tool-install="ffmpeg"/.test(missing), 'missing + rollback: ' + missing);
    v.ctx.__locale = 'en';
    v.run('htRenderPanel()');
    assert.ok(v.els.hToolsList.innerHTML.includes('A previous install could not be undone'), 'en: ' + v.els.hToolsList.innerHTML);
    for (const t of TOOLS_FIXTURE()) assert.equal(t.rollback_incomplete, false, t.name + ': fixture carries rollback_incomplete:false');
  });

  if (failed.length) throw new Error('Tools track U failed: ' + failed.join('; '));
}
