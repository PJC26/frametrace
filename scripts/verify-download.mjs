// Verify single + multi file download from the viewer.
// MANUAL harness - not in scripts/ci-harnesses.sh (T13): needs headless Edge on
// :9222, a workstation (FT_BASE, default :8477 - refused without --i-know, R4 F7)
// with a case open, and writes to C:/Temp.
// Note: headless Edge cancels every download (even blob:) at the download
// manager, so we assert on Page.downloadWillBegin — the browser observed
// the request, resolved the filename, and started a download. Byte-level
// correctness of /media?download=1 is verified separately with curl.
const CDP = 'http://127.0.0.1:9222';
import { guardBase } from './lib/real-server-guard.mjs';
const BASE = guardBase('verify-download', process.env.FT_BASE || 'http://127.0.0.1:8477'); // R4 F7: refuses the real :8477 without --i-know
const fs = await import('fs');
const DL = 'C:/Temp/ft-dl-test';
fs.mkdirSync(DL, { recursive: true });
let nextId = 1;
const pending = new Map();
const begun = [];
function connect(url) { return new Promise((res, rej) => { const w = new WebSocket(url); w.onopen = () => res(w); w.onerror = rej; }); }
function send(ws, method, params = {}, sessionId) {
  const id = nextId++;
  return new Promise((resolve, reject) => { pending.set(id, { resolve, reject }); ws.send(JSON.stringify({ id, method, params, sessionId })); });
}
function wire(ws) { ws.onmessage = e => { const m = JSON.parse(e.data);
  if (m.method === 'Page.downloadWillBegin') begun.push(m.params.url);
  if (m.id && pending.has(m.id)) { const { resolve, reject } = pending.get(m.id); pending.delete(m.id); m.error ? reject(new Error(m.error.message)) : resolve(m.result); } }; }
async function ev(ws, sid, expr) {
  const r = await send(ws, 'Runtime.evaluate', { expression: expr, returnByValue: true, awaitPromise: true, userGesture: true }, sid);
  if (r.exceptionDetails) throw new Error(JSON.stringify(r.exceptionDetails.exception?.description || r.exceptionDetails.text));
  return r.result.value;
}
const sleep = ms => new Promise(r => setTimeout(r, ms));
const version = await (await fetch(CDP + '/json/version')).json();
const ws = await connect(version.webSocketDebuggerUrl); wire(ws);
const { targetId } = await send(ws, 'Target.createTarget', { url: BASE + '/review/evidence-viewer.html' });
const { sessionId: sid } = await send(ws, 'Target.attachToTarget', { targetId, flatten: true });
await send(ws, 'Page.enable', {}, sid);
await send(ws, 'Page.setDownloadBehavior', { behavior: 'allow', downloadPath: DL }, sid);
await sleep(2500);
const results = [];
const check = (n, ok, d = '') => { results.push(ok); console.log(`${ok ? 'PASS' : 'FAIL'}  ${n}${d ? '  — ' + d : ''}`); };

// select a card, click ⤓ 저장
await ev(ws, sid, `(() => { const c = document.querySelector('#recordGrid .card'); c.click(); return 1; })()`);
await sleep(800);
await ev(ws, sid, `document.getElementById('btnDownloadFile').click()`);
await sleep(2500);
check('single download begins with correct /media URL',
  begun.some(u => u.includes('/media?path=') && u.includes('download=1')),
  begun[0] ? decodeURIComponent(begun[0]).slice(-80) : 'none');

// multi: tick two card checkboxes (the real selection UI), then the menu item
begun.length = 0;
const selCount = await ev(ws, sid, `(async () => {
  // re-query each time — every selection re-renders the grid
  for (let i = 0; i < 2; i++) {
    const box = [...document.querySelectorAll('#recordGrid input[data-check]')].find(b => !b.checked);
    if (!box) break;
    box.checked = true;
    box.dispatchEvent(new Event('change', { bubbles: true }));
    await new Promise(r => setTimeout(r, 300));
  }
  const count = state.selectedIds.size;
  document.getElementById('btnDownloadFiles').click();
  return count;
})()`);
await sleep(4000);
check('multi download fires one request per selected record', selCount >= 2 && begun.length >= 2, `selected=${selCount} begun=${begun.length}`);
console.log(`\n${results.filter(Boolean).length}/${results.length} passed`);
ws.close(); process.exit(results.every(Boolean) ? 0 : 1);
