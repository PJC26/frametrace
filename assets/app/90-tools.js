function flashLabel(btn, text, base) {
  btn.textContent = text;
  setTimeout(() => { btn.textContent = base; }, 1600);
}
function legacyCopy(text) {
  const ta = document.createElement("textarea");
  ta.value = text;
  ta.setAttribute("readonly", "");
  ta.style.position = "fixed"; ta.style.opacity = "0";
  document.body.appendChild(ta);
  ta.select();
  let ok = false;
  try { ok = document.execCommand("copy"); } catch (e) { ok = false; }
  ta.remove();
  return ok;
}
// Log copy / save / jump-to-latest live with the log in 32-jobs.js.

// Restore last-used paths/options, then validate what was restored.
(function restorePrefs() {
  const prefs = loadPrefs();
  // The evidence kind is not restored: it is detected from each path.
  for (const [id, key] of [["withHash", "withHash"], ["withFfprobe", "withFfprobe"], ["withDeepfake", "withDeepfake"], ["skipE01Verify", "skipE01Verify"], ["directE01", "directE01"]]) {
    if (typeof prefs[key] === "boolean") $(id).checked = prefs[key];
  }
  // The empty state opens empty: the last source/case stay one click away
  // (path suggestions, '이전 프로젝트') instead of being pre-filled — a
  // pre-filled case folder hidden under '옵션' would silently send new
  // evidence into the previous case.
  syncScanMode();
  renderRecent();
})();
