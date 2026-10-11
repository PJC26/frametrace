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
        // Ranges sync to the case DB (FOR-01): an undone/redone range is a pending change.
        if (JSON.stringify(now.range) !== JSON.stringify(item.range)) annotationChanged = true;
      }
    }
    if (annotationChanged) touchAnnotation(id);
  }
  storageSet(MARKS_KEY, state.marks);
  storageSet(NOTES_KEY, state.notes);
  storageSet(TAGS_KEY, state.tags);
  storageSet(RANGES_KEY, state.ranges);
}

// --- toasts ------------------------------------------------------------
// Top-right stack (never over the bottom action bar), at most 3 at once,
// 6 s auto-dismiss that pauses while the pointer or keyboard focus is on a
// toast, Esc dismisses, and error toasts stay until dismissed. This
// definition replaces the plain stub in 30-records.js (a later function
// declaration of the same name wins inside the viewer closure). Each toast
// is also announced through the single polite status line (15-announce.js)
// instead of making #toastHost a live region of its own.
const TOAST_MAX = 3;
const TOAST_MS = 6000;
const TOAST_ERROR_RE = /실패|오류|failed|failure|error|연결할 수 없|오프라인|offline|unreachable/i;

function dismissToast(item) {
  if (!item) return;
  clearTimeout(item._ftTimer);
  if (item.isConnected) item.remove();
}
function dismissToasts() {
  const host = document.getElementById("toastHost");
  if (!host) return false;
  const items = [...host.querySelectorAll(".toast:not(.clip-progress)")];
  items.forEach(dismissToast);
  return items.length > 0;
}
// Auto-dismiss timer with hover/focus pause: leaving restarts the
// remaining time (never less than 1.5 s so the text stays readable).
function armToast(item, ms) {
  item._ftLeft = ms;
  const start = () => {
    clearTimeout(item._ftTimer);
    item._ftPaused = false;
    item._ftStarted = Date.now();
    item._ftTimer = setTimeout(() => dismissToast(item), item._ftLeft);
  };
  const pause = () => {
    if (item._ftPaused) return;
    item._ftPaused = true;
    clearTimeout(item._ftTimer);
    item._ftLeft = Math.max(1500, item._ftLeft - (Date.now() - item._ftStarted));
  };
  item.addEventListener("mouseenter", pause);
  item.addEventListener("focusin", pause);
  item.addEventListener("mouseleave", () => { if (!item.contains(document.activeElement)) start(); });
  item.addEventListener("focusout", () => { if (!item.matches(":hover")) start(); });
  start();
}
function toast(message, opts) {
  const host = document.getElementById("toastHost");
  const text = String(message == null ? "" : message);
  const error = opts && opts.error != null ? !!opts.error : TOAST_ERROR_RE.test(text);
  ftAnnounce(text);
  if (!host) return null;
  const item = document.createElement("div");
  item.className = error ? "toast toast-error" : "toast";
  const body = document.createElement("span");
  body.className = "toast-text";
  body.textContent = text;
  const close = document.createElement("button");
  close.type = "button";
  close.className = "toast-close";
  close.textContent = "\u2715";
  close.title = t("toast.close");
  close.setAttribute("aria-label", t("toast.close"));
  close.addEventListener("click", () => dismissToast(item));
  item.append(body, close);
  host.appendChild(item);
  const stack = [...host.querySelectorAll(".toast:not(.clip-progress)")];
  while (stack.length > TOAST_MAX) {
    const drop = stack.find(el => !el.classList.contains("toast-error")) || stack[0];
    stack.splice(stack.indexOf(drop), 1);
    dismissToast(drop);
  }
  if (!error) armToast(item, (opts && opts.ms) || TOAST_MS);
  return item;
}
function toastError(message) { return toast(message, { error: true }); }

// Toast with an inline Undo button. Only the newest undo toast is kept so
// rapid 1/2/3 triage does not stack a column of stale buttons.
function attachUndoButton(item) {
  const btn = document.createElement("button");
  btn.type = "button";
  btn.className = "mini toast-action";
  btn.textContent = t("undo.btn");
  btn.addEventListener("click", () => { dismissToast(item); undoJournal(); });
  item.classList.add("toast-undo");
  item.insertBefore(btn, item.querySelector(".toast-close"));
}
function toastUndo(message) {
  const host = document.getElementById("toastHost");
  if (!host) return;
  host.querySelectorAll(".toast-undo").forEach(el => dismissToast(el));
  const item = toast(message);
  if (item) attachUndoButton(item);
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
    host.querySelectorAll(".toast-undo").forEach(el => dismissToast(el));
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
// Runs `run` now for small batches; above the threshold it first asks via
// the in-page dialog (Esc/Cancel = do nothing). Small batches stay
// synchronous so single-key flows never gain a microtask delay.
function confirmThen(ids, action, run) {
  if (ids.length <= BULK_CONFIRM_MIN) { run(); return; }
  askDialog({ message: tf("confirm.bulk", { n: ids.length, action }) }).then(res => { if (res) run(); });
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
  confirmThen(ids, label, () => {
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
  });
}

function applyTagJournaled(tag) {
  const ids = targetIds();
  if (!ids.length) { journaled(tf("journal.tag", { tag }), ids, () => applyTag(tag)); return; }
  confirmThen(ids, tf("journal.tag", { tag }), () => journaled(tf("journal.tag", { tag }), ids, () => applyTag(tag)));
}

function clearTagsJournaled() {
  const ids = targetIds();
  if (!ids.length) { journaled(t("journal.tagClear"), ids, () => clearTags()); return; }
  confirmThen(ids, t("journal.tagClear"), () => journaled(t("journal.tagClear"), ids, () => clearTags()));
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
  const clearBtn = document.getElementById("btnClearSelection");
  if (!bar || !count || !clearBtn) return null;
  // Review progress sits right after the scope controls: "검토 12/340" + meter.
  const progress = document.createElement("span");
  progress.id = "reviewProgress";
  progress.className = "bar-progress";
  const progressText = document.createElement("span");
  const meter = document.createElement("progress");
  meter.setAttribute("aria-hidden", "true");
  progress.append(progressText, meter);
  clearBtn.insertAdjacentElement("afterend", progress);
  return {
    count, progress, progressText, meter,
    scopeActive: document.getElementById("btnScopeActive"),
    scopeSelected: document.getElementById("btnScopeSelected"),
    scopeLabel: document.getElementById("scopeSelLabel")
  };
})();

// Sets a button's tooltip through data-i18n-title so applyChromeI18n()
// (which re-runs on every render) keeps the same, localised text.
function setI18nTitle(el, key) {
  if (!el) return;
  el.dataset.i18nTitle = key;
  el.title = t(key);
}

function updateReviewMeta() {
  if (!reviewMeta) return;
  const total = records.length;
  const reviewed = records.reduce((n, record) => n + (isReviewed(record) ? 1 : 0), 0);
  reviewMeta.progressText.textContent = tf("progress.label", { n: reviewed, m: total });
  reviewMeta.meter.max = Math.max(1, total);
  reviewMeta.meter.value = reviewed;
  reviewMeta.progress.title = t("progress.title");
  // Scope segment: shows what the verdict buttons / tag menu act on. The
  // rule is unchanged (selection wins, otherwise the active item).
  const selected = state.selectedIds.size;
  const { scopeActive, scopeSelected, scopeLabel } = reviewMeta;
  scopeLabel.textContent = tf("scope.btnSel", { n: selected });
  scopeSelected.disabled = selected === 0;
  scopeSelected.setAttribute("aria-pressed", selected ? "true" : "false");
  setI18nTitle(scopeSelected, selected ? "scope.tipSelected" : "scope.tipNone");
  const noTarget = !selected && !state.activeId;
  scopeActive.disabled = noTarget;
  scopeActive.setAttribute("aria-pressed", selected || noTarget ? "false" : "true");
  setI18nTitle(scopeActive, noTarget ? "scope.none" : selected ? "scope.tipActive" : "scope.tipActiveOnly");
}
if (reviewMeta) {
  reviewMeta.scopeActive.addEventListener("click", () => { if (state.selectedIds.size) clearSelection(); });
}

// --- consolidated status line (count · selection · save state) ----------
// One debounced polite message replaces the aria-live attributes that used
// to sit on #selectionCount, #saveState, the toast host and the index chip.
// Only a CHANGED sentence is announced, so j/k navigation stays silent.
let lastStatusLine = null;
function composeStatusLine() {
  if (!reviewMeta) return "";
  const saveEl = document.getElementById("saveState");
  return [reviewMeta.count.textContent, reviewMeta.progressText.textContent, saveEl ? saveEl.textContent : ""]
    .map(part => String(part || "").trim()).filter(Boolean).join(" · ");
}
function refreshStatusLine() {
  const line = composeStatusLine();
  if (lastStatusLine === null) { lastStatusLine = line; return; }
  if (line === lastStatusLine) return;
  lastStatusLine = line;
  ftAnnounce(line);
}
// renderMetrics() rewrites #selectionCount on every render — piggyback on
// that instead of patching render() in a file this module doesn't own.
if (reviewMeta && typeof MutationObserver === "function") {
  new MutationObserver(() => { updateReviewMeta(); refreshStatusLine(); })
    .observe(reviewMeta.count, { childList: true, characterData: true, subtree: true });
  const saveEl = document.getElementById("saveState");
  if (saveEl) {
    let lastSaveKind = saveEl.dataset.state || "";
    new MutationObserver(() => {
      const kind = saveEl.dataset.state || "";
      refreshStatusLine();
      // Assertive only when saving newly FAILS — never for routine states.
      if (kind === "failed" && lastSaveKind !== "failed") {
        ftAnnounce(`${saveEl.textContent}. ${saveEl.title || ""}`, { assertive: true });
      }
      lastSaveKind = kind;
    }).observe(saveEl, { childList: true, characterData: true, subtree: true, attributes: true, attributeFilter: ["data-state"] });
  }
}
updateReviewMeta();
lastStatusLine = composeStatusLine();

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
    case ",": stepFrame(-1); break;
    case ".": stepFrame(1); break;
    case "J": shuttleKey(-1); break;
    case "K": shuttleKey(0); break;
    case "L": shuttleKey(1); break;
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
      dismissToasts();
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
els.videoMode.addEventListener("change", () => {
  state.layout.videoMode = els.videoMode.value;
  // A preset size (50/100/200%) drives the zoom value so the slider and
  // the rendered size always agree.
  const preset = Number(els.videoMode.value);
  if (Number.isFinite(preset) && preset > 0) {
    state.layout.videoZoom = preset;
    els.videoZoom.value = String(preset);
  }
  saveLayout();
  applyVideoScale();
});
els.videoZoom.addEventListener("input", () => {
  // Dragging the slider leaves "fit" for a custom zoom (it used to do
  // nothing in fit mode).
  state.layout.videoMode = "custom";
  els.videoMode.value = "custom";
  state.layout.videoZoom = Number(els.videoZoom.value);
  saveLayoutSoon();
  applyVideoScale();
});
document.getElementById("btnTheater").addEventListener("click", toggleTheater);
document.getElementById("btnFullscreen").addEventListener("click", toggleFullscreen);
document.getElementById("btnPip").addEventListener("click", togglePip);
document.getElementById("btnSkipBack").addEventListener("click", () => skipVideo(-10));
document.getElementById("btnStepBack").addEventListener("click", () => stepFrame(-1));
document.getElementById("btnStepFwd").addEventListener("click", () => stepFrame(1));
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
  await withBusy(ev.currentTarget, async () => {
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
  }, { fail: () => t("toast.telemetryOffline") });
});

// Grab the decoded frame straight off the <video> element — no ffmpeg round
// trip — and store it as a hashed case artifact (artifacts/captures/).
async function captureCurrentFrame(videoEl, recordId, btn) {
  if (!videoEl || !videoEl.videoWidth) { toast(t("toast.noVideo")); return; }
  const canvas = document.createElement("canvas");
  canvas.width = videoEl.videoWidth;
  canvas.height = videoEl.videoHeight;
  canvas.getContext("2d").drawImage(videoEl, 0, 0);
  const dataUrl = canvas.toDataURL("image/jpeg", 0.92);
  const b64 = dataUrl.slice(dataUrl.indexOf(",") + 1);
  await withBusy(btn, async () => {
    const res = await fetch("/api/capture-frame", {
      method: "POST",
      headers: { "Content-Type": "text/plain" },
      body: JSON.stringify({ id: recordId, image: b64, time: videoEl.currentTime.toFixed(2) })
    });
    const data = await res.json();
    if (!data.ok) throw new Error(data.error || "");
    toast(tf("toast.captureSaved", { path: data.path }));
  }, { fail: err => (err instanceof TypeError || err instanceof SyntaxError) ? t("toast.captureOffline") : tf("toast.captureFail", { err: err?.message || "" }) });
}
document.getElementById("btnCaptureFrame").addEventListener("click", ev => {
  const record = selectedRecord();
  captureCurrentFrame(currentVideo(), record ? record.id : "unknown", ev.currentTarget);
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
    // Ranges persist server-side (FOR-01): queue the case-DB sync.
    touchAnnotation(record.id);
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
  await withBusy(btn, async () => {
    await requestReviewProxy(record);
    toast(t("toast.proxyPlaying"));
  }, { start: "toast.proxyBuilding", fail: err => tf("toast.proxyFail", { err: err?.message || "" }) });
});
// "12.5", "1:02.250" or "0:01:02" -> seconds, NaN when unparseable.
function parseClipTime(text) {
  const parts = String(text || "").trim().split(":");
  if (!parts.length || parts.length > 3 || parts.some(p => p.trim() === "")) return NaN;
  let total = 0;
  for (const p of parts) {
    const n = Number(p);
    if (!Number.isFinite(n) || n < 0) return NaN;
    total = total * 60 + n;
  }
  return total;
}

// Clip export runs as its own server job (UX-09, src/serve/media.rs):
// POST /api/export-clip → {ok, job:{id, state, path, bytes_written,
// elapsed_secs, sha256, error}, deduped}; an identical request joins the
// same job. Progress: GET /api/export-clip-status?job=ID; cancel: POST
// /api/export-clip-cancel {job}. A finished job carries the clip's case-
// relative `path` and `sha256`; a failed/cancelled one never leaves a clip.
function fmtClipBytes(n) {
  const v = Number(n) || 0;
  if (v >= 1048576) return (v / 1048576).toFixed(1) + " MB";
  if (v >= 1024) return Math.round(v / 1024) + " KB";
  return v + " B";
}
// Persistent progress row (in the toast host) with a cancel button.
function clipProgressUi(onCancel) {
  const host = document.getElementById("toastHost");
  const item = document.createElement("div");
  item.className = "toast clip-progress";
  const text = document.createElement("span");
  text.className = "toast-text";
  const cancel = document.createElement("button");
  cancel.type = "button";
  cancel.className = "mini toast-action";
  cancel.textContent = t("clip.cancel");
  cancel.addEventListener("click", () => {
    cancel.disabled = true;
    cancel.textContent = t("clip.cancelling");
    onCancel();
  });
  item.append(text, " ", cancel);
  if (host) host.appendChild(item);
  return {
    set(job) {
      text.textContent = tf("clip.progress", { sec: Number(job.elapsed_secs) || 0, size: fmtClipBytes(job.bytes_written) });
    },
    remove() { item.remove(); }
  };
}
// Polls one export job until it leaves "running"; resolves with the final
// job record (state done|failed|cancelled). Network blips are retried.
async function pollExportJob(job, btn, ui) {
  const id = job.id;
  const base = btn.dataset.label || btn.textContent;
  btn.dataset.label = base;
  let current = job;
  let misses = 0;
  while (current.state === "running") {
    ui.set(current);
    btn.textContent = `${base} ${Number(current.elapsed_secs) || 0}s`;
    await new Promise(r => setTimeout(r, 1000));
    let data = null;
    try {
      const res = await fetch(`/api/export-clip-status?job=${encodeURIComponent(id)}`, { cache: "no-store" });
      data = await res.json();
    } catch {
      misses += 1;
      if (misses >= 30) throw new Error(t("toast.clipOffline"));
      continue;
    }
    misses = 0;
    if (!data || !data.ok || !data.job) throw new Error((data && data.error) || "export-clip-status");
    current = data.job;
  }
  btn.textContent = base;
  return current;
}

async function exportClip(btn, burn) {
  const record = selectedRecord();
  const range = rangeOf(record);
  if (!record || !range || !Number.isFinite(range.in) || !Number.isFinite(range.out) || range.out <= range.in) {
    toast(t("toast.noRange"));
    return;
  }
  const fields = [
    { name: "in", label: t("clip.in"), value: range.in.toFixed(3) },
    { name: "out", label: t("clip.out"), value: range.out.toFixed(3) }
  ];
  if (burn) fields.push({ name: "exhibit", label: t("clip.exhibitPrompt"), value: t("clip.exhibitDefault") });
  const ans = await askDialog({ title: t(burn ? "toolbar.clipBurn" : "toolbar.clip"), message: t("clip.dlgMsg"), fields, ok: t("clip.go") });
  if (!ans) return;
  const tin = parseClipTime(ans.in), tout = parseClipTime(ans.out);
  if (!Number.isFinite(tin) || !Number.isFinite(tout) || tout <= tin) { toast(t("clip.badRange")); return; }
  const body = { id: record.id, path: record.path || "", start: tin.toFixed(3), duration: (tout - tin).toFixed(3) };
  if (burn) { body.burn_in = "true"; body.exhibit = (ans.exhibit || "").trim(); }
  await withBusy(btn, async () => {
    let res;
    try {
      res = await fetch("/api/export-clip", { method: "POST", headers: { "Content-Type": "text/plain" }, body: JSON.stringify(body) });
    } catch {
      throw new Error(t("toast.clipOffline"));
    }
    const data = await res.json();
    if (!data.ok) throw new Error(data.error || "");
    let job = data.job || null;
    if (job && job.state === "running") {
      if (data.deduped) toast(t("clip.joined"));
      const ui = clipProgressUi(async () => {
        try {
          const r = await fetch("/api/export-clip-cancel", { method: "POST", headers: { "Content-Type": "text/plain" }, body: JSON.stringify({ job: job.id }) });
          const d = await r.json();
          if (!d.ok) toast(tf("toast.clipFail", { err: d.error || "" }));
        } catch { toast(t("toast.clipOffline")); }
      });
      try { job = await pollExportJob(job, btn, ui); } finally { ui.remove(); }
    }
    if (job && job.state === "cancelled") { toast(t("toast.clipCancelled")); return; }
    if (job && job.state === "failed") throw new Error(job.error || "export failed");
    const path = (job && job.path) || data.path || "";
    const sha = (job && job.sha256) || data.sha256 || "";
    const caseDir = String(DATA.caseDir || "");
    const sep = caseDir.includes("\\") && !caseDir.includes("/") ? "\\" : "/";
    const full = path && caseDir && !/^([a-zA-Z]:|[\\/])/.test(path)
      ? caseDir.replace(/[\\/]+$/, "") + sep + path.split("/").join(sep) : path;
    toast(path ? tf("toast.clipDone", { path }) : t("clip.doneNoPath"));
    // Exhibit summary: what was cut, from what, where it is, and its hash.
    await askDialog({
      title: t("clip.summaryTitle"),
      message: [
        `${t("clip.src")}: ${record.name || record.id}`,
        `${t("clip.range")}: ${tin.toFixed(3)}s – ${tout.toFixed(3)}s (${(tout - tin).toFixed(3)}s)`,
        `${t("clip.out.path")}: ${full || "-"}`,
        job && job.size_bytes ? `${t("clip.size")}: ${fmtClipBytes(job.size_bytes)} (${Number(job.size_bytes).toLocaleString()} B)` : "",
        `SHA-256: ${sha || t("clip.noSha")}`,
        burn && body.exhibit ? `${t("clip.exhibit")}: ${body.exhibit}` : ""
      ].filter(Boolean).join("\n"),
      single: true
    });
  }, { fail: err => tf("toast.clipFail", { err: err?.message || "" }) });
}
document.getElementById("btnExportClip").addEventListener("click", ev => exportClip(ev.currentTarget, false));
document.getElementById("btnExportClipBurn").addEventListener("click", ev => exportClip(ev.currentTarget, true));
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
    `<div class="tag-menu-row"><button type="button" data-keep data-tag="${escapeHtml(tag)}">${escapeHtml(tag)}</button><button type="button" class="tag-del" data-keep data-del-preset="${escapeHtml(tag)}" title="${t("tagmenu.delPreset")}" aria-label="${escapeHtml(tf("tagmenu.delOne", { tag }))}">×</button></div>`
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
  if (del) { removeTagPreset(del.dataset.delPreset); rebuildTagMenu(); tagMenuList.querySelector("button")?.focus(); return; }
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

// Menus (action bar, header "보기", toolbar "더보기"): toggle on the anchor
// button, close on outside click / Escape / focus leaving / a menu item
// without data-keep (tag items stay open so several tags can be applied to
// one selection). Keyboard: Enter / Space / ArrowDown opens and focuses the
// first item (ArrowUp the last), arrows / Home / End move, Esc closes and
// returns focus to the anchor. <select> and range inputs inside a menu keep
// their own arrow keys.
const MENU_BUTTONS = { tagMenuList: "btnTagMenu", exportMenuList: "btnExportMenu", viewMenuList: "btnViewMenu", moreToolsList: "btnMoreTools" };
const MENU_ITEM_SELECTOR = "button:not(:disabled), input:not(:disabled), select:not(:disabled)";
function menuAnchor(listId) { return document.getElementById(MENU_BUTTONS[listId]); }
function menuItems(list) {
  return [...list.querySelectorAll(MENU_ITEM_SELECTOR)].filter(el => el.offsetParent !== null || el.getClientRects().length);
}
function setMenuExpanded(listId, open) {
  const btn = menuAnchor(listId);
  if (btn) btn.setAttribute("aria-expanded", open ? "true" : "false");
}
function openMenuLists() { return [...document.querySelectorAll(".menu-list")].filter(l => !l.hidden); }
function closeAllMenus() {
  document.querySelectorAll(".menu-list").forEach(l => { l.hidden = true; setMenuExpanded(l.id, false); });
}
// The toolbar popover lives inside the overflow:hidden player, so it is
// positioned in viewport coordinates next to its anchor (open downwards when
// there is room, otherwise upwards) and never clipped.
function positionFixedMenu(list) {
  const anchor = menuAnchor(list.id);
  if (!anchor || !list.classList.contains("menu-fixed")) return;
  const r = anchor.getBoundingClientRect();
  // Never open over the action bar: the space below ends at the bar's top edge.
  const bar = document.getElementById("selectionBar");
  const floor = bar ? bar.getBoundingClientRect().top : window.innerHeight;
  const below = floor - r.bottom - 8;
  const above = r.top - 56; // keep clear of the 46px header
  const down = below >= Math.min(list.scrollHeight, 320) || below >= above;
  list.style.maxHeight = Math.max(140, down ? below : above) + "px";
  list.style.left = "auto";
  list.style.right = Math.max(8, window.innerWidth - r.right) + "px";
  list.style.top = down ? r.bottom + 6 + "px" : "auto";
  list.style.bottom = down ? "auto" : window.innerHeight - r.top + 6 + "px";
}
function toggleMenu(listId, focus) {
  const list = document.getElementById(listId);
  const willOpen = list.hidden;
  closeAllMenus();
  list.hidden = !willOpen;
  setMenuExpanded(listId, willOpen);
  if (willOpen) {
    positionFixedMenu(list);
    if (focus) { const items = menuItems(list); (focus === "last" ? items[items.length - 1] : items[0])?.focus(); }
  }
}
function closeMenuAndFocusAnchor(list) {
  list.hidden = true;
  setMenuExpanded(list.id, false);
  const anchor = menuAnchor(list.id);
  if (anchor && anchor.offsetParent !== null) anchor.focus({ preventScroll: true });
}
[["btnTagMenu", "tagMenuList"], ["btnExportMenu", "exportMenuList"], ["btnViewMenu", "viewMenuList"], ["btnMoreTools", "moreToolsList"]].forEach(([btnId, listId]) => {
  const btn = document.getElementById(btnId);
  if (!btn) return;
  btn.addEventListener("click", e => {
    e.stopPropagation();
    if (listId === "tagMenuList") rebuildTagMenu();
    // detail === 0: activated from the keyboard (Enter/Space) -> move into the menu.
    toggleMenu(listId, e.detail === 0 ? "first" : null);
  });
  btn.addEventListener("keydown", e => {
    if (e.key !== "ArrowDown" && e.key !== "ArrowUp") return;
    e.preventDefault();
    e.stopPropagation();
    if (listId === "tagMenuList") rebuildTagMenu();
    const list = document.getElementById(listId);
    if (list.hidden) toggleMenu(listId, e.key === "ArrowUp" ? "last" : "first");
    else { const items = menuItems(list); (e.key === "ArrowUp" ? items[items.length - 1] : items[0])?.focus(); }
  });
});
document.querySelectorAll(".menu-list").forEach(list => {
  // Capture phase: a clicked action item closes its menu and hands focus
  // back to the anchor BEFORE the item's own handler runs, so dialogs it
  // opens (askDialog) restore focus to a visible control.
  list.addEventListener("click", e => {
    const item = e.target.closest("button");
    if (item && !item.hasAttribute("data-keep") && list.contains(item)) closeMenuAndFocusAnchor(list);
  }, true);
  list.addEventListener("click", e => {
    // Keep the document-level closer from seeing in-menu clicks.
    e.stopPropagation();
  });
  list.addEventListener("keydown", e => {
    if (e.key === "Escape") {
      e.preventDefault();
      e.stopPropagation();
      closeMenuAndFocusAnchor(list);
      return;
    }
    if (e.key === "Tab") { closeMenuAndFocusAnchor(list); return; }
    const tag = e.target.tagName;
    const own = tag === "SELECT" || (tag === "INPUT" && e.target.type === "range");
    if (own && (e.key.startsWith("Arrow") || e.key === "Home" || e.key === "End")) return;
    if (!["ArrowDown", "ArrowUp", "Home", "End"].includes(e.key)) return;
    if (tag === "INPUT" && (e.key === "Home" || e.key === "End")) return; // caret keys in the tag text field
    const items = menuItems(list);
    if (!items.length) return;
    e.preventDefault();
    e.stopPropagation();
    const i = items.indexOf(e.target);
    const next = e.key === "Home" ? 0 : e.key === "End" ? items.length - 1
      : e.key === "ArrowDown" ? (i + 1) % items.length : (i - 1 + items.length) % items.length;
    items[next].focus();
  });
  // Keyboard focus leaving the menu entirely (e.g. Shift+Tab off the anchor)
  // closes it; a null relatedTarget (mouse click in Safari) does not.
  list.parentElement.addEventListener("focusout", e => {
    if (e.relatedTarget && !list.parentElement.contains(e.relatedTarget)) { list.hidden = true; setMenuExpanded(list.id, false); }
  });
});
document.addEventListener("click", closeAllMenus);
window.addEventListener("resize", closeAllMenus);
// Esc with a menu open closes only the menu (capture phase, so the global
// handler below does not also leave theater mode / dismiss toasts).
window.addEventListener("keydown", e => {
  if (e.key !== "Escape") return;
  const open = openMenuLists();
  if (!open.length) return;
  e.preventDefault();
  e.stopPropagation();
  const inside = open.find(l => l.parentElement.contains(document.activeElement));
  if (inside) closeMenuAndFocusAnchor(inside); else closeAllMenus();
}, true);
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

// --- accessible names for icon-only chrome --------------------------------
// "?" lives in the header markup; its name is attached here and goes through
// data-i18n-aria-label so the language switch keeps it localised. ◐ and EN
// keep their visible text as the name (a different aria-label would break
// "label in name"); their title carries the explanation.
[["btnShortcuts", "tip.shortcuts"]].forEach(([id, key]) => {
  const el = document.getElementById(id);
  if (!el) return;
  el.dataset.i18nAriaLabel = key;
  el.setAttribute("aria-label", t(key));
});
[["btnMarkReviewed", "1"], ["btnMarkImportant", "2"], ["btnMarkVerify", "3"], ["btnMarkClear", "0"],
  ["btnSetIn", "i"], ["btnSetOut", "o"], ["btnStepBack", ","], ["btnStepFwd", "."]].forEach(([id, keys]) => {
  document.getElementById(id)?.setAttribute("aria-keyshortcuts", keys);
});

// --- player toolbar: transport row + "더보기" overflow ----------------------
// Row 1 (transport) is always inline. The secondary tools (pop-out, dual,
// save, capture, clip exports, proxy, size/zoom) are the SAME elements moved
// — never cloned, so ids stay unique and every handler stays bound — into a
// popover when the media column is narrower than 1000 px, and back into row 2
// when it is wider. A ResizeObserver drives it; nothing is ever clipped.
const TOOLBAR_INLINE_MIN_WIDTH = 1000;
const toolbarOverflow = (() => {
  const wrap = document.getElementById("tbMore");
  const list = document.getElementById("moreToolsList");
  const row = document.getElementById("toolsRow");
  const media = document.querySelector("section.media");
  const more = document.getElementById("btnMoreTools");
  if (!wrap || !list || !row || !media || !more) return null;
  const tools = [...row.children];
  let compact = null;
  const syncBusy = () => {
    const busy = tools.some(el => el.getAttribute && el.getAttribute("aria-busy") === "true");
    more.classList.toggle("busy", busy);
    if (busy) more.setAttribute("aria-busy", "true"); else more.removeAttribute("aria-busy");
  };
  function apply(narrow) {
    if (narrow === compact) return;
    compact = narrow;
    list.hidden = true;
    setMenuExpanded(list.id, false);
    if (narrow) tools.forEach(el => list.appendChild(el));
    else tools.forEach(el => row.appendChild(el));
    row.hidden = narrow;
    wrap.hidden = !narrow;
  }
  if (typeof MutationObserver === "function") {
    const mo = new MutationObserver(syncBusy);
    tools.forEach(el => { if (el.nodeType === 1) mo.observe(el, { attributes: true, attributeFilter: ["aria-busy"] }); });
  }
  const measure = () => apply((media.clientWidth || 0) < TOOLBAR_INLINE_MIN_WIDTH);
  measure();
  if (typeof ResizeObserver === "function") new ResizeObserver(measure).observe(media);
  else window.addEventListener("resize", measure);
  return { apply, measure };
})();

// --- live index chip (created by 99-loader.js) -----------------------------
// 99-loader appends a fixed bottom-right chip with its own aria-live, which
// sat on top of the action bar. It is moved into the header (inline, between
// the case line and the actions) and loses its live semantics: the chip text
// changes every poll. A NEW "reload available" button and a lost status poll
// are announced once through ftAnnounce instead (controls.css restyles it;
// the id is unchanged). Embedded (no header) it docks top-right via CSS.
if (typeof MutationObserver === "function") {
  const adoptChip = chip => {
    chip.removeAttribute("aria-live");
    chip.setAttribute("role", "group");
    chip.setAttribute("aria-label", t("chip.live"));
    const header = document.querySelector("body > header");
    const actions = header && header.querySelector(".header-actions");
    // Embedded pages hide the header; the chip then docks top-right (CSS).
    if (actions && getComputedStyle(header).display !== "none") header.insertBefore(chip, actions);
    let reloadShown = false;
    let lostShown = false;
    new MutationObserver(() => {
      const button = chip.querySelector("button");
      const reload = !!button && !button.hidden;
      if (reload && !reloadShown) ftAnnounce(button.textContent);
      reloadShown = reload;
      const lost = chip.textContent.includes("상태 확인 실패");
      if (lost && !lostShown) ftAnnounce(t("chip.lost"), { assertive: true });
      lostShown = lost;
    }).observe(chip, { subtree: true, childList: true, characterData: true, attributes: true, attributeFilter: ["hidden"] });
  };
  const existing = document.getElementById("ft-live-chip");
  if (existing) adoptChip(existing);
  else {
    const watch = new MutationObserver(() => {
      const chip = document.getElementById("ft-live-chip");
      if (chip) { watch.disconnect(); adoptChip(chip); }
    });
    watch.observe(document.body, { childList: true });
  }
}

// --- Ctrl+K command palette -------------------------------------------------
// A filtered list over the visible button labels already on the page (action
// bar, player toolbar incl. the 더보기 popover, header, export menu). Entries
// call the real button's click(), so behaviour, busy and offline states are
// the buttons' own. Reuses the modal styling of the shortcuts sheet.
const commandPalette = (() => {
  let back = null, input = null, listEl = null, entries = [], shown = [], cursor = 0, opener = null;
  const SOURCES = "#selectionBar button, #mediaToolbar button, header button, #exportMenuList button, #viewMenuList button";
  const labelOf = btn => {
    const own = [...btn.childNodes].filter(n => n.nodeName !== "KBD" && !(n.nodeType === 1 && n.getAttribute("aria-hidden") === "true"))
      .map(n => n.textContent).join("").replace(/\s+/g, " ").trim();
    return own || btn.getAttribute("aria-label") || btn.title || btn.id;
  };
  function collect() {
    const seen = new Set();
    entries = [];
    document.querySelectorAll(SOURCES).forEach(btn => {
      if (!btn.id || seen.has(btn.id) || btn.id === "btnMoreTools" || btn.closest("#tagMenuList") || btn.id === "btnShortcutsClose") return;
      seen.add(btn.id);
      entries.push({ btn, label: labelOf(btn), tip: btn.title || "" });
    });
  }
  function draw() {
    const q = input.value.trim().toLowerCase();
    shown = entries.filter(e => !q || e.label.toLowerCase().includes(q) || e.btn.id.toLowerCase().includes(q.replace(/\s/g, "")) || e.tip.toLowerCase().includes(q));
    cursor = Math.min(cursor, Math.max(0, shown.length - 1));
    listEl.innerHTML = shown.length ? shown.map((e, i) =>
      `<li id="cmdOpt${i}" role="option" aria-selected="${i === cursor}" aria-disabled="${e.btn.disabled}" class="cmd-item${i === cursor ? " on" : ""}${e.btn.disabled ? " off" : ""}" data-i="${i}"><span class="cmd-label">${escapeHtml(e.label)}</span>` +
      `<span class="cmd-tip">${escapeHtml(e.btn.disabled ? t("palette.disabled") + (e.tip ? " — " + e.tip : "") : e.tip)}</span></li>`
    ).join("") : `<li class="cmd-empty" role="presentation">${escapeHtml(t("palette.empty"))}</li>`;
    input.setAttribute("aria-activedescendant", shown.length ? "cmdOpt" + cursor : "");
    listEl.querySelector(".on")?.scrollIntoView({ block: "nearest" });
  }
  function close() {
    if (!back || back.hidden) return;
    back.hidden = true;
    try { opener && opener.focus && opener.focus(); } catch { /* opener gone */ }
  }
  function run(index) {
    const entry = shown[index];
    if (!entry) return;
    close();
    if (entry.btn.disabled) { toast(entry.tip || t("palette.disabled")); return; }
    entry.btn.click();
  }
  function build() {
    back = document.createElement("div");
    back.className = "modal-backdrop";
    back.id = "cmdPalette";
    back.hidden = true;
    back.innerHTML = `<div class="modal palette" role="dialog" aria-modal="true" aria-label="${escapeHtml(t("palette.title"))}">` +
      `<input type="text" class="palette-input" role="combobox" aria-expanded="true" aria-controls="cmdList" aria-autocomplete="list" aria-label="${escapeHtml(t("palette.title"))}" placeholder="${escapeHtml(t("palette.placeholder"))}" autocomplete="off">` +
      `<ul id="cmdList" class="palette-list" role="listbox" aria-label="${escapeHtml(t("palette.title"))}"></ul></div>`;
    document.body.appendChild(back);
    input = back.querySelector("input");
    listEl = back.querySelector("ul");
    input.addEventListener("input", () => { cursor = 0; draw(); });
    listEl.addEventListener("mousedown", e => e.preventDefault());
    listEl.addEventListener("click", e => { const li = e.target.closest("[data-i]"); if (li) run(Number(li.dataset.i)); });
    back.addEventListener("click", e => { if (e.target === back) close(); });
    back.addEventListener("keydown", e => {
      if (e.key === "Escape") { e.preventDefault(); close(); }
      else if (e.key === "ArrowDown" || e.key === "ArrowUp") {
        e.preventDefault();
        if (shown.length) cursor = (cursor + (e.key === "ArrowDown" ? 1 : -1) + shown.length) % shown.length;
        draw();
      } else if (e.key === "Enter") { e.preventDefault(); run(cursor); }
      else if (e.key === "Tab") e.preventDefault();
      e.stopPropagation(); // app shortcuts (j/k/1/2/space ...) must not fire under the palette
    });
  }
  function open() {
    if (!back) build();
    if (!back.hidden) return;
    if (document.querySelector(".modal-backdrop:not([hidden]):not(#cmdPalette)")) return;
    closeAllMenus();
    opener = document.activeElement;
    collect();
    input.value = "";
    cursor = 0;
    back.hidden = false;
    draw();
    input.focus();
  }
  window.addEventListener("keydown", e => {
    if (!(e.ctrlKey || e.metaKey) || e.altKey || e.shiftKey || String(e.key).toLowerCase() !== "k") return;
    e.preventDefault();
    e.stopPropagation();
    if (back && !back.hidden) close(); else open();
  }, true);
  return { open, close };
})();
