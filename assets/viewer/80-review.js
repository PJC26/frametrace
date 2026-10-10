function moveActive(step) {
  const filtered = filteredRecords();
  if (!filtered.length) return;
  const index = filtered.findIndex(record => record.id === state.activeId);
  const next = Math.max(0, Math.min(filtered.length - 1, (index < 0 ? 0 : index + step)));
  const nextId = filtered[next].id;
  const nextPage = Math.floor(next / state.pageSize) + 1;
  const pageChanged = nextPage !== state.currentPage;
  state.activeId = nextId;
  if (pageChanged) {
    state.currentPage = nextPage;
    render();
    const card = els.recordGrid.querySelector(`.card[data-id="${CSS.escape(state.activeId)}"]`);
    card?.scrollIntoView({ block: "nearest" });
    return;
  }
  // Same page: avoid rebuilding up to 1000 cards on every j/k.
  els.recordGrid.querySelectorAll(".card.active").forEach(el => { el.classList.remove("active"); el.tabIndex = -1; });
  const card = els.recordGrid.querySelector(`.card[data-id="${CSS.escape(state.activeId)}"]`);
  if (card) { card.classList.add("active"); card.tabIndex = 0; }
  card?.scrollIntoView({ block: "nearest" });
  renderDetails();
}

function toggleActiveSelection() {
  if (!state.activeId) return;
  if (state.selectedIds.has(state.activeId)) state.selectedIds.delete(state.activeId);
  else state.selectedIds.add(state.activeId);
  render();
}

let shortcutsLastFocus = null;
function toggleShortcuts(open) {
  const modal = document.getElementById("shortcutsModal");
  if (open && modal.hidden) shortcutsLastFocus = document.activeElement;
  modal.hidden = !open;
  if (open) document.getElementById("btnShortcutsClose")?.focus();
  else if (shortcutsLastFocus) { shortcutsLastFocus.focus(); shortcutsLastFocus = null; }
}
document.getElementById("shortcutsModal").addEventListener("keydown", e => {
  if (e.key !== "Tab") return;
  const focusables = [...e.currentTarget.querySelectorAll("button, input, select, a[href], [tabindex]")]
    .filter(el => !el.disabled);
  if (!focusables.length) return;
  const first = focusables[0];
  const last = focusables[focusables.length - 1];
  if (e.shiftKey && document.activeElement === first) { e.preventDefault(); last.focus(); }
  else if (!e.shiftKey && document.activeElement === last) { e.preventDefault(); first.focus(); }
});

// --- Undo/redo journal (UX-04) ---
// Every examiner-visible mutation of marks / notes / tags / IN-OUT ranges
// that goes through journaled() records a per-id before/after snapshot,
// so a stray key or a bulk action over a large selection is reversible
// with Ctrl+Z / Ctrl+Y (or the Undo button on the toast).
const JOURNAL_LIMIT = 200;
const BULK_CONFIRM_MIN = 5;
const undoStack = [];
const redoStack = [];

function snapshotIds(ids) {
  return ids.map(id => ({
    id,
    mark: state.marks[id] ? { ...state.marks[id] } : null,
    note: Object.prototype.hasOwnProperty.call(state.notes, id) ? state.notes[id] : null,
    tags: state.tags[id] ? [...state.tags[id]] : null,
    range: state.ranges[id] ? { ...state.ranges[id] } : null
  }));
}

const JOURNAL_FIELDS = ["mark", "note", "tags", "range"];

// Per id, the fields a journaled action actually changed. Undo / redo
// restore only those, so a note or tag edited later through an
// unjournaled path (note field, detail tag buttons, custom tag input)
// survives Ctrl+Z on an earlier mark and is not lost on redo either.
function journalDiff(before, after) {
  const afterById = new Map(after.map(item => [item.id, item]));
  const changes = [];
  for (const prev of before) {
    const next = afterById.get(prev.id);
    if (!next) continue;
    const fields = JOURNAL_FIELDS.filter(field => JSON.stringify(prev[field]) !== JSON.stringify(next[field]));
    if (fields.length) changes.push({ id: prev.id, fields, before: prev, after: next });
  }
  return changes;
}

function applyJournalSide(changes, side) {
  const current = new Map(snapshotIds(changes.map(change => change.id)).map(item => [item.id, item]));
  for (const change of changes) {
    const id = change.id;
    const item = change[side];
    const now = current.get(id);
    let annotationChanged = false;
    for (const field of change.fields) {
      if (field === "mark") {
        if (item.mark) state.marks[id] = { ...item.mark }; else delete state.marks[id];
        if (JSON.stringify(now.mark) !== JSON.stringify(item.mark)) annotationChanged = true;
      } else if (field === "note") {
        if (item.note == null) delete state.notes[id]; else state.notes[id] = item.note;
        if ((now.note || "") !== (item.note || "")) annotationChanged = true;
      } else if (field === "tags") {
        if (item.tags) state.tags[id] = [...item.tags]; else delete state.tags[id];
        if (JSON.stringify(now.tags || []) !== JSON.stringify(item.tags || [])) annotationChanged = true;
      } else if (field === "range") {
        if (item.range) state.ranges[id] = { ...item.range }; else delete state.ranges[id];
      }
    }
    if (annotationChanged) touchAnnotation(id);
  }
  storageSet(MARKS_KEY, state.marks);
  storageSet(NOTES_KEY, state.notes);
  storageSet(TAGS_KEY, state.tags);
  storageSet(RANGES_KEY, state.ranges);
}

// Toast with an inline Undo button. Only the newest undo toast is kept so
// rapid 1/2/3 triage does not stack a column of stale buttons.
function attachUndoButton(item) {
  const btn = document.createElement("button");
  btn.type = "button";
  btn.className = "mini";
  btn.style.marginLeft = "10px";
  btn.textContent = t("undo.btn");
  btn.addEventListener("click", () => { item.remove(); undoJournal(); });
  item.classList.add("toast-undo");
  item.appendChild(btn);
}
function toastUndo(message) {
  const host = document.getElementById("toastHost");
  if (!host) return;
  host.querySelectorAll(".toast-undo").forEach(el => el.remove());
  const item = document.createElement("div");
  item.className = "toast";
  const text = document.createElement("span");
  text.textContent = message;
  item.appendChild(text);
  attachUndoButton(item);
  host.appendChild(item);
  setTimeout(() => item.remove(), 6000);
}

// Runs mutate(), journals the before/after state of `ids` when anything
// changed, and offers Undo. When `message` is omitted and mutate() raised
// its own toast, the Undo button is attached to that toast instead of
// adding a second one.
function journaled(label, ids, mutate, message) {
  const unique = [...new Set(ids)];
  const host = document.getElementById("toastHost");
  const lastToastBefore = host ? host.lastElementChild : null;
  const activeBefore = state.activeId;
  const before = snapshotIds(unique);
  const result = mutate();
  const after = snapshotIds(unique);
  const changes = journalDiff(before, after);
  if (!changes.length) return result;
  undoStack.push({ label, changes, activeBefore });
  if (undoStack.length > JOURNAL_LIMIT) undoStack.shift();
  redoStack.length = 0;
  const raised = host && host.lastElementChild !== lastToastBefore ? host.lastElementChild : null;
  if (message == null && raised && raised.classList.contains("toast")) {
    host.querySelectorAll(".toast-undo").forEach(el => el.remove());
    attachUndoButton(raised);
  } else {
    toastUndo(message == null ? label : message);
  }
  return result;
}

function focusRecordId(id) {
  if (!id || !records.some(record => record.id === id)) return;
  state.activeId = id;
  const index = filteredRecords().findIndex(record => record.id === id);
  if (index >= 0) state.currentPage = Math.floor(index / state.pageSize) + 1;
}

function undoJournal() {
  const entry = undoStack.pop();
  if (!entry) { toast(t("toast.nothingUndo")); return false; }
  applyJournalSide(entry.changes, "before");
  redoStack.push(entry);
  focusRecordId(entry.activeBefore);
  render();
  updateRangeLabel();
  toast(tf("toast.undone", { what: entry.label }));
  return true;
}

function redoJournal() {
  const entry = redoStack.pop();
  if (!entry) { toast(t("toast.nothingRedo")); return false; }
  applyJournalSide(entry.changes, "after");
  undoStack.push(entry);
  focusRecordId(entry.activeBefore);
  render();
  updateRangeLabel();
  toast(tf("toast.redone", { what: entry.label }));
  return true;
}

// Bulk actions over more than BULK_CONFIRM_MIN items ask first; the
// journal still makes them undoable afterwards.
function confirmBulk(ids, action) {
  if (ids.length <= BULK_CONFIRM_MIN) return true;
  return window.confirm(tf("confirm.bulk", { n: ids.length, action }));
}

function markStatusLabel(status) {
  return status === null ? t("mark.cleared") : markLabel(status);
}

// Selection-bar mark buttons. Clearing a mark keeps the examiner's note —
// notes are only ever edited in the note field.
function applyMarkJournaled(status) {
  const ids = targetIds();
  if (!ids.length) { toast(t("toast.selectTarget")); return; }
  const label = markStatusLabel(status);
  if (!confirmBulk(ids, label)) return;
  journaled(label, ids, () => {
    const stamped = Math.floor(Date.now() / 1000);
    ids.forEach(id => {
      if (status === null) delete state.marks[id];
      else state.marks[id] = { status, marked_unix: stamped };
      touchAnnotation(id);
    });
    storageSet(MARKS_KEY, state.marks);
  }, tf("toast.markApplied", { n: ids.length, status: label }));
  render();
}

function applyTagJournaled(tag) {
  const ids = targetIds();
  if (ids.length && !confirmBulk(ids, tf("journal.tag", { tag }))) return;
  journaled(tf("journal.tag", { tag }), ids, () => applyTag(tag));
}

function clearTagsJournaled() {
  const ids = targetIds();
  if (ids.length && !confirmBulk(ids, t("journal.tagClear"))) return;
  journaled(t("journal.tagClear"), ids, () => clearTags());
}

// Single-key triage: mark the active record and advance so an examiner
// can clear a review queue without touching the mouse. Key 0 removes the
// mark only — the note survives — and every mark is undoable.
function markActive(status) {
  const before = filteredRecords();
  const index = before.findIndex(record => record.id === state.activeId);
  if (index < 0) return;
  const id = state.activeId;
  const label = markStatusLabel(status);
  journaled(label, [id], () => {
    if (status === null) delete state.marks[id];
    else state.marks[id] = { status, marked_unix: Math.floor(Date.now() / 1000) };
    touchAnnotation(id);
    storageSet(MARKS_KEY, state.marks);
  }, tf("toast.markApplied", { n: 1, status: label }));
  const after = filteredRecords();
  const next = before.slice(index + 1).find(record => after.some(item => item.id === record.id));
  state.activeId = next?.id || after[Math.min(index, after.length - 1)]?.id || null;
  state.currentPage = Math.max(1, Math.floor(after.findIndex(record => record.id === state.activeId) / state.pageSize) + 1);
  render();
}

// --- Review progress + action scope (UX-16 / UX-04) ---
function isReviewed(record) { return !!state.marks[record.id]; }

// n / Shift+N: jump to the next / previous record in the current filter
// order that has no mark yet, wrapping around.
function moveToUnreviewed(step) {
  const filtered = filteredRecords();
  if (!filtered.length) return;
  const start = filtered.findIndex(record => record.id === state.activeId);
  const base = start < 0 ? (step > 0 ? -1 : 0) : start;
  const total = filtered.length;
  for (let offset = 1; offset <= total; offset += 1) {
    const index = (((base + step * offset) % total) + total) % total;
    const record = filtered[index];
    if (!isReviewed(record)) {
      focusRecordId(record.id);
      render();
      els.recordGrid.querySelector(`.card[data-id="${CSS.escape(state.activeId)}"]`)?.scrollIntoView({ block: "nearest" });
      return;
    }
  }
  toast(t("toast.allReviewed"));
}

const reviewMeta = (() => {
  const bar = document.getElementById("selectionBar");
  const count = document.getElementById("selectionCount");
  if (!bar || !count) return null;
  const progress = document.createElement("span");
  progress.id = "reviewProgress";
  progress.className = "grp-label";
  progress.style.cssText = "display:inline-flex;align-items:center;gap:6px;white-space:nowrap";
  const meter = document.createElement("progress");
  meter.style.cssText = "width:64px;height:8px";
  const progressText = document.createElement("span");
  progress.append(progressText, meter);
  count.insertAdjacentElement("afterend", progress);
  const scope = document.createElement("span");
  scope.id = "markScope";
  scope.className = "grp-label";
  scope.style.whiteSpace = "nowrap";
  const seg = bar.querySelector(".seg");
  if (seg) seg.insertAdjacentElement("beforebegin", scope);
  else bar.appendChild(scope);
  return { count, progress, progressText, meter, scope };
})();

function updateReviewMeta() {
  if (!reviewMeta) return;
  const total = records.length;
  const reviewed = records.reduce((n, record) => n + (isReviewed(record) ? 1 : 0), 0);
  reviewMeta.progressText.textContent = tf("progress.label", { n: reviewed, m: total });
  reviewMeta.meter.max = Math.max(1, total);
  reviewMeta.meter.value = reviewed;
  reviewMeta.progress.title = t("progress.title");
  const selected = state.selectedIds.size;
  reviewMeta.scope.textContent = selected
    ? tf("scope.selected", { n: selected })
    : state.activeId ? t("scope.active") : t("scope.none");
}
// renderMetrics() rewrites #selectionCount on every render — piggyback on
// that instead of patching render() in a file this module doesn't own.
if (reviewMeta && typeof MutationObserver === "function") {
  new MutationObserver(updateReviewMeta).observe(reviewMeta.count, { childList: true, characterData: true, subtree: true });
}
updateReviewMeta();

// --- Global keyboard (UX-03 / UX-08 / UX-15) ---
// Only real text entry captures keys. A focused <select>, checkbox, range
// slider or <video> keeps its own navigation keys; every other shortcut
// still works so the viewer never seems frozen after using a filter.
const TEXT_INPUT_TYPES = new Set(["", "text", "search", "email", "url", "tel", "password", "number", "date", "datetime-local", "month", "week", "time"]);
const NATIVE_NAV_KEYS = new Set(["ArrowUp", "ArrowDown", "ArrowLeft", "ArrowRight", "Home", "End", "PageUp", "PageDown", " ", "Enter", "Tab"]);
function isTypingTarget(el) {
  if (!el) return false;
  if (el.isContentEditable) return true;
  if (el.tagName === "TEXTAREA") return true;
  return el.tagName === "INPUT" && TEXT_INPUT_TYPES.has(String(el.getAttribute("type") || "").toLowerCase());
}

document.addEventListener("keydown", event => {
  if (event.defaultPrevented) return;
  const el = document.activeElement;
  const tag = el?.tagName;
  if (isTypingTarget(el)) {
    if (event.key === "Escape") el.blur();
    return;
  }
  if ((tag === "SELECT" || tag === "INPUT" || tag === "VIDEO") && NATIVE_NAV_KEYS.has(event.key)) return;
  if ((tag === "BUTTON" || tag === "A" || tag === "SUMMARY") && (event.key === " " || event.key === "Enter")) {
    return; // let the focused control receive its normal activation keys
  }
  // AltGr reports ctrl+alt on Windows layouts that type [ ] ? with it —
  // treat it as plain typing, not as a modifier combo.
  const altGraph = typeof event.getModifierState === "function" && event.getModifierState("AltGraph");
  const mod = !altGraph && (event.ctrlKey || event.metaKey);
  if (!altGraph && event.altKey) return; // Alt+Left etc. belong to the browser
  if (mod) {
    const key = String(event.key).toLowerCase();
    if (key === "z" && !event.shiftKey) { event.preventDefault(); undoJournal(); }
    else if (key === "y" || (key === "z" && event.shiftKey)) { event.preventDefault(); redoJournal(); }
    else if (event.ctrlKey && !event.metaKey && !event.shiftKey && key === "a") { event.preventDefault(); selectAllFiltered(); }
    else if (event.ctrlKey && !event.metaKey && !event.shiftKey && key === "d") { event.preventDefault(); clearSelection(); }
    return; // every other Ctrl/Cmd combo (zoom, find, print…) stays the browser's
  }
  let handled = true;
  switch (event.key) {
    case "j": moveActive(1); break;
    case "k": moveActive(-1); break;
    case "n": moveToUnreviewed(1); break;
    case "N": moveToUnreviewed(-1); break;
    case " ": event.preventDefault(); togglePlayback(); break;
    case "x": toggleActiveSelection(); break;
    case "Enter":
      currentVideo()?.play().catch(() => {});
      break;
    case "1": markActive("reviewed"); break;
    case "2": markActive("important"); break;
    case "3": markActive("needs_verification"); break;
    case "0": markActive(null); break;
    case "ArrowLeft": skipVideo(-10); break;
    case "ArrowRight": skipVideo(10); break;
    case "[": rateStep(-1); break;
    case "]": rateStep(1); break;
    case "i": setRangePoint("in"); break;
    case "o": setRangePoint("out"); break;
    case "f": toggleFullscreen(); break;
    case "t": toggleTheater(); break;
    case "p": togglePip(); break;
    case "?": toggleShortcuts(true); break;
    case "Escape":
      if (tag === "SELECT" || tag === "INPUT") el.blur();
      toggleShortcuts(false);
      if (state.layout.theater) toggleTheater();
      break;
    default: handled = false; break;
  }
  // A focused <select> would also type-ahead on the same key.
  if (handled && tag === "SELECT") event.preventDefault();
});

els.query.addEventListener("input", () => {
  clearTimeout(els.query._debounce);
  els.query._debounce = setTimeout(() => {
    state.query = els.query.value.trim().toLowerCase();
    state.currentPage = 1;
    render();
  }, 150);
});
els.kind.addEventListener("change", () => { state.kind = els.kind.value; state.currentPage = 1; render(); });
els.status.addEventListener("change", () => { state.status = els.status.value; state.currentPage = 1; render(); });
els.pageSize.addEventListener("change", () => {
  state.pageSize = Number(els.pageSize.value) || 100;
  state.currentPage = 1;
  render();
});
els.prevPage.addEventListener("click", () => { state.currentPage -= 1; render(); });
els.nextPage.addEventListener("click", () => { state.currentPage += 1; render(); });
els.sortBy.addEventListener("change", () => { state.sortBy = els.sortBy.value; state.currentPage = 1; render(); });
els.groupBy.addEventListener("change", () => { state.groupBy = els.groupBy.value; state.currentPage = 1; render(); });
els.tagFilter.addEventListener("change", () => {
  state.chip = els.tagFilter.value;
  state.status = "";
  els.status.value = "";
  state.currentPage = 1;
  render();
});
els.dateFrom.addEventListener("change", () => { state.dateFrom = els.dateFrom.value; state.currentPage = 1; render(); });
els.dateTo.addEventListener("change", () => { state.dateTo = els.dateTo.value; state.currentPage = 1; render(); });
document.getElementById("btnClearDates").addEventListener("click", () => {
  state.dateFrom = "";
  state.dateTo = "";
  els.dateFrom.value = "";
  els.dateTo.value = "";
  state.currentPage = 1;
  render();
});
els.videoMode.addEventListener("change", () => { state.layout.videoMode = els.videoMode.value; saveLayout(); applyVideoScale(); });
els.videoZoom.addEventListener("input", () => {
  state.layout.videoMode = els.videoMode.value === "fit" ? "fit" : els.videoMode.value;
  if (els.videoMode.value !== "fit") state.layout.videoMode = els.videoMode.value;
  state.layout.videoZoom = Number(els.videoZoom.value);
  if (els.videoMode.value === "fit") { els.videoMode.value = "fit"; }
  saveLayoutSoon();
  applyVideoScale();
});
document.getElementById("btnTheater").addEventListener("click", toggleTheater);
document.getElementById("btnFullscreen").addEventListener("click", toggleFullscreen);
document.getElementById("btnPip").addEventListener("click", togglePip);
document.getElementById("btnSkipBack").addEventListener("click", () => skipVideo(-10));
document.getElementById("btnSkipFwd").addEventListener("click", () => skipVideo(10));
document.getElementById("playRate").addEventListener("change", () => setRate(Number(els.playRate.value) || 1));
document.getElementById("btnPopPlayer").addEventListener("click", openPlayerWindow);
document.getElementById("btnDual").addEventListener("click", () => {
  state.layout.dual = !state.layout.dual;
  saveLayout();
  applyLayout();
  if (state.layout.dual) {
    const record = selectedRecord();
    if (!record || !matesOf(record).length) toast(t("toast.noPair"));
  }
  mediaRenderedFor = null;
  renderDetails();
});
document.getElementById("btnTelemetry").addEventListener("click", async (ev) => {
  const record = selectedRecord();
  if (!record) return;
  const btn = ev.currentTarget;
  btn.disabled = true;
  try {
    const res = await fetch("/api/telemetry", {
      method: "POST",
      headers: { "Content-Type": "text/plain" },
      body: JSON.stringify({ id: record.id, path: record.path || "" })
    });
    const data = await res.json();
    if (data.ok && data.report) {
      record.telemetry = data.report;
      if (DATA.telemetry) DATA.telemetry[String(record.id).replace(/[:\\\/]/g, "_")] = data.report;
      renderDetails();
      toast(tf("toast.telemetryDone", { n: data.report.point_count || 0 }));
    } else {
      toast(tf("toast.telemetryFail", { err: data.error || "" }));
    }
  } catch {
    toast(t("toast.telemetryOffline"));
  } finally {
    btn.disabled = false;
  }
});

// Grab the decoded frame straight off the <video> element — no ffmpeg round
// trip — and store it as a hashed case artifact (artifacts/captures/).
async function captureCurrentFrame(videoEl, recordId) {
  if (!videoEl || !videoEl.videoWidth) { toast(t("toast.noVideo")); return; }
  const canvas = document.createElement("canvas");
  canvas.width = videoEl.videoWidth;
  canvas.height = videoEl.videoHeight;
  canvas.getContext("2d").drawImage(videoEl, 0, 0);
  const dataUrl = canvas.toDataURL("image/jpeg", 0.92);
  const b64 = dataUrl.slice(dataUrl.indexOf(",") + 1);
  try {
    const res = await fetch("/api/capture-frame", {
      method: "POST",
      headers: { "Content-Type": "text/plain" },
      body: JSON.stringify({ id: recordId, image: b64, time: videoEl.currentTime.toFixed(2) })
    });
    const data = await res.json();
    toast(data.ok ? tf("toast.captureSaved", { path: data.path }) : tf("toast.captureFail", { err: data.error || "" }));
  } catch {
    toast(t("toast.captureOffline"));
  }
}
document.getElementById("btnCaptureFrame").addEventListener("click", () => {
  const record = selectedRecord();
  captureCurrentFrame(currentVideo(), record ? record.id : "unknown");
});

// --- 구간(인/아웃) 마킹 + 클립보내기: 기록별로 localStorage에 유지됨 ---
function rangeOf(record) { return record ? state.ranges[record.id] : null; }
function updateRangeLabel() {
  const el = document.getElementById("rangeLabel");
  const record = selectedRecord();
  const range = rangeOf(record);
  const point = value => Number.isFinite(value) ? `${value.toFixed(1)}s` : "—";
  el.textContent = range && (Number.isFinite(range.in) || Number.isFinite(range.out))
    ? tf("range.label", { in: point(range.in), out: point(range.out) })
    : "";
  const proxyBtn = document.getElementById("btnProxy");
  if (proxyBtn) proxyBtn.classList.toggle("on", !!(record && state.proxies[record.id]));
}
// IN/OUT marking. The local is `now`, not `t` — shadowing the i18n t()
// put every toast in this function into its TDZ (UX-01). A point that
// would make OUT <= IN is rejected and the existing range kept intact.
function setRangePoint(which) {
  const video = currentVideo();
  const record = selectedRecord();
  const now = video ? Number(video.currentTime) : NaN;
  if (!video || !record || !Number.isFinite(now)) { toast(t("toast.pickEvidence")); return; }
  const existing = state.ranges[record.id] || { in: null, out: null };
  const range = { in: existing.in ?? null, out: existing.out ?? null };
  if (which === "in") range.in = now; else range.out = now;
  if (Number.isFinite(range.in) && Number.isFinite(range.out) && range.out <= range.in) {
    toast(t("toast.outBeforeIn"));
    return;
  }
  journaled(t("journal.range"), [record.id], () => {
    state.ranges[record.id] = range;
    storageSet(RANGES_KEY, state.ranges);
  }, tf("range.label", {
    in: Number.isFinite(range.in) ? `${range.in.toFixed(1)}s` : "—",
    out: Number.isFinite(range.out) ? `${range.out.toFixed(1)}s` : "—"
  }));
  updateRangeLabel();
}
document.getElementById("btnSetIn").addEventListener("click", () => setRangePoint("in"));
document.getElementById("btnSetOut").addEventListener("click", () => setRangePoint("out"));
// Shared proxy request used by the manual toggle and by the automatic
// fallback when the selected record's container can't decode in-browser.
// Returns the proxy path on success; throws with the server error text.
async function requestReviewProxy(record) {
  const res = await fetch("/api/proxy", {
    method: "POST",
    headers: { "Content-Type": "text/plain" },
    body: JSON.stringify({ id: record.id, path: record.path || "" })
  });
  const data = await res.json();
  if (!data.ok) throw new Error(data.error || "proxy failed");
  state.proxies[record.id] = data.path;
  storageSet(PROXIES_KEY, state.proxies);
  mediaRenderedFor = null;
  render();
  return data.path;
}

// id → "building" | "failed:<err>" — one automatic attempt per record per
// page session; a failure is shown honestly, never retried in a loop.
const proxyAutoState = new Map();
function autoRequestProxy(record) {
  proxyAutoState.set(record.id, "building");
  requestReviewProxy(record).catch(err => {
    proxyAutoState.set(record.id, "failed:" + (err?.message || ""));
    mediaRenderedFor = null;
    render();
  });
}

// Review proxy toggle: heavy originals (4K/HEVC) stutter on exam machines,
// so the viewer can lazily ask the server for a low-bitrate proxy.
document.getElementById("btnProxy").addEventListener("click", async () => {
  const record = selectedRecord();
  if (!record) { toast(t("toast.playPick")); return; }
  const btn = document.getElementById("btnProxy");
  if (state.proxies[record.id]) {
    delete state.proxies[record.id];
    storageSet(PROXIES_KEY, state.proxies);
    mediaRenderedFor = null;
    render();
    toast(t("toast.playOriginal"));
    return;
  }
  btn.disabled = true;
  toast(t("toast.proxyBuilding"));
  try {
    await requestReviewProxy(record);
    toast(t("toast.proxyPlaying"));
  } catch (err) {
    toast(tf("toast.proxyFail", { err: err?.message || "" }));
  } finally {
    btn.disabled = false;
  }
});
document.getElementById("btnExportClip").addEventListener("click", async () => {
  const record = selectedRecord();
  const range = rangeOf(record);
  if (!record || !range || !Number.isFinite(range.in) || !Number.isFinite(range.out) || range.out <= range.in) {
    toast(t("toast.noRange"));
    return;
  }
  try {
    const res = await fetch("/api/export-clip", {
      method: "POST",
      headers: { "Content-Type": "text/plain" },
      body: JSON.stringify({ id: record.id, path: record.path || "", start: range.in.toFixed(3), duration: (range.out - range.in).toFixed(3) })
    });
    const data = await res.json();
    toast(data.ok ? tf("toast.clipDone", { path: data.path }) : tf("toast.clipFail", { err: data.error || "" }));
  } catch {
    toast(t("toast.clipOffline"));
  }
});
document.getElementById("btnExportClipBurn").addEventListener("click", async () => {
  const record = selectedRecord();
  const range = rangeOf(record);
  if (!record || !range || !Number.isFinite(range.in) || !Number.isFinite(range.out) || range.out <= range.in) {
    toast(t("toast.noRange"));
    return;
  }
  const exhibit = window.prompt(t("clip.exhibitPrompt"), t("clip.exhibitDefault")) ?? "";
  try {
    const res = await fetch("/api/export-clip", {
      method: "POST",
      headers: { "Content-Type": "text/plain" },
      body: JSON.stringify({ id: record.id, path: record.path || "", start: range.in.toFixed(3), duration: (range.out - range.in).toFixed(3), burn_in: "true", exhibit })
    });
    const data = await res.json();
    toast(data.ok ? tf("toast.clipDone", { path: data.path }) : tf("toast.clipFail", { err: data.error || "" }));
  } catch {
    toast(t("toast.clipOffline"));
  }
});
document.getElementById("btnShortcuts").addEventListener("click", () => toggleShortcuts(true));
document.getElementById("btnShortcutsClose").addEventListener("click", () => toggleShortcuts(false));
document.getElementById("btnSelectFiltered").addEventListener("click", selectAllFiltered);
document.getElementById("btnClearSelection").addEventListener("click", clearSelection);
document.getElementById("btnMarkReviewed").addEventListener("click", () => applyMarkJournaled("reviewed"));
document.getElementById("btnMarkImportant").addEventListener("click", () => applyMarkJournaled("important"));
document.getElementById("btnMarkVerify").addEventListener("click", () => applyMarkJournaled("needs_verification"));
document.getElementById("btnMarkClear").addEventListener("click", () => applyMarkJournaled(null));
document.getElementById("btnCopyIds").addEventListener("click", () => {
  const ids = targetIds();
  if (ids.length) copyText(ids.join("\n"), t("copy.ids"));
});
document.getElementById("btnCopyPaths").addEventListener("click", () => {
  const ids = new Set(targetIds());
  const paths = records.filter(record => ids.has(record.id)).map(record => record.path);
  if (paths.length) copyText(paths.join("\n"), t("copy.paths"));
  else toast(t("toast.selectTarget"));
});
// Tag menu: preset buttons are rebuilt from the editable preset list.
// Each row applies the tag to the current selection; the trailing ×
// removes the preset (applied tags on records are untouched). The
// input registers a new preset — and, when a selection exists, applies
// it right away.
const tagMenuList = document.getElementById("tagMenuList");
function rebuildTagMenu() {
  tagMenuList.innerHTML = state.tagPresets.map(tag =>
    `<div class="tag-menu-row"><button type="button" data-keep data-tag="${escapeHtml(tag)}">${escapeHtml(tag)}</button><button type="button" class="tag-del" data-keep data-del-preset="${escapeHtml(tag)}" title="${t("tagmenu.delPreset")}">×</button></div>`
  ).join("")
    + `<button type="button" data-tag-clear>${t("tagmenu.clear")}</button><hr>`
    + `<div class="tag-add"><input type="text" id="tagPresetInput" placeholder="${t("tagmenu.new")}" maxlength="24">`
    + `<button type="button" data-keep data-tag-add>${t("tagmenu.add")}</button></div>`;
}
function addTagPresetFromMenu() {
  const input = document.getElementById("tagPresetInput");
  const name = (input?.value || "").trim();
  if (!name) return;
  const isNew = !state.tagPresets.includes(name);
  addTagPreset(name);
  rebuildTagMenu();
  document.getElementById("tagPresetInput")?.focus();
  if (targetIds().length) applyTagJournaled(name);
  else if (isNew) toast(tf("toast.presetAdded", { tag: name }));
}
tagMenuList.addEventListener("click", e => {
  const del = e.target.closest("[data-del-preset]");
  if (del) { removeTagPreset(del.dataset.delPreset); rebuildTagMenu(); return; }
  if (e.target.closest("[data-tag-add]")) { addTagPresetFromMenu(); return; }
  if (e.target.closest("[data-tag-clear]")) { clearTagsJournaled(); return; }
  const apply = e.target.closest("[data-tag]");
  if (apply) applyTagJournaled(apply.dataset.tag);
});
tagMenuList.addEventListener("keydown", e => {
  if (e.key === "Enter" && e.target.id === "tagPresetInput") {
    e.preventDefault();
    addTagPresetFromMenu();
  }
});

// Selection-bar menus: toggle on the anchor button, close on outside
// click / Escape / a menu item without data-keep (tag items stay open so
// several tags can be applied to one selection).
const MENU_BUTTONS = { tagMenuList: "btnTagMenu", exportMenuList: "btnExportMenu", viewMenuList: "btnViewMenu" };
function setMenuExpanded(listId, open) {
  const btn = document.getElementById(MENU_BUTTONS[listId]);
  if (btn) btn.setAttribute("aria-expanded", open ? "true" : "false");
}
function closeAllMenus() {
  document.querySelectorAll(".menu-list").forEach(l => { l.hidden = true; setMenuExpanded(l.id, false); });
}
function toggleMenu(listId) {
  const list = document.getElementById(listId);
  const willOpen = list.hidden;
  closeAllMenus();
  list.hidden = !willOpen;
  setMenuExpanded(listId, !willOpen);
}
document.getElementById("btnTagMenu").addEventListener("click", e => {
  e.stopPropagation();
  rebuildTagMenu();
  toggleMenu("tagMenuList");
});
document.getElementById("btnExportMenu").addEventListener("click", e => {
  e.stopPropagation();
  toggleMenu("exportMenuList");
});
document.getElementById("btnViewMenu").addEventListener("click", e => {
  e.stopPropagation();
  toggleMenu("viewMenuList");
});
document.getElementById("btnMoreFilters").addEventListener("click", e => {
  const extra = document.getElementById("filtersExtra");
  extra.hidden = !extra.hidden;
  e.currentTarget.setAttribute("aria-expanded", extra.hidden ? "false" : "true");
});
const btnGroupKind = document.getElementById("btnGroupKind");
btnGroupKind.addEventListener("click", () => {
  state.groupBy = state.groupBy === "kind" ? "none" : "kind";
  els.groupBy.value = state.groupBy;
  state.currentPage = 1;
  render();
});
const syncGroupKindChip = () => btnGroupKind.classList.toggle("active", state.groupBy === "kind");
els.groupBy.addEventListener("change", syncGroupKindChip);
document.querySelectorAll(".menu-list").forEach(list => {
  list.addEventListener("click", e => {
    // Keep the document-level closer from seeing in-menu clicks; an
    // action item without data-keep closes its own menu explicitly.
    e.stopPropagation();
    if (e.target.closest("button") && !e.target.closest("button").hasAttribute("data-keep")) {
      list.hidden = true;
      setMenuExpanded(list.id, false);
    }
  });
});
document.addEventListener("click", closeAllMenus);
document.addEventListener("keydown", e => {
  if (e.key === "Escape") closeAllMenus();
});
document.getElementById("btnDownloadSelection").addEventListener("click", () => {
  const selected = selectedRecords();
  if (!selected.length) { toast(t("toast.selectFirst")); return; }
  downloadJSON(`frametrace-selection-${manifest.case_id || "case"}-${Date.now()}.json`, {
    schema_version: 1,
    case_id: manifest.case_id || null,
    created_unix: Math.floor(Date.now() / 1000),
    items: selected.map(record => ({
      selector: record.id,
      kind: record.kind,
      action: record.kind === "video" ? "export" : record.kind === "candidate" ? "recover" : "validate",
      format: "mp4",
      notes: record.name
    }))
  });
});
