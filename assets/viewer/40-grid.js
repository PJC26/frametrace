const els = {
  root: ftRoot,
  caseLine: document.getElementById("caseLine"),
  resultCount: document.getElementById("resultCount"),
  triageStatus: document.getElementById("triageStatus"),
  metricVideos: document.getElementById("metricVideos"),
  metricCarved: document.getElementById("metricCarved"),
  metricVerified: document.getElementById("metricVerified"),
  metricFailed: document.getElementById("metricFailed"),
  metricAnomaly: document.getElementById("metricAnomaly"),
  query: document.getElementById("query"),
  kind: document.getElementById("kind"),
  status: document.getElementById("status"),
  pageSize: document.getElementById("pageSize"),
  presetChips: document.getElementById("presetChips"),
  recordGrid: document.getElementById("recordGrid"),
  prevPage: document.getElementById("prevPage"),
  nextPage: document.getElementById("nextPage"),
  pageStatus: document.getElementById("pageStatus"),
  mediaTitle: document.getElementById("mediaTitle"),
  mediaStatus: document.getElementById("mediaStatus"),
  mediaStage: document.getElementById("mediaStage"),
  detailBadges: document.getElementById("detailBadges"),
  summaryList: document.getElementById("summaryList"),
  metaList: document.getElementById("metaList"),
  validationList: document.getElementById("validationList"),
  selectionBar: document.getElementById("selectionBar"),
  selectionCount: document.getElementById("selectionCount"),
  videoMode: document.getElementById("videoMode"),
  videoZoom: document.getElementById("videoZoom"),
  playRate: document.getElementById("playRate"),
  facetTree: document.getElementById("facetTree"),
  sortBy: document.getElementById("sortBy"),
  groupBy: document.getElementById("groupBy"),
  tagFilter: document.getElementById("tagFilter"),
  dateFrom: document.getElementById("dateFrom"),
  dateTo: document.getElementById("dateTo"),
  dayHistogram: document.getElementById("dayHistogram"),
  periodLabel: document.getElementById("periodLabel"),
  dflSummary: document.getElementById("dflSummary"),
  caseWarnings: document.getElementById("caseWarnings")
};

// [value, label key, secondary] — secondary chips hide behind the
// "더보기" expander; their filters still work via the status/mark selects.
const PRESET_CHIPS = [
  ["", "chip.all"],
  ["kind:recovery", "chip.recovery"],
  ["status:candidate-unvalidated", "chip.candidate"],
  ["mark:important", "chip.markImportant"],
  ["mark:none", "chip.markNone"],
  ["marked:any", "chip.marked", true],
  ["anomaly", "chip.anomaly", true],
  ["warning", "chip.warning", true],
  ["status:validation-failed", "chip.failed", true],
  ["status:duplicate-candidate", "chip.duplicate", true],
  ["mark:reviewed", "chip.markReviewed", true],
];

// PERF-03: the haystack fields and sort keys are immutable per record, so
// they are lowered/collated once instead of on every keystroke and render.
// Intl.Collator replaces per-comparison localeCompare (which re-resolves
// the locale on every call).
const idCollator = new Intl.Collator();
const nameCollator = new Intl.Collator("ko");
function searchHaystack(record) {
  if (record._hay === undefined) {
    record._hay = [record.id, record.name, record.path, record.originalPath, record.parser, record.vendor, record.sha256, record.note, record.status]
      .map(value => String(value ?? "").toLowerCase())
      .join("\u0000");
  }
  return record._hay;
}

// Filter+sort memo. Every input of a non-annotation filter is either an
// immutable record field or one of the state fields in the key, so the
// result can be reused until one of them changes. Chips that read marks,
// notes or tags ("marked:any", "mark:*", "tag:*") also key on
// annotationGen (30-records.js), which every write of state.marks / notes /
// tags bumps (touchAnnotation, draft replay, hydration, server values), so
// j/k under a mark or tag filter reuses the list instead of filtering and
// sorting every record on each key press (U7).
let filteredMemo = { key: null, list: null };
// How many times the full filter + sort actually ran (memo misses): read by
// harnesses through FTViewer._debugStats() (U7 — j/k must not recompute).
var filterComputeCount = 0;
// Every FILTER input (no sort): the facet memo keys on this alone, so a
// new filter dimension added here invalidates both memos and a sort change
// never recomputes the facet counts (no positional slicing of the key).
function filterStateKey(withGen = true) {
  const chip = state.chip || "";
  const annotated = chip === "marked:any" || chip.startsWith("mark:") || chip.startsWith("tag:");
  return [state.kind, state.recType, state.status, chip, state.dateFrom, state.dateTo, state.query, state.audio, annotated && withGen ? "a" + annotationGen : ""].join("\u0001");
}
function filterMemoKey(withGen = true) {
  const base = filterStateKey(withGen);
  return base === null ? null : base + "\u0001" + state.sortBy;
}
// R7 U4: the ids whose annotations changed since the memoised list was
// made (30-records.js bumpAnnotations). Under a mark / tag chip a mark
// used to re-filter and re-sort every record and rebuild the id → index
// map; now only these ids are re-tested against the filter: a record that
// left is cut out of the old list (same order — the sort never reads
// annotations) and the old position map is reused with that shift. A
// record that JOINS the list (its sorted place unknown) or a change of
// everything (hydration, > 256 ids) still runs the full filter + sort.
var filterTouched = new Set();
var filterTouchedAll = true;
function noteFilterTouched(id) {
  if (!filterTouched) return; // boot hydration runs before this file: the first list is a full compute anyway
  if (id == null || filterTouched.size >= 256) { filterTouchedAll = true; filterTouched.clear(); }
  else if (!filterTouchedAll) filterTouched.add(id);
}
function recordPassesFilter(record, chip, query) {
  return passesKind(record) && passesRecType(record) && passesDate(record) && passesAudio(record) && passesRest(record, chip, query);
}
// The new list from `old` when only the touched ids changed, or null (run
// the full filter). Returns `old` itself when no touched id left or joined.
function deriveFilteredList(old) {
  if (filterTouchedAll || !Array.isArray(old)) return null;
  const chip = state.chip || "";
  const query = state.query || "";
  const cut = [];
  for (const id of filterTouched) {
    const record = recordById.get(id);
    const at = indexInList(old, id);
    const passes = !!record && recordPassesFilter(record, chip, query);
    if (at >= 0 && !passes) cut.push(at);
    else if (at < 0 && passes) return null;
  }
  if (!cut.length) return old;
  cut.sort((a, b) => a - b);
  const list = old.slice();
  for (let i = cut.length - 1; i >= 0; i--) list.splice(cut[i], 1);
  indexInListDerive(list, old, cut);
  return list;
}

// R5 U17: position of a record id in a list (the memoised filteredRecords()
// array, the virtual grid's copy). j/k, a click, a mark and every
// gridEnsureActiveVisible used to findIndex() through all 50k records —
// O(n) per key. One id → index map per list (built once, checked against
// the list's length and the entry it points at, so a list changed in place
// is re-indexed rather than misread).
// R7 U4: an entry may be DERIVED from an older list's map (`cut`: sorted
// positions, in that map's numbering, cut out since) — a position is the
// mapped one minus the cuts before it, checked against the entry it points
// at like any other.
function indexInList(list, id) {
  if (!Array.isArray(list) || id == null) return -1;
  const listIndexCache = indexInList.cache || (indexInList.cache = new WeakMap()); // self-contained (VM harness)
  let entry = listIndexCache.get(list);
  if (!entry || entry.len !== list.length) {
    const map = new Map();
    for (let i = 0; i < list.length; i++) if (list[i] && !map.has(list[i].id)) map.set(list[i].id, i);
    entry = { len: list.length, map, cut: null };
    listIndexCache.set(list, entry);
  }
  let at = entry.map.get(id);
  if (at === undefined) return -1;
  if (entry.cut) {
    const cut = entry.cut;
    let lo = 0, hi = cut.length;
    while (lo < hi) { const mid = (lo + hi) >> 1; if (cut[mid] < at) lo = mid + 1; else hi = mid; }
    if (lo < cut.length && cut[lo] === at) return -1; // cut out of this list
    at -= lo;
  }
  if (list[at] && list[at].id === id) return at;
  listIndexCache.delete(list);
  return list.findIndex(record => record && record.id === id);
}
// R7 U4: `list` = `old` minus the records at positions `cutNow` (old's
// numbering, sorted) — it reuses old's map instead of building its own.
// Past 512 accumulated cuts the next lookup rebuilds the map once instead.
function indexInListDerive(list, old, cutNow) {
  const listIndexCache = indexInList.cache || (indexInList.cache = new WeakMap());
  const base = listIndexCache.get(old);
  if (!base || base.len !== old.length) return;
  let cut = cutNow;
  if (base.cut) {
    // old's position → the base map's position (the pos-th kept one).
    const toBase = (pos) => {
      let q = pos, k = 0;
      for (;;) {
        while (k < base.cut.length && base.cut[k] <= q) k++;
        if (pos + k === q) return q;
        q = pos + k;
      }
    };
    cut = base.cut.concat(cutNow.map(toBase)).sort((a, b) => a - b);
  }
  if (cut.length > 512) return;
  listIndexCache.set(list, { len: list.length, map: base.map, cut });
}
function filteredRecords() {
  const key = filterMemoKey();
  if (key !== null && filteredMemo.key === key) {
    filterTouched.clear();
    filterTouchedAll = false;
    return filteredMemo.list;
  }
  filterComputeCount += 1;
  const base = filterMemoKey(false);
  const sameFilter = key !== null && Array.isArray(filteredMemo.list) && filteredMemo.base === base;
  const list = (sameFilter && deriveFilteredList(filteredMemo.list)) || computeFilteredRecords();
  filterTouched.clear();
  filterTouchedAll = false;
  filteredMemo = { key, base, list };
  return list;
}

// Per-dimension predicates. Facet counts reuse them to count a record
// against every filter EXCEPT the dimension being counted, so picking a
// day does not collapse the calendar to that one day (UX-22).
function passesKind(record) { return !state.kind || record.kind === state.kind; }
function passesRecType(record) { return !state.recType || (record.recType || "unclassified") === state.recType; }
function passesAudio(record) { return !state.audio || record.audioFacet === state.audio; }
function passesDate(record) {
  if (!state.dateFrom && !state.dateTo) return true;
  if (!record.recDay) return false;
  if (state.dateFrom && record.recDay < state.dateFrom) return false;
  if (state.dateTo && record.recDay > state.dateTo) return false;
  return true;
}
function passesRest(record, chip, query) {
  if (state.status && record.status !== state.status) return false;
  if (chip === "anomaly" && !record.hasAnomaly) return false;
  if (chip === "warning" && !(record.warnings || []).length) return false;
  if (chip === "kind:recovery" && record.kind !== "candidate" && record.kind !== "filesystem") return false;
  if (chip === "marked:any" && !state.marks[record.id] && !(state.tags[record.id] || []).length && !(state.notes[record.id] || "").trim()) return false;
  if (chip.startsWith("tag:")) {
    const tags = state.tags[record.id] || [];
    if (!tags.includes(chip.slice(4))) return false;
  }
  if (chip.startsWith("mark:")) {
    const wanted = chip.slice(5);
    const mark = state.marks[record.id];
    if (wanted === "none") { if (mark) return false; }
    else if (!mark || mark.status !== wanted) return false;
  }
  return !query || searchHaystack(record).includes(query);
}

// Counts for the calendar and the facet tree, each excluding its own
// dimension's filter. One pass; memoised on the filter key minus sort,
// and fetched once per render() and passed to both renderers.
let facetMemo = { key: null, value: null, gen: 0 };
function facetCounts() {
  const base = filterStateKey();
  const key = base === null ? null : base + "\u0001" + records.length;
  if (key !== null && facetMemo.key === key) return facetMemo.value;
  const chip = state.chip || "";
  const query = state.query || "";
  const typeCounts = new Map(), kindCounts = new Map(), dayCounts = new Map(), audioCounts = new Map();
  records.forEach(record => {
    if (!passesRest(record, chip, query)) return;
    const k = passesKind(record), ty = passesRecType(record), d = passesDate(record), a = passesAudio(record);
    if (k && d && a) { const tk = record.recType || "unclassified"; typeCounts.set(tk, (typeCounts.get(tk) || 0) + 1); }
    if (ty && d && a) kindCounts.set(record.kind, (kindCounts.get(record.kind) || 0) + 1);
    if (k && ty && a && record.recDay) dayCounts.set(record.recDay, (dayCounts.get(record.recDay) || 0) + 1);
    if (k && ty && d) audioCounts.set(record.audioFacet, (audioCounts.get(record.audioFacet) || 0) + 1);
  });
  const value = { typeCounts, kindCounts, dayCounts, audioCounts };
  facetMemo = { key, value, gen: facetMemo.gen + 1 };
  return value;
}

function computeFilteredRecords() {
  const chip = state.chip || "";
  const query = state.query || "";
  const list = records.filter(record =>
    passesKind(record) && passesRecType(record) && passesDate(record) && passesAudio(record) && passesRest(record, chip, query));
  const byId = (a, b) => idCollator.compare(String(a.id), String(b.id));
  switch (state.sortBy) {
    case "time-desc":
      list.sort((a, b) => (b.recTime ?? -Infinity) - (a.recTime ?? -Infinity) || byId(a, b));
      break;
    case "time-asc":
      list.sort((a, b) => (a.recTime ?? Infinity) - (b.recTime ?? Infinity) || byId(a, b));
      break;
    case "name":
      list.sort((a, b) => nameCollator.compare(String(a.name), String(b.name)) || byId(a, b));
      break;
    case "size-desc":
      list.sort((a, b) => (b.size || 0) - (a.size || 0) || byId(a, b));
      break;
    default:
      list.sort(byId);
  }
  return list;
}

function groupKeyFor(record) {
  switch (state.groupBy) {
    case "day": return record.recDay || t("group.noTime");
    case "kind": return ({ video: t("filter.kind.video"), carved: t("filter.kind.carved"), filesystem: t("filter.kind.filesystem"), candidate: t("filter.kind.candidate"), joined: t("filter.kind.joined") })[record.kind] || record.kind;
    case "recType": return recTypeLabel(record.recType);
    case "status": return statusLabel(record.status);
    case "mark": return markOf(record) ? markLabel(markOf(record).status) : t("group.noMark");
    case "prefix": return record.prefix;
    case "channel": return record.channel || t("group.noChannel");
    default: return "";
  }
}

function selectedRecord() { return state.activeId == null ? undefined : recordById.get(state.activeId); }
function markOf(record) { return state.marks[record.id] || null; }

function saveLayout() {
  const { videoMode, videoZoom, theater, dual, stageH, colSplit, rate } = state.layout;
  storageSet(LAYOUT_KEY, { videoMode, videoZoom, theater, dual, stageH, colSplit, rate });
}

let layoutSaveTimer = null;
function saveLayoutSoon() {
  clearTimeout(layoutSaveTimer);
  layoutSaveTimer = setTimeout(saveLayout, 250);
}

function applyLayout() {
  els.root.classList.toggle("theater", !!state.layout.theater);
  // The stage is 16:9 of the media column by default (CSS aspect-ratio);
  // a dragged #hsplitter height pins it.
  const h = Number(state.layout.stageH) || 0;
  els.mediaStage.style.height = h >= STAGE_MIN ? h + "px" : "";
  applyColumnSplit();
  applyVideoScale();
  els.videoMode.value = state.layout.videoMode;
  els.videoZoom.value = String(state.layout.videoZoom);
  els.playRate.value = String(state.layout.rate || 1);
  const rateVideo = els.mediaStage.querySelector("video");
  if (rateVideo) rateVideo.playbackRate = state.layout.rate || 1;
  document.getElementById("btnDual")?.classList.toggle("on", !!state.layout.dual);
}

// Column split between the browse pane and the media pane. The default lives
// in CSS (grid fills, media 40% / min 420px); a dragged or keyed value is
// stored as the browse pane percentage of .main (28–72).
const COL_MIN = 28, COL_MAX = 72;
// #hsplitter drag range for the video stage height (px).
const STAGE_MIN = 120;
const stageMax = () => Math.max(STAGE_MIN, window.innerHeight - 320);
function applyColumnSplit() {
  const main = ftRoot.querySelector(".main");
  const pct = Number(state.layout.colSplit) || 0;
  if (pct >= COL_MIN && pct <= COL_MAX) {
    main.style.gridTemplateColumns = `minmax(0, 1fr) 6px minmax(min(420px, 55%), ${(100 - pct).toFixed(2)}%)`;
  } else {
    main.style.gridTemplateColumns = "";
  }
  syncSplitterAria();
}

// aria-valuenow follows the rendered split (browse % of .main).
function syncSplitterAria() {
  const splitter = document.getElementById("vsplitter");
  const main = ftRoot.querySelector(".main");
  const browse = ftRoot.querySelector(".browse");
  if (!splitter || !main || !browse) return;
  const w = main.clientWidth;
  const pct = w > 0 ? Math.round((browse.getBoundingClientRect().width / w) * 100) : Math.round(Number(state.layout.colSplit) || 60);
  splitter.setAttribute("aria-valuenow", String(Math.max(COL_MIN, Math.min(COL_MAX, pct || 60))));
  const hs = document.getElementById("hsplitter");
  if (hs && els.mediaStage) {
    hs.setAttribute("aria-valuenow", String(Math.round(els.mediaStage.offsetHeight || 0)));
    hs.setAttribute("aria-valuemin", String(STAGE_MIN));
    hs.setAttribute("aria-valuemax", String(stageMax()));
  }
}

function setupColumnSplitter() {
  const splitter = document.getElementById("vsplitter");
  const main = ftRoot.querySelector(".main");
  let dragging = false;
  splitter.addEventListener("pointerdown", event => {
    dragging = true;
    splitter.classList.add("dragging");
    splitter.setPointerCapture(event.pointerId);
    event.preventDefault();
  });
  splitter.addEventListener("pointermove", event => {
    if (!dragging) return;
    const rect = main.getBoundingClientRect();
    const pct = ((event.clientX - rect.left) / rect.width) * 100;
    state.layout.colSplit = Math.max(COL_MIN, Math.min(COL_MAX, pct));
    applyColumnSplit();
  });
  const end = () => {
    if (!dragging) return;
    dragging = false;
    splitter.classList.remove("dragging");
    saveLayout();
  };
  splitter.addEventListener("pointerup", end);
  splitter.addEventListener("dblclick", () => {
    state.layout.colSplit = 0;
    saveLayout();
    applyLayout();
  });
  // Keyboard: ←/→ 2%, Shift ×5, Home/End to the limits, Enter resets.
  // preventDefault keeps the global ←/→ video seek from also firing.
  splitter.addEventListener("keydown", event => {
    const now = Number(splitter.getAttribute("aria-valuenow")) || 60;
    const step = event.shiftKey ? 10 : 2;
    let next = null;
    if (event.key === "ArrowLeft") next = now - step;
    else if (event.key === "ArrowRight") next = now + step;
    else if (event.key === "Home") next = COL_MIN;
    else if (event.key === "End") next = COL_MAX;
    else if (event.key === "Enter" && !imeComposing(event)) { event.preventDefault(); state.layout.colSplit = 0; saveLayout(); applyLayout(); return; }
    if (next === null) return;
    event.preventDefault();
    event.stopPropagation();
    state.layout.colSplit = Math.max(COL_MIN, Math.min(COL_MAX, next));
    applyColumnSplit();
    saveLayoutSoon();
  });
}

function setupHeightSplitter() {
  const splitter = document.getElementById("hsplitter");
  const stage = els.mediaStage;
  let startY = 0;
  let startH = 0;
  let dragging = false;
  splitter.addEventListener("pointerdown", event => {
    dragging = true;
    startY = event.clientY;
    startH = stage.offsetHeight;
    splitter.classList.add("dragging");
    splitter.setPointerCapture(event.pointerId);
    event.preventDefault();
  });
  splitter.addEventListener("pointermove", event => {
    if (!dragging) return;
    state.layout.stageH = Math.max(STAGE_MIN, Math.min(stageMax(), startH + event.clientY - startY));
    applyLayout();
  });
  const end = () => {
    if (!dragging) return;
    dragging = false;
    splitter.classList.remove("dragging");
    saveLayout();
  };
  splitter.addEventListener("pointerup", end);
  splitter.addEventListener("dblclick", () => {
    state.layout.stageH = 0;
    saveLayout();
    applyLayout();
  });
  splitter.addEventListener("keydown", event => {
    const step = event.shiftKey ? 80 : 20;
    const cur = stage.offsetHeight;
    let next = null;
    if (event.key === "ArrowUp") next = cur - step;
    else if (event.key === "ArrowDown") next = cur + step;
    else if (event.key === "Enter" && !imeComposing(event)) { event.preventDefault(); state.layout.stageH = 0; saveLayout(); applyLayout(); return; }
    if (next === null) return;
    event.preventDefault();
    event.stopPropagation();
    state.layout.stageH = Math.max(STAGE_MIN, Math.min(stageMax(), next));
    applyLayout();
    saveLayoutSoon();
  });
}

function applyVideoScale() {
  const mode = state.layout.videoMode;
  const zoom = Math.max(30, Math.min(300, Number(state.layout.videoZoom) || 100));
  els.mediaStage.classList.toggle("mode-fit", mode === "fit");
  els.mediaStage.classList.toggle("mode-zoom", mode !== "fit");
  const video = els.mediaStage.querySelector("video");
  if (video && mode !== "fit") {
    const native = video.videoWidth || 1280;
    const width = Math.round(native * zoom / 100);
    video.style.width = Math.min(width, 8000) + "px";
    video.style.height = "auto";
  } else if (video) {
    video.style.width = "";
    video.style.height = "";
  }
}

// Deepfake screening results are fixed at load — count them once.
let dflDistributionCache = null;
function dflDistribution() {
  if (dflDistributionCache) return dflDistributionCache;
  const dist = { high: 0, medium: 0, low: 0, unknown: 0, failed: 0 };
  let screened = 0;
  records.forEach(record => {
    const dfl = record.dfl;
    if (!dfl) return;
    if (dfl.error) { dist.failed += 1; screened += 1; return; }
    const band = String(dfl.band || "unknown");
    if (Object.prototype.hasOwnProperty.call(dist, band)) dist[band] += 1; else dist.unknown += 1;
    screened += 1;
  });
  dflDistributionCache = { dist, screened };
  return dflDistributionCache;
}

// ---- PERF-02: virtual grid -------------------------------------------------
// Only the rows intersecting the viewport (+/- VG_BUFFER_ROWS) exist in the
// DOM, inside a spacer whose height equals the full list, so DOM size is
// constant for 100k+ records. Rows have a fixed height (--card-h, the same
// value contain-intrinsic-size uses) so row positions are pure arithmetic.
// Paging (state.pageSize / prev / next) is kept as the fallback when the grid
// has no layout box (hidden iframe) or when ?grid=paged is requested.
const VG_BUFFER_ROWS = 2;
const VG_MIN_CARD_W = 132;
const VG_GAP = 7;
const VG_HEADER_H = 30;
let vg = null; // current virtual layout, null when the paged path is active
let vgScrollFrame = 0;
let lastGridActive = null;
let gridRenderedOnce = false;

// Intl formatters are expensive to construct; build once, reuse per card.
// Compact, locale-neutral card time (YYYY-MM-DD HH:mm, 24h) so it fits a
// 140px card; the detail panel keeps the full localized timestamp.
const cardTimeFormat = { format: date => { const p = n => String(n).padStart(2, "0"); return `${date.getFullYear()}-${p(date.getMonth() + 1)}-${p(date.getDate())} ${p(date.getHours())}:${p(date.getMinutes())}`; } };

const gridForcedPaging = (() => {
  try { return new URLSearchParams(location.search).get("grid") === "paged"; } catch { return false; }
})();

function virtualGridEnabled() {
  return !gridForcedPaging && els.recordGrid.clientWidth > 0 && els.recordGrid.clientHeight > 0;
}

function gridMetrics() {
  const grid = els.recordGrid;
  const cs = getComputedStyle(grid);
  const inner = grid.clientWidth - (parseFloat(cs.paddingLeft) || 0) - (parseFloat(cs.paddingRight) || 0);
  // Card min width and row height come from CSS so each density keeps the
  // row math exact (--card-min-w / --card-h on .grid).
  const minW = parseFloat(cs.getPropertyValue("--card-min-w")) || VG_MIN_CARD_W;
  const cols = Math.max(1, Math.floor((inner + VG_GAP) / (minW + VG_GAP)));
  const rowH = parseFloat(cs.getPropertyValue("--card-h")) || 188;
  return { cols, rowH };
}

function uniformModel(count, cols, rowH) {
  const stride = rowH + VG_GAP;
  const rowCount = Math.ceil(count / cols);
  return {
    rowCount,
    total: rowCount * stride,
    find: y => Math.max(0, Math.min(rowCount - 1, Math.floor(y / stride))),
    get: i => ({ type: "c", s: i * cols, e: Math.min(count, (i + 1) * cols), top: i * stride, h: rowH }),
    rowOfIndex: idx => Math.floor(idx / cols),
  };
}

function groupedModel(filtered, cols, rowH) {
  const counts = new Map();
  filtered.forEach(record => {
    const key = groupKeyFor(record);
    counts.set(key, (counts.get(key) || 0) + 1);
  });
  const rows = [];
  let y = 0;
  let runStart = -1, runEnd = -1;
  const flush = () => {
    if (runStart < 0) return;
    for (let s = runStart; s < runEnd; s += cols) {
      rows.push({ type: "c", s, e: Math.min(s + cols, runEnd), top: y, h: rowH });
      y += rowH + VG_GAP;
    }
    runStart = -1;
  };
  let lastGroup = null;
  for (let i = 0; i < filtered.length; i++) {
    const record = filtered[i];
    const key = groupKeyFor(record);
    if (key !== lastGroup) {
      lastGroup = key;
      flush();
      rows.push({ type: "h", key, kind: record.kind || "", count: counts.get(key) || 0, collapsed: state.collapsedGroups.has(key), top: y, h: VG_HEADER_H });
      y += VG_HEADER_H + VG_GAP;
    }
    if (state.collapsedGroups.has(key)) continue;
    if (runStart < 0) runStart = i;
    runEnd = i + 1;
  }
  flush();
  const find = target => {
    let lo = 0, hi = rows.length - 1;
    while (lo < hi) {
      const mid = (lo + hi + 1) >> 1;
      if (rows[mid].top <= target) lo = mid; else hi = mid - 1;
    }
    return Math.max(0, lo);
  };
  return {
    rowCount: rows.length,
    total: y,
    find,
    get: i => rows[i],
    rowOfIndex: idx => rows.findIndex(row => row.type === "c" && idx >= row.s && idx < row.e),
  };
}

// `index` is the row's 0-based position in the model: aria-rowindex lets a
// screen reader report "row 412 of 2000" although only a window of rows is
// in the DOM (U14; the grid carries aria-rowcount).
function vgRowElement(row, index) {
  if (row.type === "h") {
    const holder = document.createElement("div");
    holder.innerHTML = groupHeaderHtml(row.key, row.kind, row.count, row.collapsed);
    const el = holder.firstElementChild;
    el.style.top = row.top + "px";
    el.style.height = row.h + "px";
    el.setAttribute("aria-rowindex", String(index + 1));
    return el;
  }
  const html = [];
  for (let i = row.s; i < row.e; i++) html.push(renderCard(vg.filtered[i]));
  const el = document.createElement("div");
  el.className = "vrow";
  el.setAttribute("role", "row");
  el.setAttribute("aria-rowindex", String(index + 1));
  el.style.top = row.top + "px";
  el.style.height = row.h + "px";
  el.innerHTML = html.join("");
  return el;
}

// Create/remove only the rows whose window membership changed. `full`
// discards every painted row (data, selection or layout changed).
function paintVirtual(full) {
  if (!vg) return;
  const grid = els.recordGrid;
  if (full) {
    vg.spacer.textContent = "";
    vg.painted.clear();
  }
  const top = Math.max(0, grid.scrollTop - 2);
  const bottom = top + grid.clientHeight;
  let first = 0, last = -1;
  if (vg.model.rowCount > 0) {
    first = Math.max(0, vg.model.find(top) - VG_BUFFER_ROWS);
    last = Math.min(vg.model.rowCount - 1, vg.model.find(bottom) + VG_BUFFER_ROWS);
  }
  for (const [index, el] of vg.painted) {
    if (index < first || index > last) { el.remove(); vg.painted.delete(index); }
  }
  let prev = null;
  for (let i = first; i <= last; i++) {
    let el = vg.painted.get(i);
    if (!el) {
      el = vgRowElement(vg.model.get(i), i);
      vg.painted.set(i, el);
    }
    const expectedNext = prev ? prev.nextSibling : vg.spacer.firstChild;
    if (el !== expectedNext) vg.spacer.insertBefore(el, expectedNext);
    prev = el;
  }
  vg.first = first;
  vg.last = last;
  syncActiveCard();
}

// The current card carries .active (ring + marker), aria-current and the
// grid's single tab stop. Several modules swap .active directly; this
// re-derives the ARIA side from state.activeId.
function syncActiveCard() {
  els.recordGrid.querySelectorAll(".card.active, .card[aria-current]").forEach(el => {
    if (el.dataset.id !== state.activeId) { el.classList.remove("active"); el.removeAttribute("aria-current"); el.tabIndex = -1; }
  });
  if (state.activeId == null) return;
  const card = els.recordGrid.querySelector(`.card[data-id="${CSS.escape(state.activeId)}"]`);
  if (card) {
    if (!card.classList.contains("active")) card.classList.add("active");
    card.setAttribute("aria-current", "true");
    card.tabIndex = 0;
  }
}

function groupHeaderHtml(key, kind, count, collapsed) {
  const kindAttr = state.groupBy === "kind" ? ` data-gkind="${escapeHtml(kind || "")}"` : "";
  return `<div class="group-header"${kindAttr} data-group="${escapeHtml(key)}" role="row"><div class="gh-cell" role="gridcell" tabindex="0" aria-expanded="${collapsed ? "false" : "true"}"><span>${escapeHtml(key)}</span><span class="muted">${count}${t("unit.count")}${collapsed ? ` · ${t("group.collapsed")}` : ""}</span></div></div>`;
}

// Scroll the active record's row into view (nearest edge), then paint it.
function gridEnsureActiveVisible() {
  if (!vg || state.activeId == null) return;
  const idx = indexInList(vg.filtered, state.activeId);
  if (idx < 0) return;
  const rowIndex = vg.model.rowOfIndex(idx);
  if (rowIndex < 0) return;
  const row = vg.model.get(rowIndex);
  const grid = els.recordGrid;
  const viewTop = grid.scrollTop;
  const viewBottom = viewTop + grid.clientHeight;
  if (row.top < viewTop) grid.scrollTop = row.top;
  else if (row.top + row.h > viewBottom) grid.scrollTop = row.top + row.h - grid.clientHeight + 4;
  paintVirtual(false);
}

function scheduleVirtualPaint() {
  if (vgScrollFrame || !vg) return;
  vgScrollFrame = scheduleFrame(() => { vgScrollFrame = 0; paintVirtual(false); });
}

els.recordGrid.addEventListener("scroll", scheduleVirtualPaint, { passive: true });
if (typeof ResizeObserver === "function") {
  let lastW = -1, lastH = -1;
  new ResizeObserver(() => {
    const w = els.recordGrid.clientWidth, h = els.recordGrid.clientHeight;
    if (w === lastW && h === lastH) return;
    const modeFlip = (lastW <= 0 || lastH <= 0) !== (w <= 0 || h <= 0);
    lastW = w; lastH = h;
    if (!gridRenderedOnce) return;
    if (modeFlip || (vg && gridMetrics().cols !== vg.cols)) requestRender();
    else scheduleVirtualPaint();
  }).observe(els.recordGrid);
}

// j/k and arrow keys step through the whole list; moveActive (80-review.js)
// only knows about pages and painted cards. Pre-select the page that holds
// the next record so it takes its cheap same-page path, then scroll the
// virtual row into view and paint it.
{
  const moveActiveByPage = moveActive;
  moveActive = function moveActiveVirtual(step) {
    if (vg && vg.filtered.length) {
      const index = indexInList(vg.filtered, state.activeId);
      const next = Math.max(0, Math.min(vg.filtered.length - 1, index < 0 ? 0 : index + step));
      state.currentPage = Math.floor(next / state.pageSize) + 1;
    }
    moveActiveByPage(step);
    gridEnsureActiveVisible();
    syncActiveCard();
  };
}

function renderVirtualGrid(filtered) {
  const grid = els.recordGrid;
  const { cols, rowH } = gridMetrics();
  const model = state.groupBy === "none" ? uniformModel(filtered.length, cols, rowH) : groupedModel(filtered, cols, rowH);
  let spacer = vg?.spacer;
  if (!spacer || spacer.parentNode !== grid) {
    grid.textContent = "";
    spacer = document.createElement("div");
    spacer.className = "vspacer";
    // grid > rowgroup > row > gridcell: the spacer is a structural element
    // of the ARIA grid, not a generic wrapper (axe aria-required-children).
    spacer.setAttribute("role", "rowgroup");
    grid.appendChild(spacer);
  }
  grid.classList.add("virtual");
  grid.setAttribute("aria-rowcount", String(model.rowCount));
  grid.style.setProperty("--vcols", String(cols));
  spacer.style.height = model.total + "px";
  vg = { filtered, model, cols, spacer, painted: new Map(), first: 0, last: -1 };
  const activeChanged = state.activeId !== lastGridActive;
  lastGridActive = state.activeId;
  paintVirtual(true);
  // A filter/sort/jump moved the active record: bring it into view.
  if (activeChanged) gridEnsureActiveVisible();
}

// A grid rebuild replaces the card nodes: keyboard focus on a card, a
// group header or the grid itself must not fall to <body> (an autosave
// retry or a resize re-render lands at any time — PR #4 Windows CI).
function gridFocusSnapshot() {
  const el = document.activeElement;
  if (!el || !els.recordGrid || !els.recordGrid.contains(el)) return null;
  const card = el.closest(".card");
  const header = card ? null : el.closest(".group-header");
  return { id: card ? card.dataset.id : null, group: header ? header.dataset.group : null };
}
function gridFocusRestore(snap) {
  if (!snap) return;
  const now = document.activeElement;
  if (now && now !== document.body && now.isConnected) return;
  const grid = els.recordGrid;
  let el = null;
  if (snap.id != null) el = grid.querySelector(`.card[data-id="${CSS.escape(snap.id)}"]`);
  else if (snap.group != null) el = grid.querySelector(`.group-header[data-group="${CSS.escape(snap.group)}"] .gh-cell`);
  if (!el) {
    el = grid;
    if (!el.hasAttribute("tabindex")) el.setAttribute("tabindex", "-1");
  }
  try { el.focus({ preventScroll: true }); } catch (e) { el.focus(); }
}
function renderGridKeepFocus(filtered) {
  const snap = gridFocusSnapshot();
  renderGrid(filtered);
  gridFocusRestore(snap);
}

function renderGrid(filtered) {
  // R6 U8: the id → index map, not a scan of the whole list per render.
  if (indexInList(filtered, state.activeId) < 0) {
    state.activeId = filtered[0]?.id || null;
  }
  gridRenderedOnce = true;
  const virtual = virtualGridEnabled();
  els.root.classList.toggle("vgrid", virtual);
  const pageCount = virtual ? 1 : Math.max(1, Math.ceil(filtered.length / state.pageSize));
  state.currentPage = virtual ? 1 : Math.min(Math.max(1, state.currentPage), pageCount);
  const start = virtual ? 0 : (state.currentPage - 1) * state.pageSize;
  const pageRows = virtual ? filtered : filtered.slice(start, start + state.pageSize);

  els.resultCount.textContent = `${filtered.length}`;
  const rangeEnd = Math.min(filtered.length, start + pageRows.length);
  const rangeStart = filtered.length ? start + 1 : 0;
  els.pageStatus.textContent = `${rangeStart}–${rangeEnd} / ${filtered.length} · ${state.currentPage}/${pageCount}`;
  // R7 U4: the incremental mark tally (30-records.js markTallyNow) — no
  // walk of the mark map on every render (j/k re-render, a click).
  const tally = markTallyNow().byStatus;
  const reviewed = tally.reviewed || 0, important = tally.important || 0, pending = tally.needs_verification || 0;
  // Status texts are plain (non-live) — one polite region (#gridLive)
  // announces the result count, and only when the filter result changes,
  // never on j/k or a mark.
  if (els.triageStatus) {
    els.triageStatus.textContent = tf("triage.line", { done: reviewed, total: records.length, imp: important, pend: pending });
    els.triageStatus.title = els.triageStatus.textContent;
  }
  if (els.dflSummary) {
    const { dist, screened } = dflDistribution();
    els.dflSummary.textContent = screened
      ? tf("dfl.dist", { n: screened, total: records.length, high: dist.high, medium: dist.medium, low: dist.low, unknown: dist.unknown, failed: dist.failed, none: records.length - screened })
      : "";
    els.dflSummary.title = els.dflSummary.textContent;
  }
  announceResultCount(filtered.length);
  els.prevPage.disabled = state.currentPage <= 1;
  els.nextPage.disabled = state.currentPage >= pageCount;
  els.prevPage.title = els.prevPage.disabled ? t("layout.pageFirst") : t("page.prev");
  els.nextPage.title = els.nextPage.disabled ? t("layout.pageLast") : t("page.next");

  if (virtual && filtered.length) {
    renderVirtualGrid(filtered);
    return;
  }
  vg = null;
  els.recordGrid.classList.remove("virtual");
  els.recordGrid.removeAttribute("aria-rowcount");

  const groupCounts = new Map();
  if (state.groupBy !== "none") {
    filtered.forEach(record => {
      const key = groupKeyFor(record);
      groupCounts.set(key, (groupCounts.get(key) || 0) + 1);
    });
  }

  // Paged path: cards stay direct CSS-grid items, but each run of cards is
  // wrapped in a display:contents role=row so the ARIA grid is well formed.
  const cardsHtml = [];
  let run = [];
  const flushRun = () => {
    if (run.length) cardsHtml.push(`<div class="prow" role="row">${run.join("")}</div>`);
    run = [];
  };
  let lastGroup = null;
  pageRows.forEach(record => {
    if (state.groupBy !== "none") {
      const key = groupKeyFor(record);
      if (key !== lastGroup) {
        lastGroup = key;
        const collapsed = state.collapsedGroups.has(key);
        flushRun();
        cardsHtml.push(groupHeaderHtml(key, record.kind, groupCounts.get(key) || 0, collapsed));
        if (collapsed) return;
      }
      if (state.collapsedGroups.has(key)) return;
    }
    run.push(renderCard(record));
  });
  flushRun();
  els.recordGrid.innerHTML = cardsHtml.join("") || `<div class="prow" role="row"><div class="fallback" role="gridcell">${t("grid.empty")}</div></div>`;
}

let lastAnnounced = null;
let announceTimer = 0;
function announceResultCount(n) {
  const live = document.getElementById("gridLive");
  if (!live) return;
  const key = `${n}|${filterMemoKey()}|${state.locale}`;
  if (key === lastAnnounced) return;
  const first = lastAnnounced === null;
  lastAnnounced = key;
  clearTimeout(announceTimer);
  // Debounced so typing in the search box yields one announcement; the
  // initial load is not announced at all.
  if (first) return;
  announceTimer = setTimeout(() => { live.textContent = tf("layout.live", { n }); }, 600);
}

// Sound flag (§2.5): the shape tells the state — measured sound (waves),
// measured silent track (flat line), no audio stream (crossed), stream
// present but not measured (bare speaker), never probed (nothing) — so the
// grid never claims a sound that was not measured. Icon + label, never
// colour alone.
const AUDIO_FLAG_ICON = { sound: "audio", silent: "audio-silent", none: "mute", track: "audio-track" };
function audioFlag(record) {
  // R9 U3: an unknown state signed by another PC's key still says so.
  const icon = AUDIO_FLAG_ICON[record.audioState] || (record.audioPcKey ? "lock" : "");
  if (!icon) return "";
  const tip = escapeHtml(audioTipFor(record));
  return `<span class="audio-flag ${escapeHtml(record.audioState)}" role="img" aria-label="${tip}" title="${tip}">${ftIcon(icon)}</span>`;
}

// Thumbnail URLs that already failed (see the capture-phase error handler
// in 65-actions.js): render the placeholder straight away.
const thumbRetried = new Set();
const thumbDead = new Set();

// R9 U1: the trust of one log entry shown next to it (details lists):
// nothing for authenticated / legacy entries.
function logTrustTagHtml(item) {
  const trust = logTrustOf(item);
  // R11 U2: a usable entry's own note (legacy: `auth` + "레거시") as a tooltip.
  const own = logTrustUsable(trust) ? logTrustNote(item, true) : "";
  if (trust === "sc") return ` <span class="badge candidate"${own ? ` title="${escapeHtml(own)}"` : ""}>${escapeHtml(t("trust.sc"))}</span>`;
  if (own) return ` <span class="trust-info" role="img" title="${escapeHtml(own)}" aria-label="${escapeHtml(own)}">${ftIcon("info")}</span>`;
  if (logTrustUsable(trust)) return "";
  const pc = logTrustPcKey(trust);
  // R10 U3: the server's own reason (trust_note) in the tooltip.
  const note = logTrustNote(item);
  const tip = (pc ? tf("trust.otherPc", { key: pc }) : t("status.tip.unauth")) + (note ? "\n" + note : "");
  return ` <span class="badge candidate" title="${escapeHtml(tip)}">${ftIcon("lock")}${escapeHtml(pc ? t("status.otherPc") : t("trust.unauth"))}</span>`;
}
// R10 U3: the trust_note of an entry this PC could not use, as a detail
// line under it ("" otherwise).
function logTrustNoteHtml(item) {
  const note = logTrustNote(item);
  return note ? `<div class="muted trust-note">${escapeHtml(note)}</div>` : "";
}

// Status is never colour alone: every status badge carries an icon + text.
function statusIcon(status) {
  if (status === "unauthenticated" || status === "auth-other-pc") return "lock";
  if (status === "ffprobe-video-stream-confirmed" || status === "ffprobe-confirmed") return "check";
  if (status === "validation-failed") return "x";
  if (status === "duplicate-candidate") return "copy";
  if (status === "derived") return "clip";
  return "question";
}
// R9 U1: `record` (optional) adds what the status rests on to the tooltip.
function statusBadgeHtml(status, record) {
  return `<span class="badge ${statusClass(status)}" title="${escapeHtml(record ? statusTitle(record) : status)}">${ftIcon(statusIcon(status))}${escapeHtml(statusLabel(status))}</span>`;
}

// While the workstation is still scanning/enriching (live index), a record
// without a thumbnail is most likely waiting for the enrichment step —
// show a "generating" skeleton instead of a final "no thumbnail".
function thumbPending(record) {
  const live = DATA.liveIndex;
  if (!live) return false;
  const phase = String(live.phase || "");
  return (phase === "running" || phase === "review-partial") && record.kind !== "candidate" && record.status !== "validation-failed";
}

function thumbHtml(record, displayName) {
  if (record.thumb && !thumbDead.has(record.thumb)) {
    return `<img src="${escapeHtml(record.thumb)}" alt="${escapeHtml(tf("thumb.alt", { name: displayName }))}" loading="lazy" decoding="async" fetchpriority="low">`;
  }
  if (record.status === "validation-failed") return `<div class="ph">${t("thumb.damaged")}</div>`;
  if (!record.thumb && thumbPending(record)) {
    return `<div class="ph pending" title="${escapeHtml(t("thumb.pendingTip"))}">${escapeHtml(t("thumb.pending"))}</div>`;
  }
  // A URL that failed (twice) can be retried; a record that never had a
  // thumbnail has nothing to retry.
  const retry = record.thumb
    ? `<button type="button" class="quiet" data-thumb-retry="${escapeHtml(record.thumb)}" title="${escapeHtml(t("thumb.retryTip"))}">${escapeHtml(t("thumb.retry"))}</button>`
    : "";
  return `<div class="ph"><span>${t("thumb.missing")}</span>${retry}</div>`;
}

function renderCard(record) {
  const mark = markOf(record);
  const markChip = mark ? markChipHtml(mark.status) : "";
  const staleTag = record.indexStatus === "stale" ? '<span class="muted">(stale)</span>' : "";
  const displayName = record.originalName && record.originalName !== record.name
    ? record.originalName
    : record.name;
  const thumb = thumbHtml(record, displayName);
  const recTypeTag = record.recType ? `<span class="rec-type">${escapeHtml(recTypeLabel(record.recType))}</span>` : "";
  const recTime = record.recTime
    ? record.recSource === "name-date"
      ? record.recDay
      // A device-clock wall time reads as written (no local-Date DST shift).
      : record.recSource === "name" && record.recWall
        ? record.recWall.slice(0, 16).replace("T", " ")
        : cardTimeFormat.format(new Date(record.recTime * 1000))
    : "";
  const tagsHtml = tagListFor(record).length
    ? `<div class="tag-row">${tagListFor(record).map(tag => `<span class="tag-chip">${escapeHtml(tag)}</span>`).join("")}</div>`
    : "";
  const anomalyChip = record.hasAnomaly
    ? `<span class="badge anomaly" title="${escapeHtml(record.anomalies.map(item => item.kind).join(", "))}">${ftIcon("warning")}${escapeHtml(t("header.anomaly"))}</span>`
    : "";
  const kindChip = `<span class="kind-badge kind-${escapeHtml(record.kind || "video")}" title="${t("tree.kind")}">${escapeHtml(KIND_SHORT[record.kind] || record.kind || "?")}</span>`;
  const warnChip = (record.warnings || []).length
    ? `<span class="badge warn" title="${escapeHtml(record.warnings.join("\n"))}">${ftIcon("info")}${tf("warn.count", { n: record.warnings.length })}</span>`
    : "";
  const dflChip = record.dfl && record.dfl.band
    ? `<span class="badge dfl dfl-${escapeHtml(record.dfl.band)}" title="${escapeHtml(tf("dfl.title", { score: record.dfl.score ?? "" }))}">${escapeHtml(tf("dfl.badge", { band: record.dfl.band_label || record.dfl.band }))}</span>`
    : (record.dfl && record.dfl.error
      ? `<span class="badge dfl dfl-low" title="${escapeHtml(tf("dfl.failTitle", { error: record.dfl.error }))}">${escapeHtml(t("dfl.fail"))}</span>`
      : "");
  const active = record.id === state.activeId;
  // role=gridcell (inside a role=row) may contain the selection checkbox and
  // the retry button — no nested-interactive inside an option.
  return `<div class="card ${active ? "active" : ""}" data-id="${escapeHtml(record.id)}" tabindex="${active ? 0 : -1}" role="gridcell" title="${escapeHtml(displayName)}" aria-selected="${state.selectedIds.has(record.id)}"${active ? ' aria-current="true"' : ""}>
    <div class="thumb">${thumb}<input type="checkbox" aria-label="${escapeHtml(t("aria.select"))}: ${escapeHtml(displayName)}" ${state.selectedIds.has(record.id) ? "checked" : ""} data-check="${escapeHtml(record.id)}">${recTypeTag}${kindChip}<span class="dur">${fmtDuration(record.duration)}</span></div>
    <div class="meta">
      <div class="name-row"><span class="name" title="${escapeHtml(displayName)}">${highlightEscape(displayName, state.query)}</span>${record.channel ? `<span class="channel-badge">${escapeHtml(record.channel)}</span>` : ""}</div>
      <div class="time-row"><span class="time-text">${recTime ? escapeHtml(recTime) : t("time.unknown")}</span><span class="sub">${fmtBytes(record.size)}${audioFlag(record)}${staleTag}</span></div>
      <div class="badge-row">${statusBadgeHtml(record.status, record)}${markChip}${anomalyChip}${warnChip}${dflChip}</div>
      ${tagsHtml}
    </div>
  </div>`;
}

// Month-calendar rendering of per-day counts. Each cell shows the day
// number and a count chip; intensity follows the count relative to the
// month's max. Click selects the day as a date filter; clicking the
// selected day clears it. Month nav steps through the recorded span.
function renderHistogram(days) {
  const allDays = [...days.keys()].sort();
  const months = [...new Set(allDays.map(d => d.slice(0, 7)))];
  if (!months.length) {
    els.dayHistogram.innerHTML = `<span class="muted">${t("hist.empty")}</span>`;
    els.periodLabel.textContent = t("hist.none");
    return;
  }
  // Default to the filtered day's month, else the latest recorded month.
  if (!state.calMonth || !months.includes(state.calMonth)) {
    const picked = state.dateFrom?.slice(0, 7);
    state.calMonth = months.includes(picked) ? picked : months[months.length - 1];
  }
  const [cy, cm] = state.calMonth.split("-").map(Number);
  const max = Math.max(1, ...months.includes(state.calMonth)
    ? allDays.filter(d => d.startsWith(state.calMonth)).map(d => days.get(d))
    : [1]);
  const first = new Date(cy, cm - 1, 1);
  const startWeekday = (first.getDay() + 6) % 7; // Monday-first
  const dim = new Date(cy, cm, 0).getDate();
  const loc = state.locale === "en" ? "en-US" : "ko-KR";
  const weekdayNames = Array.from({ length: 7 }, (_, i) =>
    new Date(2024, 0, 1 + i).toLocaleDateString(loc, { weekday: "narrow" })); // 2024-01-01 = Monday
  const cells = weekdayNames.map(w => `<span class="cal-wd">${escapeHtml(w)}</span>`);
  for (let i = 0; i < startWeekday; i++) cells.push(`<span class="cal-day empty"></span>`);
  for (let d = 1; d <= dim; d++) {
    const day = `${state.calMonth}-${String(d).padStart(2, "0")}`;
    const count = days.get(day) || 0;
    const selected = state.dateFrom === day && state.dateTo === day;
    const level = count ? Math.min(4, 1 + Math.floor((count * 3) / max)) : 0;
    cells.push(count
      ? `<button type="button" class="quiet toggle cal-day l${level}${selected ? " selected" : ""}" data-day="${day}" aria-pressed="${selected}" title="${escapeHtml(tf("hist.item", { day, count }))}"><span class="cal-d">${d}</span><span class="sr-only"> · </span><span class="cal-n">${count > 99 ? "99+" : count}</span><span class="sr-only">${escapeHtml(t("unit.count"))}</span></button>`
      : `<span class="cal-day l0${selected ? " selected" : ""}" data-day="${day}"><span class="cal-d">${d}</span></span>`);
  }
  const monthLabel = new Date(cy, cm - 1, 1).toLocaleDateString(loc, { year: "numeric", month: "long" });
  const mi = months.indexOf(state.calMonth);
  const years = [...new Set(months.map(m => m.slice(0, 4)))];
  const yearSel = years.length > 1
    ? `<select id="calYear" class="cal-year" aria-label="${escapeHtml(t("cal.year"))}">${years.map(y => `<option value="${y}"${y === String(cy) ? " selected" : ""}>${y}</option>`).join("")}</select>`
    : "";
  // Fits the 200px rail: ‹ [year] month ›. With a year select the label
  // shows the month only. Disabled arrows say why in their tooltip.
  const shortMonth = yearSel ? new Date(cy, cm - 1, 1).toLocaleDateString(loc, { month: "long" }) : monthLabel;
  const prevTip = mi <= 0 ? t("layout.calPrevNone") : t("cal.prev");
  const nextTip = mi >= months.length - 1 ? t("layout.calNextNone") : t("cal.next");
  const nav = months.length > 1
    ? `<div class="cal-nav">
        <button type="button" class="quiet icon cal-nav-btn cal-prev" id="calPrev" ${mi <= 0 ? "disabled" : ""} title="${escapeHtml(prevTip)}" aria-label="${escapeHtml(prevTip)}">${ftIcon("chevron-right")}</button>
        ${yearSel}<span class="cal-month">${escapeHtml(shortMonth)}</span>
        <button type="button" class="quiet icon cal-nav-btn" id="calNext" ${mi >= months.length - 1 ? "disabled" : ""} title="${escapeHtml(nextTip)}" aria-label="${escapeHtml(nextTip)}">${ftIcon("chevron-right")}</button>
      </div>`
    : `<div class="cal-nav"><span class="cal-month">${escapeHtml(monthLabel)}</span></div>`;
  els.dayHistogram.innerHTML = `<div class="cal">${nav}<div class="cal-grid">${cells.join("")}</div></div>`;
  els.dayHistogram.querySelectorAll("[data-day]").forEach(cell => {
    cell.addEventListener("click", () => {
      const already = state.dateFrom === cell.dataset.day && state.dateTo === cell.dataset.day;
      state.dateFrom = already ? "" : cell.dataset.day;
      state.dateTo = already ? "" : cell.dataset.day;
      els.dateFrom.value = state.dateFrom;
      els.dateTo.value = state.dateTo;
      state.currentPage = 1;
      render();
    });
  });
  document.getElementById("calYear")?.addEventListener("change", e => {
    const inYear = months.filter(m => m.startsWith(e.target.value));
    if (inYear.length) { state.calMonth = inYear[inYear.length - 1]; render(); }
  });
  document.getElementById("calPrev")?.addEventListener("click", () => { state.calMonth = months[mi - 1]; render(); });
  document.getElementById("calNext")?.addEventListener("click", () => { state.calMonth = months[mi + 1]; render(); });
  const known = knownDaySpan();
  els.periodLabel.textContent = known.count
    ? tf("hist.range", { from: known.from, to: known.to, known: known.count, total: records.length })
    : t("hist.none");
}

// Recorded-day span over the whole case — immutable, so computed once.
let knownDaySpanCache = null;
function knownDaySpan() {
  if (knownDaySpanCache) return knownDaySpanCache;
  let from = null, to = null, count = 0;
  records.forEach(record => {
    const day = record.recDay;
    if (!day) return;
    count += 1;
    if (from === null || day < from) from = day;
    if (to === null || day > to) to = day;
  });
  knownDaySpanCache = { from, to, count };
  return knownDaySpanCache;
}

// ---- layout chrome --------------------------------------------------------
// Rail (Ctrl+B), card density, the 정렬·표시 panel, 보기 ▾ keyboard support,
// count pills, detail tabs and the thumbnail retry button. Wired once from
// 95-init.js. Preferences are per-browser conveniences: every storage access
// is guarded and the page works without it.
const RAIL_KEY = "ft.viewer.rail.v2";
const DENSITY_KEY = "ft.viewer.density.v1";
const DETAIL_TAB_KEY = "ft.viewer.detailTab.v1";
const RAIL_AUTO_BELOW = 1000; // docs/ui-rebuild-contract.md §6.1: open from 1000px, Ctrl+B collapses
function prefGet(key, fallback) {
  try { const raw = localStorage.getItem(key); return raw ? JSON.parse(raw) : fallback; } catch { return fallback; }
}
function prefSet(key, value) {
  try { localStorage.setItem(key, JSON.stringify(value)); } catch { /* storage blocked: keep the in-memory value */ }
}

// The rail remembers open/closed separately for wide (>=1000px) and narrow
// frames; narrow frames start collapsed so the grid keeps its columns.
// v2: the threshold moved from 1500px to 1000px (1366 opens by default).
let railPrefs = Object.assign({ wide: true, narrow: false }, prefGet(RAIL_KEY, {}) || {});
let railWidthClass = null;
const railClassNow = () => (window.innerWidth >= RAIL_AUTO_BELOW ? "wide" : "narrow");
function applyRail() {
  railWidthClass = railClassNow();
  const open = !!railPrefs[railWidthClass];
  els.root.classList.toggle("rail-collapsed", !open);
  document.getElementById("btnRail")?.setAttribute("aria-expanded", open ? "true" : "false");
}
function toggleRail() {
  const key = railClassNow();
  railPrefs[key] = !railPrefs[key];
  prefSet(RAIL_KEY, railPrefs);
  applyRail();
  syncSplitterAria();
}

let gridDensity = prefGet(DENSITY_KEY, "comfortable") === "compact" ? "compact" : "comfortable";
function applyDensity() {
  els.root.classList.toggle("density-compact", gridDensity === "compact");
  ftRoot.querySelectorAll("[data-density]").forEach(btn => btn.setAttribute("aria-pressed", btn.dataset.density === gridDensity ? "true" : "false"));
}

function menuItems(list) {
  return [...list.querySelectorAll("button:not([disabled]), select:not([disabled]), input:not([disabled])")].filter(el => el.offsetParent !== null);
}

function setupLayoutChrome() {
  applyRail();
  applyDensity();
  window.addEventListener("resize", () => {
    if (railClassNow() !== railWidthClass) applyRail();
    syncSplitterAria();
  }, { signal: ftSignal });
  document.getElementById("btnRail")?.addEventListener("click", toggleRail);
  document.addEventListener("keydown", event => {
    if (event.defaultPrevented || event.altKey || event.shiftKey) return;
    // shortcutKey (80-review): a Russian / Korean layout's Ctrl+"и" is Ctrl+B.
    if (!(event.ctrlKey || event.metaKey) || shortcutKey(event).toLowerCase() !== "b") return;
    // Nothing behind an open viewer modal changes (U12).
    if (viewerModalOpen()) return;
    const el = document.activeElement;
    if (el && (el.tagName === "TEXTAREA" || el.isContentEditable)) return;
    if (el && el.closest && el.closest('[data-keys="off"]')) return;
    event.preventDefault();
    toggleRail();
  }, { signal: ftSignal });

  ftRoot.querySelectorAll("[data-density]").forEach(btn => btn.addEventListener("click", () => {
    if (gridDensity === btn.dataset.density) return;
    gridDensity = btn.dataset.density;
    prefSet(DENSITY_KEY, gridDensity);
    applyDensity();
    requestRender();
  }));

  // 정렬·표시 panel (its toggle lives in 80-review.js): outside click and
  // Esc close it, Esc returns focus to the button, a keyboard open moves
  // focus into the panel.
  const moreBtn = document.getElementById("btnMoreFilters");
  const extra = document.getElementById("filtersExtra");
  if (moreBtn && extra) {
    const closeExtra = focusBtn => {
      if (extra.hidden) return;
      extra.hidden = true;
      moreBtn.setAttribute("aria-expanded", "false");
      if (focusBtn) moreBtn.focus();
    };
    moreBtn.addEventListener("click", event => {
      const viaKeyboard = event.detail === 0;
      setTimeout(() => { if (!extra.hidden && viaKeyboard) menuItems(extra)[0]?.focus(); }, 0);
    });
    moreBtn.addEventListener("keydown", event => {
      if (event.key !== "ArrowDown" || event.defaultPrevented) return;
      event.preventDefault();
      if (extra.hidden) moreBtn.click();
      setTimeout(() => menuItems(extra)[0]?.focus(), 0);
    });
    extra.addEventListener("keydown", event => {
      if (event.key === "Escape") { event.preventDefault(); closeExtra(true); }
    });
    document.addEventListener("click", event => {
      if (!extra.hidden && event.target instanceof Element && !event.target.closest(".menu-filters")) closeExtra(false);
    }, { signal: ftSignal });
  }

  // 보기 ▾: Enter/Space/ArrowDown open and focus the first item, arrows move,
  // Esc closes and returns focus, Tab closes.
  const viewBtn = document.getElementById("btnViewMenu");
  const viewList = document.getElementById("viewMenuList");
  if (viewBtn && viewList) {
    const focusItem = idx => {
      const items = menuItems(viewList);
      if (items.length) items[(idx + items.length) % items.length].focus();
    };
    viewBtn.addEventListener("click", event => {
      const viaKeyboard = event.detail === 0;
      setTimeout(() => { if (!viewList.hidden && viaKeyboard) focusItem(0); }, 0);
    });
    viewBtn.addEventListener("keydown", event => {
      if (event.defaultPrevented || (event.key !== "ArrowDown" && event.key !== "ArrowUp")) return;
      event.preventDefault();
      if (viewList.hidden) viewBtn.click();
      setTimeout(() => focusItem(event.key === "ArrowUp" ? -1 : 0), 0);
    });
    viewList.addEventListener("keydown", event => {
      if (event.defaultPrevented) return;
      const items = menuItems(viewList);
      const at = items.indexOf(document.activeElement);
      if (event.key === "ArrowDown" || event.key === "ArrowUp") {
        event.preventDefault();
        focusItem(at + (event.key === "ArrowDown" ? 1 : -1));
      } else if (event.key === "Home" || event.key === "End") {
        event.preventDefault();
        focusItem(event.key === "Home" ? 0 : -1);
      } else if (event.key === "Escape") {
        event.preventDefault();
        viewList.hidden = true;
        viewBtn.setAttribute("aria-expanded", "false");
        viewBtn.focus();
      } else if (event.key === "Tab") {
        viewList.hidden = true;
        viewBtn.setAttribute("aria-expanded", "false");
      }
    });
  }

  // Count pills filter by status / anomaly; pressing the active one clears.
  ftRoot.querySelectorAll(".count-pills [data-pill]").forEach(pill => pill.addEventListener("click", () => {
    applyPresetChip(pill.getAttribute("aria-pressed") === "true" ? "" : pill.dataset.pill);
  }));

  // Active-filter chip row (shown while the rail is collapsed).
  document.getElementById("activeFilters")?.addEventListener("click", event => {
    const chip = event.target.closest("[data-af]");
    if (chip) clearActiveFilter(chip.dataset.af);
  });

  // Thumbnail retry: forget the failed URL and repaint.
  els.recordGrid.addEventListener("click", event => {
    const btn = event.target.closest("[data-thumb-retry]");
    if (!btn) return;
    const url = btn.dataset.thumbRetry;
    thumbDead.delete(url);
    thumbRetried.delete(url);
    requestRender();
  });

  // Detail tabs: roving tabindex, ←/→/Home/End, selection remembered.
  const tabs = [...document.querySelectorAll("#detailTabs [role='tab']")];
  const selectTab = (tab, focus) => {
    tabs.forEach(item => {
      const on = item === tab;
      item.setAttribute("aria-selected", on ? "true" : "false");
      item.tabIndex = on ? 0 : -1;
      const panel = document.getElementById(item.getAttribute("aria-controls"));
      if (panel) panel.hidden = !on;
    });
    if (focus) tab.focus();
    prefSet(DETAIL_TAB_KEY, tab.id);
  };
  tabs.forEach(tab => tab.addEventListener("click", () => selectTab(tab, false)));
  document.getElementById("detailTabs")?.addEventListener("keydown", event => {
    const at = tabs.indexOf(document.activeElement);
    if (at < 0) return;
    let next = null;
    if (event.key === "ArrowRight") next = at + 1;
    else if (event.key === "ArrowLeft") next = at - 1;
    else if (event.key === "Home") next = 0;
    else if (event.key === "End") next = tabs.length - 1;
    if (next === null) return;
    // preventDefault: the global ←/→ seek must not also fire.
    event.preventDefault();
    event.stopPropagation();
    selectTab(tabs[(next + tabs.length) % tabs.length], true);
  });
  const savedTab = tabs.find(tab => tab.id === prefGet(DETAIL_TAB_KEY, ""));
  if (savedTab) selectTab(savedTab, false);
}
