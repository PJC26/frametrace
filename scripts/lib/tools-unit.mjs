// Tools track U (docs/PLAN_TOOLS_2026-10-20.md U1-U2 + "§S as implemented",
// contract §28): assets/app/56-tools.js in a VM with stubbed network /
// dialog / timers, fed the real track S shapes.
// Run from `node scripts/verify-review-batch.mjs --unit`.
import { readFileSync } from 'node:fs';
import { runInNewContext } from 'node:vm';
import assert from 'node:assert/strict';
import { appBundleJs } from './app-bundle.mjs';
import { TOOLS_FIXTURE } from './tools-mock.mjs';

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
  const J = (state, tool, extra = {}) => ({ id: 7, tool, state, stage: { downloading: 'download', verifying: 'verify', extracting: 'install', done: 'done', failed: 'failed' }[state], bytes: null, total: null, error: null, started_unix: 1, finished_unix: null, installed_files: [], audit_warning: null, ...extra });
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
      askDialog: async (o) => { calls.dialogs.push(o); return confirm; },
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
    assert.ok(/h-tool-warn" data-tool="ffmpeg"/.test(list) && list.includes('FFmpeg (LGPL build) 없음 — 필수') && /data-tool-install="ffmpeg"[^>]*>설치</.test(list), 'U1: FFmpeg missing is a warning: ' + list);
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
    assert.ok(d && d.title === 'FFmpeg (LGPL build) 설치' && d.html.includes('<dd>ffmpeg-n8.1.3-9-g29e619e767-win64-lgpl-8.1.zip</dd>') && d.html.includes('github.com') && d.html.includes('4.0 MB') && d.html.includes('LGPL-3.0-or-later'), 'U1: dialog body: ' + (d && d.html));
    const x = v.run('htConfirmHtml(' + JSON.stringify({ ...OPT, download: null, size_bytes: null, pinned_version: '1.2', license: '<b>x</b>' }) + ')');
    assert.ok(x.includes('X 도구 1.2') && x.includes('알 수 없음') && x.includes('&lt;b&gt;x&lt;/b&gt;'), 'U1: fallbacks: ' + x);
    assert.equal(await v.run('htInstall("sleuthkit")'), false, 'U1: not installable → nothing');
    assert.equal(v.calls.dialogs.length, 1, 'U1: no dialog for a tool without a button');
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
    assert.ok(v.els.hToolsList.innerHTML.includes('FFmpeg (LGPL build) 설치됨') && v.calls.loadEnv === 1 && v.calls.toasts.some(([, k]) => k === 'ok'), 'U1: result + loadEnv: ' + v.els.hToolsList.innerHTML);
    const idle = make({ replies: [reply([FF], J('failed', 'ffmpeg', { error: 'x' }))] });
    await idle.run('htLoad()');
    assert.equal(idle.calls.timers.length, 0, 'U1: a finished job is not polled');
  });

  await row('Tools U1 [중단]: POST /api/tools/install-cancel; not_cancellable → "이 단계는 중단할 수 없습니다" and the install goes on; accepted → "설치를 중단했습니다"', async () => {
    const v = make({ replies: [reply([OPT], J('extracting', 'x-opt'))], status: [st(J('extracting', 'x-opt'))], cancel: { ok: false, code: 'not_cancellable', error: 'x' } });
    await v.run('htLoad()');
    assert.ok(/data-tool-cancel\][^]*htCancel\(b\.dataset\.toolCancel\)/.test(read('assets/app/56-tools.js').split('(function htWire()')[1]), 'U1: the panel wires [중단] to htCancel');
    await v.run('htCancel("x-opt")');
    assert.deepEqual(plain(v.calls.posts[0]), { url: '/api/tools/install-cancel', body: {} });
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
    assert.deepEqual(c.map((x) => [x.level, x.text]), [['warn', 'FFmpeg (LGPL build) 없음 — 영상 검증·프록시를 만들 수 없음']]);
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
    assert.ok(v.els.hToolsList.innerHTML.includes('FFmpeg (LGPL build) missing — required') && v.els.hToolsPanelTitle.textContent === 'Tools', 'U2: English');
    assert.ok(/id="hToolsPanel"[^>]*aria-labelledby="hToolsPanelTitle"[^>]*hidden/.test(html), 'U2: markup');
    assert.ok(/id="startToolPrompt"/.test(html) && /id="advToolPrompt"/.test(finSrc), 'U2: prompt mounts');
    assert.ok(!/class="sblock[^"]*" id="hToolsPanel"/.test(html), 'the empty state keeps exactly two blocks');
    assert.equal((legacyHtml.match(/frametrace tools list/g) || []).length, 1, 'U2: legacy hint');
  });

  if (failed.length) throw new Error('Tools track U failed: ' + failed.join('; '));
}
