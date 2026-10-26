// Shell store (docs/ui-rebuild-contract.md §4). One plain object holds
// what the shell renders from; writers call FTStore.set(patch) and every
// renderer listens to the single "change" event, which carries the set of
// changed keys ("phase", "project", "project.caseDir", …).
//
// Writers: the status poll (/api/status, 40-status.js), the
// viewer bridge (viewer.*), and 97-shell.js — the only writer of panels.*.
//
// T2 addition (additive to §4): ui.home — the examiner asked for the empty
// state although the server still has a case bound (restored session, or
// "새 증거 넣기"). FTStore.hasProject() honours it, so anything that mounts
// the viewer on "a project is open" must ask hasProject(), not phase.
const H_REVIEW_PHASES = ["review-partial", "review-ready", "done", "finalized"];
const FTStore = (() => {
  const state = {
    project: { caseDir: "", name: "", packageDir: "", packageIncomplete: false, restored: false, sourcePath: "" },
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
    viewer: { mounted: false, saveState: null, dirty: 0, selection: 0, activeId: null, locale: "ko", theme: "light", live: false, liveGeneration: null, canReload: true },
    server: { instance: "", lost: false, auth: false, failures: 0 },
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

// R20 U5: a trailing `\.` is the folder itself (`E:\.` → "E:", never ".").
// R21 U2 (follow-up): strip trailing separators and single `.` steps with
// a loop — linear time (the regex form was quadratic on long input). Same
// result as /[\\/](?:\.?[\\/])*\.?$/ ; a `..` is kept.
function hTrimTail(text) {
  const s = String(text || "");
  const sep = (c) => c === "\\" || c === "/";
  let end = s.length;
  for (;;) {
    if (end > 0 && sep(s[end - 1])) { end--; continue; }
    if (end > 1 && s[end - 1] === "." && sep(s[end - 2])) { end--; continue; }
    break;
  }
  return s.slice(0, end);
}
function hBaseName(dir) {
  return hTrimTail(dir).split(/[\\/]/).filter(Boolean).pop() || "";
}
// The name a case is shown by: a given name unless it is "." / "..".
function hCaseName(name, dir) {
  const n = String(name || "").trim();
  return n && n !== "." && n !== ".." ? n : hBaseName(dir) || String(dir || "");
}


// ---- the one door to the review viewer ----
// The viewer fragment lives in this document (#hViewerHost) and the viewer
// bundle exposes window.FTViewer (docs/ui-rebuild-contract.md §3). Mounting
// goes through 22-viewer-host.js (hostViewerMount / hostViewerUnmount /
// hostViewerSwitch), which only mounts while FTStore.hasProject(). Shell
// code talks to the viewer only through hViewer — never through viewer ids.
function hViewerApi() {
  const v = typeof window !== "undefined" ? window.FTViewer : null;
  return v && typeof v.init === "function" ? v : null;
}
// saveState → unsaved? pendingCount() is only asked for "local".
function hViewerStateUnsaved(ss, pendingCount) {
  // readonly (A3: annotations unreadable, saving blocked) and unknown (A5:
  // conflict lookup failed) are never "saved" either.
  if (ss === "pending" || ss === "saving" || ss === "failed" || ss === "queued" || ss === "readonly" || ss === "unknown") return true;
  return ss === "local" && (typeof pendingCount === "function" ? pendingCount() : 0) > 0;
}
// R9 U6: a patient save ({ patient: true }) that answered false only
// because its time ran out while the request is still on the wire is
// "아직 저장 중", not a failed save: `opts.timedOut` + saveState "saving".
const H_STILL_SAVING = "아직 저장 중입니다 — 끝나면 저장 상태가 ‘저장됨’으로 바뀝니다. 잠시 뒤 다시 시도하십시오.";
function hSaveStillSaving(opts, api) {
  if (!opts || !opts.timedOut) return false;
  try { return !!api && api.saveState() === "saving"; } catch (e) { return false; }
}
let hHostedWired = false;
// A mount that failed is not retried for the same case on every render
// (that would spin); opening another case — or this one again — retries.
let hMountFailedFor = null;
const hViewer = {
  hosted() { return !!hViewerApi(); },
  mounted() { return !!FTStore.state.viewer.mounted; },
  mount() {
    if (FTStore.state.viewer.mounted || !hViewerApi()) return;
    const caseDir = FTStore.state.project.caseDir;
    if (hMountFailedFor !== null && hMountFailedFor === caseDir) return;
    hWireHostedViewer(hViewerApi());
    // Optimistic: the renderer must not ask twice while init runs; the
    // viewer's ready/destroyed events keep the flag honest afterwards.
    FTStore.set({ viewer: { mounted: true } });
    Promise.resolve(hostViewerMount()).then(ok => {
      if (ok) { hMountFailedFor = null; return; }
      FTStore.set({ viewer: { mounted: false } });
      if (!FTStore.hasProject()) return;
      hMountFailedFor = caseDir;
      toast("검토 화면을 열지 못했습니다 — 페이지를 새로 고치거나 케이스를 다시 여십시오.", "err");
    });
  },
  // The examiner asked for a case again (switch / reopen): try anew.
  retry() { hMountFailedFor = null; },
  unmount() {
    if (!FTStore.state.viewer.mounted) return;
    try { hostViewerUnmount(); } catch (e) { console.error("viewer unmount failed:", e); }
    FTStore.set({ viewer: { mounted: false, saveState: null, dirty: 0, selection: 0, activeId: null }, popout: { open: false } });
  },
  // "saved"|"pending"|"saving"|"failed"|"queued"|"readonly"|"unknown"|"local"|null (null = no viewer).
  saveState() {
    const v = hViewerApi();
    if (!v || !FTStore.state.viewer.mounted) return null;
    try { return v.saveState() || null; } catch (e) { return null; }
  },
  // The one "unsaved" rule for every shell guard (project switch, leaving,
  // quit): anything not yet in the case DB — including edits that so far
  // live only in this browser ("local" with a pending count).
  hasUnsaved() {
    return hViewerStateUnsaved(this.saveState(), () => {
      const s = this.summary();
      return Math.max(Number(FTStore.state.viewer.dirty) || 0, Number(s && s.pending) || 0);
    });
  },
  // Starts a save now; true when a viewer took the request.
  save() {
    const v = hViewerApi();
    if (!v || !FTStore.state.viewer.mounted) return false;
    try { Promise.resolve(v.save({ patient: true })).catch(() => {}); return true; } catch (e) { return false; }
  },
  summary() {
    const v = hViewerApi();
    if (!v || !FTStore.state.viewer.mounted) return null;
    try { return v.summary() || null; } catch (e) { return null; }
  },
  setTheme(t) { const v = hViewerApi(); try { if (v) v.setTheme(t); } catch (e) {} },
  setLocale(l) { const v = hViewerApi(); try { if (v) v.setLocale(l); } catch (e) {} },
  getLocale() { const v = hViewerApi(); try { return (v && v.getLocale()) || ""; } catch (e) { return ""; } },
  popout() { const v = hViewerApi(); try { return v && FTStore.state.viewer.mounted ? v.popout() : null; } catch (e) { return null; } },
  closePopout() { const v = hViewerApi(); try { if (v) v.closePopout(); } catch (e) {} },
  isPoppedOut() { const v = hViewerApi(); try { return !!(v && v.isPoppedOut()); } catch (e) { return false; } },
  // popout() on an open pop-out brings that window to the front.
  focusPopout() { return this.isPoppedOut() && !!this.popout(); },
  focus() { const v = hViewerApi(); try { return !!(v && FTStore.state.viewer.mounted && v.focus()); } catch (e) { return false; } },
  // Shell toasts go to the viewer's single toast host while it is mounted
  // (§6.5); false = show it in the shell's own box.
  toast(msg, kind) {
    const v = hViewerApi();
    if (!v || !FTStore.state.viewer.mounted) return false;
    try { v.toast(msg, kind === "err" ? "error" : kind || "info"); return true; } catch (e) { return false; }
  },
};
// Viewer events → store (§3.2) that 22-viewer-host.js does not mirror
// already. Registered once; FTViewer's EventTarget outlives destroy()/init().
function hWireHostedViewer(v) {
  if (hHostedWired || !v || typeof v.on !== "function") return;
  hHostedWired = true;
  const d = e => (e && e.detail) || {};
  v.on("ready", e => FTStore.set({ counts: { indexed: Number(d(e).total) || 0 } }));
  v.on("locale", () => syncLangBtn());
  v.on("popout", e => hSetPopped(!!d(e).open));
  v.on("request-popout", () => hPopoutOpen());
  v.on("error", e => { if (d(e).stage === "load") toast("검토 데이터를 불러오지 못했습니다: " + (d(e).message || ""), "err"); });
  // The viewer's own toasts (a failed save, …) would land in its host
  // inside the inert #main while the modal deliverables sheet is open:
  // take them over (cancelable event, §6.5) into the body-level box.
  v.on("toast", e => {
    if (!FTStore.state.panels.finalize || !e || typeof e.preventDefault !== "function") return;
    e.preventDefault();
    toast(String(d(e).message || ""), d(e).kind === "error" ? "err" : d(e).kind === "warn" ? "warn" : "info");
  });
}
