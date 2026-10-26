// CDP driver: verify evidence-viewer popup player bridge against a live
// Chrome headless instance on :9222 and the workstation server on :8477.
//
// Usage:
//   1. Start the workstation:  target/debug/frametrace-app.exe
//   2. Open a case:            POST /api/open-case {"case_dir": "..."}
//   3. Start headless Chrome:  chrome --headless=new --remote-debugging-port=9222
//      --user-data-dir=<temp> --autoplay-policy=no-user-gesture-required --mute-audio about:blank
//   4. Run:                    node scripts/verify-viewer-popup.mjs
//
// Covers: __ftBridge mark/tag/navigate/focus, popup open + no-video
// overlay, popup mark/tag/auto-advance/prev-next/rate/keys, orphan
// warning, and main-window transport controls.
const CDP = 'http://127.0.0.1:9222';
// F7: drives a REAL workstation; lib/real-server-guard.mjs (track F)
// refuses the user's own 8477 unless --i-know.
const { guardBase } = await import('./lib/real-server-guard.mjs');
const BASE = guardBase('verify-viewer-popup', process.env.FT_BASE || 'http://127.0.0.1:8477');
const VIEWER = BASE + '/review/evidence-viewer.html';

let nextId = 1;
const pending = new Map();

function connect(url) {
  return new Promise((resolve, reject) => {
    const ws = new WebSocket(url);
    ws.onopen = () => resolve(ws);
    ws.onerror = reject;
  });
}

function send(ws, method, params = {}, sessionId) {
  const id = nextId++;
  return new Promise((resolve, reject) => {
    pending.set(id, { resolve, reject });
    ws.send(JSON.stringify({ id, method, params, sessionId }));
  });
}

function wire(ws) {
  ws.onmessage = (event) => {
    const msg = JSON.parse(event.data);
    if (msg.id && pending.has(msg.id)) {
      const { resolve, reject } = pending.get(msg.id);
      pending.delete(msg.id);
      if (msg.error) reject(new Error(msg.error.message));
      else resolve(msg.result);
    }
  };
}

async function evalIn(ws, sessionId, expression, userGesture = false) {
  const res = await send(ws, 'Runtime.evaluate', {
    expression, returnByValue: true, awaitPromise: true, userGesture,
  }, sessionId);
  if (res.exceptionDetails) {
    throw new Error('page exception: ' + JSON.stringify(res.exceptionDetails.exception?.description || res.exceptionDetails.text));
  }
  return res.result.value;
}

const sleep = (ms) => new Promise(r => setTimeout(r, ms));

async function attachToNewTarget(browserWs, url) {
  const { targetId } = await send(browserWs, 'Target.createTarget', { url });
  const { sessionId } = await send(browserWs, 'Target.attachToTarget', { targetId, flatten: true });
  return { targetId, sessionId };
}

async function main() {
  const results = [];
  const check = (name, ok, detail = '') => {
    results.push({ name, ok, detail });
    console.log(`${ok ? 'PASS' : 'FAIL'}  ${name}${detail ? '  — ' + detail : ''}`);
  };

  const version = await (await fetch(CDP + '/json/version')).json();
  const browserWs = await connect(version.webSocketDebuggerUrl);
  wire(browserWs);
  await send(browserWs, 'Target.setDiscoverTargets', { discover: true });

  // --- main viewer ---
  const main = await attachToNewTarget(browserWs, VIEWER);
  await sleep(2500);

  const recs = await evalIn(browserWs, main.sessionId,
    `(() => ({ count: records.length, active: state.activeId, bridge: typeof window.__ftBridge }))()`);
  check('viewer loads records + bridge exists', recs.count > 0 && recs.bridge === 'object', `records=${recs.count}`);

  // position on the FIRST filtered record with a playable src, and make
  // sure it is not the last one so auto-advance can be observed
  const desc = await evalIn(browserWs, main.sessionId,
    `(() => { const f = filteredRecords(); const i = f.findIndex(r => mediaSrcFor(r));
        state.activeId = f[i].id; render();
        return { d: __ftBridge.descriptor(), idx: i, total: f.length }; })()`);
  check('descriptor returns record with playable src', !!(desc.d && desc.d.src), `id=${desc.d && desc.d.id} pos=${desc.idx + 1}/${desc.total}`);

  // --- /media endpoint serves bytes (Range GET, like <video> uses) ---
  const mediaCheck = await evalIn(browserWs, main.sessionId,
    `fetch('${desc.d.src}', { headers: { Range: 'bytes=0-15' } }).then(async r => ({ status: r.status, len: (await r.arrayBuffer()).byteLength })).catch(e => ({ err: String(e) }))`);
  check('/media serves range bytes', mediaCheck.status === 206 || (mediaCheck.status === 200 && mediaCheck.len > 0), JSON.stringify(mediaCheck));

  // --- mark via bridge: marks + auto-advance + persistence ---
  const markRes = await evalIn(browserWs, main.sessionId,
    `(() => { const f = filteredRecords(); state.activeId = f[0].id; render();
        const before = state.activeId; const after = __ftBridge.mark('important');
        const key = Object.keys(localStorage).find(k => k.endsWith('.marks'));
        const stored = JSON.parse(localStorage.getItem(key) || '{}');
        return { before, afterId: after && after.id, storedMark: stored[before] && stored[before].status, moved: after && after.id !== before }; })()`);
  check('bridge mark() marks + persists + advances', markRes.storedMark === 'important' && markRes.moved, JSON.stringify(markRes));

  // --- tag via bridge: assert TOGGLE semantics (add then remove) ---
  const tagRes = await evalIn(browserWs, main.sessionId,
    `(() => { const d1 = __ftBridge.toggleTag('테스트태그'); const added = d1.tags.includes('테스트태그');
        const d2 = __ftBridge.toggleTag('테스트태그'); const removed = !d2.tags.includes('테스트태그');
        return { added, removed, id: d1.id }; })()`);
  check('bridge toggleTag() toggles on and off', tagRes.added && tagRes.removed, JSON.stringify(tagRes));

  // --- navigate clamps at end ---
  const navRes = await evalIn(browserWs, main.sessionId,
    `(() => { const f = filteredRecords(); state.activeId = f[f.length - 1].id; render();
        const r = __ftBridge.navigate(1); return { moved: r.moved, last: f[f.length - 1].id === state.activeId }; })()`);
  check('navigate clamps at last record', !navRes.moved && navRes.last, JSON.stringify(navRes));

  // --- focus() re-aims activeId ---
  const focusRes = await evalIn(browserWs, main.sessionId,
    `(() => { const f = filteredRecords(); const target = f[0].id; state.activeId = f[f.length - 1].id; render();
        const d = __ftBridge.focus(target); return { aimed: state.activeId === target, descId: d.id === target }; })()`);
  check('focus() re-aims activeId to popup record', focusRes.aimed && focusRes.descId, JSON.stringify(focusRes));

  // --- popup window (userGesture emulates a real click for popup allow) ---
  // Open on the FIRST filtered record — a pre-recovery candidate with no
  // playable src — so next/mark/ended advancement is observable and the
  // "no video" overlay path is exercised too.
  const targetsBefore = new Set((await send(browserWs, 'Target.getTargets')).targetInfos.map(t => t.targetId));
  await evalIn(browserWs, main.sessionId,
    `(() => { const f = filteredRecords(); state.activeId = f[0].id; render(); openPlayerWindow(); return true; })()`, true);
  await sleep(1500);
  const targetsAfter = (await send(browserWs, 'Target.getTargets')).targetInfos;
  const popupInfo = targetsAfter.find(t => !targetsBefore.has(t.targetId) && t.type === 'page');
  check('popup window opens as new target', !!popupInfo, popupInfo && popupInfo.url);

  if (popupInfo) {
    const { sessionId: pop } = await send(browserWs, 'Target.attachToTarget', { targetId: popupInfo.targetId, flatten: true });
    await sleep(800);
    const popState = await evalIn(browserWs, pop,
      `(() => ({ hasVideo: !!document.getElementById('vv'), bridge: typeof B(), title: document.getElementById('title').textContent, src: document.getElementById('vv').getAttribute('src'), novid: !document.getElementById('novid').hidden, cur: cur && cur.id }))()`);
    check('popup has video + bridge + title', popState.hasVideo && popState.bridge === 'object' && !!popState.title, `title="${popState.title}"`);
    check('popup shows no-video overlay for pre-recovery candidate', popState.novid && !popState.src, `cur=${popState.cur}`);

    // mark button in popup -> opener state changes + popup advances AND
    // loads the next record's playable src
    const popMark = await evalIn(browserWs, pop,
      `(() => { const before = cur.id; document.querySelector('.mk[data-m="important"]').click();
        return { before, after: cur.id, moved: cur.id !== before, src: document.getElementById('vv').getAttribute('src'), novid: !document.getElementById('novid').hidden }; })()`);
    check('popup mark button advances + loads next video', popMark.moved && !!popMark.src && !popMark.novid, JSON.stringify(popMark));

    // verify mark actually landed on the record that was displayed
    const markVerify = await evalIn(browserWs, main.sessionId,
      `(() => { const key = Object.keys(localStorage).find(k => k.endsWith('.marks')); return JSON.parse(localStorage.getItem(key))['${popMark.before}']?.status; })()`);
    check('popup mark persisted on displayed record', markVerify === 'important', `mark=${markVerify}`);

    // tag toggle in popup — assert the state flips both ways
    const popTag = await evalIn(browserWs, pop,
      `(() => { const btn = document.querySelector('.tg'); const t = btn.dataset.tag;
        const was = cur.tags.includes(t); btn.click(); const now1 = cur.tags.includes(t);
        btn.click(); const now2 = cur.tags.includes(t);
        return { tag: t, was, now1, now2, ui: btn.classList.contains('on') }; })()`);
    check('popup tag toggle flips record state + button class', popTag.now1 !== popTag.was && popTag.now2 === popTag.was && popTag.ui === popTag.was, JSON.stringify(popTag));

    // ended -> auto-advance (go back to first record so advance is observable)
    const popEnd = await evalIn(browserWs, pop,
      `(() => { document.getElementById('prev').click(); const onFirst = cur.id;
        document.getElementById('vv').dispatchEvent(new Event('ended'));
        return { onFirst, after: cur.id, moved: cur.id !== onFirst }; })()`);
    check('ended event auto-advances to next record', popEnd.moved, JSON.stringify(popEnd));

    // prev/next buttons
    const popNav = await evalIn(browserWs, pop,
      `(() => { const a = cur.id; document.getElementById('prev').click(); const b = cur.id; document.getElementById('next').click(); return { a, back: b, ret: cur.id }; })()`);
    check('popup prev/next navigation round-trips', popNav.a === popNav.ret && popNav.a !== popNav.back, JSON.stringify(popNav));

    // rate select
    const popRate = await evalIn(browserWs, pop,
      `(() => { const r = document.getElementById('rate'); r.value = '2'; r.dispatchEvent(new Event('change'));
        return { rate: document.getElementById('vv').playbackRate }; })()`);
    check('popup rate select sets playbackRate', popRate.rate === 2, `rate=${popRate.rate}`);

    // keyboard seek
    const popKeys = await evalIn(browserWs, pop,
      `(() => { const v = document.getElementById('vv'); Object.defineProperty(v, 'duration', { value: 100, configurable: true });
        v.currentTime = 50; document.dispatchEvent(new KeyboardEvent('keydown', { key: 'ArrowRight' }));
        const fwd = v.currentTime; document.dispatchEvent(new KeyboardEvent('keydown', { key: 'ArrowLeft' }));
        return { fwd, back: v.currentTime }; })()`);
    check('popup arrow keys seek +-10s', popKeys.fwd === 60 && popKeys.back === 50, JSON.stringify(popKeys));

    // orphan guard: simulate opener bridge gone
    const orphan = await evalIn(browserWs, pop,
      `(() => { window.opener.__ftBridge = null; document.getElementById('next').click();
        return { note: document.getElementById('note').textContent }; })()`);
    check('popup shows warning when opener bridge gone', orphan.note.includes('닫혔습니다'), orphan.note);

    await send(browserWs, 'Target.closeTarget', { targetId: popupInfo.targetId });
  }

  // --- main-window transport controls ---
  const mainTransport = await evalIn(browserWs, main.sessionId,
    `(() => { const v = document.querySelector('#mediaStage video');
      if (!v) return { video: false };
      Object.defineProperty(v, 'duration', { value: 200, configurable: true });
      v.currentTime = 100;
      document.getElementById('btnSkipBack').click(); const back = v.currentTime;
      document.getElementById('btnSkipFwd').click(); const fwd = v.currentTime;
      const sel = document.getElementById('playRate'); sel.value = '0.5'; sel.dispatchEvent(new Event('change'));
      return { video: true, back, fwd, rate: v.playbackRate }; })()`);
  check('main viewer skip buttons +-10s', mainTransport.video && mainTransport.back === 90 && mainTransport.fwd === 100, JSON.stringify(mainTransport));
  check('main viewer rate select sets playbackRate', mainTransport.rate === 0.5, `rate=${mainTransport.rate}`);

  // rate persists in layout state
  const persisted = await evalIn(browserWs, main.sessionId,
    `(() => { const key = Object.keys(localStorage).find(k => k.includes('.layout')); const l = JSON.parse(localStorage.getItem(key)); return l.rate; })()`);
  check('rate persisted in layout state', persisted === 0.5, `layout.rate=${persisted}`);

  await send(browserWs, 'Target.closeTarget', { targetId: main.targetId });

  const failed = results.filter(r => !r.ok);
  console.log(`\n=== ${results.length - failed.length}/${results.length} passed ===`);
  process.exit(failed.length ? 1 : 0);
}

main().catch(e => { console.error('DRIVER ERROR:', e); process.exit(2); });
