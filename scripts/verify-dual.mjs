// Verify dual-channel sync playback in the evidence viewer.
// MANUAL harness - not part of scripts/ci-harnesses.sh (needs an externally
// started Chrome and a case with a dual-channel pair; T13).
// Prereq: headless Chrome with --remote-debugging-port=9223 (override with CDP_URL).
// Usage: node scripts/verify-dual.mjs <page-url> [shotdir]
// Exits non-zero when the dual view does not show two synced panes/videos.
const CDP = process.env.CDP_URL || 'http://127.0.0.1:9223';
const URL_ = process.argv[2];
if (!URL_) { console.error('usage: node scripts/verify-dual.mjs <evidence-viewer url> [shotdir]'); process.exit(2); }
const SHOTS = process.argv[3] || 'tmp/dualtest/shots';
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
  if (r.exceptionDetails) throw new Error(JSON.stringify(r.exceptionDetails));
  return r.result.value;
}
const sleep = ms => new Promise(r => setTimeout(r, ms));
const version = await (await fetch(CDP + '/json/version')).json();
const ws = await connect(version.webSocketDebuggerUrl); wire(ws);
const { targetId } = await send(ws, 'Target.createTarget', { url: URL_ });
const { sessionId: sid } = await send(ws, 'Target.attachToTarget', { targetId, flatten: true });
await send(ws, 'Page.enable', {}, sid);
fs.mkdirSync(SHOTS, { recursive: true });
async function shot(name) { const r = await send(ws, 'Page.captureScreenshot', { format: 'png' }, sid); fs.writeFileSync(`${SHOTS}/${name}.png`, Buffer.from(r.data, 'base64')); console.log('  [shot]', name); }
await sleep(2500);

const boot = await ev(ws, sid, `(() => ({ cards: document.querySelectorAll('.card').length,
  dualBtn: !!document.getElementById('btnDual') }))()`);
console.log('boot:', JSON.stringify(boot));

// select the _F record card explicitly then toggle dual
await ev(ws, sid, `(() => { document.getElementById('btnDual').click(); return 1; })()`);
await sleep(1200);
const dual = await ev(ws, sid, `(() => ({
  panes: document.querySelectorAll('.dual-pane').length,
  videos: document.querySelectorAll('.dual-stage video').length,
  labels: Array.from(document.querySelectorAll('.dual-label')).map(e => e.textContent),
}))()`);
console.log('dual:', JSON.stringify(dual));

// sync check: seek the first video, confirm mate follows
const sync = await ev(ws, sid, `(async () => {
  const vs = Array.from(document.querySelectorAll('.dual-stage video'));
  if (vs.length < 2) return { skip: true };
  vs[0].currentTime = 3.0;
  await new Promise(r => setTimeout(r, 700));
  return { a: vs[0].currentTime, b: vs[1].currentTime, drift: Math.abs(vs[0].currentTime - vs[1].currentTime) };
})()`);
console.log('sync:', JSON.stringify(sync));
await shot('dual');
const problems = [];
if (!boot.dualBtn) problems.push('no #btnDual button');
if (dual.panes < 2 || dual.videos < 2) problems.push(`expected >=2 dual panes/videos, got ${dual.panes}/${dual.videos}`);
if (sync.skip) problems.push('fewer than two dual videos to sync');
else if (!(sync.drift < 0.5)) problems.push(`mate did not follow the seek (drift ${sync.drift})`);
if (problems.length) { console.error('FAIL:', problems.join('; ')); process.exit(1); }
console.log('PASS dual-channel sync');
process.exit(0);
