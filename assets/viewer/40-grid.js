const els = {
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
// notes or tags ("marked:any", "mark:*", "tag:*") depend on annotation
// state that several modules mutate directly, so those are recomputed on
// every call rather than risk serving a stale list.
let filteredMemo = { key: null, list: null };
function filterMemoKey() {
  const chip = state.chip || "";
  if (chip === "marked:any" || chip.startsWith("mark:") || chip.startsWith("tag:")) return null;
  return [state.kind, state.recType, state.status, chip, state.dateFrom, state.dateTo, state.query, state.sortBy].join("\u0001");
}

function filteredRecords() {
  const key = filterMemoKey();
  if (key !== null && filteredMemo.key === key) return filteredMemo.list;
  const list = computeFilteredRecords();
  filteredMemo = { key, list };
  return list;
}

function computeFilteredRecords() {
  const chip = state.chip || "";
  const query = state.query || "";
  const list = records.filter(record => {
    if (state.kind && record.kind !== state.kind) return false;
    if (state.recType && (record.recType || "unclassified") !== state.recType) return false;
    if (state.status && record.status !== state.status) return false;
    if (chip === "anomaly" && !record.hasAnomaly) return false;
    if (chip === "warning" && !(record.warnings || []).length) return false;
    if (chip === "kind:recovery" && record.kind !== "candidate" && record.kind !== "filesystem") return false;
    if (chip === "marked:any" && !state.marks[record.id] && !(state.tags[record.id] || []).length && !(state.notes[record.id] || "").trim()) return false;
    if (state.dateFrom || state.dateTo) {
      if (!record.recDay) return false;
      if (state.dateFrom && record.recDay < state.dateFrom) return false;
      if (state.dateTo && record.recDay > state.dateTo) return false;
    }
    if (chip.startsWith("tag:")) {
      const wanted = chip.slice(4);
      const tags = state.tags[record.id] || [];
      if (!tags.includes(wanted)) return false;
    }
    if (chip.startsWith("mark:")) {
      const wanted = chip.slice(5);
      const mark = state.marks[record.id];
      if (wanted === "none") { if (mark) return false; }
      else if (!mark || mark.status !== wanted) return false;
    }
    if (!query) return true;
    return searchHaystack(record).includes(query);
  });
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
    case "kind": return ({ video: t("filter.kind.video"), carved: t("filter.kind.carved"), filesystem: t("filter.kind.filesystem"), candidate: t("filter.kind.candidate") })[record.kind] || record.kind;
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
  const { videoMode, videoZoom, theater, dual, playerH, colSplit, rate } = state.layout;
  storageSet(LAYOUT_KEY, { videoMode, videoZoom, theater, dual, playerH, colSplit, rate });
}

let layoutSaveTimer = null;
function saveLayoutSoon() {
  clearTimeout(layoutSaveTimer);
  layoutSaveTimer = setTimeout(saveLayout, 250);
}

function applyLayout() {
  document.body.classList.toggle("theater", !!state.layout.theater);
  const player = document.querySelector(".player");
  const h = Number(state.layout.playerH) || 0;
  player.style.height = h >= 160 ? h + "px" : "";
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
// in CSS (55%/45%); a dragged value is stored as the left pane percentage.
function applyColumnSplit() {
  const main = document.querySelector(".main");
  const pct = Number(state.layout.colSplit) || 0;
  if (pct >= 28 && pct <= 72) {
    main.style.gridTemplateColumns = `minmax(0, ${pct}%) 10px minmax(0, 1fr)`;
  } else {
    main.style.gridTemplateColumns = "";
  }
}

function setupColumnSplitter() {
  const splitter = document.getElementById("vsplitter");
  const main = document.querySelector(".main");
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
    state.layout.colSplit = Math.max(28, Math.min(72, pct));
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
}

function setupHeightSplitter() {
  const splitter = document.getElementById("hsplitter");
  const player = document.querySelector(".player");
  let startY = 0;
  let startH = 0;
  let dragging = false;
  splitter.addEventListener("pointerdown", event => {
    dragging = true;
    startY = event.clientY;
    startH = player.offsetHeight;
    splitter.classList.add("dragging");
    splitter.setPointerCapture(event.pointerId);
    event.preventDefault();
  });
  splitter.addEventListener("pointermove", event => {
    if (!dragging) return;
    state.layout.playerH = Math.max(160, Math.min(window.innerHeight - 260, startH + event.clientY - startY));
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
    state.layout.playerH = 0;
    saveLayout();
    applyLayout();
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

function renderGrid(filtered) {
  if (!filtered.some(record => record.id === state.activeId)) {
    state.activeId = filtered[0]?.id || null;
  }
  const pageCount = Math.max(1, Math.ceil(filtered.length / state.pageSize));
  state.currentPage = Math.min(Math.max(1, state.currentPage), pageCount);
  const start = (state.currentPage - 1) * state.pageSize;
  const pageRows = filtered.slice(start, start + state.pageSize);

  els.resultCount.textContent = `${filtered.length}`;
  const rangeEnd = Math.min(filtered.length, start + pageRows.length);
  const rangeStart = filtered.length ? start + 1 : 0;
  els.pageStatus.textContent = `${rangeStart}–${rangeEnd} / ${filtered.length} · ${state.currentPage}/${pageCount}`;
  // Marks are sparse: walk the mark map, not every record (PERF-03).
  let reviewed = 0, important = 0, pending = 0;
  for (const [id, mark] of Object.entries(state.marks)) {
    if (!recordById.has(id)) continue;
    const status = mark?.status;
    if (status === "reviewed") reviewed += 1;
    else if (status === "important") important += 1;
    else if (status === "needs_verification") pending += 1;
  }
  if (els.triageStatus) {
    els.triageStatus.textContent = tf("triage.line", { done: reviewed, total: records.length, imp: important, pend: pending });
  }
  if (els.dflSummary) {
    const { dist, screened } = dflDistribution();
    els.dflSummary.textContent = screened
      ? tf("dfl.dist", { n: screened, total: records.length, high: dist.high, medium: dist.medium, low: dist.low, unknown: dist.unknown, failed: dist.failed, none: records.length - screened })
      : "";
  }
  els.prevPage.disabled = state.currentPage <= 1;
  els.nextPage.disabled = state.currentPage >= pageCount;

  const groupCounts = new Map();
  if (state.groupBy !== "none") {
    filtered.forEach(record => {
      const key = groupKeyFor(record);
      groupCounts.set(key, (groupCounts.get(key) || 0) + 1);
    });
  }

  const cardsHtml = [];
  let lastGroup = null;
  pageRows.forEach(record => {
    if (state.groupBy !== "none") {
      const key = groupKeyFor(record);
      if (key !== lastGroup) {
        lastGroup = key;
        const collapsed = state.collapsedGroups.has(key);
        const kindAttr = state.groupBy === "kind" ? ` data-gkind="${escapeHtml(record.kind || "")}"` : "";
        cardsHtml.push(`<div class="group-header"${kindAttr} data-group="${escapeHtml(key)}" tabindex="0" role="button"><span>${escapeHtml(key)}</span><span class="muted">${groupCounts.get(key) || 0}${t("unit.count")}${collapsed ? ` · ${t("group.collapsed")}` : ""}</span></div>`);
        if (collapsed) return;
      }
      if (state.collapsedGroups.has(key)) return;
    }
    cardsHtml.push(renderCard(record));
  });
  els.recordGrid.innerHTML = cardsHtml.join("") || `<div class="fallback">${t("grid.empty")}</div>`;
}

// Tri-state audio flag: probed codec present → speaker, probed with no
// audio stream → muted, unprobed (carved/candidate) → nothing so we never
// claim audio state we haven't verified.
function audioFlag(record) {
  const codec = String(record.audioCodec || "").toLowerCase();
  if (codec && codec !== "none" && codec !== "-") {
    return `<span class="audio-flag has" title="${escapeHtml(record.audioCodec)}">🔊</span>`;
  }
  if (record.probeOk === true) {
    return `<span class="audio-flag none" title="${escapeHtml(t("audio.absent"))}">🔇</span>`;
  }
  return "";
}

function renderCard(record) {
  const mark = markOf(record);
  const markChip = mark ? `<span class="mark-chip ${escapeHtml(mark.status)}">${escapeHtml(markLabel(mark.status))}</span>` : "";
  const staleTag = record.indexStatus === "stale" ? '<span class="muted">(stale)</span>' : "";
  const thumb = record.thumb
    ? `<img src="${escapeHtml(record.thumb)}" loading="lazy" decoding="async" fetchpriority="low">`
    : `<div class="ph">${record.status === "validation-failed" ? t("thumb.damaged") : t("thumb.missing")}</div>`;
  const recTypeTag = record.recType ? `<span class="rec-type">${escapeHtml(recTypeLabel(record.recType))}</span>` : "";
  const channel = record.channel ? `<span class="channel-badge">${escapeHtml(record.channel)}</span>` : "";
  const displayName = record.originalName && record.originalName !== record.name
    ? record.originalName
    : record.name;
  const recTime = record.recTime
    ? record.recSource === "name-date"
      ? record.recDay
      : new Date(record.recTime * 1000).toLocaleString("ko-KR", { year: "numeric", month: "2-digit", day: "2-digit", hour: "2-digit", minute: "2-digit" })
    : "";
  const tagsHtml = tagListFor(record).length
    ? `<div class="tag-row">${tagListFor(record).map(tag => `<span class="tag-chip">${escapeHtml(tag)}</span>`).join("")}</div>`
    : "";
  const anomalyChip = record.hasAnomaly
    ? `<span class="badge anomaly" title="${escapeHtml(record.anomalies.map(item => item.kind).join(", "))}">${escapeHtml(t("header.anomaly"))}</span>`
    : "";
  const kindChip = `<span class="kind-badge kind-${escapeHtml(record.kind || "video")}" title="${t("tree.kind")}">${escapeHtml(KIND_SHORT[record.kind] || record.kind || "?")}</span>`;
  const warnChip = (record.warnings || []).length
    ? `<span class="badge warn" title="${escapeHtml(record.warnings.join("\n"))}">${tf("warn.count", { n: record.warnings.length })}</span>`
    : "";
  const dflChip = record.dfl && record.dfl.band
    ? `<span class="badge dfl dfl-${escapeHtml(record.dfl.band)}" title="${escapeHtml(tf("dfl.title", { score: record.dfl.score ?? "" }))}">${escapeHtml(tf("dfl.badge", { band: record.dfl.band_label || record.dfl.band }))}</span>`
    : (record.dfl && record.dfl.error
      ? `<span class="badge dfl dfl-low" title="${escapeHtml(tf("dfl.failTitle", { error: record.dfl.error }))}">${escapeHtml(t("dfl.fail"))}</span>`
      : "");
  return `<div class="card ${record.id === state.activeId ? "active" : ""}" data-id="${escapeHtml(record.id)}" tabindex="${record.id === state.activeId ? 0 : -1}" role="option" aria-selected="${state.selectedIds.has(record.id)}">
    <div class="thumb">${thumb}<input type="checkbox" aria-label="${escapeHtml(t("aria.select"))}" ${state.selectedIds.has(record.id) ? "checked" : ""} data-check="${escapeHtml(record.id)}">${recTypeTag}${kindChip}<span class="dur">${fmtDuration(record.duration)}</span></div>
    <div class="meta">
      <div class="name-row"><span class="name" title="${escapeHtml(displayName)}">${highlightEscape(displayName, state.query)}</span>${channel ? `<span class="channel-badge">${escapeHtml(record.channel)}</span>` : ""}</div>
      <div class="time-row"><span class="time-text">${recTime ? escapeHtml(recTime) : t("time.unknown")}</span><span class="badge ${statusClass(record.status)}" title="${escapeHtml(record.status)}">${escapeHtml(statusLabel(record.status))}</span>${anomalyChip}${warnChip}${dflChip}</div>
      ${tagsHtml}
      <div class="sub-row"><span class="sub">${fmtBytes(record.size)}${audioFlag(record)}${markChip}${staleTag}</span></div>
    </div>
  </div>`;
}

// Month-calendar rendering of per-day counts. Each cell shows the day
// number and a count chip; intensity follows the count relative to the
// month's max. Click selects the day as a date filter; clicking the
// selected day clears it. Month nav steps through the recorded span.
function renderHistogram(filtered) {
  const days = new Map();
  filtered.forEach(record => {
    if (record.recDay) days.set(record.recDay, (days.get(record.recDay) || 0) + 1);
  });
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
  for (let i = 0; i < startWeekday; i++) cells.push(`<span class="cal-cell empty"></span>`);
  for (let d = 1; d <= dim; d++) {
    const day = `${state.calMonth}-${String(d).padStart(2, "0")}`;
    const count = days.get(day) || 0;
    const selected = state.dateFrom === day && state.dateTo === day;
    const level = count ? Math.min(4, 1 + Math.floor((count * 3) / max)) : 0;
    cells.push(count
      ? `<button type="button" class="cal-cell l${level}${selected ? " selected" : ""}" data-day="${day}" title="${escapeHtml(tf("hist.item", { day, count }))}"><span class="cal-d">${d}</span><span class="cal-n">${count > 99 ? "99+" : count}</span></button>`
      : `<span class="cal-cell l0${selected ? " selected" : ""}" data-day="${day}"><span class="cal-d">${d}</span></span>`);
  }
  const monthLabel = new Date(cy, cm - 1, 1).toLocaleDateString(loc, { year: "numeric", month: "long" });
  const mi = months.indexOf(state.calMonth);
  const nav = months.length > 1
    ? `<div class="cal-nav">
        <button type="button" class="cal-btn" id="calPrev" ${mi <= 0 ? "disabled" : ""} title="${escapeHtml(t("cal.prev"))}">‹</button>
        <span class="cal-month">${escapeHtml(monthLabel)}</span>
        <button type="button" class="cal-btn" id="calNext" ${mi >= months.length - 1 ? "disabled" : ""} title="${escapeHtml(t("cal.next"))}">›</button>
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
