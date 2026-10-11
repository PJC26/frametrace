// Theme toggle — shared ft-theme key, mirrored into the embedded review
// iframe so its chrome follows without a reload.
const themeBtn = $("btnTheme");
const rootEl = () => document.documentElement || { dataset: {} };
function syncThemeBtn() {
  const dark = rootEl().dataset.theme === "dark";
  // Never empty: the label always names the target theme.
  themeBtn.textContent = dark ? "☀ 라이트" : "◐ 다크";
  themeBtn.title = dark ? "라이트 테마로 전환" : "다크 테마로 전환";
  themeBtn.setAttribute("aria-label", themeBtn.title);
}
themeBtn.addEventListener("click", () => {
  const next = rootEl().dataset.theme === "dark" ? "light" : "dark";
  rootEl().dataset.theme = next;
  try { localStorage.setItem("ft-theme", next); } catch (e) {}
  const fr = $("reviewFrame");
  try { if (fr && fr.contentDocument) fr.contentDocument.documentElement.dataset.theme = next; } catch (e) {}
  syncThemeBtn();
});
syncThemeBtn();

