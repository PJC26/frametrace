// MANUAL harness (not in ci-harnesses.sh) - attaches to a running server; pass --i-know for the real :8477.
// Verify the tidied viewer top filter area. Prereqs: app on :8477 (case open), Edge on :9222.
const CDP = 'http://127.0.0.1:9222';
import { guardBase } from './lib/real-server-guard.mjs';
const BASE = guardBase('verify-topfilters', process.env.FT_BASE || 'http://127.0.0.1:8477'); // R4 F7: refuses the real :8477 without --i-know
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
const { targetId } = await send(ws, 'Target.createTarget', { url: BASE + '/review/evidence-viewer.html' });
const { sessionId: sid } = await send(ws, 'Target.attachToTarget', { targetId, flatten: true });
await sleep(2500);
const results = [];
const check = (n, ok, d = '') => { results.push(ok); console.log(`${ok ? 'PASS' : 'FAIL'}  ${n}${d ? '  — ' + d : ''}`); };

const top = await ev(ws, sid, `(() => ({
  chipCount: document.querySelectorAll('#presetChips [data-chip]').length,
  tagChips: document.querySelectorAll('#presetChips [data-chip^="tag:"]').length,
  groupKindInExtra: document.getElementById('filtersExtra').contains(document.getElementById('btnGroupKind')),
  tagSelExists: !!document.getElementById('tagFilter'),
  filterRows: (() => { const f = document.querySelector('.browse .filters'); return f.getBoundingClientRect().height; })(),
}))()`);
check('status chips only in chip row', top.tagChips === 0 && top.chipCount === 11, JSON.stringify(top));
check('group-kind toggle moved into extra panel', top.groupKindInExtra, '');
check('tag select exists', top.tagSelExists, '');

// tag filter via select
const tag = await ev(ws, sid, `(async () => {
  const sel = document.getElementById('tagFilter');
  sel.value = 'tag:사고'; sel.dispatchEvent(new Event('change'));
  await new Promise(r => setTimeout(r, 400));
  const after = { chip: state.chip, shown: document.getElementById('resultCount').textContent };
  sel.value = ''; sel.dispatchEvent(new Event('change'));
  await new Promise(r => setTimeout(r, 300));
  return { after, reset: state.chip };
})()`);
check('tag select filters records', tag.after.chip === 'tag:사고' && tag.reset === '', JSON.stringify(tag));

// extra panel toggle still works
const extra = await ev(ws, sid, `(async () => {
  const p = document.getElementById('filtersExtra');
  const was = p.hidden;
  document.getElementById('btnMoreFilters').click();
  await new Promise(r => setTimeout(r, 200));
  return { was, now: p.hidden };
})()`);
check('정렬·표시 panel toggles', extra.was === true && extra.now === false, JSON.stringify(extra));

const r = await send(ws, 'Page.captureScreenshot', { format: 'png' }, sid);
fs.mkdirSync('shots-user', { recursive: true });
fs.writeFileSync('shots-user/60-topfilters.png', Buffer.from(r.data, 'base64'));
await ev(ws, sid, `(() => { document.getElementById('btnMoreFilters').click(); return 1; })()`);
await sleep(300);
const r2 = await send(ws, 'Page.captureScreenshot', { format: 'png' }, sid);
fs.writeFileSync('shots-user/61-topfilters-open.png', Buffer.from(r2.data, 'base64'));
console.log(`\n${results.filter(Boolean).length}/${results.length} passed`);
ws.close(); process.exit(results.every(Boolean) ? 0 : 1);
