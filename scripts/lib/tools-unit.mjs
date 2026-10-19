// Tools track U (docs/PLAN_TOOLS_2026-10-20.md U1-U2, contract §28):
// assets/app/56-tools.js in a VM with stubbed network / dialog / timers.
// Run from `node scripts/verify-review-batch.mjs --unit`.
import { readFileSync } from 'node:fs';
import { runInNewContext } from 'node:vm';
import assert from 'node:assert/strict';
import { appBundleJs } from './app-bundle.mjs';

const read = (rel) => readFileSync(new URL('../../' + rel, import.meta.url), 'utf8');

export async function toolsTrackU() {
  const appJs = appBundleJs();
  const toolsSrc = read('assets/app/56-tools.js').split('(function htWire()')[0];
  const finSrc = read('assets/app/41-finalize.js');
  const startSrc = read('assets/app/50-start.js');
  const html = read('assets/examiner_app.html');
  const legacyHtml = read('assets/legacy/examiner_app.html');
  const fmtBytesSrc = appJs.match(/^function fmtBytes\([^]*?^}/m)[0];
  const escSrc = appJs.match(/^const esc = .*$/m)[0];
  const failed = [];
  const row = async (name, body) => {
    try { await body(); console.log('PASS VM ' + name); } catch (err) { failed.push(name); console.log('FAIL VM ' + name + ': ' + (err && err.message || err)); }
  };
  const tick = async (n = 6) => { for (let i = 0; i < n; i++) await new Promise((r) => setImmediate(r)); };
  const plain = (v) => JSON.parse(JSON.stringify(v));
  const FF = { name: 'ffmpeg', label: 'FFmpeg', required: true, installed: false, path: null, version: null, pinned_version: '7.1', license: 'LGPL-2.1', homepage: 'https://ffmpeg.org/', source_host: 'github.com', size_bytes: 4 * 1048576, installable: true, unavailable_reason: null, kind: 'binary' };
  const EWF = { name: 'libewf', label: 'libewf', required: false, installed: false, pinned_version: '20231119', license: 'LGPL-3.0', homepage: 'https://github.com/libyal/libewf', source_host: 'github.com', size_bytes: null, installable: true, kind: 'binary' };
  const TSK = { name: 'sleuthkit', label: 'Sleuth Kit', required: false, installed: false, license: 'IPL/CPL', homepage: 'javascript:alert(1)', installable: false, unavailable_reason: '공식 체크섬 없음', kind: 'binary' };
  const DF = { name: 'deepfake-lens', label: 'deepfake-lens', required: false, installed: false, pinned_version: '0.4.2', license: 'MIT', homepage: 'https://pypi.org/project/deepfake-lens/', source_host: 'pypi.org', size_bytes: 2048, installable: true, kind: 'pip', pip_command: 'pip install deepfake-lens==0.4.2' };
  const el = (id) => ({ id, hidden: true, innerHTML: '', textContent: '', dataset: {}, classList: { toggle() {} }, listeners: [], addEventListener(t, f) { this.listeners.push(f); }, contains: () => false, querySelector: () => null, querySelectorAll: () => [], setAttribute() {}, closest: () => null });
  // A fresh VM: GET /api/tools answers from a script of replies (the last repeats).
  const make = ({ replies = [], confirm = true, post = { ok: true } } = {}) => {
    const els = {};
    const calls = { dialogs: [], posts: [], gets: 0, timers: [], loadEnv: 0, toasts: [], announced: [] };
    const ctx = {
      console, document: { activeElement: null }, setTimeout: (f) => { calls.timers.push(f); return calls.timers.length; }, clearTimeout() {},
      $: (id) => (els[id] ||= el(id)),
      jgetSoft: async () => { calls.gets += 1; const r = replies.length > 1 ? replies.shift() : replies[0]; return r === undefined ? null : plain(r); },
      jpost: async (url, body) => { calls.posts.push({ url, body }); return post; },
      askDialog: async (o) => { calls.dialogs.push(o); return confirm; },
      announce: (m, a) => calls.announced.push([m, !!a]), toast: (m, k) => calls.toasts.push([m, k]), loadEnv: async () => { calls.loadEnv += 1; },
      hLocalePref: () => ctx.__locale || '',
    };
    runInNewContext(escSrc + '\n' + fmtBytesSrc + '\n' + toolsSrc, ctx);
    return { ctx, els, calls, run: (expr) => runInNewContext(expr, ctx) };
  };
  const reply = (tools, install = { state: 'idle', tool: null, bytes: null, total: null, error: null }) => ({ ok: true, tools, install });

  await row('Tools U1 an older server (404 / no tools) shows no panel, never an error', async () => {
    for (const r of [{ ok: false, status: 404, error: 'x' }, { ok: false, network: true, status: 0 }, { ok: true }, null]) {
      const v = make({ replies: [r] });
      await v.run('htLoad()');
      assert.equal(v.els.hToolsPanel.hidden, true, 'U1: hidden for ' + JSON.stringify(r));
      assert.equal(v.run('htNeedsTool("deepfake-lens")'), false, 'U1: an older server never asks to install');
    }
  });

  await row('Tools U1 panel: required FFmpeg = warning, optional = info + 설치, not installable = reason + https homepage; deepfake-lens never listed', async () => {
    const v = make({ replies: [reply([FF, EWF, TSK, DF, { ...EWF, name: 'x-ok', installed: true }])] });
    await v.run('htLoad()');
    const list = v.els.hToolsList.innerHTML;
    assert.equal(v.els.hToolsPanel.hidden, false, 'U1: shown when a tool is missing');
    assert.ok(/h-tool-warn" data-tool="ffmpeg"/.test(list) && list.includes('FFmpeg 없음 — 필수'), 'U1: FFmpeg missing is a warning: ' + list);
    assert.ok(/h-tool-info" data-tool="libewf"/.test(list) && /data-tool-install="libewf"[^>]*>설치</.test(list), 'U1: libewf info + 설치');
    assert.ok(/data-tool="sleuthkit"/.test(list) && list.includes('공식 체크섬 없음') && !/data-tool-install="sleuthkit"/.test(list), 'U1: not installable → reason, no button');
    assert.ok(!/javascript:/.test(list), 'U1: a non-https homepage is never shown as a link');
    assert.ok(!/deepfake/.test(list) && !/pip install/.test(list), 'U1: deepfake-lens / pip never in the panel');
    assert.ok(!/x-ok/.test(list), 'U1: installed tools are not listed');
    const m = v.run('htRowModel(' + JSON.stringify({ ...EWF, installable: false, unavailable_reason: '', homepage: 'https://github.com/libyal/libewf' }) + ', null, {})');
    assert.equal(m.action, 'manual'); assert.equal(m.homepage, 'https://github.com/libyal/libewf'); assert.equal(m.reason, '여기서 설치할 수 없음');
    assert.ok(v.run('htRowHtml(htRowModel(' + JSON.stringify({ ...TSK, homepage: 'https://sleuthkit.org/' }) + ', null, {}))').includes('수동 설치: <a href="https://sleuthkit.org/"'), 'U1: manual-install hint links the homepage');
    const allOk = make({ replies: [reply([{ ...FF, installed: true }])] });
    await allOk.run('htLoad()');
    assert.equal(allOk.els.hToolsPanel.hidden, true, 'U1: nothing missing → no panel');
  });

  await row('Tools U1 confirm dialog lists file · source host · size · license (escaped) before anything is sent; cancel sends nothing', async () => {
    const v = make({ replies: [reply([FF, { ...EWF, license: '<b>x</b>' }, TSK])], confirm: false });
    await v.run('htLoad()');
    assert.equal(await v.run('htInstall("libewf")'), false);
    assert.equal(v.calls.posts.length, 0, 'U1: cancel → no POST');
    const d = v.calls.dialogs[0];
    assert.ok(d && d.title === 'libewf 설치' && d.html.includes('libewf 20231119') && d.html.includes('github.com') && d.html.includes('알 수 없음') && d.html.includes('&lt;b&gt;x&lt;/b&gt;'), 'U1: dialog body: ' + (d && d.html));
    const ff = v.run('htConfirmHtml(' + JSON.stringify(FF) + ')');
    assert.ok(ff.includes('FFmpeg 7.1') && ff.includes('4.0 MB') && ff.includes('LGPL-2.1') && ff.includes('출처'), 'U1: size shown: ' + ff);
    assert.equal(await v.run('htInstall("sleuthkit")'), false, 'U1: not installable → nothing');
    assert.equal(v.calls.dialogs.length, 1, 'U1: no dialog for a tool without a button');
  });

  await row('Tools U1 install: one POST {name}, duplicate clicks ignored, progress polled (bytes), result shown, env refreshed', async () => {
    const st = (state, bytes) => reply([FF, EWF], { state, tool: 'libewf', bytes, total: 4 * 1048576, error: null });
    const v = make({ replies: [reply([FF, EWF]), st('downloading', 1048576), st('verifying', 4 * 1048576), reply([FF, { ...EWF, installed: true }], { state: 'done', tool: 'libewf', bytes: 1, total: 1, error: null })] });
    await v.run('htLoad()');
    const p = v.run('htInstall("libewf")');
    const dup = v.run('htInstall("libewf")');
    await p; await dup;
    assert.equal(v.calls.posts.length, 1, 'U1: one POST');
    assert.equal(v.calls.dialogs.length, 1, 'U1: the second click opens no dialog');
    assert.deepEqual(plain(v.calls.posts[0]), { url: '/api/tools/install', body: { name: 'libewf' } });
    await tick();
    let list = v.els.hToolsList.innerHTML;
    assert.ok(list.includes('내려받는 중 1.0 MB / 4.0 MB (25%)') && /data-tool-install="libewf"[^>]* disabled/.test(list) && /data-tool-install="ffmpeg"[^>]* disabled/.test(list), 'U1: progress + every install button disabled: ' + list);
    assert.ok(v.els.hToolsLive.textContent.includes('내려받는 중'), 'U2: the live region names the state (not every byte)');
    v.calls.timers.shift()(); await tick();
    assert.ok(v.els.hToolsList.innerHTML.includes('해시 확인 중'), 'U1: verifying shown');
    v.calls.timers.shift()(); await tick(10);
    list = v.els.hToolsList.innerHTML;
    assert.ok(list.includes('libewf 설치됨') && /h-tool-ok/.test(list), 'U1: result row: ' + list);
    assert.ok(v.calls.loadEnv === 1 && v.calls.toasts.some(([m, k]) => m === 'libewf 설치됨' && k === 'ok'), 'U1: env refreshed + toast');
    assert.equal(v.run('HT.busy'), '', 'U1: idle again');
  });

  await row('Tools U1 an install failure and a refused request are shown (failure assertively), and can be retried', async () => {
    const v = make({ replies: [reply([EWF]), reply([EWF], { state: 'failed', tool: 'libewf', bytes: null, total: null, error: 'SHA-256 불일치' })] });
    await v.run('htLoad()');
    await v.run('htInstall("libewf")'); await tick(10);
    const list = v.els.hToolsList.innerHTML;
    assert.ok(list.includes('설치 실패: SHA-256 불일치') && v.calls.announced.some(([m, a]) => a && /설치 실패/.test(m)), 'U1: failure: ' + list);
    assert.ok(/data-tool-install="libewf"/.test(list) && !/data-tool-install="libewf"[^>]*disabled/.test(list), 'U1: retry possible');
    const r = make({ replies: [reply([EWF])], post: { ok: false, error: 'busy' } });
    await r.run('htLoad()');
    await r.run('htInstall("libewf")');
    assert.ok(r.els.hToolsList.innerHTML.includes('설치 요청 실패: busy'), 'U1: refused request shown');
  });

  await row('Tools U1 finalize checklist: one warning line per missing required tool only', async () => {
    const v = make({ replies: [reply([FF, EWF, DF])] });
    const c = plain(await v.run('htFinalizeChecks()'));
    assert.deepEqual(c.map((x) => [x.level, x.text]), [['warn', 'FFmpeg 없음 — 영상 검증·프록시를 만들 수 없음']]);
    const none = make({ replies: [{ ok: false, status: 404 }] });
    assert.equal((await none.run('htFinalizeChecks()')).length, 0, 'U1: older server → no line');
    assert.ok(/htFinalizeChecks\(\)/.test(finSrc), 'U1: gatherFinalizeChecks uses it');
  });

  await row('Tools deepfake-lens prompt: tool_missing → "합성 영상 검사에 필요한 도구가 없습니다" → 설치 (pypi.org) → progress → 다시 검사', async () => {
    const v = make({ replies: [reply([FF, DF]), reply([FF, DF]), reply([FF, DF], { state: 'downloading', tool: 'deepfake-lens', bytes: 1024, total: 2048, error: null }), reply([FF, { ...DF, installed: true }], { state: 'done', tool: 'deepfake-lens', bytes: 2048, total: 2048, error: null })] });
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
    assert.ok(d && d.html.includes('pypi.org') && d.html.includes('deepfake-lens 0.4.2') && d.html.includes('2.0 KB') && d.html.includes('MIT'), 'dialog: ' + (d && d.html));
    assert.ok(!/pip install/.test(d.html + box.innerHTML), 'the pip command is never shown');
    assert.deepEqual(plain(v.calls.posts[0].body), { name: 'deepfake-lens' });
    await tick();
    assert.ok(box.innerHTML.includes('내려받는 중 1.0 KB / 2.0 KB (50%)') && /data-ht-prompt="install"[^>]*disabled/.test(box.innerHTML), 'progress: ' + box.innerHTML);
    v.calls.timers.shift()(); await tick(10);
    assert.ok(box.innerHTML.includes('deepfake-lens 설치됨') && /data-ht-prompt="retry"[^>]*>다시 검사</.test(box.innerHTML), 'retry offered: ' + box.innerHTML);
    click('retry');
    assert.equal(retried, 1, 'retry runs the screening again');
    assert.ok(box.hidden, 'the prompt goes away on retry');
  });

  await row('Tools deepfake-lens prompt: not installable → only the server reason; older server → manual hint, no button', async () => {
    const v = make({ replies: [reply([{ ...DF, installable: false, unavailable_reason: 'Python 3.10 이상이 없습니다' }])] });
    await v.run('htShowToolPrompt($("startToolPrompt"), "deepfake-lens", () => 0, "retryStart")');
    const b = v.ctx.$('startToolPrompt').innerHTML;
    assert.ok(b.includes('합성 영상 검사에 필요한 도구가 없습니다') && b.includes('Python 3.10 이상이 없습니다') && !/<button/.test(b), b);
    const o = make({ replies: [{ ok: false, status: 404 }] });
    await o.run('htShowToolPrompt($("startToolPrompt"), "deepfake-lens", () => 0)');
    assert.ok(!/<button/.test(o.ctx.$('startToolPrompt').innerHTML), 'older server: no button');
    assert.ok(/res\.tool_missing/.test(finSrc) && /res\.tool_missing/.test(startSrc), 'both run paths read tool_missing');
    assert.ok(/htNeedsTool\("deepfake-lens"\)/.test(finSrc) && /htNeedsTool\("deepfake-lens"\)/.test(startSrc), 'a known-missing tool prompts before running');
  });

  await row('Tools U2 ko/en strings match and follow the viewer language; live region, labelled list; legacy shell: one hint line', async () => {
    const v = make({ replies: [reply([FF])] });
    assert.deepEqual(Object.keys(v.run('HT_STR.en')).sort(), Object.keys(v.run('HT_STR.ko')).sort(), 'U2: same keys');
    v.ctx.__locale = 'en';
    await v.run('htLoad()');
    assert.ok(v.els.hToolsList.innerHTML.includes('FFmpeg missing — required') && v.els.hToolsPanelTitle.textContent === 'Tools', 'U2: English');
    assert.ok(/id="hToolsPanel"[^>]*aria-labelledby="hToolsPanelTitle"[^>]*hidden/.test(html) && /id="hToolsLive" role="status" aria-live="polite"/.test(html), 'U2: markup');
    assert.ok(/id="startToolPrompt"/.test(html) && /id="advToolPrompt"/.test(finSrc), 'U2: prompt mounts');
    assert.ok(!/class="sblock[^"]*" id="hToolsPanel"/.test(html), 'the empty state keeps exactly two blocks');
    assert.equal((legacyHtml.match(/frametrace tools list/g) || []).length, 1, 'U2: legacy hint');
  });

  if (failed.length) throw new Error('Tools track U failed: ' + failed.join('; '));
}
