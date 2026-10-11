// Theme + viewer language live in the top strip (the shell owns both; the
// hosted viewer hides its own header). Theme: shared ft-theme key, so one
// toggle themes every surface. Language: the review viewer's display
// language (the shell itself is Korean only), remembered per browser.
const hThemeBtn = $("hBtnTheme");
const rootEl = () => document.documentElement || { dataset: {} };
function hThemeNow() { return rootEl().dataset.theme === "dark" ? "dark" : "light"; }
function syncThemeBtn() {
  const dark = hThemeNow() === "dark";
  const label = dark ? "라이트 테마로 전환" : "다크 테마로 전환";
  hThemeBtn.title = label;
  hThemeBtn.setAttribute("aria-label", label);
}
// fromPeer: the change came from another shell tab (ft-review channel).
function hSetTheme(next, fromPeer) {
  const theme = next === "dark" ? "dark" : "light";
  rootEl().dataset.theme = theme;
  try { localStorage.setItem("ft-theme", theme); } catch (e) {}
  hViewer.setTheme(theme);
  FTStore.set({ viewer: { theme } });
  syncThemeBtn();
  if (!fromPeer) hBroadcast("theme", theme);
}
hThemeBtn.addEventListener("click", () => hSetTheme(hThemeNow() === "dark" ? "light" : "dark"));
syncThemeBtn();

// "" = never chosen here (the viewer keeps its own per-case choice).
const H_LOCALE_KEY = "ft.examiner.viewerLocale";
function hLocalePref() {
  try { const v = localStorage.getItem(H_LOCALE_KEY); return v === "en" || v === "ko" ? v : ""; } catch (e) { return ""; }
}
function syncLangBtn() {
  const btn = $("hBtnLang");
  if (!btn) return;
  const cur = hLocalePref() || hViewer.getLocale() || "ko";
  btn.textContent = cur === "en" ? "English" : "한국어";
  btn.setAttribute("aria-label", cur === "en" ? "검토 뷰어 언어 English — 한국어로 전환" : "검토 뷰어 언어 한국어 — English로 전환");
}
function hSetLocale(next, fromPeer) {
  const locale = next === "en" ? "en" : "ko";
  try { localStorage.setItem(H_LOCALE_KEY, locale); } catch (e) {}
  hViewer.setLocale(locale);
  FTStore.set({ viewer: { locale } });
  syncLangBtn();
  if (!fromPeer) hBroadcast("locale", locale);
}
$("hBtnLang").addEventListener("click", () => {
  hSetLocale((hLocalePref() || hViewer.getLocale() || "ko") === "en" ? "ko" : "en");
});
syncLangBtn();
