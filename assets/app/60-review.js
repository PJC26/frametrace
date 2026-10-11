// @interim (legacy viewer frame) — until the viewer fragment is hosted in
// this document (T5: the "@include:viewer.body" marker + window.FTViewer), the
// review page runs in an <iframe> inside #hViewerHost. Everything that
// touches that frame lives in this file, behind hLegacyViewer (the same
// surface as FTViewer, docs/ui-rebuild-contract.md §3), and is inert once
// FTViewer exists. Deleting this file is the whole cleanup.
let reviewFrame = null;
let reviewFrameLoaded = false;
function hEnsureFrame() {
  if (reviewFrame) return reviewFrame;
  const host = $("hViewerHost");
  let fr = $("reviewFrame");
  if (!fr && host && typeof host.insertAdjacentHTML === "function") {
    host.insertAdjacentHTML("beforeend",
      `<iframe class="h-frame" id="reviewFrame" title="증거 뷰어" allow="fullscreen"></iframe>` +
      `<div class="h-framefail" id="frameFail" role="alert" hidden><b id="frameFailTitle">뷰어를 불러오지 못했습니다</b>` +
      `<span id="frameFailMsg"></span><span class="h-row"><button type="button" id="btnFrameWindow">새 탭으로 열기</button>` +
      `<button type="button" class="primary" id="btnFrameRetry">다시 시도</button></span></div>`);
    fr = $("reviewFrame");
  }
  if (!fr) return null;
  reviewFrame = fr;
  if (host && host.classList) host.classList.add("h-legacy");
  fr.addEventListener("load", onReviewFrameLoad);
  $("btnFrameRetry").addEventListener("click", () => {
    if (!frameUrl) return;
    if (frameState !== "failed") { showFrameFail(null); return; }
    loadReviewFrame(frameUrl);
  });
  $("btnFrameWindow").addEventListener("click", () => {
    const win = window.open(standaloneViewerUrl(), "_blank");
    if (!win) toast("팝업이 차단되었습니다 — 브라우저에서 이 사이트의 팝업을 허용하십시오.", "warn");
  });
  return fr;
}

// ---- UX-18: frame load watchdog + visible retry ----
// "loading" until the frame's load event shows a real viewer document;
// "failed" after FRAME_TIMEOUT_MS without one, or when it loaded an error
// page (404 text has no <script>). Retrying a failed frame is safe — a
// viewer that never came up holds no marks.
const FRAME_TIMEOUT_MS = 20000;
let frameState = "idle", frameUrl = "", frameAttempt = 0, frameTimer = 0;
function loadReviewFrame(url) {
  if (!hEnsureFrame()) return;
  frameUrl = url;
  frameAttempt += 1;
  frameState = "loading";
  showFrameFail(null);
  clearTimeout(frameTimer);
  frameTimer = setTimeout(frameWatchdog, FRAME_TIMEOUT_MS);
  // A retry needs a distinct URL or a cached failure may be replayed.
  reviewFrame.src = frameAttempt > 1 ? `${url}${url.includes("?") ? "&" : "?"}retry=${frameAttempt}` : url;
}
function frameDocState() {
  try {
    const doc = reviewFrame.contentDocument;
    if (!doc) return "unknown";
    const w = reviewFrame.contentWindow;
    if (w && typeof w.__frametraceSummary === "function") return "alive";
    if (doc.querySelector && doc.querySelector("script")) return "alive";
    return doc.readyState === "loading" ? "loading" : "empty";
  } catch (e) { return "unknown"; }
}
function frameWatchdog() {
  frameTimer = 0;
  if (frameState !== "loading" || framePopped) return;
  if (frameDocState() === "alive") { frameState = "ok"; return; }
  frameFailed(`${Math.round(FRAME_TIMEOUT_MS / 1000)}초 동안 뷰어가 응답하지 않았습니다 — 서버가 바쁘거나 연결이 끊겼을 수 있습니다.`);
}
function frameFailed(msg) {
  frameState = "failed";
  clearTimeout(frameTimer);
  frameTimer = 0;
  showFrameFail(msg);
}
function showFrameFail(msg) {
  const box = $("frameFail");
  if (!box) return;
  box.hidden = !msg;
  if (msg) $("frameFailMsg").textContent = msg;
}
function onReviewFrameLoad() {
  if (!reviewFrameLoaded || framePopped) return;
  const st = frameDocState();
  if (st === "empty") {
    frameFailed("뷰어 페이지가 비어 있거나 오류 응답입니다 — 검토 데이터가 아직 만들어지지 않았거나 서버 오류일 수 있습니다.");
    return;
  }
  clearTimeout(frameTimer);
  frameTimer = 0;
  frameState = "ok";
  showFrameFail(null);
  // Theme/locale are pushed until the viewer acks, so a viewer that wires
  // up late still follows the shell.
  framePushTheme(hThemeNow());
  framePushLocale(hLocalePref());
  syncLangBtn();
  bridgeFrameKeys();
}

// ---- theme / language into the frame ----
// postMessage ({type:"ft-theme"} / {type:"ft-locale"}) for a viewer that
// listens, plus a same-origin fallback (retag the document / drive the
// viewer's own hidden language button).
function framePost(msg) {
  try { if (reviewFrame && reviewFrame.contentWindow) { reviewFrame.contentWindow.postMessage(msg, "*"); return true; } } catch (e) {}
  return false;
}
function framePushTheme(theme) {
  if (!reviewFrame) return;
  framePost({ type: "ft-theme", theme });
  try { if (reviewFrame.contentDocument && reviewFrame.contentDocument.documentElement) reviewFrame.contentDocument.documentElement.dataset.theme = theme; } catch (e) {}
}
// The viewer sets <html lang> from its locale.
function frameLocale() {
  try {
    const lang = reviewFrame && reviewFrame.contentDocument && reviewFrame.contentDocument.documentElement ? reviewFrame.contentDocument.documentElement.lang : "";
    return lang === "en" || lang === "ko" ? lang : "";
  } catch (e) { return ""; }
}
// The viewer answers {type:"ft-locale-ack", locale}; until then (or until
// the frame reloads, which restarts the sequence) the shell re-posts.
let localeSeq = 0, localeAcked = "";
const LOCALE_RETRY_MS = 500, LOCALE_RETRY_MAX = 24;
window.addEventListener("message", e => {
  if (!e.data || e.data.type !== "ft-locale-ack") return;
  try { if (e.source && reviewFrame && e.source !== reviewFrame.contentWindow) return; } catch (err) {}
  localeAcked = e.data.locale || "";
});
function framePushLocale(locale) {
  if (!locale || !reviewFrame) return;
  const seq = ++localeSeq;
  localeAcked = "";
  let tries = 0;
  const tick = () => {
    if (seq !== localeSeq || localeAcked === locale) return;
    framePost({ type: "ft-locale", locale });
    if (++tries < LOCALE_RETRY_MAX) setTimeout(tick, LOCALE_RETRY_MS);
  };
  tick();
  try {
    const doc = reviewFrame.contentDocument;
    const cur = frameLocale();
    const btn = doc ? doc.getElementById("btnLang") : null;
    if (btn && cur && cur !== locale) btn.click();
  } catch (e) {}
}

// ---- viewer data refresh after a background job (UX-06) ----
// Never `src = src`: that navigation drops marks still being saved.
// 1) ask the viewer over postMessage ({type:"ft-reload"}); a viewer that
//    handles it answers {type:"ft-reload-ack"} (or reloads itself, which
//    fires the frame's load event); 2) otherwise reload it only when its
//    save indicator is clean; 3) else defer and let the examiner retry.
const VIEWER_RELOAD_ACK_MS = 1500;
function frameSaveState() {
  try {
    const doc = reviewFrameLoaded && reviewFrame && reviewFrame.contentDocument;
    const el = doc ? doc.getElementById("saveState") : null;
    return el && el.dataset ? (el.dataset.state || null) : null;
  } catch (e) { return null; }
}
function frameHasUnsaved() {
  const ss = frameSaveState();
  return ss === "pending" || ss === "saving" || ss === "failed" || ss === "queued";
}
function frameSave() {
  try {
    const doc = reviewFrameLoaded && reviewFrame && reviewFrame.contentDocument;
    const el = doc ? doc.getElementById("saveState") : null;
    if (el) { el.click(); return true; }
  } catch (e) {}
  return false;
}
function waitForViewerAck(ms) {
  return new Promise(resolve => {
    let done = false;
    const finish = v => {
      if (done) return;
      done = true;
      window.removeEventListener("message", onMsg);
      reviewFrame.removeEventListener("load", onLoad);
      resolve(v);
    };
    const onMsg = e => {
      let fromFrame = true;
      try { fromFrame = !e.source || e.source === reviewFrame.contentWindow; } catch (err) { fromFrame = true; }
      if (fromFrame && e.data && e.data.type === "ft-reload-ack") finish(e.data.reloaded === false ? "refused" : "ack");
    };
    const onLoad = () => finish("ack");
    window.addEventListener("message", onMsg);
    reviewFrame.addEventListener("load", onLoad);
    setTimeout(() => finish(null), ms);
  });
}
function waitForFrameSettled(ms) {
  return new Promise(resolve => {
    let t = 0, iv = 0;
    const done = () => { clearTimeout(t); clearInterval(iv); reviewFrame.removeEventListener("load", done); resolve(); };
    iv = setInterval(() => { if (frameState !== "loading") done(); }, 250);
    reviewFrame.addEventListener("load", done);
    t = setTimeout(done, ms);
  });
}
// "none" (nothing mounted — the next open loads fresh data), "posted"
// (the viewer took it), "reloaded", "deferred" (unsaved marks).
async function refreshViewerData(reason) {
  if (hViewer.hosted()) {
    const r = await hViewer.reload({ reason: reason || "" });
    return r === "refused" ? "deferred" : r === "none" ? "none" : "posted";
  }
  return frameRefresh(reason);
}
async function frameRefresh(reason) {
  if (!reviewFrameLoaded) return "none";
  // Popped out: the separate window owns the data; ask it to refresh.
  if (framePopped) {
    try { if (popoutWin && !popoutWin.closed) popoutWin.postMessage({ type: "ft-reload", reason: reason || "" }, "*"); } catch (e) {}
    return "none";
  }
  if (frameState === "failed") {
    loadReviewFrame(frameUrl);
    return "reloaded";
  }
  if (frameState === "loading") {
    await waitForFrameSettled(FRAME_TIMEOUT_MS + 1000);
    if (frameState === "failed") { loadReviewFrame(frameUrl); return "reloaded"; }
    if (frameState !== "ok") return "deferred";
  }
  const ack = waitForViewerAck(VIEWER_RELOAD_ACK_MS);
  framePost({ type: "ft-reload", reason: reason || "" });
  const got = await ack;
  if (got === "ack") return "posted";
  if (got === "refused") return "deferred";
  return guardedFrameReload();
}
function guardedFrameReload() {
  if (hViewer.hosted()) {
    if (hViewer.hasUnsaved()) { hViewer.save(); return "deferred"; }
    hViewer.reload({});
    return "reloaded";
  }
  if (frameHasUnsaved()) {
    frameSave(); // start saving now; the examiner retries after
    return "deferred";
  }
  try {
    reviewFrame.contentWindow.location.reload();
  } catch (e) {
    loadReviewFrame(frameUrl);
  }
  return "reloaded";
}

// ---- second monitor: one writer at a time ----
// The legacy frame cannot fold its own media column, so the whole viewer
// moves to the second window and the frame here is parked (about:blank)
// behind #hPopoutNote — marks are never written from two viewers at once.
// Parking is refused while this frame still has unsaved marks; returning
// is refused while the pop-out has unsaved marks. Closing the pop-out
// window brings the frame back automatically.
let popoutWin = null, popoutTimer = 0, framePopped = false;
function popoutSaveBadge() {
  try {
    const pdoc = popoutWin && !popoutWin.closed ? popoutWin.document : null;
    return pdoc ? pdoc.getElementById("saveState") : null;
  } catch (e) { return null; }
}
function popoutSaveState() {
  const el = popoutSaveBadge();
  return el && el.dataset ? (el.dataset.state || null) : null;
}
function setPopoutUi(on) {
  framePopped = on;
  if (reviewFrame) reviewFrame.hidden = on;
  const msg = $("hPopoutNoteMsg");
  if (msg && on) msg.textContent = "검토 화면 전체가 보조 창에 열려 있습니다. 마크·메모는 그 창 한 곳에서만 저장됩니다.";
  hSetPopped(on);
}
function openViewerPopout() {
  if (framePopped && popoutWin && !popoutWin.closed) { try { popoutWin.focus(); } catch (e) {} return popoutWin; }
  if (reviewFrameLoaded && frameState === "ok" && frameHasUnsaved()) {
    frameSave();
    toast("아직 케이스에 저장되지 않은 마크·메모가 있습니다 — 저장 상태가 ‘저장됨’이 된 뒤 다시 새 창으로 여십시오.", "warn");
    return null;
  }
  let win = window.open(standaloneViewerUrl(), "frametrace-viewer", "popup=yes,width=1500,height=950");
  if (!win) win = window.open(standaloneViewerUrl(), "_blank");
  if (!win) { toast("팝업이 차단되었습니다 — 브라우저에서 이 사이트의 팝업을 허용하십시오.", "warn"); return null; }
  try { win.focus(); } catch (e) {}
  popoutWin = win;
  if (reviewFrameLoaded) {
    clearTimeout(frameTimer);
    frameTimer = 0;
    showFrameFail(null);
    setPopoutUi(true);
    try { reviewFrame.src = "about:blank"; } catch (e) {}
  }
  clearInterval(popoutTimer);
  popoutTimer = setInterval(() => {
    let closed = true;
    try { closed = !popoutWin || popoutWin.closed; } catch (e) { closed = true; }
    if (closed) returnViewerHere(true);
  }, 1000);
  return win;
}
function returnViewerHere(windowClosed) {
  if (!windowClosed) {
    const ss = popoutSaveState();
    if (ss === "pending" || ss === "saving" || ss === "failed" || ss === "queued") {
      try { popoutSaveBadge().click(); } catch (e) {}
      toast("보조 창에 아직 저장되지 않은 마크·메모가 있습니다 — 그 창에서 ‘저장됨’을 확인한 뒤 다시 누르십시오.", "warn");
      return;
    }
    try { if (popoutWin && !popoutWin.closed) popoutWin.close(); } catch (e) {}
  }
  clearInterval(popoutTimer);
  popoutTimer = 0;
  popoutWin = null;
  if (!framePopped) return;
  setPopoutUi(false);
  if (frameUrl && reviewFrameLoaded) loadReviewFrame(frameUrl);
}

// Same-origin frame: the shell's Alt+P/J/R work with focus inside it, and
// a click inside closes any open shell popover.
let bridgedDoc = null;
function bridgeFrameKeys() {
  let doc = null;
  try { doc = reviewFrame.contentDocument; } catch (e) { doc = null; }
  if (!doc || doc === bridgedDoc || typeof doc.addEventListener !== "function") return;
  bridgedDoc = doc;
  doc.addEventListener("keydown", e => { hShellShortcut(e, doc.activeElement); });
  doc.addEventListener("mousedown", () => hClosePopovers(false));
}

hLegacyViewer = {
  mount(partial) {
    if (hViewer.hosted() || !hEnsureFrame()) return;
    reviewFrame.hidden = framePopped;
    if (!reviewFrameLoaded) {
      reviewFrameLoaded = true;
      // UX-02: before make-review has written review/ the server's
      // built-in /viewer/ page (data via /api/records?live=1) is the frame.
      loadReviewFrame(partial ? "/viewer/?embed=1" : "/review/evidence-viewer.html?embed=1");
    }
  },
  unmount() {
    if (!reviewFrame) return;
    clearTimeout(frameTimer);
    frameTimer = 0;
    showFrameFail(null);
    try { reviewFrame.src = "about:blank"; } catch (e) {}
    reviewFrame.hidden = true;
    reviewFrameLoaded = false;
    frameState = "idle";
    frameAttempt = 0;
    bridgedDoc = null;
  },
  saveState: frameSaveState,
  save: frameSave,
  summary() {
    try {
      const fn = reviewFrameLoaded && reviewFrame.contentWindow && reviewFrame.contentWindow.__frametraceSummary;
      return typeof fn === "function" ? fn() : null;
    } catch (e) { return null; }
  },
  async reload(opts) {
    const how = await frameRefresh((opts && opts.reason) || "");
    return how === "deferred" ? "refused" : "reloaded";
  },
  setTheme: framePushTheme,
  setLocale: framePushLocale,
  getLocale: frameLocale,
  popout: openViewerPopout,
  closePopout() { returnViewerHere(false); },
  isPoppedOut() { return framePopped; },
  focusPopout() {
    try { if (popoutWin && !popoutWin.closed) { popoutWin.focus(); return true; } } catch (e) {}
    return false;
  },
  focus() { try { reviewFrame.focus(); return true; } catch (e) { return false; } },
  examinerInput() {
    try { return reviewFrameLoaded && reviewFrame.contentDocument ? reviewFrame.contentDocument.getElementById("examinerName") : null; } catch (e) { return null; }
  },
};
