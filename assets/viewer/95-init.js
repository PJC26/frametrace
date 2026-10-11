const examinerInput = document.getElementById("examinerName");
if (examinerInput) {
  examinerInput.value = state.examiner || "";
  examinerInput.addEventListener("input", () => {
    state.examiner = examinerInput.value;
    storageSet(EXAMINER_KEY, state.examiner);
  });
}
// Embedded mode (inside the examiner workstation iframe): drop the
// viewer's own chrome — case title, stat badges, locale toggle, view
// menu — because the host already shows them. Grid/media keep full
// functionality. The host flips it off over postMessage when the
// iframe goes fullscreen, so the full UI returns there.
const setEmbed = on => document.body.classList.toggle("embed", on);
setEmbed(new URLSearchParams(location.search).has("embed") || window.self !== window.top);
const exitFsBtn = document.getElementById("btnExitFs");
window.addEventListener("message", event => {
  if (!event.data || event.data.type !== "ft-embed") return;
  setEmbed(!!event.data.embed);
  // Framed + embed off = the host fullscreened us: show a way back.
  if (exitFsBtn) exitFsBtn.hidden = event.data.embed || window.self === window.top;
});
// UX-06: the workstation shell posts {type:"ft-reload"} after a
// Recover/Carve job rewrote the case. The record model is derived once at
// boot, so new data means re-loading the page — which pages /api/records
// and /api/records-meta afresh (layout/selection live in localStorage).
// That is only done when every review change has reached the case DB:
// otherwise the reload is refused (and a save started), so annotations are
// never lost to a refresh. The shell always gets an answer:
// {type:"ft-reload-ack", reloaded:true|false, reason}.
async function viewerReloadBlocker() {
  if (pendingReviewSync || saveInFlight || state.deletedIds.length) return "unsaved";
  if (queuedOnServer > 0) {
    // Marks durably queued on the server are applied before the job frees
    // the case; confirm the queue is empty before reloading over them.
    try {
      const res = await fetch("/api/status", { cache: "no-store" });
      const st = res.ok ? await res.json() : null;
      if (st && Number(st.pending_marks) === 0) {
        queuedOnServer = 0;
        renderSaveState();
        return "";
      }
    } catch (e) { /* unknown → treat as still queued */ }
    return "queued";
  }
  return "";
}
window.addEventListener("message", async event => {
  if (!event.data || event.data.type !== "ft-reload") return;
  if (event.source && window.parent !== window && event.source !== window.parent) return;
  const target = event.source || window.parent;
  const answer = (reloaded, reason) => {
    try { target.postMessage({ type: "ft-reload-ack", reloaded, reason: reason || "" }, "*"); } catch (e) { /* host gone */ }
  };
  const blocker = await viewerReloadBlocker();
  if (blocker) {
    if (blocker === "unsaved" && viewerIsServed && !saveInFlight) syncMarksNow();
    toast(t("reload.refused"));
    answer(false, blocker);
    return;
  }
  answer(true, "");
  location.reload();
});
if (exitFsBtn) {
  exitFsBtn.addEventListener("click", () => {
    try { window.parent.postMessage({ type: "ft-exit-fullscreen" }, "*"); } catch (e) {}
  });
}
setupHeightSplitter();
setupColumnSplitter();
setupGridDelegation();
document.getElementById("btnLang")?.addEventListener("click", () => {
  state.locale = state.locale === "ko" ? "en" : "ko";
  storageSet(LOCALE_KEY, state.locale);
  render();
});
// Shared ft-theme key — same choice as the workstation shell. When this
// page is the embedded iframe the host also retags this document
// directly, so both paths land on the same attribute.
const themeBtn = document.getElementById("btnTheme");
const syncThemeBtn = () => {
  if (!themeBtn) return;
  const dark = document.documentElement.dataset.theme === "dark";
  themeBtn.textContent = dark ? "☀ " + t("theme.light") : "◐ " + t("theme.dark");
};
themeBtn?.addEventListener("click", () => {
  const next = document.documentElement.dataset.theme === "dark" ? "light" : "dark";
  document.documentElement.dataset.theme = next;
  try { localStorage.setItem("ft-theme", next); } catch (e) {}
  syncThemeBtn();
});
syncThemeBtn();
applyLayout();
render();
})();

// Data loading lives in 99-loader.js (outside the IIFE; see its header).
