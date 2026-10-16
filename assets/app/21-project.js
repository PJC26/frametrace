// ---- project switcher (docs/ui-rebuild-contract.md §6.6) ----
// #hProjectBtn (the case name in the strip) opens #hProjectPop: the open
// project, up to 8 previous projects, "다른 케이스 폴더 열기…", "새 증거
// 넣기" and the index dashboard. Switching never reloads the page: unsaved
// check → viewer unmount → POST /api/open-case → one status poll → mount.
const H_PHASE_LABEL = {
  running: "분석 중", "review-partial": "색인 완료 · 보강 전", "review-ready": "검토 가능",
  done: "검토 가능", finalized: "결과물 생성됨", error: "분석 실패",
};
function hProjectPopRender() {
  const s = FTStore.state;
  const cur = $("hProjectCur");
  if (cur) {
    // R10 U4: the open project's last package did not finish finalizing.
    const incomplete = !!s.project.packageIncomplete;
    const label = incomplete ? H_PKG_INCOMPLETE : H_PHASE_LABEL[s.phase] || "";
    const cls = incomplete ? " warn" : s.phase === "finalized" ? " ok" : s.phase === "review-partial" ? " warn" : "";
    cur.innerHTML = s.project.caseDir
      ? `<b title="${esc(s.project.caseDir)}">${esc(s.project.name || s.project.caseDir)}</b>` +
        `<span class="mono">${esc(s.project.caseDir)}</span>` +
        (label ? `<span class="h-badge${cls}">${esc(label)}</span>` : "")
      : "";
  }
  const list = $("hProjectRecent");
  if (!list || typeof document.createElement !== "function") return;
  const others = (typeof recentItems !== "undefined" ? recentItems : [])
    .filter(it => !samePathish(it.case_dir, s.project.caseDir)).slice(0, 8);
  list.innerHTML = "";
  if (!others.length) {
    const li = document.createElement("li");
    li.className = "h-empty-row";
    li.textContent = "다른 프로젝트가 없습니다.";
    list.appendChild(li);
    return;
  }
  for (const it of others) {
    const li = document.createElement("li");
    const btn = document.createElement("button");
    btn.type = "button";
    btn.className = "quiet h-mi";
    const name = it.name || hBaseName(it.case_dir) || it.case_dir;
    const st = recentState(it, lastStatus);
    btn.innerHTML = `<span title="${esc(it.case_dir)}">${esc(name)}</span><small>${esc([fmtWhen(it.last_opened_unix), st.state].filter(Boolean).join(" · "))}</small>`;
    if (it.exists === false) { btn.disabled = true; btn.title = "폴더를 찾을 수 없습니다"; }
    btn.addEventListener("click", () => hSwitchProject(it.case_dir));
    li.appendChild(btn);
    list.appendChild(li);
  }
}

// Resolves true when the viewer has nothing unsaved (or the examiner saved
// it from the dialog); false keeps the current project.
async function hConfirmLeave(title) {
  const unsaved = () => hViewer.hasUnsaved();
  if (!FTStore.state.viewer.mounted || !unsaved()) return true;
  const go = await askDialog({
    title,
    html: "<p><b>케이스에 아직 반영되지 않은 마크·메모 변경</b>이 있습니다.</p><p>‘저장 다시 시도’로 저장한 뒤 진행합니다.</p>",
    okLabel: "저장 다시 시도", cancelLabel: "취소",
    onOk: async () => {
      hViewer.save();
      $("dlgNote").textContent = "저장 중…";
      for (let i = 0; i < 24; i++) {
        await new Promise(r => setTimeout(r, 250));
        if (!unsaved()) return true;
      }
      $("dlgNote").textContent = "아직 저장되지 않았습니다 — 뷰어의 저장 상태를 확인한 뒤 다시 시도하십시오.";
      return false;
    },
  });
  return !!go && !unsaved();
}
function hFoldPopout() {
  if (!FTStore.state.popout.open) return;
  hViewer.closePopout();
  hSetPopped(false);
}
// Takes the viewer down after the unsaved check (→ empty state). ui.home
// goes up first so the renderer does not remount the old case in between.
async function hLeaveViewer(title) {
  if (!(await hConfirmLeave(title))) return false;
  hFoldPopout();
  hCloseSheet("finalize", false);
  FTStore.set({ ui: { home: true } });
  hViewer.unmount();
  return true;
}
// True while a project switch runs: the renderer keeps the project screen
// but must not remount the old case between destroy and init.
let hSwitching = false;
async function hSwitchProject(dir) {
  hClosePopovers(false);
  if (!dir) return false;
  if (!(await hConfirmLeave("프로젝트 전환"))) return false;
  hFoldPopout();
  hCloseSheet("finalize", false);
  hViewer.retry();
  let opened = false;
  let switched = false;
  hSwitching = true;
  try {
    // save → destroy → POST /api/open-case → one poll → init; no page reload
    // (22-viewer-host.js). Refused (false) when unsaved marks would not save.
    switched = await hostViewerSwitch(async () => {
      FTStore.set({ viewer: { mounted: false, saveState: null, dirty: 0, selection: 0, activeId: null } });
      opened = await openCase(dir);
      if (!opened) return false;
      try { await poll(); } catch (e) { /* the poll loop retries */ }
      FTStore.set({ ui: { home: false } });
      return true;
    });
  } finally {
    hSwitching = false;
  }
  if (!switched) {
    toast(hostViewer.stillSaving
      ? "판독 내용을 아직 저장 중이라 프로젝트를 바꾸지 않았습니다 — 저장이 끝난 뒤 다시 시도하십시오."
      : "저장되지 않은 판독 내용이 있어 프로젝트를 바꾸지 않았습니다 — 뷰어의 저장 상태를 확인하십시오.", "warn");
    return false;
  }
  if (!opened) toast(($("startErr").textContent || "케이스를 열지 못했습니다."), "err");
  hRender();
  return opened;
}
async function hGoHome() {
  hClosePopovers(false);
  if (!(await hLeaveViewer("새 증거 넣기"))) return false;
  hCloseSheet("jobs", false);
  setTimeout(() => { try { $("sourcePath").focus(); } catch (e) {} }, 0);
  return true;
}
$("hProjectNew").addEventListener("click", () => { hGoHome(); });
$("hProjectOpenOther").addEventListener("click", () => {
  openPicker("caseDir", "case", dir => { hSwitchProject(dir); });
});
