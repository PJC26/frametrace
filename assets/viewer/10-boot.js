/* FrameTrace Evidence Viewer — serverless single-file review UI.
 * Data arrives via window.__FRAMETRACE_DATA__ (inlined by frametrace make-review).
 * Reviewer state (layout, marks, selection) persists in localStorage per case. */

// One mount of the viewer. Called only through FTViewer.init()
// (00-host.js); everything below 95-init.js runs inside this function, so a
// destroy()/init() pair rebuilds the whole record model from scratch.
//   opts.root      the fragment root (#ftViewer) — state classes live here
//   opts.signal    AbortSignal of this mount: every document/window/channel
//                  listener is registered with it, timers stop on abort
//   opts.hosted    true inside the workstation shell (no iframe)
//   opts.embed     hide the standalone title row
//   opts.data      inline bundle (else loadViewerData() resolves it)
async function ftViewerBoot(opts) {
const ftRoot = opts.root;
const ftSignal = opts.signal;
const ftHosted = !!opts.hosted;
const DATA = opts.data && opts.data.manifest ? opts.data : await loadViewerData({ root: ftRoot, signal: ftSignal, url: opts.url || "" });
// destroy() while the list was still loading: this mount is void.
if (!opts.isCurrent()) return;
const manifest = DATA.manifest || {};
const scan = DATA.scan || {};
// The carve audit log is append-only across runs: a re-carve re-emits
// the same id space, so earlier entries for an id are stale records of a
// superseded run — keep the latest entry per id (the same rule the
// standalone carve report applies) or the grid shows every generation of
// artifacts that ever landed in the log.
const carveLog = [...new Map(
  (Array.isArray(DATA.carveLog) ? DATA.carveLog : [])
    .filter(item => item && item.id)
    .map(item => [String(item.id), item])
).values()];
const filesystemLog = Array.isArray(DATA.filesystemLog) ? DATA.filesystemLog : [];
const validationLog = Array.isArray(DATA.validationLog) ? DATA.validationLog : [];
const anomalyLog = Array.isArray(DATA.anomalyLog) ? DATA.anomalyLog : [];
const anomalyFindings = anomalyLog.filter(item => item && item.kind && item.kind !== "none");
const anomaliesBySelector = new Map();
for (const item of anomalyFindings) {
  const key = String(item.selector || "");
  if (!key) continue;
  const list = anomaliesBySelector.get(key) || [];
  list.push(item);
  anomaliesBySelector.set(key, list);
}
const BS = String.fromCharCode(92);
const EXT_PREFIX = BS + BS + "?" + BS;
const EXT_UNC = EXT_PREFIX + "unc" + BS;
const LAYOUT_KEY = "ft.viewer." + (manifest.case_id || "case") + ".layout.v2";
// MARKS_KEY / NOTES_KEY / TAGS_KEY: pre-v2 whole-map stores, no longer read or
// written (drafts are per id, §10.3). ANNOTATIONS_KEY: the pre-§10.3 single-key
// drafts, migrated once at boot (30-records migrateLegacyDrafts).
const MARKS_KEY = "ft.viewer." + (manifest.case_id || "case") + ".marks";
const NOTES_KEY = "ft.viewer." + (manifest.case_id || "case") + ".notes";
const ANNOTATIONS_KEY = "ft.viewer." + (manifest.case_id || "case") + ".annotations.v1";
const RANGES_KEY = "ft.viewer." + (manifest.case_id || "case") + ".ranges";
const PROXIES_KEY = "ft.viewer." + (manifest.case_id || "case") + ".proxies";
const EXAMINER_KEY = "ft.viewer." + (manifest.case_id || "case") + ".examiner";
const MARK_STATUSES = ["reviewed", "important", "needs_verification"];
// Tag presets are the quick-apply taxonomy for the tag menu, the tag
// filter, the detail editor, and the popup player. The defaults cover
// any case type — accident, assault, fraud, missing-person, digital
// evidence — with the traffic set kept at the end for dashcam cases.
// Examiners can add/remove presets in the tag menu; the list is stored
// globally (not per case) because an investigator's tag vocabulary is
// personal, while applied tags stay per case (case DB + per-id drafts).
const DEFAULT_TAG_PRESETS = ["핵심증거", "현장", "관련인", "동선·위치", "시간대확인", "조작의심", "출처불명", "참고자료", "보존요청", "제외", "사고", "과속", "신호위반", "차선변경", "보행자", "음주의심"];
const TAG_PRESETS_KEY = "ft.viewer.tag_presets";
const TAGS_KEY = "ft.viewer." + (manifest.case_id || "case") + ".tags";
const LOCALE_KEY = "ft.viewer." + (manifest.case_id || "case") + ".locale";
