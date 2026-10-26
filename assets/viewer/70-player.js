// --- media transport: skip / playback rate / popup player ---

const RATES = [0.25, 0.5, 0.75, 1, 1.25, 1.5, 2, 4];

function currentVideo() {
  return els.mediaStage.querySelector("video");
}

// A <video> element can exist (selected record) with no loaded source or a
// failed load; transport keys must treat that as "no video".
function playableVideo() {
  const video = currentVideo();
  if (!video) return null;
  if (video.error || !(video.currentSrc || video.getAttribute("src"))) return null;
  return video;
}

function skipVideo(delta) {
  const video = currentVideo();
  if (!video) return;
  video.currentTime = Math.max(0, Math.min(video.duration || 1e9, video.currentTime + delta));
}

// Space in both the main viewer and the popup player (UX-08).
function togglePlayback() {
  const video = playableVideo();
  if (!video) { toast(t("toast.noVideo")); return; }
  // Space always returns to the rate chip's speed: a forward shuttle's 2x/4x
  // must not linger after pause (stopShuttle restores state.layout.rate).
  if (shuttle.dir !== 0) stopShuttle();
  if (video.paused) video.play().catch(() => {});
  else video.pause();
}

// --- frame step + J/K/L shuttle (Shift+J / Shift+K / Shift+L, "," / ".") ---
const SHUTTLE_SPEEDS = [1, 2, 4];
const shuttle = { dir: 0, level: 0, timer: null, video: null };

function frameDuration() {
  const rec = typeof selectedRecord === "function" ? selectedRecord() : null;
  const fps = Number(rec?.fps || rec?.frameRate || rec?.frame_rate);
  return Number.isFinite(fps) && fps >= 1 && fps <= 1000 ? 1 / fps : 1 / 30;
}

function stopShuttle() {
  // Forward shuttle borrowed playbackRate; give it back so the rate chip
  // (state.layout.rate) and the real rate stay consistent.
  if (shuttle.dir > 0 && shuttle.video) shuttle.video.playbackRate = Number(state.layout.rate) || 1;
  if (shuttle.timer) clearInterval(shuttle.timer);
  shuttle.timer = null;
  shuttle.dir = 0;
  shuttle.level = 0;
  shuttle.video = null;
}

function stepFrame(dir) {
  const video = playableVideo();
  if (!video) { toast(t("toast.noVideo")); return; }
  stopShuttle();
  if (!video.paused) video.pause();
  const max = Number.isFinite(video.duration) ? video.duration : 1e9;
  video.currentTime = Math.max(0, Math.min(max, video.currentTime + dir * frameDuration()));
}

function shuttleKey(dir) {
  const video = playableVideo();
  if (!video) { toast(t("toast.noVideo")); return; }
  if (dir === 0) {
    stopShuttle();
    video.pause();
    video.playbackRate = Number(state.layout.rate) || 1;
    toast(t("toast.shuttlePause"));
    return;
  }
  const sameDir = shuttle.dir === dir && shuttle.video === video && (dir < 0 || !video.paused);
  const level = sameDir ? Math.min(SHUTTLE_SPEEDS.length - 1, shuttle.level + 1) : 0;
  stopShuttle();
  shuttle.dir = dir;
  shuttle.level = level;
  shuttle.video = video;
  const speed = SHUTTLE_SPEEDS[level];
  if (dir > 0) {
    video.playbackRate = speed;
    video.play().catch(() => {});
  } else {
    // Negative playbackRate is not portable: emulate by stepping back.
    video.pause();
    shuttle.timer = setInterval(() => {
      if (currentVideo() !== video || video.currentTime <= 0) { stopShuttle(); return; }
      video.currentTime = Math.max(0, video.currentTime - speed * 0.1);
    }, 100);
  }
  toast(tf("toast.shuttle", { dir: dir > 0 ? "▶" : "◀", speed }));
}

function setRate(rate) {
  state.layout.rate = rate;
  saveLayout();
  els.playRate.value = String(rate);
  const video = currentVideo();
  if (video) video.playbackRate = rate;
}

function rateStep(delta) {
  const index = RATES.indexOf(Number(state.layout.rate) || 1);
  const next = RATES[Math.max(0, Math.min(RATES.length - 1, (index < 0 ? 3 : index) + delta))];
  setRate(next);
}

// Descriptor handed to the popup player so it can render the record the
// main window considers active — the popup never owns review state.
function playerDescriptor() {
  const record = selectedRecord();
  if (!record) return null;
  return {
    id: record.id,
    name: record.originalName || record.name || record.id,
    status: record.status,
    statusLabel: statusLabel(record.status),
    src: mediaSrcFor(record),
    mark: state.marks[record.id]?.status || "",
    tags: tagListFor(record),
    warnings: record.warnings || [],
    index: indexOfFiltered(record.id) + 1,
    total: filteredRecords().length,
  };
}

// Same-origin popup windows drive review through this bridge — marks and
// tags flow through the same state/localStorage path as the main viewer,
// so popup actions are identical to clicking the buttons there. The popup
// looks window.opener.__ftBridge up on every call, so after a hosted
// destroy()/init() it talks to the new mount; a destroyed mount clears the
// reference (00-host.js beforeDestroy) and never writes again — the main
// window stays the single writer (docs/ui-rebuild-contract.md §3.3).
const viewerBridge = {
  descriptor: () => playerDescriptor(),
  focus(id) {
    if (state.activeId !== id && records.some(record => record.id === id)) {
      state.activeId = id;
      render();
    }
    return playerDescriptor();
  },
  navigate(step) {
    const before = state.activeId;
    moveActive(step);
    return { moved: state.activeId !== before, descriptor: playerDescriptor() };
  },
  // markActive applies the mark and advances — the popup treats the
  // returned descriptor as the next clip to play.
  // R6 U6: an edit the main viewer refused (read-only: the review records
  // could not be read; nothing to undo) comes back as `refused` (the
  // reason) on the descriptor, so the pop-out never claims "실행 취소됨"
  // or a mark that did not happen.
  mark(status) {
    if (refuseLockedEdit()) return bridgeRefused("locked");
    markActive(status);
    return playerDescriptor();
  },
  toggleTag(tag) {
    if (refuseLockedEdit()) return bridgeRefused("locked");
    const id = state.activeId;
    if (id) {
      journaled(tf("journal.tag", { tag }), [id], () => toggleTag(id, tag));
      render();
    }
    return playerDescriptor();
  },
  // Ctrl+Z / Ctrl+Y in the popup share the main viewer's journal.
  undo() {
    return undoJournal() ? playerDescriptor() : bridgeRefused(annotationsLocked() ? "locked" : "undo");
  },
  redo() {
    return redoJournal() ? playerDescriptor() : bridgeRefused(annotationsLocked() ? "locked" : "redo");
  },
  // R6 U7: the pop-out asks first whether this window still answers.
  ping: () => ({ alive: true }),
  // R5 U8: the pop-out's frame capture goes through the viewer's common
  // POST door (viewerPost) like every other request: a 401 reads as the
  // authorization refusal (and reaches the host banner), not "offline".
  // Resolves { ok, path?, error?, auth? } — never rejects.
  // R6 U7: `arg.rid` (the pop-out's request id) makes a resend of the same
  // capture (its reply was late) answer the first request's outcome —
  // one frame, one file. Kept per window, so it outlives a remount.
  capture(arg) {
    const rid = arg && typeof arg === "object" && typeof arg.rid === "string" ? arg.rid.slice(0, 80) : "";
    const seen = window.__ftCaptureRids || (window.__ftCaptureRids = new Map());
    if (rid && seen.has(rid)) return seen.get(rid);
    const run = (async () => {
      try {
        const body = arg && typeof arg === "object" ? { id: String(arg.id || "unknown"), image: String(arg.image || ""), time: String(arg.time || "") } : {};
        const data = await viewerPost("/api/capture-frame", body);
        return data && typeof data === "object" ? data : { ok: false, error: "?" };
      } catch (err) {
        return err && err.ftAuth ? { ok: false, auth: true, error: err.message } : { ok: false, offline: true, error: t("player.capOffline") };
      }
    })();
    if (rid) {
      seen.set(rid, run);
      while (seen.size > 32) seen.delete(seen.keys().next().value);
    }
    return run;
  },
  // Filtered list for the popup's quick-jump sidebar — mirrors the main
  // grid order so popup position always matches what the examiner sees.
  list() {
    return filteredRecords().map((r, i) => ({
      id: r.id,
      name: r.originalName || r.name || r.id,
      index: i + 1,
      mark: state.marks[r.id]?.status || "",
      markLabel: state.marks[r.id]?.status ? markLabel(state.marks[r.id].status) : "",
      hasVideo: !!mediaSrcFor(r),
    }));
  },
};
// The descriptor of the active record plus why the bridge did nothing.
function bridgeRefused(why) {
  const d = playerDescriptor();
  const reason = why === "locked" ? t(lockKey("player.refusedReadonly")) : why === "undo" ? t("toast.nothingUndo") : why === "redo" ? t("toast.nothingRedo") : String(why || "");
  return d ? { ...d, refused: reason } : { refused: reason, none: true };
}
window.__ftBridge = viewerBridge;

// BroadcastChannel mirror of __ftBridge: a popup bound to this case keeps
// working even when window.opener was severed (browser popup policies,
// parent navigation), because the channel is name-addressed rather than
// object-referenced. Requests carry a correlation seq; replies echo it.
const playerChannel = ("BroadcastChannel" in window)
  ? new BroadcastChannel(`ft-player:${CASE_KEY}`)
  : null;
if (playerChannel) {
  playerChannel.addEventListener("message", (ev) => {
    const m = ev.data || {};
    // Single writer: only the window that opened the pop-out (its token is
    // baked into the popup) answers; other tabs on the case stay silent.
    if (m.token !== FTViewer.writerToken) return;
    const reply = (payload) => playerChannel.postMessage({ seq: m.seq, payload });
    if (m.type === "descriptor") reply(playerDescriptor());
    else if (m.type === "focus") reply(viewerBridge.focus(m.arg));
    else if (m.type === "navigate") reply(viewerBridge.navigate(m.arg));
    else if (m.type === "mark") reply(viewerBridge.mark(m.arg));
    else if (m.type === "toggleTag") reply(viewerBridge.toggleTag(m.arg));
    else if (m.type === "list") reply(viewerBridge.list());
    else if (m.type === "undo") reply(viewerBridge.undo());
    else if (m.type === "redo") reply(viewerBridge.redo());
    else if (m.type === "ping") reply(viewerBridge.ping());
    else if (m.type === "capture") viewerBridge.capture(m.arg).then(reply);
  }, { signal: ftSignal });
  // One channel per mount: closing it on destroy() means a stale mount can
  // never answer (or write) for the popup.
  ftSignal.addEventListener("abort", () => { try { playerChannel.close(); } catch (e) { /* closed */ } }, { once: true });
}

// A minimal dedicated player window — the examiner can park the clip on
// a second monitor and keep triaging: marks, tags, and next/previous
// navigation all route back through __ftBridge.
// JSON literal that is safe inside an inline <script> written via
// document.write: `<` (so a closing script tag or comment opener cannot break out) and the
// JS line separators are emitted as \u escapes (SEC-03).
function jsLit(value) {
  return JSON.stringify(value === undefined ? null : value)
    .replace(/</g, "\\u003c")
    .replace(/\u2028/g, "\\u2028")
    .replace(/\u2029/g, "\\u2029");
}

function openPlayerWindow() {
  const record = selectedRecord();
  if (!record) { toast(t("toast.noRecord")); return null; }
  const win = window.open("", "frametrace-player", "popup=yes,width=1100,height=880");
  if (!win) { toast(t("toast.popupBlocked")); return null; }
  const e = escapeHtml;
  // Only a listed speed reaches the inline script (U10): a stored layout
  // value is never written into the popup's code as a literal.
  const rate = RATES.includes(Number(state.layout.rate)) ? Number(state.layout.rate) : 1;
  const rateOptions = RATES.map(r => `<option value="${r}"${r === rate ? " selected" : ""}>${r}×</option>`).join("");
  const tagButtons = tagPresets().map(tag => `<button class="tg" data-tag="${e(tag)}">${e(tag)}</button>`).join("");
  win.document.open();
  win.document.write(`<!doctype html><html lang="${state.locale}"><head><meta charset="utf-8">
<title>${e(record.name || record.id)} — FrameTrace Player</title>
<style>
body{margin:0;background:#101815;color:#dfe8e4;font-family:ui-sans-serif,system-ui,"Segoe UI",sans-serif;display:flex;flex-direction:column;height:100vh}
header{padding:8px 14px;font-size:13px;display:flex;gap:10px;align-items:center;border-bottom:1px solid #24332e;flex-wrap:wrap}
header .id{color:#8fa79e;font-family:monospace;font-size:11px}
#wrap{flex:1;min-height:0;display:flex}
#left{flex:1;min-width:0;display:flex;flex-direction:column}
#stage{flex:1;min-height:0;display:grid;place-items:center;background:#000;position:relative}
video{width:100%;height:100%;object-fit:contain}
#novid{position:absolute;color:#8fa79e;font-size:13px;background:rgba(0,0,0,.6);padding:8px 14px;border-radius:6px}
#list{width:230px;flex:0 0 auto;overflow-y:auto;border-left:1px solid #24332e;font-size:12px}
.li{padding:6px 10px;cursor:pointer;border-bottom:1px solid #1c2825;overflow:hidden;text-overflow:ellipsis;white-space:nowrap}
.li:hover{background:#1b2b26}
.li:focus-visible{outline:2px solid #3fa08e;outline-offset:-2px}
.li.on{background:#3fa08e;color:#08130f;font-weight:700}
.li.novid{color:#5a6b64}
.li .idx{color:#8fa79e;margin-right:4px;font-family:monospace;font-size:11px}
.li.on .idx{color:#0a1c18}
.li .mk{float:right;color:#d9b45b}
.li.on .mk{color:#08130f}
.bar{display:flex;gap:8px;align-items:center;padding:6px 14px;border-top:1px solid #24332e;font-size:13px;flex-wrap:wrap}
button,select{font:inherit;height:28px;border:1px solid #3a4f48;border-radius:5px;background:#1b2b26;color:#dfe8e4;padding:0 10px;cursor:pointer;font-size:12px}
button:hover,select:hover{border-color:#3fa08e}
button.on{background:#3fa08e;border-color:#3fa08e;color:#08130f}
.muted{color:#8fa79e;font-size:12px}
#note{color:#d9b45b;font-size:12px}
</style></head><body>
<header><strong id="title"></strong><span class="id" id="meta"></span><span id="note"></span></header>
<div id="wrap"><div id="left">
<div id="stage"><video id="vv" controls autoplay></video><div id="novid" hidden>${t("player.novid")}</div></div>
<div class="bar">
<button id="prev">${t("player.prev")}</button>
<button id="b10">« -10s</button><button id="b1">-1s</button>
<button id="f1">+1s</button><button id="f10">+10s »</button>
<select id="rate">${rateOptions}</select>
<button id="play">${t("player.play")}</button>
<button id="next">${t("player.next")}</button>
<button id="cap" title="${t("player.capTitle")}">${t("player.cap")}</button>
<span class="muted">${t("player.hint")}</span>
</div>
<div class="bar">
<span class="muted">${t("player.mark")}</span>
<button class="mk" data-m="reviewed">${t("player.reviewed")}</button>
<button class="mk" data-m="important">${t("player.important")}</button>
<button class="mk" data-m="needs_verification">${t("player.verify")}</button>
<button class="mk" data-m="">${t("player.clear")}</button>
<span class="muted">${t("player.tags")}</span>${tagButtons}
</div>
</div><div id="list" role="listbox" aria-label="${t("player.list")}"></div></div>
<script>
var v=document.getElementById("vv");
var RATES=[${RATES.join(",")}];
var rate=document.getElementById("rate");
var cur=null;
v.playbackRate=${jsLit(rate)};
var TOKEN=${jsLit(FTViewer.writerToken)};var seqN=0,pend={},chan=("BroadcastChannel" in window)?new BroadcastChannel(${jsLit(`ft-player:${CASE_KEY}`)}):null;
if(chan){chan.onmessage=function(ev){var m=ev.data||{},f=m.seq&&pend[m.seq];if(f){delete pend[m.seq];f(m.payload);}};}
function call(op,arg,cb,ms){
var b=(window.opener&&!window.opener.closed&&window.opener.__ftBridge)||null;
if(b){var done=false,fin=function(x){if(done)return;done=true;cb(x);};
try{var r=b[op](arg);if(r&&typeof r.then==="function"){r.then(fin,function(){fin(undefined);});setTimeout(function(){fin(undefined);},ms||3000);}else fin(r);}catch(e){fin(undefined);}return;}
if(!chan){cb(undefined);return;}
var s=++seqN;pend[s]=cb;
chan.postMessage({type:op,seq:s,arg:arg,token:TOKEN});
setTimeout(function(){if(pend[s]){delete pend[s];cb(undefined);}},ms||3000);
}
function esc(s){return String(s==null?"":s).replace(/[&<>"']/g,function(c){return{"&":"&amp;","<":"&lt;",">":"&gt;",'"':"&quot;","'":"&#39;"}[c]});}
function note(t){document.getElementById("note").textContent=t||"";}
function dead(){note(${jsLit(t("player.dead"))});}
function applyDesc(d,autoplay){
if(!d){document.getElementById("title").textContent=${jsLit(t("player.none"))};return;}
cur=d;
document.getElementById("title").textContent=d.name;
document.getElementById("meta").textContent=d.id+" · "+d.statusLabel+" · "+(d.index||"-")+"/"+d.total+(d.mark?" · "+${jsLit(t("player.mark"))}+":"+d.mark:"")+(d.warnings.length?" · "+${jsLit(t("player.warnShort"))}+" "+d.warnings.length:"");
document.title=d.name+" — FrameTrace Player";
if(d.src){document.getElementById("novid").hidden=true;if(v.getAttribute("src")!==d.src){v.setAttribute("src",d.src);v.load();}v.playbackRate=Number(rate.value);if(autoplay!==false)v.play().catch(function(){});}
else{document.getElementById("novid").hidden=false;v.removeAttribute("src");v.load();}
document.querySelectorAll(".mk").forEach(function(b){b.classList.toggle("on",b.dataset.m===d.mark);});
document.querySelectorAll(".tg").forEach(function(b){b.classList.toggle("on",d.tags.indexOf(b.dataset.tag)>=0);});
renderList();
}
var listEl=document.getElementById("list");
function renderList(){
call("list",null,function(items){
if(!items){listEl.innerHTML="";return;}
listEl.innerHTML=items.map(function(it){
return '<div class="li'+(it.id===(cur&&cur.id)?" on":"")+(it.hasVideo?"":" novid")+'" data-id="'+esc(it.id)+'" tabindex="0" role="option" aria-selected="'+(it.id===(cur&&cur.id))+'"><span class="idx">'+it.index+'</span>'+esc(it.name)+(it.markLabel?'<span class="mk">'+esc(it.markLabel)+'</span>':'')+'</div>';
}).join("");
var on=listEl.querySelector(".li.on");if(on)on.scrollIntoView({block:"nearest"});
});
}
listEl.addEventListener("click",function(e){var it=e.target.closest(".li");if(!it)return;call("focus",it.dataset.id,function(d){if(d===undefined){dead();return;}applyDesc(d,true);});});
listEl.addEventListener("keydown",function(e){if((e.key!=="Enter"&&e.key!==" ")||e.isComposing||e.keyCode===229)return;var it=e.target.closest(".li");if(!it)return;e.preventDefault();call("focus",it.dataset.id,function(d){if(d===undefined){dead();return;}applyDesc(d,true);});});
function nav(step,autoplay){call("navigate",step,function(r){if(!r){dead();return;}applyDesc(r.descriptor,autoplay);if(!r.moved)note(step>0?${jsLit(t("player.last"))}:${jsLit(t("player.first"))});else note("");});}
function refused(d){if(!d||!d.refused)return false;if(!d.none)applyDesc(d,false);note(d.refused);return true;}
function doMark(m){if(!cur)return;call("focus",cur.id,function(f){if(f===undefined){dead();return;}call("mark",m||null,function(d){if(d===undefined){dead();return;}if(refused(d))return;applyDesc(d,true);});});}
function doHist(op){call(op,null,function(d){if(d===undefined){dead();return;}if(refused(d))return;applyDesc(d,false);note(op==="undo"?${jsLit(t("player.undone"))}:${jsLit(t("player.redone"))});});}
function doTag(t){if(!cur)return;call("focus",cur.id,function(f){if(f===undefined){dead();return;}call("toggleTag",t,function(d){if(d===undefined){dead();return;}if(refused(d))return;applyDesc(d,false);});});}
function skip(d){v.currentTime=Math.max(0,Math.min(v.duration||1e9,v.currentTime+d));}
function stepRate(d){var i=RATES.indexOf(Number(rate.value));var n=RATES[Math.max(0,Math.min(RATES.length-1,(i<0?3:i)+d))];rate.value=String(n);v.playbackRate=n;}
document.getElementById("b10").onclick=function(){skip(-10)};
document.getElementById("b1").onclick=function(){skip(-1)};
document.getElementById("f1").onclick=function(){skip(1)};
document.getElementById("f10").onclick=function(){skip(10)};
document.getElementById("prev").onclick=function(){nav(-1,true)};
document.getElementById("next").onclick=function(){nav(1,true)};
rate.onchange=function(){v.playbackRate=Number(rate.value)};
document.getElementById("play").onclick=function(){v.paused?v.play():v.pause()};
document.querySelectorAll(".mk").forEach(function(b){b.onclick=function(){doMark(b.dataset.m)}});
document.querySelectorAll(".tg").forEach(function(b){b.onclick=function(){doTag(b.dataset.tag)}});
document.getElementById("cap").onclick=function(){
if(!v.videoWidth){note(${jsLit(t("player.noVideo"))});return;}
var c=document.createElement("canvas");c.width=v.videoWidth;c.height=v.videoHeight;
c.getContext("2d").drawImage(v,0,0);
var du=c.toDataURL("image/jpeg",0.92);
var arg={id:cur?cur.id:"unknown",image:du.slice(du.indexOf(",")+1),time:v.currentTime.toFixed(2),rid:TOKEN+"-"+Date.now().toString(36)+"-"+(++capN)};
capture(arg);
};
// R7 U6: the opener path (a direct __ftBridge call) has the same time
// limit as the channel path — a capture promise that never settles no
// longer leaves the pop-out waiting forever — and a viewer that still
// answers the ping but never sent the capture result says so (capSlow),
// not "the main viewer closed" (capUnsure: the ping got no answer).
// R6 U7: the capture goes through the main viewer when it still answers
// (a short ping first); only a viewer that is gone means the direct POST —
// at once, not after a 30 s wait. A capture reply that is late is asked
// for again with the SAME request id (the viewer answers the first
// request's outcome: one frame, one file); if the viewer died meanwhile
// the frame may or may not be saved — said so, never posted a second time.
var capN=0,CAP_MS=30000,PING_MS=1500;
function shown(d){note(d.ok?${jsLit(t("player.savedPfx"))}+" "+d.path:d.auth?String(d.error||${jsLit(t("net.authRefused"))}):${jsLit(t("player.capFailPfx"))}+" "+(d.error||""));}
function alive(cb){call("ping",null,function(d){cb(!!(d&&d.alive));},PING_MS);}
function capDirect(arg){var body={id:arg.id,image:arg.image,time:arg.time};
fetch("/api/capture-frame",{method:"POST",headers:{"Content-Type":"text/plain"},body:JSON.stringify(body)}).then(function(r){if(r.status===401)return{ok:false,auth:true,error:${jsLit(t("net.authRefused"))}};return r.json().catch(function(){return{ok:false,error:"HTTP "+r.status};});}).then(shown).catch(function(){note(${jsLit(t("player.capOffline"))})});}
function capVia(arg,tries){call("capture",arg,function(d){if(d){shown(d);return;}alive(function(ok){if(ok&&tries<4)capVia(arg,tries+1);else note(ok?${jsLit(t("player.capSlow"))}:${jsLit(t("player.capUnsure"))});});},CAP_MS);}
function capture(arg){alive(function(ok){if(ok)capVia(arg,0);else capDirect(arg);});}
v.addEventListener("ended",function(){nav(1,true);});
var NAV={ArrowUp:1,ArrowDown:1,ArrowLeft:1,ArrowRight:1,Home:1,End:1,PageUp:1,PageDown:1," ":1,Enter:1,Tab:1};
var NLS=new RegExp(${jsLit(NON_LATIN_SCRIPT_RE.source)},"u");
function skey(e){var k=String(e.key||""),c=String(e.code||""),m;
if(k==="Process"||k==="Unidentified"||NLS.test(k)){
if((m=/^Key([A-Z])$/.exec(c)))k=m[1].toLowerCase();else if((m=/^(?:Digit|Numpad)([0-9])$/.exec(c))&&!e.shiftKey)k=m[1];}
if(/^[a-z]$/i.test(k))k=e.shiftKey?k.toUpperCase():k.toLowerCase();return k;}
document.addEventListener("keydown",function(e){
var tn=e.target&&e.target.tagName;
if(tn==="SELECT"&&NAV[e.key])return;
var ag=e.getModifierState&&e.getModifierState("AltGraph");
var K=skey(e);
if(!ag&&(e.ctrlKey||e.metaKey)){var k=K.toLowerCase();
if(k==="z"&&!e.shiftKey){e.preventDefault();doHist("undo");}
else if(k==="y"||(k==="z"&&e.shiftKey)){e.preventDefault();doHist("redo");}
return;}
if(!ag&&e.altKey)return;
if(tn==="SELECT")e.preventDefault();
if(e.target&&e.target.closest&&e.target.closest("#list")){
if(e.key==="ArrowDown"||e.key==="ArrowUp"){e.preventDefault();var items=listEl.querySelectorAll(".li");var idx=Array.prototype.indexOf.call(items,e.target);var n=items[idx+(e.key==="ArrowDown"?1:-1)];if(n)n.focus();}
return;}
if(e.key==="ArrowLeft")skip(-10);else if(e.key==="ArrowRight")skip(10);
else if(e.key==="ArrowUp"){e.preventDefault();stepRate(1);}
else if(e.key==="ArrowDown"){e.preventDefault();stepRate(-1);}
else if(e.key===" "){e.preventDefault();v.paused?v.play():v.pause();}
else if(K==="j")nav(1,true);else if(K==="k")nav(-1,true);
else if(K==="1")doMark("reviewed");else if(K==="2")doMark("important");
else if(K==="3")doMark("needs_verification");else if(K==="0")doMark("");});
call("descriptor",null,function(d){if(d===undefined){dead();}else{applyDesc(d,true);}});
<\/script></body></html>`);
  win.document.close();
  win.focus();
  FTViewer._trackPopup(win);
  return win;
}


// CapsLock flips event.key's case: Caps+j arrives as "J" without Shift and
// Shift+j with CapsLock as "j". The global handler (80-review.js) reads keys
// through shortcutKey(), where a letter's case follows Shift alone — so the
// J/K/L shuttle needs a real Shift and Caps+j still navigates like j.
