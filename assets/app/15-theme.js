// Theme toggle — shared ft-theme key, mirrored into the embedded review
// iframe so its chrome follows without a reload.
//
// UX-12: the embedded viewer hides its own header (theme + language
// toggles) in embed mode, so the shell owns both controls and pushes them
// into the frame: a postMessage ({type:"ft-theme"} / {type:"ft-locale"})
// for a viewer that listens, plus a same-origin fallback that retags the
// document / drives the viewer's own (hidden) language button.
const themeBtn = $("btnTheme");
const rootEl = () => document.documentElement || { dataset: {} };
function syncThemeBtn() {
  const dark = rootEl().dataset.theme === "dark";
  // Never empty: the label always names the target theme.
  themeBtn.textContent = dark ? "☀ 라이트" : "◐ 다크";
  themeBtn.title = dark ? "라이트 테마로 전환" : "다크 테마로 전환";
  themeBtn.setAttribute("aria-label", themeBtn.title);
}
function viewerFrameEl() { return $("reviewFrame"); }
function postToViewer(msg) {
  const fr = viewerFrameEl();
  try { if (fr && fr.contentWindow) { fr.contentWindow.postMessage(msg, "*"); return true; } } catch (e) {}
  return false;
}
function pushThemeToViewer(theme) {
  postToViewer({ type: "ft-theme", theme });
  const fr = viewerFrameEl();
  try { if (fr && fr.contentDocument && fr.contentDocument.documentElement) fr.contentDocument.documentElement.dataset.theme = theme; } catch (e) {}
}
themeBtn.addEventListener("click", () => {
  const next = rootEl().dataset.theme === "dark" ? "light" : "dark";
  rootEl().dataset.theme = next;
  try { localStorage.setItem("ft-theme", next); } catch (e) {}
  pushThemeToViewer(next);
  syncThemeBtn();
});
syncThemeBtn();

// Viewer language: remembered by the shell, re-applied on every frame
// load. "" = never chosen here (the viewer keeps its own per-case choice).
const VIEWER_LOCALE_KEY = "ft.examiner.viewerLocale";
function viewerLocalePref() {
  try { const v = localStorage.getItem(VIEWER_LOCALE_KEY); return v === "en" || v === "ko" ? v : ""; } catch (e) { return ""; }
}
// The viewer sets <html lang> from its locale (applyChromeI18n).
function viewerCurrentLocale() {
  try {
    const fr = viewerFrameEl();
    const lang = fr && fr.contentDocument && fr.contentDocument.documentElement ? fr.contentDocument.documentElement.lang : "";
    return lang === "en" || lang === "ko" ? lang : "";
  } catch (e) { return ""; }
}
function syncViewerLangBtn() {
  const btn = $("btnViewerLang");
  if (!btn) return;
  const cur = viewerLocalePref() || viewerCurrentLocale() || "ko";
  btn.textContent = cur === "en" ? "뷰어 언어: English" : "뷰어 언어: 한국어";
  btn.setAttribute("aria-label", (cur === "en" ? "검토 뷰어 언어 English — 한국어로 전환" : "검토 뷰어 언어 한국어 — English로 전환"));
}
function pushLocaleToViewer(locale) {
  if (!locale) return;
  postToViewer({ type: "ft-locale", locale });
  // Same-origin fallback: the viewer's language button is in its hidden
  // header but still clickable; it toggles and persists per case.
  try {
    const fr = viewerFrameEl();
    const doc = fr && fr.contentDocument;
    const cur = viewerCurrentLocale();
    const btn = doc ? doc.getElementById("btnLang") : null;
    if (btn && cur && cur !== locale) btn.click();
  } catch (e) {}
}
$("btnViewerLang").addEventListener("click", () => {
  const next = (viewerLocalePref() || viewerCurrentLocale() || "ko") === "en" ? "ko" : "en";
  try { localStorage.setItem(VIEWER_LOCALE_KEY, next); } catch (e) {}
  pushLocaleToViewer(next);
  syncViewerLangBtn();
});
// Called whenever the review frame (re)loads: theme + language follow the
// shell even if they changed while the frame was loading.
function syncViewerChrome() {
  pushThemeToViewer(rootEl().dataset.theme === "dark" ? "dark" : "light");
  pushLocaleToViewer(viewerLocalePref());
  syncViewerLangBtn();
}
syncViewerLangBtn();

