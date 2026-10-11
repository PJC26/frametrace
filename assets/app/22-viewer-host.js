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
const hostViewer = { mounted: false, pending: null, bridged: false, seq: 0 };

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
  api.on("ready", () => hostViewerStore({ mounted: true, saveState: api.saveState() }));
  api.on("destroyed", () => hostViewerStore({ mounted: false, saveState: null, dirty: 0, selection: 0, activeId: null }));
}

function hostViewerWanted() {
  return typeof FTStore !== "object" || !FTStore || typeof FTStore.hasProject !== "function" || FTStore.hasProject();
}

async function hostViewerMount() {
  if (!hostViewerAvailable() || !hostViewerWanted()) return false;
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

function hostViewerUnmount() {
  if (!hostViewerAvailable()) return;
  hostViewer.seq += 1;
  hostViewer.pending = null;
  window.FTViewer.destroy();
  hostViewer.mounted = false;
}

// Project switch without a page reload. Resolves false (and changes
// nothing) when unsaved marks could not be saved — the caller asks the
// examiner (askDialog) before forcing anything. openFn may resolve false
// (the server did not switch); the viewer then stays down and the shell
// decides what to show.
async function hostViewerSwitch(openFn) {
  if (hostViewerAvailable() && hostViewer.mounted) {
    const state = window.FTViewer.saveState();
    if (state && state !== "saved" && state !== "local") {
      const saved = await window.FTViewer.save();
      if (!saved) return false;
    }
    hostViewerUnmount();
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
