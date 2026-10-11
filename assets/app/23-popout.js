// ---- second monitor (docs/ui-rebuild-contract.md §6.7, §3.3) ----
// "새 창" (or the viewer's own pop-out button, request-popout) moves video
// + verdict into a second window: FTViewer.popout() opens the popup player
// (video, transport, marks 1/2/3/0, tags, j/k, undo). This window keeps
// the grid, rail and selection bar; the media column folds under
// #hPopoutNote in the same place (no layout shift).
//
// Single writer: only this window saves. The popup never writes marks
// itself — it hands them to this window over the existing
// ft-player:<case_id> bridge — so two writers cannot happen by design.
// BroadcastChannel "ft-review" tells other shell tabs (they disable their
// own pop-out button) and keeps theme / language in step across tabs.
const H_REVIEW_CHANNEL = "ft-review";
const hInstance = Math.random().toString(36).slice(2, 10);
let hChan = null, hChanTried = false;
let hPeerPopped = false;
function hChannel() {
  if (hChan || hChanTried) return hChan;
  hChanTried = true;
  if (typeof BroadcastChannel !== "function") return null;
  try {
    hChan = new BroadcastChannel(H_REVIEW_CHANNEL);
    hChan.onmessage = ev => hOnPeer(ev && ev.data);
  } catch (e) { hChan = null; }
  return hChan;
}
function hBroadcast(type, value) {
  const c = hChannel();
  if (!c) return;
  try { c.postMessage({ type, caseDir: FTStore.state.project.caseDir || "", value: value === undefined ? null : value, from: hInstance }); } catch (e) {}
}
function hOnPeer(m) {
  if (!m || typeof m !== "object" || m.from === hInstance) return;
  if (m.type === "popout-open" || m.type === "popout-closed") {
    hPeerPopped = m.type === "popout-open";
    hRenderStrip(hMode());
  } else if (m.type === "theme" && (m.value === "dark" || m.value === "light") && m.value !== hThemeNow()) {
    hSetTheme(m.value, true);
  } else if (m.type === "locale" && (m.value === "en" || m.value === "ko") && m.value !== hLocalePref()) {
    hSetLocale(m.value, true);
  }
}
function hPopoutOpen() {
  if (!FTStore.state.viewer.mounted || hPeerPopped) return;
  if (FTStore.state.popout.open) { hViewer.focusPopout(); return; }
  const win = hViewer.popout();
  if (!win) {
    toast("팝업이 차단되었습니다 — 브라우저에서 이 사이트의 팝업을 허용하십시오.", "warn");
    return;
  }
  hSetPopped(true);
}
// The note covers exactly the viewer's media column (same place, same
// width — no layout shift); the column's width is the viewer's own, so it
// is measured rather than assumed.
function hPlacePopnote() {
  const note = $("hPopoutNote"), host = $("hViewerHost");
  if (!note || note.hidden || !host || typeof host.querySelector !== "function") return;
  const media = host.querySelector(".ft-viewer .media");
  if (!media || typeof media.getBoundingClientRect !== "function") return;
  const m = media.getBoundingClientRect(), h = host.getBoundingClientRect();
  if (!m.width) return;
  note.style.left = Math.round(m.left - h.left) + "px";
  note.style.width = Math.round(m.width) + "px";
  note.style.top = Math.round(m.top - h.top) + "px";
  note.style.bottom = Math.round(h.bottom - m.bottom) + "px";
  note.style.right = "auto";
}
if (typeof window !== "undefined" && typeof window.addEventListener === "function") window.addEventListener("resize", hPlacePopnote);
function hSetPopped(on) {
  const open = !!on;
  const was = !!FTStore.state.popout.open;
  FTStore.set({ popout: { open, writer: "main" } });
  $("hPopoutNote").hidden = !open;
  const host = $("hViewerHost");
  const root = host && host.querySelector ? host.querySelector(".ft-viewer") : null;
  if (open) hPlacePopnote();
  if (root && root.classList) root.classList.toggle("popped", open);
  if (was !== open) {
    hBroadcast(open ? "popout-open" : "popout-closed");
    announce(open ? "영상·판독을 보조 창으로 옮겼습니다. 저장은 이 창에서 합니다." : "영상을 이 화면으로 되돌렸습니다.");
  }
  hRenderStrip(hMode());
}
$("hBtnPopout").addEventListener("click", hPopoutOpen);
$("hBtnPopoutReturn").addEventListener("click", () => {
  hViewer.closePopout();
  hSetPopped(false);
});
$("hBtnPopoutFocus").addEventListener("click", () => {
  if (!hViewer.focusPopout() && !hViewer.isPoppedOut()) hSetPopped(false);
});
hChannel();
