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
  if (pendingReviewSync || saveInFlight || state.deletedIds.length) return "unsaved";
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
  setLocale: viewerSetLocale,
  getLocale: () => state.locale,
  syncTheme: syncThemeBtn,
  focus: viewerFocus,
  summary: viewerSummary,
  saveState: () => (saveStateEl && saveStateEl.dataset.state) || null,
  // Resolves true once nothing is pending (marks durably queued on the
  // server count as saved: they reach the case DB when the job ends).
  async save() {
    if (!viewerIsServed) return false;
    const idle = () => !pendingReviewSync && !state.deletedIds.length;
    for (let attempt = 0; attempt < 4; attempt++) {
      while (saveInFlight) await new Promise(resolve => setTimeout(resolve, 100));
      if (idle()) return true;
      const result = await syncMarksNow();
      if (!result.ok && !result.queued) return false;
    }
    while (saveInFlight) await new Promise(resolve => setTimeout(resolve, 100));
    return idle();
  },
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
render();
FTViewer._emit("records", { total: records.length, byKind: viewerSummary().byKind, byStatus: viewerSummary().byStatus, generation: DATA.liveIndex ? DATA.liveIndex.generation : null });
FTViewer._emit("ready", { total: records.length, live: !!DATA.liveIndex, generation: DATA.liveIndex ? DATA.liveIndex.generation : null });
}

// Data loading lives in 99-loader.js (outside ftViewerBoot; see its header).
