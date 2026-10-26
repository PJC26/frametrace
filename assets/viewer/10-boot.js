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
// R10 U5: the trust codes of a bundle (data-bundle.js, an inline bundle)
// were computed on the PC that generated it, not on this one — trust texts
// then say "생성 PC 기준" instead of "이 PC" (20-i18n trustI18nKey). Only
// data the loader fetched from this server's API is "live".
const trustFromBundle = !(typeof LIVE_TRUST_DATA === "object" && LIVE_TRUST_DATA.has(DATA));
// R7 (S ↔ U): make-review / records-meta embed `null` for a case.json or
// evidence index that is not valid JSON (util::json_for_script). An empty
// list would read as "no evidence": say the case files were not read
// (90-sync.js showCaseInputProblems, role=alert) — like the review / carve
// pages' "케이스 파일을 읽지 못했습니다".
const caseInputProblems = [];
if (!DATA.manifest || typeof DATA.manifest !== "object" || DATA.manifestUnreadable) caseInputProblems.push("manifest");
if (!DATA.scan || typeof DATA.scan !== "object" || DATA.scanUnreadable) caseInputProblems.push("scan");
const manifest = DATA.manifest && typeof DATA.manifest === "object" ? DATA.manifest : {};
// R8 U1: case.json was not read — the case's identity is unknown, so the
// viewer is read-only (30-records hydrateAnnotations: the R5 read-only
// path) and its storage keys come from the case folder, never "case"
// (00-host.js caseStorageKey).
const caseFileUnreadable = caseInputProblems.includes("manifest");
const CASE_KEY = FTViewer.caseStorageKey(caseFileUnreadable ? null : manifest, DATA.caseDir);
// R20 U6: older keys of this folder whose drafts are moved to CASE_KEY at boot.
const CASE_KEY_ALIASES = FTViewer.caseStorageKeyAliases ? FTViewer.caseStorageKeyAliases(caseFileUnreadable ? null : manifest, DATA.caseDir) : [];
const scan = DATA.scan && typeof DATA.scan === "object" ? DATA.scan : {};
// The carve audit log is append-only across runs: a re-carve re-emits
// the same id space, so earlier entries for an id are stale records of a
// superseded run — keep the latest entry per id (the same rule the
// standalone carve report applies) or the grid shows every generation of
// artifacts that ever landed in the log. R9 U1: an authenticated (or
// legacy / self-consistent) entry is the current one; a later entry this
// PC could not authenticate never replaces it (30-records currentLogEntries).
const carveLog = currentLogEntries(Array.isArray(DATA.carveLog) ? DATA.carveLog : [], item => item.id ? String(item.id) : "",
  DATA.logChains && typeof DATA.logChains === "object" ? DATA.logChains.carveLog : null);
const filesystemLog = Array.isArray(DATA.filesystemLog) ? DATA.filesystemLog : [];
const validationLog = Array.isArray(DATA.validationLog) ? DATA.validationLog : [];
const anomalyLog = Array.isArray(DATA.anomalyLog) ? DATA.anomalyLog : [];
// R10 U3: records-meta's per-lane chain state (60-details logChainItems);
// absent from older servers and bundles.
const logChains = DATA.logChains && typeof DATA.logChains === "object" ? DATA.logChains : null;
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
const LAYOUT_KEY = "ft.viewer." + CASE_KEY + ".layout.v2";
// MARKS_KEY / NOTES_KEY / TAGS_KEY: pre-v2 whole-map stores, no longer read or
// written (drafts are per id, §10.3). ANNOTATIONS_KEY: the pre-§10.3 single-key
// drafts, migrated once at boot (30-records migrateLegacyDrafts).
const MARKS_KEY = "ft.viewer." + CASE_KEY + ".marks";
const NOTES_KEY = "ft.viewer." + CASE_KEY + ".notes";
const ANNOTATIONS_KEY = "ft.viewer." + CASE_KEY + ".annotations.v1";
// RANGES_KEY: the pre-R4 whole-map range store — retired (A2), only read once
// at boot to migrate (30-records migrateLegacyRanges).
const RANGES_KEY = "ft.viewer." + CASE_KEY + ".ranges";
// R5 U3: RANGES_KEY entries of ids this case does not (yet) list are held
// here instead of being dropped with the retired key; every boot re-checks
// them (a growing live index may list the id later).
const RANGES_HELD_KEY = RANGES_KEY + ".held";
const PROXIES_KEY = "ft.viewer." + CASE_KEY + ".proxies";
const EXAMINER_KEY = "ft.viewer." + CASE_KEY + ".examiner";
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
const TAGS_KEY = "ft.viewer." + CASE_KEY + ".tags";
const LOCALE_KEY = "ft.viewer." + CASE_KEY + ".locale";
