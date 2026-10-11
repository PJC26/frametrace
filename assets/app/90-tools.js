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
$("btnCopyLog").addEventListener("click", async () => {
  const text = $("log").textContent;
  let ok = false;
  try { await navigator.clipboard.writeText(text); ok = true; }
  catch (e) { ok = legacyCopy(text); }
  if (ok) flashLabel($("btnCopyLog"), "복사됨", "로그 복사");
  else toast("클립보드에 복사하지 못했습니다 — '로그 저장'으로 파일로 저장하십시오.", "err");
});
$("btnSaveLog").addEventListener("click", () => {
  const p2 = n => String(n).padStart(2, "0");
  const d = new Date();
  const name = `frametrace-log-${d.getFullYear()}${p2(d.getMonth() + 1)}${p2(d.getDate())}-${p2(d.getHours())}${p2(d.getMinutes())}.txt`;
  const blob = new Blob([$("log").textContent], { type: "text/plain;charset=utf-8" });
  const url = URL.createObjectURL(blob);
  const a = document.createElement("a");
  a.href = url; a.download = name;
  document.body.appendChild(a); a.click(); a.remove();
  setTimeout(() => URL.revokeObjectURL(url), 1000);
});
$("log").addEventListener("scroll", () => {
  if (logAtBottom($("log"))) $("btnLogBottom").hidden = true;
});
$("btnLogBottom").addEventListener("click", () => {
  const el = $("log");
  el.scrollTop = el.scrollHeight;
  $("btnLogBottom").hidden = true;
});

// Restore last-used paths/options, then validate what was restored.
(function restorePrefs() {
  const prefs = loadPrefs();
  if (prefs.inputKind) {
    const radio = document.querySelector(`input[name='inputKind'][value='${prefs.inputKind}']`);
    if (radio && !radio.disabled) radio.checked = true;
  }
  for (const [id, key] of [["withHash", "withHash"], ["withFfprobe", "withFfprobe"], ["withDeepfake", "withDeepfake"], ["skipE01Verify", "skipE01Verify"], ["directE01", "directE01"]]) {
    if (typeof prefs[key] === "boolean") $(id).checked = prefs[key];
  }
  if (prefs.lastSource) $("sourcePath").value = prefs.lastSource;
  if (prefs.lastCase) $("caseDir").value = prefs.lastCase;
  syncScanMode();
  renderRecent();
})();

// First-run onboarding: shown once until dismissed, reopenable via
// '안내 다시 보기'. Persisted in the same prefs bucket as recent paths.
if (!loadPrefs().onboarded) $("onboard").hidden = false;
$("btnOnboardOk").addEventListener("click", () => {
  savePrefs({ onboarded: true });
  $("onboard").hidden = true;
});
$("btnOnboardShow").addEventListener("click", () => {
  $("onboard").hidden = false;
  $("onboard").scrollIntoView({ block: "nearest" });
});

