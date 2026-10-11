// Shell store (docs/ui-rebuild-contract.md §4). One plain object holds
// what the shell renders from; writers call FTStore.set(patch) and every
// renderer listens to the single "change" event, which carries the set of
// changed keys ("phase", "project", "project.caseDir", …).
//
// Writers: the status poll (/api/status → set via hStoreFromStatus), the
// viewer bridge (viewer.*), and 97-shell.js — the only writer of panels.*.
//
// T2 addition (additive to §4): ui.home — the examiner asked for the empty
// state although the server still has a case bound (restored session, or
// "새 증거 넣기"). FTStore.hasProject() honours it, so anything that mounts
// the viewer on "a project is open" must ask hasProject(), not phase.
const H_REVIEW_PHASES = ["review-partial", "review-ready", "done", "finalized"];
const FTStore = (() => {
  const state = {
    project: { caseDir: "", name: "", packageDir: "", restored: false, sourcePath: "" },
    phase: "empty",
    jobs: {
      pipeline: { steps: [], stepNames: [], current: "", progress: null, enrichment: null, warnings: [], logs: [], busy: false, error: "" },
      caseJob: null,
      clipJobs: [], clipsRunning: 0,
      pendingMarks: 0, jobKind: null,
    },
    counts: { indexed: 0, total: 0, generation: 0 },
    panels: { jobs: false, finalize: false, project: false, tools: false, help: false },
    popout: { open: false, writer: "main" },
    viewer: { mounted: false, saveState: null, dirty: 0, selection: 0, activeId: null, locale: "ko", theme: "light" },
    server: { instance: "", lost: false, failures: 0 },
    env: null,
    ui: { home: false },
  };
  const listeners = new Set();
  const isObj = v => !!v && typeof v === "object" && !Array.isArray(v);
  const same = (a, b) => {
    if (Object.is(a, b)) return true;
    if (!a || !b || typeof a !== "object" || typeof b !== "object") return false;
    try { return JSON.stringify(a) === JSON.stringify(b); } catch (e) { return false; }
  };
  let emitting = 0;
  let pending = null;
  function emit(keys) {
    // A listener that writes again is queued, so every listener sees one
    // consistent state per notification (no re-entrant half-renders).
    if (emitting) { pending = pending || new Set(); keys.forEach(k => pending.add(k)); return; }
    emitting += 1;
    try {
      let batch = keys;
      while (batch) {
        for (const fn of [...listeners]) {
          try { fn({ keys: batch }); } catch (e) { console.error("FTStore listener failed:", e); }
        }
        batch = pending;
        pending = null;
      }
    } finally { emitting -= 1; }
  }
  return {
    state,
    // get("project.caseDir") — dotted path into the state.
    get(path) {
      return String(path || "").split(".").filter(Boolean).reduce((o, k) => (o == null ? undefined : o[k]), state);
    },
    // Shallow merge per top-level key: set({ project: { caseDir } }) keeps
    // the other project fields. Returns the changed key set.
    set(patch) {
      const keys = new Set();
      for (const [k, v] of Object.entries(patch || {})) {
        const cur = state[k];
        if (isObj(cur) && isObj(v)) {
          for (const [k2, v2] of Object.entries(v)) {
            if (same(cur[k2], v2)) continue;
            cur[k2] = v2;
            keys.add(k + "." + k2);
            keys.add(k);
          }
        } else if (!same(cur, v)) {
          state[k] = v;
          keys.add(k);
        }
      }
      if (keys.size) emit(keys);
      return keys;
    },
    on(type, fn) { if (type === "change" && typeof fn === "function") listeners.add(fn); },
    off(type, fn) { if (type === "change") listeners.delete(fn); },
    isBusy() {
      const j = state.jobs;
      return !!(j.pipeline.busy || (j.caseJob && !j.caseJob.outcome && j.caseJob.state !== "done"));
    },
    hasProject() {
      return !!state.project.caseDir && H_REVIEW_PHASES.includes(state.phase) && !state.ui.home;
    },
    canFinalize() {
      return ["review-ready", "done", "finalized"].includes(state.phase) && !this.isBusy();
    },
  };
})();

function hBaseName(dir) {
  return String(dir || "").replace(/[\\/]+$/, "").split(/[\\/]/).filter(Boolean).pop() || "";
}

// Pure: one /api/status reply → an FTStore patch. Server phases are
// idle|running|review-partial|review-ready|finalizing|done|error; the store
// speaks the contract's vocabulary (idle → empty, finalizing → review-ready
// while busy, done + package → finalized).
function hStoreFromStatus(st) {
  const s = st && typeof st === "object" ? st : {};
  const raw = String(s.phase || "idle");
  let phase = raw === "idle" ? "empty" : raw === "finalizing" ? "review-ready" : raw;
  if (raw === "done" && s.package_dir) phase = "finalized";
  if (!s.has_job && !s.case_dir) phase = "empty";
  return {
    phase,
    project: {
      caseDir: s.case_dir || "",
      name: hBaseName(s.case_dir),
      packageDir: s.package_dir || "",
      restored: !!s.restored,
    },
    jobs: {
      pipeline: {
        steps: Array.isArray(s.steps) ? s.steps : [],
        stepNames: Array.isArray(s.step_names) ? s.step_names : [],
        current: s.current || "",
        progress: s.progress || null,
        enrichment: s.enrichment || null,
        warnings: Array.isArray(s.warnings) ? s.warnings : [],
        logs: Array.isArray(s.logs) ? s.logs : [],
        busy: !!s.busy,
        error: s.error || "",
      },
      caseJob: s.job || null,
      clipJobs: Array.isArray(s.clip_jobs) ? s.clip_jobs : [],
      clipsRunning: Number(s.clips_running) || 0,
      pendingMarks: Number(s.pending_marks) || 0,
      jobKind: s.job_kind || null,
    },
    counts: { generation: Number(s.index_generation) || 0 },
    server: { instance: s.instance || "", lost: false, failures: 0 },
  };
}

// ---- the one door to the review viewer ----
// Hosted (the viewer fragment lives in this document, T5): window.FTViewer
// (§3). Until that lands the review page runs in an iframe behind the same
// mount point (60-review.js registers hLegacyViewer). Shell code talks to
// the viewer only through hViewer — never through viewer DOM ids.
function hViewerApi() {
  const v = typeof window !== "undefined" ? window.FTViewer : null;
  if (v && typeof v.init === "function") return v;
  return hLegacyViewer || null;
}
let hHostedWired = false;
// @interim: the legacy iframe adapter, assigned by 60-review.js.
let hLegacyViewer = null;
const hViewer = {
  hosted() { const v = typeof window !== "undefined" ? window.FTViewer : null; return !!(v && typeof v.init === "function"); },
  mounted() { return !!FTStore.state.viewer.mounted; },
  mount(partial) {
    if (FTStore.state.viewer.mounted) return;
    const v = hViewerApi();
    if (!v) return;
    if (!this.hosted()) {
      v.mount(!!partial);
      FTStore.set({ viewer: { mounted: true } });
      return;
    }
    hWireHostedViewer(v);
    FTStore.set({ viewer: { mounted: true } });
    // A host component (22-viewer-host.js) may have mounted it already:
    // init() then refuses with "already mounted", which is fine.
    const refused = e => /already mounted/i.test(String((e && e.message) || e));
    const failed = e => {
      if (refused(e)) return;
      FTStore.set({ viewer: { mounted: false } });
      console.error("viewer init failed:", e);
      toast("검토 화면을 열지 못했습니다: " + ((e && e.message) || e), "err");
    };
    try {
      const p = v.init({ embed: true, host: $("hViewerHost"), theme: hThemeNow(), locale: hLocalePref() || null });
      if (p && typeof p.catch === "function") p.catch(failed);
    } catch (e) { failed(e); }
  },
  unmount() {
    if (!FTStore.state.viewer.mounted) return;
    const v = hViewerApi();
    try { if (v) { if (this.hosted()) v.destroy(); else v.unmount(); } } catch (e) { console.error("viewer unmount failed:", e); }
    FTStore.set({ viewer: { mounted: false, saveState: null, dirty: 0, selection: 0, activeId: null }, popout: { open: false } });
  },
  // "saved"|"pending"|"saving"|"failed"|"queued"|"local"|null (null = no viewer).
  saveState() {
    const v = hViewerApi();
    if (!v || !FTStore.state.viewer.mounted) return null;
    try { return v.saveState() || null; } catch (e) { return null; }
  },
  hasUnsaved() {
    const ss = this.saveState();
    return ss === "pending" || ss === "saving" || ss === "failed" || ss === "queued";
  },
  // Starts a save now; true when a viewer took the request.
  save() {
    const v = hViewerApi();
    if (!v || !FTStore.state.viewer.mounted) return false;
    try {
      const r = v.save();
      if (r && typeof r.catch === "function") { r.catch(() => {}); return true; }
      return !!r;
    } catch (e) { return false; }
  },
  summary() {
    const v = hViewerApi();
    if (!v || !FTStore.state.viewer.mounted) return null;
    try { return v.summary() || null; } catch (e) { return null; }
  },
  // Resolves "reloaded" | "refused" | "none" (nothing mounted).
  async reload(opts) {
    const v = hViewerApi();
    if (!v || !FTStore.state.viewer.mounted) return "none";
    try { return (await v.reload(Object.assign({ keepUnsaved: true }, opts || {}))) || "reloaded"; } catch (e) { return "refused"; }
  },
  setTheme(t) { const v = hViewerApi(); try { if (v && v.setTheme) v.setTheme(t); } catch (e) {} },
  setLocale(l) { const v = hViewerApi(); try { if (v && v.setLocale) v.setLocale(l); } catch (e) {} },
  getLocale() { const v = hViewerApi(); try { return (v && v.getLocale && v.getLocale()) || ""; } catch (e) { return ""; } },
  popout() { const v = hViewerApi(); try { return v && FTStore.state.viewer.mounted ? v.popout() : null; } catch (e) { return null; } },
  closePopout() { const v = hViewerApi(); try { if (v) v.closePopout(); } catch (e) {} },
  isPoppedOut() { const v = hViewerApi(); try { return !!(v && v.isPoppedOut()); } catch (e) { return false; } },
  focusPopout() { const v = hViewerApi(); try { if (v && v.focusPopout) return v.focusPopout(); } catch (e) {} return false; },
  focus() { const v = hViewerApi(); try { return !!(v && FTStore.state.viewer.mounted && v.focus()); } catch (e) { return false; } },
  // Shell toasts go to the viewer's single toast host while it is mounted
  // (§6.5); false = show it in the shell's own box.
  toast(msg, kind) {
    if (!this.hosted() || !FTStore.state.viewer.mounted) return false;
    try { window.FTViewer.toast(msg, kind === "err" ? "error" : kind || "info"); return true; } catch (e) { return false; }
  },
  // Legacy-only: the iframe viewer's examiner field (hosted: the viewer
  // owns that field; the finalize code asks the viewer, not its DOM).
  examinerInput() { const v = hViewerApi(); try { return v && v.examinerInput ? v.examinerInput() : null; } catch (e) { return null; } },
};
// Viewer events → store (§3.2). Registered once; FTViewer's EventTarget
// outlives destroy()/init().
function hWireHostedViewer(v) {
  if (hHostedWired || !v || typeof v.on !== "function") return;
  hHostedWired = true;
  const d = e => (e && e.detail) || {};
  v.on("save-state", e => FTStore.set({ viewer: { saveState: d(e).state || null } }));
  v.on("dirty", e => FTStore.set({ viewer: { dirty: Number(d(e).count) || 0 } }));
  v.on("selection", e => FTStore.set({ viewer: { selection: Number(d(e).count) || 0 } }));
  v.on("active-record", e => FTStore.set({ viewer: { activeId: d(e).id || null } }));
  v.on("records", e => FTStore.set({ counts: { indexed: Number(d(e).total) || 0, generation: Number(d(e).generation) || 0 } }));
  v.on("ready", e => FTStore.set({ counts: { indexed: Number(d(e).total) || 0 } }));
  v.on("locale", e => { FTStore.set({ viewer: { locale: d(e).locale || "ko" } }); syncLangBtn(); });
  v.on("theme", e => FTStore.set({ viewer: { theme: d(e).theme || "light" } }));
  v.on("popout", e => hSetPopped(!!d(e).open));
  v.on("request-popout", () => hPopoutOpen());
  v.on("error", e => { if (d(e).stage === "load") toast("검토 데이터를 불러오지 못했습니다: " + (d(e).message || ""), "err"); });
}
