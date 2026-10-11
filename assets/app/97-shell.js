// ---- one integrated screen (docs/ui-rebuild-contract.md §6) ----
// Three modes, rendered from FTStore: "empty" (#inputCard — no project,
// a restored session, or "새 증거 넣기"), "wait" (an analysis is indexing;
// the host shows one line and the jobs sheet opener) and "project" (the
// review viewer mounted in #hViewerHost). Nothing navigates; the jobs and
// deliverables panels are right-hand sheets over the grid.
let hPrevMode = "";
let hStatusSeen = false;
function hMode() {
  const s = FTStore.state;
  if (s.ui.home) return "empty";
  if (s.phase === "running") return "wait";
  if (FTStore.hasProject()) return "project";
  return "empty";
}
function hRender() {
  // Until the first status reply the screen stays blank instead of
  // flashing the empty state in front of a bound case.
  if (!hStatusSeen) return;
  const s = FTStore.state;
  const mode = hMode();
  rootEl().dataset.mode = mode;
  $("inputCard").hidden = mode !== "empty";
  $("hViewerHost").hidden = mode === "empty";
  $("hHostWait").hidden = mode !== "wait";
  if (mode === "project" && !s.viewer.mounted && !hSwitching) hViewer.mount();
  if (mode !== "project" && s.panels.finalize) hCloseSheet("finalize", false);
  if (mode === "empty" && hPrevMode && hPrevMode !== "empty" && typeof onHomeShown === "function") onHomeShown();
  // First paint of the empty state: the evidence field takes focus (the
  // markup's autofocus cannot, the section starts hidden).
  if (mode === "empty" && !hPrevMode && (!document.activeElement || document.activeElement === document.body)) {
    try { $("sourcePath").focus(); } catch (e) {}
  }
  hPrevMode = mode;
  hRenderStrip(mode);
}
// Called by the status poll for every reply (null = the poll failed).
function hOnStatus(st) {
  if (!hStatusSeen) {
    hStatusSeen = true;
    // A restored session (server reopened the last case) and "새 분석"
    // (which reloads the page) both land on the empty state; the bound
    // case is the first row of 이전 프로젝트 with [이어서 검토].
    let wantNew = false;
    try {
      wantNew = sessionStorage.getItem("ft_want_new") === "1";
      if (wantNew) sessionStorage.removeItem("ft_want_new");
    } catch (e) { /* storage blocked */ }
    if (st && (wantNew || st.restored)) FTStore.set({ ui: { home: true } });
  }
  hRender();
}
function hRenderStrip(mode) {
  const s = FTStore.state;
  const pb = $("hProjectBtn");
  const hasCase = mode !== "empty" && !!s.project.caseDir;
  pb.disabled = !hasCase;
  $("hProjectName").textContent = hasCase ? (s.project.name || s.project.caseDir) : "프로젝트 없음";
  pb.title = hasCase ? `${s.project.caseDir} — 프로젝트 전환 (Alt+P)` : "열린 프로젝트가 없습니다";
  pb.setAttribute("aria-label", hasCase ? `프로젝트: ${s.project.name || s.project.caseDir} — 전환 (Alt+P)` : "프로젝트 없음");
  if (!hasCase && s.panels.project) hClosePopovers(false);
  const fb = $("hBtnFinalize");
  fb.disabled = mode !== "project";
  fb.title = mode === "project" ? "결과물 — 보고서·패키지 (Alt+R)"
    : mode === "wait" ? "첫 색인이 끝나면 열 수 있습니다" : "열린 프로젝트가 없습니다";
  const pop = $("hBtnPopout");
  pop.disabled = mode !== "project" || !s.viewer.mounted || hPeerPopped;
  pop.title = hPeerPopped ? "다른 창에서 열림 — 보조 창은 한 번에 하나만 엽니다"
    : mode !== "project" ? "열린 프로젝트가 없습니다"
    : s.popout.open ? "보조 창 보기" : "보조 모니터로 영상·판독 분리 — 저장은 이 창에서만 합니다";
}
FTStore.on("change", ({ keys }) => {
  if (keys.has("phase") || keys.has("project") || keys.has("ui") || keys.has("viewer") || keys.has("popout")) hRender();
});

// ---- sheets (§6.3) ----
// Jobs: non-modal, focus to its close button, Esc while focus is inside,
// never closed by an outside click, auto-opened once after the first
// analysis start. Deliverables: modal — #main and the strip go inert,
// focus is trapped, Esc closes unless a job in it is busy. Never both.
const H_SHEETS = { jobs: "hJobsSheet", finalize: "hFinalizeSheet" };
const H_SHEET_OPENERS = { jobs: ["hBtnJobs", "hBtnHostJobs"], finalize: ["hBtnFinalize"] };
const hSheetReturn = { jobs: null, finalize: null };
let hJobsAutoOpened = false, hJobsUserClosed = false;
function hFocusables(root) {
  if (!root || typeof root.querySelectorAll !== "function") return [];
  return [...root.querySelectorAll("button, input, select, textarea, summary, a[href], [tabindex='0']")]
    .filter(el => !el.disabled && !el.hidden && el.offsetParent !== null);
}
function hSetOpenerState(which, open) {
  for (const id of H_SHEET_OPENERS[which] || []) { const b = $(id); if (b) b.setAttribute("aria-expanded", open ? "true" : "false"); }
}
function hOpenSheet(which, opener) {
  const sheet = $(H_SHEETS[which]);
  if (!sheet) return;
  const other = which === "jobs" ? "finalize" : "jobs";
  if (FTStore.state.panels[other]) hCloseSheet(other, false);
  hClosePopovers(false);
  hSheetReturn[which] = opener || document.activeElement || null;
  sheet.hidden = false;
  rootEl().dataset.sheet = which;
  hSetOpenerState(which, true);
  if (which === "finalize") { $("main").inert = true; $("hTopStrip").inert = true; }
  FTStore.set({ panels: { [which]: true } });
  // The sheet's contents are rendered by its owner (32-jobs / 41-finalize)
  // and may land a moment later; retry briefly until focus is inside.
  let tries = 0;
  const focusFirst = () => {
    if (sheet.hidden || !FTStore.state.panels[which]) return;
    if (typeof sheet.contains === "function" && sheet.contains(document.activeElement)) return;
    const first = which === "finalize"
      ? ((sheet.querySelector && sheet.querySelector("#finalExaminer")) || hFocusables(sheet).find(el => el.id !== "hFinalizeClose") || $("hFinalizeClose"))
      : ($("hJobsClose") || hFocusables(sheet)[0]);
    try { if (first) first.focus(); } catch (e) {}
    if (++tries < 10 && typeof sheet.contains === "function" && !sheet.contains(document.activeElement)) setTimeout(focusFirst, 50);
  };
  setTimeout(focusFirst, 0);
}
function hCloseSheet(which, refocus) {
  const sheet = $(H_SHEETS[which]);
  if (!sheet) return;
  const wasOpen = !sheet.hidden;
  sheet.hidden = true;
  if (rootEl().dataset.sheet === which) delete rootEl().dataset.sheet;
  hSetOpenerState(which, false);
  if (which === "finalize") { $("main").inert = false; $("hTopStrip").inert = false; }
  FTStore.set({ panels: { [which]: false } });
  if (!wasOpen) return;
  if (which === "jobs") hJobsUserClosed = true;
  if (refocus) {
    const cand = [...(H_SHEET_OPENERS[which] || []).map(id => $(id)), hSheetReturn[which], $("hProjectBtn"), $("main")];
    const target = cand.find(el => el && !el.disabled && el.offsetParent !== null);
    try { if (target) target.focus(); } catch (e) {}
  }
}
function hToggleSheet(which, opener) {
  if (FTStore.state.panels[which]) hCloseSheet(which, true);
  else hOpenSheet(which, opener);
}
// The first analysis start of this page opens the jobs sheet once; after
// the examiner closes it, progress stays in the chip only.
function hAutoOpenJobs() {
  if (hJobsAutoOpened || hJobsUserClosed) return;
  hJobsAutoOpened = true;
  hOpenSheet("jobs", $("hBtnJobs"));
}
$("hBtnJobs").addEventListener("click", () => hToggleSheet("jobs", $("hBtnJobs")));
$("hBtnHostJobs").addEventListener("click", () => hOpenSheet("jobs", $("hBtnHostJobs")));
$("hBtnFinalize").addEventListener("click", () => hToggleSheet("finalize", $("hBtnFinalize")));
// Close buttons are matched by delegation: the sheets' contents are
// re-rendered by the jobs / finalize code.
for (const [which, id] of Object.entries(H_SHEETS)) {
  $(id).addEventListener("click", e => {
    const t = e.target && e.target.closest ? e.target.closest("#hJobsClose, #hFinalizeClose, [data-sheet-close]") : null;
    if (t) hCloseSheet(which, true);
  });
}

// ---- popovers (project, tools, help) ----
// Disclosure popovers: click / Enter / Space toggles, ArrowDown opens and
// focuses the first item, arrows / Home / End move, Esc or Tab-out closes
// (Esc returns focus to the opener). An action item closes the popover
// first, so a dialog it opens restores focus to the opener.
const POPOVERS = [["hProjectBtn", "hProjectPop", "project"], ["hBtnTools", "hToolsPop", "tools"], ["hBtnHelp", "hHelpPop", "help"]];
function popoverItems(pop) {
  return [...pop.querySelectorAll("button, a[href]")]
    .filter(el => !el.disabled && !el.hidden && !(el.closest && el.closest("[hidden]")));
}
function hClosePopovers(refocus) {
  for (const [btnId, popId, key] of POPOVERS) {
    const btn = $(btnId), pop = $(popId);
    if (!btn || !pop || pop.hidden) continue;
    pop.hidden = true;
    btn.setAttribute("aria-expanded", "false");
    FTStore.set({ panels: { [key]: false } });
    if (refocus) { try { btn.focus(); } catch (e) {} }
  }
}
// Kept under its old name: other parts call it when something else opens.
const closeAllPopovers = hClosePopovers;
function hOpenPopover(btnId, popId, key, focusFirst) {
  hClosePopovers(false);
  const btn = $(btnId), pop = $(popId);
  if (!btn || !pop || btn.disabled) return;
  if (key === "project" && typeof hProjectPopRender === "function") hProjectPopRender();
  pop.hidden = false;
  btn.setAttribute("aria-expanded", "true");
  FTStore.set({ panels: { [key]: true } });
  if (focusFirst) {
    const items = popoverItems(pop);
    if (items[0]) items[0].focus();
  }
}
function hTogglePopover(key, focusFirst) {
  const row = POPOVERS.find(p => p[2] === key);
  if (!row) return;
  const pop = $(row[1]);
  if (pop && !pop.hidden) hClosePopovers(true);
  else hOpenPopover(row[0], row[1], row[2], focusFirst);
}
for (const [btnId, popId, key] of POPOVERS) {
  const btn = $(btnId), pop = $(popId);
  if (!btn || !pop) continue;
  btn.addEventListener("click", () => {
    if (pop.hidden) hOpenPopover(btnId, popId, key, false);
    else hClosePopovers(false);
  });
  btn.addEventListener("keydown", e => {
    if (e.key === "ArrowDown") { e.preventDefault(); hOpenPopover(btnId, popId, key, true); }
  });
  pop.addEventListener("keydown", e => {
    const items = popoverItems(pop);
    const i = items.indexOf(document.activeElement);
    let next = -1;
    if (e.key === "ArrowDown") next = i < 0 ? 0 : (i + 1) % items.length;
    else if (e.key === "ArrowUp") next = i <= 0 ? items.length - 1 : i - 1;
    else if (e.key === "Home") next = 0;
    else if (e.key === "End") next = items.length - 1;
    else if (e.key === "Escape") { e.preventDefault(); e.stopPropagation(); hClosePopovers(true); return; }
    if (next >= 0 && items[next]) { e.preventDefault(); items[next].focus(); }
  });
  pop.addEventListener("click", e => {
    const item = e.target && e.target.closest ? e.target.closest("button, a[href]") : null;
    if (item && !item.disabled) hClosePopovers(true);
  }, true);
  pop.addEventListener("focusout", e => {
    const to = e.relatedTarget;
    if (to && (pop.contains(to) || to === btn)) return;
    if (to) hClosePopovers(false);
  });
}
document.addEventListener("mousedown", e => {
  const t = e.target;
  for (const [btnId, popId, key] of POPOVERS) {
    const btn = $(btnId), pop = $(popId);
    if (!btn || !pop || pop.hidden) continue;
    if (t && (pop.contains(t) || btn.contains(t))) continue;
    pop.hidden = true;
    btn.setAttribute("aria-expanded", "false");
    FTStore.set({ panels: { [key]: false } });
  }
});

// ---- keyboard (§7) ----
// The shell owns only Alt+P (project), Alt+J (jobs sheet), Alt+R
// (deliverables) and Esc. event.code, so macOS Alt-letters still match;
// AltGr (Windows ctrl+alt) and text fields are left alone. The viewer
// ignores plain-Alt keys, so nothing fires twice. Alt+1…4 are gone.
function isTextField(el) {
  if (!el) return false;
  const tag = (el.tagName || "").toUpperCase();
  if (tag === "TEXTAREA" || tag === "SELECT" || el.isContentEditable) return true;
  if (tag !== "INPUT") return false;
  const type = (el.type || "text").toLowerCase();
  return !["checkbox", "radio", "button", "submit", "reset", "range", "color", "file"].includes(type);
}
function hDialogOpen() { return !$("dlgBackdrop").hidden || !$("pickerBackdrop").hidden; }
function hShellShortcut(e, activeEl) {
  if (!e.altKey || e.ctrlKey || e.metaKey || e.shiftKey) return false;
  if (typeof e.getModifierState === "function" && e.getModifierState("AltGraph")) return false;
  const code = e.code || "";
  if (code !== "KeyP" && code !== "KeyJ" && code !== "KeyR") return false;
  if (isTextField(activeEl) || hDialogOpen()) return false;
  e.preventDefault();
  if (code === "KeyP") {
    if ($("hProjectBtn").disabled) toast("열린 프로젝트가 없습니다 — ‘이전 프로젝트’에서 여십시오.", "warn");
    else hTogglePopover("project", true);
  } else if (code === "KeyJ") {
    hToggleSheet("jobs", $("hBtnJobs"));
  } else if (hMode() !== "project") {
    toast("결과물은 프로젝트를 연 뒤에 만들 수 있습니다.", "warn");
  } else {
    hToggleSheet("finalize", $("hBtnFinalize"));
  }
  return true;
}
document.addEventListener("keydown", e => {
  if (hShellShortcut(e, document.activeElement)) return;
  if (e.key === "Tab" && FTStore.state.panels.finalize && !hDialogOpen()) {
    // Deliverables sheet is modal: Tab cycles inside it.
    const sheet = $("hFinalizeSheet");
    const f = hFocusables(sheet);
    if (!f.length) return;
    const first = f[0], last = f[f.length - 1];
    const inside = sheet.contains(document.activeElement);
    if (!inside) { e.preventDefault(); (e.shiftKey ? last : first).focus(); }
    else if (e.shiftKey && document.activeElement === first) { e.preventDefault(); last.focus(); }
    else if (!e.shiftKey && document.activeElement === last) { e.preventDefault(); first.focus(); }
    return;
  }
  // Esc order on the shell side: deliverables sheet, jobs sheet (focus
  // inside), popover, newest toast. Dialogs and the viewer handle theirs
  // first and mark the event handled.
  if (e.key !== "Escape" || e.defaultPrevented || hDialogOpen()) return;
  if (FTStore.state.panels.finalize) {
    const sheet = $("hFinalizeSheet");
    if (sheet.querySelector && sheet.querySelector("[aria-busy='true']")) return;
    e.preventDefault();
    hCloseSheet("finalize", true);
    return;
  }
  if (FTStore.state.panels.jobs && $("hJobsSheet").contains(document.activeElement)) {
    e.preventDefault();
    hCloseSheet("jobs", true);
    return;
  }
  if (POPOVERS.some(([, popId]) => $(popId) && !$(popId).hidden)) { e.preventDefault(); hClosePopovers(true); return; }
  if (dismissNewestToast()) e.preventDefault();
});
