// popup=yes + dimensions asks the browser for a real separate window
// (not just another tab) — useful for dual-monitor review. The named
// target re-focuses an already-open viewer instead of stacking copies.
const reviewFrame = $("reviewFrame");
let reviewFrameLoaded = false;
// UX-02: before make-review has written review/ the server's built-in
// /viewer/ page (same viewer, data via /api/records?live=1) is the frame.
// Once loaded, the frame is never swapped behind the examiner's back —
// a navigation could drop marks that are still being saved.
function ensureReviewFrame(partial) {
  if (!reviewFrameLoaded) {
    reviewFrameLoaded = true;
    loadReviewFrame(partial ? "/viewer/?embed=1" : "/review/evidence-viewer.html?embed=1");
  }
}

// ---- UX-18: frame load watchdog + visible retry ----
// "loading" until the frame's load event shows a real viewer document;
// "failed" after FRAME_TIMEOUT_MS without one, or when it loaded an error
// page (404 text has no <script>). Retrying a failed frame is safe — a
// viewer that never came up holds no marks.
const FRAME_TIMEOUT_MS = 20000;
let frameState = "idle", frameUrl = "", frameAttempt = 0, frameTimer = 0;
function loadReviewFrame(url) {
  frameUrl = url;
  frameAttempt += 1;
  frameState = "loading";
  showFrameFail(null);
  clearTimeout(frameTimer);
  frameTimer = setTimeout(frameWatchdog, FRAME_TIMEOUT_MS);
  // A retry needs a distinct URL or a cached failure may be replayed.
  reviewFrame.src = frameAttempt > 1 ? `${url}${url.includes("?") ? "&" : "?"}retry=${frameAttempt}` : url;
}
// Same-origin inspection: a viewer page carries scripts (and soon its
// summary hook); a plain-text error body does not. Cross-origin or
// uninspectable frames are trusted once they fire load.
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
  if (frameState !== "loading") return;
  // DOM is up and the viewer is running even though some subresource
  // still holds the load event: not a failure.
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
  if (!reviewFrameLoaded) return;
  const st = frameDocState();
  if (st === "empty") {
    frameFailed("뷰어 페이지가 비어 있거나 오류 응답입니다 — 검토 데이터가 아직 만들어지지 않았거나 서버 오류일 수 있습니다.");
    return;
  }
  clearTimeout(frameTimer);
  frameTimer = 0;
  frameState = "ok";
  showFrameFail(null);
  // Theme/locale are pushed until the viewer acks (see 15-theme.js), so no
  // fixed-delay retries are needed for a viewer that wires up late.
  syncViewerChrome();
}
reviewFrame.addEventListener("load", onReviewFrameLoad);
$("btnFrameRetry").addEventListener("click", () => {
  if (!frameUrl) return;
  if (frameState !== "failed") { showFrameFail(null); return; }
  loadReviewFrame(frameUrl);
});
$("btnFrameWindow").addEventListener("click", () => {
  const win = window.open(standaloneViewerUrl(), "_blank");
  if (!win) toast("팝업이 차단되었습니다 — 브라우저에서 이 사이트의 팝업을 허용하십시오.", "warn");
});

// ---- viewer data refresh after a background job (UX-06) ----
// Never `src = src`: that navigation drops marks still being saved.
// 1) ask the viewer over postMessage ({type:"ft-reload"}); a viewer that
//    handles it answers {type:"ft-reload-ack"} (or reloads itself, which
//    fires the frame's load event); 2) otherwise reload it only when its
//    save indicator is clean; 3) else defer and let the examiner retry.
const VIEWER_RELOAD_ACK_MS = 1500;
function viewerHasUnsaved() {
  const ss = viewerSaveState();
  return ss === "pending" || ss === "saving" || ss === "failed" || ss === "queued";
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
    let t = 0, poll = 0;
    const done = () => { clearTimeout(t); clearInterval(poll); reviewFrame.removeEventListener("load", done); resolve(); };
    // onReviewFrameLoad (registered first) has already set frameState.
    poll = setInterval(() => { if (frameState !== "loading") done(); }, 250);
    reviewFrame.addEventListener("load", done);
    t = setTimeout(done, ms);
  });
}
// "none" (no frame yet — the next open loads fresh data), "posted"
// (the viewer took it), "reloaded", "deferred" (unsaved marks).
async function refreshViewerData(reason) {
  if (!reviewFrameLoaded) return "none";
  if (frameState === "failed") {
    // Nothing reviewable is on screen: (re)load is safe.
    loadReviewFrame(frameUrl);
    return "reloaded";
  }
  if (frameState === "loading") {
    // The viewer is still coming up: it may hold marks by the time it is
    // live, so wait for it and use the normal ft-reload handshake below.
    await waitForFrameSettled(FRAME_TIMEOUT_MS + 1000);
    if (frameState === "failed") { loadReviewFrame(frameUrl); return "reloaded"; }
    if (frameState !== "ok") return "deferred";
  }
  const ack = waitForViewerAck(VIEWER_RELOAD_ACK_MS);
  postToViewer({ type: "ft-reload", reason: reason || "" });
  const got = await ack;
  if (got === "ack") return "posted";
  if (got === "refused") return "deferred";
  return guardedFrameReload();
}
function guardedFrameReload() {
  if (viewerHasUnsaved()) {
    retryViewerSave(); // start saving now; the examiner retries after
    return "deferred";
  }
  try {
    reviewFrame.contentWindow.location.reload();
  } catch (e) {
    loadReviewFrame(frameUrl);
  }
  return "reloaded";
}
function refreshCaseStats() {
  const el = $("caseStats");
  let s = null;
  try {
    const fn = reviewFrame.contentWindow && reviewFrame.contentWindow.__frametraceSummary;
    if (typeof fn === "function") s = fn();
  } catch (e) { /* not loaded yet */ }
  if (!s) { el.innerHTML = `<span class="muted" style="font-size:12px">뷰어 로딩 중…</span>`; return; }
  if (!s.total) {
    el.innerHTML =
      `<span class="stat" style="border-color:#f0cf9e"><i style="color:#b2560f">발견된 영상 없음</i></span>` +
      `<span class="muted" style="font-size:12px;align-self:center">다른 소스로 다시 분석하거나, 디스크 이미지라면 '이미지 카빙'을 시도하십시오.</span>`;
    return;
  }
  const kindLabel = { video: "원본", carved: "카빙", filesystem: "복구", candidate: "복구 전" };
  const chips = [`<span class="stat"><i>전체</i>${s.total}</span>`];
  ["video", "carved", "filesystem", "candidate"].forEach(k => {
    if (s.byKind[k]) chips.push(`<span class="stat k-${k}"><i>${kindLabel[k]}</i>${s.byKind[k]}</span>`);
  });
  const failed = s.byStatus["validation-failed"] || 0;
  if (failed) chips.push(`<span class="stat" style="border-color:#f0b6b6"><i style="color:#b91c1c">검증 실패</i>${failed}</span>`);
  if (s.warned) chips.push(`<span class="stat" style="border-color:#f0cf9e"><i style="color:#b2560f">경고</i>${s.warned}</span>`);
  chips.push(`<span class="stat"><i>마크</i>${s.marked}</span>`);
  if (s.noted) chips.push(`<span class="stat"><i>메모</i>${s.noted}</span>`);
  el.innerHTML = chips.join("");
}
reviewFrame.addEventListener("load", () => setTimeout(refreshCaseStats, 400));
setInterval(() => { if (reviewFrameLoaded && $("resultCard").classList.contains("on")) refreshCaseStats(); }, 2500);

$("btnViewerFull").addEventListener("click", () => {
  reviewFrame.requestFullscreen ? reviewFrame.requestFullscreen() : window.open(standaloneViewerUrl(), "_blank");
});
// Fullscreen restores the viewer's full chrome; inline stays compact.
document.addEventListener("fullscreenchange", () => {
  const full = document.fullscreenElement === reviewFrame;
  try { reviewFrame.contentWindow.postMessage({ type: "ft-embed", embed: !full }, "*"); } catch (e) {}
});
window.addEventListener("message", e => {
  if (e.data && e.data.type === "ft-exit-fullscreen" && document.fullscreenElement === reviewFrame) {
    document.exitFullscreen();
  }
});

async function ensureReportFrame() {
  const wrap = $("reportWrap"), frame = $("reportFrame");
  try {
    const head = await fetch("/case/reports/case-report.html");
    if (!head.ok) return;
  } catch (e) { return; }
  wrap.hidden = false;
  if (!frame.src || frame.src === "about:blank") frame.src = "/case/reports/case-report.html";
  else frame.contentWindow && frame.contentWindow.location.reload();
}

$("btnViewerWindow").addEventListener("click", () => {
  const win = window.open(
    standaloneViewerUrl(),
    "frametrace-viewer",
    "popup=yes,width=1500,height=950"
  );
  if (win) win.focus();
  else window.open(standaloneViewerUrl(), "_blank"); // popup blocked: fall back to a tab
});

// Recover / Carve run as background case jobs: see 65-jobs.js.

