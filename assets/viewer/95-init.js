const examinerInput = document.getElementById("examinerName");
if (examinerInput) {
  examinerInput.value = state.examiner || "";
  examinerInput.addEventListener("input", () => {
    state.examiner = examinerInput.value;
    storageSet(EXAMINER_KEY, state.examiner);
  });
}
// Embedded mode (hosted in the workstation shell, or the legacy iframe):
// only the title row goes (case title + the viewer's own theme/language
// buttons — the host owns those). The filter bar with its count pills,
// 보기 ▾, ?, the facet rail, grid and media all stay. The state class sits
// on the fragment root (#ftViewer), never on <body>, so a hosting page's
// own body is untouched.
const setEmbed = on => ftRoot.classList.toggle("embed", on);
setEmbed(!!opts.embed || ftHosted);
const exitFsBtn = document.getElementById("btnExitFs");
// UX-06: new case data (a Recover/Carve job rewrote the case) means a fresh
// record model. That is only done when every review change has reached the
// case DB: otherwise the reload is refused (and a save started), so
// annotations are never lost to a refresh. Shared by FTViewer.reload()
// (hosted) and the legacy ft-reload postMessage (standalone iframe).
async function viewerReloadBlocker() {
  if (pendingReviewSync || saveInFlight || state.dirtyIds.size || conflictIds.size) return "unsaved";
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
// U4: what the examiner was looking at survives a reload — filters, search,
// sort/group, selection, the active card, the grid scroll and the undo
// journal (80-review journalExport/journalImport). Only view state: the
// reload itself runs only when nothing is unsaved (viewerReloadBlocker), and
// drafts / dirty ids / conflicts come back from storage through hydration
// as on any boot. Hosted: FTViewer.reload() hands it to the next init()
// (opts.restore); standalone (a page reload): one sessionStorage entry that
// the next boot of the same case consumes.
const VIEW_STASH_KEY = "ft.viewer." + (manifest.case_id || "case") + ".reloadView";
const VIEW_STRING_FIELDS = ["query", "kind", "status", "chip", "calMonth", "sortBy", "groupBy", "dateFrom", "dateTo", "recType", "audio"];
function viewerViewState() {
  const view = { v: 1, caseId: manifest.case_id || "", queryText: els.query ? els.query.value : "" };
  for (const field of VIEW_STRING_FIELDS) view[field] = String(state[field] || "");
  view.activeId = state.activeId;
  view.selectedIds = [...state.selectedIds];
  view.collapsedGroups = [...state.collapsedGroups];
  view.chipsExpanded = !!state.chipsExpanded;
  view.pageSize = state.pageSize;
  view.currentPage = state.currentPage;
  view.scrollTop = els.recordGrid ? els.recordGrid.scrollTop : 0;
  view.journal = journalExport();
  return view;
}
function viewerApplyViewState(view) {
  if (!view || view.v !== 1 || view.caseId !== (manifest.case_id || "")) return false;
  for (const field of VIEW_STRING_FIELDS) if (typeof view[field] === "string") state[field] = view[field];
  if (typeof view.chipsExpanded === "boolean") state.chipsExpanded = view.chipsExpanded;
  if (Number.isFinite(view.pageSize) && view.pageSize > 0) state.pageSize = view.pageSize;
  if (Number.isFinite(view.currentPage) && view.currentPage > 0) state.currentPage = view.currentPage;
  state.selectedIds = new Set((Array.isArray(view.selectedIds) ? view.selectedIds : []).filter(id => recordById.has(id)));
  state.collapsedGroups = new Set((Array.isArray(view.collapsedGroups) ? view.collapsedGroups : []).map(String));
  if (view.activeId != null && recordById.has(view.activeId)) state.activeId = view.activeId;
  // The controls show what the state now filters by.
  if (els.query) els.query.value = typeof view.queryText === "string" ? view.queryText : state.query;
  const setSelect = (el, value) => { if (el) el.value = value; };
  setSelect(els.kind, state.kind);
  setSelect(els.status, state.status);
  setSelect(els.sortBy, state.sortBy);
  setSelect(els.groupBy, state.groupBy);
  setSelect(els.pageSize, String(state.pageSize));
  setSelect(els.dateFrom, state.dateFrom);
  setSelect(els.dateTo, state.dateTo);
  journalImport(view.journal);
  return true;
}
// sessionStorage outlives the page (it stays readable in this tab until
// it is closed): the stash carries no note text. Journal changes keep ids,
// fields and mark / tag / range values only; a change of the note field is
// dropped (Ctrl+Z of a note does not survive a standalone reload).
function stashJournal(journal) {
  if (!journal) return null;
  const strip = side => {
    if (!side || typeof side !== "object") return side;
    const copy = { ...side };
    delete copy.note;
    return copy;
  };
  const plain = stack => (Array.isArray(stack) ? stack : []).map(entry => ({
    label: entry.label,
    activeBefore: entry.activeBefore,
    changes: (entry.changes || [])
      .map(change => ({ id: change.id, fields: (change.fields || []).filter(f => f !== "note"), before: strip(change.before), after: strip(change.after) }))
      .filter(change => change.fields.length)
  })).filter(entry => entry.changes.length);
  return { undo: plain(journal.undo), redo: plain(journal.redo) };
}
function viewerStashViewState() {
  let view = null;
  try { view = viewerViewState(); view.journal = stashJournal(view.journal); } catch (e) { return; }
  try { sessionStorage.setItem(VIEW_STASH_KEY, JSON.stringify(view)); return; } catch (e) { /* quota: retry without the journal */ }
  try { view.journal = null; sessionStorage.setItem(VIEW_STASH_KEY, JSON.stringify(view)); } catch (e) { /* storage blocked: a plain reload */ }
}
function viewerTakeStashedViewState() {
  let raw = null;
  try { raw = sessionStorage.getItem(VIEW_STASH_KEY); } catch (e) { raw = null; }
  // Removed on every read, whether or not it is used (F8: no stale copy).
  try { sessionStorage.removeItem(VIEW_STASH_KEY); } catch (e) { /* storage blocked */ }
  if (!raw) return null;
  try { return JSON.parse(raw); } catch (e) { return null; }
}
function viewerRefuseReload(blocker) {
  if (blocker === "unsaved" && viewerIsServed && !saveInFlight) syncMarksNow();
  toast(t("reload.refused"));
}
function viewerSetLocale(locale) {
  if (locale !== "ko" && locale !== "en") return;
  if (state.locale === locale) return;
  state.locale = locale;
  storageSet(LOCALE_KEY, state.locale);
  render();
  syncThemeBtn();
  FTViewer._emit("locale", { locale });
}
// Legacy iframe protocol (docs/ui-rebuild-contract.md §3.1/§9.3): only a
// standalone page listens — a hosted viewer is driven through FTViewer.
if (!ftHosted) {
  window.addEventListener("message", event => {
    if (!event.data || event.data.type !== "ft-embed") return;
    setEmbed(!!event.data.embed);
    // Framed + embed off = the host fullscreened us: show a way back.
    if (exitFsBtn) exitFsBtn.hidden = event.data.embed || window.self === window.top;
  }, { signal: ftSignal });
  // The legacy shell posts {type:"ft-reload"} and always gets an answer:
  // {type:"ft-reload-ack", reloaded:true|false, reason}.
  window.addEventListener("message", async event => {
    if (!event.data || event.data.type !== "ft-reload") return;
    if (event.source && window.parent !== window && event.source !== window.parent) return;
    const target = event.source || window.parent;
    const answer = (reloaded, reason) => {
      try { target.postMessage({ type: "ft-reload-ack", reloaded, reason: reason || "" }, "*"); } catch (e) { /* host gone */ }
    };
    const blocker = await viewerReloadBlocker();
    if (blocker) {
      viewerRefuseReload(blocker);
      answer(false, blocker);
      return;
    }
    answer(true, "");
    viewerStashViewState();
    location.reload();
  }, { signal: ftSignal });
  // The legacy shell pushes theme / language into the framed viewer. Same
  // source and origin checks as ft-reload; each is acked so the shell can
  // stop retrying (a viewer that loaded late simply misses the first posts).
  window.addEventListener("message", event => {
    const d = event.data;
    if (!d || (d.type !== "ft-locale" && d.type !== "ft-theme")) return;
    if (event.source && window.parent !== window && event.source !== window.parent) return;
    if (event.origin && event.origin !== "null" && location.origin !== "null" && event.origin !== location.origin) return;
    const target = event.source || window.parent;
    const ack = msg => { try { target.postMessage(msg, "*"); } catch (e) { /* host gone */ } };
    if (d.type === "ft-locale") {
      if (d.locale !== "ko" && d.locale !== "en") return;
      viewerSetLocale(d.locale);
      ack({ type: "ft-locale-ack", locale: d.locale });
    } else {
      if (d.theme !== "dark" && d.theme !== "light") return;
      FTViewer._applyTheme(d.theme);
      ack({ type: "ft-theme-ack", theme: d.theme });
    }
  }, { signal: ftSignal });
  if (exitFsBtn) {
    exitFsBtn.addEventListener("click", () => {
      try { window.parent.postMessage({ type: "ft-exit-fullscreen" }, "*"); } catch (e) {}
    });
  }
}
setupHeightSplitter();
setupColumnSplitter();
setupGridDelegation();
setupLayoutChrome();
document.getElementById("btnLang")?.addEventListener("click", () => {
  viewerSetLocale(state.locale === "ko" ? "en" : "ko");
});
// Shared ft-theme key — same choice as the workstation shell (hosted: the
// same document, so one attribute serves both).
const themeBtn = document.getElementById("btnTheme");
function syncThemeBtn() {
  if (!themeBtn) return;
  const dark = document.documentElement.dataset.theme === "dark";
  const lbl = document.getElementById("btnThemeLabel");
  if (lbl) lbl.textContent = dark ? t("theme.light") : t("theme.dark");
}
themeBtn?.addEventListener("click", () => {
  FTViewer._applyTheme(document.documentElement.dataset.theme === "dark" ? "light" : "dark");
});
syncThemeBtn();

// ---- host API (docs/ui-rebuild-contract.md §3) ----------------------------
function viewerSummary() {
  const base = window.__frametraceSummary();
  return { ...base, pending: pendingSaveCount() };
}
function viewerFocus() {
  const card = state.activeId != null
    ? els.recordGrid.querySelector(`.card[data-id="${CSS.escape(state.activeId)}"]`)
    : null;
  const el = card || els.recordGrid;
  if (!el) return false;
  if (!card && !el.hasAttribute("tabindex")) el.setAttribute("tabindex", "-1");
  el.focus();
  return document.activeElement === el;
}
FTViewer._attach({
  reloadBlocker: viewerReloadBlocker,
  refuseReload: viewerRefuseReload,
  viewState: viewerViewState,
  stashViewState: viewerStashViewState,
  setLocale: viewerSetLocale,
  getLocale: () => state.locale,
  syncTheme: syncThemeBtn,
  focus: viewerFocus,
  summary: viewerSummary,
  saveState: () => (saveStateEl && saveStateEl.dataset.state) || null,
  // Resolves true once nothing is pending (marks durably queued on the
  // server count as saved: they reach the case DB when the job ends).
  // An open save conflict is never "saved": it needs the examiner's choice.
  async save() {
    if (!viewerIsServed) return false;
    const idle = () => reviewIdle();
    for (let attempt = 0; attempt < 4; attempt++) {
      await whenSaveSettled();
      await whenConflictsChecked();
      if (idle()) return true;
      if (!state.dirtyIds.size && !pendingReviewSync && conflictIds.size) return false;
      const result = await syncMarksNow();
      if (!result.ok && !result.queued) return false;
    }
    await whenSaveSettled();
    await whenConflictsChecked();
    return idle();
  },
  // Host unmount (22-viewer-host.js): wait for the request in flight so
  // its reply settles the drafts before the mount goes.
  whenSaveSettled: () => whenSaveSettled(),
  // Same path as typing into the viewer's own 검토자 field (#examinerName).
  setExaminer(name) {
    const value = String(name == null ? "" : name);
    if (examinerInput) {
      examinerInput.value = value;
      examinerInput.dispatchEvent(new Event("input", { bubbles: true }));
    } else {
      state.examiner = value;
      storageSet(EXAMINER_KEY, state.examiner);
    }
  },
  getExaminer: () => state.examiner || "",
  // Host re-push after the examiner name becomes known late (finalize
  // sheet): marks/notes saved without an examiner get this one and go
  // through the normal draft + save path. The server upsert takes the new
  // non-empty examiner over the stored one and logs it in review_mark_history.
  attributeUnassignedMarks(name) {
    FTViewer.setExaminer(name);
    const who = (state.examiner || "").trim();
    if (!who) return 0;
    const ids = new Set([
      ...Object.keys(state.marks),
      ...Object.keys(state.notes).filter(id => (state.notes[id] || "").trim())
    ]);
    let count = 0;
    for (const id of ids) {
      if ((state.examiners[id] || "").trim()) continue;
      touchAnnotation(id);
      count += 1;
    }
    if (count) { try { void syncMarksNow(); } catch (e) { /* retried by autosave */ } }
    return count;
  },
  liveState: () => liveStateDetail(),
  debugStats: () => ({ filterComputes: filterComputeCount, activeId: state.activeId }),
  popout: () => openPlayerWindow(),
  toast: (message, kind) => (kind === "error" ? toastError(message) : toast(message)),
  beforeDestroy() {
    clearTimeout(autosaveTimer);
    if (queueWatchTimer) clearInterval(queueWatchTimer);
    queueWatchTimer = null;
    stopShuttle();
    if (window.__ftBridge === viewerBridge) window.__ftBridge = null;
    if (window.__frametraceSummary === viewerSummaryHook) window.__frametraceSummary = undefined;
  },
});
const viewerSummaryHook = window.__frametraceSummary;
applyLayout();
// U4: a reload brings back what the examiner was looking at (hosted: the
// state FTViewer.reload() captured; standalone: the stashed copy).
// The stash is always consumed (a hosted mount must not leave one behind).
const stashedView = viewerTakeStashedViewState();
const restoredView = opts.restore || stashedView;
const viewRestored = restoredView ? viewerApplyViewState(restoredView) : false;
render();
if (viewRestored && Number.isFinite(restoredView.scrollTop) && restoredView.scrollTop > 0 && els.recordGrid) {
  const top = restoredView.scrollTop;
  els.recordGrid.scrollTop = top;
  // The virtual grid may size its spacer a frame later (ResizeObserver).
  requestAnimationFrame(() => { if (!ftSignal.aborted && els.recordGrid.scrollTop !== top) els.recordGrid.scrollTop = top; });
}
FTViewer._emit("records", { total: records.length, byKind: viewerSummary().byKind, byStatus: viewerSummary().byStatus, generation: DATA.liveIndex ? DATA.liveIndex.generation : null });
FTViewer._emit("ready", { total: records.length, live: !!DATA.liveIndex, generation: DATA.liveIndex ? DATA.liveIndex.generation : null });
// The measured-sound log could not be read: say so once per case and
// error (not on every remount), while the 소리 states read "측정 결과를
// 읽지 못함" for the whole session. Never silent, never a toast storm.
if (audioLevelsError) {
  const noteKey = "ft-audio-err:" + String(DATA.caseDir || "") + "|" + audioLevelsError;
  let noted = false;
  try { noted = sessionStorage.getItem(noteKey) === "1"; sessionStorage.setItem(noteKey, "1"); } catch (e) { noted = false; }
  if (!noted) toastError(tf("audio.readError", { err: audioLevelsError.split(/\r?\n/)[0].slice(0, 200) }));
}
}

// Data loading lives in 99-loader.js (outside ftViewerBoot; see its header).
