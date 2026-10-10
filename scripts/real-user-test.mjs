// Real first-time user journey, driven purely through DOM like a person:
// open page -> read onboarding -> dismiss -> type paths -> start analysis ->
// wait for review -> inspect a record -> generate report -> quit.
const CDP = 'http://127.0.0.1:9223';
const BASE = 'http://127.0.0.1:8477';
// SEC-04: servers require a token by default; pass the one the server was started with.
const TOKEN = process.env.FRAMETRACE_TOKEN || '';
const AUTH_QUERY = TOKEN ? `?token=${encodeURIComponent(TOKEN)}` : '';
const SOURCE = 'D:\\ft-usertest\\evidence';
const CASE = 'D:\\ft-usertest\\case-001';
const SHOTS = 'shots-realuser';
const fs = await import('fs');

let nextId = 1;
const pending = new Map();
function send(ws, method, params = {}, sessionId) {
  const id = nextId++;
  return new Promise((resolve, reject) => {
    pending.set(id, { resolve, reject });
    ws.send(JSON.stringify({ id, method, params, sessionId }));
  });
}
const sleep = (ms) => new Promise(r => setTimeout(r, ms));

async function main() {
  fs.mkdirSync(SHOTS, { recursive: true });
  const version = await (await fetch(CDP + '/json/version')).json();
  const ws = new WebSocket(version.webSocketDebuggerUrl);
  await new Promise(r => ws.onopen = r);
  ws.onmessage = e => {
    const m = JSON.parse(e.data);
    if (m.id && pending.has(m.id)) {
      const p = pending.get(m.id);
      pending.delete(m.id);
      if (m.error) p.reject(new Error(m.error.message)); else p.resolve(m.result);
    }
  };
  const { targetId } = await send(ws, 'Target.createTarget', { url: BASE + '/' + AUTH_QUERY });
  const { sessionId } = await send(ws, 'Target.attachToTarget', { targetId, flatten: true });
  const ev = async (expr) => {
    const r = await send(ws, 'Runtime.evaluate', { expression: expr, returnByValue: true, awaitPromise: true, userGesture: true }, sessionId);
    if (r.exceptionDetails) throw new Error('page exception: ' + JSON.stringify(r.exceptionDetails));
    return r.result.value;
  };
  const shot = async (name) => {
    const r = await send(ws, 'Page.captureScreenshot', { format: 'png' }, sessionId);
    fs.writeFileSync(`${SHOTS}/${name}.png`, Buffer.from(r.data, 'base64'));
    console.log(`  [shot] ${name}`);
  };
  const waitFor = async (expr, timeoutMs, label) => {
    const t0 = Date.now();
    while (Date.now() - t0 < timeoutMs) {
      if (await ev(expr)) return true;
      await sleep(500);
    }
    throw new Error('timeout: ' + label);
  };

  // 1. First screen — onboarding visible?
  await sleep(3000);
  const s1 = await ev(`JSON.stringify({
    onboard: !document.getElementById('onboard').hidden,
    onboardVisible: document.getElementById('onboard').offsetHeight > 0,
    view: [...document.querySelectorAll('.view')].find(v=>v.classList.contains('on'))?.id,
    badges: document.querySelectorAll('#env .badge').length,
    quitBtn: !!document.getElementById('btnQuit')
  })`);
  console.log('1. first screen:', s1);
  await shot('01-first-onboarding');

  // 2. Dismiss onboarding
  await ev(`document.getElementById('btnOnboardOk').click(); 'ok'`);
  await sleep(300);
  const dismissed = await ev(`document.getElementById('onboard').hidden`);
  console.log('2. onboarding dismissed:', dismissed);

  // 3. Type source path + case dir like a user
  await ev(`(() => { const el=document.getElementById('sourcePath'); el.value=${JSON.stringify(SOURCE)}; el.dispatchEvent(new Event('input')); })()`);
  await ev(`(() => { const el=document.getElementById('caseDir'); el.value=${JSON.stringify(CASE)}; el.dispatchEvent(new Event('input')); })()`);
  await sleep(1000);
  const paths = await ev(`JSON.stringify({
    src: document.getElementById('sourceStatus').textContent,
    case: document.getElementById('caseStatus').textContent
  })`);
  console.log('3. path validation:', paths);
  await shot('02-input-filled');

  // 4. Click 분석 시작
  await ev(`document.getElementById('btnStart').click(); 'started'`);
  await sleep(2000);
  await shot('03-progress');
  const prog = await ev(`JSON.stringify({view:[...document.querySelectorAll('.view')].find(v=>v.classList.contains('on'))?.id, log:(document.getElementById('log')||{}).textContent?.slice(0,300)})`);
  console.log('4. progress started:', prog);

  // 5. Wait for review stage (poll status via DOM stage switch)
  try {
    await waitFor(`[...document.querySelectorAll('.view')].find(v=>v.classList.contains('on'))?.id === 'resultCard'`, 300000, 'review stage');
    console.log('5. reached review stage');
    await sleep(2500);
    await shot('04-review');
  } catch (e) {
    const err = await ev(`JSON.stringify({view:[...document.querySelectorAll('.view')].find(v=>v.classList.contains('on'))?.id, log:(document.getElementById('log')||{}).textContent?.slice(-800)})`);
    console.log('5. FAILED to reach review:', err);
    throw e;
  }

  // 6. Review stats + try marking a record in the iframe viewer
  const stats = await ev(`(document.getElementById('caseStats')||{}).textContent`);
  console.log('6. case stats:', stats);

  // 7. Results stage — generate report/package
  await ev(`document.getElementById('s4').click(); 's4'`);
  await sleep(600);
  await ev(`document.getElementById('btnFinalize').click(); 'finalizing'`);
  try {
    await waitFor(`document.querySelectorAll('#finalLinks a').length > 0 || (document.getElementById('finalHint')||{}).textContent?.includes('완료')`, 180000, 'report links');
  } catch { /* continue to read state */ }
  await sleep(1500);
  const fin = await ev(`JSON.stringify({
    links: [...document.querySelectorAll('#finalLinks a')].map(a=>a.textContent.trim()),
    pkg: (document.getElementById('pkgPath')||{}).textContent,
    hint: (document.getElementById('finalHint')||{}).textContent
  })`);
  console.log('7. finalize:', fin);
  await shot('05-results');

  // 8. Quit via the header button (override confirm dialogs like a user clicking OK)
  await ev(`window.confirm = () => true; 'armed'`);
  await ev(`document.getElementById('btnQuit').click(); 'quit-clicked'`);
  await sleep(3000);
  const quit = await ev(`JSON.stringify({quitPage: !document.getElementById('quitPage').hidden})`).catch(() => '{"quitPage":"eval-failed"}');
  console.log('8. quit page shown:', quit);
  await shot('06-quit');

  // 9. Server actually dead?
  await sleep(2000);
  const alive = await fetch(BASE + '/api/status', { headers: TOKEN ? { 'X-FrameTrace-Token': TOKEN } : {}, signal: AbortSignal.timeout(3000) }).then(() => true).catch(() => false);
  console.log('9. server alive after quit:', alive);
  process.exit(0);
}
main().catch(e => { console.log('DRIVER ERROR:', e.message); process.exit(1); });
