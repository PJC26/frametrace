// Hosted viewer mount (docs/ui-rebuild-contract.md §3.1, track T5).
// The shell shows the evidence viewer in its own document: the fragment
// (build.rs include marker "@include:viewer.body") sits inside #hViewerHost and the viewer
// bundle exposes window.FTViewer. This file is the shell's ONLY door to it:
//   hostViewerMount()          init once a project is open
//   hostViewerUnmount()        destroy (markup stays, state + listeners go)
//   hostViewerSwitch(openFn)   save → destroy → openFn() → init (no page reload)
//   hostViewerRefresh(reason)  new case data → FTViewer.reload()
// Viewer events are mirrored into FTStore.state.viewer when the store exists.
// The shell renderer (97-shell.js, via hViewer.mount) decides WHEN to mount;
// this file refuses to mount unless FTStore.hasProject() (an open project
// the examiner has not left for the empty state).
// Everything is inert while the page is not a host (<html data-ft-host>
// missing — e.g. the pre-rebuild iframe shell or FRAMETRACE_UI=legacy).
// No top-level name may start with ft/FT/viewer/loader/live (shared global
// scope with the viewer bundle; scripts/check-js.mjs).
const HOST_VIEWER_NODE = "hViewerHost";
const HOST_VIEWER_EMPTY = "inputCard";
const hostViewer = { mounted: false, pending: null, unmounting: null, bridged: false, seq: 0, stillSaving: false };

function hostViewerAvailable() {
  const html = document.documentElement;
  return !!html && typeof html.hasAttribute === "function" && html.hasAttribute("data-ft-host") &&
    typeof window.FTViewer === "object" && window.FTViewer !== null &&
    !!document.getElementById(HOST_VIEWER_NODE);
}

function hostViewerStore(patch) {
  if (typeof FTStore !== "object" || !FTStore || typeof FTStore.set !== "function") return;
  const current = (FTStore.state && FTStore.state.viewer) || {};
  FTStore.set({ viewer: { ...current, ...patch } });
}

function hostViewerBridge() {
  if (hostViewer.bridged) return;
  hostViewer.bridged = true;
  const api = window.FTViewer;
  api.on("save-state", e => hostViewerStore({ saveState: e.detail.state, dirty: e.detail.pending }));
  api.on("dirty", e => hostViewerStore({ dirty: e.detail.count }));
  api.on("selection", e => hostViewerStore({ selection: e.detail.count }));
  api.on("active-record", e => hostViewerStore({ activeId: e.detail.id }));
  api.on("locale", e => hostViewerStore({ locale: e.detail.locale }));
  api.on("theme", e => hostViewerStore({ theme: e.detail.theme }));
  // Which index generation the mounted data is (live-state, §3.2): the
  // chip / jobs sheet compare it with the status poll's generation and
  // offer '새 데이터 반영' — the viewer itself shows no live chip here.
  api.on("live-state", e => hostViewerStore({ live: !!e.detail.live, liveGeneration: e.detail.generation == null ? null : e.detail.generation, canReload: e.detail.canReload !== false }));
  api.on("ready", () => {
    hostViewerStore({ mounted: true, saveState: api.saveState() });
    hostViewerSeedExaminer(api);
    // A reload() rebuilds the viewer root with its pristine classes: while
    // the pop-out is open the media column must fold again under the note
    // (if the window closed meanwhile, the viewer's "popout" event unfolds).
    if (typeof hSetPopped === "function" && typeof FTStore === "object" && FTStore && FTStore.state.popout.open) {
      hSetPopped(true);
    }
    if (typeof hExaminerRender === "function") hExaminerRender();
  });
  // A join started inside the viewer (FTViewer "derived-job"): the shell
  // polls fast while it runs and offers '새 데이터 반영' once it is done,
  // even when the 5 s idle poll never saw it running (31-progress.js).
  // A save refused with 401 (U8): the shell banner says "save refused —
  // close the other FrameTrace window and reload", not "server down"
  // (40-status.js). It stays while the viewer's saves keep failing — a good
  // /api/status proves nothing in no-token mode — and goes once a save gets
  // through: save-state carries auth on every change of that state.
  // A13: a refused viewer request other than the marks save (export,
  // telemetry, capture …, stage "request") is a "write" refusal — it clears
  // with the next write that gets through, not with a save.
  api.on("error", e => {
    if (!(e.detail && e.detail.auth) || typeof noteAuthExpired !== "function") return;
    noteAuthExpired(e.detail.stage === "request" ? "write" : "save");
  });
  api.on("save-state", e => {
    const d = e.detail || {};
    if (d.auth && typeof noteAuthExpired === "function") noteAuthExpired("save");
    else if (d.auth === false && typeof noteAuthOk === "function") noteAuthOk("save");
  });
  api.on("destroyed", () => { if (typeof noteAuthOk === "function") noteAuthOk("save"); });
  api.on("derived-job", e => { if (typeof noteViewerDerivedJob === "function") noteViewerDerivedJob(e.detail || {}); });
  api.on("destroyed", () => hostViewerStore({ mounted: false, saveState: null, dirty: 0, selection: 0, activeId: null, live: false, liveGeneration: null, canReload: true }));
}

// The hosted selection bar has no 검토자 field (the deliverables sheet's
// #finalExaminer is the one place to type it): a viewer with no examiner
// for this case starts from the examiner name the shell remembers, so new
// marks keep their attribution.
function hostViewerSeedExaminer(api) {
  try {
    if (typeof api.getExaminer !== "function" || String(api.getExaminer() || "").trim()) return;
    const prefs = typeof loadPrefs === "function" ? loadPrefs() : null;
    const name = prefs && typeof prefs.examinerName === "string" ? prefs.examinerName.trim() : "";
    if (name) api.setExaminer(name);
  } catch (e) { /* no stored name: marks carry none until the sheet sets one */ }
}

function hostViewerWanted() {
  return typeof FTStore !== "object" || !FTStore || typeof FTStore.hasProject !== "function" || FTStore.hasProject();
}

async function hostViewerMount() {
  if (!hostViewerAvailable() || !hostViewerWanted()) return false;
  // An unmount still waiting for its save goes first (FTViewer is single-mount).
  if (hostViewer.unmounting) {
    await hostViewer.unmounting;
    if (!hostViewerAvailable() || !hostViewerWanted()) return false;
  }
  if (hostViewer.mounted) return true;
  if (hostViewer.pending) return hostViewer.pending;
  hostViewerBridge();
  const node = document.getElementById(HOST_VIEWER_NODE);
  const empty = document.getElementById(HOST_VIEWER_EMPTY);
  node.hidden = false;
  if (empty) empty.hidden = true;
  const theme = document.documentElement.dataset.theme === "dark" ? "dark" : "light";
  const storeLocale = typeof FTStore === "object" && FTStore && FTStore.state && FTStore.state.viewer ? FTStore.state.viewer.locale : null;
  // An unmount while init is still loading supersedes it (seq).
  const seq = ++hostViewer.seq;
  hostViewer.pending = window.FTViewer.init({ embed: true, host: node, theme, locale: storeLocale || null })
    .then(() => { if (seq !== hostViewer.seq) return false; hostViewer.mounted = true; return true; })
    .catch(err => { console.error("viewer mount failed", err); return false; })
    .finally(() => { hostViewer.pending = null; });
  return hostViewer.pending;
}

// A marks save still in flight is waited for (bounded, FTViewer
// _whenSaveSettled) before the mount goes, so its reply settles the drafts
// in this mount rather than racing the next one. Returns a promise; the
// next hostViewerMount() waits for it. Callers that cannot wait (the store
// renderer) may ignore it: the viewer itself settles a late reply too.
function hostViewerUnmount() {
  if (!hostViewerAvailable()) return Promise.resolve();
  hostViewer.seq += 1;
  hostViewer.pending = null;
  hostViewer.mounted = false;
  if (hostViewer.unmounting) return hostViewer.unmounting;
  const api = window.FTViewer;
  let saving = false;
  try { saving = typeof api._whenSaveSettled === "function" && api.saveState() === "saving"; } catch (e) { saving = false; }
  if (!saving) {
    api.destroy();
    return Promise.resolve();
  }
  hostViewer.unmounting = api._whenSaveSettled(15000)
    .catch(() => false)
    .then(() => { api.destroy(); })
    .finally(() => { hostViewer.unmounting = null; });
  return hostViewer.unmounting;
}

// Project switch without a page reload. Resolves false (and changes
// nothing) when unsaved marks could not be saved — the caller asks the
// examiner (askDialog) before forcing anything. openFn may resolve false
// (the server did not switch); the viewer then stays down and the shell
// decides what to show.
async function hostViewerSwitch(openFn) {
  if (hostViewerAvailable() && hostViewer.mounted) {
    // Same rule as every other shell guard (hViewer.hasUnsaved): edits
    // that live only in this browser ("local" with a pending count) are
    // unsaved too, so they are saved before the viewer goes down.
    const api = window.FTViewer;
    const state = api.saveState();
    const pending = () => { try { return Number((api.summary() || {}).pending) || 0; } catch (e) { return 1; } };
    const unsaved = typeof hViewerStateUnsaved === "function"
      ? hViewerStateUnsaved(state, pending)
      : !!state && state !== "saved" && !(state === "local" && pending() === 0);
    hostViewer.stillSaving = false;
    if (unsaved) {
      const so = { patient: true };
      const saved = await window.FTViewer.save(so); // R8 U2: explicit — waits while the save progresses
      // R9 U6: the caller says "still saving" rather than "not saved".
      if (!saved) { hostViewer.stillSaving = typeof hSaveStillSaving === "function" && hSaveStillSaving(so, api); return false; }
    }
    await hostViewerUnmount();
  }
  const opened = typeof openFn === "function" ? await openFn() : true;
  if (opened !== false && hostViewerAvailable()) await hostViewerMount();
  return true;
}

// Called after a job rewrote the case. "refused" = unsaved marks; the
// viewer has already started a save and told the examiner.
async function hostViewerRefresh(reason) {
  if (!hostViewerAvailable() || !hostViewer.mounted) return "refused";
  return window.FTViewer.reload({ reason: reason || "" });
}
