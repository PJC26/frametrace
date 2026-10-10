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
