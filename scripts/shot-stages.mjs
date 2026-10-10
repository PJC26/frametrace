// Capture stage 3 + 4 screenshots. Prereqs: app on :8477 with case open, Edge on :9222.
const CDP = 'http://127.0.0.1:9222';
const BASE = 'http://127.0.0.1:8477';
// SEC-04: servers require a token by default; pass the one the server was started with.
const TOKEN = process.env.FRAMETRACE_TOKEN || '';
const AUTH_QUERY = TOKEN ? `?token=${encodeURIComponent(TOKEN)}` : '';
const fs = await import('fs');
let nextId = 1;
const pending = new Map();
function connect(url) { return new Promise((res, rej) => { const w = new WebSocket(url); w.onopen = () => res(w); w.onerror = rej; }); }
function send(ws, method, params = {}, sessionId) {
  const id = nextId++;
  return new Promise((resolve, reject) => { pending.set(id, { resolve, reject }); ws.send(JSON.stringify({ id, method, params, sessionId })); });
}
function wire(ws) { ws.onmessage = e => { const m = JSON.parse(e.data); if (m.id && pending.has(m.id)) { const { resolve, reject } = pending.get(m.id); pending.delete(m.id); m.error ? reject(new Error(m.error.message)) : resolve(m.result); } }; }
async function ev(ws, sid, expr) {
  const r = await send(ws, 'Runtime.evaluate', { expression: expr, returnByValue: true, awaitPromise: true, userGesture: true }, sid);
  if (r.exceptionDetails) throw new Error(JSON.stringify(r.exceptionDetails.exception?.description || r.exceptionDetails.text));
  return r.result.value;
}
const sleep = ms => new Promise(r => setTimeout(r, ms));
const version = await (await fetch(CDP + '/json/version')).json();
const ws = await connect(version.webSocketDebuggerUrl); wire(ws);
const { targetId } = await send(ws, 'Target.createTarget', { url: BASE + '/' + AUTH_QUERY });
const { sessionId: sid } = await send(ws, 'Target.attachToTarget', { targetId, flatten: true });
await send(ws, 'Page.enable', {}, sid);
await sleep(4000); // poll → auto-jump to stage 3, iframe loads
await ev(ws, sid, `(() => { document.querySelector('.navstep[data-view="3"]').click(); return 1; })()`);
await sleep(4000);
async function shot(name) { const r = await send(ws, 'Page.captureScreenshot', { format: 'png' }, sid); fs.writeFileSync(`shots-user/${name}.png`, Buffer.from(r.data, 'base64')); console.log('[shot]', name); }
await shot('50-stage3');
await ev(ws, sid, `(() => { document.querySelector('.navstep[data-view="4"]').click(); return 1; })()`);
await sleep(3500);
const r4 = await ev(ws, sid, `(() => ({ wrapHidden: document.getElementById('reportWrap').hidden,
  frameSrc: document.getElementById('reportFrame').src }))()`);
console.log('stage4:', JSON.stringify(r4));
await shot('51-stage4');
ws.close(); process.exit(0);
