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
    reviewFrame.src = partial ? "/viewer/?embed=1" : "/review/evidence-viewer.html?embed=1";
  }
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

$("btnRecoverDeleted").addEventListener("click", async () => {
  const btn = $("btnRecoverDeleted");
  btn.disabled = true;
  $("recoverHint").textContent = "삭제 영상 후보 복구 중… (후보 수에 따라 시간이 걸릴 수 있습니다)";
  let done = false;
  try {
    const res = await jpost("/api/recover-deleted", {});
    if (!res.ok) {
      $("recoverHint").textContent = "복구 실패: " + (res.error || "알 수 없는 오류");
      return;
    }
    done = true;
    $("recoverHint").textContent = "복구 완료 — 뷰어를 갱신합니다.";
    if (reviewFrameLoaded) reviewFrame.src = reviewFrame.src;
  } finally {
    btn.disabled = false;
    if (!done && /중…/.test($("recoverHint").textContent)) $("recoverHint").textContent = "복구가 완료되지 않았습니다 — 다시 시도할 수 있습니다.";
  }
});

