// One-click installer for missing forensic tools (/api/tools/*), shown in
// the '도구' popover under the status badges. One install runs at a time;
// while it runs the status line is polled and, once it settles, the env
// probe is refreshed so badges, the E01 hint and '도구 N/5' update at once.
const TOOL_SETUP = { timer: 0, lastPhase: "idle" };

async function refreshToolSetup() {
  const res = await jgetSoft("/api/tools/install-status");
  const box = $("hToolSetup");
  if (!box) return res;
  if (!res.ok || !res.supported) { box.hidden = true; return res; }
  renderToolSetup(box, res);
  return res;
}

function renderToolSetup(box, st) {
  const running = st.phase === "running";
  const missing = (st.packages || []).filter(p => !p.installed);
  const rows = missing.map(p =>
    `<div class="h-ts-row"><span class="h-ts-name">${esc(p.label)} <small>${esc(p.version)} · ${esc(p.estimate)}</small></span>` +
    `<button type="button" class="h-ts-btn" data-install="${esc(p.id)}"${running ? " disabled" : ""}>` +
    `${running && st.package === p.id ? "설치 중…" : "설치"}</button></div>`).join("");
  const kind = st.phase === "failed" ? " bad" : st.phase === "done" ? " ok" : "";
  const status = st.phase === "idle" ? "" :
    `<p class="h-ts-status${kind}" role="status">${esc(st.message || "")}</p>`;
  box.innerHTML = rows + status;
  box.hidden = !rows && !status;
}

function confirmInstallHtml(p) {
  const libewf = p.id === "libewf"
    ? "<p>libewf는 공식 Windows 실행 파일이 없어 MSYS2(사용자 폴더 msys64)를 받아 소스에서 빌드합니다. 관리자 권한은 필요 없습니다.</p>"
    : "";
  return `<p><b>${esc(p.label)} ${esc(p.version)}</b> — ${esc(p.estimate)}</p>` +
    `<p>고정된 공식 배포 파일을 받아 SHA-256을 확인한 뒤 실행 파일 옆 <code>tools/${esc(p.id)}</code>에 설치합니다. 설치 기록(파일별 해시)이 함께 남습니다.</p>` +
    libewf +
    "<p>Windows 스마트 앱 컨트롤이 켜져 있으면 서명 없는 도구 실행이 차단될 수 있습니다.</p>";
}

async function startToolInstall(id) {
  const st = await refreshToolSetup();
  const pkg = ((st && st.packages) || []).find(p => p.id === id);
  if (!pkg) return;
  const ok = await askDialog({ title: `${pkg.label} 설치`, html: confirmInstallHtml(pkg), okLabel: "설치" });
  if (!ok) return;
  const res = await jpost("/api/tools/install", { package: id });
  if (!res.ok) { toast(res.error || "설치를 시작할 수 없습니다", "err"); return; }
  toast(`${pkg.label} 설치를 시작했습니다`, "ok");
  pollToolInstall();
}

async function pollToolInstall() {
  clearTimeout(TOOL_SETUP.timer);
  const st = await refreshToolSetup();
  const phase = (st && st.phase) || "idle";
  if (phase === "running") {
    TOOL_SETUP.lastPhase = phase;
    TOOL_SETUP.timer = setTimeout(pollToolInstall, 1000);
    return;
  }
  if (TOOL_SETUP.lastPhase === "running") {
    if (phase === "done") toast(st.message || "설치 완료", "ok");
    if (phase === "failed") toast(st.message || "설치 실패", "err");
    await loadEnv(true);
    await refreshToolSetup();
  }
  TOOL_SETUP.lastPhase = phase;
}

$("hToolSetup").addEventListener("click", e => {
  const btn = e.target.closest("button[data-install]");
  if (btn && !btn.disabled) startToolInstall(btn.dataset.install);
});
// Resume the progress line after a reload mid-install.
pollToolInstall();
